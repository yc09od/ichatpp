//! Friend-request and friendship endpoints (TODO [24] / [25] /
//! ARCHITECTURE.md §4.3):
//!
//! ```text
//! POST   /api/friends/requests            # send a request
//! GET    /api/friends/requests/pending    # list inbound pending requests
//! PUT    /api/friends/requests/:id        # accept or reject
//! GET    /api/friends                     # list current friendships
//! DELETE /api/friends/:friend_id          # remove a friendship
//! ```
//!
//! ## Direction-symmetric pending check
//!
//! The schema's `uq_friend_requests_pending` index covers
//! `(from_user_id, to_user_id) WHERE status='pending'` only — i.e. it
//! stops the *same* sender from re-issuing to the same recipient, but
//! does not stop B from sending to A while A's pending request to B
//! already exists. The application-level [`pending_request_exists`]
//! check below handles that case before we hit the INSERT, so the
//! 409 response is consistent regardless of direction.
//!
//! ## Pending row vs. friends row
//!
//! On accept, the `friend_requests` row's status flips to 'accepted'
//! AND a row is inserted into `friends` with canonical
//! `user_id_1 < user_id_2` ordering. Both happen in one transaction
//! (with `SELECT … FOR UPDATE` on the request row) so a concurrent
//! double-accept does not leave the request marked accepted with no
//! friendship row.
//!
//! ## Two ways to address a target
//!
//! `POST /requests` accepts `user_id` (UUID) **or** `account_code`
//! (10-digit string), but not both. The pure-function
//! [`parse_target_selector`] does the shape check; [`resolve_target`]
//! turns the selector into a `Uuid` with one DB lookup.

#![allow(dead_code)] // first non-test consumer mounts in TODO [24];
                     // [25] adds list/delete on the same scope.

use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::authenticated::AuthenticatedUser;
use crate::errors::{AppError, AppResult};
use crate::responses::ApiResponse;
use crate::AppState;

/// Account-code shape: 10 ASCII digits, matches the schema's
/// `^[0-9]{10}$` CHECK. Pre-validated at the handler so an obviously
/// malformed code never hits the DB.
const ACCOUNT_CODE_LEN: usize = 10;

/// SQLSTATE for a unique-constraint violation in PostgreSQL. Used to
/// catch the rare TOCTOU race between [`pending_request_exists`] and
/// the actual INSERT.
const PG_UNIQUE_VIOLATION: &str = "23505";

const STATUS_PENDING: &str = "pending";
const STATUS_ACCEPTED: &str = "accepted";
const STATUS_REJECTED: &str = "rejected";

// ────────────────────────────────────────────────────────────────────────
// Request / response DTOs
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateFriendRequestBody {
    pub user_id: Option<Uuid>,
    pub account_code: Option<String>,
}

/// Internal selector — exactly one of UUID or account-code, picked from
/// the request body by [`parse_target_selector`]. Pure-function
/// validation lives on this enum so the handler's DB hit is gated on a
/// well-formed selector.
#[derive(Debug, PartialEq, Eq)]
enum TargetSelector {
    UserId(Uuid),
    AccountCode(String),
}

/// Action for `PUT /requests/:id`. Serde rejects anything other than
/// `"accept"` / `"reject"` so the handler never sees a bad enum value.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FriendRequestAction {
    Accept,
    Reject,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFriendRequestBody {
    pub action: FriendRequestAction,
}

