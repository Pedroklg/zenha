use std::time::Duration;

use async_trait::async_trait;
use reqwest::{StatusCode, header};
use rooms_protocol::{
    ChatHistoryResponse, ChatMessage, RoomSummary, RoomsResponse, SaveChatRequest, TokenRequest,
    TokenResponse, validate_room_name,
};

use crate::api::SessionError;
use crate::chat::validate_chat_id;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const INVITE_HEADER: &str = "x-invite-code";

#[async_trait]
pub trait TokenClient: Send + Sync + 'static {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError>;
    async fn fetch_rooms(&self, invite_code: &str) -> Result<Vec<RoomSummary>, SessionError>;
    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError>;
    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError>;
    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError>;
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

    fn chat_url(&self, room: &str) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!("/rooms/{room}/chat")))
    }

    fn chat_history_url(&self, room: &str, before: Option<&str>) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        match before {
            Some(cursor) => {
                validate_chat_id(cursor)?;
                Ok(format!("{base}?before={cursor}"))
            }
            None => Ok(base),
        }
    }

    fn chat_delete_url(&self, room: &str, id: &str) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        validate_chat_id(id)?;
        Ok(format!("{base}/{id}"))
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

fn chat_status_error(status: StatusCode) -> SessionError {
    match status {
        StatusCode::UNAUTHORIZED => SessionError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => SessionError::RateLimited,
        StatusCode::FORBIDDEN => {
            SessionError::TokenServer("not allowed for this room or message".to_owned())
        }
        StatusCode::NOT_FOUND => SessionError::TokenServer("message not found".to_owned()),
        StatusCode::CONFLICT => SessionError::TokenServer("message id conflict".to_owned()),
        StatusCode::SERVICE_UNAVAILABLE => {
            SessionError::TokenServer("chat history unavailable on server".to_owned())
        }
        StatusCode::BAD_REQUEST => SessionError::TokenServer("invalid chat request".to_owned()),
        other => SessionError::TokenServer(format!("server returned HTTP {}", other.as_u16())),
    }
}

async fn send_chat_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, SessionError> {
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    if !response.status().is_success() {
        return Err(chat_status_error(response.status()));
    }
    Ok(response)
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

    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        let url = self.chat_url(room)?;
        let response =
            send_chat_request(self.http.post(url).bearer_auth(jwt).json(&request)).await?;
        response
            .json::<ChatMessage>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        let url = self.chat_history_url(room, before)?;
        let response = send_chat_request(
            self.http
                .get(url)
                .bearer_auth(jwt)
                .header(header::ACCEPT, "application/json"),
        )
        .await?;
        response
            .json::<ChatHistoryResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let url = self.chat_delete_url(room, id)?;
        let _ = send_chat_request(self.http.delete(url).bearer_auth(jwt)).await?;
        Ok(())
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

    async fn save_chat(
        &self,
        _jwt: &str,
        _room: &str,
        _request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn fetch_chat_history(
        &self,
        _jwt: &str,
        _room: &str,
        _before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn delete_chat(&self, _jwt: &str, _room: &str, _id: &str) -> Result<(), SessionError> {
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
    fn chat_status_mapping() {
        assert_eq!(
            chat_status_error(StatusCode::UNAUTHORIZED),
            SessionError::Unauthorized
        );
        let rate_limited = chat_status_error(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rate_limited, SessionError::RateLimited);
        assert!(!rate_limited.to_string().contains("429"));
        assert!(matches!(
            chat_status_error(StatusCode::FORBIDDEN),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::NOT_FOUND),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::CONFLICT),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::SERVICE_UNAVAILABLE),
            SessionError::TokenServer(_)
        ));
        assert_eq!(
            chat_status_error(StatusCode::INTERNAL_SERVER_ERROR),
            SessionError::TokenServer("server returned HTTP 500".to_owned())
        );
    }

    #[test]
    fn url_joins_without_double_slash() {
        let client = HttpTokenClient::new("http://host:8080/").map(|c| c.url("/token"));
        assert_eq!(client.as_deref(), Ok("http://host:8080/token"));
    }

    #[test]
    fn chat_urls_build_and_validate() {
        let Ok(client) = HttpTokenClient::new("http://host:8080/") else {
            panic!("valid url should construct client");
        };
        assert_eq!(
            client.chat_url("geral").as_deref(),
            Ok("http://host:8080/rooms/geral/chat")
        );
        assert_eq!(
            client.chat_history_url("geral", None).as_deref(),
            Ok("http://host:8080/rooms/geral/chat")
        );
        assert_eq!(
            client.chat_history_url("geral", Some("msg_9-a")).as_deref(),
            Ok("http://host:8080/rooms/geral/chat?before=msg_9-a")
        );
        assert_eq!(
            client.chat_delete_url("geral", "msg_9-a").as_deref(),
            Ok("http://host:8080/rooms/geral/chat/msg_9-a")
        );
        assert!(client.chat_url("../bad").is_err());
        assert!(client.chat_history_url("geral", Some("a&b")).is_err());
        assert!(client.chat_delete_url("geral", "").is_err());
    }
}
