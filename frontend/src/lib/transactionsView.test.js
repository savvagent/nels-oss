import { describe, it, expect } from "vitest";
import {
  assignableCategories,
  formatAmount,
  amountTone,
  provenanceKind,
  needsReview,
  filterNeedsReview,
  markReviewed,
  possibleDuplicate,
  resolveMatch,
  groupTransactions,
  searchTransactions,
  isPending,
  filterPending,
  isUncategorized,
  filterUncategorized,
  applyTransactionFilters,
  findTwin,
  filterCounts,
  splitAccountLabel,
} from "./transactionsView.js";

describe("assignableCategories", () => {
  const cats = [
    { id: "1", name: "Food", category_type: "expense", linked_budget_id: null },
    { id: "2", name: "Salary", category_type: "income", linked_budget_id: null },
    { id: "3", name: "Mirror", category_type: "expense", linked_budget_id: "b2" },
    { id: "4", name: "Savings", category_type: "savings", linked_budget_id: null },
    { id: "5", name: "Rent", category_type: "expense" },
  ];

  it("keeps expense non-mirror categories only", () => {
    expect(assignableCategories(cats).map((c) => c.id)).toEqual(["1", "5"]);
  });

  it("returns [] for null/undefined", () => {
    expect(assignableCategories(null)).toEqual([]);
    expect(assignableCategories(undefined)).toEqual([]);
  });
});

describe("formatAmount", () => {
  it("uses the row's currency when present", () => {
    // Non-USD code must actually change the output vs USD.
    const eur = formatAmount(12.5, "EUR");
    const usd = formatAmount(12.5, "USD");
    expect(eur).not.toEqual(usd);
    expect(eur).toContain("12.50");
  });

  it("falls back to USD when currency is null/blank (Nels-logged rows)", () => {
    expect(formatAmount(12.5, null)).toEqual(formatAmount(12.5, "USD"));
    expect(formatAmount(12.5, "")).toEqual(formatAmount(12.5, "USD"));
    expect(formatAmount(12.5, undefined)).toContain("12.50");
  });

  it("falls back to USD (never throws) on an invalid ISO code", () => {
    expect(formatAmount(5, "NOTACODE")).toEqual(formatAmount(5, "USD"));
  });

  it("treats null/undefined amount as 0", () => {
    expect(formatAmount(null, "USD")).toEqual(formatAmount(0, "USD"));
  });
});

describe("amountTone", () => {
  it("keys off category type, not amount sign", () => {
    expect(amountTone("income")).toBe("income");
    expect(amountTone("expense")).toBe("expense");
    expect(amountTone("savings")).toBe("neutral");
    expect(amountTone(null)).toBe("neutral"); // uncategorized import
    expect(amountTone(undefined)).toBe("neutral");
  });
});

describe("provenanceKind", () => {
  it("maps the API source field to a badge kind", () => {
    expect(provenanceKind({ source: "ai" })).toBe("ai");
    expect(provenanceKind({ source: "imported" })).toBe("imported");
    expect(provenanceKind({ source: "manual" })).toBe(null);
    expect(provenanceKind({})).toBe(null);
    expect(provenanceKind(null)).toBe(null);
  });
});

describe("needsReview", () => {
  it("is true only for review_status === 'needs_review'", () => {
    expect(needsReview({ review_status: "needs_review" })).toBe(true);
    expect(needsReview({ review_status: "reviewed" })).toBe(false);
    // A row with no review_status (older API / Nels row) is not flagged.
    expect(needsReview({})).toBe(false);
    expect(needsReview(null)).toBe(false);
    expect(needsReview(undefined)).toBe(false);
  });
});

