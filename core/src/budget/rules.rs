//! Budget field validators and rollup-link rules (#48, #52, #116, #300).
//! Guards return `RuleError`; the backend maps the kind to 400/409.

use uuid::Uuid;

use crate::error::RuleError;

/// The only permission levels a share may be granted. `check_permission`
/// fail-closes anything else to `View`, so an unknown value is not exploitable
/// today, but accepting it is a foot-gun for any future code path that compares
/// against another literal. Reject anything outside this set up front.
pub const ALLOWED_PERMISSION_LEVELS: [&str; 2] = ["view", "edit"];

/// Validate a share `permission_level` against the allowed set, returning a 400
/// for anything else.
pub fn validate_permission_level(level: &str) -> Result<(), RuleError> {
    if ALLOWED_PERMISSION_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err(RuleError::bad_request(
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
pub fn higher_share_level(a: &str, b: &str) -> String {
    let rank = |s: &str| if s == "edit" { 2 } else { 1 };
    if rank(a).max(rank(b)) == 2 { "edit".to_string() } else { "view".to_string() }
}

/// The only budget types a budget may have. `time_based` is the existing
/// calendar-period behavior (the default); `project` is a one-off project
/// budget that runs from creation until it is closed. Reject anything outside
/// this set up front.
pub const ALLOWED_BUDGET_TYPES: [&str; 2] = ["time_based", "project"];

/// Validate a `budget_type` against the allowed set, returning a 400 for
/// anything else.
pub fn validate_budget_type(budget_type: &str) -> Result<(), RuleError> {
    if ALLOWED_BUDGET_TYPES.contains(&budget_type) {
        Ok(())
    } else {
        Err(RuleError::bad_request(
            "budget_type must be 'time_based' or 'project'".to_string(),
        ))
    }
}

/// The amount modes a budget may use (#116): 'derived' = base is the sum of
/// expense category amounts (default); 'fixed' = base is the stored budget_limit.
pub const ALLOWED_AMOUNT_MODES: [&str; 2] = ["derived", "fixed"];

/// Validate an `amount_mode`, returning a 400 for anything else. Mirrors
/// `validate_budget_type`. Case-sensitive — the DB CHECK is case-sensitive too.
pub fn validate_amount_mode(amount_mode: &str) -> Result<(), RuleError> {
    if ALLOWED_AMOUNT_MODES.contains(&amount_mode) {
        Ok(())
    } else {
        Err(RuleError::bad_request(
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
pub fn validate_budget_strategy(budget_strategy: &str) -> Result<(), RuleError> {
    if ALLOWED_BUDGET_STRATEGIES.contains(&budget_strategy) {
        Ok(())
    } else {
        Err(RuleError::bad_request(
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
pub fn validate_category_type(category_type: &str) -> Result<(), RuleError> {
    if ALLOWED_CATEGORY_TYPES.contains(&category_type) {
        Ok(())
    } else {
        Err(RuleError::bad_request(
            "category_type must be 'income', 'savings', or 'expense'".to_string(),
        ))
    }
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
) -> Result<(), RuleError> {
    if parent_id == child_id {
        return Err(RuleError::bad_request(
            "A budget cannot be rolled up into itself".to_string(),
        ));
    }
    if parent_is_child {
        return Err(RuleError::conflict(
            "The target budget is itself rolled up into another budget; rollup is single-level"
                .to_string(),
        ));
    }
    if child_is_parent {
        return Err(RuleError::conflict(
            "That budget already has budgets rolled up into it; rollup is single-level".to_string(),
        ));
    }
    // #300: budgets can only be rolled up together when they share the same
    // budget_type AND the same budget_strategy. Checked after the self-link/
    // chain-violation guards so those keep priority (same order convention as
    // this function's other checks).
    if parent_type != child_type {
        return Err(RuleError::conflict(
            format!(
                "Budgets can only be rolled up together when they share the same budget type \
                 (the parent is '{parent_type}', the child is '{child_type}'; both must be \
                 'time_based' or both must be 'project')."
            ),
        ));
    }
    if parent_strategy != child_strategy {
        return Err(RuleError::conflict(
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
) -> Result<(), RuleError> {
    if requested == current || (!is_child && !is_parent) {
        return Ok(());
    }
    let role = if is_child { "child" } else { "parent" };
    Err(RuleError::conflict(
        format!(
            "Budgets can only be rolled up together when they share the same {field_label}. \
             This budget is a rollup {role}; changing its {field_label} from '{current}' to \
             '{requested}' would break that. Unlink it first if you need to change this."
        ),
    ))
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
) -> Result<String, RuleError> {
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

    Err(RuleError::conflict(
        format!(
            "The target budget already has categories named \"{}\" and \"{}\"; \
             rename one before rolling up",
            source_name, suffixed
        ),
    ))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RuleErrorKind;

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
    fn higher_share_level_picks_edit_over_view_regardless_of_order() {
        // "edit" > "view" by privilege, NOT lexically (lexically "edit" < "view").
        assert_eq!(higher_share_level("view", "edit"), "edit");
        assert_eq!(higher_share_level("edit", "view"), "edit");
        assert_eq!(higher_share_level("view", "view"), "view");
        assert_eq!(higher_share_level("edit", "edit"), "edit");
    }

    #[test]
    fn validate_permission_level_accepts_view_and_rejects_owner_as_bad_request() {
        assert!(validate_permission_level("view").is_ok());
        let err = validate_permission_level("owner").unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(err.message, "permission_level must be 'view' or 'edit'");
    }

    #[test]
    fn validate_budget_type_accepts_project_and_rejects_unknown_as_bad_request() {
        assert!(validate_budget_type("project").is_ok());
        let err = validate_budget_type("Project").unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(err.message, "budget_type must be 'time_based' or 'project'");
    }

    #[test]
    fn validate_amount_mode_accepts_fixed_and_rejects_unknown_as_bad_request() {
        assert!(validate_amount_mode("fixed").is_ok());
        let err = validate_amount_mode("auto").unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(err.message, "amount_mode must be 'derived' or 'fixed'");
    }

    #[test]
    fn validate_budget_strategy_accepts_zero_based_and_rejects_unknown_as_bad_request() {
        assert!(validate_budget_strategy("zero_based").is_ok());
        let err = validate_budget_strategy("fifty_thirty_twenty").unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(
            err.message,
            "budget_strategy must be 'zero_based' or 'limit_spent_remaining'"
        );
    }

    #[test]
    fn validate_category_type_accepts_savings_and_rejects_unknown_as_bad_request() {
        assert!(validate_category_type("savings").is_ok());
        let err = validate_category_type("groceries").unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(
            err.message,
            "category_type must be 'income', 'savings', or 'expense'"
        );
    }

    #[test]
    fn validate_rollup_link_rejects_a_self_link_as_bad_request() {
        let id = Uuid::nil();
        let err = validate_rollup_link(
            id, id, false, false, "time_based", "time_based",
            "limit_spent_remaining", "limit_spent_remaining",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::BadRequest);
        assert_eq!(err.message, "A budget cannot be rolled up into itself");
    }

    #[test]
    fn validate_rollup_link_rejects_a_parent_that_is_a_child_as_conflict() {
        let err = validate_rollup_link(
            Uuid::from_u128(1), Uuid::from_u128(2), true, false,
            "time_based", "time_based", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "The target budget is itself rolled up into another budget; rollup is single-level"
        );
    }

    #[test]
    fn validate_rollup_link_rejects_a_child_that_is_a_parent_as_conflict() {
        let err = validate_rollup_link(
            Uuid::from_u128(1), Uuid::from_u128(2), false, true,
            "time_based", "time_based", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "That budget already has budgets rolled up into it; rollup is single-level"
        );
    }

    #[test]
    fn validate_rollup_link_rejects_a_type_mismatch_as_conflict() {
        let err = validate_rollup_link(
            Uuid::from_u128(1), Uuid::from_u128(2), false, false,
            "time_based", "project", "zero_based", "zero_based",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "Budgets can only be rolled up together when they share the same budget type \
             (the parent is 'time_based', the child is 'project'; both must be \
             'time_based' or both must be 'project')."
        );
    }

    #[test]
    fn validate_rollup_link_rejects_a_strategy_mismatch_as_conflict() {
        let err = validate_rollup_link(
            Uuid::from_u128(1), Uuid::from_u128(2), false, false,
            "time_based", "time_based", "zero_based", "limit_spent_remaining",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "Budgets can only be rolled up together when they share the same budgeting \
             strategy (the parent is 'zero_based', the child is 'limit_spent_remaining'; \
             both must be 'zero_based' or both must be 'limit_spent_remaining')."
        );
    }

    #[test]
    fn validate_rollup_link_accepts_matching_standalone_budgets() {
        assert!(validate_rollup_link(
            Uuid::from_u128(1), Uuid::from_u128(2), false, false,
            "project", "project", "zero_based", "zero_based",
        )
        .is_ok());
    }

    #[test]
    fn ensure_rollup_type_or_strategy_unchanged_rejects_a_change_on_a_child_as_conflict() {
        assert!(ensure_rollup_type_or_strategy_unchanged(
            true, false, "budget type", "time_based", "time_based"
        )
        .is_ok());
        let err = ensure_rollup_type_or_strategy_unchanged(
            true, false, "budget type", "time_based", "project",
        )
        .unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "Budgets can only be rolled up together when they share the same budget type. \
             This budget is a rollup child; changing its budget type from 'time_based' to \
             'project' would break that. Unlink it first if you need to change this."
        );
    }

    #[test]
    fn rollup_category_name_suffixes_when_only_the_plain_name_is_taken() {
        let existing = vec!["groceries".to_string()];
        assert_eq!(
            rollup_category_name("Groceries", &existing).unwrap(),
            "Groceries (rolled up)"
        );
    }

    #[test]
    fn rollup_category_name_rejects_when_both_names_are_taken_as_conflict() {
        let existing = vec![
            "Groceries".to_string(),
            "groceries (ROLLED UP)".to_string(),
        ];
        let err = rollup_category_name("Groceries", &existing).unwrap_err();
        assert_eq!(err.kind, RuleErrorKind::Conflict);
        assert_eq!(
            err.message,
            "The target budget already has categories named \"Groceries\" and \
             \"Groceries (rolled up)\"; rename one before rolling up"
        );
    }
}
