//! Auth cookie helpers — write/clear the three-cookie set the SPA expects.
//!
//! Per ARCHITECTURE.md §4.1:
//! - `access_token`  : httpOnly, Secure, SameSite=Lax, Max-Age=3600,   Path=/
//! - `refresh_token` : httpOnly, Secure, SameSite=Lax, Max-Age=604800, Path=/api/auth/refresh
//! - `csrf_token`    : non-httpOnly, Secure, SameSite=Lax, Max-Age=3600, Path=/
//!
//! The CSRF cookie is built by [`crate::middleware::build_csrf_cookie`] —
//! we only re-export the pattern here so login / register / refresh
//! handlers (TODOs [17]/[18]/[19]) have a single call site.

#![allow(dead_code)] // consumed by handlers landing in TODOs [17]/[18]/[19]

use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::HttpResponseBuilder;

use crate::config::Config;
use crate::middleware::{build_csrf_cookie, CSRF_COOKIE_NAME};

pub const ACCESS_COOKIE_NAME: &str = "access_token";
pub const REFRESH_COOKIE_NAME: &str = "refresh_token";

/// Path scope for the refresh cookie. Restricting it to the refresh endpoint
/// shrinks the request surface where it can leak (e.g. CSRF chains, log
/// captures of unrelated endpoints).
pub const REFRESH_COOKIE_PATH: &str = "/api/auth/refresh";

/// The triple emitted on a successful authentication. `csrf` is `None` for
/// `/api/auth/refresh` (per ARCHITECTURE.md §4.1: refresh re-issues access +
/// csrf; the refresh cookie itself is unchanged).
pub struct AuthCookies {
    pub access: String,
    pub refresh: Option<String>,
    pub csrf: Option<String>,
}

/// Attach access / refresh / csrf cookies to `builder`. Cookies whose values
/// are `None` are skipped — the partial-set case (refresh endpoint) reuses
/// this same helper.
pub fn set_auth_cookies(
    builder: &mut HttpResponseBuilder,
    cookies: &AuthCookies,
    config: &Config,
) {
    builder.cookie(build_access_cookie(cookies.access.clone(), config));

    if let Some(refresh) = cookies.refresh.clone() {
        builder.cookie(build_refresh_cookie(refresh, config));
    }
    if let Some(csrf) = cookies.csrf.clone() {
        builder.cookie(build_csrf_cookie(csrf, config));
    }
}

/// Set `Set-Cookie` headers that immediately expire all three auth cookies.
/// Used by `/api/auth/logout` (TODO [19]).
///
/// Each cleared cookie must use the *same* `Path` / `Domain` as when set,
/// otherwise the browser keeps the original.
pub fn clear_auth_cookies(builder: &mut HttpResponseBuilder, config: &Config) {
    builder.cookie(expire_cookie(ACCESS_COOKIE_NAME, "/", config));
    builder.cookie(expire_cookie(
        REFRESH_COOKIE_NAME,
        REFRESH_COOKIE_PATH,
        config,
    ));
    builder.cookie(expire_cookie(CSRF_COOKIE_NAME, "/", config));
}

fn build_access_cookie(value: String, config: &Config) -> Cookie<'static> {
    Cookie::build(ACCESS_COOKIE_NAME, value)
        .http_only(true)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(CookieDuration::seconds(config.jwt_access_ttl_seconds))
        .domain(config.cookie_domain.clone())
        .finish()
}

fn build_refresh_cookie(value: String, config: &Config) -> Cookie<'static> {
    Cookie::build(REFRESH_COOKIE_NAME, value)
        .http_only(true)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .path(REFRESH_COOKIE_PATH)
        .max_age(CookieDuration::seconds(config.jwt_refresh_ttl_seconds))
        .domain(config.cookie_domain.clone())
        .finish()
}