/// Compact summary returned by `POST /requests` and `PUT /requests/:id`.
#[derive(Debug, Serialize)]
pub struct FriendRequestSummary {
    pub id: Uuid,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Public snippet of the *sender* attached to each pending request the
/// SPA renders. Mirrors the public-user shape from `users.rs` minus
/// `signature` and `is_visible` — just enough to render an avatar +
/// display name in a notification list.
#[derive(Debug, Serialize)]
pub struct FriendRequestPeer {
    pub id: Uuid,
    pub account_code: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PendingFriendRequest {
    pub id: Uuid,
    pub from_user: FriendRequestPeer,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PendingListResponse {
    pub requests: Vec<PendingFriendRequest>,
}

// ────────────────────────────────────────────────────────────────────────
// SQL row types
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct FriendRequestRow {
    id: Uuid,
    from_user_id: Uuid,
    to_user_id: Uuid,
    status: String,
    created_at: NaiveDateTime,
}

impl From<FriendRequestRow> for FriendRequestSummary {
    fn from(r: FriendRequestRow) -> Self {
        Self {
            id: r.id,
            from_user_id: r.from_user_id,
            to_user_id: r.to_user_id,
            status: r.status,
            // schema column is TIMESTAMP (no TZ); we store UTC values so
            // and_utc() is faithful.
            created_at: r.created_at.and_utc(),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PendingRow {
    request_id: Uuid,
    request_created_at: NaiveDateTime,
    from_id: Uuid,
    from_account_code: String,
    from_nickname: Option<String>,
    from_avatar_url: Option<String>,
}

// ────────────────────────────────────────────────────────────────────────
// Pure-function validation
// ────────────────────────────────────────────────────────────────────────

fn parse_target_selector(body: &CreateFriendRequestBody) -> Result<TargetSelector, AppError> {
    match (&body.user_id, &body.account_code) {
        (Some(id), None) => Ok(TargetSelector::UserId(*id)),
        (None, Some(code)) => {
            // Cheap shape gate so the DB lookup never sees a malformed
            // code. Same regex as the schema CHECK.
            if code.len() != ACCOUNT_CODE_LEN || !code.chars().all(|c| c.is_ascii_digit()) {
                return Err(AppError::BadRequest(
                    "account_code must be exactly 10 digits".into(),
                ));
            }
            Ok(TargetSelector::AccountCode(code.clone()))
        }
        (Some(_), Some(_)) => Err(AppError::BadRequest(
            "provide exactly one of user_id or account_code".into(),
        )),
        (None, None) => Err(AppError::BadRequest(
            "provide one of user_id or account_code".into(),
        )),
    }
}

// ────────────────────────────────────────────────────────────────────────
// DAO helpers
// ────────────────────────────────────────────────────────────────────────

/// Resolve the selector to a concrete `users.id`. Both branches do
/// exactly one DB call; both 404 when the target doesn't exist.
async fn resolve_target(pool: &PgPool, selector: TargetSelector) -> AppResult<Uuid> {
    match selector {
        TargetSelector::UserId(id) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("check target id: {e}")))?;
            if !exists {
                return Err(AppError::NotFound("target user not found".into()));
            }
            Ok(id)
        }
        TargetSelector::AccountCode(code) => {
            let id_opt: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM users WHERE account_code = $1")
                    .bind(&code)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("lookup account_code: {e}"))
                    })?;
            id_opt.ok_or_else(|| AppError::NotFound("target user not found".into()))
        }
    }
}

/// True iff `a` and `b` share a row in `friends`. `a == b` is reported
/// as *not* friends so the self-check at the top of the create handler
/// remains the canonical "can't friend yourself" gate; the pair-ordering
/// uses Postgres's uuid LEAST/GREATEST which agrees with the byte-order
/// `user_id_1 < user_id_2` CHECK constraint.
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

/// True iff a `pending` friend_requests row exists in *either* direction
/// between `a` and `b`. See module docs for why we have to check both
/// directions in the application layer.
async fn pending_request_exists(
    pool: &PgPool,
    a: Uuid,
    b: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM friend_requests
            WHERE status = 'pending'
              AND ((from_user_id = $1 AND to_user_id = $2)
                OR (from_user_id = $2 AND to_user_id = $1))
         )",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
}

// ────────────────────────────────────────────────────────────────────────
// Handlers
// ────────────────────────────────────────────────────────────────────────

/// POST /api/friends/requests
pub async fn create_request_handler(
    user: AuthenticatedUser,
    body: web::Json<CreateFriendRequestBody>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let selector = parse_target_selector(&body.into_inner())?;
    let target_id = resolve_target(&state.db, selector).await?;

    if target_id == user.user_id {
        return Err(AppError::BadRequest(
            "cannot send a friend request to yourself".into(),
        ));
    }

    if are_friends(&state.db, user.user_id, target_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("check friendship: {e}")))?
    {
        return Err(AppError::Conflict("already friends".into()));
    }

    if pending_request_exists(&state.db, user.user_id, target_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("check pending: {e}")))?
    {
        return Err(AppError::Conflict(
            "a pending friend request already exists between these users".into(),
        ));
    }

    let row: FriendRequestRow = sqlx::query_as(
        r#"
        INSERT INTO friend_requests (from_user_id, to_user_id, status)
        VALUES ($1, $2, 'pending')
        RETURNING id, from_user_id, to_user_id, status, created_at
        "#,
    )
    .bind(user.user_id)
    .bind(target_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        // TOCTOU between our pending check and INSERT — the unique index
        // catches same-direction duplicates. Map the SQLSTATE to a 409
        // so the caller sees the same response regardless of which path
        // (app-check or DB-check) flagged the duplicate.
        if let sqlx::Error::Database(db_err) = &e {
            if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION) {
                return AppError::Conflict(
                    "a pending friend request already exists between these users".into(),
                );
            }
        }
        AppError::Internal(anyhow::anyhow!("insert friend request: {e}"))
    })?;

    Ok(HttpResponse::Created().json(ApiResponse::build(FriendRequestSummary::from(row))))
}

