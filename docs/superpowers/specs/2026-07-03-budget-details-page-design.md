# Budget details page at /budgets/{budget_id} (#240) — Design

## Brief (verbatim AC, condensed)

Clicking/tapping the budget name in the header (mobile and desktop) must navigate to a new
budget details view rendered in the main router outlet, at path `/budgets/{budget_id}`.
`router.js` must support parsing `budgets/<segment>` hash paths generically (a dynamic id
segment for this ticket; leaving room for a future static `list` segment for #241 without
re-architecting). The details page must show: name, period, project-vs-periodic type, rollover
status, and rollup parent/children (read-only list) for the given budget. Name, period, and
project/periodic type must be editable inline and persist via `PUT /budgets/:id`. A back action
must return to the chat view, consistent with `CategoriesView`/`Insights`. Closed/archived
budgets must render read-only (no inline-edit affordances). Backend validation errors must
surface inline on the details page rather than failing silently. `router.test.js` must cover the
new `budgets/{id}` parsing alongside the existing flat routes.

Almost everything this needs already exists server-side (rollup, rollover, project-vs-periodic
are fully modeled and exposed via `GET/PUT /budgets/:id`); this is overwhelmingly a frontend gap.

**Note on routing (resolves an apparent spec/tracker mismatch):** the GitHub issue #240 body
currently on record still shows an earlier "flat `\"budgets\"` route, scoped to the active budget
only" design (its own "Design decisions locked for v1" §1, written when no reviewer response had
come back yet). Issue #241's on-file body independently proposes the exact same flat `"budgets"`
token in `ROUTES` for a *different* page (a budgets list/switcher). **Both are superseded** by an
explicit, more recent product-owner decision — relayed directly for this work rather than yet
reflected back into either issue body — adopting hierarchical `/budgets/{budget_id}` (this ticket)
/ `/budgets/list` (#241, deferred) path-segment routing instead of two competing flat tokens. This
spec implements the path-segment side of that decision; a comment is being posted to **both** #240
and #241 recording the supersession (see Phase 1/close-out of this ticket's workflow) so the
tracker stays authoritative for #241's future implementer, who will otherwise start from #241's
stale flat-route text the same way this ticket's stale text was found in round-1 review. Every
reference to "the AC" below means the routing-superseded AC, not the flat-route wording literally
still on GitHub.

## Assumptions

- **Routing stays hash-based** (`#/budgets/<uuid>`), consistent with #233's `router.js` and the
  same no-SW-fallback constraint documented there — a hash never leaves `index.html` as the
  request path.
- **`route` stays a flat string state** (`"chat" | "categories" | "insights" | "budgetDetails"`)
  rather than becoming an object, so every existing `route === "..."` comparison in `App.svelte`
  keeps working unchanged. The dynamic id parameter lives in a sibling `$state` (`budgetDetailsId`)
  set alongside `route`, mirroring how `activeConversationId` sits beside `route`/`activeScreen`
  today. This is the minimal change that satisfies "generic enough for a future static segment
  without re-architecting": the id-vs-static-token branching lives entirely inside `router.js`'s
  new `parseBudgetsPath`, not inside `App.svelte`'s state shape.
- **`router.js`'s existing exports (`ROUTES`, `routeFromHash`, `hashForRoute`) are unchanged.**
  They keep governing only the 3 flat routes; `"budgets/..."` is parsed by new, separate exports
  (`parseBudgetsPath`, `hashForBudgetDetails`, `resolveRoute`) so #241 can add a `"list"` static
  token to `BUDGET_STATIC_SEGMENTS` later without touching this ticket's id-parsing branch or any
  existing flat-route test.
- **`parseBudgetsPath` returns a discriminated `{ kind, ... }` shape** (`{ kind: "id", id }` for a
  UUID segment, `{ kind: "<token>" }` for a token found in `BUDGET_STATIC_SEGMENTS` — empty in
  this ticket, since #241 owns adding `"list"` — or `null` for anything else under `budgets/`).
  `resolveRoute(hash)` composes it with `routeFromHash` into `{ route, budgetId }` for `App.svelte`
  to consume at its 3 hash-resolution call sites (`saveSession`, `fetchMe`, `handleHashChange`),
  replacing the current bare `route = routeFromHash(window.location.hash)` there.
- **Budget id validation is a UUID-shaped regex, not a server round-trip.** A malformed segment
  under `budgets/` (not a UUID, not a recognized static token) resolves to `null` from
  `parseBudgetsPath`, and `resolveRoute` falls through to the existing `routeFromHash` fallback
  (`"chat"`) — mirroring how an unrecognized flat route already falls back today. A
  syntactically-valid-but-nonexistent/inaccessible budget id is a *runtime* concern (404/403 from
  `GET /budgets/:id`), handled by `BudgetDetails.svelte`'s existing loading/error state, not by the
  router.
- **`BudgetDetails.svelte` self-fetches**, matching `CategoriesView`/`Insights`. Props are
  `{ fetchApi, budgetId, onBack }` per the ticket's proposed design, **plus one addition**:
  `onBudgetUpdated` (called after every successful `PUT`). Precedent for adding beyond the
  ticket's illustrative prop list already exists (`CategoriesView` takes `activeBudget` beyond its
  base two; `Sidebar` takes `onRename`/`onDelete` callback props) — `App.svelte`'s own
  `activeBudget`/`budgets` state (driving the header name and status strip) must reflect an
  in-page rename, so `App.svelte` wires `onBudgetUpdated={fetchBudgets}` (the same refresh already
  used after every other budget mutation path).
- **Rollup parent/child *names* are resolved via per-id `GET /budgets/:id` fetches, not a
  `budgets` list prop.** `BudgetListItem` only exposes `rollup_parent_id`/`rollup_child_ids` as
  UUIDs — no names. Passing `App.svelte`'s `budgets` array would be simpler but is lossy: that
  list excludes archived budgets by default (#50), and an archived budget remains a legitimate
  rollup parent/child ("archived budgets remain fully readable... only affects default list
  visibility"). A direct fetch-by-id is correct regardless of the caller's list-filtering state, at
  the cost of up to `1 + rollup_child_ids.length` extra round trips — acceptable for a details page
  that is not on a hot path. Each name lookup is independently best-effort: `lookupName`'s own
  try/catch converts a failed fetch into a placeholder value rather than a rejection, so a plain
  `Promise.all` over parent + children never rejects as a whole (equivalent to `allSettled` given
  every individual promise is already guaranteed to resolve) — one failed related-budget fetch
  degrades to a placeholder for that one entry, never blocks the page or the primary budget's own
  load. The whole details panel (including the rollup section) waits for these lookups to resolve
  before rendering at all, rather than rendering the panel early and letting the rollup section
  flash a placeholder while lookups are still in flight — see Architecture.
- **`budget_limit` must be echoed back verbatim on every `PUT`, unedited.** `update_budget` binds
  `budget_limit` directly from the payload with **no** `COALESCE`-preserve-when-absent (unlike
  `rollover_enabled`/`budget_type`/`amount_mode`, which are all `COALESCE($n, existing)`) — omitting
  it would silently null out a `'fixed'`-mode budget's stored amount on every inline edit made from
  this page. For a `'fixed'`-mode budget, `GET`'s `budget_limit` **is** the raw stored value
  (`resolve_base_amount("fixed", limit, _) == limit`), so echoing it is exact. For `'derived'` mode
  the raw column is inert (`resolve_base_amount` ignores it entirely), so echoing the computed sum
  back is a functional no-op today; it is documented as a known non-goal edge (see Risks).
- **The `PUT` response is never used directly to refresh the page's `budget` state — a follow-up
  `GET /budgets/:id` is always issued after a successful `PUT` instead.** Verified in
  `update_budget` (`backend/src/budget.rs`, the handler's final `Ok(Json(BudgetListItem { ...
  }.with_amounts(...)))`): unlike `get_budget`, it never calls `.with_rollup(child_ids,
  aggregated_base, aggregated_effective)` — so **every** successful `PUT` response reports
  `rollup_child_ids: []` and `aggregated_base_amount`/`aggregated_effective_amount` collapsed to the
  budget's own (non-aggregated) figures, regardless of whether the budget is actually a rollup
  parent (`rollup_parent_id`, the child-side backlink, *is* bound correctly — only the parent-side
  `rollup_child_ids`/aggregated fields are wrong). Trusting the `PUT` response directly would make a
  rollup-parent budget's details page incorrectly flip to "not part of a rollup" after any inline
  edit. This is a pre-existing backend response-construction gap (shared by `close_budget`/
  `archive_budget`/`unarchive_budget`, which have the same `Vec::new()`-without-`.with_rollup()`
  pattern) that nothing in the frontend has depended on until now, since `activeBudget` has always
  come from the list endpoint (which does call `.with_rollup()`). Fixed entirely on the frontend,
  at the one-request cost of a `GET` after every successful edit, keeping this ticket backend-free
  as scoped; see Risks for the alternative (a backend fix) considered and not taken.
- **`auto_renew`/`rollover_enabled`/`amount_mode`/`budget_type` are omitted from the `PUT` payload
  unless a patch explicitly changes `budget_type`.** All three of `rollover_enabled`/`budget_type`/
  `amount_mode` are server-`COALESCE`d when absent (preserved). `auto_renew` has no `COALESCE` in
  the SQL itself, but the handler resolves it in code (`payload.auto_renew.unwrap_or(existing)`)
  before binding, which likewise preserves-on-absent — **and** it is the mechanism that correctly
  clears auto-renew when converting to a project. Explicitly echoing `auto_renew: true` while also
  switching `budget_type` to `"project"` would trip the handler's own `400` guard ("Only
  time-based budgets can auto-renew"); omitting it instead lets the server's existing to-project
  auto-renew-clear behavior run exactly as it does for every other update path. `name`/`description`/
  `time_frame` have no such preservation and are always sent (see Architecture).
- **Editable-field scope is exactly the AC's three fields**: name, `time_frame` (period), and
  `budget_type` (project vs. periodic). Rollover, auto-renew, and rollup relationships are
  display-only in v1 (matches the ticket's Proposed design point 5 and the Design Decisions
  section explicitly; no control is rendered for them).
- **The period selector is hidden (not merely disabled) when `budget_type` is `"project"`**,
  replaced by a short explanatory note. A project budget tracks lifetime spend
  (`created_at -> closed_at`) rather than rotating through `time_frame` periods (AGENTS.md #48);
  showing an editable period control for a project budget would be actively misleading, not just
  unused chrome.
- **Empty-name submission is silently declined client-side**, mirroring `Sidebar.svelte`'s
  `commitRename` (`trim()`; if empty, revert without calling the API) — never sent to the server.
  **Duplicate-name rejection has no current server-side implementation** (`grep` of `budget.rs`/
  migrations turns up no unique constraint or explicit duplicate check on `budgets.name`; the
  AC's "surface the existing backend validation error... inline" is honored generically: **any**
  non-2xx `PUT` response (`fetchApi` throws with the response body as `Error.message`) is caught
  and rendered inline as `saveError`, whatever the backend does or does not currently reject. This
  is forward-compatible with a future server-side uniqueness check with zero frontend changes, and
  does not invent new backend behavior outside this ticket's stated scope (backend work is
  explicitly framed as already-done).
- **Closed AND archived both disable inline-edit affordances on this page**, per the ticket's
  explicit Edge Cases text. This is a stricter UI-only rule than the backend actually enforces
  (AGENTS.md #50 is explicit that archiving is *not* read-only server-side — only *closing* a
  project budget is, via `ensure_not_closed`). Treated as a deliberate, ticket-specified UX choice
  scoped to this page only: it doesn't change what the API permits, doesn't touch any other archived
  entry point (chat, etc.), and is a strict narrowing (never widens what's editable), so it can't
  violate the gate the backend already enforces.
- **No new automated component tests.** Same finding as #233's design doc: `vitest.config.js` runs
  `environment: "node"` with no jsdom/testing-library, and every existing `*.test.js` in this repo
  tests a pure `.js` module only. Per that established pattern, the testable logic is extracted into
  two pure modules (`router.js`'s new exports; a new `budgetDetails.js` for payload-building and
  display-formatting helpers) with real unit coverage; `BudgetDetails.svelte` itself is verified via
  `pnpm run build` plus manual exercise (`pnpm run dev`).
- **i18n**: new keys added to all 6 locale files (`en`/`de`/`es`/`fr`/`it`/`pt`) under a new
  `budgetDetails.*` namespace plus one `header.*` addition for the clickable name's `aria-label`,
  matching the existing per-locale JSON structure and the #233/#232 precedent of translating (not
  just English-duplicating) every locale.

## Goal & Success Criteria

Give users a dedicated, editable details view for any budget they own or can edit, reachable by
clicking the header's budget name, built on a router extension general enough for #241's future
`/budgets/list` route.

- Clicking/tapping the header budget name (mobile and desktop) navigates to
  `#/budgets/<activeBudget.id>` and renders `BudgetDetails` in the main outlet.
- `router.js` correctly parses `budgets/<uuid>` (this ticket) and leaves `budgets/<static-token>`
  parsing as an additive extension point for #241, without changing any existing flat-route
  behavior or breaking any existing `router.test.js` case.
- The details page shows name, period, type, rollover status, and rollup parent/children (names,
  not raw ids) for the given budget, sourced from `GET /budgets/:id`.
- Name, period, and type are each independently inline-editable and persist via
  `PUT /budgets/:id`, without corrupting any field the user didn't touch (no accidental
  `budget_limit`/`auto_renew`/`rollover_enabled`/`amount_mode` clobbering).
