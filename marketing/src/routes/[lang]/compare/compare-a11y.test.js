// Rendering-layer guard for the comparison tables' accessibility affordances,
// across BOTH templates — the /compare hub and every published
// /compare/<slug> deep dive. The success criteria involved:
//
//   - SC 1.3.1 (info and relationships) — the `scope`d header cells
//   - SC 1.1.1 (non-text content) — the mark glyphs. 1.1.1 reaches a text
//     character like "✓" through failure F26, "using a non-text mark to
//     convey information"; an auditor could as reasonably file this under
//     1.3.1, so both are worth knowing about
//   - SC 2.1.1 (keyboard), via technique SCR29 — the focusable scroll wrapper
//   - SC 4.1.2 (name, role, value) — that wrapper's `role="region"` and its
//     `aria-label`. SCR29 itself says nothing about naming a region
//
// Four affordances shipped with no rendering test at all; #470 inventories all
// four and attributes each to the change that introduced it. This file is that
// test. For every table on both templates it asserts:
//
//   1. every column header is a `<th scope="col">`
//   2. every mark cell pairs an `aria-hidden` glyph with a non-empty
//      `.sr-only` text equivalent, so no cell announces as a bare "✓"
//   3. the scrollable wrapper is keyboard-operable — `role="region"`,
//      `tabindex="0"`, and a non-empty `aria-label`, distinct per page
//   4. every body row's first cell is a `<th scope="row">`
//
// plus two things that make the first four meaningful rather than decorative:
// the table keeps its own semantics — nothing is `aria-hidden`, nothing is
// `role="presentation"` — and the hand-written pricing row #458 added as the
// first `<tr>` of every `<tbody>` is held to the same rules. That row sits
// outside the `{#each FEATURE_KEYS}` loop, so nothing structural protects it;
// it is both the row most likely to be edited by hand and the one where
// `<td>Price</td>` is the natural thing to type.
//
// COUNTS AND CONTENT ARE DERIVED, NEVER HARDCODED. Column counts come from
// `COMPETITORS`, row counts from `FEATURE_KEYS`, table counts from `SEGMENTS`,
// and the deep-dive cases from `publishedCompetitors()`. Publishing a
// competitor EXTENDS this file's coverage rather than breaking it, which is
// the property that lets content work land without touching this file.
// Attribute and class expectations are literal on purpose — those ARE the
// affordance, not data.
//
// The cost of deriving expectations, stated plainly because it is easy to miss:
// a derived expectation is blind to a defect in the function that derives it.
// `mark()`, `markLabel()` and `SEGMENT_KEY` have no other test in the repo, so
// collapsing `mark` to a constant would otherwise leave every cell rendering
// "✓" and announcing "Yes" with this file still green. `pins the glyph, label
// and segment mappings` below breaks that circularity with literals, and is
// the assertion the rest of the file leans on.
//
// WHAT THIS DOES NOT PROVE. jsdom has no accessibility tree, so nothing here
// is evidence of what a screen reader announces; real behaviour still rests on
// manual verification. Note that this limit is narrower than it sounds — most
// of the ways to silence a correct-looking table (`aria-hidden`,
// `role="presentation"`) are plain markup that jsdom exposes perfectly well,
// and those are asserted here. Three assertions are deliberate class-presence
// proxies, disclosed again at each call site in the same style as
// `header-a11y.test.js`:
//   - `font-normal` on a row header stands in for "it still renders like the
//     plain cell it replaced"; without it the whole first column goes bold
//   - `text-start` rather than `text-left` keeps the alignment
//     direction-agnostic. No RTL locale ships today, so this is a cheap
//     property to keep rather than one being exercised
//   - `overflow-x-auto` on the wrapper stands in for "this region actually
//     scrolls", which is the precondition that makes SCR29 relevant at all. A
//     `role="region"` on a non-scrolling div is noise, not an affordance
import { describe, it, expect, beforeAll } from 'vitest';
import { render } from '@testing-library/svelte';

