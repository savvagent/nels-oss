//! Retirement profile rules (nels#465 / nels#466), moved from
//! backend/src/retirement.rs (spec savvagent/nels-oss#5). Units, defaults and
//! validation rules are documented in AGENTS.md §21 and §23.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One tier of an employer match formula.
///
/// UNITS: both fields are PERCENT, not fractions.
/// - `employee_pct_up_to` is the CUMULATIVE employee-contribution percentage of
///   gross at which this tier stops (`3.0` = "up to 3% of gross").
/// - `match_pct` is the percentage of the covered band the employer contributes
///   (`100.0` = dollar-for-dollar, `50.0` = fifty cents on the dollar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchTier {
    pub employee_pct_up_to: f64,
    pub match_pct: f64,
}

/// An ordered, cumulative list of match tiers plus an optional annual dollar cap.
///
/// Stored as JSONB in `retirement_profiles.employer_match_formula`. Deliberately
/// NOT free text and NOT a single percentage — see nels#465's employer-match
/// paragraph. `annual_dollar_cap` is in absolute dollars; `None` means uncapped.
///
/// This is the LENIENT, INPUT-side type: `#[serde(default)]` on both fields lets
/// a REST caller or the LLM send `{"tiers": [...]}` without a cap, or a cap
/// alone. That leniency is correct for input and WRONG for storage — a stored
/// value missing `tiers` would decode to a silent "no employer match", which is
/// exactly the understatement this module refuses to make. Reading the column
/// therefore goes through `StoredEmployerMatch`, which has no defaults at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmployerMatch {
    #[serde(default)]
    pub tiers: Vec<MatchTier>,
    #[serde(default)]
    pub annual_dollar_cap: Option<f64>,
}

/// The STRICT, STORAGE-side mirror of `EmployerMatch`, used at the one point
/// `upsert_profile` decodes the `employer_match_formula` JSONB column.
///
/// No `#[serde(default)]` on either field and `deny_unknown_fields` on the
/// struct, so a stored value is only accepted if it is exactly what our own
/// writer emits — and our writer always emits BOTH keys, because
/// `serde_json::to_value` on an `EmployerMatch` serializes `annual_dollar_cap:
/// None` as an explicit `null`.
///
/// The point is the failure mode it removes. Against the lenient type, a stored
/// `{"annual_dollar_cap": 5000}` with `tiers` missing — a hand-edited row, a
/// half-finished migration, a future writer that forgets a key — decodes
/// CLEANLY to zero tiers, i.e. to "this user has no employer match". That is the
/// silently-defaulted no-match this module and AGENTS.md section 20 both say
/// must never happen: it UNDERSTATES a real match with no signal at all, and
/// nels#467's projection would consume it as fact. With this type the same value
/// is a decode error, which `upsert_profile` surfaces as a **500** — corrupt
/// JSONB in our own table is our bug, and the honest answer is to fail loudly.
/// `{}` fails for the same reason.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredEmployerMatch {
    pub tiers: Vec<MatchTier>,
    pub annual_dollar_cap: Option<f64>,
}

impl From<StoredEmployerMatch> for EmployerMatch {
    fn from(stored: StoredEmployerMatch) -> Self {
        EmployerMatch { tiers: stored.tiers, annual_dollar_cap: stored.annual_dollar_cap }
    }
}

/// One `retirement_profiles` row, exactly as stored.
///
/// `employer_match_formula` is the raw `serde_json::Value` of the JSONB column,
/// not an `EmployerMatch`, so the DB boundary stays a faithful mirror of the
/// table. Code that needs the typed value decodes it explicitly and surfaces a
/// decode failure as a 500 rather than defaulting to "no match" — see
/// `upsert_profile`.
///
/// UNITS are the migration's UNITS block: the two contribution rates,
/// `expected_real_return` and `inflation_rate` are PERCENT; the
/// `target_replacement_ratio` is a FRACTION; `current_gross_income` and the
/// match formula's cap are DOLLARS; the two ages are WHOLE YEARS.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct RetirementProfile {
    pub id: Uuid,
    pub user_id: Uuid,
    pub member_ordinal: i32,
    pub country: String,
    pub birth_date: NaiveDate,
    pub target_retirement_age: i32,
    pub current_gross_income: f64,
    pub contribution_rate_pre_tax: f64,
    pub contribution_rate_roth: f64,
    pub employer_match_formula: serde_json::Value,
    pub expected_real_return: f64,
    pub inflation_rate: f64,
    pub life_expectancy_age: i32,
    pub target_replacement_ratio: f64,
    // --- Social Security (nels#466) ---
    //
    // ALL SIX ARE `Option`, AND THAT IS THE POINT. A user who has not entered a
    // Social Security figure has `None` here, never `Some(0.0)`. #469 renders a
    // missing figure as an explicitly labelled ABSENT segment of the stacked
    // income bar, because a zero segment reads as "you get nothing", which is
    // both alarming and wrong. Nothing on the path out of this struct is
    // permitted to collapse `None` into `0.0` — see `SocialSecurityView`.
    //
    // `#[serde(default)]` on each is for EXPLICITNESS and consistency with the
    // rest of this file, NOT because serde needs it: a derived `Deserialize`
    // already treats a missing `Option<T>` field as `None`. An earlier version
    // of this comment claimed the attribute was load-bearing; it is not, and the
    // claim was wrong. Keep the attribute — every other optional field here
    // carries it and an inconsistent one invites a reader to wonder why — but do
    // not repeat the false rationale.
    /// DOLLARS PER MONTH, as printed on the user's SSA statement. Not annual.
    #[serde(default)]
    pub ss_monthly_benefit: Option<f64>,
    /// MONTHS from birth (744 = 62y, 840 = 70y): the age the figure is quoted at.
    #[serde(default)]
    pub ss_benefit_at_age_months: Option<i32>,
    /// `'fra'` or `'explicit'` — WHY the months column has the value it has. See
    /// `SsAnchor` and the migration header: an `'fra'` anchor is re-derived from
    /// the birth date on every write so it cannot go stale.
    #[serde(default)]
    pub ss_benefit_at_age_anchor: Option<String>,
    /// MONTHS from birth: the age the user intends to claim at.
    #[serde(default)]
    pub ss_claiming_age_months: Option<i32>,
    #[serde(default)]
    pub ss_claiming_age_anchor: Option<String>,
    /// Provenance. `'user_entered'` is the only value v1 can produce, and no
    /// caller can set it — the write path derives it. See `SS_SOURCE_USER_ENTERED`.
    #[serde(default)]
    pub ss_source: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Lenient scalar deserializers for `RetirementProfileInput`.
//
// This struct is reached from the LLM. `AiActionParams` nests it, and a serde
// failure ANYWHERE inside `AiStructuredResponse` discards the ENTIRE chat turn
// through the malformed-response fallback — the user gets a generic "I had
// trouble" with no indication which field was wrong, and the model gets no
// signal to correct itself. This struct is also unusually exposed to that:
// `target_retirement_age` and `life_expectancy_age` are the ONLY integer-typed
// params reachable from `AiActionParams` (every other numeric there is `f64`),
// and `birth_date` is its only `NaiveDate` — the house pattern for a
// model-supplied date is `Option<String>` parsed leniently at the use site, as
// `target_date` does. So the strictest types in the whole params surface sit on
// the newest action, and `62.0`, `"62"`, `"120000"` and
// `"1985-06-15T00:00:00Z"` — all routine LLM formatting variants, none of them
// ambiguous — each sank the whole turn.
//
// These helpers accept the FORMATTING variants and nothing else. Two rules
// hold throughout:
//
//   * Never silently drop a field. REST shares this type, and quietly ignoring
//     a key the caller sent is worse than a 400 — it looks like a successful
//     save of a value that was never stored. Unparseable input still ERRORS.
//   * Never round. A float with a real fractional part on an AGE field is a
//     misunderstanding, not a format; `64.5` errors rather than becoming 64.
// ---------------------------------------------------------------------------

/// The shapes a number is allowed to arrive in. Untagged, so serde tries each
/// in turn and this stays deserializer-agnostic rather than assuming JSON.
#[derive(Deserialize)]
#[serde(untagged)]
enum LenientNumber {
    Int(i64),
    Float(f64),
    Str(String),
}

/// A whole-year integer field: a JSON integer, a float with NO fractional part,
/// or a string holding either. `64.5` and `"sixty"` are errors.
fn de_lenient_i32<'de, D>(d: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let Some(raw) = Option::<LenientNumber>::deserialize(d)? else {
        return Ok(None);
    };
    let whole = match raw {
        LenientNumber::Int(i) => i,
        LenientNumber::Float(f) => float_as_whole::<D>(f)?,
        LenientNumber::Str(s) => {
            let t = s.trim();
            match t.parse::<i64>() {
                Ok(i) => i,
                // `"62.0"` is the same formatting variant as bare `62.0`, so it
                // gets the same treatment — including the same refusal to round.
                Err(_) => match t.parse::<f64>() {
                    Ok(f) => float_as_whole::<D>(f)?,
                    Err(_) => {
                        return Err(D::Error::custom(format!(
                            "expected a whole number of years, got {s:?}"
                        )))
                    }
                },
            }
        }
    };
    i32::try_from(whole)
        .map(Some)
        .map_err(|_| D::Error::custom(format!("{whole} is out of range for a whole-year field")))
}

/// Shared by both integer paths: a float is acceptable only when it names an
/// exact integer. Rounding a half-year into an age would silently change the
/// caller's meaning, which is the one thing these helpers must not do.
fn float_as_whole<'de, D>(f: f64) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    if !f.is_finite() || f.fract() != 0.0 {
        return Err(D::Error::custom(format!(
            "expected a whole number of years, got {f} — ages are not rounded"
        )));
    }
    Ok(f as i64)
}

/// A float field: any JSON number, or a string holding one. Non-finite values
/// are rejected — they cannot survive the DB column and would poison #467's
/// math with a silent `NaN` rather than an error.
fn de_lenient_f64<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let Some(raw) = Option::<LenientNumber>::deserialize(d)? else {
        return Ok(None);
    };
    let v = match raw {
        LenientNumber::Int(i) => i as f64,
        LenientNumber::Float(f) => f,
        LenientNumber::Str(s) => s.trim().parse::<f64>().map_err(|_| {
            D::Error::custom(format!("expected a number, got {s:?}"))
        })?,
    };
    if !v.is_finite() {
        return Err(D::Error::custom(format!("expected a finite number, got {v}")));
    }
    Ok(Some(v))
}

/// A date field: `YYYY-MM-DD`, or an RFC3339-style value whose trailing time
/// component is discarded.
///
/// A date of birth is a DATE; a model that attaches midnight and a zone to it
/// has said the same thing in a different shape. Where an offset is present the
/// date is read IN THAT OFFSET, which is the day the caller wrote down.
/// Anything else errors — a birth date is far too load-bearing to guess at.
fn de_lenient_date<'de, D>(d: D) -> Result<Option<NaiveDate>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let Some(raw) = Option::<String>::deserialize(d)? else {
        return Ok(None);
    };
    let s = raw.trim();
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Some(date));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(dt.date_naive()));
    }
    // A timestamp with no zone at all, in either the `T` or space spelling.
    if let Some((head, _)) = s.split_once(['T', ' ']) {
        if let Ok(date) = NaiveDate::parse_from_str(head, "%Y-%m-%d") {
            return Ok(Some(date));
        }
    }
    Err(D::Error::custom(format!("expected a date as YYYY-MM-DD, got {raw:?}")))
}

/// How a caller named a Social Security age: either an explicit whole-year age,
/// or "my full retirement age" — which is a different thing and must stay a
/// different thing.
///
/// FRA is not a whole number of years for ten birth cohorts (1938-1942 and
/// 1955-1959 have FRAs from 65y2m to 66y10m). The FRA figure is also the most
/// prominent number on an SSA statement, so "the figure at my full retirement
/// age" is the single most likely thing a user copies. Forcing that through a
/// whole-year field would make a 1957 user pick between 66 and 67, each of which
/// misstates their implied PIA by about 3% — silently, and forever.
///
/// Keeping the anchor rather than only its resolved month count also keeps it
/// from going STALE. If the user later corrects their birth date — routine, and
/// the reason `de_lenient_date` exists — an `Fra` anchor re-derives to the new
/// FRA, while a stored bare month count would quietly stop meaning "at FRA" and
/// start meaning "some months early".
/// `Serialize` is HAND-WRITTEN so the type round-trips its own wire form.
///
/// A derived impl emits `"Fra"` and `{"Age":67}`, neither of which
/// `de_lenient_ss_anchor` accepts — so `RetirementProfileInput`, which derives
/// `Serialize` and is nested in `AiActionParams`, could emit a document it
/// cannot read back. Nothing does that today, which is exactly why it would be
/// a trap rather than a bug: it would sit here until something did. Emitting the
/// same `"fra"` / bare-number forms the deserializer accepts closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SsAnchor {
    /// The user's own full retirement age, whatever that turns out to be.
    Fra,
    /// An explicit whole-year age. Range-checked by `validate_profile`, NOT
    /// here — see `de_lenient_ss_anchor`.
    Age(i32),
}

impl Serialize for SsAnchor {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SsAnchor::Fra => s.serialize_str(SS_ANCHOR_FRA),
            SsAnchor::Age(years) => s.serialize_i32(*years),
        }
    }
}

/// The string a caller sends to mean [`SsAnchor::Fra`], and the value stored in
/// the `*_anchor` columns for it.
pub const SS_ANCHOR_FRA: &str = "fra";
/// The value stored in the `*_anchor` columns for [`SsAnchor::Age`].
pub const SS_ANCHOR_EXPLICIT: &str = "explicit";

