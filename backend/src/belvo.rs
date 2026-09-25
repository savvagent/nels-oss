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

/// GET an ALREADY-FULLY-QUALIFIED URL (Basic-auth'd the same as `belvo_get`,
/// which instead builds the URL from a relative `path`) — needed to follow a
/// paginated list response's own `next` URL, which Belvo returns as a
/// complete absolute URL rather than a bare cursor token (code review
/// finding: `sync_account_transactions` was silently truncating any
/// multi-page transactions response before this existed).
async fn belvo_get_absolute(url: &str) -> Result<(reqwest::StatusCode, serde_json::Value), (StatusCode, String)> {
    let (id, password) = belvo_credentials()?;
    let resp = http_client().get(url).basic_auth(id, Some(password)).send().await
        .map_err(|e| internal_error(format!("belvo GET (paginated) {url}: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| internal_error(format!("belvo decode (paginated) {url}: {e}")))?;
    Ok((status, body))
}

/// POST returning the raw status alongside the decoded body — needed by
/// `sync_account_transactions`, which must inspect an ERROR body's contents
/// to detect an invalid-Link response (mirrors `gocardless::gocardless_get_raw`).
/// Reads the body as TEXT first rather than going straight through `.json()`
/// (Copilot review finding): the spec's "not ready yet" response
/// (`api/transactions/` returning a 202-class status with nothing to
/// import — spec §"Sync semantics") can have an EMPTY body, and `.json()`
/// on an empty body errors, which turned the spec's documented clean
/// `Ok(0)` no-op into a hard 500. An empty/whitespace-only body is treated
/// as `Value::Null` (which every caller below already handles the same way
/// it handles a present-but-empty `results` array); a genuinely malformed
/// non-empty body still surfaces as a decode error, same as before.
async fn belvo_post_raw(path: &str, json_body: &serde_json::Value) -> Result<(reqwest::StatusCode, serde_json::Value), (StatusCode, String)> {
    let (id, password) = belvo_credentials()?;
    let url = format!("{}/{}", belvo_api_base().trim_end_matches('/'), path.trim_start_matches('/'));
    let resp = http_client().post(&url).basic_auth(id, Some(password)).json(json_body).send().await
        .map_err(|e| internal_error(format!("belvo POST {path}: {e}")))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| internal_error(format!("belvo read body {path}: {e}")))?;
    let body: serde_json::Value = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).map_err(|e| internal_error(format!("belvo decode {path}: {e}")))?
    };
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

/// Belvo transaction amounts are a plain signed(-ish) number, unlike
/// GoCardless's decimal string or Stripe's integer cents (spec Assumption
/// 10) — this codebase always stores a positive magnitude regardless of
/// direction, so this takes the absolute value.
pub(crate) fn normalize_amount(amount: f64) -> f64 {
    amount.abs()
}

/// Normalize a country code to the canonical uppercase form this codebase
/// persists/matches everywhere else (`belvo_link_sessions.country`'s CHECK
/// constraint, `currency_for_country`'s match arms) — `Provider::for_country`
/// validates case-insensitively (it happily accepts `"mx"`), so a caller that
/// only checks against `for_country` before forwarding the ORIGINAL, still
/// possibly-lowercase string can pass validation and then fail the INSERT's
/// case-sensitive CHECK constraint with a raw 500, or (were that guard ever
/// missing) feed the wrong-cased string into `currency_for_country`'s exact
/// match (Copilot review finding).
fn normalize_country(country: &str) -> String {
    country.to_uppercase()
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

/// Constant-time byte-equality check (Copilot review finding): a plain `==`
/// on the decoded credential string short-circuits at the first differing
/// byte, which can leak how many leading bytes of a submitted webhook
/// credential were correct via response-timing differences. No new
/// dependency needed for this — a simple length check plus an OR-accumulated
/// XOR over every byte pair (never branching on an individual byte's
/// comparison result) is the standard hand-rolled approach.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
    constant_time_eq(decoded_str.as_bytes(), format!("{expected_user}:{expected_password}").as_bytes())
}

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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    let country = normalize_country(country);

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO belvo_link_sessions (id, budget_id, user_id, country) VALUES ($1, $2, $3, $4)")
        .bind(session_id).bind(budget_id).bind(user_id).bind(&country)
        .execute(pool).await.map_err(internal_error)?;

    let (secret_id, secret_password) = belvo_credentials()?;
    let access_token = belvo_mint_widget_token(&secret_id, &secret_password, &session_id.to_string(), &country).await?;

    Ok(BelvoWidgetSessionResponse { access_token, session_id })
}