// Registers the catalogs as an import side effect; waitLocale then makes $_
// return strings rather than message keys during render.
import '$lib/i18n/index.js';
import { _, waitLocale } from 'svelte-i18n';
import { get } from 'svelte/store';
import { publishedCompetitors } from '$lib/compare/paths.js';
import { COMPETITORS, FEATURE_KEYS, SEGMENTS } from '$lib/compare/competitors.js';
import { pricingSummary } from '$lib/compare/pricing.js';
import {
  NELS_FEATURES,
  NELS_PRICING_SUMMARY,
  SEGMENT_KEY,
  mark,
  markLabel,
} from '$lib/compare/nels.js';

import PriceCell from '$lib/components/PriceCell.svelte';
import HubPage from './+page.svelte';
import DeepDivePage from './[competitor]/+page.svelte';

beforeAll(async () => {
  await waitLocale();
});

/**
 * The three feature values, each pinned to its glyph and its message key with
 * literals. This is the one place in the file that does not derive, and that
 * is the point — see the header. `t()` is not used here so the pin holds
 * whatever locale the suite runs in.
 * @type {[import('$lib/compare/competitors.js').FeatureValue, string, string][]}
 */
const MARK_PAIRS = [
  [true, '✓', 'compare.mark.yes'],
  ['partial', '~', 'compare.mark.partial'],
  [false, '—', 'compare.mark.no'],
];

// role values that strip an element's implicit semantics. On a <table> this
// removes every header association the rest of this file asserts, while every
// `scope` attribute stays exactly where it was.
const SEMANTICS_STRIPPING_ROLES = new Set(['presentation', 'none']);

/**
 * Resolve an i18n message.
 *
 * On the `waitLocale()` above: `$lib/i18n/index.js` registers every catalog
 * synchronously with `addMessages` and calls `init()` at import time, so by
 * the time any test runs the messages are already resolved and that await is
 * a no-op today. It is kept for parity with the sibling render tests and
 * because a switch to svelte-i18n's async `register()` would make it matter.
 * Do not read it as the thing that makes this file's text comparisons sound:
 * if a catalog ever failed to load, svelte-i18n would echo the message KEY
 * rather than throw, and the templates would echo the identical key, so every
 * comparison here would still match. `resolves the i18n catalog` below asserts
 * resolution directly, which is what actually rules that out.
 *
 * Never call this at module scope: `CASES` is built at collection time, before
 * `beforeAll` has run, and must stay i18n-free.
 * @param {string} key
 * @param {Record<string, string|null>} [values]
 * @returns {string}
 */
const t = (key, values) => get(_)(key, values ? { values } : {});

/**
 * @typedef {Object} TableModel
 * @property {string[]} headers - column header text, in render order
 * @property {import('$lib/compare/competitors.js').Features[]} featureSources -
 *   one feature map per data column (Nels first), in render order
 * @property {import('$lib/compare/pricing.js').PricingSummary[]} pricingSources -
 *   one pricing descriptor per data column, in the same order
 * @property {string} scrollLabel - the expected `aria-label` on the wrapper
 * @property {string} caption - the expected `<caption>` text on the table
 */

/**
 * The tables the hub is expected to render, derived from the data modules so
 * publishing a competitor cannot make this stale.
 *
 * Known seam: this re-derives the template's own grouping rather than
 * specifying it independently, so a grouping bug duplicated on both sides
 * would not be caught. The ordered header-text equality in
 * `assertColumnHeaders` is what keeps that seam narrow.
 * @returns {TableModel[]}
 */
function hubModels() {
  return SEGMENTS.map((segment) => {
    const items = COMPETITORS.filter((c) => c.segment === segment);
    const segmentName = t(`compare.segment.${SEGMENT_KEY[segment]}`);
    return {
      headers: [t('compare.colFeature'), t('compare.colNels'), ...items.map((c) => c.name)],
      featureSources: [NELS_FEATURES, ...items.map((c) => c.features)],
      pricingSources: [NELS_PRICING_SUMMARY, ...items.map((c) => pricingSummary(c.pricing))],
      scrollLabel: t('compare.scrollRegion', { name: segmentName }),
      caption: t('compare.tableScrollLabel', { name: segmentName }),
    };
  });
}

