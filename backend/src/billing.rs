//! Stripe subscription billing (#25): hosted Checkout + Customer Portal + webhook-synced
//! entitlement state. Webhooks are the ONLY source of entitlement truth.

use chrono::{DateTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;

/// Process-wide reqwest client so Stripe calls reuse pooled TCP/TLS connections
/// (creating a fresh `Client` per request would pay a full handshake each time).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Reasons signature verification can fail. The handler maps all of these to a
/// 400 without leaking which check failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SigError {
    MissingHeader,
    BadFormat,
    NoMatch,
    Expired,
}

/// True iff `status` grants Pro entitlement. Only Stripe's `trialing` and `active`
/// count; every other status (past_due, unpaid, incomplete, incomplete_expired,
/// canceled, paused, unknown) is NOT entitled.
pub fn user_is_pro(status: Option<&str>) -> bool {
    matches!(status, Some("trialing") | Some("active"))
}

/// Verify a Stripe `Stripe-Signature` header against the raw request body.
///
/// Header form: `t=<unix>,v1=<hex hmac>[,v1=<hex>...]`. The signed payload is
/// `"{t}.{body}"`, HMAC-SHA256 keyed with the endpoint signing secret. Comparison
/// is constant-time (`Mac::verify_slice`). Rejects if `|now_unix - t| > tolerance_secs`.
/// `now_unix`/`tolerance_secs` are parameters (not read from the clock) so this is
/// deterministically testable.
pub fn verify_stripe_signature(
    payload: &[u8],
    sig_header: &str,
    secret: &str,
    now_unix: i64,
    tolerance_secs: i64,
) -> Result<(), SigError> {
    let mut t: Option<i64> = None;
    let mut v1s: Vec<&str> = Vec::new();
    for part in sig_header.split(',') {
        let (k, v) = part.split_once('=').ok_or(SigError::BadFormat)?;
        match k.trim() {
            "t" => t = v.trim().parse().ok(),
            "v1" => v1s.push(v.trim()),
            _ => {}
        }
    }
    let t = t.ok_or(SigError::MissingHeader)?;
    if v1s.is_empty() {
        return Err(SigError::MissingHeader);
    }
    if (now_unix - t).abs() > tolerance_secs {
        return Err(SigError::Expired);
    }
    let mut signed = Vec::with_capacity(payload.len() + 16);
    signed.extend_from_slice(t.to_string().as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(payload);
    for v1 in v1s {
        let sig_bytes = match hex_decode(v1) {
            Some(b) => b,
            None => continue,
        };
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| SigError::BadFormat)?;
        mac.update(&signed);
        if mac.verify_slice(&sig_bytes).is_ok() {
            return Ok(());
        }
    }
    Err(SigError::NoMatch)
}

/// Decode a lowercase/uppercase hex string to bytes. Returns None on odd length or
/// a non-hex digit.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// The subset of a Stripe webhook event envelope we parse. `created` is the
/// event's Unix timestamp (the out-of-order ordering key).
#[derive(Debug, Deserialize)]
pub struct StripeEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub created: i64,
    pub data: StripeEventData,
}

#[derive(Debug, Deserialize)]
pub struct StripeEventData {
    pub object: serde_json::Value,
}

/// Normalized fields we persist from a subscription-bearing event. Resolution of
/// which user/customer this applies to happens in the handler (via the stored
/// customer id), not here.
#[derive(Debug, PartialEq)]
pub struct SubscriptionUpsert {
    pub stripe_customer_id: String,
    pub stripe_subscription_id: Option<String>,
    pub status: Option<String>,
    pub price_id: Option<String>,
    pub current_period_end: Option<DateTime<Utc>>,
    /// `None` for events that don't carry the flag (checkout/payment_failed/deleted)
    /// so they don't transiently clobber a previously-set value — only
    /// `customer.subscription.created|updated` set it. Persisted via COALESCE.
    pub cancel_at_period_end: Option<bool>,
    pub event_created: DateTime<Utc>,
}

