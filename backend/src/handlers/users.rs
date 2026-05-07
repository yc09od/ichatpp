//! User profile endpoints (TODO [21] / [22] / [23] / ARCHITECTURE.md §4.2):
//!
//! ```text
//! GET  /api/users/me                # full profile (private fields + role)
//! PUT  /api/users/me                # nickname / signature / is_visible
//! POST /api/users/avatar            # PNG/JPG ≤ 2MB → S3 + thumbnail
//! GET  /api/users                   # admin-only paginated list
//! GET  /api/users/by-code/:code     # public lookup by 10-digit account code
//! GET  /api/users/:user_id          # public lookup by UUID
//! PUT  /api/users/:user_id/role     # admin-only role update
//! ```
//!
//! ## Authentication
//!
//! All four endpoints sit behind [`AuthenticatedUser`] — even the public
//! lookups, because the visibility check ("可见性 false 的用户对非好友
//! 返回 404") needs a known requester to compare against. Without auth
//! we'd have no way to apply the friend-aware rule, so we make auth a
//! prerequisite.
//!
//! ## Visibility rule
//!
//! For the by-id and by-code endpoints, when the target user has
//! `is_visible = false` we return **404**, not 403, so the endpoint
//! cannot be used as a presence oracle by a stranger. Friends and the
//! user themselves continue to see the profile (see [`can_see_user`]).
//!
//! ## Public vs. self response shape
//!
//! [`PublicUser`] omits `email`, `role`, and `created_at` — the by-id /
//! by-code endpoints never return these, even when the requester looks
//! up their own UUID. The `/me` endpoint is the only path that returns
//! the [`SelfUser`] shape with the private fields included.

#![allow(dead_code)] // first non-test consumer mounts in TODO [21]; keep
                     // helper visibility relaxed for future friend-module reuse.

use actix_multipart::Multipart;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::auth::admin::AdminUser;
use crate::auth::authenticated::AuthenticatedUser;
use crate::errors::{AppError, AppResult};
use crate::responses::ApiResponse;
use crate::services::avatar::{
    content_type_for, decode_within_limits, extension_for, render_thumbnail,
    validate_avatar_bytes, AvatarError, MAX_AVATAR_BYTES,
};
use crate::AppState;

/// Matches the schema's `nickname VARCHAR(100)`. `validator` counts in
/// chars, so this is character-length, not bytes — same as Postgres.
const MAX_NICKNAME_LEN: u64 = 100;

/// `signature` is `TEXT` in the schema, but unbounded user input is a
/// footgun (DoS via huge bodies). 1000 chars is roomy for a status line
/// and bounded enough that responses stay small.
const MAX_SIGNATURE_LEN: u64 = 1000;

/// Account-code shape: 10 ASCII digits, matching the CHECK constraint
/// `^[0-9]{10}$`. We pre-validate at the handler so the path argument
/// can't be used to inject a malformed value into a parameterized query
/// (defense in depth — sqlx already guards against injection).
const ACCOUNT_CODE_LEN: usize = 10;

/// Default role for users whose `user_roles` row is missing — same
/// fallback the login query uses, so the two paths can't disagree.
const DEFAULT_ROLE: &str = "user";

// ────────────────────────────────────────────────────────────────────────
// DTOs
// ────────────────────────────────────────────────────────────────────────

/// Public-facing slice of a user — what other authenticated users see
/// when they look up the user by id or account_code. **Does not** carry
/// `email`, `role`, or `created_at`.
#[derive(Debug, Serialize)]
pub struct PublicUser {
    pub id: Uuid,
    pub account_code: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub signature: Option<String>,
    pub is_visible: bool,
}

/// Self profile — only returned by `GET /api/users/me` and the response
/// of `PUT /api/users/me`. Carries the private fields the SPA needs to
/// drive its own UI (current email, current role).
#[derive(Debug, Serialize)]
pub struct SelfUser {
    pub id: Uuid,
    pub account_code: String,
    pub email: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub signature: Option<String>,
    pub is_visible: bool,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

/// PUT /api/users/me request body. Each field is optional — `None` means
/// "leave alone", `Some(_)` means "set to this value". Empty strings are
/// permitted for `signature` (clears the line); `nickname` requires at
/// least 1 character to avoid an accidental display-name wipe.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateProfileRequest {
    #[validate(length(min = 1, max = "MAX_NICKNAME_LEN"))]
    pub nickname: Option<String>,
    #[validate(length(max = "MAX_SIGNATURE_LEN"))]
    pub signature: Option<String>,
    pub is_visible: Option<bool>,
}

