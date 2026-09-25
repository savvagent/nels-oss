import { describe, it, expect } from "vitest";
import {
  groupConversations,
  relativeTime,
  titleFor,
  shouldCommitRename,
  findFirstUserMessage,
} from "./history.js";

describe("groupConversations", () => {
  const now = new Date("2026-07-03T12:00:00.000Z");

  it("buckets conversations into Today/Yesterday/Earlier", () => {
    const conversations = [
      { id: "today", updated_at: "2026-07-03T09:00:00.000Z" },
      { id: "yesterday", updated_at: "2026-07-02T09:00:00.000Z" },
      { id: "earlier", updated_at: "2026-06-20T09:00:00.000Z" },
    ];
    expect(groupConversations(conversations, now)).toEqual([
      { label: "Today", items: [conversations[0]] },
      { label: "Yesterday", items: [conversations[1]] },
      { label: "Earlier", items: [conversations[2]] },
    ]);
  });

  it("drops empty buckets", () => {
    const conversations = [{ id: "today", updated_at: "2026-07-03T09:00:00.000Z" }];
    expect(groupConversations(conversations, now)).toEqual([
      { label: "Today", items: conversations },
    ]);
  });

  it("returns an empty array for no conversations", () => {
    expect(groupConversations([], now)).toEqual([]);
    expect(groupConversations(undefined, now)).toEqual([]);
  });

  it("preserves original relative order within a bucket", () => {
    const conversations = [
      { id: "a", updated_at: "2026-07-03T09:00:00.000Z" },
      { id: "b", updated_at: "2026-07-03T10:00:00.000Z" },
    ];
    expect(groupConversations(conversations, now)[0].items.map((c) => c.id)).toEqual([
      "a",
      "b",
    ]);
  });
});

describe("relativeTime", () => {
  const now = new Date("2026-07-03T12:00:00.000Z");

  it("returns 'now' for under a minute", () => {
    expect(relativeTime("2026-07-03T11:59:30.000Z", now)).toBe("now");
  });

  it("returns minutes for under an hour", () => {
    expect(relativeTime("2026-07-03T11:45:00.000Z", now)).toBe("15m");
  });

  it("returns hours for under a day", () => {
    expect(relativeTime("2026-07-03T09:00:00.000Z", now)).toBe("3h");
  });

  it("returns days for under a week", () => {
    expect(relativeTime("2026-07-01T12:00:00.000Z", now)).toBe("2d");
  });

  it("returns weeks for under five weeks", () => {
    expect(relativeTime("2026-06-19T12:00:00.000Z", now)).toBe("2w");
  });

  it("falls back to a localized date at five weeks or more", () => {
    const result = relativeTime("2026-05-01T12:00:00.000Z", now, "en");
    expect(result).not.toMatch(/[wdhm]$/);
    expect(result).not.toBe("now");
  });

  // Threshold boundaries: each `< N` comparison in relativeTime's
  // implementation is exactly where an off-by-one (e.g. an accidental `<=`)
  // would silently regress on a future edit. These pin the exact instant on
  // each side of the minute/hour/day/week cutoffs.
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  const week = 7 * day;
  const isoBefore = (ms) => new Date(now.getTime() - ms).toISOString();

  it("stays in minutes just under the hour boundary, switches to hours at it", () => {
    expect(relativeTime(isoBefore(hour - 1000), now)).toBe("59m");
    expect(relativeTime(isoBefore(hour), now)).toBe("1h");
  });

  it("stays in hours just under the day boundary, switches to days at it", () => {
    expect(relativeTime(isoBefore(day - minute), now)).toBe("23h");
    expect(relativeTime(isoBefore(day), now)).toBe("1d");
  });

  it("stays in days just under the week boundary, switches to weeks at it", () => {
    expect(relativeTime(isoBefore(week - hour), now)).toBe("6d");
    expect(relativeTime(isoBefore(week), now)).toBe("1w");
  });

  it("stays in weeks just under the five-week boundary, falls back to a date at it", () => {
    expect(relativeTime(isoBefore(4 * week + 6 * day), now)).toBe("4w");
    const atFiveWeeks = relativeTime(isoBefore(5 * week), now, "en");
    expect(atFiveWeeks).not.toMatch(/[wdhm]$/);
    expect(atFiveWeeks).not.toBe("now");
  });

  it("treats a future-dated (clock-skew) timestamp as 'now' rather than throwing or going negative", () => {
    expect(relativeTime(isoBefore(-minute), now)).toBe("now");
  });
});

describe("titleFor", () => {
  it("returns the trimmed title when present", () => {
    expect(titleFor({ title: "  Grocery budget  " })).toBe("  Grocery budget  ");
  });

  it("returns null for a blank/whitespace-only title", () => {
    expect(titleFor({ title: "   " })).toBe(null);
  });

  it("returns null for a missing title", () => {
    expect(titleFor({})).toBe(null);
    expect(titleFor(null)).toBe(null);
  });
});

describe("shouldCommitRename", () => {
  it("returns true for a non-empty, changed value", () => {
    expect(shouldCommitRename("New name", "Old name")).toBe(true);
  });

  it("returns false for an empty/whitespace-only value", () => {
    expect(shouldCommitRename("   ", "Old name")).toBe(false);
    expect(shouldCommitRename("", "Old name")).toBe(false);
  });

  it("returns false when unchanged from the current title", () => {
    expect(shouldCommitRename("Old name", "Old name")).toBe(false);
  });

  it("returns false when trimmed value equals the current title", () => {
    expect(shouldCommitRename("  Old name  ", "Old name")).toBe(false);
  });
});

describe("findFirstUserMessage", () => {
  it("finds the first message with sender 'user'", () => {
    const messages = [
      { id: "m1", sender: "ai", message_text: "Hi, how can I help?" },
      { id: "m2", sender: "user", message_text: "How much did I spend on groceries?" },
      { id: "m3", sender: "ai", message_text: "You spent $120." },
    ];
    expect(findFirstUserMessage(messages)).toEqual(messages[1]);
  });

  it("returns null when there is no user message", () => {
    expect(findFirstUserMessage([{ id: "m1", sender: "ai", message_text: "Hi" }])).toBe(
      null,
    );
  });

  it("returns null for an empty or missing list", () => {
    expect(findFirstUserMessage([])).toBe(null);
    expect(findFirstUserMessage(undefined)).toBe(null);
  });
});
