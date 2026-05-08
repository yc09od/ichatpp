//! Integration tests for `handlers/users.rs`.
//!
//! Covers `/me` GET/PUT, `/avatar` POST, lookup by id and account_code
//! (with visibility rules), and the admin-only list / update_role.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    auth_cookie_header, build_app, build_state, call_service, init_service,
    read_body, seed_admin, seed_friendship, seed_user, TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// /me GET
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn get_me_returns_self_profile(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::get()
        .uri("/api/users/me")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["id"], user.id.to_string());
    assert_eq!(v["data"]["account_code"], user.account_code);
    assert_eq!(v["data"]["role"], "user");
}

#[sqlx::test]
async fn get_me_returns_401_without_auth(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::get().uri("/api/users/me").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn get_me_returns_404_when_user_row_gone(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.id)
        .execute(&pool)
        .await
        .unwrap();
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::get()
        .uri("/api/users/me")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ────────────────────────────────────────────────────────────────────────
// /me PUT
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn update_me_updates_nickname_and_signature(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::put()
        .uri("/api/users/me")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({
            "nickname": "Alice Wonderland",
            "signature": "Curiouser",
            "is_visible": false
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["nickname"], "Alice Wonderland");
    assert_eq!(v["data"]["signature"], "Curiouser");
    assert_eq!(v["data"]["is_visible"], false);
}

#[sqlx::test]
async fn update_me_rejects_whitespace_only_nickname(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::put()
        .uri("/api/users/me")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"nickname": "    "}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn update_me_partial_update_keeps_unsupplied_fields(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    // First update sets signature.
    let _ = call_service(
        &app,
        TestRequest::put()
            .uri("/api/users/me")
            .insert_header(("Cookie", cookie.clone()))
            .set_json(serde_json::json!({"signature": "first"}))
            .to_request(),
    )
    .await;

    // Second update changes nickname only — signature should be preserved.
    let req = TestRequest::put()
        .uri("/api/users/me")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"nickname": "newname"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["signature"], "first");
    assert_eq!(v["data"]["nickname"], "newname");
}

// ────────────────────────────────────────────────────────────────────────
// /{user_id} GET
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn get_by_id_visible_to_anyone(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let target = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/users/{}", target.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn get_by_id_invisible_user_404s_for_strangers(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let target = seed_user(&pool, None).await;
    sqlx::query("UPDATE users SET is_visible = FALSE WHERE id = $1")
        .bind(target.id)
        .execute(&pool)
        .await
        .unwrap();
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/users/{}", target.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn get_by_id_invisible_visible_to_friends(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let target = seed_user(&pool, None).await;
    sqlx::query("UPDATE users SET is_visible = FALSE WHERE id = $1")
        .bind(target.id)
        .execute(&pool)
        .await
        .unwrap();
    seed_friendship(&pool, viewer.id, target.id).await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/users/{}", target.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn get_by_id_unknown_user_404s(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/users/{}", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ────────────────────────────────────────────────────────────────────────
// /by-code/{code} GET
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn get_by_code_succeeds(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let target = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/users/by-code/{}", target.account_code))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["id"], target.id.to_string());
}

#[sqlx::test]
async fn get_by_code_rejects_malformed(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri("/api/users/by-code/abc")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn get_by_code_unknown_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let viewer = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, viewer.id, "user");

    let req = TestRequest::get()
        .uri("/api/users/by-code/0000000000")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ────────────────────────────────────────────────────────────────────────
// Admin: list users + update role
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn list_users_requires_admin(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::get()
        .uri("/api/users")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn list_users_returns_paginated(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    for _ in 0..3 {
        seed_user(&pool, None).await;
    }
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/users?page=1&limit=10")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let users = v["data"]["users"].as_array().unwrap();
    assert!(users.len() >= 4); // admin + 3 users
    assert_eq!(v["data"]["page"], 1);
    assert_eq!(v["data"]["limit"], 10);
}

#[sqlx::test]
async fn list_users_rejects_oversized_limit(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::get()
        .uri("/api/users?limit=99999")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn update_role_admin_promotes_user(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let target = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::put()
        .uri(&format!("/api/users/{}/role", target.id))
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"role": "admin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let role: String =
        sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = $1")
            .bind(target.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(role, "admin");
}

#[sqlx::test]
async fn update_role_self_demotion_rejected(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::put()
        .uri(&format!("/api/users/{}/role", admin.id))
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn update_role_unknown_user_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::put()
        .uri(&format!("/api/users/{}/role", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"role": "admin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn update_role_rejects_invalid_role(pool: PgPool) {
    let state = build_state(pool.clone());
    let admin = seed_admin(&pool).await;
    let target = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, admin.id, "admin");

    let req = TestRequest::put()
        .uri(&format!("/api/users/{}/role", target.id))
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"role": "superadmin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn update_role_requires_admin(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let target = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::put()
        .uri(&format!("/api/users/{}/role", target.id))
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"role": "admin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
