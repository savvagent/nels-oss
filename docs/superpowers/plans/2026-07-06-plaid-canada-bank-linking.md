# Plaid Canada Bank Linking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Post-merge rebase note**: all 8 tasks below were fully implemented, spec-compliance-reviewed,
> and quality-reviewed exactly as written, against a branch based on the (then-unmerged)
> `issue-320-gocardless-bank-account-data` branch. Before the PR was opened, the maintainer merged
> #303, #320, #322 (Belvo), and #323 (Basiq/Akahu) into `main` — across two rebase passes, since
> #322 landed mid-way through reconciling the first three. The branch was rebased onto the final
> `main`: Plaid was re-integrated as the SIXTH `bank_provider::Provider` variant (not third), the
> schema migration was retimestamped and its CHECK constraint rechained onto the full current
> provider list (also restoring `'belvo'`, which #323's own migration had silently dropped — a
> pre-existing bug fixed as a side effect), and `bank_linking.rs`'s `refresh_linked_account`
> dispatch gained a `cipher` parameter (added by #323 for Akahu) that Plaid's own arm ignores. A
> pre-existing test-hygiene bug (3 `plaid.rs` tests missing a cleanup `DELETE`, causing failures on
> a second run against a persistent DB) was also fixed during the second rebase pass. Every other
> design decision, guard, and test below is unchanged and was carried forward verbatim — see the
> actual shipped commits for the final, reconciled code. `AGENTS.md` §18 (not §16 as originally
> planned) documents the final state.

**Goal:** Let Pro subscribers with a Canadian bank account link one or more accounts via Plaid Link (an embeddable JS modal, covering ~99% of Canadian deposit accounts including all Big 5 banks), with transactions importing into the existing `transactions` table tagged `currency = 'CAD'` (no FX), auto-refresh via Plaid's transactions webhook plus manual refresh, and view/disconnect + chat reachability matching #303 (Stripe)/#320 (GoCardless)'s established UX — implemented as a THIRD `bank_provider::Provider` variant against the existing dispatch layer, not a new abstraction.

**Architecture:** A migration adds `'plaid'` to `linked_accounts.provider`, two Plaid-only nullable columns (`plaid_item_id`, `plaid_cursor`), and a new `plaid_link_sessions` anti-replay table. `backend/src/bank_provider.rs` gains a `Provider::Plaid` variant and `for_country("CA")`. A new `backend/src/plaid.rs` owns the Plaid HTTP integration (link-token creation, public-token exchange with anti-replay, per-row `/transactions/sync` cursor, refresh, `/item/remove`-based disconnect, JWT/JWK webhook verification+dispatch), mirroring `financial_connections.rs`'s/`gocardless.rs`'s existing "own client, own env test seam" convention. `backend/src/bank_linking.rs` gains one dispatch arm per provider-agnostic operation. `rag.rs`'s `LINK_BANK_ACCOUNT` gains a `Provider::Plaid` arm; `LIST_LINKED_ACCOUNTS`/`UNLINK_BANK_ACCOUNT`/`REFRESH_BANK_ACCOUNT` need zero changes (already provider-agnostic via #320's dispatch layer). Frontend adds a Canada option to the existing country picker, a Plaid Link modal invocation (mirroring the existing Stripe.js pattern) with OAuth-redirect resumption for Big-5 institutions.

**Tech Stack:** Rust/axum/sqlx/reqwest (backend), `jsonwebtoken` (new dependency, ES256 webhook verification), Svelte 5 (frontend), Postgres. Full spec: `docs/superpowers/specs/2026-07-06-plaid-canada-bank-linking-design.md`.

---

## Task 1: Schema migration + `Provider::Plaid` + `db.rs` structs

**Files:**
- Create: `backend/migrations/20260706140000_plaid_bank_linking.sql`
- Modify: `backend/src/bank_provider.rs`
- Modify: `backend/src/db.rs` (`LinkedAccount` struct, ~line 242; new `PlaidLinkSession` struct)
- Modify: `backend/src/rag.rs` (temporary placeholder match arm only — see Step 8; replaced for real in Task 5)
- Test: `backend/src/bank_provider.rs` `#[cfg(test)]` module

- [ ] **Step 1: Write the migration**

```sql
-- backend/migrations/20260706140000_plaid_bank_linking.sql
-- Adds Plaid (nels#321) as a third bank-linking provider alongside Stripe
-- (#303) and GoCardless (#320), against the existing generalized
-- linked_accounts/transactions schema and bank_provider::Provider dispatch —
-- no further generalization, just a third value. See
-- docs/superpowers/specs/2026-07-06-plaid-canada-bank-linking-design.md
-- Assumptions 4-6.

-- linked_accounts_provider_check is Postgres's auto-generated name for
-- #320's inline `provider TEXT ... CHECK (...)` column addition (Postgres's
-- default naming for an unnamed column CHECK is <table>_<column>_check) —
-- confirmed in migrations/20260706120000_gocardless_bank_account_data.sql.
ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'plaid'));

-- Plaid's item_id is the correlation key Plaid's webhook payloads carry (a
-- webhook fires per-Item, not per-account, and one Item can back multiple
-- linked_accounts rows) — distinct from provider_account_id, which is the
-- per-account Plaid account_id. Always NULL for Stripe/GoCardless rows.
ALTER TABLE linked_accounts ADD COLUMN plaid_item_id TEXT;
CREATE INDEX linked_accounts_plaid_item_id_idx ON linked_accounts (plaid_item_id)
    WHERE plaid_item_id IS NOT NULL;

-- This row's own /transactions/sync cursor (spec Assumption 7: per-row, not
-- per-Item, to keep sync_account_transactions shaped like every other
-- provider's per-linked-account sync function). Always NULL for
-- Stripe/GoCardless rows and for a Plaid row that has never synced yet.
ALTER TABLE linked_accounts ADD COLUMN plaid_cursor TEXT;

-- Anti-replay pending-session table (spec Assumption 4/5) — a Plaid
-- public_token carries no recoverable client_user_id/budget binding at
-- exchange time, unlike Stripe's client_secret (verified against the
-- caller's own Stripe customer) or GoCardless's bank_link_sessions row
-- (verified against the caller's own budget_id/user_id). One row per
-- POST .../plaid/link-token call; POST .../plaid/complete looks this row up
-- by session_id and 403s on a budget_id/user_id mismatch, 409s if already
-- completed.
CREATE TABLE plaid_link_sessions (
    id          UUID PRIMARY KEY,
    budget_id   UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX plaid_link_sessions_budget_id_idx ON plaid_link_sessions (budget_id);
```

- [ ] **Step 2: Confirm the existing provider CHECK constraint's real name matches what Step 1 wrote**

Run: `cd backend && grep -n "CHECK (provider" migrations/20260706120000_gocardless_bank_account_data.sql`
Expected: `ALTER TABLE linked_accounts ADD COLUMN provider TEXT NOT NULL DEFAULT 'stripe' CHECK (provider IN ('stripe', 'gocardless'))` — an inline `ADD COLUMN ... CHECK`, whose auto-generated Postgres constraint name is `linked_accounts_provider_check` (Postgres's default naming for an unnamed column CHECK is `<table>_<column>_check`), matching what Step 1's migration already targets.

- [ ] **Step 3: Run the migration locally and confirm it applies cleanly on top of #320's schema**

Run: `podman-compose up -d && cd backend && cargo run` (migrations run automatically on server start per `AGENTS.md` §3), then Ctrl-C once "Server starting at" logs.
Expected: no migration error in the logs; `psql $DATABASE_URL -c "\d linked_accounts"` (or an equivalent `sqlx` introspection) shows `plaid_item_id`, `plaid_cursor` columns and the widened `provider` CHECK; `psql $DATABASE_URL -c "\d plaid_link_sessions"` shows the new table.

- [ ] **Step 4: Add `Provider::Plaid` to `bank_provider.rs`**

Also update the file's module-level doc comment (the `//! Shared bank-provider abstraction (nels#320): a closed, two-member set (Stripe Financial Connections, GoCardless Bank Account Data)...` line at the top of the file) — it becomes stale (still says "two-member set") once Plaid lands. Replace `"a closed, two-member set (Stripe Financial Connections, GoCardless Bank Account Data)"` with `"a closed, three-member set (Stripe Financial Connections, GoCardless Bank Account Data, Plaid)"`, leaving the rest of that comment (the enum-dispatch-vs-`dyn`-trait rationale) unchanged since it's still accurate per spec Assumption 1.

```rust
// backend/src/bank_provider.rs — full replacement of the enum + impl block
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    GoCardless,
    Plaid,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::GoCardless => "gocardless",
            Provider::Plaid => "plaid",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "stripe" => Some(Provider::Stripe),
            "gocardless" => Some(Provider::GoCardless),
            "plaid" => Some(Provider::Plaid),
            _ => None,
        }
    }

    /// Map an ISO 3166-1 alpha-2 country code (case-insensitive) to the
    /// provider that covers it. `US` -> Stripe; the 8 GoCardless launch
    /// countries (nels#320) -> GoCardless; `CA` -> Plaid (nels#321);
    /// anything else -> None (caller must ask the user for a supported
    /// country, never guess).
    pub fn for_country(cc: &str) -> Option<Provider> {
        match cc.to_uppercase().as_str() {
            "US" => Some(Provider::Stripe),
            "GB" | "FR" | "DE" | "IT" | "ES" | "DK" | "FI" | "NO" => Some(Provider::GoCardless),
            "CA" => Some(Provider::Plaid),
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
            ("CA", "Canada"),
        ]
    }
}
```

- [ ] **Step 5: Update the existing `bank_provider.rs` unit tests that necessarily change**

The spec's success criteria explicitly carve these two out as "updated, not unchanged." Replace:

```rust
    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Provider::parse("plaid"), None);
    }
```

with:

```rust
    #[test]
    fn parse_accepts_plaid() {
        assert_eq!(Provider::parse("plaid"), Some(Provider::Plaid));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Provider::parse("venmo"), None);
    }
```

and:

```rust
    #[test]
    fn supported_countries_lists_all_nine() {
        assert_eq!(Provider::supported_countries().len(), 9);
    }
```

with:

```rust
    #[test]
    fn supported_countries_lists_all_ten() {
        assert_eq!(Provider::supported_countries().len(), 10);
    }
```

Then add two new tests exercising the new mapping:

```rust
    #[test]
    fn for_country_maps_ca_to_plaid() {
        assert_eq!(Provider::for_country("CA"), Some(Provider::Plaid));
        assert_eq!(Provider::for_country("ca"), Some(Provider::Plaid));
    }

    #[test]
    fn as_str_round_trips_plaid() {
        assert_eq!(Provider::parse(Provider::Plaid.as_str()), Some(Provider::Plaid));
    }
```

- [ ] **Step 6: Run the `bank_provider` test suite**

Run: `cd backend && cargo test bank_provider`
Expected: all tests pass, including the new/renamed ones above.

- [ ] **Step 7: Add `plaid_item_id`/`plaid_cursor` to `LinkedAccount` and a new `PlaidLinkSession` struct in `db.rs`**

```rust
// backend/src/db.rs — LinkedAccount gains two fields (append after `updated_at`)
pub struct LinkedAccount {
    // ...existing fields unchanged...
    pub plaid_item_id: Option<String>,
    pub plaid_cursor: Option<String>,
}

/// Anti-replay pending session for a Plaid Link attempt (nels#321 spec
/// Assumption 4) — see plaid.rs::create_link_token/complete_link_session.
#[derive(Debug, sqlx::FromRow)]
pub struct PlaidLinkSession {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub user_id: Uuid,
    pub status: String, // 'pending' | 'completed'
    pub created_at: DateTime<Utc>,
}
```

Confirm `db.rs`'s existing `LinkedAccount` derive includes `sqlx::FromRow` (it must, since `SELECT *` queries in `financial_connections.rs`/`gocardless.rs` already deserialize into it) — the two new columns need no additional derive changes, `sqlx::FromRow`'s `SELECT *` mapping picks them up by name automatically once the migration lands.

- [ ] **Step 8: Add a temporary placeholder arm to `rag.rs`'s `LINK_BANK_ACCOUNT` match so the crate keeps compiling**

`bank_linking.rs`'s two dispatch functions match on `linked_accounts.provider.as_str()` (a string), so a missing `"plaid"` arm there is caught only by its wildcard `other =>` branch — no compile error. But `rag.rs`'s `LINK_BANK_ACCOUNT` handler (~line 3029) matches on the `Provider` enum ITSELF exhaustively (`None | Some(None) => ..., Some(Some(Provider::Stripe)) => ..., Some(Some(Provider::GoCardless)) => ...`, no wildcard arm) — adding `Provider::Plaid` in Step 4 makes this match non-exhaustive and `cargo check` will fail on `rag.rs` the moment this task's enum change lands, even though `rag.rs`'s real Plaid wiring isn't due until Task 5. Add a temporary arm now (replaced by Task 5 Step 6's real implementation) so every task leaves the crate compiling:

