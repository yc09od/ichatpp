//! Auth handlers — registration (TODO [17]), login (TODO [18]),
//! refresh and logout (TODO [19]).
//!
//! ## Registration flow (TODO [17] / ARCHITECTURE.md §4.1)
//!
//! 1. Validate email shape and password strength up-front (no DB hit yet).
//! 2. SHA-256 the invitation code and look up `invitations` by hash; reject
//!    if not unused or expired. The acceptance criterion calls out one
//!    specific case: a code with status='used' must return **409 Conflict**.
//! 3. bcrypt the password (slow; do it before opening the transaction so
//!    we don't hold a DB connection during the hash).
//! 4. Inside one transaction:
//!    - INSERT users (with retried `account_code` allocation on collision),
//!    - INSERT user_roles(role='user'),
//!    - UPDATE invitations SET status='used', used_by, used_at — guarded by
//!      `WHERE status='unused'` so a concurrent registration loses cleanly
//!      with 409 instead of two users sharing one code.
//! 5. Sign access + refresh JWTs, generate a CSRF token, and emit all three
//!    via `Set-Cookie`. The response body carries **no token material** —
//!    that's the whole reason the cookies are httpOnly.
//!
//! ## Login flow (TODO [18])
//!
//! Three security properties the handler is built around:
//!
//! - **No user-existence oracle.** Wrong-password and unknown-email cases
//!   return the *exact same* 401 body, and bcrypt runs against a constant
//!   dummy hash when the email isn't found so the response time is
//!   indistinguishable from "user exists, password wrong".
//! - **Per-email lockout.** Five failures within 15 minutes locks the
//!   email; the counter is a Redis key with a sliding TTL, so each
//!   subsequent failure pushes the unlock time out. Successful login
//!   clears the counter.
//! - **Per-IP rate limit.** 10 req/min/IP enforced by the auth-scoped
//!   governor in `main.rs`. The lockout and the rate limiter are
//!   orthogonal: an attacker behind one IP burns the rate limit before
//!   they can fill anyone's lockout slot, and even with infinite IPs they
//!   bottleneck on the per-email lockout.
//!
//! ## Refresh & logout flow (TODO [19])
//!
//! - **Refresh** rotates the *access* and *csrf* cookies; the refresh
//!   cookie itself is left untouched (unchanged path-scoped lifetime).
//!   Validation is two-step — JWT signature/expiry/type, then a Redis
//!   whitelist check. Either failure produces an opaque 401.
//!
//! - **Logout** authenticates against the *access* cookie (the refresh
//!   cookie is path-scoped to `/api/auth/refresh` and is not sent to
//!   `/api/auth/logout`), wipes the user's whole refresh-token set in
//!   Redis ("logout everywhere"), and emits Set-Cookie expirations for
//!   all three auth cookies. The acceptance criterion ("登出后 refresh
//!   端点返回 401") is met by the Redis DEL, not by anything client-side
//!   — even an attacker who'd captured the refresh JWT before logout
//!   cannot use it once the whitelist row is gone.

use std::sync::OnceLock;

use actix_web::{web, HttpRequest, HttpResponse};
use bcrypt::{hash as bcrypt_hash, verify as bcrypt_verify, DEFAULT_COST};
use chrono::Utc;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use validator::ValidateEmail;

use crate::auth::cookies::{
    clear_auth_cookies, set_auth_cookies, ACCESS_COOKIE_NAME, REFRESH_COOKIE_NAME,
};
use crate::auth::jwt::TokenType;
use crate::auth::refresh_store::{
    add_refresh_token, is_refresh_token_active, revoke_all_refresh_tokens,
};
use crate::auth::AuthCookies;
use crate::errors::{AppError, AppResult};
use crate::handlers::invitations::{find_invitation_by_hash, hash_code};
use crate::middleware::generate_csrf_token;
use crate::responses::ApiResponse;
use crate::AppState;

/// bcrypt cost factor. ARCHITECTURE.md §4.1 requires ≥ 12; bcrypt's
/// default of 12 matches, and pinning it here documents the choice. Higher
/// is fine to bump later, but doubling cost doubles login latency.
const BCRYPT_COST: u32 = DEFAULT_COST;

/// Inclusive lower / upper bounds. Eight-char minimum mirrors the TODO;
/// 256 is a sanity cap so an attacker can't push us into pathologically
/// slow bcrypt input.
const MIN_PASSWORD_LEN: usize = 8;
const MAX_PASSWORD_LEN: usize = 256;

/// Matches the schema's `email VARCHAR(255)`.
const MAX_EMAIL_LEN: usize = 255;

