# Comparison Pages — Plan 1: Infrastructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the data module, dynamic route, segmented hub, and anti-drift wiring for named-competitor comparison pages — shipping with zero deep dives published.

**Architecture:** All competitor facts live once in a dependency-free `competitors.js`. A sibling `paths.js` derives the localized page paths from it, and both `svelte.config.js` (prerender entries) and the sitemap import that helper so they cannot drift. A single `[competitor]` dynamic route 404s on unknown or unpublished slugs. The hub renders a segmented grid from the data.

**Tech Stack:** SvelteKit 2, Svelte 5 runes, Tailwind + DaisyUI, svelte-i18n, vitest (jsdom), pnpm.

**Reference spec:** `docs/superpowers/specs/2026-07-27-competitor-comparison-pages-design.md`

## Global Constraints

- Marketing uses **pnpm**, not npm: `cd marketing && pnpm test && pnpm run build && pnpm check`.
- **No Svelte component tests.** vitest here is jsdom but the project convention is pure-helper tests only (`marketing/src/lib/*.test.js`). Test helpers and data, not components.
- **`competitors.js` and `paths.js` must have zero imports** beyond each other. `svelte.config.js` runs in plain node and cannot resolve the `$lib` alias. Nels' own feature row lives in `nels.js`, which is component-only and MAY import `SITE` — never inside `competitors.js`.
- Prices in locale JSON are forbidden — they interpolate from the data module.
- Every competitor record requires a non-empty `strength` (the "what they do better" honesty rule, spec §7).
- Feature values are exactly `true | false | 'partial'`. The `features` key set is closed and identical for every competitor.
- Six locales: `en`, `es`, `fr`, `de`, `it`, `pt`. Non-English additions carry the existing `_meta.machineTranslated` convention.
- No self-attribution in commits.

---

## File Structure

- **Create:** `marketing/src/lib/compare/competitors.js` — the eleven competitor records, `FEATURE_KEYS`, `SEGMENTS`, `findCompetitor()`. Zero imports.
- **Create:** `marketing/src/lib/compare/competitors.test.js` — schema integrity tests.
- **Create:** `marketing/src/lib/compare/paths.js` — `comparePaths()`, `publishedCompetitors()`. Zero imports.
- **Create:** `marketing/src/lib/compare/paths.test.js` — prerender/sitemap parity tests.
- **Create:** `marketing/src/lib/compare/nels.js` — Nels' own feature row, `mark()`, `SEGMENT_KEY`. Imported by components only, so it MAY import `SITE`.
- **Create:** `marketing/src/routes/[lang]/compare/[competitor]/+page.js` — slug validation, 404.
- **Create:** `marketing/src/routes/[lang]/compare/[competitor]/+page.svelte` — deep-dive template.
- **Modify:** `marketing/src/routes/[lang]/compare/+page.svelte` — rebuild as segmented hub.
- **Modify:** `marketing/svelte.config.js:8` — derive entries from `comparePaths()`.
- **Modify:** `marketing/src/routes/sitemap.xml/+server.js:9` — derive localized pages from `comparePaths()`.
- **Modify:** `marketing/src/lib/site.js:24` — add `/compare` to `NAV`.
- **Modify:** `marketing/src/lib/components/Footer.svelte` — link `/compare`.
- **Modify:** `marketing/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json` — `compare.*` keys.
- **Create:** `marketing/src/lib/compare/i18n-parity.test.js` — locale coverage.

---

### Task 1: Competitor data module and schema tests

**Files:**
- Create: `marketing/src/lib/compare/competitors.js`
- Test: `marketing/src/lib/compare/competitors.test.js`

**Interfaces:**
- Consumes: nothing.
- Produces: `COMPETITORS` (array), `FEATURE_KEYS` (string[11]), `SEGMENTS` (string[3]), `findCompetitor(slug) → competitor | undefined` (returns `undefined` for unknown **and** unpublished slugs).

- [ ] **Step 1: Write the failing test**

Create `marketing/src/lib/compare/competitors.test.js`:

