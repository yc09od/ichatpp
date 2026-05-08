//! Shared fixture for ichatpp integration tests.
//!
//! Each test in `tests/*.rs` does `mod common;` and uses helpers from
//! here to build a real `AppState` (against the docker-compose Postgres /
//! Redis / MinIO running at the repo root) and exercise route handlers
//! through the full Actix stack.
//!
//! ## Isolation posture
//!
//! - **Postgres**: the `#[sqlx::test]` macro provisions a fresh database
//!   per test from the `DATABASE_URL` template. Migrations are auto-run.
//! - **Redis**: `flush_redis()` wipes the configured DB at the top of
//!   each test that uses it. Run integration tests with
//!   `--test-threads=1` if they touch Redis to avoid cross-test races.
//! - **MinIO**: tests write to UUID-keyed paths so they don't collide;
//!   we don't bother cleaning up between runs.
//!
//! ## CSRF / Governor
//!
//! Tests bypass both — CSRF is covered by its own middleware unit tests,
//! and Governor would interfere with tight test loops. Tests still build
//! the same `App` route topology as `main.rs`, just without those
//! middleware wraps.

#![allow(dead_code)] // helpers are used selectively across test files

use std::sync::Arc;

use actix_web::{
    body::BoxBody,
    cookie::Cookie,
    dev::{ServiceFactory, ServiceRequest, ServiceResponse},
    test, web, App, Error,
};
use ichatpp::auth::jwt::Keys;
use ichatpp::config::Config;
use ichatpp::services::storage::ObjectStore;
use ichatpp::ws::registry::SessionRegistry;
use ichatpp::{handlers, AppState};
use sqlx::PgPool;
use uuid::Uuid;

// ────────────────────────────────────────────────────────────────────────
// Embedded test JWT keys
// ────────────────────────────────────────────────────────────────────────

/// Re-uses the dev keys committed under `backend/keys/`. They never reach
/// production — `JWT_PRIVATE_KEY_PATH` in deploy environments points at a
/// freshly generated keypair — so embedding them here is a test-only
/// convenience, not a leak.
const JWT_PRIVATE_PEM: &[u8] = include_bytes!("../../keys/jwt-private.pem");
const JWT_PUBLIC_PEM: &[u8] = include_bytes!("../../keys/jwt-public.pem");

// ────────────────────────────────────────────────────────────────────────
// Config + AppState construction
// ────────────────────────────────────────────────────────────────────────

pub fn test_config() -> Config {
    Config {
        // database_url is unused once we hand a fresh `PgPool` straight in;
        // keep it shaped so any code that reads it for diagnostics doesn't
        // hit an empty string.
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://vuugo:vuugo_dev@localhost:5432/ichatpp".to_owned()),
        redis_url: std::env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://:redis_dev@localhost:6379".to_owned()),
        jwt_private_key_path: "keys/jwt-private.pem".to_owned(),
        jwt_public_key_path: "keys/jwt-public.pem".to_owned(),
        jwt_access_ttl_seconds: 3600,
        jwt_refresh_ttl_seconds: 604800,
        s3_endpoint: std::env::var("S3_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:9000".to_owned()),
        s3_access_key: std::env::var("S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".to_owned()),
        s3_secret_key: std::env::var("S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".to_owned()),
        s3_region: "us-east-1".to_owned(),
        s3_bucket_avatars: "avatars".to_owned(),
        s3_bucket_emojis: "emojis".to_owned(),
        s3_bucket_exports: "exports".to_owned(),
        bind_addr: "0.0.0.0:8080".to_owned(),
        frontend_origin: "http://localhost:3000".to_owned(),
        cookie_domain: "localhost".to_owned(),
        cookie_secure: false,
    }
}

pub fn build_state(pool: PgPool) -> AppState {
    let cfg = test_config();
    let redis = redis::Client::open(cfg.redis_url.clone()).expect("open redis client");
    let storage = ObjectStore::from_config(&cfg).expect("build object store");
    let jwt_keys = Keys::from_pem(
        JWT_PRIVATE_PEM,
        JWT_PUBLIC_PEM,
        cfg.jwt_access_ttl_seconds,
        cfg.jwt_refresh_ttl_seconds,
    )
    .expect("load embedded jwt keys");

    AppState {
        db: pool,
        redis,
        config: Arc::new(cfg),
        jwt_keys,
        storage,
        session_registry: SessionRegistry::new(),
        instance_id: Uuid::new_v4(),
    }
}

// ────────────────────────────────────────────────────────────────────────
// Route mounting
// ────────────────────────────────────────────────────────────────────────

