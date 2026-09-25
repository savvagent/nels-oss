//! Retirement profile & assumptions (nels#465, part of the #454 epic).

use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::internal_error;

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
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

/// The stored row plus the DERIVED region gate, flattened into one JSON object
/// (the `goals::GoalDetail` precedent).
///
/// `supported` is carried on the response so consumers never re-derive the
/// US-only rule and drift from `retirement_supported_country`. A non-US profile
/// still SAVES and simply reports `supported: false`; the gate is a rendering
/// gate, not a write gate.
#[derive(Debug, Clone, Serialize)]
pub struct RetirementProfileResponse {
    #[serde(flatten)]
    pub profile: RetirementProfile,
    pub supported: bool,
    /// The DERIVED Social Security view (nels#466), or `None` for a profile
    /// outside `retirement_supported_country`.
    ///
    /// It carries only DERIVATIONS — full retirement age, the cohort's delayed
    /// credit, the adjusted figure, and whether an adjustment was applied. The
    /// values the USER entered are not repeated here: they are already on the
    /// flattened row as `ss_monthly_benefit`, `ss_benefit_at_age_months` and
    /// `ss_claiming_age_months`. Shipping each fact under exactly one name is
    /// deliberate — two spellings of the same value is how two consumers come to
    /// disagree about which one is authoritative.
    ///
    /// `None` for a non-US profile because every field in it is US STATUTE. A
    /// French user's profile still saves and still returns all of their own
    /// entered values; what it must not do is hand them a US Social Security
    /// full retirement age as though it were a fact about their pension. That is
    /// #454 decision 2's "no dial, no empty bar, no zeros" applied at the API
    /// rather than left to the UI.
    pub social_security: Option<SocialSecurityView>,
    /// nels#533: present only when the save succeeded with a substituted default
    /// target age that is behind the caller's current age — i.e. "we saved it,
    /// but the placeholder age is wrong; give us a real one". OMITTED from the
    /// JSON on every ordinary save, so existing consumers see no change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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

/// The user's retirement profile, or `None` when they have not set one up.
///
/// Always `member_ordinal = 1`: v1 has exactly one household member and no
/// surface can create a second, so the ordinal is pinned in the query rather
/// than parameterised.
pub async fn get_profile(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<RetirementProfile>, (StatusCode, String)> {
    sqlx::query_as::<_, RetirementProfile>(
        "SELECT * FROM retirement_profiles WHERE user_id = $1 AND member_ordinal = 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)
}

/// The ONE write path for a retirement profile. Both the REST `PUT` handler and
/// the chat `SET_RETIREMENT_PROFILE` arm call this, which is how "chat and REST
/// write the same rows" is satisfied structurally rather than by two parallel
/// pieces of SQL that can drift.
///
/// Returns `(profile, warning)`. The second element is the nels#533 stale-default
/// disclosure: a first-time create that omitted the target retirement age has
/// the 65 default substituted, and when that default is already in the user's
/// past the write still SUCCEEDS but returns a warning naming both figures
/// (REST surfaces it as `RetirementProfileResponse.warning`, chat appends it to
/// the success log). It is `None` for every other write — see
/// `stale_defaulted_age_warning`.
///
/// The whole read-merge-validate-write runs in ONE transaction, and the read
/// takes a `SELECT … FOR UPDATE` row lock:
/// 1. lock and read the existing row, then decode its `employer_match_formula`.
///    A decode failure is a **500**, never a 400 — corrupt JSONB in our own
///    table is our bug, and silently defaulting it to "no match" would
///    understate a real employer match with no signal at all;
/// 2. `resolve_input` overlays the payload, so its `Err` can only ever mean
///    "the caller's input is wrong" → 400;
/// 3. `validate_profile` checks the MERGED result, so a partial update cannot
///    half-write its way into an invalid state → 400, nothing written;
/// 4. a single `INSERT … ON CONFLICT DO UPDATE … RETURNING *`, then commit.
///
/// **Why the lock is load-bearing.** This function merges in Rust and then binds
/// a fully resolved literal for EVERY column; there is no SQL `COALESCE` here to
/// let the database resolve "not supplied → keep current" against the live row.
/// Without the lock, two concurrent partial writes — two browser tabs, or a REST
/// `PUT` racing the chat arm — would each read the same pre-image, each resolve
/// the other's untouched fields to the STALE value, and the second commit would
/// silently discard the first writer's change even though that writer already
/// received a `200` showing it. Holding the row lock from the read through the
/// write serializes them instead: the loser blocks, then re-reads the winner's
/// committed values and merges onto those.
///
/// Switching to SQL-level `COALESCE($n, column)` would also close the lost
/// update, but it is deliberately NOT what this function does, because the
/// cross-field invariants here are not per-column. `life_expectancy_age >
/// target_retirement_age` and `target_retirement_age > current_age` would then
/// be validated in Rust against a stale snapshot while the database assembled
/// the final row out of live values, producing a stored row that no writer ever
/// validated. The lock buys both properties at once: one consistent snapshot to
/// validate, and no lost update.
///
/// **Concurrent CREATEs need a second lock, and that is what the
/// `pg_advisory_xact_lock` is for.** `FOR UPDATE` locks nothing when it selects
/// nothing, so for a user with no row yet both callers would see `None` and both
/// would merge onto the DEFAULTS. `ON CONFLICT` does NOT rescue that, because
/// the eight defaultable fields are exactly where the data is lost: writer A
/// sends the three required fields plus `contribution_rate_pre_tax: 6.0` and
/// gets a `200` echoing `6.0`; writer B sends the three required fields plus
/// `target_retirement_age: 62`, resolves A's untouched contribution rate to its
/// own default of `0.0`, and its `DO UPDATE` overwrites A's value. Both callers
/// saw success and nothing was logged. The advisory lock is taken on the USER
/// key BEFORE the `SELECT`, so the no-row case serializes exactly like the
/// existing-row case: the loser blocks, then re-reads the winner's committed row
/// and merges onto it.
///
/// `today` is a parameter rather than `Utc::now()` inside, so the validation it
/// feeds stays deterministic for callers and tests.
pub async fn upsert_profile(
    pool: &PgPool,
    user_id: Uuid,
    input: &RetirementProfileInput,
    today: NaiveDate,
) -> Result<(RetirementProfile, Option<String>), (StatusCode, String)> {
    let mut tx = pool.begin().await.map_err(internal_error)?;

    // Serialize every writer for THIS user, including the ones whose `SELECT …
    // FOR UPDATE` below will match no row at all. A row lock cannot lock a row
    // that does not exist yet, so without this two concurrent CREATEs both read
    // `None`, both merge onto the DEFAULTS, and the second `DO UPDATE` silently
    // overwrites whatever the first writer supplied for any of the eight
    // defaultable fields — with both callers having received a 200. The lock is
    // transaction-scoped, so it releases on commit or rollback with no unlock
    // path to forget, and it is keyed on the user, so it never serializes
    // unrelated users against each other.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

    // Deliberately NOT `get_profile`: that helper is the unlocked read the `GET`
    // handler wants. This read must take the row lock that the doc comment above
    // depends on, and it must run on the same transaction as the write.
    let existing = sqlx::query_as::<_, RetirementProfile>(
        "SELECT * FROM retirement_profiles WHERE user_id = $1 AND member_ordinal = 1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal_error)?;

    // STRICT decode, deliberately NOT the lenient `EmployerMatch` the input side
    // uses. `StoredEmployerMatch` has no `#[serde(default)]` on either field, so
    // a stored `{"annual_dollar_cap": 5000}` with `tiers` missing is an ERROR
    // here rather than a clean decode to zero tiers — i.e. rather than a silent
    // "this user has no employer match", which would UNDERSTATE a real match and
    // feed #467's projection a fabricated fact. Corrupt JSONB in our own table is
    // our bug, so the honest answer is a loud 500, not a quiet zero. See
    // `StoredEmployerMatch`'s own doc comment.
    let existing_match: Option<EmployerMatch> = match existing.as_ref() {
        Some(row) => Some(
            serde_json::from_value::<StoredEmployerMatch>(row.employer_match_formula.clone())
                .map_err(|e| internal_error(format!("corrupt employer_match_formula: {e}")))?
                .into(),
        ),
        None => None,
    };

    let resolved = resolve_input(existing.as_ref().zip(existing_match), input)
        .map_err(|msg| (StatusCode::BAD_REQUEST, msg))?;
    validate_profile(&resolved, today).map_err(|msg| (StatusCode::BAD_REQUEST, msg))?;

    // nels#533: a substituted default target age that is already behind the user
    // is no longer a rejection (see `validate_profile`); the write proceeds and
    // the warning is surfaced on the response / in the chat log. `None` on every
    // ordinary save.
    let warning = stale_defaulted_age_warning(&resolved, today);

    let formula = serde_json::to_value(&resolved.employer_match_formula).map_err(internal_error)?;

    let written = sqlx::query_as::<_, RetirementProfile>(
        "INSERT INTO retirement_profiles (\
             id, user_id, member_ordinal, country, birth_date, target_retirement_age, \
             current_gross_income, contribution_rate_pre_tax, contribution_rate_roth, \
             employer_match_formula, expected_real_return, inflation_rate, \
             life_expectancy_age, target_replacement_ratio, \
             ss_monthly_benefit, ss_benefit_at_age_months, ss_benefit_at_age_anchor, \
             ss_claiming_age_months, ss_claiming_age_anchor, ss_source\
         ) VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
                   $14, $15, $16, $17, $18, $19) \
         ON CONFLICT (user_id, member_ordinal) DO UPDATE SET \
             country = EXCLUDED.country, \
             birth_date = EXCLUDED.birth_date, \
             target_retirement_age = EXCLUDED.target_retirement_age, \
             current_gross_income = EXCLUDED.current_gross_income, \
             contribution_rate_pre_tax = EXCLUDED.contribution_rate_pre_tax, \
             contribution_rate_roth = EXCLUDED.contribution_rate_roth, \
             employer_match_formula = EXCLUDED.employer_match_formula, \
             expected_real_return = EXCLUDED.expected_real_return, \
             inflation_rate = EXCLUDED.inflation_rate, \
             life_expectancy_age = EXCLUDED.life_expectancy_age, \
             target_replacement_ratio = EXCLUDED.target_replacement_ratio, \
             ss_monthly_benefit = EXCLUDED.ss_monthly_benefit, \
             ss_benefit_at_age_months = EXCLUDED.ss_benefit_at_age_months, \
             ss_benefit_at_age_anchor = EXCLUDED.ss_benefit_at_age_anchor, \
             ss_claiming_age_months = EXCLUDED.ss_claiming_age_months, \
             ss_claiming_age_anchor = EXCLUDED.ss_claiming_age_anchor, \
             ss_source = EXCLUDED.ss_source, \
             updated_at = now() \
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(&resolved.country)
    .bind(resolved.birth_date)
    .bind(resolved.target_retirement_age)
    .bind(resolved.current_gross_income)
    .bind(resolved.contribution_rate_pre_tax)
    .bind(resolved.contribution_rate_roth)
    .bind(formula)
    .bind(resolved.expected_real_return)
    .bind(resolved.inflation_rate)
    .bind(resolved.life_expectancy_age)
    .bind(resolved.target_replacement_ratio)
    // The resolved values are bound as-is, `None` included. `resolve_input`
    // already merged the stored row in under the transaction's row lock, so a
    // bound `None` means "this user has no Social Security figure", never
    // "unspecified, keep whatever is there" — that distinction was settled one
    // layer up.
    .bind(resolved.ss_monthly_benefit)
    .bind(resolved.ss_benefit_at_age.map(|(months, _)| months))
    .bind(resolved.ss_benefit_at_age.map(|(_, anchor)| anchor))
    .bind(resolved.ss_claiming_age.map(|(months, _)| months))
    .bind(resolved.ss_claiming_age.map(|(_, anchor)| anchor))
    .bind(resolved.ss_source)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal_error)?;

    tx.commit().await.map_err(internal_error)?;
    Ok((written, warning))
}

/// `GET /api/retirement/profile` — the caller's own profile, or a `200` with a
/// JSON `null` body when they have not set one up.
///
/// Absent is NOT a 404: "not set up yet" is a normal state, not an error, and a
/// synthesized placeholder would have to fabricate a date of birth, so `null` is
/// the honest answer.
///
/// Deliberately UNGATED while the `PUT` is Pro-gated: per AGENTS.md section 14 a
/// lapsed subscriber must still be able to see their own data. Writing a profile
/// is the gated action; reading your own row back is not.
pub async fn get_profile_handler(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
) -> Result<Json<Option<RetirementProfileResponse>>, (StatusCode, String)> {
    let profile = get_profile(&state.db, user_id).await?;
    Ok(Json(profile.map(|profile| RetirementProfileResponse {
        supported: retirement_supported_country(&profile.country),
        social_security: social_security_view(&profile),
        warning: None,
        profile,
    })))
}

/// `PUT /api/retirement/profile` — create or partially update the caller's own
/// profile. Pro-gated (#454 decision 4).
///
/// The gate is `require_caller_tier`, NOT `require_tier`: `require_tier`
/// resolves the BUDGET OWNER's subscription, and retirement data is user-scoped
/// with no budget in play at all, so the caller's own subscription is the only
/// correct thing to read here.
///
/// A non-US profile saves successfully and comes back with `supported: false` —
/// the region gate is a rendering gate, not a write gate.
pub async fn put_profile_handler(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
    Json(payload): Json<RetirementProfileInput>,
) -> Result<Json<RetirementProfileResponse>, (StatusCode, String)> {
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
    let (profile, warning) =
        upsert_profile(&state.db, user_id, &payload, Utc::now().date_naive()).await?;
    Ok(Json(RetirementProfileResponse {
        supported: retirement_supported_country(&profile.country),
        social_security: social_security_view(&profile),
        warning,
        profile,
    }))
}

// ===========================================================================
// nels#467 — the projection surface.
//
// `retirement_projection.rs` is PURE: it cannot read a profile, cannot read an
// asset, and has no clock. Everything below is the I/O half that assembles its
// `ProjectionRequest` from stored rows and the server clock. The split is the
// point — keeping the maths free of `sqlx` is what makes the engine testable
// against hand-built fixtures, and this file is the one place allowed to know
// where the numbers come from.
// ===========================================================================

/// What a caller is told when they ask for a projection before setting up a
/// profile. Names the missing thing and the action, because "not found" alone
/// reads as a bug to someone who has no idea a profile is a prerequisite.
pub const NO_PROFILE_MESSAGE: &str =
    "Set up your retirement profile first — a projection needs your date of birth, \
     income and target retirement age.";

/// The what-if body of `POST /api/retirement/projection`: #469's sliders.
///
/// Every field is absent-or-set, COALESCE-style — an absent field means "use
/// what is stored", never "reset it to a default". NOTHING here is persisted;
/// see [`post_projection_handler`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectionOverrides {
    pub contribution_rate_pre_tax: Option<f64>,
    pub contribution_rate_roth: Option<f64>,
    pub expected_real_return: Option<f64>,
    pub target_retirement_age: Option<u32>,
    pub current_gross_income: Option<f64>,
    pub target_replacement_ratio: Option<f64>,
    pub effective_tax_rate: Option<f64>,
    pub employer_match_formula: Option<EmployerMatch>,
    /// Claiming-age what-if in MONTHS (62–70 × 12), mirroring the profile's
    /// `ss_claiming_age_months` column and the engine's `SsInput` field so the
    /// override can flow straight through. Only applied when the stored profile
    /// already has a complete SS triple — a half-entered state keeps its
    /// `None` (no SS income) regardless of this override, so an override can
    /// never invent an SS stream that was not entered. A value outside 62–70
    /// years is rejected by the engine's own `validate` as a 400.
    pub ss_claiming_age_months: Option<i32>,
}

/// Sum the caller's assets into the engine's three tax-treatment buckets.
///
/// Two rules that are easy to get wrong and expensive to get wrong:
///
/// * **Closed assets are excluded.** `status` is `'active'`/`'closed'` and
///   closing an account never deletes the row (see the assets migration), so
///   summing every row would count a rolled-over 401(k) twice — once in the
///   closed shell and once in the account it moved to — and overstate
///   readiness. Overstating how ready someone is for retirement is the single
///   worst direction for this number to be wrong in.
/// * **A missing `current_balance` falls back to the holdings.** `None` means
///   no balance has ever been REPORTED, which is not the same fact as a
///   balance of zero; an asset carrying holdings but no roll-up balance would
///   otherwise contribute nothing at all.
///
/// The per-asset holdings read is a loop rather than one aggregate query: it
/// only runs for assets with no reported balance, a user has a handful of
/// assets, and reusing the ownership-checked `holdings_for_asset` helper keeps
/// this function from growing SQL of its own.
async fn projection_buckets(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<crate::retirement_projection::Buckets, (StatusCode, String)> {
    use crate::assets::TaxTreatment;

    let assets = crate::assets::list_assets_for_user(pool, user_id).await.map_err(internal_error)?;
    let mut buckets =
        crate::retirement_projection::Buckets { pre_tax: 0.0, roth: 0.0, taxable: 0.0 };

    for asset in assets.iter().filter(|a| a.status == "active") {
        let value = match asset.current_balance {
            Some(balance) => balance,
            None => crate::assets::holdings_for_asset(pool, user_id, asset.id)
                .await
                .map_err(internal_error)?
                .map(|holdings| holdings.iter().map(|h| h.market_value).sum())
                .unwrap_or(0.0),
        };
        match asset.tax_treatment {
            TaxTreatment::PreTax => buckets.pre_tax += value,
            TaxTreatment::Roth => buckets.roth += value,
            // HSA and Other fold into taxable: v1 has no dedicated HSA
            // withdrawal rule, and the engine's ordering is taxable first.
            TaxTreatment::Taxable | TaxTreatment::Hsa | TaxTreatment::Other => {
                buckets.taxable += value
            }
        }
    }
    Ok(buckets)
}

/// Assemble a [`ProjectionRequest`] from the caller's stored profile, their
/// assets and `today`, applying `overrides` on top without persisting them.
///
/// Shared by the two REST handlers and the chat arm, so all three project from
/// exactly the same inputs and cannot drift.
///
/// [`ProjectionRequest`]: crate::retirement_projection::ProjectionRequest
pub async fn build_projection_request(
    pool: &PgPool,
    user_id: Uuid,
    today: NaiveDate,
    overrides: &ProjectionOverrides,
) -> Result<crate::retirement_projection::ProjectionRequest, (StatusCode, String)> {
    use crate::retirement_projection as engine;

    let profile = get_profile(pool, user_id)
        .await?
        .ok_or((StatusCode::NOT_FOUND, NO_PROFILE_MESSAGE.to_string()))?;

    // STRICT decode, exactly as `upsert_profile` does on the write path and
    // deliberately NOT the lenient `EmployerMatch`. A stored formula missing
    // `tiers` decodes cleanly under the lenient type to "no employer match",
    // which would feed the projection a fabricated fact and UNDERSTATE this
    // user's retirement income with no signal at all. Corrupt JSONB in our own
    // table is our bug: fail loudly with a 500. See `StoredEmployerMatch`.
    let stored_match: EmployerMatch =
        serde_json::from_value::<StoredEmployerMatch>(profile.employer_match_formula.clone())
            .map_err(|e| internal_error(format!("corrupt employer_match_formula: {e}")))?
            .into();

    // Social Security enters the projection only when all three figures are
    // present. A half-entered state is computed WITH (as no SS income), never
    // rejected — #466's partial-input rule. `None` here means the SS stream is
    // 0.0 for every year, and #469 renders that as an explicitly ABSENT
    // segment rather than a zero one.
    let ss = match (
        profile.ss_monthly_benefit,
        profile.ss_benefit_at_age_months,
        profile.ss_claiming_age_months,
    ) {
        (Some(monthly_benefit), Some(quoted_at_age_months), Some(profile_claiming_age_months)) => {
            Some(engine::SsInput {
                monthly_benefit,
                quoted_at_age_months,
                claiming_age_months: overrides
                    .ss_claiming_age_months
                    .unwrap_or(profile_claiming_age_months),
                birth_date: profile.birth_date,
            })
        }
        _ => None,
    };

    Ok(engine::ProjectionRequest {
        starting_buckets: projection_buckets(pool, user_id).await?,
        birth_date: profile.birth_date,
        today,
        target_retirement_age: overrides
            .target_retirement_age
            .unwrap_or_else(|| profile.target_retirement_age.max(0) as u32),
        life_expectancy_age: profile.life_expectancy_age.max(0) as u32,
        current_gross_income: overrides
            .current_gross_income
            .unwrap_or(profile.current_gross_income),
        contribution_rate_pre_tax: overrides
            .contribution_rate_pre_tax
            .unwrap_or(profile.contribution_rate_pre_tax),
        contribution_rate_roth: overrides
            .contribution_rate_roth
            .unwrap_or(profile.contribution_rate_roth),
        expected_real_return: overrides
            .expected_real_return
            .unwrap_or(profile.expected_real_return),
        target_replacement_ratio: overrides
            .target_replacement_ratio
            .unwrap_or(profile.target_replacement_ratio),
        employer_match_formula: overrides
            .employer_match_formula
            .clone()
            .unwrap_or(stored_match),
        // The profile has no tax-rate column; the default is a named,
        // documented planning convention the POST body can override.
        effective_tax_rate: overrides
            .effective_tax_rate
            .unwrap_or(engine::DEFAULT_EFFECTIVE_TAX_RATE),
        ss,
        mc_seed: engine::MC_DEFAULT_SEED,
        mc_paths: engine::MC_DEFAULT_PATHS,
        w_max: engine::DEFAULT_W_MAX,
    })
}

/// Run the engine and translate its one error variant into an HTTP status.
///
/// `InvalidInput` is a 400 whatever produced it. On the `GET` path that is
/// nearly unreachable — `validate_profile` already refused the same shapes on
/// the way in — but "nearly" is not "never" (a profile written before a
/// validation rule existed, or an age the user has since aged past), and a
/// 400 naming the offending field beats a 500 naming nothing.
fn run_and_map(
    req: &crate::retirement_projection::ProjectionRequest,
) -> Result<crate::retirement_projection::Projection, (StatusCode, String)> {
    crate::retirement_projection::run_projection(req).map_err(|e| match e {
        crate::retirement_projection::ProjectionError::InvalidInput(msg) => {
            (StatusCode::BAD_REQUEST, msg)
        }
    })
}

/// `GET /api/retirement/projection` — the caller's projection from their
/// stored profile and assets, with no overrides.
///
/// Pro-gated on the CALLER's own tier (`require_caller_tier`, not
/// `require_tier`: retirement data is user-scoped and there is no budget in
/// play). Unlike `GET /api/retirement/profile`, which is deliberately ungated
/// so a lapsed subscriber can still read their own data back, a PROJECTION is
/// the paid feature itself — computing one is the product, not the record.
///
/// The gate runs before the profile load, so a free caller is refused without
/// a DB read and is never told whether they have a profile.
pub async fn get_projection_handler(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
) -> Result<Json<crate::retirement_projection::Projection>, (StatusCode, String)> {
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
    let req = build_projection_request(
        &state.db,
        user_id,
        Utc::now().date_naive(),
        &ProjectionOverrides::default(),
    )
    .await?;
    Ok(Json(run_and_map(&req)?))
}

/// `POST /api/retirement/projection` — the same projection with #469's slider
/// values merged over the stored profile.
///
/// **Nothing is persisted.** No DB write, no audit row. This is a what-if, and
/// a slider drag that quietly rewrote the stored profile would be data loss
/// dressed up as a feature. The response shape is identical to the `GET`'s, so
/// the UI swaps one for the other without a second renderer.
///
/// Override validation is the ENGINE's `validate`, reached through
/// `run_projection` — one set of range rules for every caller rather than a
/// second copy here that can drift from it. An out-of-range override is a 400.
pub async fn post_projection_handler(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
    Json(overrides): Json<ProjectionOverrides>,
) -> Result<Json<crate::retirement_projection::Projection>, (StatusCode, String)> {
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
    let req =
        build_projection_request(&state.db, user_id, Utc::now().date_naive(), &overrides).await?;
    Ok(Json(run_and_map(&req)?))
}

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

    fn wire_response(country: &str) -> serde_json::Value {
        let profile = wire_row(country);
        let response = RetirementProfileResponse {
            supported: retirement_supported_country(&profile.country),
            social_security: social_security_view(&profile),
            warning: None,
            profile,
        };
        serde_json::to_value(&response).expect("response must serialize")
    }

    /// Pins the WIRE SHAPE of `RetirementProfileResponse`.
    ///
    /// `#[serde(flatten)]` on `profile` is the only thing keeping the row's
    /// fields at the top level of the JSON body. Deleting that one attribute
    /// would silently nest every field under a `profile` key and break #469's
    /// planner screen, while every behavioural test in this file kept passing —
    /// they all read the Rust struct, never the serialized body. This test reads
    /// the body.
    #[test]
    fn response_serializes_flat_with_supported_alongside_the_row_fields() {
        let json = wire_response("US");
        let obj = json.as_object().expect("response must be a JSON object");

        assert!(obj.contains_key("country"), "country must be top level: {json}");
        assert!(obj.contains_key("birth_date"), "birth_date must be top level: {json}");
        assert!(
            obj.contains_key("target_retirement_age"),
            "target_retirement_age must be top level: {json}"
        );
        assert!(obj.contains_key("supported"), "supported must be top level: {json}");
        assert!(
            !obj.contains_key("profile"),
            "the row must be FLATTENED, not nested under a `profile` key: {json}"
        );
        assert_eq!(obj["country"], serde_json::json!("US"));
        assert_eq!(obj["supported"], serde_json::json!(true));
    }

    /// Pins the #466 half of the wire shape: the ENTERED values live on the
    /// flattened row and the DERIVED ones live in a nested `social_security`
    /// object, with no value appearing under both spellings.
    ///
    /// Two spellings of one fact is how two consumers come to disagree about
    /// which is authoritative, so this test asserts the absence as hard as the
    /// presence.
    #[test]
    fn social_security_derivations_are_nested_and_entered_values_are_not_duplicated() {
        let json = wire_response("US");
        let obj = json.as_object().expect("object");

        // Entered values: flattened, top level, exactly as stored.
        assert_eq!(obj["ss_monthly_benefit"], serde_json::json!(2_000.0));
        assert_eq!(obj["ss_benefit_at_age_months"], serde_json::json!(804));
        assert_eq!(obj["ss_claiming_age_months"], serde_json::json!(840));
        assert_eq!(obj["ss_source"], serde_json::json!("user_entered"));

        // Derived values: nested, and NOT flattened alongside the row.
        let ss = obj["social_security"].as_object().expect("social_security object");
        assert!(
            !obj.contains_key("adjusted_monthly_benefit"),
            "derived fields must stay INSIDE social_security: {json}"
        );
        for duplicated in ["monthly_benefit", "benefit_at_age_months", "claiming_age_months", "source"] {
            assert!(
                !ss.contains_key(duplicated),
                "`{duplicated}` is an ENTERED value and must not be repeated inside social_security: {json}"
            );
        }

        // Born mid-1980 => FRA 67y0m, and the 1943+ delayed credit.
        assert_eq!(ss["full_retirement_age_years"], serde_json::json!(67));
        assert_eq!(ss["full_retirement_age_months"], serde_json::json!(0));
        assert_eq!(ss["full_retirement_age_total_months"], serde_json::json!(804));
        assert_eq!(ss["delayed_credit_pct_per_year"], serde_json::json!(8.0));

        // Quoted at FRA, claiming at 70: $2,000 x 1.24.
        assert_eq!(ss["adjusted_monthly_benefit"], serde_json::json!(2_480.0));
        assert_eq!(ss["adjustment_applied"], serde_json::json!(true));
        assert_eq!(ss["benefit_entered"], serde_json::json!(true));
    }

    /// A non-US profile keeps every value its owner entered and loses only the
    /// DERIVED block, because every field in that block is US statute.
    ///
    /// Handing a French user a US Social Security full retirement age is exactly
    /// the fabricated fact #454 decision 2's "no dial, no empty bar, no zeros"
    /// forbids — and it would come from the same handler that is simultaneously
    /// reporting `supported: false`.
    #[test]
    fn a_non_us_profile_gets_no_derived_social_security_block() {
        let json = wire_response("FR");
        let obj = json.as_object().expect("object");

        assert_eq!(obj["supported"], serde_json::json!(false));
        assert_eq!(
            obj["social_security"],
            serde_json::Value::Null,
            "a non-US profile must not receive a US SSA full retirement age: {json}"
        );
        // Their own entered figure still comes back — the region gate is a
        // RENDERING gate, not a data-visibility one.
        assert_eq!(obj["ss_monthly_benefit"], serde_json::json!(2_000.0));
    }

    /// The acceptance criterion this whole feature turns on: a blank benefit is
    /// ABSENT on the wire, and absent is distinguishable from an entered $0.
    ///
    /// #469 renders the absent case as an explicitly labelled "not entered"
    /// segment of the stacked income bar. A zero segment would read as "you get
    /// nothing", which is both alarming and wrong — so `null` and `0.0` must
    /// never converge anywhere on the path out.
    #[test]
    fn a_blank_benefit_serializes_as_null_and_is_distinguishable_from_an_entered_zero() {
        let mut blank = wire_row("US");
        blank.ss_monthly_benefit = None;
        blank.ss_source = None;
        let blank_json = serde_json::to_value(RetirementProfileResponse {
            supported: true,
            social_security: social_security_view(&blank),
            warning: None,
            profile: blank,
        })
        .expect("serializes");

        let mut zero = wire_row("US");
        zero.ss_monthly_benefit = Some(0.0);
        let zero_json = serde_json::to_value(RetirementProfileResponse {
            supported: true,
            social_security: social_security_view(&zero),
            warning: None,
            profile: zero,
        })
        .expect("serializes");

        assert_eq!(blank_json["ss_monthly_benefit"], serde_json::Value::Null);
        assert_eq!(zero_json["ss_monthly_benefit"], serde_json::json!(0.0));
        assert_ne!(
            blank_json["ss_monthly_benefit"], zero_json["ss_monthly_benefit"],
            "absent must not serialize the same as an entered zero"
        );

        // The explicit flag, so a consumer never has to infer absence from a
        // falsy JSON value.
        assert_eq!(blank_json["social_security"]["benefit_entered"], serde_json::json!(false));
        assert_eq!(zero_json["social_security"]["benefit_entered"], serde_json::json!(true));

        // And the derived figure follows the same rule: absent stays absent, an
        // entered zero adjusts to a real zero.
        assert_eq!(
            blank_json["social_security"]["adjusted_monthly_benefit"],
            serde_json::Value::Null
        );
        assert_eq!(
            zero_json["social_security"]["adjusted_monthly_benefit"],
            serde_json::json!(0.0)
        );
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
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
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

/// DB-backed tests. `#[ignore]`d because they need a live Postgres; run with
/// `cargo test --bin backend -- --ignored` against the local pgvector container.
#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn mk_user(db: &PgPool) -> Uuid {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid)
            .bind(format!("retirement-{uid}@test.example"))
            .execute(db)
            .await
            .unwrap();
        uid
    }

    /// Seed `user` as a Pro subscriber for the handler-level tests.
    ///
    /// CRITICAL — the status MUST be `trialing`, not `active`. Do not "fix" this.
    /// `require_caller_tier` reads `PriceCatalog::from_env()`, and
    /// `STRIPE_PRICE_PRO_MONTHLY` / `STRIPE_PRICE_PRO_ANNUAL` are unset in the
    /// test process, so `tier_for()` returns `None` and an `active` row fails
    /// safe to `Tier::Basic` — a seeded "Pro" user would still get a 402.
    /// `entitlement::resolve` maps `trialing` to `Tier::Pro` unconditionally with
    /// no env var involved, which is the only way a real handler path (which
    /// cannot be handed an explicit catalog the way `access.rs`'s `*_with` tests
    /// are) can be tested as Pro without mutating process-global env vars.
    async fn mk_pro(db: &PgPool, user: Uuid) {
        // The customer id MUST be unique per user, not a literal. `subscriptions`
        // carries a UNIQUE index on `stripe_customer_id`
        // - see 20260630120000_subscriptions.sql - so a hardcoded 'cus_test'
        // makes the second concurrently-running test seeding a Pro user die with
        // a 23505 duplicate-key error. cargo runs tests in parallel by default,
        // so this is a real failure, not a theoretical one. Do not revert this to
        // a literal. Follows the `rag.rs` `seed_active_sub` house pattern of
        // binding the customer id as a parameter.
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) VALUES ($1, $2, 'trialing')",
        )
        .bind(user)
        .bind(format!("cus_test_{user}"))
        .execute(db)
        .await
        .unwrap();
    }

