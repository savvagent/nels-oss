//! Asset taxonomy and allocation math (nels#464), moved from
//! backend/src/assets.rs (spec savvagent/nels-oss#5). See AGENTS.md §20.

use serde::{Deserialize, Serialize};

/// The smallest difference between two money amounts this module treats as real:
/// half a cent.
///
/// # Why a CURRENCY-scale epsilon and not a float-scale one
///
/// The usual reflex for comparing floats is a tiny relative tolerance like
/// `f64::EPSILON` or `1e-9`. That is the wrong tool here, and the difference is
/// not academic. Real balances reproduce it: `18504.06 + 30199.96` evaluates to
/// `48704.020000000004`, so reconciling those holdings against a reported
/// balance of `48704.02` yields a residual of `-7.28e-12`. A float-scale epsilon
/// is small enough to let that through, and this module's contract reads a
/// NEGATIVE residual as "the reported balance disagrees with the holdings --
/// surface it as stale or trigger a refresh". Two figures that are the same
/// amount of money would raise a data-quality alarm on the user's screen.
/// Symmetrically a `+1e-12` residual would fold a nonsense sliver into `Other`
/// and render an "unclassified" slice worth a picodollar.
///
/// The quantity being compared is not an abstract real number, it is money, and
/// money has a natural resolution: the minor unit. Any residual smaller than
/// half a minor unit cannot correspond to a real discrepancy in any currency
/// this product handles -- it can only be accumulated binary-representation
/// error -- so `0.005` is both large enough to absorb every plausible
/// cancellation artefact and far too small to hide a discrepancy a user could
/// ever notice or care about.
///
/// # Currencies with other minor units
///
/// `0.005` is half a cent for the two-decimal currencies that dominate here.
/// For a zero-decimal currency such as JPY it is simply stricter than needed,
/// which is harmless. For a three-decimal currency such as KWD it is five
/// thousandths of a dinar -- half a fils -- which is again the right order.
/// There is no currency for which a fixed `0.005` is too coarse to matter.
pub const RECONCILIATION_EPSILON: f64 = 0.005;

/// A `securities.security_type` value, mirroring that column's CHECK exactly.
///
/// The variants are the CHECK's value list and nothing else, so widening the
/// CHECK and forgetting this enum shows up as an unrecognised value at decode
/// time rather than as a silent misclassification.
///
/// # Why decoding here is LENIENT, unlike [`TaxTreatment`] and [`AssetType`]
///
/// [`from_db`](Self::from_db) maps anything it does not recognise to
/// [`SecurityType::Other`] and logs a warning, rather than failing. That is
/// deliberate and is the opposite of the strict policy the tax and asset
/// taxonomies use, because the consequence of each unknown value is different.
/// An unknown security type has an honest home: `Other` means "we do not know
/// what this is", which is exactly true, and it renders to the user as an
/// explicit unclassified slice. An unknown tax treatment has no honest home --
/// every fallback silently mis-taxes the money -- so that one must fail loudly.
///
/// The `tracing::warn!` is not optional decoration. An unrecognised value can
/// only mean the CHECK was widened and this enum was not, and that is a defect
/// to be found in the logs, not a state to absorb quietly.
///
/// `Etf` and `MutualFund` stay DISTINCT from `Equity` even though
/// [`classify_security_type`] currently maps all three to
/// [`AllocationClass::Equity`]. Collapsing them at the type level would throw
/// away information the database still holds, and #468 may well need to tell a
/// bond fund from a stock once the vendor's asset-class data is being fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityType {
    Equity,
    Etf,
    MutualFund,
    FixedIncome,
    Cash,
    Other,
}

impl SecurityType {
    /// Parse a raw `securities.security_type` string, mapping anything
    /// unrecognised to [`SecurityType::Other`] with a warning. See the type
    /// docs for why this one is lenient where the others are strict.
    pub fn from_db(raw: &str) -> Self {
        match raw {
            "equity" => Self::Equity,
            "etf" => Self::Etf,
            "mutual_fund" => Self::MutualFund,
            "fixed_income" => Self::FixedIncome,
            "cash" => Self::Cash,
            "other" => Self::Other,
            unknown => {
                tracing::warn!(
                    security_type = %unknown,
                    "unrecognised securities.security_type; classifying as Other. \
                     This means the column's CHECK was widened without updating \
                     SecurityType and classify_security_type"
                );
                Self::Other
            }
        }
    }

    /// The CHECK literal for writing this value back to `securities`.
    ///
    /// The mirror of [`Self::from_db`] on the WRITE side, kept STRICT: the
    /// `securities` column's CHECK is the one place an unrecognised variant
    /// must never land, so this has no `Other`-by-default fallback — every
    /// variant is spelled out, and adding a `SecurityType` variant makes this
    /// match non-exhaustive (a compile error) until the writer picks a literal.
    pub fn db_literal(self) -> &'static str {
        match self {
            Self::Equity => "equity",
            Self::Etf => "etf",
            Self::MutualFund => "mutual_fund",
            Self::FixedIncome => "fixed_income",
            Self::Cash => "cash",
            Self::Other => "other",
        }
    }
}

