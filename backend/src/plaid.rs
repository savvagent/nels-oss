//! Plaid bank-account linking for Canada (nels#321): link a bank account via
//! Plaid Link's embeddable JS modal, auto-import its transactions into the
//! existing `transactions` table, keep them refreshed via Plaid's transactions
//! webhook or manual refresh. Third `bank_provider::Provider` variant against
//! #320's existing dispatch layer — mirrors `financial_connections.rs`'s/
//! `gocardless.rs`'s "own HTTP client, own env-driven test seam" convention,
//! but JSON-bodied with client_id/secret in every request body (Plaid's own
//! auth convention), not a bearer token or cached credential.

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AppState;
use crate::billing::user_is_pro;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;
// `ListLinkedAccountsResponse` reused rather than redefined — gocardless.rs
// sets this exact precedent (its complete_link_session returns
// `crate::financial_connections::ListLinkedAccountsResponse` too).
// `guess_category_id` is shared as-is (pure, provider-agnostic substring
// heuristic) by `sync_account_transactions` below.
use crate::financial_connections::{guess_category_id, ListLinkedAccountsResponse};

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

pub(crate) fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
pub(crate) fn plaid_api_base() -> String {
    env_opt("PLAID_API_BASE").unwrap_or_else(|| "https://production.plaid.com".to_string())
}
// Mirrors `gocardless.rs`'s `app_url()` exactly (same fallback value) — the
// single source of truth for APP_URL's dev fallback, so every APP_URL-derived
// URL in this module (redirect_uri, webhook) degrades the same way when
// APP_URL is unset, instead of `webhook` silently going host-less.
fn app_url() -> String {
    env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string())
}
fn plaid_redirect_uri() -> String {
    env_opt("PLAID_REDIRECT_URI").unwrap_or_else(|| format!("{}/", app_url().trim_end_matches('/')))
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// POST JSON to the Plaid API, with `client_id`/`secret` merged into every
/// body (Plaid's own auth convention — unlike Stripe's bearer-token or
/// GoCardless's cached-token approaches). Mirrors `stripe_post`'s/
/// `gocardless_post`'s error-shape convention (502 on any non-2xx, logged).
pub(crate) async fn plaid_post(path: &str, mut body: serde_json::Value) -> Result<serde_json::Value, (StatusCode, String)> {
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
        "webhook": format!("{}/api/plaid/webhook", app_url().trim_end_matches('/')),
        "redirect_uri": plaid_redirect_uri(),
    });
    let resp = plaid_post("link/token/create", body).await?;
    let link_token = resp.get("link_token").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid link/token/create missing link_token"))?
        .to_string();
    Ok(LinkTokenResponse { link_token, session_id })
}

