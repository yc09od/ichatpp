//! In-process registry of live WebSocket sessions.
//!
//! Each accepted upgrade creates an mpsc channel; the **sender** is
//! handed to this registry (keyed by user id) so any other task on the
//! same instance can push events to the user without knowing which
//! session task owns the actual socket. The session task itself reads
//! from the **receiver** in its main `select!` loop and writes the
//! event out to the WS frame.
//!
//! `Vec<Sender>` per user supports multiple simultaneous sessions for
//! the same user (e.g. two browser tabs). All of them get a copy of any
//! event addressed to that user.
//!
//! ## Drop-on-full backpressure
//!
//! Sends use `try_send`, not `send`. A slow consumer with a full
//! 64-slot buffer simply misses the event — that is the right
//! behaviour for presence notifications because:
//!
//! - The SPA refreshes friend-online state on (re)connect anyway.
//! - Blocking the broadcaster on a single slow client would stall
//!   *every* friend's notification.

#![allow(dead_code)] // SessionRegistry::new + len mounts in TODO [27];
                     // accessor count() is also consumed by integration
                     // tests once the WS test harness lands.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ws::event::OutboundEvent;

/// Per-session outbound buffer. 64 slots gives headroom for a burst of
/// presence + chat events without blocking the broadcaster, but is
/// small enough that a stalled client never piles up megabytes.
pub const SESSION_CHANNEL_CAPACITY: usize = 64;

/// Cheap-to-clone handle to the process-wide session registry. The
/// shared `Arc<DashMap<…>>` means every clone reads/writes the same
/// underlying state.
#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    inner: Arc<DashMap<Uuid, Vec<mpsc::Sender<OutboundEvent>>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `sender` as one of `user_id`'s active session writers.
    /// The session task should call this *after* it is ready to read
    /// from the receiver side — events sent before then would land in
    /// the buffer and only be drained on the first `select!` tick.
    pub fn add(&self, user_id: Uuid, sender: mpsc::Sender<OutboundEvent>) {
        self.inner.entry(user_id).or_default().push(sender);
    }

    /// Remove a specific `sender` from the user's list. Identifies the
    /// sender by `same_channel` rather than equality — `mpsc::Sender`
    /// is `Clone` and there's no `PartialEq`, so two clones of the same
    /// sender compare via `same_channel` (they share the inner channel).
    /// If the user has no remaining senders, the bucket is dropped.
    pub fn remove(&self, user_id: Uuid, sender: &mpsc::Sender<OutboundEvent>) {
        // `get_mut` holds a shard lock; release it before calling
        // `remove` to avoid a self-deadlock.
        let bucket_empty = {
            match self.inner.get_mut(&user_id) {
                Some(mut entry) => {
                    entry.retain(|s| !s.same_channel(sender));
                    entry.is_empty()
                }
                None => false,
            }
        };
        if bucket_empty {
            self.inner.remove(&user_id);
        }
    }

    /// Best-effort fan-out to every live session of `user_id`. Slow
    /// consumers (full buffer) silently miss the event — see module
    /// docs on the drop-on-full policy.
    pub fn send(&self, user_id: Uuid, event: &OutboundEvent) {
        if let Some(senders) = self.inner.get(&user_id) {
            for sender in senders.iter() {
                let _ = sender.try_send(event.clone());
            }
        }
    }

    /// Number of *senders* (not users) the registry currently holds.
    /// One user with two tabs counts as 2.
    pub fn total_senders(&self) -> usize {
        self.inner.iter().map(|entry| entry.value().len()).sum()
    }

    pub fn local_session_count(&self, user_id: Uuid) -> usize {
        self.inner.get(&user_id).map(|v| v.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> (mpsc::Sender<OutboundEvent>, mpsc::Receiver<OutboundEvent>) {
        mpsc::channel(SESSION_CHANNEL_CAPACITY)
    }

    #[tokio::test]
    async fn add_then_send_delivers_event() {
        let registry = SessionRegistry::new();
        let user = Uuid::new_v4();
        let (tx, mut rx) = make_channel();
        registry.add(user, tx);

        let event = OutboundEvent::UserOnline { user_id: Uuid::new_v4() };
        registry.send(user, &event);

        let received = rx.recv().await.expect("event arrived");
        assert_eq!(received, event);
    }

    /// Multi-tab simulation — two senders for the same user must both
    /// receive a copy. This is the property that makes the registry
    /// useful on the second-tab scenario.
    #[tokio::test]
    async fn send_fans_out_to_all_senders_for_user() {
        let registry = SessionRegistry::new();
        let user = Uuid::new_v4();
        let (tx1, mut rx1) = make_channel();
        let (tx2, mut rx2) = make_channel();
        registry.add(user, tx1);
        registry.add(user, tx2);

        let event = OutboundEvent::UserOnline { user_id: Uuid::new_v4() };
        registry.send(user, &event);

        assert_eq!(rx1.recv().await, Some(event.clone()));
        assert_eq!(rx2.recv().await, Some(event));
    }

    #[tokio::test]
    async fn remove_only_drops_the_targeted_sender() {
        let registry = SessionRegistry::new();
        let user = Uuid::new_v4();
        let (tx1, mut rx1) = make_channel();
        let (tx2, mut rx2) = make_channel();
        registry.add(user, tx1.clone());
        registry.add(user, tx2);

        registry.remove(user, &tx1);

        let event = OutboundEvent::UserOffline { user_id: Uuid::new_v4() };
        registry.send(user, &event);

        // tx1 was unregistered — nothing arrives.
        assert!(rx1.try_recv().is_err());
        // tx2 still gets the event.
        assert_eq!(rx2.recv().await, Some(event));
    }

    #[tokio::test]
    async fn remove_drops_user_bucket_when_empty() {
        let registry = SessionRegistry::new();
        let user = Uuid::new_v4();
        let (tx, _rx) = make_channel();
        registry.add(user, tx.clone());
        assert_eq!(registry.local_session_count(user), 1);

        registry.remove(user, &tx);
        assert_eq!(registry.local_session_count(user), 0);
        // After a complete clear, total_senders should reflect the
        // empty registry — pin so future code that "leaves the bucket
        // around to avoid HashMap churn" must update this expectation.
        assert_eq!(registry.total_senders(), 0);
    }

    /// Sending to a user with no live sessions is a no-op (not a panic
    /// and not an error). This is the property that lets the broadcast
    /// path call `send(friend_id, &event)` without first checking
    /// `local_session_count`.
    #[tokio::test]
    async fn send_to_unknown_user_is_a_noop() {
        let registry = SessionRegistry::new();
        registry.send(
            Uuid::new_v4(),
            &OutboundEvent::UserOnline { user_id: Uuid::new_v4() },
        );
    }

    /// Pin the buffer size — it directly affects how lossy presence
    /// fan-out is on a slow tab. A bump to 256 isn't wrong but should
    /// be a conscious decision.
    #[test]
    fn channel_capacity_is_pinned() {
        assert_eq!(SESSION_CHANNEL_CAPACITY, 64);
    }
}
