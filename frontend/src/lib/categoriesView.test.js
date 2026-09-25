import { describe, it, expect } from "vitest";
import {
  NEAR_LIMIT_THRESHOLD,
  GROUP_ORDER,
  meterPercent,
  healthStatus,
  groupCategories,
  searchCategories,
  filterByChip,
  chipCounts,
  categoryCarryKind,
  showsRolloverBadge,
  showsFundBalance,
  expenseSummary,
  isDuplicateName,
  formMessageFor,
  buildCategoryPatch,
  isValidCategoriesEnvelope,
  loadErrorKey,
  MALFORMED_ENVELOPE,
  SAVE_FAILED_KEY,
  DELETE_FAILED_KEY,
} from "./categoriesView.js";

/** An error shaped like the ones App.svelte's `fetchApi` throws. */
const apiError = (message, status) => {
  const e = new Error(message);
  if (status != null) e.status = status;
  return e;
};

// `linked_budget_id: null` matches the real envelope: `CategoryViewRow.linked_budget_id`
// is an `Option<Uuid>` with no `skip_serializing_if`, so a non-mirror row ships an
// explicit null, not an absent field. It is also what makes `!= null` distinguishable
// from `!== undefined` in `categoryCarryKind` — without it that mutation survives.
const row = (over = {}) => ({
  id: "c1",
  name: "Groceries",
  category_type: "expense",
  category_limit: 100,
  effective_limit: 100,
  spent: 50,
  is_fund: false,
  fund_balance: 0,
  rollover_enabled: false,
  is_mirror: false,
  linked_budget_id: null,
  linked_budget_name: null,
  carried_amount: 0,
  ...over,
});

describe("meterPercent", () => {
  it("returns the spent share of the effective limit", () => {
    expect(meterPercent(50, 100)).toBe(50);
  });
  it("clamps above 100 for an overspend", () => {
    expect(meterPercent(150, 100)).toBe(100);
  });
  it("returns 0 for a null or zero limit", () => {
    expect(meterPercent(50, null)).toBe(0);
    expect(meterPercent(50, 0)).toBe(0);
  });
  it("clamps a negative effective limit to a full meter", () => {
    expect(meterPercent(10, -20)).toBe(100);
  });
});

describe("healthStatus", () => {
  it("is 'none' with no limit", () => {
    expect(healthStatus(50, null)).toBe("none");
  });
  it("is 'healthy' below the near threshold", () => {
    expect(healthStatus(79, 100)).toBe("healthy");
  });
  it("is 'near' at exactly the threshold", () => {
    expect(healthStatus(80, 100)).toBe("near");
  });
  it("is 'over' at exactly the limit", () => {
    expect(healthStatus(100, 100)).toBe("over");
  });
  it("is 'over' beyond the limit", () => {
    expect(healthStatus(101, 100)).toBe("over");
  });
  it("exposes the threshold it uses", () => {
    expect(NEAR_LIMIT_THRESHOLD).toBe(0.8);
  });
  it("is 'none' — not 'over' — for a zero limit with no spend", () => {
    expect(healthStatus(0, 0)).toBe("none");
    expect(meterPercent(0, 0)).toBe(0);
  });
  it("is 'none' for spend against a zero limit: no denominator, not over", () => {
    expect(healthStatus(50, 0)).toBe("none");
  });
  it("is 'over' for a genuinely negative effective limit", () => {
    expect(healthStatus(10, -20)).toBe("over");
    expect(meterPercent(10, -20)).toBe(100);
  });
});

// The two helpers drive the same row's icon, words, meter width and
// aria-valuenow. If they ever disagree the card contradicts itself, so pin the
// agreement rather than each helper in isolation.
describe("healthStatus / meterPercent agreement", () => {
  it("whenever the status is 'over', the meter is full", () => {
    const cases = [
      [0, 0], [50, 0], [0, null], [50, null], [10, -20], [0, -1],
      [100, 100], [101, 100], [500, 100], [80, 100], [79, 100], [0, 100],
    ];
    for (const [spent, limit] of cases) {
      if (healthStatus(spent, limit) === "over") {
        expect(meterPercent(spent, limit), `spent=${spent} limit=${limit}`).toBe(100);
      }
    }
  });
  it("a zero or missing denominator is 'none' with an empty meter, both ways", () => {
    for (const [spent, limit] of [[0, 0], [50, 0], [0, null], [50, null]]) {
      expect(healthStatus(spent, limit), `spent=${spent} limit=${limit}`).toBe("none");
      expect(meterPercent(spent, limit), `spent=${spent} limit=${limit}`).toBe(0);
    }
  });
});