/// Exchange a Plaid `public_token` for accounts and persist them, after
/// verifying (spec Assumption 5) that `session_id` is a `pending` session
/// belonging to THIS `budget_id`/`user_id` — the anti-replay check a raw
/// Plaid public_token has no other way to get. Pro-gated (402),
/// Edit-or-Owner-gated (403), budget-not-closed-gated (409) — same three
/// guards as `create_link_token`.
pub async fn complete_link_session(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    budget_id: Uuid,
    session_id: Uuid,
    public_token: &str,
) -> Result<ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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

    // Atomically claim this session as completed BEFORE calling out to Plaid
    // or touching `linked_accounts` — closes a TOCTOU window where two
    // concurrent requests for the same session_id could both pass a plain
    // `status == "pending"` application-code check before either committed
    // its completion, both exchanging the same (single-use) public_token and
    // double-inserting accounts. The `AND status = 'pending'` guard makes the
    // claim atomic: only one concurrent request's UPDATE can affect a row: a
    // losing request sees `rows_affected() == 0` and bails out with the same
    // 409 the old post-hoc `session.status != "pending"` check returned,
    // without ever calling `item/public_token/exchange`.
    let claim = sqlx::query("UPDATE plaid_link_sessions SET status = 'completed' WHERE id = $1 AND status = 'pending'")
        .bind(session_id)
        .execute(pool).await.map_err(internal_error)?;
    if claim.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, "This link session was already completed".to_string()));
    }

    let exchange = plaid_post("item/public_token/exchange", serde_json::json!({ "public_token": public_token })).await?;
    let access_token = exchange.get("access_token").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing access_token"))?.to_string();
    let item_id = exchange.get("item_id").and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing item_id"))?.to_string();

    // `access_token` is a live bank-data credential, not just an identifier —
    // encrypted before being persisted into `provider_ref` (Copilot review
    // finding, #321: storing it in plaintext increases the blast radius of a
    // DB leak), reusing the same `crypto::SecretCipher` AES-256-GCM scheme
    // this codebase already uses for Akahu's access token (#323) and TOTP
    // secrets. The RAW `access_token` above is still used for the remaining
    // Plaid API calls in this function; only the DB-persisted copy is
    // encrypted. Every reader of a Plaid `provider_ref`
    // (`sync_account_transactions`, `disconnect_linked_account`) must decrypt
    // it before using it as a bearer token.
    let encrypted_token = cipher.encrypt(&access_token)
        .map_err(|e| internal_error(format!("plaid token encrypt: {e}")))?;

    let accounts_resp = plaid_post("accounts/get", serde_json::json!({ "access_token": access_token })).await?;
    let institution_id = accounts_resp.pointer("/item/institution_id").and_then(|v| v.as_str()).map(str::to_string);
    let institution_name = match &institution_id {
        Some(iid) => {
            match plaid_post("institutions/get_by_id", serde_json::json!({
                "institution_id": iid, "country_codes": ["CA"],
            })).await {
                Ok(inst) => inst.pointer("/institution/name").and_then(|n| n.as_str()).map(str::to_string),
                Err((status, msg)) => {
                    // Best-effort secondary lookup (mirrors gocardless.rs's
                    // account-details-fetch failure handling): a transient
                    // Plaid outage here must not silently masquerade as "this
                    // institution genuinely has no name" — log so a missing
                    // institution_name can be diagnosed after the fact, then
                    // fall back to None exactly as before.
                    tracing::warn!(
                        ?status, %msg, institution_id = %iid,
                        "plaid institutions/get_by_id failed; proceeding without institution_name"
                    );
                    None
                }
            }
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
                provider_ref = EXCLUDED.provider_ref, plaid_item_id = EXCLUDED.plaid_item_id, \
                institution_name = EXCLUDED.institution_name, display_name = EXCLUDED.display_name, \
                last4 = EXCLUDED.last4, status = 'active', disconnected_at = NULL, updated_at = now() \
             RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(&account_id).bind(&encrypted_token)
            .bind(&institution_id).bind(&institution_name).bind(&display_name).bind(&last4).bind(&item_id)
            .fetch_one(pool).await.map_err(internal_error)?;

        if let Err((status, msg)) = sync_account_transactions(pool, cipher, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial plaid sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    // Session was already atomically claimed as 'completed' above, before the
    // Plaid exchange — no further write to plaid_link_sessions needed here.
    Ok(ListLinkedAccountsResponse { accounts: results })
}

/// Hard upper bound on `/transactions/sync` pages fetched by a single call —
/// a safety valve that does NOT trust Plaid's own `has_more`/`next_cursor`
/// fields unconditionally, mirroring `financial_connections.rs`'s pagination
/// loop (which independently breaks on `rows.is_empty()` regardless of
/// Stripe's own `has_more`). Without this, a buggy/misbehaving response
/// (`has_more: true` forever, or missing `next_cursor` so no progress is
/// ever made) would hang the caller — the initial link's inline sync, manual
/// refresh's synchronous call, and eventually the webhook handler — while
/// holding a DB connection/task open and hammering Plaid without limit. 500
/// pages is generous for any real account's transaction volume.
const MAX_SYNC_PAGES: u32 = 500;

/// Pull transactions for one linked account via Plaid's `/transactions/sync`
/// using THIS ROW'S OWN cursor (spec Assumption 7 — Plaid's sync model is
/// per-Item, but this codebase's sync abstraction is per-linked_accounts-row;
/// filtering the item-wide delta to this row's own account_id and advancing
/// this row's own cursor avoids a new Item-level table at the cost of
/// redundant calls when one Item backs multiple rows — a documented,
/// idempotent-safe v1 tradeoff). Returns the count of NEWLY inserted rows
/// (already-seen rows via provider_transaction_id are silently skipped;
/// Plaid's `removed` array is applied as deletes and does NOT affect this
/// count). Same "closed budget -> clean no-op" guard as
/// `financial_connections::sync_account_transactions`.
pub async fn sync_account_transactions(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
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

    // Standing ignore rules (#374): loaded once per sync, applied to both the
    // `added` and (first-import) `modified` INSERT paths below.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

    // `provider_ref` holds the Plaid access token ENCRYPTED at rest (Copilot
    // review finding, #321, mirroring Akahu's #323 fix) — decrypted here,
    // once, before use as the bearer credential in every /transactions/sync page.
    let access_token = cipher.decrypt(&linked_account.provider_ref)
        .map_err(|e| internal_error(format!("plaid token decrypt: {e}")))?;

    let mut imported: u64 = 0;
    let mut cursor = linked_account.plaid_cursor.clone();
    let mut has_more = true;
    let mut plaid_error: Option<(StatusCode, String)> = None;
    let mut pages: u32 = 0;
    while has_more {
        pages += 1;
        if pages > MAX_SYNC_PAGES {
            tracing::error!(
                account_id = %linked_account.id, budget_id = %linked_account.budget_id, pages,
                "plaid transactions/sync exceeded the max page cap without has_more=false; aborting to avoid an unbounded loop"
            );
            plaid_error = Some((StatusCode::BAD_GATEWAY, "Bank linking provider returned an unexpected response — please try again".to_string()));
            break;
        }
        let mut body = serde_json::json!({ "access_token": access_token });
        if let Some(c) = &cursor {
            body["cursor"] = serde_json::Value::String(c.clone());
        }
        let page = match plaid_post("transactions/sync", body).await {
            Ok(p) => p,
            Err(e) => { plaid_error = Some(e); break; }
        };
        let added = page.get("added").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let modified = page.get("modified").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let removed = page.get("removed").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        // `added` and `modified` are handled with DIFFERENT conflict behavior
        // (code review finding): `added` uses ON CONFLICT DO NOTHING, correct
        // for the idempotent-redelivery case (a webhook/manual-refresh
        // redelivering a page we've already imported must never duplicate or
        // clobber a user's own edit to an already-imported row). `modified`
        // is semantically different — Plaid is telling us the SOURCE data
        // itself changed (e.g. a pending amount corrected to its final
        // posted amount, or a date shifted) — so silently no-op'ing it via
        // the same DO NOTHING clause would leave the user's ledger holding a
        // stale/wrong amount forever with no path to correct it. `modified`
        // therefore uses ON CONFLICT DO UPDATE for amount/description/
        // transaction_date (Plaid-sourced fields only) while deliberately
        // NOT touching category_id, preserving any category the user (or
        // guess_category_id) already assigned.
        for tx in added.iter() {
            let tx_account_id = tx.get("account_id").and_then(|v| v.as_str()).unwrap_or("");
            if tx_account_id != linked_account.provider_account_id {
                continue; // belongs to a sibling account under the same Item — its own row's sync call covers it
            }
            let tx_id = match tx.get("transaction_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    tracing::warn!(account_id = %linked_account.id, "plaid transactions/sync: added entry missing transaction_id; skipping");
                    continue;
                }
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
            let excluded = crate::ignore_rules::transaction_matches_ignore_rule(&description, linked_account.id, &ignore_rules);

            let new_tx_id = Uuid::new_v4();
            let res = sqlx::query(
                "INSERT INTO transactions \
                    (id, budget_id, category_id, amount, transaction_date, description, \
                     external_account_id, provider_transaction_id, currency, excluded_from_budget, source, review_status) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'CAD', $9, 'imported', 'needs_review') \
                 ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING")
                .bind(new_tx_id).bind(linked_account.budget_id).bind(category_id).bind(amount)
                .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id).bind(excluded)
                .execute(pool).await.map_err(internal_error)?;
            if res.rows_affected() > 0 {
                imported += 1;
                // #403 P3: reconcile a genuinely new import against a Nels-logged
                // twin. Only the `added` path runs the matcher; `modified` below is
                // a DO UPDATE to an already-imported row, never a fresh duplicate.
                crate::duplicate_match::link_duplicate_for_import(pool, linked_account.budget_id, new_tx_id).await;
            }
        }
        for tx in modified.iter() {
            let tx_account_id = tx.get("account_id").and_then(|v| v.as_str()).unwrap_or("");
            if tx_account_id != linked_account.provider_account_id {
                continue; // belongs to a sibling account under the same Item — its own row's sync call covers it
            }
            let tx_id = match tx.get("transaction_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    tracing::warn!(account_id = %linked_account.id, "plaid transactions/sync: modified entry missing transaction_id; skipping");
                    continue;
                }
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
            let excluded = crate::ignore_rules::transaction_matches_ignore_rule(&description, linked_account.id, &ignore_rules);

            // ON CONFLICT DO UPDATE (not DO NOTHING): a `modified` entry for a
            // transaction not yet imported (rare — e.g. we missed its own
            // `added` event on an earlier page) is inserted fresh with a
            // guessed category, same as `added`; one that already exists has
            // its Plaid-sourced fields refreshed to the correction.
            // category_id is intentionally excluded from the UPDATE so a
            // user's own re-categorization of an already-imported row is
            // never overwritten by this correction. excluded_from_budget is
            // likewise set only on first INSERT and deliberately NOT in the
            // SET list, so a user's manual include/exclude toggle on an
            // already-imported row survives a `modified` re-sync (#374).
            sqlx::query(
                "INSERT INTO transactions \
                    (id, budget_id, category_id, amount, transaction_date, description, \
                     external_account_id, provider_transaction_id, currency, excluded_from_budget, source, review_status) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'CAD', $9, 'imported', 'needs_review') \
                 ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO UPDATE SET \
                    amount = EXCLUDED.amount, transaction_date = EXCLUDED.transaction_date, \
                    description = EXCLUDED.description, updated_at = now()")
                .bind(Uuid::new_v4()).bind(linked_account.budget_id).bind(category_id).bind(amount)
                .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id).bind(excluded)
                .execute(pool).await.map_err(internal_error)?;
        }
        // Plaid retracts transactions (a pending charge later cancelled, or
        // merged into a posted transaction under a different id) via this
        // `removed` array — distinct from `added`/`modified`. Left unhandled,
        // retracted transactions would stay in `transactions` forever. Same
        // per-row account_id filtering discipline as `added`/`modified` above
        // (an Item can back multiple linked_accounts rows). Does not affect
        // `imported` — a removal isn't an import.
        for tx in &removed {
            let tx_account_id = tx.get("account_id").and_then(|v| v.as_str()).unwrap_or("");
            if tx_account_id != linked_account.provider_account_id {
                continue; // belongs to a sibling account under the same Item
            }
            let tx_id = match tx.get("transaction_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    tracing::warn!(account_id = %linked_account.id, "plaid transactions/sync: removed entry missing transaction_id; skipping");
                    continue;
                }
            };
            sqlx::query("DELETE FROM transactions WHERE external_account_id = $1 AND provider_transaction_id = $2")
                .bind(linked_account.id).bind(&tx_id)
                .execute(pool).await.map_err(internal_error)?;
        }

        has_more = page.get("has_more").and_then(|v| v.as_bool()).unwrap_or(false);
        let next_cursor = page.get("next_cursor").and_then(|v| v.as_str()).filter(|c| !c.is_empty()).map(str::to_string);
        match next_cursor {
            Some(nc) => cursor = Some(nc),
            None if has_more => {
                // Plaid says more pages remain but gave us nothing to advance
                // the cursor with. Repeating the identical request would
                // never make progress (this is exactly the case that would
                // otherwise loop forever) — treat it as a real provider error
                // rather than silently breaking and reporting success.
                tracing::error!(
                    account_id = %linked_account.id, budget_id = %linked_account.budget_id,
                    "plaid transactions/sync returned has_more=true with no next_cursor; aborting to avoid an unbounded loop"
                );
                plaid_error = Some((StatusCode::BAD_GATEWAY, "Bank linking provider returned an unexpected response — please try again".to_string()));
                break;
            }
            None => {} // has_more is false — cursor keeps its last real value, loop ends below anyway
        }
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

/// Manual refresh: Pro-gated, budget-not-closed-gated. UNLIKE Stripe/
/// GoCardless (which kick off an async request and let a webhook/poll
/// deliver the result later), Plaid has no separate "please check now"
/// endpoint — `/transactions/sync` IS the pull, synchronous and immediate
/// (spec Assumption 13). Short-circuits with 409 on a `consent_expired` row
/// WITHOUT calling Plaid, mirroring GoCardless's manual-refresh guard.
pub async fn refresh_linked_account(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'plaid'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;
    if row.status == "consent_expired" {
        return Err((StatusCode::CONFLICT, "This account's bank connection needs to be reconnected.".to_string()));
    }
    if row.status != "active" {
        return Err((StatusCode::NOT_FOUND, "Linked account not found".to_string()));
    }

    sync_account_transactions(pool, cipher, &row).await?;
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
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    budget_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;

    let row = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'plaid'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    // provider_ref is encrypted at rest (see sync_account_transactions) — decrypt before calling Plaid.
    let access_token = cipher.decrypt(&row.provider_ref)
        .map_err(|e| internal_error(format!("plaid token decrypt: {e}")))?;
    plaid_post("item/remove", serde_json::json!({ "access_token": access_token })).await?;

    let item_id = row.plaid_item_id.clone();
    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() \
         WHERE (plaid_item_id = $1 AND $1 IS NOT NULL) OR id = $2")
        .bind(&item_id).bind(account_id)
        .execute(pool).await.map_err(internal_error)?;
    Ok(())
}

// --- Webhook: JWT/JWK verification + dispatch (nels#321) -------------------

use axum::body::Bytes;
use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

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

#[derive(Debug, Clone, Deserialize)]
struct PlaidJwk {
    kid: String,
    #[serde(rename = "x")]
    x: String,
    #[serde(rename = "y")]
    y: String,
    // Plaid's own canonical guidance for `/webhook_verification_key/get`:
    // a non-null `expired_at` means Plaid has revoked/rotated this key and it
    // must never again be trusted to verify a signature — even one that was
    // already sitting in `JWK_CACHE` (see `fetch_jwk` below). Nullable Unix
    // timestamp, hence `Option<i64>`.
    #[serde(default)]
    expired_at: Option<i64>,
    // Only asserted, never used to pick an algorithm — this module already
    // hard-requires ES256 (see `verify_plaid_webhook`). Checking it explicitly
    // just turns "a non-P-256 JWK silently produces a bogus DecodingKey" into
    // a clear rejection instead.
    #[serde(default)]
    crv: Option<String>,
}

/// Process-wide JWK cache keyed by `kid`, mirroring gocardless.rs's cached-
/// bearer-token precedent — Plaid states these keys rotate infrequently and
/// recommends caching for up to 24h; this cache has no TTL eviction (a v1
/// simplification: a compromised/rotated key would require a process
/// restart to pick up, same operational bar as this codebase's other
/// process-wide caches). A JWK with `expired_at` set is NEVER inserted here
/// (see `fetch_jwk_fresh`), so a cache hit only ever needs to re-check
/// `expired_at` defensively, not as its primary enforcement point.
static JWK_CACHE: std::sync::OnceLock<RwLock<HashMap<String, PlaidJwk>>> = std::sync::OnceLock::new();
fn jwk_cache() -> &'static RwLock<HashMap<String, PlaidJwk>> {
    JWK_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// How long a failed `kid` lookup is remembered before `fetch_jwk` will try
/// Plaid again for that exact `kid`. `decode_header` runs before any
/// signature check, so `kid` is fully attacker-controlled; without this, an
/// unauthenticated caller could force one outbound Plaid API call per request
/// forever by sending garbage `kid`s in `Plaid-Verification`, risking Plaid's
/// account-level rate limit. This is a defense-in-depth bound, not a full
/// rate limiter — a fixed short TTL per distinct failed `kid` is enough to
/// stop the unbounded-retry amplification loop.
const FAILED_KID_CACHE_TTL: Duration = Duration::from_secs(60);
/// Hard backstop against a burst of many *distinct* attacker-supplied `kid`s
/// arriving within a single `FAILED_KID_CACHE_TTL` window (the sweep in
/// `record_failed_kid` only removes entries once they've expired, so it can't
/// bound size within one still-live window on its own). 10,000 distinct
/// failed kids in 60s is far beyond legitimate traffic for this endpoint.
const FAILED_KID_CACHE_MAX_ENTRIES: usize = 10_000;
static FAILED_KID_CACHE: std::sync::OnceLock<RwLock<HashMap<String, Instant>>> = std::sync::OnceLock::new();
fn failed_kid_cache() -> &'static RwLock<HashMap<String, Instant>> {
    FAILED_KID_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record a failed `kid` lookup. `kid` is read straight from an unverified
/// JWT header before any signature check, so it is fully attacker-controlled
/// — an unauthenticated caller can send unlimited distinct garbage `kid`s to
/// the public webhook endpoint. Sweeping already-expired entries out of the
/// map on every insert (under the same write lock, so it's a single cheap
/// O(n) pass rather than a separate background task) keeps the map's
/// steady-state size bounded by "distinct kids that failed within the last
/// TTL window" instead of growing without bound for the life of the process.
/// The size cap below is a backstop for a burst of distinct kids within one
/// still-live TTL window, which the sweep alone can't bound.
async fn record_failed_kid(kid: &str) {
    let mut cache = failed_kid_cache().write().await;
    let now = Instant::now();
    cache.retain(|_, failed_at| now.duration_since(*failed_at) < FAILED_KID_CACHE_TTL);
    if cache.len() >= FAILED_KID_CACHE_MAX_ENTRIES {
        let overflow = cache.len() + 1 - FAILED_KID_CACHE_MAX_ENTRIES;
        let mut by_age: Vec<(String, Instant)> = cache.iter().map(|(k, v)| (k.clone(), *v)).collect();
        by_age.sort_by_key(|(_, failed_at)| *failed_at);
        for (oldest_kid, _) in by_age.into_iter().take(overflow) {
            cache.remove(&oldest_kid);
        }
    }
    cache.insert(kid.to_string(), now);
}

/// Test-only seam: insert a failed-kid entry with an arbitrary `Instant`
/// directly, bypassing `record_failed_kid`'s sweep. Lets tests plant
/// already-expired entries (an `Instant` older than `FAILED_KID_CACHE_TTL`)
/// without sleeping in real time, then assert `record_failed_kid` sweeps
/// them out on the next insert.
#[cfg(test)]
async fn insert_failed_kid_at(kid: &str, failed_at: Instant) {
    failed_kid_cache().write().await.insert(kid.to_string(), failed_at);
}

async fn fetch_jwk(kid: &str) -> Result<PlaidJwk, (StatusCode, String)> {
    if let Some(failed_at) = failed_kid_cache().read().await.get(kid).copied() {
        if failed_at.elapsed() < FAILED_KID_CACHE_TTL {
            return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
        }
    }

    if let Some(jwk) = jwk_cache().read().await.get(kid) {
        if jwk.expired_at.is_none() {
            return Ok(jwk.clone());
        }
        // Defense in depth: a cached copy later found to be expired (e.g. a
        // pre-fix cache entry, or a future code path that ever caches before
        // checking) must not go on verifying signatures just because it's
        // already in the map.
        record_failed_kid(kid).await;
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }

    let result = fetch_jwk_fresh(kid).await;
    if result.is_err() {
        record_failed_kid(kid).await;
    }
    result
}

async fn fetch_jwk_fresh(kid: &str) -> Result<PlaidJwk, (StatusCode, String)> {
    let resp = plaid_post("webhook_verification_key/get", serde_json::json!({ "key_id": kid })).await?;
    let key = resp.get("key").ok_or_else(|| internal_error("plaid webhook_verification_key/get missing key"))?;
    let jwk: PlaidJwk = serde_json::from_value(key.clone())
        .map_err(|e| internal_error(format!("plaid JWK decode: {e}")))?;
    if jwk.expired_at.is_some() {
        tracing::warn!(%kid, "plaid webhook_verification_key/get returned an expired/revoked key; rejecting");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    if let Some(crv) = &jwk.crv {
        if crv != "P-256" {
            tracing::warn!(%kid, %crv, "plaid JWK has an unexpected curve; rejecting");
            return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
        }
    }
    jwk_cache().write().await.insert(kid.to_string(), jwk.clone());
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
    // Every rejection branch below logs a `tracing::warn!` naming the failure
    // CATEGORY (never the token/signature itself) before returning the same
    // generic 400 to the caller (Plaid) — this is the single most
    // security-critical verification path in this module, and previously
    // failed completely silently on the server side, making a real
    // misconfiguration (secret rotation, clock skew, Plaid key rotation)
    // indistinguishable in the logs from routine attacker probing (code
    // review finding).
    let token = headers.get("plaid-verification").and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            tracing::warn!("plaid webhook rejected: missing Plaid-Verification header");
            (StatusCode::BAD_REQUEST, "Missing Plaid-Verification header".to_string())
        })?;
    let header = decode_header(token).map_err(|e| {
        tracing::warn!(error = %e, "plaid webhook rejected: malformed JWT header");
        (StatusCode::BAD_REQUEST, "Invalid signature".to_string())
    })?;
    if header.alg != Algorithm::ES256 {
        tracing::warn!(alg = ?header.alg, "plaid webhook rejected: unexpected JWT algorithm");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let kid = header.kid.ok_or_else(|| {
        tracing::warn!("plaid webhook rejected: JWT header missing kid");
        (StatusCode::BAD_REQUEST, "Invalid signature".to_string())
    })?;
    let jwk = fetch_jwk(&kid).await?;
    let decoding_key = DecodingKey::from_ec_components(&jwk.x, &jwk.y)
        .map_err(|_| internal_error("plaid JWK -> DecodingKey conversion failed"))?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.validate_exp = false; // Plaid webhook JWTs carry iat, not exp — freshness checked via iat below
    validation.required_spec_claims.clear();
    let data = decode::<PlaidWebhookClaims>(token, &decoding_key, &validation)
        .map_err(|e| {
            tracing::warn!(kid = %kid, error = %e, "plaid webhook rejected: JWT signature verification failed");
            (StatusCode::BAD_REQUEST, "Invalid signature".to_string())
        })?;

    let now = chrono::Utc::now().timestamp();
    if !iat_within_tolerance(data.claims.iat, now, 300) {
        tracing::warn!(iat = data.claims.iat, now, "plaid webhook rejected: iat outside tolerance");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    if !body_hash_matches(body, &data.claims.request_body_sha256) {
        tracing::warn!("plaid webhook rejected: body hash does not match JWT claim");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    Ok(())
}

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
            // Fetched WITHOUT the `status = 'active'` filter (unlike the
            // original version of this query) so an empty result here means
            // "no linked_accounts row at all for this item_id" — distinct
            // from "row(s) exist but are disconnected" — letting the
            // unknown-item_id case below be logged precisely, mirroring
            // financial_connections.rs's webhook handler logging
            // "financial_connections webhook for unknown account" for its
            // own analogous `None` case (spec section 6, code review #321).
            let rows = sqlx::query_as::<_, crate::db::LinkedAccount>(
                "SELECT * FROM linked_accounts WHERE plaid_item_id = $1 AND provider = 'plaid'")
                .bind(&payload.item_id)
                .fetch_all(&state.db).await.map_err(internal_error)?;
            if rows.is_empty() {
                tracing::warn!(item_id = %payload.item_id, "plaid webhook for unknown item_id");
            }
            // Batch the subscription-status lookup by distinct user_id rather
            // than issuing one `SELECT status FROM subscriptions` per row —
            // an Item's rows all share one `plaid_item_id`, but a multi-
            // account Item can still produce several rows per user_id (and
            // the count grows with however many accounts the user linked
            // under this Item), so a per-row query was an avoidable N+1
            // (code review finding on nels#321).
            let active_user_ids: Vec<Uuid> = rows.iter()
                .filter(|r| r.status == "active")
                .map(|r| r.user_id)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let owner_statuses: HashMap<Uuid, Option<String>> = if active_user_ids.is_empty() {
                HashMap::new()
            } else {
                sqlx::query_as::<_, (Uuid, Option<String>)>(
                    "SELECT user_id, status FROM subscriptions WHERE user_id = ANY($1)")
                    .bind(&active_user_ids)
                    .fetch_all(&state.db).await.map_err(internal_error)?
                    .into_iter()
                    .collect()
            };
            for row in rows {
                if row.status != "active" {
                    continue;
                }
                let owner_status = owner_statuses.get(&row.user_id).cloned().flatten();
                if user_is_pro(owner_status.as_deref()) {
                    if let Err((status, msg)) = sync_account_transactions(&state.db, &state.cipher, &row).await {
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
            // Existence checked regardless of `is_login_required` so an
            // unknown item_id is logged even for ITEM/ERROR codes this
            // handler otherwise ignores (spec section 6: "Webhook for an
            // unknown item_id ... logged at warn, 200").
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM linked_accounts WHERE plaid_item_id = $1)")
                .bind(&payload.item_id)
                .fetch_one(&state.db).await.map_err(internal_error)?;
            if !exists {
                tracing::warn!(item_id = %payload.item_id, "plaid webhook for unknown item_id");
            } else if is_login_required {
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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use sqlx::postgres::PgPoolOptions;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    // --- verify_plaid_webhook: real ES256 JWT/JWK cryptographic path -------
    //
    // Everything above only tests the pure helpers in isolation. These tests
    // generate a REAL P-256 keypair, sign a REAL JWT with it, mock Plaid's
    // `/webhook_verification_key/get` to return the matching JWK, and drive
    // `verify_plaid_webhook`/`fetch_jwk` through their actual ES256 signature
    // verification / JWK accept-reject decision — the module's most
    // security-critical code, and until now its least tested.

    /// Generate a fresh P-256 ECDSA keypair via `ring` (the same crypto
    /// backend `jsonwebtoken`'s own ES256 encode/decode path uses) and
    /// return a jsonwebtoken `EncodingKey` alongside the raw (x, y)
    /// public-key coordinates already base64url-encoded with no padding —
    /// the exact shape of the JWK Plaid's `/webhook_verification_key/get`
    /// returns and `PlaidJwk`/`DecodingKey::from_ec_components` expect.
    fn generate_es256_keypair() -> (jsonwebtoken::EncodingKey, String, String) {
        use base64::Engine as _;
        use ring::rand::SystemRandom;
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .expect("generate P-256 keypair");
        let keypair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .expect("load generated P-256 keypair");
        // Uncompressed SEC1 point: 0x04 || X (32 bytes) || Y (32 bytes).
        let public = keypair.public_key().as_ref();
        assert_eq!(public.len(), 65, "P-256 uncompressed point must be 65 bytes");
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[1..33]);
        let y = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[33..65]);
        let encoding_key = jsonwebtoken::EncodingKey::from_ec_der(pkcs8.as_ref());
        (encoding_key, x, y)
    }

    /// Sign a Plaid-shaped webhook JWT (`kid` in the header, `iat` +
    /// `request_body_sha256` as claims — mirrors `PlaidWebhookClaims`) with a
    /// real ES256 `EncodingKey`.
    fn sign_plaid_webhook_jwt(encoding_key: &jsonwebtoken::EncodingKey, kid: &str, iat: i64, body: &[u8]) -> String {
        let request_body_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(body);
            hex::encode(hasher.finalize())
        };
        let mut header = jsonwebtoken::Header::new(Algorithm::ES256);
        header.kid = Some(kid.to_string());
        let claims = serde_json::json!({ "iat": iat, "request_body_sha256": request_body_sha256 });
        jsonwebtoken::encode(&header, &claims, encoding_key).expect("sign test JWT")
    }

    /// Mount Plaid's `/webhook_verification_key/get` to return a JWK for
    /// `kid` built from a real public key's (x, y) — `expired_at: None`
    /// mirrors an active, unrevoked key; `Some(ts)` mirrors one Plaid has
    /// revoked/rotated.
    async fn mount_plaid_jwk(server: &MockServer, kid: &str, x: &str, y: &str, expired_at: Option<i64>) {
        Mock::given(method("POST")).and(path("/webhook_verification_key/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": {
                    "kid": kid, "kty": "EC", "crv": "P-256", "x": x, "y": y,
                    "use": "sig", "alg": "ES256", "expired_at": expired_at,
                }
            })))
            .expect(1)
            .mount(server).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn verify_plaid_webhook_accepts_a_genuinely_signed_jwt() {
        let (encoding_key, x, y) = generate_es256_keypair();
        let kid = "kid-accept-1";
        let body = br#"{"webhook_type":"TRANSACTIONS","webhook_code":"DEFAULT_UPDATE","item_id":"item-accept-1"}"#.to_vec();
        let now = chrono::Utc::now().timestamp();
        let token = sign_plaid_webhook_jwt(&encoding_key, kid, now, &body);

        let server = MockServer::start().await;
        set_plaid_env(&server);
        mount_plaid_jwk(&server, kid, &x, &y, None).await;

        let mut headers = HeaderMap::new();
        headers.insert("plaid-verification", token.parse().expect("token is a valid header value"));

        let result = verify_plaid_webhook(&headers, &Bytes::from(body)).await;
        assert!(result.is_ok(), "a genuinely ES256-signed JWT over the exact body must verify: {result:?}");

        clear_plaid_env();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn verify_plaid_webhook_rejects_a_tampered_body_against_a_valid_signature() {
        let (encoding_key, x, y) = generate_es256_keypair();
        let kid = "kid-tamper-1";
        let original_body = br#"{"webhook_type":"TRANSACTIONS","webhook_code":"DEFAULT_UPDATE","item_id":"item-tamper-1"}"#.to_vec();
        let now = chrono::Utc::now().timestamp();
        // Signed over `original_body`'s hash — the JWT and its signature are
        // both genuinely valid.
        let token = sign_plaid_webhook_jwt(&encoding_key, kid, now, &original_body);

        let server = MockServer::start().await;
        set_plaid_env(&server);
        mount_plaid_jwk(&server, kid, &x, &y, None).await;

        let mut headers = HeaderMap::new();
        headers.insert("plaid-verification", token.parse().expect("token is a valid header value"));

        // ...but the request that actually arrived carries a DIFFERENT body.
        // This exercises the body-hash claim check specifically (the most
        // security-critical negative case) against a real, otherwise-valid
        // signature — not merely a broken/forged JWT.
        let tampered_body = br#"{"webhook_type":"TRANSACTIONS","webhook_code":"DEFAULT_UPDATE","item_id":"item-ATTACKER"}"#.to_vec();
        let result = verify_plaid_webhook(&headers, &Bytes::from(tampered_body)).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST, "a tampered body must be rejected even though the JWT's own signature is genuinely valid");

        clear_plaid_env();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_jwk_rejects_and_does_not_cache_an_expired_key() {
        let (_encoding_key, x, y) = generate_es256_keypair();
        let kid = "kid-expired-1";
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // expired_at set (non-null) -> Plaid has revoked/rotated this key.
        mount_plaid_jwk(&server, kid, &x, &y, Some(1_700_000_000)).await;

        let result = fetch_jwk(kid).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST, "a JWK with expired_at set must be rejected");
        assert!(
            jwk_cache().read().await.get(kid).is_none(),
            "an expired JWK must never be inserted into JWK_CACHE, even transiently"
        );

        clear_plaid_env();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_jwk_short_circuits_repeated_lookups_of_a_previously_failed_kid() {
        let kid = "kid-negcache-1";
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // Plaid genuinely has no such key (e.g. an attacker-forged kid in an
        // unauthenticated `Plaid-Verification` header) — respond with a
        // failure. `.expect(1)` fails this test on server drop if `fetch_jwk`
        // ever calls Plaid a second time for this exact kid.
        Mock::given(method("POST")).and(path("/webhook_verification_key/get"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error_code": "INVALID_KID"})))
            .expect(1)
            .mount(&server).await;

        let first = fetch_jwk(kid).await;
        assert!(first.is_err(), "an unknown/attacker-forged kid must fail");

        // Same kid, well within FAILED_KID_CACHE_TTL (60s) — must short-
        // circuit via the negative cache instead of calling Plaid again,
        // bounding the outbound-call amplification an unauthenticated caller
        // could otherwise trigger with unlimited garbage kids.
        let second = fetch_jwk(kid).await;
        assert!(second.is_err(), "a kid that failed moments ago must still fail without a second Plaid call");

        clear_plaid_env();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn record_failed_kid_sweeps_expired_entries_instead_of_growing_forever() {
        let stale_1 = "kid-sweep-stale-1";
        let stale_2 = "kid-sweep-stale-2";
        let still_live = "kid-sweep-live-1";
        let newly_failed = "kid-sweep-new-1";

        // Plant entries whose TTL has already lapsed (well past
        // FAILED_KID_CACHE_TTL) plus one still-live entry, all without
        // sleeping in real time — `insert_failed_kid_at` is a test-only seam
        // that bypasses `record_failed_kid`'s sweep so we can control the
        // recorded `Instant` directly.
        let long_expired = Instant::now().checked_sub(FAILED_KID_CACHE_TTL + Duration::from_secs(1))
            .expect("test process must have been running long enough to backdate an Instant");
        insert_failed_kid_at(stale_1, long_expired).await;
        insert_failed_kid_at(stale_2, long_expired).await;
        insert_failed_kid_at(still_live, Instant::now()).await;

        // Recording a brand-new failed kid must sweep the already-expired
        // entries as a side effect of the insert (the fix under test) —
        // previously entries were only ever consulted for TTL expiry, never
        // removed, so this cache grew without bound for the life of the
        // process under a flood of distinct attacker-supplied kids.
        record_failed_kid(newly_failed).await;

        let cache = failed_kid_cache().read().await;
        assert!(cache.get(stale_1).is_none(), "an expired failed-kid entry must be swept on the next insert");
        assert!(cache.get(stale_2).is_none(), "an expired failed-kid entry must be swept on the next insert");
        assert!(cache.get(still_live).is_some(), "a still-live failed-kid entry must not be swept early");
        assert!(cache.get(newly_failed).is_some(), "the newly recorded kid must be present after the insert");
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn mk_user_and_budget(db: &PgPool) -> (Uuid, Uuid) {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("plaid-{uid}@test.example")).execute(db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(db).await.unwrap();
        (uid, bid)
    }

    /// Mirrors `gocardless.rs`'s own `mk_pro_subscription` helper exactly
    /// (same signature) — every test below the Pro-gate itself needs the
    /// budget OWNER to resolve to Tier::Pro, or `require_tier` 402s before
    /// the anti-replay check under test ever runs. 'trialing' (not 'active')
    /// so `entitlement::resolve` maps to Tier::Pro unconditionally, with no
    /// price_id/STRIPE_PRICE_* env dependency.
    async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
            .bind(user_id).bind(customer_id).execute(db).await.unwrap();
    }

    /// Mirrors `akahu.rs`'s own `test_cipher` helper exactly (same key) —
    /// `provider_ref` is encrypted at rest (code review fix, #321), so every
    /// test that exercises a code path touching it (encrypt on
    /// link/complete_link_session, decrypt on sync/refresh/disconnect) needs
    /// this same cipher instance.
    fn test_cipher() -> crate::crypto::SecretCipher {
        crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()
    }

    /// Mirrors `gocardless.rs`'s own `set_gc_env`/`clear_gc_env` pair exactly
    /// (same purpose: point this module's HTTP client at a `wiremock`
    /// `MockServer` instead of the real Plaid API, and supply the two env
    /// vars `plaid_post` requires to not 503 as "not configured").
    fn set_plaid_env(server: &MockServer) {
        std::env::set_var("PLAID_API_BASE", server.uri());
        std::env::set_var("PLAID_CLIENT_ID", "client_test");
        std::env::set_var("PLAID_SECRET", "secret_test");
    }
    fn clear_plaid_env() {
        std::env::remove_var("PLAID_API_BASE");
        std::env::remove_var("PLAID_CLIENT_ID");
        std::env::remove_var("PLAID_SECRET");
    }

    /// Mount the three Plaid endpoints `complete_link_session` calls on a
    /// successful path: `item/public_token/exchange`, `accounts/get`, and
    /// `institutions/get_by_id`. `access_token`/`item_id` are parameterized so
    /// re-link tests can assert the ON CONFLICT refresh picks up a NEW pair.
    async fn mount_successful_exchange(server: &MockServer, access_token: &str, item_id: &str, account_id: &str) {
        Mock::given(method("POST")).and(path("/item/public_token/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": access_token, "item_id": item_id,
            })))
            .mount(server).await;
        Mock::given(method("POST")).and(path("/accounts/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accounts": [{"account_id": account_id, "name": "Chequing", "mask": "1234"}],
                "item": {"institution_id": "ins_1"},
            })))
            .mount(server).await;
        Mock::given(method("POST")).and(path("/institutions/get_by_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "institution": {"institution_id": "ins_1", "name": "Test Bank"},
            })))
            .mount(server).await;
    }

    /// Insert a `pending` `plaid_link_sessions` row directly (bypassing
    /// `create_link_token`'s HTTP call) — used by tests that only need to
    /// exercise `complete_link_session`.
    async fn mk_pending_session(db: &PgPool, budget_id: Uuid, user_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO plaid_link_sessions (id, budget_id, user_id) VALUES ($1, $2, $3)")
            .bind(id).bind(budget_id).bind(user_id)
            .execute(db).await.unwrap();
        id
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

        let result = complete_link_session(&db, &test_cipher(), uid, bid, session_id, "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);

        // Test-hygiene fix: this test previously left its two user rows
        // behind, causing a duplicate-key failure on `subscriptions`'s
        // `stripe_customer_id` unique index on a second run against a
        // persistent test DB (unlike every sibling test in this file).
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(_other_uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_unknown_session_id() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_p2").await;
        let result = complete_link_session(&db, &test_cipher(), uid, bid, Uuid::new_v4(), "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
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

        // Exercises the new atomic-claim code path: the `UPDATE ... WHERE id =
        // $1 AND status = 'pending'` affects 0 rows because this row is
        // already 'completed', so `complete_link_session` must 409 without
        // ever attempting a Plaid call (no PLAID_CLIENT_ID/PLAID_SECRET are
        // set in this test process, so reaching `plaid_post` would surface as
        // a 503 instead — confirming the atomic claim short-circuits first).
        let result = complete_link_session(&db, &test_cipher(), uid, bid, session_id, "public-sandbox-token").await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_second_call_on_same_session_loses_the_race() {
        // Simulates the concurrent-request scenario the TOCTOU fix targets:
        // two requests racing to complete the SAME session_id. The first call
        // wins the atomic `UPDATE ... WHERE status = 'pending'` claim and
        // proceeds to (successfully, via wiremock) exchange/insert. The
        // second call, for the identical session_id, must find the claim
        // UPDATE affects 0 rows (status is already 'completed') and 409
        // WITHOUT calling Plaid again — asserted by mounting each Plaid mock
        // exactly once (`expect(1)`), which would panic on drop if a second
        // exchange attempt were made.
        let server = MockServer::start().await;
        set_plaid_env(&server);
        Mock::given(method("POST")).and(path("/item/public_token/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-race", "item_id": "item-race",
            })))
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/accounts/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accounts": [{"account_id": "acct-race", "name": "Chequing", "mask": "1234"}],
                "item": {"institution_id": "ins_1"},
            })))
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/institutions/get_by_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "institution": {"institution_id": "ins_1", "name": "Test Bank"},
            })))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_race").await;
        let session_id = mk_pending_session(&db, bid, uid).await;

        let first = complete_link_session(&db, &test_cipher(), uid, bid, session_id, "public-sandbox-token").await;
        assert!(first.is_ok(), "first request should win the claim and complete: {first:?}");

        let second = complete_link_session(&db, &test_cipher(), uid, bid, session_id, "public-sandbox-token").await;
        assert_eq!(second.unwrap_err().0, StatusCode::CONFLICT, "second request must lose the race and 409");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_relink_refreshes_provider_ref_and_item_id() {
        // Reproduces the "Critical" finding directly: disconnect + relink of
        // the SAME provider_account_id must overwrite the stale/possibly-
        // revoked `provider_ref` (access_token) and `plaid_item_id` with the
        // freshly exchanged ones, not silently keep the old row's values via
        // the ON CONFLICT ... DO UPDATE clause.
        // Two separate MockServers (one per exchange) rather than two mocks
        // on one server: wiremock matches the FIRST-registered mock for a
        // given path/method when priorities tie, so a second `Mock::given`
        // for the same path on the same server would never be reached — a
        // fresh server per exchange is the simplest way to give each call a
        // distinct canned access_token/item_id response.
        let server_1 = MockServer::start().await;
        set_plaid_env(&server_1);
        mount_successful_exchange(&server_1, "access-OLD", "item-OLD", "acct-relink").await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_relink").await;

        // First link: establishes the row with the "OLD" credentials.
        let session_1 = mk_pending_session(&db, bid, uid).await;
        let first = complete_link_session(&db, &test_cipher(), uid, bid, session_1, "public-token-1").await.expect("first link ok");
        assert_eq!(first.accounts.len(), 1);
        let account_id = first.accounts[0].id;

        let stored: crate::db::LinkedAccount = sqlx::query_as("SELECT * FROM linked_accounts WHERE id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        // provider_ref is encrypted at rest (code review fix, #321) — decrypt
        // before asserting on the underlying access_token value.
        assert_eq!(test_cipher().decrypt(&stored.provider_ref).unwrap(), "access-OLD");
        assert_eq!(stored.plaid_item_id.as_deref(), Some("item-OLD"));

        // Simulate a disconnect (mirrors the real disconnect flow — flips
        // status, does NOT delete the row) followed by a re-link that gets a
        // FRESH access_token/item_id from Plaid, exactly as the finding
        // describes.
        sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now() WHERE id = $1")
            .bind(account_id).execute(&db).await.unwrap();

        // Same provider_account_id ("acct-relink") but a brand NEW
        // access_token/item_id pair, as Plaid issues on every fresh exchange.
        let server_2 = MockServer::start().await;
        set_plaid_env(&server_2);
        mount_successful_exchange(&server_2, "access-NEW", "item-NEW", "acct-relink").await;
        let session_2 = mk_pending_session(&db, bid, uid).await;
        let second = complete_link_session(&db, &test_cipher(), uid, bid, session_2, "public-token-2").await.expect("relink ok");
        assert_eq!(second.accounts.len(), 1);
        assert_eq!(second.accounts[0].id, account_id, "relink must reconcile onto the SAME row, not insert a duplicate");
        assert_eq!(second.accounts[0].status, "active", "relink must reactivate the row");

        let refreshed: crate::db::LinkedAccount = sqlx::query_as("SELECT * FROM linked_accounts WHERE id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        assert_eq!(test_cipher().decrypt(&refreshed.provider_ref).unwrap(), "access-NEW", "ON CONFLICT must refresh provider_ref to the new access_token");
        assert_eq!(refreshed.plaid_item_id.as_deref(), Some("item-NEW"), "ON CONFLICT must refresh plaid_item_id to the new item_id");
        assert!(refreshed.disconnected_at.is_none());

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

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

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_linked_account_short_circuits_on_consent_expired_with_zero_plaid_calls() {
        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        // 'trialing' (not 'active') — under the new owner-derived `require_tier`
        // gate, 'active' with no price_id/STRIPE_PRICE_* env fails safe to
        // Basic (402) before this test ever reaches the consent_expired
        // short-circuit it means to exercise.
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, 'cus_p', 'trialing')")
            .bind(uid).execute(&db).await.unwrap();
        let account_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_p1', 'access-p1', 'consent_expired')")
            .bind(account_id).bind(bid).bind(uid).execute(&db).await.unwrap();

        // No PLAID_CLIENT_ID/PLAID_SECRET configured in this test process —
        // if refresh_linked_account made a real Plaid call here it would 503
        // (missing config), not the 409 asserted below. Getting 409 proves
        // the consent_expired short-circuit fired before any Plaid call —
        // and, notably, before the cipher ever needs to decrypt this row's
        // deliberately-plaintext-for-this-test provider_ref.
        let result = refresh_linked_account(&db, &test_cipher(), uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_flips_every_sibling_row_sharing_the_item_id() {
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
        // provider_ref is encrypted at rest (code review fix, #321) — this
        // test's disconnect call actually reaches Plaid's /item/remove (unlike
        // the consent_expired short-circuit tests above), so the stored value
        // must be genuinely decryptable with test_cipher(), not plaintext.
        let encrypted_shared = test_cipher().encrypt("access-shared").unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_a', $4, $5, 'active')")
            .bind(acct_a).bind(bid).bind(uid).bind(&encrypted_shared).bind(item_id).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_b', $4, $5, 'active')")
            .bind(acct_b).bind(bid).bind(uid).bind(&encrypted_shared).bind(item_id).execute(&db).await.unwrap();

        disconnect_linked_account(&db, &test_cipher(), uid, bid, acct_a).await.unwrap();

        let status_b: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(acct_b).fetch_one(&db).await.unwrap();
        assert_eq!(status_b, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        std::env::remove_var("PLAID_API_BASE");
        std::env::remove_var("PLAID_CLIENT_ID");
        std::env::remove_var("PLAID_SECRET");
    }

    // --- sync_account_transactions: pagination, the no-progress safety
    // valve, and `removed` handling -----------------------------------
    //
    // The single most complex piece of new logic in this task was previously
    // unexercised by any test. These three close that gap.

    /// Insert a `linked_accounts` row directly (bypassing `complete_link_session`)
    /// and fetch it back, for tests that only need to exercise
    /// `sync_account_transactions` against an already-linked account.
    /// `access_token` is encrypted with `test_cipher()` before being stored,
    /// matching how `complete_link_session` really persists it (code review
    /// fix, #321) — every caller of this helper must decrypt with the SAME
    /// `test_cipher()` to get back the plaintext value it passed in.
    async fn mk_plaid_linked_account(
        db: &PgPool,
        budget_id: Uuid,
        user_id: Uuid,
        provider_account_id: &str,
        access_token: &str,
    ) -> crate::db::LinkedAccount {
        let id = Uuid::new_v4();
        let encrypted_token = test_cipher().encrypt(access_token).unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'plaid', $4, $5, 'active')")
            .bind(id).bind(budget_id).bind(user_id).bind(provider_account_id).bind(&encrypted_token)
            .execute(db).await.unwrap();
        sqlx::query_as::<_, crate::db::LinkedAccount>("SELECT * FROM linked_accounts WHERE id = $1")
            .bind(id).fetch_one(db).await.unwrap()
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_pages_across_multiple_calls_and_advances_cursor() {
        // Two mocks on the SAME path: wiremock matches in priority order (tie
        // broken by insertion order) and a mock stops matching once its
        // `up_to_n_times` cap is hit, so request 1 is served by `page_1` and
        // request 2 falls through to `page_2` — see wiremock 0.6's
        // `MountedMock::matches`.
        let server = MockServer::start().await;
        set_plaid_env(&server);
        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [{
                    "transaction_id": "tx-page1", "account_id": "acct-paged",
                    "amount": 12.5, "name": "Coffee", "date": "2026-07-01",
                }],
                "modified": [], "removed": [],
                "has_more": true, "next_cursor": "cursor-page-1",
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [{
                    "transaction_id": "tx-page2", "account_id": "acct-paged",
                    "amount": 30.0, "name": "Groceries", "date": "2026-07-02",
                }],
                "modified": [], "removed": [],
                "has_more": false, "next_cursor": "cursor-page-2",
            })))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let row = mk_plaid_linked_account(&db, bid, uid, "acct-paged", "access-paged").await;

        let imported = sync_account_transactions(&db, &test_cipher(), &row).await.expect("sync should succeed");
        assert_eq!(imported, 2, "both pages' transactions must be imported");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE external_account_id = $1")
            .bind(row.id).fetch_one(&db).await.unwrap();
        assert_eq!(count, 2);

        let cursor: Option<String> = sqlx::query_scalar("SELECT plaid_cursor FROM linked_accounts WHERE id = $1")
            .bind(row.id).fetch_one(&db).await.unwrap();
        assert_eq!(cursor.as_deref(), Some("cursor-page-2"), "cursor must advance to the LAST page's next_cursor");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_aborts_instead_of_looping_when_has_more_but_no_next_cursor() {
        // Plaid claiming `has_more: true` with no `next_cursor` to advance by
        // is exactly the malformed-response shape that used to hang this
        // function forever (`cursor = ...or(cursor)` silently kept the OLD
        // cursor, so the identical request repeated indefinitely). This must
        // now abort on the very first page rather than needing to exhaust the
        // MAX_SYNC_PAGES cap — cheaper and faster to verify, and exercises the
        // no-progress guard specifically rather than the iteration cap.
        let server = MockServer::start().await;
        set_plaid_env(&server);
        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [], "modified": [], "removed": [],
                "has_more": true,
                // no "next_cursor" field at all
            })))
            .expect(1) // must NOT be called a second time — the guard must fire after page 1
            .mount(&server).await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let row = mk_plaid_linked_account(&db, bid, uid, "acct-stuck", "access-stuck").await;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sync_account_transactions(&db, &test_cipher(), &row),
        ).await.expect("sync_account_transactions must return promptly, not hang");
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_GATEWAY);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_deletes_a_transaction_plaid_reports_removed() {
        let server = MockServer::start().await;
        set_plaid_env(&server);

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let row = mk_plaid_linked_account(&db, bid, uid, "acct-removed", "access-removed").await;

        // A transaction previously imported by an earlier sync.
        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, amount, transaction_date, description, external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, 10.0, now(), 'Old pending charge', $3, 'tx-to-remove', 'CAD')")
            .bind(tx_id).bind(bid).bind(row.id)
            .execute(&db).await.unwrap();

        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [], "modified": [],
                "removed": [{"transaction_id": "tx-to-remove", "account_id": "acct-removed"}],
                "has_more": false, "next_cursor": "cursor-removed-1",
            })))
            .expect(1)
            .mount(&server).await;

        let imported = sync_account_transactions(&db, &test_cipher(), &row).await.expect("sync should succeed");
        assert_eq!(imported, 0, "a removal is not an import");

        let still_present: Option<Uuid> = sqlx::query_scalar("SELECT id FROM transactions WHERE id = $1")
            .bind(tx_id).fetch_optional(&db).await.unwrap();
        assert!(still_present.is_none(), "the removed transaction must be deleted");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_applies_a_modified_transactions_correction() {
        // Code review finding (#321): `added` and `modified` used to share the
        // same `ON CONFLICT ... DO NOTHING` insert, so a `modified` entry whose
        // provider_transaction_id already existed was silently discarded —
        // Plaid's correction (e.g. a pending amount becoming its final posted
        // amount) never reached the ledger. This proves the fix: a `modified`
        // entry for an already-imported row updates amount/description/
        // transaction_date, while a `modified` entry for a row the user has
        // NOT yet re-categorized still gets the guessed category_id from
        // insert (not overwritten on the update path).
        let server = MockServer::start().await;
        set_plaid_env(&server);

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        let row = mk_plaid_linked_account(&db, bid, uid, "acct-modified", "access-modified").await;

        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, amount, transaction_date, description, external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, 12.34, '2026-07-01', 'Pending charge', $3, 'tx-to-modify', 'CAD')")
            .bind(tx_id).bind(bid).bind(row.id)
            .execute(&db).await.unwrap();

        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [],
                "modified": [{
                    "transaction_id": "tx-to-modify",
                    "account_id": "acct-modified",
                    "amount": 15.67,
                    "name": "Posted charge (corrected)",
                    "date": "2026-07-02",
                }],
                "removed": [], "has_more": false, "next_cursor": "cursor-modified-1",
            })))
            .expect(1)
            .mount(&server).await;

        sync_account_transactions(&db, &test_cipher(), &row).await.expect("sync should succeed");

        let (amount, description, transaction_date): (f64, String, chrono::NaiveDate) = sqlx::query_as(
            "SELECT amount, description, transaction_date::date FROM transactions WHERE id = $1")
            .bind(tx_id).fetch_one(&db).await.unwrap();
        assert_eq!(amount, 15.67, "modified amount must be applied, not silently dropped");
        assert_eq!(description, "Posted charge (corrected)");
        assert_eq!(transaction_date, chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap());

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

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

    // --- webhook(): real axum-handler end-to-end dispatch (nels#321) -------
    //
    // Everything above either exercises verify_plaid_webhook/fetch_jwk in
    // isolation or drives the dispatch SQL directly against a hand-built
    // payload. Neither actually calls the real `webhook` axum handler
    // function with a genuinely signed JWT, so a wiring mistake between
    // "JWT verifies" and "the right linked_accounts row gets synced" could
    // slip through undetected. This test closes that gap for the
    // TRANSACTIONS/SYNC_UPDATES_AVAILABLE path (code review, #321).

    fn test_state(db: PgPool) -> AppState {
        AppState {
            db,
            // Same key as test_cipher() (used everywhere else in this
            // module's tests) so a row encrypted via one is decryptable via
            // the other.
            cipher: std::sync::Arc::new(test_cipher()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_end_to_end_dispatches_transactions_sync_for_matching_item_id() {
        let (encoding_key, x, y) = generate_es256_keypair();
        let kid = "kid-e2e-sync-1";
        let item_id = "item-e2e-sync-1";
        let body = serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "SYNC_UPDATES_AVAILABLE",
            "item_id": item_id,
        })).unwrap();
        let now = chrono::Utc::now().timestamp();
        let token = sign_plaid_webhook_jwt(&encoding_key, kid, now, &body);

        let server = MockServer::start().await;
        set_plaid_env(&server);
        mount_plaid_jwk(&server, kid, &x, &y, None).await;
        // `.expect(1)` proves the webhook actually dispatched into
        // sync_account_transactions for this row, not just that the JWT
        // verified.
        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [], "modified": [], "removed": [], "has_more": false, "next_cursor": "cursor-e2e-1",
            })))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let (uid, bid) = mk_user_and_budget(&db).await;
        mk_pro_subscription(&db, uid, "cus_wh_e2e_1").await;
        let account_id = Uuid::new_v4();
        // provider_ref is encrypted at rest (code review fix, #321) — test_state's
        // cipher is the same test_cipher() key, so this must be genuinely
        // decryptable, not plaintext, for sync_account_transactions to work.
        let encrypted_token = test_cipher().encrypt("access-e2e-sync-1").unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct-e2e-sync-1', $4, $5, 'active')")
            .bind(account_id).bind(bid).bind(uid).bind(&encrypted_token).bind(item_id)
            .execute(&db).await.unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("plaid-verification", token.parse().expect("token is a valid header value"));
        let state = test_state(db.clone());

        let status = webhook(State(state), headers, Bytes::from(body)).await
            .expect("a genuinely signed webhook for a known, active, Pro-owned item_id must succeed");
        assert_eq!(status, StatusCode::OK);
        // wiremock verifies `.expect(1)` on `server`'s drop below — proving
        // the real handler actually reached sync_account_transactions.

        let last_synced_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        assert!(last_synced_at.is_some(), "a successful sync must stamp last_synced_at");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_plaid_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_end_to_end_logs_and_200s_for_an_unknown_item_id() {
        // No linked_accounts row anywhere shares this item_id — the spec
        // (section 6) requires this to be logged at `warn` and still 200,
        // not silently swallowed or treated as an error.
        let (encoding_key, x, y) = generate_es256_keypair();
        let kid = "kid-e2e-unknown-1";
        let item_id = "item-e2e-does-not-exist";
        let body = serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "SYNC_UPDATES_AVAILABLE",
            "item_id": item_id,
        })).unwrap();
        let now = chrono::Utc::now().timestamp();
        let token = sign_plaid_webhook_jwt(&encoding_key, kid, now, &body);

        let server = MockServer::start().await;
        set_plaid_env(&server);
        mount_plaid_jwk(&server, kid, &x, &y, None).await;
        // No linked_accounts row matches, so sync_account_transactions must
        // never be reached — `.expect(0)` panics on drop if it is.
        Mock::given(method("POST")).and(path("/transactions/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added": [], "modified": [], "removed": [], "has_more": false,
            })))
            .expect(0)
            .mount(&server).await;

        let mut headers = HeaderMap::new();
        headers.insert("plaid-verification", token.parse().expect("token is a valid header value"));
        let db = test_pool().await;
        let state = test_state(db.clone());

        let status = webhook(State(state), headers, Bytes::from(body)).await
            .expect("an unknown item_id must still 200 (Plaid should not retry indefinitely)");
        assert_eq!(status, StatusCode::OK);

        clear_plaid_env();
    }
}
