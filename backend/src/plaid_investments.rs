//! Retirement asset linking — Plaid Investments (nels#468).
//!
//! Lets a US Pro subscriber link investment accounts so their balances and
//! holdings flow into the retirement balance sheet (`assets`, #464), the
//! projection engine (#467), and the dashboard (#469).
//!
//! # Why a dedicated integrating surface, deliberately parallel to `plaid.rs`
//!
//! The cash bank-link flow (`plaid.rs`, #321) links **budget-scoped,
//! transaction-producing** accounts in Canada via the `transactions` product.
//! Investments are the opposite on every axis that matters:
//!
//! - **Budget-less**: `assets` is `user_id`-scoped by design (§20), so the link
//!   routes here carry NO `:budget_id` and the `linked_accounts` rows they
//!   create have `budget_id = NULL`. A budget-param route would let a budget
//!   collaborator reach a shareholder's retirement accounts, precisely the
//!   share edge §20 exists to remove.
//! - **Different Plaid product**: `products = ["investments"]`, US-only
//!   `country_codes = ["US"]`, and a separate access token from the cash Item.
//! - **Different write target**: holdings/securities/balance go to the
//!   `securities`/`asset_holdings`/`asset_balance_history` tables, NOT
//!   `transactions`.
//!
//! The two surfaces share only the reusable Plaid plumbing ([`plaid::plaid_post`],
//! [`plaid::plaid_api_base`], the `PLAID_API_BASE` seam dart) — which is why
//! those helpers are `pub(crate)`. The cash flow is untouched.
//!
//! # The per-account classification step (approved decision, #468)
//!
//! `assets.asset_type`/`tax_treatment` are NOT NULL and §20/§21 forbid deriving
//! them from a vendor payload. So link **completion** returns the newly created
//! investment `linked_accounts` rows but writes NO `assets` row; a **second,
//! required step** (`investments/classify`) takes the user's enum choices per
//! account and only then creates the assets and performs the initial holdings
//! sync. Nels never fabricates a fact about the user.
//!
//! # Poll-only (no webhook)
//!
//! Plaid Investments has no reliable per-account push we depend on for v1; a
//! scheduled job polls active investment accounts (offsetting the
//! `GOCARDLESS_POLL_INTERVAL_HOURS` precedent, separated since its cadence is
//! independent). Manual refresh is synchronous — Plaid has no "please check
//! now" for holdings, so the refresh awaits `/investments/holdings/get`
//! directly (mirroring `plaid::refresh_linked_account`'s synchronous shape).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::internal_error;
use crate::plaid::plaid_post;
use crate::{assets, entitlement::Tier};

/// How often the poll job syncs active investment accounts (hours). Default 8,
/// matching GoCardless's cadence and staying comfortably inside Plaid's
/// per-account rate guidance. `.max(1)` mirrors the GoCardless interval clamp
/// so an explicit `0` can't panic `tokio::time::interval`.
pub fn investments_poll_interval_hours() -> u64 {
    crate::plaid::env_opt("INVESTMENTS_POLL_INTERVAL_HOURS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
        .max(1)
}

/// A `linked_accounts` row as the investments surface reads it.
///
/// This deliberately is NOT `db::LinkedAccount`, whose `budget_id` is a non-null
/// `Uuid` — investment rows have `budget_id = NULL` by design (§20), so reading
/// them through the cash struct would fail the decode. The module owns its own
/// read shape.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InvestmentLinkedAccount {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub provider_account_id: String,
    pub provider_ref: String,
    pub institution_name: Option<String>,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub status: String, // 'active' | 'disconnected'
    pub plaid_item_id: Option<String>,
    pub is_investments: bool,
}

#[derive(Debug, Serialize)]
pub struct InvestmentLinkTokenResponse {
    pub link_token: String,
    pub session_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct InvestmentCompleteRequest {
    pub session_id: Uuid,
    pub public_token: String,
}

/// One investment account returned after completion, awaiting classification.
#[derive(Debug, Serialize)]
pub struct InvestmentAccountPending {
    pub linked_account_id: Uuid,
    pub display_name: Option<String>,
    pub last4: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InvestmentAccountsResponse {
    pub accounts: Vec<InvestmentAccountPending>,
}

/// One classification entry: which linked account owns this person's money, and
/// the two enum axes the user supplies (never derived from the vendor payload).
#[derive(Debug, Deserialize)]
pub struct InvestmentClassification {
    pub linked_account_id: Uuid,
    pub asset_type: assets::AssetType,
    pub tax_treatment: assets::TaxTreatment,
}

#[derive(Debug, Deserialize)]
pub struct InvestmentClassifyRequest {
    pub session_id: Uuid,
    pub classifications: Vec<InvestmentClassification>,
}

/// The count of newly synced holdings after a refresh, plus the asset id.
#[derive(Debug, Serialize)]
pub struct InvestmentRefreshResponse {
    pub asset_id: Uuid,
    pub holdings: u64,
}

/// Create a Plaid Investments Link token for `user_id` and an
/// `investment_link_sessions` row to anti-replay-bind the eventual
/// public_token exchange to this specific user_id.
///
/// Pro-gated via the CALLER's own tier (`require_caller_tier`), matching the
/// `GET/PUT /assets` precedent — no budget is in scope.
pub async fn create_investment_link_token(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<InvestmentLinkTokenResponse, (StatusCode, String)> {
    crate::access::require_caller_tier(pool, user_id, Tier::Pro).await?;

    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO investment_link_sessions (id, user_id) VALUES ($1, $2)")
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(internal_error)?;

    let body = serde_json::json!({
        "client_name": "Nels",
        "language": "en",
        "country_codes": ["US"],
        "user": { "client_user_id": user_id.to_string() },
        "products": ["investments"],
    });
    let resp = plaid_post("link/token/create", body).await?;
    let link_token = resp
        .get("link_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid link/token/create missing link_token"))?
        .to_string();
    Ok(InvestmentLinkTokenResponse {
        link_token,
        session_id,
    })
}

/// Exchange a Plaid `public_token` for investment accounts and persist them as
/// budget-less `linked_accounts` rows (`budget_id IS NULL`,
/// `is_investments = TRUE`), after atomically claiming the session.
///
/// Returns the created accounts so the caller can drive the per-account
/// classification step. Writes NO `assets` row — that is the classify step's job.
pub async fn complete_investment_link(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    session_id: Uuid,
    public_token: &str,
) -> Result<InvestmentAccountsResponse, (StatusCode, String)> {
    crate::access::require_caller_tier(pool, user_id, Tier::Pro).await?;

    // Atomic completion claim (mirrors plaid::complete_link_session) — a raw
    // public_token is single-use and carries no user binding, so the session is
    // what closes the TOCTOU double-exchange race.
    let claim = sqlx::query(
        "UPDATE investment_link_sessions SET status = 'completed' WHERE id = $1 AND user_id = $2 AND status = 'pending'",
    )
    .bind(session_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(internal_error)?;
    if claim.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "This link session is not pending for you".to_string(),
        ));
    }