/**
 * The single table a deep-dive page is expected to render.
 * @param {import('$lib/compare/competitors.js').Competitor} c
 * @returns {TableModel[]}
 */
function deepDiveModels(c) {
  return [
    {
      headers: [t('compare.colFeature'), t('compare.colNels'), c.name],
      featureSources: [NELS_FEATURES, c.features],
      pricingSources: [NELS_PRICING_SUMMARY, pricingSummary(c.pricing)],
      scrollLabel: t('compare.scrollRegion', { name: c.name }),
      caption: t('compare.tableScrollLabel', { name: c.name }),
    },
  ];
}

/**
 * Tolerates a nullish element so a failure message built from a missing cell
 * reports the assertion that follows rather than throwing a TypeError over it.
 * @param {Element|null|undefined} el
 */
const text = (el) => (el?.textContent ?? '').trim();

/** @param {string} s */
const squash = (s) => s.replace(/\s+/g, ' ').trim();

/**
 * Every table on the page, with the count checked against the model up front —
 * without this, deleting a whole segment table would leave every per-table
 * `forEach` below iterating nothing and passing.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 * @returns {HTMLTableElement[]}
 */
function tablesOf(container, models) {
  const tables = [...container.querySelectorAll('table')];
  expect(tables).toHaveLength(models.length);
  return tables;
}

/**
 * Nothing on the path to a cell may be hidden from assistive tech. A
 * `tabindex="0"` element inside an `aria-hidden` subtree is its own WCAG 4.1.2
 * failure, and it removes the table from the accessibility tree entirely,
 * which would make every other assertion in this file moot while they all
 * still pass.
 * @param {Element} el
 * @param {string} what
 */
function expectExposed(el, what) {
  expect(el.getAttribute('aria-hidden'), `${what} must not be aria-hidden`).toBeNull();
  expect(
    el.closest('[aria-hidden="true"]'),
    `${what} must not sit inside an aria-hidden subtree`
  ).toBeNull();
}

