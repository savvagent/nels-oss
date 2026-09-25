# Return to Chat After Send Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Submitting a prompt from any non-chat outlet route (categories, insights, budgetDetails, budgetsList, history, settings, deleteAccount) returns the view to the chat transcript, scrolled to the bottom, so the assistant's reply is visible — while an explicit post-response navigation (a navigating slash-command, or `open_insights`/`open_budgets_list`/`categories_table_html`) still wins and lands on its target outlet.

**Architecture:** `performSend` in `frontend/src/App.svelte` already has a "deliberate action" block at its top (`pinnedToBottom = true; showAllMessages = false;`) that runs before slash-command interception and before the `/chat` network round-trip. Adding `navigate("chat")` to that block resets the outlet to chat first; any navigation the response itself triggers (later in the same function, after the `await`) runs afterward and overrides it. `navigate()` already does an unconditional `route = ROUTES.includes(name) ? name : "chat"` plus a guarded hash assignment (no-op if the hash is already correct), so this single call correctly resets away from all 7 outlet routes, including `budgetDetails`/`budgetsList` which aren't in the flat `ROUTES` array.

**Tech Stack:** Svelte 5 (runes), Vite, vitest (existing pure-module tests only — no component harness for `App.svelte`).

**On the ticket's "extract a pure helper" testing suggestion:** declined. The ticket floats extracting the "where should a submit land" decision into a pure, `router.test.js`-style helper *if automated coverage is desired*. The actual resolution here is "always chat, unconditionally, on every deliberate send" — there is no conditional branching to isolate into a helper (a function that always returns the same constant has no test value beyond what reading the source already tells you). `router.js`/`router.test.js` stay untouched; verification of this change is necessarily manual (Task 2), which is consistent with the ticket's own testing notes acknowledging no `App.svelte` component harness exists.

---

### Task 1: Add the chat-transcript reset to `performSend`

**Files:**
- Modify: `frontend/src/App.svelte:1165-1173` (inside `performSend`, the existing "deliberate action" comment block)

- [ ] **Step 1: Confirm current code at the insertion point**

Read `frontend/src/App.svelte` around line 1165 and confirm it still reads:

```js
  async function performSend(rawInput) {
    // Sending is a deliberate action: re-pin so the user's own message, the
    // loading indicator, and the reply scroll into view even if they had
    // scrolled up to read history (AC 2). The scroll-up guard still applies to
    // passively-arriving replies, not to content the user just submitted. #108
    // Re-pinning per dequeued item (not just the first) keeps this true across
    // an entire queue drain, not only the send that started it.
    pinnedToBottom = true;
    showAllMessages = false;

    // Intercept slash-commands before they reach the AI endpoint.
    if (isCommand(rawInput)) {
```

If line numbers have shifted (other work may have landed on `main` first), locate the block by this exact comment + code text instead of by line number.

- [ ] **Step 2: Add the `navigate("chat")` call**

Replace:

```js
    pinnedToBottom = true;
    showAllMessages = false;

    // Intercept slash-commands before they reach the AI endpoint.
    if (isCommand(rawInput)) {
```

With:

```js
    pinnedToBottom = true;
    showAllMessages = false;
    // Always return to the chat transcript on a deliberate submit (#278) —
    // otherwise a reply appended while an outlet view (categories/insights/
    // budgetDetails/budgetsList/history/settings/deleteAccount) is showing is
    // invisible behind that view. Runs before slash-command interception and
    // before the /chat round-trip below, so any navigation the response
    // itself triggers (a navigating slash-command, or open_insights /
    // open_budgets_list / categories_table_html further down this function)
    // still runs afterward and wins — this is only the default landing spot,
    // not an override of an explicit navigation. navigate("chat") is a no-op
    // on window.location.hash when already on chat (it compares before
    // assigning), so this adds no spurious browser-history entry for the
    // common case of submitting from chat itself.
    navigate("chat");

    // Intercept slash-commands before they reach the AI endpoint.
    if (isCommand(rawInput)) {
```

- [ ] **Step 3: Sanity-check no other call site needs the same fix**

Run:
```bash
cd frontend && grep -n "async function performSend\|function drainQueue\|function sendChatMessage" src/App.svelte
```
Expected: exactly one `performSend` definition, confirming `drainQueue`/`sendChatMessage` both funnel through the single `performSend` you just edited (no second send path to patch).

- [ ] **Step 4: Build check**

