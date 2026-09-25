# Dedicated Accounts page for synced bank accounts (#338) — Design

## Brief (verbatim AC, condensed)

Add a dedicated "Accounts" page, linked from the sidebar, for viewing/connecting/refreshing/
disconnecting synced bank accounts — the account CRUD UI (`LinkedAccounts.svelte`) currently
embedded in Settings. The "Linked bank accounts" panel (`Settings.svelte:221-231`) is removed from
Settings; Settings keeps its subscription/plan panel and other sections. A non-upgraded user sees an
upgrade CTA on the Accounts page instead of the link UI (the same `isPro` gating `LinkedAccounts.svelte`
already has, just surfaced on its own page). An upgraded user sees the existing list/refresh/
disconnect/link-by-country UI, unchanged. For US users the upgrade costs $3/month; the price-lookup
mechanism changes so a price can be selected per the user's country, with only a US price configured
now — adding a second country's price later must not require re-architecting checkout. Existing
chat-reachability and `isProGateError` handling for linked accounts continue to work unchanged.

Builds on `backend/src/billing.rs` (#25) and the linked-accounts work (#303/#320/#321/#322/#323). Does
**not** introduce a new subscription tier — "upgrading for syncing" is the existing Pro entitlement,
whose US monthly price becomes $3/month. Per-country pricing beyond US is out of scope; this ticket
only ensures the price-selection path is extensible by country. #339 (marketing copy) and #340 (admin
visibility) are concurrent, non-overlapping follow-ups worked in separate worktrees.

## Assumptions

- **"Same gating behavior, just surfaced on its own page" means reuse `LinkedAccounts.svelte`
  unchanged, not build new page-level gating.** `LinkedAccounts.svelte` already renders its account
  list (`view`/`disconnect` are intentionally ungated per AGENTS.md §14 — "a lapsed user must still be
  able to manage their own linked data") plus either the country/link picker (`isPro`) or an upgrade
  button (`!isPro`) at the bottom. Replacing this with a page-level "CTA instead of list" for
  non-upgraded users would regress the documented lapsed-user view/disconnect capability and invent
  gating logic the ticket doesn't ask for (its parenthetical explicitly defers to the component's
  existing behavior). The new Accounts page hosts `<LinkedAccounts>` exactly as Settings did.
- **New route follows the existing flat-hash-route pattern** (`frontend/src/lib/router.js`, added by
  #233 and documented in `docs/superpowers/specs/2026-07-03-router-outlet-design.md`): `"accounts"` is
  added to the flat `ROUTES` array, not the separate hierarchical `budgets/` path-segment system (this
  is a single flat page with no sub-resource, like `settings`/`categories`/`insights`).
- **No DB schema change.** Country-aware price *selection* is a request-time lookup over existing env
  vars; nothing needs to be persisted (no user-country column exists or is added).
- **Price-lookup mechanism keeps the existing env var names (`STRIPE_PRICE_MONTHLY`/
  `STRIPE_PRICE_ANNUAL`) rather than renaming them to a country-suffixed form.** Renaming would require
  a coordinated production Fly-secret rename at deploy time (an out-of-band step this change cannot
  itself perform) for zero behavioral gain today — only one country is configured. Instead,
  `resolve_price_env_var(cadence, country)` becomes a small country-keyed table that currently has one
  row per cadence for `"US"` (pointing at the existing env vars) and falls back to the `"US"` row for
  any other/absent country — so **every existing caller (no `country` field sent) is unaffected**, and
  adding a second country later is one new table row + one new env var, per the AC's "must not require
  re-architecting checkout." This mirrors the existing `bank_provider::Provider::for_country` pattern
  (§15 in AGENTS.md) already established in this codebase for exactly this kind of country-keyed
  dispatch.
- **Actually creating a $3/month Stripe Price object and rotating the `STRIPE_PRICE_MONTHLY` Fly
  secret is an operational step outside this code change** — this agent has no live Stripe dashboard
  or Fly secrets access, and swapping a production billing price is a business action, not a code
  change. `docs/billing/stripe-setup.md` is updated to document the new target price and instruct the
  deploying operator explicitly; this is called out again in Scope and Risks below and is NOT
  something Phase 5 verification can close out — it is a documented follow-up for a human operator.
- **Frontend does not need to detect/send an actual `country` value for this ticket.** The
  Notes explicitly scope "per-country pricing beyond the US" out; sending a real geo-detected country
  would only ever resolve to the same US price today (nothing to observably differ), and building
  country detection now is speculative work for a follow-up ticket that doesn't exist yet.
  `CheckoutRequest.country` is added as `Option<String>` so the request shape is forward-compatible,
  and the frontend omits it — resolving to the `"US"` fallback path, identical to today's behavior.
- **i18n: new copy is added to `en.json` only**, matching the existing precedent that the `menu` and
  `history` top-level sections are *already* English-only across all 6 locale files (verified: `de`/
  `es`/`fr`/`it`/`pt` each lack both sections; `svelte-i18n`'s `fallbackLocale: "en"` renders them
  correctly in every locale regardless). The Accounts page's own strings (`linkedAccounts.*`) are
  already fully translated everywhere and are unchanged by this ticket.
- **The sidebar row's label uses `accounts.openLabel`, not a new `menu.accounts` key.**
  `MainMenu.svelte` has two competing label conventions today: the two oldest rows (History,
  Categories) read `menu.history`/`menu.categories`, but the three most recently added rows (Budgets,
  Insights, Settings) all read `<feature>.openLabel` (`budgetsList.openLabel`, `insights.openLabel`,
  `settings.openLabel`). Accounts follows the newer/majority `openLabel` convention — no `menu.*`
  addition, avoiding an unused key.
- **`Settings.svelte` has no `settings.back` key of its own** — its back button reuses `history.back`.
  Only `CategoriesView.svelte` has a namespaced `<feature>.back` key. `AccountsView.svelte` follows
  `CategoriesView.svelte`'s (not Settings') precedent by getting its own `accounts.back` key, since it
  is a new standalone routed page like Categories, not a reuse of the History back label.
