//! Invitation code primitives — generation, hashing, and the insert DAO.
//!
//! Per ARCHITECTURE.md §4.6 and TODO [13] the invariants are:
//!
//! - Plaintext format: `INV-{20 base62 chars}` — 24 chars total.
//! - Plaintext is returned to the admin **exactly once** (in the response of
//!   `POST /api/invitations/generate`). The DB never stores it.
//! - The DB stores only `code_hash` (SHA-256 hex, 64 chars), `code_prefix`
//!   (the first 8 plaintext chars, e.g. `INV-AbCd`, used by admins to
//!   identify a batch — never used to validate a code), `created_by`, and
//!   `expires_at`.
//!
//! This module deliberately stops at the data layer. The HTTP endpoints
//! that wrap these helpers (admin generate / list / stats / delete /
//! validate) land in TODOs [14]–[16].

// Several admin-side helpers below are consumed by handlers landing in
// later TODOs (e.g. registration in [17] uses the same hash → row lookup).
#![allow(dead_code)]

use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::admin::AdminUser;
use crate::errors::{AppError, AppResult};
use crate::responses::ApiResponse;

/// Plaintext prefix every code carries. Must be ASCII so it survives
/// `chars().take()` cleanly when deriving `code_prefix`.
const CODE_PLAINTEXT_PREFIX: &str = "INV-";

/// Number of base62 chars after the `INV-` prefix.
const CODE_RANDOM_LEN: usize = 20;

/// How many leading plaintext chars are stored as `code_prefix` for admin
/// identification. With `INV-` (4 chars) + 4 random chars this yields the
/// `INV-AbCd` form referenced in ARCHITECTURE.md §4.6.
pub const CODE_PREFIX_LEN: usize = 8;

/// Base62 alphabet used for the random suffix. Chosen for URL-safety,
/// case-distinctness, and easy manual entry from a printed CSV.
const BASE62: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Largest byte value that's a clean multiple of 62 (rejection-sampling
/// threshold): `4 * 62 = 248`. Bytes in `[0, 248)` give a uniform
/// distribution mod 62; bytes in `[248, 256)` are resampled. This avoids
/// the modulo bias of `byte % 62`.
const REJECTION_THRESHOLD: u8 = 248;

/// Generate a fresh plaintext invitation code: `INV-{20 base62 chars}`.
///
/// Uses [`OsRng`] (the OS CSPRNG) directly via `RngCore::fill_bytes`, with
/// rejection sampling to keep the alphabet distribution uniform.
pub fn generate_code() -> String {
    let mut rng = OsRng;
    let mut out = String::with_capacity(CODE_PLAINTEXT_PREFIX.len() + CODE_RANDOM_LEN);
    out.push_str(CODE_PLAINTEXT_PREFIX);

    let mut buf = [0u8; 1];
    for _ in 0..CODE_RANDOM_LEN {
        loop {
            rng.fill_bytes(&mut buf);
            if buf[0] < REJECTION_THRESHOLD {
                break;
            }
        }
        out.push(BASE62[(buf[0] % 62) as usize] as char);
    }
    out
}

/// SHA-256 hex digest of the plaintext code: 64 lowercase hex chars,
/// matching the `CHAR(64)` shape of `invitations.code_hash`.
pub fn hash_code(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    hex::encode(digest)
}

/// First [`CODE_PREFIX_LEN`] plaintext chars, suitable for `code_prefix`.
/// Caller is responsible for not feeding in something shorter; with our
/// generator that never happens.
pub fn code_prefix(plaintext: &str) -> String {
    plaintext.chars().take(CODE_PREFIX_LEN).collect()
}