/// A Social Security age field: a whole-year number in any of the formatting
/// variants the sibling helpers accept, or the case-insensitive string `"fra"`.
///
/// Deliberately hand-written rather than a `#[serde(untagged)]` derive. An
/// untagged enum over `i32` and `String` gets BOTH of the cases that matter
/// wrong, and both failures are silent or turn-destroying:
///
///  * `67.0` — a routine LLM float-for-integer — matches NEITHER variant, so it
///    is a hard serde error inside `AiActionParams`, which discards the ENTIRE
///    chat turn through the malformed-response fallback. That is exactly the
///    defect `de_lenient_i32` exists to prevent, handed straight back on the
///    newest fields.
///  * `"67"` — a quoted integer, equally routine — is swallowed by the `String`
///    variant and becomes the FRA anchor. For a 1960 birth year that silently
///    turns "quoted at 62" into "quoted at FRA 67", dividing the entered benefit
///    by 1.0 instead of 0.70: a ~43% error in a dollar figure, with no error
///    anywhere.
///
/// So the string branch parses a number FIRST and only then considers `"fra"`.
///
/// RANGE IS NOT CHECKED HERE, on purpose. `75` parses cleanly and is then
/// rejected by `validate_profile` with a message naming the field and the 62-70
/// range — a 400 the user can act on. A range violation raised as a serde error
/// would instead throw the whole chat turn away and tell them nothing.
fn de_lenient_ss_anchor<'de, D>(d: D) -> Result<Option<SsAnchor>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let Some(raw) = Option::<LenientNumber>::deserialize(d)? else {
        return Ok(None);
    };
    let years = match raw {
        LenientNumber::Int(i) => i,
        LenientNumber::Float(f) => float_as_whole::<D>(f)?,
        LenientNumber::Str(s) => {
            let t = s.trim();
            // Numbers first, so a quoted integer can never be mistaken for the
            // FRA keyword.
            if let Ok(i) = t.parse::<i64>() {
                i
            } else if let Ok(f) = t.parse::<f64>() {
                float_as_whole::<D>(f)?
            } else if t.eq_ignore_ascii_case(SS_ANCHOR_FRA)
                || t.to_ascii_lowercase().contains("full retirement age")
            {
                // `contains`, not equality, so "my full retirement age" and
                // "full retirement age (67)" are accepted. A model that phrases
                // the same unambiguous answer slightly differently must not cost
                // the user their entire chat turn — that is the whole reason
                // this deserializer is lenient. Numeric parsing still runs
                // FIRST, so a quoted integer can never reach this branch.
                return Ok(Some(SsAnchor::Fra));
            } else {
                return Err(D::Error::custom(format!(
                    "expected a whole age in years or the word \"fra\", got {s:?}"
                )));
            }
        }
    };
    i32::try_from(years)
        .map(|y| Some(SsAnchor::Age(y)))
        .map_err(|_| D::Error::custom(format!("{years} is out of range for an age in years")))
}

/// All-Option partial input, shared by the REST `PUT` payload and the chat
/// action params so the two write paths cannot drift.
///
/// `member_ordinal` is deliberately ABSENT: v1 only ever writes member 1 and
/// neither surface may create a second (#454 decision 6). An absent field means
/// "leave it alone" on an update and "use the named default" on a create — the
/// COALESCE semantics `goals::update_goal` already sets as the house pattern.
///
/// Every scalar below deserializes through a LENIENT helper. That is not
/// decoration: this struct is nested inside `AiActionParams`, and one strict
/// field rejecting a routine LLM formatting variant discards the whole chat
/// turn. See the helpers' comment block above for the rules they follow — in
/// particular that they widen the accepted FORMAT and never the accepted
/// MEANING, and never drop a field the caller sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetirementProfileInput {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_date")]
    pub birth_date: Option<NaiveDate>,
    #[serde(default, deserialize_with = "de_lenient_i32")]
    pub target_retirement_age: Option<i32>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub current_gross_income: Option<f64>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub contribution_rate_pre_tax: Option<f64>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub contribution_rate_roth: Option<f64>,
    /// WHOLE-OBJECT REPLACEMENT, unlike every other field on this struct.
    ///
    /// The scalars above are COALESCE-style: absent means "leave it alone". This
    /// one is absent-or-total. Sending it AT ALL replaces the entire stored
    /// formula, and any key omitted from the object you send falls to
    /// `EmployerMatch`'s own `#[serde(default)]` — empty `tiers`, no cap.
    ///
    /// The consequence is deliberate and worth stating plainly, because it is
    /// surprising: a caller with stored tiers who sends
    /// `PUT {"employer_match_formula": {"annual_dollar_cap": 5000}}` MEANING
    /// "add a cap to what I already have" gets all of their tiers WIPED, with a
    /// `200` and no warning. To add a cap you must resend the tiers alongside it.
    ///
    /// Whole-object replacement is kept rather than deep-merged because a tier
    /// LIST has no stable per-element identity to merge on: there is no key that
    /// says which stored tier an incoming tier is meant to update, so any merge
    /// rule would have to invent one - append? match on `employee_pct_up_to`? -
    /// and each choice makes some perfectly ordinary edit impossible. Notably,
    /// deep-merge would leave a caller no way to DELETE a tier at all. Replacement
    /// is the only rule under which every reachable formula is also expressible.
    ///
    /// Chat cannot reach this: prompt rule 21d forbids the model from setting
    /// `employer_match_formula`, so the wipe is a REST-only sharp edge. It is
    /// pinned by
    /// `resolve_input_replaces_the_whole_employer_match_rather_than_merging_it`
    /// so it stays a decision rather than an accident, and is called out in
    /// AGENTS.md section 21.
    #[serde(default)]
    pub employer_match_formula: Option<EmployerMatch>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub expected_real_return: Option<f64>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub inflation_rate: Option<f64>,
    #[serde(default, deserialize_with = "de_lenient_i32")]
    pub life_expectancy_age: Option<i32>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub target_replacement_ratio: Option<f64>,
    /// The figure printed on the user's SSA statement, in DOLLARS PER MONTH.
    ///
    /// This is an ENTERED figure, never an estimated one (#454 decision 1, which
    /// is binding). Nels does not derive a benefit from earnings history.
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub ss_monthly_benefit: Option<f64>,
    /// The age that figure is quoted at: a whole age, or `"fra"`.
    #[serde(default, deserialize_with = "de_lenient_ss_anchor")]
    pub ss_benefit_at_age: Option<SsAnchor>,
    /// The age the user intends to claim at: a whole age, or `"fra"`.
    #[serde(default, deserialize_with = "de_lenient_ss_anchor")]
    pub ss_claiming_age: Option<SsAnchor>,
    // NOTE there is no `ss_source` field, deliberately. v1 has exactly one legal
    // provenance and `resolve_input` derives it; letting a caller assert
    // provenance would make the column meaningless the moment a second value
    // exists.
}

/// The derived half of a profile's Social Security picture (nels#466).
///
/// Everything here is a pure function of the stored row; nothing is stored. See
/// `social_security_view`, which is the ONE constructor — the REST handlers and
/// the chat arm all go through it so they cannot drift on the region gate or on
/// the renormalization.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SocialSecurityView {
    pub full_retirement_age_years: i32,
    pub full_retirement_age_months: i32,
    /// The same FRA as a single month count, so a consumer never re-derives it
    /// with its own `years * 12 + months` and drifts.
    pub full_retirement_age_total_months: i32,
    /// PERCENT PER YEAR. Birth-year dependent: 8%/yr only from 1943.
    pub delayed_credit_pct_per_year: f64,
    /// The entered figure re-expressed at the intended claiming age, in DOLLARS
    /// PER MONTH and in TODAY'S DOLLARS.
    ///
    /// `None` unless a benefit, a quoted age AND a claiming age are all present.
    /// It is NEVER `Some(0.0)` standing in for "not enough information" — an
    /// entered benefit of $0 is a different fact from an unanswered question,
    /// and #469 renders them differently.
    ///
    /// NO INFLATION OR COLA IS APPLIED, in either direction. Social Security
    /// carries a statutory COLA, so an SSA-statement figure is already in today's
    /// dollars; #467 must carry this number through its real-return projection
    /// untouched. See `social_security::claiming_adjustment`'s contract note.
    pub adjusted_monthly_benefit: Option<f64>,
    /// True when the renormalization could NOT be computed even though all three
    /// inputs were present — i.e. a stored row that `claiming_adjustment`
    /// refuses.
    ///
    /// It exists so that `adjusted_monthly_benefit: None` has exactly ONE
    /// meaning per flag rather than two. Without it, "the user has not told us a
    /// claiming age yet" and "we have everything and the computation failed"
    /// are the same `None`, and a consumer cannot tell a question from a bug.
    /// The failure is also logged at `error` level; this flag is how the API
    /// says so.
    ///
    /// Only reachable from a row that bypassed both `validate_profile` and the
    /// migration's CHECKs — a restore, a backfill, a hand-edit, or a future
    /// writer. That is precisely why it is surfaced rather than swallowed.
    pub adjustment_unavailable: bool,
    /// True when the claiming age differs from the age the figure was quoted at,
    /// i.e. when Nels CHANGED the user's number.
    ///
    /// #466's compliance rule hangs on this: where an adjustment has been
    /// applied the UI says so and shows BOTH figures, because a user shown only
    /// an adjusted number will not recognise it and cannot check it.
    pub adjustment_applied: bool,
    /// Whether a figure was entered at all — the ABSENT-vs-ZERO distinction,
    /// made explicit rather than left to a consumer to infer from a `null`.
    ///
    /// `false` here means #469 renders an explicitly labelled "not entered"
    /// segment. It must never render a zero one.
    pub benefit_entered: bool,
}

/// Build the derived Social Security view for a stored row, or `None` when the
/// planner does not apply to that user's country.
///
/// The ONE constructor, called by the `GET` handler, the `PUT` handler and the
/// chat arm alike. That structural sharing is why the three surfaces cannot
/// disagree about the region gate, the FRA table or the renormalization — the
/// same reason `upsert_profile` is the one write path.
///
/// Pure: no clock, no pool. A row is all it needs.
pub fn social_security_view(profile: &RetirementProfile) -> Option<SocialSecurityView> {
    if !retirement_supported_country(&profile.country) {
        return None;
    }
    let birth_year = crate::social_security::fra_birth_year(profile.birth_date);
    let fra = crate::social_security::full_retirement_age(birth_year);

    // All three inputs, or no adjusted figure. A partial answer is reported as
    // absent rather than filled in with a guess — the guess would be a number the
    // user never gave, rendered as though they had.
    //
    // The error is NOT discarded with `.ok()`. An earlier version did exactly
    // that, and it produced the one state this feature must never reach: a row
    // reporting `adjustment_applied: true` alongside `adjusted_monthly_benefit:
    // null`, which contradicts `SocialSecurityView`'s own contract that where an
    // adjustment was applied BOTH figures are shown. It also made the chat arm
    // tell a user who HAD supplied a claiming age to supply their claiming age.
    // A stored row our own math refuses is our bug, so it is logged loudly and
    // flagged on the response rather than rendered as a plausible absence.
    let mut adjustment_unavailable = false;
    let adjusted = match (
        profile.ss_monthly_benefit,
        profile.ss_benefit_at_age_months,
        profile.ss_claiming_age_months,
    ) {
        (Some(benefit), Some(quoted), Some(claiming)) => {
            match crate::social_security::claiming_adjustment(
                benefit,
                quoted,
                claiming,
                profile.birth_date,
            ) {
                Ok(value) => Some(value),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        user_id = %profile.user_id,
                        quoted_age_months = quoted,
                        claiming_age_months = claiming,
                        "stored retirement profile failed Social Security renormalization; \
                         it bypassed validate_profile or a CHECK",
                    );
                    adjustment_unavailable = true;
                    None
                }
            }
        }
        _ => None,
    };

    Some(SocialSecurityView {
        full_retirement_age_years: fra.years,
        full_retirement_age_months: fra.months,
        full_retirement_age_total_months: fra.total_months(),
        delayed_credit_pct_per_year: crate::social_security::delayed_credit_pct_per_year(birth_year),
        adjusted_monthly_benefit: adjusted,
        adjustment_unavailable,
        // Gated on `adjusted.is_some()`, NOT merely on the two ages differing.
        // The UI promises to show BOTH figures whenever this is set, and it
        // cannot keep that promise with only one of them. Two states make the
        // narrower gate necessary: a renormalization that failed, and — the one
        // that is perfectly legal — a user who has given both AGES but not the
        // benefit yet, which the migration explicitly permits as "a user
        // part-way through". Both would otherwise report that Nels changed a
        // number it never had.
        adjustment_applied: adjusted.is_some()
            && match (profile.ss_benefit_at_age_months, profile.ss_claiming_age_months) {
                (Some(quoted), Some(claiming)) => quoted != claiming,
                _ => false,
            },
        benefit_entered: profile.ss_monthly_benefit.is_some(),
    })
}