/// An `assets.tax_treatment` value, mirroring that column's CHECK exactly.
///
/// # Why this decodes STRICTLY
///
/// The migration explicitly plans to widen this CHECK per country: adding the
/// UK means adding an ISA/SIPP treatment, adding Canada means adding
/// RRSP/TFSA-shaped ones. As a bare `String` reaching #467's projection maths,
/// that widening produces NO compile error anywhere -- the new value simply
/// falls through whatever `match` arm happens to be last and the money is
/// silently taxed as something it is not. That is a retirement-projection
/// error, not a display error: it changes the number a person plans their life
/// around, and it changes it in a direction nobody is watching.
///
/// So there is no lenient fallback and no `Other`-by-default: an unrecognised
/// value fails the decode, the query returns an error, and the request 500s.
/// A loud failure at the boundary is strictly better than a plausible-looking
/// wrong projection, and it points at the exact line that needs changing.
///
/// The serialized form is `snake_case`, matching the CHECK's literals
/// character for character, so the API JSON is byte-identical to what the bare
/// `String` produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type), sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum TaxTreatment {
    PreTax,
    Roth,
    Taxable,
    Hsa,
    Other,
}

/// An `assets.asset_type` value, mirroring that column's CHECK exactly.
///
/// This is the COUNTRY-NEUTRAL wrapper axis -- what KIND of container an asset
/// is, never a national account product. It is orthogonal to
/// [`TaxTreatment`]: a 401(k) is `(RetirementAccount, PreTax)`, a Roth IRA is
/// `(RetirementAccount, Roth)`, a taxable brokerage is `(Brokerage, Taxable)`.
///
/// Decoding is strict for the same reason [`TaxTreatment`]'s is: an
/// unrecognised container kind means the CHECK moved and this enum did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type), sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    RetirementAccount,
    Brokerage,
    Cash,
    Pension,
    Annuity,
    RealEstate,
    Other,
}

/// Coarse allocation buckets.
///
/// Deliberately only four. The product question these answer is "roughly how
/// much growth risk is this person carrying", which does not need sector or
/// region detail, and the vendor data behind it is not reliable enough to
/// support finer buckets honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationClass {
    Equity,
    FixedIncome,
    Cash,
    /// Everything unrecognized, including missing type data. Explicitly means
    /// "we do not know what this is", never "miscellaneous small stuff".
    Other,
}

/// Map a [`SecurityType`] onto its allocation bucket.
///
/// - [`Equity`](SecurityType::Equity), [`Etf`](SecurityType::Etf),
///   [`MutualFund`](SecurityType::MutualFund) -> [`AllocationClass::Equity`]
/// - [`FixedIncome`](SecurityType::FixedIncome) ->
///   [`AllocationClass::FixedIncome`]
/// - [`Cash`](SecurityType::Cash) -> [`AllocationClass::Cash`]
/// - [`Other`](SecurityType::Other) -> [`AllocationClass::Other`]
///
/// # There is deliberately NO `_ =>` arm
///
/// Every variant is matched by name. That is the point of taking a
/// [`SecurityType`] rather than a `&str`: widening the `securities`
/// `security_type` CHECK means adding a variant, and adding a variant makes
/// THIS function fail to compile until somebody decides which bucket the new
/// instrument belongs in. A wildcard arm would silently absorb it into `Other`
/// and the decision would never be made. The compile error is the feature.
///
/// # `mutual_fund` -> `Equity` is a deliberate approximation
///
/// A fund's true split is simply not knowable from its type alone: a target-date
/// fund or a bond fund is a `mutual_fund` too, and lumping those into `Equity`
/// overstates their growth risk. We accept that today because the large majority
/// of fund holdings in retirement accounts are equity or equity-heavy funds, and
/// because the alternative -- dropping every fund into `Other` -- would leave a
/// typical 401(k) reading as almost entirely unclassified, which is less useful
/// and no more true. #468 may refine this from the vendor's asset-class data
/// once holdings sync is fetching it; when it does, this is the function to
/// change and the change should be invisible to every caller.
///
/// # Unknown -> `Other` (and why not something more convenient)
///
/// An unrecognized or missing type could be forced into a real bucket, and both
/// options are wrong in a way that matters. Calling it equity would overstate
/// expected growth in #467's projections, quietly flattering the user's
/// retirement outlook. Calling it cash would understate growth and make the
/// projection needlessly pessimistic. `Other` is the only answer that is
/// actually true: we do not know. It surfaces to the user as an explicit
/// unclassified slice rather than as false precision, which is the compliance
/// decision recorded on #454 -- a retirement projection must not imply
/// confidence the underlying data does not support.
pub fn classify_security_type(t: SecurityType) -> AllocationClass {
    match t {
        SecurityType::Equity | SecurityType::Etf | SecurityType::MutualFund => {
            AllocationClass::Equity
        }
        SecurityType::FixedIncome => AllocationClass::FixedIncome,
        SecurityType::Cash => AllocationClass::Cash,
        SecurityType::Other => AllocationClass::Other,
    }
}

