import { describe, it, expect } from "vitest";
import {
  ROUTES,
  routeFromHash,
  hashForRoute,
  parseBudgetsPath,
  hashForBudgetDetails,
  hashForBudgetsList,
  resolveRoute,
  applyFlaggedRoutes,
} from "./router.js";

describe("ROUTES", () => {
  it("lists the nine flat routes", () => {
    expect(ROUTES).toEqual([
      "chat",
      "categories",
      "transactions",
      "insights",
      "history",
      "accounts",
      "retirement",
      "settings",
      "deleteAccount",
    ]);
  });
});

describe("routeFromHash", () => {
  it("maps #/categories to categories", () => {
    expect(routeFromHash("#/categories")).toBe("categories");
  });

  it("maps #/insights to insights", () => {
    expect(routeFromHash("#/insights")).toBe("insights");
  });

  it("falls back to chat for an empty hash", () => {
    expect(routeFromHash("")).toBe("chat");
  });

  it("falls back to chat for a bare #", () => {
    expect(routeFromHash("#")).toBe("chat");
  });

  it("falls back to chat for #/", () => {
    expect(routeFromHash("#/")).toBe("chat");
  });

  it("maps #/history to history", () => {
    expect(routeFromHash("#/history")).toBe("history");
  });

  it("maps #/accounts to accounts", () => {
    expect(routeFromHash("#/accounts")).toBe("accounts");
  });

  it("maps #/retirement to retirement", () => {
    expect(routeFromHash("#/retirement")).toBe("retirement");
  });

  it("maps #/settings to settings", () => {
    expect(routeFromHash("#/settings")).toBe("settings");
  });

  it("maps #/deleteAccount to deleteAccount", () => {
    expect(routeFromHash("#/deleteAccount")).toBe("deleteAccount");
  });

  it("falls back to chat for an unrecognized route", () => {
    expect(routeFromHash("#/nope")).toBe("chat");
  });

  it("falls back to chat for garbage/unrelated hashes", () => {
    expect(routeFromHash("#foo=bar")).toBe("chat");
  });

  it("falls back to chat for null/undefined", () => {
    expect(routeFromHash(null)).toBe("chat");
    expect(routeFromHash(undefined)).toBe("chat");
  });

  it("is case-sensitive (unrecognized case falls back to chat)", () => {
    expect(routeFromHash("#/Categories")).toBe("chat");
  });

  it("falls back to chat for a bare route name with no # prefix", () => {
    expect(routeFromHash("categories")).toBe("chat");
    expect(routeFromHash("insights")).toBe("chat");
  });

  it("falls back to chat for an explicit #/chat", () => {
    expect(routeFromHash("#/chat")).toBe("chat");
  });

  it("falls back to chat for a trailing slash", () => {
    expect(routeFromHash("#/categories/")).toBe("chat");
  });

  it("falls back to chat for a doubled slash", () => {
    expect(routeFromHash("#//categories")).toBe("chat");
  });

  it("falls back to chat for a hash with a query-like suffix", () => {
    expect(routeFromHash("#/categories?x=1")).toBe("chat");
  });
});

describe("hashForRoute", () => {
  it("returns an empty string for chat (no hash)", () => {
    expect(hashForRoute("chat")).toBe("");
  });

  it("returns #/categories for categories", () => {
    expect(hashForRoute("categories")).toBe("#/categories");
  });

  it("returns #/insights for insights", () => {
    expect(hashForRoute("insights")).toBe("#/insights");
  });

  it("returns #/history for history", () => {
    expect(hashForRoute("history")).toBe("#/history");
  });

  it("returns #/retirement for retirement", () => {
    expect(hashForRoute("retirement")).toBe("#/retirement");
  });

  it("returns #/settings for settings", () => {
    expect(hashForRoute("settings")).toBe("#/settings");
  });

  it("returns #/deleteAccount for deleteAccount", () => {
    expect(hashForRoute("deleteAccount")).toBe("#/deleteAccount");
  });

  it("falls back to empty string for an unknown route", () => {
    expect(hashForRoute("nope")).toBe("");
  });

  it("round-trips with routeFromHash for every route", () => {
    for (const route of ROUTES) {
      expect(routeFromHash(hashForRoute(route))).toBe(route);
    }
  });

  it("falls back to empty string for null/undefined/non-string input", () => {
    expect(hashForRoute(null)).toBe("");
    expect(hashForRoute(undefined)).toBe("");
    expect(hashForRoute(42)).toBe("");
  });
});

