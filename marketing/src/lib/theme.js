// Theme management for the Nels marketing site. Mirrors frontend/src/lib/theme.js
// so the site and product share one contract:
//
//   "light"  -> data-theme="nels-light"
//   "dark"   -> data-theme="nels-dark"
//   "system" -> no data-theme attribute; daisyUI auto-resolves from the OS
//               (--default = nels-light, --prefersdark = nels-dark)

export const STORAGE_KEY = 'nels_theme';

// Order the picker displays: light, system, dark.
export const THEMES = ['light', 'system', 'dark'];

/** @type {Record<string, string>} */
const THEME_ATTR = {
  light: 'nels-light',
  dark: 'nels-dark',
};

// Read the saved choice, defaulting to "system" when absent or invalid.
export function getStoredTheme() {
  const stored = typeof localStorage !== 'undefined' ? localStorage.getItem(STORAGE_KEY) : null;
  return typeof stored === 'string' && THEMES.includes(stored) ? stored : 'system';
}

// Apply a theme choice to <html> and persist it. "system" removes the
// attribute so daisyUI's prefers-color-scheme resolution takes over.
/** @param {string} theme */
export function applyTheme(theme) {
  const choice = THEMES.includes(theme) ? theme : 'system';
  const root = document.documentElement;
  const attr = THEME_ATTR[choice];
  if (attr) {
    root.setAttribute('data-theme', attr);
  } else {
    root.removeAttribute('data-theme');
  }
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode); theme still applies.
  }
  return choice;
}
