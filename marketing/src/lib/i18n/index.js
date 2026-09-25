// i18n setup for the Nels marketing site.
//
// Unlike the frontend (a runtime SPA), the marketing site is prerendered: each
// locale is its own indexable URL under /[lang]/…, and the page copy is baked
// into static HTML at build time. The active locale is therefore driven by the
// route param in [lang]/+layout.js (which calls locale.set + waitLocale), NOT
// by navigator/localStorage. Catalogs are registered synchronously so $_ is
// ready during SSR/prerender with no flash of message keys.
import { addMessages, init, locale, waitLocale } from 'svelte-i18n';

import en from './locales/en.json';
import es from './locales/es.json';
import fr from './locales/fr.json';
import de from './locales/de.json';
import it from './locales/it.json';
import pt from './locales/pt.json';

export const STORAGE_KEY = 'nels_lang';
export const FALLBACK = 'en';

// Display order in the picker; labels are endonyms (the language's own name).
export const LOCALES = [
  { code: 'en', label: 'English' },
  { code: 'es', label: 'Español' },
  { code: 'fr', label: 'Français' },
  { code: 'de', label: 'Deutsch' },
  { code: 'it', label: 'Italiano' },
  { code: 'pt', label: 'Português' },
];

const CODES = LOCALES.map((l) => l.code);

/** @param {string} code */
export function isLocale(code) {
  return CODES.includes(code);
}

// Drop the non-message `_meta` flag (machine-translation status) before
// registering, so it never becomes a lookup key and the dictionaries satisfy
// svelte-i18n's string-valued LocaleDictionary type.
/** @param {Record<string, any>} dict @returns {any} */
function messages(dict) {
  const { _meta, ...rest } = dict;
  return rest;
}

addMessages('en', messages(en));
addMessages('es', messages(es));
addMessages('fr', messages(fr));
addMessages('de', messages(de));
addMessages('it', messages(it));
addMessages('pt', messages(pt));

init({ fallbackLocale: FALLBACK, initialLocale: FALLBACK });

// Persist the visitor's language choice so a future smart apex redirect can
// honour it. The route prefix remains the source of truth for the active locale.
/** @param {string} code */
export function persistLocale(code) {
  if (!isLocale(code)) return;
  try {
    localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode).
  }
}

export { locale, waitLocale };
