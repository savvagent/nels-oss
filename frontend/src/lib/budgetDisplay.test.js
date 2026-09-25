import { describe, it, expect } from "vitest";
import {
  displayBudgetName,
  activeBudgetStatuses,
  zeroBasedAvailable,
  zeroBasedAllocated,
  zeroBasedLeft,
} from "./budgetDisplay.js";

describe("displayBudgetName", () => {
  it("strips a trailing whole-word 'Budget'", () => {
    expect(displayBudgetName("Groceries Budget")).toBe("Groceries");
  });

  it("strips a trailing 'Budget' even with trailing whitespace after it", () => {
    // A user-typed trailing space (e.g. "Groceries Budget ") must not defeat
    // the match — the regex's trailing \s* absorbs it before the $ anchor.
    expect(displayBudgetName("Groceries Budget ")).toBe("Groceries");
    expect(displayBudgetName("Groceries Budget   ")).toBe("Groceries");
  });

  it("is case-insensitive", () => {
    expect(displayBudgetName("Groceries budget")).toBe("Groceries");
    expect(displayBudgetName("Groceries BUDGET")).toBe("Groceries");
  });

  it("leaves a mid-string 'Budget' untouched", () => {
    expect(displayBudgetName("Budget for July")).toBe("Budget for July");
  });

  it("leaves a name with no 'Budget' word untouched", () => {
    expect(displayBudgetName("Groceries")).toBe("Groceries");
  });

  it("returns the bare word unchanged (no leading whitespace for the regex to strip)", () => {
    expect(displayBudgetName("Budget")).toBe("Budget");
  });

  it("falls back to the original (untrimmed) name when stripping would empty it", () => {
    // A leading-space name IS matched by /\s+budget\s*$/i in full, so the
    // stripped result is "" and the fallback branch (stripped || name)
    // actually fires — unlike the bare-word case above, which never reaches
    // that branch because the regex doesn't match without a preceding
    // whitespace run.
    expect(displayBudgetName(" Budget")).toBe(" Budget");
  });

  it("does not strip a word that merely contains 'budget' as a substring", () => {
    expect(displayBudgetName("July Budgeting")).toBe("July Budgeting");
  });

  it("handles null/undefined/empty input without throwing", () => {
    expect(displayBudgetName(null)).toBe(null);
    expect(displayBudgetName(undefined)).toBe(undefined);
    expect(displayBudgetName("")).toBe("");
  });
});

