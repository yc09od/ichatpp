//! Chat message ingest, persistence, and delivery (TODO [28]).
//!
//! Pipeline for a client → server `message` frame:
//!
//! 1. **Parse** the JSON into `InboundEvent::Message`.
//! 2. **Validate** content length, recipient ≠ self, sender-recipient
//!    friendship.
//! 3. **Persist** — single transaction inserts the `messages` row and
//!    every `message_emojis` link.
//! 4. **Ack** the sender with `OutboundEvent::MessageReceived`.
//! 5. **Deliver** the recipient-side `OutboundEvent::Message`:
//!    - locally (mpsc) if the recipient has any session on this
//!      instance,
//!    - cross-instance (Redis Pub/Sub) if `online_users` says so,
//!    - to the offline queue (`offline_messages:{user_id}` Redis
//!      List) otherwise.
//!
//! On reconnect, [`drain_offline_queue`] pops every pending message
//! the user has accumulated and pushes them through their fresh mpsc.
//!
//! ## Cross-instance delivery
//!
//! Uses a **target-tagged** envelope on a separate Pub/Sub channel
//! (`chat_events`) — distinct from the origin-tagged presence path.
//! Subscribers route by `target_user_id`; self-suppression happens via
//! `origin_instance_id` so the publisher's own subscriber doesn't
//! double-deliver alongside the local mpsc fan-out.
//!
//! ## Lossy edge case
//!
//! If a recipient disconnects in the millisecond between the sender's
//! `is_online` check (returns true) and the recipient's `mark_offline`,
//! the message is published to Pub/Sub but no instance has the user
//! locally — so the message is dropped. Acceptable for the MVP
//! acceptance criterion (online B gets it; offline B gets it on
//! reconnect via the queue); proper at-least-once delivery would need
//! sequence numbers or read receipts, future work.

#![allow(dead_code)] // First non-test consumer mounts in TODO [28];
                     // run_subscriber starts from main.rs.

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ws::event::{InboundEvent, OutboundEvent};
use crate::ws::presence;
use crate::ws::registry::SessionRegistry;
use crate::AppState;

/// Hard cap on a single chat message's character length. Mirrors the
/// TODO ("内容长度 ≤ 5000") and is counted in `chars()` (Unicode
/// codepoints) so a multibyte payload doesn't accidentally squeeze
/// past a byte-count check.
pub const MAX_MESSAGE_CONTENT_LEN: usize = 5000;

/// Redis Pub/Sub channel for chat events. Distinct from
/// `presence_events` because the routing model is different
/// (target-tagged direct route vs. origin-tagged friend fan-out).
pub(crate) const CHAT_CHANNEL: &str = "chat_events";

/// Redis List key prefix for per-user offline message queues.
const OFFLINE_QUEUE_PREFIX: &str = "offline_messages:";

/// Subscriber reconnect back-off, mirrors the presence subscriber's
/// value for consistency.
const SUBSCRIBER_RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Wire envelope for chat events on the Pub/Sub channel. Carries
/// `origin_instance_id` so the publisher's own subscriber filters
/// itself out, and `target_user_id` so subscribers route directly
/// without re-querying friend lists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatEnvelope {
    pub origin_instance_id: Uuid,
    pub target_user_id: Uuid,
    pub event: OutboundEvent,
}

fn offline_key(user_id: Uuid) -> String {
    format!("{OFFLINE_QUEUE_PREFIX}{user_id}")
}

// ────────────────────────────────────────────────────────────────────────
// Inbound dispatch
// ────────────────────────────────────────────────────────────────────────

/// Entry point called from the session task on every text frame.
/// `sender_tx` is the session's own outbound mpsc — used for ack and
/// any error feedback to the originating client.
pub async fn handle_inbound_text(
    state: &AppState,
    sender_id: Uuid,
    sender_tx: &mpsc::Sender<OutboundEvent>,
    raw: &str,
) {
    let event: InboundEvent = match serde_json::from_str(raw) {
        Ok(e) => e,
        Err(e) => {
            send_error(
                sender_tx,
                "BAD_PAYLOAD",
                &format!("could not parse event: {e}"),
            );
            return;
        }
    };

    match event {
        InboundEvent::Message {
            to_user_id,
            content,
            emoji_ids,
        } => {
            send_message(state, sender_id, to_user_id, content, emoji_ids, sender_tx).await;
        }
    }
}

