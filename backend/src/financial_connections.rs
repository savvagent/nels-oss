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

#[derive(Debug, Serialize)]
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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

/// Validate the shape of a client-supplied Stripe Financial Connections
/// session id BEFORE it's interpolated into a Stripe API URL path segment.
/// Real FC session ids are `fcsess_` followed by ASCII alphanumeric/underscore
/// characters. This is a pure, dependency-free check (no DB/HTTP) specifically
/// so it's unit-testable in isolation — see the tests below.
pub(crate) fn is_valid_fc_session_id(s: &str) -> bool {
    s.starts_with("fcsess_") && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Finish a link session: re-fetch the session SERVER-SIDE (never trust a
/// client-supplied account list) to get the authoritative linked accounts,
/// verify the session belongs to the caller's OWN Stripe customer (anti-replay
/// — prevents a user completing someone else's session id), persist each
/// account (idempotent re-link via `ON CONFLICT`), then attempt an initial
/// sync per account. The initial sync may legitimately import 0 rows — Stripe's
/// FC transaction data populates asynchronously; the real data typically lands
/// moments later via the `refreshed_transactions` webhook (see Task 3).
///
/// Returns `ListLinkedAccountsResponse` (the same shape `list_linked_accounts`
/// returns) rather than a bare `Vec` — both endpoints hand back conceptually
/// the same "list of linked accounts" payload, so they should look identical
/// to any consumer.
pub async fn complete_link_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    session_id: &str,
) -> Result<ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    if !is_valid_fc_session_id(session_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid session id".to_string()));
    }

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

        // `provider_account_id` is globally UNIQUE, but a Stripe FC account is
        // scoped to the ONE budget it was first linked into — the ON CONFLICT
        // below deliberately does not rebind budget_id/user_id. Without this
        // check, selecting an already-linked account while linking into a
        // DIFFERENT budget would silently reactivate/update the OTHER
        // budget's row (even un-disconnecting it) while `complete` reports
        // success for THIS budget — the account would never appear in this
        // budget's list. Reject explicitly instead of silently misattributing.
        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(&stripe_account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(
                    user_id = %user_id, stripe_account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "financial_connections account already linked to a different budget"
                );
                return Err((
                    StatusCode::CONFLICT,
                    "This bank account is already linked to a different budget. Disconnect it there first.".to_string(),
                ));
            }
        }

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

        // Best-effort initial sync; a failure here must not fail the whole
        // link (the account is already persisted and will pick up data via
        // the webhook regardless).
        if let Err((status, msg)) = sync_account_transactions(pool, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial financial_connections sync failed; will retry via webhook/manual refresh");
        }

        results.push(row.into());
    }
    Ok(ListLinkedAccountsResponse { accounts: results })
}

