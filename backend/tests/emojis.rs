//! Integration tests for `handlers/emojis.rs`.
//!
//! Covers upload (multipart), list (with Redis cache), get_by_id, and
//! delete. Uploads hit the live MinIO at the docker-compose endpoint —
//! tests assume the `emojis` bucket exists (created by the docker-compose
//! `minio-init` one-shot).

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    auth_cookie_header, build_app, build_state, call_service, flush_redis,
    init_service, multipart_file_body, read_body, seed_user, tiny_png_bytes,
    TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// /api/emojis (list)
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn list_returns_empty_for_new_user(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::get()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["emojis"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn list_requires_auth(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::get().uri("/api/emojis").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ────────────────────────────────────────────────────────────────────────
// /api/emojis (upload + delete) — exercises MinIO end-to-end
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn upload_emoji_round_trip(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let png = tiny_png_bytes();
    let (content_type, body) = multipart_file_body(
        "file",
        "happy.png",
        "image/png",
        &png,
        &[("name", "happy")],
    );

    // Upload.
    let req = TestRequest::post()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie.clone()))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let emoji = &v["data"]["emoji"];
    let id = emoji["id"].as_str().unwrap().to_owned();
    assert_eq!(emoji["name"], "happy");
    assert!(emoji["file_url"].as_str().unwrap().contains("/emojis/"));
    assert!(emoji["thumbnail_url"].is_string());

    // List should now include it.
    let req = TestRequest::get()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let emojis = v["data"]["emojis"].as_array().unwrap();
    assert_eq!(emojis.len(), 1);
    assert_eq!(emojis[0]["id"], id);

    // Get-by-id (any authenticated user can resolve).
    let req = TestRequest::get()
        .uri(&format!("/api/emojis/{id}"))
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["id"], id);
    // user_id must NOT leak in by-id lookup either.
    assert!(v["data"].as_object().unwrap().get("user_id").is_none());

    // Delete.
    let req = TestRequest::delete()
        .uri(&format!("/api/emojis/{id}"))
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // List should now be empty again.
    let req = TestRequest::get()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["emojis"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn upload_rejects_non_image_payload(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let (content_type, body) = multipart_file_body(
        "file",
        "fake.png",
        "image/png",
        b"this-is-not-a-png-just-text",
        &[],
    );
    let req = TestRequest::post()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn upload_rejects_missing_file_field(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let (content_type, body) = multipart_file_body(
        "wrong_field",
        "x.png",
        "image/png",
        &tiny_png_bytes(),
        &[],
    );
    let req = TestRequest::post()
        .uri("/api/emojis")
        .insert_header(("Cookie", cookie))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn upload_requires_auth(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;

    let (content_type, body) = multipart_file_body(
        "file",
        "x.png",
        "image/png",
        &tiny_png_bytes(),
        &[],
    );
    let req = TestRequest::post()
        .uri("/api/emojis")
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ────────────────────────────────────────────────────────────────────────
// /api/emojis/{id}
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn get_by_id_unknown_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/emojis/{}", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn delete_unknown_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let user = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, user.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/emojis/{}", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn delete_by_other_user_returns_404(pool: PgPool) {
    // Existence-leakage prevention: a user trying to delete someone else's
    // emoji must see 404, not 403.
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let owner = seed_user(&pool, None).await;
    let stranger = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let owner_cookie = auth_cookie_header(&state, owner.id, "user");

    // Owner uploads.
    let (ct, body) = multipart_file_body(
        "file",
        "x.png",
        "image/png",
        &tiny_png_bytes(),
        &[],
    );
    let req = TestRequest::post()
        .uri("/api/emojis")
        .insert_header(("Cookie", owner_cookie))
        .insert_header(("Content-Type", ct))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let id = v["data"]["emoji"]["id"].as_str().unwrap().to_owned();

    // Stranger tries to delete.
    let stranger_cookie = auth_cookie_header(&state, stranger.id, "user");
    let req = TestRequest::delete()
        .uri(&format!("/api/emojis/{id}"))
        .insert_header(("Cookie", stranger_cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