/// `try_send` an error event back to the sender. Best-effort — if the
/// sender's mpsc is full (drop-on-full policy from registry.rs), a
/// queued real message will arrive ahead of any error feedback, which
/// is fine.
fn send_error(sender_tx: &mpsc::Sender<OutboundEvent>, code: &str, message: &str) {
    let _ = sender_tx.try_send(OutboundEvent::Error {
        code: code.to_owned(),
        message: message.to_owned(),
    });
}

// ────────────────────────────────────────────────────────────────────────
// Send pipeline
// ────────────────────────────────────────────────────────────────────────

async fn send_message(
    state: &AppState,
    sender_id: Uuid,
    recipient_id: Uuid,
    content: String,
    emoji_ids: Vec<Uuid>,
    sender_tx: &mpsc::Sender<OutboundEvent>,
) {
    // Pure validation first — these checks all fail without any DB
    // round-trip, so a bad payload doesn't tie up the pool.
    if content.is_empty() {
        send_error(sender_tx, "INVALID_CONTENT", "content cannot be empty");
        return;
    }
    if content.chars().count() > MAX_MESSAGE_CONTENT_LEN {
        send_error(
            sender_tx,
            "INVALID_CONTENT",
            &format!("content exceeds the {MAX_MESSAGE_CONTENT_LEN}-char limit"),
        );
        return;
    }
    if sender_id == recipient_id {
        send_error(
            sender_tx,
            "INVALID_RECIPIENT",
            "cannot send a message to yourself",
        );
        return;
    }

    // Friendship gate. A non-friend recipient gets a clean error
    // rather than 404'ing a row that would happen to exist — this is
    // consistent with the architecture rule that messages flow only
    // along confirmed friendships.
    let are_friends = match check_friendship(&state.db, sender_id, recipient_id).await {
        Ok(b) => b,
        Err(e) => {
            log::warn!("chat friendship check failed for {sender_id}→{recipient_id}: {e}");
            send_error(sender_tx, "INTERNAL", "could not verify friendship");
            return;
        }
    };
    if !are_friends {
        send_error(
            sender_tx,
            "NOT_FRIENDS",
            "you can only send messages to friends",
        );
        return;
    }

    let (message_id, created_at) =
        match persist_message(&state.db, sender_id, recipient_id, &content, &emoji_ids).await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("chat persist failed for {sender_id}→{recipient_id}: {e}");
                send_error(sender_tx, "INTERNAL", "could not save message");
                return;
            }
        };

    // Ack the sender. Use try_send — sender's own mpsc is the same
    // drop-on-full path as any other write.
    let _ = sender_tx.try_send(OutboundEvent::MessageReceived {
        message_id,
        timestamp: created_at,
    });

    let event = OutboundEvent::Message {
        message_id,
        from_user_id: sender_id,
        content,
        emoji_ids,
        created_at,
    };

    deliver(state, recipient_id, event).await;
}

