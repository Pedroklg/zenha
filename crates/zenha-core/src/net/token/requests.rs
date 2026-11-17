//! Montagem de URLs e requisições do cliente HTTP do servidor de tokens.

use std::collections::BTreeMap;

use reqwest::header;
use zenha_protocol::{
    ROOM_KEYS_HEADER, TokenRequest, encode_room_keys, validate_avatar_version, validate_client_id,
    validate_room_name,
};

use super::{HttpTokenClient, INVITE_HEADER};
use crate::api::SessionError;
use crate::chat::validate_chat_id;

impl HttpTokenClient {
    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub(super) fn token_http_request(&self, request: &TokenRequest) -> reqwest::RequestBuilder {
        self.http.post(self.url("/token")).json(request)
    }

    pub(super) fn rooms_http_request(
        &self,
        invite_code: &str,
        room_keys: &BTreeMap<String, String>,
    ) -> reqwest::RequestBuilder {
        let request = self
            .http
            .get(self.url("/rooms"))
            .header(INVITE_HEADER, invite_code)
            .header(header::ACCEPT, "application/json");
        if room_keys.is_empty() {
            return request;
        }
        let keys = encode_room_keys(room_keys.iter().map(|(r, k)| (r.as_str(), k.as_str())));
        request.header(ROOM_KEYS_HEADER, keys)
    }

    pub(super) fn chat_url(&self, room: &str) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!("/rooms/{room}/chat")))
    }

    pub(super) fn chat_history_url(
        &self,
        room: &str,
        before: Option<&str>,
    ) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        match before {
            Some(cursor) => {
                validate_chat_id(cursor)?;
                Ok(format!("{base}?before={cursor}"))
            }
            None => Ok(base),
        }
    }

    pub(super) fn chat_delete_url(&self, room: &str, id: &str) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        validate_chat_id(id)?;
        Ok(format!("{base}/{id}"))
    }

    pub(super) fn admin_room_url(&self, id: &str) -> Result<String, SessionError> {
        validate_room_name(id).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!("/admin/rooms/{id}")))
    }

    pub(super) fn admin_participant_url(
        &self,
        room: &str,
        identity: &str,
        action: &str,
    ) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        validate_client_id(identity).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!(
            "/admin/rooms/{room}/participants/{identity}/{action}"
        )))
    }

    pub(super) fn admin_chat_delete_url(
        &self,
        room: &str,
        id: &str,
    ) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        validate_chat_id(id)?;
        Ok(self.url(&format!("/admin/rooms/{room}/chat/{id}")))
    }

    pub(super) fn avatar_get_url(
        &self,
        identity: &str,
        version: Option<&str>,
    ) -> Result<String, SessionError> {
        validate_client_id(identity).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        let base = self.url(&format!("/avatar/{identity}"));
        match version {
            Some(version) => {
                validate_avatar_version(version)
                    .map_err(|err| SessionError::InvalidInput(err.to_string()))?;
                Ok(format!("{base}?v={version}"))
            }
            None => Ok(base),
        }
    }
}
