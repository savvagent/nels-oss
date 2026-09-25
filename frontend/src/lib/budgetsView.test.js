import { describe, it, expect } from "vitest";
import {
  UNKNOWN_ROLLUP_LABEL,
  canSwitchTo,
  resolveRollupNames,
  ownershipLabel,
  isActivationKey,
  switchErrorKey,
} from "./budgetsView.js";

describe("canSwitchTo", () => {
  const active = { id: "active-id" };

  it("returns true for an owned budget that is not the active one", () => {
    const budget = { id: "other-id", is_owner: true };
    expect(canSwitchTo(budget, active)).toBe(true);
  });

  it("returns false for an owned budget that IS the active one", () => {
    const budget = { id: "active-id", is_owner: true };
    expect(canSwitchTo(budget, active)).toBe(false);
  });

  it("returns true for a shared (non-owned) budget that is not active", () => {
    const budget = { id: "other-id", is_owner: false };
    expect(canSwitchTo(budget, active)).toBe(true);
  });

  it("returns false for a shared budget that IS the active one", () => {
    const budget = { id: "active-id", is_owner: false };
    expect(canSwitchTo(budget, active)).toBe(false);
  });

  it("returns true for a shared budget when there is no active budget yet", () => {
    const budget = { id: "other-id", is_owner: false };
    expect(canSwitchTo(budget, null)).toBe(true);
  });

  it("returns false for a null/undefined budget", () => {
    expect(canSwitchTo(null, active)).toBe(false);
    expect(canSwitchTo(undefined, active)).toBe(false);
  });

  it("returns true when activeBudget is null/undefined and the candidate is owned", () => {
    const budget = { id: "other-id", is_owner: true };
    expect(canSwitchTo(budget, null)).toBe(true);
    expect(canSwitchTo(budget, undefined)).toBe(true);
  });

  // Regression lock-in: canSwitchTo deliberately does NOT gate on archived_at/closed_at —
  // the backend's set_active_budget has no such restriction beyond `check_permission != None`
  // (#255/#391), and archiving is documented as explicitly not read-only (AGENTS.md
  // #50). This is a considered decision, not an oversight (see the comment above
  // canSwitchTo's definition) — this test exists so a future "fix" based on intuition alone
  // would fail here first.
  it("returns true for an owned, archived, non-active budget (no archived/closed special-case)", () => {
    const budget = { id: "other-id", is_owner: true, archived_at: "2026-01-01T00:00:00Z" };
    expect(canSwitchTo(budget, active)).toBe(true);
  });

  it("returns true for an owned, closed, non-active budget (no archived/closed special-case)", () => {
    const budget = { id: "other-id", is_owner: true, closed_at: "2026-01-01T00:00:00Z" };
    expect(canSwitchTo(budget, active)).toBe(true);
  });
});

describe("resolveRollupNames", () => {
  it("returns null parentName and empty childNames for a standalone budget", () => {
    const budget = { rollup_parent_id: null, rollup_child_ids: [] };
    expect(resolveRollupNames(budget, [])).toEqual({ parentName: null, childNames: [] });
  });

  it("resolves the parent name when the parent id is present in allBudgets", () => {
    const budget = { rollup_parent_id: "parent-id", rollup_child_ids: [] };
    const allBudgets = [{ id: "parent-id", name: "Household" }];
    expect(resolveRollupNames(budget, allBudgets)).toEqual({
      parentName: "Household",
      childNames: [],
    });
  });

  it("resolves the parent name to UNKNOWN_ROLLUP_LABEL when not present in allBudgets", () => {
    const budget = { rollup_parent_id: "missing-id", rollup_child_ids: [] };
    expect(resolveRollupNames(budget, [])).toEqual({
      parentName: UNKNOWN_ROLLUP_LABEL,
      childNames: [],
    });
  });

  it("resolves a mix of present and missing child ids, preserving length and order", () => {
    const budget = { rollup_parent_id: null, rollup_child_ids: ["a", "b", "c"] };
    const allBudgets = [
      { id: "a", name: "Vacation" },
      { id: "c", name: "Car" },
    ];
    expect(resolveRollupNames(budget, allBudgets)).toEqual({
      parentName: null,
      childNames: ["Vacation", UNKNOWN_ROLLUP_LABEL, "Car"],
    });
  });

  it("resolves all children to UNKNOWN_ROLLUP_LABEL when allBudgets is undefined/empty", () => {
    const budget = { rollup_parent_id: null, rollup_child_ids: ["a", "b"] };
    expect(resolveRollupNames(budget, undefined)).toEqual({
      parentName: null,
      childNames: [UNKNOWN_ROLLUP_LABEL, UNKNOWN_ROLLUP_LABEL],
    });
    expect(resolveRollupNames(budget, [])).toEqual({
      parentName: null,
      childNames: [UNKNOWN_ROLLUP_LABEL, UNKNOWN_ROLLUP_LABEL],
    });
  });
});

describe("ownershipLabel", () => {
  it("returns { kind: 'owned' } when is_owner is true, regardless of owner_name", () => {
    expect(ownershipLabel({ is_owner: true })).toEqual({ kind: "owned" });
    expect(ownershipLabel({ is_owner: true, owner_name: "Alice" })).toEqual({ kind: "owned" });
  });

  it("returns { kind: 'shared', ownerName } when is_owner is false and owner_name is set", () => {
    expect(ownershipLabel({ is_owner: false, owner_name: "Alice" })).toEqual({
      kind: "shared",
      ownerName: "Alice",
    });
  });

  it("returns { kind: 'shared', ownerName: null } when owner_name is null", () => {
    expect(ownershipLabel({ is_owner: false, owner_name: null })).toEqual({
      kind: "shared",
      ownerName: null,
    });
  });

  it("returns { kind: 'shared', ownerName: null } when owner_name is absent entirely", () => {
    expect(ownershipLabel({ is_owner: false })).toEqual({ kind: "shared", ownerName: null });
  });
});

describe("isActivationKey", () => {
  it("returns true for Enter", () => {
    expect(isActivationKey({ key: "Enter" })).toBe(true);
  });
  it("returns true for Space (' ')", () => {
    expect(isActivationKey({ key: " " })).toBe(true);
  });
  it("returns false for other keys", () => {
    expect(isActivationKey({ key: "a" })).toBe(false);
    expect(isActivationKey({ key: "Tab" })).toBe(false);
  });
  it("returns false for a nullish event", () => {
    expect(isActivationKey(null)).toBe(false);
    expect(isActivationKey(undefined)).toBe(false);
  });
});

describe("switchErrorKey", () => {
  it("maps 403 to the access-revoked message", () => {
    expect(switchErrorKey(403)).toBe("budgetsList.switchErrorRevoked");
  });

  it("maps 404 to the budget-deleted message", () => {
    expect(switchErrorKey(404)).toBe("budgetsList.switchErrorDeleted");
  });

  it("returns null for other statuses so the caller keeps its generic copy", () => {
    expect(switchErrorKey(500)).toBe(null);
    expect(switchErrorKey(undefined)).toBe(null);
    expect(switchErrorKey(null)).toBe(null);
  });
});