- A closed or archived budget renders fully (including its rollup/rollover history) with every
  inline-edit affordance disabled.
- Any `PUT` failure (validation or otherwise) surfaces inline on the page, never a silent no-op.
- A back action returns to `route === "chat"` and clears the hash, identical to
  `CategoriesView`/`Insights`'s existing `onBack`/`navigate("chat")` pattern.

## Scope

**In scope:** `frontend/src/lib/router.js` (+ test, additive exports only), a new
`frontend/src/lib/budgetDetails.js` (+ test, pure payload/display helpers), a new
`frontend/src/lib/BudgetDetails.svelte`, `frontend/src/App.svelte` (header markup, route state,
outlet wiring, `fetchBudgets` callback wiring), 6 locale JSON files (new keys only).

**Out of scope** (per the ticket): the `/budgets/list` route/page itself (#241 — only the router's
generic segment-parsing groundwork is built here); drill-down navigation from a rollup
parent/child into *its own* details page (v1 shows names as a read-only list); a dedicated "Edit
budget" modal (inline edit-in-place is the chosen v1 pattern); the two documented chat-parity gaps
in `rag.rs`'s `UPDATE_BUDGET` arm (`time_frame`/`budget_type` on an existing budget via chat); any
backend/Rust change (the existing `GET`/`PUT /budgets/:id` already provide everything needed).

