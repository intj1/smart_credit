//! Database-backed operations shared by the API, the sync worker and the
//! scheduler: period generation and running the matching engine for a card.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::catalog::{Benefit, BENEFIT_COLUMNS};
use crate::engine::{self, BenefitFlags, PeriodIn, Txn};
use crate::periods::{self, CardDates};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub totp_secret_enc: Option<String>,
    pub totp_enabled: bool,
    pub totp_last_step: i64,
    pub timezone: String,
    pub alert_email: Option<String>,
    pub alert_days: Vec<i32>,
    pub alerts_enabled: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Card {
    pub id: Uuid,
    pub user_id: Uuid,
    pub connection_id: Option<Uuid>,
    pub product_id: String,
    pub last4: Option<String>,
    pub nickname: String,
    pub anniversary_month: i32,
    pub opened_date: Option<NaiveDate>,
    pub account_ids: Vec<String>,
    pub housing_payment_cents: Option<i64>,
    pub created_at: chrono::DateTime<Utc>,
}

/// The app has exactly one user.
pub async fn user(pool: &PgPool) -> Result<User> {
    sqlx::query_as::<_, User>(
        "select id, email, password_hash, totp_secret_enc, totp_enabled, totp_last_step, timezone,
                alert_email, alert_days, alerts_enabled from users order by created_at limit 1",
    )
    .fetch_one(pool)
    .await
    .context("no user exists; set ADMIN_EMAIL and ADMIN_PASSWORD for the first start")
}

pub fn today_in(timezone: &str) -> NaiveDate {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    Utc::now().with_timezone(&tz).date_naive()
}

/// Today's date where the user lives; this is what ends a period.
pub async fn today(pool: &PgPool) -> NaiveDate {
    match sqlx::query_scalar::<_, String>("select timezone from users order by created_at limit 1").fetch_optional(pool).await {
        Ok(Some(tz)) => today_in(&tz),
        _ => Utc::now().date_naive(),
    }
}

pub async fn audit(pool: &PgPool, action: &str, detail: Value) {
    if let Err(e) = sqlx::query("insert into audit_log (action, detail) values ($1, $2)").bind(action).bind(detail).execute(pool).await {
        tracing::error!(error = %e, action, "failed to write audit log");
    }
}

pub async fn card(pool: &PgPool, id: Uuid) -> Result<Card, sqlx::Error> {
    sqlx::query_as::<_, Card>("select * from cards where id = $1").bind(id).fetch_one(pool).await
}

async fn benefits_for_card(pool: &PgPool, card: &Card) -> Result<Vec<Benefit>> {
    // Active catalog entries plus retired ones that still have periods.
    Ok(sqlx::query_as::<_, Benefit>(&format!(
        "select {BENEFIT_COLUMNS} from benefits b where product_id = $1
         and (active or exists (select 1 from benefit_periods p where p.benefit_id = b.id and p.card_id = $2))
         order by id"
    ))
    .bind(&card.product_id)
    .bind(card.id)
    .fetch_all(pool)
    .await?)
}

