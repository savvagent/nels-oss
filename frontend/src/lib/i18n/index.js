// i18n setup for Nels. Mirrors the theme.js contract: a storage key, a getter
// that resolves the active locale, and a setter that applies + persists it.
import { addMessages, init, locale } from "svelte-i18n";
import { get } from "svelte/store";

import en from "./locales/en.json";
import es from "./locales/es.json";
import fr from "./locales/fr.json";
import de from "./locales/de.json";
import it from "./locales/it.json";
import pt from "./locales/pt.json";

export const STORAGE_KEY = "nels_lang";
export const FALLBACK = "en";

// Display order in the picker; labels are endonyms (the language's own name).
export const LOCALES = [
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "it", label: "Italiano" },
  { code: "pt", label: "Português" },
];

const SUPPORTED = LOCALES.map((l) => l.code);

// Register synchronously so messages exist before first paint (no flash of keys).
addMessages("en", en);
addMessages("es", es);
addMessages("fr", fr);
addMessages("de", de);
addMessages("it", it);
addMessages("pt", pt);

// Resolve initial locale: stored choice -> base of navigator.language -> fallback.
export function getStoredLocale() {
  let stored = null;
  try {
    stored = localStorage.getItem(STORAGE_KEY);
  } catch {
    stored = null;
  }
  if (stored && SUPPORTED.includes(stored)) return stored;
  const nav =
    typeof navigator !== "undefined" && navigator.language
      ? navigator.language.slice(0, 2)
      : null;
  if (nav && SUPPORTED.includes(nav)) return nav;
  return FALLBACK;
}

const initialLocale = getStoredLocale();

init({ fallbackLocale: FALLBACK, initialLocale });

if (typeof document !== "undefined") {
  document.documentElement.setAttribute("lang", initialLocale);
}

// Active base code (region stripped), e.g. "es". Used when calling the backend.
export function currentLocale() {
  const l = get(locale) || FALLBACK;
  return l.slice(0, 2);
}

// Apply + persist a locale choice. Unknown codes fall back silently.
export function setLocale(code) {
  const choice = SUPPORTED.includes(code) ? code : FALLBACK;
  locale.set(choice);
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode).
  }
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("lang", choice);
  }
  return choice;
}