describe("filterNeedsReview", () => {
  const txs = [
    { id: "a", review_status: "needs_review" },
    { id: "b", review_status: "reviewed" },
    { id: "c", review_status: "needs_review" },
    { id: "d" },
  ];

  it("narrows to needs-review rows when the chip is active", () => {
    expect(filterNeedsReview(txs, true).map((t) => t.id)).toEqual(["a", "c"]);
  });

  it("is identity (all rows) when the chip is inactive", () => {
    expect(filterNeedsReview(txs, false)).toBe(txs);
  });

  it("is safe on null/undefined input", () => {
    expect(filterNeedsReview(null, true)).toEqual([]);
    expect(filterNeedsReview(undefined, false)).toEqual([]);
  });
});

describe("markReviewed", () => {
  it("immutably flips the target row's review_status to reviewed", () => {
    const txs = [
      { id: "a", review_status: "needs_review", amount: 1 },
      { id: "b", review_status: "needs_review", amount: 2 },
    ];
    const out = markReviewed(txs, "a");
    expect(out).not.toBe(txs); // new array
    expect(out.find((t) => t.id === "a").review_status).toBe("reviewed");
    expect(out.find((t) => t.id === "a").amount).toBe(1); // other fields preserved
    expect(out.find((t) => t.id === "b").review_status).toBe("needs_review"); // others untouched
    // Original array not mutated.
    expect(txs.find((t) => t.id === "a").review_status).toBe("needs_review");
  });

  it("returns a new array unchanged when the id is absent", () => {
    const txs = [{ id: "a", review_status: "needs_review" }];
    const out = markReviewed(txs, "zzz");
    expect(out).not.toBe(txs);
    expect(out[0].review_status).toBe("needs_review");
  });

  it("is safe on null/undefined input", () => {
    expect(markReviewed(null, "a")).toEqual([]);
    expect(markReviewed(undefined, "a")).toEqual([]);
  });
});

describe("possibleDuplicate", () => {
  it("is true only when linked AND still needs_review", () => {
    expect(
      possibleDuplicate({ matched_transaction_id: "x", review_status: "needs_review" }),
    ).toBe(true);
    // Resolved (merge/dismiss both -> reviewed): banner clears even if link kept.
    expect(
      possibleDuplicate({ matched_transaction_id: "x", review_status: "reviewed" }),
    ).toBe(false);
    // needs_review but no link (an ordinary unmatched import) -> not a duplicate.
    expect(
      possibleDuplicate({ matched_transaction_id: null, review_status: "needs_review" }),
    ).toBe(false);
    expect(possibleDuplicate({ review_status: "needs_review" })).toBe(false);
    expect(possibleDuplicate({})).toBe(false);
    expect(possibleDuplicate(null)).toBe(false);
    expect(possibleDuplicate(undefined)).toBe(false);
  });
});

describe("resolveMatch", () => {
  const base = () => [
    { id: "nels", amount: 15, excluded_from_budget: false, review_status: "reviewed" },
    {
      id: "imp",
      amount: 15,
      review_status: "needs_review",
      matched_transaction_id: "nels",
      excluded_from_budget: false,
    },
  ];

  it("merge marks imported reviewed, excludes the twin, keeps the link, immutably", () => {
    const txs = base();
    const out = resolveMatch(txs, "imp", "merge");
    expect(out).not.toBe(txs); // new array
    const imp = out.find((t) => t.id === "imp");
    const nels = out.find((t) => t.id === "nels");
    expect(imp.review_status).toBe("reviewed");
    expect(imp.matched_transaction_id).toBe("nels"); // link kept on merge
    expect(nels.excluded_from_budget).toBe(true); // twin excluded
    // banner now clears
    expect(possibleDuplicate(imp)).toBe(false);
    // original untouched
    expect(txs.find((t) => t.id === "nels").excluded_from_budget).toBe(false);
    expect(txs.find((t) => t.id === "imp").review_status).toBe("needs_review");
  });

  it("dismiss marks imported reviewed and clears the link; twin untouched", () => {
    const out = resolveMatch(base(), "imp", "dismiss");
    const imp = out.find((t) => t.id === "imp");
    const nels = out.find((t) => t.id === "nels");
    expect(imp.review_status).toBe("reviewed");
    expect(imp.matched_transaction_id).toBe(null); // link cleared
    expect(nels.excluded_from_budget).toBe(false); // both kept counting
    expect(possibleDuplicate(imp)).toBe(false);
  });

  it("is safe on null/undefined input", () => {
    expect(resolveMatch(null, "imp", "merge")).toEqual([]);
    expect(resolveMatch(undefined, "imp", "dismiss")).toEqual([]);
  });
});

