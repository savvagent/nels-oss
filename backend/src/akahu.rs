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

#[derive(Debug, Serialize)]
pub struct AkahuLinkSessionResponse {
    pub redirect_url: String,
    pub reference: Uuid,
}

/// Start an Akahu OAuth2 consent flow ("Akahu Connect"): persist a pending
/// `bank_link_sessions` row keyed by the session id, sent to Akahu as the
/// OAuth `state` parameter. The actual anti-CSRF/anti-replay protection is
/// NOT a round-trip `state` comparison (Akahu's returned `state` isn't read
/// back anywhere in this module — that wiring is REST-layer/Task 4's job);
/// it's `complete_consent_session`'s ownership check against this pending
/// row's own `budget_id`/`user_id`, same as `gocardless`'s and `basiq`'s
/// pending-session pattern. Pro-gated, Edit-or-Owner-gated, closed-budget
/// rejected — mirrors `gocardless::start_link_session`/`basiq::create_consent_session`
/// except no institution_id (spec Assumption 3).
pub async fn create_consent_session(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<AkahuLinkSessionResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
///
/// The token is a live bank-data credential, not just an identifier — it is
/// ENCRYPTED before being persisted into `provider_ref` (code review fix,
/// #323: Copilot correctly flagged storing it in plaintext as increasing the
/// blast radius of a DB leak), reusing the same `crypto::SecretCipher`
/// AES-256-GCM scheme this codebase already uses for TOTP secrets. Every
/// reader of an Akahu `provider_ref` (`sync_account_transactions`,
/// `refresh_linked_account`) must decrypt it before using it as a bearer
/// token; `disconnect_linked_account` never needs to (it's local-status-only
/// and never calls Akahu's API at all).
pub async fn complete_consent_session(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    budget_id: Uuid,
    akahu_ref: Uuid,
    code: &str,
) -> Result<crate::financial_connections::ListLinkedAccountsResponse, (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
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
    let encrypted_token = cipher.encrypt(&token.access_token)
        .map_err(|e| internal_error(format!("akahu token encrypt: {e}")))?;

    let accounts_body = akahu_get("accounts", &token.access_token).await?;
    // A missing `items` key (vs. a present-but-empty array) means the
    // response didn't have the shape this code expects — log it rather than
    // silently treating it identically to "this user genuinely has zero
    // accounts" (code review fix, #323), mirroring the same fix in basiq.rs.
    if accounts_body.get("items").and_then(|v| v.as_array()).is_none() {
        tracing::warn!(%user_id, response = %accounts_body, "akahu accounts response missing expected 'items' array");
    }
    let accounts = accounts_body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // `GET accounts` returns EVERY account this Akahu access token can see —
    // per spec Assumption 5, one Akahu token legitimately covers a user's
    // accounts across MULTIPLE institutions/consent events, not just the one
    // this session just authorized. A cross-budget conflict on ONE
    // pre-existing, unrelated account must therefore SKIP that account (log
    // a warning) and continue processing the rest, rather than aborting the
    // whole function — mirrors the identical fix in basiq.rs (code review
    // finding, #323): the original "abort entirely" behavior meant an old
    // account linked to a completely different budget could silently block
    // this session's actually-relevant new account(s) from ever completing.
    // If literally every account in this batch conflicts, `results` stays
    // empty and the CONFLICT error is still returned below.
    let mut results = Vec::with_capacity(accounts.len());
    let mut had_conflict = false;
    for acc in &accounts {
        let account_id = match acc.get("_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let institution_name = acc.pointer("/connection/name").and_then(|v| v.as_str()).map(str::to_string);
        let display_name = acc.get("name").and_then(|v| v.as_str()).map(str::to_string);
        // Akahu's `formatted_account` is PUNCTUATED (e.g. "12-3456-7890123-00"),
        // unlike Basiq's/GoCardless's plain digit strings — reversing and
        // taking the raw last 4 characters (the pattern those two modules use)
        // would grab dashes/partial segments instead of real digits (code
        // review fix, #323). Strip non-digit characters first so "last 4"
        // always means the last 4 actual account-number digits.
        let last4 = acc.get("formatted_account").and_then(|v| v.as_str())
            .map(|s| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
            .map(|digits| digits.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());

        let existing_budget_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = $1")
            .bind(&account_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        if let Some(existing_bid) = existing_budget_id {
            if existing_bid != budget_id {
                tracing::warn!(%user_id, account_id, existing_budget_id = %existing_bid, requested_budget_id = %budget_id,
                    "akahu account already linked to a different budget — skipping this account, not aborting the whole session");
                had_conflict = true;
                continue;
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
            .bind(&account_id).bind(&encrypted_token).bind(&institution_name).bind(&display_name).bind(&last4)
            .fetch_one(pool).await.map_err(internal_error)?;

        if let Err((status, msg)) = sync_account_transactions(pool, cipher, &row).await {
            tracing::warn!(?status, %msg, account_id = %row.id, "initial akahu sync failed; will retry via webhook/manual refresh");
        }
        results.push(row.into());
    }

    if results.is_empty() && had_conflict {
        return Err((StatusCode::CONFLICT, "This bank account is already linked to a different budget. Disconnect it there first.".to_string()));
    }

    sqlx::query("UPDATE bank_link_sessions SET status = 'completed' WHERE id = $1")
        .bind(akahu_ref).execute(pool).await.map_err(internal_error)?;

    Ok(crate::financial_connections::ListLinkedAccountsResponse { accounts: results })
}

/// Pull transactions for one linked Akahu account and idempotently insert new
/// rows. `currency = 'NZD'` hardcoded (spec Assumption 8 — Akahu is NZ-only in
/// this app). `provider_ref` holds the shared access token ENCRYPTED at rest
/// (code review fix, #323) — decrypted here before use as the bearer token.
pub async fn sync_account_transactions(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
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

    // Standing ignore rules (#374): loaded once per sync.
    let ignore_rules = crate::ignore_rules::fetch_ignore_rules(pool, linked_account.budget_id).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, budget_id = %linked_account.budget_id, "failed to fetch ignore rules; continuing without them");
        Vec::new()
    });

    let bearer = cipher.decrypt(&linked_account.provider_ref)
        .map_err(|e| internal_error(format!("akahu token decrypt: {e}")))?;
    let path = format!("accounts/{}/transactions", linked_account.provider_account_id);
    let body = akahu_get(&path, &bearer).await?;
    let rows = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut imported: u64 = 0;
    for tx in &rows {
        let tx_id = match tx.get("_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        // A missing/non-numeric `amount` must be DROPPED, not silently
        // imported as a fabricated $0 transaction (code review fix, #323) —
        // matches gocardless.rs's/basiq.rs's identical "malformed amount ->
        // warn + skip" convention for the same operation; a $0 phantom
        // transaction is a worse failure mode than one that's simply absent.
        let Some(amount) = tx.get("amount").and_then(|v| v.as_f64()) else {
            tracing::warn!(account_id = %linked_account.id, tx_id, raw_tx = %tx, "akahu sync: dropping transaction with a missing or non-numeric amount");
            continue;
        };
        let amount = normalize_amount(amount);
        let description = tx.get("description").and_then(|v| v.as_str()).unwrap_or("Imported transaction").to_string();
        let transacted_at = tx.get("date").and_then(|v| v.as_str())
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
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'NZD', $9, 'imported', 'needs_review') \
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

/// Manual refresh: Pro-gated, synchronous re-sync — same shape as Basiq's.
pub async fn refresh_linked_account(
    pool: &PgPool, cipher: &crate::crypto::SecretCipher, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_edit_or_owner(pool, user_id, budget_id).await?;
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
    ensure_not_closed(pool, budget_id).await?;

    let linked = sqlx::query_as::<_, crate::db::LinkedAccount>(
        "SELECT * FROM linked_accounts WHERE id = $1 AND budget_id = $2 AND provider = 'akahu' AND status = 'active'")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))?;

    sync_account_transactions(pool, cipher, &linked).await?;
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
        // Public, unauthenticated route — a rejected signature must be
        // visible in logs, not vanish silently (code review fix, #323).
        tracing::warn!("invalid akahu webhook signature");
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "invalid akahu webhook payload");
            return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string()));
        }
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
                if let Err((status, msg)) = sync_account_transactions(&state.db, &state.cipher, &row).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed test key (matches `financial_connections.rs`'s/`bank_linking.rs`'s
    /// own `test_state`/`test_cipher` helpers) — Akahu tokens are encrypted at
    /// rest via `crypto::SecretCipher` (code review fix, #323), so any test
    /// that persists/reads a `linked_accounts.provider_ref` for an `akahu` row
    /// needs one.
    fn test_cipher() -> crate::crypto::SecretCipher {
        crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()
    }

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
        // 'trialing' (not 'active') so `entitlement::resolve` maps the owner to
        // `Tier::Pro` unconditionally in tests, without needing a Pro price env
        // var or `#[serial]`, unlike 'active' which requires a Pro price to be
        // configured to resolve above Basic.
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
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
        // `provider_ref` is ENCRYPTED at rest for akahu rows (code review fix,
        // #323) — encrypt the given plaintext token with the same fixed
        // `test_cipher()` key every test in this module uses, so any test
        // that goes on to call sync_account_transactions (which decrypts it)
        // gets back the original plaintext.
        let encrypted = test_cipher().encrypt(provider_ref).unwrap();
        sqlx::query_as::<_, crate::db::LinkedAccount>(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'akahu', $4, $5) RETURNING *")
            .bind(Uuid::new_v4()).bind(budget_id).bind(user_id).bind(provider_account_id).bind(&encrypted)
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

        let completed = complete_consent_session(&db, &test_cipher(), uid, bid, started.reference, "auth_code_1").await.expect("complete ok");
        assert_eq!(completed.accounts.len(), 1);
        assert_eq!(completed.accounts[0].provider, "akahu");
        // Regression assertion (code review fix, #323): "12-3456-7890123-00"
        // is the exact punctuated shape that broke the naive raw-last-4-chars
        // trick (it grabbed "3-00" — a dash plus one digit — instead of the
        // real last 4 account-number digits). Digits-only, last 4 = "2300".
        assert_eq!(completed.accounts[0].last4.as_deref(), Some("2300"), "last4 must strip punctuation, not grab it");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_akahu_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_consent_session_skips_a_cross_budget_conflict_instead_of_aborting() {
        // Regression test (code review finding, #323): `GET accounts` returns
        // EVERY account this Akahu access token can see — per spec Assumption
        // 5, one token legitimately spans multiple institutions/consent
        // events for the same Nels user, not just the one this session just
        // authorized. A cross-budget conflict on ONE pre-existing, unrelated
        // account must be skipped, not abort the whole completion — mirrors
        // the identical basiq.rs regression test.
        let server = MockServer::start().await;
        set_akahu_env(&server);
        Mock::given(method("POST")).and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "user_tok_multi"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_akahu_multi").await;
        // Already linked to budget A from an earlier, unrelated session,
        // sharing what will be the SAME Akahu token once budget B's session
        // completes (Akahu's token is user-wide, not per-session).
        mk_linked_account(&db, bid_a, uid, "acc_old_a", "user_tok_multi").await;

        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
             VALUES ($1, $2, $3, '', 'akahu', 'pending')")
            .bind(Uuid::new_v4()).bind(bid_b).bind(uid).execute(&db).await.unwrap();
        let akahu_ref: Uuid = sqlx::query_scalar(
            "SELECT id FROM bank_link_sessions WHERE budget_id = $1 AND provider = 'akahu'")
            .bind(bid_b).fetch_one(&db).await.unwrap();

        // Akahu's account list now contains BOTH the old budget-A account and
        // a brand-new budget-B account (this is the real-world shape: the
        // endpoint is scoped to the TOKEN, which spans every consent event).
        Mock::given(method("GET")).and(path("/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "items": [
                    {"_id": "acc_old_a", "name": "Old", "formatted_account": "11-1111-1111111-00", "connection": {"name": "ASB Bank"}},
                    {"_id": "acc_new_b", "name": "New", "formatted_account": "22-2222-2222222-00", "connection": {"name": "BNZ"}}
                ]
            })))
            .mount(&server).await;

        let completed = complete_consent_session(&db, &test_cipher(), uid, bid_b, akahu_ref, "auth_code_multi").await
            .expect("completion must succeed despite an unrelated cross-budget account sharing the same Akahu token");
        assert_eq!(completed.accounts.len(), 1, "only the new budget-B account should be returned");
        assert_eq!(completed.accounts[0].provider, "akahu");

        let new_account_budget: Uuid = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = 'acc_new_b'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(new_account_budget, bid_b);

        let old_account_budget: Uuid = sqlx::query_scalar(
            "SELECT budget_id FROM linked_accounts WHERE provider_account_id = 'acc_old_a'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(old_account_budget, bid_a, "the old account must be completely untouched — still under budget A");

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

        let first = sync_account_transactions(&db, &test_cipher(), &linked).await.unwrap();
        assert_eq!(first, 1);
        let second = sync_account_transactions(&db, &test_cipher(), &linked).await.unwrap();
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
        let linked = mk_linked_account(&db, bid, uid, "acc_history_1", "user_tok_history").await;
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, amount, transaction_date, description, external_account_id, provider_transaction_id, currency) \
             VALUES ($1, $2, 42.0, now(), 'Kept transaction', $3, 'akahu_tx_history_1', 'NZD')")
            .bind(Uuid::new_v4()).bind(bid).bind(linked.id).execute(&db).await.unwrap();

        disconnect_linked_account(&db, uid, bid, linked.id).await.expect("disconnect ok");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'akahu_tx_history_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1, "disconnect must not delete previously-imported transactions");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn sync_account_transactions_drops_transaction_with_missing_amount() {
        // Regression test (code review fix, #323): a transaction with no
        // `amount` field at all must be dropped with a warning, not imported
        // as a fabricated $0.00 row.
        let server = MockServer::start().await;
        set_akahu_env(&server);
        Mock::given(method("GET")).and(path("/accounts/acc_noamount_1/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "items": [{"_id": "akahu_tx_noamount_1", "description": "Missing amount", "date": "2026-07-01T00:00:00Z"}]
            })))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        let linked = mk_linked_account(&db, bid, uid, "acc_noamount_1", "user_tok_noamount").await;

        let imported = sync_account_transactions(&db, &test_cipher(), &linked).await.unwrap();
        assert_eq!(imported, 0, "a transaction with a missing amount must be dropped, not imported as $0.00");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'akahu_tx_noamount_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 0);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_akahu_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_consent_session_errors_when_every_account_conflicts() {
        // Mirrors basiq.rs's identical test: if EVERY account in the batch is
        // an existing cross-budget conflict, the session must surface an
        // explicit 409 rather than silently "succeeding" with zero accounts.
        let server = MockServer::start().await;
        set_akahu_env(&server);
        Mock::given(method("POST")).and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "user_tok_allconflict"})))
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid_a = mk_budget(&db, uid).await;
        let bid_b = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_akahu_allconflict").await;
        mk_linked_account(&db, bid_a, uid, "acc_only_a_akahu", "user_tok_allconflict").await;

        sqlx::query(
            "INSERT INTO bank_link_sessions (id, budget_id, user_id, requisition_id, provider, status) \
             VALUES ($1, $2, $3, '', 'akahu', 'pending')")
            .bind(Uuid::new_v4()).bind(bid_b).bind(uid).execute(&db).await.unwrap();
        let akahu_ref: Uuid = sqlx::query_scalar(
            "SELECT id FROM bank_link_sessions WHERE budget_id = $1 AND provider = 'akahu'")
            .bind(bid_b).fetch_one(&db).await.unwrap();

        Mock::given(method("GET")).and(path("/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "items": [{"_id": "acc_only_a_akahu", "name": "Old", "formatted_account": "11-1111-1111111-00", "connection": {"name": "ASB Bank"}}]
            })))
            .mount(&server).await;

        let result = complete_consent_session(&db, &test_cipher(), uid, bid_b, akahu_ref, "auth_code_allconflict").await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_akahu_env();
    }

    // --- webhook handler coverage -------------------------------------
    //
    // Previously zero test coverage (pr-test-analyzer review finding, #323):
    // mirrors basiq.rs's webhook test pattern exactly.

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

    fn akahu_webhook_body(event_type: &str, account_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": event_type,
            "item": { "resource": { "_account": account_id } },
        })).unwrap()
    }

    fn test_state(db: PgPool) -> crate::auth::AppState {
        crate::auth::AppState {
            db,
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    fn signed_akahu_headers(secret: &str, body: &[u8]) -> axum::http::HeaderMap {
        let t = chrono::Utc::now().timestamp();
        let sig = sign(secret, t, body);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-akahu-signature", axum::http::HeaderValue::from_str(&sig).unwrap());
        headers
    }

    fn set_akahu_webhook_secret(secret: &str) {
        std::env::set_var("AKAHU_WEBHOOK_SECRET", secret);
    }
    fn clear_akahu_webhook_secret() {
        std::env::remove_var("AKAHU_WEBHOOK_SECRET");
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_rejects_invalid_signature() {
        set_akahu_webhook_secret("whsec_akahu_test_invalid");
        let db = test_pool().await;
        let body = akahu_webhook_body("TRANSACTION_CREATED", "acc_x");
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-akahu-signature", axum::http::HeaderValue::from_static("t=1,v1=deadbeef"));
        let state = test_state(db);

        let result = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);

        clear_akahu_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_skips_sync_for_non_pro_owner() {
        const SECRET: &str = "whsec_akahu_test_nonpro";
        let server = MockServer::start().await;
        set_akahu_env(&server);
        set_akahu_webhook_secret(SECRET);
        // If the Pro-gate is broken and sync runs anyway, this GET fires;
        // `.expect(0)` makes wiremock panic on drop.
        Mock::given(method("GET")).and(path("/accounts/acc_wh_nonpro_1/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true, "items": []})))
            .expect(0)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        // No subscriptions row at all -> not Pro.
        let linked = mk_linked_account(&db, bid, uid, "acc_wh_nonpro_1", "user_tok_wh_nonpro").await;
        assert_eq!(linked.status, "active");

        let body = akahu_webhook_body("TRANSACTION_CREATED", "acc_wh_nonpro_1");
        let headers = signed_akahu_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await
            .expect("webhook must return Ok even for a guarded no-op");
        assert_eq!(status, StatusCode::OK);
        // wiremock verifies `.expect(0)` on `server`'s drop below.

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_akahu_env();
        clear_akahu_webhook_secret();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn webhook_syncs_active_pro_owned_account_on_transaction_created() {
        const SECRET: &str = "whsec_akahu_test_syncs";
        let server = MockServer::start().await;
        set_akahu_env(&server);
        set_akahu_webhook_secret(SECRET);

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let bid = mk_budget(&db, uid).await;
        mk_pro_subscription(&db, uid, "cus_akahu_wh_sync").await;
        let linked = mk_linked_account(&db, bid, uid, "acc_wh_sync_1", "user_tok_wh_sync").await;

        Mock::given(method("GET")).and(path("/accounts/acc_wh_sync_1/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "items": [{"_id": "akahu_tx_wh_1", "amount": -10.0, "description": "Webhook synced", "date": "2026-07-01T00:00:00Z"}]
            })))
            .expect(1)
            .mount(&server).await;

        let body = akahu_webhook_body("TRANSACTION_CREATED", "acc_wh_sync_1");
        let headers = signed_akahu_headers(SECRET, &body);
        let state = test_state(db.clone());

        let status = webhook(axum::extract::State(state), headers, axum::body::Bytes::from(body)).await
            .expect("webhook ok");
        assert_eq!(status, StatusCode::OK);

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE provider_transaction_id = 'akahu_tx_wh_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(count, 1, "webhook must have triggered a real sync for an active, Pro-owned account");
        let _ = linked;

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_akahu_env();
        clear_akahu_webhook_secret();
    }
}
