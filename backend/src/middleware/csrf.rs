//! CSRF protection — double-submit cookie pattern.
//!
//! Per ARCHITECTURE.md §4.1 / §6, on successful login the server sets a
//! **non-httpOnly** `csrf_token` cookie. The SPA reads it and echoes the
//! value back in the `X-CSRF-Token` header on every mutation. This module
//! provides:
//!
//! 1. [`CsrfProtection`] — actix middleware that rejects mutations whose
//!    cookie/header pair is missing or mismatched.
//! 2. [`generate_csrf_token`] / [`build_csrf_cookie`] — helpers for the
//!    auth handlers (register/login/refresh, landing in TODO [12]/[17]/[18])
//!    to attach a fresh token cookie on successful authentication.
//!
//! ## Bypass list
//!
//! Login, register, and the public invitation-validate endpoint are
//! pre-authentication entry points: no cookie exists yet. They are guarded
//! by the rate limiter and password / invitation-code checks instead.
//!
//! ## Why constant-time compare?
//!
//! Tokens are random and equal-length, so a naive `==` would in theory leak
//! one byte at a time via timing. The risk is small here, but the cost of a
//! 32-byte XOR loop is negligible — so we do it.

#![allow(dead_code)] // helpers consumed by login/register in TODOs [12]/[17]/[18]

use std::future::{ready, Ready};
use std::rc::Rc;

use actix_web::body::{BoxBody, EitherBody};
use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::http::Method;
use actix_web::{Error, HttpResponse};
use futures_util::future::LocalBoxFuture;
use rand::RngCore;
use serde_json::json;

use crate::config::Config;

/// Bytes of entropy in a CSRF token. Hex-encoded → 64-character cookie value.
const CSRF_TOKEN_BYTES: usize = 32;

/// Cookie name carrying the CSRF token (non-httpOnly so the SPA can read it).
pub const CSRF_COOKIE_NAME: &str = "csrf_token";

/// Header name carrying the CSRF token from the SPA on mutations.
pub const CSRF_HEADER_NAME: &str = "X-CSRF-Token";

/// Generate a fresh, cryptographically-strong CSRF token using `OsRng`.
pub fn generate_csrf_token() -> String {
    let mut bytes = [0u8; CSRF_TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Build the `csrf_token` cookie per ARCHITECTURE.md §4.1.
///
/// **non-httpOnly** is intentional: the SPA must read the value to populate
/// the `X-CSRF-Token` header on mutations. The `Secure` flag is gated by
/// `COOKIE_SECURE` so local dev over plain HTTP still works.
pub fn build_csrf_cookie(token: String, config: &Config) -> Cookie<'static> {
    Cookie::build(CSRF_COOKIE_NAME, token)
        .http_only(false)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(CookieDuration::seconds(3600))
        .domain(config.cookie_domain.clone())
        .finish()
}

/// Constant-time equality. Length itself is non-secret here, so an early
/// length check is fine; for equal-length inputs we never short-circuit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn is_exempt_path(path: &str) -> bool {
    matches!(
        path,
        "/api/auth/login" | "/api/auth/register" | "/api/invitations/validate"
    )
}

