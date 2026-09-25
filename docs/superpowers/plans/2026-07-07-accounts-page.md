# Dedicated Accounts Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a dedicated, sidebar-linked "Accounts" page hosting the existing `LinkedAccounts.svelte`
CRUD UI (removed from Settings), and make the Pro checkout price selectable per country (US-only wired
today, extensible without re-architecting checkout).

**Architecture:** Extend the existing flat `ROUTES` array (`frontend/src/lib/router.js`, #233) with an
`"accounts"` entry; add a thin `AccountsView.svelte` wrapper that mounts the untouched
`LinkedAccounts.svelte` component (relocation, not a rewrite); wire a new `MainMenu.svelte` sidebar
row; strip the panel out of `Settings.svelte`. On the backend, replace `billing.rs::checkout()`'s
inline cadence-only `match` with a small country-keyed lookup table (`resolve_price_env_var`),
mirroring the existing `bank_provider::Provider::for_country` dispatch pattern (#320) — only `"US"` is
configured, every other/absent country falls back to it, so existing callers are unaffected.

**Tech Stack:** Rust (Axum, serde), Svelte 5 (runes), svelte-i18n, Vitest (`environment: "node"`,
pure-module tests only — no jsdom/testing-library in this repo). Full spec:
`docs/superpowers/specs/2026-07-07-accounts-page-design.md`.

**Prerequisite:** Working in the worktree `/home/robhicks/dev/nels/.worktrees/issue-338-accounts-page`
on branch `issue-338-accounts-page` (already created, based on `origin/main`).

---

## File Structure

- Modify: `backend/src/billing.rs` — `CheckoutRequest.country`, `resolve_price_env_var`, `checkout()`.
- Modify: `docs/billing/stripe-setup.md` — $3/mo US pricing note + per-country extension doc.
- Modify: `frontend/src/lib/router.js` — add `"accounts"` to `ROUTES`.
- Modify: `frontend/src/lib/router.test.js` — cover the new route.
- Modify: `frontend/src/lib/i18n/locales/en.json` — new `accounts.*` section.
- Create: `frontend/src/lib/AccountsView.svelte` — new routed page hosting `LinkedAccounts.svelte`.
- Modify: `frontend/src/App.svelte` — import `AccountsView`, add outlet branch, drop dead `Settings`
  prop.
- Modify: `frontend/src/lib/MainMenu.svelte` — new "Accounts" sidebar row.
- Modify: `frontend/src/lib/Settings.svelte` — remove the Linked-accounts panel + dead prop/import.

---

## Task 1: Backend — country-keyed price lookup

**Files:**
- Modify: `backend/src/billing.rs:340-389` (struct + `checkout()`), `backend/src/billing.rs:510-534`
  (test module, insert new tests after the `use super::*;` line and before
  `fn sign(secret: &str, ...)`)

- [ ] **Step 1: Write the failing tests**

Open `backend/src/billing.rs`. In the `#[cfg(test)] mod tests` block, immediately after `use
super::*;` (around line 512) and before `fn sign(...)`, insert:

```rust
    #[test]
    fn resolve_price_env_var_monthly_us() {
        assert_eq!(resolve_price_env_var("monthly", Some("US")), Some("STRIPE_PRICE_MONTHLY"));
    }

    #[test]
    fn resolve_price_env_var_annual_us() {
        assert_eq!(resolve_price_env_var("annual", Some("US")), Some("STRIPE_PRICE_ANNUAL"));
    }

    #[test]
    fn resolve_price_env_var_defaults_to_us_when_country_absent() {
        assert_eq!(resolve_price_env_var("monthly", None), Some("STRIPE_PRICE_MONTHLY"));
    }

    #[test]
    fn resolve_price_env_var_falls_back_to_us_for_unknown_country() {
        assert_eq!(resolve_price_env_var("monthly", Some("XX")), Some("STRIPE_PRICE_MONTHLY"));
        assert_eq!(resolve_price_env_var("monthly", Some("")), Some("STRIPE_PRICE_MONTHLY"));
    }

    #[test]
    fn resolve_price_env_var_country_is_case_insensitive() {
        assert_eq!(resolve_price_env_var("monthly", Some("us")), Some("STRIPE_PRICE_MONTHLY"));
    }

    #[test]
    fn resolve_price_env_var_unknown_cadence_is_none() {
        assert_eq!(resolve_price_env_var("bogus", Some("US")), None);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test billing::tests::resolve_price_env_var`
Expected: FAIL — compile error, `resolve_price_env_var` is not defined.

- [ ] **Step 3: Implement `resolve_price_env_var` and wire it into `checkout()`**

Replace the existing (around line 340-389):

```rust
#[derive(Deserialize)]
pub struct CheckoutRequest { pub cadence: String } // "monthly" | "annual"
#[derive(Serialize)]
pub struct UrlResponse { pub url: String }

pub async fn checkout(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
    Json(req): Json<CheckoutRequest>,
) -> Result<Json<UrlResponse>, (StatusCode, String)> {
    let price = match req.cadence.as_str() {
        "monthly" => env_opt("STRIPE_PRICE_MONTHLY"),
        "annual" => env_opt("STRIPE_PRICE_ANNUAL"),
        _ => return Err((StatusCode::BAD_REQUEST, "cadence must be monthly or annual".to_string())),
    }.ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
```

with:

```rust
#[derive(Deserialize)]
pub struct CheckoutRequest {
    pub cadence: String, // "monthly" | "annual"
    /// ISO 3166-1 alpha-2 country code, optional (#338). Only "US" is
    /// configured today; any other/absent value falls back to the US price
    /// via `resolve_price_env_var` — see its doc comment.
    #[serde(default)]
    pub country: Option<String>,
}
#[derive(Serialize)]
pub struct UrlResponse { pub url: String }

/// (cadence, country) -> the Stripe price env var to read (#338). Mirrors
/// `bank_provider::Provider::for_country`'s single `match` dispatch on an
/// uppercased country code (#320) — not a table/find lookup (see the
/// "post-implementation-review" note below). Only "US" is configured; any
/// other/absent/unrecognized country falls back to the US row today, keeping
/// every existing (country-less) caller's behavior byte-identical. Adding a
/// second country later is two new match arms (one per cadence) plus one new
/// env var — no other checkout code changes. `None` means an unrecognized
/// `cadence` (unrelated to country) — the same 400 the old inline match
/// produced.
fn resolve_price_env_var(cadence: &str, country: Option<&str>) -> Option<&'static str> {
    let country = country.map(str::to_uppercase).unwrap_or_else(|| "US".to_string());
    match (country.as_str(), cadence) {
        ("US", "monthly") => Some("STRIPE_PRICE_MONTHLY"),
        ("US", "annual") => Some("STRIPE_PRICE_ANNUAL"),
        // Unrecognized/unsupported country: no country-specific price is
        // configured, so fall back to the one price that IS configured (US)
        // rather than failing the checkout outright.
        (_, "monthly") => Some("STRIPE_PRICE_MONTHLY"),
        (_, "annual") => Some("STRIPE_PRICE_ANNUAL"),
        _ => None, // unrecognized cadence
    }
}

pub async fn checkout(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
    Json(req): Json<CheckoutRequest>,
) -> Result<Json<UrlResponse>, (StatusCode, String)> {
    let price_env_var = resolve_price_env_var(&req.cadence, req.country.as_deref())
        .ok_or((StatusCode::BAD_REQUEST, "cadence must be monthly or annual".to_string()))?;
    let price = env_opt(price_env_var)
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
```

Leave every line after this point in `checkout()` (the existing-subscription guard, `ensure_customer`,
the Checkout Session `form` build, etc.) exactly as-is — only the price-resolution block above changes.

> **Post-implementation-review note:** this step originally specified a `const TABLE` scanned with
> `.find()`/`.any()` for `resolve_price_env_var`. Task 1's code-quality review found this didn't
> actually match the doc comment's claim of mirroring `Provider::for_country` (a single `match`, not
> a table/find) and double-scanned + allocated unconditionally on every call; it was replaced with
> the `match`-based version shown above (commit `98b835b`), which is what actually shipped and is
> reflected in the snippet above.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test billing::`
Expected: PASS — all `billing::tests::*` tests green, including the 6 new ones and every pre-existing
test in the file (`user_is_pro_only_trialing_and_active`, `signature_valid`, `maps_subscription_*`,
etc. — none of those are touched by this task, they must still pass unmodified).

- [ ] **Step 5: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(#338): select Stripe checkout price per country (US-only today)"
```

---

## Task 2: Backend docs — pricing + per-country extension guide

**Files:**
- Modify: `docs/billing/stripe-setup.md:9-16`

- [ ] **Step 1: Update the pricing summary callout**

Replace (lines 9-11):

```markdown
> Pricing/trial are the source-of-truth values in `marketing/src/lib/site.js`:
> **$4/mo, $36/yr, 7-day trial** (the issue body's `$5/$50/14-day` predates PR #218).
> Price *amounts* live in Stripe; the code only references price IDs + `STRIPE_TRIAL_DAYS`.
```

with:

```markdown
> Pricing/trial are the source-of-truth values in `marketing/src/lib/site.js`:
> **$3/mo (US), $36/yr, 7-day trial** (the issue body's `$5/$50/14-day` predates PR #218; the
> monthly price changed from $4 to $3 per #338 — `marketing/src/lib/site.js` itself is updated by
> #339, not here).
> Price *amounts* live in Stripe; the code only references price IDs + `STRIPE_TRIAL_DAYS`.
```

- [ ] **Step 2: Update "Product & Prices" and add the per-country extension subsection**

Replace (lines 13-16):

```markdown
## 1. Product & Prices
- Create a Product **"Nels Pro"** with two recurring Prices:
  - **$4 / month** → record its price ID for `STRIPE_PRICE_MONTHLY`.
  - **$36 / year** → record its price ID for `STRIPE_PRICE_ANNUAL`.
```

with:

```markdown
## 1. Product & Prices
- Create a Product **"Nels Pro"** with two recurring Prices:
  - **$3 / month (US)** → record its price ID for `STRIPE_PRICE_MONTHLY`. (#338: this is a manual
    Stripe-dashboard step — code alone cannot change what a live subscription charges. If
    `STRIPE_PRICE_MONTHLY` currently points at the old $4/mo Price, create the new $3/mo Price and
    `fly secrets set STRIPE_PRICE_MONTHLY=price_... -a nels-api` to switch over.)
  - **$36 / year** → record its price ID for `STRIPE_PRICE_ANNUAL` (unchanged by #338).

### Per-country pricing (#338)
`billing::resolve_price_env_var(cadence, country)` resolves which env var to read for a given
`(cadence, country)` pair — a `match (country, cadence)`, not a table (see Task 1's
post-implementation-review note above). Only `"US"` is configured today — every other/absent country
falls back to the US price. Adding a second country's price is:
1. Create the new recurring Price in Stripe, record its price ID.
2. Add two new match arms to `resolve_price_env_var` (`backend/src/billing.rs`) — one per cadence,
   e.g. `("GB", "monthly") => Some("STRIPE_PRICE_MONTHLY_GB")`, `("GB", "annual") => Some(...)`,
   placed ABOVE the existing `(_, "monthly")`/`(_, "annual")` catch-all arms (Rust `match` takes the
   first arm that matches — a new country-specific arm placed after the wildcard would silently
   never be reached).
3. `fly secrets set STRIPE_PRICE_MONTHLY_GB=price_... -a nels-api`.

No other checkout code changes — `checkout()` itself is unaware of how many countries are configured.
```

- [ ] **Step 3: Commit**

```bash
git add docs/billing/stripe-setup.md
git commit -m "docs(#338): document \$3/mo US price and per-country checkout extension"
```

---

## Task 3: Frontend router — add the "accounts" route

**Files:**
- Modify: `frontend/src/lib/router.js:15`
- Modify: `frontend/src/lib/router.test.js:11-21`

- [ ] **Step 1: Write the failing tests**

In `frontend/src/lib/router.test.js`, replace the `describe("ROUTES", ...)` block (lines 11-21):

```javascript
describe("ROUTES", () => {
  it("lists the six flat routes", () => {
    expect(ROUTES).toEqual([
      "chat",
      "categories",
      "insights",
      "history",
      "settings",
      "deleteAccount",
    ]);
  });
});
```

with:

```javascript
describe("ROUTES", () => {
  it("lists the seven flat routes", () => {
    expect(ROUTES).toEqual([
      "chat",
      "categories",
      "insights",
      "history",
      "accounts",
      "settings",
      "deleteAccount",
    ]);
  });
});
```

Then, in the `describe("routeFromHash", ...)` block, immediately after the existing `it("maps
#/history to history", ...)` test, add:

```javascript
  it("maps #/accounts to accounts", () => {
    expect(routeFromHash("#/accounts")).toBe("accounts");
  });
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd frontend && pnpm test -- router.test.js`
Expected: FAIL — `ROUTES` doesn't include `"accounts"` yet, and `routeFromHash("#/accounts")` returns
`"chat"` (the unrecognized-route fallback), not `"accounts"`.

- [ ] **Step 3: Implement**

In `frontend/src/lib/router.js`, replace line 15:

```javascript
export const ROUTES = ["chat", "categories", "insights", "history", "settings", "deleteAccount"];
```

with:

```javascript
export const ROUTES = ["chat", "categories", "insights", "history", "accounts", "settings", "deleteAccount"];
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd frontend && pnpm test -- router.test.js`
Expected: PASS — all tests in `router.test.js` green, including the two new ones and every
pre-existing test (`parseBudgetsPath`, `hashForBudgetDetails`, `resolveRoute`, etc. — untouched).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/router.js frontend/src/lib/router.test.js
git commit -m "feat(#338): add accounts route"
```

---

## Task 4: Frontend i18n — Accounts page copy

**Files:**
- Modify: `frontend/src/lib/i18n/locales/en.json:308-321`

- [ ] **Step 1: Add the new `accounts` section**

In `frontend/src/lib/i18n/locales/en.json`, the `"notifications"` section currently ends at line 320
with `},` immediately followed by `"linkedAccounts": {` at line 321. Insert a new top-level `"accounts"`
section between them — i.e. change:

```json
    "justNow": "now"
  },
  "linkedAccounts": {
```

to:

```json
    "justNow": "now"
  },
  "accounts": {
    "title": "Accounts",
    "back": "Back",
    "openLabel": "Accounts"
  },
  "linkedAccounts": {
```

(This does not touch any other key in the file — `menu`/`history` stay English-only, matching the
existing precedent documented in the spec's Assumptions; no other locale file is touched.)

- [ ] **Step 2: Verify the JSON is still valid**

Run: `cd frontend && node -e "JSON.parse(require('fs').readFileSync('src/lib/i18n/locales/en.json', 'utf8')); console.log('valid')"`
Expected: prints `valid` with no error.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/lib/i18n/locales/en.json
git commit -m "feat(#338): add accounts page i18n copy"
```

---

## Task 5: `AccountsView.svelte` — new routed page

**Files:**
- Create: `frontend/src/lib/AccountsView.svelte`

- [ ] **Step 1: Write the component**

Create `frontend/src/lib/AccountsView.svelte`:

```svelte
<script>
  // Router-outlet view for the dedicated Accounts page (#338) — hosts the
  // existing LinkedAccounts.svelte CRUD UI, previously embedded in
  // Settings.svelte (see Settings.svelte's git history / #303). LinkedAccounts
  // itself is unmodified: its isPro-gated list/link-picker/upgrade-CTA
  // behavior, pro-gate error handling, and chat-reachability are unaffected
  // by this relocation.
  import { _ } from "svelte-i18n";
  import { ArrowLeft } from "lucide-svelte";
  import LinkedAccounts from "./LinkedAccounts.svelte";

  let { fetchApi, activeBudget, subscription, onUpgrade, onBack } = $props();

  let backButtonEl = $state(null);

  // Mounting this route IS the "opened" signal (matches Settings.svelte's/
  // CategoriesView.svelte's convention) — move focus to the back button for
  // keyboard/a11y users.
  $effect(() => {
    backButtonEl?.focus();
  });
</script>

<div class="flex flex-col h-full">
  <div class="flex items-center justify-between gap-2 mb-3">
    <h3 class="font-bold text-lg">{$_("accounts.title")}</h3>
    <button type="button" bind:this={backButtonEl} class="btn btn-sm btn-ghost gap-1.5" onclick={() => onBack?.()}>
      <ArrowLeft class="w-4 h-4" /> {$_("accounts.back")}
    </button>
  </div>

  <div class="flex-grow overflow-y-auto">
    {#if !activeBudget}
      <p class="text-sm text-base-content/50 py-4">
        {$_("commands.noActiveBudget")}
      </p>
    {:else}
      <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3">
        <LinkedAccounts
          budgetId={activeBudget.id}
          isPro={subscription?.is_pro}
          {onUpgrade}
          {fetchApi}
        />
      </div>
    {/if}
  </div>
</div>
```

- [ ] **Step 2: Verify the project still builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (this new component isn't imported/mounted anywhere yet, so this just
confirms it's valid Svelte with no syntax errors — Task 6 wires it in).

- [ ] **Step 3: Commit**

```bash
git add frontend/src/lib/AccountsView.svelte
git commit -m "feat(#338): add AccountsView page component"
```

---

## Task 6: Wire `AccountsView` into `App.svelte`

**Files:**
- Modify: `frontend/src/App.svelte:19` (import), `frontend/src/App.svelte:2523-2532` (outlet branch +
  drop dead `Settings` prop)

- [ ] **Step 1: Add the import**

In `frontend/src/App.svelte`, immediately after the existing:

```javascript
  import Settings from "./lib/Settings.svelte";
```

add:

```javascript
  import AccountsView from "./lib/AccountsView.svelte";
```

- [ ] **Step 2: Add the outlet branch and drop the dead `Settings` prop**

Replace (around line 2523-2532):

```svelte
          {:else if route === "settings"}
            <Settings
              {fetchApi}
              budgetId={activeBudget?.id}
              {subscription}
              {billingFinalizing}
              onUpgrade={startCheckout}
              onManage={openPortal}
              onBack={() => navigate("chat")}
            />
```

with:

```svelte
          {:else if route === "accounts"}
            <AccountsView
              {fetchApi}
              {activeBudget}
              {subscription}
              onUpgrade={startCheckout}
              onBack={() => navigate("chat")}
            />
          {:else if route === "settings"}
            <Settings
              {fetchApi}
              {subscription}
              {billingFinalizing}
              onUpgrade={startCheckout}
              onManage={openPortal}
              onBack={() => navigate("chat")}
            />
```

(`budgetId` is removed from the `<Settings>` invocation here — it becomes dead once Task 8 removes
`Settings.svelte`'s own use of it. Removing it now vs. in Task 8 makes no functional difference since
Svelte silently ignores an extra prop a component doesn't declare, but doing it in this same edit keeps
the diff for this call site in one place.)

- [ ] **Step 3: Verify the project builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "feat(#338): route to AccountsView from the accounts outlet"
```

---

## Task 7: `MainMenu.svelte` — sidebar "Accounts" row

**Files:**
- Modify: `frontend/src/lib/MainMenu.svelte:1-15` (icon import), `frontend/src/lib/MainMenu.svelte:140-149` (new row)

- [ ] **Step 1: Add the `Landmark` icon import**

Replace the existing lucide-svelte import block (lines 1-15):

```javascript
  import {
    History as HistoryIcon,
    Tags,
    Wallet,
    BarChart3,
    Copy,
    Check,
    AlertTriangle,
    Download,
    Settings as SettingsIcon,
    LogOut,
    Trash2,
    RefreshCw,
  } from "lucide-svelte";
```

with:

```javascript
  import {
    History as HistoryIcon,
    Tags,
    Wallet,
    BarChart3,
    Landmark,
    Copy,
    Check,
    AlertTriangle,
    Download,
    Settings as SettingsIcon,
    LogOut,
    Trash2,
    RefreshCw,
  } from "lucide-svelte";
```

- [ ] **Step 2: Add the new row**

Replace (lines 140-149):

```svelte
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("insights")}
        >
          <BarChart3 class="w-4 h-4 shrink-0" /> <span>{$_("insights.openLabel")}</span>
        </button>
      </li>
      <li><hr class="border-base-300 mx-0" /></li>
```

with:

```svelte
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("insights")}
        >
          <BarChart3 class="w-4 h-4 shrink-0" /> <span>{$_("insights.openLabel")}</span>
        </button>
      </li>
      <li>
        <button
          type="button"
          class="flex items-center gap-3 px-4 min-h-[44px] w-full text-left text-sm hover:bg-base-100 rounded-none"
          onclick={() => navigateAndClose("accounts")}
        >
          <Landmark class="w-4 h-4 shrink-0" /> <span>{$_("accounts.openLabel")}</span>
        </button>
      </li>
      <li><hr class="border-base-300 mx-0" /></li>