/// Matches the schema's `nickname VARCHAR(100)`. Counted in chars, not
/// bytes, because Postgres VARCHAR(n) is character-length.
const MAX_NICKNAME_LEN: usize = 100;

/// Length of the user-facing account number (matches CHECK constraint
/// `^[0-9]{10}$` on `users.account_code`).
const ACCOUNT_CODE_LEN: usize = 10;

/// Bound on the inner retry loop. With 10^10 possible codes and far fewer
/// users, a collision is extraordinarily rare — hitting this cap means
/// something is wrong (e.g. RNG broken), so we fail loud rather than spin.
const ACCOUNT_CODE_MAX_RETRIES: usize = 10;

/// Default role assigned at registration (`user_roles.role`).
const DEFAULT_ROLE: &str = "user";

/// SQLSTATE for a unique-constraint violation in PostgreSQL.
const PG_UNIQUE_VIOLATION: &str = "23505";

/// Constant used in account_code RNG sampling: largest byte value that's
/// a clean multiple of 10 (250 = 10 × 25). Bytes in `[0, 250)` give a
/// uniform mod-10 distribution; `[250, 256)` are resampled.
const ACCOUNT_CODE_REJECTION_THRESHOLD: u8 = 250;

/// Login lockout: 5 failures triggers the lock per the TODO. The 5th
/// failure is the "you are now locked" boundary — i.e. a user with 4
/// failures can still authenticate with the right password and clear
/// their counter; once they hit 5, every subsequent attempt is denied
/// until the TTL expires.
const MAX_LOGIN_FAILURES: u32 = 5;

/// Lockout window. Sliding — every failure refreshes the TTL, so a user
/// who gets locked has to stop trying for the full 15 minutes (not just
/// 15 minutes from their first failure).
const LOGIN_LOCKOUT_TTL_SECONDS: u64 = 15 * 60;

/// Redis key prefix for the per-email failure counter. Single source of
/// truth so the read/write/clear paths cannot drift.
const LOGIN_LOCKOUT_KEY_PREFIX: &str = "login_lockout:";

/// The one and only message the login endpoint emits on auth failure.
/// Pinned as a constant so the "wrong password" and "no such email" paths
/// physically cannot diverge — that's the acceptance criterion verbatim
/// ("错误密码不暴露 email 是否存在").
const LOGIN_GENERIC_ERROR: &str = "邮箱或密码错误";

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub invitation_code: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

/// Response body for successful registration. **No token material** —
/// access / refresh / csrf tokens travel exclusively via `Set-Cookie`,
/// per ARCHITECTURE.md §4.1.
#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub user_id: Uuid,
    pub account_code: String,
}

/// Generate a 10-digit numeric account_code matching the CHECK constraint
/// `^[0-9]{10}$`. Uses [`OsRng`] with rejection sampling (rather than
/// `byte % 10`) so the digit distribution is uniform.
fn generate_account_code() -> String {
    let mut rng = OsRng;
    let mut out = String::with_capacity(ACCOUNT_CODE_LEN);
    let mut buf = [0u8; 1];
    for _ in 0..ACCOUNT_CODE_LEN {
        loop {
            rng.fill_bytes(&mut buf);
            if buf[0] < ACCOUNT_CODE_REJECTION_THRESHOLD {
                break;
            }
        }
        out.push(char::from(b'0' + (buf[0] % 10)));
    }
    out
}

/// Up-front input validation. Runs before any DB hit so malformed
/// requests fail with a single round-trip and don't tie up a connection
/// on bcrypt.
fn validate_register(req: &RegisterRequest) -> AppResult<()> {
    let email = req.email.trim();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || !email.validate_email() {
        return Err(AppError::BadRequest("invalid email format".into()));
    }
    if req.password.len() < MIN_PASSWORD_LEN || req.password.len() > MAX_PASSWORD_LEN {
        return Err(AppError::BadRequest(format!(
            "password must be {MIN_PASSWORD_LEN}..={MAX_PASSWORD_LEN} characters"
        )));
    }
    if let Some(n) = req.nickname.as_deref() {
        if n.chars().count() > MAX_NICKNAME_LEN {
            return Err(AppError::BadRequest(format!(
                "nickname must be ≤ {MAX_NICKNAME_LEN} characters"
            )));
        }
    }
    // Cheap shape check on the invitation code so a wildly malformed
    // value bails out before the SHA-256 hash. Length matches the
    // `INV-{20 chars}` plaintext shape; non-matching plaintext won't
    // hit any row anyway, but we'd rather not waste a DB round-trip.
    if req.invitation_code.is_empty() || req.invitation_code.len() > 64 {
        return Err(AppError::BadRequest("invalid invitation code".into()));
    }
    Ok(())
}