/// Validate the shape of a client-supplied Belvo Link id BEFORE it's
/// interpolated into a Belvo API URL path/query segment. `complete_link_session`
/// is the one place this codebase ever splices a client-supplied provider id
/// straight into an outbound URL (GoCardless's/Stripe's equivalents only ever
/// look up their provider ids from a server-side session/agreement row) — a
/// value containing `/`, `?`, `&`, etc. could otherwise redirect the
/// credentialed request to an unintended path under Belvo's API. Belvo Link
/// ids are UUIDs in practice; this is deliberately a little more permissive
/// than a strict UUID parse (ASCII alphanumeric + hyphen only) so a
/// same-shape-but-differently-formatted id from a future Belvo API version
/// doesn't spuriously break this check — the goal is "safe URL segment," not
/// "exactly a UUID." Pure/dependency-free so it's unit-testable in isolation.
pub(crate) fn is_valid_belvo_link_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    if !is_valid_belvo_link_id(belvo_link_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid link id".to_string()));
    }

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

    // Standing ignore rules (#374): loaded once per sync.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

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

    // Belvo returns either a bare array or a `{"results": [...], "next": ...}`
    // envelope depending on endpoint/version (documented-but-unverified —
    // spec §8 Risks); handling both is cheap and avoids depending on which
    // shape this specific account/region returns. A bare array has no
    // pagination concept, so `next` is only ever consulted on the envelope
    // shape — the loop below follows it to completion (code review finding:
    // a first full-history sync could otherwise silently truncate to just
    // the first page).
    let mut txs: Vec<serde_json::Value> = body.as_array().cloned()
        .or_else(|| body.get("results").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let mut next_url = body.get("next").and_then(|v| v.as_str()).map(str::to_string);
    while let Some(url) = next_url.take() {
        let (page_status, page_body) = belvo_get_absolute(&url).await?;
        if !page_status.is_success() {
            // Copilot review finding: this used to `break` and fall through
            // to the unconditional `last_synced_at = now()` update below,
            // which PERMANENTLY skipped every transaction on this and any
            // later page — the next sync's `date_from` starts at the
            // just-advanced marker, not the original one. Failing the whole
            // sync instead leaves `last_synced_at` untouched, so the next
            // webhook/manual refresh retries the SAME `date_from..now` range
            // in full; any transactions already inserted from earlier pages
            // in this attempt are safely re-imported no-ops via the
            // `ON CONFLICT ... DO NOTHING` below.
            tracing::error!(?page_status, url, account_id = %linked_account.id,
                "belvo sync: a transactions pagination page failed; aborting this sync so a retry covers every page");
            return Err((StatusCode::BAD_GATEWAY, "Bank provider error".to_string()));
        }
        let mut more: Vec<serde_json::Value> = page_body.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        txs.append(&mut more);
        next_url = page_body.get("next").and_then(|v| v.as_str()).map(str::to_string);
    }
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
                // Escalated to `error!` (code review finding, not just a
                // `warn!`): unlike GoCardless's single-currency-zone EUR
                // default, this fallback can be ACTIVELY WRONG, not merely
                // imprecise — some Mexican/Brazilian accounts are
                // USD-denominated, so a missing currency field here could
                // silently mislabel a transaction's real magnitude by an
                // order of magnitude rather than just a cosmetic label
                // error. Worth investigating every time it fires, not just
                // noting.
                tracing::error!(account_id = %linked_account.id, tx_id, fallback, "belvo sync: transaction missing currency; falling back to the linked account's own country currency — verify this transaction's real currency manually");
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
        let excluded = crate::ignore_rules::transaction_matches_ignore_rule(&description, linked_account.id, &ignore_rules);

        let new_tx_id = Uuid::new_v4();
        let res = sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, \
                 external_account_id, provider_transaction_id, currency, excluded_from_budget, source, review_status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'imported', 'needs_review') \
             ON CONFLICT (external_account_id, provider_transaction_id) WHERE provider_transaction_id IS NOT NULL DO NOTHING"
        )
        .bind(new_tx_id).bind(linked_account.budget_id).bind(category_id).bind(amount)
        .bind(transacted_at).bind(&description).bind(linked_account.id).bind(tx_id).bind(&currency).bind(excluded)
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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