    let exchange = plaid_post(
        "item/public_token/exchange",
        serde_json::json!({ "public_token": public_token }),
    )
    .await?;
    let access_token = exchange
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing access_token"))?
        .to_string();
    let item_id = exchange
        .get("item_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| internal_error("plaid exchange missing item_id"))?
        .to_string();

    // The access token is a live credential — encrypt before persisting.
    let encrypted_token = cipher
        .encrypt(&access_token)
        .map_err(|e| internal_error(format!("plaid token encrypt: {e}")))?;

    let accounts_resp = plaid_post(
        "accounts/get",
        serde_json::json!({ "access_token": access_token }),
    )
    .await?;
    let institution_id = accounts_resp
        .pointer("/item/institution_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let institution_name = match &institution_id {
        Some(iid) => match plaid_post(
            "institutions/get_by_id",
            serde_json::json!({ "institution_id": iid, "country_codes": ["US"] }),
        )
        .await
        {
            Ok(inst) => inst
                .pointer("/institution/name")
                .and_then(|n| n.as_str())
                .map(str::to_string),
            Err((status, msg)) => {
                tracing::warn!(
                    ?status, %msg, institution_id = %iid,
                    "plaid institutions/get_by_id failed; proceeding without institution_name"
                );
                None
            }
        },
        None => None,
    };

    let accounts = accounts_resp
        .get("accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut results = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let account_id = match acc.get("account_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let display_name = acc.get("name").and_then(|v| v.as_str()).map(str::to_string);
        let last4 = acc.get("mask").and_then(|v| v.as_str()).map(str::to_string);

        let row = sqlx::query_as::<_, InvestmentLinkedAccount>(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, institution_name, display_name, last4, country, \
                 plaid_item_id, is_investments) \
             VALUES ($1, NULL, $2, 'plaid', $3, $4, $5, $6, $7, $8, 'US', $9, TRUE) \
             ON CONFLICT (provider_account_id) DO UPDATE SET \
                 provider_ref = EXCLUDED.provider_ref, plaid_item_id = EXCLUDED.plaid_item_id, \
                 institution_name = EXCLUDED.institution_name, display_name = EXCLUDED.display_name, \
                 last4 = EXCLUDED.last4, status = 'active', disconnected_at = NULL, updated_at = now() \
             RETURNING id, user_id, budget_id, provider_account_id, provider_ref, \
                 institution_name, display_name, last4, status, plaid_item_id, is_investments",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(&account_id)
        .bind(&encrypted_token)
        .bind(&institution_id)
        .bind(&institution_name)
        .bind(&display_name)
        .bind(&last4)
        .bind(&item_id)
        .fetch_one(pool)
        .await
        .map_err(internal_error)?;

        results.push(InvestmentAccountPending {
            linked_account_id: row.id,
            display_name: row.display_name,
            last4: row.last4,
        });
    }

    Ok(InvestmentAccountsResponse { accounts: results })
}

/// The user-supplied classification step: for each pending investment account,
/// create the `assets` row (with the caller-supplied `asset_type`/
/// `tax_treatment`, never derived from the vendor payload) and run the initial
/// holdings sync.
///
/// Pro-gated. Returns the created asset ids. A `linked_account_id` that is not
/// one of this user's investment accounts is a 403 (never reveals whether it
/// exists) — it must be a budget-less (`is_investments`) row.
pub async fn classify_investment_accounts(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    req: InvestmentClassifyRequest,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    crate::access::require_caller_tier(pool, user_id, Tier::Pro).await?;

    // Verify the session owning this classification.
    let session_user: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM investment_link_sessions WHERE id = $1 AND status = 'completed'",
    )
    .bind(req.session_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    match session_user {
        Some(uid) if uid == user_id => {}
        Some(_) => {
            tracing::warn!(user_id = %user_id, session_id = %req.session_id, "investment classify session user mismatch — possible replay attempt");
            return Err((
                StatusCode::FORBIDDEN,
                "This link session does not belong to you".to_string(),
            ));
        }
        None => {
            return Err((StatusCode::NOT_FOUND, "Link session not found".to_string()));
        }
    }

    let mut asset_ids = Vec::with_capacity(req.classifications.len());
    for cls in req.classifications {
        // The account must be THIS user's budget-less investment row.
        let account: InvestmentLinkedAccount = sqlx::query_as(
            "SELECT id, user_id, budget_id, provider_account_id, provider_ref, \
                    institution_name, display_name, last4, status, plaid_item_id, is_investments \
             FROM linked_accounts WHERE id = $1 AND user_id = $2 AND is_investments",
        )
        .bind(cls.linked_account_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::FORBIDDEN,
            "This investment account does not belong to you".to_string(),
        ))?;

        // Idempotent: if an asset already exists for this account, return it — creating
        // a second would violate the unique implied asset-per-linked-account invariant.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM assets WHERE user_id = $1 AND linked_account_id = $2",
        )
        .bind(user_id)
        .bind(account.id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error)?;
        if let Some(existing) = existing {
            asset_ids.push(existing);
            continue;
        }

