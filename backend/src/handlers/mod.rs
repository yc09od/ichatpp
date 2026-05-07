//! HTTP handlers, grouped by resource.
//!
//! Each submodule owns the request/response types, validation, and DAO
//! calls for one slice of the API. Cross-cutting concerns (auth, CSRF,
//! rate limiting) live in `crate::middleware`.

pub mod invitations;