describe("groupCategories", () => {
  it("orders groups Income, Savings, Expense and omits empty ones", () => {
    const groups = groupCategories([
      row({ id: "a", category_type: "expense" }),
      row({ id: "b", category_type: "income" }),
    ]);
    expect(groups.map((g) => g.type)).toEqual(["income", "expense"]);
  });
  it("totals limit and spend per group", () => {
    const groups = groupCategories([
      row({ id: "a", effective_limit: 100, spent: 20 }),
      row({ id: "b", effective_limit: 50, spent: 5 }),
    ]);
    expect(groups[0].totalLimit).toBe(150);
    expect(groups[0].totalSpent).toBe(25);
  });
  it("excludes a null-limit row from the limit total but keeps its spend", () => {
    const groups = groupCategories([
      row({ id: "a", effective_limit: 100, spent: 20 }),
      row({ id: "b", effective_limit: null, category_limit: null, spent: 7 }),
    ]);
    expect(groups[0].totalLimit).toBe(100);
    expect(groups[0].totalSpent).toBe(27);
  });

  // DOCUMENTED behavior, not desired behavior. `categories.category_type` has
  // no CHECK constraint in the database, so this IS reachable — and when it
  // happens the row does not merely lose its group heading, it disappears from
  // the page, from search and from every total.
  it("drops a row whose category_type is not in GROUP_ORDER", () => {
    const groups = groupCategories([
      row({ id: "known", category_type: "expense", spent: 20, effective_limit: 100 }),
      row({ id: "mystery", category_type: "unknown_type", spent: 999, effective_limit: 999 }),
    ]);
    const seen = groups.flatMap((g) => g.rows.map((r) => r.id));
    expect(seen).toEqual(["known"]);
    expect(groups.every((g) => g.type !== "unknown_type")).toBe(true);
    // And its spend is not silently folded into some other group's total.
    expect(groups[0].totalSpent).toBe(20);
  });

  // Drift guard, the client-side twin of the backend's
  // `grouped_types_match_allowed_category_types`. GROUP_ORDER is a
  // hand-maintained literal in a different file from the server's
  // ALLOWED_CATEGORY_TYPES; if a future ticket adds a fourth legitimate type
  // without updating it, every category of that type would vanish exactly the
  // way the unknown-type row above does. Fail loudly here instead.
  it("GROUP_ORDER is exactly the three allowed category types", () => {
    expect([...GROUP_ORDER].sort()).toEqual(["expense", "income", "savings"]);
  });
});

describe("searchCategories", () => {
  it("matches case-insensitively on name", () => {
    const rows = [row({ name: "Groceries" }), row({ id: "c2", name: "Rent" })];
    expect(searchCategories(rows, "gro").map((r) => r.name)).toEqual(["Groceries"]);
  });
  it("returns everything for an empty query", () => {
    const rows = [row(), row({ id: "c2", name: "Rent" })];
    expect(searchCategories(rows, "  ")).toHaveLength(2);
  });
});

describe("filterByChip", () => {
  const rows = [
    row({ id: "ok", spent: 10, effective_limit: 100 }),
    row({ id: "near", spent: 85, effective_limit: 100 }),
    row({ id: "over", spent: 150, effective_limit: 100 }),
    row({ id: "nolimit", category_limit: null, effective_limit: null }),
    row({ id: "fund", is_fund: true }),
    row({ id: "roll", rollover_enabled: true }),
  ];
  it("returns everything for 'all'", () => {
    expect(filterByChip(rows, "all")).toHaveLength(6);
  });
  it("filters over-limit rows", () => {
    expect(filterByChip(rows, "over").map((r) => r.id)).toEqual(["over"]);
  });
  it("filters near-limit rows without including over-limit ones", () => {
    expect(filterByChip(rows, "near").map((r) => r.id)).toEqual(["near"]);
  });
  it("filters limitless rows", () => {
    expect(filterByChip(rows, "nolimit").map((r) => r.id)).toEqual(["nolimit"]);
  });
  it("filters funds and rollover rows", () => {
    expect(filterByChip(rows, "funds").map((r) => r.id)).toEqual(["fund"]);
    expect(filterByChip(rows, "rollover").map((r) => r.id)).toEqual(["roll"]);
  });
  it("excludes income/savings from over and near — they are target-based, not limits", () => {
    // An income category that meets or beats its target is NOT 'over limit';
    // counting it would pollute the Over/Near chips with good news.
    const typed = [
      row({ id: "exp-over", category_type: "expense", spent: 150, effective_limit: 100 }),
      row({ id: "inc-met", category_type: "income", spent: 500, effective_limit: 500 }),
      row({ id: "sav-met", category_type: "savings", spent: 90, effective_limit: 100 }),
    ];
    expect(filterByChip(typed, "over").map((r) => r.id)).toEqual(["exp-over"]);
    expect(filterByChip(typed, "near").map((r) => r.id)).toEqual([]);
  });

  // #433: a fund supersedes the #49 carry, so a fund+rollover row belongs to
  // the funds chip only. The plain rollover row proves the predicate NARROWS
  // rather than simply returning false for everything.
  it("excludes fund rows from the rollover chip but keeps them under funds", () => {
    const mixed = [
      row({ id: "roll", rollover_enabled: true }),
      row({ id: "fundroll", is_fund: true, rollover_enabled: true }),
    ];
    expect(filterByChip(mixed, "rollover").map((r) => r.id)).toEqual(["roll"]);
    expect(filterByChip(mixed, "funds").map((r) => r.id)).toEqual(["fundroll"]);
  });
});