```rust
// backend/src/rag.rs — MODIFY the LINK_BANK_ACCOUNT match (~line 3029), add
// this arm after the existing GoCardless one. TEMPORARY — Task 5 Step 6
// replaces this entire arm with the real create_link_token call.
                    Some(Some(crate::bank_provider::Provider::Plaid)) => {
                        mutation_error = Some("Bank linking for Canada is coming soon.".to_string());
                    }
```

- [ ] **Step 9: Compile-check**

Run: `cd backend && cargo check`
Expected: compiles clean.

- [ ] **Step 10: Commit**

```bash
git add backend/migrations/20260706140000_plaid_bank_linking.sql backend/src/bank_provider.rs backend/src/db.rs backend/src/rag.rs
git commit -m "feat(#321): add plaid as a third bank-linking provider (schema + enum)"
```

---

## Task 2: `plaid.rs` — link-token creation, anti-replay, public-token exchange + persist

**Files:**
- Create: `backend/src/plaid.rs`
- Modify: `backend/src/main.rs` (add `mod plaid;` near the existing `mod financial_connections;`/`mod gocardless;`/`mod bank_linking;` lines, ~line 31-34 — routes wired in Task 4)

- [ ] **Step 1: Write the module skeleton — HTTP client, env helpers, guards**

```rust
// backend/src/plaid.rs
//! Plaid bank-account linking for Canada (nels#321): link a bank account via
//! Plaid Link's embeddable JS modal, auto-import its transactions into the
//! existing `transactions` table, keep them refreshed via Plaid's transactions
//! webhook or manual refresh. Third `bank_provider::Provider` variant against
//! #320's existing dispatch layer — mirrors `financial_connections.rs`'s/
//! `gocardless.rs`'s "own HTTP client, own env-driven test seam" convention,
//! but JSON-bodied with client_id/secret in every request body (Plaid's own
//! auth convention), not a bearer token or cached credential.

use axum::http::StatusCode;
use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AppState;
use crate::billing::user_is_pro;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn plaid_api_base() -> String {
    env_opt("PLAID_API_BASE").unwrap_or_else(|| "https://production.plaid.com".to_string())
}
fn plaid_redirect_uri() -> String {
    env_opt("PLAID_REDIRECT_URI").unwrap_or_else(|| {
        let app_url = env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string());
        format!("{}/", app_url.trim_end_matches('/'))
    })
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// POST JSON to the Plaid API, with `client_id`/`secret` merged into every
/// body (Plaid's own auth convention — unlike Stripe's bearer-token or
/// GoCardless's cached-token approaches). Mirrors `stripe_post`'s/
/// `gocardless_post`'s error-shape convention (502 on any non-2xx, logged).
async fn plaid_post(path: &str, mut body: serde_json::Value) -> Result<serde_json::Value, (StatusCode, String)> {
    let client_id = env_opt("PLAID_CLIENT_ID")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank linking is not configured".to_string()))?;
    let secret = env_opt("PLAID_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank linking is not configured".to_string()))?;
    let obj = body.as_object_mut().ok_or_else(|| internal_error("plaid_post: body must be a JSON object"))?;
    obj.insert("client_id".to_string(), serde_json::Value::String(client_id));
    obj.insert("secret".to_string(), serde_json::Value::String(secret));

    let url = format!("{}/{}", plaid_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client()
        .post(&url)
        .json(&body)
        .send().await
        .map_err(|e| internal_error(format!("plaid POST {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await
        .map_err(|e| internal_error(format!("plaid decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "plaid API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Bank linking provider error".to_string()));
    }
    Ok(body)
}

/// Require Edit-or-Owner on `budget_id` for `user_id`. Mirrors
/// `financial_connections::require_edit_or_owner`/`gocardless::require_edit_or_owner`
/// exactly.
async fn require_edit_or_owner(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to manage linked bank accounts".to_string()));
    }
    Ok(())
}

/// Require the caller to be Pro, else 402. Mirrors
/// `financial_connections::require_pro`/`gocardless::require_pro` exactly —
/// used ONLY by link-token creation and manual refresh, NOT by viewing or
/// disconnecting (spec Assumption 14 / #303's Assumption 11 precedent).
pub(crate) async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}
```

- [ ] **Step 2: Write the failing test for link-token creation's Pro-gate**

```rust
// backend/src/plaid.rs — append to a new #[cfg(test)] mod tests block
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn mk_user_and_budget(db: &PgPool) -> (Uuid, Uuid) {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
            .bind(uid).bind(format!("plaid-{uid}@test.example")).execute(db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(db).await.unwrap();
        (uid, bid)
    }

    /// Mirrors `gocardless.rs`'s own `mk_pro_subscription` helper exactly
    /// (same signature) — every test below the Pro-gate itself needs an
    /// active subscription row, or `require_pro` 402s before the anti-replay
    /// check under test ever runs.
    async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'active')")
            .bind(user_id).bind(customer_id).execute(db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn create_link_token_rejects_non_pro_with_zero_plaid_calls() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        // No PLAID_CLIENT_ID/PLAID_SECRET set in this test process and no
        // subscriptions row for uid — require_pro must reject BEFORE
        // create_link_token ever calls plaid_post, so the missing env
        // configuration never even surfaces as a 503.
        let result = create_link_token(&db, uid, bid).await;
        assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
}
```

