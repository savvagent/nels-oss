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
    } catch (err) {
      // Swallowed deliberately — see function doc above. A failing item must not stop the drain.
      // Still logged so a genuine regression (sendFn starting to throw) leaves a debug trail
      // instead of vanishing silently.
      console.error("chatQueue.runDrain: sendFn rejected unexpectedly", err);
    }
  }
}
