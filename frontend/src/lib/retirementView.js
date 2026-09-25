// Pure helpers for the Retirement planner view (nels#469). No Svelte, no DOM,
// no fetch — the only part of the page under unit test, matching
// categoriesView.js / transactionsView.js (vitest runs `environment: "node"`
// in this repo). The component (RetirementView.svelte) composes these and
// nothing else does; the four #454 compliance constraints are load-bearing in
// the SHAPE of these helpers, not just in copy:
//
//  1. The headline is a RANGE (10th-90th percentile band) with the
//     deterministic value secondary. `bandParts` is the only producer of the
//     headline figures; it returns two values, so a "bare point" headline
//     cannot be expressed through it.
//  2. Assumptions render on the same screen as the number — the component
//     draws them from the echoed `assumptions` struct, never re-derived.
//  3. The fixed hypothetical-illustration disclosure is a CONSTANT string here
//     (referenced by key, translated per locale) that the component renders
//     directly under the band.
//  4. Nothing here ever emits a recommendation. The only numbers computed are
//     arithmetic (percent-of-goal, segment shares, depletion phrasing).

import { fmtMoney } from "./money.js";
import { isProGateError } from "./linkedAccounts.js";

// ---------------------------------------------------------------------------
// Page state machine
// ---------------------------------------------------------------------------

/** The one-of states the view can be in. */
export const VIEW_STATE = {
  loading: "loading",
  error: "error",
  proRequired: "proRequired",
  noProfile: "noProfile",
  unsupported: "unsupported",
  noAccounts: "noAccounts",
  loaded: "loaded",
};

/**
 * Choose the page state from what the three user-scoped loads produced.
 *
 * ORDER IS LOAD-BEARING and each branch is tested on both sides:
 *   1. A load `error` wins over everything — except a Pro-gate 402, which is
 *      `proRequired` (the profile GET is ungated, so a 402 can only come from
 *      the gated projection/assets reads; a lapsed subscriber sees the upgrade
 *      CTA, never a bare error and never a $0 projection).
 *   2. `profile === null` -> `noProfile`. No country is known yet, so the
 *      US-only gate cannot be decided; the chat `SET_RETIREMENT_PROFILE` path
 *      is where a profile is created.
 *   3. `!profile.supported` -> `unsupported` (US-only). Full-page state, no
 *      dial, no empty bar, no zeros (#454 decision 2 / epic AC 5).
 *   4. `assets` empty -> `noAccounts`. NEVER a projection built from $0 —
 *      the connect-your-accounts empty state comes before any loaded view.
 *   5. `projection` present -> `loaded`; absent -> `error` (a successful
 *      projection request that returned no projection is a broken response).
 *
 * @param {{profile: object|null, projection: object|null, assets: object[],
 *          error: object|null}} load
 * @returns {string} one of `VIEW_STATE`
 */
export function viewStateFor({ profile, projection, assets, error }) {
  if (error) {
    return isProGateError(error) ? VIEW_STATE.proRequired : VIEW_STATE.error;
  }
  if (profile == null) return VIEW_STATE.noProfile;
  if (profile.supported === false) return VIEW_STATE.unsupported;
  if (!Array.isArray(assets) || assets.length === 0) return VIEW_STATE.noAccounts;
  if (projection == null) return VIEW_STATE.error;
  return VIEW_STATE.loaded;
}

// ---------------------------------------------------------------------------
// Headline band (compliance constraint 1)
// ---------------------------------------------------------------------------

/**
 * The two headline figures, formatted, from the 10th-90th percentile band.
 *
 * Returns a PAIR (`{ low, high }`), never a single figure. A bare "$6,200/mo"
 * is exactly what makes an illustration read as a promise, so the only
 * headline this module can produce is a range. The deterministic/median value
 * is rendered separately and secondarily by the component.
 *
 * Returns `null` for a band that is not the expected shape (missing or
 * non-numeric bounds) so the caller can degrade to an error state rather than
 * render "undefined".
 *
 * @param {{p10?: number, p90?: number}|null|undefined} band
 * @returns {{low: string, high: string}|null}
 */
export function bandParts(band) {
  const low = band?.p10;
  const high = band?.p90;
  if (typeof low !== "number" || typeof high !== "number") return null;
  // `typeof NaN === "number"`, so the type check above does not catch it. The
  // engine never emits NaN, but a NaN that reaches the headline renders as
  // "$NaN" — which a "range" headline must never show.
  if (Number.isNaN(low) || Number.isNaN(high)) return null;
  return { low: fmtMoney(low), high: fmtMoney(high) };
}

// ---------------------------------------------------------------------------
// Percent-of-goal dial
// ---------------------------------------------------------------------------