/// One position inside an asset, reduced to just what allocation needs.
///
/// `security_type` is classified through [`classify_security_type`] rather than
/// being pre-bucketed by the caller, so the classification rules live in
/// exactly one place. It is a [`SecurityType`] rather than a raw string: the
/// lenient unknown-to-`Other` mapping happens once, at the database boundary in
/// [`SecurityType::from_db`], and everything downstream of that point works
/// with a closed set of variants.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldingValue {
    pub security_type: SecurityType,
    pub market_value: f64,
}

/// An asset's allocation, in currency units (not fractions -- see
/// [`AllocationFractions`] for those).
///
/// Two invariants hold for every value this module produces:
///
/// 1. **[`total()`](Allocation::total) is ALWAYS the sum of the four buckets.**
///    This is not a maintained property but a theorem: `total` is a METHOD over
///    the four fields, not a stored field, so there is no representable
///    `Allocation` whose total disagrees with its buckets. Nothing can be
///    counted in the total without landing in a bucket, and no caller --
///    including a future #467 or #469 constructing one of these by hand -- can
///    make it otherwise.
/// 2. **`residual` is `reported_balance - holdings_total`**, and is exactly
///    `0.0` when nothing was reconciled -- that is, whenever the value came
///    from [`allocate`], or from [`allocate_reconciled`] with no reported
///    balance, or with one that already matched the holdings to within
///    [`RECONCILIATION_EPSILON`].
///
/// A positive `residual` has already been folded into `other`, and therefore
/// into the total. A negative one has not been folded into anything; it is
/// carried purely as a signal (see [`allocate_reconciled`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Allocation {
    pub equity: f64,
    pub fixed_income: f64,
    pub cash: f64,
    pub other: f64,
    pub residual: f64,
}

/// An allocation expressed as **fractions**, NOT percents.
///
/// `0.6` means sixty percent. Nothing here is pre-multiplied by 100 -- the
/// times-100 and the `%` belong together at the presentation layer, where the
/// locale-aware formatter already lives. The name says `Fractions` rather than
/// `Pct` for exactly that reason: a type called `Pct` invites a caller to
/// render `p.equity` directly next to a percent sign and be wrong by two orders
/// of magnitude.
///
/// # The invariant, stated honestly
///
/// The four fields sum to `1.0` (within float tolerance) by construction. They
/// are **NOT** individually bounded to `0.0..=1.0`, and code must not assume
/// they are. A negative `market_value` -- a short position, which real vendor
/// holdings data does contain -- puts a negative amount in one bucket, and if
/// the overall total is still positive the corresponding fraction is negative
/// while another exceeds `1.0`. Both still sum to one. Anything rendering these
/// as bar widths or pie slices has to decide what a negative slice looks like;
/// it cannot rely on a range guarantee this data cannot honour.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AllocationFractions {
    pub equity: f64,
    pub fixed_income: f64,
    pub cash: f64,
    pub other: f64,
}

/// Sum `holdings` into the four allocation buckets.
///
/// [`total()`](Allocation::total) is the sum of those buckets -- equivalently,
/// the sum of every holding's `market_value` -- and `residual` is `0.0`,
/// because nothing has been reconciled against an externally reported balance.
/// Use [`allocate_reconciled`] when the asset also carries a `current_balance`
/// worth checking the holdings against.
///
/// An empty slice yields an all-zero allocation rather than an error: an asset
/// with no holdings recorded is an ordinary state (a manually entered pension, a
/// house, an account whose holdings have not synced yet), not a failure.
///
/// # Assumes ONE currency across all holdings
///
/// The market values are added together as bare numbers. That is only
/// meaningful if every holding is denominated in the same currency, and nothing
/// in this function checks it. `securities.currency` is nullable and per-row, so
/// a genuinely multi-currency account -- a UK employer plan holding a US-listed
/// fund, say -- would silently produce a total that is the sum of two different
/// units. Today no write path exists to create such an asset, so the assumption
/// holds vacuously. It stops holding the moment #468's holdings sync writes real
/// vendor data, and #468/#467 must confront it then: either convert to the
/// asset's `currency` before summing, or refuse to allocate a mixed-currency
/// asset. Adding an FX rate silently here would be the worst of the three.
pub fn allocate(holdings: &[HoldingValue]) -> Allocation {
    let mut a = Allocation {
        equity: 0.0,
        fixed_income: 0.0,
        cash: 0.0,
        other: 0.0,
        residual: 0.0,
    };
    for hv in holdings {
        let bucket = match classify_security_type(hv.security_type) {
            AllocationClass::Equity => &mut a.equity,
            AllocationClass::FixedIncome => &mut a.fixed_income,
            AllocationClass::Cash => &mut a.cash,
            AllocationClass::Other => &mut a.other,
        };
        *bucket += hv.market_value;
    }
    a
}

