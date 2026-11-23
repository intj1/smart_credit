//! End-to-end API tests against a real Postgres and a mock Plaid server.
//!
//!     TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo test
//!
//! Each test creates (and drops) its own database. Without TEST_DATABASE_URL
//! the tests are skipped.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use card_benefits::config::{Config, PlaidConfig};
use card_benefits::{bootstrap, crypto, new_state, routes, service, sync, AppState};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::Connection;
use tower::ServiceExt;

struct TestApp {
    state: AppState,
    router: Router,
    cookie: Mutex<Option<String>>,
    admin_url: String,
    db_name: String,
}

impl TestApp {
    async fn new(require_mfa: bool, plaid_url: Option<String>) -> Option<TestApp> {
        let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL not set; skipping");
            return None;
        };
        let db_name = format!("cbt_test_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(&admin_url).await.unwrap();
        sqlx::query(&format!("create database {db_name}")).execute(&mut admin).await.unwrap();
        let base = admin_url.rsplit_once('/').unwrap().0;
        let config = Config {
            database_url: format!("{base}/{db_name}"),
            bind_addr: "127.0.0.1:0".into(),
            static_dir: "./static".into(),
            catalog_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/../catalog").into(),
            encryption_key: [9u8; 32],
            admin_email: Some("me@example.com".into()),
            admin_password: Some("correct horse battery".into()),
            require_mfa,
            cookie_secure: false,
            plaid: plaid_url.map(|base_url| PlaidConfig { client_id: "cid".into(), secret: "sec".into(), base_url, redirect_uri: None }),
            mail: None,
            sync_hour: 3,
            alert_hour: 8,
            run_scheduler: false,
        };
        let pool = PgPoolOptions::new().max_connections(3).connect(&config.database_url).await.unwrap();
        let state = new_state(pool, config);
        bootstrap(&state).await.unwrap();
        sqlx::query("update users set timezone = 'UTC'").execute(&state.pool).await.unwrap();
        let router = routes::router(state.clone());
        Some(TestApp { state, router, cookie: Mutex::new(None), admin_url, db_name })
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(format!("/api{path}"));
        if let Some(c) = self.cookie.lock().unwrap().clone() {
            req = req.header(header::COOKIE, c);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())).unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        if let Some(set) = resp.headers().get(header::SET_COOKIE) {
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_string();
            *self.cookie.lock().unwrap() = Some(pair);
        }
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.call(Method::GET, path, None).await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {v}");
        v
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.call(Method::POST, path, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "POST {path}: {v}");
        v
    }

    async fn login(&self) -> Value {
        self.post("/auth/login", json!({ "email": "me@example.com", "password": "correct horse battery" })).await
    }

    async fn cleanup(self) {
        self.state.pool.close().await;
        let mut admin = sqlx::PgConnection::connect(&self.admin_url).await.unwrap();
        sqlx::query(&format!("drop database if exists {} with (force)", self.db_name)).execute(&mut admin).await.unwrap();
    }
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn period<'a>(rows: &'a Value, benefit_id: &str) -> &'a Value {
    rows.as_array().unwrap().iter().find(|r| r["benefit_id"] == benefit_id).unwrap_or_else(|| panic!("no row for {benefit_id}"))
}