/**
 * The table keeps its own semantics. `role="presentation"` on the `<table>`
 * strips the table role and with it every header association — one attribute
 * that voids all four affordances while leaving every `scope` in place.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertTableSemantics(container, models) {
  for (const table of tablesOf(container, models)) {
    expectExposed(table, 'the comparison table');
    expect(SEMANTICS_STRIPPING_ROLES.has(table.getAttribute('role') ?? '')).toBe(false);
    for (const cell of table.querySelectorAll('th, td')) {
      expect(SEMANTICS_STRIPPING_ROLES.has(cell.getAttribute('role') ?? ''), text(cell)).toBe(false);
      expect(cell.getAttribute('aria-hidden'), text(cell)).toBeNull();
    }
  }
}

/**
 * Affordance 1 — column headers. Loops every cell of the header row rather
 * than sampling with a `th[scope="col"]` selector, so dropping `scope` from a
 * single column, or turning one into a `<td>`, fails. Comparing the full
 * ordered header-text array is also what makes a column swap fail
 * unconditionally, including between two competitors with identical features.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertColumnHeaders(container, models) {
  tablesOf(container, models).forEach((table, i) => {
    const headRows = table.querySelectorAll('thead tr');
    expect(headRows).toHaveLength(1);
    const cells = [...headRows[0].children];
    // Rules out an empty model, which would make the equality below vacuous.
    // It does NOT rule out an unresolved one: svelte-i18n echoes a missing key
    // rather than returning '', and both sides would echo it identically. That
    // case is covered by `resolves the i18n catalog` and by i18n-parity.test.js.
    for (const header of models[i].headers) expect(header.length).toBeGreaterThan(0);
    expect(cells.map(text)).toEqual(models[i].headers);
    for (const cell of cells) {
      expect(cell.tagName).toBe('TH');
      expect(cell.getAttribute('scope')).toBe('col');
    }
  });
}

/**
 * Affordance 2 — every mark cell carries a visual glyph hidden from assistive
 * tech plus a text equivalent that is not.
 *
 * Two passes on purpose. The first walks `FEATURE_KEYS` and checks the exact
 * glyph/label PAIRING per column, which is what catches a swap. The second
 * sweeps every body cell, generated or not — the first pass only visits rows
 * the `{#each}` loop produces, and a hand-written row is precisely what #470
 * exists to catch.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertMarkCells(container, models) {
  tablesOf(container, models).forEach((table, i) => {
    const sources = models[i].featureSources;
    const rows = [...table.querySelectorAll('tbody tr')];
    const byLabel = new Map(rows.map((r) => [text(r.firstElementChild), r]));
    // Duplicate row-header text would silently drop a row from the lookup —
    // and would itself be a defect.
    expect(byLabel.size).toBe(rows.length);

    for (const key of FEATURE_KEYS) {
      const row = byLabel.get(t(`compare.feature.${key}`));
      expect(row, `feature row for "${key}"`).toBeDefined();
      const cells = [...(/** @type {Element} */ (row).children)].slice(1);
      expect(cells, `data cells in the "${key}" row`).toHaveLength(sources.length);

      cells.forEach((cell, col) => {
        const value = sources[col][key];
        const where = `"${key}" x column ${col}`;

        // querySelectorAll with a length check, not querySelector: a second
        // stray glyph would otherwise hide behind the first.
        const glyphs = cell.querySelectorAll('span[aria-hidden="true"]');
        expect(glyphs, `aria-hidden glyph for ${where}`).toHaveLength(1);
        expect(text(glyphs[0]), `glyph for ${where}`).toBe(mark(value));

        const srs = cell.querySelectorAll('.sr-only');
        expect(srs, `.sr-only text equivalent for ${where}`).toHaveLength(1);
        // Hiding the text equivalent too would leave the cell announcing
        // nothing at all, which is worse than the bare glyph.
        expect(srs[0].getAttribute('aria-hidden')).toBeNull();
        const alt = text(srs[0]);
        expect(alt.length, `.sr-only text for ${where}`).toBeGreaterThan(0);
        expect(alt, `.sr-only text for ${where}`).toBe(t(markLabel(value)));
      });
    }

    // Second pass. A cell that has one half of the pattern must have the
    // other. The one shape this cannot see is a cell holding a bare glyph with
    // neither `aria-hidden` nor `.sr-only`, which is indistinguishable from an
    // ordinary text cell without knowing what the row means.
    for (const cell of table.querySelectorAll('tbody td')) {
      const glyphs = cell.querySelectorAll('[aria-hidden="true"]');
      const srs = cell.querySelectorAll('.sr-only');
      if (glyphs.length === 0 && srs.length === 0) continue;
      const where = `cell "${squash(text(cell))}"`;
      expect(glyphs, `aria-hidden glyph in ${where}`).toHaveLength(1);
      expect(srs, `.sr-only equivalent in ${where}`).toHaveLength(1);
      expect(text(srs[0]).length, `.sr-only text in ${where}`).toBeGreaterThan(0);
      expect(srs[0].getAttribute('aria-hidden')).toBeNull();
    }
  });
}

