use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::Auth;
use crate::catalog::{Benefit, Product, BENEFIT_COLUMNS};
use crate::error::{bad, AppError, AppResult};
use crate::views::{is_unclaimed, period_views, PeriodFilter, PeriodView};
use crate::{csv_import, service, AppState};

fn none_if_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------- unclaimed

#[derive(Serialize)]
pub struct Totals {
    /// Dollars left in open periods that end this month / this year / at all.
    pub expiring_month_cents: i64,
    pub expiring_year_cents: i64,
    pub total_remaining_cents: i64,
}

fn totals(rows: &[PeriodView], today: NaiveDate) -> Totals {
    let usd = |p: &&PeriodView| p.unit == "usd" && p.status != "not_for_me";
    let sum = |pred: &dyn Fn(NaiveDate) -> bool| -> i64 {
        rows.iter().filter(usd).filter(|p| p.deadline.is_some_and(pred)).map(|p| p.remaining).sum()
    };
    Totals {
        expiring_month_cents: sum(&|d| d.year() == today.year() && d.month() == today.month()),
        expiring_year_cents: sum(&|d| d.year() == today.year()),
        total_remaining_cents: rows.iter().filter(usd).map(|p| p.remaining).sum(),
    }
}

async fn open_unclaimed(pool: &PgPool, mut filter: PeriodFilter, today: NaiveDate, include_hidden: bool) -> AppResult<Vec<PeriodView>> {
    filter.open_on = Some(today);
    let rows = period_views(pool, &filter, today).await?;
    Ok(rows
        .into_iter()
        .filter(|p| is_unclaimed(p) || (include_hidden && p.status == "not_for_me" && p.kind != "mortgage_points" && p.remaining > 0))
        .collect())
}

#[derive(Deserialize)]
pub struct UnclaimedQuery {
    provider: Option<String>,
    card: Option<Uuid>,
    cadence: Option<String>,
    category: Option<String>,
    #[serde(default)]
    include_hidden: bool,
}

pub async fn unclaimed(State(state): State<AppState>, _: Auth, Query(q): Query<UnclaimedQuery>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let filter = PeriodFilter {
        card_id: q.card,
        provider: none_if_empty(q.provider),
        cadence: none_if_empty(q.cadence),
        category: none_if_empty(q.category),
        ..Default::default()
    };
    let rows = open_unclaimed(&state.pool, filter, today, q.include_hidden).await?;
    Ok(Json(json!({ "today": today, "totals": totals(&rows, today), "rows": rows })))
}

pub async fn home(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let all_open = period_views(&state.pool, &PeriodFilter { open_on: Some(today), ..Default::default() }, today).await?;
    let rows: Vec<&PeriodView> = all_open.iter().filter(|p| is_unclaimed(p)).collect();
    let owned: Vec<PeriodView> = rows.iter().map(|p| (*p).clone()).collect();
    let next: Vec<&PeriodView> = rows.iter().filter(|p| p.deadline.is_some()).take(5).copied().collect();
    let mortgage: Vec<&PeriodView> = all_open.iter().filter(|p| p.kind == "mortgage_points" && !p.not_for_me).collect();
    let broken: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', id, 'provider', provider, 'institution_name', institution_name,
                                  'status', status, 'error', error, 'last_sync_at', last_sync_at)
         from connections where status <> 'ok' order by created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    let alerts: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', id, 'kind', kind, 'title', title, 'body', body, 'created_at', created_at)
         from alerts where dismissed_at is null order by created_at desc limit 20",
    )
    .fetch_all(&state.pool)
    .await?;
    let last_sync: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select max(at) from audit_log where action = 'sync'").fetch_one(&state.pool).await?;
    let cards: i64 = sqlx::query_scalar("select count(*) from cards").fetch_one(&state.pool).await?;
    Ok(Json(json!({
        "today": today,
        "totals": totals(&owned, today),
        "unclaimed_count": rows.len(),
        "next_deadlines": next,
        "mortgage": mortgage,
        "broken_connections": broken,
        "alerts": alerts,
        "last_sync_at": last_sync,
        "card_count": cards,
    })))
}

