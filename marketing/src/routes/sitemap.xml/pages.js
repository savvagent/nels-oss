// Page list consumed by the sitemap endpoint. Lives in its own module (not
// +server.js) because SvelteKit endpoint files only permit a fixed set of
// exports (GET, POST, prerender, entries, ...) — adding LOCALIZED_PAGES there
// fails the build's postbuild export-validation step. Exported here so a unit
// test can assert parity with the competitor data.
import { comparePaths } from '$lib/compare/paths.js';

// Localized pages live under every locale prefix; legal pages are English-only
// and stay at the top level.
export const LOCALIZED_PAGES = [
  '', '/features', '/how-it-works', '/pricing', '/security', '/compare', '/about', '/faq',
  ...comparePaths(),
];
