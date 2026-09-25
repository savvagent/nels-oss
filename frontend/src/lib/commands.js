// Slash-command registry for the chat input (#56).
//
// Pure, framework-free helpers so they can be reasoned about (and, if a test
// runner is added later, unit-tested) independently of the Svelte component.
// The component owns side effects (clearing the view, calling the backend,
// rendering feedback); this module only describes the commands and parses
// input.

// The registry. `name` is the literal typed in the chat input; `descKey` is the
// i18n key for the palette description. Add a new entry here + its handler in
// App.svelte's runCommand to extend the palette.
export const COMMANDS = [
  { name: "/clear", descKey: "commands.clearDesc" },
  { name: "/issues-list", descKey: "commands.issuesListDesc" },
  { name: "/issues-create", descKey: "commands.issuesCreateDesc" },
  { name: "/budgets-list", descKey: "commands.budgetsListDesc" },
  { name: "/categories-list", descKey: "commands.categoriesListDesc" },
  { name: "/transactions-list", descKey: "commands.transactionsListDesc" },
  { name: "/budgets-insights", descKey: "commands.budgetsInsightsDesc" },
  { name: "/budgets-switch", descKey: "commands.budgetsSwitchDesc" },
  { name: "/tokens", descKey: "commands.tokensDesc" },
];

// True when the trimmed input looks like a slash-command invocation.
export function isCommand(input) {
  return typeof input === "string" && input.trimStart().startsWith("/");
}

// Parse a raw input string into { name, args } when it is a command, else null.
// `name` is the lowercased first token (e.g. "/issues-create"); `args` is the
// remainder, trimmed.
export function parseCommand(input) {
  if (!isCommand(input)) return null;
  const trimmed = input.trim();
  const spaceIdx = trimmed.indexOf(" ");
  if (spaceIdx === -1) {
    return { name: trimmed.toLowerCase(), args: "" };
  }
  return {
    name: trimmed.slice(0, spaceIdx).toLowerCase(),
    args: trimmed.slice(spaceIdx + 1).trim(),
  };
}

// Return the registry entries whose name matches the current input prefix, for
// the autocomplete palette. An input of exactly "/" (or "/" + partial command,
// before any space) surfaces matches; once a space is typed the user is into
// arguments and the palette closes.
export function matchCommands(input) {
  if (!isCommand(input)) return [];
  const trimmed = input.trimStart();
  if (trimmed.includes(" ")) return [];
  const prefix = trimmed.toLowerCase();
  return COMMANDS.filter((c) => c.name.startsWith(prefix));
}

// Return the known command name exactly matching the input's first token, or
// null. Used to detect unknown commands for clear feedback.
export function knownCommandName(input) {
  const parsed = parseCommand(input);
  if (!parsed) return null;
  return COMMANDS.some((c) => c.name === parsed.name) ? parsed.name : null;
}

// Split the `/issues-create` argument string into { title, body }. The first
// " | " (space-pipe-space) separates an optional body. Returns title (possibly
// empty) and body (possibly null).
export function parseIssueArgs(args) {
  if (!args) return { title: "", body: null };
  const sep = args.indexOf(" | ");
  if (sep === -1) return { title: args.trim(), body: null };
  return {
    title: args.slice(0, sep).trim(),
    body: args.slice(sep + 3).trim() || null,
  };
}

// Resolve a budget name (case-insensitive) against a budgets array for
// /budgets-switch. Returns one of:
//   { status: "empty" }                  — no name given
//   { status: "unknown" }                — no name matches
//   { status: "ambiguous", count }       — 2+ names match
//   { status: "ok", budget }             — exactly one match
// Pure: makes no network call and mutates nothing, so the "no state change"
// guarantee on the error paths holds structurally (resolver runs before any POST).
export function resolveBudgetByName(budgets, name) {
  const trimmed = (name || "").trim();
  if (!trimmed) return { status: "empty" };
  const target = trimmed.toLowerCase();
  const matches = (budgets || []).filter(
    (b) => (b.name || "").toLowerCase() === target,
  );
  if (matches.length === 0) return { status: "unknown" };
  if (matches.length > 1) return { status: "ambiguous", count: matches.length };
  return { status: "ok", budget: matches[0] };
}

// Whether the current user may flip `budget`'s owner-scoped is_default flag via
// POST /budgets/:id/default. Only the owner may do this — the backend enforces
// it too (Permission::Owner required, #238) — so a budget merely shared with the
// user (View/Edit) is never a valid target. Fails closed: a missing/falsy
// is_owner, or a null/undefined budget, is rejected.
//
// NOTE (#255, #391): nothing in the UI calls this today. Both the /budgets-switch
// chat command and the budgets-list-page Switch button now use the any-access
// POST /budgets/:id/activate (setActiveBudget). This helper is retained,
// intentionally unused, as the correct owner-only gate for any FUTURE surface that
// genuinely writes budgets.is_default via POST /budgets/:id/default (#238).
export function canSetAsDefault(budget) {
  return budget?.is_owner === true;
}

// Resolve the viewer's app-wide active budget from the combined owned+shared list returned by
// GET /budgets. `is_default` is a per-row, OWNER-scoped column (unique_default_budget_per_user is
// a partial index on owner_id, enforcing AT MOST one default per owner) — it can be true on a
// budget merely SHARED with the viewer, reflecting the remote owner's own default choice, not the
// viewer's (#266). Scope selection to owned rows: the viewer's own default, falling back to any
// owned budget (defensive — by convention (not a hard DB guarantee) create_budget/delete_budget
// keep an owner with >=1 budget pointed at exactly one owned default row, so this branch should be
// unreachable in practice, but is not enforced at the data layer), falling back to null (no active
// budget) when the viewer owns none — never silently picks a shared budget.
export function resolveActiveBudget(budgets) {
  const owned = (budgets || []).filter((b) => b?.is_owner);
  return owned.find((b) => b.is_default) ?? owned[0] ?? null;
}