    async fn cleanup(db: &PgPool, user: Uuid) {
        // `retirement_profiles` and `subscriptions` both cascade off `users`.
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(db).await.ok();
    }

    fn state_for(db: &PgPool) -> AppState {
        AppState {
            db: db.clone(),
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid test date")
    }

    /// A fixed clock, so these tests are as deterministic as the pure ones.
    fn today() -> NaiveDate {
        d(2026, 7, 28)
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

    async fn row_count(db: &PgPool, user: Uuid) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM retirement_profiles WHERE user_id = $1")
            .bind(user)
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn upsert_creates_then_updates_the_same_row() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let (first, first_warn) = upsert_profile(&db, user, &create_input(), today()).await.unwrap();
        let update = RetirementProfileInput {
            current_gross_income: Some(120_000.0),
            target_retirement_age: Some(67),
            ..Default::default()
        };
        let (second, second_warn) = upsert_profile(&db, user, &update, today()).await.unwrap();
        assert!(first_warn.is_none(), "a future default age warns nothing: {first_warn:?}");
        assert!(second_warn.is_none(), "an explicit age warns nothing: {second_warn:?}");

        // The UNIQUE (user_id, member_ordinal) key means the second write lands
        // on the SAME row rather than creating a second member.
        assert_eq!(first.id, second.id, "second write must update the same row");
        assert_eq!(row_count(&db, user).await, 1);
        assert_eq!(second.current_gross_income, 120_000.0);
        assert_eq!(second.target_retirement_age, 67);
        // STRICTLY greater: `>=` is trivially true even if the upsert forgot its
        // `updated_at = now()` entirely, so it pins nothing.
        assert!(
            second.updated_at > first.updated_at,
            "the update must bump updated_at: {} then {}",
            first.updated_at,
            second.updated_at
        );

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn fresh_profile_carries_the_named_default_constants() {
        // The AC's explicit requirement: create with only the three fields that
        // cannot be defaulted, and every assumption must arrive from the named
        // Rust constants — the migration deliberately has no SQL DEFAULTs.
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let (row, warn) = upsert_profile(&db, user, &create_input(), today()).await.unwrap();
        assert!(warn.is_none(), "46-year-old, default 65 is in the future: {warn:?}");
        assert_eq!(row.target_retirement_age, DEFAULT_TARGET_RETIREMENT_AGE);
        assert_eq!(row.expected_real_return, DEFAULT_EXPECTED_REAL_RETURN);
        assert_eq!(row.inflation_rate, DEFAULT_INFLATION_RATE);
        assert_eq!(row.life_expectancy_age, DEFAULT_LIFE_EXPECTANCY_AGE);
        assert_eq!(row.target_replacement_ratio, DEFAULT_TARGET_REPLACEMENT_RATIO);
        let stored_match: EmployerMatch =
            serde_json::from_value(row.employer_match_formula.clone()).unwrap();
        assert_eq!(stored_match, default_employer_match());
        // Nothing is assumed about what the user contributes.
        assert_eq!(row.contribution_rate_pre_tax, 0.0);
        assert_eq!(row.contribution_rate_roth, 0.0);
        assert_eq!(row.member_ordinal, 1);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn unique_user_member_ordinal_rejects_a_duplicate_insert() {
        // The constraint, not just the application code, is what makes a second
        // household member impossible in v1.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        upsert_profile(&db, user, &create_input(), today()).await.unwrap();

        let dup = sqlx::query(
            "INSERT INTO retirement_profiles (\
                 id, user_id, member_ordinal, country, birth_date, target_retirement_age, \
                 current_gross_income, contribution_rate_pre_tax, contribution_rate_roth, \
                 employer_match_formula, expected_real_return, inflation_rate, \
                 life_expectancy_age, target_replacement_ratio\
             ) VALUES ($1, $2, 1, 'US', '1980-01-01', 65, 100000, 0, 0, '{}', 5, 2.5, 90, 0.75)",
        )
        .bind(Uuid::new_v4())
        .bind(user)
        .execute(&db)
        .await;
        assert!(dup.is_err(), "a second (user, member_ordinal = 1) row must be rejected");
        assert_eq!(row_count(&db, user).await, 1);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn partial_update_preserves_untouched_fields() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let full = RetirementProfileInput {
            contribution_rate_pre_tax: Some(8.0),
            contribution_rate_roth: Some(2.0),
            expected_real_return: Some(4.0),
            employer_match_formula: Some(EmployerMatch {
                tiers: vec![MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 }],
                annual_dollar_cap: Some(5_000.0),
            }),
            ..create_input()
        };
        upsert_profile(&db, user, &full, today()).await.unwrap();

        // Touch exactly one field; everything else must survive untouched.
        let (row, warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput { inflation_rate: Some(3.1), ..Default::default() },
            today(),
        )
        .await
        .unwrap();
        assert!(warn.is_none(), "an unrelated edit must not re-judge the stored age: {warn:?}");

        assert_eq!(row.inflation_rate, 3.1);
        assert_eq!(row.contribution_rate_pre_tax, 8.0);
        assert_eq!(row.contribution_rate_roth, 2.0);
        assert_eq!(row.expected_real_return, 4.0);
        assert_eq!(row.current_gross_income, 100_000.0);
        assert_eq!(row.birth_date, d(1980, 1, 1));
        let stored_match: EmployerMatch =
            serde_json::from_value(row.employer_match_formula.clone()).unwrap();
        assert_eq!(stored_match.tiers.len(), 1);
        assert_eq!(stored_match.annual_dollar_cap, Some(5_000.0));

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn non_us_country_saves_and_is_reported_unsupported() {
        // The region gate is a RENDERING gate, not a write gate: a French user's
        // country must be recordable, or the column cannot serve its stated
        // purpose. The lowercase input also pins the write path's uppercasing,
        // without which the DB's `^[A-Z]{2}$` CHECK would turn a perfectly good
        // payload into a 500.
        //
        // The "is reported" half is asserted off the ACTUAL handler responses,
        // not by re-calling `retirement_supported_country` here. Re-deriving the
        // rule in the test would only prove the rule agrees with itself and
        // would leave `supported: false` never once produced by real code — the
        // US/true case alone is not coverage of the gate.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;

        let input = RetirementProfileInput { country: Some("fr".to_string()), ..create_input() };
        let Json(written) =
            put_profile_handler(State(state_for(&db)), axum::Extension(user), Json(input))
                .await
                .expect("a non-US profile must SAVE, not be rejected");
        assert_eq!(written.profile.country, "FR");
        assert!(!written.supported, "a non-US profile must be reported unsupported");

        // And it reads back unsupported too, so the flag is not a write-path
        // accident.
        let Json(fetched) =
            get_profile_handler(State(state_for(&db)), axum::Extension(user)).await.unwrap();
        let fetched = fetched.expect("profile should exist");
        assert_eq!(fetched.profile.country, "FR");
        assert!(!fetched.supported);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn put_handler_writes_a_row() {
        // The REST half of "chat arm + REST both write the same rows": driven
        // through the actual handler, so the Pro gate and the response shape are
        // covered too.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;

        let Json(res) = put_profile_handler(
            State(state_for(&db)),
            axum::Extension(user),
            Json(create_input()),
        )
        .await
        .unwrap();

        assert!(res.supported, "a US profile must report supported");
        assert_eq!(res.profile.user_id, user);
        assert_eq!(res.profile.country, "US");
        assert_eq!(row_count(&db, user).await, 1);

        // And the GET handler reads back exactly that row, ungated.
        let Json(fetched) =
            get_profile_handler(State(state_for(&db)), axum::Extension(user)).await.unwrap();
        assert_eq!(fetched.expect("profile should exist").profile.id, res.profile.id);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn put_handler_rejects_a_non_pro_caller_with_402_and_writes_nothing() {
        let db = test_pool().await;
        let user = mk_user(&db).await; // no subscription row at all
        let err = put_profile_handler(
            State(state_for(&db)),
            axum::Extension(user),
            Json(create_input()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(row_count(&db, user).await, 0);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn get_handler_returns_null_for_a_user_with_no_profile() {
        // "Not set up yet" is a normal state, answered with a 200 and a JSON
        // null, never a 404 and never a fabricated profile.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let Json(res) =
            get_profile_handler(State(state_for(&db)), axum::Extension(user)).await.unwrap();
        assert!(res.is_none());
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn invalid_profile_is_rejected_and_writes_nothing() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        // Retirement age below the user's current age: validation runs on the
        // MERGED value, before any statement is issued.
        let input =
            RetirementProfileInput { target_retirement_age: Some(30), ..create_input() };
        let err = upsert_profile(&db, user, &input, today()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("current age"), "message was: {}", err.1);
        assert_eq!(row_count(&db, user).await, 0, "a rejected profile must write nothing");

        // A create missing a required field is also a 400, not a 500.
        let bare = RetirementProfileInput { country: Some("US".to_string()), ..Default::default() };
        let err = upsert_profile(&db, user, &bare, today()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("birth_date"), "message was: {}", err.1);
        assert_eq!(row_count(&db, user).await, 0);

        cleanup(&db, user).await;
    }

    /// The C1 regression, end to end against a real row.
    ///
    /// A profile created legally at 64 with a target retirement age of 65 must
    /// stay editable after the user's 65th birthday. Before the fix it did not:
    /// `validate_profile` runs on the MERGED profile, so the stored age was
    /// re-checked against today on every later write and a partial update
    /// touching only `inflation_rate` came back
    /// `400 "target retirement age must be greater than your current age"` — the
    /// profile was permanently uneditable, via REST and via chat alike, for
    /// income, assumptions, everything.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_profile_stays_editable_once_the_user_passes_their_target_retirement_age() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        // Born 1 Jan 1962. On the create date they are 64 and 65 is still in the
        // future, so this profile is legal.
        let create = RetirementProfileInput {
            birth_date: Some(d(1962, 1, 1)),
            target_retirement_age: Some(65),
            ..create_input()
        };
        let (created, created_warn) =
            upsert_profile(&db, user, &create, d(2026, 7, 28)).await.unwrap();
        assert_eq!(created.target_retirement_age, 65);
        assert!(created_warn.is_none(), "explicit age, no default substitution: {created_warn:?}");

        // Now advance the clock past their 65th birthday and touch ONE unrelated
        // field. The stored age is history; it must not block this.
        let later = d(2027, 7, 28); // they are 65
        let (updated, updated_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput { inflation_rate: Some(3.1), ..Default::default() },
            later,
        )
        .await
        .expect("an unrelated partial update must not be blocked by a stale retirement age");
        assert_eq!(updated.inflation_rate, 3.1);
        assert_eq!(updated.target_retirement_age, 65, "the stored age is carried forward untouched");
        assert_eq!(updated.id, created.id);
        assert!(
            updated_warn.is_none(),
            "an unrelated edit must not re-judge the stored (explicitly-set) age: {updated_warn:?}"
        );

        // But explicitly SETTING an age at or below the current age is still
        // rejected — the rule was scoped, not weakened.
        let err = upsert_profile(
            &db,
            user,
            &RetirementProfileInput { target_retirement_age: Some(65), ..Default::default() },
            later,
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("current age"), "message was: {}", err.1);

        cleanup(&db, user).await;
    }

    /// nels#533, end to end: an over-65 first-time create with no explicit
    /// target age SAVES — with the substituted default and a warning — instead
    /// of discarding the whole gathered create as a 400, and a follow-up turn
    /// that names a real age MERGES onto that row. One row, no re-gathering,
    /// no re-asking for the fields already stored on it.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn over_65_create_with_no_target_age_saves_and_warns_then_merges() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        // Born 1955: 71 on the create date. No target retirement age supplied,
        // so the default 65 is substituted — and it is already behind them.
        // Before nels#533 this whole create was a hard 400.
        let create = RetirementProfileInput {
            country: Some("US".to_string()),
            birth_date: Some(d(1955, 4, 2)),
            current_gross_income: Some(90_000.0),
            ..Default::default()
        };
        let (created, warn) = upsert_profile(&db, user, &create, d(2026, 7, 28)).await.unwrap();
        assert_eq!(created.target_retirement_age, DEFAULT_TARGET_RETIREMENT_AGE);
        let warn = warn.expect("the substituted default is behind this user, so it must warn");
        assert!(warn.contains("65"), "warning names the default: {warn}");
        assert!(warn.contains("71"), "warning names the current age: {warn}");
        assert_eq!(created.current_gross_income, 90_000.0, "everything gathered so far was saved");

        // The next turn supplies the real age. It must MERGE onto the saved row
        // — not re-create, not re-ask for the fields already on it.
        let (updated, updated_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput { target_retirement_age: Some(72), ..Default::default() },
            d(2026, 7, 28),
        )
        .await
        .unwrap();
        assert_eq!(updated.id, created.id, "a follow-up edit must update the same row");
        assert_eq!(updated.target_retirement_age, 72);
        assert_eq!(updated.current_gross_income, 90_000.0, "earlier fields survive the merge");
        assert!(
            updated_warn.is_none(),
            "an explicit age never warns, even in the past-behind position: {updated_warn:?}"
        );
        assert_eq!(row_count(&db, user).await, 1, "exactly one row through the whole flow");

        cleanup(&db, user).await;
    }

    /// A partial update that FAILS validation must leave the stored row exactly
    /// as it was.
    ///
    /// `invalid_profile_is_rejected_and_writes_nothing` only covers CREATES,
    /// where there was no row to damage in the first place. This is the case the
    /// whole read-merge-validate-then-write-once transaction exists for: an
    /// implementation that wrote first and validated after, or that validated
    /// per-field mid-write, would corrupt a good row here.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_rejected_partial_update_leaves_the_stored_row_untouched() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let seed = RetirementProfileInput {
            contribution_rate_pre_tax: Some(8.0),
            inflation_rate: Some(2.5),
            target_retirement_age: Some(67),
            ..create_input()
        };
        let (before, before_warn) = upsert_profile(&db, user, &seed, today()).await.unwrap();
        assert!(before_warn.is_none(), "explicit target age, no default: {before_warn:?}");

        // A valid field paired with an invalid one: the WHOLE update must be
        // rejected, including the field that was fine on its own.
        let err = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                contribution_rate_pre_tax: Some(12.0),
                inflation_rate: Some(-1.0),
                ..Default::default()
            },
            today(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        let after = get_profile(&db, user).await.unwrap().expect("profile should still exist");
        assert_eq!(after.contribution_rate_pre_tax, before.contribution_rate_pre_tax);
        assert_eq!(after.inflation_rate, before.inflation_rate);
        assert_eq!(after.target_retirement_age, before.target_retirement_age);
        assert_eq!(after.updated_at, before.updated_at, "a rejected update must not even bump updated_at");
        assert_eq!(row_count(&db, user).await, 1);

        cleanup(&db, user).await;
    }

    /// Corrupt `employer_match_formula` JSONB must FAIL LOUDLY as a 500 and
    /// change nothing, never decode quietly to "this user has no employer match".
    ///
    /// All three of these decoded CLEANLY through the lenient `EmployerMatch`
    /// before the storage side was switched to `StoredEmployerMatch`: `{}` and
    /// `{"annual_dollar_cap": 5000}` both hit `#[serde(default)]` on `tiers` and
    /// yielded an empty tier list, i.e. a fabricated "no match" that the very
    /// next `DO UPDATE` would have written back over the user's real formula and
    /// that #467 would then have consumed as fact. Understating a match is the
    /// exact direction of error this module refuses to make.
    ///
    /// `{"tiers": 7}` is the type-error case and always failed; it is kept as the
    /// control that proves the assertion below can distinguish the two.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_corrupt_stored_employer_match_is_a_500_and_leaves_the_row_alone() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let real_match = EmployerMatch {
            tiers: vec![
                MatchTier { employee_pct_up_to: 3.0, match_pct: 100.0 },
                MatchTier { employee_pct_up_to: 5.0, match_pct: 50.0 },
            ],
            annual_dollar_cap: None,
        };
        let (seeded, seeded_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                employer_match_formula: Some(real_match),
                contribution_rate_pre_tax: Some(6.0),
                ..create_input()
            },
            today(),
        )
        .await
        .unwrap();
        assert!(seeded_warn.is_none(), "no default-age substitution here: {seeded_warn:?}");

        for corrupt in [
            // The control: a type error, which failed even before the fix.
            r#"{"tiers": 7}"#,
            // The two that USED to decode cleanly to zero tiers.
            r#"{"annual_dollar_cap": 5000}"#,
            "{}",
        ] {
            sqlx::query(
                "UPDATE retirement_profiles SET employer_match_formula = $1::jsonb \
                 WHERE user_id = $2 AND member_ordinal = 1",
            )
            .bind(corrupt)
            .bind(user)
            .execute(&db)
            .await
            .unwrap();

            let err = upsert_profile(
                &db,
                user,
                &RetirementProfileInput { inflation_rate: Some(3.1), ..Default::default() },
                today(),
            )
            .await
            .unwrap_err();
            assert_eq!(
                err.0,
                StatusCode::INTERNAL_SERVER_ERROR,
                "corrupt JSONB in our own table is OUR bug, so it is a 500 and not a 400: {corrupt}"
            );

            // The transaction rolled back: neither the update the caller asked
            // for nor a rewritten formula landed.
            let (formula, inflation): (serde_json::Value, f64) = sqlx::query_as(
                "SELECT employer_match_formula, inflation_rate FROM retirement_profiles \
                 WHERE user_id = $1 AND member_ordinal = 1",
            )
            .bind(user)
            .fetch_one(&db)
            .await
            .unwrap();
            assert_eq!(
                formula,
                serde_json::from_str::<serde_json::Value>(corrupt).unwrap(),
                "the corrupt value must be left exactly as found, not overwritten with an empty match"
            );
            assert_eq!(
                inflation, seeded.inflation_rate,
                "a failed decode must not partially apply the caller's update: {corrupt}"
            );
            assert_eq!(row_count(&db, user).await, 1);
        }

        cleanup(&db, user).await;
    }

    /// Cross-user isolation. This is user-scoped financial PII with no
    /// budget-permission layer in front of it, so the `user_id` predicate on
    /// every statement is the ONLY thing separating two users' profiles.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn one_users_write_cannot_read_or_overwrite_another_users_profile() {
        let db = test_pool().await;
        let alice = mk_user(&db).await;
        let bob = mk_user(&db).await;

        let (alice_row, alice_warn) = upsert_profile(&db, alice, &create_input(), today()).await.unwrap();
        assert!(alice_warn.is_none(), "no default-age substitution: {alice_warn:?}");
        let bob_input = RetirementProfileInput {
            country: Some("US".to_string()),
            birth_date: Some(d(1990, 3, 3)),
            current_gross_income: Some(55_000.0),
            target_retirement_age: Some(70),
            ..Default::default()
        };
        let (bob_row, bob_warn) = upsert_profile(&db, bob, &bob_input, today()).await.unwrap();
        assert!(bob_warn.is_none(), "explicit age, no warning: {bob_warn:?}");

        // Separate rows, not one shared one.
        assert_ne!(alice_row.id, bob_row.id);
        assert_eq!(row_count(&db, alice).await, 1);
        assert_eq!(row_count(&db, bob).await, 1);

        // Bob's write did not touch a single one of Alice's values.
        let alice_after = get_profile(&db, alice).await.unwrap().expect("alice's profile");
        assert_eq!(alice_after.id, alice_row.id);
        assert_eq!(alice_after.current_gross_income, 100_000.0);
        assert_eq!(alice_after.birth_date, d(1980, 1, 1));
        assert_eq!(alice_after.target_retirement_age, DEFAULT_TARGET_RETIREMENT_AGE);

        // And each read is scoped to its own owner.
        let bob_after = get_profile(&db, bob).await.unwrap().expect("bob's profile");
        assert_eq!(bob_after.user_id, bob);
        assert_eq!(bob_after.current_gross_income, 55_000.0);

        // Deleting Bob leaves Alice entirely alone.
        cleanup(&db, bob).await;
        assert_eq!(row_count(&db, alice).await, 1);
        cleanup(&db, alice).await;
    }

