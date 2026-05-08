//! Wire vocabulary for the WebSocket channel — both directions.
//!
//! All events use the discriminator-tagged JSON described in
//! ARCHITECTURE.md §4.7:
//!
//! ```json
//! // server → client (presence)
//! { "type": "user_online",  "user_id": "<uuid>" }
//! { "type": "user_offline", "user_id": "<uuid>" }
//!
//! // server → client (chat)
//! { "type": "message_received", "message_id": "<uuid>", "timestamp": "..." }
//! { "type": "message", "message_id": "...", "from_user_id": "...",
//!   "content": "...", "emoji_ids": ["..."], "created_at": "..." }
//! { "type": "error", "code": "...", "message": "..." }
//!
//! // client → server
//! { "type": "message", "to_user_id": "<uuid>",
//!   "content": "...", "emoji_ids": ["..."] }
//! ```
//!
//! Pinning the wire format on these enums (rather than on ad-hoc JSON
//! literals at each call site) is what keeps the SPA and server from
//! drifting. Tests at the bottom of the file sanity-check serialisation
//! shapes that the SPA reads as fixed strings.

#![allow(dead_code)] // First non-test consumer: TODO [27] presence
                     // broadcast. Chat / error variants land in
                     // TODOs [28] / [30].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Outbound event the server pushes to a client. `serde(tag = "type",
/// rename_all = "snake_case")` produces the wire format the SPA reads.
///
/// `Clone` is required because a single event fans out to many local
/// sessions (each session task gets its own copy via mpsc).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundEvent {
    UserOnline {
        user_id: Uuid,
    },
    UserOffline {
        user_id: Uuid,
    },
    /// Sender ack — the server persisted the message and assigned it
    /// a `message_id`. Echoed back to the **sender** only.
    MessageReceived {
        message_id: Uuid,
        timestamp: DateTime<Utc>,
    },
    /// New chat message pushed to a recipient. Carries `message_id` so
    /// the SPA can dedupe replays (offline-list pop + late pub/sub) and
    /// implement reply-to / delete actions later (TODOs [31]–[33]).
    Message {
        message_id: Uuid,
        from_user_id: Uuid,
        content: String,
        emoji_ids: Vec<Uuid>,
        created_at: DateTime<Utc>,
    },
    /// Stable error envelope for client-visible failures. The `code`
    /// is intended for SPA-side branching; `message` is for display
    /// or logging. Detail-leak posture matches `AppError` for the REST
    /// surface — never reflect internal error chains here.
    Error {
        code: String,
        message: String,
    },
}

/// Client → server event. The session task parses inbound text frames
/// into this enum and dispatches by variant.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InboundEvent {
    /// Send a chat message to `to_user_id`. `emoji_ids` defaults to
    /// `[]` if omitted so the SPA can leave it out for plain-text.
    Message {
        to_user_id: Uuid,
        content: String,
        #[serde(default)]
        emoji_ids: Vec<Uuid>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the JSON shape against accidental refactors. The SPA reads
    /// `type` as a literal string discriminator; renaming the variant
    /// in Rust without updating the SPA would silently break the live
    /// channel.
    #[test]
    fn user_online_serializes_with_type_discriminator() {
        let id = Uuid::nil();
        let v = OutboundEvent::UserOnline { user_id: id };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["type"], "user_online");
        assert_eq!(json["user_id"], id.to_string());
    }

    #[test]
    fn user_offline_serializes_with_type_discriminator() {
        let id = Uuid::nil();
        let v = OutboundEvent::UserOffline { user_id: id };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["type"], "user_offline");
        assert_eq!(json["user_id"], id.to_string());
    }

    /// Round-trip via JSON — proves the (de)serialization is symmetric
    /// so the Pub/Sub envelope can carry the same wire bytes the SPA
    /// would receive directly.
    #[test]
    fn round_trip_through_json() {
        let id = Uuid::new_v4();
        for v in [
            OutboundEvent::UserOnline { user_id: id },
            OutboundEvent::UserOffline { user_id: id },
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: OutboundEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }

    /// `MessageReceived` is the sender-side ack. Pin the type
    /// discriminator + the field shape so the SPA's "did my message
    /// land?" branching can't silently break.
    #[test]
    fn message_received_serializes_as_spec() {
        let id = Uuid::nil();
        let ts = chrono::Utc::now();
        let v = OutboundEvent::MessageReceived {
            message_id: id,
            timestamp: ts,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["type"], "message_received");
        assert_eq!(json["message_id"], id.to_string());
        // `timestamp` is rendered as RFC 3339 by chrono's default serde.
        assert!(json["timestamp"].is_string());
    }

    /// `Message` is the recipient-side push. Same anti-drift pin —
    /// `from_user_id`, `content`, `emoji_ids`, `created_at`,
    /// `message_id` are the contracted fields.
    #[test]
    fn message_serializes_as_spec() {
        let v = OutboundEvent::Message {
            message_id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            content: "hi".into(),
            emoji_ids: vec![],
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&v).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(json["type"], "message");
        for required in [
            "message_id",
            "from_user_id",
            "content",
            "emoji_ids",
            "created_at",
        ] {
            assert!(obj.contains_key(required), "missing {required}: {json}");
        }
    }

    /// Error envelope shape — pinned because the SPA branches on `code`.
    #[test]
    fn error_serializes_as_spec() {
        let v = OutboundEvent::Error {
            code: "NOT_FRIENDS".into(),
            message: "you can only message friends".into(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "NOT_FRIENDS");
    }

    // ── InboundEvent ──

    #[test]
    fn inbound_message_parses_with_emoji_ids() {
        let raw = r#"{"type":"message","to_user_id":"00000000-0000-0000-0000-000000000000",
            "content":"hi","emoji_ids":[]}"#;
        let v: InboundEvent = serde_json::from_str(raw).unwrap();
        assert!(matches!(v, InboundEvent::Message { .. }));
    }

    /// `emoji_ids` is `#[serde(default)]` — an inbound message that
    /// omits it must still parse, and the resulting list must be empty.
    #[test]
    fn inbound_message_emoji_ids_defaults_to_empty() {
        let raw = r#"{"type":"message","to_user_id":"00000000-0000-0000-0000-000000000000",
            "content":"hi"}"#;
        let v: InboundEvent = serde_json::from_str(raw).unwrap();
        match v {
            InboundEvent::Message { emoji_ids, .. } => assert!(emoji_ids.is_empty()),
        }
    }

    /// Unknown variant fails parsing — that's the property that gives
    /// `chat::handle_inbound_text` a single bad-payload branch instead
    /// of silently ignoring future-protocol frames.
    #[test]
    fn inbound_unknown_type_fails_to_parse() {
        let raw = r#"{"type":"typing","to_user_id":"00000000-0000-0000-0000-000000000000"}"#;
        let result: Result<InboundEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown type should not parse");
    }
}
