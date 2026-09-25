import { describe, it, expect } from "vitest";
import {
  VIEW_STATE,
  viewStateFor,
  bandParts,
  dialPercent,
  dialBand,
  percentLabel,
  needleAngle,
  incomeSegments,
  depletionInfo,
  clamp,
  activeAssets,
  buildProjectionOverrides,
  buildProfileSaveBody,
  debounce,
  segmentLabelKey,
  assetTypeLabelKey,
  taxTreatmentLabelKey,
  DISCLOSURE_KEY,
  SLIDER_DISCLOSURE_KEY,
} from "./retirementView.js";

const BAND = { p10: 3400, p25: 4100, p50: 4800, p75: 5600, p90: 6800 };

describe("viewStateFor", () => {
  it("reports loading only via the caller — the selector has no loading input", () => {
    // Loading is a component concern; the selector starts from results.
    expect(viewStateFor({ profile: null, projection: null, assets: [], error: null }))
      .toBe(VIEW_STATE.noProfile);
  });

  it("maps a Pro-gate 402 to proRequired, ahead of everything else", () => {
    expect(viewStateFor({
      profile: { supported: true },
      projection: null,
      assets: [],
      error: { status: 402 },
    })).toBe(VIEW_STATE.proRequired);
  });

  it("maps a non-402 error to error, even when a profile and assets exist", () => {
    expect(viewStateFor({
      profile: { supported: true },
      projection: null,
      assets: [{}],
      error: { status: 500 },
    })).toBe(VIEW_STATE.error);
  });

  it("maps a null profile to noProfile", () => {
    expect(viewStateFor({
      profile: null, projection: null, assets: [], error: null,
    })).toBe(VIEW_STATE.noProfile);
  });

  it("maps a non-US profile to unsupported before assets are considered", () => {
    expect(viewStateFor({
      profile: { supported: false },
      projection: null,
      assets: [{ id: "a" }],
      error: null,
    })).toBe(VIEW_STATE.unsupported);
  });

  it("maps a supported profile with no assets to noAccounts — never a $0 projection", () => {
    expect(viewStateFor({
      profile: { supported: true },
      projection: { deterministic_monthly_income: 0 },
      assets: [],
      error: null,
    })).toBe(VIEW_STATE.noAccounts);
    expect(viewStateFor({
      profile: { supported: true },
      projection: { deterministic_monthly_income: 0 },
      assets: undefined,
      error: null,
    })).toBe(VIEW_STATE.noAccounts);
  });

  it("maps supported + assets + a projection to loaded", () => {
    expect(viewStateFor({
      profile: { supported: true },
      projection: { deterministic_monthly_income: 5000 },
      assets: [{ id: "a" }],
      error: null,
    })).toBe(VIEW_STATE.loaded);
  });

  it("maps supported + assets but a MISSING projection to error (broken response)", () => {
    expect(viewStateFor({
      profile: { supported: true },
      projection: null,
      assets: [{ id: "a" }],
      error: null,
    })).toBe(VIEW_STATE.error);
  });

  it("treats a 402-shaped error object as the gate even without a profile read", () => {
    expect(viewStateFor({
      profile: null, projection: null, assets: [], error: { status: 402 },
    })).toBe(VIEW_STATE.proRequired);
  });
});

describe("bandParts", () => {
  it("returns the two formatted band bounds", () => {
    const { low, high } = bandParts(BAND);
    expect(low).toMatch(/\$/);
    expect(high).toMatch(/\$/);
    expect(low).not.toBe(high);
    expect(high).toContain("6,800");
  });

  it("never returns a single figure — the headline is structurally a pair", () => {
    const parts = bandParts(BAND);
    expect(typeof parts).toBe("object");
    expect(parts).toHaveProperty("low");
    expect(parts).toHaveProperty("high");
  });

  it("returns null for a degenerate band so no undefined renders", () => {
    expect(bandParts(null)).toBeNull();
    expect(bandParts(undefined)).toBeNull();
    expect(bandParts({})).toBeNull();
    expect(bandParts({ p10: "3,400", p90: 6800 })).toBeNull();
    expect(bandParts({ p10: NaN, p90: 6800 })).toBeNull();
  });

  it("renders a flat band (all bounds equal) as a normal pair, not an error", () => {
    const parts = bandParts({ p10: 5000, p25: 5000, p50: 5000, p75: 5000, p90: 5000 });
    expect(parts.low).toBe(parts.high);
  });
});

