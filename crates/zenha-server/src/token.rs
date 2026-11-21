use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::Config;
use crate::room_access::hmac_sha256;

pub const ADMIN_TOKEN_KIND: &str = "zenha-admin";
pub const ADMIN_ROLE_ATTR: &str = "zenha_role";

const SERVER_TOKEN_TTL_SECS: u64 = 60;
const NBF_LEEWAY_SECS: i64 = 5;
const JWT_HEADER_JSON: &[u8] = br#"{"alg":"HS256","typ":"JWT"}"#;
const BASE64URL_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const BASE64_STD_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("system clock error")]
    Clock,
    #[error("failed to serialize jwt claims")]
    Serialize,
    #[error("malformed jwt")]
    Malformed,
    #[error("unsupported jwt algorithm")]
    UnsupportedAlgorithm,
    #[error("invalid jwt signature")]
    InvalidSignature,
    #[error("invalid jwt issuer")]
    InvalidIssuer,
    #[error("jwt expired")]
    Expired,
    #[error("jwt not yet valid")]
    NotYetValid,
    #[error("missing webhook body hash")]
    MissingBodyHash,
    #[error("invalid webhook body hash")]
    InvalidBodyHash,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoGrants {
    #[serde(skip_serializing_if = "is_false")]
    pub room_join: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub room: String,
    #[serde(skip_serializing_if = "is_false")]
    pub room_admin: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub room_create: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub room_list: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_publish: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_subscribe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_publish_data: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_update_own_metadata: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Claims {
    #[serde(default)]
    pub iss: String,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub nbf: i64,
    #[serde(default)]
    pub exp: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<String, String>,
    #[serde(default)]
    pub video: VideoGrants,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct JwtHeader {
    #[serde(default)]
    alg: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn now_unix_secs() -> Result<i64, TokenError> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TokenError::Clock)?
        .as_secs();
    i64::try_from(secs).map_err(|_| TokenError::Clock)
}

fn expiry_from(now: i64, ttl_secs: u64) -> i64 {
    let ttl = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    now.saturating_add(ttl)
}

pub fn sign_claims(api_secret: &str, claims: &Claims) -> Result<String, TokenError> {
    let header_b64 = base64url_encode(JWT_HEADER_JSON);
    let payload_json = serde_json::to_vec(claims).map_err(|_| TokenError::Serialize)?;
    let payload_b64 = base64url_encode(&payload_json);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(api_secret.as_bytes(), signing_input.as_bytes());
    let sig_b64 = base64url_encode(&sig);
    Ok(format!("{signing_input}.{sig_b64}"))
}

pub fn mint(config: &Config, identity: &str, name: &str, room: &str) -> Result<String, TokenError> {
    let now = now_unix_secs()?;
    let claims = Claims {
        iss: config.livekit_api_key.clone(),
        sub: identity.to_owned(),
        nbf: now,
        exp: expiry_from(now, config.token_ttl_secs),
        name: name.to_owned(),
        kind: String::new(),
        attributes: HashMap::new(),
        video: VideoGrants {
            room_join: true,
            room: room.to_owned(),
            room_admin: false,
            room_create: false,
            room_list: false,
            can_publish: Some(true),
            can_subscribe: Some(true),
            can_publish_data: Some(true),
            can_update_own_metadata: Some(true),
        },
        sha256: None,
    };
    sign_claims(&config.livekit_api_secret, &claims)
}

pub fn mint_admin(config: &Config, client_id: &str) -> Result<(String, u64), TokenError> {
    let now = now_unix_secs()?;
    let exp = expiry_from(now, config.token_ttl_secs);
    let claims = Claims {
        iss: config.livekit_api_key.clone(),
        sub: client_id.to_owned(),
        nbf: now,
        exp,
        name: String::new(),
        kind: ADMIN_TOKEN_KIND.to_owned(),
        attributes: HashMap::from([(ADMIN_ROLE_ATTR.to_owned(), "admin".to_owned())]),
        video: VideoGrants {
            room_join: false,
            room: String::new(),
            room_admin: false,
            room_create: false,
            room_list: false,
            can_publish: Some(false),
            can_subscribe: Some(false),
            can_publish_data: Some(false),
            can_update_own_metadata: None,
        },
        sha256: None,
    };
    let jwt = sign_claims(&config.livekit_api_secret, &claims)?;
    let expires_at = u64::try_from(exp).unwrap_or(u64::MAX);
    Ok((jwt, expires_at))
}

pub fn mint_server_token(
    api_key: &str,
    api_secret: &str,
    room: Option<&str>,
) -> Result<String, TokenError> {
    let now = now_unix_secs()?;
    let claims = Claims {
        iss: api_key.to_owned(),
        sub: String::new(),
        nbf: now,
        exp: expiry_from(now, SERVER_TOKEN_TTL_SECS),
        name: String::new(),
        kind: String::new(),
        attributes: HashMap::new(),
        video: VideoGrants {
            room_join: false,
            room: room.unwrap_or_default().to_owned(),
            room_admin: true,
            room_create: true,
            room_list: true,
            can_publish: None,
            can_subscribe: None,
            can_publish_data: None,
            can_update_own_metadata: None,
        },
        sha256: None,
    };
    sign_claims(api_secret, &claims)
}

pub fn verify_token(api_key: &str, api_secret: &str, token: &str) -> Result<Claims, TokenError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };

    let sig = base64url_decode(sig_b64)?;
    let signing_input = format!("{header_b64}.{payload_b64}");
    let expected = hmac_sha256(api_secret.as_bytes(), signing_input.as_bytes());
    if !bool::from(sig.as_slice().ct_eq(&expected)) {
        return Err(TokenError::InvalidSignature);
    }

    let header_bytes = base64url_decode(header_b64)?;
    let header: JwtHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| TokenError::Malformed)?;
    if header.alg != "HS256" {
        return Err(TokenError::UnsupportedAlgorithm);
    }

    let payload_bytes = base64url_decode(payload_b64)?;
    let claims: Claims =
        serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::Malformed)?;

    if !bool::from(claims.iss.as_bytes().ct_eq(api_key.as_bytes())) {
        return Err(TokenError::InvalidIssuer);
    }

    let now = now_unix_secs()?;
    if now >= claims.exp {
        return Err(TokenError::Expired);
    }
    if claims.nbf > now.saturating_add(NBF_LEEWAY_SECS) {
        return Err(TokenError::NotYetValid);
    }

    Ok(claims)
}

