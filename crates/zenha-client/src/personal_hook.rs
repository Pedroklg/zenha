//! Background fetch of the user's personal Gamers Club webhook URL (`POST /personal-hook`).

use std::sync::mpsc::Sender;
use std::time::Duration;

use eframe::egui;
use zenha_protocol::{PersonalHookRequest, PersonalHookResponse};

use crate::demo::{DEMO_SERVER_URL, SELF_IDENTITY};
use crate::state::PersonalHookFetch;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEMO_SIG: &str = "0123456789abcdef0123456789abcdef";

pub fn demo_url() -> String {
    format!("{DEMO_SERVER_URL}/hooks/u/{SELF_IDENTITY}/{DEMO_SIG}")
}

pub fn build_url(server_url: &str, path: &str) -> Result<String, String> {
    let base = server_url.trim().trim_end_matches('/');
    let valid_base = base.starts_with("http://") || base.starts_with("https://");
    let valid_path =
        path.starts_with("/hooks/u/") && !path.chars().any(|c| c.is_whitespace() || c.is_control());
    if !valid_base || !valid_path {
        return Err("Resposta inválida do servidor.".to_owned());
    }
    Ok(format!("{base}{path}"))
}

pub fn spawn_fetch(
    ctx: egui::Context,
    fetch: PersonalHookFetch,
    tx: Sender<(u64, Result<String, String>)>,
) {
    let id = fetch.id;
    let tx_thread = tx.clone();
    let ctx_thread = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("personal-hook-fetch".into())
        .spawn(move || {
            let result = fetch_blocking(&fetch);
            if tx_thread.send((id, result)).is_ok() {
                ctx_thread.request_repaint();
            }
        });
    if let Err(err) = spawned {
        tracing::debug!(%err, "could not spawn personal hook fetch thread");
        let _ = tx.send((id, Err("Não foi possível carregar a URL.".to_owned())));
        ctx.request_repaint();
    }
}

fn fetch_blocking(fetch: &PersonalHookFetch) -> Result<String, String> {
    let base = fetch.server_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("Servidor não configurado.".to_owned());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            tracing::debug!(%err, "could not initialize runtime for personal hook fetch");
            "Não foi possível carregar a URL.".to_owned()
        })?;

    zenha_core::install_crypto_provider();
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                tracing::debug!(%err, "could not build http client for personal hook fetch");
                "Não foi possível carregar a URL.".to_owned()
            })?;
        let response = client
            .post(format!("{base}/personal-hook"))
            .json(&PersonalHookRequest {
                invite_code: fetch.invite_code.clone(),
                client_id: fetch.client_id.clone(),
            })
            .send()
            .await
            .map_err(|err| {
                tracing::debug!(%err, "personal hook request failed");
                "Não foi possível falar com o servidor.".to_owned()
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err("Senha do servidor recusada.".to_owned());
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err("Este servidor ainda não suporta URL pessoal.".to_owned());
        }
        if !status.is_success() {
            tracing::debug!(status = %status, "personal hook request returned error status");
            return Err("Não foi possível carregar a URL.".to_owned());
        }

        let payload: PersonalHookResponse = response.json().await.map_err(|err| {
            tracing::debug!(%err, "could not decode personal hook response");
            "Resposta inválida do servidor.".to_owned()
        })?;
        build_url(base, &payload.path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_joins_base_and_validates_personal_hook_path() {
        assert_eq!(
            build_url("https://voz.exemplo/", "/hooks/u/abc/0123456789abcdef").unwrap(),
            "https://voz.exemplo/hooks/u/abc/0123456789abcdef"
        );
        assert!(build_url("https://voz.exemplo", "/hooks/other/123").is_err());
        assert!(build_url("https://voz.exemplo", "/hooks/u/abc/bad space").is_err());
        assert!(build_url("ftp://voz.exemplo", "/hooks/u/abc/123").is_err());
    }

    #[test]
    fn demo_url_uses_demo_server_and_personal_hook_prefix() {
        let url = demo_url();
        assert!(url.starts_with(DEMO_SERVER_URL));
        assert!(url.contains("/hooks/u/demo-self/"));
    }
}