/// [`allocate`], then reconcile the result against the balance the asset itself
/// reports (`assets.current_balance`).
///
/// Sets `residual = reported_balance - holdings_total`. When that residual is
/// positive the reported balance exceeds what the holdings account for, and the
/// difference is added to `other` -- which makes [`total()`](Allocation::total)
/// agree with the balance the user sees on their statement, automatically,
/// because the total is computed from the buckets rather than tracked alongside
/// them. A residual of zero or less changes no bucket.
///
/// # `None` means "no balance has ever been reported", not "zero"
///
/// `assets.current_balance` is nullable precisely so those two can be told
/// apart, exactly as `balance_as_of` is. `None` here means there is nothing to
/// reconcile against, so this degrades to plain [`allocate`] with a `0.0`
/// residual. Treating a missing balance as `0.0` instead would compute
/// `residual = -holdings_total`, a large negative residual, which the contract
/// below defines as "the reported balance looks stale" -- firing a false
/// data-quality alarm on every freshly synced asset whose balance has not
/// landed yet.
///
/// # Near-zero residuals are exactly zero
///
/// The comparison is against [`RECONCILIATION_EPSILON`], not against `0.0`:
/// `|residual| < RECONCILIATION_EPSILON` sets `residual = 0.0` and folds
/// nothing. Real balances make this necessary rather than fastidious -- see
/// that constant's docs for the worked example where two figures that are the
/// same amount of money differ by `-7.28e-12` and would otherwise be reported
/// as a stale-balance discrepancy.
///
/// # Why a positive residual goes to `Other` and not to `Cash`
///
/// It is tempting to call it cash, because most of the time it is: an
/// uninvested sweep balance sitting in the account. But it is equally often a
/// position we failed to fetch -- a security the vendor did not return, an
/// instrument type the connector does not model yet, a holdings response that
/// arrived partial. Those two possibilities have opposite implications for
/// growth risk, and we cannot tell them apart from here. `Other` says
/// "unclassified", which is the honest answer and the one that shows up in the
/// UI as a visible gap rather than as confident-looking cash.
///
/// # Why a negative residual does not scale the buckets down
///
/// A reported balance below the holdings total means the two sources disagree,
/// and the holdings are the more granular, more recently itemized truth --
/// `current_balance` is frequently the stale one, carried over from an earlier
/// sync or typed in by hand months ago. Scaling every bucket down to force
/// agreement would silently corrupt a good allocation with a bad scalar and
/// erase the evidence that anything was ever wrong. Instead the buckets stand
/// as computed and the signed `residual` is handed to the caller, which can
/// surface it as "this balance looks stale" or trigger a refresh.
pub fn allocate_reconciled(
    holdings: &[HoldingValue],
    reported_balance: Option<f64>,
) -> Allocation {
    let mut a = allocate(holdings);
    let Some(reported_balance) = reported_balance else {
        // Nothing was ever reported, so there is nothing to disagree with.
        // `allocate` already left `residual` at 0.0.
        return a;
    };
    let residual = reported_balance - a.total();
    if residual.abs() < RECONCILIATION_EPSILON {
        // Sub-half-cent: binary representation noise, not money. Report exact
        // agreement rather than a discrepancy of one part in 10^12.
        a.residual = 0.0;
        return a;
    }
    a.residual = residual;
    if residual > 0.0 {
        // Only `other` is touched. The total follows, because it is derived.
        a.other += residual;
    }
    a
}

impl Allocation {
    /// The sum of the four buckets: `equity + fixed_income + cash + other`.
    ///
    /// A method rather than a stored field so the invariant is a THEOREM rather
    /// than a maintained property. With six `pub f64` fields, any caller --
    /// this crate is a binary, so #467 and #469 are callers -- could construct
    /// or mutate an `Allocation` whose `total` disagreed with its buckets, and
    /// nothing would catch it. Deriving the total removes the representable
    /// wrong state entirely, at the cost of three additions.
    pub fn total(&self) -> f64 {
        self.equity + self.fixed_income + self.cash + self.other
    }