/**
 * The legend under each table repeats the glyph/text pattern outside any cell,
 * and is the only other place it appears. Dropping `aria-hidden` there makes
 * every table trail "✓ Yes ~ Partial — No".
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertMarkLegend(container, models) {
  const labels = MARK_PAIRS.map(([, , key]) => t(key));
  const legends = [...container.querySelectorAll('p')].filter((p) =>
    labels.every((label) => (p.textContent ?? '').includes(label))
  );
  expect(legends, 'one mark legend per table').toHaveLength(models.length);
  for (const legend of legends) {
    const glyphs = [...legend.querySelectorAll('span')];
    expect(glyphs).toHaveLength(MARK_PAIRS.length);
    glyphs.forEach((glyph, i) => {
      expect(glyph.getAttribute('aria-hidden')).toBe('true');
      expect(text(glyph)).toBe(MARK_PAIRS[i][1]);
    });
  }
}

/**
 * Affordance 3 — the horizontally scrollable wrapper is keyboard-operable
 * (SCR29) and named (SC 4.1.2).
 *
 * The wrapper must be a real ancestor, not the table itself: `closest()`
 * matches its own start element, so moving `role`/`tabindex`/`aria-label` onto
 * the `<table>` would otherwise pass — while producing no scroll container at
 * all, since `overflow` has no effect on `display: table`.
 *
 * `overflow-x-auto` is a class-presence proxy for "this actually scrolls";
 * jsdom computes no layout.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertScrollRegion(container, models) {
  /** @type {string[]} */
  const labels = [];
  /** @type {Element[]} */
  const regions = [];
  tablesOf(container, models).forEach((table, i) => {
    const region = table.closest('[role="region"]');
    expect(region, 'scrollable wrapper with role="region"').not.toBeNull();
    const el = /** @type {Element} */ (region);
    expect(el, 'the region must wrap the table, not be it').not.toBe(table);
    expect(table.parentElement).toBe(el);
    expect(el.getAttribute('tabindex')).toBe('0');
    expect(el.classList.contains('overflow-x-auto')).toBe(true);
    const label = el.getAttribute('aria-label') ?? '';
    expect(label.length).toBeGreaterThan(0);
    expect(label).toBe(models[i].scrollLabel);
    labels.push(label);
    regions.push(el);
  });
  // One region per table, never one shared by two.
  expect(new Set(regions).size).toBe(regions.length);
  // Not redundant with the equality above: that would still pass if two
  // segments resolved to the SAME name, because the model would collide the
  // same way. Identically named regions are indistinguishable in a landmark
  // list. Vacuous on a deep dive, which renders a single table.
  expect(new Set(labels).size).toBe(labels.length);
}

/**
 * Each comparison table has an accessible name on the table itself via
 * `<caption class="sr-only">`, distinct from the scroll-region landmark label
 * so they don't announce the same string back to back.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertTableCaption(container, models) {
  tablesOf(container, models).forEach((table, i) => {
    const captions = table.querySelectorAll(':scope > caption');
    expect(captions, 'table must have exactly one caption').toHaveLength(1);
    const caption = /** @type {Element} */ (captions[0]);
    expect(caption.classList.contains('sr-only'), 'caption must be visually hidden').toBe(true);
    expect(caption.getAttribute('aria-hidden')).toBeNull();
    expect((caption.textContent ?? '').trim().length).toBeGreaterThan(0);
    expect((caption.textContent ?? '').trim()).toBe(models[i].caption);
  });
}

