// Pure display-logic helpers for the chat header's active-budget display (#232).
// No Svelte, no I/O — unit-tested directly. Keeping this out of App.svelte lets
// the header markup stay declarative while the name/status formatting rules are
// exercised in isolation (mirrors frontend/src/lib/chatWindow.js).

// Strips a trailing, whole-word "Budget" from a budget name for display only
// (e.g. "Groceries Budget" -> "Groceries"). Mid-string occurrences ("Budget for
// July") are left alone — the AC only asks for a trailing/standalone word. The
// trailing `\s*` (after "budget", before the `$` anchor) absorbs any trailing
// whitespace the user may have typed after the word (e.g. "Groceries Budget ")
// so it doesn't defeat the match. If stripping would leave an empty string
// (e.g. a whitespace-prefixed name like " Budget" whose entire content is
// stripped), the original name is returned unchanged so the header never
// renders blank. Note the bare word "Budget" alone never reaches this fallback
// branch — the regex requires a preceding whitespace run to match, so it's
// returned as-is via the no-match path instead (same visible result, different
// code path; see budgetDisplay.test.js).
export function displayBudgetName(name) {
  if (!name) return name;
  const stripped = name.replace(/\s+budget\s*$/i, "").trim();
  return stripped || name;
}

// Returns an ordered list of { key, label, description } for every currently
// active status flag on `budget`, replicating the exact conditions and labels
// of the 8 badges this replaces (see App.svelte pre-#232 for the original
// per-badge markup) and reusing their original `title` text where one existed
// (fixed/rollover/auto-renew/rollup/rolled-up). The project/closed/archived
// badges had no `title` before, so their descriptions here are newly composed
// copy, not replicated text (see the design doc's Risks section). Order
// matches the original badge order: project, fixed, closed, archived,
// rollover, auto-renew, rollup/rolled-up.
export function activeBudgetStatuses(budget) {
  if (!budget) return [];
  const statuses = [];

  if (budget.budget_type === "project") {
    statuses.push({
      key: "project",
      label: "Project",
      description:
        "A one-off budget for a finite effort — doesn't reset or carry over like a recurring budget.",
    });
  }

  if (budget.amount_mode === "fixed") {
    statuses.push({
      key: "fixed",
      label: "Fixed",
      description: `Fixed amount — set to $${(budget.base_amount ?? 0).toFixed(2)}, not derived from category totals`,
    });
  }

  if (budget.closed_at) {
    statuses.push({
      key: "closed",
      label: "Closed",
      description: "This budget is closed.",
    });
  }

  if (budget.archived_at) {
    statuses.push({
      key: "archived",
      label: "Archived",
      description: "This budget is archived.",
    });
  }

  if (budget.rollover_enabled && budget.budget_type !== "project") {
    statuses.push({
      key: "rollover",
      label: budget.has_partial_category_rollover ? "Rollover (some)" : "Rollover",
      description: budget.has_partial_category_rollover
        ? "Rollover on — selected categories carry forward"
        : "Rollover on — all categories carry forward",
    });
  }

  if (budget.auto_renew && budget.budget_type !== "project") {
    statuses.push({
      key: "auto-renew",
      label: "Auto-renew",
      description: budget.next_renewal_at
        ? `Auto-renews on ${new Date(budget.next_renewal_at).toLocaleDateString(undefined, { timeZone: "UTC" })}`
        : "Auto-renews each period",
    });
  }

  if (budget.rollup_child_ids && budget.rollup_child_ids.length > 0) {
    statuses.push({
      key: "rollup",
      label: "Rollup",
      description: `Aggregates ${budget.rollup_child_ids.length} rolled-up budget${budget.rollup_child_ids.length === 1 ? "" : "s"} — combined budget $${(budget.aggregated_base_amount ?? 0).toFixed(2)}`,
    });
  } else if (budget.rollup_parent_id) {
    statuses.push({
      key: "rolled-up",
      label: "Rolled up",
      description: "Rolled up into another budget",
    });
  }

  return statuses;
}

// Zero-based strip figures (#358). Available = total income; Allocated = expense
// base + own savings (derived from the backend's own-only `aggregated_base_amount`
// and `aggregated_savings_amount`); Left = Available − Allocated (positive = still
// to allocate, negative = over-allocated, zero = fully budgeted).
export function zeroBasedAvailable(budget) {
  return budget?.aggregated_income_amount ?? 0;
}
export function zeroBasedAllocated(budget) {
  return (budget?.aggregated_base_amount ?? 0) + (budget?.aggregated_savings_amount ?? 0);
}
export function zeroBasedLeft(budget) {
  return zeroBasedAvailable(budget) - zeroBasedAllocated(budget);
}