/// Mounts the same route topology as `main.rs` minus middleware.
/// Tests covering middleware live next to the middleware modules.
pub fn build_app(
    state: AppState,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Response = ServiceResponse<BoxBody>,
        Config = (),
        InitError = (),
        Error = Error,
    >,
> {
    App::new()
        .app_data(web::Data::new(state))
        .route("/api/ws", web::get().to(handlers::ws::ws_handler))
        .service(
            web::scope("/api/auth")
                .route("/login", web::post().to(handlers::auth::login_handler))
                .route(
                    "/register",
                    web::post().to(handlers::auth::register_handler),
                )
                .route(
                    "/refresh",
                    web::post().to(handlers::auth::refresh_handler),
                )
                .route("/logout", web::post().to(handlers::auth::logout_handler)),
        )
        .service(web::scope("/api/users").configure(handlers::users::routes))
        .service(web::scope("/api/friends").configure(handlers::friends::routes))
        .service(web::scope("/api/messages").configure(handlers::messages::routes))
        .service(web::scope("/api/emojis").configure(handlers::emojis::routes))
        .service(
            web::scope("/api/invitations")
                .service(
                    web::resource("/validate")
                        .route(web::post().to(handlers::invitations::validate_handler)),
                )
                .configure(handlers::invitations::routes),
        )
}

// ────────────────────────────────────────────────────────────────────────
// Redis helpers
// ────────────────────────────────────────────────────────────────────────

pub async fn flush_redis(state: &AppState) {
    let mut conn = state
        .redis
        .get_multiplexed_async_connection()
        .await
        .expect("connect redis");
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut conn)
        .await
        .expect("flushdb");
}

// ────────────────────────────────────────────────────────────────────────
// SQL fixtures — direct DB inserts for tests that don't want the full
// register/admin-grant flow on every setup.
// ────────────────────────────────────────────────────────────────────────

pub struct SeededUser {
    pub id: Uuid,
    pub email: String,
    pub account_code: String,
}

/// Insert a user + 'user' role row directly. `email` defaults to a unique
/// value; supply your own to test email collisions.
pub async fn seed_user(pool: &PgPool, email: Option<&str>) -> SeededUser {
    let id = Uuid::new_v4();
    let email = email.map(str::to_owned).unwrap_or_else(|| {
        format!("user-{}@test.local", &id.simple().to_string()[..12])
    });
    let account_code = unique_account_code();
    let pwd_hash =
        bcrypt::hash("Password123", 4).expect("hash test password (low cost)");

    sqlx::query(
        r#"
        INSERT INTO users (id, email, password_hash, account_code, nickname, is_visible)
        VALUES ($1, $2, $3, $4, $5, TRUE)
        "#,
    )
    .bind(id)
    .bind(&email)
    .bind(&pwd_hash)
    .bind(&account_code)
    .bind::<Option<&str>>(None)
    .execute(pool)
    .await
    .expect("seed user row");

    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'user')")
        .bind(id)
        .execute(pool)
        .await
        .expect("seed user_roles");

    SeededUser {
        id,
        email,
        account_code,
    }
}

/// Insert an admin user + 'admin' role row. Useful for tests that exercise
/// admin-only endpoints without going through the seed_admin CLI.
pub async fn seed_admin(pool: &PgPool) -> SeededUser {
    let user = seed_user(pool, None).await;
    sqlx::query("UPDATE user_roles SET role = 'admin' WHERE user_id = $1")
        .bind(user.id)
        .execute(pool)
        .await
        .expect("promote to admin");
    user
}