describe("chipCounts", () => {
  it("counts each chip independently", () => {
    const rows = [
      row({ id: "over", spent: 150, effective_limit: 100 }),
      row({ id: "fund", is_fund: true }),
    ];
    expect(chipCounts(rows)).toEqual({
      all: 2, over: 1, near: 0, nolimit: 0, funds: 1, rollover: 0,
    });
  });

  it("counts a fund+rollover row under funds, leaving rollover to the plain row", () => {
    expect(
      chipCounts([
        row({ id: "roll", rollover_enabled: true }),
        row({ id: "fundroll", is_fund: true, rollover_enabled: true }),
      ]),
    ).toEqual({ all: 2, over: 0, near: 0, nolimit: 0, funds: 1, rollover: 1 });
  });

  // #474 REGRESSION GUARD, and the counterpart to `showsFundBalance` being
  // false for this row. The Funds chip follows the `is_fund` FLAG, NOT
  // `categoryCarryKind`: a fund that is also a #52 mirror is still a fund and
  // still keeps its Fund badge — it just carries nothing, so it loses only the
  // balance line. Routing this predicate through `categoryCarryKind` the way
  // the other three now are would silently drop the row from the chip and its
  // count. That mutation is invisible without this test.
  it("still counts a fund+mirror row under funds, though it carries nothing (#474)", () => {
    const fundMirror = row({ id: "fundmirror", is_fund: true, is_mirror: true });
    expect(filterByChip([fundMirror], "funds")).toHaveLength(1);
    expect(chipCounts([fundMirror]).funds).toBe(1);
    // The other half of the same rule: badge yes, balance no.
    expect(showsFundBalance(fundMirror)).toBe(false);
  });

  it("does not count a row whose is_fund field is absent under funds", () => {
    const { is_fund, ...noFund } = row({ id: "x" });
    expect(is_fund).toBe(false);
    expect(filterByChip([noFund], "funds")).toHaveLength(0);
  });

  it("no longer counts mirror or non-expense rows under rollover (#474)", () => {
    expect(
      chipCounts([
        row({ id: "roll", rollover_enabled: true }),
        row({ id: "mirror", is_mirror: true, rollover_enabled: true }),
        row({ id: "income", category_type: "income", rollover_enabled: true }),
      ]),
    ).toEqual({ all: 3, over: 0, near: 0, nolimit: 0, funds: 0, rollover: 1 });
  });
});

