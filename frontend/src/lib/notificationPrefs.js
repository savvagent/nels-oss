// Stores per-category in-app notification preferences in localStorage
// (categories in CATEGORIES; default enabled). Read accessors are stable for any
// in-app notification feed that consumes these prefs.

export const STORAGE_KEY = "nels_notif_prefs";

// Same-tab change signal. The `storage` event only fires in OTHER tabs, so a
// Settings toggle in the current tab needs an in-tab event for live consumers
// (e.g. the notification bell badge) to re-read prefs immediately (#54 AC: apply
// immediately). Cross-tab updates still arrive via the native `storage` event.
export const PREFS_CHANGED_EVENT = "nels-notif-prefs-changed";

export const CATEGORIES = ["limitAlerts", "reminders"];

// Defaults: every known category enabled.
function defaults() {
  const map = {};
  for (const cat of CATEGORIES) map[cat] = true;
  return map;
}

// Return a boolean per CATEGORY. Missing keys stay true; unknown stored keys are
// ignored; values are coerced to Boolean. Parse failures fall back to defaults.
export function getStoredNotifPrefs() {
  const prefs = defaults();
  if (typeof localStorage === "undefined") return prefs;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === "object") {
        for (const cat of CATEGORIES) {
          // Own-property check (not `in`) so a stored key from the prototype
          // chain can't be mistaken for a real preference.
          if (Object.prototype.hasOwnProperty.call(parsed, cat)) {
            prefs[cat] = Boolean(parsed[cat]);
          }
        }
      }
    }
  } catch {
    // Non-fatal: corrupt/unavailable storage; fall back to defaults.
  }
  return prefs;
}

// Set one category's preference and persist the whole map. No-op for unknown
// categories. Returns the updated map.
export function setNotifPref(category, enabled) {
  const prefs = getStoredNotifPrefs();
  if (!CATEGORIES.includes(category)) return prefs;
  prefs[category] = !!enabled;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Non-fatal: persistence unavailable (e.g. private mode).
  }
  // Notify same-tab consumers so the badge/feed reflect the change immediately,
  // without waiting for the next panel-open. Guarded for non-browser (SSR/Node)
  // contexts; any environment with `window` also has `CustomEvent`.
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(PREFS_CHANGED_EVENT));
  }
  return prefs;
}

// Whether a category is currently enabled (default true for known categories).
export function isNotifEnabled(category) {
  return getStoredNotifPrefs()[category] === true;
}

// Map a backend notification `kind` to its preference CATEGORY (#55 feed
// honoring #54 prefs). The backend emits 'limit_warning' | 'limit_exceeded' |
// 'reminder'; limit kinds map to "limitAlerts", reminders to "reminders".
// Unknown/future kinds return null so they are never hidden by a stale mapping.
export function kindToCategory(kind) {
  switch (kind) {
    case "limit_warning":
    case "limit_exceeded":
      return "limitAlerts";
    case "reminder":
      return "reminders";
    default:
      return null;
  }
}

// Filter a list of notifications to only those whose category is enabled in
// `prefs` (defaults to the stored prefs). A notification whose kind has no known
// category (kindToCategory === null) is ALWAYS kept — a new notification type
// must not silently vanish just because the client predates its preference.
export function filterByPrefs(notifications, prefs = getStoredNotifPrefs()) {
  if (!Array.isArray(notifications)) return [];
  return notifications.filter((n) => {
    const cat = kindToCategory(n && n.kind);
    if (cat === null) return true; // unknown kind: show by default
    return prefs[cat] === true;
  });
}
