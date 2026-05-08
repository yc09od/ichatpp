//! HTTP handlers, grouped by resource.
//!
//! Each submodule owns the request/response types, validation, and DAO
//! calls for one slice of the API. Cross-cutting concerns (auth, CSRF,
//! rate limiting) live in `crate::middleware`.

pub mod auth;
pub mod emojis;
pub mod friends;
pub mod invitations;
pub mod messages;
pub mod users;
pub mod ws;
