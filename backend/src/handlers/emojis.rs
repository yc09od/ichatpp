//! Emoji upload, list, and delete endpoints (TODOs [35]–[36];
//! ARCHITECTURE.md §4.5).
//!
//! ```text
//! POST   /api/emojis            # multipart upload (PNG/JPG ≤ 500 KB)
//! GET    /api/emojis            # list current user's emojis (60 s Redis cache)
//! DELETE /api/emojis/{id}       # delete row + S3 original + S3 thumbnail
//! ```
//!
//! ## Per-user cap
//!
//! TODO [35] sets a hard 100-emoji limit. We check inside the upload
//! transaction (the count + insert sit inside one tx so concurrent
//! uploads can't both squeak past 100). Hitting the cap returns 409
//! "limit reached" — the SPA surfaces it as a "delete one first" dialog.
//!
//! ## Read cache
//!
//! `GET /api/emojis` serves from a per-user Redis key (60 s TTL). Upload
//! and delete invalidate it via `DEL`. The cache is best-effort: if the
//! Redis hop fails the handler falls back to the DB and logs a warning,
//! so a brief Redis hiccup never makes the endpoint unreachable.
//!
//! ## Object cleanup posture
//!
//! `delete_object` is idempotent (S3 returns 204 either way), so a retry
//! after a partial failure is safe. Best-effort logging only — we never
//! block the API response on object-store cleanup.

#![allow(dead_code)] // routes mount in main.rs once the module is wired.

use actix_multipart::Multipart;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::authenticated::AuthenticatedUser;
use crate::errors::{AppError, AppResult};
use crate::responses::ApiResponse;
use crate::services::emoji::{
    content_type_for, decode_within_limits, extension_for, render_thumbnail,
    validate_emoji_bytes, EmojiError, MAX_EMOJI_BYTES,
};
use crate::AppState;

/// Hard cap per user — TODO [35]: "≤ 100, 超出返回 409".
const MAX_EMOJIS_PER_USER: i64 = 100;

/// Cache TTL for `GET /api/emojis` per user — TODO [36]: "Redis 缓存 60s".
const LIST_CACHE_TTL_SECONDS: u64 = 60;

/// Multipart field name expected from the SPA. Matches the avatar
/// endpoint so the upload component can be shared.
const EMOJI_FIELD_NAME: &str = "file";

/// Optional multipart field for the emoji's display name. The SPA may
/// omit it; we fall back to the source filename minus extension.
const NAME_FIELD: &str = "name";

/// Maximum length of `emojis.name` column (`VARCHAR(100)` in the schema).
const MAX_NAME_LEN: usize = 100;

/// Per-user list-cache key namespace.
const LIST_CACHE_PREFIX: &str = "emojis:list:";

// ────────────────────────────────────────────────────────────────────────
// DTOs
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct EmojiView {
    pub id: Uuid,
    pub name: String,
    pub file_url: String,
    pub thumbnail_url: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub emojis: Vec<EmojiView>,
}

#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub emoji: EmojiView,
}

// ────────────────────────────────────────────────────────────────────────
// SQL row
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct EmojiRow {
    id: Uuid,
    name: String,
    file_url: String,
    thumbnail_url: Option<String>,
    mime_type: Option<String>,
    file_size: Option<i32>,
    created_at: NaiveDateTime,
}

