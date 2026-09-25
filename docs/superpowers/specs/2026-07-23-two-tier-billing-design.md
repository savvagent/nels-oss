# Two-Tier Billing & Budget-Owner Entitlement — Design

**Date:** 2026-07-23
**Status:** Approved (brainstorming), pending implementation plan
**Related:** #25 (Stripe billing), #391 (`users.active_budget_id`), bank-linking providers (Plaid/GoCardless/Basiq/Belvo/Akahu/Financial Connections)

## Problem

Nels ships a single-plan Stripe integration that is broken in both directions:

- **Acquisition:** 7 of 9 marketing CTAs carry no cadence and lead to no purchase; the 2 that do start a **no-card** trial whose Stripe default end-behavior invoices at day 7, producing an involuntary `past_due` lapse rather than a clean expiry.
- **Recovery:** once lapsed, every in-app CTA points at **Checkout** (never the Portal), so a lapsed user cannot fix a card or resubscribe from inside the app; the "Manage" button is hidden behind `is_pro`; the refresh path is a terminal red banner with no CTA; a `past_due` user can even open a **second** subscription (the duplicate guard only fires when already Pro).
- **Product/pricing mismatch:** the marketing site lists 7 "included" features but only 1 (bank sync) is enforced. The app is effectively free forever for anyone who never subscribes.

Only 2 production users exist (1 subscription row), so breaking changes need no data-migration machinery.

## Goals

1. Two paid tiers: **Basic** ($3/mo, $30/yr) = full app, no bank sync; **Pro** ($5/mo, $50/yr) = Basic + bank sync.
2. Entitlement is a property of a **budget**, derived from its **owner**. Viewers inherit the owner's tier; owning zero budgets costs nothing (pure viewers are free).
3. A 7-day, no-card, **Pro** trial that expires **clean to `canceled`** (never `past_due`).
4. Lapsed budgets go **read-only for everyone**; the owner sees a resubscribe path, viewers see an explanation.
5. `past_due` (a real renewal failure on a card-on-file customer) keeps **full access during Stripe's dunning grace**, with an in-app warning.
6. Every billing CTA routes correctly: **start** → Checkout, **change/fix** → Customer Portal.

## Non-Goals (YAGNI)

