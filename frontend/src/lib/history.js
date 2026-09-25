// Pure helpers for the History page (#261) — the conversation grouping,
// relative-time formatting, and "first user message" lookup factored out of
// History.svelte so it's unit-testable under vitest's `environment: "node"`
// (no jsdom/Svelte compiler needed here), mirroring the existing
// frontend/src/lib/budgetsView.js / budgetDetails.js convention. No Svelte,
// no I/O — ported verbatim from Sidebar.svelte's existing grouping/rename
// logic (the conversation-list behavior this page replaces), just extracted
// so it can be tested directly instead of only through component rendering
// (this codebase has no jsdom/@testing-library/svelte setup — see
// vitest.config.js's `environment: "node"` — so every other page component
// in src/lib follows this same "logic lives in a plain .js sibling" split).

function startOfDay(d) {
  const x = new Date(d);
  x.setHours(0, 0, 0, 0);
  return x;
}

// Buckets `conversations` into Today / Yesterday / Earlier groups (relative
// to `now`, defaulting to the real current time), dropping empty buckets and
// preserving each bucket's original relative order. Mirrors
// Sidebar.svelte's existing `groups` $derived.by logic exactly.
export function groupConversations(conversations, now = new Date()) {
  const today = startOfDay(now);
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);

  const buckets = { Today: [], Yesterday: [], Earlier: [] };
  for (const c of conversations ?? []) {
    const day = startOfDay(c.updated_at);
    if (day.getTime() === today.getTime()) buckets.Today.push(c);
    else if (day.getTime() === yesterday.getTime()) buckets.Yesterday.push(c);
    else buckets.Earlier.push(c);
  }
  return ["Today", "Yesterday", "Earlier"]
    .map((label) => ({ label, items: buckets[label] }))
    .filter((g) => g.items.length > 0);
}

// Formats `iso` relative to `now` (defaulting to the real current time).
// Ported verbatim from Sidebar.svelte's existing relativeTime. The final
// "month day" fallback (5+ weeks out) is locale-sensitive — pass
// `localeCode` (e.g. from svelte-i18n's `$locale`) through from the
// component; this module has no i18n access of its own.
export function relativeTime(iso, now = new Date(), localeCode = "en") {
  const then = new Date(iso).getTime();
  const diff = now.getTime() - then;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h`;
  const days = Math.floor(hrs / 24);
  if (days < 7) return `${days}d`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}w`;
  return new Date(iso).toLocaleDateString(localeCode || "en", {
    month: "short",
    day: "numeric",
  });
}

// A conversation's display title, or null when it has none (empty/blank
// `title`). This module has no i18n access (see budgetsView.js's
// UNKNOWN_ROLLUP_LABEL precedent) — History.svelte substitutes its own
// translated fallback (`sidebar.newConversation`) when this returns null,
// exactly like Sidebar.svelte's existing titleFor did via get(_)(...).
export function titleFor(conversation) {
  const t = conversation?.title;
  return t && t.trim() ? t : null;
}

// Whether committing a rename should actually fire onRename: the trimmed
// value must be non-empty AND different from the conversation's current
// display title (which may itself be a translated fallback — the caller
// passes the already-resolved current title, not the raw `title` field, so
// renaming an untitled conversation to its own fallback text still no-ops).
// Ported verbatim from Sidebar.svelte's existing commitRename guard.
export function shouldCommitRename(newValue, currentTitle) {
  const v = (newValue ?? "").trim();
  return Boolean(v) && v !== currentTitle;
}

// The first user-authored message in `messages` (assumed ordered oldest ->
// newest, matching GET /conversations/:id/messages), or null if the
// conversation has none (e.g. an all-assistant/system thread). The
// backend's ChatHistoryItem uses a `sender` field with value "user" for
// user-authored messages (not "role") — confirmed against backend/src/rag.rs.
export function findFirstUserMessage(messages) {
  return (messages ?? []).find((m) => m?.sender === "user") ?? null;
}
