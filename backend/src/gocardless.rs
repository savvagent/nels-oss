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

/// GoCardless amounts are decimal strings (e.g. "-15.00"), unlike Stripe's
/// integer cents — a separate, independently-tested normalization function
/// (spec Assumption 20). Returns None on a malformed string rather than
/// panicking; callers (`sync_account_transactions`) treat `None` as "drop this
/// transaction entirely" (logging a warning first) rather than importing it
/// with a fabricated 0.0 amount — a malformed amount string means we cannot
/// trust the payload, so silently recording a wrong dollar figure would be
/// worse than skipping the row.
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
    if status != reqwest::StatusCode::CONFLICT && status != reqwest::StatusCode::FORBIDDEN {
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
    // `country` reaches here straight from the REST query param / chat action
    // params — percent-encode it before interpolating into the URL so a stray
    // `&`/`#`/space can't alter the request (`url` is already a direct
    // dependency, so this needs no new crate).
    let encoded_country: String = url::form_urlencoded::byte_serialize(country.to_lowercase().as_bytes()).collect();
    let body = gocardless_get(&format!("institutions/?country={encoded_country}")).await?;
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    let institutions = list_institutions(country).await?;
    let found_institution = institutions.iter().find(|i| i.id == institution_id);
    let max_days = found_institution.map(|i| i.transaction_total_days).unwrap_or(90);
    // Persisted onto the session so `complete_link_session` can later write the
    // bank's real display name to `linked_accounts.institution_name` instead of
    // the machine institution_id (e.g. "MONZO_MONZ_GB") — code-review fix,
    // avoids a second `list_institutions` round-trip at completion time.
    let institution_name = found_institution.map(|i| i.name.clone()).unwrap_or_else(|| institution_id.to_string());
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
        "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, agreement_id, institution_id, institution_name, country) \
         VALUES ($1, $2, $3, '', $4, $5, $6, $7)")
        .bind(session_id).bind(budget_id).bind(user_id).bind(&agreement_id).bind(institution_id).bind(&institution_name).bind(country)
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    #[derive(sqlx::FromRow)]
    struct PendingSession {
        budget_id: Uuid,
        user_id: Uuid,
        requisition_id: String,
        institution_id: String,
        institution_name: String,
        country: String,
        status: String,
    }
    let session: PendingSession = sqlx::query_as(
        "SELECT budget_id, user_id, requisition_id, institution_id, institution_name, country, status FROM bank_link_sessions WHERE id = $1")
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
        // `provider_account_id` is globally UNIQUE, but a GoCardless account is
        // scoped to the ONE budget it was first linked into — the ON CONFLICT
        // below (for the non-reconciliation insert path) deliberately does not
        // rebind budget_id/user_id. Without this check, selecting an
        // already-linked account while linking into a DIFFERENT budget would
        // silently reactivate/update the OTHER budget's row while `complete`
        // reports success for THIS budget — the account would never appear in
        // this budget's list. Mirrors financial_connections::complete_link_session's
        // identical guard. Checked BEFORE the details fetch below (fail fast,
        // no wasted GoCardless call) and BEFORE the same-budget IBAN
        // reconciliation lookup further down — it does not conflict with that
        // lookup: reconciliation only ever matches a row already scoped to
        // `budget_id = $1` (this budget), so a cross-budget match here can
        // only ever be a genuine different-budget conflict.
        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(
                    %user_id, account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "gocardless account already linked to a different budget"
                );
                return Err((
                    StatusCode::CONFLICT,
                    "This bank account is already linked to a different budget. Disconnect it there first.".to_string(),
                ));
            }
        }

        // A failed details fetch (transient GoCardless outage/timeout) must
        // NOT be silently treated the same as "this account genuinely has no
        // IBAN" — the two look identical downstream (`iban = None`) and a
        // transient failure here would cause the IBAN reconciliation below to
        // silently produce a duplicate row instead of recognizing an existing
        // consent_expired account. Log which case actually happened so a
        // duplicate can be diagnosed after the fact.
        let details = match gocardless_get(&format!("accounts/{account_id}/details/")).await {
            Ok(d) => d,
            Err((status, msg)) => {
                tracing::warn!(
                    ?status, %msg, account_id,
                    "gocardless account details fetch failed (not a genuine no-IBAN account); \
                     proceeding without IBAN — reconciliation onto a consent_expired row will be skipped for this account"
                );
                serde_json::json!({})
            }
        };
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
                    institution_name = $6, \
                    status = 'active', disconnected_at = NULL, consent_expires_at = $7, updated_at = now() \
                 WHERE id = $1 RETURNING *")
                .bind(id).bind(account_id).bind(&session.requisition_id).bind(&display_name).bind(&last4)
                .bind(&session.institution_name)
                .bind(consent_expires_at)
                .fetch_one(pool).await.map_err(internal_error)?
        } else {
            sqlx::query_as::<_, crate::db::LinkedAccount>(
                "INSERT INTO linked_accounts \
                    (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                     institution_id, institution_name, display_name, last4, iban, country, \
                     status, consent_expires_at) \
                 VALUES ($1, $2, $3, 'gocardless', $4, $5, $6, $7, $8, $9, $10, $11, 'active', $12) \
                 ON CONFLICT (provider_account_id) DO UPDATE SET \
                    display_name = EXCLUDED.display_name, last4 = EXCLUDED.last4, \
                    institution_name = EXCLUDED.institution_name, \
                    status = 'active', disconnected_at = NULL, consent_expires_at = EXCLUDED.consent_expires_at, \
                    updated_at = now() \
                 RETURNING *")
                .bind(Uuid::new_v4()).bind(budget_id).bind(user_id)
                .bind(account_id).bind(&session.requisition_id)
                .bind(&session.institution_id).bind(&session.institution_name).bind(&display_name).bind(&last4).bind(&iban).bind(&session.country)
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

    // Standing ignore rules (#374): loaded once per sync.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

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
        let Some(tx_id) = tx_id else {
            tracing::warn!(
                account_id = %linked_account.id, raw_tx = %tx,
                "gocardless sync: dropping transaction with no transactionId/internalTransactionId"
            );
            continue;
        };
        let amount_str = tx.pointer("/transactionAmount/amount").and_then(|v| v.as_str()).unwrap_or("0");
        let Some(amount) = normalize_amount(amount_str) else {
            tracing::warn!(
                account_id = %linked_account.id, tx_id, amount_str,
                "gocardless sync: dropping transaction with a malformed amount"
            );
            continue;
        };
        let currency = match tx.pointer("/transactionAmount/currency").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                tracing::warn!(
                    account_id = %linked_account.id, tx_id,
                    "gocardless sync: transaction missing currency field; defaulting to EUR"
                );
                "EUR".to_string()
            }
        };
        let description = tx.get("remittanceInformationUnstructured").and_then(|v| v.as_str())
            .unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("bookingDate").and_then(|v| v.as_str())
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
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
            Ok(s) => s.flatten(),
            Err(e) => {
                // A genuine DB error here is NOT the same as "owner has no
                // subscriptions row" (not Pro) — falling through to the "not
                // Pro" branch below would actively mislead an on-call
                // investigation into thinking this is expected gating rather
                // than a query failure. Log the real error and skip this
                // account for this cycle (it will be retried next poll).
                tracing::error!(%e, account_id = %account.id, "gocardless poll: failed to check owner subscription status; skipping account this cycle");
                continue;
            }
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

    #[test]
    fn is_consent_expired_error_ignores_401_even_when_body_mentions_expired() {
        // A 401 means OUR bearer token is invalid/stale (internal auth glitch),
        // not that the end user's PSD2 consent has lapsed — must not be treated
        // as a consent-expiry signal even if the body happens to say "expired".
        let body = serde_json::json!({"summary": "Token is invalid or expired"});
        assert!(!is_consent_expired_error(reqwest::StatusCode::UNAUTHORIZED, &body));
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

    // --- #[ignore]+wiremock DB-integration tests, mirroring
    // financial_connections.rs's style. Test helper SHAPES are duplicated
    // here (module-private test helpers) rather than importing
    // financial_connections's private test fns, matching this codebase's
    // per-module test-helper convention.
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
            .bind(id).bind(format!("gc-{id}@test.example")).execute(db).await.unwrap();
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

    /// Mirrors `financial_connections.rs`'s `mk_linked_account` test helper, for
    /// an already-active `gocardless` row (used by tests that need a
    /// pre-existing linked account rather than exercising the full link flow).
    async fn mk_linked_account(db: &PgPool, budget_id: Uuid, user_id: Uuid, provider_account_id: &str) -> crate::db::LinkedAccount {
        sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'gocardless', $4, 'req_x') RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(provider_account_id)
            .fetch_one(db).await.unwrap()
    }

    /// Insert a pending `bank_link_sessions` row directly (bypassing
    /// `start_link_session`'s HTTP calls) and return its id — used by tests
    /// that only need to exercise `complete_link_session`.
    async fn mk_pending_session(db: &PgPool, budget_id: Uuid, user_id: Uuid, requisition_id: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, institution_id, institution_name, country, status) \
             VALUES ($1, $2, $3, $4, 'MONZO_MONZ_GB', 'Monzo', 'GB', 'pending')")
            .bind(id).bind(budget_id).bind(user_id).bind(requisition_id)
            .execute(db).await.unwrap();
        id
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

        Mock::given(method("GET")).and(path("/requisitions/req_1/"))
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
        assert_eq!(
            completed.accounts[0].institution_name.as_deref(), Some("Monzo"),
            "institution_name must be the bank's real display name, not the raw institution_id"
        );

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

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
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, institution_id, institution_name, country, status) \
             VALUES ($1, $2, $3, 'req_new', 'MONZO_MONZ_GB', 'Monzo', 'GB', 'pending')")
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

        let (provider_account_id, status, institution_name): (String, String, Option<String>) = sqlx::query_as(
            "SELECT provider_account_id, status, institution_name FROM linked_accounts WHERE id = $1")
            .bind(old_row_id).fetch_one(&db).await.unwrap();
        assert_eq!(provider_account_id, "acct_new");
        assert_eq!(status, "active");
        assert_eq!(institution_name.as_deref(), Some("Monzo"), "reconciliation must also refresh institution_name to the real bank name");

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

    // --- Cross-budget re-link guard (mirrors financial_connections.rs's
    // `complete_link_session_rejects_relink_to_a_different_budget`) ---------

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
        set_gc_env(&server);
        mount_token(&server).await;
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_relink").await;
        // Already linked (active) to budget A.
        let existing = mk_linked_account(&db, bid_a, uid, "acct_relink_1").await;

        let gc_ref = mk_pending_session(&db, bid_b, uid, "req_relink").await;
        Mock::given(method("GET")).and(path("/requisitions/req_relink/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_relink", "accounts": ["acct_relink_1"]})))
            .mount(&server).await;
        // The guard must fire BEFORE this is ever called.
        Mock::given(method("GET")).and(path("/accounts/acct_relink_1/details/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"account": {"iban": "GB00IRRELEVANT"}})))
            .expect(0)
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid_b, gc_ref).await;
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
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    // --- Closed-budget guard coverage (mirrors financial_connections.rs's
    // `sync_account_transactions_skips_closed_budget` and
    // `refresh_linked_account_rejects_closed_budget`) ------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_skips_closed_budget() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;
        // If the guard is broken and GoCardless gets called anyway, this GET
        // fires; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/accounts/acct_gc_closed_sync/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acct_gc_closed_sync").await;

        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = now() WHERE id = $1")
            .bind(bid)
            .execute(&db).await.unwrap();

        let result = sync_account_transactions(&db, &linked).await.unwrap();
        assert_eq!(result, 0, "a closed budget must import 0 transactions and never call GoCardless");
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_linked_account_rejects_closed_budget() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;
        // If the guard is broken, refresh_linked_account would call GoCardless's
        // transactions endpoint; `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/accounts/acct_gc_closed_refresh/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_closed_refresh").await;
        let linked = mk_linked_account(&db, bid, uid, "acct_gc_closed_refresh").await;

        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = now() WHERE id = $1")
            .bind(bid)
            .execute(&db).await.unwrap();

        let result = refresh_linked_account(&db, uid, bid, linked.id).await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::CONFLICT,
            "manual refresh on a closed budget must 409, not reach GoCardless"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    // --- complete_link_session anti-replay + double-completion coverage ----

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_session_belonging_to_another_user() {
        // Anti-replay: a `gc_ref` whose session row belongs to a DIFFERENT
        // (user_id, budget_id) than the caller's must be rejected — mirrors
        // Stripe's session-customer-mismatch check.
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let owner_uid = mk_user(&db).await;
        let owner_bid = mk_budget(&db, owner_uid).await;
        mk_pro_subscription(&db, owner_uid, "cus_gc_owner").await;
        let gc_ref = mk_pending_session(&db, owner_bid, owner_uid, "req_replay").await;

        let attacker_uid = mk_user(&db).await;
        let attacker_bid = mk_budget(&db, attacker_uid).await;
        mk_pro_subscription(&db, attacker_uid, "cus_gc_attacker").await;

        // No GoCardless call must be made at all if the ownership check
        // short-circuits correctly.
        Mock::given(method("GET")).and(path("/requisitions/req_replay/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_replay", "accounts": []})))
            .expect(0)
            .mount(&server).await;

        let result = complete_link_session(&db, attacker_uid, attacker_bid, gc_ref).await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::FORBIDDEN,
            "a session belonging to a different user/budget must be rejected"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner_uid).execute(&db).await.unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(attacker_uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_rejects_already_completed_session() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_double").await;
        let gc_ref = mk_pending_session(&db, bid, uid, "req_double").await;
        sqlx::query("UPDATE bank_link_sessions SET status = 'completed' WHERE id = $1")
            .bind(gc_ref).execute(&db).await.unwrap();

        // No GoCardless call must be made for an already-completed session.
        Mock::given(method("GET")).and(path("/requisitions/req_double/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_double", "accounts": []})))
            .expect(0)
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid, gc_ref).await;
        assert_eq!(
            result.unwrap_err().0,
            StatusCode::CONFLICT,
            "completing an already-completed session must 409, not re-run the link"
        );
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    // --- IBAN reconciliation: non-matching and missing IBAN -----------------
    // (only the matching case was previously tested, by
    // `complete_link_session_reconciles_onto_existing_consent_expired_row_by_iban`)

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_does_not_reconcile_when_iban_does_not_match() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_noniban").await;

        // Seed a consent_expired row for the same budget + institution, but a
        // DIFFERENT IBAN than what the new consent will report.
        let old_row_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, iban, status) \
             VALUES ($1, $2, $3, 'gocardless', 'acct_old_noniban', 'req_old_noniban', 'MONZO_MONZ_GB', 'GB00OLDIBAN00000000001', 'consent_expired')")
            .bind(old_row_id).bind(bid).bind(uid).execute(&db).await.unwrap();

        let gc_ref = mk_pending_session(&db, bid, uid, "req_noniban").await;
        Mock::given(method("GET")).and(path("/requisitions/req_noniban/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_noniban", "accounts": ["acct_new_noniban"]})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acct_new_noniban/details/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"account": {"iban": "GB99NEWIBAN00000000009", "ownerName": "Test User"}})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acct_new_noniban/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": [], "pending": []}})))
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid, gc_ref).await.expect("complete ok");
        assert_eq!(result.accounts.len(), 1);
        assert_ne!(result.accounts[0].id, old_row_id, "a non-matching IBAN must NOT reconcile onto the old row");

        // Both rows must now exist: the untouched old consent_expired row, and
        // a brand-new active row for the new account.
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert_eq!(total, 2, "a non-matching IBAN must insert a new row, not silently reuse the old one");

        let old_status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(old_row_id).fetch_one(&db).await.unwrap();
        assert_eq!(old_status, "consent_expired", "the old row must remain untouched");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_link_session_does_not_reconcile_when_iban_missing() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_noiban").await;

        // Seed a consent_expired row for the same budget + institution.
        let old_row_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, iban, status) \
             VALUES ($1, $2, $3, 'gocardless', 'acct_old_noiban', 'req_old_noiban', 'MONZO_MONZ_GB', 'GB00OLDIBAN00000000002', 'consent_expired')")
            .bind(old_row_id).bind(bid).bind(uid).execute(&db).await.unwrap();

        let gc_ref = mk_pending_session(&db, bid, uid, "req_noiban").await;
        Mock::given(method("GET")).and(path("/requisitions/req_noiban/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "req_noiban", "accounts": ["acct_new_noiban"]})))
            .mount(&server).await;
        // The bank genuinely has no IBAN on this account (e.g. a credit card).
        Mock::given(method("GET")).and(path("/accounts/acct_new_noiban/details/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"account": {"ownerName": "Test User"}})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acct_new_noiban/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": [], "pending": []}})))
            .mount(&server).await;

        let result = complete_link_session(&db, uid, bid, gc_ref).await.expect("complete ok");
        assert_eq!(result.accounts.len(), 1);
        assert_ne!(result.accounts[0].id, old_row_id, "a missing IBAN must NOT reconcile onto the old row");

        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE budget_id = $1")
            .bind(bid).fetch_one(&db).await.unwrap();
        assert_eq!(total, 2, "a missing IBAN must insert a new row, not silently reuse the old one");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    // --- poll_active_accounts coverage (previously zero tests) --------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn poll_active_accounts_only_syncs_active_accounts() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_poll_status").await;

        let active = mk_linked_account(&db, bid, uid, "acct_poll_active").await;
        let disconnected = mk_linked_account(&db, bid, uid, "acct_poll_disconnected").await;
        sqlx::query("UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now() WHERE id = $1")
            .bind(disconnected.id).execute(&db).await.unwrap();
        let consent_expired = mk_linked_account(&db, bid, uid, "acct_poll_consent_expired").await;
        sqlx::query("UPDATE linked_accounts SET status = 'consent_expired' WHERE id = $1")
            .bind(consent_expired.id).execute(&db).await.unwrap();

        Mock::given(method("GET")).and(path("/accounts/acct_poll_active/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(1)
            .mount(&server).await;
        // Non-active accounts must never be polled.
        Mock::given(method("GET")).and(path("/accounts/acct_poll_disconnected/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(0)
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acct_poll_consent_expired/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(0)
            .mount(&server).await;

        poll_active_accounts(&db).await;
        // wiremock verifies the `.expect(...)` calls above on `server`'s drop below.

        let synced_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(active.id).fetch_one(&db).await.unwrap();
        assert!(synced_at.is_some(), "the active account must have actually been synced");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn poll_active_accounts_skips_non_pro_owner() {
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // Deliberately NO subscriptions row for this owner -> not Pro.
        let linked = mk_linked_account(&db, bid, uid, "acct_poll_nonpro").await;
        assert_eq!(linked.status, "active");

        // The per-account Pro re-check must skip this account before ever
        // calling GoCardless; `.expect(0)` makes wiremock panic on drop if
        // the guard is broken.
        Mock::given(method("GET")).and(path("/accounts/acct_poll_nonpro/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(0)
            .mount(&server).await;

        poll_active_accounts(&db).await;
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        let synced_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(linked.id).fetch_one(&db).await.unwrap();
        assert!(synced_at.is_none(), "a non-Pro owner's account must not be synced");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn poll_active_accounts_continues_after_one_account_fails() {
        // One account's sync erroring (e.g. a GoCardless 500) must not stop
        // the loop from reaching the remaining accounts.
        let server = MockServer::start().await;
        set_gc_env(&server);
        mount_token(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_gc_poll_multi").await;

        let failing = mk_linked_account(&db, bid, uid, "acct_poll_failing").await;
        let healthy = mk_linked_account(&db, bid, uid, "acct_poll_healthy").await;

        Mock::given(method("GET")).and(path("/accounts/acct_poll_failing/transactions/"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({"summary": "Internal error"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/accounts/acct_poll_healthy/transactions/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transactions": {"booked": []}})))
            .expect(1)
            .mount(&server).await;

        poll_active_accounts(&db).await;
        // wiremock verifies `.expect(1)` on `server`'s drop below.

        let healthy_synced: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(healthy.id).fetch_one(&db).await.unwrap();
        assert!(healthy_synced.is_some(), "the healthy account must still be synced despite the other account's failure");

        let failing_synced: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_synced_at FROM linked_accounts WHERE id = $1")
            .bind(failing.id).fetch_one(&db).await.unwrap();
        assert!(failing_synced.is_none(), "the failing account's last_synced_at must not be touched on a hard error");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_gc_env();
    }
}
