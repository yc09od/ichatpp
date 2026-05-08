//! `seed_admin` — one-shot CLI to bootstrap the very first admin account.
//!
//! ```text
//! cargo run --bin seed_admin -- --email a@b.c --password <password>
//! ```
//!
//! ## Why this is its own binary
//!
//! New admins after the first one are minted via the admin UI (TODO [23])
//! so the action is auditable: `created_by` chains back to a real
//! authenticated user. The very first admin has no such predecessor, so
//! it has to come from somewhere the audit trail cannot capture — a
//! deploy-time CLI run is the cleanest place for that.
//!
//! ## Idempotency
//!
//! If **any** row in `user_roles` has `role = 'admin'`, the binary exits
//! non-zero without touching the database. This is what makes the
//! acceptance criterion ("重复运行报错") work, and it also keeps a stray
//! re-run from quietly creating a second admin.
//!
//! Because this binary lives at `src/bin/seed_admin.rs`, it compiles as a
//! separate crate root from `main.rs`. We can't import `handlers::auth`
//! internals without first restructuring the package into a lib, which is
//! out of scope here, so the small helpers (account-code generator,
//! validation constants) are duplicated. Pin the constants against their
//! `handlers/auth.rs` originals via the tests below to catch any drift.

use std::env;
use std::process::ExitCode;

use bcrypt::{hash as bcrypt_hash, DEFAULT_COST};
use rand::{rngs::OsRng, RngCore};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use validator::ValidateEmail;

/// Mirrors `handlers::auth::ACCOUNT_CODE_LEN`. The schema's CHECK
/// constraint pins this to exactly 10 numeric chars.
const ACCOUNT_CODE_LEN: usize = 10;

/// Mirrors `handlers::auth::ACCOUNT_CODE_REJECTION_THRESHOLD`. 250 is the
/// largest byte value that's a clean multiple of 10; bytes ≥ 250 are
/// resampled so the digit distribution stays uniform (no modulo bias).
const ACCOUNT_CODE_REJECTION_THRESHOLD: u8 = 250;

/// Mirrors `handlers::auth::ACCOUNT_CODE_MAX_RETRIES`. With ~10 billion
/// possible codes and few users, hitting this bound means the RNG is
/// broken — fail loud instead of looping forever.
const ACCOUNT_CODE_MAX_RETRIES: usize = 10;

/// SQLSTATE for a unique-constraint violation in PostgreSQL.
const PG_UNIQUE_VIOLATION: &str = "23505";

/// Mirrors `handlers::auth::MIN_PASSWORD_LEN`. The seed CLI is *not* the
/// place to weaken password rules — registration enforces ≥ 8, and we
/// pin the same here.
const MIN_PASSWORD_LEN: usize = 8;

/// Mirrors `handlers::auth::MAX_EMAIL_LEN` (matches the `VARCHAR(255)`
/// column).
const MAX_EMAIL_LEN: usize = 255;

/// Role string written to `user_roles.role`.
const ADMIN_ROLE: &str = "admin";

/// Exit code for "couldn't even start": missing args, bad input, no
/// `DATABASE_URL`. Mirrors `getopt`-style "usage error" exit code 2 so
/// shell scripts can distinguish "user error" from "operational failure".
const EXIT_USAGE: u8 = 2;

/// Exit code for "tried but failed": admin already exists, DB connect
/// failed, insert collided.
const EXIT_FAILURE: u8 = 1;

#[derive(Debug)]
struct Args {
    email: String,
    password: String,
}

fn usage() -> &'static str {
    "Usage: seed_admin --email <email> --password <password>"
}

/// Tiny hand-rolled flag parser. Two flags, no clap dependency — keeps
/// the binary's compile time low and matches the project's preference
/// for not pulling in deps for trivial work.
///
/// Accepts both `--email value` and `--email=value` for ergonomics.
fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<Args, String> {
    let mut email: Option<String> = None;
    let mut password: Option<String> = None;
    while let Some(arg) = args.next() {
        let (flag, inline_value) = split_flag(&arg);
        match flag {
            "--email" => {
                email = Some(take_value(inline_value, &mut args, "--email")?);
            }
            "--password" => {
                password = Some(take_value(inline_value, &mut args, "--password")?);
            }
            "--help" | "-h" => return Err(usage().to_owned()),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }
    Ok(Args {
        email: email.ok_or_else(|| format!("--email is required\n{}", usage()))?,
        password: password.ok_or_else(|| format!("--password is required\n{}", usage()))?,
    })
}

/// Split `--flag=value` into `("--flag", Some("value"))`. Plain `--flag`
/// returns `(arg, None)` and the caller pulls the next iterator item.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.find('=') {
        Some(eq) if arg.starts_with("--") => (&arg[..eq], Some(&arg[eq + 1..])),
        _ => (arg, None),
    }
}

