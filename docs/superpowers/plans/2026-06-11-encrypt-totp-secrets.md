# Encrypt TOTP Secrets At Rest — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Encrypt the `totp_secret` (the sole login credential) with AES-256-GCM before it is written to Postgres, so a DB dump or backup contains no usable credential, while login/enrollment keep working end-to-end.

**Architecture:** A new `crypto.rs` module exposes a `SecretCipher` wrapping `Aes256Gcm`. The key is loaded once from the `TOTP_ENC_KEY` env var at startup (fail-fast if absent/invalid) and stored in `AppState`. Secrets are stored as `"encv1:" + base64(nonce ‖ ciphertext+tag)`. `auth.rs` encrypts on every write to `users` / `auth_flows` and decrypts only in-memory to build the TOTP. Greenfield: no backfill, reads are decrypt-only.

**Tech Stack:** Rust, axum 0.7, sqlx 0.7 (Postgres), `aes-gcm` 0.10, `base64` 0.22, `rand` 0.8, `totp-rs` 5.6.

---

## File Structure

- **Create** `backend/src/crypto.rs` — `SecretCipher` (encrypt/decrypt/key loading) + `CryptoError`. Single responsibility: symmetric crypto for stored secrets.
- **Modify** `backend/Cargo.toml` — add `aes-gcm = "0.10"`.
- **Modify** `backend/src/main.rs` — declare `mod crypto;`, build the cipher (fail-fast), put it in `AppState`.
- **Modify** `backend/src/auth.rs` — add `cipher` to `AppState`; encrypt before INSERT, decrypt after SELECT, in the four handlers.
- **Modify** `backend/.env.example`, `.env.example`, `README.md`, `AGENTS.md`, `backend/fly.toml` — document `TOTP_ENC_KEY`.

All work happens in the worktree `/home/robhicks/dev/nels-worktrees/encrypt-totp-secrets` on branch `feat/encrypt-totp-secrets`. Run all `cargo` commands from `backend/`.

---

## Task 1: Add the `aes-gcm` dependency

**Files:**
- Modify: `backend/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `backend/Cargo.toml`, in the `[dependencies]` section, add this line after the existing `base64 = "0.22"` line:

```toml
aes-gcm = "0.10"
```

- [ ] **Step 2: Verify it resolves and compiles**

Run (from `backend/`): `cargo build`
Expected: builds successfully; `aes-gcm v0.10.x` appears in the dependency resolution / `Cargo.lock` is updated.

- [ ] **Step 3: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock
git commit -m "build: add aes-gcm dependency for at-rest secret encryption (#15)"
```

---

## Task 2: `SecretCipher` core — construct, encrypt, decrypt (TDD)

**Files:**
- Create: `backend/src/crypto.rs`
- Modify: `backend/src/main.rs` (declare the module so tests compile)

- [ ] **Step 1: Declare the module**

In `backend/src/main.rs`, add `mod crypto;` to the existing list of `mod` declarations (next to `mod db;`, `mod auth;`):

```rust
mod db;
mod auth;
mod crypto;
```

- [ ] **Step 2: Write `crypto.rs` with the failing tests first**

Create `backend/src/crypto.rs` with the full module below. It contains the implementation AND the tests; we write it in one file but will run the tests to confirm they pass after.

