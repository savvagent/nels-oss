//! nels#467 — the retirement projection engine.
//!
//! **Purity contract (enforced by review):** this module imports neither
//! `sqlx`, nor `reqwest`, nor `std::env`, nor any clock. The current date is a
//! field on [`ProjectionRequest`]; callers (the `retirement.rs` REST handlers
//! and the `rag.rs` chat arm) assemble the request from `retirement_profiles` +
//! `assets` + `asset_holdings` DB rows and hand it over. This module only
//! computes. Same discipline as `entitlement.rs`.
//!
//! **Everything is in today's dollars at a real return.** There is no nominal
//! path anywhere here, and no inflation/COLA adjustment is applied to the
//! Social Security stream (that contract is #466's, and `claiming_adjustment`
//! has no inflation parameter — see `social_security.rs`).
//!
//! **Bucket model.** Three buckets mirror `assets::TaxTreatment`:
//! `pre_tax`, `roth`, and `taxable` (into which `Hsa`/`Other` fold — v1 has no
//! dedicated HSA withdrawal rule). `pre_tax` is internally tracked as two
//! sub-accounts, `pre_tax_own` and `pre_tax_employer`, from the first
//! contribution to the last withdrawal, because the stacked bar must split
//! "own savings" from "employer contributions" and no post-hoc allocation can
//! reproduce the split. The OPENING pre-tax balance is attributed entirely to
//! `pre_tax_own`; sim-time employer-match dollars accrue to
//! `pre_tax_employer`. Roth is entirely own; taxable is entirely "other assets".
//!
//! **Year loop.** Annual time step, whole-year age granularity, and the `&[f64]`
//! per-year real-return series is the seam that lets the deterministic path and
//! the Monte Carlo path share one loop and lets tests hand-build a return
//! sequence (the sequence-risk test is impossible without it). Within a year
//! the return applies FIRST to the start-of-year balance, then contributions
//! are added — contributions earn no return in the year they are made.
//!
//! **Deliberate simplifications (documented, not bugs):** a single portfolio
//! volatility constant `MC_REAL_RETURN_STD_DEV` regardless of the user's actual
//! allocation; `current_gross_income` held constant in real terms (no salary
//! growth — conservative, never overstates readiness); HSA/Other folded into
//! `taxable`; the sub-annual claiming-age precision floored to whole years
//! (monthly SS figure stays exact; only the START year is floored).
//!
//! **Everything entering the sim is `is_finite()`-guarded FIRST** — never an
//! ordered comparison, since `NaN <= 0.0` is false and a NaN would otherwise
//! sail past a bare magnitude check into the math and serialize as JSON `null`
//! (the §20/#464 trap). The engine never emits NaN/Infinity and never panics.

use chrono::{Datelike, NaiveDate};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};
use serde::{Deserialize, Serialize};

use crate::retirement::EmployerMatch;

/// Bisection iterations for the sustainable-withdrawal solve
/// ([`sustainable_annual_w_for_series`]). Each iteration is a full
/// decumulation (≤ a few hundred year-steps), so 40 trades ~40 runtimes per
/// solve against sub-cent convergence on a figure the UI renders rounded to
/// dollars; the deterministic budget in Task 11 measures it. A named constant
/// so the precision/runtime trade is reviewable and pinned by a test.
pub const BISECTION_ITERATIONS: u32 = 40;

/// Portfolio real-return volatility used by the Monte Carlo path: the US
/// 60/40 (equities/bonds) portfolio's annualized real-return standard
/// deviation over 1901–2022, 13.46%, per the Dimson–Marsh–Staunton / CFA
/// Institute Global Returns Yearbook (2023). ONE constant for the whole
/// portfolio regardless of the user's actual allocation — a documented
/// simplification (the request carries no allocation information).
///
/// **Provenance:** it is a volatility, not a forecast — the same disclaimers
/// that apply to `retirement::DEFAULT_EXPECTED_REAL_RETURN` apply here, and
/// #469's assumptions panel surfaces it as `mc_sigma`.
pub const MC_REAL_RETURN_STD_DEV: f64 = 0.1346;

/// Fixed production default MC seed — any fixed literal, so every user's
/// default projection is reproducible (same inputs → identical band, forever).
/// `ProjectionRequest.mc_seed` overrides it; the handlers set it when the
/// request is assembled.
pub const MC_DEFAULT_SEED: u64 = 0x4e454c53; // "NELS"

/// Number of Monte Carlo paths for the default projection.
///
/// **Measured, not guessed.** `tests::runtime_benchmarks` on a release build,
/// against the standard mid-career fixture — 44 accumulation years plus a
/// 25-year horizon:
///
/// | path                       | time     |
/// |----------------------------|----------|
/// | deterministic headline     | ~21 µs   |
/// | Monte Carlo, per path      | ~22 µs   |
/// | Monte Carlo at 1000 paths  | ~22 ms   |
/// | full `run_projection`      | ~22 ms   |
///
/// The Monte Carlo is linear in `mc_paths` and dwarfs everything else, so this
/// constant alone sets the endpoint's cost.
///
/// 1000 is where two constraints meet. Below roughly 500 the p10/p90 estimates
/// visibly move between neighbouring seeds, which would make the band jitter as
/// a user drags a slider — the band would look like a live reading of something
/// when it was really sampling noise. Above ~2000 the recompute crosses into
/// territory a user perceives as lag with no meaningful gain in decile
/// stability.
///
/// **Recommendation for #469, which this number exists to settle:** the two
/// figures are three orders of magnitude apart, so they get different
/// treatments. Recompute the deterministic headline LIVE on every slider
/// input — at ~21 µs it is free, and it is the number the dial shows. Recompute
/// the BAND only on settle, debounced. Do not run the Monte Carlo per
/// keystroke.
pub const MC_DEFAULT_PATHS: u32 = 1000;

/// Upper bound of the sustainable-withdrawal bisection, in ANNUAL dollars.
///
/// Deliberately far above any realistic annual withdrawal: it is a SEARCH
/// bound, not a policy limit, and the bisection's cost is a fixed
/// [`BISECTION_ITERATIONS`] regardless of how wide it is. At 40 iterations this
/// bound converges to about 1e-5 dollars, so widening it costs precision the
/// UI cannot render anyway and buys immunity from the one failure mode that
/// matters — an over-funded plan whose true sustainable withdrawal sits ABOVE
/// the bound is silently CAPPED at it, under-reporting the headline. See
/// `sustainable_annual_w_for_series`.
pub const DEFAULT_W_MAX: f64 = 10_000_000.0;

/// The effective tax rate applied to pre-tax withdrawals when the caller does
/// not override it, as a FRACTION.
///
/// **This is a planning convention, not tax advice, and not a bracket
/// calculation.** `retirement_profiles` has no tax-rate column — #465 did not
/// add one — so a projection assembled from a stored profile needs a documented
/// default, and `POST /api/retirement/projection` takes `effective_tax_rate` as
/// an override so #469's assumptions panel can expose it as an editable field.
///
/// 15% is chosen as a mid-range EFFECTIVE federal rate for a household drawing
/// a replacement income in the low six figures — meaningfully below the 22%
/// marginal bracket such a household sits in, because an effective rate
/// averages over the lower brackets beneath it. It is surfaced to the user via
/// `ProjectionAssumptions.effective_tax_rate`, per the #454 compliance decision
/// that every assumption renders on the same screen as the number.
pub const DEFAULT_EFFECTIVE_TAX_RATE: f64 = 0.15;

/// The lognormal drift: `ln(1 + r) − σ²/2` where `r` is the ANNUAL real return
/// as a FRACTION (`expected_real_return/100`). This is the log-mean that makes
/// a lognormal multiplier's GEOMETRIC mean equal `1 + r`. Pinned by a test so
/// the percent→fraction conversion cannot silently regress by 100×.
fn log_return_mean(r: f64, sigma: f64) -> f64 {
    (1.0 + r).ln() - (sigma * sigma) / 2.0
}


/// The three tax-treatment buckets, mirrored from `assets::TaxTreatment`.
///
/// `pre_tax` covers `PreTax`, `roth` covers `Roth`, and `taxable` folds in
/// `Taxable`, `Hsa` and `Other` (v1 has no dedicated HSA withdrawal rule and
/// the epic's withdrawal ordering is taxable → pre-tax → Roth).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Buckets {
    pub pre_tax: f64,
    pub roth: f64,
    pub taxable: f64,
}

impl Buckets {
    fn total(&self) -> f64 {
        self.pre_tax + self.roth + self.taxable
    }
}

/// The bucket a withdrawal is drawn from — drives [`TaxModel::tax_on_withdrawal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxedBucket {
    Taxable,
    PreTax,
    Roth,
}

/// A small, closed-set tax model: a single user-editable effective rate applied
/// to pre-tax withdrawals. NOT a trait — same discipline as
/// `bank_provider::Provider`. A bracket engine later phases in per-year rates
/// through the `year` parameter without changing call sites; v1 ships exactly
/// one implementation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TaxModel {
    /// Effective tax rate as a FRACTION (`0.2` = 20%), validated finite and in
    /// `0.0..=1.0` by [`ProjectionRequest::validate`].
    pub effective_rate: f64,
}

impl TaxModel {
    /// Tax owed on a withdrawal of `amount` from `kind`. `0.0` for
    /// `Taxable`/`Roth`; `amount × effective_rate` for `PreTax` (pre-tax
    /// withdrawals are taxed once, on the gross, when withdrawn). `year` is the
    /// seam for a future bracket engine — UNUSED in v1.
    pub fn tax_on_withdrawal(&self, kind: TaxedBucket, amount: f64, _year: u32) -> f64 {
        match kind {
            TaxedBucket::Taxable | TaxedBucket::Roth => 0.0,
            TaxedBucket::PreTax => amount * self.effective_rate,
        }
    }
}

/// Social Security input, present on the request only when the caller has all
/// three fields (benefit + both ages). An absent `ss` (or a partial input the
/// handler could not complete) means SS income `0.0` for every decumulation
/// year — a half-entered state is computed with, never an error.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SsInput {
    /// MONTHLY benefit in today's dollars, as printed on the SSA statement.
    pub monthly_benefit: f64,
    /// The age (in MONTHS) the benefit is quoted at — drives `claiming_adjustment`.
    pub quoted_at_age_months: i32,
    /// The age (in MONTHS) the user will start claiming.
    pub claiming_age_months: i32,
    pub birth_date: NaiveDate,
}

/// The engine's ONLY entry point: plain fixture-shaped inputs the engine does
/// not know how to load. Every `f64` entering the sim is validated finite and
/// in range by [`ProjectionRequest::validate`]; the engine never panics and
/// never emits NaN/Infinity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionRequest {
    pub starting_buckets: Buckets,
    pub birth_date: NaiveDate,
    pub today: NaiveDate,
    /// Whole years. A past retirement age is legal (zero accumulation years).
    pub target_retirement_age: u32,
    /// Whole years; the projection horizon.
    pub life_expectancy_age: u32,
    /// ANNUAL gross income in today's dollars, held constant in real terms.
    pub current_gross_income: f64,
    /// PERCENT of gross (`6.0` = 6%), same scale as #465's profile fields.
    pub contribution_rate_pre_tax: f64,
    /// PERCENT of gross (`3.0` = 3%).
    pub contribution_rate_roth: f64,
    /// PERCENT real annual return (`5.0` = 5%), the deterministic path's constant.
    pub expected_real_return: f64,
    /// FRACTION of gross (`0.75` = 75%) the user wants in retirement.
    pub target_replacement_ratio: f64,
    pub employer_match_formula: EmployerMatch,
    /// Effective tax rate as a FRACTION (`0.2` = 20%).
    pub effective_tax_rate: f64,
    pub ss: Option<SsInput>,
    /// Seeded MC: same seed + same inputs = identical output, always.
    pub mc_seed: u64,
    /// Number of Monte Carlo paths; must be `> 0`.
    pub mc_paths: u32,
    /// Upper bound of the sustainable-withdrawal bisection, in ANNUAL dollars.
    pub w_max: f64,
}