describe("groupTransactions", () => {
  // VERIFIED: 2026-07-23 is a Thursday (getDay 4); the most-recent Sunday
  // (week start) is 2026-07-19. So 2026-07-20 (Mon) is "This Week", and any
  // July date before the 19th (e.g. the 10th) falls to the "July 2026" month
  // bucket, distinct from the relative today/yesterday/thisWeek July rows.
  const now = new Date(2026, 6, 23, 12, 0, 0);
  const rows = [
    { id: "today1", transaction_date: new Date(2026, 6, 23).toISOString(), amount: 10, currency: "USD", excluded_from_budget: false },
    { id: "today2", transaction_date: new Date(2026, 6, 23).toISOString(), amount: 5, currency: "USD", excluded_from_budget: false },
    { id: "todayExcl", transaction_date: new Date(2026, 6, 23).toISOString(), amount: 99, currency: "USD", excluded_from_budget: true },
    { id: "yest", transaction_date: new Date(2026, 6, 22).toISOString(), amount: 42.18, currency: "USD", excluded_from_budget: false },
    { id: "week", transaction_date: new Date(2026, 6, 20).toISOString(), amount: 7, currency: "USD", excluded_from_budget: false },
    { id: "julOld", transaction_date: new Date(2026, 6, 10).toISOString(), amount: 3, currency: "USD", excluded_from_budget: false },
    { id: "jun", transaction_date: new Date(2026, 5, 5).toISOString(), amount: 8, currency: "USD", excluded_from_budget: false },
  ];

  it("buckets rows into the relative + month groups in order", () => {
    const groups = groupTransactions(rows, now);
    expect(groups.map((g) => g.key)).toEqual([
      "today",
      "yesterday",
      "thisWeek",
      "2026-07",
      "2026-06",
    ]);
  });

  it("sums the running total over budget-affecting (non-excluded) rows only", () => {
    const groups = groupTransactions(rows, now);
    const today = groups.find((g) => g.key === "today");
    // 10 + 5 = 15; the excluded 99 is NOT counted...
    expect(today.total).toBe(15);
    // ...but the excluded row is still present in the group for rendering.
    expect(today.rows.map((r) => r.id)).toContain("todayExcl");
    expect(groups.find((g) => g.key === "yesterday").total).toBe(42.18);
  });

  it("formats month-bucket labels as 'Month Year'", () => {
    const groups = groupTransactions(rows, now);
    expect(groups.find((g) => g.key === "2026-06").label).toBe(
      new Date(2026, 5).toLocaleDateString(undefined, {
        month: "long",
        year: "numeric",
      }),
    );
  });

  it("carries the first non-excluded row's currency for the group", () => {
    const mixed = [
      { id: "x", transaction_date: new Date(2026, 6, 23).toISOString(), amount: 1, currency: "EUR", excluded_from_budget: true },
      { id: "y", transaction_date: new Date(2026, 6, 23).toISOString(), amount: 2, currency: "GBP", excluded_from_budget: false },
    ];
    expect(groupTransactions(mixed, now).find((g) => g.key === "today").currency).toBe("GBP");
  });

  it("returns [] for empty/null input and skips unparseable dates", () => {
    expect(groupTransactions([], now)).toEqual([]);
    expect(groupTransactions(null, now)).toEqual([]);
    const bad = [{ id: "b", transaction_date: "not-a-date", amount: 1 }];
    expect(groupTransactions(bad, now)).toEqual([]);
  });

  it("buckets future-dated rows as today", () => {
    const fut = [{ id: "f", transaction_date: new Date(2026, 6, 25).toISOString(), amount: 1, excluded_from_budget: false }];
    expect(groupTransactions(fut, now)[0].key).toBe("today");
  });
});