impl From<EmojiRow> for EmojiView {
    fn from(r: EmojiRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            file_url: r.file_url,
            thumbnail_url: r.thumbnail_url,
            mime_type: r.mime_type,
            file_size: r.file_size,
            created_at: r.created_at.and_utc(),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Multipart parsing
// ────────────────────────────────────────────────────────────────────────

struct ParsedUpload {
    bytes: Vec<u8>,
    /// User-supplied display name, or filename fallback. Already
    /// length-clamped + non-empty.
    name: String,
}

fn map_emoji_error(e: EmojiError) -> AppError {
    match e {
        EmojiError::Empty
        | EmojiError::TooLarge
        | EmojiError::UnsupportedFormat
        | EmojiError::DimensionsTooLarge
        | EmojiError::Decode => AppError::BadRequest(e.to_string()),
        EmojiError::Encode => AppError::Internal(anyhow::anyhow!("emoji encode: {e}")),
    }
}

/// Stream the multipart body and pull the file payload + optional name.
/// Aborts on oversize so a bad client cannot tie up a worker with a 1 GB
/// upload.
async fn read_multipart(mut multipart: Multipart) -> Result<ParsedUpload, AppError> {
    let mut bytes: Option<Vec<u8>> = None;
    let mut name_field: Option<String> = None;
    let mut filename_fallback: Option<String> = None;

    while let Some(mut field) = multipart
        .try_next()
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart payload: {e}")))?
    {
        let field_name = field.name().map(str::to_owned);
        match field_name.as_deref() {
            Some(EMOJI_FIELD_NAME) => {
                if filename_fallback.is_none() {
                    filename_fallback = field
                        .content_disposition()
                        .and_then(|d| d.get_filename())
                        .map(str::to_owned);
                }
                let mut buf = Vec::with_capacity(64 * 1024);
                while let Some(chunk) = field
                    .try_next()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("upload stream error: {e}")))?
                {
                    if buf.len() + chunk.len() > MAX_EMOJI_BYTES {
                        return Err(map_emoji_error(EmojiError::TooLarge));
                    }
                    buf.extend_from_slice(&chunk);
                }
                bytes = Some(buf);
            }
            Some(NAME_FIELD) => {
                let mut buf = Vec::with_capacity(128);
                while let Some(chunk) = field
                    .try_next()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("name stream error: {e}")))?
                {
                    // A wildly long `name` is almost certainly a bug; cap
                    // before allocating any further.
                    if buf.len() + chunk.len() > MAX_NAME_LEN * 4 {
                        return Err(AppError::BadRequest(format!(
                            "name must be ≤ {MAX_NAME_LEN} chars"
                        )));
                    }
                    buf.extend_from_slice(&chunk);
                }
                name_field = Some(
                    String::from_utf8(buf)
                        .map_err(|_| AppError::BadRequest("name must be UTF-8".into()))?,
                );
            }
            _ => {
                // Drain unknown fields to keep the parser progressing.
                while field.try_next().await.map_err(|e| {
                    AppError::BadRequest(format!("unknown field stream error: {e}"))
                })?.is_some()
                {}
            }
        }
    }

    let bytes = bytes.ok_or_else(|| {
        AppError::BadRequest(format!("missing multipart field '{EMOJI_FIELD_NAME}'"))
    })?;

    let name = name_field
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            filename_fallback
                .as_deref()
                .map(strip_extension)
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "emoji".to_owned());
    let name = clamp_name(&name);

    Ok(ParsedUpload { bytes, name })
}

/// Trim filename trailing extension (everything after the last `.`).
fn strip_extension(filename: &str) -> &str {
    match filename.rfind('.') {
        Some(idx) if idx > 0 => &filename[..idx],
        _ => filename,
    }
}

/// Clamp `name` to MAX_NAME_LEN chars (Unicode codepoints, matching the
/// Postgres `VARCHAR(100)` semantics — Postgres counts in characters,
/// not bytes).
fn clamp_name(name: &str) -> String {
    name.chars().take(MAX_NAME_LEN).collect()
}

// ────────────────────────────────────────────────────────────────────────
// Cache helpers
// ────────────────────────────────────────────────────────────────────────

fn cache_key(user_id: Uuid) -> String {
    format!("{LIST_CACHE_PREFIX}{user_id}")
}

async fn cache_get(client: &redis::Client, user_id: Uuid) -> redis::RedisResult<Option<String>> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("GET")
        .arg(cache_key(user_id))
        .query_async::<Option<String>>(&mut conn)
        .await
}

async fn cache_set(
    client: &redis::Client,
    user_id: Uuid,
    body: &str,
) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("SET")
        .arg(cache_key(user_id))
        .arg(body)
        .arg("EX")
        .arg(LIST_CACHE_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await
}