/**
 * Affordance 4 — every body row leads with a row header, and nothing after it
 * is one. Looping every row rather than sampling the table is what makes a
 * hand-written row (the pricing row today, whatever lands next tomorrow) fail
 * instead of slipping through.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertEveryRowHasAHeader(container, models) {
  tablesOf(container, models).forEach((table, i) => {
    const rows = table.querySelectorAll('tbody tr');
    // `>=`, never `===`: the feature rows plus at least the pricing row,
    // leaving room for rows a later change adds without this failing for a
    // reason unrelated to row headers. Paired with the per-row check below,
    // a new row is *required* to carry a header rather than exempted.
    expect(rows.length).toBeGreaterThanOrEqual(FEATURE_KEYS.length + 1);
    for (const row of rows) {
      const cells = [...row.children];
      // Every body row spans the full table, so a row missing a cell — or
      // carrying a stray extra one — misaligns the header association.
      expect(cells, `cells in row "${text(cells[0])}"`).toHaveLength(models[i].headers.length);

      const first = cells[0];
      expect(first.tagName).toBe('TH');
      expect(first.getAttribute('scope')).toBe('row');
      expect(text(first).length).toBeGreaterThan(0);
      // `font-normal` keeps the first column from rendering bold and
      // `text-start` keeps its alignment direction-agnostic; both are
      // class-presence proxies, per the header.
      expect(first.classList.contains('font-normal')).toBe(true);
      expect(first.classList.contains('text-start')).toBe(true);

      // Only the first cell is a header; a `<th>` in a data column would
      // claim to label the rest of the row.
      for (const cell of cells.slice(1)) expect(cell.tagName).toBe('TD');
    }
  });
}

/**
 * The pricing row #458 added. Asserting identity and position rather than
 * counting rows is deliberate: an exact row count fails the next time ANY row
 * is added, and the only available fix is to bump the constant. This is also
 * strictly stronger — swapping the pricing row for another row, or moving it
 * below the feature rows, both fail here and neither would fail a count.
 *
 * Each cell is compared against a standalone render of `PriceCell` fed the
 * descriptor `pricing.js` produces for that column. That checks the WHOLE cell
 * rather than a substring, so a dropped annual line, a dropped "from" hedge, a
 * dropped promo caveat or a second price smuggled into the same cell all fail
 * — every one of which would tilt this table in Nels' favour, for the reasons
 * argued at length in `pricing.js` and `nels.js`. What it cannot catch is a
 * regression inside PriceCell itself, since both sides would move together;
 * that is `PriceCell.test.js`'s contract, and it pins the shells with literals.
 * @param {HTMLElement} container
 * @param {TableModel[]} models
 */
function assertPricingRowIsFirst(container, models) {
  const label = t('compare.pricing.label');
  expect(label.length).toBeGreaterThan(0);
  tablesOf(container, models).forEach((table, i) => {
    const row = table.querySelector('tbody tr:first-child');
    expect(row).not.toBeNull();
    const cells = [...(/** @type {Element} */ (row).children)];
    expect(cells[0].tagName).toBe('TH');
    expect(text(cells[0])).toBe(label);

    const sources = models[i].pricingSources;
    const dataCells = cells.slice(1);
    expect(dataCells).toHaveLength(sources.length);
    dataCells.forEach((cell, col) => {
      const summary = sources[col];
      const { container: expected } = render(PriceCell, { props: { summary } });
      const where = `pricing cell for column ${col}`;
      expect(squash(text(expected)).length, `${where}: expectation is empty`).toBeGreaterThan(0);
      expect(squash(text(cell)), where).toBe(squash(text(expected)));
      expect(cell.querySelectorAll('span'), where).toHaveLength(
        expected.querySelectorAll('span').length
      );
    });
  });
}

/**
 * One case per rendered template. The deep-dive cases come from the data, so
 * a competitor published by a later change is covered automatically and no
 * slug is hardcoded here.
 * @type {{ name: string, competitor: import('$lib/compare/competitors.js').Competitor|null }[]}
 */
const CASES = [
  { name: 'the compare hub', competitor: null },
  ...publishedCompetitors().map((c) => ({ name: `the ${c.slug} deep dive`, competitor: c })),
];

/**
 * @param {import('$lib/compare/competitors.js').Competitor|null} competitor
 * @returns {{ container: HTMLElement, models: TableModel[] }}
 */
function setup(competitor) {
  if (competitor) {
    const { container } = render(DeepDivePage, { props: { data: { lang: 'en', competitor } } });
    return { container, models: deepDiveModels(competitor) };
  }
  const { container } = render(HubPage, { props: { data: { lang: 'en' } } });
  return { container, models: hubModels() };
}