## Architecture

- **`frontend/src/lib/router.js`** (additive): a `BUDGET_ID_RE` UUID pattern and an (initially
  empty) `BUDGET_STATIC_SEGMENTS` array back a new `parseBudgetsPath(hash)`, which strips the
  `budgets/` prefix and classifies the remaining single segment as `{ kind: "id", id }` (UUID
  match), `{ kind: "<token>" }` (a `BUDGET_STATIC_SEGMENTS` match — none registered yet), or `null`
  (anything else, including a segment containing another `/`, an empty segment, or no `budgets/`
  prefix at all). `hashForBudgetDetails(budgetId)` is the inverse for the id case
  (`` `#/budgets/${id}` ``, or `""` for a falsy id). `resolveRoute(hash)` composes
  `parseBudgetsPath` with the existing `routeFromHash`: `{ kind: "id" }` maps to
  `{ route: "budgetDetails", budgetId: id }`; anything else falls through to
  `{ route: routeFromHash(hash), budgetId: null }`. `ROUTES`/`routeFromHash`/`hashForRoute` are
  untouched.
- **`frontend/src/lib/budgetDetails.js`** (new, pure, mirrors `budgetDisplay.js`):
  - `TIME_FRAMES = ["monthly", "quarterly", "yearly"]` (the exact `BudgetPayload.time_frame`
    domain) for the period `<select>`.
  - `buildUpdatePayload(existing, patch = {})` returns the exact `PUT` body: `name`/`time_frame`
    from `patch` falling back to `existing` (always both present — no server-side preservation for
    either); `description: existing.description ?? null` (straight passthrough); `budget_limit:
    existing.budget_limit ?? null` (echoed verbatim, see Assumptions); `budget_type: patch.budget_type`
    (left `undefined` when not patched, so `JSON.stringify` naturally omits the key and the
    server's own `COALESCE` preserves it — this is also what correctly clears `auto_renew` on a
    to-project conversion, see Assumptions). `rollover_enabled`/`auto_renew`/`amount_mode` are never
    included.
  - `rollupSummary(budget, parentName, childNames)` — pure formatter returning
    `{ hasParent, parentName, hasChildren, childNames }` for the template to render (keeps the
    "omit the section / show 'not part of a rollup'" branch and the name-join logic testable
    without mounting the component).
  - `isReadOnly(budget)` — `!!(budget?.closed_at || budget?.archived_at)`.
