# FIFO Chat Queue + Auto-Send Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user keep typing, dictating, recalling prompt history, and running slash commands
while Nels is generating a reply (`frontend/src/App.svelte`), queuing anything submitted mid-flight
and auto-draining it strictly FIFO, one request at a time, once the current turn resolves —
savvagent/nels#230.

**Architecture:** A new pure module `frontend/src/lib/chatQueue.js` owns FIFO queue mechanics (cap
enforcement, enqueue/dequeue/remove, and the drain loop itself as `runDrain(getQueue, sendFn,
onQueueChange)` — a *live accessor*, not a one-time array, so a drain in progress always sees an
item enqueued or cancelled while a previous item's `sendFn` is still pending) so the
cap/ordering/resilience logic is unit-tested without needing a component-test harness (this repo
has none — `vitest` runs `environment: "node"`). `App.svelte`'s `sendChatMessage` is refactored
into three functions: `sendChatMessage` (the form/Enter entry point — enqueues, then kicks off a
drain), `drainQueue` (re-entrancy-guarded wrapper around `runDrain`), and `performSend` (exactly
today's send body, parameterized on the raw text instead of reading `chatInput`). Every send — busy
or not — goes through the same enqueue-then-drain path, so there is no separate "direct send"
branch to keep in sync. A new `QueuedMessages.svelte` popover (mirrors the existing
`BudgetStatusInfo.svelte` click-toggle/outside-click/Escape-close pattern) surfaces the "N queued"
indicator with per-item cancel + clear-all. See design doc:
`docs/superpowers/specs/2026-07-03-fifo-chat-queue-design.md`.

**Tech Stack:** Svelte 5 (runes), Vite, Tailwind v4 + DaisyUI, `lucide-svelte`, `svelte-i18n`,
Vitest (`environment: "node"`, existing config at `frontend/vitest.config.js`).

---

## File Structure

- **Create** `frontend/src/lib/chatQueue.js` — pure FIFO queue helpers: `QUEUE_CAP`, `canEnqueue`,
  `enqueue`, `dequeue`, `removeFromQueue`, `runDrain`.
- **Create** `frontend/src/lib/chatQueue.test.js` — Vitest unit tests for all of the above,
  including regression tests proving `runDrain` survives a rejecting `sendFn` and correctly picks
  up an enqueue/cancel that happens while a previous item's `sendFn` call is still pending.
- **Create** `frontend/src/lib/QueuedMessages.svelte` — the "N queued" pill + preview/cancel
  popover.
- **Modify** `frontend/src/App.svelte` — imports, new `pendingQueue`/`isDraining` state, relaxed
  arrow-key recall guards, `sendChatMessage`/`drainQueue`/`performSend` refactor, relaxed
  textarea/mic/recall disables, send-button gating, mount `QueuedMessages`, queue-full hint.
- **Modify** locale JSON files under `frontend/src/lib/i18n/locales/` (`en`, `de`, `es`, `fr`, `it`,
  `pt`) — add 5 new `chat.queue*` keys to each.

---

## Task 1: Pure FIFO queue module (TDD)

**Files:**
- Create: `frontend/src/lib/chatQueue.test.js`
- Create: `frontend/src/lib/chatQueue.js`

- [ ] **Step 1: Write the failing tests — `frontend/src/lib/chatQueue.test.js`**

```js
import { describe, it, expect, vi } from "vitest";
import {
  QUEUE_CAP,
  canEnqueue,
  enqueue,
  dequeue,
  removeFromQueue,
  runDrain,
} from "./chatQueue.js";

describe("canEnqueue", () => {
  it("is true for an empty queue", () => {
    expect(canEnqueue([])).toBe(true);
  });

  it("is true one below the cap", () => {
    const queue = Array.from({ length: QUEUE_CAP - 1 }, (_, i) => ({ id: `${i}`, text: `m${i}` }));
    expect(canEnqueue(queue)).toBe(true);
  });

  it("is false at the cap", () => {
    const queue = Array.from({ length: QUEUE_CAP }, (_, i) => ({ id: `${i}`, text: `m${i}` }));
    expect(canEnqueue(queue)).toBe(false);
  });

  it("respects a custom cap", () => {
    expect(canEnqueue([{ id: "1", text: "a" }], 1)).toBe(false);
    expect(canEnqueue([], 1)).toBe(true);
  });
});

describe("enqueue", () => {
  it("appends to an empty queue", () => {
    expect(enqueue([], "id1", "hello")).toEqual([{ id: "id1", text: "hello" }]);
  });

  it("appends to the end, preserving existing order", () => {
    const queue = [{ id: "id1", text: "first" }];
    expect(enqueue(queue, "id2", "second")).toEqual([
      { id: "id1", text: "first" },
      { id: "id2", text: "second" },
    ]);
  });

  it("does not mutate the input array", () => {
    const queue = [{ id: "id1", text: "first" }];
    enqueue(queue, "id2", "second");
    expect(queue).toEqual([{ id: "id1", text: "first" }]);
  });

  it("refuses to grow past the cap, returning the same reference", () => {
    const queue = Array.from({ length: QUEUE_CAP }, (_, i) => ({ id: `${i}`, text: `m${i}` }));
    const result = enqueue(queue, "overflow", "nope");
    expect(result).toBe(queue);
    expect(result).toHaveLength(QUEUE_CAP);
  });

  it("respects a custom cap", () => {
    const queue = [{ id: "1", text: "a" }];
    expect(enqueue(queue, "2", "b", 1)).toBe(queue);
  });
});

describe("dequeue", () => {
  it("returns null item and the original (empty) array for an empty queue", () => {
    const empty = [];
    expect(dequeue(empty)).toEqual({ item: null, rest: empty });
  });

  it("pops the oldest item first (FIFO)", () => {
    const queue = [
      { id: "1", text: "first" },
      { id: "2", text: "second" },
    ];
    const { item, rest } = dequeue(queue);
    expect(item).toEqual({ id: "1", text: "first" });
    expect(rest).toEqual([{ id: "2", text: "second" }]);
  });

  it("does not mutate the input array", () => {
    const queue = [{ id: "1", text: "first" }];
    dequeue(queue);
    expect(queue).toEqual([{ id: "1", text: "first" }]);
  });
});

describe("removeFromQueue", () => {
  const queue = [
    { id: "1", text: "a" },
    { id: "2", text: "b" },
    { id: "3", text: "c" },
  ];

  it("removes the matching item and preserves order of the rest", () => {
    expect(removeFromQueue(queue, "2")).toEqual([
      { id: "1", text: "a" },
      { id: "3", text: "c" },
    ]);
  });

  it("is a no-op (new array, same contents) when the id isn't present", () => {
    const result = removeFromQueue(queue, "missing");
    expect(result).toEqual(queue);
    expect(result).not.toBe(queue);
  });

  it("handles an empty queue", () => {
    expect(removeFromQueue([], "1")).toEqual([]);
  });
});

describe("runDrain", () => {
  // Test double mirroring how App.svelte actually wires runDrain: a mutable store plus a
  // getQueue()/setQueue() pair, so tests can mutate the "live" queue mid-drain (from inside a
  // mock sendFn) the same way a real enqueue/remove/clear during an in-flight POST would.
  function makeStore(initial) {
    let value = initial;
    return {
      getQueue: () => value,
      setQueue: (next) => {
        value = next;
      },
    };
  }

  it("sends every item in FIFO order", async () => {
    const sent = [];
    const sendFn = vi.fn(async (text) => {
      sent.push(text);
    });
    const store = makeStore([
      { id: "1", text: "first" },
      { id: "2", text: "second" },
      { id: "3", text: "third" },
    ]);
    await runDrain(store.getQueue, sendFn, store.setQueue);
    expect(sent).toEqual(["first", "second", "third"]);
  });

  it("calls onQueueChange with the remaining items after each dequeue", async () => {
    const snapshots = [];
    const store = makeStore([
      { id: "1", text: "first" },
      { id: "2", text: "second" },
    ]);
    await runDrain(store.getQueue, async () => {}, (rest) => {
      store.setQueue(rest);
      snapshots.push(rest);
    });
    expect(snapshots).toEqual([[{ id: "2", text: "second" }], []]);
  });

  it("resolves immediately for an empty queue without calling sendFn", async () => {
    const sendFn = vi.fn();
    const store = makeStore([]);
    await runDrain(store.getQueue, sendFn, store.setQueue);
    expect(sendFn).not.toHaveBeenCalled();
  });

  it("completes the full drain even when sendFn rejects for one or more items (regression: a failing item must never wedge the queue)", async () => {
    const sent = [];
    const sendFn = vi.fn(async (text) => {
      sent.push(text);
      if (text === "second") throw new Error("boom");
    });
    const store = makeStore([
      { id: "1", text: "first" },
      { id: "2", text: "second" },
      { id: "3", text: "third" },
    ]);
    await expect(runDrain(store.getQueue, sendFn, store.setQueue)).resolves.toBeUndefined();
    expect(sent).toEqual(["first", "second", "third"]);
  });

  it("picks up an item enqueued while a previous item's sendFn call is still pending (regression: a stale local snapshot must never drop a mid-drain enqueue)", async () => {
    const sent = [];
    const store = makeStore([{ id: "1", text: "first" }]);
    const sendFn = vi.fn(async (text) => {
      sent.push(text);
      // Simulate a concurrent sendChatMessage() enqueueing a second item while this first
      // item's own "network call" is still in flight — the exact race a plan review caught.
      if (text === "first") {
        store.setQueue([...store.getQueue(), { id: "2", text: "second" }]);
      }
    });
    await runDrain(store.getQueue, sendFn, store.setQueue);
    expect(sent).toEqual(["first", "second"]);
  });

  it("respects a mid-drain removeFromQueue/clear on items not yet dequeued (regression: a stale local snapshot must never silently undo a cancel)", async () => {
    const sent = [];
    const store = makeStore([
      { id: "1", text: "first" },
      { id: "2", text: "second" },
    ]);
    const sendFn = vi.fn(async (text) => {
      sent.push(text);
      // Simulate the user cancelling item "2" (the only one still queued) while item "1" is
      // still being sent.
      if (text === "first") store.setQueue([]);
    });
    await runDrain(store.getQueue, sendFn, store.setQueue);
    expect(sent).toEqual(["first"]); // "second" was cancelled before its turn — never sent
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && pnpm test -- chatQueue`
Expected: FAIL — `Cannot find module './chatQueue.js'` (the module doesn't exist yet).

- [ ] **Step 3: Write the implementation — `frontend/src/lib/chatQueue.js`**

```js
// Pure FIFO queue helpers for "type while Nels is responding" (#230). Framework-free so the
// cap/ordering/drain-resilience logic is unit-testable without mounting App.svelte (this repo's
// vitest runs environment: "node" — no jsdom/testing-library, see every other *.test.js).
// App.svelte owns the reactive `pendingQueue` $state array; every function here is pure (or, for
// runDrain, pure aside from calling the injected functions it's given) — nothing here touches
// Svelte state directly.

export const QUEUE_CAP = 10;

// True if another item can be pushed onto `queue` without exceeding `cap`.
export function canEnqueue(queue, cap = QUEUE_CAP) {
  return Array.isArray(queue) && queue.length < cap;
}

// Returns a NEW array with { id, text } appended. Never mutates `queue`. Returns `queue`
// unchanged (same reference) when already at `cap` — callers use `canEnqueue` first to decide
// whether to attempt a send at all; this is a second, cheap guard against exceeding the cap.
export function enqueue(queue, id, text, cap = QUEUE_CAP) {
  const list = Array.isArray(queue) ? queue : [];
  if (list.length >= cap) return list;
  return [...list, { id, text }];
}

// FIFO pop: returns { item, rest }. `item` is null and `rest` is the (possibly empty) original
// list when `queue` is already empty. Never mutates `queue`.
export function dequeue(queue) {
  const list = Array.isArray(queue) ? queue : [];
  if (list.length === 0) return { item: null, rest: list };
  const [item, ...rest] = list;
  return { item, rest };
}

// Returns a NEW array with the item matching `id` removed. Order of the remaining items is
// preserved. A no-op (new array, same contents) if `id` isn't present.
export function removeFromQueue(queue, id) {
  const list = Array.isArray(queue) ? queue : [];
  return list.filter((q) => q.id !== id);
}

// Drains whatever `getQueue()` returns, strictly FIFO, awaiting `sendFn(text)` for each item in
// turn before dequeuing the next.
//
// IMPORTANT: `getQueue` is a LIVE accessor, called fresh on every iteration — this function never
// caches a local snapshot of the queue. That matters because `sendFn` is an `await` boundary: a
// caller can enqueue a new item (or cancel/clear an existing one) while a previous item's
// `sendFn` call is still pending, and the NEXT iteration must see that live change, not a stale
// copy captured before the await. (An earlier draft of this function took a plain `queue` array
// instead of a `getQueue` accessor and cached it locally — a plan review caught that this
// silently dropped items enqueued mid-drain and silently undid mid-drain cancels, since the
// stale local copy would win the next `onQueueChange` call. Always re-reading through `getQueue`
// closes both holes — see the two regression tests below.)
//
// `onQueueChange(rest)` is called immediately after each dequeue (before `sendFn` resolves) so a
// caller mirroring this into reactive state sees the "N queued" count drop the moment an item
// starts sending, not after it finishes. `onQueueChange` is also how the live queue this function
// reads back via `getQueue` actually gets updated — callers should have `getQueue`/`onQueueChange`
// point at the same backing store (e.g. `() => pendingQueue` / `(rest) => { pendingQueue = rest }`
// in `App.svelte`).
//
// A rejection from `sendFn` is swallowed and draining continues with the next item — this is the
// regression guard for a failure mode a spec review caught: `sendFn` failing must never leave
// later queued items stuck. (`App.svelte`'s `performSend` already turns every failure into its
// own chat error bubble internally and does not itself throw; this catch is defense-in-depth in
// case that ever changes.) Resolves once `getQueue()` reports empty.
export async function runDrain(getQueue, sendFn, onQueueChange) {
  while (true) {
    const current = getQueue();
    const list = Array.isArray(current) ? current : [];
    if (list.length === 0) break;
    const { item, rest } = dequeue(list);
    onQueueChange(rest);
    if (!item) break;
    try {
      await sendFn(item.text);
    } catch {
      // Swallowed deliberately — see function doc above. A failing item must not stop the drain.
    }
  }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- chatQueue`
Expected: PASS — all `canEnqueue`/`enqueue`/`dequeue`/`removeFromQueue`/`runDrain` cases green.

- [ ] **Step 5: Commit**

```bash
cd frontend
git add src/lib/chatQueue.js src/lib/chatQueue.test.js
git commit -m "feat(#230): add FIFO chat-queue pure module (chatQueue.js)"
```

---

## Task 2: `QueuedMessages.svelte` popover + i18n keys

**Files:**
- Create: `frontend/src/lib/QueuedMessages.svelte`
- Modify: `frontend/src/lib/i18n/locales/en.json`
- Modify: `frontend/src/lib/i18n/locales/de.json`
- Modify: `frontend/src/lib/i18n/locales/es.json`
- Modify: `frontend/src/lib/i18n/locales/fr.json`
- Modify: `frontend/src/lib/i18n/locales/it.json`
- Modify: `frontend/src/lib/i18n/locales/pt.json`

No component-test harness exists in this repo (vitest runs `environment: "node"`, no
jsdom/testing-library — see every existing `*.test.js`, all of which test pure `.js` modules only,
and `frontend/vitest.config.js`). This task is not TDD; verification is `pnpm run build` here plus
the manual walkthrough in Task 3 Step 7.

- [ ] **Step 1: Add 5 new keys to every locale file's `"chat"` object**

Each locale file has `"recallAria"` immediately followed by `"micStart"` inside the top-level
`"chat"` object (both at the same line numbers, 102-103, in every one of the 6 files — verify with
`grep -n '"recallAria"' frontend/src/lib/i18n/locales/*.json` before editing since line numbers may
have shifted; locate by the literal `"recallAria"` / `"micStart"` content, not the line number).

`frontend/src/lib/i18n/locales/en.json` — insert between them:
```json
    "recallAria": "Recall previous prompt",
    "queuedCount": "{count} queued",
    "queueFull": "Queue full (10) — wait for a reply",
    "queueClearAll": "Clear all",
    "queueRemoveAria": "Remove queued message",
    "queueToggleAria": "View queued messages",
    "micStart": "Speak",
```

`frontend/src/lib/i18n/locales/de.json`:
```json
    "recallAria": "Vorherige Eingabe abrufen",
    "queuedCount": "{count} in Warteschlange",
    "queueFull": "Warteschlange voll (10) – auf eine Antwort warten",
    "queueClearAll": "Alle entfernen",
    "queueRemoveAria": "Nachricht aus der Warteschlange entfernen",
    "queueToggleAria": "Warteschlange anzeigen",
    "micStart": "Sprechen",
```

`frontend/src/lib/i18n/locales/es.json`:
```json
    "recallAria": "Recuperar mensaje anterior",
    "queuedCount": "{count} en cola",
    "queueFull": "Cola llena (10): espera una respuesta",
    "queueClearAll": "Borrar todo",
    "queueRemoveAria": "Quitar mensaje de la cola",
    "queueToggleAria": "Ver mensajes en cola",
    "micStart": "Hablar",
```

`frontend/src/lib/i18n/locales/fr.json`:
```json
    "recallAria": "Rappeler le message précédent",
    "queuedCount": "{count} en file d'attente",
    "queueFull": "File d'attente pleine (10) : attendez une réponse",
    "queueClearAll": "Tout effacer",
    "queueRemoveAria": "Retirer le message de la file d'attente",
    "queueToggleAria": "Afficher les messages en file d'attente",
    "micStart": "Parler",
```

`frontend/src/lib/i18n/locales/it.json`:
```json
    "recallAria": "Richiama il messaggio precedente",
    "queuedCount": "{count} in coda",
    "queueFull": "Coda piena (10): attendi una risposta",
    "queueClearAll": "Cancella tutto",
    "queueRemoveAria": "Rimuovi messaggio dalla coda",
    "queueToggleAria": "Visualizza i messaggi in coda",
    "micStart": "Parla",
```

`frontend/src/lib/i18n/locales/pt.json`:
```json
    "recallAria": "Recuperar mensagem anterior",
    "queuedCount": "{count} na fila",
    "queueFull": "Fila cheia (10) — aguarde uma resposta",
    "queueClearAll": "Limpar tudo",
    "queueRemoveAria": "Remover mensagem da fila",
    "queueToggleAria": "Ver mensagens na fila",
    "micStart": "Falar",
```

Keep every file valid JSON (comma after `"recallAria"`'s value and after every inserted key except
the last, which is followed by `"micStart"` — so all 6 new/kept lines need trailing commas, nothing
special at the boundary since `micStart` already existed with its own trailing comma).

- [ ] **Step 2: Create `frontend/src/lib/QueuedMessages.svelte`**

```svelte
<script>
  // "N queued" indicator + preview/cancel popover for messages queued while Nels is generating
  // (#230). Mirrors BudgetStatusInfo.svelte's click-toggle / outside-click / Escape-close pattern
  // — the same repo idiom, not a new one. Renders nothing when the queue is empty. Purely
  // presentational: all queue mutation goes back through the two callback props so App.svelte
  // stays the single owner of `pendingQueue` state.
  import { X } from "lucide-svelte";
  import { _ } from "svelte-i18n";

  let { queue = [], onRemove = () => {}, onClearAll = () => {} } = $props();

  let open = $state(false);
  let panelEl = $state(null);
  let buttonEl = $state(null);

  function toggle() {
    open = !open;
  }

  function close() {
    open = false;
  }

  function onWindowClick(e) {
    if (!open) return;
    if (panelEl?.contains(e.target) || buttonEl?.contains(e.target)) return;
    close();
  }

  function onWindowKeydown(e) {
    if (e.key === "Escape" && open) close();
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKeydown} />

{#if queue.length > 0}
  <div class="relative">
    <button
      bind:this={buttonEl}
      type="button"
      onclick={toggle}
      aria-haspopup="true"
      aria-expanded={open}
      aria-label={$_("chat.queueToggleAria")}
      class="badge badge-sm badge-neutral gap-1 cursor-pointer"
    >
      {$_("chat.queuedCount", { values: { count: queue.length } })}
    </button>

    {#if open}
      <div
        bind:this={panelEl}
        role="dialog"
        aria-label={$_("chat.queueToggleAria")}
        class="absolute bottom-full right-0 mb-2 z-50 w-72 max-w-[calc(100vw-1.5rem)]
               rounded-xl bg-base-200 border border-base-300 shadow-2xl p-3"
      >
        <ul class="flex flex-col gap-1.5 max-h-56 overflow-y-auto">
          {#each queue as item (item.id)}
            <li class="flex items-start gap-2">
              <span class="flex-1 text-xs text-base-content/80 line-clamp-2">{item.text}</span>
              <button
                type="button"
                onclick={() => onRemove(item.id)}
                aria-label={$_("chat.queueRemoveAria")}
                class="shrink-0 text-base-content/50 hover:text-base-content"
              >
                <X class="w-3.5 h-3.5" />
              </button>
            </li>
          {/each}
        </ul>
        <button
          type="button"
          onclick={onClearAll}
          class="btn btn-ghost btn-xs w-full mt-2 text-base-content/70"
        >
          {$_("chat.queueClearAll")}
        </button>
      </div>
    {/if}
  </div>
{/if}
```

- [ ] **Step 3: Verify the frontend still builds**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (the component isn't imported anywhere yet, so this just confirms the
new file and every locale JSON edit is syntactically valid — a JSON syntax error in any locale
file fails the build with a parse error naming the file).

- [ ] **Step 4: Commit**

```bash
cd frontend
git add src/lib/QueuedMessages.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json \
  src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json \
  src/lib/i18n/locales/pt.json
git commit -m "feat(#230): add QueuedMessages popover component and i18n keys"
```

---

## Task 3: Wire `App.svelte`

**Files:**
- Modify: `frontend/src/App.svelte`

**Locate every block below by its literal code content, NOT by the stated line number.** Line
numbers are illustrative/approximate only (confirmed against this branch's `App.svelte` at
plan-writing time; earlier edits in this same task shift later line numbers). Use an exact-string
find (an editor's Edit-by-old-string, or `grep -n` on a distinctive substring) to re-locate each
block immediately before editing it.

- [ ] **Step 1: Add the new imports**

Find (top `<script>` block, ~line 24-25):
```js
  import BudgetStatusInfo from "./lib/BudgetStatusInfo.svelte";
  import nelsMark from "./assets/nels-mark.png";
```

Replace with:
```js
  import BudgetStatusInfo from "./lib/BudgetStatusInfo.svelte";
  import QueuedMessages from "./lib/QueuedMessages.svelte";
  import nelsMark from "./assets/nels-mark.png";
```

Find (~line 41):
```js
  import { windowMessages } from "./lib/chatWindow.js";
```

Replace with:
```js
  import { windowMessages } from "./lib/chatWindow.js";
  import { canEnqueue, enqueue, removeFromQueue, runDrain } from "./lib/chatQueue.js";
```

- [ ] **Step 2: Add `pendingQueue`/`isDraining` state**

Find (~line 272-275):
```js
  // UI state indicators
  let isLoading = $state(false);
  let isChatLoading = $state(false);
  let errorAlert = $state("");
```

Replace with:
```js
  // UI state indicators
  let isLoading = $state(false);
  let isChatLoading = $state(false);
  // FIFO queue for messages/commands submitted while a turn is in flight (#230). Draining is
  // guarded by isDraining so only one drainQueue() runs at a time; see performSend/drainQueue
  // below sendChatMessage.
  let pendingQueue = $state([]);
  let isDraining = $state(false);
  let errorAlert = $state("");
```

- [ ] **Step 3: Relax the arrow-key recall guards in `handleChatInputKeydown`**

Find (~line 233-251):
```js
    const onRecalledPrompt =
      historyCursor !== -1 && chatInput === promptHistory[historyCursor];
    if (
      !isChatLoading &&
      e.key === "ArrowUp" &&
      (chatInput === "" || onRecalledPrompt)
    ) {
      e.preventDefault();
      // An empty composer always starts recall from the most recent prompt,
      // even if a stale cursor lingers from an earlier, since-edited recall.
      if (chatInput === "") historyCursor = -1;
      recallPrev();
      return;
    }
    if (!isChatLoading && e.key === "ArrowDown" && onRecalledPrompt) {
      e.preventDefault();
      recallNext();
      return;
    }
```

Replace with:
```js
    const onRecalledPrompt =
      historyCursor !== -1 && chatInput === promptHistory[historyCursor];
    // Arrow-key recall stays usable while a turn is in flight (#230) — the composer is never
    // disabled anymore, so there's no reason to suppress it during isChatLoading/draining.
    if (e.key === "ArrowUp" && (chatInput === "" || onRecalledPrompt)) {
      e.preventDefault();
      // An empty composer always starts recall from the most recent prompt,
      // even if a stale cursor lingers from an earlier, since-edited recall.
      if (chatInput === "") historyCursor = -1;
      recallPrev();
      return;
    }
    if (e.key === "ArrowDown" && onRecalledPrompt) {
      e.preventDefault();
      recallNext();
      return;
    }
```

- [ ] **Step 4: Replace `sendChatMessage` with `performSend` / `drainQueue` / `sendChatMessage`**

Find the entire current `sendChatMessage` function, from its `// --- AI Copilot Handlers ---`
comment through its closing brace (~line 1003-1145 — do NOT include the blank line or
`function handlePromptChip` that follows):

```js
  // --- AI Copilot Handlers ---
  async function sendChatMessage(e) {
    if (e) e.preventDefault();
    if (!chatInput.trim() || isChatLoading) return;

    // Sending is a deliberate action: re-pin so the user's own message, the
    // loading indicator, and the reply scroll into view even if they had
    // scrolled up to read history (AC 2). The scroll-up guard still applies to
    // passively-arriving replies, not to content the user just submitted. #108
    pinnedToBottom = true;
    showAllMessages = false;

    // Intercept slash-commands before they reach the AI endpoint.
    if (isCommand(chatInput)) {
      const rawInput = chatInput.trim();
      recordPrompt(rawInput);
      chatInput = "";
      await runCommand(rawInput);
      // Some commands (e.g. /issues-list) toggle isChatLoading and so disable
      // the textarea mid-flight; restore focus once the DOM settles, matching
      // the normal send path (issue #104).
      await tick();
      chatInputEl?.focus();
      return;
    }

    const userMessage = chatInput.trim();
    recordPrompt(userMessage);
    chatInput = "";

    // Optimistically push user message
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "user",
        message_text: userMessage,
        created_at: new Date().toISOString(),
      },
    ];

    isChatLoading = true;

    try {
      const chatRes = await fetchApi("/chat", {
        method: "POST",
        body: JSON.stringify({
          message: userMessage,
          budget_id: activeBudget ? activeBudget.id : null,
          conversation_id: activeConversationId,
          locale: currentLocale(),
        }),
      });

      // Track the thread the backend used (created lazily on first send).
      activeConversationId = chatRes.conversation_id;

      // Push AI reply (keep optimistic messages; never reload from history)
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: chatRes.response,
          created_at: new Date().toISOString(),
        },
      ];

      // Keep the header / active-budget display in sync after a chat-driven
      // change. The header binds the active budget's name + status badges
      // (project/closed/archived/rollover), all of which are budget-row fields
      // that an in-place edit (rename, limit/rollover, close, archive) changes
      // WITHOUT changing the budget id — so the previous id-only gate left the
      // header stale until a manual reload (issue #87). Refresh the budget LIST
      // (never chat history — that would clobber the thread) when EITHER:
      //   - the active budget switched / was created (id differs / no active), OR
      //   - the chat reported any mutation (`action_taken`). The chat response
      //     exposes only a human-readable mutation log, not the action type, so
      //     we cannot cheaply tell a header-relevant edit from an unrelated one
      //     (e.g. logging an expense). We deliberately re-fetch on any mutation:
      //     `/budgets` is a small list query already issued on switch/delete, and
      //     guaranteed header correctness is worth one extra GET over coupling
      //     this gate to the backend's log wording. Pure Q&A turns leave
      //     `action_taken` null and skip the fetch.
      const budgetSwitched =
        chatRes.budget_id &&
        (!activeBudget || activeBudget.id !== chatRes.budget_id);
      if (budgetSwitched || chatRes.action_taken) {
        await fetchBudgets();
        // A mutation (e.g. logging an expense) can trip a budget/category limit
        // alert server-side — re-poll the feed so the badge updates promptly
        // instead of waiting for the 60s poll.
        notificationsRef?.refresh();
      }

      // The assistant never deletes directly; it proposes a deletion and we
      // confirm it through a custom modal before calling the REST endpoint.
      if (chatRes.pending_deletion) {
        pendingDeletion = chatRes.pending_deletion;
      }

      // Navigate to the insights outlet when the assistant signals it (#157,
      // #233). Mirrors the sidebar path, which calls the same navigate().
      // Advisory only — no data changes; this just surfaces the existing
      // insights view.
      if (chatRes.open_insights) {
        navigate("insights");
      }

      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). The HTML itself is discarded here —
      // CategoriesView re-fetches its own fresh copy on mount, the same
      // convention Insights already uses (self-fetch via fetchApi, no data
      // via props).
      if (chatRes.categories_table_html) {
        navigate("categories");
      }

      // Surface the new thread / generated title in the sidebar.
      await fetchConversations();
    } catch (e) {
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: `⚠️ I was unable to connect to my AI node. Error: ${e.message}. If offline, try asking standard budgeting queries!`,
          created_at: new Date().toISOString(),
        },
      ];
    } finally {
      isChatLoading = false;
      // Disabling the textarea while loading dropped its focus (a disabled
      // element can't hold focus); re-enabling doesn't restore it. Await the DOM
      // update so the field is interactive again, then refocus so the user can
      // type the next prompt without clicking back in (issue #104). Skip when a
      // focus-owning modal is open — the deletion-confirm dialog or the insights
      // dialog — since each moves focus to its own controls, and pulling it back
      // to the textarea behind the modal would be wrong for keyboard users.
      await tick();
      if (!pendingDeletion && route === "chat") chatInputEl?.focus();
    }
  }
```

Replace with:
```js
  // --- AI Copilot Handlers ---
  // Runs ONE already-dequeued item (or the very first, non-busy send) through the actual send
  // path: slash-command interception, or the optimistic-bubble + POST /chat round trip. Never
  // reads or clears `chatInput` — callers (sendChatMessage / drainQueue) own that. Never throws —
  // both branches below catch their own failures — so drainQueue's while loop can never be
  // wedged by an item that fails (#230).
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
      recordPrompt(rawInput);
      try {
        await runCommand(rawInput);
      } catch (err) {
        // Defensive: every existing runCommand branch already catches its own network calls, so
        // this should never actually trigger — but performSend must never be a throw point
        // either way, or a queued command failure would wedge the rest of the drain.
        pushAiMessage(commandErrorMessage(err));
      }
      return;
    }

    recordPrompt(rawInput);

    // Optimistically push user message
    chatMessages = [
      ...chatMessages,
      {
        id: Math.random().toString(),
        sender: "user",
        message_text: rawInput,
        created_at: new Date().toISOString(),
      },
    ];

    isChatLoading = true;

    try {
      const chatRes = await fetchApi("/chat", {
        method: "POST",
        body: JSON.stringify({
          message: rawInput,
          budget_id: activeBudget ? activeBudget.id : null,
          conversation_id: activeConversationId,
          locale: currentLocale(),
        }),
      });

      // Track the thread the backend used (created lazily on first send).
      activeConversationId = chatRes.conversation_id;

      // Push AI reply (keep optimistic messages; never reload from history)
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: chatRes.response,
          created_at: new Date().toISOString(),
        },
      ];

      // Keep the header / active-budget display in sync after a chat-driven
      // change. The header binds the active budget's name + status badges
      // (project/closed/archived/rollover), all of which are budget-row fields
      // that an in-place edit (rename, limit/rollover, close, archive) changes
      // WITHOUT changing the budget id — so the previous id-only gate left the
      // header stale until a manual reload (issue #87). Refresh the budget LIST
      // (never chat history — that would clobber the thread) when EITHER:
      //   - the active budget switched / was created (id differs / no active), OR
      //   - the chat reported any mutation (`action_taken`). The chat response
      //     exposes only a human-readable mutation log, not the action type, so
      //     we cannot cheaply tell a header-relevant edit from an unrelated one
      //     (e.g. logging an expense). We deliberately re-fetch on any mutation:
      //     `/budgets` is a small list query already issued on switch/delete, and
      //     guaranteed header correctness is worth one extra GET over coupling
      //     this gate to the backend's log wording. Pure Q&A turns leave
      //     `action_taken` null and skip the fetch.
      const budgetSwitched =
        chatRes.budget_id &&
        (!activeBudget || activeBudget.id !== chatRes.budget_id);
      if (budgetSwitched || chatRes.action_taken) {
        await fetchBudgets();
        // A mutation (e.g. logging an expense) can trip a budget/category limit
        // alert server-side — re-poll the feed so the badge updates promptly
        // instead of waiting for the 60s poll.
        notificationsRef?.refresh();
      }

      // The assistant never deletes directly; it proposes a deletion and we
      // confirm it through a custom modal before calling the REST endpoint.
      if (chatRes.pending_deletion) {
        pendingDeletion = chatRes.pending_deletion;
      }

      // Navigate to the insights outlet when the assistant signals it (#157,
      // #233). Mirrors the sidebar path, which calls the same navigate().
      // Advisory only — no data changes; this just surfaces the existing
      // insights view.
      if (chatRes.open_insights) {
        navigate("insights");
      }

      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). The HTML itself is discarded here —
      // CategoriesView re-fetches its own fresh copy on mount, the same
      // convention Insights already uses (self-fetch via fetchApi, no data
      // via props).
      if (chatRes.categories_table_html) {
        navigate("categories");
      }

      // Surface the new thread / generated title in the sidebar.
      await fetchConversations();
    } catch (e) {
      chatMessages = [
        ...chatMessages,
        {
          id: Math.random().toString(),
          sender: "ai",
          message_text: `⚠️ I was unable to connect to my AI node. Error: ${e.message}. If offline, try asking standard budgeting queries!`,
          created_at: new Date().toISOString(),
        },
      ];
    } finally {
      isChatLoading = false;
    }
  }

  // Drains pendingQueue strictly FIFO, one item at a time, via the shared chatQueue.js runDrain
  // helper. Guarded by isDraining so a fire-and-forget call from sendChatMessage while a drain is
  // already running is a safe no-op — the already-running while loop picks up a newly-queued item
  // (or a mid-drain cancel/clear) on its next iteration because runDrain re-reads the LIVE
  // `pendingQueue` via the `() => pendingQueue` accessor every iteration, never a cached snapshot
  // (see chatQueue.js's runDrain doc comment — this is the exact bug a plan review caught in an
  // earlier draft that passed a plain array instead). try/finally is defense-in-depth alongside
  // runDrain's own internal catch: isDraining must reset even if performSend somehow throws (#230
  // spec review).
  async function drainQueue() {
    if (isDraining) return;
    isDraining = true;
    try {
      await runDrain(
        () => pendingQueue,
        performSend,
        (rest) => {
          pendingQueue = rest;
        },
      );
    } finally {
      isDraining = false;
    }
  }

  // Entry point for both the form submit and Enter-to-send. Every call — whether Nels is idle or
  // mid-turn — enqueues the trimmed input and (re-)kicks off drainQueue(); when idle, the queue
  // was empty so drainQueue's first loop iteration dequeues and sends this item immediately
  // (synchronously, before any paint), which is why a direct send never flashes a "1 queued"
  // indicator. This unifies the "not busy" and "busy" cases into one mechanism instead of two
  // (#230).
  async function sendChatMessage(e) {
    if (e) e.preventDefault();
    const raw = chatInput.trim();
    if (!raw) return;
    if (!canEnqueue(pendingQueue)) return; // queue cap hit; leave the composer text untouched
    chatInput = "";
    pendingQueue = enqueue(pendingQueue, Math.random().toString(), raw);
    drainQueue();
  }

  function removeQueuedMessage(id) {
    pendingQueue = removeFromQueue(pendingQueue, id);
  }

  function clearQueuedMessages() {
    pendingQueue = [];
  }
```

Note `pushAiMessage` and `commandErrorMessage` are both existing functions already used elsewhere
in this file for exactly this purpose (`commandErrorMessage` is defined immediately above the old
`sendChatMessage`, at the top of the block you're replacing's preceding lines; `pushAiMessage` is
defined later in the file as a hoisted `function` declaration, so calling it here before its
textual definition is safe in JS). Do not duplicate either — reuse them as shown.

- [ ] **Step 5: Mount `QueuedMessages` above the composer form**

Find (~line 2133-2134, the `{/if}` closing the slash-command palette block, immediately before the
form):
```svelte
          {/if}
          <form onsubmit={sendChatMessage} class="flex gap-2 items-end">
```

Replace with:
```svelte
          {/if}
          <div class="flex justify-end mb-1">
            <QueuedMessages
              queue={pendingQueue}
              onRemove={removeQueuedMessage}
              onClearAll={clearQueuedMessages}
            />
          </div>
          <form onsubmit={sendChatMessage} class="flex gap-2 items-end">
```

- [ ] **Step 6: Relax the textarea/mic/recall disables and the send-button gate**

Find:
```svelte
              onkeydown={handleChatInputKeydown}
              disabled={isChatLoading}
            ></textarea>
```

Replace with (textarea is never disabled now — it stays interactive during generation, #230):
```svelte
              onkeydown={handleChatInputKeydown}
            ></textarea>
```

Find:
```svelte
                      onclick={toggleDictation}
                      disabled={isChatLoading}
                      aria-pressed={dictation.listening}
```

Replace with:
```svelte
                      onclick={toggleDictation}
                      aria-pressed={dictation.listening}
```

Find:
```svelte
                    onclick={recallFromButton}
                    disabled={isChatLoading || promptHistory.length === 0}
                    aria-label={$_("chat.recallAria")}
```

Replace with:
```svelte
                    onclick={recallFromButton}
                    disabled={promptHistory.length === 0}
                    aria-label={$_("chat.recallAria")}
```

Find:
```svelte
              <button
                type="submit"
                class="btn h-auto min-h-[5.25rem] w-14 p-0 self-stretch btn-primary border-none text-primary-content"
                disabled={!chatInput.trim() || isChatLoading}
                aria-label={$_("chat.sendAria")}
              >
                <Send class="w-5 h-5" />
              </button>
            </div>
          </form>
          {#if dictation.error}
            <p class="text-error text-xs mt-2" role="alert">
              {dictation.error === "denied"
                ? $_("chat.micDenied")
                : $_("chat.micError")}
            </p>
          {/if}
```

Replace with (send is gated on trimmed input + the queue cap only, never on `isChatLoading`; a
cap-hit hint appears under the input):
```svelte
              <button
                type="submit"
                class="btn h-auto min-h-[5.25rem] w-14 p-0 self-stretch btn-primary border-none text-primary-content"
                disabled={!chatInput.trim() || !canEnqueue(pendingQueue)}
                aria-label={$_("chat.sendAria")}
              >
                <Send class="w-5 h-5" />
              </button>
            </div>
          </form>
          {#if !canEnqueue(pendingQueue)}
            <p class="text-warning text-xs mt-2" role="status">
              {$_("chat.queueFull")}
            </p>
          {/if}
          {#if dictation.error}
            <p class="text-error text-xs mt-2" role="alert">
              {dictation.error === "denied"
                ? $_("chat.micDenied")
                : $_("chat.micError")}
            </p>
          {/if}
```

- [ ] **Step 7: Manual verification**

Run: `cd frontend && pnpm run dev`

Log in / use an existing session with at least one budget. Confirm:

1. While Nels is generating a reply (ask something that takes a couple seconds, or use a slow
   network throttle in devtools), the textarea is NOT disabled, keeps focus, and you can keep
   typing.
2. Press Send (and separately, on desktop, press Enter) while a reply is in flight: the message is
   queued (not sent immediately — no new user bubble appears yet), the textarea clears, and a
   small "N queued" pill appears near the input.
3. Queue 2-3 items, including at least one slash command (e.g. `/tokens`). When the in-flight
   reply returns, confirm the queued items auto-send one at a time, IN ORDER, each producing its
   user bubble immediately before its own reply (not all bubbles up front).
4. Click the "N queued" pill: a popover lists each queued item's text with a per-item remove (×);
   removing one drops it from the indicator and it never sends. A "Clear all" action empties the
   whole queue the same way.
5. Force a failure on one queued item (e.g. briefly stop the backend, queue a message, restart the
   backend before the earlier items finish) and confirm it gets its own error bubble and the
   remaining queued items still send afterward.
6. Confirm mic/dictation and the history-recall button (and ArrowUp/ArrowDown recall) are usable
   while `isChatLoading`/draining is true (not disabled).
7. Queue 10 items (the cap): confirm Send becomes disabled and the "Queue full" hint text appears;
   confirm it clears once the count drops back under 10.
8. Open the Network tab, queue 3 plain messages, and confirm all three `POST /chat` calls use the
   SAME `conversation_id` (i.e., no duplicate conversation is created) and that no two `POST /chat`
   calls are ever in flight concurrently (each one completes before the next starts).

- [ ] **Step 8: Run the full test suite and build**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing tests plus the new `chatQueue.test.js` suite green.

Run: `cd frontend && pnpm run build`
Expected: build succeeds with no errors.

- [ ] **Step 9: Commit**

```bash
cd frontend
git add src/App.svelte
git commit -m "feat(#230): allow typing during AI response via FIFO queue + auto-send"
```

---

## Task 4: Final pass

- [ ] **Step 1: Re-read the diff against the AC**

Run: `git diff origin/main...HEAD --stat` and `git diff origin/main...HEAD` from the worktree root.

Confirm against savvagent/nels#230's acceptance criteria:
- [ ] Textarea usable and focused during generation (Task 3 Steps 3, 6; verified Step 7.1).
- [ ] Send/Enter during generation queues, clears input, shows "N queued" (Task 3 Steps 4-6;
      verified Step 7.2).
- [ ] Queued items auto-send FIFO, one at a time, on resolution (Task 1 `runDrain`, Task 3 Step 4;
      verified Step 7.3).
- [ ] Transcript order is correct (user bubble appended at dequeue time inside `performSend`, never
      at enqueue time — Task 3 Step 4; verified Step 7.3).
- [ ] Slash commands queue and dequeue through the same `isCommand`/`runCommand` interception, no
      special-casing (Task 3 Step 4's `performSend`; verified Step 7.3).
- [ ] A failed item gets its own error bubble and doesn't block the rest (Task 1 `runDrain` +
      `performSend`'s own `try/catch`; verified Step 7.5).
- [ ] Mic/dictation, history-recall button, arrow-key recall usable during generation (Task 3
      Steps 3, 6; verified Step 7.6).
- [ ] Individual cancel + clear-all before send (Task 2's `QueuedMessages.svelte`, Task 3 Step 4's
      `removeQueuedMessage`/`clearQueuedMessages`; verified Step 7.4).
- [ ] No duplicate `conversation_id`, no out-of-order replies (sequential `runDrain` + shared
      `activeConversationId`; verified Step 7.8).

- [ ] **Step 2: Confirm no orphaned code or unused imports**

Run: `grep -n "isChatLoading" frontend/src/App.svelte` — every remaining hit should be either the
`$state` declaration, the `{#if isChatLoading}` "analyzing" bubble, or inside `performSend`'s
try/finally around the `POST /chat` call. There should be NO remaining `disabled={isChatLoading}`
(bare or `||`-combined) anywhere in the file.

Run: `grep -n "tick()" frontend/src/App.svelte` — confirm the only remaining call sites are
unrelated to `sendChatMessage` (e.g. `moveCaretToEnd`); the two `sendChatMessage`-specific
tick-then-refocus call sites removed in Task 3 Step 4 should be gone.

- [ ] **Step 3: Final full check**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: both succeed.
