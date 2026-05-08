//! Integration tests for `handlers/friends.rs`.
//!
//! Covers the friend-request lifecycle (POST → pending → PUT accept/reject)
//! and the friendship list/delete endpoints. Tests bypass middleware so we
//! can mint access tokens directly via `auth_cookie_header`.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use common::{
    auth_cookie_header, build_app, build_state, call_service, init_service,
    read_body, seed_friendship, seed_user, TestRequest,
};

// ────────────────────────────────────────────────────────────────────────
// POST /api/friends/requests
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn create_request_by_account_code_succeeds(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"account_code": bob.account_code}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(v["data"]["from_user_id"], alice.id.to_string());
    assert_eq!(v["data"]["to_user_id"], bob.id.to_string());
    assert_eq!(v["data"]["status"], "pending");
}

#[sqlx::test]
async fn create_request_by_user_id_succeeds(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[sqlx::test]
async fn create_request_to_self_fails(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"user_id": alice.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn create_request_with_invalid_account_code_fails(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"account_code": "abc"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn create_request_to_unknown_user_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"user_id": Uuid::new_v4()}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn create_request_when_already_friends_returns_409(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_friendship(&pool, alice.id, bob.id).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn create_duplicate_pending_returns_409_in_either_direction(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");
    let bob_cookie = auth_cookie_header(&state, bob.id, "user");

    // A → B (success)
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie.clone()))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // A → B again — same direction → 409
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // B → A — opposite direction with pending row → 409 (app-level check)
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", bob_cookie))
        .set_json(serde_json::json!({"user_id": alice.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn create_request_unauthenticated_returns_401(pool: PgPool) {
    let state = build_state(pool);
    let app = init_service(build_app(state)).await;
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .set_json(serde_json::json!({"account_code": "1234567890"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ────────────────────────────────────────────────────────────────────────
// GET /api/friends/requests/pending
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn list_pending_returns_inbound_only(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let charlie = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");
    let bob_cookie = auth_cookie_header(&state, bob.id, "user");

    // Bob → Alice
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", bob_cookie))
        .set_json(serde_json::json!({"user_id": alice.id}))
        .to_request();
    let _ = call_service(&app, req).await;

    // Alice → Charlie (outbound from Alice's POV)
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie.clone()))
        .set_json(serde_json::json!({"user_id": charlie.id}))
        .to_request();
    let _ = call_service(&app, req).await;

    // Alice's pending should only show Bob's inbound, not the outbound to Charlie.
    let req = TestRequest::get()
        .uri("/api/friends/requests/pending")
        .insert_header(("Cookie", alice_cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let reqs = v["data"]["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["from_user"]["id"], bob.id.to_string());
}

// ────────────────────────────────────────────────────────────────────────
// PUT /api/friends/requests/{id}
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn accept_creates_friendship_in_canonical_order(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");
    let bob_cookie = auth_cookie_header(&state, bob.id, "user");

    // Alice sends → request created
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let req_id = v["data"]["id"].as_str().unwrap().to_owned();

    // Bob accepts.
    let req = TestRequest::put()
        .uri(&format!("/api/friends/requests/{req_id}"))
        .insert_header(("Cookie", bob_cookie))
        .set_json(serde_json::json!({"action": "accept"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Friends row exists in canonical order.
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM friends
            WHERE user_id_1 = LEAST($1::uuid, $2::uuid)
              AND user_id_2 = GREATEST($1::uuid, $2::uuid)
        )
        "#,
    )
    .bind(alice.id)
    .bind(bob.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exists);
}

#[sqlx::test]
async fn reject_marks_request_rejected_without_friends_row(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");
    let bob_cookie = auth_cookie_header(&state, bob.id, "user");

    // Alice sends.
    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let req_id = v["data"]["id"].as_str().unwrap().to_owned();

    // Bob rejects.
    let req = TestRequest::put()
        .uri(&format!("/api/friends/requests/{req_id}"))
        .insert_header(("Cookie", bob_cookie))
        .set_json(serde_json::json!({"action": "reject"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM friends WHERE user_id_1 = $1 OR user_id_2 = $1")
            .bind(alice.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn accept_by_non_recipient_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let charlie = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");
    let charlie_cookie = auth_cookie_header(&state, charlie.id, "user");

    let req = TestRequest::post()
        .uri("/api/friends/requests")
        .insert_header(("Cookie", alice_cookie))
        .set_json(serde_json::json!({"user_id": bob.id}))
        .to_request();
    let resp = call_service(&app, req).await;
    let v: serde_json::Value =
        serde_json::from_slice(&read_body(resp).await).unwrap();
    let req_id = v["data"]["id"].as_str().unwrap().to_owned();

    // Charlie tries to accept — not the recipient.
    let req = TestRequest::put()
        .uri(&format!("/api/friends/requests/{req_id}"))
        .insert_header(("Cookie", charlie_cookie))
        .set_json(serde_json::json!({"action": "accept"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ────────────────────────────────────────────────────────────────────────
// GET /api/friends + DELETE /api/friends/{id}
// ────────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn list_friends_returns_both_sides_after_accept(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_friendship(&pool, alice.id, bob.id).await;
    let app = init_service(build_app(state.clone())).await;

    for u in [&alice, &bob] {
        let cookie = auth_cookie_header(&state, u.id, "user");
        let req = TestRequest::get()
            .uri("/api/friends")
            .insert_header(("Cookie", cookie))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let v: serde_json::Value =
            serde_json::from_slice(&read_body(resp).await).unwrap();
        let friends = v["data"]["friends"].as_array().unwrap();
        assert_eq!(friends.len(), 1, "user {} should see 1 friend", u.id);
    }
}

#[sqlx::test]
async fn delete_friend_removes_friendship_only(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    seed_friendship(&pool, alice.id, bob.id).await;
    let app = init_service(build_app(state.clone())).await;
    let alice_cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/friends/{}", bob.id))
        .insert_header(("Cookie", alice_cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM friends")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn delete_friend_self_returns_400(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/friends/{}", alice.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn delete_friend_when_not_friends_returns_404(pool: PgPool) {
    let state = build_state(pool.clone());
    let alice = seed_user(&pool, None).await;
    let bob = seed_user(&pool, None).await;
    let app = init_service(build_app(state.clone())).await;
    let cookie = auth_cookie_header(&state, alice.id, "user");

    let req = TestRequest::delete()
        .uri(&format!("/api/friends/{}", bob.id))
        .insert_header(("Cookie", cookie))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
