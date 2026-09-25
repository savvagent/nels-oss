// @vitest-environment node
/// <reference types="vite/client" />
import { describe, it, expect } from 'vitest';
import { COMPETITORS } from './competitors.js';
import { comparePaths, publishedCompetitors } from './paths.js';
import { LOCALIZED_PAGES } from '../../routes/sitemap.xml/pages.js';
import { LOCALES } from '../i18n/index.js';
import config from '../../../svelte.config.js';
import competitorsSrc from './competitors.js?raw';
import pathsSrc from './paths.js?raw';

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
  // Read via Vite's `?raw` import (source-as-string) rather than node:fs, so
  // this file needs no Node type definitions.
  it('competitors.js and paths.js import nothing outside their own directory', () => {
    for (const src of [competitorsSrc, pathsSrc]) {
      const imports = [...src.matchAll(/^\s*import\s.*?from\s+'([^']+)'/gm)].map((m) => m[1]);
      for (const spec of imports) expect(spec.startsWith('./')).toBe(true);
    }
  });

  // Blunt backstop: the regex above only catches single-line, single-quoted
  // `import … from '…'` statements. It misses multi-line imports,
  // double-quoted specifiers, side-effect imports (`import './x'`),
  // `export … from '...'` re-exports, and dynamic `import(...)`. Assert
  // directly that neither file's source ever opens a module specifier with
  // the `$lib` alias, which covers every one of those forms in a single
  // check. Matches a quote immediately followed by `$lib/` (how the alias is
  // always actually used) rather than the bare substring `$lib`, so this
  // doesn't false-positive on the header comments in both files that
  // mention `` `$lib` `` in backticked prose. Keep the regex check above too
  // — this one doesn't explain *which* import is wrong.
  it('competitors.js and paths.js never reference the $lib alias', () => {
    for (const src of [competitorsSrc, pathsSrc]) {
      expect(src).not.toMatch(/['"]\$lib\//);
    }
  });
});

describe('prerender entries parity (anti-drift)', () => {
  // svelte.config.js spreads ...comparePaths() into its locale × page matrix.
  // Deleting that spread doesn't fail any existing test (sitemap parity above
  // only checks LOCALIZED_PAGES, not the prerender config) — the build would
  // stay green while quietly no longer prerendering any deep-dive page. This
  // asserts the prerender entries directly so that regression is caught.
  it('svelte.config.js prerenders every published competitor deep-dive, in every locale', () => {
    const entries = config.kit?.prerender?.entries;
    if (!entries) throw new Error('svelte.config.js has no kit.prerender.entries to check');

    const published = COMPETITORS.filter((c) => c.published);
    for (const { code } of LOCALES) {
      for (const c of published) {
        expect(entries).toContain(`/${code}/compare/${c.slug}`);
      }
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
