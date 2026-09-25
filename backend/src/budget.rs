use axum::{
    extract::{Path, Query, State, Extension},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use chrono::{DateTime, Datelike, TimeZone, Utc};

use crate::auth::AppState;
use crate::db::{Budget, Category, Transaction, BudgetShare};
use crate::error::{internal_error, internal_error_message};

// Structs for Requests / Responses
#[derive(Deserialize, Serialize)]
pub struct BudgetPayload {
    pub name: String,
    pub description: Option<String>,
    pub time_frame: String, // 'monthly', 'quarterly', 'yearly'
    // Optional input only: a budget can be created/updated without an amount.
    // A provided value is persisted to `budgets.budget_limit`, but responses no
    // longer expose it — `BudgetListItem.budget_limit` always reports the
    // computed total (the sum of expense category amounts; see
    // `computed_budget_total`). Absent or null deserializes to None.
    #[serde(default)]
    pub budget_limit: Option<f64>,
    /// Per-budget rollover toggle (#47). Absent on create -> defaults to false;
    /// absent on update -> the existing value is preserved.
    #[serde(default)]
    pub rollover_enabled: Option<bool>,
    /// Budget type (#48): 'time_based' (default) or 'project'. Absent on create
    /// -> defaults to 'time_based'; absent on update -> the existing value is
    /// preserved.
    #[serde(default)]
    pub budget_type: Option<String>,
    /// Per-budget auto-renew toggle (#51). When true, the budget renews on its
    /// `time_frame` cadence via the hourly ticker. Absent on create -> defaults
    /// to false; absent on update -> the existing value is preserved. Only valid
    /// for time-based budgets.
    #[serde(default)]
    pub auto_renew: Option<bool>,
    /// Per-budget amount mode (#116). Valid values are exactly `"derived"` (sum of
    /// expense category amounts — the default) and `"fixed"` (use the stored
    /// `budget_limit`). Absent on create -> defaults to 'derived'; absent on update
    /// -> the existing value is preserved (COALESCE). The field is `Option<String>`;
    /// `None` means the key was absent.
    #[serde(default)]
    pub amount_mode: Option<String>,
    /// Per-budget budgeting strategy (#300): 'zero_based' or 'limit_spent_remaining'. Absent
    /// on create -> defaults to 'limit_spent_remaining'; absent on update -> the existing
    /// value is preserved (COALESCE). Chat-driven creation has a stricter rule (see rag.rs)
    /// — this silent REST default only applies to direct API callers.
    #[serde(default)]
    pub budget_strategy: Option<String>,
    /// Budget currency, ISO 4217 alpha-3. Absent on create -> defaults to 'USD';
    /// absent on update -> the existing value is preserved (COALESCE).
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct BudgetListItem {
    pub id: Uuid,
    pub owner_id: Uuid,
    /// Display name of the budget's owner (#241). `None` for a row you own
    /// yourself, or when the owner never set a display name. Populated only by
    /// `list_budgets` — every other construction site in this file (including
    /// `get_budget`/`update_budget`/`close_budget`/`archive_budget`/
    /// `unarchive_budget`, all of which are reachable on a budget shared with a
    /// non-owner) unconditionally leaves it `None`, REGARDLESS of whether the
    /// caller is the owner. On those endpoints `None` therefore does NOT mean
    /// "no display name" — it means "this endpoint doesn't populate the field
    /// at all". Do not read `owner_name` from any response other than
    /// `list_budgets`'s as meaningful; if a future feature needs it on a
    /// single-budget endpoint, populate it there the same way `list_budgets`
    /// does (a `JOIN users` on the shared-row path) rather than trusting this
    /// field's existing `None` default.
    pub owner_name: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub time_frame: String,
    // The budget total, computed as the SUM of category amounts (missing
    // category amounts count as 0). See `computed_budget_total`.
    pub budget_limit: f64,
    pub is_default: bool,
    pub is_owner: bool,
    pub permission_level: String, // "owner", "view", "edit"
    pub created_at: DateTime<Utc>,
    /// Whether per-budget rollover is enabled (#47).
    pub rollover_enabled: bool,
    /// The budget's base amount for the period: the computed total (sum of
    /// expense category amounts). Equals `budget_limit` today.
    pub base_amount: f64,
    /// Amount carried over from the previous period under rollover. 0 when
    /// rollover is disabled or the previous period had no unused remainder
    /// (overspend clamps to 0 — it does not carry).
    pub carried_amount: f64,
    /// The spendable amount for the current period: base_amount + carried_amount.
    pub effective_amount: f64,
    /// Budget type (#48): 'time_based' (calendar-period) or 'project' (one-off,
    /// runs from creation until closed). Project budgets are excluded from
    /// rollover.
    pub budget_type: String,
    /// When a project budget was closed, or None if still open / not a project
    /// budget (#48).
    pub closed_at: Option<DateTime<Utc>>,
    /// When the budget was archived (hidden from the main list while preserving
    /// all data/history), or None if active/unarchived (#50). Distinct from
    /// `closed_at`: archiving is a reversible visibility flag for any budget.
    pub archived_at: Option<DateTime<Utc>>,
    /// True when the budget has rollover on, is not a project budget, and at
    /// least one EXPENSE category has opted OUT of rollover (#49) — i.e. some but
    /// not necessarily all categories carry. Lets the UI flag "partial" rollover.
    pub has_partial_category_rollover: bool,
    /// Whether this budget auto-renews on its `time_frame` cadence (#51). Only
    /// ever true for time-based budgets.
    pub auto_renew: bool,
    /// The next period boundary at which the budget will auto-renew, or None when
    /// auto-renew is off (#51).
    pub next_renewal_at: Option<DateTime<Utc>>,
    /// Modeled currency (ISO 4217 alpha-3), no longer inferred from transactions.
    pub currency: String,
    /// The parent budget this budget is rolled up into, or None if standalone
    /// (#52). When set, this budget is a CHILD whose totals/spend are aggregated
    /// into the parent's reported figures.
    pub rollup_parent_id: Option<Uuid>,
    /// The ids of the non-archived budgets rolled up INTO this budget (its
    /// children, ordered by name), or empty if it is not a rollup parent (#52).
    pub rollup_child_ids: Vec<Uuid>,
    /// This budget's OWN base amount, which is already mirror-inclusive (#52):
    /// under the linked-category model a parent's mirror categories carry their
    /// sources' totals into its own base, so there is no separate child summation.
    /// Equals `base_amount` for a standalone budget. Missing amounts count as 0 (#46).
    pub aggregated_base_amount: f64,
    /// This budget's OWN effective (spendable) amount, which is already
    /// mirror-inclusive (#52): the parent's mirror categories are folded into its
    /// own effective amount, so there is no separate child summation. Equals
    /// `effective_amount` for a standalone budget.
    pub aggregated_effective_amount: f64,
    /// Total income target for the budget: SUM of own `category_type='income'`
    /// category limits (null->0). Not mirror-derived -- mirror categories are always
    /// expense-type (#52), so income never rolls up. Drives the zero-based strip's
    /// "Available" (#358).
    pub aggregated_income_amount: f64,
    /// This budget's OWN savings total: SUM of own `category_type='savings'` limits
    /// (null->0). Own-only because mirror categories are always expense-type (#52),
    /// so savings never rolls up. The frontend derives zero-based "Allocated" as
    /// `aggregated_base_amount + aggregated_savings_amount` (#358).
    pub aggregated_savings_amount: f64,
    /// Per-budget amount mode (#116): 'derived' (base = sum of expense category
    /// amounts, the default) or 'fixed' (base = stored `budget_limit`). Surfaced so
    /// clients can show/edit the mode.
    pub amount_mode: String,
    /// Per-budget budgeting strategy (#300): 'zero_based' or 'limit_spent_remaining'.
    pub budget_strategy: String,
    /// Whether this budget is the viewer's currently-resolved active budget
    /// (#255): their explicit per-viewer preference (`users.active_budget_id`)
    /// points at this budget, decoupled from the owner-scoped `is_default`
    /// flag. Populated ONLY by `list_budgets` — every other construction site
    /// in this file (create/get/update/close/archive/unarchive/
    /// link_rollup/unlink_rollup, via `budget_item_for`) unconditionally
    /// leaves it `false`, mirroring the existing `owner_name` field's
    /// documented precedent above. Do not read `is_active` from any response
    /// other than `list_budgets`'s as meaningful.
    pub is_active: bool,
}

impl BudgetListItem {
    /// Set the rollover amount fields, enforcing the invariant
    /// `effective_amount == base_amount + carried_amount` in one place so the
    /// four construction sites cannot drift. `budget_limit` continues to mirror
    /// `base_amount` for backward compatibility.
    fn with_amounts(mut self, rollover_enabled: bool, base: f64, carried: f64) -> Self {
        self.rollover_enabled = rollover_enabled;
        self.base_amount = base;
        self.carried_amount = carried;
        self.effective_amount = base + carried;
        self.budget_limit = base;
        // Default the aggregated view to the budget's OWN amounts. A standalone
        // budget (no children) is correct as-is; the parent read paths (list/get)
        // override these via `with_rollup` once the children are summed. This keeps
        // every construction site that only calls `with_amounts` (close/archive/
        // unarchive, which act on a single budget) reporting aggregated == own.
        self.aggregated_base_amount = base;
        self.aggregated_effective_amount = base + carried;
        self
    }

    /// Override the rollup/aggregated view fields (#52) for a parent budget. Call
    /// AFTER `with_amounts` (which seeds aggregated == own); list/get use this once
    /// the parent's own (mirror-inclusive) amounts have been read.
    fn with_rollup(
        mut self,
        child_ids: Vec<Uuid>,
        aggregated_base: f64,
        aggregated_effective: f64,
    ) -> Self {
        self.rollup_child_ids = child_ids;
        self.aggregated_base_amount = aggregated_base;
        self.aggregated_effective_amount = aggregated_effective;
        self
    }

    /// Set the zero-based aggregate view (#358). Both fields are own-only sums
    /// (income and savings), so this is now order-independent — it no longer reads
    /// `aggregated_base_amount` and may be called before or after `with_rollup`.
    /// It must still be called so the fields are populated. The frontend derives
    /// Allocated as `aggregated_base_amount + aggregated_savings_amount`.
    fn with_zero_based(mut self, income: f64, savings: f64) -> Self {
        self.aggregated_income_amount = income;
        self.aggregated_savings_amount = savings;
        self
    }
}

#[derive(Deserialize)]
pub struct CategoryPayload {
    pub name: String,
    pub category_type: String, // 'income', 'savings', 'expense'
    pub category_limit: Option<f64>,
    /// Per-category rollover opt-in (#49). Absent -> DEFAULT TRUE on create.
    #[serde(default)]
    pub rollover_enabled: Option<bool>,
}

/// Partial-update payload for a category (#49). Only the provided fields change;
/// absent fields are preserved via COALESCE in SQL.
#[derive(Deserialize)]
pub struct CategoryUpdatePayload {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub category_type: Option<String>,
    #[serde(default)]
    pub category_limit: Option<f64>,
    #[serde(default)]
    pub rollover_enabled: Option<bool>,
    /// Fund toggle (#228) — added in #426 to give the categories view a REST
    /// write path. Absent (`None`) leaves fund status unchanged, matching the
    /// COALESCE semantics of every other field on this payload.
    #[serde(default)]
    pub is_fund: Option<bool>,
}

#[derive(Serialize)]
pub struct CategoryResponse {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub category_type: String,
    pub category_limit: Option<f64>,
    /// Per-category rollover preference (#49).
    pub rollover_enabled: bool,
    pub created_at: DateTime<Utc>,
    /// Read-only computed fields (#49). `base_amount` is `category_limit` (0 if
    /// unset). `carried_amount` is the per-category carry from the previous
    /// period (0 unless an expense category with both rollover switches on) —
    /// OR, for a fund category (#228), the materialized `fund_balance`
    /// instead (is_fund takes precedence over #49's rollover to avoid
    /// double-counting the same accumulated credit under two labels).
    /// `prev_period_spent` is the source spend for the #49 carry (0 for fund
    /// categories, which don't use a single-period look-back).
    /// `effective_amount` is `base_amount + carried_amount` either way.
    pub base_amount: f64,
    pub carried_amount: f64,
    pub effective_amount: f64,
    pub prev_period_spent: f64,
    /// When set, this category is a live mirror of another budget's total under
    /// issue #52's linked rollup; `base_amount`/`effective_amount` reflect the
    /// source budget's own expense total and `carried_amount` is 0. NULL = an
    /// ordinary category. Additive field — existing consumers ignore it.
    pub linked_budget_id: Option<Uuid>,
    /// Whether this category is a fund (#228) and its materialized running
    /// balance. `fund_balance` is 0 for a category that has NEVER been a fund,
    /// but a category that WAS a fund and got disabled (`is_fund = false`)
    /// keeps its last accrued `fund_balance` (non-destructive disable, so a
    /// later re-enable resumes from it) — do not assume `is_fund == false`
    /// implies `fund_balance == 0`.
    pub is_fund: bool,
    pub fund_balance: f64,
}

#[derive(Deserialize)]
pub struct TransactionPayload {
    pub category_id: Option<Uuid>,
    pub amount: f64,
    pub description: String,
    pub transaction_date: Option<DateTime<Utc>>,
}

/// Partial-update payload for `update_transaction` (#199). Every field is
/// optional and applied via COALESCE — an omitted (`None`) field is left
/// unchanged. Note: because `category_id` is itself nullable, COALESCE cannot
/// express "set to NULL", so this endpoint cannot re-assign a transaction to
/// uncategorized; that is intentionally out of scope.
#[derive(Deserialize)]
pub struct TransactionUpdatePayload {
    pub category_id: Option<Uuid>,
    pub amount: Option<f64>,
    pub description: Option<String>,
    pub transaction_date: Option<DateTime<Utc>>,
    pub excluded_from_budget: Option<bool>,
}

#[derive(Serialize)]
pub struct TransactionResponse {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub category_type: Option<String>,
    pub amount: f64,
    pub description: String,
    pub transaction_date: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub excluded_from_budget: bool,
    /// Provenance (#403): `ai` (Logged by Nels), `imported` (bank-synced), or
    /// `manual` (reserved future). Drives the row's provenance indicator.
    pub source: String,
    /// ISO currency code of `amount` (#403), or NULL on Nels-logged rows. The
    /// client falls back to the budget default / USD when NULL.
    pub currency: Option<String>,
    /// The linked bank account this imported row came from (#403); NULL for
    /// `ai`/`manual` rows. Present so the client can group by / link to the account.
    pub external_account_id: Option<Uuid>,
    /// Human-friendly account label for imported rows (#403), e.g.
    /// `Apple Card ••7793`, following the same precedence the chat read path uses
    /// (`display_name → institution_name → "linked account"`, plus `••{last4}`).
    /// NULL for non-imported rows.
    pub account_label: Option<String>,
    /// Review lifecycle (#403 P2): `needs_review` for a freshly bank-synced row
    /// awaiting approval, else `reviewed`. Drives the "Needs review" indicator +
    /// inline approve + pending de-emphasis on the list. Only imported rows are
    /// ever `needs_review`; `ai`/`manual` and pre-existing rows are `reviewed`.
    pub review_status: String,
    /// Duplicate-reconciliation link (#403 P3): on an `imported` row, the id of
    /// the Nels-logged (`ai`/`manual`) transaction it likely duplicates; NULL
    /// otherwise. Drives the "possible duplicate" affordance — the client shows
    /// it when this is set AND `review_status == needs_review`, and resolves it
    /// via the `resolve-match` endpoint (merge/dismiss).
    pub matched_transaction_id: Option<Uuid>,
    /// The bank provider's own transaction id (#403 P5), present only on
    /// imported rows. Surfaced so the detail view can show it for cross-
    /// referencing against the bank app; NULL on ai/manual rows.
    pub provider_transaction_id: Option<String>,
}

/// Build the human-friendly linked-account label shown on an imported
/// transaction row (#403). Mirrors the precedence of the chat read path's
/// `imported_source_suffix` (`display_name → institution_name → "linked account"`,
/// with a ` ••{last4}` suffix when a mask is present) so the list and chat stay
/// consistent. Returns `None` for non-imported rows (`is_imported == false`).
/// Pure, so it is unit-testable without a DB.
pub fn build_account_label(
    is_imported: bool,
    display_name: Option<&str>,
    institution_name: Option<&str>,
    last4: Option<&str>,
) -> Option<String> {
    if !is_imported {
        return None;
    }
    fn non_blank(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    let name = non_blank(display_name)
        .or_else(|| non_blank(institution_name))
        .unwrap_or("linked account");
    match non_blank(last4) {
        Some(l) => Some(format!("{} ••{}", name, l)),
        None => Some(name.to_string()),
    }
}

/// Fetch the `build_account_label` for a single transaction's linked account
/// (#403), used by the create/finalize/update single-row responses. Returns
/// `None` when the row is not imported (`external_account_id` is `None`) or the
/// account lookup finds nothing / errors — the label is a display nicety, never
/// load-bearing.
async fn fetch_account_label(
    pool: &sqlx::PgPool,
    external_account_id: Option<Uuid>,
) -> Option<String> {
    let id = external_account_id?;
    let row = sqlx::query(
        "SELECT display_name, institution_name, last4 FROM linked_accounts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    build_account_label(
        true,
        row.get::<Option<String>, _>("display_name").as_deref(),
        row.get::<Option<String>, _>("institution_name").as_deref(),
        row.get::<Option<String>, _>("last4").as_deref(),
    )
}

#[derive(Serialize)]
pub struct CreateTransactionResponse {
    pub transaction: TransactionResponse,
    pub alerts: Vec<String>,
}

/// Resolve a pending category choice (#376) into a logged transaction. Supply
/// `category_id` (an existing category the user picked) OR `new_category_name`
/// (create-new / typed). If BOTH are present, `category_id` takes precedence and
/// `new_category_name` is ignored; supplying NEITHER is a 400.
#[derive(Deserialize)]
pub struct FinalizeTransactionPayload {
    pub amount: f64,
    pub description: String,
    pub category_id: Option<Uuid>,
    pub new_category_name: Option<String>,
}

/// Response for `finalize_transaction` (#376). `category_created` tells the
/// frontend whether a brand-new (auto_created) category was minted.
#[derive(Serialize)]
pub struct FinalizeTransactionResponse {
    pub transaction: TransactionResponse,
    pub category_created: bool,
    pub alerts: Vec<String>,
}

#[derive(Deserialize)]
pub struct SharePayload {
    pub email: String,
    pub permission_level: String, // 'view', 'edit'
}

/// The only permission levels a share may be granted. `check_permission`
/// fail-closes anything else to `View`, so an unknown value is not exploitable
/// today, but accepting it is a foot-gun for any future code path that compares
/// against another literal. Reject anything outside this set up front.
pub const ALLOWED_PERMISSION_LEVELS: [&str; 2] = ["view", "edit"];

/// Validate a share `permission_level` against the allowed set, returning a 400
/// for anything else.
pub fn validate_permission_level(level: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_PERMISSION_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "permission_level must be 'view' or 'edit'".to_string(),
        ))
    }
}

// Permission level enum for helper
#[derive(PartialEq, PartialOrd, Debug)]
pub enum Permission {
    None,
    View,
    Edit,
    Owner,
}

/// The more-privileged of two share levels ("view"/"edit"), compared by
/// privilege rank — NOT lexically (lexically "edit" < "view", which would
/// invert the result). Used to combine a direct share with an inherited one.
fn higher_share_level(a: &str, b: &str) -> String {
    let rank = |s: &str| if s == "edit" { 2 } else { 1 };
    if rank(a).max(rank(b)) == 2 { "edit".to_string() } else { "view".to_string() }
}

// Helper to check user permission on a budget (Runtime query).
//
// #396: access also flows one level through the rollup backlink — a user with
// access to a budget's rollup PARENT inherits access to the child (rollup is
// single-level, so exactly one hop). Inherited access is CAPPED at Edit (it never
// confers Owner, so owner-only child ops stay owner-gated); when a child is also
// directly shared, the HIGHER of direct and inherited wins.
pub async fn check_permission(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<Permission, String> {
    // 1. Owner + rollup parent in one query.
    let row = sqlx::query("SELECT owner_id, rollup_parent_id FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error_message)?;

    let (owner_id, rollup_parent_id): (Uuid, Option<Uuid>) = match row {
        Some(r) => (r.get("owner_id"), r.get("rollup_parent_id")),
        None => return Ok(Permission::None),
    };

    if owner_id == user_id {
        return Ok(Permission::Owner);
    }

    // Email drives both the direct and inherited share lookups.
    let user_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(internal_error_message)?;

    // 2. Direct share on this budget.
    let direct = share_level(pool, budget_id, &user_email).await?;

    // 3. Inherited via a single-level rollup parent (capped at Edit).
    let inherited = match rollup_parent_id {
        Some(parent_id) => {
            inherited_parent_permission(pool, user_id, &user_email, parent_id).await?
        }
        None => Permission::None,
    };

    // Higher of direct and inherited (Permission is PartialOrd: None<View<Edit<Owner).
    Ok(if direct >= inherited { direct } else { inherited })
}

/// A user's DIRECT share level on a budget (`Permission::None` if no share row).
/// `budget_shares.permission_level` is only ever "view"/"edit", so this is
/// inherently capped at Edit.
async fn share_level(
    pool: &PgPool,
    budget_id: Uuid,
    user_email: &str,
) -> Result<Permission, String> {
    let row = sqlx::query(
        "SELECT permission_level FROM budget_shares WHERE budget_id = $1 AND shared_with_email = $2",
    )
    .bind(budget_id)
    .bind(user_email)
    .fetch_optional(pool)
    .await
    .map_err(internal_error_message)?;

    Ok(match row {
        Some(r) => match r.get::<String, _>("permission_level").as_str() {
            "edit" => Permission::Edit,
            "view" => Permission::View,
            other => {
                tracing::warn!(
                    permission_level = %other,
                    budget_id = %budget_id,
                    "unexpected budget_shares.permission_level; treating as view"
                );
                Permission::View
            }
        },
        None => Permission::None,
    })
}

/// Access a user inherits from a rollup PARENT, capped at Edit (inheritance never
/// confers Owner). Owner-of-parent -> Edit; else the parent's direct share level;
/// else None. Single-level per `validate_rollup_link`, so this makes no recursive
/// call — the parent itself can never be a rollup child.
async fn inherited_parent_permission(
    pool: &PgPool,
    user_id: Uuid,
    user_email: &str,
    parent_id: Uuid,
) -> Result<Permission, String> {
    let owner_row = sqlx::query("SELECT owner_id FROM budgets WHERE id = $1")
        .bind(parent_id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error_message)?;

    let owner_id: Uuid = match owner_row {
        Some(r) => r.get("owner_id"),
        None => return Ok(Permission::None),
    };

    if owner_id == user_id {
        // Defensive: owner-of-parent implies owner-of-child under owner-of-both
        // linking, so the direct path already returned Owner. Cap at Edit here.
        return Ok(Permission::Edit);
    }

    share_level(pool, parent_id, user_email).await
}

// Helper to log audit activity (Runtime query). Budget-scoped variant.
pub async fn log_audit(
    pool: &PgPool,
    budget_id: Uuid,
    user_id: Uuid,
    action: &str,
    details: &str,
) {
    log_audit_row(pool, Some(budget_id), user_id, action, details).await;
}

/// User-level (non-budget-scoped) audit write (#192). Writes an audit row with a
/// NULL `budget_id` for actions that aren't tied to a budget (e.g. REPORT_ISSUE
/// — which files to the project repo, not a budget — or SET_USER_NAME). Same
/// fail-safe behavior as `log_audit`.
pub async fn log_user_audit(pool: &PgPool, user_id: Uuid, action: &str, details: &str) {
    log_audit_row(pool, None, user_id, action, details).await;
}

/// Shared audit-row writer. `budget_id` is `None` for user-scoped rows. Fail-safe:
/// a write failure is logged at `warn` (with the identifying fields, not the
/// potentially-large `details`) and never propagated — an audit gap must not
/// break the user-facing action (#168).
async fn log_audit_row(
    pool: &PgPool,
    budget_id: Option<Uuid>,
    user_id: Uuid,
    action: &str,
    details: &str,
) {
    let log_id = Uuid::new_v4();
    if let Err(e) = sqlx::query(
        "INSERT INTO audit_logs (id, budget_id, user_id, action, details) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(log_id)
    .bind(budget_id)
    .bind(user_id)
    .bind(action)
    .bind(details)
    .execute(pool)
    .await
    {
        tracing::warn!(
            error = ?e,
            log_id = %log_id,
            budget_id = ?budget_id,
            user_id = %user_id,
            action = %action,
            "audit log insert failed"
        );
    }
}

/// Sum a budget's category amounts into a single budget total. A category with
/// no amount (NULL `category_limit`) counts as 0, and a budget with no
/// categories totals 0 (issue #46).
///
/// Retained only as a unit-tested pure helper documenting the null-as-0 rule;
/// it has NO current production caller. The budget totals are computed in SQL by
/// `computed_budget_total` / `computed_budget_totals`, which inline their own
/// `COALESCE(..., 0)` (and resolve #52 mirror categories) rather than routing
/// through this function.
pub fn sum_category_amounts(amounts: &[Option<f64>]) -> f64 {
    amounts.iter().map(|a| a.unwrap_or(0.0)).sum()
}

/// Resolve a budget's base amount given its mode (#116). 'fixed' budgets report
/// their stored `budget_limit` (NULL -> 0.0); all other modes (incl. 'derived')
/// report the summed expense-category total. One definition so every read site
/// stays consistent.
pub fn resolve_base_amount(amount_mode: &str, budget_limit: Option<f64>, category_sum: f64) -> f64 {
    if amount_mode == "fixed" {
        budget_limit.unwrap_or(0.0)
    } else {
        category_sum
    }
}

/// The amount carried over into the current period from the previous period
/// under per-budget rollover (issue #47). When rollover is disabled, nothing
/// carries (0). When enabled, the unused remainder `base - prev_spent` carries,
/// but a NEGATIVE remainder (overspend) is clamped to 0 — a deficit does NOT
/// carry into the next period.
///
/// `base` is the budget's allotted total (sum of expense category limits);
/// `prev_spent` is the actual expense transactions in the previous period — a
/// deliberately different source, so a limit-less budget (base 0) never carries.
pub fn carried_amount(enabled: bool, base: f64, prev_spent: f64) -> f64 {
    if enabled {
        (base - prev_spent).max(0.0)
    } else {
        0.0
    }
}

/// The only budget types a budget may have. `time_based` is the existing
/// calendar-period behavior (the default); `project` is a one-off project
/// budget that runs from creation until it is closed. Reject anything outside
/// this set up front.
pub const ALLOWED_BUDGET_TYPES: [&str; 2] = ["time_based", "project"];

/// Validate a `budget_type` against the allowed set, returning a 400 for
/// anything else.
pub fn validate_budget_type(budget_type: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_BUDGET_TYPES.contains(&budget_type) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "budget_type must be 'time_based' or 'project'".to_string(),
        ))
    }
}

/// The amount modes a budget may use (#116): 'derived' = base is the sum of
/// expense category amounts (default); 'fixed' = base is the stored budget_limit.
pub const ALLOWED_AMOUNT_MODES: [&str; 2] = ["derived", "fixed"];

/// Validate an `amount_mode`, returning a 400 for anything else. Mirrors
/// `validate_budget_type`. Case-sensitive — the DB CHECK is case-sensitive too.
pub fn validate_amount_mode(amount_mode: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_AMOUNT_MODES.contains(&amount_mode) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "amount_mode must be 'derived' or 'fixed'".to_string(),
        ))
    }
}

/// Every allowed `budget_strategy` value (#300). Ship exactly these two now — the issue's
/// "extensible to 50/30/20, envelope, pay-yourself-first later" is a property of this being
/// a plain TEXT + CHECK column (additive to extend), not a requirement to pre-build
/// unimplemented methodologies today.
pub const ALLOWED_BUDGET_STRATEGIES: [&str; 2] = ["zero_based", "limit_spent_remaining"];

/// Validate a `budget_strategy` against the allowed set, returning a 400 for anything else.
/// Mirrors `validate_budget_type`/`validate_amount_mode`.
pub fn validate_budget_strategy(budget_strategy: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_BUDGET_STRATEGIES.contains(&budget_strategy) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "budget_strategy must be 'zero_based' or 'limit_spent_remaining'".to_string(),
        ))
    }
}

/// The category kinds the carry logic understands. Only `expense` categories
/// ever carry; the per-category rollover machinery (`category_carried`,
/// `has_partial_category_rollover`, the RAG annotation) keys off the literal
/// string `"expense"`, so an unvalidated junk type would silently be treated as
/// non-expense and never carry. Validate it at the write sites instead.
pub const ALLOWED_CATEGORY_TYPES: [&str; 3] = ["income", "savings", "expense"];

/// Validate a `category_type` against the allowed set, returning a 400 for
/// anything else. Mirrors `validate_budget_type`.
pub fn validate_category_type(category_type: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_CATEGORY_TYPES.contains(&category_type) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "category_type must be 'income', 'savings', or 'expense'".to_string(),
        ))
    }
}

/// Map a `unique_category_name_per_budget` violation to the 409 both category
/// write paths return, or `None` for any other error (leave it to the caller's
/// own fallback).
///
/// Shared by `create_category` and `update_category` so the two are
/// INDISTINGUISHABLE to the client: the editor highlights the name field on
/// this exact status, and a create that returned the generic 500 instead left
/// the user retrying a request that could never succeed (#426).
pub fn duplicate_category_name_error(e: &sqlx::Error) -> Option<(StatusCode, String)> {
    if let sqlx::Error::Database(db_err) = e {
        if db_err.is_unique_violation() {
            return Some((
                StatusCode::CONFLICT,
                "A category with that name already exists in this budget".to_string(),
            ));
        }
    }
    None
}

/// Resolve the category type to persist on the chat write path (#168). The
/// LLM may supply nothing (defaults to "expense") or an invalid type (e.g.
/// "groceries", or the wrong-case "Expense"); the `categories` column is
/// `VARCHAR(50) NOT NULL` with no CHECK constraint, so this is the only guard
/// against junk types reaching the carry logic that keys off the literal
/// "expense". Anything `validate_category_type` rejects falls back to "expense".
pub fn resolve_category_type(supplied: Option<&str>) -> String {
    let resolved = supplied.unwrap_or("expense");
    if validate_category_type(resolved).is_ok() {
        resolved.to_string()
    } else {
        "expense".to_string()
    }
}

/// The carry amount for a budget, honoring its type. Project budgets are
/// excluded from rollover/reset (#48), so they never carry (always 0).
/// Time-based budgets delegate to `carried_amount`.
pub fn effective_carried(budget_type: &str, rollover_enabled: bool, base: f64, prev_spent: f64) -> f64 {
    if budget_type == "project" {
        0.0
    } else {
        carried_amount(rollover_enabled, base, prev_spent)
    }
}

/// Per-category carry (#49). A category carries its own unused remainder only
/// when BOTH the budget master rollover switch and the category's own preference
/// are on, and the budget is not a project budget (#48 exclusion). Overspend
/// clamps to 0. Each category clamps independently, so one category's overspend
/// never consumes another's remainder.
pub fn category_carried(
    budget_rollover: bool,
    category_rollover: bool,
    budget_type: &str,
    base: f64,
    prev_spent: f64,
) -> f64 {
    if budget_type == "project" || !budget_rollover || !category_rollover {
        0.0
    } else {
        (base - prev_spent).max(0.0)
    }
}

/// Effective limit for a fund category (#228): the base limit plus the
/// materialized, CUMULATIVE, BIDIRECTIONAL running balance. Unlike
/// `category_carried`/`carried_amount` (#47/#49), there is NO floor — a
/// sustained overspend can leave a fund category with a genuine deficit that
/// reduces future periods' effective limit until repaid. Non-fund categories
/// are unaffected: this returns `category_limit` unchanged regardless of
/// whatever `fund_balance` happens to hold (a disabled fund's balance is
/// frozen but inert on the read path — see `chat_set_category_fund`).
pub fn fund_effective_limit(is_fund: bool, category_limit: f64, fund_balance: f64) -> f64 {
    if is_fund {
        category_limit + fund_balance
    } else {
        category_limit
    }
}

/// The result of comparing a requested spend amount against a category's
/// ACTUAL effective remaining balance this period (CATEGORY_AFFORDABILITY,
/// #302). `remaining` and `overage` are always computed (even when
/// `can_afford` is true, `overage` is 0.0 — never negative), so a caller can
/// render both branches of the answer from one value without re-deriving the
/// arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffordabilityCheck {
    pub can_afford: bool,
    pub remaining: f64,
    pub overage: f64,
}

/// Compare a requested spend amount against a category's effective remaining
/// balance (#302). `effective_limit` must already be the FUND-AWARE effective
/// limit (the caller resolves `category_limit: Option<f64>` — including the
/// "no limit configured" case — BEFORE calling this; see
/// `rag::format_affordability_message`, which mirrors
/// `push_category_row`'s `category_limit.map(|l| fund_effective_limit(...))`
/// pattern). `remaining` can legitimately be negative (an already-overspent
/// or fund-deficit category) — in that case any positive `requested` is
/// unaffordable, and `overage` is exactly `requested - remaining` (larger
/// than `requested` itself). Affordability is inclusive at the boundary:
/// `requested == remaining` is affordable (`<=`, not `<`).
pub fn evaluate_affordability(requested: f64, effective_limit: f64, spent: f64) -> AffordabilityCheck {
    let remaining = effective_limit - spent;
    let can_afford = requested <= remaining;
    let overage = if can_afford { 0.0 } else { requested - remaining };
    AffordabilityCheck { can_afford, remaining, overage }
}

/// Guard a budget against mutation once it has been closed (#48). A closed
/// (project) budget is read-only: every REST mutation path calls this AFTER its
/// own permission check and BEFORE the write, so a closed budget returns 409
/// CONFLICT. A missing row is treated as not-closed (Ok) — the handlers do their
/// own existence/permission checks, so this helper only enforces the closed
/// invariant. `pub` so the chat/RAG mutation path (#48 Task 4) can reuse it.
pub async fn ensure_not_closed(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let closed_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT closed_at FROM budgets WHERE id = $1")
            .bind(budget_id)
            .fetch_optional(pool)
            .await
            .map_err(internal_error)?
            // Outer Option = row presence; inner = the nullable column. A missing
            // row -> Ok (handlers check existence); a present-but-NULL closed_at
            // flattens to None -> Ok.
            .flatten();

    if closed_at.is_some() {
        return Err((StatusCode::CONFLICT, "budget is closed".to_string()));
    }
    Ok(())
}

/// Render the user-facing chat message for an `ensure_not_closed` failure,
/// distinguishing a genuine closed-budget rejection from a transient DB fault
/// (#168). `ensure_not_closed` returns CONFLICT only when the budget is actually
/// closed; any other status (e.g. INTERNAL_SERVER_ERROR from the underlying
/// SELECT failing) is a transient/internal fault the user should RETRY, not a
/// permanent rejection. The transient branch deliberately does NOT interpolate
/// the underlying message (it is the generic internal_error body) and instructs
/// the user to try again. `action_phrase` is the per-site verb ("change",
/// "log to") so the closed-budget wording stays identical to the prior inline
/// `format!` at every call site.
/// Intended specifically for `ensure_not_closed` results: it assumes any
/// non-CONFLICT status carries only the generic internal_error body. Do NOT
/// reuse it for errors whose non-CONFLICT body is itself meaningful to the user
/// (e.g. a BAD_REQUEST validation message) — that body would be silently dropped.
pub fn closed_or_transient_message(err: (StatusCode, String), action_phrase: &str) -> String {
    // Only the CONFLICT branch surfaces the underlying message (`err.1`); the
    // transient branch intentionally drops it so the generic internal_error body
    // is never sent to the user. Accessing the tuple field directly (rather than
    // destructuring into a `msg` binding the transient branch would ignore) keeps
    // that "do not leak" contract explicit.
    if err.0 == StatusCode::CONFLICT {
        format!("I can't {} this budget: {}.", action_phrase, err.1)
    } else {
        format!(
            "I couldn't {} this budget right now due to a temporary problem — please try again.",
            action_phrase
        )
    }
}

/// The full lifetime span of a project budget — from creation until it is
/// closed (or now if still open). Returns `(created_at, closed_at.unwrap_or(now))`.
pub fn project_span(
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    (created_at, closed_at.unwrap_or(now))
}

/// The half-open `[start, end)` UTC window of the period IMMEDIATELY BEFORE the
/// period containing `now`, used to source the previous period's spend for
/// per-budget rollover (issue #47). Periods are anchored to the calendar:
/// `monthly` -> the previous calendar month, `yearly` -> the previous calendar
/// year, `quarterly` -> the previous fixed quarter (Q1=Jan-Mar, Q2=Apr-Jun,
/// Q3=Jul-Sep, Q4=Oct-Dec). Any unrecognized `time_frame` falls back to
/// `monthly`.
pub fn previous_period_window(
    time_frame: &str,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let year = now.year();
    let month = now.month();

    match time_frame {
        "yearly" => {
            let end = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();
            let start = Utc.with_ymd_and_hms(year - 1, 1, 1, 0, 0, 0).unwrap();
            (start, end)
        }
        "quarterly" => {
            // First calendar month of the quarter containing `now` (1, 4, 7, 10).
            let q_start_month = ((month - 1) / 3) * 3 + 1;
            // The current quarter starts here; that's also where the previous
            // quarter ends (half-open).
            let end = Utc.with_ymd_and_hms(year, q_start_month, 1, 0, 0, 0).unwrap();
            // Previous quarter begins three months earlier; underflow from Q1
            // (Jan) rolls back to Q4 (Oct) of the prior year.
            let (start_year, start_month) = if q_start_month == 1 {
                (year - 1, 10)
            } else {
                (year, q_start_month - 3)
            };
            let start = Utc
                .with_ymd_and_hms(start_year, start_month, 1, 0, 0, 0)
                .unwrap();
            (start, end)
        }
        // "monthly" and any unrecognized value.
        _ => {
            // End of the previous month is the first of the current month.
            let end = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).unwrap();
            // Previous month start; underflow from January rolls to December.
            let (start_year, start_month) = if month == 1 {
                (year - 1, 12)
            } else {
                (year, month - 1)
            };
            let start = Utc
                .with_ymd_and_hms(start_year, start_month, 1, 0, 0, 0)
                .unwrap();
            (start, end)
        }
    }
}

/// The UTC start of the period IMMEDIATELY AFTER the period containing `now` —
/// i.e. the next period boundary, used by auto-renew (#51) to schedule a budget's
/// next renewal and to advance the marker once a period has elapsed. Anchored to
/// the calendar exactly like `previous_period_window`: `monthly` -> first of next
/// month, `quarterly` -> first month of the next fixed quarter (Q1=Jan, Q2=Apr,
/// Q3=Jul, Q4=Oct), `yearly` -> Jan 1 of next year. Any unrecognized `time_frame`
/// falls back to `monthly`.
///
/// Because the boundary is computed relative to `now` (not a stale stored
/// marker), renewing after the server was down across several boundaries jumps
/// straight to the next FUTURE boundary in a single advance — no catch-up loop.
pub fn next_period_boundary(time_frame: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let year = now.year();
    let month = now.month();

    match time_frame {
        "yearly" => Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap(),
        "quarterly" => {
            // First calendar month of the quarter containing `now` (1, 4, 7, 10).
            let q_start_month = ((month - 1) / 3) * 3 + 1;
            // The next quarter begins three months later; overflow from Q4 (Oct)
            // rolls forward to Q1 (Jan) of the next year.
            if q_start_month == 10 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, q_start_month + 3, 1, 0, 0, 0)
                    .unwrap()
            }
        }
        // "monthly" and any unrecognized value.
        _ => {
            // First of next month; overflow from December rolls to January.
            if month == 12 {
                Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap()
            } else {
                Utc.with_ymd_and_hms(year, month + 1, 1, 0, 0, 0).unwrap()
            }
        }
    }
}

/// The `next_renewal_at` marker to persist for a budget given its auto-renew
/// state: `Some(next boundary)` when auto-renew is on, `None` when off. Keeping
/// this in one place ensures the REST and chat paths compute the marker
/// identically (enable -> schedule next boundary; disable -> clear). (#51)
pub fn renewal_marker(
    auto_renew: bool,
    time_frame: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if auto_renew {
        Some(next_period_boundary(time_frame, now))
    } else {
        None
    }
}

/// The half-open `[start, end)` UTC window of the period CONTAINING `now` — i.e.
/// the CURRENT period — used to source the current period's spend (#52 rollup
/// aggregation surfaces a parent's combined current-period spend). It is exactly
/// the gap between the previous-period window's end and the next period boundary,
/// so it is anchored to the calendar identically to `previous_period_window` /
/// `next_period_boundary` (monthly/quarterly/yearly; unrecognized -> monthly).
/// Defined in terms of those two helpers so the three windows can never drift.
pub fn current_period_window(
    time_frame: &str,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    // previous_period_window's `end` is the start of the current period; the next
    // boundary is the start of the following period (the current period's end).
    let (_, current_start) = previous_period_window(time_frame, now);
    let current_end = next_period_boundary(time_frame, now);
    (current_start, current_end)
}

/// Whether a rollup link `child -> parent` is permitted, given the two budgets'
/// existing rollup roles (#52). Rollup is SINGLE-LEVEL: a budget is either a
/// standalone, a parent (has children), or a child (rolled up into a parent) —
/// never both a parent and a child. This pure guard, combined with the DB
/// self-link CHECK and the single-level rule, makes rollup cycles structurally
/// impossible:
/// - `parent_id == child_id` -> 400 (a budget cannot roll up into itself).
/// - `parent_is_child` (the prospective parent is itself rolled up into something)
///   -> 409 (linking under it would create a 2-level chain).
/// - `child_is_parent` (the prospective child already has its own children) -> 409
///   (it would become both a parent and a child).
///
/// All inputs are derived from the DB by the caller; the guard itself is pure and
/// unit-tested so the cycle/nesting rules have a single, testable definition.
pub fn validate_rollup_link(
    parent_id: Uuid,
    child_id: Uuid,
    parent_is_child: bool,
    child_is_parent: bool,
    parent_type: &str,
    child_type: &str,
    parent_strategy: &str,
    child_strategy: &str,
) -> Result<(), (StatusCode, String)> {
    if parent_id == child_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "A budget cannot be rolled up into itself".to_string(),
        ));
    }
    if parent_is_child {
        return Err((
            StatusCode::CONFLICT,
            "The target budget is itself rolled up into another budget; rollup is single-level"
                .to_string(),
        ));
    }
    if child_is_parent {
        return Err((
            StatusCode::CONFLICT,
            "That budget already has budgets rolled up into it; rollup is single-level".to_string(),
        ));
    }
    // #300: budgets can only be rolled up together when they share the same
    // budget_type AND the same budget_strategy. Checked after the self-link/
    // chain-violation guards so those keep priority (same order convention as
    // this function's other checks).
    if parent_type != child_type {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "Budgets can only be rolled up together when they share the same budget type \
                 (the parent is '{parent_type}', the child is '{child_type}'; both must be \
                 'time_based' or both must be 'project')."
            ),
        ));
    }
    if parent_strategy != child_strategy {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "Budgets can only be rolled up together when they share the same budgeting \
                 strategy (the parent is '{parent_strategy}', the child is '{child_strategy}'; \
                 both must be 'zero_based' or both must be 'limit_spent_remaining')."
            ),
        ));
    }
    Ok(())
}

/// Whether an in-place `budget_type` or `budget_strategy` change is allowed on a budget that
/// participates in a rollup relationship (#317). `validate_rollup_link` (#52/#300) already
/// rejects LINKING two budgets whose `budget_type` or `budget_strategy` differ; without this
/// guard, an ordinary edit to an ALREADY-linked budget could silently recreate that exact
/// mismatch after the link exists (neither REST `update_budget` nor chat's UPDATE_BUDGET
/// re-checked the rollup relationship before this issue).
///
/// `is_child`/`is_parent` describe the budget BEING EDITED's own rollup role, derived from the
/// DB by the caller (`is_child` = its `rollup_parent_id IS NOT NULL`; `is_parent` = via
/// `is_rollup_parent`, below). `field_label` is the human label used in the error message
/// ("budget type" / "budgeting strategy"). Only an ACTUAL change (`requested != current`) is
/// rejected — resubmitting the budget's current value is always a no-op, mirroring how
/// re-linking to the SAME parent is idempotent rather than an error (see `link_rollup`) and
/// how the frontend's `buildEditPatch` already treats an unchanged value as nothing-to-save.
///
/// A budget that is a rollup PARENT is blocked from changing its OWN `budget_type`/
/// `budget_strategy` while it has ANY child (archived or not) — not just when the new value
/// would conflict with a specific child — because pre-existing rollup links are never
/// retroactively re-validated (a parent's children are not guaranteed to already agree with
/// each other), so "does this match child X" is not well-defined for a parent in general.
/// The same blanket rule applies to a CHILD for symmetry (one rule, one function, one message
/// shape for both roles).
pub fn ensure_rollup_type_or_strategy_unchanged(
    is_child: bool,
    is_parent: bool,
    field_label: &str,
    current: &str,
    requested: &str,
) -> Result<(), (StatusCode, String)> {
    if requested == current || (!is_child && !is_parent) {
        return Ok(());
    }
    let role = if is_child { "child" } else { "parent" };
    Err((
        StatusCode::CONFLICT,
        format!(
            "Budgets can only be rolled up together when they share the same {field_label}. \
             This budget is a rollup {role}; changing its {field_label} from '{current}' to \
             '{requested}' would break that. Unlink it first if you need to change this."
        ),
    ))
}

/// Whether ANY budget (archived or not) is currently rolled up into `budget_id` — i.e.
/// whether `budget_id` is a rollup PARENT (#317). Extracted from `link_rollup`'s own
/// `child_is_parent` guard (identical SQL) so `update_budget`, chat's
/// `apply_budget_strategy_update`, and `link_rollup` itself share one definition instead of
/// three copies drifting apart.
pub async fn is_rollup_parent(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $1)")
        .bind(budget_id)
        .fetch_one(pool)
        .await
        .map_err(internal_error)
}

/// Decide the name for the mirror expense category created in a target budget when
/// a source budget is rolled up into it (#52). The category is named after the
/// source budget; if the target already has a category with that name we suffix it,
/// and if both the plain and suffixed names are taken there is no free name to use.
///
/// Comparison against `existing` is case-insensitive, matching how this file treats
/// budget/category name collisions. Pure (no DB) so both the REST and chat handlers
/// can share one definition and it stays unit-testable.
///
/// - No collision -> the source name verbatim.
/// - Plain name taken -> `"{source_name} (rolled up)"`.
/// - Both taken -> 409 CONFLICT.
pub fn rollup_category_name(
    source_name: &str,
    existing: &[String],
) -> Result<String, (StatusCode, String)> {
    let collides = |candidate: &str| {
        existing
            .iter()
            .any(|name| name.eq_ignore_ascii_case(candidate))
    };

    if !collides(source_name) {
        return Ok(source_name.to_string());
    }

    let suffixed = format!("{} (rolled up)", source_name);
    if !collides(&suffixed) {
        return Ok(suffixed);
    }

    Err((
        StatusCode::CONFLICT,
        format!(
            "The target budget already has categories named \"{}\" and \"{}\"; \
             rename one before rolling up",
            source_name, suffixed
        ),
    ))
}

/// A parent budget's AGGREGATED amounts (#52): the parent's own figures PLUS the
/// sum of every NON-ARCHIVED child rolled up into it. Each member's figures are
/// computed on read using the SAME helpers as the standalone read path
/// (`computed_budget_total` for base, `effective_carried` over its previous-period
/// window for carry, `period_expense_spent` over its CURRENT-period window — or its
/// project span for a project budget — for spend), so the aggregated view and the
/// per-budget view can never diverge. Missing amounts count as 0 (#46). Archived
/// children are EXCLUDED (they are inactive, #50). Closed/project children ARE
/// included (a closed/project pool still contributes its budgeted total + spend).
///
/// Returns `(aggregated_base, aggregated_effective, aggregated_spent)`. For a
/// standalone budget (no children) this equals the budget's own base/effective/
/// current-period spend.
pub async fn aggregated_budget_amounts(
    pool: &PgPool,
    parent: &Budget,
) -> Result<(f64, f64, f64), (StatusCode, String)> {
    let now = chrono::Utc::now();

    // Compute one budget's (base, effective, spent) on read, honoring its type.
    async fn member_amounts(
        pool: &PgPool,
        b: &Budget,
        now: DateTime<Utc>,
    ) -> Result<(f64, f64, f64), (StatusCode, String)> {
        let base = computed_budget_total(pool, b.id).await?;
        let (prev_start, prev_end) = previous_period_window(&b.time_frame, now);
        let prev_spent = period_expense_spent(pool, b.id, prev_start, prev_end).await?;
        let carried = effective_carried(&b.budget_type, b.rollover_enabled, base, prev_spent);
        let effective = base + carried;
        // Spend: a project budget tracks spend across its whole lifetime span; a
        // time-based budget uses the current calendar period.
        let (spend_start, spend_end) = if b.budget_type == "project" {
            project_span(b.created_at, b.closed_at, now)
        } else {
            current_period_window(&b.time_frame, now)
        };
        let spent = period_expense_spent(pool, b.id, spend_start, spend_end).await?;
        Ok((base, effective, spent))
    }

    // Return the parent's OWN base/effective/spent only — no children loop.
    //
    // Under issue #52's live linked-category rollup the mirror category created in
    // the parent now carries each child's amount in the parent's OWN base via
    // `computed_budget_total` (which resolves `linked_budget_id` one level deep).
    // Summing children here too would double-count those bases, so the children
    // loop is gone. The function is retained (returning the parent's own amounts)
    // so all existing callers — `list_budgets`, `get_budget`, `budget_item_for`,
    // and rag.rs's `rollup_line` — keep compiling and never double-count.
    member_amounts(pool, parent, now).await
}

/// The ids of every NON-ARCHIVED budget rolled up into `parent_id` (#52), ordered
/// by name for a deterministic response. Empty when the budget has no children.
pub async fn rollup_child_ids(
    pool: &PgPool,
    parent_id: Uuid,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM budgets WHERE rollup_parent_id = $1 AND archived_at IS NULL ORDER BY name, id",
    )
    .bind(parent_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;
    Ok(ids)
}

/// The `[start, end)` spend-window a rollup SOURCE budget contributes through,
/// keyed off the source's OWN `budget_type` (never the parent's) — a project
/// source's full lifetime span, or a time_based source's current calendar
/// period. Shared by `linked_budgets_spent` (the aggregate combined-spend
/// figure) and `category_table_rows`'s mirror-row resolution (nels#298) so the
/// two "what window does this source's mirror represent" answers cannot drift
/// apart.
fn source_spend_window(
    budget_type: &str,
    time_frame: &str,
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    if budget_type == "project" {
        project_span(created_at, closed_at, now)
    } else {
        current_period_window(time_frame, now)
    }
}

/// The total expense SPEND of every source budget mirrored into `parent_id` via a
/// linked (mirror) category (#52), summed over each source's CURRENT period — or,
/// for a `project` source, over its whole project span. The parent's mirror
/// categories carry the sources' BUDGETED totals into the parent's base (via
/// `computed_budget_total`); this companion surfaces the matching live SPEND so a
/// parent's aggregated remaining/spent can reflect what its children have spent.
///
/// Resolution is one level deep: the distinct `linked_budget_id` values of the
/// parent's categories identify the source budgets. ARCHIVED sources are skipped
/// (they are inactive, #50). Single-level rollup is enforced upstream, so no
/// source is itself a mirror-parent — no recursion. Called from the RAG ACTIVE
/// BUDGET DETAILS combined-spent line in `rag.rs` (the rollup_line), where it is
/// added to the parent's own current-period spend to report combined spend.
pub async fn linked_budgets_spent(
    pool: &PgPool,
    parent_id: Uuid,
    now: DateTime<Utc>,
) -> Result<f64, (StatusCode, String)> {
    // The distinct source budgets this parent mirrors. A parent may mirror the
    // same source through only one category, but DISTINCT guards against dupes.
    let source_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT linked_budget_id FROM categories \
         WHERE budget_id = $1 AND linked_budget_id IS NOT NULL",
    )
    .bind(parent_id)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    if source_ids.is_empty() {
        return Ok(0.0);
    }

    let sources = sqlx::query_as::<_, Budget>(
        "SELECT * FROM budgets WHERE id = ANY($1) AND archived_at IS NULL",
    )
    .bind(&source_ids)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    // Each source's spend window depends on its own type (project span vs current
    // period), so build the per-source windows then sum their spend in ONE batched
    // query rather than a round-trip per source (avoids an N+1 over the sources).
    // Each source id appears once (DISTINCT above), satisfying the one-window-per-
    // budget requirement of `period_expense_spent_many`.
    let windows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>)> = sources
        .iter()
        .map(|src| {
            let (start, end) = source_spend_window(
                &src.budget_type,
                &src.time_frame,
                src.created_at,
                src.closed_at,
                now,
            );
            (src.id, start, end)
        })
        .collect();

    let spent_by_source = period_expense_spent_many(pool, &windows).await?;
    Ok(spent_by_source.values().sum())
}

/// Compute a budget's total as the SUM of its expense category amounts.
/// Missing/NULL category amounts count as 0; a budget with no categories totals
/// 0. Only `expense` categories contribute, so the total represents the
/// spendable envelope.
///
/// A linked (mirror) expense category — `linked_budget_id IS NOT NULL`, created
/// under issue #52's live rollup — does NOT contribute its own (unset) limit;
/// instead it contributes the SOURCE budget's own expense total, resolved one
/// level deep via a correlated subquery. Single-level rollup is enforced upstream
/// (`validate_rollup_link` + the `rollup_parent_id` backlink), so a source budget
/// can never itself be a mirror-parent — the one-level resolution cannot recurse.
/// An ARCHIVED source contributes nothing: its mirror resolves to 0, symmetric
/// with `linked_budgets_spent` (which also excludes archived sources), so the
/// combined budget and combined spend always cover the same set of children.
///
/// SCOPE CAVEAT (#52): this mirror-inclusive total is NOT reflected in budget
/// limit alerts. `notifications::check_and_notify_limits` sums the raw
/// `category_limit` column and does NOT resolve mirror categories, so a rollup
/// parent's budget-limit alert ignores the children's contribution (each source
/// fires its own alerts). Do not assume this function and the notification limit
/// check agree for a rollup parent.
pub async fn computed_budget_total(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<f64, (StatusCode, String)> {
    // Resolve linked categories one level deep in SQL: a non-linked category
    // contributes its own (null-as-0) limit; a mirror category contributes the
    // source budget's own expense total. (Not routed through
    // `sum_category_amounts`; the null-as-0 rule is inlined here as COALESCE.)
    let category_sum = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(
           CASE WHEN c.linked_budget_id IS NULL
                THEN COALESCE(c.category_limit, 0)
                -- The mirror resolves the SOURCE budget's mode-aware base (#116): a
                -- 'fixed' source contributes its stored budget_limit; a 'derived'
                -- source contributes the SUM of its OWN expense category limits (the
                -- inner `lc.category_type = 'expense'` filter excludes the source's
                -- non-expense categories). The `lb.archived_at IS NULL` filter drops
                -- an ARCHIVED source entirely (no row -> outer COALESCE 0), keeping
                -- the amount symmetric with `linked_budgets_spent`: archived children
                -- count toward neither the parent's combined budget nor its spend.
                ELSE COALESCE((SELECT CASE WHEN lb.amount_mode = 'fixed'
                                           THEN COALESCE(lb.budget_limit, 0)
                                           ELSE COALESCE((SELECT SUM(COALESCE(lc.category_limit, 0))
                                                          FROM categories lc
                                                          WHERE lc.budget_id = lb.id
                                                            AND lc.category_type = 'expense'), 0)
                                      END
                               FROM budgets lb
                               WHERE lb.id = c.linked_budget_id
                                 AND lb.archived_at IS NULL), 0)
           END
         ), 0)::float8
         FROM categories c
         WHERE c.budget_id = $1 AND c.category_type = 'expense'",
    )
    .bind(budget_id)
    .fetch_one(pool)
    .await
    .map_err(internal_error)?;

    // Resolve by the budget's amount mode (#116): a 'fixed' budget reports its
    // stored budget_limit; 'derived' (and a missing row) reports the
    // linked-category-aware sum computed above.
    let row = sqlx::query("SELECT amount_mode, budget_limit FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error)?;
    let (amount_mode, budget_limit) = match row {
        Some(r) => (
            r.get::<String, _>("amount_mode"),
            r.get::<Option<f64>, _>("budget_limit"),
        ),
        // The caller always holds a budget it just read, so a missing row means the
        // budget was deleted mid-request — surface it rather than silently defaulting.
        None => {
            tracing::error!(budget_id = %budget_id, "computed_budget_total: budget row missing mid-request; defaulting to 'derived'");
            ("derived".to_string(), None)
        }
    };

    Ok(resolve_base_amount(&amount_mode, budget_limit, category_sum))
}

/// Sum a budget's OWN income and savings category limits (null->0), returned as
/// `(income, savings)`. Not mirror-resolved: mirror categories are always
/// `expense`-type (#52), so income/savings are inherently own-only. Used for the
/// zero-based strip aggregates (#358).
pub async fn computed_income_savings_totals(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<(f64, f64), (StatusCode, String)> {
    let row = sqlx::query(
        "SELECT \
           COALESCE(SUM(COALESCE(category_limit,0)) FILTER (WHERE category_type='income'),0)::float8 AS income, \
           COALESCE(SUM(COALESCE(category_limit,0)) FILTER (WHERE category_type='savings'),0)::float8 AS savings \
         FROM categories WHERE budget_id = $1",
    )
    .bind(budget_id)
    .fetch_one(pool)
    .await
    .map_err(internal_error)?;
    Ok((row.get::<f64, _>("income"), row.get::<f64, _>("savings")))
}

/// Batched twin of `computed_income_savings_totals`: budget_id -> (income, savings)
/// for many budgets in one query (absent budget = (0,0)). Avoids N+1 in list_budgets (#358).
pub async fn computed_income_savings_totals_many(
    pool: &PgPool,
    budget_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, (f64, f64)>, (StatusCode, String)> {
    if budget_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT budget_id, \
           COALESCE(SUM(COALESCE(category_limit,0)) FILTER (WHERE category_type='income'),0)::float8 AS income, \
           COALESCE(SUM(COALESCE(category_limit,0)) FILTER (WHERE category_type='savings'),0)::float8 AS savings \
         FROM categories WHERE budget_id = ANY($1) GROUP BY budget_id",
    )
    .bind(budget_ids)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("budget_id"),
                (r.get::<f64, _>("income"), r.get::<f64, _>("savings")),
            )
        })
        .collect())
}

/// Compute expense-category totals for many budgets in a single query, returning
/// a map of budget_id -> total. Budgets with no expense categories are simply
/// absent from the map (callers treat a miss as 0). Avoids the N+1 round-trips
/// that per-budget `computed_budget_total` calls would incur when listing.
pub async fn computed_budget_totals(
    pool: &PgPool,
    budget_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, f64>, (StatusCode, String)> {
    if budget_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    // Mirror `computed_budget_total`'s one-level linked-category resolution so the
    // batched and single paths can never disagree: a mirror category contributes
    // the source budget's mode-aware base (#116) — its budget_limit when 'fixed',
    // its own expense total when 'derived' — via the correlated subquery.
    let rows = sqlx::query(
        "SELECT c.budget_id, COALESCE(SUM(
           CASE WHEN c.linked_budget_id IS NULL
                THEN COALESCE(c.category_limit, 0)
                ELSE COALESCE((SELECT CASE WHEN lb.amount_mode = 'fixed'
                                           THEN COALESCE(lb.budget_limit, 0)
                                           ELSE COALESCE((SELECT SUM(COALESCE(lc.category_limit, 0))
                                                          FROM categories lc
                                                          WHERE lc.budget_id = lb.id
                                                            AND lc.category_type = 'expense'), 0)
                                      END
                               FROM budgets lb
                               WHERE lb.id = c.linked_budget_id
                                 AND lb.archived_at IS NULL), 0)
           END
         ), 0)::float8 AS total \
         FROM categories c \
         WHERE c.budget_id = ANY($1) AND c.category_type = 'expense' \
         GROUP BY c.budget_id",
    )
    .bind(budget_ids)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    // Per-budget category sums (a budget with no expense categories is absent ->
    // treated as 0 below).
    let category_sums: std::collections::HashMap<Uuid, f64> = rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("budget_id"), r.get::<f64, _>("total")))
        .collect();

    // Resolve each budget's base by its own amount mode (#116). Iterate the
    // BUDGETS query (not the category groups) so a 'fixed' budget with zero
    // categories still surfaces its budget_limit. Budgets missing from this query
    // (shouldn't happen) fall back to their category sum / 0.
    let mode_rows = sqlx::query(
        "SELECT id, amount_mode, budget_limit FROM budgets WHERE id = ANY($1)",
    )
    .bind(budget_ids)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    Ok(mode_rows
        .into_iter()
        .map(|r| {
            let id: Uuid = r.get("id");
            let amount_mode: String = r.get("amount_mode");
            let budget_limit: Option<f64> = r.get("budget_limit");
            let category_sum = category_sums.get(&id).copied().unwrap_or(0.0);
            (
                id,
                resolve_base_amount(&amount_mode, budget_limit, category_sum),
            )
        })
        .collect())
}

/// Sum a budget's expense-category spend within the half-open window
/// [start, end). Used to compute the rollover carry from the previous period
/// (#47). Expense-only, matching `computed_budget_total` and the limit checks.
///
/// Sums raw transaction amounts (expense amounts are treated as positive
/// outflows, matching the spend convention used in notifications and reports);
/// negative/refund rows are not specially handled.
pub async fn period_expense_spent(
    pool: &PgPool,
    budget_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<f64, (StatusCode, String)> {
    let spent: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(t.amount), 0)::float8 \
         FROM transactions t \
         JOIN categories c ON c.id = t.category_id \
         WHERE t.budget_id = $1 AND c.category_type = 'expense' \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date >= $2 AND t.transaction_date < $3",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await
    .map_err(internal_error)?;
    Ok(spent)
}

/// Batched form of `period_expense_spent`: for each `(budget_id, start, end)`
/// triple, sum that budget's expense-category spend within its own half-open
/// window `[start, end)` in a SINGLE query, returning a map of budget_id -> spent.
/// Each budget can have a different window (the previous-period window depends on
/// its `time_frame`), so the per-budget bounds are unnested alongside the ids and
/// the join is correlated to each budget's own bounds. Budgets with no expense
/// categories are absent from the map; a budget with expense categories but no
/// in-window transactions appears with 0. Either way callers treat a miss as 0.
/// Avoids the
/// N+1 round-trips that calling `period_expense_spent` per budget would incur
/// when listing — mirrors `computed_budget_totals`.
pub async fn period_expense_spent_many(
    pool: &PgPool,
    windows: &[(Uuid, DateTime<Utc>, DateTime<Utc>)],
) -> Result<std::collections::HashMap<Uuid, f64>, (StatusCode, String)> {
    if windows.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids: Vec<Uuid> = windows.iter().map(|w| w.0).collect();
    let starts: Vec<DateTime<Utc>> = windows.iter().map(|w| w.1).collect();
    let ends: Vec<DateTime<Utc>> = windows.iter().map(|w| w.2).collect();

    let rows = sqlx::query(
        "SELECT w.budget_id, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[]) \
              AS w(budget_id, start_ts, end_ts) \
         JOIN categories c ON c.budget_id = w.budget_id AND c.category_type = 'expense' \
         LEFT JOIN transactions t \
           ON t.category_id = c.id \
          AND NOT t.excluded_from_budget \
          AND t.budget_id = w.budget_id \
          AND t.transaction_date >= w.start_ts \
          AND t.transaction_date < w.end_ts \
         GROUP BY w.budget_id",
    )
    .bind(&ids)
    .bind(&starts)
    .bind(&ends)
    .fetch_all(pool)
    .await
    .map_err(internal_error)?;

    Ok(rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("budget_id"), r.get::<f64, _>("spent")))
        .collect())
}

/// Per-CATEGORY form of `period_expense_spent_many` (#49): for each
/// `(budget_id, start, end)` window, sum each EXPENSE category's spend within
/// that budget's half-open window `[start, end)` in a SINGLE query, returning a
/// map of category_id -> spent. Only expense categories are included (the spend
/// convention; income/savings never carry). A category with no in-window
/// transactions appears with 0 (LEFT JOIN); callers treat a miss as 0 too. Empty
/// windows -> empty map. Mirrors `period_expense_spent_many` but keyed/grouped by
/// category id. The per-category base comes from `category_limit` at the read
/// site — this helper returns only spend.
///
/// WARNING: each `budget_id` must appear at most once in `windows`. Because the
/// query groups by `c.id` alone, two windows for the same budget would collapse
/// into one group and aggregate across both windows, double-counting that
/// budget's category spend. It is currently only ever called with single-element
/// slices.
pub async fn period_category_expense_spent_many(
    pool: &PgPool,
    windows: &[(Uuid, DateTime<Utc>, DateTime<Utc>)],
) -> Result<std::collections::HashMap<Uuid, f64>, (StatusCode, String)> {
    if windows.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids: Vec<Uuid> = windows.iter().map(|w| w.0).collect();
    let starts: Vec<DateTime<Utc>> = windows.iter().map(|w| w.1).collect();
    let ends: Vec<DateTime<Utc>> = windows.iter().map(|w| w.2).collect();

    let rows = sqlx::query(
        "SELECT c.id AS category_id, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[]) \
              AS w(budget_id, start_ts, end_ts) \
         JOIN categories c ON c.budget_id = w.budget_id AND c.category_type = 'expense' \
         LEFT JOIN transactions t \
           ON t.category_id = c.id \
          AND NOT t.excluded_from_budget \
          AND t.budget_id = w.budget_id \
          AND t.transaction_date >= w.start_ts \
          AND t.transaction_date < w.end_ts \
         GROUP BY c.id",
    )
    .bind(&ids)
    .bind(&starts)
    .bind(&ends)
    .fetch_all(pool)
    .await
    // Name the operation and the budgets in the log line. A bare
    // `internal_error(e)` logs `internal server error error="…"` with no
    // operation identity and no budget, and `main.rs` builds a plain
    // `tracing_subscriber` registry with no `TraceLayer` — there is no request
    // span and no request id to supply that context from anywhere else. Three of
    // the four callers propagate (#432) and add nothing of their own, so this is
    // the only record their 500 leaves behind; the fourth, `rag.rs`, logs its own
    // line and degrades. `internal_error` still genericizes the client-facing
    // body, so none of this detail reaches the response.
    .map_err(|e| internal_error(format!(
        "previous-period category spend read failed (budgets: {:?}): {e}",
        ids
    )))?;

    Ok(rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("category_id"), r.get::<f64, _>("spent")))
        .collect())
}

/// Whether a single budget has any EXPENSE category opted OUT of rollover (#49).
/// The expense-only predicate is identical to the batched `bool_or(...)` query in
/// `list_budgets`, kept in one place so the single- and multi-budget read paths
/// cannot diverge. Callers still gate on the budget's own rollover/type.
pub async fn has_partial_category_rollover(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM categories \
         WHERE budget_id = $1 AND category_type = 'expense' AND NOT rollover_enabled)",
    )
    .bind(budget_id)
    .fetch_one(pool)
    .await
    .map_err(internal_error)
}

// --- BUDGET HANDLERS ---

pub async fn create_budget(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<BudgetPayload>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let budget_id = Uuid::new_v4();

    // Reject an unknown budget_type up front (a present value must be valid).
    if let Some(bt) = &payload.budget_type {
        validate_budget_type(bt)?;
    }

    // Reject an unknown amount_mode (#116) up front; absent defaults to 'derived'.
    if let Some(am) = &payload.amount_mode {
        validate_amount_mode(am)?;
    }
    let amount_mode = payload.amount_mode.as_deref().unwrap_or("derived");

    // Reject an unknown budget_strategy up front (#300); absent defaults to
    // 'limit_spent_remaining' (mirrors amount_mode/budget_type).
    if let Some(bs) = &payload.budget_strategy {
        validate_budget_strategy(bs)?;
    }
    let budget_strategy = payload
        .budget_strategy
        .as_deref()
        .unwrap_or("limit_spent_remaining")
        .to_string();

    // Auto-renew (#51) is only valid for time-based budgets; reject it on a
    // project create up front rather than letting the DB CHECK reject the INSERT.
    let auto_renew = payload.auto_renew.unwrap_or(false);
    let budget_type_str = payload.budget_type.as_deref().unwrap_or("time_based");
    if auto_renew && budget_type_str != "time_based" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Only time-based budgets can auto-renew".to_string(),
        ));
    }
    // When auto-renew is on, schedule the first renewal at the next period
    // boundary for the budget's timeframe; otherwise leave the marker NULL.
    let next_renewal_at = renewal_marker(auto_renew, &payload.time_frame, chrono::Utc::now());

    // Two-tier billing: a user's FIRST budget starts a Pro trial (or is blocked
    // if their subscription lapsed). No-op for users who already own a budget.
    crate::billing::ensure_ready_to_own_budget(&state.db, user_id).await?;

    // Insert the budget and switch the active (default) budget to it atomically.
    // New budgets start EMPTY — we do not auto-seed starter categories. The
    // partial unique index permits only one is_default=TRUE per user, so the
    // old default must be cleared before the new one is set; doing it in a
    // transaction prevents a partial write from leaving the user with no (or
    // a stale) active budget.
    let mut tx = state.db.begin().await.map_err(internal_error)?;

    let currency = payload.currency.clone().unwrap_or_else(|| "USD".to_string());

    let budget = sqlx::query_as::<_, Budget>(
        "INSERT INTO budgets (id, owner_id, name, description, time_frame, budget_limit, is_default, rollover_enabled, budget_type, auto_renew, next_renewal_at, amount_mode, budget_strategy, currency) VALUES ($1, $2, $3, $4, $5, $6, FALSE, $7, $8, $9, $10, $11, $12, $13) RETURNING *"
    )
    .bind(budget_id)
    .bind(user_id)
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(&payload.time_frame)
    .bind(payload.budget_limit)
    .bind(payload.rollover_enabled.unwrap_or(false))
    .bind(budget_type_str)
    .bind(auto_renew)
    .bind(next_renewal_at)
    .bind(amount_mode)
    .bind(&budget_strategy)
    .bind(&currency)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| internal_error(format!("Failed to create budget: {}", e)))?;

    sqlx::query("UPDATE budgets SET is_default = FALSE WHERE owner_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
        .bind(budget_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    // #391: also point the creator's own per-viewer preference (users.active_budget_id,
    // #255) at the new budget. Without this, a user who has ever clicked the list page's
    // Switch button (so their preference is already set to some other budget) would have
    // this new budget flip `is_default` while `resolve_active_budget_id` keeps preferring
    // the stale preference over it — the new budget silently never becomes "active" for
    // chat/insights purposes even though the REST response and is_default both say it did.
    sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
        .bind(budget_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    tx.commit().await.map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "CREATE_BUDGET", &format!("Created budget: {}", budget.name)).await;

    // A brand-new budget has no categories yet, so its computed total (base) is
    // 0; with a 0 base the carried and effective amounts are necessarily 0 too,
    // regardless of the rollover toggle. Return those directly rather than firing
    // another query that could fail after the budget was already committed.
    let rollover_enabled = budget.rollover_enabled;
    Ok(Json(BudgetListItem {
        id: budget.id,
        owner_id: budget.owner_id,
        owner_name: None,
        name: budget.name,
        description: budget.description,
        time_frame: budget.time_frame,
        budget_limit: 0.0,
        is_default: true,
        is_active: false,
        is_owner: true,
        permission_level: "owner".to_string(),
        created_at: budget.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: budget.budget_type.clone(),
        closed_at: budget.closed_at,
        archived_at: budget.archived_at,
        // A brand-new budget has no categories, so no category can have opted out.
        has_partial_category_rollover: false,
        auto_renew: budget.auto_renew,
        next_renewal_at: budget.next_renewal_at,
        rollup_parent_id: budget.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: budget.amount_mode.clone(),
        budget_strategy: budget.budget_strategy.clone(),
        currency: budget.currency.clone(),
    }
    .with_amounts(rollover_enabled, 0.0, 0.0)
    .with_zero_based(0.0, 0.0)))
}

/// Query parameters for `list_budgets` (#50). `archived` selects which budgets
/// the list returns: absent/None or `false` -> active budgets only
/// (`archived_at IS NULL`); `true` -> archived budgets only
/// (`archived_at IS NOT NULL`).
#[derive(Debug, Default, Deserialize)]
pub struct ListBudgetsQuery {
    #[serde(default)]
    pub archived: Option<bool>,
}

/// Pure archive-list filter decision (#50): keep a row iff its archived state
/// matches the requested view. `row_archived` is whether the budget's
/// `archived_at` is set; `want_archived` is the resolved query intent
/// (`query.archived.unwrap_or(false)`). Extracted so both list loops share one
/// rule and it can be unit-tested without a DB.
fn matches_archive_filter(row_archived: bool, want_archived: bool) -> bool {
    row_archived == want_archived
}

pub async fn list_budgets(
    State(state): State<AppState>,
    Query(query): Query<ListBudgetsQuery>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<BudgetListItem>>, (StatusCode, String)> {
    // Default (archived absent or false) -> active budgets only; true -> archived
    // only. Resolve once and apply the same filter to both loops below.
    let want_archived = query.archived.unwrap_or(false);

    // Fetch user email + active-budget preference (#255) in one query.
    let user_row = sqlx::query("SELECT email, active_budget_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;

    let email: String = user_row.get("email");
    let active_budget_id: Option<Uuid> = user_row.get("active_budget_id");

    // Retrieve owned and shared budgets
    let owned_rows = sqlx::query("SELECT * FROM budgets WHERE owner_id = $1")
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;

    let shared_rows = sqlx::query(
        "SELECT b.*, bs.permission_level, u.name AS owner_name FROM budgets b
         JOIN budget_shares bs ON b.id = bs.budget_id
         JOIN users u ON u.id = b.owner_id
         WHERE bs.shared_with_email = $1"
    )
    .bind(&email)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    // Archive filter (#50): keep only rows matching the requested view BEFORE the
    // batched totals/prev-spend/partial-rollover queries below, so the default
    // (active-only) path does no DB work or in-memory computation for archived
    // rows. A row is archived iff its archived_at is set.
    let row_matches_view = |r: &sqlx::postgres::PgRow| {
        matches_archive_filter(
            r.get::<Option<DateTime<Utc>>, _>("archived_at").is_some(),
            want_archived,
        )
    };
    let owned_rows: Vec<_> = owned_rows.into_iter().filter(&row_matches_view).collect();
    let mut shared_rows: Vec<_> = shared_rows.into_iter().filter(&row_matches_view).collect();

    // Rollup child visibility (#396): a collaborator with access to a SHARED parent
    // can also see the budgets rolled up into it. Children are owned by the parent's
    // owner (owner-of-both linking), so children under an OWNED parent already appear
    // in owned_rows — only children under a SHARED parent are missing. Rollup is
    // single-level, so this is exactly one hop. Source the parent ids from the
    // already archive-filtered shared_rows so a child only surfaces when its parent
    // is visible in the same view (no orphaned child).
    let shared_parent_level: std::collections::HashMap<Uuid, String> = shared_rows
        .iter()
        .map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("permission_level")))
        .collect();
    let shared_parent_ids: Vec<Uuid> = shared_parent_level.keys().copied().collect();
    let existing_ids: std::collections::HashSet<Uuid> = owned_rows
        .iter()
        .chain(shared_rows.iter())
        .map(|r| r.get::<Uuid, _>("id"))
        .collect();

    if !shared_parent_ids.is_empty() {
        // Children carry the PARENT's share level (aliased to `permission_level`,
        // the same column the shared-row loop reads) and their own owner's name —
        // so they can be processed uniformly alongside directly-shared rows.
        let inherited_rows = sqlx::query(
            "SELECT b.*, bs.permission_level, u.name AS owner_name \
             FROM budgets b \
             JOIN budget_shares bs ON bs.budget_id = b.rollup_parent_id \
             JOIN users u ON u.id = b.owner_id \
             WHERE bs.shared_with_email = $1 AND b.rollup_parent_id = ANY($2)",
        )
        .bind(&email)
        .bind(&shared_parent_ids)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;

        // Apply the same archive-view filter; drop any child already listed as
        // owned or directly shared (a directly-shared child keeps its own row and
        // gets its level raised in the push loop below).
        let inherited_rows: Vec<_> = inherited_rows
            .into_iter()
            .filter(&row_matches_view)
            .filter(|r| !existing_ids.contains(&r.get::<Uuid, _>("id")))
            .collect();

        shared_rows.extend(inherited_rows);
    }

    let mut list = Vec::new();

    // Compute every budget's total in one query (map miss => 0) to avoid an
    // N+1 round-trip per budget.
    let all_ids: Vec<Uuid> = owned_rows
        .iter()
        .chain(shared_rows.iter())
        .map(|r| r.get::<Uuid, _>("id"))
        .collect();
    let totals = computed_budget_totals(&state.db, &all_ids).await?;
    // Zero-based aggregates (#358): batch each budget's own income & savings
    // totals in one query (map miss => (0,0)) to avoid an N+1 per budget.
    let income_savings = computed_income_savings_totals_many(&state.db, &all_ids).await?;

    // Compute each budget's previous-period window (depends on its time_frame)
    // and batch the previous-period expense-spend into ONE query (map miss => 0)
    // to avoid an N+1 round-trip per budget for the rollover carry.
    let now = chrono::Utc::now();
    let windows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>)> = owned_rows
        .iter()
        .chain(shared_rows.iter())
        .map(|r| {
            let id: Uuid = r.get("id");
            let tf: String = r.get("time_frame");
            let (ws, we) = previous_period_window(&tf, now);
            (id, ws, we)
        })
        .collect();
    let prev_spends = period_expense_spent_many(&state.db, &windows).await?;

    // Batch "does this budget have any expense category opted OUT of rollover?"
    // into one query (#49). Column is NOT NULL so `NOT rollover_enabled` is safe.
    let partial_map: std::collections::HashMap<Uuid, bool> = {
        let rows = sqlx::query(
            "SELECT budget_id, bool_or(category_type = 'expense' AND NOT rollover_enabled) AS partial \
             FROM categories WHERE budget_id = ANY($1) GROUP BY budget_id",
        )
        .bind(&all_ids)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;
        rows.into_iter()
            .map(|r| (r.get::<Uuid, _>("budget_id"), r.get::<bool, _>("partial")))
            .collect()
    };

    for r in owned_rows {
        let budget_id: Uuid = r.get("id");
        let total = totals.get(&budget_id).copied().unwrap_or(0.0);
        let rollover_enabled: bool = r.get("rollover_enabled");
        let time_frame: String = r.get("time_frame");
        let budget_type_str: String = r.get("budget_type");
        let prev_spent = prev_spends.get(&budget_id).copied().unwrap_or(0.0);
        let carried = effective_carried(&budget_type_str, rollover_enabled, total, prev_spent);
        let has_partial = rollover_enabled
            && budget_type_str != "project"
            && partial_map.get(&budget_id).copied().unwrap_or(false);
        let (income, savings) = income_savings.get(&budget_id).copied().unwrap_or((0.0, 0.0));
        list.push(BudgetListItem {
            id: budget_id,
            owner_id: r.get("owner_id"),
            // The caller owns these rows, so no display name is needed
            // (mirrors BudgetContextRow's treatment of owned rows).
            owner_name: None,
            name: r.get("name"),
            description: r.get("description"),
            time_frame,
            budget_limit: total,
            is_default: r.get("is_default"),
            is_active: active_budget_id == Some(budget_id),
            is_owner: true,
            permission_level: "owner".to_string(),
            created_at: r.get("created_at"),
            rollover_enabled: false,
            base_amount: 0.0,
            carried_amount: 0.0,
            effective_amount: 0.0,
            budget_type: budget_type_str,
            closed_at: r.get("closed_at"),
            archived_at: r.get("archived_at"),
            has_partial_category_rollover: has_partial,
            auto_renew: r.get("auto_renew"),
            next_renewal_at: r.get("next_renewal_at"),
            rollup_parent_id: r.get("rollup_parent_id"),
            rollup_child_ids: Vec::new(),
            aggregated_base_amount: 0.0,
            aggregated_effective_amount: 0.0,
            aggregated_income_amount: 0.0,
            aggregated_savings_amount: 0.0,
            amount_mode: r.get("amount_mode"),
            budget_strategy: r.get("budget_strategy"),
            currency: r.get("currency"),
        }
        .with_amounts(rollover_enabled, total, carried)
        .with_zero_based(income, savings));
    }

    for r in shared_rows {
        let budget_id: Uuid = r.get("id");
        // #396: if this row is itself a child of a budget shared with the viewer,
        // its effective level is the higher of its own (direct) share and the
        // inherited parent level. For a purely inherited child the column already
        // holds the parent's level, so max() is a no-op. Ranked by privilege.
        let direct_level: String = r.get("permission_level");
        let effective_level = match r.get::<Option<Uuid>, _>("rollup_parent_id") {
            Some(pid) => match shared_parent_level.get(&pid) {
                Some(parent_level) => higher_share_level(&direct_level, parent_level),
                None => direct_level,
            },
            None => direct_level,
        };
        let total = totals.get(&budget_id).copied().unwrap_or(0.0);
        let rollover_enabled: bool = r.get("rollover_enabled");
        let time_frame: String = r.get("time_frame");
        let budget_type_str: String = r.get("budget_type");
        let prev_spent = prev_spends.get(&budget_id).copied().unwrap_or(0.0);
        let carried = effective_carried(&budget_type_str, rollover_enabled, total, prev_spent);
        let has_partial = rollover_enabled
            && budget_type_str != "project"
            && partial_map.get(&budget_id).copied().unwrap_or(false);
        let (income, savings) = income_savings.get(&budget_id).copied().unwrap_or((0.0, 0.0));
        list.push(BudgetListItem {
            id: budget_id,
            owner_id: r.get("owner_id"),
            // This is an `Option<String>` column (#241) — `Row::get` handles the
            // NULL case (no display name set) natively via the target type.
            owner_name: r.get("owner_name"),
            name: r.get("name"),
            description: r.get("description"),
            time_frame,
            budget_limit: total,
            is_default: r.get("is_default"),
            is_active: active_budget_id == Some(budget_id),
            is_owner: false,
            permission_level: effective_level,
            created_at: r.get("created_at"),
            rollover_enabled: false,
            base_amount: 0.0,
            carried_amount: 0.0,
            effective_amount: 0.0,
            budget_type: budget_type_str,
            closed_at: r.get("closed_at"),
            archived_at: r.get("archived_at"),
            has_partial_category_rollover: has_partial,
            auto_renew: r.get("auto_renew"),
            next_renewal_at: r.get("next_renewal_at"),
            rollup_parent_id: r.get("rollup_parent_id"),
            rollup_child_ids: Vec::new(),
            aggregated_base_amount: 0.0,
            aggregated_effective_amount: 0.0,
            aggregated_income_amount: 0.0,
            aggregated_savings_amount: 0.0,
            amount_mode: r.get("amount_mode"),
            budget_strategy: r.get("budget_strategy"),
            currency: r.get("currency"),
        }
        .with_amounts(rollover_enabled, total, carried)
        .with_zero_based(income, savings));
    }

    // Rollup aggregation (#52): for every PARENT budget in the list (one with at
    // least one non-archived child), override its aggregated_* fields and
    // rollup_child_ids with the combined parent+children figures. Most lists have
    // no rollups, so this does zero extra work in the common case: a single batched
    // query finds which listed budgets are parents, and only those are aggregated.
    let listed_ids: Vec<Uuid> = list.iter().map(|i| i.id).collect();
    let parent_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT rollup_parent_id FROM budgets \
         WHERE rollup_parent_id = ANY($1) AND archived_at IS NULL",
    )
    .bind(&listed_ids)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    for parent_id in parent_ids {
        // Fetch the parent row to compute its aggregated amounts on read.
        if let Some(parent) =
            sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
                .bind(parent_id)
                .fetch_optional(&state.db)
                .await
                .map_err(internal_error)?
        {
            let (own_base, own_effective, _own_spent) =
                aggregated_budget_amounts(&state.db, &parent).await?;
            let child_ids = rollup_child_ids(&state.db, parent_id).await?;
            if let Some(item) = list.iter_mut().find(|i| i.id == parent_id) {
                item.rollup_child_ids = child_ids;
                item.aggregated_base_amount = own_base;
                item.aggregated_effective_amount = own_effective;
                // No zero-based patch needed here: `aggregated_savings_amount` and
                // `aggregated_income_amount` are own-only sums, already set at push
                // time via `with_zero_based` and independent of the rollup base
                // override. The frontend derives Allocated = base + savings (#358).
            }
        }
    }

    // Sort by default first, then name
    list.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));

    Ok(Json(list))
}

pub async fn get_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied to budget".to_string()));
    }

    let budget = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "budget lookup failed");
            (StatusCode::NOT_FOUND, "Budget not found".to_string())
        })?;

    let perm_str = match perm {
        Permission::Owner => "owner",
        Permission::Edit => "edit",
        Permission::View | _ => "view",
    };

    let total = computed_budget_total(&state.db, budget_id).await?;

    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(&state.db, budget_id, win_start, win_end).await?;
    let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
    let rollover_enabled = budget.rollover_enabled;

    // Single-budget form of the list_budgets partial-rollover query (#49):
    // identical expense-only "opted out" predicate.
    let has_partial = rollover_enabled
        && budget.budget_type != "project"
        && has_partial_category_rollover(&state.db, budget_id).await?;

    // Rollup aggregated view (#52): if this budget is a parent, its aggregated_*
    // fields combine its own amounts with its non-archived children's; otherwise
    // they default to its own amounts (set by `with_amounts`). Computed on read via
    // `&budget` BEFORE the struct consumes its fields.
    let (get_agg_base, get_agg_effective, _get_agg_spent) =
        aggregated_budget_amounts(&state.db, &budget).await?;
    let get_child_ids = rollup_child_ids(&state.db, budget_id).await?;
    let (gi, gs) = computed_income_savings_totals(&state.db, budget_id).await?;

    Ok(Json(BudgetListItem {
        id: budget.id,
        owner_id: budget.owner_id,
        owner_name: None,
        name: budget.name,
        description: budget.description,
        time_frame: budget.time_frame,
        budget_limit: total,
        is_default: budget.is_default,
        is_active: false,
        is_owner: perm == Permission::Owner,
        permission_level: perm_str.to_string(),
        created_at: budget.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: budget.budget_type.clone(),
        closed_at: budget.closed_at,
        archived_at: budget.archived_at,
        has_partial_category_rollover: has_partial,
        auto_renew: budget.auto_renew,
        next_renewal_at: budget.next_renewal_at,
        rollup_parent_id: budget.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: budget.amount_mode.clone(),
        budget_strategy: budget.budget_strategy.clone(),
        currency: budget.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_rollup(get_child_ids, get_agg_base, get_agg_effective)
    .with_zero_based(gi, gs)))
}

pub async fn update_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<BudgetPayload>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "You do not have permission to modify this budget".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    // Reject an unknown budget_type up front (a present value must be valid).
    if let Some(bt) = &payload.budget_type {
        validate_budget_type(bt)?;
    }

    // Reject an unknown amount_mode (#116) up front; absent preserves the existing
    // value via COALESCE in the UPDATE below.
    if let Some(am) = &payload.amount_mode {
        validate_amount_mode(am)?;
    }

    // Reject an unknown budget_strategy up front (#300); absent preserves the
    // existing value via COALESCE in the UPDATE below.
    if let Some(bs) = &payload.budget_strategy {
        validate_budget_strategy(bs)?;
    }

    // A closed (project) budget is read-only; the close endpoint is the only
    // permitted mutation. Field edits are blocked with 409.
    ensure_not_closed(&state.db, budget_id).await?;

    // Auto-renew (#51) depends on the RESULTING budget_type and timeframe, so
    // resolve both from the payload-with-fallback to the existing row before the
    // UPDATE. Read the current state once.
    let existing = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Budget not found".to_string()))?;

    // #317: a budget in a rollup relationship (parent or child) must keep the same
    // budget_type/budget_strategy as its counterpart(s) — validate_rollup_link enforces this
    // at LINK time; this closes the gap where an ordinary edit could silently recreate the
    // exact mismatch link time rejects. Only an ACTUAL change is blocked (see
    // ensure_rollup_type_or_strategy_unchanged).
    if payload.budget_type.is_some() || payload.budget_strategy.is_some() {
        let is_child = existing.rollup_parent_id.is_some();
        let is_parent = is_rollup_parent(&state.db, budget_id).await?;
        if let Some(bt) = &payload.budget_type {
            ensure_rollup_type_or_strategy_unchanged(
                is_child, is_parent, "budget type", &existing.budget_type, bt,
            )?;
        }
        if let Some(bs) = &payload.budget_strategy {
            ensure_rollup_type_or_strategy_unchanged(
                is_child, is_parent, "budgeting strategy", &existing.budget_strategy, bs,
            )?;
        }
    }

    let resolved_type = payload
        .budget_type
        .clone()
        .unwrap_or_else(|| existing.budget_type.clone());
    // Resolve the desired auto_renew: explicit payload wins, else preserve.
    let resolved_auto_renew = payload.auto_renew.unwrap_or(existing.auto_renew);
    // Auto-renew is time-based only. Reject an explicit attempt to enable it on a
    // (resulting) project budget; if the budget is merely being converted to a
    // project, silently clear auto_renew rather than erroring.
    let auto_renew = if resolved_type != "time_based" {
        if payload.auto_renew == Some(true) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Only time-based budgets can auto-renew".to_string(),
            ));
        }
        false
    } else {
        resolved_auto_renew
    };
    // Only recompute the marker when something that actually affects it changed:
    // the auto_renew state, the budget_type (project clears it), or the time_frame
    // (a different cadence -> a different boundary). Otherwise PRESERVE the stored
    // marker. Recomputing on every unrelated edit (e.g. a rename) would, for a
    // budget whose marker is briefly past-due between the boundary and the next
    // hourly tick, advance the marker here and silently skip the ticker's audited
    // AUTO_RENEW_BUDGET renewal.
    let marker_inputs_changed = payload.auto_renew.is_some()
        || (payload.budget_type.is_some() && resolved_type != existing.budget_type)
        || payload.time_frame != existing.time_frame;
    let next_renewal_at = if marker_inputs_changed {
        renewal_marker(auto_renew, &payload.time_frame, chrono::Utc::now())
    } else {
        existing.next_renewal_at
    };

    let budget = sqlx::query_as::<_, Budget>(
        "UPDATE budgets SET name = $1, description = $2, time_frame = $3, budget_limit = $4, rollover_enabled = COALESCE($6, rollover_enabled), budget_type = COALESCE($7, budget_type), auto_renew = $8, next_renewal_at = $9, amount_mode = COALESCE($10, amount_mode), budget_strategy = COALESCE($11, budget_strategy), currency = COALESCE($12, currency) WHERE id = $5 RETURNING *"
    )
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(&payload.time_frame)
    .bind(payload.budget_limit)
    .bind(budget_id)
    .bind(payload.rollover_enabled)
    .bind(&payload.budget_type)
    .bind(auto_renew)
    .bind(next_renewal_at)
    .bind(&payload.amount_mode)
    .bind(&payload.budget_strategy)
    .bind(&payload.currency)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal_error(format!("Update failed: {}", e)))?;

    let limit_log = payload.budget_limit
        .map(|l| l.to_string())
        .unwrap_or_else(|| "none".to_string());
    log_audit(&state.db, budget_id, user_id, "UPDATE_BUDGET", &format!("Updated budget settings. Limit: {}", limit_log)).await;

    let perm_str = if perm == Permission::Owner { "owner" } else { "edit" };

    let total = computed_budget_total(&state.db, budget_id).await?;

    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(&state.db, budget_id, win_start, win_end).await?;
    let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
    let rollover_enabled = budget.rollover_enabled;

    // Recompute the partial-rollover flag against the live budget (#49) so an
    // update that flips the budget's master rollover is reflected immediately.
    let has_partial = rollover_enabled
        && budget.budget_type != "project"
        && has_partial_category_rollover(&state.db, budget_id).await?;
    let (ui, us) = computed_income_savings_totals(&state.db, budget_id).await?;

    Ok(Json(BudgetListItem {
        id: budget.id,
        owner_id: budget.owner_id,
        owner_name: None,
        name: budget.name,
        description: budget.description,
        time_frame: budget.time_frame,
        budget_limit: total,
        is_default: budget.is_default,
        is_active: false,
        is_owner: perm == Permission::Owner,
        permission_level: perm_str.to_string(),
        created_at: budget.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: budget.budget_type.clone(),
        closed_at: budget.closed_at,
        archived_at: budget.archived_at,
        has_partial_category_rollover: has_partial,
        auto_renew: budget.auto_renew,
        next_renewal_at: budget.next_renewal_at,
        rollup_parent_id: budget.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: budget.amount_mode.clone(),
        budget_strategy: budget.budget_strategy.clone(),
        currency: budget.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_zero_based(ui, us)))
}

pub async fn delete_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only the budget owner can delete the budget".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    // Remember whether we're deleting the active budget — only then do we need
    // to promote a replacement. The row is read before the DELETE.
    //
    // #493: the row can vanish between `check_permission` above and this read;
    // a vanished row is "not found", NOT a silently-false `was_default` that
    // skips the replacement-default promotion while still reporting success.
    // Both vanished-row paths warn (this read AND the DELETE guard below) and
    // share one "Budget not found" body — for delete_budget there is no
    // never-existed 404 twin (an absent row fails `check_permission` as 403),
    // so the warn marks the concurrent-deletion window the 404 alone could not.
    let was_default: bool = match sqlx::query("SELECT is_default FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
    {
        Some(r) => r.get("is_default"),
        None => {
            tracing::warn!(
                %budget_id,
                %user_id,
                "delete_budget's pre-delete read found no row after its permission \
                 check passed; the budget was deleted concurrently"
            );
            return Err((StatusCode::NOT_FOUND, "Budget not found".to_string()));
        }
    };

    let deleted = sqlx::query("DELETE FROM budgets WHERE id = $1")
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    // #493. #434's guard, extended to delete_budget. The read above and this
    // DELETE are two statements, so the budget can disappear between them;
    // `rows_affected() == 0` then means THIS request removed nothing, and 204
    // there would be indistinguishable from a real deletion. The guard runs
    // BEFORE the promotion block, so a zero-row delete never promotes a
    // replacement for a deletion it did not perform. Same surfaced-not-closed
    // contract as #434: no transaction, no `FOR UPDATE`, no `DELETE ...
    // RETURNING is_default` — the long gaps are covered by the read above.
    if deleted.rows_affected() == 0 {
        tracing::warn!(
            %budget_id,
            %user_id,
            "delete_budget matched zero rows after its existence check passed; \
             the budget was deleted concurrently"
        );
        return Err((StatusCode::NOT_FOUND, "Budget not found".to_string()));
    }

    // Promote a replacement ONLY when the deleted budget was the active one.
    // Promoting unconditionally would collide with the still-present default
    // row (partial unique index) and silently fail. Prefer the most recently
    // created remaining budget.
    if was_default {
        let remaining_budget = sqlx::query(
            "SELECT id FROM budgets WHERE owner_id = $1 ORDER BY created_at DESC LIMIT 1"
        )
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

        if let Some(r) = remaining_budget {
            let bid: Uuid = r.get("id");
            sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
                .bind(bid)
                .execute(&state.db)
                .await
                .map_err(internal_error)?;
            // #391: the FK's ON DELETE SET NULL already cleared the deleted budget's
            // owner's own active_budget_id preference IF it pointed here; repair ONLY
            // that case (active_budget_id IS NULL). Do NOT do this unconditionally: the
            // owner's preference can independently point at a budget other than the
            // deleted default (set_active_budget never touches `budgets`, so switching
            // to a shared budget or another owned budget leaves is_default on the old
            // default) — an unconditional write here would silently yank the owner out
            // of that budget and into the newly-promoted one, exactly the "your active
            // budget changed under you" failure class this branch exists to eliminate.
            sqlx::query(
                "UPDATE users SET active_budget_id = $1 WHERE id = $2 AND active_budget_id IS NULL",
            )
                .bind(bid)
                .bind(user_id)
                .execute(&state.db)
                .await
                .map_err(internal_error)?;
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

// #391: this endpoint writes ONLY the owner-scoped `is_default` flag, never
// the per-viewer `users.active_budget_id` preference — a fifth site with the
// same write-only-is_default shape as SWITCH_BUDGET/CREATE_BUDGET/create_budget/
// delete_budget's promotion, all of which had to be taught to also write the
// preference during #391. It's left as-is here ONLY because it's currently
// unreachable from the UI (see App.svelte:880 — no caller wires it up), so the
// "your active budget changed under you" bug this branch fixes can't manifest
// through it today. If this endpoint is ever re-exposed, it MUST also write
// users.active_budget_id (scoped the same way set_active_budget does), or it
// silently re-opens the exact bug #391 was filed to close.
pub async fn set_default_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the budget owner can set it as default".to_string(),
        ));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    // Set all budgets for this user to is_default = false
    sqlx::query("UPDATE budgets SET is_default = FALSE WHERE owner_id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    // Set target budget to is_default = true. Defense-in-depth: scope this UPDATE
    // by owner_id = $2 too, not just id = $1 — the check_permission guard above
    // is the primary control, but keeping the mutation itself owner-scoped means
    // a future accidental weakening of that guard (or a TOCTOU race where the
    // budget's ownership changes between the check and this write) can't flip a
    // foreign budget's default. rows_affected() == 0 means the budget no longer
    // matches (deleted or reassigned concurrently) — treat it like "not found"
    // rather than silently reporting success.
    let updated = sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1 AND owner_id = $2")
        .bind(budget_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;
    if updated.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Budget not found".to_string()));
    }

    log_audit(&state.db, budget_id, user_id, "SET_DEFAULT", "Set budget as default").await;

    Ok(StatusCode::OK)
}

/// Set the caller's per-viewer active-budget preference (#255), decoupled
/// from the owner-scoped `is_default` column set by `set_default_budget`.
/// Unlike that endpoint, this one is gated on ANY access level (View, Edit,
/// or Owner) — it never mutates the `budgets` table, only the caller's own
/// `users` row, so there is no cross-owner mutation risk: a collaborator can
/// make a shared budget their own active view without touching the owner's
/// `is_default` flag.
pub async fn set_active_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    // TOCTOU defense-in-depth, in the same SPIRIT as set_default_budget's
    // rows_affected guard (though the mechanism differs — that UPDATE is scoped
    // by owner_id and checks rows_affected() == 0; this one's target row is the
    // caller's own always-existing user row, so it instead catches the FK
    // violation): if the budget is deleted in the narrow window between the
    // permission check above and this write, the UPDATE would otherwise violate
    // active_budget_id's FK and surface as an opaque 500. Map that specific case
    // to a clean 404 instead — the FK's own ON DELETE SET NULL still protects
    // every ALREADY-set preference from ever dangling; this only covers the
    // write-time race.
    if let Err(e) = sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
        .bind(budget_id)
        .bind(user_id)
        .execute(&state.db)
        .await
    {
        if let sqlx::Error::Database(db_err) = &e {
            if db_err.is_foreign_key_violation() {
                return Err((StatusCode::NOT_FOUND, "Budget not found".to_string()));
            }
        }
        return Err(internal_error(e));
    }

    // User-scoped (#192's log_user_audit), not budget-scoped: this action changes
    // only the CALLER's own per-viewer preference, never the budget's own data or
    // the owner's row. A budget-scoped log_audit row would surface in the budget
    // OWNER's audit trail/export (#10 in AGENTS.md) even when the caller is a
    // sharee acting entirely on their own state — in tension with this whole
    // endpoint's point (never touching anything the owner can see as "theirs").
    // The activated budget's id is preserved in the free-text details instead of
    // the structured column, mirroring AI_SET_USER_NAME/AI_CREATE_REMINDER's
    // existing user-scoped precedent for actions that aren't budget-content edits.
    log_user_audit(
        &state.db,
        user_id,
        "SET_ACTIVE_BUDGET",
        &format!("Set budget {budget_id} as active view"),
    )
    .await;

    Ok(StatusCode::OK)
}

/// Close a project budget (#48). Owner-only, project-only, and idempotent:
/// closing an already-closed budget returns its current state rather than
/// erroring. Once closed, a project budget is read-only across the REST and
/// chat mutation paths (enforced by `ensure_not_closed`): new transactions,
/// category create/seed/delete, and budget field edits are rejected with 409.
/// Deleting the whole budget (`delete_budget`) is still allowed. Time-based
/// budgets cannot be closed (400).
pub async fn close_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only the budget owner can close the budget".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let budget = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "budget lookup failed");
            (StatusCode::NOT_FOUND, "Budget not found".to_string())
        })?;

    if budget.budget_type != "project" {
        return Err((StatusCode::BAD_REQUEST, "Only project budgets can be closed".to_string()));
    }

    // The base/carry computation is shared by the idempotent (already-closed) and
    // the just-closed paths. Carry is 0 for project budgets (`effective_carried`
    // ignores prev_spent for them), but mirror get_budget's sourcing for
    // consistency.
    let total = computed_budget_total(&state.db, budget_id).await?;
    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(&state.db, budget_id, win_start, win_end).await?;

    // Zero-based income/savings aggregates (#358), fetched once for both branches.
    let (ci, cs) = computed_income_savings_totals(&state.db, budget_id).await?;

    // Already closed -> idempotent: return the current state, do not error or
    // overwrite the original closed_at.
    if budget.closed_at.is_some() {
        let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
        let rollover_enabled = budget.rollover_enabled;
        return Ok(Json(BudgetListItem {
            id: budget.id,
            owner_id: budget.owner_id,
            owner_name: None,
            name: budget.name,
            description: budget.description,
            time_frame: budget.time_frame,
            budget_limit: total,
            is_default: budget.is_default,
            is_active: false,
            is_owner: true,
            permission_level: "owner".to_string(),
            created_at: budget.created_at,
            rollover_enabled: false,
            base_amount: 0.0,
            carried_amount: 0.0,
            effective_amount: 0.0,
            budget_type: budget.budget_type.clone(),
            closed_at: budget.closed_at,
            archived_at: budget.archived_at,
            // Project budgets are excluded from rollover (#48/#49) -> always false.
            has_partial_category_rollover: false,
            auto_renew: budget.auto_renew,
            next_renewal_at: budget.next_renewal_at,
            rollup_parent_id: budget.rollup_parent_id,
            rollup_child_ids: Vec::new(),
            aggregated_base_amount: 0.0,
            aggregated_effective_amount: 0.0,
            aggregated_income_amount: 0.0,
            aggregated_savings_amount: 0.0,
            amount_mode: budget.amount_mode.clone(),
            budget_strategy: budget.budget_strategy.clone(),
            currency: budget.currency.clone(),
        }
        .with_amounts(rollover_enabled, total, carried)
        .with_zero_based(ci, cs)));
    }

    let closed = sqlx::query_as::<_, Budget>(
        // COALESCE keeps the first close timestamp if two close requests race
        // (both saw closed_at IS NULL above): the later UPDATE preserves the
        // original rather than overwriting it, keeping close idempotent.
        "UPDATE budgets SET closed_at = COALESCE(closed_at, now()) WHERE id = $1 RETURNING *",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "CLOSE_BUDGET", &format!("Closed project budget: {}", closed.name)).await;

    let carried = effective_carried(&closed.budget_type, closed.rollover_enabled, total, prev_spent);
    let rollover_enabled = closed.rollover_enabled;

    Ok(Json(BudgetListItem {
        id: closed.id,
        owner_id: closed.owner_id,
        owner_name: None,
        name: closed.name,
        description: closed.description,
        time_frame: closed.time_frame,
        budget_limit: total,
        is_default: closed.is_default,
        is_active: false,
        is_owner: true,
        permission_level: "owner".to_string(),
        created_at: closed.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: closed.budget_type.clone(),
        closed_at: closed.closed_at,
        archived_at: closed.archived_at,
        // Project budgets are excluded from rollover (#48/#49) -> always false.
        has_partial_category_rollover: false,
        auto_renew: closed.auto_renew,
        next_renewal_at: closed.next_renewal_at,
        rollup_parent_id: closed.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: closed.amount_mode.clone(),
        budget_strategy: closed.budget_strategy.clone(),
        currency: closed.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_zero_based(ci, cs)))
}

/// Archive a budget (#50): hide it from the default budget list while preserving
/// all data/history. Owner-only and reversible (see `unarchive_budget`). Unlike
/// `close_budget`, ANY budget type can be archived. Idempotent: archiving an
/// already-archived budget returns its current state without overwriting the
/// original `archived_at`.
pub async fn archive_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only the budget owner can archive the budget".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let budget = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "budget lookup failed");
            (StatusCode::NOT_FOUND, "Budget not found".to_string())
        })?;

    // Base/carry sourcing mirrors close_budget so the idempotent and just-archived
    // paths report the same amounts as the list/get endpoints.
    let total = computed_budget_total(&state.db, budget_id).await?;
    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(&state.db, budget_id, win_start, win_end).await?;

    // Zero-based income/savings aggregates (#358), fetched once for both branches.
    let (ai, as_) = computed_income_savings_totals(&state.db, budget_id).await?;

    // Already archived -> idempotent: return current state, do not overwrite the
    // original archived_at.
    if budget.archived_at.is_some() {
        let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
        let rollover_enabled = budget.rollover_enabled;
        // Mirror get_budget: archive applies to any budget type, so report the
        // same partial-rollover flag get_budget/list_budgets would for this budget.
        let has_partial = rollover_enabled
            && budget.budget_type != "project"
            && has_partial_category_rollover(&state.db, budget_id).await?;
        return Ok(Json(BudgetListItem {
            id: budget.id,
            owner_id: budget.owner_id,
            owner_name: None,
            name: budget.name,
            description: budget.description,
            time_frame: budget.time_frame.clone(),
            budget_limit: total,
            is_default: budget.is_default,
            is_active: false,
            is_owner: true,
            permission_level: "owner".to_string(),
            created_at: budget.created_at,
            rollover_enabled: false,
            base_amount: 0.0,
            carried_amount: 0.0,
            effective_amount: 0.0,
            budget_type: budget.budget_type.clone(),
            closed_at: budget.closed_at,
            archived_at: budget.archived_at,
            has_partial_category_rollover: has_partial,
            auto_renew: budget.auto_renew,
            next_renewal_at: budget.next_renewal_at,
            rollup_parent_id: budget.rollup_parent_id,
            rollup_child_ids: Vec::new(),
            aggregated_base_amount: 0.0,
            aggregated_effective_amount: 0.0,
            aggregated_income_amount: 0.0,
            aggregated_savings_amount: 0.0,
            amount_mode: budget.amount_mode.clone(),
            budget_strategy: budget.budget_strategy.clone(),
            currency: budget.currency.clone(),
        }
        .with_amounts(rollover_enabled, total, carried)
        .with_zero_based(ai, as_)));
    }

    let archived = sqlx::query_as::<_, Budget>(
        // COALESCE keeps the first archive timestamp if two archive requests race
        // (both saw archived_at IS NULL above), keeping archive idempotent.
        "UPDATE budgets SET archived_at = COALESCE(archived_at, now()) WHERE id = $1 RETURNING *",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "ARCHIVE_BUDGET", &format!("Archived budget: {}", archived.name)).await;

    let carried = effective_carried(&archived.budget_type, archived.rollover_enabled, total, prev_spent);
    let rollover_enabled = archived.rollover_enabled;
    // Mirror get_budget: archive applies to any budget type, so report the
    // same partial-rollover flag get_budget/list_budgets would for this budget.
    let has_partial = rollover_enabled
        && archived.budget_type != "project"
        && has_partial_category_rollover(&state.db, budget_id).await?;

    Ok(Json(BudgetListItem {
        id: archived.id,
        owner_id: archived.owner_id,
        owner_name: None,
        name: archived.name,
        description: archived.description,
        time_frame: archived.time_frame.clone(),
        budget_limit: total,
        is_default: archived.is_default,
        is_active: false,
        is_owner: true,
        permission_level: "owner".to_string(),
        created_at: archived.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: archived.budget_type.clone(),
        closed_at: archived.closed_at,
        archived_at: archived.archived_at,
        has_partial_category_rollover: has_partial,
        auto_renew: archived.auto_renew,
        next_renewal_at: archived.next_renewal_at,
        rollup_parent_id: archived.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: archived.amount_mode.clone(),
        budget_strategy: archived.budget_strategy.clone(),
        currency: archived.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_zero_based(ai, as_)))
}

/// Unarchive a budget (#50): restore it to the default budget list. Owner-only,
/// the reverse of `archive_budget`. Idempotent: unarchiving an already-active
/// budget returns its current state unchanged.
pub async fn unarchive_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only the budget owner can unarchive the budget".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let budget = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "budget lookup failed");
            (StatusCode::NOT_FOUND, "Budget not found".to_string())
        })?;

    let total = computed_budget_total(&state.db, budget_id).await?;
    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(&state.db, budget_id, win_start, win_end).await?;

    // Zero-based income/savings aggregates (#358), fetched once for both branches.
    let (uai, uas) = computed_income_savings_totals(&state.db, budget_id).await?;

    // Already unarchived -> idempotent: return current state unchanged.
    if budget.archived_at.is_none() {
        let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
        let rollover_enabled = budget.rollover_enabled;
        // Mirror get_budget: archive applies to any budget type, so report the
        // same partial-rollover flag get_budget/list_budgets would for this budget.
        let has_partial = rollover_enabled
            && budget.budget_type != "project"
            && has_partial_category_rollover(&state.db, budget_id).await?;
        return Ok(Json(BudgetListItem {
            id: budget.id,
            owner_id: budget.owner_id,
            owner_name: None,
            name: budget.name,
            description: budget.description,
            time_frame: budget.time_frame.clone(),
            budget_limit: total,
            is_default: budget.is_default,
            is_active: false,
            is_owner: true,
            permission_level: "owner".to_string(),
            created_at: budget.created_at,
            rollover_enabled: false,
            base_amount: 0.0,
            carried_amount: 0.0,
            effective_amount: 0.0,
            budget_type: budget.budget_type.clone(),
            closed_at: budget.closed_at,
            archived_at: budget.archived_at,
            has_partial_category_rollover: has_partial,
            auto_renew: budget.auto_renew,
            next_renewal_at: budget.next_renewal_at,
            rollup_parent_id: budget.rollup_parent_id,
            rollup_child_ids: Vec::new(),
            aggregated_base_amount: 0.0,
            aggregated_effective_amount: 0.0,
            aggregated_income_amount: 0.0,
            aggregated_savings_amount: 0.0,
            amount_mode: budget.amount_mode.clone(),
            budget_strategy: budget.budget_strategy.clone(),
            currency: budget.currency.clone(),
        }
        .with_amounts(rollover_enabled, total, carried)
        .with_zero_based(uai, uas)));
    }

    let unarchived = sqlx::query_as::<_, Budget>(
        "UPDATE budgets SET archived_at = NULL WHERE id = $1 RETURNING *",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "UNARCHIVE_BUDGET", &format!("Unarchived budget: {}", unarchived.name)).await;

    let carried = effective_carried(&unarchived.budget_type, unarchived.rollover_enabled, total, prev_spent);
    let rollover_enabled = unarchived.rollover_enabled;
    // Mirror get_budget: archive applies to any budget type, so report the
    // same partial-rollover flag get_budget/list_budgets would for this budget.
    let has_partial = rollover_enabled
        && unarchived.budget_type != "project"
        && has_partial_category_rollover(&state.db, budget_id).await?;

    Ok(Json(BudgetListItem {
        id: unarchived.id,
        owner_id: unarchived.owner_id,
        owner_name: None,
        name: unarchived.name,
        description: unarchived.description,
        time_frame: unarchived.time_frame.clone(),
        budget_limit: total,
        is_default: unarchived.is_default,
        is_active: false,
        is_owner: true,
        permission_level: "owner".to_string(),
        created_at: unarchived.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: unarchived.budget_type.clone(),
        closed_at: unarchived.closed_at,
        archived_at: unarchived.archived_at,
        has_partial_category_rollover: has_partial,
        auto_renew: unarchived.auto_renew,
        next_renewal_at: unarchived.next_renewal_at,
        rollup_parent_id: unarchived.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: unarchived.amount_mode.clone(),
        budget_strategy: unarchived.budget_strategy.clone(),
        currency: unarchived.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_zero_based(uai, uas)))
}

/// The body of a link-rollup request (#52): the child budget to roll up into the
/// parent named in the path.
#[derive(Deserialize)]
pub struct RollupPayload {
    pub child_budget_id: Uuid,
}

/// Build a fully-populated `BudgetListItem` for a single budget INCLUDING its
/// rollup aggregated view (#52), mirroring `get_budget`'s read path. Used by the
/// rollup link/unlink handlers so their responses match the canonical get/list
/// shape (rollover carry, partial-rollover flag, aggregated_* fields, child ids).
async fn budget_item_for(
    pool: &PgPool,
    perm: Permission,
    budget: &Budget,
) -> Result<BudgetListItem, (StatusCode, String)> {
    let perm_str = match perm {
        Permission::Owner => "owner",
        Permission::Edit => "edit",
        _ => "view",
    };
    let total = computed_budget_total(pool, budget.id).await?;
    let (win_start, win_end) = previous_period_window(&budget.time_frame, chrono::Utc::now());
    let prev_spent = period_expense_spent(pool, budget.id, win_start, win_end).await?;
    let carried = effective_carried(&budget.budget_type, budget.rollover_enabled, total, prev_spent);
    let rollover_enabled = budget.rollover_enabled;
    let has_partial = rollover_enabled
        && budget.budget_type != "project"
        && has_partial_category_rollover(pool, budget.id).await?;
    let (own_base, own_effective, _own_spent) = aggregated_budget_amounts(pool, budget).await?;
    let child_ids = rollup_child_ids(pool, budget.id).await?;
    let (bi, bs) = computed_income_savings_totals(pool, budget.id).await?;

    Ok(BudgetListItem {
        id: budget.id,
        owner_id: budget.owner_id,
        owner_name: None,
        name: budget.name.clone(),
        description: budget.description.clone(),
        time_frame: budget.time_frame.clone(),
        budget_limit: total,
        is_default: budget.is_default,
        is_active: false,
        is_owner: perm == Permission::Owner,
        permission_level: perm_str.to_string(),
        created_at: budget.created_at,
        rollover_enabled: false,
        base_amount: 0.0,
        carried_amount: 0.0,
        effective_amount: 0.0,
        budget_type: budget.budget_type.clone(),
        closed_at: budget.closed_at,
        archived_at: budget.archived_at,
        has_partial_category_rollover: has_partial,
        auto_renew: budget.auto_renew,
        next_renewal_at: budget.next_renewal_at,
        rollup_parent_id: budget.rollup_parent_id,
        rollup_child_ids: Vec::new(),
        aggregated_base_amount: 0.0,
        aggregated_effective_amount: 0.0,
        aggregated_income_amount: 0.0,
        aggregated_savings_amount: 0.0,
        amount_mode: budget.amount_mode.clone(),
        budget_strategy: budget.budget_strategy.clone(),
        currency: budget.currency.clone(),
    }
    .with_amounts(rollover_enabled, total, carried)
    .with_rollup(child_ids, own_base, own_effective)
    .with_zero_based(bi, bs))
}

/// Roll a child budget up into a parent (#52). Owner-only on BOTH budgets (you can
/// only roll up budgets you own into a parent you own). Single-level: rejects
/// self-link (400), a parent that is itself a child (409), and a child that is
/// already a parent (409) — see `validate_rollup_link`. A child already linked to a
/// DIFFERENT parent must be unlinked first (409); re-linking to the SAME parent is
/// idempotent. Returns the PARENT's aggregated `BudgetListItem`.
pub async fn link_rollup(
    State(state): State<AppState>,
    Path(parent_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<RollupPayload>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let child_id = payload.child_budget_id;

    // Owner of the PARENT.
    let parent_perm = check_permission(&state.db, user_id, parent_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if parent_perm != Permission::Owner {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the budget owner can roll budgets up into it".to_string(),
        ));
    }
    // Owner of the CHILD — you can only roll up budgets you own.
    let child_perm = check_permission(&state.db, user_id, child_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if child_perm != Permission::Owner {
        return Err((
            StatusCode::FORBIDDEN,
            "You can only roll up budgets you own".to_string(),
        ));
    }
    // Gate on the CHILD's entitlement, not the parent's: a lapsed child owner
    // must not have their budget re-parented into someone else's rollup.
    crate::access::require_owner_entitled(&state.db, child_id).await?;

    let parent = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(parent_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Parent budget not found".to_string()))?;
    let child = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(child_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Child budget not found".to_string()))?;

    // Child already rolled up into a DIFFERENT parent -> must unlink first.
    // Re-linking to the SAME parent is idempotent (handled below: the UPDATE +
    // mirror INSERT both no-op / self-reconcile).
    if child.rollup_parent_id.is_some() && child.rollup_parent_id != Some(parent_id) {
        return Err((
            StatusCode::CONFLICT,
            "That budget is already rolled up into another budget; unlink it first".to_string(),
        ));
    }

    // Cycle / single-level guards. The parent is a child if it already has a
    // parent; the child is a parent if anything (even archived) is rolled up into
    // it. Both derived from the DB, then validated by the pure guard.
    let parent_is_child = parent.rollup_parent_id.is_some();
    let child_is_parent = is_rollup_parent(&state.db, child_id).await?;
    validate_rollup_link(
        parent_id,
        child_id,
        parent_is_child,
        child_is_parent,
        &parent.budget_type,
        &child.budget_type,
        &parent.budget_strategy,
        &child.budget_strategy,
    )?;

    // Resolve the mirror name from the parent's EXISTING category names, excluding
    // this child's own existing mirror (if any) so an idempotent re-link reuses the
    // same slot instead of spuriously suffixing/409-ing on its own name. This is a
    // read-only query, so we run it BEFORE opening the transaction: a name-collision
    // 409 is then returned without ever opening/holding a transaction.
    let existing_names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM categories \
         WHERE budget_id = $1 AND (linked_budget_id IS NULL OR linked_budget_id <> $2)",
    )
    .bind(parent_id)
    .bind(child_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;
    let mirror_name = rollup_category_name(&child.name, &existing_names)?;

    // Atomically (1) link the child to this parent and (2) create the mirror
    // expense category in the parent. Both steps are idempotent so re-linking to
    // the same parent reconciles a missing mirror without erroring.
    let mut tx = state.db.begin().await.map_err(internal_error)?;

    // The UPDATE only succeeds when the child is unlinked or already linked to THIS
    // parent. This closes a TOCTOU race: the pre-tx 409 guard above reads a pool
    // snapshot, so without this WHERE clause a concurrent link to a DIFFERENT parent
    // could be silently stolen here, orphaning the other parent's mirror. If
    // rows_affected() is 0 the child was concurrently re-parented elsewhere, so we
    // return 409 (the early return drops `tx`, rolling back).
    let linked = sqlx::query(
        "UPDATE budgets SET rollup_parent_id = $1 \
         WHERE id = $2 AND (rollup_parent_id IS NULL OR rollup_parent_id = $1)",
    )
    .bind(parent_id)
    .bind(child_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    if linked.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "That budget is already rolled up into another budget; unlink it first".to_string(),
        ));
    }

    // Insert the mirror. The unique index categories_one_mirror_per_source_idx
    // (budget_id, linked_budget_id) WHERE linked_budget_id IS NOT NULL makes this
    // idempotent: a re-link with an existing mirror is a no-op, and a missing
    // mirror is created (self-reconciling).
    //
    // The mirror category's NAME is a snapshot taken at link time and intentionally
    // does NOT track later renames of the source (child) budget; only its AMOUNT is
    // live (resolved on read). This is by design per the spec.
    sqlx::query(
        "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
         VALUES ($1, $2, $3, 'expense', NULL, $4) \
         ON CONFLICT (budget_id, linked_budget_id) WHERE linked_budget_id IS NOT NULL DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(parent_id)
    .bind(&mirror_name)
    .bind(child_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;

    tx.commit().await.map_err(internal_error)?;

    log_audit(
        &state.db,
        parent_id,
        user_id,
        "ROLLUP_BUDGET",
        &format!("Rolled budget '{}' up into '{}'", child.name, parent.name),
    )
    .await;

    Ok(Json(budget_item_for(&state.db, parent_perm, &parent).await?))
}

/// Unlink a child budget from its rollup parent (#52), restoring it to standalone.
/// Owner-only. Idempotent: unlinking a child that is not rolled up into this parent
/// (or not rolled up at all) is a no-op returning the parent's current state.
/// Returns the PARENT's aggregated `BudgetListItem`.
pub async fn unlink_rollup(
    State(state): State<AppState>,
    Path((parent_id, child_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<BudgetListItem>, (StatusCode, String)> {
    let parent_perm = check_permission(&state.db, user_id, parent_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if parent_perm != Permission::Owner {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the budget owner can unlink rolled-up budgets".to_string(),
        ));
    }
    // Gate on the PARENT — the only authorized entity here: this handler verifies
    // `parent_perm == Owner` but never checks the caller owns `child_id` (a raw path
    // param), so entitlement must be resolved from the parent, not the unverified
    // child (see fd922ab). This differs from link_rollup, which checks BOTH parent-
    // and child-ownership and so gates on the child.
    crate::access::require_owner_entitled(&state.db, parent_id).await?;

    let parent = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
        .bind(parent_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Parent budget not found".to_string()))?;

    // Atomically remove the mirror category and clear the link. Both are scoped to
    // THIS parent/child pair and idempotent: if nothing is linked, both affect 0
    // rows and we still return the parent's current state (no error, no audit churn).
    let mut tx = state.db.begin().await.map_err(internal_error)?;

    sqlx::query("DELETE FROM categories WHERE budget_id = $1 AND linked_budget_id = $2")
        .bind(parent_id)
        .bind(child_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    // Only clear the link when the child is actually rolled up into THIS parent.
    let affected = sqlx::query(
        "UPDATE budgets SET rollup_parent_id = NULL WHERE id = $1 AND rollup_parent_id = $2",
    )
    .bind(child_id)
    .bind(parent_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?
    .rows_affected();

    tx.commit().await.map_err(internal_error)?;

    if affected > 0 {
        log_audit(
            &state.db,
            parent_id,
            user_id,
            "UNROLLUP_BUDGET",
            &format!("Unlinked rolled-up budget {} from '{}'", child_id, parent.name),
        )
        .await;
    }

    Ok(Json(budget_item_for(&state.db, parent_perm, &parent).await?))
}

// --- CATEGORY HANDLERS ---

/// The (prev_period_spent, carried_amount) selection shared by the REST
/// category read path (`category_response_with_carry`) and the categories view
/// endpoint (`categories_view`, #426), so the two surfaces can never disagree
/// about a category's effective amount.
///
/// Fund precedence (#228): a fund category's carry comes from the materialized
/// `fund_balance`, NOT the #49 one-period carry — stacking both would
/// double-count the same accumulated credit. A #52 mirror and any non-expense
/// category never carry at all.
pub fn category_carry_for(
    is_expense: bool,
    is_linked: bool,
    is_fund: bool,
    fund_balance: f64,
    budget_rollover: bool,
    rollover_enabled: bool,
    budget_type: &str,
    base: f64,
    prev_spent: f64,
) -> (f64, f64) {
    if is_expense && !is_linked && is_fund {
        (0.0, fund_balance)
    } else if is_expense && !is_linked {
        (
            prev_spent,
            category_carried(budget_rollover, rollover_enabled, budget_type, base, prev_spent),
        )
    } else {
        (0.0, 0.0)
    }
}

/// Build a `CategoryResponse` with computed carry (#49) for a single category.
/// `prev_spent` is this category's previous-period expense spend (only
/// meaningful for expense categories; pass 0 otherwise). Non-expense categories
/// never carry. Centralizes the base/carry/effective math so list, create, and
/// update cannot drift.
///
/// `linked_base` carries the resolved source-budget expense total for a linked
/// (mirror) category (#52); it is ignored for ordinary categories. When the
/// category is linked (`linked_budget_id.is_some()`) its `base_amount` is that
/// source total, `carried_amount` is 0, and `effective_amount` equals the base —
/// the mirror reflects the source's live envelope, it does not carry on its own.
fn category_response_with_carry(
    cat: Category,
    budget_rollover: bool,
    budget_type: &str,
    prev_spent: f64,
    linked_base: f64,
) -> CategoryResponse {
    let is_linked = cat.linked_budget_id.is_some();
    let is_expense = cat.category_type == "expense";
    let base = if is_linked {
        linked_base
    } else {
        cat.category_limit.unwrap_or(0.0)
    };
    let (prev, carried) = category_carry_for(
        is_expense,
        is_linked,
        cat.is_fund,
        cat.fund_balance,
        budget_rollover,
        cat.rollover_enabled,
        budget_type,
        base,
        prev_spent,
    );
    CategoryResponse {
        id: cat.id,
        budget_id: cat.budget_id,
        name: cat.name,
        category_type: cat.category_type,
        category_limit: cat.category_limit,
        rollover_enabled: cat.rollover_enabled,
        created_at: cat.created_at,
        base_amount: base,
        carried_amount: carried,
        effective_amount: base + carried,
        prev_period_spent: prev,
        linked_budget_id: cat.linked_budget_id,
        is_fund: cat.is_fund,
        fund_balance: cat.fund_balance,
    }
}

pub async fn create_category(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CategoryPayload>,
) -> Result<Json<CategoryResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    ensure_not_closed(&state.db, budget_id).await?;

    // Reject junk category types up front — the carry logic keys off the literal
    // "expense", so an invalid type would silently never carry (#49).
    validate_category_type(&payload.category_type)?;

    let category_id = Uuid::new_v4();

    let cat = sqlx::query_as::<_, Category>(
        "INSERT INTO categories (id, budget_id, name, category_type, category_limit, rollover_enabled) VALUES ($1, $2, $3, $4, $5, $6) RETURNING *"
    )
    .bind(category_id)
    .bind(budget_id)
    .bind(&payload.name)
    .bind(&payload.category_type)
    .bind(payload.category_limit)
    .bind(payload.rollover_enabled.unwrap_or(true))
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        // A name already used in this budget violates the (budget_id, name)
        // UNIQUE constraint. Surface the SAME 409 `update_category` returns
        // rather than letting `internal_error` swallow it into the constant
        // "Internal server error" body — the add-category form has no way to
        // tell the user which field is wrong from a 500, and retrying is
        // guaranteed to fail forever (#426).
        duplicate_category_name_error(&e)
            .unwrap_or_else(|| internal_error(format!("Failed to create category: {}", e)))
    })?;

    log_audit(&state.db, budget_id, user_id, "CREATE_CATEGORY", &format!("Created category: {} ({})", cat.name, cat.category_type)).await;

    // Route through the shared response builder to avoid drift with the read
    // sites. A brand-new category has no previous period, so prev_spent is 0 —
    // the helper then yields carried=0 and effective=base regardless of the
    // rollover switches. We still fetch the budget's rollover/type so the shared
    // path is identical to update_category's.
    let brow = sqlx::query(
        "SELECT rollover_enabled, budget_type FROM budgets WHERE id = $1",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to load budget rollover settings: {}", e)))?;
    let budget_rollover: bool = brow.get("rollover_enabled");
    let budget_type: String = brow.get("budget_type");

    // A linked (mirror) category reflects the source budget's own expense total
    // (#52). Ordinary categories resolve to 0 here (unused by the helper).
    let linked_base = match cat.linked_budget_id {
        Some(src) => computed_budget_total(&state.db, src).await?,
        None => 0.0,
    };

    Ok(Json(category_response_with_carry(cat, budget_rollover, &budget_type, 0.0, linked_base)))
}

pub async fn list_categories(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<CategoryResponse>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let rows = sqlx::query_as::<_, Category>("SELECT * FROM categories WHERE budget_id = $1 ORDER BY name ASC")
        .bind(budget_id)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;

    // Fetch the budget's rollover master switch, type, and time_frame in one
    // query to drive the per-category carry computation (#49).
    let brow = sqlx::query(
        "SELECT rollover_enabled, budget_type, time_frame FROM budgets WHERE id = $1",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to load budget rollover settings: {}", e)))?;
    let budget_rollover: bool = brow.get("rollover_enabled");
    let budget_type: String = brow.get("budget_type");
    let time_frame: String = brow.get("time_frame");

    // Source each expense category's previous-period spend in one batched query.
    // A failure here PROPAGATES (#432): a failed carry read is an error, not a
    // licence to serve a plausible-but-wrong number.
    //
    // Note the degradation INFLATED the carry rather than zeroing it — #432's own
    // description has this backwards, and the direction matters. An empty map made
    // `prev_spent` 0, and `category_carried` is `(base - prev_spent).max(0.0)`, so
    // every ordinary (non-fund, non-mirror) EXPENSE category with rollover on
    // carried its ENTIRE limit forward: measured on the 100-limit / 80-prev-spend
    // fixture in `list_categories_propagates_a_failed_previous_period_spend_read`,
    // a failed read turned the true `carried 20 / effective 120` into
    // `carried 100 / effective 200`. "You spent nothing last period" reads as
    // "you carry everything forward", so the user is shown roughly double their
    // real headroom and is invited to overspend into money that is not there.
    // Funds and mirrors were untouched: `category_carry_for` short-circuits both
    // before `prev_spent` is consulted. It also made this surface disagree with
    // `categories_view`, which already propagates with `?` on the same data under
    // the same failure.
    //
    // That rule scopes to the REST read/write surfaces (here, `update_category`,
    // `categories_view`). `rag.rs` deliberately does the OPPOSITE with the same
    // helper — it logs and degrades to an empty map, because a partial DB failure
    // must not take chat down wholesale. The divergence is intentional, not drift;
    // reconciling the two surfaces is tracked as #477.
    //
    // Skip the query entirely when no category can carry — the budget master
    // switch is off or this is a project budget — since `category_carried` returns
    // 0 for every category regardless of prev spend (#49). Like `categories_view`,
    // that fast path leaves every row's `prev_period_spent` AND `carried_amount`
    // at 0, so a category that genuinely spent last period reports 0 spend here;
    // the carry itself is honestly 0, so this is a skip, not a degradation.
    let prev_map = if budget_rollover && budget_type != "project" {
        let (win_start, win_end) = previous_period_window(&time_frame, chrono::Utc::now());
        period_category_expense_spent_many(&state.db, &[(budget_id, win_start, win_end)]).await?
    } else {
        std::collections::HashMap::new()
    };

    // Resolve each linked (mirror) category's source-budget expense total up front
    // (#52) in TWO batched queries — the source totals and the set of ARCHIVED
    // sources — instead of a `computed_budget_total` round-trip per mirror (avoids
    // an N+1 when a parent mirrors many sources). An archived source contributes 0,
    // matching `computed_budget_total` on the parent, so its mirror's base is 0
    // too. The map is keyed by category id; the sync map closure below reads it.
    let linked_src_ids: Vec<Uuid> = rows.iter().filter_map(|c| c.linked_budget_id).collect();
    let (src_totals, archived_srcs) = if linked_src_ids.is_empty() {
        (std::collections::HashMap::new(), std::collections::HashSet::new())
    } else {
        let totals = computed_budget_totals(&state.db, &linked_src_ids).await?;
        let archived: std::collections::HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM budgets WHERE id = ANY($1) AND archived_at IS NOT NULL",
        )
        .bind(&linked_src_ids)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?
        .into_iter()
        .collect();
        (totals, archived)
    };
    let linked_bases: std::collections::HashMap<Uuid, f64> = rows
        .iter()
        .filter_map(|c| {
            c.linked_budget_id.map(|src| {
                let base = if archived_srcs.contains(&src) {
                    0.0
                } else {
                    src_totals.get(&src).copied().unwrap_or(0.0)
                };
                (c.id, base)
            })
        })
        .collect();

    let resp = rows
        .into_iter()
        .map(|c| {
            let prev_spent = prev_map.get(&c.id).copied().unwrap_or(0.0);
            let linked_base = linked_bases.get(&c.id).copied().unwrap_or(0.0);
            category_response_with_carry(c, budget_rollover, &budget_type, prev_spent, linked_base)
        })
        .collect();

    Ok(Json(resp))
}

/// Edit one category's name / type / limit / rollover / fund flag.
///
/// ATOMIC as of #426: the fund disable, the main UPDATE, the fund enable and
/// the reconciling re-read all run inside ONE transaction, so the request
/// either applies in full or leaves the row exactly as it was. This matters
/// because the categories editor sends `name` and `is_fund` in the SAME
/// request: previously a rename onto a taken name returned 409 from the main
/// UPDATE while the already-committed disable had silently switched the fund
/// off and stopped its accrual, with nothing in the response saying so.
///
/// The pre-write validation and the disable-before-UPDATE /
/// enable-after-UPDATE ordering are retained: the transaction makes the
/// sequence all-or-nothing, but `categories_is_fund_expense_only_check` is an
/// IMMEDIATE constraint, so an out-of-order write inside the transaction would
/// still fail — as a bare 500 rather than the actionable 400 the pre-write
/// check produces. The ordering is separately regression-tested by
/// `update_category_fund_toggle_orders_writes_around_the_expense_check`.
pub async fn update_category(
    State(state): State<AppState>,
    Path((budget_id, category_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CategoryUpdatePayload>,
) -> Result<Json<CategoryResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    ensure_not_closed(&state.db, budget_id).await?;

    // If the caller is changing category_type, it must be one of the known kinds
    // — the carry logic keys off the literal "expense", so an invalid type would
    // silently misclassify the category and never carry (#49).
    if let Some(ct) = payload.category_type.as_deref() {
        validate_category_type(ct)?;
    }

    // A linked rollup mirror category (#52, `linked_budget_id IS NOT NULL`) is
    // system-managed: its shape is an invariant — `category_type = 'expense'`,
    // `category_limit = NULL` — that `computed_budget_total` relies on. Letting a
    // client edit it would either silently corrupt the parent total (changing the
    // type drops the mirror from the expense-only sum, so the rolled-up source
    // vanishes) or set a limit that the computed-amount model ignores. Reject the
    // edit; the mirror is managed by rolling the source budget up / unlinking it,
    // never by editing the category directly. A genuine miss (no such category in
    // this budget) 404s here rather than being mistaken for a mirror.
    //
    // The whole row is read rather than just `linked_budget_id` because the
    // fund toggle's pre-write VALIDATION below needs the current type and fund
    // flag to resolve the intended end-state. It is only a validation input: a
    // stale read here can at worst produce a spurious 400, never a silent wrong
    // write, because the toggle's writes are NOT gated on it.
    let current = sqlx::query_as::<_, Category>(
        "SELECT * FROM categories WHERE id = $1 AND budget_id = $2",
    )
    .bind(category_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Category not found".to_string()))?;
    if current.linked_budget_id.is_some() {
        return Err((
            StatusCode::CONFLICT,
            "This is a linked rollup category and can't be edited directly. Manage it by rolling the source budget up or unlinking it.".to_string(),
        ));
    }

    // Fund toggle (#426). Resolve the intended end-state BEFORE writing anything:
    // the categories_is_fund_expense_only_check CHECK makes an is_fund +
    // non-expense pair unrepresentable, so validating after the UPDATE would
    // surface a bare 500 instead of an actionable 400. The writes below are
    // transactional, so a late failure no longer leaves a partial write — but a
    // pre-write 400 is still the right shape for an invalid request.
    let target_type = payload
        .category_type
        .as_deref()
        .unwrap_or(current.category_type.as_str());
    let target_fund = payload.is_fund.unwrap_or(current.is_fund);

    if target_fund && target_type != "expense" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Only expense categories can be funds. Turn off fund status before changing the type."
                .to_string(),
        ));
    }

    // Everything from here to the commit is ONE transaction (#426). The fund
    // disable, the main UPDATE, the fund enable and the reconciling re-read
    // used to be four independent statements, so a rename that tripped the
    // `unique_category_name_per_budget` constraint returned 409 with the fund
    // ALREADY switched off and its accrual stopped — invisible to the client,
    // which had only been told the name was taken. Any error path below drops
    // `tx` and rolls the whole sequence back; the row is touched only if the
    // handler reaches `tx.commit()`.
    let mut tx = state.db.begin().await.map_err(internal_error)?;

    // Disable BEFORE the main UPDATE so a simultaneous type change cannot
    // transiently violate the CHECK.
    //
    // Called whenever the payload ASKS for it — deliberately NOT gated on the
    // `current` snapshot read above. Idempotency lives in the helper's SQL
    // (`WHERE id = $1 AND is_fund = TRUE`), so the DATABASE decides whether the
    // row actually changes. Re-adding a `&& current.is_fund` gate here would
    // reintroduce a lost update: two concurrent PUTs can share a pre-image, and
    // the second would skip the write it explicitly asked for while still
    // returning 200. Do not "optimize" the gate back in.
    if payload.is_fund == Some(false) {
        disable_category_fund(&mut *tx, category_id)
            .await
            .map_err(internal_error)?;
    }

    let cat = sqlx::query_as::<_, Category>(
        "UPDATE categories SET \
            name = COALESCE($1, name), \
            category_type = COALESCE($2, category_type), \
            category_limit = COALESCE($3, category_limit), \
            rollover_enabled = COALESCE($4, rollover_enabled) \
         WHERE id = $5 AND budget_id = $6 RETURNING *",
    )
    .bind(&payload.name)
    .bind(&payload.category_type)
    .bind(payload.category_limit)
    .bind(payload.rollover_enabled)
    .bind(category_id)
    .bind(budget_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        // Renaming a category to a name already used in this budget violates the
        // (budget_id, name) UNIQUE constraint — surface a 409 instead of a
        // generic 500 (#49). `create_category` maps the same violation to the
        // same status and the SAME message, via `duplicate_category_name_error`,
        // so the two paths are indistinguishable to the client.
        duplicate_category_name_error(&e)
            .unwrap_or_else(|| internal_error(format!("Failed to update category: {}", e)))
    })?
    .ok_or((StatusCode::NOT_FOUND, "Category not found".to_string()))?;

    // Enable AFTER the main UPDATE, when the type is guaranteed 'expense'. As
    // with the disable above, this is driven by the PAYLOAD alone, never by the
    // `current` snapshot: idempotency lives in the helper's SQL guard
    // (`WHERE id = $2 AND is_fund = FALSE`), which makes a repeat enable a
    // 0-row no-op that leaves `fund_balance` / `fund_advanced_through` exactly
    // as they stand. Gating on the stale read instead would let a concurrent
    // write silently swallow an explicit request.
    //
    // The budget's time_frame is read here rather than reused from the response
    // block below because that block runs after this one; the extra round trip
    // only happens on the enable path, and the binding is scoped to this branch
    // so it can't collide with the response block's own `time_frame`.
    //
    // Whenever the payload touched the fund flag at all, the row is re-read
    // ONCE afterwards so the response reflects the fund columns as they
    // actually stand — the main UPDATE's `RETURNING *` image predates the
    // enable, and nothing else in this handler reconciles it.
    let cat = if payload.is_fund.is_some() {
        if payload.is_fund == Some(true) {
            let time_frame: String =
                sqlx::query_scalar("SELECT time_frame FROM budgets WHERE id = $1")
                    .bind(budget_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_error)?;
            enable_category_fund(&mut *tx, category_id, &time_frame)
                .await
                .map_err(internal_error)?;
        }
        sqlx::query_as::<_, Category>(
            "SELECT * FROM categories WHERE id = $1 AND budget_id = $2",
        )
        .bind(category_id)
        .bind(budget_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal_error)?
    } else {
        cat
    };

    // Nothing above has been made visible to anyone else yet; this is the point
    // at which the whole disable → UPDATE → enable sequence becomes durable.
    tx.commit().await.map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "UPDATE_CATEGORY", &format!("Updated category: {}", cat.name)).await;

    // Build the response with computed carry, sourcing the budget's rollover
    // settings and this category's previous-period spend (#49).
    let brow = sqlx::query(
        "SELECT rollover_enabled, budget_type, time_frame FROM budgets WHERE id = $1",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to load budget rollover settings: {}", e)))?;
    let budget_rollover: bool = brow.get("rollover_enabled");
    let budget_type: String = brow.get("budget_type");
    let time_frame: String = brow.get("time_frame");

    // Skip the previous-period spend query when no category can carry — the
    // budget master switch is off or this is a project budget — since the carry
    // is 0 either way (#49).
    //
    // When the query DOES run, a failure PROPAGATES (#432), on the rule stated in
    // full at the matching binding in `list_categories` — including the fact that
    // the old degradation INFLATED the carry to the whole limit rather than
    // zeroing it. What is specific to THIS handler: the read runs AFTER
    // `tx.commit()` above, so a failure returns 500
    // on a write that IS durable. That is accepted — the alternative is lying
    // about the money — and it matches the `SELECT rollover_enabled, budget_type,
    // time_frame FROM budgets` immediately above, the pre-existing post-commit
    // propagating precedent in this block. (The fund re-read further up runs on
    // the transaction, before the commit, so it is not the same case.)
    //
    // A third option was weighed and REJECTED: making
    // `period_category_expense_spent_many` generic over `sqlx::Executor` — as
    // `enable_category_fund` / `disable_category_fund` already are (AGENTS.md §
    // `CategoryUpdatePayload.is_fund`) — would let this read run inside the
    // transaction, making the failure atomic and removing the durable-write 500
    // entirely. The price is a signature change across its four call sites and
    // holding the `categories` row lock across one more query. We chose not to;
    // it was weighed, not missed. Do not restore the degradation either.
    let prev_spent = if budget_rollover && budget_type != "project" {
        let (win_start, win_end) = previous_period_window(&time_frame, chrono::Utc::now());
        period_category_expense_spent_many(&state.db, &[(budget_id, win_start, win_end)])
            .await?
            .get(&cat.id)
            .copied()
            .unwrap_or(0.0)
    } else {
        0.0
    };

    // Mirror categories are rejected up front (see the `is_mirror` guard above),
    // so `cat` is always an ordinary category here and its linked base is 0 — the
    // shared response helper ignores `linked_base` for non-mirror rows anyway.
    Ok(Json(category_response_with_carry(cat, budget_rollover, &budget_type, prev_spent, 0.0)))
}

pub async fn delete_category(
    State(state): State<AppState>,
    Path((budget_id, category_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    ensure_not_closed(&state.db, budget_id).await?;

    // Scope the lookup/delete to this budget so a category can never be removed
    // from a budget the URL doesn't name. Transactions reference the category
    // with ON DELETE SET NULL, so they survive as uncategorized.
    let row = sqlx::query("SELECT name FROM categories WHERE id = $1 AND budget_id = $2")
        .bind(category_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

    let name: String = match row {
        Some(r) => r.get("name"),
        None => return Err((StatusCode::NOT_FOUND, "Category not found".to_string())),
    };

    let deleted = sqlx::query("DELETE FROM categories WHERE id = $1 AND budget_id = $2")
        .bind(category_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    // #434. The SELECT above and this DELETE are two statements, so the row can
    // disappear between them; `rows_affected() == 0` then means THIS request
    // removed nothing, and reporting 204 there is indistinguishable from a real
    // deletion. Treat it as "not found" — mirroring `set_default_budget`'s
    // rows_affected guard — and warn, so the race leaves an operator-visible
    // trace the 404 alone does not. The 404 body is deliberately the SAME text
    // as the existence check's above: the caller's remedy is identical either
    // way, so the warn, not the response, is what separates the two for us.
    // That makes the warn contract rather than incidental logging, and a test
    // captures it.
    //
    // Do NOT over-read the window this covers. It is exactly these two
    // statements — microseconds wide, and the SAME for every caller. A category
    // that vanished EARLIER never reaches here at all: the existence check above
    // already 404s it. That is what actually covers the long, human-scale gaps,
    // on BOTH surfaces — the seconds a user spends in the chat flow's
    // confirmation modal (`rag.rs`'s `DELETE_CATEGORY` is resolve-only; it hands
    // a PendingDeletion to the frontend, which then calls this endpoint), and
    // the equally unbounded gap between the categories page loading its rows and
    // the user tapping delete. Neither caller is more exposed to THIS branch than
    // the other. A concurrently deleted BUDGET usually does not reach here
    // either: `check_permission` above sees no access and returns 403 first.
    //
    // Deliberately NOT closed (no transaction, no `FOR UPDATE`, no
    // `DELETE ... RETURNING name`): #434 asks for the zero-row case to be
    // SURFACED, not eliminated.
    //
    // The converse hole is accepted and unchanged: `log_audit` below is
    // fail-safe by #168, so a REAL deletion whose audit write fails still
    // returns 204 with no audit row. Suppressing the audit row here only
    // prevents the opposite lie, and note it does not guarantee some other row
    // records the disappearance — a cascading `delete_budget` or an
    // `unlink_rollup` mirror delete writes no `DELETE_CATEGORY` at all.
    if deleted.rows_affected() == 0 {
        tracing::warn!(
            %budget_id,
            %category_id,
            %user_id,
            "delete_category matched zero rows after its existence check passed; \
             the category was removed concurrently"
        );
        return Err((StatusCode::NOT_FOUND, "Category not found".to_string()));
    }

    log_audit(&state.db, budget_id, user_id, "DELETE_CATEGORY", &format!("Deleted category: {}", name)).await;

    Ok(StatusCode::NO_CONTENT)
}

// --- TRANSACTION HANDLERS ---

pub async fn create_transaction(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<TransactionPayload>,
) -> Result<Json<CreateTransactionResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to add transactions".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    ensure_not_closed(&state.db, budget_id).await?;

    let tx_id = Uuid::new_v4();
    let date = payload.transaction_date.unwrap_or_else(Utc::now);

    // Generate a semantic embedding for the description so the transaction is
    // searchable (#195). Tolerant: with no GEMINI_API_KEY (or on any embedding
    // failure) we insert NULL and the transaction is still created — embedding is
    // a best-effort enrichment, never a hard requirement. The backfill job
    // (backfill.rs) can fill NULLs later.
    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    let embedding = if !api_key.is_empty() {
        crate::rag::get_gemini_embedding(&payload.description, &api_key, &state.db, user_id).await
    } else {
        None
    };
    let embedding_str = embedding.as_ref().map(|e| crate::rag::vector_to_string(e));

    let transaction = sqlx::query_as::<_, Transaction>(
        // Not reachable from the UI today; writes source='manual' to reserve that
        // provenance value for the future manual add-transaction form (#403).
        "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description, embedding, source)
         VALUES ($1, $2, $3, $4, $5, $6, $7::vector, 'manual') RETURNING *"
    )
    .bind(tx_id)
    .bind(budget_id)
    .bind(payload.category_id)
    .bind(payload.amount)
    .bind(date)
    .bind(&payload.description)
    .bind(embedding_str)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to create transaction: {}", e)))?;

    log_audit(&state.db, budget_id, user_id, "ADD_TRANSACTION", &format!("Logged transaction: {} - ${}", transaction.description, payload.amount)).await;

    // Reverse duplicate match (#403 P5): if the bank already imported this charge,
    // link that import to this row so the user gets a resolvable duplicate instead
    // of a silent double-count. Non-destructive, error-swallowing.
    crate::duplicate_match::link_duplicate_for_logged(&state.db, budget_id, transaction.id).await;

    // Fetch category details
    let mut cat_name = None;
    let mut cat_type = None;
    if let Some(cat_id) = transaction.category_id {
        let cat_row = sqlx::query("SELECT name, category_type FROM categories WHERE id = $1")
            .bind(cat_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
        
        if let Some(r) = cat_row {
            cat_name = Some(r.get::<String, &str>("name"));
            cat_type = Some(r.get::<String, &str>("category_type"));
        }
    }

    let response = TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
        updated_at: transaction.updated_at,
        excluded_from_budget: transaction.excluded_from_budget,
        source: transaction.source,
        currency: transaction.currency,
        external_account_id: transaction.external_account_id,
        account_label: fetch_account_label(&state.db, transaction.external_account_id).await,
        review_status: transaction.review_status,
        matched_transaction_id: transaction.matched_transaction_id,
        provider_transaction_id: transaction.provider_transaction_id.clone(),
    };

    let alerts = crate::notifications::check_and_notify_limits(
        &state.db, user_id, budget_id, response.category_id,
    ).await;

    Ok(Json(CreateTransactionResponse { transaction: response, alerts }))
}

/// Resolve a pending category choice (#376) into a logged transaction, reusing
/// the shared find-or-create + embedding-insert helpers so a chip-logged
/// transaction is semantically searchable and any created category is flagged
/// `auto_created`. Called by the frontend when the user taps a category chip.
pub async fn finalize_transaction(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<FinalizeTransactionPayload>,
) -> Result<Json<FinalizeTransactionResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to add transactions".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    // Resolve the target category: an existing pick (validated to this budget) or
    // a brand-new one (flagged auto_created so the cleanup can reclaim it later).
    let (category_id, category_created) = if let Some(cid) = payload.category_id {
        let in_budget: bool = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1 AND budget_id = $2)",
        )
        .bind(cid)
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;
        if !in_budget {
            return Err((StatusCode::NOT_FOUND, "Category not found in this budget".to_string()));
        }
        (cid, false)
    } else if let Some(name) = payload
        .new_category_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        find_or_create_category(&state.db, budget_id, name, "expense", true)
            .await
            .map_err(|e| internal_error(format!("Failed to resolve category: {}", e)))?
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "category_id or new_category_name is required".to_string(),
        ));
    };

    let tx_id = insert_transaction_with_embedding(
        &state.db, budget_id, Some(category_id), payload.amount, &payload.description, user_id,
    )
    .await
    .map_err(|e| internal_error(format!("Failed to log transaction: {}", e)))?;

    log_audit(
        &state.db, budget_id, user_id, "ADD_TRANSACTION",
        &format!("Logged transaction: {} - ${:.2}", payload.description, payload.amount),
    ).await;

    let tx = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1 AND budget_id = $2",
    )
    .bind(tx_id)
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    // The category was just resolved/created, so it exists — propagate a lookup
    // error (don't swallow it to a blank name that renders "Logged $X to .").
    let mut cat_name = None;
    let mut cat_type = None;
    if let Some(r) = sqlx::query("SELECT name, category_type FROM categories WHERE id = $1 AND budget_id = $2")
        .bind(category_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
    {
        cat_name = Some(r.get::<String, &str>("name"));
        cat_type = Some(r.get::<String, &str>("category_type"));
    }

    let response = TransactionResponse {
        id: tx.id,
        budget_id: tx.budget_id,
        category_id: tx.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: tx.amount,
        description: tx.description,
        transaction_date: tx.transaction_date,
        created_at: tx.created_at,
        updated_at: tx.updated_at,
        excluded_from_budget: tx.excluded_from_budget,
        source: tx.source,
        currency: tx.currency,
        external_account_id: tx.external_account_id,
        account_label: fetch_account_label(&state.db, tx.external_account_id).await,
        review_status: tx.review_status,
        matched_transaction_id: tx.matched_transaction_id,
        provider_transaction_id: tx.provider_transaction_id.clone(),
    };

    let alerts = crate::notifications::check_and_notify_limits(
        &state.db, user_id, budget_id, Some(category_id),
    ).await;

    Ok(Json(FinalizeTransactionResponse { transaction: response, category_created, alerts }))
}

pub async fn update_transaction(
    State(state): State<AppState>,
    Path((budget_id, transaction_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<TransactionUpdatePayload>,
) -> Result<Json<CreateTransactionResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to edit transactions".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    // Load the existing row, scoped to this budget. A miss here is the
    // cross-budget / unknown-id guard (#199 AC2).
    let existing = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1 AND budget_id = $2",
    )
    .bind(transaction_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Transaction not found".to_string()))?;

    // A supplied category must belong to THIS budget. transactions.category_id
    // only FKs categories(id) (not (budget_id, id)), so without this check a
    // caller could point the transaction at another budget's category — which
    // also makes the category-scoped limit check (notifications filter by
    // c.budget_id) silently skip alerts. Reject the mismatch up front.
    if let Some(cid) = payload.category_id {
        let in_budget: bool = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1 AND budget_id = $2)",
        )
        .bind(cid)
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;
        if !in_budget {
            return Err((StatusCode::NOT_FOUND, "Category not found in this budget".to_string()));
        }
    }

    // Capture the pre-update category so, if the edit reassigns the transaction,
    // an auto-created source category left empty can be cleaned up (#376).
    let old_category_id: Option<Uuid> = existing.category_id;

    let new_description = payload.description.clone().unwrap_or_else(|| existing.description.clone());
    let description_changed = new_description != existing.description;
    let new_category_id = payload.category_id.or(existing.category_id);
    let category_changed = new_category_id != existing.category_id;
    let new_amount = payload.amount.unwrap_or(existing.amount);
    // Exact equality: an unchanged amount is the same f64 value (either the
    // stored value reused, or a JSON-parsed copy), so no epsilon is warranted.
    let amount_changed = new_amount != existing.amount;

    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();

    // On a description change, regenerate the embedding (best-effort, #195):
    // on no key / failure store NULL; the backfill job repopulates. On a
    // non-description edit, leave the embedding column untouched.
    let transaction = if description_changed {
        let embedding = if !api_key.is_empty() {
            crate::rag::get_gemini_embedding(&new_description, &api_key, &state.db, user_id).await
        } else {
            None
        };
        let embedding_str = embedding.as_ref().map(|e| crate::rag::vector_to_string(e));
        sqlx::query_as::<_, Transaction>(
            "UPDATE transactions SET \
                category_id = COALESCE($1, category_id), \
                amount = COALESCE($2, amount), \
                transaction_date = COALESCE($3, transaction_date), \
                description = COALESCE($4, description), \
                excluded_from_budget = COALESCE($5, excluded_from_budget), \
                embedding = $6::vector \
             WHERE id = $7 AND budget_id = $8 RETURNING *",
        )
        .bind(payload.category_id)
        .bind(payload.amount)
        .bind(payload.transaction_date)
        .bind(&payload.description)
        .bind(payload.excluded_from_budget)
        .bind(embedding_str)
        .bind(transaction_id)
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| internal_error(format!("Failed to update transaction: {}", e)))?
    } else {
        sqlx::query_as::<_, Transaction>(
            "UPDATE transactions SET \
                category_id = COALESCE($1, category_id), \
                amount = COALESCE($2, amount), \
                transaction_date = COALESCE($3, transaction_date), \
                description = COALESCE($4, description), \
                excluded_from_budget = COALESCE($5, excluded_from_budget) \
             WHERE id = $6 AND budget_id = $7 RETURNING *",
        )
        .bind(payload.category_id)
        .bind(payload.amount)
        .bind(payload.transaction_date)
        .bind(&payload.description)
        .bind(payload.excluded_from_budget)
        .bind(transaction_id)
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| internal_error(format!("Failed to update transaction: {}", e)))?
    };

    // Only audit a real change. An all-None payload COALESCEs every field to its
    // current value, so nothing changed (the trigger likewise leaves updated_at
    // untouched) — writing an audit row for that no-op would be misleading.
    let date_changed = payload
        .transaction_date
        .is_some_and(|d| d != existing.transaction_date);
    let excluded_changed = payload
        .excluded_from_budget
        .is_some_and(|v| v != existing.excluded_from_budget);
    if amount_changed || category_changed || description_changed || date_changed || excluded_changed {
        log_audit(
            &state.db, budget_id, user_id, "EDIT_TRANSACTION",
            &format!(
                "Edited transaction '{}' (${:.2}) -> '{}' (${:.2})",
                existing.description, existing.amount, transaction.description, transaction.amount,
            ),
        ).await;
    }

    let mut cat_name = None;
    let mut cat_type = None;
    if let Some(cat_id) = transaction.category_id {
        // Budget-scope the metadata lookup so category name/type can never leak
        // across budgets even if a row were ever mis-linked.
        if let Some(r) = sqlx::query("SELECT name, category_type FROM categories WHERE id = $1 AND budget_id = $2")
            .bind(cat_id)
            .bind(budget_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None)
        {
            cat_name = Some(r.get::<String, &str>("name"));
            cat_type = Some(r.get::<String, &str>("category_type"));
        }
    }

    let response = TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
        updated_at: transaction.updated_at,
        excluded_from_budget: transaction.excluded_from_budget,
        source: transaction.source,
        currency: transaction.currency,
        external_account_id: transaction.external_account_id,
        account_label: fetch_account_label(&state.db, transaction.external_account_id).await,
        review_status: transaction.review_status,
        matched_transaction_id: transaction.matched_transaction_id,
        provider_transaction_id: transaction.provider_transaction_id.clone(),
    };

    let alerts = if amount_changed || category_changed || excluded_changed {
        crate::notifications::check_and_notify_limits(
            &state.db, user_id, budget_id, response.category_id,
        ).await
    } else {
        Vec::new()
    };

    // If the reassignment emptied an auto-created source category, remove it (#376).
    if category_changed {
        // The old category's counted spend dropped — reconcile its limit alerts too (#388).
        let _ = crate::notifications::check_and_notify_limits(
            &state.db, user_id, budget_id, old_category_id,
        ).await;
        cleanup_orphaned_auto_category(&state.db, old_category_id).await;
    }

    Ok(Json(CreateTransactionResponse { transaction: response, alerts }))
}

/// Approve a bank-synced transaction (#403 P2): flip `review_status` from
/// `needs_review` to `reviewed`, clearing the "Needs review" indicator on the
/// list. Mirrors `update_transaction`'s permission + audit discipline: Owner/Edit
/// only, budget-scoped 404 guard, closed-budget guard, and an audit row on a real
/// state change. Idempotent — approving an already-`reviewed` row is a 200 no-op
/// with no audit entry (re-clicking, or a stale optimistic retry, is harmless).
pub async fn approve_transaction(
    State(state): State<AppState>,
    Path((budget_id, transaction_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<TransactionResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to approve transactions".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    // Load the existing row, scoped to this budget. A miss is the cross-budget /
    // unknown-id guard (mirrors update_transaction).
    let existing = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1 AND budget_id = $2",
    )
    .bind(transaction_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Transaction not found".to_string()))?;

    // Idempotency is enforced by the `review_status = 'needs_review'` guard IN
    // the UPDATE, not by the pre-read above: two concurrent approvals both see
    // `needs_review` when they SELECT, but only ONE matches the guarded UPDATE
    // and gets a RETURNING row — so exactly one audit entry is written, no matter
    // how the calls race. A racing/duplicate approval matches zero rows and is a
    // clean no-op.
    let updated = sqlx::query_as::<_, Transaction>(
        "UPDATE transactions SET review_status = 'reviewed' \
         WHERE id = $1 AND budget_id = $2 AND review_status = 'needs_review' RETURNING *",
    )
    .bind(transaction_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to approve transaction: {}", e)))?;

    let transaction = match updated {
        Some(row) => {
            log_audit(
                &state.db, budget_id, user_id, "APPROVE_TRANSACTION",
                &format!(
                    "Approved transaction '{}' (${:.2})",
                    row.description, row.amount,
                ),
            ).await;
            row
        }
        // The guarded UPDATE matched no row precisely because `review_status` is
        // no longer `needs_review` — i.e. it is already `reviewed` (the only other
        // CHECK-permitted value). Return the row in its terminal state without a
        // second audit entry. Correcting `existing` avoids a re-fetch AND avoids
        // reporting a stale `needs_review` if another approval won the race.
        None => Transaction { review_status: "reviewed".to_string(), ..existing },
    };

    let mut cat_name = None;
    let mut cat_type = None;
    if let Some(cat_id) = transaction.category_id {
        if let Some(r) = sqlx::query("SELECT name, category_type FROM categories WHERE id = $1 AND budget_id = $2")
            .bind(cat_id)
            .bind(budget_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None)
        {
            cat_name = Some(r.get::<String, &str>("name"));
            cat_type = Some(r.get::<String, &str>("category_type"));
        }
    }

    let response = TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
        updated_at: transaction.updated_at,
        excluded_from_budget: transaction.excluded_from_budget,
        source: transaction.source,
        currency: transaction.currency,
        external_account_id: transaction.external_account_id,
        account_label: fetch_account_label(&state.db, transaction.external_account_id).await,
        review_status: transaction.review_status,
        matched_transaction_id: transaction.matched_transaction_id,
        provider_transaction_id: transaction.provider_transaction_id.clone(),
    };

    Ok(Json(response))
}

/// Body of `resolve-match` (#403 P3): how to settle a surfaced possible
/// duplicate. `merge` = "these are the same charge, keep the bank-imported copy"
/// (excludes the Nels-logged twin from budget aggregates); `dismiss` = "keep
/// both" (clears the link). Any other value is a 400.
#[derive(Debug, Deserialize)]
pub struct ResolveMatchPayload {
    pub action: String,
}

/// Resolve a possible-duplicate link (#403 P3) — the non-destructive fix for the
/// budget double-count. Called on the **imported** row (the one carrying
/// `matched_transaction_id`).
///
/// - `merge`: mark the linked Nels-logged twin `excluded_from_budget = true` (so
///   the active budget counts the charge exactly once, via the existing #374
///   aggregate filter) and mark the imported row `reviewed`. The link is kept.
/// - `dismiss`: clear `matched_transaction_id` and mark the imported row
///   `reviewed`. Both twins keep counting; the user judged them distinct.
///
/// NEVER deletes a row. Mirrors `approve_transaction`'s discipline: Owner/Edit
/// only, budget-scoped 404 guard, closed-budget guard, audit on a real state
/// change, and idempotent — re-resolving (or resolving an already-settled /
/// unlinked row) is a 200 no-op with no second audit entry, so a re-sync or a
/// stale optimistic retry never resurfaces or double-acts.
pub async fn resolve_match(
    State(state): State<AppState>,
    Path((budget_id, transaction_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<ResolveMatchPayload>,
) -> Result<Json<TransactionResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to resolve a duplicate".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    let action = payload.action.trim();
    if action != "merge" && action != "dismiss" {
        return Err((StatusCode::BAD_REQUEST, "action must be 'merge' or 'dismiss'".to_string()));
    }

    // Load the imported row, budget-scoped (the cross-budget / unknown-id 404).
    let existing = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1 AND budget_id = $2",
    )
    .bind(transaction_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Transaction not found".to_string()))?;

    // A real state change happened iff at least one guarded UPDATE below touched a
    // row; drives the single audit entry (mirrors approve's audit-on-change).
    let mut changed = false;

    match (action, existing.matched_transaction_id) {
        // No link to resolve (never matched, or already dismissed): 200 no-op.
        (_, None) => {}
        ("merge", Some(matched_id)) => {
            // Exclude the Nels twin so the active budget stops double-counting.
            // Guarded on `= false` so a repeat merge is a clean no-op.
            let excl = sqlx::query(
                "UPDATE transactions SET excluded_from_budget = true \
                 WHERE id = $1 AND budget_id = $2 AND excluded_from_budget = false",
            )
            .bind(matched_id)
            .bind(budget_id)
            .execute(&state.db)
            .await
            .map_err(|e| internal_error(format!("Failed to exclude matched transaction: {}", e)))?;
            // The imported row is the reconciled, kept copy → mark it reviewed.
            let rev = sqlx::query(
                "UPDATE transactions SET review_status = 'reviewed' \
                 WHERE id = $1 AND budget_id = $2 AND review_status <> 'reviewed'",
            )
            .bind(transaction_id)
            .bind(budget_id)
            .execute(&state.db)
            .await
            .map_err(|e| internal_error(format!("Failed to mark imported row reviewed: {}", e)))?;
            changed = excl.rows_affected() > 0 || rev.rows_affected() > 0;
            if changed {
                log_audit(
                    &state.db, budget_id, user_id, "RESOLVE_DUPLICATE",
                    &format!(
                        "Merged possible duplicate: excluded matched transaction '{}' (${:.2}), kept the imported copy",
                        existing.description, existing.amount,
                    ),
                ).await;
            }
        }
        ("dismiss", Some(_)) => {
            // Keep both; drop the link and mark the imported row reviewed.
            let res = sqlx::query(
                "UPDATE transactions SET matched_transaction_id = NULL, review_status = 'reviewed' \
                 WHERE id = $1 AND budget_id = $2 \
                   AND (matched_transaction_id IS NOT NULL OR review_status <> 'reviewed')",
            )
            .bind(transaction_id)
            .bind(budget_id)
            .execute(&state.db)
            .await
            .map_err(|e| internal_error(format!("Failed to dismiss duplicate link: {}", e)))?;
            changed = res.rows_affected() > 0;
            if changed {
                log_audit(
                    &state.db, budget_id, user_id, "RESOLVE_DUPLICATE",
                    &format!(
                        "Dismissed possible duplicate for '{}' (${:.2}); kept both transactions",
                        existing.description, existing.amount,
                    ),
                ).await;
            }
        }
        _ => unreachable!("action validated above"),
    }

    // Re-read the imported row for an accurate response after the mutations.
    let transaction = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1 AND budget_id = $2",
    )
    .bind(transaction_id)
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    let mut cat_name = None;
    let mut cat_type = None;
    if let Some(cat_id) = transaction.category_id {
        if let Some(r) = sqlx::query("SELECT name, category_type FROM categories WHERE id = $1 AND budget_id = $2")
            .bind(cat_id)
            .bind(budget_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None)
        {
            cat_name = Some(r.get::<String, &str>("name"));
            cat_type = Some(r.get::<String, &str>("category_type"));
        }
    }

    let response = TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
        updated_at: transaction.updated_at,
        excluded_from_budget: transaction.excluded_from_budget,
        source: transaction.source,
        currency: transaction.currency,
        external_account_id: transaction.external_account_id,
        account_label: fetch_account_label(&state.db, transaction.external_account_id).await,
        review_status: transaction.review_status,
        matched_transaction_id: transaction.matched_transaction_id,
        provider_transaction_id: transaction.provider_transaction_id.clone(),
    };

    Ok(Json(response))
}

pub async fn delete_transaction(
    State(state): State<AppState>,
    Path((budget_id, transaction_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to delete transactions".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    ensure_not_closed(&state.db, budget_id).await?;

    // Scope the lookup/delete to this budget so a transaction can never be
    // removed via a URL naming a different budget (mirrors delete_category /
    // update_transaction's cross-budget guard).
    let row = sqlx::query("SELECT description, amount, category_id FROM transactions WHERE id = $1 AND budget_id = $2")
        .bind(transaction_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

    // category_id captured so an auto-created category left empty by the delete
    // can be cleaned up afterward (#376).
    let (description, amount, old_category_id): (String, f64, Option<Uuid>) = match row {
        Some(r) => (r.get("description"), r.get("amount"), r.get("category_id")),
        None => return Err((StatusCode::NOT_FOUND, "Transaction not found".to_string())),
    };

    let deleted = sqlx::query("DELETE FROM transactions WHERE id = $1 AND budget_id = $2")
        .bind(transaction_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    // #493. This is #434's guard, extended to the exact-twin handler: the SELECT
    // above and this DELETE are two statements, so the row can disappear between
    // them; `rows_affected() == 0` then means THIS request removed nothing, and
    // reporting 204 there is indistinguishable from a real deletion — and worse,
    // would still write a DELETE_TRANSACTION audit row claiming we deleted a
    // transaction someone else had already removed. Treat it as "not found" and
    // warn, so the race leaves an operator-visible trace. The 404 body is
    // deliberately the SAME text as the existence check's above: the caller's
    // remedy is identical either way, so the warn, not the response, is what
    // separates the two for us.
    //
    // Deliberately NOT closed (no transaction, no `FOR UPDATE`, no
    // `DELETE ... RETURNING description`): the window is exactly these two
    // statements — microseconds wide, and the SAME for every caller. The long
    // human-scale gaps on BOTH surfaces (the chat flow's confirmation modal in
    // `rag.rs`, and the transactions list loading its rows before the user taps
    // delete) are covered by the existence check's pre-existing 404, not by this
    // branch.
    if deleted.rows_affected() == 0 {
        tracing::warn!(
            %budget_id,
            %transaction_id,
            %user_id,
            "delete_transaction matched zero rows after its existence check passed; \
             the transaction was removed concurrently"
        );
        return Err((StatusCode::NOT_FOUND, "Transaction not found".to_string()));
    }

    log_audit(
        &state.db, budget_id, user_id, "DELETE_TRANSACTION",
        &format!("Deleted transaction: {} - ${:.2}", description, amount),
    ).await;

    // The deleted transaction's spend is gone — reconcile the category's limit
    // alerts so a now-under-limit category's stale "exceeded" alert clears (#388).
    let _ = crate::notifications::check_and_notify_limits(
        &state.db, user_id, budget_id, old_category_id,
    ).await;

    cleanup_orphaned_auto_category(&state.db, old_category_id).await;

    Ok(StatusCode::NO_CONTENT)
}

// --- #376: shared category/transaction helpers ---
//
// The single source of truth for chat ADD, chat EDIT, and the finalize endpoint,
// so find-or-create, embedding-insert, and emptied-category cleanup behave
// identically across every entry point.

/// Case-insensitive find-or-create. Returns `(category_id, created)`. On a
/// `UNIQUE(budget_id, name)` race the INSERT loses -> re-SELECT and return
/// `(id, false)`. Never flips an existing category's `auto_created` flag: an
/// existing match is returned untouched regardless of the requested value.
pub(crate) async fn find_or_create_category(
    db: &PgPool,
    budget_id: Uuid,
    name: &str,
    category_type: &str,
    auto_created: bool,
) -> Result<(Uuid, bool), sqlx::Error> {
    if let Some(r) = sqlx::query(
        "SELECT id FROM categories WHERE budget_id = $1 AND LOWER(name) = LOWER($2)",
    )
    .bind(budget_id)
    .bind(name)
    .fetch_optional(db)
    .await?
    {
        return Ok((r.get::<Uuid, &str>("id"), false));
    }

    let cid = Uuid::new_v4();
    match sqlx::query(
        "INSERT INTO categories (id, budget_id, name, category_type, auto_created) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(cid)
    .bind(budget_id)
    .bind(name)
    .bind(category_type)
    .bind(auto_created)
    .fetch_one(db)
    .await
    {
        Ok(r) => Ok((r.get::<Uuid, &str>("id"), true)),
        Err(e) => {
            // Most likely a UNIQUE(budget_id, name) race: someone created it between
            // our SELECT and INSERT. Re-SELECT and return the existing category
            // (created=false) rather than surfacing a spurious error. Log the
            // original error first so a NON-race INSERT failure (check/NOT NULL/
            // connection) isn't masked by a downstream RowNotFound from the re-SELECT.
            tracing::warn!(
                error = ?e, %budget_id, name,
                "find_or_create_category INSERT failed; assuming a name race and re-selecting"
            );
            let r = sqlx::query(
                "SELECT id FROM categories WHERE budget_id = $1 AND LOWER(name) = LOWER($2)",
            )
            .bind(budget_id)
            .bind(name)
            .fetch_one(db)
            .await?;
            Ok((r.get::<Uuid, &str>("id"), false))
        }
    }
}

/// Insert a transaction, generating a best-effort pgvector embedding for the
/// description (#195). Tolerant: with no `GEMINI_API_KEY` (or on any embedding
/// failure) the embedding is stored NULL and the transaction is still created.
pub(crate) async fn insert_transaction_with_embedding(
    db: &PgPool,
    budget_id: Uuid,
    category_id: Option<Uuid>,
    amount: f64,
    description: &str,
    user_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    let embedding = if !api_key.is_empty() {
        crate::rag::get_gemini_embedding(description, &api_key, db, user_id).await
    } else {
        None
    };
    if !api_key.is_empty() && embedding.is_none() {
        // A key is configured but embedding came back None — the transaction is
        // still logged (NULL embedding, backfilled later) but won't be
        // semantically searchable until then. Surface it so a systemic embedding
        // outage is observable rather than only manifesting as missing search hits.
        tracing::warn!(%budget_id, "insert_transaction_with_embedding: embedding unavailable despite GEMINI_API_KEY set; storing NULL");
    }
    let embedding_str = embedding.as_ref().map(|e| crate::rag::vector_to_string(e));

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO transactions (id, budget_id, category_id, amount, description, embedding) \
         VALUES ($1, $2, $3, $4, $5, $6::vector) RETURNING id",
    )
    .bind(id)
    .bind(budget_id)
    .bind(category_id)
    .bind(amount)
    .bind(description)
    .bind(embedding_str)
    .fetch_one(db)
    .await?;

    // Reverse duplicate match (#403 P5) — covers finalize_transaction and the
    // chat add-transaction path, which both insert through this helper.
    crate::duplicate_match::link_duplicate_for_logged(db, budget_id, id).await;
    Ok(id)
}

/// Delete the old category iff it was auto-created AND is now empty. Safe,
/// idempotent no-op otherwise (a `None` id, a user-created category, or one that
/// still holds transactions is left untouched). Failures are logged, not
/// propagated — cleanup is best-effort and must never fail the primary edit.
pub(crate) async fn cleanup_orphaned_auto_category(db: &PgPool, old_category_id: Option<Uuid>) {
    let Some(cid) = old_category_id else {
        return;
    };
    if let Err(e) = sqlx::query(
        "DELETE FROM categories WHERE id = $1 AND auto_created = true \
         AND NOT EXISTS (SELECT 1 FROM transactions WHERE category_id = $1)",
    )
    .bind(cid)
    .execute(db)
    .await
    {
        tracing::warn!("cleanup_orphaned_auto_category failed for {cid}: {e}");
    }
}

pub async fn list_transactions(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<TransactionResponse>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    // LEFT JOIN linked_accounts so imported rows (#403) carry their account's
    // display_name/institution_name/last4 for the provenance badge label. `t.*`
    // already includes the new `source` and `currency` columns.
    let rows = sqlx::query(
        "SELECT t.*, c.name as category_name, c.category_type, \
                la.display_name as account_display_name, \
                la.institution_name as account_institution_name, \
                la.last4 as account_last4 \
         FROM transactions t \
         LEFT JOIN categories c ON t.category_id = c.id \
         LEFT JOIN linked_accounts la ON t.external_account_id = la.id \
         WHERE t.budget_id = $1 \
         ORDER BY t.transaction_date DESC, t.created_at DESC"
    )
    .bind(budget_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let mut resp = Vec::new();
    for r in rows {
        let external_account_id: Option<Uuid> = r.get("external_account_id");
        resp.push(TransactionResponse {
            id: r.get("id"),
            budget_id: r.get("budget_id"),
            category_id: r.get("category_id"),
            category_name: r.get("category_name"),
            category_type: r.get("category_type"),
            amount: r.get("amount"),
            description: r.get("description"),
            transaction_date: r.get("transaction_date"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
            excluded_from_budget: r.get("excluded_from_budget"),
            source: r.get("source"),
            currency: r.get("currency"),
            external_account_id,
            account_label: build_account_label(
                external_account_id.is_some(),
                r.get::<Option<String>, _>("account_display_name").as_deref(),
                r.get::<Option<String>, _>("account_institution_name").as_deref(),
                r.get::<Option<String>, _>("account_last4").as_deref(),
            ),
            review_status: r.get("review_status"),
            matched_transaction_id: r.get("matched_transaction_id"),
            provider_transaction_id: r.get("provider_transaction_id"),
        });
    }

    Ok(Json(resp))
}

// --- COLLABORATION AND SHARING HANDLERS ---

pub async fn share_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<SharePayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only the budget owner can share it".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    validate_permission_level(&payload.permission_level)?;

    let share_id = Uuid::new_v4();
    let email = payload.email.trim().to_lowercase();

    // Verify user doesn't share with themselves
    let user_row = sqlx::query("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;

    let user_email: String = user_row.get("email");
    if user_email == email {
        return Err((StatusCode::BAD_REQUEST, "You cannot share a budget with yourself".to_string()));
    }

    sqlx::query(
        "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) 
         VALUES ($1, $2, $3, $4) 
         ON CONFLICT (budget_id, shared_with_email) 
         DO UPDATE SET permission_level = EXCLUDED.permission_level"
    )
    .bind(share_id)
    .bind(budget_id)
    .bind(&email)
    .bind(&payload.permission_level)
    .execute(&state.db)
    .await
    .map_err(|e| internal_error(format!("Failed to share: {}", e)))?;

    log_audit(&state.db, budget_id, user_id, "SHARE_BUDGET", &format!("Shared budget with {} ({})", email, payload.permission_level)).await;

    Ok(StatusCode::OK)
}

#[derive(Serialize)]
pub struct ShareResponse {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub shared_with_email: String,
    pub permission_level: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list_shares(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<ShareResponse>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let rows = sqlx::query_as::<_, BudgetShare>("SELECT * FROM budget_shares WHERE budget_id = $1 ORDER BY created_at DESC")
        .bind(budget_id)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;

    let resp = rows.into_iter().map(|s| ShareResponse {
        id: s.id,
        budget_id: s.budget_id,
        shared_with_email: s.shared_with_email,
        permission_level: s.permission_level,
        created_at: s.created_at,
    }).collect();

    Ok(Json(resp))
}

pub async fn revoke_share(
    State(state): State<AppState>,
    Path((budget_id, share_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner {
        return Err((StatusCode::FORBIDDEN, "Only owners can revoke shares".to_string()));
    }

    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    // Scope the lookup/delete to this budget so a share can never be revoked
    // from a budget the URL doesn't name.
    let share_row = sqlx::query(
        "SELECT shared_with_email FROM budget_shares WHERE id = $1 AND budget_id = $2",
    )
    .bind(share_id)
    .bind(budget_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let shared_email: String = match share_row {
        Some(r) => r.get("shared_with_email"),
        None => return Err((StatusCode::NOT_FOUND, "Share not found".to_string())),
    };

    let deleted = sqlx::query("DELETE FROM budget_shares WHERE id = $1 AND budget_id = $2")
        .bind(share_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    if deleted.rows_affected() == 0 {
        tracing::warn!(
            %budget_id,
            %share_id,
            %user_id,
            "revoke_share matched zero rows after its existence check passed; \
             the share was revoked concurrently"
        );
        return Err((StatusCode::NOT_FOUND, "Share not found".to_string()));
    }

    log_audit(&state.db, budget_id, user_id, "REVOKE_SHARE", &format!("Revoked share for {}", shared_email)).await;

    Ok(StatusCode::OK)
}

// --- AUDIT HISTORY HANDLERS ---

#[derive(Serialize)]
pub struct AuditLogResponse {
    pub id: Uuid,
    pub budget_id: Option<Uuid>,
    pub user_email: Option<String>,
    pub action: String,
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn list_audit_logs(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<AuditLogResponse>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let rows = sqlx::query(
        "SELECT a.*, u.email as user_email 
         FROM audit_logs a 
         LEFT JOIN users u ON a.user_id = u.id 
         WHERE a.budget_id = $1 
         ORDER BY a.created_at DESC LIMIT 100"
    )
    .bind(budget_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let mut resp = Vec::new();
    for r in rows {
        resp.push(AuditLogResponse {
            id: r.get("id"),
            // Sourced from the path param (not the row) so it is guaranteed Some
            // independent of the SELECT list; the field is Option<Uuid> only to
            // match the now-nullable column.
            budget_id: Some(budget_id),
            user_email: r.get("user_email"),
            action: r.get("action"),
            details: r.get("details"),
            created_at: r.get("created_at"),
        });
    }

    Ok(Json(resp))
}

/// Default age (days) after which audit_logs rows are purged.
const DEFAULT_AUDIT_LOG_RETENTION_DAYS: i64 = 365;

/// Parse an audit-log retention day count. Falls back to the default for
/// missing, unparseable, or out-of-range values. The upper bound (`i32::MAX`)
/// guards the `days as i32` bind in `purge_old_audit_logs`: a larger value would
/// truncate to an incorrect (often negative) day count and delete the wrong
/// rows, so it is rejected.
fn parse_audit_retention(raw: Option<String>) -> i64 {
    match raw {
        Some(s) if !s.trim().is_empty() => match s.trim().parse::<i64>() {
            Ok(d) if (1..=i32::MAX as i64).contains(&d) => d,
            _ => {
                tracing::warn!(
                    value = %s.trim(),
                    "AUDIT_LOG_RETENTION_DAYS is invalid; using default {}",
                    DEFAULT_AUDIT_LOG_RETENTION_DAYS
                );
                DEFAULT_AUDIT_LOG_RETENTION_DAYS
            }
        },
        _ => DEFAULT_AUDIT_LOG_RETENTION_DAYS,
    }
}

/// Read audit-log retention from `AUDIT_LOG_RETENTION_DAYS`.
pub fn audit_log_retention_days() -> i64 {
    parse_audit_retention(std::env::var("AUDIT_LOG_RETENTION_DAYS").ok())
}

/// Batch size for the retention purge. Large enough that steady-state volumes
/// drain in a single batch (so normal operation is one DELETE, as before),
/// small enough that a first-run backlog (#76) is chunked into bounded,
/// lock-friendly DELETEs instead of one large locking statement.
const AUDIT_PURGE_BATCH_SIZE: i64 = 5_000;

/// Delete audit_logs rows older than `days`. Idempotent: once nothing remains
/// beyond the window, re-running deletes zero rows. Returns rows deleted. `days`
/// is clamped to `1..=i32::MAX` before the SQL bind; see
/// [`purge_old_audit_logs_batched`] for the batching and clamp-safety rationale.
pub async fn purge_old_audit_logs(db: &PgPool, days: i64) -> Result<u64, sqlx::Error> {
    purge_old_audit_logs_batched(db, days, AUDIT_PURGE_BATCH_SIZE).await
}

/// Batched core of [`purge_old_audit_logs`]: deletes at most `batch_size` rows
/// per statement, looping until a batch deletes fewer than `batch_size` (i.e.
/// the eligible set is drained). Returns the total deleted across all batches.
///
/// Termination is guaranteed because the eligible set is finite and not
/// replenished during the run: `created_at` is immutable, and a newly-inserted
/// row has `created_at = NOW()`, which never satisfies
/// `created_at < NOW() - interval(>= 1 day)`. Each batch deletes at least one
/// eligible row (until none remain), so the loop drains in a bounded number of
/// iterations.
///
/// The `SELECT id … LIMIT $2` has no `ORDER BY` on purpose: every eligible row
/// is equally deletable, so an arbitrary per-batch subset is fine and each batch
/// makes progress (deleted rows vanish from the next subquery via MVCC). The
/// hourly ticker is spawned per backend process, so a horizontally-scaled API
/// could run two purges concurrently. Without an ordering that is benign at this
/// scale: the worst case is a transient row-lock deadlock that Postgres resolves
/// by aborting one transaction, which surfaces as the caller's `Err` arm (a
/// logged `warn`) and is retried on the next hourly tick — no data is lost and
/// the deleted set is unchanged. (If concurrent purges ever became common, a
/// stable `ORDER BY id` or `FOR UPDATE SKIP LOCKED` would remove even that.)
///
/// `days` and `batch_size` are clamped to `1..=i32::MAX` before the `as i32`
/// binds, so a caller bypassing the config helpers cannot pass a value that
/// truncates to a negative interval (which would invert the predicate) or a
/// non-positive LIMIT.
async fn purge_old_audit_logs_batched(
    db: &PgPool,
    days: i64,
    batch_size: i64,
) -> Result<u64, sqlx::Error> {
    let days = days.clamp(1, i32::MAX as i64) as i32;
    let batch_size = batch_size.clamp(1, i32::MAX as i64) as i32;
    let mut total: u64 = 0;
    loop {
        let result = sqlx::query(
            "DELETE FROM audit_logs WHERE id IN (\
                 SELECT id FROM audit_logs \
                 WHERE created_at < NOW() - make_interval(days => $1::int) \
                 LIMIT $2::int\
             )",
        )
        .bind(days)
        .bind(batch_size)
        .execute(db)
        .await?;
        let n = result.rows_affected();
        total += n;
        if n < batch_size as u64 {
            break;
        }
    }
    Ok(total)
}

/// Auto-renew (#51) every budget whose renewal period has elapsed. Run hourly by
/// the background ticker, mirroring `purge_old_audit_logs`' shape and error
/// handling. Returns the number of budgets renewed.
///
/// A budget is "due" when it has opted into auto-renew, its persisted marker has
/// passed, and it is an ACTIVE TIME-BASED budget — project budgets do not recur
/// (#48), and closed (#48) / archived (#50) budgets are inactive, so all three
/// are excluded by the WHERE filter (and the CHECK constraint already forbids
/// auto_renew on a non-time_based budget).
///
/// Renewal advances `next_renewal_at` to the next period boundary computed from
/// `now()` for the budget's `time_frame`. Because the boundary is derived from
/// the current time, the new marker is always strictly in the future, so:
/// (1) re-running the tick within the same period renews NOTHING (the marker no
/// longer satisfies `<= now()`), making the job idempotent and safe hourly; and
/// (2) a server that missed several boundaries jumps straight to the next future
/// boundary in a single advance. Renewal is in-place: categories and amounts are
/// unchanged, and crossing the boundary makes the just-completed period the
/// "previous period", so rollover (#47/#49) carries automatically on the next
/// computed-on-read. Each renewal writes an `AUTO_RENEW_BUDGET` audit row.
pub async fn renew_due_budgets(db: &PgPool) -> Result<u64, sqlx::Error> {
    let now = Utc::now();

    // Select the due budgets and their timeframe so each new boundary can be
    // computed in Rust (the calendar logic lives in `next_period_boundary`).
    let due = sqlx::query(
        "SELECT id, owner_id, time_frame, name FROM budgets \
         WHERE auto_renew = TRUE \
           AND next_renewal_at IS NOT NULL \
           AND next_renewal_at <= $1 \
           AND budget_type = 'time_based' \
           AND closed_at IS NULL \
           AND archived_at IS NULL",
    )
    .bind(now)
    .fetch_all(db)
    .await?;

    let mut renewed: u64 = 0;
    for r in due {
        let id: Uuid = r.get("id");
        let owner_id: Uuid = r.get("owner_id");
        let time_frame: String = r.get("time_frame");
        let name: String = r.get("name");
        let next = next_period_boundary(&time_frame, now);

        // Guard the UPDATE with the same due predicate so two concurrent ticks
        // (or a racing REST/chat write) cannot double-renew: only the first
        // advance, which still sees the old marker, matches.
        let res = sqlx::query(
            "UPDATE budgets SET next_renewal_at = $1 \
             WHERE id = $2 AND auto_renew = TRUE AND next_renewal_at <= $3 \
               AND budget_type = 'time_based' AND closed_at IS NULL AND archived_at IS NULL",
        )
        .bind(next)
        .bind(id)
        .bind(now)
        .execute(db)
        .await?;

        if res.rows_affected() > 0 {
            renewed += 1;
            // Audit the renewal so it is observable beyond tracing. user_id is
            // the owner (the renewal acts on their behalf).
            log_audit(
                db,
                id,
                owner_id,
                "AUTO_RENEW_BUDGET",
                &format!("Auto-renewed budget '{}' for the next {} period", name, time_frame),
            )
            .await;
        }
    }

    Ok(renewed)
}

/// Enable fund status on category `category_id`, atomically guarded by
/// `is_fund = FALSE` so re-enabling an ALREADY-fund category is a no-op (never
/// touches its balance). `fund_advanced_through` is always reset to the
/// CURRENT period's start (via `current_period_window`) — periods
/// that elapsed while disabled never count. `fund_balance` is handled
/// differently depending on whether this category was EVER a fund before,
/// distinguished by `fund_advanced_through`, which is only ever set once a
/// category first becomes a fund and is never cleared on disable:
/// - `fund_advanced_through IS NULL` (genuinely never was a fund): a FRESH
///   enable, so `fund_balance` resets to 0.
/// - `fund_advanced_through IS NOT NULL` (was a fund, then disabled): a
///   RE-enable, so the existing `fund_balance` is PRESERVED, resuming accrual
///   from where it left off (non-destructive, matching the disable side's
///   documented behavior).
/// Shared by SET_CATEGORY_FUND (`chat_set_category_fund`) and CREATE_CATEGORY
/// (`chat_create_categories`) so the two call sites can't diverge in this logic
/// (#228 spec §5.5). Returns the query's `Result` so each caller can decide how
/// to log/react to a failure per its own context.
///
/// Moved here from `rag.rs` in #426 so the REST write path (`update_category`)
/// can share it with those two chat call sites — rather than growing a third
/// copy of the semantics. Pure move: the SQL is unchanged.
///
/// Takes a generic `Executor` rather than `&AppState` so it composes INSIDE a
/// caller's transaction (`&mut *tx`) as well as against the bare pool
/// (`&state.db`). `update_category` depends on the former: its disable → UPDATE
/// → enable sequence must commit or roll back as one unit.
pub async fn enable_category_fund<'e, E>(
    executor: E,
    category_id: Uuid,
    time_frame: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let (period_start, _) = current_period_window(time_frame, Utc::now());
    sqlx::query(
        "UPDATE categories SET is_fund = TRUE, \
         fund_balance = CASE WHEN fund_advanced_through IS NULL THEN 0 ELSE fund_balance END, \
         fund_advanced_through = $1 \
         WHERE id = $2 AND is_fund = FALSE",
    )
    .bind(period_start)
    .bind(category_id)
    .execute(executor)
    .await
}

/// Disable fund status (#228) on one category, idempotently.
///
/// Extracted in #426 from the inline statement in `chat_set_category_fund`.
/// Non-destructive by design: `fund_balance` and `fund_advanced_through` are
/// PRESERVED so a later re-enable resumes from the accrued balance (see
/// `enable_category_fund`). The `is_fund = TRUE` guard makes a repeated call a
/// no-op (0 rows affected) and keeps a concurrent flip from double-counting.
///
/// Takes a generic `Executor` for the same reason as `enable_category_fund`: so
/// `update_category` can run it inside its transaction and have a later failure
/// roll the disable back.
pub async fn disable_category_fund<'e, E>(
    executor: E,
    category_id: Uuid,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query("UPDATE categories SET is_fund = FALSE WHERE id = $1 AND is_fund = TRUE")
        .bind(category_id)
        .execute(executor)
        .await
}

/// Advance every eligible fund category's `fund_balance` through every
/// COMPLETED period boundary its `fund_advanced_through` marker has not yet
/// crossed (#228). Unlike `renew_due_budgets` (#51), which jumps straight to
/// the next FUTURE boundary in one step, this walks one period increment at a
/// time — funds accumulate PER PERIOD, so downtime spanning several
/// boundaries must apply one `+= (limit - spent)` per crossed period, not a
/// single catch-up jump. Each increment uses the category's CURRENT
/// `category_limit` (there is no historical per-period limit table in this
/// schema — see AGENTS.md "Fund Categories (#228)" for the documented
/// limit-change-fidelity limitation this implies for catch-up after
/// downtime).
///
/// Eligible: `is_fund = TRUE`, has a marker (`fund_advanced_through IS NOT
/// NULL` — always true once enabled), on a `time_based` budget that is
/// neither closed nor archived (mirrors `renew_due_budgets`'s exclusions;
/// project budgets have no periods, so they are excluded by the
/// `budget_type = 'time_based'` filter alone — there is no CHECK forbidding
/// `is_fund` on a project-budget category since project-ness is budget-level,
/// not category-level).
///
/// Idempotent within a period: each per-period UPDATE is guarded by
/// `fund_advanced_through = $old_marker` AND `is_fund = TRUE`, so a
/// concurrent/repeated tick can advance a category at most once per boundary
/// (mirrors `renew_due_budgets`'s guarded UPDATE), and a category that has
/// `is_fund` disabled mid-catch-up stops advancing immediately (its
/// `fund_balance` is frozen, as documented on `Category.fund_balance`).
/// Bounded to 24 iterations per category per tick (two years of missed
/// monthly periods, with margin) to guard against a pathological loop.
///
/// A per-period query error (e.g. a transient DB error) is isolated to the
/// category it occurred on: it is logged and that category's inner loop
/// stops, but whatever periods it already successfully advanced before the
/// error are still counted and audited, and the outer loop continues on to
/// the next category. This is a deliberate widening of error handling beyond
/// `renew_due_budgets` (#51): that job's single-UPDATE-per-item shape can't
/// have a "partial success within one item, then error" case, but this job's
/// nested per-period loop can.
///
/// Returns the number of categories that advanced through at least one
/// period this tick.
pub async fn advance_fund_categories(db: &PgPool) -> Result<u64, sqlx::Error> {
    let now = Utc::now();

    let due = sqlx::query(
        "SELECT c.id, c.budget_id, c.category_limit, c.fund_advanced_through, \
                b.time_frame, b.owner_id, b.name AS budget_name \
         FROM categories c \
         JOIN budgets b ON b.id = c.budget_id \
         WHERE c.is_fund = TRUE \
           AND c.fund_advanced_through IS NOT NULL \
           AND b.budget_type = 'time_based' \
           AND b.closed_at IS NULL \
           AND b.archived_at IS NULL",
    )
    .fetch_all(db)
    .await?;

    let mut advanced_count: u64 = 0;
    for row in due {
        let category_id: Uuid = row.get("id");
        let budget_id: Uuid = row.get("budget_id");
        let raw_category_limit: Option<f64> = row.get("category_limit");
        let category_limit: f64 = raw_category_limit.unwrap_or(0.0);
        let time_frame: String = row.get("time_frame");
        let owner_id: Uuid = row.get("owner_id");
        let budget_name: String = row.get("budget_name");
        let mut marker: DateTime<Utc> = row.get("fund_advanced_through");

        if raw_category_limit.is_none() {
            // A fund category with no limit set is reachable (neither
            // SET_CATEGORY_FUND nor CREATE_CATEGORY requires one) but
            // silently coerced to a $0 limit below, so every dollar spent
            // directly drains the fund balance with zero allotment — make
            // this degradation observable rather than silent.
            tracing::warn!(
                category_id = %category_id,
                budget_id = %budget_id,
                "advance_fund_categories: fund category has no category_limit; treating as $0 for this tick's advancement"
            );
        }

        let mut this_category_advanced = false;
        for _ in 0..24 {
            let boundary = next_period_boundary(&time_frame, marker);
            if boundary > now {
                break;
            }

            let spent: f64 = match sqlx::query_scalar(
                "SELECT COALESCE(SUM(amount), 0)::float8 FROM transactions \
                 WHERE category_id = $1 AND NOT excluded_from_budget \
                   AND transaction_date >= $2 AND transaction_date < $3",
            )
            .bind(category_id)
            .bind(marker)
            .bind(boundary)
            .fetch_one(db)
            .await
            {
                Ok(spent) => spent,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        category_id = %category_id,
                        "advance_fund_categories: spend query failed"
                    );
                    break;
                }
            };

            let delta = category_limit - spent;

            // Guard the UPDATE with the marker equality and `is_fund = TRUE`
            // so a concurrent tick that already advanced this category past
            // `marker`, or a user who disabled `is_fund` on it mid-catch-up,
            // cannot have this increment double-applied or applied to a
            // fund_balance that's now supposed to be frozen.
            let res = match sqlx::query(
                "UPDATE categories SET fund_balance = fund_balance + $1, fund_advanced_through = $2 \
                 WHERE id = $3 AND fund_advanced_through = $4 AND is_fund = TRUE",
            )
            .bind(delta)
            .bind(boundary)
            .bind(category_id)
            .bind(marker)
            .execute(db)
            .await
            {
                Ok(res) => res,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        category_id = %category_id,
                        "advance_fund_categories: update failed"
                    );
                    break;
                }
            };

            if res.rows_affected() == 0 {
                // Another tick already advanced this category past `marker`,
                // or `is_fund` was disabled on it — stop; do not re-derive
                // state from a marker we no longer own.
                break;
            }

            marker = boundary;
            this_category_advanced = true;
        }

        if this_category_advanced {
            advanced_count += 1;
            log_audit(
                db,
                budget_id,
                owner_id,
                "ADVANCE_FUND_CATEGORY",
                &format!(
                    "Advanced fund balance for a category in budget '{}' through {}",
                    budget_name, marker
                ),
            )
            .await;
        }
    }

    Ok(advanced_count)
}

// ---------------------------------------------------------------------------
// Categories HTML table in chat (#176)
//
// A single backend builder aggregates a budget's categories with their
// current-period spending and renders an escaped HTML table. Both entry points
// — the `/categories-list` slash command (via the `categories_table` endpoint)
// and the `LIST_CATEGORIES` chat action (rag.rs) — call the SAME builder so the
// two paths render identically.
// ---------------------------------------------------------------------------

/// One row of the categories table: a category plus its current-period spend.
#[derive(Debug, Clone)]
pub struct CategoryTableRow {
    pub id: Uuid,
    pub name: String,
    pub category_type: String,
    pub category_limit: Option<f64>,
    pub spent: f64,
    /// Fund status and materialized balance (#228). `fund_balance` is always
    /// present (defaults to 0) but only meaningful when `is_fund` is true.
    pub is_fund: bool,
    pub fund_balance: f64,
    /// Per-category rollover toggle (#49) — needed by the chat CATEGORIES
    /// context render (nels#282) alongside the period-windowed `spent`.
    pub rollover_enabled: bool,
    /// Set when this category is a rollup mirror (#52) pointing at another
    /// budget's own total, rather than an ordinary category — needed by the
    /// chat CATEGORIES context render (nels#282) to render the "linked rollup
    /// budget" annotation instead of a bare limit.
    pub linked_budget_id: Option<Uuid>,
}

/// Aggregate categories for `budget_id` with current-period spending.
///
/// Uses the budget's `time_frame` to compute the active period window via
/// `current_period_window`, then sums transactions per category within that
/// window (across all category types). Categories with no transactions in the
/// window appear with `spent = 0` (LEFT JOIN). Ordered by name to give a stable
/// table layout, matching `list_categories`. This is the SOLE period-aware
/// source for per-category current-period spend in the backend — both the
/// LIST_CATEGORIES table and the chat CATEGORIES context (nels#282) read from
/// it, so they cannot drift out of sync again.
pub async fn category_table_rows(
    pool: &sqlx::PgPool,
    budget_id: Uuid,
) -> Result<Vec<CategoryTableRow>, sqlx::Error> {
    let bud = sqlx::query("SELECT time_frame FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(pool)
        .await?;
    let time_frame: String = bud.get("time_frame");
    // Captured once and threaded through to `resolve_linked_source_totals_and_spend`
    // below, rather than each call site independently calling `Utc::now()` — so a
    // response straddling a period boundary can't compute the parent's own window
    // from one instant and a mirrored source's window from a slightly later one.
    let now = Utc::now();
    let (start, end) = current_period_window(&time_frame, now);

    let rows = sqlx::query(
        "SELECT c.id, c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance, \
                c.rollover_enabled, c.linked_budget_id, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM categories c \
         LEFT JOIN transactions t \
           ON c.id = t.category_id \
          AND NOT t.excluded_from_budget \
          AND t.transaction_date >= $2 AND t.transaction_date < $3 \
         WHERE c.budget_id = $1 \
         GROUP BY c.id, c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance, \
                  c.rollover_enabled, c.linked_budget_id \
         ORDER BY c.name ASC",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await?;

    let mut table_rows: Vec<CategoryTableRow> = rows
        .into_iter()
        .map(|r| CategoryTableRow {
            id: r.get("id"),
            name: r.get("name"),
            category_type: r.get("category_type"),
            category_limit: r.get("category_limit"),
            spent: r.get("spent"),
            is_fund: r.get("is_fund"),
            fund_balance: r.get("fund_balance"),
            rollover_enabled: r.get("rollover_enabled"),
            linked_budget_id: r.get("linked_budget_id"),
        })
        .collect();

    // Resolve rollup mirror rows (#52) to their SOURCE budget's own aggregate
    // limit/spend (nels#298), instead of the mirror category's own columns —
    // category_limit is always NULL by construction, and a mirror never
    // receives transactions directly (all activity lives on the source's own
    // categories). Skipped entirely when this budget has no mirror categories,
    // so an ordinary (non-rollup) budget's read pays zero extra queries.
    let linked_ids: Vec<Uuid> = table_rows
        .iter()
        .filter_map(|r| r.linked_budget_id)
        .collect();
    if !linked_ids.is_empty() {
        let (totals, spent) =
            resolve_linked_source_totals_and_spend(pool, &linked_ids, now).await?;
        for row in table_rows.iter_mut() {
            if let Some(src) = row.linked_budget_id {
                row.category_limit = Some(totals.get(&src).copied().unwrap_or(0.0));
                row.spent = spent.get(&src).copied().unwrap_or(0.0);
            }
        }
    }

    Ok(table_rows)
}

/// Batch-resolve each rollup mirror's SOURCE budget to (its own aggregate
/// limit, its own current-window spend) — the same "child's real aggregate
/// position" already surfaced elsewhere for rollups
/// (`computed_budget_total`/`linked_budgets_spent`). Archived sources are
/// excluded from BOTH returned maps (so callers' `unwrap_or(0.0)` resolves
/// them to 0), matching the #52 "archived children don't count on either
/// side" rule already enforced by `computed_budget_total` and
/// `linked_budgets_spent`. One query for the sources' own metadata, one for
/// their mode-aware totals (`computed_budget_totals`), one for their windowed
/// spend (`period_expense_spent_many`) — never N+1 per mirror.
///
/// Returns `sqlx::Error` (not `(StatusCode, String)`, unlike the two helpers
/// it calls) so it composes with `category_table_rows`'s existing signature,
/// which callers (e.g. `rag.rs::resolve_category_balance`) depend on via a
/// direct `.await?` in a `sqlx::Error` context.
///
/// `now` is passed in (not read via a fresh `Utc::now()` here) so a single
/// `category_table_rows` response resolves the parent's own window and every
/// mirrored source's window from the SAME instant — otherwise a response
/// straddling a period boundary could compute the parent's own spend from one
/// side of the boundary and a mirror's spend from the other.
async fn resolve_linked_source_totals_and_spend(
    pool: &sqlx::PgPool,
    linked_ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<
    (
        std::collections::HashMap<Uuid, f64>,
        std::collections::HashMap<Uuid, f64>,
    ),
    sqlx::Error,
> {
    // No de-dup needed: `categories_one_mirror_per_source_idx` (a unique partial
    // index on `(budget_id, linked_budget_id)`) guarantees a single parent can
    // mirror the same source through at most one category, so `linked_ids`
    // (collected from one parent's rows) can never contain a duplicate value.
    let sources = sqlx::query_as::<_, Budget>(
        "SELECT * FROM budgets WHERE id = ANY($1) AND archived_at IS NULL",
    )
    .bind(linked_ids)
    .fetch_all(pool)
    .await?;

    if sources.is_empty() {
        return Ok((
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        ));
    }

    let active_ids: Vec<Uuid> = sources.iter().map(|s| s.id).collect();
    let totals = computed_budget_totals(pool, &active_ids)
        .await
        .map_err(|(_, msg)| sqlx::Error::Protocol(msg))?;

    let windows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>)> = sources
        .iter()
        .map(|src| {
            let (start, end) = source_spend_window(
                &src.budget_type,
                &src.time_frame,
                src.created_at,
                src.closed_at,
                now,
            );
            (src.id, start, end)
        })
        .collect();
    let spent = period_expense_spent_many(pool, &windows)
        .await
        .map_err(|(_, msg)| sqlx::Error::Protocol(msg))?;

    Ok((totals, spent))
}

/// HTML-escape a string for safe embedding in table cell text. Covers the five
/// characters that are dangerous in HTML body/attribute context so a category
/// name like `<script>` can never inject markup through the `{@html}` render
/// path on the frontend.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Format a currency amount as a whole-dollar string with thousands separators
/// and no decimal digits (e.g. `$1,000`, `-$1,000`, `$0`). Rounds to the nearest
/// dollar (half away from zero); the sign precedes the `$`, grouping applies to
/// the magnitude.
pub(crate) fn money(v: f64) -> String {
    let rounded = v.round();
    let neg = rounded < 0.0;
    let digits = format!("{}", rounded.abs() as i64);
    let len = digits.len();
    let mut grouped = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{}${}", if neg { "-" } else { "" }, grouped)
}

/// Build an escaped HTML table of categories with limit, spend, and remaining,
/// plus a totals row.
///
/// All dynamic text (category names) is HTML-escaped. A category with no
/// limit renders "—" for both its limit and remaining cells (spending still
/// shows). The totals row sums limit/spent/remaining over EXPENSE categories
/// only (`remaining = limit - spent`), mirroring the insights spend-vs-budget
/// rollup which filters to expense categories; income/savings categories are
/// still listed but excluded from the totals because "remaining" is not
/// meaningful for them.
///
/// Categories are grouped into Income -> Savings -> Expense sections (#239),
/// each preceded by a per-group 4-column `<tr class="cat-group">` header row
/// of `<th scope="col">` cells (#299: reflects each group's own column
/// semantics, e.g. Income's "Target"/"Received" rather than Expense's
/// "Limit"/"Spent"; `scope="col"` lets assistive tech associate the section's
/// data rows with these headers even though — unlike the flat/single-type
/// case below — there is no wrapping `<thead>`) — but ONLY when `rows`
/// contains more than one distinct `category_type`. A single-type
/// budget keeps the flat, generic-header list from before this change — an
/// intentional, documented scope boundary (see
/// docs/superpowers/specs/2026-07-04-categories-table-group-headings-design.md),
/// not an oversight. A group with no rows never emits its header. `rows` is
/// expected pre-sorted alphabetically by name (as returned by
/// `category_table_rows`'s `ORDER BY c.name ASC`); partitioning that
/// already-sorted slice preserves alphabetical order within each rendered
/// group without re-sorting. A row whose `category_type` is outside the
/// three recognized values is silently excluded from the grouped body — this
/// should be unreachable given `ALLOWED_CATEGORY_TYPES` write-time
/// validation, and mirrors this function's own totals loop, which already
/// only ever matched the exact string `"expense"`.
/// The three category-table groups in display order (Income -> Savings ->
/// Expense), paired with each group's own column headings: `(type_key,
/// group_heading, amount_label, activity_label)` — the 4th column is always
/// "Remaining" (#299). Keys MUST stay exactly `ALLOWED_CATEGORY_TYPES` (in
/// any order) — pinned by the `grouped_types_match_allowed_category_types`
/// test — so a future addition to `ALLOWED_CATEGORY_TYPES` can't silently
/// reintroduce the "unrecognized type is silently dropped" case in
/// `build_categories_table_html` without a test failure calling it out.
const CATEGORY_TABLE_GROUPS: [(&str, &str, &str, &str); 3] = [
    ("income", "Income", "Target", "Received"),
    ("savings", "Savings", "Target", "Invested"),
    ("expense", "Expenses", "Limit", "Spent"),
];

pub fn build_categories_table_html(rows: &[CategoryTableRow]) -> String {
    let distinct_types: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.category_type.as_str()).collect();
    let grouped = distinct_types.len() > 1;

    let mut out = String::from("<table class=\"cat-table\">");
    if !grouped {
        out.push_str(
            "<thead><tr>\
             <th>Category</th><th>Limit</th><th>Spent</th><th>Remaining</th>\
             </tr></thead>",
        );
    }
    out.push_str("<tbody>");

    // Totals accumulate over ALL rows regardless of grouping (expense only).
    // A fund row's contribution is its EFFECTIVE limit (limit + fund_balance)
    // — the true available headroom this period — not the bare category_limit
    // (#228). For a non-fund row these are numerically identical.
    let (mut tot_limit, mut tot_spent) = (0.0_f64, 0.0_f64);
    for r in rows {
        if r.category_type == "expense" {
            let base = r.category_limit.unwrap_or(0.0);
            tot_limit += fund_effective_limit(r.is_fund, base, r.fund_balance);
            tot_spent += r.spent;
        }
    }

    if grouped {
        for (type_key, group_label, amount_label, activity_label) in CATEGORY_TABLE_GROUPS {
            let group_rows: Vec<&CategoryTableRow> =
                rows.iter().filter(|r| r.category_type == type_key).collect();
            if group_rows.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "<tr class=\"cat-group\">\
                 <th scope=\"col\">{}</th><th scope=\"col\">{}</th>\
                 <th scope=\"col\">{}</th><th scope=\"col\">Remaining</th></tr>",
                group_label, amount_label, activity_label
            ));
            for r in group_rows {
                push_category_row(&mut out, r, amount_label, activity_label);
            }
        }
        // A row whose category_type matched none of CATEGORY_TABLE_GROUPS
        // never rendered above. This should be unreachable
        // (ALLOWED_CATEGORY_TYPES is enforced at every write path — see
        // validate_category_type / resolve_category_type), but if the
        // invariant is ever violated (e.g. a direct-SQL migration/backfill,
        // or CATEGORY_TABLE_GROUPS drifting out of sync with
        // ALLOWED_CATEGORY_TYPES) a user's category would otherwise vanish
        // from their own table with zero signal. Warn loudly instead,
        // matching this file's existing convention of logging defensive/
        // should-never-happen conditions rather than failing silently (e.g.
        // the budget-row-missing `tracing::error!` in computed_budget_total).
        for r in rows {
            if !CATEGORY_TABLE_GROUPS
                .iter()
                .any(|(type_key, _, _, _)| r.category_type == *type_key)
            {
                tracing::warn!(
                    category_name = %r.name,
                    category_type = %r.category_type,
                    "build_categories_table_html: row has an unrecognized category_type and was dropped from the grouped table"
                );
            }
        }
    } else {
        for r in rows {
            push_category_row(&mut out, r, "Limit", "Spent");
        }
    }

    out.push_str(&format!(
        "</tbody><tfoot><tr>\
         <td data-label=\"Category\">Totals (expense)</td>\
         <td data-label=\"Limit\">{}</td>\
         <td data-label=\"Spent\">{}</td>\
         <td data-label=\"Remaining\">{}</td>\
         </tr></tfoot></table>",
        money(tot_limit),
        money(tot_spent),
        money(tot_limit - tot_spent)
    ));
    out
}

/// Render one category's `<tr>` for `build_categories_table_html`. Shared by
/// both the grouped and flat rendering paths so row markup is identical
/// either way. `amount_label`/`activity_label` set the 2nd/3rd cell's
/// `data-label` (#299 — e.g. "Target"/"Received" for an Income row,
/// "Limit"/"Spent" for an Expense row or the flat/ungrouped path) so the
/// responsive mobile layout's per-cell label always matches the heading the
/// cell falls under. The 1st cell's `data-label` stays "Category" (inert on
/// mobile regardless — the first cell's `::before` is unconditionally
/// suppressed by existing CSS) and the 4th stays "Remaining" (#181).
fn push_category_row(
    out: &mut String,
    r: &CategoryTableRow,
    amount_label: &str,
    activity_label: &str,
) {
    let effective_limit = r.category_limit.map(|l| fund_effective_limit(r.is_fund, l, r.fund_balance));
    let (limit_cell, remaining_cell) = match effective_limit {
        Some(eff) => (money(eff), money(eff - r.spent)),
        None => ("—".to_string(), "—".to_string()),
    };
    // Reuse EXISTING styled daisyUI/Tailwind classes (see the Styling note
    // above) — badge-warning for the fund indicator, text-error for a
    // negative (deficit) balance/remaining. Never invent a bespoke class name
    // here; it would render completely unstyled.
    let name_cell = if r.is_fund {
        let balance_class = if r.fund_balance < 0.0 { " text-error" } else { "" };
        format!(
            "{} <span class=\"badge badge-warning badge-sm\">Fund</span><br>\
             <small class=\"{}\">Balance: {}</small>",
            html_escape(&r.name),
            balance_class.trim_start(),
            money(r.fund_balance),
        )
    } else {
        html_escape(&r.name)
    };
    // Deficit styling is scoped to fund categories only: an ordinary (non-fund)
    // category could already go overspent before this feature, and that case
    // never carried a `class` attribute — widening it to every overspent row
    // would be an unreviewed scope expansion beyond "fund categories".
    let remaining_class = match effective_limit {
        Some(eff) if r.is_fund && eff - r.spent < 0.0 => " class=\"text-error\"",
        _ => "",
    };
    out.push_str(&format!(
        "<tr><td data-label=\"Category\">{}</td>\
         <td data-label=\"{}\">{}</td>\
         <td data-label=\"{}\">{}</td>\
         <td data-label=\"Remaining\"{}>{}</td></tr>",
        name_cell,
        amount_label, limit_cell,
        activity_label, money(r.spent),
        remaining_class,
        remaining_cell
    ));
}

/// Response for `GET /budgets/:id/categories-table`. `html` is `None` when the
/// budget has no categories, so the caller can render a friendly empty message
/// instead of an empty table.
#[derive(Serialize)]
pub struct CategoriesTableResponse {
    pub html: Option<String>,
}

/// `GET /budgets/:id/categories-table` — render the active categories of a
/// budget as an escaped HTML table. Read-only; gated at View permission like
/// `list_categories`.
pub async fn categories_table(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<CategoriesTableResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let rows = category_table_rows(&state.db, budget_id)
        .await
        .map_err(internal_error)?;
    let html = if rows.is_empty() {
        None
    } else {
        Some(build_categories_table_html(&rows))
    };
    Ok(Json(CategoriesTableResponse { html }))
}

/// One row of the categories view (#426) — everything the rebuilt
/// CategoriesView.svelte needs to render a category card without doing any
/// budgeting arithmetic of its own.
#[derive(Serialize)]
pub struct CategoryViewRow {
    pub id: Uuid,
    pub name: String,
    pub category_type: String,
    /// Mirror-resolved for a #52 linked category (the source budget's own
    /// expense total), otherwise the category's own limit. NULL = no limit.
    pub category_limit: Option<f64>,
    /// Current-period spend, mirror-resolved, excluding excluded_from_budget.
    pub spent: f64,
    pub is_fund: bool,
    pub fund_balance: f64,
    pub rollover_enabled: bool,
    pub linked_budget_id: Option<Uuid>,
    /// Display name of the source budget for a #52 mirror; None otherwise.
    pub linked_budget_name: Option<String>,
    pub is_mirror: bool,
    /// #49 carry, or the fund balance for a fund — never both.
    pub carried_amount: f64,
    pub prev_period_spent: f64,
    /// base + carried. NULL when the category has no limit, so the frontend
    /// can render "no limit" rather than a meter with a zero denominator.
    pub effective_limit: Option<f64>,
}

/// Envelope for `GET /budgets/:id/categories-view` (#426, #431).
#[derive(Serialize)]
pub struct CategoriesViewResponse {
    pub categories: Vec<CategoryViewRow>,
    /// The budget's own currency (#431). NEVER falls back — the column is NOT
    /// NULL DEFAULT 'USD'.
    pub currency: String,
    /// True when the budget has transactions in more than one currency.
    pub currency_is_mixed: bool,
    pub time_frame: String,
    pub closed_at: Option<DateTime<Utc>>,
    /// "owner" | "edit" | "view" — lets the client mirror the server's guards.
    pub permission_level: String,
    /// #47 budget-level master switch. A category's own `rollover_enabled` is
    /// inert without it, so the client must gate the rollover badge and chip
    /// on this rather than on the per-category flag alone.
    pub rollover_enabled: bool,
    /// #48 budget type. A `project` budget never carries, so rollover is n/a.
    pub budget_type: String,
}

/// `GET /budgets/:id/categories-view` (#426) — the JSON backing the rebuilt
/// categories page. Composes the existing period-windowed, mirror-resolved
/// `category_table_rows` with the #49 carry that read path does not compute,
/// via the shared `category_carry_for`.
pub async fn categories_view(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<CategoriesViewResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let rows = category_table_rows(&state.db, budget_id)
        .await
        .map_err(internal_error)?;

    let brow = sqlx::query(
        "SELECT rollover_enabled, budget_type, time_frame, closed_at, currency FROM budgets WHERE id = $1",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
    let budget_rollover: bool = brow.get("rollover_enabled");
    let budget_type: String = brow.get("budget_type");
    let time_frame: String = brow.get("time_frame");
    let closed_at: Option<DateTime<Utc>> = brow.get("closed_at");
    let currency: String = brow.get("currency");

    // Previous-period spend for the #49 carry. ONE (budget_id, start, end)
    // tuple in, a CATEGORY-keyed map out. Skipped entirely when no category can
    // carry — the budget master switch is off or this is a project budget —
    // mirroring `list_categories`: `category_carry_for` already returns
    // (0.0, 0.0) in those cases, so an empty map leaves every row's
    // `prev_period_spent` and `carried_amount` at 0 while avoiding the query.
    let prev_spent_map = if budget_rollover && budget_type != "project" {
        let (prev_start, prev_end) = previous_period_window(&time_frame, Utc::now());
        period_category_expense_spent_many(&state.db, &[(budget_id, prev_start, prev_end)]).await?
    } else {
        std::collections::HashMap::new()
    };

    // Source names for #52 mirrors — one batched query, skipped entirely when
    // this budget has no mirrors.
    let linked_ids: Vec<Uuid> = rows.iter().filter_map(|r| r.linked_budget_id).collect();
    let mut names: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    if !linked_ids.is_empty() {
        for r in sqlx::query("SELECT id, name FROM budgets WHERE id = ANY($1)")
            .bind(&linked_ids)
            .fetch_all(&state.db)
            .await
            .map_err(internal_error)?
        {
            names.insert(r.get("id"), r.get("name"));
        }
    }

    // Determine whether the budget has transactions in multiple currencies.
    // Filter on `transactions.budget_id` DIRECTLY — do NOT join through
    // `categories`. `transactions.category_id` is nullable, and `currency` is
    // populated only by bank sync, whose rows arrive UNCATEGORIZED pending
    // review. Joining through categories would drop exactly the population
    // that carries a currency.
    let currency_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT t.currency) FROM transactions t \
         WHERE t.budget_id = $1 AND t.currency IS NOT NULL",
    )
    .bind(budget_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
    let currency_is_mixed = currency_count > 1;

    let categories = rows
        .into_iter()
        .map(|r| {
            let is_linked = r.linked_budget_id.is_some();
            let is_expense = r.category_type == "expense";
            let base = r.category_limit.unwrap_or(0.0);
            let prev_spent = prev_spent_map.get(&r.id).copied().unwrap_or(0.0);
            let (prev, carried) = category_carry_for(
                is_expense,
                is_linked,
                r.is_fund,
                r.fund_balance,
                budget_rollover,
                r.rollover_enabled,
                &budget_type,
                base,
                prev_spent,
            );
            CategoryViewRow {
                id: r.id,
                name: r.name,
                category_type: r.category_type,
                // A limit-less category has no meter denominator, so
                // effective_limit stays None even for a fund with a balance.
                effective_limit: r.category_limit.map(|l| l + carried),
                category_limit: r.category_limit,
                spent: r.spent,
                is_fund: r.is_fund,
                fund_balance: r.fund_balance,
                rollover_enabled: r.rollover_enabled,
                linked_budget_name: r.linked_budget_id.and_then(|id| names.get(&id).cloned()),
                linked_budget_id: r.linked_budget_id,
                is_mirror: is_linked,
                carried_amount: carried,
                prev_period_spent: prev,
            }
        })
        .collect();

    // There is NO `permission_label` helper in this codebase — the existing
    // sites all match inline.
    let permission_level = match perm {
        Permission::Owner => "owner".to_string(),
        Permission::Edit => "edit".to_string(),
        _ => "view".to_string(),
    };

    Ok(Json(CategoriesViewResponse {
        categories,
        currency,
        currency_is_mixed,
        time_frame,
        closed_at,
        permission_level,
        // The client gates its rollover affordances on these two exactly as the
        // chat CATEGORIES context does: a per-category `rollover_enabled` is
        // inert when the master switch is off or the budget is `project`.
        rollover_enabled: budget_rollover,
        budget_type,
    }))
}

#[cfg(test)]
mod categories_table_tests {
    use super::*;

    fn row(name: &str, ty: &str, limit: Option<f64>, spent: f64) -> CategoryTableRow {
        CategoryTableRow {
            id: Uuid::new_v4(),
            name: name.into(),
            category_type: ty.into(),
            category_limit: limit,
            spent,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        }
    }

    #[test]
    fn money_formats_whole_dollars_with_separators() {
        assert_eq!(money(1000.0), "$1,000");
        assert_eq!(money(0.0), "$0");
        assert_eq!(money(0.5), "$1"); // round half away from zero
        assert_eq!(money(-1000.0), "-$1,000");
        assert_eq!(money(999999.0), "$999,999");
        assert_eq!(money(1_000_000.0), "$1,000,000");
        assert_eq!(money(5.0), "$5");
        assert_eq!(money(-0.4), "$0"); // -0.0 must not produce a spurious -$0
        assert_eq!(money(-0.5), "-$1"); // negative, half away from zero
    }

    #[test]
    fn escapes_category_names() {
        let html = build_categories_table_html(&[row(
            "<script>alert('x')</script>",
            "expense",
            Some(10.0),
            1.0,
        )]);
        // The raw injection must not appear; the escaped form must.
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&#39;"));
    }

    #[test]
    fn escapes_category_names_on_fund_row() {
        let html = build_categories_table_html(&[CategoryTableRow {
            id: Uuid::new_v4(),
            name: "<script>alert('x')</script>".into(),
            category_type: "expense".into(),
            category_limit: Some(10.0),
            spent: 1.0,
            is_fund: true,
            fund_balance: 50.0,
            rollover_enabled: false,
            linked_budget_id: None,
        }]);
        // The raw injection must not appear; the escaped form must, even on
        // the is_fund branch of push_category_row (which formats the name
        // alongside the "Fund" badge and balance rather than returning it
        // unadorned).
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&#39;"));
    }

    #[test]
    fn null_limit_renders_dash() {
        let html = build_categories_table_html(&[row("Misc", "expense", None, 5.0)]);
        assert!(html.contains("—"));
        // Spending still displays even with no limit.
        assert!(html.contains("$5"));
    }

    #[test]
    fn build_categories_table_html_renders_resolved_mirror_limit_and_spent() {
        // Simulates what category_table_rows now produces for a mirror row after
        // resolution (nels#298): category_limit/spent carry the SOURCE's real
        // aggregate position, not "—"/$0.
        let mut mirror = row("Mirror", "expense", Some(300.0), 120.0);
        mirror.linked_budget_id = Some(Uuid::new_v4());
        let html = build_categories_table_html(&[mirror]);
        assert!(html.contains("$300"), "resolved limit renders as a real number: {html}");
        assert!(html.contains("$120"), "resolved spend renders as a real number: {html}");
        assert!(html.contains("$180"), "remaining = limit - spent: {html}");
    }

    /// Extract the `<tfoot>…</tfoot>` segment so assertions target the totals
    /// row specifically, not a data row that happens to share a value.
    fn tfoot(html: &str) -> String {
        let start = html.find("<tfoot>").expect("tfoot present");
        let end = html.find("</tfoot>").expect("tfoot close present");
        html[start..end].to_string()
    }

    #[test]
    fn totals_row_sums_expense_only() {
        // Two expense categories whose totals ($1300 limit / $1150 spent /
        // $150 remaining) deliberately do NOT match any single row's cells, so
        // the assertions can only pass if the tfoot computation is correct.
        // Income and savings rows must be excluded from every total.
        let rows = vec![
            row("Rent", "expense", Some(1000.0), 900.0),
            row("Food", "expense", Some(300.0), 250.0),
            row("Salary", "income", Some(5000.0), 4000.0),
            row("Emergency", "savings", Some(2000.0), 100.0),
        ];
        let foot = tfoot(&build_categories_table_html(&rows));
        assert!(foot.contains("$1,300"), "expense limit total in tfoot");
        assert!(foot.contains("$1,150"), "expense spent total in tfoot");
        assert!(foot.contains("$150"), "remaining = 1300 - 1150 in tfoot");
        // Non-expense limits/spend must never reach the totals.
        assert!(!foot.contains("$5,000"));
        assert!(!foot.contains("$7,000")); // 5000 income + 2000 savings
        assert!(!foot.contains("$2,000"));
    }

    #[test]
    fn null_limit_excluded_from_expense_totals() {
        // An expense category with no limit contributes its spend but 0 to the
        // limit total (limit unwrap_or(0.0)); remaining still nets correctly.
        let rows = vec![
            row("Rent", "expense", Some(1000.0), 900.0),
            row("Misc", "expense", None, 40.0),
        ];
        let foot = tfoot(&build_categories_table_html(&rows));
        assert!(foot.contains("$1,000")); // only Rent's limit
        assert!(foot.contains("$940")); // 900 + 40 spent
        assert!(foot.contains("$60")); // 1000 - 940
    }

    #[test]
    fn overspend_renders_negative_remaining_with_sign_before_dollar() {
        // spent > limit -> remaining = limit - spent is negative. The sign must
        // precede the `$` (e.g. -$50, not $-50), and the tfoot total must also go
        // negative when expenses overspend in aggregate.
        let rows = vec![row("Rent", "expense", Some(100.0), 150.0)];
        let html = build_categories_table_html(&rows);
        // Exact match on the body row's Remaining <td> (no `class` attribute at
        // all) — a plain `html.contains("data-label=\"Remaining\">-$50</td>")`
        // would ALSO match if a `class` were present after `Remaining"` (it
        // isn't, since the attribute would land between the two `>` here), but
        // this pins the non-fund overspend invariant precisely: this is a
        // non-fund row (`row()` sets `is_fund: false`), so its Remaining cell
        // must render with NO class attribute, even though it is overspent —
        // deficit styling is scoped to fund categories only (#228).
        assert!(html.contains("<td data-label=\"Remaining\">-$50</td>"));
        let foot = tfoot(&html);
        assert!(foot.contains("data-label=\"Remaining\">-$50</td>"));
    }

    #[test]
    fn fund_overspend_gets_text_error_class_non_fund_does_not() {
        // Locks in the fund-only scoping: a fund row's deficit Remaining <td>
        // gets `class="text-error"`, while an otherwise-identical non-fund
        // deficit row's Remaining <td> carries no class attribute at all.
        let fund_row = CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Car Repair Fund".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            spent: 150.0,
            is_fund: true,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        };
        let non_fund_row = row("Rent", "expense", Some(100.0), 150.0);

        let fund_html = build_categories_table_html(&[fund_row]);
        assert!(
            fund_html.contains("<td data-label=\"Remaining\" class=\"text-error\">-$50</td>"),
            "fund category's deficit Remaining <td> must carry class=\"text-error\": {fund_html}"
        );

        let non_fund_html = build_categories_table_html(&[non_fund_row]);
        assert!(
            non_fund_html.contains("<td data-label=\"Remaining\">-$50</td>"),
            "non-fund category's deficit Remaining <td> must carry NO class attribute: {non_fund_html}"
        );
        assert!(!non_fund_html.contains("text-error"));
    }

    #[test]
    fn large_value_renders_multi_comma_in_cell() {
        let html = build_categories_table_html(&[row("Mortgage", "expense", Some(1_234_567.0), 1_000_000.0)]);
        assert!(html.contains("data-label=\"Limit\">$1,234,567</td>"));
        assert!(html.contains("data-label=\"Spent\">$1,000,000</td>"));
    }

    #[test]
    fn cells_carry_data_label_paired_with_value() {
        // The data-label attributes are the contract the responsive frontend CSS
        // depends on: at narrow widths the <thead> is hidden and each cell shows
        // its data-label instead (#181). A dropped or mis-paired label is
        // invisible on desktop (the header row still shows) but breaks the mobile
        // stacked layout, so it must be guarded here — there is no frontend test.
        let html = build_categories_table_html(&[row("Rent", "expense", Some(1000.0), 900.0)]);
        // Every column label is present AND paired with the correct value, in
        // order, so a label/value swap across the four <td>s is caught.
        assert!(html.contains("data-label=\"Category\">Rent</td>"));
        assert!(html.contains("data-label=\"Limit\">$1,000</td>"));
        assert!(html.contains("data-label=\"Spent\">$900</td>"));
        assert!(html.contains("data-label=\"Remaining\">$100</td>"));
        // The Type column has been removed (#185): no header, no cells.
        assert!(!html.contains("<th>Type</th>"));
        assert!(!html.contains("data-label=\"Type\""));
        // The tfoot totals row carries the same labels so the mobile "Totals"
        // card is labeled too.
        let foot = tfoot(&html);
        assert!(foot.contains("data-label=\"Category\">Totals (expense)</td>"));
        assert!(foot.contains("data-label=\"Limit\">$1,000</td>"));
        assert!(foot.contains("data-label=\"Spent\">$900</td>"));
        assert!(foot.contains("data-label=\"Remaining\">$100</td>"));
    }

    #[test]
    fn categories_table_renders_fund_balance_and_effective_limit() {
        let rows = vec![CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Groceries".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            spent: 200.0,
            is_fund: true,
            fund_balance: 80.0,
            rollover_enabled: false,
            linked_budget_id: None,
        }];
        let html = build_categories_table_html(&rows);
        // Effective limit = 100 + 80 = 180; remaining = 180 - 200 = -20 (a
        // deficit, must render with the money() helper's "-$" sign).
        assert!(html.contains("$180"), "shows the effective limit, not the bare category_limit");
        assert!(html.contains("-$20"), "remaining computed against the effective limit, signed");
        assert!(html.contains("Fund"), "a fund indicator is present in the row markup");
        // Styling must reuse EXISTING daisyUI/Tailwind utility classes already
        // scanned from frontend source (e.g. frontend/src/lib/BudgetsView.svelte
        // uses "badge badge-warning badge-sm" and "text-error") — a novel class
        // name invented only in backend-generated HTML gets NO CSS, since
        // Tailwind's JIT scanner only emits rules for classes it finds in
        // frontend source files. See Step 4 below for the exact classes to use.
        assert!(html.contains("badge-warning"), "fund badge uses an existing, styled daisyUI badge class");
        assert!(html.contains("text-error"), "deficit remaining uses the existing text-error utility class");
    }

    #[test]
    fn categories_table_non_fund_row_unaffected() {
        let rows = vec![CategoryTableRow {
            id: Uuid::new_v4(),
            name: "Rent".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(1000.0),
            spent: 400.0,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        }];
        let html = build_categories_table_html(&rows);
        assert!(html.contains("$1,000"), "non-fund limit is the bare category_limit");
        assert!(html.contains("$600"), "non-fund remaining is limit - spent");
    }

    #[test]
    fn empty_rows_still_produce_well_formed_table() {
        // The builder always emits a well-formed table; the None-on-empty
        // decision lives in the CALLER (the handler / chat arm), not here.
        let html = build_categories_table_html(&[]);
        assert!(html.starts_with("<table"));
        assert!(html.ends_with("</table>"));
        assert!(html.contains("<tfoot>"));
    }

    #[test]
    fn mixed_type_groups_render_in_income_savings_expense_order() {
        // Rows arrive GLOBALLY alphabetical-by-name (mirrors the real input:
        // category_table_rows sorts by name only, so types interleave — this
        // fixture interleaves savings/income/savings/expense/income/expense
        // on purpose). Names are chosen so alphabetical-by-name does NOT
        // already put rows in Income/Savings/Expense order overall, so the
        // header-order assertions below can only be explained by real
        // grouping, not incidental name order. Because the implementation
        // partitions this already-sorted slice by filtering (order-preserving,
        // no re-sort), each group's rows must appear in the SAME relative
        // order they have in this fixture — so the fixture itself must be
        // alphabetical for the intra-group order assertions to hold.
        let rows = vec![
            row("Anchor Fund", "savings", Some(500.0), 0.0),
            row("Bonus", "income", Some(1000.0), 0.0),
            row("Car Repair Fund", "savings", Some(200.0), 50.0),
            row("Groceries", "expense", Some(300.0), 100.0),
            row("Wages", "income", Some(4000.0), 4000.0),
            row("Zesty Snacks", "expense", Some(50.0), 10.0),
        ];
        let html = build_categories_table_html(&rows);

        // #299: a grouped (multi-type) table has NO top-level shared <thead>
        // at all — each group's own 4-column <th> header row (inside
        // <tbody>) replaces it entirely.
        assert!(!html.contains("<thead>"), "grouped table must have no table-wide thead: {html}");

        // Exactly one 4-column header per present type, in Income -> Savings
        // -> Expense order, using each group's own column semantics (#299).
        let income_hdr = html
            .find("<tr class=\"cat-group\"><th scope=\"col\">Income</th><th scope=\"col\">Target</th><th scope=\"col\">Received</th><th scope=\"col\">Remaining</th></tr>")
            .expect("Income header present");
        let savings_hdr = html
            .find("<tr class=\"cat-group\"><th scope=\"col\">Savings</th><th scope=\"col\">Target</th><th scope=\"col\">Invested</th><th scope=\"col\">Remaining</th></tr>")
            .expect("Savings header present");
        let expense_hdr = html
            .find("<tr class=\"cat-group\"><th scope=\"col\">Expenses</th><th scope=\"col\">Limit</th><th scope=\"col\">Spent</th><th scope=\"col\">Remaining</th></tr>")
            .expect("Expense header present");
        assert!(income_hdr < savings_hdr, "Income header must precede Savings header");
        assert!(savings_hdr < expense_hdr, "Savings header must precede Expense header");

        // Within each group, rows stay alphabetical by name (Bonus < Wages;
        // Anchor Fund < Car Repair Fund; Groceries < Zesty Snacks) and every
        // row in a group appears after that group's header and before the
        // next group's header.
        let bonus = html.find("data-label=\"Category\">Bonus</td>").expect("Bonus row present");
        let wages = html.find("data-label=\"Category\">Wages</td>").expect("Wages row present");
        assert!(income_hdr < bonus, "Bonus must come after the Income header");
        assert!(bonus < wages, "Bonus (alphabetically first) must precede Wages within Income group");
        assert!(wages < savings_hdr, "Wages (last Income row) must precede the Savings header");

        let anchor = html.find("data-label=\"Category\">Anchor Fund</td>").expect("Anchor Fund row present");
        let car_repair = html.find("data-label=\"Category\">Car Repair Fund</td>").expect("Car Repair Fund row present");
        assert!(savings_hdr < anchor, "Anchor Fund must come after the Savings header");
        assert!(anchor < car_repair, "Anchor Fund (alphabetically first) must precede Car Repair Fund within Savings group");
        assert!(car_repair < expense_hdr, "Car Repair Fund (last Savings row) must precede the Expense header");

        let groceries = html.find("data-label=\"Category\">Groceries</td>").expect("Groceries row present");
        let zesty = html.find("data-label=\"Category\">Zesty Snacks</td>").expect("Zesty Snacks row present");
        assert!(expense_hdr < groceries, "Groceries must come after the Expense header");
        assert!(groceries < zesty, "Groceries (alphabetically first) must precede Zesty Snacks within Expense group");

        // #299: each row's 2nd/3rd data-label matches its OWN group's
        // amount/activity columns, not a shared "Limit"/"Spent" — this is
        // the contract the mobile stacked layout depends on.
        assert!(html.contains("data-label=\"Target\">$1,000</td>"), "Bonus's limit cell is labeled Target: {html}");
        assert!(html.contains("data-label=\"Received\">$0</td>"), "Bonus's spent cell is labeled Received: {html}");
        assert!(html.contains("data-label=\"Target\">$500</td>"), "Anchor Fund's limit cell is labeled Target: {html}");
        assert!(html.contains("data-label=\"Invested\">$0</td>"), "Anchor Fund's spent cell is labeled Invested: {html}");
        assert!(html.contains("data-label=\"Limit\">$300</td>"), "Groceries's limit cell is still labeled Limit: {html}");
        assert!(html.contains("data-label=\"Spent\">$100</td>"), "Groceries's spent cell is still labeled Spent: {html}");

        // tfoot still sums expense-only (Groceries 300/100 + Zesty Snacks 50/10 = 350/110).
        let foot = tfoot(&html);
        assert!(foot.contains("$350"), "expense limit total in tfoot");
        assert!(foot.contains("$110"), "expense spent total in tfoot");
    }

    #[test]
    fn single_type_budget_stays_flat_with_no_group_header() {
        // Multiple rows, all the SAME category_type -> today's flat behavior:
        // no header row of any kind, rows just render in input order. #299
        // deliberately scopes its fix to grouped (multi-type) budgets only
        // (see the design doc's Assumption 1) — a single-type budget keeps
        // the generic Category/Limit/Spent/Remaining thead and data-labels
        // verbatim, even though the type here (income) doesn't semantically
        // match "Limit"/"Spent" either. That is an intentional, documented
        // scope boundary, not an oversight.
        let rows = vec![
            row("Bonus", "income", Some(1000.0), 0.0),
            row("Salary", "income", Some(4000.0), 4000.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(!html.contains("cat-group"), "single-type budget must not render any group header");
        assert!(!html.contains(">Income<"), "no standalone Income header text");
        // The generic shared thead is untouched (#299 scope boundary).
        assert!(html.contains(
            "<thead><tr><th>Category</th><th>Limit</th><th>Spent</th><th>Remaining</th></tr></thead>"
        ));
        assert!(html.contains("data-label=\"Limit\">$1,000</td>"), "flat rows keep the generic Limit label");
        // Rows are still present and in the given (already-alphabetical) order.
        let bonus = html.find("data-label=\"Category\">Bonus</td>").expect("Bonus row present");
        let salary = html.find("data-label=\"Category\">Salary</td>").expect("Salary row present");
        assert!(bonus < salary);
    }

    #[test]
    fn absent_group_renders_no_empty_header() {
        // Income + Expense present, Savings absent entirely -> two headers
        // (Income, Expense), never a "Savings" header/text anywhere.
        let rows = vec![
            row("Salary", "income", Some(4000.0), 4000.0),
            row("Rent", "expense", Some(1000.0), 900.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(html.contains(
            "<tr class=\"cat-group\"><th scope=\"col\">Income</th><th scope=\"col\">Target</th><th scope=\"col\">Received</th><th scope=\"col\">Remaining</th></tr>"
        ));
        assert!(html.contains(
            "<tr class=\"cat-group\"><th scope=\"col\">Expenses</th><th scope=\"col\">Limit</th><th scope=\"col\">Spent</th><th scope=\"col\">Remaining</th></tr>"
        ));
        assert!(
            !html.contains("Savings"),
            "no Savings header or any other Savings text when no savings categories are present"
        );
    }

    #[test]
    fn absent_income_group_renders_savings_and_expense_headers_only() {
        // The mirror case of absent_group_renders_no_empty_header above:
        // Savings + Expense present, Income absent entirely -> two headers
        // (Savings, Expenses), never an "Income" header/text anywhere. Both
        // absent-group combinations are now covered (review feedback on
        // PR #311 — the loop over CATEGORY_TABLE_GROUPS is uniform and
        // already pinned by the drift-guard test, but exercising a second
        // combination catches an off-by-one in the Income/Savings/Expense
        // iteration order that a single combination could miss).
        let rows = vec![
            row("Emergency Fund", "savings", Some(2000.0), 500.0),
            row("Rent", "expense", Some(1000.0), 900.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(html.contains(
            "<tr class=\"cat-group\"><th scope=\"col\">Savings</th><th scope=\"col\">Target</th><th scope=\"col\">Invested</th><th scope=\"col\">Remaining</th></tr>"
        ));
        assert!(html.contains(
            "<tr class=\"cat-group\"><th scope=\"col\">Expenses</th><th scope=\"col\">Limit</th><th scope=\"col\">Spent</th><th scope=\"col\">Remaining</th></tr>"
        ));
        assert!(
            !html.contains("Income"),
            "no Income header or any other Income text when no income categories are present"
        );
    }

    #[test]
    fn grouped_table_fund_row_uses_group_specific_labels() {
        // Review feedback on PR #311: push_category_row's amount_label/
        // activity_label params are threaded down identically on both the
        // fund and non-fund branches, but every existing fund-specific test
        // (categories_table_renders_fund_balance_and_effective_limit,
        // fund_overspend_gets_text_error_class_non_fund_does_not,
        // escapes_category_names_on_fund_row) uses a single-type (flat,
        // "Limit"/"Spent") fixture. This puts a fund row on a non-expense
        // type (savings) inside a GROUPED (multi-type) table to confirm the
        // new per-group labels compose correctly with the pre-existing
        // fund-badge/effective-limit formatting.
        let rows = vec![
            CategoryTableRow {
                id: Uuid::new_v4(),
                name: "Vacation Fund".to_string(),
                category_type: "savings".to_string(),
                category_limit: Some(100.0),
                spent: 150.0,
                is_fund: true,
                fund_balance: 80.0,
                rollover_enabled: false,
                linked_budget_id: None,
            },
            row("Rent", "expense", Some(1000.0), 900.0),
        ];
        let html = build_categories_table_html(&rows);
        // Effective limit = 100 + 80 = 180; remaining = 180 - 150 = $30 (not
        // a deficit) — the fund math is unaffected by which group it's in.
        assert!(
            html.contains("data-label=\"Target\">$180</td>"),
            "fund row's effective limit still renders under the Savings group's Target label: {html}"
        );
        assert!(
            html.contains("data-label=\"Invested\">$150</td>"),
            "fund row's spend still renders under the Savings group's Invested label: {html}"
        );
        assert!(
            html.contains("data-label=\"Remaining\">$30</td>"),
            "fund row's remaining is computed against the effective limit: {html}"
        );
        assert!(html.contains("Fund"), "the fund badge still renders inside a grouped table");
        assert!(html.contains("badge-warning"), "fund badge styling is unaffected by grouping");
        // The Expense row alongside it is unaffected (still Limit/Spent).
        assert!(html.contains("data-label=\"Limit\">$1,000</td>"));
        assert!(html.contains("data-label=\"Spent\">$900</td>"));
    }

    #[test]
    fn unrecognized_category_type_is_silently_excluded_from_grouped_body() {
        // Defensive/documented behavior (should be unreachable in practice —
        // ALLOWED_CATEGORY_TYPES is enforced at every write path): the
        // grouped path activates whenever 2+ DISTINCT category_type strings
        // are present, whether or not they're recognized — the gate
        // (`distinct_types.len() > 1`) doesn't distinguish valid from
        // invalid types. Here that's one valid type (income) plus one
        // invalid type (unknown_type), which is already enough to trigger
        // grouping. Once grouped, a row whose category_type is outside
        // {income, savings, expense} is not rendered anywhere (though it IS
        // logged via tracing::warn! — see build_categories_table_html).
        // This mirrors the pre-existing totals loop's exact-match-only
        // "expense" check, which already made the same implicit assumption
        // before this change.
        let rows = vec![
            row("Salary", "income", Some(4000.0), 4000.0),
            row("Mystery", "unknown_type", Some(999.0), 1.0),
        ];
        let html = build_categories_table_html(&rows);
        assert!(html.contains("data-label=\"Category\">Salary</td>"));
        assert!(
            !html.contains("Mystery"),
            "a row with an unrecognized category_type must not appear in the grouped body"
        );
    }

    #[test]
    fn grouped_types_match_allowed_category_types() {
        // Drift guard (#239 follow-up): CATEGORY_TABLE_GROUPS is a separate
        // literal from ALLOWED_CATEGORY_TYPES, hand-maintained in a
        // different part of this file. If a future ticket adds a 4th
        // legitimate category type to ALLOWED_CATEGORY_TYPES without also
        // updating CATEGORY_TABLE_GROUPS, every category of that new type
        // would silently vanish from any mixed-type budget's grouped view
        // (dropped by the same code path unrecognized-type rows take today).
        // This test fails loudly the moment the two lists diverge, instead
        // of relying on that invariant only being documented in a comment.
        let mut group_keys: Vec<&str> =
            CATEGORY_TABLE_GROUPS.iter().map(|(key, _, _, _)| *key).collect();
        group_keys.sort_unstable();
        let mut allowed: Vec<&str> = ALLOWED_CATEGORY_TYPES.to_vec();
        allowed.sort_unstable();
        assert_eq!(
            group_keys, allowed,
            "CATEGORY_TABLE_GROUPS's keys must be exactly ALLOWED_CATEGORY_TYPES"
        );
    }

    // DB-backed: only the active-period transactions count toward a category's
    // spend. Seeds a monthly budget with one in-window and one out-of-window
    // transaction and asserts only the in-window amount is summed.
    //   podman-compose up -d
    //   cargo test -p backend category_table_rows_sums_current_period -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_table_rows_sums_current_period() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let budget_id = Uuid::new_v4();
        let groceries_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("cat-table-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'cat table test', 'monthly', 1000.0)",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Groceries', 'expense', 200.0)",
        )
        .bind(groceries_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed category");

        // In-window transaction (now, well inside the current monthly period).
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 50.0, now(), 'in window')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(groceries_id)
        .execute(&pool)
        .await
        .expect("seed in-window txn");
        // Out-of-window transaction (~2 months ago) must NOT be counted.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 999.0, now() - interval '60 days', 'out of window')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(groceries_id)
        .execute(&pool)
        .await
        .expect("seed out-of-window txn");

        let rows = category_table_rows(&pool, budget_id)
            .await
            .expect("aggregate rows");
        let groceries = rows
            .iter()
            .find(|r| r.name == "Groceries")
            .expect("groceries row present");
        assert_eq!(groceries.spent, 50.0, "only in-window spend counted");
        assert_eq!(groceries.category_limit, Some(200.0));
        assert_eq!(groceries.id, groceries_id, "row carries its own category id");
        // NOTE: categories.rollover_enabled defaults to TRUE (deliberately, per
        // 20260617150000_category_rollover.sql — existing budgets with
        // rollover on keep every category carrying by default; a user must
        // explicitly opt a category OUT). The seeded Groceries row above never
        // sets the column, so it reads back at that TRUE default, not FALSE.
        assert!(
            groceries.rollover_enabled,
            "rollover_enabled defaults to true (see 20260617150000_category_rollover.sql)"
        );
        assert_eq!(groceries.linked_budget_id, None, "an ordinary category has no linked_budget_id");

        // A second budget to link against, and a second category that actually
        // sets rollover_enabled = FALSE and a real linked_budget_id — proving
        // category_table_rows reads back the FALSE/Some cases correctly, not
        // just the TRUE/None defaults asserted above.
        let linked_budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'cat table test - linked source', 'monthly', 500.0)",
        )
        .bind(linked_budget_id).bind(user_id).execute(&pool).await.expect("seed linked budget");
        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, rollover_enabled, linked_budget_id) \
             VALUES ($1, $2, 'Mirror', 'expense', FALSE, $3)",
        )
        .bind(mirror_id).bind(budget_id).bind(linked_budget_id).execute(&pool).await.expect("seed mirror category");

        let rows2 = category_table_rows(&pool, budget_id).await.expect("aggregate rows (2nd read)");
        let mirror = rows2.iter().find(|r| r.name == "Mirror").expect("mirror row present");
        assert_eq!(mirror.id, mirror_id, "mirror row carries its own category id");
        assert!(!mirror.rollover_enabled, "rollover_enabled FALSE must read back as false");
        assert_eq!(mirror.linked_budget_id, Some(linked_budget_id), "linked_budget_id must read back as Some(...)");

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(linked_budget_id).execute(&pool).await.ok();

        // Cleanup (children first for FK safety).
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id).execute(&pool).await.ok();
    }

    // DB-backed regression guard (#374): a category whose ONLY transaction is
    // `excluded_from_budget = true` must STILL appear in the listing with
    // `spent = 0` — NOT vanish. `category_table_rows` LEFT JOINs transactions and
    // keeps the `NOT t.excluded_from_budget` predicate in the ON clause precisely
    // so the excluded row is dropped from the SUM without dropping the category.
    // If that predicate were ever moved into the WHERE clause, the category would
    // disappear and this test would fail.
    //   podman-compose up -d
    //   cargo test -p backend category_table_rows_keeps_category -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_table_rows_keeps_category_with_only_excluded_tx_at_zero() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let budget_id = Uuid::new_v4();
        let groceries_id = Uuid::new_v4();
        let txn_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("cat-excluded-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'cat excluded test', 'monthly', 1000.0)",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Groceries', 'expense', 200.0)",
        )
        .bind(groceries_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed category");

        // The category's ONLY transaction, in-window, then excluded from budget.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 50.0, now(), 'excluded spend')",
        )
        .bind(txn_id)
        .bind(budget_id)
        .bind(groceries_id)
        .execute(&pool)
        .await
        .expect("seed txn");
        sqlx::query("UPDATE transactions SET excluded_from_budget = true WHERE id = $1")
            .bind(txn_id)
            .execute(&pool)
            .await
            .expect("exclude txn from budget");

        let rows = category_table_rows(&pool, budget_id)
            .await
            .expect("aggregate rows");
        let groceries = rows
            .iter()
            .find(|r| r.name == "Groceries")
            .expect("category with only excluded txns must STILL appear (LEFT JOIN, not WHERE)");
        assert_eq!(
            groceries.spent, 0.0,
            "excluded transaction must not count toward spend"
        );
        assert_eq!(groceries.id, groceries_id, "row carries its own category id");

        // Cleanup (children first for FK safety).
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id).execute(&pool).await.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Wire-compat guard for #198: `AuditLogResponse.budget_id` is `Option<Uuid>`
    /// (to match the now-nullable column), but `list_audit_logs` always yields
    /// `Some(..)`. serde must serialize `Some(uuid)` as a bare UUID string —
    /// identical to the prior `Uuid` field — so existing consumers are unchanged.
    #[test]
    fn audit_log_response_budget_id_serializes_as_bare_uuid() {
        let bid = Uuid::new_v4();
        let resp = AuditLogResponse {
            id: Uuid::new_v4(),
            budget_id: Some(bid),
            user_email: Some("a@example.test".to_string()),
            action: "AI_UPDATE_BUDGET".to_string(),
            details: None,
            created_at: Utc::now(),
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(
            v["budget_id"],
            serde_json::json!(bid.to_string()),
            "Some(uuid) must serialize as a bare UUID string, not a wrapped/object form"
        );
        assert!(
            v["budget_id"].is_string(),
            "budget_id must be a plain JSON string for wire compatibility"
        );
    }

    #[test]
    fn closed_message_is_byte_identical_for_conflict() {
        // CONFLICT must reproduce the prior inline message exactly, so closed-budget UX is unchanged (#168).
        assert_eq!(
            closed_or_transient_message((StatusCode::CONFLICT, "budget is closed".to_string()), "change"),
            "I can't change this budget: budget is closed."
        );
        assert_eq!(
            closed_or_transient_message((StatusCode::CONFLICT, "budget is closed".to_string()), "log to"),
            "I can't log to this budget: budget is closed."
        );
    }

    #[test]
    fn transient_message_distinguishes_and_invites_retry() {
        // A non-CONFLICT (e.g. transient DB fault) must NOT read as a permanent
        // rejection and must invite a retry — and must NOT leak the internal body (#168).
        let m = closed_or_transient_message(
            (StatusCode::INTERNAL_SERVER_ERROR, "internal database error".to_string()),
            "change",
        );
        assert!(m.contains("temporary"), "got: {m}");
        assert!(m.contains("try again"), "got: {m}");
        assert!(!m.contains("budget is closed"), "got: {m}");
        assert!(!m.contains("internal database error"), "must not leak internal detail: {m}");
    }

    #[test]
    fn category_type_normalization_contract() {
        // The chat write path (chat_create_categories) normalizes via this
        // validator, falling back to "expense" when it rejects (#168). This test
        // pins the VALIDATOR's accept/reject contract — the underlying invariant
        // the chat-path fallback relies on. (The two-line fallback in rag.rs is
        // not separately exercised here.)
        assert!(validate_category_type("expense").is_ok());
        assert!(validate_category_type("income").is_ok());
        assert!(validate_category_type("savings").is_ok());
        assert!(validate_category_type("groceries").is_err());
        assert!(validate_category_type("Expense").is_err());
    }

    #[test]
    fn resolve_category_type_defaults_keeps_and_falls_back() {
        // None -> default "expense"
        assert_eq!(resolve_category_type(None), "expense");
        // valid types pass through unchanged
        assert_eq!(resolve_category_type(Some("income")), "income");
        assert_eq!(resolve_category_type(Some("savings")), "savings");
        assert_eq!(resolve_category_type(Some("expense")), "expense");
        // invalid / junk / wrong-case -> fall back to "expense"
        assert_eq!(resolve_category_type(Some("groceries")), "expense");
        assert_eq!(resolve_category_type(Some("Expense")), "expense");
        assert_eq!(resolve_category_type(Some("")), "expense");
    }

    #[test]
    fn accepts_view_and_edit() {
        assert!(validate_permission_level("view").is_ok());
        assert!(validate_permission_level("edit").is_ok());
    }

    #[test]
    fn rejects_unknown_level_with_400() {
        let err = validate_permission_level("owner").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_empty_and_case_variants() {
        assert!(validate_permission_level("").is_err());
        assert!(validate_permission_level("View").is_err());
        assert!(validate_permission_level("EDIT").is_err());
    }

    #[test]
    fn budget_total_is_zero_with_no_categories() {
        assert_eq!(sum_category_amounts(&[]), 0.0);
    }

    #[test]
    fn budget_total_treats_missing_amounts_as_zero() {
        // A category with no amount counts as 0 in the budget total.
        assert_eq!(sum_category_amounts(&[None, None]), 0.0);
        assert_eq!(sum_category_amounts(&[Some(100.0), None]), 100.0);
    }

    #[test]
    fn budget_total_sums_category_amounts() {
        assert_eq!(
            sum_category_amounts(&[Some(100.0), Some(250.50), None, Some(0.0)]),
            350.50
        );
    }

    #[test]
    fn resolve_base_amount_fixed_uses_budget_limit() {
        // 'fixed' reports the stored budget_limit, ignoring the category sum.
        assert_eq!(resolve_base_amount("fixed", Some(2000.0), 100.0), 2000.0);
    }

    #[test]
    fn resolve_base_amount_fixed_null_limit_is_zero() {
        // A 'fixed' budget with no stored amount reports 0.
        assert_eq!(resolve_base_amount("fixed", None, 100.0), 0.0);
    }

    #[test]
    fn resolve_base_amount_derived_uses_category_sum() {
        // 'derived' (and any non-'fixed' mode) reports the summed category total.
        assert_eq!(resolve_base_amount("derived", Some(2000.0), 100.0), 100.0);
        assert_eq!(resolve_base_amount("derived", None, 100.0), 100.0);
    }

    #[test]
    fn budget_payload_amount_is_optional_on_deserialization() {
        // Absent field -> None (the "create without an amount" path, issue #46).
        let absent: BudgetPayload = serde_json::from_str(
            r#"{"name":"Vacation","time_frame":"monthly"}"#,
        )
        .expect("absent budget_limit must deserialize");
        assert_eq!(absent.budget_limit, None);

        // Explicit null -> None.
        let explicit_null: BudgetPayload = serde_json::from_str(
            r#"{"name":"Vacation","time_frame":"monthly","budget_limit":null}"#,
        )
        .expect("null budget_limit must deserialize");
        assert_eq!(explicit_null.budget_limit, None);

        // Present value -> Some, preserved exactly.
        let present: BudgetPayload = serde_json::from_str(
            r#"{"name":"Vacation","time_frame":"monthly","budget_limit":500.0}"#,
        )
        .expect("present budget_limit must deserialize");
        assert_eq!(present.budget_limit, Some(500.0));
    }

    #[test]
    fn budget_payload_rollover_is_optional() {
        let absent: BudgetPayload = serde_json::from_str(r#"{"name":"V","time_frame":"monthly"}"#).unwrap();
        assert_eq!(absent.rollover_enabled, None);
        let null: BudgetPayload = serde_json::from_str(r#"{"name":"V","time_frame":"monthly","rollover_enabled":null}"#).unwrap();
        assert_eq!(null.rollover_enabled, None);
        let on: BudgetPayload = serde_json::from_str(r#"{"name":"V","time_frame":"monthly","rollover_enabled":true}"#).unwrap();
        assert_eq!(on.rollover_enabled, Some(true));
        let off: BudgetPayload = serde_json::from_str(r#"{"name":"V","time_frame":"monthly","rollover_enabled":false}"#).unwrap();
        assert_eq!(off.rollover_enabled, Some(false));
    }

    #[test]
    fn budget_payload_budget_type_is_optional() {
        // Absent field -> None (create defaults to 'time_based'; update preserves).
        let absent: BudgetPayload =
            serde_json::from_str(r#"{"name":"V","time_frame":"monthly"}"#).unwrap();
        assert_eq!(absent.budget_type, None);
        // Explicit null -> None.
        let null: BudgetPayload = serde_json::from_str(
            r#"{"name":"V","time_frame":"monthly","budget_type":null}"#,
        )
        .unwrap();
        assert_eq!(null.budget_type, None);
        // Present value -> Some, preserved exactly.
        let project: BudgetPayload = serde_json::from_str(
            r#"{"name":"V","time_frame":"monthly","budget_type":"project"}"#,
        )
        .unwrap();
        assert_eq!(project.budget_type, Some("project".to_string()));
    }

    #[test]
    fn budget_payload_amount_mode_is_optional() {
        // Absent field -> None (#116: create defaults to 'derived'; update preserves).
        let absent: BudgetPayload =
            serde_json::from_str(r#"{"name":"V","time_frame":"monthly"}"#).unwrap();
        assert_eq!(absent.amount_mode, None);
        // Explicit null -> None.
        let null: BudgetPayload = serde_json::from_str(
            r#"{"name":"V","time_frame":"monthly","amount_mode":null}"#,
        )
        .unwrap();
        assert_eq!(null.amount_mode, None);
        // Present value -> Some, preserved exactly.
        let fixed: BudgetPayload = serde_json::from_str(
            r#"{"name":"V","time_frame":"monthly","amount_mode":"fixed"}"#,
        )
        .unwrap();
        assert_eq!(fixed.amount_mode, Some("fixed".to_string()));
    }

    #[test]
    fn budget_payload_budget_strategy_option_semantics() {
        let absent: BudgetPayload = serde_json::from_str(
            r#"{"name":"X","time_frame":"monthly"}"#,
        )
        .unwrap();
        assert_eq!(absent.budget_strategy, None);

        let present: BudgetPayload = serde_json::from_str(
            r#"{"name":"X","time_frame":"monthly","budget_strategy":"zero_based"}"#,
        )
        .unwrap();
        assert_eq!(present.budget_strategy, Some("zero_based".to_string()));
    }

    #[test]
    fn rollup_link_rejects_self_link() {
        // A budget cannot roll up into itself (#52) -> 400.
        let id = Uuid::new_v4();
        let err = validate_rollup_link(
            id, id, false, false, "time_based", "time_based", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rollup_link_rejects_parent_that_is_a_child() {
        // The prospective parent is itself rolled up into something -> would create
        // a 2-level chain; single-level only (#52) -> 409.
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let err = validate_rollup_link(
            parent, child, true, false, "time_based", "time_based", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn rollup_link_rejects_child_that_is_a_parent() {
        // The prospective child already has its own children -> it would become both
        // a parent and a child; single-level only (#52) -> 409. This is the core
        // cycle/nesting guard.
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let err = validate_rollup_link(
            parent, child, false, true, "time_based", "time_based", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn rollup_link_allows_two_standalone_budgets() {
        // Neither is already in a rollup relationship -> the link is permitted.
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        assert!(validate_rollup_link(
            parent, child, false, false, "time_based", "time_based", "zero_based", "zero_based",
        )
        .is_ok());
    }

    #[test]
    fn validate_rollup_link_rejects_type_mismatch() {
        // #300: budgets can only be rolled up together when they share the same
        // budget_type.
        let err = validate_rollup_link(
            Uuid::new_v4(), Uuid::new_v4(), false, false,
            "time_based", "project", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn validate_rollup_link_rejects_strategy_mismatch() {
        // #300: budgets can only be rolled up together when they share the same
        // budget_strategy.
        let err = validate_rollup_link(
            Uuid::new_v4(), Uuid::new_v4(), false, false,
            "time_based", "time_based", "zero_based", "limit_spent_remaining",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn validate_rollup_link_allows_matching_type_and_strategy() {
        assert!(validate_rollup_link(
            Uuid::new_v4(), Uuid::new_v4(), false, false,
            "project", "project", "limit_spent_remaining", "limit_spent_remaining",
        )
        .is_ok());
    }

    #[test]
    fn validate_rollup_link_self_link_still_wins_over_type_mismatch() {
        // The self-link guard fires before the new type/strategy checks even when
        // both would independently fail.
        let id = Uuid::new_v4();
        let err = validate_rollup_link(
            id, id, false, false,
            "time_based", "project", "zero_based", "limit_spent_remaining",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST); // self-link check fires first
    }

    #[test]
    fn validate_rollup_link_chain_violation_still_wins_over_strategy_mismatch() {
        // The existing single-level chain guard (parent_is_child) fires before the
        // new strategy-mismatch check even when both would independently fail.
        let err = validate_rollup_link(
            Uuid::new_v4(), Uuid::new_v4(), true, false,
            "time_based", "time_based", "zero_based", "limit_spent_remaining",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        // The existing chain-violation message, not the new strategy-mismatch one.
        assert!(err.1.contains("rollup is single-level"));
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_allows_non_participant_change() {
        assert!(ensure_rollup_type_or_strategy_unchanged(
            false, false, "budget type", "time_based", "project"
        )
        .is_ok());
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_allows_unchanged_value_on_child() {
        assert!(ensure_rollup_type_or_strategy_unchanged(
            true, false, "budget type", "time_based", "time_based"
        )
        .is_ok());
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_rejects_change_on_child() {
        let err = ensure_rollup_type_or_strategy_unchanged(
            true, false, "budget type", "time_based", "project",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("child"));
        assert!(err.1.contains("time_based"));
        assert!(err.1.contains("project"));
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_rejects_change_on_parent() {
        let err = ensure_rollup_type_or_strategy_unchanged(
            false, true, "budgeting strategy", "limit_spent_remaining", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("parent"));
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_rejects_change_when_both_flags_set() {
        // Structurally shouldn't happen (validate_rollup_link prevents a budget from becoming
        // both), but the guard must still reject rather than silently allow.
        let err = ensure_rollup_type_or_strategy_unchanged(
            true, true, "budget type", "time_based", "project",
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn rollup_category_name_returns_source_when_no_collision() {
        // No existing category matches the source budget's name -> use it verbatim (#52).
        let existing = vec!["Groceries".to_string(), "Rent".to_string()];
        assert_eq!(
            rollup_category_name("Vacation", &existing).unwrap(),
            "Vacation"
        );
    }

    #[test]
    fn rollup_category_name_suffixes_on_case_insensitive_collision() {
        // The plain name collides (case-insensitively) -> fall back to the
        // "(rolled up)" suffixed name (#52).
        let existing = vec!["groceries".to_string()];
        assert_eq!(
            rollup_category_name("Groceries", &existing).unwrap(),
            "Groceries (rolled up)"
        );
    }

    #[test]
    fn rollup_category_name_rejects_when_both_taken() {
        // Both the plain and the suffixed names are already taken (case-insensitively)
        // -> there is no free name, so reject with 409 (#52).
        let existing = vec![
            "Groceries".to_string(),
            "groceries (ROLLED UP)".to_string(),
        ];
        let err = rollup_category_name("Groceries", &existing).unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn current_period_window_is_between_prev_end_and_next_boundary() {
        // The current period starts where the previous period ends and ends at the
        // next boundary — anchored identically to the other two window helpers (#52).
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();
        for tf in ["monthly", "quarterly", "yearly", "weekly"] {
            let (_, prev_end) = previous_period_window(tf, now);
            let next = next_period_boundary(tf, now);
            let (cur_start, cur_end) = current_period_window(tf, now);
            assert_eq!(cur_start, prev_end, "current start == previous end ({tf})");
            assert_eq!(cur_end, next, "current end == next boundary ({tf})");
            assert!(cur_start <= now && now < cur_end, "now is inside the current window ({tf})");
        }
    }

    #[test]
    fn carried_zero_when_disabled() {
        assert_eq!(carried_amount(false, 500.0, 200.0), 0.0);
        assert_eq!(carried_amount(false, 500.0, 700.0), 0.0);
    }

    #[test]
    fn carried_is_unused_remainder_when_enabled() {
        assert_eq!(carried_amount(true, 500.0, 200.0), 300.0);
    }

    #[test]
    fn carried_clamps_overspend_to_zero() {
        // Overspend (prev_spent > base) does NOT carry as a negative — clamps at 0.
        assert_eq!(carried_amount(true, 500.0, 700.0), 0.0);
    }

    #[test]
    fn carried_zero_when_base_zero() {
        assert_eq!(carried_amount(true, 0.0, 0.0), 0.0);
    }

    #[test]
    fn carried_full_base_when_nothing_spent() {
        assert_eq!(carried_amount(true, 500.0, 0.0), 500.0);
    }

    // --- category_carried (#49) ---

    #[test]
    fn category_carried_zero_when_budget_off() {
        // Budget master switch off -> 0, even with the category opted in.
        assert_eq!(category_carried(false, true, "time_based", 500.0, 100.0), 0.0);
    }

    #[test]
    fn category_carried_zero_when_category_off() {
        assert_eq!(category_carried(true, false, "time_based", 500.0, 100.0), 0.0);
    }

    #[test]
    fn category_carried_zero_for_project() {
        // Project budgets are excluded from rollover (#48), regardless of switches.
        assert_eq!(category_carried(true, true, "project", 500.0, 100.0), 0.0);
    }

    #[test]
    fn category_carried_clamps_overspend() {
        // Overspend (700 > 500) clamps to 0, never a negative carry.
        assert_eq!(category_carried(true, true, "time_based", 500.0, 700.0), 0.0);
    }

    #[test]
    fn category_carried_zero_base() {
        assert_eq!(category_carried(true, true, "time_based", 0.0, 0.0), 0.0);
    }

    #[test]
    fn category_carried_zero_when_spent_equals_base() {
        // Exact boundary: nothing left over (base - prev_spent == 0) carries 0.
        // Guards the .max(0.0) clamp against an off-by-one refactor at equality.
        assert_eq!(category_carried(true, true, "time_based", 500.0, 500.0), 0.0);
    }

    #[test]
    fn category_carried_remainder_when_both_on() {
        assert_eq!(category_carried(true, true, "time_based", 500.0, 200.0), 300.0);
    }

    #[test]
    fn per_category_clamp_differs_from_budget_clamp_on_mixed_over_under() {
        // A overspent (over by 50), B underspent (under by 60). Per-category clamps
        // A's overspend to 0 independently, so only B's remainder carries (60).
        // Budget-level nets the two together: carried_amount(true, 200, 190) = 10.
        let per_cat = category_carried(true, true, "time_based", 100.0, 150.0)
            + category_carried(true, true, "time_based", 100.0, 40.0);
        let budget_level = carried_amount(true, 200.0, 190.0);
        assert_eq!(per_cat, 60.0);
        assert_eq!(budget_level, 10.0);
        assert_ne!(per_cat, budget_level);
    }

    #[test]
    fn per_category_equals_budget_when_all_underspent() {
        // With no overspend anywhere, per-category summation matches budget-level.
        let per_cat = category_carried(true, true, "time_based", 100.0, 30.0)
            + category_carried(true, true, "time_based", 200.0, 50.0);
        let budget_level = carried_amount(true, 300.0, 80.0);
        assert!((per_cat - budget_level).abs() < 1e-9);
    }

    // --- category_carry_for (#426) ---

    #[test]
    fn category_carry_for_fund_beats_rollover() {
        // #228 precedence: a fund uses fund_balance, never the #49 one-period carry.
        let (prev, carried) = category_carry_for(true, false, true, 42.0, true, true, "time_based", 100.0, 80.0);
        assert_eq!(prev, 0.0);
        assert_eq!(carried, 42.0);
    }

    #[test]
    fn category_carry_for_rollover_when_not_fund() {
        // base 100, prev_spent 80 -> carry 20.
        let (prev, carried) = category_carry_for(true, false, false, 0.0, true, true, "time_based", 100.0, 80.0);
        assert_eq!(prev, 80.0);
        assert_eq!(carried, 20.0);
    }

    #[test]
    fn category_carry_for_overspend_clamps_to_zero() {
        let (_, carried) = category_carry_for(true, false, false, 0.0, true, true, "time_based", 100.0, 150.0);
        assert_eq!(carried, 0.0);
    }

    #[test]
    fn category_carry_for_mirror_is_zero() {
        // A #52 mirror never carries — it reflects another budget's live total.
        let (prev, carried) = category_carry_for(true, true, true, 99.0, true, true, "time_based", 100.0, 80.0);
        assert_eq!(prev, 0.0);
        assert_eq!(carried, 0.0);
    }

    #[test]
    fn category_carry_for_non_expense_is_zero() {
        let (prev, carried) = category_carry_for(false, false, false, 0.0, true, true, "time_based", 100.0, 80.0);
        assert_eq!(prev, 0.0);
        assert_eq!(carried, 0.0);
    }

    #[test]
    fn category_carry_for_rollover_off_is_zero() {
        let (_, carried) = category_carry_for(true, false, false, 0.0, false, true, "time_based", 100.0, 80.0);
        assert_eq!(carried, 0.0);
    }

    // Fund categories (#228): effective_limit = category_limit + fund_balance,
    // no floor. Non-fund categories are unaffected (returns category_limit
    // unchanged, ignoring whatever fund_balance happens to hold).
    #[test]
    fn fund_effective_limit_non_fund_ignores_balance() {
        assert_eq!(fund_effective_limit(false, 100.0, 9999.0), 100.0);
    }

    #[test]
    fn fund_effective_limit_adds_positive_balance() {
        assert_eq!(fund_effective_limit(true, 100.0, 50.0), 150.0);
    }

    #[test]
    fn fund_effective_limit_allows_negative_result() {
        // Sustained overspend can push the effective limit below zero — no floor.
        assert_eq!(fund_effective_limit(true, 100.0, -180.0), -80.0);
    }

    #[test]
    fn fund_effective_limit_zero_balance_is_plain_limit() {
        assert_eq!(fund_effective_limit(true, 100.0, 0.0), 100.0);
    }

    // Reproduces the ticket's worked example exactly (nels#228): a $100
    // monthly fund category, chained Jan -> Apr. Each period's balance-after
    // and effective-limit-for-the-NEXT-period must match the ticket's table.
    #[test]
    fn fund_worked_example_jan_through_apr() {
        let limit = 100.0;

        // Jan: spent 50. Effective limit for Jan itself is just the base (no
        // balance has accrued yet at the start of the fund's first period).
        let jan_effective = fund_effective_limit(true, limit, 0.0);
        assert_eq!(jan_effective, 100.0, "Jan effective limit");
        let balance_after_jan = 0.0 + (limit - 50.0);
        assert_eq!(balance_after_jan, 50.0, "balance after Jan");

        // Feb: spent 70. Effective limit for Feb reflects Jan's carried balance.
        let feb_effective = fund_effective_limit(true, limit, balance_after_jan);
        assert_eq!(feb_effective, 150.0, "Feb effective limit (100 + 50)");
        let balance_after_feb = balance_after_jan + (limit - 70.0);
        assert_eq!(balance_after_feb, 80.0, "balance after Feb");

        // Mar: spent 200 (overspend). Effective limit for Mar reflects Feb's
        // balance; the overspend then drives the balance negative.
        let mar_effective = fund_effective_limit(true, limit, balance_after_feb);
        assert_eq!(mar_effective, 180.0, "Mar effective limit (100 + 80)");
        let balance_after_mar = balance_after_feb + (limit - 200.0);
        assert_eq!(balance_after_mar, -20.0, "balance after Mar (overspend pushes negative)");

        // Apr: no spend yet. Effective limit for Apr reflects Mar's deficit —
        // a genuine reduction below the base limit.
        let apr_effective = fund_effective_limit(true, limit, balance_after_mar);
        assert_eq!(apr_effective, 80.0, "Apr effective limit (100 - 20)");
    }

    // CATEGORY_AFFORDABILITY (#302): compares a requested spend amount against a
    // category's already-resolved effective remaining balance. Pure arithmetic —
    // callers (rag.rs) are responsible for resolving `effective_limit` (including
    // the "no limit configured" case) before calling this.
    #[test]
    fn evaluate_affordability_affordable_with_room() {
        // limit 200, spent 80 -> remaining 120; requesting 50 fits with room to spare.
        let check = evaluate_affordability(50.0, 200.0, 80.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 120.0);
        assert_eq!(check.overage, 0.0);
    }

    #[test]
    fn evaluate_affordability_affordable_at_exact_boundary() {
        // remaining is exactly 120; requesting exactly 120 must still be affordable (<=, not <).
        let check = evaluate_affordability(120.0, 200.0, 80.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 120.0);
        assert_eq!(check.overage, 0.0);
    }

    #[test]
    fn evaluate_affordability_unaffordable_reports_overage() {
        // limit 200, spent 160 -> remaining 40; requesting 50 is 10 over.
        let check = evaluate_affordability(50.0, 200.0, 160.0);
        assert!(!check.can_afford);
        assert_eq!(check.remaining, 40.0);
        assert_eq!(check.overage, 10.0);
    }

    #[test]
    fn evaluate_affordability_negative_remaining_from_fund_deficit() {
        // A fund category already in deficit (effective_limit already negative, e.g. -20 from a
        // sustained overspend) makes ANY positive request unaffordable, with overage = requested
        // minus the (negative) remaining.
        let check = evaluate_affordability(10.0, -20.0, 0.0);
        assert!(!check.can_afford);
        assert_eq!(check.remaining, -20.0);
        assert_eq!(check.overage, 30.0);
    }

    #[test]
    fn evaluate_affordability_zero_request_is_always_affordable_unless_already_over() {
        let check = evaluate_affordability(0.0, 100.0, 100.0);
        assert!(check.can_afford);
        assert_eq!(check.remaining, 0.0);
        assert_eq!(check.overage, 0.0);
    }

    #[test]
    fn category_response_with_carry_fund_category_reports_fund_balance() {
        let cat = Category {
            id: Uuid::new_v4(),
            budget_id: Uuid::new_v4(),
            name: "Groceries".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            rollover_enabled: true, // #228 precedence: is_fund wins, rollover ignored
            linked_budget_id: None,
            is_fund: true,
            fund_balance: 80.0,
            fund_advanced_through: Some(Utc::now()),
            created_at: Utc::now(),
        };
        // budget_rollover=true, prev_spent=999 would normally drive a large
        // #49 carry via category_carried — but for a fund category the fund
        // balance must be used INSTEAD, not stacked on top.
        let resp = category_response_with_carry(cat, true, "time_based", 999.0, 0.0);
        assert!(resp.is_fund);
        assert_eq!(resp.fund_balance, 80.0);
        assert_eq!(resp.carried_amount, 80.0, "carried_amount reports the fund balance, not the #49 carry");
        assert_eq!(resp.effective_amount, 180.0, "effective = base(100) + fund_balance(80)");
    }

    #[test]
    fn category_response_with_carry_non_fund_category_unaffected() {
        let cat = Category {
            id: Uuid::new_v4(),
            budget_id: Uuid::new_v4(),
            name: "Rent".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(1000.0),
            rollover_enabled: true,
            linked_budget_id: None,
            is_fund: false,
            fund_balance: 0.0,
            fund_advanced_through: None,
            created_at: Utc::now(),
        };
        let resp = category_response_with_carry(cat, true, "time_based", 200.0, 0.0);
        assert!(!resp.is_fund);
        // Unaffected: still the plain #49 carry, per category_carried's
        // existing (and unit-tested) semantics — max(0, 1000 - 200) = 800.
        assert_eq!(resp.carried_amount, 800.0);
        assert_eq!(resp.effective_amount, 1800.0);
    }

    #[test]
    fn prev_window_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 3, 14, 12, 0, 0).unwrap();
        let (s, e) = previous_period_window("monthly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_monthly_january_crosses_year() {
        let now = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let (s, e) = previous_period_window("monthly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_yearly() {
        let now = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let (s, e) = previous_period_window("yearly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_quarterly_q1_goes_to_prev_q4() {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap(); // Q1 2026
        let (s, e) = previous_period_window("quarterly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2025, 10, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_quarterly_q2_goes_to_q1() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 0, 0, 0).unwrap(); // Q2 2026
        let (s, e) = previous_period_window("quarterly", now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(e, Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn prev_window_unknown_timeframe_falls_back_to_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 3, 14, 0, 0, 0).unwrap();
        assert_eq!(
            previous_period_window("weekly", now),
            previous_period_window("monthly", now)
        );
    }

    #[test]
    fn audit_retention_defaults_when_absent() {
        assert_eq!(parse_audit_retention(None), 365);
    }

    #[test]
    fn audit_retention_defaults_when_empty() {
        assert_eq!(parse_audit_retention(Some(String::new())), 365);
    }

    #[test]
    fn audit_retention_defaults_when_unparseable() {
        assert_eq!(parse_audit_retention(Some("abc".to_string())), 365);
    }

    #[test]
    fn audit_retention_defaults_when_below_minimum() {
        // 0 and negatives are nonsensical -> fall back to default
        assert_eq!(parse_audit_retention(Some("0".to_string())), 365);
        assert_eq!(parse_audit_retention(Some("-5".to_string())), 365);
    }

    #[test]
    fn audit_retention_parses_valid_value() {
        assert_eq!(parse_audit_retention(Some("180".to_string())), 180);
        assert_eq!(parse_audit_retention(Some("  90 ".to_string())), 90);
    }

    #[test]
    fn next_period_boundary_monthly() {
        // Mid-month -> first of next month.
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 10, 30, 0).unwrap();
        assert_eq!(
            next_period_boundary("monthly", now),
            Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap()
        );
        // December rolls over to January of the next year.
        let dec = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap();
        assert_eq!(
            next_period_boundary("monthly", dec),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_quarterly() {
        // Q2 (Apr-Jun) -> start of Q3 (Jul 1).
        let q2 = Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q2),
            Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap()
        );
        // Q1 (Jan-Mar) -> start of Q2 (Apr 1).
        let q1 = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q1),
            Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()
        );
        // Q4 (Oct-Dec) -> start of Q1 next year (Jan 1).
        let q4 = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("quarterly", q4),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_yearly() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("yearly", now),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_period_boundary_unknown_falls_back_to_monthly() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        assert_eq!(
            next_period_boundary("weekly", now),
            next_period_boundary("monthly", now)
        );
    }

    #[test]
    fn renewal_marker_set_when_enabled_cleared_when_disabled() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        // Disabled -> no marker.
        assert_eq!(renewal_marker(false, "monthly", now), None);
        // Enabled -> the next boundary for the timeframe.
        assert_eq!(
            renewal_marker(true, "monthly", now),
            Some(next_period_boundary("monthly", now))
        );
        assert_eq!(
            renewal_marker(true, "yearly", now),
            Some(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn audit_retention_rejects_above_i32_max() {
        // Values past i32::MAX would wrap to a negative interval in the
        // `days as i32` bind, so they fall back to the default.
        assert_eq!(parse_audit_retention(Some("2147483647".to_string())), 2147483647);
        assert_eq!(parse_audit_retention(Some("2147483648".to_string())), 365);
        assert_eq!(parse_audit_retention(Some("9999999999".to_string())), 365);
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Seeds an isolated user/budget with unique UUIDs and cleans up after.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_audit_logs_drops_old_keeps_recent() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();

        // Seed an isolated user + budget (audit_logs.budget_id FK requires both).
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("audit-retention-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit)
             VALUES ($1, $2, 'audit retention test', 'monthly', 1000.0)",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

        // Helper to insert one audit_log row at a chosen age (days ago).
        async fn seed_log(
            pool: &PgPool,
            budget: uuid::Uuid,
            user: uuid::Uuid,
            action: &str,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO audit_logs (id, budget_id, user_id, action, created_at)
                 VALUES ($1, $2, $3, $4, NOW() - make_interval(days => $5::int))",
            )
            .bind(id)
            .bind(budget)
            .bind(user)
            .bind(action)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed audit log");
            id
        }

        let old = seed_log(&pool, budget_id, user_id, "old-action", 400).await;
        let recent = seed_log(&pool, budget_id, user_id, "recent-action", 0).await;

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM audit_logs WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // The table-wide `deleted` count is not asserted: a sibling test calling
        // purge_old_audit_logs concurrently can delete OUR expired row first,
        // making even a `>= 1` lower bound racy. The per-id check below is
        // isolated by UUID and holds regardless of which purge did the deleting.
        let _ = purge_old_audit_logs(&pool, 365).await.expect("purge");
        assert!(!exists(&pool, old).await, "old row deleted");
        assert!(exists(&pool, recent).await, "recent row retained");

        // Idempotency: a second run must not touch OUR rows. The table-wide
        // return value is fragile (a concurrent test could insert+leave an
        // expired row between the two calls), so assert on our scoped state
        // instead: the recent row for this budget still remains.
        let _ = purge_old_audit_logs(&pool, 365).await.expect("purge again");
        let remaining_after_second: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1")
                .bind(budget_id)
                .fetch_one(&pool)
                .await
                .expect("count remaining after second purge");
        assert_eq!(
            remaining_after_second, 1,
            "second run leaves our recent row intact"
        );

        // Cleanup: audit_logs, then budget, then user.
        sqlx::query("DELETE FROM audit_logs WHERE budget_id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup audit_logs");
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup budget");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Seeds an isolated user/budget with unique UUIDs and cleans up after.
    //
    // Pins the strict `<` window boundary in `purge_old_audit_logs`: with a
    // 365-day window, a 364-day-old row survives and a 366-day-old row is
    // deleted. Asserted by id so the result is immune to the shared DB.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_audit_logs_respects_window_boundary() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("audit-boundary-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit)
             VALUES ($1, $2, 'audit boundary test', 'monthly', 1000.0)",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

        async fn seed_log(
            pool: &PgPool,
            budget: uuid::Uuid,
            user: uuid::Uuid,
            action: &str,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO audit_logs (id, budget_id, user_id, action, created_at)
                 VALUES ($1, $2, $3, $4, NOW() - make_interval(days => $5::int))",
            )
            .bind(id)
            .bind(budget)
            .bind(user)
            .bind(action)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed audit log");
            id
        }

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM audit_logs WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // Window = 365: 364d survives (364 < 365), 366d deleted (366 > 365).
        let day_364 = seed_log(&pool, budget_id, user_id, "boundary-364", 364).await;
        let day_366 = seed_log(&pool, budget_id, user_id, "boundary-366", 366).await;

        // The table-wide `deleted` count is intentionally not asserted here: a
        // sibling test calling `purge_old_audit_logs` concurrently can delete
        // OUR expired row first, making even a `>= 1` lower bound racy. The
        // per-id assertions below are isolated by UUID and are the meaningful
        // checks — they hold regardless of who issued the deleting purge.
        let _ = purge_old_audit_logs(&pool, 365).await.expect("purge");

        assert!(exists(&pool, day_364).await, "364d survives: 364 < 365");
        assert!(!exists(&pool, day_366).await, "366d deleted: 366 > 365");

        // Cleanup: audit_logs, then budget, then user.
        sqlx::query("DELETE FROM audit_logs WHERE budget_id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup audit_logs");
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup budget");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Proves the batched loop drains MORE than one batch: with batch_size = 2 and
    // 5 expired rows, the loop must iterate (2 + 2 + 1) to delete them all.
    // Asserted by id (not by the table-wide return count) because sibling --ignored
    // tests purge the same age predicate concurrently; by-id checks are race-immune.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_audit_logs_batches_until_drained() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("audit-batch-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit)
             VALUES ($1, $2, 'audit batch test', 'monthly', 1000.0)",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

        async fn seed_log(
            pool: &PgPool,
            budget: uuid::Uuid,
            user: uuid::Uuid,
            action: &str,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO audit_logs (id, budget_id, user_id, action, created_at)
                 VALUES ($1, $2, $3, $4, NOW() - make_interval(days => $5::int))",
            )
            .bind(id)
            .bind(budget)
            .bind(user)
            .bind(action)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed audit log");
            id
        }

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM audit_logs WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // 5 expired rows + 1 recent row, all scoped to our isolated budget.
        let mut old_ids = Vec::new();
        for i in 0..5 {
            old_ids.push(seed_log(&pool, budget_id, user_id, &format!("old-{i}"), 400).await);
        }
        let recent = seed_log(&pool, budget_id, user_id, "recent", 0).await;

        // batch_size = 2 forces multiple iterations to drain the 5 expired rows.
        let _ = purge_old_audit_logs_batched(&pool, 365, 2)
            .await
            .expect("batched purge");

        for id in &old_ids {
            assert!(!exists(&pool, *id).await, "expired row {id} drained across batches");
        }
        assert!(exists(&pool, recent).await, "recent row retained");

        // Cleanup: audit_logs, then budget, then user.
        sqlx::query("DELETE FROM audit_logs WHERE budget_id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup audit_logs");
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("cleanup budget");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    #[test]
    fn accepts_time_based_and_project() {
        assert!(validate_budget_type("time_based").is_ok());
        assert!(validate_budget_type("project").is_ok());
    }

    #[test]
    fn rejects_unknown_budget_type_with_400() {
        for bad in ["", "Project", "monthly"] {
            let err = validate_budget_type(bad).unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn accepts_derived_and_fixed_amount_modes() {
        // #116: the two valid amount modes.
        assert!(validate_amount_mode("derived").is_ok());
        assert!(validate_amount_mode("fixed").is_ok());
    }

    #[test]
    fn rejects_unknown_amount_mode_with_400() {
        // Case-sensitive and non-empty (#116) — anything else is a 400.
        for bad in ["FIXED", "", "Derived", "auto"] {
            let err = validate_amount_mode(bad).unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn validate_budget_strategy_accepts_known_and_rejects_unknown() {
        assert!(validate_budget_strategy("zero_based").is_ok());
        assert!(validate_budget_strategy("limit_spent_remaining").is_ok());
        let err = validate_budget_strategy("fifty_thirty_twenty").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("zero_based"));
        assert!(err.1.contains("limit_spent_remaining"));
    }

    #[test]
    fn project_never_carries() {
        // Project budgets are excluded from rollover even when enabled with a
        // positive unused remainder.
        assert_eq!(effective_carried("project", true, 500.0, 200.0), 0.0);
    }

    #[test]
    fn time_based_enabled_delegates_to_carried_amount() {
        assert_eq!(
            effective_carried("time_based", true, 500.0, 200.0),
            carried_amount(true, 500.0, 200.0)
        );
        assert_eq!(effective_carried("time_based", true, 500.0, 200.0), 300.0);
    }

    #[test]
    fn time_based_disabled_carries_zero() {
        assert_eq!(effective_carried("time_based", false, 500.0, 200.0), 0.0);
    }

    #[test]
    fn project_span_open_ends_at_now() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let (start, end) = project_span(created, None, now);
        assert_eq!(start, created);
        assert_eq!(end, now);
    }

    #[test]
    fn project_span_closed_ends_at_closed_at() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let closed = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let (start, end) = project_span(created, Some(closed), now);
        assert_eq!(start, created);
        assert_eq!(end, closed);
    }

    #[test]
    fn archive_filter_defaults_to_active_only() {
        // Absent/None query -> default to active budgets only (want_archived false).
        let q = ListBudgetsQuery::default();
        let want = q.archived.unwrap_or(false);
        assert!(!want);
        // Active rows (archived_at IS NULL) match; archived rows do not.
        assert!(matches_archive_filter(false, want));
        assert!(!matches_archive_filter(true, want));
    }

    #[test]
    fn archive_filter_explicit_false_returns_active_only() {
        let q: ListBudgetsQuery = ListBudgetsQuery { archived: Some(false) };
        let want = q.archived.unwrap_or(false);
        assert!(!want);
        assert!(matches_archive_filter(false, want));
        assert!(!matches_archive_filter(true, want));
    }

    #[test]
    fn archive_filter_true_returns_archived_only() {
        let q = ListBudgetsQuery { archived: Some(true) };
        let want = q.archived.unwrap_or(false);
        assert!(want);
        // Archived rows match; active rows do not.
        assert!(matches_archive_filter(true, want));
        assert!(!matches_archive_filter(false, want));
    }

    #[test]
    fn higher_share_level_picks_edit_over_view_regardless_of_order() {
        // "edit" > "view" by privilege, NOT lexically (lexically "edit" < "view").
        assert_eq!(higher_share_level("view", "edit"), "edit");
        assert_eq!(higher_share_level("edit", "view"), "edit");
        assert_eq!(higher_share_level("view", "view"), "view");
        assert_eq!(higher_share_level("edit", "edit"), "edit");
    }

    // --- Auto-renew DB-backed tests (#51) ---
    //
    // Run on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Each test seeds an isolated user with unique UUIDs and cleans up after.

    /// Connect, seed a user, and return (pool, user_id). Caller cleans up.
    #[cfg(test)]
    async fn renew_test_setup() -> (PgPool, Uuid) {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("auto-renew-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        (pool, user_id)
    }

    /// Insert a budget with explicit auto_renew / next_renewal_at / type / closed /
    /// archived state. Returns its id.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn seed_budget(
        pool: &PgPool,
        owner: Uuid,
        time_frame: &str,
        budget_type: &str,
        auto_renew: bool,
        next_renewal_at: Option<DateTime<Utc>>,
        closed: bool,
        archived: bool,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets \
             (id, owner_id, name, time_frame, budget_type, auto_renew, next_renewal_at, closed_at, archived_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(owner)
        .bind(format!("renew-test-{id}"))
        .bind(time_frame)
        .bind(budget_type)
        .bind(auto_renew)
        .bind(next_renewal_at)
        .bind(if closed { Some(Utc::now()) } else { None })
        .bind(if archived { Some(Utc::now()) } else { None })
        .execute(pool)
        .await
        .expect("seed budget");
        id
    }

    #[cfg(test)]
    async fn marker_of(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
        sqlx::query_scalar("SELECT next_renewal_at FROM budgets WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch marker")
    }

    #[cfg(test)]
    async fn cleanup(pool: &PgPool, user_id: Uuid) {
        // audit_logs FK -> budgets; delete audits via the budgets we own, then
        // budgets, then the user.
        sqlx::query(
            "DELETE FROM audit_logs WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)",
        )
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
        sqlx::query("DELETE FROM budgets WHERE owner_id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .ok();
    }

    /// Insert an expense category on `budget_id` with explicit fund state.
    /// Returns its id. Mirrors `seed_budget`'s shape for fund-category tests.
    #[cfg(test)]
    async fn seed_fund_category(
        pool: &PgPool,
        budget_id: Uuid,
        limit: f64,
        fund_balance: f64,
        fund_advanced_through: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories \
             (id, budget_id, name, category_type, category_limit, is_fund, fund_balance, fund_advanced_through) \
             VALUES ($1, $2, $3, 'expense', $4, TRUE, $5, $6)",
        )
        .bind(id)
        .bind(budget_id)
        .bind(format!("fund-cat-{id}"))
        .bind(limit)
        .bind(fund_balance)
        .bind(fund_advanced_through)
        .execute(pool)
        .await
        .expect("seed fund category");
        id
    }

    #[cfg(test)]
    async fn fund_state_of(pool: &PgPool, id: Uuid) -> (f64, DateTime<Utc>) {
        let row = sqlx::query("SELECT fund_balance, fund_advanced_through FROM categories WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch fund state");
        (
            row.get("fund_balance"),
            row.get::<Option<DateTime<Utc>>, _>("fund_advanced_through").expect("marker set"),
        )
    }

    // --- Rollup (#52) DB-backed test helpers ---

    /// The test database URL: `DATABASE_URL` if set, else the local pgvector
    /// container documented in AGENTS.md. Hoisted so the restricted-role helper
    /// below derives its own URL from the SAME source the normal pool uses.
    #[cfg(test)]
    fn test_db_url() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        })
    }

    /// Connect to the test DB and seed a fresh user, returning (pool, user_id).
    #[cfg(test)]
    async fn rollup_test_setup() -> (PgPool, Uuid) {
        let url = test_db_url();
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("rollup-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        (pool, user_id)
    }

    /// Seed a time-based budget owned by `owner` with a single expense category
    /// (limit `cat_limit`) and one expense transaction dated `now` for `spent`.
    /// `archived` controls archived_at. Returns the budget id. The current-period
    /// spend window contains `now`, so the seeded transaction counts toward the
    /// aggregated current-period spend.
    #[cfg(test)]
    async fn seed_rollup_budget(
        pool: &PgPool,
        owner: Uuid,
        cat_limit: f64,
        spent: f64,
        archived: bool,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, archived_at) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', $4)",
        )
        .bind(id)
        .bind(owner)
        .bind(format!("rollup-budget-{id}"))
        .bind(if archived { Some(Utc::now()) } else { None })
        .execute(pool)
        .await
        .expect("seed budget");

        let cat_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Food', 'expense', $3)",
        )
        .bind(cat_id)
        .bind(id)
        .bind(cat_limit)
        .execute(pool)
        .await
        .expect("seed category");

        if spent != 0.0 {
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
                 VALUES ($1, $2, $3, $4, now(), 'seed spend')",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(cat_id)
            .bind(spent)
            .execute(pool)
            .await
            .expect("seed transaction");
        }
        id
    }

    /// Tear down every budget owned by the given users plus their dependent rows.
    #[cfg(test)]
    async fn rollup_cleanup(pool: &PgPool, users: &[Uuid]) {
        for &u in users {
            // Clear rollup links first so no FK references survive the budget delete.
            sqlx::query("UPDATE budgets SET rollup_parent_id = NULL WHERE owner_id = $1")
                .bind(u).execute(pool).await.ok();
        }
        for &u in users {
            sqlx::query("DELETE FROM transactions WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)")
                .bind(u).execute(pool).await.ok();
            sqlx::query("DELETE FROM categories WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)")
                .bind(u).execute(pool).await.ok();
            sqlx::query("DELETE FROM audit_logs WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)")
                .bind(u).execute(pool).await.ok();
            sqlx::query("DELETE FROM budgets WHERE owner_id = $1").bind(u).execute(pool).await.ok();
            sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(pool).await.ok();
        }
    }

    #[cfg(test)]
    fn test_state(pool: &PgPool) -> AppState {
        AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    /// Build a SECOND pool, connected as a throwaway login role that can read and
    /// write everything `list_categories` / `update_category` touch EXCEPT
    /// `transactions`. That makes `period_category_expense_spent_many` — the only
    /// query in either handler that reads `transactions` — fail with a permission
    /// error while every other statement succeeds, which is precisely the partial
    /// DB failure #432 is about. Returns `(pool, role_name)`.
    ///
    /// A fresh role starts with ZERO table privileges, so there is no REVOKE
    /// here: `transactions` is simply never granted. That invariant holds only
    /// while no migration issues `GRANT ... TO PUBLIC` or `ALTER DEFAULT
    /// PRIVILEGES` covering these tables — if the fault leg ever stops failing,
    /// grep `backend/migrations` for those two before suspecting anything else.
    ///
    /// The grant list is ENUMERATED rather than `GRANT ... ON ALL TABLES IN
    /// SCHEMA public` for two reasons, neither of them locking: a blanket grant
    /// would sweep in `transactions` and silently un-inject the fault, and `ON
    /// ALL TABLES` additionally errors on any table the connecting role neither
    /// owns nor holds GRANT OPTION on (a non-owner run stops on
    /// `_sqlx_migrations`; the local superuser happens to get away with it).
    ///
    /// `lock_timeout` is cheap insurance against CATALOG contention, NOT against
    /// a table-lock hang — the latter cannot happen. `GRANT` takes no lock at all
    /// on the tables it names: probed on PG16, a `GRANT` on `budgets` completes
    /// in single-digit milliseconds while a second session holds `LOCK TABLE
    /// budgets IN ROW EXCLUSIVE MODE`, and `pg_locks` for the granting backend
    /// shows only `AccessShareLock` on `pg_class` and its indexes. What it DOES
    /// contend on is the `pg_class` tuple it rewrites, so two concurrent `GRANT`s
    /// naming the same table serialize on it; with `lock_timeout` set, that
    /// surfaces here as a loud "canceling statement due to lock timeout ... while
    /// updating tuple in relation pg_class" instead of a stall the `--ignored`
    /// suite would blame on some unrelated test.
    #[cfg(test)]
    async fn restricted_pool_denied_transactions_select(admin: &PgPool) -> (PgPool, String) {
        // Lowercase and 43 chars, so the name is under Postgres' 63-byte
        // identifier cap and needs no quoting when interpolated into DDL. A fresh
        // UUID each call, so it cannot collide with a live or stray role and needs
        // no `DROP ROLE IF EXISTS` ahead of the CREATE.
        let role = format!("nels_test_r{}", Uuid::new_v4().simple());

        // The literal password below is not a secret: it only ever authenticates
        // to the local throwaway test cluster, and the role holds nothing but
        // SELECT/INSERT/UPDATE/DELETE on six tables — no superuser, no CREATE, and
        // specifically no access to `transactions`.
        sqlx::query(&format!("CREATE ROLE {role} LOGIN PASSWORD 'restricted'"))
            .execute(admin)
            .await
            .expect("create restricted role");

        // From here on the role EXISTS, so every fallible step runs inside this
        // block and returns `Err` instead of panicking in place. The `Err` arm
        // below drops the role and only THEN panics, so the helper cleans up after
        // its own failures — including its documented one, a `lock_timeout` trip
        // on the GRANT, which previously leaked the role. Failures stay LOUD: the
        // error is re-raised as a panic, never softened into a silent skip.
        let built: Result<PgPool, String> = async {
            // One pooled CONNECTION for the whole SET / GRANT / reset sequence:
            // `SET` is session-scoped, so issuing it against the pool could land on
            // a different connection than the GRANT and silently do nothing.
            {
                let mut conn = admin
                    .acquire()
                    .await
                    .map_err(|e| format!("acquire admin connection: {e}"))?;
                sqlx::query("SET lock_timeout = '10s'")
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| format!("set lock_timeout: {e}"))?;
                sqlx::query(&format!("GRANT USAGE ON SCHEMA public TO {role}"))
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| format!("grant schema usage: {e}"))?;
                sqlx::query(&format!(
                    "GRANT SELECT, INSERT, UPDATE, DELETE ON \
                     budgets, categories, users, budget_shares, subscriptions, audit_logs TO {role}"
                ))
                .execute(&mut *conn)
                .await
                .map_err(|e| format!("grant table privileges: {e}"))?;
                sqlx::query("SET lock_timeout = DEFAULT")
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| format!("reset lock_timeout: {e}"))?;
            }

            // Rebuild the URL by splitting on the LAST '@': everything before it is
            // scheme + credentials (which may itself contain '@' inside a password,
            // or carry no password at all), everything after it is host/port/db and
            // is kept verbatim. A `user:pass@` string-replace would break on all
            // three of those shapes.
            let url = test_db_url();
            let suffix = url
                .rsplit_once('@')
                .map(|(_, host)| host.to_string())
                .unwrap_or_else(|| {
                    url.split_once("://")
                        .map(|(_, rest)| rest.to_string())
                        .expect("DATABASE_URL has a scheme")
                });
            PgPool::connect(&format!("postgres://{role}:restricted@{suffix}"))
                .await
                .map_err(|e| format!("connect as restricted role: {e}"))
        }
        .await;

        match built {
            Ok(pool) => (pool, role),
            Err(e) => {
                sqlx::query(&format!("DROP OWNED BY {role}")).execute(admin).await.ok();
                sqlx::query(&format!("DROP ROLE IF EXISTS {role}")).execute(admin).await.ok();
                panic!("restricted role setup failed (role {role} dropped): {e}");
            }
        }
    }

    /// Close the restricted pool and drop its role.
    ///
    /// `DROP OWNED BY` must run BEFORE `DROP ROLE`: a role still holding table
    /// grants cannot be dropped ("cannot be dropped because some objects depend
    /// on it"). Neither statement is allowed to panic — turning teardown into a
    /// second failure would mask the real one — but both LOG on the error branch,
    /// so a genuine stray is findable instead of invisible.
    ///
    /// A Postgres role is CLUSTER-GLOBAL, so it outlives the test process. The
    /// discipline that keeps it from leaking is: bind the handler's `Result`, tear
    /// down, and only THEN assert on it. Do not move an assertion back above the
    /// teardown. `Drop` is not an escape hatch — panics DO unwind here (there is
    /// no `panic = "abort"` in `backend/Cargo.toml`) and `Drop` impls DO run, but
    /// `Drop` cannot `await`, so an async `DROP ROLE` cannot be expressed as an
    /// RAII guard; there is deliberately no such guard to "fix".
    ///
    /// That covers assertion panics in the test BODY;
    /// `restricted_pool_denied_transactions_select` separately cleans up after its
    /// own setup failures. Neither covers a hard kill (SIGKILL, `--nocapture`
    /// abort). After one, list strays with
    /// `SELECT rolname FROM pg_roles WHERE rolname LIKE 'nels_test_r%'` and
    /// `DROP OWNED BY <r>; DROP ROLE <r>;` each. A stray is low-risk because this
    /// is a LOCAL THROWAWAY cluster — not because it is harmless in itself: it
    /// retains SELECT/INSERT/UPDATE/DELETE on six tables including `users` and
    /// `budgets`, and logs in with a hardcoded password. If this helper is ever
    /// pointed at a shared or CI database, that trade no longer holds.
    #[cfg(test)]
    async fn drop_restricted_role(admin: &PgPool, pool: PgPool, role: &str) {
        pool.close().await;
        if let Err(e) = sqlx::query(&format!("DROP OWNED BY {role}")).execute(admin).await {
            eprintln!("teardown: DROP OWNED BY {role} failed: {e}");
        }
        if let Err(e) = sqlx::query(&format!("DROP ROLE IF EXISTS {role}")).execute(admin).await {
            eprintln!("teardown: DROP ROLE {role} failed (role may be stray): {e}");
        }
    }

    /// Seed an already-entitled ('active') subscription for `user`, so that
    /// `create_budget`'s first-budget trial gate (`ensure_ready_to_own_budget`)
    /// takes the "entitled -> Ok" branch instead of "never subscribed -> start a
    /// real Stripe trial". With no STRIPE_PRICE_* env vars set in the test
    /// process, `entitlement::resolve("active", None, <empty catalog>)`
    /// fail-safes to `Tier::Basic` (never `Tier::None`), so this never triggers
    /// a Stripe call. Used by create_budget tests whose users predate the gate
    /// and would otherwise hit it as "first budget" (#billing two-tier).
    #[cfg(test)]
    async fn seed_active_sub(pool: &PgPool, user: Uuid) {
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'active')",
        )
        .bind(user)
        .bind(format!("cus_{user}"))
        .execute(pool)
        .await
        .expect("seed active subscription");
    }

    // The Stripe HTTP client (billing::stripe_post) reads these env vars at
    // call time; point it at a wiremock server for the trial-gate tests below
    // and clear them after so nothing leaks across tests in the same binary
    // (mirrors billing::tests::set_stripe_env/clear_stripe_env).
    #[cfg(test)]
    fn set_stripe_env(server: &MockServer) {
        std::env::set_var("STRIPE_API_BASE", server.uri());
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
    }

    #[cfg(test)]
    fn clear_stripe_env() {
        std::env::remove_var("STRIPE_API_BASE");
        std::env::remove_var("STRIPE_SECRET_KEY");
    }

    fn minimal_budget_payload(name: &str) -> BudgetPayload {
        BudgetPayload {
            name: name.to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: None,
            auto_renew: None,
            amount_mode: None,
            budget_strategy: None,
            currency: None,
        }
    }

    // Task 4 (billing two-tier): the REST create_budget handler now runs
    // ensure_ready_to_own_budget before the insert. A brand-new user (no
    // budgets, no subscription row) creating their first budget must trigger a
    // Stripe trial (mocked here) and still end up with the budget row created.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial]
    async fn create_budget_first_budget_starts_trial_and_creates_budget() {
        let server = MockServer::start().await;
        set_stripe_env(&server);
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        Mock::given(method("POST"))
            .and(path("v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_new"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("v1/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"sub_new","status":"trialing"})))
            .expect(1)
            .mount(&server)
            .await;

        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);

        let created = create_budget(
            State(state),
            Extension(user),
            Json(minimal_budget_payload("Trial-Gated Budget")),
        )
        .await
        .expect("create_budget should succeed after starting the trial");

        let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM budgets WHERE owner_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("count budgets");
        assert_eq!(owned, 1, "the budget row must be created");
        assert_eq!(created.0.name, "Trial-Gated Budget");

        clear_stripe_env();
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
        rollup_cleanup(&pool, &[user]).await;
        // wiremock verifies .expect(1) on the subscriptions POST at drop
    }

    // Task 4 (billing two-tier): a user whose subscription already lapsed
    // (status='canceled') gets no second free trial — create_budget must
    // return 402 and must NOT write a budget row.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_budget_lapsed_subscription_is_blocked_and_writes_no_budget() {
        let (pool, user) = rollup_test_setup().await;
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status, stripe_subscription_id) VALUES ($1, $2, 'canceled', 'sub_old')",
        )
        .bind(user)
        .bind(format!("cus_{user}"))
        .execute(&pool)
        .await
        .expect("seed lapsed subscription");
        let state = test_state(&pool);

        let err = create_budget(
            State(state),
            Extension(user),
            Json(minimal_budget_payload("Should Not Exist")),
        )
        .await
        .expect_err("lapsed subscriber must be blocked");
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);

        let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM budgets WHERE owner_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("count budgets");
        assert_eq!(owned, 0, "no budget row must be written when the gate blocks creation");

        rollup_cleanup(&pool, &[user]).await;
    }

    /// Seed one category of the given type/limit under `budget` with a
    /// collision-proof (uuid-suffixed) name. Zero-based aggregate tests (#358).
    #[cfg(test)]
    async fn seed_typed_category(pool: &PgPool, budget: Uuid, ctype: &str, limit: f64) {
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(budget)
        .bind(format!("{ctype}-{}", Uuid::new_v4()))
        .bind(ctype)
        .bind(limit)
        .execute(pool)
        .await
        .expect("seed typed category");
    }

    /// Set the child->parent rollup backlink directly (#396 visibility/auth tests
    /// depend on the backlink, not the mirror category).
    #[cfg(test)]
    async fn link_child(pool: &PgPool, parent: Uuid, child: Uuid) {
        sqlx::query("UPDATE budgets SET rollup_parent_id = $1 WHERE id = $2")
            .bind(parent)
            .bind(child)
            .execute(pool)
            .await
            .expect("seed rollup backlink");
    }

    /// Share `budget` with `user` at `level` ("view"/"edit").
    #[cfg(test)]
    async fn share_budget_with(pool: &PgPool, budget: Uuid, user: Uuid, level: &str) {
        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(pool)
            .await
            .expect("fetch user email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(budget)
        .bind(&email)
        .bind(level)
        .execute(pool)
        .await
        .expect("seed share");
    }

    // #396: a collaborator with access to a shared PARENT inherits access to the
    // budgets rolled up into it (single-level via rollup_parent_id). Inheritance is
    // capped at Edit; direct + inherited resolve to the higher level.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_permission_grants_view_through_shared_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;

        let perm = check_permission(&pool, viewer, child)
            .await
            .expect("check_permission");
        assert_eq!(
            perm,
            Permission::View,
            "view-shared parent grants view on child"
        );
        // Was Permission::None before #396.
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_permission_grants_edit_through_shared_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "edit").await;

        let perm = check_permission(&pool, viewer, child)
            .await
            .expect("check_permission");
        assert_eq!(
            perm,
            Permission::Edit,
            "edit-shared parent grants edit on child"
        );
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_permission_caps_inherited_at_edit() {
        // Even an edit-shared parent must not confer Owner: owner-only child ops stay 403.
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "edit").await;

        let perm = check_permission(&pool, viewer, child)
            .await
            .expect("check_permission");
        assert_eq!(
            perm,
            Permission::Edit,
            "edit-shared parent grants Edit on the child"
        );
        assert!(
            perm != Permission::Owner,
            "inherited access never confers Owner"
        );
        // Integration: delete_budget is owner-only -> must still 403 for the sharee.
        let state = test_state(&pool);
        let res = delete_budget(State(state), Path(child), Extension(viewer)).await;
        let err = res.err().expect("owner-only op must be denied");
        assert_eq!(
            err.0,
            StatusCode::FORBIDDEN,
            "sharee cannot delete an inherited child"
        );
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_permission_direct_share_wins_when_higher() {
        // Child directly shared 'edit', parent shared 'view' -> higher (edit) wins.
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;
        share_budget_with(&pool, child, viewer, "edit").await;

        let perm = check_permission(&pool, viewer, child)
            .await
            .expect("check_permission");
        assert_eq!(
            perm,
            Permission::Edit,
            "higher of direct(edit) and inherited(view)"
        );
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_permission_denies_unrelated_child() {
        // A child with no direct share and no shared parent stays None (403).
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        // No share of parent OR child with `stranger`.

        let perm = check_permission(&pool, stranger, child)
            .await
            .expect("check_permission");
        assert_eq!(perm, Permission::None, "no over-broad grant");
        rollup_cleanup(&pool, &[owner, stranger]).await;
    }

    // #396: list_budgets surfaces the children of a SHARED parent as their own rows.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_includes_child_of_shared_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        // users.name is NULL by default (rollup_test_setup seeds only id/email/totp);
        // set it so the owner_name pass-through has a deterministic value to assert.
        sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
            .bind("Ollie Owner").bind(owner).execute(&pool).await.expect("set owner name");
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        let p = list.iter().find(|b| b.id == parent).expect("shared parent present");
        let c = list.iter().find(|b| b.id == child).expect("child of shared parent present");
        assert!(!p.is_owner && !c.is_owner, "both are viewer-shared, not owned");
        assert_eq!(c.permission_level, "view", "child inherits the parent's view level");
        assert_eq!(
            c.owner_name,
            Some("Ollie Owner".to_string()),
            "inherited child carries its owner's display name"
        );
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_child_inherits_edit_from_shared_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "edit").await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        let c = list.iter().find(|b| b.id == child).expect("child present");
        assert_eq!(c.permission_level, "edit", "child inherits the parent's edit level");
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_child_direct_share_wins_over_lower_parent_share() {
        // SC5: child directly shared 'view', parent shared 'edit' -> higher (edit) wins
        // on the child's list row (and vice-versa direction is covered by the unit test).
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "edit").await;
        share_budget_with(&pool, child, viewer, "view").await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        let c = list.iter().find(|b| b.id == child).expect("child present");
        assert_eq!(c.permission_level, "edit", "higher of direct(view) and inherited(edit)");
        // The child must appear exactly once (directly-shared child not duplicated by the
        // inherited-children query — it is deduped via existing_ids).
        assert_eq!(list.iter().filter(|b| b.id == child).count(), 1, "child listed once");
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_excludes_unrelated_child() {
        // A child whose parent is NOT shared with the viewer never surfaces.
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        // Nothing shared with `stranger`.

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(stranger),
        )
        .await
        .expect("list_budgets must succeed");

        assert!(list.iter().all(|b| b.id != child), "unrelated child must not surface");
        rollup_cleanup(&pool, &[owner, stranger]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_excludes_archived_child_of_shared_parent() {
        // #396: an ARCHIVED child of an active shared parent must not surface in the
        // default (active-only) view — the inherited-children rows are archive-filtered.
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, true).await; // archived
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        assert!(list.iter().any(|b| b.id == parent), "active shared parent present");
        assert!(list.iter().all(|b| b.id != child), "archived child excluded from active view");
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_does_not_orphan_child_of_archived_shared_parent() {
        // #396: when the shared PARENT is archived (not in the active-view shared_rows),
        // its active child must not be orphaned into the active view — children only
        // surface when their parent is visible in the same view.
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, true).await; // archived parent
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await; // active child
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        assert!(list.iter().all(|b| b.id != parent), "archived parent absent from active view");
        assert!(list.iter().all(|b| b.id != child), "active child of archived parent not orphaned");
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn get_budget_on_child_of_shared_parent_returns_ok() {
        // SC2: get_budget on the child returns 200 (was 403) for a parent-sharee.
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        link_child(&pool, parent, child).await;
        share_budget_with(&pool, parent, viewer, "view").await;

        let state = test_state(&pool);
        let Json(item) = get_budget(State(state), Path(child), Extension(viewer))
            .await
            .expect("get_budget on inherited child must return 200");
        assert_eq!(item.id, child);
        assert!(!item.is_owner, "inherited child is not owned by the viewer");
        assert_eq!(item.permission_level, "view");
        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #358: computed_income_savings_totals sums a budget's OWN income and savings
    // category limits (null->0) BY TYPE, independent of the mode-aware expense
    // base. Mirrors the rollup DB-test pattern (fresh user via rollup_test_setup,
    // unique UUIDs, rollup_cleanup teardown).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn computed_income_savings_totals_sums_by_type() {
        let (pool, user) = rollup_test_setup().await;

        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, budget_strategy) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', 'zero_based')",
        )
        .bind(budget_id)
        .bind(user)
        .bind(format!("zb-{budget_id}"))
        .execute(&pool)
        .await
        .expect("seed zero_based budget");

        // income = 14906 (14000 + 906), savings = 385, expense = 8062.
        seed_typed_category(&pool, budget_id, "income", 14000.0).await;
        seed_typed_category(&pool, budget_id, "income", 906.0).await;
        seed_typed_category(&pool, budget_id, "savings", 385.0).await;
        seed_typed_category(&pool, budget_id, "expense", 8062.0).await;

        let totals = computed_income_savings_totals(&pool, budget_id)
            .await
            .expect("income/savings totals");
        assert_eq!(totals, (14906.0, 385.0), "income sums income rows; savings sums savings rows");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #358: get_budget surfaces the zero-based strip aggregates — Available is the
    // own income total; Allocated is the mode-aware expense base + own savings.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn get_budget_exposes_zero_based_aggregates() {
        let (pool, user) = rollup_test_setup().await;

        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, budget_strategy) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', 'zero_based')",
        )
        .bind(budget_id)
        .bind(user)
        .bind(format!("zb-{budget_id}"))
        .execute(&pool)
        .await
        .expect("seed zero_based budget");

        // income 14906, savings 385, expense 8062 -> allocated = 8062 + 385 = 8447.
        seed_typed_category(&pool, budget_id, "income", 14906.0).await;
        seed_typed_category(&pool, budget_id, "savings", 385.0).await;
        seed_typed_category(&pool, budget_id, "expense", 8062.0).await;

        let state = test_state(&pool);
        let Json(item) = get_budget(State(state), Path(budget_id), Extension(user))
            .await
            .expect("get_budget must succeed");

        assert_eq!(item.aggregated_income_amount, 14906.0, "Available = own income total");
        // The frontend derives Allocated = base + savings; assert both own-only
        // components so 8062 + 385 = 8447 is still verifiable here.
        assert_eq!(item.aggregated_base_amount, 8062.0, "mode-aware expense base");
        assert_eq!(item.aggregated_savings_amount, 385.0, "own savings total");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #358: for a rollup PARENT, the base must fold the mirrored child's expense
    // total (mirror-inclusive), while savings stays own-only (savings never rolls
    // up). The frontend derives Allocated = aggregated_base_amount + own savings.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn rollup_parent_base_is_mirror_inclusive_savings_own_only() {
        let (pool, user) = rollup_test_setup().await;

        // Parent owns one expense category (limit 100). Source owns expense 200.
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;

        // Parent also owns a savings category (own-only, must NOT roll up).
        seed_typed_category(&pool, parent, "savings", 50.0).await;

        // Mirror category in the parent pointing at the source (expense-type).
        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Child (rolled up)', 'expense', NULL, $3)",
        )
        .bind(mirror_id)
        .bind(parent)
        .bind(source)
        .execute(&pool)
        .await
        .expect("seed mirror category");

        let state = test_state(&pool);
        let Json(item) = get_budget(State(state), Path(parent), Extension(user))
            .await
            .expect("get_budget must succeed");

        // Base = own expense 100 + mirrored source 200 = 300. Savings (own) = 50.
        assert_eq!(item.aggregated_base_amount, 300.0, "base is mirror-inclusive");
        assert_eq!(
            item.aggregated_savings_amount, 50.0,
            "savings is own-only (never rolls up); frontend Allocated = base + savings = 350"
        );

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_id).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // Linked-category rollup (#52): a parent mirrors a SOURCE budget through a
    // single mirror expense category (`linked_budget_id` set). `computed_budget_total`
    // resolves that mirror one level deep to the source's OWN expense total, so the
    // parent's own total = its own categories PLUS the mirrored source total. Because
    // `aggregated_budget_amounts` now returns the parent's OWN amounts (base built on
    // `computed_budget_total`), the mirror is already folded into base — no separate
    // child-summing loop, and thus no double-counting.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn mirror_category_includes_source_total_in_parent() {
        let (pool, user) = rollup_test_setup().await;

        // Parent owns one expense category (limit 100). Source ("child") owns its own
        // expense categories totaling 200.
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;

        // Create the MIRROR category in the parent pointing at the source. Its own
        // limit is NULL; it contributes the source's own expense total instead.
        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Child (rolled up)', 'expense', NULL, $3)",
        )
        .bind(mirror_id)
        .bind(parent)
        .bind(source)
        .execute(&pool)
        .await
        .expect("seed mirror category");

        // Parent's own total = own 100 + mirrored source 200.
        let total = computed_budget_total(&pool, parent).await.expect("computed total");
        assert_eq!(total, 300.0, "parent total = own 100 + mirrored source 200");

        // `aggregated_budget_amounts` returns the parent's OWN amounts; its base is
        // built on `computed_budget_total`, so the mirror is already included.
        let parent_row = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
            .bind(parent).fetch_one(&pool).await.expect("parent row");
        let (own_base, _own_effective, _own_spent) =
            aggregated_budget_amounts(&pool, &parent_row).await.expect("aggregate");
        assert_eq!(own_base, 300.0, "own base includes the mirrored source total");

        // Clean up the rows we inserted (and the seeded users/budgets).
        sqlx::query("DELETE FROM categories WHERE id = $1")
            .bind(mirror_id).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // nels#298: category_table_rows's mirror row must resolve to the SOURCE's
    // own aggregate limit/spend (the same figures computed_budget_total and
    // linked_budgets_spent already compute), not the mirror category's own
    // (always-NULL/never-transacted) columns.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_table_rows_resolves_mirror_to_source_aggregate() {
        let (pool, user) = rollup_test_setup().await;

        // Parent has its own category (limit 100, no spend — irrelevant to the
        // mirror assertion below). Source has its own limit 300 and 120 of
        // in-window spend.
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 300.0, 120.0, false).await;

        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Child (rolled up)', 'expense', NULL, $3)",
        )
        .bind(mirror_id)
        .bind(parent)
        .bind(source)
        .execute(&pool)
        .await
        .expect("seed mirror category");

        let rows = category_table_rows(&pool, parent).await.expect("aggregate rows");
        let mirror = rows
            .iter()
            .find(|r| r.linked_budget_id == Some(source))
            .expect("mirror row present");
        assert_eq!(mirror.category_limit, Some(300.0), "mirror limit resolves to the source's aggregate total");
        assert_eq!(mirror.spent, 120.0, "mirror spend resolves to the source's own in-window spend");

        // The parent's OWN (non-mirror) row is untouched by this fix.
        let own = rows
            .iter()
            .find(|r| r.linked_budget_id.is_none())
            .expect("parent's own row present");
        assert_eq!(own.category_limit, Some(100.0), "an ordinary category's limit is unaffected by mirror resolution");
        assert_eq!(own.spent, 0.0);

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_id).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // nels#298 edge case: an ARCHIVED rollup source must resolve to $0 for
    // both limit and spend on its mirror row — matching the #52 "archived
    // children don't count on either side" precedent already enforced by
    // computed_budget_total (limit) and linked_budgets_spent (spend) — not
    // leave the pre-fix "—"/stale-looking state, and not leak the archived
    // source's real (non-zero) figures either.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_table_rows_archived_mirror_source_resolves_to_zero() {
        let (pool, user) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        // Archived source with a real limit (500) and real spend (200) — both
        // must be suppressed to 0 on the parent's mirror row.
        let source = seed_rollup_budget(&pool, user, 500.0, 200.0, true).await;

        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Archived Mirror', 'expense', NULL, $3)",
        )
        .bind(mirror_id)
        .bind(parent)
        .bind(source)
        .execute(&pool)
        .await
        .expect("seed mirror category");

        let rows = category_table_rows(&pool, parent).await.expect("aggregate rows");
        let mirror = rows
            .iter()
            .find(|r| r.linked_budget_id == Some(source))
            .expect("mirror row present");
        assert_eq!(mirror.category_limit, Some(0.0), "archived source's mirror limit resolves to $0, not its real total");
        assert_eq!(mirror.spent, 0.0, "archived source's mirror spend resolves to $0, not its real spend");

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_id).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // nels#298: with TWO mirrors on one parent, each must resolve independently
    // to its OWN source's figures — the batched maps in
    // resolve_linked_source_totals_and_spend must key correctly per source id,
    // not conflate or cross-contaminate distinct sources' limit/spend.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_table_rows_resolves_multiple_mirrors_independently() {
        let (pool, user) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source_a = seed_rollup_budget(&pool, user, 300.0, 120.0, false).await;
        let source_b = seed_rollup_budget(&pool, user, 700.0, 50.0, false).await;

        let mirror_a = Uuid::new_v4();
        let mirror_b = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Mirror A', 'expense', NULL, $3)",
        )
        .bind(mirror_a).bind(parent).bind(source_a).execute(&pool).await.expect("seed mirror A");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Mirror B', 'expense', NULL, $3)",
        )
        .bind(mirror_b).bind(parent).bind(source_b).execute(&pool).await.expect("seed mirror B");

        let rows = category_table_rows(&pool, parent).await.expect("aggregate rows");
        let resolved_a = rows.iter().find(|r| r.linked_budget_id == Some(source_a)).expect("mirror A present");
        let resolved_b = rows.iter().find(|r| r.linked_budget_id == Some(source_b)).expect("mirror B present");
        assert_eq!(resolved_a.category_limit, Some(300.0), "mirror A resolves to source A's own limit");
        assert_eq!(resolved_a.spent, 120.0, "mirror A resolves to source A's own spend");
        assert_eq!(resolved_b.category_limit, Some(700.0), "mirror B resolves to source B's own limit, not source A's");
        assert_eq!(resolved_b.spent, 50.0, "mirror B resolves to source B's own spend, not source A's");

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_a).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_b).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // Per-budget amount mode (#116): a 'fixed' budget reports its stored
    // budget_limit as the base, ignoring its category sum; flipping it back to
    // 'derived' restores the summed-category behavior.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn computed_budget_total_honors_amount_mode() {
        let (pool, user) = rollup_test_setup().await;

        // Budget with one expense category limit=100. Make it 'fixed' with
        // budget_limit=2000.
        let id = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET amount_mode = 'fixed', budget_limit = 2000.0 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("set fixed mode");

        let fixed_total = computed_budget_total(&pool, id).await.expect("fixed total");
        assert_eq!(fixed_total, 2000.0, "fixed budget reports its budget_limit");

        // Flip back to 'derived' -> reports the category sum (100).
        sqlx::query("UPDATE budgets SET amount_mode = 'derived' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("set derived mode");

        let derived_total = computed_budget_total(&pool, id).await.expect("derived total");
        assert_eq!(derived_total, 100.0, "derived budget reports the category sum");

        rollup_cleanup(&pool, &[user]).await;
    }

    // REST write path (#116): create_budget persists a supplied amount_mode, and
    // update_budget preserves it on absence (COALESCE) yet applies it when present.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn rest_create_and_update_persist_amount_mode() {
        let (pool, user) = rollup_test_setup().await;
        // This user owns no budget yet; seed an entitled subscription so the
        // first-budget trial gate takes the "entitled -> Ok" branch instead of
        // starting a real Stripe trial (no wiremock in this test).
        seed_active_sub(&pool, user).await;
        let state = test_state(&pool);

        async fn stored_mode(pool: &PgPool, id: Uuid) -> String {
            sqlx::query_scalar("SELECT amount_mode FROM budgets WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch amount_mode")
        }

        // create_budget with amount_mode='fixed' persists it.
        let created = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Fixed Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: Some(2000.0),
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: Some("fixed".to_string()),
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("create_budget Ok");
        let budget_id = created.0.id;
        assert_eq!(stored_mode(&pool, budget_id).await, "fixed", "create persists amount_mode");
        assert_eq!(created.0.amount_mode, "fixed", "response reflects the stored mode");

        // update_budget with amount_mode ABSENT preserves the existing value (COALESCE).
        update_budget(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(BudgetPayload {
                name: "Fixed Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: Some(2000.0),
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("update_budget (absent mode) Ok");
        assert_eq!(
            stored_mode(&pool, budget_id).await,
            "fixed",
            "an update with amount_mode absent must preserve the existing mode"
        );

        // update_budget with amount_mode='derived' applies it.
        update_budget(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(BudgetPayload {
                name: "Fixed Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: Some(2000.0),
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: Some("derived".to_string()),
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("update_budget (derived) Ok");
        assert_eq!(stored_mode(&pool, budget_id).await, "derived", "update applies the new mode");

        // create_budget with an invalid amount_mode is rejected with 400.
        let bad = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Bad Mode".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: Some("FIXED".to_string()),
                budget_strategy: None,
                currency: None,
            }),
        )
        .await;
        assert!(matches!(bad, Err((StatusCode::BAD_REQUEST, _))), "invalid mode -> 400");

        rollup_cleanup(&pool, &[user]).await;
    }

    // REST write path (#300): create_budget defaults an absent budget_strategy to
    // 'limit_spent_remaining', persists a supplied one, and rejects an unknown one.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_budget_persists_and_defaults_budget_strategy() {
        let (pool, user) = rollup_test_setup().await;
        // First-budget trial gate: seed an entitled subscription (see
        // seed_active_sub) so create_budget doesn't hit a real Stripe call.
        seed_active_sub(&pool, user).await;
        let state = test_state(&pool);

        async fn stored_strategy(pool: &PgPool, id: Uuid) -> String {
            sqlx::query_scalar("SELECT budget_strategy FROM budgets WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch budget_strategy")
        }

        // create_budget with budget_strategy ABSENT defaults to 'limit_spent_remaining'.
        let defaulted = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Default Strategy Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("create_budget Ok");
        assert_eq!(
            stored_strategy(&pool, defaulted.0.id).await,
            "limit_spent_remaining",
            "absent budget_strategy defaults to limit_spent_remaining"
        );
        assert_eq!(defaulted.0.budget_strategy, "limit_spent_remaining");

        // create_budget with budget_strategy='zero_based' persists it.
        let zero_based = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Zero Based Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("zero_based".to_string()),
                currency: None,
            }),
        )
        .await
        .expect("create_budget Ok");
        assert_eq!(stored_strategy(&pool, zero_based.0.id).await, "zero_based");
        assert_eq!(zero_based.0.budget_strategy, "zero_based");

        // create_budget with an invalid budget_strategy is rejected with 400.
        let bad = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Bad Strategy Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("fifty_thirty_twenty".to_string()),
                currency: None,
            }),
        )
        .await;
        assert!(matches!(bad, Err((StatusCode::BAD_REQUEST, _))), "invalid strategy -> 400");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #391 FINDING 1 regression guard: create_budget must also point the
    // creator's per-viewer preference (users.active_budget_id, #255) at the new
    // budget, not just flip is_default. Before the fix, a user whose preference
    // was already set to a DIFFERENT budget (e.g. via /activate on the list
    // page) would create a new budget that flipped is_default but left the
    // stale preference in place — resolve_active_budget_id (which checks the
    // preference first) would then keep resolving to the OLD budget.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_budget_updates_active_budget_id_preference() {
        let (pool, user) = rollup_test_setup().await;
        let old_budget = seed_rollup_budget(&pool, user, 50.0, 0.0, false).await;

        // Preference already set to a DIFFERENT budget than is_default targets.
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(old_budget)
            .bind(user)
            .execute(&pool)
            .await
            .expect("set initial preference");

        let state = test_state(&pool);
        let created = create_budget(
            State(state),
            Extension(user),
            Json(BudgetPayload {
                name: "New Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("create_budget Ok");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(
            prefs,
            Some(created.0.id),
            "the preference must follow the new budget, not stay stuck on the old one"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // #391 FINDING 1 regression guard: delete_budget's promotion of a new
    // default must also restore the owner's per-viewer preference to the
    // promoted budget. The FK's ON DELETE SET NULL already clears the
    // preference when it pointed at the deleted budget, so without this the
    // owner is left with NO active budget at all until they explicitly
    // /activate one, even though is_default (and the REST response) say a new
    // budget is now the default.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_budget_promotion_restores_active_budget_id_preference() {
        let (pool, user) = rollup_test_setup().await;
        let deleted = seed_rollup_budget(&pool, user, 50.0, 0.0, false).await;
        let survivor = seed_rollup_budget(&pool, user, 75.0, 0.0, false).await;

        sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
            .bind(deleted)
            .execute(&pool)
            .await
            .expect("mark deleted budget as default");
        sqlx::query("UPDATE budgets SET is_default = FALSE WHERE id = $1")
            .bind(survivor)
            .execute(&pool)
            .await
            .expect("clear survivor default");
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(deleted)
            .bind(user)
            .execute(&pool)
            .await
            .expect("set preference to the soon-to-be-deleted budget");

        let state = test_state(&pool);
        delete_budget(State(state), Path(deleted), Extension(user))
            .await
            .expect("delete_budget Ok");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(
            prefs,
            Some(survivor),
            "the preference must be restored to the promoted budget, not left NULL by the FK's ON DELETE SET NULL"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // #391 re-review BLOCKING regression guard: the promotion repair must be
    // scoped to active_budget_id IS NULL, not unconditional. When the owner's
    // preference already points somewhere else (e.g. a budget shared with them,
    // or another owned budget — reachable because set_active_budget never
    // touches `budgets`, so is_default can lag the real preference), deleting
    // the stale is_default budget must NOT clobber that preference with the
    // newly-promoted budget. Doing so was the newly-introduced bug: it silently
    // yanked the owner out of whatever they were actually looking at.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_budget_promotion_does_not_clobber_a_preference_pointed_elsewhere() {
        let (pool, user) = rollup_test_setup().await;
        let deleted = seed_rollup_budget(&pool, user, 50.0, 0.0, false).await;
        let survivor = seed_rollup_budget(&pool, user, 75.0, 0.0, false).await;
        let elsewhere = seed_rollup_budget(&pool, user, 30.0, 0.0, false).await;

        sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
            .bind(deleted)
            .execute(&pool)
            .await
            .expect("mark deleted budget as default");
        sqlx::query("UPDATE budgets SET is_default = FALSE WHERE id = ANY($1)")
            .bind(&[survivor, elsewhere][..])
            .execute(&pool)
            .await
            .expect("clear other defaults");
        // The owner's preference points at a THIRD, surviving budget — not the
        // one about to be deleted, and not the one that will be promoted.
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(elsewhere)
            .bind(user)
            .execute(&pool)
            .await
            .expect("set preference to a budget unrelated to the deletion");

        let state = test_state(&pool);
        delete_budget(State(state), Path(deleted), Extension(user))
            .await
            .expect("delete_budget Ok");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(
            prefs,
            Some(elsewhere),
            "deleting a stale is_default budget must not move the owner off a preference that already pointed elsewhere"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // #493. `delete_budget` reads the budget's is_default, then issues a
    // separate `DELETE`. A concurrent deleter can commit in that window, leaving
    // OUR delete matching zero rows — the handler must not then claim success,
    // and must not promote a replacement default budget for a deletion it did
    // not perform. Same deterministic race induction and the same load-bearing
    // `current_thread` flavor as `delete_category`'s #434 test. Against the
    // pre-fix handler this fails with 204 (and the `.unwrap_or(false)` was_default
    // path is exactly the "stale read silently skips promotion" class this guards).
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_budget_reports_404_when_the_row_vanishes_before_the_delete() {
        let captured = CapturedEvents::default();
        let _log_guard = tracing::subscriber::set_default(
            tracing_subscriber::layer::SubscriberExt::with(
                tracing_subscriber::registry(),
                captured.clone(),
            ),
        );

        let (pool, user) = rollup_test_setup().await;
        let outcome = delete_budget_race(&pool, user).await;
        rollup_cleanup(&pool, &[user]).await;

        let (deleted, survivor, status, msg, survivor_is_default) =
            outcome.expect("the #493 race must leave delete_budget reporting a failure");

        assert_eq!(status, StatusCode::NOT_FOUND, "a zero-row delete is a 404, not a 204");
        assert_eq!(
            msg, "Budget not found",
            "the body is deliberately the same as the never-existed 404 — the warn, not the \
             response, is what distinguishes them",
        );
        assert!(
            !survivor_is_default,
            "a delete that removed nothing must not promote a replacement default budget",
        );

        // The warn is the ONLY observable separating this 404 from the
        // never-existed one, so it is part of the contract. Without this the
        // whole tracing call could be deleted and both tests would still pass.
        let warns = captured.warns_containing("delete_budget matched zero rows");
        assert_eq!(warns.len(), 1, "exactly one #493 warn, got {warns:?}");
        let warn = &warns[0];
        for (label, id) in [("budget_id", deleted), ("user_id", user)] {
            assert!(warn.contains(&id.to_string()), "warn must carry {label}, got: {warn}");
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_preserves_budget_strategy_when_absent_and_updates_when_present() {
        let (pool, user) = rollup_test_setup().await;
        // First-budget trial gate: seed an entitled subscription (see
        // seed_active_sub) so create_budget doesn't hit a real Stripe call.
        seed_active_sub(&pool, user).await;
        let state = test_state(&pool);

        async fn stored_strategy(pool: &PgPool, id: Uuid) -> String {
            sqlx::query_scalar("SELECT budget_strategy FROM budgets WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch budget_strategy")
        }

        // Create a budget with budget_strategy='zero_based'.
        let created = create_budget(
            State(state.clone()),
            Extension(user),
            Json(BudgetPayload {
                name: "Strategy Update Budget".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("zero_based".to_string()),
                currency: None,
            }),
        )
        .await
        .expect("create_budget Ok");
        let budget_id = created.0.id;
        assert_eq!(stored_strategy(&pool, budget_id).await, "zero_based");

        // update_budget with budget_strategy ABSENT but a changed name preserves
        // the existing strategy (COALESCE).
        let updated = update_budget(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(BudgetPayload {
                name: "Strategy Update Budget (renamed)".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("update_budget (absent strategy) Ok");
        assert_eq!(
            stored_strategy(&pool, budget_id).await,
            "zero_based",
            "an update with budget_strategy absent must preserve the existing strategy"
        );
        assert_eq!(updated.0.budget_strategy, "zero_based");

        // update_budget with budget_strategy='limit_spent_remaining' applies it.
        let updated2 = update_budget(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(BudgetPayload {
                name: "Strategy Update Budget (renamed)".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("limit_spent_remaining".to_string()),
                currency: None,
            }),
        )
        .await
        .expect("update_budget (limit_spent_remaining) Ok");
        assert_eq!(stored_strategy(&pool, budget_id).await, "limit_spent_remaining");
        assert_eq!(updated2.0.budget_strategy, "limit_spent_remaining");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #317: update_budget rejects an ACTUAL budget_type change on a rollup CHILD, leaving the
    // row unchanged — closing the gap where an ordinary edit could silently recreate the
    // mismatch validate_rollup_link rejects at link time.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_rejects_budget_type_change_on_rollup_child() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed (matching type/strategy)");

        let err = update_budget(
            State(state.clone()),
            Path(child),
            Extension(owner),
            Json(BudgetPayload {
                name: "child-renamed".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: Some("project".to_string()),
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect_err("changing budget_type on a rollup child must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("budget type"));

        let stored_type: String = sqlx::query_scalar("SELECT budget_type FROM budgets WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .expect("fetch budget_type");
        assert_eq!(stored_type, "time_based", "budget_type must remain unchanged after the rejected edit");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #317: same guard, budget_strategy axis, on a rollup CHILD.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_rejects_budget_strategy_change_on_rollup_child() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed");

        let err = update_budget(
            State(state.clone()),
            Path(child),
            Extension(owner),
            Json(BudgetPayload {
                name: "child-renamed".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("zero_based".to_string()),
                currency: None,
            }),
        )
        .await
        .expect_err("changing budget_strategy on a rollup child must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("budgeting strategy"));

        let stored_strategy: String =
            sqlx::query_scalar("SELECT budget_strategy FROM budgets WHERE id = $1")
                .bind(child)
                .fetch_one(&pool)
                .await
                .expect("fetch budget_strategy");
        assert_eq!(stored_strategy, "limit_spent_remaining");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #317: the guard also blocks the PARENT's own type/strategy from changing while it has a
    // child — both axes, one test each.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_rejects_budget_type_change_on_rollup_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed");

        let err = update_budget(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(BudgetPayload {
                name: "parent-renamed".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: Some("project".to_string()),
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect_err("changing budget_type on a rollup parent must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("parent"));

        rollup_cleanup(&pool, &[owner]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_rejects_budget_strategy_change_on_rollup_parent() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed");

        let err = update_budget(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(BudgetPayload {
                name: "parent-renamed".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("zero_based".to_string()),
                currency: None,
            }),
        )
        .await
        .expect_err("changing budget_strategy on a rollup parent must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("parent"));

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #317: resubmitting the SAME budget_type/budget_strategy on a rollup child is a no-op, not
    // an error (Assumption 2 of the spec) — an update that merely echoes the current value back
    // must still succeed.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_allows_unchanged_budget_type_and_strategy_on_rollup_child() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed");

        let updated = update_budget(
            State(state.clone()),
            Path(child),
            Extension(owner),
            Json(BudgetPayload {
                name: "child-renamed".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: Some("time_based".to_string()),
                auto_renew: None,
                amount_mode: None,
                budget_strategy: Some("limit_spent_remaining".to_string()),
                currency: None,
            }),
        )
        .await
        .expect("resubmitting the current budget_type/budget_strategy must succeed as a no-op");
        assert_eq!(updated.0.name, "child-renamed", "the unrelated name change must still apply");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #317: an update that omits both budget_type and budget_strategy (e.g. a plain rename) on a
    // rollup child/parent must be entirely unaffected by the new guard.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_budget_allows_unrelated_field_change_on_rollup_child() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("initial link must succeed");

        let updated = update_budget(
            State(state.clone()),
            Path(child),
            Extension(owner),
            Json(BudgetPayload {
                name: "renamed-without-touching-type-or-strategy".to_string(),
                description: None,
                time_frame: "monthly".to_string(),
                budget_limit: None,
                rollover_enabled: None,
                budget_type: None,
                auto_renew: None,
                amount_mode: None,
                budget_strategy: None,
                currency: None,
            }),
        )
        .await
        .expect("a rename with budget_type/budget_strategy both absent must succeed");
        assert_eq!(updated.0.name, "renamed-without-touching-type-or-strategy");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // Batched list path (#116): `computed_budget_totals` resolves each budget by
    // its own mode. A 'fixed' budget surfaces its budget_limit even though it has
    // a category; a 'derived' budget surfaces its category sum.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn computed_budget_totals_honors_amount_mode_per_budget() {
        let (pool, user) = rollup_test_setup().await;

        // A: fixed, budget_limit=2000, category=100 -> 2000.
        let a = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET amount_mode = 'fixed', budget_limit = 2000.0 WHERE id = $1")
            .bind(a)
            .execute(&pool)
            .await
            .expect("set A fixed");
        // B: derived, category=300 -> 300.
        let b = seed_rollup_budget(&pool, user, 300.0, 0.0, false).await;
        // C: fixed, budget_limit=500, but NO expense category at all -> 500. This is
        // the subtle case the batched query must handle: it iterates the BUDGETS
        // query, not the category groups, so a fixed budget absent from the category
        // SUM still surfaces its limit (a regression to grouping by category would
        // silently drop C to 0).
        let c = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        sqlx::query("DELETE FROM categories WHERE budget_id = $1")
            .bind(c)
            .execute(&pool)
            .await
            .expect("strip C's categories");
        sqlx::query("UPDATE budgets SET amount_mode = 'fixed', budget_limit = 500.0 WHERE id = $1")
            .bind(c)
            .execute(&pool)
            .await
            .expect("set C fixed");

        let totals = computed_budget_totals(&pool, &[a, b, c])
            .await
            .expect("batched totals");
        assert_eq!(totals.get(&a).copied(), Some(2000.0), "fixed A reports budget_limit");
        assert_eq!(totals.get(&b).copied(), Some(300.0), "derived B reports category sum");
        assert_eq!(
            totals.get(&c).copied(),
            Some(500.0),
            "category-less fixed C still reports its budget_limit"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Rollup aggregation (#116 x #52): a parent that rolls up one fixed child
    // (budget_limit=2000) and one derived child (category=300) reports an aggregated
    // base of 2300. Under #52's live linked-category model the children are linked
    // via `link_rollup` (which creates mirror categories in the parent), and the
    // parent's base flows through the now-mode-aware `computed_budget_total` — so a
    // mirrored fixed source contributes its budget_limit, not its category sum.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn aggregated_amounts_honor_child_amount_mode() {
        let (pool, user) = rollup_test_setup().await;

        // Parent: derived, no own category amount (limit 0) -> own base 0.
        let parent = seed_rollup_budget(&pool, user, 0.0, 0.0, false).await;
        // Fixed child: budget_limit=2000, category=100 (ignored by fixed mode).
        let fixed_child = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET amount_mode = 'fixed', budget_limit = 2000.0 WHERE id = $1")
            .bind(fixed_child)
            .execute(&pool)
            .await
            .expect("set fixed child");
        // Derived child: category=300.
        let derived_child = seed_rollup_budget(&pool, user, 300.0, 0.0, false).await;

        // Link both children into the parent via the live linked-category rollup,
        // creating the mirror categories the parent's base resolves through.
        let state = test_state(&pool);
        for c in [fixed_child, derived_child] {
            link_rollup(
                State(state.clone()),
                Path(parent),
                Extension(user),
                Json(RollupPayload { child_budget_id: c }),
            )
            .await
            .expect("link child into parent");
        }

        let parent_row = sqlx::query_as::<_, Budget>("SELECT * FROM budgets WHERE id = $1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .expect("parent row");
        let (agg_base, _agg_effective, _agg_spent) =
            aggregated_budget_amounts(&pool, &parent_row).await.expect("aggregate");

        // 0 (parent) + 2000 (fixed child, via budget_limit) + 300 (derived child).
        assert_eq!(agg_base, 2300.0, "aggregated base honors each child's mode");

        // The batched list path resolves mirrors through the same mode-aware SQL.
        let batched = computed_budget_totals(&pool, &[parent])
            .await
            .expect("batched totals");
        assert_eq!(
            batched.get(&parent).copied(),
            Some(2300.0),
            "batched mirror resolution also honors the fixed child"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Owner-scoping (#52): you may only roll up budgets you OWN. A foreign child
    // (owned by another user) is rejected 403 even by an owner of the parent.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_rollup_rejects_non_owned_child() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, other) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let foreign_child = seed_rollup_budget(&pool, other, 50.0, 0.0, false).await;

        let state = test_state(&pool);
        let err = link_rollup(
            State(state),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: foreign_child }),
        )
        .await
        .expect_err("linking a non-owned child must be rejected");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "foreign child -> 403");

        // The link was NOT made.
        let still_standalone: Option<Uuid> =
            sqlx::query_scalar("SELECT rollup_parent_id FROM budgets WHERE id = $1")
                .bind(foreign_child).fetch_one(&pool).await.unwrap();
        assert_eq!(still_standalone, None, "foreign child remains standalone");

        rollup_cleanup(&pool, &[owner, other]).await;
    }

    // #300: budgets can only be rolled up together when they share the same
    // budget_type. A time_based parent + a project child is rejected 409, and no
    // mirror category is created in the parent.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_rollup_rejects_budget_type_mismatch() {
        let (pool, owner) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        // seed_rollup_budget hardcodes 'time_based'; flip the child to 'project' so
        // the two budgets differ only on budget_type (both keep the default
        // 'limit_spent_remaining' budget_strategy).
        sqlx::query("UPDATE budgets SET budget_type = 'project' WHERE id = $1")
            .bind(child)
            .execute(&pool)
            .await
            .expect("set child budget_type to project");

        let state = test_state(&pool);
        let err = link_rollup(
            State(state),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect_err("mismatched budget_type must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT, "budget_type mismatch -> 409");

        let mirror_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("mirror count query");
        assert_eq!(mirror_count, 0, "no mirror category created on a rejected link");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #300: budgets can only be rolled up together when they share the same
    // budget_strategy. Two time_based budgets with different strategies are
    // rejected 409, and no mirror category is created in the parent.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_rollup_rejects_budget_strategy_mismatch() {
        let (pool, owner) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
        // Both default to 'limit_spent_remaining'; flip the child to 'zero_based' so
        // they differ only on budget_strategy (both keep 'time_based' budget_type).
        sqlx::query("UPDATE budgets SET budget_strategy = 'zero_based' WHERE id = $1")
            .bind(child)
            .execute(&pool)
            .await
            .expect("set child budget_strategy to zero_based");

        let state = test_state(&pool);
        let err = link_rollup(
            State(state),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect_err("mismatched budget_strategy must be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT, "budget_strategy mismatch -> 409");

        let mirror_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("mirror count query");
        assert_eq!(mirror_count, 0, "no mirror category created on a rejected link");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // Regression guard for #52/#298: two budgets sharing the same budget_type AND
    // budget_strategy still link successfully and create exactly one mirror
    // category — the #300 checks must not affect the existing matching-budget path.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_rollup_succeeds_when_type_and_strategy_match() {
        let (pool, owner) = rollup_test_setup().await;

        // Both default to 'time_based' / 'limit_spent_remaining' from seed_rollup_budget.
        let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;

        let state = test_state(&pool);
        link_rollup(
            State(state),
            Path(parent),
            Extension(owner),
            Json(RollupPayload { child_budget_id: child }),
        )
        .await
        .expect("matching type/strategy link must succeed");

        let mirror_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("mirror count query");
        assert_eq!(mirror_count, 1, "exactly one mirror category created");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #238: a View/Edit sharee must NOT be able to flip is_default on a budget
    // they don't own — check_permission's floor for this endpoint is Owner,
    // not "any access", mirroring share_budget/revoke_share.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_default_budget_rejects_non_owner_share() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, view_sharee) = rollup_test_setup().await;
        let (_, edit_sharee) = rollup_test_setup().await;

        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        async fn seed_share(pool: &PgPool, budget_id: Uuid, sharee: Uuid, level: &str) {
            let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
                .bind(sharee)
                .fetch_one(pool)
                .await
                .expect("fetch sharee email");
            sqlx::query(
                "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(budget_id)
            .bind(&email)
            .bind(level)
            .execute(pool)
            .await
            .expect("seed share");
        }
        seed_share(&pool, owner_budget, view_sharee, "view").await;
        seed_share(&pool, owner_budget, edit_sharee, "edit").await;

        let state = test_state(&pool);

        // Neither a View- nor an Edit-permission sharee may flip is_default —
        // the ticket's exploited path named both ("a user with only View or
        // Edit access"), and Edit is a distinct Permission variant from View
        // (check_permission maps 'edit' -> Permission::Edit separately), so
        // this exercises a genuinely different branch, not just a re-run of
        // the View case.
        for (sharee, label) in [(view_sharee, "view"), (edit_sharee, "edit")] {
            let err = set_default_budget(
                State(state.clone()),
                Path(owner_budget),
                Extension(sharee),
            )
            .await
            .expect_err("a non-owner sharee must not be able to set the default");
            assert_eq!(err.0, StatusCode::FORBIDDEN, "{label}-permission sharee -> 403");

            let still_false: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
                .bind(owner_budget)
                .fetch_one(&pool)
                .await
                .expect("read is_default after rejected attempt");
            assert!(!still_false, "{label} sharee's rejected attempt must not flip is_default");
        }

        // The real owner's own flow still works.
        let ok = set_default_budget(State(state), Path(owner_budget), Extension(owner)).await;
        assert!(ok.is_ok(), "the real owner must still be able to set their own default");

        let now_true: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
            .bind(owner_budget)
            .fetch_one(&pool)
            .await
            .expect("read is_default after owner's own call");
        assert!(now_true, "owner's own call must still flip is_default to true");

        rollup_cleanup(&pool, &[owner, view_sharee, edit_sharee]).await;
    }

    // #255: unlike set_default_budget (Owner-only, #238), set_active_budget
    // is the entire point of the ticket — a View OR Edit sharee CAN set a
    // shared budget as their own active-view preference, without touching
    // the owner's is_default row (that column is untouched by this endpoint).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_allows_any_sharee() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, view_sharee) = rollup_test_setup().await;
        let (_, edit_sharee) = rollup_test_setup().await;

        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        async fn seed_share(pool: &PgPool, budget_id: Uuid, sharee: Uuid, level: &str) {
            let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
                .bind(sharee)
                .fetch_one(pool)
                .await
                .expect("fetch sharee email");
            sqlx::query(
                "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(budget_id)
            .bind(&email)
            .bind(level)
            .execute(pool)
            .await
            .expect("seed share");
        }
        seed_share(&pool, owner_budget, view_sharee, "view").await;
        seed_share(&pool, owner_budget, edit_sharee, "edit").await;

        let state = test_state(&pool);

        for (sharee, label) in [(view_sharee, "view"), (edit_sharee, "edit")] {
            let ok = set_active_budget(
                State(state.clone()),
                Path(owner_budget),
                Extension(sharee),
            )
            .await;
            assert!(ok.is_ok(), "{label}-permission sharee must be able to set a shared budget active");

            let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
                .bind(sharee)
                .fetch_one(&pool)
                .await
                .expect("read preference");
            assert_eq!(prefs, Some(owner_budget), "{label} sharee's preference must now point at the shared budget");

            // The owner's is_default row is completely untouched by this endpoint.
            let owner_default: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
                .bind(owner_budget)
                .fetch_one(&pool)
                .await
                .expect("read is_default");
            assert!(!owner_default, "{label} sharee's activate call must never flip the owner's is_default");
        }

        rollup_cleanup(&pool, &[owner, view_sharee, edit_sharee]).await;
    }

    // #255: a user with NO access at all (not owner, not shared) is rejected,
    // and their preference (if they had one) is left untouched.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_denies_no_access() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let err = set_active_budget(State(state), Path(owner_budget), Extension(stranger))
            .await
            .expect_err("a user with no access must be rejected");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "no access -> 403");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(stranger)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(prefs, None, "rejected attempt must not set a preference");

        rollup_cleanup(&pool, &[owner, stranger]).await;
    }

    // #255: the owner can of course set their own budget as their active view too.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_allows_owner() {
        let (pool, owner) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let ok = set_active_budget(State(state), Path(owner_budget), Extension(owner)).await;
        assert!(ok.is_ok(), "the owner must be able to set their own budget active");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(prefs, Some(owner_budget));

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255: calling /activate twice on the same target is idempotent — the
    // second call succeeds and leaves the preference unchanged (still pointing
    // at the same budget), not doubled or errored.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_is_idempotent_on_repeat_call() {
        let (pool, owner) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let first = set_active_budget(State(state.clone()), Path(owner_budget), Extension(owner)).await;
        assert!(first.is_ok(), "first call must succeed");
        let second = set_active_budget(State(state), Path(owner_budget), Extension(owner)).await;
        assert!(second.is_ok(), "repeating the same activation must also succeed, not error");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(prefs, Some(owner_budget), "repeat call leaves the same preference in place");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255: set_active_budget's audit row must be USER-scoped (NULL budget_id),
    // not budget-scoped — a budget-scoped row would surface in the budget
    // OWNER's own audit trail/export even when the caller is a sharee acting
    // entirely on their own per-viewer preference, in tension with this whole
    // endpoint's point (never touching anything the owner can see as "theirs").
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_audit_row_is_user_scoped() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, sharee) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(sharee)
            .fetch_one(&pool)
            .await
            .expect("fetch sharee email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(owner_budget)
        .bind(&email)
        .execute(&pool)
        .await
        .expect("seed share");

        let state = test_state(&pool);
        let ok = set_active_budget(State(state), Path(owner_budget), Extension(sharee)).await;
        assert!(ok.is_ok(), "sharee activation must succeed");

        let row: Option<(Option<Uuid>, String)> = sqlx::query_as(
            "SELECT budget_id, details FROM audit_logs \
             WHERE user_id = $1 AND action = 'SET_ACTIVE_BUDGET' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(sharee)
        .fetch_optional(&pool)
        .await
        .expect("read audit row");
        let (audit_budget_id, details) = row.expect("a SET_ACTIVE_BUDGET audit row must exist");
        assert_eq!(audit_budget_id, None, "audit row must be user-scoped (NULL budget_id)");
        assert!(
            details.contains(&owner_budget.to_string()),
            "the activated budget's id must still be traceable via the free-text details"
        );

        sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(sharee).execute(&pool).await.ok();
        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

    // #255: the whole design leans on active_budget_id's ON DELETE SET NULL FK
    // (users.active_budget_id -> budgets.id) to guarantee a deleted budget can
    // never leave a dangling preference — this is asserted throughout the code's
    // doc comments and the migration's own header comment, but was previously
    // never exercised end-to-end by a test. Delete the budget a preference points
    // at and confirm the FK cascade actually clears it (not just that the code
    // assumes it will), and that list_budgets keeps working cleanly afterward
    // (falls through to the no-preference path, no error, no dangling reference).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn deleting_active_budget_clears_the_preference_via_fk_cascade() {
        let (pool, owner) = rollup_test_setup().await;
        let target = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let other = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;

        let state = test_state(&pool);
        let ok = set_active_budget(State(state), Path(target), Extension(owner)).await;
        assert!(ok.is_ok(), "seeding the preference must succeed");

        let before: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read preference before delete");
        assert_eq!(before, Some(target), "sanity: preference is set before the delete");

        // Delete the budget the preference points at — the FK's ON DELETE SET
        // NULL (not an application-level cleanup step) is what must clear this.
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(target)
            .execute(&pool)
            .await
            .expect("delete the preferred budget");

        let after: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read preference after delete");
        assert_eq!(after, None, "FK's ON DELETE SET NULL must clear the dangling preference");

        // list_budgets must still work cleanly afterward — no error, and no
        // row incorrectly marked active now that the preference is NULL.
        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(owner),
        )
        .await
        .expect("list_budgets must still succeed after the cascade");
        let remaining = list.iter().find(|b| b.id == other).expect("remaining budget present");
        assert!(!remaining.is_active, "no row should be marked active once the preference is NULL");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255: list_budgets surfaces is_active on exactly the row matching the
    // caller's active_budget_id preference — owned case.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_marks_owned_active_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let budget_a = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(budget_b)
            .bind(owner)
            .execute(&pool)
            .await
            .expect("seed preference");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(owner),
        )
        .await
        .expect("list_budgets must succeed");

        let a = list.iter().find(|b| b.id == budget_a).expect("budget_a present");
        let b = list.iter().find(|b| b.id == budget_b).expect("budget_b present");
        assert!(!a.is_active, "non-preferred owned budget must not be marked active");
        assert!(b.is_active, "preferred owned budget must be marked active");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #358: list_budgets surfaces the same own-only zero-based aggregates as
    // get_budget — income and savings totals, plus the mode-aware expense base.
    // Guards the previously-untested list handler path against divergence.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_exposes_zero_based_aggregates() {
        let (pool, user) = rollup_test_setup().await;

        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, budget_strategy) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', 'zero_based')",
        )
        .bind(budget_id)
        .bind(user)
        .bind(format!("zb-{budget_id}"))
        .execute(&pool)
        .await
        .expect("seed zero_based budget");

        // income = 14906 (14000 + 906), savings = 385, expense = 8062.
        seed_typed_category(&pool, budget_id, "income", 14000.0).await;
        seed_typed_category(&pool, budget_id, "income", 906.0).await;
        seed_typed_category(&pool, budget_id, "savings", 385.0).await;
        seed_typed_category(&pool, budget_id, "expense", 8062.0).await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(user),
        )
        .await
        .expect("list_budgets must succeed");

        let item = list.iter().find(|b| b.id == budget_id).expect("seeded budget present");
        assert_eq!(item.aggregated_income_amount, 14906.0, "Available = own income total");
        assert_eq!(item.aggregated_savings_amount, 385.0, "own savings total");
        assert_eq!(item.aggregated_base_amount, 8062.0, "mode-aware expense base");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #255: list_budgets surfaces is_active on a SHARED budget when the
    // preference points at it — the entire point of the ticket (decoupled
    // from is_default, which stays owner-scoped and untouched here).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_marks_shared_active_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let shared_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let viewer_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("fetch viewer email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(shared_budget)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared_budget)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("seed preference");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        let shared = list.iter().find(|b| b.id == shared_budget).expect("shared budget present");
        assert!(shared.is_active, "shared budget matching the preference must be marked active");
        assert!(!shared.is_owner, "sanity: this row is shared, not owned, by the viewer");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #255: no preference set (active_budget_id NULL) -> every row is_active: false,
    // is_default/list semantics are completely unaffected (regression guard against
    // this feature changing existing default-resolution behavior).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_all_inactive_when_no_preference_set() {
        let (pool, owner) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(owner),
        )
        .await
        .expect("list_budgets must succeed");

        let item = list.iter().find(|b| b.id == budget).expect("budget present");
        assert!(!item.is_active, "no preference set -> is_active must be false");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255 edge case: a preference pointing at a budget whose SHARE was later
    // revoked (not deleted -> no FK cascade fires) must not surface is_active
    // anywhere, and must not error. The stale preference silently stops
    // resolving rather than leaking a budget the viewer can no longer see.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_ignores_preference_after_share_revoked() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let shared_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let viewer_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("fetch viewer email");
        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(share_id)
        .bind(shared_budget)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared_budget)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("seed preference");

        // Revoke the share -> the budget drops out of the viewer's accessible list,
        // but active_budget_id (no FK to budget_shares) stays set.
        sqlx::query("DELETE FROM budget_shares WHERE id = $1")
            .bind(share_id)
            .execute(&pool)
            .await
            .expect("revoke share");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed even with a dangling preference");

        assert!(list.is_empty(), "viewer has no accessible budgets after the share was revoked");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #241: list_budgets surfaces the owner's display name on SHARED rows only —
    // a caller's OWN rows have no need for it (mirrors BudgetContextRow's
    // treatment of owned rows in rag.rs).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_owner_name_populated_for_shared_only() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, sharee) = rollup_test_setup().await;

        // rollup_test_setup does not set a display name; set one explicitly so
        // the assertion is deterministic.
        sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
            .bind("Ollie Owner")
            .bind(owner)
            .execute(&pool)
            .await
            .expect("set owner display name");

        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let sharee_own_budget = seed_rollup_budget(&pool, sharee, 50.0, 0.0, false).await;

        let sharee_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(sharee)
            .fetch_one(&pool)
            .await
            .expect("fetch sharee email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(owner_budget)
        .bind(&sharee_email)
        .bind("view")
        .execute(&pool)
        .await
        .expect("seed share");

        let state = test_state(&pool);
        let list = list_budgets(
            State(state),
            Query(ListBudgetsQuery::default()),
            Extension(sharee),
        )
        .await
        .expect("list_budgets Ok")
        .0;

        let shared_item = list
            .iter()
            .find(|b| b.id == owner_budget)
            .expect("owner's shared budget present in sharee's list");
        assert_eq!(
            shared_item.owner_name,
            Some("Ollie Owner".to_string()),
            "shared row surfaces the owner's display name"
        );

        let own_item = list
            .iter()
            .find(|b| b.id == sharee_own_budget)
            .expect("sharee's own budget present in their own list");
        assert_eq!(
            own_item.owner_name, None,
            "a caller's own row must not populate owner_name"
        );

        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

    // #241: when the OWNER of a shared budget never set a display name,
    // list_budgets must pass the NULL through as owner_name: None on the
    // sharee's row — never coerce it to a placeholder string here (the
    // frontend's ownershipLabel/"shared by someone" fallback owns that UX
    // decision, not the backend). Distinct from
    // list_budgets_owner_name_populated_for_shared_only above, which
    // deliberately sets a name so THAT assertion is deterministic; this test
    // instead relies on rollup_test_setup's own default (no name set) to
    // exercise the opposite branch of the same nullable column.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_owner_name_is_none_when_owner_has_no_display_name() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, sharee) = rollup_test_setup().await;

        let owner_name: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read owner's default display name");
        assert_eq!(
            owner_name, None,
            "precondition: rollup_test_setup must not set a display name by default"
        );

        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let sharee_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(sharee)
            .fetch_one(&pool)
            .await
            .expect("fetch sharee email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(owner_budget)
        .bind(&sharee_email)
        .bind("view")
        .execute(&pool)
        .await
        .expect("seed share");

        let state = test_state(&pool);
        let list = list_budgets(
            State(state),
            Query(ListBudgetsQuery::default()),
            Extension(sharee),
        )
        .await
        .expect("list_budgets Ok")
        .0;

        let shared_item = list
            .iter()
            .find(|b| b.id == owner_budget)
            .expect("owner's shared budget present in sharee's list");
        assert_eq!(
            shared_item.owner_name, None,
            "a NULL users.name must pass through as owner_name: None, not a placeholder"
        );

        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

    // Cycle / single-level guards (#52) enforced through the handler: self-link
    // -> 400; linking a child that is already a parent -> 409.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_rollup_rejects_self_and_nesting() {
        let (pool, user) = rollup_test_setup().await;
        let a = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let b = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let c = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let state = test_state(&pool);

        // Self-link -> 400.
        let self_err = link_rollup(
            State(state.clone()), Path(a), Extension(user),
            Json(RollupPayload { child_budget_id: a }),
        ).await.expect_err("self-link rejected");
        assert_eq!(self_err.0, StatusCode::BAD_REQUEST, "self-link -> 400");

        // Make `b` a parent by rolling `c` into it.
        let _ = link_rollup(
            State(state.clone()), Path(b), Extension(user),
            Json(RollupPayload { child_budget_id: c }),
        ).await.expect("roll c into b");

        // Now rolling `b` (a parent) into `a` -> 409 (single-level, would nest).
        let nest_err = link_rollup(
            State(state.clone()), Path(a), Extension(user),
            Json(RollupPayload { child_budget_id: b }),
        ).await.expect_err("nesting rejected");
        assert_eq!(nest_err.0, StatusCode::CONFLICT, "child-is-parent -> 409");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Link is idempotent on re-link to the same parent; a child already linked to a
    // DIFFERENT parent is rejected 409; unlink is idempotent (#52).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_unlink_idempotency_and_relink_conflict() {
        let (pool, user) = rollup_test_setup().await;
        let p1 = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let p2 = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let child = seed_rollup_budget(&pool, user, 50.0, 0.0, false).await;
        let state = test_state(&pool);

        // Link child -> p1.
        let _ = link_rollup(State(state.clone()), Path(p1), Extension(user),
            Json(RollupPayload { child_budget_id: child })).await.expect("link p1");
        // Re-link to SAME parent -> idempotent (no error).
        let _ = link_rollup(State(state.clone()), Path(p1), Extension(user),
            Json(RollupPayload { child_budget_id: child })).await.expect("idempotent relink");

        // Link to a DIFFERENT parent while already linked -> 409.
        let conflict = link_rollup(State(state.clone()), Path(p2), Extension(user),
            Json(RollupPayload { child_budget_id: child })).await.expect_err("relink elsewhere");
        assert_eq!(conflict.0, StatusCode::CONFLICT, "already-linked -> 409");

        // Unlink from p1, then unlink again -> idempotent.
        let _ = unlink_rollup(State(state.clone()), Path((p1, child)), Extension(user))
            .await.expect("unlink");
        let _ = unlink_rollup(State(state.clone()), Path((p1, child)), Extension(user))
            .await.expect("idempotent unlink");

        let parent_now: Option<Uuid> =
            sqlx::query_scalar("SELECT rollup_parent_id FROM budgets WHERE id = $1")
                .bind(child).fetch_one(&pool).await.unwrap();
        assert_eq!(parent_now, None, "child is standalone after unlink");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Unlinking a source removes its mirror category from the parent and restores
    // the parent's own total; the child's backlink is cleared (#52). Exercises the
    // real `link_rollup`/`unlink_rollup` handlers end-to-end against Postgres.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn unlink_removes_mirror_and_restores_total() {
        let (pool, user) = rollup_test_setup().await;
        // Parent owns expense 100; source owns expense 200.
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        // Link via the real handler: creates the mirror in the parent.
        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        let linked_total = computed_budget_total(&pool, parent).await.expect("linked total");
        assert_eq!(linked_total, 300.0, "parent total = own 100 + mirrored source 200");

        // Unlink via the real handler.
        let _ = unlink_rollup(State(state), Path((parent, source)), Extension(user))
            .await
            .expect("unlink source from parent");

        let restored = computed_budget_total(&pool, parent).await.expect("restored total");
        assert_eq!(restored, 100.0, "parent total restored to its own 100");

        // No mirror category for the source remains.
        let mirrors: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(mirrors, 0, "mirror category removed on unlink");

        // The source's backlink is cleared.
        let backlink: Option<Uuid> =
            sqlx::query_scalar("SELECT rollup_parent_id FROM budgets WHERE id = $1")
                .bind(source)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(backlink, None, "source rollup_parent_id cleared on unlink");

        rollup_cleanup(&pool, &[user]).await;
    }

    // The mirror is LIVE: raising an expense limit on the source budget is reflected
    // in the parent's computed total on the very next read, with no job/refresh (#52).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn editing_source_updates_parent_total() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("total"),
            300.0,
            "parent total = own 100 + mirrored source 200"
        );

        // Raise the source's expense category limit by 50 (200 -> 250).
        sqlx::query(
            "UPDATE categories SET category_limit = category_limit + 50 \
             WHERE budget_id = $1 AND category_type = 'expense'",
        )
        .bind(source)
        .execute(&pool)
        .await
        .expect("raise source limit");

        // The change is reflected immediately on the next read — the link is live.
        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("total after edit"),
            350.0,
            "parent total tracks the source edit live (own 100 + mirrored 250)"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Archiving a source drops it out of the parent's rollup on BOTH sides (#52):
    // `linked_budgets_spent` excludes the archived source's spend AND
    // `computed_budget_total` excludes its budgeted envelope. One rule —
    // "archived children don't count" — so the combined budget and combined spend
    // always cover the same set of sources.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn archived_source_excluded_from_rollup_amount_and_spent() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        // Source owns expense 200 AND a current-period transaction of 30 (spent).
        let source = seed_rollup_budget(&pool, user, 200.0, 30.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        let now = Utc::now();
        let spent_active = linked_budgets_spent(&pool, parent, now)
            .await
            .expect("linked spent (active source)");
        assert!(spent_active > 0.0, "active source contributes its current-period spend");

        // Archive the source budget.
        sqlx::query("UPDATE budgets SET archived_at = now() WHERE id = $1")
            .bind(source)
            .execute(&pool)
            .await
            .expect("archive source");

        let spent_archived = linked_budgets_spent(&pool, parent, now)
            .await
            .expect("linked spent (archived source)");
        assert_eq!(spent_archived, 0.0, "archived source excluded from linked spent");

        // The mirror's envelope drops out too: an archived source no longer
        // contributes its budgeted amount, so the parent total falls back to its
        // own 100 — symmetric with the spend exclusion above.
        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("total"),
            100.0,
            "archived source excluded from the parent's computed total"
        );
        // The batched path agrees with the single read for the archived case.
        let batched = computed_budget_totals(&pool, &[parent])
            .await
            .expect("batched totals");
        assert_eq!(
            batched.get(&parent).copied().unwrap_or(0.0),
            100.0,
            "batched total also excludes the archived source's mirror"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // `linked_budgets_spent` sums EVERY active source's current-period spend, not
    // just one: two sources spending 30 and 45 give a combined 75. Protects the
    // batched-query summation (#52) against dropping or double-counting a source.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn linked_spent_sums_all_active_sources() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source_a = seed_rollup_budget(&pool, user, 200.0, 30.0, false).await;
        let source_b = seed_rollup_budget(&pool, user, 150.0, 45.0, false).await;
        let state = test_state(&pool);

        for src in [source_a, source_b] {
            let _ = link_rollup(
                State(state.clone()),
                Path(parent),
                Extension(user),
                Json(RollupPayload { child_budget_id: src }),
            )
            .await
            .expect("link source into parent");
        }

        let spent = linked_budgets_spent(&pool, parent, Utc::now())
            .await
            .expect("combined linked spent");
        assert_eq!(spent, 75.0, "combined spend = source_a 30 + source_b 45");

        rollup_cleanup(&pool, &[user]).await;
    }

    // `list_categories` resolves a mirror category's base from its source budget's
    // own total, and ZEROES it once the source is archived (#52) — keeping the REST
    // per-category view consistent with the archived-aware parent total. Also
    // protects the batched source-total resolution against an off-by-source bug.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_categories_mirror_base_reflects_source_and_zeroes_when_archived() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        let Json(before) = list_categories(State(state.clone()), Path(parent), Extension(user))
            .await
            .expect("list categories");
        let mirror = before
            .iter()
            .find(|c| c.linked_budget_id == Some(source))
            .expect("parent has the mirror category");
        assert_eq!(mirror.base_amount, 200.0, "active source's mirror base = its envelope");

        sqlx::query("UPDATE budgets SET archived_at = now() WHERE id = $1")
            .bind(source)
            .execute(&pool)
            .await
            .expect("archive source");

        let Json(after) = list_categories(State(state.clone()), Path(parent), Extension(user))
            .await
            .expect("list categories after archive");
        let mirror = after
            .iter()
            .find(|c| c.linked_budget_id == Some(source))
            .expect("mirror still present after archive");
        assert_eq!(mirror.base_amount, 0.0, "archived source's mirror base is 0");
        assert_eq!(mirror.effective_amount, 0.0, "archived mirror effective is 0 too");

        rollup_cleanup(&pool, &[user]).await;
    }

    // --- #432: a failed previous-period spend read must not degrade carries ---

    /// Seed a monthly time_based budget owned by `owner` with a single expense
    /// category (limit 100) and one 80.0 transaction dated inside the PREVIOUS
    /// period window. With `rollover` on, the #49 carry is a non-zero 20.0; with
    /// it off, the handlers take the skip-the-spend-read fast path and every carry
    /// is 0 — the only difference between the two, so a pair of these isolates the
    /// fast path exactly. Returns (budget_id, category_id).
    #[cfg(test)]
    async fn seed_prev_period_carry_budget(pool: &PgPool, owner: Uuid, rollover: bool) -> (Uuid, Uuid) {
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, rollover_enabled) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', $4)",
        )
        .bind(budget_id)
        .bind(owner)
        .bind(format!("carry-{budget_id}"))
        .bind(rollover)
        .execute(pool)
        .await
        .expect("seed budget");

        let cat_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Groceries', 'expense', 100.0)",
        )
        .bind(cat_id)
        .bind(budget_id)
        .execute(pool)
        .await
        .expect("seed category");

        // Dated one day INTO the previous calendar period so it lands inside the
        // window `previous_period_window` computes, not merely "60 days ago".
        let (prev_start, _prev_end) = previous_period_window("monthly", Utc::now());
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 80.0, $4, 'prev period')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(cat_id)
        .bind(prev_start + chrono::Duration::days(1))
        .execute(pool)
        .await
        .expect("seed previous-period txn");

        (budget_id, cat_id)
    }

    // A failed previous-period spend read is an ERROR, not a licence to invent a
    // number (#432) — the rule and its rationale live at the `prev_map` binding in
    // `list_categories`; this test pins the behaviour. Note the pre-fix
    // degradation INFLATED the carry: on this exact fixture it turned the true
    // `carried 20 / effective 120` into `carried 100 / effective 200`, because an
    // empty map means `prev_spent = 0` and the carry is `(base - prev_spent)`.
    // Leg 1 asserts the true values, so it also pins the number the degradation
    // got wrong.
    //
    // Three legs, all bound before anything is asserted (see `drop_restricted_role`):
    //   1. CONTROL, admin pool, rollover ON  -> 200 with the real carry. Without
    //      it the fault leg's 500 could pass vacuously, since a handler that 500s
    //      for an unrelated reason (bad seed, missing column) looks identical.
    //   2. FAULT, restricted pool, rollover ON -> 500.
    //   3. FAST PATH, restricted pool, rollover OFF -> 200 with a 0 carry. The
    //      positive control ON THE RESTRICTED POOL. Leg 2 asserts only a status,
    //      and every statement ahead of the spend read maps to 500 too, so alone
    //      it would keep passing if a table fell out of the grant list. Leg 3 runs
    //      the same handler on the same role with only `rollover_enabled` flipped
    //      — the one thing deciding whether the spend read is issued — so a 200
    //      here pins that every OTHER statement succeeds and leg 2's 500 really is
    //      the `transactions` read. It also pins the deliberately-retained
    //      skip-the-query fast path, otherwise exercised only incidentally and
    //      only on the admin pool where hoisting the query out of the `if` would
    //      change nothing observable. It belongs here, not in a sibling test,
    //      because it is only meaningful against the same role as leg 2.
    //
    // `#[serial]` NOT because the role name could collide — it is a fresh UUID —
    // but because CREATE/DROP ROLE and GRANT rewrite shared catalog rows
    // (`pg_authid`, `pg_class`), and concurrent GRANTs on the same table serialize
    // on the `pg_class` tuple. Belt and braces: `#[serial]` only orders these
    // against the other `#[serial]` tests, not against the parallel majority.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial]
    async fn list_categories_propagates_a_failed_previous_period_spend_read() {
        let (pool, user) = rollup_test_setup().await;
        let (budget_id, cat_id) = seed_prev_period_carry_budget(&pool, user, true).await;
        let (no_roll_budget, no_roll_cat) =
            seed_prev_period_carry_budget(&pool, user, false).await;

        let control = list_categories(State(test_state(&pool)), Path(budget_id), Extension(user)).await;

        let (rpool, role) = restricted_pool_denied_transactions_select(&pool).await;
        let faulted =
            list_categories(State(test_state(&rpool)), Path(budget_id), Extension(user)).await;
        let fast_path =
            list_categories(State(test_state(&rpool)), Path(no_roll_budget), Extension(user)).await;

        drop_restricted_role(&pool, rpool, &role).await;
        rollup_cleanup(&pool, &[user]).await;

        let Json(rows) = control.expect("list_categories on the unrestricted pool");
        let row = rows.iter().find(|c| c.id == cat_id).expect("groceries row present");
        assert_eq!(row.carried_amount, 20.0, "limit 100 less previous spend 80 carries 20");
        assert_eq!(row.effective_amount, 120.0, "effective is base 100 + carry 20");

        let (status, _msg) = faulted
            .err()
            .expect("a failed prev-period spend read must not return 200 with an invented carry");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let Json(rows) = fast_path
            .expect("rollover-off budget never issues the spend read, so the restricted role suffices");
        let row = rows.iter().find(|c| c.id == no_roll_cat).expect("groceries row present");
        assert_eq!(row.carried_amount, 0.0, "rollover off carries nothing");
        assert_eq!(row.effective_amount, 100.0, "effective is the bare limit");
        // The skipped query also leaves this at 0 for a category that DID spend
        // 80 last period — the documented consequence of the fast path.
        assert_eq!(row.prev_period_spent, 0.0, "the skipped read reports no prev spend");
    }

    // The same #432 rule on the write path; the rationale, including why a 500 on
    // an already-durable write is accepted here, is at the `prev_spent` binding in
    // `update_category`. `#[serial]` for the reason given on the test above.
    //
    // No subscription is seeded: `require_owner_entitled` returns Ok immediately
    // unless `BILLING_WRITE_ENFORCEMENT` is set, which nothing in this crate sets
    // (it is read only in `access.rs`, from the environment).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial]
    async fn update_category_propagates_a_failed_previous_period_spend_read() {
        let (pool, user) = rollup_test_setup().await;
        let (budget_id, cat_id) = seed_prev_period_carry_budget(&pool, user, true).await;

        // CONTROL leg on the admin pool. It is also the crate's only assertion on
        // `update_category`'s carry ARITHMETIC — every other `update_category_*`
        // test checks names, types and limits, none of them `carried_amount` — so
        // without it the response's carry could be silently wrong and only the
        // fault leg would still pass. A distinct rename so it cannot be confused
        // with the fault leg's payload.
        let control = update_category(
            State(test_state(&pool)),
            Path((budget_id, cat_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: Some("Larder".to_string()),
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: None,
            }),
        )
        .await;

        let (rpool, role) = restricted_pool_denied_transactions_select(&pool).await;
        let faulted = update_category(
            State(test_state(&rpool)),
            Path((budget_id, cat_id)),
            Extension(user),
            // `is_fund` left absent so no fund path runs; this is a plain rename.
            Json(CategoryUpdatePayload {
                name: Some("Provisions".to_string()),
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: None,
            }),
        )
        .await;

        // Read the row back on the ADMIN pool, BEFORE `rollup_cleanup` deletes it,
        // to pin the documented "500 on a write that IS durable" contract: the
        // fault leg's rename must have COMMITTED, proving the 500 came from the
        // post-commit spend read and not from anything earlier in the handler.
        // This is what goes red if someone later moves the read inside the
        // transaction instead of recording that choice.
        let name_after: Result<String, sqlx::Error> =
            sqlx::query_scalar("SELECT name FROM categories WHERE id = $1")
                .bind(cat_id)
                .fetch_one(&pool)
                .await;

        drop_restricted_role(&pool, rpool, &role).await;
        rollup_cleanup(&pool, &[user]).await;

        let Json(updated) = control.expect("update_category on the unrestricted pool");
        assert_eq!(updated.name, "Larder", "the rename applied");
        assert_eq!(updated.carried_amount, 20.0, "limit 100 less previous spend 80 carries 20");
        assert_eq!(updated.effective_amount, 120.0, "effective is base 100 + carry 20");

        let (status, _msg) = faulted
            .err()
            .expect("a failed prev-period spend read must not return 200 with an invented carry");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        assert_eq!(
            name_after.expect("category row still readable after the 500"),
            "Provisions",
            "the 500 was raised AFTER tx.commit(), so the write is durable",
        );
    }

    // --- #434: delete_category must not report success on a zero-row delete ---

    /// Records `(level, message + rendered fields)` for every tracing event, so a
    /// test can assert not just THAT a warn fired but what context it carried.
    /// `notifications.rs` has a message-only twin of this for #201; #434 needs the
    /// fields too, because the `budget_id`/`category_id`/`user_id` context IS the
    /// contract — the 404 body is deliberately identical to the never-existed
    /// case, so the warn is the only thing separating them.
    #[cfg(test)]
    #[derive(Clone, Default)]
    struct CapturedEvents(std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

    #[cfg(test)]
    struct EventVisitor(String);

    #[cfg(test)]
    impl tracing::field::Visit for EventVisitor {
        // `%foo` (Display) and the message both arrive here; `format_args!`'s
        // Debug renders unquoted, so a UUID lands as its plain hyphenated form.
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push_str(&format!(" {}={}", field.name(), value));
        }
    }

    #[cfg(test)]
    impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for CapturedEvents {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = EventVisitor(String::new());
            event.record(&mut visitor);
            self.0.lock().unwrap().push((*event.metadata().level(), visitor.0));
        }
    }

    #[cfg(test)]
    impl CapturedEvents {
        /// Every WARN-level event whose rendered form contains `needle`. Other
        /// crates (sqlx especially) log through the same subscriber, hence the
        /// level and substring filter.
        fn warns_containing(&self, needle: &str) -> Vec<String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(level, msg)| *level == tracing::Level::WARN && msg.contains(needle))
                .map(|(_, msg)| msg.clone())
                .collect()
        }
    }

    /// Block until some backend on this database is blocked BY `blocker_pid`, or
    /// return a diagnosis. Scoping by `pg_blocking_pids` rather than by matching
    /// the handler's SQL text is what makes this precise. The handler's own
    /// statements before its DELETE are all non-locking reads, so the FIRST
    /// statement that can block on the racing session is the DELETE itself —
    /// anything blocked BY that session at that point IS our handler. The racing
    /// session's lock count is not the point and differs per caller:
    /// `delete_budget_race`'s `DELETE FROM budgets` cascades an extra lock onto
    /// the seeded category, but the handler never touches that row, so the
    /// identification still holds. Keep the fixture light regardless —
    /// `seed_rollup_budget(.., spent = 0.0)` seeds no transactions, so the
    /// racing delete takes no lock through `ON DELETE SET NULL` on a referencing
    /// `transactions` row. This also survives a harmless reformat of the
    /// handler's DELETE, and cannot be satisfied by an unrelated backend — a
    /// second `cargo test` against the same database, or a dev server — the way
    /// a statement-text match could.
    ///
    /// `handler.is_finished()` is checked each pass so an early return from the
    /// handler (a permission, closed-budget or entitlement guard rejecting the
    /// request before the DELETE) is reported as itself, instead of burning the
    /// deadline and then blaming the race for not happening.
    #[cfg(test)]
    async fn await_blocked_by(
        pool: &PgPool,
        blocker_pid: i32,
        handler: &tokio::task::JoinHandle<Result<StatusCode, (StatusCode, String)>>,
        within: std::time::Duration,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + within;
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))",
            )
            .bind(blocker_pid)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("probe pg_stat_activity: {e}"))?;
            if blocked > 0 {
                return Ok(());
            }
            if handler.is_finished() {
                return Err("the handler returned without ever blocking on the racing \
                            session's row lock, so it exited before reaching the DELETE and \
                            the race was never induced"
                    .to_string());
            }
            if std::time::Instant::now() >= deadline {
                return Err("the handler never blocked on the racing session's row lock \
                            within the deadline, so the check-then-delete race was never \
                            induced and this test would be vacuous"
                    .to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Count `DELETE_CATEGORY` audit rows for a budget. Returns `Result` rather
    /// than panicking so `delete_category_race` can propagate with `?` and still
    /// reach its caller's `rollup_cleanup` — a panic here would unwind past it
    /// and strand the fixture rows.
    #[cfg(test)]
    async fn delete_category_audit_count(pool: &PgPool, budget_id: Uuid) -> Result<i64, String> {
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_logs WHERE budget_id = $1 AND action = 'DELETE_CATEGORY'",
        )
        .bind(budget_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("count audit rows: {e}"))
    }

    /// Drive the #434 race and hand back what the handler did, WITHOUT panicking.
    /// Every failure — including the helpers it calls — is an `Err` string rather
    /// than a panic, so the caller always reaches `rollup_cleanup` before it
    /// asserts; otherwise a failure anywhere in here would strand the seeded
    /// user, budget and category in the database.
    #[cfg(test)]
    async fn delete_category_race(
        pool: &PgPool,
        user: Uuid,
    ) -> Result<(Uuid, Uuid, StatusCode, String, i64), String> {
        let budget_id = seed_rollup_budget(pool, user, 100.0, 0.0, false).await;
        let cat_id = only_category_id(pool, budget_id).await;

        // Session B, on its own connection: delete the row but hold the
        // transaction open, so the row stays visible to other snapshots while
        // its lock is held.
        let mut b = pool.begin().await.map_err(|e| format!("open the racing transaction: {e}"))?;
        let b_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *b)
            .await
            .map_err(|e| format!("read the racing session's pid: {e}"))?;
        let deleted_by_b = sqlx::query("DELETE FROM categories WHERE id = $1")
            .bind(cat_id)
            .execute(&mut *b)
            .await
            .map_err(|e| format!("racing delete: {e}"))?
            .rows_affected();
        if deleted_by_b != 1 {
            return Err(format!(
                "the racing session must be the one that wins the row, but it deleted {deleted_by_b}"
            ));
        }

        let handler = tokio::spawn({
            let state = test_state(pool);
            async move {
                delete_category(State(state), Path((budget_id, cat_id)), Extension(user)).await
            }
        });

        await_blocked_by(pool, b_pid, &handler, std::time::Duration::from_secs(10)).await?;
        b.commit().await.map_err(|e| format!("commit the racing delete: {e}"))?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), handler)
            .await
            .map_err(|_| {
                "delete_category did not finish after the racing delete committed".to_string()
            })?
            .map_err(|e| format!("delete_category task panicked: {e}"))?;

        let audit_rows = delete_category_audit_count(pool, budget_id).await?;
        let (status, msg) = match result {
            Ok(ok) => {
                return Err(format!(
                    "a delete that removed nothing reported an unqualified success: {ok}"
                ))
            }
            Err(err) => err,
        };
        Ok((budget_id, cat_id, status, msg, audit_rows))
    }

    /// Drive the #493 race for `delete_transaction` and hand back what the
    /// handler did, WITHOUT panicking. Same contract as `delete_category_race`:
    /// every failure is an `Err` string so the caller always reaches
    /// `rollup_cleanup` before asserting.
    #[cfg(test)]
    async fn delete_transaction_race(
        pool: &PgPool,
        user: Uuid,
    ) -> Result<(Uuid, Uuid, StatusCode, String, i64), String> {
        let budget_id = seed_rollup_budget(pool, user, 100.0, 0.0, false).await;
        let cat_id = only_category_id(pool, budget_id).await;
        let state = test_state(pool);
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 12.5, "coffee").await;

        // Session B, on its own connection: delete the row but hold the
        // transaction open, so the row stays visible to other snapshots while
        // its lock is held.
        let mut b = pool.begin().await.map_err(|e| format!("open the racing transaction: {e}"))?;
        let b_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *b)
            .await
            .map_err(|e| format!("read the racing session's pid: {e}"))?;
        let deleted_by_b = sqlx::query("DELETE FROM transactions WHERE id = $1")
            .bind(tx_id)
            .execute(&mut *b)
            .await
            .map_err(|e| format!("racing delete: {e}"))?
            .rows_affected();
        if deleted_by_b != 1 {
            return Err(format!(
                "the racing session must be the one that wins the row, but it deleted {deleted_by_b}"
            ));
        }

        let handler = tokio::spawn({
            let state = test_state(pool);
            async move {
                delete_transaction(State(state), Path((budget_id, tx_id)), Extension(user)).await
            }
        });

        await_blocked_by(pool, b_pid, &handler, std::time::Duration::from_secs(10)).await?;
        b.commit().await.map_err(|e| format!("commit the racing delete: {e}"))?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), handler)
            .await
            .map_err(|_| {
                "delete_transaction did not finish after the racing delete committed".to_string()
            })?
            .map_err(|e| format!("delete_transaction task panicked: {e}"))?;

        let audit_rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_logs WHERE budget_id = $1 AND action = 'DELETE_TRANSACTION'",
        )
        .bind(budget_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("count audit rows: {e}"))?;

        let (status, msg) = match result {
            Ok(ok) => {
                return Err(format!(
                    "a delete that removed nothing reported an unqualified success: {ok}"
                ))
            }
            Err(err) => err,
        };
        Ok((budget_id, tx_id, status, msg, audit_rows))
    }

    /// Drive the #493 race for `delete_budget` and hand back what the handler
    /// did, WITHOUT panicking. Returns `(deleted, survivor, status, msg,
    /// survivor_is_default)` — the survivor's default flag is read INSIDE the
    /// helper so a panic in the test body cannot strand the fixture. Same
    /// contract as `delete_category_race`: every failure is an `Err` string.
    #[cfg(test)]
    async fn delete_budget_race(
        pool: &PgPool,
        user: Uuid,
    ) -> Result<(Uuid, Uuid, StatusCode, String, bool), String> {
        let deleted = seed_rollup_budget(pool, user, 100.0, 0.0, false).await;
        let survivor = seed_rollup_budget(pool, user, 75.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
            .bind(deleted)
            .execute(pool)
            .await
            .map_err(|e| format!("mark deleted budget as default: {e}"))?;

        // Session B, on its own connection: delete the row but hold the
        // transaction open, so the row stays visible to other snapshots while
        // its lock is held.
        let mut b = pool.begin().await.map_err(|e| format!("open the racing transaction: {e}"))?;
        let b_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *b)
            .await
            .map_err(|e| format!("read the racing session's pid: {e}"))?;
        let deleted_by_b = sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(deleted)
            .execute(&mut *b)
            .await
            .map_err(|e| format!("racing delete: {e}"))?
            .rows_affected();
        if deleted_by_b != 1 {
            return Err(format!(
                "the racing session must be the one that wins the row, but it deleted {deleted_by_b}"
            ));
        }

        let handler = tokio::spawn({
            let state = test_state(pool);
            async move { delete_budget(State(state), Path(deleted), Extension(user)).await }
        });

        await_blocked_by(pool, b_pid, &handler, std::time::Duration::from_secs(10)).await?;
        b.commit().await.map_err(|e| format!("commit the racing delete: {e}"))?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), handler)
            .await
            .map_err(|_| {
                "delete_budget did not finish after the racing delete committed".to_string()
            })?
            .map_err(|e| format!("delete_budget task panicked: {e}"))?;

        let survivor_is_default: bool =
            sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
                .bind(survivor)
                .fetch_one(pool)
                .await
                .map_err(|e| format!("read survivor is_default: {e}"))?;

        let (status, msg) = match result {
            Ok(ok) => {
                return Err(format!(
                    "a delete that removed nothing reported an unqualified success: {ok}"
                ))
            }
            Err(err) => err,
        };
        Ok((deleted, survivor, status, msg, survivor_is_default))
    }

    /// #434. `delete_category` reads the category's name, then issues a separate
    /// `DELETE`. A concurrent deleter can commit in that window, leaving OUR
    /// delete matching zero rows — the handler must not then claim success.
    ///
    /// The race is induced deterministically, not raced for, and every step of
    /// the ordering is observed rather than assumed: `deleted_by_b != 1` pins
    /// that the racing session really took the row, `await_blocked_by` pins that
    /// the handler really reached its DELETE and blocked on that session (so the
    /// handler's non-locking SELECT must have seen the row — had it missed, the
    /// handler would have 404'd at the existence check without ever issuing the
    /// DELETE, and the poll would have timed out), and the assertions below pin
    /// the outcome. Against the pre-fix handler this fails with 204.
    ///
    /// `flavor = "current_thread"` is load-bearing: `set_default` installs the
    /// capturing subscriber thread-locally, and the handler runs in a spawned
    /// task, so it only shares that thread on the single-threaded runtime. A
    /// later switch to the multi-thread flavor would silently stop capturing.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_category_reports_404_when_the_row_vanishes_before_the_delete() {
        let captured = CapturedEvents::default();
        let _log_guard = tracing::subscriber::set_default(
            tracing_subscriber::layer::SubscriberExt::with(
                tracing_subscriber::registry(),
                captured.clone(),
            ),
        );

        let (pool, user) = rollup_test_setup().await;
        let outcome = delete_category_race(&pool, user).await;
        rollup_cleanup(&pool, &[user]).await;

        let (budget_id, cat_id, status, msg, audit_rows) =
            outcome.expect("the #434 race must leave delete_category reporting a failure");

        assert_eq!(status, StatusCode::NOT_FOUND, "a zero-row delete is a 404, not a 204");
        assert_eq!(
            msg, "Category not found",
            "the body is deliberately the same as the never-existed 404 — the warn, not the \
             response, is what distinguishes them",
        );
        assert_eq!(
            audit_rows, 0,
            "a delete that removed nothing must not write a DELETE_CATEGORY audit row claiming it did",
        );

        // The warn is the ONLY observable separating this 404 from the
        // never-existed one, so it is part of the contract. Without this the
        // whole tracing call could be deleted and both tests would still pass.
        let warns = captured.warns_containing("delete_category matched zero rows");
        assert_eq!(warns.len(), 1, "exactly one #434 warn, got {warns:?}");
        let warn = &warns[0];
        for (label, id) in [("budget_id", budget_id), ("category_id", cat_id), ("user_id", user)] {
            assert!(warn.contains(&id.to_string()), "warn must carry {label}, got: {warn}");
        }
    }

    /// #434's second AC: the ordinary path is untouched — one row deleted, 204,
    /// and exactly one audit row naming the category. Without this the fix could
    /// satisfy the first AC by 404ing every delete.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_category_still_returns_204_for_a_normal_single_row_delete() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        let result =
            delete_category(State(test_state(&pool)), Path((budget_id, cat_id)), Extension(user)).await;

        // Both reads are bound as `Result` and asserted only AFTER cleanup:
        // they must run before `rollup_cleanup` deletes the rows they inspect,
        // but a panic here would strand the fixture. Same shape as the #432
        // tests above.
        let still_there: Result<i64, sqlx::Error> =
            sqlx::query_scalar("SELECT count(*) FROM categories WHERE id = $1")
                .bind(cat_id)
                .fetch_one(&pool)
                .await;
        // Pins the pre-delete name read, which is otherwise consumed only here.
        let details: Result<Vec<String>, sqlx::Error> = sqlx::query_scalar(
            "SELECT details FROM audit_logs WHERE budget_id = $1 AND action = 'DELETE_CATEGORY'",
        )
        .bind(budget_id)
        .fetch_all(&pool)
        .await;
        rollup_cleanup(&pool, &[user]).await;

        assert_eq!(result.expect("a normal delete succeeds"), StatusCode::NO_CONTENT);
        assert_eq!(still_there.expect("count the category row"), 0, "the category row is gone");
        assert_eq!(
            details.expect("read audit details"),
            vec!["Deleted category: Food".to_string()],
            "a real deletion is audited exactly once, naming the category",
        );
    }

    // A linked rollup mirror category is system-managed (#52): `update_category`
    // must reject direct edits with 409, since changing its `category_type` would
    // silently drop the rolled-up source from the parent total and setting a limit
    // would corrupt the computed-amount model. The mirror is managed via
    // rollup/unlink, not by editing the category. A normal category still updates.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_category_rejects_edits_to_mirror() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state.clone()),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        let mirror_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(source)
        .fetch_one(&pool)
        .await
        .expect("mirror category exists");

        // Attempting to set a limit on the mirror is rejected with 409.
        let res = update_category(
            State(state.clone()),
            Path((parent, mirror_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: None,
                category_limit: Some(500.0),
                rollover_enabled: None,
                is_fund: None,
            }),
        )
        .await;
        match res {
            Err((status, _)) => assert_eq!(status, StatusCode::CONFLICT, "mirror edit returns 409"),
            Ok(_) => panic!("editing a mirror category should be rejected"),
        }

        // The mirror row is untouched: still a NULL-limit expense mirror.
        let (ct, lim): (String, Option<f64>) = sqlx::query_as(
            "SELECT category_type, category_limit FROM categories WHERE id = $1",
        )
        .bind(mirror_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ct, "expense", "mirror type unchanged");
        assert_eq!(lim, None, "mirror limit still NULL after a rejected edit");

        // A normal (non-mirror) category in the same parent still updates fine.
        let normal_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM categories WHERE budget_id = $1 AND linked_budget_id IS NULL LIMIT 1",
        )
        .bind(parent)
        .fetch_one(&pool)
        .await
        .expect("parent has its own seeded category");
        let Json(updated) = update_category(
            State(state.clone()),
            Path((parent, normal_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: None,
                category_limit: Some(123.0),
                rollover_enabled: None,
                is_fund: None,
            }),
        )
        .await
        .expect("normal category updates");
        assert_eq!(updated.category_limit, Some(123.0), "normal category edit applies");

        rollup_cleanup(&pool, &[user]).await;
    }

    // The fund toggle on PUT /categories/:id (#426) resolves the intended
    // end-state BEFORE writing, so the data layer's
    // `categories_is_fund_expense_only_check` can never be reached: an
    // is_fund + non-expense pair is a 400 with NOTHING written (no partial
    // write of the other fields), and a type change paired with turning the
    // fund off succeeds because the disable is sequenced before the UPDATE.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_category_fund_toggle_orders_writes_around_the_expense_check() {
        let (pool, user) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let state = test_state(&pool);
        let fund_id = seed_fund_category(&pool, budget, 50.0, 25.0, Utc::now()).await;

        let read = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, bool, f64, String)>(
                    "SELECT category_type, is_fund, fund_balance, name FROM categories WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("read category")
            }
        };

        // 1. Retyping a fund to income without clearing the fund is a 400 — not
        //    the 500 the raw CHECK violation would produce — and the type is
        //    left untouched.
        let res = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: Some("income".to_string()),
                category_limit: None,
                rollover_enabled: None,
                is_fund: None,
            }),
        )
        .await;
        match res {
            Err((status, _)) => assert_eq!(status, StatusCode::BAD_REQUEST, "fund + income is 400"),
            Ok(_) => panic!("retyping a fund to income should be rejected"),
        }
        let (ct, fund, _, _) = read(fund_id).await;
        assert_eq!(ct, "expense", "rejected retype leaves the type unchanged");
        assert!(fund, "rejected retype leaves fund status unchanged");

        // 2. Clearing the fund in the SAME request lets the retype through: the
        //    disable runs before the UPDATE, so the CHECK is never transiently
        //    violated. The balance survives (non-destructive disable).
        let Json(updated) = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: Some("income".to_string()),
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(false),
            }),
        )
        .await
        .expect("clearing the fund alongside the retype succeeds");
        assert_eq!(updated.category_type, "income", "type changed");
        assert!(!updated.is_fund, "fund cleared");
        let (ct, fund, bal, name_before) = read(fund_id).await;
        assert_eq!(ct, "income");
        assert!(!fund);
        assert_eq!(bal, 25.0, "disable preserves the accrued balance");

        // 3. Turning the fund ON for a non-expense category is a 400 and writes
        //    NOTHING — the name change bundled into the same request must not
        //    have landed.
        let res = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: Some("should-not-apply".to_string()),
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(true),
            }),
        )
        .await;
        match res {
            Err((status, _)) => {
                assert_eq!(status, StatusCode::BAD_REQUEST, "fund on an income category is 400")
            }
            Ok(_) => panic!("enabling a fund on an income category should be rejected"),
        }
        let (_, fund, _, name_after) = read(fund_id).await;
        assert!(!fund, "rejected enable leaves fund off");
        assert_eq!(name_after, name_before, "rejected enable writes nothing at all");

        // 4. Turning the fund ON for an expense category succeeds, and the
        //    response carries the columns the enable helper wrote.
        let food_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM categories WHERE budget_id = $1 AND name = 'Food'",
        )
        .bind(budget)
        .fetch_one(&pool)
        .await
        .expect("seeded expense category");
        let Json(enabled) = update_category(
            State(state.clone()),
            Path((budget, food_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(true),
            }),
        )
        .await
        .expect("enabling a fund on an expense category succeeds");
        assert!(enabled.is_fund, "response reflects the enable");
        let marker: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT fund_advanced_through FROM categories WHERE id = $1")
                .bind(food_id)
                .fetch_one(&pool)
                .await
                .expect("read marker");
        assert!(marker.is_some(), "enable sets the idempotency marker");

        // 5. Re-asserting `is_fund: true` on a category that is ALREADY a fund
        //    is a 200 NO-OP. The toggle is driven by the payload alone (no
        //    stale-snapshot gate), so the helper does run — but its
        //    `is_fund = FALSE` guard matches nothing, leaving `fund_balance`
        //    and `fund_advanced_through` exactly as they stand. A repeat enable
        //    must never reset an accrued balance.
        //    The marker is backdated first so the assertion below is sharp: a
        //    helper that actually fired would rewrite it to the CURRENT period
        //    start, which is nowhere near a year ago.
        sqlx::query(
            "UPDATE categories SET fund_balance = 40, fund_advanced_through = $2 WHERE id = $1",
        )
        .bind(food_id)
        .bind(Utc::now() - chrono::Duration::days(400))
        .execute(&pool)
        .await
        .expect("seed an accrued balance and a backdated marker on the fund");
        // Read the marker back so the comparison uses Postgres' own microsecond
        // rounding rather than the nanosecond value that went in.
        let (_, stale_marker) = fund_state_of(&pool, food_id).await;
        let Json(again) = update_category(
            State(state.clone()),
            Path((budget, food_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(true),
            }),
        )
        .await
        .expect("re-enabling an existing fund succeeds");
        assert!(again.is_fund, "repeat enable still reports the fund on");
        assert_eq!(
            again.fund_balance, 40.0,
            "repeat enable does not reset the accrued balance"
        );
        let (bal_after, marker_after) = fund_state_of(&pool, food_id).await;
        assert_eq!(bal_after, 40.0, "stored balance survives the repeat enable");
        assert_eq!(
            marker_after, stale_marker,
            "repeat enable leaves the advance marker untouched — the SQL guard, \
             not a pre-read, is what makes it a no-op"
        );

        // 6. And the mirror image: `is_fund: false` on a category that is
        //    already NOT a fund is a 200, not an error, and leaves the row as
        //    it stands.
        //
        //    NOT a guard regression test, unlike step 5 — do not read it as
        //    one. `disable_category_fund` only ever writes `is_fund = FALSE`
        //    and never touches `fund_balance`, so both assertions below hold
        //    identically whether the `is_fund = TRUE` guard matched or not.
        //    Step 5 is sharp only because the ENABLE helper rewrites
        //    `fund_advanced_through` and the marker was deliberately
        //    backdated, which gives a fired helper somewhere to show. There is
        //    no equivalent observable on the disable side; what this step does
        //    cover is that a redundant disable is a clean 200 rather than a
        //    500 or a 404.
        let Json(off_again) = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(false),
            }),
        )
        .await
        .expect("re-disabling a non-fund succeeds");
        assert!(!off_again.is_fund, "repeat disable still reports the fund off");
        let (_, fund, bal, _) = read(fund_id).await;
        assert!(!fund, "repeat disable leaves fund off");
        assert_eq!(bal, 25.0, "repeat disable preserves the accrued balance");

        // 7. The ENABLE-side mirror of step 2, and the reason the enable is
        //    sequenced AFTER the main UPDATE. `fund_id` is an INCOME category
        //    right now; retyping it to expense while switching the fund ON in
        //    the SAME request is a supported payload (`target_type` resolves
        //    from the payload, so the pre-write check passes). If someone
        //    hoisted `enable_category_fund` above the UPDATE, the row would
        //    still be `income` when `is_fund = TRUE` landed and
        //    `categories_is_fund_expense_only_check` would reject it as a 500.
        //    Step 4 cannot catch that: it targets an already-expense category
        //    and sends no `category_type`, so it passes under either ordering.
        let Json(retyped_on) = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: None,
                category_type: Some("expense".to_string()),
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(true),
            }),
        )
        .await
        .expect("retyping to expense while enabling the fund succeeds");
        assert_eq!(
            retyped_on.category_type, "expense",
            "the retype landed before the enable"
        );
        assert!(retyped_on.is_fund, "the enable landed after the retype");
        let (ct, fund, _, _) = read(fund_id).await;
        assert_eq!(ct, "expense", "stored type is expense");
        assert!(fund, "stored fund flag is on");
        // `fund_state_of` unwraps the marker, so this asserts it is NOT NULL —
        // i.e. the enable helper genuinely ran rather than the UPDATE alone
        // having flipped nothing.
        let (bal_after, _marker) = fund_state_of(&pool, fund_id).await;
        assert_eq!(
            bal_after, 25.0,
            "a RE-enable resumes from the preserved balance rather than resetting it"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Fix A's regression guard (#426): the category update path is ONE
    // transaction, so a request that both renames onto a TAKEN name and clears
    // the fund must roll the disable back with the 409.
    //
    // Before the transaction this test failed on the last assertion: the
    // disable committed as its own statement, then the main UPDATE tripped
    // `unique_category_name_per_budget` and returned 409. The client was told
    // only that the name was taken, while the fund had already been switched
    // off and its accrual silently stopped.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_category_rolls_back_the_fund_disable_when_the_rename_conflicts() {
        let (pool, user) = rollup_test_setup().await;
        // `seed_rollup_budget` seeds one expense category named 'Food'; that is
        // the name the rename below collides with.
        let budget = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let state = test_state(&pool);
        let marker = Utc::now() - chrono::Duration::days(3);
        let fund_id = seed_fund_category(&pool, budget, 50.0, 25.0, marker).await;

        let res = update_category(
            State(state.clone()),
            Path((budget, fund_id)),
            Extension(user),
            Json(CategoryUpdatePayload {
                name: Some("Food".to_string()),
                category_type: None,
                category_limit: None,
                rollover_enabled: None,
                is_fund: Some(false),
            }),
        )
        .await;
        match res {
            Err((status, msg)) => {
                assert_eq!(status, StatusCode::CONFLICT, "a duplicate name is a 409");
                assert!(
                    msg.contains("already exists"),
                    "the 409 names the real problem, got: {msg}"
                );
            }
            Ok(_) => panic!("renaming onto a taken name must be rejected"),
        }

        let (name, is_fund, bal): (String, bool, f64) = sqlx::query_as(
            "SELECT name, is_fund, fund_balance FROM categories WHERE id = $1",
        )
        .bind(fund_id)
        .fetch_one(&pool)
        .await
        .expect("read category back");
        assert_ne!(name, "Food", "the rejected rename did not land");
        assert!(
            is_fund,
            "the fund disable rolled back with the 409 — a rejected request must \
             not leave the fund switched off and its accrual stopped"
        );
        assert_eq!(bal, 25.0, "the balance is untouched");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Linking the same source twice creates exactly ONE mirror category: the unique
    // partial index + ON CONFLICT DO NOTHING make the link idempotent (#52).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn link_idempotent_no_duplicate_mirror() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        for _ in 0..2 {
            let _ = link_rollup(
                State(state.clone()),
                Path(parent),
                Extension(user),
                Json(RollupPayload { child_budget_id: source }),
            )
            .await
            .expect("link is idempotent");
        }

        let mirrors: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(mirrors, 1, "exactly one mirror category despite double link");

        // Total is still single-counted: own 100 + mirrored 200.
        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("total"),
            300.0,
            "no double-counting from the duplicate link attempt"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Batched `computed_budget_totals` must agree with the per-budget
    // `computed_budget_total` for a rollup parent: both resolve the mirror one
    // level deep, so the parent's entry in the batched map equals the single read
    // (own 100 + mirrored 200 = 300). Guards against the two paths drifting (#52).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn computed_budget_totals_batched_parity_for_rollup_parent() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");

        let single = computed_budget_total(&pool, parent).await.expect("single total");
        let batched = computed_budget_totals(&pool, &[parent]).await.expect("batched totals");
        assert_eq!(single, 300.0, "single total = own 100 + mirrored 200");
        assert_eq!(
            batched.get(&parent).copied(),
            Some(single),
            "batched map entry for the parent equals the per-budget total"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // ON DELETE CASCADE: deleting the SOURCE budget removes its mirror category
    // from the parent (the FK `categories.linked_budget_id ... ON DELETE CASCADE`),
    // and the parent's computed total drops back to its own (#52). The backlink FK
    // is ON DELETE SET NULL on the child side, so deleting the source needs no
    // pre-clear of `rollup_parent_id` (the source row itself is gone).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn deleting_source_cascades_mirror_and_restores_total() {
        let (pool, user) = rollup_test_setup().await;
        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 200.0, 0.0, false).await;
        let state = test_state(&pool);

        let _ = link_rollup(
            State(state),
            Path(parent),
            Extension(user),
            Json(RollupPayload { child_budget_id: source }),
        )
        .await
        .expect("link source into parent");
        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("linked total"),
            300.0,
            "parent total = own 100 + mirrored source 200 before delete"
        );

        // Delete the source budget's dependents then the source itself (transactions
        // and the source's own categories FK to the source budget; the mirror in the
        // PARENT cascades on the source delete via linked_budget_id ON DELETE CASCADE).
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(source).execute(&pool).await.expect("del source txns");
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(source).execute(&pool).await.expect("del source cats");
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(source).execute(&pool).await.expect("delete source budget");

        // The mirror in the parent is gone (cascaded), and the total is restored.
        let mirrors: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM categories WHERE budget_id = $1 AND linked_budget_id = $2",
        )
        .bind(parent)
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(mirrors, 0, "mirror category cascaded away when the source was deleted");
        assert_eq!(
            computed_budget_total(&pool, parent).await.expect("restored total"),
            100.0,
            "parent total restored to its own 100 after the source was deleted"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // Running the hourly tick twice within a period renews a due budget AT MOST
    // ONCE: the first run advances the marker into the future, so the second run
    // no longer finds it due. This is the core idempotency guarantee.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn renew_due_budgets_is_idempotent_within_a_period() {
        let (pool, user_id) = renew_test_setup().await;

        // A monthly auto-renew budget whose marker is in the past -> due now.
        let past = Utc::now() - chrono::Duration::days(1);
        let id = seed_budget(&pool, user_id, "monthly", "time_based", true, Some(past), false, false).await;

        // First tick renews it and advances the marker to a FUTURE boundary.
        let n1 = renew_due_budgets(&pool).await.expect("first renew");
        let after_first = marker_of(&pool, id).await.expect("marker set");
        assert!(n1 >= 1, "the due budget is renewed on the first tick");
        assert!(after_first > Utc::now(), "marker advanced into the future");
        assert_eq!(
            after_first,
            next_period_boundary("monthly", Utc::now()),
            "marker advanced to the next monthly boundary"
        );

        // Second tick within the same period: the marker is now in the future,
        // so THIS budget is not renewed again and its marker is unchanged.
        let _ = renew_due_budgets(&pool).await.expect("second renew");
        let after_second = marker_of(&pool, id).await.expect("marker still set");
        assert_eq!(after_second, after_first, "second tick did not re-renew this budget");

        // Exactly one AUTO_RENEW_BUDGET audit row was written for this budget.
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'AUTO_RENEW_BUDGET'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("count audits");
        assert_eq!(audit_count, 1, "renewed exactly once");

        cleanup(&pool, user_id).await;
    }

    // Project, closed, and archived budgets are EXCLUDED from auto-renew even when
    // their marker is past-due; a plain time-based active budget IS renewed.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn renew_due_budgets_excludes_project_closed_archived() {
        let (pool, user_id) = renew_test_setup().await;
        let past = Utc::now() - chrono::Duration::days(1);

        // Eligible control: time-based, active, due.
        let eligible =
            seed_budget(&pool, user_id, "monthly", "time_based", true, Some(past), false, false).await;
        // Excluded: archived TIME-BASED budget with auto_renew on and a past-due
        // marker. Archiving (#50) does not conflict with time_based, so this is the
        // sharpest test that the archived filter excludes an otherwise-eligible row.
        let archived =
            seed_budget(&pool, user_id, "monthly", "time_based", true, Some(past), false, true).await;

        // A project budget cannot have auto_renew = TRUE (the CHECK constraint
        // forbids it), and only a project may be closed (#48's CHECK). So the
        // closed and project exclusions are tested via project budgets with
        // auto_renew = FALSE and a past-due marker; neither may ever be renewed.
        let project =
            seed_budget(&pool, user_id, "monthly", "project", false, Some(past), false, false).await;
        let closed =
            seed_budget(&pool, user_id, "monthly", "project", false, Some(past), true, false).await;

        let _ = renew_due_budgets(&pool).await.expect("renew");

        // Only the eligible budget advanced its marker; the excluded ones kept the
        // original past-due marker untouched.
        assert_eq!(
            marker_of(&pool, eligible).await,
            Some(next_period_boundary("monthly", Utc::now())),
            "eligible time-based budget renewed"
        );
        // Excluded budgets are NOT renewed: their marker stays in the past rather
        // than being advanced to a future boundary. (Compare against `now`, not the
        // seeded `past`, since Postgres timestamptz truncates to microseconds.)
        let now = Utc::now();
        let unchanged = |m: Option<DateTime<Utc>>| matches!(m, Some(t) if t < now);
        assert!(unchanged(marker_of(&pool, closed).await), "closed excluded (marker not advanced)");
        assert!(unchanged(marker_of(&pool, archived).await), "archived excluded (marker not advanced)");
        assert!(unchanged(marker_of(&pool, project).await), "project excluded (marker not advanced)");

        cleanup(&pool, user_id).await;
    }

    // Running the hourly tick twice within a period advances a due fund
    // category AT MOST ONCE: the first run advances the marker past `now`, so
    // the second run finds nothing left to cross. Mirrors
    // renew_due_budgets_is_idempotent_within_a_period (#51's test shape).
    #[tokio::test]
    #[serial]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_is_idempotent_within_a_period() {
        let (pool, user_id) = renew_test_setup().await;
        let budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;

        // Marker one full period ago, derived from the CURRENT period's start
        // via calendar month arithmetic.
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let marker = Utc.with_ymd_and_hms(py, pm, 1, 0, 0, 0).unwrap();

        let cat = seed_fund_category(&pool, budget, 100.0, 0.0, marker).await;

        // Spend 60 in that one completed period -> delta = 100 - 60 = 40.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 60.0, $4, 'seed spend')",
        )
        .bind(Uuid::new_v4())
        .bind(budget)
        .bind(cat)
        .bind(marker + chrono::Duration::days(5))
        .execute(&pool)
        .await
        .expect("seed transaction");

        let n1 = advance_fund_categories(&pool).await.expect("first advance");
        assert!(n1 >= 1, "the due category advances on the first tick");
        let (balance_after_first, marker_after_first) = fund_state_of(&pool, cat).await;
        assert_eq!(balance_after_first, 40.0, "balance = limit(100) - spent(60)");
        assert_eq!(marker_after_first, cur_start, "marker advanced to the current period start");

        // Second tick within the same period: nothing left to cross.
        let _ = advance_fund_categories(&pool).await.expect("second advance");
        let (balance_after_second, marker_after_second) = fund_state_of(&pool, cat).await;
        assert_eq!(balance_after_second, balance_after_first, "second tick did not re-advance");
        assert_eq!(marker_after_second, marker_after_first, "marker unchanged on second tick");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'ADVANCE_FUND_CATEGORY'",
        )
        .bind(budget)
        .fetch_one(&pool)
        .await
        .expect("count audits");
        assert_eq!(audit_count, 1, "advanced exactly once");

        cleanup(&pool, user_id).await;
    }

    // A fund category whose marker is 3 whole months stale (simulated
    // downtime) advances through 3 SEPARATE increments — not a single
    // catch-up jump like #51's renewal — landing on the correct cumulative
    // balance. This directly exercises the ticket's Jan->Apr worked example
    // as a catch-up scenario.
    #[tokio::test]
    #[serial]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_multi_period_catchup() {
        let (pool, user_id) = renew_test_setup().await;
        let budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;

        // Marker 3 months before the current period's start.
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let mut yy = y;
        let mut mm = m as i32 - 3;
        while mm <= 0 {
            mm += 12;
            yy -= 1;
        }
        let marker = Utc.with_ymd_and_hms(yy, mm as u32, 1, 0, 0, 0).unwrap();

        let cat = seed_fund_category(&pool, budget, 100.0, 0.0, marker).await;

        // Three completed periods' worth of spend: 50, 70, 200 (mirroring the
        // ticket's Jan/Feb/Mar rows). Each transaction is dated 5 days into
        // its respective month so it lands inside that period's window.
        for (i, spent) in [50.0_f64, 70.0, 200.0].iter().enumerate() {
            let mut yy2 = yy;
            let mut mm2 = mm + i as i32;
            while mm2 > 12 {
                mm2 -= 12;
                yy2 += 1;
            }
            let tx_date = Utc.with_ymd_and_hms(yy2, mm2 as u32, 5, 0, 0, 0).unwrap();
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
                 VALUES ($1, $2, $3, $4, $5, 'seed spend')",
            )
            .bind(Uuid::new_v4())
            .bind(budget)
            .bind(cat)
            .bind(*spent)
            .bind(tx_date)
            .execute(&pool)
            .await
            .expect("seed transaction");
        }

        let n = advance_fund_categories(&pool).await.expect("catch-up advance");
        assert!(n >= 1, "the stale category advances");

        // Matches the ticket's worked example: balance after Jan(+50) Feb(+30->80) Mar(-100->-20).
        let (balance, marker_after) = fund_state_of(&pool, cat).await;
        let expected = (100.0 - 50.0) + (100.0 - 70.0) + (100.0 - 200.0);
        assert_eq!(balance, expected, "cumulative balance across 3 caught-up periods");
        assert_eq!(balance, -20.0, "matches the ticket's worked-example Mar balance");
        assert_eq!(marker_after, cur_start, "marker caught up to the current period start");

        cleanup(&pool, user_id).await;
    }

    // Project, closed, and archived budgets' fund categories are EXCLUDED
    // from advancement even when their marker is past-due; a plain
    // time-based active budget's fund category IS advanced. Mirrors
    // renew_due_budgets_excludes_project_closed_archived's shape.
    #[tokio::test]
    #[serial]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_excludes_project_closed_archived() {
        let (pool, user_id) = renew_test_setup().await;
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let marker = Utc.with_ymd_and_hms(py, pm, 1, 0, 0, 0).unwrap();

        let eligible_budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;
        let eligible = seed_fund_category(&pool, eligible_budget, 100.0, 0.0, marker).await;

        let archived_budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, true).await;
        let archived = seed_fund_category(&pool, archived_budget, 100.0, 0.0, marker).await;

        let project_budget = seed_budget(&pool, user_id, "monthly", "project", false, None, false, false).await;
        let project = seed_fund_category(&pool, project_budget, 100.0, 0.0, marker).await;

        let closed_budget = seed_budget(&pool, user_id, "monthly", "project", false, None, true, false).await;
        let closed = seed_fund_category(&pool, closed_budget, 100.0, 0.0, marker).await;

        let _ = advance_fund_categories(&pool).await.expect("advance");

        let (eligible_balance, eligible_marker) = fund_state_of(&pool, eligible).await;
        assert_eq!(eligible_balance, 100.0, "eligible category advanced (0 spend -> +100)");
        assert_eq!(eligible_marker, cur_start, "eligible marker advanced");

        let unchanged = |m: DateTime<Utc>| m == marker;
        let (archived_balance, archived_marker) = fund_state_of(&pool, archived).await;
        assert_eq!(archived_balance, 0.0, "archived excluded (balance untouched)");
        assert!(unchanged(archived_marker), "archived excluded (marker not advanced)");

        let (project_balance, project_marker) = fund_state_of(&pool, project).await;
        assert_eq!(project_balance, 0.0, "project excluded (balance untouched)");
        assert!(unchanged(project_marker), "project excluded (marker not advanced)");

        let (closed_balance, closed_marker) = fund_state_of(&pool, closed).await;
        assert_eq!(closed_balance, 0.0, "closed excluded (balance untouched)");
        assert!(unchanged(closed_marker), "closed excluded (marker not advanced)");

        cleanup(&pool, user_id).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn log_user_audit_writes_user_scoped_row() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("audit-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        log_user_audit(&pool, user_id, "AI_REPORT_ISSUE", "Filed issue #1: x").await;

        let row = sqlx::query(
            "SELECT budget_id, user_id, action, details FROM audit_logs \
             WHERE user_id = $1 AND action = 'AI_REPORT_ISSUE'",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("user-scoped audit row written");

        let budget_id: Option<Uuid> = row.get("budget_id");
        let got_user: Uuid = row.get("user_id");
        let action: String = row.get("action");
        let details: String = row.get("details");
        assert!(budget_id.is_none(), "user-scoped row must have NULL budget_id");
        assert_eq!(got_user, user_id);
        assert_eq!(action, "AI_REPORT_ISSUE");
        assert_eq!(details, "Filed issue #1: x");

        // audit_logs.user_id is ON DELETE SET NULL, so delete the row explicitly first.
        let _ = sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(user_id).execute(&pool).await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn log_audit_still_writes_budget_scoped_row_after_nullable_migration() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("audit-b-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'Audit Test', 'monthly', 0)")
            .bind(budget_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("seed budget");

        log_audit(&pool, budget_id, user_id, "AI_TEST", "scoped").await;

        let got: Option<Uuid> = sqlx::query_scalar(
            "SELECT budget_id FROM audit_logs WHERE user_id = $1 AND action = 'AI_TEST'",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("budget-scoped audit row written");
        assert_eq!(got, Some(budget_id), "budget-scoped row keeps its budget_id");

        let _ = sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(user_id).execute(&pool).await;
        let _ = sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    // --- #199: PUT update_transaction ---

    /// Fetch the single seeded category id for a budget (seed_rollup_budget makes
    /// exactly one, "Food").
    #[cfg(test)]
    async fn only_category_id(pool: &PgPool, budget_id: Uuid) -> Uuid {
        sqlx::query_scalar("SELECT id FROM categories WHERE budget_id = $1")
            .bind(budget_id)
            .fetch_one(pool)
            .await
            .expect("category id")
    }

    /// Insert a transaction via the create_transaction handler and return its id.
    #[cfg(test)]
    async fn make_transaction(
        state: &AppState,
        user: Uuid,
        budget_id: Uuid,
        category_id: Option<Uuid>,
        amount: f64,
        description: &str,
    ) -> Uuid {
        create_transaction(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(TransactionPayload {
                category_id,
                amount,
                description: description.to_string(),
                transaction_date: None,
            }),
        )
        .await
        .expect("create_transaction Ok")
        .0
        .transaction
        .id
    }

    // #403 P1: build_account_label follows the chat read path's precedence
    // (display_name → institution_name → "linked account") + `••{last4}`, and
    // returns None for non-imported rows. Pure — no DB.
    #[test]
    fn build_account_label_precedence_and_last4() {
        // Non-imported → always None, whatever the (stale) name columns say.
        assert_eq!(
            build_account_label(false, Some("Apple Card"), Some("Goldman"), Some("7793")),
            None,
        );
        // display_name wins over institution_name; last4 appended.
        assert_eq!(
            build_account_label(true, Some("Apple Card"), Some("Goldman Sachs"), Some("7793")),
            Some("Apple Card ••7793".to_string()),
        );
        // Falls back to institution_name when display_name is blank/absent.
        assert_eq!(
            build_account_label(true, Some("  "), Some("Chase"), Some("0001")),
            Some("Chase ••0001".to_string()),
        );
        // Falls back to the generic label when both names are absent.
        assert_eq!(
            build_account_label(true, None, None, None),
            Some("linked account".to_string()),
        );
        // No last4 → no mask suffix.
        assert_eq!(
            build_account_label(true, Some("Wallet"), None, None),
            Some("Wallet".to_string()),
        );
    }

    // #403 P1: the `source` column defaults to 'ai' (so chat ADD / finalize
    // inherit it with no code change), accepts 'imported'/'manual', and the CHECK
    // rejects anything else.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn transaction_source_defaults_to_ai_and_check_rejects_bogus() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        // Insert with no explicit source → defaults to 'ai' (simulates chat ADD).
        let ai_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description) \
             VALUES ($1, $2, $3, 10.0, 'coffee')",
        )
        .bind(ai_id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert ai row");
        let src: String = sqlx::query_scalar("SELECT source FROM transactions WHERE id = $1")
            .bind(ai_id).fetch_one(&pool).await.expect("read source");
        assert_eq!(src, "ai", "no explicit source defaults to ai");

        // Explicit 'imported' persists.
        let imp_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source) \
             VALUES ($1, $2, $3, 20.0, 'sync', 'imported')",
        )
        .bind(imp_id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert imported row");
        let src2: String = sqlx::query_scalar("SELECT source FROM transactions WHERE id = $1")
            .bind(imp_id).fetch_one(&pool).await.expect("read source 2");
        assert_eq!(src2, "imported");

        // CHECK rejects an unknown value.
        let bad = sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source) \
             VALUES ($1, $2, $3, 5.0, 'bad', 'bogus')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(cat_id)
        .execute(&pool).await;
        assert!(bad.is_err(), "CHECK rejects source='bogus'");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P2: the review_status column defaults to 'reviewed' for every non-sync
    // insert path (so existing/Nels-logged rows are never retro-flagged), and the
    // CHECK rejects a non-enum value.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn review_status_defaults_reviewed_and_check_enforced() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        // No explicit review_status → defaults to 'reviewed' (chat ADD / existing rows).
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description) \
             VALUES ($1, $2, $3, 10.0, 'coffee')",
        )
        .bind(id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert row");
        let rs: String = sqlx::query_scalar("SELECT review_status FROM transactions WHERE id = $1")
            .bind(id).fetch_one(&pool).await.expect("read review_status");
        assert_eq!(rs, "reviewed", "no explicit review_status defaults to reviewed");

        // CHECK rejects an unknown value.
        let bad = sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, review_status) \
             VALUES ($1, $2, $3, 5.0, 'bad', 'bogus')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(cat_id)
        .execute(&pool).await;
        assert!(bad.is_err(), "CHECK rejects review_status='bogus'");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P2: approve flips a needs_review row to reviewed, returns the updated
    // TransactionResponse, and writes exactly one audit row. A second approve is an
    // idempotent no-op — no state change, no second audit row.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn approve_transaction_flips_and_is_idempotent_and_audits() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        // A freshly-synced imported row: needs_review.
        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source, review_status) \
             VALUES ($1, $2, $3, 12.50, 'BLUE BOTTLE', 'imported', 'needs_review')",
        )
        .bind(tx_id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert needs_review row");

        let resp = approve_transaction(State(state.clone()), Path((budget_id, tx_id)), Extension(user))
            .await.expect("approve ok").0;
        assert_eq!(resp.review_status, "reviewed", "approve flips to reviewed");
        assert_eq!(resp.id, tx_id);

        let stored: String = sqlx::query_scalar("SELECT review_status FROM transactions WHERE id = $1")
            .bind(tx_id).fetch_one(&pool).await.expect("read review_status");
        assert_eq!(stored, "reviewed", "persisted");

        let audit_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'APPROVE_TRANSACTION'",
        ).bind(budget_id).fetch_one(&pool).await.expect("count audit");
        assert_eq!(audit_after_first, 1, "exactly one audit row after first approve");

        // Idempotent: approving again succeeds and writes NO second audit row.
        let resp2 = approve_transaction(State(state.clone()), Path((budget_id, tx_id)), Extension(user))
            .await.expect("second approve ok (no-op)").0;
        assert_eq!(resp2.review_status, "reviewed");
        let audit_after_second: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'APPROVE_TRANSACTION'",
        ).bind(budget_id).fetch_one(&pool).await.expect("count audit 2");
        assert_eq!(audit_after_second, 1, "idempotent re-approve writes no second audit row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P2: approve requires Owner/Edit — a viewer is rejected with 403 and the
    // row stays needs_review.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn approve_transaction_requires_edit_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, owner, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source, review_status) \
             VALUES ($1, $2, $3, 12.50, 'BLUE BOTTLE', 'imported', 'needs_review')",
        )
        .bind(tx_id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert needs_review row");

        // A second user, granted 'view' only.
        let viewer = Uuid::new_v4();
        let viewer_email = format!("viewer-{viewer}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(viewer).bind(&viewer_email)
            .execute(&pool).await.expect("seed viewer");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(&viewer_email)
        .execute(&pool).await.expect("seed view share");

        let result = approve_transaction(State(state.clone()), Path((budget_id, tx_id)), Extension(viewer)).await;
        assert!(matches!(result, Err((StatusCode::FORBIDDEN, _))), "view permission -> 403");

        let stored: String = sqlx::query_scalar("SELECT review_status FROM transactions WHERE id = $1")
            .bind(tx_id).fetch_one(&pool).await.expect("read review_status");
        assert_eq!(stored, "needs_review", "row unchanged after denied approve");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #403 P2: approving an id that belongs to a different budget (or does not
    // exist) is a 404 via the budget-scoped SELECT — no cross-budget leak.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn approve_transaction_cross_budget_id_is_404() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;

        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source, review_status) \
             VALUES ($1, $2, $3, 9.0, 'in A', 'imported', 'needs_review')",
        )
        .bind(tx_id).bind(budget_a).bind(cat_a)
        .execute(&pool).await.expect("insert row in budget A");

        // Approve the A-owned tx while addressing budget B → 404.
        let result = approve_transaction(State(state.clone()), Path((budget_b, tx_id)), Extension(user)).await;
        assert!(matches!(result, Err((StatusCode::NOT_FOUND, _))), "cross-budget id -> 404");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P3 helpers: seed a Nels-logged twin + a linked imported row already
    // matched to it (the state the sync-path matcher leaves behind).
    #[cfg(test)]
    async fn seed_matched_pair(pool: &PgPool, budget: Uuid, cat: Uuid, amount: f64) -> (Uuid, Uuid) {
        let nels = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, source, review_status) \
             VALUES ($1, $2, $3, $4, 'coffee', 'ai', 'reviewed')",
        )
        .bind(nels).bind(budget).bind(cat).bind(amount)
        .execute(pool).await.expect("insert nels twin");
        let imported = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, currency, source, review_status, matched_transaction_id) \
             VALUES ($1, $2, $3, $4, 'BLUE BOTTLE', 'USD', 'imported', 'needs_review', $5)",
        )
        .bind(imported).bind(budget).bind(cat).bind(amount).bind(nels)
        .execute(pool).await.expect("insert imported twin");
        (nels, imported)
    }

    #[cfg(test)]
    async fn counted_total(pool: &PgPool, budget: Uuid) -> f64 {
        sqlx::query_scalar::<_, Option<f64>>(
            "SELECT COALESCE(SUM(amount), 0) FROM transactions \
             WHERE budget_id = $1 AND excluded_from_budget = false",
        )
        .bind(budget)
        .fetch_one(pool)
        .await
        .expect("sum counted")
        .unwrap_or(0.0)
    }

    // #403 P3: merge excludes the Nels twin (dropping the counted total by exactly
    // one copy), marks the imported row reviewed, keeps the link, writes ONE audit
    // row, and is idempotent (a second merge writes no second audit row). Nothing
    // is deleted.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn resolve_match_merge_de_double_counts_and_is_idempotent() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat = only_category_id(&pool, budget).await;
        let (nels, imported) = seed_matched_pair(&pool, budget, cat, 15.0).await;

        let before = counted_total(&pool, budget).await;
        assert_eq!(before, 30.0, "both twins count before merge");

        let resp = resolve_match(
            State(state.clone()), Path((budget, imported)), Extension(user),
            Json(ResolveMatchPayload { action: "merge".into() }),
        ).await.expect("merge ok").0;
        assert_eq!(resp.review_status, "reviewed", "imported row marked reviewed");
        assert_eq!(resp.matched_transaction_id, Some(nels), "merge keeps the link");

        let after = counted_total(&pool, budget).await;
        assert_eq!(after, 15.0, "counted total drops by exactly one copy");

        // Neither row was deleted — both still exist.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE id IN ($1, $2)")
            .bind(nels).bind(imported).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 2, "non-destructive: both rows remain");
        let nels_excluded: bool = sqlx::query_scalar("SELECT excluded_from_budget FROM transactions WHERE id = $1")
            .bind(nels).fetch_one(&pool).await.unwrap();
        assert!(nels_excluded, "the Nels twin is excluded");

        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'RESOLVE_DUPLICATE'",
        ).bind(budget).fetch_one(&pool).await.unwrap();
        assert_eq!(audits, 1, "one audit row after first merge");

        // Idempotent: a second merge is a no-op with no second audit + total unchanged.
        let _ = resolve_match(
            State(state.clone()), Path((budget, imported)), Extension(user),
            Json(ResolveMatchPayload { action: "merge".into() }),
        ).await.expect("second merge ok");
        assert_eq!(counted_total(&pool, budget).await, 15.0, "idempotent: total unchanged");
        let audits2: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'RESOLVE_DUPLICATE'",
        ).bind(budget).fetch_one(&pool).await.unwrap();
        assert_eq!(audits2, 1, "idempotent re-merge writes no second audit row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P3: dismiss keeps both twins counting, clears the link, marks the
    // imported row reviewed. Non-destructive.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn resolve_match_dismiss_keeps_both_and_clears_link() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat = only_category_id(&pool, budget).await;
        let (_nels, imported) = seed_matched_pair(&pool, budget, cat, 15.0).await;

        let resp = resolve_match(
            State(state.clone()), Path((budget, imported)), Extension(user),
            Json(ResolveMatchPayload { action: "dismiss".into() }),
        ).await.expect("dismiss ok").0;
        assert_eq!(resp.matched_transaction_id, None, "dismiss clears the link");
        assert_eq!(resp.review_status, "reviewed", "imported row marked reviewed");

        assert_eq!(counted_total(&pool, budget).await, 30.0, "both twins still count");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE budget_id = $1")
            .bind(budget).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 2, "non-destructive: both rows remain");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P3: resolve requires Owner/Edit — a viewer is rejected 403 and nothing
    // changes.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn resolve_match_requires_edit_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget = seed_rollup_budget(&pool, owner, 1000.0, 0.0, false).await;
        let cat = only_category_id(&pool, budget).await;
        let (nels, imported) = seed_matched_pair(&pool, budget, cat, 15.0).await;

        let viewer = Uuid::new_v4();
        let viewer_email = format!("viewer-{viewer}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(viewer).bind(&viewer_email).execute(&pool).await.expect("seed viewer");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4()).bind(budget).bind(&viewer_email).execute(&pool).await.expect("seed view share");

        let result = resolve_match(
            State(state.clone()), Path((budget, imported)), Extension(viewer),
            Json(ResolveMatchPayload { action: "merge".into() }),
        ).await;
        assert!(matches!(result, Err((StatusCode::FORBIDDEN, _))), "view permission -> 403");

        let excluded: bool = sqlx::query_scalar("SELECT excluded_from_budget FROM transactions WHERE id = $1")
            .bind(nels).fetch_one(&pool).await.unwrap();
        assert!(!excluded, "denied resolve leaves the Nels twin untouched");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #403 P3: resolving an id in another budget (or unknown) is a 404; an
    // unlinked row is a 200 no-op; a bad action is a 400.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn resolve_match_cross_budget_404_unlinked_noop_and_bad_action_400() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;
        let (_nels, imported) = seed_matched_pair(&pool, budget_a, cat_a, 15.0).await;

        // Cross-budget id → 404.
        let x = resolve_match(
            State(state.clone()), Path((budget_b, imported)), Extension(user),
            Json(ResolveMatchPayload { action: "merge".into() }),
        ).await;
        assert!(matches!(x, Err((StatusCode::NOT_FOUND, _))), "cross-budget id -> 404");

        // Bad action → 400.
        let bad = resolve_match(
            State(state.clone()), Path((budget_a, imported)), Extension(user),
            Json(ResolveMatchPayload { action: "nope".into() }),
        ).await;
        assert!(matches!(bad, Err((StatusCode::BAD_REQUEST, _))), "bad action -> 400");

        // An unlinked (plain ai) row → 200 no-op.
        let plain = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description) \
             VALUES ($1, $2, $3, 5.0, 'plain')",
        )
        .bind(plain).bind(budget_a).bind(cat_a).execute(&pool).await.unwrap();
        let noop = resolve_match(
            State(state.clone()), Path((budget_a, plain)), Extension(user),
            Json(ResolveMatchPayload { action: "dismiss".into() }),
        ).await.expect("unlinked dismiss is a 200 no-op").0;
        assert_eq!(noop.id, plain);
        assert_eq!(noop.matched_transaction_id, None);

        rollup_cleanup(&pool, &[user]).await;
    }

    // #403 P1: list_transactions surfaces source / currency / external_account_id
    // and a built account_label for imported rows, and leaves them null/None for
    // Nels-logged rows.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_transactions_surfaces_provenance_and_account_label() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        // A Nels-logged (ai) row: default source, NULL currency, no linked account.
        let ai_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description) \
             VALUES ($1, $2, $3, 12.50, 'nels coffee')",
        )
        .bind(ai_id).bind(budget_id).bind(cat_id)
        .execute(&pool).await.expect("insert ai row");

        // A linked account + an imported row referencing it.
        let acct_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts \
                (id, budget_id, user_id, provider_account_id, provider_ref, \
                 display_name, institution_name, last4, provider, status) \
             VALUES ($1, $2, $3, $4, 'ref', 'Apple Card', 'Goldman Sachs', '7793', 'stripe', 'active')",
        )
        .bind(acct_id).bind(budget_id).bind(user)
        .bind(format!("pa-{acct_id}"))
        .execute(&pool).await.expect("insert linked account");

        let imp_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, description, external_account_id, \
                 provider_transaction_id, currency, source) \
             VALUES ($1, $2, NULL, 12.50, 'BLUE BOTTLE COFFEE #14', $3, 'ptx-1', 'USD', 'imported')",
        )
        .bind(imp_id).bind(budget_id).bind(acct_id)
        .execute(&pool).await.expect("insert imported row");

        sqlx::query("UPDATE transactions SET provider_transaction_id = 'fctxn_test123' WHERE id = $1")
            .bind(imp_id)
            .execute(&pool)
            .await
            .unwrap();

        let rows = list_transactions(State(state.clone()), Path(budget_id), Extension(user))
            .await
            .expect("list ok")
            .0;

        let ai = rows.iter().find(|r| r.id == ai_id).expect("ai row present");
        assert_eq!(ai.source, "ai");
        assert_eq!(ai.currency, None);
        assert_eq!(ai.external_account_id, None);
        assert_eq!(ai.account_label, None);
        // #403 P2: review_status is surfaced; these rows carry the default 'reviewed'.
        assert_eq!(ai.review_status, "reviewed");

        let imp = rows.iter().find(|r| r.id == imp_id).expect("imported row present");
        assert_eq!(imp.source, "imported");
        assert_eq!(imp.currency.as_deref(), Some("USD"));
        assert_eq!(imp.external_account_id, Some(acct_id));
        assert_eq!(imp.account_label.as_deref(), Some("Apple Card ••7793"));
        assert_eq!(imp.review_status, "reviewed");
        assert_eq!(
            imp.provider_transaction_id.as_deref(),
            Some("fctxn_test123"),
            "imported row surfaces its bank provider transaction id"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // An edit applies the new amount + description, the BEFORE UPDATE trigger
    // bumps updated_at past created_at, and (no GEMINI key locally) a description
    // change clears the embedding to NULL.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_edits_fields_and_bumps_updated_at() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 10.0, "old description").await;

        // At insert created_at == updated_at (the BEFORE UPDATE trigger has not fired).
        let (c0, u0): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT created_at, updated_at FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("timestamps at insert");
        assert_eq!(c0, u0, "created_at == updated_at at insert");

        let updated = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: Some(25.0),
                description: Some("new description".to_string()),
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await
        .expect("update_transaction Ok");
        assert_eq!(updated.0.transaction.amount, 25.0, "amount reflects the edit");
        assert_eq!(updated.0.transaction.description, "new description", "description reflects the edit");

        // Distinct statement -> NOW() differs; the trigger advanced updated_at.
        let (c1, u1): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT created_at, updated_at FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("timestamps after update");
        assert!(u1 > c1, "updated_at bumped past created_at by the trigger");

        // Description edit with no GEMINI key -> embedding stored as NULL.
        let embedding_is_null: bool = sqlx::query_scalar(
            "SELECT embedding IS NULL FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("embedding null check");
        assert!(embedding_is_null, "description edit with no key clears the embedding");

        rollup_cleanup(&pool, &[user]).await;
    }

    // --- #376: shared find-or-create / insert / cleanup helpers ---

    // A case-insensitive match returns the EXISTING category (created=false) and
    // never flips its auto_created flag, even when the caller requests true.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn find_or_create_returns_existing_case_insensitive() {
        let (pool, user) = rollup_test_setup().await;
        // seed_rollup_budget makes exactly one expense category "Food"
        // (auto_created defaults to false).
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let food_id = only_category_id(&pool, budget_id).await;

        let (id, created) =
            find_or_create_category(&pool, budget_id, "food", "expense", true)
                .await
                .expect("find_or_create Ok");
        assert_eq!(id, food_id, "case-insensitive match returns the Food id");
        assert!(!created, "an existing match reports created=false");

        let still_false: bool = sqlx::query_scalar(
            "SELECT auto_created FROM categories WHERE id = $1",
        )
        .bind(food_id)
        .fetch_one(&pool)
        .await
        .expect("fetch auto_created");
        assert!(!still_false, "an existing category's auto_created flag is never flipped");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A miss inserts a new category carrying the requested auto_created flag and
    // category_type.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn find_or_create_creates_with_auto_created_flag() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;

        let (id, created) =
            find_or_create_category(&pool, budget_id, "Haircut", "expense", true)
                .await
                .expect("find_or_create Ok");
        assert!(created, "a fresh name reports created=true");

        let (auto, ctype): (bool, String) = sqlx::query_as(
            "SELECT auto_created, category_type FROM categories WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch new category");
        assert!(auto, "new category carries the requested auto_created=true");
        assert_eq!(ctype, "expense", "new category has the requested type");

        rollup_cleanup(&pool, &[user]).await;
    }

    // insert_transaction_with_embedding inserts a row (embedding NULL with no
    // GEMINI key) with the given amount/description/category.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn insert_transaction_with_embedding_inserts_row() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        let tx_id = insert_transaction_with_embedding(
            &pool, budget_id, Some(cat_id), 42.5, "a snack", user,
        )
        .await
        .expect("insert Ok");

        let (amount, desc, got_cat): (f64, String, Option<Uuid>) = sqlx::query_as(
            "SELECT amount, description, category_id FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("fetch inserted row");
        assert_eq!(amount, 42.5);
        assert_eq!(desc, "a snack");
        assert_eq!(got_cat, Some(cat_id));

        rollup_cleanup(&pool, &[user]).await;
    }

    // An auto_created category with no transactions is removed by cleanup.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn cleanup_removes_empty_auto_created_category() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;

        let (cat_id, _) =
            find_or_create_category(&pool, budget_id, "Ephemeral", "expense", true)
                .await
                .expect("create Ok");

        cleanup_orphaned_auto_category(&pool, Some(cat_id)).await;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1)",
        )
        .bind(cat_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(!exists, "empty auto_created category is deleted");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A user-created (auto_created=false) empty category is preserved by cleanup.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn cleanup_keeps_user_created_empty_category() {
        let (pool, user) = rollup_test_setup().await;
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        // "Food" seeded by seed_rollup_budget is auto_created=false and empty.
        let food_id = only_category_id(&pool, budget_id).await;

        cleanup_orphaned_auto_category(&pool, Some(food_id)).await;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1)",
        )
        .bind(food_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "user-created category is never auto-removed");

        rollup_cleanup(&pool, &[user]).await;
    }

    // An auto_created category that still holds a transaction is preserved.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn cleanup_keeps_nonempty_auto_created_category() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;

        let (cat_id, _) =
            find_or_create_category(&pool, budget_id, "Busy", "expense", true)
                .await
                .expect("create Ok");
        make_transaction(&state, user, budget_id, Some(cat_id), 5.0, "still here").await;

        cleanup_orphaned_auto_category(&pool, Some(cat_id)).await;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1)",
        )
        .bind(cat_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a non-empty auto_created category is preserved");

        rollup_cleanup(&pool, &[user]).await;
    }

    // --- #376: finalize endpoint (resolve a pending category choice) ---

    // finalize with an existing category_id logs the transaction under it and
    // reports category_created=false.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn finalize_with_category_id_inserts_under_category() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;

        let res = finalize_transaction(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(FinalizeTransactionPayload {
                amount: 35.0,
                description: "haircut".to_string(),
                category_id: Some(cat_id),
                new_category_name: None,
            }),
        )
        .await
        .expect("finalize Ok");
        assert_eq!(res.0.transaction.category_id, Some(cat_id));
        assert!(!res.0.category_created, "picking an existing category creates none");

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM transactions WHERE category_id = $1 AND description = 'haircut'",
        )
        .bind(cat_id)
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(count, 1, "the transaction is logged under the chosen category");

        rollup_cleanup(&pool, &[user]).await;
    }

    // finalize with new_category_name creates an auto_created category and logs
    // under it, reporting category_created=true.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn finalize_with_new_name_creates_auto_created_category() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;

        let res = finalize_transaction(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(FinalizeTransactionPayload {
                amount: 35.0,
                description: "haircut".to_string(),
                category_id: None,
                new_category_name: Some("Haircut".to_string()),
            }),
        )
        .await
        .expect("finalize Ok");
        assert!(res.0.category_created, "a fresh name creates a category");

        let (auto, ctype): (bool, String) = sqlx::query_as(
            "SELECT auto_created, category_type FROM categories WHERE budget_id = $1 AND name = 'Haircut'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("fetch new category");
        assert!(auto, "the created category is flagged auto_created");
        assert_eq!(ctype, "expense");
        assert_eq!(res.0.transaction.category_name.as_deref(), Some("Haircut"));

        rollup_cleanup(&pool, &[user]).await;
    }

    // finalize with neither category_id nor new_category_name -> 400.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn finalize_without_category_is_bad_request() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;

        let result = finalize_transaction(
            State(state.clone()),
            Path(budget_id),
            Extension(user),
            Json(FinalizeTransactionPayload {
                amount: 35.0,
                description: "haircut".to_string(),
                category_id: None,
                new_category_name: None,
            }),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::BAD_REQUEST, _))),
            "neither category_id nor new_category_name -> 400",
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // finalize with a category_id from another budget -> 404.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn finalize_foreign_category_is_not_found() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_b = only_category_id(&pool, budget_b).await;

        let result = finalize_transaction(
            State(state.clone()),
            Path(budget_a),
            Extension(user),
            Json(FinalizeTransactionPayload {
                amount: 35.0,
                description: "haircut".to_string(),
                category_id: Some(cat_b),
                new_category_name: None,
            }),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::NOT_FOUND, _))),
            "a foreign category_id -> 404",
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // #376 end-to-end: reassigning a transaction OUT of an auto_created category
    // empties it, and the REST update_transaction cleanup deletes that now-orphaned
    // category — while leaving the user-created destination untouched.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_reassign_removes_emptied_auto_category() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let keep_id = only_category_id(&pool, budget_id).await; // "Food", user-created
        let (auto_id, _) = find_or_create_category(&pool, budget_id, "Haircut", "expense", true)
            .await
            .expect("create auto category");
        let tx_id = make_transaction(&state, user, budget_id, Some(auto_id), 35.0, "haircut").await;

        update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: Some(keep_id),
                amount: None,
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await
        .expect("reassign Ok");

        let auto_gone: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM categories WHERE id = $1)",
        )
        .bind(auto_id)
        .fetch_one(&pool)
        .await
        .expect("check auto category");
        assert!(auto_gone, "the emptied auto_created category is cleaned up on reassign");
        let keep_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM categories WHERE id = $1)",
        )
        .bind(keep_id)
        .fetch_one(&pool)
        .await
        .expect("check destination category");
        assert!(keep_exists, "the user-created destination category is untouched");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Cross-budget / unknown-id guard (#199 AC2): editing budget A's transaction
    // through budget B (owned by the same user) 404s.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_cross_budget_id_is_404() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;
        let tx_a = make_transaction(&state, user, budget_a, Some(cat_a), 10.0, "in A").await;

        let result = update_transaction(
            State(state.clone()),
            Path((budget_b, tx_a)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: Some(99.0),
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::NOT_FOUND, _))),
            "cross-budget id -> 404",
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // A viewer (share with 'view') cannot edit transactions -> 403.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_requires_edit_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, owner, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, owner, budget_id, Some(cat_id), 10.0, "owned").await;

        // A second user, granted 'view' on the owner's budget.
        let viewer = Uuid::new_v4();
        let viewer_email = format!("viewer-{viewer}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(viewer)
            .bind(&viewer_email)
            .execute(&pool)
            .await
            .expect("seed viewer");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed view share");

        let result = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(viewer),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: Some(99.0),
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::FORBIDDEN, _))),
            "view permission -> 403",
        );

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // A non-description edit (amount only) leaves a previously-stored embedding
    // untouched while still bumping updated_at (amount is a user-visible change).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_non_description_edit_leaves_embedding() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 10.0, "keep me").await;

        // Seed a 768-dim embedding directly (mirrors the backfill job's system
        // write; the trigger does NOT fire on an embedding-only update).
        let vec_literal = format!(
            "[{}]",
            (0..768)
                .map(|i| format!("{}", 0.001 * (i as f64 + 1.0)))
                .collect::<Vec<_>>()
                .join(",")
        );
        sqlx::query("UPDATE transactions SET embedding = $1::vector WHERE id = $2")
            .bind(&vec_literal)
            .bind(tx_id)
            .execute(&pool)
            .await
            .expect("seed embedding");
        let emb_before: String = sqlx::query_scalar("SELECT embedding::text FROM transactions WHERE id = $1")
            .bind(tx_id)
            .fetch_one(&pool)
            .await
            .expect("embedding before");

        update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: Some(42.0),
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await
        .expect("update_transaction Ok");

        let emb_after: Option<String> = sqlx::query_scalar("SELECT embedding::text FROM transactions WHERE id = $1")
            .bind(tx_id)
            .fetch_one(&pool)
            .await
            .expect("embedding after");
        assert_eq!(emb_after.as_deref(), Some(emb_before.as_str()), "amount-only edit leaves embedding untouched");

        let (c1, u1): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT created_at, updated_at FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("timestamps after update");
        assert!(u1 > c1, "amount edit still bumps updated_at");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Marking a transaction excluded via the PUT update endpoint sets the flag
    // (returned in the response) and drops it out of period_expense_spent;
    // un-excluding restores it. Guards the Task-3 REST wiring against the
    // Task-2 aggregate filter (#374).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_exclude_toggles_flag_and_aggregate() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 100.0, "groceries").await;

        // Window straddling now() so the freshly-created (now-dated) tx counts.
        let start = Utc::now() - chrono::Duration::days(1);
        let end = Utc::now() + chrono::Duration::days(1);

        let before = period_expense_spent(&pool, budget_id, start, end)
            .await
            .expect("spent before exclude");
        assert!((before - 100.0).abs() < 1e-9, "tx counts before exclusion: {before}");

        // Exclude it.
        let excluded = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: None,
                description: None,
                transaction_date: None,
                excluded_from_budget: Some(true),
            }),
        )
        .await
        .expect("exclude update Ok");
        assert!(excluded.0.transaction.excluded_from_budget, "response reports excluded=true");

        let after_exclude = period_expense_spent(&pool, budget_id, start, end)
            .await
            .expect("spent after exclude");
        assert!(after_exclude.abs() < 1e-9, "excluded tx drops out of the total: {after_exclude}");

        // Un-exclude it.
        let restored = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: None,
                description: None,
                transaction_date: None,
                excluded_from_budget: Some(false),
            }),
        )
        .await
        .expect("un-exclude update Ok");
        assert!(!restored.0.transaction.excluded_from_budget, "response reports excluded=false");

        let after_restore = period_expense_spent(&pool, budget_id, start, end)
            .await
            .expect("spent after restore");
        assert!((after_restore - 100.0).abs() < 1e-9, "un-excluded tx counts again: {after_restore}");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Raising the amount above the category limit returns a non-empty alert.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_amount_raise_over_limit_returns_alert() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        // Small category limit (50) so a raised amount blows past it.
        let budget_id = seed_rollup_budget(&pool, user, 50.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 10.0, "under limit").await;

        let updated = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: None,
                amount: Some(100.0),
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await
        .expect("update_transaction Ok");
        assert!(!updated.0.alerts.is_empty(), "raising amount over the limit returns an alert");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Moving a transaction to a DIFFERENT category re-runs the limit check
    // against the new category (AC4: limit checks re-run on amount OR category
    // change) and writes an EDIT_TRANSACTION audit row.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_category_change_rechecks_limit_and_audits() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        // seed_rollup_budget gives a "Food" category with a generous 1000 limit.
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let food_id = only_category_id(&pool, budget_id).await;
        // A second category "Dining" with a tiny 5.0 limit.
        let dining_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Dining', 'expense', 5.0)",
        )
        .bind(dining_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed Dining category");

        // A $40 transaction in Food (well under Food's 1000 limit, no alert).
        let tx_id = make_transaction(&state, user, budget_id, Some(food_id), 40.0, "lunch").await;

        // Move it to Dining (limit 5) — its $40 now blows past Dining's limit.
        let updated = update_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: Some(dining_id),
                amount: None,
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await
        .expect("update_transaction Ok");

        assert_eq!(
            updated.0.transaction.category_id,
            Some(dining_id),
            "transaction moved to the Dining category"
        );
        assert!(
            !updated.0.alerts.is_empty(),
            "moving spend into an over-limit category re-runs the limit check and alerts"
        );

        // An EDIT_TRANSACTION audit row was written for this budget.
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'EDIT_TRANSACTION'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("count audit rows");
        assert_eq!(audit_count, 1, "exactly one EDIT_TRANSACTION audit row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A category_id from a DIFFERENT budget is rejected (404) rather than
    // silently linking a transaction across budgets (which would also dodge the
    // category-scoped limit check).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn update_transaction_rejects_foreign_category() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;
        let tx_id = make_transaction(&state, user, budget_a, Some(cat_a), 10.0, "in A").await;

        // A second budget (same owner) with its own category.
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_b = only_category_id(&pool, budget_b).await;

        let res = update_transaction(
            State(state.clone()),
            Path((budget_a, tx_id)),
            Extension(user),
            Json(TransactionUpdatePayload {
                category_id: Some(cat_b), // belongs to budget_b, not budget_a
                amount: None,
                description: None,
                transaction_date: None,
                excluded_from_budget: None,
            }),
        )
        .await;
        assert!(
            matches!(res, Err((StatusCode::NOT_FOUND, _))),
            "a category from another budget is rejected"
        );

        rollup_cleanup(&pool, &[user]).await;
    }

    // DELETE /budgets/:id/transactions/:transaction_id (#258): permission,
    // closed-budget, and cross-budget-scope guards mirror delete_category's
    // shape; a successful delete removes the row and writes an audit entry.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_removes_row_and_audits() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 12.5, "coffee").await;

        let status = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
        )
        .await
        .expect("delete_transaction Ok");
        assert_eq!(status, StatusCode::NO_CONTENT);

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(!exists, "transaction row must be gone after delete");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'DELETE_TRANSACTION'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(audit_count, 1, "delete must write exactly one DELETE_TRANSACTION audit row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // #493. `delete_transaction` reads the transaction's description/amount,
    // then issues a separate `DELETE`. A concurrent deleter can commit in that
    // window, leaving OUR delete matching zero rows — the handler must not then
    // claim success, and must NOT write a DELETE_TRANSACTION audit row for a
    // transaction it did not remove. Same deterministic race induction and the
    // same load-bearing `current_thread` flavor as `delete_category`'s #434
    // test. Against the pre-fix handler this fails with 204.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_reports_404_when_the_row_vanishes_before_the_delete() {
        let captured = CapturedEvents::default();
        let _log_guard = tracing::subscriber::set_default(
            tracing_subscriber::layer::SubscriberExt::with(
                tracing_subscriber::registry(),
                captured.clone(),
            ),
        );

        let (pool, user) = rollup_test_setup().await;
        let outcome = delete_transaction_race(&pool, user).await;
        rollup_cleanup(&pool, &[user]).await;

        let (budget_id, tx_id, status, msg, audit_rows) =
            outcome.expect("the #493 race must leave delete_transaction reporting a failure");

        assert_eq!(status, StatusCode::NOT_FOUND, "a zero-row delete is a 404, not a 204");
        assert_eq!(
            msg, "Transaction not found",
            "the body is deliberately the same as the never-existed 404 — the warn, not the \
             response, is what distinguishes them",
        );
        assert_eq!(
            audit_rows, 0,
            "a delete that removed nothing must not write a DELETE_TRANSACTION audit row claiming it did",
        );

        // The warn is the ONLY observable separating this 404 from the
        // never-existed one, so it is part of the contract. Without this the
        // whole tracing call could be deleted and both tests would still pass.
        let warns = captured.warns_containing("delete_transaction matched zero rows");
        assert_eq!(warns.len(), 1, "exactly one #493 warn, got {warns:?}");
        let warn = &warns[0];
        for (label, id) in [("budget_id", budget_id), ("transaction_id", tx_id), ("user_id", user)] {
            assert!(warn.contains(&id.to_string()), "warn must carry {label}, got: {warn}");
        }
    }

    // Cross-budget / unknown-id guard: deleting budget A's transaction through
    // budget B (owned by the same user) 404s and leaves the row untouched.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_cross_budget_id_is_404() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;
        let tx_a = make_transaction(&state, user, budget_a, Some(cat_a), 10.0, "in A").await;

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_b, tx_a)),
            Extension(user),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::NOT_FOUND, _))),
            "cross-budget id -> 404",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_a)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 404'd delete must not remove the row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A viewer (share with 'view') cannot delete transactions -> 403.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_requires_edit_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, owner, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, owner, budget_id, Some(cat_id), 10.0, "owned").await;

        let viewer = Uuid::new_v4();
        let viewer_email = format!("viewer-{viewer}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(viewer)
            .bind(&viewer_email)
            .execute(&pool)
            .await
            .expect("seed viewer");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed view share");

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(viewer),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::FORBIDDEN, _))),
            "view permission -> 403",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 403'd delete must not remove the row");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // A closed budget is read-only for deletes too (mirrors update_transaction's
    // implicit ensure_not_closed guard).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_rejects_on_closed_budget() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 10.0, "pre-close").await;

        // closed_at is only representable on a project budget (DB CHECK
        // budgets_closed_only_project_check) — flip budget_type too, since
        // ensure_not_closed itself only inspects closed_at.
        sqlx::query("UPDATE budgets SET budget_type = 'project', closed_at = NOW() WHERE id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("close budget");

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::CONFLICT, _))),
            "closed budget -> 409",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 409'd delete must not remove the row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // --- categories_view (#426) DB-backed tests ---

    // The #49 carry that `category_table_rows` does NOT compute must appear on
    // the view rows, and the modal currency must be derived from
    // `transactions.budget_id` directly — an UNCATEGORIZED bank-synced row
    // (category_id NULL) is exactly the population that carries a currency, so
    // joining through `categories` would silently lose it.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn categories_view_returns_carry_and_currency() {
        let (pool, user) = rollup_test_setup().await;

        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, rollover_enabled, currency) \
             VALUES ($1, $2, $3, 'monthly', 'time_based', TRUE, 'EUR')",
        )
        .bind(budget_id)
        .bind(user)
        .bind(format!("cat-view-{budget_id}"))
        .execute(&pool)
        .await
        .expect("seed budget");

        let cat_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Groceries', 'expense', 100.0)",
        )
        .bind(cat_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed category");

        // Previous-period spend of 80 -> carry of 100 - 80 = 20. Dated one day
        // into the previous calendar period so it lands inside the window
        // `previous_period_window` computes, not merely "60 days ago".
        let (prev_start, _prev_end) = previous_period_window("monthly", Utc::now());
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 80.0, $4, 'prev period')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(cat_id)
        .bind(prev_start + chrono::Duration::days(1))
        .execute(&pool)
        .await
        .expect("seed previous-period txn");

        // Current-period, UNCATEGORIZED, EUR — as a bank-synced row arrives.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description, currency) \
             VALUES ($1, $2, NULL, 10.0, now(), 'unreviewed import', 'EUR')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed uncategorized EUR txn");

        let state = test_state(&pool);
        let Json(resp) = categories_view(State(state), Path(budget_id), Extension(user))
            .await
            .expect("categories_view");

        let row = resp
            .categories
            .iter()
            .find(|c| c.id == cat_id)
            .expect("groceries row present");
        assert_eq!(row.carried_amount, 20.0, "limit 100 less previous spend 80 carries 20");
        assert_eq!(row.prev_period_spent, 80.0);
        assert_eq!(row.effective_limit, Some(120.0), "effective_limit is base + carry");
        assert_eq!(row.category_limit, Some(100.0));
        assert!(!row.is_mirror);
        assert_eq!(row.linked_budget_name, None);
        assert_eq!(
            resp.currency,
            "EUR",
            "currency comes from the budget's own column"
        );
        assert!(
            !resp.currency_is_mixed,
            "only EUR transactions exist so the budget is not mixed-currency"
        );
        assert_eq!(resp.time_frame, "monthly");
        assert_eq!(resp.closed_at, None);
        assert_eq!(resp.permission_level, "owner");
        // The client gates its rollover badge/chip on the MASTER switch plus the
        // budget type, so both must ride along on the envelope.
        assert!(
            resp.rollover_enabled,
            "the budget-level #47 master switch is echoed to the client"
        );
        assert_eq!(resp.budget_type, "time_based");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A #52 rollup mirror row must surface the SOURCE budget's own aggregate
    // limit/windowed spend plus its display name, and must never carry (#49) —
    // the mirror reflects the source's live envelope, it does not accumulate.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn categories_view_resolves_rollup_mirror_to_source_totals() {
        let (pool, user) = rollup_test_setup().await;

        let parent = seed_rollup_budget(&pool, user, 100.0, 0.0, false).await;
        let source = seed_rollup_budget(&pool, user, 300.0, 120.0, false).await;
        // Parent must have rollover ON so a zero carry on the mirror proves the
        // mirror rule, not merely a globally disabled rollover.
        sqlx::query("UPDATE budgets SET rollover_enabled = TRUE WHERE id = $1")
            .bind(parent)
            .execute(&pool)
            .await
            .expect("enable parent rollover");
        let source_name: String = sqlx::query_scalar("SELECT name FROM budgets WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .expect("source name");

        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, linked_budget_id) \
             VALUES ($1, $2, 'Child (rolled up)', 'expense', NULL, $3)",
        )
        .bind(mirror_id)
        .bind(parent)
        .bind(source)
        .execute(&pool)
        .await
        .expect("seed mirror category");

        let state = test_state(&pool);
        let Json(resp) = categories_view(State(state), Path(parent), Extension(user))
            .await
            .expect("categories_view");

        let mirror = resp
            .categories
            .iter()
            .find(|c| c.linked_budget_id == Some(source))
            .expect("mirror row present");
        assert!(mirror.is_mirror, "a linked category is flagged as a mirror");
        assert_eq!(
            mirror.category_limit,
            Some(300.0),
            "mirror limit resolves to the source's own aggregate total"
        );
        assert_eq!(mirror.spent, 120.0, "mirror spend resolves to the source's own in-window spend");
        assert_eq!(
            mirror.linked_budget_name,
            Some(source_name),
            "mirror carries the source budget's display name"
        );
        assert_eq!(mirror.carried_amount, 0.0, "a mirror never carries");
        assert_eq!(mirror.prev_period_spent, 0.0);

        sqlx::query("DELETE FROM categories WHERE id = $1")
            .bind(mirror_id)
            .execute(&pool)
            .await
            .ok();
        rollup_cleanup(&pool, &[user]).await;
    }

    // --- revoke_share (#492) ---

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn revoke_share_removes_a_share() {
        let (pool, owner) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        // Create a second user to share with.
        let sharee = Uuid::new_v4();
        let sharee_email = format!("sharee-{sharee}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(sharee)
            .bind(&sharee_email)
            .execute(&pool)
            .await
            .expect("seed sharee");

        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(share_id)
        .bind(budget)
        .bind(&sharee_email)
        .execute(&pool)
        .await
        .expect("seed share");

        let state = test_state(&pool);
        let res = revoke_share(State(state), Path((budget, share_id)), Extension(owner)).await;
        assert!(res.is_ok(), "owner can revoke a share: {res:?}");

        // The share row must be gone.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM budget_shares WHERE id = $1")
            .bind(share_id)
            .fetch_one(&pool)
            .await
            .expect("query share count");
        assert_eq!(count, 0, "share row must be deleted");

        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn revoke_share_rejects_a_share_id_from_another_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let budget_a = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, owner, 200.0, 0.0, false).await;

        let sharee = Uuid::new_v4();
        let sharee_email = format!("sharee-{sharee}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(sharee)
            .bind(&sharee_email)
            .execute(&pool)
            .await
            .expect("seed sharee");

        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(share_id)
        .bind(budget_a)
        .bind(&sharee_email)
        .execute(&pool)
        .await
        .expect("seed share on budget_a");

        let state = test_state(&pool);
        // Attempt to revoke budget_a's share through budget_b's endpoint.
        let res = revoke_share(State(state), Path((budget_b, share_id)), Extension(owner)).await;
        let err = res.err().expect("must reject a share_id from another budget");
        assert_eq!(err.0, StatusCode::NOT_FOUND, "non-belonging share -> 404");
        assert!(err.1.contains("not found"), "body should mention not found");

        // The share must still exist.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM budget_shares WHERE id = $1")
            .bind(share_id)
            .fetch_one(&pool)
            .await
            .expect("query share count");
        assert_eq!(count, 1, "share must not be deleted");

        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn revoke_share_requires_owner_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let sharee = Uuid::new_v4();
        let sharee_email = format!("sharee-{sharee}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(sharee)
            .bind(&sharee_email)
            .execute(&pool)
            .await
            .expect("seed sharee");

        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(share_id)
        .bind(budget)
        .bind(&sharee_email)
        .execute(&pool)
        .await
        .expect("seed share");

        let state = test_state(&pool);
        let res = revoke_share(State(state), Path((budget, share_id)), Extension(stranger)).await;
        let err = res.err().expect("a non-owner must be denied");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "non-owner -> 403");
        assert!(err.1.contains("Only owners"), "body names the permission");

        rollup_cleanup(&pool, &[owner, stranger, sharee]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn revoke_share_rejects_edit_collaborator() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, editor) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        // The editor is shared onto the budget with Edit permission.
        let editor_email = format!("editor-{editor}@example.test");
        sqlx::query("UPDATE users SET email = $1 WHERE id = $2")
            .bind(&editor_email)
            .bind(editor)
            .execute(&pool)
            .await
            .expect("set editor email");

        let editor_share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(editor_share_id)
        .bind(budget)
        .bind(&editor_email)
        .execute(&pool)
        .await
        .expect("seed editor share");

        // A separate share on the same budget for the editor to try revoking.
        let sharee = Uuid::new_v4();
        let sharee_email = format!("sharee-{sharee}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(sharee)
            .bind(&sharee_email)
            .execute(&pool)
            .await
            .expect("seed sharee");

        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(share_id)
        .bind(budget)
        .bind(&sharee_email)
        .execute(&pool)
        .await
        .expect("seed share to revoke");

        let state = test_state(&pool);
        let res = revoke_share(State(state), Path((budget, share_id)), Extension(editor)).await;
        let err = res.err().expect("an edit collaborator must be denied");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "edit collaborator -> 403");
        assert!(err.1.contains("Only owners"), "body names the permission");

        rollup_cleanup(&pool, &[owner, editor, sharee]).await;
    }

    // The Permission::None guard must 403, not hand back an empty 200 that a
    // client would render as "this budget has no categories".
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn categories_view_denies_a_non_member() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let res = categories_view(State(state), Path(budget), Extension(stranger)).await;
        let err = res.err().expect("a non-member must be denied");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "no permission row -> 403, not an empty 200");

        rollup_cleanup(&pool, &[owner, stranger]).await;
    }

    // `permission_level` is the ENTIRE write-affordance model on the client:
    // every control in the categories view is gated on `!== "view"`. The owner
    // arm is covered by `categories_view_returns_carry_and_currency`; this
    // covers the two SHARE arms, which the owner path can never exercise.
    // Swapping them would either strip every control from a legitimate editor
    // or show controls to a viewer whose submits then 403 (#426).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn categories_view_reports_the_sharee_permission_level() {
        let (pool, owner) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let state = test_state(&pool);

        // The sharee is resolved by EMAIL (`share_level`), so the users row and
        // the share row must agree on it.
        let sharee = Uuid::new_v4();
        let sharee_email = format!("sharee-{sharee}@example.test");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(sharee)
            .bind(&sharee_email)
            .execute(&pool)
            .await
            .expect("seed sharee");

        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(share_id)
        .bind(budget)
        .bind(&sharee_email)
        .execute(&pool)
        .await
        .expect("seed edit share");

        let Json(as_editor) = categories_view(State(state.clone()), Path(budget), Extension(sharee))
            .await
            .expect("an edit sharee can read the view");
        assert_eq!(
            as_editor.permission_level, "edit",
            "an edit share must NOT collapse to \"view\" — that strips every \
             control from a legitimate editor"
        );

        // Same user, same budget: only the share level changes.
        sqlx::query("UPDATE budget_shares SET permission_level = 'view' WHERE id = $1")
            .bind(share_id)
            .execute(&pool)
            .await
            .expect("downgrade the share");

        let Json(as_viewer) = categories_view(State(state), Path(budget), Extension(sharee))
            .await
            .expect("a view sharee can read the view");
        assert_eq!(
            as_viewer.permission_level, "view",
            "a view share must NOT report as an editor — the controls it would \
             unlock all 403 on submit"
        );

        rollup_cleanup(&pool, &[owner, sharee]).await;
    }

}
