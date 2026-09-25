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

#[derive(Debug, Serialize)]
pub struct BasiqLinkSessionResponse {
    pub redirect_url: String,
    pub reference: Uuid,
}

/// Start a Basiq consent flow: create (or genuinely REUSE) a Basiq user
/// scoped to this Nels user, mint a CLIENT_ACCESS token for that Basiq user,
/// persist a pending `bank_link_sessions` row, and build the hosted "Basiq
/// Connect" consent URL. Pro-gated, Edit-or-Owner-gated, closed-budget
/// rejected — mirrors `gocardless::start_link_session` exactly except there
/// is no institution_id/country argument (spec Assumption 2).
///
/// Reuse is backed by the dedicated `basiq_users` table (one row per Nels
/// `user_id`), NOT derived after the fact from `bank_link_sessions` — this
/// is what lets `disconnect_linked_account` reliably resolve the Basiq user
/// id for a connection made in an EARLIER call, even after this user has
/// since linked additional connections (code review fix, #323).
pub async fn create_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<BasiqLinkSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    let server_tok = server_token().await?;
    let existing_basiq_user_id: Option<String> = sqlx::query_scalar(
        "SELECT basiq_user_id FROM basiq_users WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(pool).await.map_err(internal_error)?;
    let basiq_user_id = match existing_basiq_user_id {
        Some(id) => id,
        None => {
            #[derive(Deserialize)]
            struct BasiqUser { id: String }
            let created: serde_json::Value = basiq_post_json("users", &server_tok, &serde_json::json!({
                "email": format!("{user_id}@nels.internal"),
            })).await?;
            let basiq_user: BasiqUser = serde_json::from_value(created)
                .map_err(|e| internal_error(format!("basiq user decode: {e}")))?;
            // ON CONFLICT ... DO UPDATE ... RETURNING (not DO NOTHING) so a
            // concurrent race between two first-time create_consent_session
            // calls for the SAME brand-new user_id can't return an orphaned
            // Basiq user id to the losing caller (code review fix, #323): the
            // DO UPDATE is a no-op write (sets the column to its own current
            // value) purely so RETURNING always reflects whichever row
            // actually persisted — the DB's canonical value — rather than the
            // in-memory value from whichever POST /users happened to run in
            // this call, which a DO NOTHING would have silently discarded.
            sqlx::query_scalar::<_, String>(
                "INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, $2) \
                 ON CONFLICT (user_id) DO UPDATE SET basiq_user_id = basiq_users.basiq_user_id \
                 RETURNING basiq_user_id")
                .bind(user_id).bind(&basiq_user.id)
                .fetch_one(pool).await.map_err(internal_error)?
        }
    };

    #[derive(Deserialize)]
    struct ClientToken { access_token: String }
    let api_key = env_opt("BASIQ_API_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Bank sync is not configured".to_string()))?;
    let resp = http_client()
        .post(format!("{}/token", basiq_api_base()))
        .header("Authorization", format!("Basic {api_key}"))
        .header("basiq-version", "3.0")
        .form(&[("scope", "CLIENT_ACCESS"), ("userId", basiq_user_id.as_str())])
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
        .bind(session_id).bind(budget_id).bind(user_id).bind(&basiq_user_id)
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
    // A missing `data` key (vs. a present-but-empty array) means the response
    // didn't have the shape this code expects — this must be a real error,
    // not silently treated as "this user genuinely has zero accounts" (code
    // review fix, #323): falling through with an empty `accounts` vec would
    // otherwise mark the session 'completed' and return an indistinguishable
    // "successful" empty link for what is actually a schema mismatch/outage.
    let Some(accounts) = accounts_body.get("data").and_then(|v| v.as_array()).cloned() else {
        tracing::error!(%user_id, basiq_user_id, response = %accounts_body, "basiq accounts response missing expected 'data' array");
        return Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()));
    };

    // `GET users/{id}/accounts` returns EVERY account this Basiq user has
    // EVER authorized, across every Nels budget the user owns — not just the
    // connection this specific consent session just created — because
    // `basiq_user_id` is now one persistent id reused across all of a Nels
    // user's budgets (spec Assumption 5's disconnect fix, code review
    // finding #323). A cross-budget conflict on ONE pre-existing, unrelated
    // account must therefore SKIP that account (log a warning) and continue
    // processing the rest, rather than aborting the whole function — the
    // original "abort entirely" behavior meant an old account linked to a
    // completely different budget could silently block this session's
    // actually-relevant new account(s) from ever completing. If literally
    // every account in this batch turns out to be an existing conflict
    // (e.g. the user re-authorized a connection already linked elsewhere),
    // `results` stays empty and the CONFLICT error is still returned below —
    // so a genuine "nothing new to link" case is not silently swallowed as
    // an empty success.
    let mut results = Vec::with_capacity(accounts.len());
    let mut had_conflict = false;
    let mut had_missing_connection = false;
    for acc in &accounts {
        let account_id = match acc.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        // A missing `connection` field must SKIP this account, not silently
        // substitute the Basiq user id as a fake connection id (code review
        // fix, #323) — `provider_ref` drives disconnect_linked_account's
        // sibling-count check, so mislabeling it could make two accounts
        // under genuinely DIFFERENT real connections look like they share
        // one, corrupting whether a disconnect actually revokes Basiq-side
        // access.
        let Some(connection_id) = acc.get("connection").and_then(|v| v.as_str()) else {
            tracing::warn!(%user_id, account_id, "basiq account missing connection id — skipping this account");
            had_missing_connection = true;
            continue;
        };
        let connection_id = connection_id.to_string();
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
                    "basiq account already linked to a different budget — skipping this account, not aborting the whole session");
                had_conflict = true;
                continue;
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

    if results.is_empty() && had_conflict {
        return Err((StatusCode::CONFLICT, "This bank account is already linked to a different budget. Disconnect it there first.".to_string()));
    }
    if results.is_empty() && had_missing_connection {
        return Err((StatusCode::BAD_GATEWAY, "Basiq returned an unexpected account response. Please try linking again.".to_string()));
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

    // Standing ignore rules (#374): loaded once per sync.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

    let server_tok = server_token().await?;
    // Basiq's transactions API is ALWAYS scoped to the Basiq USER, not the
    // account — there is no `/accounts/{accountId}/transactions` route on
    // Basiq's real API (code review fix, #323: the original code called a
    // fabricated endpoint that would 404 in production against every real
    // Basiq account). Resolve this account's owning Basiq user id from
    // `basiq_users` (the same table `disconnect_linked_account` already
    // resolves it from) and call the real, user-scoped, account-filtered
    // endpoint instead.
    let basiq_user_id: String = sqlx::query_scalar(
        "SELECT basiq_user_id FROM basiq_users WHERE user_id = $1")
        .bind(linked_account.user_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or_else(|| internal_error(format!(
            "basiq sync: no basiq_users row for user_id {} — cannot resolve Basiq user id",
            linked_account.user_id
        )))?;
    let filter = urlencoding_basiq(&format!("account.id.eq('{}')", linked_account.provider_account_id));
    let path = format!("users/{basiq_user_id}/transactions?filter={filter}");
    let body = basiq_get(&path, &server_tok).await?;
    let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut imported: u64 = 0;
    for tx in &rows {
        let tx_id = match tx.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        // A MISSING `amount` field must be dropped, same as a malformed one
        // (code review fix, #323) — `.unwrap_or("0")` before the parse check
        // would otherwise let an absent field silently masquerade as a
        // genuine $0.00 transaction and sail straight past the malformed-
        // amount guard below, since normalize_amount("0") parses fine.
        let Some(amount_str) = tx.get("amount").and_then(|v| v.as_str()) else {
            tracing::warn!(account_id = %linked_account.id, tx_id, "basiq sync: dropping transaction with a missing amount field");
            continue;
        };
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
        let excluded = crate::ignore_rules::transaction_matches_ignore_rule(&description, linked_account.id, &ignore_rules);

        let new_tx_id = Uuid::new_v4();
        let res = sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, \
                 external_account_id, provider_transaction_id, currency, excluded_from_budget, source, review_status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'AUD', $9, 'imported', 'needs_review') \
             ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
        )
        .bind(new_tx_id).bind(linked_account.budget_id).bind(category_id).bind(amount)
        .bind(transacted_at).bind(&description).bind(linked_account.id).bind(&tx_id).bind(excluded)
        .execute(pool).await.map_err(internal_error)?;
        if res.rows_affected() > 0 {
            imported += 1;
            // #403 P3: reconcile a genuinely new import against a Nels-logged twin.
            crate::duplicate_match::link_duplicate_for_import(pool, linked_account.budget_id, new_tx_id).await;
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
///
/// The sibling-count check and the final status flip are two separate
/// statements (not wrapped in one DB transaction) — a race between two
/// concurrent disconnects of sibling rows is possible in theory (both could
/// read `sibling_count == 1` before either flips its own row) but is
/// accepted for v1: worst case is one redundant/missing Basiq-side revoke
/// call, never data loss, and Basiq's own delete is idempotent-ish (404 is
/// already treated as success by `basiq_delete`).
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
        // Basiq's connection-delete endpoint is scoped by BOTH user id and
        // connection id (`/users/{userId}/connections/{connectionId}`).
        // `basiq_users` holds exactly ONE Basiq user id per Nels `user_id`
        // (`create_consent_session` reuses it across every connection that
        // user links — see its doc comment), keyed off the linked account's
        // OWN `user_id` column, so this is correct regardless of how many
        // connections/sessions this user has accumulated over time. This
        // replaces an earlier "most recent completed bank_link_sessions row"
        // lookup that silently resolved to the WRONG (newest) Basiq user id
        // as soon as a user had linked a second Basiq connection (code
        // review fix, #323).
        let basiq_user_id: Option<String> = sqlx::query_scalar(
            "SELECT basiq_user_id FROM basiq_users WHERE user_id = $1")
            .bind(row.user_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(basiq_user_id) = basiq_user_id {
            let server_tok = server_token().await?;
            basiq_delete(&format!("users/{basiq_user_id}/connections/{}", row.provider_ref), &server_tok).await?;
        } else {
            tracing::warn!(account_id = %account_id, "basiq disconnect: no basiq_users row found to resolve the Basiq user id; local-only disconnect");
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
/// HMAC scheme against `BASIQ_WEBHOOK_SECRET` (spec Assumption 6). NOTE: this
/// assumes Basiq's real webhook signature header uses the SAME `t=...,v1=...`
/// comma-separated framing `verify_stripe_signature` parses, not just the
/// same HMAC primitive — this is unverified against Basiq's actual
/// dashboard-configured webhook format (tests only self-sign with this same
/// helper, so they can't catch a framing mismatch) and MUST be confirmed
/// before relying on this in production, mirroring the same kind of gap the
/// spec already flags for Akahu's RSA signature verification.
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
        // This is a PUBLIC, unauthenticated route — a rejected signature
        // (misconfigured secret, a real forgery attempt, or Basiq's actual
        // format diverging from this HMAC scheme) must be visible in logs,
        // not vanish silently (code review fix, #323).
        tracing::warn!("invalid basiq webhook signature");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "invalid basiq webhook payload");
            return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string()));
        }
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
            .bind(id).bind(format!("basiq-{id}@test.example")).execute(db).await.unwrap();
        id
    }
    async fn mk_budget(db: &PgPool, owner_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(id).bind(owner_id).execute(db).await.unwrap();
        id
    }
    // 'trialing' (not 'active') so `entitlement::resolve` maps the owner to
    // Tier::Pro unconditionally (no price_id/STRIPE_PRICE_* env needed) — see
    // entitlement.rs's `resolve`: `Some("trialing") => Tier::Pro` regardless
    // of price/catalog, unlike 'active' which requires a Pro price to be
    // configured or fails safe to Basic.
    async fn mk_pro_subscription(db: &PgPool, user_id: Uuid, customer_id: &str) {
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
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

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_consent_session_skips_a_cross_budget_conflict_instead_of_aborting() {
        // Regression test (code review finding, #323): `GET
        // users/{id}/accounts` returns EVERY account this Basiq user has ever
        // authorized, across ALL of the user's Nels budgets, since one
        // basiq_users row is now reused across budgets (the disconnect fix).
        // A Pro user with an account already linked to budget A must still
        // be able to complete a BRAND NEW consent session for an unrelated
        // account into budget B — the old account's cross-budget conflict
        // must be skipped, not abort the whole completion.
        let server = MockServer::start().await;
        set_basiq_env(&server);
        mount_server_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_basiq_multi").await;
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'basiq_user_multi')")
            .bind(uid).execute(&db).await.unwrap();
        // Already linked to budget A from an earlier, unrelated session.
        mk_linked_account(&db, bid_a, uid, "acc_old_a", "conn_old").await;

        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
             VALUES ($1, $2, $3, 'basiq_user_multi', 'basiq', 'pending')")
            .bind(Uuid::new_v4()).bind(bid_b).bind(uid).execute(&db).await.unwrap();
        let basiq_ref: Uuid = sqlx::query_scalar(
            "SELECT id FROM bank_link_sessions WHERE budget_id = $1 AND provider = 'basiq'")
            .bind(bid_b).fetch_one(&db).await.unwrap();

        // Basiq's account list now contains BOTH the old budget-A account and
        // a brand-new budget-B account (this is the real-world shape: the
        // endpoint is scoped to the Basiq USER, not to one consent session).
        Mock::given(method("GET")).and(path("/users/basiq_user_multi/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "acc_old_a", "accountNo": "1111", "name": "Old", "connection": "conn_old", "institution": {"shortName": "ANZ"}},
                    {"id": "acc_new_b", "accountNo": "2222", "name": "New", "connection": "conn_new", "institution": {"shortName": "Westpac"}}
                ]
            })))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acc_new_b/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&server).await;

        let completed = complete_consent_session(&db, uid, bid_b, basiq_ref).await
            .expect("completion must succeed despite an unrelated cross-budget account in the same Basiq user's account list");
        assert_eq!(completed.accounts.len(), 1, "only the new budget-B account should be returned");
        assert_eq!(completed.accounts[0].provider, "basiq");

        // The new account must actually be attributed to budget B.
        let new_account_budget: Uuid = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = 'acc_new_b'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(new_account_budget, bid_b);

        // The old account must be completely untouched — still under budget A.
        let old_account_budget: Uuid = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = 'acc_old_a'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(old_account_budget, bid_a);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_consent_session_errors_when_every_account_conflicts() {
        // If EVERY account in the batch is an existing cross-budget conflict
        // (nothing new to link), the session must still surface an explicit
        // 409 rather than silently "succeeding" with zero accounts.
        let server = MockServer::start().await;
        set_basiq_env(&server);
        mount_server_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_basiq_allconflict").await;
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'basiq_user_allconflict')")
            .bind(uid).execute(&db).await.unwrap();
        mk_linked_account(&db, bid_a, uid, "acc_only_a", "conn_only").await;

        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
             VALUES ($1, $2, $3, 'basiq_user_allconflict', 'basiq', 'pending')")
            .bind(Uuid::new_v4()).bind(bid_b).bind(uid).execute(&db).await.unwrap();
        let basiq_ref: Uuid = sqlx::query_scalar(
            "SELECT id FROM bank_link_sessions WHERE budget_id = $1 AND provider = 'basiq'")
            .bind(bid_b).fetch_one(&db).await.unwrap();

        Mock::given(method("GET")).and(path("/users/basiq_user_allconflict/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "acc_only_a", "accountNo": "1111", "name": "Old", "connection": "conn_only", "institution": {"shortName": "ANZ"}}]
            })))
            .mount(&server).await;

        let result = complete_consent_session(&db, uid, bid_b, basiq_ref).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

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
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // `sync_account_transactions` requests `/users/{basiqUserId}/transactions?filter=...`
        // (code review fix, #323 — the real Basiq API has no
        // `/accounts/{id}/transactions` route), so a `basiq_users` row must
        // exist to resolve this account's owning Basiq user id.
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'basiq_user_sync_owner')")
            .bind(uid).execute(&db).await.unwrap();
        Mock::given(method("GET")).and(path("/users/basiq_user_sync_owner/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "basiq_tx_1", "amount": "-52.30", "description": "Woolworths", "postDate": "2026-07-01T00:00:00Z", "account": "acc_sync_1"}]
            })))
            .mount(&server).await;

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

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn create_consent_session_reuses_existing_basiq_user() {
        let server = MockServer::start().await;
        set_basiq_env(&server);
        mount_server_token(&server).await;
        // `.expect(1)`: if `create_consent_session` regresses to minting a new
        // Basiq user on every call, this fires twice and the mock's
        // expectation fails when the MockServer is dropped at the end of the
        // test (the same "prove a call did NOT happen twice" technique
        // `disconnect_revokes_connection_only_when_last_active_sibling` uses
        // via `.expect(0)`).
        Mock::given(method("POST")).and(path("/users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "basiq_user_reuse"})))
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/token")).and(wiremock::matchers::body_string_contains("scope=CLIENT_ACCESS"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "client_tok", "expires_in": 3600})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_basiq_reuse").await;

        create_consent_session(&db, uid, bid).await.expect("first call creates a basiq user");
        let first_id: String = sqlx::query_scalar("SELECT basiq_user_id FROM basiq_users WHERE user_id = $1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(first_id, "basiq_user_reuse");

        create_consent_session(&db, uid, bid).await.expect("second call reuses the existing basiq user");
        let second_id: String = sqlx::query_scalar("SELECT basiq_user_id FROM basiq_users WHERE user_id = $1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(second_id, first_id, "second call must reuse the same Basiq user id, not mint a new one");
        let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM basiq_users WHERE user_id = $1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(row_count, 1, "exactly one basiq_users row per Nels user, even after multiple consent sessions");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_resolves_basiq_user_id_from_basiq_users_not_stale_sessions() {
        let server = MockServer::start().await;
        set_basiq_env(&server);
        mount_server_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;

        // Two DISTINCT Basiq connections for the SAME Nels user: "conn_old" was
        // linked first, "conn_new" was linked afterwards — reproducing the
        // regression scenario where a user accumulates a second Basiq
        // connection over time.
        let acc_old = mk_linked_account(&db, bid, uid, "acc_multi_old", "conn_old").await;
        let _acc_new = mk_linked_account(&db, bid, uid, "acc_multi_new", "conn_new").await;

        // Stale/misleading bank_link_sessions rows, exactly the shape that
        // broke the OLD "most recent completed session" resolution: the
        // NEWER session (for conn_new) recorded a DIFFERENT Basiq user id
        // than the older one. If disconnect_linked_account still derived the
        // Basiq user id from these, disconnecting the OLDER connection would
        // wrongly use the NEWER session's user id.
        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status, created_at) \
             VALUES ($1, $2, $3, 'stale_user_from_old_session', 'basiq', 'completed', now() - interval '2 hours')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status, created_at) \
             VALUES ($1, $2, $3, 'stale_user_from_new_session', 'basiq', 'completed', now())")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();

        // The canonical mapping this fix introduces: one Basiq user id per
        // Nels user, independent of session history.
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'correct_basiq_user')")
            .bind(uid).execute(&db).await.unwrap();

        Mock::given(method("DELETE")).and(path("/users/correct_basiq_user/connections/conn_old"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server).await;
        // If the old bug regressed, disconnect would call the NEWER
        // (wrong) session's Basiq user id instead of the correct one.
        Mock::given(method("DELETE")).and(path("/users/stale_user_from_new_session/connections/conn_old"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server).await;

        disconnect_linked_account(&db, uid, bid, acc_old.id).await.expect("disconnect ok");
        let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(acc_old.id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_keeps_past_transaction_history() {
        // AC: "disconnecting stops future imports without deleting previously-
        // imported transactions" — a regression test the pr-test-analyzer
        // review round flagged as missing (code review finding, #323).
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acc_history_1", "conn_history_only").await;
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, amount, transaction_date, description, external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, 42.0, now(), 'Kept transaction', $3, 'basiq_tx_history_1', 'AUD')")
            .bind(Uuid::new_v4()).bind(bid).bind(linked.id).execute(&db).await.unwrap();

        disconnect_linked_account(&db, uid, bid, linked.id).await.expect("disconnect ok");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'basiq_tx_history_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1, "disconnect must not delete previously-imported transactions");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    // --- webhook handler coverage -------------------------------------
    //
    // Previously zero test coverage (pr-test-analyzer review finding, #323):
    // only the pure map_basiq_webhook_event parser was tested, never the
    // actual `webhook` handler's signature verification or its Pro-gate
    // before syncing — exactly mirroring financial_connections.rs's own
    // webhook test pattern (test_state/sign/signed_headers helpers).

    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256Test = Hmac<Sha256>;

    fn sign(secret: &str, t: i64, payload: &[u8]) -> String {
        let mut signed = format!("{t}.").into_bytes();
        signed.extend_from_slice(payload);
        let mut mac = HmacSha256Test::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(&signed);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("t={t},v1={hex}")
    }

    fn basiq_webhook_body(event_type: &str, connection_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": event_type,
            "connection": { "id": connection_id },
        })).unwrap()
    }

    fn test_state(db: PgPool) -> crate::auth::AppState {
        crate::auth::AppState {
            db,
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    fn signed_basiq_headers(secret: &str, body: &[u8]) -> axum::http::HeaderMap {
        let t = chrono::Utc::now().timestamp();
        let sig = sign(secret, t, body);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("basiq-signature", axum::http::HeaderValue::from_str(&sig).unwrap());
        headers
    }

    fn set_basiq_webhook_secret(secret: &str) {
        std::env::set_var("BASIQ_WEBHOOK_SECRET", secret);
    }
    fn clear_basiq_webhook_secret() {
        std::env::remove_var("BASIQ_WEBHOOK_SECRET");
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_rejects_invalid_signature() {
        set_basiq_webhook_secret("whsec_basiq_test_invalid");
        let db = test_pool().await;
        let body = basiq_webhook_body("transactions.updated", "conn_x");
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("basiq-signature", axum::http::HeaderValue::from_static("t=1,v1=deadbeef"));
        let state = test_state(db);

        let result = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);

        clear_basiq_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_skips_sync_for_non_pro_owner() {
        const SECRET: &str = "whsec_basiq_test_nonpro";
        let server = MockServer::start().await;
        set_basiq_env(&server);
        set_basiq_webhook_secret(SECRET);
        // If the Pro-gate is broken and sync runs anyway, this GET fires;
        // `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/users/basiq_user_wh_nonpro/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // No subscriptions row at all -> not Pro.
        let linked = mk_linked_account(&db, bid, uid, "acc_wh_nonpro_1", "conn_wh_nonpro").await;
        assert_eq!(linked.status, "active");

        let body = basiq_webhook_body("transactions.updated", "conn_wh_nonpro");
        let headers = signed_basiq_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await
            .expect("webhook must return Ok even for a guarded no-op");
        assert_eq!(status, StatusCode::OK);
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
        clear_basiq_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_syncs_active_pro_owned_account_on_transactions_updated() {
        const SECRET: &str = "whsec_basiq_test_syncs";
        let server = MockServer::start().await;
        set_basiq_env(&server);
        set_basiq_webhook_secret(SECRET);
        mount_server_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_wh_sync").await;
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'basiq_user_wh_sync')")
            .bind(uid).execute(&db).await.unwrap();
        let linked = mk_linked_account(&db, bid, uid, "acc_wh_sync_1", "conn_wh_sync").await;

        Mock::given(method("GET")).and(path("/users/basiq_user_wh_sync/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "basiq_tx_wh_1", "amount": "10.00", "description": "Webhook synced", "postDate": "2026-07-01T00:00:00Z", "account": "acc_wh_sync_1"}]
            })))
            .expect(1)
            .mount(&server).await;

        let body = basiq_webhook_body("transactions.updated", "conn_wh_sync");
        let headers = signed_basiq_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await
            .expect("webhook ok");
        assert_eq!(status, StatusCode::OK);

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'basiq_tx_wh_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1, "webhook must have triggered a real sync for an active, Pro-owned account");
        let _ = linked; // used only to assert precondition via mk_linked_account

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
        clear_basiq_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_consent_session_errors_when_accounts_response_missing_data_key() {
        // Regression test (code review fix, #323): a malformed provider
        // response (missing the expected `data` array entirely) must not be
        // silently treated the same as "zero accounts, success".
        let server = MockServer::start().await;
        set_basiq_env(&server);
        mount_server_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_basiq_badshape").await;
        sqlx::query("INSERT INTO basiq_users (user_id, basiq_user_id) VALUES ($1, 'basiq_user_badshape')")
            .bind(uid).execute(&db).await.unwrap();

        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
             VALUES ($1, $2, $3, 'basiq_user_badshape', 'basiq', 'pending')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let basiq_ref: Uuid = sqlx::query_scalar(
            "SELECT id FROM bank_link_sessions WHERE budget_id = $1 AND provider = 'basiq'")
            .bind(bid).fetch_one(&db).await.unwrap();

        // No "data" key at all — a malformed/unexpected response shape.
        Mock::given(method("GET")).and(path("/users/basiq_user_badshape/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"error": "unexpected shape"})))
            .mount(&server).await;

        let result = complete_consent_session(&db, uid, bid, basiq_ref).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_GATEWAY);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_basiq_env();
    }
}