```js
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
      if (c.pricing.note) expect(['promo', 'second-hand', 'undisclosed']).toContain(c.pricing.note);
    }
  });

  it('ships Phase 1 with nothing published', () => {
    for (const c of COMPETITORS) expect(c.published).toBe(false);
  });
});

describe('findCompetitor', () => {
  it('returns undefined for an unknown slug', () => {
    expect(findCompetitor('nope')).toBeUndefined();
  });
  it('returns undefined for a known but unpublished slug', () => {
    expect(findCompetitor('ynab')).toBeUndefined();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd marketing && pnpm vitest run src/lib/compare/competitors.test.js`
Expected: FAIL — "Failed to resolve import ./competitors.js"

- [ ] **Step 3: Write the data module**

Create `marketing/src/lib/compare/competitors.js`. **No imports in this file.**

```js
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

export const SEGMENTS = ['ai-native', 'traditional', 'planning'];

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

export const COMPETITORS = [
  {
    slug: 'chatgpt',
    name: 'Finances in ChatGPT',
    segment: 'ai-native',
    url: 'https://openai.com',
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
      passwordless: false,
      aiInsights: true,
      goals: false,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.chatgpt.strength',
    sourceUrl: 'https://openai.com/index/personal-finance-chatgpt/',
    verifiedOn: '2026-07-27',
    published: false,
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
      note: 'second-hand',
    },
    features: {
      conversationalLogging: true,
      writableLedger: 'partial',
      envelopeBudgeting: false,
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: true,
      goals: 'partial',
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.cleo.strength',
    sourceUrl: 'https://web.meetcleo.com/pricing',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'era',
    name: 'Era',
    segment: 'ai-native',
    url: 'https://www.era.app',
    pricing: { model: 'undisclosed', hasFreeTier: true, tiers: [], note: 'undisclosed' },
    features: {
      conversationalLogging: 'partial',
      writableLedger: false,
      envelopeBudgeting: false,
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: true,
      goals: false,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.era.strength',
    sourceUrl: 'https://www.era.app',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'origin',
    name: 'Origin',
    segment: 'ai-native',
    url: 'https://useorigin.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Origin', annual: 1 }],
      note: 'promo',
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: 'partial',
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: true,
      goals: true,
      humanAdvisors: false,
      retirementProjection: 'partial',
    },
    strength: 'compare.origin.strength',
    sourceUrl: 'https://useorigin.com',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'copilot',
    name: 'Copilot Money',
    segment: 'ai-native',
    url: 'https://copilot.money',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Copilot', monthly: 13 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: 'partial',
      householdSharing: false,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: true,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.copilot.strength',
    sourceUrl: 'https://copilot.money/pricing',
    verifiedOn: '2026-07-27',
    published: false,
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
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: false,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.ynab.strength',
    sourceUrl: 'https://www.ynab.com/pricing',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'monarch',
    name: 'Monarch Money',
    segment: 'traditional',
    url: 'https://www.monarchmoney.com',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Premium', monthly: 14.99, annual: 99.99 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: 'partial',
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.monarch.strength',
    sourceUrl: 'https://www.monarchmoney.com/pricing',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'simplifi',
    name: 'Quicken Simplifi',
    segment: 'traditional',
    url: 'https://www.quicken.com/simplifi',
    pricing: {
      model: 'subscription',
      hasFreeTier: false,
      tiers: [{ name: 'Simplifi', monthly: 5.99, annual: 47.88 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      householdSharing: 'partial',
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: 'partial',
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.simplifi.strength',
    sourceUrl: 'https://www.quicken.com/simplifi',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'everydollar',
    name: 'EveryDollar',
    segment: 'traditional',
    url: 'https://www.ramseysolutions.com/ramseyplus/everydollar',
    pricing: {
      model: 'freemium',
      hasFreeTier: true,
      tiers: [{ name: 'Premium', monthly: 17.99, annual: 79.99 }],
    },
    features: {
      conversationalLogging: false,
      writableLedger: true,
      envelopeBudgeting: true,
      householdSharing: true,
      auditLog: false,
      bankSync: 'partial',
      passwordless: false,
      aiInsights: false,
      goals: true,
      humanAdvisors: false,
      retirementProjection: false,
    },
    strength: 'compare.everydollar.strength',
    sourceUrl: 'https://www.ramseysolutions.com/ramseyplus/everydollar',
    verifiedOn: '2026-07-27',
    published: false,
  },
  {
    slug: 'range',
    name: 'Range',
    segment: 'planning',
    url: 'https://www.range.com',
    pricing: { model: 'flat-fee', hasFreeTier: false, tiers: [{ name: 'Premium' }, { name: 'Platinum' }, { name: 'Titanium' }], note: 'undisclosed' },
    features: {
      conversationalLogging: false,
      writableLedger: false,
      envelopeBudgeting: false,
      householdSharing: true,
      auditLog: false,
      bankSync: true,
      passwordless: false,
      aiInsights: true,
      goals: true,
      humanAdvisors: true,
      retirementProjection: true,
    },
    strength: 'compare.range.strength',
    sourceUrl: 'https://www.range.com/pricing',
    verifiedOn: '2026-07-27',
    published: false,
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
      passwordless: false,
      aiInsights: true,
      goals: true,
      humanAdvisors: true,
      retirementProjection: true,
    },
    strength: 'compare.boldin.strength',
    sourceUrl: 'https://www.boldin.com/retirement/pricing/',
    verifiedOn: '2026-07-27',
    published: false,
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd marketing && pnpm vitest run src/lib/compare/competitors.test.js`
Expected: PASS (11 tests)

