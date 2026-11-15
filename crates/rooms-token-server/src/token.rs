use std::time::Duration;

use livekit_api::access_token::{AccessToken, AccessTokenError, Claims, VideoGrants};

use crate::config::Config;

pub const ADMIN_TOKEN_KIND: &str = "zenha-admin";
pub const ADMIN_ROLE_ATTR: &str = "zenha_role";

pub fn mint(
    config: &Config,
    identity: &str,
    name: &str,
    room: &str,
) -> Result<String, AccessTokenError> {
    AccessToken::with_api_key(&config.livekit_api_key, &config.livekit_api_secret)
        .with_ttl(Duration::from_secs(config.token_ttl_secs))
        .with_identity(identity)
        .with_name(name)
        .with_grants(VideoGrants {
            room_join: true,
            room: room.to_owned(),
            can_publish: Some(true),
            can_subscribe: Some(true),
            can_publish_data: Some(true),
            ..Default::default()
        })
        .to_jwt()
}

pub fn mint_admin(config: &Config, client_id: &str) -> Result<(String, u64), AccessTokenError> {
    let token = AccessToken::with_api_key(&config.livekit_api_key, &config.livekit_api_secret)
        .with_ttl(Duration::from_secs(config.token_ttl_secs))
        .with_identity(client_id)
        .with_kind(ADMIN_TOKEN_KIND)
        .with_attributes([(ADMIN_ROLE_ATTR, "admin")])
        .with_grants(VideoGrants {
            room_join: false,
            can_publish: Some(false),
            can_subscribe: Some(false),
            can_publish_data: Some(false),
            ..Default::default()
        })
        .to_jwt()?;
    let claims = Claims::from_unverified(&token)?;
    let expires_at = u64::try_from(claims.exp).unwrap_or(u64::MAX);
    Ok((token, expires_at))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use livekit_api::access_token::TokenVerifier;

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
        let claims = TokenVerifier::with_api_key(&cfg.livekit_api_key, &cfg.livekit_api_secret)
            .verify(&jwt)
            .unwrap();
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
    }

    #[test]
    fn room_token_is_not_accepted_as_admin_claims() {
        let cfg = sample_config();
        let jwt = mint(&cfg, "client-1", "Ana", "geral").unwrap();
        let claims = TokenVerifier::with_api_key(&cfg.livekit_api_key, &cfg.livekit_api_secret)
            .verify(&jwt)
            .unwrap();
        assert!(!is_admin_claims(&claims));
    }
}
