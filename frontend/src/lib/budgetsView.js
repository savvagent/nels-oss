// Pure helpers for the budgets list/overview page (#241) — the "which budget
// can I switch to", "what does this rollup relationship resolve to", and
// "is this row mine or shared with me" logic factored out of BudgetsView.svelte
// so it's unit-testable under vitest's `environment: "node"` (no jsdom/Svelte
// compiler needed here), mirroring the existing frontend/src/lib/budgetDetails.js
// convention. No Svelte, no I/O.

// A SENTINEL for a rollup relationship pointing at a budget id NOT present in the caller's
// own accessible `budgets` list (e.g. the other side of the relationship was never shared
// with this viewer). Deliberately does not attempt an extra fetch to resolve it — see design
// note in the plan/spec: this list page already holds the full accessible set, so an id
// missing from it means "not visible to you", not "still loading".
//
// This is a pure, no-Svelte module (no access to $_/svelte-i18n), so this value is NOT
// user-facing text — BudgetsView.svelte must translate it (budgetsList.unknownRollupBudget)
// before rendering rather than displaying this string directly, or every non-English locale
// would show a mixed-language sentence. Callers should compare against this export by
// identity, not print it.
export const UNKNOWN_ROLLUP_LABEL = "__unknown_rollup_budget__";

// Whether `budget` may be switched to the caller's active budget FROM this list page.
//
// #391: this is deliberately NOT an ownership gate. Switching writes the caller's own
// per-viewer preference (users.active_budget_id, #255) via POST /budgets/:id/activate,
// which the backend allows for ANY access level and which never touches the owner's
// is_default row — so a budget merely SHARED with the viewer is a valid target. Every
// row in the caller's list is, by construction, one they can access (list_budgets
// returns owned + shared-with-you only); the backend's `check_permission != None` gate
// on /activate is the real authority.
//
// "Active" is determined by matching id against the App-resolved `activeBudget` — NOT
// by the row's own raw `is_default` flag, which can be true on a SHARED row reflecting
// the REMOTE owner's default rather than the viewer's (#266).
//
// Deliberately does NOT special-case a closed or archived budget: set_active_budget has
// no closed/archived guard, and archiving is documented as explicitly NOT read-only
// (AGENTS.md #50), so this predicate matches the backend's actual capability.
export function canSwitchTo(budget, activeBudget) {
  if (!budget) return false;
  return budget.id !== activeBudget?.id;
}

// Resolves a budget's rollup parent/children names by looking them up in the ALREADY-FETCHED
// `allBudgets` list (no extra network call — this list page already has the full accessible
// set in scope, unlike the single-budget details page which must fetch each name
// individually). An id not found in `allBudgets` resolves to UNKNOWN_ROLLUP_LABEL rather than
// being silently dropped, so a child/parent count the user can't fully see still renders as
// one (unresolved) entry, not a missing one.
export function resolveRollupNames(budget, allBudgets) {
  const list = allBudgets || [];
  const findName = (id) => list.find((b) => b.id === id)?.name ?? UNKNOWN_ROLLUP_LABEL;
  const parentId = budget?.rollup_parent_id ?? null;
  const childIds = budget?.rollup_child_ids ?? [];
  return {
    parentName: parentId ? findName(parentId) : null,
    childNames: childIds.map(findName),
  };
}

// Classifies a budget row as owned-by-you or shared-by-someone, surfacing the owner's display
// name (from the new BudgetListItem.owner_name field, #241) when shared. The component
// decides the "shared by X" vs "shared by someone" copy from `ownerName` being non-null.
export function ownershipLabel(budget) {
  if (budget?.is_owner) return { kind: "owned" };
  return { kind: "shared", ownerName: budget?.owner_name ?? null };
}

// Enter or Space activates BudgetsView's role="button" budget cards for keyboard
// accessibility. This is a deliberate SIMPLIFICATION of native <button> semantics, not a
// faithful reproduction of them: a real <button> fires on Enter via keydown but fires on
// Space via keyup (so a user can move focus off the button before release to cancel the
// press), whereas this helper is called from the card's onkeydown for BOTH keys — so, unlike
// a native button, Space here activates on press-down rather than on release.
//
// The `event?.` optional-chaining guard is nullish-safe even though today's only caller
// (BudgetsView.svelte's onkeydown) always passes a live KeyboardEvent — matching the
// defense-in-depth convention elsewhere in this pair of files (see the missing-onSwitch
// handler in BudgetsView.svelte): a future caller passing an undefined/null event can't
// accidentally throw here instead of just evaluating to false.
export function isActivationKey(event) {
  return event?.key === "Enter" || event?.key === " ";
}

// #391 (#255 follow-up): map a failed POST /budgets/:id/activate to a specific i18n key.
// set_active_budget distinguishes exactly two states worth naming — 403 (check_permission
// returned None: the share was revoked between this list load and the click) and 404 (the
// TOCTOU branch: the budget was deleted mid-write). Everything else stays on the caller's
// existing generic copy, so an unmapped status can never render a misleading cause.
export function switchErrorKey(status) {
  if (status === 403) return "budgetsList.switchErrorRevoked";
  if (status === 404) return "budgetsList.switchErrorDeleted";
  return null;
}