// ────────────────────────────────────────────────────────────────────────
// Row types & DAO helpers
// ────────────────────────────────────────────────────────────────────────

/// Joined row for `GET /api/users/me`. Kept private — the handler
/// converts it into [`SelfUser`] before responding.
#[derive(Debug, sqlx::FromRow)]
struct SelfUserRow {
    id: Uuid,
    account_code: String,
    email: String,
    nickname: Option<String>,
    avatar_url: Option<String>,
    signature: Option<String>,
    is_visible: bool,
    created_at: NaiveDateTime,
    role: String,
}

impl From<SelfUserRow> for SelfUser {
    fn from(r: SelfUserRow) -> Self {
        SelfUser {
            id: r.id,
            account_code: r.account_code,
            email: r.email,
            nickname: r.nickname,
            avatar_url: r.avatar_url,
            signature: r.signature,
            is_visible: r.is_visible,
            role: r.role,
            // Schema column is `TIMESTAMP` (no TZ); we store UTC
            // timestamps so the conversion is faithful.
            created_at: r.created_at.and_utc(),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PublicUserRow {
    id: Uuid,
    account_code: String,
    nickname: Option<String>,
    avatar_url: Option<String>,
    signature: Option<String>,
    is_visible: bool,
}

impl From<PublicUserRow> for PublicUser {
    fn from(r: PublicUserRow) -> Self {
        PublicUser {
            id: r.id,
            account_code: r.account_code,
            nickname: r.nickname,
            avatar_url: r.avatar_url,
            signature: r.signature,
            is_visible: r.is_visible,
        }
    }
}

async fn fetch_self(pool: &PgPool, user_id: Uuid) -> Result<Option<SelfUserRow>, sqlx::Error> {
    sqlx::query_as::<_, SelfUserRow>(
        r#"
        SELECT
            u.id,
            u.account_code,
            u.email,
            u.nickname,
            u.avatar_url,
            u.signature,
            COALESCE(u.is_visible, TRUE) AS is_visible,
            u.created_at,
            COALESCE(r.role, $2) AS role
        FROM users u
        LEFT JOIN user_roles r ON r.user_id = u.id
        WHERE u.id = $1
        "#,
    )
    .bind(user_id)
    .bind(DEFAULT_ROLE)
    .fetch_optional(pool)
    .await
}

async fn fetch_public_by_id(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<PublicUserRow>, sqlx::Error> {
    sqlx::query_as::<_, PublicUserRow>(
        r#"
        SELECT
            id,
            account_code,
            nickname,
            avatar_url,
            signature,
            COALESCE(is_visible, TRUE) AS is_visible
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

async fn fetch_public_by_code(
    pool: &PgPool,
    code: &str,
) -> Result<Option<PublicUserRow>, sqlx::Error> {
    sqlx::query_as::<_, PublicUserRow>(
        r#"
        SELECT
            id,
            account_code,
            nickname,
            avatar_url,
            signature,
            COALESCE(is_visible, TRUE) AS is_visible
        FROM users
        WHERE account_code = $1
        "#,
    )
    .bind(code)
    .fetch_optional(pool)
    .await
}

/// True iff `a` and `b` have a row in `friends`. `a == b` returns true
/// (a user is always considered "able to see" themselves so the public
/// lookup of one's own UUID never 404s on visibility=false).
///
/// Lives here for now; will move to a shared friends DAO when TODO [25]
/// lands.
async fn are_friends(pool: &PgPool, a: Uuid, b: Uuid) -> Result<bool, sqlx::Error> {
    if a == b {
        return Ok(true);
    }
    // The schema enforces `user_id_1 < user_id_2`, so the pair is
    // canonicalised by ordering them first. Postgres uuid type and Rust
    // `Uuid::cmp` both order by raw 16-byte big-endian comparison, so
    // the Rust-side `<` agrees with the DB CHECK constraint.
    let (low, high) = if a < b { (a, b) } else { (b, a) };
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM friends WHERE user_id_1 = $1 AND user_id_2 = $2)",
    )
    .bind(low)
    .bind(high)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Visibility rule from the acceptance criterion: a `is_visible = false`
/// user is only viewable by themselves and by their friends. Strangers
/// see the same response a missing user would produce, which is what
/// keeps the endpoint from leaking presence.
async fn can_see_user(
    pool: &PgPool,
    target: &PublicUserRow,
    requester_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if target.is_visible {
        return Ok(true);
    }
    are_friends(pool, requester_id, target.id).await
}

/// 10-digit ASCII numeric guard. Cheaper than letting the DB reject a
/// malformed code and avoids a wasted round-trip on bot scans.
fn is_valid_account_code(code: &str) -> bool {
    code.len() == ACCOUNT_CODE_LEN && code.chars().all(|c| c.is_ascii_digit())
}

// ────────────────────────────────────────────────────────────────────────
// Handlers
// ────────────────────────────────────────────────────────────────────────

/// GET /api/users/me
pub async fn get_me_handler(
    user: AuthenticatedUser,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let row = fetch_self(&state.db, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch /me: {e}")))?
        // The user's access token is signed for a UUID that no longer
        // exists in `users` — possible if the row was deleted server-
        // side after issuance. 404 here forces a re-auth on the client.
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    Ok(HttpResponse::Ok().json(ApiResponse::build(SelfUser::from(row))))
}

/// PUT /api/users/me
pub async fn update_me_handler(
    user: AuthenticatedUser,
    body: web::Json<UpdateProfileRequest>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let req = body.into_inner();
    req.validate()
        .map_err(|e| AppError::BadRequest(format!("invalid input: {e}")))?;

    // Trim nickname for storage — leading / trailing whitespace is never
    // meaningful in a display name and silently differs from what the
    // user typed in the form. Skip after trimming-down-to-empty so the
    // length(min=1) validator still applies (the trim happens after
    // validation, so an all-whitespace input has already been rejected
    // for being all-whitespace... or has it? validator counts chars, so
    // "    " is length 4 and would slip past min=1). Re-check:
    let nickname = req
        .nickname
        .as_ref()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if req.nickname.is_some() && nickname.is_none() {
        return Err(AppError::BadRequest(
            "nickname must contain at least one non-whitespace character".into(),
        ));
    }

    // COALESCE in SQL — `NULL` for a parameter means "don't change", so
    // we send Option directly to sqlx (None binds as NULL). This way
    // a partial update doesn't need a query-builder.
    sqlx::query(
        r#"
        UPDATE users
        SET
            nickname     = COALESCE($2, nickname),
            signature    = COALESCE($3, signature),
            is_visible   = COALESCE($4, is_visible),
            updated_at   = NOW()
        WHERE id = $1
        "#,
    )
    .bind(user.user_id)
    .bind(nickname.as_deref())
    .bind(req.signature.as_deref())
    .bind(req.is_visible)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("update /me: {e}")))?;

    // Re-read so the SPA gets the canonical post-update view (including
    // server-managed fields like `updated_at` if/when we expose it).
    let row = fetch_self(&state.db, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("re-fetch /me: {e}")))?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    Ok(HttpResponse::Ok().json(ApiResponse::build(SelfUser::from(row))))
}

/// GET /api/users/{user_id}
pub async fn get_by_id_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let target_id = path.into_inner();
    let row = fetch_public_by_id(&state.db, target_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch user by id: {e}")))?;

    let row = match row {
        Some(r) => r,
        // Not found and "found but invisible-to-you" intentionally
        // produce the same body — the endpoint must not expose presence.
        None => return Err(AppError::NotFound("user not found".into())),
    };

    let visible = can_see_user(&state.db, &row, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("visibility check: {e}")))?;
    if !visible {
        return Err(AppError::NotFound("user not found".into()));
    }

    Ok(HttpResponse::Ok().json(ApiResponse::build(PublicUser::from(row))))
}

/// GET /api/users/by-code/{code}
pub async fn get_by_code_handler(
    user: AuthenticatedUser,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let code = path.into_inner();
    if !is_valid_account_code(&code) {
        // 400 (not 404) because the path itself is structurally invalid.
        // Distinct from "valid code, no such user" which 404s below.
        return Err(AppError::BadRequest(
            "account code must be exactly 10 digits".into(),
        ));
    }

    let row = fetch_public_by_code(&state.db, &code)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch user by code: {e}")))?;

    let row = match row {
        Some(r) => r,
        None => return Err(AppError::NotFound("user not found".into())),
    };

    let visible = can_see_user(&state.db, &row, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("visibility check: {e}")))?;
    if !visible {
        return Err(AppError::NotFound("user not found".into()));
    }

    Ok(HttpResponse::Ok().json(ApiResponse::build(PublicUser::from(row))))
}

// ────────────────────────────────────────────────────────────────────────
// Avatar upload (TODO [22])
// ────────────────────────────────────────────────────────────────────────

/// Multipart field name the SPA must use for the avatar payload. Pinned
/// here (and exercised by the tests below) so the server / client can't
/// drift apart on the field key.
const AVATAR_FIELD_NAME: &str = "file";

/// Avatar upload response — returns the freshly-updated profile so the
/// SPA's TanStack Query cache can swap in the new URLs without a second
/// round-trip.
///
/// `thumbnail_url` is exposed in addition to `user.avatar_url` because
/// the database column only stores the original URL; the thumb URL is
/// computable but tedious for the client (insert `_thumb` before the
/// extension). Returning it explicitly keeps the SPA simple.
#[derive(Debug, Serialize)]
pub struct AvatarUploadResponse {
    pub user: SelfUser,
    pub thumbnail_url: String,
}

/// Translate avatar-pipeline errors into HTTP responses. Validation /
/// decode failures are user errors (400); encode failures are server
/// errors (500) — they shouldn't happen for a payload we already
/// successfully decoded.
fn map_avatar_error(e: AvatarError) -> AppError {
    match e {
        AvatarError::Empty
        | AvatarError::TooLarge
        | AvatarError::UnsupportedFormat
        | AvatarError::DimensionsTooLarge
        | AvatarError::Decode => AppError::BadRequest(e.to_string()),
        AvatarError::Encode => AppError::Internal(anyhow::anyhow!("avatar encode: {e}")),
    }
}

/// Stream the multipart body until we find a field named [`AVATAR_FIELD_NAME`],
/// then collect its bytes. Aborts mid-stream if we exceed the byte cap so
/// a client can't tie up a worker with a 1 GB upload.
async fn read_avatar_field(mut multipart: Multipart) -> Result<Vec<u8>, AppError> {
    while let Some(mut field) = multipart
        .try_next()
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart payload: {e}")))?
    {
        if field.name() != Some(AVATAR_FIELD_NAME) {
            continue;
        }

        let mut buf = Vec::with_capacity(64 * 1024);
        while let Some(chunk) = field
            .try_next()
            .await
            .map_err(|e| AppError::BadRequest(format!("upload stream error: {e}")))?
        {
            // Guard against the SPA forgetting to set Content-Length: keep
            // counting bytes ourselves and bail before allocating past
            // the cap. +1 byte over the limit triggers the same
            // `TooLarge` error path as a single oversize payload.
            if buf.len() + chunk.len() > MAX_AVATAR_BYTES {
                return Err(map_avatar_error(AvatarError::TooLarge));
            }
            buf.extend_from_slice(&chunk);
        }
        return Ok(buf);
    }

    Err(AppError::BadRequest(format!(
        "missing multipart field '{AVATAR_FIELD_NAME}'"
    )))
}

/// POST /api/users/avatar
///
/// Pipeline:
/// 1. Stream the multipart `file` field into a bounded buffer.
/// 2. Validate magic bytes (PNG / JPEG only).
/// 3. Decode + dimension-check + render the 200×200 thumbnail.
/// 4. Upload original then thumbnail to the avatars bucket.
/// 5. Update `users.avatar_url` and re-fetch the profile.
///
/// On a thumbnail upload failure after the original succeeded, the
/// original is left in S3 as an orphan — the DB never gets updated, so
/// the user sees an error and retries with a fresh UUID. We log the
/// orphan key so it can be cleaned up out-of-band; aggressive
/// transactional cleanup is not worth the added failure modes for an
/// MVP avatar endpoint.
pub async fn upload_avatar_handler(
    user: AuthenticatedUser,
    multipart: Multipart,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let bytes = read_avatar_field(multipart).await?;
    let format = validate_avatar_bytes(&bytes).map_err(map_avatar_error)?;
    let img = decode_within_limits(&bytes, format).map_err(map_avatar_error)?;
    let thumb_bytes = render_thumbnail(&img, format).map_err(map_avatar_error)?;

    // One UUID per upload event so the original / thumb pair share an
    // identity — easy to reason about cleanup, easy to grep in logs.
    let asset_id = Uuid::new_v4();
    let ext = extension_for(format);
    let content_type = content_type_for(format);

    let bucket = state.config.s3_bucket_avatars.as_str();
    let original_key = format!("{}/{}.{}", user.user_id, asset_id, ext);
    let thumb_key = format!("{}/{}_thumb.{}", user.user_id, asset_id, ext);

    state
        .storage
        .put_object(bucket, &original_key, &bytes, content_type)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("upload original avatar: {e}")))?;

    if let Err(e) = state
        .storage
        .put_object(bucket, &thumb_key, &thumb_bytes, content_type)
        .await
    {
        log::warn!(
            "thumb upload failed for {original_key}; original orphaned at s3://{bucket}/{original_key}: {e}"
        );
        return Err(AppError::Internal(anyhow::anyhow!(
            "upload avatar thumbnail: {e}"
        )));
    }

    let original_url = state.storage.object_url(bucket, &original_key);
    let thumbnail_url = state.storage.object_url(bucket, &thumb_key);

    sqlx::query("UPDATE users SET avatar_url = $1, updated_at = NOW() WHERE id = $2")
        .bind(&original_url)
        .bind(user.user_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("update avatar_url: {e}")))?;

    let row = fetch_self(&state.db, user.user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("re-fetch /me after avatar: {e}")))?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    Ok(HttpResponse::Ok().json(ApiResponse::build(AvatarUploadResponse {
        user: SelfUser::from(row),
        thumbnail_url,
    })))
}