/// Invalidation is intentionally fire-and-forget — a stale cache for up
/// to TTL seconds is acceptable, but blocking the response on Redis
/// availability isn't.
async fn cache_invalidate(client: &redis::Client, user_id: Uuid) {
    let conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("emoji cache invalidate connect failed for {user_id}: {e}");
            return;
        }
    };
    let mut conn = conn;
    if let Err(e) = redis::cmd("DEL")
        .arg(cache_key(user_id))
        .query_async::<i32>(&mut conn)
        .await
    {
        log::warn!("emoji cache DEL failed for {user_id}: {e}");
    }
}

// ────────────────────────────────────────────────────────────────────────
// DAO
// ────────────────────────────────────────────────────────────────────────

async fn count_user_emojis(pool: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM emojis WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
}

async fn fetch_user_emojis(pool: &PgPool, user_id: Uuid) -> Result<Vec<EmojiRow>, sqlx::Error> {
    sqlx::query_as::<_, EmojiRow>(
        r#"
        SELECT id, name, file_url, thumbnail_url, mime_type, file_size, created_at
        FROM emojis
        WHERE user_id = $1
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

// ────────────────────────────────────────────────────────────────────────
// Handlers
// ────────────────────────────────────────────────────────────────────────

/// `POST /api/emojis`
///
/// Pipeline:
/// 1. Stream the `file` field into a bounded buffer + read optional `name`.
/// 2. Validate magic bytes (PNG / JPEG only).
/// 3. Decode + dimension-check + render the 100×100 thumbnail.
/// 4. Verify under the 100-emoji cap (transactional).
/// 5. Upload original then thumbnail to the emojis bucket.
/// 6. INSERT the DB row + commit.
/// 7. Invalidate the per-user list cache.
pub async fn upload_handler(
    user: AuthenticatedUser,
    multipart: Multipart,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let parsed = read_multipart(multipart).await?;
    let format = validate_emoji_bytes(&parsed.bytes).map_err(map_emoji_error)?;
    let img = decode_within_limits(&parsed.bytes, format).map_err(map_emoji_error)?;
    let thumb_bytes = render_thumbnail(&img, format).map_err(map_emoji_error)?;

    // Cap check inside a transaction so two concurrent uploads can't both
    // squeak past 100. We acquire an advisory lock on the user_id to
    // serialise concurrent uploads from the same user — emoji uploads
    // are rare, so the lock contention is negligible.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("begin emoji tx: {e}")))?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(user.user_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("advisory lock: {e}")))?;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emojis WHERE user_id = $1")
        .bind(user.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("count emojis: {e}")))?;

    if count >= MAX_EMOJIS_PER_USER {
        return Err(AppError::Conflict(format!(
            "emoji limit reached ({MAX_EMOJIS_PER_USER}); delete one first"
        )));
    }

    let asset_id = Uuid::new_v4();
    let ext = extension_for(format);
    let content_type = content_type_for(format);
    let bucket = state.config.s3_bucket_emojis.as_str();
    let original_key = format!("{}/{}.{}", user.user_id, asset_id, ext);
    let thumb_key = format!("{}/{}_thumb.{}", user.user_id, asset_id, ext);

    state
        .storage
        .put_object(bucket, &original_key, &parsed.bytes, content_type)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("upload original emoji: {e}")))?;

    if let Err(e) = state
        .storage
        .put_object(bucket, &thumb_key, &thumb_bytes, content_type)
        .await
    {
        log::warn!(
            "emoji thumb upload failed for {original_key}; original orphaned at s3://{bucket}/{original_key}: {e}"
        );
        return Err(AppError::Internal(anyhow::anyhow!(
            "upload emoji thumbnail: {e}"
        )));
    }

    let file_url = state.storage.object_url(bucket, &original_key);
    let thumbnail_url = state.storage.object_url(bucket, &thumb_key);
    let file_size: i32 = parsed.bytes.len().try_into().unwrap_or(i32::MAX);

    let row: EmojiRow = sqlx::query_as(
        r#"
        INSERT INTO emojis (user_id, name, file_url, thumbnail_url, mime_type, file_size)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, name, file_url, thumbnail_url, mime_type, file_size, created_at
        "#,
    )
    .bind(user.user_id)
    .bind(&parsed.name)
    .bind(&file_url)
    .bind(&thumbnail_url)
    .bind(content_type)
    .bind(file_size)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("insert emoji: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("commit emoji tx: {e}")))?;

    cache_invalidate(&state.redis, user.user_id).await;

    Ok(HttpResponse::Created().json(ApiResponse::build(UploadResponse {
        emoji: EmojiView::from(row),
    })))
}

