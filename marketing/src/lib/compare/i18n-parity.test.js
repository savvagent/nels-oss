import { describe, it, expect } from 'vitest';
import { COMPETITORS, FEATURE_KEYS, SEGMENTS } from './competitors.js';
import en from '../i18n/locales/en.json';
import es from '../i18n/locales/es.json';
import fr from '../i18n/locales/fr.json';
import de from '../i18n/locales/de.json';
import itLocale from '../i18n/locales/it.json';
import pt from '../i18n/locales/pt.json';

const LOCALES = { es, fr, de, it: itLocale, pt };

/** @typedef {Record<string, string>} StringMap */
/**
 * Loose shape for the `compare` section of a locale file: the three nested
 * dictionaries are keyed dynamically (by FEATURE_KEYS/SEGMENTS/NOTE_KEYS), and
 * the rest of the section is indexed by arbitrary chrome keys or checked for
 * absence (e.g. the retired `row1`). Intersecting with `Record<string,
 * unknown>` lets both kinds of dynamic access type-check without widening the
 * three nested dictionaries themselves past `Record<string, string>`.
 * @typedef {{ feature: StringMap, segment: StringMap, note: StringMap, pricing: StringMap, seo: { title: string, description: string } } & Record<string, unknown>} CompareLocale
 */

/** @type {Record<string, string>} */
const SEGMENT_KEY = { 'ai-native': 'aiNative', traditional: 'traditional', planning: 'planning' };
const NOTE_KEYS = ['promo', 'secondHand', 'undisclosed', 'annualOnly'];
const PRICING_KEYS = ['label', 'monthly', 'annual', 'from', 'freeThen', 'freeThenFrom', 'orAnnual'];

// Which placeholder each shell must keep. `label` is the row header and
// interpolates nothing, so it is deliberately absent.
const PRICING_PLACEHOLDERS = {
  monthly: '{amount}',
  annual: '{amount}',
  from: '{price}',
  freeThen: '{price}',
  freeThenFrom: '{price}',
  orAnnual: '{price}',
};
const CHROME = ['verifiedOn', 'colFeature', 'colNels', 'hubIntro', 'spreadsheetRow', 'viewComparison'];

/** @type {CompareLocale} */
const enCompare = en.compare;