/// Insert a user, retrying on `account_code` collisions and surfacing
/// email conflicts as `AppError::Conflict`. Returns the freshly-allocated
/// `(user_id, account_code)` on success.
///
/// Lives on the transaction connection so `email`/`account_code` writes
/// roll back together with the role insert and invitation update if any
/// later step fails.
async fn insert_user_with_retry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    email: &str,
    password_hash: &str,
    nickname: Option<&str>,
) -> AppResult<(Uuid, String)> {
    for _ in 0..ACCOUNT_CODE_MAX_RETRIES {
        let account_code = generate_account_code();
        // ON CONFLICT (account_code) makes a code clash a silent no-op
        // (Ok(None) below) — we just retry. Any *other* unique violation
        // (in practice: email) still raises so the caller can map it to
        // a 409.
        let result = sqlx::query_as::<_, (Uuid,)>(
            r#"
            INSERT INTO users (email, password_hash, account_code, nickname)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (account_code) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(email)
        .bind(password_hash)
        .bind(&account_code)
        .bind(nickname)
        .fetch_optional(&mut **tx)
        .await;

        match result {
            Ok(Some((id,))) => return Ok((id, account_code)),
            // account_code collision (the only one ON CONFLICT swallows).
            Ok(None) => continue,
            Err(sqlx::Error::Database(db_err))
                if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION) =>
            {
                // With ON CONFLICT (account_code) DO NOTHING above, the
                // remaining unique constraint that can fire here is
                // `users_email_key`. Treat any 23505 that escaped the
                // ON CONFLICT swallow as an email duplicate — a UUID
                // collision on `id` is astronomically unlikely.
                return Err(AppError::Conflict("email already registered".into()));
            }
            Err(e) => {
                return Err(AppError::Internal(anyhow::anyhow!("insert user: {e}")));
            }
        }
    }
    Err(AppError::Internal(anyhow::anyhow!(
        "could not allocate unique account_code after {ACCOUNT_CODE_MAX_RETRIES} attempts"
    )))
}

/// Mark `invitation_id` as consumed by `user_id`. The `WHERE status='unused'`
/// guard makes this a compare-and-swap: a concurrent registration that
/// already flipped the row to 'used' will leave us with 0 rows affected,
/// signalling a 409.
async fn mark_invitation_used(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    invitation_id: Uuid,
    user_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE invitations
        SET status = 'used', used_by = $1, used_at = NOW()
        WHERE id = $2 AND status = 'unused'
        "#,
    )
    .bind(user_id)
    .bind(invitation_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// POST /api/auth/register
pub async fn register_handler(
    body: web::Json<RegisterRequest>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let req = body.into_inner();
    validate_register(&req)?;

    // Normalize for storage: emails compare case-insensitively in
    // practice, so we'd rather not let `Foo@x.com` and `foo@x.com`
    // become two accounts. Trim too — leading/trailing whitespace is
    // never meaningful here.
    let email = req.email.trim().to_lowercase();
    let nickname = req
        .nickname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // Step 2: invitation lookup. Hash, then index-hit on code_hash.
    let inv_hash = hash_code(&req.invitation_code);
    let inv = find_invitation_by_hash(&state.db, &inv_hash)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("lookup invitation: {e}")))?
        .ok_or_else(|| AppError::BadRequest("invalid invitation code".into()))?;

    // The acceptance criterion calls out the used-code case explicitly:
    // 409 Conflict. Other invalid cases (revoked, expired, etc.) collapse
    // to a generic 400 — there's no actionable distinction for the user.
    if inv.status == "used" {
        return Err(AppError::Conflict("invitation code already used".into()));
    }
    if inv.status != "unused" || inv.expires_at.and_utc() <= Utc::now() {
        return Err(AppError::BadRequest("invalid invitation code".into()));
    }

    // Step 3: bcrypt before opening the transaction. Hashing at cost 12
    // takes ~250ms on commodity hardware and we don't want to tie up a
    // pooled DB connection for that long.
    let password_hash = bcrypt_hash(&req.password, BCRYPT_COST)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("bcrypt hash: {e}")))?;

    // Step 4: one transaction for the user/role/invitation triple.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("begin tx: {e}")))?;

    let (user_id, account_code) =
        insert_user_with_retry(&mut tx, &email, &password_hash, nickname.as_deref()).await?;

    sqlx::query(r#"INSERT INTO user_roles (user_id, role) VALUES ($1, $2)"#)
        .bind(user_id)
        .bind(DEFAULT_ROLE)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("insert user_role: {e}")))?;

    let invitation_rows = mark_invitation_used(&mut tx, inv.id, user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("mark invitation used: {e}")))?;
    if invitation_rows == 0 {
        // Race: a concurrent request consumed the same code between
        // our SELECT and UPDATE. Drop tx (auto-rollback) and report
        // the same 409 the explicit "already used" branch would.
        return Err(AppError::Conflict("invitation code already used".into()));
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("commit tx: {e}")))?;

    // Step 5: sign tokens and emit the three-cookie set. None of these
    // values appear in the JSON body — that's the whole point.
    let access_token = state
        .jwt_keys
        .sign_access_token(user_id, DEFAULT_ROLE)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("sign access token: {e}")))?;
    let refresh_token = state
        .jwt_keys
        .sign_refresh_token(user_id)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("sign refresh token: {e}")))?;
    let csrf_token = generate_csrf_token();

    // Whitelist the refresh token *before* writing cookies. If Redis is
    // unreachable we'd rather fail the registration (the user retries and
    // the DB constraint will accept the second attempt because the
    // invitation was rolled back along with the user row had the txn
    // failed — but the txn already committed here, so a Redis outage
    // means a partially-onboarded user). Best-effort: log a warning and
    // continue. The user can always re-login once Redis is back; their
    // first /api/auth/refresh would 401 until they do.
    if let Err(e) = add_refresh_token(
        &state.redis,
        user_id,
        &refresh_token,
        state.config.jwt_refresh_ttl_seconds.max(0) as u64,
    )
    .await
    {
        log::warn!("register whitelist add failed for {user_id}: {e}");
    }

    let cookies = AuthCookies {
        access: access_token,
        refresh: Some(refresh_token),
        csrf: Some(csrf_token),
    };

    let mut builder = HttpResponse::Created();
    set_auth_cookies(&mut builder, &cookies, &state.config);
    Ok(builder.json(ApiResponse::build(RegisterResponse {
        user_id,
        account_code,
    })))
}

