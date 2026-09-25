# Encrypt TOTP secrets at rest

> **Superseded:** login no longer uses TOTP — see
> [savvagent/nels#551](https://github.com/savvagent/nels/issues/551) (WebAuthn
> passkey conversion). `crypto.rs`'s `SecretCipher` described below now
> encrypts bank-provider access tokens instead; kept here as historical record
> of the original design.

**Issue:** [savvagent/nels#15](https://github.com/savvagent/nels/issues/15)
**Date:** 2026-06-11
**Status:** Approved

## Problem

`totp_secret` is the sole login credential in this passwordless, TOTP-only auth
system — it is password-equivalent, not a second factor. It is currently stored
as plaintext base32 in two Postgres columns:

- `users.totp_secret` (`backend/migrations/20260609000000_init.sql`)
- `auth_flows.totp_secret` (`backend/migrations/20260609010000_auth_persistence.sql`)

Anyone with logical read access to the database or a backup can derive valid
TOTP codes and authenticate as any user (CWE-256 / CWE-312). Host/volume-level
disk encryption does not mitigate this: the application reads plaintext, so any
logical DB access yields working credentials.

## Goal

Encrypt `totp_secret` at the application layer (AES-256-GCM, key from env / Fly
secret, never in the DB) so a raw DB dump or backup contains no usable
credential. Login and enrollment continue to work end-to-end.

## Decisions

| Decision | Choice |
|---|---|
| Existing data | **Greenfield** — no production rows to backfill. New writes are encrypted; reads are strict (decrypt-only, no plaintext fallback). |
| Key rotation | **Versioned format, one active key.** A version marker (`encv1:`) is embedded so rotation is possible later; only one active key today. |
| Missing key | **Fail fast everywhere.** Backend refuses to start without a valid `TOTP_ENC_KEY` in all environments. |
| Privacy page | **Out of scope for this PR.** The marketing privacy page lives on the unmerged `feat/marketing-site` branch and is handled separately. |
| Library | `aes-gcm` 0.10 (RustCrypto, pure Rust, pairs with the existing rustls stack). |

## Architecture

### New module: `backend/src/crypto.rs`

A `SecretCipher` value type wrapping an `Aes256Gcm` cipher.

```
pub struct SecretCipher { /* holds Aes256Gcm */ }

impl SecretCipher {
    // Reads TOTP_ENC_KEY (base64 of exactly 32 bytes), builds the cipher.
    // Missing or malformed key -> Err (propagates out of main, aborts startup).
    pub fn from_env() -> Result<SecretCipher, CryptoError>;

    // Fresh random 12-byte nonce per call. Returns:
    //   "encv1:" + base64( nonce(12) || ciphertext+tag )
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError>;

    // Validates "encv1:" prefix + version, splits nonce/ciphertext,
    // authenticates and decrypts. Tampering / wrong key -> Err.
    pub fn decrypt(&self, stored: &str) -> Result<String, CryptoError>;
}
```

**Stored format:** `encv1:` + standard-base64 of `nonce (12 bytes) || ciphertext`
(the `aes-gcm` crate appends the 16-byte GCM auth tag to the ciphertext). The
`encv1:` prefix is the version marker and makes encrypted values greppable and
distinguishable from any stray plaintext.

**Size:** plaintext base32 secret is ~26 chars; encrypted blob is
`12 + 26 + 16 = 54` bytes → base64 ≈ 72 chars, plus the 6-char prefix ≈ 78 chars.
Comfortably within the existing `VARCHAR(255)` columns — **no DB migration
required**.

**Nonce generation:** random 12 bytes from `rand::thread_rng()` (already a
dependency). A random 96-bit nonce per encryption is the standard GCM construction;
collision risk is negligible at this volume.

### State wiring

- `AppState` gains `cipher: Arc<SecretCipher>` (so the `Clone` derive stays cheap).
- `main.rs` builds the cipher immediately after `dotenv()`:
  `let cipher = Arc::new(SecretCipher::from_env()?);`
  The `?` on `main`'s `Result<(), Box<dyn Error>>` aborts startup with a clear
  error if the key is missing/invalid.

### `auth.rs` changes

| Handler | Change |
|---|---|
| `register_start` | Encrypt the base32 secret before inserting into `auth_flows`. Still returns **plaintext** `secret` + `url` to the client (the QR / manual-entry code must stay plaintext). |
| `register_finish` | Decrypt from `auth_flows` to verify the code, then store a **freshly-encrypted** value (new nonce) in `users`. |
| `login_start` | `user.totp_secret` is already ciphertext; copy it straight into `auth_flows` — no decrypt/re-encrypt needed. |
| `login_finish` | Decrypt from `auth_flows`, then verify the code. |
| `build_totp` | Unchanged — still receives plaintext base32. |

Decryption failures return `500` with a generic message (no detail leaked).

### `db.rs`

`User.totp_secret` stays `String` (now holds ciphertext) — no struct change.

## Error handling

- **Startup:** missing or non-32-byte `TOTP_ENC_KEY` → `from_env()` returns `Err`,
  `main()` exits non-zero with a clear message. Never starts with the ability to
  write plaintext.
- **Encrypt failure:** `500`, generic message.
- **Decrypt failure** (corruption, wrong key, tamper): `500`, generic message —
  do not echo crypto internals to the client.

## Testing

### Unit tests (in `crypto.rs`, TDD)

1. Round-trip: `decrypt(encrypt(x)) == x`.
2. Ciphertext ≠ plaintext and carries the `encv1:` prefix.
3. Same input encrypted twice → different outputs (nonce uniqueness).
4. Tampered ciphertext → `decrypt` errors (auth tag rejects it).
5. Wrong-format / wrong-version input → `decrypt` errors.
6. Constructing a cipher from a key that is not 32 bytes → errors.

(`from_env`'s env-var reading is exercised via a helper that takes raw key bytes,
to keep tests free of process-global env mutation.)

### Manual end-to-end

- Register a new user → confirm `users.totp_secret` in the DB is `encv1:…`
  ciphertext, not base32.
- Log in as that user → succeeds.
- Confirm a raw `pg_dump` / `SELECT totp_secret` shows only ciphertext.

## Configuration & docs

- Add `aes-gcm = "0.10"` to `backend/Cargo.toml`.
- Document `TOTP_ENC_KEY` in `.env.example`, `backend/.env.example`, the backend
  env section of `README`/`AGENTS.md`, and a `fly.toml` comment.
  Generation: `openssl rand -base64 32`.
- Operational step (run by the maintainer against the Fly account):
  `fly secrets set TOTP_ENC_KEY=<base64-32-bytes> -a nels-api`.

## Out of scope

- Backfilling/migrating existing plaintext rows (greenfield).
- Multi-key rotation tooling (format is versioned so it can be added later).
- Privacy-page wording (separate branch / PR — issue item 4).
- Encrypting other columns or addressing info-disclosure paths (issue #12).
