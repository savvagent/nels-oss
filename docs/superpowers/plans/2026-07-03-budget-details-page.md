# Budget Details Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a router-outlet page at `#/budgets/<uuid>`, reached by clicking the header's budget
name, showing a budget's name/period/type/rollover/rollup details (sourced from
`GET /budgets/:id`) with inline edit-in-place for name/period/type persisted via
`PUT /budgets/:id`, on a `router.js` extension general enough for #241's future `/budgets/list`
route.

**Architecture:** `frontend/src/lib/router.js` gains additive exports (`parseBudgetsPath`,
`hashForBudgetDetails`, `resolveRoute`) that parse a `budgets/<segment>` hash into either a
dynamic UUID id (this ticket) or a future static token (left for #241), without touching the
existing flat `ROUTES`/`routeFromHash`/`hashForRoute`. A new pure `frontend/src/lib/budgetDetails.js`
holds payload-building and display-formatting helpers (mirrors `budgetDisplay.js`). A new
`frontend/src/lib/BudgetDetails.svelte` — sibling to `CategoriesView.svelte`/`Insights.svelte` —
self-fetches the budget and its rollup-relation names, and always re-fetches via `GET` after a
successful `PUT` (a documented backend response gap means the `PUT` response cannot be trusted for
rollup fields — see design doc). `App.svelte` gains a `budgetDetailsId` state sibling to `route`,
a `navigateToBudgetDetails()` helper, clickable header spans, and one more outlet branch.
See design doc: `docs/superpowers/specs/2026-07-03-budget-details-page-design.md`.

**Tech Stack:** Svelte 5 (runes), Vite, Tailwind v4 + DaisyUI, `lucide-svelte`, `svelte-i18n`,
Vitest (`environment: "node"`, existing config at `frontend/vitest.config.js`).

---

## File Structure

- **Modify** `frontend/src/lib/router.js` — add `parseBudgetsPath`/`hashForBudgetDetails`/
  `resolveRoute`, leave `ROUTES`/`routeFromHash`/`hashForRoute` untouched.
- **Modify** `frontend/src/lib/router.test.js` — add coverage for the new exports.
- **Create** `frontend/src/lib/budgetDetails.js` — pure `TIME_FRAMES`, `buildUpdatePayload`,
  `rollupSummary`, `isReadOnly`.
- **Create** `frontend/src/lib/budgetDetails.test.js` — Vitest unit tests.
- **Create** `frontend/src/lib/BudgetDetails.svelte` — the new details outlet view.
- **Modify** `frontend/src/App.svelte` — imports, `budgetDetailsId` state,
  `navigateToBudgetDetails()`, clickable header spans, hash-resolution call sites, outlet branch.
- **Modify** locale JSON files under `frontend/src/lib/i18n/locales/` (`en`, `de`, `es`, `fr`, `it`,
  `pt`) — add a `budgetDetails.*` namespace + one `header.budgetNameAria` key.

---

## Task 1: Router extension for `budgets/<segment>` (TDD)

**Files:**
- Modify: `frontend/src/lib/router.test.js`
- Modify: `frontend/src/lib/router.js`

- [ ] **Step 1: Write the failing tests — append to `frontend/src/lib/router.test.js`**

Add this import change at the top of the file (replace the existing import line):

```js
import { describe, it, expect } from "vitest";
import {
  ROUTES,
  routeFromHash,
  hashForRoute,
  parseBudgetsPath,
  hashForBudgetDetails,
  resolveRoute,
} from "./router.js";
```

Append these new `describe` blocks at the end of the file (after the existing `hashForRoute`
block's closing `});`):

```js
describe("parseBudgetsPath", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("parses a lowercase uuid segment as a dynamic id", () => {
    expect(parseBudgetsPath(`#/budgets/${UUID}`)).toEqual({ kind: "id", id: UUID });
  });

  it("parses an uppercase uuid segment as a dynamic id (case-insensitive)", () => {
    const upper = UUID.toUpperCase();
    expect(parseBudgetsPath(`#/budgets/${upper}`)).toEqual({ kind: "id", id: upper });
  });

  it("returns null for a non-uuid, non-registered-token segment", () => {
    expect(parseBudgetsPath("#/budgets/nope")).toBe(null);
  });

  it("returns null for the 'list' token (not yet registered — #241 adds it later)", () => {
    expect(parseBudgetsPath("#/budgets/list")).toBe(null);
  });

  it("returns null for a missing segment", () => {
    expect(parseBudgetsPath("#/budgets/")).toBe(null);
    expect(parseBudgetsPath("#/budgets")).toBe(null);
  });

  it("returns null for a segment with a nested extra path part", () => {
    expect(parseBudgetsPath(`#/budgets/${UUID}/extra`)).toBe(null);
  });

  it("returns null for a hash with no budgets/ prefix", () => {
    expect(parseBudgetsPath("#/categories")).toBe(null);
    expect(parseBudgetsPath("#/insights")).toBe(null);
    expect(parseBudgetsPath("")).toBe(null);
  });

  it("returns null for non-string/null/undefined input", () => {
    expect(parseBudgetsPath(null)).toBe(null);
    expect(parseBudgetsPath(undefined)).toBe(null);
    expect(parseBudgetsPath(42)).toBe(null);
  });

  it("returns null for a hash lacking the # prefix", () => {
    expect(parseBudgetsPath(`budgets/${UUID}`)).toBe(null);
  });
});

describe("hashForBudgetDetails", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("returns #/budgets/<id> for a given id", () => {
    expect(hashForBudgetDetails(UUID)).toBe(`#/budgets/${UUID}`);
  });

  it("returns an empty string for a falsy id", () => {
    expect(hashForBudgetDetails(null)).toBe("");
    expect(hashForBudgetDetails(undefined)).toBe("");
    expect(hashForBudgetDetails("")).toBe("");
  });

  it("round-trips with parseBudgetsPath", () => {
    expect(parseBudgetsPath(hashForBudgetDetails(UUID))).toEqual({ kind: "id", id: UUID });
  });
});

