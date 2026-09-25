import { describe, it, expect } from "vitest";
import {
  TIME_FRAMES,
  BUDGET_STRATEGIES,
  buildUpdatePayload,
  buildEditPatch,
  rollupSummary,
  isReadOnly,
  isRollupLinked,
} from "./budgetDetails.js";

describe("TIME_FRAMES", () => {
  it("lists the three backend-recognized time frames", () => {
    expect(TIME_FRAMES).toEqual(["monthly", "quarterly", "yearly"]);
  });
});

describe("buildUpdatePayload", () => {
  const existing = {
    name: "Groceries",
    description: "Monthly food budget",
    time_frame: "monthly",
    budget_limit: 500,
    budget_type: "time_based",
    rollover_enabled: true,
    auto_renew: true,
    amount_mode: "derived",
  };

  it("echoes name/time_frame/description/budget_limit unchanged with no patch", () => {
    const payload = buildUpdatePayload(existing, {});
    expect(payload.name).toBe("Groceries");
    expect(payload.time_frame).toBe("monthly");
    expect(payload.description).toBe("Monthly food budget");
    expect(payload.budget_limit).toBe(500);
  });

  it("omits budget_type/rollover_enabled/auto_renew/amount_mode with no patch (verified via JSON, not just undefined)", () => {
    const payload = buildUpdatePayload(existing, {});
    const json = JSON.stringify(payload);
    expect(json).not.toContain("budget_type");
    expect(json).not.toContain("rollover_enabled");
    expect(json).not.toContain("auto_renew");
    expect(json).not.toContain("amount_mode");
  });

  it("overrides only name when patching name", () => {
    const payload = buildUpdatePayload(existing, { name: "Food" });
    expect(payload.name).toBe("Food");
    expect(payload.time_frame).toBe("monthly");
    expect(JSON.stringify(payload)).not.toContain("budget_type");
  });

  it("overrides only time_frame when patching time_frame", () => {
    const payload = buildUpdatePayload(existing, { time_frame: "yearly" });
    expect(payload.time_frame).toBe("yearly");
    expect(payload.name).toBe("Groceries");
  });

  it("includes budget_type only when patching budget_type", () => {
    const payload = buildUpdatePayload(existing, { budget_type: "project" });
    expect(payload.budget_type).toBe("project");
    expect(payload.name).toBe("Groceries");
    expect(payload.time_frame).toBe("monthly");
  });

  it("defaults description to null when the existing budget has none", () => {
    const noDescription = { ...existing, description: null };
    const payload = buildUpdatePayload(noDescription, {});
    expect(payload.description).toBe(null);
  });

  it("defaults budget_limit to null when the existing budget has none", () => {
    const noLimit = { ...existing, budget_limit: null };
    const payload = buildUpdatePayload(noLimit, {});
    expect(payload.budget_limit).toBe(null);
  });
});

describe("buildEditPatch", () => {
  const budget = { name: "Groceries", time_frame: "monthly", budget_type: "time_based" };

  it("returns a name patch for a genuinely changed, non-empty name", () => {
    expect(buildEditPatch("name", "Food", budget)).toEqual({ name: "Food" });
  });

  it("trims a name before comparing/patching", () => {
    expect(buildEditPatch("name", "  Food  ", budget)).toEqual({ name: "Food" });
  });

  it("returns null for an empty or whitespace-only name (declined client-side)", () => {
    expect(buildEditPatch("name", "", budget)).toBe(null);
    expect(buildEditPatch("name", "   ", budget)).toBe(null);
  });

  it("returns null when the name is unchanged", () => {
    expect(buildEditPatch("name", "Groceries", budget)).toBe(null);
    expect(buildEditPatch("name", "  Groceries  ", budget)).toBe(null);
  });

  it("returns a time_frame patch for a genuinely changed value", () => {
    expect(buildEditPatch("time_frame", "yearly", budget)).toEqual({ time_frame: "yearly" });
  });

  it("returns null when time_frame is unchanged", () => {
    expect(buildEditPatch("time_frame", "monthly", budget)).toBe(null);
  });

  it("returns a budget_type patch for a genuinely changed value", () => {
    expect(buildEditPatch("budget_type", "project", budget)).toEqual({ budget_type: "project" });
  });

  it("returns null when budget_type is unchanged", () => {
    expect(buildEditPatch("budget_type", "time_based", budget)).toBe(null);
  });

  it("returns null for an unrecognized field", () => {
    expect(buildEditPatch("description", "new description", budget)).toBe(null);
  });

  it("returns null for a null/undefined budget rather than throwing", () => {
    expect(buildEditPatch("name", "Food", null)).toEqual({ name: "Food" });
    expect(buildEditPatch("time_frame", "yearly", undefined)).toEqual({ time_frame: "yearly" });
  });
});

