//! JWT signing & verification with RS256.
//!
//! Per ARCHITECTURE.md §4.1:
//! - Asymmetric signing (RS256) — the private key never leaves the issuer.
//! - Two token types: `access` (1h, carries role for authz) and `refresh`
//!   (7d, only `sub`; the refresh handler does authz lookup separately).
//! - The `typ` claim is non-standard but lets us reject access ↔ refresh
//!   substitution at verification time without a separate KID rotation.
//!
//! Keys are loaded once at startup via [`Keys::from_config`] and shared
//! across workers in `AppState` (cheap `Clone` — both halves are `Arc`-like
//! internally in `jsonwebtoken`).

#![allow(dead_code)] // consumed by handlers landing in TODOs [17]/[18]/[19]

use std::fs;

use chrono::Utc;
use jsonwebtoken::{
    decode, encode, errors::ErrorKind, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;

/// Token kind, encoded into the `typ` claim. Refusing to verify an access
/// token where a refresh was expected (and vice versa) closes a class of
/// confused-deputy bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenType {
    Access,
    Refresh,
}

/// All claims we put in either token. `role` only carries a value for access
/// tokens — it's `None` for refresh tokens (and serialized as absent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the user UUID.
    pub sub: Uuid,
    /// Token kind (access | refresh).
    #[serde(rename = "typ")]
    pub token_type: TokenType,
    /// User role; only present on access tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Issued-at, seconds since Unix epoch.
    pub iat: i64,
    /// Expiry, seconds since Unix epoch. `jsonwebtoken` enforces this.
    pub exp: i64,
}

/// Loaded RS256 key material. Cheap to clone: `EncodingKey` /
/// `DecodingKey` are reference-counted internally.
#[derive(Clone)]
pub struct Keys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    access_ttl: i64,
    refresh_ttl: i64,
}

impl Keys {
    /// Load private + public key PEM files referenced by the config.
    /// Fails fast on disk / parse errors; the server cannot start without
    /// valid keys.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let private_pem = fs::read(&cfg.jwt_private_key_path).map_err(|e| {
            anyhow::anyhow!(
                "JWT_PRIVATE_KEY_PATH ({}) not readable: {e}",
                cfg.jwt_private_key_path
            )
        })?;
        let public_pem = fs::read(&cfg.jwt_public_key_path).map_err(|e| {
            anyhow::anyhow!(
                "JWT_PUBLIC_KEY_PATH ({}) not readable: {e}",
                cfg.jwt_public_key_path
            )
        })?;

        Self::from_pem(
            &private_pem,
            &public_pem,
            cfg.jwt_access_ttl_seconds,
            cfg.jwt_refresh_ttl_seconds,
        )
    }

    /// Construct from raw PEM bytes — useful for tests with embedded keys
    /// and for callers that supply keys via env directly rather than path.
    pub fn from_pem(
        private_pem: &[u8],
        public_pem: &[u8],
        access_ttl: i64,
        refresh_ttl: i64,
    ) -> anyhow::Result<Self> {
        let encoding = EncodingKey::from_rsa_pem(private_pem)
            .map_err(|e| anyhow::anyhow!("invalid RSA private key: {e}"))?;
        let decoding = DecodingKey::from_rsa_pem(public_pem)
            .map_err(|e| anyhow::anyhow!("invalid RSA public key: {e}"))?;
        Ok(Self {
            encoding,
            decoding,
            access_ttl,
            refresh_ttl,
        })
    }

    /// Sign an access token. Carries `role` so authz middleware avoids a
    /// per-request DB lookup.
    pub fn sign_access_token(&self, user_id: Uuid, role: &str) -> Result<String, JwtError> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id,
            token_type: TokenType::Access,
            role: Some(role.to_owned()),
            iat: now,
            exp: now + self.access_ttl,
        };
        encode(&Header::new(Algorithm::RS256), &claims, &self.encoding)
            .map_err(|e| JwtError::Sign(e.to_string()))
    }

    /// Sign a refresh token. No `role` — the refresh handler revalidates.
    pub fn sign_refresh_token(&self, user_id: Uuid) -> Result<String, JwtError> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id,
            token_type: TokenType::Refresh,
            role: None,
            iat: now,
            exp: now + self.refresh_ttl,
        };
        encode(&Header::new(Algorithm::RS256), &claims, &self.encoding)
            .map_err(|e| JwtError::Sign(e.to_string()))
    }

    /// Verify a token's signature & expiry, then assert the embedded `typ`
    /// matches `expected`. Returns the decoded claims on success.
    pub fn verify_token(&self, token: &str, expected: TokenType) -> Result<Claims, JwtError> {
        let validation = Validation::new(Algorithm::RS256);
        let data = decode::<Claims>(token, &self.decoding, &validation).map_err(map_jwt_err)?;

        if data.claims.token_type != expected {
            return Err(JwtError::WrongType {
                got: data.claims.token_type,
                expected,
            });
        }
        Ok(data.claims)
    }

    pub fn access_ttl(&self) -> i64 {
        self.access_ttl
    }
    pub fn refresh_ttl(&self) -> i64 {
        self.refresh_ttl
    }
}

