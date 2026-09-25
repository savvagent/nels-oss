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
