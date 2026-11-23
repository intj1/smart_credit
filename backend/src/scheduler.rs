//! Background jobs: nightly sync and the daily alert run, both keyed to the
//! user's local clock. Each runs at most once per local day; if the machine
//! was off at the scheduled hour, the job runs when it comes back.

use std::time::Duration;

use chrono::{NaiveDate, Timelike, Utc};

use crate::{alerts, service, sync, AppState};

pub async fn run(state: AppState) {
    let mut last_sync: Option<NaiveDate> = None;
    let mut last_alerts: Option<NaiveDate> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    loop {
        tick.tick().await;
        let Ok(user) = service::user(&state.pool).await else { continue };
        let tz: chrono_tz::Tz = user.timezone.parse().unwrap_or(chrono_tz::UTC);
        let now = Utc::now().with_timezone(&tz);
        let today = now.date_naive();

        if now.hour() >= state.config.sync_hour && last_sync != Some(today) {
            last_sync = Some(today);
            match sync::sync_all(&state, "nightly").await {
                Ok(summary) => tracing::info!(?summary, "nightly sync done"),
                Err(e) => tracing::error!(error = ?e, "nightly sync failed"),
            }
            if let Err(e) = sqlx::query("delete from sessions where expires_at < now()").execute(&state.pool).await {
                tracing::warn!(error = %e, "session cleanup failed");
            }
        }
        // Alerts run after the day's sync so they describe fresh data.
        if now.hour() >= state.config.alert_hour.max(state.config.sync_hour) && last_alerts != Some(today) {
            last_alerts = Some(today);
            if let Err(e) = alerts::run_daily(&state).await {
                tracing::error!(error = ?e, "alert run failed");
            }
        }
    }
}
