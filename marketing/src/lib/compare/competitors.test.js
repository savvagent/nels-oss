import { describe, it, expect } from 'vitest';
import { COMPETITORS, FEATURE_KEYS, SEGMENTS, findCompetitor } from './competitors.js';

describe('COMPETITORS schema', () => {
  it('has the eleven competitors from the spec', () => {
    expect(COMPETITORS).toHaveLength(11);
    expect(COMPETITORS.map((c) => c.slug).sort()).toEqual(
      ['boldin', 'chatgpt', 'cleo', 'copilot', 'era', 'everydollar', 'monarch', 'origin', 'range', 'simplifi', 'ynab']
    );
  });

  it('slugs are unique and URL-safe', () => {
    const slugs = COMPETITORS.map((c) => c.slug);
    expect(new Set(slugs).size).toBe(slugs.length);
    for (const s of slugs) expect(s).toMatch(/^[a-z0-9-]+$/);
  });

  it('every competitor has a closed, identical feature key set', () => {
    expect(FEATURE_KEYS).toHaveLength(11);
    for (const c of COMPETITORS) {
      expect(Object.keys(c.features).sort()).toEqual([...FEATURE_KEYS].sort());
    }
  });

  it('feature values are true, false, or partial', () => {
    for (const c of COMPETITORS) {
      for (const v of Object.values(c.features)) {
        expect([true, false, 'partial']).toContain(v);
      }
    }
  });

  it('verifiedOn is an ISO date that parses', () => {
    for (const c of COMPETITORS) {
      expect(c.verifiedOn).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      expect(Number.isNaN(Date.parse(c.verifiedOn))).toBe(false);
    }
  });

  it('url and sourceUrl are https', () => {
    for (const c of COMPETITORS) {
      expect(c.url.startsWith('https://')).toBe(true);
      expect(c.sourceUrl.startsWith('https://')).toBe(true);
    }
  });

  it('every competitor names a strength (the honesty rule)', () => {
    for (const c of COMPETITORS) {
      expect(typeof c.strength).toBe('string');
      expect(c.strength.length).toBeGreaterThan(0);
    }
  });

  it('segments are valid', () => {
    expect(SEGMENTS).toEqual(['ai-native', 'traditional', 'planning']);
    for (const c of COMPETITORS) expect(SEGMENTS).toContain(c.segment);
  });

  it('pricing shape is internally consistent', () => {
    for (const c of COMPETITORS) {
      expect(['freemium', 'subscription', 'flat-fee', 'undisclosed']).toContain(c.pricing.model);
      if (c.pricing.model === 'freemium') expect(c.pricing.hasFreeTier).toBe(true);
      if (c.pricing.model !== 'undisclosed') expect(c.pricing.tiers.length).toBeGreaterThan(0);
      if (c.pricing.note) expect(['promo', 'second-hand', 'undisclosed', 'annual-only']).toContain(c.pricing.note);
    }
  });
});

describe('findCompetitor', () => {
  it('returns undefined for an unknown slug', () => {
    expect(findCompetitor('nope')).toBeUndefined();
  });
  // Named for what it actually proves today, which is NOT the published gate.
  // It used to require an unpublished competitor to exist and threw otherwise;
  // Phase 3 (#451) published the last of the eleven, so that fixture is gone.
  // With `unpublished` empty the first loop is a no-op and only the identity
  // assertion runs — meaning you could delete `&& c.published` from
  // findCompetitor and this suite would stay green. That gap is real and is
  // the price of having no seam to inject a synthetic record; do not let the
  // test name imply otherwise. The gate re-arms for free the moment a twelfth
  // competitor is seeded ahead of its copy.
  it('resolves every published slug, and would reject an unpublished one', () => {
    const unpublished = COMPETITORS.filter((c) => !c.published);
    for (const c of unpublished) expect(findCompetitor(c.slug)).toBeUndefined();
    for (const c of COMPETITORS.filter((c) => c.published)) {
      expect(findCompetitor(c.slug)).toBe(c);
    }
  });
});
