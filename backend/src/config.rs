//! Application configuration loaded from environment variables.
//!
//! Source of truth for required keys: `.env.example` at the repo root.
//! `.env` is soft-loaded if present; values from the real environment
//! always win over `.env`.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,

    pub jwt_private_key_path: String,
    pub jwt_public_key_path: String,
    pub jwt_access_ttl_seconds: i64,
    pub jwt_refresh_ttl_seconds: i64,

    pub s3_endpoint: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_region: String,
    pub s3_bucket_avatars: String,
    pub s3_bucket_emojis: String,
    pub s3_bucket_exports: String,

    pub bind_addr: String,
    pub frontend_origin: String,
    pub cookie_domain: String,
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        // .env is convenience for local dev; ignore "not found" silently.
        let _ = dotenvy::dotenv();

        Ok(Self {
            database_url: required("DATABASE_URL")?,
            redis_url: required("REDIS_URL")?,

            jwt_private_key_path: required("JWT_PRIVATE_KEY_PATH")?,
            jwt_public_key_path: required("JWT_PUBLIC_KEY_PATH")?,
            jwt_access_ttl_seconds: optional_or("JWT_ACCESS_TTL_SECONDS", 3600)?,
            jwt_refresh_ttl_seconds: optional_or("JWT_REFRESH_TTL_SECONDS", 604800)?,

            s3_endpoint: required("S3_ENDPOINT")?,
            s3_access_key: required("S3_ACCESS_KEY")?,
            s3_secret_key: required("S3_SECRET_KEY")?,
            s3_region: optional_string("S3_REGION", "us-east-1"),
            s3_bucket_avatars: optional_string("S3_BUCKET_AVATARS", "avatars"),
            s3_bucket_emojis: optional_string("S3_BUCKET_EMOJIS", "emojis"),
            s3_bucket_exports: optional_string("S3_BUCKET_EXPORTS", "exports"),

            bind_addr: optional_string("BIND_ADDR", "0.0.0.0:8080"),
            frontend_origin: optional_string("FRONTEND_ORIGIN", "http://localhost:3000"),
            cookie_domain: optional_string("COOKIE_DOMAIN", "localhost"),
            cookie_secure: env::var("COOKIE_SECURE")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        })
    }
}

fn required(key: &str) -> anyhow::Result<String> {
    env::var(key).map_err(|_| anyhow::anyhow!("required env var {key} is not set"))
}

fn optional_string(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn optional_or<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(v) => v
            .parse()
            .map_err(|e| anyhow::anyhow!("env {key} has invalid value: {e}")),
        Err(_) => Ok(default),
    }
}
