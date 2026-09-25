# Nels Marketing Site + Subscriptions — Design

Date: 2026-06-11
Status: Draft for review

## Goal

Add an SEO-friendly marketing website at `nels.money` with a subscription CTA, and
give subscribers a place to manage their plan. Drive Free → Pro conversions for the
existing Nels budgeting app.

Content & SEO strategy (personas, positioning, keywords, page copy, metadata,
schema, content plan) lives in `docs/marketing/nels-seo-strategy.md` and is the
source of truth for all copy. This document covers the technical design.

## Decisions locked

- **Marketing stack**: SvelteKit 2 + Svelte 5 (runes) + Tailwind v4 + daisyUI v5,
  brand purple `#aa3bff`, light/dark themes mirroring the app.
- **Hosting**: Cloudflare Pages, fully prerendered static (`adapter-cloudflare`,
  `prerender = true`). New Pages project `nels-site`. $0.
- **Billing**: Stripe — Checkout (subscribe) + hosted Customer Portal (manage) +
  webhooks → entitlement in Postgres. Stripe account exists; Stripe MCP available
  to create products/prices and configure the portal.
- **Plan model**: **Single plan, no free tier.** 14-day **cardless** free trial on
  signup, then a paywall — **$5/mo or $50/yr** (annual ≈ 2 months free). Entitlement
  is **binary**: in-trial or active subscription → full access; trial expired with no
  active subscription → locked. No Free-vs-Pro feature split.
- **Clean slate**: no existing users, so no migration/grandfathering concerns.
- **Domains**: `nels.money` (marketing), `app.nels.money` (app), `nels-api.fly.dev`
  → optionally `api.nels.money` later.

## Resolved decisions

1. **Cardless trial** — yes. The 14-day trial is tracked locally (no Stripe object,
   no card) and Checkout is only used when the user actually subscribes.
2. **No Free/Pro split** — there is one plan; access is binary (see Plan model).
3. **Prices** — $5/mo and $50/yr (USD).

## Scope decomposition (two sub-projects)

### Sub-project A — Marketing site (ship first, standalone)

New top-level `marketing/` dir in the monorepo.

- **Routes** (from strategy): `/`, `/features`, `/how-it-works`, `/pricing`,
  `/security`, `/compare`, `/about`, `/faq`. Blog (`/blog`, `/blog/[slug]`) is
  scaffolded but content is Phase 3.
- **SEO infrastructure**:
  - Reusable `<Seo>` component → per-route `<title>`, meta description, canonical,
    Open Graph, Twitter card. Defaults in root layout, overridden per page.
  - JSON-LD components: `Organization` + `SoftwareApplication` site-wide;
    `Product`/`Offer` on `/pricing`; `FAQPage` on `/faq` (and homepage FAQ);
    `HowTo` on `/how-it-works`; `BreadcrumbList` site-wide.
  - `/sitemap.xml` and `/robots.txt` as prerendered `+server.js` endpoints.
  - Canonical host `https://nels.money`.
- **Branding**: define `nels-light` / `nels-dark` daisyUI themes mirroring the app;
  reuse the existing logo/mark + favicons from `frontend/public`.
- **CTA wiring**:
  - Phase A: "Start free trial" → `https://app.nels.money` signup (passwordless
    register flow).
  - Phase B: → Stripe Checkout session (sub-project B).
- **Deploy**: Cloudflare Pages project `nels-site`; GitHub Actions
  `deploy-marketing.yml` path-filtered to `marketing/**` (mirrors the existing
  frontend/backend workflows); custom domain apex `nels.money`.

### Sub-project B — Subscriptions (Stripe + backend)

Extends the existing Rust/Axum + Postgres backend.

- **Stripe objects** (created via Stripe MCP): one Product "Nels" with two recurring
  Prices ($5/mo, $50/yr); Customer Portal configured (allow cancel, switch
  monthly↔annual, update payment method, view invoices). Stripe's own trial is **not**
  used — the trial is tracked locally (below).
- **Data model** (new migration):
  - `users.trial_ends_at` (timestamptz) — set to `now() + interval '14 days'` at
    registration. This is the cardless trial; no Stripe object exists yet.
  - `subscriptions` table (created on first checkout): `user_id` (FK),
    `stripe_customer_id`, `stripe_subscription_id`, `status`, `plan` (monthly|annual),
    `current_period_end`, `cancel_at_period_end`, timestamps.
  - **Access (binary, derived)**: `now() < users.trial_ends_at` **OR** latest
    subscription `status ∈ {active, past_due}` (past_due = short grace while Stripe
    retries). Otherwise **locked**.