// ────────────────────────────────────────────────────────────────────────
// Login (TODO [18])
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Login response body. Same minimal shape as registration — anything
/// useful enough to expose here (role, etc.) the SPA can fetch from
/// `/api/users/me` once it's authenticated.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user_id: Uuid,
    pub account_code: String,
}

/// User row joined with role for login. Not exposed publicly — fields are
/// consumed by the handler and dropped before any response is built.
#[derive(Debug, sqlx::FromRow)]
struct UserAuthRow {
    id: Uuid,
    password_hash: String,
    account_code: String,
    role: String,
}

/// Look up by lowercased email. The `LEFT JOIN` + `COALESCE` makes the
/// query resilient to a (theoretically impossible — register inserts both
/// rows in one transaction) missing `user_roles` row: rather than 500ing,
/// the user gets the default 'user' role.
async fn find_user_for_login(
    pool: &PgPool,
    email: &str,
) -> Result<Option<UserAuthRow>, sqlx::Error> {
    sqlx::query_as::<_, UserAuthRow>(
        r#"
        SELECT u.id, u.password_hash, u.account_code, COALESCE(r.role, 'user') AS role
        FROM users u
        LEFT JOIN user_roles r ON r.user_id = u.id
        WHERE u.email = $1
        "#,
    )
    .bind(email)
    .fetch_optional(pool)
    .await
}

/// One-time-computed dummy hash used when the looked-up email returns no
/// row. Running `bcrypt_verify` against this keeps the wall-clock time of
/// "no such user" indistinguishable from "wrong password" — the bcrypt
/// hash dominates the request, and both paths run it exactly once.
///
/// `OnceLock` over `LazyLock` because the project's MSRV is 1.70 (and
/// LazyLock stabilized in 1.80).
static DUMMY_PASSWORD_HASH: OnceLock<String> = OnceLock::new();

fn dummy_password_hash() -> &'static str {
    DUMMY_PASSWORD_HASH
        .get_or_init(|| {
            bcrypt_hash("dummy-password-no-real-user-has-this", BCRYPT_COST)
                .expect("compute dummy bcrypt hash")
        })
        .as_str()
}

fn lockout_key(email: &str) -> String {
    format!("{LOGIN_LOCKOUT_KEY_PREFIX}{email}")
}

/// Read the current failure counter for `email`. Returns 0 when the key
/// is missing — a brand-new email has no Redis entry yet.
async fn login_failure_count(client: &redis::Client, email: &str) -> redis::RedisResult<u32> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let count: Option<u32> = redis::cmd("GET")
        .arg(lockout_key(email))
        .query_async(&mut conn)
        .await?;
    Ok(count.unwrap_or(0))
}