describe("searchTransactions", () => {
  const rows = [
    { id: "a", description: "Target run", category_name: "Household", amount: 14.5 },
    { id: "b", description: "Salary", category_name: "Income", amount: 1200 },
    { id: "c", description: "Coffee", category_name: "Dining", amount: 12.5 },
  ];

  it("is identity (same ref) on a blank query", () => {
    expect(searchTransactions(rows, "")).toBe(rows);
    expect(searchTransactions(rows, "   ")).toBe(rows);
  });

  it("narrows by description, case-insensitive", () => {
    expect(searchTransactions(rows, "target").map((r) => r.id)).toEqual(["a"]);
    expect(searchTransactions(rows, "COFFEE").map((r) => r.id)).toEqual(["c"]);
  });

  it("narrows by category name", () => {
    expect(searchTransactions(rows, "dining").map((r) => r.id)).toEqual(["c"]);
  });

  it("matches an amount typed with cents (12.50 -> 12.5)", () => {
    expect(searchTransactions(rows, "12.50").map((r) => r.id)).toEqual(["c"]);
    expect(searchTransactions(rows, "1200").map((r) => r.id)).toEqual(["b"]);
  });

  it("returns [] when nothing matches, and is safe on null", () => {
    expect(searchTransactions(rows, "zzz")).toEqual([]);
    expect(searchTransactions(null, "x")).toEqual([]);
  });
});

describe("isPending", () => {
  it("is true for needs_review rows", () => {
    expect(isPending({ review_status: "needs_review", category_id: "c1" })).toBe(true);
  });
  it("is true for uncategorized rows even when reviewed", () => {
    expect(isPending({ review_status: "reviewed", category_id: null })).toBe(true);
  });
  it("is false for a reviewed, categorized row", () => {
    expect(isPending({ review_status: "reviewed", category_id: "c1" })).toBe(false);
  });
  it("is false/robust for null-ish input", () => {
    expect(isPending(null)).toBe(false);
    expect(isPending(undefined)).toBe(false);
  });
});

describe("pending + uncategorized chips", () => {
  it("isUncategorized is true when category_id is null/absent", () => {
    expect(isUncategorized({ category_id: null })).toBe(true);
    expect(isUncategorized({})).toBe(true);
    expect(isUncategorized({ category_id: "c1" })).toBe(false);
  });

  const rows = [
    { id: "a", review_status: "needs_review", category_id: null },
    { id: "b", review_status: "reviewed", category_id: "c1" },
    { id: "c", review_status: "needs_review", category_id: "c2" },
    { id: "d", review_status: "reviewed", category_id: null },
  ];

  it("filterPending narrows when active, identity when inactive", () => {
    expect(filterPending(rows, true).map((r) => r.id)).toEqual(["a", "c", "d"]);
    expect(filterPending(rows, false)).toBe(rows);
    expect(filterPending(null, true)).toEqual([]);
  });

  it("filterUncategorized narrows when active, identity when inactive", () => {
    expect(filterUncategorized(rows, true).map((r) => r.id)).toEqual(["a", "d"]);
    expect(filterUncategorized(rows, false)).toBe(rows);
    expect(filterUncategorized(null, true)).toEqual([]);
  });
});

