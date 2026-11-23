use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not signed in")]
    Unauthorized,
    #[error("two-factor verification required")]
    MfaRequired,
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("too many attempts, try again in a minute")]
    TooManyRequests,
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

pub fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound,
            other => AppError::Internal(other.into()),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::MfaRequired => (StatusCode::UNAUTHORIZED, "mfa_required"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let message = match &self {
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message, "code": code }))).into_response()
    }
}
