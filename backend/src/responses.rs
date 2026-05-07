//! Helpers for the success-response envelope defined in ARCHITECTURE.md §6:
//!
//! ```json
//! { "data": { /* T */ }, "meta": { "timestamp": "2026-05-07T12:00:00Z" } }
//! ```

// Exercised by tests below; first non-test consumer lands with TODO [21]
// (`/api/users/me`). Remove this allow once a real handler imports
// `ApiResponse`.
#![allow(dead_code)]

use actix_web::HttpResponse;
use chrono::Utc;
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    pub meta: Meta,
}

#[derive(Serialize)]
pub struct Meta {
    pub timestamp: String,
}

impl<T: Serialize> ApiResponse<T> {
    fn build(data: T) -> Self {
        Self {
            data,
            meta: Meta {
                timestamp: Utc::now().to_rfc3339(),
            },
        }
    }

    /// Render as `200 OK` with the envelope.
    pub fn ok(data: T) -> HttpResponse {
        HttpResponse::Ok().json(Self::build(data))
    }

    /// Render as `201 Created` with the envelope (use after successful inserts).
    pub fn created(data: T) -> HttpResponse {
        HttpResponse::Created().json(Self::build(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{
        get,
        test::{call_service, init_service, read_body, TestRequest},
        App,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    struct Greeting<'a> {
        hello: &'a str,
    }

    #[get("/test/ok")]
    async fn ok_endpoint() -> HttpResponse {
        ApiResponse::ok(Greeting { hello: "world" })
    }

    #[actix_web::test]
    async fn ok_endpoint_wraps_data_with_meta_timestamp() {
        let app = init_service(App::new().service(ok_endpoint)).await;
        let req = TestRequest::get().uri("/test/ok").to_request();
        let resp = call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body = read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).expect("body is JSON");

        assert_eq!(json["data"]["hello"], "world");
        let ts = json["meta"]["timestamp"].as_str().expect("timestamp is string");
        // RFC3339 starts with a 4-digit year and contains a 'T' separator.
        assert!(ts.contains('T'), "expected RFC3339 timestamp, got {ts}");
    }
}
