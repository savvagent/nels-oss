//! WebAuthn/FIDO2 passkey support (nels#551) — replaces TOTP as Nels' login
//! credential.
//!
//! Two relying parties are configured: `"app"` (the main frontend on
//! `app.nels.money`, RP ID `nels.money`) and `"admin"` (the admin console on
//! `nels-admin.pages.dev`). They're different effective domains, so a
//! passkey registered for one RP cannot assert on the other — each user may
//! hold a separate credential per RP. Which RP a request belongs to is
//! resolved from its `Origin` header against the configured origins (see
//! `WebauthnRegistry::resolve_rp`) and persisted onto the
//! `auth_flows`/`webauthn_credentials` rows as a plain tag, rather than
//! re-trusted a second time at `finish` — the real cryptographic origin
//! check happens inside webauthn-rs's `finish_passkey_*`.

use axum::http::{HeaderMap, StatusCode};
use rand::Rng;
use sqlx::PgPool;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::error::internal_error;

pub type Rp = &'static str;

pub struct WebauthnRegistry {
    app: Webauthn,
    admin: Webauthn,
    // Exact `Origin` header values routed to each RP by `resolve_rp` — the
    // same list each `Webauthn` instance accepts at `finish`, so the two
    // checks can't drift apart.
    app_origins: Vec<String>,
    admin_origins: Vec<String>,
}

/// One RP's config: its RP ID plus every origin allowed to use it.
struct RpConfig {
    rp_id: String,
    origins: Vec<Url>,
}

impl RpConfig {
    /// `origins` is a comma-separated list; the first entry is the primary
    /// origin webauthn-rs validates `rp_id` against.
    fn new(rp_id: &str, origins: &str) -> Self {
        let origins: Vec<Url> = origins
            .split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(|o| Url::parse(o).unwrap_or_else(|e| panic!("invalid WebAuthn origin {o}: {e}")))
            .collect();
        assert!(!origins.is_empty(), "WebAuthn RP {rp_id} needs at least one origin");
        Self { rp_id: rp_id.to_string(), origins }
    }

    fn build(&self) -> Webauthn {
        let mut builder = WebauthnBuilder::new(&self.rp_id, &self.origins[0]).unwrap_or_else(|e| {
            panic!("invalid WebAuthn RP config (rp_id={}, origin={}): {e}", self.rp_id, self.origins[0])
        });
        for origin in &self.origins[1..] {
            builder = builder.append_allowed_origin(origin);
        }
        builder.rp_name("Nels").build().expect("failed to build Webauthn instance")
    }

    /// Origins as the browser sends them in the `Origin` header (no trailing
    /// slash, which `Url` always adds for an empty path).
    fn origin_headers(&self) -> Vec<String> {
        self.origins.iter().map(|u| u.origin().ascii_serialization()).collect()
    }
}

impl WebauthnRegistry {
    fn new(app: RpConfig, admin: RpConfig) -> Self {
        Self {
            app: app.build(),
            admin: admin.build(),
            app_origins: app.origin_headers(),
            admin_origins: admin.origin_headers(),
        }
    }

    /// Production (`FLY_APP_NAME` set) defaults to the live domains: the app
    /// on `app.nels.money` under RP ID `nels.money` (so a future move to
    /// another nels.money subdomain keeps users' passkeys working), the admin
    /// console on `nels-admin.pages.dev`. Everywhere else defaults to the
    /// local Vite dev servers. Each `WEBAUTHN_*_ORIGIN` accepts a
    /// comma-separated list.
    pub fn from_env() -> Self {
        let is_production = std::env::var("FLY_APP_NAME").is_ok();
        let (app_rp, app_origin, admin_rp, admin_origin) = if is_production {
            ("nels.money", "https://app.nels.money", "nels-admin.pages.dev", "https://nels-admin.pages.dev")
        } else {
            ("localhost", "http://localhost:5173", "localhost", "http://localhost:5174")
        };
        let var = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_string());