- **Settings.svelte's now-unused `budgetId` prop is removed**, not left dangling — it existed solely to
  feed `LinkedAccounts`; grep-verified no other use in the file.
- **Sidebar entry placement**: "Accounts" is grouped with the other content/data nav rows (History,
  Categories, Budgets, Insights) rather than the account-management rows (Copy Passcode, Export Data,
  Settings) — inserted immediately after Insights, before the existing divider. This is a judgment
  call the ticket doesn't specify; grouping by "what kind of screen this is" (content view vs. account
  action) matches the existing divider's apparent intent.

## Goal & Success Criteria

Move bank-account management out of Settings onto its own sidebar-linked page, gated the same way it
already is, and make the Pro checkout price selectable per country (US-only wired today) without
touching the checkout architecture.

- `MainMenu.svelte` has an "Accounts" row that navigates to a new `"accounts"` route.
- `Settings.svelte` no longer renders `LinkedAccounts` or the now-dead `budgetId` prop; its other
  panels are untouched.
- The new Accounts page renders `LinkedAccounts.svelte` unmodified, with the same `isPro`/upgrade-CTA/
  pro-gate-error behavior it has today, for an active budget; a no-active-budget state is handled
  gracefully (existing `commands.noActiveBudget` copy) instead of rendering nothing.
- `POST /api/billing/checkout` accepts an optional `country`; price resolution is table-driven and
  keyed by `(cadence, country)`, defaulting to the existing US price for any unrecognized/absent
  country — existing callers (no `country` sent) see byte-identical behavior.
- `cd backend && cargo test` and `cd frontend && pnpm test && pnpm run build` all pass.

## Scope

**In scope:**
- `frontend/src/lib/router.js` (+ `router.test.js`) — add `"accounts"` to `ROUTES`.
- `frontend/src/lib/AccountsView.svelte` (new) — page wrapper hosting `LinkedAccounts.svelte`.
- `frontend/src/lib/MainMenu.svelte` — new sidebar row.
- `frontend/src/lib/Settings.svelte` — remove the Linked-accounts panel + dead `budgetId` prop/import.
- `frontend/src/App.svelte` — import `AccountsView`, add the `route === "accounts"` outlet branch,
  drop the now-unused `budgetId` prop passed to `<Settings>`.
- `frontend/src/lib/i18n/locales/en.json` — new `accounts.{title,back,openLabel}` section.
- `backend/src/billing.rs` — `CheckoutRequest.country`, `resolve_price_env_var(cadence, country)`,
  unit tests.
- `docs/billing/stripe-setup.md` — document the $3/mo US price target, the per-country extension
  point, and the manual Stripe/Fly steps required to realize it.