```rust
//! Application-layer encryption for the TOTP secret — the sole login
//! credential in this passwordless system. Secrets are AES-256-GCM encrypted
//! before they touch Postgres so a DB dump / backup yields no usable
//! credential. See docs/superpowers/specs/2026-06-11-encrypt-totp-secrets-design.md
//! and issue #15.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rand::RngCore;

/// Version marker prefixed to every stored ciphertext. Lets us recognise
/// encrypted values and gives us a rotation handle later (we only emit/accept
/// v1 today).
const PREFIX: &str = "encv1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Debug)]
pub struct CryptoError(pub String);

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crypto error: {}", self.0)
    }
}

impl std::error::Error for CryptoError {}

/// Symmetric cipher for stored secrets. Holds the AES-256-GCM key in memory;
/// the key itself is never written to the database.
#[derive(Clone)]
pub struct SecretCipher {
    cipher: Aes256Gcm,
}

impl SecretCipher {
    /// Build a cipher from raw key bytes. The key MUST be exactly 32 bytes.
    pub fn new(key_bytes: &[u8]) -> Result<Self, CryptoError> {
        if key_bytes.len() != KEY_LEN {
            return Err(CryptoError(format!(
                "key must be {} bytes, got {}",
                KEY_LEN,
                key_bytes.len()
            )));
        }
        let key = Key::<Aes256Gcm>::from_slice(key_bytes);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// Read `TOTP_ENC_KEY` (base64 of exactly 32 bytes) from the environment
    /// and build a cipher. Returns Err if the var is missing, not valid
    /// base64, or not 32 bytes — the caller (main) aborts startup on Err.
    pub fn from_env() -> Result<Self, CryptoError> {
        let encoded = std::env::var("TOTP_ENC_KEY")
            .map_err(|_| CryptoError("TOTP_ENC_KEY is not set".to_string()))?;
        let key_bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|e| CryptoError(format!("TOTP_ENC_KEY is not valid base64: {e}")))?;
        Self::new(&key_bytes)
    }

    /// Encrypt a plaintext secret. Output: `encv1:` + base64(nonce ‖ ct+tag).
    /// A fresh random 96-bit nonce is used per call.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError(format!("encryption failed: {e}")))?;

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(format!("{PREFIX}{}", STANDARD.encode(&blob)))
    }

    /// Decrypt a value produced by `encrypt`. Errors on wrong prefix/version,
    /// bad base64, truncated blob, or failed authentication (tamper / wrong key).
    pub fn decrypt(&self, stored: &str) -> Result<String, CryptoError> {
        let b64 = stored
            .strip_prefix(PREFIX)
            .ok_or_else(|| CryptoError("missing/unknown version prefix".to_string()))?;

        let blob = STANDARD
            .decode(b64)
            .map_err(|e| CryptoError(format!("invalid base64: {e}")))?;

        if blob.len() <= NONCE_LEN {
            return Err(CryptoError("ciphertext too short".to_string()));
        }

        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CryptoError("decryption/authentication failed".to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|e| CryptoError(format!("decrypted bytes are not utf8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed 32-byte test key. Never used outside tests.
    fn test_cipher() -> SecretCipher {
        SecretCipher::new(&[7u8; KEY_LEN]).unwrap()
    }

    #[test]
    fn round_trip_returns_original() {
        let c = test_cipher();
        let secret = "JBSWY3DPEHPK3PXP";
        let stored = c.encrypt(secret).unwrap();
        assert_eq!(c.decrypt(&stored).unwrap(), secret);
    }

    #[test]
    fn ciphertext_is_not_plaintext_and_has_prefix() {
        let c = test_cipher();
        let secret = "JBSWY3DPEHPK3PXP";
        let stored = c.encrypt(secret).unwrap();
        assert!(stored.starts_with("encv1:"));
        assert!(!stored.contains(secret));
    }

    #[test]
    fn same_input_encrypts_differently() {
        let c = test_cipher();
        let a = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        let b = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        assert_ne!(a, b, "nonce reuse: identical ciphertext for identical input");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let c = test_cipher();
        let stored = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        // Flip the last base64 char to corrupt the auth tag.
        let mut chars: Vec<char> = stored.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(c.decrypt(&tampered).is_err());
    }

    #[test]
    fn wrong_format_is_rejected() {
        let c = test_cipher();
        assert!(c.decrypt("JBSWY3DPEHPK3PXP").is_err()); // no prefix (legacy plaintext)
        assert!(c.decrypt("encv2:abc").is_err()); // unknown version prefix
        assert!(c.decrypt("encv1:not!base64!").is_err()); // bad base64
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let a = SecretCipher::new(&[1u8; KEY_LEN]).unwrap();
        let b = SecretCipher::new(&[2u8; KEY_LEN]).unwrap();
        let stored = a.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        assert!(b.decrypt(&stored).is_err());
    }

    #[test]
    fn key_must_be_32_bytes() {
        assert!(SecretCipher::new(&[0u8; 16]).is_err());
        assert!(SecretCipher::new(&[0u8; 31]).is_err());
        assert!(SecretCipher::new(&[0u8; 33]).is_err());
        assert!(SecretCipher::new(&[0u8; 32]).is_ok());
    }
}
```

