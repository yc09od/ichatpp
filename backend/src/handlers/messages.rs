//! Message history, search, soft-delete, and export endpoints
//! (TODOs [31]–[34]; ARCHITECTURE.md §4.4).
//!
//! ```text
//! GET    /api/messages/{friend_id}              # cursor-paginated history
//! GET    /api/messages/search                   # full-text search
//! DELETE /api/messages/{message_id}             # sender-only soft delete
//! POST   /api/messages/export                   # one-shot json/csv download
//! GET    /api/messages/download/{token}         # one-shot signed download
//! ```
//!
//! ## Conversation matching
//!
//! All conversation queries match by the canonical `(LEAST, GREATEST)`
//! pair so that a single index pair (`idx_messages_conversation` and
//! `_reverse`) covers both directions A→B and B→A. The Rust side mirrors
//! `Uuid::cmp`, which orders 16-byte big-endian (same as Postgres uuid),
//! so the WHERE clause is index-friendly in either direction.
//!
//! ## Soft-delete posture
//!
//! `is_deleted = TRUE` rows are returned with `content` blanked to an
//! empty string so the SPA can render the spec's "消息已撤回" placeholder
//! without leaking the original text. Nothing here purges the row — the
//! conversation timeline still shows a hole at the right place.

#![allow(dead_code)] // routes mount in main.rs once the module is wired.

use std::collections::HashMap;

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::authenticated::AuthenticatedUser;
use crate::errors::{AppError, AppResult};
use crate::responses::ApiResponse;
use crate::AppState;

// ────────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────────

/// Default page size for `GET /api/messages/{friend_id}`. Matches the
/// TODO ("`limit=50`").
const DEFAULT_HISTORY_LIMIT: u32 = 50;
/// Hard cap on the requested page size — bounds response time and the
/// number of emoji rows we may have to fan out per page.
const MAX_HISTORY_LIMIT: u32 = 200;

/// Default + cap for the search endpoint. Search results are typically
/// browsed not paged-through, so the cap is tighter than history.
const DEFAULT_SEARCH_LIMIT: u32 = 30;
const MAX_SEARCH_LIMIT: u32 = 100;

/// Length of the `ts_headline` snippet PostgreSQL emits for hits. Long
/// enough to give context, short enough that 100 hits don't blow past
/// a few hundred KB of response.
const HEADLINE_MAX_WORDS: u32 = 30;
const HEADLINE_MIN_WORDS: u32 = 8;

/// Bytes of OS randomness in the export-download token. 16 bytes = 128
/// bits, hex-encoded to 32 chars — same shape as the invitation CSV
/// download token from TODO [15] so URL handling stays consistent.
const DOWNLOAD_TOKEN_BYTES: usize = 16;

/// TTL on the export download URL. The TODO calls out 30 minutes; this
/// is also the Redis SET EX value, so the link auto-expires even if no
/// one ever clicks it.
const EXPORT_TTL_SECONDS: u64 = 30 * 60;

/// Hard cap on rows in a single export. Refuses pathological requests
/// (e.g. an admin tool exporting an entire account) before we burn time
/// streaming millions of rows out of Postgres.
const MAX_EXPORT_ROWS: i64 = 100_000;

/// Redis key namespace for a one-shot export download. Mirrors the
/// invitation download prefix.
const EXPORT_KEY_PREFIX: &str = "export:";

/// Placeholder content surfaced for soft-deleted rows. The SPA looks at
/// `is_deleted` to decide whether to render this — but we still blank
/// the field server-side so even a buggy client cannot render the
/// original.
const DELETED_PLACEHOLDER: &str = "";

// ────────────────────────────────────────────────────────────────────────
// DTOs
// ────────────────────────────────────────────────────────────────────────

/// One historical message, as returned by the history and search
/// endpoints. `emoji_ids` is filled in by a follow-up query so the
/// caller can render emoji links without a second round-trip per row.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct MessageView {
    pub id: Uuid,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    pub content: String,
    pub is_deleted: bool,
    pub emoji_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub messages: Vec<MessageView>,
    /// Opaque cursor to pass back as `?cursor=` for the next page; `None`
    /// when there are no more rows.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Search hit. `headline` is the highlighted snippet `ts_headline`
