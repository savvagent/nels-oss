# Simplify Busy Chat Header Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Declutter the chat header (`frontend/src/App.svelte`, "Consolidated Top Bar" ~L1565):
drop the header's clear/new-conversation button and the Insights button, strip a trailing "Budget"
word from the displayed budget name (display-only), and collapse the 8 status badges into a single
info icon/popover — with identical behavior on mobile and desktop.

**Architecture:** Purely a client/display change. Two pure, framework-free helpers
(`displayBudgetName`, `activeBudgetStatuses`) move the name-formatting and status-flag logic out of
the template so they're unit-testable in isolation (mirrors the existing `frontend/src/lib/chatWindow.js`
pattern). A new `BudgetStatusInfo.svelte` component owns the info icon + popover, reusing the
click-toggle / outside-click / Escape-close pattern already proven in `frontend/src/lib/Notifications.svelte`.
`App.svelte` is edited to delete the two buttons and both badge clusters, swap in the display-name
helper, and mount one `BudgetStatusInfo` instance. See design doc:
`docs/superpowers/specs/2026-07-03-simplify-chat-header-design.md`.

**Tech Stack:** Svelte 5 (runes), Vite, Tailwind v4 + DaisyUI, `lucide-svelte`, `svelte-i18n`,
Vitest (`environment: "node"`, existing config at `frontend/vitest.config.js`).

---

## File Structure

- **Create** `frontend/src/lib/budgetDisplay.js` — pure helpers: `displayBudgetName(name)`,
  `activeBudgetStatuses(budget)`. One responsibility: turn a budget record into display data.
- **Create** `frontend/src/lib/budgetDisplay.test.js` — Vitest unit tests for both helpers.
- **Create** `frontend/src/lib/BudgetStatusInfo.svelte` — the info icon + popover component.
- **Modify** `frontend/src/App.svelte` — remove the clear + Insights header buttons and both badge
  clusters, use `displayBudgetName` for the name spans, mount `BudgetStatusInfo`.
- **Modify** locale JSON files under `frontend/src/lib/i18n/locales/` (`en`, `de`, `es`, `fr`, `it`,
  `pt`) — add the `header.statusInfoLabel` key.

---

## Task 1: Pure display-logic module (TDD)

**Files:**
- Create: `frontend/src/lib/budgetDisplay.test.js`
- Create: `frontend/src/lib/budgetDisplay.js`

- [ ] **Step 1: Write the failing tests — `frontend/src/lib/budgetDisplay.test.js`**