        let name = account
            .display_name
            .as_deref()
            .unwrap_or("Investment account");
        let asset = assets::insert_asset(
            pool,
            user_id,
            None,
            name,
            cls.asset_type,
            cls.tax_treatment,
            Some(account.id),
            None,
            "USD",
        )
        .await
        .map_err(internal_error)?;

        // Initial holdings sync — best-effort; a freshness problem here surfaces
        // on the next poll/manual refresh, exactly like the cash plaid initial sync.
        if let Err((status, msg)) = sync_investment_holdings(pool, cipher, &account, asset.id).await
        {
            tracing::warn!(?status, %msg, account_id = %account.id, "initial investment holdings sync failed; will retry via poll/manual refresh");
        }
        asset_ids.push(asset.id);
    }
    Ok(asset_ids)
}

/// List the caller's investment (`is_investments`) linked accounts.
pub async fn list_investment_accounts(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<InvestmentLinkedAccount>, (StatusCode, String)> {
    crate::access::require_caller_tier(pool, user_id, Tier::Pro).await?;
    sqlx::query_as(
        "SELECT id, user_id, budget_id, provider_account_id, provider_ref, \
                institution_name, display_name, last4, status, plaid_item_id, is_investments \
         FROM linked_accounts WHERE user_id = $1 AND is_investments ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)
}

/// Synchronous refresh of one investment account: pull `/investments/holdings`
/// for it and write securities/holdings/reported balance. Plaid has no "check
/// now" endpoint separate from the holdings read, so this awaits the sync
/// directly (mirroring `plaid::refresh_linked_account`'s sync shape).
pub async fn refresh_investment_account(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    account_id: Uuid,
) -> Result<InvestmentRefreshResponse, (StatusCode, String)> {
    crate::access::require_caller_tier(pool, user_id, Tier::Pro).await?;

    let account: InvestmentLinkedAccount = sqlx::query_as(
        "SELECT id, user_id, budget_id, provider_account_id, provider_ref, \
                institution_name, display_name, last4, status, plaid_item_id, is_investments \
         FROM linked_accounts WHERE id = $1 AND user_id = $2 AND is_investments AND status = 'active'",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Investment account not found".to_string()))?;

    let asset_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM assets WHERE user_id = $1 AND linked_account_id = $2 AND status = 'active'",
    )
    .bind(user_id)
    .bind(account.id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    let asset_id = asset_id.ok_or((
        StatusCode::CONFLICT,
        "This investment account has not been classified yet. Complete classification first."
            .to_string(),
    ))?;

    let holdings = sync_investment_holdings(pool, cipher, &account, asset_id).await?;
    Ok(InvestmentRefreshResponse { asset_id, holdings })
}

/// Disconnect an investment account: call Plaid `/item/remove` (Item-wide) FIRST,
/// then flip every `linked_accounts` row sharing the Item to 'disconnected' and
/// close its `assets` row. Not Pro-gated — a lapsed user must still be able to
/// remove their own linked data (§14 precedent).
pub async fn disconnect_investment_account(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
    account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let account: InvestmentLinkedAccount = sqlx::query_as(
        "SELECT id, user_id, budget_id, provider_account_id, provider_ref, \
                institution_name, display_name, last4, status, plaid_item_id, is_investments \
         FROM linked_accounts WHERE id = $1 AND user_id = $2 AND is_investments",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?
    .ok_or((
        StatusCode::NOT_FOUND,
        "Investment account not found".to_string(),
    ))?;

    // Decrypt the access token to remove the Item server-side. A decrypt failure
    // must be treated as a config/internal error, never "successful disconnect".
    let access_token = cipher
        .decrypt(&account.provider_ref)
        .map_err(|e| internal_error(format!("plaid token decrypt: {e}")))?;

    // Call Plaid FIRST; a failed call must leave the local state untouched so
    // we never believe an account is disconnected while Plaid could still serve
    // it (mirrors §14/§15/§16 disconnect ordering).
    plaid_post(
        "item/remove",
        serde_json::json!({ "access_token": access_token }),
    )
    .await?;

    let tx = pool.begin().await.map_err(internal_error)?;
    // Disconnect ALL rows sharing this Item (Plaid /item/remove revokes the
    // whole Item; no partial removal).
    let mut tx = tx;
    sqlx::query(
        "UPDATE linked_accounts SET status = 'disconnected', disconnected_at = now(), updated_at = now() \
         WHERE user_id = $1 AND provider = 'plaid' AND is_investments AND plaid_item_id = $2",
    )
    .bind(user_id)
    .bind(&account.plaid_item_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    // Close the assets this Item backed so the projection stops counting them.
    sqlx::query(
        "UPDATE assets SET status = 'closed', updated_at = now() \
         WHERE user_id = $1 AND linked_account_id IN (\
             SELECT id FROM linked_accounts WHERE user_id = $1 AND provider = 'plaid' \
                 AND is_investments AND plaid_item_id = $2)",
    )
    .bind(user_id)
    .bind(&account.plaid_item_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;
    Ok(())
}

/// Reconcile a reported balance + holdings snapshot for one investment account
/// into `securities`, `asset_holdings`, and `assets.current_balance`.
///
/// Access control is the `asset_id`-belongs-to-user filter inside every writer;
/// neither a foreign asset nor a decrypt failure is swallowed.
async fn sync_investment_holdings(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    account: &InvestmentLinkedAccount,
    asset_id: Uuid,
) -> Result<u64, (StatusCode, String)> {
    let access_token = cipher
        .decrypt(&account.provider_ref)
        .map_err(|e| internal_error(format!("plaid token decrypt: {e}")))?;

    let resp = plaid_post(
        "investments/holdings/get",
        serde_json::json!({
            "access_token": access_token,
            "options": { "account_ids": [account.provider_account_id] },
        }),
    )
    .await?;

    // ---- securities (shared cache) ----
    let securities = resp
        .get("securities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut security_id_to_db_id: std::collections::HashMap<String, Uuid> =
        std::collections::HashMap::new();
    for sec in &securities {
        let psid = match sec.get("security_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let ticker = sec.get("ticker").and_then(|v| v.as_str());
        let name = sec.get("name").and_then(|v| v.as_str());
        let currency = sec.get("currency_code").and_then(|v| v.as_str());
        // Lenient: Plaid's `type` uses values beyond the CHECK (e.g. "crypto",
        // "derivative"); SecurityType::from_db maps the recognised ones and
        // buckets the rest into Other.
        let security_type = assets::SecurityType::from_db(
            sec.get("type").and_then(|v| v.as_str()).unwrap_or("other"),
        );
        let db_id = assets::upsert_security(pool, &psid, ticker, name, security_type, currency)
            .await
            .map_err(internal_error)?;
        security_id_to_db_id.insert(psid, db_id);
    }

    // ---- accounts (for reported balance) ----
    let mut reported_balance: Option<f64> = None;
    // Plaid returns a top-level `as_of` — the date the holdings/balance data was
    // last refreshed. It is the fallback date when a holding carries no per-holding
    // `institution_price_as_of`.
    let balance_as_of: Option<NaiveDate> = resp
        .get("as_of")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    if let Some(accounts) = resp.get("accounts").and_then(|v| v.as_array()) {
        for acc in accounts {
            if acc.get("account_id").and_then(|v| v.as_str()) != Some(&account.provider_account_id)
            {
                continue;
            }
            reported_balance = acc.pointer("/balances/current").and_then(|v| v.as_f64());
            break;
        }
    }

    // ---- holdings ----
    let holdings = resp
        .get("holdings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut security_holdings: Vec<assets::SecurityHolding> = Vec::new();
    for h in &holdings {
        if h.get("account_id").and_then(|v| v.as_str()) != Some(&account.provider_account_id) {
            continue;
        }
        let psid = match h.get("security_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => continue,
        };
        let Some(&db_id) = security_id_to_db_id.get(psid) else {
            continue;
        };
        let quantity = h.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let market_value = h
            .get("institution_value")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        // as_of: prefer the per-holding price date, fall back to the balance date.
        let as_of = h
            .get("institution_price_as_of")
            .and_then(|v| v.as_str())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .or(balance_as_of)
            .map(|d| {
                d.and_hms_opt(0, 0, 0)
                    .expect("midnight is a valid time")
                    .and_utc()
            })
            .unwrap_or_else(chrono::Utc::now);
        security_holdings.push(assets::SecurityHolding {
            security_id: db_id,
            quantity,
            market_value,
            as_of,
        });
    }

    let written =
        assets::replace_holdings_for_asset(pool, account.user_id, asset_id, &security_holdings)
            .await
            .map_err(internal_error)?;
    match written {
        Some(()) => {}
        None => {
            // The composite same-user FK should have prevented this at classify
            // time; if it ever fires, callers see a loud denial, not $0.
            return Err((
                StatusCode::FORBIDDEN,
                "Investment asset no longer belongs to this user".to_string(),
            ));
        }
    }

    if let Some(balance) = reported_balance {
        if balance.is_finite() {
            let as_of = balance_as_of.unwrap_or_else(|| chrono::Utc::now().date_naive());
            assets::update_reported_balance(pool, account.user_id, asset_id, Some(balance), as_of)
                .await
                .map_err(internal_error)?;
        } else {
            tracing::warn!(account_id = %account.id, "plaid reported a non-finite investment balance; skipping balance write");
        }
    }

    Ok(security_holdings.len() as u64)
}

/// Scheduled poll (nels#468). Syncs every active investment account whose owner
/// is still Pro. Mirrors `gocardless::poll_active_accounts`'s shape and
/// gating; a `[plaid_investments::InvestmentLinkedAccount]` row is only selected
/// when an asset exists for it (nothing useful to sync until classification).
pub async fn poll_investment_accounts(
    pool: &PgPool,
    cipher: std::sync::Arc<crate::crypto::SecretCipher>,
) {
    let accounts: Vec<InvestmentLinkedAccount> = match sqlx::query_as(
        "SELECT la.id, la.user_id, la.budget_id, la.provider_account_id, la.provider_ref, \
                la.institution_name, la.display_name, la.last4, la.status, la.plaid_item_id, la.is_investments \
         FROM linked_accounts la \
         JOIN assets a ON a.linked_account_id = la.id AND a.user_id = la.user_id AND a.status = 'active' \
         WHERE la.is_investments AND la.status = 'active'",
    )
    .fetch_all(pool)
    .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(%e, "investments poll: failed to load active accounts");
            return;
        }
    };
    for account in accounts {
        let owner_status: Option<String> = match sqlx::query_scalar(
            "SELECT status FROM subscriptions WHERE user_id = $1",
        )
        .bind(account.user_id)
        .fetch_optional(pool)
        .await
        {
            Ok(s) => s.flatten(),
            Err(e) => {
                tracing::error!(%e, account_id = %account.id, "investments poll: failed to check owner subscription status; skipping this cycle");
                continue;
            }
        };
        if !crate::billing::user_is_pro(owner_status.as_deref()) {
            tracing::info!(account_id = %account.id, "investments poll: skipping, owner not Pro");
            continue;
        }
        let asset_id: Option<Uuid> = match sqlx::query_scalar(
            "SELECT id FROM assets WHERE user_id = $1 AND linked_account_id = $2 AND status = 'active'",
        )
        .bind(account.user_id)
        .bind(account.id)
        .fetch_optional(pool)
        .await
        {
            Ok(v) => v.flatten(),
            Err(e) => {
                tracing::error!(%e, account_id = %account.id, "investments poll: failed to resolve asset; skipping");
                continue;
            }
        };
        if let Some(asset_id) = asset_id {
            if let Err((status, msg)) =
                sync_investment_holdings(pool, &cipher, &account, asset_id).await
            {
                tracing::warn!(?status, %msg, account_id = %account.id, "investments poll-driven sync failed");
            }
        }
    }
}

// --- axum handlers ---------------------------------------------------------

pub async fn investments_link_token_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<InvestmentLinkTokenResponse>, (StatusCode, String)> {
    create_investment_link_token(&state.db, user_id)
        .await
        .map(Json)
}

pub async fn investments_complete_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<InvestmentCompleteRequest>,
) -> Result<Json<InvestmentAccountsResponse>, (StatusCode, String)> {
    complete_investment_link(
        &state.db,
        &state.cipher,
        user_id,
        req.session_id,
        &req.public_token,
    )
    .await
    .map(Json)
}

pub async fn investments_classify_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<InvestmentClassifyRequest>,
) -> Result<Json<Vec<Uuid>>, (StatusCode, String)> {
    classify_investment_accounts(&state.db, &state.cipher, user_id, req)
        .await
        .map(Json)
}

pub async fn investments_list_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<InvestmentAccountSummaryPublic>>, (StatusCode, String)> {
    list_investment_accounts(&state.db, user_id)
        .await
        .map(|accs| {
            Json(
                accs.into_iter()
                    .map(|a| InvestmentAccountSummaryPublic {
                        id: a.id,
                        display_name: a.display_name,
                        last4: a.last4,
                        institution_name: a.institution_name,
                        status: a.status,
                    })
                    .collect(),
            )
        })
}

#[derive(Debug, Serialize)]
pub struct InvestmentAccountSummaryPublic {
    pub id: Uuid,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub institution_name: Option<String>,
    pub status: String,
}

pub async fn investments_refresh_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<InvestmentRefreshResponse>, (StatusCode, String)> {
    refresh_investment_account(&state.db, &state.cipher, user_id, account_id)
        .await
        .map(Json)
}

pub async fn investments_disconnect_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    disconnect_investment_account(&state.db, &state.cipher, user_id, account_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// A `users` row with no subscription. Investment routes are Pro-gated via
    /// `require_caller_tier`, so tests that need the gate to pass seed a
    /// `subscriptions` row via [`mk_pro_subscription`].
    async fn mk_user(db: &PgPool) -> Uuid {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid)
            .bind(format!("plaid-inv-{uid}@test.example"))
            .execute(db)
            .await
            .unwrap();
        uid
    }

    /// Mirrors `plaid.rs`/`gocardless.rs` exactly: 'trialing' (never 'active')
    /// so `entitlement::resolve` maps to Tier::Pro unconditionally, with no
    /// price_id/STRIPE_PRICE_* env dependency, and the UNIQUE
    /// `stripe_customer_id` is bind per-user so parallel tests don't collide.
    async fn mk_pro_subscription(db: &PgPool, user_id: Uuid) {
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')")
            .bind(user_id)
            .bind(format!("cus_inv_{user_id}"))
            .execute(db).await.unwrap();
    }

    /// Mirrors `plaid.rs`'s `test_cipher` exactly (same key) — `provider_ref`
    /// is encrypted at rest, so every path that encrypts/decrypts it needs this.
    fn test_cipher() -> crate::crypto::SecretCipher {
        crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()
    }

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

    /// Mount the endpoints `create_investment_link_token` needs. `link_token` is
    /// parameterized so tests can assert what's returned.
    async fn mount_link_token(server: &MockServer, link_token: &str) {
        Mock::given(method("POST"))
            .and(path("/link/token/create"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "link_token": link_token,
            })))
            .mount(server)
            .await;
    }

    /// Mount the endpoints `complete_investment_link` calls on a successful path
    /// for a single account: exchange, accounts/get, institutions/get_by_id.
    async fn mount_successful_exchange(
        server: &MockServer,
        access_token: &str,
        item_id: &str,
        account_id: &str,
        mask: &str,
    ) {
        Mock::given(method("POST"))
            .and(path("/item/public_token/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": access_token, "item_id": item_id,
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/accounts/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accounts": [{"account_id": account_id, "name": "Robinhood", "mask": mask}],
                "item": {"institution_id": "ins_1"},
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/institutions/get_by_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "institution": {"institution_id": "ins_1", "name": "Test Broker"},
            })))
            .mount(server)
            .await;
    }

    /// Mount `/investments/holdings/get` returning one security, one holding and
    /// a reported balance for `account_id`. Parameterized so refresh tests can
    /// assert the value is read table-side.
    async fn mount_holdings(server: &MockServer, account_id: &str, balance: f64) {
        Mock::given(method("POST"))
            .and(path("/investments/holdings/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "as_of": "2026-08-01",
                "securities": [{"security_id": "sec_1", "ticker": "VTSAX", "name": "Vanguard",
                                 "type": "mutual_fund", "currency_code": "USD"}],
                "accounts": [{"account_id": account_id, "balances": {"current": balance}}],
                "holdings": [{"account_id": account_id, "security_id": "sec_1",
                              "quantity": 10.0, "institution_value": balance,
                              "institution_price_as_of": "2026-08-01"}],
            })))
            .mount(server)
            .await;
    }

    /// Insert a `pending` `investment_link_sessions` row directly, bypassing
    /// `create_investment_link_token`'s HTTP call.
    async fn mk_pending_session(db: &PgPool, user_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO investment_link_sessions (id, user_id) VALUES ($1, $2)")
            .bind(id)
            .bind(user_id)
            .execute(db)
            .await
            .unwrap();
        id
    }

    // --- create_investment_link_token -------------------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn create_investment_link_token_rejects_non_pro_with_zero_plaid_calls() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // No subscription → must reject BEFORE plaid_post (no PLAID calls at all).
        let result = create_investment_link_token(&db, uid).await;
        assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn create_investment_link_token_happy_path_returns_link_token_and_session() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        let handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handled_arc = handled.clone();
        Mock::given(method("POST"))
            .and(path("/link/token/create"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({ "products": ["investments"], "country_codes": ["US"] }),
            ))
            .respond_with(move |_req: &wiremock::Request| {
                handled_arc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "link_token": "lt_test" }))
            })
            .mount(&server)
            .await;
        set_plaid_env(&server);

        let resp = create_investment_link_token(&db, uid).await.unwrap();
        assert_eq!(resp.link_token, "lt_test");
        assert_eq!(
            handled.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one plaid call"
        );

        // The session row must exist and be pending (not yet claimed).
        let (exists, status): (Option<Uuid>, String) = sqlx::query_as(
            "SELECT id, status FROM investment_link_sessions WHERE id = $1 AND user_id = $2",
        )
        .bind(resp.session_id)
        .bind(uid)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(exists, Some(resp.session_id));
        assert_eq!(status, "pending");

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    // --- complete_investment_link -----------------------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_investment_link_rejects_unknown_session_with_zero_plaid_calls() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // No pending session for this id → 409 before any Plaid call; the mock
        // server will panic at drop if a request actually arrives.
        let result =
            complete_investment_link(&db, &test_cipher(), uid, Uuid::new_v4(), "public-token")
                .await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_investment_link_second_call_loses_the_atomic_claim() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_atomic", "1234").await;
        set_plaid_env(&server);
        let session_id = mk_pending_session(&db, uid).await;

        let first = complete_investment_link(&db, &test_cipher(), uid, session_id, "public-token")
            .await
            .expect("first completion ok");
        assert_eq!(first.accounts.len(), 1);

        // The session is now 'completed'; a second completion must 409 WITHOUT
        // a second Plaid exchange (mock `.expect(1)` verifies at drop).
        let second =
            complete_investment_link(&db, &test_cipher(), uid, session_id, "public-token").await;
        assert_eq!(second.unwrap_err().0, StatusCode::CONFLICT);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn complete_investment_link_persists_budget_less_investment_account() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(
            &server,
            "access_token_live",
            "item_1",
            "acc_persist",
            "4321",
        )
        .await;
        set_plaid_env(&server);
        let session_id = mk_pending_session(&db, uid).await;

        let resp = complete_investment_link(&db, &test_cipher(), uid, session_id, "public-token")
            .await
            .unwrap();
        assert_eq!(resp.accounts.len(), 1);
        let pending = &resp.accounts[0];
        assert_eq!(pending.last4.as_deref(), Some("4321"));
        assert_eq!(pending.display_name.as_deref(), Some("Robinhood"));

        // The row must be budget-less (budget_id IS NULL) and flagged is_investments,
        // with the access token stored ENCRYPTED (provider_ref != the plaintext).
        let (budget_id, is_inv, stored_ref, item_id, country): (Option<Uuid>, bool, String, Option<String>, String) =
            sqlx::query_as("SELECT budget_id, is_investments, provider_ref, plaid_item_id, country FROM linked_accounts WHERE id = $1")
                .bind(pending.linked_account_id)
                .fetch_one(&db).await.unwrap();
        assert_eq!(budget_id, None, "investment accounts are budget-less");
        assert!(is_inv);
        assert_eq!(item_id.as_deref(), Some("item_1"));
        assert_eq!(country, "US");
        assert_ne!(
            stored_ref, "access_token_live",
            "access token must be encrypted at rest"
        );

        // Completion writes NO assets row — that is the classify step's job.
        let assets_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE user_id = $1")
                .bind(uid)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(assets_count, 0);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    // --- classify_investment_accounts -------------------------------------

    /// Helper: complete a link for `uid` and return (completed_session_id,
    /// created linked_account_id). The session is now `status='completed'`, so
    /// callers can pass it straight to `classify_investment_accounts`, which
    /// requires a COMPLETED session (not a fresh pending one).
    async fn complete_one(db: &PgPool, _server: &MockServer, uid: Uuid) -> (Uuid, Uuid) {
        let session_id = mk_pending_session(db, uid).await;
        let resp = complete_investment_link(db, &test_cipher(), uid, session_id, "public-token")
            .await
            .unwrap();
        (session_id, resp.accounts[0].linked_account_id)
    }

    /// Directly seed a budget-less investment `linked_accounts` row + a completed
    /// `investment_link_sessions` row for `uid`, bypassing the Pro-gated
    /// `complete_investment_link` HTTP path (used by the poll-skip test, whose
    /// whole point is a NON-Pro owner). The `provider_ref` is encrypted with the
    /// same cipher `poll_investment_accounts` uses, so it can decrypt it.
    /// `provider_account_id` is parameterized because that column is globally
    /// unique — a test seeding several accounts must give each a distinct id.
    /// Returns `(completed_session_id, linked_account_id)`.
    async fn mk_direct_investment_setup(
        db: &PgPool,
        uid: Uuid,
        provider_account_id: &str,
    ) -> (Uuid, Uuid) {
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO investment_link_sessions (id, user_id, status) VALUES ($1, $2, 'completed')")
            .bind(session_id).bind(uid)
            .execute(db).await.unwrap();
        let account_id = Uuid::new_v4();
        let encrypted = test_cipher().encrypt("access_direct").unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider, provider_account_id, provider_ref, \
                 institution_id, institution_name, display_name, last4, country, \
                 plaid_item_id, is_investments) \
             VALUES ($1, NULL, $2, 'plaid', $3, $4, 'ins_1', 'Direct Bank', 'Direct', '9999', 'US', 'item_direct', TRUE)",
        )
        .bind(account_id).bind(uid).bind(provider_account_id).bind(encrypted)
        .execute(db).await.unwrap();
        (session_id, account_id)
    }

    /// Directly seed the full classified state — completed session, investment
    /// `linked_accounts` row, and an `assets` row (status `active`) — via SQL,
    /// bypassing BOTH Pro-gated HTTP paths (`complete_investment_link` and
    /// `classify_investment_accounts`). Used by `poll_skips_accounts_whose_owner_is_not_pro`,
    /// where the owner being NON-Pro is precisely the thing under test and either
    /// gate would 402 before the poll logic was ever reached. Returns `account_id`.
    async fn mk_direct_classified_setup(db: &PgPool, uid: Uuid, provider_account_id: &str) -> Uuid {
        let (session_id, account_id) =
            mk_direct_investment_setup(db, uid, provider_account_id).await;
        let _ = session_id;
        let asset = assets::insert_asset(
            db,
            uid,
            None,
            "Direct Bank",
            assets::AssetType::Brokerage,
            assets::TaxTreatment::Taxable,
            Some(account_id),
            None,
            "USD",
        )
        .await
        .unwrap();
        assert_eq!(asset.status, "active");
        account_id
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn classify_rejects_session_for_another_user() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        let attacker = mk_user(&db).await;
        mk_pro_subscription(&db, attacker).await;
        mk_pro_subscription(&db, owner).await; // complete_investment_link is Pro-gated
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // owner's session, completed.
        let session_id = mk_pending_session(&db, owner).await;
        mount_successful_exchange(&server, "a", "i", "acc_sessrej", "0000").await;
        complete_investment_link(&db, &test_cipher(), owner, session_id, "p")
            .await
            .unwrap();

        // Attacker (Pro) tries to classify owner's completed session → 403.
        let req = InvestmentClassifyRequest {
            session_id,
            classifications: vec![InvestmentClassification {
                linked_account_id: Uuid::new_v4(),
                asset_type: assets::AssetType::Brokerage,
                tax_treatment: assets::TaxTreatment::Taxable,
            }],
        };
        let result = classify_investment_accounts(&db, &test_cipher(), attacker, req).await;
        assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
            .bind(owner)
            .bind(attacker)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn classify_rejects_account_not_owned_by_user() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let other = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        mk_pro_subscription(&db, other).await;
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // Direct-seed both users (complete_investment_link mounts would shadow on
        // the shared account path). uid gets its own completed session; `other`
        // owns the account uid will try to classify.
        let uid_session = Uuid::new_v4();
        sqlx::query("INSERT INTO investment_link_sessions (id, user_id, status) VALUES ($1, $2, 'completed')")
            .bind(uid_session).bind(uid)
            .execute(&db).await.unwrap();
        let (_uid_session, _uid_account) = mk_direct_investment_setup(&db, uid, "uid_acc").await;
        let other_session = Uuid::new_v4();
        sqlx::query("INSERT INTO investment_link_sessions (id, user_id, status) VALUES ($1, $2, 'completed')")
            .bind(other_session).bind(other)
            .execute(&db).await.unwrap();
        let (_other_session, owner_account) =
            mk_direct_investment_setup(&db, other, "other_acc").await;

        let req = InvestmentClassifyRequest {
            session_id: uid_session, // a completed session belonging to uid
            classifications: vec![InvestmentClassification {
                linked_account_id: owner_account, // belongs to `other`, not uid
                asset_type: assets::AssetType::Brokerage,
                tax_treatment: assets::TaxTreatment::Taxable,
            }],
        };
        let result = classify_investment_accounts(&db, &test_cipher(), uid, req).await;
        assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
            .bind(uid)
            .bind(other)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn classify_happy_path_creates_asset_with_user_classification_and_syncs_holdings() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_classify", "1111").await;
        mount_holdings(&server, "acc_classify", 15000.5).await;
        set_plaid_env(&server);
        let (comp_session_id, account_id) = complete_one(&db, &server, uid).await;

        let req = InvestmentClassifyRequest {
            // classify requires a COMPLETED session; complete_one returns the
            // session it just completed (a fresh pending one would 404).
            session_id: comp_session_id,
            classifications: vec![InvestmentClassification {
                linked_account_id: account_id,
                asset_type: assets::AssetType::RetirementAccount,
                tax_treatment: assets::TaxTreatment::PreTax,
            }],
        };
        let asset_ids = classify_investment_accounts(&db, &test_cipher(), uid, req)
            .await
            .unwrap();
        assert_eq!(asset_ids.len(), 1);
        let asset_id = asset_ids[0];

        // The asset takes the USER-SUPPLIED classification (never derived).
        let (asset_type, tax_treatment): (String, String) = sqlx::query_as(
            "SELECT asset_type, tax_treatment FROM assets WHERE id = $1 AND user_id = $2",
        )
        .bind(asset_id)
        .bind(uid)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(asset_type, "retirement_account");
        assert_eq!(tax_treatment, "pre_tax");

        // Securities cache, holding, reported balance and balance history written.
        let security_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM securities WHERE provider = 'plaid' AND provider_security_id = 'sec_1'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(security_count, 1);
        let holding_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM asset_holdings WHERE asset_id = $1")
                .bind(asset_id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(holding_count, 1);
        let (balance, history): (f64, i64) = sqlx::query_as(
            "SELECT current_balance, (SELECT COUNT(*) FROM asset_balance_history WHERE asset_id = assets.id) FROM assets WHERE id = $1",
        )
        .bind(asset_id).fetch_one(&db).await.unwrap();
        assert!((balance - 15000.5).abs() < 0.001);
        assert_eq!(
            history, 1,
            "a balance-history snapshot must be recorded for the reported balance"
        );

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn classify_is_idempotent_per_linked_account() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_idem", "2222").await;
        mount_holdings(&server, "acc_idem", 100.0).await;
        set_plaid_env(&server);
        let (comp_session_id, account_id) = complete_one(&db, &server, uid).await;

        let mk = |sid: Uuid| InvestmentClassifyRequest {
            session_id: sid,
            classifications: vec![InvestmentClassification {
                linked_account_id: account_id,
                asset_type: assets::AssetType::Brokerage,
                tax_treatment: assets::TaxTreatment::Taxable,
            }],
        };

        // Two separate completed sessions (the completed session is not consumed
        // by classify — only its existence+owner is verified) so the two classify
        // calls are genuinely independent invocations.
        let second_session = Uuid::new_v4();
        sqlx::query("INSERT INTO investment_link_sessions (id, user_id, status) VALUES ($1, $2, 'completed')")
            .bind(second_session).bind(uid)
            .execute(&db).await.unwrap();
        let first = classify_investment_accounts(&db, &test_cipher(), uid, mk(comp_session_id))
            .await
            .unwrap();
        let second = classify_investment_accounts(&db, &test_cipher(), uid, mk(second_session))
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "re-classifying the same account must return the SAME asset, not create a second"
        );

        let assets_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE user_id = $1")
                .bind(uid)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(assets_count, 1, "exactly one asset per linked account");

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    // --- refresh_investment_account ---------------------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_rejects_unclassified_account_with_zero_plaid_calls() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        // complete_one drives the Pro-gated link-complete path, which needs the
        // exchange mock mounted (accounts/get best-effort, no holdings needed).
        mount_successful_exchange(&server, "access_1", "item_1", "acc_unclass", "6666").await;
        set_plaid_env(&server);
        let (_comp_session, account_id) = complete_one(&db, &server, uid).await; // linked but NOT classified

        let result = refresh_investment_account(&db, &test_cipher(), uid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_rejects_account_of_another_user_with_zero_plaid_calls() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let other = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        // `other` is seeded directly (NOT via the Pro-gated complete_one) so we
        // do not need to make them Pro just to set up the foreign account.
        let server = MockServer::start().await;
        set_plaid_env(&server);
        let (_other_session, other_account) =
            mk_direct_investment_setup(&db, other, "other_acc").await;

        let result = refresh_investment_account(&db, &test_cipher(), uid, other_account).await;
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
            .bind(uid)
            .bind(other)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_happy_path_syncs_holdings_and_balance() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_refresh", "3333").await;
        mount_holdings(&server, "acc_refresh", 20000.0).await;
        set_plaid_env(&server);
        let (comp_session_id, account_id) = complete_one(&db, &server, uid).await;
        // Classify (no extra holdings mock needed; the mount already covers it).
        let asset_id = classify_investment_accounts(
            &db,
            &test_cipher(),
            uid,
            InvestmentClassifyRequest {
                session_id: comp_session_id,
                classifications: vec![InvestmentClassification {
                    linked_account_id: account_id,
                    asset_type: assets::AssetType::Brokerage,
                    tax_treatment: assets::TaxTreatment::Taxable,
                }],
            },
        )
        .await
        .unwrap()[0];

        let resp = refresh_investment_account(&db, &test_cipher(), uid, account_id)
            .await
            .unwrap();
        assert_eq!(resp.asset_id, asset_id);
        assert_eq!(resp.holdings, 1);

        let balance: f64 = sqlx::query_scalar("SELECT current_balance FROM assets WHERE id = $1")
            .bind(asset_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(
            (balance - 20000.0).abs() < 0.001,
            "refresh must overwrite the reported balance"
        );

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    // --- disconnect_investment_account ------------------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_calls_item_remove_first_then_disconnects_and_closes() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_disc", "5555").await;
        set_plaid_env(&server);
        let (comp_session_id, account_id) = complete_one(&db, &server, uid).await;
        // Classify so there is an asset to close.
        let asset_id = classify_investment_accounts(
            &db,
            &test_cipher(),
            uid,
            InvestmentClassifyRequest {
                session_id: comp_session_id,
                classifications: vec![InvestmentClassification {
                    linked_account_id: account_id,
                    asset_type: assets::AssetType::Pension,
                    tax_treatment: assets::TaxTreatment::PreTax,
                }],
            },
        )
        .await
        .unwrap()[0];

        // item/remove must be hit exactly once (verified at drop).
        Mock::given(method("POST"))
            .and(path("/item/remove"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"request_id": "r"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let resp = disconnect_investment_account(&db, &test_cipher(), uid, account_id).await;
        assert!(resp.is_ok());

        let (status, disconnected_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT status, disconnected_at FROM linked_accounts WHERE id = $1")
                .bind(account_id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "disconnected");
        assert!(disconnected_at.is_some());
        let asset_status: String = sqlx::query_scalar("SELECT status FROM assets WHERE id = $1")
            .bind(asset_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(asset_status, "closed", "a disconnected investment account's asset must be closed so projections stop counting it");

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_is_not_pro_gated_and_rejects_foreign_account_before_plaid() {
        let db = test_pool().await;
        let uid = mk_user(&db).await; // NO subscription — disconnect must still work
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // FOREIGN account id; must 404 with zero Plaid calls (mock server panics at drop if one arrives).
        let result = disconnect_investment_account(&db, &test_cipher(), uid, Uuid::new_v4()).await;
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    // --- poll_investment_accounts -----------------------------------------

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn poll_skips_accounts_whose_owner_is_not_pro() {
        let db = test_pool().await;
        let uid = mk_user(&db).await; // no subscription → not Pro
        let server = MockServer::start().await;
        set_plaid_env(&server);
        // Seeded directly (NOT via complete_one/classify, which are Pro-gated
        // and would 402 before the poll logic was ever reached): a completed
        // session + active investment account + active asset.
        mk_direct_classified_setup(&db, uid, "poll_skip_acc").await;

        // No holdings/get mock mounted → the poll must NOT call Plaid, otherwise
        // the server panics at drop for an unmatched request.
        poll_investment_accounts(&db, Arc::new(test_cipher())).await;

        // Balance stays NULL — nothing was synced.
        let balance: Option<f64> =
            sqlx::query_scalar("SELECT current_balance FROM assets WHERE user_id = $1")
                .bind(uid)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(balance, None);

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn poll_syncs_active_investment_account_for_pro_owner() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_pro_subscription(&db, uid).await;
        let server = MockServer::start().await;
        mount_successful_exchange(&server, "access_1", "item_1", "acc_poll", "7777").await;
        mount_holdings(&server, "acc_poll", 30000.0).await;
        set_plaid_env(&server);
        let (comp_session_id, account_id) = complete_one(&db, &server, uid).await;
        classify_investment_accounts(
            &db,
            &test_cipher(),
            uid,
            InvestmentClassifyRequest {
                session_id: comp_session_id,
                classifications: vec![InvestmentClassification {
                    linked_account_id: account_id,
                    asset_type: assets::AssetType::Brokerage,
                    tax_treatment: assets::TaxTreatment::Taxable,
                }],
            },
        )
        .await
        .unwrap();

        poll_investment_accounts(&db, Arc::new(test_cipher())).await;

        let balance: f64 =
            sqlx::query_scalar("SELECT current_balance FROM assets WHERE user_id = $1")
                .bind(uid)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(
            (balance - 30000.0).abs() < 0.001,
            "poll must sync the reported balance for a Pro owner"
        );

        clear_plaid_env();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
    }
}