```

- [ ] **Step 3: Verify the project builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/lib/MainMenu.svelte
git commit -m "feat(#338): add Accounts row to the sidebar menu"
```

---

## Task 8: `Settings.svelte` — remove the Linked-accounts panel

**Files:**
- Modify: `frontend/src/lib/Settings.svelte:12` (import), `:14-22` (props), `:221-231` (panel block)

- [ ] **Step 1: Remove the `LinkedAccounts` import**

Delete this line (line 12):

```javascript
  import LinkedAccounts from "./LinkedAccounts.svelte";
```

- [ ] **Step 2: Remove the dead `budgetId` prop**

Replace (lines 14-22):

```javascript
  let {
    fetchApi,
    budgetId = null,
    subscription = null,
    billingFinalizing = false,
    onUpgrade,
    onManage,
    onBack,
  } = $props();
```

with:

```javascript
  let {
    fetchApi,
    subscription = null,
    billingFinalizing = false,
    onUpgrade,
    onManage,
    onBack,
  } = $props();
```

- [ ] **Step 3: Remove the Linked-accounts panel block**

Delete this block (lines 221-231, immediately after the Subscription/plan panel's closing `</div>`
and immediately before the "Token usage" panel's opening comment):

```svelte
      <!-- Linked bank accounts (#303, Stripe Financial Connections) -->
      {#if budgetId}
        <div class="bg-base-200 border border-base-300 rounded-xl p-4 space-y-3">
          <LinkedAccounts
            {budgetId}
            isPro={subscription?.is_pro}
            {onUpgrade}
            {fetchApi}
          />
        </div>
      {/if}

```

(Leave exactly one blank line between the Subscription panel's closing `</div>` and the "Token usage"
comment — matching the existing blank-line spacing between every other pair of panels in this file.)

- [ ] **Step 4: Verify the project builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds, no unused-import warnings for `LinkedAccounts`.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/Settings.svelte
git commit -m "feat(#338): remove linked accounts panel from Settings"
```

---

## Task 9: Full verification sweep

**Files:** none (verification only)

- [ ] **Step 1: Run the full backend test suite**

Run: `cd backend && cargo test`
Expected: PASS. (DB-backed `#[ignore]`d tests are not run by default and require a running
`podman-compose up -d` Postgres — this is pre-existing repo behavior, not something this task changes;
do not attempt to bring up the DB just for this sweep unless it's already running.)

- [ ] **Step 2: Run the full frontend test suite**

Run: `cd frontend && pnpm test`
Expected: PASS — every `*.test.js` file green, including `router.test.js`'s new/updated tests and
`linkedAccounts.test.js` (untouched, must still pass since `LinkedAccounts.svelte`'s logic wasn't
modified).

- [ ] **Step 3: Run the frontend build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds cleanly.

- [ ] **Step 4: Grep-verify no leftover references**

Run: `grep -rn "budgetId" frontend/src/lib/Settings.svelte; grep -n "LinkedAccounts" frontend/src/lib/Settings.svelte`
Expected: both commands print nothing (no output) — confirms Task 8's removal was complete.

- [ ] **Step 5: Manual smoke check (if a dev environment is available)**

Run: `cd frontend && pnpm run dev` (requires `podman-compose up -d` + `cd backend && cargo run` in
separate terminals per `AGENTS.md`'s Developer Commands). Log in, open the sidebar, click "Accounts" —
confirm it navigates to a page showing either the upgrade CTA (non-Pro test account) or the linked
accounts list/picker (Pro test account), and that Settings no longer shows a "Linked bank accounts"
panel. This step is best-effort — if no local Postgres/dev environment is available in the execution
context, skip it and rely on Steps 1-4 plus the PR's own review/CI cycle.

No commit for this task (verification only, no code changes).