// Guards the suite itself. Each of these, left unasserted, is a way for the
// whole file to keep reporting green while testing nothing.
describe('comparison table a11y — suite guards', () => {
  it('has a deep-dive case for every published competitor', () => {
    // With no published competitors, every deep-dive describe below silently
    // disappears rather than failing.
    expect(publishedCompetitors().length).toBeGreaterThan(0);
    expect(CASES).toHaveLength(publishedCompetitors().length + 1);
  });

  it('has segments and features to iterate', () => {
    // With either of these empty, the hub renders no tables (or tables with no
    // feature rows) and every per-table loop passes over nothing.
    expect(SEGMENTS.length).toBeGreaterThan(0);
    expect(FEATURE_KEYS.length).toBeGreaterThan(0);
  });

  it('resolves the i18n catalog', () => {
    // Unresolved, svelte-i18n echoes the message key on BOTH sides of every
    // text comparison in this file — expectation and markup alike — so they
    // would all still match and the file would report green over an entirely
    // untranslated page. This is the only assertion here that can tell the
    // difference, so it is not redundant with the `length > 0` guards.
    expect(t('compare.colFeature')).not.toBe('compare.colFeature');
    expect(t('compare.pricing.label')).not.toBe('compare.pricing.label');
    expect(t('compare.tableScrollLabel', { name: 'x' })).not.toBe('compare.tableScrollLabel');
    expect(t('compare.scrollRegion', { name: 'x' })).not.toBe('compare.scrollRegion');
  });

  it('pins the glyph, label and segment mappings', () => {
    // Literal, not derived — see the header. Without this, collapsing `mark`
    // or `markLabel` to a constant leaves every assertion elsewhere in this
    // file comparing a mutated function against itself.
    for (const [value, glyph, key] of MARK_PAIRS) {
      expect(mark(value)).toBe(glyph);
      expect(markLabel(value)).toBe(key);
      expect(t(key), `"${key}" must resolve`).not.toBe(key);
    }
    expect(new Set(MARK_PAIRS.map(([, glyph]) => glyph)).size).toBe(MARK_PAIRS.length);
    expect(new Set(MARK_PAIRS.map(([, , key]) => t(key))).size).toBe(MARK_PAIRS.length);

    // The segment mapping names the hub's tables and their scroll regions, so
    // swapping two entries mislabels a region as well as a heading.
    expect(SEGMENT_KEY['ai-native']).toBe('aiNative');
    expect(SEGMENT_KEY.traditional).toBe('traditional');
    expect(SEGMENT_KEY.planning).toBe('planning');
    // Left open for a segment a later change adds: unpinned, but still
    // required to be distinct.
    expect(new Set(SEGMENTS.map((s) => SEGMENT_KEY[s])).size).toBe(SEGMENTS.length);
  });
});

for (const { name, competitor } of CASES) {
  describe(`comparison table a11y — ${name}`, () => {
    it('keeps the table and its cells exposed to assistive tech', () => {
      const { container, models } = setup(competitor);
      assertTableSemantics(container, models);
    });

    it('gives every column header th[scope="col"]', () => {
      const { container, models } = setup(competitor);
      assertColumnHeaders(container, models);
    });

    it('pairs an aria-hidden glyph with an .sr-only equivalent in every mark cell', () => {
      const { container, models } = setup(competitor);
      assertMarkCells(container, models);
    });

    it('hides the glyphs in the mark legend from assistive tech', () => {
      const { container, models } = setup(competitor);
      assertMarkLegend(container, models);
    });

    it('wraps every table in a keyboard-operable, labelled scroll region', () => {
      const { container, models } = setup(competitor);
      assertScrollRegion(container, models);
    });

    it('gives every table a sr-only caption with an accessible name', () => {
      const { container, models } = setup(competitor);
      assertTableCaption(container, models);
    });

    it('gives every body row a th[scope="row"] header', () => {
      const { container, models } = setup(competitor);
      assertEveryRowHasAHeader(container, models);
    });

    it('puts the pricing row first in every table', () => {
      const { container, models } = setup(competitor);
      assertPricingRowIsFirst(container, models);
    });
  });
}