```js
import { describe, it, expect } from "vitest";
import { displayBudgetName, activeBudgetStatuses } from "./budgetDisplay.js";

describe("displayBudgetName", () => {
  it("strips a trailing whole-word 'Budget'", () => {
    expect(displayBudgetName("Groceries Budget")).toBe("Groceries");
  });

  it("is case-insensitive", () => {
    expect(displayBudgetName("Groceries budget")).toBe("Groceries");
    expect(displayBudgetName("Groceries BUDGET")).toBe("Groceries");
  });

  it("leaves a mid-string 'Budget' untouched", () => {
    expect(displayBudgetName("Budget for July")).toBe("Budget for July");
  });

  it("leaves a name with no 'Budget' word untouched", () => {
    expect(displayBudgetName("Groceries")).toBe("Groceries");
  });

  it("falls back to the original name when stripping would empty it", () => {
    expect(displayBudgetName("Budget")).toBe("Budget");
  });

  it("does not strip a word that merely contains 'budget' as a substring", () => {
    expect(displayBudgetName("July Budgeting")).toBe("July Budgeting");
  });

  it("handles null/undefined/empty input without throwing", () => {
    expect(displayBudgetName(null)).toBe(null);
    expect(displayBudgetName(undefined)).toBe(undefined);
    expect(displayBudgetName("")).toBe("");
  });
});

describe("activeBudgetStatuses", () => {
  it("returns [] for a null or undefined budget", () => {
    expect(activeBudgetStatuses(null)).toEqual([]);
    expect(activeBudgetStatuses(undefined)).toEqual([]);
  });

  it("returns [] when no status flags are active", () => {
    expect(activeBudgetStatuses({ budget_type: "time_based" })).toEqual([]);
  });

  it("reports a project budget", () => {
    const statuses = activeBudgetStatuses({ budget_type: "project" });
    expect(statuses).toEqual([
      {
        key: "project",
        label: "Project",
        description:
          "A one-off budget for a finite effort — doesn't reset or carry over like a recurring budget.",
      },
    ]);
  });

  it("reports a fixed-amount budget with the amount in the description", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
      base_amount: 250,
    });
    expect(statuses).toEqual([
      {
        key: "fixed",
        label: "Fixed",
        description:
          "Fixed amount — set to $250.00, not derived from category totals",
      },
    ]);
  });

  it("defaults the fixed-amount description to $0.00 when base_amount is missing", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
    });
    expect(statuses[0].description).toBe(
      "Fixed amount — set to $0.00, not derived from category totals",
    );
  });

  it("reports closed and archived independently", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      closed_at: "2026-06-01T00:00:00Z",
      archived_at: "2026-06-02T00:00:00Z",
    });
    expect(statuses.map((s) => s.key)).toEqual(["closed", "archived"]);
  });

  it("reports rollover with the 'some categories' wording when partial", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollover_enabled: true,
      has_partial_category_rollover: true,
    });
    expect(statuses).toEqual([
      {
        key: "rollover",
        label: "Rollover (some)",
        description: "Rollover on — selected categories carry forward",
      },
    ]);
  });

  it("reports rollover with the 'all categories' wording when not partial", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollover_enabled: true,
      has_partial_category_rollover: false,
    });
    expect(statuses).toEqual([
      {
        key: "rollover",
        label: "Rollover",
        description: "Rollover on — all categories carry forward",
      },
    ]);
  });

  it("suppresses rollover and auto-renew for a project budget even if the flags are set", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "project",
      rollover_enabled: true,
      auto_renew: true,
    });
    expect(statuses.map((s) => s.key)).toEqual(["project"]);
  });

  it("reports auto-renew with the next renewal date when present", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      auto_renew: true,
      next_renewal_at: "2026-08-01T00:00:00Z",
    });
    expect(statuses[0].key).toBe("auto-renew");
    expect(statuses[0].description).toContain("Auto-renews on");
  });

  it("reports auto-renew with a generic description when no next date is known", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      auto_renew: true,
    });
    expect(statuses[0].description).toBe("Auto-renews each period");
  });

  it("reports rollup (parent) with child count and aggregated amount", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a", "b"],
      aggregated_base_amount: 500,
    });
    expect(statuses).toEqual([
      {
        key: "rollup",
        label: "Rollup",
        description:
          "Aggregates 2 rolled-up budgets — combined budget $500.00",
      },
    ]);
  });

  it("singularizes 'budget' when there is exactly one rolled-up child", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a"],
      aggregated_base_amount: 100,
    });
    expect(statuses[0].description).toBe(
      "Aggregates 1 rolled-up budget — combined budget $100.00",
    );
  });

  it("reports 'rolled up' (child) only when there are no rollup children", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_parent_id: "parent-1",
    });
    expect(statuses).toEqual([
      {
        key: "rolled-up",
        label: "Rolled up",
        description: "Rolled up into another budget",
      },
    ]);
  });

  it("prefers rollup over rolled-up when both ids are somehow present", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a"],
      aggregated_base_amount: 100,
      rollup_parent_id: "parent-1",
    });
    expect(statuses.map((s) => s.key)).toEqual(["rollup"]);
  });

  it("returns statuses in a stable, badge-matching order", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
      base_amount: 10,
      closed_at: "2026-06-01T00:00:00Z",
      archived_at: "2026-06-02T00:00:00Z",
      rollover_enabled: true,
      auto_renew: true,
      rollup_parent_id: "parent-1",
    });
    expect(statuses.map((s) => s.key)).toEqual([
      "fixed",
      "closed",
      "archived",
      "rollover",
      "auto-renew",
      "rolled-up",
    ]);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && pnpm test -- budgetDisplay`
