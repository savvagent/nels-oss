//! Retirement profile & assumptions (nels#465, part of the #454 epic).

use axum::{extract::State, http::StatusCode, Json};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::internal_error;

// Pure domain rules live in nels-core (core/, spec savvagent/nels-oss#5).
// Re-exported so existing `crate::retirement::…` paths keep resolving.
// Add NEW pure rules to core/, not here.
pub use nels_core::retirement::{
    resolve_input, retirement_supported_country,
    social_security_view, stale_defaulted_age_warning, validate_profile, EmployerMatch,
    RetirementProfile, RetirementProfileInput, SocialSecurityView, SsAnchor,
    StoredEmployerMatch, MISSING_REQUIRED_FIELD_SUFFIX, NO_PROFILE_MESSAGE,
    SS_NEEDS_QUOTED_AGE,
};
// Only the backend's tests still name these through `crate::retirement::…`.
#[cfg(test)]
pub use nels_core::retirement::{
    default_employer_match, MatchTier, DEFAULT_EXPECTED_REAL_RETURN, DEFAULT_INFLATION_RATE,
    DEFAULT_LIFE_EXPECTANCY_AGE, DEFAULT_TARGET_REPLACEMENT_RATIO,
    DEFAULT_TARGET_RETIREMENT_AGE, SS_ANCHOR_EXPLICIT, SS_ANCHOR_FRA, SS_SOURCE_USER_ENTERED,
};

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
    use chrono::DateTime;

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
    /// `require_caller_tier` reads `price_catalog_from_env()`, and
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