/// GET /api/friends/requests/pending
///
/// Returns *inbound* pending requests only — the SPA renders these as
/// actionable notifications. Outbound requests can be inferred client-
/// side by the act of sending, and re-sending raises a 409 anyway, so
/// a separate "my outbound" view isn't worth the second endpoint at MVP.
pub async fn list_pending_handler(
    user: AuthenticatedUser,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let rows: Vec<PendingRow> = sqlx::query_as(
        r#"
        SELECT
            fr.id           AS request_id,
            fr.created_at   AS request_created_at,
            u.id            AS from_id,
            u.account_code  AS from_account_code,
            u.nickname      AS from_nickname,
            u.avatar_url    AS from_avatar_url
        FROM friend_requests fr
        JOIN users u ON u.id = fr.from_user_id
        WHERE fr.to_user_id = $1 AND fr.status = 'pending'
        ORDER BY fr.created_at DESC
        "#,
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("list pending: {e}")))?;

    let requests = rows
        .into_iter()
        .map(|r| PendingFriendRequest {
            id: r.request_id,
            created_at: r.request_created_at.and_utc(),
            from_user: FriendRequestPeer {
                id: r.from_id,
                account_code: r.from_account_code,
                nickname: r.from_nickname,
                avatar_url: r.from_avatar_url,
            },
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiResponse::build(PendingListResponse { requests })))
}

