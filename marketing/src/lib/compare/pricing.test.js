import { describe, it, expect } from 'vitest';
import { COMPETITORS } from './competitors.js';
import { formatUsd, pricingSummary } from './pricing.js';
import { NELS_PRICING_SUMMARY, oldestVerifiedOn } from './nels.js';
import { SITE } from '$lib/site.js';

describe('formatUsd', () => {
  it('drops the decimals on whole dollars', () => {
    expect(formatUsd(13)).toBe('$13');
    expect(formatUsd(99.0)).toBe('$99');
    expect(formatUsd(144)).toBe('$144');
  });

  it('gives exactly two decimals on anything else', () => {
    expect(formatUsd(5.99)).toBe('$5.99');
    expect(formatUsd(12.99)).toBe('$12.99');
    // 950.4 must not render as $950.4 — a price is never one decimal.
    expect(formatUsd(950.4)).toBe('$950.40');
  });

  it('groups thousands', () => {
    // Range's tiers. Without grouping these render $3950 / $12500, which read
    // as typos rather than prices.
    expect(formatUsd(3950)).toBe('$3,950');
    expect(formatUsd(12500)).toBe('$12,500');
    expect(formatUsd(3200)).toBe('$3,200');
    expect(formatUsd(1234.5)).toBe('$1,234.50');
  });
});

// Driven off the real data rather than fixtures: an edit to competitors.js
// that breaks what the page claims fails HERE, next to the rule it broke,
// instead of silently changing a live page.
//
// The @type annotation is load-bearing, not decoration: without it, indexing
// this literal by `c.slug` is an implicit-any under checkJs and adds a sixth
// svelte-check error, breaking the gate.
/** @type {Record<string, import('./pricing.js').PricingSummary>} */
const EXPECTED = {
  chatgpt: { variant: 'exact', period: 'monthly', display: '$20', annualDisplay: null, caveat: null },
  cleo: { variant: 'freeThenFrom', period: 'monthly', display: '$5.99', annualDisplay: null, caveat: null },
  era: { variant: 'freeThenFrom', period: 'monthly', display: '$9.99', annualDisplay: '$95.88', caveat: null },
  origin: { variant: 'exact', period: 'monthly', display: '$12.99', annualDisplay: '$99', caveat: null },
  copilot: { variant: 'exact', period: 'monthly', display: '$13', annualDisplay: '$95', caveat: null },
  ynab: { variant: 'exact', period: 'monthly', display: '$14.99', annualDisplay: '$109', caveat: null },
  // "from" rather than "exact" because Monarch sells a second, dearer tier
  // (Plus) alongside Core; both figures below are Core's.
  monarch: { variant: 'from', period: 'monthly', display: '$14.99', annualDisplay: '$99.99', caveat: null },
  // No annualDisplay: Quicken bills Simplifi annually but publishes no annual
  // total, and its own footnote warns against multiplying the monthly by 12.
  // The `annualOnly` caveat is what stops that gap reading as "cheap to try
  // monthly" next to competitors whose monthly figures really are monthly.
  simplifi: { variant: 'exact', period: 'monthly', display: '$6.99', annualDisplay: null, caveat: 'annualOnly' },
  everydollar: { variant: 'freeThen', period: 'monthly', display: '$17.99', annualDisplay: '$79.99', caveat: null },
  // Range publishes all three tiers now, annual-only, so this reads as a floor.
  // `annualDisplay` is null because the headline already IS the annual figure.
  range: { variant: 'from', period: 'annual', display: '$3,950', annualDisplay: null, caveat: null },
  // Boldin publishes only an annual figure plus a flat advisor fee. The flat
  // fee is a second tier, which is what makes it read "from"; it is never the
  // headline amount. `annualDisplay` stays null because the headline already
  // IS the annual figure — repeating it as an alternative would be circular.
  boldin: { variant: 'freeThenFrom', period: 'annual', display: '$144', annualDisplay: null, caveat: null },
};

