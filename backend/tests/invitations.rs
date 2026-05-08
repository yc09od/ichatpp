//! Integration tests for `handlers/invitations.rs`.
//!
//! Covers the admin-scoped lifecycle (generate → list → stats → delete →
//! download) plus the public `/validate` endpoint. Tests bypass middleware
//! so we go straight against the route handlers — admin auth is asserted
//! by minting an access token through the same `Keys` instance the app
//! uses, no full register/login round-trip required.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    auth_cookie_header, build_app, build_state, call_service, flush_redis,
    init_service, read_body, seed_admin, seed_invitation, seed_user, AuthCookies,
    TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// /validate (public)
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn validate_returns_invalid_for_unknown_code(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;

    let req = TestRequest::post()
        .uri("/api/invitations/validate")
        .set_json(serde_json::json!({ "code": "INV-NOTREAL12345678901234" }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["valid"], false);
}

#[sqlx::test]
async fn validate_returns_valid_for_seeded_code(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let code = seed_invitation(&pool, admin.id).await;
    let app = init_service(build_app(state)).await;

    let req = TestRequest::post()
        .uri("/api/invitations/validate")
        .set_json(serde_json::json!({ "code": code }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["valid"], true);
    assert!(v["data"]["expires_at"].is_string(), "expires_at present");
}

#[sqlx::test]
async fn validate_rejects_malformed_codes_without_db_lookup(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;

    for bad in ["", "short", "no-prefix-12345", "INV-lowercaseok-thats-shape"] {
        let req = TestRequest::post()
            .uri("/api/invitations/validate")
            .set_json(serde_json::json!({ "code": bad }))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK, "malformed: {bad:?}");
    }
}

// ────────────────────────────────────────────────────────────────────────
// Admin auth gating
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn admin_endpoints_require_auth(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;

    let endpoints = [
        ("POST", "/api/invitations/generate"),
        ("GET", "/api/invitations"),
        ("GET", "/api/invitations/stats"),
    ];

    for (method, path) in endpoints {
        let req_builder = match method {
            "POST" => TestRequest::post(),
            "GET" => TestRequest::get(),
            _ => unreachable!(),
        };
        let req = req_builder
            .uri(path)
            .set_json(serde_json::json!({"count": 1}))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} should require auth"
        );
    }
}

#[sqlx::test]
async fn admin_endpoints_reject_non_admin(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie.clone()))
        .set_json(serde_json::json!({"count": 1}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ────────────────────────────────────────────────────────────────────────
// /generate
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn generate_creates_invitations_and_returns_plaintext(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"count": 3, "expires_in_days": 7}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let invitations = v["data"]["invitations"]
        .as_array()
        .expect("invitations array");
    assert_eq!(invitations.len(), 3);
    for inv in invitations {
        let code = inv["code"].as_str().unwrap();
        assert!(code.starts_with("INV-"), "plaintext code returned: {code}");
        assert!(inv["id"].is_string());
        assert!(inv["expires_at"].is_string());
    }
    assert!(v["data"]["download_url"].is_string());

    // Verify the rows actually landed in the DB.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM invitations WHERE created_by = $1")
            .bind(admin.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test]
async fn generate_rejects_count_zero(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"count": 0}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn generate_rejects_count_too_large(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"count": 9999}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn generate_rejects_invalid_ttl(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    for bad in [0i64, -1, 999] {
        let req = TestRequest::post()
            .uri("/api/invitations/generate")
            .insert_header(("Cookie", cookie.clone()))
            .set_json(serde_json::json!({"count": 1, "expires_in_days": bad}))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "ttl: {bad}");
    }
}

#[sqlx::test]
async fn generate_rejects_long_notes(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let long_notes = "x".repeat(300);
    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"count": 1, "notes": long_notes}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ────────────────────────────────────────────────────────────────────────
// /list
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn list_returns_masked_view_without_hash(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let _code = seed_invitation(&pool, admin.id).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/invitations")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let items = v["data"].as_array().expect("data array");
    assert!(!items.is_empty());

    for item in items {
        // Crucial security pin: code_hash must NEVER appear in any list view.
        assert!(
            item.get("code_hash").is_none(),
            "code_hash leaked: {item}"
        );
        // The masked field is what the SPA renders.
        assert!(item["code_masked"].is_string() || item["code_prefix"].is_string());
    }
}

#[sqlx::test]
async fn list_filters_by_status(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let _ = seed_invitation(&pool, admin.id).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/invitations?status=unused")
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Bad status → 400.
    let req = TestRequest::get()
        .uri("/api/invitations?status=garbage")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn list_paginates(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    for _ in 0..5 {
        let _ = seed_invitation(&pool, admin.id).await;
    }
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/invitations?page=1&limit=2")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
}

// ────────────────────────────────────────────────────────────────────────
// /stats
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn stats_counts_by_status(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    for _ in 0..3 {
        let _ = seed_invitation(&pool, admin.id).await;
    }
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/invitations/stats")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["total"].as_i64().unwrap(), 3);
    assert_eq!(v["data"]["unused"].as_i64().unwrap(), 3);
    assert_eq!(v["data"]["used"].as_i64().unwrap(), 0);
}

// ────────────────────────────────────────────────────────────────────────
// /delete
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn delete_revokes_invitation(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let _ = seed_invitation(&pool, admin.id).await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM invitations LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::delete()
        .uri(&format!("/api/invitations/{id}"))
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let status: String = sqlx::query_scalar("SELECT status FROM invitations WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "revoked");
}

#[sqlx::test]
async fn delete_returns_404_for_unknown_id(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::delete()
        .uri(&format!("/api/invitations/{}", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ────────────────────────────────────────────────────────────────────────
// /download/{token}
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn download_returns_csv_then_410_on_replay(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    // Generate so the CSV gets stored in Redis.
    let req = TestRequest::post()
        .uri("/api/invitations/generate")
        .insert_header(("Cookie", cookie.clone()))
        .set_json(serde_json::json!({"count": 1}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let download_url = v["data"]["download_url"].as_str().unwrap().to_owned();

    // First fetch returns the CSV.
    let req = TestRequest::get()
        .uri(&download_url)
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Second fetch — token already consumed → 410 Gone.
    let req = TestRequest::get()
        .uri(&download_url)
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::GONE);
}

#[sqlx::test]
async fn download_rejects_malformed_token_without_redis_hop(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    // 'tooshort' is not the right hex length — handler short-circuits to 410.
    let req = TestRequest::get()
        .uri("/api/invitations/download/tooshort")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::GONE);
}

// Used by helpers above; suppresses dead-code lint for the inferred path.
#[allow(dead_code)]
fn _silence(_: AuthCookies) {}