/// `GET /api/emojis`
///
/// Cache lookup → DB fallback. The cache stores the rendered JSON of the
/// inner `ListResponse` (data only, no envelope) so the handler always
/// re-wraps with a fresh `meta.timestamp` — meta isn't cached.
pub async fn list_handler(
    user: AuthenticatedUser,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    if let Ok(Some(cached)) = cache_get(&state.redis, user.user_id).await {
        if let Ok(parsed) = serde_json::from_str::<ListResponse>(&cached) {
            return Ok(HttpResponse::Ok().json(ApiResponse::build(parsed)));
        }
        // Malformed cache entry — fall through to DB and overwrite.
        log::warn!("emoji cache for {} contained malformed JSON", user.user_id);
    }

    let rows = fetch_user_emojis(&state.db, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("list emojis: {e}")))?;
    let response = ListResponse {
        emojis: rows.into_iter().map(EmojiView::from).collect(),
    };

    if let Ok(serialised) = serde_json::to_string(&response) {
        if let Err(e) = cache_set(&state.redis, user.user_id, &serialised).await {
            log::warn!("emoji cache write failed for {}: {e}", user.user_id);
        }
    }

    Ok(HttpResponse::Ok().json(ApiResponse::build(response)))
}

/// `DELETE /api/emojis/{id}`
///
/// 404 if the emoji doesn't exist or belongs to someone else (don't leak
/// existence). Best-effort object cleanup: even if the S3 deletes fail
/// the DB row is gone, and `delete_object` is idempotent on retry.
///
/// Note: messages that referenced this emoji via `message_emojis` keep
/// working — the FK `ON DELETE CASCADE` cleans up the link rows; the
/// message itself isn't touched.
pub async fn delete_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let emoji_id = path.into_inner();

    // Fetch the URLs *before* the delete so we know which keys to remove
    // from the bucket.
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT file_url, thumbnail_url FROM emojis WHERE id = $1 AND user_id = $2",
    )
    .bind(emoji_id)
    .bind(user.user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("lookup emoji: {e}")))?;

    let (file_url, thumb_url) =
        row.ok_or_else(|| AppError::NotFound("emoji not found".into()))?;

    let result = sqlx::query("DELETE FROM emojis WHERE id = $1 AND user_id = $2")
        .bind(emoji_id)
        .bind(user.user_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("delete emoji row: {e}")))?;

    if result.rows_affected() == 0 {
        // Race: the row vanished between our SELECT and DELETE. Treat as
        // already-deleted for idempotency.
        return Ok(HttpResponse::NoContent().finish());
    }

    // Best-effort S3 cleanup. delete_object is idempotent so even if the
    // first attempt half-failed an orphan-sweeper job can mop up later.
    let bucket = state.config.s3_bucket_emojis.as_str();
    if let Some(key) = key_from_url(&file_url, bucket, &state.storage.object_url(bucket, "")) {
        if let Err(e) = state.storage.delete_object(bucket, &key).await {
            log::warn!("emoji original delete failed for s3://{bucket}/{key}: {e}");
        }
    }
    if let Some(thumb_url) = thumb_url {
        if let Some(key) =
            key_from_url(&thumb_url, bucket, &state.storage.object_url(bucket, ""))
        {
            if let Err(e) = state.storage.delete_object(bucket, &key).await {
                log::warn!("emoji thumb delete failed for s3://{bucket}/{key}: {e}");
            }
        }
    }

    cache_invalidate(&state.redis, user.user_id).await;

    Ok(HttpResponse::NoContent().finish())
}