/// produced — wraps matched terms in `<mark>…</mark>` HTML; the SPA is
/// expected to render this as innerHTML inside a sanitised wrapper, or
/// to strip the tags if it doesn't trust them. `<mark>` is on the
/// allowlist so this is safe under standard sanitisers.
#[derive(Debug, Serialize, PartialEq)]
pub struct SearchHit {
    #[serde(flatten)]
    pub message: MessageView,
    pub headline: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    /// Only return messages where `from_user_id == from`. UUID, optional.
    pub from: Option<Uuid>,
    /// Only return messages where `to_user_id == to`.
    pub to: Option<Uuid>,
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
}

/// `format` for the export endpoint. Locked down to the two values the
/// TODO calls out; serde rejects anything else with 400.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Json,
    Csv,
}

#[derive(Debug, Deserialize)]
pub struct ExportRequest {
    pub friend_id: Uuid,
    pub format: ExportFormat,
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ExportResponse {
    /// Path-only URL the SPA can open in a new tab. Single-use; expires
    /// after [`EXPORT_TTL_SECONDS`].
    pub download_url: String,
    pub expires_at: DateTime<Utc>,
    pub format: ExportFormat,
    pub row_count: i64,
}

// ────────────────────────────────────────────────────────────────────────
// SQL row types
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow, Clone)]
struct MessageRow {
    id: Uuid,
    from_user_id: Uuid,
    to_user_id: Uuid,
    content: String,
    is_deleted: bool,
    created_at: NaiveDateTime,
}

#[derive(Debug, sqlx::FromRow, Clone)]
struct SearchRow {
    id: Uuid,
    from_user_id: Uuid,
    to_user_id: Uuid,
    content: String,
    is_deleted: bool,
    created_at: NaiveDateTime,
    headline: String,
}

impl MessageRow {
    fn into_view(self, emoji_ids: Vec<Uuid>) -> MessageView {
        let (content, is_deleted) = if self.is_deleted {
            (DELETED_PLACEHOLDER.to_owned(), true)
        } else {
            (self.content, false)
        };
        MessageView {
            id: self.id,
            from_user_id: self.from_user_id,
            to_user_id: self.to_user_id,
            content,
            is_deleted,
            emoji_ids,
            created_at: self.created_at.and_utc(),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Cursor encoding (TODO [31])
// ────────────────────────────────────────────────────────────────────────

/// Cursors are deliberately readable — they're not secrets and we don't
/// need the client to be able to forge anything dangerous from them
/// (the friend_id in the path scopes every query). `<unix_micros>_<uuid>`
/// is unambiguous and works without a base64 dep.
fn encode_cursor(t: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", t.timestamp_micros(), id)
}

fn decode_cursor(raw: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (ts, id) = raw.split_once('_')?;
    let micros: i64 = ts.parse().ok()?;
    let uuid = Uuid::parse_str(id).ok()?;
    let when = DateTime::<Utc>::from_timestamp_micros(micros)?;
    Some((when, uuid))
}

// ────────────────────────────────────────────────────────────────────────
// Friendship + emoji-link helpers
// ────────────────────────────────────────────────────────────────────────

/// Same canonical-pair friendship check the WS layer uses; redeclared
/// locally so handlers/messages doesn't depend on ws/chat.
async fn are_friends(pool: &PgPool, a: Uuid, b: Uuid) -> Result<bool, sqlx::Error> {
    if a == b {
        return Ok(false);
    }
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM friends
            WHERE user_id_1 = LEAST($1::uuid, $2::uuid)
              AND user_id_2 = GREATEST($1::uuid, $2::uuid)
         )",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
}

/// Bulk-fetch emoji_ids for a set of message ids, preserving the
/// per-message `position` order. One round-trip regardless of page size.
async fn fetch_emoji_ids_for(
    pool: &PgPool,
    message_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<Uuid>>, sqlx::Error> {
    let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::with_capacity(message_ids.len());
    if message_ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        r#"
        SELECT message_id, emoji_id
        FROM message_emojis
        WHERE message_id = ANY($1)
        ORDER BY message_id, position
        "#,
    )
    .bind(message_ids)
    .fetch_all(pool)
    .await?;
    for (msg_id, emoji_id) in rows {
        out.entry(msg_id).or_default().push(emoji_id);
    }
    Ok(out)
}

// ────────────────────────────────────────────────────────────────────────
// History (TODO [31])
// ────────────────────────────────────────────────────────────────────────

/// Fetch up to `limit + 1` rows ordered newest-first, optionally before
/// a `(created_at, id)` cursor. The `+1` is the standard "look-ahead"
/// trick: if we got back `limit + 1` rows, the trailing one becomes the
/// next cursor and is dropped from the response.
async fn fetch_history_page(
    pool: &PgPool,
    me: Uuid,
    friend: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    // `(LEAST, GREATEST)` lets the query plan pick whichever of the two
    // conversation indexes hits its leading column first; the WHERE
    // covers both A→B and B→A rows.
    let (lo, hi) = if me < friend { (me, friend) } else { (friend, me) };
    match cursor {
        None => sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT id, from_user_id, to_user_id, content, is_deleted, created_at
            FROM messages
            WHERE LEAST(from_user_id, to_user_id) = $1
              AND GREATEST(from_user_id, to_user_id) = $2
            ORDER BY created_at DESC, id DESC
            LIMIT $3
            "#,
        )
        .bind(lo)
        .bind(hi)
        .bind(limit)
        .fetch_all(pool)
        .await,
        Some((ts, id)) => sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT id, from_user_id, to_user_id, content, is_deleted, created_at
            FROM messages
            WHERE LEAST(from_user_id, to_user_id) = $1
              AND GREATEST(from_user_id, to_user_id) = $2
              AND (created_at, id) < ($3, $4)
            ORDER BY created_at DESC, id DESC
            LIMIT $5
            "#,
        )
        .bind(lo)
        .bind(hi)
        .bind(ts.naive_utc())
        .bind(id)
        .bind(limit)
        .fetch_all(pool)
        .await,
    }
}