/// A fully merged, non-Option profile: the payload's supplied fields overlaid on
/// either the stored row or the named defaults. This is the carrier
/// `validate_profile` reads and the value the upsert path binds, so validation
/// is TOTAL — a partial update cannot half-write its way into an invalid state,
/// because the merged result is what gets checked.
///
/// `target_retirement_age_was_defaulted` is not a stored column. It records that
/// the caller never sent `target_retirement_age` and the named default was
/// substituted for them. It gates the nels#533 stale-default warning
/// (`stale_defaulted_age_warning`) — a first create that omits the age no longer
/// REJECTS when the default lands in the past; it saves and warns — and it lets
/// the (no longer reachable for this case) rejection message say so rather than
/// blame the user for a number they never gave.
///
/// `target_retirement_age_is_being_set` is likewise not stored. It records
/// whether THIS write is the one choosing the target retirement age — true on
/// every create, and on an update only when the caller actually supplied the
/// field. `validate_profile`'s "must be in the future" rule keys off it, because
/// that rule is about the value the caller is SETTING, not about a stored
/// historical fact. See the rule's own comment for why.
///
/// UNITS follow the migration's UNITS block exactly: the two contribution rates,
/// `expected_real_return` and `inflation_rate` are PERCENT; the
/// `target_replacement_ratio` is a FRACTION.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub country: String,
    pub birth_date: NaiveDate,
    pub target_retirement_age: i32,
    pub target_retirement_age_was_defaulted: bool,
    pub target_retirement_age_is_being_set: bool,
    pub current_gross_income: f64,
    pub contribution_rate_pre_tax: f64,
    pub contribution_rate_roth: f64,
    pub employer_match_formula: EmployerMatch,
    pub expected_real_return: f64,
    pub inflation_rate: f64,
    pub life_expectancy_age: i32,
    pub target_replacement_ratio: f64,
    /// Social Security, fully resolved (nels#466). Each stays `Option` all the
    /// way through — a merged profile with no SS figure is a legitimate,
    /// complete profile, not an incomplete one, and must never resolve to zero.
    pub ss_monthly_benefit: Option<f64>,
    /// The anchor pair: `(months from birth, 'fra' | 'explicit')`. Kept
    /// together because the migration's pairing CHECK requires both or neither,
    /// and because separating them is how one of them gets forgotten.
    pub ss_benefit_at_age: Option<(i32, &'static str)>,
    pub ss_claiming_age: Option<(i32, &'static str)>,
    pub ss_source: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Default assumptions (#454 decision 3).
//
// These are named constants rather than SQL DEFAULTs or magic numbers so there
// is exactly ONE source of truth and #469 can render each one alongside its
// provenance. Every numeric planning default below is a CONVENTIONAL PLANNING
// FIGURE, NOT A NELS FORECAST — that phrase is the provenance the issue's
// "labelled with where the default came from" requirement asks for, and the
// disclosure copy quotes it.
//
// UNITS are stated explicitly on each constant because #467's projection math
// is wrong by a factor of 100 if percent-scale and fraction-scale are confused.
// ---------------------------------------------------------------------------

/// Default target retirement age, in WHOLE YEARS.
/// 65 is a conventional planning figure, not a Nels forecast.
pub const DEFAULT_TARGET_RETIREMENT_AGE: i32 = 65;

/// Default expected investment return, as PERCENT PER YEAR and REAL, i.e. already
/// net of inflation, not nominal (`5.0` = 5%/yr).
/// 5% real is a conventional planning figure, not a Nels forecast.
pub const DEFAULT_EXPECTED_REAL_RETURN: f64 = 5.0;

/// Default inflation assumption, as PERCENT PER YEAR (`2.5` = 2.5%/yr).
/// 2.5% is a conventional planning figure, not a Nels forecast.
pub const DEFAULT_INFLATION_RATE: f64 = 2.5;

/// Default planning life expectancy, in WHOLE YEARS.
/// 90 is a conventional planning figure, not a Nels forecast.
pub const DEFAULT_LIFE_EXPECTANCY_AGE: i32 = 90;

/// Default share of current gross income the user wants to replace in
/// retirement, as a FRACTION and NOT a percent (`0.75` = 75%). It is
/// fraction-scaled because the issue calls it a *ratio* and gives `0.75`,
/// unlike the percent-scaled rate fields.
/// 0.75 is a conventional planning figure, not a Nels forecast.
pub const DEFAULT_TARGET_REPLACEMENT_RATIO: f64 = 0.75;

/// Default employer match: NO tiers and no cap, i.e. no employer match assumed
/// until the user tells us about one.
///
/// The issue says "default is a single tier", which is read as a statement about
/// the *shape* users edit into rather than a licence to invent a match
/// percentage nobody stated. A nonzero default would overstate retirement
/// readiness — exactly the direction of error nels#465's own employer-match
/// paragraph warns about. The evaluator handles 0..N tiers; one tier is simply
/// its ordinary case.
pub fn default_employer_match() -> EmployerMatch {
    EmployerMatch { tiers: vec![], annual_dollar_cap: None }
}

// ---------------------------------------------------------------------------
// Plausibility guards.
//
// Every constant below is a PLAUSIBILITY GUARD, NOT A FORECAST and NOT a
// modelling opinion — none of them says anything about what a return, an
// inflation rate or a lifespan will actually be. They exist because
// `validate_profile` is the ONLY choke point between a caller and #467's
// projection math, and the rate fields already have a lower bound but had no
// upper one at all. The failure they catch is SCALE CONFUSION, which the
// migration header, every default's doc comment and prompt rule 21d all warn
// about: `target_replacement_ratio: 75.0` — a user or a model saying "75%" on
// the field that is a FRACTION — passes every other rule and stores a number
// 100x off, and the projection is then wrong by that factor with no signal.
//
// Each bound is deliberately far outside any real value so it can only ever
// fire on a mistake, and each message NAMES THE EXPECTED SCALE so the caller can
// fix the input rather than guess. The comparison is strictly greater-than, so
// the constant itself is accepted.
// ---------------------------------------------------------------------------

/// Plausibility guard, not a forecast: the largest accepted
/// `target_replacement_ratio`, which is a FRACTION of gross (`0.75` = 75%).
///
/// 2.0 means "replace twice your current gross income in retirement" — already
/// far beyond any real plan. Anything above it is almost certainly a PERCENT
/// that should have been a fraction, which is the single most likely scale
/// mistake on this table.
pub const MAX_PLAUSIBLE_TARGET_REPLACEMENT_RATIO: f64 = 2.0;

/// Plausibility guard, not a forecast: the largest accepted
/// `expected_real_return`, in PERCENT PER YEAR (`5.0` = 5%/yr).
///
/// 30%/yr real, sustained for a whole working life, is roughly double the best
/// long-run equity record anywhere; the realistic reading of a larger number is
/// that a fraction was typed where a percent belongs, or a nominal headline
/// figure landed on a real-return field.
pub const MAX_PLAUSIBLE_EXPECTED_REAL_RETURN: f64 = 30.0;

/// Plausibility guard, not a forecast: the largest accepted `inflation_rate`, in
/// PERCENT PER YEAR (`2.5` = 2.5%/yr).
///
/// Looser than the return bound on purpose — sustained high inflation is a real
/// thing in some economies and this column must stay able to record it. 50%/yr
/// is well past anything a US-gated v1 planner will legitimately see.
pub const MAX_PLAUSIBLE_INFLATION_RATE: f64 = 50.0;

/// Plausibility guard, not a forecast: the largest accepted
/// `life_expectancy_age`, in WHOLE YEARS.
///
/// 130 is comfortably beyond the longest verified human lifespan, so it only
/// ever catches a typo or a units mix-up while leaving every real planning
/// horizon — including deliberately conservative ones — untouched.
pub const MAX_PLAUSIBLE_LIFE_EXPECTANCY_AGE: i32 = 130;

/// Plausibility guard, not a forecast: the largest accepted
/// `current_gross_income`, in ANNUAL DOLLARS.
///
/// $100,000,000 a year is not a salary anyone types by accident; a larger figure
/// is a stray digit or a currency confusion, and it would dominate every derived
/// number in the projection.
pub const MAX_PLAUSIBLE_CURRENT_GROSS_INCOME: f64 = 100_000_000.0;

/// The only `ss_source` value v1 can produce (#454 decision 1).
///
/// It is DERIVED by `resolve_input`, never supplied by a caller: v1 has exactly
/// one legal provenance, so a settable field would be noise now and a lie later.
/// The column exists so that if Nels ever does compute its own figure, adding
/// `'estimated'` costs a CHECK widening rather than a migration on a table that
/// by then has rows.
pub const SS_SOURCE_USER_ENTERED: &str = "user_entered";

/// Plausibility guard, not a forecast: the largest accepted
/// `ss_monthly_benefit`, in DOLLARS PER MONTH.
///
/// The 2025 SSA maximum at 70 is roughly $5,100/month, so $10,000 is about twice
/// any real benefit and can only fire on a mistake. The mistake it is actually
/// aimed at is an ANNUAL figure typed into a monthly field: a $30,000/yr benefit
/// entered as `30000` is caught here rather than tripling the projected
/// retirement income. It does NOT catch every such slip — an annual $9,000 is
/// under the bound — which is why the message names the scale rather than
/// implying the guard is a proof.
pub const MAX_PLAUSIBLE_SS_MONTHLY_BENEFIT: f64 = 10_000.0;

/// Pure evaluator for a tiered employer match, in DOLLARS.
///
/// Tiers are ordered and CUMULATIVE: `employee_pct_up_to` is the cumulative
/// employee-contribution percentage at which a tier stops, so tier *i* covers
/// the band `(previous_up_to, up_to_i]` and only that band.
///
/// ```text
/// matched = SUM_i gross x (band_i_covered_pct / 100) x (match_pct_i / 100)
///   where band_i_covered_pct = max(0, min(employee_pct, up_to_i) - up_to_{i-1})
/// final   = matched.min(cap).max(0.0)   when a cap is present
///           matched.max(0.0)            otherwise
/// ```
///
/// Worked example — "100% of the first 3%, then 50% of the next 2%" on a
/// $100,000 gross while contributing 5%: tiers `[{3, 100}, {5, 50}]` give
/// `100000 x 3% x 100% = 3000` plus `100000 x 2% x 50% = 1000`, i.e. **$4000**.
/// Contributing more than the top tier's 5% adds nothing: the excess is
/// unmatched.
///
/// UNITS: `gross_income` and the cap are DOLLARS; `employee_contribution_pct`,
/// `employee_pct_up_to` and `match_pct` are all PERCENT.
///
/// INVARIANT — this formula ASSUMES a STRICTLY INCREASING tier list. It
/// subtracts the PREVIOUS tier's `up_to` rather than a running maximum, which is
/// exactly what nels#465 specifies, but on a list like `[{5,100},{3,100},{7,100}]`
/// it charges 9 percentage points of match against a top ceiling of only 7, i.e.
/// it OVER-states the employer match. That invariant is enforced by
/// `validate_profile`, which every write path runs, so a non-increasing list
/// cannot reach storage; this evaluator therefore stays faithful to the
/// documented formula instead of quietly diverging from it. If you ever call
/// this on a list that did not come through `validate_profile`, validate first.
///
/// Defensive clamps: a negative gross, a negative employee percentage, a
/// negative `match_pct` and a negative cap all collapse to `0.0`, and a
/// non-monotonic tier list contributes 0 for the out-of-order band via the
/// `max(0.0, ...)` rather than panicking or going negative. The trailing
/// `.max(0.0)` composes with `.min(cap)` so the negative-cap clamp and the cap
/// clamp are one expression rather than two rules a reader must reconcile. The
/// result is never negative and there is no division by any input.
pub fn employer_match(gross_income: f64, employee_contribution_pct: f64, formula: &EmployerMatch) -> f64 {
    let gross = gross_income.max(0.0);
    let employee_pct = employee_contribution_pct.max(0.0);

    let mut matched = 0.0;
    let mut prev_up_to = 0.0;
    for tier in &formula.tiers {
        let band_pct = (employee_pct.min(tier.employee_pct_up_to) - prev_up_to).max(0.0);
        matched += gross * (band_pct / 100.0) * (tier.match_pct.max(0.0) / 100.0);
        prev_up_to = tier.employee_pct_up_to;
    }

    match formula.annual_dollar_cap {
        Some(cap) => matched.min(cap).max(0.0),
        None => matched.max(0.0),
    }
}

/// Age in WHOLE YEARS on `today`, by `NaiveDate` component arithmetic: the year
/// difference, minus one when this year's `(month, day)` has not occurred yet.
///
/// `today` is a parameter rather than `Utc::now()` so the function — and every
/// validation rule built on it — stays pure and unit-testable. No `chrono`
/// duration division is used: dividing elapsed days by 365.25 gets leap-day
/// birth dates wrong, and a 29 Feb birth date simply has no 29th in a non-leap
/// year, which component comparison handles correctly by rolling the birthday
/// to 1 March.
pub fn current_age(birth_date: NaiveDate, today: NaiveDate) -> i32 {
    let mut age = today.year() - birth_date.year();
    if (today.month(), today.day()) < (birth_date.month(), birth_date.day()) {
        age -= 1;
    }
    age
}

/// Closed-set region gate for the retirement planner (nels#465, #454 decision 2).
/// v1 is US-only.
///
/// Mirrors `bank_provider::Provider::for_country`'s closed-set, case-insensitive
/// style deliberately: adding a country later costs one match arm, not an
/// abstraction.
///
/// This is NOT inferred from linked accounts. A user can legitimately link
/// accounts in several countries; this reads the user's own stated `country`
/// column on `retirement_profiles`, which is the only place in the database
/// that records where a *user* lives.
pub fn retirement_supported_country(cc: &str) -> bool {
    matches!(cc.to_uppercase().as_str(), "US")
}

/// Validate a merged profile. `Ok(())` or a single human-readable message the
/// caller maps to a 400 (REST) or a chat `mutation_error`.
///
/// Rule order is fixed and deliberate so the message a caller gets is
/// deterministic rather than dependent on field iteration:
/// country -> birth date -> income -> each rate -> employer match tiers ->
/// contribution sum -> retirement age vs current age -> life expectancy vs
/// retirement age.
///
/// The birth-date rule sits early because every later age-derived rule depends
/// on it, and the tier rules sit here rather than inside `employer_match`
/// because this function is the single choke point every write path passes
/// through — see the invariant note on `employer_match`.
///
/// `today` is a parameter, not `Utc::now()`, so this stays pure and testable.
pub fn validate_profile(p: &ResolvedProfile, today: NaiveDate) -> Result<(), String> {
    if p.country.len() != 2 || !p.country.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err("country must be a two-letter ISO 3166-1 alpha-2 code".to_string());
    }

    // Rejected BEFORE any age-derived rule. `current_age` returns a negative
    // number for a future birth date, which would make the
    // `target_retirement_age <= current_age` rule pass trivially, so a nonsense
    // date would save cleanly and silently poison #467's projection math. Born
    // TODAY is allowed: it is not a date in the future, and anything wrong that
    // follows from an age of 0 belongs to the retirement-age rule, not this one.
    if p.birth_date > today {
        return Err("birth date cannot be in the future".to_string());
    }

    if p.current_gross_income < 0.0 {
        return Err("current gross income cannot be negative".to_string());
    }
    if p.current_gross_income > MAX_PLAUSIBLE_CURRENT_GROSS_INCOME {
        return Err(format!(
            "current gross income is ANNUAL dollars and cannot be more than {MAX_PLAUSIBLE_CURRENT_GROSS_INCOME:.0}"
        ));
    }

    // The five percent/fraction-scaled rate fields. Named with their payload
    // field names so a 400 tells the caller exactly which key to fix.
    for (name, value) in [
        ("contribution_rate_pre_tax", p.contribution_rate_pre_tax),
        ("contribution_rate_roth", p.contribution_rate_roth),
        ("expected_real_return", p.expected_real_return),
        ("inflation_rate", p.inflation_rate),
        ("target_replacement_ratio", p.target_replacement_ratio),
    ] {
        if value < 0.0 {
            return Err(format!("{name} cannot be negative"));
        }
    }

    // Upper PLAUSIBILITY guards. These are not forecasts and not opinions about
    // the numbers — see the constants' own comment block. They exist because
    // this function is the only thing standing between a caller and #467's math,
    // and a rate field with a floor but no ceiling lets the single most likely
    // mistake on this table — a percent typed into the fraction-scaled
    // `target_replacement_ratio` — store a value 100x off and validate cleanly.
    // Each message names the SCALE so the fix is obvious.
    if p.target_replacement_ratio > MAX_PLAUSIBLE_TARGET_REPLACEMENT_RATIO {
        return Err(format!(
            "target_replacement_ratio is a FRACTION of gross income, not a percent — 0.75 means 75% — so it cannot be more than {MAX_PLAUSIBLE_TARGET_REPLACEMENT_RATIO}"
        ));
    }
    if p.expected_real_return > MAX_PLAUSIBLE_EXPECTED_REAL_RETURN {
        return Err(format!(
            "expected_real_return is a PERCENT per year, so 5 means 5% — it cannot be more than {MAX_PLAUSIBLE_EXPECTED_REAL_RETURN}"
        ));
    }
    if p.inflation_rate > MAX_PLAUSIBLE_INFLATION_RATE {
        return Err(format!(
            "inflation_rate is a PERCENT per year, so 2.5 means 2.5% — it cannot be more than {MAX_PLAUSIBLE_INFLATION_RATE}"
        ));
    }
    if p.life_expectancy_age > MAX_PLAUSIBLE_LIFE_EXPECTANCY_AGE {
        return Err(format!(
            "life expectancy age is in whole years and cannot be more than {MAX_PLAUSIBLE_LIFE_EXPECTANCY_AGE}"
        ));
    }

    // Employer-match tier list. `employer_match`'s cumulative-band formula
    // subtracts the PREVIOUS tier's ceiling rather than a running maximum, which
    // is faithful to the issue's formula but only well behaved on a strictly
    // increasing list: tiers whose ceilings go 5 -> 3 -> 7 would charge 9
    // percentage points of match against a top ceiling of only 7, OVER-stating
    // the employer match — the exact direction of error nels#465's employer-match
    // paragraph warns about. Rather than patch the evaluator away from the
    // documented formula, this rule makes the pathological list unrepresentable:
    // it runs on every write path, so no such formula can reach storage.
    let mut prev_up_to: Option<f64> = None;
    for tier in &p.employer_match_formula.tiers {
        if tier.employee_pct_up_to < 0.0 {
            return Err("employer match employee_pct_up_to cannot be negative".to_string());
        }
        if tier.match_pct < 0.0 {
            return Err("employer match match_pct cannot be negative".to_string());
        }
        if let Some(prev) = prev_up_to {
            if tier.employee_pct_up_to <= prev {
                return Err(
                    "employer match tiers must be ordered with strictly increasing employee_pct_up_to values"
                        .to_string(),
                );
            }
        }
        prev_up_to = Some(tier.employee_pct_up_to);
    }
    if p.employer_match_formula.annual_dollar_cap.is_some_and(|cap| cap < 0.0) {
        return Err("employer match annual_dollar_cap cannot be negative".to_string());
    }

    // Both rates are PERCENT of gross, which is the only scale on which the
    // issue's own "summing above 100" rule is meaningful. Exactly 100 is allowed.
    if p.contribution_rate_pre_tax + p.contribution_rate_roth > 100.0 {
        return Err("contribution rates cannot add up to more than 100% of gross income".to_string());
    }

    // "Target retirement age must be in the future" is a rule about the value
    // THIS WRITE IS SETTING, which is why it is gated on
    // `target_retirement_age_is_being_set` rather than run unconditionally.
    //
    // `validate_profile` runs on the MERGED profile, so an unconditional rule
    // would evaluate a STORED, historical value on every later write. A profile
    // legitimately created at 64 with a target of 65 would then become
    // permanently uneditable from the user's 65th birthday: not their income,
    // not their assumptions, via neither REST nor chat, because a partial update
    // touching only `inflation_rate` would still re-check the old age against
    // today. The stored value is a historical fact and must not block unrelated
    // edits; the rule fires on every create, and on an update only when the
    // caller actually supplies the field.
    //
    // The life-expectancy rule below stays UNCONDITIONAL on purpose: it compares
    // two stored values to each other and has no dependence on the clock, so it
    // can never start failing merely because time passed.
    let age = current_age(p.birth_date, today);
    // The rule still bites whenever THIS WRITE actually chose the age — every
    // create (whether the caller named the age or the default was substituted)
    // and an update that supplies the field. nels#533 (fleshed out) relaxes the
    // SUBSTITUTED-DEFAULT case: the default of 65 being behind a user over 65 is
    // not the user's error, and rejecting the whole create threw away a valid
    // multi-turn gather of country / birth date / income because of a number the
    // user never chose. That case now SAVES (so the follow-up "I want to retire
    // at 75" merges onto a stored row instead of starting a new create) and
    // surfaces the problem via `stale_defaulted_age_warning` rather than
    // failing. An EXPLICITLY-set past age is still the caller's own choice and
    // is still rejected.
    if p.target_retirement_age_is_being_set
        && p.target_retirement_age <= age
        && !p.target_retirement_age_was_defaulted
    {
        return Err("target retirement age must be greater than your current age".to_string());
    }

    if p.life_expectancy_age <= p.target_retirement_age {
        return Err("life expectancy age must be greater than the target retirement age".to_string());
    }

    // --- Social Security (nels#466) ---
    //
    // NOTE WHAT IS ABSENT HERE: not one of these rules compares a Social
    // Security age against the user's CURRENT age or birth date. That is
    // deliberate and it is the #465 froze-the-profile defect class being avoided
    // by construction. A 68-year-old who already claimed at 65 must be able to
    // record that, and a profile saved legally at 61 must not become uneditable
    // on its owner's 62nd birthday. The 62-70 bounds below are STATUTORY
    // constants, not clock-dependent comparisons, so no passage of time can turn
    // a valid stored profile into an invalid one.
    if let Some(benefit) = p.ss_monthly_benefit {
        // Messages name the PAYLOAD KEY as well as the prose, per this
        // function's own convention — there are TWO Social Security age keys and
        // a caller must not have to reverse-engineer which one to fix.
        if !benefit.is_finite() {
            return Err("ss_monthly_benefit must be a finite number of MONTHLY dollars".to_string());
        }
        if benefit < 0.0 {
            return Err("ss_monthly_benefit is MONTHLY dollars and cannot be negative".to_string());
        }
        if benefit > MAX_PLAUSIBLE_SS_MONTHLY_BENEFIT {
            return Err(format!(
                "ss_monthly_benefit is the MONTHLY dollar figure from your statement, not the annual one, so it cannot be more than {MAX_PLAUSIBLE_SS_MONTHLY_BENEFIT:.0}"
            ));
        }
        // A dollar figure with no age attached cannot be renormalized to any
        // other claiming age, so it is not a usable input — it is a half-entered
        // one. Caught here as a 400 the user can act on rather than as the
        // migration's CHECK, which would surface as a 500.
        if p.ss_benefit_at_age.is_none() {
            // Phrased as a question and marked with `SS_NEEDS_QUOTED_AGE` so the
            // chat arm routes it down the CLARIFYING channel rather than the
            // warning one. See that constant's doc comment.
            return Err(format!(
                "tell me {SS_NEEDS_QUOTED_AGE} — send `ss_benefit_at_age` as a whole age from 62 to 70, or \"fra\" for your full retirement age"
            ));
        }
    }

    for (name, anchor) in [
        ("ss_benefit_at_age, the age your Social Security figure is quoted at", p.ss_benefit_at_age),
        ("ss_claiming_age, the age you intend to claim at", p.ss_claiming_age),
    ] {
        if let Some((months, _)) = anchor {
            if !(crate::social_security::MIN_CLAIM_AGE_MONTHS
                ..=crate::social_security::MAX_CLAIM_AGE_MONTHS)
                .contains(&months)
            {
                // Rejected, never clamped. Quietly rewriting 71 to 70 or 61 to 62
                // would hide a misunderstanding behind a plausible number.
                return Err(format!(
                    "{name} must be between 62 and 70 — Social Security cannot be claimed before 62, and no credit accrues after 70 (got {} years {} months)",
                    months / 12,
                    months % 12
                ));
            }
        }
    }

    Ok(())
}

/// The warning for the nels#533 create case: a user for whom the substituted
/// default target age is already behind them.
///
/// This is the exact case `validate_profile` USED to reject as a hard error — a
/// 71-year-old creating a profile without stating a target age had the WHOLE
/// create refused over the default of 65, throwing away their country / birth
/// date / income gathered across several turns. It now SAVES, and this function
/// is how the save surfaces that the placeholder age is wrong and a real one is
/// needed.
///
/// It fires only when the age on THIS write is a substituted default
/// (`target_retirement_age_was_defaulted` is true only on a first create that
/// omitted the field), never when the caller explicitly set the age (that case
/// is still a hard 400 from `validate_profile`) and never on an update that
/// merely carries a historical 65-target forward — the C1 "do not re-judge
/// stored values" rule, applied to warnings as well as rejections.
///
/// Called from `upsert_profile`, the ONE write path, so chat and REST receive
/// the same text and cannot drift.
pub fn stale_defaulted_age_warning(p: &ResolvedProfile, today: NaiveDate) -> Option<String> {
    let age = current_age(p.birth_date, today);
    if p.target_retirement_age_is_being_set
        && p.target_retirement_age_was_defaulted
        && p.target_retirement_age <= age
    {
        Some(format!(
            "no target retirement age was given, so the default of {} was used, but it is \
             behind your current age of {age} — tell me the age you want to retire at and I'll update it",
            p.target_retirement_age
        ))
    } else {
        None
    }
}

/// The tail of every "you have not given me enough to create a profile yet"
/// message `resolve_input` produces, e.g. `"birth_date is required to create a
/// retirement profile"`.
///
/// Exported as a constant with exactly ONE producer because the chat arm has to
/// tell that case apart from a genuine validation failure: "I still need your
/// date of birth" is a CLARIFYING QUESTION that belongs in `response_text`,
/// while "target retirement age must be greater than your current age" is a real
/// rejection that belongs in the `⚠️ System Update` warning channel. Matching on
/// a shared constant keeps the two ends from drifting; see
/// `rag::chat_set_retirement_profile`.
pub const MISSING_REQUIRED_FIELD_SUFFIX: &str = "is required to create a retirement profile";

/// The marker phrase in the "a benefit needs an age attached" validation
/// message, exported for exactly the same reason as
/// `MISSING_REQUIRED_FIELD_SUFFIX` and with exactly one producer.
///
/// "Which age is that figure quoted at?" is a CLARIFYING QUESTION, not a
/// rejection: the user gave us something real and we need one more thing to use
/// it. Returned down `chat_set_retirement_profile`'s error half it would render
/// as `**System Update:** ⚠️ …` — a warning marker on a turn where the user did
/// nothing wrong — which is precisely what this file's #300 precedent forbids
/// and what `MISSING_REQUIRED_FIELD_SUFFIX` already exists to prevent for the
/// create-field case.
///
/// It is highly reachable, not theoretical: prompt rule 21d tells the model to
/// populate ONLY the fields the user stated, and the most likely first Social
/// Security utterance — "my statement says $2,400 a month" — supplies the
/// benefit alone.
pub const SS_NEEDS_QUOTED_AGE: &str = "which age your Social Security figure is quoted at";

/// Merge a partial payload onto either an existing row or the named defaults,
/// producing the total value `validate_profile` checks and the upsert binds.
///
/// Pure on purpose — no pool, no clock — so every merge rule is unit-testable
/// without a database.
///
/// `existing` carries the row together with its ALREADY-DECODED
/// `employer_match_formula`. Decoding happens in `upsert_profile` rather than
/// here so that this function's `Err(String)` means exactly one thing: the
/// CALLER's input is wrong and the answer is a 400. Corrupt JSONB in our own
/// table is our bug, not theirs, and is a 500 raised before this is called.
///
/// Create semantics: `country`, `birth_date` and `current_gross_income` are
/// required, because they are the only three fields that cannot be defaulted
/// without fabricating a fact about the user. Everything else falls back to its
/// named constant. Update semantics: every absent field falls back to the stored
/// value, matching `goals::update_goal`'s COALESCE style.
///
/// `country` is trimmed and UPPERCASED here. That is load-bearing: the DB CHECK
/// is `^[A-Z]{2}$` while `validate_profile` deliberately accepts either case, so
/// without this a lowercase `"us"` would pass validation and then fail the
/// constraint as a 500 instead of saving.
pub fn resolve_input(
    existing: Option<(&RetirementProfile, EmployerMatch)>,
    input: &RetirementProfileInput,
) -> Result<ResolvedProfile, String> {
    let row = existing.as_ref().map(|(r, _)| *r);

    let missing = |field: &str| format!("{field} {MISSING_REQUIRED_FIELD_SUFFIX}");

    let country = match input.country.as_deref() {
        Some(c) => c.trim().to_uppercase(),
        None => row.map(|r| r.country.clone()).ok_or_else(|| missing("country"))?,
    };
    let birth_date = match input.birth_date {
        Some(b) => b,
        None => row.map(|r| r.birth_date).ok_or_else(|| missing("birth_date"))?,
    };
    let current_gross_income = match input.current_gross_income {
        Some(i) => i,
        None => row
            .map(|r| r.current_gross_income)
            .ok_or_else(|| missing("current_gross_income"))?,
    };

    // True only when the caller omitted the age AND there was no stored value to
    // fall back on, i.e. the named default was substituted FOR them. On an
    // update the stored value is the user's own number from an earlier turn, so
    // the validation message must not blame the substitution.
    let target_retirement_age_was_defaulted =
        input.target_retirement_age.is_none() && row.is_none();

    // True when THIS write is the one choosing the target retirement age: every
    // create (whether the caller named the age or the default was substituted),
    // and an update only when the caller actually supplied the field. An update
    // that leaves it alone is carrying a historical value forward and must not
    // be re-judged against today's date — see `validate_profile`'s rule.
    let target_retirement_age_is_being_set =
        input.target_retirement_age.is_some() || row.is_none();

    let merged_ss_benefit =
        input.ss_monthly_benefit.or_else(|| row.and_then(|r| r.ss_monthly_benefit));

    Ok(ResolvedProfile {
        country,
        birth_date,
        target_retirement_age: input
            .target_retirement_age
            .or_else(|| row.map(|r| r.target_retirement_age))
            .unwrap_or(DEFAULT_TARGET_RETIREMENT_AGE),
        target_retirement_age_was_defaulted,
        target_retirement_age_is_being_set,
        current_gross_income,
        // Nothing is assumed about what the user contributes: the default is 0.
        contribution_rate_pre_tax: input
            .contribution_rate_pre_tax
            .or_else(|| row.map(|r| r.contribution_rate_pre_tax))
            .unwrap_or(0.0),
        contribution_rate_roth: input
            .contribution_rate_roth
            .or_else(|| row.map(|r| r.contribution_rate_roth))
            .unwrap_or(0.0),
        employer_match_formula: input
            .employer_match_formula
            .clone()
            .or_else(|| existing.map(|(_, m)| m))
            .unwrap_or_else(default_employer_match),
        expected_real_return: input
            .expected_real_return
            .or_else(|| row.map(|r| r.expected_real_return))
            .unwrap_or(DEFAULT_EXPECTED_REAL_RETURN),
        inflation_rate: input
            .inflation_rate
            .or_else(|| row.map(|r| r.inflation_rate))
            .unwrap_or(DEFAULT_INFLATION_RATE),
        life_expectancy_age: input
            .life_expectancy_age
            .or_else(|| row.map(|r| r.life_expectancy_age))
            .unwrap_or(DEFAULT_LIFE_EXPECTANCY_AGE),
        target_replacement_ratio: input
            .target_replacement_ratio
            .or_else(|| row.map(|r| r.target_replacement_ratio))
            .unwrap_or(DEFAULT_TARGET_REPLACEMENT_RATIO),
        // Social Security. NO named default and NO fallback to zero: absent stays
        // absent, because "has not entered a Social Security figure" is a real
        // and common state that #469 must render differently from an entered $0.
        ss_monthly_benefit: merged_ss_benefit,
        ss_benefit_at_age: resolve_ss_anchor(
            input.ss_benefit_at_age,
            row.and_then(|r| r.ss_benefit_at_age_months.zip(r.ss_benefit_at_age_anchor.as_deref())),
            birth_date,
        ),
        ss_claiming_age: resolve_ss_anchor(
            input.ss_claiming_age,
            row.and_then(|r| r.ss_claiming_age_months.zip(r.ss_claiming_age_anchor.as_deref())),
            birth_date,
        ),
        // Derived, never supplied. Present exactly when a benefit is, which is
        // the migration's `ss_source_pairing_check`.
        // Derived from the SAME merged value bound above, not a second copy of
        // the merge expression. If the two ever drifted, the migration's
        // `ss_source_pairing_check` would fire as an unactionable 500 for what
        // is a pure Rust bug.
        ss_source: merged_ss_benefit.map(|_| SS_SOURCE_USER_ENTERED),
    })
}

/// Resolve one Social Security age anchor to `(months from birth, anchor kind)`.
///
/// Pure, and the one place the FRA-staleness rule lives. Three cases:
///
///  * the caller supplied an anchor this turn — use it;
///  * they did not, and the stored anchor is `'fra'` — RE-DERIVE the month count
///    from the MERGED birth date rather than carrying the stored number forward.
///    This is the whole reason the anchor column exists. A user born 1957 who
///    said "claim at my FRA" stored 798 months; if a later turn corrects their
///    birth date to 1960 their FRA becomes 804, and carrying 798 forward would
///    silently reinterpret "at FRA" as "six months early" and apply a 3.33%
///    reduction they never asked for;
///  * they did not, and the stored anchor is `'explicit'` — carry the stored
///    month count untouched. It is a number the user actually said, and a birth
///    date correction does not change what they said.
///
/// An unrecognised stored anchor string is treated as explicit AND LOGGED at
/// `error` level. The migration's CHECK makes it unreachable today; treating it
/// as explicit preserves the user's stored number rather than recomputing it
/// from a rule they may never have chosen.
///
/// State plainly what that costs, because it is not obvious: the resolved anchor
/// is WRITTEN BACK by `upsert_profile`, so the reinterpretation is PERSISTED —
/// any later write, even one touching only `inflation_rate`, permanently
/// rewrites the stored string to `'explicit'`. That is fine while `'fra'` and
/// `'explicit'` are the only values, and it is a data-destroying bug the day a
/// third anchor kind is added. The CHECK is designed to be widened, so this is a
/// live hazard, not a hypothetical one. The match arm below is written OUT
/// rather than left as a bare wildcard so that the residual is visible, and it
/// logs, because exhaustiveness checking cannot help with a `&str`.
fn resolve_ss_anchor(
    supplied: Option<SsAnchor>,
    stored: Option<(i32, &str)>,
    birth_date: NaiveDate,
) -> Option<(i32, &'static str)> {
    let fra_months = || {
        crate::social_security::full_retirement_age(crate::social_security::fra_birth_year(
            birth_date,
        ))
        .total_months()
    };
    match supplied {
        Some(SsAnchor::Fra) => Some((fra_months(), SS_ANCHOR_FRA)),
        // `checked_mul`, not `saturating_mul`: saturating turns an absurd input
        // like 300000000 into `i32::MAX`, and `validate_profile` then reports
        // "got 178956970 years 7 months" — a number the caller never typed. The
        // fallback keeps the range check meaningful by producing a value that is
        // out of range for the reason it actually is.
        Some(SsAnchor::Age(years)) => {
            Some((years.checked_mul(12).unwrap_or(i32::MAX), SS_ANCHOR_EXPLICIT))
        }
        None => match stored {
            Some((_, SS_ANCHOR_FRA)) => Some((fra_months(), SS_ANCHOR_FRA)),
            Some((months, SS_ANCHOR_EXPLICIT)) => Some((months, SS_ANCHOR_EXPLICIT)),
            Some((months, other)) => {
                tracing::error!(
                    anchor = %other,
                    months,
                    "unrecognised stored Social Security anchor; keeping the stored month \
                     count and rewriting the anchor as explicit",
                );
                Some((months, SS_ANCHOR_EXPLICIT))
            }
            None => None,
        },
    }
}

/// What a caller is told when they ask for a projection before setting up a
/// profile. Names the missing thing and the action, because "not found" alone
/// reads as a bug to someone who has no idea a profile is a prerequisite.
pub const NO_PROFILE_MESSAGE: &str =
    "Set up your retirement profile first — a projection needs your date of birth, \
     income and target retirement age.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_country_accepts_us() {
        assert!(retirement_supported_country("US"));
    }

    #[test]
    fn supported_country_is_case_insensitive() {
        assert!(retirement_supported_country("us"));
        assert!(retirement_supported_country("Us"));
    }

    #[test]
    fn supported_country_rejects_unsupported() {
        for cc in ["CA", "GB", "XX", "", "USA"] {
            assert!(!retirement_supported_country(cc), "country {cc}");
        }
    }

    // Pins all six documented defaults so that changing one is a deliberate act
    // that also updates this test, not an accidental edit. #454 decision 3
    // requires the defaults be visible and labelled.
    #[test]
    fn default_constants_have_their_documented_values() {
        assert_eq!(DEFAULT_TARGET_RETIREMENT_AGE, 65);
        assert_eq!(DEFAULT_EXPECTED_REAL_RETURN, 5.0);
        assert_eq!(DEFAULT_INFLATION_RATE, 2.5);
        assert_eq!(DEFAULT_LIFE_EXPECTANCY_AGE, 90);
        assert_eq!(DEFAULT_TARGET_REPLACEMENT_RATIO, 0.75);
        assert_eq!(
            default_employer_match(),
            EmployerMatch { tiers: vec![], annual_dollar_cap: None }
        );
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid test date")
    }

    /// A serializable row for the wire-shape tests. `country` is a parameter
    /// because the non-US case is a DIFFERENT wire shape, not merely a different
    /// value.
    fn wire_row(country: &str) -> RetirementProfile {
        RetirementProfile {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            member_ordinal: 1,
            country: country.to_string(),
            // Mid-June, deliberately NOT 1 January: `fra_birth_year` moves a
            // 1 January birth to the previous year, and a fixture that silently
            // exercised that rule would make every FRA assertion below read as
            // wrong by two months.
            birth_date: d(1980, 6, 15),
            target_retirement_age: 65,
            current_gross_income: 100_000.0,
            contribution_rate_pre_tax: 6.0,
            contribution_rate_roth: 0.0,
            employer_match_formula: serde_json::json!({"tiers": [], "annual_dollar_cap": null}),
            expected_real_return: 5.0,
            inflation_rate: 2.5,
            life_expectancy_age: 90,
            target_replacement_ratio: 0.75,
            ss_monthly_benefit: Some(2_000.0),
            ss_benefit_at_age_months: Some(67 * 12),
            ss_benefit_at_age_anchor: Some(SS_ANCHOR_FRA.to_string()),
            ss_claiming_age_months: Some(70 * 12),
            ss_claiming_age_anchor: Some(SS_ANCHOR_EXPLICIT.to_string()),
            ss_source: Some(SS_SOURCE_USER_ENTERED.to_string()),
            created_at: DateTime::<Utc>::from_timestamp(0, 0).expect("valid timestamp"),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).expect("valid timestamp"),
        }
    }

    /// `adjustment_applied` means "Nels changed the number the user gave us",
    /// not "the number differs from the PIA".
    ///
    /// The distinction has a concrete case: a figure quoted at 62 and claimed at
    /// 62 sits 30% below the PIA, and yet nothing was applied to it — telling
    /// the user an adjustment was made would be false, and #466's compliance
    /// rule keys the "we adjusted this" disclosure off exactly this flag.
    #[test]
    fn adjustment_applied_is_false_when_the_claiming_age_equals_the_quoted_age() {
        let mut row = wire_row("US");
        row.ss_benefit_at_age_months = Some(62 * 12);
        row.ss_claiming_age_months = Some(62 * 12);
        let view = social_security_view(&row).expect("US profile has a view");
        assert!(!view.adjustment_applied, "same age in and out means nothing was applied");
        assert_eq!(view.adjusted_monthly_benefit, Some(2_000.0), "the entered figure, unchanged");

        row.ss_claiming_age_months = Some(63 * 12);
        let view = social_security_view(&row).expect("US profile has a view");
        assert!(view.adjustment_applied, "a different claiming age IS an adjustment");
    }

    /// A benefit with no claiming age yet yields NO adjusted figure — not the
    /// entered figure passed through, and not zero.
    #[test]
    fn a_missing_claiming_age_yields_no_adjusted_figure_rather_than_a_guess() {
        let mut row = wire_row("US");
        row.ss_claiming_age_months = None;
        row.ss_claiming_age_anchor = None;
        let view = social_security_view(&row).expect("US profile has a view");
        assert_eq!(view.adjusted_monthly_benefit, None);
        assert!(!view.adjustment_applied);
        assert!(view.benefit_entered, "the entered figure is still entered");
    }

    // --- Social Security: merge, staleness, validation, LLM boundary --------

    #[test]
    fn an_explicit_ss_age_resolves_to_whole_months_and_is_marked_explicit() {
        let input = RetirementProfileInput {
            ss_claiming_age: Some(SsAnchor::Age(67)),
            ..create_input()
        };
        let resolved = resolve_input(None, &input).expect("resolves");
        assert_eq!(resolved.ss_claiming_age, Some((804, SS_ANCHOR_EXPLICIT)));
    }

    #[test]
    fn an_fra_anchor_resolves_against_the_users_own_full_retirement_age() {
        // Born 1957 => FRA 66y6m => 798 months. The whole reason the anchor is
        // not a whole-year age: neither 66 nor 67 is right for this cohort.
        let input = RetirementProfileInput {
            birth_date: Some(d(1957, 3, 4)),
            ss_claiming_age: Some(SsAnchor::Fra),
            ..create_input()
        };
        let resolved = resolve_input(None, &input).expect("resolves");
        assert_eq!(resolved.ss_claiming_age, Some((798, SS_ANCHOR_FRA)));
    }

    /// THE STALENESS TEST. An `fra` anchor is re-derived from the MERGED birth
    /// date on every write, so correcting a birth date moves the anchored age
    /// with it.
    ///
    /// Without this, a 1957 user who said "claim at my FRA" (798 months) and
    /// later corrected their birth date to 1960 would keep the stored 798 while
    /// their FRA moved to 804 — so "at FRA" would silently become "six months
    /// early" and quietly apply a 3.33% reduction they never asked for, with
    /// `adjustment_applied` flipping to true for a user who requested no
    /// adjustment.
    #[test]
    fn an_fra_anchored_age_is_re_derived_when_the_birth_date_is_later_corrected() {
        let mut stored = stored_row();
        stored.birth_date = d(1957, 3, 4);
        stored.ss_claiming_age_months = Some(798);
        stored.ss_claiming_age_anchor = Some(SS_ANCHOR_FRA.to_string());

        // A later turn corrects ONLY the birth date.
        let input = RetirementProfileInput {
            birth_date: Some(d(1960, 3, 4)),
            ..Default::default()
        };
        let resolved = resolve_input(Some((&stored, default_employer_match())), &input)
            .expect("resolves");
        assert_eq!(
            resolved.ss_claiming_age,
            Some((804, SS_ANCHOR_FRA)),
            "an fra anchor must follow the corrected birth date"
        );
    }

    /// The mirror image: an EXPLICIT age is a number the user actually said, and
    /// a birth-date correction does not change what they said.
    #[test]
    fn an_explicitly_anchored_age_survives_a_birth_date_correction_unchanged() {
        let mut stored = stored_row();
        stored.birth_date = d(1957, 3, 4);
        stored.ss_claiming_age_months = Some(62 * 12);
        stored.ss_claiming_age_anchor = Some(SS_ANCHOR_EXPLICIT.to_string());

        let input = RetirementProfileInput {
            birth_date: Some(d(1960, 3, 4)),
            ..Default::default()
        };
        let resolved = resolve_input(Some((&stored, default_employer_match())), &input)
            .expect("resolves");
        assert_eq!(resolved.ss_claiming_age, Some((744, SS_ANCHOR_EXPLICIT)));
    }

    #[test]
    fn ss_source_is_derived_from_the_presence_of_a_benefit_and_never_supplied() {
        let none = resolve_input(None, &create_input()).expect("resolves");
        assert_eq!(none.ss_source, None, "no benefit means no provenance");

        let entered = resolve_input(
            None,
            &RetirementProfileInput {
                ss_monthly_benefit: Some(2_100.0),
                ss_benefit_at_age: Some(SsAnchor::Fra),
                ..create_input()
            },
        )
        .expect("resolves");
        assert_eq!(entered.ss_source, Some(SS_SOURCE_USER_ENTERED));
    }

    #[test]
    fn an_absent_ss_field_leaves_the_stored_value_alone() {
        let mut stored = stored_row();
        stored.ss_monthly_benefit = Some(1_950.0);
        stored.ss_benefit_at_age_months = Some(70 * 12);
        stored.ss_benefit_at_age_anchor = Some(SS_ANCHOR_EXPLICIT.to_string());

        // A partial update touching an unrelated field.
        let input = RetirementProfileInput { inflation_rate: Some(2.0), ..Default::default() };
        let resolved = resolve_input(Some((&stored, default_employer_match())), &input)
            .expect("resolves");
        assert_eq!(resolved.ss_monthly_benefit, Some(1_950.0));
        assert_eq!(resolved.ss_benefit_at_age, Some((840, SS_ANCHOR_EXPLICIT)));
    }

    #[test]
    fn a_profile_with_no_social_security_at_all_is_valid() {
        // The common case. It must never be treated as incomplete.
        assert!(validate_profile(&valid_profile(), today()).is_ok());
    }

    #[test]
    fn a_benefit_without_an_anchor_age_is_rejected() {
        let mut p = valid_profile();
        p.ss_monthly_benefit = Some(2_000.0);
        let err = validate_profile(&p, today()).expect_err("must reject");
        assert!(err.contains("quoted at"), "{err}");
    }

    #[test]
    fn an_ss_age_outside_62_to_70_is_rejected_rather_than_clamped() {
        for months in [61 * 12, 71 * 12, 0, 75 * 12] {
            let mut p = valid_profile();
            p.ss_claiming_age = Some((months, SS_ANCHOR_EXPLICIT));
            let err = validate_profile(&p, today())
                .expect_err("must reject out-of-range claiming age");
            assert!(err.contains("between 62 and 70"), "{err}");
        }
    }

    #[test]
    fn an_implausible_benefit_is_rejected_with_a_message_naming_the_scale() {
        let mut p = valid_profile();
        p.ss_benefit_at_age = Some((804, SS_ANCHOR_FRA));
        p.ss_monthly_benefit = Some(36_000.0); // an ANNUAL figure in a monthly field
        let err = validate_profile(&p, today()).expect_err("must reject");
        assert!(err.contains("MONTHLY"), "{err}");

        p.ss_monthly_benefit = Some(-1.0);
        assert!(validate_profile(&p, today()).is_err(), "negative benefit");
    }

    /// #465's froze-the-profile defect class, checked for by construction on the
    /// new fields.
    ///
    /// A profile saved with a claiming age of 62 while its owner was 55 must
    /// still validate when they are 63, 68 and 75. None of the Social Security
    /// rules compares an age against the clock, so no passage of time can turn a
    /// valid stored profile into an uneditable one.
    #[test]
    fn a_stored_ss_claiming_age_never_becomes_invalid_merely_because_time_passed() {
        let mut p = valid_profile();
        p.birth_date = d(1960, 5, 1);
        p.ss_monthly_benefit = Some(2_000.0);
        p.ss_benefit_at_age = Some((804, SS_ANCHOR_FRA));
        p.ss_claiming_age = Some((744, SS_ANCHOR_EXPLICIT)); // claimed at 62
        // The rule about the TARGET RETIREMENT AGE is the one #465 had to gate;
        // leave it alone so this test can only fail on an SS rule.
        p.target_retirement_age_is_being_set = false;

        for year in [2015, 2022, 2023, 2028, 2035] {
            assert!(
                validate_profile(&p, d(year, 7, 28)).is_ok(),
                "profile must stay editable in {year}"
            );
        }
    }

    /// The LLM boundary. Every one of these is routine model formatting, and
    /// under a `#[serde(untagged)]` derive `62.0` would be a hard serde error
    /// that discards the ENTIRE chat turn while `"62"` would be silently
    /// swallowed by the string variant and become the FRA anchor.
    #[test]
    fn ss_anchor_accepts_every_routine_formatting_variant_of_an_age_and_of_fra() {
        for (json, expected) in [
            (serde_json::json!(62), SsAnchor::Age(62)),
            (serde_json::json!(62.0), SsAnchor::Age(62)),
            (serde_json::json!("62"), SsAnchor::Age(62)),
            (serde_json::json!("62.0"), SsAnchor::Age(62)),
            (serde_json::json!(" 67 "), SsAnchor::Age(67)),
            (serde_json::json!("fra"), SsAnchor::Fra),
            (serde_json::json!("FRA"), SsAnchor::Fra),
            (serde_json::json!(" Fra "), SsAnchor::Fra),
            (serde_json::json!("full retirement age"), SsAnchor::Fra),
            (serde_json::json!("my full retirement age"), SsAnchor::Fra),
            (serde_json::json!("Full Retirement Age (67)"), SsAnchor::Fra),
        ] {
            let input: RetirementProfileInput =
                serde_json::from_value(serde_json::json!({ "ss_claiming_age": json }))
                    .unwrap_or_else(|e| panic!("{json} must deserialize, got {e}"));
            assert_eq!(input.ss_claiming_age, Some(expected), "{json}");
        }
    }

    /// An OUT-OF-RANGE age must parse and then be rejected by validation, NOT
    /// rejected by serde.
    ///
    /// A serde failure inside `AiActionParams` throws the whole chat turn away
    /// and tells the user nothing; a validation failure is a 400 that names the
    /// field and the range. The range check therefore deliberately does not live
    /// in the deserializer.
    #[test]
    fn an_out_of_range_ss_age_parses_and_is_rejected_by_validation_not_by_serde() {
        let input: RetirementProfileInput =
            serde_json::from_value(serde_json::json!({ "ss_claiming_age": 75 }))
                .expect("75 must PARSE — the range check belongs to validate_profile");
        assert_eq!(input.ss_claiming_age, Some(SsAnchor::Age(75)));
    }

    /// `SsAnchor` must round-trip its own wire form: what `Serialize` emits,
    /// `de_lenient_ss_anchor` must accept. A derived `Serialize` emits `"Fra"`
    /// and `{"Age":67}` and fails this.
    #[test]
    fn ss_anchor_round_trips_through_its_own_serialize() {
        for anchor in [SsAnchor::Fra, SsAnchor::Age(62), SsAnchor::Age(70)] {
            let input = RetirementProfileInput { ss_claiming_age: Some(anchor), ..Default::default() };
            let wire = serde_json::to_value(&input).expect("serializes");
            let back: RetirementProfileInput =
                serde_json::from_value(wire.clone()).unwrap_or_else(|e| {
                    panic!("{anchor:?} serialized to {wire} which cannot be read back: {e}")
                });
            assert_eq!(back.ss_claiming_age, Some(anchor));
        }
    }

    #[test]
    fn ss_anchor_rejects_a_fractional_age_and_meaningless_text() {
        for bad in [serde_json::json!(64.5), serde_json::json!("sixty-two"), serde_json::json!("")] {
            let out: Result<RetirementProfileInput, _> =
                serde_json::from_value(serde_json::json!({ "ss_claiming_age": bad }));
            assert!(out.is_err(), "{bad} must not parse");
        }
    }

    #[test]
    fn ss_monthly_benefit_accepts_the_lenient_number_variants() {
        for json in [serde_json::json!(2000), serde_json::json!(2000.5), serde_json::json!("2000.5")] {
            let input: RetirementProfileInput =
                serde_json::from_value(serde_json::json!({ "ss_monthly_benefit": json }))
                    .unwrap_or_else(|e| panic!("{json} must deserialize, got {e}"));
            assert!(input.ss_monthly_benefit.is_some(), "{json}");
        }
    }

    /// The "no inflation adjustment" acceptance criterion, asserted at the layer
    /// where the mistake is actually REPRESENTABLE.
    ///
    /// `claiming_adjustment` has no inflation parameter, so no test of it can
    /// falsify much. `social_security_view` DOES have `profile.inflation_rate`
    /// in scope, so this is the one place someone could plausibly "helpfully"
    /// multiply the figure through to retirement. Two profiles differing ONLY in
    /// inflation must produce the identical adjusted benefit.
    ///
    /// Why no adjustment is correct: everything else in the projection runs in
    /// today's dollars at a REAL return, and Social Security carries a STATUTORY
    /// COLA — so an SSA-statement figure is ALREADY in today's dollars.
    /// Inflating it double-counts the COLA and overstates readiness; deflating
    /// it understates. #467 must carry this number through untouched.
    #[test]
    fn the_adjusted_benefit_does_not_move_with_the_inflation_assumption() {
        let mut low = wire_row("US");
        low.inflation_rate = 2.5;
        let mut high = wire_row("US");
        high.inflation_rate = 9.0;

        let low_view = social_security_view(&low).expect("view");
        let high_view = social_security_view(&high).expect("view");
        assert_eq!(
            low_view.adjusted_monthly_benefit, high_view.adjusted_monthly_benefit,
            "the Social Security figure is already in today's dollars — no COLA, no inflation"
        );
        assert_eq!(low_view.adjusted_monthly_benefit, Some(2_480.0));

        // The same for the real-return assumption, the other rate that could
        // plausibly be applied here by mistake.
        let mut fast = wire_row("US");
        fast.expected_real_return = 12.0;
        assert_eq!(
            social_security_view(&fast).expect("view").adjusted_monthly_benefit,
            Some(2_480.0)
        );
    }

    /// A stored row our own math refuses is reported as UNAVAILABLE, not as a
    /// plausible absence — and `adjustment_applied` cannot be true alongside a
    /// missing figure.
    ///
    /// Only reachable from a row that bypassed both `validate_profile` and the
    /// migration's CHECKs. That is exactly why it is surfaced: the alternative
    /// swallowed the error with `.ok()` and produced a response promising the UI
    /// both figures while carrying one.
    #[test]
    fn a_row_that_fails_renormalization_is_flagged_rather_than_reported_as_absent() {
        let mut broken = wire_row("US");
        broken.ss_claiming_age_months = Some(71 * 12); // outside 62..=70

        let view = social_security_view(&broken).expect("US profile still has a view");
        assert_eq!(view.adjusted_monthly_benefit, None);
        assert!(view.adjustment_unavailable, "the failure must be visible on the response");
        assert!(
            !view.adjustment_applied,
            "an adjustment cannot be 'applied' when there is no adjusted figure to show"
        );
        assert!(view.benefit_entered, "the entered figure is still entered");

        // And the healthy case does NOT set the flag.
        let ok = social_security_view(&wire_row("US")).expect("view");
        assert!(!ok.adjustment_unavailable);
    }

    /// Both AGES but no benefit is a legal half-entered state, and it must NOT
    /// report that an adjustment was applied.
    ///
    /// The migration permits it explicitly — "an age with no figure yet is a
    /// user part-way through" — so this is an ordinary state, not a corrupt one.
    /// Deriving the flag from the two ages alone reported `adjustment_applied:
    /// true` with no figures at all, telling #469 to show BOTH of two numbers
    /// that do not exist.
    #[test]
    fn ages_without_a_benefit_do_not_report_an_adjustment_as_applied() {
        let mut row = wire_row("US");
        row.ss_monthly_benefit = None;
        row.ss_source = None;
        // Both ages still present, and different from each other.
        assert_eq!(row.ss_benefit_at_age_months, Some(804));
        assert_eq!(row.ss_claiming_age_months, Some(840));

        let view = social_security_view(&row).expect("view");
        assert!(!view.benefit_entered);
        assert_eq!(view.adjusted_monthly_benefit, None);
        assert!(!view.adjustment_unavailable, "nothing failed — there is simply no figure yet");
        assert!(
            !view.adjustment_applied,
            "Nels cannot have changed a number it was never given"
        );
    }

    /// The 62..=70 bound is spelled in TWO places — these constants and the
    /// migration's `744`/`840` literals — so pin the Rust half.
    ///
    /// Widening the constant without widening the CHECK turns what should be an
    /// actionable 400 into an opaque 500 from a constraint violation.
    #[test]
    fn the_claim_age_bounds_match_the_migration_literals() {
        assert_eq!(
            crate::social_security::MIN_CLAIM_AGE_MONTHS,
            744,
            "must match retirement_profiles_ss_*_age_check in 20260728220000_retirement_social_security.sql"
        );
        assert_eq!(crate::social_security::MAX_CLAIM_AGE_MONTHS, 840, "same migration");
    }

    /// AGENTS.md section 22 states that an explicit JSON `null` means "leave it
    /// alone", exactly as an absent key does, and that there is therefore NO way
    /// to clear a stored Social Security figure in v1.
    ///
    /// That is a documented limitation rather than an accident, so pin the
    /// behaviour: if someone later gives these fields clears-on-null semantics,
    /// this test is what tells them the documentation needs to change with it.
    #[test]
    fn an_explicit_null_leaves_a_stored_ss_value_alone_exactly_as_an_absent_key_does() {
        let mut stored = stored_row();
        stored.ss_monthly_benefit = Some(1_950.0);
        stored.ss_benefit_at_age_months = Some(840);
        stored.ss_benefit_at_age_anchor = Some(SS_ANCHOR_EXPLICIT.to_string());

        let input: RetirementProfileInput = serde_json::from_value(serde_json::json!({
            "ss_monthly_benefit": null,
            "ss_claiming_age": null,
        }))
        .expect("explicit nulls parse");

        let resolved = resolve_input(Some((&stored, default_employer_match())), &input)
            .expect("resolves");
        assert_eq!(resolved.ss_monthly_benefit, Some(1_950.0), "null does NOT clear");
        assert_eq!(resolved.ss_source, Some(SS_SOURCE_USER_ENTERED));
    }

    /// An anchor age with NO benefit is a legitimate half-entered state — a user
    /// part-way through — and must validate.
    ///
    /// The reverse is not: a dollar figure with no age cannot be renormalized.
    #[test]
    fn a_claiming_age_with_no_benefit_yet_is_valid() {
        let mut p = valid_profile();
        p.ss_claiming_age = Some((840, SS_ANCHOR_EXPLICIT));
        assert!(validate_profile(&p, today()).is_ok());
    }

    #[test]
    fn current_age_counts_a_birthday_already_passed_this_year() {
        assert_eq!(current_age(d(1980, 3, 10), d(2026, 7, 28)), 46);
    }

    #[test]
    fn current_age_does_not_count_a_birthday_not_yet_reached() {
        assert_eq!(current_age(d(1980, 12, 10), d(2026, 7, 28)), 45);
    }

    #[test]
    fn current_age_counts_a_birthday_that_is_exactly_today() {
        assert_eq!(current_age(d(1980, 7, 28), d(2026, 7, 28)), 46);
    }

    #[test]
    fn current_age_handles_a_leap_day_birth_date_in_a_non_leap_year() {
        // Born 29 Feb 2000. In 2026 (not a leap year) the 29th never occurs, so
        // on 28 Feb they are still 25 and from 1 Mar they are 26.
        assert_eq!(current_age(d(2000, 2, 29), d(2026, 2, 28)), 25);
        assert_eq!(current_age(d(2000, 2, 29), d(2026, 3, 1)), 26);
    }

    /// The worked example from the spec: "100% of the first 3%, then 50% of the
    /// next 2%", uncapped.
    fn worked_example_tiers() -> EmployerMatch {
        EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 5.0, match_pct: 50.0 },
            ],
            annual_dollar_cap: None,
        }
    }

    #[test]
    fn employer_match_single_tier() {
        // 100% of the first 4%; contributing 4% of $80,000 => $3,200.
        let m = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 4.0, match_pct: 100.0 }],
            annual_dollar_cap: None,
        };
        assert_eq!(employer_match(80_000.0, 4.0, &m), 3_200.0);
    }

    #[test]
    fn employer_match_multi_tier() {
        // 100,000 x 3% x 100% = 3000, plus 100,000 x 2% x 50% = 1000 => 4000.
        assert_eq!(employer_match(100_000.0, 5.0, &worked_example_tiers()), 4_000.0);
    }

    #[test]
    fn employer_match_contribution_below_first_tier() {
        // Only 2 of the first tier's 3 points are covered: 100,000 x 2% x 100%.
        assert_eq!(employer_match(100_000.0, 2.0, &worked_example_tiers()), 2_000.0);
    }

    #[test]
    fn employer_match_contribution_above_all_tiers() {
        // Everything above the top tier's 5% is unmatched, so this is the same
        // 4000 as contributing exactly 5%.
        assert_eq!(employer_match(100_000.0, 10.0, &worked_example_tiers()), 4_000.0);
    }

    #[test]
    fn employer_match_dollar_cap_binds() {
        let mut m = worked_example_tiers();
        m.annual_dollar_cap = Some(2_500.0);
        assert_eq!(employer_match(100_000.0, 5.0, &m), 2_500.0);
    }

    #[test]
    fn employer_match_dollar_cap_that_does_not_bind_is_ignored() {
        // The mirror of the binding case, and the one that pins `.min(cap)` as a
        // MINIMUM rather than an assignment: a cap of $10,000 sits above the
        // $4,000 the tiers earn, so the match must still be $4,000. Without this
        // an "always return the cap" implementation passes the whole suite.
        let mut m = worked_example_tiers();
        m.annual_dollar_cap = Some(10_000.0);
        assert_eq!(employer_match(100_000.0, 5.0, &m), 4_000.0);
    }

    #[test]
    fn employer_match_zero_contribution() {
        assert_eq!(employer_match(100_000.0, 0.0, &worked_example_tiers()), 0.0);
    }

    #[test]
    fn employer_match_zero_income() {
        assert_eq!(employer_match(0.0, 5.0, &worked_example_tiers()), 0.0);
    }

    #[test]
    fn employer_match_empty_tiers_is_zero() {
        assert_eq!(employer_match(100_000.0, 10.0, &default_employer_match()), 0.0);
    }

    #[test]
    fn employer_match_clamps_negative_inputs() {
        let m = worked_example_tiers();
        assert_eq!(employer_match(-100_000.0, 5.0, &m), 0.0, "negative gross");
        assert_eq!(employer_match(100_000.0, -5.0, &m), 0.0, "negative employee pct");

        let negative_match_pct = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: -100.0 }],
            annual_dollar_cap: None,
        };
        assert_eq!(employer_match(100_000.0, 5.0, &negative_match_pct), 0.0, "negative match pct");

        let mut negative_cap = worked_example_tiers();
        negative_cap.annual_dollar_cap = Some(-500.0);
        assert_eq!(employer_match(100_000.0, 5.0, &negative_cap), 0.0, "negative cap");
    }

    #[test]
    fn employer_match_non_monotonic_tiers_do_not_panic() {
        // A tier whose ceiling is BELOW the previous tier's covers no band at
        // all; it must contribute 0 rather than a negative amount or a panic.
        let m = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 5.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
            ],
            annual_dollar_cap: None,
        };
        assert_eq!(employer_match(100_000.0, 10.0, &m), 5_000.0);
    }

    /// The fixed "today" every validation test is evaluated against.
    fn today() -> NaiveDate {
        d(2026, 7, 28)
    }

    /// A profile that passes every rule; each validation test mutates exactly
    /// one field so a failure can only be the rule under test.
    fn valid_profile() -> ResolvedProfile {
        ResolvedProfile {
            country: "US".to_string(),
            birth_date: d(1980, 1, 1), // 46 on TODAY
            target_retirement_age: 65,
            target_retirement_age_was_defaulted: false,
            target_retirement_age_is_being_set: true,
            current_gross_income: 100_000.0,
            contribution_rate_pre_tax: 6.0,
            contribution_rate_roth: 3.0,
            employer_match_formula: default_employer_match(),
            expected_real_return: DEFAULT_EXPECTED_REAL_RETURN,
            inflation_rate: DEFAULT_INFLATION_RATE,
            life_expectancy_age: DEFAULT_LIFE_EXPECTANCY_AGE,
            target_replacement_ratio: DEFAULT_TARGET_REPLACEMENT_RATIO,
            // No Social Security entered: the common case, and the one that must
            // stay valid. Tests that exercise the SS rules set these explicitly.
            ss_monthly_benefit: None,
            ss_benefit_at_age: None,
            ss_claiming_age: None,
            ss_source: None,
        }
    }

    #[test]
    fn validate_accepts_a_valid_profile() {
        assert_eq!(validate_profile(&valid_profile(), today()), Ok(()));
    }

    #[test]
    fn validate_rejects_retirement_age_at_or_below_current_age() {
        let mut below = valid_profile();
        below.target_retirement_age = 40; // current age is 46
        let err = validate_profile(&below, today()).unwrap_err();
        assert!(err.contains("greater than your current age"), "message was: {err}");

        let mut equal = valid_profile();
        equal.target_retirement_age = 46;
        let err = validate_profile(&equal, today()).unwrap_err();
        assert!(err.contains("greater than your current age"), "message was: {err}");
    }

    #[test]
    fn defaulted_past_age_warns_instead_of_rejecting() {
        // nels#533. A user aged 68 who never sent target_retirement_age: the
        // default 65 was substituted for them and it is already behind them.
        // `validate_profile` USED to reject this as a hard 400, throwing away
        // the whole gathered create; it now SAVES, and the save carries a
        // warning naming BOTH the substituted default and their current age so
        // the placeholder is never silently stored as their plan.
        let mut p = valid_profile();
        p.birth_date = d(1958, 1, 1); // 68 on TODAY
        p.target_retirement_age = DEFAULT_TARGET_RETIREMENT_AGE;
        p.target_retirement_age_was_defaulted = true;
        assert_eq!(validate_profile(&p, today()), Ok(()), "the create must SAVE, not reject");

        let warn = stale_defaulted_age_warning(&p, today())
            .expect("the substituted default behind the user's age must warn");
        assert!(warn.contains("65"), "warning must name the default: {warn}");
        assert!(warn.contains("68"), "warning must name the current age: {warn}");
    }

    #[test]
    fn explicit_past_age_is_a_rejection_and_no_warning() {
        // The other half of the branch: the caller EXPLICITLY set 65 at age 68.
        // That stays a hard 400 — they asked for it — and produces no warning,
        // because a warning would blur "we substituted a placeholder" with
        // "you asked for the impossible".
        let mut p = valid_profile();
        p.birth_date = d(1958, 1, 1); // 68 on TODAY
        p.target_retirement_age = 65;
        p.target_retirement_age_is_being_set = true;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("greater than your current age"), "message was: {err}");
        assert_eq!(
            stale_defaulted_age_warning(&p, today()),
            None,
            "an explicit (non-defaulted) age never warns"
        );
    }

    #[test]
    fn defaulted_future_age_neither_rejects_nor_warns() {
        // A 46-year-old who omitted the age gets the default 65, which is in
        // the future: no warning is needed, the placeholder IS the plan.
        let p = valid_profile(); // born 1980, 46 on TODAY
        assert_eq!(validate_profile(&p, today()), Ok(()));
        assert_eq!(
            stale_defaulted_age_warning(&p, today()),
            None,
            "a future default is not a warning condition"
        );
    }

    #[test]
    fn stale_stored_default_age_does_not_warn_on_an_unrelated_edit() {
        // The C1 rule, applied to warnings as well as rejections: an update
        // that merely carries a historical 65-target forward must not re-judge
        // it — not even by warning — or a profile created legally at 64 would
        // start nagging its owner from their 65th birthday onward on every
        // unrelated edit.
        let mut p = valid_profile();
        p.birth_date = d(1958, 1, 1); // 68 on TODAY
        p.target_retirement_age = 65;
        p.target_retirement_age_was_defaulted = true; // it WAS a default, once
        p.target_retirement_age_is_being_set = false; // but this write isn't touching it
        assert_eq!(
            stale_defaulted_age_warning(&p, today()),
            None,
            "an unrelated edit must not re-judge the stored age, even as a warning"
        );
    }

    /// The C1 regression, at the pure level.
    ///
    /// A stored `target_retirement_age` the caller is NOT touching must never
    /// block an unrelated edit, however long ago it was set. Before the fix, a
    /// profile created legally at 64 with a target of 65 became permanently
    /// uneditable on that user's 65th birthday — a partial update touching only
    /// `inflation_rate` came back
    /// `400 "target retirement age must be greater than your current age"`, via
    /// REST and via chat alike, because validation runs on the MERGED profile.
    #[test]
    fn validate_ignores_a_stale_stored_retirement_age_when_the_write_is_not_setting_it() {
        let mut p = valid_profile();
        p.birth_date = d(1958, 1, 1); // 68 on TODAY
        p.target_retirement_age = 65; // set years ago, when it WAS in the future
        p.target_retirement_age_is_being_set = false;
        assert_eq!(
            validate_profile(&p, today()),
            Ok(()),
            "a historical stored retirement age must not block an unrelated edit"
        );
    }

    /// The other half of C1: the rule still bites when the caller is actually
    /// setting the field, so relaxing the merged-profile case did not weaken it.
    #[test]
    fn validate_still_rejects_explicitly_setting_a_retirement_age_in_the_past() {
        let mut p = valid_profile();
        p.birth_date = d(1958, 1, 1); // 68 on TODAY
        p.target_retirement_age = 65;
        p.target_retirement_age_is_being_set = true;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("greater than your current age"), "message was: {err}");

        // Equal to the current age is rejected too.
        let mut equal = valid_profile();
        equal.birth_date = d(1958, 1, 1);
        equal.target_retirement_age = 68;
        equal.target_retirement_age_is_being_set = true;
        let err = validate_profile(&equal, today()).unwrap_err();
        assert!(err.contains("greater than your current age"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_life_expectancy_at_or_below_retirement_age() {
        let mut below = valid_profile();
        below.life_expectancy_age = 64; // retirement age is 65
        let err = validate_profile(&below, today()).unwrap_err();
        assert!(err.contains("life expectancy"), "message was: {err}");

        let mut equal = valid_profile();
        equal.life_expectancy_age = 65;
        let err = validate_profile(&equal, today()).unwrap_err();
        assert!(err.contains("life expectancy"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_contribution_rate_pre_tax() {
        let mut p = valid_profile();
        p.contribution_rate_pre_tax = -0.1;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("contribution_rate_pre_tax"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_contribution_rate_roth() {
        let mut p = valid_profile();
        p.contribution_rate_roth = -0.1;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("contribution_rate_roth"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_expected_real_return() {
        let mut p = valid_profile();
        p.expected_real_return = -1.0;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("expected_real_return"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_inflation_rate() {
        let mut p = valid_profile();
        p.inflation_rate = -1.0;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("inflation_rate"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_target_replacement_ratio() {
        let mut p = valid_profile();
        p.target_replacement_ratio = -0.75;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("target_replacement_ratio"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_negative_income() {
        let mut p = valid_profile();
        p.current_gross_income = -1.0;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("current gross income"), "message was: {err}");
    }

    // --- plausibility guards ------------------------------------------------
    //
    // One test per bound, asserting BOTH sides of the boundary: the constant
    // itself is accepted, anything above it is rejected with a message that
    // names the scale. These are guards against scale confusion, not forecasts —
    // see the constants' comment block.

    #[test]
    fn validate_rejects_a_replacement_ratio_above_the_plausibility_bound() {
        // The motivating case: "75%" typed into the FRACTION-scaled field stores
        // a number 100x off and passes every other rule.
        let mut percent_by_mistake = valid_profile();
        percent_by_mistake.target_replacement_ratio = 75.0;
        let err = validate_profile(&percent_by_mistake, today()).unwrap_err();
        assert!(err.contains("target_replacement_ratio"), "message was: {err}");
        assert!(err.contains("FRACTION"), "message must name the scale: {err}");

        let mut just_above = valid_profile();
        just_above.target_replacement_ratio = MAX_PLAUSIBLE_TARGET_REPLACEMENT_RATIO + 0.01;
        assert!(validate_profile(&just_above, today()).is_err());

        let mut at_bound = valid_profile();
        at_bound.target_replacement_ratio = MAX_PLAUSIBLE_TARGET_REPLACEMENT_RATIO;
        assert_eq!(validate_profile(&at_bound, today()), Ok(()), "the bound itself is accepted");
    }

    #[test]
    fn validate_rejects_an_expected_real_return_above_the_plausibility_bound() {
        let mut above = valid_profile();
        above.expected_real_return = MAX_PLAUSIBLE_EXPECTED_REAL_RETURN + 0.1;
        let err = validate_profile(&above, today()).unwrap_err();
        assert!(err.contains("expected_real_return"), "message was: {err}");
        assert!(err.contains("PERCENT"), "message must name the scale: {err}");

        let mut at_bound = valid_profile();
        at_bound.expected_real_return = MAX_PLAUSIBLE_EXPECTED_REAL_RETURN;
        assert_eq!(validate_profile(&at_bound, today()), Ok(()), "the bound itself is accepted");
    }

    #[test]
    fn validate_rejects_an_inflation_rate_above_the_plausibility_bound() {
        let mut above = valid_profile();
        above.inflation_rate = MAX_PLAUSIBLE_INFLATION_RATE + 0.1;
        let err = validate_profile(&above, today()).unwrap_err();
        assert!(err.contains("inflation_rate"), "message was: {err}");
        assert!(err.contains("PERCENT"), "message must name the scale: {err}");

        let mut at_bound = valid_profile();
        at_bound.inflation_rate = MAX_PLAUSIBLE_INFLATION_RATE;
        assert_eq!(validate_profile(&at_bound, today()), Ok(()), "the bound itself is accepted");
    }

    #[test]
    fn validate_rejects_a_life_expectancy_above_the_plausibility_bound() {
        let mut above = valid_profile();
        above.life_expectancy_age = MAX_PLAUSIBLE_LIFE_EXPECTANCY_AGE + 1;
        let err = validate_profile(&above, today()).unwrap_err();
        assert!(err.contains("life expectancy"), "message was: {err}");
        assert!(err.contains("whole years"), "message must name the scale: {err}");

        let mut at_bound = valid_profile();
        at_bound.life_expectancy_age = MAX_PLAUSIBLE_LIFE_EXPECTANCY_AGE;
        assert_eq!(validate_profile(&at_bound, today()), Ok(()), "the bound itself is accepted");
    }

    #[test]
    fn validate_rejects_an_income_above_the_plausibility_bound() {
        let mut above = valid_profile();
        above.current_gross_income = MAX_PLAUSIBLE_CURRENT_GROSS_INCOME + 1.0;
        let err = validate_profile(&above, today()).unwrap_err();
        assert!(err.contains("current gross income"), "message was: {err}");
        assert!(err.contains("ANNUAL"), "message must name the scale: {err}");

        let mut at_bound = valid_profile();
        at_bound.current_gross_income = MAX_PLAUSIBLE_CURRENT_GROSS_INCOME;
        assert_eq!(validate_profile(&at_bound, today()), Ok(()), "the bound itself is accepted");
    }

    #[test]
    fn validate_rejects_malformed_country() {
        for cc in ["U", "USA", "u1", ""] {
            let mut p = valid_profile();
            p.country = cc.to_string();
            let result = validate_profile(&p, today());
            assert!(result.is_err(), "country {cc:?} should be rejected");
            let err = result.unwrap_err();
            assert!(err.contains("two-letter"), "message was: {err}");
        }
    }

    #[test]
    fn validate_rejects_contribution_rates_summing_above_100() {
        let mut p = valid_profile();
        p.contribution_rate_pre_tax = 60.0;
        p.contribution_rate_roth = 40.1;
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("more than 100%"), "message was: {err}");
    }

    #[test]
    fn validate_allows_contribution_rates_summing_to_exactly_100() {
        let mut p = valid_profile();
        p.contribution_rate_pre_tax = 60.0;
        p.contribution_rate_roth = 40.0;
        assert_eq!(validate_profile(&p, today()), Ok(()));
    }

    #[test]
    fn validate_rejects_a_future_birth_date() {
        // `current_age` returns a NEGATIVE number for a future birth date, so the
        // retirement-age rule passes trivially and a nonsense profile would save
        // cleanly and then feed garbage into #467's projection math.
        let mut p = valid_profile();
        p.birth_date = d(2030, 1, 1);
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("birth date"), "message was: {err}");
        assert!(err.contains("future"), "message was: {err}");
    }

    #[test]
    fn validate_allows_a_birth_date_of_today() {
        // The boundary is inclusive: born today is not a date in the future. A
        // newborn is a nonsense profile, but it is not the DATE that is wrong,
        // and the retirement-age rule owns anything that follows from the age.
        let mut p = valid_profile();
        p.birth_date = today();
        assert_eq!(validate_profile(&p, today()), Ok(()));
    }

    #[test]
    fn validate_rejects_a_negative_employee_pct_up_to() {
        let mut p = valid_profile();
        p.employer_match_formula = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: -1.0, match_pct: 100.0 }],
            annual_dollar_cap: None,
        };
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("employee_pct_up_to"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_a_negative_match_pct() {
        let mut p = valid_profile();
        p.employer_match_formula = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: -100.0 }],
            annual_dollar_cap: None,
        };
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("match_pct"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_a_non_monotonic_tier_list() {
        // This is the shape that makes `employer_match` OVER-state the match:
        // three tiers whose ceilings go 5 -> 3 -> 7 charge 9 percentage points of
        // match against a top ceiling of only 7. Rejecting it here is what makes
        // that case unrepresentable rather than merely untested.
        let mut p = valid_profile();
        p.employer_match_formula = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 5.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 7.0, match_pct: 100.0 },
            ],
            annual_dollar_cap: None,
        };
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("increasing"), "message was: {err}");

        // Equal consecutive ceilings are also rejected: STRICTLY increasing.
        let mut equal = valid_profile();
        equal.employer_match_formula = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 3.0, match_pct: 50.0 },
            ],
            annual_dollar_cap: None,
        };
        let err = validate_profile(&equal, today()).unwrap_err();
        assert!(err.contains("increasing"), "message was: {err}");
    }

    #[test]
    fn validate_rejects_a_negative_annual_dollar_cap() {
        let mut p = valid_profile();
        p.employer_match_formula = EmployerMatch {
            tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 }],
            annual_dollar_cap: Some(-1.0),
        };
        let err = validate_profile(&p, today()).unwrap_err();
        assert!(err.contains("annual_dollar_cap"), "message was: {err}");
        assert!(err.contains("cannot be negative"), "message was: {err}");
    }

    #[test]
    fn validate_accepts_a_strictly_increasing_tier_list() {
        let mut p = valid_profile();
        p.employer_match_formula = worked_example_tiers();
        assert_eq!(validate_profile(&p, today()), Ok(()));

        let mut capped = valid_profile();
        capped.employer_match_formula =
            EmployerMatch { tiers: worked_example_tiers().tiers, annual_dollar_cap: Some(5_000.0) };
        assert_eq!(validate_profile(&capped, today()), Ok(()));
    }

    #[test]
    fn validate_accepts_an_empty_tier_list() {
        // The default is zero tiers, so an empty list must stay valid.
        let mut p = valid_profile();
        p.employer_match_formula = default_employer_match();
        assert_eq!(validate_profile(&p, today()), Ok(()));
    }

    // --- resolve_input --------------------------------------------------

    /// A stored row every update test merges onto. Its values are deliberately
    /// all distinct from the named defaults so a test that accidentally reads a
    /// default instead of the stored value fails loudly.
    fn stored_row() -> RetirementProfile {
        RetirementProfile {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            member_ordinal: 1,
            country: "US".to_string(),
            birth_date: d(1975, 6, 15),
            target_retirement_age: 62,
            current_gross_income: 90_000.0,
            contribution_rate_pre_tax: 8.0,
            contribution_rate_roth: 2.0,
            employer_match_formula: serde_json::to_value(worked_example_tiers()).unwrap(),
            expected_real_return: 4.0,
            inflation_rate: 3.0,
            life_expectancy_age: 95,
            target_replacement_ratio: 0.8,
            ss_monthly_benefit: None,
            ss_benefit_at_age_months: None,
            ss_benefit_at_age_anchor: None,
            ss_claiming_age_months: None,
            ss_claiming_age_anchor: None,
            ss_source: None,
            created_at: DateTime::<Utc>::from_timestamp(0, 0).expect("valid timestamp"),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).expect("valid timestamp"),
        }
    }

    /// The minimum payload that can create a profile.
    fn create_input() -> RetirementProfileInput {
        RetirementProfileInput {
            country: Some("US".to_string()),
            birth_date: Some(d(1980, 1, 1)),
            current_gross_income: Some(100_000.0),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_input_create_requires_country() {
        let input = RetirementProfileInput { country: None, ..create_input() };
        let err = resolve_input(None, &input).unwrap_err();
        assert!(err.contains("country"), "message was: {err}");
        assert!(err.contains("required"), "message was: {err}");
    }

    #[test]
    fn resolve_input_create_requires_birth_date() {
        let input = RetirementProfileInput { birth_date: None, ..create_input() };
        let err = resolve_input(None, &input).unwrap_err();
        assert!(err.contains("birth_date"), "message was: {err}");
        assert!(err.contains("required"), "message was: {err}");
    }

    #[test]
    fn resolve_input_create_requires_current_gross_income() {
        let input = RetirementProfileInput { current_gross_income: None, ..create_input() };
        let err = resolve_input(None, &input).unwrap_err();
        assert!(err.contains("current_gross_income"), "message was: {err}");
        assert!(err.contains("required"), "message was: {err}");
    }

    #[test]
    fn resolve_input_create_fills_every_default_from_the_named_constants() {
        let r = resolve_input(None, &create_input()).unwrap();
        assert_eq!(r.target_retirement_age, DEFAULT_TARGET_RETIREMENT_AGE);
        assert_eq!(r.expected_real_return, DEFAULT_EXPECTED_REAL_RETURN);
        assert_eq!(r.inflation_rate, DEFAULT_INFLATION_RATE);
        assert_eq!(r.life_expectancy_age, DEFAULT_LIFE_EXPECTANCY_AGE);
        assert_eq!(r.target_replacement_ratio, DEFAULT_TARGET_REPLACEMENT_RATIO);
        assert_eq!(r.employer_match_formula, default_employer_match());
        // Contribution rates default to zero: nothing is assumed about what the
        // user actually contributes.
        assert_eq!(r.contribution_rate_pre_tax, 0.0);
        assert_eq!(r.contribution_rate_roth, 0.0);
    }

    #[test]
    fn resolve_input_update_preserves_untouched_fields() {
        let row = stored_row();
        let existing_match = worked_example_tiers();
        // An empty payload against an existing row changes nothing at all.
        let r = resolve_input(
            Some((&row, existing_match.clone())),
            &RetirementProfileInput::default(),
        )
        .unwrap();
        assert_eq!(r.country, row.country);
        assert_eq!(r.birth_date, row.birth_date);
        assert_eq!(r.target_retirement_age, row.target_retirement_age);
        assert_eq!(r.current_gross_income, row.current_gross_income);
        assert_eq!(r.contribution_rate_pre_tax, row.contribution_rate_pre_tax);
        assert_eq!(r.contribution_rate_roth, row.contribution_rate_roth);
        assert_eq!(r.expected_real_return, row.expected_real_return);
        assert_eq!(r.inflation_rate, row.inflation_rate);
        assert_eq!(r.life_expectancy_age, row.life_expectancy_age);
        assert_eq!(r.target_replacement_ratio, row.target_replacement_ratio);
        assert_eq!(r.employer_match_formula, existing_match);
    }

    #[test]
    fn resolve_input_update_overrides_supplied_fields() {
        let row = stored_row();
        let input = RetirementProfileInput {
            target_retirement_age: Some(67),
            contribution_rate_pre_tax: Some(12.5),
            employer_match_formula: Some(default_employer_match()),
            ..Default::default()
        };
        let r = resolve_input(Some((&row, worked_example_tiers())), &input).unwrap();
        assert_eq!(r.target_retirement_age, 67);
        assert_eq!(r.contribution_rate_pre_tax, 12.5);
        assert_eq!(r.employer_match_formula, default_employer_match());
        // Everything else still comes from the row.
        assert_eq!(r.contribution_rate_roth, row.contribution_rate_roth);
        assert_eq!(r.current_gross_income, row.current_gross_income);
    }

    /// Pins the ONE field on `RetirementProfileInput` that is not COALESCE-style.
    ///
    /// `employer_match_formula` is absent-or-TOTAL: sending it at all replaces
    /// the whole stored formula, and any key omitted from the object sent falls
    /// to `EmployerMatch`'s `#[serde(default)]`. So a caller with stored tiers who
    /// sends `{"employer_match_formula": {"annual_dollar_cap": 5000}}` meaning
    /// "add a cap to what I have" loses every tier, with a `200` and no warning.
    ///
    /// This test exists to make that a DELIBERATE, discoverable behaviour rather
    /// than an accident nobody noticed. It deserializes the payload from real
    /// JSON rather than building the struct by hand, because the wipe is
    /// precisely a consequence of what serde does with the ABSENT `tiers` key —
    /// constructing `EmployerMatch` in Rust would skip the step under test. See
    /// the field's own doc comment for why replacement is kept.
    #[test]
    fn resolve_input_replaces_the_whole_employer_match_rather_than_merging_it() {
        let row = stored_row();
        let stored_tiers = worked_example_tiers();
        assert!(!stored_tiers.tiers.is_empty(), "the stored formula must have something to lose");

        // Exactly the REST body a user would send to "add a cap".
        let input: RetirementProfileInput =
            serde_json::from_str(r#"{"employer_match_formula": {"annual_dollar_cap": 5000}}"#)
                .expect("a cap-only formula is valid INPUT — the lenient type accepts it");

        let r = resolve_input(Some((&row, stored_tiers.clone())), &input).unwrap();
        assert_eq!(r.employer_match_formula.annual_dollar_cap, Some(5000.0));
        assert!(
            r.employer_match_formula.tiers.is_empty(),
            "documented: sending the object at all REPLACES it, so the stored tiers are wiped"
        );

        // The other half of the contract: OMITTING the key entirely leaves the
        // stored formula alone. That is the difference between "replace" and
        // "clobber unconditionally", and it is what makes every other partial
        // update safe.
        let untouched: RetirementProfileInput =
            serde_json::from_str(r#"{"inflation_rate": 3.1}"#).unwrap();
        let r2 = resolve_input(Some((&row, stored_tiers.clone())), &untouched).unwrap();
        assert_eq!(r2.employer_match_formula, stored_tiers);
    }

    /// The strict storage-side type must REJECT exactly what the lenient input
    /// type accepts, because "no `tiers` key" means two different things on the
    /// two sides: "I am not telling you about tiers" on input, and "this row's
    /// match has been lost" in storage.
    #[test]
    fn stored_employer_match_rejects_what_the_input_type_leniently_defaults() {
        // The value our own writer emits round-trips, cap or no cap.
        let ours = serde_json::to_value(worked_example_tiers()).unwrap();
        let decoded: StoredEmployerMatch = serde_json::from_value(ours).expect("our own output");
        assert_eq!(decoded.tiers.len(), worked_example_tiers().tiers.len());
        let no_cap = serde_json::to_value(default_employer_match()).unwrap();
        assert!(
            serde_json::from_value::<StoredEmployerMatch>(no_cap).is_ok(),
            "a null cap is an explicit key in our output, so the strict type takes it"
        );

        // Everything a missing `tiers` key could look like is an ERROR, never a
        // clean decode to "no employer match".
        for corrupt in [r#"{"annual_dollar_cap": 5000}"#, "{}", r#"{"tiers": 7}"#] {
            assert!(
                serde_json::from_str::<StoredEmployerMatch>(corrupt).is_err(),
                "stored value {corrupt} must not decode silently"
            );
            // …and each of these is exactly what the LENIENT type would have
            // swallowed, which is why the two types exist.
            if corrupt != r#"{"tiers": 7}"# {
                let lenient: EmployerMatch = serde_json::from_str(corrupt).unwrap();
                assert!(lenient.tiers.is_empty(), "the lenient type defaults to NO match: {corrupt}");
            }
        }

        // An unknown key is rejected too: our writer never emits one, so its
        // presence means something other than us wrote this row.
        assert!(serde_json::from_str::<StoredEmployerMatch>(
            r#"{"tiers": [], "annual_dollar_cap": null, "surprise": 1}"#
        )
        .is_err());
    }

    #[test]
    fn resolve_input_uppercases_and_trims_country() {
        // Load-bearing: the DB CHECK is `^[A-Z]{2}$` while `validate_profile`
        // deliberately accepts lowercase. Without this normalisation a lowercase
        // country would pass validation and then die at the constraint as a 500
        // instead of being accepted as the perfectly good input it is.
        let input = RetirementProfileInput { country: Some("  us ".to_string()), ..create_input() };
        assert_eq!(resolve_input(None, &input).unwrap().country, "US");
    }

    #[test]
    fn resolve_input_marks_the_age_defaulted_only_on_a_create_that_omitted_it() {
        // Create, age omitted: the default was substituted FOR the user, so the
        // validation message must be allowed to say so.
        assert!(resolve_input(None, &create_input()).unwrap().target_retirement_age_was_defaulted);

        // Create, age supplied: it is the user's own number.
        let supplied = RetirementProfileInput { target_retirement_age: Some(70), ..create_input() };
        assert!(!resolve_input(None, &supplied).unwrap().target_retirement_age_was_defaulted);

        // Update, age omitted: the stored value is the user's own number from an
        // earlier turn, NOT a fresh default, so it must not be blamed on us.
        let row = stored_row();
        let r = resolve_input(
            Some((&row, worked_example_tiers())),
            &RetirementProfileInput::default(),
        )
        .unwrap();
        assert!(!r.target_retirement_age_was_defaulted);
    }

    #[test]
    fn resolve_input_marks_the_age_as_being_set_on_creates_and_on_explicit_updates_only() {
        // Create, age omitted: the default is still a value THIS write chooses,
        // so the "must be in the future" rule must run.
        assert!(resolve_input(None, &create_input()).unwrap().target_retirement_age_is_being_set);

        // Create, age supplied: likewise.
        let supplied = RetirementProfileInput { target_retirement_age: Some(70), ..create_input() };
        assert!(resolve_input(None, &supplied).unwrap().target_retirement_age_is_being_set);

        let row = stored_row();

        // Update that SUPPLIES the age: the caller is setting it, so it is
        // checked against today.
        let updating = RetirementProfileInput { target_retirement_age: Some(70), ..Default::default() };
        assert!(resolve_input(Some((&row, worked_example_tiers())), &updating)
            .unwrap()
            .target_retirement_age_is_being_set);

        // Update that does NOT touch it: the stored value is historical fact and
        // must not be re-judged, or the profile becomes uneditable once the user
        // reaches that age.
        let untouched = RetirementProfileInput { inflation_rate: Some(3.1), ..Default::default() };
        assert!(!resolve_input(Some((&row, worked_example_tiers())), &untouched)
            .unwrap()
            .target_retirement_age_is_being_set);
    }
}