// The three arms of `budget::category_carry_for`. `!is_linked` is a conjunct of
// the FUND arm itself, not merely of the rollover arm below it, which is the
// whole reason a fund that is also a #52 mirror carries nothing — see
// `category_carry_for_mirror_is_zero`. The conjuncts within an arm commute, so
// nothing is "checked before" anything; what matters is which arm each appears
// in, and that the fund arm precedes the rollover arm.
describe("categoryCarryKind", () => {
  it("classifies a plain non-fund, non-mirror expense row as rollover", () => {
    expect(categoryCarryKind(row())).toBe("rollover");
  });
  it("classifies a non-mirror expense fund as fund", () => {
    expect(categoryCarryKind(row({ is_fund: true }))).toBe("fund");
  });
  it("classifies a fund that is ALSO a mirror as none — !is_linked is a conjunct of the fund arm", () => {
    expect(categoryCarryKind(row({ is_fund: true, is_mirror: true }))).toBe("none");
  });
  it("classifies a non-fund mirror as none", () => {
    expect(categoryCarryKind(row({ is_mirror: true }))).toBe("none");
  });
  it("classifies income and savings as none", () => {
    expect(categoryCarryKind(row({ category_type: "income" }))).toBe("none");
    expect(categoryCarryKind(row({ category_type: "savings" }))).toBe("none");
  });
  it("classifies a non-expense row flagged is_fund as none — is_expense is a conjunct of both non-terminal arms", () => {
    expect(categoryCarryKind(row({ category_type: "income", is_fund: true }))).toBe("none");
  });
  it("treats a linked_budget_id as a mirror even when is_mirror is absent", () => {
    const { is_mirror, ...noFlag } = row({ linked_budget_id: "b2" });
    expect(is_mirror).toBe(false);
    expect(categoryCarryKind(noFlag)).toBe("none");
  });
  it("treats an absent is_mirror with no linked_budget_id as not a mirror", () => {
    const { is_mirror, linked_budget_id, ...noFlags } = row();
    expect(categoryCarryKind(noFlags)).toBe("rollover");
  });
  it("treats an absent is_fund as not a fund", () => {
    const { is_fund, ...noFlag } = row();
    expect(categoryCarryKind(noFlag)).toBe("rollover");
  });
  it("returns none for a null or undefined row", () => {
    expect(categoryCarryKind(null)).toBe("none");
    expect(categoryCarryKind(undefined)).toBe("none");
  });
  it("returns none for a row with no category_type", () => {
    const { category_type, ...noType } = row();
    expect(categoryCarryKind(noType)).toBe("none");
  });
});

describe("showsRolloverBadge", () => {
  it("shows for a non-fund row with rollover enabled and the budget switch on", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: true, is_fund: false }), true)).toBe(true);
  });
  it("hides for a fund row even with rollover enabled and the budget switch on (#433)", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: true, is_fund: true }), true)).toBe(false);
  });
  it("hides when the budget-level rollover switch is off", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: true }), false)).toBe(false);
  });
  it("hides when the row's own rollover flag is off", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: false }), true)).toBe(false);
  });
  // The next THREE pin the STRICT `=== true` contract. The first two cover
  // absent input — the page renders before the first response lands, so a null
  // row is real and `rolloverActive` reads `data?.…`. Those two alone do NOT
  // pin strictness, though: `!!undefined` is already `false`, so a truthiness
  // loosening survives them. The third passes a TRUTHY-BUT-NOT-`true` value,
  // which is the only shape that distinguishes `=== true` from `!!x`. Do not
  // delete any of them as testing-the-impossible.
  it("hides for a null or undefined row", () => {
    expect(showsRolloverBadge(null, true)).toBe(false);
    expect(showsRolloverBadge(undefined, true)).toBe(false);
  });
  it("hides when rolloverActive is undefined", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: true }), undefined)).toBe(false);
  });
  it("hides for a truthy-but-not-true rolloverActive, pinning === true over truthiness", () => {
    expect(showsRolloverBadge(row({ rollover_enabled: true }), 1)).toBe(false);
    expect(showsRolloverBadge(row({ rollover_enabled: true }), "yes")).toBe(false);
  });

  // Every fixture above comes from `row()`, which sets `is_fund: false`
  // EXPLICITLY — so `!== true` and `=== false` are indistinguishable across the
  // whole suite without this. A row whose `is_fund` is simply absent is not
  // hypothetical: it is what an older backend, or any envelope predating #228,
  // sends, and it must badge like the non-fund it is.
  it("shows for a row whose is_fund field is absent, not just false", () => {
    const { is_fund, ...noFund } = row({ rollover_enabled: true });
    expect(is_fund).toBe(false);
    expect(showsRolloverBadge(noFund, true)).toBe(true);
    expect(filterByChip([{ ...noFund, id: "x" }], "rollover")).toHaveLength(1);
  });

  // `rollover_enabled` is the fourth boolean in these expressions and the one
  // the absent-flag discipline above originally skipped — every other fixture
  // sets it explicitly, so `=== true`, `!== false` and bare truthiness were all
  // indistinguishable on it. This pins it on BOTH sides, since the badge and
  // the chip duplicate that term by convention rather than sharing it.
  it("treats an absent rollover_enabled as off, not on", () => {
    const { rollover_enabled, ...noFlag } = row();
    expect(rollover_enabled).toBe(false);
    expect(showsRolloverBadge(noFlag, true)).toBe(false);
    expect(filterByChip([{ ...noFlag, id: "x" }], "rollover")).toHaveLength(0);
  });

  // #474 — the two shapes whose carry is literally zero.
  it("hides for a #52 mirror row even with rollover enabled and the switch on", () => {
    expect(showsRolloverBadge(row({ is_mirror: true, rollover_enabled: true }), true)).toBe(false);
  });
  it("hides for a mirror identified only by linked_budget_id", () => {
    const { is_mirror, ...noFlag } = row({ linked_budget_id: "b2", rollover_enabled: true });
    expect(showsRolloverBadge(noFlag, true)).toBe(false);
  });
  it("hides for income and savings rows", () => {
    expect(showsRolloverBadge(row({ category_type: "income", rollover_enabled: true }), true)).toBe(false);
    expect(showsRolloverBadge(row({ category_type: "savings", rollover_enabled: true }), true)).toBe(false);
  });
  it("still shows for a plain expense row whose is_mirror field is absent", () => {
    const { is_mirror, ...noFlag } = row({ rollover_enabled: true });
    expect(showsRolloverBadge(noFlag, true)).toBe(true);
  });
});

