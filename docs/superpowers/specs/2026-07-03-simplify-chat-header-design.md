# Simplify busy chat header (#232) — Design

## Goal
Declutter the chat header (`frontend/src/App.svelte`, "Consolidated Top Bar" ~L1565) so the
active budget and the conversation are the visual focus: drop the header's clear/new-conversation
control and the Insights button, strip a trailing "Budget" word from the displayed budget name, and
collapse the 8 status badges into a single info icon/popover — without losing any reachable
functionality, and with mobile/desktop parity.

## Key decisions (assumptions)
- **"The clear button"** = the header's `PenSquare` "New conversation" icon button (L1588-1596,
  `onclick={handleNewConversation}`) — the only conversation-clearing control in the header. It
  stays reachable from the sidebar, already wired to the same `handleNewConversation` via
  `onNewConversation`.
- **"Budget" strip is trailing-word-only**, case-insensitive (e.g. "Groceries Budget" ->
  "Groceries"); mid-string occurrences untouched. If stripping empties the string (name is exactly
  "Budget"), fall back to the original name so the header never renders blank.
- **Popover, not hover tooltip**, for the info affordance — mirrors the existing
  `Notifications.svelte` click-toggle + outside-click/Escape-close pattern; a hover-only tooltip is
  unusable on touch and mobile/desktop parity is an explicit AC line.
- **One icon instance**, not duplicated per breakpoint: rendered once after both the mobile and
  desktop budget-name spans. Since those spans are already mutually exclusive by breakpoint
  (`md:hidden` / `hidden md:inline`) and the icon carries no responsive-visibility classes, it
  naturally sits next to whichever name is visible.
- **Icon omitted when zero status flags are active**, mirroring today's zero-badges behavior.
- **No new i18n keys for popover content** (status labels/descriptions stay hardcoded English,
  matching the existing badge text/titles); only the icon's own aria-label gets a new key
  (`header.statusInfoLabel`) across all 6 locales, matching how other header/sidebar aria-labels
  are done.
- **No new component-level tests**: vitest runs `environment: "node"` with no jsdom/testing-library;
  every existing test covers a pure `.js` module. The new display logic is extracted into a pure
  `.js` module specifically so it's unit-testable the same way (mirrors `chatWindow.js`).

## Architecture
- `frontend/src/lib/budgetDisplay.js` (new, pure): `displayBudgetName(name)` (trailing-word strip)
  and `activeBudgetStatuses(budget)` (ordered `{key,label,description}[]`, replicating the exact
  conditions/label/title text of the 8 badges being removed: project, fixed, closed, archived,
  rollover(+partial), auto-renew(+next date), rollup(+count/amount), rolled-up).
- `frontend/src/lib/BudgetStatusInfo.svelte` (new): `budget` prop, `$derived` statuses, renders
  nothing when empty; otherwise an `Info` (lucide-svelte) icon button (`aria-haspopup`,
  `aria-expanded`, i18n aria-label) toggling a popover listing each status's label + description,
  using the same open/outside-click/Escape-close pattern as `Notifications.svelte`.
- `App.svelte`: delete the `PenSquare` button; delete the `BarChart3` Insights button (keep
  `<Notifications>`); swap both name spans to `displayBudgetName(activeBudget.name)`; delete both
  badge clusters (mobile ~L1620-1666, desktop ~L1684-1730); add one
  `<BudgetStatusInfo budget={activeBudget} />` guarded by the existing `{#if activeBudget}`.
- i18n: add `header.statusInfoLabel` to all 6 locale files (`en`/`de`/`es`/`fr`/`it`/`pt`).

## Success criteria (all AC-covered)
1. No clear/new-conversation button renders in the header (mobile or desktop); still reachable from
   the sidebar.
2. No Insights button renders in the header; still reachable from the sidebar and the assistant's
   `showInsights` signal.
3. Header budget name has a trailing "Budget" word stripped for display only; `activeBudget.name`
   itself is untouched.
4. The 8 status badges are gone, replaced by one info icon communicating the same details on
   demand.
5. Identical behavior on mobile and desktop.

## Tests
- Pure: `frontend/src/lib/budgetDisplay.test.js` — `displayBudgetName` (trailing strip,
  case-insensitivity, no-op mid-string/absent, empty-after-strip fallback, null/undefined input);
  `activeBudgetStatuses` (each flag independently, project suppressing rollover/auto-renew, rollup
  vs. rolled-up mutual exclusivity, zero-flag -> `[]`, null budget -> `[]`).
- Manual: `pnpm run dev` — header renders without clear/Insights buttons on both a narrow and wide
  viewport; budget name shows trimmed; info icon opens/closes correctly; Insights still opens from
  the sidebar and via the assistant's `showInsights` flow.
- `cd frontend && pnpm test` and `cd frontend && pnpm run build` before opening the PR (no
  dedicated lint script in this package).

## Risks
- Popover description copy for Project/Closed/Archived is newly composed (no prior `title` existed
  for those three) — additive UX text, no behavior change.
- Icon placement (adjacent to the budget name) is a judgment call for "consistent across mobile and
  desktop" — chosen to preserve the removed badges' original visual association.
- Verify the `sidebar.newConversation` i18n key isn't orphaned once the header's `PenSquare` button
  is deleted (the sidebar's own "New conversation" entry likely reuses the same key — keep the key,
  just remove the header's usage of it).
