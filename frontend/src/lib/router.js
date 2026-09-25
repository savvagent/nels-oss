// Pure hash <-> route mapping for the main-content outlet (#233). No Svelte,
// no I/O — unit-tested directly, mirrors frontend/src/lib/chatWindow.js and
// commands.js. The reactive `route` state itself lives in App.svelte (a
// `$state` sibling to the existing `activeScreen`); this module only owns the
// pure translation between a URL hash and a route name, kept separate so it's
// testable under vitest's `environment: "node"` (no jsdom/Svelte compiler
// needed here).
//
// Hash-based (not pushState/history-based) deliberately: this is a
// single-`index.html` Vite SPA with a hand-rolled service worker (see
// frontend/public/sw.js) that has no "unknown path -> index.html" fallback
// rule. A hash never leaves index.html as the request path, so routing needs
// zero SW changes.

export const ROUTES = ["chat", "categories", "transactions", "insights", "history", "accounts", "retirement", "settings", "deleteAccount"];

// "#/categories" -> "categories", "#/insights" -> "insights"; anything else
// (empty, "#", "#/", unrecognized, non-string) falls back to "chat" — the
// outlet always has a valid view to show.
export function routeFromHash(hash) {
  if (typeof hash !== "string" || !hash.startsWith("#")) return "chat";
  const name = hash.replace(/^#\/?/, "");
  return ROUTES.includes(name) && name !== "chat" ? name : "chat";
}

// Inverse of routeFromHash. "chat" (the default) clears the hash entirely
// rather than round-tripping through "#/chat", so the URL stays clean for the
// common case.
export function hashForRoute(route) {
  return ROUTES.includes(route) && route !== "chat" ? `#/${route}` : "";
}

// Hierarchical "budgets/<segment>" path-segment routing (#240) — deliberately
// separate from the flat ROUTES/routeFromHash/hashForRoute above, not folded
// into them. A single "budgets/" prefix fans out into multiple segment KINDS:
// a dynamic budget-id segment (#240, wired below) and static tokens like
// "list" (#241, registered in BUDGET_STATIC_SEGMENTS without touching the
// id-parsing branch here).

const BUDGET_ID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// Static tokens recognized as the segment under "budgets/". #240 wired the
// dynamic-id branch; #241 registers "list" (the budgets list/overview page).
const BUDGET_STATIC_SEGMENTS = ["list"];

// "#/budgets/<uuid>" -> { kind: "id", id: "<uuid>" }
// "#/budgets/<token>" (a token registered in BUDGET_STATIC_SEGMENTS, e.g.
//   "list") -> { kind: "<token>" }
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

// Inverse of the static "list" case: always "#/budgets/list" (no id, unlike
// hashForBudgetDetails).
export function hashForBudgetsList() {
  return "#/budgets/list";
}

// Composes parseBudgetsPath with the existing flat routeFromHash into the one
// shape App.svelte needs at each of its 3 hash-resolution call sites: which
// outlet to show, plus a budget id when that outlet is the details page (null
// otherwise). A budgets/<unrecognized-token> hash still falls through to
// routeFromHash's own "chat" fallback, exactly like any other unrecognized
// hash today.
export function resolveRoute(hash) {
  const budgetsPath = parseBudgetsPath(hash);
  if (budgetsPath?.kind === "id") {
    return { route: "budgetDetails", budgetId: budgetsPath.id };
  }
  if (budgetsPath?.kind === "list") {
    return { route: "budgetsList", budgetId: null };
  }
  return { route: routeFromHash(hash), budgetId: null };
}

// Feature-flag gating for the retirement planner route (#469). When the runtime
// `retirement_planner_enabled` flag (surfaced by the backend via GET
// /api/auth/me) is OFF, a route resolved to "retirement" collapses to "chat" —
// so no #/retirement hash is ever recorded and the outlet always has a valid
// view. Composed with resolveRoute at App.svelte's hash-resolution sites (and
// mirrored by the guard inside App.svelte's navigate() for programmatic
// navigation); kept pure so it is unit-testable under vitest's node env.
export function applyFlaggedRoutes(resolved, { retirementPlannerEnabled }) {
  if (resolved.route === "retirement" && retirementPlannerEnabled !== true) {
    return { route: "chat", budgetId: null };
  }
  return resolved;
}
