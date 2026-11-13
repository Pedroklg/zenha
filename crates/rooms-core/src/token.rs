use std::time::Duration;

use async_trait::async_trait;
use reqwest::{StatusCode, header};
use rooms_protocol::{RoomSummary, RoomsResponse, TokenRequest, TokenResponse};

use crate::api::SessionError;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const INVITE_HEADER: &str = "x-invite-code";

#[async_trait]
pub trait TokenClient: Send + Sync + 'static {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError>;
    async fn fetch_rooms(&self, invite_code: &str) -> Result<Vec<RoomSummary>, SessionError>;
}

#[derive(Debug, Clone)]
pub struct HttpTokenClient {
    http: reqwest::Client,
    base_url: String,
}

impl HttpTokenClient {
    pub fn new(token_server_url: &str) -> Result<Self, SessionError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| SessionError::Internal(err.to_string()))?;
        Ok(Self {
            http,
            base_url: token_server_url.trim_end_matches('/').to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

fn transport_error(err: &reqwest::Error) -> SessionError {
    let message = if err.is_timeout() {
        "request timed out"
    } else if err.is_connect() {
        "connection failed"
    } else if err.is_decode() {
        "invalid response"
    } else {
        "request failed"
    };
    SessionError::TokenServer(message.to_owned())
}

fn status_error(status: StatusCode) -> SessionError {
    if status == StatusCode::UNAUTHORIZED {
        SessionError::InviteRejected
    } else {
        SessionError::TokenServer(format!("server returned HTTP {}", status.as_u16()))
    }
}

#[async_trait]
impl TokenClient for HttpTokenClient {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError> {
        let response = self
            .http
            .post(self.url("/token"))
            .json(&request)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        response
            .json::<TokenResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn fetch_rooms(&self, invite_code: &str) -> Result<Vec<RoomSummary>, SessionError> {
        let response = self
            .http
            .get(self.url("/rooms"))
            .header(INVITE_HEADER, invite_code)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let body = response
            .json::<RoomsResponse>()
            .await
            .map_err(|err| transport_error(&err))?;
        Ok(body.rooms)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct UnavailableTokenClient;

#[async_trait]
impl TokenClient for UnavailableTokenClient {
    async fn fetch_token(&self, _request: TokenRequest) -> Result<TokenResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn fetch_rooms(&self, _invite_code: &str) -> Result<Vec<RoomSummary>, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED),
            SessionError::InviteRejected
        );
        assert_eq!(
            status_error(StatusCode::INTERNAL_SERVER_ERROR),
            SessionError::TokenServer("server returned HTTP 500".to_owned())
        );
    }

    #[test]
    fn url_joins_without_double_slash() {
        let client = HttpTokenClient::new("http://host:8080/").map(|c| c.url("/token"));
        assert_eq!(client.as_deref(), Ok("http://host:8080/token"));
    }
}