/// Creates any missing periods for the card: history back to the start of last
/// year, the current period, and the next one ("generated ahead").
async fn ensure_periods(tx: &mut sqlx::PgConnection, card: &Card, benefits: &[Benefit], today: NaiveDate) -> Result<()> {
    let dates = CardDates {
        anniversary_month: card.anniversary_month as u32,
        opened_date: card.opened_date,
        added_date: card.created_at.date_naive(),
    };
    let (from, to) = periods::generation_window(today);
    let active: HashSet<String> =
        sqlx::query_scalar("select id from benefits where product_id = $1 and active").bind(&card.product_id).fetch_all(&mut *tx).await?.into_iter().collect();
    for b in benefits.iter().filter(|b| active.contains(&b.id)) {
        let mut list = periods::periods_between(b, dates, from, to);
        let current_end = periods::period_containing(b, dates, today).1;
        if current_end < periods::no_deadline() {
            let next = current_end + Duration::days(1);
            list.extend(periods::periods_between(b, dates, next, next));
        }
        for (start, end, value) in list {
            // The engine owns `value` for mortgage periods (it depends on the payment).
            sqlx::query(
                "insert into benefit_periods (id, card_id, benefit_id, start_date, end_date, value)
                 values ($1,$2,$3,$4,$5,$6)
                 on conflict (card_id, benefit_id, start_date) do update
                   set end_date = excluded.end_date,
                       value = case when $7 then benefit_periods.value else excluded.value end",
            )
            .bind(Uuid::new_v4())
            .bind(card.id)
            .bind(&b.id)
            .bind(start)
            .bind(end)
            .bind(value)
            .bind(b.kind == "mortgage_points")
            .execute(&mut *tx)
            .await?;
        }
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct PeriodRow {
    id: Uuid,
    benefit_id: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
    value: i64,
}

#[derive(sqlx::FromRow)]
struct TxnRow {
    id: Uuid,
    posted_date: NaiveDate,
    amount_cents: i64,
    merchant: String,
    description: String,
    mcc: Option<String>,
    category: Option<String>,
    pending: bool,
}

/// Generates periods and re-runs the matching engine for one card.
pub async fn recompute_card(pool: &PgPool, card_id: Uuid) -> Result<()> {
    let today = today(pool).await;
    let card = card(pool, card_id).await?;
    let benefits = benefits_for_card(pool, &card).await?;

    let mut tx = pool.begin().await?;
    // One recompute per card at a time (sync worker vs. a user override).
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))").bind(card_id).execute(&mut *tx).await?;
    ensure_periods(&mut tx, &card, &benefits, today).await?;

    let period_rows: Vec<PeriodRow> =
        sqlx::query_as("select id, benefit_id, start_date, end_date, value from benefit_periods where card_id = $1")
            .bind(card_id)
            .fetch_all(&mut *tx)
            .await?;
    let period_ins: Vec<PeriodIn> = period_rows
        .iter()
        .map(|p| {
            // Mortgage periods start from zero each run; the engine sets their value.
            let mortgage = benefits.iter().any(|b| b.id == p.benefit_id && b.kind == "mortgage_points");
            PeriodIn { id: p.id, benefit_id: p.benefit_id.clone(), start: p.start_date, end: p.end_date, value: if mortgage { 0 } else { p.value } }
        })
        .collect();
    let txns: Vec<Txn> = sqlx::query_as::<_, TxnRow>(
        "select id, posted_date, amount_cents, merchant, description, mcc, category, pending from transactions where card_id = $1",
    )
    .bind(card_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|t| Txn { id: t.id, date: t.posted_date, amount: t.amount_cents, merchant: t.merchant, description: t.description, mcc: t.mcc, category: t.category, pending: t.pending })
    .collect();
    let exclusions: HashSet<(Uuid, String)> = sqlx::query_as::<_, (Uuid, String)>(
        "select e.transaction_id, e.benefit_id from match_exclusions e join transactions t on t.id = e.transaction_id where t.card_id = $1",
    )
    .bind(card_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let manual: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "select m.period_id, m.amount from benefit_matches m join benefit_periods p on p.id = m.period_id
         where p.card_id = $1 and m.source = 'manual'",
    )
    .bind(card_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let flags: HashMap<String, BenefitFlags> =
        sqlx::query_as::<_, (String, bool, bool)>("select benefit_id, enrolled, not_for_me from card_benefits where card_id = $1")
            .bind(card_id)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(id, enrolled, not_for_me)| (id, BenefitFlags { enrolled, not_for_me }))
            .collect();

    let output = engine::run(&engine::Input {
        today,
        benefits: &benefits,
        periods: &period_ins,
        txns: &txns,
        exclusions: &exclusions,
        manual: &manual,
        flags: &flags,
        housing_payment_cents: card.housing_payment_cents,
    });

    sqlx::query(
        "delete from benefit_matches m using benefit_periods p
         where p.id = m.period_id and p.card_id = $1 and m.source <> 'manual'",
    )
    .bind(card_id)
    .execute(&mut *tx)
    .await?;
    for m in &output.matches {
        sqlx::query("insert into benefit_matches (id, period_id, transaction_id, amount, source, confidence, note) values ($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::new_v4())
            .bind(m.period_id)
            .bind(m.transaction_id)
            .bind(m.amount)
            .bind(m.source)
            .bind(m.confidence)
            .bind(&m.note)
            .execute(&mut *tx)
            .await?;
    }
    for p in &output.periods {
        sqlx::query("update benefit_periods set value = $2, used = $3, pending = $4, status = $5, flag = $6, detail = $7 where id = $1")
            .bind(p.id)
            .bind(p.value)
            .bind(p.used)
            .bind(p.pending)
            .bind(p.status)
            .bind(p.flag)
            .bind(&p.detail)
            .execute(&mut *tx)
            .await?;
    }

    // Merchant offers: auto-redeem on a matching purchase, expire when past due.
    let offers: Vec<(Uuid, String, i64, NaiveDate, NaiveDate)> =
        sqlx::query_as("select id, merchant, min_spend_cents, added_date, expires from offers where card_id = $1 and status = 'active'")
            .bind(card_id)
            .fetch_all(&mut *tx)
            .await?;
    for (id, merchant, min_spend, added, expires) in offers {
        if let Some(txn) = engine::offer_redeemed_by(&merchant, min_spend, added, expires, &txns) {
            sqlx::query("update offers set status = 'redeemed', redeemed_transaction_id = $2 where id = $1").bind(id).bind(txn).execute(&mut *tx).await?;
        } else if expires < today {
            sqlx::query("update offers set status = 'expired' where id = $1").bind(id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn recompute_all(pool: &PgPool) -> Result<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar("select id from cards").fetch_all(pool).await?;
    for id in ids {
        recompute_card(pool, id).await.with_context(|| format!("recomputing card {id}"))?;
    }
    Ok(())
}
