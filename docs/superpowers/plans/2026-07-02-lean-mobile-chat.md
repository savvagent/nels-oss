# Lean Mobile Chat + De-emphasized History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep long chat sessions responsive on mobile by rendering only the most recent messages (with on-demand full scrollback), add a one-tap "New Chat" button, and turn the conversation-history sidebar into an opt-in drawer on every screen size.

**Architecture:** Purely a client/display change — the model's context and recall are untouched (context is rebuilt server-side from the DB on every turn; every message stays embedded in pgvector regardless of what the browser renders). The chat sluggishness on long sessions is driven by DOM node count / Svelte reactivity over a large list, so we window the *rendered* slice (default: most recent 30) and let the user reveal the rest on demand, re-fetching the authoritative transcript from the existing `GET /conversations/:id/messages` endpoint. The sidebar changes from a desktop-persistent panel to a fixed off-canvas drawer everywhere. The windowing math is extracted into a pure, framework-free module (mirroring `frontend/src/lib/commands.js`) and unit-tested with a newly-added minimal Vitest setup; the Svelte/CSS changes are gated on `npm run build` plus manual verification in the running app (there is no component-test harness and adding one for this is disproportionate).

**Tech Stack:** Svelte 5 (runes), Vite, Tailwind v4 + DaisyUI, `lucide-svelte`, `svelte-i18n`; Vitest (added here) for the pure unit test.

---

## File Structure

- **Create** `frontend/src/lib/chatWindow.js` — pure windowing helper (`windowMessages`, `MAX_VISIBLE_MESSAGES`). One responsibility: decide which messages to render.
- **Create** `frontend/src/lib/chatWindow.test.js` — Vitest unit tests for the helper.
- **Create** `frontend/vitest.config.js` — minimal Vitest config (node env, `src/**/*.test.js`).
- **Modify** `frontend/package.json` — add `vitest` devDependency + `test` / `test:watch` scripts.
- **Modify** `frontend/src/App.svelte` — window the rendered transcript, add the "Show earlier" affordance + `showEarlierMessages()` handler, reset the reveal flag at conversation boundaries, add the header "New Chat" button, make the mobile hamburger + backdrop visible on all sizes, pass `expanded={true}` to the sidebar.
- **Modify** `frontend/src/lib/Sidebar.svelte` — change the `<aside>` from desktop-persistent to a fixed drawer on all breakpoints.
- **Modify** locale JSON files under `frontend/src/lib/i18n/` — add the `chat.showEarlier` key.

---

## Task 1: Pure windowing module + Vitest setup (TDD)

**Files:**
- Modify: `frontend/package.json`
- Create: `frontend/vitest.config.js`
- Create: `frontend/src/lib/chatWindow.test.js`
- Create: `frontend/src/lib/chatWindow.js`

- [ ] **Step 1: Add Vitest as a dev dependency**

Run (from `frontend/`):
```bash
npm install -D vitest
```
Expected: `vitest` added under `devDependencies` in `frontend/package.json`, lockfile updated.

- [ ] **Step 2: Add test scripts to `frontend/package.json`**

In the `"scripts"` block, add `test` and `test:watch` alongside the existing `dev`/`build`/`preview`:
```json
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "preview": "vite preview",
    "test": "vitest run",
    "test:watch": "vitest"
  },
```

- [ ] **Step 3: Create `frontend/vitest.config.js`**

A dedicated config so tests run in a plain node environment and do NOT pull in the Svelte Vite plugin:
```js
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.js"],
  },
});
```

- [ ] **Step 4: Write the failing test — `frontend/src/lib/chatWindow.test.js`**