fn expire_cookie(name: &'static str, path: &'static str, config: &Config) -> Cookie<'static> {
    // Empty value + Max-Age=0 is the canonical "delete" cookie. We must
    // mirror domain + path or the browser treats it as a different cookie.
    Cookie::build(name, "")
        .http_only(true)
        .secure(config.cookie_secure)
        .same_site(SameSite::Lax)
        .path(path)
        .max_age(CookieDuration::seconds(0))
        .domain(config.cookie_domain.clone())
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::header::SET_COOKIE;
    use actix_web::HttpResponse;

    fn test_config(secure: bool) -> Config {
        Config {
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
            cookie_secure: secure,
        }
    }

    fn collected_set_cookies(resp: HttpResponse) -> Vec<String> {
        resp.headers()
            .get_all(SET_COOKIE)
            .map(|v| v.to_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn set_auth_cookies_emits_all_three_on_login() {
        let cfg = test_config(true);
        let cookies = AuthCookies {
            access: "ACCESS_JWT".into(),
            refresh: Some("REFRESH_JWT".into()),
            csrf: Some("CSRF_HEX".into()),
        };

        let mut builder = HttpResponse::Ok();
        set_auth_cookies(&mut builder, &cookies, &cfg);
        let resp = builder.finish();
        let headers: Vec<String> = collected_set_cookies(resp);

        assert_eq!(headers.len(), 3, "expected 3 Set-Cookie headers");

        let access = headers
            .iter()
            .find(|h| h.starts_with("access_token="))
            .expect("access cookie present");
        assert!(access.contains("ACCESS_JWT"));
        assert!(access.contains("HttpOnly"));
        assert!(access.contains("Secure"));
        assert!(access.contains("SameSite=Lax"));
        assert!(access.contains("Path=/"));
        assert!(access.contains("Max-Age=3600"));
        assert!(access.contains("Domain=example.com"));

        let refresh = headers
            .iter()
            .find(|h| h.starts_with("refresh_token="))
            .expect("refresh cookie present");
        assert!(refresh.contains("REFRESH_JWT"));
        assert!(refresh.contains("HttpOnly"));
        assert!(refresh.contains("Secure"));
        assert!(refresh.contains("Path=/api/auth/refresh"));
        assert!(refresh.contains("Max-Age=604800"));

        let csrf = headers
            .iter()
            .find(|h| h.starts_with("csrf_token="))
            .expect("csrf cookie present");
        assert!(csrf.contains("CSRF_HEX"));
        assert!(
            !csrf.contains("HttpOnly"),
            "csrf cookie must be readable by JS, got: {csrf}"
        );
        assert!(csrf.contains("Secure"));
        assert!(csrf.contains("SameSite=Lax"));
        assert!(csrf.contains("Path=/"));
        assert!(csrf.contains("Max-Age=3600"));
    }

    #[test]
    fn set_auth_cookies_supports_partial_refresh_only_access_and_csrf() {
        // Refresh endpoint use case: re-issue access + csrf, leave refresh.
        let cfg = test_config(true);
        let cookies = AuthCookies {
            access: "NEW_ACCESS".into(),
            refresh: None,
            csrf: Some("NEW_CSRF".into()),
        };

        let mut builder = HttpResponse::Ok();
        set_auth_cookies(&mut builder, &cookies, &cfg);
        let headers = collected_set_cookies(builder.finish());

        assert_eq!(headers.len(), 2);
        assert!(headers.iter().any(|h| h.starts_with("access_token=NEW_ACCESS")));
        assert!(headers.iter().any(|h| h.starts_with("csrf_token=NEW_CSRF")));
        assert!(!headers.iter().any(|h| h.starts_with("refresh_token=")));
    }

    #[test]
    fn cookie_secure_flag_follows_config() {
        // In dev (HTTP), the Secure flag must be off or browsers will drop
        // the cookie entirely.
        let cfg = test_config(false);
        let cookies = AuthCookies {
            access: "x".into(),
            refresh: Some("y".into()),
            csrf: Some("z".into()),
        };
        let mut builder = HttpResponse::Ok();
        set_auth_cookies(&mut builder, &cookies, &cfg);
        let headers = collected_set_cookies(builder.finish());

        for h in &headers {
            assert!(!h.contains("Secure"), "no Secure flag in dev mode: {h}");
        }
    }

    #[test]
    fn clear_auth_cookies_emits_three_zero_age_headers() {
        let cfg = test_config(true);
        let mut builder = HttpResponse::Ok();
        clear_auth_cookies(&mut builder, &cfg);
        let headers = collected_set_cookies(builder.finish());

        assert_eq!(headers.len(), 3);

        // Each clearing cookie must mirror the original Path so the browser
        // matches and overwrites — not creates a sibling.
        let refresh = headers
            .iter()
            .find(|h| h.starts_with("refresh_token="))
            .expect("refresh clear present");
        assert!(refresh.contains("Path=/api/auth/refresh"));
        assert!(refresh.contains("Max-Age=0"));

        for h in &headers {
            assert!(h.contains("Max-Age=0"), "expected expiry 0 on {h}");
            assert!(h.contains("Domain=example.com"));
        }
    }
}