describe("activeBudgetStatuses", () => {
  it("returns [] for a null or undefined budget", () => {
    expect(activeBudgetStatuses(null)).toEqual([]);
    expect(activeBudgetStatuses(undefined)).toEqual([]);
  });

  it("returns [] when no status flags are active", () => {
    expect(activeBudgetStatuses({ budget_type: "time_based" })).toEqual([]);
  });

  it("reports a project budget", () => {
    const statuses = activeBudgetStatuses({ budget_type: "project" });
    expect(statuses).toEqual([
      {
        key: "project",
        label: "Project",
        description:
          "A one-off budget for a finite effort — doesn't reset or carry over like a recurring budget.",
      },
    ]);
  });

  it("reports a fixed-amount budget with the amount in the description", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
      base_amount: 250,
    });
    expect(statuses).toEqual([
      {
        key: "fixed",
        label: "Fixed",
        description:
          "Fixed amount — set to $250.00, not derived from category totals",
      },
    ]);
  });

  it("defaults the fixed-amount description to $0.00 when base_amount is missing", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
    });
    expect(statuses[0].description).toBe(
      "Fixed amount — set to $0.00, not derived from category totals",
    );
  });

  it("reports closed and archived independently", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      closed_at: "2026-06-01T00:00:00Z",
      archived_at: "2026-06-02T00:00:00Z",
    });
    expect(statuses.map((s) => s.key)).toEqual(["closed", "archived"]);
  });

  it("reports rollover with the 'some categories' wording when partial", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollover_enabled: true,
      has_partial_category_rollover: true,
    });
    expect(statuses).toEqual([
      {
        key: "rollover",
        label: "Rollover (some)",
        description: "Rollover on — selected categories carry forward",
      },
    ]);
  });

  it("reports rollover with the 'all categories' wording when not partial", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollover_enabled: true,
      has_partial_category_rollover: false,
    });
    expect(statuses).toEqual([
      {
        key: "rollover",
        label: "Rollover",
        description: "Rollover on — all categories carry forward",
      },
    ]);
  });

  it("suppresses rollover and auto-renew for a project budget even if the flags are set", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "project",
      rollover_enabled: true,
      auto_renew: true,
    });
    expect(statuses.map((s) => s.key)).toEqual(["project"]);
  });

  it("still reports fixed/closed/archived/rollup for a project budget (only rollover/auto-renew are suppressed)", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "project",
      amount_mode: "fixed",
      base_amount: 50,
      closed_at: "2026-06-01T00:00:00Z",
      archived_at: "2026-06-02T00:00:00Z",
      rollup_child_ids: ["a"],
      aggregated_base_amount: 50,
    });
    expect(statuses.map((s) => s.key)).toEqual([
      "project",
      "fixed",
      "closed",
      "archived",
      "rollup",
    ]);
  });

  it("reports auto-renew with the next renewal date when present", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      auto_renew: true,
      next_renewal_at: "2026-08-01T00:00:00Z",
    });
    expect(statuses[0].key).toBe("auto-renew");
    expect(statuses[0].description).toContain("Auto-renews on");
  });

  it("reports auto-renew with a generic description when no next date is known", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      auto_renew: true,
    });
    expect(statuses[0].description).toBe("Auto-renews each period");
  });

  it("reports rollup (parent) with child count and aggregated amount", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a", "b"],
      aggregated_base_amount: 500,
    });
    expect(statuses).toEqual([
      {
        key: "rollup",
        label: "Rollup",
        description:
          "Aggregates 2 rolled-up budgets — combined budget $500.00",
      },
    ]);
  });

  it("defaults the rollup description to $0.00 when aggregated_base_amount is missing", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a", "b"],
    });
    expect(statuses[0].description).toBe(
      "Aggregates 2 rolled-up budgets — combined budget $0.00",
    );
  });

  it("singularizes 'budget' when there is exactly one rolled-up child", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a"],
      aggregated_base_amount: 100,
    });
    expect(statuses[0].description).toBe(
      "Aggregates 1 rolled-up budget — combined budget $100.00",
    );
  });

  it("reports 'rolled up' (child) only when there are no rollup children", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_parent_id: "parent-1",
    });
    expect(statuses).toEqual([
      {
        key: "rolled-up",
        label: "Rolled up",
        description: "Rolled up into another budget",
      },
    ]);
  });

  it("prefers rollup over rolled-up when both ids are somehow present", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      rollup_child_ids: ["a"],
      aggregated_base_amount: 100,
      rollup_parent_id: "parent-1",
    });
    expect(statuses.map((s) => s.key)).toEqual(["rollup"]);
  });

  it("returns statuses in a stable, badge-matching order", () => {
    const statuses = activeBudgetStatuses({
      budget_type: "time_based",
      amount_mode: "fixed",
      base_amount: 10,
      closed_at: "2026-06-01T00:00:00Z",
      archived_at: "2026-06-02T00:00:00Z",
      rollover_enabled: true,
      auto_renew: true,
      rollup_parent_id: "parent-1",
    });
    expect(statuses.map((s) => s.key)).toEqual([
      "fixed",
      "closed",
      "archived",
      "rollover",
      "auto-renew",
      "rolled-up",
    ]);
  });
});

describe("zero-based figures", () => {
  it("use income and derive allocated as expense base + own savings", () => {
    const b = {
      aggregated_base_amount: 8062,
      aggregated_savings_amount: 385,
      aggregated_income_amount: 14906,
    };
    expect(zeroBasedAvailable(b)).toBe(14906);
    expect(zeroBasedAllocated(b)).toBe(8447);
    expect(zeroBasedLeft(b)).toBe(6459);
  });
  it("are null-safe", () => {
    expect(zeroBasedAvailable(null)).toBe(0);
    expect(zeroBasedAllocated(null)).toBe(0);
    expect(zeroBasedAllocated(undefined)).toBe(0);
    expect(zeroBasedLeft(null)).toBe(0);
    expect(zeroBasedLeft({ aggregated_income_amount: 100 })).toBe(100);
    // Partial object: savings absent -> counts as 0, so allocated = base only.
    expect(zeroBasedAllocated({ aggregated_base_amount: 8062 })).toBe(8062);
  });
  it("reports negative Left when over-allocated", () => {
    expect(
      zeroBasedLeft({
        aggregated_income_amount: 100,
        aggregated_base_amount: 120,
        aggregated_savings_amount: 30,
      }),
    ).toBe(-50);
  });
});