/// Map a parsed Stripe event to the fields to upsert, or `None` for event types we
/// intentionally ignore (handler returns 200). Handles:
/// - `customer.subscription.created|updated`  (object = Subscription)
/// - `customer.subscription.deleted`     (object = Subscription → status forced `canceled`)
/// - `invoice.payment_failed`            (object = Invoice → status `past_due`)
pub fn apply_subscription_event(event: &StripeEvent) -> Option<SubscriptionUpsert> {
    let obj = &event.data.object;
    let event_created = Utc.timestamp_opt(event.created, 0).single().unwrap_or_else(|| {
        tracing::warn!(created = event.created, "unparseable stripe event.created; using now()");
        Utc::now()
    });
    let str_field = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    // current_period_end lives at the subscription top level in older API
    // versions and on items.data[0] in 2025-03-31.basil+; read top level, then
    // fall back to the line item.
    let period_end = |v: &serde_json::Value| -> Option<DateTime<Utc>> {
        v.get("current_period_end").and_then(|x| x.as_i64())
            .or_else(|| v.get("items").and_then(|i| i.get("data")).and_then(|d| d.get(0))
                .and_then(|it| it.get("current_period_end")).and_then(|x| x.as_i64()))
            .and_then(|s| Utc.timestamp_opt(s, 0).single())
    };

    match event.event_type.as_str() {
        // checkout.session.completed carries no authoritative subscription status,
        // and the customer row already exists (created in `ensure_customer` before
        // the redirect). `customer.subscription.created/updated` carry the full
        // state. We intentionally DO NOT persist this event: doing so would advance
        // the shared `last_stripe_event_at` watermark and could suppress a reordered
        // `subscription.created` (Stripe does not guarantee webhook ordering),
        // leaving a paying user non-entitled.
        "checkout.session.completed" => None,
        "customer.subscription.created" | "customer.subscription.updated" => {
            let price_id = obj
                .get("items").and_then(|i| i.get("data")).and_then(|d| d.get(0))
                .and_then(|item| item.get("price")).and_then(|p| p.get("id"))
                .and_then(|x| x.as_str()).map(str::to_string);
            Some(SubscriptionUpsert {
                stripe_customer_id: str_field(obj, "customer")?,
                stripe_subscription_id: str_field(obj, "id"),
                status: str_field(obj, "status"),
                price_id,
                current_period_end: period_end(obj),
                cancel_at_period_end: obj.get("cancel_at_period_end").and_then(|x| x.as_bool()),
                event_created,
            })
        }
        "customer.subscription.deleted" => Some(SubscriptionUpsert {
            stripe_customer_id: str_field(obj, "customer")?,
            stripe_subscription_id: str_field(obj, "id"),
            status: Some("canceled".to_string()),
            price_id: None,
            current_period_end: period_end(obj),
            cancel_at_period_end: None, // terminal; leave flag as-is
            event_created,
        }),
        "invoice.payment_failed" => Some(SubscriptionUpsert {
            stripe_customer_id: str_field(obj, "customer")?,
            stripe_subscription_id: str_field(obj, "subscription"),
            // Temporary: the accompanying customer.subscription.updated event carries
            // the authoritative status (past_due/unpaid/canceled) and, having an equal-or-later
            // event.created, overwrites this via the idempotency guard. Self-corrects.
            status: Some("past_due".to_string()),
            price_id: None,
            current_period_end: None,
            cancel_at_period_end: None, // not carried by this event
            event_created,
        }),
        _ => None,
    }
}

use axum::{extract::State, http::{HeaderMap, StatusCode}, Json};
use axum::body::Bytes;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;
use crate::auth::AppState;
use crate::error::internal_error;

const DEFAULT_TRIAL_DAYS: u32 = 7;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn stripe_api_base() -> String {
    env_opt("STRIPE_API_BASE").unwrap_or_else(|| "https://api.stripe.com".to_string())
}
fn app_url() -> String {
    env_opt("APP_URL").unwrap_or_else(|| "http://localhost:5173".to_string())
}
fn trial_days() -> u32 {
    env_opt("STRIPE_TRIAL_DAYS").and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_TRIAL_DAYS)
}

/// POST form-encoded to the Stripe API with the secret key as bearer. Returns the
/// parsed JSON body. Errors are genericized for the client (logged server-side).
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

/// DELETE against the Stripe API with the secret key as bearer. Used for
/// immediate subscription cancellation. Treats "already canceled" as success.
async fn stripe_delete(path: &str) -> Result<(), (StatusCode, String)> {
    let secret = env_opt("STRIPE_SECRET_KEY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let url = format!("{}/{}", stripe_api_base().trim_end_matches('/'), path);
    let resp = http_client()
        .delete(&url)
        .bearer_auth(secret)
        .send().await
        .map_err(|e| internal_error(format!("stripe DELETE {path}: {e}")))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    // Idempotent success: an already-canceled/nonexistent subscription. Stripe
    // can return either 404 or 400 for this depending on state, so we apply the
    // SAME confirmation check to both status codes rather than trusting either
    // bare status code alone — this gate stands between an irreversible account
    // deletion and a possibly-still-live paid subscription, so an unconfirmed
    // error (wrong body, wrong host, misconfigured STRIPE_API_BASE, an unrelated
    // 400) must abort, not silently proceed.
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let code = body.pointer("/error/code").and_then(|c| c.as_str()).unwrap_or("");
    let msg = body.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("");
    // Only a 404/400 is an "already gone" candidate; every other status aborts.
    let idempotent_status = status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST;
    // `error.code` is Stripe's version-stable identifier — trust it first.
    // `error.message` is human-readable documentation text with NO stability
    // guarantee, so a message-text match is only a narrow, distinctly-logged
    // fallback, never the primary signal.
    if idempotent_status && code == "resource_missing" {
        tracing::warn!(?status, ?body, "stripe DELETE {path}: confirmed resource_missing, treating as already-canceled");
        return Ok(());
    }
    if idempotent_status && (msg.contains("canceled") || msg.contains("cancelled")) {
        tracing::warn!(?status, ?body,
            "stripe DELETE {path}: already-canceled inferred from error message text \
             (not the stable error.code) — treating as success");
        return Ok(());
    }
    tracing::error!(?status, ?body, "stripe API error on DELETE {path}: unconfirmed, aborting");
    Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()))
}

