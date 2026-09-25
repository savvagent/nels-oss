import { describe, it, expect } from "vitest";
import {
  hasEntitledPlan,
  planLabelKey,
  needsPaymentUpdate,
} from "./subscriptionDisplay.js";

describe("hasEntitledPlan", () => {
  it("is true for basic and pro", () => {
    expect(hasEntitledPlan({ tier: "basic" })).toBe(true);
    expect(hasEntitledPlan({ tier: "pro" })).toBe(true);
  });

  it("is true for a past_due Pro grace subscription (the old is_pro gate hid this)", () => {
    expect(
      hasEntitledPlan({
        tier: "pro",
        status: "past_due",
        payment_warning: true,
        is_pro: false,
      }),
    ).toBe(true);
  });

  it("is false for none / empty / null", () => {
    expect(hasEntitledPlan({ tier: "none" })).toBe(false);
    expect(hasEntitledPlan({})).toBe(false);
    expect(hasEntitledPlan(null)).toBe(false);
  });
});

describe("planLabelKey", () => {
  it("labels a trial as the Pro trial regardless of tier", () => {
    expect(planLabelKey({ status: "trialing", tier: "pro" })).toBe(
      "settings.planProTrial",
    );
  });

  it("labels by resolved tier", () => {
    expect(planLabelKey({ status: "active", tier: "pro" })).toBe(
      "settings.planPro",
    );
    expect(planLabelKey({ status: "active", tier: "basic" })).toBe(
      "settings.planBasic",
    );
  });

  it("falls back to the free label when not entitled", () => {
    expect(planLabelKey({ tier: "none" })).toBe("settings.planFree");
    expect(planLabelKey(null)).toBe("settings.planFree");
  });
});

describe("needsPaymentUpdate", () => {
  it("is true only when payment_warning is set", () => {
    expect(needsPaymentUpdate({ payment_warning: true })).toBe(true);
    expect(needsPaymentUpdate({ payment_warning: false })).toBe(false);
    expect(needsPaymentUpdate({})).toBe(false);
    expect(needsPaymentUpdate(null)).toBe(false);
  });
});
