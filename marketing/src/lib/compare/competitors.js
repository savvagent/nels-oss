// Competitor facts for the /compare pages. Facts live here ONCE, in JS; locale
// JSON holds only prose and labels, so a price correction is a one-line edit
// instead of six translation edits.
//
// IMPORTANT: this module must have ZERO imports. svelte.config.js imports it to
// derive prerender entries and runs in plain node, where the `$lib` alias does
// not resolve. The Nels comparison row is assembled in the page components from
// SITE, never here.
//
// Every figure below carries a sourceUrl and verifiedOn. Re-verify from the
// vendor's own pricing page before each release — this category moves fast.

/** @typedef {'ai-native'|'traditional'|'planning'} Segment */
/** @typedef {true|false|'partial'} FeatureValue */
/** @typedef {Record<string, FeatureValue>} Features */
/**
 * @typedef {Object} Competitor
 * @property {string} slug
 * @property {string} name
 * @property {Segment} segment
 * @property {string} url - Not currently rendered (only `sourceUrl` is linked
 *   today). If a future page ever links this field, it must carry the same
 *   `rel="nofollow noopener" target="_blank"` treatment as `sourceUrl` — this
 *   points at a competitor's own site.
 * @property {*} pricing
 * @property {Features} features
 * @property {string} strength
 * @property {string} sourceUrl
 * @property {string} verifiedOn
 * @property {boolean} published
 */

export const SEGMENTS = ['ai-native', 'traditional', 'planning'];

// A competitor qualifies for `passwordless: true` when a sign-in path exists
// that does not require the user to recall a password — "sign in with
// Google/Apple", a passkey, an authenticator code, or an emailed one-time code
// or link that logs you straight in. A traditional password also being offered
// does not disqualify it.
//
// The boundary that actually decides cases here: a link that SIGNS YOU IN
// counts; a link that makes you SET A NEW PASSWORD before you get in is
// password recovery and does not. Both arrive by email from a "forgot
// password" affordance, so they look identical from the login page and can
// only be told apart from the vendor's own description of what the link does.
// Simplifi is `true` on this test and Range is `false`, and that is the
// distinction between them — not a difference in how hard each was looked at.
//
// That Phase 3 audit has now happened, and the warning it was written under
// was justified: five of the six then-unpublished competitors carried
// `passwordless: false` unaudited, and five of those six were wrong in Nels'
// favour. Each now cites its evidence inline. Range is the only remaining
// `false` in the column, and it is verified rather than assumed —
// range.com/login offers email, password, and nothing else.
//
// `'partial'` carries TWO meanings in this table, and both are legitimate, so
// the distinction is written here rather than rediscovered per competitor:
//
//   1. Degraded — the feature exists but is materially weaker than a plain
//      yes. Simplifi's household sharing caps at one extra person; Copilot's
//      is a shared link with no separate logins.
//   2. Tier-gated — the feature is absent from the plan the price cell leads
//      with, and present higher up. Nels' own bankSync is 'partial' because it
//      is Pro-only; EveryDollar's bankSync and householdSharing are 'partial'
//      because its cell leads with "Free" and both need Premium.
//
// Rule of thumb for the gated case: compare against the tier the pricing cell
// advertises, not the vendor's best plan. A product with no free tier is
// judged on its entry paid plan, which is why YNAB and Monarch are plain
// `true` for sharing included at their only/entry tier.
//
// Closed feature set. Every competitor must define exactly these keys.
export const FEATURE_KEYS = [
  'conversationalLogging',
  'writableLedger',
  'envelopeBudgeting',
  'householdSharing',
  'auditLog',
  'bankSync',
  'passwordless',
  'aiInsights',
  'goals',
  'humanAdvisors',
  'retirementProjection',
];