    /// This allocation as fractions of [`total()`](Self::total), or `None` when
    /// there is no meaningful total to divide by.
    ///
    /// `None` covers three cases, all of which would otherwise hand the caller
    /// a number that renders as nonsense:
    ///
    /// 1. **A non-finite total.** A `NaN` or infinite total divides into `NaN`
    ///    fractions, and `serde_json` serializes a non-finite `f64` as JSON
    ///    `null` without raising anything -- so the failure reaches the user as
    ///    a silently missing number rather than as an error.
    /// 2. **A zero or negative total.** An empty or zero-value portfolio, or
    ///    the pathological all-short case, which divide by zero or invert every
    ///    sign.
    /// 3. **A total below currency resolution.** A total of `1e-12` left over
    ///    from cancellation between a long and a short position is not a
    ///    portfolio worth showing an allocation for, and dividing by it yields
    ///    fractions in the millions.
    ///
    /// # The NaN trap, spelled out
    ///
    /// `is_finite()` is checked FIRST and as a SEPARATE condition, and that
    /// ordering is load-bearing rather than stylistic. `NaN <= 0.0` evaluates
    /// to **false** in Rust, as IEEE 754 requires of every ordered comparison
    /// involving NaN, so a bare `if self.total() <= 0.0` guard does not reject
    /// a NaN total -- it waves it straight through and returns `Some` full of
    /// NaN fractions. `inf <= 0.0` is likewise false, and `inf / inf` is NaN.
    /// Non-finite values must be rejected by a predicate, never by an ordered
    /// comparison.
    pub fn fractions(&self) -> Option<AllocationFractions> {
        let total = self.total();
        if !total.is_finite() || total < RECONCILIATION_EPSILON {
            return None;
        }
        Some(AllocationFractions {
            equity: self.equity / total,
            fixed_income: self.fixed_income / total,
            cash: self.cash / total,
            other: self.other / total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Float tolerance. Every assertion below compares sums of f64s, so exact
    /// equality would be a flaky test, not a stricter one.
    const EPS: f64 = 1e-9;

    /// Build a holding from a RAW database string, so these tests exercise the
    /// same `from_db` boundary the query does rather than bypassing it.
    fn h(security_type: &str, market_value: f64) -> HoldingValue {
        HoldingValue {
            security_type: SecurityType::from_db(security_type),
            market_value,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // -- SecurityType::from_db -------------------------------------------

    #[test]
    fn security_type_from_db_maps_every_check_value() {
        assert_eq!(SecurityType::from_db("equity"), SecurityType::Equity);
        assert_eq!(SecurityType::from_db("etf"), SecurityType::Etf);
        assert_eq!(SecurityType::from_db("mutual_fund"), SecurityType::MutualFund);
        assert_eq!(
            SecurityType::from_db("fixed_income"),
            SecurityType::FixedIncome
        );
        assert_eq!(SecurityType::from_db("cash"), SecurityType::Cash);
        assert_eq!(SecurityType::from_db("other"), SecurityType::Other);
    }

    /// The LENIENT half of the contract: an unrecognised value -- which can
    /// only mean the CHECK was widened without updating this enum -- becomes
    /// `Other` rather than failing the whole read.
    #[test]
    fn security_type_from_db_is_lenient_about_unknown_values() {
        assert_eq!(SecurityType::from_db("wat"), SecurityType::Other);
        assert_eq!(SecurityType::from_db(""), SecurityType::Other);
        assert_eq!(SecurityType::from_db("Equity"), SecurityType::Other);
    }

    /// `SecurityType` must keep serializing to the exact `snake_case` literals
    /// the `securities.security_type` CHECK lists, so the enum and the column
    /// cannot drift apart in spelling.
    #[test]
    fn security_type_serializes_to_the_check_literals() {
        for (variant, literal) in [
            (SecurityType::Equity, "equity"),
            (SecurityType::Etf, "etf"),
            (SecurityType::MutualFund, "mutual_fund"),
            (SecurityType::FixedIncome, "fixed_income"),
            (SecurityType::Cash, "cash"),
            (SecurityType::Other, "other"),
        ] {
            assert_eq!(
                serde_json::to_string(&variant).unwrap(),
                format!("\"{literal}\"")
            );
        }
    }

    /// The tax and asset taxonomies must serialize to the literals their CHECKs
    /// list too -- this is what keeps the `GET /api/assets` JSON byte-identical
    /// to what the bare `String` fields produced.
    #[test]
    fn tax_and_asset_taxonomies_serialize_to_the_check_literals() {
        for (variant, literal) in [
            (TaxTreatment::PreTax, "pre_tax"),
            (TaxTreatment::Roth, "roth"),
            (TaxTreatment::Taxable, "taxable"),
            (TaxTreatment::Hsa, "hsa"),
            (TaxTreatment::Other, "other"),
        ] {
            assert_eq!(
                serde_json::to_string(&variant).unwrap(),
                format!("\"{literal}\"")
            );
        }
        for (variant, literal) in [
            (AssetType::RetirementAccount, "retirement_account"),
            (AssetType::Brokerage, "brokerage"),
            (AssetType::Cash, "cash"),
            (AssetType::Pension, "pension"),
            (AssetType::Annuity, "annuity"),
            (AssetType::RealEstate, "real_estate"),
            (AssetType::Other, "other"),
        ] {
            assert_eq!(
                serde_json::to_string(&variant).unwrap(),
                format!("\"{literal}\"")
            );
        }
    }

    // -- classify_security_type ------------------------------------------

    #[test]
    fn classify_equity() {
        assert_eq!(
            classify_security_type(SecurityType::Equity),
            AllocationClass::Equity
        );
    }

    #[test]
    fn classify_etf_is_equity() {
        assert_eq!(
            classify_security_type(SecurityType::Etf),
            AllocationClass::Equity
        );
    }

    #[test]
    fn classify_mutual_fund_is_equity() {
        assert_eq!(
            classify_security_type(SecurityType::MutualFund),
            AllocationClass::Equity
        );
    }

    #[test]
    fn classify_fixed_income() {
        assert_eq!(
            classify_security_type(SecurityType::FixedIncome),
            AllocationClass::FixedIncome
        );
    }

    #[test]
    fn classify_cash() {
        assert_eq!(
            classify_security_type(SecurityType::Cash),
            AllocationClass::Cash
        );
    }

    #[test]
    fn classify_other() {
        assert_eq!(
            classify_security_type(SecurityType::Other),
            AllocationClass::Other
        );
    }

    /// An unrecognized vendor string must land in `Other`, never be guessed
    /// into a real bucket -- end to end, from the raw string to the bucket.
    #[test]
    fn classify_unknown_string_is_other() {
        assert_eq!(
            classify_security_type(SecurityType::from_db("wat")),
            AllocationClass::Other
        );
    }

    // -- allocate --------------------------------------------------------

    /// AC: normal mix.
    #[test]
    fn allocate_normal_mix() {
        let a = allocate(&[
            h("equity", 100.0),
            h("etf", 50.0),
            h("fixed_income", 25.0),
            h("cash", 25.0),
        ]);
        assert!(close(a.equity, 150.0), "{a:?}");
        assert!(close(a.fixed_income, 25.0), "{a:?}");
        assert!(close(a.cash, 25.0), "{a:?}");
        assert!(close(a.other, 0.0), "{a:?}");
        assert!(close(a.total(), 200.0), "{a:?}");
        assert!(close(a.residual, 0.0), "{a:?}");
    }

    /// AC: single-holding portfolio.
    #[test]
    fn allocate_single_holding() {
        let a = allocate(&[h("equity", 1234.56)]);
        assert!(close(a.equity, 1234.56), "{a:?}");
        assert!(close(a.fixed_income, 0.0), "{a:?}");
        assert!(close(a.cash, 0.0), "{a:?}");
        assert!(close(a.other, 0.0), "{a:?}");
        assert!(close(a.total(), 1234.56), "{a:?}");
        assert!(close(a.residual, 0.0), "{a:?}");
    }

    /// AC: zero-value portfolio (no holdings at all).
    #[test]
    fn allocate_empty_slice_is_zero_portfolio() {
        let a = allocate(&[]);
        assert!(close(a.equity, 0.0), "{a:?}");
        assert!(close(a.fixed_income, 0.0), "{a:?}");
        assert!(close(a.cash, 0.0), "{a:?}");
        assert!(close(a.other, 0.0), "{a:?}");
        assert!(close(a.total(), 0.0), "{a:?}");
        assert!(close(a.residual, 0.0), "{a:?}");
    }

    /// AC: zero-value portfolio (holdings exist but are all worth nothing).
    #[test]
    fn allocate_zero_value_holdings_is_zero_portfolio() {
        let a = allocate(&[h("equity", 0.0), h("cash", 0.0)]);
        assert!(close(a.total(), 0.0), "{a:?}");
        assert!(a.fractions().is_none(), "{a:?}");
    }

    /// AC: unknown security type.
    #[test]
    fn allocate_all_unknown_lands_in_other() {
        let a = allocate(&[h("wat", 10.0), h("other", 15.0), h("crypto", 5.0)]);
        assert!(close(a.equity, 0.0), "{a:?}");
        assert!(close(a.fixed_income, 0.0), "{a:?}");
        assert!(close(a.cash, 0.0), "{a:?}");
        assert!(close(a.other, 30.0), "{a:?}");
        assert!(close(a.total(), 30.0), "{a:?}");
    }

    // -- allocate_reconciled ---------------------------------------------

    /// AC: holdings sum to a value that disagrees with the asset's
    /// `current_balance` -- reported ABOVE holdings. The unexplained remainder
    /// is credited to `Other` and to `total`.
    #[test]
    fn allocate_reconciled_reported_above_holdings_credits_other() {
        let holdings = [h("equity", 100.0), h("cash", 50.0)];
        let a = allocate_reconciled(&holdings, Some(200.0));
        assert!(close(a.equity, 100.0), "{a:?}");
        assert!(close(a.fixed_income, 0.0), "{a:?}");
        assert!(close(a.cash, 50.0), "{a:?}");
        assert!(close(a.other, 50.0), "{a:?}");
        assert!(close(a.total(), 200.0), "{a:?}");
        assert!(close(a.residual, 50.0), "{a:?}");
    }

    /// AC: reported BELOW holdings. Buckets and total are untouched -- the
    /// holdings are the finer-grained truth -- and the shortfall is recorded as
    /// a negative `residual` the caller can surface.
    #[test]
    fn allocate_reconciled_reported_below_holdings_records_negative_residual() {
        let holdings = [h("equity", 100.0), h("cash", 50.0)];
        let plain = allocate(&holdings);
        let a = allocate_reconciled(&holdings, Some(120.0));
        assert!(close(a.equity, plain.equity), "{a:?}");
        assert!(close(a.fixed_income, plain.fixed_income), "{a:?}");
        assert!(close(a.cash, plain.cash), "{a:?}");
        assert!(close(a.other, plain.other), "{a:?}");
        assert!(close(a.total(), 150.0), "{a:?}");
        assert!(close(a.residual, -30.0), "{a:?}");
    }

    /// AC: reported exactly equal to the holdings total -- nothing to
    /// reconcile, so the result matches `allocate` with a zero residual.
    #[test]
    fn allocate_reconciled_exact_match_has_zero_residual() {
        let holdings = [h("equity", 100.0), h("fixed_income", 50.0)];
        let a = allocate_reconciled(&holdings, Some(150.0));
        assert_eq!(a, allocate(&holdings));
        assert!(close(a.residual, 0.0), "{a:?}");
        assert!(close(a.total(), 150.0), "{a:?}");
    }

    /// Reconciling an empty portfolio against a reported balance puts the whole
    /// balance in `Other` -- we know it is worth something, we just do not know
    /// what it holds.
    #[test]
    fn allocate_reconciled_empty_holdings_puts_everything_in_other() {
        let a = allocate_reconciled(&[], Some(500.0));
        assert!(close(a.other, 500.0), "{a:?}");
        assert!(close(a.total(), 500.0), "{a:?}");
        assert!(close(a.residual, 500.0), "{a:?}");
    }

    /// `None` means "no balance has ever been reported", which is nothing to
    /// reconcile against -- NOT a reported zero. The distinction matters
    /// because a reported zero would produce a large negative residual, the
    /// module's own "this balance looks stale" signal, on every freshly synced
    /// asset whose balance has not landed yet.
    #[test]
    fn allocate_reconciled_none_is_plain_allocate_with_zero_residual() {
        let holdings = [h("equity", 100.0), h("cash", 50.0)];
        let a = allocate_reconciled(&holdings, None);
        assert_eq!(a, allocate(&holdings));
        assert!(close(a.residual, 0.0), "{a:?}");
        assert!(close(a.total(), 150.0), "{a:?}");

        // And a reported ZERO is emphatically not the same thing.
        let zero = allocate_reconciled(&holdings, Some(0.0));
        assert!(close(zero.residual, -150.0), "{zero:?}");
        assert_ne!(a, zero, "None must not behave like Some(0.0)");
    }

    // -- the currency-scale epsilon --------------------------------------

    /// The exact real-money case that motivates [`RECONCILIATION_EPSILON`]:
    /// `18504.06 + 30199.96` evaluates to `48704.020000000004`, so reconciling
    /// against the reported `48704.02` gives a residual of about `-7.28e-12`.
    /// Without a currency-scale threshold that is a NEGATIVE residual, which
    /// this module's contract reads as "the balance looks stale" -- a false
    /// data-quality alarm raised over two figures that are the same money.
    #[test]
    fn allocate_reconciled_absorbs_float_cancellation_as_exact_agreement() {
        let holdings = [h("equity", 18_504.06), h("fixed_income", 30_199.96)];
        // The bug this guards against, demonstrated first.
        assert_ne!(
            18_504.06_f64 + 30_199.96_f64,
            48_704.02_f64,
            "fixture: these must genuinely differ in f64, or the test proves nothing"
        );
        let a = allocate_reconciled(&holdings, Some(48_704.02));
        assert_eq!(a.residual, 0.0, "sub-cent noise must be EXACTLY zero: {a:?}");
        assert_eq!(
            a,
            allocate(&holdings),
            "nothing may be folded into a bucket for a sub-cent residual"
        );
    }

    /// The symmetric direction: a tiny POSITIVE residual must not fold a
    /// nonsense sliver into `Other` and render an unclassified slice worth a
    /// picodollar.
    #[test]
    fn allocate_reconciled_ignores_tiny_positive_residual() {
        let holdings = [h("equity", 100.0)];
        let a = allocate_reconciled(&holdings, Some(100.0 + 1e-12));
        assert_eq!(a.residual, 0.0, "{a:?}");
        assert_eq!(a.other, 0.0, "nothing may be folded into Other: {a:?}");
    }

    /// The threshold must not swallow a discrepancy a user could notice. One
    /// cent is above half a cent, so it is real and must be reported.
    #[test]
    fn allocate_reconciled_still_reports_a_one_cent_discrepancy() {
        let holdings = [h("equity", 100.0)];
        let over = allocate_reconciled(&holdings, Some(100.01));
        assert!(close(over.residual, 0.01), "{over:?}");
        assert!(close(over.other, 0.01), "{over:?}");

        let under = allocate_reconciled(&holdings, Some(99.99));
        assert!(close(under.residual, -0.01), "{under:?}");
        assert!(close(under.other, 0.0), "{under:?}");
    }

    // -- the cross-cutting reconciliation contract -----------------------

    /// `total()` is now a derived method, so "buckets sum to total" is a
    /// tautology and no longer worth asserting. What IS still worth asserting
    /// over the same matrix is the reconciliation contract itself, which is
    /// genuinely falsifiable: after `allocate_reconciled`, either the residual
    /// was positive and `total()` now equals the REPORTED balance, or it was
    /// zero-or-negative and `total()` still equals the raw HOLDINGS total.
    #[test]
    fn reconciliation_matrix_obeys_the_residual_contract() {
        let portfolios: Vec<Vec<HoldingValue>> = vec![
            vec![],
            vec![h("equity", 1234.56)],
            vec![h("equity", 0.0), h("cash", 0.0)],
            vec![
                h("equity", 100.0),
                h("etf", 50.0),
                h("mutual_fund", 33.33),
                h("fixed_income", 25.0),
                h("cash", 25.0),
                h("other", 1.0),
            ],
            vec![h("wat", 10.0), h("other", 15.0)],
            vec![h("cash", 7.77)],
        ];
        let reported = [-10.0, 0.0, 0.01, 100.0, 250.0, 1_000_000.0];

        for p in &portfolios {
            let plain = allocate(p);
            assert_eq!(plain.residual, 0.0, "unreconciled residual must be 0: {plain:?}");

            for r in reported {
                let a = allocate_reconciled(p, Some(r));
                if a.residual > 0.0 {
                    assert!(
                        close(a.total(), r),
                        "a positive residual must make total() equal the reported balance: {a:?}"
                    );
                } else {
                    assert!(
                        close(a.total(), plain.total()),
                        "a non-positive residual must leave total() at the holdings total: {a:?}"
                    );
                }
                // Whatever the branch, only `other` may ever have moved.
                assert!(close(a.equity, plain.equity), "{a:?}");
                assert!(close(a.fixed_income, plain.fixed_income), "{a:?}");
                assert!(close(a.cash, plain.cash), "{a:?}");
            }

            // `None` never reconciles anything, for any portfolio.
            assert_eq!(allocate_reconciled(p, None), plain);
        }
    }

    // -- fractions -------------------------------------------------------

    #[test]
    fn fractions_sum_to_one() {
        let a = allocate(&[
            h("equity", 60.0),
            h("fixed_income", 30.0),
            h("cash", 5.0),
            h("wat", 5.0),
        ]);
        let p = a.fractions().expect("positive total yields fractions");
        assert!(close(p.equity, 0.6), "{p:?}");
        assert!(close(p.fixed_income, 0.3), "{p:?}");
        assert!(close(p.cash, 0.05), "{p:?}");
        assert!(close(p.other, 0.05), "{p:?}");
        assert!(
            close(p.equity + p.fixed_income + p.cash + p.other, 1.0),
            "{p:?}"
        );
    }

    #[test]
    fn fractions_sum_to_one_after_reconciliation() {
        let a = allocate_reconciled(&[h("equity", 75.0)], Some(100.0));
        let p = a.fractions().expect("positive total yields fractions");
        assert!(close(p.equity, 0.75), "{p:?}");
        assert!(close(p.other, 0.25), "{p:?}");
        assert!(
            close(p.equity + p.fixed_income + p.cash + p.other, 1.0),
            "{p:?}"
        );
    }

    #[test]
    fn fractions_none_when_total_is_zero() {
        assert!(allocate(&[]).fractions().is_none());
    }

    #[test]
    fn fractions_none_when_total_is_negative() {
        let a = allocate(&[h("equity", -25.0)]);
        assert!(a.total() < 0.0, "{a:?}");
        assert!(a.fractions().is_none(), "{a:?}");
    }

    /// A total left over from cancellation between a long and a short position
    /// is below currency resolution and is not a portfolio worth showing an
    /// allocation for. Dividing by it yields fractions in the millions.
    #[test]
    fn fractions_none_when_total_is_below_currency_resolution() {
        let a = Allocation {
            equity: 1e-12,
            fixed_income: 0.0,
            cash: 0.0,
            other: 0.0,
            residual: 0.0,
        };
        assert!(a.total() > 0.0, "fixture: the total is positive, just tiny");
        assert!(
            a.fractions().is_none(),
            "a sub-cent total must not produce fractions: {a:?}"
        );
    }

    /// THE regression this guards: `NaN <= 0.0` is FALSE in Rust, so a bare
    /// `<= 0.0` guard lets a NaN total through and returns `Some` full of NaN
    /// fractions -- which `serde_json` then emits as JSON `null` without ever
    /// raising. The `is_finite()` check must reject it first.
    #[test]
    fn fractions_none_for_non_finite_totals() {
        // The trap itself, asserted so the rationale cannot rot. Bound through
        // a loop variable rather than compared as a literal, which would trip
        // the `invalid_nan_comparisons` lint -- a lint that exists to warn
        // about exactly the mistake being demonstrated here.
        for sails_past_a_le_zero_guard in [f64::NAN, f64::INFINITY] {
            assert!(
                !(sails_past_a_le_zero_guard <= 0.0),
                "{sails_past_a_le_zero_guard} <= 0.0 must be FALSE -- this is \
                 precisely why is_finite() has to come first and separately"
            );
        }

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let a = Allocation {
                equity: bad,
                fixed_income: 10.0,
                cash: 0.0,
                other: 0.0,
                residual: 0.0,
            };
            assert!(
                a.fractions().is_none(),
                "a non-finite total must yield None, got {:?} for {bad}",
                a.fractions()
            );
        }
    }

    /// The honest statement of the fractions invariant: they sum to 1.0, but
    /// individual fields are NOT bounded to `0.0..=1.0`. A short position --
    /// which real vendor holdings data contains -- puts a negative amount in a
    /// bucket, so with a still-positive total one fraction goes negative and
    /// another exceeds 1.0.
    #[test]
    fn fractions_are_not_bounded_to_zero_one() {
        let a = allocate(&[h("equity", 150.0), h("fixed_income", -50.0)]);
        let p = a.fractions().expect("total is +100, so fractions exist");
        assert!(close(p.equity, 1.5), "a fraction above 1.0 is possible: {p:?}");
        assert!(
            close(p.fixed_income, -0.5),
            "a negative fraction is possible: {p:?}"
        );
        assert!(
            close(p.equity + p.fixed_income + p.cash + p.other, 1.0),
            "they still sum to one: {p:?}"
        );
    }
}
