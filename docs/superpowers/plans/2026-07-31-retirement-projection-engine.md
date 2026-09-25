# Retirement Projection Engine — Accumulation, Decumulation, Monte Carlo (nels#467) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a pure, fixture-testable retirement projection engine — a deterministic headline monthly income, a seeded Monte Carlo percentile band around it, %-of-goal, an income-source decomposition for the stacked bar, a depletion age, and the full assumption set that produced the numbers — surfaced via `GET`/`POST /api/retirement/projection` and a read-only chat `RETIREMENT_PROJECTION` card. The engine is the highest-value TDD unit in epic #454: it has no `sqlx`/`reqwest`/`env`/clock, computes in today's dollars at real returns, and is consumed identically by REST and chat so the two surfaces cannot disagree.

**Architecture:** A new pure module `backend/src/retirement_projection.rs` (NOT the existing `retirement.rs` — see Decisions) owns all math behind one `ProjectionRequest` input struct and one `Projection` output struct. Three core sim functions (`accumulate`, `decumulate`, `monte_carlo`) share a single year-loop shape that accepts a `&[f64]` per-year real-return series — the mandatory seam that makes the AC's sequence-risk test writable. `decumulate` models RMDs (static IRS Uniform Lifetime Table), withdrawal ordering (taxable → pre-tax → Roth), a `TaxModel` struct (single user-editable effective rate, not brackets), and the Social Security stream via `social_security::claiming_adjustment`. `sustainable_annual_w` (bisection) produces the headline; `target_run_with_depletion` produces `depletes`/`depletion_age`. REST handlers in `retirement.rs` assemble inputs from DB rows + today and serialize `Projection`; the chat arm in `rag.rs` runs the same path, renders a fixed card, and attaches the structured `Projection` to `ChatResponse`. Both surfaces are Pro-gated via `require_caller_tier`. The offline "forecast" branch (`rag.rs:2271-2272`, currently a fabricated `$100.00` string) is repointed at the engine for retirement-phrased queries.

**Tech Stack:** Rust, axum, sqlx, `rand` 0.8 (existing) + `rand_distr` 0.4 (new, for `Normal`). Engine unit tests are always-run; handler/DB tests are `#[ignore]`; runtime bench is `#[ignore]`.

**Reference spec:** `docs/superpowers/specs/2026-07-31-retirement-projection-engine-design.md` (approved; single source of truth). **Depends on:** #464 (asset/holding types), #465 (profile types + `employer_match`), #466 (`claiming_adjustment`, `SocialSecurityView`) — all merged. **Does NOT depend on** #468 (fixture-built).

## Decisions baked in (from approved spec + investigation)