- [ ] **Step 3: Run the tests**

Run (from `backend/`): `cargo test --lib crypto`
Expected: all 7 tests in `crypto::tests` PASS. (If `--lib` reports "no library targets", use `cargo test crypto` instead — this is a binary crate, so unit tests run via the bin target.)

- [ ] **Step 4: Lint clean**

Run (from `backend/`): `cargo clippy -- -D warnings`
Expected: no warnings from `crypto.rs`.

- [ ] **Step 5: Commit**

```bash
git add backend/src/crypto.rs backend/src/main.rs
git commit -m "feat: add SecretCipher (AES-256-GCM) for TOTP secrets (#15)"
```

---

## Task 3: Wire the cipher into `AppState` and fail fast at startup

**Files:**
- Modify: `backend/src/auth.rs:25-28` (`AppState`)
- Modify: `backend/src/main.rs` (build cipher, populate `AppState`)

- [ ] **Step 1: Add `cipher` to `AppState`**

In `backend/src/auth.rs`, add the `Arc` import near the top (with the other `use` lines):

```rust
use std::sync::Arc;
```

Then extend `AppState` (currently just `db`):

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cipher: Arc<crate::crypto::SecretCipher>,
}
```

- [ ] **Step 2: Build the cipher in `main.rs` (fail-fast)**

In `backend/src/main.rs`, add the import near the other `use` lines:

```rust
use std::sync::Arc;
```

In `main()`, immediately after the `let _ = dotenv();` line (step "2. Load Environment Variables"), add:

```rust
    // Load the AES key for at-rest TOTP-secret encryption. Refuse to start if
    // it is missing or malformed — never run with the ability to write
    // plaintext credentials. Generate one with: openssl rand -base64 32
    let cipher = Arc::new(crypto::SecretCipher::from_env().map_err(|e| {
        tracing::error!("Cannot start: {e}. Set TOTP_ENC_KEY (base64 of 32 random bytes).");
        e
    })?);
    tracing::info!("TOTP secret encryption enabled.");
```

- [ ] **Step 3: Populate `AppState` with the cipher**

In `backend/src/main.rs`, change the `AppState` construction (step "5. Build App State"):

```rust
    let state = AppState { db: pool, cipher };
```

- [ ] **Step 4: Verify it compiles**

Run (from `backend/`): `cargo build`
Expected: compiles. (`auth.rs` handlers don't use `cipher` yet — that's Task 4 — but the struct field and wiring compile.)

- [ ] **Step 5: Verify fail-fast behavior manually**

Run (from `backend/`), with the var explicitly unset:

```bash
env -u TOTP_ENC_KEY DATABASE_URL=postgres://invalid cargo run 2>&1 | head -5
```

Expected: process exits early with the log line `Cannot start: crypto error: TOTP_ENC_KEY is not set ...` (it must fail on the missing key before/independent of any DB connection error). Stop the process.

- [ ] **Step 6: Commit**

```bash
git add backend/src/main.rs backend/src/auth.rs
git commit -m "feat: load TOTP_ENC_KEY into AppState, fail fast if unset (#15)"
```

---

## Task 4: Encrypt on write / decrypt on read in `auth.rs`

**Files:**
- Modify: `backend/src/auth.rs` — `register_start`, `register_finish`, `login_start`, `login_finish`

No new tests here (crypto is unit-tested in Task 2; this task is verified by the compiler plus the manual end-to-end in Task 6). Each edit is small and mechanical.

- [ ] **Step 1: `register_start` — encrypt before inserting into `auth_flows`**

In `register_start` (around `backend/src/auth.rs:140-155`), after `let url = totp.get_url();` and before the `auth_flows` INSERT, add:

```rust
    let encrypted_secret = state
        .cipher
        .encrypt(&totp_secret_base32)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