/// `GET /api/messages/{friend_id}?cursor=&limit=`
pub async fn history_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    query: web::Query<HistoryQuery>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let friend_id = path.into_inner();
    let q = query.into_inner();

    // Self-conversation isn't a thing — bounce early so the index hit
    // doesn't waste time on a (lo == hi) pair that can never have rows.
    if friend_id == user.user_id {
        return Err(AppError::BadRequest(
            "cannot query a conversation with yourself".into(),
        ));
    }

    // Limit clamp: 0 → default; > MAX → 400 (loud rather than silent so
    // a buggy client surfaces the bug immediately).
    let limit = match q.limit {
        None | Some(0) => DEFAULT_HISTORY_LIMIT,
        Some(n) if n > MAX_HISTORY_LIMIT => {
            return Err(AppError::BadRequest(format!(
                "limit must be ≤ {MAX_HISTORY_LIMIT}"
            )));
        }
        Some(n) => n,
    };

    let cursor = match q.cursor.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(decode_cursor(raw).ok_or_else(|| {
            AppError::BadRequest("malformed cursor".into())
        })?),
    };

    // Friendship gate. Even on a friendship that's been removed, history
    // is still readable — the TODO for [25] explicitly says deleting a
    // friend leaves messages queryable. So we *don't* enforce friendship
    // here; instead the messages-belong-to-me check is implicit in the
    // `(LEAST/GREATEST)` filter (the user's own UUID must be on one side
    // of every row).

    let mut rows = fetch_history_page(
        &state.db,
        user.user_id,
        friend_id,
        cursor,
        (limit + 1) as i64,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("history page: {e}")))?;

    // Look-ahead trick: if we got `limit + 1` rows, the trailing row
    // becomes the cursor and is dropped from the response.
    let next_cursor = if rows.len() > limit as usize {
        let last = rows.pop().expect("len > limit so non-empty");
        Some(encode_cursor(last.created_at.and_utc(), last.id))
    } else {
        None
    };

    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut emoji_map = fetch_emoji_ids_for(&state.db, &ids)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("history emoji fetch: {e}")))?;

    let messages = rows
        .into_iter()
        .map(|r| {
            let emojis = emoji_map.remove(&r.id).unwrap_or_default();
            r.into_view(emojis)
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiResponse::build(HistoryResponse {
        messages,
        next_cursor,
    })))
}

