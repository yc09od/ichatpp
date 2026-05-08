//! Cross-instance presence tracking via a Redis Set.
//!
//! Per ARCHITECTURE.md §4.5 / TODO [26], when a WS session opens we add
//! the user's id to the `online_users` Set; when it closes we remove it.
//! Other backend instances (and the friend-fan-out path landing in
//! TODO [27]) read this set to know who's currently reachable.
//!
//! ## Multi-tab caveat
//!
//! `SADD` / `SREM` on a single key means a user with two tabs who closes
//! one drops out of the set even though their other tab is still live.
//! Acceptable for the TODO [26] acceptance criterion ("登录后连接成功，
//! Redis 中可见 user_id"); we'll switch to a per-session counter (or a
//! Set of session ids) when fan-out (TODO [27]) actually depends on
//! accurate aggregate state.

#![allow(dead_code)] // first non-test consumer mounts in TODO [26];
                     // is_online() is consumed by TODOs [27]/[28]/[29].

use uuid::Uuid;

/// Redis key for the global online-user Set. Single source of truth so
/// add / remove / read paths can't drift apart.
pub(crate) const ONLINE_USERS_KEY: &str = "online_users";

/// SADD the user to the global online set. Idempotent — a re-add is a
/// no-op (Redis SADD returns 0 instead of erroring).
pub async fn mark_online(client: &redis::Client, user_id: Uuid) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("SADD")
        .arg(ONLINE_USERS_KEY)
        .arg(user_id.to_string())
        .query_async::<u32>(&mut conn)
        .await?;
    Ok(())
}

/// SREM the user. Also idempotent. Called from the session-loop's
/// cleanup path so a server-side abort still drops the user from the
/// set; the worst case (process-crash mid-loop) leaves a stale entry
/// until the user reconnects, which TODO [30]'s heartbeat reaper covers.
pub async fn mark_offline(client: &redis::Client, user_id: Uuid) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("SREM")
        .arg(ONLINE_USERS_KEY)
        .arg(user_id.to_string())
        .query_async::<u32>(&mut conn)
        .await?;
    Ok(())
}

/// SISMEMBER lookup — used by the friend-event fan-out path so a server
/// instance with no local Session for `user_id` can still tell whether
/// some *other* instance has the user connected.
pub async fn is_online(client: &redis::Client, user_id: Uuid) -> redis::RedisResult<bool> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let member: bool = redis::cmd("SISMEMBER")
        .arg(ONLINE_USERS_KEY)
        .arg(user_id.to_string())
        .query_async(&mut conn)
        .await?;
    Ok(member)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the Redis key so a future rename doesn't silently leave the
    /// add / remove / read paths reading three different keys.
    #[test]
    fn online_users_key_is_pinned() {
        assert_eq!(ONLINE_USERS_KEY, "online_users");
    }
}
