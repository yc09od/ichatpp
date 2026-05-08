//! Fan-out for presence events (TODO [27]).
//!
//! Two delivery paths cooperate:
//!
//! 1. **Local fan-out via [`SessionRegistry`]** — the originator's
//!    instance walks the friend list, looks up each friend in the
//!    in-process registry, and `try_send`s the event into every live
//!    session for that friend.
//!
//! 2. **Cross-instance fan-out via Redis Pub/Sub** — the originator
//!    publishes a [`PresenceEnvelope`] tagged with its `instance_id`.
//!    Other instances subscribe to [`PRESENCE_CHANNEL`], skip messages
//!    they themselves originated, and run their own local fan-out
//!    against the originator's friend list (which they re-query from
//!    the DB so a stale list doesn't stick around).
//!
//! ## Why originator-tagged messages instead of per-target events?
//!
//! Per-target Pub/Sub messages would push N messages onto Redis (one
//! per friend) for a single online event. Originator-tagged with
//! receiver-side fan-out is O(1) publish + one friend-list query per
//! receiving instance — same wire cost on a small cluster, vastly
//! smaller cost as friend lists grow.
//!
//! ## Self-suppression
//!
//! The originator's instance also receives its own publish (Redis
//! Pub/Sub broadcasts to all subscribers, including the publisher).
//! The `origin_instance_id` check skips those so a friend on the
//! originator's instance does not see two copies of `user_online`.

#![allow(dead_code)] // First non-test consumer mounts in TODO [27];
                     // run_subscriber is started from main.rs.

use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::ws::event::OutboundEvent;
use crate::ws::registry::SessionRegistry;
use crate::AppState;

/// Redis Pub/Sub channel for presence events. Single source of truth
/// so the publish path and the subscriber both reference the same
/// constant.
pub(crate) const PRESENCE_CHANNEL: &str = "presence_events";

/// How long to back off after the subscriber's connection drops before
/// reconnecting. Long enough that a Redis blip doesn't tight-loop;
/// short enough that the cluster catches up quickly after recovery.
const SUBSCRIBER_RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Wire envelope for a Redis Pub/Sub message. Carries the originator's
/// instance id so the publisher's own subscriber can filter the
/// message back out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresenceEnvelope {
    /// The instance that originated the event. Subscribers compare
    /// against their own `state.instance_id` and skip self-originated
    /// messages.
    pub origin_instance_id: Uuid,
    /// The user whose online state changed. Receiver-side fan-out
    /// re-queries this user's friends.
    pub origin_user_id: Uuid,
    /// The actual event the SPA will see (a re-encoded copy of this
    /// envelope's `event` is what gets written to each friend's WS).
    pub event: OutboundEvent,
}

// ────────────────────────────────────────────────────────────────────────
// Public entrypoints called from the session task
// ────────────────────────────────────────────────────────────────────────

/// User just connected — broadcast `user_online` to all of their
/// friends across every instance.
pub async fn user_online(state: &AppState, user_id: Uuid) {
    fan_out(state, user_id, OutboundEvent::UserOnline { user_id }).await;
}

/// User just disconnected — broadcast `user_offline` similarly.
pub async fn user_offline(state: &AppState, user_id: Uuid) {
    fan_out(state, user_id, OutboundEvent::UserOffline { user_id }).await;
}

async fn fan_out(state: &AppState, originator: Uuid, event: OutboundEvent) {
    // 1. Friend list query — one call per event. Bail loud-but-safe on
    //    failure: we'd rather miss a single presence event than poison
    //    the session task with an error that propagates upward.
    let friends = match get_friend_ids(&state.db, originator).await {
        Ok(f) => f,
        Err(e) => {
            log::warn!("presence fan_out friend query failed for {originator}: {e}");
            return;
        }
    };

    // 2. Local fan-out — fast, no Redis hop. Friends with no live
    //    session locally are silently skipped by `send()`.
    for friend_id in &friends {
        state.session_registry.send(*friend_id, &event);
    }

    // 3. Cross-instance fan-out via Pub/Sub. Tagged with our instance
    //    id so our own subscriber suppresses the echo.
    let envelope = PresenceEnvelope {
        origin_instance_id: state.instance_id,
        origin_user_id: originator,
        event,
    };
    if let Err(e) = publish_presence(&state.redis, &envelope).await {
        log::warn!("presence publish failed for {originator}: {e}");
    }
}