describe("dialPercent / percentLabel", () => {
  it("clamps the arc fraction to 0..1", () => {
    expect(dialPercent(1.24)).toBe(1);
    expect(dialPercent(-0.5)).toBe(0);
    expect(dialPercent(0.75)).toBeCloseTo(0.75);
  });

  it("handles missing / NaN values as a zero arc", () => {
    expect(dialPercent(undefined)).toBe(0);
    expect(dialPercent(NaN)).toBe(0);
  });

  it("the LABEL is not clamped — an over-funded plan reads 124%", () => {
    expect(percentLabel(1.24)).toBe("124%");
    expect(percentLabel(0.62)).toBe("62%");
    expect(percentLabel(undefined)).toBe("—");
    expect(percentLabel(NaN)).toBe("—");
  });
});

describe("needleAngle", () => {
  it("rotates +90 at 0% so the needle points UP (shared origin with the band arc)", () => {
    // The regression this pins: the original `goalPct * 360 - 90` drew 0% at
    // the BOTTOM and 100% at the LEFT — mirrored from the band arc it shares
    // the dial with, which starts at the top (the SVG is -rotate-90).
    expect(needleAngle(0)).toBe(90);
    expect(needleAngle(0.25)).toBe(180);
    expect(needleAngle(0.5)).toBe(270);
    expect(needleAngle(0.75)).toBe(360);
    expect(needleAngle(1)).toBe(450);
  });
});

describe("activeAssets", () => {
  it("drops closed assets (the projection already excludes them, §25)", () => {
    expect(activeAssets([
      { id: "a", status: "active" },
      { id: "b", status: "closed" },
      { id: "c" },
    ]).map((a) => a.id)).toEqual(["a", "c"]);
  });

  it("treats non-arrays as an empty list", () => {
    expect(activeAssets(null)).toEqual([]);
    expect(activeAssets(undefined)).toEqual([]);
    expect(activeAssets("nope")).toEqual([]);
  });

  it("keeps future status values visible rather than silently emptying the list", () => {
    expect(activeAssets([{ id: "x", status: "archived" }]).map((a) => a.id)).toEqual(["x"]);
  });
});

describe("dialBand", () => {
  const PROJ = {
    percent_of_goal: 1.0,
    deterministic_monthly_income: 6000,
    percentile_band: { p10: 4000, p25: 5000, p50: 6000, p75: 6500, p90: 5500 },
  };

  it("converts the band to fractions of the target (derived from the engine's own numbers)", () => {
    const b = dialBand(PROJ);
    expect(b.lowPct).toBeCloseTo(4000 / 6000, 5);
    expect(b.highPct).toBeCloseTo(5500 / 6000, 5);
  });

  it("clamps the fractions to 0..1 for the arc", () => {
    const wide = {
      ...PROJ,
      percentile_band: { p10: -500, p90: 12000 },
    };
    const b = dialBand(wide);
    expect(b.lowPct).toBe(0);
    expect(b.highPct).toBe(1);
  });

  it("returns null when the goal is absent (zero/negative target income)", () => {
    expect(dialBand({ ...PROJ, percent_of_goal: null })).toBe(null);
    expect(dialBand({ ...PROJ, percent_of_goal: 0 })).toBe(null);
  });

  it("returns null for missing or NaN figures, never NaN fractions", () => {
    expect(dialBand(null)).toBe(null);
    expect(dialBand({})).toBe(null);
    expect(dialBand({ ...PROJ, percentile_band: undefined })).toBe(null);
    expect(dialBand({ ...PROJ, percentile_band: { p10: NaN, p90: 8000 } })).toBe(null);
    expect(dialBand({ ...PROJ, deterministic_monthly_income: NaN })).toBe(null);
  });
});

