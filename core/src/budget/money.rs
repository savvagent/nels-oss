//! Carry, rollover, fund and affordability math (#46, #47, #49, #228, #426).

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

/// One completed period's change to a fund's running balance (#228):
/// `category_limit - spent`, with no floor — an overspend subtracts and can
/// drive the balance negative. Shared by the backend's hourly
/// `advance_fund_categories` job and any client that replays fund history.
pub fn fund_period_delta(category_limit: f64, spent: f64) -> f64 {
    category_limit - spent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fund_period_delta_is_limit_minus_spent_and_may_go_negative() {
        assert_eq!(fund_period_delta(100.0, 60.0), 40.0);
        assert_eq!(fund_period_delta(100.0, 150.0), -50.0);
        assert_eq!(fund_period_delta(0.0, 0.0), 0.0);
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
}
