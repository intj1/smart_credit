pub mod alerts;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod crypto;
pub mod csv_import;
pub mod engine;
pub mod error;
pub mod periods;
pub mod plaid;
pub mod routes;
pub mod scheduler;
pub mod service;
pub mod sync;
pub mod views;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use sqlx::PgPool;

pub struct Inner {
    pub pool: PgPool,
    pub config: config::Config,
    pub plaid: Option<plaid::Plaid>,
    /// Failed-login counter: (failures, window start). One user, so one counter.
    pub login_failures: Mutex<(u32, Instant)>,
    /// Serialises sync runs (nightly job vs. the Refresh button).
    pub sync_lock: tokio::sync::Mutex<()>,
}

pub type AppState = Arc<Inner>;

pub fn new_state(pool: PgPool, config: config::Config) -> AppState {
    let plaid = config.plaid.clone().map(plaid::Plaid::new);
    Arc::new(Inner { pool, config, plaid, login_failures: Mutex::new((0, Instant::now())), sync_lock: tokio::sync::Mutex::new(()) })
}

/// Runs migrations, loads the catalog and seeds the single user if needed.
pub async fn bootstrap(state: &AppState) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(&state.pool).await?;
    let catalog = catalog::load_dir(std::path::Path::new(&state.config.catalog_dir))?;
    catalog::sync_to_db(&state.pool, &catalog).await?;
    tracing::info!(products = catalog.products.len(), benefits = catalog.benefits.len(), "catalog loaded");
    auth::seed_user(state).await?;
    service::recompute_all(&state.pool).await?;
    Ok(())
}