describe("rollupSummary", () => {
  const base = { rollup_parent_id: null, rollup_child_ids: [] };

  it("reports no parent and no children for a standalone budget", () => {
    const summary = rollupSummary(base, null, []);
    expect(summary).toEqual({
      hasParent: false,
      parentName: null,
      hasChildren: false,
      childNames: [],
    });
  });

  it("reports a parent when rollup_parent_id is set", () => {
    const budget = { ...base, rollup_parent_id: "parent-id" };
    const summary = rollupSummary(budget, "Household", []);
    expect(summary.hasParent).toBe(true);
    expect(summary.parentName).toBe("Household");
  });

  it("reports children when rollup_child_ids is non-empty", () => {
    const budget = { ...base, rollup_child_ids: ["a", "b"] };
    const summary = rollupSummary(budget, null, ["Vacation", "Car"]);
    expect(summary.hasChildren).toBe(true);
    expect(summary.childNames).toEqual(["Vacation", "Car"]);
  });

  it("reports both a parent and children when the budget is both simultaneously", () => {
    const budget = { rollup_parent_id: "parent-id", rollup_child_ids: ["a"] };
    const summary = rollupSummary(budget, "Household", ["Vacation"]);
    expect(summary).toEqual({
      hasParent: true,
      parentName: "Household",
      hasChildren: true,
      childNames: ["Vacation"],
    });
  });

  it("defaults childNames to an empty array when ids are present but names haven't resolved yet", () => {
    const budget = { ...base, rollup_child_ids: ["a", "b"] };
    const summary = rollupSummary(budget, null, undefined);
    expect(summary.hasChildren).toBe(true);
    expect(summary.childNames).toEqual([]);
  });
});

describe("BUDGET_STRATEGIES", () => {
  it("lists both shipped strategies", () => {
    expect(BUDGET_STRATEGIES).toEqual(["zero_based", "limit_spent_remaining"]);
  });
});

describe("buildEditPatch budget_strategy", () => {
  it("returns null when budget_strategy is unchanged", () => {
    const budget = { budget_strategy: "zero_based" };
    expect(buildEditPatch("budget_strategy", "zero_based", budget)).toBeNull();
  });

  it("returns a patch when budget_strategy changes", () => {
    const budget = { budget_strategy: "zero_based" };
    expect(buildEditPatch("budget_strategy", "limit_spent_remaining", budget)).toEqual({
      budget_strategy: "limit_spent_remaining",
    });
  });
});

describe("buildUpdatePayload budget_strategy", () => {
  it("includes patch.budget_strategy (undefined when not patched)", () => {
    const existing = {
      name: "X",
      time_frame: "monthly",
      budget_limit: null,
      budget_type: "time_based",
      budget_strategy: "zero_based",
    };
    const payload = buildUpdatePayload(existing, { budget_strategy: "limit_spent_remaining" });
    expect(payload.budget_strategy).toBe("limit_spent_remaining");
    const unpatched = buildUpdatePayload(existing, {});
    expect(unpatched.budget_strategy).toBeUndefined();
  });
});

describe("isReadOnly", () => {
  it("is false when neither closed_at nor archived_at is set", () => {
    expect(isReadOnly({ closed_at: null, archived_at: null })).toBe(false);
  });

  it("is true when closed_at is set", () => {
    expect(isReadOnly({ closed_at: "2026-01-01T00:00:00Z", archived_at: null })).toBe(true);
  });

  it("is true when archived_at is set", () => {
    expect(isReadOnly({ closed_at: null, archived_at: "2026-01-01T00:00:00Z" })).toBe(true);
  });

  it("is true when both are set", () => {
    expect(
      isReadOnly({ closed_at: "2026-01-01T00:00:00Z", archived_at: "2026-01-02T00:00:00Z" }),
    ).toBe(true);
  });

  it("is false for a null/undefined budget", () => {
    expect(isReadOnly(null)).toBe(false);
    expect(isReadOnly(undefined)).toBe(false);
  });
});

describe("isRollupLinked", () => {
  it("is false when neither rollup_parent_id nor rollup_child_ids is set", () => {
    expect(isRollupLinked({ rollup_parent_id: null, rollup_child_ids: [] })).toBe(false);
  });

  it("is true when rollup_parent_id is set (this budget is a CHILD)", () => {
    expect(
      isRollupLinked({ rollup_parent_id: "11111111-1111-1111-1111-111111111111", rollup_child_ids: [] }),
    ).toBe(true);
  });

  it("is true when rollup_child_ids is non-empty (this budget is a PARENT)", () => {
    expect(
      isRollupLinked({
        rollup_parent_id: null,
        rollup_child_ids: ["22222222-2222-2222-2222-222222222222"],
      }),
    ).toBe(true);
  });

  it("is false when rollup_child_ids is present but empty", () => {
    expect(isRollupLinked({ rollup_parent_id: null, rollup_child_ids: [] })).toBe(false);
  });

  it("is false for a null/undefined budget", () => {
    expect(isRollupLinked(null)).toBe(false);
    expect(isRollupLinked(undefined)).toBe(false);
  });

  it("is false when rollup_child_ids is entirely absent from the object", () => {
    expect(isRollupLinked({ rollup_parent_id: null })).toBe(false);
  });
});
