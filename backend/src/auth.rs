//! Single-user login: password (argon2) plus TOTP, cookie sessions.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::header::COOKIE;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::error::AppError;
use crate::{crypto, AppState};

pub const COOKIE_NAME: &str = "cbt_session";
pub const SESSION_DAYS: i64 = 30;
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow!("hashing failed: {e}"))?.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok()).unwrap_or(false)
}

/// Creates the one user from ADMIN_EMAIL / ADMIN_PASSWORD on first start.
/// Later starts ignore those variables, so they can be removed afterwards.
pub async fn seed_user(state: &AppState) -> Result<()> {
    let exists: bool = sqlx::query_scalar("select exists (select 1 from users)").fetch_one(&state.pool).await?;
    if exists {
        return Ok(());
    }
    let (Some(email), Some(password)) = (&state.config.admin_email, &state.config.admin_password) else {
        return Err(anyhow!("first start: set ADMIN_EMAIL and ADMIN_PASSWORD to create the login"));
    };
    if password.len() < 10 {
        return Err(anyhow!("ADMIN_PASSWORD must be at least 10 characters"));
    }
    sqlx::query("insert into users (id, email, password_hash, alert_email) values ($1, $2, $3, $2)")
        .bind(Uuid::new_v4())
        .bind(email.to_lowercase())
        .bind(hash_password(password)?)
        .execute(&state.pool)
        .await?;
    tracing::info!(%email, "created the app user");
    Ok(())
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Shared throttle for password and TOTP attempts.
pub fn check_throttle(state: &AppState) -> Result<(), AppError> {
    let mut guard = state.login_failures.lock().unwrap();
    if guard.1.elapsed() > LOCKOUT {
        *guard = (0, Instant::now());
    }
    if guard.0 >= MAX_FAILURES {
        return Err(AppError::TooManyRequests);
    }
    Ok(())
}

pub fn record_failure(state: &AppState) {
    let mut guard = state.login_failures.lock().unwrap();
    if guard.0 == 0 {
        guard.1 = Instant::now();
    }
    guard.0 += 1;
}

pub fn clear_failures(state: &AppState) {
    *state.login_failures.lock().unwrap() = (0, Instant::now());
}

pub fn session_cookie(state: &AppState, token: &str, max_age_secs: i64) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{}",
        if state.config.cookie_secure { "; Secure" } else { "" }
    )
}

pub fn token_from(parts: &Parts) -> Option<String> {
    parts.headers.get_all(COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE_NAME).then(|| v.to_string())
    })
}

#[derive(Debug, Clone)]
pub struct Session {
    pub token_hash: String,
    pub user_id: Uuid,
    pub mfa_verified: bool,
}

async fn load_session(parts: &Parts, state: &AppState) -> Result<Session, AppError> {
    let token = token_from(parts).ok_or(AppError::Unauthorized)?;
    let token_hash = crypto::sha256_hex(&token);
    let row: Option<(Uuid, bool)> =
        sqlx::query_as("select user_id, mfa_verified from sessions where token_hash = $1 and expires_at > now()")
            .bind(&token_hash)
            .fetch_optional(&state.pool)
            .await?;
    let (user_id, mfa_verified) = row.ok_or(AppError::Unauthorized)?;
    Ok(Session { token_hash, user_id, mfa_verified })
}

/// A session that passed the password step; TOTP may still be outstanding.
pub struct PartialAuth(pub Session);

impl FromRequestParts<AppState> for PartialAuth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        load_session(parts, state).await.map(PartialAuth)
    }
}

/// A fully signed-in session. Every data route takes this.
pub struct Auth(pub Session);

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let session = load_session(parts, state).await?;
        if state.config.require_mfa && !session.mfa_verified {
            return Err(AppError::MfaRequired);
        }
        Ok(Auth(session))
    }
}