- **The engine lives in `retirement_projection.rs`, NOT the AC's literal `retirement.rs`.** Post-#465, `retirement.rs` is a 4000+ line `sqlx`-using module; the AC's intent is a pure engine and the literal filename is unsatisfiable without moving #465's handlers (churn, no benefit). Registered as `mod retirement_projection;` in `main.rs`; purity contract stated in module doc, enforced by review (same discipline as `entitlement.rs`).
- **`ProjectionRequest` is the engine's only entry.** All fields `f64`/`u32`/`NaiveDate`/`EmployerMatch`/`SsInput`, `#[derive(Serialize, Deserialize, Clone, Copy, Debug)]` where types allow. The engine imports only pure types from pure modules: `retirement::EmployerMatch`/`MatchTier`, `social_security::claiming_adjustment`. It never constructs a `sqlx::Executor` or reads an env var.
- **Bucket model mirrors #464's `TaxTreatment`:** `PreTax` → `pre_tax`; `Roth` → `roth`; `Taxable`/`Hsa`/`Other` → `taxable` (HSA folding documented). `pre_tax` is internally `pre_tax_own` + `pre_tax_employer` from first contribution to last withdrawal; the OPENING pre-tax balance is attributed entirely to `pre_tax_own`; sim-time employer-match dollars accrue to `pre_tax_employer`. Roth is entirely own; taxable is entirely "other assets".
- **Everything real, annual inside the sim, monthly at the boundary.** `current_gross_income` held constant in real terms (no salary growth — conservative conventional planning figure, not a forecast). Contributions = `gross × rate/100`. Employer match = `retirement::employer_match(gross, pre_tax_rate + roth_rate, formula)` added to `pre_tax_employer`. **Return applies first to the start-of-year balance, then contributions are added** (contributions earn no return in their year — documented convention, consistent across deterministic/MC/tests).
- **Annual time step, whole-year age granularity.** Horizon is `current_age..life_expectancy_age` as attained-age years; `current_age` via pure floored `age_on(today, birth_date)`. Accumulation runs while `current_age + year_index < target_retirement_age`; decumulation from `target_retirement_age` through `life_expectancy_age` inclusive.
- **Decumulation year = forced RMD, then needs-driven withdrawal.** RMD fires when `age >= rmd_start_age(birth_year)` and `pre_tax > 0`: gross `pre_tax / divisor(a)`, split proportionally across the sub-accounts, its NET counting toward the year's need. `rmd_start_age` = 73 for `birth_year <= 1959`, 75 for `>= 1960` (pre-1951 cohorts collapsed to 73, documented). Divisors are a static IRS Uniform Lifetime Table (Pub. 590-B, Table III) `const` array indexed by `age − RMD_START_FLOOR`, tail reuses the last printed divisor. Needs-driven withdrawal: net need = `target_income − ss` (clamped ≥ 0), funded taxable (untaxed) → pre-tax (grossed up by `1/(1 − effective_rate)`, split own/employer) → Roth (untaxed). If RMD already delivered more net than the need, no further withdrawal (genuine surplus; the portfolio, not the target, is exhausted). Depletion: withdraw everything available, clamp balances to `0.0` (never negative — AC), record the year.
- **SS is a deterministic income stream from the claiming age onward, in today's dollars, never inflated/deflated.** Uses the real signature `social_security::claiming_adjustment(benefit: f64, quoted_at_age_months: i32, claiming_age_months: i32, birth_date: NaiveDate) -> Result<f64, String>`, then treats `adjusted × 12` as annual income for decumulation years where attained age `>= claiming_age_months / 12` (floored). Partial SS input → income `0.0`, computed normally. SS is NOT taxed.
- **`TaxModel` is a struct, not a trait** (closed-set discipline like `bank_provider::Provider`): `pub struct TaxModel { pub effective_rate: f64 }` (fraction, `0.0..=1.0`) with `pub fn tax_on_withdrawal(&self, kind: TaxedBucket, amount: f64, _year: u32) -> f64` (`0.0` for `Taxable`/`Roth`, `amount × effective_rate` for `PreTax`). `_year` is the seam for a future bracket engine; v1 ships exactly one implementation.
- **Deterministic path uses `expected_real_return` (PERCENT, → fraction) as a constant real annual return every year.** The percent→fraction conversion is pinned by a test (§21/#465 units bullet is load-bearing).
- **MC: lognormal annual real returns, seeded, path-major draw order.** Per-year multiplier from lognormal with log-mean `ln(1 + r) − σ²/2`, `σ = MC_REAL_RETURN_STD_DEV` (single constant for the whole portfolio, regardless of allocation — documented v1 simplification). `StdRng::seed_from_u64(seed)` (`rand` 0.8; `rand_distr = "0.4"` NEW dep for `Normal`). Seed is a REQUIRED field of `ProjectionRequest` — same seed + same inputs = identical output. Draw order: outer loop over paths, inner loop over years ascending. **No `thread_rng`, no entropy, anywhere.** Production handlers use fixed `MC_DEFAULT_SEED`.
- **Success rate** = fraction of paths whose target-withdrawal run reaches `life_expectancy_age` undepleted (over-funded → 100%, under-funded → 0%, AC-pinned). **Percentile band** = percentiles 10/25/50/75/90 of per-path total monthly income = per-path sustainable portfolio monthly income + `ss_monthly` (deterministic, identical across paths), so the band always bounds the deterministic headline by construction. `paths` is a required field; `MC_DEFAULT_PATHS` is the named production default pinned against the measured bench.
- **The headline is the SUSTAINABLE withdrawal, not the target.** `sustainable_annual_w` = constant real ANNUAL net portfolio withdrawal exactly exhausting the portfolio at `life_expectancy_age`, solved by bisection over `w ∈ [0, w_max]` (named constant, sourced, comfortably above any realistic withdrawal) with a fixed named iteration count. **The solve-run is `decumulate` with the trial `w` substituted for `target_income`** (SS still offsets, RMDs still forced) — it does not fix the target and measure shortfall. Then `deterministic_monthly_income = sustainable_annual_w/12 + ss_monthly`, `percent_of_goal = deterministic_monthly_income / target_monthly_income` (`Option<f64>`, `None` when `target_monthly_income <= 0`). **Wire format (spec-review fix): `percent_of_goal` is a FRACTION, never pre-multiplied — `1.24` = 124% — and all monthly figures are dollars; the UI formats, the engine returns raw numbers.** This is why the dial can exceed 100%.
- **`Projection` is `#[derive(Serialize)]` and carries `assumptions: ProjectionAssumptions`** — every input echoed + derived `current_age`/`rmd_start_age`, `σ`, seed/paths used, tax rate, SS adjusted benefit + claiming age used, `sustainable_annual_w` + bisection count, `w_max`, current date. Compliance decision on #454: the UI renders assumptions on the same screen as the number; the engine returning them stops the UI from re-deriving and drifting. `IncomeSources { own_savings, employer_contributions, social_security, other_assets, gap }` (monthly, from the deterministic sustainable run; `gap = max(0, target_monthly − sum)`). `depletion_age: Option<i32>` + `depletes: bool` from the deterministic TARGET-withdrawal run.
- **Degenerate inputs are first-class** (§2.15): no assets = valid contributions-only; no profile = handler-level 404 (engine never sees it); retirement age passed = zero accumulation years; life expectancy <= current age or < retirement age = `ProjectionError::InvalidInput`; 100% Roth / 100% taxable = ordinary bucket shapes; zero gross income → `percent_of_goal = None`; non-finite/out-of-range → `ProjectionError::InvalidInput`. **`is_finite()` is guarded FIRST** — never an ordered comparison (the §20/#464 NaN trap: `NaN <= 0.0` is false). `paths = 0` → `InvalidInput`.
- **REST surface:** `GET /api/retirement/projection` (Pro via `require_caller_tier` — caller's own tier, NOT `require_tier`; no budget in scope) assembles the request from stored profile + assets + today. `POST /api/retirement/projection` accepts `ProjectionOverrides` (slider fields: `contribution_rate_pre_tax`, `contribution_rate_roth`, `expected_real_return`, `target_retirement_age`, `current_gross_income`, `target_replacement_ratio`, `effective_tax_rate`, `employer_match_formula`) merged over the stored profile WITHOUT persisting (no DB write, no audit row — a what-if, not a save), validated (finite/in-range, `ProjectionError::InvalidInput` on violation). Both return the SAME `Projection` shape.
- **Chat surface:** read-only `RETIREMENT_PROJECTION` action; the system prompt forbids editorializing (mirroring the #465/#466 no-advice rule); the arm is Pro-gated, renders a FIXED `build_projection_card_html` card (mirroring `build_categories_table_html`'s shape) as `response_text`, and attaches `ChatResponse.retirement_projection: Option<Projection>` (new field, `skip_serializing_if`) so #469 can render the real card without re-running. The offline forecast branch is NARROWED: retirement-phrasing words (retire/retirement/projection) claim the branch and get the real deterministic figure; existing "forecast"/"next month" budget semantics keep their own arm.
- **No migration. No schema.** Only repo-level change beyond code is `rand_distr = "0.4"` matched to existing `rand = "0.8"` (`backend/Cargo.toml:22`).

## Global Constraints

- Backend is a **bin-only crate**: `cargo test --bin backend`, never `--lib`. DB/handler tests `#[ignore]` (`--ignored`, pgvector 6153). `rg` is NOT installed in this environment — use `grep -n` / `git grep -n`.
- **The engine module must never import `sqlx`, `reqwest`, `std::env`, or `Utc::now()`** — the module doc states the contract and review enforces it. The handlers may touch I/O; the engine may not.
- No self-attribution in commits. No comments unless load-bearing/provenance-required.
- Every default constant is named, source-commented, and pinned by a value-asserting test: `MC_REAL_RETURN_STD_DEV = 0.1346` (US 60/40 real-return volatility 1901–2022, CFA Institute / Dimson-Marsh-Staunton 2023 — source `https://rpc.cfainstitute.org/sites/default/files/docs/research-reports/monash-report-1_performance-of-the-6040_online.pdf`), `MC_DEFAULT_SEED` (fixed), `MC_DEFAULT_PATHS` (placeholder until bench; must stay under the debounced recompute budget), `w_max`, bisection iterations, `rmd_start_age` thresholds (73/75), `TaxModel` bounds (finite, `0.0..=1.0`).
- The 402 status is preserved (frontend `isProGateError` keys on it); `require_caller_tier` returns `402 PAYMENT_REQUIRED` on failure. Do NOT change the existing `SET_RETIREMENT_PROFILE` arm or `offline_retirement_action`'s behavior — only ADD the new read-only projection surface and NARROW the offline forecast branch.
- Seed a test Pro user with `status = 'trialing'`, NEVER `'active'` (§21/#465 test-infra note; `require_caller_tier` resolves through `PriceCatalog::from_env()` whose price vars are unset in tests, so `active` fails safe to Basic/402). `subscriptions.stripe_customer_id` is UNIQUE — bind a per-user id like `cus_test_{user_id}`.
- Deploy gate: none specific to this issue (no migration; new endpoints are additive and Pro-gated). Rides the normal release-gated flow.

## Test ripple

- `retirement.rs` tests that call `get_profile`/`put_profile` are unaffected (no signature changes to those handlers). New handler tests are added, not modified.
- `rag.rs`'s offline tests: `offline_retirement_action_does_not_collide_with_sibling_matchers` (:8627) must keep passing — the NEW projection offline matcher must not steal phrasings that test already assigns to other arms (assert the existing arms AND the new projection arm together). The existing offline "forecast"/"next month" arm's behavior for BUDGET utterances is unchanged; only retirement-phrased messages change.
- `rand_distr` at the module boundary: a fixed seed draws the same series across two runs (pinned); draws are stable (the version is pinned to 0.4).

---

## File Structure

- **Modify:** `backend/Cargo.toml` — add `rand_distr = "0.4"` (next to `rand = "0.8"` at line 22).
- **Create:** `backend/src/retirement_projection.rs` — the pure engine + unit tests + `#[ignore]`d `runtime_benchmarks`.
- **Modify:** `backend/src/main.rs` — `mod retirement_projection;`; `GET`/`POST /api/retirement/projection` in `protected_routes` (:273-378), next to `/retirement/profile` (:377), Pro-gated.
- **Modify:** `backend/src/retirement.rs` — `GET`/`POST` projection handlers (assemble `ProjectionRequest` from DB rows + today; `POST` merges `ProjectionOverrides` without persisting; both call the engine and serialize `Projection`); `#[ignore]`d handler/DB tests.
- **Modify:** `backend/src/rag.rs` — `RETIREMENT_PROJECTION` action arm; `build_projection_card_html`; `ChatResponse.retirement_projection: Option<Projection>` (:94-120, `skip_serializing_if`); system-prompt rule; offline forecast-branch repoint (offline chain :2204-2283); `#[ignore]`d chat tests.
- **Modify:** `AGENTS.md` — new §25 documenting the engine.

---

### Task 1: `rand_distr = "0.4"` dependency

**Files:**
- Modify: `backend/Cargo.toml`

- [ ] **Step 1:** Add `rand_distr = "0.4"` immediately after `rand = "0.8"` (line 22). Run `cargo check` to confirm the lockfile resolves cleanly (no version bump to `rand` itself).

---

### Task 2: Pure engine data types — `Buckets`, `TaxModel`, `SsInput`, `ProjectionRequest`, `ProjectionError`

**Files:**
- Create: `backend/src/retirement_projection.rs` (types only in this task)
- Modify: `backend/src/main.rs` (register `mod retirement_projection;` so the file compiles)

**Interfaces:**
- Consumes: `retirement::{EmployerMatch, MatchTier}`, `social_security::claiming_adjustment` (imported at the call site in Task 5's sim loop), `chrono::NaiveDate`, `rand`/`rand_distr` (later tasks).
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug)] pub struct Buckets { pub pre_tax: f64, pub roth: f64, pub taxable: f64 }`
  - `pub enum TaxedBucket { Taxable, PreTax, Roth }`
  - `pub struct TaxModel { pub effective_rate: f64 }` + `pub fn tax_on_withdrawal(&self, kind: TaxedBucket, amount: f64, _year: u32) -> f64`
  - `pub struct SsInput { pub monthly_benefit: f64, pub quoted_at_age_months: i32, pub claiming_age_months: i32, pub birth_date: NaiveDate }` (with a "absent benefit" representation — see Step 2)
  - `pub struct ProjectionRequest { pub starting_buckets: Buckets, pub birth_date: NaiveDate, pub today: NaiveDate, pub target_retirement_age: u32, pub life_expectancy_age: u32, pub current_gross_income: f64, pub contribution_rate_pre_tax: f64, pub contribution_rate_roth: f64, pub expected_real_return: f64, pub target_replacement_ratio: f64, pub employer_match_formula: EmployerMatch, pub effective_tax_rate: f64, pub ss: Option<SsInput>, pub mc_seed: u64, pub mc_paths: u32, pub w_max: f64 }`
  - `#[derive(Debug)] pub enum ProjectionError { InvalidInput(String) }` + `impl Display` + `impl std::error::Error` (matches the house style for typed handler errors)

- [ ] **Step 1: Write the failing validation tests first**

Add to a `#[cfg(test)] mod tests` in `retirement_projection.rs` (no DB needed):

```rust
    #[test]
    fn tax_model_validation_rejects_out_of_range_and_non_finite_rates() {
        // effective_rate must be finite and 0.0..=1.0
    }

    #[test]
    fn project_request_validation_rejects_impossible_geometry() {
        // life_expectancy <= current_age, or life_expectancy < retirement_age -> InvalidInput
    }
```

Also pin the `TaxedBucket` mapping and that `tax_on_withdrawal` returns 0.0 for Taxable/Roth and `amount × rate` for PreTax.

- [ ] **Step 2: Implement the types**

Define the structs exactly per the interfaces above. Decide the "absent SS" representation: `SsInput` is present in the request only when benefit data exists; an absent `ss` field means `ss_income = 0.0` for all years (§2.7 partial-input rule). Note `ProjectionRequest` CANNOT derive `Copy` (contains `EmployerMatch`, which has a `Vec<MatchTier>`) — derive `Serialize, Deserialize, Clone, Debug`. Add a private validation helper `validate_request(&self) -> Result<(), ProjectionError>` covering: finite + in-range `effective_tax_rate` (`0.0..=1.0`), finite non-negative balances, finite returns/rates/income, impossible geometry (`life_expectancy_age <= age_on(today, birth_date)`, `life_expectancy_age < target_retirement_age`) — with the `!x.is_finite()` guard evaluated FIRST, never an ordered comparison. Return per-field messages.

- [ ] **Step 3: Run to verify the tests pass**
  - `cd backend && cargo test --bin backend retirement_projection::tests -- --nocapture`

---

### Task 3: Pure helpers — `age_on`, `rmd_start_age`, Uniform Lifetime Table

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `pub(crate) fn age_on(today: NaiveDate, birth_date: NaiveDate) -> u32` — **REUSE `retirement::current_age` (:905), do NOT duplicate the calendar arithmetic.** #465 already ships the pure floored-whole-years helper (calendar `(month, day)` comparison, leap-day-correct); it returns `i32`, so `age_on` is a thin `u32` wrapper: `retirement::current_age(birth_date, today).max(0) as u32`. Single source of truth; pinned by the same tests either way.
- `pub(crate) fn rmd_start_age(birth_year: i32) -> u32` — 73 for `birth_year <= 1959`, 75 for `>= 1960`.
- `const RMD_START_FLOOR: u32` — lowest possible start age (73) that indexes the table.
- `const UNIFORM_LIFETIME_DIVISORS: [f64; N]` — IRS Uniform Lifetime Table (Pub. 590-B, Table III), sourced-commented with the publication + edition, indexed by `age − RMD_START_FLOOR`; the tail (past the printed table) reuses the last printed divisor.
- `fn rmd_divisor(age: u32) -> f64` — `UNIFORM_LIFETIME_DIVISORS[min(age, tail_floor) − RMD_START_FLOOR]` (no computed divisors ever).

- [ ] **Step 1: Write the failing tests first**

```rust
    #[test]
    fn age_on_floors_to_whole_years() {
        // birthday already passed this year vs. not yet: 34 vs 33
    }

    #[test]
    fn rmd_start_age_differs_by_birth_year() {
        assert_eq!(rmd_start_age(1959), 73);
        assert_eq!(rmd_start_age(1960), 75);
        // pre-1951 cohorts also 73 (collapsed, documented)
        assert_eq!(rmd_start_age(1940), 73);
    }

    #[test]
    fn uniform_lifetime_divisors_sample_and_tail() {
        // representative sample of printed rows (age 73, 80, 90) plus the tail
        // rule: any age past the printed table returns the last printed divisor.
    }
```

Source the sample rows from the printed Table III (Pub. 590-B, 2025 edition, Appendix B). 73 → 26.5, 80 → 20.2, 90 → 12.2, 120 → 2.0 (the printed "120 and over" row is the tail; any age past it reuses 2.0). Note these replace the planning-notes samples (80 → 18.7, 90 → 11.4) which were NOT the printed table — verify against the IRS source before changing.

- [ ] **Step 2: Implement the helpers**
- [ ] **Step 3: Run `cargo test --bin backend retirement_projection::tests`**

---

### Task 4: `accumulate` — today → retirement, per-year real return + contributions + match

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `fn accumulate(req: &ProjectionRequest, returns: &[f64]) -> Buckets` — year-loop over accumulation years (`current_age + year_index < target_retirement_age`), applying `returns[i]` first to the start-of-year balance, then adding contributions (which earn no return in their year). Pre-tax split maintained internally: `pre_tax_own`/`pre_tax_employer`; opening `pre_tax` → entirely `pre_tax_own`; contributions split pre-tax (`gross × contribution_rate_pre_tax/100` → own) and Roth (`gross × contribution_rate_roth/100` → roth); employer match `retirement::employer_match(gross, pre_tax_rate + roth_rate, &formula)` → `pre_tax_employer`. Returns `Buckets` with `pre_tax = own + employer`.
  - **UNITS (critique fix): the total employee percent is passed as a PERCENT, never divided by 100.** #465's evaluator (`retirement.rs:878`) compares `employee_contribution_pct` DIRECTLY against the percent-scaled `tier.employee_pct_up_to` (`3.0` = 3%): `band_pct = (employee_pct.min(up_to) - prev).max(0.0)` then `gross * (band_pct / 100.0)`. Passing `(pre_tax_rate + roth_rate) / 100.0` (a fraction) would understate the match by 100×. Both rates are already PERCENT-scaled (#465 §21 UNITS), so the total is `pre_tax_rate + roth_rate` as-is.
  - **PIN the §21 worked example** (test in Step 1): $100,000 gross, tiers `[{3, 100}, {5, 50}]` ("100% of the first 3%, then 50% of the next 2%"), total 5% → `100000 × 3% × 100% + 100000 × 2% × 50%` = **$4000**; a total of 10% → STILL $4000 (excess above the top tier is unmatched).
- `pub fn deterministic_accumulate(req: &ProjectionRequest) -> Buckets` — the same loop with a constant `expected_real_return/100` per year (the deterministic path). Internally shares the `&[f64]`-shaped loop.

- [ ] **Step 1: Write the failing tests first** (each AC-pinned case):
  - Identity: zero years to retirement → buckets unchanged (zero accumulation years).
  - Zero return: balance grows by contributions only.
  - Zero contributions: balance grows by return only.
  - Employer match at/below/above each tier boundary: match dollars = `employer_match(gross, total_pct, formula)`, where `total_pct` is PERCENT-scaled (`5.0` = 5%) — assert the §21 worked example ($100k, `[{3,100},{5,50}]`, 5% → $4000) AND that a 10% total still yields $4000 (unmatched excess).
  - Pre-tax vs Roth tracked separately end to end: a Roth contribution never lands in pre-tax and vice versa.
  - Match dollars land in `pre_tax_employer`, never `pre_tax_own`.
- [ ] **Step 2: Implement `accumulate` + `deterministic_accumulate`**
- [ ] **Step 3: Run `cargo test --bin backend retirement_projection::tests`**

---

### Task 5: `decumulate` — retirement → horizon, RMD + ordering + tax + SS + depletion

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `struct YearOutcome { pub net_income: f64, pub depleted: bool }`
- `fn decumulate(req: &ProjectionRequest, returns: &[f64], target_annual_income: f64, tax: &TaxModel) -> Vec<YearOutcome>` — one entry per decumulation year (attained ages `target_retirement_age..=life_expectancy_age`). Per year, with the start-of-year balances (sub-accounts maintained):
  1. **RMD** (if `age >= rmd_start_age(birth_year)` and `pre_tax > 0`): gross `pre_tax / rmd_divisor(age)`, split proportionally across `pre_tax_own`/`pre_tax_employer`; net after `tax.tax_on_withdrawal(PreTax, gross, year)` counts toward the year's income.
  2. **SS**: `ss_annual` for the year when attained age `>= claiming_age_months / 12` (floored); `0.0` if `req.ss` is None. Derived via `claiming_adjustment` once up front (see Step 2 for error handling).
  3. **Needs-driven withdrawal**: net need = `max(0, target_annual_income − ss_annual − rmd_net)`. Fund in order: taxable (untaxed, no gross-up) → pre-tax (gross-up by `1/(1 − effective_rate)`, split own/employer) → Roth (untaxed). A withdrawal takes `min(available, needed)`; if a bucket runs out, move to the next.
  4. **Depletion**: if the year's total available (net of tax, after RMD) < the year's need, withdraw everything, clamp all buckets to `0.0` (NEVER negative), mark `depleted = true` and stop (subsequent years are not simulated — the caller derives the depletion year from the first `depleted` index).
  - Return applied first to start-of-year balances; contributions do not exist in decumulation.
- `pub fn deterministic_decumulate(req: &ProjectionRequest, target_annual_income: f64, tax: &TaxModel) -> Vec<YearOutcome>` — the constant-return wrapper.
- `pub fn target_run_with_depletion(req: &ProjectionRequest) -> (bool, Option<i32>)` — deterministic `decumulate` against `target_annual_income`; returns `(depletes, depletion_age)` (`None` when the plan survives to `life_expectancy_age`). This is Assumption 12's target run with constant returns.

- [ ] **Step 1: Write the failing tests first**:
  - Ordering exhausts taxable before pre-tax before Roth (asserted at the point each bucket hits zero — e.g. a mixed-bucket input where taxable runs dry in year 1, pre-tax in year 3, Roth never touches before then).
  - An RMD larger than the spending need forces the larger gross withdrawal (surplus income that year; portfolio never negative).
  - `rmd_start_age` differs for a 1959 (73) vs 1960 (75) birth year in the actual sim (assert the year RMD first fires).
  - Portfolio never goes negative (random/boundary inputs; assert all year-end balances `>= 0.0`).
  - A plan that survives reports `depletes: false` / `depletion_age: None`.
  - Pre-tax withdrawals are grossed up by `1/(1 − rate)`; SS is not taxed.
- [ ] **Step 2: Implement `decumulate` + the deterministic wrappers**

`claiming_adjustment` error handling: this is called with the caller-assembled `SsInput`; an error (our own math refusing a stored row — the #466 `adjustment_unavailable` case) must NOT be silently recomputed. Surface it as `ProjectionError::InvalidInput` from the enclosing public entry points (Tasks 7/9); the sim itself calls an internal `fn ss_annual_for(req) -> Result<f64, ProjectionError>` computed ONCE before the loop (not per year). A partial SS input (benefit but no ages) → `0.0` with no error, per §2.7.

- [ ] **Step 3: Run `cargo test --bin backend retirement_projection::tests`**

---

### Task 6: `sustainable_annual_w` (bisection) + `IncomeSources` + full `Projection` assembly

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `pub const BISECTION_ITERATIONS: u32` — named, sourced-commented (e.g. 40; each iteration is a full decumulation, so this trades precision against runtime — the bench in Task 11 measures it).
- `pub fn sustainable_annual_w(req: &ProjectionRequest, tax: &TaxModel) -> f64` — bisection over `w ∈ [0, w_max]`: `decumulate` with trial `w` as `target_annual_income` (SS offsets, RMDs forced); the search converges on the `w` whose need-stream the portfolio exactly funds through `life_expectancy_age`. Pathological inputs needing more than `w_max` → capped at `w_max` (documented; the band's sustainable figure is capped, `percent_of_goal` still reported >1).
- `#[derive(Serialize, Debug, Clone)] pub struct IncomeSources { pub own_savings: f64, pub employer_contributions: f64, pub social_security: f64, pub other_assets: f64, pub gap: f64 }` — monthly, from the deterministic sustainable run (own = roth + pre_tax_own withdrawals, employer = pre_tax_employer withdrawals, social_security = adjusted SS monthly, other_assets = taxable withdrawals, `gap = max(0, target_monthly − sum)`). Built by a private `income_sources_from_run(...)` helper.
- `#[derive(Serialize, Debug, Clone)] pub struct ProjectionAssumptions { ... }` — every field of `ProjectionRequest` (echoed), plus derived `current_age`, `rmd_start_age`, `mc_sigma`, `mc_seed`, `mc_paths`, `effective_tax_rate`, `ss_adjusted_monthly_benefit`/`ss_claiming_age_used` (`Option`), `sustainable_annual_w`, `bisection_iterations`, `w_max`, `current_date`. All sourced-commented where they are numbers with provenance.
- `pub fn run_projection(req: &ProjectionRequest) -> Result<Projection, ProjectionError>` — the deterministic headline assembly: validate → `deterministic_accumulate` → `sustainable_annual_w` (constant returns) → `deterministic_monthly_income = sustainable_annual_w/12 + ss_monthly` → `percent_of_goal` (`Option`, `None` when `target_monthly_income <= 0`) → `IncomeSources` → `target_run_with_depletion` → `assumptions`.
- `#[derive(Serialize, Debug, Clone)] pub struct Projection { pub deterministic_monthly_income: f64, pub percent_of_goal: Option<f64>, pub percentile_band: PercentileBand, pub success_rate: f64, pub income_sources: IncomeSources, pub depletion_age: Option<i32>, pub depletes: bool, pub assumptions: ProjectionAssumptions }` (percentile_band is wired in Task 8; `run_projection` is the deterministic-only entry for now and Task 8 extends it with the MC field).

- [ ] **Step 1: Write the failing tests first**:
  - Bisection convergence: an over-funded plan sustains MORE than its target (percent_of_goal > 1); an under-funded plan sustains less.
  - `percent_of_goal = None` when `target_monthly_income <= 0` (zero gross income).
  - `IncomeSources` sums correctly and `gap = max(0, target − sum)`.
  - `depletes: false`/`None` for a surviving plan (matching Task 5).
  - Constant-value pins: `BISECTION_ITERATIONS`, `w_max` values asserted.
- [ ] **Step 2: Implement `sustainable_annual_w`, `IncomeSources`, `ProjectionAssumptions`, `run_projection`**
- [ ] **Step 3: Run `cargo test --bin backend retirement_projection::tests`**

---

### Task 7: `monte_carlo` — seeded lognormal, per-path target-run + sustainable solve, percentiles, success rate

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `pub const MC_REAL_RETURN_STD_DEV: f64 = 0.1346;` — sourced-commented (US 60/40 real-return volatility 1901–2022, CFA Institute / Dimson-Marsh-Staunton 2023; URL in the comment). Single constant, whole portfolio.
- `pub const MC_DEFAULT_SEED: u64` — fixed production default (any literal, documented).
- `pub const MC_DEFAULT_PATHS: u32` — PLACEHOLDER (e.g. 100) whose real value is pinned in Task 11 against the bench. Documented as "placeholder chosen against the measured runtime budget; final value in the #467 PR."
- `#[derive(Serialize, Debug, Clone)] pub struct PercentileBand { pub p10: f64, pub p25: f64, pub p50: f64, pub p75: f64, pub p90: f64 }`
- `pub fn monte_carlo(req: &ProjectionRequest) -> Result<(PercentileBand, f64), ProjectionError>` — validates (`paths > 0`, else `InvalidInput`), seeds `StdRng::seed_from_u64(req.mc_seed)`, draws per-path per-year multipliers from `Normal` with mean `ln(1 + r) − σ²/2` and std `σ` (path-major, years ascending — one draw per accumulation year and per decumulation year). Per path: `target_run_with_depletion` (success counter) AND a per-path sustainable solve (`sustainable_annual_w` against the path's own return series). Total monthly = per-path sustainable/12 + `ss_monthly` (deterministic, identical across paths). Percentiles computed on the sorted per-path totals (p10/p25/p50/p75/p90; ties fine — flat band for degenerate inputs is reported, not an error). Success rate = fraction of paths reaching `life_expectancy_age` undepleted.
- The path-major loop needs `accumulate`/`decumulate` to accept the path's `Vec<f64>` return series — the `&[f64]` seam from Tasks 4/5 already provides this; `monte_carlo` just builds the series per path.

- [ ] **Step 1: Write the failing tests first**:
  - Same seed reproduces identical output across two runs (byte-identical `PercentileBand` + success rate).
  - **Sequence risk**: hand-build a return series `[bad, bad, bad, good…]` and its reversal; run the SAME simulation on both via the `&[f64]` seam; assert the front-loaded one yields a strictly worse sustainable figure. (This is why the `&[f64]` seam is mandatory.)
  - Percentiles ordered: p10 ≤ p25 ≤ p50 ≤ p75 ≤ p90.
  - Success rate 100% for a trivially over-funded input, 0% for a trivially under-funded one.
  - `paths = 0` → `ProjectionError::InvalidInput`.
  - `rand_distr` draws stable for a fixed seed at the module boundary.
- [ ] **Step 2: Implement `monte_carlo`** — the log-mean conversion `ln(1 + r) − σ²/2` (note: `r` here is `expected_real_return/100`, the percent→fraction conversion from Task 4) is pinned by a test.
- [ ] **Step 3: Run `cargo test --bin backend retirement_projection::tests`**

---

### Task 8: Wire the MC band into `Projection` and finish the serialization boundary

**Files:**
- Create: `backend/src/retirement_projection.rs` (additions)

**Interfaces:**
- `run_projection` gains the MC step: it now computes `monte_carlo(req)` and fills `Projection.percentile_band` + `success_rate`. The deterministic headline (Task 6) is unchanged; the band always bounds it by construction (per-path portfolio + `ss_monthly`).
- All `Projection`-family structs (`Projection`, `ProjectionRequest`, `ProjectionAssumptions`, `Buckets`, `IncomeSources`, `TaxModel`, `SsInput`, `PercentileBand`) are serde-derived: `Serialize` on everything; `Deserialize` on the input-side types. They are plain data types with no behavior — chat and REST serialize the SAME `Projection`.

- [ ] **Step 1: Write a serialization test** asserting the response shape round-trips and that `Option` fields serialize to `null` (e.g. `percent_of_goal: None`, `depletion_age: None`), never omitted keys the UI would re-derive. **Do NOT assert "band bounds headline" as a strict inequality (critique advisory):** the SS shift and tied/degenerate paths make equality legitimate — assert the INCLUSIVE `p10 <= headline <= p90`, because the band bounds it by construction.
- [ ] **Step 2: Extend `run_projection` with the MC step; derive serde on all structs.**
- [ ] **Step 3: Run the full engine test module: `cargo test --bin backend retirement_projection::tests`** — all Tasks 2–8 green.

---

### Task 9: REST handlers — `GET`/`POST /api/retirement/projection` in `retirement.rs` + routes in `main.rs`

**Files:**
- Modify: `backend/src/retirement.rs` (handlers + `#[ignore]` tests)
- Modify: `backend/src/main.rs` (routes)

**Interfaces:**
- `pub async fn get_projection_handler(State(state): State<AppState>, auth: AuthUser) -> Result<Json<retirement_projection::Projection>, ApiError>` — `require_caller_tier(&state.db, user_id, Tier::Pro)`; load the profile (`get_profile`); 404-style `"set up your retirement profile first"` when `None`; assemble `ProjectionRequest` from the profile + `assets`/`asset_holdings` (via the `assets::` helpers: `current_balance` when present, else sum of `market_value`, else 0) + `today` (server clock — this is the HANDLER, so `Utc::now()` is legal here); default `mc_seed = MC_DEFAULT_SEED`, `mc_paths = MC_DEFAULT_PATHS`; call `run_projection`; map `ProjectionError::InvalidInput` → 400.
- **`employer_match_formula` decode is STRICT — `StoredEmployerMatch`, never the lenient type (critique fix).** The stored JSONB must be decoded through `retirement::StoredEmployerMatch` (no `#[serde(default)]`, `deny_unknown_fields` — §21/#465's storage-side type) and then `.into()` to `EmployerMatch` for the request; a decode failure is a **500** (corrupt JSONB in our own table is our bug), NEVER a silent default to "no match" that the engine would consume as fact. Do NOT copy the lenient `serde_json::from_value::<EmployerMatch>` fixture pattern at `retirement.rs:3185`/`:3257` — that leniency is correct for TEST fixtures (and for the `POST` override body, which IS an input) and WRONG for reading a stored row.
- **Non-US profiles compute identically (decision — critique advisory).** The engine has no country input: a profile with `country = "FR"` still projects (the RMD/SS/tax rules are the user's own entered figures, and `retirement_supported_country`'s `supported: false` is a #469 RENDERING gate per #465's "the gate is a rendering gate, not a write gate" precedent). SS data, if the user entered it, is passed through as stored for ANY country; a profile without SS data passes `ss: None` → SS income `0.0`.
- `#[derive(Deserialize)] pub struct ProjectionOverrides { pub contribution_rate_pre_tax: Option<f64>, pub contribution_rate_roth: Option<f64>, pub expected_real_return: Option<f64>, pub target_retirement_age: Option<u32>, pub current_gross_income: Option<f64>, pub target_replacement_ratio: Option<f64>, pub effective_tax_rate: Option<f64>, pub employer_match_formula: Option<EmployerMatch> }`
- `pub async fn post_projection_handler(...) -> Result<Json<Projection>, ApiError>` — same gate + profile load; merge `ProjectionOverrides` over the STORED profile values (COALESCE-style "absent = stored"); override validation = the engine's `validate_request` + `validate_profile` spirit (finite/in-range); NO persistence (no DB write, no audit row). Same `Projection` response shape.
- Routes in `main.rs` `protected_routes` (:273-378), next to `/retirement/profile` (:377):
  ```rust
  .route("/api/retirement/projection", get(retirement::get_projection_handler).post(retirement::post_projection_handler))
  ```
  (user-scoped — no `:budget_id` — mirroring `/api/retirement/profile`; Pro gate lives in the handler via `require_caller_tier`, matching the `:378` gate pattern.)

- [ ] **Step 1: Write the failing `#[ignore]` handler tests first** (in `retirement.rs`, reusing `mk_pro`-seeded Pro users with `status = 'trialing'`, per-user `cus_test_{user_id}`):
  - Pro gate: non-Pro caller → 402, zero engine work (no profile → no profile error, still 402).
  - No profile → 404-style "set up your retirement profile first".
  - `GET` returns a `Projection` for a seeded profile + assets fixture (assert the deterministic figure is a concrete number and `assumptions` echoes the inputs).
  - `POST` applies overrides without persisting: after the call, the stored profile row is UNCHANGED.
  - `POST` with an invalid override (NaN/out-of-range `effective_tax_rate`) → 400.
  - Corrupt stored `employer_match_formula` (e.g. `{}` or a missing `tiers` key hand-edited into the row) → 500, never a silent no-match projection.
  - A non-US profile (`country = 'FR'`) with a full SS input projects normally and its SS stream comes from `claiming_adjustment` on the stored figures.
- [ ] **Step 2: Implement the handlers + `ProjectionOverrides` merge**
- [ ] **Step 3: Register the routes; run `cargo test --bin backend -- --ignored retirement`** (and `cargo test --bin backend` for the compile gate)

---

### Task 10: Chat `RETIREMENT_PROJECTION` arm + `build_projection_card_html` + `ChatResponse` field

**Files:**
- Modify: `backend/src/rag.rs`

**Interfaces:**
- `ChatResponse` (:94-120) gains:
  ```rust
  /// Structured projection for nels#467: set ONLY by the RETIREMENT_PROJECTION
  /// chat action, so #469 can render the real card without re-running anything.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub retirement_projection: Option<retirement_projection::Projection>,
  ```
- Actions are `String`s — there is NO `AiAction` enum (critique fix). The action vocabulary lives in the system prompt's `"action": "NONE" | ...` string list (:1627) and is matched by `match effective_action` on strings. Add `"RETIREMENT_PROJECTION"` to that list; `AiActionParams` needs no new field (the `..Default::default()` literals cover it — the projection action reads only the profile).
- System prompt rule (after 21d, `SET_RETIREMENT_PROFILE_RULE` at :990): a short rule 21e — read-only; invoke when the user asks for a projection/forecast of retirement income; NO editorializing, NO advice, NO comparison to goals, NO commentary on the numbers (mirroring 21d's prohibitions); the arm returns a fixed card.
- `fn build_projection_card_html(p: &Projection) -> String` — mirrors `build_categories_table_html`'s card shape (fixed headings, `money()` formatting where available, escaped): headline, percentile band (p10/p25/p50/p75/p90), %-of-goal, success rate, income sources (five segments), depletion, and an assumption-disclosure line. Fixed text — the model never writes it.
- Dispatch arm in the `match effective_action` block (near `SET_RETIREMENT_PROFILE` at :3941-3963): `"RETIREMENT_PROJECTION" => { ... }` — Pro-gated via `require_caller_tier` (402 → `mutation_error`), assemble the request from the stored profile (404-style message when no profile), run `run_projection`, set `parsed_ai_res.response_text = build_projection_card_html(&projection)`, set the `retirement_projection` field, and a fixed non-editorializing success line. `chat_action_is_write_gated` (:725-745) must NOT cover this action (it is read-only, like OPEN_INSIGHTS).
- Thread the new field through the `ChatResponse` construction (:4627-4645).

- [ ] **Step 1: Write the failing `#[ignore]` chat tests first** (in `rag.rs`):
  - `RETIREMENT_PROJECTION` returns the card in `response_text` AND the structured `Projection` on `ChatResponse`.
  - Pro gate: non-Pro → 402 `mutation_error`, no card.
  - No profile → the 404-style message, no card.
  - A system-prompt rule assertion mirroring the existing rule tests (:8692-8742): the new rule forbids editorializing and names the action.
- [ ] **Step 2: Implement the arm, the card builder, the rule, and the field**
- [ ] **Step 3: Run `cargo test --bin backend -- --ignored rag::tests` + `cargo test --bin backend`**

---

### Task 11: Offline forecast-branch repoint + narrowed retirement matcher

**Files:**
- Modify: `backend/src/rag.rs` (offline chain :2204-2283, matchers)

**Interfaces:**
- A new pure matcher `pub(crate) fn offline_retirement_projection_action(msg_lower: &str) -> Option<&'static str>` — NARROW by design: claims the branch only on retirement-phrasing words in a NON-question message (e.g. `retire`, `retirement`, `retirement projection`), NEVER on `forecast`/`next month`/`left over` (those keep the budget forecast arm). Interrogatives refuse (a "what will I have left over" question is budget semantics). **Also refuses (critique fix):** `save for` / `saving for` retirement (`CREATE_GOAL`'s phrasing — mirror the guard `offline_retirement_action` already carries, and never steal it) and a BARE `projection` with no `retire`/`retirement` word (a bare "show me a projection" is the budget forecast arm's domain — only `retirement projection` / `retire` / `retirement` claim the branch). Returns `Some("RETIREMENT_PROJECTION")` when claimed.
- In the offline if/else chain (:2204-2283), place the retirement-projection branch ABOVE the existing forecast arm (:2271-2272): `else if let Some(projection_action) = offline_retirement_projection_action(&msg_lower) { ... }`. It builds the deterministic card from a fixture/empty or stored profile (offline = no LLM; there may be no DB-backed profile in a demo — see Step 2), and sets `response_text` to the real deterministic figure (NOT the fabricated `$100.00`). The existing `forecast`/`next month` arm keeps its budget semantics UNCHANGED for budget phrasings.
- The existing `offline_retirement_action` (:7076-7106) is UNCHANGED — it routes profile WRITES; the new matcher routes projection READS. Both must coexist without stealing each other's phrasings (`retire at 62` → write; `retirement projection` → read).

- [ ] **Step 1: Write the failing unit tests first**:
  - `offline_retirement_projection_action("show my retirement projection") == Some("RETIREMENT_PROJECTION")`; `("retire")` matches; `("what will I have left over next month") == None`; `("forecast next month") == None` (budget arm); `("when can i retire") == None` (interrogative).
  - **Critique-fix exclusions:** `("save for retirement") == None` and `("saving for retirement") == None` (`CREATE_GOAL` phrasing — must not be stolen); `("projection next month") == None` and `("show me a projection") == None` (bare `projection` without a retirement word stays the budget forecast arm's).
  - Extend `offline_retirement_action_does_not_collide_with_sibling_matchers` (:8627) so the NEW matcher and the existing matchers together still leave each phrase exactly one owner.
- [ ] **Step 2: Implement the matcher + the offline branch.**

Offline data source decision: in offline mode there is no real profile to read without a DB. Options: (a) build a small demo fixture (a documented conventional-planning-figure profile) so the card shows real math; (b) return a fixed "set up your retirement profile first" card. Choose (a) if a demo fixture is testable in pure unit tests; otherwise (b). Document the choice in the branch comment. The branch's output MUST be deterministic and never the `$100.00` string.

- [ ] **Step 3: Run `cargo test --bin backend rag::tests` (unit) + `cargo test --bin backend`**

---

### Task 12: `runtime_benchmarks` ignored test + pin `MC_DEFAULT_PATHS`

**Files:**
- Create: `backend/src/retirement_projection.rs` (addition to `mod tests`)

**Interfaces:**
- `#[tokio::test] #[ignore] async fn runtime_benchmarks()` — times (via `std::time::Instant`) a deterministic `run_projection` and a full `monte_carlo` at `MC_DEFAULT_PATHS` against a standard fixture set (the same fixture shape the unit tests use); prints both with a documented run command in the doc comment (`cargo test --bin backend retirement_projection::tests::runtime_benchmarks -- --ignored --nocapture`). `#[ignore]` because CI timing is unreliable.

- [ ] **Step 1: Implement the bench test.**
- [ ] **Step 2: Run it** (`cargo run --release` build first — `cargo test --release --bin backend retirement_projection::tests::runtime_benchmarks -- --ignored --nocapture`), record the numbers, and **pin `MC_DEFAULT_PATHS`** to a value that keeps a full MC in the single-digit-ms to low-tens-of-ms budget. Update the constant's doc comment with the measured numbers.
- [ ] **Step 3: Record both numbers + the resulting #469 recommendation (live point figure + settle-debounced band) for the PR body.** Add an always-run test asserting the FINAL `MC_DEFAULT_PATHS` value.

---

### Task 13: AGENTS.md §25 + full test suite + final review

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1:** Write AGENTS.md §25 "Retirement Projection Engine (#467)" documenting: the pure module + purity contract; the `retirement_projection.rs` vs `retirement.rs` split and WHY; the bucket/sub-account model (opening pre-tax → own; match → employer); today's-dollars-at-real-return; RMD ages 73/75 + Uniform Lifetime Table + tail rule; `TaxModel` struct (not trait) + `_year` seam; the `&[f64]` return-series seam and the sequence-risk test; the MC convention (lognormal, path-major, seeded, `MC_DEFAULT_SEED`, single `MC_REAL_RETURN_STD_DEV = 0.1346` sourced); `sustainable_annual_w` bisection + `w_max` + "the dial exceeds 100%"; `Projection` carrying `assumptions`; the degenerate-input rules + `is_finite()`-first trap; REST GET/POST (Pro via `require_caller_tier`, overrides do NOT persist, strict `StoredEmployerMatch` decode on the stored formula, non-US profiles compute identically); the chat card + no-editorializing rule; the offline forecast repoint + narrowed matcher; and that #469 consumes the structured field. Note the deliberate simplifications (single σ, no real salary growth, HSA→taxable, sub-annual claiming-age floor, `percent_of_goal` as a fraction).
- [ ] **Step 2:** Run the full gate: `cd backend && cargo test` (all always-run tests; `--ignored` DB tests if the database is up). Confirm no regressions in `retirement.rs`/`rag.rs`/`assets.rs` tests.
- [ ] **Step 3:** Final review against the AC checklist in spec §3 (all eight boxes) and the module-doc purity contract. No `sqlx`/`reqwest`/`env::var`/`Utc::now()` anywhere in `retirement_projection.rs`.

---

## PR

- Branch `issue-467-retirement-projection-engine` (already checked out in this worktree, base `b021792`).
- Commit per task or in logical chunks; no self-attribution.
- PR body MUST publish: the deterministic-path timing and the full-MC timing at the final `MC_DEFAULT_PATHS` value, the pinned `MC_DEFAULT_PATHS`, and the resulting #469 recompute recommendation (live point figure + settle-debounced band).
- Note in the PR: the AC's literal `retirement.rs` file reference is interpreted as `retirement_projection.rs` (spec Assumption 1); the offline `$100.00` string is gone for retirement-phrased queries; the `rag.rs:2208`/`2204-2211` ADD_TRANSACTION guard and the `offline_retirement_action` write matcher are untouched.
- Iterate on review; when merged, update AGENTS.md §25 is already in the PR (Task 13).