// ────────────────────────────────────────────────────────────────────────
// Search (TODO [32])
// ────────────────────────────────────────────────────────────────────────

/// Build the conversation search WHERE clause. Returns the SQL fragment
/// and a closure that binds parameters in the same order — kept as one
/// unit so the search count and search rows queries can't drift.
///
/// The user is implicitly scoped to messages they're a party to: every
/// search row must have `me` on either side. This is the security
/// boundary — without it, the FTS index would let any user search every
/// message in the system.
// Eight params: query needle, identity, four filters, plus limit. Fewer
// would mean either packing them into a struct (more boilerplate than
// the call sites are worth right now) or inlining the function (loses
// the count-vs-rows symmetry). Suppress the lint locally.
#[allow(clippy::too_many_arguments)]
async fn search_messages(
    pool: &PgPool,
    me: Uuid,
    q: &str,
    from: Option<Uuid>,
    to: Option<Uuid>,
    date_from: Option<DateTime<Utc>>,
    date_to: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<(Vec<SearchRow>, i64), sqlx::Error> {
    // `plainto_tsquery('simple', $q)` turns user input into a tsquery
    // that ANDs all whitespace-separated tokens — exactly the behaviour
    // most chat searches expect. `simple` matches the index config so
    // the GIN index is usable.
    //
    // `ts_headline` returns the original content with `<mark>` around
    // matches; configured options bound the snippet length.
    let rows: Vec<SearchRow> = sqlx::query_as(
        r#"
        SELECT
            id,
            from_user_id,
            to_user_id,
            content,
            is_deleted,
            created_at,
            ts_headline(
                'simple',
                content,
                plainto_tsquery('simple', $1),
                'StartSel=<mark>, StopSel=</mark>, MaxWords=' || $7 || ', MinWords=' || $8
            ) AS headline
        FROM messages
        WHERE NOT is_deleted
          AND (from_user_id = $2 OR to_user_id = $2)
          AND ($3::uuid IS NULL OR from_user_id = $3)
          AND ($4::uuid IS NULL OR to_user_id   = $4)
          AND ($5::timestamp IS NULL OR created_at >= $5)
          AND ($6::timestamp IS NULL OR created_at <  $6)
          AND to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
        ORDER BY created_at DESC, id DESC
        LIMIT $9
        "#,
    )
    .bind(q)
    .bind(me)
    .bind(from)
    .bind(to)
    .bind(date_from.map(|d| d.naive_utc()))
    .bind(date_to.map(|d| d.naive_utc()))
    .bind(HEADLINE_MAX_WORDS as i32)
    .bind(HEADLINE_MIN_WORDS as i32)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM messages
        WHERE NOT is_deleted
          AND (from_user_id = $2 OR to_user_id = $2)
          AND ($3::uuid IS NULL OR from_user_id = $3)
          AND ($4::uuid IS NULL OR to_user_id   = $4)
          AND ($5::timestamp IS NULL OR created_at >= $5)
          AND ($6::timestamp IS NULL OR created_at <  $6)
          AND to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
        "#,
    )
    .bind(q)
    .bind(me)
    .bind(from)
    .bind(to)
    .bind(date_from.map(|d| d.naive_utc()))
    .bind(date_to.map(|d| d.naive_utc()))
    .fetch_one(pool)
    .await?;

    Ok((rows, total))
}

