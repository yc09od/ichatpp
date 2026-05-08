//! ichatpp backend library entry point.
//!
//! `tests/*.rs` integration tests import handlers, services, and the
//! shared `AppState` from this module — extracting these into a library
//! crate is what lets a black-box integration test exercise a handler
//! through the full Actix routing layer without duplicating the route
//! wiring from `main.rs`.
//!
//! The bin target (`src/main.rs`) is a thin shell that builds the live
//! `AppState` from environment variables and starts the HTTP server.

pub mod auth;
pub mod config;
pub mod errors;
pub mod handlers;
pub mod middleware;
pub mod responses;
pub mod services;
pub mod ws;

use std::sync::Arc;

use crate::auth::jwt::Keys;
use crate::config::Config;
use crate::services::storage::ObjectStore;
use crate::ws::registry::SessionRegistry;

/// Application state shared across all handlers.
///
/// Pools are built lazily so the server can boot even if Postgres / Redis
/// are momentarily unreachable; first DB query / Redis command will then
/// establish the connection (and surface a clean error to the caller).
/// `Keys`, by contrast, must be valid at startup — we cannot sign or verify
/// JWTs at all without them.
#[allow(dead_code)] // fields are read by handlers landing in upcoming TODOs
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::Client,
    pub config: Arc<Config>,
    pub jwt_keys: Keys,
    pub storage: ObjectStore,
    /// Process-wide registry of live WS sessions (TODO [27]).
    pub session_registry: SessionRegistry,
    /// Random per-process id used to filter out a publisher's own
    /// Pub/Sub echoes in the presence broadcast path (TODO [27]).
    pub instance_id: uuid::Uuid,
}