describe('compare base keys', () => {
  it('en has a label for every feature key', () => {
    for (const k of FEATURE_KEYS) expect(typeof enCompare.feature[k]).toBe('string');
  });

  it('en has a heading for every segment', () => {
    for (const s of SEGMENTS) expect(typeof enCompare.segment[SEGMENT_KEY[s]]).toBe('string');
  });

  it('en has the pricing-caveat notes', () => {
    for (const k of NOTE_KEYS) expect(typeof enCompare.note[k]).toBe('string');
  });

  // The pricing row interpolates its amounts from competitors.js and SITE at
  // render time; the locale files hold only the format shells. Both halves are
  // gated: every shell exists in every locale, and no shell smuggles a
  // currency symbol back into JSON.
  for (const [code, dict] of Object.entries({ en, ...LOCALES })) {
    it(`${code}: has every pricing format key, non-empty`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      for (const k of PRICING_KEYS) {
        expect(typeof dictCompare.pricing[k]).toBe('string');
        expect(dictCompare.pricing[k].length).toBeGreaterThan(0);
      }
    });

    it(`${code}: pricing format shells carry no currency symbol`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      // Stricter than the CURRENCY_DIGIT regex further down, deliberately:
      // these shells hold a placeholder rather than a digit, so a hardcoded
      // "{amount} $" would slip past a digit-adjacency check while still
      // pinning a currency the formatter has already applied.
      expect(/[$€£]/.test(JSON.stringify(dictCompare.pricing))).toBe(false);
    });

    it(`${code}: every pricing shell keeps its interpolation placeholder`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      // A shell that loses its placeholder is the quiet failure mode here:
      // "{amount}/mo" translated as plain "/mo" still passes the presence and
      // no-currency gates above, and still renders — as a period suffix with
      // no price in front of it. The figure would vanish from a live page in
      // that locale only, which is exactly the kind of thing nobody notices.
      for (const [k, placeholder] of Object.entries(PRICING_PLACEHOLDERS)) {
        expect(dictCompare.pricing[k]).toContain(placeholder);
      }
    });
  }

  // Moved out of the en-only test above: the CHROME loop used to run against
  // `en` alone, so a chrome-key drop in es/fr/de/it/pt would go undetected.
  // Looping over every locale (including en) here closes that gap.
  for (const [code, dict] of Object.entries({ en, ...LOCALES })) {
    it(`${code}: has the page chrome keys`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      for (const k of CHROME) expect(typeof dictCompare[k]).toBe('string');
    });
  }

  // Regression guard: en's meta tags fit the length budget by construction,
  // but nothing previously checked the translations. es/fr/it/pt have all
  // blown past 155 chars on `seo.description` before — keep every locale
  // inside Google's practical title/description budget.
  for (const [code, dict] of Object.entries({ en, ...LOCALES })) {
    it(`${code}: seo.title is at most 60 characters`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      expect(dictCompare.seo.title.length).toBeLessThanOrEqual(60);
    });

    it(`${code}: seo.description is at most 155 characters`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      expect(dictCompare.seo.description.length).toBeLessThanOrEqual(155);
    });
  }

  it('drops the retired generic rows', () => {
    expect(enCompare.row1).toBeUndefined();
  });

  for (const [code, dict] of Object.entries(LOCALES)) {
    it(`${code}: has every base key, translated (not the English fallback)`, () => {
      /** @type {CompareLocale} */
      const dictCompare = dict.compare;
      for (const k of FEATURE_KEYS) {
        expect(typeof dictCompare.feature[k]).toBe('string');
        expect(dictCompare.feature[k].length).toBeGreaterThan(0);
      }
      for (const s of SEGMENTS) expect(typeof dictCompare.segment[SEGMENT_KEY[s]]).toBe('string');
      for (const k of NOTE_KEYS) expect(typeof dictCompare.note[k]).toBe('string');
      expect(dictCompare.row1).toBeUndefined();
    });
  }
});

// The sixteen copy keys a deep-dive page needs to render without leaking raw
// i18n keys (`compare.ynab.seo.title`, etc.) to a visitor. Dot-paths resolve
// under `compare.<slug>.…` in each locale file.
const REQUIRED_COMPETITOR_KEYS = [
  'seo.title',
  'seo.description',
  'headline',
  'intro',
  'whoForTitle',
  'whoFor',
  'strengthTitle',
  'strength',
  'verdictTitle',
  'verdict',
  'faq.q1',
  'faq.a1',
  'faq.q2',
  'faq.a2',
  'faq.q3',
  'faq.a3',
];

/**
 * Resolve a dot path (e.g. 'seo.title') against a nested object of unknown
 * shape, returning `undefined` if any segment is missing.
 * @param {unknown} obj
 * @param {string} path
 * @returns {unknown}
 */
function resolvePath(obj, path) {
  return path.split('.').reduce((acc, key) => {
    if (acc && typeof acc === 'object' && key in acc) {
      return /** @type {Record<string, unknown>} */ (acc)[key];
    }
    return undefined;
  }, obj);
}

const ALL_LOCALES = { en, ...LOCALES };

