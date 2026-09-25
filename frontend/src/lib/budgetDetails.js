// Pure helpers for the budget details outlet page (#240) — payload-building
// and display-formatting logic factored out of BudgetDetails.svelte so it's
// unit-testable under vitest's `environment: "node"` (no jsdom/Svelte compiler
// needed here), mirroring the existing frontend/src/lib/budgetDisplay.js
// convention. No Svelte, no I/O.

// The exact BudgetPayload.time_frame domain accepted by the backend
// (backend/src/budget.rs BudgetPayload doc comment).
export const TIME_FRAMES = ["monthly", "quarterly", "yearly"];

// The exact BudgetPayload.budget_strategy domain accepted by the backend (#300).
export const BUDGET_STRATEGIES = ["zero_based", "limit_spent_remaining"];

// Builds the exact PUT /budgets/:id body for an edit made on this page.
//
// `name`/`time_frame` are bound directly server-side with NO absent-preserving
// COALESCE — always echoed from `existing` (falling back only when `patch`
// supplies a replacement) so an edit to ONE field never blanks another.
// `budget_limit` is likewise bound directly with no COALESCE: for a
// 'fixed'-mode budget the GET response's `budget_limit` IS the raw stored
// value, so echoing it is exact; for 'derived' mode the raw column is inert
// (the server ignores it while amount_mode stays 'derived'), so echoing the
// computed total back is a no-op. `description` is a straight passthrough
// field, echoed as-is.
//
// `budget_type`/`rollover_enabled`/`amount_mode`/`budget_strategy` are
// server-COALESCEd when absent, and `auto_renew` is preserved-on-absent via
// the handler's own unwrap_or (which ALSO clears it when converting to a
// project) — so all five are deliberately OMITTED unless `patch` explicitly
// changes them, letting the server's own preserve/clear rules run untouched.
// `budget_type`/`budget_strategy` are included via `patch.budget_type`/
// `patch.budget_strategy` — `undefined` when not patched, which
// JSON.stringify naturally drops from the request body.
export function buildUpdatePayload(existing, patch = {}) {
  return {
    name: patch.name ?? existing.name,
    description: existing.description ?? null,
    time_frame: patch.time_frame ?? existing.time_frame,
    budget_limit: existing.budget_limit ?? null,
    budget_type: patch.budget_type,
    budget_strategy: patch.budget_strategy,
  };
}

// Determines the single-key patch (if any) that committing `field`'s
// `draftValue` against `budget` should send. Returns `null` when there is
// nothing to save — an empty/whitespace-only name (declined client-side
// rather than submitted, mirroring Sidebar.svelte's conversation-rename
// behavior) or a value unchanged from the field's current value on `budget`
// (nothing to persist, and skipping the PUT avoids a redundant round trip).
// A non-null return is a single-key object ready to pass straight into
// `buildUpdatePayload`'s `patch` argument.
export function buildEditPatch(field, draftValue, budget) {
  if (field === "name") {
    const v = (draftValue ?? "").trim();
    if (!v || v === budget?.name) return null;
    return { name: v };
  }
  if (field === "time_frame") {
    if (draftValue === budget?.time_frame) return null;
    return { time_frame: draftValue };
  }
  if (field === "budget_type") {
    if (draftValue === budget?.budget_type) return null;
    return { budget_type: draftValue };
  }
  if (field === "budget_strategy") {
    if (draftValue === budget?.budget_strategy) return null;
    return { budget_strategy: draftValue };
  }
  return null;
}

// Formats the rollup parent/children display block from a loaded budget plus
// separately-resolved names (BudgetListItem only carries ids, not names — see
// design doc). Pure so the "omit the section / show 'not part of a rollup'"
// branch and the child-name-list logic are testable without mounting the
// component.
export function rollupSummary(budget, parentName, childNames) {
  const childIds = budget?.rollup_child_ids ?? [];
  return {
    hasParent: !!budget?.rollup_parent_id,
    parentName: budget?.rollup_parent_id ? (parentName ?? null) : null,
    hasChildren: childIds.length > 0,
    childNames: childIds.length > 0 ? childNames ?? [] : [],
  };
}

// A closed or archived budget renders read-only on this page (no inline-edit
// affordances) — a deliberate frontend-only narrowing, stricter than the
// backend actually enforces for archiving alone (see design doc Assumptions).
export function isReadOnly(budget) {
  return !!(budget?.closed_at || budget?.archived_at);
}

// Whether `budget` participates in a rollup relationship as either a PARENT (has one or more
// linked children) or a CHILD (`rollup_parent_id` set) (#317). Unlike `isReadOnly`, this does
// NOT make the whole budget read-only — only the Type/Strategy fields are locked while linked
// (see BudgetDetails.svelte) — mirroring the backend's #317 reject-on-edit guard: changing
// either field while rolled up would silently recreate the exact type/strategy mismatch
// `validate_rollup_link` rejects at link time.
export function isRollupLinked(budget) {
  return !!(budget?.rollup_parent_id || (budget?.rollup_child_ids?.length ?? 0) > 0);
}