pub async fn catalog(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let products: Vec<Product> = sqlx::query_as(
        "select id, provider, issuer, name, annual_fee_cents, catalog_version, name_hints from card_products where active order by issuer, name",
    )
    .fetch_all(&state.pool)
    .await?;
    let benefits: Vec<Benefit> =
        sqlx::query_as(&format!("select {BENEFIT_COLUMNS} from benefits where active order by name")).fetch_all(&state.pool).await?;
    let out: Vec<Value> = products
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap();
            v["benefits"] = json!(benefits.iter().filter(|b| b.product_id == p.id).collect::<Vec<_>>());
            v
        })
        .collect();
    Ok(Json(json!({ "products": out })))
}

// -------------------------------------------------------------------- cards

#[derive(Serialize, sqlx::FromRow)]
pub struct CardView {
    id: Uuid,
    nickname: String,
    name: String,
    product_id: String,
    product_name: String,
    provider: String,
    last4: Option<String>,
    anniversary_month: i32,
    opened_date: Option<NaiveDate>,
    connection_id: Option<Uuid>,
    connection_status: Option<String>,
    housing_payment_cents: Option<i64>,
    annual_fee_cents: i64,
    /// Credits used in benefit periods that overlap this calendar year.
    captured_ytd_cents: i64,
    open_remaining_cents: i64,
    has_mortgage_benefit: bool,
    transaction_count: i64,
    last_transaction_date: Option<NaiveDate>,
}

async fn card_views(pool: &PgPool, today: NaiveDate, id: Option<Uuid>) -> Result<Vec<CardView>, sqlx::Error> {
    let year_start = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap();
    sqlx::query_as(
        "select c.id, c.nickname, case when c.nickname <> '' then c.nickname else cp.name end as name,
                c.product_id, cp.name as product_name, cp.provider, c.last4, c.anniversary_month, c.opened_date,
                c.connection_id, cn.status as connection_status, c.housing_payment_cents, cp.annual_fee_cents,
                coalesce((select sum(p.used) from benefit_periods p join benefits b on b.id = p.benefit_id
                          where p.card_id = c.id and b.unit = 'usd' and b.kind <> 'mortgage_points'
                            and p.end_date >= $1 and p.start_date <= $2), 0)::bigint as captured_ytd_cents,
                coalesce((select sum(greatest(p.value - p.used, 0)) from benefit_periods p join benefits b on b.id = p.benefit_id
                          where p.card_id = c.id and b.unit = 'usd' and b.kind <> 'mortgage_points'
                            and p.start_date <= $2 and p.end_date >= $2 and p.status not in ('used', 'not_for_me')), 0)::bigint as open_remaining_cents,
                exists (select 1 from benefits b where b.product_id = c.product_id and b.active and b.kind = 'mortgage_points') as has_mortgage_benefit,
                (select count(*) from transactions t where t.card_id = c.id) as transaction_count,
                (select max(posted_date) from transactions t where t.card_id = c.id) as last_transaction_date
         from cards c
         join card_products cp on cp.id = c.product_id
         left join connections cn on cn.id = c.connection_id
         where ($3::uuid is null or c.id = $3)
         order by c.created_at",
    )
    .bind(year_start)
    .bind(today)
    .bind(id)
    .fetch_all(pool)
    .await
}

pub async fn list_cards(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    Ok(Json(json!({ "cards": card_views(&state.pool, today, None).await? })))
}

pub async fn get_card(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let card = card_views(&state.pool, today, Some(id)).await?.into_iter().next().ok_or(AppError::NotFound)?;
    let mut periods = period_views(&state.pool, &PeriodFilter { card_id: Some(id), ..Default::default() }, today).await?;
    // Upcoming periods exist in the database but are not shown yet.
    periods.retain(|p| p.start_date <= today);
    Ok(Json(json!({ "today": today, "card": card, "periods": periods })))
}