/// Three-way delivery dispatch — see module doc.
async fn deliver(state: &AppState, recipient_id: Uuid, event: OutboundEvent) {
    let local_session_count = state.session_registry.local_session_count(recipient_id);
    if local_session_count > 0 {
        // Local fan-out for every session of this user on this
        // instance (multi-tab support is just "Vec<Sender>" → all
        // get a clone).
        state.session_registry.send(recipient_id, &event);
    }

    // Decide the global-online state via the authoritative Set, NOT
    // the local registry — recipient could be on another instance.
    let globally_online = match presence::is_online(&state.redis, recipient_id).await {
        Ok(b) => b,
        Err(e) => {
            // If Redis is briefly unreachable: trust the local
            // delivery if it happened, otherwise treat as offline so
            // the message isn't silently dropped.
            log::warn!("chat is_online check failed for {recipient_id}: {e}");
            local_session_count > 0
        }
    };

    if globally_online {
        // Publish even when local delivery already happened — the
        // recipient may have additional tabs on other instances. Self-
        // suppression in the subscriber prevents double-delivery on
        // the publisher's own instance.
        let envelope = ChatEnvelope {
            origin_instance_id: state.instance_id,
            target_user_id: recipient_id,
            event,
        };
        if let Err(e) = publish_chat(&state.redis, &envelope).await {
            log::warn!("chat publish failed for {recipient_id}: {e}");
        }
    } else {
        // Offline — store for next reconnect. Use RPUSH so a later
        // LPOP returns oldest-first (FIFO order).
        if let Err(e) = enqueue_offline(&state.redis, recipient_id, &event).await {
            log::warn!("chat enqueue offline failed for {recipient_id}: {e}");
        }
    }
}

async fn publish_chat(
    client: &redis::Client,
    envelope: &ChatEnvelope,
) -> redis::RedisResult<()> {
    let payload = serde_json::to_string(envelope).map_err(|e| {
        redis::RedisError::from((
            redis::ErrorKind::TypeError,
            "serialise chat envelope",
            e.to_string(),
        ))
    })?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("PUBLISH")
        .arg(CHAT_CHANNEL)
        .arg(payload)
        .query_async::<u32>(&mut conn)
        .await?;
    Ok(())
}

async fn enqueue_offline(
    client: &redis::Client,
    user_id: Uuid,
    event: &OutboundEvent,
) -> redis::RedisResult<()> {
    let payload = serde_json::to_string(event).map_err(|e| {
        redis::RedisError::from((
            redis::ErrorKind::TypeError,
            "serialise offline message",
            e.to_string(),
        ))
    })?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("RPUSH")
        .arg(offline_key(user_id))
        .arg(payload)
        .query_async::<u32>(&mut conn)
        .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────
// Offline queue drain — called from session task on connect
// ────────────────────────────────────────────────────────────────────────

/// LPOP every queued message for `user_id` and push them onto `dest`.
/// Stops at empty (LPOP returns `None`) — at which point Redis itself
/// has dropped the empty list, satisfying TODO [29]'s "推送成功后删除
/// Redis 列表" without an explicit DEL (which would be racy against a
/// concurrent RPUSH from a slow sender that still reads `is_online ==
/// false`).
///
/// Errors during drain are logged but not propagated: we'd rather the
/// user reconnect with a partial inbox than have the session task abort.
///
/// Returns `(delivered, dropped)`:
/// - `delivered` = events that landed on `dest` successfully,
/// - `dropped` = events whose payload parsed but whose mpsc try_send
///   failed (buffer full). Dropped events are *not* re-queued — the SPA
///   can fall back to message-history scroll to recover them.
///
/// The acceptance test for [29] reads: "B 离线期间收到 5 条消息，B 上线
/// 后 5 条全部到达且顺序正确" — with the 64-slot mpsc buffer, 5 events
/// fit comfortably, so `dropped` will be 0 and the order is the FIFO
/// sequence the senders RPUSHed.
pub async fn drain_offline_queue(
    client: &redis::Client,
    user_id: Uuid,
    dest: &mpsc::Sender<OutboundEvent>,
) -> (u32, u32) {
    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("chat drain conn failed for {user_id}: {e}");
            return (0, 0);
        }
    };
    let key = offline_key(user_id);
    let mut delivered: u32 = 0;
    let mut dropped: u32 = 0;
    loop {
        let payload: Option<String> = match redis::cmd("LPOP")
            .arg(&key)
            .query_async(&mut conn)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                log::warn!("chat drain LPOP failed for {user_id}: {e}");
                return (delivered, dropped);
            }
        };
        let payload = match payload {
            Some(p) => p,
            None => break,
        };
        match serde_json::from_str::<OutboundEvent>(&payload) {
            Ok(event) => {
                // try_send is the right primitive here: the session
                // task owning the receiver hasn't entered its select!
                // loop yet (drain runs synchronously before), so a
                // blocking send().await would deadlock on a full
                // buffer.
                if dest.try_send(event).is_err() {
                    dropped += 1;
                } else {
                    delivered += 1;
                }
            }
            Err(e) => {
                log::warn!("chat drain malformed offline payload for {user_id}: {e}");
            }
        }
    }
    if delivered > 0 || dropped > 0 {
        log::info!(
            "chat drain for {user_id}: delivered={delivered} dropped={dropped}"
        );
    }
    (delivered, dropped)
}

