// Render-layer coverage for PriceCell.
//
// pricing.js is well covered as pure logic, but nothing exercised the component
// that turns a descriptor into text — and two of its branches are dormant in
// data, so no prerendered page exercises them either. The `promo` and
// `secondHand` caveats only appear once a competitor record grows a
// `pricing.note`, which is a data edit that will not come with a UI review.
//
// The i18n parity suite catches a deleted key, but not a MOVED one: relocating
// these messages to a different path in the catalog would leave every existing
// gate green while the cell rendered a raw message key to a visitor. Asserting
// the rendered text here is what closes that gap.
import { describe, it, expect, beforeAll } from 'vitest';
import { render } from '@testing-library/svelte';

import '$lib/i18n/index.js';
import { waitLocale } from 'svelte-i18n';
import PriceCell from './PriceCell.svelte';

beforeAll(async () => {
  await waitLocale();
});

/**
 * @param {Partial<import('$lib/compare/pricing.js').PricingSummary>} overrides
 * @returns {import('$lib/compare/pricing.js').PricingSummary}
 */
const summary = (overrides) => ({
  variant: 'exact',
  period: 'monthly',
  display: '$9',
  annualDisplay: null,
  caveat: null,
  ...overrides,
});

/** @param {Partial<import('$lib/compare/pricing.js').PricingSummary>} overrides */
const textOf = (overrides) => {
  const { container } = render(PriceCell, { props: { summary: summary(overrides) } });
  return /** @type {string} */ (container.textContent).replace(/\s+/g, ' ').trim();
};

describe('PriceCell', () => {
  it('renders an exact monthly price bare', () => {
    expect(textOf({})).toBe('$9/month');
  });

  it('hedges a multi-tier price with "from"', () => {
    expect(textOf({ variant: 'from' })).toBe('From $9/month');
  });

  it('renders a free tier ahead of the paid entry price', () => {
    expect(textOf({ variant: 'freeThen' })).toBe('Free, then $9/month');
    expect(textOf({ variant: 'freeThenFrom' })).toBe('Free, then from $9/month');
  });

  it('renders an annual-only price with the year period', () => {
    expect(textOf({ period: 'annual', display: '$144' })).toBe('$144/year');
  });

  it('offers the annual alternative when the entry tier publishes one', () => {
    expect(textOf({ annualDisplay: '$90' })).toBe('$9/month or $90/year');
  });

  it('says so plainly when a competitor publishes no pricing', () => {
    // The cell text IS the caveat here, which is why the descriptor carries no
    // separate caveat for this variant — otherwise it would say it twice.
    expect(textOf({ variant: 'undisclosed', period: null, display: null })).toBe(
      'Pricing not published.'
    );
  });

  it('renders a promotional caveat under the price', () => {
    expect(textOf({ caveat: 'promo' })).toBe('$9/month Promotional rate, not list price.');
  });

  it('renders a second-hand caveat under the price', () => {
    expect(textOf({ caveat: 'secondHand' })).toContain('third-party reporting');
  });

  it('never leaks a raw message key', () => {
    for (const variant of /** @type {const} */ ([
      'exact',
      'from',
      'freeThen',
      'freeThenFrom',
      'undisclosed',
    ])) {
      expect(textOf({ variant, caveat: 'promo', annualDisplay: '$90' })).not.toMatch(/compare\./);
    }
  });

  it('does not compound opacity on the caveat line', () => {
    // The competitor cells are already `opacity-70` and CSS opacity compounds
    // through nesting, so a second `opacity-70` here would render the caveat at
    // an effective 0.49 — below WCAG 1.4.3 AA at this size. See PriceCell.svelte.
    const { container } = render(PriceCell, { props: { summary: summary({ caveat: 'promo' }) } });
    const caveat = /** @type {Element} */ (container.querySelector('span.block'));
    expect(caveat).not.toBeNull();
    expect(caveat.classList.contains('opacity-70')).toBe(false);
  });
});
