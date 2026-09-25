# Named-Competitor Comparison Pages — Design

**Date:** 2026-07-27
**Status:** Approved (brainstorming), pending implementation
**Related:** #445 (umbrella), `docs/marketing/nels-seo-strategy.md` §4 (page inventory), §6 (meta), §7 (JSON-LD)

## Problem

`/[lang]/compare` (`marketing/src/routes/[lang]/compare/+page.svelte`, 48 lines) is a four-row table
comparing Nels to "Traditional apps." It names no competitor, so it ranks for nothing — nobody
searches "nels vs traditional apps." The SEO strategy anticipated exactly this page and specified
named competitors for it (§4: *"Nels vs. 3 competitors (YNAB, Mint/EveryDollar, traditional
spreadsheets). Honest feature parity table."*); that was never built, and the competitive landscape
has since moved on considerably.

Three concrete defects:

1. **No named competitors.** Cluster E in the SEO strategy — "best AI budgeting app 2026"
   (~200–500/mo), "YNAB vs AI budgeting" (~50–150/mo), "Mint alternative AI" (~80–200/mo) — has no
   landing surface. High-intent "vs" queries are the last search a prospect runs before subscribing.
2. **The hub is orphaned.** `/compare` is prerendered and in the sitemap, but `NAV`
   (`marketing/src/lib/site.js:24`) omits it and neither `Nav.svelte` nor `Footer.svelte` links to
   it. Organic visitors cannot reach it by navigation.
3. **Nothing is sourced or dated.** Comparative claims carry no "verified on" date and no link to
   the competitor's own pricing page — weak for SEO, and a poor posture for comparative advertising.

## Goals

1. Rebuild `/compare` as a **segmented hub** and add **eleven deep dives** at `/[lang]/compare/{slug}`.
2. Hold every competitor fact **once, in JS**, with a source URL and a verification date.
3. Keep prerender entries and sitemap paths **derived** from that data, so they cannot drift.
4. Full six-locale coverage, matching every other localized page.
5. Make each page **honest** — every one names something the competitor does better.

## Non-Goals (YAGNI)

- Automated price scraping or a scheduled freshness job. Verification is manual, recorded in
  `verifiedOn`, and re-checked per release.
- A CMS or MDX pipeline. Copy lives in the existing locale JSON.
- Per-competitor screenshots or logos (see §7 — deliberately avoided).
- Comparison *blog* posts. The SEO strategy's Cluster E blog content is separate work.
- A `/compare/spreadsheet` deep dive. Spreadsheets are a **row on the hub** — "vs a spreadsheet" is
  an argument about effort, not features, and there is no pricing to verify.

---

## Section 1 — Page inventory

Twelve pages: one hub plus eleven deep dives, in three segments. The "why Nels instead" argument
differs per segment, so the hub groups them rather than rendering one unreadable 12-column table.

### Segment `ai-native` — conversational AI over your money

Nels' direct positioning rivals. "Conversational" is **no longer a differentiator on its own**;
these pages must argue specifics.

| Slug | Product | Verified 2026-07-27 |
|---|---|---|
| `chatgpt` | Finances in ChatGPT | Pro 2026-05-15, Plus 2026-06-25. 12,000+ institutions via Plaid; dashboard of spending, subscriptions, upcoming payments. **Read-only** — cannot move money, pay a bill, or place a trade. ChatGPT Plus $20/mo. |
| `cleo` | Cleo | Chat-first budgeting plus cash advances. Free, then Plus ~$5.99/mo, Pro ~$8.99/mo, Builder ~$14.99/mo. **Second-hand pricing — see §7.** |
| `era` | Era (era.app) | AI-agent finance platform: MCP server ("Context"), mobile app ("Agency"), investing research ("Thesis"). Free tier plus paid; pricing not published on the site. |
| `origin` | Origin (useorigin.com) | AI budgeting + investing + planning, no human advisors. Advertising a **$1/year promo** — not list price. |
| `copilot` | Copilot Money | Apple-centric, strong automatic categorization. |

### Segment `traditional` — established budgeting apps

