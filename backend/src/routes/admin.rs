use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::data::{insert_card, NewCard};
use crate::auth::{self, Auth};
use crate::catalog::{self, Product};
use crate::error::{bad, AppError, AppResult};
use crate::plaid::Plaid;
use crate::{alerts, crypto, service, sync, AppState};

fn plaid(state: &AppState) -> AppResult<&Plaid> {
    state.plaid.as_ref().ok_or_else(|| bad("Plaid is not configured on this server. Add the card manually and import a statement CSV."))
}

fn upstream(e: crate::plaid::PlaidError) -> AppError {
    AppError::Upstream(e.to_string())
}

async fn access_token(state: &AppState, connection_id: Uuid) -> AppResult<String> {
    let enc: String = sqlx::query_scalar("select access_token_enc from connections where id = $1").bind(connection_id).fetch_one(&state.pool).await?;
    Ok(crypto::decrypt(&state.config.encryption_key, &enc)?)
}

// -------------------------------------------------------------------- plaid

#[derive(Deserialize)]
pub struct LinkTokenBody {
    /// Set to re-authenticate an existing connection (Plaid update mode).
    connection_id: Option<Uuid>,
}

pub async fn link_token(State(state): State<AppState>, Auth(session): Auth, Json(body): Json<LinkTokenBody>) -> AppResult<Json<Value>> {
    let plaid = plaid(&state)?;
    let existing = match body.connection_id {
        Some(id) => Some(access_token(&state, id).await?),
        None => None,
    };
    let token = plaid.link_token(&session.user_id.to_string(), existing.as_deref()).await.map_err(upstream)?;
    Ok(Json(json!({ "link_token": token, "redirect_uri": plaid.redirect_uri() })))
}

async fn accounts_json(state: &AppState, connection_id: Uuid) -> AppResult<Vec<Value>> {
    let plaid = plaid(state)?;
    let provider: String = sqlx::query_scalar("select provider from connections where id = $1").bind(connection_id).fetch_one(&state.pool).await?;
    let token = access_token(state, connection_id).await?;
    let accounts = plaid.accounts(&token).await.map_err(upstream)?;
    let products: Vec<Product> = sqlx::query_as(
        "select id, provider, issuer, name, annual_fee_cents, catalog_version, name_hints from card_products where active",
    )
    .fetch_all(&state.pool)
    .await?;
    let tracked: Vec<String> =
        sqlx::query_scalar("select unnest(account_ids) from cards where connection_id = $1").bind(connection_id).fetch_all(&state.pool).await?;
    Ok(accounts
        .iter()
        .filter(|a| a.kind == "credit" || a.subtype.as_deref() == Some("credit card"))
        .map(|a| {
            let label = format!("{} {}", a.official_name.as_deref().unwrap_or(""), a.name);
            json!({
                "account_id": a.account_id,
                "name": a.official_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.name.clone()),
                "mask": a.mask,
                "suggested_product_id": catalog::suggest_product(&products, &provider, &label).map(|p| p.id.clone()),
                "tracked": tracked.contains(&a.account_id),
            })
        })
        .collect())
}

#[derive(Deserialize)]
pub struct ExchangeBody {
    public_token: String,
    provider: String,
    institution_name: Option<String>,
}

/// Step 2 of "Add an account": swap Plaid's public token for an access token,
/// store it encrypted, and return the card accounts on that login.
pub async fn exchange(State(state): State<AppState>, Auth(session): Auth, Json(body): Json<ExchangeBody>) -> AppResult<Json<Value>> {
    let plaid = plaid(&state)?;
    if !catalog::PROVIDERS.contains(&body.provider.as_str()) {
        return Err(bad("provider must be capital_one, amex or bilt"));
    }
    let (token, item_id) = plaid.exchange(&body.public_token).await.map_err(upstream)?;
    let id = Uuid::new_v4();
    sqlx::query("insert into connections (id, user_id, provider, institution_name, item_id, access_token_enc) values ($1,$2,$3,$4,$5,$6)")
        .bind(id)
        .bind(session.user_id)
        .bind(&body.provider)
        .bind(&body.institution_name)
        .bind(&item_id)
        .bind(crypto::encrypt(&state.config.encryption_key, &token)?)
        .execute(&state.pool)
        .await?;
    service::audit(&state.pool, "connection_added", json!({ "connection_id": id, "provider": body.provider })).await;
    Ok(Json(json!({ "connection_id": id, "accounts": accounts_json(&state, id).await? })))
}

pub async fn connection_accounts(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "connection_id": id, "accounts": accounts_json(&state, id).await? })))
}

#[derive(Deserialize)]
pub struct ConnectionCards {
    cards: Vec<NewCard>,
}