/**
 * The band's span as a fraction of the TARGET, for drawing the tinted band arc
 * on the dial (the dial itself marks `percent_of_goal`).
 *
 * The band figures (`p10`/`p90`) are absolute monthly income, while the dial's
 * unit is "fraction of goal", so this helper converts one to the other using
 * the projection's OWN numbers only: `target = deterministic / percent_of_goal`
 * — no independent re-derivation of the target from profile fields, so the
 * dial cannot drift from the engine's `percent_of_goal`.
 *
 * Returns clamped `lowPct`/`highPct` (0..1) for the arc, or `null` when the
 * conversion is undefined (missing/NaN numbers, or `percent_of_goal` absent
 * because the target income is zero or negative — a dial with no goal has no
 * band either).
 *
 * @param {{percent_of_goal?: number, deterministic_monthly_income?: number,
 *          percentile_band?: {p10?: number, p90?: number}}|null|undefined} projection
 * @returns {{lowPct: number, highPct: number}|null}
 */
export function dialBand(projection) {
  const goal = projection?.percent_of_goal;
  const det = projection?.deterministic_monthly_income;
  const band = projection?.percentile_band;
  if (typeof goal !== "number" || typeof det !== "number" || !band) return null;
  const low = band.p10;
  const high = band.p90;
  if (typeof low !== "number" || typeof high !== "number") return null;
  if (Number.isNaN(low) || Number.isNaN(high) || Number.isNaN(goal) || Number.isNaN(det)) {
    return null;
  }
  if (det <= 0 || goal <= 0) return null;
  const target = det / goal;
  return {
    lowPct: Math.max(0, Math.min(1, low / target)),
    highPct: Math.max(0, Math.min(1, high / target)),
  };
}

/**
 * Fraction of the dial arc to fill, clamped to 0..1 for rendering.
 *
 * `percent_of_goal` is a FRACTION the engine may report above 1.0 (an
 * over-funded plan sustains more than its target — the dial can exceed 100%).
 * The arc is clamped; the LABEL is not, so a 124% plan renders a full ring and
 * a "124%" caption. The dial only ever renders inside the band block (#454
 * constraint 1 — never alone).
 *
 * @param {number|undefined} percentOfGoal
 * @returns {number} 0..1
 */
export function dialPercent(percentOfGoal) {
  if (typeof percentOfGoal !== "number" || Number.isNaN(percentOfGoal)) return 0;
  return Math.max(0, Math.min(1, percentOfGoal));
}

/**
 * The dial's caption — the TRUE percent, not clamped.
 *
 * @param {number|undefined} percentOfGoal
 * @returns {string} e.g. "124%"
 */
export function percentLabel(percentOfGoal) {
  if (typeof percentOfGoal !== "number" || Number.isNaN(percentOfGoal)) return "—";
  return `${Math.round(percentOfGoal * 100)}%`;
}

/**
 * The dial needle's rotation in degrees, from the (clamped) percent-of-goal.
 *
 * The dial SVG is rotated `-rotate-90` so 0% starts at the top; the needle is
 * an untransformed vertical line, so its own rotation must compensate: +90.
 * The original `goalPct * 360 - 90` drew the needle MIRRORED (0% pointing to
 * the bottom, 100% to the left) — wrong for a marker that shares the dial with
 * the band arc, which also starts at the top. Pinned here so the fix is a
 * tested fact, not a component edit with no regression guard.
 *
 * @param {number} goalPct a `dialPercent()` result (already clamped 0..1)
 * @returns {number} degrees
 */