#[derive(Deserialize)]
pub struct NewCard {
    pub product_id: String,
    #[serde(default)]
    pub nickname: String,
    pub last4: Option<String>,
    pub anniversary_month: i32,
    pub opened_date: Option<NaiveDate>,
    #[serde(default)]
    pub account_ids: Vec<String>,
    #[serde(default)]
    pub enrolled_benefit_ids: Vec<String>,
    pub housing_payment_cents: Option<i64>,
}

fn clean_last4(last4: Option<String>) -> AppResult<Option<String>> {
    match none_if_empty(last4) {
        None => Ok(None),
        // Never store more of the card number than the last four digits.
        Some(v) if v.len() <= 4 && v.chars().all(|c| c.is_ascii_digit()) => Ok(Some(v)),
        Some(_) => Err(bad("last4 must be at most the last four digits of the card")),
    }
}

pub async fn insert_card(pool: &PgPool, user_id: Uuid, connection_id: Option<Uuid>, c: NewCard) -> AppResult<Uuid> {
    if !(1..=12).contains(&c.anniversary_month) {
        return Err(bad("anniversary month must be 1-12"));
    }
    let product_ok: bool = sqlx::query_scalar("select exists (select 1 from card_products where id = $1 and active)")
        .bind(&c.product_id)
        .fetch_one(pool)
        .await?;
    if !product_ok {
        return Err(bad("unknown card product"));
    }
    if c.housing_payment_cents.is_some_and(|v| v < 0) {
        return Err(bad("housing payment cannot be negative"));
    }
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "insert into cards (id, user_id, connection_id, product_id, last4, nickname, anniversary_month, opened_date, account_ids, housing_payment_cents)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(user_id)
    .bind(connection_id)
    .bind(&c.product_id)
    .bind(clean_last4(c.last4)?)
    .bind(c.nickname.trim())
    .bind(c.anniversary_month)
    .bind(c.opened_date)
    .bind(&c.account_ids)
    .bind(c.housing_payment_cents)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "insert into card_benefits (card_id, benefit_id, enrolled)
         select $1, b.id, true from benefits b where b.product_id = $2 and b.id = any($3)",
    )
    .bind(id)
    .bind(&c.product_id)
    .bind(&c.enrolled_benefit_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    service::recompute_card(pool, id).await?;
    service::audit(pool, "card_added", json!({ "card_id": id, "product_id": c.product_id, "manual": connection_id.is_none() })).await;
    Ok(id)
}

pub async fn create_card(State(state): State<AppState>, Auth(session): Auth, Json(body): Json<NewCard>) -> AppResult<Json<Value>> {
    let body = NewCard { account_ids: vec![], ..body };
    let id = insert_card(&state.pool, session.user_id, None, body).await?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct CardPatch {
    nickname: Option<String>,
    last4: Option<String>,
    anniversary_month: Option<i32>,
    /// Present-and-null clears the value.
    #[serde(default, deserialize_with = "double_option")]
    opened_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "double_option")]
    housing_payment_cents: Option<Option<i64>>,
}

fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

