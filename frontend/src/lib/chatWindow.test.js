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
    expect(visible[visible.length - 1]).toBe(msgs[msgs.length - 1]);
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