/** @type {Competitor[]} */
export const COMPETITORS = [
  {
    slug: 'chatgpt',
    name: 'Finances in ChatGPT',
    segment: 'ai-native',
    url: 'https://chatgpt.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Plus', monthly: 20 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: false,
      envelopeBudgeting: false,
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: true,
      aiInsights: true,
      goals: false,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.chatgpt.strength',
    sourceUrl: 'https://openai.com/index/personal-finance-chatgpt/',
    verifiedOn: '2026-07-27',
    published: true,
  },
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
    },
    features: {
      conversationalLogging: true,
      writableLedger: 'partial',
      envelopeBudgeting: false,
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: true,
      aiInsights: true,
      goals: 'partial',
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.cleo.strength',
    // web.meetcleo.com/pricing renders fine in a real browser but returns 403
    // to automated fetches (curl, headless tools) — a bot-blocking quirk, not
    // a dead page. Re-verifying here will hit that 403; it is not evidence
    // the tiers are stale or that the `note: 'second-hand'` flag (removed
    // once confirmed) was dropped without cause. The tiers above were
    // verified against Cleo's own help-centre articles, which return 200.
    sourceUrl: 'https://web.meetcleo.com/pricing',
    verifiedOn: '2026-07-27',
    published: true,
  },
  {
    slug: 'era',
    name: 'Era',
    segment: 'ai-native',
    url: 'https://www.era.app',
    pricing: {
      model: 'freemium',
      hasFreeTier: true,
      tiers: [
        { name: 'Organize', monthly: 9.99, annual: 95.88 },
        { name: 'Automate', monthly: 24.99, annual: 239.88 },
        { name: 'Operate', monthly: 99.0, annual: 950.4 },
      ],
    },
    features: {
      conversationalLogging: 'partial',
      writableLedger: false,
      envelopeBudgeting: false,
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: true,
      aiInsights: true,
      goals: false,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.era.strength',
    sourceUrl: 'https://www.era.app/pricing',
    verifiedOn: '2026-07-27',
    published: true,
  },
  {
    slug: 'origin',
    name: 'Origin',
    segment: 'ai-native',
    url: 'https://useorigin.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Origin', monthly: 12.99, annual: 99 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: 'partial',
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      passwordless: true,
      aiInsights: true,
      goals: true,
      humanAdvisors: 'partial',
      retirementProjection: 'partial',
    },
    strength: 'compare.origin.strength',
    sourceUrl: 'https://support.useorigin.com/hc/en-us/articles/21022711456141-How-much-does-Origin-cost',
    verifiedOn: '2026-07-27',
    published: true,
  },
  {
    slug: 'copilot',
    name: 'Copilot Money',
    segment: 'ai-native',
    url: 'https://copilot.money',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Copilot', monthly: 13, annual: 95 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: 'partial',
      householdSharing: 'partial',
      auditLog: false,
      bankSync: true,
      passwordless: true,
      aiInsights: true,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.copilot.strength',
    sourceUrl: 'https://copilot.money/pricing',
    verifiedOn: '2026-07-27',
    published: true,
  },
  {
    slug: 'ynab',
    name: 'YNAB',
    segment: 'traditional',
    url: 'https://www.ynab.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'YNAB', monthly: 14.99, annual: 109 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      // Verified on the pricing page: "Share YNAB with partners, families, and
      // other close-knit groups of up to six people—all for the price of a
      // single subscription."
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      // Sign in with Google or Apple, plus passkeys.
      // support.ynab.com/en_us/how-to-sign-in-with-google-or-apple-BkE7jElRq
      passwordless: true,
      aiInsights: false,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.ynab.strength',
    sourceUrl: 'https://www.ynab.com/pricing',
    verifiedOn: '2026-07-28',
    published: true,
  },
  {
    slug: 'monarch',
    name: 'Monarch Money',
    segment: 'traditional',
    // monarchmoney.com now redirects to monarch.com; both fields point at the
    // final URL for the same reason Boldin's does.
    url: 'https://www.monarch.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      // Two tiers, read off the live pricing page with its billing-period
      // control set each way. The seeded record had a single tier named
      // "Premium", which both misnamed the entry plan and hid the fact that
      // Monarch sells a more expensive one — so its cell used to read as an
      // exact price rather than a floor.
      tiers: [
        { name: 'Core', monthly: 14.99, annual: 99.99 },
        { name: 'Plus', annual: 199.99 },
      ],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      // Unlimited collaborators on the entry tier, not per-seat. This is the
      // one competitor that contests household sharing head-on.
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      // "Continue with Google" / "Continue with Apple".
      passwordless: true,
      aiInsights: 'partial',
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.monarch.strength',
    sourceUrl: 'https://www.monarch.com/pricing',
    verifiedOn: '2026-07-28',
    published: true,
  },
  {
    slug: 'simplifi',
    name: 'Quicken Simplifi',
    segment: 'traditional',
    url: 'https://www.quicken.com/products/simplifi/',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      // $6.99/month billed annually is the LIST price — the page shows it
      // struck through against a discounted first-year rate, and the seeded
      // $47.88 "annual" was that promo's yearly total. No annual figure is
      // recorded because Quicken publishes none: its own footnote says "The
      // final price may differ from the monthly discounted price multiplied
      // by 12 months", so deriving $83.88 would invent a price it does not
      // sell. Same rule pricing.js applies to Boldin in the other direction.
      //
      // `annualOnly` is load-bearing rather than decorative. Quicken sells no
      // month-to-month plan at all, so a bare "$6.99/month" sits in a column
      // where YNAB's $14.99, EveryDollar's $17.99 and Nels' own $3 are real
      // month-to-month rates, and reads as cheaper-to-try than it is. That
      // understates a competitor's commitment rather than ours, but it is the
      // same category of distortion the annual figures were added to fix.
      note: 'annual-only',
      tiers: [{ name: 'Simplifi', monthly: 6.99 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      // "You can add one other person" — a second seat, not a household.
      householdSharing: 'partial',
      auditLog: false,
      bankSync: true,
      // Quicken's own support article, verbatim: "Quicken Simplifi can send a
      // passwordless sign-in link to your email if you've forgotten your
      // password." The link signs you in — you are not made to set a new
      // password first — which is the side of the boundary above that counts,
      // even though it is reached from a forgotten-password affordance.
      // support.simplifi.quicken.com/en/articles/5488767
      passwordless: true,
      aiInsights: 'partial',
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.simplifi.strength',
    sourceUrl: 'https://www.quicken.com/products/simplifi/',
    verifiedOn: '2026-07-28',
    published: true,
  },
  {
    slug: 'everydollar',
    name: 'EveryDollar',
    segment: 'traditional',
    // /ramseyplus/everydollar now redirects to /money/everydollar.
    url: 'https://www.ramseysolutions.com/money/everydollar',
    pricing: {
      model: 'freemium',
      hasFreeTier: true,
      tiers: [{ name: 'Premium', monthly: 17.99, annual: 79.99 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      // "The household feature lets couples manage one budget together, even
      // if you each sign in with separate emails" — but only on Premium. That
      // is the same tier structure as bankSync directly below, so it takes the
      // same value: marking one `'partial'` and the other `true` for identical
      // free-vs-Premium gating is indefensible whichever way it leans. It also
      // matters more here than elsewhere in the column, because EveryDollar's
      // price cell leads with "Free" — a `true` next to that reads as "the
      // free plan shares", which it does not.
      householdSharing: 'partial',
      auditLog: false,
      // The free tier is manual entry; bank connect is Premium-only. This is
      // the tier-gating case, and it matches how Nels marks its own bankSync.
      bankSync: 'partial',
      // Passkeys across every Ramsey product since March 2026.
      // everydollar.help.ramseysolutions.com/hc/en-us/articles/44174159024525
      passwordless: true,
      aiInsights: false,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.everydollar.strength',
    sourceUrl: 'https://www.ramseysolutions.com/money/everydollar',
    verifiedOn: '2026-07-28',
    published: true,
  },
  {
    slug: 'range',
    name: 'Range',
    segment: 'planning',
    url: 'https://www.range.com',
    // Range publishes its tier prices now, so the seeded `note: 'undisclosed'`
    // is gone: "We offer 3 tiers of transparent, flat-fee pricing. No
    // percentage of your assets. No hidden fees." All three are priced per
    // year; none is billed monthly.
    //
    // DO NOT "correct" these to $2,400 / $4,800 from reading the page source.
    // Those figures are in range.com/pricing's HTML and review flagged them as
    // the real, cheaper prices. They are not reachable by any visitor: they sit
    // in a `div.tab-pane-tab-1.w-tab-pane` with `display: none`, and the page
    // contains zero `.w-tab-link` and zero `.w-tab-menu` elements, so no
    // control exists that could switch to that pane. It also names only
    // Premium and Platinum — no Titanium — which is what a stale two-tier
    // draft looks like. Checked in a real browser: the hidden node measures
    // 0×0 with a null offsetParent and is absent from `document.body.innerText`
    // entirely, while $3,950 renders at 135×51. Grepping raw HTML cannot tell
    // these apart; that is how the figures got reported as current.
    pricing: {
      model: 'flat-fee',
      hasFreeTier: false,
      tiers: [
        { name: 'Premium', annual: 3950 },
        { name: 'Platinum', annual: 5950 },
        { name: 'Titanium', annual: 12500 },
      ],
    },
    features: {
      conversationalLogging: false,
      writableLedger: false,
      // Range does ship budgeting tools ("Budget & Cash Flow", "Best-In-Class
      // Budgeting Tools"); what it does not ship is envelope budgeting, which
      // is the specific thing this row asks about.
      envelopeBudgeting: false,
      // "Membership Accounts for You and Your Partner".
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      // The only remaining `false` in this column, and the only one verified
      // by looking: range.com/login renders Email, Password, Forgot Password
      // and no third-party or passkey option. "Forgot Password" is ordinary
      // password recovery — Range documents it as changing or resetting the
      // password, not as a link that signs you in — so it falls on the far
      // side of the boundary from Simplifi's. Flip this the moment Range
      // documents otherwise; it is the one cell in this column still resting
      // on the absence of evidence rather than on a vendor's own words.
      passwordless: false,
      // "Access to Range's Proprietary AI (Rai)".
      aiInsights: true,
      goals: true,
      humanAdvisors: true,
      retirementProjection: true,
    },
    strength: 'compare.range.strength',
    sourceUrl: 'https://www.range.com/pricing',
    verifiedOn: '2026-07-28',
    published: true,
  },
  {
    slug: 'boldin',
    name: 'Boldin',
    segment: 'planning',
    url: 'https://www.boldin.com',
    pricing: {
      model: 'freemium',
      hasFreeTier: true,
      tiers: [
        { name: 'PlannerPlus', annual: 144 },
        { name: 'Boldin Advisors', flat: 3200 },
      ],
    },
    features: {
      conversationalLogging: false,
      writableLedger: false,
      envelopeBudgeting: false,
      householdSharing: 'partial',
      auditLog: false,
      bankSync: true,
      // "Sign up with Google" on boldin.com/auth/default/sign-up.
      passwordless: true,
      aiInsights: true,
      goals: true,
      humanAdvisors: true,
      retirementProjection: true,
    },
    strength: 'compare.boldin.strength',
    // boldin.com/pricing 301s here; this is the final URL, not the redirect.
    sourceUrl: 'https://www.boldin.com/retirement/pricing/',
    verifiedOn: '2026-07-28',
    published: true,
  },
];

/**
 * Look up a competitor for the [competitor] route. Unpublished competitors are
 * treated as absent so a page 404s rather than rendering empty prose.
 * @param {string} slug
 */
export function findCompetitor(slug) {
  return COMPETITORS.find((c) => c.slug === slug && c.published);
}
