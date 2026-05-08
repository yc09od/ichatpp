//! HTTP middleware builders: CORS, rate limiting, request logging, CSRF.
//!
//! All factories take `&Config` so the wiring stays out of `main.rs`.

mod csrf;

// Helpers consumed by the auth handlers landing in TODOs [12]/[17]/[18];
// re-export now so wiring later only needs `use crate::middleware::...`.
#[allow(unused_imports)]
pub use csrf::{
    build_csrf_cookie, generate_csrf_token, CsrfProtection, CSRF_COOKIE_NAME, CSRF_HEADER_NAME,
};

use actix_cors::Cors;
use actix_governor::{
    governor::middleware::NoOpMiddleware, GovernorConfig, GovernorConfigBuilder,
    PeerIpKeyExtractor,
};
use actix_web::http::header;
use actix_web::middleware::Logger;

use crate::config::Config;

/// Concrete `GovernorConfig` produced by both helper builders below.
pub type StandardGovernorConfig = GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware>;

/// `actix-web`'s built-in access logger. Emits one line per request:
/// `<peer> "METHOD path HTTP/1.x" status size <duration_ms>ms`.
pub fn request_logger() -> Logger {
    Logger::new(r#"%a "%r" %s %b %Dms"#)
}

/// CORS policy — credentials-aware, no wildcards.
///
/// Per ARCHITECTURE.md §6 we cannot send `Access-Control-Allow-Origin: *`
/// alongside `Allow-Credentials: true` (browsers reject it), so the allowed
/// origin is taken verbatim from `FRONTEND_ORIGIN`. `X-CSRF-Token` must be
/// listed because it is a non-simple header and would otherwise fail
/// preflight.
pub fn cors(config: &Config) -> Cors {
    Cors::default()
        .allowed_origin(&config.frontend_origin)
        .allowed_methods(["GET", "POST", "PUT", "PATCH", "DELETE"])
        .allowed_headers([header::CONTENT_TYPE, header::ACCEPT])
        .allowed_header("X-CSRF-Token")
        .expose_headers([header::CONTENT_TYPE])
        .supports_credentials()
        .max_age(3600)
}

/// Global per-IP rate limit — generous, only meant to slow obvious abuse.
/// ~100 req/min/IP (1 token / 600 ms, burst 100).
pub fn global_governor_config() -> StandardGovernorConfig {
    GovernorConfigBuilder::default()
        .milliseconds_per_request(600)
        .burst_size(100)
        .finish()
        .expect("global governor: invalid configuration")
}

/// Tight per-IP rate limit for `/api/auth/*`: 10 req/min/IP per
/// ARCHITECTURE.md §6. Burst 10, refills 1 token / 6 s — so 11 quick
/// requests yield a 429 on the 11th.
pub fn auth_governor_config() -> StandardGovernorConfig {
    GovernorConfigBuilder::default()
        .seconds_per_request(6)
        .burst_size(10)
        .finish()
        .expect("auth governor: invalid configuration")
}

/// Per-IP rate limit for `POST /api/invitations/validate` (TODO [16]):
/// 30 req/min/IP. Burst 30, refills 1 token / 2 s. Tighter than the
/// global limit so an attacker can't trivially turn the validate
/// endpoint into a code-guessing oracle, looser than the auth limit so
/// the SPA's pre-submit check on the registration form is responsive.
pub fn invitation_validate_governor_config() -> StandardGovernorConfig {
    GovernorConfigBuilder::default()
        .seconds_per_request(2)
        .burst_size(30)
        .finish()
        .expect("validate governor: invalid configuration")
}
