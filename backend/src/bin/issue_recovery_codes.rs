//! One-time operator tool (nels#551): issues a fresh batch of recovery codes
//! for an existing account and prints them to THIS process's own stdout —
//! run locally by the operator (`cargo run --bin issue_recovery_codes --
//! someone@example.com`), never as part of the deployed service. This is
//! deliberately separate from the `backend` binary and never logs anything
//! through `tracing`: the deployed server's logs are shipped to Fly's log
//! aggregation, and a plaintext credential must never land there. Standalone
//! rather than reusing `passkeys::issue_recovery_codes` because this crate is
//! bin-only (no `lib.rs`) — the code-gen/hashing here must stay in sync with
//! `backend/src/passkeys.rs`'s `generate_one_code`/`hash_code` if either
//! changes.
//!
//! Usage: `DATABASE_URL=... cargo run --bin issue_recovery_codes -- <email>`
//! The operator hand-delivers the printed codes out of band (no email
//! infrastructure) and discards their terminal scrollback afterward.

use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const RECOVERY_CODE_COUNT: usize = 10;
const RECOVERY_CODE_BYTES: usize = 10;

fn generate_one_code() -> String {
    let random_bytes: Vec<u8> = rand::thread_rng()
        .sample_iter(&rand::distributions::Standard)
        .take(RECOVERY_CODE_BYTES)
        .collect();
    let raw = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &random_bytes);
    raw.as_bytes()
        .chunks(5)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("-")
}

fn hash_code(code: &str) -> String {
    let normalized = code.trim().to_uppercase();
    hex::encode(Sha256::digest(normalized.as_bytes()))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let email = std::env::args().nth(1).ok_or("usage: issue_recovery_codes <email>")?;
    let email = email.trim().to_lowercase();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must be set (point it at the target environment's DB)")?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&database_url).await?;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| format!("no user with email {email}"))?;

    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL")
        .bind(user_id)
        .fetch_one(&pool)
        .await?;
    if existing > 0 {
        return Err(format!(
            "{email} already has {existing} unused recovery code(s) — refusing to add more. \
             Use the admin console's credential reset if you need a fresh batch."
        )
        .into());
    }

    let mut codes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    let mut tx = pool.begin().await?;
    for _ in 0..RECOVERY_CODE_COUNT {
        let code = generate_one_code();
        let hash = hash_code(&code);
        sqlx::query("INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        codes.push(code);
    }
    tx.commit().await?;

    println!("Recovery codes for {email} (shown once — copy them now):\n");
    for code in &codes {
        println!("  {code}");
    }
    println!("\nHand-deliver these out of band, then clear your terminal scrollback.");

    Ok(())
}