- **Backend endpoints** (Axum):
  - `POST /api/billing/checkout` (auth) → create Checkout Session (mode
    `subscription`, chosen price, 14-day trial, success/cancel → app), return URL.
  - `POST /api/billing/portal` (auth) → create Customer Portal session, return URL.
    Backs the app's "Manage subscription" link.
  - `POST /api/billing/webhook` (NO auth middleware; raw body) → verify Stripe
    signature, handle `checkout.session.completed`,
    `customer.subscription.created/updated/deleted`, `invoice.payment_failed`;
    upsert the `subscriptions` row idempotently (keyed by
    `stripe_subscription_id`).
  - Access state exposed via `/api/auth/me` (add `access` boolean, `trial_ends_at`,
    and subscription `status`/`plan`) so the app can show the paywall.
- **Access gate (binary)**: a check layered after the existing `auth_middleware` on
  the protected routes returns **402 Payment Required** when the user is locked
  (trial expired, no active subscription). Exempt endpoints that must stay reachable
  while locked: `/api/auth/me`, `/api/auth/logout`, and `/api/billing/*` (so the user
  can pay or manage their plan). The frontend treats 402 as "show paywall → Checkout."
  No per-feature gating — it's all-or-nothing.
- **Rust integration**: use the `async-stripe` crate (typed API + webhook signature
  verification via `Webhook::construct_event`), rather than hand-rolling REST on the
  existing `reqwest`.
- **Secrets** (Fly): `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`,
  `STRIPE_PRICE_MONTHLY`, `STRIPE_PRICE_ANNUAL`.

## Cross-cutting

- **Domain migration**: attach `app.nels.money` to the existing `nels` Pages
  project; update backend `CORS_ALLOWED_ORIGINS` to include `https://app.nels.money`
  (and `https://nels.money` for the Checkout CTA). The app's `VITE_API_BASE` stays
  `https://nels-api.fly.dev/api` (or move to `api.nels.money` later).
- **Legal dependency**: a privacy policy and terms of service are required before
  taking payments and before the security/privacy copy can make its claims. Draft
  these (legal-advisor) as part of Phase 2; link them in the footer.

## Honesty / YMYL guardrails (must-fix in copy)

The strategy draft contains a few claims that must match reality before launch:

- **"Export your data"** — no export feature exists yet. Either build a minimal
  export or remove the claim. (Recommend: remove for launch, add later.)
- **"Encrypted at rest"** — true only if confirmed for Fly Managed Postgres; verify
  before stating it, otherwise say "encrypted in transit" only.
- **"Money-back guarantee"** — only state if actually offered; default to "Cancel
  anytime."
- **No fabricated testimonials, user counts, or savings guarantees.** Keep the
  "not financial advice" disclaimer (already in the FAQ copy).
- Placeholder social links (`twitter.com/nelsmoney`, etc.) in the JSON-LD must be
  real or removed.
- **Pricing page/teaser**: the strategy's Free-vs-Pro comparison table must be
  reframed to the actual model — a single plan, "free for 14 days, then $5/mo or
  $50/yr," no perpetual free tier. Update the `Product`/`Offer` JSON-LD accordingly.

## Build order

1. **Phase 1 — Marketing MVP**: SvelteKit scaffold; core pages with strategy copy
   (honesty pass applied); SEO infra (meta, JSON-LD, sitemap, robots); branding;
   CTA → app signup. Deploy to `nels.money`.
2. **Phase 2 — Billing**: Stripe product/prices + portal (via MCP); DB migration;
   checkout/portal/webhook/status endpoints; feature gating; Fly secrets; wire both
   the marketing and in-app CTAs to Checkout; "Manage subscription" → Portal; legal
   pages.
3. **Phase 3 — SEO content**: blog via MDsveX; first topic cluster; analytics;
   ongoing content per the strategy roadmap.

## Out of scope (YAGNI now)

Blog authoring, paid ads / email newsletter infra, multiple paid tiers (Team),
the app's charts UI.

## Risks

- **Honesty/YMYL** — copy must trace to real features (see guardrails).
- **Webhook idempotency** — upsert by `stripe_subscription_id`; tolerate
  out-of-order/duplicate events.
- **Auth ↔ Checkout linkage** — user must be authenticated to subscribe; map the
  Stripe customer to the Nels user (by id, stored on first checkout).
- **Trial abuse** (cardless trials) — acceptable at this stage.
