//! Provider-agnostic bank-linking operations (nels#320 spec Assumption 8):
//! callers (rag.rs, REST handlers, frontend-facing responses) call these
//! instead of reaching into `financial_connections::*`/`gocardless::*`
//! directly for list/refresh/disconnect/sync — each function matches on
//! `linked_accounts.provider` exactly once.

use axum::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use crate::budget::{check_permission, Permission};
use crate::error::internal_error;
use crate::financial_connections::ListLinkedAccountsResponse;

/// Provider-agnostic — queries the table directly, same as #303's original
/// `financial_connections::list_linked_accounts`. View-or-above only.
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

async fn provider_of(pool: &PgPool, account_id: Uuid, budget_id: Uuid) -> Result<String, (StatusCode, String)> {
    sqlx::query_scalar("SELECT provider FROM linked_accounts WHERE id = $1 AND budget_id = $2")
        .bind(account_id).bind(budget_id)
        .fetch_optional(pool).await.map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Linked account not found".to_string()))
}

/// `cipher` is consumed by the `akahu` and `plaid` arms (their `provider_ref`
/// is encrypted at rest — code review fixes #323 and #321 respectively) —
/// every other provider ignores it. Threaded through here rather than looked
/// up per-call so callers (the REST handler, `rag.rs`'s `REFRESH_BANK_ACCOUNT`
/// action) pass the same `AppState`-derived cipher instance already
/// available to them.
pub async fn refresh_linked_account(
    pool: &PgPool, cipher: &crate::crypto::SecretCipher, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "belvo" => crate::belvo::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "basiq" => crate::basiq::refresh_linked_account(pool, user_id, budget_id, account_id).await,
        "akahu" => crate::akahu::refresh_linked_account(pool, cipher, user_id, budget_id, account_id).await,
        "plaid" => crate::plaid::refresh_linked_account(pool, cipher, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

/// `cipher` is consumed by the `plaid` arm only — Plaid's `/item/remove` call
/// needs the decrypted access_token (code review fix #321). Akahu's own
/// disconnect is local-status-only and never calls Akahu's API, so it does
/// not need it (see `akahu::disconnect_linked_account`'s doc comment).
pub async fn disconnect_linked_account(
    pool: &PgPool, cipher: &crate::crypto::SecretCipher, user_id: Uuid, budget_id: Uuid, account_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match provider_of(pool, account_id, budget_id).await?.as_str() {
        "stripe" => crate::financial_connections::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "gocardless" => crate::gocardless::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "belvo" => crate::belvo::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "basiq" => crate::basiq::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "akahu" => crate::akahu::disconnect_linked_account(pool, user_id, budget_id, account_id).await,
        "plaid" => crate::plaid::disconnect_linked_account(pool, cipher, user_id, budget_id, account_id).await,
        other => Err(internal_error(format!("unknown provider {other}"))),
    }
}

/// Disconnect EVERY still-live linked account that will vanish when `user_id`'s
/// account is deleted — both the rows they linked (`user_id`) and the rows on
/// budgets they own (`budget_id -> budgets.owner_id`), which includes accounts a
/// shared editor linked into an owned budget. Revokes each at its provider via
/// the same dispatch the manual-unlink route uses. Called from
/// `account::delete_account` BEFORE local data is destroyed; a provider failure
/// propagates so the caller aborts the deletion (fail-closed) rather than
/// erasing the local record while the provider authorization stays live (nels#351).
///
/// The set is `status <> 'disconnected'` (i.e. 'active' OR 'consent_expired'),
/// NOT just 'active': a `consent_expired` row can still hold a LIVE provider
/// authorization — e.g. a Plaid Item flipped to `consent_expired` on
/// ITEM_LOGIN_REQUIRED is NOT removed at Plaid — so it must be revoked too, not
/// skipped, before it cascade-deletes and orphans that authorization (nels#351).
///
/// The `budgets` JOIN both unions the two cascade paths and yields each row's
/// budget OWNER, which is passed as the acting user to the reused
/// permission-gated `disconnect_linked_account`. Passing the OWNER (rather than
/// the deleting user) means the `require_edit_or_owner` check inside every
/// provider's disconnect always passes, so a self-deletion is never spuriously
/// 403-blocked for an account the user linked into someone else's budget whose
/// edit-share was later revoked. This changes nothing for the common
/// owner == deleter case, and no provider uses the acting-user param for
/// anything but that permission check (basiq keys its basiq_users lookup off the
/// row's own user_id column, not the param).
///
/// One-at-a-time re-query loop: each successful disconnect flips the processed
/// row (and, for Plaid, its item siblings) to 'disconnected' — out of the
/// `<> 'disconnected'` set — so a provider whose revoke covers multiple rows is
/// never re-invoked for an already-handled row, and the loop terminates.
pub async fn disconnect_all_linked_accounts(
    pool: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    loop {
        let next: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT la.id, la.budget_id, b.owner_id \
             FROM linked_accounts la \
             JOIN budgets b ON b.id = la.budget_id \
             WHERE (la.user_id = $1 OR b.owner_id = $1) AND la.status <> 'disconnected' \
             ORDER BY la.created_at LIMIT 1")
            .bind(user_id)
            .fetch_optional(pool).await.map_err(internal_error)?;
        let Some((account_id, budget_id, owner_id)) = next else { return Ok(()); };
        disconnect_linked_account(pool, cipher, owner_id, budget_id, account_id).await?;
    }
}

// --- REST handlers (replace financial_connections.rs's list/refresh/disconnect handlers) ---
use axum::{extract::{Path, State}, Extension, Json};
use crate::auth::AppState;

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
    refresh_linked_account(&state.db, &state.cipher, user_id, budget_id, account_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn disconnect_linked_account_handler(
    State(state): State<AppState>,
    Path((budget_id, account_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    disconnect_linked_account(&state.db, &state.cipher, user_id, budget_id, account_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- GoCardless-specific REST handlers (start/complete/institutions) ---

#[derive(serde::Deserialize)]
pub struct GcStartLinkRequest {
    pub country: String,
    pub institution_id: String,
}

pub async fn gc_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<GcStartLinkRequest>,
) -> Result<Json<crate::gocardless::GcLinkSessionResponse>, (StatusCode, String)> {
    crate::gocardless::start_link_session(&state.db, user_id, budget_id, &req.country, &req.institution_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct GcCompleteLinkRequest {
    pub gc_ref: Uuid,
}

pub async fn gc_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<GcCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::gocardless::complete_link_session(&state.db, user_id, budget_id, req.gc_ref).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct GcInstitutionsQuery {
    pub country: String,
}

/// Pro-gated (Copilot review finding): the institutions list is only ever
/// needed to start a GoCardless link, and hitting it makes a real GoCardless
/// API call — without this gate, any authenticated user (Pro or not) could
/// trigger unlimited external provider traffic through this endpoint.
pub async fn gc_institutions_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    axum::extract::Query(q): axum::extract::Query<GcInstitutionsQuery>,
) -> Result<Json<Vec<crate::gocardless::Institution>>, (StatusCode, String)> {
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
    crate::gocardless::list_institutions(&q.country).await.map(Json)
}

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
    // Code review finding: without this check, an unsupported/malformed
    // `country` (or one that's valid but resolves to a DIFFERENT provider,
    // e.g. "US") sailed straight through to `start_link_session`'s bare
    // INSERT and only failed on the `belvo_link_sessions.country` CHECK
    // constraint — a raw 500 via `internal_error`, not a clean 400. It also
    // meant a lowercase `"mx"` (which `Provider::for_country` happily
    // accepts, case-insensitively, exactly like the chat path does) 500'd
    // here even though it's a perfectly valid country, since the SQL CHECK
    // is case-sensitive. Validating against the SAME `Provider::for_country`
    // the chat path already uses closes both gaps in one place.
    if crate::bank_provider::Provider::for_country(&req.country) != Some(crate::bank_provider::Provider::Belvo) {
        return Err((StatusCode::BAD_REQUEST, "Unsupported country for Belvo".to_string()));
    }
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

// --- Basiq/Akahu-specific REST handlers (start/complete) ---
// (no institutions handler needed for Basiq/Akahu — spec Assumptions 2-3)

pub async fn basiq_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::basiq::BasiqLinkSessionResponse>, (StatusCode, String)> {
    crate::basiq::create_consent_session(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct BasiqCompleteLinkRequest {
    pub basiq_ref: Uuid,
}

pub async fn basiq_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<BasiqCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::basiq::complete_consent_session(&state.db, user_id, budget_id, req.basiq_ref).await.map(Json)
}

pub async fn akahu_start_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::akahu::AkahuLinkSessionResponse>, (StatusCode, String)> {
    crate::akahu::create_consent_session(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct AkahuCompleteLinkRequest {
    pub akahu_ref: Uuid,
    pub code: String,
}

pub async fn akahu_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<AkahuCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::akahu::complete_consent_session(&state.db, &state.cipher, user_id, budget_id, req.akahu_ref, &req.code).await.map(Json)
}

// --- Plaid-specific REST handlers (link-token/complete, nels#321) ---
// Plaid Link is a client-side embeddable JS modal driven by a link_token
// (closer in shape to Stripe's client_secret than to the redirect-based
// Belvo/Basiq/Akahu/GoCardless flows), so its REST surface returns a
// token/session pair rather than a redirect_url.

pub async fn plaid_link_token_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<crate::plaid::LinkTokenResponse>, (StatusCode, String)> {
    crate::plaid::create_link_token(&state.db, user_id, budget_id).await.map(Json)
}

#[derive(serde::Deserialize)]
pub struct PlaidCompleteLinkRequest {
    pub session_id: Uuid,
    pub public_token: String,
}

pub async fn plaid_complete_link_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<PlaidCompleteLinkRequest>,
) -> Result<Json<ListLinkedAccountsResponse>, (StatusCode, String)> {
    crate::plaid::complete_link_session(&state.db, &state.cipher, user_id, budget_id, req.session_id, &req.public_token).await.map(Json)
}

#[cfg(test)]
mod tests {
    // provider_of/list_linked_accounts/refresh/disconnect are thin DB-backed
    // dispatchers with no pure branching logic worth a unit test beyond what
    // gocardless.rs's and financial_connections.rs's own #[ignore] suites
    // already cover end-to-end; a dedicated bank_linking #[ignore] test
    // confirms the dispatch itself picks the right implementation:
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn test_cipher() -> crate::crypto::SecretCipher {
        crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_dispatches_to_gocardless_for_a_gocardless_row() {
        let db = test_pool().await;
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("bl-{uid}@test.example")).execute(&db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'gocardless', 'acct_bl', 'req_bl', 'consent_expired')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl'")
            .fetch_one(&db).await.unwrap();
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, 'cus_bl', 'trialing')")
            .bind(uid).execute(&db).await.unwrap();

        // consent_expired -> gocardless::refresh_linked_account's own guard
        // fires (409), proving dispatch reached the GoCardless implementation
        // and not Stripe's (which would 404 on a provider mismatch instead).
        let result = refresh_linked_account(&db, &test_cipher(), uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_dispatches_to_basiq_for_a_basiq_row() {
        let db = test_pool().await;
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("bl-basiq-{uid}@test.example")).execute(&db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(&db).await.unwrap();
        // No subscriptions row -> not Pro -> refresh_linked_account's require_pro
        // guard fires 402, proving dispatch reached Basiq's implementation (a
        // GoCardless/Stripe row would 404 on a provider mismatch instead, same
        // reasoning as the existing GoCardless dispatch test).
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'basiq', 'acct_bl_basiq', 'conn_bl_basiq')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl_basiq'")
            .fetch_one(&db).await.unwrap();

        let result = refresh_linked_account(&db, &test_cipher(), uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn disconnect_dispatches_to_akahu_for_an_akahu_row() {
        let db = test_pool().await;
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("bl-akahu-{uid}@test.example")).execute(&db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'akahu', 'acct_bl_akahu', 'user_tok_bl_akahu')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl_akahu'")
            .fetch_one(&db).await.unwrap();

        // Akahu disconnect is NOT Pro-gated and makes zero external calls — this
        // must simply succeed and flip local status, proving dispatch reached
        // Akahu's implementation.
        disconnect_linked_account(&db, &test_cipher(), uid, bid, account_id).await.expect("akahu disconnect ok");
        let status: String = sqlx::query_scalar("SELECT status FROM linked_accounts WHERE id = $1")
            .bind(account_id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "disconnected");

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn refresh_dispatches_to_plaid_for_a_plaid_row() {
        let db = test_pool().await;
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("bl-plaid-{uid}@test.example")).execute(&db).await.unwrap();
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid).bind(uid).execute(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status) \
             VALUES ($1, $2, $3, 'plaid', 'acct_bl_plaid', 'access_bl_plaid', 'consent_expired')")
            .bind(Uuid::new_v4()).bind(bid).bind(uid).execute(&db).await.unwrap();
        let account_id: Uuid = sqlx::query_scalar("SELECT id FROM linked_accounts WHERE provider_account_id = 'acct_bl_plaid'")
            .fetch_one(&db).await.unwrap();
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, 'cus_bl_plaid', 'trialing')")
            .bind(uid).execute(&db).await.unwrap();

        // consent_expired -> plaid::refresh_linked_account's own guard fires
        // (409), proving dispatch reached the Plaid implementation and not a
        // sibling provider's (which would 404 on a provider mismatch instead).
        let result = refresh_linked_account(&db, &test_cipher(), uid, bid, account_id).await;
        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await.unwrap();
    }
}