/// `GET /api/messages/search?q=&from=&to=&date_from=&date_to=`
pub async fn search_handler(
    user: AuthenticatedUser,
    query: web::Query<SearchQuery>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let q = query.into_inner();

    // The TODO acceptance criterion calls out: "空 q 返回 400". A request
    // with no `q` param OR an empty/whitespace string is the same shape
    // of mistake.
    let needle = q.q.as_deref().unwrap_or("").trim();
    if needle.is_empty() {
        return Err(AppError::BadRequest("q parameter is required".into()));
    }

    if let (Some(from), Some(to)) = (q.date_from, q.date_to) {
        if from >= to {
            return Err(AppError::BadRequest(
                "date_from must be before date_to".into(),
            ));
        }
    }

    let limit = match q.limit {
        None | Some(0) => DEFAULT_SEARCH_LIMIT,
        Some(n) if n > MAX_SEARCH_LIMIT => {
            return Err(AppError::BadRequest(format!(
                "limit must be ≤ {MAX_SEARCH_LIMIT}"
            )));
        }
        Some(n) => n,
    };

    let (rows, total) = search_messages(
        &state.db,
        user.user_id,
        needle,
        q.from,
        q.to,
        q.date_from,
        q.date_to,
        limit as i64,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("message search: {e}")))?;

    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut emoji_map = fetch_emoji_ids_for(&state.db, &ids)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("search emoji fetch: {e}")))?;

    let hits = rows
        .into_iter()
        .map(|r| {
            let emojis = emoji_map.remove(&r.id).unwrap_or_default();
            let message = MessageRow {
                id: r.id,
                from_user_id: r.from_user_id,
                to_user_id: r.to_user_id,
                content: r.content,
                is_deleted: r.is_deleted,
                created_at: r.created_at,
            }
            .into_view(emojis);
            SearchHit {
                message,
                headline: r.headline,
            }
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiResponse::build(SearchResponse { hits, total })))
}

// ────────────────────────────────────────────────────────────────────────
// Soft delete (TODO [33])
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct DeleteAuthRow {
    from_user_id: Uuid,
    is_deleted: bool,
}

/// `DELETE /api/messages/{message_id}`
///
/// Sender-only soft delete. Unknown id → 404. Not-the-sender → 403.
/// Already-deleted → 204 (idempotent so a double-click doesn't surface
/// a confusing 4xx).
pub async fn delete_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let message_id = path.into_inner();

    let row: Option<DeleteAuthRow> = sqlx::query_as(
        "SELECT from_user_id, is_deleted FROM messages WHERE id = $1",
    )
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("lookup message: {e}")))?;

    let row = row.ok_or_else(|| AppError::NotFound("message not found".into()))?;

    if row.from_user_id != user.user_id {
        // 403 (not 404) so a sender knows the row exists but they can't
        // act on it; the existence isn't a secret since both parties to
        // a conversation already know about every message via history.
        return Err(AppError::Forbidden(
            "only the sender can delete a message".into(),
        ));
    }

    if row.is_deleted {
        return Ok(HttpResponse::NoContent().finish());
    }

    // Note: we *don't* clear `content` in the DB — keeping the original
    // means a future audit / restore tool could surface it. Read-side
    // queries blank it via `into_view`, which is the only place the SPA
    // ever reads from.
    sqlx::query("UPDATE messages SET is_deleted = TRUE, updated_at = NOW() WHERE id = $1")
        .bind(message_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("soft-delete message: {e}")))?;

    Ok(HttpResponse::NoContent().finish())
}

// ────────────────────────────────────────────────────────────────────────
// Export (TODO [34])
// ────────────────────────────────────────────────────────────────────────