/// Increment the failure counter and (re-)set its TTL. Returns the new
/// value so the caller can detect the boundary "this attempt just locked
/// the account". The TTL refresh on every increment is what makes the
/// lockout window slide — sustained brute force never expires the key.
async fn record_login_failure(client: &redis::Client, email: &str) -> redis::RedisResult<u32> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = lockout_key(email);
    let count: u32 = redis::cmd("INCR")
        .arg(&key)
        .query_async(&mut conn)
        .await?;
    redis::cmd("EXPIRE")
        .arg(&key)
        .arg(LOGIN_LOCKOUT_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await?;
    Ok(count)
}

/// Drop the failure counter on successful login.
async fn clear_login_failures(client: &redis::Client, email: &str) -> redis::RedisResult<()> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("DEL")
        .arg(lockout_key(email))
        .query_async::<()>(&mut conn)
        .await?;
    Ok(())
}

/// POST /api/auth/login
pub async fn login_handler(
    body: web::Json<LoginRequest>,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let req = body.into_inner();

    // Cheap shape gate. Fails with the same generic 401 the auth path
    // uses so the caller can't tell "field missing" from "wrong creds".
    if req.email.trim().is_empty()
        || req.password.is_empty()
        || req.email.len() > MAX_EMAIL_LEN
        || req.password.len() > MAX_PASSWORD_LEN
    {
        return Err(AppError::Unauthorized(LOGIN_GENERIC_ERROR.into()));
    }

    let email = req.email.trim().to_lowercase();

    let user = find_user_for_login(&state.db, &email)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("login user lookup: {e}")))?;

    // Run bcrypt regardless of user presence — timing must not betray
    // existence. Both branches do exactly one verify against a real-shape
    // hash; whether it returns Ok(true) is what differs.
    let password_correct = match &user {
        Some(u) => bcrypt_verify(&req.password, &u.password_hash).unwrap_or(false),
        None => {
            let _ = bcrypt_verify(&req.password, dummy_password_hash()).unwrap_or(false);
            false
        }
    };

    // Lockout check is best-effort: a Redis hiccup mustn't take auth
    // entirely offline. Treat a read failure as "not locked" and rely on
    // the per-IP rate limiter as the safety net.
    let locked = match &user {
        Some(_) => login_failure_count(&state.redis, &email)
            .await
            .map(|c| c >= MAX_LOGIN_FAILURES)
            .unwrap_or_else(|e| {
                log::warn!("login lockout read failed for {email}: {e}");
                false
            }),
        None => false,
    };

    if !password_correct || locked {
        // Only record failures against existing emails. Counting unknown
        // emails would let an attacker stuff Redis with arbitrary keys
        // and inflate memory.
        if user.is_some() && !password_correct {
            match record_login_failure(&state.redis, &email).await {
                Ok(count) if count == MAX_LOGIN_FAILURES => {
                    log::info!("login lockout triggered for {email} after {count} failures");
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("login failure counter increment failed for {email}: {e}");
                }
            }
        }
        return Err(AppError::Unauthorized(LOGIN_GENERIC_ERROR.into()));
    }

    // Success: by elimination, user.is_some() && password_correct && !locked.
    let user = user.expect("user is Some after successful password verify");

    // Best-effort clear — a stale counter just means one more wrong
    // attempt later might lock the user prematurely. Not worth failing
    // a successful login over.
    if let Err(e) = clear_login_failures(&state.redis, &email).await {
        log::warn!("login lockout clear failed for {email}: {e}");
    }

    let access_token = state
        .jwt_keys
        .sign_access_token(user.id, &user.role)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("sign access token: {e}")))?;
    let refresh_token = state
        .jwt_keys
        .sign_refresh_token(user.id)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("sign refresh token: {e}")))?;
    let csrf_token = generate_csrf_token();

    // Whitelist the freshly minted refresh token. Same best-effort policy
    // as register — a Redis hiccup degrades only that specific session's
    // first refresh attempt; the user can re-login.
    if let Err(e) = add_refresh_token(
        &state.redis,
        user.id,
        &refresh_token,
        state.config.jwt_refresh_ttl_seconds.max(0) as u64,
    )
    .await
    {
        log::warn!("login whitelist add failed for {}: {e}", user.id);
    }

    let cookies = AuthCookies {
        access: access_token,
        refresh: Some(refresh_token),
        csrf: Some(csrf_token),
    };

    let mut builder = HttpResponse::Ok();
    set_auth_cookies(&mut builder, &cookies, &state.config);
    Ok(builder.json(ApiResponse::build(LoginResponse {
        user_id: user.id,
        account_code: user.account_code,
    })))
}

// ────────────────────────────────────────────────────────────────────────
// Refresh & logout (TODO [19])
// ────────────────────────────────────────────────────────────────────────

