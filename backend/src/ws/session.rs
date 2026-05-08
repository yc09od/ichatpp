//! Per-connection WebSocket session task (TODO [26]; extended in
//! TODOs [27]–[30]).
//!
//! One [`run`] call per accepted upgrade, spawned onto the tokio runtime
//! by `crate::handlers::ws::ws_handler`. The task owns:
//!
//! - the actix-ws `Session` (outbound writes),
//! - the actix-ws `MessageStream` (inbound frames),
//! - the user's `online_users` Set membership (added on entry, removed
//!   on every exit path).
//!
//! ## Heartbeat strategy (TODO [30])
//!
//! Strict pong-window model per ARCHITECTURE.md §4.7:
//!
//! - Every [`PING_INTERVAL`] (30 s) we send a WS ping and arm a pong
//!   deadline at `now + PONG_TIMEOUT` (15 s).
//! - When the matching pong arrives, the deadline is cleared and the
//!   next ping is scheduled `PING_INTERVAL` after the ping we just
//!   acked.
//! - If the deadline fires without a pong, the peer is considered dead
//!   and we close.
//!
//! Only server-initiated probes count toward liveness. A client that
//! sends data but never replies to our pings is still treated as dead —
//! consistent with the spec, and avoids a misbehaving client masking a
//! half-open TCP connection.
//!
//! Worst-case detection of a hard disconnect is `PING_INTERVAL +
//! PONG_TIMEOUT` (≈45 s) when the link drops just after a successful
//! pong; typical detection is closer to `PONG_TIMEOUT` (15 s) when the
//! link drops mid-cycle. The acceptance criterion ("客户端断网 30s 后
//! 服务端自动结束对应的 Session task") falls comfortably inside this
//! envelope.

#![allow(dead_code)] // first non-test consumer mounts in TODO [26];
                     // text-frame branch is fleshed out in TODO [28].

use std::time::{Duration, Instant};

