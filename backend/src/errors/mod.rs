//! Error type used by handlers and services across the backend.
//!
//! `AppError` implements `actix_web::ResponseError`, so any handler
//! returning `Result<_, AppError>` will automatically render the JSON body
//! defined in ARCHITECTURE.md §6:
//!
//! ```json
//! { "error": { "code": "NOT_FOUND", "message": "...", "details": { } } }
//! ```
//!
//! `details` is omitted from the JSON when not provided.

// Items are exercised by tests below and consumed by handlers landing in
// upcoming TODOs ([10] middleware, [17] register, etc.). Remove the allow
// once a real handler imports `AppError`.
#![allow(dead_code)]

use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;

/// All public errors the API can produce. New variants must be small and
/// orthogonal — prefer attaching context via `details` over inventing a new
/// variant per use case.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Unauthorized(String),

    #[error("{0}")]
    Forbidden(String),

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Conflict(String),

    /// Anything unexpected. The underlying `anyhow::Error` is logged at
    /// ERROR level; callers see only a generic message to avoid leaking
    /// internal details.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NOT_FOUND",
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Forbidden(_) => "FORBIDDEN",
            Self::BadRequest(_) => "BAD_REQUEST",
            Self::Conflict(_) => "CONFLICT",
            Self::Internal(_) => "INTERNAL",
        }
    }

    fn user_message(&self) -> String {
        match self {
            Self::Internal(e) => {
                // Log the full chain, but never leak it to the client.
                log::error!("internal error: {e:?}");
                "internal server error".to_owned()
            }
            other => other.to_string(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        self.status()
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status()).json(ErrorBody {
            error: ErrorDetail {
                code: self.code(),
                message: self.user_message(),
                details: None,
            },
        })
    }
}

/// Convenience alias mirroring the project convention.
pub type AppResult<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{
        get,
        test::{call_service, init_service, read_body, TestRequest},
        App,
    };

    /// Verification handler for TODO [8]: returns `AppError::NotFound`.
    #[get("/test/not-found")]
    async fn not_found_endpoint() -> AppResult<HttpResponse> {
        Err(AppError::NotFound("user 42 not found".into()))
    }

    #[actix_web::test]
    async fn not_found_endpoint_renders_correct_json() {
        let app = init_service(App::new().service(not_found_endpoint)).await;
        let req = TestRequest::get().uri("/test/not-found").to_request();
        let resp = call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 404);
        let body = read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).expect("body is JSON");

        assert_eq!(json["error"]["code"], "NOT_FOUND");
        assert_eq!(json["error"]["message"], "user 42 not found");
        // `details` is absent (skipped via serde) — assert it doesn't appear.
        assert!(json["error"].get("details").is_none());
    }

    #[test]
    fn status_codes_match_variants() {
        assert_eq!(
            AppError::NotFound("x".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AppError::Unauthorized("x".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            AppError::Forbidden("x".into()).status_code(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            AppError::BadRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Conflict("x".into()).status_code(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            AppError::Internal(anyhow::anyhow!("x")).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn internal_error_does_not_leak_details() {
        let err = AppError::Internal(anyhow::anyhow!("DATABASE_URL credentials are wrong"));
        assert_eq!(err.user_message(), "internal server error");
    }

    #[test]
    fn anyhow_into_app_error_via_question_mark() {
        // `?` should convert anyhow::Error → AppError::Internal.
        fn maybe_fail() -> AppResult<()> {
            let result: anyhow::Result<()> = Err(anyhow::anyhow!("boom"));
            result?;
            Ok(())
        }
        let err = maybe_fail().unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
