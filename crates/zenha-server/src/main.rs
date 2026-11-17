mod avatar;
mod config;
mod directory;
mod room_access;
mod routes;
mod store;
mod token;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::store::ChatStore;
use crate::config::Config;
use crate::directory::LiveKitDirectory;
use crate::routes::{
    AdminLoginLimiter, AppState, AvatarRateLimiter, ChatRateLimiter, DEFAULT_ADMIN_FAILURE_DELAY,
    RoomUnlockLimiter, router,
};

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
    let seed_rooms = config.rooms.clone();
    let chat = match tokio::task::spawn_blocking(move || -> anyhow::Result<ChatStore> {
        let store = ChatStore::open(&db_path)?;
        store.seed_rooms_if_empty_sync(&seed_rooms)?;
        Ok(store)
    })
    .await
    {
        Ok(Ok(store)) => Some(Arc::new(store)),
        Ok(Err(err)) => {
            tracing::error!(
                path = %config.chat_db_path.display(),
                %err,
                "failed to open database; room admin, chat, and invite routes will return 503"
            );
            None
        }
        Err(err) => {
            tracing::error!(
                path = %config.chat_db_path.display(),
                %err,
                "database init task failed; room admin, chat, and invite routes will return 503"
            );
            None
        }
    };

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    let directory = Arc::new(LiveKitDirectory::new(&config));
    let state = AppState {
        directory: directory.clone(),
        moderator: directory,
        chat,
        chat_limiter: Arc::new(ChatRateLimiter::default()),
        avatar_limiter: Arc::new(AvatarRateLimiter::default()),
        admin_login_limiter: Arc::new(AdminLoginLimiter::default()),
        unlock_limiter: Arc::new(RoomUnlockLimiter::default()),
        admin_failure_delay: DEFAULT_ADMIN_FAILURE_DELAY,
        config: Arc::new(config),
    };

    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("serving")
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!(%err, "could not listen for ctrl-c");
    }
}
