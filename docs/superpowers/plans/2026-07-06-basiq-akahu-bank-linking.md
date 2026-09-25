# Basiq (Australia) + Akahu (New Zealand) Bank Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Pro subscribers in Australia link via Basiq (CDR-based hosted consent) and subscribers in New Zealand link via Akahu (its own OAuth2 hosted consent), importing transactions the same way #303 (Stripe)/#320 (GoCardless) already do — on top of the real `Provider` enum / `bank_linking.rs` dispatch layer #320 introduced.

**Architecture:** Two independent flat modules — `backend/src/basiq.rs` and `backend/src/akahu.rs` — each mirroring `gocardless.rs`'s "own HTTP client, own env test seam, own `require_pro`" convention. Both are hosted-redirect consent flows (no institution picker needed on our side, unlike GoCardless), reusing #320's `bank_link_sessions` table (generalized with a `provider` column) and #320's `bank_linking.rs` dispatch layer (`match` arms added, no new abstraction). Auto-refresh is webhook-driven for both (no poll job, unlike GoCardless which has no webhook at all).

**Tech Stack:** Rust (Axum, SQLx/Postgres, reqwest, hmac+sha2 for webhook signatures), Basiq REST API (CDR consent, hosted "Basiq Connect"), Akahu REST API (OAuth2, hosted "Akahu Connect"), Svelte 5. Full spec: `docs/superpowers/specs/2026-07-06-basiq-akahu-bank-linking-design.md`.

**Prerequisite:** This plan is built on top of `origin/issue-320-gocardless-bank-account-data`, which already ships the `Provider` enum (`bank_provider.rs`), the `bank_linking.rs` dispatch layer, `linked_accounts.provider`/`provider_account_id`, and `transactions.currency`. Verify `backend/src/bank_provider.rs`, `backend/src/bank_linking.rs`, and `backend/src/gocardless.rs` exist before starting — if they don't, the wrong base branch was used.

---

## File Structure

- Modify: `backend/src/bank_provider.rs` — add `Basiq`/`Akahu` variants.
- Create: `backend/migrations/20260707000000_basiq_akahu_bank_linking.sql` — generalize `bank_link_sessions`, extend `linked_accounts.provider` CHECK.
- Create: `backend/src/basiq.rs` — Basiq integration (Australia).
- Create: `backend/src/akahu.rs` — Akahu integration (New Zealand).
- Modify: `backend/src/bank_linking.rs` — dispatch arms + REST handlers for both providers.
- Modify: `backend/src/main.rs` — `mod basiq; mod akahu;`, routes, webhook routes.
- Modify: `backend/src/rag.rs` — `LINK_BANK_ACCOUNT` arms, system prompt, `offline_country_hint`.
- Modify: `backend/.env.example` — new env vars.
- Modify: `frontend/src/lib/linkedAccounts.js` — Basiq/Akahu thin API wrappers.
- Modify: `frontend/src/lib/LinkedAccounts.svelte` — country picker gains AU/NZ.
- Modify: `frontend/src/App.svelte` — `handleBasiqQueryParams`/`handleAkahuQueryParams`.
- Modify: `frontend/src/lib/linkedAccounts.test.js` — new pure-function tests.
- Modify (i18n): `frontend/src/lib/i18n/en.json` (or wherever `linkedAccounts.*` strings live — locate via `grep -rn "linkedAccounts.connectViaGoCardless"` before editing) — no NEW copy needed since the picker reuses `linkedAccounts.link`/`connectViaGoCardless`-style existing strings; verify no missing key surfaces as a raw key at Task 6.

---

## Task 1: Migration + `Provider` enum extension

**Files:**
- Create: `backend/migrations/20260707000000_basiq_akahu_bank_linking.sql`
- Modify: `backend/src/bank_provider.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// backend/src/bank_provider.rs — ADD to the existing #[cfg(test)] mod tests block
#[test]
fn for_country_maps_au_to_basiq() {
    assert_eq!(Provider::for_country("AU"), Some(Provider::Basiq));
}

#[test]
fn for_country_maps_nz_to_akahu() {
    assert_eq!(Provider::for_country("NZ"), Some(Provider::Akahu));
}

#[test]
fn for_country_au_nz_case_insensitive() {
    assert_eq!(Provider::for_country("au"), Some(Provider::Basiq));
    assert_eq!(Provider::for_country("nz"), Some(Provider::Akahu));
}

#[test]
fn supported_countries_lists_all_eleven() {
    assert_eq!(Provider::supported_countries().len(), 11);
}

#[test]
fn as_str_round_trips_basiq_and_akahu() {
    for p in [Provider::Basiq, Provider::Akahu] {
        assert_eq!(Provider::parse(p.as_str()), Some(p));
    }
}
```

