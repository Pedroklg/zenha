use std::fmt;
use std::net::SocketAddr;

use rooms_protocol::validate_room_name;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8787";
const DEFAULT_TOKEN_TTL_SECS: u64 = 3600;
const MAX_TOKEN_TTL_SECS: u64 = 24 * 3600;
const DEFAULT_ROOM_LIST: &str = "geral,jogos,afk";

#[derive(Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub invite_code: String,
    pub livekit_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub token_ttl_secs: u64,
    pub rooms: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {name}: {reason}")]
    Invalid { name: &'static str, reason: String },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |name: &'static str| match lookup(name) {
            Some(value) if !value.trim().is_empty() => Ok(value),
            _ => Err(ConfigError::Missing(name)),
        };

        let bind_addr = lookup("ROOMS_BIND_ADDR")
            .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned())
            .parse()
            .map_err(|err: std::net::AddrParseError| ConfigError::Invalid {
                name: "ROOMS_BIND_ADDR",
                reason: err.to_string(),
            })?;

        Ok(Self {
            bind_addr,
            invite_code: required("ROOMS_INVITE_CODE")?,
            livekit_url: required("LIVEKIT_URL")?,
            livekit_api_key: required("LIVEKIT_API_KEY")?,
            livekit_api_secret: required("LIVEKIT_API_SECRET")?,
            token_ttl_secs: parse_token_ttl(lookup("ROOMS_TOKEN_TTL_SECS").as_deref())?,
            rooms: parse_room_list(lookup("ROOMS_ROOM_LIST").as_deref())?,
        })
    }
}

pub fn parse_token_ttl(value: Option<&str>) -> Result<u64, ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        name: "ROOMS_TOKEN_TTL_SECS",
        reason,
    };
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DEFAULT_TOKEN_TTL_SECS);
    };
    let secs: u64 = value
        .parse()
        .map_err(|err: std::num::ParseIntError| invalid(err.to_string()))?;
    if secs == 0 || secs > MAX_TOKEN_TTL_SECS {
        return Err(invalid(format!(
            "must be between 1 and {MAX_TOKEN_TTL_SECS}"
        )));
    }
    Ok(secs)
}

pub fn parse_room_list(value: Option<&str>) -> Result<Vec<String>, ConfigError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ROOM_LIST);
    let mut rooms: Vec<String> = Vec::new();
    for name in value.split(',').map(str::trim) {
        validate_room_name(name).map_err(|err| ConfigError::Invalid {
            name: "ROOMS_ROOM_LIST",
            reason: format!("room {name:?}: {err}"),
        })?;
        if rooms.iter().any(|room| room == name) {
            return Err(ConfigError::Invalid {
                name: "ROOMS_ROOM_LIST",
                reason: format!("duplicate room {name:?}"),
            });
        }
        rooms.push(name.to_owned());
    }
    Ok(rooms)
}

// Secrets never reach the logs.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("livekit_url", &self.livekit_url)
            .field("token_ttl_secs", &self.token_ttl_secs)
            .field("rooms", &self.rooms)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_list_defaults_when_unset_or_blank() {
        let expected = vec!["geral", "jogos", "afk"];
        assert_eq!(parse_room_list(None).unwrap(), expected);
        assert_eq!(parse_room_list(Some("  ")).unwrap(), expected);
    }

    #[test]
    fn room_list_keeps_order_and_trims() {
        assert_eq!(
            parse_room_list(Some(" b , a,c_1 ")).unwrap(),
            vec!["b", "a", "c_1"]
        );
    }

    #[test]
    fn room_list_rejects_invalid_names() {
        for bad in ["geral,,afk", "Geral", "a b", "a,", &"x".repeat(33)] {
            assert!(
                matches!(
                    parse_room_list(Some(bad)),
                    Err(ConfigError::Invalid {
                        name: "ROOMS_ROOM_LIST",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn room_list_rejects_duplicates() {
        assert!(parse_room_list(Some("a,b,a")).is_err());
    }

    #[test]
    fn ttl_defaults_and_parses() {
        assert_eq!(parse_token_ttl(None).unwrap(), 3600);
        assert_eq!(parse_token_ttl(Some("")).unwrap(), 3600);
        assert_eq!(parse_token_ttl(Some(" 120 ")).unwrap(), 120);
    }

    #[test]
    fn ttl_rejects_bad_values() {
        for bad in ["0", "-5", "abc", "1.5", "86401"] {
            assert!(parse_token_ttl(Some(bad)).is_err(), "{bad:?}");
        }
    }

    fn lookup(name: &str) -> Option<String> {
        match name {
            "ROOMS_INVITE_CODE" => Some("code".to_owned()),
            "LIVEKIT_URL" => Some("ws://localhost:7880".to_owned()),
            "LIVEKIT_API_KEY" => Some("key".to_owned()),
            "LIVEKIT_API_SECRET" => Some("secret".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn from_lookup_applies_defaults() {
        let config = Config::from_lookup(lookup).unwrap();
        assert_eq!(config.token_ttl_secs, 3600);
        assert_eq!(config.rooms, vec!["geral", "jogos", "afk"]);
        assert_eq!(config.bind_addr.to_string(), "127.0.0.1:8787");
    }

    #[test]
    fn from_lookup_requires_secrets() {
        let result = Config::from_lookup(|name| {
            if name == "LIVEKIT_API_SECRET" {
                None
            } else {
                lookup(name)
            }
        });
        assert!(matches!(
            result,
            Err(ConfigError::Missing("LIVEKIT_API_SECRET"))
        ));
    }

    #[test]
    fn debug_omits_secrets() {
        let config = Config::from_lookup(lookup).unwrap();
        let text = format!("{config:?}");
        assert!(!text.contains("secret"));
        assert!(!text.contains("code\""));
    }
}
