//! Alerts: built once a day from the database, stored (so the Home screen can
//! show them) and emailed as one digest. Each alert has a stable key, so a
//! rerun never repeats one.

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate};
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::service::{self, User};
use crate::views::{fmt_amount, period_views, PeriodFilter, PeriodView};
use crate::AppState;

struct NewAlert {
    key: String,
    kind: &'static str,
    title: String,
    body: String,
}

fn label(p: &PeriodView) -> String {
    format!("{} · {}", p.card_name, p.benefit_name)
}

/// Works out today's alerts and stores the new ones. Returns how many were new.
pub async fn generate(pool: &PgPool, user: &User, today: NaiveDate) -> Result<usize> {
    let mut alerts: Vec<NewAlert> = Vec::new();
    let open = period_views(pool, &PeriodFilter { open_on: Some(today), ..Default::default() }, today).await?;

    // 1. Period ending soon with money left.
    let mut thresholds: Vec<i64> = user.alert_days.iter().map(|d| *d as i64).filter(|d| *d >= 0).collect();
    thresholds.sort();
    for p in open.iter().filter(|p| p.kind != "mortgage_points" && p.remaining > 0 && !matches!(p.status.as_str(), "used" | "not_for_me")) {
        let Some(days_left) = p.days_left else { continue };
        // The tightest threshold reached, so a missed day still alerts once.
        if let Some(t) = thresholds.iter().find(|t| days_left <= **t) {
            alerts.push(NewAlert {
                key: format!("expiring:{}:{t}", p.id),
                kind: "expiring",
                title: format!("{} left on {} — {}", fmt_amount(p.remaining, &p.unit), label(p), match days_left {
                    0 => "ends today".to_string(),
                    1 => "ends tomorrow".to_string(),
                    n => format!("ends in {n} days"),
                }),
                body: p.how_to_use.clone(),
            });
        }
    }

    // 2. First run of the month: what just opened.
    let month_start = today.with_day(1).unwrap();
    let opened: Vec<&PeriodView> = open
        .iter()
        .filter(|p| p.start_date == month_start && matches!(p.cadence.as_str(), "monthly" | "quarterly") && p.kind != "mortgage_points" && p.status != "not_for_me")
        .collect();
    if !opened.is_empty() {
        let total: i64 = opened.iter().filter(|p| p.unit == "usd").map(|p| p.value).sum();
        alerts.push(NewAlert {
            key: format!("opened:{}", month_start.format("%Y-%m")),
            kind: "opened",
            title: format!("{} credits opened for {} ({})", opened.len(), month_start.format("%B"), fmt_amount(total, "usd")),
            body: opened.iter().map(|p| format!("{} — {}", label(p), fmt_amount(p.value, &p.unit))).collect::<Vec<_>>().join("\n"),
        });
    }

    // 3. Enrollment still needed.
    for p in open.iter().filter(|p| p.status == "needs_enrollment") {
        alerts.push(NewAlert {
            key: format!("enroll:{}:{}:{}", p.card_id, p.benefit_id, today.year()),
            kind: "enrollment",
            title: format!("Enroll for {}", label(p)),
            body: format!("This benefit needs enrollment with the issuer before purchases earn the credit. Tick it as enrolled in the app once done.\n{}", p.how_to_use),
        });
    }

    // 4. Pending credit that never posted. Includes recently closed periods.
    let recent = period_views(pool, &PeriodFilter { flagged: true, ended_after: Some(today - Duration::days(120)), ..Default::default() }, today).await?;
    for p in &recent {
        alerts.push(NewAlert {
            key: format!("stale_pending:{}", p.id),
            kind: "stale_pending",
            title: format!("No credit posted for {}", label(p)),
            body: "A qualifying purchase has gone 2 statement cycles without its statement credit. Check enrollment or eligibility with the issuer.".into(),
        });
    }

    // 5. Connections that stopped syncing (re-raised weekly while broken).
    let broken: Vec<(Uuid, String, Option<String>, String)> = sqlx::query_as(
        "select id, provider, institution_name, status from connections
         where status <> 'ok' or last_sync_at is null or last_sync_at < now() - interval '3 days'",
    )
    .fetch_all(pool)
    .await?;
    for (id, provider, name, status) in broken {
        let week = today.iso_week();
        alerts.push(NewAlert {
            key: format!("connection:{id}:{}-{}", week.year(), week.week()),
            kind: "connection",
            title: format!("{} stopped syncing", name.unwrap_or(provider)),
            body: if status == "needs_reauth" {
                "The issuer needs you to sign in again. Open Settings and reconnect.".into()
            } else {
                "No successful sync for 3 days. Open Settings to check the connection, or import a statement CSV.".into()
            },
        });
    }

    let mut created = 0;
    for a in alerts {
        let res = sqlx::query("insert into alerts (id, key, kind, title, body) values ($1,$2,$3,$4,$5) on conflict (key) do nothing")
            .bind(Uuid::new_v4())
            .bind(&a.key)
            .bind(a.kind)
            .bind(&a.title)
            .bind(&a.body)
            .execute(pool)
            .await?;
        created += res.rows_affected() as usize;
    }
    Ok(created)
}

pub async fn send_email(state: &AppState, to: &str, subject: &str, body: String) -> Result<()> {
    let mail = state.config.mail.as_ref().context("email is not configured (SMTP_URL, MAIL_FROM)")?;
    let message = Message::builder()
        .from(mail.from.parse::<Mailbox>().context("MAIL_FROM is not a valid address")?)
        .to(to.parse::<Mailbox>().context("alert email is not a valid address")?)
        .subject(subject)
        .body(body)?;
    let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(&mail.smtp_url).context("SMTP_URL is not valid")?.build();
    transport.send(message).await.context("sending email")?;
    Ok(())
}

/// Emails every stored alert that has not gone out yet, as one digest.
pub async fn send_pending(state: &AppState, user: &User) -> Result<usize> {
    if state.config.mail.is_none() || !user.alerts_enabled {
        return Ok(0);
    }
    let Some(to) = user.alert_email.clone().filter(|e| !e.is_empty()) else { return Ok(0) };
    let pending: Vec<(Uuid, String, String)> =
        sqlx::query_as("select id, title, body from alerts where emailed_at is null and dismissed_at is null order by created_at")
            .fetch_all(&state.pool)
            .await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let mut text = String::new();
    for (_, title, body) in &pending {
        text.push_str(&format!("• {title}\n"));
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            text.push_str(&format!("    {line}\n"));
        }
        text.push('\n');
    }
    let subject = if pending.len() == 1 { pending[0].1.clone() } else { format!("Card benefits: {} things to look at", pending.len()) };
    send_email(state, &to, &subject, text).await?;
    let ids: Vec<Uuid> = pending.iter().map(|p| p.0).collect();
    sqlx::query("update alerts set emailed_at = now() where id = any($1)").bind(&ids).execute(&state.pool).await?;
    Ok(pending.len())
}

pub async fn run_daily(state: &AppState) -> Result<()> {
    let user = service::user(&state.pool).await?;
    let today = service::today_in(&user.timezone);
    let created = generate(&state.pool, &user, today).await?;
    let emailed = send_pending(state, &user).await?;
    service::audit(&state.pool, "alerts", json!({ "created": created, "emailed": emailed })).await;
    Ok(())
}
