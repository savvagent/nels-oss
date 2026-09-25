# Belvo Bank Account Data (Mexico, Brazil) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Belvo as a third bank-linking provider (Mexico, Brazil) against the existing `bank_provider::Provider`/`bank_linking.rs` dispatch layer from #320, so Pro subscribers there can link accounts via Belvo's embeddable widget, with transactions importing (MXN/BRL, no FX) and auto-refreshing via Belvo's real webhooks.

**Architecture:** A new `backend/src/belvo.rs` module mirrors `gocardless.rs`'s shape (own HTTP client, own env test seam, `require_pro`/`normalize_amount`/`sync_account_transactions` pattern) but uses HTTP Basic auth per request (no bearer-token cache) and a client-side widget-token flow (no redirect) backed by a new minimal `belvo_link_sessions` table. Belvo has real webhooks, so there is no poll job — `bank_linking.rs` gains a third `"belvo"` dispatch arm, `main.rs` gains new REST + public webhook routes, and `rag.rs`'s `LINK_BANK_ACCOUNT` chat action gains a third match arm.

**Tech Stack:** Rust (axum, sqlx/Postgres, reqwest, serde_json), Svelte 5 (runes) frontend, `wiremock` for HTTP-mocked integration tests.

**Repo conventions (Phase 0.5):**
- Test command: `cd backend && cargo test` (unit) / `cargo test -- --ignored` (DB+wiremock integration, needs `podman-compose up -d` for Postgres on `127.0.0.1:6153`). Frontend: `cd frontend && pnpm run test` (vitest).
- Lint: `cargo clippy` (manual, no CI gate).
- Commit format: `feat(#322): ...` / `fix(#322): ...` / `docs(#322): ...` (conventional commits, issue-scoped).
- No new Cargo/pnpm dependencies needed — `reqwest`, `base64 = "0.22"`, `wiremock`, `serial_test`, `url` are already dependencies (confirmed in `backend/Cargo.toml`).
- Deploy: release-please-gated Fly.io/Cloudflare Pages deploy off `main` — not part of this plan's scope (see Phase 4/5 of the general-development run).

---

## Task 1: Migration + `bank_provider.rs` — the `Belvo` variant

**Files:**
- Create: `backend/migrations/20260706140000_belvo_bank_account_data.sql`
- Modify: `backend/src/bank_provider.rs`

- [ ] **Step 1: Write the migration**

```sql
-- Adds Belvo (Mexico, Brazil) as a third linked_accounts provider (nels#322),
-- against the linked_accounts.provider/provider_account_id generalization
-- #320 already built. See docs/superpowers/specs/2026-07-06-belvo-bank-account-data-design.md
-- Assumptions 3 and 13.

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'belvo'));

-- One row per pending Belvo widget-token mint (spec Assumption 3). Simpler
-- than GoCardless's bank_link_sessions: Belvo's widget handles institution
-- selection AND authentication entirely client-side, so there's no
-- requisition/agreement bookkeeping to persist — just enough to
-- anti-replay-verify who completed it (see belvo.rs::complete_link_session).
CREATE TABLE IF NOT EXISTS belvo_link_sessions (
    id          UUID PRIMARY KEY,
    budget_id   UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    country     TEXT NOT NULL CHECK (country IN ('MX', 'BR')),
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS belvo_link_sessions_budget_id_idx ON belvo_link_sessions (budget_id);
```

- [ ] **Step 2: Run the migration against the local DB and verify**