fn export_token() -> String {
    let mut buf = [0u8; DOWNLOAD_TOKEN_BYTES];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn redis_export_key(token: &str) -> String {
    format!("{EXPORT_KEY_PREFIX}{token}")
}

/// One-shot Redis storage for the rendered export body. SET EX so the
/// key auto-expires even if no one ever clicks the link.
async fn store_export_oneshot(
    client: &redis::Client,
    token: &str,
    content_type: &str,
    body: &[u8],
) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    // Pack content-type + body together so the download handler doesn't
    // need a second key. `\n` separator is safe — content-type strings
    // don't contain newlines.
    let mut payload = Vec::with_capacity(body.len() + content_type.len() + 1);
    payload.extend_from_slice(content_type.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(body);
    redis::cmd("SET")
        .arg(redis_export_key(token))
        .arg(payload)
        .arg("EX")
        .arg(EXPORT_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await
}

/// Atomic GETDEL — second access returns 410 because the key is already
/// gone after the first.
async fn take_export_oneshot(
    client: &redis::Client,
    token: &str,
) -> redis::RedisResult<Option<Vec<u8>>> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("GETDEL")
        .arg(redis_export_key(token))
        .query_async::<Option<Vec<u8>>>(&mut conn)
        .await
}

/// Fetch the entire conversation in time-ascending order (export reads
/// chronologically, opposite of history). Capped at [`MAX_EXPORT_ROWS`].
async fn fetch_export_rows(
    pool: &PgPool,
    me: Uuid,
    friend: Uuid,
    date_from: Option<DateTime<Utc>>,
    date_to: Option<DateTime<Utc>>,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    let (lo, hi) = if me < friend { (me, friend) } else { (friend, me) };
    sqlx::query_as::<_, MessageRow>(
        r#"
        SELECT id, from_user_id, to_user_id, content, is_deleted, created_at
        FROM messages
        WHERE LEAST(from_user_id, to_user_id) = $1
          AND GREATEST(from_user_id, to_user_id) = $2
          AND ($3::timestamp IS NULL OR created_at >= $3)
          AND ($4::timestamp IS NULL OR created_at <  $4)
        ORDER BY created_at ASC, id ASC
        LIMIT $5
        "#,
    )
    .bind(lo)
    .bind(hi)
    .bind(date_from.map(|d| d.naive_utc()))
    .bind(date_to.map(|d| d.naive_utc()))
    .bind(MAX_EXPORT_ROWS)
    .fetch_all(pool)
    .await
}

/// Render the rows to the requested format. Both branches preallocate
/// roughly the right size to keep allocator churn low on big exports.
fn render_export(rows: &[MessageView], format: ExportFormat) -> (Vec<u8>, &'static str) {
    match format {
        ExportFormat::Json => {
            // Wrap in a top-level object so future fields (export
            // timestamp, friend metadata) can be added without breaking
            // existing parsers.
            let body = serde_json::to_vec_pretty(&serde_json::json!({ "messages": rows }))
                .expect("MessageView serialises");
            (body, "application/json")
        }
        ExportFormat::Csv => {
            let mut out = String::with_capacity(64 + rows.len() * 96);
            out.push_str("id,from_user_id,to_user_id,created_at,is_deleted,content\n");
            for r in rows {
                out.push_str(&r.id.to_string());
                out.push(',');
                out.push_str(&r.from_user_id.to_string());
                out.push(',');
                out.push_str(&r.to_user_id.to_string());
                out.push(',');
                out.push_str(&r.created_at.to_rfc3339());
                out.push(',');
                out.push_str(if r.is_deleted { "true" } else { "false" });
                out.push(',');
                out.push_str(&csv_escape(&r.content));
                out.push('\n');
            }
            (out.into_bytes(), "text/csv; charset=utf-8")
        }
    }
}

/// Minimal RFC 4180 CSV escaping: wrap in quotes and double internal
/// quotes if the value contains a comma, quote, or newline. Keeps unquoted
/// values for the common case (most chat lines are plain text).
fn csv_escape(s: &str) -> String {
    let needs_quoting = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs_quoting {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// `POST /api/messages/export`
pub async fn export_handler(
    user: AuthenticatedUser,
    body: web::Json<ExportRequest>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let req = body.into_inner();

    if req.friend_id == user.user_id {
        return Err(AppError::BadRequest(
            "cannot export a conversation with yourself".into(),
        ));
    }
    if let (Some(from), Some(to)) = (req.date_from, req.date_to) {
        if from >= to {
            return Err(AppError::BadRequest(
                "date_from must be before date_to".into(),
            ));
        }
    }

    // Friendship is *not* required (consistent with history): exports of
    // an old conversation must work after un-friending. The user-on-side
    // filter in fetch_export_rows is the security boundary.

    let rows = fetch_export_rows(
        &state.db,
        user.user_id,
        req.friend_id,
        req.date_from,
        req.date_to,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch export: {e}")))?;

    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut emoji_map = fetch_emoji_ids_for(&state.db, &ids)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("export emoji fetch: {e}")))?;

    let views: Vec<MessageView> = rows
        .into_iter()
        .map(|r| {
            let emojis = emoji_map.remove(&r.id).unwrap_or_default();
            r.into_view(emojis)
        })
        .collect();
    let row_count = views.len() as i64;

    let (body, content_type) = render_export(&views, req.format);

    let token = export_token();
    store_export_oneshot(&state.redis, &token, content_type, &body)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("store export: {e}")))?;

    let expires_at = Utc::now() + chrono::Duration::seconds(EXPORT_TTL_SECONDS as i64);

    Ok(HttpResponse::Ok().json(ApiResponse::build(ExportResponse {
        download_url: format!("/api/messages/download/{token}"),
        expires_at,
        format: req.format,
        row_count,
    })))
}