describe("incomeSegments", () => {
  const SOURCES = {
    own_savings: 2000,
    employer_contributions: 500,
    social_security: 1500,
    other_assets: 250,
    gap: 750,
  };

  it("returns all five segments with shares summing to 100", () => {
    const segs = incomeSegments(SOURCES, false);
    expect(segs).toHaveLength(5);
    expect(segs.map((s) => s.key)).toEqual([
      "ownSavings", "employer", "socialSecurity", "otherAssets", "gap",
    ]);
    expect(segs.reduce((sum, s) => sum + s.pct, 0)).toBeCloseTo(100, 5);
  });

  it("marks the SS segment absent and gives it ZERO width when the figure is missing", () => {
    const segs = incomeSegments(SOURCES, true);
    const ss = segs.find((s) => s.key === "socialSecurity");
    expect(ss.absent).toBe(true);
    expect(ss.pct).toBe(0);
    // The other four still total the visible bar.
    const visible = segs.filter((s) => !s.absent);
    expect(visible.reduce((sum, s) => sum + s.pct, 0)).toBeCloseTo(100, 5);
  });

  it("an entered zero SS figure is NOT absent — distinct from not entered (#466)", () => {
    const sources = { ...SOURCES, social_security: 0 };
    const segs = incomeSegments(sources, false);
    const ss = segs.find((s) => s.key === "socialSecurity");
    expect(ss.absent).toBe(false);
    expect(ss.pct).toBe(0);
  });

  it("a completely empty projection yields all-zero widths, never NaN", () => {
    const segs = incomeSegments({}, false);
    for (const s of segs) {
      expect(Number.isNaN(s.pct)).toBe(false);
      expect(s.pct).toBe(0);
    }
  });

  it("tolerates null/undefined sources", () => {
    for (const bad of [null, undefined]) {
      const segs = incomeSegments(bad, false);
      expect(segs).toHaveLength(5);
      expect(segs.every((s) => s.pct === 0)).toBe(true);
    }
  });
});

describe("depletionInfo", () => {
  it("reports a depletion age when depletes is true", () => {
    expect(depletionInfo(true, 78)).toEqual({ depletes: true, age: 78 });
  });

  it("reports no depletion when depletes is false, whatever the age says", () => {
    expect(depletionInfo(false, null)).toEqual({ depletes: false, age: null });
    // The engine's contract: a non-depleting run has no age. Defensive: if a
    // caller passes both, the false verdict wins.
    expect(depletionInfo(false, 95)).toEqual({ depletes: false, age: null });
  });

  it("treats a missing depletes as not depleting", () => {
    expect(depletionInfo(undefined, 80)).toEqual({ depletes: false, age: null });
  });
});

describe("clamp", () => {
  it("clamps into range", () => {
    expect(clamp(12, 0, 100)).toBe(12);
    expect(clamp(-5, 0, 100)).toBe(0);
    expect(clamp(150, 0, 100)).toBe(100);
  });

  it("parses numeric strings (slider input values)", () => {
    expect(clamp("8", 0, 100)).toBe(8);
  });

  it("falls back to min for NaN, blank, and non-numeric input", () => {
    expect(clamp(NaN, 0, 100)).toBe(0);
    expect(clamp(undefined, 0, 100)).toBe(0);
    expect(clamp("", 0, 100)).toBe(0);
    expect(clamp("abc", 5, 10)).toBe(5);
  });
});

describe("buildProjectionOverrides", () => {
  it("maps control state to the server's ProjectionOverrides field names", () => {
    expect(buildProjectionOverrides({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 0,
    })).toEqual({
      contribution_rate_pre_tax: 6,
      contribution_rate_roth: 3,
      expected_real_return: 5,
      target_retirement_age: 65,
      current_gross_income: 100000,
      target_replacement_ratio: 0.75,
      effective_tax_rate: 0.15,
    });
  });

  it("sends every field — the what-if is full-replace on the override body", () => {
    const body = buildProjectionOverrides({
      preTax: 6, roth: 0, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 0,
    });
    expect(Object.keys(body)).toHaveLength(7);
  });

  it("sends ss_claiming_age_months only for a real 62-70 claiming age", () => {
    const withAge = buildProjectionOverrides({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 70,
    });
    expect(withAge.ss_claiming_age_months).toBe(840);
    // 0 = no SS entered -> the field is OMITTED, so a no-SS profile never
    // sends a meaningless claiming age the server would have to ignore.
    expect(buildProjectionOverrides({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 0,
    })).not.toHaveProperty("ss_claiming_age_months");
    // 61/71 are outside the statutory range — refuse to send them at all.
    expect(buildProjectionOverrides({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 61,
    })).not.toHaveProperty("ss_claiming_age_months");
    expect(buildProjectionOverrides({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 71,
    })).not.toHaveProperty("ss_claiming_age_months");
  });
});