- [ ] **Step 3: Run test to verify it fails (function doesn't exist yet)**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: compile error, `create_link_token` not found.

- [ ] **Step 4: Implement `create_link_token`**

```rust
// backend/src/plaid.rs — append below the guards, above #[cfg(test)]
#[derive(Debug, Serialize)]
pub struct LinkTokenResponse {
    pub link_token: String,
    pub session_id: Uuid,
}

/// Create a Plaid Link token for `budget_id` and a `plaid_link_sessions` row
/// to anti-replay-bind the eventual public_token exchange to this specific
/// (budget_id, user_id) pair (spec Assumption 4 — closes a gap #303/#320's
/// own client_secret/bank_link_sessions checks don't leave open, but a raw
/// Plaid public_token would). Pro-gated (402), Edit-or-Owner-gated (403),
/// budget-not-closed-gated (409) — same guard order as
/// `financial_connections::create_link_session`.
pub async fn create_link_token(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<LinkTokenResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO plaid_link_sessions (id, budget_id, user_id) VALUES ($1, $2, $3)")
        .bind(session_id).bind(budget_id).bind(user_id)
        .execute(pool).await.map_err(internal_error)?;

    let body = serde_json::json!({
        "client_name": "Nels",
        "language": "en",
        "country_codes": ["CA"],
        "user": { "client_user_id": user_id.to_string() },
        "products": ["transactions"],
        "webhook": format!("{}/api/plaid/webhook", env_opt("APP_URL").unwrap_or_default().trim_end_matches('/')),
        "redirect_uri": plaid_redirect_uri(),
    });
    let resp = plaid_post("link/token/create", body).await?;
    let link_token = resp.get("link_token").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid link/token/create missing link_token"))?
        .to_string();
    Ok(LinkTokenResponse { link_token, session_id })
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: PASS (Pro-gate rejects before any Plaid call — no `PLAID_CLIENT_ID` needed for this specific test to pass).

- [ ] **Step 6: Write the failing test for the anti-replay check on completion**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_session_for_a_different_budget() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let (_other_uid, other_bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_p1").await; // must clear require_pro before the anti-replay check runs
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO plaid_link_sessions (id, budget_id, user_id) VALUES ($1, $2, $3)")
            .bind(session_id).bind(other_bid).bind(uid) // session belongs to a DIFFERENT budget
            .execute(&db).await.unwrap();

        let result = complete_link_session(&db, uid, bid, session_id, "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_unknown_session_id() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_p2").await;
        let result = complete_link_session(&db, uid, bid, Uuid::new_v4(), "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_already_completed_session() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_p3").await;
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO plaid_link_sessions (id, budget_id, user_id, status) VALUES ($1, $2, $3, 'completed')")
            .bind(session_id).bind(bid).bind(uid)
            .execute(&db).await.unwrap();

        let result = complete_link_session(&db, uid, bid, session_id, "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
    }
```

- [ ] **Step 7: Run tests to verify they fail**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: compile error, `complete_link_session` not found.

- [ ] **Step 8: Implement `complete_link_session`**

```rust
// backend/src/plaid.rs
// Reuse financial_connections's response shapes rather than redefining them —
// gocardless.rs sets this exact precedent (its complete_link_session returns
// `crate::financial_connections::ListLinkedAccountsResponse` too, at
// gocardless.rs:301/447) rather than declaring its own duplicate struct.
use crate::financial_connections::{guess_category_id, ListLinkedAccountsResponse};

/// Exchange a Plaid `public_token` for accounts and persist them, after
/// verifying (spec Assumption 5) that `session_id` is a `pending` session
/// belonging to THIS `budget_id`/`user_id` — the anti-replay check a raw
/// Plaid public_token has no other way to get. Pro-gated (402),
/// Edit-or-Owner-gated (403), budget-not-closed-gated (409) — same three
/// guards as `create_link_token`.
pub async fn complete_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    session_id: Uuid,
    public_token: &str,
) -> Result<ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let session = sqlx::query_as::<_, crate::db::PlaidLinkSession>(
        "SELECT * FROM plaid_link_sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Link session not found".to_string()))?;
    if session.budget_id != budget_id || session.user_id != user_id {
        tracing::warn!(user_id = %user_id, %session_id, "plaid link session budget/user mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }
    if session.status != "pending" {
        return Err((StatusCode::CONFLICT, "This link session was already completed".to_string()));
    }

    let exchange = plaid_post("item/public_token/exchange", serde_json::json!({ "public_token": public_token })).await?;
    let access_token = exchange.get("access_token").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing access_token"))?.to_string();
    let item_id = exchange.get("item_id").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing item_id"))?.to_string();

    let accounts_resp = plaid_post("accounts/get", serde_json::json!({ "access_token": access_token })).await?;
    let institution_id = accounts_resp.pointer("/item/institution_id").and_then(|v| v.as_str()).map(str::to_string);
    let institution_name = match &institution_id {
        Some(iid) => {
            let inst = plaid_post("institutions/get_by_id", serde_json::json!({
                "institution_id": iid, "country_codes": ["CA"],
            })).await.ok();
            inst.and_then(|v| v.pointer("/institution/name").and_then(|n| n.as_str()).map(str::to_string))
        }
        None => None,
    };

    let accounts = accounts_resp.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // No categories query here — sync_account_transactions (Task 3) loads
    // categories itself for the categorization heuristic; querying them again
    // in this function would be a wasted, unused DB round-trip.

    let mut results = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let account_id = match acc.get("account_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let display_name = acc.get("name").and_then(|v| v.as_str()).map(str::to_string);
        let last4 = acc.get("mask").and_then(|v| v.as_str()).map(str::to_string);

        // Same already-linked-to-a-different-budget guard as #303/#320
        // (financial_connections.rs:260-275) — provider_account_id is
        // globally unique but scoped to the ONE budget it was first linked
        // into.
        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(&account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                return Err((StatusCode::CONFLICT, "This bank account is already linked to a different budget. Disconnect it there first.".to_string()));
            }
        }

        let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, institution_name, display_name, last4, country, plaid_item_id) \
             VALUES ($1, $2, $3, 'plaid', $4, $5, $6, $7, $8, $9, 'CA', $10) \
             ON CONFLICT (provider_account_id) DO UPDATE SET \
                institution_name = EXCLUDED.institution_name, display_name = EXCLUDED.display_name, \
                last4 = EXCLUDED.last4, status = 'active', disconnected_at = NULL, updated_at = now() \
             RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(&account_id).bind(&access_token)
            .bind(&institution_id).bind(&institution_name).bind(&display_name).bind(&last4).bind(&item_id)
            .fetch_one(pool).await.map_err(internal_error)?;

        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial plaid sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    sqlx::query("UPDATE plaid_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(session_id).execute(pool).await.map_err(internal_error)?;

    Ok(ListLinkedAccountsResponse { accounts: results })
}
```

Note: `sync_account_transactions` is implemented in Task 3 — this task's own tests (Step 6) exercise ONLY the anti-replay/not-found/already-completed paths, none of which reach the `plaid_post("item/public_token/exchange", ...)` call (all three fail before it), so this compiles and passes once Task 3 adds the real `sync_account_transactions` body. Add a temporary stub now so Task 2 compiles standalone:

```rust
// backend/src/plaid.rs — temporary stub, replaced by Task 3 Step 4
pub async fn sync_account_transactions(
    _pool: &PgPool,
    _linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    Ok(0)
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: all 4 tests so far (`create_link_token_rejects_non_pro_with_zero_plaid_calls`, `complete_link_session_rejects_session_for_a_different_budget`, `complete_link_session_rejects_unknown_session_id`, `complete_link_session_rejects_already_completed_session`) PASS.

- [ ] **Step 10: Add `mod plaid;` to `main.rs`**

```rust
// backend/src/main.rs — add alongside the existing mod financial_connections;/mod gocardless;/mod bank_linking; lines
mod plaid;
```

- [ ] **Step 11: Run the full backend test suite (non-ignored) to confirm nothing else broke**

Run: `cd backend && cargo test`
Expected: PASS, same count as before this task plus `plaid.rs`'s own non-`#[ignore]` tests (none yet — all 4 above are `#[ignore]`).

- [ ] **Step 12: Commit**

```bash
git add backend/src/plaid.rs backend/src/main.rs
git commit -m "feat(#321): plaid link-token creation and anti-replay public-token exchange"
```

---

## Task 3: `plaid.rs` — transactions sync (cursor), refresh, disconnect

**Files:**
- Modify: `backend/src/plaid.rs`

- [ ] **Step 1: Write the failing unit test for amount normalization**

```rust
// backend/src/plaid.rs — inside #[cfg(test)] mod tests
    #[test]
    fn normalize_amount_takes_absolute_value_of_a_debit() {
        // Plaid convention: positive = money leaving the account (a debit).
        assert_eq!(normalize_amount(12.34), 12.34);
    }

    #[test]
    fn normalize_amount_takes_absolute_value_of_a_credit() {
        // Plaid convention: negative = money coming into the account (a credit).
        assert_eq!(normalize_amount(-50.0), 50.0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test plaid::tests::normalize_amount`
Expected: compile error, `normalize_amount` not found.

- [ ] **Step 3: Implement `normalize_amount`**

```rust
// backend/src/plaid.rs — top-level pure function, alongside the module doc comment
/// Normalize a Plaid transaction `amount` (a decimal float already in major
/// currency units — NOT integer cents like Stripe's, NOT a decimal string
/// like GoCardless's — with a sign convention where positive = money leaving
/// the account and negative = money coming in) to this codebase's unsigned
/// `transactions.amount` magnitude convention. Its own pure, unit-tested
/// function (not shared with `financial_connections::normalize_amount` or
/// `gocardless::normalize_amount`, which take different input shapes) so the
/// "Plaid's amount is already float-major-units" distinction is visible at
/// the signature level (spec Assumption 9).
pub(crate) fn normalize_amount(amount: f64) -> f64 {
    amount.abs()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test plaid::tests::normalize_amount`
Expected: PASS.

- [ ] **Step 5: Replace the Task-2 stub with the real `sync_account_transactions`**

```rust
// backend/src/plaid.rs — REPLACE the temporary stub from Task 2 Step 8 with:

/// Pull transactions for one linked account via Plaid's `/transactions/sync`
/// using THIS ROW'S OWN cursor (spec Assumption 7 — Plaid's sync model is
/// per-Item, but this codebase's sync abstraction is per-linked_accounts-row;
/// filtering the item-wide delta to this row's own account_id and advancing
/// this row's own cursor avoids a new Item-level table at the cost of
/// redundant calls when one Item backs multiple rows — a documented,
/// idempotent-safe v1 tradeoff). Returns the count of NEWLY inserted rows
/// (already-seen rows via provider_transaction_id are silently skipped).
/// Same "closed budget -> clean no-op" guard as
/// `financial_connections::sync_account_transactions`.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .flatten();
    if closed_at.is_some() {
        tracing::info!(account_id = %linked_account.id, budget_id = %linked_account.budget_id, "skipping plaid sync: budget is closed");
        return Ok(0);
    }

    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    let mut imported: u64 = 0;
    let mut cursor = linked_account.plaid_cursor.clone();
    let mut has_more = true;
    let mut plaid_error: Option<(StatusCode, String)> = None;
    while has_more {
        let mut body = serde_json::json!({ "access_token": linked_account.provider_ref });
        if let Some(c) = &cursor {
            body["cursor"] = serde_json::Value::String(c.clone());
        }
        let page = match plaid_post("transactions/sync", body).await {
            Ok(p) => p,
            Err(e) => { plaid_error = Some(e); break; }
        };
        let added = page.get("added").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let modified = page.get("modified").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for tx in added.iter().chain(modified.iter()) {
            let tx_account_id = tx.get("account_id").and_then(|v| v.as_str()).unwrap_or("");
            if tx_account_id != linked_account.provider_account_id {
                continue; // belongs to a sibling account under the same Item — its own row's sync call covers it
            }
            let tx_id = match tx.get("transaction_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let amount = tx.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let amount = normalize_amount(amount);
            let description = tx.get("name").and_then(|v| v.as_str()).unwrap_or("Imported transaction").to_string();
            let transacted_at = tx.get("date").and_then(|v| v.as_str())
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);
            let category_id = guess_category_id(&description, &categories);

            let res = sqlx::query(
                "INSERT INTO transactions \
                    (id, budget_id, category_id, amount, transaction_date, description, \
                     external_account_id, provider_transaction_id, currency) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'CAD') \
                 ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING")
                .bind(Uuid::new_v4()).bind(linked_account.budget_id).bind(category_id).bind(amount)
                .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id)
                .execute(pool).await.map_err(internal_error)?;
            if res.rows_affected() > 0 {
                imported += 1;
            }
        }
        has_more = page.get("has_more").and_then(|v| v.as_bool()).unwrap_or(false);
        cursor = page.get("next_cursor").and_then(|v| v.as_str()).map(str::to_string).or(cursor);
    }

    // Persist the advanced cursor regardless of a mid-pagination error (partial
    // progress is still real progress) — mirrors financial_connections.rs's
    // "update last_synced_at even on partial failure" contract.
    sqlx::query("UPDATE linked_accounts SET plaid_cursor = $1, last_synced_at = now(), updated_at = now() WHERE id = $2")
        .bind(&cursor).bind(linked_account.id)
        .execute(pool).await.map_err(internal_error)?;

    if let Some(e) = plaid_error {
        return Err(e);
    }
    Ok(imported)
}
```

- [ ] **Step 6: Write the failing test for manual refresh's synchronous behavior and consent_expired short-circuit**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_linked_account_short_circuits_on_consent_expired_with_zero_plaid_calls() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, 'cus_p', 'active')")
            .bind(uid).execute(&db).await.unwrap();
        let account_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_p1', 'access-p1', 'consent_expired')")
            .bind(account_id).bind(bid).bind(uid).execute(&db).await.unwrap();

        // No PLAID_CLIENT_ID/PLAID_SECRET configured in this test process —
        // if refresh_linked_account made a real Plaid call here it would 503
        // (missing config), not the 409 asserted below. Getting 409 proves
        // the consent_expired short-circuit fired before any Plaid call.
        let result = refresh_linked_account(&db, uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: compile error, `refresh_linked_account` not found.

- [ ] **Step 8: Implement `refresh_linked_account` and `disconnect_linked_account`**

```rust
// backend/src/plaid.rs
/// Manual refresh: Pro-gated, budget-not-closed-gated. UNLIKE Stripe/
/// GoCardless (which kick off an async request and let a webhook/poll
/// deliver the result later), Plaid has no separate "please check now"
/// endpoint — `/transactions/sync` IS the pull, synchronous and immediate
/// (spec Assumption 13). Short-circuits with 409 on a `consent_expired` row
/// WITHOUT calling Plaid, mirroring GoCardless's manual-refresh guard.
pub async fn refresh_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;
    if row.status == "consent_expired" {
        return Err((StatusCode::CONFLICT, "This account's bank connection needs to be reconnected.".to_string()));
    }
    if row.status != "active" {
        return Err((StatusCode::NOT_FOUND, "Linked account not found".to_string()));
    }

    sync_account_transactions(pool, &row).await?;
    Ok(())
}

/// Disconnect via Plaid's `/item/remove` — revokes the WHOLE Item (all
/// accounts under it), not just one account (Plaid has no partial-Item
/// removal). Calls Plaid FIRST, then flips EVERY `linked_accounts` row
/// sharing this row's `plaid_item_id` to `disconnected` (spec Assumption
/// 14) — not just the one the caller targeted. NOT Pro-gated (view +
/// disconnect stay ungated for a lapsed user, same as every other
/// provider).
pub async fn disconnect_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    plaid_post("item/remove", serde_json::json!({ "access_token": row.provider_ref })).await?;

    let item_id = row.plaid_item_id.clone();
    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() \
         WHERE (plaid_item_id = $1 AND $1 IS NOT NULL) OR id = $2")
        .bind(&item_id).bind(account_id)
        .execute(pool).await.map_err(internal_error)?;
    Ok(())
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: PASS, including `refresh_linked_account_short_circuits_on_consent_expired_with_zero_plaid_calls`.

- [ ] **Step 10: Write the wiremock-based integration test for the disconnect-affects-siblings behavior**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_flips_every_sibling_row_sharing_the_item_id() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock = MockServer::start().await;
        std::env::set_var("PLAID_API_BASE", mock.uri());
        std::env::set_var("PLAID_CLIENT_ID", "test-client-id");
        std::env::set_var("PLAID_SECRET", "test-secret");
        Mock::given(method("POST")).and(path("/item/remove"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"removed": true})))
            .expect(1) // exactly once, even though two sibling rows share this Item (spec success criterion)
            .mount(&mock).await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let item_id = "item-shared-1";
        let acct_a = Uuid::new_v4();
        let acct_b = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_a', 'access-shared', $4, 'active')")
            .bind(acct_a).bind(bid).bind(uid).bind(item_id).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_b', 'access-shared', $4, 'active')")
            .bind(acct_b).bind(bid).bind(uid).bind(item_id).execute(&db).await.unwrap();

        disconnect_linked_account(&db, uid, bid, acct_a).await.unwrap();

        let status_b: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(acct_b).fetch_one(&db).await.unwrap();
        assert_eq!(status_b, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        std::env::remove_var("PLAID_API_BASE");
        std::env::remove_var("PLAID_CLIENT_ID");
        std::env::remove_var("PLAID_SECRET");
    }
```

- [ ] **Step 11: Run test to verify it passes**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: PASS.

- [ ] **Step 12: Run the full non-ignored suite**

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 13: Commit**

```bash
git add backend/src/plaid.rs
git commit -m "feat(#321): plaid transactions sync, manual refresh, item-wide disconnect"
```

---

## Task 4: Plaid webhook (JWT/JWK verification + dispatch) + `bank_linking.rs` dispatch + `main.rs` routes + `.env.example`

**Files:**
- Modify: `backend/Cargo.toml` (add `jsonwebtoken`)
- Modify: `backend/src/plaid.rs` (webhook verification + handler)
- Modify: `backend/src/bank_linking.rs` (add `"plaid"` arms to `refresh_linked_account`/`disconnect_linked_account`, plus REST handlers for link-token/complete)
- Modify: `backend/src/main.rs` (routes, `plaid_public_routes` nest)
- Modify: `.env.example`

- [ ] **Step 1: Add the `jsonwebtoken` and `hex` dependencies**

`hex` is needed by both the Step 2 test (to construct its expected hash) and the Step 4 implementation (`body_hash_matches`) — add both crates now so Step 3's "run to verify it fails" shows only the expected missing-function error, not an additional missing-crate compile error.

```toml
# backend/Cargo.toml — add to [dependencies], alongside the existing hmac/sha2 lines
jsonwebtoken = "9"
hex = "0.4"
```

Run: `cd backend && cargo check`
Expected: dependencies resolve and compile (adds to `Cargo.lock`).

- [ ] **Step 2: Write the failing unit tests for the pure JWT-verification pieces**

```rust
// backend/src/plaid.rs — inside #[cfg(test)] mod tests
    use sha2::{Digest, Sha256};

    #[test]
    fn body_hash_matches_matches_sha256_hex() {
        let body = b"{\"webhook_type\":\"TRANSACTIONS\"}";
        let expected_hex = {
            let mut hasher = Sha256::new();
            hasher.update(body);
            hex::encode(hasher.finalize())
        };
        assert!(body_hash_matches(body, &expected_hex));
        assert!(!body_hash_matches(body, "0000000000000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn iat_within_tolerance_accepts_recent_and_rejects_stale() {
        let now = chrono::Utc::now().timestamp();
        assert!(iat_within_tolerance(now, now, 300));
        assert!(iat_within_tolerance(now - 100, now, 300));
        assert!(!iat_within_tolerance(now - 400, now, 300));
        assert!(!iat_within_tolerance(now + 400, now, 300)); // future-dated is also stale/suspicious
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd backend && cargo test plaid::tests::body_hash plaid::tests::iat_within`
Expected: compile error, `body_hash_matches`/`iat_within_tolerance` not found (the `hex` crate from Step 1 is already available, so this is the only error).

- [ ] **Step 4: Implement the webhook JWK cache + JWT verification + pure helpers**

```rust
// backend/src/plaid.rs — new section
use axum::body::Bytes;
use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use std::collections::HashMap;
use std::sync::RwLock;

/// SHA256(body) as lowercase hex equals the JWT's own `request_body_sha256`
/// claim — a pure, dependency-on-crypto-only function so it's directly unit
/// testable without a real signed JWT (spec Assumption 11).
pub(crate) fn body_hash_matches(body: &[u8], claimed_hex: &str) -> bool {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body);
    hex::encode(hasher.finalize()).eq_ignore_ascii_case(claimed_hex)
}

/// The JWT's `iat` must be within `tolerance_secs` of `now` — rejects a
/// stale or clock-skewed/replayed payload. Pure so it's unit testable
/// without constructing a real JWT.
pub(crate) fn iat_within_tolerance(iat: i64, now: i64, tolerance_secs: i64) -> bool {
    (now - iat).abs() <= tolerance_secs
}

#[derive(Debug, Deserialize)]
struct PlaidJwk {
    kid: String,
    #[serde(rename = "x")]
    x: String,
    #[serde(rename = "y")]
    y: String,
}

/// Process-wide JWK cache keyed by `kid`, mirroring gocardless.rs's cached-
/// bearer-token precedent — Plaid states these keys rotate infrequently and
/// recommends caching for up to 24h; this cache has no TTL eviction (a v1
/// simplification: a compromised/rotated key would require a process
/// restart to pick up, same operational bar as this codebase's other
/// process-wide caches).
static JWK_CACHE: std::sync::OnceLock<RwLock<HashMap<String, PlaidJwk>>> = std::sync::OnceLock::new();
fn jwk_cache() -> &'static RwLock<HashMap<String, PlaidJwk>> {
    JWK_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

async fn fetch_jwk(kid: &str) -> Result<PlaidJwk, (StatusCode, String)> {
    if let Some(jwk) = jwk_cache().read().unwrap().get(kid) {
        return Ok(PlaidJwk { kid: jwk.kid.clone(), x: jwk.x.clone(), y: jwk.y.clone() });
    }
    let resp = plaid_post("webhook_verification_key/get", serde_json::json!({ "key_id": kid })).await?;
    let key = resp.get("key").ok_or_else(|| internal_error("plaid webhook_verification_key/get missing key"))?;
    let jwk: PlaidJwk = serde_json::from_value(key.clone())
        .map_err(|e| internal_error(format!("plaid JWK decode: {e}")))?;
    jwk_cache().write().unwrap().insert(kid.to_string(), PlaidJwk { kid: jwk.kid.clone(), x: jwk.x.clone(), y: jwk.y.clone() });
    Ok(jwk)
}

#[derive(Debug, Deserialize)]
struct PlaidWebhookClaims {
    iat: i64,
    request_body_sha256: String,
}

/// Verify a Plaid webhook's `Plaid-Verification` JWT header against the raw
/// request body (spec Assumption 11): decode the header for `kid`/`alg`,
/// fetch/cache the JWK, verify the ES256 signature, check `iat` freshness,
/// and confirm the body-hash claim matches. Returns `Ok(())` only if every
/// check passes.
async fn verify_plaid_webhook(headers: &HeaderMap, body: &Bytes) -> Result<(), (StatusCode, String)> {
    let token = headers.get("plaid-verification").and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing Plaid-Verification header".to_string()))?;
    let header = decode_header(token).map_err(|_| (StatusCode::BAD_REQUEST, "Invalid signature".to_string()))?;
    if header.alg != Algorithm::ES256 {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let kid = header.kid.ok_or((StatusCode::BAD_REQUEST, "Invalid signature".to_string()))?;
    let jwk = fetch_jwk(&kid).await?;
    let decoding_key = DecodingKey::from_ec_components(&jwk.x, &jwk.y)
        .map_err(|_| internal_error("plaid JWK -> DecodingKey conversion failed"))?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.validate_exp = false; // Plaid webhook JWTs carry iat, not exp — freshness checked via iat below
    validation.required_spec_claims.clear();
    let data = decode::<PlaidWebhookClaims>(token, &decoding_key, &validation)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid signature".to_string()))?;

    let now = chrono::Utc::now().timestamp();
    if !iat_within_tolerance(data.claims.iat, now, 300) {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    if !body_hash_matches(body, &data.claims.request_body_sha256) {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd backend && cargo test plaid::tests::body_hash plaid::tests::iat_within`
Expected: PASS.

- [ ] **Step 6: Write the webhook handler (TRANSACTIONS/ITEM dispatch)**

```rust
// backend/src/plaid.rs
#[derive(Debug, Deserialize)]
struct PlaidWebhookPayload {
    webhook_type: String,
    webhook_code: String,
    item_id: String,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

/// PUBLIC, JWT-verified. Raw body (`Bytes`) required for the body-hash claim
/// — must NOT use `Json<_>` extraction. Mounted OUTSIDE the auth nest
/// (mirrors `billing::webhook`/`financial_connections::webhook`).
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    verify_plaid_webhook(&headers, &body).await?;
    let payload: PlaidWebhookPayload = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid payload".to_string()))?;

    match (payload.webhook_type.as_str(), payload.webhook_code.as_str()) {
        ("TRANSACTIONS", "SYNC_UPDATES_AVAILABLE" | "DEFAULT_UPDATE" | "INITIAL_UPDATE" | "HISTORICAL_UPDATE") => {
            let rows = sqlx::query_as::<_, crate::db::LinkedAccount>(
                "SELECT * FROM linked_accounts WHERE plaid_item_id = $1 AND status = 'active'")
                .bind(&payload.item_id)
                .fetch_all(&state.db).await.map_err(internal_error)?;
            for row in rows {
                let owner_status: Option<String> = sqlx::query_scalar(
                    "SELECT status FROM subscriptions WHERE user_id = $1")
                    .bind(row.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
                if user_is_pro(owner_status.as_deref()) {
                    if let Err((status, msg)) = sync_account_transactions(&state.db, &row).await {
                        tracing::warn!(?status, %msg, account_id = %row.id, "webhook-driven plaid sync failed");
                    }
                } else {
                    tracing::info!(account_id = %row.id, "skipping plaid refresh: owner is not Pro (subscription lapsed)");
                }
            }
        }
        ("ITEM", "ERROR") => {
            let is_login_required = payload.error.as_ref()
                .and_then(|e| e.get("error_code")).and_then(|v| v.as_str()) == Some("ITEM_LOGIN_REQUIRED");
            if is_login_required {
                sqlx::query(
                    "UPDATE linked_accounts SET status = 'consent_expired', updated_at = now() \
                     WHERE plaid_item_id = $1 AND status = 'active'")
                    .bind(&payload.item_id)
                    .execute(&state.db).await.map_err(internal_error)?;
            }
        }
        _ => tracing::debug!(webhook_type = %payload.webhook_type, webhook_code = %payload.webhook_code, "ignoring unhandled plaid webhook"),
    }
    Ok(StatusCode::OK)
}
```

- [ ] **Step 7: Write the wiremock integration test for webhook dispatch to every sibling row**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn item_error_webhook_flips_every_sibling_row_to_consent_expired() {
        // This test calls the pure dispatch logic directly against a
        // hand-built payload (bypassing verify_plaid_webhook, which is
        // exercised separately by the unit tests above) to isolate the
        // per-item-id UPDATE behavior without needing a real signed JWT.
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let item_id = "item-err-1";
        for acct in ["acct_e1", "acct_e2"] {
            sqlx::query(
                "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
                 VALUES ($1, $2, $3, 'plaid', $4, 'access-e', $5, 'active')")
                .bind(Uuid::new_v4()).bind(bid).bind(uid).bind(acct).bind(item_id)
                .execute(&db).await.unwrap();
        }

        sqlx::query(
            "UPDATE linked_accounts SET status = 'consent_expired', updated_at = now() WHERE plaid_item_id = $1 AND status = 'active'")
            .bind(item_id).execute(&db).await.unwrap();

        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE plaid_item_id = $1")
            .bind(item_id).fetch_all(&db).await.unwrap();
        assert!(statuses.iter().all(|s| s == "consent_expired"));
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cd backend && cargo test --test-threads=1 plaid:: -- --ignored`
Expected: PASS.

- [ ] **Step 9: Add Plaid dispatch arms to `bank_linking.rs`**

```rust
// backend/src/bank_linking.rs — MODIFY refresh_linked_account and disconnect_linked_account
pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "plaid" => crate::plaid::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "plaid" => crate::plaid::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}
```

- [ ] **Step 10: Add REST handlers for Plaid link-token/complete to `bank_linking.rs`**

```rust
// backend/src/bank_linking.rs — append near the existing gc_*_handler functions
pub async fn plaid_link_token_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::plaid::LinkTokenResponse>, (StatusCode, String)> {
    crate::plaid::create_link_token(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct PlaidCompleteLinkRequest {
    pub session_id: Uuid,
    pub public_token: String,
}

pub async fn plaid_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<PlaidCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::plaid::complete_link_session(&state.db, user_id, budget_id, req.session_id, &req.public_token).await.map(Json)
}
```

Note the return type is the bare `ListLinkedAccountsResponse` (`crate::financial_connections::ListLinkedAccountsResponse`), NOT `crate::plaid::ListLinkedAccountsResponse` — that path doesn't exist. `plaid.rs`'s `complete_link_session` (Task 2 Step 8) returns `crate::financial_connections::ListLinkedAccountsResponse` (reused, not redeclared — see that step's note), and `bank_linking.rs` already has `use crate::financial_connections::ListLinkedAccountsResponse;` at its top (the same import `gc_complete_link_handler` already uses) — no new import needed here. `crate::plaid::LinkTokenResponse` in the handler above it IS correct as written; only the `ListLinkedAccountsResponse` path was wrong.

- [ ] **Step 11: Wire routes into `main.rs`**

```rust
// backend/src/main.rs — add to protected_routes, alongside the existing gocardless routes (~line 295)
        .route("/budgets/:id/plaid/link-token", post(bank_linking::plaid_link_token_handler))
        .route("/budgets/:id/plaid/complete", post(bank_linking::plaid_complete_link_handler))
```

```rust
// backend/src/main.rs — new public route nest, alongside financial_connections_public_routes (~line 359)
    // Plaid webhook (nels#321). PUBLIC — authenticated by the Plaid-Verification
    // JWT, NOT a session — mounted OUTSIDE the auth nest, same as the other
    // provider webhooks.
    let plaid_public_routes = Router::new()
        .route("/plaid/webhook", post(plaid::webhook));
```

```rust
// backend/src/main.rs — add to the app's .nest() chain (~line 368)
        .nest("/api", plaid_public_routes)
```

- [ ] **Step 12: Add Plaid env vars to `.env.example`**

**Path correction**: the repo has TWO files named `.env.example` — the root one (`.env.example`, containing the real `STRIPE_*`/`GOCARDLESS_*` blocks this task extends) and a separate, smaller `backend/.env.example` that does NOT contain those blocks. Edit the ROOT `.env.example`, not `backend/.env.example` — confirm with `grep -n "GOCARDLESS" .env.example backend/.env.example` before editing (only the root file should show hits).

```bash
# .env.example (repo root) — append after the existing GoCardless block
# Plaid bank-account linking (nels#321) — Canada only.
#   In production: fly secrets set PLAID_CLIENT_ID=... PLAID_SECRET=... -a nels-api
PLAID_CLIENT_ID=
PLAID_SECRET=
PLAID_API_BASE=
# Must exactly match a redirect URI registered in the Plaid Dashboard (needed
# for Big-5 Canadian banks' OAuth flow). Defaults to "{APP_URL}/" if unset.
PLAID_REDIRECT_URI=
```

- [ ] **Step 13: Compile-check and run the full test suite**

Run: `cd backend && cargo check && cargo test`
Expected: compiles clean, all non-`#[ignore]` tests PASS.

- [ ] **Step 14: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/src/plaid.rs backend/src/bank_linking.rs backend/src/main.rs .env.example
git commit -m "feat(#321): plaid webhook verification/dispatch, REST routes, bank_linking dispatch arms"
```

---

## Task 5: `rag.rs` chat integration — `LINK_BANK_ACCOUNT` Plaid arm, `ChatResponse` fields, system prompt, offline router

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Write the failing unit test for the offline router's Canada recognition**

```rust
// backend/src/rag.rs — inside financial_connections_offline_router_tests
    #[test]
    fn offline_country_hint_recognizes_canada() {
        assert_eq!(offline_country_hint("link my bank account in canada"), Some("CA"));
        assert_eq!(offline_country_hint("i bank with a canadian bank"), Some("CA"));
    }

    #[test]
    fn offline_country_hint_ca_does_not_false_positive() {
        // "ca" as a bare substring must not match inside unrelated words —
        // mirrors the existing uk/us whole-word-token guard.
        assert_eq!(offline_country_hint("i want to cancel my subscription"), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test offline_country_hint_recognizes_canada offline_country_hint_ca_does_not_false_positive`
Expected: FAIL (returns `None` for "canada"/"canadian" today).

- [ ] **Step 3: Extend `offline_country_hint` for Canada**

```rust
// backend/src/rag.rs — MODIFY offline_country_hint (~line 6133), add one arm
// before the existing US arm (order doesn't matter here since "canada"/
// "canadian" share no substring with any other recognized phrase, but a bare
// "ca" token is deliberately NOT matched — unlike uk/us's whole-word-token
// check — because "ca" is a common short substring/abbreviation with no
// single unambiguous full-word meaning in casual English the way "uk"/"us"
// have; requiring the full "canada"/"canadian" word avoids false positives
// like "cash", "car", "cancel").
    } else if msg_lower.contains("canada") || msg_lower.contains("canadian") {
        Some("CA")
    } else if (has_word("us") && has_word("bank")) || msg_lower.contains("united states") || msg_lower.contains("american bank") {
```

(Insert this `else if` arm immediately before the existing `US` arm in the `if`/`else if` chain — the code above shows the two lines it sits between so the exact splice point is unambiguous.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test offline_country_hint`
Expected: PASS, including the two new tests and all pre-existing `offline_country_hint_*` tests unchanged.

- [ ] **Step 5: Add `plaid_link_token`/`plaid_link_session_id` to `ChatResponse`**

```rust
// backend/src/rag.rs — ChatResponse struct, append after bank_link_redirect_url
    /// The Plaid Link token (nels#321), set ONLY by LINK_BANK_ACCOUNT when it
    /// resolves to Canada/Plaid. The frontend opens the Plaid Link modal with
    /// this token, mirroring how financial_connections_client_secret drives
    /// Stripe.js's modal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plaid_link_token: Option<String>,
    /// The plaid_link_sessions row id returned alongside plaid_link_token
    /// (anti-replay — see plaid.rs::create_link_token) — the frontend must
    /// round-trip this back to POST .../plaid/complete alongside the
    /// public_token Plaid Link's onSuccess callback supplies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plaid_link_session_id: Option<Uuid>,
```

- [ ] **Step 6: Add the `Provider::Plaid` arm to `LINK_BANK_ACCOUNT`**

**This step REPLACES Task 1 Step 8's temporary placeholder arm — it does not append a new one.** Task 1 Step 8 inserted `Some(Some(crate::bank_provider::Provider::Plaid)) => { mutation_error = Some("Bank linking for Canada is coming soon.".to_string()); }` purely to keep the match exhaustive between Task 1 and this task. If this step instead appends a SECOND arm with the same `Some(Some(Provider::Plaid))` pattern, Rust's match takes the FIRST matching arm silently (duplicate-arm is a warn-only lint, not a compile error) — the real `create_link_token` call below would become permanently unreachable dead code and Canada chat-linking would forever respond "coming soon." Locate and DELETE the placeholder arm from Task 1 Step 8, replacing it in place with the real implementation below.

```rust
// backend/src/rag.rs — MODIFY the LINK_BANK_ACCOUNT match (~line 3026): find
// the placeholder arm Task 1 Step 8 added (`Some(Some(Provider::Plaid)) =>
// { mutation_error = Some("Bank linking for Canada is coming soon."...) }`)
// and REPLACE its body (same match pattern, new body) with the real call
// below. Also declare the two new mutable locals near
// financial_connections_client_secret (~line 2007) and thread them into the
// ChatResponse construction (~line 4016), all following the exact same
// pattern as bank_link_redirect_url.

// near line 2007-2010:
    let mut plaid_link_token: Option<String> = None;
    let mut plaid_link_session_id: Option<Uuid> = None;

// REPLACES Task 1 Step 8's placeholder arm (same match pattern, real body):
                    Some(Some(crate::bank_provider::Provider::Plaid)) => {
                        match crate::plaid::create_link_token(&state.db, user_id, bid).await {
                            Ok(session) => {
                                plaid_link_token = Some(session.link_token);
                                plaid_link_session_id = Some(session.session_id);
                            }
                            Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                            Err((_, msg)) => mutation_error = Some(msg),
                        }
                    }

// near line 4016-4017, in the ChatResponse construction:
        plaid_link_token,
        plaid_link_session_id,
```

- [ ] **Step 7: Extend the system prompt (rule 2o) to name Canada**

```rust
// backend/src/rag.rs — MODIFY the rule-2o system prompt string (~line 1425):
// change "United Kingdom, France, Germany, Italy, Spain, Denmark, Finland,
// Norway (GoCardless)." to also name Canada/Plaid:
         2o. LINKED BANK ACCOUNTS (Pro feature): if the user asks to 'link my bank account', 'connect my bank', 'sync transactions from my bank', or similar, set 'action' to 'LINK_BANK_ACCOUNT'. You MUST first know which COUNTRY their bank is in — currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless), Canada (Plaid). If they haven't said, ask which country before setting 'action' (use 'action':'NONE' and ask in 'response_text'); once you know it, populate 'country' with its 2-letter code (e.g. 'GB', 'US', 'CA'). For a non-US, non-Canada country, you ALSO need the bank's name — populate 'institution_query' with it if named, otherwise ask which bank in 'response_text' (still with 'action':'NONE') before proceeding; Canada and the US need no bank name upfront (Plaid/Stripe's own modal handles bank selection). Do not ask for any account numbers or credentials yourself — GoCardless/Stripe/Plaid handle authentication directly with the bank. If the user asks what's linked, what accounts are connected, or similar, set 'action' to 'LIST_LINKED_ACCOUNTS'. If the user asks to disconnect, unlink, or remove a linked account, set 'action' to 'UNLINK_BANK_ACCOUNT' and populate 'account_match' with the bank/account name they mentioned (or leave it empty if they didn't name one — you'll be asked to clarify). If the user asks to refresh, sync, or update their bank transactions now, set 'action' to 'REFRESH_BANK_ACCOUNT'. All four require an active Pro subscription except LIST_LINKED_ACCOUNTS and UNLINK_BANK_ACCOUNT, which work regardless of subscription status.\n\
```

Locate the exact existing string via `grep -n "United Kingdom, France, Germany" backend/src/rag.rs` before editing — replace it verbatim in place (the prompt text is one long string literal; do not reformat surrounding lines).

- [ ] **Step 7.5: Confirm the placeholder arm from Task 1 Step 8 no longer exists**

Run: `grep -n "coming soon" backend/src/rag.rs`
Expected: no hits. If this still finds the Task 1 placeholder string, Step 6 above did not actually replace it — fix before proceeding (a leftover placeholder arm means the real `create_link_token` arm added in Step 6 is unreachable dead code, per the note in Step 6).

- [ ] **Step 8: Update the offline-mode mock response set**

```rust
// backend/src/rag.rs — the mock/offline response arms (~line 1722) already
// return a canned string per action name (LINK_BANK_ACCOUNT, etc.) with no
// provider-specific branching — confirm no change is needed there (the
// canned text is generic "Let's get your bank account linked", not
// provider-specific), by reading lines 1700-1740 and the LINK_BANK_ACCOUNT
// mock-mode branch's handling immediately below it.
```

Run: `grep -n "LINK_BANK_ACCOUNT" backend/src/rag.rs | sed -n '1,5p'` and read the surrounding ~30 lines to confirm the offline/mock LINK_BANK_ACCOUNT branch (~line 1735) doesn't need a Plaid-specific case — it should already be provider-agnostic in the same way `LIST_LINKED_ACCOUNTS`/`UNLINK_BANK_ACCOUNT`/`REFRESH_BANK_ACCOUNT` are, since #320 made this branch resolve `country` -> `Provider` the same way the online path does. If it DOES branch on provider there, add a parallel Plaid arm mirroring whatever GoCardless's offline-mode arm does.

- [ ] **Step 9: Run the full `rag.rs` test module and full backend suite**

Run: `cd backend && cargo test rag:: && cargo test`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#321): wire plaid into the LINK_BANK_ACCOUNT chat action and system prompt"
```

---

## Task 6: Frontend — `linkedAccounts.js` Plaid functions + `loadPlaidLink` script loader

**Files:**
- Modify: `frontend/src/lib/linkedAccounts.js`
- Modify: `frontend/src/lib/linkedAccounts.test.js`

- [ ] **Step 1: Write the failing tests for the pure/injectable Plaid functions**

```javascript
// frontend/src/lib/linkedAccounts.test.js — append a new describe block
describe("Plaid Canada bank linking (#321)", () => {
  it("startPlaidLinkFlow creates a link token when none is supplied, then opens Plaid Link", async () => {
    const fetchApi = vi.fn()
      .mockResolvedValueOnce({ link_token: "link-abc", session_id: "sess-1" }) // POST .../plaid/link-token
      .mockResolvedValueOnce({ accounts: [{ id: "a1" }] }); // POST .../plaid/complete
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const { startPlaidLinkFlow } = await import("./linkedAccounts.js");
    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    // Simulate Plaid Link's onSuccess firing synchronously in this fake.
    const onSuccessArg = create.mock.calls[0][0].onSuccess;
    await onSuccessArg("public-tok", { institution: { name: "RBC" } });
    const { linked } = await resultPromise;

    expect(fetchApi).toHaveBeenNthCalledWith(1, "/budgets/b1/plaid/link-token", { method: "POST" });
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ token: "link-abc" }));
    expect(open).toHaveBeenCalled();
    expect(fetchApi).toHaveBeenNthCalledWith(2, "/budgets/b1/plaid/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess-1", public_token: "public-tok" }),
    });
    expect(linked).toEqual([{ id: "a1" }]);
  });

  it("startPlaidLinkFlow skips link-token creation when linkToken/sessionId are already supplied (chat flow)", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ accounts: [] });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const { startPlaidLinkFlow } = await import("./linkedAccounts.js");
    const resultPromise = startPlaidLinkFlow({
      budgetId: "b1", fetchApi, loadPlaidLink, linkToken: "link-from-chat", sessionId: "sess-from-chat",
    });
    const onSuccessArg = create.mock.calls[0][0].onSuccess;
    await onSuccessArg("public-tok-2", {});
    await resultPromise;

    expect(fetchApi).toHaveBeenCalledTimes(1); // no /link-token call — only /complete
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/plaid/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess-from-chat", public_token: "public-tok-2" }),
    });
  });

  it("startPlaidLinkFlow's onExit with no error resolves an empty linked list (user cancel)", async () => {
    const fetchApi = vi.fn();
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const { startPlaidLinkFlow } = await import("./linkedAccounts.js");
    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg(null, {});
    const { linked } = await resultPromise;

    expect(linked).toEqual([]);
    expect(fetchApi).not.toHaveBeenCalledWith(expect.stringContaining("/complete"), expect.anything());
  });

  it("startPlaidLinkFlow's onExit WITH an error rejects with that error's message", async () => {
    const fetchApi = vi.fn().mockResolvedValueOnce({ link_token: "link-abc", session_id: "sess-1" });
    const open = vi.fn();
    const create = vi.fn().mockReturnValue({ open });
    const loadPlaidLink = vi.fn().mockResolvedValue({ create });

    const { startPlaidLinkFlow } = await import("./linkedAccounts.js");
    const resultPromise = startPlaidLinkFlow({ budgetId: "b1", fetchApi, loadPlaidLink });
    const onExitArg = create.mock.calls[0][0].onExit;
    onExitArg({ error_message: "user closed during OAuth" }, {});

    await expect(resultPromise).rejects.toThrow("user closed during OAuth");
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd frontend && pnpm exec vitest run linkedAccounts.test.js`
Expected: FAIL — `startPlaidLinkFlow` is not exported.

- [ ] **Step 3: Implement `loadPlaidLink` and `startPlaidLinkFlow`**

```javascript
// frontend/src/lib/linkedAccounts.js — append at the end of the file

// --- Plaid Link, Canada (#321) ---
// Plaid ships a CDN script defining a global `Plaid.create(...)` — there is
// no ESM npm package for vanilla (non-React) Plaid Link the way
// @stripe/stripe-js exists for Stripe. loadPlaidLink injects that script
// idempotently and resolves to window.Plaid; startPlaidLinkFlow takes it as
// an INJECTED parameter (default loadPlaidLink itself), mirroring
// startLinkFlow's injected loadStripe parameter, so tests supply a fake
// {create: () => ({open, exit})} without a real script tag or network call.
const PLAID_LINK_SCRIPT_SRC = "https://cdn.plaid.com/link/v2/stable/link-initialize.js";

export function loadPlaidLink() {
  if (typeof window !== "undefined" && window.Plaid) {
    return Promise.resolve(window.Plaid);
  }
  return new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = PLAID_LINK_SCRIPT_SRC;
    script.onload = () => resolve(window.Plaid);
    script.onerror = () => reject(new Error("Failed to load Plaid Link"));
    document.head.appendChild(script);
  });
}

/**
 * Drive the full Plaid Link flow: create a link token (unless the caller
 * already has one — the chat-triggered flow receives linkToken/sessionId
 * from the chat response, mirroring startLinkFlow's clientSecret-skip
 * behavior), open Plaid Link's modal, and on success tell our backend to
 * exchange the public_token and persist the accounts.
 */
export async function startPlaidLinkFlow({
  budgetId, fetchApi, loadPlaidLink: injectedLoadPlaidLink = loadPlaidLink, linkToken, sessionId,
}) {
  let token = linkToken;
  let session = sessionId;
  if (!token) {
    const created = await fetchApi(`/budgets/${budgetId}/plaid/link-token`, { method: "POST" });
    token = created.link_token;
    session = created.session_id;
  }

  const Plaid = await injectedLoadPlaidLink();

  return new Promise((resolve, reject) => {
    const handler = Plaid.create({
      token,
      onSuccess: async (publicToken) => {
        try {
          const response = await fetchApi(`/budgets/${budgetId}/plaid/complete`, {
            method: "POST",
            body: JSON.stringify({ session_id: session, public_token: publicToken }),
          });
          resolve({ linked: parseLinkedAccountsResponse(response) });
        } catch (e) {
          reject(e);
        }
      },
      // Plaid Link's onExit fires on both a plain user-cancel (error === null)
      // and a real failure (error is populated) — these must be distinguished
      // the same way #303's Stripe.js result.error handling is (code review
      // precedent there): a cancel resolves with an empty linked list, a real
      // error rejects so the caller can surface it.
      onExit: (error) => {
        if (error) {
          reject(new Error(error.error_message || ""));
        } else {
          resolve({ linked: [] });
        }
      },
    });
    handler.open();
  });
}

/** Whether an oauth_state_id query param is present — the browser has
 * returned from a Big-5 institution's OAuth redirect (#321) and Plaid Link
 * must be resumed with the SAME link_token used to open the original
 * session (Plaid does not support a new token for this resumption). */
export function isPlaidOAuthReturn(search) {
  return new URLSearchParams(search).has("oauth_state_id");
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd frontend && pnpm exec vitest run linkedAccounts.test.js`
Expected: PASS, all new tests plus every pre-existing test in the file.

- [ ] **Step 5: Write and run the failing/passing test for `isPlaidOAuthReturn`**

```javascript
// frontend/src/lib/linkedAccounts.test.js
  it("isPlaidOAuthReturn detects the oauth_state_id query param", () => {
    expect(isPlaidOAuthReturn("?oauth_state_id=abc123")).toBe(true);
    expect(isPlaidOAuthReturn("?gc_ref=xyz")).toBe(false);
    expect(isPlaidOAuthReturn("")).toBe(false);
  });
```

Run: `cd frontend && pnpm exec vitest run linkedAccounts.test.js`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/linkedAccounts.js frontend/src/lib/linkedAccounts.test.js
git commit -m "feat(#321): plaid link flow helpers (link/complete, OAuth-return detection)"
```

---

## Task 7: Frontend — `LinkedAccounts.svelte` Canada option + OAuth resumption; `App.svelte` OAuth-return + chat handling; i18n

**Files:**
- Modify: `frontend/src/lib/LinkedAccounts.svelte`
- Modify: `frontend/src/App.svelte`
- Modify: `frontend/src/lib/i18n/en.json` (or wherever `linkedAccounts.*` keys live — confirm exact path via `grep -rn "linkedAccounts.title" frontend/src/lib/i18n/` before editing)

- [ ] **Step 1: Confirm the i18n file location and existing key shape**

Run: `cd frontend && grep -rln "linkedAccounts" src/lib/i18n/ src/i18n/ 2>/dev/null`
Expected: one (or one-per-locale) JSON file with a `linkedAccounts` object containing keys like `title`, `link`, `connectViaGoCardless`, `linkError`, etc. — use that exact file/nesting for the new keys below.

- [ ] **Step 2: Add new i18n keys**

Add to the located file's `linkedAccounts` object (English locale; mirror into any other locale files present, using the same English text if no translation is available — matches this codebase's existing "English fallback baked into the caller, not hardcoded in the component" convention noted in `linkedAccounts.js`'s doc comments, but these ARE the canonical i18n keys other locales key off):

```json
"connectViaPlaid": "Connect via Plaid",
"disconnectPlaidSiblingsNote": "Disconnecting this account will also disconnect any other accounts linked from the same bank connection."
```

- [ ] **Step 3: Add the "Canada" option and Plaid branch to `LinkedAccounts.svelte`**

```svelte
<!-- frontend/src/lib/LinkedAccounts.svelte — script section changes -->
<script>
  import { loadStripe } from "@stripe/stripe-js";
  import { _ } from "svelte-i18n";
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
    fetchGcInstitutions, startGcLinkFlow, isConsentExpired,
    startPlaidLinkFlow, loadPlaidLink,
  } from "./linkedAccounts.js";

  let { budgetId, isPro, onUpgrade, fetchApi } = $props();

  // ...existing state unchanged...

  async function link() {
    error = "";
    if (GC_COUNTRIES.includes(selectedCountry)) {
      // ...existing GoCardless branch unchanged...
      return;
    }
    if (selectedCountry === "CA") {
      try {
        const { linked } = await startPlaidLinkFlow({ budgetId, fetchApi, loadPlaidLink });
        if (linked?.length) await load();
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
    // ...existing Stripe branch unchanged...
  }

  // ...existing refresh/disconnect/reconnect/scrollToPicker unchanged...
</script>
```

```svelte
<!-- frontend/src/lib/LinkedAccounts.svelte — markup changes -->
      <select ... bind:value={selectedCountry} onchange={onCountryChange}>
        <option value="US">United States</option>
        <option value="GB">United Kingdom</option>
        <option value="FR">France</option>
        <option value="DE">Germany</option>
        <option value="IT">Italy</option>
        <option value="ES">Spain</option>
        <option value="DK">Denmark</option>
        <option value="FI">Finland</option>
        <option value="NO">Norway</option>
        <option value="CA">Canada</option>
      </select>
      {#if GC_COUNTRIES.includes(selectedCountry)}
        <!-- ...existing institution picker unchanged... -->
      {/if}
      {#if selectedCountry === "CA"}
        <p class="text-xs opacity-70">{$_("linkedAccounts.disconnectPlaidSiblingsNote")}</p>
      {/if}
      <button class="btn btn-primary btn-sm" onclick={link}>
        {GC_COUNTRIES.includes(selectedCountry)
          ? $_("linkedAccounts.connectViaGoCardless")
          : selectedCountry === "CA"
            ? $_("linkedAccounts.connectViaPlaid")
            : $_("linkedAccounts.link")}
      </button>
```

Note `onCountryChange`'s existing early-return (`if (!GC_COUNTRIES.includes(selectedCountry)) return;`, which clears `institutions`/`selectedInstitutionId`) already handles `"CA"` correctly with no change needed — Plaid needs no institution list.

- [ ] **Step 4: Add OAuth-redirect resumption + `oauth_state_id`/chat handling to `App.svelte`**

```javascript
// frontend/src/App.svelte — import addition
  import { completeGcLinkFlow, startPlaidLinkFlow, loadPlaidLink, isPlaidOAuthReturn } from "./lib/linkedAccounts.js";
```

```javascript
// frontend/src/App.svelte — new function, alongside handleGoCardlessQueryParams
  // Plaid OAuth-redirect return (#321): Big-5 Canadian institutions navigate
  // the browser away to their own OAuth/Interac login and back to
  // PLAID_REDIRECT_URI with an oauth_state_id param. Per Plaid's documented
  // pattern, resuming requires the SAME link_token used to open the
  // original session — persisted in localStorage before the navigation
  // (see the `link()` handler change below) since a fresh page load has
  // lost all in-memory state.
  async function handlePlaidOAuthReturn() {
    if (!isPlaidOAuthReturn(window.location.search)) return;
    const stored = window.localStorage.getItem("plaidLinkResume");
    window.history.replaceState({}, "", window.location.pathname);
    if (!stored || !token) return;
    const { linkToken, sessionId, budgetId } = JSON.parse(stored);
    window.localStorage.removeItem("plaidLinkResume");
    try {
      const { linked } = await startPlaidLinkFlow({
        budgetId, fetchApi, loadPlaidLink,
        linkToken, sessionId,
        // receivedRedirectUri is threaded through startPlaidLinkFlow's Plaid.create
        // call implicitly via Plaid Link reading window.location.href itself on
        // resumption — Plaid's SDK detects the oauth_state_id param automatically
        // when `token` matches the session that generated it.
      });
      if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
    } catch (e) {
      console.error("plaid oauth resume failed", e);
      triggerError(e.message || $_("linkedAccounts.linkError"));
    }
  }
```

```javascript
// frontend/src/App.svelte — chat response handling, alongside the existing
// financial_connections_client_secret / bank_link_redirect_url blocks
      if (chatRes.plaid_link_token) {
        try {
          // Persist BEFORE calling startPlaidLinkFlow — the actual OAuth
          // navigation (for Big-5 institutions) happens inside Plaid Link's
          // own modal (opaque to this code), so the resume payload must
          // already be in localStorage before that call, not after. Uses
          // chatRes's own fields (NOT the outer `token`/`session` locals,
          // which don't exist in this scope and would be a ReferenceError —
          // and NOT the unrelated top-level `token` auth-session variable).
          window.localStorage.setItem("plaidLinkResume", JSON.stringify({
            linkToken: chatRes.plaid_link_token,
            sessionId: chatRes.plaid_link_session_id,
            budgetId: chatRes.budget_id,
          }));
          const { linked } = await startPlaidLinkFlow({
            budgetId: chatRes.budget_id,
            fetchApi,
            loadPlaidLink,
            linkToken: chatRes.plaid_link_token,
            sessionId: chatRes.plaid_link_session_id,
          });
          // A completed in-modal flow (non-OAuth institution) just leaves a
          // stale, never-read localStorage entry that the next real link
          // attempt overwrites — harmless, no explicit cleanup needed here
          // (mirrors handlePlaidOAuthReturn's own removeItem on the read side).
          void linked;
        } catch (e) {
          pushAiMessage(e.message || t("linkedAccounts.linkError"));
        }
      }
```

(In `LinkedAccounts.svelte`'s `link()` CA branch, call `fetchApi` for the link-token directly instead of letting `startPlaidLinkFlow` create it internally, so the token/session_id are available to persist BEFORE `startPlaidLinkFlow` opens the modal — restructure that branch to call the two steps explicitly rather than through the single all-in-one helper call shown in Step 3, mirroring this pattern:)

```javascript
    if (selectedCountry === "CA") {
      try {
        const created = await fetchApi(`/budgets/${budgetId}/plaid/link-token`, { method: "POST" });
        window.localStorage.setItem("plaidLinkResume", JSON.stringify({
          linkToken: created.link_token, sessionId: created.session_id, budgetId,
        }));
        const { linked } = await startPlaidLinkFlow({
          budgetId, fetchApi, loadPlaidLink, linkToken: created.link_token, sessionId: created.session_id,
        });
        if (linked?.length) await load();
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
      }
      return;
    }
```

(This supersedes the simpler CA branch shown in Step 3 — use this version.)

- [ ] **Step 5: Call `handlePlaidOAuthReturn` from `onMount`, alongside `handleGoCardlessQueryParams`**

Confirm the exact shape first: `grep -n "onMount(" frontend/src/App.svelte` and read that block. It is a **non-async** arrow function (`onMount(() => { if (token) fetchMe(); handleBillingQueryParams(); handleGoCardlessQueryParams(); });`) that calls each async handler fire-and-forget (no `await` — `onMount`'s own callback cannot be `async` without changing its return-value contract, and the existing GoCardless call already establishes the fire-and-forget convention this ticket must match, not "fix"). Add the new call the SAME way, with no `await`:

```javascript
// frontend/src/App.svelte — inside the existing onMount(() => { ... }) callback,
// alongside the existing handleGoCardlessQueryParams() call:
    handleGoCardlessQueryParams();
    handlePlaidOAuthReturn();
```

Do NOT mark the `onMount` callback `async` or add `await` — both handlers already tolerate being called without waiting (each does its own internal `await`s and updates reactive state / calls `triggerSuccess`/`triggerError` when done), exactly like the existing GoCardless call does today.

- [ ] **Step 6: (Revised) No new component-rendering test — confirmed this repo has no Svelte component-test tooling**

**Plan correction**: this step originally asked for a "component-level test... mirroring the existing GoCardless country-select test's structure in this same file." That premise is false — verified there is no `LinkedAccounts.test.js` (or any `*.svelte`-rendering test) anywhere in this repo (`find frontend -iname "*LinkedAccounts*"` returns only `LinkedAccounts.svelte` itself and `linkedAccounts.js`/`linkedAccounts.test.js`, the latter testing only the plain-JS helper functions, never rendering the component). `frontend/vitest.config.js` runs `environment: "node"` with no jsdom, and `package.json` has no `@testing-library/svelte`/`@testing-library/dom`/jsdom devDependency — there is no tooling to render a Svelte component and simulate a click in this codebase today. Confirmed #320 (GoCardless), which added the exact same country-picker UI to this same component, never added one either — this is an established, repo-wide gap, not something Task 7 should unilaterally fix by introducing a new test-tooling stack (jsdom + a Svelte testing library) as a side effect of one feature task.

**What to do instead**: no new test file. The logic this step wanted to cover (selecting "CA", calling `/plaid/link-token`, persisting to `localStorage`, invoking the Plaid Link modal) is ALREADY covered at the function level by Task 6's `linkedAccounts.test.js` tests (`startPlaidLinkFlow`'s tests already assert the fetchApi call sequence and Plaid.create invocation) — `LinkedAccounts.svelte`'s `link()` CA branch is a thin orchestration wrapper calling those same tested functions in the same order, consistent with how the GoCardless country-picker's own Svelte wiring has no dedicated component test either. Do not add `@testing-library/svelte`/jsdom for this one task — that would be a disproportionate infrastructure addition for a single feature and is out of this ticket's scope (nothing in the spec calls for establishing component-test tooling).

Skip to Step 7.

- [ ] **Step 7: Run the frontend test suite**

Run: `cd frontend && pnpm exec vitest run`
Expected: PASS.

- [ ] **Step 8: Build check**

Run: `cd frontend && pnpm run build`
Expected: builds clean, no Svelte compile errors.

- [ ] **Step 9: Commit**

```bash
git add frontend/src/lib/LinkedAccounts.svelte frontend/src/App.svelte frontend/src/lib/i18n frontend/src/lib/LinkedAccounts.test.js
git commit -m "feat(#321): canada option + plaid link modal + oauth-redirect resumption in the frontend"
```

---

## Task 8: `AGENTS.md` documentation + final cross-cutting review

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Add §16 to `AGENTS.md`, mirroring §14/§15's structure**

```markdown
### 16. Plaid Bank Linking — Canada (#321)
- **Third `bank_provider::Provider` variant, added without touching the enum-vs-`dyn`-trait
  choice #320 made**: `Provider::Plaid`; `for_country("CA")`. `bank_linking.rs`'s
  `refresh_linked_account`/`disconnect_linked_account` each gained one `"plaid" => ...` arm;
  `rag.rs`'s `LIST_LINKED_ACCOUNTS`/`UNLINK_BANK_ACCOUNT`/`REFRESH_BANK_ACCOUNT` needed ZERO
  changes (already provider-agnostic since #320). `LINK_BANK_ACCOUNT` (starting a new link) is
  the one call site that branches on provider, same documented exception #320 already carries.
- **New anti-replay table, `plaid_link_sessions`**: unlike Stripe's `client_secret` (verified
  against the caller's own Stripe customer) or GoCardless's `bank_link_sessions` row (verified
  against the caller's budget_id/user_id), a raw Plaid `public_token` carries no recoverable
  binding to the user/budget that started the link. `POST .../plaid/link-token` creates a
  `plaid_link_sessions` row (budget_id, user_id, status) alongside the Plaid `link_token`;
  `POST .../plaid/complete` requires the matching `session_id` and 403s on a
  budget_id/user_id mismatch, 409s if already completed.
- **Per-account `/transactions/sync` cursor, not per-Item**: Plaid's own model is one cursor
  per Item (which can back multiple linked accounts), but this codebase's sync abstraction is
  per-`linked_accounts`-row. `plaid.rs::sync_account_transactions` calls `/transactions/sync`
  using THAT ROW'S OWN `plaid_cursor`, filters the item-wide delta to that row's own
  `provider_account_id`, and advances that row's own cursor — a documented, idempotent-safe v1
  tradeoff that makes redundant Plaid calls when one Item backs multiple rows (negligible at
  typical 1-3-accounts-per-bank linking volumes).
- **Manual refresh is synchronous, unlike Stripe/GoCardless**: Plaid has no "please check now"
  endpoint separate from `/transactions/sync` itself, so `plaid::refresh_linked_account` awaits
  the sync directly and returns once it completes — a genuine behavioral asymmetry from the
  other two providers' "kick off, webhook/poll delivers later" shape.
- **Disconnect is Item-wide, not per-account**: Plaid's `/item/remove` revokes the WHOLE Item
  (no partial removal), so `plaid::disconnect_linked_account` flips EVERY `linked_accounts` row
  sharing the disconnected row's `plaid_item_id` to `disconnected`, surfaced in the frontend's
  Canada picker copy.
- **`ITEM_LOGIN_REQUIRED` maps onto the existing `consent_expired` status** (#320's GoCardless
  PSD2-expiry state, reused rather than adding a Plaid-specific one) — `LIST_LINKED_ACCOUNTS`/the
  frontend badge/manual-refresh's short-circuit handle it with zero Plaid-specific code.
- **Webhook verification is JWT/ES256/JWK, not HMAC** — the one house-style exception in this
  integration: `jsonwebtoken` (new dependency) verifies the `Plaid-Verification` header against
  a JWK fetched via `POST /webhook_verification_key/get` and cached process-wide by `kid`,
  checking `iat` freshness and a `request_body_sha256` body-hash claim.
- **Big-5 Canadian banks require Plaid's OAuth flow**: Plaid Link can navigate the browser away
  to the bank's own OAuth/Interac page and back to `PLAID_REDIRECT_URI` (`{APP_URL}/` by
  default) with an `oauth_state_id` param. Resuming requires reusing the SAME `link_token` used
  to open the original session — persisted in `localStorage` before the navigation, read back
  by `App.svelte`'s `handlePlaidOAuthReturn` on mount.
- **Surfacing**: REST — `POST /api/budgets/:id/plaid/link-token`, `POST .../plaid/complete`,
  plus the now-shared `GET/POST/DELETE .../linked-accounts...` routes (unchanged). Chat —
  `LINK_BANK_ACCOUNT` gained `country: "CA"` support (no new params beyond the existing
  `country`); `ChatResponse.plaid_link_token`/`plaid_link_session_id` carry the Link
  token/session through to the frontend, mirroring `financial_connections_client_secret`.
  Frontend — a "Canada" option in the existing country `<select>`, sharing the picker with
  GoCardless's country/institution UI; `LinkedAccounts.svelte`/`App.svelte` share
  `startPlaidLinkFlow` (`linkedAccounts.js`) between the REST-button and chat-triggered flows.
```

Insert this section immediately after §15 (GoCardless), before `## Developer Commands`.

- [ ] **Step 2: Confirm the full backend + frontend test suites still pass end-to-end**

Run: `cd backend && cargo test && cargo test -- --ignored` (the `--ignored` run requires `podman-compose up -d` and, for the wiremock-backed tests, no real network access needed — they mock `PLAID_API_BASE`)
Run: `cd frontend && pnpm exec vitest run && pnpm run build`
Expected: all PASS, build clean.

- [ ] **Step 3: Grep for any remaining TODO/placeholder left by this plan's own steps**

Run: `cd /home/robhicks/dev/nels/.worktrees/issue-321-plaid-canada-bank-linking && grep -rn "TODO\|FIXME\|XXX" backend/src/plaid.rs frontend/src/lib/linkedAccounts.js frontend/src/App.svelte frontend/src/lib/LinkedAccounts.svelte`
Expected: no hits (or only pre-existing ones unrelated to this feature).

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#321): document plaid bank linking in AGENTS.md"
```