describe('pricingSummary over the real competitor data', () => {
  it('covers every competitor', () => {
    expect(Object.keys(EXPECTED).sort()).toEqual(COMPETITORS.map((c) => c.slug).sort());
  });

  for (const c of COMPETITORS) {
    it(`${c.slug} summarizes as expected`, () => {
      expect(pricingSummary(c.pricing)).toEqual(EXPECTED[c.slug]);
    });
  }
});

describe('pricingSummary edge cases', () => {
  it('treats missing pricing as undisclosed rather than throwing', () => {
    expect(pricingSummary(undefined)).toEqual({
      variant: 'undisclosed', period: null, display: null, annualDisplay: null, caveat: null,
    });
    expect(pricingSummary(null)).toEqual({
      variant: 'undisclosed', period: null, display: null, annualDisplay: null, caveat: null,
    });
  });

  it('treats figure-free tiers as undisclosed', () => {
    const summary = pricingSummary({ model: 'flat-fee', hasFreeTier: false, tiers: [{ name: 'Gold' }] });
    expect(summary.variant).toBe('undisclosed');
    expect(summary.display).toBeNull();
  });

  it('honours an explicit undisclosed model with no note', () => {
    expect(pricingSummary({ model: 'undisclosed', hasFreeTier: false }).variant).toBe('undisclosed');
  });

  // Both guards below are load-bearing rather than belt-and-braces. Every other
  // test that reaches them also has no priced tier, so the `!period` fallback
  // would return an identical object and deleting either guard would go
  // unnoticed. The case that matters is a competitor gaining a figure while
  // keeping the note — Range is one edit away from exactly that — where a stray
  // figure must not override the vendor's own statement that the price is not
  // public.
  it('lets an undisclosed NOTE win over a figure that appears alongside it', () => {
    const summary = pricingSummary({
      model: 'flat-fee', hasFreeTier: false, tiers: [{ name: 'Premium', monthly: 50 }], note: 'undisclosed',
    });
    expect(summary.variant).toBe('undisclosed');
    expect(summary.display).toBeNull();
  });

  it('lets an undisclosed MODEL win over a figure that appears alongside it', () => {
    const summary = pricingSummary({
      model: 'undisclosed', hasFreeTier: false, tiers: [{ name: 'Premium', monthly: 50 }],
    });
    expect(summary.variant).toBe('undisclosed');
    expect(summary.display).toBeNull();
  });

  it('surfaces a promo note as a caveat alongside the list price', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false, tiers: [{ name: 'Plan', monthly: 9 }], note: 'promo',
    });
    expect(summary).toEqual({
      variant: 'exact', period: 'monthly', display: '$9', annualDisplay: null, caveat: 'promo',
    });
  });

  it('camelCases the second-hand note into its i18n key suffix', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false, tiers: [{ name: 'Plan', monthly: 9 }], note: 'second-hand',
    });
    expect(summary.caveat).toBe('secondHand');
  });

  it('does not repeat an undisclosed note as a caveat', () => {
    // The undisclosed CELL already reads compare.note.undisclosed, so echoing
    // the same string underneath it would say the same thing twice.
    expect(pricingSummary({ model: 'flat-fee', hasFreeTier: false, tiers: [], note: 'undisclosed' }).caveat).toBeNull();
  });

  it('takes the cheapest tier, not the first, so reordering cannot change the claim', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false,
      tiers: [{ name: 'Big', monthly: 40 }, { name: 'Small', monthly: 4 }],
    });
    expect(summary).toEqual({
      variant: 'from', period: 'monthly', display: '$4', annualDisplay: null, caveat: null,
    });
  });

  it('prefers a monthly figure over an annual one and never derives one from the other', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false, tiers: [{ name: 'Plan', monthly: 10, annual: 96 }],
    });
    expect(summary.period).toBe('monthly');
    expect(summary.display).toBe('$10');
  });

  it('treats a figure-free tier as evidence of a higher price, not a lower one', () => {
    // A "contact us" plan alongside a listed one means the listed price is a
    // floor. Reading `exact` here would claim a single price for a product
    // that demonstrably has a more expensive option.
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false,
      tiers: [{ name: 'Pro', monthly: 12.99 }, { name: 'Enterprise' }],
    });
    expect(summary.variant).toBe('from');
  });
});