Run: `cd backend && podman-compose -f ../docker-compose.yml up -d && sqlx migrate run --source ./migrations --database-url postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`
(If `podman-compose` isn't already running the `budget-rag-db` container, `podman ps -a` first — it may already exist from prior work; `podman start budget-rag-db` if it exists but is stopped.)
Expected: migration applies with no error. Confirm: `PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -c "\d linked_accounts"` shows `linked_accounts_provider_check ... CHECK (provider = ANY (ARRAY['stripe'::text, 'gocardless'::text, 'belvo'::text]))` and `\d belvo_link_sessions` shows the new table.

- [ ] **Step 3: Extend `bank_provider.rs`'s `Provider` enum**

Modify `backend/src/bank_provider.rs`. Update the module doc comment (line 1-3) from "a closed, two-member set" to reflect three, add the `Belvo` variant, and its `as_str`/`parse`/`for_country` arms:

```rust
//! Shared bank-provider abstraction (nels#320, extended by nels#322): a
//! closed, three-member set (Stripe Financial Connections, GoCardless Bank
//! Account Data, Belvo), so enum dispatch is used instead of an
//! `async-trait`+`dyn` object — see the GoCardless spec's Assumption 8 for
//! the original tradeoff rationale, reaffirmed by the Belvo spec's
//! Assumption 14 (a third provider costs one match arm per site, not a
//! rewrite).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    GoCardless,
    Belvo,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::GoCardless => "gocardless",
            Provider::Belvo => "belvo",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "stripe" => Some(Provider::Stripe),
            "gocardless" => Some(Provider::GoCardless),
            "belvo" => Some(Provider::Belvo),
            _ => None,
        }
    }

    /// Map an ISO 3166-1 alpha-2 country code (case-insensitive) to the
    /// provider that covers it. `US` -> Stripe; the 8 GoCardless launch
    /// countries (nels#320) -> GoCardless; `MX`/`BR` -> Belvo (nels#322);
    /// anything else -> None (caller must ask the user for a supported
    /// country, never guess).
    pub fn for_country(cc: &str) -> Option<Provider> {
        match cc.to_uppercase().as_str() {
            "US" => Some(Provider::Stripe),
            "GB" | "FR" | "DE" | "IT" | "ES" | "DK" | "FI" | "NO" => Some(Provider::GoCardless),
            "MX" | "BR" => Some(Provider::Belvo),
            _ => None,
        }
    }

    /// (code, display name) pairs for every supported country, in the order
    /// the tickets list them — used both by the REST institutions-country
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
            ("MX", "Mexico"),
            ("BR", "Brazil"),
        ]
    }
}
```

- [ ] **Step 4: Add/update unit tests in the same file's `#[cfg(test)]` module**

Add these (alongside the existing tests — extend, don't replace, `for_country_maps_gocardless_countries` and `for_country_rejects_unsupported` since `BR` moves from "rejected" to "accepted"):

```rust
    #[test]
    fn for_country_maps_belvo_countries() {
        assert_eq!(Provider::for_country("MX"), Some(Provider::Belvo));
        assert_eq!(Provider::for_country("BR"), Some(Provider::Belvo));
    }

    #[test]
    fn for_country_is_case_insensitive_for_belvo() {
        assert_eq!(Provider::for_country("mx"), Some(Provider::Belvo));
        assert_eq!(Provider::for_country("br"), Some(Provider::Belvo));
    }

    #[test]
    fn supported_countries_lists_all_eleven() {
        assert_eq!(Provider::supported_countries().len(), 11);
    }

    #[test]
    fn belvo_round_trips_through_parse() {
        assert_eq!(Provider::parse(Provider::Belvo.as_str()), Some(Provider::Belvo));
    }
```

Update the existing `fn for_country_rejects_unsupported()` test — it currently asserts `Provider::for_country("BR")` is `None`; that's no longer true (Belvo now covers it). Change it to a genuinely unsupported code:

```rust
    #[test]
    fn for_country_rejects_unsupported() {
        assert_eq!(Provider::for_country("CA"), None);
        assert_eq!(Provider::for_country(""), None);
    }
```

Replace the now-incorrect `supported_countries_lists_all_nine` test (its count assertion is wrong once Mexico/Brazil are added — the `supported_countries_lists_all_eleven` test added in this same step already supersedes it):

```rust
    #[test]
    fn supported_countries_lists_all_nine() {
        assert_eq!(Provider::supported_countries().len(), 9);
    }
```

becomes (delete this test entirely — `supported_countries_lists_all_eleven`, added above, is its replacement):

```rust
    // (removed: supported_countries_lists_all_nine — superseded by
    // supported_countries_lists_all_eleven above, added in this same step)
```

- [ ] **Step 5: Run tests**

Run: `cd backend && cargo test bank_provider`
Expected: all `bank_provider::tests::*` pass, including the new/updated ones.

- [ ] **Step 6: Commit**

```bash
git add backend/migrations/20260706140000_belvo_bank_account_data.sql backend/src/bank_provider.rs
git commit -m "feat(#322): add Belvo provider migration and Provider::Belvo variant"
```

---

## Task 2: `belvo.rs` — the Belvo integration core

**Files:**
- Create: `backend/src/belvo.rs`
- Modify: `backend/src/main.rs:33` (add `mod belvo;` alongside the existing `mod gocardless;`)

- [ ] **Step 1: Create `backend/src/belvo.rs` with HTTP helpers, guards, and pure functions**

```rust
//! Belvo bank-account linking (nels#322): Mexico and Brazil, via Belvo's
//! embeddable Connect Widget (client-side, unlike GoCardless's full-page
//! redirect) and real webhooks (unlike GoCardless, which has none — see
//! `gocardless.rs`'s doc comment). Mirrors `gocardless.rs`'s "own HTTP
//! client, own env test seam" convention, but HTTP Basic auth per request
//! (no bearer-token cache — spec Assumption 1) and webhook-driven, not
//! polled (spec Assumption 6). See
//! docs/superpowers/specs/2026-07-06-belvo-bank-account-data-design.md.

use axum::http::StatusCode;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::billing::user_is_pro;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn belvo_api_base() -> String {
    env_opt("BELVO_API_BASE").unwrap_or_else(|| "https://api.belvo.com".to_string())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// `BELVO_SECRET_ID`/`BELVO_SECRET_PASSWORD` — Basic-auth credentials used on
/// every Belvo API call except the widget-token mint itself (Assumption 1).
fn belvo_credentials() -> Result<(String, String), (StatusCode, String)> {
    let id = env_opt("BELVO_SECRET_ID")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    let password = env_opt("BELVO_SECRET_PASSWORD")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    Ok((id, password))
}

async fn belvo_get(path: &str) -> Result<serde_json::Value, (StatusCode, String)> {
    let (id, password) = belvo_credentials()?;
    let url = format!("{}/{}", belvo_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().get(&url).basic_auth(id, Some(password)).send().await
        .map_err(|e| internal_error(format!("belvo GET {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("belvo decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "belvo API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()));
    }
    Ok(body)
}

async fn belvo_post(path: &str, json_body: &serde_json::Value) -> Result<serde_json::Value, (StatusCode, String)> {
    belvo_post_raw(path, json_body).await.and_then(|(status, body)| {
        if status.is_success() { Ok(body) } else {
            tracing::error!(?status, ?body, "belvo API error on {path}");
            Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()))
        }
    })
}

/// POST returning the raw status alongside the decoded body — needed by
/// `sync_account_transactions`, which must inspect an ERROR body's contents
/// to detect an invalid-Link response (mirrors `gocardless::gocardless_get_raw`).
async fn belvo_post_raw(path: &str, json_body: &serde_json::Value) -> Result<(reqwest::StatusCode, serde_json::Value), (StatusCode, String)> {
    let (id, password) = belvo_credentials()?;
    let url = format!("{}/{}", belvo_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().post(&url).basic_auth(id, Some(password)).json(json_body).send().await
        .map_err(|e| internal_error(format!("belvo POST {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("belvo decode {path}: {e}")))?;
    Ok((status, body))
}

async fn belvo_delete(path: &str) -> Result<(), (StatusCode, String)> {
    let (id, password) = belvo_credentials()?;
    let url = format!("{}/{}", belvo_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().delete(&url).basic_auth(id, Some(password)).send().await
        .map_err(|e| internal_error(format!("belvo DELETE {path}: {e}")))?;
    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        tracing::error!(?status, ?body, "belvo API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()));
    }
    Ok(())
}

/// Mint a short-lived widget access token (Belvo's `POST /api/token/` — this
/// ONE endpoint takes the master secret pair in the JSON body rather than as
/// Basic auth, per Belvo's own docs; every other endpoint uses Basic auth).
/// `external_id` is our own `belvo_link_sessions.id`, embedded so the created
/// Link round-trips back to the right pending session (spec Assumption 3).
async fn belvo_mint_widget_token(
    secret_id: &str,
    secret_password: &str,
    external_id: &str,
    country: &str,
) -> Result<String, (StatusCode, String)> {
    let url = format!("{}/api/token/", belvo_api_base().trim_end_matches('/'));
    let resp = http_client().post(&url)
        .json(&serde_json::json!({
            "id": secret_id,
            "password": secret_password,
            "scopes": "read_institutions,write_links",
            "fetch_resources": ["ACCOUNTS", "TRANSACTIONS"],
            "widget": {"external_id": external_id, "country_codes": [country]},
        }))
        .send().await.map_err(|e| internal_error(format!("belvo token POST: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("belvo token decode: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "belvo token error");
        return Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()));
    }
    body.get("access").and_then(|v| v.as_str()).map(str::to_string)
        .ok_or_else(|| internal_error("belvo token response missing access"))
}

async fn require_edit_or_owner(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to manage linked bank accounts".to_string()));
    }
    Ok(())
}

/// `pub(crate)` (rather than private) so `rag.rs`'s `LINK_BANK_ACCOUNT` chat
/// arm can Pro-gate BEFORE minting a widget token, same reason
/// `gocardless::require_pro` is `pub(crate)`.
pub(crate) async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}

/// Belvo transaction amounts are a plain signed(-ish) number, unlike
/// GoCardless's decimal string or Stripe's integer cents (spec Assumption
/// 10) — this codebase always stores a positive magnitude regardless of
/// direction, so this takes the absolute value.
pub(crate) fn normalize_amount(amount: f64) -> f64 {
    amount.abs()
}

/// Fallback currency when a Belvo transaction is missing its own `currency`
/// field — derived from the LINKED ACCOUNT's own country, not a single
/// hardcoded default (spec Assumption 9; contrast with `gocardless.rs`'s
/// blanket `"EUR"` default, which would be actively wrong here since Belvo
/// covers two different currency zones). The `_ => "USD"` arm is defensive
/// and practically unreachable — `Provider::for_country` only ever resolves
/// `Belvo` for `"MX"`/`"BR"`.
pub(crate) fn currency_for_country(country: &str) -> &'static str {
    match country {
        "MX" => "MXN",
        "BR" => "BRL",
        _ => "USD",
    }
}

/// Detect whether a Belvo error response indicates the Link is no longer
/// `valid` (needs reconnecting) vs. some other transient error — mirrors
/// `gocardless::is_consent_expired_error`'s loose, case-insensitive
/// substring check over a 400/404/409-class body (Belvo's exact error body
/// shape for this case is a documented-but-unverified assumption; spec §8
/// Risks). A false negative here just means a real transient-error code
/// path runs instead (safe).
pub(crate) fn is_link_invalid_error(status: reqwest::StatusCode, body: &serde_json::Value) -> bool {
    if status != reqwest::StatusCode::BAD_REQUEST
        && status != reqwest::StatusCode::NOT_FOUND
        && status != reqwest::StatusCode::CONFLICT {
        return false;
    }
    let text = body.to_string().to_lowercase();
    text.contains("invalid") || text.contains("token_required") || text.contains("unconfirmed")
}

/// Verify a Belvo webhook's `Authorization: Basic base64(user:password)`
/// header against the credentials configured when the webhook URL was
/// registered in Belvo's dashboard (spec Assumption 7 — flagged as an
/// unverified-against-live-Belvo risk in the spec's §8). Pure/testable
/// without a real header-parsing round trip.
pub(crate) fn verify_webhook_auth(header: Option<&str>, expected_user: &str, expected_password: &str) -> bool {
    let Some(h) = header else { return false };
    let Some(encoded) = h.strip_prefix("Basic ") else { return false };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else { return false };
    let Ok(decoded_str) = String::from_utf8(decoded) else { return false };
    decoded_str == format!("{expected_user}:{expected_password}")
}
```

- [ ] **Step 2: Run pure-function unit tests to confirm the module compiles so far**

Add this `#[cfg(test)]` block at the bottom of `belvo.rs` (more tests are appended in later steps/tasks — this is the first batch):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_amount_takes_absolute_value() {
        assert_eq!(normalize_amount(-15.0), 15.0);
        assert_eq!(normalize_amount(15.0), 15.0);
    }

    #[test]
    fn currency_for_country_maps_mx_and_br() {
        assert_eq!(currency_for_country("MX"), "MXN");
        assert_eq!(currency_for_country("BR"), "BRL");
    }

    #[test]
    fn currency_for_country_defensive_default_for_unknown() {
        assert_eq!(currency_for_country("XX"), "USD");
    }

    #[test]
    fn is_link_invalid_error_detects_invalid_status() {
        let body = serde_json::json!({"detail": "Link status is INVALID"});
        assert!(is_link_invalid_error(reqwest::StatusCode::BAD_REQUEST, &body));
    }

    #[test]
    fn is_link_invalid_error_detects_token_required() {
        let body = serde_json::json!({"detail": "token_required"});
        assert!(is_link_invalid_error(reqwest::StatusCode::CONFLICT, &body));
    }

    #[test]
    fn is_link_invalid_error_ignores_unrelated_errors() {
        let body = serde_json::json!({"detail": "Rate limit exceeded"});
        assert!(!is_link_invalid_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &body));
    }

    #[test]
    fn verify_webhook_auth_accepts_correct_basic_header() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"whuser:whpass");
        let header = format!("Basic {encoded}");
        assert!(verify_webhook_auth(Some(&header), "whuser", "whpass"));
    }

    #[test]
    fn verify_webhook_auth_rejects_wrong_credentials() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"whuser:wrongpass");
        let header = format!("Basic {encoded}");
        assert!(!verify_webhook_auth(Some(&header), "whuser", "whpass"));
    }

    #[test]
    fn verify_webhook_auth_rejects_missing_header() {
        assert!(!verify_webhook_auth(None, "whuser", "whpass"));
    }

    #[test]
    fn verify_webhook_auth_rejects_non_basic_scheme() {
        assert!(!verify_webhook_auth(Some("Bearer sometoken"), "whuser", "whpass"));
    }
}
```

Add `mod belvo;` to `backend/src/main.rs` right after `mod gocardless;` (line 33) so the module is registered:

```rust
mod bank_provider;
mod gocardless;
mod belvo;
mod bank_linking;
```

Run: `cd backend && cargo test belvo::tests`
Expected: all 9 new tests pass; `cargo check` compiles with no errors (there will be `dead_code` warnings for the not-yet-used pub functions until Step 3 wires them up — that's expected mid-task).

- [ ] **Step 3: Commit**

```bash
git add backend/src/belvo.rs backend/src/main.rs
git commit -m "feat(#322): add belvo.rs HTTP helpers, guards, and pure functions"
```

---

## Task 3: `belvo.rs` — link flow, sync, refresh, disconnect

**Files:**
- Modify: `backend/src/belvo.rs`

- [ ] **Step 1: Add the widget-token session start**

Append to `backend/src/belvo.rs` (after the pure functions, before the `#[cfg(test)]` module — move the `#[cfg(test)]` block to the very end of the file if it isn't already there):