/// `GET /api/messages/download/{token}` — one-shot, public
/// (no AuthenticatedUser extractor).
///
/// Public on purpose: the token *is* the credential, and the SPA wants
/// to open the URL in a fresh tab where the cookie may or may not flow.
/// `take_export_oneshot` GETDELs atomically so any second access lands
/// on the 410 path.
pub async fn export_download_handler(
    path: web::Path<String>,
    state: web::Data<AppState>,
    req: HttpRequest,
) -> AppResult<HttpResponse> {
    let _ = req; // future-proof for richer per-request logging.
    let token = path.into_inner();

    let payload = take_export_oneshot(&state.redis, &token)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("take export: {e}")))?;

    let bytes = match payload {
        Some(b) => b,
        // Either expired, never existed, or already consumed — the
        // client can't tell which (intentional).
        None => return Err(AppError::Gone("download link is no longer valid".into())),
    };

    // Split content-type prefix from body (single newline separator;
    // see store_export_oneshot).
    let split_at = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("malformed export payload")))?;
    let content_type = std::str::from_utf8(&bytes[..split_at])
        .map_err(|e| AppError::Internal(anyhow::anyhow!("export content-type utf-8: {e}")))?
        .to_owned();
    let body = bytes[split_at + 1..].to_vec();

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            "Content-Disposition",
            r#"attachment; filename="chat-export""#,
        ))
        .body(body))
}

// ────────────────────────────────────────────────────────────────────────
// Route configuration
// ────────────────────────────────────────────────────────────────────────

