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
