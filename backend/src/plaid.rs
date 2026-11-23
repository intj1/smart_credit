//! Minimal Plaid REST client (Plaid has no official Rust SDK). Read-only
//! products only: transactions. Nothing here can move money.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::PlaidConfig;

#[derive(Debug, thiserror::Error)]
pub enum PlaidError {
    #[error("Plaid error {code}: {message}")]
    Api { code: String, message: String },
    #[error("Plaid request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected Plaid response: {0}")]
    Shape(String),
}

impl PlaidError {
    pub fn needs_reauth(&self) -> bool {
        matches!(self, PlaidError::Api { code, .. }
            if matches!(code.as_str(), "ITEM_LOGIN_REQUIRED" | "PENDING_EXPIRATION" | "PENDING_DISCONNECT" | "ACCESS_NOT_GRANTED" | "USER_PERMISSION_REVOKED"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub account_id: String,
    #[serde(default)]
    pub name: String,
    pub official_name: Option<String>,
    pub mask: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
    pub subtype: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaidTxn {
    pub transaction_id: String,
    pub account_id: String,
    pub amount: f64,
    pub date: chrono::NaiveDate,
    #[serde(default)]
    pub name: String,
    pub merchant_name: Option<String>,
    pub original_description: Option<String>,
    #[serde(default)]
    pub pending: bool,
    pub personal_finance_category: Option<Pfc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pfc {
    pub detailed: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Removed {
    pub transaction_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SyncPage {
    #[serde(default)]
    pub added: Vec<PlaidTxn>,
    #[serde(default)]
    pub modified: Vec<PlaidTxn>,
    #[serde(default)]
    pub removed: Vec<Removed>,
    pub next_cursor: String,
    #[serde(default)]
    pub has_more: bool,
}

pub struct Plaid {
    cfg: PlaidConfig,
    http: reqwest::Client,
}

impl Plaid {
    pub fn new(cfg: PlaidConfig) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("http client");
        Plaid { cfg, http }
    }

    pub fn redirect_uri(&self) -> Option<&str> {
        self.cfg.redirect_uri.as_deref()
    }

    async fn post(&self, path: &str, mut body: Value) -> Result<Value, PlaidError> {
        body["client_id"] = json!(self.cfg.client_id);
        body["secret"] = json!(self.cfg.secret);
        let resp = self.http.post(format!("{}{}", self.cfg.base_url, path)).json(&body).send().await?;
        let ok = resp.status().is_success();
        let value: Value = resp.json().await?;
        if ok {
            Ok(value)
        } else {
            Err(PlaidError::Api {
                code: value["error_code"].as_str().unwrap_or("UNKNOWN").to_string(),
                message: value["error_message"].as_str().unwrap_or("").to_string(),
            })
        }
    }

    /// Link token for a new connection, or for re-authenticating an existing
    /// one when `access_token` is given (update mode).
    pub async fn link_token(&self, user_id: &str, access_token: Option<&str>) -> Result<String, PlaidError> {
        let mut body = json!({
            "client_name": "Card Benefits Tracker",
            "language": "en",
            "country_codes": ["US"],
            "user": { "client_user_id": user_id },
        });
        match access_token {
            Some(token) => body["access_token"] = json!(token),
            None => {
                body["products"] = json!(["transactions"]);
                body["transactions"] = json!({ "days_requested": 730 });
                body["account_filters"] = json!({ "credit": { "account_subtypes": ["credit card"] } });
            }
        }
        if let Some(uri) = &self.cfg.redirect_uri {
            body["redirect_uri"] = json!(uri);
        }
        let v = self.post("/link/token/create", body).await?;
        v["link_token"].as_str().map(str::to_string).ok_or_else(|| PlaidError::Shape("no link_token".into()))
    }

    /// Returns (access_token, item_id).
    pub async fn exchange(&self, public_token: &str) -> Result<(String, String), PlaidError> {
        let v = self.post("/item/public_token/exchange", json!({ "public_token": public_token })).await?;
        match (v["access_token"].as_str(), v["item_id"].as_str()) {
            (Some(a), Some(i)) => Ok((a.to_string(), i.to_string())),
            _ => Err(PlaidError::Shape("no access_token".into())),
        }
    }

    pub async fn accounts(&self, access_token: &str) -> Result<Vec<Account>, PlaidError> {
        let v = self.post("/accounts/get", json!({ "access_token": access_token })).await?;
        serde_json::from_value(v["accounts"].clone()).map_err(|e| PlaidError::Shape(e.to_string()))
    }

    pub async fn transactions_sync(&self, access_token: &str, cursor: Option<&str>) -> Result<SyncPage, PlaidError> {
        let mut body = json!({
            "access_token": access_token,
            "count": 500,
            "options": { "include_original_description": true },
        });
        if let Some(c) = cursor {
            body["cursor"] = json!(c);
        }
        let v = self.post("/transactions/sync", body).await?;
        serde_json::from_value(v).map_err(|e| PlaidError::Shape(e.to_string()))
    }

    pub async fn item_remove(&self, access_token: &str) -> Result<(), PlaidError> {
        self.post("/item/remove", json!({ "access_token": access_token })).await.map(|_| ())
    }
}
