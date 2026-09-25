# Router-driven main content outlet (#233) — Design

## Brief (verbatim AC, condensed)

Categories, insights, and similar chat-triggered views currently render as a mix of inline
HTML-in-chat (`categories_table_html` injected via `{@html msg.table_html}`) and boolean-toggled
modals (`showInsights`/`showSettings` gating always-mounted `<Insights>`/`<Settings>`). The header,
zero-based/budget status strip, and bottom prompt/input region must stay exactly as they are today.
The main chat response area must become a router outlet: a single region whose content is
determined by the current route. Categories and Insights move into that outlet (categories stops
being a chat bubble; Insights stops being a modal). A normal chat reply with no matching route
falls back to the existing chat conversation view. Both LLM-detected intents (`ChatResponse`
fields) and slash commands must be able to trigger a route change. Switching routes must not lose
in-flight chat state or reload the page. Settings stays a modal — out of scope (see Assumptions).

## Assumptions

- **No router library.** `frontend/` is a plain Vite SPA (confirmed: no SvelteKit, no router
  package in `package.json`, zero `pushState`/`location.hash`/`popstate` usage anywhere today). The
  entire routing surface this ticket needs is 3 flat states with no params and no nesting
  (`chat` | `categories` | `insights`), gated only while `activeScreen === "chat"`. A hand-rolled
  route store is proportionate; pulling in a dependency (e.g. `svelte-spa-router`) would be
  over-engineering for 3 states.
- **Hash-based, not history/pushState-based.** `frontend/vite.config.js` builds a single
  `index.html` with a hand-rolled service worker (no `vite-plugin-pwa`, no multi-page entries). A
  `pushState` router would require the SW's fetch handler to fall back to `index.html` for unknown
  paths — an SW change outside this ticket's scope and risk budget. A hash (`#/categories`,
  `#/insights`) never leaves `index.html` as the request path, so it needs zero SW changes, works
  with the existing `history.replaceState` calls in `handleBillingQueryParams()` (which only ever
  touch the query string, never the hash), and gets browser back/forward + deep-linking for free.
- **The outlet is the message-list scroll container, not all of `<main>`.** "Main chat response
  area" = `#chat-scroll-area` (`App.svelte` ~L2007, `class="flex-grow p-4 md:p-6 overflow-y-auto
  space-y-4 bg-base-100/40"`). The header (~L1566-1640), the status micro-strip (~L1964-2008), and
  the bottom prompt/input (~L2096-2206) are siblings of this div and are untouched. This matches
  the AC's "remain exactly as they are today" literally and keeps `pinnedToBottom`/
  `handleChatScroll` scroll behavior applying uniformly regardless of which view is active.
- **Categories and Insights views self-fetch**, matching the existing `Insights.svelte` convention
  (props in: `fetchApi` + context; no data via props). `ChatResponse.categories_table_html` is
  still sent by the backend for a `LIST_CATEGORIES` turn, but the frontend now uses its mere
  presence only as a "navigate to categories" signal and lets the new `CategoriesView` re-fetch via
  the existing `GET /budgets/:id/categories-table` REST endpoint — one extra round trip, traded for
  a single code path (`CategoriesView` behaves identically whether reached via slash command or
  chat intent). No backend changes are needed or made.