- [ ] **Step 5: Commit**

```bash
git add marketing/src/lib/compare/competitors.js marketing/src/lib/compare/competitors.test.js
git commit -m "feat(marketing): add competitor data module for comparison pages"
```

---

### Task 2: Path derivation and anti-drift wiring

**Files:**
- Create: `marketing/src/lib/compare/paths.js`
- Test: `marketing/src/lib/compare/paths.test.js`
- Modify: `marketing/svelte.config.js:8`
- Modify: `marketing/src/routes/sitemap.xml/+server.js:9`

**Interfaces:**
- Consumes: `COMPETITORS` from Task 1.
- Produces: `publishedCompetitors() → competitor[]`, `comparePaths() → string[]` (e.g. `['/compare/ynab']`, locale-independent, published only).

- [ ] **Step 1: Write the failing test**

Create `marketing/src/lib/compare/paths.test.js`:

```js
import { describe, it, expect } from 'vitest';
import { COMPETITORS } from './competitors.js';
import { comparePaths, publishedCompetitors } from './paths.js';
import { LOCALIZED_PAGES } from '../../routes/sitemap.xml/+server.js';

describe('comparePaths', () => {
  it('emits one locale-independent path per published competitor', () => {
    const published = COMPETITORS.filter((c) => c.published);
    expect(comparePaths()).toEqual(published.map((c) => `/compare/${c.slug}`));
  });

  it('excludes unpublished competitors', () => {
    const unpublished = COMPETITORS.filter((c) => !c.published).map((c) => c.slug);
    for (const slug of unpublished) {
      expect(comparePaths()).not.toContain(`/compare/${slug}`);
    }
  });

  it('publishedCompetitors returns only published records', () => {
    expect(publishedCompetitors().every((c) => c.published)).toBe(true);
  });
});

describe('the data modules stay $lib-free', () => {
  // svelte.config.js imports these from plain node, where `$lib` does not
  // resolve. A stray `import { SITE } from '$lib/site.js'` would break the
  // build in a way that is confusing to diagnose, so assert it can't happen.
  it('competitors.js and paths.js import nothing outside their own directory', async () => {
    const fs = await import('node:fs/promises');
    for (const f of ['competitors.js', 'paths.js']) {
      const src = await fs.readFile(new URL(f, import.meta.url), 'utf8');
      const imports = [...src.matchAll(/^\s*import\s.*?from\s+'([^']+)'/gm)].map((m) => m[1]);
      for (const spec of imports) expect(spec.startsWith('./')).toBe(true);
    }
  });
});

describe('sitemap parity (anti-drift)', () => {
  it('the sitemap covers every published competitor path', () => {
    for (const p of comparePaths()) expect(LOCALIZED_PAGES).toContain(p);
  });

  it('the sitemap lists no unpublished competitor path', () => {
    const unpublished = COMPETITORS.filter((c) => !c.published).map((c) => `/compare/${c.slug}`);
    for (const p of unpublished) expect(LOCALIZED_PAGES).not.toContain(p);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd marketing && pnpm vitest run src/lib/compare/paths.test.js`