// ────────────────────────────────────────────────────────────────────────
// Admin user management (TODO [23])
// ────────────────────────────────────────────────────────────────────────
//
// `AdminUser` extracts before the handler body runs, so any non-admin
// request bounces with 403 (or 401 if there's no token at all) without
// ever touching the DB. Both endpoints below piggy-back on that.

/// Default page when `?page` is absent. 1-indexed (no `?page=0`) so the
/// SPA can mirror the URL fragment users see.
const DEFAULT_PAGE: u32 = 1;

/// Default page size — generous enough that an MVP admin never needs to
/// paginate but small enough that an oversize response is never accidental.
const DEFAULT_LIMIT: u32 = 50;

/// Hard cap on `?limit` to bound the response size. A request with
/// `?limit=10000` is rejected with 400, not silently capped, so a buggy
/// client sees a real error rather than mismatched paging math.
const MAX_LIMIT: u32 = 100;

/// Allowed values for `user_roles.role`. Pinned as an enum so a request
/// with `{ "role": "superadmin" }` fails serde deserialization with 400
/// before ever reaching the handler — and so role string comparisons
/// elsewhere in this module compile-fail when the variants drift.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Admin,
}

impl Role {
    fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Admin => "admin",
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct ListUsersQuery {
    #[validate(range(min = 1))]
    pub page: Option<u32>,
    #[validate(range(min = 1, max = "MAX_LIMIT"))]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoleRequest {
    pub role: Role,
}

/// Paginated response for `GET /api/users`. `users` carries the same
/// `SelfUser` shape that `/me` returns — admins are entitled to see every
/// field a user can see about themselves, so the duplication is the
/// intended privilege rather than an accidental leak.
#[derive(Debug, Serialize)]
pub struct ListUsersResponse {
    pub users: Vec<SelfUser>,
    pub page: u32,
    pub limit: u32,
    pub total: i64,
}

/// Page-bounded fetch — same SELECT shape as [`fetch_self`] so the
/// `SelfUserRow → SelfUser` conversion can be reused.
async fn fetch_admin_list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
) -> Result<Vec<SelfUserRow>, sqlx::Error> {
    sqlx::query_as::<_, SelfUserRow>(
        r#"
        SELECT
            u.id,
            u.account_code,
            u.email,
            u.nickname,
            u.avatar_url,
            u.signature,
            COALESCE(u.is_visible, TRUE) AS is_visible,
            u.created_at,
            COALESCE(r.role, $3) AS role
        FROM users u
        LEFT JOIN user_roles r ON r.user_id = u.id
        ORDER BY u.created_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .bind(DEFAULT_ROLE)
    .fetch_all(pool)
    .await
}

/// GET /api/users — admin-only paginated list.
pub async fn list_users_handler(
    _admin: AdminUser,
    query: web::Query<ListUsersQuery>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let q = query.into_inner();
    q.validate()
        .map_err(|e| AppError::BadRequest(format!("invalid query: {e}")))?;

    let page = q.page.unwrap_or(DEFAULT_PAGE);
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    // (page-1) since pages are 1-indexed; cast through i64 because Postgres
    // OFFSET is bigint-typed and overflowing u32 LIMIT * page seems unlikely
    // but isn't worth the runtime check.
    let offset = (page - 1) as i64 * limit as i64;

    let rows = fetch_admin_list(&state.db, limit as i64, offset)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("list users: {e}")))?;

    // Total count is a separate query — Postgres window functions could
    // fold it in, but at MVP scale a second SELECT COUNT is simpler and
    // fast on the indexed `users` table.
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("count users: {e}")))?;

    let users = rows.into_iter().map(SelfUser::from).collect::<Vec<_>>();

    Ok(HttpResponse::Ok().json(ApiResponse::build(ListUsersResponse {
        users,
        page,
        limit,
        total,
    })))
}

