//! Cross-cutting services that handlers depend on.
//!
//! - [`avatar`] — pure-function image validation and thumbnail rendering.
//! - [`storage`] — object-storage client (S3 / MinIO) wrapper.
//!
//! Both modules are kept off the request path: `services::storage` does
//! not know about `actix_web`, and `services::avatar` is fully
//! synchronous. This makes them easy to unit-test without spinning up a
//! request context, and easy to reuse for future upload endpoints
//! (emojis, exports).

pub mod avatar;
pub mod storage;
