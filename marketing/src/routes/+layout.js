// Prerender every route to static HTML — best SEO, $0 hosting.
// Importing the i18n module here runs init() once for the whole app, so $_
// resolves on every route — including the English-only legal pages that live
// outside the [lang] tree.
import '$lib/i18n/index.js';
import { waitLocale } from 'svelte-i18n';

export const prerender = true;
export const trailingSlash = 'never';

export async function load() {
  await waitLocale();
  return {};
}
