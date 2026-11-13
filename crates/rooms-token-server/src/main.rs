mod config;
mod directory;
mod routes;
mod token;

use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::directory::LiveKitDirectory;
use crate::routes::{AppState, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A missing .env is fine: production reads real environment variables.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env().context("loading configuration")?;
    tracing::info!(?config, "starting");

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    let state = AppState {
        directory: Arc::new(LiveKitDirectory::new(&config)),
        config: Arc::new(config),
    };

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!(%err, "could not listen for ctrl-c");
    }
}
