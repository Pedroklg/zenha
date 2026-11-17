use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use argon2::{Argon2, PasswordHash, PasswordHasher};
use zenha_protocol::validate_room_name;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8787";
const DEFAULT_TOKEN_TTL_SECS: u64 = 3600;
const MAX_TOKEN_TTL_SECS: u64 = 24 * 3600;
const DEFAULT_ROOM_LIST: &str = "geral,jogos,afk";
const DEFAULT_CHAT_DB: &str = "chat.db";

#[derive(Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub invite_code: String,
    pub invite_code_hash: Option<String>,
    pub admin_code_hash: Option<String>,
    pub livekit_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub token_ttl_secs: u64,
    pub rooms: Vec<String>,
    pub chat_db_path: PathBuf,
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
        Self::from_lookup(|name| {
            std::env::var(name).ok().or_else(|| {
                let legacy = name.replacen("ZENHA_", "ROOMS_", 1);
                std::env::var(legacy).ok()
            })
        })
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |name: &'static str| match lookup(name) {
            Some(value) if !value.trim().is_empty() => Ok(value),
            _ => Err(ConfigError::Missing(name)),
        };

        let bind_addr = lookup("ZENHA_BIND_ADDR")
            .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned())
            .parse()
            .map_err(|err: std::net::AddrParseError| ConfigError::Invalid {
                name: "ZENHA_BIND_ADDR",
                reason: err.to_string(),
            })?;

        let invite_code_hash =
            parse_password_hash("ZENHA_INVITE_CODE_HASH", lookup("ZENHA_INVITE_CODE_HASH"))?;
        let admin_code_hash =
            parse_password_hash("ZENHA_ADMIN_CODE_HASH", lookup("ZENHA_ADMIN_CODE_HASH"))?;

        let invite_code = if invite_code_hash.is_some() {
            lookup("ZENHA_INVITE_CODE")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_default()
        } else {
            let code = required("ZENHA_INVITE_CODE")?;
            tracing::warn!(
                var = "ZENHA_INVITE_CODE",
                "using plaintext invite code; set ZENHA_INVITE_CODE_HASH instead"
            );
            code
        };

        let chat_db_path = lookup("ZENHA_CHAT_DB")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_CHAT_DB.to_owned());

        Ok(Self {
            bind_addr,
            invite_code,
            invite_code_hash,
            admin_code_hash,
            livekit_url: required("LIVEKIT_URL")?,
            livekit_api_key: required("LIVEKIT_API_KEY")?,
            livekit_api_secret: required("LIVEKIT_API_SECRET")?,
            token_ttl_secs: parse_token_ttl(lookup("ZENHA_TOKEN_TTL_SECS").as_deref())?,
            rooms: parse_room_list(lookup("ZENHA_ROOM_LIST").as_deref())?,
            chat_db_path: PathBuf::from(chat_db_path),
        })
    }
}

fn parse_password_hash(
    name: &'static str,
    value: Option<String>,
) -> Result<Option<String>, ConfigError> {
    match value {
        Some(value) if !value.trim().is_empty() => {
            let trimmed = value.trim();
            let parsed = PasswordHash::new(trimmed).map_err(|err| ConfigError::Invalid {
                name,
                reason: err.to_string(),
            })?;
            if parsed.salt.is_none() || parsed.hash.is_none() {
                return Err(ConfigError::Invalid {
                    name,
                    reason: "hash must include salt and digest".to_owned(),
                });
            }
            Ok(Some(trimmed.to_owned()))
        }
        _ => Ok(None),
    }
}

pub fn hash_secret(secret: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!secret.trim().is_empty(), "password must not be empty");
    let hash: PasswordHash = Argon2::default()
        .hash_password(secret.as_bytes())
        .map_err(|err| anyhow::anyhow!("argon2 hash failed: {err}"))?;
    Ok(hash.to_string())
}

pub fn hash_password(line: &str) -> anyhow::Result<String> {
    let password = line.trim_end_matches(['\r', '\n']);
    hash_secret(password)
}