impl ProjectionRequest {
    /// Validate the whole request up front. `is_finite()` is guarded FIRST for
    /// every float — never an ordered comparison, since `NaN <= 0.0` is false.
    /// Per-field messages so a caller can fix the input rather than guess.
    pub(crate) fn validate(&self) -> Result<(), ProjectionError> {
        // Balances: finite and non-negative.
        for (name, v) in [
            ("starting_buckets.pre_tax", self.starting_buckets.pre_tax),
            ("starting_buckets.roth", self.starting_buckets.roth),
            ("starting_buckets.taxable", self.starting_buckets.taxable),
        ] {
            reject_non_finite(name, v)?;
            reject_negative(name, v)?;
        }

        // Rates: finite and in range. The finite guard comes FIRST.
        reject_non_finite("effective_tax_rate", self.effective_tax_rate)?;
        if self.effective_tax_rate < 0.0 || self.effective_tax_rate > 1.0 {
            return Err(ProjectionError::InvalidInput(format!(
                "effective_tax_rate must be a fraction between 0 and 1, got {}",
                self.effective_tax_rate
            )));
        }

        reject_non_finite("current_gross_income", self.current_gross_income)?;
        reject_negative("current_gross_income", self.current_gross_income)?;
        reject_non_finite("contribution_rate_pre_tax", self.contribution_rate_pre_tax)?;
        reject_negative("contribution_rate_pre_tax", self.contribution_rate_pre_tax)?;
        reject_non_finite("contribution_rate_roth", self.contribution_rate_roth)?;
        reject_negative("contribution_rate_roth", self.contribution_rate_roth)?;

        // The deterministic path multiplies by expected_real_return/100; the
        // MC log-mean is ln(1 + r) with r the fraction, so 1 + r must stay > 0.
        reject_non_finite("expected_real_return", self.expected_real_return)?;
        if self.expected_real_return <= -100.0 {
            return Err(ProjectionError::InvalidInput(format!(
                "expected_real_return must be greater than -100 (a return of -100% or worse is not a valid log-mean input), got {}",
                self.expected_real_return
            )));
        }

        reject_non_finite("target_replacement_ratio", self.target_replacement_ratio)?;
        reject_negative("target_replacement_ratio", self.target_replacement_ratio)?;

        reject_non_finite("w_max", self.w_max)?;
        if self.w_max <= 0.0 {
            return Err(ProjectionError::InvalidInput(format!(
                "w_max must be a positive annual dollar bound, got {}",
                self.w_max
            )));
        }

        if self.mc_paths == 0 {
            return Err(ProjectionError::InvalidInput(
                "mc_paths must be at least 1 (a percentile band from zero paths is meaningless)".into(),
            ));
        }

        // Impossible geometry. Retirement age already passed is legal (zero
        // accumulation years); only life-expectancy geometry is rejected.
        let current_age = age_on(self.today, self.birth_date);
        if self.life_expectancy_age <= current_age {
            return Err(ProjectionError::InvalidInput(format!(
                "life_expectancy_age ({}) must be greater than current age ({})",
                self.life_expectancy_age, current_age
            )));
        }
        if self.life_expectancy_age < self.target_retirement_age {
            return Err(ProjectionError::InvalidInput(format!(
                "life_expectancy_age ({}) must not be earlier than target_retirement_age ({})",
                self.life_expectancy_age, self.target_retirement_age
            )));
        }

        Ok(())
    }
}

/// Floored whole-year age. **Delegates to `retirement::current_age`** — #465's
/// existing pure helper is the single source of truth for the calendar
/// arithmetic (a birthday that has not occurred this year counts as last year);
/// this wrapper only adapts its `i32` to the engine's `u32` shape.
pub(crate) fn age_on(today: NaiveDate, birth_date: NaiveDate) -> u32 {
    crate::retirement::current_age(birth_date, today).max(0) as u32
}

/// Age at which RMDs begin, by birth year (SECURE Act 2.0).
///
/// 73 for `birth_year <= 1959`, 75 for `>= 1960`. Pre-1951 cohorts (whose
/// RMD age was 72, or 70½ before that) are COLLAPSED to 73 — a documented
/// simplification: projecting a pre-1951 user at 73 rather than 72 is
/// slightly conservative (a year's RMDs defer) and every real user in that
/// cohort is already past both ages, so the planner's accumulation window is
/// unaffected. No user-facing rule reads this back; it only shapes the
/// decumulation RMD stream.
pub(crate) fn rmd_start_age(birth_year: i32) -> u32 {
    if birth_year >= 1960 {
        75
    } else {
        73
    }
}

/// Lowest age that indexes [`UNIFORM_LIFETIME_DIVISORS`] — the RMD starting
/// age (73). The printed table also prints age 72, but an RMD never starts
/// before 73, so 72 is unreachable through [`rmd_divisor`] and is omitted.
pub(crate) const RMD_START_FLOOR: u32 = 73;

/// Last age with a printed divisor. The table's final row prints
/// "120 and over", so every age past this reuses its divisor (the tail rule).
pub(crate) const RMD_TABLE_TAIL_AGE: u32 = 120;

/// IRS Uniform Lifetime Table (Pub. 590-B, 2025 edition, Appendix B, Table III).
///
/// Static divisors, transcribed verbatim from the printed table, indexed by
/// `age − RMD_START_FLOOR`. **No computed divisors ever** — the RMD rules
/// define a lookup table, not a formula, and the 2022+ tables diverge from any
/// curve fitted to them. The 2025 publication's tables match every prior
/// edition back to 2022 (the IRS introduced the current life-expectancy tables
/// effective for RMDs in 2022), so this is stable for all open planning years.
pub(crate) const UNIFORM_LIFETIME_DIVISORS: [f64; (RMD_TABLE_TAIL_AGE - RMD_START_FLOOR + 1) as usize] = [
    26.5, // 73
    25.5, // 74
    24.6, // 75
    23.7, // 76
    22.9, // 77
    22.0, // 78
    21.1, // 79
    20.2, // 80
    19.4, // 81
    18.5, // 82
    17.7, // 83
    16.8, // 84
    16.0, // 85
    15.2, // 86
    14.4, // 87
    13.7, // 88
    12.9, // 89
    12.2, // 90
    11.5, // 91
    10.8, // 92
    10.1, // 93
    9.5,  // 94
    8.9,  // 95
    8.4,  // 96
    7.8,  // 97
    7.3,  // 98
    6.8,  // 99
    6.4,  // 100
    6.0,  // 101
    5.6,  // 102
    5.2,  // 103
    4.9,  // 104
    4.6,  // 105
    4.3,  // 106
    4.1,  // 107
    3.9,  // 108
    3.7,  // 109
    3.5,  // 110
    3.4,  // 111
    3.3,  // 112
    3.1,  // 113
    3.0,  // 114
    2.9,  // 115
    2.8,  // 116
    2.7,  // 117
    2.5,  // 118
    2.3,  // 119
    2.0, // 120 (and over)
];

/// RMD divisor for a whole-year `age` — the applicable denominator from
/// [`UNIFORM_LIFETIME_DIVISORS`]. Ages past the printed table reuse the last
/// printed divisor (2.0); the low clamp documents that an RMD never fires
/// below [`RMD_START_FLOOR`]. Never a computed value.
pub(crate) fn rmd_divisor(age: u32) -> f64 {
    let clamped = age.min(RMD_TABLE_TAIL_AGE).max(RMD_START_FLOOR);
    UNIFORM_LIFETIME_DIVISORS[(clamped - RMD_START_FLOOR) as usize]
}

/// The engine's internal money state: the three [`Buckets`] with the pre-tax
/// balance carried as TWO sub-accounts.
///
/// The own/employer split exists because the stacked income bar must attribute
/// "own savings" versus "employer contributions" and no post-hoc allocation can
/// reproduce it: the opening pre-tax balance is entirely own, sim-time
/// contributions are own, and employer-match dollars accrue to `pre_tax_employer`
/// from the first contribution to the last withdrawal. Only `pre_tax_own` and
/// `pre_tax_employer` carry it — `roth` and `taxable` are already as specific as
/// they need to be.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SimState {
    pre_tax_own: f64,
    pre_tax_employer: f64,
    roth: f64,
    taxable: f64,
}

impl SimState {
    /// The opening pre-tax balance is attributed ENTIRELY to "own" — there is no
    /// employer split on day one (documented decision).
    fn from_buckets(b: &Buckets) -> Self {
        Self { pre_tax_own: b.pre_tax, pre_tax_employer: 0.0, roth: b.roth, taxable: b.taxable }
    }

    fn pre_tax_total(&self) -> f64 {
        self.pre_tax_own + self.pre_tax_employer
    }

    fn to_buckets(&self) -> Buckets {
        Buckets { pre_tax: self.pre_tax_total(), roth: self.roth, taxable: self.taxable }
    }

    /// Split an RMD's gross proportionally across the two sub-accounts, so
    /// neither ever goes negative (rounding-guarded). The split is invisible
    /// through the aggregated totals — RMDs reduce `pre_tax_total` by exactly
    /// the gross either way — but it preserves each sub-account's share for
    /// #469's stacked income bar.
    fn split_rmd(&self, rmd_gross: f64) -> (f64, f64) {
        let total = self.pre_tax_total();
        if total <= 0.0 {
            return (0.0, 0.0);
        }
        let own = (rmd_gross * (self.pre_tax_own / total)).min(self.pre_tax_own);
        let employer = (rmd_gross - own).min(self.pre_tax_employer);
        (own, employer)
    }

    /// Withdraw `amount` gross from pre-tax, OWN first then EMPLOYER, returning
    /// what each sub-account contributed. Every pre-tax withdrawal path (RMD
    /// apart — that is proportional, not ordered) goes through here so no call
    /// site can re-derive the ordering.
    fn take_pre_tax(&mut self, amount: f64) -> (f64, f64) {
        let own = self.pre_tax_own.min(amount);
        self.pre_tax_own -= own;
        let employer = (amount - own).min(self.pre_tax_employer);
        self.pre_tax_employer -= employer;
        (own, employer)
    }
}

/// Whole accumulation years between today and the retirement boundary. A past
/// retirement age yields zero (legal geometry, pinned by a Task 2 test).
fn accumulation_years(req: &ProjectionRequest) -> u32 {
    req.target_retirement_age.saturating_sub(age_on(req.today, req.birth_date))
}

/// Run the accumulation years, maintaining the own/employer split.
///
/// The `&[f64]` return series is indexed from the first year; deterministic and
/// Monte Carlo callers always pass an exact-length series, and a series SHORTER
/// than the accumulation span falls back to a 0% real return for the missing
/// years (defensive only — the engine never panics, and no production caller
/// reaches the fallback).
fn accumulate_state(req: &ProjectionRequest, returns: &[f64]) -> SimState {
    let mut s = SimState::from_buckets(&req.starting_buckets);
    let years = accumulation_years(req);
    let gross = req.current_gross_income;
    let own_rate = req.contribution_rate_pre_tax / 100.0;
    let roth_rate = req.contribution_rate_roth / 100.0;
    // PERCENT-scaled total, as #465's evaluator expects (never divided by 100).
    let total_employee_pct = req.contribution_rate_pre_tax + req.contribution_rate_roth;
    let formula = &req.employer_match_formula;

    for i in 0..years {
        let r = returns.get(i as usize).copied().unwrap_or(0.0);
        // Return applies FIRST to the start-of-year balance…
        s.pre_tax_own *= 1.0 + r;
        s.pre_tax_employer *= 1.0 + r;
        s.roth *= 1.0 + r;
        s.taxable *= 1.0 + r;
        // …then contributions, which earn no return in their own year.
        s.pre_tax_own += gross * own_rate;
        s.roth += gross * roth_rate;
        s.pre_tax_employer += crate::retirement::employer_match(gross, total_employee_pct, formula);
    }
    s
}