async fn publish_presence(
    client: &redis::Client,
    envelope: &PresenceEnvelope,
) -> redis::RedisResult<()> {
    let payload = serde_json::to_string(envelope).map_err(|e| {
        redis::RedisError::from((
            redis::ErrorKind::TypeError,
            "serialise presence envelope",
            e.to_string(),
        ))
    })?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("PUBLISH")
        .arg(PRESENCE_CHANNEL)
        .arg(payload)
        .query_async::<u32>(&mut conn)
        .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────
// Subscriber — one task per server, started at boot
// ────────────────────────────────────────────────────────────────────────

/// Background task driver. Spawned once at startup; keeps a Pub/Sub
/// connection open and re-subscribes on connection drops with a fixed
/// back-off. Lives for the lifetime of the process.
pub async fn run_subscriber(state: AppState) {
    log::info!("presence subscriber started for instance {}", state.instance_id);
    loop {
        match subscribe_loop(&state).await {
            Ok(()) => log::warn!("presence subscriber loop returned (no error)"),
            Err(e) => log::warn!("presence subscriber loop error: {e}"),
        }
        tokio::time::sleep(SUBSCRIBER_RECONNECT_DELAY).await;
    }
}

async fn subscribe_loop(state: &AppState) -> redis::RedisResult<()> {
    let mut pubsub = state.redis.get_async_pubsub().await?;
    pubsub.subscribe(PRESENCE_CHANNEL).await?;
    let mut on_message = pubsub.on_message();
    while let Some(msg) = on_message.next().await {
        let payload: String = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("presence: bad payload (not a string): {e}");
                continue;
            }
        };
        process_envelope(state, &payload).await;
    }
    Ok(())
}

async fn process_envelope(state: &AppState, payload: &str) {
    let envelope: PresenceEnvelope = match serde_json::from_str(payload) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("presence: malformed envelope: {e}");
            return;
        }
    };

    // Self-suppression — see module docs.
    if envelope.origin_instance_id == state.instance_id {
        return;
    }

    let friends = match get_friend_ids(&state.db, envelope.origin_user_id).await {
        Ok(f) => f,
        Err(e) => {
            log::warn!(
                "presence subscriber friend query failed for {}: {e}",
                envelope.origin_user_id
            );
            return;
        }
    };

    deliver_to_friends(&state.session_registry, &friends, &envelope.event);
}

/// Pure function — given a friend list and an event, fan out via the
/// registry. Extracted so the subscriber loop can be tested without a
/// running Redis or DB.
fn deliver_to_friends(
    registry: &SessionRegistry,
    friends: &[Uuid],
    event: &OutboundEvent,
) {
    for friend_id in friends {
        registry.send(*friend_id, event);
    }
}

// ────────────────────────────────────────────────────────────────────────
// Friend-list query
// ────────────────────────────────────────────────────────────────────────

/// Returns every user_id `me` is friends with. Mirrors the CASE
/// pattern used in `handlers::friends::list_friends_handler` so a
/// schema change to the `friends` table need only update one place
/// (the SQL is identical).
async fn get_friend_ids(pool: &PgPool, me: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT
            CASE WHEN user_id_1 = $1 THEN user_id_2 ELSE user_id_1 END
        FROM friends
        WHERE user_id_1 = $1 OR user_id_2 = $1
        "#,
    )
    .bind(me)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Pin the channel name so a rename in this module can't silently
    /// leave publishers and subscribers talking past each other.
    #[test]
    fn presence_channel_name_is_pinned() {
        assert_eq!(PRESENCE_CHANNEL, "presence_events");
    }

    /// Round-trip the envelope through JSON. The Pub/Sub wire format
    /// IS this JSON — a refactor that breaks (de)serialisation would
    /// silently break cross-instance fan-out.
    #[test]
    fn envelope_round_trips_through_json() {
        let env = PresenceEnvelope {
            origin_instance_id: Uuid::new_v4(),
            origin_user_id: Uuid::new_v4(),
            event: OutboundEvent::UserOnline {
                user_id: Uuid::new_v4(),
            },
        };
        let s = serde_json::to_string(&env).unwrap();
        let back: PresenceEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, back);
    }

    /// `deliver_to_friends` is the pure core of the subscriber's
    /// fan-out — verify it actually writes the event to each
    /// registered friend's session.
    #[tokio::test]
    async fn deliver_to_friends_walks_all_targets() {
        let registry = SessionRegistry::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let (tx_a, mut rx_a) = mpsc::channel(8);
        let (tx_b, mut rx_b) = mpsc::channel(8);
        registry.add(alice, tx_a);
        registry.add(bob, tx_b);

        let event = OutboundEvent::UserOnline {
            user_id: Uuid::new_v4(),
        };
        deliver_to_friends(&registry, &[alice, bob], &event);

        assert_eq!(rx_a.recv().await, Some(event.clone()));
        assert_eq!(rx_b.recv().await, Some(event));
    }

    /// Friends not in the local registry are silently skipped — the
    /// property that lets cross-instance fan-out be naive about which
    /// instance hosts each friend.
    #[tokio::test]
    async fn deliver_to_friends_skips_offline_friends() {
        let registry = SessionRegistry::new();
        let online = Uuid::new_v4();
        let offline = Uuid::new_v4();
        let (tx, mut rx) = mpsc::channel(8);
        registry.add(online, tx);

        let event = OutboundEvent::UserOffline {
            user_id: Uuid::new_v4(),
        };
        deliver_to_friends(&registry, &[online, offline], &event);

        assert_eq!(rx.recv().await, Some(event));
    }
}