/// Generic 401 emitted by the refresh handler. Pinned for the same reason
/// `LOGIN_GENERIC_ERROR` is: a single message keeps "no cookie", "bad
/// signature", and "revoked" indistinguishable from the client side.
const REFRESH_GENERIC_ERROR: &str = "invalid or expired refresh token";

/// Body returned on a successful refresh. We don't expose any token
/// material — just confirm the user the cookies belong to so a SPA can
/// double-check identity if it wants to.
#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub user_id: Uuid,
}

/// Body returned on logout. Trivial — the client only cares that the
/// cookies are gone.
#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub ok: bool,
}

/// Look up the user's role for a freshly minted access token. Falls back
/// to the default role if the user_roles row is missing — same defensive
/// posture as the login query — so a corrupt-but-recoverable role row
/// never traps the user in a refresh loop.
async fn fetch_user_role(pool: &PgPool, user_id: Uuid) -> Result<String, sqlx::Error> {
    let role: Option<String> = sqlx::query_scalar(
        r#"
        SELECT role
        FROM user_roles
        WHERE user_id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(role.unwrap_or_else(|| DEFAULT_ROLE.to_owned()))
}

/// POST /api/auth/refresh
///
/// Reads the path-scoped `refresh_token` cookie, verifies it, asserts it
/// is still in the Redis whitelist, then mints a new access + csrf pair.
/// The refresh cookie itself is unchanged — its lifetime is anchored to
/// the original login.
pub async fn refresh_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let token = req
        .cookie(REFRESH_COOKIE_NAME)
        .map(|c| c.value().to_owned())
        .ok_or_else(|| AppError::Unauthorized(REFRESH_GENERIC_ERROR.into()))?;

    // JWT validation: signature, exp, and `typ == refresh`. Any failure
    // collapses to the same 401 — we never tell the client *why*.
    let claims = state
        .jwt_keys
        .verify_token(&token, TokenType::Refresh)
        .map_err(|_| AppError::Unauthorized(REFRESH_GENERIC_ERROR.into()))?;

    let user_id = claims.sub;

    // Redis whitelist check is mandatory: a token whose JWT verifies but
    // is missing from the whitelist has either been revoked (logout) or
    // was never recorded (e.g. login during a Redis outage). Both cases
    // are treated as "not currently a valid session" — re-login required.
    let active = is_refresh_token_active(&state.redis, user_id, &token)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("refresh whitelist check: {e}")))?;
    if !active {
        return Err(AppError::Unauthorized(REFRESH_GENERIC_ERROR.into()));
    }

    // Refresh tokens carry no `role` claim — fetch the current one so the
    // new access token reflects any privilege change since login (e.g.
    // admin promotion via TODO [23]).
    let role = fetch_user_role(&state.db, user_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch role for refresh: {e}")))?;

    let access_token = state
        .jwt_keys
        .sign_access_token(user_id, &role)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("sign access token: {e}")))?;
    let csrf_token = generate_csrf_token();

    // refresh: None — the existing refresh cookie keeps its original
    // expiry. Per ARCHITECTURE.md §4.1 the refresh endpoint re-issues
    // access + csrf only.
    let cookies = AuthCookies {
        access: access_token,
        refresh: None,
        csrf: Some(csrf_token),
    };

    let mut builder = HttpResponse::Ok();
    set_auth_cookies(&mut builder, &cookies, &state.config);
    Ok(builder.json(ApiResponse::build(RefreshResponse { user_id })))
}

