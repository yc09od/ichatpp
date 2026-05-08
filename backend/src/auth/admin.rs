//! `AdminUser` extractor — gates the admin-only endpoints.
//!
//! Per ARCHITECTURE.md §4.6, the role is embedded in the access token's
//! claims, so admin checks don't need a per-request DB roundtrip. The
//! tradeoff: demoting a user only takes effect once their access token
//! expires (or is revoked via the refresh-token blocklist landing in
//! TODO [19]).
//!
//! Failures are split between two status codes:
//! - missing / invalid / wrong-type token → **401 Unauthorized**
//! - valid access token but role ≠ "admin" → **403 Forbidden**
//!
//! TODO [14]'s acceptance criterion ("非管理员调用全部返回 403") covers the
//! authenticated-but-wrong-role case; missing-credential still uses 401
//! because that's what callers act on (re-auth vs. give up).

use std::future::{ready, Ready};

use actix_web::{dev::Payload, web, FromRequest, HttpRequest};
use uuid::Uuid;

use crate::auth::cookies::ACCESS_COOKIE_NAME;
use crate::auth::jwt::TokenType;
use crate::errors::AppError;
use crate::AppState;

const ADMIN_ROLE: &str = "admin";

/// Marker type that handlers list as a parameter to require admin auth.
/// `user_id` is exposed so handlers can record `created_by` etc.
#[derive(Debug, Clone, Copy)]
pub struct AdminUser {
    pub user_id: Uuid,
}

impl FromRequest for AdminUser {
    type Error = AppError;
    type Future = Ready<Result<Self, AppError>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(extract(req))
    }
}

fn extract(req: &HttpRequest) -> Result<AdminUser, AppError> {
    let cookie = req
        .cookie(ACCESS_COOKIE_NAME)
        .ok_or_else(|| AppError::Unauthorized("missing access_token cookie".into()))?;

    // Misconfiguration — the route was mounted without the AppState
    // `web::Data`. Surface as 500 (Internal) since clients can't fix it.
    let state = req
        .app_data::<web::Data<AppState>>()
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "AdminUser extractor: AppState not registered on this scope"
            ))
        })?;

    // `verify_token(.., Access)` covers signature, expiry, AND token-type
    // confusion — a refresh token presented in the access cookie is
    // rejected at this step (the `typ` claim mismatches).
    let claims = state
        .jwt_keys
        .verify_token(cookie.value(), TokenType::Access)
        .map_err(|_| AppError::Unauthorized("invalid access token".into()))?;

    if claims.role.as_deref() != Some(ADMIN_ROLE) {
        return Err(AppError::Forbidden("admin role required".into()));
    }

    Ok(AdminUser {
        user_id: claims.sub,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::jwt::Keys;
    use crate::config::Config;
    use actix_web::{
        cookie::Cookie,
        get,
        test::{call_service, init_service, TestRequest},
        App, HttpResponse,
    };
    use std::sync::Arc;

    const PRIV_PEM: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_priv.pem");
    const PUB_PEM: &[u8] = include_bytes!("../../tests/fixtures/jwt_test_pub.pem");

    fn make_state() -> AppState {
        let cfg = Config {
            database_url: "postgres://x@localhost/x".into(),
            redis_url: "redis://localhost".into(),
            jwt_private_key_path: "x".into(),
            jwt_public_key_path: "x".into(),
            jwt_access_ttl_seconds: 3600,
            jwt_refresh_ttl_seconds: 604800,
            s3_endpoint: "http://localhost:9000".into(),
            s3_access_key: "x".into(),
            s3_secret_key: "x".into(),
            s3_region: "us-east-1".into(),
            s3_bucket_avatars: "avatars".into(),
            s3_bucket_emojis: "emojis".into(),
            s3_bucket_exports: "exports".into(),
            bind_addr: "0.0.0.0:0".into(),
            frontend_origin: "http://localhost:3000".into(),
            cookie_domain: "localhost".into(),
            cookie_secure: false,
        };

        AppState {
            // `connect_lazy` doesn't open a connection until first use, so
            // tests that never query work with an unreachable URL.
            db: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy(&cfg.database_url)
                .expect("lazy pool"),
            redis: redis::Client::open(cfg.redis_url.clone()).expect("redis client"),
            jwt_keys: Keys::from_pem(PRIV_PEM, PUB_PEM, 3600, 604800).expect("test keys"),
            // ObjectStore parses creds at construction; tests don't talk
            // to S3 so the dummy values from `cfg` are sufficient.
            storage: crate::services::storage::ObjectStore::from_config(&cfg)
                .expect("test object store"),
            session_registry: crate::ws::registry::SessionRegistry::new(),
            instance_id: Uuid::new_v4(),
            config: Arc::new(cfg),
        }
    }

    /// Stub handler — extractor runs first; this body only executes if
    /// the extractor accepts the request.
    #[get("/admin/echo")]
    async fn admin_echo(admin: AdminUser) -> HttpResponse {
        HttpResponse::Ok().body(admin.user_id.to_string())
    }

    macro_rules! mount_admin_echo {
        ($state:expr) => {
            init_service(
                App::new()
                    .app_data(web::Data::new($state))
                    .service(admin_echo),
            )
            .await
        };
    }

    #[actix_web::test]
    async fn missing_access_cookie_returns_401() {
        let app = mount_admin_echo!(make_state());
        let req = TestRequest::get().uri("/admin/echo").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn malformed_token_returns_401() {
        let app = mount_admin_echo!(make_state());
        let req = TestRequest::get()
            .uri("/admin/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, "not-a-real-jwt"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn refresh_token_in_access_cookie_returns_401() {
        // Type-confusion guard: a valid refresh token must not be accepted
        // here. `verify_token(.., Access)` rejects on `typ` mismatch.
        let state = make_state();
        let token = state.jwt_keys.sign_refresh_token(Uuid::new_v4()).unwrap();
        let app = mount_admin_echo!(state);
        let req = TestRequest::get()
            .uri("/admin/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn user_role_returns_403() {
        // The TODO [14] verification: a logged-in non-admin must get 403.
        let state = make_state();
        let token = state
            .jwt_keys
            .sign_access_token(Uuid::new_v4(), "user")
            .unwrap();
        let app = mount_admin_echo!(state);
        let req = TestRequest::get()
            .uri("/admin/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn admin_role_passes_through() {
        let state = make_state();
        let admin_id = Uuid::new_v4();
        let token = state
            .jwt_keys
            .sign_access_token(admin_id, "admin")
            .unwrap();
        let app = mount_admin_echo!(state);
        let req = TestRequest::get()
            .uri("/admin/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
        let body = actix_web::test::read_body(resp).await;
        assert_eq!(std::str::from_utf8(&body).unwrap(), admin_id.to_string());
    }
}
