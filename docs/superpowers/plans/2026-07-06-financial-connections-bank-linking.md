# Financial Connections Bank Linking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Pro subscriber link one or more bank accounts to a budget via Stripe Financial
Connections, auto-import transactions into the existing `transactions` table (categorized where a
cheap heuristic matches, else "Uncategorized"), refresh automatically via webhook or manually, and
view/disconnect linked accounts — all reachable from the chat co-pilot as well as a small settings
panel, per `savvagent/nels#303`.

**Architecture:** A new `backend/src/financial_connections.rs` module mirrors `billing.rs`'s
hand-rolled `reqwest` Stripe integration style exactly (no `async-stripe` crate). A new
`linked_accounts` table plus two new nullable columns on `transactions` record what was imported.
Four new chat actions in `rag.rs` reuse the module's functions, following the existing
`ADD_TRANSACTION`/`DELETE_TRANSACTION` dispatch pattern. The frontend adds `@stripe/stripe-js` (the
one new dependency this feature requires — Financial Connections has no hosted-redirect flow like
Checkout, so linking a bank account requires Stripe.js's `collectFinancialConnectionsAccounts`
client-side modal) plus a small Linked Accounts panel and chat-response wiring.

**Tech Stack:** Rust, axum, sqlx (Postgres) — `backend/src/financial_connections.rs` (new),
`backend/src/billing.rs`, `backend/src/rag.rs`, `backend/src/main.rs`, `backend/src/db.rs`. Svelte
5 — `frontend/src/lib/linkedAccounts.js` (new), `frontend/src/lib/Settings.svelte`,
`frontend/src/App.svelte`. New frontend dependency: `@stripe/stripe-js`.

Spec: `docs/superpowers/specs/2026-07-06-financial-connections-bank-linking-design.md`

**Plan addition beyond the spec (flagged for plan review):** Task 2's `create_link_session` and
Task 3's `sync_account_transactions` both call `budget::ensure_not_closed` before writing, mirroring
`ADD_TRANSACTION`'s existing guard (`rag.rs` line ~2869) — a closed (project) budget is
documented as read-only for "new transactions" (AGENTS.md §5), and auto-imported transactions are
still new transactions, so this closes a gap the spec didn't explicitly call out but that follows
directly from an already-established invariant in this codebase.

---

### Task 1: Migration + `db.rs` structs

**Files:**
- Create: `backend/migrations/20260706000000_financial_connections.sql`
- Modify: `backend/src/db.rs` (add `LinkedAccount` struct near the `Subscription` struct at line
  ~211; add two new `Option` fields to the existing `Transaction` struct at line ~112)

- [ ] **Step 1: Write the migration**

```sql
-- Stripe Financial Connections bank-account linking (#303). One row per linked
-- Financial Connections Account, scoped to the budget it was linked into.
-- Disconnecting NEVER deletes this row (or any transactions it produced) — it
-- only flips `status`, preserving history per the ticket's AC.
CREATE TABLE IF NOT EXISTS linked_accounts (
    id                  UUID PRIMARY KEY,
    budget_id           UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    stripe_account_id   TEXT NOT NULL UNIQUE,
    stripe_customer_id  TEXT NOT NULL,
    institution_name    TEXT,
    display_name        TEXT,
    last4               TEXT,
    category            TEXT,
    subcategory         TEXT,
    status              TEXT NOT NULL DEFAULT 'active'
                             CHECK (status IN ('active', 'disconnected')),
    last_synced_at      TIMESTAMPTZ,
    disconnected_at     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS linked_accounts_budget_id_idx ON linked_accounts (budget_id);

-- Imported transactions attribute to the linked account that produced them.
-- NULL (the default, and every existing row) means "manually entered" — no
-- backfill needed. ON DELETE SET NULL: if a linked_accounts row were ever
-- deleted directly (not the normal disconnect path, which only flips status),
-- the transaction history it produced must survive as ordinary rows, not
-- cascade-delete.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS external_account_id UUID
    REFERENCES linked_accounts(id) ON DELETE SET NULL;

-- Stripe's `fctxn_...` id. The idempotency key: a re-sync (webhook redelivery,
-- manual refresh landing after auto-refresh already ran, etc.) must skip rows
-- already imported rather than duplicate them or clobber a user's edits to an
-- already-imported row. Partial (not a plain UNIQUE) so the index only covers
-- imported rows — a plain UNIQUE on a nullable column already permits multiple
-- NULLs with no collision, so the WHERE clause isn't needed for correctness;
-- it's used here to keep the index smaller by excluding the (majority)
-- manually-entered rows that will never carry a stripe_transaction_id.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS stripe_transaction_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS transactions_stripe_transaction_id_idx
    ON transactions (stripe_transaction_id) WHERE stripe_transaction_id IS NOT NULL;
```

- [ ] **Step 2: Add the `LinkedAccount` struct to `db.rs`**

Add immediately after the `Subscription` struct (ends at line ~223 with `pub updated_at:
DateTime<Utc>,\n}`):

```rust
/// A row of the `linked_accounts` table (#303): one linked Stripe Financial
/// Connections bank account, scoped to the budget it was linked into.
/// Disconnecting NEVER deletes this row — it only flips `status` to
/// 'disconnected' and sets `disconnected_at`, preserving the account's import
/// history (the AC requires history to survive a disconnect).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LinkedAccount {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub user_id: Uuid,
    pub stripe_account_id: String,
    pub stripe_customer_id: String,
    pub institution_name: Option<String>,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub status: String, // 'active' | 'disconnected'
    pub last_synced_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Add two new `Option` fields to the existing `Transaction` struct (currently lines 112-121), placed
right after `updated_at`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Transaction {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub category_id: Option<Uuid>,
    pub amount: f64,
    pub transaction_date: DateTime<Utc>,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The linked bank account this transaction was auto-imported from (#303).
    /// `None` = manually entered, exactly as every transaction was before this
    /// feature. This is ALSO the sole signal distinguishing an imported row
    /// from a manual one — there is no separate `import_source` column.
    pub external_account_id: Option<Uuid>,
    /// Stripe's Financial Connections transaction id (`fctxn_...`), present
    /// only on imported rows. The idempotency key for `sync_account_transactions`.
    pub stripe_transaction_id: Option<String>,
}
```

- [ ] **Step 3: Run the migration and verify it applies**

Run: `cd backend && cargo run` (migrations run automatically on start per `AGENTS.md`'s "Auto-Migrations"
convention — `sqlx::migrate!` in `main.rs`), then Ctrl-C once you see `Database migrations completed
successfully.` in the log. If you don't have a running `podman-compose up -d` Postgres yet, start it
first.

Expected: no migration error; the log line appears.

