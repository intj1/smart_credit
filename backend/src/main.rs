use anyhow::Context;
use card_benefits::{bootstrap, config::Config, new_state, routes, scheduler};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `card-benefits healthcheck`: used by the container health check, since
    // the runtime image has no shell or curl.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;

    // The database container may still be starting when we do.
    let mut attempt = 0;
    let pool = loop {
        match PgPoolOptions::new().max_connections(5).connect(&config.database_url).await {
            Ok(pool) => break pool,
            Err(e) if attempt < 30 => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "database not ready, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e).context("connecting to the database"),
        }
    };

    let state = new_state(pool, config);
    bootstrap(&state).await?;

    if state.config.run_scheduler {
        tokio::spawn(scheduler::run(state.clone()));
    }

    let listener = tokio::net::TcpListener::bind(&state.config.bind_addr)
        .await
        .with_context(|| format!("binding {}", state.config.bind_addr))?;
    tracing::info!(addr = %state.config.bind_addr, plaid = state.plaid.is_some(), mail = state.config.mail.is_some(), "listening");
    axum::serve(listener, routes::router(state)).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}

fn healthcheck() -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let port = bind.rsplit(':').next().unwrap_or("8080");
    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    stream.write_all(b"GET /api/healthz HTTP/1.0\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    anyhow::ensure!(response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"), "unhealthy");
    Ok(())
}