export function needleAngle(goalPct) {
  return goalPct * 360 + 90;
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

/**
 * The assets the dashboard may LIST — active rows only.
 *
 * The projection engine already excludes closed assets (§25), but the list
 * card renders raw `GET /assets` output, where a closed (rolled-over,
 * disconnected) account is a real row that never disappears. Showing it would
 * contradict the very numbers beside it. `status` is `'active'`/`'closed'`
 * (§20); anything else is treated as active for display (a future status value
 * must not silently empty the list).
 *
 * @param {object[]|null|undefined} assets
 * @returns {object[]}
 */
export function activeAssets(assets) {
  if (!Array.isArray(assets)) return [];
  return assets.filter((a) => a?.status !== "closed");
}

// ---------------------------------------------------------------------------
// Stacked income-source bar
// ---------------------------------------------------------------------------

/**
 * Compute the stacked-bar segments from `IncomeSources` (the engine's five
 * monthly after-tax figures), handling the absent-vs-zero Social Security rule
 * (#466/#469).
 *
 * A missing SS figure is distinguishable from an entered zero because the
 * engine's `assumptions.ss_adjusted_monthly_benefit` is `null` only when the
 * user never entered a benefit (the engine then simulates SS income as 0 and
 * the `gap` widens accordingly). The caller passes that flag through; the SS
 * segment is marked `absent: true` and given a ZERO share of the bar width, so
 * the component can render it as a labelled hatched marker instead of a $0
 * slice that reads as "you get nothing".
 *
 * When there is no income at all (all values zero) every segment is 0% and the
 * bar renders as a flat line — the component labels that state, never a NaN
 * width.
 *
 * @param {{own_savings?: number, employer_contributions?: number,
 *          social_security?: number, other_assets?: number, gap?: number}|null|undefined} sources
 * @param {boolean} ssAbsent true when `assumptions.ss_adjusted_monthly_benefit` is null
 * @returns {Array<{key: string, value: number, pct: number, absent: boolean}>}
 */
export function incomeSegments(sources, ssAbsent) {
  const rows = [
    { key: "ownSavings", value: sources?.own_savings ?? 0 },
    { key: "employer", value: sources?.employer_contributions ?? 0 },
    { key: "socialSecurity", value: sources?.social_security ?? 0, absent: ssAbsent === true },
    { key: "otherAssets", value: sources?.other_assets ?? 0 },
    { key: "gap", value: sources?.gap ?? 0 },
  ];
  // An absent SS figure takes NO bar width: its value is the 0 the engine
  // simulated with, and displaying it as a $0 slice is the exact false reading
  // #466 forbids. The non-absent total is the denominator.
  const total = rows.reduce((sum, r) => sum + (r.absent ? 0 : r.value), 0);
  return rows.map((r) => ({
    ...r,
    pct: total > 0 ? (r.absent ? 0 : r.value / total) * 100 : 0,
  }));
}

// ---------------------------------------------------------------------------
// Depletion line
// ---------------------------------------------------------------------------

/**
 * Normalize the "how long will my money last" verdict into a shape the
 * component can render with one i18n key and one number.
 *
 * The engine's `depletes` is the boolean and `depletion_age` the age at which
 * the target-withdrawal run first runs dry. `depletes: false` with a null age
 * is the engine's explicit "does not deplete before the horizon"; anything
 * else is the depletion age.
 *
 * @param {boolean|undefined} depletes
 * @param {number|null|undefined} depletionAge
 * @returns {{depletes: boolean, age: number|null}}
 */
export function depletionInfo(depletes, depletionAge) {
  const depletesTrue = depletes === true;
  return {
    depletes: depletesTrue,
    age: depletesTrue && typeof depletionAge === "number" ? depletionAge : null,
  };
}

// ---------------------------------------------------------------------------
// Slider / input plumbing
// ---------------------------------------------------------------------------

/**
 * Clamp a number into `[min, max]`, treating non-numbers as `min`.
 *
 * Used by the slider handlers so a stray NaN (e.g. a cleared input) can never
 * reach the projection POST, where the engine's `validate` would 400.
 *
 * @param {number|string|undefined} n
 * @param {number} min
 * @param {number} max
 * @returns {number}
 */
export function clamp(n, min, max) {
  const v = typeof n === "string" ? Number(n) : n;
  if (typeof v !== "number" || Number.isNaN(v)) return min;
  return Math.max(min, Math.min(max, v));
}

/**
 * Build the `POST /api/retirement/projection` override body from the what-if
 * control state. Absent-or-set, exactly the server's `ProjectionOverrides`
 * contract: every field is sent with the current control value, and nothing
 * here is persisted by the backend (a slider drag must never rewrite the stored
 * profile — the what-if is ephemeral).
 *
 * The Social Security claiming age is a LIVE-ONLY what-if: it is sent as
 * `ss_claiming_age_months` (months, matching the profile column and the
 * engine's `SsInput`) only when the control holds a real 62–70 age, and the
 * server only applies it to a profile that already has a COMPLETE entered SS
 * triple. A `0` (no SS entered) omits the field entirely, so a no-SS profile
 * never sends a meaningless claiming age.
 *
 * @param {{preTax: number, roth: number, realReturn: number, retirementAge: number,
 *          grossIncome: number, replacementRatio: number, taxRate: number,
 *          ssClaimingAge: number}} controls
 * @returns {object}
 */
export function buildProjectionOverrides(controls) {
  const body = {
    contribution_rate_pre_tax: controls.preTax,
    contribution_rate_roth: controls.roth,
    expected_real_return: controls.realReturn,
    target_retirement_age: controls.retirementAge,
    current_gross_income: controls.grossIncome,
    target_replacement_ratio: controls.replacementRatio,
    effective_tax_rate: controls.taxRate,
  };
  const ssAge = controls.ssClaimingAge;
  if (typeof ssAge === "number" && Number.isFinite(ssAge) && ssAge >= 62 && ssAge <= 70) {
    body.ss_claiming_age_months = Math.round(ssAge * 12);
  }
  return body;
}

/**
 * The what-if controls that CAN be persisted to the stored profile. The
 * profile has NO tax-rate column (the engine's `DEFAULT_EFFECTIVE_TAX_RATE`
 * supplies one per-run), so `effective_tax_rate` is a projection-only
 * override and must never be sent to `PUT /retirement/profile` — a
 * zero-based `/`-mismatch on the profile write would reject the whole body.
 *
 * `ss_claiming_age_months` is excluded for a different reason: the Social
 * Security claiming age is deliberately a LIVE-ONLY what-if (see
 * `buildProjectionOverrides`). Persisting it would let a "what if I claim at
 * 70?" experiment silently rewrite the user's stored plan, and the profile's
 * own claiming age carries an anchor (`'fra'`/`'explicit'`) this dashboard
 * control does not reproduce. The SS fields remain settable where #466 put
 * them — the chat arm and the REST `PUT` payload's `ss_claiming_age`.
 */
export function buildProfileSaveBody(controls) {
  const { effective_tax_rate, ss_claiming_age_months, ...body } = buildProjectionOverrides(controls);
  return body;
}

/**
 * Minimal trailing-edge debounce for the what-if recompute. The issue's
 * recompute strategy (#467's measured numbers) is: the deterministic figure
 * recomputes LIVE (debounced ~150ms on the server), the Monte Carlo band
 * recomputes on slider SETTLE. This helper implements the settle half; the
 * component holds the last-good band while a recompute is in flight and marks
 * it stale rather than blanking it.
 *
 * Pure and testable: fires `fn` with the LATEST arguments once no further call
 * has arrived for `ms`. Uses `setTimeout`/`clearTimeout`, which exist in node.
 *
 * @param {Function} fn
 * @param {number} ms
 * @returns {(...args: any[]) => void}
 */
export function debounce(fn, ms) {
  let timer = null;
  return (...args) => {
    if (timer != null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, ms);
  };
}

/**
 * The fixed hypothetical-illustration disclosure (constraint 3 / #454 decision
 * 3). Referenced by i18n key so the copy lives in the locale files, but the
 * KEY is pinned here so a refactor can never silently stop rendering it.
 */
export const DISCLOSURE_KEY = "retirement.disclosure";
export const SLIDER_DISCLOSURE_KEY = "retirement.sliderDisclosure";

// ---------------------------------------------------------------------------
// Label-key helpers
// ---------------------------------------------------------------------------

const SEGMENT_LABEL_KEYS = {
  ownSavings: "retirement.sourceOwnSavings",
  employer: "retirement.sourceEmployer",
  socialSecurity: "retirement.sourceSocialSecurity",
  otherAssets: "retirement.sourceOtherAssets",
  gap: "retirement.sourceGap",
};

/**
 * i18n key for a stacked-bar segment's legend label. Unknown keys fall back to
 * the other-assets label (a non-NaN, non-crash render is always better than an
 * `undefined` key).
 * @param {string} key one of `incomeSegments()`'s `key` values
 * @returns {string}
 */
export function segmentLabelKey(key) {
  return SEGMENT_LABEL_KEYS[key] ?? "retirement.sourceOtherAssets";
}

/**
 * i18n key for an asset's `asset_type` value (snake_case from the CHECK
 * constraint, serialized bare). Unknown values fall back to "Other" so a
 * widened CHECK never renders a raw key string.
 * @param {string} assetType
 * @returns {string}
 */
export function assetTypeLabelKey(assetType) {
  const map = {
    retirement_account: "retirement.assetTypeRetirementAccount",
    brokerage: "retirement.assetTypeBrokerage",
    cash: "retirement.assetTypeCash",
    pension: "retirement.assetTypePension",
    annuity: "retirement.assetTypeAnnuity",
    real_estate: "retirement.assetTypeRealEstate",
    other: "retirement.assetTypeOther",
  };
  return map[assetType] ?? "retirement.assetTypeOther";
}

/**
 * i18n key for an asset's `tax_treatment` value. Same fallback rule as
 * `assetTypeLabelKey`.
 * @param {string} taxTreatment
 * @returns {string}
 */
export function taxTreatmentLabelKey(taxTreatment) {
  const map = {
    pre_tax: "retirement.taxPreTax",
    roth: "retirement.taxRoth",
    taxable: "retirement.taxTaxable",
    hsa: "retirement.taxHsa",
    other: "retirement.taxOther",
  };
  return map[taxTreatment] ?? "retirement.taxOther";
}
