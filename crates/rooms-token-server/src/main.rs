mod chat_store;
mod config;
mod directory;
mod routes;
mod token;

use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::chat_store::ChatStore;
use crate::config::Config;
use crate::directory::LiveKitDirectory;
use crate::routes::{AppState, ChatRateLimiter, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("hash-password") {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("reading password from stdin")?;
        let hash = config::hash_password(&line)?;
        println!("{hash}");
        return Ok(());
    }

    // A missing .env is fine: production reads real environment variables.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env().context("loading configuration")?;
    tracing::info!(?config, "starting");

    let db_path = config.chat_db_path.clone();
    let chat = match tokio::task::spawn_blocking(move || ChatStore::open(&db_path)).await {
        Ok(Ok(store)) => Some(Arc::new(store)),
        Ok(Err(err)) => {
            tracing::error!(
                path = %config.chat_db_path.display(),
                %err,
                "failed to open chat database; chat routes will return 503"
            );
            None
        }
        Err(err) => {
            tracing::error!(
                path = %config.chat_db_path.display(),
                %err,
                "chat database init task failed; chat routes will return 503"
            );
            None
        }
    };

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    let state = AppState {
        directory: Arc::new(LiveKitDirectory::new(&config)),
        chat,
        chat_limiter: Arc::new(ChatRateLimiter::default()),
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
