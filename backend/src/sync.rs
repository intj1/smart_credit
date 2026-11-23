//! The sync worker: pulls transactions for every connection, then re-runs the
//! matching engine. Used by the nightly job and the Refresh button.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::plaid::{PlaidError, PlaidTxn};
use crate::{crypto, service, AppState};

#[derive(Debug, Default, Serialize)]
pub struct SyncSummary {
    pub connections: usize,
    pub failed: usize,
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
}

#[derive(sqlx::FromRow)]
struct ConnRow {
    id: Uuid,
    access_token_enc: String,
    sync_cursor: Option<String>,
}

fn to_cents(amount: f64) -> i64 {
    (amount * 100.0).round() as i64
}

async fn upsert(tx: &mut sqlx::PgConnection, card_id: Uuid, t: &PlaidTxn) -> Result<()> {
    let description = match &t.original_description {
        Some(orig) if !orig.is_empty() && !orig.eq_ignore_ascii_case(&t.name) => format!("{} | {}", t.name, orig),
        _ => t.name.clone(),
    };
    sqlx::query(
        "insert into transactions (id, card_id, posted_date, amount_cents, merchant, description, category, pending, aggregator_id, source)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,'plaid')
         on conflict (card_id, aggregator_id) do update set posted_date = excluded.posted_date,
           amount_cents = excluded.amount_cents, merchant = excluded.merchant, description = excluded.description,
           category = excluded.category, pending = excluded.pending",
    )
    .bind(Uuid::new_v4())
    .bind(card_id)
    .bind(t.date)
    .bind(to_cents(t.amount))
    .bind(t.merchant_name.clone().unwrap_or_default())
    .bind(description)
    .bind(t.personal_finance_category.as_ref().and_then(|c| c.detailed.clone()))
    .bind(t.pending)
    .bind(&t.transaction_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Pulls everything new for one connection. All pages are applied in one
/// database transaction together with the new cursor, so a failure part-way
/// leaves the previous state intact.
pub async fn sync_connection(state: &AppState, connection_id: Uuid) -> Result<SyncSummary> {
    let plaid = state.plaid.as_ref().ok_or_else(|| anyhow!("Plaid is not configured"))?;
    let conn: ConnRow = sqlx::query_as("select id, access_token_enc, sync_cursor from connections where id = $1")
        .bind(connection_id)
        .fetch_one(&state.pool)
        .await?;
    let token = crypto::decrypt(&state.config.encryption_key, &conn.access_token_enc)?;

    let cards: Vec<(Uuid, Vec<String>)> =
        sqlx::query_as("select id, account_ids from cards where connection_id = $1").bind(conn.id).fetch_all(&state.pool).await?;
    let by_account: HashMap<&str, Uuid> =
        cards.iter().flat_map(|(id, accounts)| accounts.iter().map(move |a| (a.as_str(), *id))).collect();

    let mut summary = SyncSummary { connections: 1, ..Default::default() };
    let mut cursor = conn.sync_cursor.clone();
    let result: Result<(), PlaidError> = async {
        let mut tx = state.pool.begin().await.map_err(|e| PlaidError::Shape(e.to_string()))?;
        loop {
            let page = plaid.transactions_sync(&token, cursor.as_deref()).await?;
            for t in page.added.iter().chain(page.modified.iter()) {
                // Accounts the user chose not to track are skipped.
                if let Some(&card_id) = by_account.get(t.account_id.as_str()) {
                    upsert(&mut tx, card_id, t).await.map_err(|e| PlaidError::Shape(e.to_string()))?;
                }
            }
            for r in &page.removed {
                sqlx::query("delete from transactions t using cards c where c.id = t.card_id and c.connection_id = $1 and t.aggregator_id = $2")
                    .bind(conn.id)
                    .bind(&r.transaction_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| PlaidError::Shape(e.to_string()))?;
            }
            summary.added += page.added.len();
            summary.modified += page.modified.len();
            summary.removed += page.removed.len();
            cursor = Some(page.next_cursor);
            if !page.has_more {
                break;
            }
        }
        // Only keep the cursor once cards exist, so nothing is skipped for
        // cards that are confirmed after the first pull.
        let keep_cursor = if by_account.is_empty() { conn.sync_cursor.clone() } else { cursor.clone() };
        sqlx::query("update connections set sync_cursor = $2, status = 'ok', error = null, last_sync_at = now() where id = $1")
            .bind(conn.id)
            .bind(keep_cursor)
            .execute(&mut *tx)
            .await
            .map_err(|e| PlaidError::Shape(e.to_string()))?;
        tx.commit().await.map_err(|e| PlaidError::Shape(e.to_string()))?;
        Ok(())
    }
    .await;

    if let Err(e) = result {
        let status = if e.needs_reauth() { "needs_reauth" } else { "error" };
        sqlx::query("update connections set status = $2, error = $3 where id = $1")
            .bind(conn.id)
            .bind(status)
            .bind(e.to_string())
            .execute(&state.pool)
            .await?;
        return Err(e.into());
    }
    for (card_id, _) in &cards {
        service::recompute_card(&state.pool, *card_id).await?;
    }
    Ok(summary)
}

/// Syncs every connection and recomputes every card (manual cards too, so
/// periods roll over even with no aggregator). Writes one audit entry.
pub async fn sync_all(state: &AppState, trigger: &str) -> Result<SyncSummary> {
    let _guard = state.sync_lock.lock().await;
    let mut total = SyncSummary::default();
    if state.plaid.is_some() {
        let ids: Vec<Uuid> = sqlx::query_scalar("select id from connections order by created_at").fetch_all(&state.pool).await?;
        for id in ids {
            total.connections += 1;
            match sync_connection(state, id).await {
                Ok(s) => {
                    total.added += s.added;
                    total.modified += s.modified;
                    total.removed += s.removed;
                }
                Err(e) => {
                    total.failed += 1;
                    tracing::warn!(connection = %id, error = %e, "sync failed");
                }
            }
        }
    }
    service::recompute_all(&state.pool).await?;
    service::audit(&state.pool, "sync", json!({ "trigger": trigger, "summary": total })).await;
    Ok(total)
}

/// After a new link, Plaid needs a few seconds to prepare history. Retry
/// briefly so matched usage shows up within the first minute.
pub fn spawn_initial_sync(state: AppState, connection_id: Uuid) {
    tokio::spawn(async move {
        for delay in [0u64, 8, 15, 30] {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            let guard = state.sync_lock.lock().await;
            let result = sync_connection(&state, connection_id).await;
            drop(guard);
            match result {
                Ok(s) if s.added > 0 => {
                    service::audit(&state.pool, "sync", json!({ "trigger": "link", "summary": s })).await;
                    break;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "initial sync attempt failed"),
            }
        }
    });
}
