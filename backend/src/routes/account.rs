use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::response::{AppendHeaders, IntoResponse};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{self, PartialAuth};
use crate::error::{bad, AppError, AppResult};
use crate::{crypto, service, AppState};

#[derive(Deserialize)]
pub struct Login {
    email: String,
    password: String,
}

fn next_step(state: &AppState, totp_enabled: bool, mfa_verified: bool) -> &'static str {
    if !state.config.require_mfa || mfa_verified {
        "ok"
    } else if totp_enabled {
        "totp"
    } else {
        "totp_setup"
    }
}

pub async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> AppResult<impl IntoResponse> {
    auth::check_throttle(&state)?;
    let user = service::user(&state.pool).await?;
    let ok = auth::verify_password(&body.password, &user.password_hash) && body.email.trim().eq_ignore_ascii_case(&user.email);
    if !ok {
        auth::record_failure(&state);
        service::audit(&state.pool, "login_failed", json!({})).await;
        return Err(bad("wrong email or password"));
    }
    let token = crypto::random_token();
    sqlx::query("insert into sessions (token_hash, user_id, mfa_verified, expires_at) values ($1,$2,$3, now() + make_interval(days => $4))")
        .bind(crypto::sha256_hex(&token))
        .bind(user.id)
        .bind(!state.config.require_mfa)
        .bind(auth::SESSION_DAYS as i32)
        .execute(&state.pool)
        .await?;
    if !state.config.require_mfa {
        auth::clear_failures(&state);
    }
    service::audit(&state.pool, "login", json!({})).await;
    let cookie = auth::session_cookie(&state, &token, auth::SESSION_DAYS * 86_400);
    Ok((AppendHeaders([(SET_COOKIE, cookie)]), Json(json!({ "next": next_step(&state, user.totp_enabled, false) }))))
}

pub async fn logout(State(state): State<AppState>, PartialAuth(session): PartialAuth) -> AppResult<impl IntoResponse> {
    sqlx::query("delete from sessions where token_hash = $1").bind(&session.token_hash).execute(&state.pool).await?;
    Ok((AppendHeaders([(SET_COOKIE, auth::session_cookie(&state, "", 0))]), Json(json!({ "ok": true }))))
}

pub async fn me(State(state): State<AppState>, PartialAuth(session): PartialAuth) -> AppResult<Json<Value>> {
    let user = service::user(&state.pool).await?;
    Ok(Json(json!({
        "email": user.email,
        "totp_enabled": user.totp_enabled,
        "next": next_step(&state, user.totp_enabled, session.mfa_verified),
    })))
}

/// Starts authenticator enrollment. Allowed with only the password step done,
/// but only while no authenticator is active; replacing one needs a full login.
pub async fn totp_setup(State(state): State<AppState>, PartialAuth(session): PartialAuth) -> AppResult<Json<Value>> {
    let user = service::user(&state.pool).await?;
    if user.totp_enabled && !session.mfa_verified {
        return Err(AppError::MfaRequired);
    }
    let secret = crypto::new_totp_secret();
    sqlx::query("update users set totp_secret_enc = $2, totp_enabled = false where id = $1")
        .bind(user.id)
        .bind(crypto::encrypt(&state.config.encryption_key, &secret)?)
        .execute(&state.pool)
        .await?;
    let label = user.email.replace(['?', '&', ':', ' '], "_");
    let url = format!("otpauth://totp/Card%20Benefits:{label}?secret={secret}&issuer=Card%20Benefits&digits=6&period=30");
    let qr_svg = qrcode::QrCode::new(url.as_bytes())
        .map_err(|e| anyhow::anyhow!("qr: {e}"))?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .build();
    service::audit(&state.pool, "totp_setup_started", json!({})).await;
    Ok(Json(json!({ "secret": secret, "otpauth_url": url, "qr_svg": qr_svg })))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
}

pub async fn totp_verify(State(state): State<AppState>, PartialAuth(session): PartialAuth, Json(body): Json<Code>) -> AppResult<Json<Value>> {
    auth::check_throttle(&state)?;
    let user = service::user(&state.pool).await?;
    let enc = user.totp_secret_enc.as_ref().ok_or_else(|| bad("set up an authenticator first"))?;
    let secret = crypto::decrypt(&state.config.encryption_key, enc)?;
    // Reject a wrong code, and a replay of one already used.
    let step = crypto::totp_verify(&secret, &body.code, auth::unix_now()).filter(|s| (*s as i64) > user.totp_last_step);
    let Some(step) = step else {
        auth::record_failure(&state);
        service::audit(&state.pool, "totp_failed", json!({})).await;
        return Err(bad("that code is not valid"));
    };
    auth::clear_failures(&state);
    sqlx::query("update users set totp_enabled = true, totp_last_step = $2 where id = $1").bind(user.id).bind(step as i64).execute(&state.pool).await?;
    sqlx::query("update sessions set mfa_verified = true where token_hash = $1").bind(&session.token_hash).execute(&state.pool).await?;
    if !user.totp_enabled {
        service::audit(&state.pool, "totp_enabled", json!({})).await;
    }
    Ok(Json(json!({ "next": "ok" })))
}