```rust
#[derive(Debug, Serialize)]
pub struct BelvoWidgetSessionResponse {
    pub access_token: String,
    pub session_id: Uuid,
}

/// Start a Belvo consent flow: persist a pending `belvo_link_sessions` row,
/// mint a widget access token scoped to `country` with our session id as its
/// `external_id`, and return both to the caller — the frontend uses
/// `access_token` to initialize Belvo's embeddable widget (Assumption 2).
/// Pro-gated, Edit-or-Owner-gated, closed-budget rejected — mirrors
/// `gocardless::start_link_session`'s gate ordering exactly.
pub async fn start_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    country: &str,
) -> Result<BelvoWidgetSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO belvo_link_sessions (id, budget_id, user_id, country) VALUES ($1, $2, $3, $4)")
        .bind(session_id).bind(budget_id).bind(user_id).bind(country)
        .execute(pool).await.map_err(internal_error)?;

    let (secret_id, secret_password) = belvo_credentials()?;
    let access_token = belvo_mint_widget_token(&secret_id, &secret_password, &session_id.to_string(), country).await?;

    Ok(BelvoWidgetSessionResponse { access_token, session_id })
}
```

- [ ] **Step 2: Add `complete_link_session`**

```rust
/// Complete a Belvo consent flow: look up the pending session, verify
/// ownership (403 on mismatch — anti-replay, mirrors GoCardless's
/// session-mismatch check), re-fetch the Link and its accounts server-side
/// (trusting nothing but the `belvo_link_id` from the client), and
/// persist/reconcile each account. Returns the same
/// `ListLinkedAccountsResponse` shape both other providers return.
pub async fn complete_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    session_id: Uuid,
    belvo_link_id: &str,
) -> Result<crate::financial_connections::ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    #[derive(sqlx::FromRow)]
    struct PendingSession {
        budget_id: Uuid,
        user_id: Uuid,
        country: String,
        status: String,
    }
    let session: PendingSession = sqlx::query_as(
        "SELECT budget_id, user_id, country, status FROM belvo_link_sessions WHERE id = $1")
        .bind(session_id).fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Link session not found or expired".to_string()))?;

    if session.budget_id != budget_id || session.user_id != user_id {
        tracing::warn!(%user_id, %budget_id, "belvo link session ownership mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }
    if session.status != "pending" {
        return Err((StatusCode::CONFLICT, "This link session has already been completed".to_string()));
    }

    let link = belvo_get(&format!("api/links/{belvo_link_id}/")).await?;
    let link_external_id = link.get("external_id").and_then(|v| v.as_str()).unwrap_or("");
    if link_external_id != session_id.to_string() {
        tracing::warn!(%user_id, %budget_id, belvo_link_id, "belvo link external_id mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link does not belong to you".to_string()));
    }
    // Belvo's Link resource carries the institution's own name/code directly
    // (spec Assumption 4) — this supplies `linked_accounts.institution_name`
    // so a Belvo-linked account never ships with a blank name in "what's
    // linked", same as Stripe reading its display name off its own session.
    // NOTE: `institution_id` and `institution_name` are deliberately read
    // from the SAME `link.institution` field pending live-API verification
    // (spec §8's blanket "unverified Belvo API shapes" risk) — if a real
    // Belvo account's Link response separates a machine code from a human
    // display name (e.g. a nested `institution.name` vs. `institution.code`)
    // rather than returning one flat string, split these two lines apart
    // accordingly. This is intentional, not a copy-paste bug.
    let institution_name = link.get("institution").and_then(|v| v.as_str())
        .unwrap_or("Unknown institution").to_string();
    let institution_id = link.get("institution").and_then(|v| v.as_str()).map(str::to_string);

    let accounts_body = belvo_get(&format!("api/accounts/?link={belvo_link_id}")).await?;
    let accounts: Vec<serde_json::Value> = accounts_body.get("results").and_then(|v| v.as_array()).cloned()
        .or_else(|| accounts_body.as_array().cloned())
        .unwrap_or_default();

    let mut results = Vec::with_capacity(accounts.len());
    for acct in &accounts {
        let account_id = acct.get("id").and_then(|v| v.as_str())
            .ok_or_else(|| internal_error("belvo account missing id"))?;

        // Cross-budget re-link guard — identical rationale/ordering to
        // `gocardless::complete_link_session`'s: `provider_account_id` is
        // globally UNIQUE but scoped to the one budget it was first linked
        // into. Checked BEFORE the reconciliation lookup below (which only
        // ever matches a row already scoped to THIS budget, so it can't
        // conflict with this guard).
        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(
                    %user_id, account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "belvo account already linked to a different budget"
                );
                return Err((
                    StatusCode::CONFLICT,
                    "This bank account is already linked to a different budget. Disconnect it there first.".to_string(),
                ));
            }
        }

        let number = acct.get("number").and_then(|v| v.as_str());
        let last4 = number.map(|n| n.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());
        let display_name = acct.get("name").and_then(|v| v.as_str()).map(str::to_string)
            .or_else(|| acct.get("category").and_then(|v| v.as_str()).map(str::to_string));

        // Reconcile onto an existing consent_expired row for the SAME budget
        // + institution when last4 matches (spec Assumption 11) — Belvo/LatAm
        // accounts don't expose an IBAN (unlike GoCardless's European ones),
        // so this uses last4 + institution_id instead. Preserves the local
        // row id (and therefore transaction history) instead of inserting a
        // disconnected-looking duplicate.
        let existing_id: Option<Uuid> = if let Some(l4) = &last4 {
            sqlx::query_scalar(
                "SELECT id FROM linked_accounts \
                 WHERE budget_id = $1 AND institution_id = $2 AND status = 'consent_expired' AND last4 = $3 \
                 LIMIT 1")
                .bind(budget_id).bind(&institution_id).bind(l4)
                .fetch_optional(pool).await.map_err(internal_error)?
        } else { None };

        let row = if let Some(id) = existing_id {
            sqlx::query_as::<_, crate::db::LinkedAccount>(
                "UPDATE linked_accounts SET \
                    provider_account_id = $2, provider_ref = $3, display_name = $4, last4 = $5, \
                    institution_name = $6, \
                    status = 'active', disconnected_at = NULL, updated_at = now() \
                 WHERE id = $1 RETURNING *")
                .bind(id).bind(account_id).bind(belvo_link_id).bind(&display_name).bind(&last4)
                .bind(&institution_name)
                .fetch_one(pool).await.map_err(internal_error)?
        } else {
            sqlx::query_as::<_, crate::db::LinkedAccount>(
                "INSERT INTO linked_accounts \
                    (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                     institution_id, institution_name, display_name, last4, country, status) \
                 VALUES ($1, $2, $3, 'belvo', $4, $5, $6, $7, $8, $9, $10, 'active') \
                 ON CONFLICT (provider_account_id) DO UPDATE SET \
                    display_name = EXCLUDED.display_name, last4 = EXCLUDED.last4, \
                    institution_name = EXCLUDED.institution_name, \
                    status = 'active', disconnected_at = NULL, updated_at = now() \
                 RETURNING *")
                .bind(Uuid::new_v4()).bind(budget_id).bind(user_id)
                .bind(account_id).bind(belvo_link_id)
                .bind(&institution_id).bind(&institution_name).bind(&display_name).bind(&last4).bind(&session.country)
                .fetch_one(pool).await.map_err(internal_error)?
        };

        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial belvo sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    sqlx::query("UPDATE belvo_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(session_id).execute(pool).await.map_err(internal_error)?;

    Ok(crate::financial_connections::ListLinkedAccountsResponse { accounts: results })
}
```