```js
import { describe, it, expect } from "vitest";
import { windowMessages, MAX_VISIBLE_MESSAGES } from "./chatWindow.js";

const makeMessages = (n) =>
  Array.from({ length: n }, (_, i) => ({ sender: "user", message_text: `m${i}` }));

describe("windowMessages", () => {
  it("returns everything with no hidden count when under the cap", () => {
    const msgs = makeMessages(5);
    expect(windowMessages(msgs, false)).toEqual({ visible: msgs, hiddenCount: 0 });
  });

  it("returns everything when exactly at the cap", () => {
    const msgs = makeMessages(MAX_VISIBLE_MESSAGES);
    const { visible, hiddenCount } = windowMessages(msgs, false);
    expect(visible).toHaveLength(MAX_VISIBLE_MESSAGES);
    expect(hiddenCount).toBe(0);
  });

  it("keeps only the most recent `max` messages when over the cap", () => {
    const msgs = makeMessages(MAX_VISIBLE_MESSAGES + 12);
    const { visible, hiddenCount } = windowMessages(msgs, false);
    expect(visible).toHaveLength(MAX_VISIBLE_MESSAGES);
    expect(hiddenCount).toBe(12);
    // order preserved: last visible is the newest message
    expect(visible[visible.length - 1]).toBe(msgs[msgs.length - 1]);
    // first visible is the (hiddenCount)-th original message
    expect(visible[0]).toBe(msgs[12]);
  });

  it("reveals the full list when showAll is true", () => {
    const msgs = makeMessages(MAX_VISIBLE_MESSAGES + 12);
    expect(windowMessages(msgs, true)).toEqual({ visible: msgs, hiddenCount: 0 });
  });

  it("respects a custom max", () => {
    const msgs = makeMessages(10);
    const { visible, hiddenCount } = windowMessages(msgs, false, 4);
    expect(visible).toHaveLength(4);
    expect(hiddenCount).toBe(6);
    expect(visible[0]).toBe(msgs[6]);
  });

  it("treats non-array input as empty", () => {
    expect(windowMessages(null, false)).toEqual({ visible: [], hiddenCount: 0 });
    expect(windowMessages(undefined, true)).toEqual({ visible: [], hiddenCount: 0 });
  });
});
```

- [ ] **Step 5: Run the test to verify it fails**

Run (from `frontend/`):
```bash
npm test
```
Expected: FAIL — cannot resolve `./chatWindow.js` (module does not exist yet).

- [ ] **Step 6: Implement `frontend/src/lib/chatWindow.js`**

```js
// Pure helpers for windowing the rendered chat transcript.
//
// A long session can accumulate hundreds of bubbles, which makes the message
// list sluggish on mobile (DOM node count + Svelte reactivity over a large
// list). We render only the most recent messages by default and let the user
// reveal the rest on demand. Keeping the slice math here — framework-free,
// mirroring commands.js — lets it be unit-tested without mounting the Svelte
// component. The component owns the reactive state and the on-demand re-fetch;
// this module only decides what to show. Recall is unaffected: every message
// stays in the DB and pgvector regardless of what the client renders.

export const MAX_VISIBLE_MESSAGES = 30;

// Given the full message list and whether the user has chosen to reveal all,
// return { visible, hiddenCount }.
//   - showAll true, or list length at/under `max`: the whole list is visible,
//     hiddenCount 0.
//   - otherwise: `visible` is the most recent `max` messages (original order
//     preserved) and `hiddenCount` is how many older messages are withheld from
//     the top.
// Non-array input is treated as empty.
export function windowMessages(messages, showAll, max = MAX_VISIBLE_MESSAGES) {
  const list = Array.isArray(messages) ? messages : [];
  if (showAll || list.length <= max) {
    return { visible: list, hiddenCount: 0 };
  }
  return { visible: list.slice(-max), hiddenCount: list.length - max };
}
```

- [ ] **Step 7: Run the test to verify it passes**

Run (from `frontend/`):
```bash
npm test
```
Expected: PASS — all 6 `windowMessages` cases green.

- [ ] **Step 8: Commit**

