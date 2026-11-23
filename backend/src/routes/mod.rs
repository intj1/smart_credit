mod account;
mod admin;
mod data;

use axum::http::header::{HeaderName, HeaderValue};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::AppState;

// Plaid Link is the only third-party script; everything else is same-origin.
const CSP: &str = "default-src 'self'; script-src 'self' https://cdn.plaid.com; frame-src https://cdn.plaid.com; \
                   connect-src 'self' https://*.plaid.com; img-src 'self' data:; style-src 'self' 'unsafe-inline'; \
                   base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/healthz", get(|| async { Json(json!({ "ok": true })) }))
        // auth
        .route("/auth/login", post(account::login))
        .route("/auth/logout", post(account::logout))
        .route("/auth/me", get(account::me))
        .route("/auth/totp/setup", post(account::totp_setup))
        .route("/auth/totp/verify", post(account::totp_verify))
        // read models
        .route("/home", get(data::home))
        .route("/unclaimed", get(data::unclaimed))
        .route("/catalog", get(data::catalog))
        // cards
        .route("/cards", get(data::list_cards).post(data::create_card))
        .route("/cards/{id}", get(data::get_card).patch(data::update_card).delete(data::delete_card))
        .route("/cards/{id}/import", post(data::import_csv))
        .route("/cards/{id}/benefits/{benefit_id}", put(data::set_benefit_flags))
        // periods
        .route("/periods/{id}", get(data::get_period))
        .route("/periods/{id}/override", post(data::override_period))
        .route("/periods/{id}/exclusions", post(data::exclude_txn))
        .route("/periods/{id}/exclusions/{txn_id}", delete(data::unexclude_txn))
        // offers
        .route("/offers", get(data::list_offers).post(data::create_offer))
        .route("/offers/{id}", axum::routing::patch(data::update_offer).delete(data::delete_offer))
        // alerts
        .route("/alerts/{id}/dismiss", post(data::dismiss_alert))
        // connections and sync
        .route("/plaid/link-token", post(admin::link_token))
        .route("/plaid/exchange", post(admin::exchange))
        .route("/connections", get(admin::list_connections))
        .route("/connections/{id}", delete(admin::delete_connection))
        .route("/connections/{id}/accounts", get(admin::connection_accounts))
        .route("/connections/{id}/cards", post(admin::create_connection_cards))
        .route("/connections/{id}/reconnected", post(admin::reconnected))
        .route("/sync", post(admin::sync_now))
        // settings
        .route("/settings", get(admin::get_settings).put(admin::put_settings))
        .route("/settings/password", post(admin::change_password))
        .route("/settings/test-email", post(admin::test_email))
        .route("/audit", get(admin::audit_log))
        .route("/export", get(admin::export))
        .route("/delete-my-data", post(admin::delete_my_data))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({ "error": "not found", "code": "not_found" }))) })
        .layer(SetResponseHeaderLayer::overriding(HeaderName::from_static("cache-control"), HeaderValue::from_static("no-store")))
        .with_state(state.clone());

    let index = format!("{}/index.html", state.config.static_dir);
    let spa = ServeDir::new(&state.config.static_dir).fallback(ServeFile::new(index));

    Router::new()
        .nest("/api", api)
        .fallback_service(spa)
        .layer(SetResponseHeaderLayer::if_not_present(HeaderName::from_static("content-security-policy"), HeaderValue::from_static(CSP)))
        .layer(SetResponseHeaderLayer::if_not_present(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(HeaderName::from_static("referrer-policy"), HeaderValue::from_static("same-origin")))
        .layer(TraceLayer::new_for_http())
}