- [ ] **Step 3: Add `sync_account_transactions`, `refresh_linked_account`, `disconnect_linked_account`**

```rust
/// Pull transactions for one linked Belvo account and idempotently insert new
/// rows. Called from: best-effort initial complete, manual refresh, and the
/// webhook handler (Task 4) — one function, three callers, same shape as
/// both existing providers. Detects an invalid Link via the transactions
/// call's own error response (spec Assumption 8/12) and flips the local row
/// to 'consent_expired' rather than propagating a generic error — reuses the
/// SAME status value GoCardless's PSD2 expiry already established, per spec
/// Assumption 12.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if closed_at.is_some() {
        tracing::info!(account_id = %linked_account.id, "skipping belvo sync: budget is closed");
        return Ok(0);
    }

    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    let date_from = linked_account.last_synced_at
        .unwrap_or(linked_account.created_at)
        .format("%Y-%m-%d").to_string();
    let date_to = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let (status, body) = belvo_post_raw("api/transactions/", &serde_json::json!({
        "link": linked_account.provider_ref,
        "account": linked_account.provider_account_id,
        "date_from": date_from,
        "date_to": date_to,
        "save_data": true,
    })).await?;

    if !status.is_success() {
        if is_link_invalid_error(status, &body) {
            sqlx::query("UPDATE linked_accounts SET status = 'consent_expired', updated_at = now() WHERE id = $1")
                .bind(linked_account.id).execute(pool).await.map_err(internal_error)?;
            tracing::info!(account_id = %linked_account.id, "belvo link invalid — flagged for reconnect");
            return Ok(0);
        }
        tracing::error!(?status, ?body, "belvo transactions error");
        return Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()));
    }

    // Belvo returns either a bare array or a `{"results": [...]}` envelope
    // depending on endpoint/version (documented-but-unverified — spec §8
    // Risks); handling both is cheap and avoids depending on which shape
    // this specific account/region returns.
    let txs: Vec<serde_json::Value> = body.as_array().cloned()
        .or_else(|| body.get("results").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let mut imported: u64 = 0;
    for tx in &txs {
        let tx_id = tx.get("id").and_then(|v| v.as_str());
        let Some(tx_id) = tx_id else {
            tracing::warn!(account_id = %linked_account.id, raw_tx = %tx, "belvo sync: dropping transaction with no id");
            continue;
        };
        let raw_amount = tx.get("amount").and_then(|v| v.as_f64());
        let Some(raw_amount) = raw_amount else {
            tracing::warn!(account_id = %linked_account.id, tx_id, "belvo sync: dropping transaction with a malformed amount");
            continue;
        };
        let amount = normalize_amount(raw_amount);
        let currency = tx.get("currency").and_then(|v| v.as_str()).map(str::to_string)
            .unwrap_or_else(|| {
                let fallback = currency_for_country(linked_account.country.as_deref().unwrap_or(""));
                tracing::warn!(account_id = %linked_account.id, tx_id, fallback, "belvo sync: transaction missing currency; falling back to the linked account's own country currency");
                fallback.to_string()
            });
        let description = tx.get("description").and_then(|v| v.as_str())
            .unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("value_date").and_then(|v| v.as_str())
            .or_else(|| tx.get("accounting_date").and_then(|v| v.as_str()))
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

/// Manual refresh. Belvo's own webhook will ALSO fire once fresh data is
/// ready (this just asks Belvo to check now, same relationship Stripe's
/// manual-refresh-vs-webhook has) — short-circuits with a clear message on
/// an already `consent_expired` account WITHOUT calling Belvo at all,
/// mirroring GoCardless's identical short-circuit.
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
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'belvo'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    if linked.status == "consent_expired" {
        return Err((StatusCode::CONFLICT, "This account's bank connection has expired — reconnect it to keep syncing.".to_string()));
    }
    if linked.status != "active" {
        return Err((StatusCode::NOT_FOUND, "Linked account not found".to_string()));
    }

    sync_account_transactions(pool, &linked).await?;
    Ok(())
}

/// Disconnect: delete the Belvo Link FIRST (ends bank-side access), then
/// flip the local row — mirrors both existing providers' ordering. NOT
/// Pro-gated (spec Assumption 15).
pub async fn disconnect_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let provider_ref: String = sqlx::query_scalar(
        "SELECT provider_ref FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'belvo'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    belvo_delete(&format!("api/links/{provider_ref}/")).await?;

    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() WHERE id = $1")
        .bind(account_id).execute(pool).await.map_err(internal_error)?;
    Ok(())
}
```