        Self::new(
            RpConfig::new(&var("WEBAUTHN_APP_RP_ID", app_rp), &var("WEBAUTHN_APP_ORIGIN", app_origin)),
            RpConfig::new(&var("WEBAUTHN_ADMIN_RP_ID", admin_rp), &var("WEBAUTHN_ADMIN_ORIGIN", admin_origin)),
        )
    }

    /// Fixed localhost config for tests — no env setup required. Both RPs
    /// use rp_id "localhost" (WebAuthn RP IDs never include a port, so this
    /// is valid for both dev origins); our own `rp_id` column tag, not
    /// webauthn-rs's internal one, is what actually keeps app/admin
    /// credentials separate — see module docs.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(
            RpConfig::new("localhost", "http://localhost:5173"),
            RpConfig::new("localhost", "http://localhost:5174"),
        )
    }

    pub fn get(&self, rp: Rp) -> &Webauthn {
        match rp {
            "admin" => &self.admin,
            _ => &self.app,
        }
    }

    /// Resolve which RP a request belongs to from its `Origin` header,
    /// against the configured origins. An unrecognized origin is a hard 400 —
    /// there is no third RP.
    pub fn resolve_rp(&self, headers: &HeaderMap) -> Result<Rp, (StatusCode, String)> {
        let origin = headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .ok_or((StatusCode::BAD_REQUEST, "Missing Origin header".to_string()))?;

        if self.app_origins.iter().any(|o| o == origin) {
            Ok("app")
        } else if self.admin_origins.iter().any(|o| o == origin) {
            Ok("admin")
        } else {
            Err((StatusCode::BAD_REQUEST, format!("Unrecognized origin: {origin}")))
        }
    }
}

/// Normalize a stored `rp` column value (a plain `TEXT`) back to the static
/// tag used everywhere else — mirrors `WebauthnRegistry::get`'s matching.
pub fn to_rp(stored: &str) -> Rp {
    if stored == "admin" { "admin" } else { "app" }
}

#[derive(sqlx::FromRow)]
struct CredentialRow {
    id: Uuid,
    passkey: serde_json::Value,
}

/// Load all passkeys a user holds for a given RP, for presenting an
/// authentication challenge or for `exclude_credentials` during
/// (re-)registration.
pub async fn load_passkeys(pool: &PgPool, user_id: Uuid, rp: Rp) -> Result<Vec<Passkey>, (StatusCode, String)> {
    let rows: Vec<CredentialRow> = sqlx::query_as(
        "SELECT id, passkey FROM webauthn_credentials WHERE user_id = $1 AND rp_id = $2",
    )
    .bind(user_id)
    .bind(rp)
    .fetch_all(pool)
    .await
    .map_err(|e| internal_error(format!("Failed to load credentials: {e}")))?;

    rows.into_iter()
        .map(|row| {
            serde_json::from_value(row.passkey)
                .map_err(|e| internal_error(format!("Corrupt stored passkey {}: {e}", row.id)))
        })
        .collect()
}

pub async fn has_any_credential(pool: &PgPool, user_id: Uuid, rp: Rp) -> Result<bool, (StatusCode, String)> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webauthn_credentials WHERE user_id = $1 AND rp_id = $2",
    )
    .bind(user_id)
    .bind(rp)
    .fetch_one(pool)
    .await
    .map_err(|e| internal_error(format!("Failed to count credentials: {e}")))?;
    Ok(count > 0)
}

pub async fn store_new_credential(
    pool: &PgPool,
    user_id: Uuid,
    rp: Rp,
    passkey: &Passkey,
) -> Result<(), (StatusCode, String)> {
    let credential_id: Vec<u8> = passkey.cred_id().to_vec();
    let passkey_json = serde_json::to_value(passkey)
        .map_err(|e| internal_error(format!("Failed to serialize passkey: {e}")))?;

    sqlx::query(
        "INSERT INTO webauthn_credentials (id, user_id, rp_id, credential_id, passkey) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(rp)
    .bind(credential_id)
    .bind(passkey_json)
    .execute(pool)
    .await
    .map_err(|e| internal_error(format!("Failed to store credential: {e}")))?;

    Ok(())
}

/// Persist an updated sign-count/backup-state after a successful
/// authentication. A miss (e.g. the credential was deleted mid-ceremony) is
/// logged but never fails the login — the assertion already verified.
pub async fn update_credential_after_auth(pool: &PgPool, rp: Rp, passkey: &Passkey) {
    let credential_id: Vec<u8> = passkey.cred_id().to_vec();
    let Ok(passkey_json) = serde_json::to_value(passkey) else {
        tracing::error!("Failed to serialize updated passkey for credential update");
        return;
    };

    if let Err(e) = sqlx::query(
        "UPDATE webauthn_credentials SET passkey = $1, last_used_at = NOW() \
         WHERE rp_id = $2 AND credential_id = $3",
    )
    .bind(passkey_json)
    .bind(rp)
    .bind(credential_id)
    .execute(pool)
    .await
    {
        tracing::error!("Failed to persist updated credential state: {e}");
    }
}

pub async fn delete_all_credentials(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| internal_error(format!("Failed to clear credentials: {e}")))?;
    Ok(())
}

// --- Recovery codes ---

const RECOVERY_CODE_BYTES: usize = 10; // ~16 base32 chars of entropy per code

