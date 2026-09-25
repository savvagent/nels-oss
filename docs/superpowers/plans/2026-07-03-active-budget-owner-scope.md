# Active-Budget Owner-Scope Fix Implementation Plan (nels#266)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `App.svelte`'s `fetchBudgets()` so it never resolves `activeBudget` to a budget the
viewer does not own, by scoping default-budget selection to owned rows via a new pure,
unit-tested helper.

**Architecture:** Add `resolveActiveBudget(budgets)` to `frontend/src/lib/commands.js` (co-located
with the existing budget-list predicates `canSetAsDefault`/`resolveBudgetByName`), unit-test it in
`frontend/src/lib/commands.test.js`, then swap `App.svelte::fetchBudgets()`'s inline `.find()`
chain for a call to it. Single logical change, one call site.

**Tech Stack:** Svelte 5 (runes), vitest (`environment: "node"` for pure-JS modules), pnpm.

---

### Task 1: Add `resolveActiveBudget` with failing tests first

**Files:**
- Modify: `frontend/src/lib/commands.js`
- Test: `frontend/src/lib/commands.test.js`

- [ ] **Step 1: Read the existing test file structure to match its style**

Run: `cat frontend/src/lib/commands.test.js`

Confirm the existing `describe("canSetAsDefault", ...)` block's shape (plain vitest `describe`/`it`/`expect`, no setup/teardown) so the new block matches it exactly.

- [ ] **Step 2: Write the failing tests**

Append to `frontend/src/lib/commands.test.js` (add `resolveActiveBudget` to the existing `import { ... } from "./commands.js";` line at the top of the file, then add this new `describe` block at the end of the file):

```js
describe("resolveActiveBudget", () => {
  it("returns the viewer's own default budget, ignoring a shared budget's is_default flag", () => {
    // Mirrors the backend's `is_default DESC, name ASC` sort: a shared budget named
    // "Alpha" whose OWNER has it flagged default can sort ahead of the viewer's own
    // default budget "Beta" — this is the exact regression from #266.
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "owned-1", name: "Beta", is_owner: true, is_default: true },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-1");
  });

  it("falls back to any owned budget when no owned row is flagged default", () => {
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "owned-1", name: "Beta", is_owner: true, is_default: false },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-1");
  });

  it("returns null when the viewer owns no budgets", () => {
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "shared-2", name: "Gamma", is_owner: false, is_default: false },
    ];
    expect(resolveActiveBudget(budgets)).toBeNull();
  });

  it("returns null for an empty list", () => {
    expect(resolveActiveBudget([])).toBeNull();
  });

  it("returns null for null/undefined input", () => {
    expect(resolveActiveBudget(null)).toBeNull();
    expect(resolveActiveBudget(undefined)).toBeNull();
  });
});
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd frontend && pnpm run test commands.test.js`
Expected: FAIL — `resolveActiveBudget is not defined` / `resolveActiveBudget is not a function` (it does not exist in `commands.js` yet, and the import at the top of the test file will also fail).
(Note: `pnpm run test -- commands.test.js` does NOT filter — pnpm's injected `--` defeats vitest's filename filter and runs the full suite. Omit it: `pnpm run test commands.test.js`.)

- [ ] **Step 4: Implement `resolveActiveBudget`**

In `frontend/src/lib/commands.js`, add this new export after `canSetAsDefault` (currently the last export in the file, ending at line 104):

```js

// Resolve the viewer's app-wide active budget from the combined owned+shared list returned by
// GET /budgets. `is_default` is a per-row, OWNER-scoped column (unique_default_budget_per_user is
// a partial index on owner_id) — it can be true on a budget merely SHARED with the viewer,
// reflecting the remote owner's own default choice, not the viewer's (#266). Scope selection to
// owned rows: the viewer's own default, falling back to any owned budget (defensive — the backend
// guarantees an owner with >=1 budget always has exactly one owned default row: create_budget
// always promotes the new budget, delete_budget re-promotes on delete of the default), falling
// back to null (no active budget) when the viewer owns none — never silently picks a shared budget.
export function resolveActiveBudget(budgets) {
  const owned = (budgets || []).filter((b) => b?.is_owner);
  return owned.find((b) => b.is_default) ?? owned[0] ?? null;
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd frontend && pnpm run test commands.test.js`
Expected: PASS — all `resolveActiveBudget` tests plus every pre-existing test in the file green.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/commands.js frontend/src/lib/commands.test.js
git commit -m "fix(#266): add owner-scoped resolveActiveBudget helper"
```

---

### Task 2: Wire `App.svelte::fetchBudgets()` to the new helper

**Files:**
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Add the import**

In `frontend/src/App.svelte`, find the existing import block (around line 43):

```js
  import {
    isCommand,
    parseCommand,
    matchCommands,
    knownCommandName,
    parseIssueArgs,
    resolveBudgetByName,
    canSetAsDefault,
  } from "./lib/commands.js";
```

Change it to:

```js
  import {
    isCommand,
    parseCommand,
    matchCommands,
    knownCommandName,
    parseIssueArgs,
    resolveBudgetByName,
    canSetAsDefault,
    resolveActiveBudget,
  } from "./lib/commands.js";
```

- [ ] **Step 2: Replace the inline selection in `fetchBudgets()`**

Find (around line 707-717 — confirm exact line numbers with `grep -n "async function fetchBudgets" frontend/src/App.svelte` since concurrent branches may have shifted them):

```js
  async function fetchBudgets() {
    try {
      budgets = await fetchApi("/budgets");
      if (budgets.length > 0) {
        const defaultB = budgets.find((b) => b.is_default) || budgets[0];
        activeBudget = defaultB;
      } else {
```

Replace the `const defaultB = ...` / `activeBudget = defaultB;` lines with:

```js
  async function fetchBudgets() {
    try {
      budgets = await fetchApi("/budgets");
      if (budgets.length > 0) {
        activeBudget = resolveActiveBudget(budgets);
      } else {
```

(Everything else in the function — the `else` branch clearing `activeBudget`/`insights`, the `loadInsights()` call, the `catch` block — is unchanged.)

- [ ] **Step 3: Manually verify the diff**

Run: `git diff frontend/src/App.svelte`
Expected: only the import line addition and the two-line `fetchBudgets()` body change (net one line removed).

- [ ] **Step 4: Run the full frontend test suite**

Run: `cd frontend && pnpm run test`
Expected: PASS — no test directly exercises `App.svelte::fetchBudgets()` (it has no dedicated test file), so this is a regression check on `commands.test.js`, `budgetsView.test.js`, and every other existing suite, confirming nothing else broke.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "fix(#266): scope fetchBudgets() default-budget selection to owned rows"
```

---

### Task 3: Lint/build verification

**Files:** none (verification only)

- [ ] **Step 1: Run the frontend build**

Run: `cd frontend && pnpm run build`
Expected: builds cleanly with no new warnings/errors attributable to this change.

- [ ] **Step 2: If the repo has a lint script, run it**

Run: `cd frontend && cat package.json | grep -A2 '"lint"'` to check if a lint script exists; if it does, run `pnpm run lint` and confirm it passes (fix only issues in files this plan touched — do not fix unrelated pre-existing lint debt).

- [ ] **Step 3: Commit any lint fixes (only if lint step produced changes)**

```bash
git add -A
git commit -m "fix(#266): lint fixes"
```

(Skip this step entirely if Step 2 made no changes.)