/// Insert a single invitation row.
///
/// The function takes the *hash* and *prefix*, never the plaintext —
/// keeping the secret out of this signature is the whole point. The
/// caller hashes once, sends the plaintext back to the admin, and drops
/// it.
///
/// `expires_at` is converted to `NaiveDateTime` because the column is
/// `TIMESTAMP WITHOUT TIME ZONE` (see initial migration). UTC is the
/// project-wide convention.
pub async fn insert_invitation(
    pool: &PgPool,
    code_hash: &str,
    code_prefix: &str,
    created_by: Uuid,
    expires_at: DateTime<Utc>,
    notes: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO invitations (code_hash, code_prefix, created_by, expires_at, notes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
    )
    .bind(code_hash)
    .bind(code_prefix)
    .bind(created_by)
    .bind(expires_at.naive_utc())
    .bind(notes)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

// ────────────────────────────────────────────────────────────────────────
// DAOs — admin-side (list, count, revoke) and lookup-by-hash (TODO [16]
// public validate, TODO [17] registration consume the same primitive).
// ────────────────────────────────────────────────────────────────────────

/// Row shape for admin list responses. Note the deliberate absence of
/// `code_hash` — the hash never leaves the database (see ARCHITECTURE.md
/// §4.6 "存储约定" and TODO [14] verification).
#[derive(Debug, sqlx::FromRow)]
pub struct InvitationRow {
    pub id: Uuid,
    pub code_prefix: Option<String>,
    pub status: String,
    pub used_by: Option<Uuid>,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub notes: Option<String>,
}

/// Page through invitations, optionally filtered by `status` (matches the
/// column literally — `unused`, `used`, `expired`, or `revoked`). Validation
/// of the status value happens in the handler so 400 reaches the client
/// rather than an opaque DB error.
pub async fn list_invitations(
    pool: &PgPool,
    status_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<InvitationRow>, sqlx::Error> {
    sqlx::query_as::<_, InvitationRow>(
        r#"
        SELECT id, code_prefix, status, used_by, created_at, expires_at, notes
        FROM invitations
        WHERE ($1::text IS NULL OR status = $1)
        ORDER BY created_at DESC
        OFFSET $2 LIMIT $3
        "#,
    )
    .bind(status_filter)
    .bind(offset)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Aggregate counts across the four reportable buckets. "Unused" and
/// "expired" both factor in `expires_at` so the dashboard reflects what's
/// *actually usable right now*, not the (possibly stale) `status` column.
pub async fn count_invitation_stats(pool: &PgPool) -> Result<StatsCounts, sqlx::Error> {
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COUNT(*),
            COUNT(*) FILTER (WHERE status = 'used'),
            COUNT(*) FILTER (WHERE status = 'unused' AND expires_at >= NOW()),
            COUNT(*) FILTER (
                WHERE status = 'expired'
                   OR (status = 'unused' AND expires_at < NOW())
            )
        FROM invitations
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(StatsCounts {
        total: row.0,
        used: row.1,
        unused: row.2,
        expired: row.3,
    })
}

/// Minimal projection used by lookup-by-hash. Deliberately omits
/// `code_hash` — the public validate handler (TODO [16]) must not be
/// able to leak it through accidental re-serialization. `id` is
/// exposed so the register handler (TODO [17]) can run the
/// `UPDATE invitations SET status='used' WHERE id=$1` follow-up.
#[derive(Debug, sqlx::FromRow)]
pub struct InvitationStatusRow {
    pub id: Uuid,
    pub status: String,
    pub expires_at: NaiveDateTime,
    pub used_by: Option<Uuid>,
}

/// Look up by SHA-256 hash. Hits `idx_invitations_code_hash` (unique
/// index on `code_hash`) so this is O(1) on the DB side. Returning
/// `Option` lets the caller decide what "not found" means: TODO [16]'s
/// public validate must NOT distinguish "no such code" from "exists but
/// invalid" — both collapse to `{ valid: false }` to avoid leaking
/// existence.
pub async fn find_invitation_by_hash(
    pool: &PgPool,
    code_hash: &str,
) -> Result<Option<InvitationStatusRow>, sqlx::Error> {
    sqlx::query_as::<_, InvitationStatusRow>(
        r#"
        SELECT id, status, expires_at, used_by
        FROM invitations
        WHERE code_hash = $1
        "#,
    )
    .bind(code_hash)
    .fetch_optional(pool)
    .await
}

/// Revoke by row id. Returns the number of rows affected so the handler
/// can distinguish 404 (no such id) from 200 (already revoked / now
/// revoked). Re-revoking is idempotent and still returns 1 here.
pub async fn revoke_invitation(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE invitations
        SET status = 'revoked'
        WHERE id = $1
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ────────────────────────────────────────────────────────────────────────
// HTTP layer — DTOs + handlers (admin-only).
// ────────────────────────────────────────────────────────────────────────

const MAX_GENERATE_COUNT: u32 = 100;
const MAX_EXPIRES_IN_DAYS: i64 = 365;
const DEFAULT_EXPIRES_IN_DAYS: i64 = 7;
const DEFAULT_PAGE: u32 = 1;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 100;

// ── One-shot CSV download (TODO [15]) ──
//
// The download is gated by an unguessable token rather than an HMAC
// signature: a 128-bit OS-random nonce is enough secrecy on its own and
// keeps us from having to manage a signing key. The token is the Redis
// lookup key, and the row is consumed atomically with `GETDEL` — second
// access returns 410 Gone.

/// Bytes of OS randomness in the download token. 16 bytes = 128 bits, hex
/// encoded → a 32-char string. Plenty of entropy against guessing in the
/// 10-minute TTL window.
const DOWNLOAD_TOKEN_BYTES: usize = 16;
const DOWNLOAD_TOKEN_HEX_LEN: usize = DOWNLOAD_TOKEN_BYTES * 2;
const DOWNLOAD_KEY_PREFIX: &str = "invitation_csv:";
const DOWNLOAD_TTL_SECONDS: u64 = 600;

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    pub count: u32,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct GeneratedInvitation {
    pub id: Uuid,
    /// Plaintext code — returned **only** in this response, never persisted.
    pub code: String,
    pub code_prefix: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct GenerateResponse {
    pub invitations: Vec<GeneratedInvitation>,
    /// One-shot CSV link. Path-only, e.g. `/api/invitations/download/{token}`.
    /// `None` if the Redis cache for one-shot CSVs is unreachable — codes
    /// are still returned in `invitations` above, so the admin isn't blocked.
    pub download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

/// Admin-list view item. The shape is hand-written (rather than
/// `Serialize`-derived from `InvitationRow`) so adding a `code_hash` field
/// to the row struct in the future cannot accidentally leak it through
/// this endpoint.
#[derive(Debug, Serialize)]
pub struct InvitationListItem {
    pub id: Uuid,
    pub code_prefix: Option<String>,
    /// Display-friendly masked form, e.g. `"INV-AbCd****"`.
    pub masked: String,
    pub status: String,
    pub used_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub notes: Option<String>,
}

impl From<InvitationRow> for InvitationListItem {
    fn from(row: InvitationRow) -> Self {
        let masked = row
            .code_prefix
            .as_deref()
            .map(|p| format!("{p}****"))
            .unwrap_or_else(|| "****".to_owned());
        Self {
            id: row.id,
            code_prefix: row.code_prefix,
            masked,
            status: row.status,
            used_by: row.used_by,
            created_at: row.created_at.and_utc(),
            expires_at: row.expires_at.and_utc(),
            notes: row.notes,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatsCounts {
    pub total: i64,
    pub used: i64,
    pub unused: i64,
    pub expired: i64,
}

const VALID_STATUSES: &[&str] = &["unused", "used", "expired", "revoked"];

// ── Public validate (TODO [16]) ──
//
// Anyone can call this — the SPA hits it before the user submits the
// register form so a typo in the invitation code surfaces immediately
// instead of after a full round-trip with email + password. Two security
// invariants:
//
// 1. **No existence oracle.** Any code that's not a currently-usable
//    invitation collapses to `{ valid: false }` — no distinction between
//    "no such row", "expired", "already used", or "revoked".
// 2. **Plaintext stays out of URLs.** Code travels in the JSON body, so
//    it cannot leak via access logs or browser history.
//
// Per-IP rate limit (30/min) is applied as middleware in `main.rs`, not
// in this handler — keeping policy out of the request path.

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    pub code: String,
}

/// Public-facing validate response. `expires_at` and `used_by` are
/// skipped when `None` — `{ valid: false }` is a fixed shape that
/// reveals nothing about why the code failed.
#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_by: Option<Uuid>,
}

impl ValidateResponse {
    fn invalid() -> Self {
        Self {
            valid: false,
            expires_at: None,
            used_by: None,
        }
    }

    fn valid_for(expires_at: DateTime<Utc>) -> Self {
        // `used_by` is intentionally `None`: a `valid: true` response
        // implies status='unused', so by definition no one has used it.
        Self {
            valid: true,
            expires_at: Some(expires_at),
            used_by: None,
        }
    }
}

/// Cheap shape check: full plaintext is `INV-` + 20 base62 chars. Codes
/// that fail this can short-circuit to `{ valid: false }` without a DB
/// round-trip — the format itself is public knowledge (see `generate_code`),
/// so refusing early doesn't leak anything new.
fn is_well_formed_code(s: &str) -> bool {
    if s.len() != CODE_PLAINTEXT_PREFIX.len() + CODE_RANDOM_LEN {
        return false;
    }
    if !s.starts_with(CODE_PLAINTEXT_PREFIX) {
        return false;
    }
    s.as_bytes()[CODE_PLAINTEXT_PREFIX.len()..]
        .iter()
        .all(|b| BASE62.contains(b))
}

/// Generate a fresh download token. 32 hex chars from OS randomness.
fn download_token() -> String {
    let mut rng = OsRng;
    let mut buf = [0u8; DOWNLOAD_TOKEN_BYTES];
    rng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Namespaced Redis key for a CSV download. Single source of truth so
/// the store and take sides cannot drift.
fn redis_csv_key(token: &str) -> String {
    format!("{DOWNLOAD_KEY_PREFIX}{token}")
}

/// Build the CSV body for the just-generated batch. Header row +
/// one row per invitation. None of the values can contain commas, quotes,
/// or newlines (UUID, base62 code, base62 prefix, RFC3339 timestamp), so
/// no escaping is needed — keeping the implementation hand-rolled and
/// dependency-free.
fn build_csv(invitations: &[GeneratedInvitation]) -> String {
    let mut out = String::with_capacity(64 + invitations.len() * 96);
    out.push_str("id,code,code_prefix,expires_at\n");
    for inv in invitations {
        out.push_str(&inv.id.to_string());
        out.push(',');
        out.push_str(&inv.code);
        out.push(',');
        out.push_str(&inv.code_prefix);
        out.push(',');
        out.push_str(&inv.expires_at.to_rfc3339());
        out.push('\n');
    }
    out
}

/// Store the CSV body under `redis_csv_key(token)` with a 10-minute TTL.
async fn store_csv_oneshot(
    client: &redis::Client,
    token: &str,
    csv: &str,
) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("SET")
        .arg(redis_csv_key(token))
        .arg(csv)
        .arg("EX")
        .arg(DOWNLOAD_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await
}

/// Fetch and delete in one Redis round-trip via `GETDEL` (Redis 6.2+).
/// Atomicity matters: two simultaneous downloads must not both succeed.
async fn take_csv_oneshot(
    client: &redis::Client,
    token: &str,
) -> redis::RedisResult<Option<String>> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("GETDEL")
        .arg(redis_csv_key(token))
        .query_async::<Option<String>>(&mut conn)
        .await
}

/// POST /api/invitations/generate
async fn generate_handler(
    admin: AdminUser,
    body: web::Json<GenerateRequest>,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let GenerateRequest {
        count,
        notes,
        expires_in_days,
    } = body.into_inner();

    if count == 0 || count > MAX_GENERATE_COUNT {
        return Err(AppError::BadRequest(format!(
            "count must be in 1..={MAX_GENERATE_COUNT}"
        )));
    }
    let ttl_days = expires_in_days.unwrap_or(DEFAULT_EXPIRES_IN_DAYS);
    if ttl_days <= 0 || ttl_days > MAX_EXPIRES_IN_DAYS {
        return Err(AppError::BadRequest(format!(
            "expires_in_days must be in 1..={MAX_EXPIRES_IN_DAYS}"
        )));
    }
    if let Some(n) = notes.as_deref() {
        if n.len() > 255 {
            return Err(AppError::BadRequest("notes must be ≤ 255 chars".into()));
        }
    }

    let expires_at = Utc::now() + chrono::Duration::days(ttl_days);
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let plaintext = generate_code();
        let hash = hash_code(&plaintext);
        let prefix = code_prefix(&plaintext);
        let id = insert_invitation(
            &state.db,
            &hash,
            &prefix,
            admin.user_id,
            expires_at,
            notes.as_deref(),
        )
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("insert invitation: {e}")))?;

        out.push(GeneratedInvitation {
            id,
            code: plaintext,
            code_prefix: prefix,
            expires_at,
        });
    }

    // The CSV link is a convenience layer — codes are already in the JSON
    // response body, so a Redis hiccup must NOT fail the whole call. Log
    // and continue with `download_url: None`; the admin can still record
    // the codes from the JSON or re-generate.
    let csv = build_csv(&out);
    let token = download_token();
    let download_url = match store_csv_oneshot(&state.redis, &token, &csv).await {
        Ok(()) => Some(format!("/api/invitations/download/{token}")),
        Err(e) => {
            log::error!("failed to store invitation CSV in Redis: {e}");
            None
        }
    };

    Ok(ApiResponse::created(GenerateResponse {
        invitations: out,
        download_url,
    }))
}

/// GET /api/invitations
async fn list_handler(
    _admin: AdminUser,
    query: web::Query<ListQuery>,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let ListQuery {
        status,
        page,
        limit,
    } = query.into_inner();

    if let Some(s) = status.as_deref() {
        if !VALID_STATUSES.contains(&s) {
            return Err(AppError::BadRequest(format!(
                "status must be one of {VALID_STATUSES:?}"
            )));
        }
    }
    let page = page.unwrap_or(DEFAULT_PAGE).max(1);
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = i64::from(page - 1) * i64::from(limit);

    let rows = list_invitations(&state.db, status.as_deref(), offset, i64::from(limit))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("list invitations: {e}")))?;

    let items: Vec<InvitationListItem> = rows.into_iter().map(Into::into).collect();
    Ok(ApiResponse::ok(items))
}

/// GET /api/invitations/stats
async fn stats_handler(
    _admin: AdminUser,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let counts = count_invitation_stats(&state.db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("invitation stats: {e}")))?;
    Ok(ApiResponse::ok(counts))
}

/// DELETE /api/invitations/:id
async fn delete_handler(
    _admin: AdminUser,
    path: web::Path<Uuid>,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let id = path.into_inner();
    let affected = revoke_invitation(&state.db, id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("revoke invitation: {e}")))?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("invitation {id} not found")));
    }
    Ok(HttpResponse::NoContent().finish())
}

