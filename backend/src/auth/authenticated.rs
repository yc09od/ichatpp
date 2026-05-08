//! `AuthenticatedUser` extractor — gates every endpoint that needs *any*
//! logged-in user (no role check).
//!
//! Companion to [`crate::auth::admin::AdminUser`]: same mechanism (read
//! the access cookie, verify JWT signature/expiry/type), the only
//! difference is that this extractor accepts whatever `role` claim the
//! token carries instead of demanding `admin`.
//!
//! 401 (vs. 403) policy: any failure here is "you couldn't authenticate"
//! — there is no role-mismatch path. A handler that wants to require
//! a specific role should use `AdminUser` (or, when more roles land,
//! a similar role-checking extractor).

#![allow(dead_code)] // first non-test consumer lands with TODO [21]

use std::future::{ready, Ready};

use actix_web::{dev::Payload, web, FromRequest, HttpRequest};
use uuid::Uuid;

use crate::auth::cookies::ACCESS_COOKIE_NAME;
use crate::auth::jwt::TokenType;
use crate::errors::AppError;
use crate::AppState;

/// Default role assumed when a (theoretically impossible) access token
/// has no `role` claim. Mirrors the same "fall back to user" defense
/// the login query uses for a missing `user_roles` row.
const DEFAULT_ROLE: &str = "user";

/// Marker type — handlers list it as a parameter to require auth, then
/// read `user_id` / `role` from the extracted struct.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    pub role: String,
}

impl FromRequest for AuthenticatedUser {
    type Error = AppError;
    type Future = Ready<Result<Self, AppError>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(extract(req))
    }
}

fn extract(req: &HttpRequest) -> Result<AuthenticatedUser, AppError> {
    let cookie = req
        .cookie(ACCESS_COOKIE_NAME)
        .ok_or_else(|| AppError::Unauthorized("missing access_token cookie".into()))?;

    // Misconfiguration — the route was mounted without the AppState
    // `web::Data`. Surface as 500 (Internal) since clients can't fix it.
    let state = req.app_data::<web::Data<AppState>>().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "AuthenticatedUser extractor: AppState not registered on this scope"
        ))
    })?;

    // `verify_token(.., Access)` covers signature, expiry, AND token-type
    // confusion — a refresh token presented in the access cookie is
    // rejected at this step (the `typ` claim mismatches).
    let claims = state
        .jwt_keys
        .verify_token(cookie.value(), TokenType::Access)
        .map_err(|_| AppError::Unauthorized("invalid access token".into()))?;

    Ok(AuthenticatedUser {
        user_id: claims.sub,
        role: claims.role.unwrap_or_else(|| DEFAULT_ROLE.to_owned()),
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
            db: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy(&cfg.database_url)
                .expect("lazy pool"),
            redis: redis::Client::open(cfg.redis_url.clone()).expect("redis client"),
            jwt_keys: Keys::from_pem(PRIV_PEM, PUB_PEM, 3600, 604800).expect("test keys"),
            storage: crate::services::storage::ObjectStore::from_config(&cfg)
                .expect("test object store"),
            session_registry: crate::ws::registry::SessionRegistry::new(),
            instance_id: Uuid::new_v4(),
            config: Arc::new(cfg),
        }
    }

    /// Stub — extractor runs first; this body only executes if it accepts.
    #[get("/me/echo")]
    async fn echo(user: AuthenticatedUser) -> HttpResponse {
        HttpResponse::Ok().body(format!("{}|{}", user.user_id, user.role))
    }

    macro_rules! mount_echo {
        ($state:expr) => {
            init_service(
                App::new()
                    .app_data(web::Data::new($state))
                    .service(echo),
            )
            .await
        };
    }

    /// The acceptance criterion's first half ("未登录调用 /api/users/me
    /// 返回 401") rides on this — no access cookie, no entry.
    #[actix_web::test]
    async fn missing_access_cookie_returns_401() {
        let app = mount_echo!(make_state());
        let req = TestRequest::get().uri("/me/echo").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn malformed_token_returns_401() {
        let app = mount_echo!(make_state());
        let req = TestRequest::get()
            .uri("/me/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, "not-a-real-jwt"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    /// Type-confusion guard — same as `AdminUser`. A valid refresh token
    /// in the access cookie must not authenticate.
    #[actix_web::test]
    async fn refresh_token_in_access_cookie_returns_401() {
        let state = make_state();
        let token = state.jwt_keys.sign_refresh_token(Uuid::new_v4()).unwrap();
        let app = mount_echo!(state);
        let req = TestRequest::get()
            .uri("/me/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    /// Unlike `AdminUser`, a non-admin role passes through here. Pin
    /// the contract: this extractor accepts *any* role.
    #[actix_web::test]
    async fn user_role_passes_through() {
        let state = make_state();
        let user_id = Uuid::new_v4();
        let token = state.jwt_keys.sign_access_token(user_id, "user").unwrap();
        let app = mount_echo!(state);
        let req = TestRequest::get()
            .uri("/me/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
        let body = actix_web::test::read_body(resp).await;
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            format!("{user_id}|user")
        );
    }

    #[actix_web::test]
    async fn admin_role_also_passes_through() {
        let state = make_state();
        let user_id = Uuid::new_v4();
        let token = state.jwt_keys.sign_access_token(user_id, "admin").unwrap();
        let app = mount_echo!(state);
        let req = TestRequest::get()
            .uri("/me/echo")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
        let body = actix_web::test::read_body(resp).await;
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            format!("{user_id}|admin")
        );
    }
}