Expected: FAIL — cannot resolve `./paths.js`, and `LOCALIZED_PAGES` is not exported

- [ ] **Step 3: Write `paths.js` and wire both consumers**

Create `marketing/src/lib/compare/paths.js` (**no imports beyond the sibling data module**):

```js
// Derives the comparison page paths from the competitor data so the prerender
// matrix (svelte.config.js) and the sitemap can never drift from each other.
// Like competitors.js, this file must stay `$lib`-free — svelte.config.js
// imports it from plain node.
import { COMPETITORS } from './competitors.js';

/** Competitors whose copy has landed and whose page should be live. */
export function publishedCompetitors() {
  return COMPETITORS.filter((c) => c.published);
}

/** Locale-independent deep-dive paths, e.g. ['/compare/ynab']. */
export function comparePaths() {
  return publishedCompetitors().map((c) => `/compare/${c.slug}`);
}
```

In `marketing/src/routes/sitemap.xml/+server.js`, export the page list and append the derived paths:

```js
import { comparePaths } from '$lib/compare/paths.js';

// Exported so a unit test can assert parity with the competitor data.
export const LOCALIZED_PAGES = [
  '', '/features', '/how-it-works', '/pricing', '/security', '/compare', '/about', '/faq',
  ...comparePaths(),
];
```

In `marketing/svelte.config.js`, replace the hand-maintained `PAGES` tail with the derived paths:

```js
import { comparePaths } from './src/lib/compare/paths.js';

const LOCALES = ['en', 'es', 'fr', 'de', 'it', 'pt'];
const PAGES = [
  '', '/features', '/how-it-works', '/pricing', '/security', '/compare', '/about', '/faq',
  ...comparePaths(),
];
const localeEntries = LOCALES.flatMap((l) => PAGES.map((p) => `/${l}${p}`));
```

- [ ] **Step 4: Run tests and build to verify**

Run: `cd marketing && pnpm vitest run src/lib/compare/ && pnpm run build`
Expected: tests PASS; build succeeds. With nothing published yet, the prerender count is unchanged from before this task.

- [ ] **Step 5: Commit**

```bash
git add marketing/src/lib/compare/paths.js marketing/src/lib/compare/paths.test.js \
        marketing/svelte.config.js marketing/src/routes/sitemap.xml/+server.js
git commit -m "feat(marketing): derive compare prerender entries and sitemap from competitor data"
```

---

### Task 3: Base i18n keys for the hub and deep-dive chrome

**Files:**
- Modify: `marketing/src/lib/i18n/locales/en.json`
- Modify: `marketing/src/lib/i18n/locales/{es,fr,de,it,pt}.json`
- Test: `marketing/src/lib/compare/i18n-parity.test.js`

**Interfaces:**
- Consumes: `FEATURE_KEYS`, `SEGMENTS` from Task 1.
- Produces: message keys `compare.feature.{key}` (11), `compare.segment.{segment}` (3), `compare.note.{promo|secondHand|undisclosed}` (3), `compare.verifiedOn`, `compare.colFeature`, `compare.colNels`, `compare.hubIntro`, `compare.spreadsheetRow`, `compare.viewComparison`.

Existing `compare.row1`–`compare.row4` keys become dead once the hub is rebuilt in Task 5 — delete them in this task, in all six locales.

- [ ] **Step 1: Write the failing test**

Create `marketing/src/lib/compare/i18n-parity.test.js`:

```js
import { describe, it, expect } from 'vitest';
import { FEATURE_KEYS, SEGMENTS } from './competitors.js';
import en from '../i18n/locales/en.json';
import es from '../i18n/locales/es.json';
import fr from '../i18n/locales/fr.json';
import de from '../i18n/locales/de.json';
import it from '../i18n/locales/it.json';
import pt from '../i18n/locales/pt.json';

const LOCALES = { es, fr, de, it, pt };
const SEGMENT_KEY = { 'ai-native': 'aiNative', traditional: 'traditional', planning: 'planning' };
const NOTE_KEYS = ['promo', 'secondHand', 'undisclosed'];
const CHROME = ['verifiedOn', 'colFeature', 'colNels', 'hubIntro', 'spreadsheetRow', 'viewComparison'];

describe('compare base keys', () => {
  it('en has a label for every feature key', () => {
    for (const k of FEATURE_KEYS) expect(typeof en.compare.feature[k]).toBe('string');
  });

  it('en has a heading for every segment', () => {
    for (const s of SEGMENTS) expect(typeof en.compare.segment[SEGMENT_KEY[s]]).toBe('string');
  });

  it('en has the pricing-caveat notes and page chrome', () => {
    for (const k of NOTE_KEYS) expect(typeof en.compare.note[k]).toBe('string');
    for (const k of CHROME) expect(typeof en.compare[k]).toBe('string');
  });

  it('drops the retired generic rows', () => {
    expect(en.compare.row1).toBeUndefined();
  });

  for (const [code, dict] of Object.entries(LOCALES)) {
    it(`${code}: has every base key, translated (not the English fallback)`, () => {
      for (const k of FEATURE_KEYS) {
        expect(typeof dict.compare.feature[k]).toBe('string');
        expect(dict.compare.feature[k].length).toBeGreaterThan(0);
      }
      for (const s of SEGMENTS) expect(typeof dict.compare.segment[SEGMENT_KEY[s]]).toBe('string');
      for (const k of NOTE_KEYS) expect(typeof dict.compare.note[k]).toBe('string');
      expect(dict.compare.row1).toBeUndefined();
    });
  }
});
```

Short labels (feature names, segment headings) are checked for **presence and non-emptiness only**,
never for difference from English: labels like "Audit log" are legitimately identical in several
target languages, and a strict check would fail the build on correct translations. The strict
"not the English fallback" assertion belongs on the long-form per-page prose, where Plans 2 and 3
apply it.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd marketing && pnpm vitest run src/lib/compare/i18n-parity.test.js`
Expected: FAIL — "Cannot read properties of undefined (reading 'conversationalLogging')"

- [ ] **Step 3: Add the keys**

In `marketing/src/lib/i18n/locales/en.json`, replace the `compare.row1`–`row4` objects with:

```json
"compare": {
  "seo": {
    "title": "Compare",
    "description": "An honest look at how Nels' conversational, passwordless budgeting compares to AI assistants, budgeting apps, and financial planners."
  },
  "title": "How Nels compares",
  "subtitle": "An honest comparison, updated as these products change. Every page names something the other product does better.",
  "hubIntro": "Pick the product you're weighing Nels against.",
  "colFeature": "Feature",
  "colNels": "Nels",
  "viewComparison": "See the full comparison",
  "verifiedOn": "Compared using publicly available pricing as of {date}.",
  "spreadsheetRow": "A spreadsheet does whatever you build, for free — as long as you keep building it. Nels is the version that maintains itself.",
  "segment": {
    "aiNative": "AI money assistants",
    "traditional": "Budgeting apps",
    "planning": "Planning and wealth management"
  },
  "note": {
    "promo": "Promotional rate, not list price.",
    "secondHand": "From third-party reporting, not the vendor's own site.",
    "undisclosed": "Pricing not published."
  },
  "feature": {
    "conversationalLogging": "Log expenses by chatting",
    "writableLedger": "Writable budget, not just a dashboard",
    "envelopeBudgeting": "Envelope / zero-based budgeting",
    "householdSharing": "Household sharing",
    "auditLog": "Audit log of changes",
    "bankSync": "Automatic bank syncing",
    "passwordless": "Passwordless sign-in",
    "aiInsights": "AI spending insights",
    "goals": "Savings and debt goals",
    "humanAdvisors": "Human financial advisors",
    "retirementProjection": "Retirement projections"
  }
}
```

Mirror the same structure into `es`, `fr`, `de`, `it`, `pt` with machine translations, keeping each file's existing `_meta` block untouched. Do not translate `{date}` — it is an interpolation placeholder.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd marketing && pnpm vitest run src/lib/compare/i18n-parity.test.js`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add marketing/src/lib/i18n/locales/ marketing/src/lib/compare/i18n-parity.test.js
git commit -m "feat(marketing): add compare feature/segment message keys in six locales"
```

---

### Task 4: The `[competitor]` route

**Files:**
- Create: `marketing/src/routes/[lang]/compare/[competitor]/+page.js`
- Create: `marketing/src/routes/[lang]/compare/[competitor]/+page.svelte`

**Interfaces:**
- Consumes: `findCompetitor()` from Task 1; `compare.*` keys from Task 3.
- Produces: a prerendered deep-dive page per published competitor. No exports other tasks consume.

There is no unit test for this task — the marketing harness has no component tests (Global Constraints). Verification is the build plus a dev-server check.

- [ ] **Step 1: Write the loader**

Create `marketing/src/routes/[lang]/compare/[competitor]/+page.js`:

```js
import { error } from '@sveltejs/kit';
import { findCompetitor } from '$lib/compare/competitors.js';