pub async fn update_card(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>, Json(body): Json<CardPatch>) -> AppResult<Json<Value>> {
    let card = service::card(&state.pool, id).await?;
    let month = body.anniversary_month.unwrap_or(card.anniversary_month);
    if !(1..=12).contains(&month) {
        return Err(bad("anniversary month must be 1-12"));
    }
    let opened = body.opened_date.unwrap_or(card.opened_date);
    let housing = body.housing_payment_cents.unwrap_or(card.housing_payment_cents);
    if housing.is_some_and(|v| v < 0) {
        return Err(bad("housing payment cannot be negative"));
    }
    let last4 = match body.last4 {
        Some(v) => clean_last4(Some(v))?,
        None => card.last4.clone(),
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("update cards set nickname = $2, last4 = $3, anniversary_month = $4, opened_date = $5, housing_payment_cents = $6 where id = $1")
        .bind(id)
        .bind(body.nickname.as_deref().map(str::trim).unwrap_or(&card.nickname))
        .bind(last4)
        .bind(month)
        .bind(opened)
        .bind(housing)
        .execute(&mut *tx)
        .await?;
    if month != card.anniversary_month || opened != card.opened_date {
        // Period boundaries moved: rebuild the periods that depend on them.
        sqlx::query(
            "delete from benefit_periods p using benefits b
             where b.id = p.benefit_id and p.card_id = $1 and (b.reset_basis = 'cardmember_year' or b.cadence = 'one_time')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    service::recompute_card(&state.pool, id).await?;
    service::audit(&state.pool, "card_updated", json!({ "card_id": id })).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_card(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    // Cascades to its periods, matches, transactions and offers.
    let res = sqlx::query("delete from cards where id = $1").bind(id).execute(&state.pool).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    service::audit(&state.pool, "card_deleted", json!({ "card_id": id })).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ImportBody {
    csv: String,
    /// true: purchases are negative in this file; false: positive; omitted: infer.
    purchases_negative: Option<bool>,
}

pub async fn import_csv(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>, Json(body): Json<ImportBody>) -> AppResult<Json<Value>> {
    service::card(&state.pool, id).await?;
    let rows = csv_import::parse(&body.csv, body.purchases_negative).map_err(|e| bad(format!("Could not read that CSV: {e}")))?;
    if rows.is_empty() {
        return Err(bad("That CSV has no transactions"));
    }
    let mut imported = 0u64;
    let mut tx = state.pool.begin().await?;
    for r in &rows {
        let res = sqlx::query(
            "insert into transactions (id, card_id, posted_date, amount_cents, merchant, description, mcc, category, pending, aggregator_id, source)
             values ($1,$2,$3,$4,$5,$6,$7,$8,false,$9,'csv') on conflict (card_id, aggregator_id) do nothing",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(r.date)
        .bind(r.amount_cents)
        .bind(&r.merchant)
        .bind(&r.description)
        .bind(&r.mcc)
        .bind(&r.category)
        .bind(&r.aggregator_id)
        .execute(&mut *tx)
        .await?;
        imported += res.rows_affected();
    }
    tx.commit().await?;
    service::recompute_card(&state.pool, id).await?;
    let purchases = rows.iter().filter(|r| r.amount_cents > 0).count();
    service::audit(&state.pool, "csv_import", json!({ "card_id": id, "rows": rows.len(), "imported": imported })).await;
    Ok(Json(json!({
        "rows": rows.len(),
        "imported": imported,
        "duplicates": rows.len() as u64 - imported,
        "purchases": purchases,
        "credits": rows.len() - purchases,
        "from": rows.iter().map(|r| r.date).min(),
        "to": rows.iter().map(|r| r.date).max(),
    })))
}

#[derive(Deserialize)]
pub struct Flags {
    enrolled: Option<bool>,
    not_for_me: Option<bool>,
}

pub async fn set_benefit_flags(
    State(state): State<AppState>,
    _: Auth,
    Path((card_id, benefit_id)): Path<(Uuid, String)>,
    Json(body): Json<Flags>,
) -> AppResult<Json<Value>> {
    let card = service::card(&state.pool, card_id).await?;
    let ok: bool = sqlx::query_scalar("select exists (select 1 from benefits where id = $1 and product_id = $2)")
        .bind(&benefit_id)
        .bind(&card.product_id)
        .fetch_one(&state.pool)
        .await?;
    if !ok {
        return Err(AppError::NotFound);
    }
    sqlx::query(
        "insert into card_benefits (card_id, benefit_id, enrolled, not_for_me) values ($1,$2,coalesce($3,false),coalesce($4,false))
         on conflict (card_id, benefit_id) do update
           set enrolled = coalesce($3, card_benefits.enrolled), not_for_me = coalesce($4, card_benefits.not_for_me)",
    )
    .bind(card_id)
    .bind(&benefit_id)
    .bind(body.enrolled)
    .bind(body.not_for_me)
    .execute(&state.pool)
    .await?;
    service::recompute_card(&state.pool, card_id).await?;
    service::audit(&state.pool, "benefit_flags", json!({ "card_id": card_id, "benefit_id": benefit_id, "enrolled": body.enrolled, "not_for_me": body.not_for_me })).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ periods

async fn one_period(pool: &PgPool, id: Uuid, today: NaiveDate) -> AppResult<PeriodView> {
    period_views(pool, &PeriodFilter { id: Some(id), ..Default::default() }, today).await?.into_iter().next().ok_or(AppError::NotFound)
}

pub async fn get_period(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let period = one_period(&state.pool, id, today).await?;
    let matches: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', m.id, 'source', m.source, 'amount', m.amount, 'confidence', m.confidence,
                  'note', m.note, 'created_at', m.created_at,
                  'transaction', case when t.id is null then null else json_build_object('id', t.id, 'date', t.posted_date,
                      'amount_cents', t.amount_cents, 'merchant', t.merchant, 'description', t.description, 'pending', t.pending) end)
         from benefit_matches m left join transactions t on t.id = m.transaction_id
         where m.period_id = $1 order by t.posted_date nulls first, m.created_at",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let excluded: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', t.id, 'date', t.posted_date, 'amount_cents', t.amount_cents,
                                  'merchant', t.merchant, 'description', t.description)
         from match_exclusions e join transactions t on t.id = e.transaction_id
         where e.benefit_id = $1 and t.card_id = $2 order by t.posted_date",
    )
    .bind(&period.benefit_id)
    .bind(period.card_id)
    .fetch_all(&state.pool)
    .await?;
    let rules: Value = sqlx::query_scalar("select rules from benefits where id = $1").bind(&period.benefit_id).fetch_one(&state.pool).await?;
    let history = period_views(&state.pool, &PeriodFilter { card_id: Some(period.card_id), ..Default::default() }, today)
        .await?
        .into_iter()
        .filter(|p| p.benefit_id == period.benefit_id && p.start_date <= today)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "today": today, "period": period, "matches": matches, "excluded": excluded, "rules": rules, "history": history })))
}