```bash
git add frontend/package.json frontend/package-lock.json frontend/vitest.config.js frontend/src/lib/chatWindow.js frontend/src/lib/chatWindow.test.js
git commit -m "feat(frontend): add pure chat-window helper + vitest setup"
```

---

## Task 2: Window the rendered transcript in App.svelte

**Files:**
- Modify: `frontend/src/App.svelte` (import ~L37, state ~L54, handlers ~L784/L1198/L1205, send reset ~L992, render ~L2120)
- Modify: locale JSON under `frontend/src/lib/i18n/`

- [ ] **Step 1: Import the windowing helper**

In `frontend/src/App.svelte`, immediately after the `./lib/commands.js` import block (ends at line 37), add:
```js
  import { windowMessages } from "./lib/chatWindow.js";
```

- [ ] **Step 2: Add reveal state + derived window**

In the `<script>`, right after `let chatMessages = $state([]);` (line 54), add:
```js
  // When false (default) the transcript renders only its most recent slice to
  // keep long mobile sessions responsive; the "Show earlier messages" affordance
  // flips this to reveal the full history. Reset at every conversation boundary
  // and on send so a revealed history re-collapses to the window.
  let showAllMessages = $state(false);
  const chatWindow = $derived(windowMessages(chatMessages, showAllMessages));
```

- [ ] **Step 3: Add the `chat.showEarlier` i18n key to every locale file**

List the locale files:
```bash
ls frontend/src/lib/i18n/
```
For **each** locale JSON that contains a top-level `"chat"` object, add this key inside that object (English copy is an acceptable fallback for non-English locales until translated):
```json
    "showEarlier": "Show {count} earlier messages",
```
Verify the English file specifically, e.g. `frontend/src/lib/i18n/en.json`, has `chat.showEarlier` set to `"Show {count} earlier messages"`.

- [ ] **Step 4: Add the `showEarlierMessages` handler**

In `frontend/src/App.svelte`, immediately before `async function handleSelectConversation(id)` (line 1205), add:
```js
  // Reveal the full transcript for the current conversation. Older bubbles are
  // withheld from the DOM by default to keep long mobile sessions responsive.
  // When a persisted conversation is active we re-fetch the authoritative
  // history (so anything from earlier sessions is included too); otherwise we
  // just reveal what is already in memory. Recall is unaffected either way —
  // this only changes what is rendered. Drop the scroll pin so revealing does
  // not immediately yank the view back to the newest message.
  async function showEarlierMessages() {
    pinnedToBottom = false;
    if (activeConversationId) {
      try {
        chatMessages = await fetchApi(`/conversations/${activeConversationId}/messages`);
      } catch (e) {
        triggerError(t("alerts.loadConversationFailed"));
        return;
      }
    }
    showAllMessages = true;
  }
```

- [ ] **Step 5: Reset the reveal flag at conversation boundaries**

In `handleNewConversation` (line 1198), add `showAllMessages = false;` after `chatMessages = [];`:
```js
  function handleNewConversation() {
    chatMessages = [];
    showAllMessages = false;
    activeConversationId = null;
    fetchSuggestedQuestion();
    closeSidebar();
  }
```

In the `/clear` branch of `runCommand` (line 784), add `showAllMessages = false;` after `chatMessages = [];`:
```js
    if (parsed.name === "/clear") {
      // Non-destructive: start a fresh conversation view client-side. The
      // server-side thread is preserved (reachable via the sidebar); only the
      // active view is reset, mirroring the "New conversation" affordance.
      chatMessages = [];
      showAllMessages = false;
      activeConversationId = null;
      fetchSuggestedQuestion();
      pushAiMessage(t("commands.clearDone"));
      return;
    }
```

In `handleSelectConversation` (line 1205), add `showAllMessages = false;` right after `chatMessages = msgs;` so entering a thread starts windowed:
```js
      const msgs = await fetchApi(`/conversations/${id}/messages`);
      chatMessages = msgs;
      showAllMessages = false;
```