describe("resolveRoute", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("resolves a budgets/<uuid> hash to budgetDetails + the id", () => {
    expect(resolveRoute(`#/budgets/${UUID}`)).toEqual({
      route: "budgetDetails",
      budgetId: UUID,
    });
  });

  it("resolves every existing flat route with a null budgetId", () => {
    for (const route of ROUTES) {
      expect(resolveRoute(hashForRoute(route))).toEqual({ route, budgetId: null });
    }
  });

  it("falls back to chat with a null budgetId for garbage input", () => {
    expect(resolveRoute("#/nope")).toEqual({ route: "chat", budgetId: null });
    expect(resolveRoute(null)).toEqual({ route: "chat", budgetId: null });
  });

  it("falls back to chat for budgets/list today (#241 hasn't registered it yet)", () => {
    expect(resolveRoute("#/budgets/list")).toEqual({ route: "chat", budgetId: null });
  });
});
```

- [ ] **Step 2: Run the tests to verify the new ones fail**

Run: `cd frontend && pnpm test -- router`
Expected: FAIL — `parseBudgetsPath`/`hashForBudgetDetails`/`resolveRoute` are not exported yet.

- [ ] **Step 3: Write the implementation — append to `frontend/src/lib/router.js`**

Append this to the end of the file (after the existing `hashForRoute` function):

```js
// Hierarchical "budgets/<segment>" path-segment routing (#240) — deliberately
// separate from the flat ROUTES/routeFromHash/hashForRoute above, not folded
// into them. A single "budgets/" prefix fans out into multiple segment KINDS:
// a dynamic budget-id segment (#240, wired below) and, in the future, static
// tokens like "list" (#241, a deferred sibling ticket that will add its token
// to BUDGET_STATIC_SEGMENTS without touching the id-parsing branch here).

const BUDGET_ID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// Static tokens recognized as the segment under "budgets/". Empty for now —
// #240 only wires the dynamic-id branch; #241 adds "list" here later.
const BUDGET_STATIC_SEGMENTS = [];