/// Pull transactions for one linked account from Stripe and idempotently
/// insert new rows into `transactions`. Returns the count actually inserted
/// (rows already seen via `provider_transaction_id` are silently skipped, NOT
/// counted). Updates `linked_accounts.last_synced_at` regardless of count —
/// including on a mid-pagination Stripe error, so a partial sync's progress
/// (however many pages were imported before the failure) is still recorded
/// rather than lost.
///
/// A genuine Stripe/network error while paginating IS surfaced as `Err` (the
/// error is ALSO already logged inside `stripe_get`) — this function's
/// contract is "best-effort, but tell the caller when it didn't complete" so
/// callers that log a warning on `Err` (`complete_link_session`, the
/// `refreshed_transactions` webhook arm) actually see that warning instead of
/// silently treating an incomplete sync as a clean success.
pub async fn sync_account_transactions(
    pool: &PgPool,
    linked_account: &crate::db::LinkedAccount,
) -> Result<u64, (StatusCode, String)> {
    // A closed (project) budget is read-only for new transactions (#48);
    // auto-imported transactions are still new transactions, so a closed
    // budget must stop receiving them via EVERY path — initial link (guarded
    // upstream in create_link_session/complete_link_session), manual refresh
    // (guarded in refresh_linked_account), AND Stripe's own webhook-driven
    // auto-refresh (which calls this function directly with no upstream
    // guard) all funnel through this single check. A closed budget is treated
    // as a clean no-op (not an error): skip quietly, don't even hit Stripe.
    let closed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT closed_at FROM budgets WHERE id = $1")
        .bind(linked_account.budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .flatten();
    if closed_at.is_some() {
        tracing::info!(
            account_id = %linked_account.id,
            budget_id = %linked_account.budget_id,
            "skipping financial_connections sync: budget is closed"
        );
        return Ok(0);
    }

    // Load the budget's categories once for the categorization heuristic,
    // in creation order (oldest first) so `guess_category_id`'s "first match
    // wins" is deterministic and stable across syncs.
    let categories: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM categories WHERE budget_id = $1 ORDER BY created_at ASC")
        .bind(linked_account.budget_id)
        .fetch_all(pool).await.map_err(internal_error)?;

    // Standing ignore rules (#374): load once per sync, then auto-set
    // excluded_from_budget on matching imports. A DB fault here degrades to
    // "no rules" rather than failing the whole sync.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

    let mut imported: u64 = 0;
    let mut starting_after: Option<String> = None;
    let mut stripe_error: Option<(StatusCode, String)> = None;
    loop {
        let mut path = format!("v1/financial_connections/transactions?account={}&limit=100", linked_account.provider_account_id);
        if let Some(cursor) = &starting_after {
            path.push_str(&format!("&starting_after={cursor}"));
        }
        let page = match stripe_get(&path).await {
            Ok(p) => p,
            // Already logged inside stripe_get. Stop paginating but fall
            // through to persist last_synced_at for the partial progress
            // made so far, THEN propagate the error to the caller.
            Err(e) => {
                stripe_error = Some(e);
                break;
            }
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
            let excluded = crate::ignore_rules::transaction_matches_ignore_rule(&description, linked_account.id, &ignore_rules);

            // NOTE: the ON CONFLICT target must repeat the partial index's WHERE
            // predicate exactly (`WHERE provider_transaction_id IS NOT NULL`) —
            // Postgres does not infer a plain column-list ON CONFLICT target
            // against a PARTIAL unique index; omitting the predicate here
            // raises "there is no unique or exclusion constraint matching the
            // ON CONFLICT specification" at runtime, on every insert. The
            // target is scoped per (external_account_id, provider_transaction_id)
            // rather than provider_transaction_id alone (#320) — Stripe FC's
            // `fctxn_...` ids are globally unique so this codebase's own
            // Stripe-only accounts never collided, but a second provider's ids
            // are not guaranteed globally unique across different accounts.
            // (Stripe FC in this codebase is US-only, so hardcoding 'USD' here
            // is a correct, not guessed, value — see spec Assumption 9.)
            let new_tx_id = Uuid::new_v4();
            let res = sqlx::query(
                "INSERT INTO transactions \
                    (id, budget_id, category_id, amount, transaction_date, description, \
                     external_account_id, provider_transaction_id, currency, excluded_from_budget, source, review_status) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'USD', $9, 'imported', 'needs_review') \
                 ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
            )
            .bind(new_tx_id)
            .bind(linked_account.budget_id)
            .bind(category_id)
            .bind(amount)
            .bind(transacted_at)
            .bind(&description)
            .bind(linked_account.id)
            .bind(&stripe_tx_id)
            .bind(excluded)
            .execute(pool).await.map_err(internal_error)?;
            if res.rows_affected() > 0 {
                imported += 1;
                // #403 P3: only a genuine fresh import (rows_affected > 0) can be a
                // new duplicate to reconcile; a re-delivery DO NOTHINGs and is skipped.
                crate::duplicate_match::link_duplicate_for_import(pool, linked_account.budget_id, new_tx_id).await;
            }
        }
        starting_after = rows.last().and_then(|t| t.get("id")).and_then(|v| v.as_str()).map(str::to_string);
        if page.get("has_more").and_then(|v| v.as_bool()) != Some(true) {
            break;
        }
    }

    sqlx::query("UPDATE linked_accounts SET last_synced_at = now(), updated_at = now() WHERE id = $1")
        .bind(linked_account.id)
        .execute(pool).await.map_err(internal_error)?;

    if let Some(e) = stripe_error {
        return Err(e);
    }

    Ok(imported)
}

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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    // Scoped to status = 'active': a disconnected account must not be
    // refreshable — refreshing it would make a needless Stripe call and
    // contradicts the invariant that only active accounts sync (mirrors the
    // webhook handler's own status == 'active' guard). A disconnected
    // account's id resolves to 404, same as a nonexistent/foreign one.
    let stripe_account_id: String = sqlx::query_scalar(
        "SELECT provider_account_id FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND status = 'active'")
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
        "SELECT provider_account_id FROM linked_accounts WHERE id = $1 AND budget_id = $2")
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

