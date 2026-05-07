//! Invitation code primitives — generation, hashing, and the insert DAO.
//!
//! Per ARCHITECTURE.md §4.6 and TODO [13] the invariants are:
//!
//! - Plaintext format: `INV-{20 base62 chars}` — 24 chars total.
//! - Plaintext is returned to the admin **exactly once** (in the response of
//!   `POST /api/invitations/generate`). The DB never stores it.
//! - The DB stores only `code_hash` (SHA-256 hex, 64 chars), `code_prefix`
//!   (the first 8 plaintext chars, e.g. `INV-AbCd`, used by admins to
//!   identify a batch — never used to validate a code), `created_by`, and
//!   `expires_at`.
//!
//! This module deliberately stops at the data layer. The HTTP endpoints
//! that wrap these helpers (admin generate / list / stats / delete /
//! validate) land in TODOs [14]–[16].

// Generator + DAO are consumed by handlers landing in TODOs [14]/[16]/[17];
// silence the unused-warnings until those land.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Plaintext prefix every code carries. Must be ASCII so it survives
/// `chars().take()` cleanly when deriving `code_prefix`.
const CODE_PLAINTEXT_PREFIX: &str = "INV-";

/// Number of base62 chars after the `INV-` prefix.
const CODE_RANDOM_LEN: usize = 20;

/// How many leading plaintext chars are stored as `code_prefix` for admin
/// identification. With `INV-` (4 chars) + 4 random chars this yields the
/// `INV-AbCd` form referenced in ARCHITECTURE.md §4.6.
pub const CODE_PREFIX_LEN: usize = 8;

/// Base62 alphabet used for the random suffix. Chosen for URL-safety,
/// case-distinctness, and easy manual entry from a printed CSV.
const BASE62: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Largest byte value that's a clean multiple of 62 (rejection-sampling
/// threshold): `4 * 62 = 248`. Bytes in `[0, 248)` give a uniform
/// distribution mod 62; bytes in `[248, 256)` are resampled. This avoids
/// the modulo bias of `byte % 62`.
const REJECTION_THRESHOLD: u8 = 248;

/// Generate a fresh plaintext invitation code: `INV-{20 base62 chars}`.
///
/// Uses [`OsRng`] (the OS CSPRNG) directly via `RngCore::fill_bytes`, with
/// rejection sampling to keep the alphabet distribution uniform.
pub fn generate_code() -> String {
    let mut rng = OsRng;
    let mut out = String::with_capacity(CODE_PLAINTEXT_PREFIX.len() + CODE_RANDOM_LEN);
    out.push_str(CODE_PLAINTEXT_PREFIX);

    let mut buf = [0u8; 1];
    for _ in 0..CODE_RANDOM_LEN {
        loop {
            rng.fill_bytes(&mut buf);
            if buf[0] < REJECTION_THRESHOLD {
                break;
            }
        }
        out.push(BASE62[(buf[0] % 62) as usize] as char);
    }
    out
}

/// SHA-256 hex digest of the plaintext code: 64 lowercase hex chars,
/// matching the `CHAR(64)` shape of `invitations.code_hash`.
pub fn hash_code(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    hex::encode(digest)
}

/// First [`CODE_PREFIX_LEN`] plaintext chars, suitable for `code_prefix`.
/// Caller is responsible for not feeding in something shorter; with our
/// generator that never happens.
pub fn code_prefix(plaintext: &str) -> String {
    plaintext.chars().take(CODE_PREFIX_LEN).collect()
}

/// Insert a single invitation row.
///
/// The function takes the *hash* and *prefix*, never the plaintext —
/// keeping the secret out of this signature is the whole point. The
/// caller hashes once, sends the plaintext back to the admin, and drops
/// it.
///
/// `expires_at` is converted to `NaiveDateTime` because the column is
/// `TIMESTAMP WITHOUT TIME ZONE` (see initial migration). UTC is the
/// project-wide convention.
pub async fn insert_invitation(
    pool: &PgPool,
    code_hash: &str,
    code_prefix: &str,
    created_by: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO invitations (code_hash, code_prefix, created_by, expires_at)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
    )
    .bind(code_hash)
    .bind(code_prefix)
    .bind(created_by)
    .bind(expires_at.naive_utc())
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_code_has_expected_shape() {
        let code = generate_code();
        assert!(code.starts_with("INV-"), "missing INV- prefix: {code}");
        assert_eq!(
            code.len(),
            CODE_PLAINTEXT_PREFIX.len() + CODE_RANDOM_LEN,
            "unexpected length: {code}"
        );
        // Every suffix char must be base62.
        let suffix = &code[CODE_PLAINTEXT_PREFIX.len()..];
        for c in suffix.chars() {
            assert!(
                BASE62.contains(&(c as u8)),
                "non-base62 char {c:?} in {code}"
            );
        }
    }

    /// 100 generations, no duplicates. (With a 62^20 search space the odds
    /// of a collision in 100 draws are astronomically small; this asserts
    /// the generator isn't accidentally reusing seeds.)
    #[test]
    fn one_hundred_generations_are_unique() {
        let mut seen = HashSet::with_capacity(100);
        for _ in 0..100 {
            let code = generate_code();
            assert!(seen.insert(code.clone()), "duplicate code generated: {code}");
        }
        assert_eq!(seen.len(), 100);
    }

    #[test]
    fn hash_is_64_lowercase_hex_chars() {
        let code = generate_code();
        let hash = hash_code(&code);
        assert_eq!(hash.len(), 64, "hash length not 64: {hash}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "hash not lowercase hex: {hash}"
        );
    }

    /// Determinism is what makes `code_hash` a usable lookup key in the
    /// invitations table — same plaintext → same hash, every time. This
    /// is the unit-test surface of the "查询时按 hash 命中" acceptance
    /// criterion (the matching DB roundtrip lands with the integration
    /// tests in TODO [47]).
    #[test]
    fn hash_lookup_key_is_deterministic() {
        let code = generate_code();
        assert_eq!(hash_code(&code), hash_code(&code));
        // And distinct plaintext yields a distinct key.
        let other = generate_code();
        assert_ne!(hash_code(&code), hash_code(&other));
    }

    #[test]
    fn hash_matches_known_vector() {
        // SHA-256("abc") — the canonical FIPS 180-2 test vector. Pins our
        // hex encoding to lowercase and confirms we're hashing bytes, not
        // some derived form.
        assert_eq!(
            hash_code("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn code_prefix_is_first_eight_chars() {
        let code = generate_code();
        let prefix = code_prefix(&code);
        assert_eq!(prefix.len(), CODE_PREFIX_LEN);
        assert!(prefix.starts_with("INV-"));
        assert!(code.starts_with(&prefix));
    }
}
