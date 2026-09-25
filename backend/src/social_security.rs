//! Social Security claiming-age math (nels#466, part of the #454 epic).
//!
//! # What this module is, and deliberately is not
//!
//! It is **not** a benefit estimator. Per #454 decision 1 — binding — the user
//! ENTERS the figure printed on their own Social Security statement, and this
//! module supplies only the DERIVATIVE: what that figure becomes at a different
//! claiming age. The blocker on estimating is data, not math: AIME/PIA needs 35
//! years of indexed earnings, and Nels's aggregation providers return bank
//! transactions, not W-2 wage records, while SSA publishes no consumer data API.
//! The statement already prints the answer. There is also a compliance argument:
//! quoting SSA's own figure is defensible in a way that publishing a competing
//! Nels estimate is not.
//!
//! So: **the user supplies the anchor; we supply the slope.**
//!
//! # Purity is structural, not a convention
//!
//! Nothing here imports `axum` or `sqlx`. Every function is a pure function of
//! its arguments — no clock, no pool, no environment — so the whole module is
//! unit-testable without a database, and #467's projection engine can call it
//! from inside a Monte Carlo loop without touching I/O.
//!
//! # Real dollars: no inflation, no COLA, applied anywhere in this module
//!
//! Everything else in the projection runs in TODAY's dollars at a REAL return
//! (#465). A Social Security benefit has a statutory COLA, so an SSA-statement
//! figure is ALREADY expressed in today's dollars. Inflating it would double-count
//! the COLA and overstate readiness; deflating it would understate. Neither is
//! done here, and `no_inflation_or_cola_is_applied_to_the_entered_figure` pins it.
//! #467 must not apply one either — see the contract note on
//! [`claiming_adjustment`].

use chrono::{Datelike, NaiveDate};

/// The earliest age Social Security retirement benefits can be claimed, in
/// MONTHS from birth (62 years). Claiming earlier is not possible, so
/// [`claiming_adjustment`] REJECTS it rather than extrapolating the reduction
/// formula into a region where it has no statutory meaning.
pub const MIN_CLAIM_AGE_MONTHS: i32 = 62 * 12;

/// The age past which delayed retirement credits stop accruing, in MONTHS from
/// birth (70 years).
///
/// This constant is used in TWO different ways, and the difference is
/// deliberate:
///
///  * [`adjustment_factor`] CLAMPS at it. Nothing accrues past 70, so the factor
///    at 71 is the factor at 70 — that is the statute, not a guard.
///  * [`claiming_adjustment`] REJECTS above it. A caller who says "I will claim
///    at 71" has said something we should query rather than silently reinterpret
///    as 70; #466's acceptance criteria require an error, not a silent clamp.
pub const MAX_CLAIM_AGE_MONTHS: i32 = 70 * 12;

/// A full retirement age, which is a number of YEARS **and MONTHS** — not a
/// whole number of years.
///
/// The months half is load-bearing rather than decorative: ten birth cohorts
/// (1938–1942 and 1955–1959) have an FRA with a non-zero month component, and
/// rounding 66y6m to either 66 or 67 moves every derived benefit by about 3%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FullRetirementAge {
    pub years: i32,
    pub months: i32,
}

impl FullRetirementAge {
    /// The FRA expressed as a single count of MONTHS from birth, which is the
    /// unit every claiming calculation actually works in.
    pub fn total_months(&self) -> i32 {
        self.years * 12 + self.months
    }
}

