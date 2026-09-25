import { error } from '@sveltejs/kit';
import { locale, waitLocale, isLocale } from '$lib/i18n/index.js';

export const prerender = true;
export const trailingSlash = 'never';

export async function load({ params }) {
  const { lang } = params;
  if (!isLocale(lang)) {
    throw error(404, `Unknown locale: ${lang}`);
  }
  // Set the active locale for this route and wait for its catalog so the page
  // renders fully translated during prerender (no flash of message keys).
  locale.set(lang);
  await waitLocale(lang);
  return { lang };
}