pub fn verify_webhook(
    api_key: &str,
    api_secret: &str,
    auth_header: &str,
    raw_body: &[u8],
) -> Result<(), TokenError> {
    let trimmed = auth_header.trim();
    let jwt = match trimmed.split_once(' ') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
        _ => trimmed,
    };
    let claims = verify_token(api_key, api_secret, jwt)?;
    let Some(claimed_hash) = claims.sha256.as_deref() else {
        return Err(TokenError::MissingBodyHash);
    };
    let digest = Sha256::digest(raw_body);
    let expected = base64_standard_encode(&digest);
    if !bool::from(expected.as_bytes().ct_eq(claimed_hash.as_bytes())) {
        return Err(TokenError::InvalidBodyHash);
    }
    Ok(())
}

pub fn is_admin_claims(claims: &Claims) -> bool {
    claims.kind == ADMIN_TOKEN_KIND
        && claims
            .attributes
            .get(ADMIN_ROLE_ATTR)
            .is_some_and(|role| role == "admin")
        && !claims.video.room_join
        && claims.video.room.is_empty()
        && !claims.video.room_admin
        && !claims.video.room_create
        && !claims.video.room_list
}

fn base64_encode(input: &[u8], table: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(table[((n >> 18) & 0x3f) as usize]));
        out.push(char::from(table[((n >> 12) & 0x3f) as usize]));
        if chunk.len() > 1 {
            out.push(char::from(table[((n >> 6) & 0x3f) as usize]));
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(table[(n & 0x3f) as usize]));
        } else if pad {
            out.push('=');
        }
    }
    out
}

fn base64url_encode(input: &[u8]) -> String {
    base64_encode(input, BASE64URL_TABLE, false)
}

pub(crate) fn base64_standard_encode(input: &[u8]) -> String {
    base64_encode(input, BASE64_STD_TABLE, true)
}