describe('pricingSummary annual alternative', () => {
  it('reports the annual price of the SAME tier as the monthly headline', () => {
    // Not the cheapest annual figure across all tiers — pairing the entry
    // monthly with some other plan's annual would invent a bundle nobody sells.
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false,
      tiers: [
        { name: 'Entry', monthly: 10, annual: 96 },
        { name: 'Big', monthly: 40, annual: 60 },
      ],
    });
    expect(summary.display).toBe('$10');
    expect(summary.annualDisplay).toBe('$96');
  });

  it('is null when the entry tier publishes no annual price', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false, tiers: [{ name: 'Plan', monthly: 20 }],
    });
    expect(summary.annualDisplay).toBeNull();
  });

  it('is null when the headline is already annual, so the cell cannot repeat itself', () => {
    const summary = pricingSummary({
      model: 'subscription', hasFreeTier: false, tiers: [{ name: 'Plan', annual: 144 }],
    });
    expect(summary.period).toBe('annual');
    expect(summary.display).toBe('$144');
    expect(summary.annualDisplay).toBeNull();
  });

  // The reason this field exists at all. Annual discounts are not uniform, and
  // Nels' is the smallest of any product in the table that offers one, so a
  // monthly-only row understates every competitor's real floor by more than it
  // understates ours.
  it('keeps every competitor that publishes an annual price from being overstated', () => {
    const withAnnual = COMPETITORS.filter((c) =>
      (c.pricing.tiers ?? []).some((/** @type {{annual?: number}} */ t) => typeof t.annual === 'number')
    );
    expect(withAnnual.length).toBeGreaterThan(0);
    for (const c of withAnnual) {
      const summary = pricingSummary(c.pricing);
      if (summary.period !== 'monthly') continue;
      expect(summary.annualDisplay).not.toBeNull();
    }
  });
});

// NELS_PRICING_SUMMARY is the change's main anti-self-favouring safeguard, and
// until now nothing referenced it outside the two templates — so hardcoding its
// variant to 'exact', which silently drops the "From" hedge and makes Nels look
// like a single cheap price against competitors that say "from", was a mutation
// no test could see. These three assertions close that.
describe('NELS_PRICING_SUMMARY', () => {
  it('hedges with "from" while more than one plan is on sale', () => {
    expect(Object.keys(SITE.plans).length).toBeGreaterThan(1);
    expect(NELS_PRICING_SUMMARY.variant).toBe('from');
  });

  it('tracks SITE.plans rather than restating the figures', () => {
    expect(NELS_PRICING_SUMMARY.display).toBe(SITE.plans.basic.monthly);
    expect(NELS_PRICING_SUMMARY.annualDisplay).toBe(SITE.plans.basic.annual);
  });

  it('publishes its annual price, exactly as every competitor does', () => {
    // Nels has the SMALLEST annual discount in the table, so a row that showed
    // monthly only would have flattered us more than anyone else.
    expect(NELS_PRICING_SUMMARY.period).toBe('monthly');
    expect(NELS_PRICING_SUMMARY.annualDisplay).not.toBeNull();
  });
});

describe('oldestVerifiedOn', () => {
  it('returns the stalest date in the group, not the freshest', () => {
    const items = [{ verifiedOn: '2026-07-27' }, { verifiedOn: '2025-01-04' }, { verifiedOn: '2026-01-01' }];
    expect(oldestVerifiedOn(items)).toBe('2025-01-04');
  });

  it('is undefined for an empty group so the template can guard it', () => {
    expect(oldestVerifiedOn([])).toBeUndefined();
  });
});