#[derive(Debug, Serialize)]
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
                "SELECT * FROM linked_accounts WHERE provider_account_id = $1")
                .bind(&stripe_account_id)
                .fetch_optional(&state.db).await.map_err(internal_error)?;
            match linked {
                Some(row) if row.status == "active" => {
                    let owner_status: Option<String> = sqlx::query_scalar(
                        "SELECT status FROM subscriptions WHERE user_id = $1")
                        .bind(row.user_id).fetch_optional(&state.db).await.map_err(internal_error)?.flatten();
                    if user_is_pro(owner_status.as_deref()) {
                        if let Err((status, msg)) = sync_account_transactions(&state.db, &row).await {
                            tracing::warn!(?status, %msg, account_id = %row.id, "webhook-driven sync failed");
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
                 WHERE provider_account_id = $1 AND status = 'active'")
                .bind(&stripe_account_id)
                .execute(&state.db).await.map_err(internal_error)?;
        }
        None => tracing::debug!(event_type = %event.event_type, "ignoring unhandled financial_connections event type"),
    }
    Ok(StatusCode::OK)
}

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
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    complete_link_session(&state.db, user_id, budget_id, &req.session_id).await.map(Json)
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

    #[test]
    fn is_valid_fc_session_id_accepts_well_formed_id() {
        assert!(is_valid_fc_session_id("fcsess_1AbC_23"));
    }

    #[test]
    fn is_valid_fc_session_id_rejects_missing_prefix() {
        assert!(!is_valid_fc_session_id("sess_1AbC23"));
    }

    #[test]
    fn is_valid_fc_session_id_rejects_empty_string() {
        assert!(!is_valid_fc_session_id(""));
    }

    #[test]
    fn is_valid_fc_session_id_rejects_non_alphanumeric_characters() {
        assert!(!is_valid_fc_session_id("fcsess_1AbC23; DROP TABLE users;"));
        assert!(!is_valid_fc_session_id("fcsess_1AbC/23"));
    }

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
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
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
        // 'trialing' (not 'active') so `entitlement::resolve` maps the owner to
        // `Tier::Pro` unconditionally in tests, without needing a Pro price env
        // var or `#[serial]`, unlike 'active' which requires a Pro price to be
        // configured to resolve above Basic.
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
            .bind(user_id).bind(customer_id).execute(db).await.unwrap();
    }

    async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, stripe_account_id: &str) -> crate::db::LinkedAccount {
        sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'stripe', $4, 'cus_x') RETURNING *")
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

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'fctxn_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1);

        // Assert the persisted amount actually went through normalize_amount's
        // cents->dollars/abs() conversion, not just that a row landed.
        let amount: f64 = sqlx::query_scalar("SELECT amount FROM transactions WHERE provider_transaction_id = 'fctxn_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(amount, 15.0, "Stripe's -1500 cents must normalize to a positive $15.00");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_auto_excludes_transactions_matching_an_ignore_rule() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        // Two imported rows: one matches the standing ignore rule
        // ("card payment"), one does not.
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id":"fctxn_excl","amount":-5000,"description":"VISA CARD PAYMENT","transacted_at":1_700_000_000},
                    {"id":"fctxn_keep","amount":-2500,"description":"GROCERY STORE","transacted_at":1_700_000_100}
                ],
                "has_more": false
            })))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_sync_ignore").await;

        // Budget-wide ignore rule (external_account_id NULL).
        sqlx::query("INSERT INTO transaction_ignore_rules (id, budget_id, match_text) VALUES ($1, $2, 'card payment')")
            .bind(Uuid::new_v4()).bind(bid).execute(&db).await.unwrap();

        let imported = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(imported, 2);

        let matched_excluded: bool = sqlx::query_scalar(
            "SELECT excluded_from_budget FROM transactions WHERE budget_id = $1 AND description ILIKE '%card payment%'")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert!(matched_excluded, "a transaction matching the ignore rule must be auto-excluded");

        let grocery_excluded: bool = sqlx::query_scalar(
            "SELECT excluded_from_budget FROM transactions WHERE budget_id = $1 AND description ILIKE '%grocery%'")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert!(!grocery_excluded, "a non-matching transaction must not be excluded");

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

    // A closed (project) budget must stop ALL sync paths (not just initial
    // link) — see the doc comments on `create_link_session` and
    // `sync_account_transactions`. These two tests cover the two remaining
    // paths the integration review flagged: manual refresh, and the single
    // choke point (`sync_account_transactions`) that also protects the
    // webhook-driven path.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_skips_closed_budget() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        // If the guard is broken and Stripe gets called anyway, this GET
        // fires; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [], "has_more": false})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_closed_sync_1").await;

        // closed_at is only representable on a project budget (DB CHECK
        // budgets_closed_only_project_check) — flip budget_type too, since
        // ensure_not_closed (and this function's own closed_at check) only
        // inspects closed_at.
        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = now() WHERE id = $1")
            .bind(bid)
            .execute(&db).await.unwrap();

        let result = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(result, 0, "a closed budget must import 0 transactions and never call Stripe");
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_linked_account_rejects_closed_budget() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        // If the guard is broken, refresh_linked_account would call Stripe's
        // refresh endpoint; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("POST")).and(path("/v1/financial_connections/accounts/fca_closed_refresh_1/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fca_closed_refresh_1"})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_closed_refresh").await;
        let linked = mk_linked_account(&db, bid, uid, "fca_closed_refresh_1").await;

        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = now() WHERE id = $1")
            .bind(bid)
            .execute(&db).await.unwrap();

        let result = refresh_linked_account(&db, uid, bid, linked.id).await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::CONFLICT,
            "manual refresh on a closed budget must 409, not reach Stripe"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_linked_account_rejects_disconnected_account() {
        // A locally-disconnected account must not be refreshable — refreshing
        // it would make a needless Stripe call and contradicts the invariant
        // that only 'active' accounts sync (mirrors the webhook handler's own
        // status == 'active' guard).
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("POST")).and(path("/v1/financial_connections/accounts/fca_disc_refresh_1/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fca_disc_refresh_1"})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_disc_refresh").await;
        let linked = mk_linked_account(&db, bid, uid, "fca_disc_refresh_1").await;
        sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now() WHERE id = $1")
            .bind(linked.id).execute(&db).await.unwrap();

        let result = refresh_linked_account(&db, uid, bid, linked.id).await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::NOT_FOUND,
            "manual refresh on a disconnected account must 404 (looks not-found), not reach Stripe"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_relink_to_a_different_budget() {
        // provider_account_id is globally UNIQUE, and the ON CONFLICT in
        // complete_link_session deliberately does not rebind budget_id. If a
        // user selects an already-linked account while linking into a
        // DIFFERENT budget, this must be rejected explicitly rather than
        // silently reactivating/updating the OTHER budget's row while
        // reporting success for THIS budget.
        let server = MockServer::start().await;
        set_stripe_env(&server);
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_relink").await;
        // Already linked to budget A.
        let existing = mk_linked_account(&db, bid_a, uid, "fca_relink_1").await;

        Mock::given(method("GET")).and(path("/v1/financial_connections/sessions/fcsess_relink_test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "fcsess_relink_test",
                "account_holder": { "customer": "cus_relink" },
                "accounts": { "data": [
                    { "id": "fca_relink_1", "display_name": "Checking", "institution_name": "Test Bank" }
                ]}
            })))
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid_b, "fcsess_relink_test").await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::CONFLICT,
            "linking an already-linked account into a different budget must be rejected, not silently reassigned"
        );

        // The original row must be untouched — still belongs to budget A, still active.
        let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
            "SELECT * FROM linked_accounts WHERE id = $1")
            .bind(existing.id).fetch_one(&db).await.unwrap();
        assert_eq!(row.budget_id, bid_a);
        assert_eq!(row.status, "active");

        // And it must NOT appear under budget B.
        let count_under_b: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
            .bind(bid_b).fetch_one(&db).await.unwrap();
        assert_eq!(count_under_b, 0);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    // --- Real end-to-end webhook-handler tests -----------------------------
    //
    // The tests above (`map_fc_webhook_event_*`) only exercise the pure event
    // parser. The webhook `handler`'s actual guard — status == 'active' AND
    // owner is Pro, checked with a DB round trip BEFORE calling
    // `sync_account_transactions` — was previously "covered" only by a test
    // that manually UPDATEd a row and re-SELECTed it, never touching
    // `webhook`, `map_fc_webhook_event`, or `sync_account_transactions` at
    // all; that test would keep passing even if the entire guard were
    // deleted. These two replace it: they sign a real webhook payload, call
    // the `webhook` axum handler directly (handlers are plain async fns —
    // no HTTP server needed), and assert via a wiremock `.expect(0)` that
    // `sync_account_transactions`'s underlying Stripe GET is never made. A
    // wiremock `Mock` panics on drop if its expectation isn't met, so a
    // regression that removes/weakens the guard fails these tests loudly
    // rather than silently.
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256Test = Hmac<Sha256>;

    /// Sign a webhook payload exactly like `billing.rs`'s own test `sign`
    /// helper (duplicated here since that one is private to billing's test
    /// module) — `t=<unix>,v1=<hex hmac of "{t}.{payload}">`.
    fn sign(secret: &str, t: i64, payload: &[u8]) -> String {
        let mut signed = format!("{t}.").into_bytes();
        signed.extend_from_slice(payload);
        let mut mac = HmacSha256Test::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(&signed);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("t={t},v1={hex}")
    }

    fn fc_webhook_body(event_type: &str, stripe_account_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": event_type,
            "created": 1_700_000_000,
            "data": { "object": { "id": stripe_account_id, "object": "financial_connections.account" } }
        })).unwrap()
    }

    fn test_state(db: PgPool) -> AppState {
        AppState {
            db,
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    fn signed_headers(secret: &str, body: &[u8]) -> axum::http::HeaderMap {
        let t = chrono::Utc::now().timestamp();
        let sig = sign(secret, t, body);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("stripe-signature", axum::http::HeaderValue::from_str(&sig).unwrap());
        headers
    }

    fn set_fc_webhook_secret(secret: &str) {
        std::env::set_var("STRIPE_FC_WEBHOOK_SECRET", secret);
    }
    fn clear_fc_webhook_secret() {
        std::env::remove_var("STRIPE_FC_WEBHOOK_SECRET");
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_skips_sync_for_disconnected_account() {
        // Owner IS Pro here — isolates that the guard's `status == 'active'`
        // half alone is enough to skip the sync, independent of the Pro check.
        const SECRET: &str = "whsec_fc_test_disconnected";
        let server = MockServer::start().await;
        set_stripe_env(&server);
        set_fc_webhook_secret(SECRET);
        // If the guard is broken and sync_account_transactions runs anyway,
        // this GET fires; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [], "has_more": false})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_wh_disc").await;
        let linked = mk_linked_account(&db, bid, uid, "fca_wh_disc_1").await;
        // Must set disconnected_at alongside status here: the DB now enforces
        // linked_accounts_status_disconnected_at_check (status='disconnected'
        // iff disconnected_at IS NOT NULL).
        sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now() WHERE id = $1")
            .bind(linked.id).execute(&db).await.unwrap();

        let body = fc_webhook_body("financial_connections.account.refreshed_transactions", "fca_wh_disc_1");
        let headers = signed_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(State(state), headers, Bytes::from(body)).await
            .expect("webhook must return Ok even for a guarded no-op (Stripe expects 200)");
        assert_eq!(status, StatusCode::OK);
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
        clear_fc_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_skips_sync_for_non_pro_owner() {
        // Status IS 'active' here — isolates that the Pro-check half alone is
        // enough to skip the sync, independent of the status check. No
        // subscriptions row at all for the owner -> not Pro (mirrors
        // `non_pro_user_cannot_start_link_session`'s "no row" convention).
        const SECRET: &str = "whsec_fc_test_nonpro";
        let server = MockServer::start().await;
        set_stripe_env(&server);
        set_fc_webhook_secret(SECRET);
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [], "has_more": false})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_wh_nonpro_1").await;
        // Sanity-check the precondition this test relies on.
        assert_eq!(linked.status, "active");

        let body = fc_webhook_body("financial_connections.account.refreshed_transactions", "fca_wh_nonpro_1");
        let headers = signed_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(State(state), headers, Bytes::from(body)).await
            .expect("webhook must return Ok even for a guarded no-op (Stripe expects 200)");
        assert_eq!(status, StatusCode::OK);
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
        clear_fc_webhook_secret();
    }

    // --- complete_link_session coverage --------------------------------
    //
    // Previously zero test coverage (pr-test-analyzer's top finding): the
    // anti-replay check, the happy path, and the closed-budget rejection
    // were all unexercised. `mk_pro_subscription` already seeds
    // `subscriptions.stripe_customer_id`, so `ensure_customer` short-circuits
    // on its DB lookup and these tests never need to mock `POST
    // /v1/customers`.

    fn fc_session_body(id: &str, customer_id: &str, accounts: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "account_holder": { "type": "customer", "customer": customer_id },
            "accounts": { "data": accounts },
        })
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_session_belonging_to_another_customer() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("GET")).and(path("/v1/financial_connections/sessions/fcsess_replay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fc_session_body(
                "fcsess_replay_1",
                "cus_someone_else",
                serde_json::json!([{"id": "fca_replay_1", "display_name": "Checking", "last4": "1234"}]),
            )))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_mine").await;

        let result = complete_link_session(&db, uid, bid, "fcsess_replay_1").await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::FORBIDDEN,
            "a session whose account_holder.customer doesn't match the caller's own customer id must be rejected"
        );

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE provider_account_id = 'fca_replay_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 0, "nothing must be persisted when the anti-replay check fails");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_happy_path_persists_account_and_returns_it() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("GET")).and(path("/v1/financial_connections/sessions/fcsess_happy_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fc_session_body(
                "fcsess_happy_1",
                "cus_happy",
                serde_json::json!([{"id": "fca_happy_1", "display_name": "Checking", "institution_name": "Test Bank", "last4": "4242"}]),
            )))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/v1/financial_connections/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [], "has_more": false})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_happy").await;

        let result = complete_link_session(&db, uid, bid, "fcsess_happy_1").await
            .expect("matching customer + valid session must succeed");
        assert_eq!(result.accounts.len(), 1);
        assert_eq!(result.accounts[0].institution_name.as_deref(), Some("Test Bank"));

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE provider_account_id = 'fca_happy_1' AND budget_id = $1")
            .bind(bid)
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_closed_budget() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        // If the guard is broken, complete_link_session would call Stripe's
        // sessions endpoint; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/v1/financial_connections/sessions/fcsess_closed_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fc_session_body(
                "fcsess_closed_1", "cus_closed", serde_json::json!([]),
            )))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_closed").await;

        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = now() WHERE id = $1")
            .bind(bid)
            .execute(&db).await.unwrap();

        let result = complete_link_session(&db, uid, bid, "fcsess_closed_1").await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::CONFLICT,
            "completing a link session on a closed budget must 409, not reach Stripe"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_malformed_session_id_before_calling_stripe() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        // Any GET at all here means the shape check didn't short-circuit.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_malformed").await;

        let result = complete_link_session(&db, uid, bid, "not-a-real-session-id").await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    // --- DB CHECK constraint coverage ----------------------------------
    //
    // These prove the invariants added to the migration are actually
    // enforced AT THE DB LEVEL, not just honored by convention in the Rust
    // call sites that happen to set both columns together today.

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn transactions_external_account_pairing_check_rejects_mixed_nulls() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_check_pairing_1").await;

        // Insert a normal manual transaction (both columns NULL — allowed),
        // then try to set only external_account_id without provider_transaction_id.
        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, amount, transaction_date, description) \
             VALUES ($1, $2, 10.0, now(), 'manual')")
            .bind(tx_id).bind(bid)
            .execute(&db).await.unwrap();

        let result = sqlx::query(
            "UPDATE transactions SET external_account_id = $1 WHERE id = $2")
            .bind(linked.id).bind(tx_id)
            .execute(&db).await;
        assert!(
            result.is_err(),
            "setting external_account_id without provider_transaction_id must violate the pairing CHECK"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn linked_accounts_status_disconnected_at_check_rejects_mismatch() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "fca_check_status_1").await;

        // status = 'disconnected' but disconnected_at left NULL must fail.
        let result = sqlx::query(
            "UPDATE linked_accounts SET status = 'disconnected' WHERE id = $1")
            .bind(linked.id)
            .execute(&db).await;
        assert!(
            result.is_err(),
            "status='disconnected' without disconnected_at must violate the status/disconnected_at CHECK"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
}