/// POST /api/auth/logout
///
/// Authenticates against the **access** cookie (the refresh cookie is
/// path-scoped and not sent here), revokes every refresh token for the
/// user via [`revoke_all_refresh_tokens`], and emits Set-Cookie headers
/// that expire all three auth cookies.
///
/// Returning 401 on a missing/expired access token is intentional: it
/// forces the SPA to refresh-then-retry rather than letting a stale
/// session quietly skip Redis revocation.
pub async fn logout_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> AppResult<HttpResponse> {
    let access = req
        .cookie(ACCESS_COOKIE_NAME)
        .map(|c| c.value().to_owned())
        .ok_or_else(|| AppError::Unauthorized("missing access token".into()))?;

    let claims = state
        .jwt_keys
        .verify_token(&access, TokenType::Access)
        .map_err(|_| AppError::Unauthorized("invalid access token".into()))?;

    // Best-effort revocation: if Redis is briefly unavailable, log loud
    // and clear cookies anyway. The acceptance criterion ("登出后 refresh
    // 端点返回 401") covers the happy path; a Redis outage means the
    // user's refresh tokens stay live until their natural 7-day expiry,
    // but their browser no longer holds the cookies.
    if let Err(e) = revoke_all_refresh_tokens(&state.redis, claims.sub).await {
        log::warn!("logout revoke failed for {}: {e}", claims.sub);
    }

    let mut builder = HttpResponse::Ok();
    clear_auth_cookies(&mut builder, &state.config);
    Ok(builder.json(ApiResponse::build(LogoutResponse { ok: true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Account code generator ──

    #[test]
    fn account_code_is_ten_digits() {
        for _ in 0..50 {
            let code = generate_account_code();
            assert_eq!(code.len(), ACCOUNT_CODE_LEN);
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "non-digit in {code}"
            );
        }
    }

    /// Pin the schema's CHECK constraint shape — if anyone changes the
    /// generator to emit non-numeric chars, the DB would reject it.
    #[test]
    fn account_code_matches_check_constraint_regex() {
        // Equivalent of `^[0-9]{10}$`.
        let code = generate_account_code();
        assert_eq!(code.len(), 10);
        for c in code.chars() {
            assert!(c.is_ascii_digit(), "char {c} fails ^[0-9]{{10}}$");
        }
    }

    #[test]
    fn account_codes_are_distinct_across_many_calls() {
        // 10^10 search space → collisions in 200 draws are statistically
        // negligible (< 1 in 50 million). If this ever fires, OsRng or the
        // rejection sampling broke.
        let mut seen = HashSet::with_capacity(200);
        for _ in 0..200 {
            assert!(seen.insert(generate_account_code()));
        }
    }

    // ── Input validation ──

    fn good_request() -> RegisterRequest {
        RegisterRequest {
            email: "alice@example.com".into(),
            password: "supersecret".into(),
            invitation_code: "INV-AAAAAAAAAAAAAAAAAAAA".into(),
            nickname: Some("Alice".into()),
        }
    }

    #[test]
    fn validate_register_accepts_minimal_valid_request() {
        validate_register(&good_request()).expect("valid input");
    }

    #[test]
    fn validate_register_rejects_short_password() {
        let mut req = good_request();
        req.password = "short".into();
        let err = validate_register(&req).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_register_rejects_eight_minus_one_password() {
        // Boundary: 7 chars must fail, 8 must pass. Pins the spec's
        // ">= 8" against an off-by-one regression.
        let mut req = good_request();
        req.password = "a".repeat(7);
        assert!(validate_register(&req).is_err());
        req.password = "a".repeat(8);
        validate_register(&req).expect("8-char password is allowed");
    }

    #[test]
    fn validate_register_rejects_oversized_password() {
        let mut req = good_request();
        req.password = "a".repeat(MAX_PASSWORD_LEN + 1);
        assert!(validate_register(&req).is_err());
    }

    #[test]
    fn validate_register_rejects_malformed_email() {
        for bad in ["", "no-at-sign.com", "x@", "@x.com", "spaces in@x.com"] {
            let mut req = good_request();
            req.email = bad.into();
            assert!(
                validate_register(&req).is_err(),
                "expected reject for: {bad:?}"
            );
        }
    }

    #[test]
    fn validate_register_rejects_oversized_nickname() {
        let mut req = good_request();
        req.nickname = Some("A".repeat(MAX_NICKNAME_LEN + 1));
        assert!(validate_register(&req).is_err());
    }

    #[test]
    fn validate_register_accepts_nickname_at_max_length() {
        let mut req = good_request();
        // VARCHAR(100) is character-length, not byte-length — pin it
        // here so a future swap to byte-counting reading would fail.
        req.nickname = Some("A".repeat(MAX_NICKNAME_LEN));
        validate_register(&req).expect("max-length nickname is allowed");
    }

    #[test]
    fn validate_register_rejects_empty_invitation_code() {
        let mut req = good_request();
        req.invitation_code = String::new();
        assert!(validate_register(&req).is_err());
    }

    // ── Response shape ──

    /// The acceptance criterion ("响应体不返回任何 token") is enforced
    /// at the type level: `RegisterResponse` has only the user_id and
    /// the public account_code. Pin this so a future refactor that adds
    /// a `token` field has to update this test.
    #[test]
    fn register_response_serializes_without_token_fields() {
        let resp = RegisterResponse {
            user_id: Uuid::new_v4(),
            account_code: "0123456789".into(),
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        let obj = json.as_object().expect("object");

        for forbidden in [
            "access_token",
            "refresh_token",
            "csrf_token",
            "token",
            "password",
            "password_hash",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "register response must not include {forbidden}: {json}"
            );
        }
        assert!(obj.contains_key("user_id"));
        assert!(obj.contains_key("account_code"));
    }

    // ── Login (TODO [18]) ──

    /// Pin the exact Redis key shape so the read/write/clear paths can't
    /// silently diverge — and so an external dashboard scraping these
    /// keys doesn't break on a rename.
    #[test]
    fn lockout_key_is_namespaced() {
        assert_eq!(
            lockout_key("alice@example.com"),
            "login_lockout:alice@example.com"
        );
        assert!(lockout_key("x").starts_with(LOGIN_LOCKOUT_KEY_PREFIX));
    }

    /// The acceptance criterion is verbatim text — pin it so a future
    /// refactor can't quietly change the message and create a (subtle)
    /// user-existence oracle by giving wrong-password and unknown-email
    /// different strings.
    #[test]
    fn login_generic_error_message_is_pinned() {
        assert_eq!(LOGIN_GENERIC_ERROR, "邮箱或密码错误");
    }

    /// `MAX_LOGIN_FAILURES` and the TTL pin the spec ("5 次锁定 15 分钟")
    /// against accidental drift.
    #[test]
    fn lockout_thresholds_match_spec() {
        assert_eq!(MAX_LOGIN_FAILURES, 5);
        assert_eq!(LOGIN_LOCKOUT_TTL_SECONDS, 15 * 60);
    }

    /// The dummy hash exists to give the no-such-email branch a real
    /// bcrypt cost. If it ever stopped being a valid bcrypt hash, the
    /// timing parity would silently regress (verify would fail-fast on
    /// a malformed hash). Pin both shape and that verify accepts it.
    #[test]
    fn dummy_password_hash_is_valid_bcrypt() {
        let h = dummy_password_hash();
        assert!(h.starts_with("$2"), "expected bcrypt prefix, got: {h}");
        // verify must not error — it should return Ok(false) for any
        // password that isn't the secret seed used to generate it.
        let result = bcrypt_verify("definitely-not-the-seed", h);
        assert!(
            result.is_ok(),
            "dummy hash must verify-clean: {result:?}"
        );
        assert!(!result.unwrap(), "an arbitrary password must not verify");
    }

    /// Successive calls return the same string slice — hashing is paid
    /// exactly once per process. Without this, every cold no-such-email
    /// request would pay an extra bcrypt cost.
    #[test]
    fn dummy_password_hash_is_memoized() {
        let a = dummy_password_hash();
        let b = dummy_password_hash();
        assert_eq!(a.as_ptr(), b.as_ptr(), "OnceLock should hand back the same allocation");
    }

    /// Same anti-leak check as for register, but for the login response.
    #[test]
    fn login_response_serializes_without_token_fields() {
        let resp = LoginResponse {
            user_id: Uuid::new_v4(),
            account_code: "0123456789".into(),
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        let obj = json.as_object().expect("object");

        for forbidden in [
            "access_token",
            "refresh_token",
            "csrf_token",
            "token",
            "password",
            "password_hash",
            "role", // intentionally not exposed; SPA fetches /api/users/me
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "login response must not include {forbidden}: {json}"
            );
        }
        assert!(obj.contains_key("user_id"));
        assert!(obj.contains_key("account_code"));
    }

    // ── Refresh & logout (TODO [19]) ──

    /// Pin the generic 401 message so a future edit can't accidentally
    /// expose *why* a refresh failed (signature vs. expiry vs. revoked).
    /// Matches the same opacity property the login error has.
    #[test]
    fn refresh_generic_error_message_is_pinned() {
        assert_eq!(REFRESH_GENERIC_ERROR, "invalid or expired refresh token");
    }

    /// Refresh response must not leak token material — same anti-leak
    /// invariant as register/login responses.
    #[test]
    fn refresh_response_serializes_without_token_fields() {
        let resp = RefreshResponse {
            user_id: Uuid::new_v4(),
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        let obj = json.as_object().expect("object");

        for forbidden in [
            "access_token",
            "refresh_token",
            "csrf_token",
            "token",
            "role",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "refresh response must not include {forbidden}: {json}"
            );
        }
        assert!(obj.contains_key("user_id"));
    }

    /// Logout response is intentionally minimal — `{ ok: true }`. Pin the
    /// shape so a future change that starts returning user data has to
    /// update this test (logout should not be a side-channel).
    #[test]
    fn logout_response_is_minimal_ok_shape() {
        let resp = LogoutResponse { ok: true };
        let json = serde_json::to_value(&resp).expect("serialize");
        let obj = json.as_object().expect("object");

        assert_eq!(obj.len(), 1, "logout response should have a single field");
        assert_eq!(obj["ok"], serde_json::json!(true));
    }
}
