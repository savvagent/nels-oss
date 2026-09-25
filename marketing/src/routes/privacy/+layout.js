import { locale, waitLocale, FALLBACK } from '$lib/i18n/index.js';

export const prerender = true;
export const trailingSlash = 'never';

// The legal pages live outside the [lang] tree and are English-only. svelte-i18n's
// active locale is a module-level singleton shared across prerendered pages and
// preserved across client navigations, so we must explicitly reset it to English
// here — otherwise arriving from a localized page (e.g. /de/features → /privacy)
// would render the nav/footer in the wrong language.
export async function load() {
  locale.set(FALLBACK);
  await waitLocale(FALLBACK);
  return {};
}