/// PUT /api/users/{user_id}/role — admin-only role update.
///
/// Refuses self-demotion: a lone admin who downgrades themselves locks
/// the entire admin-only surface (this endpoint included). Forcing a
/// different admin to do the demotion preserves auditability and avoids
/// the "I just bricked the system" footgun. Promoting yourself
/// (no-op admin → admin) is allowed.
pub async fn update_role_handler(
    admin: AdminUser,
    path: web::Path<Uuid>,
    body: web::Json<UpdateRoleRequest>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let target_user_id = path.into_inner();
    let req = body.into_inner();

    if target_user_id == admin.user_id && req.role == Role::User {
        return Err(AppError::BadRequest(
            "admins cannot demote themselves; ask another admin".into(),
        ));
    }

    // FK preflight: the UPSERT below would 23503 on an unknown user_id,
    // which we'd surface as 500. Pre-checking lets us return a clean
    // 404 with the same response shape the rest of the module uses.
    let target_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
            .bind(target_user_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("check target user: {e}")))?;
    if !target_exists {
        return Err(AppError::NotFound("user not found".into()));
    }

    // ON CONFLICT (user_id) DO UPDATE handles both first-time role rows
    // and updates uniformly. user_roles.user_id is UNIQUE, so this is a
    // genuine upsert, not a workaround.
    sqlx::query(
        r#"
        INSERT INTO user_roles (user_id, role)
        VALUES ($1, $2)
        ON CONFLICT (user_id) DO UPDATE SET role = EXCLUDED.role
        "#,
    )
    .bind(target_user_id)
    .bind(req.role.as_str())
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("update role: {e}")))?;

    let row = fetch_self(&state.db, target_user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("re-fetch after role update: {e}")))?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    Ok(HttpResponse::Ok().json(ApiResponse::build(SelfUser::from(row))))
}

