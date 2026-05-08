//! Smoke tests — verify the integration test fixture itself compiles
//! and can stand up an Actix app against the live docker-compose stack.

mod common;

use actix_web::http::StatusCode;
use sqlx::PgPool;

#[sqlx::test]
async fn app_starts_and_validate_endpoint_responds(pool: PgPool) {
    let state = common::build_state(pool);
    let app = common::init_service(common::build_app(state)).await;

    // /api/invitations/validate is the cheapest public POST endpoint we
    // ship — no auth, no DB row required, and it tells us routing,
    // serde, and JSON extraction all line up.
    let req = common::TestRequest::post()
        .uri("/api/invitations/validate")
        .set_json(serde_json::json!({ "code": "INV-DOES-NOT-EXIST" }))
        .to_request();

    let resp = common::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = common::read_body(resp).await;
    let v: serde_json::Value =
        serde_json::from_slice(&body).expect("response should be JSON");
    // Per ARCHITECTURE.md §6 the success envelope is { data, meta }; the
    // validation result lives under data.
    assert_eq!(v["data"]["valid"], false);
}