// `prerender` and `trailingSlash` are inherited from [lang]/+layout.js.
export async function load({ params, parent }) {
  const { lang } = await parent();
  const competitor = findCompetitor(params.competitor);
  if (!competitor) throw error(404, `Unknown competitor: ${params.competitor}`);
  return { lang, competitor };
}
```

- [ ] **Step 2: Create the shared component helpers**

Create `marketing/src/lib/compare/nels.js`. This holds the values BOTH comparison pages need, so
they are defined once. Unlike `competitors.js` and `paths.js`, this file is imported only by Svelte
components — never by `svelte.config.js` — so it may safely import `SITE`.

```js
// Values shared by the compare hub and the deep-dive pages. Kept out of
// competitors.js because that module must stay import-free for svelte.config.js;
// this one is component-only, so it may read SITE.
import { SITE } from '$lib/site.js';

/** Nels' own row in the feature matrix. Keys mirror FEATURE_KEYS exactly. */
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

/** Maps a competitor's `segment` to its i18n key suffix. */
export const SEGMENT_KEY = {
  'ai-native': 'aiNative',
  traditional: 'traditional',
  planning: 'planning',
};

/** Renders a feature value as a table mark. */
export const mark = (v) => (v === true ? '✓' : v === 'partial' ? '~' : '—');

/** Nels' entry price, for copy that contrasts cost. */
export const NELS_ENTRY_PRICE = SITE.plans.basic.monthly;
```

- [ ] **Step 3: Write the page component**

Create `marketing/src/routes/[lang]/compare/[competitor]/+page.svelte`, importing the shared helpers rather than redefining them.

```svelte
<script>
  import { _ } from 'svelte-i18n';
  import { FEATURE_KEYS } from '$lib/compare/competitors.js';
  import { NELS_FEATURES, mark } from '$lib/compare/nels.js';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';
  import JsonLd from '$lib/components/JsonLd.svelte';
  import { faqPage } from '$lib/components/schema.js';

  let { data } = $props();
  const c = $derived(data.competitor);

  const faq = $derived(
    faqPage([
      { q: $_(`compare.${c.slug}.faq.q1`), a: $_(`compare.${c.slug}.faq.a1`) },
      { q: $_(`compare.${c.slug}.faq.q2`), a: $_(`compare.${c.slug}.faq.a2`) },
      { q: $_(`compare.${c.slug}.faq.q3`), a: $_(`compare.${c.slug}.faq.a3`) },
    ])
  );
</script>

<Seo
  title={$_(`compare.${c.slug}.seo.title`)}
  path={`/compare/${c.slug}`}
  lang={data.lang}
  description={$_(`compare.${c.slug}.seo.description`)}
/>
<JsonLd data={faq} />

