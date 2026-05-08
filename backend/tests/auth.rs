//! Integration tests for `handlers/auth.rs`.
//!
//! Exercises register / login / refresh / logout end-to-end. Each test is
//! gated on its own fresh Postgres DB via `#[sqlx::test]`; Redis is shared
//! and explicitly flushed at the top of any test that depends on lockout
//! counters or refresh-whitelist state.
//!
//! bcrypt at cost 12 dominates wall-clock time on the registration paths,
//! so we batch related assertions into one `#[sqlx::test]` rather than
//! spinning up 5 separate registers.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;

use common::{
    auth_cookie_header, build_app, build_state, call_service, cookie_value,
    flush_redis, init_service, read_body, seed_admin, seed_invitation, seed_user,
    AuthCookies, TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// /register
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn register_emits_three_cookies_and_persists_user(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let admin = seed_admin(&pool).await;
    let code = seed_invitation(&pool, admin.id).await;

    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({
            "email": "Alice@Example.Com",
            "password": "Password123",
            "invitation_code": code,
            "nickname": "Alice"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // All three auth cookies set.
    assert!(cookie_value(&resp, "access_token").is_some());
    assert!(cookie_value(&resp, "refresh_token").is_some());
    assert!(cookie_value(&resp, "csrf_token").is_some());

    // Body carries no token material — only public ids.
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert!(v["data"]["user_id"].is_string());
    assert!(v["data"]["account_code"].is_string());
    let body_str = serde_json::to_string(&v).unwrap();
    assert!(!body_str.contains("eyJ"), "no JWT in body");

    // Email is normalised to lowercase + trimmed.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT email FROM users WHERE email = 'alice@example.com'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("alice@example.com"));

    // Invitation flipped to 'used'.
    let inv_status: String = sqlx::query_scalar(
        "SELECT status FROM invitations WHERE used_by IS NOT NULL LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inv_status, "used");
}

#[sqlx::test]
async fn register_rejects_used_invitation_with_409(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let admin = seed_admin(&pool).await;
    let code = seed_invitation(&pool, admin.id).await;
    // Pre-mark the invitation used (any user_id will do).
    sqlx::query(
        r#"UPDATE invitations SET status='used', used_by=$1, used_at=NOW()
           WHERE created_by=$2"#,
    )
    .bind(admin.id)
    .bind(admin.id)
    .execute(&pool)
    .await
    .unwrap();

    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({
            "email": "alice@example.com",
            "password": "Password123",
            "invitation_code": code,
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn register_rejects_unknown_invitation_with_400(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({
            "email": "alice@example.com",
            "password": "Password123",
            "invitation_code": "INV-NOPE0123456789012345"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn register_validates_email_password_format(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let code = seed_invitation(&pool, admin.id).await;
    let app = init_service(build_app(state)).await;

    let bad_inputs = [
        // bad email
        serde_json::json!({"email": "not-an-email", "password": "Password123", "invitation_code": code}),
        // password too short
        serde_json::json!({"email": "ok@ok.io", "password": "short", "invitation_code": code}),
        // empty invitation
        serde_json::json!({"email": "ok2@ok.io", "password": "Password123", "invitation_code": ""}),
    ];

    for body in bad_inputs {
        let req = TestRequest::post()
            .uri("/api/auth/register")
            .set_json(&body)
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body: {body}");
    }
}

#[sqlx::test]
async fn register_rejects_duplicate_email_with_409(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let admin = seed_admin(&pool).await;

    // Existing user with email
    let existing = seed_user(&pool, Some("dup@example.com")).await;
    let _ = existing;

    let code = seed_invitation(&pool, admin.id).await;
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({
            "email": "DUP@example.com",
            "password": "Password123",
            "invitation_code": code,
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ────────────────────────────────────────────────────────────────────────
// /login
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn login_succeeds_with_correct_password(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, Some("login@test.io")).await;
    let app = init_service(build_app(state)).await;

    let req = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": user.email,
            "password": "Password123"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(cookie_value(&resp, "access_token").is_some());
    assert!(cookie_value(&resp, "refresh_token").is_some());
    assert!(cookie_value(&resp, "csrf_token").is_some());

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["user_id"], user.id.to_string());
}

#[sqlx::test]
async fn login_returns_401_for_wrong_password(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, Some("wrong@test.io")).await;
    let app = init_service(build_app(state)).await;

    let req = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": user.email,
            "password": "WRONG-password"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn login_returns_401_for_unknown_email(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": "ghost@nowhere.invalid",
            "password": "anything"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn login_rejects_empty_or_oversize_inputs(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;

    for body in [
        serde_json::json!({"email": "", "password": "x"}),
        serde_json::json!({"email": "a@b.c", "password": ""}),
        serde_json::json!({"email": "a".repeat(500), "password": "x"}),
    ] {
        let req = TestRequest::post()
            .uri("/api/auth/login")
            .set_json(&body)
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "body: {body}");
    }
}

#[sqlx::test]
async fn login_locks_after_five_failures(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, Some("lockout@test.io")).await;
    let app = init_service(build_app(state)).await;

    // Five wrong attempts → counter hits MAX_LOGIN_FAILURES.
    for _ in 0..5 {
        let req = TestRequest::post()
            .uri("/api/auth/login")
            .set_json(serde_json::json!({
                "email": user.email,
                "password": "WRONG"
            }))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // Sixth attempt with the *correct* password is denied — account is locked.
    let req = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": user.email,
            "password": "Password123"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ────────────────────────────────────────────────────────────────────────
// /refresh
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn refresh_returns_new_access_cookie_with_valid_refresh(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, Some("refresh@test.io")).await;
    let app = init_service(build_app(state)).await;

    // Login to seed the whitelist entry for the refresh token.
    let login = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": user.email,
            "password": "Password123"
        }))
        .to_request();
    let login_resp = call_service(&app, login).await;
    let cookies = AuthCookies::from_response(&login_resp);

    // Use the refresh cookie.
    let refresh_cookie = format!(
        "refresh_token={}",
        cookies.refresh_token.as_deref().unwrap()
    );
    let req = TestRequest::post()
        .uri("/api/auth/refresh")
        .insert_header(("Cookie", refresh_cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(cookie_value(&resp, "access_token").is_some());
    // Refresh endpoint does NOT re-issue the refresh cookie.
    assert!(cookie_value(&resp, "refresh_token").is_none());
    assert!(cookie_value(&resp, "csrf_token").is_some());
}

#[sqlx::test]
async fn refresh_returns_401_without_cookie(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/refresh")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn refresh_returns_401_for_garbage_token(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/auth/refresh")
        .insert_header(("Cookie", "refresh_token=not-a-real-jwt"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ────────────────────────────────────────────────────────────────────────
// /logout
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn logout_clears_cookies_and_revokes_refresh(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;

    // Login first to get cookies + Redis whitelist entry.
    let login = TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({
            "email": user.email,
            "password": "Password123"
        }))
        .to_request();
    let login_resp = call_service(&app, login).await;
    let cookies = AuthCookies::from_response(&login_resp);

    // Logout with the access cookie.
    let access_header = format!("access_token={}", cookies.access_token);
    let logout = TestRequest::post()
        .uri("/api/auth/logout")
        .insert_header(("Cookie", access_header))
        .to_request();
    let resp = call_service(&app, logout).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // After logout, refresh must 401 — the whitelist entry is gone.
    let refresh_header = format!(
        "refresh_token={}",
        cookies.refresh_token.as_deref().unwrap()
    );
    let after = TestRequest::post()
        .uri("/api/auth/refresh")
        .insert_header(("Cookie", refresh_header))
        .to_request();
    let after_resp = call_service(&app, after).await;
    assert_eq!(after_resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn logout_returns_401_without_access_cookie(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post().uri("/api/auth/logout").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// Bypass dead-code lint for helpers we re-export but use only conditionally.
#[allow(dead_code)]
fn _silence(_: &str) {
    auth_cookie_header;
}