Also update the existing `supported_countries_lists_all_nine` test's expected count to match the new plural rename below (it will be replaced by `supported_countries_lists_all_eleven` above — delete the old one, don't leave both asserting different counts).

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test bank_provider`
Expected: FAIL — `Provider::Basiq`/`Provider::Akahu` don't exist yet (compile error).

- [ ] **Step 3: Implement the enum additions**

```rust
// backend/src/bank_provider.rs — replace the whole file
//! Shared bank-provider abstraction (nels#320, extended nels#323): a closed,
//! four-member set (Stripe Financial Connections, GoCardless Bank Account
//! Data, Basiq, Akahu), so enum dispatch is used instead of an
//! `async-trait`+`dyn` object — see the spec's Assumption 8 (nels#320) and
//! Assumption 14 (nels#323) for the tradeoff rationale, including why four
//! variants still doesn't warrant revisiting the choice.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    GoCardless,
    Basiq,
    Akahu,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::GoCardless => "gocardless",
            Provider::Basiq => "basiq",
            Provider::Akahu => "akahu",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "stripe" => Some(Provider::Stripe),
            "gocardless" => Some(Provider::GoCardless),
            "basiq" => Some(Provider::Basiq),
            "akahu" => Some(Provider::Akahu),
            _ => None,
        }
    }

    /// Map an ISO 3166-1 alpha-2 country code (case-insensitive) to the
    /// provider that covers it. `US` -> Stripe; the 8 GoCardless launch
    /// countries (nels#320) -> GoCardless; `AU` -> Basiq; `NZ` -> Akahu
    /// (nels#323 — Akahu chosen over Basiq's partial NZ coverage); anything
    /// else -> None (caller must ask the user for a supported country, never
    /// guess).
    pub fn for_country(cc: &str) -> Option<Provider> {
        match cc.to_uppercase().as_str() {
            "US" => Some(Provider::Stripe),
            "GB" | "FR" | "DE" | "IT" | "ES" | "DK" | "FI" | "NO" => Some(Provider::GoCardless),
            "AU" => Some(Provider::Basiq),
            "NZ" => Some(Provider::Akahu),
            _ => None,
        }
    }

    /// (code, display name) pairs for every supported country, in the order
    /// the ticket lists them — used both by the REST institutions-country
    /// list and chat's clarifying-question copy.
    pub fn supported_countries() -> &'static [(&'static str, &'static str)] {
        &[
            ("US", "United States"),
            ("GB", "United Kingdom"),
            ("FR", "France"),
            ("DE", "Germany"),
            ("IT", "Italy"),
            ("ES", "Spain"),
            ("DK", "Denmark"),
            ("FI", "Finland"),
            ("NO", "Norway"),
            ("AU", "Australia"),
            ("NZ", "New Zealand"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_country_maps_us_to_stripe() {
        assert_eq!(Provider::for_country("US"), Some(Provider::Stripe));
    }

    #[test]
    fn for_country_maps_gocardless_countries() {
        for cc in ["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"] {
            assert_eq!(Provider::for_country(cc), Some(Provider::GoCardless), "country {cc}");
        }
    }

    #[test]
    fn for_country_maps_au_to_basiq() {
        assert_eq!(Provider::for_country("AU"), Some(Provider::Basiq));
    }

    #[test]
    fn for_country_maps_nz_to_akahu() {
        assert_eq!(Provider::for_country("NZ"), Some(Provider::Akahu));
    }

    #[test]
    fn for_country_au_nz_case_insensitive() {
        assert_eq!(Provider::for_country("au"), Some(Provider::Basiq));
        assert_eq!(Provider::for_country("nz"), Some(Provider::Akahu));
    }

    #[test]
    fn for_country_is_case_insensitive() {
        assert_eq!(Provider::for_country("gb"), Some(Provider::GoCardless));
        assert_eq!(Provider::for_country("us"), Some(Provider::Stripe));
    }

    #[test]
    fn for_country_rejects_unsupported() {
        assert_eq!(Provider::for_country("BR"), None);
        assert_eq!(Provider::for_country(""), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for p in [Provider::Stripe, Provider::GoCardless, Provider::Basiq, Provider::Akahu] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Provider::parse("plaid"), None);
    }

    #[test]
    fn supported_countries_lists_all_eleven() {
        assert_eq!(Provider::supported_countries().len(), 11);
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd backend && cargo test bank_provider`
Expected: PASS (all `bank_provider::tests::*`).

- [ ] **Step 5: Write the migration**

```sql
-- backend/migrations/20260707000000_basiq_akahu_bank_linking.sql
-- Extends #320's provider/bank_link_sessions generalization to support two
-- more providers (Basiq/Australia, Akahu/New Zealand — nels#323). Unlike
-- GoCardless, both Basiq and Akahu use a HOSTED consent flow (Basiq Connect /
-- Akahu Connect): Nels never picks an institution on its own side, so
-- bank_link_sessions.institution_id/institution_name/country (NOT NULL today,
-- populated by GoCardless's own institution-picker flow) must become
-- nullable for these two providers. See spec Assumptions 2-4.

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'basiq', 'akahu'));

ALTER TABLE bank_link_sessions ADD COLUMN provider TEXT NOT NULL DEFAULT 'gocardless'
    CHECK (provider IN ('gocardless', 'basiq', 'akahu'));

ALTER TABLE bank_link_sessions ALTER COLUMN institution_id DROP NOT NULL;
ALTER TABLE bank_link_sessions ALTER COLUMN institution_name DROP NOT NULL;
ALTER TABLE bank_link_sessions ALTER COLUMN country DROP NOT NULL;

CREATE INDEX IF NOT EXISTS bank_link_sessions_provider_idx ON bank_link_sessions (provider);
```

- [ ] **Step 6: Confirm the actual constraint name before applying (do not assume)**

Run: `cd backend && grep -n "provider TEXT NOT NULL DEFAULT 'stripe'" migrations/20260706120000_gocardless_bank_account_data.sql`
Expected: confirms the CHECK was added as an inline column constraint with no explicit name, so Postgres auto-names it `linked_accounts_provider_check` by convention (column name + `_check`). Verify against a running scratch DB with `SELECT conname FROM pg_constraint WHERE conrelid = 'linked_accounts'::regclass;` if in doubt, and correct the migration's `DROP CONSTRAINT` name to match before proceeding.

- [ ] **Step 7: Run the migration against a scratch DB and verify it applies cleanly**

Run: `cd backend && cargo test bank_linking:: -- --ignored --test-threads=1 2>&1 | tail -30`
Expected: the `#[ignore]`d `bank_linking` test (which runs `sqlx::migrate!` in its pool setup) applies this migration with no SQL errors, then passes (it doesn't touch the new columns, so behavior is unchanged).

- [ ] **Step 8: Commit**

```bash
git add backend/src/bank_provider.rs backend/migrations/20260707000000_basiq_akahu_bank_linking.sql
git commit -m "feat(#323): extend Provider enum and generalize bank_link_sessions for Basiq/Akahu"
```

---

## Task 2: `basiq.rs` — the Basiq integration (Australia)

**Files:**
- Create: `backend/src/basiq.rs`
- Modify: `backend/src/main.rs` (add `mod basiq;` only — route wiring is Task 4)

This is the largest task. Build it in the same "pure helpers first, HTTP/DB functions after" order `gocardless.rs` uses.

- [ ] **Step 1: Write failing unit tests for the pure helpers**

```rust
// backend/src/basiq.rs (new file) — start with just this
#[cfg(test)]
mod tests {
    use super::*;

    fn webhook_body(event_type: &str, connection_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": event_type,
            "connection": { "id": connection_id }
        })).unwrap()
    }

    #[test]
    fn map_basiq_webhook_event_transactions_updated() {
        let body = webhook_body("transactions.updated", "conn_1");
        let event: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            map_basiq_webhook_event(&event),
            Some(BasiqEvent::TransactionsUpdated { connection_id: "conn_1".to_string() })
        );
    }

    #[test]
    fn map_basiq_webhook_event_connection_deleted() {
        let body = webhook_body("connection.deleted", "conn_2");
        let event: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            map_basiq_webhook_event(&event),
            Some(BasiqEvent::ConnectionDeleted { connection_id: "conn_2".to_string() })
        );
    }

    #[test]
    fn map_basiq_webhook_event_ignores_unknown_type() {
        let body = webhook_body("job.created", "conn_3");
        let event: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(map_basiq_webhook_event(&event), None);
    }

    #[test]
    fn map_basiq_webhook_event_missing_connection_id_is_none() {
        let event: serde_json::Value = serde_json::json!({ "type": "transactions.updated" });
        assert_eq!(map_basiq_webhook_event(&event), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test basiq`
Expected: FAIL to compile — `basiq` module doesn't exist yet, and `main.rs` has no `mod basiq;` yet either. Add `mod basiq;` to `main.rs` now (alongside the existing `mod gocardless;` line) so the crate compiles once `basiq.rs` exists, then re-run to confirm the failure is now specifically about missing items inside `basiq.rs`.

- [ ] **Step 3: Implement the module — env/client setup, auth, webhook event mapping**

```rust
//! Basiq integration (nels#323): Australia, CDR (Consumer Data Right) hosted
//! consent ("Basiq Connect"). Mirrors `gocardless.rs`'s "own HTTP client, own
//! env test seam, own require_pro" convention. Unlike GoCardless, Nels never
//! picks an institution on its own side — the user searches for and
//! authenticates with their bank on Basiq's own hosted page (spec Assumption
//! 2), so there is no `list_institutions`/`find_institution_id` here.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::billing::user_is_pro;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn basiq_api_base() -> String {
    env_opt("BASIQ_API_BASE").unwrap_or_else(|| "https://au-api.basiq.io".to_string())
}
fn app_url() -> String {
    env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Basiq server-side token (`SERVER_ACCESS` scope) — used for account-holder
/// (user) management and any server-to-server call. Cached the same way
/// GoCardless's bearer token is (`gocardless.rs::access_token`), with the same
/// 60s safety margin before expiry.
struct CachedToken {
    access: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}
static SERVER_TOKEN: tokio::sync::RwLock<Option<CachedToken>> = tokio::sync::RwLock::const_new(None);

async fn server_token() -> Result<String, (StatusCode, String)> {
    {
        let guard = SERVER_TOKEN.read().await;
        if let Some(t) = guard.as_ref() {
            if t.expires_at > chrono::Utc::now() {
                return Ok(t.access.clone());
            }
        }
    }
    let api_key = env_opt("BASIQ_API_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    #[derive(Deserialize)]
    struct TokenResp { access_token: String, #[serde(default = "default_expires_in")] expires_in: i64 }
    fn default_expires_in() -> i64 { 3600 }
    let resp = http_client()
        .post(format!("{}/token", basiq_api_base()))
        .header("Authorization", format!("Basic {api_key}"))
        .header("basiq-version", "3.0")
        .form(&[("scope", "SERVER_ACCESS")])
        .send().await
        .map_err(|e| internal_error(format!("basiq token POST: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("basiq token decode: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "basiq token error");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    let parsed: TokenResp = serde_json::from_value(body)
        .map_err(|e| internal_error(format!("basiq token response decode: {e}")))?;
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(parsed.expires_in) - chrono::Duration::seconds(60);
    let mut guard = SERVER_TOKEN.write().await;
    *guard = Some(CachedToken { access: parsed.access_token.clone(), expires_at });
    Ok(parsed.access_token)
}

async fn basiq_get(path: &str, bearer: &str) -> Result<serde_json::Value, (StatusCode, String)> {
    let resp = http_client()
        .get(format!("{}/{}", basiq_api_base().trim_end_matches('/'), path.trim_start_matches('/')))
        .bearer_auth(bearer)
        .header("basiq-version", "3.0")
        .send().await
        .map_err(|e| internal_error(format!("basiq GET {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("basiq decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "basiq API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

async fn basiq_delete(path: &str, bearer: &str) -> Result<(), (StatusCode, String)> {
    let resp = http_client()
        .delete(format!("{}/{}", basiq_api_base().trim_end_matches('/'), path.trim_start_matches('/')))
        .bearer_auth(bearer)
        .header("basiq-version", "3.0")
        .send().await
        .map_err(|e| internal_error(format!("basiq DELETE {path}: {e}")))?;
    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        tracing::error!(?status, ?body, "basiq API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(())
}

async fn require_edit_or_owner(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to manage linked bank accounts".to_string()));
    }
    Ok(())
}

/// `pub(crate)` (not private) so `rag.rs`'s `LINK_BANK_ACCOUNT` arm can
/// pre-check before making any Basiq API call, mirroring
/// `gocardless::require_pro`'s exact rationale.
pub(crate) async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}

/// The two Basiq webhook event types this module handles, pre-parsed to just
/// the field the handler needs — mirrors `financial_connections::FcAccountEvent`'s
/// pure-mapping-function shape (unit-testable without a DB/HTTP handler).
#[derive(Debug, PartialEq)]
pub(crate) enum BasiqEvent {
    TransactionsUpdated { connection_id: String },
    ConnectionDeleted { connection_id: String },
}

pub(crate) fn map_basiq_webhook_event(event: &serde_json::Value) -> Option<BasiqEvent> {
    let connection_id = event.pointer("/connection/id").and_then(|v| v.as_str())?.to_string();
    match event.get("type").and_then(|v| v.as_str())? {
        "transactions.updated" => Some(BasiqEvent::TransactionsUpdated { connection_id }),
        "connection.deleted" => Some(BasiqEvent::ConnectionDeleted { connection_id }),
        _ => None,
    }
}
```

- [ ] **Step 4: Run to verify the pure-helper tests pass**

Run: `cd backend && cargo test basiq::tests`
Expected: PASS (all 4 `map_basiq_webhook_event_*` tests).

- [ ] **Step 5: Write failing tests for the consent + sync + disconnect flow (DB+HTTP integration, `#[ignore]`d)**

```rust
// backend/src/basiq.rs — append inside the SAME #[cfg(test)] mod tests block
// (after the existing 4 tests, before the closing brace)

use sqlx::postgres::PgPoolOptions;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
        "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}
async fn mk_user(db: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
        .bind(id).bind(format!("basiq-{id}@test.example")).execute(db).await.unwrap();
    id
}
async fn mk_budget(db: &PgPool, owner_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
        .bind(id).bind(owner_id).execute(db).await.unwrap();
    id
}
async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
    sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'active')")
        .bind(user_id).bind(customer_id).execute(db).await.unwrap();
}
fn set_basiq_env(server: &MockServer) {
    std::env::set_var("BASIQ_API_BASE", server.uri());
    std::env::set_var("BASIQ_API_KEY", "key_test");
}
fn clear_basiq_env() {
    std::env::remove_var("BASIQ_API_BASE");
    std::env::remove_var("BASIQ_API_KEY");
}
async fn mount_server_token(server: &MockServer) {
    // Matched on the request body's `scope=SERVER_ACCESS` field, NOT just the
    // path — `/token` is also used for the CLIENT_ACCESS exchange in
    // create_consent_session, and wiremock's tie-break for two mocks matching
    // the same path with no distinguishing matcher is "first mounted wins"
    // (see wiremock::mock's priority docs), which would silently route the
    // CLIENT_ACCESS request to this mock too if not disambiguated.
    Mock::given(method("POST")).and(path("/token")).and(wiremock::matchers::body_string_contains("scope=SERVER_ACCESS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "srv_tok", "expires_in": 3600})))
        .mount(server).await;
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn non_pro_user_cannot_start_basiq_link() {
    let server = MockServer::start().await;
    set_basiq_env(&server);
    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let result = create_consent_session(&db, uid, bid).await;
    assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_basiq_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn pro_user_can_start_and_complete_basiq_link() {
    let server = MockServer::start().await;
    set_basiq_env(&server);
    mount_server_token(&server).await;
    Mock::given(method("POST")).and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "basiq_user_1"})))
        .mount(&server).await;
    // Disambiguated from mount_server_token's SERVER_ACCESS mock by matching
    // on `scope=CLIENT_ACCESS` in the form body (see mount_server_token's
    // comment for why an undistinguished second /token mock would silently
    // never be reached).
    Mock::given(method("POST")).and(path("/token")).and(wiremock::matchers::body_string_contains("scope=CLIENT_ACCESS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "client_tok", "expires_in": 3600})))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    mk_pro_subscription(&db, uid, "cus_basiq").await;

    let started = create_consent_session(&db, uid, bid).await.expect("pro user can start a link");
    assert!(started.redirect_url.contains("client_tok"), "consent URL must carry the client-scoped token");

    // Complete: re-fetch the Basiq user's accounts server-side.
    Mock::given(method("GET")).and(path("/users/basiq_user_1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "id": "acc_1", "accountNo": "1234", "name": "Everyday",
                "connection": "conn_1",
                "institution": { "shortName": "ANZ" }
            }]
        })))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/users/basiq_user_1/transactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
        .mount(&server).await;

    let completed = complete_consent_session(&db, uid, bid, started.reference).await.expect("complete ok");
    assert_eq!(completed.accounts.len(), 1);
    assert_eq!(completed.accounts[0].provider, "basiq");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_basiq_env();
}

async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, provider_account_id: &str, provider_ref: &str) -> crate::db::LinkedAccount {
    sqlx::query_as::<_, crate::db::LinkedAccount>(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'basiq', $4, $5) RETURNING *")
        .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(provider_account_id).bind(provider_ref)
        .fetch_one(db).await.unwrap()
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn sync_account_transactions_is_idempotent_and_tags_aud() {
    let server = MockServer::start().await;
    set_basiq_env(&server);
    mount_server_token(&server).await;
    Mock::given(method("GET")).and(path("/users/basiq_user_sync/transactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "basiq_tx_1", "amount": "-52.30", "description": "Woolworths", "postDate": "2026-07-01T00:00:00Z", "account": "acc_sync_1"}]
        })))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let linked = mk_linked_account(&db, bid, uid, "acc_sync_1", "basiq_user_sync").await;

    let first = sync_account_transactions(&db, &linked).await.unwrap();
    assert_eq!(first, 1);
    let second = sync_account_transactions(&db, &linked).await.unwrap();
    assert_eq!(second, 0, "re-sync of the same Basiq transaction must import 0, not duplicate");

    let (currency, amount): (Option<String>, f64) = sqlx::query_as(
        "SELECT currency, amount FROM transactions WHERE provider_transaction_id = 'basiq_tx_1'")
        .fetch_one(&db).await.unwrap();
    assert_eq!(currency.as_deref(), Some("AUD"));
    assert_eq!(amount, 52.30);

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_basiq_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn disconnect_revokes_connection_only_when_last_active_sibling() {
    let server = MockServer::start().await;
    set_basiq_env(&server);
    mount_server_token(&server).await;
    // Two linked_accounts rows share the SAME Basiq connection (provider_ref).
    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let acc_a = mk_linked_account(&db, bid, uid, "acc_disc_a", "conn_shared").await;
    let acc_b = mk_linked_account(&db, bid, uid, "acc_disc_b", "conn_shared").await;

    // Disconnecting the FIRST of two active siblings must NOT call Basiq's delete.
    Mock::given(method("DELETE")).and(path("/users/basiq_user_x/connections/conn_shared"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server).await;
    disconnect_linked_account(&db, uid, bid, acc_a.id).await.expect("disconnect ok");
    let status_a: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(acc_a.id).fetch_one(&db).await.unwrap();
    assert_eq!(status_a, "disconnected");
    let status_b: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(acc_b.id).fetch_one(&db).await.unwrap();
    assert_eq!(status_b, "active", "sibling sharing the same connection must stay active");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_basiq_env();
}
```

- [ ] **Step 6: Run to verify these fail**

Run: `cd backend && cargo test basiq -- --ignored`
Expected: FAIL to compile — `create_consent_session`/`complete_consent_session`/`sync_account_transactions`/`disconnect_linked_account`/`refresh_linked_account` don't exist yet.

- [ ] **Step 7: Implement the consent, sync, refresh, and disconnect functions**

```rust
// backend/src/basiq.rs — insert ABOVE the #[cfg(test)] block, after map_basiq_webhook_event

#[derive(Debug, Serialize)]
pub struct BasiqLinkSessionResponse {
    pub redirect_url: String,
    pub reference: Uuid,
}

/// Start a Basiq consent flow: create (or reuse) a Basiq user scoped to this
/// Nels user, mint a CLIENT_ACCESS token for that Basiq user, persist a
/// pending `bank_link_sessions` row, and build the hosted "Basiq Connect"
/// consent URL. Pro-gated, Edit-or-Owner-gated, closed-budget rejected —
/// mirrors `gocardless::start_link_session` exactly except there is no
/// institution_id/country argument (spec Assumption 2).
pub async fn create_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<BasiqLinkSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let server_tok = server_token().await?;
    #[derive(Deserialize)]
    struct BasiqUser { id: String }
    let created: serde_json::Value = basiq_post_json("users", &server_tok, &serde_json::json!({
        "email": format!("{user_id}@nels.internal"),
    })).await?;
    let basiq_user: BasiqUser = serde_json::from_value(created)
        .map_err(|e| internal_error(format!("basiq user decode: {e}")))?;

    #[derive(Deserialize)]
    struct ClientToken { access_token: String }
    let api_key = env_opt("BASIQ_API_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let resp = http_client()
        .post(format!("{}/token", basiq_api_base()))
        .header("Authorization", format!("Basic {api_key}"))
        .header("basiq-version", "3.0")
        .form(&[("scope", "CLIENT_ACCESS"), ("userId", basiq_user.id.as_str())])
        .send().await
        .map_err(|e| internal_error(format!("basiq client token POST: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("basiq client token decode: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "basiq client token error");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    let client_token: ClientToken = serde_json::from_value(body)
        .map_err(|e| internal_error(format!("basiq client token response decode: {e}")))?;

    let session_id = Uuid::new_v4();
    // `institution_id`/`institution_name`/`country` are NULL — Basiq's hosted
    // consent picks the bank on our behalf (spec Assumption 2). `requisition_id`
    // is reused generically to hold the Basiq user id (spec Assumption 4).
    sqlx::query(
        "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
         VALUES ($1, $2, $3, $4, 'basiq', 'pending')")
        .bind(session_id).bind(budget_id).bind(user_id).bind(&basiq_user.id)
        .execute(pool).await.map_err(internal_error)?;

    let redirect_url = format!(
        "https://consent.basiq.io/home?token={}&action=connect&redirectUrl={}",
        client_token.access_token,
        urlencoding_basiq(&format!("{}/?basiq_ref={session_id}", app_url().trim_end_matches('/'))),
    );
    Ok(BasiqLinkSessionResponse { redirect_url, reference: session_id })
}

/// Minimal percent-encoding for a URL used as a query-string VALUE (not a
/// dependency addition — `url::form_urlencoded` is already used by
/// `gocardless.rs`, reused here rather than hand-rolling a second encoder).
fn urlencoding_basiq(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn basiq_post_json(path: &str, bearer: &str, json_body: &serde_json::Value) -> Result<serde_json::Value, (StatusCode, String)> {
    let resp = http_client()
        .post(format!("{}/{}", basiq_api_base().trim_end_matches('/'), path.trim_start_matches('/')))
        .bearer_auth(bearer)
        .header("basiq-version", "3.0")
        .json(json_body)
        .send().await
        .map_err(|e| internal_error(format!("basiq POST {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("basiq decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "basiq API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

/// Finish a Basiq consent flow: look up the pending session, verify
/// ownership, re-fetch the Basiq user's accounts SERVER-SIDE, and persist
/// each as a `linked_accounts` row (`provider = 'basiq'`). `provider_ref`
/// stores the Basiq CONNECTION id (not the Basiq user id) so multiple
/// accounts under one bank connection share a value — required for
/// `disconnect_linked_account`'s "last active sibling" rule (spec
/// Assumption 5). Returns the shared `ListLinkedAccountsResponse` shape.
pub async fn complete_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    basiq_ref: Uuid,
) -> Result<crate::financial_connections::ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    #[derive(sqlx::FromRow)]
    struct PendingSession { budget_id: Uuid, user_id: Uuid, requisition_id: String, status: String }
    let session: PendingSession = sqlx::query_as(
        "SELECT budget_id, user_id, requisition_id, status FROM bank_link_sessions WHERE id = $1 AND provider = 'basiq'")
        .bind(basiq_ref).fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Link session not found or expired".to_string()))?;

    if session.budget_id != budget_id || session.user_id != user_id {
        tracing::warn!(%user_id, %budget_id, "basiq link session ownership mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }
    if session.status != "pending" {
        return Err((StatusCode::CONFLICT, "This link session has already been completed".to_string()));
    }
    let basiq_user_id = session.requisition_id;

    let server_tok = server_token().await?;
    let accounts_body = basiq_get(&format!("users/{basiq_user_id}/accounts"), &server_tok).await?;
    let accounts = accounts_body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut results = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let account_id = match acc.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let connection_id = acc.get("connection").and_then(|v| v.as_str()).unwrap_or(&basiq_user_id).to_string();
        let institution_name = acc.pointer("/institution/shortName").and_then(|v| v.as_str()).map(str::to_string);
        let display_name = acc.get("name").and_then(|v| v.as_str()).map(str::to_string);
        let last4 = acc.get("accountNo").and_then(|v| v.as_str())
            .map(|s| s.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());

        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(&account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(%user_id, account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "basiq account already linked to a different budget");
                return Err((StatusCode::CONFLICT, "This bank account is already linked to a different budget. Disconnect it there first.".to_string()));
            }
        }

        let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, institution_name, display_name, last4) \
             VALUES ($1, $2, $3, 'basiq', $4, $5, $6, $7, $8) \
             ON CONFLICT (provider_account_id) DO UPDATE SET \
                institution_name = EXCLUDED.institution_name, display_name = EXCLUDED.display_name, \
                last4 = EXCLUDED.last4, status = 'active', disconnected_at = NULL, updated_at = now() \
             RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id)
            .bind(&account_id).bind(&connection_id).bind(&institution_name).bind(&display_name).bind(&last4)
            .fetch_one(pool).await.map_err(internal_error)?;

        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial basiq sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    sqlx::query("UPDATE bank_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(basiq_ref).execute(pool).await.map_err(internal_error)?;

    Ok(crate::financial_connections::ListLinkedAccountsResponse { accounts: results })
}

/// Pull transactions for one linked Basiq account and idempotently insert new
/// rows. Mirrors `gocardless::sync_account_transactions`'s contract exactly
/// (closed-budget no-op, best-effort-but-error-surfacing, updates
/// `last_synced_at` regardless of outcome). `currency = 'AUD'` is hardcoded
/// (spec Assumption 8 — Basiq is AU-only in this app).
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if closed_at.is_some() {
        tracing::info!(account_id = %linked_account.id, "skipping basiq sync: budget is closed");
        return Ok(0);
    }

    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    let server_tok = server_token().await?;
    // `provider_ref` holds the Basiq CONNECTION id for accounts persisted via
    // complete_consent_session, but the transactions endpoint is scoped to the
    // Basiq USER id — fetch it once via the account's owning user lookup is
    // unnecessary because Basiq's `/users/{userId}/transactions?filter=account.id.eq('...')`
    // needs the user id, which this codebase does not separately store per
    // account (only `provider_ref` = connection id is kept). Basiq also
    // exposes `/accounts/{accountId}/transactions` scoped directly to the
    // account, which needs no user id at all — used here instead, avoiding
    // the extra lookup entirely.
    let path = format!("accounts/{}/transactions", linked_account.provider_account_id);
    let body = basiq_get(&path, &server_tok).await?;
    let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut imported: u64 = 0;
    for tx in &rows {
        let tx_id = match tx.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let amount_str = tx.get("amount").and_then(|v| v.as_str()).unwrap_or("0");
        let Some(amount) = crate::gocardless::normalize_amount(amount_str) else {
            tracing::warn!(account_id = %linked_account.id, tx_id, amount_str, "basiq sync: dropping transaction with a malformed amount");
            continue;
        };
        let description = tx.get("description").and_then(|v| v.as_str()).unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("postDate").and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let category_id = crate::financial_connections::guess_category_id(&description, &categories);

        let res = sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, \
                 external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'AUD') \
             ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
        )
        .bind(Uuid::new_v4()).bind(linked_account.budget_id).bind(category_id).bind(amount)
        .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id)
        .execute(pool).await.map_err(internal_error)?;
        if res.rows_affected() > 0 {
            imported += 1;
        }
    }

    sqlx::query("UPDATE linked_accounts SET last_synced_at = now(), updated_at = now() WHERE id = $1")
        .bind(linked_account.id).execute(pool).await.map_err(internal_error)?;
    Ok(imported)
}

/// Manual refresh: Pro-gated, synchronous re-sync (Basiq has no "ask them to
/// check now" push like Stripe FC — this just re-pulls directly, same shape
/// as GoCardless's manual refresh).
pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'basiq' AND status = 'active'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    sync_account_transactions(pool, &linked).await?;
    Ok(())
}

/// Disconnect: connection-scoped (spec Assumption 5) — only call Basiq's
/// connection-delete endpoint when NO OTHER active local row shares this
/// row's `provider_ref` (Basiq connection id); always flip only THIS row's
/// local status regardless. Not Pro-gated.
pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'basiq'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    let sibling_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM linked_accounts WHERE provider_ref = $1 AND id != $2 AND status = 'active'")
        .bind(&row.provider_ref).bind(account_id)
        .fetch_one(pool).await.map_err(internal_error)?;

    if sibling_count == 0 {
        // We don't separately store the Basiq user id per row (only the
        // connection id, in provider_ref) — Basiq's connection-delete
        // endpoint is scoped by BOTH user id and connection id
        // (`/users/{userId}/connections/{connectionId}`). The user id is
        // recoverable via the account's OWN accounts lookup at connect time,
        // but is not persisted separately today; deriving it here would need
        // an extra Basiq call this codebase doesn't otherwise need. Since
        // `bank_link_sessions.requisition_id` held the Basiq user id at
        // link time and sessions aren't purged, look it up from the
        // most recent completed session for this budget+user as a
        // pragmatic bridge.
        let basiq_user_id: Option<String> = sqlx::query_scalar(
            "SELECT requisition_id FROM bank_link_sessions \
             WHERE budget_id = $1 AND user_id = $2 AND provider = 'basiq' AND status = 'completed' \
             ORDER BY created_at DESC LIMIT 1")
            .bind(budget_id).bind(user_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(basiq_user_id) = basiq_user_id {
            let server_tok = server_token().await?;
            basiq_delete(&format!("users/{basiq_user_id}/connections/{}", row.provider_ref), &server_tok).await?;
        } else {
            tracing::warn!(account_id = %account_id, "basiq disconnect: no completed link session found to resolve the Basiq user id; local-only disconnect");
        }
    } else {
        tracing::info!(account_id = %account_id, provider_ref = %row.provider_ref, sibling_count, "basiq disconnect: other active accounts share this connection — skipping Basiq-side revoke");
    }

    sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() WHERE id = $1")
        .bind(account_id).execute(pool).await.map_err(internal_error)?;
    Ok(())
}

/// PUBLIC, signature-verified. Raw body required for HMAC. Mounted OUTSIDE
/// the auth nest. Verifies via the shared `billing::verify_stripe_signature`
/// HMAC scheme against `BASIQ_WEBHOOK_SECRET` (spec Assumption 6).
pub async fn webhook(
    axum::extract::State(state): axum::extract::State<crate::auth::AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let secret = env_opt("BASIQ_WEBHOOK_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let sig = headers.get("basiq-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = chrono::Utc::now().timestamp();
    if crate::billing::verify_stripe_signature(&body, sig, &secret, now, 300).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string())),
    };

    match map_basiq_webhook_event(&event) {
        Some(BasiqEvent::TransactionsUpdated { connection_id }) => {
            let linked: Vec<crate::db::LinkedAccount> = sqlx::query_as(
                "SELECT * FROM linked_accounts WHERE provider = 'basiq' AND provider_ref = $1 AND status = 'active'")
                .bind(&connection_id)
                .fetch_all(&state.db).await.map_err(internal_error)?;
            for row in linked {
                let owner_status: Option<String> = sqlx::query_scalar(
                    "SELECT status FROM subscriptions WHERE user_id = $1")
                    .bind(row.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
                if user_is_pro(owner_status.as_deref()) {
                    if let Err((status, msg)) = sync_account_transactions(&state.db, &row).await {
                        tracing::warn!(?status, %msg, account_id = %row.id, "webhook-driven basiq sync failed");
                    }
                } else {
                    tracing::info!(account_id = %row.id, "skipping basiq refresh: owner is not Pro (subscription lapsed)");
                }
            }
        }
        Some(BasiqEvent::ConnectionDeleted { connection_id }) => {
            sqlx::query(
                "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() \
                 WHERE provider = 'basiq' AND provider_ref = $1 AND status = 'active'")
                .bind(&connection_id)
                .execute(&state.db).await.map_err(internal_error)?;
        }
        None => tracing::debug!(?event, "ignoring unhandled basiq event type"),
    }
    Ok(StatusCode::OK)
}
```

- [ ] **Step 8: Run to verify all `basiq` tests pass**

Run: `cd backend && cargo test basiq -- --include-ignored --test-threads=1 2>&1 | tail -40`
Expected: PASS for all pure unit tests; the `#[ignore]`d ones PASS too if a local `podman-compose up -d` Postgres is running (per `AGENTS.md`'s Developer Commands) — if no local DB is available, at minimum confirm `cargo test basiq` (non-ignored) passes and `cargo check` is clean.

- [ ] **Step 9: Commit**

```bash
git add backend/src/basiq.rs backend/src/main.rs
git commit -m "feat(#323): add Basiq (Australia) bank-linking integration"
```

---

## Task 3: `akahu.rs` — the Akahu integration (New Zealand)

**Files:**
- Create: `backend/src/akahu.rs`
- Modify: `backend/src/main.rs` (add `mod akahu;` only)

Same build order as Task 2.

- [ ] **Step 1: Write failing unit tests for the pure helpers**

```rust
// backend/src/akahu.rs (new file) — start with just this
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_amount_takes_absolute_value() {
        assert_eq!(normalize_amount(-52.30), 52.30);
        assert_eq!(normalize_amount(52.30), normalize_amount(-52.30));
    }

    #[test]
    fn normalize_amount_zero_is_zero() {
        assert_eq!(normalize_amount(0.0), 0.0);
    }

    fn webhook_body(event_type: &str, account_id: &str) -> serde_json::Value {
        serde_json::json!({ "type": event_type, "item": { "resource": { "_account": account_id } } })
    }

    #[test]
    fn map_akahu_webhook_event_transaction_created() {
        let event = webhook_body("TRANSACTION_CREATED", "acc_1");
        assert_eq!(map_akahu_webhook_event(&event), Some(AkahuEvent::TransactionCreated { account_id: "acc_1".to_string() }));
    }

    #[test]
    fn map_akahu_webhook_event_ignores_unknown_type() {
        let event = webhook_body("ACCOUNT_UPDATED", "acc_2");
        assert_eq!(map_akahu_webhook_event(&event), None);
    }

    #[test]
    fn map_akahu_webhook_event_missing_account_id_is_none() {
        let event = serde_json::json!({ "type": "TRANSACTION_CREATED" });
        assert_eq!(map_akahu_webhook_event(&event), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test akahu`
Expected: FAIL to compile. Add `mod akahu;` to `main.rs` now (alongside `mod basiq;`) so the crate compiles once the file exists.

- [ ] **Step 3: Implement the module — env/client setup, OAuth token exchange, webhook event mapping**

```rust
//! Akahu integration (nels#323): New Zealand, OAuth2 hosted consent ("Akahu
//! Connect"). Chosen over Basiq for NZ users per the ticket's explicit design
//! note (Basiq's NZ coverage is partial; Akahu is NZ-native). Mirrors
//! `gocardless.rs`'s "own HTTP client, own env test seam, own require_pro"
//! convention. One Akahu access token can cover a user's accounts across
//! MULTIPLE NZ institutions (spec Assumption 5) — this drives the
//! local-status-only disconnect below.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::billing::user_is_pro;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn akahu_api_base() -> String {
    env_opt("AKAHU_API_BASE").unwrap_or_else(|| "https://api.akahu.io/v1".to_string())
}
fn akahu_oauth_base() -> String {
    env_opt("AKAHU_OAUTH_BASE").unwrap_or_else(|| "https://oauth.akahu.nz".to_string())
}
fn app_url() -> String {
    env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Akahu amounts are signed JSON floats in dollars (not cents, not decimal
/// strings like GoCardless/Basiq) — a small dedicated normalizer, mirroring
/// the "pure, independently unit-tested" convention (spec Assumption 9).
pub(crate) fn normalize_amount(amount: f64) -> f64 {
    amount.abs()
}

async fn require_edit_or_owner(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to manage linked bank accounts".to_string()));
    }
    Ok(())
}

pub(crate) async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}

/// The one Akahu webhook event type this module acts on — Akahu pushes a
/// per-transaction event, but this module reacts by triggering a full
/// re-sync of the referenced account (spec Architecture note) rather than
/// parsing the individual transaction out of the payload, keeping the same
/// "webhook triggers sync_account_transactions" shape as Basiq/Stripe.
#[derive(Debug, PartialEq)]
pub(crate) enum AkahuEvent {
    TransactionCreated { account_id: String },
}

pub(crate) fn map_akahu_webhook_event(event: &serde_json::Value) -> Option<AkahuEvent> {
    let account_id = event.pointer("/item/resource/_account").and_then(|v| v.as_str())?.to_string();
    match event.get("type").and_then(|v| v.as_str())? {
        "TRANSACTION_CREATED" => Some(AkahuEvent::TransactionCreated { account_id }),
        _ => None,
    }
}
```

- [ ] **Step 4: Run to verify the pure-helper tests pass**

Run: `cd backend && cargo test akahu::tests`
Expected: PASS (5 tests: 2 `normalize_amount_*`, 3 `map_akahu_webhook_event_*`).

- [ ] **Step 5: Write failing tests for the consent + sync + disconnect flow**

```rust
// backend/src/akahu.rs — append inside the SAME #[cfg(test)] mod tests block

use sqlx::postgres::PgPoolOptions;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
        "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}
async fn mk_user(db: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
        .bind(id).bind(format!("akahu-{id}@test.example")).execute(db).await.unwrap();
    id
}
async fn mk_budget(db: &PgPool, owner_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
        .bind(id).bind(owner_id).execute(db).await.unwrap();
    id
}
async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
    sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'active')")
        .bind(user_id).bind(customer_id).execute(db).await.unwrap();
}
fn set_akahu_env(server: &MockServer) {
    std::env::set_var("AKAHU_API_BASE", server.uri());
    std::env::set_var("AKAHU_OAUTH_BASE", server.uri());
    std::env::set_var("AKAHU_APP_TOKEN", "app_test");
    std::env::set_var("AKAHU_APP_SECRET", "secret_test");
}
fn clear_akahu_env() {
    std::env::remove_var("AKAHU_API_BASE");
    std::env::remove_var("AKAHU_OAUTH_BASE");
    std::env::remove_var("AKAHU_APP_TOKEN");
    std::env::remove_var("AKAHU_APP_SECRET");
}
async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, provider_account_id: &str, provider_ref: &str) -> crate::db::LinkedAccount {
    sqlx::query_as::<_, crate::db::LinkedAccount>(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'akahu', $4, $5) RETURNING *")
        .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(provider_account_id).bind(provider_ref)
        .fetch_one(db).await.unwrap()
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn non_pro_user_cannot_start_akahu_link() {
    let server = MockServer::start().await;
    set_akahu_env(&server);
    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let result = create_consent_session(&db, uid, bid).await;
    assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_akahu_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn pro_user_can_start_and_complete_akahu_link() {
    let server = MockServer::start().await;
    set_akahu_env(&server);
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "user_tok_1"})))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    mk_pro_subscription(&db, uid, "cus_akahu").await;

    let started = create_consent_session(&db, uid, bid).await.expect("pro user can start a link");
    assert!(started.redirect_url.starts_with(&server.uri()) || started.redirect_url.contains("oauth"));

    Mock::given(method("GET")).and(path("/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "items": [{"_id": "acc_akahu_1", "name": "Everyday", "formatted_account": "12-3456-7890123-00", "connection": {"name": "ASB Bank"}}]
        })))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/transactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true, "items": []})))
        .mount(&server).await;

    let completed = complete_consent_session(&db, uid, bid, started.reference, "auth_code_1").await.expect("complete ok");
    assert_eq!(completed.accounts.len(), 1);
    assert_eq!(completed.accounts[0].provider, "akahu");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_akahu_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn sync_account_transactions_is_idempotent_and_tags_nzd() {
    let server = MockServer::start().await;
    set_akahu_env(&server);
    Mock::given(method("GET")).and(path("/accounts/acc_sync_1/transactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "items": [{"_id": "akahu_tx_1", "amount": -52.30, "description": "Countdown", "date": "2026-07-01T00:00:00Z"}]
        })))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let linked = mk_linked_account(&db, bid, uid, "acc_sync_1", "user_tok_sync").await;

    let first = sync_account_transactions(&db, &linked).await.unwrap();
    assert_eq!(first, 1);
    let second = sync_account_transactions(&db, &linked).await.unwrap();
    assert_eq!(second, 0, "re-sync of the same Akahu transaction must import 0, not duplicate");

    let (currency, amount): (Option<String>, f64) = sqlx::query_as(
        "SELECT currency, amount FROM transactions WHERE provider_transaction_id = 'akahu_tx_1'")
        .fetch_one(&db).await.unwrap();
    assert_eq!(currency.as_deref(), Some("NZD"));
    assert_eq!(amount, 52.30);

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_akahu_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn disconnect_is_local_only_and_makes_zero_akahu_calls() {
    // Akahu's shared-token design (spec Assumption 5): disconnect must NEVER
    // call Akahu's revoke endpoint, since the same token covers other
    // Akahu-linked accounts for this user. `.expect(0)` on ANY mounted
    // endpoint proves zero calls were made.
    let server = MockServer::start().await;
    set_akahu_env(&server);
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let linked = mk_linked_account(&db, bid, uid, "acc_disc_1", "user_tok_disc").await;

    disconnect_linked_account(&db, uid, bid, linked.id).await.expect("disconnect ok");
    let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(linked.id).fetch_one(&db).await.unwrap();
    assert_eq!(status, "disconnected");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_akahu_env();
}
```

- [ ] **Step 6: Run to verify these fail**

Run: `cd backend && cargo test akahu -- --ignored`
Expected: FAIL to compile — `create_consent_session`/`complete_consent_session`/`sync_account_transactions`/`disconnect_linked_account` don't exist yet.

- [ ] **Step 7: Implement the consent, sync, refresh, and disconnect functions**

```rust
// backend/src/akahu.rs — insert ABOVE the #[cfg(test)] block, after map_akahu_webhook_event

#[derive(Debug, Serialize)]
pub struct AkahuLinkSessionResponse {
    pub redirect_url: String,
    pub reference: Uuid,
}

/// Start an Akahu OAuth2 consent flow ("Akahu Connect"): persist a pending
/// `bank_link_sessions` row keyed by the session id (used as OAuth `state`
/// for anti-CSRF/correlation), and build the hosted authorize URL. Pro-gated,
/// Edit-or-Owner-gated, closed-budget rejected — mirrors
/// `gocardless::start_link_session`/`basiq::create_consent_session` except no
/// institution_id (spec Assumption 3).
pub async fn create_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<AkahuLinkSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let app_token = env_opt("AKAHU_APP_TOKEN")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
         VALUES ($1, $2, $3, '', 'akahu', 'pending')")
        .bind(session_id).bind(budget_id).bind(user_id)
        .execute(pool).await.map_err(internal_error)?;

    let redirect_uri = format!("{}/?akahu_ref={session_id}", app_url().trim_end_matches('/'));
    let redirect_url = format!(
        "{}/authorize?response_type=code&client_id={}&redirect_uri={}&state={session_id}&scope=ENDURING_CONSENT",
        akahu_oauth_base(), app_token, url::form_urlencoded::byte_serialize(redirect_uri.as_bytes()).collect::<String>(),
    );
    Ok(AkahuLinkSessionResponse { redirect_url, reference: session_id })
}

async fn akahu_get(path: &str, bearer: &str) -> Result<serde_json::Value, (StatusCode, String)> {
    let app_token = env_opt("AKAHU_APP_TOKEN")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let resp = http_client()
        .get(format!("{}/{}", akahu_api_base().trim_end_matches('/'), path.trim_start_matches('/')))
        .bearer_auth(bearer)
        .header("X-Akahu-Id", app_token)
        .send().await
        .map_err(|e| internal_error(format!("akahu GET {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("akahu decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "akahu API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

/// Finish an Akahu consent flow: verify the pending session, exchange the
/// OAuth `code` for a user access token, fetch the user's accounts, and
/// persist each as a `linked_accounts` row. `provider_ref` stores the SAME
/// Akahu access token for every account this user links (spec Assumption 5 —
/// the token is shared across institutions), which is what makes
/// `disconnect_linked_account`'s local-only behavior safe/necessary.
pub async fn complete_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    akahu_ref: Uuid,
    code: &str,
) -> Result<crate::financial_connections::ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    #[derive(sqlx::FromRow)]
    struct PendingSession { budget_id: Uuid, user_id: Uuid, status: String }
    let session: PendingSession = sqlx::query_as(
        "SELECT budget_id, user_id, status FROM bank_link_sessions WHERE id = $1 AND provider = 'akahu'")
        .bind(akahu_ref).fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Link session not found or expired".to_string()))?;

    if session.budget_id != budget_id || session.user_id != user_id {
        tracing::warn!(%user_id, %budget_id, "akahu link session ownership mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }
    if session.status != "pending" {
        return Err((StatusCode::CONFLICT, "This link session has already been completed".to_string()));
    }

    let app_token = env_opt("AKAHU_APP_TOKEN")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let app_secret = env_opt("AKAHU_APP_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let redirect_uri = format!("{}/?akahu_ref={akahu_ref}", app_url().trim_end_matches('/'));

    #[derive(Deserialize)]
    struct TokenResp { access_token: String }
    let resp = http_client()
        .post(format!("{}/token", akahu_oauth_base()))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", app_token.as_str()),
            ("client_secret", app_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send().await
        .map_err(|e| internal_error(format!("akahu token exchange: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("akahu token decode: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "akahu token exchange error");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    let token: TokenResp = serde_json::from_value(body)
        .map_err(|e| internal_error(format!("akahu token response decode: {e}")))?;

    let accounts_body = akahu_get("accounts", &token.access_token).await?;
    let accounts = accounts_body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut results = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let account_id = match acc.get("_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let institution_name = acc.pointer("/connection/name").and_then(|v| v.as_str()).map(str::to_string);
        let display_name = acc.get("name").and_then(|v| v.as_str()).map(str::to_string);
        let last4 = acc.get("formatted_account").and_then(|v| v.as_str())
            .map(|s| s.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());

        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(&account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(%user_id, account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "akahu account already linked to a different budget");
                return Err((StatusCode::CONFLICT, "This bank account is already linked to a different budget. Disconnect it there first.".to_string()));
            }
        }

        let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, institution_name, display_name, last4) \
             VALUES ($1, $2, $3, 'akahu', $4, $5, $6, $7, $8) \
             ON CONFLICT (provider_account_id) DO UPDATE SET \
                institution_name = EXCLUDED.institution_name, display_name = EXCLUDED.display_name, \
                last4 = EXCLUDED.last4, provider_ref = EXCLUDED.provider_ref, \
                status = 'active', disconnected_at = NULL, updated_at = now() \
             RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id)
            .bind(&account_id).bind(&token.access_token).bind(&institution_name).bind(&display_name).bind(&last4)
            .fetch_one(pool).await.map_err(internal_error)?;

        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial akahu sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    sqlx::query("UPDATE bank_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(akahu_ref).execute(pool).await.map_err(internal_error)?;

    Ok(crate::financial_connections::ListLinkedAccountsResponse { accounts: results })
}

/// Pull transactions for one linked Akahu account and idempotently insert new
/// rows. `currency = 'NZD'` hardcoded (spec Assumption 8 — Akahu is NZ-only in
/// this app). `provider_ref` (the shared access token) is used as the bearer.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if closed_at.is_some() {
        tracing::info!(account_id = %linked_account.id, "skipping akahu sync: budget is closed");
        return Ok(0);
    }

    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    let path = format!("accounts/{}/transactions", linked_account.provider_account_id);
    let body = akahu_get(&path, &linked_account.provider_ref).await?;
    let rows = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut imported: u64 = 0;
    for tx in &rows {
        let tx_id = match tx.get("_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let amount = tx.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let amount = normalize_amount(amount);
        let description = tx.get("description").and_then(|v| v.as_str()).unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("date").and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let category_id = crate::financial_connections::guess_category_id(&description, &categories);

        let res = sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, \
                 external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'NZD') \
             ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
        )
        .bind(Uuid::new_v4()).bind(linked_account.budget_id).bind(category_id).bind(amount)
        .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id)
        .execute(pool).await.map_err(internal_error)?;
        if res.rows_affected() > 0 {
            imported += 1;
        }
    }

    sqlx::query("UPDATE linked_accounts SET last_synced_at = now(), updated_at = now() WHERE id = $1")
        .bind(linked_account.id).execute(pool).await.map_err(internal_error)?;
    Ok(imported)
}

/// Manual refresh: Pro-gated, synchronous re-sync — same shape as Basiq's.
pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'akahu' AND status = 'active'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    sync_account_transactions(pool, &linked).await?;
    Ok(())
}

/// Disconnect: LOCAL-STATUS-ONLY, always (spec Assumption 5) — Akahu's
/// access token is shared across a user's accounts at potentially multiple
/// institutions, so revoking it here would silently break every other
/// Akahu-linked account for this user. This is a deliberate product
/// tradeoff, not an oversight — see the doc comment on the module and the
/// spec's Assumption 5. Not Pro-gated.
pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'akahu'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?;
    if exists.is_none() {
        return Err((StatusCode::NOT_FOUND, "Linked account not found".to_string()));
    }

    sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() WHERE id = $1")
        .bind(account_id).execute(pool).await.map_err(internal_error)?;
    Ok(())
}

/// PUBLIC, signature-verified. Verifies via the shared HMAC scheme against
/// `AKAHU_WEBHOOK_SECRET` (spec Assumption 6 — NOT Akahu's real RSA scheme;
/// documented gap).
pub async fn webhook(
    axum::extract::State(state): axum::extract::State<crate::auth::AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let secret = env_opt("AKAHU_WEBHOOK_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let sig = headers.get("x-akahu-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = chrono::Utc::now().timestamp();
    if crate::billing::verify_stripe_signature(&body, sig, &secret, now, 300).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string())),
    };

    if let Some(AkahuEvent::TransactionCreated { account_id }) = map_akahu_webhook_event(&event) {
        let linked: Option<crate::db::LinkedAccount> = sqlx::query_as(
            "SELECT * FROM linked_accounts WHERE provider = 'akahu' AND provider_account_id = $1 AND status = 'active'")
            .bind(&account_id)
            .fetch_optional(&state.db).await.map_err(internal_error)?;
        if let Some(row) = linked {
            let owner_status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM subscriptions WHERE user_id = $1")
                .bind(row.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
            if user_is_pro(owner_status.as_deref()) {
                if let Err((status, msg)) = sync_account_transactions(&state.db, &row).await {
                    tracing::warn!(?status, %msg, account_id = %row.id, "webhook-driven akahu sync failed");
                }
            } else {
                tracing::info!(account_id = %row.id, "skipping akahu refresh: owner is not Pro (subscription lapsed)");
            }
        } else {
            tracing::debug!(account_id, "akahu webhook for unknown or inactive account");
        }
    }
    Ok(StatusCode::OK)
}
```

- [ ] **Step 8: Run to verify all `akahu` tests pass**

Run: `cd backend && cargo test akahu -- --include-ignored --test-threads=1 2>&1 | tail -40`
Expected: PASS for all pure unit tests; the `#[ignore]`d ones PASS if a local Postgres is running.

- [ ] **Step 9: Commit**

```bash
git add backend/src/akahu.rs backend/src/main.rs
git commit -m "feat(#323): add Akahu (New Zealand) bank-linking integration"
```

---

## Task 4: `bank_linking.rs` dispatch + REST wiring + `main.rs`

**Files:**
- Modify: `backend/src/bank_linking.rs`
- Modify: `backend/src/main.rs`
- Modify: `backend/.env.example`

- [ ] **Step 1: Write the failing dispatch test**

```rust
// backend/src/bank_linking.rs — add inside the existing #[cfg(test)] mod tests block,
// after refresh_dispatches_to_gocardless_for_a_gocardless_row
#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn refresh_dispatches_to_basiq_for_a_basiq_row() {
    let db = test_pool().await;
    let uid = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
        .bind(uid).bind(format!("bl-basiq-{uid}@test.example")).execute(&db).await.unwrap();
    let bid = Uuid::new_v4();
    sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
        .bind(bid).bind(uid).execute(&db).await.unwrap();
    // No subscriptions row -> not Pro -> refresh_linked_account's require_pro
    // guard fires 402, proving dispatch reached Basiq's implementation (a
    // GoCardless/Stripe row would 404 on a provider mismatch instead, same
    // reasoning as the existing GoCardless dispatch test).
    sqlx::query(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'basiq', 'acct_bl_basiq', 'conn_bl_basiq')")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
    let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl_basiq'")
        .fetch_one(&db).await.unwrap();

    let result = refresh_linked_account(&db, uid, bid, account_id).await;
    assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn disconnect_dispatches_to_akahu_for_an_akahu_row() {
    let db = test_pool().await;
    let uid = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
        .bind(uid).bind(format!("bl-akahu-{uid}@test.example")).execute(&db).await.unwrap();
    let bid = Uuid::new_v4();
    sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
        .bind(bid).bind(uid).execute(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'akahu', 'acct_bl_akahu', 'user_tok_bl_akahu')")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
    let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl_akahu'")
        .fetch_one(&db).await.unwrap();

    // Akahu disconnect is NOT Pro-gated and makes zero external calls — this
    // must simply succeed and flip local status, proving dispatch reached
    // Akahu's implementation.
    disconnect_linked_account(&db, uid, bid, account_id).await.expect("akahu disconnect ok");
    let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(account_id).fetch_one(&db).await.unwrap();
    assert_eq!(status, "disconnected");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test bank_linking -- --ignored`
Expected: FAIL — `provider_of`'s `match` has no `"basiq"`/`"akahu"` arms yet, so both new tests hit the `other => Err(internal_error(...))` fallback instead of the expected status.

- [ ] **Step 3: Add the dispatch arms and REST handlers**

```rust
// backend/src/bank_linking.rs — replace the two existing dispatch functions
pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "basiq" => crate::basiq::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "akahu" => crate::akahu::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "basiq" => crate::basiq::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "akahu" => crate::akahu::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}
```

```rust
// backend/src/bank_linking.rs — add these REST handlers after the existing
// gc_start_link_handler/gc_complete_link_handler/gc_institutions_handler block
// (no institutions handler needed for Basiq/Akahu — spec Assumptions 2-3)

pub async fn basiq_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::basiq::BasiqLinkSessionResponse>, (StatusCode, String)> {
    crate::basiq::create_consent_session(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct BasiqCompleteLinkRequest {
    pub basiq_ref: Uuid,
}

pub async fn basiq_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<BasiqCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::basiq::complete_consent_session(&state.db, user_id, budget_id, req.basiq_ref).await.map(Json)
}

pub async fn akahu_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::akahu::AkahuLinkSessionResponse>, (StatusCode, String)> {
    crate::akahu::create_consent_session(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct AkahuCompleteLinkRequest {
    pub akahu_ref: Uuid,
    pub code: String,
}

pub async fn akahu_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<AkahuCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::akahu::complete_consent_session(&state.db, user_id, budget_id, req.akahu_ref, &req.code).await.map(Json)
}
```

- [ ] **Step 4: Run to verify the dispatch tests pass**

Run: `cd backend && cargo test bank_linking -- --ignored --test-threads=1`
Expected: PASS (both new dispatch tests, plus the existing GoCardless one).

- [ ] **Step 5: Wire routes and webhook routes into `main.rs`**

```rust
// backend/src/main.rs — protected_routes: add after the existing gocardless routes
        .route("/budgets/:id/basiq/session", post(bank_linking::basiq_start_link_handler))
        .route("/budgets/:id/basiq/complete", post(bank_linking::basiq_complete_link_handler))
        .route("/budgets/:id/akahu/session", post(bank_linking::akahu_start_link_handler))
        .route("/budgets/:id/akahu/complete", post(bank_linking::akahu_complete_link_handler))
```

```rust
// backend/src/main.rs — add a new public router block, mirroring
// financial_connections_public_routes, and nest it the same way
    // Basiq/Akahu webhooks (#323). PUBLIC — authenticated by HMAC signature,
    // NOT a session — mounted OUTSIDE the auth nest, same as Financial
    // Connections's webhook.
    let bank_webhook_public_routes = Router::new()
        .route("/basiq/webhook", post(basiq::webhook))
        .route("/akahu/webhook", post(akahu::webhook));
```

```rust
// backend/src/main.rs — add mod basiq; mod akahu; are already added in Tasks 2/3;
// add the new public route nest alongside the existing ones:
        .nest("/api", financial_connections_public_routes)
        .nest("/api", bank_webhook_public_routes)
```

- [ ] **Step 6: Add the new env vars to `.env.example`**

```bash
# backend/.env.example — append

# Basiq (Australia CDR bank-linking, #323). BASIQ_API_KEY is the server API
# key from the Basiq dashboard (base64'd application id:secret, used as
# `Authorization: Basic <key>` per Basiq's token endpoint). Optional — if
# unset, Basiq linking degrades to a clear "not configured" error.
BASIQ_API_KEY=
BASIQ_API_BASE=https://au-api.basiq.io
# HMAC secret used to verify Basiq webhook signatures (spec Assumption 6).
BASIQ_WEBHOOK_SECRET=

# Akahu (New Zealand bank-linking, #323). AKAHU_APP_TOKEN/AKAHU_APP_SECRET are
# the OAuth2 app credentials from the Akahu developer dashboard.
AKAHU_APP_TOKEN=
AKAHU_APP_SECRET=
AKAHU_API_BASE=https://api.akahu.io/v1
AKAHU_OAUTH_BASE=https://oauth.akahu.nz
# Shared-secret HMAC used to verify Akahu webhook signatures — NOT Akahu's
# real RSA scheme (documented gap, spec Assumption 6).
AKAHU_WEBHOOK_SECRET=
```

- [ ] **Step 7: Run the full backend test suite to confirm nothing else broke**

Run: `cd backend && cargo check && cargo test`
Expected: compiles clean; all non-`#[ignore]`d tests PASS.

- [ ] **Step 8: Commit**

```bash
git add backend/src/bank_linking.rs backend/src/main.rs backend/.env.example
git commit -m "feat(#323): wire Basiq/Akahu into bank_linking dispatch, routes, and webhooks"
```

---

## Task 5: `rag.rs` chat co-pilot integration

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Write the failing offline-router tests**

```rust
// backend/src/rag.rs — add inside the existing offline_country_hint tests block
#[test]
fn offline_country_hint_recognizes_australia() {
    assert_eq!(offline_country_hint("link my australian bank account"), Some("AU"));
    assert_eq!(offline_country_hint("link my bank account in australia"), Some("AU"));
}

#[test]
fn offline_country_hint_recognizes_new_zealand() {
    assert_eq!(offline_country_hint("link my new zealand bank account"), Some("NZ"));
    assert_eq!(offline_country_hint("i bank in nz"), Some("NZ"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test rag::tests::offline_country_hint_recognizes_australia rag::tests::offline_country_hint_recognizes_new_zealand`
Expected: FAIL — `offline_country_hint` doesn't recognize "australia"/"new zealand"/"nz" yet.

- [ ] **Step 3: Extend `offline_country_hint`**

```rust
// backend/src/rag.rs — locate offline_country_hint (~line 6133) and add two
// more branches BEFORE the final `else { None }`, using the SAME whole-word
// "nz" guard the existing "uk" check uses (to avoid a substring false-positive
// the same way "uk" is guarded, e.g. a message containing "franz" must not
// match "nz")
    } else if msg_lower.contains("australia") || msg_lower.contains("australian") {
        Some("AU")
    } else if msg_lower.contains("new zealand") || has_word("nz") {
        Some("NZ")
```

Note: `has_word` is already defined earlier in this same function (the `tokens`/`has_word` closure used for the `"uk"` check) — reuse it, do not redefine.

- [ ] **Step 4: Run to verify it passes**

Run: `cd backend && cargo test rag::tests::offline_country_hint`
Expected: PASS (all `offline_country_hint_*` tests, including the two new ones).

- [ ] **Step 5: Extend the `LINK_BANK_ACCOUNT` match arm with Basiq/Akahu cases**

```rust
// backend/src/rag.rs — locate the "LINK_BANK_ACCOUNT" match arm (~line 3026)
// and add two more inner match arms alongside the existing Stripe/GoCardless
// ones, BEFORE the closing brace of the `match country.as_deref()...` block
                    Some(Some(crate::bank_provider::Provider::Basiq)) => {
                        // Pro-gate BEFORE any Basiq API call — mirrors the
                        // GoCardless arm's require_pro pre-check rationale
                        // exactly (create_consent_session Pro-gates
                        // internally too, but only after already making a
                        // Basiq API call to create the user).
                        match crate::basiq::require_pro(&state.db, user_id).await {
                            Err((_, msg)) => mutation_error = Some(msg),
                            Ok(()) => {
                                match crate::basiq::create_consent_session(&state.db, user_id, bid).await {
                                    Ok(session) => bank_link_redirect_url = Some(session.redirect_url),
                                    Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                                    Err((_, msg)) => mutation_error = Some(msg),
                                }
                            }
                        }
                    }
                    Some(Some(crate::bank_provider::Provider::Akahu)) => {
                        match crate::akahu::require_pro(&state.db, user_id).await {
                            Err((_, msg)) => mutation_error = Some(msg),
                            Ok(()) => {
                                match crate::akahu::create_consent_session(&state.db, user_id, bid).await {
                                    Ok(session) => bank_link_redirect_url = Some(session.redirect_url),
                                    Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                                    Err((_, msg)) => mutation_error = Some(msg),
                                }
                            }
                        }
                    }
```

- [ ] **Step 6: Extend the LLM system prompt's supported-country list (~line 1425)**

```rust
// backend/src/rag.rs — in the system prompt string, replace:
//   "currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless)."
// with:
"currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless), Australia (Basiq), New Zealand (Akahu)."
```

Keep the rest of that same prompt paragraph (rule "2o") unchanged — only the country-list clause changes; the "you must first know which country" / "populate institution_query" instructions still apply to GoCardless only, since Basiq/Akahu never need an `institution_query` (spec Assumptions 2-3) — add one clause clarifying that:

```
For Australia (Basiq) and New Zealand (Akahu), no bank/institution name is needed — the user picks their bank on the provider's own hosted consent page, so proceed with just the country code.
```

- [ ] **Step 7: Run the full `rag` test suite**

Run: `cd backend && cargo test rag::`
Expected: PASS (existing tests unaffected; new `offline_country_hint_recognizes_*` tests pass).

- [ ] **Step 8: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#323): wire Basiq/Akahu into the LINK_BANK_ACCOUNT chat action"
```

---

## Task 6: Frontend — country picker, redirect handling

**Files:**
- Modify: `frontend/src/lib/linkedAccounts.js`
- Modify: `frontend/src/lib/LinkedAccounts.svelte`
- Modify: `frontend/src/App.svelte`
- Modify: `frontend/src/lib/linkedAccounts.test.js`
- Modify (i18n): locate the file defining `linkedAccounts.connectViaGoCardless`/`linkedAccounts.gcCompleteSuccess` (run `grep -rln "connectViaGoCardless" frontend/src/` first) and add the Basiq/Akahu equivalents there.

- [ ] **Step 1: Write the failing pure-function tests**

```javascript
// frontend/src/lib/linkedAccounts.test.js — add, mirroring the existing
// fetchGcInstitutions/startGcLinkFlow/completeGcLinkFlow test style (read
// that file first to match its exact mocking pattern for fetchApi)
import { startBasiqLinkFlow, completeBasiqLinkFlow, startAkahuLinkFlow, completeAkahuLinkFlow } from "./linkedAccounts.js";

describe("Basiq link flow", () => {
  it("posts to the basiq session endpoint and returns redirect_url/reference", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://consent.basiq.io/x", reference: "ref-1" });
    const result = await startBasiqLinkFlow({ budgetId: "b1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/basiq/session", { method: "POST" });
    expect(result.redirect_url).toBe("https://consent.basiq.io/x");
  });

  it("completes with the basiq_ref", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "basiq" }] });
    const result = await completeBasiqLinkFlow({ budgetId: "b1", basiqRef: "ref-1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/basiq/complete", {
      method: "POST",
      body: JSON.stringify({ basiq_ref: "ref-1" }),
    });
    expect(result.linked).toHaveLength(1);
  });
});

