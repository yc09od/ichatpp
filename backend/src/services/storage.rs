//! S3 / MinIO object-storage client wrapper.
//!
//! Sits between the handlers and `rust-s3` so that:
//!
//! 1. The `Region` + `Credentials` are constructed exactly once at
//!    startup from [`crate::config::Config`] — handlers don't repeat the
//!    parsing / error mapping.
//! 2. URL construction lives in one place, so the path-style URL the SPA
//!    receives ("{endpoint}/{bucket}/{key}") cannot drift away from the
//!    actual upload destination.
//! 3. Tests can swap in a mock by depending on `ObjectStore` rather than
//!    the SDK directly.
//!
//! ## Path-style addressing
//!
//! MinIO and a self-hosted S3 with custom DNS both require path-style
//! ("https://host/bucket/key") instead of vhost-style
//! ("https://bucket.host/key"). We pin path-style on every bucket here
//! so a later AWS deployment doesn't need handler-side branching — set
//! `S3_ENDPOINT` to the regional endpoint and AWS accepts path-style
//! just fine.

#![allow(dead_code)] // first non-test consumer mounts in TODO [22]; emoji
                     // upload (TODO [35]) and exports (TODO [34]) reuse it.

use s3::creds::Credentials;
use s3::error::S3Error;
use s3::{Bucket, Region};

use crate::config::Config;

/// Process-wide handle to the configured S3 endpoint. Cheap to clone
/// (`Region` and `Credentials` are both small `Clone` structs). The
/// `Bucket` value is reconstructed per request because `rust-s3`'s
/// `Bucket` is parameterised by name and we have several (avatars,
/// emojis, exports).
#[derive(Clone, Debug)]
pub struct ObjectStore {
    region: Region,
    credentials: Credentials,
    /// Pre-trimmed for URL construction. Stored separately because
    /// `Region::Custom` consumes the endpoint string and there's no
    /// public accessor for it back out.
    endpoint_base: String,
}

impl ObjectStore {
    /// Build from the same env-loaded [`Config`] the rest of the server uses.
    /// Validates the credentials at boot so a misconfigured access key
    /// fails the server start instead of failing the first upload.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let endpoint = cfg.s3_endpoint.trim_end_matches('/').to_owned();
        let region = Region::Custom {
            region: cfg.s3_region.clone(),
            endpoint: endpoint.clone(),
        };
        let credentials = Credentials::new(
            Some(&cfg.s3_access_key),
            Some(&cfg.s3_secret_key),
            None, // session_token (we use static credentials)
            None, // security_token (deprecated alias of session_token)
            None, // profile (we don't read ~/.aws/credentials)
        )
        .map_err(|e| anyhow::anyhow!("invalid S3 credentials: {e}"))?;

        Ok(Self {
            region,
            credentials,
            endpoint_base: endpoint,
        })
    }

    /// PUT `bytes` to `bucket/key` with the given Content-Type. Returns
    /// `Ok(())` only on a 2xx response from S3 — `rust-s3` raises
    /// `S3Error::HttpFailWithBody` for any non-2xx, which the caller
    /// maps to `AppError::Internal`.
    pub async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), S3Error> {
        // `Bucket::new` returns `Box<Bucket>`; `with_path_style` consumes
        // and returns the same. We dereference for the actual call so
        // the borrow doesn't escape this function.
        let bucket = Bucket::new(bucket, self.region.clone(), self.credentials.clone())?
            .with_path_style();
        bucket
            .put_object_with_content_type(key, bytes, content_type)
            .await?;
        Ok(())
    }

    /// Build the public path-style URL for an uploaded object. The bucket
    /// must be configured for public-read in MinIO (or fronted by a CDN
    /// that proxies to it) for browsers to fetch this URL directly.
    pub fn object_url(&self, bucket: &str, key: &str) -> String {
        format!("{}/{}/{}", self.endpoint_base, bucket, key)
    }

    /// DELETE `bucket/key`. S3 returns 204 even if the key didn't exist,
    /// so this is naturally idempotent — useful for the emoji-delete
    /// path (TODO [36]) where the DB row may have been removed but the
    /// object orphaned in a previous half-failed delete.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<(), S3Error> {
        let bucket = Bucket::new(bucket, self.region.clone(), self.credentials.clone())?
            .with_path_style();
        bucket.delete_object(key).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            database_url: "x".into(),
            redis_url: "x".into(),
            jwt_private_key_path: "x".into(),
            jwt_public_key_path: "x".into(),
            jwt_access_ttl_seconds: 3600,
            jwt_refresh_ttl_seconds: 604800,
            s3_endpoint: "http://localhost:9000".into(),
            s3_access_key: "minioadmin".into(),
            s3_secret_key: "minioadmin".into(),
            s3_region: "us-east-1".into(),
            s3_bucket_avatars: "avatars".into(),
            s3_bucket_emojis: "emojis".into(),
            s3_bucket_exports: "exports".into(),
            bind_addr: "0.0.0.0:0".into(),
            frontend_origin: "http://localhost:3000".into(),
            cookie_domain: "localhost".into(),
            cookie_secure: false,
        }
    }

    #[test]
    fn from_config_accepts_minio_style_endpoint() {
        // Construct succeeds without making any network call — the SDK
        // defers connection to the first PUT.
        let store = ObjectStore::from_config(&test_config()).expect("valid config");
        assert_eq!(store.endpoint_base, "http://localhost:9000");
    }

    #[test]
    fn from_config_strips_trailing_slash() {
        // A user copy-pasting the MinIO URL with a trailing slash would
        // otherwise produce double-slash URLs ("/avatars//user_id/...").
        let mut cfg = test_config();
        cfg.s3_endpoint = "http://localhost:9000/".into();
        let store = ObjectStore::from_config(&cfg).unwrap();
        assert_eq!(store.endpoint_base, "http://localhost:9000");
    }

    #[test]
    fn object_url_is_path_style() {
        // Path-style is the format MinIO and AWS S3 both accept; vhost-
        // style would force a DNS lookup per bucket which won't work
        // for self-hosted MinIO. Pin the format.
        let store = ObjectStore::from_config(&test_config()).unwrap();
        let url = store.object_url("avatars", "uid/abc.png");
        assert_eq!(url, "http://localhost:9000/avatars/uid/abc.png");
    }

    /// Pin against the case where someone "fixes" the URL builder by
    /// percent-encoding the slashes in the key — the key is already a
    /// path-segment so splitting it would break the upload.
    #[test]
    fn object_url_does_not_encode_key_slashes() {
        let store = ObjectStore::from_config(&test_config()).unwrap();
        let url = store.object_url("b", "a/b/c");
        assert!(url.ends_with("/b/a/b/c"));
    }
}
