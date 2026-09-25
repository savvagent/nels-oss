import adapter from '@sveltejs/adapter-cloudflare';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { comparePaths } from './src/lib/compare/paths.js';

// Locale × page matrix for prerendering. The dynamic [lang] route has a required
// param, so the default '*' entry can't reach it — list every /{lang}/… URL
// explicitly. '*' still covers the English-only legal pages, sitemap, and robots.
const LOCALES = ['en', 'es', 'fr', 'de', 'it', 'pt'];
const PAGES = [
  '', '/features', '/how-it-works', '/pricing', '/security', '/compare', '/about', '/faq',
  ...comparePaths(),
];
const localeEntries = /** @type {Array<`/${string}`>} */ (
  LOCALES.flatMap((l) => PAGES.map((p) => `/${l}${p}`))
);

// The [competitor] deep-dive route inherits `prerender = true` from the [lang]
// layout, but until a competitor is published (Phase 2) comparePaths() is
// empty, so the crawler legitimately renders zero instances of it. SvelteKit
// treats a prerenderable route with zero instances as a build error by
// default; allow it for this one route id while still failing hard on any
// other route that goes unseen.
//
// TEMPORARY, Phase-1-only allowance: the moment comparePaths() is non-empty
// (a competitor is published), the allowance disables itself and an unseen
// [competitor] route becomes a real build failure again, same as any other
// route. This closes the case where a regression makes publishedCompetitors()
// wrongly return [] again — the build would otherwise stay green. It does NOT
// catch a subset of published competitors silently failing to prerender;
// SvelteKit only tracks "seen" per route id, not per instance, so one
// successfully-crawled competitor clears the whole route id. That's an
// inherent SvelteKit limitation, out of scope here.
/** @param {{ routes: string[], message: string }} details */
function handleUnseenRoutes({ routes, message }) {
  const expectedEmpty = new Set(['/[lang]/compare/[competitor]']);
  const stillExpectedEmpty = comparePaths().length === 0;
  if (!stillExpectedEmpty || routes.some((id) => !expectedEmpty.has(id))) {
    throw new Error(message);
  }
}

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      routes: {
        include: ['/*'],
        exclude: [
          '<build>',
          '<files>',
          '/en', '/es', '/fr', '/de', '/it', '/pt',
          '/en/*', '/es/*', '/fr/*', '/de/*', '/it/*', '/pt/*',
          '/privacy', '/terms', '/robots.txt', '/sitemap.xml',
        ],
      },
    }),
    prerender: {
      entries: ['*', ...localeEntries],
      handleUnseenRoutes,
    },
  },
};
export default config;