#[derive(Deserialize)]
pub struct Override {
    /// "used" = all of it, "amount" = exactly `amount`, "clear" = back to automatic.
    mode: String,
    amount: Option<i64>,
}

pub async fn override_period(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>, Json(body): Json<Override>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let period = one_period(&state.pool, id, today).await?;
    let amount = match body.mode.as_str() {
        "clear" => None,
        "used" if period.kind == "mortgage_points" => return Err(bad("mortgage points are calculated from the payment and spend")),
        "used" => Some(period.value),
        "amount" => {
            let a = body.amount.ok_or_else(|| bad("amount is required"))?;
            if a < 0 || (period.value > 0 && a > period.value) {
                return Err(bad("amount must be between 0 and the period value"));
            }
            Some(a)
        }
        _ => return Err(bad("mode must be used, amount or clear")),
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("delete from benefit_matches where period_id = $1 and source = 'manual'").bind(id).execute(&mut *tx).await?;
    if let Some(a) = amount {
        sqlx::query("insert into benefit_matches (id, period_id, amount, source, confidence, note) values ($1,$2,$3,'manual',1.0,$4)")
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(a)
            .bind(if a >= period.value { "Marked as used by you" } else { "Amount set by you" })
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    service::recompute_card(&state.pool, period.card_id).await?;
    service::audit(&state.pool, "override", json!({ "period_id": id, "benefit_id": period.benefit_id, "mode": body.mode, "amount": amount })).await;
    Ok(Json(json!({ "period": one_period(&state.pool, id, today).await? })))
}

#[derive(Deserialize)]
pub struct Exclude {
    transaction_id: Uuid,
}

pub async fn exclude_txn(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>, Json(body): Json<Exclude>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let period = one_period(&state.pool, id, today).await?;
    let res = sqlx::query(
        "insert into match_exclusions (transaction_id, benefit_id)
         select t.id, $2 from transactions t where t.id = $1 and t.card_id = $3 on conflict do nothing",
    )
    .bind(body.transaction_id)
    .bind(&period.benefit_id)
    .bind(period.card_id)
    .execute(&state.pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(bad("that transaction is not on this card, or is already excluded"));
    }
    service::recompute_card(&state.pool, period.card_id).await?;
    service::audit(&state.pool, "override", json!({ "period_id": id, "benefit_id": period.benefit_id, "mode": "not_this_one", "transaction_id": body.transaction_id })).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn unexclude_txn(State(state): State<AppState>, _: Auth, Path((id, txn_id)): Path<(Uuid, Uuid)>) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let period = one_period(&state.pool, id, today).await?;
    sqlx::query("delete from match_exclusions where transaction_id = $1 and benefit_id = $2").bind(txn_id).bind(&period.benefit_id).execute(&state.pool).await?;
    service::recompute_card(&state.pool, period.card_id).await?;
    service::audit(&state.pool, "override", json!({ "period_id": id, "benefit_id": period.benefit_id, "mode": "undo_not_this_one", "transaction_id": txn_id })).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------- offers

const OFFER_SELECT: &str = "select json_build_object('id', o.id, 'card_id', o.card_id,
        'card_name', case when c.nickname <> '' then c.nickname else cp.name end, 'provider', cp.provider,
        'merchant', o.merchant, 'reward', o.reward, 'reward_cents', o.reward_cents, 'min_spend_cents', o.min_spend_cents,
        'added_date', o.added_date, 'expires', o.expires, 'status', o.status,
        'days_left', (o.expires - $1::date),
        'redeemed_by', case when t.id is null then null else json_build_object('date', t.posted_date,
            'amount_cents', t.amount_cents, 'description', t.description) end)
     from offers o join cards c on c.id = o.card_id join card_products cp on cp.id = c.product_id
     left join transactions t on t.id = o.redeemed_transaction_id";

pub async fn list_offers(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let today = service::today(&state.pool).await;
    let offers: Vec<Value> = sqlx::query_scalar(&format!("{OFFER_SELECT} order by (o.status <> 'active'), o.expires, o.created_at"))
        .bind(today)
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(json!({ "today": today, "offers": offers })))
}

#[derive(Deserialize)]
pub struct NewOffer {
    card_id: Uuid,
    merchant: String,
    #[serde(default)]
    reward: String,
    reward_cents: Option<i64>,
    #[serde(default)]
    min_spend_cents: i64,
    expires: NaiveDate,
}

pub async fn create_offer(State(state): State<AppState>, _: Auth, Json(body): Json<NewOffer>) -> AppResult<Json<Value>> {
    service::card(&state.pool, body.card_id).await?;
    let merchant = body.merchant.trim();
    if merchant.is_empty() {
        return Err(bad("merchant is required"));
    }
    if body.min_spend_cents < 0 || body.reward_cents.is_some_and(|v| v < 0) {
        return Err(bad("amounts cannot be negative"));
    }
    let today = service::today(&state.pool).await;
    let id = Uuid::new_v4();
    sqlx::query("insert into offers (id, card_id, merchant, reward, reward_cents, min_spend_cents, added_date, expires) values ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(id)
        .bind(body.card_id)
        .bind(merchant)
        .bind(body.reward.trim())
        .bind(body.reward_cents)
        .bind(body.min_spend_cents)
        .bind(today)
        .bind(body.expires)
        .execute(&state.pool)
        .await?;
    service::recompute_card(&state.pool, body.card_id).await?;
    service::audit(&state.pool, "offer_added", json!({ "offer_id": id, "merchant": merchant })).await;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct OfferPatch {
    /// "active" | "redeemed" | "expired"
    status: String,
}

pub async fn update_offer(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>, Json(body): Json<OfferPatch>) -> AppResult<Json<Value>> {
    if !matches!(body.status.as_str(), "active" | "redeemed" | "expired") {
        return Err(bad("status must be active, redeemed or expired"));
    }
    // A manual status drops the link to any transaction that auto-redeemed it.
    let res = sqlx::query("update offers set status = $2, redeemed_transaction_id = null where id = $1").bind(id).bind(&body.status).execute(&state.pool).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    service::audit(&state.pool, "offer_updated", json!({ "offer_id": id, "status": body.status })).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_offer(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let res = sqlx::query("delete from offers where id = $1").bind(id).execute(&state.pool).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn dismiss_alert(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    sqlx::query("update alerts set dismissed_at = now() where id = $1").bind(id).execute(&state.pool).await?;
    Ok(Json(json!({ "ok": true })))
}