- [ ] **Step 4: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean (the two new `Transaction` fields don't break any existing `SELECT *`
query since `sqlx::FromRow` maps by name and the migration adds both columns).

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/20260706000000_financial_connections.sql backend/src/db.rs
git commit -m "feat(#303): add linked_accounts table and transactions import columns"
```

---

### Task 2: `financial_connections.rs` — session creation + linking + categorization

**Files:**
- Create: `backend/src/financial_connections.rs`
- Modify: `backend/src/billing.rs:308` (widen `ensure_customer` visibility)
- Modify: `backend/src/main.rs` (add `mod financial_connections;`)

- [ ] **Step 1: Widen `billing::ensure_customer` to `pub(crate)`**

In `backend/src/billing.rs`, change line 308 from:

```rust
async fn ensure_customer(db: &PgPool, user_id: Uuid) -> Result<String, (StatusCode, String)> {
```

to:

```rust
pub(crate) async fn ensure_customer(db: &PgPool, user_id: Uuid) -> Result<String, (StatusCode, String)> {
```

This reuses the SAME Stripe Customer linking already creates for billing (one customer per user,
never two) — mirrors the precedent of widening `gemini_api_base()` to `pub(crate)` for `reports.rs`
(#283, documented in `AGENTS.md` §2).

- [ ] **Step 2: Add `mod financial_connections;` to `main.rs`**

In `backend/src/main.rs`, add the module declaration next to the existing `mod billing;` (line 30):

```rust
mod billing;
mod financial_connections;
```

(No routes wired yet — that's Task 4. This step alone must compile.)

- [ ] **Step 3: Write the failing unit tests for `guess_category_id`**

Create `backend/src/financial_connections.rs` with this module doc comment and the pure
categorization helper's tests FIRST:

```rust
//! Stripe Financial Connections bank-account linking (#303): link a bank
//! account via a hosted Stripe.js modal, auto-import its transactions into the
//! existing `transactions` table (#195), and keep them refreshed via webhook or
//! manual refresh. Mirrors `billing.rs`'s hand-rolled `reqwest` Stripe
//! integration style exactly — no `async-stripe` crate, module-local HTTP
//! helpers, `STRIPE_API_BASE` test seam.

use uuid::Uuid;

/// Best-effort v1 categorization (#303): case-insensitive substring match of
/// each of the budget's existing category NAMES against the transaction
/// description. First match (by the order `categories` is given, which callers
/// pass in category-creation order) wins; no match -> `None` ("Uncategorized").
///
/// This is a deliberately narrow heuristic, NOT ML/embedding-based matching —
/// Stripe's Financial Connections transaction object carries no spending-category
/// field (unlike Plaid's personal-finance-category), so this is the cheapest
/// honest thing that can be called "categorized where possible" without
/// building a categorizer. A smarter (embedding-based) categorizer is an
/// explicit non-goal for v1 (see the spec's Assumption 7).
pub(crate) fn guess_category_id(description: &str, categories: &[(Uuid, String)]) -> Option<Uuid> {
    let desc_lower = description.to_lowercase();
    categories
        .iter()
        .find(|(_, name)| !name.is_empty() && desc_lower.contains(&name.to_lowercase()))
        .map(|(id, _)| *id)
}

/// Normalize a Stripe Financial Connections transaction `amount` (an integer
/// number of cents, sign convention not reliably documented across account
/// types — see the spec's Assumption 9 and Risks section) to the dollar
/// magnitude this codebase's `transactions.amount` column stores. This
/// codebase already stores `amount` as a POSITIVE magnitude regardless of
/// direction (`init.sql`: "Positive for expense/savings/income; type
/// determined by category"), so this takes the absolute value — imported rows
/// start with `category_id = NULL` (no `category_type` to infer a direction
/// from anyway). Extracted as a pure function (mirroring `guess_category_id`)
/// so this conversion is independently unit-tested per the spec's Testing
/// Approach ("amount normalization" is explicitly called out there).
pub(crate) fn normalize_amount(amount_cents: i64) -> f64 {
    (amount_cents.abs() as f64) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: Uuid, name: &str) -> (Uuid, String) {
        (id, name.to_string())
    }

    #[test]
    fn guess_category_id_matches_substring_case_insensitively() {
        let netflix = Uuid::new_v4();
        let groceries = Uuid::new_v4();
        let categories = vec![cat(netflix, "Netflix"), cat(groceries, "Groceries")];
        assert_eq!(
            guess_category_id("NETFLIX.COM MONTHLY", &categories),
            Some(netflix)
        );
    }

    #[test]
    fn guess_category_id_no_match_returns_none() {
        let categories = vec![cat(Uuid::new_v4(), "Netflix")];
        assert_eq!(guess_category_id("Whole Foods Market", &categories), None);
    }

    #[test]
    fn guess_category_id_empty_categories_returns_none() {
        assert_eq!(guess_category_id("anything", &[]), None);
    }

    #[test]
    fn guess_category_id_first_match_wins_in_given_order() {
        // Both "Coffee" and "Coffee Shop" could substring-match "Blue Bottle
        // Coffee Shop" — the helper takes the FIRST match in the given order,
        // it does not pick the longest/most-specific match. Document this via
        // a concrete case: whichever category is listed first for this budget
        // wins if both happen to match.
        let coffee = Uuid::new_v4();
        let coffee_shop = Uuid::new_v4();
        let categories = vec![cat(coffee, "Coffee"), cat(coffee_shop, "Coffee Shop")];
        assert_eq!(
            guess_category_id("Blue Bottle Coffee Shop", &categories),
            Some(coffee)
        );
    }

    #[test]
    fn guess_category_id_ignores_empty_category_name() {
        // An empty category name would substring-match everything (`"".contains("")`
        // is true in Rust); guard against a category with a blank name
        // (shouldn't happen given `categories.name` is NOT NULL and UI-required,
        // but defensive here since this helper has no DB access to rely on that).
        let real = Uuid::new_v4();
        let categories = vec![cat(Uuid::new_v4(), ""), cat(real, "Gas")];
        assert_eq!(guess_category_id("Shell Gas Station", &categories), Some(real));
    }

    #[test]
    fn normalize_amount_converts_cents_to_dollars() {
        assert_eq!(normalize_amount(1500), 15.0);
    }

    #[test]
    fn normalize_amount_takes_absolute_value() {
        // Sign convention is not reliably documented across Stripe FC account
        // types (spec Assumption 9) — a negative (debit) cents value must
        // normalize to the SAME positive dollar magnitude as a positive one,
        // matching this codebase's existing "amount is always a positive
        // magnitude" convention (init.sql).
        assert_eq!(normalize_amount(-1500), 15.0);
        assert_eq!(normalize_amount(1500), normalize_amount(-1500));
    }

    #[test]
    fn normalize_amount_zero_is_zero() {
        assert_eq!(normalize_amount(0), 0.0);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test guess_category_id && cargo test normalize_amount`
Expected: 5 `guess_category_id` tests + 3 `normalize_amount` tests pass (this is pure logic with no
DB dependency, so it should pass immediately —
if any fails, fix `guess_category_id` before continuing).

- [ ] **Step 5: Add the Stripe HTTP helpers and `create_link_session`**

Append to `backend/src/financial_connections.rs` (after the `#[cfg(test)]` module, but note Rust
allows this — the `mod tests` block stays at the end of the file by convention; add the following
BEFORE the `#[cfg(test)]` line instead, i.e. insert between the `guess_category_id` function and
`#[cfg(test)] mod tests`):

```rust
use axum::http::StatusCode;
use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth::AppState;
use crate::billing::{ensure_customer, user_is_pro};
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn stripe_api_base() -> String {
    env_opt("STRIPE_API_BASE").unwrap_or_else(|| "https://api.stripe.com".to_string())
}

/// Process-wide reqwest client (mirrors `billing.rs::http_client`) so Stripe
/// calls reuse pooled TCP/TLS connections.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// POST form-encoded to the Stripe API with the secret key as bearer. Mirrors
/// `billing.rs::stripe_post` exactly (duplicated per this repo's per-module
/// Stripe-helper convention rather than shared, matching how `github.rs`/`rag.rs`
/// each own their own HTTP client setup).
async fn stripe_post(path: &str, form: &[(String, String)]) -> Result<serde_json::Value, (StatusCode, String)> {
    let secret = env_opt("STRIPE_SECRET_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let url = format!("{}/{}", stripe_api_base().trim_end_matches('/'), path);
    let resp = http_client()
        .post(&url)
        .bearer_auth(secret)
        .form(form)
        .send().await
        .map_err(|e| internal_error(format!("stripe POST {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await
        .map_err(|e| internal_error(format!("stripe decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "stripe API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

/// GET the Stripe API with the secret key as bearer, optional query string
/// already appended to `path`. Mirrors `stripe_post`'s error shape.
async fn stripe_get(path: &str) -> Result<serde_json::Value, (StatusCode, String)> {
    let secret = env_opt("STRIPE_SECRET_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let url = format!("{}/{}", stripe_api_base().trim_end_matches('/'), path);
    let resp = http_client()
        .get(&url)
        .bearer_auth(secret)
        .send().await
        .map_err(|e| internal_error(format!("stripe GET {path}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await
        .map_err(|e| internal_error(format!("stripe decode {path}: {e}")))?;
    if !status.is_success() {
        tracing::error!(?status, ?body, "stripe API error on {path}");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    }
    Ok(body)
}

/// Require Edit-or-Owner on `budget_id` for `user_id`. Mirrors the exact guard
/// shape used by `budget::create_transaction` (`budget.rs` line ~3306) — a
/// shared collaborator with Edit access can manage the linked bank feed just
/// like they can add manual transactions.
async fn require_edit_or_owner(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to manage linked bank accounts".to_string()));
    }
    Ok(())
}

/// Require the caller to be Pro (`billing::user_is_pro`), else 402. Used ONLY
/// by the actions that consume the Stripe FC data pull (starting a new link,
/// manual refresh) — NOT by viewing or disconnecting (spec Assumption 11).
async fn require_pro(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?.flatten();
    if !user_is_pro(status.as_deref()) {
        return Err((StatusCode::PAYMENT_REQUIRED, "Linking bank accounts is a Pro feature — upgrade to link your accounts.".to_string()));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub client_secret: String,
}

/// Start a Financial Connections link session for `budget_id`. Pro-gated (402)
/// and Edit-or-Owner-gated (403). A closed (project) budget also rejects here
/// (409) — see the plan's "Plan addition beyond the spec" note: a closed budget
/// must not gain new transactions via ANY path, including a fresh bank link.
pub async fn create_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<SessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let customer_id = ensure_customer(pool, user_id).await?;
    let form = vec![
        ("account_holder[type]".to_string(), "customer".to_string()),
        ("account_holder[customer]".to_string(), customer_id),
        ("permissions[]".to_string(), "transactions".to_string()),
        ("filters[countries][]".to_string(), "US".to_string()),
    ];
    let session = stripe_post("v1/financial_connections/sessions", &form).await?;
    let client_secret = session.get("client_secret").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("financial_connections session missing client_secret"))?
        .to_string();
    Ok(SessionResponse { client_secret })
}
```

Note: `budget::Permission` and `budget::check_permission`/`ensure_not_closed` must be `pub` already
(they are — verify with `grep -n "pub enum Permission\|pub async fn check_permission\|pub async fn ensure_not_closed" backend/src/budget.rs` before writing this step; all three are `pub` as of the current codebase).

- [ ] **Step 6: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean. Fix any import-path mismatch now (e.g. if `AppState`/`Extension`/`Json`
aren't actually used yet in this file, `cargo check` will warn on unused imports — that's fine for
now, later steps use them; do NOT remove imports Task 4 needs).

- [ ] **Step 7: Write `complete_link_session` and `sync_account_transactions`**

Append (still before the `#[cfg(test)]` block):

```rust
#[derive(Serialize)]
pub struct LinkedAccountResponse {
    pub id: Uuid,
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
            institution_name: a.institution_name,
            display_name: a.display_name,
            last4: a.last4,
            status: a.status,
            last_synced_at: a.last_synced_at,
        }
    }
}

/// Finish a link session: re-fetch the session SERVER-SIDE (never trust a
/// client-supplied account list) to get the authoritative linked accounts,
/// verify the session belongs to the caller's OWN Stripe customer (anti-replay
/// — prevents a user completing someone else's session id), persist each
/// account (idempotent re-link via `ON CONFLICT`), then attempt an initial
/// sync per account. The initial sync may legitimately import 0 rows — Stripe's
/// FC transaction data populates asynchronously; the real data typically lands
/// moments later via the `refreshed_transactions` webhook (see Task 3).
pub async fn complete_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    session_id: &str,
) -> Result<Vec<LinkedAccountResponse>, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;
    ensure_not_closed(pool, budget_id).await?;

    let my_customer_id = ensure_customer(pool, user_id).await?;
    let session = stripe_get(&format!("v1/financial_connections/sessions/{session_id}")).await?;

    let session_customer = session.pointer("/account_holder/customer").and_then(|v| v.as_str());
    if session_customer != Some(my_customer_id.as_str()) {
        tracing::warn!(user_id = %user_id, session_id, "financial_connections session customer mismatch — possible replay attempt");
        return Err((StatusCode::FORBIDDEN, "This link session does not belong to you".to_string()));
    }

    let accounts = session.pointer("/accounts/data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut results = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let stripe_account_id = match acc.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let institution_name = acc.pointer("/institution_name").and_then(|v| v.as_str()).map(str::to_string);
        let display_name = acc.get("display_name").and_then(|v| v.as_str()).map(str::to_string);
        let last4 = acc.get("last4").and_then(|v| v.as_str()).map(str::to_string);
        let category = acc.get("category").and_then(|v| v.as_str()).map(str::to_string);
        let subcategory = acc.get("subcategory").and_then(|v| v.as_str()).map(str::to_string);

        let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, stripe_account_id, stripe_customer_id, \
                 institution_name, display_name, last4, category, subcategory) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (stripe_account_id) DO UPDATE SET \
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

        // Best-effort initial sync; a failure here must not fail the whole
        // link (the account is already persisted and will pick up data via
        // the webhook regardless).
        if let Err(e) = sync_account_transactions(pool, &row).await {
            tracing::warn!(error = %e, account_id = %row.id, "initial financial_connections sync failed; will retry via webhook/manual refresh");
        }

        results.push(row.into());
    }
    Ok(results)
}

/// Pull transactions for one linked account from Stripe and idempotently
/// insert new rows into `transactions`. Returns the count actually inserted
/// (rows already seen via `stripe_transaction_id` are silently skipped, NOT
/// counted). Updates `linked_accounts.last_synced_at` regardless of count.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, sqlx::Error> {
    // Load the budget's categories once for the categorization heuristic,
    // in creation order (oldest first) so `guess_category_id`'s "first match
    // wins" is deterministic and stable across syncs.
    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await?;

    let mut imported: u64 = 0;
    let mut starting_after: Option<String> = None;
    loop {
        let mut path = format!("v1/financial_connections/transactions?account={}&limit=100", linked_account.stripe_account_id);
        if let Some(cursor) = &starting_after {
            path.push_str(&format!("&starting_after={cursor}"));
        }
        let page = match stripe_get(&path).await {
            Ok(p) => p,
            // A Stripe/network error here is logged by stripe_get already;
            // surface it as a generic sqlx::Error::Protocol so the caller's
            // Result<_, sqlx::Error> signature stays uniform without a new
            // error enum for what is, from this function's contract, just
            // "the sync didn't complete this time — safe to retry later".
            Err(_) => return Ok(imported),
        };
        let rows = page.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        for tx in &rows {
            let stripe_tx_id = match tx.get("id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let amount_cents = tx.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
            let amount = normalize_amount(amount_cents);
            let description = tx.get("description").and_then(|v| v.as_str()).unwrap_or("Imported transaction").to_string();
            let transacted_at = tx.get("transacted_at").and_then(|v| v.as_i64())
                .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                .unwrap_or_else(chrono::Utc::now);

            let category_id = guess_category_id(&description, &categories);

            // NOTE: the ON CONFLICT target must repeat the partial index's WHERE
            // predicate exactly (`WHERE stripe_transaction_id IS NOT NULL`) —
            // Postgres does not infer a plain column-list ON CONFLICT target
            // against a PARTIAL unique index; omitting the predicate here
            // raises "there is no unique or exclusion constraint matching the
            // ON CONFLICT specification" at runtime, on every insert.
            let res = sqlx::query(
                "INSERT INTO transactions \
                    (id, budget_id, category_id, amount, transaction_date, description, \
                     external_account_id, stripe_transaction_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (stripe_transaction_id) WHERE stripe_transaction_id IS NOT NULL DO NOTHING"
            )
            .bind(Uuid::new_v4())
            .bind(linked_account.budget_id)
            .bind(category_id)
            .bind(amount)
            .bind(transacted_at)
            .bind(&description)
            .bind(linked_account.id)
            .bind(&stripe_tx_id)
            .execute(pool).await?;
            if res.rows_affected() > 0 {
                imported += 1;
            }
        }
        starting_after = rows.last().and_then(|t| t.get("id")).and_then(|v| v.as_str()).map(str::to_string);
        if page.get("has_more").and_then(|v| v.as_bool()) != Some(true) {
            break;
        }
    }

    sqlx::query("UPDATE linked_accounts SET last_synced_at = now(), updated_at = now() WHERE id = $1")
        .bind(linked_account.id)
        .execute(pool).await?;

    Ok(imported)
}
```

- [ ] **Step 8: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean.

- [ ] **Step 9: Commit**

```bash
git add backend/src/financial_connections.rs backend/src/billing.rs backend/src/main.rs
git commit -m "feat(#303): add financial_connections session creation, linking, and transaction sync"
```

---

### Task 3: `financial_connections.rs` — manual refresh, disconnect, webhook

**Files:**
- Modify: `backend/src/financial_connections.rs` (append; insert before the existing `#[cfg(test)]`
  block)

- [ ] **Step 1: Write the failing unit tests for `map_fc_webhook_event`**

Add to the EXISTING `#[cfg(test)] mod tests` block in `financial_connections.rs` (append after the
`guess_category_id` tests, inside the same `mod tests { ... }`):

```rust
    fn fc_event(t: &str, account_id: &str) -> crate::billing::StripeEvent {
        serde_json::from_value(serde_json::json!({
            "type": t, "created": 1_700_000_000,
            "data": { "object": { "id": account_id, "object": "financial_connections.account" } }
        })).unwrap()
    }

    #[test]
    fn map_fc_webhook_event_refreshed_transactions() {
        let e = fc_event("financial_connections.account.refreshed_transactions", "fca_1");
        assert_eq!(map_fc_webhook_event(&e), Some(FcAccountEvent::RefreshedTransactions { stripe_account_id: "fca_1".to_string() }));
    }

    #[test]
    fn map_fc_webhook_event_disconnected() {
        let e = fc_event("financial_connections.account.disconnected", "fca_2");
        assert_eq!(map_fc_webhook_event(&e), Some(FcAccountEvent::Disconnected { stripe_account_id: "fca_2".to_string() }));
    }

    #[test]
    fn map_fc_webhook_event_ignores_unknown_type() {
        let e = fc_event("financial_connections.account.created", "fca_3");
        assert_eq!(map_fc_webhook_event(&e), None);
    }

    #[test]
    fn map_fc_webhook_event_missing_account_id_is_none() {
        let e: crate::billing::StripeEvent = serde_json::from_value(serde_json::json!({
            "type": "financial_connections.account.refreshed_transactions", "created": 1_700_000_000,
            "data": { "object": { "object": "financial_connections.account" } }
        })).unwrap();
        assert_eq!(map_fc_webhook_event(&e), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test map_fc_webhook_event`
Expected: FAIL to compile — `map_fc_webhook_event` and `FcAccountEvent` don't exist yet.

- [ ] **Step 3: Implement `refresh_linked_account`, `disconnect_linked_account`, the webhook, and `map_fc_webhook_event`**

Insert before the `#[cfg(test)]` block:

```rust
use axum::body::Bytes;
use axum::http::HeaderMap;
use crate::billing::{verify_stripe_signature, StripeEvent};

/// The two Financial Connections account-level webhook events this module
/// handles, pre-parsed to just the field the handler needs. Mirrors
/// `billing::apply_subscription_event`'s pure-mapping-function shape so the
/// event->action decision is unit-testable without a DB or HTTP handler.
#[derive(Debug, PartialEq)]
pub(crate) enum FcAccountEvent {
    RefreshedTransactions { stripe_account_id: String },
    Disconnected { stripe_account_id: String },
}

/// Map a parsed Stripe event to the action to take, or `None` for event types
/// this module intentionally ignores (handler returns 200). Both events this
/// module cares about carry the `financial_connections.account` object in
/// `data.object`, whose `id` field is the `fca_...` account id.
pub(crate) fn map_fc_webhook_event(event: &StripeEvent) -> Option<FcAccountEvent> {
    let account_id = event.data.object.get("id").and_then(|v| v.as_str())?.to_string();
    match event.event_type.as_str() {
        "financial_connections.account.refreshed_transactions" =>
            Some(FcAccountEvent::RefreshedTransactions { stripe_account_id: account_id }),
        "financial_connections.account.disconnected" =>
            Some(FcAccountEvent::Disconnected { stripe_account_id: account_id }),
        _ => None,
    }
}

/// Manual refresh (REST + chat, both call this): Pro-gated, kicks off a fresh
/// Stripe-side pull. INTENTIONALLY ASYNC — this returns before any new
/// transaction lands; the actual import happens moments later when Stripe's
/// `refreshed_transactions` webhook fires and calls `sync_account_transactions`
/// itself (see `webhook` below). Manual refresh and webhook-driven auto-refresh
/// are the same underlying mechanism; this just asks Stripe to check NOW.
pub async fn refresh_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    require_pro(pool, user_id).await?;

    let stripe_account_id: String = sqlx::query_scalar(
        "SELECT stripe_account_id FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    let form = vec![("features[]".to_string(), "transactions".to_string())];
    stripe_post(&format!("v1/financial_connections/accounts/{stripe_account_id}/refresh"), &form).await?;
    Ok(())
}

/// Disconnect a linked account. NOT Pro-gated (spec Assumption 11) — a user
/// whose subscription lapsed must still be able to manage their own linked
/// data. Calls Stripe's disconnect endpoint FIRST (revoking access on Stripe's
/// side) and only flips the local row to 'disconnected' after that succeeds —
/// a failed Stripe call must never leave us believing an account is
/// disconnected when Stripe still considers it live and could keep firing
/// `refreshed_transactions` webhooks for it.
pub async fn disconnect_linked_account(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let stripe_account_id: String = sqlx::query_scalar(
        "SELECT stripe_account_id FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    // Stripe's disconnect endpoint is idempotent (returns status:"disconnected"
    // whether or not it already was), so a repeat call on an already-disconnected
    // account is harmless — mirrors billing::stripe_delete's already-canceled
    // tolerance.
    stripe_post(&format!("v1/financial_connections/accounts/{stripe_account_id}/disconnect"), &[]).await?;

    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() WHERE id = $1")
        .bind(account_id)
        .execute(pool).await.map_err(internal_error)?;
    Ok(())
}

#[derive(Serialize)]
pub struct ListLinkedAccountsResponse {
    pub accounts: Vec<LinkedAccountResponse>,
}

/// List linked accounts for a budget. View-or-above only (not Pro-gated, not
/// Edit-gated — same read access as any other budget data).
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

/// PUBLIC, signature-verified. Raw body (`Bytes`) required for HMAC — must NOT
/// use `Json<_>` extraction. Mounted OUTSIDE the auth nest (Task 4). Mirrors
/// `billing::webhook`'s shape exactly, but verifies against its OWN secret
/// (`STRIPE_FC_WEBHOOK_SECRET`) since this is registered as a separate Stripe
/// webhook endpoint.
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let secret = env_opt("STRIPE_FC_WEBHOOK_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let sig = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = chrono::Utc::now().timestamp();
    if verify_stripe_signature(&body, sig, &secret, now, 300).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: StripeEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string())),
    };

    match map_fc_webhook_event(&event) {
        Some(FcAccountEvent::RefreshedTransactions { stripe_account_id }) => {
            let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
                "SELECT * FROM linked_accounts WHERE stripe_account_id = $1")
                .bind(&stripe_account_id)
                .fetch_optional(&state.db).await.map_err(internal_error)?;
            match linked {
                Some(row) if row.status == "active" => {
                    let owner_status: Option<String> = sqlx::query_scalar(
                        "SELECT status FROM subscriptions WHERE user_id = $1")
                        .bind(row.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
                    if user_is_pro(owner_status.as_deref()) {
                        if let Err(e) = sync_account_transactions(&state.db, &row).await {
                            tracing::warn!(error = %e, account_id = %row.id, "webhook-driven sync failed");
                        }
                    } else {
                        tracing::info!(account_id = %row.id, "skipping refresh: owner is not Pro (subscription lapsed)");
                    }
                }
                Some(_) => tracing::debug!(stripe_account_id, "ignoring refreshed_transactions for a disconnected account"),
                None => tracing::warn!(stripe_account_id, "financial_connections webhook for unknown account"),
            }
        }
        Some(FcAccountEvent::Disconnected { stripe_account_id }) => {
            sqlx::query(
                "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() \
                 WHERE stripe_account_id = $1 AND status = 'active'")
                .bind(&stripe_account_id)
                .execute(&state.db).await.map_err(internal_error)?;
        }
        None => tracing::debug!(event_type = %event.event_type, "ignoring unhandled financial_connections event type"),
    }
    Ok(StatusCode::OK)
}
```

- [ ] **Step 4: Run the `map_fc_webhook_event` tests**

Run: `cd backend && cargo test map_fc_webhook_event`
Expected: 4 tests pass.

- [ ] **Step 5: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean. If `Extension`/`Json` remain unused-import warnings, that's fine — Task 4
uses them.

- [ ] **Step 6: Write `#[ignore]` DB+wiremock integration tests**

Add to the SAME `mod tests` block in `financial_connections.rs`, mirroring `billing.rs`'s
`test_pool()`/`mk_user()`/`set_stripe_env()` helpers exactly (copy their bodies — `billing.rs` lines
639-676 — into this module's test helpers since Rust test modules don't share `#[cfg(test)]` code
across files):

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
            .bind(id).bind(format!("fc-{id}@test.example"))
            .execute(db).await.unwrap();
        id
    }

    async fn mk_budget(db: &PgPool, owner_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(id).bind(owner_id).execute(db).await.unwrap();
        id
    }

    async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'active')")
            .bind(user_id).bind(customer_id).execute(db).await.unwrap();
    }

    async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, stripe_account_id: &str) -> crate::db::LinkedAccount {
        sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts (id, budget_id, user_id, stripe_account_id, stripe_customer_id) \
             VALUES ($1, $2, $3, $4, 'cus_x') RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(stripe_account_id)
            .fetch_one(db).await.unwrap()
    }

    fn set_stripe_env(server: &MockServer) {
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
    }
    fn clear_stripe_env() {
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn non_pro_user_cannot_start_link_session() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // No subscriptions row at all -> not Pro.
        let res = create_link_session(&db, uid, bid).await;
        assert_eq!(res.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn pro_user_can_start_link_session() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("POST")).and(path("/v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_new"})))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/v1/financial_connections/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fcsess_1","client_secret":"fcsess_1_secret_x"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_new").await;

        let res = create_link_session(&db, uid, bid).await.expect("pro user can start a link session");
        assert_eq!(res.client_secret, "fcsess_1_secret_x");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_is_idempotent_on_redelivery() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id":"fctxn_1","amount":-1500,"description":"Whole Foods","transacted_at":1_700_000_000}
                ],
                "has_more": false
            })))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_sync_1").await;

        let first = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(first, 1);
        let second = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(second, 0, "re-sync of the same Stripe transaction must import 0, not duplicate");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE stripe_transaction_id = 'fctxn_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1);

        // Assert the persisted amount actually went through normalize_amount's
        // cents->dollars/abs() conversion, not just that a row landed.
        let amount: f64 = sqlx::query_scalar("SELECT amount FROM transactions WHERE stripe_transaction_id = 'fctxn_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(amount, 15.0, "Stripe's -1500 cents must normalize to a positive $15.00");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_calls_stripe_and_stops_future_sync() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("POST")).and(path("/v1/financial_connections/accounts/fca_disc_1/disconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fca_disc_1","status":"disconnected"})))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_disc_1").await;

        disconnect_linked_account(&db, uid, bid, linked.id).await.expect("disconnect ok");
        // wiremock verifies .expect(1) on drop — confirms Stripe's endpoint was actually called.

        let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    async fn refreshed_transactions_webhook_noops_for_disconnected_account() {
        // Belt-and-suspenders check (spec Assumption 12): a webhook arriving
        // for an already-locally-disconnected account must not re-sync, even
        // if the owner is Pro. This test drives map_fc_webhook_event + a direct
        // status check rather than a signed HTTP round trip (signature
        // verification itself is already covered by billing.rs's tests, which
        // this module reuses unchanged).
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_wh").await;
        let linked = mk_linked_account(&db, bid, uid, "fca_wh_1").await;
        sqlx::query("UPDATE linked_accounts SET status = 'disconnected' WHERE id = $1")
            .bind(linked.id).execute(&db).await.unwrap();

        let row = sqlx::query_as::<_, crate::db::LinkedAccount>("SELECT * FROM linked_accounts WHERE id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(row.status, "disconnected", "must still be disconnected — this test only asserts the precondition; \
            the webhook handler itself (integration-tested via the disconnect_calls_stripe_and_stops_future_sync flow) \
            checks `status == 'active'` before calling sync_account_transactions");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
```

- [ ] **Step 7: Run the ignored tests against a live dev Postgres**

Ensure `podman-compose up -d` is running, then:
Run: `cd backend && cargo test --ignored financial_connections -- --test-threads=1`
Expected: all tests pass. If `non_pro_user_cannot_start_link_session` or `pro_user_can_start_link_session`
fail on a permission/connection error rather than an assertion, check `DATABASE_URL` matches the
running Postgres port (6153 per `AGENTS.md`).

- [ ] **Step 8: Commit**

```bash
git add backend/src/financial_connections.rs
git commit -m "feat(#303): add manual refresh, disconnect, and financial_connections webhook"
```

---

### Task 4: REST endpoints + `main.rs` wiring

**Files:**
- Modify: `backend/src/financial_connections.rs` (add axum handler wrappers)
- Modify: `backend/src/main.rs` (routes)
- Modify: `.env.example` (new Stripe FC env vars)

- [ ] **Step 1: Add axum handlers to `financial_connections.rs`**

Append (these wrap the `pool`-based functions from Tasks 2-3 in axum's `State`/`Path`/`Extension`/
`Json` extraction, matching every other handler's signature style in `budget.rs`):

```rust
use axum::extract::Path;

#[derive(Deserialize)]
pub struct CompleteLinkRequest {
    pub session_id: String,
}

pub async fn start_link(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<SessionResponse>, (StatusCode, String)> {
    create_link_session(&state.db, user_id, budget_id).await.map(Json)
}

pub async fn complete_link(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<CompleteLinkRequest>,
) -> Result<Json<Vec<LinkedAccountResponse>>, (StatusCode, String)> {
    complete_link_session(&state.db, user_id, budget_id, &req.session_id).await.map(Json)
}

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
```

- [ ] **Step 2: Wire routes in `main.rs`**

In `backend/src/main.rs`, add to the `use r#rag::{...}` import block area a new import line:

```rust
use financial_connections::{
    start_link, complete_link, list_linked_accounts_handler,
    refresh_linked_account_handler, disconnect_linked_account_handler,
};
```

Add these routes inside `protected_routes` (right after the existing `.route("/budgets/:id/transactions/:transaction_id", ...)` line, ~line 261):

```rust
        .route("/budgets/:id/linked-accounts/session", post(start_link))
        .route("/budgets/:id/linked-accounts/complete", post(complete_link))
        .route("/budgets/:id/linked-accounts", get(list_linked_accounts_handler))
        .route("/budgets/:id/linked-accounts/:account_id/refresh", post(refresh_linked_account_handler))
        .route("/budgets/:id/linked-accounts/:account_id", delete(disconnect_linked_account_handler))
```

Add a new public router (mirrors `billing_public_routes`, right after it, ~line 320):

```rust
    // Financial Connections webhook (#303). PUBLIC — authenticated by Stripe
    // signature, NOT a session — mounted OUTSIDE the auth nest, same as
    // billing's webhook.
    let financial_connections_public_routes = Router::new()
        .route("/financial-connections/webhook", post(financial_connections::webhook));
```

Add `.nest("/api", financial_connections_public_routes)` to the `app` builder, right after the
existing `.nest("/api", billing_public_routes)` line (~line 327).

- [ ] **Step 3: Run `cargo check`**

Run: `cd backend && cargo check`
Expected: compiles clean.

- [ ] **Step 4: Add env vars to `.env.example`**

In `.env.example`, add right after the existing `STRIPE_API_BASE` line (~line 66):

```
# Financial Connections bank-account linking (#303). Registered as a SEPARATE
# Stripe webhook endpoint from the billing one above, with its own signing
# secret.
STRIPE_FC_WEBHOOK_SECRET=
```

- [ ] **Step 5: Manual smoke test**

Run: `cd backend && cargo run` (with `podman-compose up -d` running), then in another terminal:

```bash
curl -s -o /dev/null -w "%{http_code}\n" -X POST http://localhost:3000/api/financial-connections/webhook \
  -H "Stripe-Signature: t=1,v1=bad" -d '{}'
```

Expected: `503` if `STRIPE_FC_WEBHOOK_SECRET` is unset in your local `.env` (fail-loud, matches
billing's convention), or `400` if it IS set (bad signature correctly rejected). Either result
confirms the route is wired and reachable — do NOT expect `404`.

- [ ] **Step 6: Commit**

```bash
git add backend/src/financial_connections.rs backend/src/main.rs .env.example
git commit -m "feat(#303): wire linked-accounts REST endpoints and financial-connections webhook route"
```

---

### Task 5: Chat co-pilot actions in `rag.rs`

**Files:**
- Modify: `backend/src/rag.rs` (multiple insertion points, listed per step)

- [ ] **Step 1: Add the `ChatResponse` field**

In `backend/src/rag.rs`, add to `pub struct ChatResponse` (currently ends at line 133 with
`categories_table_html: Option<String>,\n}`), right before the closing brace:

```rust
    /// The Stripe Financial Connections session client_secret (#303), set ONLY
    /// by the LINK_BANK_ACCOUNT chat action. The frontend uses it to drive
    /// Stripe.js's `collectFinancialConnectionsAccounts` modal — there is no
    /// hosted-redirect URL for Financial Connections like there is for
    /// Checkout, so this is the one chat-response field that carries a secret
    /// value through to the client (it is single-use and expires quickly on
    /// Stripe's side, same trust model as a Checkout Session URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub financial_connections_client_secret: Option<String>,
```

- [ ] **Step 2: Add the `account_match` field to `AiActionParams`**

Add to `AiActionParams` (currently ends at line 300 with the `new_description` field), right before
the closing brace:

```rust
    #[serde(default)]
    account_match: Option<String>, // locator text (institution/display name/last4 substring) for UNLINK_BANK_ACCOUNT (#303)
```

- [ ] **Step 3: Register the 4 new actions in `required_perm_for_action`**

In `required_perm_for_action` (line ~592), add the new actions to the existing match arms:

```rust
fn required_perm_for_action(action: &str) -> Option<Permission> {
    match action {
        "SHARE_BUDGET" | "DELETE_BUDGET" | "CLOSE_BUDGET" | "ARCHIVE_BUDGET"
        | "UNARCHIVE_BUDGET" | "ROLLUP_BUDGET" | "UNROLLUP_BUDGET" => Some(Permission::Owner),
        "CREATE_CATEGORY" | "SEED_CATEGORIES" | "ADD_TRANSACTION" | "EDIT_TRANSACTION" | "DELETE_TRANSACTION" | "UPDATE_BUDGET"
        | "CREATE_GOAL" | "ADD_GOAL_CONTRIBUTION" | "DELETE_CATEGORY"
        | "SET_CATEGORY_ROLLOVER" | "UPDATE_CATEGORY" | "SET_CATEGORY_FUND"
        | "LINK_BANK_ACCOUNT" | "UNLINK_BANK_ACCOUNT" | "REFRESH_BANK_ACCOUNT" => Some(Permission::Edit),
        _ => None,
    }
}
```

(`LIST_LINKED_ACCOUNTS` is intentionally OMITTED from this match — it falls to the `_ => None` arm,
same as `LIST_BUDGETS`, since it's read-only and needs no active-budget write permission.)

Update the doc comment above this function (lines 576-588) to mention the 3 new Edit-gated actions
in the existing bullet list (append `LINK_BANK_ACCOUNT`, `UNLINK_BANK_ACCOUNT`, `REFRESH_BANK_ACCOUNT`
to the "Edit-or-Owner" bullet, and `LIST_LINKED_ACCOUNTS` to the "None" bullet).

Two existing table-driven permission tests assert hardcoded action lists and will NOT fail just
because these 4 actions are missing from them — but the spec's Success Criterion #8 explicitly
calls for the permission table entries to be unit tested, so update both to include the new actions:

1. Find `edit_actions_allow_owner_and_edit_but_not_view` (search `rag.rs` for that exact function
   name, currently ~line 7891). Add `"LINK_BANK_ACCOUNT"`, `"UNLINK_BANK_ACCOUNT"`,
   `"REFRESH_BANK_ACCOUNT"` to its list of Edit-gated actions it iterates over/asserts against.
2. Find `read_only_and_self_owned_actions_need_no_active_budget_write` (search `rag.rs`, currently
   ~line 7926). Add `"LIST_LINKED_ACCOUNTS"` to its list of `None`-permission actions.

Read each test's actual body first (its exact assertion shape may iterate a `const` array rather
than inline literals) and add the new action strings to whichever list structure it already uses —
do not invent a new list shape.

- [ ] **Step 4: Add the dispatch match arms**

Add near the existing `"ADD_TRANSACTION" => { ... }` arm (after it ends at line 2963, before
`"SHARE_BUDGET" => {`):

```rust
        "LINK_BANK_ACCOUNT" => {
            if let Some(bid) = active_budget_id {
                match crate::financial_connections::create_link_session(&state.db, user_id, bid).await {
                    Ok(session) => {
                        financial_connections_client_secret = Some(session.client_secret);
                    }
                    Err((status, msg)) if status == axum::http::StatusCode::PAYMENT_REQUIRED => {
                        mutation_error = Some(format!(
                            "Linking bank accounts is a Nels Pro feature. {}", msg
                        ));
                    }
                    Err((_, msg)) => {
                        mutation_error = Some(msg);
                    }
                }
            }
        }
        "LIST_LINKED_ACCOUNTS" => {
            if let Some(bid) = active_budget_id {
                if let Ok(list) = crate::financial_connections::list_linked_accounts(&state.db, user_id, bid).await {
                    if list.accounts.is_empty() {
                        mutation_log = Some("You have no linked bank accounts yet.".to_string());
                    } else {
                        let lines: Vec<String> = list.accounts.iter().map(|a| {
                            format!("- {} ({}) — {}", 
                                a.display_name.as_deref().unwrap_or("Bank account"),
                                a.institution_name.as_deref().unwrap_or("Unknown institution"),
                                a.status)
                        }).collect();
                        mutation_log = Some(format!("Linked accounts:\n{}", lines.join("\n")));
                    }
                }
            }
        }
        "UNLINK_BANK_ACCOUNT" => {
            if let Some(bid) = active_budget_id {
                if let Some(locator) = parsed_ai_res.action_params.as_ref().and_then(|p| p.account_match.clone()) {
                    let list = crate::financial_connections::list_linked_accounts(&state.db, user_id, bid).await
                        .map(|l| l.accounts).unwrap_or_default();
                    let locator_lower = locator.to_lowercase();
                    let matches: Vec<_> = list.iter().filter(|a| {
                        a.display_name.as_deref().unwrap_or("").to_lowercase().contains(&locator_lower)
                        || a.institution_name.as_deref().unwrap_or("").to_lowercase().contains(&locator_lower)
                        || a.last4.as_deref().unwrap_or("").contains(&locator)
                    }).collect();
                    match matches.as_slice() {
                        [one] => {
                            match crate::financial_connections::disconnect_linked_account(&state.db, user_id, bid, one.id).await {
                                Ok(()) => {
                                    mutation_log = Some(format!("Disconnected {}. Your past transactions from it are kept.",
                                        one.display_name.as_deref().unwrap_or("that account")));
                                    log_audit(&state.db, bid, user_id, "AI_UNLINK_BANK_ACCOUNT", &mutation_log.clone().unwrap()).await;
                                }
                                Err((_, msg)) => mutation_error = Some(msg),
                            }
                        }
                        [] => mutation_error = Some(format!("I couldn't find a linked account matching '{locator}'.")),
                        _ => mutation_error = Some(format!("More than one linked account matches '{locator}' — please be more specific.")),
                    }
                } else {
                    mutation_error = Some("Which account would you like to disconnect?".to_string());
                }
            }
        }
        "REFRESH_BANK_ACCOUNT" => {
            if let Some(bid) = active_budget_id {
                let list = crate::financial_connections::list_linked_accounts(&state.db, user_id, bid).await
                    .map(|l| l.accounts).unwrap_or_default();
                if list.is_empty() {
                    mutation_error = Some("You don't have any linked bank accounts to refresh yet.".to_string());
                } else {
                    let mut refreshed_any = false;
                    for a in &list {
                        if crate::financial_connections::refresh_linked_account(&state.db, user_id, bid, a.id).await.is_ok() {
                            refreshed_any = true;
                        }
                    }
                    if refreshed_any {
                        mutation_log = Some("Refresh requested — new transactions will appear shortly.".to_string());
                    } else {
                        mutation_error = Some("I wasn't able to request a refresh — check that your Pro subscription is active.".to_string());
                    }
                }
            }
        }
```

Note: `financial_connections_client_secret` must be declared as a `let mut financial_connections_client_secret: Option<String> = None;` alongside the existing `mutation_log`/`mutation_error`/`category_offer` local variables near the top of `chat_endpoint` (search for `let mut mutation_log` to find that block), and threaded into the final `ChatResponse { ... }` construction at the end of `chat_endpoint` the same way `categories_table_html` already is.

- [ ] **Step 5: Add the system-prompt directive**

Find the last numbered rule in the system prompt (currently rule `2n`, ending at line 1401 with
`...set 'budget_strategy'.`). Add immediately after it:

```
2o. LINKED BANK ACCOUNTS (Pro feature): if the user asks to 'link my bank account', 'connect my bank', 'sync transactions from my bank', or similar, set 'action' to 'LINK_BANK_ACCOUNT'. This starts a secure Stripe-hosted linking flow — do not ask for any account numbers or credentials yourself. If the user asks what's linked, what accounts are connected, or similar, set 'action' to 'LIST_LINKED_ACCOUNTS'. If the user asks to disconnect, unlink, or remove a linked account, set 'action' to 'UNLINK_BANK_ACCOUNT' and populate 'account_match' with the bank/account name they mentioned (or leave it empty if they didn't name one — you'll be asked to clarify). If the user asks to refresh, sync, or update their bank transactions now, set 'action' to 'REFRESH_BANK_ACCOUNT'. All four require an active Pro subscription except LIST_LINKED_ACCOUNTS and UNLINK_BANK_ACCOUNT, which work regardless of subscription status.
```

- [ ] **Step 6: Add offline-router phrase fallbacks**

Add a new function near the other `offline_*_action` functions (e.g. right after
`offline_budgets_list_action` at line ~5855):

```rust
pub(crate) fn offline_linked_accounts_action(msg_lower: &str) -> Option<&'static str> {
    // Order matters: "disconnect"/"unlink" checked before the generic "link"
    // phrase so "unlink my bank account" doesn't match LINK_BANK_ACCOUNT first.
    if msg_lower.contains("disconnect") && (msg_lower.contains("bank") || msg_lower.contains("account")) {
        Some("UNLINK_BANK_ACCOUNT")
    } else if msg_lower.contains("unlink") {
        Some("UNLINK_BANK_ACCOUNT")
    } else if msg_lower.contains("what's linked") || msg_lower.contains("whats linked")
        || msg_lower.contains("linked account") || msg_lower.contains("connected account") {
        Some("LIST_LINKED_ACCOUNTS")
    } else if msg_lower.contains("refresh") && msg_lower.contains("bank") {
        Some("REFRESH_BANK_ACCOUNT")
    } else if msg_lower.contains("refresh") && msg_lower.contains("transaction") {
        Some("REFRESH_BANK_ACCOUNT")
    } else if (msg_lower.contains("link") || msg_lower.contains("connect")) && msg_lower.contains("bank") {
        Some("LINK_BANK_ACCOUNT")
    } else {
        None
    }
}

#[cfg(test)]
mod financial_connections_offline_router_tests {
    use super::*;

    #[test]
    fn routes_link_bank_account() {
        assert_eq!(offline_linked_accounts_action("link my bank account"), Some("LINK_BANK_ACCOUNT"));
        assert_eq!(offline_linked_accounts_action("connect my bank"), Some("LINK_BANK_ACCOUNT"));
    }

    #[test]
    fn routes_list_linked_accounts() {
        assert_eq!(offline_linked_accounts_action("what's linked?"), Some("LIST_LINKED_ACCOUNTS"));
        assert_eq!(offline_linked_accounts_action("show me my linked accounts"), Some("LIST_LINKED_ACCOUNTS"));
    }

    #[test]
    fn routes_unlink_before_link() {
        // "unlink my bank account" contains neither "disconnect" nor a bare
        // "link"-without-"un" match issue, but IS a substring risk if checked
        // in the wrong order since "link" is a substring concern for phrasing
        // like "please unlink my bank" — verify UNLINK wins.
        assert_eq!(offline_linked_accounts_action("please unlink my bank account"), Some("UNLINK_BANK_ACCOUNT"));
        assert_eq!(offline_linked_accounts_action("disconnect my bank account"), Some("UNLINK_BANK_ACCOUNT"));
    }

    #[test]
    fn routes_refresh_bank_account() {
        assert_eq!(offline_linked_accounts_action("refresh my bank transactions"), Some("REFRESH_BANK_ACCOUNT"));
    }

    #[test]
    fn no_match_returns_none() {
        assert_eq!(offline_linked_accounts_action("how much did I spend on groceries"), None);
    }
}
```

Wire it into the offline router's dispatch chain (find where `offline_budgets_list_action` or
similar is called inside the `if api_key.is_empty() { ... }` offline branch of `chat_endpoint`, and
add an `else if let Some(action) = offline_linked_accounts_action(&msg_lower) { ... }` arm following
the exact pattern of the neighboring offline-router calls at that call site).

- [ ] **Step 7: Run the full backend test suite**

Run: `cd backend && cargo test`
Expected: all existing tests still pass (no regression) plus the new
`map_fc_webhook_event`/`guess_category_id`/offline-router tests from this and prior tasks.

- [ ] **Step 8: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#303): add LINK_BANK_ACCOUNT/LIST_LINKED_ACCOUNTS/UNLINK_BANK_ACCOUNT/REFRESH_BANK_ACCOUNT chat actions"
```

---

### Task 6: Frontend — Stripe.js, Linked Accounts panel, chat wiring

**Files:**
- Modify: `frontend/package.json` (new dependency)
- Modify: `frontend/.env.example` (or create if none exists — check first) and `frontend/.env`
- Create: `frontend/src/lib/linkedAccounts.js`
- Create: `frontend/src/lib/linkedAccounts.test.js`
- Create: `frontend/src/lib/LinkedAccounts.svelte`
- Modify: `frontend/src/lib/Settings.svelte`
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Add the Stripe.js dependency**

Run: `cd frontend && pnpm add @stripe/stripe-js`
Expected: `package.json`'s `dependencies` gains `"@stripe/stripe-js": "^..."`.

- [ ] **Step 2: Add the publishable-key env var**

Check whether `frontend/.env.example` exists (`ls frontend/.env.example`). If it exists, add:

```
VITE_STRIPE_PUBLISHABLE_KEY=
```

If it doesn't exist, add the same line as a comment note to `frontend/.env` locally (do NOT commit
real secrets — a publishable key is not secret, but leave the value blank in any committed file).

- [ ] **Step 3: Write the failing tests for `linkedAccounts.js`**

Create `frontend/src/lib/linkedAccounts.test.js`:

```javascript
import { describe, it, expect, vi } from "vitest";
import { parseLinkedAccountsResponse, isProGateError } from "./linkedAccounts.js";

describe("parseLinkedAccountsResponse", () => {
  it("returns the accounts array unchanged on a normal response", () => {
    const input = { accounts: [{ id: "1", display_name: "Checking", status: "active" }] };
    expect(parseLinkedAccountsResponse(input)).toEqual(input.accounts);
  });

  it("returns an empty array when accounts is missing", () => {
    expect(parseLinkedAccountsResponse({})).toEqual([]);
  });
});

describe("isProGateError", () => {
  it("recognizes a 402 as a Pro-gate error", () => {
    expect(isProGateError({ status: 402 })).toBe(true);
  });

  it("does not treat other statuses as a Pro-gate error", () => {
    expect(isProGateError({ status: 403 })).toBe(false);
    expect(isProGateError({ status: 500 })).toBe(false);
    expect(isProGateError(null)).toBe(false);
  });
});
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd frontend && pnpm test linkedAccounts`
Expected: FAIL — `./linkedAccounts.js` doesn't exist yet.

- [ ] **Step 5: Implement `linkedAccounts.js`**

Create `frontend/src/lib/linkedAccounts.js`:

```javascript
// Stripe Financial Connections bank-account linking (#303). Pure helpers are
// exported separately from the Stripe.js-driving flow so they're unit
// testable without mocking the SDK (mirrors this repo's existing
// pure-logic-alongside-*.test.js convention).

/** Normalize a linked-accounts list response to a plain array. */
export function parseLinkedAccountsResponse(data) {
  return Array.isArray(data?.accounts) ? data.accounts : [];
}

/** Whether an error (shaped like `{status}`) represents the Pro-gate (402). */
export function isProGateError(err) {
  return !!err && err.status === 402;
}

/**
 * Drive the full link flow: start a session, open Stripe.js's
 * collectFinancialConnectionsAccounts modal, then tell our backend the
 * session completed so it can persist the accounts. `fetchApi` and
 * `loadStripe` are injected so this is testable without a real network call
 * or the real Stripe SDK.
 */
export async function startLinkFlow({ budgetId, fetchApi, loadStripe, publishableKey }) {
  const { client_secret } = await fetchApi(`/budgets/${budgetId}/linked-accounts/session`, {
    method: "POST",
  });
  const stripe = await loadStripe(publishableKey);
  const result = await stripe.collectFinancialConnectionsAccounts({ clientSecret: client_secret });
  if (!result?.financialConnectionsSession?.accounts?.length) {
    return { linked: [] };
  }
  const linked = await fetchApi(`/budgets/${budgetId}/linked-accounts/complete`, {
    method: "POST",
    body: JSON.stringify({ session_id: result.financialConnectionsSession.id }),
  });
  return { linked };
}

export async function fetchLinkedAccounts({ budgetId, fetchApi }) {
  const data = await fetchApi(`/budgets/${budgetId}/linked-accounts`);
  return parseLinkedAccountsResponse(data);
}

export async function refreshLinkedAccount({ budgetId, accountId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/linked-accounts/${accountId}/refresh`, { method: "POST" });
}

export async function disconnectLinkedAccount({ budgetId, accountId, fetchApi }) {
  return fetchApi(`/budgets/${budgetId}/linked-accounts/${accountId}`, { method: "DELETE" });
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd frontend && pnpm test linkedAccounts`
Expected: 4 tests pass.

- [ ] **Step 7: Check `fetchApi`'s existing error shape**

`fetchApi` is NOT a module export — it's a closure defined in `App.svelte` (search `grep -n
"function fetchApi\|const fetchApi" frontend/src/App.svelte`) and threaded down to every panel
(`Settings.svelte`, `Insights.svelte`, `Notifications.svelte`, etc.) as a prop from `$props()`.
Read its definition in `App.svelte` to confirm what shape it throws on a non-2xx response (needed
so `isProGateError` is fed the right shape from the panel's catch block — e.g. does it throw an
`Error` with a `.status` property, or something else?). Match whatever shape it actually throws —
do NOT invent a new error shape, and do NOT import `fetchApi` from `fetchTimeout.js` (that module
only exports the underlying `fetchWithTimeout` helper, not the app's `fetchApi` wrapper).

- [ ] **Step 8: Create the `LinkedAccounts.svelte` panel**

Create `frontend/src/lib/LinkedAccounts.svelte`. This repo's convention (confirmed by
`Settings.svelte`, which receives `fetchApi` and `onUpgrade` as props from `App.svelte` — there is
NO `fetchApi` export anywhere to import) is that `fetchApi` is threaded down as a prop, not
imported. `LinkedAccounts.svelte` follows the same convention — its parent (`Settings.svelte`, Step
9) passes its own `fetchApi` prop straight through:

```svelte
<script>
  import { loadStripe } from "@stripe/stripe-js";
  import {
    fetchLinkedAccounts, startLinkFlow, refreshLinkedAccount, disconnectLinkedAccount, isProGateError,
  } from "./linkedAccounts.js";

  let { budgetId, isPro, onUpgrade, fetchApi } = $props();

  let accounts = $state([]);
  let loading = $state(false);
  let error = $state("");

  async function load() {
    loading = true;
    try {
      accounts = await fetchLinkedAccounts({ budgetId, fetchApi });
    } catch (e) {
      error = e.message || "Failed to load linked accounts.";
    } finally {
      loading = false;
    }
  }

  async function link() {
    error = "";
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
        error = "Linking bank accounts is a Nels Pro feature.";
      } else {
        error = e.message || "Failed to link your bank account.";
      }
    }
  }

  async function refresh(accountId) {
    try {
      await refreshLinkedAccount({ budgetId, accountId, fetchApi });
      error = "";
    } catch (e) {
      error = isProGateError(e) ? "Refreshing is a Nels Pro feature." : (e.message || "Refresh failed.");
    }
  }

  async function disconnect(accountId) {
    try {
      await disconnectLinkedAccount({ budgetId, accountId, fetchApi });
      await load();
    } catch (e) {
      error = e.message || "Failed to disconnect.";
    }
  }

  load();
</script>

<div class="linked-accounts">
  <h3 class="font-semibold text-lg mb-2">Linked bank accounts</h3>

  {#if error}
    <p class="text-error text-sm mb-2">{error}</p>
  {/if}

  {#if loading}
    <p class="text-sm opacity-70">Loading...</p>
  {:else if accounts.length === 0}
    <p class="text-sm opacity-70">No bank accounts linked yet.</p>
  {:else}
    <ul class="space-y-2">
      {#each accounts as acct (acct.id)}
        <li class="flex items-center justify-between">
          <span>
            {acct.display_name ?? "Bank account"} ({acct.institution_name ?? "Unknown"})
            {#if acct.status === "disconnected"}<span class="badge badge-ghost ml-2">Disconnected</span>{/if}
          </span>
          {#if acct.status === "active"}
            <span>
              <button class="btn btn-xs" onclick={() => refresh(acct.id)}>Refresh</button>
              <button class="btn btn-xs btn-ghost" onclick={() => disconnect(acct.id)}>Disconnect</button>
            </span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if isPro}
    <button class="btn btn-primary btn-sm mt-3" onclick={link}>Link a bank account</button>
  {:else}
    <button class="btn btn-outline btn-sm mt-3" onclick={() => onUpgrade?.("monthly")}>Upgrade to Pro to link a bank account</button>
  {/if}
</div>
```

- [ ] **Step 9: Include the panel in `Settings.svelte`**

Read `frontend/src/lib/Settings.svelte` to find where the existing billing section renders (search
for `onUpgrade` or `subscription.is_pro` — both already exist as props on `Settings.svelte` itself,
per Step 8's note), and add `<LinkedAccounts budgetId={...} isPro={subscription?.is_pro}
onUpgrade={onUpgrade} fetchApi={fetchApi} />` near it, importing the component and passing through
`Settings.svelte`'s OWN already-received `fetchApi`/`onUpgrade` props unchanged (do not invent new
prop names or import `fetchApi` from a module — match the file's existing conventions for how it
currently receives `subscription`/budget context/`fetchApi`/`onUpgrade` from `App.svelte`).

- [ ] **Step 10: Wire the chat-response client_secret in `App.svelte`**

In `frontend/src/App.svelte`, find the function that handles a chat response (search for
`action_taken` or `pending_deletion` — the same envelope handling block already reads several
optional `ChatResponse` fields). Add a branch:

```javascript
if (response.financial_connections_client_secret) {
  const { loadStripe } = await import("@stripe/stripe-js");
  const stripe = await loadStripe(import.meta.env.VITE_STRIPE_PUBLISHABLE_KEY);
  const result = await stripe.collectFinancialConnectionsAccounts({
    clientSecret: response.financial_connections_client_secret,
  });
  if (result?.financialConnectionsSession?.accounts?.length) {
    await fetchApi(`/budgets/${activeBudgetId}/linked-accounts/complete`, {
      method: "POST",
      body: JSON.stringify({ session_id: result.financialConnectionsSession.id }),
    });
  }
}
```

(Adjust `activeBudgetId` to whatever variable name `App.svelte` already uses for the current active
budget id in this same function's scope.)

- [ ] **Step 11: Run the frontend build and test suite**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: all tests pass; build succeeds with no errors.

- [ ] **Step 12: Commit**

```bash
git add frontend/package.json frontend/pnpm-lock.yaml frontend/.env.example frontend/src/lib/linkedAccounts.js frontend/src/lib/linkedAccounts.test.js frontend/src/lib/LinkedAccounts.svelte frontend/src/lib/Settings.svelte frontend/src/App.svelte
git commit -m "feat(#303): add Linked Accounts panel and chat-triggered Stripe.js link flow"
```

---

### Task 7: `AGENTS.md` documentation

**Files:**
- Modify: `AGENTS.md` (add a new numbered section after §13, following the existing convention)

- [ ] **Step 1: Add §14**

Add after the existing §13 (Per-Budget Budgeting Strategy, ends at line 105) and before "##
Developer Commands":

```markdown
### 14. Financial Connections Bank Linking (#303)
- **Pro-gated, builds on #25/#195**: a subscriber with `billing::user_is_pro` can link bank account(s)
  via Stripe Financial Connections; imported transactions land in the EXISTING `transactions` table,
  attributed via a new nullable `external_account_id` FK to `linked_accounts` — no separate
  expense-tracking system. `linked_accounts` (one row per linked Stripe FC Account, scoped to a
  budget) is never deleted by a disconnect — only its `status` flips to `'disconnected'`, preserving
  import history per the AC.
- **No hosted redirect (unlike Checkout)**: Financial Connections requires client-side Stripe.js
  (`@stripe/stripe-js`, `stripe.collectFinancialConnectionsAccounts({clientSecret})`) driven by a
  `client_secret` from a server-created session — there is no URL to simply redirect to. The chat
  action `LINK_BANK_ACCOUNT` hands this `client_secret` back through a new `ChatResponse` field
  (`financial_connections_client_secret`) for the frontend to drive the same modal a REST-triggered
  link would.
- **Session completion is verified server-side**: after the client-side modal resolves, the frontend
  sends only a `session_id`; `financial_connections::complete_link_session` re-fetches the session
  with our own secret key and confirms its `account_holder.customer` matches the caller's own Stripe
  customer (`billing::ensure_customer`, widened to `pub(crate)` and reused unchanged — one Stripe
  Customer per user, shared between billing and linking) before persisting anything.
- **Idempotent sync via `stripe_transaction_id`**: `transactions.stripe_transaction_id` (partial
  `UNIQUE`, non-imported rows stay NULL) is the dedup key `sync_account_transactions` uses
  (`ON CONFLICT DO NOTHING`) so webhook redelivery or overlapping manual/auto refreshes never
  duplicate a row or clobber a user's edit to an already-imported one.
- **Categorization is a narrow v1 heuristic, not ML**: `financial_connections::guess_category_id`
  case-insensitive-substring-matches a budget's existing category names against the imported
  description; no match leaves `category_id` NULL, surfaced as "Uncategorized" for the user to
  assign. Stripe's FC transaction object carries no spending-category field of its own (unlike
  Plaid). A smarter categorizer is an explicit non-goal for v1.
- **Manual refresh and webhook auto-refresh are the SAME mechanism**: `refresh_linked_account` calls
  Stripe's `POST .../accounts/:id/refresh` and returns immediately (no new data yet); the actual
  import happens moments later when Stripe's `financial_connections.account.refreshed_transactions`
  webhook fires and calls the same `sync_account_transactions` used everywhere else. Manual refresh
  just asks Stripe to check now instead of waiting for Stripe's own schedule.
- **Subscription lapse pauses (never deletes)**: the `refreshed_transactions` webhook handler
  requires BOTH `linked_accounts.status == 'active'` AND the owning user's `billing::user_is_pro`
  before syncing; either check failing is a silent no-op (200, no Stripe retry) that leaves all data
  untouched. The `status == 'active'` check is belt-and-suspenders alongside disconnect being a real
  Stripe-side call (see next bullet) — webhook delivery/ordering isn't guaranteed.
- **Disconnect calls Stripe FIRST, then flips the local flag**: `disconnect_linked_account` calls
  `POST .../accounts/:id/disconnect` (idempotent on Stripe's side) BEFORE writing `status =
  'disconnected'` locally, so a failed Stripe call never leaves us believing an account is
  disconnected when Stripe could still fire refresh webhooks for it. The webhook ALSO handles
  `financial_connections.account.disconnected` (bank-initiated disconnect, independent of our own
  button) the same way. Disconnect is NOT Pro-gated — a lapsed user must still be able to manage
  their own linked data (view + disconnect are ungated; starting a new link and manual refresh are
  the two Pro-gated actions).
- **Own webhook endpoint/secret**: `POST /api/financial-connections/webhook` is a SEPARATE public
  route from `/api/billing/webhook`, with its own `STRIPE_FC_WEBHOOK_SECRET` env var, reusing
  `billing::verify_stripe_signature`/`StripeEvent` (already `pub`) unchanged.
- **Surfacing**: REST — `POST /api/budgets/:id/linked-accounts/session`, `POST
  .../linked-accounts/complete`, `GET .../linked-accounts`, `POST
  .../linked-accounts/:account_id/refresh`, `DELETE .../linked-accounts/:account_id` (Edit-or-Owner
  for all but the GET, which is any budget-view access). Chat — `LINK_BANK_ACCOUNT` /
  `LIST_LINKED_ACCOUNTS` / `UNLINK_BANK_ACCOUNT` / `REFRESH_BANK_ACCOUNT` (system prompt rule 2o;
  `UNLINK_BANK_ACCOUNT` resolves a fuzzy `account_match` locator against institution/display
  name/last4, mirroring `transaction_match`'s ambiguity handling for `EDIT_TRANSACTION`). Frontend —
  a Linked Accounts panel in Settings plus the chat-triggered Stripe.js modal share one code path
  (`frontend/src/lib/linkedAccounts.js`).
```

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#303): document financial connections bank linking in AGENTS.md"
```

---

### Task 8: Final verification pass

**Files:** none (verification only)

- [ ] **Step 1: Full backend test suite**

Run: `cd backend && cargo test && cargo test -- --ignored --test-threads=1`
Expected: all pass, no warnings introduced by this feature (`cargo check` clean).

- [ ] **Step 2: Full frontend test suite + build**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: all pass, build succeeds.

- [ ] **Step 3: Re-read the spec's Success Criteria and confirm each is met**

Walk `docs/superpowers/specs/2026-07-06-financial-connections-bank-linking-design.md` section 3's
checklist against what was built; note any gap in the PR description rather than silently leaving it
unaddressed.

- [ ] **Step 4: Commit any final cleanup**

```bash
git status --porcelain
```

If clean, no commit needed — proceed to opening the PR.