/// Full retirement age for a birth year, as the SSA step table (nels#466).
///
/// This is a STEP TABLE, not a formula. It is closed data reproduced from SSA's
/// published schedule, in the same closed-set style as
/// `bank_provider::Provider::for_country`:
///
/// | birth year | FRA |
/// |---|---|
/// | 1937 and earlier | 65y 0m |
/// | 1938 | 65y 2m |
/// | 1939 | 65y 4m |
/// | 1940 | 65y 6m |
/// | 1941 | 65y 8m |
/// | 1942 | 65y 10m |
/// | 1943–1954 | 66y 0m |
/// | 1955 | 66y 2m |
/// | 1956 | 66y 4m |
/// | 1957 | 66y 6m |
/// | 1958 | 66y 8m |
/// | 1959 | 66y 10m |
/// | 1960 and later | 67y 0m |
///
/// Takes a birth YEAR, not a date, per #466's stated signature. SSA's
/// "born on January 1 counts as the previous year" rule therefore lives in
/// [`fra_birth_year`], which every caller holding a `NaiveDate` must go through.
pub fn full_retirement_age(birth_year: i32) -> FullRetirementAge {
    match birth_year {
        // The two ramps rise two months per birth year. They are written as
        // arithmetic on the ramp's first year rather than as five literal arms
        // each so that a transcription slip is impossible; the boundary years of
        // both ramps are pinned by tests either way.
        ..=1937 => FullRetirementAge { years: 65, months: 0 },
        1938..=1942 => FullRetirementAge { years: 65, months: 2 * (birth_year - 1937) },
        1943..=1954 => FullRetirementAge { years: 66, months: 0 },
        1955..=1959 => FullRetirementAge { years: 66, months: 2 * (birth_year - 1954) },
        1960.. => FullRetirementAge { years: 67, months: 0 },
    }
}

/// The delayed retirement credit for a birth year, as a PERCENT PER YEAR of
/// delay beyond full retirement age.
///
/// A second SSA step table, and one it is easy not to notice exists: the
/// familiar 8%/yr applies only to people born in **1943 or later**. Earlier
/// cohorts earn less, down to 3%/yr. Hardcoding 8% would overstate the benefit
/// of delaying for every profile born before 1943 — a wrong number with no
/// signal, which is the failure mode this whole feature area refuses to ship.
///
/// | birth year | credit |
/// |---|---|
/// | 1924 and earlier | 3.0%/yr |
/// | 1925–1926 | 3.5%/yr |
/// | 1927–1928 | 4.0%/yr |
/// | 1929–1930 | 4.5%/yr |
/// | 1931–1932 | 5.0%/yr |
/// | 1933–1934 | 5.5%/yr |
/// | 1935–1936 | 6.0%/yr |
/// | 1937–1938 | 6.5%/yr |
/// | 1939–1940 | 7.0%/yr |
/// | 1941–1942 | 7.5%/yr |
/// | 1943 and later | 8.0%/yr |
pub fn delayed_credit_pct_per_year(birth_year: i32) -> f64 {
    match birth_year {
        ..=1924 => 3.0,
        1925..=1926 => 3.5,
        1927..=1928 => 4.0,
        1929..=1930 => 4.5,
        1931..=1932 => 5.0,
        1933..=1934 => 5.5,
        1935..=1936 => 6.0,
        1937..=1938 => 6.5,
        1939..=1940 => 7.0,
        1941..=1942 => 7.5,
        1943.. => 8.0,
    }
}

/// The birth year SSA's benefit tables key on, which is NOT always the calendar
/// year of birth.
///
/// SSA's rule: *"If you were born on January 1st, we figure your benefit as if
/// your birthday was in the previous year."* Applying it here rather than inside
/// [`full_retirement_age`] keeps that function a pure function of a year — the
/// signature #466 specifies — while still getting the rule right for anyone
/// holding a date. Both [`full_retirement_age`] and
/// [`delayed_credit_pct_per_year`] take the result of this function, because the
/// January-1 rule applies to both tables.
pub fn fra_birth_year(birth_date: NaiveDate) -> i32 {
    if (birth_date.month(), birth_date.day()) == (1, 1) {
        birth_date.year() - 1
    } else {
        birth_date.year()
    }
}