- [ ] **Step 6: Reset the reveal flag on send**

In `sendChatMessage`, at the line that sets `pinnedToBottom = true;` (line 992), add a reset immediately after it so a previously-revealed long history re-collapses to the window on the next turn:
```js
    pinnedToBottom = true;
    showAllMessages = false;
```

- [ ] **Step 7: Render the windowed slice + "Show earlier" affordance**

In the message-window template, replace the `{:else}` … `{#each chatMessages as msg}` opening (lines 2120-2121) so it renders the windowed slice and shows a reveal button when messages are hidden. Change:
```svelte
          {:else}
            {#each chatMessages as msg}
```
to:
```svelte
          {:else}
            {#if chatWindow.hiddenCount > 0}
              <div class="flex justify-center">
                <button
                  type="button"
                  onclick={showEarlierMessages}
                  class="btn btn-ghost btn-xs gap-1 text-base-content/60"
                >
                  <History class="w-3.5 h-3.5" />
                  {$_("chat.showEarlier", { values: { count: chatWindow.hiddenCount } })}
                </button>
              </div>
            {/if}
            {#each chatWindow.visible as msg}
```
(The matching `{/each}` at line 2139 is unchanged. `History` is already imported at line 16.)

- [ ] **Step 8: Verify the build compiles**

Run (from `frontend/`):
```bash
npm run build
```
Expected: build succeeds with no Svelte/Vite errors.

- [ ] **Step 9: Verify behavior in the running app**

Run (from `frontend/`): `npm run dev`, sign in, open chat. Confirm:
- A short conversation renders normally with no "Show earlier" button.
- After a conversation exceeds 30 messages (send enough turns, or open an existing long thread from the sidebar), only the most recent ~30 bubbles render and a "Show N earlier messages" button appears at the top.
- Clicking it reveals the full transcript without jumping to the bottom.
- Starting a new conversation, `/clear`, switching threads, and sending a new message each return to the windowed view.

- [ ] **Step 10: Run the unit test (still green) and commit**

```bash
cd frontend && npm test
git add frontend/src/App.svelte frontend/src/lib/i18n/
git commit -m "feat(frontend): window chat transcript with on-demand scrollback"
```
Expected: `npm test` PASS.

---

## Task 3: One-tap "New Chat" header button

**Files:**
- Modify: `frontend/src/App.svelte` (import ~L11-17, header ~L1560)

- [ ] **Step 1: Import the pencil icon**

In the `lucide-svelte` import (lines 3-17 of `frontend/src/App.svelte`), add `PenSquare` to the list (e.g. after `History,`):
```js
    History,
    PenSquare,
  } from "lucide-svelte";
```

- [ ] **Step 2: Add the New Chat button after the mobile hamburger**

In the top bar's LEFT cluster, the hamburger `<button>` ends at line 1560 (`</button>`), still inside the `{#if activeScreen === "chat"}` block that closes at line 1561. Insert the New Chat button between them — after the hamburger's `</button>` and before `{/if}`:
```svelte
        <button
          type="button"
          class="p-1.5 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
          onclick={handleNewConversation}
          title={$_("sidebar.newConversation")}
          aria-label={$_("sidebar.newConversation")}
        >
          <PenSquare class="w-5 h-5" />
        </button>
```
(Reuses the existing `sidebar.newConversation` i18n key, so no new locale entries. Unlike the hamburger it has no `md:hidden`, so it shows on every screen size.)

- [ ] **Step 3: Verify the build compiles**

Run (from `frontend/`):
```bash
npm run build
```
Expected: build succeeds.

- [ ] **Step 4: Verify behavior in the running app**