- [ ] **Step 4: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean (the `#[cfg(test)]` module from Task 2 must now be at the END of the file, after these new `pub` functions — move it if you haven't already).

- [ ] **Step 5: Commit**

```bash
git add backend/src/belvo.rs
git commit -m "feat(#322): add belvo link/complete/sync/refresh/disconnect flow"
```

---

## Task 4: `belvo.rs` webhook handler + `bank_linking.rs` dispatch + `main.rs` routes

**Files:**
- Modify: `backend/src/belvo.rs` (add `webhook` handler)
- Modify: `backend/src/bank_linking.rs`
- Modify: `backend/src/main.rs`

- [ ] **Step 1: Add the webhook handler to `belvo.rs`**

Append (before the final `#[cfg(test)]` module):

```rust
use axum::extract::State;
use axum::Json;
use axum::http::HeaderMap;
use crate::auth::AppState;

#[derive(Debug, Deserialize)]
pub(crate) struct BelvoWebhookPayload {
    pub webhook_type: String,
    pub webhook_code: String,
    pub link_id: String,
}

/// PUBLIC, Basic-auth-verified (spec Assumption 7 — NOT HMAC-signed like
/// Stripe's; Belvo's webhook-registration model supports an optional Basic
/// auth header instead). Mounted OUTSIDE the auth nest (Task 4's `main.rs`
/// step). Reacts to ANY webhook payload naming a `link_id` by attempting a
/// resync of every active, Belvo-provider `linked_accounts` row for that
/// Link — `sync_account_transactions` itself detects and surfaces an
/// invalid-Link response (spec Assumption 6), so this handler doesn't need
/// to branch on `webhook_type`/`webhook_code` beyond logging them.
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<BelvoWebhookPayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    let expected_user = env_opt("BELVO_WEBHOOK_USER")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    let expected_password = env_opt("BELVO_WEBHOOK_PASSWORD")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    let auth_header = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !verify_webhook_auth(auth_header, &expected_user, &expected_password) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid webhook credentials".to_string()));
    }

    tracing::debug!(
        webhook_type = %payload.webhook_type, webhook_code = %payload.webhook_code, link_id = %payload.link_id,
        "belvo webhook received"
    );

    let accounts: Vec<crate::db::LinkedAccount> = sqlx::query_as(
        "SELECT * FROM linked_accounts WHERE provider = 'belvo' AND provider_ref = $1 AND status = 'active'")
        .bind(&payload.link_id)
        .fetch_all(&state.db).await.map_err(internal_error)?;

    for account in accounts {
        let owner_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM subscriptions WHERE user_id = $1")
            .bind(account.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
        if !user_is_pro(owner_status.as_deref()) {
            tracing::info!(account_id = %account.id, "belvo webhook: skipping, owner not Pro (subscription lapsed)");
            continue;
        }
        if let Err((status, msg)) = sync_account_transactions(&state.db, &account).await {
            tracing::warn!(?status, %msg, account_id = %account.id, "belvo webhook-driven sync failed");
        }
    }
    Ok(StatusCode::OK)
}
```

- [ ] **Step 2: Add `"belvo"` dispatch arms to `bank_linking.rs`**

Modify `backend/src/bank_linking.rs`'s `refresh_linked_account`/`disconnect_linked_account`:

```rust
pub async fn refresh_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "belvo" => crate::belvo::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

pub async fn disconnect_linked_account(
    pool: &PgPool, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "belvo" => crate::belvo::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}
```

Add new REST handlers to `bank_linking.rs` (after the existing GoCardless-specific handlers, before the `#[cfg(test)]` module):

```rust
// --- Belvo-specific REST handlers (session/complete; no institutions
// endpoint — Belvo's widget renders its own institution picker, spec
// Assumption 2) ---

#[derive(serde::Deserialize)]
pub struct BelvoStartLinkRequest {
    pub country: String,
}

pub async fn belvo_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<BelvoStartLinkRequest>,
) -> Result<Json<crate::belvo::BelvoWidgetSessionResponse>, (StatusCode, String)> {
    crate::belvo::start_link_session(&state.db, user_id, budget_id, &req.country).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct BelvoCompleteLinkRequest {
    pub session_id: Uuid,
    pub belvo_link_id: String,
}

pub async fn belvo_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<BelvoCompleteLinkRequest>,
) -> Result<Json<crate::financial_connections::ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::belvo::complete_link_session(&state.db, user_id, budget_id, req.session_id, &req.belvo_link_id).await.map(Json)
}
```

- [ ] **Step 3: Wire routes in `main.rs`**

Add to `protected_routes` (`backend/src/main.rs`, right after the existing `/gocardless/institutions` route, ~line 295):

```rust
        .route("/budgets/:id/belvo/session", post(bank_linking::belvo_start_link_handler))
        .route("/budgets/:id/belvo/complete", post(bank_linking::belvo_complete_link_handler))
```

Add a new public router alongside `financial_connections_public_routes` (~line 358):

```rust
    // Belvo webhook (#322). PUBLIC — authenticated by Basic-auth header
    // (spec Assumption 7), NOT a session — mounted OUTSIDE the auth nest,
    // same as the other two providers' webhook/no-webhook precedent.
    let belvo_public_routes = Router::new()
        .route("/belvo/webhook", post(belvo::webhook));
```

Add `.nest("/api", belvo_public_routes)` to the `app` builder, alongside the existing `.nest("/api", financial_connections_public_routes)` line (~line 368):

```rust
    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .nest("/api/auth", auth_routes)
        .nest("/api", protected_routes)
        .nest("/api", admin_routes)
        .nest("/api", billing_public_routes)
        .nest("/api", financial_connections_public_routes)
        .nest("/api", belvo_public_routes)
        .with_state(state)
        .layer(cors);
```

- [ ] **Step 4: Run `cargo check` and unit tests**

Run: `cd backend && cargo check && cargo test`
Expected: compiles clean, all existing + new unit tests pass (no `#[ignore]`d tests run yet).

- [ ] **Step 5: Commit**

```bash
git add backend/src/belvo.rs backend/src/bank_linking.rs backend/src/main.rs
git commit -m "feat(#322): add belvo webhook handler, dispatch arms, and REST routes"
```

---

## Task 5: `#[ignore]`+`wiremock` integration tests for `belvo.rs`

**Files:**
- Modify: `backend/src/belvo.rs` (extend the `#[cfg(test)]` module)

- [ ] **Step 1: Add test helpers (module-private, mirrors `gocardless.rs`'s per-module convention)**

Append inside the existing `#[cfg(test)] mod tests { ... }` block in `belvo.rs`:

```rust
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
            .bind(id).bind(format!("belvo-{id}@test.example")).execute(db).await.unwrap();
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
    fn set_belvo_env(server: &MockServer) {
        std::env::set_var("BELVO_API_BASE", server.uri());
        std::env::set_var("BELVO_SECRET_ID", "sid_test");
        std::env::set_var("BELVO_SECRET_PASSWORD", "spass_test");
        std::env::set_var("BELVO_WEBHOOK_USER", "whuser_test");
        std::env::set_var("BELVO_WEBHOOK_PASSWORD", "whpass_test");
    }
    fn clear_belvo_env() {
        std::env::remove_var("BELVO_API_BASE");
        std::env::remove_var("BELVO_SECRET_ID");
        std::env::remove_var("BELVO_SECRET_PASSWORD");
        std::env::remove_var("BELVO_WEBHOOK_USER");
        std::env::remove_var("BELVO_WEBHOOK_PASSWORD");
    }
    async fn mount_token(server: &MockServer, external_id_echo: bool) {
        let _ = external_id_echo;
        Mock::given(method("POST")).and(path("/api/token/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access": "tok_x", "refresh": "ref_x"})))
            .mount(server).await;
    }

    /// Mirrors `gocardless.rs`'s `mk_linked_account` test helper, for an
    /// already-active `belvo` row.
    async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, provider_account_id: &str, country: &str) -> crate::db::LinkedAccount {
        sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, country) \
             VALUES ($1, $2, $3, 'belvo', $4, 'link_x', $5) RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(provider_account_id).bind(country)
            .fetch_one(db).await.unwrap()
    }

    /// Insert a pending `belvo_link_sessions` row directly (bypassing
    /// `start_link_session`'s HTTP calls) and return its id.
    async fn mk_pending_session(db: &PgPool, budget_id: Uuid, user_id: Uuid, country: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO belvo_link_sessions (id, budget_id, user_id, country, status) VALUES ($1, $2, $3, $4, 'pending')")
            .bind(id).bind(budget_id).bind(user_id).bind(country)
            .execute(db).await.unwrap();
        id
    }

    /// `AppState` (`backend/src/auth.rs`) carries `db` AND `cipher` — this
    /// mirrors `financial_connections.rs`'s own `test_state` helper exactly
    /// (its webhook tests need the same full `AppState`, for the same
    /// reason: the `webhook` handler below takes `State(state): State<AppState>`).
    fn test_state(db: PgPool) -> AppState {
        AppState {
            db,
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()),
        }
    }
```

- [ ] **Step 2: Write the Pro-gate rejection test**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn non_pro_user_cannot_start_belvo_link() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let result = start_link_session(&db, uid, bid, "MX").await;
        assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }
```

Run: `cd backend && podman-compose -f ../docker-compose.yml up -d && cargo test belvo::tests::non_pro_user_cannot_start_belvo_link -- --ignored`
Expected: PASS.

- [ ] **Step 3: Write the full link → sync happy-path test**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn pro_user_can_start_and_complete_belvo_link() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        mount_token(&server, true).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_belvo").await;

        let started = start_link_session(&db, uid, bid, "MX").await.expect("pro user can start a link");
        assert_eq!(started.access_token, "tok_x");

        let session_id_str = started.session_id.to_string();
        Mock::given(method("GET")).and(path("/api/links/link_1/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "link_1", "institution": "banregio_mx", "external_id": session_id_str,
            })))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/api/accounts/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "acct_1", "number": "0000001234", "name": "Checking"}
            ])))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server).await;

        let completed = complete_link_session(&db, uid, bid, started.session_id, "link_1").await.expect("complete ok");
        assert_eq!(completed.accounts.len(), 1);
        assert_eq!(completed.accounts[0].provider, "belvo");
        assert_eq!(
            completed.accounts[0].institution_name.as_deref(), Some("banregio_mx"),
            "institution_name must come from the Link's own institution field"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }
```

Run: `cd backend && cargo test belvo::tests::pro_user_can_start_and_complete_belvo_link -- --ignored`
Expected: PASS.

- [ ] **Step 4: Write the currency-passthrough-with-country-fallback and reconciliation tests**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_imports_transaction_with_currency_from_payload() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "tx_1", "amount": -150.50, "currency": "MXN", "description": "Coffee Shop", "value_date": "2026-07-01"}
            ])))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_curr", "MX").await;

        let imported = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(imported, 1);
        let (amount, currency): (f64, Option<String>) = sqlx::query_as(
            "SELECT amount, currency FROM transactions WHERE external_account_id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(amount, 150.50);
        assert_eq!(currency.as_deref(), Some("MXN"));

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_falls_back_to_country_currency_when_missing() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "tx_2", "amount": 42.0, "description": "Padaria", "value_date": "2026-07-01"}
            ])))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_nocurr", "BR").await;

        sync_account_transactions(&db, &linked).await.unwrap();
        let currency: Option<String> = sqlx::query_scalar(
            "SELECT currency FROM transactions WHERE external_account_id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(currency.as_deref(), Some("BRL"), "must fall back to the linked account's own country currency, not a hardcoded default");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_reconciles_onto_existing_consent_expired_row_by_last4() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        mount_token(&server, true).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_reconcile").await;

        let old_row_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, last4, status) \
             VALUES ($1, $2, $3, 'belvo', 'acct_old', 'link_old', 'banregio_mx', '1234', 'consent_expired')")
            .bind(old_row_id).bind(bid).bind(uid).execute(&db).await.unwrap();

        let session_id = mk_pending_session(&db, bid, uid, "MX").await;
        Mock::given(method("GET")).and(path("/api/links/link_new/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "link_new", "institution": "banregio_mx", "external_id": session_id.to_string(),
            })))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/api/accounts/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "acct_new", "number": "0000001234", "name": "Checking"}
            ])))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid, session_id, "link_new").await.expect("complete ok");
        assert_eq!(result.accounts.len(), 1);
        assert_eq!(result.accounts[0].id, old_row_id, "must reconcile onto the existing row's id, not create a new one");

        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert_eq!(total, 1, "reconciliation must not leave a duplicate row behind");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }
