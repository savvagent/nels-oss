// Turns a competitor's `pricing` object into a render-agnostic descriptor the
// compare templates display. The free-tier / multi-tier / annual-only /
// undisclosed decisions live here so they are unit-tested once, instead of
// being hand-written per page where only a reader would notice them drifting.
//
// Like competitors.js and paths.js this module has ZERO imports and stays
// importable from plain node.
//
// Amounts are formatted as USD in every locale on purpose. These are US list
// prices read off US vendor pages; re-punctuating them for fr/de/it/pt without
// converting the currency would imply a local price that does not exist. The
// `$` therefore lives here and never in locale JSON — the i18n messages are
// shells like "{amount}/mo", which is what lets a price correction in
// competitors.js stay a one-line edit instead of six translation edits.

/** @typedef {{ name: string, monthly?: number, annual?: number, flat?: number }} Tier */
/** @typedef {{ model?: string, hasFreeTier?: boolean, tiers?: Tier[], note?: string }} Pricing */
/** @typedef {'undisclosed'|'exact'|'from'|'freeThen'|'freeThenFrom'} PricingVariant */
/**
 * @typedef {Object} PricingSummary
 * @property {PricingVariant} variant
 * @property {'monthly'|'annual'|null} period
 * @property {string|null} display - already formatted for display, e.g. '$5.99'
 * @property {string|null} annualDisplay - the same tier's annual price when it
 *   publishes one and `period` is monthly. Rendering the monthly figure alone
 *   is NOT the neutral choice it looks like: annual discounts run 20 to 63
 *   percent across this table and Nels' is the smallest of any product that
 *   offers one, so a monthly-only row would understate every competitor's real
 *   floor by more than it understates ours.
 * @property {'promo'|'secondHand'|'annualOnly'|null} caveat - i18n key suffix
 *   under `compare.note`
 */

/**
 * Format a USD amount. Whole dollars drop the decimals — $13, not $13.00 —
 * and anything else gets exactly two, so 950.4 renders $950.40. Thousands are
 * grouped, because Range's tiers are the first four-figure amounts the table
 * has had to render and $3950 reads as a typo next to $3,950.
 *
 * The grouping is en-US regardless of the visitor's locale, deliberately and
 * for the same reason the `$` is hardcoded above: these are US list prices, and
 * re-punctuating 3.950 for de/it/pt would imply a local price that isn't sold.
 * @param {number} n
 */
export function formatUsd(n) {
  return `$${n.toLocaleString('en-US', {
    minimumFractionDigits: Number.isInteger(n) ? 0 : 2,
    maximumFractionDigits: Number.isInteger(n) ? 0 : 2,
  })}`;
}

// Frozen because every undisclosed path returns this same object by reference.
/** @type {PricingSummary} */
const UNDISCLOSED = Object.freeze({
  variant: 'undisclosed',
  period: null,
  display: null,
  annualDisplay: null,
  caveat: null,
});

/**
 * Maps a `pricing.note` to its i18n key suffix under `compare.note`, or null
 * when the note adds nothing the cell does not already say. `undisclosed`
 * returns null because the undisclosed cell IS `compare.note.undisclosed`.
 * @param {string|undefined} note
 * @returns {'promo'|'secondHand'|'annualOnly'|null}
 */
function caveatFor(note) {
  if (note === 'promo') return 'promo';
  if (note === 'second-hand') return 'secondHand';
  // A monthly figure the vendor will not actually sell by the month. Without
  // this the cell reads as a month-to-month rate alongside competitors whose
  // monthly figures genuinely are, which is the same mixing of billing periods
  // `annualDisplay` exists to prevent.
  if (note === 'annual-only') return 'annualOnly';
  return null;
}

/**
 * Summarize a competitor's pricing as an entry price. The full tier ladder
 * stays on the vendor's own page, which every competitor links via sourceUrl.
 * @param {Pricing|null|undefined} pricing
 * @returns {PricingSummary}
 */
export function pricingSummary(pricing) {
  if (!pricing) return UNDISCLOSED;
  if (pricing.note === 'undisclosed' || pricing.model === 'undisclosed') return UNDISCLOSED;

  const tiers = pricing.tiers ?? [];

  // Monthly wins whenever any tier publishes one. Boldin publishes only an
  // annual figure, and 144/12 would invent a monthly rate it does not sell.
  const period = tiers.some((t) => typeof t.monthly === 'number')
    ? 'monthly'
    : tiers.some((t) => typeof t.annual === 'number')
      ? 'annual'
      : null;
  if (!period) return UNDISCLOSED;

  const priced = tiers.filter((t) => typeof (period === 'monthly' ? t.monthly : t.annual) === 'number');
  const amounts = /** @type {number[]} */ (
    priced.map((t) => (period === 'monthly' ? t.monthly : t.annual))
  );

  // The cheapest tier, not the first, so reordering competitors.js cannot
  // silently change what the page claims. Both figures below are read off THAT
  // tier, so the monthly and annual prices shown always belong to one plan
  // rather than being the best of each column.
  const entry = priced[amounts.indexOf(Math.min(...amounts))];
  const display = formatUsd(Math.min(...amounts));
  const annualDisplay =
    period === 'monthly' && typeof entry.annual === 'number' ? formatUsd(entry.annual) : null;

  // More than one tier means the entry price is a floor rather than the price.
  // Every tier counts, including one that publishes no figure at all: a
  // "contact us" plan is evidence the product can cost more than its cheapest
  // listed option, not evidence that it cannot. Boldin picks up the "from"
  // hedge on this rule — as `freeThenFrom`, since it is also freemium —
  // because of its flat advisor fee, which is never the headline amount: a
  // one-off fee is not an entry price.
  const many = tiers.length > 1;

  /** @type {PricingVariant} */
  const variant = pricing.hasFreeTier
    ? many
      ? 'freeThenFrom'
      : 'freeThen'
    : many
      ? 'from'
      : 'exact';

  return { variant, period, display, annualDisplay, caveat: caveatFor(pricing.note) };
}