/// Steps 3-6: which accounts to track (several accounts may roll up into one
/// card, e.g. authorized users), the product, anniversary and enrollments.
pub async fn create_connection_cards(
    State(state): State<AppState>,
    Auth(session): Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<ConnectionCards>,
) -> AppResult<Json<Value>> {
    let exists: bool = sqlx::query_scalar("select exists (select 1 from connections where id = $1)").bind(id).fetch_one(&state.pool).await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    if body.cards.is_empty() || body.cards.iter().any(|c| c.account_ids.is_empty()) {
        return Err(bad("choose at least one account for each card"));
    }
    let mut ids = Vec::new();
    for card in body.cards {
        ids.push(insert_card(&state.pool, session.user_id, Some(id), card).await?);
    }
    // Pull from the beginning so the new cards get their full history.
    sqlx::query("update connections set sync_cursor = null where id = $1").bind(id).execute(&state.pool).await?;
    sync::spawn_initial_sync(state.clone(), id);
    Ok(Json(json!({ "card_ids": ids })))
}

/// Called after Plaid update mode succeeds for a broken connection.
pub async fn reconnected(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let res = sqlx::query("update connections set status = 'ok', error = null where id = $1").bind(id).execute(&state.pool).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    service::audit(&state.pool, "connection_reconnected", json!({ "connection_id": id })).await;
    sync::spawn_initial_sync(state.clone(), id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_connections(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', c.id, 'provider', c.provider, 'institution_name', c.institution_name,
                  'status', c.status, 'error', c.error, 'last_sync_at', c.last_sync_at, 'created_at', c.created_at,
                  'cards', coalesce((select json_agg(json_build_object('id', k.id, 'last4', k.last4,
                               'name', case when k.nickname <> '' then k.nickname else p.name end))
                            from cards k join card_products p on p.id = k.product_id where k.connection_id = c.id), '[]'::json))
         from connections c order by c.created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "connections": rows, "plaid_configured": state.plaid.is_some() })))
}

/// Revokes the link at Plaid, then deletes the connection with its cards and
/// their transactions.
async fn remove_connection(state: &AppState, id: Uuid) -> AppResult<Option<String>> {
    let mut warning = None;
    if let Some(plaid) = &state.plaid {
        match access_token(state, id).await {
            Ok(token) => {
                if let Err(e) = plaid.item_remove(&token).await {
                    tracing::warn!(error = %e, "could not revoke the Plaid item");
                    warning = Some(format!("Deleted here, but Plaid did not confirm the revocation: {e}"));
                }
            }
            Err(AppError::NotFound) => return Err(AppError::NotFound),
            Err(e) => warning = Some(format!("Deleted here, but the stored token could not be read to revoke it: {e}")),
        }
    } else {
        warning = Some("Deleted here. Plaid is not configured, so the link could not be revoked at Plaid.".into());
    }
    let res = sqlx::query("delete from connections where id = $1").bind(id).execute(&state.pool).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    service::audit(&state.pool, "connection_removed", json!({ "connection_id": id, "revoked": warning.is_none() })).await;
    Ok(warning)
}

pub async fn delete_connection(State(state): State<AppState>, _: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let warning = remove_connection(&state, id).await?;
    Ok(Json(json!({ "ok": true, "warning": warning })))
}

pub async fn sync_now(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let summary = sync::sync_all(&state, "refresh").await?;
    Ok(Json(json!({ "summary": summary })))
}

// ----------------------------------------------------------------- settings

pub async fn get_settings(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let user = service::user(&state.pool).await?;
    let versions: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', id, 'name', name, 'catalog_version', catalog_version) from card_products where active order by name",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({
        "email": user.email,
        "timezone": user.timezone,
        "alert_email": user.alert_email,
        "alert_days": user.alert_days,
        "alerts_enabled": user.alerts_enabled,
        "totp_enabled": user.totp_enabled,
        "mail_configured": state.config.mail.is_some(),
        "plaid_configured": state.plaid.is_some(),
        "sync_hour": state.config.sync_hour,
        "alert_hour": state.config.alert_hour,
        "catalog": versions,
    })))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    timezone: String,
    alert_email: Option<String>,
    alert_days: Vec<i32>,
    alerts_enabled: bool,
}

pub async fn put_settings(State(state): State<AppState>, _: Auth, Json(body): Json<SettingsBody>) -> AppResult<Json<Value>> {
    if body.timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(bad("unknown timezone; use an IANA name such as America/New_York"));
    }
    let mut days: Vec<i32> = body.alert_days.into_iter().filter(|d| (0..=60).contains(d)).collect();
    days.sort_unstable_by(|a, b| b.cmp(a));
    days.dedup();
    let email = body.alert_email.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
    if email.as_ref().is_some_and(|e| e.parse::<lettre::Address>().is_err()) {
        return Err(bad("alert email is not a valid address"));
    }
    sqlx::query("update users set timezone = $1, alert_email = $2, alert_days = $3, alerts_enabled = $4")
        .bind(&body.timezone)
        .bind(&email)
        .bind(&days)
        .bind(body.alerts_enabled)
        .execute(&state.pool)
        .await?;
    // "Today" may have moved with the timezone.
    service::recompute_all(&state.pool).await?;
    service::audit(&state.pool, "settings_updated", json!({ "timezone": body.timezone, "alert_days": days })).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