fn base64url_decode(input: &str) -> Result<Vec<u8>, TokenError> {
    let bytes = input.as_bytes();
    if bytes.len() % 4 == 1 {
        return Err(TokenError::Malformed);
    }
    let mut out = Vec::with_capacity((bytes.len() * 3) / 4);
    let mut chunks = bytes.chunks_exact(4);
    for chunk in &mut chunks {
        let a = decode_b64url_byte(chunk[0])?;
        let b = decode_b64url_byte(chunk[1])?;
        let c = decode_b64url_byte(chunk[2])?;
        let d = decode_b64url_byte(chunk[3])?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
        out.push((c << 6) | d);
    }
    match chunks.remainder() {
        [] => {}
        [c0, c1] => {
            let a = decode_b64url_byte(*c0)?;
            let b = decode_b64url_byte(*c1)?;
            if b & 0x0f != 0 {
                return Err(TokenError::Malformed);
            }
            out.push((a << 2) | (b >> 4));
        }
        [c0, c1, c2] => {
            let a = decode_b64url_byte(*c0)?;
            let b = decode_b64url_byte(*c1)?;
            let c = decode_b64url_byte(*c2)?;
            if c & 0x03 != 0 {
                return Err(TokenError::Malformed);
            }
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
        }
        _ => return Err(TokenError::Malformed),
    }
    Ok(out)
}