/// GET /api/invitations/download/:token
///
/// One-shot CSV download. The token is consumed atomically (`GETDEL`),
/// so a second access — even by the same admin — returns 410 Gone.
async fn download_handler(
    _admin: AdminUser,
    path: web::Path<String>,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let token = path.into_inner();

    // Cheap shape check — saves a Redis round-trip on garbage URLs and
    // turns "obviously not from us" into a single 410 path.
    if token.len() != DOWNLOAD_TOKEN_HEX_LEN
        || !token.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(AppError::Gone(
            "download link is invalid or expired".into(),
        ));
    }

    let csv = take_csv_oneshot(&state.redis, &token)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("redis getdel: {e}")))?
        .ok_or_else(|| AppError::Gone("download link is invalid or expired".into()))?;

    let filename = format!("invitations-{}.csv", Utc::now().format("%Y%m%d-%H%M%S"));
    Ok(HttpResponse::Ok()
        .content_type("text/csv; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(csv))
}

/// POST /api/invitations/validate
///
/// Public — no auth required. Mounted in `main.rs` outside `routes()`
/// so it can carry its own per-IP rate limiter and bypass the CSRF
/// middleware (a not-yet-registered visitor has no `csrf_token` cookie).
pub async fn validate_handler(
    body: web::Json<ValidateRequest>,
    state: web::Data<crate::AppState>,
) -> AppResult<HttpResponse> {
    let ValidateRequest { code } = body.into_inner();

    if !is_well_formed_code(&code) {
        return Ok(ApiResponse::ok(ValidateResponse::invalid()));
    }

    let hash = hash_code(&code);
    let row = find_invitation_by_hash(&state.db, &hash)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("validate invitation: {e}")))?;

    // `expires_at` is stored as `TIMESTAMP WITHOUT TIME ZONE` and the
    // project convention is UTC throughout, so `.and_utc()` is the
    // round-trip-safe way to compare against `Utc::now()`.
    let resp = match row {
        Some(inv) if inv.status == "unused" && inv.expires_at.and_utc() > Utc::now() => {
            ValidateResponse::valid_for(inv.expires_at.and_utc())
        }
        _ => ValidateResponse::invalid(),
    };

    Ok(ApiResponse::ok(resp))
}