pub async fn change_password(State(state): State<AppState>, Auth(session): Auth, Json(body): Json<PasswordBody>) -> AppResult<Json<Value>> {
    auth::check_throttle(&state)?;
    let user = service::user(&state.pool).await?;
    if !auth::verify_password(&body.current, &user.password_hash) {
        auth::record_failure(&state);
        return Err(bad("current password is wrong"));
    }
    if body.new.len() < 10 {
        return Err(bad("new password must be at least 10 characters"));
    }
    sqlx::query("update users set password_hash = $2 where id = $1").bind(user.id).bind(auth::hash_password(&body.new)?).execute(&state.pool).await?;
    // Sign out everywhere else.
    sqlx::query("delete from sessions where token_hash <> $1").bind(&session.token_hash).execute(&state.pool).await?;
    service::audit(&state.pool, "password_changed", json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn test_email(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let user = service::user(&state.pool).await?;
    let to = user.alert_email.filter(|e| !e.is_empty()).ok_or_else(|| bad("set an alert email first"))?;
    alerts::send_email(&state, &to, "Card Benefits Tracker: test email", "Alerts from Card Benefits Tracker will arrive at this address.\n".into())
        .await
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "to": to })))
}

pub async fn audit_log(State(state): State<AppState>, _: Auth) -> AppResult<Json<Value>> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', id, 'at', at, 'action', action, 'detail', detail) from audit_log order by id desc limit 200",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "entries": rows })))
}

/// Everything the app holds about you, minus secrets (password hash, TOTP
/// secret, aggregator tokens).
pub async fn export(State(state): State<AppState>, _: Auth) -> AppResult<impl IntoResponse> {
    async fn table(state: &AppState, sql: &str) -> AppResult<Value> {
        Ok(sqlx::query_scalar(&format!("select coalesce(json_agg(t), '[]'::json) from ({sql}) t")).fetch_one(&state.pool).await?)
    }
    let out = json!({
        "exported_at": chrono::Utc::now(),
        "user": table(&state, "select id, email, timezone, alert_email, alert_days, alerts_enabled, totp_enabled, created_at from users").await?,
        "connections": table(&state, "select id, provider, institution_name, status, error, last_sync_at, created_at from connections").await?,
        "cards": table(&state, "select * from cards").await?,
        "card_benefits": table(&state, "select * from card_benefits").await?,
        "benefit_periods": table(&state, "select * from benefit_periods order by card_id, benefit_id, start_date").await?,
        "transactions": table(&state, "select * from transactions order by card_id, posted_date").await?,
        "benefit_matches": table(&state, "select * from benefit_matches").await?,
        "match_exclusions": table(&state, "select * from match_exclusions").await?,
        "offers": table(&state, "select * from offers").await?,
        "alerts": table(&state, "select * from alerts order by created_at").await?,
        "audit_log": table(&state, "select * from audit_log order by id").await?,
    });
    service::audit(&state.pool, "export", json!({})).await;
    Ok((
        [(CONTENT_TYPE, "application/json"), (CONTENT_DISPOSITION, "attachment; filename=\"card-benefits-export.json\"")],
        serde_json::to_string_pretty(&out).map_err(|e| anyhow::anyhow!(e))?,
    ))
}

#[derive(Deserialize)]
pub struct DeleteBody {
    confirm: String,
}

/// Revokes every aggregator link and deletes all card data. The login itself
/// stays so the app remains usable.
pub async fn delete_my_data(State(state): State<AppState>, _: Auth, Json(body): Json<DeleteBody>) -> AppResult<Json<Value>> {
    if body.confirm != "DELETE" {
        return Err(bad("type DELETE to confirm"));
    }
    let _guard = state.sync_lock.lock().await;
    let ids: Vec<Uuid> = sqlx::query_scalar("select id from connections").fetch_all(&state.pool).await?;
    let mut warnings = Vec::new();
    for id in ids {
        if let Some(w) = remove_connection(&state, id).await? {
            warnings.push(w);
        }
    }
    let mut tx = state.pool.begin().await?;
    for table in ["cards", "alerts", "audit_log"] {
        sqlx::query(&format!("delete from {table}")).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    service::audit(&state.pool, "data_deleted", json!({})).await;
    Ok(Json(json!({ "ok": true, "warnings": warnings })))
}