/// The multiplier that turns the PIA (the benefit at full retirement age) into
/// the benefit at `age_months`.
///
/// ```text
/// early  (age < FRA):  1 - [ min(n,36) x 5/9%  +  max(0, n-36) x 5/12% ]
/// at FRA:              exactly 1.0
/// delayed (age > FRA): 1 + [ (min(age, 70y) - FRA) x (drc/12)% ]
/// ```
///
/// where `n` is the number of months early. The 36-month hinge is statutory: the
/// first three years early cost 5/9 of 1% per month, and every month beyond that
/// costs 5/12 of 1%. The delayed side CLAMPS at 70 — nothing accrues past it.
///
/// FLOAT NOTE, stated precisely because it is easy to overclaim. Every
/// expression below multiplies the month count BEFORE dividing. At the round
/// month counts that produce the headline figures the two groupings happen to
/// agree exactly — `36 x 5 / 9` and `36 x (5/9)` both land on `20.0`, and both
/// give a 30% reduction of exactly `0.70` and a 24% credit of exactly `1.24`.
/// They do NOT agree everywhere: at 7 months early, for instance, they differ by
/// one ulp. So multiply-first is kept because it is the grouping that keeps the
/// intermediate percentages exact at the statutory hinges, not because a
/// reordering would visibly break the headline numbers — it would not.
/// `arithmetic_ordering_keeps_the_headline_factors_exact` pins the exactness
/// that IS asserted; every other test uses a tolerance, deliberately.
///
/// `drc_pct_per_year` is a parameter rather than a constant because the credit is
/// birth-year dependent — see [`delayed_credit_pct_per_year`]. Callers should get
/// it from there rather than passing a literal.
pub fn adjustment_factor(age_months: i32, fra_months: i32, drc_pct_per_year: f64) -> f64 {
    if age_months == fra_months {
        // Returned as an exact literal rather than falling out of one of the
        // branches below, so "claim at your FRA" is bit-for-bit the identity and
        // the renormalization round-trip has no drift to accumulate.
        return 1.0;
    }
    if age_months < fra_months {
        let n = fra_months - age_months;
        let first = (n.min(36) as f64) * 5.0 / 9.0;
        let beyond = ((n - 36).max(0) as f64) * 5.0 / 12.0;
        return 1.0 - (first + beyond) / 100.0;
    }
    let delayed = age_months.min(MAX_CLAIM_AGE_MONTHS) - fra_months;
    // A person whose FRA is already past 70 is not expressible under any current
    // SSA table, but the clamp above could make `delayed` negative if one ever
    // were; `max(0)` keeps this a credit rather than a silent reduction.
    1.0 + (delayed.max(0) as f64) * drc_pct_per_year / 12.0 / 100.0
}

/// Re-express a benefit quoted at one age as the benefit at another.
///
/// This is the whole point of the module. The SSA statement quotes one figure at
/// one age; the user then moves a claiming-age control and wants to know what
/// they would get instead. Normalize the quoted figure back to the PIA by
/// dividing out its own factor, then re-apply the factor for the intended age:
///
/// ```text
/// pia      = benefit / factor(quoted_at)
/// adjusted = pia x factor(claiming)
/// ```
///
/// Both the early reduction and the delayed credit are multiplicative on the
/// PIA, so this is exact algebra on the USER'S OWN NUMBER — not an estimate, and
/// not a reconstruction of their earnings history. That distinction is what keeps
/// it on the right side of #454 decision 1. `factor` never returns zero (its
/// floor is 0.70 at 62 against a 67 FRA), so the division is safe.
///
/// FRA and the delayed-retirement credit are BOTH derived here from the BIRTH
/// DATE, deliberately, rather than being separate parameters: they are two views
/// of the same cohort and passing them independently would make a mismatched
/// pair representable.
///
/// It takes a DATE rather than a year for the same reason. Both tables key on
/// [`fra_birth_year`], not on the calendar year, because of SSA's January-1
/// rule — and a caller who has a `NaiveDate` and passes `date.year()` to an
/// `i32` parameter gets a silently wrong answer for every 1 January birth, with
/// nothing in the signature to warn them. Taking the date makes that mistake
/// unrepresentable.
///
/// # Errors
///
/// Out-of-range ages are REJECTED, never clamped. #466 is explicit: claiming
/// below 62 is not possible and claiming above 70 earns nothing more, and in
/// both cases silently reinterpreting the caller's number would hide a
/// misunderstanding rather than surface it. (The internal clamp in
/// [`adjustment_factor`] is a different thing — it encodes the statute for
/// callers that legitimately evaluate the factor past 70.)
///
/// # Contract for #467
///
/// The value returned is in TODAY'S DOLLARS and is FINAL. Social Security
/// carries a statutory COLA, so an SSA-statement figure is already real; the
/// projection must carry this number through its real-return accumulation
/// WITHOUT inflating or deflating it. Doing either double-counts the COLA and
/// moves the headline materially. See the module docs.
pub fn claiming_adjustment(
    benefit: f64,
    quoted_at_age_months: i32,
    claiming_age_months: i32,
    birth_date: NaiveDate,
) -> Result<f64, String> {
    if !benefit.is_finite() || benefit < 0.0 {
        return Err(format!(
            "Social Security monthly benefit must be a finite, non-negative dollar amount, got {benefit}"
        ));
    }
    check_claim_age(quoted_at_age_months, "the age the benefit is quoted at")?;
    check_claim_age(claiming_age_months, "the claiming age")?;

    let birth_year = fra_birth_year(birth_date);
    let fra = full_retirement_age(birth_year).total_months();
    let drc = delayed_credit_pct_per_year(birth_year);

    let pia = benefit / adjustment_factor(quoted_at_age_months, fra, drc);
    Ok(pia * adjustment_factor(claiming_age_months, fra, drc))
}