- Usage-based / metered billing.
- Per-seat pricing for shared editors.
- Custom in-app proration or plan-swap UI (use the Stripe Customer Portal).
- Custom dunning emails (rely on **Stripe's built-in dunning** for `past_due`).
- More than two tiers.

---

## Section 1 — Entitlement model & states

Replace the `billing::user_is_pro(status) -> bool` primitive with a small typed resolver in one module. This is the single source of truth; every gate calls it.

```rust
// Ordered so Basic < Pro; None is "no entitlement".
enum Tier { None, Basic, Pro }

struct Entitlement {
    tier: Tier,
    payment_warning: bool, // true during past_due grace → UI shows "update your card"
}

// Pure function of persisted Stripe fields (status + price_id). No DB, unit-testable.
fn resolve(status: Option<&str>, price_id: Option<&str>) -> Entitlement
```

### Status → entitled?

Three statuses form the **grace set** (keep entitlement); all others lapse to `Tier::None`.

| Stripe status | Entitled? | Tier | Notes |
|---|---|---|---|
| `trialing` | ✅ | Pro | 7-day no-card trial always grants Pro |
| `active` | ✅ | from `price_id` | paying, healthy |
| `past_due` | ✅ | from `price_id` | **grace**; `payment_warning = true` |
| `canceled`, `unpaid`, `incomplete_expired`, `paused`, `incomplete` | ❌ | None | read-only lapse |
| _no subscription row_ | ❌ | None | pure viewer, or pre-first-budget |

### The `past_due` grace decision

Because trials now expire clean to `canceled` (Section 4), `past_due` can **only** occur for a customer who already provided a card and paid at least once — i.e. a genuine renewal failure, not a trial artifact. Such users retain **full entitlement** for the duration of Stripe's Smart Retries window (~2–3 weeks; Stripe owns the timer — we run no scheduled job). The app shows a persistent "Payment failed — update your card" banner (Section 3). Only when Stripe gives up and the subscription reaches a terminal status (`canceled`/`unpaid`) does the budget go read-only. We read terminal status from the webhook we already persist.

### Tier from `price_id`

Map the four price env vars → tier:

| Env var | Tier |
|---|---|
| `STRIPE_PRICE_BASIC_MONTHLY`, `STRIPE_PRICE_BASIC_ANNUAL` | Basic |
| `STRIPE_PRICE_PRO_MONTHLY`, `STRIPE_PRICE_PRO_ANNUAL` | Pro |

**Fail-safe rule:** an entitled status (grace set) with an **unrecognized** `price_id` resolves to **`Tier::Basic`** and logs a `tracing::error!` alert. A paying customer is never locked out; an unknown price never silently grants Pro.

### Consumer helpers

Two thin helpers build on `resolve`, both resolving the **owner's** subscription:

- `budget_entitlement(pool, budget_id) -> Entitlement` — look up the budget's `owner_id`, load the owner's subscription row, `resolve(status, price_id)`. Used by the write-gate and the Pro-gate.
- Existing per-provider `require_pro` sites become `require_tier(pool, budget_id, Tier::Pro)`.

Background sync already resolves *owner* entitlement (`plaid.rs`, `gocardless.rs`, `belvo.rs`, etc. read `owner_status`); this unifies that ad-hoc logic onto `resolve`.

---

## Section 2 — Backend enforcement

Two gates, both keyed off the budget's **owner** entitlement, attached at layers that already exist.

### Gate 1 — Write gate (Basic OR Pro): "is this budget writable?"

The shared chokepoint today is `budget::check_permission(pool, user_id, budget_id) -> Permission` (`None/View/Edit/Owner`; already resolves `owner_id`). Every mutation requires `Edit`/`Owner`. Add one shared helper:

```rust
async fn require_writable_budget(pool, user_id, budget_id) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id).await?;   // existing
    if perm < Permission::Edit { return Err(403); }                 // existing rule
    let ent = budget_entitlement(pool, budget_id).await?;           // NEW
    if ent.tier == Tier::None {
        return Err((402, "This budget is read-only — the owner's subscription has lapsed."));
    }
    Ok(())
}
```

- **Reads are unaffected** — they still go through plain `check_permission`; a `View` (or Edit/Owner) on a lapsed budget still renders. This is the read-only promise.
- This helper **replaces the `require_edit_or_owner` copy-pasted into** `belvo/akahu/basiq/gocardless/plaid/financial_connections` — one implementation instead of seven (legitimate consolidation of code we're already touching).
- Net effect: owner/editor on a lapsed budget can look but not mutate; viewer is unaffected either way.

Write-guard call sites (~101 across 15 files) migrate from `require_edit_or_owner` to `require_writable_budget`. The migration is mechanical; the plan will sequence it.

### Gate 2 — Pro gate (Pro only): "can this budget use bank features?"

Existing `require_pro` sites become `require_tier(pool, budget_id, Tier::Pro)`, resolving the **owner's** tier, returning 402 on failure. Bank link/refresh/consent stay Pro-only; a Basic owner (or their editors) is routed to the upgrade path (Section 3).

### Trial trigger — first budget creation

- `auth.rs` (registration) **stops auto-creating** "My First Budget."
- The budget-create handler gains a pre-step that branches on the user's subscription state (a subscription row that has only a `stripe_customer_id` with `status IS NULL` counts as "never subscribed" — the `ensure_customer` placeholder is not a subscription):

  | User owns 0 budgets AND… | Action |
  |---|---|
  | **never subscribed** (no row, or row with `status IS NULL`) | start a **Pro trial subscription server-side**, then insert the budget |
  | **currently entitled** (grace set) | insert the budget normally |
  | **lapsed** (`canceled`/`unpaid`/… — trial already consumed) | **block creation**, route to Checkout/resubscribe (no second free trial) |

  (When the user already owns ≥1 budget, creation follows the normal write path — the owner is by definition already an entitled or lapsed owner, handled by Gate 1.)
- Trial creation uses the Stripe API — `trial_period_days: 7`, no card (`payment_method_collection: if_required`), `trial_settings.end_behavior.missing_payment_method = cancel`. No Checkout redirect for the $0 no-card trial; the `customer.subscription.created` webhook lands the `trialing` row.
- A pure viewer never creates a budget → never gets a subscription → free forever. The free-viewer guarantee is enforced by construction. The **one free trial per user** rule is enforced by the "lapsed → block" branch above.

**Edge cases:**

- **Idempotency:** two concurrent "create first budget" calls must not create two subscriptions. Guard on the existing unique `stripe_customer_id` row plus a "no existing subscription" check, mirroring `ensure_customer`'s race handling.
- **Stripe-down must not orphan an un-entitled owner:** if trial creation fails, **block budget creation with a clear retryable error** rather than creating an owner with no subscription (that would reopen the leak).

---

## Section 3 — CTA / UX paths

**One routing rule** replaces the "always Checkout" bug:

```
route_billing_action(entitlement, desired):
    live sub  (trialing | active | past_due)  → Customer Portal          (upgrade, downgrade, fix card, cancel)
    no live sub (canceled | unpaid | none)    → Checkout(tier, cadence)  (fresh start)
```

### Marketing (`Cta.svelte`)

- Pricing page shows **two tier cards** (Basic / Pro), each with monthly + annual.
- Paid-plan CTAs carry tier + cadence: `?plan=pro&cadence=annual`. The app reads these post-login only when the user explicitly chose a plan.
- The cadence-less CTAs (nav/hero/features/how-it-works/compare/security — "Start free trial") point at the app and resolve to the default experience: **create account → make first budget → auto Pro trial**. No up-front billing decision.

### In-app billing screen (`Settings.svelte`) — rebuilt from `Entitlement`

| State | Shows | Primary CTA → |
|---|---|---|
| `trialing` | "Pro trial · N days left" | "Add payment method" / "Choose plan" → Portal |
| `active` Basic | "Basic plan" | "Upgrade to Pro" → **Portal** (proration); "Manage" → Portal |
| `active` Pro | "Pro plan" | "Manage" / "Switch to annual" → Portal |
| `past_due` | ⚠️ "Payment failed — update your card" | "Update card" → Portal |
| `canceled` / no sub, owns budgets | "Subscription ended — budgets are read-only" | "Resubscribe" → **Checkout** (pick tier) |
| pure viewer, no budgets | "You're a free member" (no billing UI) | — |

The **Manage/Portal button is always present when a live sub exists** — fixing the bug where it was hidden behind `is_pro`.

### Lapsed read-only surfaces (402 from Gate 1)

- **Owner:** "This budget is read-only — resubscribe to make changes" → Resubscribe (Checkout).
- **Editor/viewer:** "The owner's subscription has lapsed — this budget is read-only." (no CTA; they can't fix someone else's billing.)

### `past_due` warning banner

App-wide, persistent, dismissible-per-session: "⚠️ Your payment failed — update your card to avoid losing access" → Portal. Full access continues during grace.

### Bank-gate surfaces (402 from Gate 2), for Basic owners

- Keep "Upgrade to Pro to link a bank account," but route through `route_billing_action` → **Portal upgrade** (they have a live Basic sub), not a fresh checkout.
- The refresh red banner ("Refreshing is a Nels Pro feature") gains the same **"Upgrade to Pro"** button — no more terminal dead-end.

### Chat (`rag.rs`)

Replace raw backend strings with structured, tier-aware guidance the frontend renders as an actionable card:

- Basic owner asks to link a bank → "Bank syncing is part of **Pro**. Upgrade for $5/mo?" + upgrade button.
- Lapsed owner tries a write action → "Your subscription ended — your budgets are read-only. Resubscribe to continue." + resubscribe button.

### Decision (approved)

Upgrades/downgrades between Basic and Pro route to the **Stripe Customer Portal** (native proration; no custom UI to build/test).

---

## Section 4 — Stripe config, migration, testing

### Stripe config

- Create **4 prices**: `STRIPE_PRICE_BASIC_MONTHLY` ($3), `STRIPE_PRICE_BASIC_ANNUAL` ($30), `STRIPE_PRICE_PRO_MONTHLY` ($5), `STRIPE_PRICE_PRO_ANNUAL` ($50). Set as Fly secrets on `nels-api`. Keep the old `STRIPE_PRICE_MONTHLY/ANNUAL` only until migration completes, then remove.
- **Customer Portal config** (dashboard): enable plan switching across all 4 prices with proration, payment-method update, and cancellation. Every "Manage/Upgrade/Fix card" CTA depends on this.
- **Trial end behavior:** set `trial_settings.end_behavior.missing_payment_method = cancel` on every trial subscription we create.
- The existing webhook already persists `status` + `price_id`; no new event types. `resolve()` reads what is already stored.

### Migration of the 2 existing production users

- **User A (a budget owner)**: subscription currently `past_due`. One-time fix: through the Portal or dashboard, add a card and move to a current price (e.g. Pro annual), or cancel and resubscribe cleanly. Their budgets become writable the moment `resolve()` sees `active`. No code migration is needed; the resolver handles them once the row is healthy.
- **User B (a household member)**: **DONE (2026-07-23).** Their auto-created starter budget (not a rollup parent, not linked, not their active budget) was deleted on production. They now own 0 budgets and are a viewer on User A's household budget only, so they are a pure viewer with free access and no subscription required.
- **No data backfill** — 2 rows, handled by hand. Code ships assuming `resolve()` is the single source of truth.

### Rollout ordering (avoid locking out mid-deploy)

1. Ship the resolver + read paths first (nothing enforced).
2. Seed the 4 prices; heal Rob's subscription row.
3. **Then** enable the write-gate and Pro-gate. Enforcement lands last, after entitlement data is correct.

### Testing

Per the project harness: backend `#[ignore]` integration tests vs local pgvector (podman, port 6153), `cargo test --bin backend`; frontend helper-TDD + `npm run build`, no Svelte component tests.

- **Resolver unit tests** (pure, no DB): every `(status, price_id)` → expected `Entitlement`. Grace set including `past_due` → `payment_warning`; unknown price → Basic + alert; each of the 4 prices → correct tier; every terminal status → `None`. (Expanded from the `user_is_pro` test table.)
- **Gate integration tests** (DB): lapsed owner write → 402; editor on lapsed budget → 402; viewer read on lapsed budget → 200; Basic owner bank-link → 402; Pro owner bank-link → pass; pure viewer never touches billing.
- **Trial-trigger integration:** first budget creation starts exactly one trial; concurrent creates don't double-subscribe; Stripe-down blocks budget create with a retryable error.
- **Frontend helper tests:** `route_billing_action(entitlement, desired)` → Portal vs Checkout for every state; tier/cadence CTA param builder.

## Open questions

None. All design decisions resolved during brainstorming.