/// Verification errors. Kept narrow on purpose — the public API surface
/// should expose only what callers can act on (renew, reject, log).
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("token expired")]
    Expired,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("token type mismatch: got {got:?}, expected {expected:?}")]
    WrongType {
        got: TokenType,
        expected: TokenType,
    },
    #[error("failed to sign token: {0}")]
    Sign(String),
}

fn map_jwt_err(e: jsonwebtoken::errors::Error) -> JwtError {
    match e.kind() {
        ErrorKind::ExpiredSignature => JwtError::Expired,
        ErrorKind::InvalidSignature => JwtError::InvalidSignature,
        // `InvalidToken`, `Base64`, `Json`, `Utf8`, `InvalidAlgorithm`, etc
        // all collapse to "this is not a valid token from us" — callers
        // don't need to distinguish.
        _ => JwtError::Malformed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two independent RSA-2048 keypairs generated once and committed under
    // tests/fixtures/. The `_other` pair drives the cross-key signature
    // rejection test.
    const PRIV_PEM: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_priv.pem");
    const PUB_PEM: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_pub.pem");
    const PRIV_PEM_OTHER: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_priv_other.pem");
    const PUB_PEM_OTHER: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_pub_other.pem");

    fn make_keys() -> Keys {
        Keys::from_pem(PRIV_PEM, PUB_PEM, 3600, 604800).expect("valid test keys")
    }

    #[test]
    fn round_trip_access_token() {
        let keys = make_keys();
        let user_id = Uuid::new_v4();
        let token = keys.sign_access_token(user_id, "user").unwrap();

        let claims = keys.verify_token(&token, TokenType::Access).unwrap();
        assert_eq!(claims.sub, user_id);
        assert_eq!(claims.token_type, TokenType::Access);
        assert_eq!(claims.role.as_deref(), Some("user"));
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, 3600);
    }

    #[test]
    fn round_trip_refresh_token_has_no_role() {
        let keys = make_keys();
        let user_id = Uuid::new_v4();
        let token = keys.sign_refresh_token(user_id).unwrap();

        let claims = keys.verify_token(&token, TokenType::Refresh).unwrap();
        assert_eq!(claims.sub, user_id);
        assert_eq!(claims.token_type, TokenType::Refresh);
        assert!(claims.role.is_none());
        assert_eq!(claims.exp - claims.iat, 604800);
    }

    #[test]
    fn refresh_role_field_omitted_from_jwt_payload() {
        // The JWT body is base64url-encoded JSON. Decode it manually and
        // check `role` is *absent* (not just `null`) — important so that an
        // attacker can't craft a refresh-with-role token expecting it to
        // round-trip.
        let keys = make_keys();
        let token = keys.sign_refresh_token(Uuid::new_v4()).unwrap();

        let payload_b64 = token.split('.').nth(1).expect("jwt has payload");
        let bytes = base64_decode_urlsafe(payload_b64);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            json.get("role").is_none(),
            "refresh token must not carry a role field, got: {json}"
        );
    }

    fn base64_decode_urlsafe(s: &str) -> Vec<u8> {
        // Pad to multiple of 4 since JWTs strip trailing '='.
        let padding = (4 - s.len() % 4) % 4;
        let padded: String = s.chars().chain(std::iter::repeat('=').take(padding)).collect();
        let translated: String = padded
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                other => other,
            })
            .collect();
        decode_b64_std(&translated)
    }

    fn decode_b64_std(s: &str) -> Vec<u8> {
        // Tiny std-only base64 decoder (sufficient for tests).
        const TABLE: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut lut = [255u8; 256];
        for (i, &b) in TABLE.iter().enumerate() {
            lut[b as usize] = i as u8;
        }
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &b in bytes {
            if b == b'=' {
                break;
            }
            let v = lut[b as usize];
            assert_ne!(v, 255, "non-base64 char {b}");
            buf = (buf << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        out
    }

    #[test]
    fn cross_key_signature_is_rejected() {
        // Token signed by key set A, verified by key set B → InvalidSignature.
        let keys_a = Keys::from_pem(PRIV_PEM, PUB_PEM, 3600, 604800).unwrap();
        let keys_b = Keys::from_pem(PRIV_PEM_OTHER, PUB_PEM_OTHER, 3600, 604800).unwrap();

        let token = keys_a.sign_access_token(Uuid::new_v4(), "user").unwrap();
        let err = keys_b.verify_token(&token, TokenType::Access).unwrap_err();
        assert!(
            matches!(err, JwtError::InvalidSignature),
            "expected InvalidSignature, got {err:?}"
        );
    }

    #[test]
    fn expired_token_is_rejected() {
        // jsonwebtoken's default `Validation` allows 60 seconds of clock
        // skew. Use -120 so the token is unambiguously past leeway.
        let keys = Keys::from_pem(PRIV_PEM, PUB_PEM, -120, -120).unwrap();
        let token = keys.sign_access_token(Uuid::new_v4(), "user").unwrap();

        let err = keys.verify_token(&token, TokenType::Access).unwrap_err();
        assert!(
            matches!(err, JwtError::Expired),
            "expected Expired, got {err:?}"
        );
    }

    #[test]
    fn malformed_token_is_rejected() {
        let keys = make_keys();
        let err = keys
            .verify_token("not-a-real-jwt", TokenType::Access)
            .unwrap_err();
        assert!(
            matches!(err, JwtError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    #[test]
    fn token_type_mismatch_is_rejected() {
        // Sign as refresh, verify-as-access → WrongType.
        let keys = make_keys();
        let token = keys.sign_refresh_token(Uuid::new_v4()).unwrap();
        let err = keys.verify_token(&token, TokenType::Access).unwrap_err();
        assert!(
            matches!(
                err,
                JwtError::WrongType {
                    got: TokenType::Refresh,
                    expected: TokenType::Access
                }
            ),
            "expected WrongType(refresh, access), got {err:?}"
        );
    }

    #[test]
    fn tampered_payload_is_rejected() {
        // Flip a byte in the payload segment — the signature won't match.
        let keys = make_keys();
        let token = keys.sign_access_token(Uuid::new_v4(), "user").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let mut payload = parts[1].to_owned();
        // Replace the first character with a different valid base64url char.
        let first = payload.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        payload.replace_range(0..1, &replacement.to_string());
        let tampered = format!("{}.{}.{}", parts[0], payload, parts[2]);

        let err = keys.verify_token(&tampered, TokenType::Access).unwrap_err();
        // Could be InvalidSignature (most common) or Malformed if base64 broke.
        assert!(
            matches!(err, JwtError::InvalidSignature | JwtError::Malformed(_)),
            "expected InvalidSignature or Malformed, got {err:?}"
        );
    }

    #[test]
    fn from_pem_rejects_garbage_input() {
        let err = Keys::from_pem(b"not a key", PUB_PEM, 3600, 604800);
        assert!(err.is_err());
    }
}