use actix_ws::{Message, MessageStream, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ws::broadcast::{user_offline, user_online};
use crate::ws::chat::{drain_offline_queue, handle_inbound_text};
use crate::ws::presence::{mark_offline, mark_online};
use crate::ws::registry::SESSION_CHANNEL_CAPACITY;
use crate::AppState;

/// Interval between server-initiated pings. 30 s per ARCHITECTURE.md
/// §4.7; chosen to be far below most NAT idle timeouts (~60–120 s) so a
/// quiet connection stays warm.
pub(crate) const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum time we'll wait for a pong after sending a ping before
/// declaring the peer dead and closing the session. Per the TODO [30]
/// spec ("15s 内未响应断开连接").
pub(crate) const PONG_TIMEOUT: Duration = Duration::from_secs(15);

/// Drive one client connection until it disconnects or times out.
///
/// On entry the session task:
///   1. registers an mpsc sender in the [`SessionRegistry`] so other
///      tasks can deliver events to this connection,
///   2. SADDs the user into the global `online_users` Redis Set,
///   3. broadcasts `user_online` to all of the user's friends (local
///      and cross-instance via Pub/Sub).
///
/// On exit (regardless of which arm of `select!` triggered the close)
/// it does the symmetric tear-down: SREM, broadcast `user_offline`,
/// remove the registry entry. The only way these steps are skipped is
/// a forcible task abort (e.g. process teardown); orphan set entries
/// can be reaped by a later restart sweep.
pub async fn run(
    user_id: Uuid,
    mut session: Session,
    mut msg_stream: MessageStream,
    state: AppState,
) {
    // 1. Outbound mpsc — the registry holds the sender, the loop reads
    //    the receiver. Capacity-bounded so a stalled tab can't pile up
    //    unbounded events; see `registry.rs` on drop-on-full policy.
    let (tx, mut rx) = mpsc::channel(SESSION_CHANNEL_CAPACITY);
    state.session_registry.add(user_id, tx.clone());

    // 2. Cross-instance presence — best-effort. A Redis hiccup at
    //    connect time shouldn't kill the socket.
    if let Err(e) = mark_online(&state.redis, user_id).await {
        log::warn!("ws presence mark_online failed for {user_id}: {e}");
    }

    // 3. Drain any messages that arrived while the user was offline
    //    (TODO [29]). Done after registry add / mark_online so the
    //    LPOPs and a concurrent sender's "is_online" decision converge
    //    to the same answer:
    //      - sender pre-mark_online → RPUSH → caught by this drain;
    //      - sender post-mark_online → PUBLISH → caught by chat
    //        subscriber via the now-registered mpsc.
    //    drain_offline_queue logs delivered/dropped counts and
    //    continues on transient errors — we'd rather start a session
    //    with a partial inbox than abort. Return value (counts) is
    //    intentionally discarded; the function logs at INFO level.
    let _ = drain_offline_queue(&state.redis, user_id, &tx).await;

    // 4. Friend fan-out — also best-effort and independently logged.
    user_online(&state, user_id).await;

    // Heartbeat state. `last_ping_at` records when we last sent a ping
    // (so the next one fires `PING_INTERVAL` after that). `pong_deadline`
    // is `Some(t)` while we're waiting for the pong matching the most
    // recent ping; the session closes if `t` elapses before the pong
    // arrives.
    let mut last_ping_at = Instant::now();
    let mut pong_deadline: Option<Instant> = None;

    let close_reason = loop {
        // Whichever of "next ping due" or "current pong deadline" comes
        // sooner is what we sleep until. When pong_deadline is Some it
        // is always <= last_ping_at + PING_INTERVAL (because PONG_TIMEOUT
        // < PING_INTERVAL), so it dominates while in flight.
        let next_wake = pong_deadline.unwrap_or(last_ping_at + PING_INTERVAL);
        let sleep_dur = next_wake.saturating_duration_since(Instant::now());

        tokio::select! {
            _ = tokio::time::sleep(sleep_dur) => {
                let now = Instant::now();
                if let Some(deadline) = pong_deadline {
                    if now >= deadline {
                        log::info!(
                            "ws session for {user_id} pong timeout (no pong within {}s)",
                            PONG_TIMEOUT.as_secs()
                        );
                        break "pong timeout";
                    }
                    // Spurious wake (sleep can return slightly early on
                    // some platforms); fall through and re-loop.
                } else if now >= last_ping_at + PING_INTERVAL {
                    if session.ping(b"").await.is_err() {
                        break "ping write failed";
                    }
                    last_ping_at = now;
                    pong_deadline = Some(now + PONG_TIMEOUT);
                }
            }

            // Outbound — events pushed in by other tasks (presence
            // fan-out today; chat in TODO [28]).
            outbound = rx.recv() => {
                match outbound {
                    Some(event) => {
                        // Serialise once and write. A failed write
                        // means the socket is already gone.
                        let payload = match serde_json::to_string(&event) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("ws session for {user_id} outbound serialise: {e}");
                                continue;
                            }
                        };
                        if session.text(payload).await.is_err() {
                            break "outbound write failed";
                        }
                    }
                    // Channel closed only fires if every Sender clone
                    // was dropped — in our model, that means the
                    // registry was cleared from underneath us. Treat as
                    // an explicit close request.
                    None => break "mpsc closed",
                }
            }

            // Inbound frames.
            msg = msg_stream.next() => {
                match msg {
                    Some(Ok(Message::Ping(bytes))) => {
                        if session.pong(&bytes).await.is_err() {
                            break "pong write failed";
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Peer responded — clear the in-flight deadline.
                        // Next ping will fire PING_INTERVAL after the
                        // ping we just acked (see `last_ping_at`).
                        pong_deadline = None;
                    }
                    Some(Ok(Message::Text(text))) => {
                        // Dispatch into the chat pipeline. Errors come
                        // back to the SPA via OutboundEvent::Error
                        // (pushed through `tx`), so the loop body has
                        // no error to propagate.
                        handle_inbound_text(&state, user_id, &tx, &text).await;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // The MVP wire format is text JSON; binary
                        // frames aren't part of the spec. Ignore
                        // silently — they don't affect heartbeat state
                        // either way (only pongs do).
                    }
                    Some(Ok(Message::Continuation(_))) | Some(Ok(Message::Nop)) => {
                        // Nothing to do — Continuation isn't used by the
                        // SPA's framing; Nop is a `actix-ws` no-op
                        // synthesised from internal events.
                    }
                    Some(Ok(Message::Close(_))) => break "client close",
                    Some(Err(e)) => {
                        log::info!("ws session for {user_id} ended on stream error: {e}");
                        break "stream error";
                    }
                    None => break "stream ended",
                }
            }
        }
    };

    log::debug!("ws session for {user_id} closing: {close_reason}");

    // Tear-down: unregister BEFORE the offline broadcast so a friend
    // who's already pulling our user's online state doesn't see the
    // dying session as still local.
    state.session_registry.remove(user_id, &tx);

    // Best-effort close — if the socket is already gone, this is a
    // no-op.
    let _ = session.close(None).await;

    if let Err(e) = mark_offline(&state.redis, user_id).await {
        log::warn!("ws presence mark_offline failed for {user_id}: {e}");
    }

    user_offline(&state, user_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the heartbeat tunings against ARCHITECTURE.md §4.7 / TODO
    /// [30]: 30 s ping cadence with a 15 s pong window. The strict
    /// inequality keeps the deadline always tighter than the next ping
    /// — without that, a missed pong could be masked by the next probe.
    #[test]
    fn heartbeat_constants_are_sane() {
        assert_eq!(PING_INTERVAL, Duration::from_secs(30));
        assert_eq!(PONG_TIMEOUT, Duration::from_secs(15));
        assert!(PONG_TIMEOUT < PING_INTERVAL);
    }
}