/// Balance at the retirement boundary for an explicit per-year real-return
/// series (the `&[f64]` seam — sequence-risk tests and the MC path use it).
/// Returns the combined [`Buckets`]; the own/employer split stays internal to
/// the sim so `decumulate` can split RMDs and attribute income correctly.
pub(crate) fn accumulate(req: &ProjectionRequest, returns: &[f64]) -> Buckets {
    accumulate_state(req, returns).to_buckets()
}

/// The deterministic accumulation path: `expected_real_return/100` constant in
/// every accumulation year, sharing the same `&[f64]`-shaped loop.
pub fn deterministic_accumulate(req: &ProjectionRequest) -> Buckets {
    let series = vec![req.expected_real_return / 100.0; accumulation_years(req) as usize];
    accumulate(req, &series)
}

/// One year of the decumulation simulation.
///
/// `net_income` is the total income realized that year — Social Security (once
/// the claiming age is reached) + the net RMD + the after-tax needs-driven
/// withdrawals. `depleted` is TRUE only on the FINAL element of the vector: the
/// sim stops at the first year whose income falls short of the target.
///
/// The remaining fields attribute that income to its sources, so
/// [`income_sources_from_run`] can build the stacked-bar breakdown without
/// re-deriving withdrawal ordering. `pre_tax_own_taken`/`pre_tax_employer_taken`
/// are GROSS pre-tax dollars (RMD split + needs-driven), because the own/employer
/// split is a property of pre-tax dollars; `taxable_taken`/`roth_taken` are
/// untaxed, and `ss_received` is the year's Social Security (0 before claiming).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YearOutcome {
    pub net_income: f64,
    pub depleted: bool,
    pub ss_received: f64,
    pub taxable_taken: f64,
    pub pre_tax_own_taken: f64,
    pub pre_tax_employer_taken: f64,
    pub roth_taken: f64,
}

/// Resolve the ANNUAL Social Security figure exactly once, before the year loop.
///
/// An absent `ss` on the request means SS income `0.0` for every year. A present
/// input goes through #466's `claiming_adjustment`, which REJECTS an out-of-range
/// age rather than clamping; its refusal is surfaced as
/// [`ProjectionError::InvalidInput`] — never silently recomputed, never
/// swallowed, per #466's `adjustment_unavailable` contract. The result is in
/// today's dollars and carries no inflation (the #466 COLA contract).
fn ss_annual_for(req: &ProjectionRequest) -> Result<f64, ProjectionError> {
    let ss = match &req.ss {
        None => return Ok(0.0),
        Some(ss) => ss,
    };
    crate::social_security::claiming_adjustment(
        ss.monthly_benefit,
        ss.quoted_at_age_months,
        ss.claiming_age_months,
        ss.birth_date,
    )
    .map(|monthly| monthly * 12.0)
    .map_err(ProjectionError::InvalidInput)
}

/// Run the decumulation years, returning one [`YearOutcome`] per year until
/// depletion or `life_expectancy_age`.
///
/// The `&[f64]` return series spans the WHOLE horizon (accumulation + decumulation),
/// indexed from today — the same seam `accumulate_state` consumes — so the
/// own/employer sub-account split persists across the boundary without an
/// explicit transfer. Within a decumulation year: return applies FIRST to the
/// start-of-year balance, then the RMD is drawn from the post-growth pre-tax
/// balance, then Social Security and the needs-driven withdrawals meet the
/// target. If the target cannot be met, the year is `depleted` and the sim
/// stops.
fn decumulate_with_ss(
    req: &ProjectionRequest,
    returns: &[f64],
    target_annual_income: f64,
    tax: &TaxModel,
    ss_annual: f64,
) -> Vec<YearOutcome> {
    let mut s = accumulate_state(req, returns);
    let accum = accumulation_years(req);
    let rmd_start = rmd_start_age(req.birth_date.year());
    let ss_start_age: i32 = req.ss.map(|ss| ss.claiming_age_months / 12).unwrap_or(i32::MAX);
    let rate = tax.effective_rate;

    let mut outcomes = Vec::new();
    for age in req.target_retirement_age..=req.life_expectancy_age {
        // Index into the full-horizon return series: years from today.
        let i = accum + (age - req.target_retirement_age);
        let calendar_year = (req.today.year() + i as i32) as u32;
        // Return applies FIRST to the start-of-year balance…
        let r = returns.get(i as usize).copied().unwrap_or(0.0);
        s.pre_tax_own *= 1.0 + r;
        s.pre_tax_employer *= 1.0 + r;
        s.roth *= 1.0 + r;
        s.taxable *= 1.0 + r;

        // …then the RMD, from the post-growth pre-tax balance, once the age is
        // at or past the SECURE 2.0 start age. Its NET (gross minus the tax)
        // counts toward income; its GROSS is split across the sub-accounts so
        // the own/employer attribution survives to the source breakdown.
        let pre_tax = s.pre_tax_total();
        let rmd_gross = if age >= rmd_start && pre_tax > 0.0 {
            pre_tax / rmd_divisor(age)
        } else {
            0.0
        };
        let (rmd_own, rmd_employer) =
            if rmd_gross > 0.0 { s.split_rmd(rmd_gross) } else { (0.0, 0.0) };
        s.pre_tax_own -= rmd_own;
        s.pre_tax_employer -= rmd_employer;
        let rmd_net = rmd_gross - tax.tax_on_withdrawal(TaxedBucket::PreTax, rmd_gross, calendar_year);

        // Social Security once the (floored whole-year) claiming age is reached.
        let ss_for_year = if (age as i32) >= ss_start_age { ss_annual } else { 0.0 };

        // Needs-driven withdrawals: taxable → pre-tax → Roth. Pre-tax is drawn
        // GROSSED UP so the after-tax amount covers the residual need.
        let mut need = (target_annual_income - ss_for_year - rmd_net).max(0.0);

        let taxable_taken = s.taxable.min(need);
        s.taxable -= taxable_taken;
        need -= taxable_taken;

        let grossed_need = if rate >= 1.0 { f64::INFINITY } else { need / (1.0 - rate) };
        let pre_tax_taken = s.pre_tax_total().min(grossed_need);
        let (pre_tax_own_taken, pre_tax_employer_taken) = s.take_pre_tax(pre_tax_taken);
        let pre_tax_net = pre_tax_taken * (1.0 - rate);
        need -= pre_tax_net;

        let roth_taken = s.roth.min(need);
        s.roth -= roth_taken;
        need -= roth_taken;

        let depleted = need > 0.0;
        if depleted {
            // Clamp every balance to zero — never negative — and STOP. Later
            // years are not simulated: the portfolio is gone.
            s.pre_tax_own = 0.0;
            s.pre_tax_employer = 0.0;
            s.roth = 0.0;
            s.taxable = 0.0;
        }

        let net_income = ss_for_year + rmd_net + taxable_taken + pre_tax_net + roth_taken;
        outcomes.push(YearOutcome {
            net_income,
            depleted,
            ss_received: ss_for_year,
            taxable_taken,
            pre_tax_own_taken: rmd_own + pre_tax_own_taken,
            pre_tax_employer_taken: rmd_employer + pre_tax_employer_taken,
            roth_taken,
        });

        if depleted {
            break;
        }
    }
    outcomes
}

/// Decumulation for an explicit per-year real-return series.
///
/// Resolves the Social Security figure ONCE (a refusal surfaces as
/// [`ProjectionError::InvalidInput`], never a silent recompute) then runs the
/// year loop. Returns per-year outcomes through `life_expectancy_age`, or
/// shorter when the portfolio depletes (the final element carries
/// `depleted: true`).
pub(crate) fn decumulate(
    req: &ProjectionRequest,
    returns: &[f64],
    target_annual_income: f64,
    tax: &TaxModel,
) -> Result<Vec<YearOutcome>, ProjectionError> {
    let ss_annual = ss_annual_for(req)?;
    Ok(decumulate_with_ss(req, returns, target_annual_income, tax, ss_annual))
}

/// Whole-horizon year count: accumulation years plus decumulation years. This
/// is the length of the `&[f64]` return series the sims consume — the single
/// series spans accumulation AND decumulation, indexed from today.
fn horizon_years(req: &ProjectionRequest) -> u32 {
    accumulation_years(req) + (req.life_expectancy_age - req.target_retirement_age + 1)
}

/// The deterministic return series: `expected_real_return/100` constant in
/// every year of the whole horizon.
fn deterministic_series(req: &ProjectionRequest) -> Vec<f64> {
    vec![req.expected_real_return / 100.0; horizon_years(req) as usize]
}

/// The deterministic decumulation path: `expected_real_return/100` constant in
/// every year of the whole horizon.
pub fn deterministic_decumulate(
    req: &ProjectionRequest,
    target_annual_income: f64,
    tax: &TaxModel,
) -> Result<Vec<YearOutcome>, ProjectionError> {
    let ss_annual = ss_annual_for(req)?;
    let series = deterministic_series(req);
    Ok(decumulate_with_ss(req, &series, target_annual_income, tax, ss_annual))
}

/// Solve the sustainable withdrawal: the constant REAL ANNUAL net portfolio
/// withdrawal the portfolio can fund every year through `life_expectancy_age`,
/// found by bisection over `w ∈ [0, req.w_max]`.
///
/// Each trial runs the FULL sim with `w` as the target income (Social Security
/// offsets, RMDs forced, taxes applied), and the predicate is monotone: a run
/// that survives can afford more, a run that depletes cannot. After
/// [`BISECTION_ITERATIONS`] the interval midpoint is the answer. Inputs whose
/// sustainable figure exceeds `w_max` (pathological oversavings) are CAPPED at
/// `w_max` — documented, and `percent_of_goal` is still reported > 1.
///
/// `ss_annual` and `returns` are the caller's to resolve exactly once — the
/// Monte Carlo path reuses this workhorse per path with the path's own series
/// and the SAME Social Security figure.
fn sustainable_annual_w_for_series(
    req: &ProjectionRequest,
    returns: &[f64],
    tax: &TaxModel,
    ss_annual: f64,
) -> f64 {
    // w = 0 always survives (need is always 0), so `lo` is a valid invariant;
    // `hi` may be capped at w_max when even that does not deplete.
    let mut lo = 0.0;
    let mut hi = req.w_max;
    for _ in 0..BISECTION_ITERATIONS {
        let mid = (lo + hi) / 2.0;
        let outcomes = decumulate_with_ss(req, returns, mid, tax, ss_annual);
        let survives = outcomes.last().map(|o| !o.depleted).unwrap_or(false);
        if survives {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// The deterministic sustainable withdrawal for a validated request.
///
/// Returns `Result` rather than the `f64` the plan sketched, because resolving
/// the Social Security figure can refuse an out-of-range claiming age (#466's
/// `adjustment_unavailable` contract) — an `f64` signature would force either a
/// panic (violating the module's never-panic invariant) or a fabricated number.
/// `run_projection` and `monte_carlo` bypass this wrapper and use
/// [`sustainable_annual_w_for_series`] with the SS figure resolved ONCE.
pub fn sustainable_annual_w(
    req: &ProjectionRequest,
    tax: &TaxModel,
) -> Result<f64, ProjectionError> {
    let ss_annual = ss_annual_for(req)?;
    let series = deterministic_series(req);
    Ok(sustainable_annual_w_for_series(req, &series, tax, ss_annual))
}

/// Monthly income the deterministic SUSTAINABLE run draws from each source —
/// the stacked income bar's segments. All segments are AFTER-TAX (pre-tax
/// withdrawals are netted at `effective_rate`), so they are amounts the user
/// actually receives. `gap = max(0, target_monthly − sum)`.
///
/// `own_savings` = net Roth + net pre-tax-own withdrawals; `employer_contributions`
/// = net pre-tax-employer withdrawals; `other_assets` = taxable withdrawals;
/// `social_security` = the ADJUSTED monthly benefit (a rate, from #466), NOT an
/// average across the horizon. The withdrawal sources are averaged over the
/// run's years (total / years / 12) so each is a monthly rate comparable to the
/// target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomeSources {
    pub own_savings: f64,
    pub employer_contributions: f64,
    pub social_security: f64,
    pub other_assets: f64,
    pub gap: f64,
}

fn income_sources_from_run(
    outcomes: &[YearOutcome],
    ss_annual: f64,
    tax: &TaxModel,
    target_monthly: f64,
) -> IncomeSources {
    let rate = tax.effective_rate;
    let years = outcomes.len().max(1) as f64;
    let own_net: f64 = outcomes
        .iter()
        .map(|o| o.pre_tax_own_taken * (1.0 - rate) + o.roth_taken)
        .sum();
    let employer_net: f64 =
        outcomes.iter().map(|o| o.pre_tax_employer_taken * (1.0 - rate)).sum();
    let taxable: f64 = outcomes.iter().map(|o| o.taxable_taken).sum();

    let own_savings = own_net / (12.0 * years);
    let employer_contributions = employer_net / (12.0 * years);
    let other_assets = taxable / (12.0 * years);
    let social_security = ss_annual / 12.0;

    let sum = own_savings + employer_contributions + social_security + other_assets;
    let gap = (target_monthly - sum).max(0.0);
    IncomeSources { own_savings, employer_contributions, social_security, other_assets, gap }
}

/// The percentile band of deterministic/expected income the Monte Carlo path
/// produces (Task 8 wires it into [`Projection`]). The band is reported even
/// when degenerate (all five equal) — a flat band for flat inputs is a fact,
/// not an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PercentileBand {
    pub p10: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p90: f64,
}

/// Every input the projection was computed from, echoed back — the "show your
/// assumptions on the same screen as the number" surface for #469. Echoing
/// stops the UI from re-deriving a parameter and drifting from the engine.
///
/// `today` is echoed as `current_date`, and the raw `ss` input as the two
/// DERIVED figures (`ss_adjusted_monthly_benefit`, `ss_claiming_age_used` — the
/// floored whole-year age the sim actually starts paying at), so the panel
/// shows what the math used, not what the user typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionAssumptions {
    // Derived figures, computed by the engine (the parts a UI must NOT re-derive).
    pub current_age: u32,
    pub rmd_start_age: u32,
    pub mc_sigma: f64,
    pub ss_adjusted_monthly_benefit: Option<f64>,
    pub ss_claiming_age_used: Option<i32>,
    pub sustainable_annual_w: f64,
    pub bisection_iterations: u32,
    pub current_date: NaiveDate,
    // Echoed request inputs.
    pub starting_buckets: Buckets,
    pub birth_date: NaiveDate,
    pub target_retirement_age: u32,
    pub life_expectancy_age: u32,
    pub current_gross_income: f64,
    pub contribution_rate_pre_tax: f64,
    pub contribution_rate_roth: f64,
    pub expected_real_return: f64,
    pub target_replacement_ratio: f64,
    pub employer_match_formula: EmployerMatch,
    pub effective_tax_rate: f64,
    pub mc_seed: u64,
    pub mc_paths: u32,
    pub w_max: f64,
}