<Section>
  <h1 class="text-center text-4xl font-bold">{$_(`compare.${c.slug}.headline`)}</h1>
  <p class="mx-auto mt-4 max-w-2xl text-center opacity-80">{$_(`compare.${c.slug}.intro`)}</p>

  <div class="mt-8 overflow-x-auto">
    <table class="table">
      <thead>
        <tr>
          <th>{$_('compare.colFeature')}</th>
          <th>{$_('compare.colNels')}</th>
          <th>{c.name}</th>
        </tr>
      </thead>
      <tbody>
        {#each FEATURE_KEYS as key}
          <tr>
            <td>{$_(`compare.feature.${key}`)}</td>
            <td class="font-semibold text-primary">{mark(NELS_FEATURES[key])}</td>
            <td class="opacity-70">{mark(c.features[key])}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <h2 class="mt-12 text-2xl font-bold">{$_(`compare.${c.slug}.whoForTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(`compare.${c.slug}.whoFor`)}</p>

  <h2 class="mt-8 text-2xl font-bold">{$_(`compare.${c.slug}.strengthTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(c.strength)}</p>

  <h2 class="mt-8 text-2xl font-bold">{$_(`compare.${c.slug}.verdictTitle`)}</h2>
  <p class="mt-2 opacity-80">{$_(`compare.${c.slug}.verdict`)}</p>

  <p class="mt-10 text-sm opacity-60">
    {$_('compare.verifiedOn', { values: { date: c.verifiedOn } })}
    <a class="link" href={c.sourceUrl} rel="nofollow noopener" target="_blank">{c.name}</a>
    {#if c.pricing.note}
      <span class="ml-1">{$_(`compare.note.${c.pricing.note === 'second-hand' ? 'secondHand' : c.pricing.note}`)}</span>
    {/if}
  </p>

  <div class="mt-6 flex justify-center gap-4">
    <a class="link" href={`/${data.lang}/compare`}>{$_('compare.title')}</a>
    <a class="link" href={`/${data.lang}/pricing`}>{$_('nav.pricing')}</a>
  </div>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
```

Competitor links use `rel="nofollow noopener"` — these are commercial rivals; do not pass link equity.

- [ ] **Step 4: Verify the route 404s while nothing is published**

Run: `cd marketing && pnpm run build`
Expected: build succeeds and prerenders **no** `/compare/{slug}` pages, because `comparePaths()` is empty until a competitor is published in Phase 2.

- [ ] **Step 5: Commit**

```bash
git add marketing/src/lib/compare/nels.js marketing/src/routes/\[lang\]/compare/\[competitor\]/
git commit -m "feat(marketing): add competitor deep-dive route and page template"
```

---

### Task 5: Rebuild `/compare` as a segmented hub

**Files:**
- Modify: `marketing/src/routes/[lang]/compare/+page.svelte`

**Interfaces:**
- Consumes: `COMPETITORS`, `FEATURE_KEYS`, `SEGMENTS` (Task 1); `compare.*` keys (Task 3).
- Produces: the hub page. No exports.

The hub grid shows **all** competitors (facts need no prose), but only links to `published` ones.

- [ ] **Step 1: Replace the page**

Replace the contents of `marketing/src/routes/[lang]/compare/+page.svelte`:

```svelte
<script>
  import { _ } from 'svelte-i18n';
  import { COMPETITORS, FEATURE_KEYS, SEGMENTS } from '$lib/compare/competitors.js';
  import { NELS_FEATURES, SEGMENT_KEY, mark } from '$lib/compare/nels.js';
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import Cta from '$lib/components/Cta.svelte';

  let { data } = $props();

  const grouped = $derived(
    SEGMENTS.map((s) => ({ segment: s, items: COMPETITORS.filter((c) => c.segment === s) }))
  );

</script>

<Seo
  title={$_('compare.seo.title')}
  path="/compare"
  lang={data.lang}
  description={$_('compare.seo.description')}
/>

<Section>
  <h1 class="text-center text-4xl font-bold">{$_('compare.title')}</h1>
  <p class="mx-auto mt-4 max-w-2xl text-center opacity-80">{$_('compare.subtitle')}</p>
  <p class="mx-auto mt-2 max-w-2xl text-center opacity-70">{$_('compare.hubIntro')}</p>

  {#each grouped as group}
    <h2 class="mt-12 text-2xl font-bold">{$_(`compare.segment.${SEGMENT_KEY[group.segment]}`)}</h2>
    <div class="mt-4 overflow-x-auto">
      <table class="table">
        <thead>
          <tr>
            <th>{$_('compare.colFeature')}</th>
            <th>{$_('compare.colNels')}</th>
            {#each group.items as c}<th>{c.name}</th>{/each}
          </tr>
        </thead>
        <tbody>
          {#each FEATURE_KEYS as key}
            <tr>
              <td>{$_(`compare.feature.${key}`)}</td>
              <td class="font-semibold text-primary">{mark(NELS_FEATURES[key])}</td>
              {#each group.items as c}<td class="opacity-70">{mark(c.features[key])}</td>{/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <div class="mt-3 flex flex-wrap gap-4">
      {#each group.items.filter((c) => c.published) as c}
        <a class="link" href={`/${data.lang}/compare/${c.slug}`}>
          {$_('compare.viewComparison')} — {c.name}
        </a>
      {/each}
    </div>
  {/each}

  <p class="mt-12 opacity-80">{$_('compare.spreadsheetRow')}</p>
  <div class="mt-10 text-center"><Cta /></div>
</Section>
```

- [ ] **Step 2: Verify the build and view the page**

Run: `cd marketing && pnpm run build && pnpm run dev`
Expected: build succeeds. Visit `http://localhost:5173/en/compare` — three segment tables render, no deep-dive links appear yet (nothing published), and the page scrolls horizontally on narrow viewports without the body overflowing.

- [ ] **Step 3: Commit**

```bash
git add marketing/src/routes/\[lang\]/compare/+page.svelte
git commit -m "feat(marketing): rebuild compare hub as a segmented competitor grid"
```

---

### Task 6: Fix the orphaned hub (nav + footer)

**Files:**
- Modify: `marketing/src/lib/site.js:24`
- Modify: `marketing/src/lib/components/Footer.svelte`
- Test: `marketing/src/lib/site.test.js`

**Interfaces:**
- Consumes: nothing new.
- Produces: `/compare` reachable by navigation. `NAV` gains a fifth entry.

- [ ] **Step 1: Write the failing test**

Append to `marketing/src/lib/site.test.js`:

```js
import { NAV } from './site.js';

describe('compare page is reachable from navigation (#445)', () => {
  it('NAV includes /compare', () => {
    expect(NAV.map((n) => n.path)).toContain('/compare');
  });
  it('the compare nav entry uses an i18n key', () => {
    const entry = NAV.find((n) => n.path === '/compare');
    expect(entry.key).toBe('nav.compare');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd marketing && pnpm vitest run src/lib/site.test.js`
Expected: FAIL — "expected [ '/features', '/how-it-works', '/pricing', '/security' ] to contain '/compare'"

- [ ] **Step 3: Add the nav entry, the message key, and the footer link**

In `marketing/src/lib/site.js`, append to `NAV`:

```js
  { key: 'nav.compare', path: '/compare' },
```

Add `"compare"` under the `nav` object in **all six** locale files (e.g. en: `"compare": "Compare"`).

In `marketing/src/lib/components/Footer.svelte`, add a `/compare` link alongside the existing locale-prefixed links, following that file's existing markup pattern.

- [ ] **Step 4: Run the full gate**

Run: `cd marketing && pnpm test && pnpm run build && pnpm check`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add marketing/src/lib/site.js marketing/src/lib/site.test.js \
        marketing/src/lib/components/Footer.svelte marketing/src/lib/i18n/locales/
git commit -m "fix(marketing): link the compare hub from nav and footer"
```

---

## Definition of Done

- [ ] `cd marketing && pnpm test && pnpm run build` green.
- [ ] `pnpm check` adds **no new errors** vs main's pre-existing baseline of 19. It is NOT green on main and this plan does not fix that; the bar is that this branch contributes zero.
- [ ] `/en/compare` renders three segment tables covering all eleven competitors.
- [ ] No `/compare/{slug}` page is prerendered (nothing published in Phase 1).
- [ ] `/compare` appears in the header nav and footer in all six locales.
- [ ] Visiting `/en/compare/ynab` in dev returns 404.
