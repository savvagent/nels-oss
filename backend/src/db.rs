use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use chrono::{DateTime, Utc, NaiveDate};
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Whether this user has admin privileges (Admin PWA, #140). Defaults to
    /// FALSE in the DB. Required here because `auth::me` decodes `SELECT *` from
    /// `users` into this struct — omitting it would fail row decode at runtime.
    pub is_admin: bool,
    /// Per-viewer active-budget preference (#255): the budget this user wants
    /// to see as their "active" view, decoupled from the owner-scoped
    /// `budgets.is_default` column. NULL means no override — callers fall back
    /// to the is_default-based resolution. Required here because every site
    /// that decodes a `User` row — `SELECT *` (e.g. `auth::me`, `auth::login`) —
    /// goes through this struct's `FromRow` derive.
    pub active_budget_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Budget {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub time_frame: String,
    // Optional directly-entered amount. The authoritative budget total is now
    // computed as the SUM of category amounts (see budget::computed_budget_total),
    // so this may be NULL for amount-less budgets.
    pub budget_limit: Option<f64>,
    /// Budget amount mode (#116): 'derived' (sum of expense category amounts — the
    /// default & historical behavior) or 'fixed' (use the stored `budget_limit`).
    /// DEFAULT 'derived' keeps existing budgets behaving exactly as before.
    pub amount_mode: String,
    pub is_default: bool,
    pub rollover_enabled: bool,
    pub budget_type: String,
    pub closed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Whether this budget auto-renews on its `time_frame` cadence (#51).
    /// DEFAULT FALSE keeps existing budgets non-recurring.
    pub auto_renew: bool,
    /// Persisted idempotency marker for auto-renew: the next period boundary at
    /// which the hourly ticker renews this budget. NULL when auto_renew is off (#51).
    pub next_renewal_at: Option<DateTime<Utc>>,
    /// The PARENT budget this budget is rolled up into (#52), or None if this
    /// budget is standalone (the default). A parent aggregates its own totals/spend
    /// plus those of every non-archived child pointing at it, computed on read.
    /// NULL keeps existing budgets standalone — backward compatible.
    pub rollup_parent_id: Option<Uuid>,
    /// Per-budget budgeting strategy (#300): 'zero_based' (tracks how much of the allocated
    /// total is still available to spend) or 'limit_spent_remaining' (tracks spending against
    /// a limit, shown as Budgeted/Spent/Remaining). Replaces the old global per-user
    /// show_zero_based_summary/show_limit_spent_remaining_summary toggles. DEFAULT
    /// 'limit_spent_remaining' via migration backfill (see 20260704000000_budget_strategy.sql).
    pub budget_strategy: String,
    /// Modeled currency instead of inferring from transactions (#431).
    pub currency: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Category {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub category_type: String, // 'income', 'savings', 'expense'
    pub category_limit: Option<f64>,
    /// Whether this category carries its unused remainder forward under rollover
    /// (#49). Master-gated by the budget's own `rollover_enabled`. DEFAULT TRUE.
    pub rollover_enabled: bool,
    /// When set, this category is a live mirror of another budget's total under
    /// issue #52's linked rollup; its amount is computed-on-read from the linked
    /// (source) budget. NULL = an ordinary category. ON DELETE CASCADE removes the
    /// mirror when the source budget is deleted.
    pub linked_budget_id: Option<Uuid>,
    /// Whether this category is a "fund" (envelope / sinking-fund, #228): its
    /// unused amount accumulates CUMULATIVELY and BIDIRECTIONALLY in
    /// `fund_balance` across all periods, unlike #49's single-period,
    /// clamped-at-zero rollover. Expense categories only (data-layer CHECK).
    /// DEFAULT FALSE.
    pub is_fund: bool,
    /// The materialized, running fund balance (#228). Advanced by the hourly
    /// `advance_fund_categories` job: `+= (period_limit - period_spent)` per
    /// completed period. Can go negative after sustained overspend. Frozen
    /// (not reset) when `is_fund` is disabled, so re-enabling is
    /// non-destructive. DEFAULT 0.
    pub fund_balance: f64,
    /// The period boundary up to which `fund_balance` already reflects
    /// completed periods (#228's idempotency marker, mirroring #51's
    /// `next_renewal_at` but inverted in direction). NULL when never a fund.
    pub fund_advanced_through: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A row of the `transactions` table (#195).
///
/// The table also carries a system-managed `embedding vector(768)` column that
/// is intentionally NOT a field here: it never serializes into API responses,
/// and `sqlx`'s `FromRow` simply ignores the extra column. `updated_at` is
/// maintained by a BEFORE UPDATE trigger that fires only on user-visible edits
/// (its WHEN clause uses `IS DISTINCT FROM`), so an embedding-only write does
/// not bump it.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Transaction {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub category_id: Option<Uuid>,
    pub amount: f64,
    pub transaction_date: DateTime<Utc>,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The linked bank account this transaction was auto-imported from (#303).
    /// `None` = manually entered, exactly as every transaction was before this
    /// feature. This is ALSO the sole signal distinguishing an imported row
    /// from a manual one — there is no separate `import_source` column.
    pub external_account_id: Option<Uuid>,
    /// The bank provider's transaction id (Stripe's `fctxn_...`, or GoCardless's
    /// bank-assigned id — nels#320), present only on imported rows. Part of the
    /// idempotency key for `sync_account_transactions` (scoped per
    /// `external_account_id` since #320, not globally unique across providers).
    pub provider_transaction_id: Option<String>,
    /// When true, this transaction is excluded from all budget aggregates
    /// (income/expense/savings totals) — Task 2 of #374 already filters these
    /// out of every sum. Set via the PUT update endpoint. Defaults to false.
    pub excluded_from_budget: bool,
    /// Provenance of this row (#403). One of `ai` (logged through Nels chat /
    /// finalize), `imported` (auto-synced from a linked bank account — these are
    /// exactly the rows with `external_account_id` set), or `manual` (reserved
    /// for a future manual add-transaction form; no current UI producer). A real
    /// first-class marker replacing the old "infer from `external_account_id` /
    /// fallback description string" approach. DB DEFAULT is `ai`.
    pub source: String,
    /// ISO currency code of `amount` (#403). Populated only by sync (#303); NULL
    /// on Nels-logged rows, which fall back to the budget default / USD for
    /// display and matching. Surfaced here so the list API and (later) the
    /// duplicate matcher can read it via `FromRow`.
    pub currency: Option<String>,
    /// Review lifecycle (#403 P2). One of `needs_review` (a freshly bank-synced
    /// row awaiting the user's approval) or `reviewed` (the terminal state, and
    /// the DB DEFAULT). Only sync inserts set `needs_review`; the approve
    /// endpoint flips it to `reviewed`. Nels-logged (`ai`) / `manual` rows and
    /// every pre-existing row are `reviewed` — never retro-flagged.
    pub review_status: String,
    /// Duplicate-reconciliation link (#403 P3). Set ONLY on an `imported` row,
    /// pointing at the pre-existing Nels-logged (`ai`/`manual`) row the sync-time
    /// matcher found it to duplicate; NULL on every other row. Drives the
    /// resolvable "possible duplicate" affordance. FK is `ON DELETE SET NULL`, so
    /// deleting either twin only clears the link — reconciliation is never
    /// destructive. Resolved via the `resolve-match` endpoint (merge/dismiss).
    pub matched_transaction_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct BudgetShare {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub shared_with_email: String,
    pub permission_level: String, // 'view', 'edit'
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuditLog {
    pub id: Uuid,
    pub budget_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub action: String,
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChatMessage {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub sender: String, // 'user', 'ai'
    pub message_text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Conversation {
    pub id: Uuid,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Goal {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub goal_type: String, // 'savings' | 'debt'
    pub target_amount: f64,
    pub target_date: Option<NaiveDate>,
    pub linked_category_id: Option<Uuid>,
    pub status: String, // 'active' | 'archived'
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GoalContribution {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub user_id: Option<Uuid>,
    pub amount: f64,
    pub note: Option<String>,
    pub contributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub is_read: bool,
    pub dedup_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Reminder {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub message: String,
    pub cadence: String,
    pub next_fire_at: DateTime<Utc>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// A row of the `subscriptions` table (#25). One row per user. `stripe_subscription_id`,
/// `status`, `price_id`, `current_period_end` are NULL until the first subscription
/// webhook event lands. Entitlement is derived from `status` (see `billing::user_is_pro`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Subscription {
    pub user_id: Uuid,
    pub stripe_customer_id: String,
    pub stripe_subscription_id: Option<String>,
    pub status: Option<String>,
    pub price_id: Option<String>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    pub last_stripe_event_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A row of the `linked_accounts` table (#303, generalized for a second
/// provider by #320 and a third by #322): one linked bank account (Stripe
/// Financial Connections, GoCardless Bank Account Data, or Belvo), scoped
/// to the budget it was linked into.
/// Disconnecting NEVER deletes this row — it only flips `status` to
/// 'disconnected' and sets `disconnected_at`, preserving the account's import
/// history (the AC requires history to survive a disconnect).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LinkedAccount {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub user_id: Uuid,
    pub provider: String, // 'stripe' | 'gocardless' | 'belvo' | 'basiq' | 'akahu' | 'plaid'
    pub provider_account_id: String,
    pub provider_ref: String,
    pub institution_name: Option<String>,
    pub institution_id: Option<String>,
    pub iban: Option<String>,
    pub display_name: Option<String>,
    pub last4: Option<String>,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub status: String, // 'active' | 'disconnected' | 'consent_expired'
    pub consent_expires_at: Option<DateTime<Utc>>,
    pub country: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    // Plaid-only (nels#321) — unlike Belvo/Basiq/Akahu, which reuse the
    // generic provider_account_id/provider_ref columns, Plaid genuinely needs
    // two dedicated fields: plaid_item_id is the correlation key Plaid's
    // webhook payloads carry (a webhook fires per-Item, not per-account, and
    // one Item can back multiple linked_accounts rows sharing one
    // access_token in provider_ref) — no existing generic column represents
    // "this account's parent Item" the way institution_id/iban/etc.
    // represent bank metadata. plaid_cursor is this row's own
    // /transactions/sync cursor (per-row, not per-Item, so
    // sync_account_transactions stays shaped like every other provider's
    // per-linked-account sync function). Both always NULL for every other
    // provider's rows.
    pub plaid_item_id: Option<String>,
    pub plaid_cursor: Option<String>,
}

/// Anti-replay pending session for a Plaid Link attempt (nels#321) — see
/// plaid.rs::create_link_token/complete_link_session. A raw Plaid
/// public_token carries no recoverable client_user_id/budget binding at
/// exchange time (unlike Stripe's client_secret or GoCardless's
/// bank_link_sessions row), so this table closes that gap.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PlaidLinkSession {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub user_id: Uuid,
    pub status: String, // 'pending' | 'completed'
    pub created_at: DateTime<Utc>,
}

/// Anti-replay pending session for a retirement-asset Plaid Investments Link
/// (nels#468). The budget-less twin of [`PlaidLinkSession`] — investment
/// accounts are user-scoped with no budget (§20), so this row binds the
/// eventual public_token exchange to a user_id alone, and is selected by the
/// investments module (which reads `linked_accounts` through its own local
/// struct, since `budget_id` is NULL for investment rows).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct InvestmentLinkSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub status: String, // 'pending' | 'completed'
    pub created_at: DateTime<Utc>,
}