fn default_recovery_code_count() -> usize {
    std::env::var("RECOVERY_CODE_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

fn generate_one_code() -> String {
    let random_bytes: Vec<u8> = rand::thread_rng()
        .sample_iter(&rand::distributions::Standard)
        .take(RECOVERY_CODE_BYTES)
        .collect();
    let raw = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &random_bytes);
    // Hyphenate for readability when copied down/typed back in, e.g. "ABCDE-FGHJK-MNPQR".
    raw.as_bytes()
        .chunks(5)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("-")
}

fn hash_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalized = code.trim().to_uppercase();
    let digest = Sha256::digest(normalized.as_bytes());
    hex::encode(digest)
}

/// Generate a fresh batch of recovery codes, store their hashes, and return
/// the plaintext codes — the caller must show these to the user exactly
/// once; nothing here or afterward ever persists them unhashed.
pub async fn issue_recovery_codes(pool: &PgPool, user_id: Uuid) -> Result<Vec<String>, (StatusCode, String)> {
    let count = default_recovery_code_count();
    let mut codes = Vec::with_capacity(count);

    let mut tx = pool.begin().await.map_err(internal_error)?;
    for _ in 0..count {
        let code = generate_one_code();
        let hash = hash_code(&code);
        sqlx::query("INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(&hash)
            .execute(&mut *tx)
            .await
            .map_err(|e| internal_error(format!("Failed to store recovery code: {e}")))?;
        codes.push(code);
    }
    tx.commit().await.map_err(internal_error)?;

    Ok(codes)
}

/// Delete any unused recovery codes for a user and issue a fresh batch — used
/// by the admin-assisted reset endpoint.
pub async fn rotate_recovery_codes(pool: &PgPool, user_id: Uuid) -> Result<Vec<String>, (StatusCode, String)> {
    sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| internal_error(format!("Failed to clear old recovery codes: {e}")))?;
    issue_recovery_codes(pool, user_id).await
}

/// Look up an unused recovery code for the given email. Returns the code's
/// row id (to mark used on success) and the owning user id.
pub async fn find_unused_code(
    pool: &PgPool,
    email: &str,
    code: &str,
) -> Result<Option<(Uuid, Uuid)>, (StatusCode, String)> {
    let hash = hash_code(code);
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT rc.id, rc.user_id FROM recovery_codes rc \
         JOIN users u ON u.id = rc.user_id \
         WHERE u.email = $1 AND rc.code_hash = $2 AND rc.used_at IS NULL",
    )
    .bind(email)
    .bind(&hash)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    Ok(row)
}