describe("applyTransactionFilters", () => {
  const rows = [
    { id: "a", description: "Target", category_name: "Household", amount: 14.5, review_status: "needs_review", category_id: null },
    { id: "b", description: "Salary", category_name: "Income", amount: 1200, review_status: "reviewed", category_id: "c1" },
    { id: "c", description: "Target cafe", category_name: "Dining", amount: 12.5, review_status: "needs_review", category_id: "c2" },
  ];

  it("is identity when nothing is active and the query is blank", () => {
    expect(
      applyTransactionFilters(rows, { query: "", needsReviewOnly: false, pendingOnly: false, uncategorizedOnly: false }),
    ).toBe(rows);
  });

  it("composes search with a chip", () => {
    const out = applyTransactionFilters(rows, {
      query: "target",
      pendingOnly: true,
    });
    // "target" matches a + c; pending keeps needs_review -> a + c both qualify
    expect(out.map((r) => r.id)).toEqual(["a", "c"]);
  });

  it("composes search + uncategorized chip to a single row", () => {
    const out = applyTransactionFilters(rows, {
      query: "target",
      uncategorizedOnly: true,
    });
    expect(out.map((r) => r.id)).toEqual(["a"]);
  });

  it("can produce a zero-result set, and is safe on null", () => {
    expect(applyTransactionFilters(rows, { query: "zzz" })).toEqual([]);
    expect(applyTransactionFilters(null, {})).toEqual([]);
  });
});

describe("findTwin", () => {
  const rows = [
    { id: "a", matched_transaction_id: "b" },
    { id: "b", matched_transaction_id: null },
  ];
  it("returns the row referenced by matched_transaction_id", () => {
    expect(findTwin(rows, rows[0])).toBe(rows[1]);
  });
  it("returns null when there is no link or no match", () => {
    expect(findTwin(rows, rows[1])).toBe(null);
    expect(findTwin(rows, { id: "a", matched_transaction_id: "zzz" })).toBe(null);
    expect(findTwin(null, rows[0])).toBe(null);
  });
});

describe("filterCounts", () => {
  it("counts each chip bucket and duplicates", () => {
    const rows = [
      { id: "1", review_status: "needs_review", category_id: null, matched_transaction_id: "9" }, // review + uncat + dup
      { id: "2", review_status: "reviewed", category_id: "c1" },                                  // none
      { id: "3", review_status: "reviewed", category_id: null },                                  // uncat + pending
    ];
    expect(filterCounts(rows)).toEqual({
      all: 3, needsReview: 1, pending: 2, uncategorized: 2, duplicates: 1,
    });
  });
  it("is all-zero for empty/null", () => {
    expect(filterCounts([])).toEqual({ all: 0, needsReview: 0, pending: 0, uncategorized: 0, duplicates: 0 });
    expect(filterCounts(null)).toEqual({ all: 0, needsReview: 0, pending: 0, uncategorized: 0, duplicates: 0 });
  });
});

describe("splitAccountLabel", () => {
  it("splits a name and its trailing mask", () => {
    expect(splitAccountLabel("Chase Sapphire Preferred Credit Card ••7793")).toEqual({
      name: "Chase Sapphire Preferred Credit Card",
      mask: "7793",
    });
  });

  it("returns the name alone when there is no mask", () => {
    expect(splitAccountLabel("Wallet")).toEqual({ name: "Wallet", mask: null });
  });

  it("returns nulls for null, undefined and empty input", () => {
    expect(splitAccountLabel(null)).toEqual({ name: null, mask: null });
    expect(splitAccountLabel(undefined)).toEqual({ name: null, mask: null });
    expect(splitAccountLabel("")).toEqual({ name: null, mask: null });
  });

  it("splits at the trailing mask when the name itself contains ••", () => {
    expect(splitAccountLabel("My ••Card•• ••7793")).toEqual({
      name: "My ••Card••",
      mask: "7793",
    });
  });

  it("returns a null name when the label is a bare mask", () => {
    expect(splitAccountLabel("••7793")).toEqual({ name: null, mask: "7793" });
  });

  it("passes a longer-than-4-char mask through as a plain name", () => {
    expect(splitAccountLabel("Bank ••123456")).toEqual({
      name: "Bank ••123456",
      mask: null,
    });
  });
});
