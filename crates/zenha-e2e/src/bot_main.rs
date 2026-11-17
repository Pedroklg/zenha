//! Entry point of the `bot` subcommand: builds the backend and hands it to the bot loop.

use std::io;

use anyhow::Context;
use tracing_subscriber::EnvFilter;
#[cfg(windows)]
use zenha_core::LiveKitBackend;
#[cfg(not(windows))]
use zenha_core::NoMediaBackend;
use zenha_core::{LiveKitBackendOptions, MicrophoneSource, SessionConfig, SessionHandle};

use crate::bot::{BotInfo, run_bot};
use crate::cli::BotArgs;

pub fn options_for(args: &BotArgs) -> LiveKitBackendOptions {
    LiveKitBackendOptions {
        microphone: match args.tone_hz {
            Some(hz) => MicrophoneSource::Tone {
                hz,
                amplitude: args.amplitude,
            },
            None => MicrophoneSource::Device,
        },
        test_pattern_target: args.test_pattern,
    }
}

fn describe(options: &LiveKitBackendOptions) -> String {
    match options.microphone {
        MicrophoneSource::Device => "device".to_owned(),
        MicrophoneSource::Tone { hz, amplitude } => format!("tone {hz} Hz at {amplitude}"),
    }
}

pub fn client_id_for_name(name: &str) -> String {
    let mut id: String = name
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_lowercase() || lower.is_ascii_digit() || lower == '-' {
                lower
            } else {
                '-'
            }
        })
        .take(zenha_protocol::MAX_CLIENT_ID_LEN)
        .collect();
    if id.is_empty() {
        id.push_str("bot");
    }
    id
}

pub fn bot_main(args: &BotArgs) -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(io::stderr)
        .try_init();

    let options = options_for(args);
    options.validate().context("invalid bot options")?;
    let client_id = client_id_for_name(&args.name);
    let config = SessionConfig {
        token_server_url: args.token_url.clone(),
        client_id: client_id.clone(),
        display_name: args.name.clone(),
        invite_code: args.invite.clone(),
        avatar_version: None,
        avatar_cache_dir: None,
        room_keys: Default::default(),
    };
    #[cfg(windows)]
    let backend = LiveKitBackend::with_options(options);
    #[cfg(not(windows))]
    let backend = NoMediaBackend::new(&client_id);
    let handle =
        SessionHandle::spawn_with_http_tokens(config, backend).context("starting the session")?;
    let info = BotInfo {
        name: args.name.clone(),
        microphone: describe(&options),
        test_pattern: args.test_pattern,
    };
    run_bot(handle, &info, io::BufReader::new(io::stdin()), io::stdout())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(tone: Option<f64>, pattern: bool) -> BotArgs {
        BotArgs {
            name: "t".into(),
            token_url: "http://x".into(),
            invite: "c".into(),
            tone_hz: tone,
            amplitude: 0.03,
            test_pattern: pattern,
            allow_non_windows: false,
        }
    }

    #[test]
    fn options_follow_the_arguments() {
        let device = options_for(&args(None, false));
        assert_eq!(device, LiveKitBackendOptions::default());
        let tone = options_for(&args(Some(1290.0), true));
        assert_eq!(
            tone.microphone,
            MicrophoneSource::Tone {
                hz: 1290.0,
                amplitude: 0.03
            }
        );
        assert!(tone.test_pattern_target);
        assert_eq!(describe(&tone), "tone 1290 Hz at 0.03");
    }

    #[test]
    fn client_id_normalizes_invalid_characters() {
        assert_eq!(client_id_for_name("alice"), "alice");
        assert_eq!(client_id_for_name("Alice_1"), "alice-1");
        assert_eq!(client_id_for_name(""), "bot");
    }
}
