use std::time::Duration;

use livekit_api::access_token::{AccessToken, AccessTokenError, VideoGrants};

use crate::config::Config;

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
