// Text-size management for Nels: persists the user's choice and applies it to
// the document via a data-text-size attribute, which app.css maps to a root
// font-size. Mirrors src/lib/theme.js's contract.
//
//   "small"  -> data-text-size="small"
//   "normal" -> data-text-size="normal"
//   "large"  -> data-text-size="large"

export const STORAGE_KEY = "nels_textsize";

// Order the picker displays: small, normal, large.
export const SIZES = ["small", "normal", "large"];

// Read the saved choice, defaulting to "normal" when absent or invalid.
export function getStoredTextSize() {
  const stored =
    typeof localStorage !== "undefined" ? localStorage.getItem(STORAGE_KEY) : null;
  return SIZES.includes(stored) ? stored : "normal";
}

// Apply a text-size choice to <html> and persist it. The data-text-size
// attribute drives the root font-size rules in app.css.
export function applyTextSize(size) {
  const choice = SIZES.includes(size) ? size : "normal";
  document.documentElement.setAttribute("data-text-size", choice);
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode); size still applies.
  }
  return choice;
}
