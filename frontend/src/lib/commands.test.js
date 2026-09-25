import { describe, it, expect } from "vitest";
import { canSetAsDefault, resolveActiveBudget } from "./commands.js";

// #238: a budget merely shared with the user (View/Edit, not Owner) must never
// be treated as a valid /budgets-switch target — the backend enforces this too
// (POST /budgets/:id/default now requires Permission::Owner), but the frontend
// should short-circuit with a clear message instead of hitting a 403.
describe("canSetAsDefault", () => {
  it("allows an owned budget", () => {
    expect(canSetAsDefault({ is_owner: true })).toBe(true);
  });

  it("rejects a budget shared with the user", () => {
    expect(canSetAsDefault({ is_owner: false })).toBe(false);
  });

  it("fails closed when is_owner is missing", () => {
    expect(canSetAsDefault({})).toBe(false);
  });

  it("fails closed for a null or undefined budget", () => {
    expect(canSetAsDefault(null)).toBe(false);
    expect(canSetAsDefault(undefined)).toBe(false);
  });
});

describe("resolveActiveBudget", () => {
  it("returns the viewer's own default budget, ignoring a shared budget's is_default flag", () => {
    // Mirrors the backend's `is_default DESC, name ASC` sort: a shared budget named
    // "Alpha" whose OWNER has it flagged default can sort ahead of the viewer's own
    // default budget "Beta" — this is the exact regression from #266.
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "owned-1", name: "Beta", is_owner: true, is_default: true },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-1");
  });

  it("falls back to any owned budget when no owned row is flagged default", () => {
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "owned-1", name: "Beta", is_owner: true, is_default: false },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-1");
  });

  it("selects the default among multiple owned budgets regardless of array position", () => {
    const budgets = [
      { id: "owned-1", name: "Alpha", is_owner: true, is_default: false },
      { id: "owned-2", name: "Beta", is_owner: true, is_default: true },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-2");
  });

  it("falls back to the first owned budget in array order when none is flagged default", () => {
    const budgets = [
      { id: "owned-1", name: "Zulu", is_owner: true, is_default: false },
      { id: "owned-2", name: "Alpha", is_owner: true, is_default: false },
    ];
    expect(resolveActiveBudget(budgets)?.id).toBe("owned-1");
  });

  it("returns null when the viewer owns no budgets", () => {
    const budgets = [
      { id: "shared-1", name: "Alpha", is_owner: false, is_default: true },
      { id: "shared-2", name: "Gamma", is_owner: false, is_default: false },
    ];
    expect(resolveActiveBudget(budgets)).toBeNull();
  });

  it("returns null for an empty list", () => {
    expect(resolveActiveBudget([])).toBeNull();
  });

  it("returns null for null/undefined input", () => {
    expect(resolveActiveBudget(null)).toBeNull();
    expect(resolveActiveBudget(undefined)).toBeNull();
  });
});