```

Then change the INSERT's bind from the plaintext to the ciphertext — replace:

```rust
    .bind(&totp_secret_base32)
```

with:

```rust
    .bind(&encrypted_secret)
```

Leave the `RegisterStartResponse { flow_id, secret: totp_secret_base32, url }` untouched — the client still receives the plaintext secret to set up its authenticator.

- [ ] **Step 2: `register_finish` — decrypt to verify, re-encrypt for `users`**

In `register_finish`, the SELECT returns the (now encrypted) secret as `totp_secret_base32`. Rename the binding for clarity and decrypt it. Replace:

```rust
    let (user_id, email, totp_secret_base32) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };

    // Verify code
    let totp = build_totp(&totp_secret_base32, &email)?;
```

with:

```rust
    let (user_id, email, encrypted_secret) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };

    // Decrypt only in-memory to build/verify the TOTP.
    let totp_secret_base32 = state
        .cipher
        .decrypt(&encrypted_secret)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify code
    let totp = build_totp(&totp_secret_base32, &email)?;
```

Then, before the `INSERT INTO users` query, add a fresh encryption (new nonce) for the persisted user row:

```rust
    let user_secret_encrypted = state
        .cipher
        .encrypt(&totp_secret_base32)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
```

And change the users INSERT bind — replace:

```rust
    .bind(&totp_secret_base32)
    .fetch_one(&mut *tx)
```

with:

```rust
    .bind(&user_secret_encrypted)
    .fetch_one(&mut *tx)
```

- [ ] **Step 3: `login_start` — copy the already-encrypted secret unchanged**

In `login_start`, `user.totp_secret` is already ciphertext (read from `users`). The INSERT into `auth_flows` binds `&user.totp_secret` — this is already correct (it stores the existing ciphertext). **No change needed.** Confirm by reading the handler that it binds `&user.totp_secret` and does not call `build_totp`.

- [ ] **Step 4: `login_finish` — decrypt before verifying**

In `login_finish`, mirror Step 2's read side. Replace:

```rust
    let (user_id, email, totp_secret_base32) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };

    // Verify code
    let totp = build_totp(&totp_secret_base32, &email)?;
```

with:

```rust
    let (user_id, email, encrypted_secret) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired flow ID".to_string())),
    };

    // Decrypt only in-memory to build/verify the TOTP.
    let totp_secret_base32 = state
        .cipher
        .decrypt(&encrypted_secret)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Verify code
    let totp = build_totp(&totp_secret_base32, &email)?;
```

- [ ] **Step 5: Build and lint**

Run (from `backend/`): `cargo build && cargo clippy -- -D warnings`
Expected: compiles with no warnings. Watch for an "unused variable" warning on any leftover binding — if one appears, you missed a rename above.

- [ ] **Step 6: Commit**

```bash
git add backend/src/auth.rs
git commit -m "feat: encrypt totp_secret on write, decrypt on read in auth flows (#15)"
```

---

## Task 5: Document `TOTP_ENC_KEY`

**Files:**
- Modify: `.env.example`, `backend/.env.example`, `README.md`, `AGENTS.md`, `backend/fly.toml`

- [ ] **Step 1: Update both `.env.example` files**

Append this block to BOTH `/.env.example` and `backend/.env.example` (identical content):

```bash