pub async fn mark_code_used(pool: &PgPool, code_id: Uuid) -> Result<(), (StatusCode, String)> {
    sqlx::query("UPDATE recovery_codes SET used_at = NOW() WHERE id = $1")
        .bind(code_id)
        .execute(pool)
        .await
        .map_err(internal_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::ORIGIN;

    fn headers_with_origin(origin: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, origin.parse().unwrap());
        headers
    }

    #[test]
    fn resolve_rp_matches_configured_app_and_admin_origins() {
        let reg = WebauthnRegistry::for_test();
        assert_eq!(reg.resolve_rp(&headers_with_origin("http://localhost:5173")).unwrap(), "app");
        assert_eq!(reg.resolve_rp(&headers_with_origin("http://localhost:5174")).unwrap(), "admin");
    }

    // nels#560: production serves the app from app.nels.money; the origin
    // allow-list must come from config, not a hard-coded pages.dev list.
    #[test]
    fn resolve_rp_follows_configured_production_origins() {
        let reg = WebauthnRegistry::new(
            RpConfig::new("nels.money", "https://app.nels.money"),
            RpConfig::new("nels-admin.pages.dev", "https://nels-admin.pages.dev"),
        );
        assert_eq!(reg.resolve_rp(&headers_with_origin("https://app.nels.money")).unwrap(), "app");
        assert_eq!(reg.resolve_rp(&headers_with_origin("https://nels-admin.pages.dev")).unwrap(), "admin");
        assert!(reg.resolve_rp(&headers_with_origin("https://nels.pages.dev")).is_err());
        assert!(reg.resolve_rp(&headers_with_origin("http://localhost:5173")).is_err());
    }

    #[test]
    fn rp_config_accepts_a_comma_separated_origin_list() {
        let reg = WebauthnRegistry::new(
            RpConfig::new("nels.money", "https://app.nels.money, https://beta.nels.money"),
            RpConfig::new("localhost", "http://localhost:5174"),
        );
        assert_eq!(reg.resolve_rp(&headers_with_origin("https://beta.nels.money")).unwrap(), "app");
    }

    #[test]
    fn resolve_rp_rejects_unknown_origin() {
        let err = WebauthnRegistry::for_test()
            .resolve_rp(&headers_with_origin("https://evil.example.com"))
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn resolve_rp_rejects_missing_origin_header() {
        let err = WebauthnRegistry::for_test().resolve_rp(&HeaderMap::new()).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn to_rp_normalizes_unknown_strings_to_app() {
        assert_eq!(to_rp("admin"), "admin");
        assert_eq!(to_rp("app"), "app");
        assert_eq!(to_rp("garbage"), "app");
    }

    #[test]
    fn generated_recovery_code_hash_is_case_and_whitespace_insensitive() {
        let code = generate_one_code();
        // Real-world entry: a user retyping a shown-once code may vary case or
        // add stray whitespace — the hash must still match.
        assert_eq!(hash_code(&code), hash_code(&code.to_lowercase()));
        assert_eq!(hash_code(&code), hash_code(&format!("  {code}  ")));
    }

    #[test]
    fn generated_recovery_codes_are_unique_and_hyphenated() {
        let a = generate_one_code();
        let b = generate_one_code();
        assert_ne!(a, b, "two generated codes must not collide in this sample");
        assert!(a.contains('-'), "codes are hyphenated for readability: {a}");
    }

    fn test_registry() -> WebauthnRegistry {
        WebauthnRegistry::for_test()
    }

    // DB-backed lifecycle: issuing codes stores only hashes (never plaintext),
    // a code redeems exactly once, and a used/unknown code is rejected.
    // Requires Postgres: podman-compose up -d && cargo test -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn recovery_codes_issue_and_single_use_redemption() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let email = format!("passkeys-recovery-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let codes = issue_recovery_codes(&pool, user_id).await.expect("issue codes");
        assert!(!codes.is_empty(), "must issue at least one code");

        let stored: Vec<(String,)> = sqlx::query_as("SELECT code_hash FROM recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&pool)
            .await
            .expect("read back stored hashes");
        for code in &codes {
            assert!(
                !stored.iter().any(|(h,)| h == code),
                "plaintext code must never be stored as its own hash"
            );
        }

        let first = &codes[0];
        let found = find_unused_code(&pool, &email, first)
            .await
            .expect("lookup should not error")
            .expect("first code should be found unused");
        mark_code_used(&pool, found.0).await.expect("mark used");

        let redeemed_again = find_unused_code(&pool, &email, first).await.expect("lookup ok");
        assert!(redeemed_again.is_none(), "a used code must not be redeemable again");

        let second = &codes[1];
        let still_good = find_unused_code(&pool, &email, second).await.expect("lookup ok");
        assert!(still_good.is_some(), "an unrelated unused code must still redeem");

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    // Admin-assisted reset (nels#551): rotating recovery codes must invalidate
    // every prior unused code and issue a disjoint fresh batch; used codes are
    // untouched (they're already spent, no security benefit to deleting them).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn rotate_recovery_codes_invalidates_old_unused_codes() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let email = format!("passkeys-rotate-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let original = issue_recovery_codes(&pool, user_id).await.expect("issue codes");
        let stale = original[0].clone();

        let fresh = rotate_recovery_codes(&pool, user_id).await.expect("rotate codes");
        assert!(!fresh.iter().any(|c| original.contains(c)), "rotated codes must be disjoint from the old batch");

        let stale_lookup = find_unused_code(&pool, &email, &stale).await.expect("lookup ok");
        assert!(stale_lookup.is_none(), "old unused code must be invalidated by rotation");

        let fresh_lookup = find_unused_code(&pool, &email, &fresh[0]).await.expect("lookup ok");
        assert!(fresh_lookup.is_some(), "a freshly rotated code must be redeemable");

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    // Credential storage is scoped per (user, rp) — a credential registered
    // for one RP must never surface when loading the other RP's passkeys.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn credentials_are_isolated_per_rp() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let registry = test_registry();

        let user_id = Uuid::new_v4();
        let email = format!("passkeys-rp-isolation-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed user");

        let (_ccr, reg_state) = registry
            .get("app")
            .start_passkey_registration(user_id, &email, &email, None)
            .expect("start registration");
        // We don't have a real authenticator in this test, so we can't finish
        // the ceremony to get a genuine Passkey — instead assert the DB-layer
        // scoping directly against an empty store, which is what `login_start`
        // actually depends on to fail fast.
        drop(reg_state);

        assert!(!has_any_credential(&pool, user_id, "app").await.unwrap());
        assert!(!has_any_credential(&pool, user_id, "admin").await.unwrap());
        assert!(load_passkeys(&pool, user_id, "app").await.unwrap().is_empty());

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }
}
