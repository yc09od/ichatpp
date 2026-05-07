mod config;
mod errors;
mod responses;

use std::sync::Arc;

use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use crate::config::Config;

/// Application state shared across all handlers.
///
/// Pools are built lazily so the server can boot even if Postgres / Redis
/// are momentarily unreachable; first DB query / Redis command will then
/// establish the connection (and surface a clean error to the caller).
#[allow(dead_code)] // fields are read by handlers landing in upcoming TODOs
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::Client,
    pub config: Arc<Config>,
}

#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(json!({"status": "ok"}))
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,ichatpp=debug"),
    )
    .init();

    let cfg = Config::from_env()?;
    let bind_addr = cfg.bind_addr.clone();

    let db = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_lazy(&cfg.database_url)?;

    let redis = redis::Client::open(cfg.redis_url.clone())?;

    let state = AppState {
        db,
        redis,
        config: Arc::new(cfg),
    };

    log::info!("ichatpp backend listening on {bind_addr}");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .service(health)
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