Expected: FAIL — `Cannot find module './budgetDisplay.js'` (the module doesn't exist yet).

- [ ] **Step 3: Write the implementation — `frontend/src/lib/budgetDisplay.js`**

```js
// Pure display-logic helpers for the chat header's active-budget display (#232).
// No Svelte, no I/O — unit-tested directly. Keeping this out of App.svelte lets
// the header markup stay declarative while the name/status formatting rules are
// exercised in isolation (mirrors frontend/src/lib/chatWindow.js).

// Strips a trailing, whole-word "Budget" from a budget name for display only
// (e.g. "Groceries Budget" -> "Groceries"). Mid-string occurrences ("Budget for
// July") are left alone — the AC only asks for a trailing/standalone word. If
// stripping would leave an empty string (the name IS just "Budget"), the
// original name is returned instead so the header never renders blank.
export function displayBudgetName(name) {
  if (!name) return name;
  const stripped = name.replace(/\s+budget$/i, "").trim();
  return stripped || name;
}

// Returns an ordered list of { key, label, description } for every currently
// active status flag on `budget`, replicating the exact conditions and
// label/title text of the 8 badges this replaces (see App.svelte pre-#232 for
// the original per-badge markup). Order matches the original badge order:
// project, fixed, closed, archived, rollover, auto-renew, rollup/rolled-up.
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- budgetDisplay`
Expected: PASS — all `displayBudgetName` and `activeBudgetStatuses` cases green.

- [ ] **Step 5: Commit**

```bash
cd frontend
git add src/lib/budgetDisplay.js src/lib/budgetDisplay.test.js
git commit -m "feat(#232): extract pure budget display-name/status helpers"
```

---

## Task 2: `BudgetStatusInfo.svelte` (info icon + popover)

**Files:**
- Create: `frontend/src/lib/BudgetStatusInfo.svelte`
- Modify: `frontend/src/lib/i18n/locales/en.json`
- Modify: `frontend/src/lib/i18n/locales/de.json`
- Modify: `frontend/src/lib/i18n/locales/es.json`
- Modify: `frontend/src/lib/i18n/locales/fr.json`
- Modify: `frontend/src/lib/i18n/locales/it.json`
- Modify: `frontend/src/lib/i18n/locales/pt.json`

No component-test harness exists in this repo (vitest runs `environment: "node"`, no
jsdom/testing-library — see `frontend/vitest.config.js` and every existing `*.test.js`, which all
test pure `.js` modules only). This task is not TDD; verification is the manual header check in
Task 3 Step 6 plus `pnpm run build`.

- [ ] **Step 1: Add the `header.statusInfoLabel` key to every locale file**

Each locale file has a top-level `"header"` object (currently just `{ "tagline": "..." }`). Add a
sibling `statusInfoLabel` key to that object in each of the 6 files:

`frontend/src/lib/i18n/locales/en.json` — inside `"header": { ... }`:
```json
"header": {
  "tagline": "Smart Budgeting",
  "statusInfoLabel": "Budget status details"
},
```

`frontend/src/lib/i18n/locales/de.json`:
```json
"statusInfoLabel": "Budgetstatus-Details"
```

`frontend/src/lib/i18n/locales/es.json`:
```json
"statusInfoLabel": "Detalles del estado del presupuesto"
```

`frontend/src/lib/i18n/locales/fr.json`:
```json
"statusInfoLabel": "Détails du statut du budget"
```

`frontend/src/lib/i18n/locales/it.json`:
```json
"statusInfoLabel": "Dettagli sullo stato del budget"
```

`frontend/src/lib/i18n/locales/pt.json`:
```json
"statusInfoLabel": "Detalhes do status do orçamento"
```

In each file, add the key as a new line inside the existing `"header"` object (keep it valid JSON —
add a trailing comma after `"tagline"`'s value, no trailing comma after the last key in the object).

- [ ] **Step 2: Create `frontend/src/lib/BudgetStatusInfo.svelte`**

```svelte
<script>
  // Single info affordance for the chat header's active-budget status flags
  // (#232) — replaces the row of Project/Fixed/Closed/Archived/Rollover/
  // Auto-renew/Rollup/Rolled-up badges with one icon that opens a popover
  // listing whichever of those are currently active. Click-toggle (not
  // hover-only) so it works identically on touch and desktop, following the
  // same open/outside-click/Escape-close pattern as ./Notifications.svelte.
  import { Info } from "lucide-svelte";
  import { _ } from "svelte-i18n";
  import { activeBudgetStatuses } from "./budgetDisplay.js";

  let { budget = null } = $props();

  let open = $state(false);
  let panelEl = $state(null);
  let buttonEl = $state(null);

  let statuses = $derived(activeBudgetStatuses(budget));

  function toggle() {
    open = !open;
  }

  function close() {
    open = false;
  }

  function onWindowClick(e) {
    if (!open) return;
    if (panelEl?.contains(e.target) || buttonEl?.contains(e.target)) return;
    close();
  }

  function onWindowKeydown(e) {
    if (e.key === "Escape" && open) close();
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKeydown} />

{#if statuses.length > 0}
  <div class="relative">
    <button
      bind:this={buttonEl}
      type="button"
      onclick={toggle}
      aria-haspopup="true"
      aria-expanded={open}
      aria-label={$_("header.statusInfoLabel")}
      title={$_("header.statusInfoLabel")}
      class="p-1 rounded-lg text-base-content/60 hover:text-base-content hover:bg-base-100 transition-colors"
    >
      <Info class="w-4 h-4" />
    </button>

    {#if open}
      <div
        bind:this={panelEl}
        role="dialog"
        aria-label={$_("header.statusInfoLabel")}
        class="absolute left-0 top-full mt-2 z-50 w-72 max-w-[calc(100vw-1.5rem)]
               rounded-xl bg-base-200 border border-base-300 shadow-2xl p-3"
      >
        <ul class="flex flex-col gap-2.5">
          {#each statuses as status (status.key)}
            <li class="flex flex-col gap-0.5">
              <span class="text-xs font-semibold text-base-content">{status.label}</span>
              <span class="text-[11px] leading-snug text-base-content/70"
                >{status.description}</span
              >
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </div>
{/if}
```

- [ ] **Step 3: Verify the frontend still builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (the component isn't imported anywhere yet, so this just confirms the
new file and locale JSON are syntactically valid — a JSON syntax error in any locale file fails
the build with a parse error naming the file).

- [ ] **Step 4: Commit**

```bash
cd frontend
git add src/lib/BudgetStatusInfo.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json \
  src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json \
  src/lib/i18n/locales/pt.json
git commit -m "feat(#232): add BudgetStatusInfo popover component"
```

---

## Task 3: Wire the header — remove clear + Insights buttons, badges, and tidy the name

**Files:**
- Modify: `frontend/src/App.svelte`

**Locate every block below by its literal code content (the exact snippets given in each step),
NOT by the stated `~L####` line number.** Those line numbers are illustrative/approximate only —
Steps 2-5 all edit this same file sequentially, so each deletion shifts every later line number.
Use an exact-string find (e.g. an editor's Edit-by-old-string, or `grep -n` on a distinctive
substring like `PenSquare`, `badge-xs badge-accent`, or `BarChart3`) to re-locate each block
immediately before editing it.

- [ ] **Step 1: Add the new imports**

In the top `<script>` block of `frontend/src/App.svelte`, find the existing import block:

```js
  import Sidebar from "./lib/Sidebar.svelte";
  import Settings from "./lib/Settings.svelte";
  import Insights from "./lib/Insights.svelte";
  import Notifications from "./lib/Notifications.svelte";
  import InstallPrompt from "./lib/InstallPrompt.svelte";
  import nelsMark from "./assets/nels-mark.png";
```

Add `BudgetStatusInfo` alongside the other local component imports, and `displayBudgetName` next to
the existing `fmtMoney` import:

```js
  import Sidebar from "./lib/Sidebar.svelte";
  import Settings from "./lib/Settings.svelte";
  import Insights from "./lib/Insights.svelte";
  import Notifications from "./lib/Notifications.svelte";
  import InstallPrompt from "./lib/InstallPrompt.svelte";
  import BudgetStatusInfo from "./lib/BudgetStatusInfo.svelte";
  import nelsMark from "./assets/nels-mark.png";
```

```js
  import { fmtMoney } from "./lib/money.js";
  import { displayBudgetName } from "./lib/budgetDisplay.js";
```

(Insert the `displayBudgetName` import directly below the existing `fmtMoney` import line.)

Also remove `PenSquare` and `BarChart3` from the `lucide-svelte` import list (they become unused
once Steps 2 and 4 below delete their only usages) — change:

```js
  import {
    Info,
    AlertTriangle,
    Send,
    UserCheck,
    CheckCircle2,
    Copy,
    Check,
    Menu,
    X,
    Sparkles,
    Mic,
    BarChart3,
    History,
    PenSquare,
  } from "lucide-svelte";
```

to:

```js
  import {
    Info,
    AlertTriangle,
    Send,
    UserCheck,
    CheckCircle2,
    Copy,
    Check,
    Menu,
    X,
    Sparkles,
    Mic,
    History,
  } from "lucide-svelte";
```

- [ ] **Step 2: Delete the header's "new conversation / clear" button**

Find this block (currently ~L1588-1596, immediately after the hamburger button and before the
"Light chip" logo comment):

```svelte
        <button
          type="button"
          class="p-1.5 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
          onclick={handleNewConversation}
          title={$_("sidebar.newConversation")}
          aria-label={$_("sidebar.newConversation")}
        >
          <PenSquare class="w-5 h-5" />
        </button>
      {/if}
```

Delete the `<button>...</button>` element (keep the `{/if}` that closes the surrounding
`{#if activeScreen === "chat"}` — it still guards the hamburger button above it). The block becomes:

```svelte
        <button
          bind:this={hamburgerEl}
          type="button"
          class="p-1.5 -ml-1 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
          onclick={toggleSidebar}
          aria-label={sidebarOpen ? "Close conversations" : "Open conversations"}
          aria-expanded={sidebarOpen}
          aria-controls="sidebar-nav"
        >
          {#if sidebarOpen}
            <X class="w-5 h-5" />
          {:else}
            <Menu class="w-5 h-5" />
          {/if}
        </button>
      {/if}
```

Do NOT delete the `handleNewConversation` function itself (defined ~L1209) — it's still passed to
`<Sidebar onNewConversation={handleNewConversation} .../>` a few hundred lines below.

- [ ] **Step 3: Replace the mobile name + badge cluster**

Find the mobile "identity line" block:

```svelte
      <div class="flex items-baseline gap-1 md:hidden">
        <span class="text-sm font-bold text-base-content">Nels</span>
        {#if activeBudget}
          <span class="text-[11px] text-base-content/60"
            >&middot; {activeBudget.name}</span
          >
          {#if activeBudget.budget_type === "project"}
            <span class="badge badge-xs badge-accent">Project</span>
          {/if}
          {#if activeBudget.amount_mode === "fixed"}
            <span
              class="badge badge-xs badge-secondary"
              title={`Fixed amount — set to $${(activeBudget.base_amount ?? 0).toFixed(2)}, not derived from category totals`}
              >Fixed</span
            >
          {/if}
          {#if activeBudget.closed_at}
            <span class="badge badge-xs badge-neutral">Closed</span>
          {/if}
          {#if activeBudget.archived_at}
            <span class="badge badge-xs badge-warning">Archived</span>
          {/if}
          {#if activeBudget.rollover_enabled && activeBudget.budget_type !== "project"}
            <span
              class="badge badge-xs badge-success"
              title={activeBudget.has_partial_category_rollover
                ? "Rollover on — selected categories carry forward"
                : "Rollover on — all categories carry forward"}
              >{activeBudget.has_partial_category_rollover
                ? "Rollover (some)"
                : "Rollover"}</span
            >
          {/if}
          {#if activeBudget.auto_renew && activeBudget.budget_type !== "project"}
            <span
              class="badge badge-xs badge-info"
              title={activeBudget.next_renewal_at
                ? `Auto-renews on ${new Date(activeBudget.next_renewal_at).toLocaleDateString(undefined, { timeZone: "UTC" })}`
                : "Auto-renews each period"}>Auto-renew</span
            >
          {/if}
          {#if activeBudget.rollup_child_ids && activeBudget.rollup_child_ids.length > 0}
            <span
              class="badge badge-xs badge-primary"
              title={`Aggregates ${activeBudget.rollup_child_ids.length} rolled-up budget${activeBudget.rollup_child_ids.length === 1 ? "" : "s"} — combined budget $${(activeBudget.aggregated_base_amount ?? 0).toFixed(2)}`}
              >Rollup</span
            >
          {:else if activeBudget.rollup_parent_id}
            <span
              class="badge badge-xs badge-primary badge-outline"
              title="Rolled up into another budget">Rolled up</span
            >
          {/if}
        {/if}
      </div>
```

Replace it with:

```svelte
      <div class="flex items-baseline gap-1 md:hidden">
        <span class="text-sm font-bold text-base-content">Nels</span>
        {#if activeBudget}
          <span class="text-[11px] text-base-content/60"
            >&middot; {displayBudgetName(activeBudget.name)}</span
          >
        {/if}
      </div>
```

- [ ] **Step 4: Replace the desktop name + badge cluster and mount `BudgetStatusInfo`**

Find the desktop block immediately after the wordmark `<div class="hidden md:flex md:flex-col">`:

```svelte
      {#if activeBudget}
        <span class="hidden md:inline text-[11px] text-base-content/60"
          >&middot; {activeBudget.name}</span
        >
        {#if activeBudget.budget_type === "project"}
          <span class="hidden md:inline badge badge-xs badge-accent">Project</span>
        {/if}
        {#if activeBudget.amount_mode === "fixed"}
          <span
            class="hidden md:inline badge badge-xs badge-secondary"
            title={`Fixed amount — set to $${(activeBudget.base_amount ?? 0).toFixed(2)}, not derived from category totals`}
            >Fixed</span
          >
        {/if}
        {#if activeBudget.closed_at}
          <span class="hidden md:inline badge badge-xs badge-neutral">Closed</span>
        {/if}
        {#if activeBudget.archived_at}
          <span class="hidden md:inline badge badge-xs badge-warning">Archived</span>
        {/if}
        {#if activeBudget.rollover_enabled && activeBudget.budget_type !== "project"}
          <span
            class="hidden md:inline badge badge-xs badge-success"
            title={activeBudget.has_partial_category_rollover
              ? "Rollover on — selected categories carry forward"
              : "Rollover on — all categories carry forward"}
            >{activeBudget.has_partial_category_rollover
              ? "Rollover (some)"
              : "Rollover"}</span
          >
        {/if}
        {#if activeBudget.auto_renew && activeBudget.budget_type !== "project"}
          <span
            class="hidden md:inline badge badge-xs badge-info"
            title={activeBudget.next_renewal_at
              ? `Auto-renews on ${new Date(activeBudget.next_renewal_at).toLocaleDateString(undefined, { timeZone: "UTC" })}`
              : "Auto-renews each period"}>Auto-renew</span
          >
        {/if}
        {#if activeBudget.rollup_child_ids && activeBudget.rollup_child_ids.length > 0}
          <span
            class="hidden md:inline badge badge-xs badge-primary"
            title={`Aggregates ${activeBudget.rollup_child_ids.length} rolled-up budget${activeBudget.rollup_child_ids.length === 1 ? "" : "s"} — combined budget $${(activeBudget.aggregated_base_amount ?? 0).toFixed(2)}`}
            >Rollup</span
          >
        {:else if activeBudget.rollup_parent_id}
          <span
            class="hidden md:inline badge badge-xs badge-primary badge-outline"
            title="Rolled up into another budget">Rolled up</span
          >
        {/if}
      {/if}
```

Replace it with:

```svelte
      {#if activeBudget}
        <span class="hidden md:inline text-[11px] text-base-content/60"
          >&middot; {displayBudgetName(activeBudget.name)}</span
        >
      {/if}
      <BudgetStatusInfo budget={activeBudget} />
```

Note `BudgetStatusInfo` is placed OUTSIDE the `{#if activeBudget}` block (it accepts `budget = null`
as its default and renders nothing when there's nothing to show — see Task 2 Step 2 — so it's safe
to mount unconditionally). It carries no `hidden`/`md:inline` responsive classes, so it renders
identically at both breakpoints, sitting next to whichever of the two name spans above is visible.

- [ ] **Step 5: Delete the header's Insights button**

Find this block (currently ~L1734-1748):

```svelte
    <!-- RIGHT: Insights button + notification bell (chat screen only).
         Account actions live in the sidebar footer (see Sidebar.svelte). -->
    <div class="flex items-center gap-1.5">
      {#if activeScreen === "chat"}
        <button
          type="button"
          onclick={() => (showInsights = true)}
          aria-label={$_("insights.openLabel")}
          title={$_("insights.openLabel")}
          class="relative p-1.5 rounded-lg text-base-content/70 hover:text-base-content hover:bg-base-100 transition-colors"
        >
          <BarChart3 class="w-5 h-5" />
        </button>
        <Notifications bind:this={notificationsRef} {fetchApi} {budgets} />
      {/if}
    </div>
```

Replace it with (delete the `<button>` element and update the comment; keep `<Notifications>`):

```svelte
    <!-- RIGHT: notification bell (chat screen only). Insights is reachable from
         the sidebar and the assistant's showInsights flow. Account actions live
         in the sidebar footer (see Sidebar.svelte). -->
    <div class="flex items-center gap-1.5">
      {#if activeScreen === "chat"}
        <Notifications bind:this={notificationsRef} {fetchApi} {budgets} />
      {/if}
    </div>
```

- [ ] **Step 6: Manual verification**

Run: `cd frontend && pnpm run dev`

In the running app (log in / use an existing session with at least one budget that has several
status flags active, e.g. rollover + auto-renew, to exercise the popover with multiple entries):

1. Confirm no clear/new-conversation icon button renders in the header, on both a narrow (mobile,
   e.g. browser devtools responsive mode at ~375px) and wide (desktop) viewport.
2. Confirm "New conversation" is still reachable from the sidebar (hamburger menu -> sidebar ->
   New conversation entry) and works.
3. Confirm no `BarChart3` Insights button renders in the header.
4. Confirm Insights is still reachable: (a) from the sidebar's Insights entry, and (b) via the
   assistant flow (e.g. ask the assistant something that triggers `showInsights`, such as an
   insights/spending-summary request, and confirm the panel opens).
5. Confirm a budget named e.g. "Groceries Budget" displays as "Groceries" in the header (both
   breakpoints); a budget with no "Budget" word in its name is unchanged.
6. Confirm the info icon appears next to the budget name when the active budget has at least one
   status flag set, and does NOT appear when it has none. Click it: a popover opens listing each
   active status with its label + description; click outside or press Escape closes it. Repeat at
   both breakpoints.

- [ ] **Step 7: Run the full test suite and build**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing tests plus the new `budgetDisplay.test.js` suite green.

Run: `cd frontend && pnpm run build`
Expected: build succeeds with no errors. Note: Vite/Rollup tree-shakes unused imports silently
rather than failing the build, so this step does NOT guarantee `PenSquare`/`BarChart3` were fully
removed from the import list — Task 4 Step 2's `grep` is the authoritative check for that.

- [ ] **Step 8: Commit**

```bash
cd frontend
git add src/App.svelte
git commit -m "feat(#232): drop header clear/Insights buttons, tidy budget name, collapse status badges into info icon"
```

---

## Task 4: Final pass

- [ ] **Step 1: Re-read the diff against the AC**

Run: `git diff origin/main...HEAD --stat` and `git diff origin/main...HEAD` from the worktree root.

Confirm against savvagent/nels#232's acceptance criteria:
- [ ] The clear button no longer renders in the header. (Task 3 Step 2)
- [ ] The Insights button no longer renders in the header; Insights is still openable from the
      sidebar and the assistant flow. (Task 3 Step 5; verified Task 3 Step 6.4)
- [ ] The header budget name has a trailing/standalone "Budget" word removed for display; the
      stored name is untouched. (Task 1, Task 3 Steps 3-4; `activeBudget.name` itself is never
      reassigned anywhere in this plan)
- [ ] The status badges are gone from the header, replaced by a single info affordance that still
      communicates each active status. (Task 2, Task 3 Step 4)
- [ ] Behavior is consistent across the mobile and desktop header variants. (Task 3 Steps 3-4 use
      the same `displayBudgetName`/`BudgetStatusInfo` for both; verified Task 3 Step 6)

- [ ] **Step 2: Confirm no orphaned i18n keys or unused imports**

Run: `grep -n "PenSquare\|BarChart3" frontend/src/App.svelte` — expect no output (both fully
removed from the import list and the template).

Run: `grep -rn "sidebar.newConversation" frontend/src/` — expect hits only in `Sidebar.svelte` (the
key is still in active use there, so it must NOT be removed from any locale file).

Run: `grep -rn "insights.openLabel" frontend/src/` — expect hits only in `Sidebar.svelte` (its own
Insights entry there already uses this key at `frontend/src/lib/Sidebar.svelte:339`). The header's
now-deleted button was the only *other* consumer, so this key also survives untouched — do not
remove it from any locale file.

- [ ] **Step 3: Final full check**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: both succeed.