/// Mounts `/api/invitations/*` (admin-only). Call from `main.rs`:
///
/// ```ignore
/// .service(web::scope("/api/invitations").configure(invitations::routes))
/// ```
///
/// Note: the public `/validate` endpoint is **not** mounted here — it
/// lives in `main.rs` so it can attach its own rate limiter and skip
/// the admin-side wrapping that the rest of this scope assumes.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/generate", web::post().to(generate_handler))
        .route("/stats", web::get().to(stats_handler))
        // Two-segment download path won't collide with the one-segment
        // `/{id}` DELETE pattern below.
        .route("/download/{token}", web::get().to(download_handler))
        .route("/{id}", web::delete().to(delete_handler))
        // The bare-scope GET goes last so the `{id}` pattern doesn't
        // shadow it on `/`.
        .route("", web::get().to(list_handler));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_code_has_expected_shape() {
        let code = generate_code();
        assert!(code.starts_with("INV-"), "missing INV- prefix: {code}");
        assert_eq!(
            code.len(),
            CODE_PLAINTEXT_PREFIX.len() + CODE_RANDOM_LEN,
            "unexpected length: {code}"
        );
        // Every suffix char must be base62.
        let suffix = &code[CODE_PLAINTEXT_PREFIX.len()..];
        for c in suffix.chars() {
            assert!(
                BASE62.contains(&(c as u8)),
                "non-base62 char {c:?} in {code}"
            );
        }
    }

    /// 100 generations, no duplicates. (With a 62^20 search space the odds
    /// of a collision in 100 draws are astronomically small; this asserts
    /// the generator isn't accidentally reusing seeds.)
    #[test]
    fn one_hundred_generations_are_unique() {
        let mut seen = HashSet::with_capacity(100);
        for _ in 0..100 {
            let code = generate_code();
            assert!(seen.insert(code.clone()), "duplicate code generated: {code}");
        }
        assert_eq!(seen.len(), 100);
    }

    #[test]
    fn hash_is_64_lowercase_hex_chars() {
        let code = generate_code();
        let hash = hash_code(&code);
        assert_eq!(hash.len(), 64, "hash length not 64: {hash}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "hash not lowercase hex: {hash}"
        );
    }

    /// Determinism is what makes `code_hash` a usable lookup key in the
    /// invitations table — same plaintext → same hash, every time. This
    /// is the unit-test surface of the "查询时按 hash 命中" acceptance
    /// criterion (the matching DB roundtrip lands with the integration
    /// tests in TODO [47]).
    #[test]
    fn hash_lookup_key_is_deterministic() {
        let code = generate_code();
        assert_eq!(hash_code(&code), hash_code(&code));
        // And distinct plaintext yields a distinct key.
        let other = generate_code();
        assert_ne!(hash_code(&code), hash_code(&other));
    }

    #[test]
    fn hash_matches_known_vector() {
        // SHA-256("abc") — the canonical FIPS 180-2 test vector. Pins our
        // hex encoding to lowercase and confirms we're hashing bytes, not
        // some derived form.
        assert_eq!(
            hash_code("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn code_prefix_is_first_eight_chars() {
        let code = generate_code();
        let prefix = code_prefix(&code);
        assert_eq!(prefix.len(), CODE_PREFIX_LEN);
        assert!(prefix.starts_with("INV-"));
        assert!(code.starts_with(&prefix));
    }

    /// Acceptance criterion for TODO [14]: the admin list endpoint must
    /// never return `code_hash`. A hand-crafted struct (rather than
    /// `Serialize`-derived from `InvitationRow`) is the line of defence;
    /// this test pins it down so a later refactor can't quietly leak the
    /// hash through.
    #[test]
    fn list_item_json_has_no_code_hash_field() {
        let item = InvitationListItem {
            id: Uuid::new_v4(),
            code_prefix: Some("INV-AbCd".into()),
            masked: "INV-AbCd****".into(),
            status: "unused".into(),
            used_by: None,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::days(7),
            notes: Some("test".into()),
        };
        let json = serde_json::to_value(&item).expect("serialize");
        let obj = json.as_object().expect("object");

        assert!(
            !obj.contains_key("code_hash"),
            "code_hash must never appear in list response: {json}"
        );
        // Positive checks — what *should* be there.
        for required in ["id", "code_prefix", "masked", "status", "expires_at"] {
            assert!(obj.contains_key(required), "missing field {required}");
        }
        assert_eq!(obj["masked"], "INV-AbCd****");
    }

    #[test]
    fn masked_falls_back_when_prefix_is_null() {
        let row = InvitationRow {
            id: Uuid::new_v4(),
            code_prefix: None,
            status: "unused".into(),
            used_by: None,
            created_at: Utc::now().naive_utc(),
            expires_at: Utc::now().naive_utc(),
            notes: None,
        };
        let item: InvitationListItem = row.into();
        assert_eq!(item.masked, "****");
    }

    // ── TODO [15]: one-shot CSV download ──

    #[test]
    fn download_token_is_32_hex_chars() {
        let token = download_token();
        assert_eq!(token.len(), DOWNLOAD_TOKEN_HEX_LEN);
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "token not lowercase hex: {token}"
        );
    }

    #[test]
    fn download_tokens_are_unique_across_many_calls() {
        let mut seen = HashSet::with_capacity(100);
        for _ in 0..100 {
            assert!(seen.insert(download_token()));
        }
    }

    #[test]
    fn redis_csv_key_uses_namespaced_prefix() {
        // Pinning the exact key format — a future rename here would
        // silently invalidate every cached CSV until TTL expiry.
        assert_eq!(redis_csv_key("abcd1234"), "invitation_csv:abcd1234");
        assert!(redis_csv_key("anything").starts_with(DOWNLOAD_KEY_PREFIX));
    }

    #[test]
    fn build_csv_has_header_and_one_row_per_invitation() {
        let now = Utc::now();
        let invs = vec![
            GeneratedInvitation {
                id: Uuid::new_v4(),
                code: "INV-AAAAAAAAAAAAAAAAAAAA".into(),
                code_prefix: "INV-AAAA".into(),
                expires_at: now,
            },
            GeneratedInvitation {
                id: Uuid::new_v4(),
                code: "INV-BBBBBBBBBBBBBBBBBBBB".into(),
                code_prefix: "INV-BBBB".into(),
                expires_at: now,
            },
        ];
        let csv = build_csv(&invs);

        let lines: Vec<&str> = csv.split_terminator('\n').collect();
        assert_eq!(lines.len(), 3, "expected 1 header + 2 data rows: {csv}");
        assert_eq!(lines[0], "id,code,code_prefix,expires_at");

        for (i, line) in lines[1..].iter().enumerate() {
            let fields: Vec<&str> = line.split(',').collect();
            assert_eq!(fields.len(), 4, "row {i}: {line}");
            // id is a parseable UUID; code matches what we put in.
            Uuid::parse_str(fields[0]).expect("first field is uuid");
            assert_eq!(fields[1], invs[i].code);
            assert_eq!(fields[2], invs[i].code_prefix);
            // RFC3339 contains 'T'.
            assert!(fields[3].contains('T'), "expected RFC3339 timestamp: {}", fields[3]);
        }
    }

    #[test]
    fn build_csv_empty_list_yields_header_only() {
        let csv = build_csv(&[]);
        assert_eq!(csv, "id,code,code_prefix,expires_at\n");
    }

    #[test]
    fn token_constants_keep_format_invariants() {
        // Cheap consistency check: hex length must be 2 × byte length.
        assert_eq!(DOWNLOAD_TOKEN_HEX_LEN, DOWNLOAD_TOKEN_BYTES * 2);
    }

    // ── TODO [16]: public validate ──

    #[test]
    fn well_formed_code_accepts_freshly_generated_codes() {
        for _ in 0..50 {
            let code = generate_code();
            assert!(is_well_formed_code(&code), "generator output must validate: {code}");
        }
    }

    #[test]
    fn well_formed_code_rejects_obvious_garbage() {
        assert!(!is_well_formed_code(""), "empty");
        assert!(!is_well_formed_code("INV-"), "too short");
        assert!(!is_well_formed_code("INVALID-AAAAAAAAAAAAAAAAAAAA"), "wrong prefix");
        assert!(!is_well_formed_code("inv-AAAAAAAAAAAAAAAAAAAA"), "lowercase prefix");
        assert!(!is_well_formed_code("INV-AAAAAAAAAAAAAAAAAAAAA"), "one too long");
        assert!(!is_well_formed_code("INV-AAAAAAAAAAAAAAAAAAA"), "one too short");
        // Non-base62 char (`!`) in the suffix.
        assert!(!is_well_formed_code("INV-AAAAAAAAAAAAAAAAAAA!"), "non-base62 suffix");
        // Non-ASCII char in the suffix — must reject without panicking.
        assert!(!is_well_formed_code("INV-AAAAAAAAAAAAAAAAAA🦀"), "non-ASCII suffix");
    }

    /// `{ valid: false }` is the *only* shape allowed for invalid codes.
    /// Any extra field would become an existence oracle, defeating the
    /// "不暴露存在性" acceptance criterion.
    #[test]
    fn invalid_response_serializes_to_just_valid_false() {
        let json = serde_json::to_value(ValidateResponse::invalid()).expect("serialize");
        let obj = json.as_object().expect("object");
        assert_eq!(obj.len(), 1, "invalid response must have exactly 1 field: {json}");
        assert_eq!(obj["valid"], false);
        assert!(!obj.contains_key("expires_at"));
        assert!(!obj.contains_key("used_by"));
    }

    /// Valid response carries `expires_at` but never `used_by` — a
    /// usable code is by definition unused.
    #[test]
    fn valid_response_includes_expires_at_but_omits_used_by() {
        let exp = Utc::now() + chrono::Duration::days(3);
        let json = serde_json::to_value(ValidateResponse::valid_for(exp)).expect("serialize");
        let obj = json.as_object().expect("object");
        assert_eq!(obj["valid"], true);
        assert!(obj.contains_key("expires_at"), "expires_at required when valid");
        assert!(!obj.contains_key("used_by"), "valid response must not include used_by");
    }

    /// Pin the exact field set so a future field addition has to be a
    /// deliberate code change with a test update — not a silent leak.
    #[test]
    fn validate_response_field_set_is_locked() {
        let exp = Utc::now() + chrono::Duration::days(7);
        let json = serde_json::to_value(ValidateResponse::valid_for(exp)).expect("serialize");
        let obj = json.as_object().expect("object");
        let allowed = ["valid", "expires_at", "used_by"];
        for key in obj.keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected field: {key}");
        }
    }
}