describe("showsFundBalance", () => {
  it("shows the balance for a non-mirror expense fund", () => {
    expect(showsFundBalance(row({ is_fund: true }))).toBe(true);
  });
  // AC 3: `category_carry_for` returns (0.0, 0.0) for a fund that is also a
  // mirror, so its fund_balance is NOT what the row carries.
  it("hides the balance for a fund that is also a mirror", () => {
    expect(showsFundBalance(row({ is_fund: true, is_mirror: true }))).toBe(false);
  });
  it("hides the balance for a fund whose linked_budget_id is set", () => {
    expect(showsFundBalance(row({ is_fund: true, linked_budget_id: "b2" }))).toBe(false);
  });
  it("hides the balance for a non-fund row", () => {
    expect(showsFundBalance(row())).toBe(false);
  });
  it("hides the balance for a non-expense row flagged is_fund", () => {
    expect(showsFundBalance(row({ category_type: "savings", is_fund: true }))).toBe(false);
  });
  it("hides the balance for a null or undefined row", () => {
    expect(showsFundBalance(null)).toBe(false);
    expect(showsFundBalance(undefined)).toBe(false);
  });
});

// Badge and chip agree on the `categoryCarryKind` term BY CONSTRUCTION since
// #474 — both call it — so this block can no longer see a mutation inside the
// classifier; the `["roll"]` guard below is what catches those. What parity
// still earns: the `rollover_enabled` term is duplicated between the two
// expressions by CONVENTION, and wholesale replacement of either predicate.
// They are NOT equivalent overall — the chip has no `rolloverActive` term on
// purpose, since the strip drops the chip entirely on an inert budget — so this
// compares them at `rolloverActive === true`, the one regime where they agree.
describe("rollover badge / chip parity (#433, #474)", () => {
  it("selects exactly the same rows as the badge when the budget switch is on", () => {
    const rows = [
      row({ id: "roll", rollover_enabled: true }),
      row({ id: "fundroll", is_fund: true, rollover_enabled: true }),
      row({ id: "plain", is_fund: false, rollover_enabled: false }),
      row({ id: "mirror", is_mirror: true, rollover_enabled: true }),
      row({ id: "fundmirror", is_fund: true, is_mirror: true, rollover_enabled: true }),
      row({ id: "income", category_type: "income", rollover_enabled: true }),
      row({ id: "savings", category_type: "savings", rollover_enabled: true }),
    ];
    expect(filterByChip(rows, "rollover")).toEqual(
      rows.filter((r) => showsRolloverBadge(r, true)),
    );
    // Guard against the assertion passing because both sides are empty. Only
    // the plain expense row carries under #49; every other shape here is one of
    // the three arms `category_carry_for` zeroes.
    expect(filterByChip(rows, "rollover").map((r) => r.id)).toEqual(["roll"]);
  });
});

describe("expenseSummary", () => {
  it("sums expense rows only and computes remaining", () => {
    const rows = [
      row({ id: "e", category_type: "expense", effective_limit: 100, spent: 30 }),
      row({ id: "i", category_type: "income", effective_limit: 500, spent: 500 }),
    ];
    expect(expenseSummary(rows)).toEqual({ totalLimit: 100, totalSpent: 30, remaining: 70 });
  });
  it("allows a negative remaining on an overspend", () => {
    const rows = [row({ effective_limit: 100, spent: 130 })];
    expect(expenseSummary(rows).remaining).toBe(-30);
  });
});

