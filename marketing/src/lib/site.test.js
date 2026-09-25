import { describe, it, expect } from 'vitest';
import { SITE, NAV } from './site.js';
import en from './i18n/locales/en.json';
import es from './i18n/locales/es.json';
import fr from './i18n/locales/fr.json';
import de from './i18n/locales/de.json';
import it_ from './i18n/locales/it.json';
import pt from './i18n/locales/pt.json';

describe('SITE two-tier pricing', () => {
  it('exposes Basic and Pro plans, each with monthly + annual', () => {
    expect(SITE.plans.basic.monthly).toBe('$3');
    expect(SITE.plans.basic.annual).toBe('$30');
    expect(SITE.plans.pro.monthly).toBe('$5');
    expect(SITE.plans.pro.annual).toBe('$50');
  });
  it('keeps `price` as the entry (Basic) price so "from" copy interpolates correctly', () => {
    expect(SITE.price.monthly).toBe(SITE.plans.basic.monthly);
    expect(SITE.price.annual).toBe(SITE.plans.basic.annual);
  });
  it('annual is a genuine 2-months-free discount (unlike the old single-plan $36)', () => {
    const n = (s) => Number(s.replace('$', ''));
    for (const p of [SITE.plans.basic, SITE.plans.pro]) {
      // 12 months billed as 10 → two months free.
      expect(n(p.annual)).toBe(n(p.monthly) * 10);
    }
  });
});

describe('en.json two-tier pricing/features copy', () => {
  it('pricing page names the bank-syncing benefit (the Pro differentiator)', () => {
    expect(en.pricing.included7).toMatch(/bank/i);
    expect(en.pricing.included7).toMatch(/sync/i);
  });
  it('pricing page carries the tier names, taglines, and Pro upsell copy', () => {
    expect(en.pricing.tierBasicName).toBeTruthy();
    expect(en.pricing.tierProName).toBeTruthy();
    expect(en.pricing.everythingInBasic).toBeTruthy();
    expect(en.pricing.bankSyncExcluded).toMatch(/sync/i);
  });
  it('features page describes the bank-syncing pillar', () => {
    expect(en.features.pillar5.h).toMatch(/sync/i);
    expect(en.features.pillar5.body).toMatch(/link/i);
    expect(en.features.pillar5.body).toMatch(/automatic/i);
    expect(en.features.pillar5.body).toMatch(/manual entry/i);
  });
  it('features pillar5 body interpolates the price instead of hardcoding it', () => {
    expect(en.features.pillar5.body).toContain('{monthly}');
    expect(en.features.pillar5.body).not.toMatch(/\$\d/);
  });
});

/** @type {Array<[string, typeof es]>} */
const LOCALES = [
  ['es', es],
  ['fr', fr],
  ['de', de],
  ['it', it_],
  ['pt', pt],
];

// The repo convention (see the _meta.machineTranslated flag + prior PR reviews):
// new user-facing copy must be translated into every locale, not left to the
// English fallback. This regression test closes that gap for the two-tier copy.
describe('every locale carries its own translated copy (not the English fallback)', () => {
  for (const [locale, dict] of LOCALES) {
    it(`${locale}: bank-sync + two-tier keys are translated, not English`, () => {
      for (const k of [
        'included7',
        'tierBasicTagline',
        'tierProTagline',
        'bankSyncExcluded',
        'everythingInBasic',
      ]) {
        expect(dict.pricing[k], `${locale}.pricing.${k} missing`).toBeTruthy();
        expect(dict.pricing[k], `${locale}.pricing.${k} must differ from en`).not.toBe(en.pricing[k]);
      }
      expect(dict.features.pillar5.body).not.toBe(en.features.pillar5.body);
    });
    it(`${locale}: features.pillar5.body interpolates the price instead of hardcoding it`, () => {
      expect(dict.features.pillar5.body).toContain('{monthly}');
      expect(dict.features.pillar5.body).not.toMatch(/\$\d/);
    });
  }
});

describe('compare page is reachable from navigation (#445)', () => {
  it('NAV includes /compare', () => {
    expect(NAV.map((n) => n.path)).toContain('/compare');
  });
  it('the compare nav entry uses an i18n key', () => {
    const entry = NAV.find((n) => n.path === '/compare');
    expect(entry?.key).toBe('nav.compare');
  });
});