- **`frontend/src/lib/BudgetDetails.svelte`** (new): props `{ fetchApi, budgetId, onBack,
  onBudgetUpdated }`. On mount / `budgetId` change (an `$effect`, same convention as
  `CategoriesView`): fetch `` `/budgets/${budgetId}` `` into `budget`; on success, **await** a
  best-effort parallel lookup (`Promise.all`, each individual lookup's own try/catch guaranteeing
  it never rejects — equivalent to `allSettled`) for `rollup_parent_id`'s name (if set) and each
  `rollup_child_ids` entry's name (if any) via the same `GET /budgets/:id` before clearing
  `loading`, storing `parentName`/`childNames` (a failed individual lookup renders a
  `budgetDetails.unknownBudget` placeholder for that one entry, per Assumptions). Awaiting these
  lookups before the panel renders at all — rather than rendering early and letting the rollup
  section resolve independently — avoids a transient state where a name that's merely still
  loading is indistinguishable from one that failed. Own `loading`/`error` state, mirroring
  `CategoriesView`'s stale-response guard (capture `budgetId` at fetch start; a stale response for
  a since-changed `budgetId` is discarded). Inline editing: one `editingField` state
  (`null | "name" | "time_frame" | "budget_type"`), a `draftValue`, and a `saving`/`saveError` pair.
  `startEdit(field)` and `cancelEdit()`/`commitEdit()` are all no-ops while `saving` is true (a
  focused input/select being `disabled={saving}` fires a native `blur`, which is wired to
  `commitEdit`/`cancelEdit` — without this re-entrancy guard that synthetic blur would re-enter
  mid-save, causing a duplicate `PUT` or a premature cancel). `startEdit(field)` is additionally a
  no-op when `isReadOnly(budget)`. Committing calls
  `buildUpdatePayload(budget, patch)`, `PUT`s via `fetchApi`, and on success **re-invokes the same
  `load()` used at mount (a fresh `GET /budgets/:id`) rather than assigning the `PUT` response
  directly to `budget`** — the `PUT` response's `rollup_child_ids`/aggregated-amount fields are not
  reliable (see Assumptions); a follow-up `GET` is the only response in this flow that correctly
  calls the server's `.with_rollup()` path. `editingField`/`saveError` clear once the re-fetch
  resolves; `onBudgetUpdated?.()` fires so `App.svelte` refreshes `activeBudget`/`budgets` too. A
  `PUT` failure sets `saveError` from the caught `Error.message` and leaves `editingField` open (so
  the input/select and the error stay visible together, letting the user retry or cancel) without
  issuing the follow-up `GET`. `onBack` renders a "back to chat" affordance identical in
  placement/markup to `CategoriesView`'s.