// "#/budgets/<uuid>" -> { kind: "id", id: "<uuid>" }
// "#/budgets/<token>" (once a future ticket registers <token> in
//   BUDGET_STATIC_SEGMENTS) -> { kind: "<token>" }
// anything else under "budgets/" (missing segment, unknown token, malformed
// uuid, a segment with a nested "/", or no "budgets/" prefix at all) -> null
export function parseBudgetsPath(hash) {
  if (typeof hash !== "string" || !hash.startsWith("#")) return null;
  const path = hash.replace(/^#\/?/, "");
  if (!path.startsWith("budgets/")) return null;
  const segment = path.slice("budgets/".length);
  if (!segment || segment.includes("/")) return null;
  if (BUDGET_ID_RE.test(segment)) return { kind: "id", id: segment };
  if (BUDGET_STATIC_SEGMENTS.includes(segment)) return { kind: segment };
  return null;
}

// Inverse of the dynamic-id case: "<uuid>" -> "#/budgets/<uuid>". A falsy id
// returns "" (mirrors hashForRoute's "no hash for the default" convention).
export function hashForBudgetDetails(budgetId) {
  return budgetId ? `#/budgets/${budgetId}` : "";
}

// Composes parseBudgetsPath with the existing flat routeFromHash into the one
// shape App.svelte needs at each of its 3 hash-resolution call sites: which
// outlet to show, plus a budget id when that outlet is the details page (null
// otherwise). A budgets/<unrecognized-token> hash (e.g. "budgets/list" before
// #241 registers it) falls through to routeFromHash's own "chat" fallback,
// exactly like any other unrecognized hash today.
export function resolveRoute(hash) {
  const budgetsPath = parseBudgetsPath(hash);
  if (budgetsPath?.kind === "id") {
    return { route: "budgetDetails", budgetId: budgetsPath.id };
  }
  return { route: routeFromHash(hash), budgetId: null };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- router`
Expected: PASS — all existing plus new `parseBudgetsPath`/`hashForBudgetDetails`/`resolveRoute`
cases green.

- [ ] **Step 5: Commit**

```bash
cd frontend
git add src/lib/router.js src/lib/router.test.js
git commit -m "feat(#240): extend router.js with budgets/<segment> path parsing"
```

---

## Task 2: Pure `budgetDetails.js` helpers (TDD)

**Files:**
- Create: `frontend/src/lib/budgetDetails.test.js`
- Create: `frontend/src/lib/budgetDetails.js`

- [ ] **Step 1: Write the failing tests — `frontend/src/lib/budgetDetails.test.js`**

```js
import { describe, it, expect } from "vitest";
import {
  TIME_FRAMES,
  buildUpdatePayload,
  rollupSummary,
  isReadOnly,
} from "./budgetDetails.js";

describe("TIME_FRAMES", () => {
  it("lists the three backend-recognized time frames", () => {
    expect(TIME_FRAMES).toEqual(["monthly", "quarterly", "yearly"]);
  });
});

describe("buildUpdatePayload", () => {
  const existing = {
    name: "Groceries",
    description: "Monthly food budget",
    time_frame: "monthly",
    budget_limit: 500,
    budget_type: "time_based",
    rollover_enabled: true,
    auto_renew: true,
    amount_mode: "derived",
  };

  it("echoes name/time_frame/description/budget_limit unchanged with no patch", () => {
    const payload = buildUpdatePayload(existing, {});
    expect(payload.name).toBe("Groceries");
    expect(payload.time_frame).toBe("monthly");
    expect(payload.description).toBe("Monthly food budget");
    expect(payload.budget_limit).toBe(500);
  });

  it("omits budget_type/rollover_enabled/auto_renew/amount_mode with no patch (verified via JSON, not just undefined)", () => {
    const payload = buildUpdatePayload(existing, {});
    const json = JSON.stringify(payload);
    expect(json).not.toContain("budget_type");
    expect(json).not.toContain("rollover_enabled");
    expect(json).not.toContain("auto_renew");
    expect(json).not.toContain("amount_mode");
  });

  it("overrides only name when patching name", () => {
    const payload = buildUpdatePayload(existing, { name: "Food" });
    expect(payload.name).toBe("Food");
    expect(payload.time_frame).toBe("monthly");
    expect(JSON.stringify(payload)).not.toContain("budget_type");
  });

  it("overrides only time_frame when patching time_frame", () => {
    const payload = buildUpdatePayload(existing, { time_frame: "yearly" });
    expect(payload.time_frame).toBe("yearly");
    expect(payload.name).toBe("Groceries");
  });

  it("includes budget_type only when patching budget_type", () => {
    const payload = buildUpdatePayload(existing, { budget_type: "project" });
    expect(payload.budget_type).toBe("project");
    expect(payload.name).toBe("Groceries");
    expect(payload.time_frame).toBe("monthly");
  });

  it("defaults description to null when the existing budget has none", () => {
    const noDescription = { ...existing, description: null };
    const payload = buildUpdatePayload(noDescription, {});
    expect(payload.description).toBe(null);
  });

  it("defaults budget_limit to null when the existing budget has none", () => {
    const noLimit = { ...existing, budget_limit: null };
    const payload = buildUpdatePayload(noLimit, {});
    expect(payload.budget_limit).toBe(null);
  });
});

describe("rollupSummary", () => {
  const base = { rollup_parent_id: null, rollup_child_ids: [] };

  it("reports no parent and no children for a standalone budget", () => {
    const summary = rollupSummary(base, null, []);
    expect(summary).toEqual({
      hasParent: false,
      parentName: null,
      hasChildren: false,
      childNames: [],
    });
  });

  it("reports a parent when rollup_parent_id is set", () => {
    const budget = { ...base, rollup_parent_id: "parent-id" };
    const summary = rollupSummary(budget, "Household", []);
    expect(summary.hasParent).toBe(true);
    expect(summary.parentName).toBe("Household");
  });

  it("reports children when rollup_child_ids is non-empty", () => {
    const budget = { ...base, rollup_child_ids: ["a", "b"] };
    const summary = rollupSummary(budget, null, ["Vacation", "Car"]);
    expect(summary.hasChildren).toBe(true);
    expect(summary.childNames).toEqual(["Vacation", "Car"]);
  });
});

describe("isReadOnly", () => {
  it("is false when neither closed_at nor archived_at is set", () => {
    expect(isReadOnly({ closed_at: null, archived_at: null })).toBe(false);
  });

  it("is true when closed_at is set", () => {
    expect(isReadOnly({ closed_at: "2026-01-01T00:00:00Z", archived_at: null })).toBe(true);
  });

  it("is true when archived_at is set", () => {
    expect(isReadOnly({ closed_at: null, archived_at: "2026-01-01T00:00:00Z" })).toBe(true);
  });

  it("is true when both are set", () => {
    expect(
      isReadOnly({ closed_at: "2026-01-01T00:00:00Z", archived_at: "2026-01-02T00:00:00Z" }),
    ).toBe(true);
  });

  it("is false for a null/undefined budget", () => {
    expect(isReadOnly(null)).toBe(false);
    expect(isReadOnly(undefined)).toBe(false);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && pnpm test -- budgetDetails`
Expected: FAIL — `Cannot find module './budgetDetails.js'`.

- [ ] **Step 3: Write the implementation — `frontend/src/lib/budgetDetails.js`**

```js
// Pure helpers for the budget details outlet page (#240) — payload-building
// and display-formatting logic factored out of BudgetDetails.svelte so it's
// unit-testable under vitest's `environment: "node"` (no jsdom/Svelte compiler
// needed here), mirroring the existing frontend/src/lib/budgetDisplay.js
// convention. No Svelte, no I/O.

// The exact BudgetPayload.time_frame domain accepted by the backend
// (backend/src/budget.rs BudgetPayload doc comment).
export const TIME_FRAMES = ["monthly", "quarterly", "yearly"];

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
// `budget_type`/`rollover_enabled`/`amount_mode` are server-COALESCEd when
// absent, and `auto_renew` is preserved-on-absent via the handler's own
// unwrap_or (which ALSO clears it when converting to a project) — so all four
// are deliberately OMITTED unless `patch` explicitly changes `budget_type`,
// letting the server's own preserve/clear rules run untouched. `budget_type`
// is included via `patch.budget_type` — `undefined` when not patched, which
// JSON.stringify naturally drops from the request body.
export function buildUpdatePayload(existing, patch = {}) {
  return {
    name: patch.name ?? existing.name,
    description: existing.description ?? null,
    time_frame: patch.time_frame ?? existing.time_frame,
    budget_limit: existing.budget_limit ?? null,
    budget_type: patch.budget_type,
  };
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- budgetDetails`
Expected: PASS — all `buildUpdatePayload`/`rollupSummary`/`isReadOnly`/`TIME_FRAMES` cases green.

- [ ] **Step 5: Commit**

```bash
cd frontend
git add src/lib/budgetDetails.js src/lib/budgetDetails.test.js
git commit -m "feat(#240): add pure payload/display helpers for the budget details page"
```

---

## Task 3: i18n keys for the details page

**Files:**
- Modify: `frontend/src/lib/i18n/locales/en.json`
- Modify: `frontend/src/lib/i18n/locales/de.json`
- Modify: `frontend/src/lib/i18n/locales/es.json`
- Modify: `frontend/src/lib/i18n/locales/fr.json`
- Modify: `frontend/src/lib/i18n/locales/it.json`
- Modify: `frontend/src/lib/i18n/locales/pt.json`

No component-test harness exists in this repo. This task is not TDD; verification is
`pnpm run build` (valid JSON) here, plus manual verification once wired into `App.svelte` in
Task 5.

- [ ] **Step 1: Add the `budgetDetails.*` namespace to every locale file**

In `frontend/src/lib/i18n/locales/en.json`, add a new top-level `"budgetDetails"` object (place it
as a sibling right after the existing `"categories"` object, before `"notifications"`):

```json
"budgetDetails": {
  "loading": "Loading budget…",
  "back": "Back to chat",
  "notFound": "Budget not found",
  "nameLabel": "Name",
  "typeLabel": "Type",
  "periodLabel": "Period",
  "typePeriodic": "Periodic",
  "typeProject": "Project",
  "periodMonthly": "Monthly",
  "periodQuarterly": "Quarterly",
  "periodYearly": "Yearly",
  "projectNote": "Project budgets track total spend from creation until closed, rather than resetting each period.",
  "rolloverLabel": "Rollover",
  "rolloverOnAll": "On — all categories carry forward",
  "rolloverOnSome": "On — some categories carry forward",
  "rolloverOff": "Off",
  "autoRenewLabel": "Auto-renew",
  "autoRenewOnNext": "On — renews {date}",
  "autoRenewOn": "On",
  "autoRenewOff": "Off",
  "rollupLabel": "Rollup",
  "rollupParent": "Part of {name}",
  "rollupChildren": "Includes: {names}",
  "rollupNone": "Not part of a rollup",
  "unknownBudget": "Unknown budget",
  "closedNotice": "This budget is closed and read-only.",
  "archivedNotice": "This budget is archived and read-only.",
  "saveError": "Couldn't save: {error}",
  "editHint": "Click to edit"
},
```

And add one key to the existing `"header"` object (next to any existing key in that object —
order doesn't matter):

```json
"budgetNameAria": "View budget details",
```

Repeat for the other 5 locales with these values (same key set, same placement — right after
`"categories"`, plus the one `header.budgetNameAria` addition):

`frontend/src/lib/i18n/locales/de.json`:
```json
"budgetDetails": {
  "loading": "Budget wird geladen…",
  "back": "Zurück zum Chat",
  "notFound": "Budget nicht gefunden",
  "nameLabel": "Name",
  "typeLabel": "Typ",
  "periodLabel": "Zeitraum",
  "typePeriodic": "Periodisch",
  "typeProject": "Projekt",
  "periodMonthly": "Monatlich",
  "periodQuarterly": "Vierteljährlich",
  "periodYearly": "Jährlich",
  "projectNote": "Projektbudgets erfassen die Gesamtausgaben von der Erstellung bis zum Abschluss, statt sich periodisch zurückzusetzen.",
  "rolloverLabel": "Übertrag",
  "rolloverOnAll": "Aktiv — alle Kategorien werden übertragen",
  "rolloverOnSome": "Aktiv — einige Kategorien werden übertragen",
  "rolloverOff": "Aus",
  "autoRenewLabel": "Automatische Verlängerung",
  "autoRenewOnNext": "Aktiv — verlängert sich am {date}",
  "autoRenewOn": "Aktiv",
  "autoRenewOff": "Aus",
  "rollupLabel": "Zusammenfassung",
  "rollupParent": "Teil von {name}",
  "rollupChildren": "Enthält: {names}",
  "rollupNone": "Nicht Teil einer Zusammenfassung",
  "unknownBudget": "Unbekanntes Budget",
  "closedNotice": "Dieses Budget ist abgeschlossen und schreibgeschützt.",
  "archivedNotice": "Dieses Budget ist archiviert und schreibgeschützt.",
  "saveError": "Speichern fehlgeschlagen: {error}",
  "editHint": "Zum Bearbeiten klicken"
},
```
```json
"budgetNameAria": "Budgetdetails anzeigen",
```

`frontend/src/lib/i18n/locales/es.json`:
```json
"budgetDetails": {
  "loading": "Cargando presupuesto…",
  "back": "Volver al chat",
  "notFound": "Presupuesto no encontrado",
  "nameLabel": "Nombre",
  "typeLabel": "Tipo",
  "periodLabel": "Periodo",
  "typePeriodic": "Periódico",
  "typeProject": "Proyecto",
  "periodMonthly": "Mensual",
  "periodQuarterly": "Trimestral",
  "periodYearly": "Anual",
  "projectNote": "Los presupuestos de proyecto registran el gasto total desde su creación hasta su cierre, en lugar de reiniciarse cada periodo.",
  "rolloverLabel": "Arrastre",
  "rolloverOnAll": "Activado — todas las categorías se arrastran",
  "rolloverOnSome": "Activado — algunas categorías se arrastran",
  "rolloverOff": "Desactivado",
  "autoRenewLabel": "Renovación automática",
  "autoRenewOnNext": "Activada — se renueva el {date}",
  "autoRenewOn": "Activada",
  "autoRenewOff": "Desactivada",
  "rollupLabel": "Agrupación",
  "rollupParent": "Parte de {name}",
  "rollupChildren": "Incluye: {names}",
  "rollupNone": "No forma parte de una agrupación",
  "unknownBudget": "Presupuesto desconocido",
  "closedNotice": "Este presupuesto está cerrado y es de solo lectura.",
  "archivedNotice": "Este presupuesto está archivado y es de solo lectura.",
  "saveError": "No se pudo guardar: {error}",
  "editHint": "Haz clic para editar"
},
```
```json
"budgetNameAria": "Ver detalles del presupuesto",
```

`frontend/src/lib/i18n/locales/fr.json`:
```json
"budgetDetails": {
  "loading": "Chargement du budget…",
  "back": "Retour au chat",
  "notFound": "Budget introuvable",
  "nameLabel": "Nom",
  "typeLabel": "Type",
  "periodLabel": "Période",
  "typePeriodic": "Périodique",
  "typeProject": "Projet",
  "periodMonthly": "Mensuel",
  "periodQuarterly": "Trimestriel",
  "periodYearly": "Annuel",
  "projectNote": "Les budgets de projet suivent les dépenses totales depuis leur création jusqu'à leur clôture, au lieu de se réinitialiser à chaque période.",
  "rolloverLabel": "Report",
  "rolloverOnAll": "Activé — toutes les catégories sont reportées",
  "rolloverOnSome": "Activé — certaines catégories sont reportées",
  "rolloverOff": "Désactivé",
  "autoRenewLabel": "Renouvellement automatique",
  "autoRenewOnNext": "Activé — se renouvelle le {date}",
  "autoRenewOn": "Activé",
  "autoRenewOff": "Désactivé",
  "rollupLabel": "Regroupement",
  "rollupParent": "Fait partie de {name}",
  "rollupChildren": "Comprend : {names}",
  "rollupNone": "Ne fait pas partie d'un regroupement",
  "unknownBudget": "Budget inconnu",
  "closedNotice": "Ce budget est clôturé et en lecture seule.",
  "archivedNotice": "Ce budget est archivé et en lecture seule.",
  "saveError": "Échec de l'enregistrement : {error}",
  "editHint": "Cliquer pour modifier"
},
```
```json
"budgetNameAria": "Voir les détails du budget",
```

`frontend/src/lib/i18n/locales/it.json`:
```json
"budgetDetails": {
  "loading": "Caricamento del budget…",
  "back": "Torna alla chat",
  "notFound": "Budget non trovato",
  "nameLabel": "Nome",
  "typeLabel": "Tipo",
  "periodLabel": "Periodo",
  "typePeriodic": "Periodico",
  "typeProject": "Progetto",
  "periodMonthly": "Mensile",
  "periodQuarterly": "Trimestrale",
  "periodYearly": "Annuale",
  "projectNote": "I budget di progetto tracciano la spesa totale dalla creazione alla chiusura, invece di azzerarsi a ogni periodo.",
  "rolloverLabel": "Riporto",
  "rolloverOnAll": "Attivo — tutte le categorie vengono riportate",
  "rolloverOnSome": "Attivo — alcune categorie vengono riportate",
  "rolloverOff": "Disattivato",
  "autoRenewLabel": "Rinnovo automatico",
  "autoRenewOnNext": "Attivo — si rinnova il {date}",
  "autoRenewOn": "Attivo",
  "autoRenewOff": "Disattivato",
  "rollupLabel": "Raggruppamento",
  "rollupParent": "Fa parte di {name}",
  "rollupChildren": "Include: {names}",
  "rollupNone": "Non fa parte di un raggruppamento",
  "unknownBudget": "Budget sconosciuto",
  "closedNotice": "Questo budget è chiuso ed è di sola lettura.",
  "archivedNotice": "Questo budget è archiviato ed è di sola lettura.",
  "saveError": "Salvataggio non riuscito: {error}",
  "editHint": "Clicca per modificare"
},
```
```json
"budgetNameAria": "Visualizza i dettagli del budget",
```

`frontend/src/lib/i18n/locales/pt.json`:
```json
"budgetDetails": {
  "loading": "Carregando orçamento…",
  "back": "Voltar ao chat",
  "notFound": "Orçamento não encontrado",
  "nameLabel": "Nome",
  "typeLabel": "Tipo",
  "periodLabel": "Período",
  "typePeriodic": "Periódico",
  "typeProject": "Projeto",
  "periodMonthly": "Mensal",
  "periodQuarterly": "Trimestral",
  "periodYearly": "Anual",
  "projectNote": "Orçamentos de projeto acompanham o gasto total desde a criação até o encerramento, em vez de reiniciar a cada período.",
  "rolloverLabel": "Transferência",
  "rolloverOnAll": "Ativada — todas as categorias são transferidas",
  "rolloverOnSome": "Ativada — algumas categorias são transferidas",
  "rolloverOff": "Desativada",
  "autoRenewLabel": "Renovação automática",
  "autoRenewOnNext": "Ativada — renova em {date}",
  "autoRenewOn": "Ativada",
  "autoRenewOff": "Desativada",
  "rollupLabel": "Agrupamento",
  "rollupParent": "Parte de {name}",
  "rollupChildren": "Inclui: {names}",
  "rollupNone": "Não faz parte de um agrupamento",
  "unknownBudget": "Orçamento desconhecido",
  "closedNotice": "Este orçamento está encerrado e é somente leitura.",
  "archivedNotice": "Este orçamento está arquivado e é somente leitura.",
  "saveError": "Falha ao salvar: {error}",
  "editHint": "Clique para editar"
},
```
```json
"budgetNameAria": "Ver detalhes do orçamento",
```

- [ ] **Step 2: Verify every locale file is still valid JSON**

Run: `cd frontend && for f in src/lib/i18n/locales/*.json; do node -e "JSON.parse(require('fs').readFileSync('$f'))" || echo "INVALID: $f"; done`
Expected: no `INVALID` lines printed.

- [ ] **Step 3: Commit**

```bash
cd frontend
git add src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json src/lib/i18n/locales/es.json \
  src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json src/lib/i18n/locales/pt.json
git commit -m "feat(#240): add budgetDetails.* i18n keys for all 6 locales"
```

---

## Task 4: `BudgetDetails.svelte` component

**Files:**
- Create: `frontend/src/lib/BudgetDetails.svelte`

Not TDD (no component-test harness in this repo — see Task 3 note). Verification is
`pnpm run build` here, plus manual verification once wired into `App.svelte` in Task 5.

- [ ] **Step 1: Create `frontend/src/lib/BudgetDetails.svelte`**

```svelte
<script>
  // Router-outlet view for a single budget's details (#240) — reached by
  // clicking the header's budget name. Sibling to ./CategoriesView.svelte and
  // ./Insights.svelte: self-fetches via the shared `fetchApi` helper rather
  // than receiving budget data as a prop, so it works for ANY budget id in
  // the hash, not just the currently active budget.
  import { _ } from "svelte-i18n";
  import { ArrowLeft, AlertTriangle, Pencil } from "lucide-svelte";
  import {
    TIME_FRAMES,
    buildUpdatePayload,
    rollupSummary,
    isReadOnly,
  } from "./budgetDetails.js";

  let { fetchApi, budgetId, onBack, onBudgetUpdated } = $props();

  let budget = $state(null);
  let loading = $state(false);
  let error = $state("");

  let parentName = $state(null);
  let childNames = $state([]);

  let editingField = $state(null); // null | "name" | "time_frame" | "budget_type"
  let draftValue = $state("");
  let saving = $state(false);
  let saveError = $state("");

  // Best-effort name lookup for one related budget id. A failure resolves to
  // a placeholder rather than rejecting — one bad lookup must never block the
  // page or any other successfully-resolved name (see design doc).
  async function lookupName(id) {
    try {
      const b = await fetchApi(`/budgets/${id}`);
      return b?.name ?? $_("budgetDetails.unknownBudget");
    } catch (e) {
      return $_("budgetDetails.unknownBudget");
    }
  }

  async function loadRollupNames(loadedBudget, forId) {
    const parentId = loadedBudget?.rollup_parent_id ?? null;
    const childIds = loadedBudget?.rollup_child_ids ?? [];

    const [parentResult, childResults] = await Promise.all([
      parentId ? lookupName(parentId) : Promise.resolve(null),
      Promise.all(childIds.map((id) => lookupName(id))),
    ]);

    // Discard a stale response if budgetId changed while these lookups were
    // in flight (mirrors CategoriesView's stale-response guard).
    if (budgetId !== forId) return;
    parentName = parentResult;
    childNames = childResults;
  }

  async function load() {
    if (!budgetId) return;
    const forId = budgetId;
    loading = true;
    error = "";
    editingField = null;
    saveError = "";
    try {
      const res = await fetchApi(`/budgets/${forId}`);
      if (budgetId !== forId) return;
      budget = res;
      parentName = null;
      childNames = [];
      // Awaited (not fire-and-forget) so the details panel — including the
      // rollup section — never renders until parent/child names are resolved.
      // This deliberately avoids a separate "names still loading" substate: a
      // brief extra wait before the whole page appears is preferable to a
      // rollup section that flashes an "unknown budget" placeholder for an
      // entry that's actually still loading (not actually a failed lookup).
      await loadRollupNames(res, forId);
    } catch (e) {
      if (budgetId !== forId) return;
      error = e?.message || "Failed to load budget";
      budget = null;
    } finally {
      if (budgetId === forId) loading = false;
    }
  }

  // Mounting IS the "open" signal (the outlet's {#if route === "budgetDetails"}
  // owns mount/unmount). Re-fetch if budgetId changes while mounted (e.g. the
  // user clicks a different budget's name without leaving the details page).
  $effect(() => {
    budgetId;
    load();
  });

  function startEdit(field, currentValue) {
    if (!budget || isReadOnly(budget) || saving) return;
    editingField = field;
    draftValue = currentValue;
    saveError = "";
  }

  // `saving` re-entrancy guard: setting `disabled={saving}` on the input/
  // select that currently has focus (see the template below) makes the
  // browser fire a native `blur` event on it — which is wired to call
  // commitEdit() (the name field) or cancelEdit() (the select fields). Without
  // this guard, that synthetic blur re-enters commitEdit() a second time (a
  // duplicate PUT) or fires cancelEdit() mid-save (prematurely resetting
  // editingField/draftValue while the first commit's PUT+refetch is still in
  // flight, flashing the UI back to the stale value). Both functions bail
  // immediately once `saving` is true; the in-flight commitEdit() call is the
  // only one that will actually run to completion.
  function cancelEdit() {
    if (saving) return;
    editingField = null;
    draftValue = "";
    saveError = "";
  }

  async function commitEdit() {
    if (!budget || !editingField || saving) return;

    const patch = {};
    if (editingField === "name") {
      const v = draftValue.trim();
      if (!v || v === budget.name) {
        cancelEdit();
        return;
      }
      patch.name = v;
    } else if (editingField === "time_frame") {
      if (draftValue === budget.time_frame) {
        cancelEdit();
        return;
      }
      patch.time_frame = draftValue;
    } else if (editingField === "budget_type") {
      if (draftValue === budget.budget_type) {
        cancelEdit();
        return;
      }
      patch.budget_type = draftValue;
    }

    saving = true;
    saveError = "";
    try {
      const payload = buildUpdatePayload(budget, patch);
      await fetchApi(`/budgets/${budgetId}`, {
        method: "PUT",
        body: JSON.stringify(payload),
      });
      // The PUT response's rollup_child_ids/aggregated_* fields are not
      // reliable (update_budget never calls the server's .with_rollup() —
      // see design doc), so re-fetch via GET rather than trusting it.
      await load();
      onBudgetUpdated?.();
    } catch (e) {
      saveError = e?.message || "Failed to update budget";
    } finally {
      saving = false;
    }
  }

  function handleEditKeydown(e) {
    if (e.key === "Enter") commitEdit();
    if (e.key === "Escape") cancelEdit();
  }

  let readOnly = $derived(isReadOnly(budget));
  let rollup = $derived(rollupSummary(budget, parentName, childNames));

  function fmtRenewalDate(iso) {
    return new Date(iso).toLocaleDateString(undefined, { timeZone: "UTC" });
  }

  // Resolves a TIME_FRAMES value ("monthly"/"quarterly"/"yearly") to its
  // translated label. Guards against an empty/unexpected value rather than
  // indexing tf[0] directly — BudgetListItem.time_frame is a non-optional
  // String server-side so this shouldn't happen with well-formed data, but a
  // details page is exactly the wrong place to hard-crash on a display quirk.
  function timeFrameLabel(tf) {
    if (!tf) return tf ?? "";
    const key = `budgetDetails.period${tf[0].toUpperCase()}${tf.slice(1)}`;
    return $_(key);
  }
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">
      {budget ? budget.name : $_("budgetDetails.loading")}
    </h3>
    <button
      type="button"
      class="btn btn-sm btn-ghost gap-1.5"
      onclick={() => onBack?.()}
    >
      <ArrowLeft class="w-4 h-4" />
      {$_("budgetDetails.back")}
    </button>
  </div>

  {#if loading}
    <div
      class="flex flex-col items-center justify-center gap-3 py-16 text-base-content/60"
    >
      <span class="loading loading-spinner loading-lg text-primary"></span>
      <span class="text-sm">{$_("budgetDetails.loading")}</span>
    </div>
  {:else if error}
    <div class="alert alert-error">
      <AlertTriangle class="w-5 h-5" />
      <span>{error}</span>
    </div>
  {:else if !budget}
    <p class="text-sm text-base-content/50 py-4">{$_("budgetDetails.notFound")}</p>
  {:else}
    <div class="flex flex-col gap-3">
      {#if budget.closed_at}
        <div class="alert alert-warning text-sm">{$_("budgetDetails.closedNotice")}</div>
      {/if}
      {#if budget.archived_at}
        <div class="alert alert-warning text-sm">{$_("budgetDetails.archivedNotice")}</div>
      {/if}

      <!-- Name -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.nameLabel")}
        </div>
        {#if editingField === "name"}
          <input
            type="text"
            bind:value={draftValue}
            onblur={commitEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          />
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("name", budget.name)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>{budget.name}</span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "name" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Type -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.typeLabel")}
        </div>
        {#if editingField === "budget_type"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            <option value="time_based">{$_("budgetDetails.typePeriodic")}</option>
            <option value="project">{$_("budgetDetails.typeProject")}</option>
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("budget_type", budget.budget_type)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_type === "project"
                ? $_("budgetDetails.typeProject")
                : $_("budgetDetails.typePeriodic")}
            </span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "budget_type" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Period (hidden for project budgets — they track lifetime spend, not a rotating period) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.periodLabel")}
        </div>
        {#if budget.budget_type === "project"}
          <p class="text-sm text-base-content/60">{$_("budgetDetails.projectNote")}</p>
        {:else if editingField === "time_frame"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            {#each TIME_FRAMES as tf (tf)}
              <option value={tf}>{timeFrameLabel(tf)}</option>
            {/each}
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("time_frame", budget.time_frame)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>
              {timeFrameLabel(budget.time_frame)}
            </span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "time_frame" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>

      <!-- Rollover (display only; excluded for project budgets, matching budgetDisplay.js) -->
      {#if budget.budget_type !== "project"}
        <div class="rounded-xl bg-base-100 border border-base-300 p-3">
          <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
            {$_("budgetDetails.rolloverLabel")}
          </div>
          <p class="text-sm">
            {#if !budget.rollover_enabled}
              {$_("budgetDetails.rolloverOff")}
            {:else if budget.has_partial_category_rollover}
              {$_("budgetDetails.rolloverOnSome")}
            {:else}
              {$_("budgetDetails.rolloverOnAll")}
            {/if}
          </p>
        </div>

        <!-- Auto-renew (display only; excluded for project budgets) -->
        <div class="rounded-xl bg-base-100 border border-base-300 p-3">
          <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
            {$_("budgetDetails.autoRenewLabel")}
          </div>
          <p class="text-sm">
            {#if !budget.auto_renew}
              {$_("budgetDetails.autoRenewOff")}
            {:else if budget.next_renewal_at}
              {$_("budgetDetails.autoRenewOnNext", {
                values: { date: fmtRenewalDate(budget.next_renewal_at) },
              })}
            {:else}
              {$_("budgetDetails.autoRenewOn")}
            {/if}
          </p>
        </div>
      {/if}

      <!-- Rollup relationships (read-only list, not navigable in v1) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.rollupLabel")}
        </div>
        {#if !rollup.hasParent && !rollup.hasChildren}
          <p class="text-sm text-base-content/60">{$_("budgetDetails.rollupNone")}</p>
        {:else}
          <div class="flex flex-col gap-1 text-sm">
            {#if rollup.hasParent}
              <p>
                {$_("budgetDetails.rollupParent", {
                  values: { name: rollup.parentName ?? $_("budgetDetails.unknownBudget") },
                })}
              </p>
            {/if}
            {#if rollup.hasChildren}
              <p>
                {$_("budgetDetails.rollupChildren", { values: { names: rollup.childNames.join(", ") } })}
              </p>
            {/if}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>
```

- [ ] **Step 2: Verify the frontend still builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (the component isn't wired into `App.svelte` yet, so nothing renders it —
this only checks the Svelte/JS is syntactically and type-shape valid).

- [ ] **Step 3: Commit**

```bash
cd frontend
git add src/lib/BudgetDetails.svelte
git commit -m "feat(#240): add BudgetDetails outlet component"
```

---

## Task 5: Wire the details page into `App.svelte`

**Files:**
- Modify: `frontend/src/App.svelte`

**Locate every block below by its literal code content, NOT by a stated line number** — earlier
steps in this task shift later line numbers. Use `grep -n` on a distinctive substring (e.g.
`routeFromHash`, `displayBudgetName`, `chat-scroll-area`) to re-locate each block immediately
before editing it.

- [ ] **Step 1: Add imports**

Find:
```js
  import Insights from "./lib/Insights.svelte";
  import CategoriesView from "./lib/CategoriesView.svelte";
  import { ROUTES, routeFromHash, hashForRoute } from "./lib/router.js";
```
Replace with:
```js
  import Insights from "./lib/Insights.svelte";
  import CategoriesView from "./lib/CategoriesView.svelte";
  import BudgetDetails from "./lib/BudgetDetails.svelte";
  import {
    ROUTES,
    hashForRoute,
    resolveRoute,
    hashForBudgetDetails,
  } from "./lib/router.js";
```

(`routeFromHash` is dropped from this import: Steps 3-4 below replace its only 3 call sites in
`App.svelte` with `resolveRoute`, so the bare symbol would otherwise become dead/unused — `grep -n
"routeFromHash" frontend/src/App.svelte` should confirm zero hits once Task 5 is complete.)

- [ ] **Step 2: Add `budgetDetailsId` state and a `navigateToBudgetDetails` helper**

Find:
```js
  // Governs #chat-scroll-area's content (#233): "chat" (default) | "categories"
  // | "insights". Synced to window.location.hash so browser back/forward and a
  // hard refresh on a deep link both work; see frontend/src/lib/router.js for
  // the pure hash<->route mapping.
  let route = $state("chat");

  function navigate(name) {
    route = ROUTES.includes(name) ? name : "chat";
    const hash = hashForRoute(route);
    // Always a plain hash assignment (never history.replaceState) — even
    // clearing the hash back to "" this way pushes a new history entry, so
    // every route transition (entering AND leaving a route) is independently
    // back/forward-navigable. A prior version special-cased the "leaving"
    // direction with replaceState, which silently destroyed the entry being
    // left and broke Back after using the in-app "back to chat" affordance.
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }
```
Replace with:
```js
  // Governs #chat-scroll-area's content: "chat" (default) | "categories" |
  // "insights" | "budgetDetails" (#233, #240). Synced to window.location.hash
  // so browser back/forward and a hard refresh on a deep link both work; see
  // frontend/src/lib/router.js for the pure hash<->route mapping. The
  // "budgetDetails" route additionally carries a dynamic budget id, which
  // ROUTES/navigate() below don't know about — it lives in the sibling
  // budgetDetailsId state instead (mirrors how activeConversationId sits
  // beside route/activeScreen).
  let route = $state("chat");
  let budgetDetailsId = $state(null);

  function navigate(name) {
    route = ROUTES.includes(name) ? name : "chat";
    // navigate() can never target "budgetDetails" (it's not in ROUTES), so
    // leaving any stale id behind here would only ever be dead state — clear
    // it so a later re-entry into budgetDetails always starts from a fresh id.
    budgetDetailsId = null;
    const hash = hashForRoute(route);
    // Always a plain hash assignment (never history.replaceState) — even
    // clearing the hash back to "" this way pushes a new history entry, so
    // every route transition (entering AND leaving a route) is independently
    // back/forward-navigable. A prior version special-cased the "leaving"
    // direction with replaceState, which silently destroyed the entry being
    // left and broke Back after using the in-app "back to chat" affordance.
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }

  // Navigates to the budget details outlet for an arbitrary budget id (#240)
  // — separate from navigate() above since it carries a parameter ROUTES
  // doesn't model. Mirrors navigate()'s hash-sync convention.
  function navigateToBudgetDetails(budgetId) {
    route = "budgetDetails";
    budgetDetailsId = budgetId;
    const hash = hashForBudgetDetails(budgetId);
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    }
  }
```

- [ ] **Step 3: Update `handleHashChange` to resolve both `route` and `budgetDetailsId`**

Find:
```js
  // Browser back/forward. Only takes effect once the chat screen is showing —
  // the auth screen has no outlet to route.
  function handleHashChange() {
    if (activeScreen === "chat") {
      route = routeFromHash(window.location.hash);
    }
  }
```
Replace with:
```js
  // Browser back/forward. Only takes effect once the chat screen is showing —
  // the auth screen has no outlet to route.
  function handleHashChange() {
    if (activeScreen === "chat") {
      const resolved = resolveRoute(window.location.hash);
      route = resolved.route;
      budgetDetailsId = resolved.budgetId;
    }
  }
```

- [ ] **Step 4: Update the two remaining `routeFromHash` call sites**

Find (inside `saveSession`):
```js
    activeScreen = "chat";
    route = routeFromHash(window.location.hash);
    startFreshSession();
```
Replace with:
```js
    activeScreen = "chat";
    const resolvedOnLogin = resolveRoute(window.location.hash);
    route = resolvedOnLogin.route;
    budgetDetailsId = resolvedOnLogin.budgetId;
    startFreshSession();
```

Find (inside `fetchMe`):
```js
      activeScreen = "chat";
      route = routeFromHash(window.location.hash);
      // Token auto-loaded from localStorage must NOT repopulate old messages.
      startFreshSession();
```
Replace with:
```js
      activeScreen = "chat";
      const resolvedOnRestore = resolveRoute(window.location.hash);
      route = resolvedOnRestore.route;
      budgetDetailsId = resolvedOnRestore.budgetId;
      // Token auto-loaded from localStorage must NOT repopulate old messages.
      startFreshSession();
```

- [ ] **Step 5: Make the header budget name clickable (mobile span)**

Find:
```svelte
        {#if activeBudget}
          <span class="text-[11px] text-base-content/60"
            >&middot; {displayBudgetName(activeBudget.name)}</span
          >
        {/if}
```
Replace with:
```svelte
        {#if activeBudget}
          <span class="text-[11px] text-base-content/60">&middot;</span>
          <button
            type="button"
            class="text-[11px] text-base-content/60 hover:text-base-content hover:underline"
            aria-label={$_("header.budgetNameAria")}
            onclick={() => navigateToBudgetDetails(activeBudget.id)}
          >
            {displayBudgetName(activeBudget.name)}
          </button>
        {/if}
```

- [ ] **Step 6: Make the header budget name clickable (desktop span)**

Find:
```svelte
      {#if activeBudget}
        <span class="hidden md:inline text-[11px] text-base-content/60"
          >&middot; {displayBudgetName(activeBudget.name)}</span
        >
      {/if}
```
Replace with:
```svelte
      {#if activeBudget}
        <span class="hidden md:inline text-[11px] text-base-content/60">&middot;</span>
        <button
          type="button"
          class="hidden md:inline text-[11px] text-base-content/60 hover:text-base-content hover:underline"
          aria-label={$_("header.budgetNameAria")}
          onclick={() => navigateToBudgetDetails(activeBudget.id)}
        >
          {displayBudgetName(activeBudget.name)}
        </button>
      {/if}
```

- [ ] **Step 7: Add the outlet branch**

Find:
```svelte
          {#if route === "categories"}
            <CategoriesView {fetchApi} {activeBudget} onBack={() => navigate("chat")} />
          {:else if route === "insights"}
            <Insights {fetchApi} onClose={() => navigate("chat")} />
```
Replace with:
```svelte
          {#if route === "categories"}
            <CategoriesView {fetchApi} {activeBudget} onBack={() => navigate("chat")} />
          {:else if route === "insights"}
            <Insights {fetchApi} onClose={() => navigate("chat")} />
          {:else if route === "budgetDetails"}
            <BudgetDetails
              {fetchApi}
              budgetId={budgetDetailsId}
              onBack={() => navigate("chat")}
              onBudgetUpdated={fetchBudgets}
            />
```

- [ ] **Step 8: Verify the build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds with no errors.

- [ ] **Step 9: Manual verification**

Run: `cd frontend && pnpm run dev`

Using a logged-in session with at least one budget:

1. Click the budget name in the header (desktop width) — confirm it navigates to the details page
   in the main content area, and the URL hash becomes `#/budgets/<id>`.
2. Resize to a mobile width and repeat — confirm the mobile header name is equally clickable.
3. Confirm the details page shows name/period/type/rollover/rollup exactly matching what
   `GET /budgets/:id` returns for that budget (cross-check via devtools network tab or `curl`).
4. Click to edit the name, change it, confirm it saves and the header text updates without a full
   reload (via `onBudgetUpdated`/`fetchBudgets`).
5. Click to edit the period (a non-project budget) and the type (project vs. periodic)
   independently — confirm each persists across a reload.
6. If you have a rollup-parent budget, edit its name/period/type and confirm the rollup
   children/aggregated info is still shown correctly immediately after — this exercises the
   post-save `GET` re-fetch fix (the raw `PUT` response would otherwise incorrectly report no
   children).
7. Find or create a closed or archived budget, open its details page, and confirm no edit
   affordance (no pencil icon, no clickable field) is shown — text renders plain.
8. Click "Back to chat" — confirm it returns to the normal chat view and the hash clears.
9. Press Escape while on the details page — confirm it also returns to chat.
10. Use the browser's back/forward buttons while navigating in and out of the details page —
    confirm they work; hard-refresh while on `#/budgets/<id>` — confirm it deep-links back into the
    details page after auth restores.

- [ ] **Step 10: Run the full test suite**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing tests plus the new `router.test.js`/`budgetDetails.test.js` cases
green.

- [ ] **Step 11: Commit**

```bash
cd frontend
git add src/App.svelte
git commit -m "feat(#240): wire the budget details page into App.svelte"
```

---

## Task 6: Final pass

- [ ] **Step 1: Re-read the diff against the AC**

Run: `git diff origin/main...HEAD --stat` and `git diff origin/main...HEAD` from the worktree root.

Confirm against savvagent/nels#240's acceptance criteria (as amended by the routing-supersession
comment posted to the issue — see design doc's "Note on routing"):
- [ ] Clicking/tapping the budget name in the header (mobile and desktop) navigates to the details
      view at `/budgets/{budget_id}`. (Task 5 Steps 5-7)
- [ ] `router.js` parses `budgets/<segment>` generically (dynamic id now; a static token left for
      #241 without re-architecting). (Task 1)
- [ ] Details page shows name, period, type, rollover status, rollup parent/children. (Task 4)
- [ ] Name, period, and type are editable inline and persist via `PUT /budgets/:id`. (Task 4, Task 2)
- [ ] A back action returns to chat, consistent with `CategoriesView`/`Insights`. (Task 4 `onBack`,
      Task 5 Step 7)
- [ ] Closed/archived budgets render read-only. (Task 2 `isReadOnly`, Task 4)
- [ ] Backend validation errors surface inline rather than failing silently. (Task 4 `saveError`)
- [ ] `router.test.js` covers `budgets/{id}` parsing alongside the existing flat routes. (Task 1)

- [ ] **Step 2: Confirm no stray references or dead imports**

Run: `grep -n "routeFromHash" frontend/src/App.svelte` — expect **zero** hits (Task 5 Step 1 drops
it from the import; Steps 3-4 replace its only 3 call sites with `resolveRoute`).

Run: `grep -n "displayBudgetName(activeBudget.name)" frontend/src/App.svelte` — expect exactly 2
hits, both now inside `<button>` elements (Task 5 Steps 5-6), not bare `<span>`s.

- [ ] **Step 3: Final full check**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: both succeed.

- [ ] **Step 4: Rebase on origin/main if it has moved, resolving any conflict by keeping all
  tickets' changes**

Concurrent work on `frontend/src/App.svelte` (from #246's `fetchApi` timeout and #239's
`:global(.cat-table)` CSS — both in different regions of the file) may have merged to `main` since
this branch was cut. Run:

```bash
git fetch origin
git rebase origin/main
```

If a conflict appears in `App.svelte`, resolve it by keeping **both** sides' changes (this
ticket's header-button/outlet/import changes plus whatever the concurrent ticket touched) — the
changes are in different regions of the file and are not in tension. No conflict is expected in
`backend/src/budget.rs` (this ticket makes no backend changes) even though #238 also touches that
file.