/// Shared range check so the quoted age and the claiming age cannot drift apart
/// on either the bound or the wording. The message names the age in years
/// because that is the unit the user typed, even though the check is in months.
fn check_claim_age(age_months: i32, what: &str) -> Result<(), String> {
    // Spelled as a range `contains` to match `validate_profile`'s form for the
    // same bound, so the two checks read identically.
    if !(MIN_CLAIM_AGE_MONTHS..=MAX_CLAIM_AGE_MONTHS).contains(&age_months) {
        return Err(format!(
            "{what} must be between 62 and 70 — Social Security cannot be claimed before 62, and no credit accrues after 70 (got {} years {} months)",
            age_months / 12,
            age_months % 12
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default assertion for a factor or a dollar figure, because most of
    /// them genuinely are not bit-exact: a renormalization divides and then
    /// multiplies, and month counts like 22 x 5/12 do not land on a
    /// representable double. The tolerance is far tighter than any error the
    /// formulas could plausibly have.
    ///
    /// The two HEADLINE factors are the exception — 0.70 and 1.24 ARE exact, and
    /// `arithmetic_ordering_keeps_the_headline_factors_exact` asserts them with
    /// `assert_eq!` precisely because this helper cannot see the difference.
    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{what}: expected {expected}, got {actual}"
        );
    }

    fn fra(years: i32, months: i32) -> FullRetirementAge {
        FullRetirementAge { years, months }
    }

    /// A birth DATE for a cohort year. Mid-June deliberately: far from the
    /// 1 January boundary `fra_birth_year` moves, so these fixtures exercise the
    /// ordinary path and the January rule has its own dedicated tests.
    fn bd(year: i32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, 6, 15).expect("valid test birth date")
    }

    // -- full_retirement_age -------------------------------------------------

    /// Every step boundary #466's acceptance criteria name, plus 1938 and 1942
    /// (the first and LAST years of the early ramp) and 2000. 1942 is here
    /// because an exclusive `1938..1942` range — an easy slip — would drop it
    /// through to the 1960+ catch-all and give it 67y0m instead of 65y10m, a
    /// fourteen-month error that the AC's own boundary list would not catch.
    #[test]
    fn full_retirement_age_matches_the_ssa_step_table() {
        assert_eq!(full_retirement_age(1937), fra(65, 0));
        assert_eq!(full_retirement_age(1938), fra(65, 2));
        assert_eq!(full_retirement_age(1939), fra(65, 4));
        assert_eq!(full_retirement_age(1940), fra(65, 6));
        assert_eq!(full_retirement_age(1941), fra(65, 8));
        assert_eq!(full_retirement_age(1942), fra(65, 10));
        assert_eq!(full_retirement_age(1943), fra(66, 0));
        assert_eq!(full_retirement_age(1954), fra(66, 0));
        assert_eq!(full_retirement_age(1955), fra(66, 2));
        assert_eq!(full_retirement_age(1956), fra(66, 4));
        assert_eq!(full_retirement_age(1957), fra(66, 6));
        assert_eq!(full_retirement_age(1958), fra(66, 8));
        assert_eq!(full_retirement_age(1959), fra(66, 10));
        assert_eq!(full_retirement_age(1960), fra(67, 0));
        assert_eq!(full_retirement_age(1985), fra(67, 0));
        assert_eq!(full_retirement_age(2000), fra(67, 0));
    }

    #[test]
    fn full_retirement_age_is_65_for_everyone_born_before_the_first_ramp() {
        assert_eq!(full_retirement_age(1936), fra(65, 0));
        assert_eq!(full_retirement_age(1900), fra(65, 0));
    }

    #[test]
    fn full_retirement_age_total_months_flattens_years_and_months() {
        assert_eq!(fra(66, 6).total_months(), 798);
        assert_eq!(fra(67, 0).total_months(), 804);
        assert_eq!(fra(65, 0).total_months(), 780);
    }

    // -- delayed_credit_pct_per_year ----------------------------------------

    /// The familiar 8%/yr applies only from 1943. Pinning every step means a
    /// future "simplify this to a constant" edit fails loudly instead of
    /// overstating the benefit of delaying for the pre-1943 cohorts.
    #[test]
    fn delayed_credit_matches_the_ssa_step_table() {
        assert_close(delayed_credit_pct_per_year(1920), 3.0, "1920");
        assert_close(delayed_credit_pct_per_year(1924), 3.0, "1924");
        assert_close(delayed_credit_pct_per_year(1925), 3.5, "1925");
        assert_close(delayed_credit_pct_per_year(1926), 3.5, "1926");
        assert_close(delayed_credit_pct_per_year(1927), 4.0, "1927");
        assert_close(delayed_credit_pct_per_year(1929), 4.5, "1929");
        assert_close(delayed_credit_pct_per_year(1931), 5.0, "1931");
        assert_close(delayed_credit_pct_per_year(1933), 5.5, "1933");
        assert_close(delayed_credit_pct_per_year(1935), 6.0, "1935");
        assert_close(delayed_credit_pct_per_year(1937), 6.5, "1937");
        assert_close(delayed_credit_pct_per_year(1938), 6.5, "1938");
        assert_close(delayed_credit_pct_per_year(1939), 7.0, "1939");
        assert_close(delayed_credit_pct_per_year(1941), 7.5, "1941");
        assert_close(delayed_credit_pct_per_year(1942), 7.5, "1942");
        assert_close(delayed_credit_pct_per_year(1943), 8.0, "1943");
        assert_close(delayed_credit_pct_per_year(1985), 8.0, "1985");
    }

    // -- fra_birth_year ------------------------------------------------------

    #[test]
    fn fra_birth_year_moves_a_january_first_birth_to_the_previous_year() {
        let jan_first = NaiveDate::from_ymd_opt(1960, 1, 1).expect("valid date");
        // Born 1 Jan 1960 is treated as a 1959 birth, so FRA is 66y10m and NOT
        // the 67y0m the calendar year alone would give — a two-month difference
        // on the single most common boundary in the whole table.
        assert_eq!(fra_birth_year(jan_first), 1959);
        assert_eq!(full_retirement_age(fra_birth_year(jan_first)), fra(66, 10));
    }

    #[test]
    fn fra_birth_year_leaves_every_other_date_on_its_calendar_year() {
        for (y, m, d) in [(1960, 1, 2), (1960, 2, 1), (1960, 12, 31), (1959, 6, 15)] {
            let date = NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
            assert_eq!(fra_birth_year(date), y, "{y}-{m}-{d}");
        }
    }

    // -- adjustment_factor ---------------------------------------------------

    /// FRA 67 (born 1960 or later), claiming at 62: 60 months early. The first
    /// 36 cost 5/9 of 1% each = 20%; the remaining 24 cost 5/12 of 1% each =
    /// 10%; total 30%. This is the figure SSA itself publishes, and #466 names
    /// it explicitly.
    #[test]
    fn factor_at_62_against_an_fra_of_67_is_a_30_percent_reduction() {
        assert_close(adjustment_factor(62 * 12, 67 * 12, 8.0), 0.70, "62 vs FRA 67");
    }

    /// FRA 67, claiming at 70: 36 months delayed at 8%/yr = 2/3 of 1% per month
    /// = 24%.
    #[test]
    fn factor_at_70_against_an_fra_of_67_is_a_24_percent_credit() {
        assert_close(adjustment_factor(70 * 12, 67 * 12, 8.0), 1.24, "70 vs FRA 67");
    }

    /// The two headline factors are EXACT doubles, not merely close, and this
    /// asserts that rather than accepting the 1e-9 tolerance the other tests
    /// use.
    ///
    /// It is a genuinely stronger contract: any change to the reduction or
    /// credit formula that introduces even one ulp of error at 62 or 70 fails
    /// here while passing every tolerance-based test in the file.
    ///
    /// What this test deliberately does NOT claim: that the multiply-first
    /// grouping is what makes them exact. It was written believing that, and a
    /// mutation run disproved it — `36 x (5/9)` also rounds to exactly `20.0`,
    /// so both groupings give `0.70` and `1.24`. The groupings differ at other
    /// month counts (7 months early, among others), which is why the tolerance
    /// is used elsewhere. `adjustment_factor`'s float note now says this
    /// accurately.
    #[test]
    fn arithmetic_ordering_keeps_the_headline_factors_exact() {
        assert_eq!(
            adjustment_factor(62 * 12, 67 * 12, 8.0),
            0.70,
            "the 30% reduction must be exact, not merely close"
        );
        assert_eq!(
            adjustment_factor(70 * 12, 67 * 12, 8.0),
            1.24,
            "the 24% credit must be exact, not merely close"
        );
    }

    #[test]
    fn factor_at_fra_is_exactly_one() {
        // Bit-for-bit, not merely close: the identity has to be exact or the
        // renormalization round-trip drifts.
        assert_eq!(adjustment_factor(67 * 12, 67 * 12, 8.0), 1.0);
        assert_eq!(adjustment_factor(798, 798, 8.0), 1.0);
    }

    #[test]
    fn factor_one_month_early_costs_five_ninths_of_one_percent() {
        assert_close(
            adjustment_factor(67 * 12 - 1, 67 * 12, 8.0),
            1.0 - (5.0 / 9.0) / 100.0,
            "1 month early",
        );
    }

    #[test]
    fn factor_exactly_36_months_early_is_the_full_20_percent_and_no_more() {
        assert_close(adjustment_factor(67 * 12 - 36, 67 * 12, 8.0), 0.80, "36 months early");
    }

    /// The slope change. Month 37 costs 5/12 of 1%, not 5/9 — if the hinge is
    /// off by one or missing entirely, this is the test that says so.
    #[test]
    fn factor_37_months_early_switches_to_the_shallower_five_twelfths_slope() {
        let thirty_seven = adjustment_factor(67 * 12 - 37, 67 * 12, 8.0);
        assert_close(thirty_seven, 0.80 - (5.0 / 12.0) / 100.0, "37 months early");

        // And it is NOT the steeper slope continuing.
        let if_slope_never_changed = 0.80 - (5.0 / 9.0) / 100.0;
        assert!(
            (thirty_seven - if_slope_never_changed).abs() > 1e-6,
            "month 37 must use 5/12, not 5/9"
        );
    }

    /// Nothing accrues past 70 — the statute, encoded as a clamp rather than as
    /// a rejection, because the factor is a statement about the benefit formula
    /// and not about what a caller is allowed to ask for. `claiming_adjustment`
    /// is where 71 becomes an error.
    #[test]
    fn factor_clamps_at_70_so_no_credit_accrues_past_it() {
        let at_70 = adjustment_factor(70 * 12, 67 * 12, 8.0);
        assert_eq!(adjustment_factor(71 * 12, 67 * 12, 8.0), at_70, "71 must equal 70");
        assert_eq!(adjustment_factor(85 * 12, 67 * 12, 8.0), at_70, "85 must equal 70");
    }

    /// An FRA with a month component, which is the case the whole
    /// months-not-years design exists for. Born 1959: FRA 66y10m = 802 months.
    /// Claiming at 62 is 58 months early: 36 x 5/9% = 20%, plus 22 x 5/12% =
    /// 9.1666…%, total 29.1666…% — which is the 29.17% SSA publishes for that
    /// cohort.
    #[test]
    fn factor_handles_an_fra_with_a_month_component() {
        let fra_1959 = full_retirement_age(1959).total_months();
        assert_eq!(fra_1959, 802);
        assert_close(
            adjustment_factor(62 * 12, fra_1959, 8.0),
            1.0 - (20.0 + 22.0 * 5.0 / 12.0) / 100.0,
            "62 vs FRA 66y10m",
        );
    }

    /// The pre-1943 cohorts earn LESS than 8%/yr for delaying, and this is the
    /// test that stops the credit being hardcoded. Born 1941: FRA 65y8m = 788
    /// months, DRC 7.5%/yr. Delaying to 70 is 52 months.
    #[test]
    fn factor_uses_the_cohort_delayed_credit_rather_than_a_hardcoded_eight_percent() {
        let birth_year = 1941;
        let fra_months = full_retirement_age(birth_year).total_months();
        assert_eq!(fra_months, 788);
        let drc = delayed_credit_pct_per_year(birth_year);
        assert_close(drc, 7.5, "1941 drc");

        let actual = adjustment_factor(70 * 12, fra_months, drc);
        assert_close(actual, 1.0 + 52.0 * 7.5 / 12.0 / 100.0, "1941 delayed to 70");

        // Concretely different from the 8% a hardcoded credit would give.
        let if_hardcoded_eight = 1.0 + 52.0 * 8.0 / 12.0 / 100.0;
        assert!(
            (actual - if_hardcoded_eight).abs() > 1e-6,
            "1941 must not earn the 1943+ credit"
        );
    }

    // -- claiming_adjustment -------------------------------------------------

    #[test]
    fn claiming_at_the_quoted_age_returns_the_entered_figure_unchanged() {
        // Quoted at FRA, claiming at FRA: the identity, exactly.
        let out = claiming_adjustment(2_000.0, 67 * 12, 67 * 12, bd(1960)).expect("valid");
        assert_eq!(out, 2_000.0);

        // And quoted at 62, claiming at 62 — the factors cancel, so this must
        // also be the entered figure and NOT the PIA.
        let out = claiming_adjustment(1_400.0, 62 * 12, 62 * 12, bd(1960)).expect("valid");
        assert_close(out, 1_400.0, "quoted 62 claimed 62");
    }

    #[test]
    fn claiming_at_62_on_a_figure_quoted_at_fra_applies_the_30_percent_reduction() {
        let out = claiming_adjustment(2_000.0, 67 * 12, 62 * 12, bd(1960)).expect("valid");
        assert_close(out, 1_400.0, "2000 at FRA 67 claimed at 62");
    }

    #[test]
    fn claiming_at_70_on_a_figure_quoted_at_fra_applies_the_24_percent_credit() {
        let out = claiming_adjustment(2_000.0, 67 * 12, 70 * 12, bd(1960)).expect("valid");
        assert_close(out, 2_480.0, "2000 at FRA 67 claimed at 70");
    }

    /// Renormalization in the harder direction: the user copied the age-62
    /// figure off their statement and then asks what claiming at 70 gives.
    /// PIA = 1400 / 0.70 = 2000; at 70 that is 2000 x 1.24 = 2480.
    #[test]
    fn a_figure_quoted_at_62_renormalizes_to_a_claim_at_70() {
        let out = claiming_adjustment(1_400.0, 62 * 12, 70 * 12, bd(1960)).expect("valid");
        assert_close(out, 2_480.0, "1400 quoted at 62 claimed at 70");
    }

    /// And the reverse: the age-70 figure re-expressed at 62.
    /// PIA = 2480 / 1.24 = 2000; at 62 that is 2000 x 0.70 = 1400.
    #[test]
    fn a_figure_quoted_at_70_renormalizes_to_a_claim_at_62() {
        let out = claiming_adjustment(2_480.0, 70 * 12, 62 * 12, bd(1960)).expect("valid");
        assert_close(out, 1_400.0, "2480 quoted at 70 claimed at 62");
    }

    #[test]
    fn renormalization_round_trips() {
        // Quoted at 62 -> claim at 70 -> quote that at 70 -> claim at 62 must
        // return the original figure. Falsifies any asymmetry between the two
        // directions.
        let original = 1_837.42;
        let at_70 = claiming_adjustment(original, 62 * 12, 70 * 12, bd(1960)).expect("valid");
        let back = claiming_adjustment(at_70, 70 * 12, 62 * 12, bd(1960)).expect("valid");
        assert_close(back, original, "round trip");
    }

    /// #466: rejected with a clear error, NOT clamped silently. A caller who
    /// says 61 or 71 has said something we should query — quietly rewriting it
    /// to 62 or 70 would hide the misunderstanding behind a plausible number.
    #[test]
    fn a_claiming_age_outside_62_to_70_is_rejected_rather_than_clamped() {
        for bad in [61 * 12, 61 * 12 + 11, 71 * 12, 0] {
            let err = claiming_adjustment(2_000.0, 67 * 12, bad, bd(1960))
                .expect_err("must reject out-of-range claiming age");
            assert!(err.contains("62 and 70"), "message must name the range: {err}");
        }
    }

    #[test]
    fn a_quoted_age_outside_62_to_70_is_rejected_too() {
        let err = claiming_adjustment(2_000.0, 61 * 12, 67 * 12, bd(1960))
            .expect_err("must reject out-of-range quoted age");
        assert!(err.contains("quoted"), "message must say which age is wrong: {err}");
    }

    #[test]
    fn a_negative_or_non_finite_benefit_is_rejected() {
        assert!(claiming_adjustment(-1.0, 67 * 12, 62 * 12, bd(1960)).is_err());
        assert!(claiming_adjustment(f64::NAN, 67 * 12, 62 * 12, bd(1960)).is_err());
        assert!(claiming_adjustment(f64::INFINITY, 67 * 12, 62 * 12, bd(1960)).is_err());
    }

    #[test]
    fn a_zero_benefit_is_accepted_and_stays_zero() {
        // Zero is a legitimate entered value — it is DIFFERENT from "not
        // entered", which is represented as an absent Option upstream and never
        // reaches this function at all.
        let out = claiming_adjustment(0.0, 67 * 12, 70 * 12, bd(1960)).expect("valid");
        assert_eq!(out, 0.0);
    }

    /// #466's real-dollar consistency requirement, asserted rather than merely
    /// commented.
    ///
    /// Everything else in the projection runs in today's dollars at a REAL
    /// return (#465). Social Security carries a STATUTORY COLA, so the figure
    /// printed on an SSA statement is ALREADY in today's dollars. Applying an
    /// inflation adjustment on top would double-count that COLA and overstate
    /// the retirement income; deflating it would understate. Neither is done —
    /// the only thing that ever moves this number is the claiming age.
    ///
    /// The strongest form available at this layer is that the function has no
    /// inflation parameter and is the exact identity when the claiming age
    /// equals the quoted age, for any horizon. #467 must not add one on its way
    /// through; the contract note on `claiming_adjustment` is the anchor its own
    /// test should reference.
    #[test]
    fn no_inflation_or_cola_is_applied_to_the_entered_figure() {
        // Same input, two cohorts 40 years apart, same quoted-equals-claiming
        // age: if anything time-dependent had crept in, these would differ.
        let young = claiming_adjustment(2_500.0, 67 * 12, 67 * 12, bd(2000)).expect("valid");
        let old = claiming_adjustment(2_500.0, 66 * 12, 66 * 12, bd(1950)).expect("valid");
        assert_eq!(young, 2_500.0, "no inflation applied for a 2000 cohort");
        assert_eq!(old, 2_500.0, "no inflation applied for a 1950 cohort");
    }
}