Run:
```bash
cd frontend && pnpm build
```
Expected: builds cleanly (no new warnings/errors). This is a plain Vite/Svelte production build — it will catch a syntax error in the edit immediately.

- [ ] **Step 5: Run the existing frontend test suite**

Run:
```bash
cd frontend && pnpm test
```
Expected: PASS, unchanged from before the edit (this change touches no pure module — `router.js`/`router.test.js` are untouched — so no test file changes and no new failures).

- [ ] **Step 6: Manual verification (no component harness exists for App.svelte — see Task 2)**

Do not commit yet — Task 2 covers driving the actual app to verify behavior before the commit, since this is a runtime UI behavior change with no automated coverage possible at the component level.

---

### Task 2: Manual verification against a running dev stack, then commit

**Files:** none (verification + the commit of Task 1's change)

- [ ] **Step 1: Start the local stack**

```bash
# from the repo root (one-time, if not already running)
podman-compose up -d
# terminal 1
cd backend && cargo run
# terminal 2
cd frontend && pnpm run dev
```
`GEMINI_API_KEY` need not be set — the backend's offline pattern-matching router still resolves category/insights/budgets navigation intents and simple chat turns deterministically enough to exercise the routing behavior under test here.

- [ ] **Step 2: Log in / register a test user, create a budget with at least one category**

Via the running frontend at `http://localhost:5173` (or the port `pnpm run dev` reports).

- [ ] **Step 3: Drive each outlet route → submit → verify reset**

For each of: Categories (`/categories-list` then a plain prompt), Insights (open via sidebar or a prompt that returns `open_insights`), Budget details (open a budget), Budgets list (`/budgets-list` or sidebar), History, Settings, Delete-account confirmation (reachable via the account menu) —
1. Navigate to the outlet.
2. Type a plain natural-language prompt (e.g. "add a $5 coffee transaction") and submit.
3. Confirm: the view switches to the chat transcript, the reply appears, and the transcript is scrolled to the bottom.

- [ ] **Step 4: Verify navigating slash-commands still land on their target**

From chat, run `/categories-list`. Confirm: after the brief round-trip the view ends on the Categories outlet (not stuck on chat). Repeat for `/budgets-insights` and `/budgets-list`. This exercises the `isCommand(rawInput)` early-return branch in `performSend` — a different code path from Step 4b below, so both need independent verification.

- [ ] **Step 4b: Verify navigation-signal chat responses still land on their target**

This is the more load-bearing check — it is the actual override race the fix depends on (the new `navigate("chat")` call runs before the `await fetchApi("/chat")`; these navigations run after it, inside the same `performSend`, at App.svelte's `open_insights`/`open_budgets_list`/`categories_table_html` handling). From an outlet view (e.g. Categories), submit a **natural-language prompt** (not a slash-command) worded to trigger each signal in turn:
- one that causes the backend to return `open_insights` (e.g. "show me insights on my spending") — confirm the view ends on the Insights outlet, not stuck on chat.
- one that causes `open_budgets_list` (e.g. "show me my budgets") — confirm it ends on the Budgets List outlet.
- one that causes `categories_table_html` (e.g. "list my categories") — confirm it ends on the Categories outlet.

If the offline pattern-matching router (no `GEMINI_API_KEY`) doesn't reliably produce one of these signals for a given phrasing, use the equivalent slash-command's underlying intent phrase from `frontend/src/lib/commands.js` as a guide, or fall back to briefly setting `GEMINI_API_KEY` for this verification pass only.

- [ ] **Step 5: Verify no spurious history entry when already on chat**

From the chat view (not an outlet), note the current history length (e.g. via browser devtools `history.length`, or simply confirm Back doesn't do anything unexpected), submit a plain prompt, and confirm Back does not land you back on "chat" as if a new entry were pushed — i.e. one Back press takes you to whatever you'd expect had you not submitted at all.

- [ ] **Step 6: Verify Back after an automatic switch**

From Categories, submit a plain prompt (triggering the auto-switch to chat). Press Back. Confirm it returns to the Categories outlet (hash stays in sync per `navigate()`'s always-push convention).

- [ ] **Step 7: Stop the dev stack**

Ctrl-C both `cargo run` and `pnpm run dev` terminals once verification passes. Leave `podman-compose` running only if other work in this repo needs it; otherwise `podman-compose down`.

- [ ] **Step 8: Commit**

```bash
cd frontend && git add src/App.svelte
git commit -m "fix(#278): return to chat transcript after every prompt submission"
```
