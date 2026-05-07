mod auth;
mod config;
mod errors;
mod handlers;
mod middleware;
mod responses;

use std::sync::Arc;

use actix_governor::Governor;
use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use crate::auth::jwt::Keys;
use crate::config::Config;

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
}

#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(json!({"status": "ok"}))
}

/// Placeholder for `POST /api/auth/login` — replaced in TODO [18].
/// Exists now so that TODO [10]'s rate-limit verification has a route to hit.
#[post("/login")]
async fn login_placeholder() -> impl Responder {
    HttpResponse::NotImplemented().json(json!({
        "error": {
            "code": "NOT_IMPLEMENTED",
            "message": "login endpoint pending — see TODO [18]"
        }
    }))
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,ichatpp=debug,actix_web=info"),
    )
    .init();

    let cfg = Config::from_env()?;
    let bind_addr = cfg.bind_addr.clone();

    let db = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_lazy(&cfg.database_url)?;

    let redis = redis::Client::open(cfg.redis_url.clone())?;

    // Loaded eagerly: a missing or malformed key is fatal — without it we
    // can't issue or verify any JWT, so refusing to boot is correct.
    let jwt_keys = Keys::from_config(&cfg)?;

    let state = AppState {
        db,
        redis,
        config: Arc::new(cfg),
        jwt_keys,
    };

    // Governor configs are constructed once; each worker calls `Governor::new`
    // against the same config so token-bucket state is shared.
    let global_gov = middleware::global_governor_config();
    let auth_gov = middleware::auth_governor_config();

    log::info!("ichatpp backend listening on {bind_addr}");

    HttpServer::new(move || {
        // CORS reads `frontend_origin` from config; rebuild per worker.
        let cors = middleware::cors(&state.config);

        // Wrap order: outermost is applied LAST.
        // Request flow:  Logger → CORS → Governor → CSRF → handler
        // Response flow: handler → CSRF → Governor → CORS → Logger
        //
        // CSRF sits innermost so a 403 still flows back through the access
        // log and picks up the right CORS headers; rate-limiter runs first
        // so brute-force attempts are throttled before token comparison.
        App::new()
            .app_data(web::Data::new(state.clone()))
            .wrap(middleware::CsrfProtection)
            .wrap(Governor::new(&global_gov))
            .wrap(cors)
            .wrap(middleware::request_logger())
            .service(health)
            .service(
                web::scope("/api/auth")
                    .wrap(Governor::new(&auth_gov))
                    .service(login_placeholder),
            )
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