/// The projection the REST handler serializes and the chat arm attaches to
/// `ChatResponse`. `percentile_band`/`success_rate` are the Monte Carlo results
/// (Task 7's `monte_carlo`); `run_projection` wires them in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// The headline: constant real monthly income (sustainable portfolio
    /// withdrawal + adjusted Social Security) in today's dollars.
    pub deterministic_monthly_income: f64,
    /// `deterministic_monthly_income / target_monthly_income` — a FRACTION,
    /// never pre-multiplied (`1.24` = 124%, the dial can exceed 100%). `None`
    /// when the target monthly income is zero or negative.
    pub percent_of_goal: Option<f64>,
    pub percentile_band: PercentileBand,
    /// Fraction of Monte Carlo paths that survive to `life_expectancy_age`
    /// (their TARGET run, not the sustainable run).
    pub success_rate: f64,
    pub income_sources: IncomeSources,
    /// Age the TARGET-withdrawal run first depletes, if it does.
    pub depletion_age: Option<i32>,
    /// Whether the TARGET-withdrawal run depletes before `life_expectancy_age`.
    pub depletes: bool,
    pub assumptions: ProjectionAssumptions,
}

/// Assemble the full projection from a request. The ONLY entry point the
/// handlers call; validates first, so an `InvalidInput` surfaces before any
/// simulation runs.
///
/// Flow: validate → resolve SS once → `sustainable_annual_w` (constant returns)
/// → headline + `percent_of_goal` → `IncomeSources` from the sustainable run →
/// the target-run depletion verdict → `monte_carlo` for the band + success rate
/// → `assumptions`. The band bounds the headline by construction (per-path
/// sustainable portfolio income + the same deterministic SS figure).
pub fn run_projection(req: &ProjectionRequest) -> Result<Projection, ProjectionError> {
    req.validate()?;
    let tax = TaxModel { effective_rate: req.effective_tax_rate };
    let ss_annual = ss_annual_for(req)?;
    let series = deterministic_series(req);

    let w = sustainable_annual_w_for_series(req, &series, &tax, ss_annual);
    let deterministic_monthly_income = w / 12.0 + ss_annual / 12.0;

    let target_monthly = req.current_gross_income * req.target_replacement_ratio / 12.0;
    let percent_of_goal = if target_monthly <= 0.0 {
        None
    } else {
        Some(deterministic_monthly_income / target_monthly)
    };

    let sustainable_outcomes = decumulate_with_ss(req, &series, w, &tax, ss_annual);
    let income_sources =
        income_sources_from_run(&sustainable_outcomes, ss_annual, &tax, target_monthly);

    let (depletes, depletion_age) = target_run_with_depletion(req)?;

    let (percentile_band, success_rate) = monte_carlo(req)?;

    let assumptions = ProjectionAssumptions {
        current_age: age_on(req.today, req.birth_date),
        rmd_start_age: rmd_start_age(req.birth_date.year()),
        mc_sigma: MC_REAL_RETURN_STD_DEV,
        ss_adjusted_monthly_benefit: req.ss.map(|_| ss_annual / 12.0),
        ss_claiming_age_used: req.ss.map(|ss| ss.claiming_age_months / 12),
        sustainable_annual_w: w,
        bisection_iterations: BISECTION_ITERATIONS,
        current_date: req.today,
        starting_buckets: req.starting_buckets,
        birth_date: req.birth_date,
        target_retirement_age: req.target_retirement_age,
        life_expectancy_age: req.life_expectancy_age,
        current_gross_income: req.current_gross_income,
        contribution_rate_pre_tax: req.contribution_rate_pre_tax,
        contribution_rate_roth: req.contribution_rate_roth,
        expected_real_return: req.expected_real_return,
        target_replacement_ratio: req.target_replacement_ratio,
        employer_match_formula: req.employer_match_formula.clone(),
        effective_tax_rate: req.effective_tax_rate,
        mc_seed: req.mc_seed,
        mc_paths: req.mc_paths,
        w_max: req.w_max,
    };

    Ok(Projection {
        deterministic_monthly_income,
        percent_of_goal,
        percentile_band,
        success_rate,
        income_sources,
        depletion_age: depletion_age.map(|a| a as i32),
        depletes,
        assumptions,
    })
}

/// Monte Carlo: seeded lognormal annual real returns, `mc_paths` paths, two
/// per-path verdicts — the TARGET run's survival (success counter) and the
/// sustainable solve (band member). Returns the 10/25/50/75/90 percentile band
/// of per-path TOTAL monthly income (sustainable portfolio withdrawal + the
/// deterministic Social Security figure, identical across paths) and the
/// success rate: the fraction of paths whose target-income run reaches
/// `life_expectancy_age` undepleted (over-funded → 1.0, under-funded → 0.0).
///
/// **Reproducibility contract:** the ONLY sources of randomness are
/// `StdRng::seed_from_u64(req.mc_seed)` and `Normal` — no `thread_rng`, no
/// entropy, anywhere. Same seed + same inputs → identical output, forever.
///
/// The path-major loop draws ONE `&[f64]` series per path spanning the whole
/// horizon (accumulation + decumulation, indexed from today — the Task 4/5
/// seam), computes the lognormal multiplier as `exp(log_return_mean + σ·z)`,
/// and reuses [`sustainable_annual_w_for_series`] with the path's own series
/// so the band is per-path sustainable income by construction. `paths` is
/// guaranteed `> 0` by [`ProjectionRequest::validate`]; this function validates
/// first so an `InvalidInput` never reaches the RNG.
pub fn monte_carlo(req: &ProjectionRequest) -> Result<(PercentileBand, f64), ProjectionError> {
    req.validate()?;
    let tax = TaxModel { effective_rate: req.effective_tax_rate };
    let ss_annual = ss_annual_for(req)?;
    let target = req.current_gross_income * req.target_replacement_ratio;

    let mean = log_return_mean(req.expected_real_return / 100.0, MC_REAL_RETURN_STD_DEV);
    let normal = Normal::new(mean, MC_REAL_RETURN_STD_DEV)
        .expect("validate() guarantees a finite, positive-sigma Normal");
    let mut rng = StdRng::seed_from_u64(req.mc_seed);
    let horizon = horizon_years(req) as usize;

    let mut totals = Vec::with_capacity(req.mc_paths as usize);
    let mut successes: u32 = 0;
    for _ in 0..req.mc_paths {
        // Path-major draw order: one whole-horizon series per path. The seam
        // stores RATES (`1.0 + r`), so the lognormal MULTIPLIER `exp(μ + σ·z)`
        // is converted back to a rate (`exp(...) - 1.0`) — storing the raw
        // multiplier would compound the portfolio at ~104% instead of ~4%.
        let series: Vec<f64> = (0..horizon)
            .map(|_| normal.sample(&mut rng).exp() - 1.0)
            .collect();

        // Per-path sustainable solve → band member.
        let w = sustainable_annual_w_for_series(req, &series, &tax, ss_annual);
        totals.push(w / 12.0 + ss_annual / 12.0);

        // Per-path TARGET run → success counter.
        let outcomes = decumulate_with_ss(req, &series, target, &tax, ss_annual);
        if outcomes.last().map(|o| !o.depleted).unwrap_or(false) {
            successes += 1;
        }
    }

    totals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let band = percentile_band(&totals);
    let success_rate = successes as f64 / req.mc_paths as f64;
    Ok((band, success_rate))
}

/// Nearest-rank percentiles over a SORTED slice of per-path totals. Degenerate
/// inputs (a flat set — all totals equal) produce a flat band, which is a fact
/// about the input, not an error.
fn percentile_band(sorted: &[f64]) -> PercentileBand {
    let n = sorted.len();
    let idx = |frac: f64| (frac * (n - 1) as f64) as usize;
    PercentileBand {
        p10: sorted[idx(0.10)],
        p25: sorted[idx(0.25)],
        p50: sorted[idx(0.50)],
        p75: sorted[idx(0.75)],
        p90: sorted[idx(0.90)],
    }
}

/// The one-number readiness answer the deterministic path feeds the card:
/// does a target-income run deplete, and at what age?
///
/// The target is `current_gross_income × target_replacement_ratio`. A `None`
/// depletion age means the portfolio survives to `life_expectancy_age`.
pub fn target_run_with_depletion(req: &ProjectionRequest) -> Result<(bool, Option<u32>), ProjectionError> {
    let tax = TaxModel { effective_rate: req.effective_tax_rate };
    let target = req.current_gross_income * req.target_replacement_ratio;
    let outcomes = deterministic_decumulate(req, target, &tax)?;
    match outcomes.last() {
        Some(YearOutcome { depleted: true, .. }) => {
            let age = req.target_retirement_age + outcomes.len() as u32 - 1;
            Ok((true, Some(age)))
        }
        _ => Ok((false, None)),
    }
}
fn reject_non_finite(name: &str, v: f64) -> Result<(), ProjectionError> {
    if !v.is_finite() {
        return Err(ProjectionError::InvalidInput(format!(
            "{name} must be a finite number, got {v}"
        )));
    }
    Ok(())
}