fn is_mutation(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Public middleware factory. Wrap with `App::wrap(CsrfProtection)`.
///
/// Place it **inside** CORS / Logger / Governor so a 403 still flows back
/// through the access log and gets the correct CORS headers.
#[derive(Clone, Copy, Default)]
pub struct CsrfProtection;

impl<S, B> Transform<S, ServiceRequest> for CsrfProtection
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B, BoxBody>>;
    type Error = Error;
    type Transform = CsrfMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(CsrfMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct CsrfMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for CsrfMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B, BoxBody>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Safe (non-mutating) methods and exempt paths bypass validation.
        if !is_mutation(req.method()) || is_exempt_path(req.path()) {
            let svc = Rc::clone(&self.service);
            return Box::pin(async move {
                svc.call(req).await.map(ServiceResponse::map_into_left_body)
            });
        }

        let cookie_token = req.cookie(CSRF_COOKIE_NAME).map(|c| c.value().to_owned());
        let header_token = req
            .headers()
            .get(CSRF_HEADER_NAME)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let valid = match (cookie_token.as_deref(), header_token.as_deref()) {
            (Some(c), Some(h)) if !c.is_empty() && !h.is_empty() => {
                constant_time_eq(c.as_bytes(), h.as_bytes())
            }
            _ => false,
        };

        if valid {
            let svc = Rc::clone(&self.service);
            Box::pin(async move {
                svc.call(req).await.map(ServiceResponse::map_into_left_body)
            })
        } else {
            let response = HttpResponse::Forbidden().json(json!({
                "error": {
                    "code": "FORBIDDEN",
                    "message": "missing or invalid CSRF token"
                }
            }));
            Box::pin(async move { Ok(req.into_response(response).map_into_right_body()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{
        cookie::Cookie as TestCookie,
        test::{call_service, init_service, TestRequest},
        web, App, HttpResponse,
    };

    async fn ok_handler() -> HttpResponse {
        HttpResponse::Ok().body("ok")
    }

    fn matching_pair() -> (&'static str, &'static str) {
        // Equal cookie + header — the happy path.
        ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    }

    #[actix_web::test]
    async fn get_passes_without_token() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::get().to(ok_handler)),
        )
        .await;

        let req = TestRequest::get().uri("/api/test").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[actix_web::test]
    async fn post_without_cookie_or_header_returns_403() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post().uri("/api/test").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn post_with_cookie_only_returns_403() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post()
            .uri("/api/test")
            .cookie(TestCookie::new(CSRF_COOKIE_NAME, "abc123"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn post_with_header_only_returns_403() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post()
            .uri("/api/test")
            .insert_header((CSRF_HEADER_NAME, "abc123"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn post_with_mismatched_cookie_and_header_returns_403() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post()
            .uri("/api/test")
            .cookie(TestCookie::new(CSRF_COOKIE_NAME, "cookieval"))
            .insert_header((CSRF_HEADER_NAME, "headerval"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn post_with_matching_cookie_and_header_passes() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let (cookie_v, header_v) = matching_pair();
        let req = TestRequest::post()
            .uri("/api/test")
            .cookie(TestCookie::new(CSRF_COOKIE_NAME, cookie_v))
            .insert_header((CSRF_HEADER_NAME, header_v))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[actix_web::test]
    async fn put_delete_patch_all_enforced() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::put().to(ok_handler))
                .route("/api/test", web::patch().to(ok_handler))
                .route("/api/test", web::delete().to(ok_handler)),
        )
        .await;

        for builder in [
            TestRequest::put(),
            TestRequest::patch(),
            TestRequest::delete(),
        ] {
            let req = builder.uri("/api/test").to_request();
            let resp = call_service(&app, req).await;
            assert_eq!(
                resp.status().as_u16(),
                403,
                "mutation without CSRF should be 403"
            );
        }
    }

    #[actix_web::test]
    async fn login_endpoint_bypasses_csrf() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/auth/login", web::post().to(ok_handler)),
        )
        .await;

        // No cookie, no header — login is exempt.
        let req = TestRequest::post().uri("/api/auth/login").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[actix_web::test]
    async fn register_endpoint_bypasses_csrf() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/auth/register", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post().uri("/api/auth/register").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    /// Public invitation-validate (TODO [16]) is called pre-registration,
    /// before any cookie exists, so it must bypass CSRF in the same way
    /// login/register do.
    #[actix_web::test]
    async fn invitation_validate_endpoint_bypasses_csrf() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/invitations/validate", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post()
            .uri("/api/invitations/validate")
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[actix_web::test]
    async fn rejection_body_uses_standard_error_envelope() {
        let app = init_service(
            App::new()
                .wrap(CsrfProtection)
                .route("/api/test", web::post().to(ok_handler)),
        )
        .await;

        let req = TestRequest::post().uri("/api/test").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);

        let body = actix_web::test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).expect("body is JSON");
        assert_eq!(json["error"]["code"], "FORBIDDEN");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("csrf"));
    }

    #[test]
    fn constant_time_eq_basic_cases() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn generate_csrf_token_is_64_hex_chars() {
        let t = generate_csrf_token();
        assert_eq!(t.len(), 64, "32 bytes hex-encoded → 64 chars");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_csrf_token_is_unique_across_calls() {
        // 100 fresh tokens with 256 bits of entropy — collisions are
        // effectively impossible. If this ever fails, OsRng is broken.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            assert!(seen.insert(generate_csrf_token()));
        }
    }

    #[test]
    fn build_csrf_cookie_attributes_match_spec() {
        let cfg = Config {
            database_url: "x".into(),
            redis_url: "x".into(),
            jwt_private_key_path: "x".into(),
            jwt_public_key_path: "x".into(),
            jwt_access_ttl_seconds: 3600,
            jwt_refresh_ttl_seconds: 604800,
            s3_endpoint: "x".into(),
            s3_access_key: "x".into(),
            s3_secret_key: "x".into(),
            s3_region: "us-east-1".into(),
            s3_bucket_avatars: "avatars".into(),
            s3_bucket_emojis: "emojis".into(),
            s3_bucket_exports: "exports".into(),
            bind_addr: "0.0.0.0:8080".into(),
            frontend_origin: "http://localhost:3000".into(),
            cookie_domain: "example.com".into(),
            cookie_secure: true,
        };
        let cookie = build_csrf_cookie("token-value".into(), &cfg);

        assert_eq!(cookie.name(), CSRF_COOKIE_NAME);
        assert_eq!(cookie.value(), "token-value");
        assert_eq!(cookie.http_only(), Some(false), "SPA must read this cookie");
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.domain(), Some("example.com"));
        assert_eq!(cookie.max_age(), Some(CookieDuration::seconds(3600)));
    }
}
