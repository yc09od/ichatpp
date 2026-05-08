//! WebSocket-side primitives, kept out of the request path so the per-
//! connection task can run on its own without holding actix-web request
//! state.
//!
//! - [`presence`] — Redis `online_users` set helpers (TODO [26]).
//! - [`session`] — the per-connection task loop (TODO [26], extended in
//!   TODOs [27]–[30]).
//!
//! The HTTP handler that bridges these into actix-web lives at
//! `crate::handlers::ws`. Per the project memory note, this whole layer
//! avoids the deprecated actix-web-actors model — Session + MessageStream
//! + a tokio task is the supported pattern.

pub mod broadcast;
pub mod chat;
pub mod event;
pub mod presence;
pub mod registry;
pub mod session;