#[tokio::test]
async fn manual_card_csv_matching_overrides_and_data_rights() {
    let Some(app) = TestApp::new(false, None).await else { return };

    // Nothing is readable without a session.
    let (status, _) = app.call(Method::GET, "/home", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app.call(Method::POST, "/auth/login", Some(json!({ "email": "me@example.com", "password": "nope" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(app.login().await["next"], "ok");

    // Catalog has the four cards from the spec.
    let catalog = app.get("/catalog").await;
    let ids: Vec<&str> = catalog["products"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    for id in ["amex-gold", "amex-platinum", "capital-one-venture-x", "bilt-palladium"] {
        assert!(ids.contains(&id), "catalog is missing {id}");
    }

    // Add a card by hand (the fallback path), already enrolled for Resy only.
    let (status, _) = app
        .call(Method::POST, "/cards", Some(json!({ "product_id": "amex-platinum", "anniversary_month": 3, "last4": "4111111111111111" })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "full card numbers must be rejected");
    let card = app
        .post("/cards", json!({ "product_id": "amex-platinum", "anniversary_month": 3, "last4": "1008", "enrolled_benefit_ids": ["amex-platinum-resy"] }))
        .await;
    let card_id = card["id"].as_str().unwrap().to_string();

    let t = today();
    let unclaimed = app.get("/unclaimed").await;
    let rows = &unclaimed["rows"];
    assert_eq!(period(rows, "amex-platinum-resy")["status"], "not_started");
    assert_eq!(period(rows, "amex-platinum-lululemon")["status"], "needs_enrollment");
    let december = t.month() == 12;
    assert_eq!(period(rows, "amex-platinum-uber-cash")["value"], if december { 3500 } else { 1500 });

    // Acceptance: list totals equal the sum of remaining over open periods.
    let sum: i64 = rows.as_array().unwrap().iter().filter(|r| r["unit"] == "usd").map(|r| r["remaining"].as_i64().unwrap()).sum();
    assert_eq!(unclaimed["totals"]["total_remaining_cents"].as_i64().unwrap(), sum);
    assert!(sum > 0);

    // Import a statement: a Resy dinner with a partial credit, a streaming
    // charge still waiting for its credit.
    let d = |date: NaiveDate| date.format("%m/%d/%Y").to_string();
    let csv = format!(
        "Date,Description,Amount\n{},RESY* LILIA BROOKLYN,60.00\n{},Platinum Resy Credit,-60.00\n{},HULU 877-8244858,18.99\n{},WHOLE FOODS,54.20\n",
        d(t),
        d(t),
        d(t),
        d(t)
    );
    let imported = app.post(&format!("/cards/{card_id}/import"), json!({ "csv": csv })).await;
    assert_eq!((imported["imported"].as_i64(), imported["duplicates"].as_i64()), (Some(4), Some(0)));
    let again = app.post(&format!("/cards/{card_id}/import"), json!({ "csv": csv })).await;
    assert_eq!((again["imported"].as_i64(), again["duplicates"].as_i64()), (Some(0), Some(4)));

    let unclaimed = app.get("/unclaimed").await;
    let resy = period(&unclaimed["rows"], "amex-platinum-resy").clone();
    assert_eq!((resy["used"].as_i64(), resy["remaining"].as_i64(), resy["status"].as_str()), (Some(6000), Some(4000), Some("partly_used")));
    // Not enrolled for digital entertainment, so the purchase is only a prompt.
    let ent = period(&unclaimed["rows"], "amex-platinum-digital-entertainment").clone();
    assert_eq!((ent["pending"].as_i64(), ent["status"].as_str()), (Some(1899), Some("needs_enrollment")));
    app.call(Method::PUT, &format!("/cards/{card_id}/benefits/amex-platinum-digital-entertainment"), Some(json!({ "enrolled": true }))).await;
    let unclaimed = app.get("/unclaimed").await;
    assert_eq!(period(&unclaimed["rows"], "amex-platinum-digital-entertainment")["status"], "pending_credit");

    // The detail view shows the transactions behind the state.
    let resy_id = resy["id"].as_str().unwrap();
    let detail = app.get(&format!("/periods/{resy_id}")).await;
    let matches = detail["matches"].as_array().unwrap();
    let credit = matches.iter().find(|m| m["source"] == "credit").expect("credit match");
    assert_eq!(credit["amount"], 6000);
    assert!(matches.iter().any(|m| m["source"] == "purchase"));

    // "Not this one" removes the credit; undo restores it.
    let txn_id = credit["transaction"]["id"].as_str().unwrap();
    app.post(&format!("/periods/{resy_id}/exclusions"), json!({ "transaction_id": txn_id })).await;
    assert_eq!(app.get(&format!("/periods/{resy_id}")).await["period"]["used"], 0);
    let (status, _) = app.call(Method::DELETE, &format!("/periods/{resy_id}/exclusions/{txn_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.get(&format!("/periods/{resy_id}")).await["period"]["used"], 6000);

    // Manual input wins and survives a later sync.
    let out = app.post(&format!("/periods/{resy_id}/override"), json!({ "mode": "amount", "amount": 2500 })).await;
    assert_eq!((out["period"]["used"].as_i64(), out["period"]["manual"].as_bool()), (Some(2500), Some(true)));
    app.post("/sync", json!({})).await;
    assert_eq!(app.get(&format!("/periods/{resy_id}")).await["period"]["used"], 2500);
    let out = app.post(&format!("/periods/{resy_id}/override"), json!({ "mode": "used" })).await;
    assert_eq!(out["period"]["status"], "used");
    assert!(app.get("/unclaimed").await["rows"].as_array().unwrap().iter().all(|r| r["benefit_id"] != "amex-platinum-resy"));
    let out = app.post(&format!("/periods/{resy_id}/override"), json!({ "mode": "clear" })).await;
    assert_eq!(out["period"]["used"], 6000);

    // Check-off benefit and "Not for me".
    let uber = period(&app.get("/unclaimed").await["rows"], "amex-platinum-uber-cash")["id"].as_str().unwrap().to_string();
    assert_eq!(app.post(&format!("/periods/{uber}/override"), json!({ "mode": "used" })).await["period"]["status"], "used");
    app.call(Method::PUT, &format!("/cards/{card_id}/benefits/amex-platinum-equinox"), Some(json!({ "not_for_me": true }))).await;
    let rows = app.get("/unclaimed").await["rows"].clone();
    assert!(rows.as_array().unwrap().iter().all(|r| r["benefit_id"] != "amex-platinum-equinox"));
    let hidden = app.get("/unclaimed?include_hidden=true").await["rows"].clone();
    assert_eq!(period(&hidden, "amex-platinum-equinox")["status"], "not_for_me");

    // Filters.
    let dining = app.get("/unclaimed?category=dining&provider=amex").await["rows"].clone();
    assert!(dining.as_array().unwrap().iter().all(|r| r["category"] == "dining"));
    assert!(app.get("/unclaimed?provider=bilt").await["rows"].as_array().unwrap().is_empty());

    // Value captured against the fee.
    let cards = app.get("/cards").await;
    let c = &cards["cards"][0];
    assert_eq!((c["annual_fee_cents"].as_i64(), c["captured_ytd_cents"].as_i64()), (Some(89500), Some(6000 + if december { 3500 } else { 1500 })));

    // Offers: redeemed automatically by a matching purchase.
    let expires = t + Duration::days(20);
    app.post("/offers", json!({ "card_id": card_id, "merchant": "Whole Foods", "reward": "$10 back on $50", "reward_cents": 1000, "min_spend_cents": 5000, "expires": expires })).await;
    app.post("/offers", json!({ "card_id": card_id, "merchant": "Blue Bottle", "reward": "10% back", "expires": expires })).await;
    let offers = app.get("/offers").await["offers"].clone();
    let status_of = |m: &str| offers.as_array().unwrap().iter().find(|o| o["merchant"] == m).unwrap()["status"].clone();
    assert_eq!((status_of("Whole Foods"), status_of("Blue Bottle")), (json!("redeemed"), json!("active")));

    // Home and alerts.
    let user = service::user(&app.state.pool).await.unwrap();
    let created = card_benefits::alerts::generate(&app.state.pool, &user, t).await.unwrap();
    assert!(created > 0);
    assert_eq!(card_benefits::alerts::generate(&app.state.pool, &user, t).await.unwrap(), 0, "alerts must not repeat");
    let home = app.get("/home").await;
    assert!(home["alerts"].as_array().unwrap().iter().any(|a| a["kind"] == "enrollment"));
    assert!(home["next_deadlines"].as_array().unwrap().len() <= 5);

    // Export has the data but no secrets; every override was audited.
    let (status, export) = app.call(Method::GET, "/export", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["transactions"].as_array().unwrap().len(), 4);
    assert!(!export.to_string().contains("password_hash"));
    assert!(export["audit_log"].as_array().unwrap().iter().any(|a| a["action"] == "override"));

    // Delete-my-data needs the confirmation word, then wipes card data.
    let (status, _) = app.call(Method::POST, "/delete-my-data", Some(json!({ "confirm": "yes" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    app.post("/delete-my-data", json!({ "confirm": "DELETE" })).await;
    assert!(app.get("/cards").await["cards"].as_array().unwrap().is_empty());
    let left: i64 = sqlx::query_scalar("select (select count(*) from transactions) + (select count(*) from benefit_periods)").fetch_one(&app.state.pool).await.unwrap();
    assert_eq!(left, 0);

    app.cleanup().await;
}

#[tokio::test]
async fn mfa_is_enforced() {
    let Some(app) = TestApp::new(true, None).await else { return };
    assert_eq!(app.login().await["next"], "totp_setup");

    // Password alone opens nothing.
    let (status, body) = app.call(Method::GET, "/home", None).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("mfa_required")));

    let setup = app.post("/auth/totp/setup", json!({})).await;
    let secret = setup["secret"].as_str().unwrap();
    assert!(setup["qr_svg"].as_str().unwrap().starts_with("<?xml") || setup["qr_svg"].as_str().unwrap().contains("<svg"));
    let (status, _) = app.call(Method::POST, "/auth/totp/verify", Some(json!({ "code": "000000" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let step = Utc::now().timestamp() as u64 / 30;
    let code = crypto::totp_at(secret, step).unwrap();
    assert_eq!(app.post("/auth/totp/verify", json!({ "code": code })).await["next"], "ok");
    app.get("/home").await;

    // A second login must present a fresh code; replaying the old one fails.
    assert_eq!(app.login().await["next"], "totp");
    let (status, _) = app.call(Method::POST, "/auth/totp/verify", Some(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.call(Method::GET, "/cards", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let next = crypto::totp_at(secret, step + 1).unwrap();
    assert_eq!(app.post("/auth/totp/verify", json!({ "code": next })).await["next"], "ok");

    // The secret is not stored in the clear.
    let stored: String = sqlx::query_scalar("select totp_secret_enc from users").fetch_one(&app.state.pool).await.unwrap();
    assert!(!stored.contains(secret));

    app.post("/auth/logout", json!({})).await;
    let (status, _) = app.call(Method::GET, "/home", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

/// A stand-in for Plaid's REST API. `calls` records the paths hit; the second
/// /transactions/sync call returns a modification and a removal.
async fn mock_plaid(calls: Arc<Mutex<Vec<String>>>) -> String {
    let t = today();
    let handler = move |path: &'static str| {
        let calls = calls.clone();
        move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                assert_eq!(body["client_id"], "cid");
                calls.lock().unwrap().push(path.to_string());
                let n = calls.lock().unwrap().iter().filter(|p| *p == path).count();
                let txn = |id: &str, account: &str, amount: f64, name: &str, pending: bool| {
                    json!({ "transaction_id": id, "account_id": account, "amount": amount, "date": t, "name": name,
                            "merchant_name": null, "pending": pending,
                            "personal_finance_category": { "primary": "FOOD_AND_DRINK", "detailed": "FOOD_AND_DRINK_RESTAURANT" } })
                };
                Json(match path {
                    "/link/token/create" => json!({ "link_token": "link-sandbox-1" }),
                    "/item/public_token/exchange" => {
                        assert_eq!(body["public_token"], "public-sandbox-1");
                        json!({ "access_token": "access-sandbox-secret", "item_id": "item-1" })
                    }
                    "/accounts/get" => json!({ "accounts": [
                        { "account_id": "acc-gold", "name": "Gold Card", "official_name": "American Express Gold Card", "mask": "2001", "type": "credit", "subtype": "credit card" },
                        { "account_id": "acc-au", "name": "Gold Card", "official_name": "American Express Gold Card (Additional)", "mask": "2019", "type": "credit", "subtype": "credit card" },
                        { "account_id": "acc-plat", "name": "Platinum Card", "official_name": null, "mask": "1008", "type": "credit", "subtype": "credit card" },
                        { "account_id": "acc-sav", "name": "Savings", "official_name": null, "mask": "9", "type": "depository", "subtype": "savings" },
                    ] }),
                    "/transactions/sync" if n == 1 => {
                        assert!(body.get("cursor").is_none());
                        json!({ "added": [
                            txn("t1", "acc-gold", 31.40, "GRUBHUB*THAI VILLA", false),
                            txn("t2", "acc-gold", -10.0, "AMEX Dining Credit", false),
                            txn("t3", "acc-au", 6.25, "DUNKIN #345", true),
                            txn("t4", "acc-plat", 99.0, "NOT TRACKED", false),
                        ], "modified": [], "removed": [], "next_cursor": "c1", "has_more": false })
                    }
                    "/transactions/sync" => {
                        assert_eq!(body["cursor"], "c1");
                        json!({ "added": [txn("t5", "acc-au", 6.25, "DUNKIN #345", false), txn("t6", "acc-gold", -6.25, "Dunkin Credit", false)],
                                "modified": [], "removed": [{ "transaction_id": "t3" }], "next_cursor": "c2", "has_more": false })
                    }
                    "/item/remove" => {
                        assert_eq!(body["access_token"], "access-sandbox-secret");
                        json!({ "request_id": "r" })
                    }
                    other => panic!("unexpected Plaid call {other}"),
                })
            }
        }
    };
    let app = Router::new()
        .route("/link/token/create", post(handler("/link/token/create")))
        .route("/item/public_token/exchange", post(handler("/item/public_token/exchange")))
        .route("/accounts/get", post(handler("/accounts/get")))
        .route("/transactions/sync", post(handler("/transactions/sync")))
        .route("/item/remove", post(handler("/item/remove")));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn plaid_link_sync_and_disconnect() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let Some(app) = TestApp::new(false, Some(mock_plaid(calls.clone()).await)).await else { return };
    app.login().await;

    assert_eq!(app.post("/plaid/link-token", json!({})).await["link_token"], "link-sandbox-1");
    let linked = app.post("/plaid/exchange", json!({ "public_token": "public-sandbox-1", "provider": "amex", "institution_name": "American Express" })).await;
    let conn = linked["connection_id"].as_str().unwrap().to_string();
    let accounts = linked["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 3, "only credit cards are offered");
    assert_eq!(accounts[0]["suggested_product_id"], "amex-gold");
    assert_eq!(accounts[2]["suggested_product_id"], "amex-platinum");

    // The access token is encrypted at rest.
    let stored: String = sqlx::query_scalar("select access_token_enc from connections").fetch_one(&app.state.pool).await.unwrap();
    assert!(!stored.contains("access-sandbox"));

    // Track the Gold card with its authorized-user account rolled up; skip Platinum.
    let anniversary = today().month();
    let (status, body) = app
        .call(Method::POST, &format!("/connections/{conn}/cards"), Some(json!({ "cards": [{
            "product_id": "amex-gold", "account_ids": ["acc-gold", "acc-au"], "last4": "2001", "anniversary_month": anniversary,
            "enrolled_benefit_ids": ["amex-gold-dining", "amex-gold-dunkin"] }] })))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let card_id = body["card_ids"][0].as_str().unwrap().to_string();

    // The link spawns a background sync; it writes its audit entry once the
    // transactions are stored and the engine has run.
    for _ in 0..200 {
        let done: bool = sqlx::query_scalar("select exists (select 1 from audit_log where action = 'sync' and detail->>'trigger' = 'link')")
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
        if done {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let detail = app.get(&format!("/cards/{card_id}")).await;
    assert_eq!(detail["card"]["transaction_count"], 3, "untracked account is skipped");
    let open: Vec<&Value> = detail["periods"].as_array().unwrap().iter().filter(|p| p["days_left"].as_i64().is_some_and(|d| d >= 0)).collect();
    let state_of = |id: &str| {
        let p = open.iter().find(|p| p["benefit_id"] == id).unwrap();
        (p["used"].as_i64().unwrap(), p["pending"].as_i64().unwrap(), p["status"].as_str().unwrap().to_string())
    };
    assert_eq!(state_of("amex-gold-dining"), (1000, 0, "used".into()));
    assert_eq!(state_of("amex-gold-dunkin"), (0, 625, "pending_credit".into()));

    // Next sync: the pending charge posts under a new id and its credit arrives.
    let summary = sync::sync_connection(&app.state, conn.parse().unwrap()).await.unwrap();
    assert_eq!((summary.added, summary.removed), (2, 1));
    let detail = app.get(&format!("/cards/{card_id}")).await;
    let dunkin = detail["periods"].as_array().unwrap().iter().find(|p| p["benefit_id"] == "amex-gold-dunkin" && p["days_left"].as_i64().is_some_and(|d| d >= 0)).unwrap();
    assert_eq!((dunkin["used"].as_i64(), dunkin["pending"].as_i64(), dunkin["status"].as_str()), (Some(625), Some(0), Some("partly_used")));

    let conns = app.get("/connections").await;
    assert_eq!(conns["connections"][0]["status"], "ok");

    // Disconnect revokes at Plaid and deletes the transactions.
    let (status, body) = app.call(Method::DELETE, &format!("/connections/{conn}"), None).await;
    assert_eq!((status, body["warning"].clone()), (StatusCode::OK, Value::Null));
    assert!(calls.lock().unwrap().iter().any(|p| p == "/item/remove"));
    let left: i64 = sqlx::query_scalar("select (select count(*) from transactions) + (select count(*) from cards)").fetch_one(&app.state.pool).await.unwrap();
    assert_eq!(left, 0);
    app.cleanup().await;
}
