import { locale, waitLocale, FALLBACK } from '$lib/i18n/index.js';

export const prerender = true;
export const trailingSlash = 'never';

// English-only legal page outside the [lang] tree. Reset svelte-i18n's shared
// locale singleton to English so arriving from a localized page doesn't leave
// the nav/footer in the wrong language. See privacy/+layout.js for details.
export async function load() {
  locale.set(FALLBACK);
  await waitLocale(FALLBACK);
  return {};
}