describe("Akahu link flow", () => {
  it("posts to the akahu session endpoint", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://oauth.akahu.nz/authorize?x", reference: "ref-2" });
    const result = await startAkahuLinkFlow({ budgetId: "b1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/akahu/session", { method: "POST" });
    expect(result.redirect_url).toContain("oauth.akahu.nz");
  });

  it("completes with the akahu_ref and code", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "akahu" }] });
    const result = await completeAkahuLinkFlow({ budgetId: "b1", akahuRef: "ref-2", code: "auth_code", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/akahu/complete", {
      method: "POST",
      body: JSON.stringify({ akahu_ref: "ref-2", code: "auth_code" }),
    });
    expect(result.linked).toHaveLength(1);
  });
});
```

- [ ] **Step 2: Run to verify these fail**

Run: `cd frontend && pnpm test -- linkedAccounts` (check `frontend/package.json`'s actual test script name first via `grep '"test"' frontend/package.json` — use whatever it names, e.g. `pnpm vitest run linkedAccounts`)
Expected: FAIL — `startBasiqLinkFlow`/`startAkahuLinkFlow`/`completeAkahuLinkFlow` are not exported yet.

- [ ] **Step 3: Add the Basiq/Akahu helpers to `linkedAccounts.js`**

```javascript
// frontend/src/lib/linkedAccounts.js — append after the existing GoCardless section