    /// Regression: two overlapping partial updates to DIFFERENT fields must both
    /// survive.
    ///
    /// `upsert_profile` reads the row, merges in Rust, then writes every column
    /// as a resolved literal — there is no SQL `COALESCE` to fall back on. So
    /// without the `FOR UPDATE` lock this is a genuine lost update: both calls
    /// read `(6.0, 2.5)`, A resolves to `(10.0, 2.5)`, B resolves to
    /// `(6.0, 3.0)`, and whichever commits second silently discards the other's
    /// field even though its caller already got a 200 showing the new value.
    ///
    /// The two calls run on SEPARATE pool connections via `tokio::join!`, which
    /// is what makes this a real concurrency test rather than a sequential one:
    /// the first `SELECT` of each is issued before either `INSERT`, exactly the
    /// interleaving that loses the update. With the lock, the second call blocks
    /// on the first's row lock and re-reads the committed value.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn concurrent_partial_updates_to_different_fields_do_not_clobber_each_other() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let seed = RetirementProfileInput {
            contribution_rate_pre_tax: Some(6.0),
            inflation_rate: Some(2.5),
            ..create_input()
        };
        upsert_profile(&db, user, &seed, today()).await.unwrap();

        let a = RetirementProfileInput {
            contribution_rate_pre_tax: Some(10.0),
            ..Default::default()
        };
        let b = RetirementProfileInput { inflation_rate: Some(3.0), ..Default::default() };

        let (ra, rb) = tokio::join!(
            upsert_profile(&db, user, &a, today()),
            upsert_profile(&db, user, &b, today()),
        );
        ra.expect("concurrent update A must succeed");
        rb.expect("concurrent update B must succeed");

        let row = get_profile(&db, user).await.unwrap().expect("profile should exist");
        assert_eq!(
            row.contribution_rate_pre_tax, 10.0,
            "A's contribution_rate_pre_tax was lost by a concurrent partial update"
        );
        assert_eq!(
            row.inflation_rate, 3.0,
            "B's inflation_rate was lost by a concurrent partial update"
        );
        assert_eq!(row_count(&db, user).await, 1);

        cleanup(&db, user).await;
    }

    /// The concurrent-CREATE twin of the test above, with NO pre-seeded row.
    ///
    /// That one seeds a row first, so it only ever exercises the case
    /// `SELECT … FOR UPDATE` already handled. This one is the case the ADVISORY
    /// lock exists for: with no row to lock, both callers read `None` and both
    /// merge onto the DEFAULTS, so A's `contribution_rate_pre_tax` is destroyed
    /// by B's default of `0.0` and B's `target_retirement_age` is destroyed by
    /// A's default of 65 — whichever commits second wins, both callers got a
    /// success, and nothing anywhere records the loss.
    ///
    /// Both writers send the three required create fields, exactly as they must,
    /// plus one defaultable field each. Whoever runs second must see the other's
    /// committed row and merge onto it rather than onto the defaults, so BOTH
    /// values survive.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn concurrent_creates_with_no_existing_row_do_not_clobber_each_other() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        assert_eq!(row_count(&db, user).await, 0, "this test's whole point is starting with no row");

        let a = RetirementProfileInput {
            contribution_rate_pre_tax: Some(6.0),
            ..create_input()
        };
        let b = RetirementProfileInput { target_retirement_age: Some(62), ..create_input() };

        let (ra, rb) = tokio::join!(
            upsert_profile(&db, user, &a, today()),
            upsert_profile(&db, user, &b, today()),
        );
        ra.expect("concurrent create A must succeed");
        rb.expect("concurrent create B must succeed");

        let row = get_profile(&db, user).await.unwrap().expect("profile should exist");
        assert_eq!(
            row.contribution_rate_pre_tax, 6.0,
            "A's contribution_rate_pre_tax was destroyed by B's default of 0.0"
        );
        assert_eq!(
            row.target_retirement_age, 62,
            "B's target_retirement_age was destroyed by A's default of 65"
        );
        assert_eq!(row_count(&db, user).await, 1, "the UNIQUE key still allows exactly one row");

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn deleting_the_user_cascades_the_profile_away() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        upsert_profile(&db, user, &create_input(), today()).await.unwrap();
        assert_eq!(row_count(&db, user).await, 1);

        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&db).await.unwrap();
        assert_eq!(row_count(&db, user).await, 0, "ON DELETE CASCADE must remove the profile");
    }

    /// The six #466 columns round-trip through the ONE write path, and
    /// `ss_source` is DERIVED rather than supplied.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn social_security_columns_round_trip_and_source_is_derived() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        // No Social Security yet: every column NULL, and NOT zero.
        let (created, created_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                country: Some("US".into()),
                birth_date: Some(NaiveDate::from_ymd_opt(1957, 3, 4).unwrap()),
                current_gross_income: Some(120_000.0),
                // Explicit, even though this cohort is already 69 on the test
                // date and nels#533 no longer rejects a defaulted age in the past
                // — explicit keeps this test about the SS columns, not the warning.
                target_retirement_age: Some(72),
                ..Default::default()
            },
            NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
        )
        .await
        .expect("create");
        assert_eq!(created.ss_monthly_benefit, None);
        assert_eq!(created.ss_source, None, "no benefit means no provenance");
        assert!(created_warn.is_none(), "explicit age, no substitution: {created_warn:?}");

        // Now the figure, anchored at FRA. Born 1957 => 66y6m => 798 months,
        // which is exactly the value a whole-year column could not hold.
        let (saved, saved_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                ss_monthly_benefit: Some(2_400.0),
                ss_benefit_at_age: Some(SsAnchor::Fra),
                ss_claiming_age: Some(SsAnchor::Age(70)),
                ..Default::default()
            },
            NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
        )
        .await
        .expect("update");
        assert!(saved_warn.is_none(), "an update not touching the age warns nothing: {saved_warn:?}");
        assert_eq!(saved.ss_monthly_benefit, Some(2_400.0));
        assert_eq!(saved.ss_benefit_at_age_months, Some(798));
        assert_eq!(saved.ss_benefit_at_age_anchor.as_deref(), Some(SS_ANCHOR_FRA));
        assert_eq!(saved.ss_claiming_age_months, Some(840));
        assert_eq!(saved.ss_claiming_age_anchor.as_deref(), Some(SS_ANCHOR_EXPLICIT));
        assert_eq!(
            saved.ss_source.as_deref(),
            Some(SS_SOURCE_USER_ENTERED),
            "provenance is derived by the write path, not supplied by the caller"
        );

        // Everything #465 stored is untouched by an SS-only write.
        assert_eq!(saved.current_gross_income, 120_000.0);
    }

    /// The staleness fix, asserted against a real row rather than only against
    /// `resolve_input`: correcting the birth date MOVES an FRA-anchored age and
    /// LEAVES an explicitly-anchored one alone.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn correcting_the_birth_date_re_derives_only_the_fra_anchored_age() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let today = NaiveDate::from_ymd_opt(2026, 7, 28).unwrap();

        upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                country: Some("US".into()),
                birth_date: Some(NaiveDate::from_ymd_opt(1957, 3, 4).unwrap()),
                current_gross_income: Some(120_000.0),
                target_retirement_age: Some(72),
                ss_monthly_benefit: Some(2_400.0),
                ss_benefit_at_age: Some(SsAnchor::Fra),      // 798 for 1957
                ss_claiming_age: Some(SsAnchor::Age(62)),    // 744, explicit
                ..Default::default()
            },
            today,
        )
        .await
        .expect("create");

        // A later turn corrects ONLY the birth date.
        let (fixed, fixed_warn) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                birth_date: Some(NaiveDate::from_ymd_opt(1960, 3, 4).unwrap()),
                ..Default::default()
            },
            today,
        )
        .await
        .expect("update");
        assert!(fixed_warn.is_none(), "an update not touching the age warns nothing: {fixed_warn:?}");

        assert_eq!(
            fixed.ss_benefit_at_age_months,
            Some(804),
            "an fra anchor must follow the corrected birth date, or \"at FRA\" silently \
             becomes \"six months early\""
        );
        assert_eq!(
            fixed.ss_claiming_age_months,
            Some(744),
            "an explicit age is what the user actually said and must not move"
        );
    }

    /// A profile carrying a Social Security claiming age stays EDITABLE as its
    /// owner ages past it.
    ///
    /// This is #465's froze-the-profile defect class, checked against the
    /// database rather than only the pure validator: a rule re-evaluated against
    /// a STORED value that a passing clock can break makes a legitimate profile
    /// permanently unwritable. None of the #466 rules is clock-dependent, and
    /// this asserts that end to end.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_profile_with_a_claiming_age_stays_editable_as_its_owner_ages_past_it() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        // Created at 61, claiming at 62.
        upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                country: Some("US".into()),
                birth_date: Some(NaiveDate::from_ymd_opt(1960, 5, 1).unwrap()),
                current_gross_income: Some(80_000.0),
                target_retirement_age: Some(66),
                ss_monthly_benefit: Some(1_900.0),
                ss_benefit_at_age: Some(SsAnchor::Age(62)),
                ss_claiming_age: Some(SsAnchor::Age(62)),
                ..Default::default()
            },
            NaiveDate::from_ymd_opt(2021, 7, 28).unwrap(),
        )
        .await
        .expect("create at 61");

        // Years later, well past both the claiming age and the target
        // retirement age, an unrelated edit must still succeed.
        for year in [2023, 2027, 2035] {
            let (updated, warn) = upsert_profile(
                &db,
                user,
                &RetirementProfileInput { inflation_rate: Some(2.1), ..Default::default() },
                NaiveDate::from_ymd_opt(year, 7, 28).unwrap(),
            )
            .await
            .unwrap_or_else(|e| panic!("profile must stay editable in {year}: {e:?}"));
            assert_eq!(updated.ss_claiming_age_months, Some(744));
            assert!(
                warn.is_none(),
                "an unrelated edit must not re-judge the stored age, even with a warning: {warn:?}"
            );
        }
    }

    /// The `ss_source` CHECK is real, and can only be reached by raw SQL —
    /// the Rust path cannot emit an invalid provenance.
    ///
    /// That is exactly why it is worth asserting: the constraint is the thing
    /// standing between a future writer (or a hand-edited row) and a value #467
    /// would consume as fact.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn the_ss_source_check_rejects_a_value_outside_the_closed_set() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let err = sqlx::query(
            "INSERT INTO retirement_profiles (id, user_id, member_ordinal, country, birth_date, \
                 target_retirement_age, current_gross_income, contribution_rate_pre_tax, \
                 contribution_rate_roth, employer_match_formula, expected_real_return, \
                 inflation_rate, life_expectancy_age, target_replacement_ratio, \
                 ss_monthly_benefit, ss_benefit_at_age_months, ss_benefit_at_age_anchor, ss_source) \
             VALUES ($1, $2, 1, 'US', '1980-01-02', 65, 100000, 0, 0, '{\"tiers\":[],\"annual_dollar_cap\":null}'::jsonb, \
                     5, 2.5, 90, 0.75, 2000, 804, 'explicit', 'estimated')",
        )
        .bind(Uuid::new_v4())
        .bind(user)
        .execute(&db)
        .await
        .expect_err("'estimated' is not a v1 provenance and the CHECK must say so");
        assert_eq!(
            err.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514"),
            "expected a CHECK violation, got {err}"
        );
    }

    /// A benefit with no anchor age is unusable, and both layers say so: the
    /// Rust validator returns a 400 the user can act on, and the migration's
    /// CHECK stands behind it for anything that bypasses the validator.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_benefit_without_an_anchor_age_is_refused_by_both_layers() {
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let (status, msg) = upsert_profile(
            &db,
            user,
            &RetirementProfileInput {
                country: Some("US".into()),
                birth_date: Some(NaiveDate::from_ymd_opt(1980, 1, 2).unwrap()),
                current_gross_income: Some(100_000.0),
                ss_monthly_benefit: Some(2_000.0),
                ..Default::default()
            },
            NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
        )
        .await
        .expect_err("a benefit with no anchor age must be a 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.contains("quoted at"), "{msg}");

        let err = sqlx::query(
            "INSERT INTO retirement_profiles (id, user_id, member_ordinal, country, birth_date, \
                 target_retirement_age, current_gross_income, contribution_rate_pre_tax, \
                 contribution_rate_roth, employer_match_formula, expected_real_return, \
                 inflation_rate, life_expectancy_age, target_replacement_ratio, \
                 ss_monthly_benefit, ss_source) \
             VALUES ($1, $2, 1, 'US', '1980-01-02', 65, 100000, 0, 0, '{\"tiers\":[],\"annual_dollar_cap\":null}'::jsonb, \
                     5, 2.5, 90, 0.75, 2000, 'user_entered')",
        )
        .bind(Uuid::new_v4())
        .bind(user)
        .execute(&db)
        .await
        .expect_err("the CHECK must refuse a benefit with no anchor");
        assert_eq!(err.as_database_error().and_then(|e| e.code()).as_deref(), Some("23514"));
    }

    /// The finite CHECK is real, and it is NOT implied by the `>= 0` sign check.
    ///
    /// PostgreSQL sorts NaN as GREATER THAN every number, so `'NaN'::float8 >= 0`
    /// is TRUE and sails straight through the sign check. The consequence is
    /// silent: a NaN benefit is refused by `claiming_adjustment`, whose error
    /// `social_security_view` discards with `.ok()`, so the adjusted figure
    /// quietly becomes null and the user is told nothing. Caught by Copilot on
    /// PR #504.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn the_ss_benefit_finite_check_rejects_nan_and_infinity() {
        let db = test_pool().await;

        // The premise, asserted so the rationale above cannot rot: NaN really
        // does pass a `>= 0` test in PostgreSQL.
        let (nan_ge_zero,): (bool,) = sqlx::query_as("SELECT 'NaN'::float8 >= 0")
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(nan_ge_zero, "if this ever changes, the finite CHECK's rationale needs rewriting");

        for literal in ["NaN", "Infinity", "-Infinity"] {
            let user = mk_user(&db).await;
            let err = sqlx::query(&format!(
                "INSERT INTO retirement_profiles (id, user_id, member_ordinal, country, birth_date, \
                     target_retirement_age, current_gross_income, contribution_rate_pre_tax, \
                     contribution_rate_roth, employer_match_formula, expected_real_return, \
                     inflation_rate, life_expectancy_age, target_replacement_ratio, \
                     ss_monthly_benefit, ss_benefit_at_age_months, ss_benefit_at_age_anchor, ss_source) \
                 VALUES ($1, $2, 1, 'US', '1980-01-02', 65, 100000, 0, 0, '{{\"tiers\":[],\"annual_dollar_cap\":null}}'::jsonb, \
                         5, 2.5, 90, 0.75, '{literal}'::float8, 804, 'explicit', 'user_entered')"
            ))
            .bind(Uuid::new_v4())
            .bind(user)
            .execute(&db)
            .await
            .unwrap_err();
            assert_eq!(
                err.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("23514"),
                "{literal} must be refused by the finite CHECK, got {err}"
            );
        }
    }

    // =====================================================================
    // nels#467 Task 9 — the projection handlers.
    // =====================================================================

    /// Seed one asset with a directly reported balance.
    async fn mk_asset(db: &PgPool, user: Uuid, name: &str, tt: crate::assets::TaxTreatment, bal: f64) {
        crate::assets::insert_asset(
            db,
            user,
            None,
            name,
            crate::assets::AssetType::RetirementAccount,
            tt,
            None,
            Some(bal),
            "USD",
        )
        .await
        .unwrap();
    }

    /// A profile with enough substance for the engine to produce a non-trivial
    /// number: mid-career, contributing, with a match.
    fn projectable_input() -> RetirementProfileInput {
        RetirementProfileInput {
            contribution_rate_pre_tax: Some(8.0),
            contribution_rate_roth: Some(2.0),
            employer_match_formula: Some(EmployerMatch {
                tiers: vec![MatchTier { employee_pct_up_to: 6.0, match_pct: 50.0 }],
                annual_dollar_cap: None,
            }),
            ..create_input()
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_get_requires_pro_and_does_no_engine_work_for_a_free_caller() {
        // The gate must fire BEFORE the profile load, so a free caller with no
        // profile at all still gets 402 and never the 404. Ordering the other
        // way would leak "you have not set up a profile" to someone who is not
        // entitled to the feature, and would run a DB read for a caller who was
        // never going to be served.
        let db = test_pool().await;
        let user = mk_user(&db).await;

        let err = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .expect_err("a non-Pro caller must be refused");
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_get_without_a_profile_is_a_404_telling_the_user_what_to_do() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;

        let err = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .expect_err("no profile must not project");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert!(
            err.1.to_lowercase().contains("retirement profile"),
            "the message must name what is missing, got {}",
            err.1
        );

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_get_returns_a_concrete_projection_echoing_its_inputs() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        upsert_profile(&db, user, &projectable_input(), today()).await.unwrap();
        mk_asset(&db, user, "401k", crate::assets::TaxTreatment::PreTax, 250_000.0).await;
        mk_asset(&db, user, "Roth IRA", crate::assets::TaxTreatment::Roth, 80_000.0).await;
        mk_asset(&db, user, "Brokerage", crate::assets::TaxTreatment::Taxable, 40_000.0).await;

        let Json(p) = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .expect("a seeded Pro user with a profile must project");

        // A real number, not a placeholder and never a NaN serialized as null.
        assert!(
            p.deterministic_monthly_income.is_finite() && p.deterministic_monthly_income > 0.0,
            "headline must be a concrete positive figure, got {}",
            p.deterministic_monthly_income
        );
        // The assumptions echo the STORED profile, which is what stops #469
        // re-deriving them and drifting.
        assert_eq!(p.assumptions.contribution_rate_pre_tax, 8.0);
        assert_eq!(p.assumptions.contribution_rate_roth, 2.0);
        assert_eq!(p.assumptions.current_gross_income, 100_000.0);
        assert_eq!(p.assumptions.birth_date, d(1980, 1, 1));
        // The buckets are assembled from the assets, split by tax treatment.
        assert_eq!(p.assumptions.starting_buckets.pre_tax, 250_000.0);
        assert_eq!(p.assumptions.starting_buckets.roth, 80_000.0);
        assert_eq!(p.assumptions.starting_buckets.taxable, 40_000.0);
        // Defaults the profile has no column for.
        assert_eq!(
            p.assumptions.effective_tax_rate,
            crate::retirement_projection::DEFAULT_EFFECTIVE_TAX_RATE
        );
        assert_eq!(p.assumptions.mc_seed, crate::retirement_projection::MC_DEFAULT_SEED);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_get_ignores_closed_assets_and_sums_holdings_when_no_balance() {
        // Two distinct facts. A CLOSED account is not part of the portfolio and
        // counting it overstates readiness. An asset with no reported balance
        // falls back to the sum of its holdings' market values rather than
        // silently contributing zero.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        upsert_profile(&db, user, &projectable_input(), today()).await.unwrap();

        mk_asset(&db, user, "Open 401k", crate::assets::TaxTreatment::PreTax, 100_000.0).await;
        let closed = crate::assets::insert_asset(
            &db, user, None, "Rolled-over 401k",
            crate::assets::AssetType::RetirementAccount,
            crate::assets::TaxTreatment::PreTax, None, Some(999_999.0), "USD",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE assets SET status = 'closed' WHERE id = $1")
            .bind(closed.id)
            .execute(&db)
            .await
            .unwrap();

        // No `current_balance` — value must come from the holdings rows.
        let held = crate::assets::insert_asset(
            &db, user, None, "Brokerage",
            crate::assets::AssetType::Brokerage,
            crate::assets::TaxTreatment::Taxable, None, None, "USD",
        )
        .await
        .unwrap();
        let sec: Uuid = sqlx::query_scalar(
            "INSERT INTO securities (id, provider, provider_security_id, ticker, name, security_type) \
             VALUES ($1, 'plaid', $2, 'TST', 'Test Fund', 'equity') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("sec_{user}"))
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value, as_of) \
             VALUES ($1, $2, $3, 10, 5000, now())",
        )
        .bind(Uuid::new_v4())
        .bind(held.id)
        .bind(sec)
        .execute(&db)
        .await
        .unwrap();

        let Json(p) = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .unwrap();

        assert_eq!(
            p.assumptions.starting_buckets.pre_tax, 100_000.0,
            "a closed asset must not count toward the portfolio"
        );
        assert_eq!(
            p.assumptions.starting_buckets.taxable, 5_000.0,
            "an asset with no reported balance must fall back to its holdings"
        );

        cleanup(&db, user).await;
        sqlx::query("DELETE FROM securities WHERE id = $1").bind(sec).execute(&db).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_post_applies_overrides_without_persisting_them() {
        // The whole point of the POST: it is a what-if, not a save. A slider
        // drag that quietly rewrote the stored profile would be a data-loss bug
        // dressed up as a feature.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        let (stored, stored_warn) =
            upsert_profile(&db, user, &projectable_input(), today()).await.unwrap();
        assert!(stored_warn.is_none(), "projectable fixture must not warn: {stored_warn:?}");
        mk_asset(&db, user, "401k", crate::assets::TaxTreatment::PreTax, 250_000.0).await;

        let Json(base) = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .unwrap();

        let overrides = ProjectionOverrides {
            contribution_rate_pre_tax: Some(20.0),
            ..Default::default()
        };
        let Json(what_if) =
            post_projection_handler(State(state_for(&db)), axum::Extension(user), Json(overrides))
                .await
                .unwrap();

        assert_eq!(what_if.assumptions.contribution_rate_pre_tax, 20.0);
        assert!(
            what_if.deterministic_monthly_income > base.deterministic_monthly_income,
            "contributing 20% instead of 8% must project more income: {} vs {}",
            what_if.deterministic_monthly_income,
            base.deterministic_monthly_income
        );
        // Absent override fields fall through to the stored value.
        assert_eq!(what_if.assumptions.contribution_rate_roth, 2.0);

        // Nothing was written.
        let after = get_profile(&db, user).await.unwrap().unwrap();
        assert_eq!(after.contribution_rate_pre_tax, 8.0, "the override must not persist");
        assert_eq!(after.updated_at, stored.updated_at, "no write may have touched the row");

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_post_applies_a_claiming_age_override_only_to_a_complete_ss_triple() {
        // The dashboard's claiming-age control is a what-if: it re-floors the
        // engine's claiming age without touching the stored profile. It must
        // only ever apply when the user HAS a complete entered SS triple —
        // half-entered profiles carry no SS income and an override must not
        // invent one.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        let input = RetirementProfileInput {
            ss_monthly_benefit: Some(2_000.0),
            ss_benefit_at_age: Some(SsAnchor::Fra),
            ss_claiming_age: Some(SsAnchor::Age(67)),
            ..projectable_input()
        };
        upsert_profile(&db, user, &input, today()).await.unwrap();
        mk_asset(&db, user, "401k", crate::assets::TaxTreatment::PreTax, 250_000.0).await;

        let Json(base) = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .unwrap();
        assert_eq!(base.assumptions.ss_claiming_age_used, Some(67));

        let overrides = ProjectionOverrides {
            ss_claiming_age_months: Some(70 * 12),
            ..Default::default()
        };
        let Json(what_if) =
            post_projection_handler(State(state_for(&db)), axum::Extension(user), Json(overrides))
                .await
                .unwrap();
        assert_eq!(what_if.assumptions.ss_claiming_age_used, Some(70));
        assert!(
            what_if.assumptions.ss_adjusted_monthly_benefit.unwrap()
                > base.assumptions.ss_adjusted_monthly_benefit.unwrap(),
            "claiming later must raise the adjusted benefit: {:?} vs {:?}",
            what_if.assumptions.ss_adjusted_monthly_benefit,
            base.assumptions.ss_adjusted_monthly_benefit
        );

        // Nothing persisted: the stored claiming age is untouched.
        let after = get_profile(&db, user).await.unwrap().unwrap();
        assert_eq!(after.ss_claiming_age_months, Some(67 * 12));

        cleanup(&db, user).await;

        // A no-SS profile must ignore the override entirely. Uses a SECOND
        // user: `upsert_profile` merges absent SS fields onto the STORED row
        // (§23 — "leave the stored value alone"), so on `user` above the stored
        // quoted/claiming ages would quietly re-complete the triple. A fresh
        // user never had an SS figure, so the override must not invent one.
        let half_user = mk_user(&db).await;
        mk_pro(&db, half_user).await;
        upsert_profile(&db, half_user, &projectable_input(), today()).await.unwrap();
        let Json(no_ss) = post_projection_handler(
            State(state_for(&db)),
            axum::Extension(half_user),
            Json(ProjectionOverrides {
                ss_claiming_age_months: Some(70 * 12),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(no_ss.assumptions.ss_claiming_age_used, None);
        assert_eq!(no_ss.assumptions.ss_adjusted_monthly_benefit, None);

        cleanup(&db, half_user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_post_rejects_an_out_of_range_override_as_a_400() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        upsert_profile(&db, user, &projectable_input(), today()).await.unwrap();

        for bad in [f64::NAN, -0.1, 1.5] {
            let err = post_projection_handler(
                State(state_for(&db)),
                axum::Extension(user),
                Json(ProjectionOverrides { effective_tax_rate: Some(bad), ..Default::default() }),
            )
            .await
            .expect_err("an out-of-range tax rate must be refused");
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "rate {bad} must be a 400");
        }

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_on_a_corrupt_stored_match_formula_is_a_500_never_a_silent_no_match() {
        // The §20/#465 rule, now load-bearing for the projection: a stored
        // formula missing `tiers` decodes CLEANLY to "no employer match" under
        // the lenient type. That would UNDERSTATE this user's retirement income
        // with no signal at all. Corrupt JSONB in our own table is our bug.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        upsert_profile(&db, user, &projectable_input(), today()).await.unwrap();
        sqlx::query(
            "UPDATE retirement_profiles SET employer_match_formula = '{\"annual_dollar_cap\": 5000}'::jsonb \
             WHERE user_id = $1",
        )
        .bind(user)
        .execute(&db)
        .await
        .unwrap();

        let err = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .expect_err("a corrupt stored formula must not project");
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);

        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn projection_computes_identically_for_a_non_us_profile_with_social_security() {
        // The region gate is a RENDERING gate (#465), not a compute gate. The
        // engine has no country input; the SS figures are the user's own
        // entered numbers whatever their country, so a French profile projects.
        let db = test_pool().await;
        let user = mk_user(&db).await;
        mk_pro(&db, user).await;
        let input = RetirementProfileInput {
            country: Some("FR".to_string()),
            ss_monthly_benefit: Some(2_000.0),
            ss_benefit_at_age: Some(SsAnchor::Fra),
            ss_claiming_age: Some(SsAnchor::Age(67)),
            ..projectable_input()
        };
        upsert_profile(&db, user, &input, today()).await.unwrap();
        mk_asset(&db, user, "401k", crate::assets::TaxTreatment::PreTax, 250_000.0).await;

        let Json(p) = get_projection_handler(State(state_for(&db)), axum::Extension(user))
            .await
            .expect("a non-US profile must still project");

        let adjusted = p
            .assumptions
            .ss_adjusted_monthly_benefit
            .expect("a profile with SS data must carry an adjusted benefit");
        assert!(adjusted > 0.0, "the SS stream must reach the projection, got {adjusted}");
        assert_eq!(p.assumptions.ss_claiming_age_used, Some(67));
        assert!(
            p.income_sources.social_security > 0.0,
            "SS must appear as its own segment of the income decomposition"
        );

        cleanup(&db, user).await;
    }
}
