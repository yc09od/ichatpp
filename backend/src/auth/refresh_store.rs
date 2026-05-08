//! Server-side refresh-token whitelist (Redis).
//!
//! Per ARCHITECTURE.md §4.1, refresh tokens are tracked in Redis so the
//! server can revoke individual sessions ahead of natural JWT expiry. We
//! never store the JWT itself — only its SHA-256 — so a Redis dump cannot
//! be replayed as live credentials.
//!
//! ## Schema
//!
//! ```text
//! key:    refresh_tokens:{user_id}  (Redis SET)
//! member: hex(sha256(jwt))          one entry per active session
//! ttl:    refreshed to refresh_ttl_seconds on every SADD
//! ```
//!
//! ## Why per-user, not per-token?
//!
//! `/api/auth/logout` only sees the **access** cookie — the refresh cookie
//! is path-scoped to `/api/auth/refresh` (see ARCHITECTURE.md §4.1) and
//! the browser will not include it on `/api/auth/logout`. Keying by
//! `user_id` lets the logout handler identify the user from the access
//! token and drop every active refresh token for that account in one call,
//! which matches the "logout everywhere" semantics most users expect.
//!
//! ## Why re-EXPIRE on every SADD?
//!
//! Redis SETs do not refresh their TTL when a member is added. Without
//! re-applying EXPIRE, a second login a few days after the first would
//! inherit the *remaining* TTL of the existing key — so the new session
//! would silently expire too early. Pinning TTL = `refresh_ttl_seconds`
//! on every add keeps each new session at its full lifetime.

#![allow(dead_code)] // consumed by auth handlers landing in TODO [19]

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Single source of truth for the Redis key prefix. The read/write/delete
/// paths all derive their key from [`whitelist_key`] so a rename here
/// cannot leave any path looking at the old shape.
const REFRESH_KEY_PREFIX: &str = "refresh_tokens:";

fn whitelist_key(user_id: Uuid) -> String {
    format!("{REFRESH_KEY_PREFIX}{user_id}")
}

/// SHA-256 of the JWT, hex-encoded. We hash so a Redis dump never carries
/// usable token material; the hash space (256 bits) is large enough that
/// SISMEMBER can serve as a strict equality check without ambiguity.
fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Add `token` to `user_id`'s active-session set. Login and register call
/// this immediately after signing a fresh refresh JWT, before the
/// `Set-Cookie` is dispatched, so the whitelist is always populated before
/// the client could possibly use the token.
pub async fn add_refresh_token(
    client: &redis::Client,
    user_id: Uuid,
    token: &str,
    ttl_seconds: u64,
) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = whitelist_key(user_id);
    redis::cmd("SADD")
        .arg(&key)
        .arg(token_hash(token))
        .query_async::<u32>(&mut conn)
        .await?;
    // Always re-apply TTL — see module docs for the rationale.
    redis::cmd("EXPIRE")
        .arg(&key)
        .arg(ttl_seconds)
        .query_async::<()>(&mut conn)
        .await?;
    Ok(())
}

/// True iff `token` is currently in `user_id`'s active-session set.
///
/// `false` covers both "never issued by us" and "explicitly revoked by
/// /api/auth/logout" — the refresh handler treats both as equivalent.
pub async fn is_refresh_token_active(
    client: &redis::Client,
    user_id: Uuid,
    token: &str,
) -> redis::RedisResult<bool> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let active: bool = redis::cmd("SISMEMBER")
        .arg(whitelist_key(user_id))
        .arg(token_hash(token))
        .query_async(&mut conn)
        .await?;
    Ok(active)
}

/// Drop every refresh token for `user_id`. Used by `/api/auth/logout` to
/// implement the "logout everywhere" semantics described above.
///
/// Idempotent: a missing key is a no-op (Redis DEL returns 0). Callers
/// should treat this as best-effort and proceed with cookie clearing
/// regardless.
pub async fn revoke_all_refresh_tokens(
    client: &redis::Client,
    user_id: Uuid,
) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("DEL")
        .arg(whitelist_key(user_id))
        .query_async::<()>(&mut conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the Redis key shape so an external dashboard scraping this
    /// namespace doesn't break on a rename — and so the read/write/delete
    /// paths can't silently diverge.
    #[test]
    fn whitelist_key_is_namespaced() {
        let id = Uuid::nil();
        assert_eq!(whitelist_key(id), format!("refresh_tokens:{id}"));
        assert!(whitelist_key(id).starts_with(REFRESH_KEY_PREFIX));
    }

    #[test]
    fn token_hash_is_64_hex_chars() {
        // SHA-256 → 32 bytes → 64 hex chars. Pin the shape so a switch to
        // a different digest (or a non-hex encoding) would fail loud.
        let h = token_hash("some.jwt.token");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_hash_is_deterministic_and_input_sensitive() {
        // Same input → same hash (whitelist lookup depends on this);
        // different input → different hash (otherwise SISMEMBER false
        // positives would let revoked tokens slip through).
        assert_eq!(token_hash("abc"), token_hash("abc"));
        assert_ne!(token_hash("abc"), token_hash("abd"));
    }
}
