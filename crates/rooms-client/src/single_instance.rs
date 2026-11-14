use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, OnceLock};

use eframe::egui;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::runtime::Runtime;

const MAX_MESSAGE: u64 = 4096;
const OPEN_RETRIES: u32 = 40;
const OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

pub struct Listener {
    pub links: Receiver<String>,
    pub ctx: Arc<OnceLock<egui::Context>>,
}

pub enum Startup {
    Run(Option<Listener>),
    Exit,
}

pub fn start(link: Option<&str>) -> Startup {
    start_with(&pipe_name(), link)
}

fn pipe_name() -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    format!(r"\\.\pipe\zenha-{user}")
}

fn start_with(name: &str, link: Option<&str>) -> Startup {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(%err, "single instance disabled: no runtime");
            return Startup::Run(None);
        }
    };
    let first = {
        let _guard = runtime.enter();
        ServerOptions::new().first_pipe_instance(true).create(name)
    };
    match first {
        Ok(server) => Startup::Run(listen(runtime, server, name.to_owned())),
        Err(_) => {
            let message = link.unwrap_or_default().as_bytes().to_vec();
            let sent = runtime.block_on(async {
                let mut client = open_client(name)?;
                client.write_all(&message).await?;
                client.shutdown().await
            });
            match sent {
                Ok(()) => Startup::Exit,
                Err(err) => {
                    tracing::warn!(%err, "could not reach the running instance");
                    Startup::Run(None)
                }
            }
        }
    }
}

fn open_client(name: &str) -> std::io::Result<NamedPipeClient> {
    let mut attempts = 0;
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            // The running instance reopens its pipe after each connection; retry briefly.
            Err(err) if attempts < OPEN_RETRIES => {
                tracing::debug!(%err, "instance pipe busy, retrying");
                attempts += 1;
                std::thread::sleep(OPEN_RETRY_DELAY);
            }
            Err(err) => return Err(err),
        }
    }
}

fn listen(runtime: Runtime, first: NamedPipeServer, name: String) -> Option<Listener> {
    let (tx, links) = channel();
    let ctx = Arc::new(OnceLock::<egui::Context>::new());
    let wake = Arc::clone(&ctx);
    let spawned = std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            runtime.block_on(async move {
                let mut server = first;
                loop {
                    if let Err(err) = server.connect().await {
                        tracing::debug!(%err, "pipe connect failed");
                    }
                    let next = match ServerOptions::new().create(&name) {
                        Ok(next) => next,
                        Err(err) => {
                            tracing::warn!(%err, "could not reopen the instance pipe");
                            return;
                        }
                    };
                    let mut conn = std::mem::replace(&mut server, next);
                    let mut buf = Vec::new();
                    if (&mut conn)
                        .take(MAX_MESSAGE)
                        .read_to_end(&mut buf)
                        .await
                        .is_err()
                    {
                        continue;
                    }
                    if tx
                        .send(String::from_utf8_lossy(&buf).trim().to_owned())
                        .is_err()
                    {
                        return;
                    }
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            });
        });
    match spawned {
        Ok(_) => Some(Listener { links, ctx }),
        Err(err) => {
            tracing::warn!(%err, "single instance disabled: no listener thread");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn second_launch_forwards_its_link_and_exits() {
        let name = format!(r"\\.\pipe\zenha-test-{}", std::process::id());
        let Startup::Run(Some(listener)) = start_with(&name, None) else {
            panic!("first launch must own the pipe");
        };
        assert!(matches!(
            start_with(&name, Some("zenha://entrar?servidor=x")),
            Startup::Exit
        ));
        assert!(matches!(start_with(&name, None), Startup::Exit));
        let timeout = Duration::from_secs(5);
        assert_eq!(
            listener.links.recv_timeout(timeout).ok().as_deref(),
            Some("zenha://entrar?servidor=x")
        );
        assert_eq!(
            listener.links.recv_timeout(timeout).ok().as_deref(),
            Some("")
        );
    }
}
