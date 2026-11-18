//! Chat sending, history, deletion and token renewal in the session.

use std::future::Future;
use std::sync::Arc;

use zenha_protocol::{ChatHistoryResponse, SaveChatRequest, validate_chat_text};

use crate::api::{Event, SessionError};
use crate::chat::{self as chat_codec, CHAT_DELETE_TOPIC, CHAT_TOPIC};
use crate::clock::unix_ms;
use crate::net::token::TokenClient;

use super::Session;
use super::connection::Phase;

impl Session {
    async fn run_chat_request<T, F, Fut>(&mut self, op: F) -> Result<T, SessionError>
    where
        F: Fn(Arc<dyn TokenClient>, String, String) -> Fut,
        Fut: Future<Output = Result<T, SessionError>>,
    {
        self.run_chat_request_once_renewed(op)
            .await
            .map_err(|err| match err {
                SessionError::TokenServer(reason) => SessionError::Chat(reason),
                other => other,
            })
    }

    async fn run_chat_request_once_renewed<T, F, Fut>(&mut self, op: F) -> Result<T, SessionError>
    where
        F: Fn(Arc<dyn TokenClient>, String, String) -> Fut,
        Fut: Future<Output = Result<T, SessionError>>,
    {
        let (room, jwt) = match &self.phase {
            Phase::Connected { room, jwt, .. } => (room.clone(), jwt.clone()),
            _ => return Err(SessionError::NotInRoom),
        };
        let tokens = self.tokens.clone();
        match op(tokens.clone(), jwt, room.clone()).await {
            Err(SessionError::Unauthorized) => {
                // Renew only the JWT on 401 so active LiveKit media tracks stay uninterrupted.
                let request = self.token_request(&room);
                let renewed = tokens.fetch_token(request).await?;
                if let Phase::Connected {
                    jwt: current_jwt,
                    room: current_room,
                    ..
                } = &mut self.phase
                    && *current_room == room
                {
                    *current_jwt = renewed.token.clone();
                }
                op(tokens, renewed.token, room).await
            }
            other => other,
        }
    }

    pub(super) async fn fetch_history_page(
        &mut self,
        before: Option<String>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        if let Some(cursor) = &before {
            chat_codec::validate_chat_id(cursor)?;
        }
        let page = self
            .run_chat_request(|tokens, jwt, room| {
                let before = before.clone();
                async move {
                    tokens
                        .fetch_chat_history(&jwt, &room, before.as_deref())
                        .await
                }
            })
            .await?;
        let mut page = page;
        page.webhook_posts.retain(|post| post.validate().is_ok());
        Ok(page)
    }