/// Cancel the user's Stripe subscription immediately (no proration/refund).
/// No-op when the user has no `stripe_subscription_id`.
pub(crate) async fn cancel_subscription(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let sub_id: Option<String> = sqlx::query_scalar(
        "SELECT stripe_subscription_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .flatten();
    match sub_id {
        Some(id) if !id.trim().is_empty() => stripe_delete(&format!("v1/subscriptions/{id}")).await,
        _ => Ok(()),
    }
}

/// Create-or-reuse the user's Stripe customer id, persisting it on first creation.
pub(crate) async fn ensure_customer(db: &PgPool, user_id: Uuid) -> Result<String, (StatusCode, String)> {
    if let Some(cid) = sqlx::query_scalar::<_, String>(
        "SELECT stripe_customer_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(db).await.map_err(internal_error)? {
        return Ok(cid);
    }
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id).fetch_optional(db).await.map_err(internal_error)?;
    let form = vec![
        ("metadata[user_id]".to_string(), user_id.to_string()),
        ("email".to_string(), email.unwrap_or_default()),
    ];
    let cust = stripe_post("v1/customers", &form).await?;
    let cid = cust.get("id").and_then(|x| x.as_str())
        .ok_or_else(|| internal_error("stripe customer missing id"))?.to_string();
    // Persist immediately; ON CONFLICT keeps the first customer if a concurrent
    // checkout created one (prevents two customers per user).
    let ins = sqlx::query(
        "INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, $2) \
         ON CONFLICT (user_id) DO NOTHING")
        .bind(user_id).bind(&cid).execute(db).await.map_err(internal_error)?;
    if ins.rows_affected() == 0 {
        tracing::warn!(user_id = %user_id,
            "concurrent checkout created a duplicate Stripe customer; an orphaned customer may exist in Stripe");
    }
    // Re-read in case a concurrent insert won the race.
    let cid = sqlx::query_scalar::<_, String>(
        "SELECT stripe_customer_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_one(db).await.map_err(internal_error)?;
    Ok(cid)
}

/// Start a 7-day, no-card **Pro** trial subscription server-side (no Checkout
/// redirect). The `customer.subscription.created` webhook then writes
/// `status='trialing'` — this function does NOT persist status itself
/// (webhook is the single source of entitlement truth). Trial expires cleanly
/// to `canceled` (no card) via `end_behavior.missing_payment_method=cancel`.
pub(crate) async fn start_trial_subscription(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let price = env_opt("STRIPE_PRICE_PRO_MONTHLY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let customer = ensure_customer(db, user_id).await?;
    let mut form = vec![
        ("customer".to_string(), customer),
        ("items[0][price]".to_string(), price),
        ("items[0][quantity]".to_string(), "1".to_string()),
        ("payment_method_collection".to_string(), "if_required".to_string()),
        ("trial_settings[end_behavior][missing_payment_method]".to_string(), "cancel".to_string()),
    ];
    let trial = trial_days();
    if trial >= 1 {
        form.push(("trial_period_days".to_string(), trial.to_string()));
    }
    // Fire-and-persist-via-webhook: we ignore the returned body beyond success.
    stripe_post("v1/subscriptions", &form).await?;
    Ok(())
}

/// Gate run before a user creates a budget. Enforces the two-tier "first budget
/// starts a trial" rule (spec Section 2):
/// - owns >=1 budget → Ok (already an owner; normal path).
/// - owns 0 budgets, never subscribed → start a Pro trial, then Ok.
/// - owns 0 budgets, currently entitled → Ok (no new trial).
/// - owns 0 budgets, lapsed → Err(402) (resubscribe; no second free trial).
/// A Stripe failure while starting the trial returns Err (budget creation is
/// blocked rather than creating an un-entitled owner).
pub(crate) async fn ensure_ready_to_own_budget(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM budgets WHERE owner_id = $1")
        .bind(user_id).fetch_one(db).await.map_err(internal_error)?;
    if owned > 0 {
        return Ok(()); // already an owner
    }
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, price_id, stripe_subscription_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(db).await.map_err(internal_error)?;
    let (status, price_id, sub_id) = row.unwrap_or((None, None, None));
    // Never subscribed: no row, or a bare ensure_customer placeholder
    // (status NULL and no subscription id yet).
    if status.is_none() && sub_id.is_none() {
        return start_trial_subscription(db, user_id).await;
    }
    // Has a subscription history: entitled → allow; lapsed → block.
    let ent = crate::entitlement::resolve(status.as_deref(), price_id.as_deref(),
        &crate::entitlement::PriceCatalog::from_env());
    if ent.tier == crate::entitlement::Tier::None {
        return Err((StatusCode::PAYMENT_REQUIRED,
            "Your subscription ended — resubscribe to create a budget.".to_string()));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CheckoutRequest {
    pub cadence: String, // "monthly" | "annual"
    /// "basic" | "pro" (case-insensitive), optional. Absent → Pro (back-compat
    /// with cadence-only marketing deep-links). See `resolve_price_env_var`.
    #[serde(default)]
    pub tier: Option<String>,
    /// ISO 3166-1 alpha-2 country code, optional (#338). Only "US" is
    /// configured today; accepted but does not branch pricing yet.
    #[serde(default)]
    pub country: Option<String>,
}
#[derive(Serialize)]
pub struct UrlResponse { pub url: String }

/// (tier, cadence) -> the Stripe price env var name. `tier` is `"basic"`/`"pro"`
/// (case-insensitive); an absent or unrecognized tier defaults to **Pro** so
/// the existing cadence-only marketing deep-links keep working and err toward
/// the fuller plan. `country` is accepted for future multi-market pricing but
/// does not branch the mapping today (only US prices are configured). `None`
/// is returned ONLY for an unrecognized `cadence` — the same 400 the caller
/// produced before tiers existed.
fn resolve_price_env_var(tier: Option<&str>, cadence: &str, country: Option<&str>) -> Option<&'static str> {
    let tier = tier.map(str::to_lowercase).unwrap_or_else(|| "pro".to_string());
    let _country = country.map(str::to_uppercase).unwrap_or_else(|| "US".to_string());
    match (tier.as_str(), cadence) {
        ("basic", "monthly") => Some("STRIPE_PRICE_BASIC_MONTHLY"),
        ("basic", "annual") => Some("STRIPE_PRICE_BASIC_ANNUAL"),
        ("pro", "monthly") => Some("STRIPE_PRICE_PRO_MONTHLY"),
        ("pro", "annual") => Some("STRIPE_PRICE_PRO_ANNUAL"),
        // Unrecognized tier defaults to Pro (back-compat with cadence-only callers).
        (_, "monthly") => Some("STRIPE_PRICE_PRO_MONTHLY"),
        (_, "annual") => Some("STRIPE_PRICE_PRO_ANNUAL"),
        _ => None, // unrecognized cadence → 400
    }
}

pub async fn checkout(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
    Json(req): Json<CheckoutRequest>,
) -> Result<Json<UrlResponse>, (StatusCode, String)> {
    let price_env_var = resolve_price_env_var(req.tier.as_deref(), &req.cadence, req.country.as_deref())
        .ok_or((StatusCode::BAD_REQUEST, "cadence must be monthly or annual".to_string()))?;
    let price = env_opt(price_env_var)
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    // Don't let an already-subscribed user open a second subscription — direct
    // them to the portal for plan changes (billing correctness, not client-trust).
    let existing_status = sqlx::query_scalar::<_, Option<String>>(
        "SELECT status FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(&state.db).await.map_err(internal_error)?;
    if let Some(status) = existing_status.flatten() {
        if user_is_pro(Some(&status)) {
            return Err((StatusCode::CONFLICT,
                "Already subscribed; use the billing portal to change plans".to_string()));
        }
    }
    let customer = ensure_customer(&state.db, user_id).await?;
    let base = app_url();
    let mut form = vec![
        ("mode".to_string(), "subscription".to_string()),
        ("customer".to_string(), customer),
        ("client_reference_id".to_string(), user_id.to_string()),
        ("line_items[0][price]".to_string(), price),
        ("line_items[0][quantity]".to_string(), "1".to_string()),
        // No card required to start the trial (matches "no credit card to start").
        ("payment_method_collection".to_string(), "if_required".to_string()),
        ("success_url".to_string(), format!("{base}/?billing=success")),
        ("cancel_url".to_string(), format!("{base}/?billing=cancel")),
    ];
    // Stripe rejects trial_period_days=0 (minimum 1); a 0 here means "no trial",
    // so only send the field when there actually is a trial.
    let trial = trial_days();
    if trial >= 1 {
        form.push(("subscription_data[trial_period_days]".to_string(), trial.to_string()));
    }
    let session = stripe_post("v1/checkout/sessions", &form).await?;
    let url = session.get("url").and_then(|x| x.as_str())
        .ok_or_else(|| internal_error("checkout session missing url"))?.to_string();
    Ok(Json(UrlResponse { url }))
}

pub async fn portal(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
) -> Result<Json<UrlResponse>, (StatusCode, String)> {
    let customer = sqlx::query_scalar::<_, String>(
        "SELECT stripe_customer_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(&state.db).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "No billing customer for this user".to_string()))?;
    let form = vec![
        ("customer".to_string(), customer),
        ("return_url".to_string(), app_url()),
    ];
    let sess = stripe_post("v1/billing_portal/sessions", &form).await?;
    let url = sess.get("url").and_then(|x| x.as_str())
        .ok_or_else(|| internal_error("portal session missing url"))?.to_string();
    Ok(Json(UrlResponse { url }))
}

#[derive(Serialize)]
pub struct SubscriptionResponse {
    pub status: Option<String>,
    pub price_id: Option<String>,
    pub current_period_end: Option<chrono::DateTime<chrono::Utc>>,
    pub cancel_at_period_end: bool,
    pub is_pro: bool,
    pub tier: crate::entitlement::Tier,
    pub payment_warning: bool,
}

/// Build the subscription API response from the persisted fields plus a price
/// catalog. Pure (no DB) so the field mapping is unit-testable. `is_pro` is kept
/// for backward compatibility with existing gates/admin/frontend (Plans B & C
/// migrate them); `tier`/`payment_warning` come from the new resolver.
fn build_subscription_response(
    status: Option<String>,
    price_id: Option<String>,
    current_period_end: Option<chrono::DateTime<chrono::Utc>>,
    cancel_at_period_end: bool,
    catalog: &crate::entitlement::PriceCatalog,
) -> SubscriptionResponse {
    let ent = crate::entitlement::resolve(status.as_deref(), price_id.as_deref(), catalog);
    SubscriptionResponse {
        is_pro: user_is_pro(status.as_deref()),
        tier: ent.tier,
        payment_warning: ent.payment_warning,
        status,
        price_id,
        current_period_end,
        cancel_at_period_end,
    }
}

pub async fn get_subscription(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
) -> Result<Json<SubscriptionResponse>, (StatusCode, String)> {
    let row = sqlx::query_as::<_, crate::db::Subscription>(
        "SELECT * FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(&state.db).await.map_err(internal_error)?;
    let catalog = crate::entitlement::PriceCatalog::from_env();
    Ok(Json(match row {
        Some(s) => build_subscription_response(
            s.status, s.price_id, s.current_period_end, s.cancel_at_period_end, &catalog),
        None => build_subscription_response(None, None, None, false, &catalog),
    }))
}

/// PUBLIC, signature-verified. Raw body (`Bytes`) is required for HMAC, so this
/// handler must NOT use `Json<_>` extraction. Mounted OUTSIDE the auth nest.
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let secret = env_opt("STRIPE_WEBHOOK_SECRET")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let sig = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = Utc::now().timestamp();
    if verify_stripe_signature(&body, sig, &secret, now, 300).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid signature".to_string()));
    }
    let event: StripeEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid payload".to_string())),
    };
    if let Some(upsert) = apply_subscription_event(&event) {
        persist_upsert(&state.db, &upsert).await.map_err(internal_error)?;
    } else {
        tracing::debug!(event_type = %event.event_type, "ignoring unhandled stripe event type");
    }
    Ok(StatusCode::OK) // 2xx for handled-and-ignored alike (avoid retry storms)
}

/// Upsert subscription state onto the row matching `stripe_customer_id`. Drops
/// out-of-order/duplicate events via `last_stripe_event_at`. NULL incoming fields
/// do not clobber existing non-null values (COALESCE).
async fn persist_upsert(db: &PgPool, u: &SubscriptionUpsert) -> Result<(), sqlx::Error> {
    let res = sqlx::query(
        "UPDATE subscriptions SET \
            stripe_subscription_id = COALESCE($2, stripe_subscription_id), \
            status                 = COALESCE($3, status), \
            price_id               = COALESCE($4, price_id), \
            current_period_end     = COALESCE($5, current_period_end), \
            cancel_at_period_end   = COALESCE($6, cancel_at_period_end), \
            last_stripe_event_at   = $7, \
            updated_at             = now() \
         WHERE stripe_customer_id = $1 \
           AND (last_stripe_event_at IS NULL OR last_stripe_event_at <= $7)")
        .bind(&u.stripe_customer_id)
        .bind(&u.stripe_subscription_id)
        .bind(&u.status)
        .bind(&u.price_id)
        .bind(u.current_period_end)
        .bind(u.cancel_at_period_end)
        .bind(u.event_created)
        .execute(db).await?;
    if res.rows_affected() == 0 {
        // 0 rows means either (a) the ordering guard rejected a stale/duplicate
        // event (benign, expected) or (b) no subscriptions row exists for this
        // customer (a real anomaly — a paid subscription update with nowhere to
        // land). Distinguish them so (b) is alertable instead of lost in warn noise.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM subscriptions WHERE stripe_customer_id = $1)")
            .bind(&u.stripe_customer_id)
            .fetch_one(db)
            .await?;
        if exists {
            tracing::debug!(customer_id = %u.stripe_customer_id, event_at = %u.event_created,
                "stripe event older than last applied; skipped (idempotency)");
        } else {
            tracing::error!(customer_id = %u.stripe_customer_id, event_at = %u.event_created,
                "stripe webhook for UNKNOWN customer — entitlement update dropped; \
                 a paid subscription may not be reflected");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn checkout_request_tier_is_optional_and_parses() {
        // Absent tier deserializes to None (handler then defaults it to Pro).
        let a: CheckoutRequest = serde_json::from_str(r#"{"cadence":"monthly"}"#).unwrap();
        assert_eq!(a.cadence, "monthly");
        assert_eq!(a.tier, None);
        // Present tier is carried through.
        let b: CheckoutRequest = serde_json::from_str(r#"{"cadence":"annual","tier":"basic"}"#).unwrap();
        assert_eq!(b.tier.as_deref(), Some("basic"));
    }

    #[test]
    fn resolve_price_basic_monthly() {
        assert_eq!(resolve_price_env_var(Some("basic"), "monthly", None), Some("STRIPE_PRICE_BASIC_MONTHLY"));
    }

    #[test]
    fn resolve_price_basic_annual() {
        assert_eq!(resolve_price_env_var(Some("basic"), "annual", None), Some("STRIPE_PRICE_BASIC_ANNUAL"));
    }

    #[test]
    fn resolve_price_pro_monthly() {
        assert_eq!(resolve_price_env_var(Some("pro"), "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
    }

    #[test]
    fn resolve_price_pro_annual() {
        assert_eq!(resolve_price_env_var(Some("pro"), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_absent_tier_defaults_to_pro() {
        // Back-compat: cadence-only marketing deep-links (no tier) get Pro.
        assert_eq!(resolve_price_env_var(None, "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(None, "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_unknown_tier_defaults_to_pro() {
        assert_eq!(resolve_price_env_var(Some("gold"), "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some(""), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_tier_is_case_insensitive() {
        assert_eq!(resolve_price_env_var(Some("BASIC"), "monthly", None), Some("STRIPE_PRICE_BASIC_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some("Pro"), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_unknown_cadence_is_none() {
        // An unrecognized cadence is the ONLY 400 — independent of tier.
        assert_eq!(resolve_price_env_var(Some("basic"), "weekly", None), None);
        assert_eq!(resolve_price_env_var(Some("pro"), "bogus", None), None);
        assert_eq!(resolve_price_env_var(None, "", None), None);
    }

    #[test]
    fn resolve_price_country_does_not_change_tiered_mapping() {
        // Only US prices are configured; country is accepted but does not
        // branch the mapping today (kept in the signature for future markets).
        assert_eq!(resolve_price_env_var(Some("pro"), "monthly", Some("US")), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some("basic"), "annual", Some("gb")), Some("STRIPE_PRICE_BASIC_ANNUAL"));
    }

    fn sign(secret: &str, t: i64, payload: &[u8]) -> String {
        let mut signed = format!("{t}.").into_bytes();
        signed.extend_from_slice(payload);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(&signed);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("t={t},v1={hex}")
    }

    #[test]
    fn user_is_pro_only_trialing_and_active() {
        assert!(user_is_pro(Some("trialing")));
        assert!(user_is_pro(Some("active")));
        for s in ["past_due", "unpaid", "incomplete", "incomplete_expired", "canceled", "paused", "bogus"] {
            assert!(!user_is_pro(Some(s)), "{s} must not be pro");
        }
        assert!(!user_is_pro(None));
    }

    #[test]
    fn subscription_response_shapes_tier_and_warning() {
        use crate::entitlement::{PriceCatalog, Tier};
        let cat = PriceCatalog::new(vec!["b".into()], vec!["p".into()]);
        // active + pro price → is_pro true, tier pro, no warning
        let r = build_subscription_response(Some("active".into()), Some("p".into()), None, false, &cat);
        assert!(r.is_pro);
        assert_eq!(r.tier, Tier::Pro);
        assert!(!r.payment_warning);
        // past_due + basic price → is_pro false (legacy), tier basic, warning on
        let r = build_subscription_response(Some("past_due".into()), Some("b".into()), None, false, &cat);
        assert!(!r.is_pro, "legacy is_pro stays false for past_due");
        assert_eq!(r.tier, Tier::Basic);
        assert!(r.payment_warning);
        // no row → none
        let r = build_subscription_response(None, None, None, false, &cat);
        assert_eq!(r.tier, Tier::None);
        assert!(!r.is_pro);
    }

    #[test]
    fn signature_valid() {
        let secret = "whsec_test";
        let payload = br#"{"id":"evt_1"}"#;
        let header = sign(secret, 1_700_000_000, payload);
        assert_eq!(verify_stripe_signature(payload, &header, secret, 1_700_000_010, 300), Ok(()));
    }

    #[test]
    fn signature_tampered_payload_fails() {
        let secret = "whsec_test";
        let header = sign(secret, 1_700_000_000, br#"{"id":"evt_1"}"#);
        let res = verify_stripe_signature(br#"{"id":"evt_TAMPERED"}"#, &header, secret, 1_700_000_010, 300);
        assert_eq!(res, Err(SigError::NoMatch));
    }

    #[test]
    fn signature_expired_fails() {
        let secret = "whsec_test";
        let payload = br#"{"id":"evt_1"}"#;
        let header = sign(secret, 1_700_000_000, payload);
        assert_eq!(
            verify_stripe_signature(payload, &header, secret, 1_700_000_000 + 9999, 300),
            Err(SigError::Expired)
        );
    }

    #[test]
    fn signature_missing_or_bad_format() {
        assert_eq!(
            verify_stripe_signature(b"x", "t=1", "s", 1, 300),
            Err(SigError::MissingHeader)
        );
        assert_eq!(
            verify_stripe_signature(b"x", "garbage", "s", 1, 300),
            Err(SigError::BadFormat)
        );
    }

    fn evt(t: &str, created: i64, object: serde_json::Value) -> StripeEvent {
        serde_json::from_value(serde_json::json!({
            "type": t, "created": created, "data": { "object": object }
        })).unwrap()
    }

    #[test]
    fn maps_subscription_updated() {
        let e = evt("customer.subscription.updated", 1_700_000_000, serde_json::json!({
            "id": "sub_1", "customer": "cus_1", "status": "active",
            "current_period_end": 1_701_000_000, "cancel_at_period_end": true,
            "items": { "data": [ { "price": { "id": "price_monthly" } } ] }
        }));
        let u = apply_subscription_event(&e).unwrap();
        assert_eq!(u.stripe_subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(u.stripe_customer_id, "cus_1");
        assert_eq!(u.status.as_deref(), Some("active"));
        assert_eq!(u.price_id.as_deref(), Some("price_monthly"));
        assert_eq!(u.cancel_at_period_end, Some(true));
    }

    #[test]
    fn maps_subscription_deleted_to_canceled() {
        let e = evt("customer.subscription.deleted", 1_700_000_000, serde_json::json!({
            "id": "sub_1", "customer": "cus_1", "status": "active"
        }));
        assert_eq!(apply_subscription_event(&e).unwrap().status.as_deref(), Some("canceled"));
    }

    #[test]
    fn maps_payment_failed_to_past_due() {
        let e = evt("invoice.payment_failed", 1_700_000_000, serde_json::json!({
            "customer": "cus_1", "subscription": "sub_1"
        }));
        assert_eq!(apply_subscription_event(&e).unwrap().status.as_deref(), Some("past_due"));
    }

    #[test]
    fn ignores_checkout_session_completed() {
        // Intentionally not persisted: it carries no authoritative status and would
        // advance the out-of-order watermark, risking suppression of a reordered
        // customer.subscription.created.
        let e = evt("checkout.session.completed", 1_700_000_000, serde_json::json!({
            "customer": "cus_1", "subscription": "sub_1"
        }));
        assert!(apply_subscription_event(&e).is_none());
    }

    #[test]
    fn ignores_unknown_event_type() {
        let e = evt("charge.refunded", 1_700_000_000, serde_json::json!({ "customer": "cus_1" }));
        assert!(apply_subscription_event(&e).is_none());
    }

    #[test]
    fn current_period_end_falls_back_to_line_item() {
        // Stripe 2025-03-31.basil+ moves current_period_end onto items.data[0].
        let e = evt("customer.subscription.updated", 1_700_000_000, serde_json::json!({
            "id": "sub_b", "customer": "cus_b", "status": "active",
            "items": { "data": [ { "price": { "id": "price_m" }, "current_period_end": 1_701_000_000 } ] }
        }));
        let u = apply_subscription_event(&e).unwrap();
        assert!(u.current_period_end.is_some(), "should read current_period_end from items.data[0]");
    }

    use sqlx::postgres::PgPoolOptions;

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
            .bind(id).bind(format!("billing-{id}@test.example"))
            .execute(db).await.unwrap();
        id
    }

    /// A user plus an active subscription row carrying the given Stripe ids.
    async fn mk_user_with_sub(db: &PgPool, customer: &str, subscription: &str) -> Uuid {
        let id = mk_user(db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, $2, $3)")
            .bind(id).bind(customer).bind(subscription).execute(db).await.unwrap();
        id
    }

    // The Stripe HTTP client reads these at call time; point it at a mock server
    // for the DELETE/cancel tests and clear them after so nothing leaks across
    // tests in the same binary (matches the existing github.rs/rag.rs convention).
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
    async fn upsert_inserts_then_updates_same_row() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_X')")
            .bind(uid).execute(&db).await.unwrap();

        let created = evt("customer.subscription.created", 1_700_000_000, serde_json::json!({
            "id": "sub_X", "customer": "cus_X", "status": "trialing",
            "current_period_end": 1_701_000_000, "cancel_at_period_end": false,
            "items": { "data": [ { "price": { "id": "price_m" } } ] }
        }));
        persist_upsert(&db, &apply_subscription_event(&created).unwrap()).await.unwrap();

        let updated = evt("customer.subscription.updated", 1_700_500_000, serde_json::json!({
            "id": "sub_X", "customer": "cus_X", "status": "active",
            "current_period_end": 1_701_000_000, "cancel_at_period_end": true,
            "items": { "data": [ { "price": { "id": "price_m" } } ] }
        }));
        persist_upsert(&db, &apply_subscription_event(&updated).unwrap()).await.unwrap();

        let row = sqlx::query_as::<_, crate::db::Subscription>(
            "SELECT * FROM subscriptions WHERE user_id = $1").bind(uid)
            .fetch_one(&db).await.unwrap();
        assert_eq!(row.status.as_deref(), Some("active"));
        assert!(row.cancel_at_period_end);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn out_of_order_event_does_not_regress() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_Y')")
            .bind(uid).execute(&db).await.unwrap();
        let newer = evt("customer.subscription.updated", 1_700_500_000, serde_json::json!({
            "id": "sub_Y", "customer": "cus_Y", "status": "active",
            "items": { "data": [ { "price": { "id": "price_m" } } ] }
        }));
        persist_upsert(&db, &apply_subscription_event(&newer).unwrap()).await.unwrap();
        let older = evt("customer.subscription.updated", 1_700_000_000, serde_json::json!({
            "id": "sub_Y", "customer": "cus_Y", "status": "trialing",
            "items": { "data": [ { "price": { "id": "price_m" } } ] }
        }));
        persist_upsert(&db, &apply_subscription_event(&older).unwrap()).await.unwrap();
        let row = sqlx::query_as::<_, crate::db::Subscription>(
            "SELECT * FROM subscriptions WHERE user_id = $1").bind(uid)
            .fetch_one(&db).await.unwrap();
        assert_eq!(row.status.as_deref(), Some("active"), "older event must not regress status");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn cascade_deletes_subscription_with_user() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_Z')")
            .bind(uid).execute(&db).await.unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM subscriptions WHERE user_id = $1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    #[ignore]
    async fn later_none_event_does_not_clobber_status() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_C')")
            .bind(uid).execute(&db).await.unwrap();
        // active + cancel=true at T.
        let active = evt("customer.subscription.updated", 1_700_000_000, serde_json::json!({
            "id": "sub_C", "customer": "cus_C", "status": "active", "cancel_at_period_end": true,
            "items": { "data": [ { "price": { "id": "price_m" } } ] }
        }));
        persist_upsert(&db, &apply_subscription_event(&active).unwrap()).await.unwrap();
        // A LATER payment_failed sets past_due, but its cancel_at_period_end/price are
        // None and must NOT clobber the stored values (COALESCE).
        let pf = evt("invoice.payment_failed", 1_700_500_000, serde_json::json!({
            "customer": "cus_C", "subscription": "sub_C"
        }));
        persist_upsert(&db, &apply_subscription_event(&pf).unwrap()).await.unwrap();
        let row = sqlx::query_as::<_, crate::db::Subscription>(
            "SELECT * FROM subscriptions WHERE user_id = $1").bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(row.status.as_deref(), Some("past_due"));
        assert!(row.cancel_at_period_end, "None cancel flag must not clobber stored true");
        assert_eq!(row.price_id.as_deref(), Some("price_m"), "None price must not clobber stored value");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn same_timestamp_subscription_update_overrides_payment_failed() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_D')")
            .bind(uid).execute(&db).await.unwrap();
        let t = 1_700_000_000;
        let pf = evt("invoice.payment_failed", t, serde_json::json!({
            "customer": "cus_D", "subscription": "sub_D" }));
        persist_upsert(&db, &apply_subscription_event(&pf).unwrap()).await.unwrap();
        // Same-second authoritative update must still apply (the <= guard, not <).
        let upd = evt("customer.subscription.updated", t, serde_json::json!({
            "id": "sub_D", "customer": "cus_D", "status": "active",
            "items": { "data": [ { "price": { "id": "price_m" } } ] } }));
        persist_upsert(&db, &apply_subscription_event(&upd).unwrap()).await.unwrap();
        let row = sqlx::query_as::<_, crate::db::Subscription>(
            "SELECT * FROM subscriptions WHERE user_id = $1").bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(row.status.as_deref(), Some("active"), "same-second updated must override past_due");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_calls_stripe_delete() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .and(path("/v1/subscriptions/sub_123"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id":"sub_123","status":"canceled"})))
            .expect(1)
            .mount(&server)
            .await;

        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_123", "sub_123").await;

        cancel_subscription(&db, user_id).await.expect("cancel ok");
        // wiremock verifies .expect(1) on drop

        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    async fn cancel_subscription_noop_without_stripe_id() {
        let db = test_pool().await;
        let user_id = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_noop')")
            .bind(user_id).execute(&db).await.unwrap(); // stripe_subscription_id stays NULL
        // No STRIPE_SECRET_KEY / server needed: must not make any call.
        cancel_subscription(&db, user_id).await.expect("noop ok");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_errors_on_stripe_5xx() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_err", "sub_err").await;
        let res = cancel_subscription(&db, user_id).await;
        assert!(res.is_err(), "5xx must propagate as error");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_confirmed_404_resource_missing_is_ok() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": "resource_missing", "type": "invalid_request_error",
                          "message": "No such subscription: 'sub_gone'"}
            })))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_gone", "sub_gone").await;
        cancel_subscription(&db, user_id).await.expect("confirmed 404 resource_missing must be treated as already-canceled");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_unconfirmed_404_aborts() {
        // A 404 with NO Stripe-shaped error body (e.g. a misconfigured
        // STRIPE_API_BASE hitting the wrong host, a proxy error page) must NOT
        // be silently treated as already-canceled — this is the exact case that
        // would otherwise let an irreversible account deletion proceed while a
        // real subscription keeps billing.
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_amb", "sub_amb").await;
        let res = cancel_subscription(&db, user_id).await;
        assert!(res.is_err(), "an unconfirmed 404 (no resource_missing/canceled signal) must abort, not silently succeed");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_confirmed_400_resource_missing_is_ok() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"code": "resource_missing", "type": "invalid_request_error",
                          "message": "Subscription 'sub_400ok' has already been canceled"}
            })))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_400ok", "sub_400ok").await;
        cancel_subscription(&db, user_id).await.expect("confirmed 400 resource_missing must be treated as already-canceled");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_400_message_fallback_confirms_canceled() {
        // Exercises the narrower error.message fallback (no resource_missing
        // code, but the message text says "canceled") — still treated as
        // already-done, distinctly logged from the code-confirmed path.
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"code": "subscription_conflict", "type": "invalid_request_error",
                          "message": "This subscription has already been canceled"}
            })))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_msg", "sub_msg").await;
        cancel_subscription(&db, user_id).await.expect("message-confirmed already-canceled must succeed");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn cancel_subscription_unrelated_400_is_not_swallowed() {
        // A 400 that is neither resource_missing nor mentions "canceled" (e.g. a
        // genuinely different validation error) must propagate as a real
        // failure, never be silently treated as already-canceled.
        let server = MockServer::start().await;
        set_stripe_env(&server);
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"code": "parameter_invalid_empty", "type": "invalid_request_error",
                          "message": "The subscription id provided was malformed"}
            })))
            .mount(&server).await;
        let db = test_pool().await;
        let user_id = mk_user_with_sub(&db, "cus_unrelated", "sub_unrelated").await;
        let res = cancel_subscription(&db, user_id).await;
        assert!(res.is_err(), "an unrelated 400 must not be swallowed as already-canceled");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
        clear_stripe_env();
    }

    #[tokio::test]
    #[ignore]
    async fn unknown_customer_event_is_noop_ok() {
        let db = test_pool().await;
        // No row for this customer: must be a no-op that returns Ok (webhook → 200,
        // no Stripe retry storm) and inserts nothing.
        let e = evt("customer.subscription.updated", 1_700_000_000, serde_json::json!({
            "id": "sub_none", "customer": "cus_DOES_NOT_EXIST", "status": "active",
            "items": { "data": [ { "price": { "id": "price_m" } } ] } }));
        persist_upsert(&db, &apply_subscription_event(&e).unwrap()).await.unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM subscriptions WHERE stripe_customer_id = 'cus_DOES_NOT_EXIST'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn start_trial_posts_subscription_with_trial_and_cancel_behavior() {
        use wiremock::matchers::{method, path, body_string_contains};
        let server = MockServer::start().await;
        set_stripe_env(&server);
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        // customer creation (ensure_customer) + subscription creation.
        Mock::given(method("POST")).and(path("v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_trial"})))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("v1/subscriptions"))
            .and(body_string_contains("trial_period_days"))
            .and(body_string_contains("missing_payment_method"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"sub_trial","status":"trialing"})))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let r = start_trial_subscription(&db, uid).await;
        assert!(r.is_ok(), "start_trial should succeed: {r:?}");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
        clear_stripe_env();
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
        // wiremock verifies .expect(1) on the subscriptions POST at drop
    }

    #[tokio::test]
    #[ignore]
    async fn ready_to_own_existing_owner_is_ok_no_stripe() {
        // Owns >=1 budget already → allowed, no Stripe call needed.
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'B','monthly',0)")
            .bind(Uuid::new_v4()).bind(uid).execute(&db).await.unwrap();
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn ready_to_own_lapsed_is_blocked_402() {
        // Owns 0 budgets, has a canceled subscription → blocked (no second trial).
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status, stripe_subscription_id) VALUES ($1,$2,'canceled','sub_old')")
            .bind(uid).bind(format!("cus_{uid}")).execute(&db).await.unwrap();
        let err = ensure_ready_to_own_budget(&db, uid).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn ready_to_own_entitled_no_budget_is_ok() {
        // Owns 0 budgets but is entitled (e.g. deleted all budgets while active) → allowed, no new trial.
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status, price_id, stripe_subscription_id) VALUES ($1,$2,'active','price_pro_m','sub_live')")
            .bind(uid).bind(format!("cus_{uid}")).execute(&db).await.unwrap();
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn ready_to_own_never_subscribed_starts_trial() {
        // Owns 0 budgets, no subscription row → starts a trial (Stripe mocked).
        let server = MockServer::start().await;
        set_stripe_env(&server);
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        Mock::given(method("POST")).and(path("v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_new"}))).mount(&server).await;
        Mock::given(method("POST")).and(path("v1/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"sub_new","status":"trialing"})))
            .expect(1).mount(&server).await;
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
        clear_stripe_env();
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
}