// ────────────────────────────────────────────────────────────────────────
// Persistence
// ────────────────────────────────────────────────────────────────────────

/// Insert the message row + every emoji link inside one transaction.
/// `position` on `message_emojis` tracks the emoji order within the
/// message — we use the index in `emoji_ids` so a later schema-aware
/// renderer can reconstruct the original sequence.
async fn persist_message(
    pool: &PgPool,
    from: Uuid,
    to: Uuid,
    content: &str,
    emoji_ids: &[Uuid],
) -> Result<(Uuid, DateTime<Utc>), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let row: (Uuid, NaiveDateTime) = sqlx::query_as(
        r#"
        INSERT INTO messages (from_user_id, to_user_id, content)
        VALUES ($1, $2, $3)
        RETURNING id, created_at
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(content)
    .fetch_one(&mut *tx)
    .await?;

    for (idx, emoji_id) in emoji_ids.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO message_emojis (message_id, emoji_id, position)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(row.0)
        .bind(emoji_id)
        .bind(idx as i32)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    // schema column is TIMESTAMP (no TZ); we store UTC values, so
    // and_utc() faithfully maps back to `DateTime<Utc>`.
    Ok((row.0, row.1.and_utc()))
}

/// Friendship check, ordered with `LEAST/GREATEST` against the
/// canonical `user_id_1 < user_id_2` constraint. Same SQL pattern as
/// the friends DAO; redeclared here to avoid making `handlers/friends`
/// a dependency of the WS layer.
async fn check_friendship(pool: &PgPool, a: Uuid, b: Uuid) -> Result<bool, sqlx::Error> {
    if a == b {
        return Ok(false);
    }
    sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM friends
            WHERE user_id_1 = LEAST($1::uuid, $2::uuid)
              AND user_id_2 = GREATEST($1::uuid, $2::uuid)
        )
        "#,
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
}

// ────────────────────────────────────────────────────────────────────────
// Cross-instance subscriber
// ────────────────────────────────────────────────────────────────────────

/// Long-lived subscriber driver — same shape as
/// `broadcast::run_subscriber` but on a different channel and with
/// target-tagged routing.
pub async fn run_subscriber(state: AppState) {
    log::info!("chat subscriber started for instance {}", state.instance_id);
    loop {
        match subscribe_loop(&state).await {
            Ok(()) => log::warn!("chat subscriber loop returned (no error)"),
            Err(e) => log::warn!("chat subscriber loop error: {e}"),
        }
        tokio::time::sleep(SUBSCRIBER_RECONNECT_DELAY).await;
    }
}

async fn subscribe_loop(state: &AppState) -> redis::RedisResult<()> {
    let mut pubsub = state.redis.get_async_pubsub().await?;
    pubsub.subscribe(CHAT_CHANNEL).await?;
    let mut on_message = pubsub.on_message();
    while let Some(msg) = on_message.next().await {
        let payload: String = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("chat subscriber: bad payload: {e}");
                continue;
            }
        };
        process_envelope(state, &payload);
    }
    Ok(())
}

fn process_envelope(state: &AppState, payload: &str) {
    let envelope: ChatEnvelope = match serde_json::from_str(payload) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("chat subscriber: malformed envelope: {e}");
            return;
        }
    };
    deliver_from_envelope(&state.session_registry, state.instance_id, envelope);
}