use axum::extract::State;
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
/// invalid-Link response (spec Assumption 8/12), so this handler doesn't need
/// to branch on `webhook_type`/`webhook_code` beyond logging them.
/// Auth is checked BEFORE the body is even parsed (raw `body: Bytes` +
/// manual `serde_json::from_slice`, deliberately NOT a `Json<_>` extractor
/// argument) — matches `financial_connections::webhook`'s established
/// ordering exactly (its own doc comment: "Raw body ... required ... must
/// NOT use `Json<_>` extraction", there for HMAC reasons; here for the
/// same-shaped reason of "verify credentials before doing anything else
/// with an unauthenticated request," since an axum `Json<_>` argument
/// deserializes — and can reject with its own 400/422 — before this
/// handler's body, and therefore its auth check, ever runs (code review
/// finding: that ordering meant a malformed body could short-circuit past
/// the auth check with no trace of the auth check even having been
/// attempted).
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let expected_user = env_opt("BELVO_WEBHOOK_USER")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    let expected_password = env_opt("BELVO_WEBHOOK_PASSWORD")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Belvo is not configured".to_string()))?;
    let auth_header = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !verify_webhook_auth(auth_header, &expected_user, &expected_password) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid webhook credentials".to_string()));
    }

    let payload: BelvoWebhookPayload = serde_json::from_slice(&body).map_err(|e| {
        // Code review finding: this error was previously discarded
        // (`map_err(|_| ...)`) with no log line — if Belvo ever changes its
        // payload shape (an explicitly-flagged unverified risk for this
        // whole integration), every webhook would start failing here with
        // zero diagnostic trail.
        tracing::warn!(error = %e, "belvo webhook: failed to parse payload");
        (StatusCode::BAD_REQUEST, "Invalid payload".to_string())
    })?;

    tracing::debug!(
        webhook_type = %payload.webhook_type, webhook_code = %payload.webhook_code, link_id = %payload.link_id,
        "belvo webhook received"
    );

    let accounts: Vec<crate::db::LinkedAccount> = sqlx::query_as(
        "SELECT * FROM linked_accounts WHERE provider = 'belvo' AND provider_ref = $1 AND status = 'active'")
        .bind(&payload.link_id)
        .fetch_all(&state.db).await.map_err(internal_error)?;

    // Code review finding: this handler used to unconditionally return
    // `Ok(StatusCode::OK)` no matter how many accounts below failed to
    // process, which defeated the entire point of isolating per-account
    // errors — that isolation exists so ONE account's own failure doesn't
    // block OTHER accounts in the same delivery from syncing; it was never
    // meant to make Belvo believe a systemic failure (e.g. every account's
    // subscription lookup or sync call failing because the DB or Belvo
    // itself is down) was a success. Track whether ANYTHING genuinely
    // failed (never set for the "owner not Pro" skip, which is an
    // intentional, successful no-op) and surface a 5xx if so, so Belvo's
    // own webhook retry mechanism gets a chance to redeliver — idempotent
    // inserts (`ON CONFLICT ... DO NOTHING`) make retrying a delivery that
    // was a mix of already-succeeded and failed accounts safe.
    let mut any_failed = false;
    for account in accounts {
        let owner_status: Option<String> = match sqlx::query_scalar(
            "SELECT status FROM subscriptions WHERE user_id = $1")
            .bind(account.user_id).fetch_optional(&state.db).await {
            Ok(s) => s.flatten(),
            Err(e) => {
                tracing::error!(%e, account_id = %account.id, "belvo webhook: failed to check owner subscription status");
                any_failed = true;
                continue;
            }
        };
        if !user_is_pro(owner_status.as_deref()) {
            tracing::info!(account_id = %account.id, "belvo webhook: skipping, owner not Pro (subscription lapsed)");
            continue;
        }
        if let Err((status, msg)) = sync_account_transactions(&state.db, &account).await {
            tracing::warn!(?status, %msg, account_id = %account.id, "belvo webhook-driven sync failed");
            any_failed = true;
        }
    }
    if any_failed {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Some accounts failed to sync".to_string()));
    }
    Ok(StatusCode::OK)
}

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
    fn normalize_country_uppercases_lowercase_input() {
        assert_eq!(normalize_country("mx"), "MX");
        assert_eq!(normalize_country("Br"), "BR");
        assert_eq!(normalize_country("MX"), "MX");
    }

    #[test]
    fn constant_time_eq_matches_and_rejects_correctly() {
        assert!(constant_time_eq(b"whuser:whpass", b"whuser:whpass"));
        assert!(!constant_time_eq(b"whuser:whpass", b"whuser:wrongpass"));
        assert!(!constant_time_eq(b"short", b"a-longer-value"));
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

    #[test]
    fn is_valid_belvo_link_id_accepts_a_uuid_shaped_id() {
        assert!(is_valid_belvo_link_id("3fa85f64-5717-4562-b3fc-2c963f66afa6"));
    }

    #[test]
    fn is_valid_belvo_link_id_rejects_empty() {
        assert!(!is_valid_belvo_link_id(""));
    }

    #[test]
    fn is_valid_belvo_link_id_rejects_path_traversal_characters() {
        assert!(!is_valid_belvo_link_id("abc/../../secrets"));
        assert!(!is_valid_belvo_link_id("abc?x=1"));
        assert!(!is_valid_belvo_link_id("abc&y=2"));
        assert!(!is_valid_belvo_link_id("abc/def"));
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
        // 'trialing' (not 'active') so `entitlement::resolve` maps the owner to
        // `Tier::Pro` unconditionally in tests, without needing a Pro price env
        // var or `#[serial]`, unlike 'active' which requires a Pro price to be
        // configured to resolve above Basic.
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
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
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

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
        Mock::given(method("GET")).and(path("/api/links/link-1/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "link-1", "institution": "banregio_mx", "external_id": session_id_str,
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

        let completed = complete_link_session(&db, uid, bid, started.session_id, "link-1").await.expect("complete ok");
        assert_eq!(completed.accounts.len(), 1);
        assert_eq!(completed.accounts[0].provider, "belvo");
        assert_eq!(
            completed.accounts[0].institution_name.as_deref(), Some("banregio_mx"),
            "institution_name must come from the Link's own institution field"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

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
        Mock::given(method("GET")).and(path("/api/links/link-new/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "link-new", "institution": "banregio_mx", "external_id": session_id.to_string(),
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

        let result = complete_link_session(&db, uid, bid, session_id, "link-new").await.expect("complete ok");
        assert_eq!(result.accounts.len(), 1);
        assert_eq!(result.accounts[0].id, old_row_id, "must reconcile onto the existing row's id, not create a new one");

        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert_eq!(total, 1, "reconciliation must not leave a duplicate row behind");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    /// Copilot review finding: `belvo_post_raw` used to call `resp.json()`
    /// unconditionally, so Belvo's documented "not ready yet" response (a
    /// 202-class status with an EMPTY body) errored instead of the spec's
    /// intended clean `Ok(0)` no-op.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_treats_empty_bodied_not_ready_response_as_clean_noop() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_not_ready", "MX").await;

        let imported = sync_account_transactions(&db, &linked).await
            .expect("an empty-bodied 202 must be a clean no-op, not a decode error");
        assert_eq!(imported, 0);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    /// Copilot review finding: a failed pagination page used to `break` and
    /// still fall through to the unconditional `last_synced_at` update,
    /// which permanently skipped every transaction on that page and beyond.
    /// This verifies a failed second page now fails the whole sync AND
    /// leaves `last_synced_at` untouched, so a retry re-covers the full range.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_aborts_and_preserves_last_synced_at_when_a_pagination_page_fails() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        let next_url = format!("{}/api/transactions/?page=2", server.uri());
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{"id": "tx_page1", "amount": 10.0, "currency": "MXN", "description": "Page 1", "value_date": "2026-07-01"}],
                "next": next_url,
            })))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({"detail": "internal error"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_page_fail", "MX").await;
        assert!(linked.last_synced_at.is_none(), "precondition: no prior sync");

        let result = sync_account_transactions(&db, &linked).await;
        assert!(result.is_err(), "a failed pagination page must fail the whole sync");

        let last_synced_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert!(last_synced_at.is_none(), "must not advance last_synced_at past an incomplete sync");

        let imported: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE external_account_id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert_eq!(imported, 0, "must not partially import page 1 while later pages are still unresolved");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

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
        let body = axum::body::Bytes::from(serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "new_transactions_available",
            "link_id": "link_x",
        })).unwrap());
        let result = webhook(axum::extract::State(state), HeaderMap::new(), body).await;
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
        let body = axum::body::Bytes::from(serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "new_transactions_available",
            "link_id": "link_lapsed",
        })).unwrap());
        let result = webhook(axum::extract::State(state), headers, body).await;
        assert_eq!(result.unwrap(), StatusCode::OK);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    /// Final-review finding: the webhook happy path (an authenticated
    /// delivery for an active, Pro-owned account actually reaching
    /// `sync_account_transactions` and importing data) had no test —
    /// only rejection and skip-for-lapsed-Pro were covered.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_syncs_active_pro_account_and_returns_ok() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "tx_webhook_1", "amount": 20.0, "currency": "MXN", "description": "Cafeteria", "value_date": "2026-07-01"}
            ])))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_webhook_ok").await;
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status, country) \
             VALUES ($1, $2, $3, 'belvo', 'acct_webhook_ok', 'link_webhook_ok', 'active', 'MX')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_webhook_ok'")
            .fetch_one(&db).await.unwrap();

        let state = test_state(db.clone());
        let mut headers = HeaderMap::new();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"whuser_test:whpass_test");
        headers.insert(axum::http::header::AUTHORIZATION, format!("Basic {encoded}").parse().unwrap());
        let body = axum::body::Bytes::from(serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "new_transactions_available",
            "link_id": "link_webhook_ok",
        })).unwrap());
        let result = webhook(axum::extract::State(state), headers, body).await;
        assert_eq!(result.unwrap(), StatusCode::OK);

        let imported: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE external_account_id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        assert_eq!(imported, 1, "webhook delivery must actually reach sync_account_transactions and import the transaction");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }

    /// Code review finding: the handler used to unconditionally return 200
    /// OK regardless of whether any account actually failed to sync, which
    /// meant Belvo would never retry a delivery that failed for a systemic
    /// reason (e.g. Belvo's own API down for this account). Verifies a
    /// genuine sync failure now surfaces as a 5xx.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_returns_error_status_when_an_account_fails_to_sync() {
        let server = MockServer::start().await;
        set_belvo_env(&server);
        Mock::given(method("POST")).and(path("/api/transactions/"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({"detail": "internal error"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_webhook_fail").await;
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'belvo', 'acct_webhook_fail', 'link_webhook_fail', 'active')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();

        let state = test_state(db.clone());
        let mut headers = HeaderMap::new();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"whuser_test:whpass_test");
        headers.insert(axum::http::header::AUTHORIZATION, format!("Basic {encoded}").parse().unwrap());
        let body = axum::body::Bytes::from(serde_json::to_vec(&serde_json::json!({
            "webhook_type": "TRANSACTIONS",
            "webhook_code": "new_transactions_available",
            "link_id": "link_webhook_fail",
        })).unwrap());
        let result = webhook(axum::extract::State(state), headers, body).await;
        assert_eq!(
            result.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR,
            "a genuine per-account sync failure must surface as a 5xx so Belvo's own retry mechanism gets a chance to redeliver"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_belvo_env();
    }
}