- **Insights stops being a DaisyUI modal but keeps its internal `fetchApi`/period-selector/KPI
  logic untouched.** Only the outer wrapper (`modal modal-open` / `modal-box` / `modal-backdrop`,
  `role="dialog"`/`aria-modal`) and the `open` prop/gate are removed — mount/unmount (owned by the
  outlet's `{#if route === "insights"}`) replaces the `open` gate, and `$effect` now loads on every
  mount instead of on `open` becoming true. This is a mechanical simplification, not a rewrite.
- **Settings stays exactly as-is** (a modal, `showSettings` boolean) per the ticket's explicit
  scoping note — it is not touched by this change at all.
- **Escape key**: when `route !== "chat"`, Escape now navigates back to `"chat"` (replacing the old
  `showInsights` branch in `handleKeydown`). Categories has no existing Escape-close precedent
  (it was never a modal) but gets the same convention for consistency now that it's a sibling
  outlet view to Insights.
- **i18n**: new keys are added to all 6 locale files (`en`/`de`/`es`/`fr`/`it`/`pt`), matching the
  existing per-locale JSON structure. Where an existing key's wording already fits the outlet
  context (`insights.close`, `insights.dismiss`), it is reused rather than duplicated. New keys are
  scoped under a new `categories.*` namespace plus one addition to `commands.*`
  (`commands.categoriesOpened`, mirroring the existing `commands.insightsOpened`).
- **No new automated component tests.** `vitest.config.js` runs `environment: "node"` with no
  jsdom/testing-library (confirmed: every existing `*.test.js` tests a pure `.js` module only, and
  `frontend/package.json` has no `@testing-library/svelte` dependency). Per the established
  pattern (see `docs/superpowers/plans/2026-07-03-simplify-chat-header.md` Task 2), the
  route-mapping logic is extracted into a pure, framework-free module specifically so it *is*
  unit-testable the same way `chatWindow.js`/`commands.js` are; the Svelte components themselves
  are verified manually (`pnpm run dev`) plus `pnpm run build`.

## Goal & Success Criteria

Replace ad hoc inline-HTML-in-chat and modal-toggle rendering for categories and insights with one
router-governed outlet, without touching the header/status-strip/prompt regions or losing chat
state on navigation.

- A reactive `route` state (`"chat" | "categories" | "insights"`) governs `#chat-scroll-area`'s
  content; header/status-strip/prompt render unconditionally regardless of `route`.
- `/categories-list` and a `LIST_CATEGORIES` chat intent both navigate to `"categories"` and render
  a dedicated `CategoriesView`, not a `table_html` chat bubble.
- `/budgets-insights`, the sidebar's Insights entry, and an `OPEN_INSIGHTS` chat intent all navigate
  to `"insights"` and render `Insights.svelte` in the outlet, not as a modal overlay.
- A normal chat reply (no route signal) leaves `route === "chat"`, rendering the existing
  `chatWindow`-backed conversation view unchanged.
- Navigating `categories -> chat -> insights` never remounts `App.svelte`, never touches
  `chatMessages`/`showAllMessages`/`chatWindow`, and never reloads the page.

## Scope

**In scope:** `frontend/src/lib/router.js` (+ test), `frontend/src/lib/CategoriesView.svelte` (new),
`frontend/src/lib/Insights.svelte` (modal -> outlet-view edit), `frontend/src/App.svelte` (state +
wiring), 6 locale JSON files (new keys only).

**Out of scope:** Settings/`showSettings` (unchanged), any backend/Rust change (the existing
`categories-table`/`/chat` endpoints already provide everything needed), nested/parameterized
routes, deep-linking beyond the 3 flat hash routes, removing/renaming any existing i18n key still
in use elsewhere.

## Architecture

- **`frontend/src/lib/router.js`** (new, pure, mirrors `chatWindow.js`/`commands.js`):
  `ROUTES = ["chat", "categories", "insights"]`; `routeFromHash(hash)` maps `"#/categories"` ->
  `"categories"`, `"#/insights"` -> `"insights"`, anything else (empty, `"#/"`, unrecognized) ->
  `"chat"`; `hashForRoute(route)` is the inverse (`"chat"` -> `""`, others -> `"#/<route>"`).
- **`App.svelte`**: a new `route = $state("chat")` sits alongside the existing `activeScreen`. A
  `navigate(name)` function sets `route` (falling back to `"chat"` for an unknown name) and syncs
  `window.location.hash` via `hashForRoute` (clearing the hash via `replaceState` for `"chat"`
  rather than leaving a stale `#`). A `<svelte:window onhashchange>` listener re-derives `route`
  from the live hash whenever `activeScreen === "chat"` (handles browser back/forward). `route` is
  also initialized from the current hash at both existing `activeScreen = "chat"` transition sites
  (auth success + restored-session paths), so a hard refresh on `#/insights` deep-links correctly.
  Every existing `showInsights = true`/`false` call site becomes `navigate("insights")`/
  `navigate("chat")`; the `showInsights` state variable and its `inert`/focus-guard reads are
  removed. `#chat-scroll-area`'s children become a 3-way `{#if route === "categories"}
  <CategoriesView .../>{:else if route === "insights"}<Insights .../>{:else}` (existing chat
  markup) `{/if}`.
- **`frontend/src/lib/CategoriesView.svelte`** (new): props `{ fetchApi, activeBudget, onBack }`.
  On mount, fetches `` `/budgets/${activeBudget.id}/categories-table` ``  (the same endpoint
  `/categories-list` already calls) and renders `{@html res.html}` inside a plain (non-modal)
  container — preserving the existing "`table_html`/`res.html` is backend-built markup only, never
  model free-text, so `{@html}` is safe" invariant (`App.svelte` ~L2067-2071). Loading/error/empty
  states mirror `Insights.svelte`'s existing pattern. A "back to chat" affordance calls `onBack`.
- **`frontend/src/lib/Insights.svelte`** (edit): drop the `open` prop and its root `{#if open}`
  gate (mount/unmount, owned by the caller, is the gate now); replace the `modal modal-open` /
  `modal-box` / `modal-backdrop` wrapper with a plain view container (drop `role="dialog"`,
  `aria-modal`, the backdrop button, and the `max-h-[90dvh] overflow-y-auto` sizing since the outer
  `#chat-scroll-area` already scrolls); `$effect` now runs `load()` unconditionally at mount instead
  of gated on `open`. Internal period-selector/KPI/trend/recommendations markup is untouched.
- **`App.svelte` command/response handlers**: `/categories-list` (only when `activeBudget` is set)
  pushes a short chat acknowledgment (new key `commands.categoriesOpened`) and calls
  `navigate("categories")` instead of fetching + `pushAiTable`; the `chatRes.categories_table_html`
  truthy check (chat-detected `LIST_CATEGORIES`) calls `navigate("categories")` instead of
  attaching `table_html` to the pushed AI message (the message still gets `chatRes.response` as its
  text, just no `table_html` field); `chatRes.open_insights` and `/budgets-insights` both call
  `navigate("insights")` instead of `showInsights = true`. `pushAiTable` and the
  `{#if msg.table_html}` chat-bubble render branch are deleted (no longer reachable).

## Error Handling & Edge Cases

- Unknown/garbage hash on load or via `hashchange` -> falls back to `"chat"` (never throws, never
  shows a blank outlet).
- `/categories-list` with no `activeBudget` -> unchanged existing behavior: push
  `commands.noActiveBudget` and do **not** navigate (nothing meaningful to show).
- `CategoriesView` fetch failure -> inline error state (mirrors `Insights.svelte`'s
  loading/error/empty pattern), not a thrown/unhandled rejection.
- `CategoriesView` with a budget that has zero categories -> reuse existing `commands.categoriesNone`
  copy (interpolating the budget name) as the empty state, matching current chat-bubble wording.
- Navigating while `isChatLoading` is true (a request in flight) -> unaffected; `route` is
  orthogonal to `chatMessages`/`isChatLoading`, so an in-flight send still completes and its
  `open_insights`/`categories_table_html` signal still navigates on arrival.
- Auth screen (`activeScreen === "auth"`) -> the outlet/hash listener are inert; `route` state
  exists but has no rendering effect until `activeScreen === "chat"`.

## Testing Approach

- **Unit (Vitest, `environment: "node"`)**: `frontend/src/lib/router.test.js` — `routeFromHash`
  (each valid route, empty/`"#/"`/garbage -> `"chat"`, case sensitivity), `hashForRoute` (inverse,
  round-trip with `routeFromHash`).
- **Manual** (`cd frontend && pnpm run dev`): `/categories-list` and a categories-triggering chat
  message both land in the outlet (not a chat bubble); `/budgets-insights`, the sidebar Insights
  entry, and an insights-triggering chat message all land in the outlet (not a modal); a plain
  chat question renders the normal conversation view; `categories -> chat -> insights` preserves
  scroll-back chat history and does not reload; browser back/forward and a hard refresh on
  `#/insights` behave correctly; Escape returns to chat from either view.
- `cd frontend && pnpm test && pnpm run build` before opening the PR (existing repo convention, no
  dedicated lint script in this package).

## Risks & Open Questions

- Nesting `Insights`/`CategoriesView` inside `#chat-scroll-area` (which already has
  `overflow-y-auto`) means both components must NOT impose their own independent scroll container
  (double-scroll) — `Insights.svelte`'s `max-h-[90dvh] overflow-y-auto` is deliberately dropped for
  this reason (see Architecture).
- The chat acknowledgment text pushed on `/categories-list`/insights navigation (existing
  `commands.insightsOpened`, new `commands.categoriesOpened`) is a UX judgment call carried over
  from current behavior — kept for continuity between the chat thread and the outlet, not
  explicitly required by the AC, easy to drop later if reviewers disagree.
- "Header/sidebar buttons" for Insights per the ticket text: only a **sidebar** entry exists today
  (`Sidebar.svelte`'s `onOpenInsights`) — a prior, unrelated ticket (#232, already merged) removed
  the header's own Insights button. This spec routes the sidebar entry point only; there is no
  header button to update.
