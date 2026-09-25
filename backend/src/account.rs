//! Account-level endpoints: GDPR-style data export (this task) and, later,
//! account deletion. The export endpoint streams every record the authenticated
//! user owns (or that is shared with them, by reference only) as a single JSON
//! attachment.
//!
//! Security invariants enforced here:
//! - Chat-message vector `embedding`s are NEVER exported (`ChatMessageExport`
//!   omits the column and the query never selects it).
//! - All queries are scoped to the authenticated `user_id` (owned budgets via
//!   `owner_id`; user-direct rows via `user_id`; shared budgets are reference-only
//!   and exclude the other owner's child records).

use axum::{
    extract::{Extension, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use webauthn_rs::prelude::*;
use webauthn_rs_proto::PublicKeyCredentialRequestOptions;

use crate::auth::AppState;
use crate::db::{
    AuditLog, Budget, Category, Conversation, Goal, GoalContribution, Notification, Reminder,
    Transaction,
};
use crate::error::internal_error;
use crate::passkeys;

/// The full export tree handed to a user. Assembled by `build_account_export`
/// and serialized to pretty JSON by the handler.
#[derive(Serialize)]
pub struct AccountExport {
    pub account: AccountInfo,
    pub owned_budgets: Vec<OwnedBudgetExport>,
    pub chat_messages: Vec<ChatMessageExport>,
    pub conversations: Vec<Conversation>,
    pub notifications: Vec<Notification>,
    pub reminders: Vec<Reminder>,
    pub shared_with_me: Vec<SharedBudgetRef>,
    /// User-scoped audit rows (NULL budget_id) — actions attributable to the user
    /// but not tied to any budget (e.g. issue filing, user-name change). Budget-
    /// scoped rows live under each owned budget's `audit_logs`.
    pub user_audit_logs: Vec<AuditLog>,
    /// The retirement balance sheet (#464). These three sit at the TOP level,
    /// not under `owned_budgets`, because `assets` is `user_id`-scoped and has
    /// no `budget_id` at all — there is no budget to nest them beneath.
    pub assets: Vec<crate::assets::Asset>,
    pub asset_holdings: Vec<crate::assets::AssetHolding>,
    pub asset_balance_history: Vec<crate::assets::AssetBalanceHistory>,
}

/// The exported account profile.
#[derive(Serialize, sqlx::FromRow)]
pub struct AccountInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A budget the user owns, with all of its child records inlined.
#[derive(Serialize)]
pub struct OwnedBudgetExport {
    #[serde(flatten)]
    pub budget: Budget,
    pub categories: Vec<Category>,
    pub transactions: Vec<Transaction>,
    pub goals: Vec<Goal>,
    pub goal_contributions: Vec<GoalContribution>,
    pub shares: Vec<crate::db::BudgetShare>,
    pub audit_logs: Vec<AuditLog>,
}

/// A chat message projected for export. The `text` field is sourced from the
/// `chat_messages.message_text` column. The vector `embedding` is excluded: no
/// Rust struct maps that column anywhere in the codebase, and the export query
/// selects an explicit column list that never names it — so it cannot leak here.
#[derive(Serialize, sqlx::FromRow)]
pub struct ChatMessageExport {
    pub id: Uuid,
    pub sender: String,
    pub text: String,
    pub budget_id: Option<Uuid>,
    pub conversation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A read-only reference to a budget owned by someone ELSE but shared with this
/// user. Deliberately excludes the other owner's categories/transactions/etc.
#[derive(Serialize, sqlx::FromRow)]
pub struct SharedBudgetRef {
    pub budget_id: Uuid,
    pub budget_name: String,
    pub owner_email: String,
    pub permission: String,
}

/// Pure assembler: builds the export tree from already-loaded parts. Kept free
/// of I/O so it can be unit-tested with synthetic data (no DB).
#[allow(clippy::too_many_arguments)]
pub fn build_account_export(
    account: AccountInfo,
    owned_budgets: Vec<OwnedBudgetExport>,
    chat_messages: Vec<ChatMessageExport>,
    conversations: Vec<Conversation>,
    notifications: Vec<Notification>,
    reminders: Vec<Reminder>,
    shared_with_me: Vec<SharedBudgetRef>,
    user_audit_logs: Vec<AuditLog>,
    assets: Vec<crate::assets::Asset>,
    asset_holdings: Vec<crate::assets::AssetHolding>,
    asset_balance_history: Vec<crate::assets::AssetBalanceHistory>,
) -> AccountExport {
    AccountExport {
        account,
        owned_budgets,
        chat_messages,
        conversations,
        notifications,
        reminders,
        shared_with_me,
        user_audit_logs,
        assets,
        asset_holdings,
        asset_balance_history,
    }
}

/// Load the complete export tree for `user_id` from the database. Owner-scoped
/// throughout; never selects `embedding`.
pub async fn fetch_account_export(
    pool: &sqlx::PgPool,
    user_id: Uuid,
) -> Result<AccountExport, (StatusCode, String)> {
    // Account profile — columns are listed explicitly.
    let account = sqlx::query_as::<_, AccountInfo>(
        "SELECT id, email, name, created_at FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(internal_error)?;

    // Owned budgets, each with its child records.
    //
    // This issues a handful of queries per owned budget (an intentional N+1).
    // The export is a deliberately infrequent, per-user GDPR/CCPA operation and
    // a personal account owns only a few budgets, so the round-trip count is
    // small; readability is preferred over a single multi-table JOIN here. If
    // ownership ever grows large (e.g. org accounts), collapse to per-table
    // JOINs keyed on the user's budget ids.
    let budgets = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE owner_id = $1")
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(internal_error)?;

    let mut owned_budgets = Vec::with_capacity(budgets.len());
    for budget in budgets {
        let budget_id = budget.id;

        let categories =
            sqlx::query_as::<_, Category>("SELECT * FROM categories WHERE budget_id = $1")
                .bind(budget_id)
                .fetch_all(pool)
                .await
                .map_err(internal_error)?;

        let transactions =
            sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE budget_id = $1")
                .bind(budget_id)
                .fetch_all(pool)
                .await
                .map_err(internal_error)?;

        let goals = sqlx::query_as::<_, Goal>("SELECT * FROM goals WHERE budget_id = $1")
            .bind(budget_id)
            .fetch_all(pool)
            .await
            .map_err(internal_error)?;

        // Contributions for exactly this budget's goals.
        let goal_contributions = sqlx::query_as::<_, GoalContribution>(
            "SELECT gc.* FROM goal_contributions gc \
             JOIN goals g ON g.id = gc.goal_id \
             WHERE g.budget_id = $1",
        )
        .bind(budget_id)
        .fetch_all(pool)
        .await
        .map_err(internal_error)?;

        let shares = sqlx::query_as::<_, crate::db::BudgetShare>(
            "SELECT * FROM budget_shares WHERE budget_id = $1",
        )
        .bind(budget_id)
        .fetch_all(pool)
        .await
        .map_err(internal_error)?;

        let audit_logs =
            sqlx::query_as::<_, AuditLog>("SELECT * FROM audit_logs WHERE budget_id = $1")
                .bind(budget_id)
                .fetch_all(pool)
                .await
                .map_err(internal_error)?;

        owned_budgets.push(OwnedBudgetExport {
            budget,
            categories,
            transactions,
            goals,
            goal_contributions,
            shares,
            audit_logs,
        });
    }

    // Chat messages — explicit column list, embedding deliberately excluded.
    let chat_messages = sqlx::query_as::<_, ChatMessageExport>(
        "SELECT id, sender, message_text AS text, budget_id, conversation_id, created_at \
         FROM chat_messages WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    // Explicit column list: the `conversations` table has a `user_id` column
    // that `db::Conversation` does not map, so `SELECT *` would carry an unmapped
    // column. Mirrors `rag::list_conversations`.
    let conversations = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, created_at, updated_at FROM conversations WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    let notifications =
        sqlx::query_as::<_, Notification>("SELECT * FROM notifications WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(internal_error)?;

    let reminders = sqlx::query_as::<_, Reminder>("SELECT * FROM reminders WHERE user_id = $1")
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(internal_error)?;

    // Budgets owned by OTHERS but shared with this user (reference only).
    let shared_with_me = sqlx::query_as::<_, SharedBudgetRef>(
        "SELECT b.id AS budget_id, b.name AS budget_name, u2.email AS owner_email, \
                bs.permission_level AS permission \
         FROM budget_shares bs \
         JOIN budgets b ON b.id = bs.budget_id \
         JOIN users u2 ON u2.id = b.owner_id \
         WHERE bs.shared_with_email = (SELECT email FROM users WHERE id = $1) \
           AND b.owner_id <> $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    // User-scoped audit rows (NULL budget_id) — attributable to the user but not
    // tied to any budget. Budget-scoped rows are already nested per owned budget.
    let user_audit_logs = sqlx::query_as::<_, AuditLog>(
        "SELECT * FROM audit_logs WHERE user_id = $1 AND budget_id IS NULL \
         ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    // Retirement balance sheet (#464). User-scoped, so these are filtered on
    // `user_id` directly rather than nested under an owned budget — `assets`
    // has no `budget_id` column and no query here may introduce one.
    //
    // `securities` is deliberately NOT exported. It is a SHARED, NON-PERSONAL
    // cache of instrument metadata ("VTSAX is a mutual fund") with no `user_id`
    // column and no per-user rows, so it holds no personal data of this or any
    // other data subject. Exporting it would ship a global vendor catalog into
    // one person's DSAR response. The personal facts — which instrument, how
    // much of it — live in `asset_holdings`, which IS exported below, and each
    // holding carries the `security_id` needed to resolve the rest.
    let assets = sqlx::query_as::<_, crate::assets::Asset>("SELECT * FROM assets WHERE user_id = $1")
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(internal_error)?;

    let asset_holdings = sqlx::query_as::<_, crate::assets::AssetHolding>(
        "SELECT ah.* FROM asset_holdings ah \
         JOIN assets a ON a.id = ah.asset_id \
         WHERE a.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    let asset_balance_history = sqlx::query_as::<_, crate::assets::AssetBalanceHistory>(
        "SELECT h.* FROM asset_balance_history h \
         JOIN assets a ON a.id = h.asset_id \
         WHERE a.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    Ok(build_account_export(
        account,
        owned_budgets,
        chat_messages,
        conversations,
        notifications,
        reminders,
        shared_with_me,
        user_audit_logs,
        assets,
        asset_holdings,
        asset_balance_history,
    ))
}

/// `GET /api/account/export` — return the authenticated user's full data export
/// as a downloadable JSON attachment.
pub async fn export_account(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Response, (StatusCode, String)> {
    let export = fetch_account_export(&state.db, user_id).await?;

    // Summary metrics for the operator log, computed before the value is moved
    // into serialization.
    let owned_budgets_len = export.owned_budgets.len();
    let total_tx: usize = export
        .owned_budgets
        .iter()
        .map(|b| b.transactions.len())
        .sum();
    let msgs_len = export.chat_messages.len();

    let json_string = serde_json::to_string_pretty(&export).map_err(internal_error)?;

    tracing::info!(
        %user_id,
        budgets = owned_budgets_len,
        transactions = total_tx,
        chat_messages = msgs_len,
        "data export generated"
    );

    let filename = format!("nels-export-{}.json", Utc::now().format("%Y-%m-%d"));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(axum::body::Body::from(json_string))
        .map_err(internal_error)
}

/// Body of `POST /api/account/delete-challenge`'s response: a WebAuthn
/// authentication challenge the client must answer to prove possession of a
/// live passkey before the irreversible `DELETE /api/account` is honored.
#[derive(Serialize)]
pub struct DeleteChallengeResponse {
    pub flow_id: String,
    pub options: PublicKeyCredentialRequestOptions,
}

/// Body of `DELETE /api/account`. All three fields are required: a re-typed
/// confirmation email, the `flow_id` from `delete-challenge`, and the
/// resulting WebAuthn assertion — making deletion a deliberate,
/// re-authenticated, irreversible action.
#[derive(Deserialize)]
pub struct DeleteAccountRequest {
    pub confirm_email: String,
    pub flow_id: String,
    pub credential: PublicKeyCredential,
}

/// Pure comparison used to confirm intent: the two emails match iff they are
/// equal after trimming surrounding whitespace and lowercasing. An empty
/// provided value never matches a real (non-empty) account email.
pub fn confirm_email_matches(authed: &str, provided: &str) -> bool {
    authed.trim().to_lowercase() == provided.trim().to_lowercase()
}

/// Irreversibly delete the user and every record tied to them, atomically.
/// Returns `true` if a user row was actually removed, `false` if it was already
/// gone (idempotent no-op, e.g. a concurrent/duplicate delete).
///
/// Deletion ordering, all in ONE transaction:
///  1. `sessions` first, to invalidate the caller's own bearer token before any
///     data is touched (closes the window where a concurrent request with the
///     same token could act on partially-deleted state).
///  2. The user's footprint on data owned by OTHER users. These FKs are
///     `SET NULL` (`audit_logs.user_id`, `goal_contributions.user_id`) or absent
///     (`auth_flows.user_id` has no FK; `budget_shares` references the user only
///     by email). A plain `DELETE FROM users` cascade would therefore LEAVE
///     these rows behind (merely nulling the actor, or not touching them at
///     all). For a GDPR/CCPA right-to-erasure the user's data must be removed,
///     not de-attributed, so we delete these rows explicitly and deliberately —
///     this also erases the user's contributions/audit entries on budgets shared
///     with them (a documented consequence, see docs/data-privacy-deletion.md).
///  3. `DELETE FROM users`, which cascades all of the user's OWN owned data
///     (budgets and their categories/transactions/goals/contributions/shares/
///     audit_logs), chat messages and their embeddings, conversations,
///     notifications, and reminders.
///
/// Any error before `commit` rolls the whole transaction back (nothing deleted).
pub(crate) async fn delete_user_data(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    email: &str,
) -> Result<bool, (StatusCode, String)> {
    let mut tx = pool.begin().await.map_err(internal_error)?;

    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    sqlx::query("DELETE FROM audit_logs WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    sqlx::query("DELETE FROM goal_contributions WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    sqlx::query("DELETE FROM budget_shares WHERE shared_with_email = $1")
        .bind(email)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    sqlx::query("DELETE FROM auth_flows WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    sqlx::query("DELETE FROM subscriptions WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    // `retirement_profiles.user_id` is ON DELETE CASCADE, so this is
    // belt-and-braces rather than strictly required — the same symmetry the
    // `subscriptions` delete above keeps. It states the intent in this function,
    // where a reader auditing "what does account deletion remove?" will look,
    // rather than only in a migration.
    sqlx::query("DELETE FROM retirement_profiles WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    let users_deleted = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?
        .rows_affected();

    tx.commit().await.map_err(internal_error)?;
    Ok(users_deleted > 0)
}

/// `POST /api/account/delete-challenge` — start the WebAuthn re-authentication
/// required before `DELETE /api/account` is honored. The caller is already an
/// authenticated session; this just proves they still hold a live passkey for
/// this app right now, before the irreversible step.
pub async fn delete_challenge(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    headers: HeaderMap,
) -> Result<Json<DeleteChallengeResponse>, (StatusCode, String)> {
    let rp = state.webauthn.resolve_rp(&headers)?;

    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;
    let email = match email {
        Some(e) => e,
        None => return Err((StatusCode::NOT_FOUND, "User not found".to_string())),
    };

    let creds = passkeys::load_passkeys(&state.db, user_id, rp).await?;
    if creds.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No passkey registered for this app on this device.".to_string(),
        ));
    }

    let (rcr, auth_state) = state
        .webauthn
        .get(rp)
        .start_passkey_authentication(&creds)
        .map_err(|e| internal_error(format!("Failed to start passkey authentication: {e}")))?;
    let ceremony_state = serde_json::to_value(&auth_state)
        .map_err(|e| internal_error(format!("Failed to serialize authentication state: {e}")))?;

    let flow_id = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO auth_flows (flow_id, flow_type, user_id, email, rp, ceremony_state, expires_at) \
         VALUES ($1, 'delete', $2, $3, $4, $5, NOW() + INTERVAL '{}')",
        crate::auth::FLOW_TTL
    ))
    .bind(&flow_id)
    .bind(user_id)
    .bind(&email)
    .bind(rp)
    .bind(&ceremony_state)
    .execute(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to store delete-challenge flow: {}", e)))?;

    Ok(Json(DeleteChallengeResponse { flow_id, options: rcr.public_key }))
}

/// `DELETE /api/account` — permanently delete the authenticated user's account
/// and all associated data after re-confirming intent (matching email) and
/// re-authenticating with a fresh WebAuthn assertion against the challenge
/// from `delete_challenge`. Returns 204 No Content on success.
pub async fn delete_account(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<DeleteAccountRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let challenge: Option<(Uuid, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT user_id, email, rp, ceremony_state FROM auth_flows \
         WHERE flow_id = $1 AND flow_type = 'delete' AND expires_at > NOW()"
    )
    .bind(&payload.flow_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let (flow_user_id, user_email, rp, ceremony_state) = match challenge {
        Some(data) => data,
        None => return Err((StatusCode::BAD_REQUEST, "Invalid or expired delete challenge".to_string())),
    };

    // The challenge must belong to the same authenticated caller — never trust
    // flow_id alone to identify whose account this deletes.
    if flow_user_id != user_id {
        return Err((StatusCode::FORBIDDEN, "This challenge does not belong to your account".to_string()));
    }

    if !confirm_email_matches(&user_email, &payload.confirm_email) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Confirmation email does not match".to_string(),
        ));
    }

    let rp = passkeys::to_rp(&rp);
    let auth_state: PasskeyAuthentication = serde_json::from_value(ceremony_state)
        .map_err(|e| internal_error(format!("Corrupt authentication state: {e}")))?;

    state
        .webauthn
        .get(rp)
        .finish_passkey_authentication(&payload.credential, &auth_state)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Passkey verification failed: {e}")))?;

    let _ = sqlx::query("DELETE FROM auth_flows WHERE flow_id = $1")
        .bind(&payload.flow_id)
        .execute(&state.db)
        .await;

    finish_account_deletion(&state, user_id, &user_email).await
}

/// The actual (irreversible) side effects of account deletion, once
/// re-authentication has already been verified by the caller: cancel any
/// live Stripe subscription, revoke any live bank-provider authorization,
/// then cascade-delete the user's data. Split out from `delete_account` so
/// this ordering/behavior can be tested directly without needing a full
/// WebAuthn ceremony (which the passkey-ceremony logic itself is tested
/// against separately).
pub(crate) async fn finish_account_deletion(
    state: &AppState,
    user_id: Uuid,
    user_email: &str,
) -> Result<StatusCode, (StatusCode, String)> {
    // Cancel the Stripe subscription BEFORE destroying local data. If this fails
    // we abort so we never delete the account while a paid subscription lives on
    // (the user could no longer reach the billing portal to cancel it).
    crate::billing::cancel_subscription(&state.db, user_id).await?;

    // Revoke every still-live bank-provider authorization (at the provider
    // where supported — Akahu is local-only) BEFORE destroying local
    // data. Fail-closed like the subscription cancel above: if a provider
    // disconnect fails we abort so we never delete the account while a live
    // bank authorization survives with no local record (nels#351).
    crate::bank_linking::disconnect_all_linked_accounts(&state.db, &state.cipher, user_id).await?;

    let deleted = delete_user_data(&state.db, user_id, user_email).await?;
    if !deleted {
        // The user was removed by a concurrent/duplicate request between our
        // lookup and the transaction. Report honestly rather than a false 204.
        return Err((StatusCode::NOT_FOUND, "User not found".to_string()));
    }
    tracing::info!(%user_id, "account and all associated data deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The assembled export must contain the account email and a chat message's
    /// text, but must NEVER contain the substring "embedding".
    #[test]
    fn build_account_export_excludes_secret_and_embedding() {
        let account = AccountInfo {
            id: Uuid::new_v4(),
            email: "alice@example.com".to_string(),
            name: Some("Alice".to_string()),
            created_at: Utc::now(),
        };

        let chat = ChatMessageExport {
            id: Uuid::new_v4(),
            sender: "user".to_string(),
            text: "remember my secret budget plan".to_string(),
            budget_id: None,
            conversation_id: None,
            created_at: Utc::now(),
        };

        // Include an owned budget so the `#[serde(flatten)] budget: Budget` path
        // is covered by the no-leak assertions below — a future field added to
        // `db::Budget` named embedding would be caught here.
        let owned = OwnedBudgetExport {
            budget: Budget {
                id: Uuid::new_v4(),
                owner_id: account.id,
                name: "Household".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: Some(1000.0),
                amount_mode: "derived".to_string(),
                is_default: true,
                rollover_enabled: false,
                budget_type: "time_based".to_string(),
                closed_at: None,
                archived_at: None,
                auto_renew: false,
                next_renewal_at: None,
                rollup_parent_id: None,
                budget_strategy: "limit_spent_remaining".to_string(),
                currency: "USD".to_string(),
                created_at: Utc::now(),
            },
            categories: Vec::new(),
            transactions: Vec::new(),
            goals: Vec::new(),
            goal_contributions: Vec::new(),
            shares: Vec::new(),
            audit_logs: Vec::new(),
        };

        let user_audit = AuditLog {
            id: Uuid::new_v4(),
            budget_id: None,
            user_id: Some(account.id),
            action: "AI_REPORT_ISSUE".to_string(),
            details: None,
            created_at: Utc::now(),
        };

        // Retirement balance sheet (#464). Included with real rows rather than
        // empty vecs so the DSAR coverage is PINNED rather than assumed: the
        // assertions below fail if a key stops being emitted, gets renamed, or
        // is dropped from `AccountExport` entirely. Nothing else in the
        // always-run suite would notice, because these tables are empty for
        // every real user until #468 ships a write path — which is exactly when
        // a silent gap would stop being harmless.
        let asset_id = Uuid::new_v4();
        let asset = crate::assets::Asset {
            id: asset_id,
            user_id: account.id,
            owner_member_id: None,
            linked_account_id: None,
            name: "Alice 401k".to_string(),
            asset_type: crate::assets::AssetType::RetirementAccount,
            tax_treatment: crate::assets::TaxTreatment::PreTax,
            institution_name: Some("Example Trust".to_string()),
            current_balance: Some(1000.0),
            currency: "USD".to_string(),
            balance_as_of: None,
            is_manual: false,
            status: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let holding = crate::assets::AssetHolding {
            id: Uuid::new_v4(),
            asset_id,
            security_id: Uuid::new_v4(),
            quantity: 12.5,
            cost_basis: None,
            market_value: 1000.0,
            as_of: Utc::now(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let history = crate::assets::AssetBalanceHistory {
            id: Uuid::new_v4(),
            asset_id,
            as_of: chrono::NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
            balance: 1000.0,
            created_at: Utc::now(),
        };

        let export = build_account_export(
            account,
            vec![owned],
            vec![chat],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![user_audit],
            vec![asset],
            vec![holding],
            vec![history],
        );

        let json = serde_json::to_string(&export).expect("serializes");

        let tree: serde_json::Value = serde_json::from_str(&json).expect("export parses as json");
        for key in ["assets", "asset_holdings", "asset_balance_history"] {
            let rows = tree
                .get(key)
                .unwrap_or_else(|| panic!("export must carry an `{key}` key"));
            assert_eq!(
                rows.as_array().map(Vec::len),
                Some(1),
                "`{key}` must carry its row into the export"
            );
        }
        assert!(
            json.contains("Alice 401k"),
            "the asset's name must reach the export body"
        );

        assert!(json.contains("alice@example.com"), "email must be present");
        assert!(json.contains("Household"), "owned budget must be present");
        assert!(
            json.contains("remember my secret budget plan"),
            "chat message text must be present"
        );
        assert!(
            !json.contains("embedding"),
            "export must not leak embedding"
        );
        assert!(
            json.contains("AI_REPORT_ISSUE"),
            "user-scoped audit action must be present"
        );
    }

    /// Confirmation matching is case-insensitive and whitespace-trimmed, but a
    /// different address (or an empty provided value) must never match.
    #[test]
    fn confirm_email_matches_normalizes_and_rejects() {
        assert!(confirm_email_matches(
            "alice@example.com",
            "  ALICE@Example.com  "
        ));
        assert!(!confirm_email_matches("alice@example.com", "bob@example.com"));
        assert!(!confirm_email_matches("alice@example.com", ""));
    }

    // Integration tests run on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test account -- --ignored
    // They seed isolated users/budgets with unique UUIDs + emails and clean up
    // after, so they are safe to run repeatedly against the shared dev DB.

    /// Small helper: scalar COUNT(*) for an arbitrary single-bind WHERE query.
    async fn count_by_uuid(pool: &sqlx::PgPool, sql: &str, id: Uuid) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count query failed [{sql}]: {e}"))
    }

    async fn count_by_text(pool: &sqlx::PgPool, sql: &str, val: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(val)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count query failed [{sql}]: {e}"))
    }

    /// `delete_user_data` must erase EVERY record tied to the user — both their
    /// own owned data (which cascades from `users`) and their footprint on data
    /// owned by OTHER users (budget_shares by email, goal_contributions and
    /// audit_logs by user_id, which would otherwise be left behind by a naive
    /// cascade) — while leaving other users' data fully intact.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn account_deletion_removes_all_user_data() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        // --- User U: the account to be deleted, with one row in every table. ---
        let u = Uuid::new_v4();
        let u_email = format!("del-test-u-{u}@example.test");
        let bu = Uuid::new_v4(); // budget owned by U
        let cat_u = Uuid::new_v4();
        let tx_u = Uuid::new_v4();
        let gu = Uuid::new_v4(); // goal in BU
        let conv_u = Uuid::new_v4();
        // Retirement balance sheet (#464): an asset plus the two tables that
        // hang off it. `sec_u` is NOT user data -- it is a row in the shared,
        // global securities cache, and it must SURVIVE the deletion.
        let asset_u = Uuid::new_v4();
        let sec_u = Uuid::new_v4();

        // --- User V: survivor, with their own budget + goal. ---
        let v = Uuid::new_v4();
        let v_email = format!("del-test-v-{v}@example.test");
        let bv = Uuid::new_v4(); // budget owned by V
        let gv = Uuid::new_v4(); // goal in BV

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(u)
            .bind(&u_email)
            .execute(&pool)
            .await
            .expect("seed user U");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(v)
            .bind(&v_email)
            .execute(&pool)
            .await
            .expect("seed user V");

        // U's own data.
        sqlx::query(
            "INSERT INTO sessions (token, user_id, expires_at) \
             VALUES ($1, $2, NOW() + INTERVAL '30 days')",
        )
        .bind(format!("sess-{u}"))
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed session");

        sqlx::query(
            "INSERT INTO auth_flows (flow_id, flow_type, user_id, email, expires_at) \
             VALUES ($1, 'login', $2, $3, NOW() + INTERVAL '30 days')",
        )
        .bind(format!("flow-{u}"))
        .bind(u)
        .bind(&u_email)
        .execute(&pool)
        .await
        .expect("seed auth_flow");

        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'U budget', 'monthly', 100.0)",
        )
        .bind(bu)
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed budget BU");

        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type) \
             VALUES ($1, $2, 'U cat', 'expense')",
        )
        .bind(cat_u)
        .bind(bu)
        .execute(&pool)
        .await
        .expect("seed category");

        sqlx::query(
            "INSERT INTO transactions (id, budget_id, amount, description) \
             VALUES ($1, $2, 12.0, 'U tx')",
        )
        .bind(tx_u)
        .bind(bu)
        .execute(&pool)
        .await
        .expect("seed transaction");

        sqlx::query(
            "INSERT INTO goals (id, budget_id, name, goal_type, target_amount) \
             VALUES ($1, $2, 'U goal', 'savings', 100.0)",
        )
        .bind(gu)
        .bind(bu)
        .execute(&pool)
        .await
        .expect("seed goal GU");

        sqlx::query(
            "INSERT INTO goal_contributions (id, goal_id, user_id, amount) \
             VALUES ($1, $2, $3, 10.0)",
        )
        .bind(Uuid::new_v4())
        .bind(gu)
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed goal_contribution in GU");

        sqlx::query("INSERT INTO audit_logs (id, budget_id, user_id, action) VALUES ($1, $2, $3, 'created')")
            .bind(Uuid::new_v4())
            .bind(bu)
            .bind(u)
            .execute(&pool)
            .await
            .expect("seed audit_log in BU");

        // Conversation must exist before the chat message: chat_messages has an
        // FK on conversation_id REFERENCES conversations(id).
        sqlx::query("INSERT INTO conversations (id, user_id) VALUES ($1, $2)")
            .bind(conv_u)
            .bind(u)
            .execute(&pool)
            .await
            .expect("seed conversation");

        sqlx::query(
            "INSERT INTO chat_messages (id, user_id, budget_id, conversation_id, sender, message_text, embedding) \
             VALUES ($1, $2, $3, $4, 'user', 'hi', array_fill(0::real, ARRAY[768])::vector)",
        )
        .bind(Uuid::new_v4())
        .bind(u)
        .bind(bu)
        .bind(conv_u)
        .execute(&pool)
        .await
        .expect("seed chat_message");

        sqlx::query(
            "INSERT INTO notifications (id, user_id, kind, message) \
             VALUES ($1, $2, 'reminder', 'hello')",
        )
        .bind(Uuid::new_v4())
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed notification");

        sqlx::query(
            "INSERT INTO reminders (id, user_id, message, cadence, next_fire_at) \
             VALUES ($1, $2, 'do it', 'daily', NOW())",
        )
        .bind(Uuid::new_v4())
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed reminder");

        // Retirement balance sheet (#464). `delete_user_data` names NONE of
        // these three tables: the migration's comment and AGENTS.md both assert
        // that erasure stays complete because `users` -> `assets` ->
        // `{asset_holdings, asset_balance_history}` cascades. That claim was
        // prose only. Worse, `assets::tests::cleanup` deletes `FROM assets`
        // explicitly, so the cascade the GDPR erasure guarantee rests on was
        // never executed anywhere in the suite. If someone later changes
        // `asset_holdings.asset_id` to RESTRICT, `delete_user_data` starts
        // failing in production; these three seeds are what says so first.
        sqlx::query(
            "INSERT INTO assets (id, user_id, name, asset_type, tax_treatment, current_balance) \
             VALUES ($1, $2, 'U 401k', 'retirement_account', 'pre_tax', 1000.0)",
        )
        .bind(asset_u)
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed asset");

        sqlx::query(
            "INSERT INTO securities (id, provider, provider_security_id, security_type) \
             VALUES ($1, 'plaid', $2, 'equity')",
        )
        .bind(sec_u)
        .bind(format!("del-sec-{sec_u}"))
        .execute(&pool)
        .await
        .expect("seed security");

        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value) \
             VALUES ($1, $2, $3, 10.0, 1000.0)",
        )
        .bind(Uuid::new_v4())
        .bind(asset_u)
        .bind(sec_u)
        .execute(&pool)
        .await
        .expect("seed asset_holding");

        sqlx::query(
            "INSERT INTO asset_balance_history (id, asset_id, as_of, balance) \
             VALUES ($1, $2, DATE '2026-07-28', 1000.0)",
        )
        .bind(Uuid::new_v4())
        .bind(asset_u)
        .execute(&pool)
        .await
        .expect("seed asset_balance_history");

        // V's own data.
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'V budget', 'monthly', 100.0)",
        )
        .bind(bv)
        .bind(v)
        .execute(&pool)
        .await
        .expect("seed budget BV");

        sqlx::query(
            "INSERT INTO goals (id, budget_id, name, goal_type, target_amount) \
             VALUES ($1, $2, 'V goal', 'savings', 100.0)",
        )
        .bind(gv)
        .bind(bv)
        .execute(&pool)
        .await
        .expect("seed goal GV");

        // U's footprint on V's data: a share granted to U by email, plus U's
        // contribution and audit entry on V's budget/goal.
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(bv)
        .bind(&u_email)
        .execute(&pool)
        .await
        .expect("seed budget_share on BV for U");

        sqlx::query(
            "INSERT INTO goal_contributions (id, goal_id, user_id, amount) \
             VALUES ($1, $2, $3, 5.0)",
        )
        .bind(Uuid::new_v4())
        .bind(gv)
        .bind(u)
        .execute(&pool)
        .await
        .expect("seed U contribution to GV");

        sqlx::query("INSERT INTO audit_logs (id, budget_id, user_id, action) VALUES ($1, $2, $3, 'viewed')")
            .bind(Uuid::new_v4())
            .bind(bv)
            .bind(u)
            .execute(&pool)
            .await
            .expect("seed U audit on BV");

        // V's OWN rows on the same SET-NULL tables, to prove the WHERE user_id = U
        // deletes are scoped and do NOT over-delete a co-owner's history.
        let v_contribution = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO goal_contributions (id, goal_id, user_id, amount) VALUES ($1, $2, $3, 9.0)",
        )
        .bind(v_contribution)
        .bind(gv)
        .bind(v)
        .execute(&pool)
        .await
        .expect("seed V contribution to GV");

        let v_audit = Uuid::new_v4();
        sqlx::query("INSERT INTO audit_logs (id, budget_id, user_id, action) VALUES ($1, $2, $3, 'viewed')")
            .bind(v_audit)
            .bind(bv)
            .bind(v)
            .execute(&pool)
            .await
            .expect("seed V audit on BV");

        // --- Act ---
        let deleted = super::delete_user_data(&pool, u, &u_email)
            .await
            .expect("delete ok");
        assert!(deleted, "a user row was removed");

        // --- Assert: every trace of U is gone. ---
        macro_rules! zero_u {
            ($sql:expr, $msg:expr) => {
                assert_eq!(count_by_uuid(&pool, $sql, u).await, 0, $msg);
            };
        }
        zero_u!("SELECT COUNT(*) FROM users WHERE id = $1", "user gone");
        zero_u!(
            "SELECT COUNT(*) FROM sessions WHERE user_id = $1",
            "sessions gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM auth_flows WHERE user_id = $1",
            "auth_flows gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM budgets WHERE owner_id = $1",
            "owned budgets gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM chat_messages WHERE user_id = $1",
            "chat_messages gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM chat_messages WHERE user_id = $1 AND embedding IS NOT NULL",
            "chat embeddings gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM conversations WHERE user_id = $1",
            "conversations gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM notifications WHERE user_id = $1",
            "notifications gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM reminders WHERE user_id = $1",
            "reminders gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM goal_contributions WHERE user_id = $1",
            "U contributions (incl. on GV) gone"
        );
        zero_u!(
            "SELECT COUNT(*) FROM audit_logs WHERE user_id = $1",
            "U audit logs (incl. on BV) gone"
        );
        assert_eq!(
            count_by_text(
                &pool,
                "SELECT COUNT(*) FROM budget_shares WHERE shared_with_email = $1",
                &u_email
            )
            .await,
            0,
            "U's share grant on V's budget removed"
        );
        // BU cascaded, so its children are gone too (scoped by budget id).
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM categories WHERE budget_id = $1", bu).await,
            0,
            "BU categories cascaded"
        );
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM transactions WHERE budget_id = $1", bu).await,
            0,
            "BU transactions cascaded"
        );
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM goals WHERE budget_id = $1", bu).await,
            0,
            "BU goals cascaded"
        );

        // The #464 cascade chain, asserted rather than assumed. `assets` goes
        // by user_id; the two children have NO user_id of their own and can
        // only be reached through the asset, which is the whole point.
        zero_u!(
            "SELECT COUNT(*) FROM assets WHERE user_id = $1",
            "assets gone"
        );
        assert_eq!(
            count_by_uuid(
                &pool,
                "SELECT COUNT(*) FROM asset_holdings WHERE asset_id = $1",
                asset_u
            )
            .await,
            0,
            "asset_holdings cascaded with the asset"
        );
        assert_eq!(
            count_by_uuid(
                &pool,
                "SELECT COUNT(*) FROM asset_balance_history WHERE asset_id = $1",
                asset_u
            )
            .await,
            0,
            "asset_balance_history cascaded with the asset"
        );
        // And the negative: `securities` is a SHARED, non-personal instrument
        // cache with no user_id. Erasing a user must not reach into it, or one
        // person's deletion would strip metadata every other user's holdings
        // still reference.
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM securities WHERE id = $1", sec_u).await,
            1,
            "the shared securities row must NOT be deleted with the user"
        );

        // --- Assert: V's data survives untouched. ---
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM users WHERE id = $1", v).await,
            1,
            "user V survives"
        );
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM budgets WHERE id = $1", bv).await,
            1,
            "budget BV survives"
        );
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM goals WHERE id = $1", gv).await,
            1,
            "goal GV survives"
        );
        // V's own SET-NULL rows must NOT be over-deleted by U's by-user_id deletes.
        assert_eq!(
            count_by_uuid(
                &pool,
                "SELECT COUNT(*) FROM goal_contributions WHERE id = $1",
                v_contribution
            )
            .await,
            1,
            "V's own contribution survives (delete scoped to U)"
        );
        assert_eq!(
            count_by_uuid(&pool, "SELECT COUNT(*) FROM audit_logs WHERE id = $1", v_audit).await,
            1,
            "V's own audit row survives (delete scoped to U)"
        );

        // --- Cleanup: U is already gone; remove V (cascades its own data) and
        // defensively any stray rows keyed by V's email/id. ---
        sqlx::query("DELETE FROM budget_shares WHERE shared_with_email = $1")
            .bind(&v_email)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(v)
            .execute(&pool)
            .await
            .ok();
        // The seeded securities row survives on purpose (asserted above), so
        // the test has to take it away itself. Its holdings cascaded with U's
        // asset, so the ON DELETE RESTRICT no longer blocks this.
        sqlx::query("DELETE FROM securities WHERE id = $1")
            .bind(sec_u)
            .execute(&pool)
            .await
            .ok();
    }

    /// Deleting an account with an active Stripe subscription must cancel that
    /// subscription first (before local data is destroyed), so the user is never
    /// left with a live paid subscription they can no longer reach via the
    /// billing portal.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_cancels_stripe_then_deletes() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};
        let server = MockServer::start().await;
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        Mock::given(method("DELETE")).and(path("/v1/subscriptions/sub_del"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id":"sub_del","status":"canceled"})))
            .expect(1).mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        let user_id = Uuid::new_v4();
        let email = format!("del-stripe-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(&email).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_del', 'sub_del')")
            .bind(user_id).execute(&pool).await.unwrap();
        let resp = finish_account_deletion(&state, user_id, &email).await;
        assert!(matches!(resp, Ok(StatusCode::NO_CONTENT)), "got {resp:?}");

        let cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(cnt, 0);
        // Ties the Stripe-cancel path to the local cascade: the AC asks for
        // "the subscription is cancelled at Stripe on account delete, AND the
        // local cascade still occurs" — the users-row check above proves the
        // account is gone, this proves the subscriptions row cascade-deleted
        // alongside it in the same run (not merely unaffected/skipped).
        let sub_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM subscriptions WHERE user_id=$1")
            .bind(user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(sub_cnt, 0, "subscriptions row must cascade-delete alongside the user");
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    /// If the Stripe cancellation call fails, `delete_account` must abort BEFORE
    /// destroying local data — leaving the account fully intact so the user (or
    /// an operator) can retry rather than losing the account while a paid
    /// subscription silently lives on.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_aborts_when_stripe_cancel_fails() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::method;
        let server = MockServer::start().await;
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(500))
            .mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        let user_id = Uuid::new_v4();
        let email = format!("del-stripe-fail-{user_id}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(&email).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_x', 'sub_x')")
            .bind(user_id).execute(&pool).await.unwrap();
        let resp = finish_account_deletion(&state, user_id, &email).await;
        assert!(resp.is_err(), "Stripe failure must abort deletion");

        let cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(cnt, 1, "account must still exist after aborted delete");

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    /// Deleting an account must revoke still-live bank authorizations at the
    /// provider BEFORE local data is destroyed. This exercises representative
    /// providers — Stripe Financial Connections (external POST), GoCardless
    /// (external DELETE), and Akahu (local-only, no provider call) — across
    /// BOTH row-selection paths: rows the user linked themselves
    /// (`linked_accounts.user_id`) AND rows on budgets they own
    /// (`budget_id -> budgets.owner_id`), which includes an account a shared
    /// editor linked into an owned budget — then completes the local cascade.
    /// Fail-closed like the Stripe-cancel step above (nels#351).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_disconnects_linked_accounts_then_deletes() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, path_regex};
        let server = MockServer::start().await;
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        std::env::set_var("GOCARDLESS_API_BASE", server.uri());
        std::env::set_var("GOCARDLESS_SECRET_ID", "sid_test");
        std::env::set_var("GOCARDLESS_SECRET_KEY", "skey_test");
        // Akahu makes no external call on disconnect, but point it at the mock
        // server defensively so nothing can escape to a real endpoint.
        std::env::set_var("AKAHU_API_BASE", server.uri());

        // GoCardless token (mirrors gocardless.rs's `mount_token`); no `.expect`
        // because the token may already be cached from an earlier serial test.
        Mock::given(method("POST")).and(path("/token/new/"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access": "tok_x", "access_expires": 3600})))
            .mount(&server).await;
        // Stripe Financial Connections disconnect.
        Mock::given(method("POST")).and(path_regex(r"/v1/financial_connections/accounts/.*/disconnect$"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": "fca_del351", "object": "financial_connections.account"})))
            .expect(1).mount(&server).await;
        // GoCardless requisition delete.
        Mock::given(method("DELETE")).and(path_regex(r"/requisitions/.*"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1).mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        // Owner U, with a budget BU.
        let u = Uuid::new_v4();
        let u_email = format!("del-bank-u-{u}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(u).bind(&u_email).execute(&pool).await.unwrap();
        let bu = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'U budget','monthly',100.0)")
            .bind(bu).bind(u).execute(&pool).await.unwrap();

        // Stripe FC row on BU, linked by U (path-(a): user_id = U).
        let stripe_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'stripe', 'fca_del351', 'fca_del351')")
            .bind(stripe_la).bind(bu).bind(u).execute(&pool).await.unwrap();
        // Akahu row on BU, linked by U (local-only disconnect, no external call).
        let akahu_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'akahu', 'akahu_del351', 'akahu_tok_del351')")
            .bind(akahu_la).bind(bu).bind(u).execute(&pool).await.unwrap();

        // Editor E, shared edit on BU, who linked a GoCardless row into BU
        // (path-(b): budget_id -> budgets.owner_id = U, but user_id = E).
        let e = Uuid::new_v4();
        let e_email = format!("del-bank-e-{e}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(e).bind(&e_email).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) VALUES ($1,$2,$3,'edit')")
            .bind(Uuid::new_v4()).bind(bu).bind(&e_email).execute(&pool).await.unwrap();
        let gc_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'gocardless', 'gc_del351', 'req_del351')")
            .bind(gc_la).bind(bu).bind(e).execute(&pool).await.unwrap();

        let resp = finish_account_deletion(&state, u, &u_email).await;
        assert!(matches!(resp, Ok(StatusCode::NO_CONTENT)), "got {resp:?}");

        let user_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(u).fetch_one(&pool).await.unwrap();
        assert_eq!(user_cnt, 0, "owner U must be deleted");
        let la_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE id = ANY($1)")
            .bind(vec![stripe_la, akahu_la, gc_la]).fetch_one(&pool).await.unwrap();
        assert_eq!(la_cnt, 0, "all 3 linked_accounts rows must be gone (cascaded)");
        let e_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(e).fetch_one(&pool).await.unwrap();
        assert_eq!(e_cnt, 1, "editor E must survive U's deletion");
        // wiremock `.expect(1)` on the FC + GC disconnect mocks verifies each was
        // hit exactly once when the server drops at end of scope.

        // Cleanup.
        sqlx::query("DELETE FROM budget_shares WHERE shared_with_email = $1")
            .bind(&e_email).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(e).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pool).await.ok();
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
        std::env::remove_var("GOCARDLESS_API_BASE");
        std::env::remove_var("GOCARDLESS_SECRET_ID");
        std::env::remove_var("GOCARDLESS_SECRET_KEY");
        std::env::remove_var("AKAHU_API_BASE");
    }

    /// If a bank-provider disconnect fails, `delete_account` must abort BEFORE
    /// destroying local data — leaving the account and its still-active linked
    /// row intact so the live authorization is never orphaned (nels#351).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_aborts_when_bank_disconnect_fails() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};
        let server = MockServer::start().await;
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        Mock::given(method("POST")).and(path_regex(r"/v1/financial_connections/accounts/.*/disconnect$"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        let u = Uuid::new_v4();
        let u_email = format!("del-bank-fail-{u}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(u).bind(&u_email).execute(&pool).await.unwrap();
        let bu = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'U budget','monthly',100.0)")
            .bind(bu).bind(u).execute(&pool).await.unwrap();
        // No subscription row -> cancel_subscription is a no-op, so we reach the
        // bank-disconnect step. Active Stripe FC row whose disconnect will 500.
        let stripe_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'stripe', 'fca_fail351', 'fca_fail351')")
            .bind(stripe_la).bind(bu).bind(u).execute(&pool).await.unwrap();

        let resp = finish_account_deletion(&state, u, &u_email).await;
        assert!(resp.is_err(), "bank-disconnect failure must abort deletion");

        let user_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(u).fetch_one(&pool).await.unwrap();
        assert_eq!(user_cnt, 1, "account must still exist after aborted delete");
        let row: (i64, String) = sqlx::query_as(
            "SELECT count(*), coalesce(max(status), '') FROM linked_accounts WHERE id = $1")
            .bind(stripe_la).fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1, "linked_accounts row must survive");
        assert_eq!(row.1, "active", "linked_accounts row must remain active");

        // Cleanup (cascades the budget + linked_accounts row).
        sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pool).await.ok();
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    /// A `consent_expired` row can still hold a LIVE provider authorization
    /// (e.g. a Plaid Item flipped to consent_expired on ITEM_LOGIN_REQUIRED is
    /// NOT removed at Plaid), so account deletion must REVOKE it, not skip it
    /// (nels#351). An already-`disconnected` row, by contrast, must NOT be
    /// re-revoked. This exercises both halves of the widened `<> 'disconnected'`
    /// filter: the Plaid item/remove is hit once for the consent_expired row,
    /// and the Stripe FC disconnect is never hit for the disconnected row.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_revokes_consent_expired_and_skips_disconnected() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, path_regex};
        let server = MockServer::start().await;
        std::env::set_var("PLAID_API_BASE", server.uri());
        std::env::set_var("PLAID_CLIENT_ID", "test-client-id");
        std::env::set_var("PLAID_SECRET", "test-secret");
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");

        // Plaid item/remove: hit exactly once for the consent_expired row (it
        // still holds a live Item, so it must be revoked before cascade-delete).
        Mock::given(method("POST")).and(path("/item/remove"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"removed": true})))
            .expect(1).mount(&server).await;
        // Stripe FC disconnect: must NEVER be called — the Stripe row is already
        // 'disconnected', so the `<> 'disconnected'` filter must exclude it.
        Mock::given(method("POST")).and(path_regex(r"/v1/financial_connections/accounts/.*/disconnect$"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0).mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        // ONE cipher instance reused for both encrypting the stored token and the
        // AppState, so `disconnect_all_linked_accounts` can decrypt it.
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let encrypted = cipher.encrypt("access-ce351").unwrap();
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        let u = Uuid::new_v4();
        let u_email = format!("del-ce-u-{u}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(u).bind(&u_email).execute(&pool).await.unwrap();
        let bu = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'U budget','monthly',100.0)")
            .bind(bu).bind(u).execute(&pool).await.unwrap();

        // consent_expired Plaid row (still live at Plaid -> must be revoked).
        let plaid_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, plaid_item_id, status) \
             VALUES ($1, $2, $3, 'plaid', 'plaid_ce351', $4, 'item_ce351', 'consent_expired')")
            .bind(plaid_la).bind(bu).bind(u).bind(&encrypted).execute(&pool).await.unwrap();
        // Already-disconnected Stripe FC row (must NOT be re-revoked). The
        // `linked_accounts_status_disconnected_at_check` CHECK requires
        // disconnected_at set iff status = 'disconnected'.
        let stripe_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref, status, disconnected_at) \
             VALUES ($1, $2, $3, 'stripe', 'fca_done351', 'fca_done351', 'disconnected', now())")
            .bind(stripe_la).bind(bu).bind(u).execute(&pool).await.unwrap();

        let resp = finish_account_deletion(&state, u, &u_email).await;
        assert!(matches!(resp, Ok(StatusCode::NO_CONTENT)), "got {resp:?}");

        let user_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(u).fetch_one(&pool).await.unwrap();
        assert_eq!(user_cnt, 0, "owner U must be deleted");
        let la_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE id = ANY($1)")
            .bind(vec![plaid_la, stripe_la]).fetch_one(&pool).await.unwrap();
        assert_eq!(la_cnt, 0, "both linked_accounts rows must be gone (cascaded)");
        // wiremock: item/remove `.expect(1)` and FC disconnect `.expect(0)` are
        // verified when the server drops at end of scope.

        // Cleanup.
        sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pool).await.ok();
        std::env::remove_var("PLAID_API_BASE");
        std::env::remove_var("PLAID_CLIENT_ID");
        std::env::remove_var("PLAID_SECRET");
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    /// A user deleting their account may hold a bank link they created inside
    /// ANOTHER owner's budget whose edit-share was later revoked. Passing the
    /// deleting user as the acting user to the permission-gated provider
    /// disconnect would 403 and permanently block the (GDPR) self-deletion; the
    /// fix passes the BUDGET OWNER instead, which always satisfies
    /// `require_edit_or_owner`. This test would FAIL (403 abort -> not
    /// NO_CONTENT) against the pre-fix code that passed the deleting user.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn delete_account_revokes_account_linked_into_foreign_budget() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, path_regex};
        let server = MockServer::start().await;
        std::env::set_var("GOCARDLESS_API_BASE", server.uri());
        std::env::set_var("GOCARDLESS_SECRET_ID", "sid_test");
        std::env::set_var("GOCARDLESS_SECRET_KEY", "skey_test");

        // GoCardless token (mirrors gocardless.rs's `mount_token`); no `.expect`
        // because the token may already be cached from an earlier serial test.
        Mock::given(method("POST")).and(path("/token/new/"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access": "tok_x", "access_expires": 3600})))
            .mount(&server).await;
        // GoCardless requisition delete: hit exactly once for the foreign-budget row.
        Mock::given(method("DELETE")).and(path_regex(r"/requisitions/.*"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1).mount(&server).await;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
        let state = AppState { db: pool.clone(), cipher, webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()) };

        // Owner O with their own budget BO.
        let o = Uuid::new_v4();
        let o_email = format!("del-foreign-o-{o}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(o).bind(&o_email).execute(&pool).await.unwrap();
        let bo = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'O budget','monthly',100.0)")
            .bind(bo).bind(o).execute(&pool).await.unwrap();

        // Deleting user U.
        let u = Uuid::new_v4();
        let u_email = format!("del-foreign-u-{u}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(u).bind(&u_email).execute(&pool).await.unwrap();

        // Active GoCardless row on O's budget, linked by U (user_id = U). NO
        // budget_share for U -> U has no edit permission on BO (revoked/absent),
        // so passing U as the acting user would 403.
        let gc_la = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'gocardless', 'gc_foreign351', 'req_foreign351')")
            .bind(gc_la).bind(bo).bind(u).execute(&pool).await.unwrap();

        let resp = finish_account_deletion(&state, u, &u_email).await;
        // Proves the owner-as-acting-user fix: passing O satisfies
        // require_edit_or_owner even though U has no permission on BO.
        assert!(matches!(resp, Ok(StatusCode::NO_CONTENT)), "got {resp:?}");

        let gc_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE id=$1")
            .bind(gc_la).fetch_one(&pool).await.unwrap();
        assert_eq!(gc_cnt, 0, "foreign-budget GoCardless row must be gone (cascaded via user_id)");
        let o_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
            .bind(o).fetch_one(&pool).await.unwrap();
        assert_eq!(o_cnt, 1, "budget owner O must survive U's deletion");
        // wiremock: requisition DELETE `.expect(1)` is verified at end of scope.

        // Cleanup (O + defensively U).
        sqlx::query("DELETE FROM users WHERE id = $1").bind(o).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pool).await.ok();
        std::env::remove_var("GOCARDLESS_API_BASE");
        std::env::remove_var("GOCARDLESS_SECRET_ID");
        std::env::remove_var("GOCARDLESS_SECRET_KEY");
    }

    /// `fetch_account_export` must be strictly owner-scoped: it returns only the
    /// caller's OWNED budgets (with their children) plus reference-only entries
    /// for budgets shared WITH the caller — never another owner's child data.
    ///
    /// # Why the retirement tables are seeded here specifically
    ///
    /// `GET /account/export` is an EXISTING endpoint, and #464's acceptance
    /// criterion is that a collaborator cannot read the owner's assets through
    /// any existing endpoint. A is exactly that collaborator: this fixture
    /// already shares B's budget with A, and every other module's guard would
    /// authorize A to read everything beneath that budget.
    ///
    /// `asset_holdings` and `asset_balance_history` have NO `user_id` column of
    /// their own. They are scoped entirely by `JOIN assets a ON a.id =
    /// ah.asset_id WHERE a.user_id = $1` — the JOIN *is* the access control.
    /// Drop that `WHERE` and every DSAR export would carry every user's
    /// retirement holdings, with no test anywhere to say so.
    ///
    /// B's own export is asserted too. Without that positive half, three
    /// queries that always returned nothing would pass this test perfectly.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn export_is_scoped_to_owner() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let a = Uuid::new_v4();
        let a_email = format!("export-a-{a}@example.test");
        let ba = Uuid::new_v4();

        let b = Uuid::new_v4();
        let b_email = format!("export-b-{b}@example.test");
        let bb = Uuid::new_v4();

        // Distinctive seeded strings to prove (no) leakage.
        let a_tx_desc = "A-public-tx";
        let a_chat = "A-public-chat";
        let b_tx_desc = "B-secret-tx";
        let b_chat = "B-secret-chat";
        let b_asset_name = "B-secret-401k";
        let b_asset = Uuid::new_v4();
        let b_security = Uuid::new_v4();

        // User A + owned budget BA with a category, transaction, chat message.
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(a)
            .bind(&a_email)
            .execute(&pool)
            .await
            .expect("seed user A");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'A budget', 'monthly', 100.0)",
        )
        .bind(ba)
        .bind(a)
        .execute(&pool)
        .await
        .expect("seed budget BA");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type) \
             VALUES ($1, $2, 'A cat', 'expense')",
        )
        .bind(Uuid::new_v4())
        .bind(ba)
        .execute(&pool)
        .await
        .expect("seed A category");
        sqlx::query("INSERT INTO transactions (id, budget_id, amount, description) VALUES ($1, $2, 1.0, $3)")
            .bind(Uuid::new_v4())
            .bind(ba)
            .bind(a_tx_desc)
            .execute(&pool)
            .await
            .expect("seed A transaction");
        sqlx::query(
            "INSERT INTO chat_messages (id, user_id, budget_id, sender, message_text, embedding) \
             VALUES ($1, $2, $3, 'user', $4, array_fill(0::real, ARRAY[768])::vector)",
        )
        .bind(Uuid::new_v4())
        .bind(a)
        .bind(ba)
        .bind(a_chat)
        .execute(&pool)
        .await
        .expect("seed A chat_message");

        // User B + owned budget BB with a category, transaction, chat message.
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(b)
            .bind(&b_email)
            .execute(&pool)
            .await
            .expect("seed user B");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'B budget', 'monthly', 100.0)",
        )
        .bind(bb)
        .bind(b)
        .execute(&pool)
        .await
        .expect("seed budget BB");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type) \
             VALUES ($1, $2, 'B cat', 'expense')",
        )
        .bind(Uuid::new_v4())
        .bind(bb)
        .execute(&pool)
        .await
        .expect("seed B category");
        sqlx::query("INSERT INTO transactions (id, budget_id, amount, description) VALUES ($1, $2, 1.0, $3)")
            .bind(Uuid::new_v4())
            .bind(bb)
            .bind(b_tx_desc)
            .execute(&pool)
            .await
            .expect("seed B transaction");
        sqlx::query(
            "INSERT INTO chat_messages (id, user_id, budget_id, sender, message_text, embedding) \
             VALUES ($1, $2, $3, 'user', $4, array_fill(0::real, ARRAY[768])::vector)",
        )
        .bind(Uuid::new_v4())
        .bind(b)
        .bind(bb)
        .bind(b_chat)
        .execute(&pool)
        .await
        .expect("seed B chat_message");

        // B's retirement balance sheet: an asset plus one row in each of the
        // two tables that have no `user_id` of their own.
        sqlx::query(
            "INSERT INTO assets (id, user_id, name, asset_type, tax_treatment, current_balance) \
             VALUES ($1, $2, $3, 'retirement_account', 'pre_tax', 5000.0)",
        )
        .bind(b_asset)
        .bind(b)
        .bind(b_asset_name)
        .execute(&pool)
        .await
        .expect("seed B asset");
        sqlx::query(
            "INSERT INTO securities (id, provider, provider_security_id, security_type) \
             VALUES ($1, 'plaid', $2, 'equity')",
        )
        .bind(b_security)
        .bind(format!("export-sec-{b_security}"))
        .execute(&pool)
        .await
        .expect("seed B security");
        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value) \
             VALUES ($1, $2, $3, 50.0, 5000.0)",
        )
        .bind(Uuid::new_v4())
        .bind(b_asset)
        .bind(b_security)
        .execute(&pool)
        .await
        .expect("seed B asset_holding");
        sqlx::query(
            "INSERT INTO asset_balance_history (id, asset_id, as_of, balance) \
             VALUES ($1, $2, DATE '2026-07-28', 5000.0)",
        )
        .bind(Uuid::new_v4())
        .bind(b_asset)
        .execute(&pool)
        .await
        .expect("seed B asset_balance_history");

        // Share BB with A (view).
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(bb)
        .bind(&a_email)
        .execute(&pool)
        .await
        .expect("share BB with A");

        // --- Act ---
        let export = super::fetch_account_export(&pool, a)
            .await
            .expect("export ok");

        // --- Assert ---
        assert_eq!(export.account.email, a_email, "account email is A's");

        assert_eq!(
            export.owned_budgets.len(),
            1,
            "A owns exactly one budget"
        );
        assert_eq!(
            export.owned_budgets[0].budget.id, ba,
            "the owned budget is BA, not BB"
        );

        assert_eq!(
            export.shared_with_me.len(),
            1,
            "exactly one budget shared with A"
        );
        let shared = &export.shared_with_me[0];
        assert_eq!(shared.budget_id, bb, "shared ref points at BB");
        assert_eq!(shared.owner_email, b_email, "shared ref names B as owner");
        assert_eq!(shared.permission, "view", "shared permission is view");

        let json = serde_json::to_string(&export).expect("serialize export");
        assert!(
            json.contains(a_tx_desc),
            "A's own transaction is present"
        );
        assert!(json.contains(a_chat), "A's own chat text is present");
        assert!(
            !json.contains(b_tx_desc),
            "B's transaction must NOT leak into A's export"
        );
        assert!(
            !json.contains(b_chat),
            "B's chat text must NOT leak into A's export"
        );

        // --- Assert: the #464 tables are scoped by their JOIN, not by luck. ---
        assert!(
            !json.contains(b_asset_name),
            "B's asset name must NOT leak into A's export"
        );
        assert!(
            export.assets.is_empty(),
            "A owns no assets, so `assets` must be empty: {:?}",
            export.assets
        );
        assert!(
            export.asset_holdings.is_empty(),
            "asset_holdings has no user_id — the JOIN is its only scoping: {:?}",
            export.asset_holdings
        );
        assert!(
            export.asset_balance_history.is_empty(),
            "asset_balance_history has no user_id — same JOIN, same risk: {:?}",
            export.asset_balance_history
        );

        // --- Assert: the POSITIVE half. Three queries that always returned
        // nothing would satisfy everything above; only B's own export
        // distinguishes "correctly scoped" from "permanently empty". ---
        let b_export = super::fetch_account_export(&pool, b)
            .await
            .expect("B's export ok");
        assert_eq!(
            b_export.assets.len(),
            1,
            "B's own asset must reach B's export: {:?}",
            b_export.assets
        );
        assert_eq!(b_export.assets[0].id, b_asset, "and it is the right asset");
        assert_eq!(b_export.assets[0].name, b_asset_name);
        assert_eq!(
            b_export.asset_holdings.len(),
            1,
            "B's holding must reach B's export: {:?}",
            b_export.asset_holdings
        );
        assert_eq!(b_export.asset_holdings[0].asset_id, b_asset);
        assert_eq!(
            b_export.asset_balance_history.len(),
            1,
            "B's balance snapshot must reach B's export: {:?}",
            b_export.asset_balance_history
        );
        assert_eq!(b_export.asset_balance_history[0].asset_id, b_asset);

        // --- Cleanup ---
        sqlx::query("DELETE FROM budget_shares WHERE shared_with_email = $1")
            .bind(&a_email)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(a)
            .execute(&pool)
            .await
            .ok();
        // Deleting B cascades assets -> holdings + balance history, which is
        // what frees the security from its ON DELETE RESTRICT.
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(b)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM securities WHERE id = $1")
            .bind(b_security)
            .execute(&pool)
            .await
            .ok();
    }

    /// A user-scoped (NULL budget_id) audit row must surface in the export's
    /// top-level `user_audit_logs`, attributable to the user — while a
    /// budget-scoped row stays nested under its budget and out of user_audit_logs.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn export_includes_user_scoped_audit_rows() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let a = Uuid::new_v4();
        let a_email = format!("export-uaudit-{a}@example.test");
        let ba = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(a)
            .bind(&a_email)
            .execute(&pool)
            .await
            .expect("seed user A");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'A budget', 'monthly', 100.0)",
        )
        .bind(ba)
        .bind(a)
        .execute(&pool)
        .await
        .expect("seed budget BA");

        // Budget-scoped audit row (nested under the budget, NOT user_audit_logs).
        sqlx::query(
            "INSERT INTO audit_logs (id, budget_id, user_id, action) \
             VALUES ($1, $2, $3, 'AI_UPDATE_BUDGET')",
        )
        .bind(Uuid::new_v4())
        .bind(ba)
        .bind(a)
        .execute(&pool)
        .await
        .expect("seed budget-scoped audit row");

        // User-scoped audit row (NULL budget_id) — the row this feature surfaces.
        sqlx::query(
            "INSERT INTO audit_logs (id, user_id, action) \
             VALUES ($1, $2, 'AI_REPORT_ISSUE')",
        )
        .bind(Uuid::new_v4())
        .bind(a)
        .execute(&pool)
        .await
        .expect("seed user-scoped audit row");

        // A second user B with their OWN user-scoped row — must never appear in
        // A's export (privacy: the export is strictly scoped to user_id = A).
        let b = Uuid::new_v4();
        let b_email = format!("export-uaudit-b-{b}@example.test");
        let b_action = "B_PRIVATE_REPORT_ISSUE";
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(b)
            .bind(&b_email)
            .execute(&pool)
            .await
            .expect("seed user B");
        sqlx::query("INSERT INTO audit_logs (id, user_id, action) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(b)
            .bind(b_action)
            .execute(&pool)
            .await
            .expect("seed B user-scoped audit row");

        // --- Act ---
        let export = super::fetch_account_export(&pool, a)
            .await
            .expect("export ok");

        // --- Assert ---
        assert_eq!(
            export.user_audit_logs.len(),
            1,
            "exactly the one user-scoped row appears in user_audit_logs"
        );
        let row = &export.user_audit_logs[0];
        assert_eq!(row.action, "AI_REPORT_ISSUE", "the user-scoped action");
        assert_eq!(row.budget_id, None, "user-scoped row has NULL budget_id");
        assert_eq!(row.user_id, Some(a), "row is attributable to user A");

        assert_eq!(export.owned_budgets.len(), 1, "A owns one budget");
        let budget_audit = &export.owned_budgets[0].audit_logs;
        assert_eq!(
            budget_audit.len(),
            1,
            "the budget-scoped row is nested under the budget"
        );
        assert_eq!(budget_audit[0].action, "AI_UPDATE_BUDGET");
        assert!(
            !export
                .user_audit_logs
                .iter()
                .any(|r| r.action == "AI_UPDATE_BUDGET"),
            "budget-scoped row must NOT leak into user_audit_logs"
        );

        // Cross-user isolation: none of B's rows appear in A's export.
        assert!(
            !export
                .user_audit_logs
                .iter()
                .any(|r| r.action == b_action || r.user_id == Some(b)),
            "another user's user-scoped row must NOT leak into A's export"
        );
        let json = serde_json::to_string(&export).expect("serialize export");
        assert!(
            !json.contains(b_action),
            "B's audit action must not appear anywhere in A's export"
        );

        // --- Cleanup ---
        for uid in [a, b] {
            sqlx::query("DELETE FROM audit_logs WHERE user_id = $1")
                .bind(uid)
                .execute(&pool)
                .await
                .ok();
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(uid)
                .execute(&pool)
                .await
                .ok();
        }
    }
}
