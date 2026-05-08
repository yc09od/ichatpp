use std::sync::Arc;

use actix_governor::Governor;
use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use ichatpp::auth::jwt::Keys;
use ichatpp::config::Config;
use ichatpp::services::storage::ObjectStore;
use ichatpp::ws::registry::SessionRegistry;
use ichatpp::{handlers, middleware, ws, AppState};

#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(json!({"status": "ok"}))
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

    // Same posture for object storage credentials: parse at boot so a
    // misconfigured access key / secret fails the server start, not the
    // first upload.
    let storage = ObjectStore::from_config(&cfg)?;

    let state = AppState {
        db,
        redis,
        config: Arc::new(cfg),
        jwt_keys,
        storage,
        session_registry: SessionRegistry::new(),
        instance_id: uuid::Uuid::new_v4(),
    };

    // Spawn the cross-instance presence subscriber. It owns its own
    // Pub/Sub connection and reconnects with back-off on transient
    // failures; sharing the AppState clone keeps it pinned to the same
    // session_registry / instance_id the request handlers see.
    actix_web::rt::spawn(ws::broadcast::run_subscriber(state.clone()));

    // Same shape as the presence subscriber — separate channel
    // (chat_events) and target-tagged routing instead of friend fan-out.
    actix_web::rt::spawn(ws::chat::run_subscriber(state.clone()));

    // Governor configs are constructed once; each worker calls `Governor::new`
    // against the same config so token-bucket state is shared.
    let global_gov = middleware::global_governor_config();
    let auth_gov = middleware::auth_governor_config();
    let validate_gov = middleware::invitation_validate_governor_config();

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
            // Top-level route — WS is a single endpoint, no scope wrapping.
            // Sits before the /api/auth scope so it's not subject to the
            // tighter auth-scoped governor; the WS upgrade itself is an
            // expensive but rare operation.
            .route("/api/ws", web::get().to(handlers::ws::ws_handler))
            .service(
                web::scope("/api/auth")
                    .wrap(Governor::new(&auth_gov))
                    .route("/login", web::post().to(handlers::auth::login_handler))
                    .route(
                        "/register",
                        web::post().to(handlers::auth::register_handler),
                    )
                    .route(
                        "/refresh",
                        web::post().to(handlers::auth::refresh_handler),
                    )
                    .route("/logout", web::post().to(handlers::auth::logout_handler)),
            )
            .service(
                web::scope("/api/users").configure(handlers::users::routes),
            )
            .service(
                web::scope("/api/friends").configure(handlers::friends::routes),
            )
            .service(
                web::scope("/api/messages").configure(handlers::messages::routes),
            )
            .service(
                web::scope("/api/emojis").configure(handlers::emojis::routes),
            )
            .service(
                web::scope("/api/invitations")
                    // /validate first so its tighter per-IP limit applies
                    // only to the public endpoint, and so future `/{id}`
                    // patterns in `routes()` can't shadow it. It's also
                    // public — the AdminUser extractor isn't on the
                    // handler — and exempt from CSRF (see `is_exempt_path`).
                    .service(
                        web::resource("/validate")
                            .wrap(Governor::new(&validate_gov))
                            .route(
                                web::post().to(handlers::invitations::validate_handler),
                            ),
                    )
                    .configure(handlers::invitations::routes),
            )
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