describe("buildProfileSaveBody", () => {
  it("excludes effective_tax_rate — the profile has no tax-rate column", () => {
    const body = buildProfileSaveBody({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 0,
    });
    expect(body).not.toHaveProperty("effective_tax_rate");
    expect(Object.keys(body)).toEqual([
      "contribution_rate_pre_tax",
      "contribution_rate_roth",
      "expected_real_return",
      "target_retirement_age",
      "current_gross_income",
      "target_replacement_ratio",
    ]);
  });

  it("also excludes ss_claiming_age_months — the claiming age is a live-only what-if", () => {
    const body = buildProfileSaveBody({
      preTax: 6, roth: 3, realReturn: 5, retirementAge: 65,
      grossIncome: 100000, replacementRatio: 0.75, taxRate: 0.15,
      ssClaimingAge: 70,
    });
    expect(body).not.toHaveProperty("ss_claiming_age_months");
    // And the persisted set is unaffected by a live SS what-if.
    expect(body).not.toHaveProperty("ss_claiming_age");
  });
});

describe("debounce", () => {
  it("fires once with the latest arguments after the delay", async () => {
    const calls = [];
    const fn = debounce((...args) => calls.push(args), 20);
    fn(1);
    fn(2);
    fn(3);
    await new Promise((r) => setTimeout(r, 60));
    expect(calls).toEqual([[3]]);
  });

  it("does not fire if the wait never elapses between calls", async () => {
    const calls = [];
    const fn = debounce((...args) => calls.push(args), 30);
    fn(1);
    await new Promise((r) => setTimeout(r, 10));
    fn(2);
    await new Promise((r) => setTimeout(r, 10));
    fn(3);
    expect(calls).toEqual([]);
  });
});

describe("compliance pins", () => {
  it("the disclosure keys are referenced by constant, not free strings", () => {
    expect(DISCLOSURE_KEY).toBe("retirement.disclosure");
    expect(SLIDER_DISCLOSURE_KEY).toBe("retirement.sliderDisclosure");
  });
});

describe("label-key helpers", () => {
  it("maps every income segment to its i18n key", () => {
    expect(segmentLabelKey("ownSavings")).toBe("retirement.sourceOwnSavings");
    expect(segmentLabelKey("employer")).toBe("retirement.sourceEmployer");
    expect(segmentLabelKey("socialSecurity")).toBe("retirement.sourceSocialSecurity");
    expect(segmentLabelKey("otherAssets")).toBe("retirement.sourceOtherAssets");
    expect(segmentLabelKey("gap")).toBe("retirement.sourceGap");
  });

  it("falls back to the other-assets label for an unknown segment", () => {
    expect(segmentLabelKey("nonsense")).toBe("retirement.sourceOtherAssets");
  });

  it("maps every asset_type CHECK literal to its label key", () => {
    expect(assetTypeLabelKey("retirement_account")).toBe("retirement.assetTypeRetirementAccount");
    expect(assetTypeLabelKey("brokerage")).toBe("retirement.assetTypeBrokerage");
    expect(assetTypeLabelKey("cash")).toBe("retirement.assetTypeCash");
    expect(assetTypeLabelKey("pension")).toBe("retirement.assetTypePension");
    expect(assetTypeLabelKey("annuity")).toBe("retirement.assetTypeAnnuity");
    expect(assetTypeLabelKey("real_estate")).toBe("retirement.assetTypeRealEstate");
  });

  it("maps every tax_treatment CHECK literal to its label key", () => {
    expect(taxTreatmentLabelKey("pre_tax")).toBe("retirement.taxPreTax");
    expect(taxTreatmentLabelKey("roth")).toBe("retirement.taxRoth");
    expect(taxTreatmentLabelKey("taxable")).toBe("retirement.taxTaxable");
    expect(taxTreatmentLabelKey("hsa")).toBe("retirement.taxHsa");
    expect(taxTreatmentLabelKey("other")).toBe("retirement.taxOther");
  });

  it("an unknown taxonomy value falls back to Other, never a raw key", () => {
    expect(assetTypeLabelKey("401k")).toBe("retirement.assetTypeOther");
    expect(taxTreatmentLabelKey("super")).toBe("retirement.taxOther");
  });
});