```

- [ ] **Step 5: Write the invalid-link detection, refresh-rejection, disconnect, and webhook tests**

```rust
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_detects_invalid_link() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"detail": "Link status is INVALID"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_invalid", "MX").await;

        let result = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(result, 0);
        let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "consent_expired");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_rejects_consent_expired_without_calling_belvo() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_ce_belvo").await;
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'belvo', 'acct_ce', 'link_ce', 'consent_expired')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_ce'")
            .fetch_one(&db).await.unwrap();

        let result = refresh_linked_account(&db, uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_deletes_link_and_marks_local_disconnected() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("DELETE")).and(path("/api/links/link_disc/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'belvo', 'acct_disc', 'link_disc')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_disc'")
            .fetch_one(&db).await.unwrap();

        disconnect_linked_account(&db, uid, bid, account_id).await.expect("disconnect ok");
        let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_rejects_missing_basic_auth() {
        set_belvo_env(&MockServer::start().await);
        let db = test_pool().await;
        let state = test_state(db.clone());
        let payload = axum::Json(BelvoWebhookPayload {
            webhook_type: "TRANSACTIONS".to_string(),
            webhook_code: "new_transactions_available".to_string(),
            link_id: "link_x".to_string(),
        });
        let result = webhook(axum::extract::State(state), HeaderMap::new(), payload).await;
        assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
        clear_belvo_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_skips_sync_for_lapsed_pro_account() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // No subscriptions row inserted at all => not Pro (lapsed/never subscribed).
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'belvo', 'acct_lapsed', 'link_lapsed', 'active')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();

        let state = test_state(db.clone());
        let mut headers = HeaderMap::new();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"whuser_test:whpass_test");
        headers.insert(axum::http::header::AUTHORIZATION, format!("Basic {encoded}").parse().unwrap());
        let payload = axum::Json(BelvoWebhookPayload {
            webhook_type: "TRANSACTIONS".to_string(),
            webhook_code: "new_transactions_available".to_string(),
            link_id: "link_lapsed".to_string(),
        });
        let result = webhook(axum::extract::State(state), headers, payload).await;
        assert_eq!(result.unwrap(), StatusCode::OK);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }
```

- [ ] **Step 6: Run the full `#[ignore]`d suite**

Run: `cd backend && cargo test belvo:: -- --ignored --test-threads=1`
Expected: all belvo tests pass. If `webhook_skips_sync_for_lapsed_pro_account`/`webhook_rejects_missing_basic_auth` fail to compile due to `AppState`'s actual shape, fix the construction per the note above and re-run.

- [ ] **Step 7: Run the full existing suite to confirm no regressions**

Run: `cd backend && cargo test && cargo test -- --ignored --test-threads=1`
Expected: ALL tests pass, including every pre-existing Stripe/GoCardless test — this task must not have touched their code paths.

- [ ] **Step 8: Commit**

```bash
git add backend/src/belvo.rs
git commit -m "test(#322): add belvo #[ignore]+wiremock integration tests"
```

---

## Task 6: `rag.rs` chat integration

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Add two new `ChatResponse` fields**

Modify `backend/src/rag.rs` (~line 141-148, right after the existing `bank_link_redirect_url` field):

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_link_redirect_url: Option<String>,
    /// The Belvo widget access token (#322), set ONLY by LINK_BANK_ACCOUNT
    /// when it resolves to Mexico/Brazil. Belvo's flow is a client-side
    /// embeddable widget (unlike GoCardless's redirect and Stripe's own
    /// hosted modal) — the frontend uses this token to initialize
    /// `cdn.belvo.io`'s widget script inline in the chat surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub belvo_widget_access_token: Option<String>,
    /// The `belvo_link_sessions` row id the widget token above was minted
    /// for (#322) — the frontend sends this back, alongside the Belvo
    /// `link_id` the widget's success callback returns, to
    /// `POST /budgets/:id/belvo/complete`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub belvo_link_session_id: Option<Uuid>,
```

- [ ] **Step 2: Add the local mutable variables**

Modify `backend/src/rag.rs` (~line 2007-2010, right after `bank_link_redirect_url`'s local):

```rust
    let mut bank_link_redirect_url: Option<String> = None;
    // Set ONLY by LINK_BANK_ACCOUNT when it resolves to Belvo (#322);
    // threaded into ChatResponse so the frontend can render the widget.
    let mut belvo_widget_access_token: Option<String> = None;
    let mut belvo_link_session_id: Option<Uuid> = None;
```

- [ ] **Step 3: Add the `Belvo` match arm to `LINK_BANK_ACCOUNT`**

Modify `backend/src/rag.rs`'s `"LINK_BANK_ACCOUNT" =>` block (~line 3026-3080), adding a third arm after the `GoCardless` one, before the closing `}` of the `match country...` block:

```rust
                    Some(Some(crate::bank_provider::Provider::Belvo)) => {
                        let cc = country.unwrap();
                        match crate::belvo::start_link_session(&state.db, user_id, bid, &cc).await {
                            Ok(session) => {
                                belvo_widget_access_token = Some(session.access_token);
                                belvo_link_session_id = Some(session.session_id);
                            }
                            Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => mutation_error = Some(msg),
                            Err((_, msg)) => mutation_error = Some(msg),
                        }
                    }
```

- [ ] **Step 4: Thread the two new fields into the final `ChatResponse` literal**

Modify `backend/src/rag.rs` (~line 4016-4017, right after `bank_link_redirect_url,`):

```rust
        bank_link_redirect_url,
        belvo_widget_access_token,
        belvo_link_session_id,
```

- [ ] **Step 5: Extend system prompt rule 2o**

Modify `backend/src/rag.rs` (~line 1425). Replace:

```
currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless). If they haven't said, ask which country before setting 'action' (use 'action':'NONE' and ask in 'response_text'); once you know it, populate 'country' with its 2-letter code (e.g. 'GB', 'US'). For a non-US country, you ALSO need the bank's name — populate 'institution_query' with it if named, otherwise ask which bank in 'response_text' (still with 'action':'NONE') before proceeding. Do not ask for any account numbers or credentials yourself — GoCardless/Stripe handle authentication directly with the bank.
```

with:

```
currently supported: United States (Stripe), United Kingdom, France, Germany, Italy, Spain, Denmark, Finland, Norway (GoCardless), Mexico, Brazil (Belvo). If they haven't said, ask which country before setting 'action' (use 'action':'NONE' and ask in 'response_text'); once you know it, populate 'country' with its 2-letter code (e.g. 'GB', 'US', 'MX', 'BR'). For a GoCardless country (UK, France, Germany, Italy, Spain, Denmark, Finland, Norway), you ALSO need the bank's name — populate 'institution_query' with it if named, otherwise ask which bank in 'response_text' (still with 'action':'NONE') before proceeding. For Mexico or Brazil (Belvo), do NOT ask for a bank name — the user picks their institution inside an embedded widget the frontend shows them; just set 'action' to 'LINK_BANK_ACCOUNT' once you have the country. Do not ask for any account numbers or credentials yourself — GoCardless/Stripe/Belvo handle authentication directly with the bank.
```

- [ ] **Step 6: Extend the offline router**

Modify `backend/src/rag.rs`'s `offline_country_hint` (~line 6133-6162), adding two new branches before the final `US` check:

```rust
    } else if msg_lower.contains("norway") || msg_lower.contains("norwegian") {
        Some("NO")
    } else if msg_lower.contains("mexico") || msg_lower.contains("méxico") || msg_lower.contains("mexican") {
        Some("MX")
    } else if msg_lower.contains("brazil") || msg_lower.contains("brasil") || msg_lower.contains("brazilian") {
        Some("BR")
    } else if (has_word("us") && has_word("bank")) || msg_lower.contains("united states") || msg_lower.contains("american bank") {
        Some("US")
    } else {
        None
    }