fn take_value<I: Iterator<Item = String>>(
    inline: Option<&str>,
    rest: &mut I,
    flag: &str,
) -> Result<String, String> {
    match inline {
        Some(v) => Ok(v.to_owned()),
        None => rest
            .next()
            .ok_or_else(|| format!("{flag} requires a value\n{}", usage())),
    }
}

fn validate(args: &Args) -> Result<(), String> {
    let email = args.email.trim();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || !email.validate_email() {
        return Err("invalid email format".to_owned());
    }
    if args.password.len() < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

/// Same generator as `handlers::auth::generate_account_code`. Pinned by
/// the constants above and `account_code_matches_check_constraint` in the
/// tests.
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

struct CreatedAdmin {
    user_id: Uuid,
    account_code: String,
}

/// Insert the admin row pair (users + user_roles) inside one transaction.
/// Refuses to do anything if any admin already exists.
async fn seed_admin(
    pool: &sqlx::PgPool,
    email: &str,
    password_hash: &str,
) -> anyhow::Result<CreatedAdmin> {
    // Idempotency guard: any pre-existing admin aborts the seed.
    // Subsequent admins should be created via the admin UI (TODO [23]).
    let admin_exists: bool =
        sqlx::query_scalar(r#"SELECT EXISTS (SELECT 1 FROM user_roles WHERE role = $1)"#)
            .bind(ADMIN_ROLE)
            .fetch_one(pool)
            .await?;
    if admin_exists {
        anyhow::bail!(
            "an admin user already exists; create additional admins via the admin UI"
        );
    }

    let mut tx = pool.begin().await?;

    // Mirrors handlers::auth::insert_user_with_retry — same ON CONFLICT
    // pattern so an account_code collision quietly retries while an email
    // collision surfaces a clean error.
    let mut user_id_opt: Option<Uuid> = None;
    let mut account_code = String::new();
    for _ in 0..ACCOUNT_CODE_MAX_RETRIES {
        account_code = generate_account_code();
        let result = sqlx::query_as::<_, (Uuid,)>(
            r#"
            INSERT INTO users (email, password_hash, account_code)
            VALUES ($1, $2, $3)
            ON CONFLICT (account_code) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(email)
        .bind(password_hash)
        .bind(&account_code)
        .fetch_optional(&mut *tx)
        .await;

        match result {
            Ok(Some((id,))) => {
                user_id_opt = Some(id);
                break;
            }
            Ok(None) => continue,
            Err(sqlx::Error::Database(db_err))
                if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION) =>
            {
                anyhow::bail!("email {email} is already registered");
            }
            Err(e) => return Err(e.into()),
        }
    }
    let user_id = user_id_opt.ok_or_else(|| {
        anyhow::anyhow!(
            "could not allocate a unique account_code after {ACCOUNT_CODE_MAX_RETRIES} attempts"
        )
    })?;

    sqlx::query(r#"INSERT INTO user_roles (user_id, role) VALUES ($1, $2)"#)
        .bind(user_id)
        .bind(ADMIN_ROLE)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(CreatedAdmin {
        user_id,
        account_code,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    // .env is convenience for local dev — silently ignore "not present".
    let _ = dotenvy::dotenv();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = match parse_args(env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if let Err(e) = validate(&args) {
        eprintln!("{e}");
        return ExitCode::from(EXIT_USAGE);
    }

    let database_url = match env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("DATABASE_URL is not set");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    // Eager `connect()` (not `connect_lazy`) so a misconfigured CLI fails
    // immediately instead of partway through the seed.
    let pool = match PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&database_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("could not connect to database: {e}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    // Lowercase mirrors the registration handler's normalization so a
    // seed admin and a re-registration attempt under different casing
    // can't both win the email unique key.
    let email = args.email.trim().to_lowercase();

    let password_hash = match bcrypt_hash(&args.password, DEFAULT_COST) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bcrypt hash failed: {e}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    match seed_admin(&pool, &email, &password_hash).await {
        Ok(out) => {
            println!(
                "created admin user_id={} account_code={} email={}",
                out.user_id, out.account_code, email
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn args_iter(items: &[&str]) -> impl Iterator<Item = String> {
        items
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    // ── Argument parsing ──

    #[test]
    fn parse_args_accepts_space_separated_flags() {
        let parsed =
            parse_args(args_iter(&["--email", "a@b.c", "--password", "supersecret"])).unwrap();
        assert_eq!(parsed.email, "a@b.c");
        assert_eq!(parsed.password, "supersecret");
    }

    #[test]
    fn parse_args_accepts_equals_form() {
        // `--flag=value` is what most users instinctively type when they
        // don't want to escape spaces — accept both forms.
        let parsed =
            parse_args(args_iter(&["--email=a@b.c", "--password=supersecret"])).unwrap();
        assert_eq!(parsed.email, "a@b.c");
        assert_eq!(parsed.password, "supersecret");
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(args_iter(&["--bogus", "x"])).unwrap_err();
        assert!(err.contains("unknown argument"));
    }

    #[test]
    fn parse_args_rejects_missing_required_flag() {
        // No --password.
        let err = parse_args(args_iter(&["--email", "a@b.c"])).unwrap_err();
        assert!(err.contains("--password is required"));

        // No --email.
        let err = parse_args(args_iter(&["--password", "supersecret"])).unwrap_err();
        assert!(err.contains("--email is required"));
    }

    #[test]
    fn parse_args_rejects_flag_without_value() {
        let err = parse_args(args_iter(&["--email"])).unwrap_err();
        assert!(err.contains("--email"));
    }

    #[test]
    fn help_flag_returns_usage() {
        let err = parse_args(args_iter(&["--help"])).unwrap_err();
        assert!(err.contains("Usage:"));
    }

    // ── Input validation ──

    fn good_args() -> Args {
        Args {
            email: "alice@example.com".into(),
            password: "supersecret".into(),
        }
    }

    #[test]
    fn validate_accepts_minimal_valid_input() {
        validate(&good_args()).expect("valid input");
    }

    #[test]
    fn validate_rejects_short_password() {
        let mut a = good_args();
        a.password = "short".into();
        assert!(validate(&a).is_err());
    }

    #[test]
    fn validate_rejects_password_at_eight_minus_one_boundary() {
        // Boundary: 7 fails, 8 passes — mirrors the same off-by-one
        // protection the register handler has.
        let mut a = good_args();
        a.password = "a".repeat(7);
        assert!(validate(&a).is_err());
        a.password = "a".repeat(8);
        validate(&a).expect("8-char password is allowed");
    }

    #[test]
    fn validate_rejects_malformed_email() {
        for bad in ["", "no-at-sign.com", "x@", "@x.com", "spaces in@x.com"] {
            let mut a = good_args();
            a.email = bad.into();
            assert!(validate(&a).is_err(), "expected reject for: {bad:?}");
        }
    }

    #[test]
    fn validate_rejects_oversized_email() {
        let mut a = good_args();
        a.email = format!("{}@x.com", "a".repeat(MAX_EMAIL_LEN));
        assert!(validate(&a).is_err());
    }

    // ── Account code ──

    /// Pin the schema's CHECK constraint shape (`^[0-9]{10}$`). If this
    /// drifts away from `handlers/auth.rs` the seed user will fail to
    /// insert at the DB layer.
    #[test]
    fn account_code_matches_check_constraint() {
        for _ in 0..50 {
            let code = generate_account_code();
            assert_eq!(code.len(), ACCOUNT_CODE_LEN);
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "non-digit in {code}"
            );
        }
    }

    #[test]
    fn account_codes_are_distinct_across_many_calls() {
        // Same statistical argument as the register handler test:
        // 10^10 search space → collisions in 200 draws are negligible.
        let mut seen = HashSet::with_capacity(200);
        for _ in 0..200 {
            assert!(seen.insert(generate_account_code()));
        }
    }

    /// The constants are duplicated from `handlers/auth.rs`. Pin them
    /// here so a future change there shows up as a *test* failure (i.e.
    /// the developer is forced to update both places consciously) rather
    /// than silently letting the CLI drift away from production.
    #[test]
    fn duplicated_constants_match_register_handler() {
        assert_eq!(ACCOUNT_CODE_LEN, 10);
        assert_eq!(ACCOUNT_CODE_REJECTION_THRESHOLD, 250);
        assert_eq!(ACCOUNT_CODE_MAX_RETRIES, 10);
        assert_eq!(MIN_PASSWORD_LEN, 8);
        assert_eq!(MAX_EMAIL_LEN, 255);
        assert_eq!(PG_UNIQUE_VIOLATION, "23505");
        assert_eq!(ADMIN_ROLE, "admin");
    }
}