- **`frontend/src/App.svelte`**:
  - Import `BudgetDetails`, and add `resolveRoute`, `hashForBudgetDetails` to the existing
    `router.js` import.
  - New `let budgetDetailsId = $state(null);` beside `let route = $state("chat")`.
  - New `function navigateToBudgetDetails(id) { route = "budgetDetails"; budgetDetailsId = id; const hash = hashForBudgetDetails(id); if (window.location.hash !== hash) window.location.hash = hash; }`.
  - `navigate(name)` (the existing flat-route function) additionally resets `budgetDetailsId = null`
    — it can never target `"budgetDetails"` itself (not in `ROUTES`), so this just prevents stale
    id state from lingering after leaving the details page via the existing "back"/Escape paths.
  - The 3 existing `route = routeFromHash(window.location.hash)` call sites (`saveSession`,
    `fetchMe`, `handleHashChange`) become `const resolved = resolveRoute(window.location.hash); route = resolved.route; budgetDetailsId = resolved.budgetId;`.
  - The header's two `<span>...{displayBudgetName(activeBudget.name)}</span>` blocks (mobile
    ~L1695-1702, desktop ~L1714-1718 as of this writing — confirm fresh, see note below) become
    `<button type="button" onclick={() => navigateToBudgetDetails(activeBudget.id)}>`, keeping
    identical text content/classes plus a new `aria-label` (`header.budgetNameAria`) and minimal
    interactive styling (`hover:underline` or similar — no functional class changes needed beyond
    making it an actual button).
  - The outlet `{#if route === "categories"} ... {:else if route === "insights"} ... {:else if ...}`
    chain (`#chat-scroll-area`) gains one more branch:
    `{:else if route === "budgetDetails"}<BudgetDetails {fetchApi} budgetId={budgetDetailsId} onBack={() => navigate("chat")} onBudgetUpdated={fetchBudgets} />`.
  - No change needed to the Escape handler (already `route !== "chat"` generic) or the
    chat-input-focus guard (already `route === "chat"` generic) — both already treat any non-chat
    route uniformly, which now correctly includes `"budgetDetails"`.

  *(Line numbers above are from this investigation and may have shifted since — the implementer
  must re-locate every block by grepping its literal current content, exactly as the #233 plan
  instructs, before editing.)*