| Slug | Product | Angle |
|---|---|---|
| `ynab` | YNAB | The zero-based benchmark; closest philosophical match to Nels' envelope/fund model. Highest "vs" search volume. |
| `monarch` | Monarch Money | The main post-Mint destination; strong household sharing, which contests a Nels differentiator directly. |
| `simplifi` | Quicken Simplifi | The value tier — closest to Nels on price, so the argument is features, not cost. |
| `everydollar` | EveryDollar | Ramsey ecosystem; free tier plus paid. |

### Segment `planning` — planning and wealth management

A different product category from day-to-day budgeting. For both, the honest verdict is
**"complementary, and a different buyer"** — not "we're better." These still earn their keep by
capturing searchers comparing categories who have not worked out which kind of tool they need.

| Slug | Product | Verified 2026-07-27 |
|---|---|---|
| `range` | Range (range.com) | Flat-fee, 0% AUM, human CFPs plus AI advisor "Rai". Targets $200k+ earners. Tiers: Premium / Platinum / Titanium; prices not public. |
| `boldin` | Boldin (formerly NewRetirement) | Retirement/financial planning — decades-long projections, Monte Carlo, Roth conversion, tax planning. Free Basic; PlannerPlus **$144/yr**; Boldin Advisors **$3,200 flat fee** (CFP). |

`arito.ai` was evaluated and **dropped**: it is an agentic AI analytics platform for enterprise
finance/RevOps teams, not a consumer budgeting product.

### The `chatgpt` page

The single most important page in the set, and the hardest to write. Finances in ChatGPT is
conversational AI over connected accounts — Nels' exact pitch, from the largest AI distributor in
the world. The differentiators that survive, each verifiable:

- **Read-only vs. writable ledger.** ChatGPT *observes* accounts; it cannot move money, pay a bill,
  or place a trade. Nels is a ledger you *write* — you allocate to envelope/fund categories and it
  holds the plan you are executing. A dashboard is not a budget.
- **Single-user.** No household sharing, no audit log of who changed what.
- **No budgeting methodology.** Insight into past spending, not a zero-based plan with goals and
  carry-over rules.
- **Price for the job.** ChatGPT Plus is $20/mo; Nels is $3–5/mo.
- **Availability.** US-only and gated behind paid ChatGPT tiers at launch.

Write it straight. Finances in ChatGPT is genuinely good and enormously distributed; a page that
pretends otherwise reads as defensive and costs more trust than it wins.

---

## Section 2 — Routing

One dynamic route: `marketing/src/routes/[lang]/compare/[competitor]/+page.js` + `+page.svelte`.

`load()` validates `params.competitor` against the data module and throws `error(404)` otherwise —
mirroring the locale check already in `[lang]/+layout.js`:

```js
export async function load({ params, parent }) {
  const { lang } = await parent();
  const competitor = findCompetitor(params.competitor);
  if (!competitor) throw error(404, `Unknown competitor: ${params.competitor}`);
  return { lang, competitor };
}
```

`prerender` and `trailingSlash` are inherited from `[lang]/+layout.js`.

## Section 3 — Data model

`marketing/src/lib/compare/competitors.js`. Competitor facts live here **once, in JS**; locale JSON
holds only prose and feature labels. A price correction is a one-line edit instead of six
translation edits, and locales cannot drift apart on facts.