```

- [ ] **Step 7: Update the pre-existing test whose expectation this intentionally changes**

The existing test `offline_country_hint_returns_none_for_unrecognized` (~line 6208-6211) asserted `"link my brazilian bank account"` resolves to `None` — that was correct BEFORE this ticket added Brazil support, and is now an intentional, expected behavior change (Brazil is now a real supported country). Modify `backend/src/rag.rs`:

```rust
    #[test]
    fn offline_country_hint_recognizes_mexico_and_brazil() {
        assert_eq!(offline_country_hint("link my mexican bank account"), Some("MX"));
        assert_eq!(offline_country_hint("link my brazilian bank account"), Some("BR"));
    }
    #[test]
    fn offline_country_hint_returns_none_for_unrecognized() {
        assert_eq!(offline_country_hint("link my canadian bank account"), None);
    }
```

(Replace the old `offline_country_hint_returns_none_for_unrecognized` body — same test name, new assertion — and add the new `offline_country_hint_recognizes_mexico_and_brazil` test above it.)

- [ ] **Step 8: Run tests**

Run: `cd backend && cargo test rag::`
Expected: all `rag`-module unit tests pass, including the updated/new offline-router tests. (`cargo check` should show zero warnings about unused `belvo_widget_access_token`/`belvo_link_session_id` now that they're threaded through.)

- [ ] **Step 9: Run the full test suite (unit + ignored) once more for a regression check**

Run: `cd backend && cargo test && cargo test -- --ignored --test-threads=1`
Expected: all pass.

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#322): wire Belvo into the LINK_BANK_ACCOUNT chat action"
```

---

## Task 7: Frontend — Belvo widget, country picker, chat integration, i18n

**Files:**
- Modify: `frontend/src/lib/linkedAccounts.js`
- Modify: `frontend/src/lib/LinkedAccounts.svelte`
- Modify: `frontend/src/App.svelte`
- Modify: `frontend/src/lib/i18n/locales/en.json`, `de.json`, `es.json`, `fr.json`, `it.json`, `pt.json`
- Test: `frontend/src/lib/linkedAccounts.test.js` (extend the existing file — confirm its exact path with `ls frontend/src/lib/*.test.js` first; it sits alongside `linkedAccounts.js` per this repo's convention)

- [ ] **Step 1: Add Belvo helpers to `linkedAccounts.js`**

Append to `frontend/src/lib/linkedAccounts.js`:

```js
// --- Belvo (Mexico, Brazil) (#322) ---
// Belvo's consent flow is a client-side embeddable widget (unlike
// GoCardless's full-page redirect and unlike Stripe's SDK-driven modal) —
// these functions own the two REST round-trips; the widget script
// loading/init is `loadBelvoWidget` below so callers can swap in a fake for
// tests without loading a real external script.

/** Start a Belvo consent flow: mint a widget access token server-side. */
export async function startBelvoLinkFlow({ budgetId, country, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/belvo/session`, {
    method: "POST",
    body: JSON.stringify({ country }),
  });
}

/** Complete a Belvo consent flow after the widget's onSuccess callback fires. */
export async function completeBelvoLinkFlow({ budgetId, sessionId, belvoLinkId, fetchApi }) {
  const response = await fetchApi(`/budgets/${budgetId}/belvo/complete`, {
    method: "POST",
    body: JSON.stringify({ session_id: sessionId, belvo_link_id: belvoLinkId }),
  });
  return { linked: parseLinkedAccountsResponse(response) };
}

const BELVO_WIDGET_SRC = "https://cdn.belvo.io/belvo-widget-1-stable.js";

// Module-scoped singleton: a second call before the first resolves reuses
// the same in-flight promise; a call after it resolves is a cheap no-op
// re-injection guard.
let belvoScriptPromise = null;

/**
 * Lazily inject Belvo's widget script once, then initialize the widget with
 * the given access token. `onSuccess(linkId)`/`onExit()` are wired to the
 * widget's own callback names. Exported as a standalone function (rather
 * than inlined in the Svelte component) so it's swappable with a fake in
 * tests without loading the real external script.
 */
export function loadBelvoWidget(accessToken, { onSuccess, onExit } = {}) {
  if (!belvoScriptPromise) {
    belvoScriptPromise = new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = BELVO_WIDGET_SRC;
      script.onload = resolve;
      script.onerror = () => reject(new Error("Failed to load Belvo widget script"));
      document.head.appendChild(script);
    });
  }
  return belvoScriptPromise.then(() => {
    // `belvoSDK` is a global injected by the CDN script above; not
    // available until that script has loaded, hence the promise chain.
    // eslint-disable-next-line no-undef
    belvoSDK.createWidget(accessToken, {
      callback: (link) => onSuccess?.(link),
      onExit: () => onExit?.(),
    }).build();
  });
}
```

- [ ] **Step 2: Extend `LinkedAccounts.svelte`**

Modify `frontend/src/lib/LinkedAccounts.svelte`'s imports and constants:

```js
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
    fetchGcInstitutions, startGcLinkFlow, isConsentExpired,
    startBelvoLinkFlow, completeBelvoLinkFlow, loadBelvoWidget,
  } from "./linkedAccounts.js";

  // ...

  const GC_COUNTRIES = ["GB", "FR", "DE", "IT", "ES", "DK", "FI", "NO"];
  const BELVO_COUNTRIES = ["MX", "BR"];
  let belvoWidgetActive = $state(false);
  let belvoSessionId = $state(null);
```

Add the Belvo options to the country `<select>` (after the `NO` option):

```svelte
        <option value="NO">Norway</option>
        <option value="MX">Mexico</option>
        <option value="BR">Brazil</option>