fn reject_negative(name: &str, v: f64) -> Result<(), ProjectionError> {
    if v < 0.0 {
        return Err(ProjectionError::InvalidInput(format!(
            "{name} must be non-negative, got {v}"
        )));
    }
    Ok(())
}

/// Typed engine error — the handlers map `InvalidInput` to a 400.
#[derive(Debug)]
pub enum ProjectionError {
    InvalidInput(String),
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectionError::InvalidInput(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProjectionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retirement::MatchTier;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// A valid, ordinary request: born 1980-06-15, today 2026-07-31 (age 46),
    /// retire 65, horizon 90, $100k gross, 6% pre-tax + 3% Roth, 5% real return.
    fn test_request() -> ProjectionRequest {
        ProjectionRequest {
            starting_buckets: Buckets { pre_tax: 100_000.0, roth: 50_000.0, taxable: 25_000.0 },
            birth_date: d(1980, 6, 15),
            today: d(2026, 7, 31),
            target_retirement_age: 65,
            life_expectancy_age: 90,
            current_gross_income: 100_000.0,
            contribution_rate_pre_tax: 6.0,
            contribution_rate_roth: 3.0,
            expected_real_return: 5.0,
            target_replacement_ratio: 0.75,
            employer_match_formula: EmployerMatch { tiers: vec![], annual_dollar_cap: None },
            effective_tax_rate: 0.2,
            ss: None,
            mc_seed: 42,
            mc_paths: 100,
            w_max: 500_000.0,
        }
    }

    #[test]
    fn a_plain_request_validates() {
        test_request().validate().unwrap();
    }

    // ------------------------------------------------------------------
    // Task 12: runtime budget for #469's live sliders
    // ------------------------------------------------------------------

    /// Measure the deterministic path and a full Monte Carlo, so #469 can pick
    /// its recompute strategy against real numbers rather than a guess.
    ///
    /// Run it with:
    ///
    /// ```text
    /// cargo test --release --bin backend \
    ///     retirement_projection::tests::runtime_benchmarks -- --ignored --nocapture
    /// ```
    ///
    /// `#[ignore]` because CI timing is unreliable and a wall-clock assertion
    /// would be a flake generator. It prints and asserts nothing about
    /// duration; the numbers it produces are recorded in `MC_DEFAULT_PATHS`'s
    /// doc comment and in the PR body. `--release` matters: the debug build is
    /// roughly an order of magnitude slower and would justify the wrong
    /// `MC_DEFAULT_PATHS`.
    ///
    /// Plain `#[test]`, not `#[tokio::test]` — the engine is pure and
    /// synchronous, so there is nothing to await and no runtime to start.
    #[test]
    #[ignore = "timing benchmark; run via: cargo test --release -- --ignored --nocapture"]
    fn runtime_benchmarks() {
        use std::time::Instant;

        let req = test_request();
        // The bisection dominates the deterministic path, and both paths walk
        // the full horizon, so the fixture's 44 accumulation + 25 decumulation
        // years is representative of a mid-career user.
        const REPS: u32 = 200;

        // Warm up, so the first-iteration cost of any lazy initialization does
        // not land inside the measurement.
        let _ = run_projection(&ProjectionRequest { mc_paths: 1, ..req.clone() }).unwrap();

        // The deterministic path ALONE: run_projection minus the Monte Carlo.
        // Measured as the sustainable solve plus the target run, which is
        // exactly what a live slider recomputes.
        let t0 = Instant::now();
        for _ in 0..REPS {
            let tax = TaxModel { effective_rate: req.effective_tax_rate };
            let ss_annual = ss_annual_for(&req).unwrap();
            let series = deterministic_series(&req);
            let w = sustainable_annual_w_for_series(&req, &series, &tax, ss_annual);
            let _ = target_run_with_depletion(&req).unwrap();
            std::hint::black_box(w);
        }
        let deterministic = t0.elapsed() / REPS;

        // A full Monte Carlo at the shipped default.
        let mc_req = ProjectionRequest { mc_paths: MC_DEFAULT_PATHS, ..req.clone() };
        let t1 = Instant::now();
        for _ in 0..REPS {
            std::hint::black_box(monte_carlo(&mc_req).unwrap());
        }
        let mc = t1.elapsed() / REPS;

        // And the whole endpoint's work, which is what the REST handler costs.
        let t2 = Instant::now();
        for _ in 0..REPS {
            std::hint::black_box(run_projection(&mc_req).unwrap());
        }
        let full = t2.elapsed() / REPS;

        println!("\n=== nels#467 runtime benchmarks ===");
        println!("deterministic path          : {deterministic:?}");
        println!("monte carlo @ {MC_DEFAULT_PATHS} paths    : {mc:?}");
        println!("full run_projection         : {full:?}");
        println!("===================================\n");
    }

    #[test]
    fn tax_model_validation_rejects_out_of_range_and_non_finite_rates() {
        // effective_rate must be finite and 0.0..=1.0.
        let mut req = test_request();
        req.effective_tax_rate = -0.01;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("effective_tax_rate")));
        req.effective_tax_rate = 1.01;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("effective_tax_rate")));
        req.effective_tax_rate = f64::NAN;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("finite")));
        req.effective_tax_rate = f64::INFINITY;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("finite")));
    }

    #[test]
    fn project_request_validation_rejects_impossible_geometry() {
        // life_expectancy <= current age (46), or life_expectancy < retirement age.
        let mut req = test_request();
        req.life_expectancy_age = 46;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("life_expectancy_age") && m.contains("current age")));
        req.life_expectancy_age = 45;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("life_expectancy_age") && m.contains("current age")));

        let mut req = test_request();
        req.life_expectancy_age = 64; // < retirement age 65
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("life_expectancy_age") && m.contains("target_retirement_age")));
    }

    #[test]
    fn a_passed_retirement_age_is_legal_geometry() {
        // Retirement age already passed → zero accumulation years, NOT an error.
        let mut req = test_request();
        req.target_retirement_age = 40; // already reached (current age 46)
        assert!(req.validate().is_ok());
    }

    #[test]
    fn validation_guards_non_finite_first_never_an_ordered_comparison() {
        // The §20/#464 trap: NaN <= 0.0 is FALSE, so a NaN must be caught by the
        // is_finite guard, never by a magnitude test. Pinned here so the guard
        // cannot silently regress into an ordered comparison.
        for sails_past_a_le_zero_guard in [f64::NAN, f64::INFINITY] {
            // Bound through a loop variable rather than compared as a literal,
            // which would trip the `invalid_nan_comparisons` lint — a lint that
            // exists to warn about exactly the mistake being demonstrated here.
            assert!(
                !(sails_past_a_le_zero_guard <= 0.0),
                "{sails_past_a_le_zero_guard} <= 0.0 must be FALSE -- this is \
                 precisely why is_finite() has to come first and separately"
            );
        }
        let mut req = test_request();
        req.starting_buckets.pre_tax = f64::NAN;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("finite")));
        req.starting_buckets.pre_tax = 0.0;
        req.starting_buckets.roth = f64::NEG_INFINITY;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("finite")));
    }

    #[test]
    fn validation_rejects_negative_balances_income_and_rates() {
        let mut req = test_request();
        req.starting_buckets.taxable = -1.0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("non-negative")));
        req.starting_buckets.taxable = 0.0;
        req.current_gross_income = -1.0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("non-negative")));
        req.current_gross_income = 100_000.0;
        req.contribution_rate_pre_tax = -1.0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("non-negative")));
    }

    #[test]
    fn validation_rejects_zero_paths_and_non_positive_w_max() {
        let mut req = test_request();
        req.mc_paths = 0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("mc_paths")));
        let mut req = test_request();
        req.w_max = 0.0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("w_max")));
        req.w_max = f64::NAN;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("finite")));
    }

    #[test]
    fn expected_real_return_below_minus_100_rejected_before_the_log_mean() {
        // The MC log-mean ln(1 + r) needs 1 + r > 0; -100% is the boundary.
        let mut req = test_request();
        req.expected_real_return = -100.0;
        assert!(matches!(req.validate(), Err(ProjectionError::InvalidInput(m)) if m.contains("expected_real_return")));
        let mut req = test_request();
        req.expected_real_return = -99.0;
        assert!(req.validate().is_ok());
    }

    #[test]
    fn tax_on_withdrawal_taxes_only_pre_tax_and_at_the_rate() {
        let tax = TaxModel { effective_rate: 0.2 };
        assert_eq!(tax.tax_on_withdrawal(TaxedBucket::Taxable, 1_000.0, 2030), 0.0);
        assert_eq!(tax.tax_on_withdrawal(TaxedBucket::Roth, 1_000.0, 2030), 0.0);
        assert_eq!(tax.tax_on_withdrawal(TaxedBucket::PreTax, 1_000.0, 2030), 200.0);
        assert_eq!(tax.tax_on_withdrawal(TaxedBucket::PreTax, 0.0, 2030), 0.0);
    }

    #[test]
    fn bucket_mapping_is_mirrored_from_tax_treatment() {
        // PreTax → pre_tax, Roth → roth, everything else → taxable. The engine
        // never sees the #464 TaxTreatment type, but the three-way mapping is
        // what the handler applies when assembling starting_buckets.
        let b = Buckets { pre_tax: 1.0, roth: 2.0, taxable: 3.0 };
        assert_eq!(b.total(), 6.0);
        assert_eq!(TaxedBucket::Taxable, TaxedBucket::Taxable);
        assert_ne!(TaxedBucket::PreTax, TaxedBucket::Roth);
    }

    #[test]
    fn age_on_delegates_to_retirement_current_age() {
        assert_eq!(age_on(d(2026, 7, 31), d(1980, 6, 15)), 46); // birthday not yet reached
        assert_eq!(age_on(d(2026, 6, 15), d(1980, 6, 15)), 46); // birthday today
        assert_eq!(age_on(d(2026, 6, 14), d(1980, 6, 15)), 45); // birthday not yet this year
        // A future birth date (degenerate) clamps to 0, matching current_age's max(0).
        assert_eq!(age_on(d(2026, 7, 31), d(2030, 1, 1)), 0);
    }

    #[test]
    fn rmd_start_age_differs_by_birth_year() {
        assert_eq!(rmd_start_age(1959), 73);
        assert_eq!(rmd_start_age(1960), 75);
        // Pre-1951 cohorts collapse to 73 (documented decision).
        assert_eq!(rmd_start_age(1940), 73);
        assert_eq!(rmd_start_age(1950), 73);
        // Post-1960 cohorts stay 75.
        assert_eq!(rmd_start_age(1961), 75);
        assert_eq!(rmd_start_age(1990), 75);
    }

    #[test]
    fn uniform_lifetime_divisors_sample_and_tail() {
        // Representative sample of the printed rows (Pub. 590-B, 2025, Table III).
        assert_eq!(rmd_divisor(73), 26.5);
        assert_eq!(rmd_divisor(80), 20.2);
        assert_eq!(rmd_divisor(90), 12.2);
        // The lowest age rmd_divisor is ever asked for (RMD_START_FLOOR).
        assert_eq!(RMD_START_FLOOR, 73);
        assert_eq!(rmd_divisor(RMD_START_FLOOR), 26.5);
        // The last printed row is "120 and over" → 2.0; the tail rule reuses it.
        assert_eq!(rmd_divisor(120), 2.0);
        assert_eq!(rmd_divisor(121), 2.0);
        assert_eq!(rmd_divisor(200), 2.0);
        // Every printed divisor is strictly positive (a non-positive divisor
        // would turn an RMD into a deposit).
        assert!(UNIFORM_LIFETIME_DIVISORS.iter().all(|d| *d > 0.0));
        // The table is indexed by age − RMD_START_FLOOR, so the last entry
        // corresponds to age RMD_TABLE_TAIL_AGE.
        assert_eq!(
            UNIFORM_LIFETIME_DIVISORS[(RMD_TABLE_TAIL_AGE - RMD_START_FLOOR) as usize],
            2.0
        );
    }

    #[test]
    fn uniform_lifetime_divisors_match_the_printed_table_row_for_row() {
        // The full 48-row sequence, ages 73..=120, transcribed from Pub. 590-B
        // (2025), Appendix B, Table III (Uniform Lifetime). Age 72 (27.4) is
        // intentionally omitted: RMD never starts before age 73, so it is not
        // reachable through rmd_divisor and RMD_START_FLOOR indexes at 73.
        let expected: [(u32, f64); 48] = [
            (73, 26.5), (74, 25.5), (75, 24.6), (76, 23.7), (77, 22.9), (78, 22.0),
            (79, 21.1), (80, 20.2), (81, 19.4), (82, 18.5), (83, 17.7), (84, 16.8),
            (85, 16.0), (86, 15.2), (87, 14.4), (88, 13.7), (89, 12.9), (90, 12.2),
            (91, 11.5), (92, 10.8), (93, 10.1), (94, 9.5), (95, 8.9), (96, 8.4),
            (97, 7.8), (98, 7.3), (99, 6.8), (100, 6.4), (101, 6.0), (102, 5.6),
            (103, 5.2), (104, 4.9), (105, 4.6), (106, 4.3), (107, 4.1), (108, 3.9),
            (109, 3.7), (110, 3.5), (111, 3.4), (112, 3.3), (113, 3.1), (114, 3.0),
            (115, 2.9), (116, 2.8), (117, 2.7), (118, 2.5), (119, 2.3), (120, 2.0),
        ];
        assert_eq!(UNIFORM_LIFETIME_DIVISORS.len(), expected.len());
        for (age, divisor) in expected {
            assert_eq!(
                rmd_divisor(age),
                divisor,
                "age {age} diverges from the printed Table III"
            );
        }
    }

    #[test]
    fn accumulate_zero_years_to_retirement_returns_buckets_unchanged() {
        // Current age 46, target retirement age 40 → zero accumulation years.
        let mut req = test_request();
        req.target_retirement_age = 40;
        let out = accumulate(&req, &[0.05; 3]);
        assert_eq!(out, req.starting_buckets);
    }

    #[test]
    fn accumulate_zero_return_grows_by_contributions_only() {
        let mut req = test_request();
        req.target_retirement_age = 48; // 2 accumulation years (age 46 → 48)
        let out = accumulate(&req, &[0.0, 0.0]);
        // 0% real each year: growth is contributions only.
        // Own pre-tax 6% of 100k/yr, Roth 3% of 100k/yr. No employer match.
        assert_eq!(out.pre_tax, 100_000.0 + 2.0 * 6_000.0);
        assert_eq!(out.roth, 50_000.0 + 2.0 * 3_000.0);
        assert_eq!(out.taxable, 25_000.0);
    }

    #[test]
    fn accumulate_zero_contributions_grows_by_return_only() {
        let mut req = test_request();
        req.target_retirement_age = 48;
        req.contribution_rate_pre_tax = 0.0;
        req.contribution_rate_roth = 0.0;
        let out = accumulate(&req, &[0.10, 0.10]);
        assert_eq!(out.pre_tax, 100_000.0 * 1.1 * 1.1);
        assert_eq!(out.roth, 50_000.0 * 1.1 * 1.1);
        assert_eq!(out.taxable, 25_000.0 * 1.1 * 1.1);
    }

    #[test]
    fn accumulate_contributions_earn_no_return_in_their_year() {
        // Return applies FIRST to the start-of-year balance; contributions are
        // added after, earning nothing in their own year.
        let mut req = test_request();
        req.target_retirement_age = 47; // 1 accumulation year
        let out = accumulate(&req, &[0.10]);
        // 100k × 1.10 + 6000, NOT (100k + 6000) × 1.10.
        assert_eq!(out.pre_tax, 100_000.0 * 1.1 + 6_000.0);
        assert_eq!(out.roth, 50_000.0 * 1.1 + 3_000.0);
        assert_eq!(out.taxable, 25_000.0 * 1.1);
    }

    fn employer_after_one_year(total_pct: f64, formula: &EmployerMatch) -> f64 {
        let mut req = test_request();
        req.target_retirement_age = 47;
        req.contribution_rate_pre_tax = total_pct;
        req.contribution_rate_roth = 0.0;
        req.employer_match_formula = formula.clone();
        accumulate_state(&req, &[0.0]).pre_tax_employer
    }

    #[test]
    fn accumulate_employer_match_is_employer_match_on_the_percent_total() {
        // §21 worked example: "100% of the first 3%, then 50% of the next 2%"
        // on a $100,000 gross. The total employee percent is passed PERCENT-scaled
        // (5.0, not 0.05) — passing the fraction understates the match by 100×.
        let formula = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 5.0, match_pct: 50.0 },
            ],
            annual_dollar_cap: None,
        };
        let s = employer_after_one_year(5.0, &formula);
        assert_eq!(s, 4_000.0);
        // The 100× trap, pinned: the SAME formula fed 0.05 instead of 5.0 gives $50.
        assert_eq!(crate::retirement::employer_match(100_000.0, 5.0, &formula), 4_000.0);
        assert_eq!(crate::retirement::employer_match(100_000.0, 0.05, &formula), 50.0);
        // A 10% total still yields $4000 — the excess above the top tier is unmatched.
        assert_eq!(employer_after_one_year(10.0, &formula), 4_000.0);
    }

    #[test]
    fn accumulate_employer_match_at_below_and_above_tier_boundaries() {
        let formula = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 5.0, match_pct: 50.0 },
            ],
            annual_dollar_cap: None,
        };
        assert_eq!(employer_after_one_year(2.0, &formula), 2_000.0); // below tier 1
        assert_eq!(employer_after_one_year(3.0, &formula), 3_000.0); // on the tier-1 boundary
        assert_eq!(employer_after_one_year(4.0, &formula), 3_500.0); // 3% + 1% × 50%
        assert_eq!(employer_after_one_year(5.0, &formula), 4_000.0); // on the tier-2 boundary
        assert_eq!(employer_after_one_year(10.0, &formula), 4_000.0); // above the top tier
        // The dollar cap binds and is honored through the engine's accumulation.
        let capped = EmployerMatch { annual_dollar_cap: Some(1_000.0), ..formula };
        assert_eq!(employer_after_one_year(5.0, &capped), 1_000.0);
    }

    #[test]
    fn accumulate_tracks_pre_tax_and_roth_separately_end_to_end() {
        let mut req = test_request();
        req.target_retirement_age = 47; // 1 year, 0% return
        let s = accumulate_state(&req, &[0.0]);
        // 6% pre-tax → own; 3% Roth → roth; neither leaks into the other.
        assert_eq!(s.pre_tax_own, 100_000.0 + 6_000.0);
        assert_eq!(s.pre_tax_employer, 0.0);
        assert_eq!(s.roth, 50_000.0 + 3_000.0);
        assert_eq!(s.taxable, 25_000.0);
        // The public Buckets view combines the pre-tax sub-accounts.
        let out = accumulate(&req, &[0.0]);
        assert_eq!(out.pre_tax, s.pre_tax_own + s.pre_tax_employer);
        assert_eq!(out.roth, s.roth);
    }

    #[test]
    fn accumulate_match_dollars_land_in_employer_never_own() {
        let formula = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 }],
            annual_dollar_cap: None,
        };
        let mut req = test_request();
        req.target_retirement_age = 47;
        req.contribution_rate_pre_tax = 3.0;
        req.contribution_rate_roth = 0.0;
        req.employer_match_formula = formula;
        let s = accumulate_state(&req, &[0.0]);
        // pre_tax_own carries ONLY the opening balance + employee contributions;
        // every match dollar is in pre_tax_employer.
        assert_eq!(s.pre_tax_own, 100_000.0 + 3_000.0);
        assert_eq!(s.pre_tax_employer, 3_000.0);
    }

    #[test]
    fn deterministic_accumulate_is_accumulate_with_constant_returns() {
        let mut req = test_request();
        req.target_retirement_age = 48; // 2 years
        req.expected_real_return = 5.0;
        req.contribution_rate_pre_tax = 0.0;
        req.contribution_rate_roth = 0.0;
        let constant = vec![0.05, 0.05];
        assert_eq!(deterministic_accumulate(&req), accumulate(&req, &constant));
        // And the value itself: 100k × 1.05² = 110250.
        assert_eq!(deterministic_accumulate(&req).pre_tax, 100_000.0 * 1.05 * 1.05);
    }

    #[test]
    fn employer_match_formula_round_trips_through_serde() {
        // The engine accepts #465's EmployerMatch directly; the input side stays
        // lenient (that leniency is correct for input, strict only for storage).
        let formula = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 }],
            annual_dollar_cap: Some(5_000.0),
        };
        let json = serde_json::to_value(&formula).unwrap();
        let back: EmployerMatch = serde_json::from_value(json).unwrap();
        assert_eq!(back, formula);
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    fn tax20() -> TaxModel {
        TaxModel { effective_rate: 0.2 }
    }

    /// Retire immediately (target = current age 46) with zero contributions, so
    /// decumulation starts at today and only decumulation mechanics matter.
    fn retire_now(req: &mut ProjectionRequest) {
        req.target_retirement_age = 46;
        req.contribution_rate_pre_tax = 0.0;
        req.contribution_rate_roth = 0.0;
    }

    #[test]
    fn decumulate_with_no_pre_tax_and_zero_target_pays_zero_and_never_depletes() {
        // No pre-tax balance means no RMD ever; target 0 means no needs-driven
        // withdrawal ever. Every year nets exactly zero, and no year depletes.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets.pre_tax = 0.0;
        let outcomes = decumulate(&req, &vec![0.0; 45], 0.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 45); // ages 46..=90
        assert!(outcomes.iter().all(|o| !o.depleted));
        assert!(outcomes.iter().all(|o| o.net_income == 0.0));
    }

    #[test]
    fn decumulate_rmd_starts_at_the_secure_2_point_0_age_and_net_income_meets_target() {
        // Birth 1960 → RMDs start at 75. The first decumulation year IS 75, so
        // the RMD fires immediately, is taxed at 20%, and — with target 0 — is
        // the year's entire net income. The next year's RMD is smaller because
        // it is computed on the reduced balance.
        let mut req = test_request();
        req.birth_date = d(1960, 6, 15);
        req.target_retirement_age = 75; // current age 66 → 9 accumulation years
        req.life_expectancy_age = 76;
        req.contribution_rate_pre_tax = 0.0;
        req.contribution_rate_roth = 0.0;
        req.starting_buckets = Buckets { pre_tax: 100_000.0, roth: 0.0, taxable: 0.0 };
        // Horizon = 9 accumulation + 2 decumulation years.
        let outcomes = decumulate(&req, &vec![0.0; 11], 0.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|o| !o.depleted));
        let r0 = 100_000.0 / rmd_divisor(75);
        let r1 = (100_000.0 - r0) / rmd_divisor(76);
        assert_close(outcomes[0].net_income, r0 * 0.8, "age-75 net RMD");
        assert_close(outcomes[1].net_income, r1 * 0.8, "age-76 net RMD");
        // No RMD ever fires before the start age (no pre-tax RMD at age 62 here:
        // the loop simply never reaches age 75 in the no-pre-tax test above).
        assert!(rmd_start_age(1960) == 75);
    }

    #[test]
    fn decumulate_marks_the_first_short_year_depleted_and_stops() {
        // Target $1M against a $175k portfolio: the very first year cannot be
        // met. Everything extractable is taken (taxable first, then grossed-up
        // pre-tax, then Roth), the year is marked depleted, and the vector ends.
        let mut req = test_request();
        retire_now(&mut req);
        let outcomes = decumulate(&req, &vec![0.0; 45], 1_000_000.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].depleted);
        // 25k taxable (untaxed) + 100k pre-tax × 0.8 + 50k Roth = 155k net.
        assert_eq!(outcomes[0].net_income, 155_000.0);
    }

    #[test]
    fn decumulate_draws_taxable_then_pre_tax_then_roth_and_depletes() {
        // Target $60k/yr against 100k pre-tax / 50k Roth / 25k taxable.
        // Year 1: all 25k taxable, then 35k net from grossed-up pre-tax.
        // Year 2: 60k from pre-tax + Roth (pre-tax runs out mid-way).
        // Year 3: Roth (35k left) cannot meet 60k → depleted.
        let mut req = test_request();
        retire_now(&mut req);
        let outcomes = decumulate(&req, &vec![0.0; 45], 60_000.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 3);
        assert_eq!(
            outcomes.iter().map(|o| (o.net_income, o.depleted)).collect::<Vec<_>>(),
            vec![(60_000.0, false), (60_000.0, false), (35_000.0, true)]
        );
    }

    #[test]
    fn decumulate_receives_social_security_after_the_claiming_age() {
        // Claiming at 62 (quoted at 62) → adjusted monthly benefit is exactly
        // the entered $2,400 → $28,800/yr, carried in today's dollars with no
        // inflation (the #466 COLA contract). Before 62 the buckets cover a
        // $20k target; from 62 on SS alone exceeds it, so net_income is exactly
        // the SS figure and the pre-tax balance is never touched.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 500_000.0, roth: 250_000.0, taxable: 125_000.0 };
        req.ss = Some(SsInput {
            monthly_benefit: 2_400.0,
            quoted_at_age_months: 744, // age 62
            claiming_age_months: 744,  // age 62
            birth_date: d(1980, 6, 15),
        });
        let outcomes = decumulate(&req, &vec![0.0; 45], 20_000.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 45);
        assert!(outcomes.iter().all(|o| !o.depleted));
        assert_eq!(outcomes[15].net_income, 20_000.0); // age 61, pre-tax year
        assert_eq!(outcomes[16].net_income, 28_800.0); // age 62, SS starts
        // Ages 63..=74: SS alone (28.8k) exceeds the 20k target, no RMD yet —
        // birth 1980 puts the SECURE 2.0 RMD start at 75, so 73/74 are also SS-only.
        assert!(outcomes[17..29].iter().all(|o| o.net_income == 28_800.0));
        // Age 75+: the RMD adds on top of SS (it is mandatory income), so the
        // year nets MORE than SS alone — never less, never depleted.
        assert!(outcomes[29..].iter().all(|o| o.net_income > 28_800.0 && !o.depleted));
    }

    #[test]
    fn decumulate_applies_the_return_to_the_pre_rmd_balance() {
        // Return applies FIRST inside a decumulation year too: the RMD is
        // computed on the POST-growth balance. 10% for all 9 accumulation years
        // AND the first decumulation year → pre-tax at RMD time is 100k × 1.1^10.
        let mut req = test_request();
        req.birth_date = d(1960, 6, 15);
        req.target_retirement_age = 75;
        req.life_expectancy_age = 75;
        req.contribution_rate_pre_tax = 0.0;
        req.contribution_rate_roth = 0.0;
        req.starting_buckets = Buckets { pre_tax: 100_000.0, roth: 0.0, taxable: 0.0 };
        let outcomes = decumulate(&req, &vec![0.10; 10], 0.0, &tax20()).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_close(
            outcomes[0].net_income,
            100_000.0 * 1.1_f64.powi(10) / rmd_divisor(75) * 0.8,
            "net RMD on the post-growth balance",
        );
    }

    #[test]
    fn deterministic_decumulate_is_decumulate_with_constant_returns() {
        let req = test_request();
        let tax = tax20();
        // The constant series must span the WHOLE horizon (19 accumulation
        // years at age 46 → 65, then decumulation to 90), exactly as
        // deterministic_decumulate builds it.
        let accum = accumulation_years(&req);
        let horizon = (accum + req.life_expectancy_age - req.target_retirement_age + 1) as usize;
        let constant = vec![req.expected_real_return / 100.0; horizon];
        let expected = decumulate(&req, &constant, 60_000.0, &tax).unwrap();
        assert_eq!(deterministic_decumulate(&req, 60_000.0, &tax).unwrap(), expected);
    }

    #[test]
    fn target_run_with_depletion_reports_the_age_and_survival() {
        // test_request (100k/50k/25k, retire immediately): the 75k/yr target
        // (gross × 0.75 ratio) depletes in the third year → depletion age 48.
        let mut req = test_request();
        retire_now(&mut req);
        assert_eq!(target_run_with_depletion(&req).unwrap(), (true, Some(48)));

        // A large-enough portfolio survives: 10M pre-tax against the 75k target
        // is never depleted, and from age 75 on the RMD itself covers the target.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 10_000_000.0, roth: 100_000.0, taxable: 100_000.0 };
        assert_eq!(target_run_with_depletion(&req).unwrap(), (false, None));
    }

    #[test]
    fn decumulate_surfaces_an_out_of_range_ss_age_as_invalid_input() {
        // #466's claiming_adjustment REJECTS a claiming age above 70 rather than
        // clamping; the engine must surface that refusal as InvalidInput, never
        // silently recompute a different figure.
        let mut req = test_request();
        retire_now(&mut req);
        req.ss = Some(SsInput {
            monthly_benefit: 2_400.0,
            quoted_at_age_months: 744,
            claiming_age_months: 900, // age 75 — above the statutory maximum
            birth_date: d(1980, 6, 15),
        });
        let err = decumulate(&req, &vec![0.0; 45], 20_000.0, &tax20()).unwrap_err();
        assert!(matches!(&err, ProjectionError::InvalidInput(m) if m.contains("62 and 70")), "{err}");
        assert!(matches!(deterministic_decumulate(&req, 20_000.0, &tax20()), Err(_)));
    }

    #[test]
    fn split_rmd_and_take_pre_tax_preserve_the_own_employer_split() {
        // RMDs split proportionally (own/employer) so neither sub-account is
        // over-drawn; needs-driven withdrawals go OWN first, then employer.
        let s = SimState { pre_tax_own: 127_000.0, pre_tax_employer: 27_000.0, roth: 0.0, taxable: 0.0 };
        let gross = 154_000.0 / rmd_divisor(75);
        let (own, employer) = s.split_rmd(gross);
        assert_close(own, gross * 127_000.0 / 154_000.0, "own RMD share");
        assert_close(employer, gross * 27_000.0 / 154_000.0, "employer RMD share");
        assert_close(own + employer, gross, "split sums to the gross");

        let mut s = SimState { pre_tax_own: 100.0, pre_tax_employer: 50.0, roth: 0.0, taxable: 0.0 };
        assert_eq!(s.take_pre_tax(80.0), (80.0, 0.0));
        assert_eq!(s.pre_tax_own, 20.0);
        assert_eq!(s.pre_tax_employer, 50.0);
        assert_eq!(s.take_pre_tax(80.0), (20.0, 50.0));
        assert_eq!(s.pre_tax_own, 0.0);
        assert_eq!(s.pre_tax_employer, 0.0); // 150 total taken in 160 → all gone
    }

    // ------------------------------------------------------------------
    // Task 6: sustainable_annual_w (bisection) + IncomeSources + run_projection
    // ------------------------------------------------------------------

    #[test]
    fn sustainable_annual_w_exhausts_a_two_year_portfolio_exactly() {
        // 100k pre-tax, 20% tax, zero return, two decumulation years (46, 47):
        // each year's gross withdrawal is w/0.8, so 2 × w/0.8 = 100 → w = 40.
        // ZERO return: the deterministic series otherwise applies the default
        // 5% to the decumulation years too, which would break the exact math.
        let mut req = test_request();
        retire_now(&mut req);
        req.expected_real_return = 0.0;
        req.life_expectancy_age = 47;
        req.starting_buckets = Buckets { pre_tax: 100.0, roth: 0.0, taxable: 0.0 };
        req.w_max = 200.0;
        assert_close(sustainable_annual_w(&req, &tax20()).unwrap(), 40.0, "sustainable w");
    }

    #[test]
    fn sustainable_withdrawal_caps_at_w_max_for_pathological_oversavings() {
        // 5M pre-tax can sustain ~220k/yr (well above the 75k target), so a
        // $100k bound is the binding constraint — the solve is CAPPED at w_max
        // (documented), never the true value, yet still >100% of goal.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 5_000_000.0, roth: 0.0, taxable: 0.0 };
        req.w_max = 100_000.0;
        let w = sustainable_annual_w(&req, &tax20()).unwrap();
        assert_close(w, 100_000.0, "capped at w_max");
        let p = run_projection(&req).unwrap();
        assert!(p.percent_of_goal.unwrap() > 1.0);
        assert_eq!(p.assumptions.w_max, 100_000.0);
        assert_eq!(p.depletes, false);
    }

    #[test]
    fn percent_of_goal_is_none_when_the_target_monthly_income_is_zero() {
        // Zero gross income → zero target → the fraction is undefined (None),
        // not an infinite/NaN percent.
        let mut req = test_request();
        retire_now(&mut req);
        req.current_gross_income = 0.0;
        let p = run_projection(&req).unwrap();
        assert_eq!(p.percent_of_goal, None);
    }

    #[test]
    fn overfunded_plan_reports_more_than_full_percent_and_no_depletion() {
        // 5M pre-tax against a $75k/yr target: sustainable w ≈ 89k > target, so
        // the dial exceeds 100%, the target run survives, and there is no gap.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 5_000_000.0, roth: 0.0, taxable: 0.0 };
        req.w_max = 1_000_000.0;
        let p = run_projection(&req).unwrap();
        let percent = p.percent_of_goal.unwrap();
        assert!(percent > 1.0, "expected >100%, got {percent}");
        assert_eq!(p.depletes, false);
        assert_eq!(p.depletion_age, None);
        assert_eq!(p.income_sources.gap, 0.0);
    }

    #[test]
    fn underfunded_plan_reports_partial_percent_a_gap_and_depletion() {
        // 500k pre-tax against the same $75k/yr target: sustainable w ≈ 9k
        // (≈12% of target), the target run exhausts the 500k at age 51
        // (5 full years × 93.75k gross + a 6th partial year), and the sources
        // plus gap reconstruct the monthly target exactly. ZERO return so the
        // hand arithmetic is exact.
        let mut req = test_request();
        retire_now(&mut req);
        req.expected_real_return = 0.0;
        req.starting_buckets = Buckets { pre_tax: 500_000.0, roth: 0.0, taxable: 0.0 };
        let p = run_projection(&req).unwrap();
        let percent = p.percent_of_goal.unwrap();
        assert!(percent < 1.0, "expected <100%, got {percent}");
        assert_eq!(p.depletes, true);
        assert_eq!(p.depletion_age, Some(51));
        assert!(p.income_sources.gap > 0.0);
        let s = &p.income_sources;
        assert_close(
            s.own_savings + s.employer_contributions + s.social_security + s.other_assets + s.gap,
            100_000.0 * 0.75 / 12.0,
            "sources + gap reconstruct the monthly target",
        );
    }

    #[test]
    fn income_sources_from_run_net_pre_tax_and_reconstruct_the_target() {
        // Hand-built one-year run: 20 taxable + 50 own gross + 50 employer gross
        // + 10 Roth, 20% rate. own = 50×0.8 + 10 = 50 net; employer = 40 net;
        // taxable = 20. Monthly (÷12). Gap fills to the 6250 target.
        let outcomes = vec![YearOutcome {
            net_income: 80.0,
            depleted: false,
            ss_received: 0.0,
            taxable_taken: 20.0,
            pre_tax_own_taken: 50.0,
            pre_tax_employer_taken: 50.0,
            roth_taken: 10.0,
        }];
        let s = income_sources_from_run(&outcomes, 0.0, &tax20(), 6250.0);
        assert_close(s.own_savings, 50.0 / 12.0, "own (net of tax + Roth)");
        assert_close(s.employer_contributions, 40.0 / 12.0, "employer (net of tax)");
        assert_close(s.other_assets, 20.0 / 12.0, "taxable");
        assert_eq!(s.social_security, 0.0);
        assert_close(
            s.own_savings + s.employer_contributions + s.other_assets + s.social_security + s.gap,
            6250.0,
            "sources + gap == target monthly",
        );
    }

    #[test]
    fn income_sources_include_social_security_as_the_adjusted_monthly_rate() {
        // A sustainable run with SS pays $2,400/mo from the claiming age; the
        // source segment is that ADJUSTED monthly rate, not an average diluted
        // by the pre-claiming years.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 500_000.0, roth: 250_000.0, taxable: 125_000.0 };
        req.ss = Some(SsInput {
            monthly_benefit: 2_400.0,
            quoted_at_age_months: 744,
            claiming_age_months: 744,
            birth_date: d(1980, 6, 15),
        });
        let p = run_projection(&req).unwrap();
        assert_close(p.income_sources.social_security, 2_400.0, "adjusted monthly benefit");
        assert_eq!(p.assumptions.ss_claiming_age_used, Some(62));
        assert_eq!(p.assumptions.ss_adjusted_monthly_benefit, Some(2_400.0));
        assert_close(
            p.deterministic_monthly_income,
            p.assumptions.sustainable_annual_w / 12.0 + 2_400.0,
            "headline is sustainable w + SS",
        );
    }

    #[test]
    fn percent_of_goal_matches_the_headline_over_the_monthly_target() {
        let mut req = test_request();
        retire_now(&mut req);
        req.life_expectancy_age = 47;
        req.starting_buckets = Buckets { pre_tax: 100.0, roth: 0.0, taxable: 0.0 };
        req.w_max = 200.0;
        let p = run_projection(&req).unwrap();
        // Self-consistency, independent of the return chosen: the dial is the
        // headline over the monthly target, and the headline is sustainable w
        // plus (here, zero) Social Security.
        let target_monthly = req.current_gross_income * req.target_replacement_ratio / 12.0;
        let expected = p.assumptions.sustainable_annual_w / 12.0 / target_monthly;
        assert_close(p.percent_of_goal.unwrap(), expected, "percent_of_goal");
        assert_close(
            p.deterministic_monthly_income,
            p.assumptions.sustainable_annual_w / 12.0,
            "headline monthly",
        );
    }

    #[test]
    fn assumptions_echo_the_request_and_the_derived_values() {
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 300_000.0, roth: 100_000.0, taxable: 50_000.0 };
        req.mc_seed = 7;
        req.mc_paths = 123;
        req.w_max = 400_000.0;
        let p = run_projection(&req).unwrap();
        let a = &p.assumptions;
        assert_eq!(a.current_age, 46);
        assert_eq!(a.rmd_start_age, 75); // born 1980 → SECURE 2.0 start 75
        assert_eq!(a.mc_sigma, MC_REAL_RETURN_STD_DEV);
        assert_eq!(a.bisection_iterations, BISECTION_ITERATIONS);
        assert_eq!(a.current_date, req.today);
        assert_eq!(a.ss_adjusted_monthly_benefit, None);
        assert_eq!(a.ss_claiming_age_used, None);
        assert_eq!(a.starting_buckets, req.starting_buckets);
        assert_eq!(a.birth_date, req.birth_date);
        assert_eq!(a.target_retirement_age, 46);
        assert_eq!(a.life_expectancy_age, 90);
        assert_eq!(a.current_gross_income, 100_000.0);
        assert_eq!(a.contribution_rate_pre_tax, 0.0);
        assert_eq!(a.contribution_rate_roth, 0.0);
        assert_eq!(a.expected_real_return, 5.0);
        assert_eq!(a.target_replacement_ratio, 0.75);
        assert_eq!(a.effective_tax_rate, 0.2);
        assert_eq!(a.mc_seed, 7);
        assert_eq!(a.mc_paths, 123);
        assert_eq!(a.w_max, 400_000.0);
        assert!(a.sustainable_annual_w > 0.0);
        // The echoed match formula round-trips through serde like the input.
        let json = serde_json::to_value(&a.employer_match_formula).unwrap();
        let back: crate::retirement::EmployerMatch = serde_json::from_value(json).unwrap();
        assert_eq!(back, req.employer_match_formula);
    }

    #[test]
    fn engine_constants_are_pinned() {
        assert_eq!(BISECTION_ITERATIONS, 40);
        assert_eq!(MC_REAL_RETURN_STD_DEV, 0.1346);
        assert_eq!(MC_DEFAULT_SEED, 0x4e454c53); // "NELS" — fixed production default
        // Pinned against the measured runtime budget — see the constant's own
        // doc comment for the benchmark table. Changing it changes both the
        // band's stability and the endpoint's latency, so it is a deliberate
        // act that must also update those numbers.
        assert_eq!(MC_DEFAULT_PATHS, 1000);
        assert_eq!(DEFAULT_W_MAX, 10_000_000.0);
        assert_eq!(DEFAULT_EFFECTIVE_TAX_RATE, 0.15);
    }

    // ------------------------------------------------------------------
    // Task 7: monte_carlo — seeded lognormal, percentiles, success rate
    // ------------------------------------------------------------------

    #[test]
    fn mc_log_return_mean_is_ln_1_plus_r_minus_sigma_squared_over_two() {
        // r is the PERCENT→fraction conversion (0.05 = 5%); the log-mean that
        // makes the lognormal multiplier's geometric mean come out to 1 + r.
        let sigma = MC_REAL_RETURN_STD_DEV;
        let expected = 1.05f64.ln() - (sigma * sigma) / 2.0;
        assert_close(log_return_mean(0.05, sigma), expected, "log-mean");
        // Degenerate: zero return AND zero volatility → geometric mean exactly 1.
        assert_close(log_return_mean(0.0, 0.0), 0.0, "zero degenerate");
    }

    #[test]
    fn mc_same_seed_reproduces_identical_band_and_success_rate() {
        let req = test_request();
        let (band_a, rate_a) = monte_carlo(&req).unwrap();
        let (band_b, rate_b) = monte_carlo(&req).unwrap();
        assert_eq!(band_a, band_b, "same seed → byte-identical percentile band");
        assert_eq!(rate_a, rate_b, "same seed → identical success rate");
    }

    #[test]
    fn mc_fixed_seed_draws_are_stable_at_the_module_boundary() {
        // Two StdRngs with the same seed draw the same series; a different seed
        // draws a different one. This is the reproducibility guarantee the
        // projection UI depends on (recompute == identical card).
        let normal = Normal::new(0.04, MC_REAL_RETURN_STD_DEV).unwrap();
        let draw = |seed: u64| {
            let mut rng = StdRng::seed_from_u64(seed);
            (0..20).map(|_| normal.sample(&mut rng)).collect::<Vec<_>>()
        };
        assert_eq!(draw(1234), draw(1234), "same seed → identical draws");
        assert_ne!(draw(1234), draw(9999), "different seed → different draws");
    }

    #[test]
    fn mc_percentiles_are_ordered() {
        let (band, _) = monte_carlo(&test_request()).unwrap();
        assert!(band.p10 <= band.p25, "p10 {:.4} <= p25 {:.4}", band.p10, band.p25);
        assert!(band.p25 <= band.p50, "p25 {:.4} <= p50 {:.4}", band.p25, band.p50);
        assert!(band.p50 <= band.p75, "p50 {:.4} <= p75 {:.4}", band.p50, band.p75);
        assert!(band.p75 <= band.p90, "p75 {:.4} <= p90 {:.4}", band.p75, band.p90);
    }

    #[test]
    fn mc_success_rate_is_100_for_overfunded_and_0_for_underfunded() {
        // Trivially over-funded: every path reaches life expectancy undepleted.
        // 500M, not 5M: with REAL volatility (the `exp(z) - 1.0` fix) a 5M
        // portfolio still lets ONE tail path out of 100 deplete, so the
        // "trivially overfunded ⇒ rate 1.0" contract needs a figure no
        // realistic 45-year draw can exhaust.
        let mut over = test_request();
        retire_now(&mut over);
        over.starting_buckets = Buckets { pre_tax: 500_000_000.0, roth: 0.0, taxable: 0.0 };
        let (_, rate_over) = monte_carlo(&over).unwrap();
        assert_eq!(rate_over, 1.0);

        // Trivially under-funded: even a great return path depletes in year 1.
        let mut under = test_request();
        retire_now(&mut under);
        under.starting_buckets = Buckets { pre_tax: 50_000.0, roth: 0.0, taxable: 0.0 };
        let (_, rate_under) = monte_carlo(&under).unwrap();
        assert_eq!(rate_under, 0.0);
    }

    #[test]
    fn mc_zero_paths_is_invalid_input() {
        let mut req = test_request();
        req.mc_paths = 0;
        match monte_carlo(&req) {
            Err(ProjectionError::InvalidInput(_)) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn mc_sequence_risk_front_loaded_bad_returns_sustain_less() {
        // The reason the `&[f64]` seam is mandatory: the SAME set of returns in a
        // different ORDER yields a different sustainable figure. Five losing
        // years early in retirement (when the balance is largest and every
        // withdrawal is a higher fraction of it) sustain strictly less than the
        // same five losing years at the very end.
        let mut req = test_request();
        retire_now(&mut req);
        req.starting_buckets = Buckets { pre_tax: 1_000_000.0, roth: 0.0, taxable: 0.0 };
        req.w_max = 200_000.0;
        let n = horizon_years(&req) as usize;
        let front_loaded: Vec<f64> =
            (0..n).map(|i| if i < 5 { -0.25 } else { 0.08 }).collect();
        let back_loaded: Vec<f64> =
            (0..n).map(|i| if i >= n - 5 { -0.25 } else { 0.08 }).collect();
        let tax = tax20();
        let w_front = sustainable_annual_w_for_series(&req, &front_loaded, &tax, 0.0);
        let w_back = sustainable_annual_w_for_series(&req, &back_loaded, &tax, 0.0);
        assert!(
            w_front < w_back,
            "front-loaded bad returns sustain {w_front} < back-loaded {w_back}"
        );
        assert!(w_front > 0.0, "both series must be survivable, got {w_front}");
    }

    // ------------------------------------------------------------------
    // Task 8: MC band wired into Projection + serialization boundary
    // ------------------------------------------------------------------

    #[test]
    fn mc_band_bounds_the_deterministic_headline_inclusively() {
        // The band is per-path sustainable portfolio income + the same SS figure,
        // so the constant-return headline sits inside it. INCLUSIVE: tied paths
        // make equality legitimate (the critique advisory).
        let p = run_projection(&test_request()).unwrap();
        let b = &p.percentile_band;
        assert!(
            b.p10 <= p.deterministic_monthly_income && p.deterministic_monthly_income <= b.p90,
            "headline {:.2} outside band [{:.2}, {:.2}]",
            p.deterministic_monthly_income,
            b.p10,
            b.p90
        );
    }

    #[test]
    fn projection_serializes_nulls_not_omissions_and_round_trips() {
        // The Option fields serialize as explicit nulls — the UI must be able to
        // tell "None because the target is undefined" from a missing key.
        let mut req = test_request();
        retire_now(&mut req);
        req.current_gross_income = 0.0; // percent_of_goal -> None
        let p = run_projection(&req).unwrap();
        assert_eq!(p.percent_of_goal, None);

        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["percent_of_goal"], serde_json::Value::Null);
        assert_eq!(json["depletion_age"], serde_json::Value::Null);
        assert!(json.get("percentile_band").is_some(), "band key present");
        assert!(json.get("income_sources").is_some(), "sources key present");
        assert!(json.get("assumptions").is_some(), "assumptions key present");

        // Round-trip: deserializing the serialized projection reproduces it.
        let back: Projection = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn projection_assumptions_serialize_mc_defaults_for_the_handlers() {
        // The handlers set mc_seed/mc_paths on the request; the assumptions echo
        // what was actually used.
        let mut req = test_request();
        req.mc_seed = MC_DEFAULT_SEED;
        req.mc_paths = MC_DEFAULT_PATHS;
        let p = run_projection(&req).unwrap();
        assert_eq!(p.assumptions.mc_seed, MC_DEFAULT_SEED);
        assert_eq!(p.assumptions.mc_paths, MC_DEFAULT_PATHS);
        assert_eq!(p.assumptions.mc_sigma, MC_REAL_RETURN_STD_DEV);
    }
}