A single `priceMonthly` number is insufficient. The set contains free tiers (Cleo, EveryDollar, Era,
Boldin), multi-tier ladders (Cleo's Plus/Pro/Builder), promotional pricing that is not list price
(Origin's $1/year), unpublished pricing (Era, Range), and products not comparable monthly at all
(Range, Boldin's $3,200 advisor tier).

```js
/**
 * @typedef {'ai-native'|'traditional'|'planning'} Segment
 * @typedef {'freemium'|'subscription'|'flat-fee'|'undisclosed'} PricingModel
 */
export const COMPETITORS = [
  {
    slug: 'cleo',
    name: 'Cleo',
    segment: 'ai-native',
    url: 'https://web.meetcleo.com',
    pricing: {
      model: 'freemium',
      hasFreeTier: true,
      tiers: [
        { name: 'Plus', monthly: 5.99 },
        { name: 'Pro', monthly: 8.99 },
        { name: 'Builder', monthly: 14.99 },
      ],
      note: 'second-hand',   // 'promo' | 'second-hand' | 'undisclosed' | undefined
    },
    features: { /* see §4 */ },
    strength: 'cleo.strength',  // i18n key — what they do better than Nels (REQUIRED)
    sourceUrl: 'https://web.meetcleo.com/pricing',
    verifiedOn: '2026-07-27',
    published: false,           // see below
  },
  // …
];
```

**The `published` flag.** Phase 1 lands all eleven competitor records so the schema is exercised and
the hub grid is real, but the per-page prose does not exist until Phases 2–3. `published: false`
excludes a competitor from prerender entries, the sitemap, and the hub's deep-dive links, while its
row still appears in the hub grid (which needs only facts, not prose). Phase 2/3 flip the flag in
the same PR that adds the copy. `findCompetitor()` returns unpublished competitors as `undefined`,
so the route 404s rather than rendering empty prose.

Nels' own row is **derived from `SITE.plans`** (`marketing/src/lib/site.js`), never hardcoded, so
the comparison can never drift from `/pricing`.

`pricing.note` drives a rendered caveat: `promo` → "promotional rate, not list price";
`second-hand` → "from third-party reporting, not the vendor's site"; `undisclosed` → "pricing not
published."

## Section 4 — Feature matrix

A canonical, closed feature list. Every competitor's `features` object must have exactly these keys
(enforced by test, §8). Values are `true | false | 'partial'`.

| Key | Meaning | Nels |
|---|---|---|
| `conversationalLogging` | Log/modify a transaction by chatting, not just query | `true` |
| `writableLedger` | A budget you edit and execute, not a read-only view | `true` |
| `envelopeBudgeting` | Zero-based / envelope / fund methodology with carry-over | `true` |
| `householdSharing` | Multiple people on one budget with permissions | `true` |
| `auditLog` | Who changed what, visible | `true` |
| `bankSync` | Automatic institution syncing | `'partial'` — Pro tier only |
| `passwordless` | Sign in without a password (TOTP) | `true` |
| `aiInsights` | Generated spending narrative / insight | `true` |
| `goals` | Savings/debt goal tracking | `true` |
| `humanAdvisors` | Access to a human CFP/planner | `false` |
| `retirementProjection` | Multi-decade projection / Monte Carlo | `false` |

`'partial'` renders a footnote rather than a bare check or cross — the honesty requirement in §7
depends on it (Nels' own `bankSync: 'partial'` is the first example).

## Section 5 — Copy and i18n

- `compare.feature.{key}` — shared feature labels (11 keys).
- `compare.segment.{segment}` — segment headings and blurbs.
- `compare.{slug}.*` — per-page prose: `headline`, `intro`, `whoFor`, `strength`, `verdict`.
- `compare.note.{promo|secondHand|undisclosed}` — pricing caveats.

All six locales (`en`, `es`, `fr`, `de`, `it`, `pt`), machine-translated, carrying the existing
`_meta.machineTranslated` convention. Locale files must not contain prices — those interpolate from
the data module.

## Section 6 — SEO

- **Per page:** `<Seo title="Nels vs YNAB" path="/compare/ynab" lang={data.lang} description=… />`.
  Canonical and hreflang alternates come free from the existing component.
- **Titles** follow §6 of the SEO strategy: ≤60 chars, ≤155 char descriptions.
- **JSON-LD:** a `FAQPage` block per deep dive (3–4 questions) via the existing `faqPage()` builder
  in `marketing/src/lib/components/schema.js`. No new schema builders needed.
- **Internal linking:** add `/compare` to `NAV` in `site.js` and link it from `Footer.svelte`
  (fixes the orphan). Hub → every deep dive; each deep dive → hub and `/pricing`.
- **Prerender + sitemap:** both derived from `COMPETITORS` (§8). 12 pages × 6 locales = 72 URLs.

## Section 7 — Accuracy, sourcing, and legal posture

- **Factual, publicly verifiable claims only.** No competitor logos, no trademark styling, no
  screenshots of competitor products.
- Every deep dive renders **"Compared using publicly available pricing as of {verifiedOn}"** linking
  to `sourceUrl` — the competitor's own pricing page.
- **Every page must name at least one thing the competitor does better** (`strength`, a required
  field). This is enforced structurally, not left to the copywriter's discretion.
- **Verify every number from the vendor's own pricing page at implementation time.** Figures in this
  spec come from a mix of vendor sites and third-party reporting and are a **starting point, not a
  source of truth**. Specifically: `web.meetcleo.com` returned HTTP 403, so Cleo's tiers are
  second-hand; Era and Range do not publish prices.
- **This category moves fast.** Finances in ChatGPT shipped in May 2026 and reached Plus users in
  June — a capability that did not exist when the current `/compare` page was written. Treat
  `verifiedOn` as load-bearing and re-check the `ai-native` segment before each release.

## Section 8 — Testing

Per the marketing harness: pure-helper unit tests plus a build. **No Svelte component tests** —
vitest here is node-env.

`marketing/src/lib/compare/competitors.test.js`:
- Slugs unique, non-empty, URL-safe.
- `verifiedOn` is ISO-8601 `YYYY-MM-DD` and parses to a real date.
- `sourceUrl` and `url` are `https`.
- Every `features` object has **exactly** the canonical key set (no missing, no extra).
- Every feature value is `true | false | 'partial'`.
- Every `segment` is one of the three valid values.
- Every competitor has a non-empty `strength` (enforces the honesty rule in §7).
- Pricing shape is internally consistent: `hasFreeTier` matches `model`; `tiers` non-empty unless
  the model is `undisclosed`.

`marketing/src/lib/compare/paths.test.js`:
- The prerender entry list and the sitemap's localized page list each cover **every published**
  slug in `COMPETITORS` — the anti-drift guarantee from §6.
- Unpublished competitors appear in **neither**, and `findCompetitor()` returns `undefined` for them.

**Import constraint:** `svelte.config.js` runs in plain node and cannot resolve the `$lib` alias, so
`competitors.js` and `paths.js` must have **no imports** — no `$lib/site.js`, no svelte-i18n. The
Nels comparison row is therefore assembled in the page components from `SITE`, not inside the data
module. A test asserts the module's import list is empty.

Locale parity, in the established style of `marketing/src/lib/site.test.js:76`:
- Every locale has `compare.{slug}.*` for every competitor and every `compare.feature.*` key.
- Non-English strings are **not** identical to the English text (catches untranslated fallbacks).

Gate: `cd marketing && pnpm test && pnpm run build && pnpm check` all green.

## Section 9 — Risks

| Risk | Mitigation |
|---|---|
| Competitor pricing goes stale and a page states something false | `verifiedOn` + `sourceUrl` rendered on every page; re-check per release. Deliberately **not** a build-breaking staleness test — CI should not fail on a page nobody touched. |
| Comparative claims draw a complaint | Factual claims only, sourced and dated; no logos or trademark styling; required `strength` field per competitor. |
| Roughly 70 new message keys × 6 locales is a large translation surface | Facts live in JS, not locale files, so factual corrections never re-translate. Phasing (§10) spreads the copy load. |
| Nels' differentiators are narrower than assumed post-ChatGPT | Confronted directly in §1; `writableLedger` and `auditLog` exist as feature keys precisely to make the surviving differences concrete. |
| Adding a competitor leaves its page unprerendered or unindexed | Prerender entries and sitemap derived from `COMPETITORS`, with a test (§8). |

## Section 10 — Phasing

Each phase is a PR boundary and a tracking issue.

- **Phase 1 — Infrastructure.** Data module, `[competitor]` route, hub rebuild, derived
  prerender/sitemap, nav+footer links, all tests. Ships with zero deep dives; the hub renders from
  whatever is in `COMPETITORS`.
- **Phase 2 — AI-native.** `chatgpt` first (sharpest threat, hardest to write), then `cleo`, `era`,
  `origin`, `copilot`.
- **Phase 3 — Traditional and planning.** `ynab`, `monarch`, `simplifi`, `everydollar`, then `range`
  and `boldin` framed as category differences.