describe("isDuplicateName", () => {
  it("matches the 409 body BOTH write paths now return", () => {
    // create_category and update_category share duplicate_category_name_error.
    const e = apiError("A category with that name already exists in this budget", 409);
    expect(isDuplicateName(e)).toBe(true);
  });
  it("still matches raw sqlx wording, so an older backend degrades gracefully", () => {
    expect(isDuplicateName(apiError("duplicate key value violates unique constraint", 500))).toBe(true);
    expect(isDuplicateName(apiError("UNIQUE constraint failed", 500))).toBe(true);
  });
  it("is case-insensitive", () => {
    expect(isDuplicateName(apiError("A Category With That Name ALREADY EXISTS"))).toBe(true);
  });
  it("does not match unrelated failures", () => {
    expect(isDuplicateName(apiError("budget is closed", 409))).toBe(false);
    expect(isDuplicateName(apiError("Category not found", 404))).toBe(false);
    expect(isDuplicateName(undefined)).toBe(false);
    expect(isDuplicateName({})).toBe(false);
  });
});

describe("formMessageFor", () => {
  it("maps 403 to the read-only explanation", () => {
    expect(formMessageFor(apiError("Insufficient permissions", 403))).toBe("readOnly");
  });
  it("maps the linked-rollup 409 to the mirror explanation", () => {
    const e = apiError(
      "This is a linked rollup category and can't be edited directly. Manage it by rolling the source budget up or unlinking it.",
      409,
    );
    expect(formMessageFor(e)).toBe("mirrorNotEditable");
  });
  it("maps the fund/expense 400 to its own key instead of 'try again'", () => {
    // Deterministic: retrying this request can only ever fail the same way, so
    // it must never reach the generic saveFailed copy.
    const e = apiError(
      "Only expense categories can be funds. Turn off fund status before changing the type.",
      400,
    );
    expect(formMessageFor(e)).toBe("fundExpenseOnly");
  });
  it("maps the closed-budget 409 to the closed explanation", () => {
    expect(formMessageFor(apiError("budget is closed", 409))).toBe("closedBudget");
  });
  it("does not label a 'closed'-containing transport error as a closed budget", () => {
    // A network error like "the server closed the connection" carries no
    // status; matching the bare body would mislabel an outage as a closed
    // budget (#494). ensure_not_closed ALWAYS 409s, so the status gate is exact.
    expect(formMessageFor(apiError("The server closed the connection"))).toBe(SAVE_FAILED_KEY);
    expect(formMessageFor(apiError("The server closed the connection"), DELETE_FAILED_KEY)).toBe(DELETE_FAILED_KEY);
  });
  it("maps 404 to notFound — the row is gone, retrying always 404s", () => {
    expect(formMessageFor(apiError("Category not found", 404))).toBe("notFound");
    expect(formMessageFor(apiError("Category not found in this budget", 404))).toBe("notFound");
  });
  it("falls back to saveFailed for anything unrecognized", () => {
    expect(formMessageFor(apiError("Internal server error", 500))).toBe(SAVE_FAILED_KEY);
    expect(formMessageFor(apiError("Failed to fetch"))).toBe(SAVE_FAILED_KEY);
    expect(formMessageFor(undefined)).toBe(SAVE_FAILED_KEY);
  });
  it("takes a caller-supplied fallback so a failed DELETE never says 'couldn't save'", () => {
    expect(formMessageFor(apiError("Internal server error", 500), DELETE_FAILED_KEY)).toBe(DELETE_FAILED_KEY);
    // ...but a recognized cause still wins over the fallback on the delete path.
    expect(formMessageFor(apiError("budget is closed", 409), DELETE_FAILED_KEY)).toBe("closedBudget");
    expect(formMessageFor(apiError("Category not found", 404), DELETE_FAILED_KEY)).toBe("notFound");
  });

  // The chain's ORDER is the behavior. Each case below satisfies TWO branches
  // at once; the assertion says which one must win.
  describe("precedence", () => {
    it("403 outranks every message match", () => {
      expect(formMessageFor(apiError("budget is closed", 403))).toBe("readOnly");
      expect(formMessageFor(apiError("This is a linked rollup category", 403))).toBe("readOnly");
      expect(formMessageFor(apiError("Only expense categories can be funds.", 403))).toBe("readOnly");
    });
    it("the linked-rollup match outranks 'closed'", () => {
      // A mirror row in a closing budget must still say WHY it is uneditable.
      const e = apiError("This is a linked rollup category and can't be edited; budget is closed", 409);
      expect(formMessageFor(e)).toBe("mirrorNotEditable");
    });
    it("the fund/expense match outranks 'closed'", () => {
      const e = apiError("Only expense categories can be funds. The type is closed to change.", 400);
      expect(formMessageFor(e)).toBe("fundExpenseOnly");
    });
    it("a 404 maps to notFound even when the body mentions 'closed'", () => {
      // `ensure_not_closed` ALWAYS 409s, so a 404 whose body says "budget is
      // closed" is not a closed-budget rejection — the row is simply gone.
      // The status gate on rule 4 makes the body irrelevant here.
      expect(formMessageFor(apiError("budget is closed", 404))).toBe("notFound");
    });
    it("the linked-rollup body match outranks the bare 404 status", () => {
      // 404 is only a status with no distinguishing text, so a body that
      // recognized a more specific cause still wins.
      expect(formMessageFor(apiError("This is a linked rollup category", 404))).toBe("mirrorNotEditable");
    });
    it("404 outranks the fallback", () => {
      expect(formMessageFor(apiError("Category not found", 404), DELETE_FAILED_KEY)).not.toBe(DELETE_FAILED_KEY);
    });
  });
});

