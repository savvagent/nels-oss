# GoCardless Bank Account Data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Pro subscribers in the UK, France, Germany, Italy, Spain, Denmark, Finland, and Norway link bank accounts via GoCardless Bank Account Data, with transactions importing into the existing `transactions` table (currency-tagged, no FX), auto-refresh via a scheduled poll (GoCardless has no transaction webhooks), explicit PSD2 ~90-day consent-expiry handling, and view/disconnect + chat reachability matching #303's Stripe experience — while generalizing `linked_accounts`/`transactions` (`provider`, `provider_account_id`, etc.) and introducing a shared `bank_linking.rs` dispatch layer so `rag.rs`, REST handlers, and the frontend treat both providers uniformly except when starting a brand-new link.

**Architecture:** One migration generalizes the #303 schema and adds `bank_link_sessions`. `backend/src/bank_provider.rs` holds the `Provider` enum + country mapping. `backend/src/gocardless.rs` owns the GoCardless HTTP integration (JSON, bearer-token-cached), mirroring `financial_connections.rs`'s existing "own client, own env test seam" convention. `backend/src/bank_linking.rs` is a thin provider-dispatch layer so callers (`rag.rs`, REST handlers) stop calling `financial_connections::*` directly for list/refresh/disconnect/sync. A new scheduled poll job (mirrors `main.rs`'s existing hourly ticker) replaces the webhook GoCardless doesn't have.

**Tech Stack:** Rust/axum/sqlx/reqwest (backend), Svelte 5 (frontend), Postgres. Full spec: `docs/superpowers/specs/2026-07-06-gocardless-bank-account-data-design.md`.

---

## Task 1: Schema generalization + mechanical Stripe rename (behavior-preserving)

**Files:**
- Create: `backend/migrations/20260706120000_gocardless_bank_account_data.sql`
- Modify: `backend/src/db.rs:239-255` (`LinkedAccount` struct)
- Modify: `backend/src/financial_connections.rs` (field/column renames only — `stripe_account_id`→`provider_account_id`, `stripe_customer_id`→`provider_ref`, `stripe_transaction_id`→`provider_transaction_id`, `ON CONFLICT` target)
- Modify: `backend/src/rag.rs` (test-fixture raw SQL that inserts into `linked_accounts`, if any references old column names)
- Test: existing `backend/src/financial_connections.rs` `#[cfg(test)]` module (must compile+pass unchanged in behavior after the rename)

- [ ] **Step 1: Confirm no other file references the old column/field names before renaming**

Run: `cd backend && grep -rn "stripe_account_id\|stripe_customer_id\|stripe_transaction_id" src/`
Expected: only hits in `financial_connections.rs`, `db.rs`, and possibly `rag.rs` test fixtures — note every hit so Step 4 renames all of them.

- [ ] **Step 2: Write the migration**

```sql
-- backend/migrations/20260706120000_gocardless_bank_account_data.sql
-- Generalizes #303's linked_accounts/transactions schema to support a second
-- provider (GoCardless Bank Account Data, nels#320) alongside Stripe, and adds
-- the pending-session table GoCardless's redirect-based consent flow needs
-- (Stripe never needs one — its client_secret round-trips via Stripe's own
-- servers). See docs/superpowers/specs/2026-07-06-gocardless-bank-account-data-design.md
-- Assumptions 4 and 9 for the full rationale.

ALTER TABLE linked_accounts ADD COLUMN provider TEXT NOT NULL DEFAULT 'stripe'
    CHECK (provider IN ('stripe', 'gocardless'));

ALTER TABLE linked_accounts RENAME COLUMN stripe_account_id TO provider_account_id;
ALTER INDEX linked_accounts_stripe_account_id_key RENAME TO linked_accounts_provider_account_id_key;

ALTER TABLE linked_accounts RENAME COLUMN stripe_customer_id TO provider_ref;

ALTER TABLE linked_accounts ADD COLUMN consent_expires_at TIMESTAMPTZ;
ALTER TABLE linked_accounts ADD COLUMN country TEXT;
ALTER TABLE linked_accounts ADD COLUMN institution_id TEXT;

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_status_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_status_check
    CHECK (status IN ('active', 'disconnected', 'consent_expired'));

ALTER TABLE transactions RENAME COLUMN stripe_transaction_id TO provider_transaction_id;
ALTER TABLE transactions ADD COLUMN currency TEXT;

-- The old index enforced global uniqueness of stripe_transaction_id alone —
-- correct for Stripe's globally-unique fctxn_... ids, NOT safe to assume for
-- GoCardless's bank-assigned transaction ids. Replace with a composite index
-- scoped per linked account.
DROP INDEX transactions_stripe_transaction_id_idx;
CREATE UNIQUE INDEX transactions_external_account_provider_tx_idx
    ON transactions (external_account_id, provider_transaction_id)
    WHERE provider_transaction_id IS NOT NULL;

-- One row per pending GoCardless consent attempt (nels#320 Assumption 4). The
-- `id` doubles as the `gc_ref` query param embedded in the redirect URL WE
-- construct (see gocardless.rs::start_link_session) — GoCardless redirects
-- verbatim to that URL, appending nothing of its own.
CREATE TABLE IF NOT EXISTS bank_link_sessions (
    id              UUID PRIMARY KEY,
    budget_id       UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    requisition_id  TEXT NOT NULL,
    agreement_id    TEXT,
    institution_id  TEXT NOT NULL,
    country         TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS bank_link_sessions_budget_id_idx ON bank_link_sessions (budget_id);
```

- [ ] **Step 2b: Find the exact existing constraint/index names before writing DROP/RENAME statements**

Run: `cd backend && grep -n "CONSTRAINT\|UNIQUE\|CREATE INDEX" migrations/20260706000000_financial_connections.sql`
Expected output confirms: the `status` CHECK is unnamed inline (`CHECK (status IN ('active', 'disconnected'))` on the column) — Postgres auto-names inline column CHECKs `linked_accounts_status_check` by convention, but CONFIRM this by running the migration against a scratch DB and querying `\d linked_accounts` (or `SELECT conname FROM pg_constraint WHERE conrelid = 'linked_accounts'::regclass;`) before trusting the DROP CONSTRAINT name in Step 2 — adjust the migration to match whatever name Postgres actually assigned if it differs. Same check for the `stripe_account_id` UNIQUE constraint's actual index name (it's declared as a column-level `UNIQUE` in the CREATE TABLE, so Postgres names it `linked_accounts_stripe_account_id_key` by convention — verify, don't assume).

- [ ] **Step 3: Run the migration against a scratch DB and verify it applies cleanly on top of #303's**

Run: `cd backend && cargo test financial_connections:: -- --ignored --test-threads=1 2>&1 | head -50`
Expected: migration runs (via `sqlx::migrate!` in the test pool setup) with no SQL errors. If a constraint/index name from Step 2b was wrong, fix the migration file and re-run.

- [ ] **Step 4: Update `db.rs`'s `LinkedAccount` struct**

```rust
// backend/src/db.rs — replace the existing LinkedAccount struct (lines 239-255)
#[derive(sqlx::FromRow, Debug, Clone)]
pub struct LinkedAccount {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub user_id: Uuid,
    pub provider: String, // 'stripe' | 'gocardless'
    pub provider_account_id: String,
    pub provider_ref: String,
    pub institution_name: Option<String>,
    pub institution_id: Option<String>,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub status: String, // 'active' | 'disconnected' | 'consent_expired'
    pub consent_expires_at: Option<DateTime<Utc>>,
    pub country: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Check the struct already derives `sqlx::FromRow` above line 239 (it's used via `query_as::<_, crate::db::LinkedAccount>` in `financial_connections.rs`) — keep whatever derive line already exists, just replace the field list.

- [ ] **Step 5: Rename every reference in `financial_connections.rs`**

Run: `cd backend && grep -n "stripe_account_id\|stripe_customer_id\|stripe_transaction_id" src/financial_connections.rs`

For each hit, apply the rename:
- `stripe_account_id` → `provider_account_id` (both the Rust field access `row.stripe_account_id` → `row.provider_account_id`/`linked_account.stripe_account_id` → `linked_account.provider_account_id`, and every SQL string literal column name in `INSERT`/`SELECT`/`WHERE` clauses)
- `stripe_customer_id` → `provider_ref` (same, both Rust and SQL)
- `stripe_transaction_id` → `provider_transaction_id` (same, both Rust and SQL)

In `complete_link_session`'s INSERT (around line 275-299), add `provider` to the column list and bind `"stripe"`:
```rust
let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
    "INSERT INTO linked_accounts \
        (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
         institution_name, display_name, last4, category, subcategory) \
     VALUES ($1, $2, $3, 'stripe', $4, $5, $6, $7, $8, $9, $10) \
     ON CONFLICT (provider_account_id) DO UPDATE SET \
        institution_name = EXCLUDED.institution_name, \
        display_name = EXCLUDED.display_name, \
        last4 = EXCLUDED.last4, \
        category = EXCLUDED.category, \
        subcategory = EXCLUDED.subcategory, \
        status = 'active', disconnected_at = NULL, updated_at = now() \
     RETURNING *"
)
.bind(Uuid::new_v4())
.bind(budget_id)
.bind(user_id)
.bind(&stripe_account_id)
.bind(&my_customer_id)
.bind(&institution_name)
.bind(&display_name)
.bind(&last4)
.bind(&category)
.bind(&subcategory)
.fetch_one(pool).await.map_err(internal_error)?;
```

In `sync_account_transactions`'s INSERT (around line 403-418), rename the column and set `currency` explicitly, and change the `ON CONFLICT` target to the new composite index:
```rust
let res = sqlx::query(
    "INSERT INTO transactions \
        (id, budget_id, category_id, amount, transaction_date, description, \
         external_account_id, provider_transaction_id, currency) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'USD') \
     ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
)
.bind(Uuid::new_v4())
.bind(linked_account.budget_id)
.bind(category_id)
.bind(amount)
.bind(transacted_at)
.bind(&description)
.bind(linked_account.id)
.bind(&stripe_tx_id)
.execute(pool).await.map_err(internal_error)?;
```
(Stripe FC in this codebase is US-only, so hardcoding `'USD'` here is a correct, not guessed, value — see spec Assumption 9.)

Also rename `LinkedAccountResponse`'s construction (the `From<crate::db::LinkedAccount>` impl around line 179-190) to read the renamed struct fields, and add a `provider: String` field to `LinkedAccountResponse` itself (surfaced to REST/chat callers so a generalized UI can show which provider an account came from):
```rust
#[derive(Debug, Serialize)]
pub struct LinkedAccountResponse {
    pub id: Uuid,
    pub provider: String,
    pub institution_name: Option<String>,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub status: String,
    pub last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<crate::db::LinkedAccount> for LinkedAccountResponse {
    fn from(a: crate::db::LinkedAccount) -> Self {
        LinkedAccountResponse {
            id: a.id,
            provider: a.provider,
            institution_name: a.institution_name,
            display_name: a.display_name,
            last4: a.last4,
            status: a.status,
            last_synced_at: a.last_synced_at,
        }
    }
}
```

- [ ] **Step 6: Update the test module's helper functions in `financial_connections.rs`**

`mk_linked_account` (around line 832-838) must bind `provider` too:
```rust
async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, stripe_account_id: &str) -> crate::db::LinkedAccount {
    sqlx::query_as::<_, crate::db::LinkedAccount>(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'stripe', $4, 'cus_x') RETURNING *")
        .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(stripe_account_id)
        .fetch_one(db).await.unwrap()
}
```
Every other test in the module that does `sqlx::query_scalar` reads against `stripe_account_id`/`stripe_customer_id`/`stripe_transaction_id` columns (e.g. `complete_link_session_rejects_relink_to_a_different_budget`'s `SELECT ... FROM linked_accounts` assertions, and `sync_account_transactions_is_idempotent_on_redelivery`'s `SELECT amount FROM transactions WHERE stripe_transaction_id = ...`) must have those column names renamed too.

- [ ] **Step 7: Search `rag.rs` for any raw-SQL `linked_accounts` test seeding**

Run: `cd backend && grep -n "INSERT INTO linked_accounts\|stripe_account_id\|stripe_customer_id" src/rag.rs`
If any hits exist, rename them the same way as Step 5/6. (If none — `rag.rs`'s existing bank-linking tests may seed via `financial_connections.rs`'s own test helpers instead — note "no hits, nothing to change" and move on.)

- [ ] **Step 8: Run the full existing test suite to confirm behavior is unchanged**

Run: `cd backend && cargo test financial_connections`
Expected: all unit tests (the ones NOT requiring `--ignored`) pass.

Run: `cd backend && cargo test -- --ignored --test-threads=4 2>&1 | tail -40`
Expected: the `financial_connections::tests::*` `#[ignore]` DB-integration tests all still pass (same count as before the rename — this is the "behavior-preserving" check the ticket requires).

