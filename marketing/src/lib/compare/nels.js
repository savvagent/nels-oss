// Values shared by the compare hub and the deep-dive pages. Kept out of
// competitors.js because that module must stay import-free for svelte.config.js;
// this one is component-only, so it may read SITE.
import { SITE } from '$lib/site.js';

/** @typedef {import('./competitors.js').FeatureValue} FeatureValue */

/**
 * Nels' own row in the feature matrix. Keys mirror FEATURE_KEYS exactly.
 * @type {Record<string, FeatureValue>}
 */
export const NELS_FEATURES = {
  conversationalLogging: true,
  writableLedger: true,
  envelopeBudgeting: true,
  householdSharing: true,
  auditLog: true,
  bankSync: 'partial', // Pro tier only
  passwordless: true,
  aiInsights: true,
  goals: true,
  humanAdvisors: false,
  retirementProjection: false,
};

/**
 * Maps a competitor's `segment` to its i18n key suffix.
 * @type {Record<string, string>}
 */
export const SEGMENT_KEY = {
  'ai-native': 'aiNative',
  traditional: 'traditional',
  planning: 'planning',
};

/**
 * Renders a feature value as a table mark.
 * @param {FeatureValue} v
 */
export const mark = (v) => (v === true ? '✓' : v === 'partial' ? '~' : '—');

/**
 * i18n key for the text alternative of a feature mark. The glyph from
 * `mark()` is visual-only (aria-hidden); screen readers get this instead.
 * @param {FeatureValue} v
 */
export const markLabel = (v) =>
  v === true ? 'compare.mark.yes' : v === 'partial' ? 'compare.mark.partial' : 'compare.mark.no';

/** Nels' entry price, for copy that contrasts cost. */
export const NELS_ENTRY_PRICE = SITE.plans.basic.monthly;

/**
 * The date a group of competitors was collectively verified, for the line that
 * sits under each hub table. Oldest wins: if these ever diverge, the honest
 * claim for a table as a whole is its stalest figure, not its freshest. ISO
 * dates sort lexicographically, so a plain sort is chronological. Returns
 * undefined for an empty group, which the template guards rather than printing
 * "as of undefined".
 * @param {{ verifiedOn: string }[]} items
 * @returns {string|undefined}
 */
export function oldestVerifiedOn(items) {
  return items.map((c) => c.verifiedOn).sort()[0];
}

/**
 * Nels' own cell in the pricing row, in the same descriptor shape
 * `pricingSummary()` returns for a competitor, so both take the identical
 * path through PriceCell.
 *
 * `display` is NELS_ENTRY_PRICE, which is already a formatted string, so it
 * deliberately bypasses `formatUsd`. The variant is derived from the number of
 * plans rather than hand-set: with Basic and Pro both live, an unqualified
 * entry price would read as Nels' only price and tilt the comparison our way
 * against competitors whose cells say "from".
 *
 * Known limitation, inherited from NELS_ENTRY_PRICE: `display` is pinned to
 * the Basic plan while the variant counts every plan, so a plan cheaper than
 * Basic would make this stale.
 *
 * @type {import('./pricing.js').PricingSummary}
 */
export const NELS_PRICING_SUMMARY = {
  variant: Object.keys(SITE.plans).length > 1 ? 'from' : 'exact',
  period: 'monthly',
  display: NELS_ENTRY_PRICE,
  // Nels shows its annual price for the same reason every competitor does.
  // Ours is the SMALLEST annual discount in the table at 17 percent, against
  // 20 to 63 percent elsewhere, so omitting annual pricing across the board
  // would have quietly flattered us most.
  annualDisplay: SITE.plans.basic.annual,
  caveat: null,
};
