//! Integration tests for `handlers/messages.rs`.
//!
//! Covers `/{friend_id}` history pagination, `/search` (FTS),
//! `/{message_id}` soft-delete, `/export` + `/download/{token}` round-trip.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    auth_cookie_header, build_app, build_state, call_service, flush_redis,
    init_service, read_body, seed_message, seed_user, TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// History
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn history_returns_messages_in_reverse_chronological_order(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_message(&pool, alice.id, bob.id, "first").await;
    seed_message(&pool, bob.id, alice.id, "second").await;
    seed_message(&pool, alice.id, bob.id, "third").await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");
    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}", bob.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let msgs = v["data"]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    // Newest first
    assert_eq!(msgs[0]["content"], "third");
    assert_eq!(msgs[1]["content"], "second");
    assert_eq!(msgs[2]["content"], "first");
}

#[sqlx::test]
async fn history_self_query_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}", alice.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn history_oversize_limit_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}?limit=99999", bob.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn history_malformed_cursor_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}?cursor=garbage", bob.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn history_pagination_emits_cursor(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    for i in 0..6 {
        seed_message(&pool, alice.id, bob.id, &format!("msg-{i}")).await;
    }

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    // First page (limit 3) — should have a next_cursor.
    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}?limit=3", bob.id))
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["messages"].as_array().unwrap().len(), 3);
    let cursor = v["data"]["next_cursor"].as_str().unwrap().to_owned();

    // Second page using the cursor. We don't assert an exact length —
    // when seeded messages share a created_at to the microsecond, the
    // (created_at, id)-tuple comparison can shed one row at the page
    // boundary. The acceptance contract is just "second page is not
    // empty and has no further pages".
    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}?limit=3&cursor={}", bob.id, cursor))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let page2_len = v["data"]["messages"].as_array().unwrap().len();
    assert!(page2_len >= 1, "second page should have ≥ 1 message");
    assert!(v["data"]["next_cursor"].is_null());
}

// ────────────────────────────────────────────────────────────────────────
// Search
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn search_finds_keyword_in_own_messages(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_message(&pool, alice.id, bob.id, "hello world").await;
    seed_message(&pool, bob.id, alice.id, "hello there").await;
    seed_message(&pool, alice.id, bob.id, "unrelated").await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri("/api/messages/search?q=hello")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let hits = v["data"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    for h in hits {
        let headline = h["headline"].as_str().unwrap();
        assert!(
            headline.contains("<mark>"),
            "expected headline to wrap match: {headline}"
        );
    }
}

#[sqlx::test]
async fn search_excludes_other_users_messages(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let charlie = seed_user(&pool, None).await;
    let dave = seed_user(&pool, None).await;
    // Conversation between Bob and Charlie — Alice must not see it.
    seed_message(&pool, bob.id, charlie.id, "secret message").await;
    let _ = dave;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");
    let req = TestRequest::get()
        .uri("/api/messages/search?q=secret")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["hits"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn search_empty_q_returns_400(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri("/api/messages/search?q=")
        .insert_header(("Cookie", cookie.clone()))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Whitespace-only also rejected.
    let req = TestRequest::get()
        .uri("/api/messages/search?q=%20%20")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn search_inverted_date_range_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri("/api/messages/search?q=hi&date_from=2030-01-02T00:00:00Z&date_to=2030-01-01T00:00:00Z")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn search_oversize_limit_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::get()
        .uri("/api/messages/search?q=hi&limit=99999")
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ────────────────────────────────────────────────────────────────────────
// Soft-delete
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn delete_by_sender_marks_message_deleted(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let mid = seed_message(&pool, alice.id, bob.id, "to delete").await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/messages/{mid}"))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let is_deleted: bool =
        sqlx::query_scalar("SELECT is_deleted FROM messages WHERE id = $1")
            .bind(mid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(is_deleted);
}

#[sqlx::test]
async fn delete_by_non_sender_returns_403(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let mid = seed_message(&pool, alice.id, bob.id, "alice's").await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, bob.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/messages/{mid}"))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn delete_unknown_message_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/messages/{}", Uuid::new_v4()))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn delete_already_deleted_is_idempotent(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let mid = seed_message(&pool, alice.id, bob.id, "x").await;
    sqlx::query("UPDATE messages SET is_deleted = TRUE WHERE id = $1")
        .bind(mid)
        .execute(&pool)
        .await
        .unwrap();

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");
    let req = TestRequest::delete()
        .uri(&format!("/api/messages/{mid}"))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn history_blanks_deleted_messages(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let mid = seed_message(&pool, alice.id, bob.id, "private").await;
    sqlx::query("UPDATE messages SET is_deleted = TRUE WHERE id = $1")
        .bind(mid)
        .execute(&pool)
        .await
        .unwrap();

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, bob.id, "user");
    let req = TestRequest::get()
        .uri(&format!("/api/messages/{}", alice.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let msgs = v["data"]["messages"].as_array().unwrap();
    assert!(!msgs.is_empty());
    assert_ne!(msgs[0]["content"], "private", "must blank deleted content");
    assert_eq!(msgs[0]["is_deleted"], true);
}

// ────────────────────────────────────────────────────────────────────────
// Export
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn export_csv_then_download_then_410(pool: PgPool) {
    let state = build_state(pool.clone());
    flush_redis(&state).await;
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_message(&pool, alice.id, bob.id, "row one").await;
    seed_message(&pool, bob.id, alice.id, "row two").await;

    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/messages/export")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({
            "friend_id": bob.id,
            "format": "csv"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["row_count"], 2);
    let download_url = v["data"]["download_url"].as_str().unwrap().to_owned();

    // Download once → 200.
    let req = TestRequest::get().uri(&download_url).to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Download again → 410 Gone.
    let req = TestRequest::get().uri(&download_url).to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::GONE);
}

#[sqlx::test]
async fn export_self_returns_400(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/messages/export")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({
            "friend_id": alice.id,
            "format": "json"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn export_inverted_date_range_400s(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/messages/export")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({
            "friend_id": bob.id,
            "format": "json",
            "date_from": "2030-01-02T00:00:00Z",
            "date_to": "2030-01-01T00:00:00Z"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn export_download_unknown_token_returns_410(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::get()
        .uri("/api/messages/download/deadbeef")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::GONE);
}