- [ ] **Step 9: Commit**

```bash
git add backend/migrations/20260706120000_gocardless_bank_account_data.sql backend/src/db.rs backend/src/financial_connections.rs backend/src/rag.rs
git commit -m "refactor(#320): generalize linked_accounts/transactions schema for a second bank provider"
```

---

## Task 2: `bank_provider.rs` — the `Provider` enum

**Files:**
- Create: `backend/src/bank_provider.rs`
- Modify: `backend/src/main.rs` (add `mod bank_provider;`)

- [ ] **Step 1: Write the failing tests**

```rust
// backend/src/bank_provider.rs (top of file, before the impl)
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
        for p in [Provider::Stripe, Provider::GoCardless] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Provider::parse("plaid"), None);
    }

    #[test]
    fn supported_countries_lists_all_nine() {
        assert_eq!(Provider::supported_countries().len(), 9);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test bank_provider`
Expected: FAIL — `bank_provider` module doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
//! Shared bank-provider abstraction (nels#320): a closed, two-member set
//! (Stripe Financial Connections, GoCardless Bank Account Data), so enum
//! dispatch is used instead of an `async-trait`+`dyn` object — see the
//! spec's Assumption 8 for the tradeoff rationale.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    GoCardless,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::GoCardless => "gocardless",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "stripe" => Some(Provider::Stripe),
            "gocardless" => Some(Provider::GoCardless),
            _ => None,
        }
    }

    /// Map an ISO 3166-1 alpha-2 country code (case-insensitive) to the
    /// provider that covers it. `US` -> Stripe; the 8 GoCardless launch
    /// countries (nels#320) -> GoCardless; anything else -> None (caller
    /// must ask the user for a supported country, never guess).
    pub fn for_country(cc: &str) -> Option<Provider> {
        match cc.to_uppercase().as_str() {
            "US" => Some(Provider::Stripe),
            "GB" | "FR" | "DE" | "IT" | "ES" | "DK" | "FI" | "NO" => Some(Provider::GoCardless),
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
        ]
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd backend && cargo test bank_provider`
Expected: PASS, 7 tests.

- [ ] **Step 5: Register the module**

In `backend/src/main.rs`, near the other `mod` declarations (line ~31, alongside `mod financial_connections;`), add:
```rust
mod bank_provider;
```

- [ ] **Step 6: Run `cargo check` to confirm the whole crate still builds**

Run: `cd backend && cargo check`
Expected: no errors (an unused-module warning is fine at this point — Task 3/4 will consume it).

- [ ] **Step 7: Commit**

```bash
git add backend/src/bank_provider.rs backend/src/main.rs
git commit -m "feat(#320): add Provider enum and country-to-provider mapping"
```

---

## Task 3: `gocardless.rs` — the GoCardless integration

**Files:**
- Create: `backend/src/gocardless.rs`
- Modify: `backend/src/main.rs` (add `mod gocardless;`)
- Modify: `backend/Cargo.toml` (no new dependencies needed — `reqwest`, `wiremock`, `serde_json`, `tokio` all already present)

This is the largest task. Build it in the same "pure helpers first, HTTP/DB functions after" order `financial_connections.rs` uses, so each piece is independently testable before wiring the next.

- [ ] **Step 1: Write failing tests for the pure helpers (amount normalization, consent-expiry detection, institution fuzzy-match)**

```rust
// backend/src/gocardless.rs (bottom of file, #[cfg(test)] mod tests, unit-test section)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_amount_parses_decimal_string() {
        assert_eq!(normalize_amount("15.00"), Some(15.0));
    }

    #[test]
    fn normalize_amount_takes_absolute_value() {
        assert_eq!(normalize_amount("-15.00"), Some(15.0));
        assert_eq!(normalize_amount("15.00"), normalize_amount("-15.00"));
    }

    #[test]
    fn normalize_amount_rejects_malformed_string() {
        assert_eq!(normalize_amount("not-a-number"), None);
        assert_eq!(normalize_amount(""), None);
    }

    #[test]
    fn is_consent_expired_error_detects_409_expired() {
        let body = serde_json::json!({"summary": "Access expired", "status_code": 409});
        assert!(is_consent_expired_error(reqwest::StatusCode::CONFLICT, &body));
    }

    #[test]
    fn is_consent_expired_error_detects_suspended() {
        let body = serde_json::json!({"detail": "Account access has been SUSPENDED by the institution"});
        assert!(is_consent_expired_error(reqwest::StatusCode::FORBIDDEN, &body));
    }

    #[test]
    fn is_consent_expired_error_ignores_unrelated_errors() {
        let body = serde_json::json!({"summary": "Rate limit exceeded"});
        assert!(!is_consent_expired_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &body));
        let body2 = serde_json::json!({"summary": "Not found"});
        assert!(!is_consent_expired_error(reqwest::StatusCode::NOT_FOUND, &body2));
    }

    fn inst(id: &str, name: &str) -> Institution {
        Institution { id: id.to_string(), name: name.to_string(), transaction_total_days: 90 }
    }

    #[test]
    fn find_institution_id_matches_case_insensitive_substring() {
        let insts = vec![inst("MONZO_MONZ_GB", "Monzo"), inst("REVOLUT_REVO_GB", "Revolut")];
        assert_eq!(find_institution_id_in(&insts, "monzo"), Ok("MONZO_MONZ_GB".to_string()));
    }

    #[test]
    fn find_institution_id_no_match_is_err() {
        let insts = vec![inst("MONZO_MONZ_GB", "Monzo")];
        assert!(find_institution_id_in(&insts, "chase").is_err());
    }

    #[test]
    fn find_institution_id_ambiguous_match_is_err() {
        let insts = vec![inst("BARCLAYS_A_GB", "Barclays"), inst("BARCLAYS_B_GB", "Barclays Business")];
        assert!(find_institution_id_in(&insts, "barclays").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test gocardless`
Expected: FAIL to compile — `gocardless` module doesn't exist yet.

- [ ] **Step 3: Implement the module skeleton, env/client setup, and token caching**

```rust
//! GoCardless Bank Account Data integration (nels#320): redirect-based
//! consent (End User Agreement + Requisition), transaction sync, PSD2
//! consent-expiry detection, and IBAN-based reconciliation on re-consent.
//! Mirrors `financial_connections.rs`'s "own HTTP client, own env test seam"
//! convention, but JSON-bodied (GoCardless's API, unlike Stripe's, is JSON
//! not form-encoded) and needs a cached bearer token instead of a static
//! secret key (Assumption 2 of the spec).

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
fn gocardless_api_base() -> String {
    env_opt("GOCARDLESS_API_BASE").unwrap_or_else(|| "https://bankaccountdata.gocardless.com/api/v2".to_string())
}
fn app_url() -> String {
    env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

struct CachedToken {
    access: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

static TOKEN: tokio::sync::RwLock<Option<CachedToken>> = tokio::sync::RwLock::const_new(None);

/// Fetch (or reuse) a bearer access token. A fixed 60s safety margin before
/// `access_expires` guards against a token expiring mid-request.
async fn access_token() -> Result<String, (StatusCode, String)> {
    {
        let guard = TOKEN.read().await;
        if let Some(t) = guard.as_ref() {
            if t.expires_at > chrono::Utc::now() {
                return Ok(t.access.clone());
            }
        }
    }
    let secret_id = env_opt("GOCARDLESS_SECRET_ID")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "GoCardless is not configured".to_string()))?;
    let secret_key = env_opt("GOCARDLESS_SECRET_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "GoCardless is not configured".to_string()))?;
    let url = format!("{}/token/new/", gocardless_api_base().trim_end_matches('/'));
    let resp = http_client().post(&url)
        .json(&serde_json::json!({"secret_id": secret_id, "secret_key": secret_key}))
        .send().await.map_err(|e| internal_error(format!("gocardless token POST: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("gocardless token decode: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "gocardless token error");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    let access = body.get("access").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("gocardless token response missing access"))?.to_string();
    let expires_in = body.get("access_expires").and_then(|v| v.as_i64()).unwrap_or(3600);
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in) - chrono::Duration::seconds(60);
    let mut guard = TOKEN.write().await;
    *guard = Some(CachedToken { access: access.clone(), expires_at });
    Ok(access)
}

/// GET the GoCardless API with a cached bearer token. Returns the decoded
/// body AND the status on error paths that need to inspect the body even on
/// failure (consent-expiry detection reads the error body's contents).
async fn gocardless_get(path: &str) -> Result<serde_json::Value, (StatusCode, String)> {
    gocardless_get_raw(path).await.and_then(|(status, body)| {
        if status.is_success() { Ok(body) } else {
            tracing::error!(?status, ?body, "gocardless API error on {path}");
            Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()))
        }
    })
}

async fn gocardless_get_raw(path: &str) -> Result<(StatusCode, serde_json::Value), (StatusCode, String)> {
    let token = access_token().await?;
    let url = format!("{}/{}", gocardless_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().get(&url).bearer_auth(token).send().await
        .map_err(|e| internal_error(format!("gocardless GET {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("gocardless decode {path}: {e}")))?;
    Ok((status, body))
}

async fn gocardless_post(path: &str, json_body: &serde_json::Value) -> Result<serde_json::Value, (StatusCode, String)> {
    let token = access_token().await?;
    let url = format!("{}/{}", gocardless_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().post(&url).bearer_auth(token).json(json_body).send().await
        .map_err(|e| internal_error(format!("gocardless POST {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("gocardless decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "gocardless API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

async fn gocardless_delete(path: &str) -> Result<(), (StatusCode, String)> {
    let token = access_token().await?;
    let url = format!("{}/{}", gocardless_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().delete(&url).bearer_auth(token).send().await
        .map_err(|e| internal_error(format!("gocardless DELETE {path}: {e}")))?;
    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        tracing::error!(?status, ?body, "gocardless API error on {path}");
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

async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}
```

- [ ] **Step 4: Implement the pure helpers (amount normalization, consent-expiry detection, institution matching)**

```rust
/// GoCardless amounts are decimal strings (e.g. "-15.00"), unlike Stripe's
/// integer cents — a separate, independently-tested normalization function
/// (spec Assumption 20). Returns None on a malformed string rather than
/// panicking; callers treat None as "skip this transaction's amount as 0.0"
/// (mirrors the description fallback pattern in financial_connections.rs).
pub(crate) fn normalize_amount(amount_str: &str) -> Option<f64> {
    amount_str.parse::<f64>().ok().map(f64::abs)
}

/// Detect whether a GoCardless error response indicates the account's
/// consent has expired/been suspended (PSD2 re-consent needed) vs. some
/// other transient error. GoCardless returns a 409/403-class response
/// whose body mentions the account status; this is deliberately a loose,
/// case-insensitive substring check over the whole body rather than a
/// strict schema match, since GoCardless's error body shape for this case
/// isn't rigidly specified — a false negative here just means a real
/// transient-error code path runs instead (safe); see spec Assumption 10.
pub(crate) fn is_consent_expired_error(status: reqwest::StatusCode, body: &serde_json::Value) -> bool {
    if status != reqwest::StatusCode::CONFLICT && status != reqwest::StatusCode::FORBIDDEN && status != reqwest::StatusCode::UNAUTHORIZED {
        return false;
    }
    let text = body.to_string().to_lowercase();
    text.contains("expired") || text.contains("suspended")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Institution {
    pub id: String,
    pub name: String,
    #[serde(default = "default_transaction_total_days")]
    pub transaction_total_days: u32,
}
fn default_transaction_total_days() -> u32 { 90 }

pub async fn list_institutions(country: &str) -> Result<Vec<Institution>, (StatusCode, String)> {
    let body = gocardless_get(&format!("institutions/?country={}", country.to_lowercase())).await?;
    let insts: Vec<Institution> = serde_json::from_value(body)
        .map_err(|e| internal_error(format!("gocardless institutions decode: {e}")))?;
    Ok(insts)
}

/// Case-insensitive substring match of `query` against institution names.
/// Extracted as a pure function over an already-fetched list (`find_institution_id`
/// wraps this with the actual `list_institutions` HTTP call) so it's unit
/// testable without a network call.
pub(crate) fn find_institution_id_in(institutions: &[Institution], query: &str) -> Result<String, String> {
    let q = query.to_lowercase();
    let matches: Vec<&Institution> = institutions.iter().filter(|i| i.name.to_lowercase().contains(&q)).collect();
    match matches.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(format!("I couldn't find a bank matching '{query}'.")),
        many => {
            let names: Vec<&str> = many.iter().map(|i| i.name.as_str()).collect();
            Err(format!("More than one bank matches '{query}': {}. Please be more specific.", names.join(", ")))
        }
    }
}

pub async fn find_institution_id(country: &str, query: &str) -> Result<String, (StatusCode, String)> {
    let institutions = list_institutions(country).await?;
    find_institution_id_in(&institutions, query).map_err(|msg| (StatusCode::BAD_REQUEST, msg))
}
```

- [ ] **Step 5: Run to verify the pure-helper tests pass**

Run: `cd backend && cargo test gocardless`
Expected: PASS for all `#[test]` (non-`#[ignore]`) tests written in Step 1.

- [ ] **Step 6: Implement `start_link_session` / `complete_link_session`**

```rust
#[derive(Debug, Serialize)]
pub struct GcLinkSessionResponse {
    pub redirect_url: String,
    pub reference: Uuid,
}

/// Start a GoCardless consent flow: create an End User Agreement (clamped to
/// the institution's own `transaction_total_days` when smaller than our
/// defaults), a Requisition, persist the pending `bank_link_sessions` row,
/// and return the redirect URL. Pro-gated, Edit-or-Owner-gated, closed-budget
/// rejected — mirrors `financial_connections::create_link_session` exactly.
pub async fn start_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    country: &str,
    institution_id: &str,
) -> Result<GcLinkSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let institutions = list_institutions(country).await?;
    let max_days = institutions.iter().find(|i| i.id == institution_id)
        .map(|i| i.transaction_total_days).unwrap_or(90);
    let configured_days: u32 = env_opt("GOCARDLESS_ACCESS_VALID_DAYS")
        .and_then(|v| v.parse().ok()).unwrap_or(90);
    let historical_days: u32 = env_opt("GOCARDLESS_MAX_HISTORICAL_DAYS")
        .and_then(|v| v.parse().ok()).unwrap_or(90);
    let access_valid_for_days = configured_days.min(max_days);
    let max_historical_days = historical_days.min(max_days);

    let agreement = gocardless_post("agreements/enduser/", &serde_json::json!({
        "institution_id": institution_id,
        "max_historical_days": max_historical_days,
        "access_valid_for_days": access_valid_for_days,
        "access_scope": ["balances", "details", "transactions"],
    })).await?;
    let agreement_id = agreement.get("id").and_then(|v| v.as_str()).map(str::to_string);

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, agreement_id, institution_id, country) \
         VALUES ($1, $2, $3, '', $4, $5, $6)")
        .bind(session_id).bind(budget_id).bind(user_id).bind(&agreement_id).bind(institution_id).bind(country)
        .execute(pool).await.map_err(internal_error)?;

    let redirect = format!("{}/?gc_ref={session_id}", app_url().trim_end_matches('/'));
    let mut req_body = serde_json::json!({
        "redirect": redirect,
        "institution_id": institution_id,
        "reference": session_id.to_string(),
        "user_language": "EN",
    });
    if let Some(aid) = &agreement_id {
        req_body["agreement"] = serde_json::Value::String(aid.clone());
    }
    let requisition = gocardless_post("requisitions/", &req_body).await?;
    let requisition_id = requisition.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("gocardless requisition missing id"))?.to_string();
    let link = requisition.get("link").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("gocardless requisition missing link"))?.to_string();

    sqlx::query("UPDATE bank_link_sessions SET requisition_id = $1 WHERE id = $2")
        .bind(&requisition_id).bind(session_id).execute(pool).await.map_err(internal_error)?;

    Ok(GcLinkSessionResponse { redirect_url: link, reference: session_id })
}

/// Complete a GoCardless consent flow: look up the pending session by
/// `gc_ref`, verify ownership (403 on mismatch — anti-replay, mirrors
/// Stripe's customer-id check), re-fetch the requisition server-side, and
/// persist/reconcile each returned account. Returns the same
/// `ListLinkedAccountsResponse` shape as Stripe's `complete_link_session`
/// and as `bank_linking::list_linked_accounts` (Task 4).
pub async fn complete_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    gc_ref: Uuid,
) -> Result<crate::financial_connections::ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    #[derive(sqlx::FromRow)]
    struct PendingSession {
        budget_id: Uuid,
        user_id: Uuid,
        requisition_id: String,
        institution_id: String,
        country: String,
        status: String,
    }
    let session: PendingSession = sqlx::query_as(
        "SELECT budget_id, user_id, requisition_id, institution_id, country, status FROM bank_link_sessions WHERE id = $1")
        .bind(gc_ref).fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Link session not found or expired".to_string()))?;

    if session.budget_id != budget_id || session.user_id != user_id {
        tracing::warn!(%user_id, %budget_id, "gocardless link session ownership mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }
    if session.status != "pending" {
        return Err((StatusCode::CONFLICT, "This link session has already been completed".to_string()));
    }

    let requisition = gocardless_get(&format!("requisitions/{}/", session.requisition_id)).await?;
    let account_ids: Vec<String> = requisition.get("accounts").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();

    let consent_expires_at = chrono::Utc::now() + chrono::Duration::days(
        env_opt("GOCARDLESS_ACCESS_VALID_DAYS").and_then(|v| v.parse().ok()).unwrap_or(90));

    let mut results = Vec::with_capacity(account_ids.len());
    for account_id in &account_ids {
        let details = gocardless_get(&format!("accounts/{account_id}/details/")).await
            .unwrap_or_else(|_| serde_json::json!({}));
        let iban = details.pointer("/account/iban").and_then(|v| v.as_str()).map(str::to_string);
        let display_name = details.pointer("/account/ownerName").and_then(|v| v.as_str()).map(str::to_string)
            .or_else(|| details.pointer("/account/name").and_then(|v| v.as_str()).map(str::to_string));
        let last4 = iban.as_deref().map(|s| s.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());

        // Reconcile onto an existing consent_expired row for the SAME budget +
        // institution when the IBAN matches (spec Assumption 11) — preserves
        // the local row id (and therefore transaction history) instead of
        // inserting a disconnected-looking duplicate. `iban` is stored on the
        // row itself (added as `linked_accounts.iban` alongside `institution_id`
        // in Task 1's migration) so this is a direct column match, no join.
        let existing_id: Option<Uuid> = if let Some(iban_val) = &iban {
            sqlx::query_scalar(
                "SELECT id FROM linked_accounts \
                 WHERE budget_id = $1 AND institution_id = $2 AND status = 'consent_expired' AND iban = $3 \
                 LIMIT 1")
                .bind(budget_id).bind(&session.institution_id).bind(iban_val)
                .fetch_optional(pool).await.map_err(internal_error)?
        } else { None };

        let row = if let Some(id) = existing_id {
            sqlx::query_as::<_, crate::db::LinkedAccount>(
                "UPDATE linked_accounts SET \
                    provider_account_id = $2, provider_ref = $3, display_name = $4, last4 = $5, \
                    status = 'active', disconnected_at = NULL, consent_expires_at = $6, updated_at = now() \
                 WHERE id = $1 RETURNING *")
                .bind(id).bind(account_id).bind(&session.requisition_id).bind(&display_name).bind(&last4)
                .bind(consent_expires_at)
                .fetch_one(pool).await.map_err(internal_error)?
        } else {
            sqlx::query_as::<_, crate::db::LinkedAccount>(
                "INSERT INTO linked_accounts \
                    (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                     institution_id, institution_name, display_name, last4, iban, country, \
                     status, consent_expires_at) \
                 VALUES ($1, $2, $3, 'gocardless', $4, $5, $6, $6, $7, $8, $9, $10, 'active', $11) \
                 ON CONFLICT (provider_account_id) DO UPDATE SET \
                    display_name = EXCLUDED.display_name, last4 = EXCLUDED.last4, \
                    status = 'active', disconnected_at = NULL, consent_expires_at = EXCLUDED.consent_expires_at, \
                    updated_at = now() \
                 RETURNING *")
                .bind(Uuid::new_v4()).bind(budget_id).bind(user_id)
                .bind(account_id).bind(&session.requisition_id)
                .bind(&session.institution_id).bind(&display_name).bind(&last4).bind(&iban).bind(&session.country)
                .bind(consent_expires_at)
                .fetch_one(pool).await.map_err(internal_error)?
        };

        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial gocardless sync failed; will retry via poll/manual refresh");
        }
        results.push(row.into());
    }

    sqlx::query("UPDATE bank_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(gc_ref).execute(pool).await.map_err(internal_error)?;

    Ok(crate::financial_connections::ListLinkedAccountsResponse { accounts: results })
}
```

- [ ] **Step 6b: Add `linked_accounts.iban` to Task 1's migration and `LinkedAccount` struct**

The reconciliation query above needs `linked_accounts.iban` (not listed in Task 1's original column set). Edit Task 1's migration file NOW (it's still pre-merge, so amending the not-yet-released migration is correct, not a new migration):

Run: `cd backend && grep -n "ALTER TABLE linked_accounts ADD COLUMN institution_id" migrations/20260706120000_gocardless_bank_account_data.sql`

Add immediately after that line in the migration file:
```sql
ALTER TABLE linked_accounts ADD COLUMN iban TEXT;
```

Add `pub iban: Option<String>,` to `crate::db::LinkedAccount` in `db.rs` (Task 1 Step 4), next to `institution_id`. Re-run `cargo check` to confirm the struct now matches every column the `RETURNING *`/`SELECT *` queries in this task produce.

- [ ] **Step 6c: Add the re-link reconciliation integration test**

```rust
// backend/src/gocardless.rs, in the #[cfg(test)] mod tests block, alongside
// the other #[ignore] tests from Step 9 — proves reconciliation actually
// runs (not silently bypassed), since nothing else in this task's tests
// exercises the existing_id branch.
#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn complete_link_session_reconciles_onto_existing_consent_expired_row_by_iban() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    mount_token(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    mk_pro_subscription(&db, uid, "cus_reconcile").await;

    // Seed a consent_expired row from a PRIOR (now-lapsed) consent, same
    // budget + institution + IBAN as the new consent will report.
    let old_row_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO linked_accounts \
            (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
             institution_id, iban, status) \
         VALUES ($1, $2, $3, 'gocardless', 'acct_old', 'req_old', 'MONZO_MONZ_GB', 'GB33MOCK00000000000099', 'consent_expired')")
        .bind(old_row_id).bind(bid).bind(uid).execute(&db).await.unwrap();

    sqlx::query(
        "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, institution_id, country, status) \
         VALUES ($1, $2, $3, 'req_new', 'MONZO_MONZ_GB', 'GB', 'pending')")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
    let gc_ref: Uuid = sqlx::query_scalar("SELECT id FROM bank_link_sessions WHERE requisition_id = 'req_new'")
        .fetch_one(&db).await.unwrap();

    Mock::given(method("GET")).and(path("/requisitions/req_new/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_new", "accounts": ["acct_new"]})))
        .mount(&server).await;
    // Same IBAN as the seeded consent_expired row — a NEW GoCardless account
    // id (banks don't guarantee a stable account id across re-consent).
    Mock::given(method("GET")).and(path("/accounts/acct_new/details/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"account": {"iban": "GB33MOCK00000000000099", "ownerName": "Test User"}})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/accounts/acct_new/transactions/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": [], "pending": []}})))
        .mount(&server).await;

    let result = complete_link_session(&db, uid, bid, gc_ref).await.expect("complete ok");
    assert_eq!(result.accounts.len(), 1);
    assert_eq!(result.accounts[0].id, old_row_id, "must reconcile onto the existing row's id, not create a new one");

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
        .bind(bid).fetch_one(&db).await.unwrap();
    assert_eq!(total, 1, "reconciliation must not leave a duplicate row behind");

    let (provider_account_id, status): (String, String) = sqlx::query_as(
        "SELECT provider_account_id, status FROM linked_accounts WHERE id = $1")
        .bind(old_row_id).fetch_one(&db).await.unwrap();
    assert_eq!(provider_account_id, "acct_new");
    assert_eq!(status, "active");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}
```

Run: `cd backend && cargo test gocardless -- --ignored --test-threads=1 complete_link_session_reconciles`
Expected: PASS.

- [ ] **Step 7: Implement `sync_account_transactions`, `refresh_linked_account`, `disconnect_linked_account`**

```rust
/// Pull booked transactions for one linked GoCardless account and idempotently
/// insert new rows, mirroring `financial_connections::sync_account_transactions`'s
/// contract (closed-budget no-op, best-effort-but-error-surfacing). Detects
/// PSD2 consent expiry via the transactions call's own error response (spec
/// Assumption 10 — no separate status call, to keep the poll job to one
/// GoCardless call per account per cycle) and flips the local row to
/// 'consent_expired' rather than propagating a generic error.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if closed_at.is_some() {
        tracing::info!(account_id = %linked_account.id, "skipping gocardless sync: budget is closed");
        return Ok(0);
    }

    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    let path = format!("accounts/{}/transactions/", linked_account.provider_account_id);
    let (status, body) = gocardless_get_raw(&path).await?;
    if !status.is_success() {
        if is_consent_expired_error(status, &body) {
            sqlx::query("UPDATE linked_accounts SET status = 'consent_expired', updated_at = now() WHERE id = $1")
                .bind(linked_account.id).execute(pool).await.map_err(internal_error)?;
            tracing::info!(account_id = %linked_account.id, "gocardless consent expired — flagged for re-consent");
            return Ok(0);
        }
        tracing::error!(?status, ?body, "gocardless transactions error");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }

    let booked = body.pointer("/transactions/booked").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut imported: u64 = 0;
    for tx in &booked {
        let tx_id = tx.get("transactionId").and_then(|v| v.as_str())
            .or_else(|| tx.get("internalTransactionId").and_then(|v| v.as_str()));
        let Some(tx_id) = tx_id else { continue };
        let amount_str = tx.pointer("/transactionAmount/amount").and_then(|v| v.as_str()).unwrap_or("0");
        let Some(amount) = normalize_amount(amount_str) else { continue };
        let currency = tx.pointer("/transactionAmount/currency").and_then(|v| v.as_str()).unwrap_or("EUR").to_string();
        let description = tx.get("remittanceInformationUnstructured").and_then(|v| v.as_str())
            .unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("bookingDate").and_then(|v| v.as_str())
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let category_id = crate::financial_connections::guess_category_id(&description, &categories);

        let res = sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, \
                 external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
        )
        .bind(Uuid::new_v4()).bind(linked_account.budget_id).bind(category_id).bind(amount)
        .bind(transacted_at).bind(&description).bind(linked_account.id).bind(tx_id).bind(&currency)
        .execute(pool).await.map_err(internal_error)?;
        if res.rows_affected() > 0 {
            imported += 1;
        }
    }

    sqlx::query("UPDATE linked_accounts SET last_synced_at = now(), updated_at = now() WHERE id = $1")
        .bind(linked_account.id).execute(pool).await.map_err(internal_error)?;
    Ok(imported)
}

/// Manual refresh. Unlike Stripe (which asks Stripe to check and waits for a
/// webhook), GoCardless has no such push mechanism — this just runs the sync
/// synchronously. Short-circuits with a clear message on an already
/// `consent_expired` account WITHOUT calling GoCardless at all (spec
/// Assumption 10).
pub async fn refresh_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'gocardless'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    if linked.status == "consent_expired" {
        return Err((StatusCode::CONFLICT, "This account's bank consent has expired — reconnect it to keep syncing.".to_string()));
    }
    if linked.status != "active" {
        return Err((StatusCode::NOT_FOUND, "Linked account not found".to_string()));
    }

    sync_account_transactions(pool, &linked).await?;
    Ok(())
}

/// Disconnect: delete the GoCardless requisition FIRST (ends bank-side
/// consent), then flip the local row — mirrors Stripe's ordering. Not
/// Pro-gated (spec Assumption 12).
pub async fn disconnect_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let provider_ref: String = sqlx::query_scalar(
        "SELECT provider_ref FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'gocardless'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    gocardless_delete(&format!("requisitions/{provider_ref}/")).await?;

    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() WHERE id = $1")
        .bind(account_id).execute(pool).await.map_err(internal_error)?;
    Ok(())
}

/// Scheduled poll (nels#320 — GoCardless has no transaction webhook, unlike
/// Stripe FC). Syncs every active GoCardless account whose owner is still
/// Pro, mirroring the Stripe webhook handler's gating. Called from
/// `main.rs`'s ticker every `GOCARDLESS_POLL_INTERVAL_HOURS` (default 8).
pub async fn poll_active_accounts(pool: &PgPool) {
    let accounts: Vec<crate::db::LinkedAccount> = match sqlx::query_as(
        "SELECT * FROM linked_accounts WHERE provider = 'gocardless' AND status = 'active'")
        .fetch_all(pool).await {
        Ok(a) => a,
        Err(e) => { tracing::error!(%e, "gocardless poll: failed to load active accounts"); return; }
    };
    for account in accounts {
        let owner_status: Option<String> = match sqlx::query_scalar(
            "SELECT status FROM subscriptions WHERE user_id = $1")
            .bind(account.user_id).fetch_optional(pool).await {
            Ok(s) => s.flatten(), Err(_) => None,
        };
        if !user_is_pro(owner_status.as_deref()) {
            tracing::info!(account_id = %account.id, "gocardless poll: skipping, owner not Pro");
            continue;
        }
        if let Err((status, msg)) = sync_account_transactions(pool, &account).await {
            tracing::warn!(?status, %msg, account_id = %account.id, "gocardless poll-driven sync failed");
        }
    }
}
```

- [ ] **Step 8: Run unit tests**

Run: `cd backend && cargo test gocardless`
Expected: PASS, all non-`#[ignore]` tests from Steps 1/4.

- [ ] **Step 9: Write `#[ignore]`+`wiremock` DB-integration tests mirroring `financial_connections.rs`'s style**

```rust
// backend/src/gocardless.rs, inside the same #[cfg(test)] mod tests block,
// below the unit tests from Step 1. Reuses the same test_pool/mk_user/
// mk_budget/mk_pro_subscription helper SHAPES as financial_connections.rs —
// duplicate them here (module-private test helpers, matching this
// codebase's per-module test-helper convention) rather than importing
// financial_connections's private test fns.
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
        .bind(id).bind(format!("gc-{id}@test.example")).execute(db).await.unwrap();
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
fn set_gc_env(server: &MockServer) {
    std::env::set_var("GOCARDLESS_API_BASE", server.uri());
    std::env::set_var("GOCARDLESS_SECRET_ID", "sid_test");
    std::env::set_var("GOCARDLESS_SECRET_KEY", "skey_test");
}
fn clear_gc_env() {
    std::env::remove_var("GOCARDLESS_API_BASE");
    std::env::remove_var("GOCARDLESS_SECRET_ID");
    std::env::remove_var("GOCARDLESS_SECRET_KEY");
}
async fn mount_token(server: &MockServer) {
    Mock::given(method("POST")).and(path("/token/new/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access": "tok_x", "access_expires": 3600})))
        .mount(server).await;
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn non_pro_user_cannot_start_gocardless_link() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let result = start_link_session(&db, uid, bid, "GB", "MONZO_MONZ_GB").await;
    assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn pro_user_can_start_and_complete_gocardless_link() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    mount_token(&server).await;
    Mock::given(method("GET")).and(path("/institutions/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "MONZO_MONZ_GB", "name": "Monzo", "transaction_total_days": 90}
        ])))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/agreements/enduser/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "agr_1"})))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/requisitions/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_1", "link": "https://ob.gocardless.com/x"})))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    mk_pro_subscription(&db, uid, "cus_gc").await;

    let started = start_link_session(&db, uid, bid, "GB", "MONZO_MONZ_GB").await.expect("pro user can start a link");
    assert_eq!(started.redirect_url, "https://ob.gocardless.com/x");

    Mock::given(method("GET")).and(path(format!("/requisitions/req_1/")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_1", "accounts": ["acct_1"]})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/accounts/acct_1/details/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"account": {"iban": "GB33MOCK00000000000001", "ownerName": "Test User"}})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/accounts/acct_1/transactions/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": [], "pending": []}})))
        .mount(&server).await;

    let completed = complete_link_session(&db, uid, bid, started.reference).await.expect("complete ok");
    assert_eq!(completed.accounts.len(), 1);
    assert_eq!(completed.accounts[0].provider, "gocardless");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn sync_detects_expired_consent() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    mount_token(&server).await;
    Mock::given(method("GET")).and(path("/accounts/acct_expired/transactions/"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({"summary": "Access expired"})))
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'gocardless', 'acct_expired', 'req_x') RETURNING *")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).fetch_one(&db).await.unwrap();

    let result = sync_account_transactions(&db, &linked).await.unwrap();
    assert_eq!(result, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(linked.id).fetch_one(&db).await.unwrap();
    assert_eq!(status, "consent_expired");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn refresh_rejects_consent_expired_without_calling_gocardless() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    mount_token(&server).await;
    Mock::given(method("GET")).and(path("/accounts/acct_ce/transactions/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
        .expect(0)
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    mk_pro_subscription(&db, uid, "cus_ce").await;
    sqlx::query(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
         VALUES ($1, $2, $3, 'gocardless', 'acct_ce', 'req_ce', 'consent_expired')")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
    let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_ce'")
        .fetch_one(&db).await.unwrap();

    let result = refresh_linked_account(&db, uid, bid, account_id).await;
    assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}

#[tokio::test]
#[ignore]
#[serial_test::serial]
async fn disconnect_deletes_requisition_and_marks_local_disconnected() {
    let server = MockServer::start().await;
    set_gc_env(&server);
    mount_token(&server).await;
    Mock::given(method("DELETE")).and(path("/requisitions/req_disc/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"summary": "deleted"})))
        .expect(1)
        .mount(&server).await;

    let db = test_pool().await;
    let uid = mk_user(&db).await;
    let bid = mk_budget(&db, uid).await;
    sqlx::query(
        "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
         VALUES ($1, $2, $3, 'gocardless', 'acct_disc', 'req_disc')")
        .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
    let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_disc'")
        .fetch_one(&db).await.unwrap();

    disconnect_linked_account(&db, uid, bid, account_id).await.expect("disconnect ok");
    let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
        .bind(account_id).fetch_one(&db).await.unwrap();
    assert_eq!(status, "disconnected");

    sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    clear_gc_env();
}
```

- [ ] **Step 10: Run the integration tests**

Run: `cd backend && cargo test gocardless -- --ignored --test-threads=1 2>&1 | tail -60`
Expected: all 5 `#[ignore]` tests pass. If `provider` field isn't recognized on `LinkedAccountResponse` (Task 1 Step 5 must have already added it), fix that first.

- [ ] **Step 11: Register the module and add env docs**

In `backend/src/main.rs`: add `mod gocardless;` next to `mod bank_provider;`.

In `.env.example`, near the existing `STRIPE_*`/`APP_URL` block:
```
# GoCardless Bank Account Data (nels#320) — bank linking for UK/FR/DE/IT/ES/DK/FI/NO
#   In production: fly secrets set GOCARDLESS_SECRET_ID=... GOCARDLESS_SECRET_KEY=... -a nels-api
GOCARDLESS_SECRET_ID=
GOCARDLESS_SECRET_KEY=
GOCARDLESS_API_BASE=
GOCARDLESS_ACCESS_VALID_DAYS=90
GOCARDLESS_MAX_HISTORICAL_DAYS=90
GOCARDLESS_POLL_INTERVAL_HOURS=8
```

- [ ] **Step 12: Full backend test run**

Run: `cd backend && cargo test && cargo test -- --ignored --test-threads=4`
Expected: everything green (existing #303 suite + new gocardless suite).

- [ ] **Step 13: Commit**

```bash
git add backend/src/gocardless.rs backend/src/main.rs backend/migrations/20260706120000_gocardless_bank_account_data.sql .env.example
git commit -m "feat(#320): add GoCardless Bank Account Data integration (link, sync, consent-expiry, disconnect)"
```

---

## Task 4: `bank_linking.rs` dispatch layer + REST wiring + poll job

**Files:**
- Create: `backend/src/bank_linking.rs`
- Modify: `backend/src/main.rs` (new routes, poll-job ticker, `mod bank_linking;`)
- Modify: `backend/src/financial_connections.rs` (remove `list_linked_accounts`/`refresh_linked_account`/`disconnect_linked_account`'s public REST-handler wrappers if now redundant — keep the underlying logic functions, just repoint the axum handlers)

- [ ] **Step 1: Write the dispatch layer with unit tests for the provider-selection logic**

```rust
//! Provider-agnostic bank-linking operations (nels#320 spec Assumption 8):
//! callers (rag.rs, REST handlers, frontend-facing responses) call these
//! instead of reaching into `financial_connections::*`/`gocardless::*`
//! directly for list/refresh/disconnect/sync — each function matches on
//! `linked_accounts.provider` exactly once.

use axum::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use crate::budget::{check_permission, Permission};
use crate::error::internal_error;
use crate::financial_connections::ListLinkedAccountsResponse;

/// Provider-agnostic — queries the table directly, same as #303's original
/// `financial_connections::list_linked_accounts`. View-or-above only.
pub async fn list_linked_accounts(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<ListLinkedAccountsResponse, (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "No access to this budget".to_string()));
    }
    let rows = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;
    Ok(ListLinkedAccountsResponse { accounts: rows.into_iter().map(Into::into).collect() })
}

async fn provider_of(pool: &PgPool, account_id: Uuid, budget_id: Uuid) -> Result<String, (StatusCode, String)> {
    sqlx::query_scalar("SELECT provider FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))
}

pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

// --- REST handlers (replace financial_connections.rs's list/refresh/disconnect handlers) ---
use axum::{extract::{Path, State}, Extension, Json};
use crate::auth::AppState;

pub async fn list_linked_accounts_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    list_linked_accounts(&state.db, user_id, budget_id).await.map(Json)
}

pub async fn refresh_linked_account_handler(
    State(state): State<AppState>,
    Path((budget_id, account_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    refresh_linked_account(&state.db, user_id, budget_id, account_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn disconnect_linked_account_handler(
    State(state): State<AppState>,
    Path((budget_id, account_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    disconnect_linked_account(&state.db, user_id, budget_id, account_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- GoCardless-specific REST handlers (start/complete/institutions) ---

#[derive(serde::Deserialize)]
pub struct GcStartLinkRequest {
    pub country: String,
    pub institution_id: String,
}

pub async fn gc_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<GcStartLinkRequest>,
) -> Result<Json<crate::gocardless::GcLinkSessionResponse>, (StatusCode, String)> {
    crate::gocardless::start_link_session(&state.db, user_id, budget_id, &req.country, &req.institution_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct GcCompleteLinkRequest {
    pub gc_ref: Uuid,
}

pub async fn gc_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<GcCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::gocardless::complete_link_session(&state.db, user_id, budget_id, req.gc_ref).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct GcInstitutionsQuery {
    pub country: String,
}

pub async fn gc_institutions_handler(
    axum::extract::Query(q): axum::extract::Query<GcInstitutionsQuery>,
) -> Result<Json<Vec<crate::gocardless::Institution>>, (StatusCode, String)> {
    crate::gocardless::list_institutions(&q.country).await.map(Json)
}

#[cfg(test)]
mod tests {
    // provider_of/list_linked_accounts/refresh/disconnect are thin DB-backed
    // dispatchers with no pure branching logic worth a unit test beyond what
    // gocardless.rs's and financial_connections.rs's own #[ignore] suites
    // already cover end-to-end; a dedicated bank_linking #[ignore] test
    // confirms the dispatch itself picks the right implementation:
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_dispatches_to_gocardless_for_a_gocardless_row() {
        let db = test_pool().await;
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
            .bind(uid).bind(format!("bl-{uid}@test.example")).execute(&db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'gocardless', 'acct_bl', 'req_bl', 'consent_expired')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl'")
            .fetch_one(&db).await.unwrap();
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, 'cus_bl', 'active')")
            .bind(uid).execute(&db).await.unwrap();

        // consent_expired -> gocardless::refresh_linked_account's own guard
        // fires (409), proving dispatch reached the GoCardless implementation
        // and not Stripe's (which would 404 on a provider mismatch instead).
        let result = refresh_linked_account(&db, uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
}
```

- [ ] **Step 2: Update `main.rs` routing**

Find the existing linked-accounts routes (around line 268-272):
```rust
        .route("/budgets/:id/linked-accounts/session", post(start_link))
        .route("/budgets/:id/linked-accounts/complete", post(complete_link))
        .route("/budgets/:id/linked-accounts", get(list_linked_accounts_handler))
        .route("/budgets/:id/linked-accounts/:account_id/refresh", post(refresh_linked_account_handler))
        .route("/budgets/:id/linked-accounts/:account_id", delete(disconnect_linked_account_handler))
```
Replace with (list/refresh/disconnect now go through `bank_linking`; Stripe's `start_link`/`complete_link` stay in `financial_connections`; new GoCardless routes added):
```rust
        .route("/budgets/:id/linked-accounts/session", post(start_link))
        .route("/budgets/:id/linked-accounts/complete", post(complete_link))
        .route("/budgets/:id/linked-accounts", get(bank_linking::list_linked_accounts_handler))
        .route("/budgets/:id/linked-accounts/:account_id/refresh", post(bank_linking::refresh_linked_account_handler))
        .route("/budgets/:id/linked-accounts/:account_id", delete(bank_linking::disconnect_linked_account_handler))
        .route("/budgets/:id/gocardless/session", post(bank_linking::gc_start_link_handler))
        .route("/budgets/:id/gocardless/complete", post(bank_linking::gc_complete_link_handler))
        .route("/gocardless/institutions", get(bank_linking::gc_institutions_handler))
```

Update the existing import at `main.rs` lines 58-61:
```rust
use financial_connections::{
    start_link, complete_link, list_linked_accounts_handler,
    refresh_linked_account_handler, disconnect_linked_account_handler,
};
```
to:
```rust
use financial_connections::{start_link, complete_link};
```
All three handler names (`list_linked_accounts_handler`, `refresh_linked_account_handler`, `disconnect_linked_account_handler`) are removed from this import — Step 3 deletes all three from `financial_connections.rs` itself, and the route table above now references `bank_linking::list_linked_accounts_handler`/`bank_linking::refresh_linked_account_handler`/`bank_linking::disconnect_linked_account_handler` instead. Leaving any of the three old names in this `use` statement produces an unresolved-import (E0432) error.

Add `mod bank_linking;` next to the other `mod` declarations.

- [ ] **Step 3: Remove the now-redundant handler wrappers from `financial_connections.rs`**

Delete `list_linked_accounts_handler`, `refresh_linked_account_handler`, `disconnect_linked_account_handler` (lines ~639-663) from `financial_connections.rs` — their logic (`list_linked_accounts`, `refresh_linked_account`, `disconnect_linked_account` free functions) stays, since `bank_linking.rs` calls those directly for the `"stripe"` branch. Only the axum-handler wrappers move.

Run: `cd backend && cargo check` — fix any now-dangling imports (e.g. if `main.rs` still imports the deleted handler names from `financial_connections`).

- [ ] **Step 4: Wire the poll job into `main.rs`'s scheduler**

Find the existing hourly ticker (around line 122-123):
```rust
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
```
Add a SEPARATE ticker (GoCardless's interval is independently configurable, default 8h, not the same cadence as the existing 1h rollover/fund ticker) right after that `tokio::spawn` block closes:
```rust
    {
        let gc_pool = pool.clone(); // reuse whatever pool variable this scope already has, matching the existing ticker's own `cleanup_pool`/`pool` clone pattern
        let interval_hours: u64 = std::env::var("GOCARDLESS_POLL_INTERVAL_HOURS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_hours * 3600));
            loop {
                ticker.tick().await;
                gocardless::poll_active_accounts(&gc_pool).await;
            }
        });
    }
```
Match the exact pool-variable name already in scope at that point in `main.rs` (run `grep -n "let.*pool" backend/src/main.rs | sed -n '1,15p'` first to confirm the right variable to `.clone()` — don't guess the name).

- [ ] **Step 5: Run full test suite**

Run: `cd backend && cargo check && cargo test && cargo test -- --ignored --test-threads=4`
Expected: all green, including the new `bank_linking::tests::refresh_dispatches_to_gocardless_for_a_gocardless_row` test.

- [ ] **Step 6: Manual route-reachability smoke**

Run: `cd backend && cargo run &` then, once it's listening:
```bash
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/api/budgets/00000000-0000-0000-0000-000000000000/gocardless/session -X POST -d '{}'
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/api/gocardless/institutions?country=GB
```
Expected: `401` for both (unauthenticated), never `404` (confirms the routes are actually mounted). Kill the background server afterward: `kill %1`.

- [ ] **Step 7: Commit**

```bash
git add backend/src/bank_linking.rs backend/src/main.rs backend/src/financial_connections.rs
git commit -m "feat(#320): add provider-agnostic bank_linking dispatch, REST routes, and GoCardless poll job"
```

---

## Task 5: `rag.rs` chat actions

**Files:**
- Modify: `backend/src/rag.rs` (`AiActionParams`, `ChatResponse`, `required_perm_for_action` — no change needed, already covers these actions — system prompt rule 2o, the four dispatch match arms, `offline_linked_accounts_action`)

- [ ] **Step 1: Add new `AiActionParams` fields**

In `backend/src/rag.rs`, in the `AiActionParams` struct (around line 264-304), add near the other bank-linking-adjacent fields:
```rust
    #[serde(default)]
    country: Option<String>,          // ISO 3166-1 alpha-2, for LINK_BANK_ACCOUNT (#320)
    #[serde(default)]
    institution_query: Option<String>, // free-text bank name, for LINK_BANK_ACCOUNT (#320)
```
(`account_match` already exists from #303 — confirm with `grep -n "account_match" backend/src/rag.rs` and reuse it unchanged for `UNLINK_BANK_ACCOUNT`.)

- [ ] **Step 1b: Fix every EXISTING exhaustive `AiActionParams { ... }` struct literal — this is required for the crate to compile**

`AiActionParams` is a plain (non-`#[non_exhaustive]`) struct with no `Default` derive, and this codebase's test suite constructs it as an exhaustive field-by-field literal (e.g. every test ending `account_match: None,`), NOT via `..Default::default()`. `#[serde(default)]` only affects JSON deserialization — it does nothing for a Rust struct-literal expression. Adding `country`/`institution_query` without updating every existing literal produces `E0063: missing fields` at every one of those call sites.

Run: `cd backend && grep -n "AiActionParams {" src/rag.rs | wc -l` and `cd backend && grep -n "AiActionParams {" src/rag.rs` to get the exact count and line numbers (double digits — roughly 14 as of this plan's writing, but re-count against the actual file since other tickets may have landed on `main`/the base branch since).

For EVERY one of those literals, add two new lines immediately after its `account_match: None,` line (or, if a given literal doesn't set `account_match` either, after whichever field it lists last before the closing `}`):
```rust
            country: None,
            institution_query: None,
```
Do this for the full list from the grep above — do not skip any. If a literal is a production (non-test) construction site rather than a test fixture, apply the same two-line addition there too (a real `AiActionParams` this ticket's own new code constructs, e.g. inside a test you write in Step 8, should set these fields directly to their intended values instead of `None`).

Run: `cd backend && cargo check` after this step, BEFORE writing any of Steps 2 onward, to confirm the crate compiles with the two new fields in place and no lingering `E0063` errors.

- [ ] **Step 2: Add `ChatResponse.bank_link_redirect_url`**

In the `ChatResponse` struct (around line 93-142), right after the existing `financial_connections_client_secret` field:
```rust
    /// The GoCardless consent redirect URL (#320), set ONLY by LINK_BANK_ACCOUNT
    /// when it resolves to a GoCardless country. Unlike Stripe's client_secret
    /// (which drives a client-side SDK modal), GoCardless's flow is a full-page
    /// redirect — the frontend navigates the browser to this URL directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_link_redirect_url: Option<String>,
```

Update the struct-construction site (around line 3949, in the final `Ok(Json(ChatResponse { ... }))`) to add the new field:
```rust
        financial_connections_client_secret,
        bank_link_redirect_url,
    }))
}
```

- [ ] **Step 3: Declare and thread the new local variable**

Near the existing `let mut financial_connections_client_secret: Option<String> = None;` (around line 1987), add:
```rust
    // Set ONLY by LINK_BANK_ACCOUNT when it resolves to GoCardless (#320);
    // threaded into ChatResponse so the frontend can redirect.
    let mut bank_link_redirect_url: Option<String> = None;
```

- [ ] **Step 4: Rewrite the `LINK_BANK_ACCOUNT` dispatch arm to branch on country**

Replace the existing `"LINK_BANK_ACCOUNT" => { ... }` arm (around line 3003-3016) with:
```rust
        "LINK_BANK_ACCOUNT" => {
            if let Some(bid) = active_budget_id {
                let country = parsed_ai_res.action_params.as_ref().and_then(|p| p.country.clone());
                match country.as_deref().map(crate::bank_provider::Provider::for_country) {
                    None | Some(None) => {
                        let names: Vec<&str> = crate::bank_provider::Provider::supported_countries()
                            .iter().map(|(_, name)| *name).collect();
                        mutation_error = Some(format!(
                            "Which country is your bank account in? I can currently link accounts in: {}.",
                            names.join(", ")
                        ));
                    }
                    Some(Some(crate::bank_provider::Provider::Stripe)) => {
                        match crate::financial_connections::create_link_session(&state.db, user_id, bid).await {
                            Ok(session) => financial_connections_client_secret = Some(session.client_secret),
                            Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                            Err((_, msg)) => mutation_error = Some(msg),
                        }
                    }
                    Some(Some(crate::bank_provider::Provider::GoCardless)) => {
                        let institution_query = parsed_ai_res.action_params.as_ref().and_then(|p| p.institution_query.clone());
                        let Some(query) = institution_query else {
                            mutation_error = Some("Which bank would you like to connect? (e.g. Monzo, Revolut, N26)".to_string());
                            continue_link: {
                                break 'link_arm;
                            }
                        };
                        let cc = country.unwrap();
                        match crate::gocardless::find_institution_id(&cc, &query).await {
                            Err((_, msg)) => mutation_error = Some(msg),
                            Ok(institution_id) => {
                                match crate::gocardless::start_link_session(&state.db, user_id, bid, &cc, &institution_id).await {
                                    Ok(session) => bank_link_redirect_url = Some(session.redirect_url),
                                    Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                                    Err((_, msg)) => mutation_error = Some(msg),
                                }
                            }
                        }
                    }
                }
            }
        }
```

**IMPORTANT — fix the `break 'link_arm'`/labeled-block placeholder above before compiling**: Rust match arms aren't loops, so `break 'link_arm` won't compile as written — it's there to flag "return early from this arm." Replace that whole `let Some(query) = institution_query else { ... }` block with a plain if/else instead of a let-else-with-break, since we're inside a `match` arm block, not a loop:
```rust
                    Some(Some(crate::bank_provider::Provider::GoCardless)) => {
                        let institution_query = parsed_ai_res.action_params.as_ref().and_then(|p| p.institution_query.clone());
                        match institution_query {
                            None => {
                                mutation_error = Some("Which bank would you like to connect? (e.g. Monzo, Revolut, N26)".to_string());
                            }
                            Some(query) => {
                                let cc = country.unwrap();
                                match crate::gocardless::find_institution_id(&cc, &query).await {
                                    Err((_, msg)) => mutation_error = Some(msg),
                                    Ok(institution_id) => {
                                        match crate::gocardless::start_link_session(&state.db, user_id, bid, &cc, &institution_id).await {
                                            Ok(session) => bank_link_redirect_url = Some(session.redirect_url),
                                            Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                                            Err((_, msg)) => mutation_error = Some(msg),
                                        }
                                    }
                                }
                            }
                        }
                    }
```
Use this corrected version in the arm from Step 4 — drop the broken `continue_link`/`break 'link_arm'` fragment entirely.

- [ ] **Step 5: Rewire `LIST_LINKED_ACCOUNTS`, `UNLINK_BANK_ACCOUNT`, `REFRESH_BANK_ACCOUNT` to call `bank_linking::*`**

In each of the three remaining arms (around lines 3019-3110), replace every `crate::financial_connections::list_linked_accounts`, `crate::financial_connections::disconnect_linked_account`, `crate::financial_connections::refresh_linked_account` call with the equivalent `crate::bank_linking::*` call (same signature, same return shape — this is a pure call-site swap, no other logic changes). For example, `LIST_LINKED_ACCOUNTS`'s arm becomes:
```rust
        "LIST_LINKED_ACCOUNTS" => {
            if let Some(bid) = active_budget_id {
                match crate::bank_linking::list_linked_accounts(&state.db, user_id, bid).await {
                    Ok(list) => {
                        if list.accounts.is_empty() {
                            mutation_log = Some("You have no linked bank accounts yet.".to_string());
                        } else {
                            let lines: Vec<String> = list.accounts.iter().map(|a| {
                                let status_note = if a.status == "consent_expired" {
                                    " — needs reconnecting (bank consent expired)"
                                } else { "" };
                                format!("- {} ({}) — {}{}",
                                    a.display_name.as_deref().unwrap_or("Bank account"),
                                    a.institution_name.as_deref().unwrap_or("Unknown institution"),
                                    a.status, status_note)
                            }).collect();
                            mutation_log = Some(format!("Linked accounts:\n{}", lines.join("\n")));
                        }
                    }
                    Err((status, msg)) => {
                        tracing::error!(?status, %msg, "LIST_LINKED_ACCOUNTS: failed to list linked accounts");
                        mutation_error = Some("I couldn't load your linked accounts right now. Please try again.".to_string());
                    }
                }
            }
        }
```
(Note the added `status_note` — surfaces `consent_expired` distinctly per the AC's "not silent failure" requirement.) Apply the same `financial_connections::` → `bank_linking::` swap verbatim to `UNLINK_BANK_ACCOUNT` and `REFRESH_BANK_ACCOUNT`'s bodies — no other line in those two arms changes.

- [ ] **Step 6: Extend the system prompt (rule 2o)**

Replace the existing rule 2o text (around line 1415) with:
```
         2o. LINKED BANK ACCOUNTS (Pro feature): if the user asks to 'link my bank account', 'connect my bank', 'sync transactions from my bank', or similar, set 'action' to 'LINK_BANK_ACCOUNT'. You MUST first know which COUNTRY their bank is in — currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless). If they haven't said, ask which country before setting 'action' (use 'action':'NONE' and ask in 'response_text'); once you know it, populate 'country' with its 2-letter code (e.g. 'GB', 'US'). For a non-US country, you ALSO need the bank's name — populate 'institution_query' with it if named, otherwise ask which bank in 'response_text' (still with 'action':'NONE') before proceeding. Do not ask for any account numbers or credentials yourself — GoCardless/Stripe handle authentication directly with the bank. If the user asks what's linked, what accounts are connected, or similar, set 'action' to 'LIST_LINKED_ACCOUNTS'. If the user asks to disconnect, unlink, or remove a linked account, set 'action' to 'UNLINK_BANK_ACCOUNT' and populate 'account_match' with the bank/account name they mentioned (or leave it empty if they didn't name one — you'll be asked to clarify). If the user asks to refresh, sync, or update their bank transactions now, set 'action' to 'REFRESH_BANK_ACCOUNT'. All four require an active Pro subscription except LIST_LINKED_ACCOUNTS and UNLINK_BANK_ACCOUNT, which work regardless of subscription status.\n\
```

- [ ] **Step 7: Extend the offline (no-LLM) router**

Add country/institution phrase recognition alongside the existing `offline_linked_accounts_action` (around line 6036-6058) — new pure helper, unit tested the same way:
```rust
/// Best-effort country-code extraction for the offline router (#320) —
/// recognizes a small set of explicit country/currency-area phrases in the
/// SAME message. No conversational memory (mirrors offline_budget_strategy's
/// same-message-only convention), so "link my UK bank account" works but a
/// bare follow-up "it's in the UK" after a prior clarifying question does not
/// — that gap only affects the offline (no GEMINI_API_KEY) path.
pub(crate) fn offline_country_hint(msg_lower: &str) -> Option<&'static str> {
    if msg_lower.contains(" uk") || msg_lower.contains("united kingdom") || msg_lower.contains("british") {
        Some("GB")
    } else if msg_lower.contains("france") || msg_lower.contains("french") {
        Some("FR")
    } else if msg_lower.contains("germany") || msg_lower.contains("german") {
        Some("DE")
    } else if msg_lower.contains("italy") || msg_lower.contains("italian") {
        Some("IT")
    } else if msg_lower.contains("spain") || msg_lower.contains("spanish") {
        Some("ES")
    } else if msg_lower.contains("denmark") || msg_lower.contains("danish") {
        Some("DK")
    } else if msg_lower.contains("finland") || msg_lower.contains("finnish") {
        Some("FI")
    } else if msg_lower.contains("norway") || msg_lower.contains("norwegian") {
        Some("NO")
    } else if msg_lower.contains("us bank") || msg_lower.contains("united states") || msg_lower.contains("american bank") {
        Some("US")
    } else {
        None
    }
}
```
This is a pure best-effort helper for the offline path only — it is NOT wired into the online (Gemini-backed) `LINK_BANK_ACCOUNT` arm, which relies on the model itself to extract `country`/`institution_query` per the rule 2o prompt update. Wire it ONLY into whatever function constructs the offline mock `AiStructuredResponse` for `"LINK_BANK_ACCOUNT"` (find it via `grep -n "Mock AI Offline Mode" backend/src/rag.rs` and the surrounding match) so the offline path can also populate `country` when detectable — if the existing offline path doesn't already thread country/institution_query params through to the same dispatch arms Step 4/5 modified, leave it defaulting to the "ask for country" branch (safe fallback, not a regression, since offline mode already returns a canned string today with no real params for this action).

- [ ] **Step 8: Add unit tests**

```rust
// backend/src/rag.rs, inside financial_connections_offline_router_tests (or a
// new adjacent mod gocardless_offline_router_tests) — mirror the existing
// #[test] style exactly.
#[test]
fn offline_country_hint_recognizes_uk() {
    assert_eq!(offline_country_hint("link my uk bank account"), Some("GB"));
}
#[test]
fn offline_country_hint_recognizes_us() {
    assert_eq!(offline_country_hint("link my us bank account"), Some("US"));
}
#[test]
fn offline_country_hint_returns_none_for_unrecognized() {
    assert_eq!(offline_country_hint("link my brazilian bank account"), None);
}
```

- [ ] **Step 9: Run tests**

Run: `cd backend && cargo test rag:: 2>&1 | tail -40`
Expected: all pass, including the new tests. Fix any compile errors from the `AiActionParams`/`ChatResponse` field additions surfacing elsewhere (e.g. any exhaustive struct literal that constructs `ChatResponse` without the new field — Step 2 already handles the one production construction site; check for test-only construction sites too via `grep -n "ChatResponse {" backend/src/rag.rs`).

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#320): extend chat co-pilot with GoCardless country/institution-aware LINK_BANK_ACCOUNT"
```

---

## Task 6: Frontend — country/institution picker, redirect handling, consent_expired UI

**Files:**
- Modify: `frontend/src/lib/linkedAccounts.js`
- Modify: `frontend/src/lib/LinkedAccounts.svelte`
- Modify: `frontend/src/App.svelte`
- Modify: `frontend/src/lib/linkedAccounts.test.js`
- Modify: i18n locale file(s) — find via `grep -rn "linkedAccounts" frontend/src/lib/i18n/ 2>/dev/null || find frontend/src -iname "*.json" | xargs grep -l "linkedAccounts"`

- [ ] **Step 1: Locate the i18n file(s) actually used**

Run: `cd frontend && grep -rln "linkedAccounts" src/ | grep -v '\.svelte$' | grep -v '\.js$'`
Expected: one or more JSON locale files (e.g. `src/lib/i18n/en.json`). Note the exact path(s) for Step 5.

- [ ] **Step 2: Add GoCardless functions to `linkedAccounts.js`**

Append to `frontend/src/lib/linkedAccounts.js`:
```js
/** List institutions for a country (GoCardless, #320). */
export async function fetchGcInstitutions({ country, fetchApi }) {
  const data = await fetchApi(`/gocardless/institutions?country=${encodeURIComponent(country)}`);
  return Array.isArray(data) ? data : [];
}

/**
 * Start a GoCardless consent flow: create the session server-side and return
 * its redirect_url — the CALLER does the actual `window.location.href`
 * navigation (kept out of this function so it stays unit-testable without a
 * real browser navigation).
 */
export async function startGcLinkFlow({ budgetId, country, institutionId, fetchApi }) {
  const session = await fetchApi(`/budgets/${budgetId}/gocardless/session`, {
    method: "POST",
    body: JSON.stringify({ country, institution_id: institutionId }),
  });
  return session; // { redirect_url, reference }
}

/** Complete a GoCardless consent flow after the user returns from their bank. */
export async function completeGcLinkFlow({ budgetId, gcRef, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/gocardless/complete`, {
    method: "POST",
    body: JSON.stringify({ gc_ref: gcRef }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

/** Whether a linked account needs the user to reconnect (PSD2 consent expired, #320). */
export function isConsentExpired(account) {
  return account?.status === "consent_expired";
}
```

- [ ] **Step 3: Write failing tests for the new `linkedAccounts.js` functions**

```js
// frontend/src/lib/linkedAccounts.test.js — add alongside the existing describe blocks
import { fetchGcInstitutions, startGcLinkFlow, completeGcLinkFlow, isConsentExpired } from "./linkedAccounts.js";

describe("GoCardless flow (#320)", () => {
  it("fetchGcInstitutions returns the institutions array", async () => {
    const fetchApi = vi.fn().mockResolvedValue([{ id: "MONZO_MONZ_GB", name: "Monzo" }]);
    const result = await fetchGcInstitutions({ country: "GB", fetchApi });
    expect(result).toEqual([{ id: "MONZO_MONZ_GB", name: "Monzo" }]);
    expect(fetchApi).toHaveBeenCalledWith("/gocardless/institutions?country=GB");
  });

  it("startGcLinkFlow posts country and institution_id and returns the session", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ redirect_url: "https://ob.gocardless.com/x", reference: "ref-1" });
    const result = await startGcLinkFlow({ budgetId: "b1", country: "GB", institutionId: "MONZO_MONZ_GB", fetchApi });
    expect(result.redirect_url).toBe("https://ob.gocardless.com/x");
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/gocardless/session", {
      method: "POST",
      body: JSON.stringify({ country: "GB", institution_id: "MONZO_MONZ_GB" }),
    });
  });

  it("completeGcLinkFlow parses the linked-accounts response", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "gocardless" }] });
    const result = await completeGcLinkFlow({ budgetId: "b1", gcRef: "ref-1", fetchApi });
    expect(result.linked).toEqual([{ id: "a1", provider: "gocardless" }]);
  });

  it("isConsentExpired detects the consent_expired status", () => {
    expect(isConsentExpired({ status: "consent_expired" })).toBe(true);
    expect(isConsentExpired({ status: "active" })).toBe(false);
    expect(isConsentExpired(null)).toBe(false);
  });
});
```

- [ ] **Step 4: Run to verify, then confirm pass**

Run: `cd frontend && pnpm test -- linkedAccounts`
Expected: first run FAILS (functions didn't exist before Step 2) if run before Step 2 — since Step 2 already added them above, running now should PASS. (If executing strictly in order, run once after Step 2+3 together and confirm PASS.)

- [ ] **Step 5: Add i18n strings**

In the locale file(s) found in Step 1, alongside the existing `linkedAccounts.*` keys, add:
```json
"linkedAccounts.selectCountry": "Country",
"linkedAccounts.selectBank": "Bank",
"linkedAccounts.loadingBanks": "Loading banks…",
"linkedAccounts.connectViaGoCardless": "Connect",
"linkedAccounts.consentExpiredBadge": "Reconnect needed",
"linkedAccounts.reconnect": "Reconnect",
"linkedAccounts.gcCompleteError": "We couldn't finish connecting your bank. Please try again.",
```
(Match the exact surrounding JSON structure/indentation of the file found in Step 1 — do not assume a flat vs. nested key shape without checking.)

- [ ] **Step 6: Extend `LinkedAccounts.svelte` with the country/institution picker and consent_expired UI**

Replace the full contents of `frontend/src/lib/LinkedAccounts.svelte` with:
```svelte
<script>
  import { loadStripe } from "@stripe/stripe-js";
  import { _ } from "svelte-i18n";
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
    fetchGcInstitutions, startGcLinkFlow, isConsentExpired,
  } from "./linkedAccounts.js";

  let { budgetId, isPro, onUpgrade, fetchApi } = $props();

  let accounts = $state([]);
  let loading = $state(false);
  let error = $state("");

  const GC_COUNTRIES = ["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"];
  let selectedCountry = $state("US");
  let institutions = $state([]);
  let selectedInstitutionId = $state("");
  let loadingInstitutions = $state(false);

  async function load() {
    loading = true;
    try {
      accounts = await fetchLinkedAccounts({ budgetId, fetchApi });
      error = "";
    } catch (e) {
      error = e.message || $_("linkedAccounts.loadError");
    } finally {
      loading = false;
    }
  }

  async function onCountryChange() {
    institutions = [];
    selectedInstitutionId = "";
    if (!GC_COUNTRIES.includes(selectedCountry)) return;
    loadingInstitutions = true;
    try {
      institutions = await fetchGcInstitutions({ country: selectedCountry, fetchApi });
    } catch (e) {
      error = e.message || $_("linkedAccounts.linkError");
    } finally {
      loadingInstitutions = false;
    }
  }

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

  async function refresh(accountId) {
    try {
      await refreshLinkedAccount({ budgetId, accountId, fetchApi });
      error = "";
    } catch (e) {
      error = isProGateError(e) ? $_("linkedAccounts.refreshProGate") : (e.message || $_("linkedAccounts.refreshError"));
    }
  }

  async function disconnect(accountId) {
    try {
      await disconnectLinkedAccount({ budgetId, accountId, fetchApi });
      await load();
      error = "";
    } catch (e) {
      error = e.message || $_("linkedAccounts.disconnectError");
    }
  }

  $effect(() => {
    void budgetId;
    load();
  });
</script>

<div class="linked-accounts">
  <h3 class="font-semibold text-lg mb-2">{$_("linkedAccounts.title")}</h3>

  {#if error}
    <p class="text-error text-sm mb-2">{error}</p>
  {/if}

  {#if loading}
    <p class="text-sm opacity-70">{$_("linkedAccounts.loading")}</p>
  {:else if accounts.length === 0}
    <p class="text-sm opacity-70">{$_("linkedAccounts.empty")}</p>
  {:else}
    <ul class="space-y-2">
      {#each accounts as acct (acct.id)}
        <li class="flex items-center justify-between">
          <span>
            {acct.display_name ?? $_("linkedAccounts.unknownAccount")} ({acct.institution_name ?? $_("linkedAccounts.unknownInstitution")})
            {#if acct.status === "disconnected"}<span class="badge badge-ghost ml-2">{$_("linkedAccounts.disconnectedBadge")}</span>{/if}
            {#if isConsentExpired(acct)}<span class="badge badge-warning ml-2">{$_("linkedAccounts.consentExpiredBadge")}</span>{/if}
          </span>
          {#if acct.status === "active" || isConsentExpired(acct)}
            <span>
              {#if acct.status === "active"}
                <button class="btn btn-xs" onclick={() => refresh(acct.id)}>{$_("linkedAccounts.refresh")}</button>
              {/if}
              <button class="btn btn-xs btn-ghost" onclick={() => disconnect(acct.id)}>{$_("linkedAccounts.disconnect")}</button>
            </span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if isPro}
    <div class="mt-3 space-y-2">
      <select class="select select-bordered select-sm" bind:value={selectedCountry} onchange={onCountryChange}>
        <option value="US">United States</option>
        <option value="GB">United Kingdom</option>
        <option value="FR">France</option>
        <option value="DE">Germany</option>
        <option value="IT">Italy</option>
        <option value="ES">Spain</option>
        <option value="DK">Denmark</option>
        <option value="FI">Finland</option>
        <option value="NO">Norway</option>
      </select>
      {#if GC_COUNTRIES.includes(selectedCountry)}
        {#if loadingInstitutions}
          <p class="text-sm opacity-70">{$_("linkedAccounts.loadingBanks")}</p>
        {:else}
          <select class="select select-bordered select-sm" bind:value={selectedInstitutionId}>
            <option value="">{$_("linkedAccounts.selectBank")}</option>
            {#each institutions as inst (inst.id)}
              <option value={inst.id}>{inst.name}</option>
            {/each}
          </select>
        {/if}
      {/if}
      <button class="btn btn-primary btn-sm" onclick={link}>{$_("linkedAccounts.link")}</button>
    </div>
  {:else}
    <button class="btn btn-outline btn-sm mt-3" onclick={() => onUpgrade?.("monthly")}>{$_("linkedAccounts.upgradeToLink")}</button>
  {/if}
</div>
```

- [ ] **Step 7: Handle `gc_ref` on `App.svelte` mount**

Run: `cd frontend && grep -n "onMount\|<script" src/App.svelte | head -10` to find the existing mount lifecycle hook and confirm `fetchApi`/active-budget-id variable names in scope there.

Add, inside the existing `onMount` (or create one if none exists at the top level — check first), using whatever the file's actual `fetchApi`/active-budget-store names are (do not invent new ones; match what's already imported/used elsewhere in `App.svelte`):
```js
import { completeGcLinkFlow } from "./lib/linkedAccounts.js";

// Inside the existing onMount(...) body, near its start:
const gcRef = new URLSearchParams(window.location.search).get("gc_ref");
if (gcRef) {
  const url = new URL(window.location.href);
  url.searchParams.delete("gc_ref");
  history.replaceState({}, "", url.toString());
  if (activeBudgetId) {
    try {
      await completeGcLinkFlow({ budgetId: activeBudgetId, gcRef, fetchApi });
    } catch (e) {
      console.error("gocardless complete failed", e);
    }
  }
}
```
Replace `activeBudgetId`/`fetchApi` with whatever the file's real in-scope variable names are for "the currently active budget id" and "the app's fetch wrapper" (confirmed via the Step 7 grep) — these are almost certainly NOT literally named `activeBudgetId`/`fetchApi` at the top level of `App.svelte` itself; trace how `LinkedAccounts.svelte` receives its own `budgetId`/`fetchApi` props (Step 6's `$props()`) back up to `App.svelte` to find the real source variables.

- [ ] **Step 8: Run frontend tests and build**

Run: `cd frontend && pnpm test`
Expected: all pass, including the new GoCardless tests from Step 3.

Run: `cd frontend && pnpm run build`
Expected: succeeds with no errors.

- [ ] **Step 9: Commit**

```bash
git add frontend/src/lib/linkedAccounts.js frontend/src/lib/linkedAccounts.test.js frontend/src/lib/LinkedAccounts.svelte frontend/src/App.svelte frontend/src/lib/i18n/*.json
git commit -m "feat(#320): add GoCardless country/bank picker, redirect handling, and consent_expired UI"
```

---

## Task 7: Documentation + final integration pass

**Files:**
- Modify: `AGENTS.md` (new §15)
- No new code files — this task is verification + docs.

- [ ] **Step 1: Add AGENTS.md §15**

After the existing §14 (Financial Connections Bank Linking), add:
```markdown
### 15. GoCardless Bank Account Data Bank Linking (#320)
- **Second provider, same `linked_accounts`/`transactions` tables, now generalized**: `linked_accounts.provider`
  (`'stripe'` | `'gocardless'`), `provider_account_id`/`provider_ref` (renamed from `stripe_account_id`/
  `stripe_customer_id`), plus GoCardless-only nullable `consent_expires_at`/`country`/`institution_id`/`iban`.
  `transactions.provider_transaction_id` (renamed from `stripe_transaction_id`) is now uniqueness-scoped per
  `(external_account_id, provider_transaction_id)`, not globally — GoCardless transaction ids are bank-assigned,
  not guaranteed globally unique the way Stripe's are. New `transactions.currency` (ISO 4217) column; no FX
  conversion anywhere in this codebase.
- **`bank_provider::Provider` + `bank_linking.rs`**: a closed two-member enum (not a `dyn` trait object — see
  the spec's Assumption 8) maps country -> provider (`US` -> Stripe; `GB`/`FR`/`DE`/`IT`/`ES`/`DK`/`FI`/`NO` ->
  GoCardless). `bank_linking.rs`'s `list_linked_accounts`/`refresh_linked_account`/`disconnect_linked_account`
  each match on `linked_accounts.provider` once and delegate — `rag.rs`'s `LIST_LINKED_ACCOUNTS`/
  `UNLINK_BANK_ACCOUNT`/`REFRESH_BANK_ACCOUNT` and the REST list/refresh/disconnect handlers call these instead
  of reaching into `financial_connections::*`/`gocardless::*` directly. `LINK_BANK_ACCOUNT` (starting a NEW
  link) is the one call site that still branches on provider, since Stripe's client_secret-driven modal and
  GoCardless's full-page redirect are genuinely different response shapes.
- **No GoCardless webhook — polled instead**: GoCardless Bank Account Data has no per-transaction webhook
  (unlike Stripe FC). A scheduled poll job (`gocardless::poll_active_accounts`, `main.rs` ticker, default every
  `GOCARDLESS_POLL_INTERVAL_HOURS=8`) syncs every active GoCardless account whose owner is still Pro — one
  GoCardless call per account per cycle (3/day at the default), staying under GoCardless's ~4-calls/day/account
  guidance with headroom for the initial link-time sync and manual refreshes.
- **Redirect-based consent, our own pending-session table**: `bank_link_sessions` persists `budget_id`/
  `user_id`/`requisition_id` keyed by a UUID we generate and embed OURSELVES in the redirect URL
  (`{APP_URL}/?gc_ref=<id>`) handed to GoCardless — GoCardless redirects the browser back to exactly that URL,
  appending nothing of its own. `App.svelte` picks up `gc_ref` on mount and calls
  `POST /api/budgets/:id/gocardless/complete`, which re-fetches the requisition server-side (never trusts the
  redirect alone) before persisting anything.
- **PSD2 ~90-day consent expiry is explicit, not silent**: `access_valid_for_days`/`max_historical_days` default
  to 90 (env-configurable), clamped to each institution's own `transaction_total_days` cap. Expiry is detected
  from the transactions call's own error response (no separate status call, to keep the poll job to one
  GoCardless call per account) and flips the local row to `status = 'consent_expired'` — surfaced distinctly by
  `LIST_LINKED_ACCOUNTS` (REST + chat) and short-circuited (no GoCardless call) by manual refresh with a clear
  "reconnect it" message. Re-consent reuses the same link flow; `complete_link_session` reconciles onto an
  existing `consent_expired` row for the same budget+institution by IBAN match when available, preserving
  transaction history — no IBAN match falls back to a new row (documented, bounded v1 limitation, no data loss
  either way).
- **Institution selection**: chat uses a fuzzy case-insensitive substring match (`gocardless::find_institution_id`,
  mirroring `UNLINK_BANK_ACCOUNT`'s `account_match` pattern) over `GET /institutions/?country=`; the REST/frontend
  flow additionally offers a real `<select>` of the country's institutions.
- **Surfacing**: REST — `POST /api/budgets/:id/gocardless/session`, `POST .../gocardless/complete`,
  `GET /api/gocardless/institutions?country=`, plus the now-shared `GET/POST/DELETE .../linked-accounts...`
  routes (provider-agnostic since Task 4). Chat — `LINK_BANK_ACCOUNT` gained `country`/`institution_query`
  params (system prompt rule 2o); `LIST_LINKED_ACCOUNTS`/`UNLINK_BANK_ACCOUNT`/`REFRESH_BANK_ACCOUNT` unchanged
  in shape, now provider-agnostic under the hood. `ChatResponse.bank_link_redirect_url` (alongside the existing
  `financial_connections_client_secret`) carries the GoCardless redirect URL to the frontend. Frontend — a
  country + bank `<select>` in the existing Linked Accounts panel; a "Reconnect needed" badge for
  `consent_expired` accounts.
```

- [ ] **Step 2: Full backend suite**

Run: `cd backend && cargo test && cargo test -- --ignored --test-threads=4 2>&1 | tail -30`
Expected: all green — record the pass counts for the PR body.

- [ ] **Step 3: Full frontend suite + build**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: all green.

- [ ] **Step 4: Manual route-reachability smoke (final confirmation, repeats Task 4 Step 6 after all changes land)**

Run: `cd backend && cargo run &`, then:
```bash
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/api/budgets/00000000-0000-0000-0000-000000000000/linked-accounts
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/api/budgets/00000000-0000-0000-0000-000000000000/gocardless/session -X POST -d '{}'
curl -s -o /dev/null -w "%{http_code}\n" "http://localhost:3000/api/gocardless/institutions?country=GB"
```
Expected: `401`/`401`/`401` (unauthenticated), never `404`. Kill the server: `kill %1`.

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#320): document GoCardless Bank Account Data integration in AGENTS.md"
```