## Error Handling & Edge Cases

- Malformed/unknown segment under `budgets/` (not a UUID, not a registered static token) ->
  `resolveRoute` falls back to `{ route: "chat", budgetId: null }` — never throws, never shows a
  blank/broken outlet.
- `budgetId` syntactically valid but the budget doesn't exist, isn't shared with the user, or the
  fetch otherwise fails -> `BudgetDetails`'s existing loading/error state (mirrors
  `CategoriesView`), not a router-level concern.
- Budget has neither `rollup_parent_id` nor a non-empty `rollup_child_ids` -> the rollup section
  renders `budgetDetails.rollupNone` ("not part of a rollup") rather than being omitted entirely,
  per the ticket's edge case (either omit-or-show-fallback is acceptable per the ticket; a visible
  fallback is chosen for consistency with every other field always rendering something).
- A related-budget name lookup (parent or a child) fails independently -> that one entry renders a
  placeholder (`budgetDetails.unknownBudget`); every other successfully-resolved name and the page
  itself remain fully functional.
- `closed_at`/`archived_at` set -> page renders fully (all display fields, including rollup/
  rollover history), every `startEdit` call is a no-op, and the field controls render as plain text
  (no pencil/edit affordance shown at all — not merely a disabled input) plus a
  `budgetDetails.closedNotice`/`archivedNotice` banner.
- Renaming to an empty/whitespace-only string -> client-side no-op (trimmed empty -> silently
  revert, no `PUT` sent), mirroring `Sidebar.svelte`'s existing `commitRename`.
- Any `PUT` failure (validation, permission, network) -> `saveError` renders inline next to the
  still-open editing control; the field is not silently reverted and no other page state changes.
- Switching `budget_type` to `"project"` while `auto_renew` was on -> handled entirely server-side
  (payload omits `auto_renew`, so the existing to-project auto-renew-clear logic runs); the next
  successful response reflects `auto_renew: false`/`next_renewal_at: null`, and the page's own
  auto-renew display line simply stops rendering (its existing display condition already excludes
  project budgets).
- Navigating away (back / Escape / another header click) mid-edit -> in-flight edit state is
  discarded (component unmounts or `budgetId` changes); no partial/unsaved state persists anywhere
  since nothing is saved until an explicit commit.

## Testing Approach