/// Insert a message between two users. Returns the new message id.
pub async fn seed_message(
    pool: &PgPool,
    from: Uuid,
    to: Uuid,
    content: &str,
) -> Uuid {
    sqlx::query_scalar(
        r#"
        INSERT INTO messages (from_user_id, to_user_id, content)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(content)
    .fetch_one(pool)
    .await
    .expect("seed message")
}

/// Insert a friendship row in canonical order. Both args may appear in
/// any order.
pub async fn seed_friendship(pool: &PgPool, a: Uuid, b: Uuid) {
    sqlx::query(
        r#"
        INSERT INTO friends (user_id_1, user_id_2)
        VALUES (LEAST($1::uuid, $2::uuid), GREATEST($1::uuid, $2::uuid))
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(a)
    .bind(b)
    .execute(pool)
    .await
    .expect("seed friends row");
}

/// Insert an unused, non-expired invitation. Returns the plaintext code
/// so tests can pass it to `/api/auth/register`.
///
/// The plaintext shape (`INV-` + 20 base62 chars) and the 8-char
/// `code_prefix` derivation match the production generator. We hand-pick
/// base62 chars from a UUID's hex digits so collisions across many seeded
/// invitations stay vanishingly rare without pulling in another RNG dep.
pub async fn seed_invitation(pool: &PgPool, created_by: Uuid) -> String {
    use sha2::{Digest, Sha256};

    let mut suffix = String::with_capacity(20);
    while suffix.len() < 20 {
        let chunk = Uuid::new_v4().simple().to_string();
        for c in chunk.chars() {
            if c.is_ascii_alphanumeric() {
                suffix.push(c);
                if suffix.len() == 20 {
                    break;
                }
            }
        }
    }
    let plain = format!("INV-{suffix}");
    let hash = hex::encode(Sha256::digest(plain.as_bytes()));
    let prefix = &plain[..8];

    sqlx::query(
        r#"
        INSERT INTO invitations (code_hash, code_prefix, created_by, expires_at, status)
        VALUES ($1, $2, $3, NOW() + INTERVAL '7 days', 'unused')
        "#,
    )
    .bind(&hash)
    .bind(prefix)
    .bind(created_by)
    .execute(pool)
    .await
    .expect("seed invitation");

    plain
}

fn unique_account_code() -> String {
    // 10-digit code drawn from the UUID's lower bits. Collisions are
    // exceedingly unlikely across one test process; the schema CHECK
    // (^[0-9]{10}$) is satisfied.
    let raw = Uuid::new_v4().as_u128();
    let mut s = format!("{:010}", raw % 9_999_999_999u128);
    if s.len() > 10 {
        s.truncate(10);
    }
    s
}

// ────────────────────────────────────────────────────────────────────────
// Cookie / response helpers
// ────────────────────────────────────────────────────────────────────────

/// Pull the value of a single cookie out of a `ServiceResponse`. Returns
/// `None` if the cookie wasn't set.
pub fn cookie_value<B>(resp: &ServiceResponse<B>, name: &str) -> Option<String> {
    for v in resp
        .response()
        .headers()
        .get_all(actix_web::http::header::SET_COOKIE)
    {
        let raw = v.to_str().ok()?;
        if let Ok(c) = Cookie::parse(raw) {
            if c.name() == name {
                return Some(c.value().to_owned());
            }
        }
    }
    None
}

/// Convenience: gather the access/refresh/csrf cookies emitted by login
/// or register so subsequent requests can re-attach them.
pub struct AuthCookies {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub csrf_token: String,
}

impl AuthCookies {
    pub fn from_response<B>(resp: &ServiceResponse<B>) -> Self {
        let access =
            cookie_value(resp, "access_token").expect("access_token cookie missing");
        let refresh = cookie_value(resp, "refresh_token");
        let csrf =
            cookie_value(resp, "csrf_token").expect("csrf_token cookie missing");
        Self {
            access_token: access,
            refresh_token: refresh,
            csrf_token: csrf,
        }
    }

    /// Render as a single `Cookie:` header value. The CSRF cookie is
    /// included for completeness, but most tests only need access_token
    /// since they bypass the CSRF middleware.
    pub fn header_value(&self) -> String {
        let mut out = format!(
            "access_token={}; csrf_token={}",
            self.access_token, self.csrf_token
        );
        if let Some(r) = &self.refresh_token {
            out.push_str(&format!("; refresh_token={}", r));
        }
        out
    }
}

// ────────────────────────────────────────────────────────────────────────
// JWT minting — for tests that need an authenticated request without
// going through register/login.
// ────────────────────────────────────────────────────────────────────────

/// Mint an access token for an arbitrary user_id + role using the same
/// keys the test app is configured with. Returns just the JWT string;
/// callers wrap it in a `Cookie:` header.
pub fn mint_access_token(state: &AppState, user_id: Uuid, role: &str) -> String {
    state
        .jwt_keys
        .sign_access_token(user_id, role)
        .expect("sign access token")
}

/// Build a `Cookie:` header that authenticates as the given user.
pub fn auth_cookie_header(state: &AppState, user_id: Uuid, role: &str) -> String {
    let access = mint_access_token(state, user_id, role);
    format!("access_token={}", access)
}

// ────────────────────────────────────────────────────────────────────────
// Multipart payload builders for upload-handler tests
// ────────────────────────────────────────────────────────────────────────

/// Render a minimal opaque PNG (8x8 of a single colour) via the `image`
/// crate. Tests use this to drive the emoji / avatar upload paths without
/// needing a fixture file on disk.
pub fn tiny_png_bytes() -> Vec<u8> {
    use image::{ImageBuffer, ImageFormat, Rgba};

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(8, 8, Rgba([10, 20, 30, 255]));
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, ImageFormat::Png)
        .expect("encode tiny png");
    buf
}

/// Pack a single file field plus optional name field into a multipart
/// body using the canonical CRLF separators. Returns `(content_type, body)`.
pub fn multipart_file_body(
    field_name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
    extra_text_fields: &[(&str, &str)],
) -> (String, Vec<u8>) {
    let boundary = format!("----ichatpp-test-{}", Uuid::new_v4().simple());
    let mut body: Vec<u8> = Vec::new();
    for (name, value) in extra_text_fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let header_value = format!("multipart/form-data; boundary={boundary}");
    (header_value, body)
}

// ────────────────────────────────────────────────────────────────────────
// Request / response convenience
// ────────────────────────────────────────────────────────────────────────

pub use test::{call_service, init_service, read_body, TestRequest};