    pub(super) async fn send_chat(&mut self, text: &str, save: bool) {
        let Phase::Connected { local_identity, .. } = &self.phase else {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        };
        let local_identity = local_identity.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let sent_at_unix_ms = unix_ms();
        let (message, payload) = if save {
            if let Err(err) = validate_chat_text(text) {
                self.emit(Event::CommandFailed(SessionError::InvalidInput(
                    err.to_string(),
                )));
                return;
            }
            let request = SaveChatRequest {
                id,
                text: text.to_owned(),
                sent_at_unix_ms,
            };
            let saved_message = match self
                .run_chat_request(|tokens, jwt, room| {
                    let request = request.clone();
                    async move { tokens.save_chat(&jwt, &room, request).await }
                })
                .await
            {
                Ok(mut message) => {
                    message.sender = local_identity.clone();
                    message
                }
                Err(err) => {
                    self.emit(Event::CommandFailed(err));
                    return;
                }
            };
            let payload = match chat_codec::encode_message(&saved_message) {
                Ok(payload) => payload,
                Err(err) => {
                    self.emit(Event::CommandFailed(err));
                    return;
                }
            };
            (saved_message, payload)
        } else {
            match chat_codec::build_outgoing(id, &local_identity, text, sent_at_unix_ms) {
                Ok(built) => built,
                Err(err) => {
                    self.emit(Event::CommandFailed(err));
                    return;
                }
            }
        };
        match self.backend.send_data(CHAT_TOPIC, payload).await {
            Ok(()) => self.emit(Event::ChatReceived(message)),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn load_chat_history(&mut self, before: Option<String>) {
        match self.fetch_history_page(before.clone()).await {
            Ok(page) => self.emit(Event::ChatHistory {
                messages: page.messages,
                has_more: page.has_more,
                before,
                webhook_posts: page.webhook_posts,
            }),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn delete_chat(&mut self, id: &str) {
        let Phase::Connected { local_identity, .. } = &self.phase else {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        };
        let local_identity = local_identity.clone();
        let payload = match chat_codec::build_delete_outgoing(id) {
            Ok(payload) => payload,
            Err(err) => {
                self.emit(Event::CommandFailed(err));
                return;
            }
        };
        if let Err(err) = self
            .run_chat_request(|tokens, jwt, room| {
                let id = id.to_owned();
                async move { tokens.delete_chat(&jwt, &room, &id).await }
            })
            .await
        {
            self.emit(Event::CommandFailed(err));
            return;
        }
        match self.backend.send_data(CHAT_DELETE_TOPIC, payload).await {
            Ok(()) => self.emit(Event::ChatDeleted {
                id: id.to_owned(),
                sender: local_identity,
            }),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use zenha_protocol::{
        ChatDeleted, ChatHistoryResponse, ChatMessage, SavedChatMessage, WebhookPost,
    };

    use crate::api::{Command, Event, SessionError};
    use crate::backend::BackendEvent;
    use crate::chat::{CHAT_DELETE_TOPIC, CHAT_TOPIC};
    use crate::testing::BackendCall;

    use super::super::test_support::{data, harness};

    fn chat_json(sender: &str, text: &str) -> Vec<u8> {
        serde_json::to_vec(&ChatMessage {
            id: "m1".to_owned(),
            sender: sender.to_owned(),
            sent_at_unix_ms: 9,
            text: text.to_owned(),
            saved: false,
        })
        .unwrap()
    }

    fn webhook_post(id: &str) -> WebhookPost {
        WebhookPost {
            id: id.to_owned(),
            name: "GC".to_owned(),
            sent_at_unix_ms: 5,
            content: Some("lobby".to_owned()),
            embeds: Vec::new(),
        }
    }

    fn saved_item(id: &str, text: &str) -> SavedChatMessage {
        SavedChatMessage {
            message: ChatMessage {
                id: id.to_owned(),
                sender: "pedro".to_owned(),
                sent_at_unix_ms: 100,
                text: text.to_owned(),
                saved: true,
            },
            sender_id: "client-pedro".to_owned(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn outgoing_chat_reaches_backend_and_is_echoed() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SendChat {
            text: "oi".to_owned(),
            save: false,
        });
        let Event::ChatReceived(echo) = h.next().await else {
            panic!("expected echo");
        };
        assert_eq!(echo.sender, "local");
        assert_eq!(echo.text, "oi");
        assert!(!echo.saved);
        assert!(echo.sent_at_unix_ms > 0);

        let sent = h.backend.calls().into_iter().find_map(|call| match call {
            BackendCall::SendData { topic, payload } => Some((topic, payload)),
            _ => None,
        });
        let (topic, payload) = sent.unwrap();
        assert_eq!(topic, "chat");
        assert_eq!(
            serde_json::from_slice::<ChatMessage>(&payload).unwrap(),
            echo
        );
    }

    #[tokio::test(start_paused = true)]
    async fn chat_outside_a_room_or_invalid_is_rejected() {
        let mut h = harness();
        h.send(Command::SendChat {
            text: "oi".to_owned(),
            save: false,
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        h.send(Command::SendChat {
            text: "   ".to_owned(),
            save: false,
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn incoming_chat_uses_backend_identity_and_drops_bad_payloads() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(data("chat", b"{broken".to_vec()));
        h.backend
            .emit(data("chat", chat_json("ana", &"x".repeat(2001))));
        h.backend.emit(data("other", chat_json("ana", "ignored")));
        h.backend.emit(data("chat", chat_json("spoofed", "olá")));
        let Event::ChatReceived(message) = h.next().await else {
            panic!("expected chat");
        };
        assert_eq!(message.sender, "ana");
        assert_eq!(message.text, "olá");
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn save_true_posts_before_publishing_with_saved_flag_and_emits_local() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SendChat {
            text: "nota importante".to_owned(),
            save: true,
        });
        let Event::ChatReceived(echo) = h.next().await else {
            panic!("expected chat received");
        };
        assert!(echo.saved);
        assert_eq!(echo.text, "nota importante");
        assert_eq!(echo.sender, "local");

        let save_calls = h.tokens.save_calls();
        assert_eq!(save_calls.len(), 1);
        assert_eq!(save_calls[0].0, "token-geral-1");
        assert_eq!(save_calls[0].1, "geral");
        assert_eq!(save_calls[0].2.id, echo.id);
        assert_eq!(save_calls[0].2.text, "nota importante");

        let sent = h.backend.calls().into_iter().find_map(|call| match call {
            BackendCall::SendData { topic, payload } => Some((topic, payload)),
            _ => None,
        });
        let (topic, payload) = sent.unwrap();
        assert_eq!(topic, CHAT_TOPIC);
        let published: ChatMessage = serde_json::from_slice(&payload).unwrap();
        assert!(published.saved);
        assert_eq!(published, echo);
    }

    #[tokio::test(start_paused = true)]
    async fn save_failure_emits_command_failed_and_does_not_publish() {
        let mut h = harness();
        h.join("geral").await;
        h.tokens.push_save_result(Err(SessionError::RateLimited));
        h.send(Command::SendChat {
            text: "oi".to_owned(),
            save: true,
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::RateLimited)
        );
        assert!(
            !h.backend
                .calls()
                .iter()
                .any(|call| matches!(call, BackendCall::SendData { .. }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unauthorized_chat_call_refreshes_token_retries_once_and_does_not_loop() {
        let mut h = harness();
        h.join("geral").await;
        assert_eq!(h.backend.connect_count(), 1);
        assert_eq!(h.tokens.requests().len(), 1);

        // Single 401 refreshes JWT, retries once, updates stored token, and does not reconnect LiveKit.
        h.tokens.push_save_result(Err(SessionError::Unauthorized));
        h.send(Command::SendChat {
            text: "primeira".to_owned(),
            save: true,
        });
        let Event::ChatReceived(first) = h.next().await else {
            panic!("expected retry success");
        };
        assert_eq!(first.text, "primeira");
        assert!(first.saved);
        assert_eq!(h.tokens.requests().len(), 2);
        assert_eq!(h.backend.connect_count(), 1);
        assert_eq!(h.tokens.save_calls()[0].0, "token-geral-1");
        assert_eq!(h.tokens.save_calls()[1].0, "token-geral-2");

        // Next call reuses the refreshed JWT directly.
        h.send(Command::SendChat {
            text: "segunda".to_owned(),
            save: true,
        });
        assert!(matches!(h.next().await, Event::ChatReceived(_)));
        assert_eq!(h.tokens.requests().len(), 2);
        assert_eq!(h.tokens.save_calls()[2].0, "token-geral-2");

        // Double 401 stops after one retry without looping infinitely.
        h.tokens.push_save_result(Err(SessionError::Unauthorized));
        h.tokens.push_save_result(Err(SessionError::Unauthorized));
        h.send(Command::SendChat {
            text: "falha".to_owned(),
            save: true,
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::Unauthorized)
        );
        assert_eq!(h.tokens.requests().len(), 3);
        assert_eq!(h.tokens.save_calls().len(), 5);
        assert_eq!(h.tokens.save_calls()[3].0, "token-geral-2");
        assert_eq!(h.tokens.save_calls()[4].0, "token-geral-3");
        assert_eq!(h.backend.connect_count(), 1);
        let published_count = h
            .backend
            .calls()
            .into_iter()
            .filter(
                |call| matches!(call, BackendCall::SendData { topic, .. } if topic == CHAT_TOPIC),
            )
            .count();
        assert_eq!(published_count, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn join_auto_loads_history_and_load_chat_history_forwards_before() {
        let mut h = harness();
        let page1 = ChatHistoryResponse {
            messages: vec![saved_item("m2", "dois"), saved_item("m1", "um")],
            has_more: true,
            webhook_posts: vec![webhook_post("w1"), webhook_post("bad id!")],
        };
        let page2 = ChatHistoryResponse {
            messages: vec![saved_item("m0", "zero")],
            has_more: false,
            webhook_posts: Vec::new(),
        };
        h.tokens.set_chat_history_page(None, page1.clone());
        h.tokens
            .set_chat_history_page(Some("m1".to_owned()), page2.clone());

        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        assert_eq!(
            h.next().await,
            Event::ChatHistory {
                messages: page1.messages,
                has_more: true,
                before: None,
                webhook_posts: vec![webhook_post("w1")],
            }
        );

        h.send(Command::LoadChatHistory {
            before: Some("m1".to_owned()),
        });
        assert_eq!(
            h.next().await,
            Event::ChatHistory {
                messages: page2.messages,
                has_more: false,
                before: Some("m1".to_owned()),
                webhook_posts: Vec::new(),
            }
        );
        assert_eq!(h.tokens.history_calls().len(), 2);

        // Explicit LoadChatHistory failure emits CommandFailed.
        h.tokens.push_history_result(Err(SessionError::RateLimited));
        h.send(Command::LoadChatHistory { before: None });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::RateLimited)
        );
        assert_eq!(h.tokens.history_calls().len(), 3);

        // Reconnect must not auto-fetch initial history again.
        h.backend.emit(BackendEvent::Disconnected {
            reason: "drop".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(matches!(h.next().await, Event::ParticipantList(_)));
        assert_eq!(h.tokens.history_calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn initial_history_failure_does_not_fail_session_or_emit_empty_history() {
        let mut h = harness();
        h.tokens
            .push_history_result(Err(SessionError::TokenServer("unavailable".to_owned())));
        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        assert!(h.backend.is_connected());

        h.send(Command::SendChat {
            text: "ping".to_owned(),
            save: false,
        });
        assert!(matches!(h.next().await, Event::ChatReceived(msg) if msg.text == "ping"));
    }

    #[tokio::test(start_paused = true)]
    async fn delete_chat_publishes_on_delete_topic_and_incoming_uses_backend_identity() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::DeleteChat {
            id: "msg-1".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::ChatDeleted {
                id: "msg-1".to_owned(),
                sender: "local".to_owned(),
            }
        );
        assert_eq!(
            h.tokens.delete_calls(),
            vec![(
                "token-geral-1".to_owned(),
                "geral".to_owned(),
                "msg-1".to_owned()
            )]
        );
        let published_delete = h.backend.calls().into_iter().find_map(|call| match call {
            BackendCall::SendData { topic, payload } if topic == CHAT_DELETE_TOPIC => Some(payload),
            _ => None,
        });
        assert_eq!(
            serde_json::from_slice::<ChatDeleted>(&published_delete.unwrap()).unwrap(),
            ChatDeleted {
                id: "msg-1".to_owned()
            }
        );

        // DELETE server failure does not publish or emit ChatDeleted.
        h.tokens.push_delete_result(Err(SessionError::TokenServer(
            "message not found".to_owned(),
        )));
        h.send(Command::DeleteChat {
            id: "msg-2".to_owned(),
        });
        assert!(matches!(h.next().await, Event::CommandFailed(_)));
        let delete_publishes = h
            .backend
            .calls()
            .into_iter()
            .filter(|call| matches!(call, BackendCall::SendData { topic, .. } if topic == CHAT_DELETE_TOPIC))
            .count();
        assert_eq!(delete_publishes, 1);

        // Incoming invalid payloads on chat.delete are dropped; valid uses backend identity.
        h.backend
            .emit(data(CHAT_DELETE_TOPIC, b"not json".to_vec()));
        h.backend.emit(data(
            CHAT_DELETE_TOPIC,
            serde_json::to_vec(&ChatDeleted { id: String::new() }).unwrap(),
        ));
        h.backend.emit(data(
            CHAT_DELETE_TOPIC,
            serde_json::to_vec(&ChatDeleted {
                id: "msg-9".to_owned(),
            })
            .unwrap(),
        ));
        assert_eq!(
            h.next().await,
            Event::ChatDeleted {
                id: "msg-9".to_owned(),
                sender: "ana".to_owned(),
            }
        );
    }
}