describe("isValidCategoriesEnvelope", () => {
  it("accepts a well-formed envelope, including a legitimately empty budget", () => {
    expect(isValidCategoriesEnvelope({ categories: [], currency: "USD" })).toBe(true);
    expect(isValidCategoriesEnvelope({ categories: [row()] })).toBe(true);
  });
  it("rejects the null parseApiResponse returns for a 204 or empty body", () => {
    // Without this, an empty response renders as "this budget has no
    // categories yet" and invites re-creating categories that already exist.
    expect(isValidCategoriesEnvelope(null)).toBe(false);
    expect(isValidCategoriesEnvelope(undefined)).toBe(false);
  });
  it("rejects an envelope whose categories field is missing or the wrong type", () => {
    expect(isValidCategoriesEnvelope({ currency: "USD" })).toBe(false);
    expect(isValidCategoriesEnvelope({ categories: null })).toBe(false);
    expect(isValidCategoriesEnvelope({ categories: {} })).toBe(false);
    expect(isValidCategoriesEnvelope({ categories: "none" })).toBe(false);
  });
});

describe("loadErrorKey", () => {
  it("flags a malformed envelope distinctly from a transport failure", () => {
    const e = new Error("malformed categories-view response");
    e[MALFORMED_ENVELOPE] = true;
    expect(loadErrorKey(e)).toBe("loadErrorMalformed");
  });
  it("maps the auth/permission statuses", () => {
    expect(loadErrorKey(apiError("Access denied", 403))).toBe("loadErrorDenied");
    expect(loadErrorKey(apiError("Unauthorized", 401))).toBe("loadErrorDenied");
  });
  it("maps 404 and the 5xx family", () => {
    expect(loadErrorKey(apiError("Budget not found", 404))).toBe("loadErrorNotFound");
    expect(loadErrorKey(apiError("Internal server error", 500))).toBe("loadErrorServer");
    expect(loadErrorKey(apiError("Bad gateway", 502))).toBe("loadErrorServer");
  });
  it("treats a statusless error as a transport failure", () => {
    // fetch() rejects with "Failed to fetch" / an AbortError — no HTTP status
    // ever existed, so "check your connection" is the only useful advice.
    expect(loadErrorKey(apiError("Failed to fetch"))).toBe("loadErrorOffline");
    expect(loadErrorKey(undefined)).toBe("loadErrorOffline");
  });
  it("falls back to the generic key for an unclassified 4xx", () => {
    expect(loadErrorKey(apiError("Bad request", 400))).toBe("loadError");
    expect(loadErrorKey(apiError("Too many requests", 429))).toBe("loadError");
  });
  // The whole point of the helper: the raw server body must never be the copy.
  it("never returns the error's own message", () => {
    for (const e of [apiError("Internal server error", 500), apiError("Access denied", 403), apiError("Failed to fetch")]) {
      expect(loadErrorKey(e)).not.toContain(" ");
      expect(loadErrorKey(e)).toMatch(/^loadError/);
    }
  });
});