// --- Basiq (Australia, #323) ---
// Basiq's consent flow is fully hosted ("Basiq Connect") — Nels never picks
// an institution on its own side (spec Assumption 2), so there is no
// institutions-fetch helper here, unlike GoCardless's fetchGcInstitutions.

/** Start a Basiq consent flow: create the session server-side and return its redirect_url. */
export async function startBasiqLinkFlow({ budgetId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/basiq/session`, { method: "POST" });
}

/** Complete a Basiq consent flow after the user returns from Basiq's hosted page. */
export async function completeBasiqLinkFlow({ budgetId, basiqRef, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/basiq/complete`, {
    method: "POST",
    body: JSON.stringify({ basiq_ref: basiqRef }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

// --- Akahu (New Zealand, #323) ---
// Also fully hosted ("Akahu Connect", OAuth2) — no institution picker needed.

/** Start an Akahu consent flow: return its hosted authorize redirect_url. */
export async function startAkahuLinkFlow({ budgetId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/akahu/session`, { method: "POST" });
}

/** Complete an Akahu consent flow: exchange the returned OAuth `code` for accounts. */
export async function completeAkahuLinkFlow({ budgetId, akahuRef, code, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/akahu/complete`, {
    method: "POST",
    body: JSON.stringify({ akahu_ref: akahuRef, code }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}
```

- [ ] **Step 4: Run to verify the new tests pass**

Run: `cd frontend && pnpm test -- linkedAccounts` (or the project's actual vitest invocation)
Expected: PASS.

- [ ] **Step 5: Extend `LinkedAccounts.svelte`'s country picker**

```svelte
<!-- frontend/src/lib/LinkedAccounts.svelte — replace the import line to add
     the new helpers -->
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
    fetchGcInstitutions, startGcLinkFlow, isConsentExpired,
    startBasiqLinkFlow, startAkahuLinkFlow,
  } from "./linkedAccounts.js";
```

```svelte
<!-- frontend/src/lib/LinkedAccounts.svelte — replace the link() function -->
  async function link() {
    error = "";
    if (GC_COUNTRIES.includes(selectedCountry)) {
      if (!selectedInstitutionId) return;
      try {
        const session = await startGcLinkFlow({
          budgetId, country: selectedCountry, institutionId: selectedInstitutionId, fetchApi,
        });
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    if (selectedCountry === "AU") {
      try {
        const session = await startBasiqLinkFlow({ budgetId, fetchApi });
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    if (selectedCountry === "NZ") {
      try {
        const session = await startAkahuLinkFlow({ budgetId, fetchApi });
        window.location.href = session.redirect_url;
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    try {
      const { linked } = await startLinkFlow({
        budgetId,
        fetchApi,
        loadStripe,
        publishableKey: import.meta.env.VITE_STRIPE_PUBLISHABLE_KEY,
      });
      if (linked?.length) await load();
    } catch (e) {
      if (isProGateError(e)) {
        error = $_("linkedAccounts.linkProGate");
      } else {
        error = e.message || $_("linkedAccounts.linkError");
      }
    }
  }
```

```svelte
<!-- frontend/src/lib/LinkedAccounts.svelte — in onCountryChange(), the early
     return for non-GoCardless countries is unaffected (Basiq/Akahu have no
     institutions list, same as US/Stripe) since it already just clears
     institutions and returns for anything not in GC_COUNTRIES. No change
     needed there. -->

<!-- Add Australia/New Zealand <option>s to the <select>, after Norway -->
        <option value="NO">Norway</option>
        <option value="AU">Australia</option>
        <option value="NZ">New Zealand</option>
      </select>
```

```svelte
<!-- Update the button label logic to cover all three non-modal flows -->
      <button class="btn btn-primary btn-sm" onclick={link}>
        {GC_COUNTRIES.includes(selectedCountry)
          ? $_("linkedAccounts.connectViaGoCardless")
          : selectedCountry === "AU"
            ? $_("linkedAccounts.connectViaBasiq")
            : selectedCountry === "NZ"
              ? $_("linkedAccounts.connectViaAkahu")
              : $_("linkedAccounts.link")}
      </button>
```

- [ ] **Step 6: Add the new i18n keys**

Run: `grep -rln "connectViaGoCardless" frontend/src/` to find the exact locale file(s), then add `connectViaBasiq`/`connectViaAkahu` keys with the same English copy pattern as `connectViaGoCardless` (e.g. `"Connect via Basiq"` / `"Connect via Akahu"`) to every locale file that already defines `connectViaGoCardless` (mirror ALL locales present, not just `en`, to avoid a raw-key fallback in other languages).

- [ ] **Step 7: Add the query-param completion handlers to `App.svelte`**

```javascript
// frontend/src/App.svelte — add two new functions, mirroring
// handleGoCardlessQueryParams exactly, right after it
  async function handleBasiqQueryParams() {
    const basiqRef = new URLSearchParams(window.location.search).get("basiq_ref");
    if (!basiqRef) return;
    window.history.replaceState({}, "", window.location.pathname);
    if (!token) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await completeBasiqLinkFlow({ budgetId: activeBudget.id, basiqRef, fetchApi });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("basiq complete failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }

  async function handleAkahuQueryParams() {
    const params = new URLSearchParams(window.location.search);
    const akahuRef = params.get("akahu_ref");
    const code = params.get("code");
    if (!akahuRef) return;
    window.history.replaceState({}, "", window.location.pathname);
    if (!code) {
      // The user reached our redirect_uri without a `code` — Akahu's
      // hosted flow was abandoned or denied; nothing to complete.
      return;
    }
    if (!token) return;
    if (!activeBudget) await fetchBudgets();
    if (!activeBudget) return;
    try {
      const { linked } = await completeAkahuLinkFlow({ budgetId: activeBudget.id, akahuRef, code, fetchApi });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("akahu complete failed", e);
      triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
    }
  }
```

```javascript
// frontend/src/App.svelte — update the import line adding completeBasiqLinkFlow/completeAkahuLinkFlow
// (find the existing `import { ..., completeGcLinkFlow } from "./lib/linkedAccounts.js";` line and extend it)
```

```javascript
// frontend/src/App.svelte — locate the onMount call site that invokes
// handleGoCardlessQueryParams() (~line 1676) and call the two new handlers
// alongside it
    handleGoCardlessQueryParams();
    handleBasiqQueryParams();
    handleAkahuQueryParams();
```

- [ ] **Step 8: Run the frontend test suite**

Run: `cd frontend && pnpm test` (or the project's actual test command)
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add frontend/src/lib/linkedAccounts.js frontend/src/lib/LinkedAccounts.svelte frontend/src/App.svelte frontend/src/lib/linkedAccounts.test.js
git commit -m "feat(#323): add Basiq/Akahu country picker and redirect-return handling"
```

(A separate commit for the i18n locale files, since Step 6 touches however many locale files exist:)

```bash
git add -A -- '*locale*' '*i18n*' 2>/dev/null || true
git status --short   # confirm exactly the locale files touched in Step 6 are staged, nothing else
git commit -m "feat(#323): add Basiq/Akahu i18n strings"
```

---

## Task 7: Documentation + final integration pass

**Files:**
- Modify: `AGENTS.md` (if it documents the bank-linking providers/env vars — check first)
- Modify: `backend/CHANGELOG.md` / `frontend/CHANGELOG.md` if the repo's `release-please` convention expects a manual entry (check `release-please-config.json` and a recent CHANGELOG.md entry to see if these are auto-generated from commit messages — if so, skip manual edits here to avoid double-entries)

- [ ] **Step 1: Check whether `AGENTS.md` documents the bank-provider list/env vars**

Run: `grep -n "GoCardless\|Stripe Financial Connections\|bank.provider\|BankProvider" AGENTS.md | head -20`
If `AGENTS.md` has a section listing supported providers/countries (e.g. the §15 enum-vs-trait note, or a "supported bank providers" list), extend it with Basiq (Australia) / Akahu (New Zealand). If no such list exists beyond the §15 architectural note, skip this step (no user-facing doc surface to update).

- [ ] **Step 2: Confirm `CHANGELOG.md` is auto-generated, not manually edited**

Run: `head -20 backend/CHANGELOG.md && cat release-please-config.json`
Expected: confirms `release-please` generates changelog entries from conventional-commit messages on release — if so, no manual CHANGELOG edit is needed (the `feat(#323): ...` commit messages from Tasks 1-6 already drive this).

- [ ] **Step 3: Run the full backend + frontend test suites one final time**

Run: `cd backend && cargo check && cargo test && cargo test -- --ignored --test-threads=1 2>&1 | tail -60` (the `--ignored` run requires a local Postgres — `podman-compose up -d` first per `AGENTS.md`)
Run: `cd frontend && pnpm test`
Expected: all PASS, `cargo check` clean.

- [ ] **Step 4: Final commit (only if Steps 1-2 produced changes)**

```bash
git add AGENTS.md 2>/dev/null
git commit -m "docs(#323): document Basiq/Akahu bank-linking support" --allow-empty-message 2>/dev/null || true
```

(Skip this commit entirely if Steps 1-2 found nothing to change — do not create an empty/no-op commit.)