describe('compare deep-dive copy (Phase 2 arming gate)', () => {
  // The per-competitor × per-locale checks below are intentionally vacuous
  // today. Phase 1 ships with every competitor's `published` flag `false`
  // (see competitors.test.js), so `published` is `[]` and the nested loops
  // register zero `it`s. That is correct, not dead weight: it is a gate that
  // arms itself the moment Phase 2 (issue #450) flips a competitor's
  // `published` to `true`. From that point on, every locale must have real
  // copy for that competitor's sixteen required keys, or the build fails
  // instead of shipping a page that renders raw `compare.<slug>.*` message
  // keys to a visitor. Do NOT delete this suite because it "does nothing" —
  // it does nothing *yet*, by design.
  //
  // Three more checks below extend the same arming gate, also vacuous until
  // the first `published: true`: per-locale deep-dive meta-tag lengths, a
  // no-hardcoded-price scan of each competitor's locale block, and a
  // non-English-must-not-equal-English check on the long-form prose keys.
  it('REQUIRED_COMPETITOR_KEYS covers the sixteen keys the deep-dive page reads', () => {
    // A describe block with zero `it`s is a Vitest error, not a pass, so this
    // always-present assertion is also what keeps the suite structurally
    // valid while `published` is empty.
    expect(REQUIRED_COMPETITOR_KEYS).toHaveLength(16);
  });

  const published = COMPETITORS.filter((c) => c.published);

  for (const c of published) {
    for (const [code, dict] of Object.entries(ALL_LOCALES)) {
      it(`${code}: ${c.slug} has every required copy key, non-empty`, () => {
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        for (const key of REQUIRED_COMPETITOR_KEYS) {
          const value = resolvePath(root, key);
          expect(typeof value).toBe('string');
          expect(/** @type {string} */ (value).length).toBeGreaterThan(0);
        }
      });

      it(`${code}: ${c.slug}'s strength field resolves to non-empty copy`, () => {
        // c.strength is a fully-qualified key, e.g. 'compare.ynab.strength' —
        // resolve it from the locale root, not from compare.<slug>.
        const value = resolvePath(dict, c.strength);
        expect(typeof value).toBe('string');
        expect(/** @type {string} */ (value).length).toBeGreaterThan(0);
      });
    }
  }

  // Same length budget the hub's compare.seo.* keys already get (see the
  // `compare base keys` describe above), applied to each published
  // competitor's own deep-dive meta tags, in every locale. German and French
  // routinely blow past these limits on hand-written or lightly-edited
  // machine translations, so this is not a theoretical check.
  for (const c of published) {
    for (const [code, dict] of Object.entries(ALL_LOCALES)) {
      it(`${code}: ${c.slug}'s seo.title is at most 60 characters`, () => {
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        const value = resolvePath(root, 'seo.title');
        expect(typeof value).toBe('string');
        expect(/** @type {string} */ (value).length).toBeLessThanOrEqual(60);
      });

      it(`${code}: ${c.slug}'s seo.description is at most 155 characters`, () => {
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        const value = resolvePath(root, 'seo.description');
        expect(typeof value).toBe('string');
        expect(/** @type {string} */ (value).length).toBeLessThanOrEqual(155);
      });
    }
  }

  // Prices live in competitors.js and are interpolated into the rendered
  // page (see the file header there); they must never be written directly
  // into locale JSON, or a price correction in competitors.js would go stale
  // in the copy without anything failing. Match a currency symbol
  // immediately (optionally one space) adjacent to a digit in EITHER order —
  // `$20`, `€ 15`, `£10` (symbol-first, the English convention) as well as
  // `20 €`, `3€`, `20€/mo` (amount-first, the convention French, German,
  // Italian, and Portuguese copy normally uses) — rather than bare digits,
  // so ordinary prose like "12,000 institutions", "16 months", or a bare
  // year like "2026" doesn't false-positive.
  const CURRENCY_DIGIT = /[$€£]\s?\d|\d\s?[$€£]/;

  for (const c of published) {
    for (const [code, dict] of Object.entries(ALL_LOCALES)) {
      it(`${code}: ${c.slug}'s copy has no hardcoded currency amount`, () => {
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        expect(CURRENCY_DIGIT.test(JSON.stringify(root))).toBe(false);
      });
    }
  }

  // Prose must not name a currency other than the dollar. The no-hardcoded-
  // amount check above only catches a symbol next to a digit, so it sails past
  // the failure this catches: a translator rendering "give every dollar a job"
  // as "chaque euro", "jedem Euro", "ogni euro" or "a cada real" — which all
  // four of es/fr/de/it/pt did on the first pass of the Phase 3 copy. Prices in
  // this table are US list prices formatted as USD in every locale on purpose
  // (see the header of pricing.js), so localizing the currency in the prose
  // states a price that is not sold and contradicts the figure rendered
  // directly above it.
  //
  // Two patterns rather than one, because "real"/"reais" is both Brazil's
  // currency and an everyday Portuguese and Spanish adjective — "as perguntas
  // reais" is prose, "a cada real" is a price. Matching the bare word would
  // fail correct copy, so the ambiguous pair is caught only behind a
  // quantifier, where it can only be the currency. The unambiguous names need
  // no such hedge.
  // A bare symbol is the cheapest and broadest signal: the amount check above
  // only fires when one sits next to a digit, so "a few \u20ac a month" slips past it.
  const CURRENCY_SYMBOL = /[\u20ac\u00a3\u00a5\u20b9]/;
  // Spelled-out names. `yen(es)?` not `yenes?` — the latter matches "yene" and
  // "yenes" but never the actual word "yen". `franc` covers French franc/francs
  // as well as es/pt franco/francos; \b keeps it out of "franchise".
  const FOREIGN_CURRENCY =
    /\b(euros?|EUR|libras?|sterlin[ae]|sterling|francs?|francos?|rupias?|yen(es)?|pfund)\b/i;
  // "real"/"reais"/"reales" is Brazil's currency AND an everyday pt/es
  // adjective — "as perguntas reais" is prose, "a cada real" is a price. So it
  // is caught only behind a quantifier, where it can only be the currency.
  // Quantifiers cover both languages, since es made this mistake too.
  const QUANTIFIED_REAL =
    /\b(cada|alguns?|algunos?|poucos?|pocos?|umas?|unas?|uns|dois|duas|dos|mil)\s+(reais|reales|real)\b/i;

  for (const c of published) {
    for (const [code, dict] of Object.entries(ALL_LOCALES)) {
      it(`${code}: ${c.slug}'s copy names no currency but the dollar`, () => {
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        const blob = JSON.stringify(root);
        expect(CURRENCY_SYMBOL.test(blob)).toBe(false);
        expect(FOREIGN_CURRENCY.test(blob)).toBe(false);
        expect(QUANTIFIED_REAL.test(blob)).toBe(false);
      });
    }
  }

  // Non-English long-form prose must actually be translated, not the English
  // string surviving untouched in the locale file. Deliberately restricted
  // to long-form keys: `headline`, `intro`, `whoFor`, `strength`, `verdict`,
  // and the three `faq.a*` answers. Short keys — `whoForTitle`,
  // `strengthTitle`, `verdictTitle`, `seo.title`, `faq.q*` — are excluded on
  // purpose: a short heading or question can legitimately read identically
  // across languages (shared loanwords, brand names, a question shape that
  // just happens to match), and a strict inequality check there would fail
  // a correct translation, not catch a bad one. Do not "fix" this by adding
  // short keys to the list.
  const PROSE_FALLBACK_KEYS = ['headline', 'intro', 'whoFor', 'strength', 'verdict', 'faq.a1', 'faq.a2', 'faq.a3'];

  for (const c of published) {
    for (const [code, dict] of Object.entries(LOCALES)) {
      it(`${code}: ${c.slug}'s prose copy is translated, not the English fallback`, () => {
        const enRoot = enCompare[c.slug];
        /** @type {CompareLocale} */
        const dictCompare = dict.compare;
        const root = dictCompare[c.slug];
        for (const key of PROSE_FALLBACK_KEYS) {
          const enValue = resolvePath(enRoot, key);
          const value = resolvePath(root, key);
          expect(typeof value).toBe('string');
          expect(value).not.toBe(enValue);
        }
      });
    }
  }
});