describe("buildCategoryPatch", () => {
  const editing = {
    id: "c1",
    name: "Rent",
    category_type: "expense",
    category_limit: 100,
    rollover_enabled: false,
    is_fund: false,
  };
  const form = (over = {}) => ({
    name: "Rent",
    limit: "100",
    rollover: false,
    fund: false,
    ...over,
  });

  it("sends nothing when nothing changed", () => {
    expect(buildCategoryPatch(form(), editing)).toEqual({});
  });

  // Rule 2: the name is trimmed BEFORE comparison, so whitespace alone is not
  // an edit. An empty patch is what tells save() to skip the request entirely
  // (Rule 4), so this is the difference between "no request" and a pointless
  // round trip that could still 409 on a concurrent rename.
  it("treats a whitespace-only name difference as a no-op", () => {
    expect(buildCategoryPatch(form({ name: "  Rent  " }), editing)).toEqual({});
  });
  it("sends the TRIMMED name when it genuinely changed", () => {
    expect(buildCategoryPatch(form({ name: "  Housing  " }), editing)).toEqual({ name: "Housing" });
  });

  // Rule 3: the server does `category_limit = COALESCE($3, category_limit)`, so
  // a null means "unchanged" and can never clear a limit. Sending nothing is
  // the honest encoding of an emptied box.
  it("omits the limit entirely when the box is emptied", () => {
    expect(buildCategoryPatch(form({ limit: "" }), editing)).toEqual({});
    expect(buildCategoryPatch(form({ limit: "   " }), editing)).toEqual({});
    expect("category_limit" in buildCategoryPatch(form({ limit: "" }), editing)).toBe(false);
  });
  it("omits the limit for a row that has none, rather than sending null", () => {
    const noLimit = { ...editing, category_limit: null };
    expect(buildCategoryPatch(form({ limit: "" }), noLimit)).toEqual({});
  });
  it("sends a changed limit as a number, including 0", () => {
    expect(buildCategoryPatch(form({ limit: "250" }), editing)).toEqual({ category_limit: 250 });
    expect(buildCategoryPatch(form({ limit: "0" }), editing)).toEqual({ category_limit: 0 });
  });
  it("sends a newly-set limit for a row that had none", () => {
    const noLimit = { ...editing, category_limit: null };
    expect(buildCategoryPatch(form({ limit: "40" }), noLimit)).toEqual({ category_limit: 40 });
  });

  it("sends rollover_enabled only when it flipped", () => {
    expect(buildCategoryPatch(form({ rollover: true }), editing)).toEqual({ rollover_enabled: true });
    expect(buildCategoryPatch(form({ rollover: false }), editing)).toEqual({});
  });

  // Rule 1: is_fund rides along ONLY when the control was actually shown. The
  // checkbox is expense-only and absent in create mode, so an unshown control
  // must never contribute a field — the server's expense-only CHECK would 400
  // on a value the user never touched.
  it("omits is_fund when the fund control is not shown, even if the values differ", () => {
    expect(buildCategoryPatch(form({ fund: true }), editing)).toEqual({});
    expect(buildCategoryPatch(form({ fund: true }), editing, { showFund: false })).toEqual({});
  });
  it("sends is_fund when the control is shown and it flipped", () => {
    expect(buildCategoryPatch(form({ fund: true }), editing, { showFund: true })).toEqual({ is_fund: true });
    const fundRow = { ...editing, is_fund: true };
    expect(buildCategoryPatch(form({ fund: false }), fundRow, { showFund: true })).toEqual({ is_fund: false });
  });
  it("omits is_fund when the control is shown but unchanged", () => {
    expect(buildCategoryPatch(form(), editing, { showFund: true })).toEqual({});
  });

  it("combines every changed field in one patch", () => {
    expect(
      buildCategoryPatch(
        form({ name: " Housing ", limit: "250", rollover: true, fund: true }),
        editing,
        { showFund: true },
      ),
    ).toEqual({
      name: "Housing",
      category_limit: 250,
      rollover_enabled: true,
      is_fund: true,
    });
  });

  // Rule 4's degenerate input: create mode has no `editing` image to diff
  // against, so there is nothing this builder could honestly produce.
  it("returns an empty patch with no row to diff against", () => {
    expect(buildCategoryPatch(form({ name: "New" }), null, { showFund: true })).toEqual({});
  });
  it("tolerates a missing form object", () => {
    expect(buildCategoryPatch(undefined, { ...editing, name: "" })).toEqual({});
  });
});