**Out of scope:** `LinkedAccounts.svelte` / `linkedAccounts.js` internals (unchanged — this is a pure
relocation), any real per-country price beyond US, live Stripe Price object creation or Fly secret
rotation, marketing copy (#339), admin visibility (#340), any DB migration.

## Architecture

- **`frontend/src/lib/router.js`**: `ROUTES` gains `"accounts"` (flat, same shape as `"settings"`/
  `"categories"`). `routeFromHash`/`hashForRoute` need no other changes — they already generalize over
  the array.
- **`frontend/src/lib/AccountsView.svelte`** (new): props `{ fetchApi, activeBudget, subscription,
  onUpgrade, onBack }`. Header (title + back button using the new `accounts.title`/`accounts.back`
  keys, mirroring `CategoriesView.svelte`'s namespaced-back-key pattern; also carries the mount-time
  back-button focus `$effect` `Settings.svelte` uses, since `AccountsView` is replacing part of what
  was previously reached via Settings). Sidebar label uses `accounts.openLabel` (see Assumptions).
  Body: `{#if !activeBudget}` renders `commands.noActiveBudget` (existing key, mirroring
  `CategoriesView.svelte`'s identical guard); otherwise mounts
  `<LinkedAccounts budgetId={activeBudget.id} isPro={subscription?.is_pro} {onUpgrade} {fetchApi} />`
  unchanged from how `Settings.svelte` mounted it.
- **`frontend/src/App.svelte`**: new `import AccountsView from "./lib/AccountsView.svelte";`; the
  route-outlet `{#if route === ...}` chain gains an `{:else if route === "accounts"}` branch (placed
  next to the `"settings"` branch) passing `{fetchApi} {activeBudget} {subscription}
  onUpgrade={startCheckout} onBack={() => navigate("chat")}` — reusing the exact `startCheckout`
  handler Settings already wires to `onUpgrade`, so `isProGateError`/upgrade flows are byte-identical
  to today. The `<Settings ...>` invocation drops its `budgetId={activeBudget?.id}` prop (dead).
- **`frontend/src/lib/MainMenu.svelte`**: new `<li>` row using the `Landmark` icon (already available
  in the installed `lucide-svelte@1.0.1`; distinct from the `Wallet` icon already used for Budgets),
  calling `navigateAndClose("accounts")`, inserted after the Insights row and before the existing
  `<hr>` divider (grouped with the other content/data nav rows — see Assumptions).
- **`frontend/src/lib/Settings.svelte`**: remove the `import LinkedAccounts …` line, the `budgetId =
  null` prop, and the `{#if budgetId}…{/if}` panel block (lines 221-231). No other panel changes.
- **`backend/src/billing.rs`**:
  - `CheckoutRequest` gains `pub country: Option<String>`.
  - New pure function. **Note (updated post-implementation-review):** the first-drafted version below
    used a `const TABLE` scanned with `.find()`/`.any()`; code-quality review on Task 1 found this
    didn't actually mirror `bank_provider::Provider::for_country`'s dispatch style as the doc comment
    claimed (`for_country` is a single `match` on an uppercased country code, not a table/find), and
    also double-scanned + allocated unconditionally. The shipped version below is what's actually in
    `backend/src/billing.rs` — a single `match (country, cadence)`, matching `for_country`'s real shape:
    ```rust
    /// (cadence, country) -> the Stripe price env var to read. Mirrors
    /// bank_provider::Provider::for_country's single `match` dispatch on an
    /// uppercased country code — not a table/find lookup. Only "US" is
    /// configured; any other/absent/unrecognized country falls back to the US
    /// row today, keeping every existing (country-less) caller's behavior
    /// byte-identical. Adding a second country later is two new match arms
    /// (one per cadence) plus one new env var — no other checkout code
    /// changes. IMPORTANT: new country-specific arms must be added ABOVE the
    /// `(_, "monthly")`/`(_, "annual")` catch-all arms below, or `match`'s
    /// first-arm-wins semantics will silently shadow them.
    fn resolve_price_env_var(cadence: &str, country: Option<&str>) -> Option<&'static str> {
        let country = country.map(str::to_uppercase).unwrap_or_else(|| "US".to_string());
        match (country.as_str(), cadence) {
            ("US", "monthly") => Some("STRIPE_PRICE_MONTHLY"),
            ("US", "annual") => Some("STRIPE_PRICE_ANNUAL"),
            (_, "monthly") => Some("STRIPE_PRICE_MONTHLY"), // unrecognized country -> US fallback
            (_, "annual") => Some("STRIPE_PRICE_ANNUAL"),
            _ => None, // unrecognized cadence — unchanged 400 behavior
        }
    }
    ```
  - `checkout()` replaces its inline `match req.cadence.as_str() { "monthly" => …, "annual" => …, _ =>
    return 400 }` with: resolve the env var via `resolve_price_env_var(&req.cadence,
    req.country.as_deref())` (400 on `None`, i.e. bad cadence — same status as today), then
    `env_opt(env_var)` (503 on unset, same as today).
- **`docs/billing/stripe-setup.md`**: update BOTH existing $4/mo mentions (the top summary callout
  around line 10 and the "## 1. Product & Prices" bullet around line 15) to $3/mo US — annual unchanged,
  this ticket only changes monthly per the AC — add a short "Adding a second country's price"
  subsection pointing at `resolve_price_env_var`, and an explicit manual-step callout: create the new
  $3/mo Price in the Stripe dashboard and `fly secrets set STRIPE_PRICE_MONTHLY=price_...` before this
  ships to production — code alone cannot realize the price change.

## Error Handling & Edge Cases

- `AccountsView` with no `activeBudget` (e.g. a brand-new user with zero budgets) -> existing
  `commands.noActiveBudget` message, no crash, no empty `<LinkedAccounts>` mount (mirrors
  `CategoriesView.svelte`).
- `resolve_price_env_var` with an unrecognized `cadence` (not `"monthly"`/`"annual"`) -> `None` ->
  `checkout()` keeps returning 400, same as today.
- `resolve_price_env_var` with an unrecognized/garbage `country` (e.g. `"XX"`, empty string, or
  omitted) -> falls back to the `"US"` row, so behavior for every existing (country-less) caller is
  unchanged, and a garbage country never 503s where a plain request would have succeeded.
- Case-insensitivity: `country: Some("us")` resolves the same as `Some("US")` (mirrors
  `bank_provider::Provider::for_country`'s existing case-insensitive convention for country codes).
- `isProGateError`/pro-gate error handling inside `LinkedAccounts.svelte` is untouched code — no new
  edge cases introduced by relocation.

## Testing Approach

- **Backend (Rust, `cargo test`)**: unit tests for `resolve_price_env_var` — `("monthly", Some("US"))`
  and `("annual", Some("US"))` resolve to the existing env var names; `("monthly", None)` defaults to
  the same US result; `("monthly", Some("xx"))`/`Some("")` fall back to US; `("monthly", Some("us"))`
  is case-insensitive; `("bogus", Some("US"))` -> `None`. A `checkout()`-level integration test is not
  added beyond what already exists (`stripe-mocked` tests are `#[ignore]`d DB-backed tests elsewhere in
  the file) — the resolver is pure and fully covered without needing the DB/HTTP mock harness.
- **Frontend (Vitest, `environment: "node"`)**: `router.test.js` — `ROUTES` includes `"accounts"`,
  `routeFromHash("#/accounts") === "accounts"`, round-trips through `hashForRoute`.
- **Manual** (`cd frontend && pnpm run dev`): sidebar "Accounts" row navigates to the new page for both
  a Pro and a non-Pro test account (upgrade CTA vs. list+picker); Settings no longer shows the linked
  accounts panel; refresh/disconnect/link-by-country still work identically from the new page;
  browser back/forward and a hard refresh on `#/accounts` resolve correctly (free via the existing
  `router.js` generalization, spot-checked); the chat-triggered `LINK_BANK_ACCOUNT` flow (a top-level
  modal in `App.svelte`, independent of the route/outlet system) still opens and completes normally —
  confirming the AC's "existing chat-reachability … continue to work unchanged from the new page" is
  actually a no-op by construction (the chat modal was never coupled to Settings' route in the first
  place).
- `cd backend && cargo test` and `cd frontend && pnpm test && pnpm run build` before opening the PR
  (repo convention; no dedicated frontend lint script).

## Risks & Open Questions

- The $3/month price is a business/billing-config change that this PR's code cannot fully realize —
  merging it does **not** change what a real checkout charges until an operator creates the new Stripe
  Price and rotates the `STRIPE_PRICE_MONTHLY` Fly secret (documented in `docs/billing/
  stripe-setup.md`, flagged again as a Phase 5 out-of-band item this agent cannot close out itself).
- Sidebar row placement (grouped with content views, not account-management rows) is a judgment call;
  easy to move in review if maintainers disagree.
- `marketing/src/lib/site.js` hardcodes `price: { monthly: '$4', annual: '$36' }` and is the
  documented source-of-truth for the marketing site's displayed price. This PR does **not** touch it
  (marketing copy is explicitly #339's scope, worked concurrently in a separate worktree) — so the
  marketing site will keep advertising $4/mo until #339 ships. This is a known, intentional gap
  between this PR and #339, not an oversight.
- i18n coverage follows the existing `menu`/`history` English-only precedent rather than translating
  into all 6 locales; if reviewers want full parity, it's a small, low-risk follow-up (3 short strings
  × 5 locales).