/// Recover the bucket key from a stored object URL by stripping the
/// `<endpoint>/<bucket>/` prefix. Returns `None` if the URL doesn't fit
/// the expected shape — in which case the cleanup logs a warning and
/// moves on; the DB row is already gone.
fn key_from_url(url: &str, _bucket: &str, bucket_root: &str) -> Option<String> {
    url.strip_prefix(bucket_root).map(str::to_owned)
}

// ────────────────────────────────────────────────────────────────────────
// Route configuration
// ────────────────────────────────────────────────────────────────────────

pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_handler))
        .route("", web::post().to(upload_handler))
        .route("/{id}", web::delete().to(delete_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Filename / name handling ──

    #[test]
    fn strip_extension_handles_common_cases() {
        assert_eq!(strip_extension("happy.png"), "happy");
        assert_eq!(strip_extension("multi.dot.jpg"), "multi.dot");
        assert_eq!(strip_extension("noext"), "noext");
        // Leading dot ("hidden file") is preserved verbatim — matches
        // Unix convention.
        assert_eq!(strip_extension(".hidden"), ".hidden");
    }

    #[test]
    fn clamp_name_truncates_to_max() {
        let long: String = "x".repeat(MAX_NAME_LEN + 50);
        let clamped = clamp_name(&long);
        assert_eq!(clamped.chars().count(), MAX_NAME_LEN);
    }

    /// Pin: clamp counts characters, not bytes — a 100-emoji name is
    /// 100 codepoints regardless of how many bytes each takes.
    #[test]
    fn clamp_name_counts_codepoints_not_bytes() {
        let multibyte: String = "嘿".repeat(MAX_NAME_LEN + 10);
        let clamped = clamp_name(&multibyte);
        assert_eq!(clamped.chars().count(), MAX_NAME_LEN);
    }

    // ── Cache key namespace ──

    #[test]
    fn cache_key_is_namespaced_per_user() {
        let id = Uuid::nil();
        assert_eq!(cache_key(id), format!("{LIST_CACHE_PREFIX}{id}"));
        assert!(cache_key(id).starts_with(LIST_CACHE_PREFIX));
    }

    // ── URL → key recovery ──

    #[test]
    fn key_from_url_strips_bucket_root() {
        let bucket_root = "http://localhost:9000/emojis/";
        let url = "http://localhost:9000/emojis/uid/abc.png";
        assert_eq!(
            key_from_url(url, "emojis", bucket_root),
            Some("uid/abc.png".into())
        );
    }

    #[test]
    fn key_from_url_returns_none_on_mismatch() {
        let bucket_root = "http://localhost:9000/emojis/";
        let url = "http://other.example.com/emojis/uid/abc.png";
        assert_eq!(key_from_url(url, "emojis", bucket_root), None);
    }

    // ── Constants ──

    #[test]
    fn max_emojis_per_user_matches_spec() {
        assert_eq!(MAX_EMOJIS_PER_USER, 100);
    }

    #[test]
    fn list_cache_ttl_matches_spec() {
        assert_eq!(LIST_CACHE_TTL_SECONDS, 60);
    }

    // ── DTO shapes ──

    #[test]
    fn list_response_round_trips() {
        let r = ListResponse {
            emojis: vec![EmojiView {
                id: Uuid::nil(),
                name: "smile".into(),
                file_url: "http://example/a.png".into(),
                thumbnail_url: Some("http://example/a_thumb.png".into()),
                mime_type: Some("image/png".into()),
                file_size: Some(1234),
                created_at: Utc::now(),
            }],
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: ListResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(r.emojis, back.emojis);
    }

    /// EmojiView must not leak `user_id` — the list endpoint is
    /// inherently scoped to the requester, so exposing user_id would be
    /// noise at best and a leak vector if the type ever gets reused for
    /// a public lookup.
    #[test]
    fn emoji_view_omits_user_id() {
        let v = EmojiView {
            id: Uuid::nil(),
            name: "n".into(),
            file_url: "u".into(),
            thumbnail_url: None,
            mime_type: None,
            file_size: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert!(json.as_object().unwrap().get("user_id").is_none());
    }
}