With `npm run dev`: on both a narrow (mobile) and wide (desktop) viewport, confirm the pencil "New Chat" button appears in the header on the chat screen, and clicking it clears the transcript to a fresh conversation (same effect as the sidebar's "New conversation") without needing to type `/clear`.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "feat(frontend): add one-tap New Chat button to chat header"
```

---

## Task 4: Make conversation history an opt-in drawer on all sizes

**Files:**
- Modify: `frontend/src/lib/Sidebar.svelte` (aside classes ~L115-127)
- Modify: `frontend/src/App.svelte` (hamburger ~L1549, sidebar prop ~L1725, backdrop ~L1749)

- [ ] **Step 1: Convert the sidebar `<aside>` to a fixed drawer on all breakpoints**

In `frontend/src/lib/Sidebar.svelte`, replace the `<aside>` class attribute (lines 119-126) — which currently makes the panel `md:static` (desktop-persistent) with an icon-rail width toggle — with a drawer-only version:
```svelte
  class="bg-base-200 border-r border-base-300 flex flex-col
         fixed top-[52px] bottom-0 left-0 z-50 w-72
         transform transition-transform duration-250 ease-in-out motion-reduce:transition-none
         {open ? 'translate-x-0' : '-translate-x-full'}"
```
(Drops every `md:`/`lg:` persistence and the `expanded ? 'md:w-64' : 'md:w-14'` rail-width toggle. The panel is now a full-width off-canvas drawer hidden by default and slid in when `open` is true.)

- [ ] **Step 2: Force the drawer's internal labels to always show**

In `frontend/src/App.svelte`, the `<Sidebar>` render passes `expanded={sidebarExpanded}` (line 1725). The drawer is now always full-width, so its internal labels/empty-state (which are hidden when `expanded` is false via `md:hidden`) must always show. Change line 1725 to:
```svelte
        expanded={true}
```
(`sidebarExpanded` state becomes unused; leaving it is harmless. Optional follow-up: remove `let sidebarExpanded` and its toggle.)

- [ ] **Step 3: Show the hamburger toggle on all screen sizes**

In `frontend/src/App.svelte`, the hamburger button class (line 1549) starts with `md:hidden`. Remove `md:hidden` so the drawer toggle is available on desktop too:
```svelte
          class="p-1.5 -ml-1 rounded-lg text-base-content/80 hover:text-base-content hover:bg-base-100"
```

- [ ] **Step 4: Show the drawer backdrop on all screen sizes**

In `frontend/src/App.svelte`, the drawer backdrop button class (line 1749) starts with `md:hidden`. Remove `md:hidden` so the dimming backdrop appears on desktop when the drawer is open:
```svelte
          class="fixed inset-0 top-[52px] z-40 bg-black/60 transition-opacity motion-reduce:transition-none"
```

- [ ] **Step 5: Verify the build compiles**

Run (from `frontend/`):
```bash
npm run build
```
Expected: build succeeds.

- [ ] **Step 6: Verify behavior in the running app**

With `npm run dev`, at both mobile and desktop widths confirm:
- The chat panel is full-width by default; the history sidebar is NOT shown persistently on desktop.
- Tapping the header hamburger slides the history drawer in over the content with a dimmed backdrop; tapping the backdrop, the drawer's X, or selecting a conversation closes it.
- Conversation labels and the empty-state render fully inside the drawer (not clipped to an icon rail).
- Selecting / renaming / deleting a conversation and "New conversation" still work from the drawer.

- [ ] **Step 7: Commit**

```bash
git add frontend/src/lib/Sidebar.svelte frontend/src/App.svelte
git commit -m "feat(frontend): make conversation history an opt-in drawer on all sizes"
```

---

## Done-When

- `frontend` `npm test` passes (windowing helper unit-tested).
- `frontend` `npm run build` passes.
- Long conversations render a windowed transcript with working on-demand "Show earlier messages".
- A one-tap "New Chat" button is in the chat header on all screen sizes; typing `/clear` is no longer required on mobile.
- The conversation-history sidebar is a hidden-by-default drawer on desktop and mobile, opened via the header toggle, with all its actions intact.
- No backend changes; model context and semantic recall are unchanged.
