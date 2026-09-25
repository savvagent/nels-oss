# Stripe Subscription Billing — Setup Guide (#25)

The billing **code** is fully env-driven and ships with this repo. This guide covers the
**external Stripe account + Fly secret configuration** that the code cannot perform itself.
Until these are set, the **checkout/portal/webhook** endpoints return
`503 "Billing is not configured"` (the `GET /api/billing/subscription` endpoint still
returns a free/empty `200` — it reads no Stripe env), and the rest of the app runs unaffected.

> Pricing/trial are the source-of-truth values in `marketing/src/lib/site.js` — as of this writing
> that file still shows the pre-#338 **$4/mo, $36/yr, 7-day trial** (the issue body's `$5/$50/14-day`
> predates PR #218). The **target** US monthly price per #338 is **$3/mo** — `marketing/src/lib/
> site.js` itself is updated by the separate #339, not by this doc or this PR; until #339 merges,
> the marketing site and this setup guide's target price will legitimately disagree.
> Price *amounts* live in Stripe; the code only references price IDs + `STRIPE_TRIAL_DAYS`.

## 1. Product & Prices
- Create a Product **"Nels Pro"** with two recurring Prices:
  - **$3 / month (US)** → record its price ID for `STRIPE_PRICE_MONTHLY`. (#338: this is a manual
    Stripe-dashboard step — code alone cannot change what a live subscription charges. If
    `STRIPE_PRICE_MONTHLY` currently points at the old $4/mo Price, create the new $3/mo Price and
    `fly secrets set STRIPE_PRICE_MONTHLY=price_... -a nels-api` to switch over.)
  - **$36 / year** → record its price ID for `STRIPE_PRICE_ANNUAL` (unchanged by #338).

### Per-country pricing (#338)
`billing::resolve_price_env_var(cadence, country)` resolves which env var to read for a given
`(cadence, country)` pair. It's a single `match (country, cadence)` — mirroring
`bank_provider::Provider::for_country`'s dispatch style, not a table/lookup. Only `"US"` is
configured today; the catch-all arms (`(_, "monthly")` / `(_, "annual")`) fall back to the US price
for every other/absent/unrecognized country. Adding a second country's price is:
1. Create the new recurring Price in Stripe, record its price ID.
2. Add two new match arms to `resolve_price_env_var` (`backend/src/billing.rs`) — one per cadence,
   e.g. `("GB", "monthly") => Some("STRIPE_PRICE_MONTHLY_GB")`, `("GB", "annual") => Some(...)`.
   **They must be added ABOVE the existing `(_, "monthly")`/`(_, "annual")` catch-all arms** — Rust
   `match` takes the first arm that matches, so a new country-specific arm placed after the
   wildcard would silently never be reached (always shadowed by the US fallback).
3. `fly secrets set STRIPE_PRICE_MONTHLY_GB=price_... -a nels-api`.

No other checkout code changes — `checkout()` itself is unaware of how many countries are configured.

## 2. Customer Portal
- Enable the **Customer Portal** and allow: plan switch (monthly ↔ annual), cancel
  subscription, and update payment method. (Switching is configured in the Stripe
  dashboard, not in code — the code is portal-config-agnostic.)

## 3. API key (least privilege)
- Create a **restricted** API key with write/read on: Customers, Checkout Sessions,
  Billing Portal Sessions, Subscriptions. → `STRIPE_SECRET_KEY`.
- Use **test-mode** keys for dev/staging; only production uses live keys.

## 4. Webhook endpoint
- Add a webhook endpoint pointing at:
  `https://nels-api.fly.dev/api/billing/webhook`
- Subscribe it to exactly these events:
  - `checkout.session.completed`
  - `customer.subscription.created`
  - `customer.subscription.updated`
  - `customer.subscription.deleted`
  - `invoice.payment_failed`
- Copy the endpoint's **signing secret** → `STRIPE_WEBHOOK_SECRET`.

## 5. Provision secrets on Fly
```bash
fly secrets set \
  STRIPE_SECRET_KEY=sk_live_... \
  STRIPE_WEBHOOK_SECRET=whsec_... \
  STRIPE_PRICE_MONTHLY=price_... \
  STRIPE_PRICE_ANNUAL=price_... \
  APP_URL=https://app.nels.money \
  -a nels-api
```
`STRIPE_TRIAL_DAYS` defaults to 7; set it only to override. `STRIPE_API_BASE` must stay
unset in production (it defaults to `https://api.stripe.com`).

## 6. Local end-to-end test
```bash
# Forward webhooks to the local backend (port 3000):
stripe listen --forward-to localhost:3000/api/billing/webhook
# The CLI prints a whsec_... — export it as STRIPE_WEBHOOK_SECRET for the local run.
```
- Test cards: `4242 4242 4242 4242` (success), `4000 0000 0000 0341` (failed payment).
- Verify: start checkout (no card required for the trial) → `trialing` → `active`;
  toggle cancel-at-period-end in the Portal and confirm the `subscriptions` row updates;
  trigger a failed payment and confirm `past_due`.

## 7. Notes
- The 7-day trial requires **no card** (`payment_method_collection=if_required`). If product
  later wants a card up front, change that single field in the checkout-session builder
  (`billing.rs::checkout`) to `always`.
- Entitlement is enforced server-side via `billing::user_is_pro(status)` (true only for
  `trialing`/`active`). It derives solely from webhook-synced DB state.
- The webhook route is mounted outside the auth nest and rejects any unsigned/invalid
  payload with `400` — never disable signature verification.
- **Known operational gap:** webhook persistence is an UPDATE keyed on `stripe_customer_id`
  (the row is created at first checkout via `ensure_customer`). An event for a customer with
  no `subscriptions` row — e.g. a subscription created **manually in the Stripe dashboard**
  for a user — matches 0 rows, logs a `warn`, and returns 200 (no Stripe retry). To onboard
  such a subscription, first `INSERT` the user's `(user_id, stripe_customer_id)` row.
