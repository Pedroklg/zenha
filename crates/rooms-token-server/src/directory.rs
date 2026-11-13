use async_trait::async_trait;
use livekit_api::services::ServerError;
use livekit_api::services::ServerErrorCode;
use livekit_api::services::ServiceError;
use livekit_api::services::room::RoomClient;

use crate::config::Config;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("room directory lookup failed: {0}")]
    Backend(String),
}

#[async_trait]
pub trait RoomDirectory: Send + Sync {
    async fn participants(&self, room: &str) -> Result<Vec<String>, DirectoryError>;
}

pub struct LiveKitDirectory {
    client: RoomClient,
}

impl LiveKitDirectory {
    pub fn new(config: &Config) -> Self {
        let client = RoomClient::with_api_key(
            &config.livekit_url,
            &config.livekit_api_key,
            &config.livekit_api_secret,
        )
        .with_request_timeout(REQUEST_TIMEOUT);
        Self { client }
    }
}

#[async_trait]
impl RoomDirectory for LiveKitDirectory {
    async fn participants(&self, room: &str) -> Result<Vec<String>, DirectoryError> {
        match self.client.list_participants(room).await {
            Ok(participants) => Ok(participants.into_iter().map(|p| p.identity).collect()),
            Err(ServiceError::Twirp(ServerError::Twirp(code)))
                if code.code == ServerErrorCode::NOT_FOUND =>
            {
                Ok(Vec::new())
            }
            Err(err) => Err(DirectoryError::Backend(err.to_string())),
        }
    }
}

#[cfg(test)]
pub use fake::FakeDirectory;

#[cfg(test)]
mod fake {
    use std::collections::{HashMap, HashSet};

    use async_trait::async_trait;

    use super::{DirectoryError, RoomDirectory};

    #[derive(Default)]
    pub struct FakeDirectory {
        rooms: HashMap<String, Vec<String>>,
        failing: HashSet<String>,
    }

    impl FakeDirectory {
        pub fn with_participants(mut self, room: &str, names: &[&str]) -> Self {
            self.rooms.insert(
                room.to_owned(),
                names.iter().map(|name| (*name).to_owned()).collect(),
            );
            self
        }

        pub fn failing_on(mut self, room: &str) -> Self {
            self.failing.insert(room.to_owned());
            self
        }
    }

    #[async_trait]
    impl RoomDirectory for FakeDirectory {
        async fn participants(&self, room: &str) -> Result<Vec<String>, DirectoryError> {
            if self.failing.contains(room) {
                return Err(DirectoryError::Backend("fake failure".to_owned()));
            }
            Ok(self.rooms.get(room).cloned().unwrap_or_default())
        }
    }
}