```

Extend `link()` with a Belvo branch (before the existing GoCardless `if`, since both checks are mutually exclusive membership tests — order doesn't matter, but placing Belvo first keeps the three branches visually grouped from most-recently-added to oldest):

```js
  async function link() {
    error = "";
    if (BELVO_COUNTRIES.includes(selectedCountry)) {
      try {
        const session = await startBelvoLinkFlow({ budgetId, country: selectedCountry, fetchApi });
        belvoSessionId = session.session_id;
        belvoWidgetActive = true;
        await loadBelvoWidget(session.access_token, {
          onSuccess: async (belvoLinkId) => {
            belvoWidgetActive = false;
            try {
              const { linked } = await completeBelvoLinkFlow({ budgetId, sessionId: belvoSessionId, belvoLinkId, fetchApi });
              if (linked?.length) await load();
            } catch (e) {
              error = e.message || $_("linkedAccounts.linkError");
            }
          },
          onExit: () => { belvoWidgetActive = false; },
        });
      } catch (e) {
        error = isProGateError(e) ? $_("linkedAccounts.linkProGate") : (e.message || $_("linkedAccounts.linkError"));
        belvoWidgetActive = false;
      }
      return;
    }
    if (GC_COUNTRIES.includes(selectedCountry)) {
      // ... (unchanged, existing GoCardless branch)
```

Add the widget container to the template, right after the closing `</ul>` and before the `{#if isPro}` picker block:

```svelte
  {#if belvoWidgetActive}
    <div id="belvo-widget-container" class="my-3"></div>
  {/if}
```

- [ ] **Step 3: Wire the chat-triggered Belvo flow into `App.svelte`**

Modify `frontend/src/App.svelte`'s imports to add `startBelvoLinkFlow` is not needed here (the session already exists from the chat response) — only `completeBelvoLinkFlow`/`loadBelvoWidget`:

```js
  import { startLinkFlow, completeGcLinkFlow, completeBelvoLinkFlow, loadBelvoWidget } from "./lib/linkedAccounts.js";
```

Add component-level state near the top of the `<script>` block, alongside other `$state` declarations:

```js
  let belvoWidgetActive = $state(false);
```

Add the handling block right after the existing `if (chatRes.bank_link_redirect_url) { ... }` block (~line 1367-1369):

```js
      // Belvo's LINK_BANK_ACCOUNT chat action (#322): unlike GoCardless's
      // full-page redirect and Stripe's SDK modal, Belvo's flow is a
      // client-side embeddable widget rendered INLINE — there is no
      // navigation away and no return-trip query param to pick up on mount.
      if (chatRes.belvo_widget_access_token && chatRes.belvo_link_session_id) {
        belvoWidgetActive = true;
        try {
          await loadBelvoWidget(chatRes.belvo_widget_access_token, {
            onSuccess: async (belvoLinkId) => {
              belvoWidgetActive = false;
              try {
                const { linked } = await completeBelvoLinkFlow({
                  budgetId: chatRes.budget_id,
                  sessionId: chatRes.belvo_link_session_id,
                  belvoLinkId,
                  fetchApi,
                });
                if (linked?.length) triggerSuccess($_("linkedAccounts.gcCompleteSuccess"));
              } catch (e) {
                console.error("belvo complete failed", e);
                triggerError(e.message || $_("linkedAccounts.gcCompleteError"));
              }
            },
            onExit: () => { belvoWidgetActive = false; },
          });
        } catch (e) {
          pushAiMessage(e.message || $_("linkedAccounts.linkError"));
          belvoWidgetActive = false;
        }
      }
```

Add the widget container somewhere sensible in the chat panel's template (near where other chat-adjacent conditional UI, like the insights/budgets-list navigation triggers, are handled) — a simple conditional div is sufficient for v1:

```svelte
{#if belvoWidgetActive}
  <div id="belvo-widget-container" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
    <div class="bg-base-100 rounded-lg p-4 max-w-md w-full"></div>
  </div>
{/if}
```

Use your judgment on exact placement/styling to match this file's existing conditional-UI conventions (e.g. how any existing modal-like block is structured) — the load-bearing part is the state variable and the `loadBelvoWidget` call wiring, not the exact CSS.

- [ ] **Step 4: Add Belvo i18n keys to all 6 locale files**

Modify `frontend/src/lib/i18n/locales/en.json`'s `linkedAccounts` block, adding after `"connectViaGoCardless": "Connect",`:

```json
    "connectViaBelvo": "Connect",
```

(No new distinct strings are needed beyond this — Belvo reuses `linkProGate`/`linkError`/`gcCompleteSuccess`/`gcCompleteError` for its own flow's messages, since the wording is provider-neutral. `connectViaBelvo` exists in case a future iteration wants Belvo-specific button copy; for v1 it can read the same as `connectViaGoCardless`'s value — keep them in sync.)

Add the identical key (translated) to `de.json`, `es.json`, `fr.json`, `it.json`, `pt.json` — find each file's `linkedAccounts.connectViaGoCardless` value and add `connectViaBelvo` with the same translated text immediately after it, in each of the 5 other locale files.

Update the `link()` button label logic in `LinkedAccounts.svelte` to use it:

```svelte
      <button class="btn btn-primary btn-sm" onclick={link}>
        {GC_COUNTRIES.includes(selectedCountry) ? $_("linkedAccounts.connectViaGoCardless")
          : BELVO_COUNTRIES.includes(selectedCountry) ? $_("linkedAccounts.connectViaBelvo")
          : $_("linkedAccounts.link")}
      </button>
```

- [ ] **Step 5: Write unit tests for the new pure helpers**

First run `ls frontend/src/lib/*.test.js` to confirm the exact existing test file name for `linkedAccounts.js` (per this repo's co-located test convention), then append to that file:

```js
import { describe, it, expect, vi } from "vitest";
import { startBelvoLinkFlow, completeBelvoLinkFlow, loadBelvoWidget } from "./linkedAccounts.js";

describe("Belvo (#322)", () => {
  it("startBelvoLinkFlow posts the country to the session endpoint", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ access_token: "tok_x", session_id: "sess_1" });
    const result = await startBelvoLinkFlow({ budgetId: "b1", country: "MX", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/belvo/session", {
      method: "POST",
      body: JSON.stringify({ country: "MX" }),
    });
    expect(result.access_token).toBe("tok_x");
  });

  it("completeBelvoLinkFlow posts session_id and belvo_link_id, returns parsed accounts", async () => {
    const fetchApi = vi.fn().mockResolvedValue({ accounts: [{ id: "a1", provider: "belvo" }] });
    const result = await completeBelvoLinkFlow({ budgetId: "b1", sessionId: "sess_1", belvoLinkId: "link_1", fetchApi });
    expect(fetchApi).toHaveBeenCalledWith("/budgets/b1/belvo/complete", {
      method: "POST",
      body: JSON.stringify({ session_id: "sess_1", belvo_link_id: "link_1" }),
    });
    expect(result.linked).toEqual([{ id: "a1", provider: "belvo" }]);
  });

  it("loadBelvoWidget injects the CDN script once and calls createWidget with the token", async () => {
    document.head.innerHTML = "";
    let capturedCallback;
    // eslint-disable-next-line no-global-assign
    globalThis.belvoSDK = {
      createWidget: vi.fn((token, opts) => {
        capturedCallback = opts;
        return { build: vi.fn() };
      }),
    };
    const onSuccess = vi.fn();
    const loadPromise = loadBelvoWidget("tok_widget", { onSuccess });
    const script = document.head.querySelector('script[src*="belvo-widget"]');
    expect(script).toBeTruthy();
    script.onload();
    await loadPromise;
    expect(globalThis.belvoSDK.createWidget).toHaveBeenCalledWith("tok_widget", expect.any(Object));
    capturedCallback.callback("belvo_link_abc");
    expect(onSuccess).toHaveBeenCalledWith("belvo_link_abc");
    delete globalThis.belvoSDK;
  });
});
```

- [ ] **Step 6: Run frontend tests**

Run: `cd frontend && pnpm run test`
Expected: all tests pass, including the 3 new Belvo tests.

- [ ] **Step 7: Commit**

```bash
git add frontend/src/lib/linkedAccounts.js frontend/src/lib/LinkedAccounts.svelte frontend/src/App.svelte \
        frontend/src/lib/i18n/locales/en.json frontend/src/lib/i18n/locales/de.json \
        frontend/src/lib/i18n/locales/es.json frontend/src/lib/i18n/locales/fr.json \
        frontend/src/lib/i18n/locales/it.json frontend/src/lib/i18n/locales/pt.json
git commit -m "feat(#322): add Belvo widget, country picker, and chat integration to the frontend"
```

---

## Task 8: Documentation + final integration pass

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Add AGENTS.md §16 documenting the Belvo integration**

Read `AGENTS.md`'s existing §15 (GoCardless) to match its structure/tone exactly, then add a new §16 immediately after it, covering: Belvo's HTTP Basic-auth model (vs. GoCardless's cached bearer token), the client-side widget flow + `belvo_link_sessions` table (vs. GoCardless's redirect + `bank_link_sessions`), webhook-driven auto-refresh (vs. GoCardless's poll job — the two providers now demonstrate BOTH ends of the "does this provider have webhooks" spectrum), currency-fallback-by-country (vs. GoCardless's blanket EUR default — call out explicitly why the difference is correct, not an inconsistency), and last4+institution-based reconciliation (vs. GoCardless's IBAN-based one, since Mexican/Brazilian accounts don't have IBANs). Reference `docs/superpowers/specs/2026-07-06-belvo-bank-account-data-design.md` for the full rationale.

- [ ] **Step 2: Full-repo final check**

Run:
```bash
cd backend && cargo check && cargo clippy --all-targets -- -D warnings 2>&1 | tail -50
```
Expected: no errors. Fix any clippy warnings introduced by this plan's new code (existing pre-#322 warnings, if any, are out of scope).

Run:
```bash
cd backend && cargo test && cargo test -- --ignored --test-threads=1
```
Expected: 100% pass, including every pre-existing Stripe/GoCardless test.

Run:
```bash
cd frontend && pnpm run test
```
Expected: 100% pass.

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#322): document the Belvo integration in AGENTS.md"
```

---

## Known Plan Gaps

- **No `LinkedAccounts.svelte` component-level test** (Task 7 Step 5 only covers `linkedAccounts.js`'s pure functions). The spec's Testing Approach calls for one, but there is no existing precedent for it either — #320's own GoCardless country/institution picker shipped with no component test. Matching this codebase's actual current practice rather than introducing a new testing pattern unilaterally; a follow-up ticket to add Svelte component tests for the whole picker (Stripe/GoCardless/Belvo) would be the right place to close this gap for all three at once, not a one-off addition scoped to just Belvo.

## Plan addition beyond the spec

- **Task 1 Step 4** explicitly updates two pre-existing `bank_provider.rs` tests (`for_country_rejects_unsupported`, `supported_countries_lists_all_nine`) whose assertions become false once `MX`/`BR`/Belvo are added — the spec's "existing tests pass unchanged" success criterion refers to the Stripe/GoCardless PROVIDER tests, not these two country-list-shape tests, which necessarily change when a new country/provider is added (exactly as #320 itself had to touch #303's tests when generalizing the schema).
- **Task 6 Step 7** explicitly updates `offline_country_hint_returns_none_for_unrecognized` for the same reason — adding "brazil"/"brasil" as recognized phrases makes the old test's "brazilian → None" assertion actively wrong, not a regression.
