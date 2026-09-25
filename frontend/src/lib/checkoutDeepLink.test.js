import { describe, it, expect } from "vitest";
import { parseCheckoutDeepLink } from "./checkoutDeepLink.js";

describe("parseCheckoutDeepLink", () => {
  it("reads plan + cadence for the two-tier link", () => {
    expect(parseCheckoutDeepLink("?plan=basic&cadence=annual")).toEqual({
      tier: "basic",
      cadence: "annual",
    });
    expect(parseCheckoutDeepLink("?plan=pro&cadence=monthly")).toEqual({
      tier: "pro",
      cadence: "monthly",
    });
  });

  it("defaults cadence to monthly when plan is given without a valid cadence", () => {
    expect(parseCheckoutDeepLink("?plan=pro")).toEqual({ tier: "pro", cadence: "monthly" });
    expect(parseCheckoutDeepLink("?plan=basic&cadence=weekly")).toEqual({
      tier: "basic",
      cadence: "monthly",
    });
  });

  it("supports the legacy cadence-only upgrade link (tier null → backend defaults Pro)", () => {
    expect(parseCheckoutDeepLink("?upgrade=monthly")).toEqual({ tier: null, cadence: "monthly" });
    expect(parseCheckoutDeepLink("?upgrade=annual")).toEqual({ tier: null, cadence: "annual" });
  });

  it("prefers plan over a legacy upgrade param when both are present", () => {
    expect(parseCheckoutDeepLink("?plan=basic&cadence=annual&upgrade=monthly")).toEqual({
      tier: "basic",
      cadence: "annual",
    });
  });

  it("returns null when there is no checkout intent", () => {
    expect(parseCheckoutDeepLink("")).toBeNull();
    expect(parseCheckoutDeepLink("?billing=success")).toBeNull();
    expect(parseCheckoutDeepLink("?plan=enterprise")).toBeNull();
    expect(parseCheckoutDeepLink("?upgrade=weekly")).toBeNull();
  });
});