/// Pure function — given an envelope and a registry, deliver if the
/// target is local AND the envelope didn't originate on this instance.
/// Lives separate from `process_envelope` so it's testable without
/// JSON parsing or a running subscriber.
fn deliver_from_envelope(
    registry: &SessionRegistry,
    self_instance: Uuid,
    envelope: ChatEnvelope,
) {
    if envelope.origin_instance_id == self_instance {
        return;
    }
    registry.send(envelope.target_user_id, &envelope.event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_event() -> OutboundEvent {
        OutboundEvent::Message {
            message_id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            content: "hi".into(),
            emoji_ids: vec![],
            created_at: Utc::now(),
        }
    }

    // ── Constants ──

    /// The TODO and the validation gate both reference 5000. Pin so a
    /// drift here doesn't silently widen the limit in production.
    #[test]
    fn max_message_content_len_matches_todo() {
        assert_eq!(MAX_MESSAGE_CONTENT_LEN, 5000);
    }

    #[test]
    fn channel_and_key_constants_are_pinned() {
        assert_eq!(CHAT_CHANNEL, "chat_events");
        assert_eq!(OFFLINE_QUEUE_PREFIX, "offline_messages:");
    }

    /// The offline-queue key is what next-reconnect's `drain_offline_queue`
    /// LPOPs against. Pin the per-user shape so a rename here doesn't
    /// silently leave RPUSH and LPOP looking at different keys.
    #[test]
    fn offline_key_is_namespaced() {
        let id = Uuid::nil();
        assert_eq!(offline_key(id), format!("offline_messages:{id}"));
        assert!(offline_key(id).starts_with(OFFLINE_QUEUE_PREFIX));
    }

    // ── Envelope round-trip ──

    #[test]
    fn chat_envelope_round_trips() {
        let env = ChatEnvelope {
            origin_instance_id: Uuid::new_v4(),
            target_user_id: Uuid::new_v4(),
            event: fake_event(),
        };
        let s = serde_json::to_string(&env).unwrap();
        let back: ChatEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, back);
    }

    // ── deliver_from_envelope (subscriber-side core) ──

    #[tokio::test]
    async fn subscriber_skips_self_originated_envelopes() {
        let registry = SessionRegistry::new();
        let me = Uuid::new_v4();
        let self_instance = Uuid::new_v4();
        let (tx, mut rx) = mpsc::channel(8);
        registry.add(me, tx);

        let envelope = ChatEnvelope {
            origin_instance_id: self_instance, // SAME as our instance
            target_user_id: me,
            event: fake_event(),
        };
        deliver_from_envelope(&registry, self_instance, envelope);

        // Publisher already delivered locally — subscriber must NOT
        // re-deliver. mpsc must be empty.
        assert!(rx.try_recv().is_err(), "self-envelope should be suppressed");
    }

    #[tokio::test]
    async fn subscriber_delivers_remote_envelopes_to_local_target() {
        let registry = SessionRegistry::new();
        let me = Uuid::new_v4();
        let (tx, mut rx) = mpsc::channel(8);
        registry.add(me, tx);

        let envelope = ChatEnvelope {
            origin_instance_id: Uuid::new_v4(), // DIFFERENT instance
            target_user_id: me,
            event: fake_event(),
        };
        let event_clone = envelope.event.clone();
        deliver_from_envelope(&registry, Uuid::new_v4(), envelope);

        let received = rx.recv().await.expect("event arrived");
        assert_eq!(received, event_clone);
    }

    #[tokio::test]
    async fn subscriber_drops_envelope_for_unknown_local_target() {
        let registry = SessionRegistry::new();
        // No sender registered for the target.
        let envelope = ChatEnvelope {
            origin_instance_id: Uuid::new_v4(),
            target_user_id: Uuid::new_v4(),
            event: fake_event(),
        };
        // Should not panic, just no-op.
        deliver_from_envelope(&registry, Uuid::new_v4(), envelope);
    }
}