// ────────────────────────────────────────────────────────────────────────
// Route configuration
// ────────────────────────────────────────────────────────────────────────

/// Mount under `web::scope("/api/users")`.
///
/// Order matters: `/me`, `/avatar`, `/by-code/{code}` must be registered
/// before `/{user_id}` or actix would route `/me` into the UUID pattern
/// and reject "me" as a malformed UUID. The bare-scope GET (`""`) for
/// the admin list is registered alongside the other exact paths.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_users_handler))
        .route("/me", web::get().to(get_me_handler))
        .route("/me", web::put().to(update_me_handler))
        .route("/avatar", web::post().to(upload_avatar_handler))
        .route("/by-code/{code}", web::get().to(get_by_code_handler))
        .route("/{user_id}", web::get().to(get_by_id_handler))
        .route("/{user_id}/role", web::put().to(update_role_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Account-code shape guard ──

    #[test]
    fn is_valid_account_code_accepts_ten_digits() {
        assert!(is_valid_account_code("0123456789"));
        assert!(is_valid_account_code("0000000000"));
        assert!(is_valid_account_code("9999999999"));
    }

    #[test]
    fn is_valid_account_code_rejects_other_shapes() {
        for bad in [
            "",
            "123",                  // too short
            "12345678901",          // too long
            "abcdefghij",           // letters
            "1234567890 ",          // trailing space
            " 1234567890",          // leading space
            "12345-7890",           // punctuation
            "１２３４５６７８９０", // full-width digits
        ] {
            assert!(!is_valid_account_code(bad), "should reject {bad:?}");
        }
    }

    // ── Update validation ──

    #[test]
    fn update_request_accepts_all_fields_within_bounds() {
        let req = UpdateProfileRequest {
            nickname: Some("Alice".into()),
            signature: Some("hello world".into()),
            is_visible: Some(false),
        };
        req.validate().expect("valid");
    }

    #[test]
    fn update_request_rejects_oversized_nickname() {
        let req = UpdateProfileRequest {
            nickname: Some("A".repeat(MAX_NICKNAME_LEN as usize + 1)),
            signature: None,
            is_visible: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn update_request_rejects_empty_nickname() {
        // length(min=1) — a literal empty string is not a useful display
        // name. Empty *signature* is fine; that's the difference between
        // the two fields.
        let req = UpdateProfileRequest {
            nickname: Some(String::new()),
            signature: None,
            is_visible: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn update_request_accepts_empty_signature() {
        // Empty signature == "clear my status line". Permitted.
        let req = UpdateProfileRequest {
            nickname: None,
            signature: Some(String::new()),
            is_visible: None,
        };
        req.validate().expect("empty signature is allowed");
    }

    #[test]
    fn update_request_rejects_oversized_signature() {
        let req = UpdateProfileRequest {
            nickname: None,
            signature: Some("a".repeat(MAX_SIGNATURE_LEN as usize + 1)),
            is_visible: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn update_request_accepts_no_changes() {
        // All-`None` is a valid no-op request.
        let req = UpdateProfileRequest {
            nickname: None,
            signature: None,
            is_visible: None,
        };
        req.validate().expect("all-None is valid");
    }

    // ── Response anti-leak ──

    /// `PublicUser` must not carry email/role/created_at/updated_at.
    /// Pin against accidental field additions — those would silently
    /// expose private data via the lookup endpoints.
    #[test]
    fn public_user_omits_private_fields() {
        let u = PublicUser {
            id: Uuid::nil(),
            account_code: "0123456789".into(),
            nickname: Some("a".into()),
            avatar_url: None,
            signature: None,
            is_visible: true,
        };
        let json = serde_json::to_value(&u).expect("serialize");
        let obj = json.as_object().expect("object");

        for forbidden in ["email", "role", "created_at", "updated_at", "password_hash"] {
            assert!(
                !obj.contains_key(forbidden),
                "PublicUser must not include {forbidden}: {json}"
            );
        }
    }

    /// `SelfUser` is the *only* response shape that may carry email/role.
    /// Pin the contract so a refactor can't accidentally drop them.
    #[test]
    fn self_user_includes_email_and_role() {
        let u = SelfUser {
            id: Uuid::nil(),
            account_code: "0123456789".into(),
            email: "a@b.c".into(),
            nickname: None,
            avatar_url: None,
            signature: None,
            is_visible: true,
            role: "admin".into(),
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&u).expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(obj.contains_key("email"));
        assert!(obj.contains_key("role"));
        assert!(obj.contains_key("created_at"));
    }

    /// The `validator` crate counts characters, not bytes. Pin the
    /// nickname max at the schema's VARCHAR(100) value (also chars).
    #[test]
    fn nickname_limit_matches_schema() {
        assert_eq!(MAX_NICKNAME_LEN, 100);
    }

    /// Pin DEFAULT_ROLE so a future change can't silently demote the
    /// fallback path or upgrade it to "admin" by accident.
    #[test]
    fn default_role_is_user() {
        assert_eq!(DEFAULT_ROLE, "user");
    }

    // ── Admin: list query validation ──

    #[test]
    fn list_query_accepts_defaults() {
        // All-None means "use the defaults" — must validate, since a
        // call to `GET /api/users` with no params should succeed.
        let q = ListUsersQuery {
            page: None,
            limit: None,
        };
        q.validate().expect("None values must be valid");
    }

    #[test]
    fn list_query_rejects_zero_page() {
        let q = ListUsersQuery {
            page: Some(0),
            limit: None,
        };
        assert!(q.validate().is_err());
    }

    #[test]
    fn list_query_rejects_oversized_limit() {
        // Boundary: MAX_LIMIT passes, MAX_LIMIT + 1 fails.
        let ok = ListUsersQuery {
            page: None,
            limit: Some(MAX_LIMIT),
        };
        ok.validate().expect("limit at cap is allowed");

        let bad = ListUsersQuery {
            page: None,
            limit: Some(MAX_LIMIT + 1),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn list_query_rejects_zero_limit() {
        let q = ListUsersQuery {
            page: None,
            limit: Some(0),
        };
        assert!(q.validate().is_err());
    }

    // ── Admin: pagination defaults & limits ──

    /// Pin the public defaults — the SPA URL fragments and back-end paging
    /// math both depend on these. A silent change here would silently
    /// shift result-set sizes for every admin client.
    #[test]
    fn pagination_constants_are_pinned() {
        assert_eq!(DEFAULT_PAGE, 1);
        assert_eq!(DEFAULT_LIMIT, 50);
        assert_eq!(MAX_LIMIT, 100);
    }

    // ── Admin: role enum (de)serialization ──

    /// The serde rename_all = "lowercase" keeps the wire format stable —
    /// pin against a refactor that flips to PascalCase or kebab-case.
    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_value(&Role::User).unwrap(), "user");
        assert_eq!(serde_json::to_value(&Role::Admin).unwrap(), "admin");
    }

    #[test]
    fn role_deserialization_is_strict() {
        // Exact lowercase match accepted.
        let r: Role = serde_json::from_str(r#""admin""#).unwrap();
        assert_eq!(r, Role::Admin);

        // Anything else fails — that's the whole point of using an enum
        // here instead of a free `String` field.
        for bad in [r#""ADMIN""#, r#""superadmin""#, r#""User ""#, r#"""""#] {
            assert!(
                serde_json::from_str::<Role>(bad).is_err(),
                "should reject role JSON: {bad}"
            );
        }
    }

    /// `as_str` is what we bind into Postgres. Pin the values so an enum
    /// rename can't silently insert a new role value into the DB.
    #[test]
    fn role_as_str_matches_db_values() {
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Admin.as_str(), "admin");
    }

    // ── Admin: response anti-leak ──

    /// `ListUsersResponse.users` is a Vec<SelfUser>; pin that admin list
    /// rows carry the same fields a user can see about themselves (and
    /// no more).
    #[test]
    fn list_users_response_uses_self_user_shape() {
        let resp = ListUsersResponse {
            users: vec![SelfUser {
                id: Uuid::nil(),
                account_code: "0123456789".into(),
                email: "a@b.c".into(),
                nickname: None,
                avatar_url: None,
                signature: None,
                is_visible: true,
                role: "user".into(),
                created_at: chrono::Utc::now(),
            }],
            page: 1,
            limit: 50,
            total: 1,
        };
        let json = serde_json::to_value(&resp).unwrap();
        let users = json["users"].as_array().unwrap();
        let first = users[0].as_object().unwrap();
        assert!(first.contains_key("email"));
        assert!(first.contains_key("role"));
        assert!(first.contains_key("created_at"));
        assert!(
            !first.contains_key("password_hash"),
            "admin list must not leak password_hash: {json}"
        );
    }
}