/// Mount under `web::scope("/api/messages")`. Order matters: the literal
/// paths (`/search`, `/export`, `/download/{token}`) must be registered
/// before `/{id}` so a `GET /search` is not parsed as a UUID.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/search", web::get().to(search_handler))
        .route("/export", web::post().to(export_handler))
        .route("/download/{token}", web::get().to(export_download_handler))
        .route("/{friend_id}", web::get().to(history_handler))
        .route("/{message_id}", web::delete().to(delete_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cursor encoding ──

    #[test]
    fn cursor_round_trips() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 123_456_000).unwrap();
        let id = Uuid::new_v4();
        let encoded = encode_cursor(now, id);
        let (back_t, back_id) = decode_cursor(&encoded).expect("decodes");
        assert_eq!(back_id, id);
        // Micros precision: equal at the microsecond, not necessarily at
        // the nanosecond.
        assert_eq!(back_t.timestamp_micros(), now.timestamp_micros());
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert!(decode_cursor("").is_none());
        assert!(decode_cursor("not-a-cursor").is_none());
        assert!(decode_cursor("123_not-a-uuid").is_none());
        assert!(decode_cursor("notanumber_00000000-0000-0000-0000-000000000000").is_none());
    }

    // ── Soft-delete view shape ──

    #[test]
    fn into_view_blanks_deleted_content() {
        let row = MessageRow {
            id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            to_user_id: Uuid::nil(),
            content: "secret".into(),
            is_deleted: true,
            created_at: Utc::now().naive_utc(),
        };
        let view = row.into_view(vec![]);
        assert!(view.is_deleted);
        assert_eq!(view.content, DELETED_PLACEHOLDER);
    }

    #[test]
    fn into_view_preserves_live_content() {
        let row = MessageRow {
            id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            to_user_id: Uuid::nil(),
            content: "hello".into(),
            is_deleted: false,
            created_at: Utc::now().naive_utc(),
        };
        let view = row.into_view(vec![Uuid::nil()]);
        assert!(!view.is_deleted);
        assert_eq!(view.content, "hello");
        assert_eq!(view.emoji_ids.len(), 1);
    }

    // ── CSV escaping ──

    #[test]
    fn csv_escape_passes_plain_text() {
        assert_eq!(csv_escape("hello world"), "hello world");
    }

    #[test]
    fn csv_escape_quotes_commas() {
        assert_eq!(csv_escape("a,b"), r#""a,b""#);
    }

    #[test]
    fn csv_escape_doubles_internal_quotes() {
        assert_eq!(csv_escape(r#"he said "hi""#), r#""he said ""hi""""#);
    }

    #[test]
    fn csv_escape_quotes_newlines() {
        assert_eq!(csv_escape("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(csv_escape("cr\rlf"), "\"cr\rlf\"");
    }

    // ── Render export ──

    fn fake_view(content: &str, deleted: bool) -> MessageView {
        MessageView {
            id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            to_user_id: Uuid::nil(),
            content: content.to_owned(),
            is_deleted: deleted,
            emoji_ids: vec![],
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn render_csv_header_and_one_row() {
        let rows = vec![fake_view("hi", false)];
        let (bytes, ct) = render_export(&rows, ExportFormat::Csv);
        assert_eq!(ct, "text/csv; charset=utf-8");
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("id,from_user_id,to_user_id,created_at,is_deleted,content\n"));
        assert!(s.contains(",hi\n"));
    }

    #[test]
    fn render_json_wraps_in_messages_object() {
        let rows = vec![fake_view("hi", false)];
        let (bytes, ct) = render_export(&rows, ExportFormat::Json);
        assert_eq!(ct, "application/json");
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["messages"].is_array());
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    }

    /// Pin: even rendered exports of soft-deleted rows must not leak the
    /// original content. The view layer blanks `content`; the renderer
    /// just passes it through.
    #[test]
    fn render_export_preserves_blanked_deleted_content() {
        let rows = vec![fake_view("", true)]; // already blanked by into_view
        let (bytes, _) = render_export(&rows, ExportFormat::Json);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["messages"][0]["content"], "");
        assert_eq!(v["messages"][0]["is_deleted"], true);
    }

    // ── Constants ──

    #[test]
    fn export_ttl_matches_spec() {
        // TODO [34] calls out 30 minutes.
        assert_eq!(EXPORT_TTL_SECONDS, 30 * 60);
    }

    #[test]
    fn history_default_limit_matches_spec() {
        // TODO [31] calls out limit=50.
        assert_eq!(DEFAULT_HISTORY_LIMIT, 50);
    }

    #[test]
    fn redis_export_key_is_namespaced() {
        let token = "abc";
        assert_eq!(redis_export_key(token), format!("{EXPORT_KEY_PREFIX}{token}"));
        assert!(redis_export_key(token).starts_with(EXPORT_KEY_PREFIX));
    }

    // ── Format enum ──

    #[test]
    fn export_format_deserializes_lowercase() {
        let j: ExportFormat = serde_json::from_str(r#""json""#).unwrap();
        assert_eq!(j, ExportFormat::Json);
        let c: ExportFormat = serde_json::from_str(r#""csv""#).unwrap();
        assert_eq!(c, ExportFormat::Csv);
    }

    #[test]
    fn export_format_rejects_unknown() {
        assert!(serde_json::from_str::<ExportFormat>(r#""xml""#).is_err());
        assert!(serde_json::from_str::<ExportFormat>(r#""JSON""#).is_err());
    }

    // ── SearchHit shape ──

    /// Pin SearchHit serialization: the `MessageView` fields are flattened
    /// into the top-level object alongside `headline`. The SPA reads
    /// `headline` next to `id` / `content`, so any accidental wrapping
    /// would silently break highlight rendering.
    #[test]
    fn search_hit_flattens_message_fields() {
        let hit = SearchHit {
            message: fake_view("hi", false),
            headline: "<mark>hi</mark>".into(),
        };
        let v = serde_json::to_value(&hit).unwrap();
        let obj = v.as_object().unwrap();
        for required in [
            "id",
            "from_user_id",
            "to_user_id",
            "content",
            "is_deleted",
            "emoji_ids",
            "created_at",
            "headline",
        ] {
            assert!(obj.contains_key(required), "missing {required}: {v}");
        }
    }
}