/// PUT /api/friends/requests/{id}
///
/// Only the *recipient* (`to_user_id == me`) can accept or reject.
/// Anything else (request not found, not pending, addressed to someone
/// else) collapses to 404 to avoid leaking which path failed.
pub async fn update_request_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    body: web::Json<UpdateFriendRequestBody>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let request_id = path.into_inner();
    let req = body.into_inner();

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("begin tx: {e}")))?;

    // SELECT … FOR UPDATE locks the row so a concurrent accept/reject
    // can't double-process the same request.
    let row_opt: Option<FriendRequestRow> = sqlx::query_as(
        r#"
        SELECT id, from_user_id, to_user_id, status, created_at
        FROM friend_requests
        WHERE id = $1 AND status = 'pending' AND to_user_id = $2
        FOR UPDATE
        "#,
    )
    .bind(request_id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("lookup friend request: {e}")))?;

    let row = row_opt.ok_or_else(|| AppError::NotFound("friend request not found".into()))?;

    let new_status = match req.action {
        FriendRequestAction::Accept => STATUS_ACCEPTED,
        FriendRequestAction::Reject => STATUS_REJECTED,
    };

    sqlx::query("UPDATE friend_requests SET status = $1, updated_at = NOW() WHERE id = $2")
        .bind(new_status)
        .bind(request_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("update status: {e}")))?;

    if matches!(req.action, FriendRequestAction::Accept) {
        // Canonical pair ordering via Postgres LEAST/GREATEST so the
        // CHECK constraint `user_id_1 < user_id_2` is always satisfied
        // regardless of which side originated the request.
        // ON CONFLICT DO NOTHING catches the rare case where a friends
        // row already exists (e.g. an earlier rejected-then-resent flow
        // that already created the pair).
        sqlx::query(
            r#"
            INSERT INTO friends (user_id_1, user_id_2)
            VALUES (LEAST($1::uuid, $2::uuid), GREATEST($1::uuid, $2::uuid))
            ON CONFLICT (user_id_1, user_id_2) DO NOTHING
            "#,
        )
        .bind(row.from_user_id)
        .bind(row.to_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("insert friends row: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("commit tx: {e}")))?;

    let updated = FriendRequestRow {
        status: new_status.to_owned(),
        ..row
    };

    Ok(HttpResponse::Ok().json(ApiResponse::build(FriendRequestSummary::from(updated))))
}

// ────────────────────────────────────────────────────────────────────────
// Friendship list & removal (TODO [25])
// ────────────────────────────────────────────────────────────────────────

/// Public summary of a friend in the user's list. Carries the same
/// public fields as [`FriendRequestPeer`] — id / account_code / nickname
/// / avatar_url — plus `is_visible` (so the SPA can flag friends who've
/// hidden their profile from strangers) and `last_message_at` (drives
/// the "recently active" sort order on the client too).
#[derive(Debug, Serialize)]
pub struct FriendSummary {
    pub id: Uuid,
    pub account_code: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub is_visible: bool,
    /// `None` when no non-deleted message has ever been exchanged.
    pub last_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct FriendListResponse {
    pub friends: Vec<FriendSummary>,
}

#[derive(Debug, sqlx::FromRow)]
struct FriendRow {
    id: Uuid,
    account_code: String,
    nickname: Option<String>,
    avatar_url: Option<String>,
    is_visible: bool,
    last_message_at: Option<NaiveDateTime>,
}

impl From<FriendRow> for FriendSummary {
    fn from(r: FriendRow) -> Self {
        Self {
            id: r.id,
            account_code: r.account_code,
            nickname: r.nickname,
            avatar_url: r.avatar_url,
            is_visible: r.is_visible,
            last_message_at: r.last_message_at.map(|t| t.and_utc()),
        }
    }
}

/// GET /api/friends
///
/// Sorted by "most recent message" first — that's what the TODO calls
/// out as the actionable ordering for chat UIs — with nickname as the
/// tiebreaker for friends with no message history. The conversation
/// indexes (idx_messages_conversation / _reverse) make the MAX subquery
/// per friend cheap on small-to-medium friend lists; if this becomes a
/// hotspot we can promote `last_message_at` into a materialised column
/// later.
pub async fn list_friends_handler(
    user: AuthenticatedUser,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let rows: Vec<FriendRow> = sqlx::query_as(
        r#"
        SELECT
            u.id,
            u.account_code,
            u.nickname,
            u.avatar_url,
            COALESCE(u.is_visible, TRUE) AS is_visible,
            (
                SELECT MAX(m.created_at)
                FROM messages m
                WHERE NOT m.is_deleted
                  AND (
                       (m.from_user_id = $1 AND m.to_user_id = u.id)
                    OR (m.from_user_id = u.id AND m.to_user_id = $1)
                  )
            ) AS last_message_at
        FROM friends f
        JOIN users u
          ON u.id = CASE
                WHEN f.user_id_1 = $1 THEN f.user_id_2
                ELSE f.user_id_1
             END
        WHERE f.user_id_1 = $1 OR f.user_id_2 = $1
        -- Most-recent-message first; chatless friends sink to alphabetical
        -- by nickname (then account_code as a stable tiebreaker for
        -- friends with NULL nickname).
        ORDER BY
            last_message_at DESC NULLS LAST,
            u.nickname      ASC NULLS LAST,
            u.account_code  ASC
        "#,
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("list friends: {e}")))?;

    let friends = rows.into_iter().map(FriendSummary::from).collect();
    Ok(HttpResponse::Ok().json(ApiResponse::build(FriendListResponse { friends })))
}

/// DELETE /api/friends/{friend_id}
///
/// Removes the canonical friends row only — message history is **not**
/// touched. Per the TODO acceptance criterion, deleted friendships
/// should leave conversations queryable so the user can still see the
/// transcript even after un-friending.
///
/// Idempotent in spirit but strict in response: deleting a non-existent
/// friendship returns 404 so a buggy client double-clicking the
/// "remove" button doesn't get a misleading 200.
pub async fn remove_friend_handler(
    user: AuthenticatedUser,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let friend_id = path.into_inner();

    if friend_id == user.user_id {
        // Can't be friends with yourself, so can't un-friend yourself
        // either. 400 (not 404) since the path itself is structurally
        // invalid.
        return Err(AppError::BadRequest(
            "cannot remove yourself as a friend".into(),
        ));
    }

    // Canonical pair ordering (Postgres LEAST/GREATEST agrees with the
    // schema's `user_id_1 < user_id_2` CHECK), so the DELETE hits the
    // unique index regardless of which side called the endpoint.
    let result = sqlx::query(
        r#"
        DELETE FROM friends
        WHERE user_id_1 = LEAST($1::uuid, $2::uuid)
          AND user_id_2 = GREATEST($1::uuid, $2::uuid)
        "#,
    )
    .bind(user.user_id)
    .bind(friend_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("delete friend: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("not friends".into()));
    }

    Ok(HttpResponse::NoContent().finish())
}

// ────────────────────────────────────────────────────────────────────────
// Route configuration
// ────────────────────────────────────────────────────────────────────────

/// Mount under `web::scope("/api/friends")`.
///
/// Order matters: `/requests/pending` (literal) is registered **before**
/// `/requests/{id}` so a `GET /requests/pending` doesn't fall into the
/// UUID path and 400 with "pending is not a valid UUID". Likewise the
/// bare-scope `GET ""` (friend list) is registered before the `/{id}`
/// pattern that handles DELETE.
pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_friends_handler))
        .route("/requests", web::post().to(create_request_handler))
        .route("/requests/pending", web::get().to(list_pending_handler))
        .route("/requests/{id}", web::put().to(update_request_handler))
        .route("/{friend_id}", web::delete().to(remove_friend_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_target_selector ──

    #[test]
    fn target_selector_accepts_uuid_only() {
        let body = CreateFriendRequestBody {
            user_id: Some(Uuid::new_v4()),
            account_code: None,
        };
        match parse_target_selector(&body).unwrap() {
            TargetSelector::UserId(_) => (),
            other => panic!("expected UserId, got {other:?}"),
        }
    }

    #[test]
    fn target_selector_accepts_account_code_only() {
        let body = CreateFriendRequestBody {
            user_id: None,
            account_code: Some("0123456789".into()),
        };
        match parse_target_selector(&body).unwrap() {
            TargetSelector::AccountCode(c) => assert_eq!(c, "0123456789"),
            other => panic!("expected AccountCode, got {other:?}"),
        }
    }

    #[test]
    fn target_selector_rejects_both_provided() {
        let body = CreateFriendRequestBody {
            user_id: Some(Uuid::new_v4()),
            account_code: Some("0123456789".into()),
        };
        assert!(matches!(
            parse_target_selector(&body),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn target_selector_rejects_neither_provided() {
        let body = CreateFriendRequestBody {
            user_id: None,
            account_code: None,
        };
        assert!(matches!(
            parse_target_selector(&body),
            Err(AppError::BadRequest(_))
        ));
    }

    /// The acceptance criterion mentions `account_code` as a search
    /// path; pin the format gate so a future loosening (allow letters,
    /// shorter codes) shows up as a test failure.
    #[test]
    fn target_selector_rejects_malformed_account_code() {
        for bad in [
            "",
            "123",
            "12345678901",
            "abcdefghij",
            "1234567890 ",
            "12345-7890",
        ] {
            let body = CreateFriendRequestBody {
                user_id: None,
                account_code: Some(bad.into()),
            };
            assert!(
                matches!(parse_target_selector(&body), Err(AppError::BadRequest(_))),
                "should reject account_code: {bad:?}"
            );
        }
    }

    // ── FriendRequestAction (de)serialization ──

    #[test]
    fn action_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(&FriendRequestAction::Accept).unwrap(),
            "accept"
        );
        assert_eq!(
            serde_json::to_value(&FriendRequestAction::Reject).unwrap(),
            "reject"
        );
    }

    #[test]
    fn action_deserialization_is_strict() {
        let r: FriendRequestAction = serde_json::from_str(r#""accept""#).unwrap();
        assert_eq!(r, FriendRequestAction::Accept);

        for bad in [r#""ACCEPT""#, r#""approve""#, r#""yes""#, r#""""#] {
            assert!(
                serde_json::from_str::<FriendRequestAction>(bad).is_err(),
                "should reject action JSON: {bad}"
            );
        }
    }

    // ── Body deserialization ──

    #[test]
    fn create_body_accepts_either_field() {
        // user_id only
        serde_json::from_str::<CreateFriendRequestBody>(
            r#"{"user_id":"00000000-0000-0000-0000-000000000000"}"#,
        )
        .expect("user_id only");
        // account_code only
        serde_json::from_str::<CreateFriendRequestBody>(r#"{"account_code":"0123456789"}"#)
            .expect("account_code only");
    }

    #[test]
    fn update_body_round_trips() {
        let body: UpdateFriendRequestBody =
            serde_json::from_str(r#"{"action":"reject"}"#).expect("parse");
        assert_eq!(body.action, FriendRequestAction::Reject);
    }

    // ── Response shape pinning ──

    /// The pending response intentionally surfaces only public fields
    /// of the *sender*. Pin against accidentally leaking `email` or
    /// `role` here — that's the kind of regression that's invisible
    /// in casual review.
    #[test]
    fn pending_request_omits_sender_private_fields() {
        let p = PendingFriendRequest {
            id: Uuid::nil(),
            created_at: Utc::now(),
            from_user: FriendRequestPeer {
                id: Uuid::nil(),
                account_code: "0123456789".into(),
                nickname: None,
                avatar_url: None,
            },
        };
        let json = serde_json::to_value(&p).unwrap();
        let from_user = json["from_user"].as_object().unwrap();
        for forbidden in ["email", "role", "password_hash", "signature", "is_visible"] {
            assert!(
                !from_user.contains_key(forbidden),
                "from_user must not include {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn summary_round_trips() {
        let s = FriendRequestSummary {
            id: Uuid::nil(),
            from_user_id: Uuid::nil(),
            to_user_id: Uuid::nil(),
            status: "pending".into(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        for required in ["id", "from_user_id", "to_user_id", "status", "created_at"] {
            assert!(obj.contains_key(required), "missing {required}: {json}");
        }
    }

    // ── Status / SQL constants ──

    /// Pin the status string vocabulary against the schema's comment
    /// `pending, accepted, rejected`. A typo here would silently leave
    /// requests in an "invalid" status that no query reads.
    #[test]
    fn status_constants_match_schema_vocabulary() {
        assert_eq!(STATUS_PENDING, "pending");
        assert_eq!(STATUS_ACCEPTED, "accepted");
        assert_eq!(STATUS_REJECTED, "rejected");
    }

    #[test]
    fn pg_unique_violation_is_pinned() {
        // Same SQLSTATE the register handler uses; if the project ever
        // adds a shared error-code table, this assertion is what catches
        // accidental drift.
        assert_eq!(PG_UNIQUE_VIOLATION, "23505");
    }

    // ── Friend list response (TODO [25]) ──

    /// `FriendSummary` mirrors the public-peer shape — pin against
    /// adding email/role/signature to the friend list, which would leak
    /// private fields to anyone who's a friend (a much wider audience
    /// than admin or self).
    #[test]
    fn friend_summary_omits_private_fields() {
        let s = FriendSummary {
            id: Uuid::nil(),
            account_code: "0123456789".into(),
            nickname: Some("Alice".into()),
            avatar_url: None,
            is_visible: true,
            last_message_at: Some(Utc::now()),
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();

        for forbidden in ["email", "role", "password_hash", "signature", "created_at"] {
            assert!(
                !obj.contains_key(forbidden),
                "FriendSummary must not include {forbidden}: {json}"
            );
        }
        // `last_message_at` *is* required — pin so it can't accidentally
        // get serde-skipped and break the SPA's ordering UI.
        assert!(obj.contains_key("last_message_at"));
    }

    /// `last_message_at` must serialize as `null` (not be omitted) when
    /// no message exists — the SPA distinguishes "no messages yet" from
    /// "field is missing" for the sort indicator.
    #[test]
    fn friend_summary_serializes_null_last_message_at() {
        let s = FriendSummary {
            id: Uuid::nil(),
            account_code: "0123456789".into(),
            nickname: None,
            avatar_url: None,
            is_visible: true,
            last_message_at: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(
            json["last_message_at"].is_null(),
            "expected null, got {json}"
        );
    }

    #[test]
    fn friend_list_response_round_trips() {
        let list = FriendListResponse {
            friends: vec![FriendSummary {
                id: Uuid::nil(),
                account_code: "0123456789".into(),
                nickname: Some("a".into()),
                avatar_url: None,
                is_visible: false,
                last_message_at: None,
            }],
        };
        let json = serde_json::to_value(&list).unwrap();
        assert!(json["friends"].is_array());
        assert_eq!(json["friends"].as_array().unwrap().len(), 1);
    }
}