# Base64-encoded 32-byte key used to encrypt TOTP secrets at rest (AES-256-GCM).
# REQUIRED — the backend refuses to start without it. Generate one with:
#   openssl rand -base64 32
TOTP_ENC_KEY=
```

- [ ] **Step 2: Document it in `README.md` and `AGENTS.md`**

In each file, find the section that lists backend environment variables (search for `DATABASE_URL` and `GEMINI_API_KEY`). Add an entry for `TOTP_ENC_KEY` alongside them, matching the surrounding format. The entry must convey: base64 of 32 random bytes, AES-256-GCM encryption of TOTP secrets at rest, **required (backend won't start without it)**, generate with `openssl rand -base64 32`, set in production via `fly secrets set TOTP_ENC_KEY=... -a nels-api`.

(If a file has no env-var section, add a short "### Environment variables" note near the backend/run instructions rather than inventing a new structure.)

- [ ] **Step 3: Add a `fly.toml` comment**

In `backend/fly.toml`, update the existing comment line:

```toml
# DATABASE_URL and GEMINI_API_KEY are set via `fly secrets set`, not here.
```

to:

```toml
# DATABASE_URL, GEMINI_API_KEY, and TOTP_ENC_KEY are set via `fly secrets set`,
# not here. TOTP_ENC_KEY is required (base64 of 32 bytes: openssl rand -base64 32);
# the backend refuses to start without it.
```

- [ ] **Step 4: Commit**

```bash
git add .env.example backend/.env.example README.md AGENTS.md backend/fly.toml
git commit -m "docs: document required TOTP_ENC_KEY env var (#15)"
```

---

## Task 6: End-to-end verification

**Files:** none (verification only)

This needs a local Postgres (the dev `DATABASE_URL` default points at `127.0.0.1:6153/budget_rag`). If the project has a compose/db helper, use it; otherwise point `DATABASE_URL` at any reachable dev Postgres with pgvector.

- [ ] **Step 1: Generate a dev key and start the backend**

```bash
export TOTP_ENC_KEY=$(openssl rand -base64 32)
cd backend && cargo run
```

Expected: logs `TOTP secret encryption enabled.` and `Database migrations completed successfully.`, then serves.

- [ ] **Step 2: Register a user end-to-end**

Using the frontend (or `curl` against `/auth/register/start` then `/auth/register/finish` with a TOTP code generated from the returned `secret`), complete a registration. Expected: registration succeeds and returns a session token.

- [ ] **Step 3: Confirm the stored secret is ciphertext**

Query the DB directly:

```bash
psql "$DATABASE_URL" -c "SELECT email, left(totp_secret, 12) AS secret_prefix FROM users ORDER BY created_at DESC LIMIT 1;"
```

Expected: `secret_prefix` is `encv1:` followed by base64 — NOT a 16/26-char base32 string. This satisfies the issue's verification checkbox "`totp_secret` is not readable as plaintext from a raw DB dump".

- [ ] **Step 4: Log in as the same user**

Complete a login flow (`/auth/login/start` → `/auth/login/finish`) with a fresh TOTP code. Expected: login succeeds and returns a session token. This satisfies "Login/enrollment still works end-to-end with encrypted-at-rest secrets".

- [ ] **Step 5: Final full check**

Run (from `backend/`): `cargo test && cargo clippy -- -D warnings && cargo build --release`
Expected: all tests pass, no clippy warnings, release build succeeds.

- [ ] **Step 6: Push the branch and open the PR**

Hand off to git-expert (per the user's global instruction to use git-expert for git/GitHub) to push `feat/encrypt-totp-secrets` and open a PR against `main` that closes #15. The PR body should note the operational follow-up: **`fly secrets set TOTP_ENC_KEY=$(openssl rand -base64 32) -a nels-api` must be run before deploying**, or the backend will refuse to start.

---

## Spec coverage check

- Encrypt `totp_secret` at app layer (AES-256-GCM, env key, not in DB) → Tasks 2–4. ✓
- Versioned, rotatable format → `encv1:` prefix (Task 2). ✓
- `auth_flows.totp_secret` gets the same protection → Task 4 Steps 1, 3. ✓ (Existing hourly purge job already covers expired flow rows — verified in `purge_expired_auth_state`, no change needed.)
- Fail-fast without key → Task 3. ✓
- Verification: ciphertext in DB dump, login/enrollment works → Task 6. ✓
- Privacy-page wording → intentionally out of scope (separate branch); noted in spec. ✓
