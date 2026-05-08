//! WebSocket upgrade handler — `GET /api/ws` (TODO [26]).
//!
//! The auth gate runs **before** [`actix_ws::handle`] so a missing /
//! invalid `access_token` cookie produces a clean 401 without
//! attempting an upgrade. Only after the JWT verifies do we hand the
//! socket off to [`crate::ws::session::run`].
//!
//! The session task is `actix_web::rt::spawn`-ed (i.e. tokio-spawned)
//! so the HTTP handler can return the upgrade response immediately;
//! the per-connection lifecycle then lives entirely on its own task,
//! independent of the request that started it.

use actix_web::{web, HttpRequest, HttpResponse};

use crate::auth::cookies::ACCESS_COOKIE_NAME;
use crate::auth::jwt::TokenType;
use crate::errors::{AppError, AppResult};
use crate::AppState;

/// `GET /api/ws`
///
/// Auth → upgrade → spawn. The handler itself is fast — all per-
/// connection work moves onto the spawned task immediately.
pub async fn ws_handler(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    // Auth gate runs first. If we let actix_ws::handle() upgrade the
    // socket and *then* checked auth, an unauthenticated client would
    // see a successful 101 followed by an immediate close — confusing
    // for both clients and tests. The acceptance criterion ("未登录连接
    // 返回 401") makes this ordering mandatory.
    let cookie = req
        .cookie(ACCESS_COOKIE_NAME)
        .ok_or_else(|| AppError::Unauthorized("missing access_token cookie".into()))?;

    // verify_token covers signature, expiry, and `typ` mismatch (a
    // refresh token in the access cookie is rejected here).
    let claims = state
        .jwt_keys
        .verify_token(cookie.value(), TokenType::Access)
        .map_err(|_| AppError::Unauthorized("invalid access token".into()))?;

    // Past this point the request is well-formed and authenticated.
    // A non-WS handshake (missing Upgrade header etc.) collapses to a
    // 400 — that's a malformed request, not an auth failure.
    let (response, session, msg_stream) = actix_ws::handle(&req, body)
        .map_err(|e| AppError::BadRequest(format!("invalid websocket handshake: {e}")))?;

    let user_id = claims.sub;
    // Clone the whole AppState into the spawned task — the session
    // needs db (for friend lookups in TODO [27] broadcast), redis,
    // session_registry, and instance_id.
    let task_state = state.get_ref().clone();

    // actix_web::rt::spawn is tokio::spawn under the hood (actix-web 4
    // runs on tokio). The session task outlives the handler return —
    // no shared state with the request, so it's safe to detach.
    actix_web::rt::spawn(async move {
        crate::ws::session::run(user_id, session, msg_stream, task_state).await;
    });

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::jwt::Keys;
    use crate::config::Config;
    use actix_web::{
        cookie::Cookie,
        test::{call_service, init_service, TestRequest},
        web, App,
    };
    use std::sync::Arc;
    use uuid::Uuid;

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

    macro_rules! mount_ws {
        ($state:expr) => {
            init_service(
                App::new()
                    .app_data(web::Data::new($state))
                    .route("/api/ws", web::get().to(ws_handler)),
            )
            .await
        };
    }

    /// Acceptance criterion: "未登录连接返回 401". With no cookie at all
    /// the handler must short-circuit before attempting an upgrade.
    #[actix_web::test]
    async fn missing_cookie_returns_401() {
        let app = mount_ws!(make_state());
        let req = TestRequest::get().uri("/api/ws").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn malformed_token_returns_401() {
        let app = mount_ws!(make_state());
        let req = TestRequest::get()
            .uri("/api/ws")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, "not-a-real-jwt"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    /// Type-confusion guard: a valid refresh token in the access cookie
    /// must not authenticate. Same property the AdminUser /
    /// AuthenticatedUser extractors enforce.
    #[actix_web::test]
    async fn refresh_token_in_access_cookie_returns_401() {
        let state = make_state();
        let token = state.jwt_keys.sign_refresh_token(Uuid::new_v4()).unwrap();
        let app = mount_ws!(state);
        let req = TestRequest::get()
            .uri("/api/ws")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    /// With a *valid* access cookie but no `Upgrade: websocket` header,
    /// auth passes and the handler should fail at the upgrade step
    /// with 400 (malformed handshake) — proves the auth gate runs
    /// before the upgrade attempt and that the upgrade error surfaces
    /// as a client error, not a server error.
    #[actix_web::test]
    async fn valid_token_without_upgrade_header_returns_400() {
        let state = make_state();
        let token = state
            .jwt_keys
            .sign_access_token(Uuid::new_v4(), "user")
            .unwrap();
        let app = mount_ws!(state);
        let req = TestRequest::get()
            .uri("/api/ws")
            .cookie(Cookie::new(ACCESS_COOKIE_NAME, token))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }
}