pub fn parse_token_ttl(value: Option<&str>) -> Result<u64, ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        name: "ZENHA_TOKEN_TTL_SECS",
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
            name: "ZENHA_ROOM_LIST",
            reason: format!("room {name:?}: {err}"),
        })?;
        if rooms.iter().any(|room| room == name) {
            return Err(ConfigError::Invalid {
                name: "ZENHA_ROOM_LIST",
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
            .field("chat_db_path", &self.chat_db_path)
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
                        name: "ZENHA_ROOM_LIST",
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
            "ZENHA_INVITE_CODE" => Some("code".to_owned()),
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
        assert_eq!(config.chat_db_path, std::path::Path::new("chat.db"));
    }

    #[test]
    fn from_lookup_reads_chat_db_path() {
        let config = Config::from_lookup(|name| match name {
            "ZENHA_CHAT_DB" => Some("  /var/lib/zenha/chat.db  ".to_owned()),
            _ => lookup(name),
        })
        .unwrap();
        assert_eq!(
            config.chat_db_path,
            std::path::Path::new("/var/lib/zenha/chat.db")
        );

        let blank = Config::from_lookup(|name| match name {
            "ZENHA_CHAT_DB" => Some("   ".to_owned()),
            _ => lookup(name),
        })
        .unwrap();
        assert_eq!(blank.chat_db_path, std::path::Path::new("chat.db"));
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
        let hash = hash_password("top-secret-password\n").unwrap();
        let admin_hash = hash_password("top-secret-admin\n").unwrap();
        let config = Config::from_lookup(|name| match name {
            "ZENHA_INVITE_CODE_HASH" => Some(hash.clone()),
            "ZENHA_ADMIN_CODE_HASH" => Some(admin_hash.clone()),
            _ => lookup(name),
        })
        .unwrap();
        let text = format!("{config:?}");
        assert!(!text.contains("secret"));
        assert!(!text.contains("code\""));
        assert!(!text.contains(&hash));
        assert!(!text.contains(&admin_hash));
        assert!(!text.contains("argon2"));
    }

    #[test]
    fn from_lookup_reads_optional_admin_hash_and_rejects_invalid() {
        let unset = Config::from_lookup(lookup).unwrap();
        assert!(unset.admin_code_hash.is_none());

        let blank = Config::from_lookup(|name| match name {
            "ZENHA_ADMIN_CODE_HASH" => Some("   ".to_owned()),
            _ => lookup(name),
        })
        .unwrap();
        assert!(blank.admin_code_hash.is_none());

        let hash = hash_password("admin-secret").unwrap();
        let valid = Config::from_lookup(|name| match name {
            "ZENHA_ADMIN_CODE_HASH" => Some(format!("  {hash}  ")),
            _ => lookup(name),
        })
        .unwrap();
        assert_eq!(valid.admin_code_hash.as_deref(), Some(hash.as_str()));

        let invalid = Config::from_lookup(|name| match name {
            "ZENHA_ADMIN_CODE_HASH" => Some("not-a-phc-string".to_owned()),
            _ => lookup(name),
        });
        assert!(matches!(
            invalid,
            Err(ConfigError::Invalid {
                name: "ZENHA_ADMIN_CODE_HASH",
                ..
            })
        ));
    }

    #[test]
    fn from_lookup_accepts_hash_without_plaintext_invite() {
        let hash = hash_password("only-hashed").unwrap();
        let config = Config::from_lookup(|name| match name {
            "ZENHA_INVITE_CODE" => None,
            "ZENHA_INVITE_CODE_HASH" => Some(hash.clone()),
            _ => lookup(name),
        })
        .unwrap();
        assert_eq!(config.invite_code_hash.as_deref(), Some(hash.as_str()));
        assert!(config.invite_code.is_empty());
    }

    #[test]
    fn from_lookup_rejects_invalid_hash() {
        let result = Config::from_lookup(|name| match name {
            "ZENHA_INVITE_CODE_HASH" => Some("not-a-phc-string".to_owned()),
            _ => lookup(name),
        });
        assert!(matches!(
            result,
            Err(ConfigError::Invalid {
                name: "ZENHA_INVITE_CODE_HASH",
                ..
            })
        ));
    }

    #[test]
    fn hash_password_trims_newlines_and_rejects_empty() {
        assert!(hash_password("").is_err());
        assert!(hash_password("\r\n").is_err());
        assert!(hash_password("   \n").is_err());

        let hash = hash_password("s3cret-pass\r\n").unwrap();
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            argon2::PasswordVerifier::verify_password(&Argon2::default(), b"s3cret-pass", &parsed)
                .is_ok()
        );
        assert!(
            argon2::PasswordVerifier::verify_password(&Argon2::default(), b"wrong", &parsed)
                .is_err()
        );
    }
}