describe("parseBudgetsPath", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("parses a lowercase uuid segment as a dynamic id", () => {
    expect(parseBudgetsPath(`#/budgets/${UUID}`)).toEqual({ kind: "id", id: UUID });
  });

  it("parses an uppercase uuid segment as a dynamic id (case-insensitive)", () => {
    const upper = UUID.toUpperCase();
    expect(parseBudgetsPath(`#/budgets/${upper}`)).toEqual({ kind: "id", id: upper });
  });

  it("returns null for a non-uuid, non-registered-token segment", () => {
    expect(parseBudgetsPath("#/budgets/nope")).toBe(null);
  });

  it("parses the 'list' token as the static budgets-list kind", () => {
    expect(parseBudgetsPath("#/budgets/list")).toEqual({ kind: "list" });
  });

  it("returns null for a missing segment", () => {
    expect(parseBudgetsPath("#/budgets/")).toBe(null);
    expect(parseBudgetsPath("#/budgets")).toBe(null);
  });

  it("returns null for a segment with a nested extra path part", () => {
    expect(parseBudgetsPath(`#/budgets/${UUID}/extra`)).toBe(null);
  });

  it("returns null for a hash with no budgets/ prefix", () => {
    expect(parseBudgetsPath("#/categories")).toBe(null);
    expect(parseBudgetsPath("#/insights")).toBe(null);
    expect(parseBudgetsPath("")).toBe(null);
  });

  it("returns null for non-string/null/undefined input", () => {
    expect(parseBudgetsPath(null)).toBe(null);
    expect(parseBudgetsPath(undefined)).toBe(null);
    expect(parseBudgetsPath(42)).toBe(null);
  });

  it("returns null for a hash lacking the # prefix", () => {
    expect(parseBudgetsPath(`budgets/${UUID}`)).toBe(null);
  });
});

describe("hashForBudgetDetails", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("returns #/budgets/<id> for a given id", () => {
    expect(hashForBudgetDetails(UUID)).toBe(`#/budgets/${UUID}`);
  });

  it("returns an empty string for a falsy id", () => {
    expect(hashForBudgetDetails(null)).toBe("");
    expect(hashForBudgetDetails(undefined)).toBe("");
    expect(hashForBudgetDetails("")).toBe("");
  });

  it("round-trips with parseBudgetsPath", () => {
    expect(parseBudgetsPath(hashForBudgetDetails(UUID))).toEqual({ kind: "id", id: UUID });
  });
});

describe("hashForBudgetsList", () => {
  it("returns exactly #/budgets/list", () => {
    expect(hashForBudgetsList()).toBe("#/budgets/list");
  });

  it("round-trips with parseBudgetsPath", () => {
    expect(parseBudgetsPath(hashForBudgetsList())).toEqual({ kind: "list" });
  });
});

describe("resolveRoute", () => {
  const UUID = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

  it("resolves a budgets/<uuid> hash to budgetDetails + the id", () => {
    expect(resolveRoute(`#/budgets/${UUID}`)).toEqual({
      route: "budgetDetails",
      budgetId: UUID,
    });
  });

  it("resolves every existing flat route with a null budgetId", () => {
    for (const route of ROUTES) {
      expect(resolveRoute(hashForRoute(route))).toEqual({ route, budgetId: null });
    }
  });

  it("falls back to chat with a null budgetId for garbage input", () => {
    expect(resolveRoute("#/nope")).toEqual({ route: "chat", budgetId: null });
    expect(resolveRoute(null)).toEqual({ route: "chat", budgetId: null });
  });

  it("resolves a budgets/list hash to budgetsList with a null budgetId", () => {
    expect(resolveRoute("#/budgets/list")).toEqual({ route: "budgetsList", budgetId: null });
  });
});

describe("applyFlaggedRoutes (#469 retirement planner flag)", () => {
  it("passes a retirement route through when the flag is on", () => {
    expect(applyFlaggedRoutes({ route: "retirement", budgetId: null }, { retirementPlannerEnabled: true }))
      .toEqual({ route: "retirement", budgetId: null });
  });

  it("collapses a retirement route to chat when the flag is off", () => {
    expect(applyFlaggedRoutes({ route: "retirement", budgetId: null }, { retirementPlannerEnabled: false }))
      .toEqual({ route: "chat", budgetId: null });
  });

  it("treats a missing flag (e.g. before /auth/me resolves) as off", () => {
    expect(applyFlaggedRoutes({ route: "retirement", budgetId: null }, {}))
      .toEqual({ route: "chat", budgetId: null });
  });

  it("leaves every other route untouched regardless of the flag", () => {
    for (const route of ROUTES.filter((r) => r !== "retirement")) {
      const resolved = { route, budgetId: null };
      expect(applyFlaggedRoutes(resolved, { retirementPlannerEnabled: true })).toBe(resolved);
      expect(applyFlaggedRoutes(resolved, { retirementPlannerEnabled: false })).toBe(resolved);
    }
  });

  it("preserves a budget id on non-retirement routes", () => {
    const budgetDetails = { route: "budgetDetails", budgetId: "3fa85f64-5717-4562-b3fc-2c963f66afa6" };
    expect(applyFlaggedRoutes(budgetDetails, { retirementPlannerEnabled: false })).toBe(budgetDetails);
  });
});