fn decode_b64url_byte(byte: u8) -> Result<u8, TokenError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(TokenError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn sample_config() -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            invite_code: "invite".to_owned(),
            invite_code_hash: None,
            admin_code_hash: None,
            livekit_url: "ws://livekit.test:7880".to_owned(),
            livekit_api_key: "key".to_owned(),
            livekit_api_secret: "secret-with-enough-length-0123456789".to_owned(),
            token_ttl_secs: 600,
            rooms: vec!["geral".to_owned()],
            chat_db_path: PathBuf::from("chat.db"),
        }
    }

    #[test]
    fn admin_token_has_admin_mark_and_no_livekit_room_or_server_grants() {
        let cfg = sample_config();
        let (jwt, expires_at) = mint_admin(&cfg, "admin-client-1").unwrap();
        let claims = verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &jwt).unwrap();
        assert_eq!(claims.sub, "admin-client-1");
        assert_eq!(expires_at, u64::try_from(claims.exp).unwrap());
        assert!(is_admin_claims(&claims));
        assert!(!claims.video.room_join);
        assert!(claims.video.room.is_empty());
        assert!(!claims.video.room_admin);
        assert!(!claims.video.room_create);
        assert!(!claims.video.room_list);
        assert_eq!(claims.video.can_publish, Some(false));
        assert_eq!(claims.video.can_subscribe, Some(false));
        assert_eq!(claims.video.can_publish_data, Some(false));
        assert_eq!(claims.video.can_update_own_metadata, None);
    }

    #[test]
    fn room_token_is_not_accepted_as_admin_claims() {
        let cfg = sample_config();
        let jwt = mint(&cfg, "client-1", "Ana", "geral").unwrap();
        let claims = verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &jwt).unwrap();
        assert!(!is_admin_claims(&claims));
        assert_eq!(claims.sub, "client-1");
        assert_eq!(claims.name, "Ana");
        assert!(claims.video.room_join);
        assert_eq!(claims.video.room, "geral");
        assert_eq!(claims.video.can_publish, Some(true));
        assert_eq!(claims.video.can_subscribe, Some(true));
        assert_eq!(claims.video.can_publish_data, Some(true));
        assert_eq!(claims.video.can_update_own_metadata, Some(true));
    }

    #[test]
    fn server_token_carries_admin_and_service_grants() {
        let cfg = sample_config();
        let jwt = mint_server_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, Some("geral"))
            .unwrap();
        let claims = verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &jwt).unwrap();
        assert!(claims.video.room_admin);
        assert!(claims.video.room_create);
        assert!(claims.video.room_list);
        assert_eq!(claims.video.room, "geral");
        assert!(!claims.video.room_join);
        assert!(!is_admin_claims(&claims));
    }

    #[test]
    fn verify_rejects_expired_future_wrong_secret_wrong_issuer_and_bad_alg() {
        let cfg = sample_config();
        let now = now_unix_secs().unwrap();

        let valid = mint(&cfg, "client-1", "Ana", "geral").unwrap();
        assert!(matches!(
            verify_token(&cfg.livekit_api_key, "wrong-secret", &valid),
            Err(TokenError::InvalidSignature)
        ));
        assert!(matches!(
            verify_token("wrong-key", &cfg.livekit_api_secret, &valid),
            Err(TokenError::InvalidIssuer)
        ));

        let expired = sign_claims(
            &cfg.livekit_api_secret,
            &Claims {
                iss: cfg.livekit_api_key.clone(),
                sub: "client-1".to_owned(),
                nbf: 0,
                exp: now - 1,
                ..Claims::default()
            },
        )
        .unwrap();
        assert!(matches!(
            verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &expired),
            Err(TokenError::Expired)
        ));

        let future = sign_claims(
            &cfg.livekit_api_secret,
            &Claims {
                iss: cfg.livekit_api_key.clone(),
                sub: "client-1".to_owned(),
                nbf: now + 60,
                exp: now + 600,
                ..Claims::default()
            },
        )
        .unwrap();
        assert!(matches!(
            verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &future),
            Err(TokenError::NotYetValid)
        ));

        let bad_header = base64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload =
            base64url_encode(format!(r#"{{"iss":"key","exp":{}}}"#, now + 600).as_bytes());
        let signing_input = format!("{bad_header}.{payload}");
        let sig = base64url_encode(&hmac_sha256(
            cfg.livekit_api_secret.as_bytes(),
            signing_input.as_bytes(),
        ));
        let bad_alg = format!("{signing_input}.{sig}");
        assert!(matches!(
            verify_token(&cfg.livekit_api_key, &cfg.livekit_api_secret, &bad_alg),
            Err(TokenError::UnsupportedAlgorithm)
        ));
    }

    #[test]
    fn verify_webhook_checks_jwt_and_body_sha256() {
        let cfg = sample_config();
        let body = br#"{"event":"participant_joined","room":{"name":"geral"}}"#;
        let now = now_unix_secs().unwrap();
        let digest = Sha256::digest(body);
        let jwt = sign_claims(
            &cfg.livekit_api_secret,
            &Claims {
                iss: cfg.livekit_api_key.clone(),
                nbf: now,
                exp: now + 300,
                sha256: Some(base64_standard_encode(&digest)),
                ..Claims::default()
            },
        )
        .unwrap();

        verify_webhook(&cfg.livekit_api_key, &cfg.livekit_api_secret, &jwt, body).unwrap();
        verify_webhook(
            &cfg.livekit_api_key,
            &cfg.livekit_api_secret,
            &format!("Bearer {jwt}"),
            body,
        )
        .unwrap();

        assert!(matches!(
            verify_webhook(
                &cfg.livekit_api_key,
                &cfg.livekit_api_secret,
                &jwt,
                br#"{"event":"participant_joined","room":{"name":"other"}}"#
            ),
            Err(TokenError::InvalidBodyHash)
        ));
        assert!(matches!(
            verify_webhook(&cfg.livekit_api_key, "wrong-secret", &jwt, body),
            Err(TokenError::InvalidSignature)
        ));

        let no_hash = sign_claims(
            &cfg.livekit_api_secret,
            &Claims {
                iss: cfg.livekit_api_key.clone(),
                nbf: now,
                exp: now + 300,
                sha256: None,
                ..Claims::default()
            },
        )
        .unwrap();
        assert!(matches!(
            verify_webhook(
                &cfg.livekit_api_key,
                &cfg.livekit_api_secret,
                &no_hash,
                body
            ),
            Err(TokenError::MissingBodyHash)
        ));
    }

    #[test]
    fn base64_standard_matches_rfc_4648_vectors() {
        assert_eq!(base64_standard_encode(b""), "");
        assert_eq!(base64_standard_encode(b"f"), "Zg==");
        assert_eq!(base64_standard_encode(b"fo"), "Zm8=");
        assert_eq!(base64_standard_encode(b"foo"), "Zm9v");
        assert_eq!(base64_standard_encode(b"foobar"), "Zm9vYmFy");
    }
}