- **Unit (Vitest, `environment: "node"`)**:
  - `frontend/src/lib/router.test.js` (extended) — `parseBudgetsPath` (valid UUID -> `{kind:"id"}`,
    case-insensitive UUID, unknown token -> `null`, empty segment -> `null`, nested extra segment
    -> `null`, no `budgets/` prefix -> `null`, non-string/`null`/`undefined` hash -> `null`);
    `hashForBudgetDetails` (id -> `` `#/budgets/${id}` ``, falsy -> `""`); `resolveRoute` (a
    `budgets/<uuid>` hash -> `{route:"budgetDetails", budgetId}`, every existing flat-route case ->
    `{route, budgetId:null}`, an unrecognized `budgets/list` hash today -> `{route:"chat",
    budgetId:null}` since #241 hasn't registered `"list"` yet). All existing `routeFromHash`/
    `hashForRoute`/`ROUTES` cases stay green unmodified.
  - `frontend/src/lib/budgetDetails.test.js` (new) — `buildUpdatePayload` (no patch echoes
    name/time_frame/description/budget_limit and omits `budget_type`/`rollover_enabled`/
    `auto_renew`/`amount_mode` entirely — asserted via `JSON.stringify` not containing those keys,
    not just `undefined` equality; a `name` patch overrides only `name`; a `budget_type` patch is
    the only case that includes the key); `rollupSummary` (no parent/no children -> `hasParent:
    false, hasChildren: false`; parent set -> `hasParent: true` with the resolved name; children
    set -> joined name list); `isReadOnly` (neither set -> `false`; either alone -> `true`; both ->
    `true`).
- **Manual** (`cd frontend && pnpm run dev`): click the header budget name (mobile viewport and
  desktop) -> lands on the details page, hash becomes `#/budgets/<id>`; every displayed field
  matches what `GET /budgets/:id` returns for a rollover-enabled, a project, and a rollup-parent
  budget; edit name/period/type independently and confirm each persists across a reload; **edit a
  rollup-parent budget's name/period/type and confirm its rollup children/aggregated amount are
  still shown correctly immediately after the save (the specific regression the `PUT`-response
  rollup-field gap would otherwise cause)**; attempt an edit on a closed and an archived budget and
  confirm no edit affordance is shown; trigger a `PUT` failure (e.g. via devtools network
  throttling/offline) and confirm the error renders inline without losing the in-progress edit;
  Back button and Escape both return to chat; browser back/forward and a hard refresh on
  `#/budgets/<id>` all resolve correctly once authed; renaming the active budget updates the header
  text without a full reload.
- `cd frontend && pnpm test && pnpm run build` before opening the PR (existing repo convention, no
  dedicated lint script in this package).

## Risks & Open Questions

- **Saving an edit re-shows the full-page loading spinner** (`BudgetDetails.svelte` reuses its
  mount-time `load()` for the post-`PUT` re-fetch — see Architecture — which also flips `loading`
  back to `true`), rather than keeping the existing content visible with a smaller inline "saving"
  indicator. A minor UX rough edge, not a functional bug; accepted for v1 rather than adding a
  second, separate loading substate for a single-purpose details page.
- **The `update_budget` response's missing `.with_rollup()` call is a real backend gap** (see
  Assumptions) — a smaller, more surgical fix would be adding the same `.with_rollup(get_child_ids,
  get_agg_base, get_agg_effective)` call `get_budget` already makes, which would let the frontend
  trust the `PUT` response directly and drop the extra `GET`. Deliberately **not** taken in this
  ticket: it's a backend change to a handler this issue's own framing says is already "done," it
  would touch `budget.rs` (a file two other in-flight tickets — #238 and, functionally,
  auto-renew/rollup logic generally — also touch, raising rebase-conflict surface for no
  frontend-visible gain over the chosen fix), and the same gap already exists on
  `close_budget`/`archive_budget`/`unarchive_budget` — fixing it only for `update_budget` here would
  be inconsistent, and fixing it everywhere is out of this ticket's scope. Worth filing as its own
  follow-up issue; flagged for reviewer awareness.
- **`budget_limit` round-tripping in `'derived'` mode** (see Assumptions) writes the
  currently-computed category sum into the raw `budget_limit` column on every inline edit made from
  this page, even though it's functionally inert while `amount_mode` stays `'derived'`. This is a
  deliberate, documented trade-off given the GET response never exposes the true raw value
  separately from the computed one; it only becomes observable if some *other*, unrelated future
  change makes the raw column meaningful again without itself supplying a fresh value at that time.
  Flagging for reviewer awareness, not proposing a fix within this ticket's scope.
- **No server-side duplicate-name check exists today** (see Assumptions) — the AC's "surface... a
  name that collides with another budget" is satisfied only in the sense that *whatever* error the
  backend does return will surface inline; it will not currently produce a collision-specific error
  because none exists. Out of scope to add server-side (backend is explicitly framed as
  already-complete for this issue).
- **Archived-but-editable is a deliberate frontend-only narrowing** (see Assumptions) that diverges
  from AGENTS.md #50's explicit "archiving is NOT read-only" backend guarantee. If a future ticket
  wants archived budgets editable from this page too, it's a one-line change to `isReadOnly`
  (drop the `archived_at` check) with no backend implications either way.
- **Rollup parent/child name-resolution cost**: up to `1 + rollup_child_ids.length` extra `GET`
  requests per page load. Acceptable for v1 given this is a details page, not a list/hot path; if a
  budget acquires many rollup children this could be revisited (e.g. a future bulk-lookup endpoint),
  but no such endpoint exists today and adding one is backend work outside this ticket's stated
  scope.
