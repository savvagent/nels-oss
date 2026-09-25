use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::fmt;
use uuid::Uuid;

use crate::auth::AppState;
use crate::budget::{check_permission, Permission};
use crate::error::internal_error;

#[derive(Debug, PartialEq)]
pub enum PeriodError {
    UnknownPreset,
    InvalidRange,
    BadDate,
    BadGranularity,
}

impl fmt::Display for PeriodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PeriodError::UnknownPreset => {
                "unknown period preset (use this_month|last_month|this_year|all_time)"
            }
            PeriodError::InvalidRange => "invalid range: start must be before end",
            PeriodError::BadDate => "could not parse start/end date (use YYYY-MM-DD or RFC3339)",
            PeriodError::BadGranularity => "unknown granularity (use daily|weekly|monthly)",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Granularity {
    Daily,
    Weekly,
    Monthly,
}

impl Granularity {
    /// Postgres `date_trunc` field name. NOT the API enum string —
    /// `date_trunc('weekly', …)` is invalid Postgres.
    fn pg_field(&self) -> &'static str {
        match self {
            Granularity::Daily => "day",
            Granularity::Weekly => "week",
            Granularity::Monthly => "month",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Granularity::Daily => "daily",
            Granularity::Weekly => "weekly",
            Granularity::Monthly => "monthly",
        }
    }
    /// Public accessor for the API enum string (e.g. "monthly"), used by the
    /// cross-budget insights module which reuses this `Granularity`.
    pub fn label_public(&self) -> &'static str {
        self.label()
    }
    /// Public accessor for the Postgres `date_trunc` field name. Comes from the
    /// fixed enum (never user text), so callers can safely interpolate it.
    pub fn pg_field_public(&self) -> &'static str {
        self.pg_field()
    }
    fn parse(s: &str) -> Option<Granularity> {
        match s {
            "daily" => Some(Granularity::Daily),
            "weekly" => Some(Granularity::Weekly),
            "monthly" => Some(Granularity::Monthly),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ResolvedPeriod {
    pub label: String,
    /// `None` lower bound = all_time (omit the lower-bound SQL predicate).
    pub start: Option<DateTime<Utc>>,
    pub end: DateTime<Utc>,
    pub granularity: Granularity,
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub period: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub granularity: Option<String>,
    pub narrative: Option<bool>,
}

#[derive(Serialize, Debug)]
pub struct PeriodMeta {
    pub label: String,
    pub start: Option<DateTime<Utc>>,
    pub end: DateTime<Utc>,
    pub granularity: String,
}

#[derive(Serialize)]
pub struct CategoryRow {
    pub category: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub spent: f64,
    pub limit: Option<f64>,
    pub pct: Option<f64>,
}

#[derive(Serialize, Default)]
pub struct Summary {
    pub income: f64,
    pub expense: f64,
    pub savings: f64,
    pub net: f64,
}

#[derive(Serialize, Debug)]
pub struct TrendPoint {
    pub bucket: String,
    pub spent: f64,
}

#[derive(Serialize)]
pub struct TopTransaction {
    pub id: Uuid,
    pub description: String,
    pub amount: f64,
    pub category: String,
    pub date: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct ReportResponse {
    pub period: PeriodMeta,
    pub by_category: Vec<CategoryRow>,
    pub summary: Summary,
    pub trend: Vec<TrendPoint>,
    pub top_transactions: Vec<TopTransaction>,
    pub narrative: String,
}

// ---------------------------------------------------------------------------
// Pure period / granularity helpers
// ---------------------------------------------------------------------------

fn first_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).unwrap()
}

fn add_month(d: DateTime<Utc>) -> DateTime<Utc> {
    let (y, m) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap()
}

fn sub_month(d: DateTime<Utc>) -> DateTime<Utc> {
    let (y, m) = if d.month() == 1 {
        (d.year() - 1, 12)
    } else {
        (d.year(), d.month() - 1)
    };
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap()
}

fn first_of_year(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), 1, 1, 0, 0, 0).unwrap()
}

fn parse_bound(s: &str) -> Result<DateTime<Utc>, PeriodError> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()));
    }
    Err(PeriodError::BadDate)
}

fn resolve_gran(
    override_s: Option<&str>,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
) -> Result<Granularity, PeriodError> {
    if let Some(g) = override_s {
        return Granularity::parse(g).ok_or(PeriodError::BadGranularity);
    }
    // all_time (no start) is a long span -> monthly buckets.
    let start = start.unwrap_or(end - Duration::days(3650));
    Ok(pick_granularity(start, end))
}

/// Resolve query params into a concrete half-open UTC range [start, end).
/// `now` is injected so the function is deterministic and testable.
pub fn resolve_period(
    period: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    granularity_override: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ResolvedPeriod, PeriodError> {
    // Custom range wins when both bounds are present.
    if let (Some(s), Some(e)) = (start, end) {
        let sd = parse_bound(s)?;
        let ed = parse_bound(e)?;
        if sd >= ed {
            return Err(PeriodError::InvalidRange);
        }
        let gran = resolve_gran(granularity_override, Some(sd), ed)?;
        return Ok(ResolvedPeriod {
            label: "custom".to_string(),
            start: Some(sd),
            end: ed,
            granularity: gran,
        });
    }

    let preset = period.unwrap_or("this_month");
    let (start_opt, end_dt) = match preset {
        "this_month" => {
            let s = first_of_month(now);
            (Some(s), add_month(s))
        }
        "last_month" => {
            let cur = first_of_month(now);
            (Some(sub_month(cur)), cur)
        }
        "this_year" => {
            let s = first_of_year(now);
            (
                Some(s),
                Utc.with_ymd_and_hms(now.year() + 1, 1, 1, 0, 0, 0).unwrap(),
            )
        }
        "all_time" => (None, now),
        _ => return Err(PeriodError::UnknownPreset),
    };
    let gran = resolve_gran(granularity_override, start_opt, end_dt)?;
    Ok(ResolvedPeriod {
        label: preset.to_string(),
        start: start_opt,
        end: end_dt,
        granularity: gran,
    })
}

/// Derive the default trend bucket from the span length.
pub fn pick_granularity(start: DateTime<Utc>, end: DateTime<Utc>) -> Granularity {
    let days = (end - start).num_days();
    if days <= 31 {
        Granularity::Daily
    } else if days <= 182 {
        Granularity::Weekly
    } else {
        Granularity::Monthly
    }
}

// ---------------------------------------------------------------------------
// SQL aggregations (transactions ⋈ categories)
// ---------------------------------------------------------------------------

pub async fn spending_by_category(
    db: &PgPool,
    budget_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
) -> Result<Vec<CategoryRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT c.name AS name, c.category_limit AS category_limit, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM categories c \
         LEFT JOIN transactions t \
           ON t.category_id = c.id \
          AND NOT t.excluded_from_budget \
          AND t.transaction_date < $3 \
          AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         WHERE c.budget_id = $1 AND c.category_type = 'expense' \
         GROUP BY c.id, c.name, c.category_limit \
         ORDER BY spent DESC",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    let mut out = Vec::new();
    for r in rows {
        let spent: f64 = r.get("spent");
        let limit: Option<f64> = r.get("category_limit");
        let pct = match limit {
            Some(l) if l > 0.0 => Some(spent / l),
            _ => None,
        };
        out.push(CategoryRow {
            category: r.get("name"),
            type_: "expense".to_string(),
            spent,
            limit,
            pct,
        });
    }
    Ok(out)
}

pub async fn type_summary(
    db: &PgPool,
    budget_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
) -> Result<Summary, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT c.category_type AS category_type, \
                COALESCE(SUM(t.amount), 0)::float8 AS total \
         FROM transactions t \
         JOIN categories c ON t.category_id = c.id \
         WHERE t.budget_id = $1 \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date < $3 \
           AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         GROUP BY c.category_type",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    let mut s = Summary::default();
    for r in rows {
        let t: String = r.get("category_type");
        let total: f64 = r.get("total");
        match t.as_str() {
            "income" => s.income = total,
            "expense" => s.expense = total,
            "savings" => s.savings = total,
            _ => {}
        }
    }
    s.net = s.income - s.expense - s.savings;
    Ok(s)
}

pub async fn spending_trend(
    db: &PgPool,
    budget_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    gran: Granularity,
) -> Result<Vec<TrendPoint>, sqlx::Error> {
    // gran.pg_field() comes from the fixed enum (never user text) so
    // interpolation here is injection-safe.
    let sql = format!(
        "SELECT date_trunc('{}', t.transaction_date) AS bucket, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM transactions t \
         JOIN categories c ON t.category_id = c.id \
         WHERE t.budget_id = $1 AND c.category_type = 'expense' \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date < $3 \
           AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         GROUP BY bucket ORDER BY bucket",
        gran.pg_field()
    );
    let rows = sqlx::query(&sql)
        .bind(budget_id)
        .bind(start)
        .bind(end)
        .fetch_all(db)
        .await?;

    let mut out = Vec::new();
    for r in rows {
        let bucket: DateTime<Utc> = r.get("bucket");
        out.push(TrendPoint {
            bucket: bucket.format("%Y-%m-%d").to_string(),
            spent: r.get("spent"),
        });
    }
    Ok(out)
}

pub async fn top_transactions(
    db: &PgPool,
    budget_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
) -> Result<Vec<TopTransaction>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT t.id AS id, t.description AS description, t.amount::float8 AS amount, \
                c.name AS category, t.transaction_date AS transaction_date \
         FROM transactions t \
         JOIN categories c ON t.category_id = c.id \
         WHERE t.budget_id = $1 AND c.category_type = 'expense' \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date < $3 \
           AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         ORDER BY t.amount DESC LIMIT 5",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    let mut out = Vec::new();
    for r in rows {
        out.push(TopTransaction {
            id: r.get("id"),
            description: r.get("description"),
            amount: r.get("amount"),
            category: r.get("category"),
            date: r.get("transaction_date"),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Narrative: deterministic template + optional LLM call (resolved provider)
// ---------------------------------------------------------------------------

pub fn template_narrative(s: &Summary, by_category: &[CategoryRow], label: &str) -> String {
    if by_category.is_empty() && s.income == 0.0 && s.expense == 0.0 && s.savings == 0.0 {
        return format!("No activity recorded for {}.", label);
    }
    let mut msg = format!(
        "Over {} you took in ${:.2}, spent ${:.2}, and set aside ${:.2} (net ${:.2}).",
        label, s.income, s.expense, s.savings, s.net
    );
    if let Some(c) = by_category
        .iter()
        .find(|c| c.pct.map(|p| p >= 0.8).unwrap_or(false))
    {
        msg.push_str(&format!(
            " {} is at {:.0}% of its limit.",
            c.category,
            c.pct.unwrap() * 100.0
        ));
    }
    msg
}

/// Returns an AI narrative when `use_ai` and the caller's resolved provider allow
/// it; otherwise (or on any failure) the deterministic template. Recorded in
/// llm_usage as call_type 'narrative' (nels-oss#3; previously unrecorded).
pub async fn generate_narrative(
    db: &sqlx::PgPool,
    user_id: uuid::Uuid,
    ai: &crate::llm::Resolved,
    s: &Summary,
    by_category: &[CategoryRow],
    label: &str,
    use_ai: bool,
) -> String {
    let template = template_narrative(s, by_category, label);
    let creds = match (use_ai, ai.creds()) {
        (true, Some(c)) => c,
        _ => return template,
    };
    let prompt = narrative_prompt(&template);
    // #283: bounded by a 20s timeout so a stalled provider connection cannot hang
    // the report handler; any failure falls back to the template.
    crate::llm::generate_text_for(db, user_id, creds, "narrative", &prompt, std::time::Duration::from_secs(20))
        .await
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or(template)
}

fn narrative_prompt(facts: &str) -> String {
    format!(
        "You are a friendly budgeting assistant. In 2-3 short sentences, give an \
         encouraging, concrete insight or recommendation based ONLY on these figures. \
         Do not invent numbers.\n\nFIGURES: {}",
        facts
    )
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn get_report_handler(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<ReportResponse>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let resolved = resolve_period(
        q.period.as_deref(),
        q.start.as_deref(),
        q.end.as_deref(),
        q.granularity.as_deref(),
        Utc::now(),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let by_category = spending_by_category(&state.db, budget_id, resolved.start, resolved.end)
        .await
        .map_err(internal_error)?;
    let summary = type_summary(&state.db, budget_id, resolved.start, resolved.end)
        .await
        .map_err(internal_error)?;
    let trend = spending_trend(
        &state.db,
        budget_id,
        resolved.start,
        resolved.end,
        resolved.granularity,
    )
    .await
    .map_err(internal_error)?;
    let top = top_transactions(&state.db, budget_id, resolved.start, resolved.end)
        .await
        .map_err(internal_error)?;

    let use_ai = q.narrative.unwrap_or(true);
    // Resolve the caller's provider only when an AI narrative was requested.
    // Named `ai` — `resolved` above is the report period.
    let ai = if use_ai {
        crate::llm::resolve_for_user(&state.db, &state.cipher, user_id)
            .await
            .unwrap_or(crate::llm::Resolved::Offline)
    } else {
        crate::llm::Resolved::Offline
    };
    let narrative = generate_narrative(
        &state.db,
        user_id,
        &ai,
        &summary,
        &by_category,
        &resolved.label,
        use_ai,
    )
    .await;

    Ok(Json(ReportResponse {
        period: PeriodMeta {
            label: resolved.label,
            start: resolved.start,
            end: resolved.end,
            granularity: resolved.granularity.label().to_string(),
        },
        by_category,
        summary,
        trend,
        top_transactions: top,
        narrative,
    }))
}

// ---------------------------------------------------------------------------
// Unit tests (pure helpers only — no DB)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn this_month_range() {
        let r = resolve_period(Some("this_month"), None, None, None, at(2026, 3, 14)).unwrap();
        assert_eq!(r.label, "this_month");
        assert_eq!(
            r.start.unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(r.end, Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn last_month_crosses_year_boundary() {
        let r = resolve_period(Some("last_month"), None, None, None, at(2026, 1, 9)).unwrap();
        assert_eq!(
            r.start.unwrap(),
            Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(r.end, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn this_year_range() {
        let r = resolve_period(Some("this_year"), None, None, None, at(2026, 7, 1)).unwrap();
        assert_eq!(
            r.start.unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(r.end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn all_time_has_no_lower_bound() {
        let now = at(2026, 7, 1);
        let r = resolve_period(Some("all_time"), None, None, None, now).unwrap();
        assert!(r.start.is_none());
        assert_eq!(r.end, now);
    }

    #[test]
    fn custom_range_overrides_preset() {
        let r = resolve_period(
            Some("this_month"),
            Some("2026-05-01"),
            Some("2026-06-01"),
            None,
            at(2026, 7, 1),
        )
        .unwrap();
        assert_eq!(r.label, "custom");
        assert_eq!(
            r.start.unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(r.end, Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn inverted_custom_range_errors() {
        let r = resolve_period(
            None,
            Some("2026-06-01"),
            Some("2026-05-01"),
            None,
            at(2026, 7, 1),
        );
        assert_eq!(r.unwrap_err(), PeriodError::InvalidRange);
    }

    #[test]
    fn unknown_preset_errors() {
        let r = resolve_period(Some("yesterday"), None, None, None, at(2026, 7, 1));
        assert_eq!(r.unwrap_err(), PeriodError::UnknownPreset);
    }

    #[test]
    fn bad_date_errors() {
        let r = resolve_period(
            None,
            Some("not-a-date"),
            Some("2026-06-01"),
            None,
            at(2026, 7, 1),
        );
        assert_eq!(r.unwrap_err(), PeriodError::BadDate);
    }

    #[test]
    fn bad_granularity_errors() {
        let r = resolve_period(
            Some("this_month"),
            None,
            None,
            Some("hourly"),
            at(2026, 7, 1),
        );
        assert_eq!(r.unwrap_err(), PeriodError::BadGranularity);
    }

    #[test]
    fn granularity_daily_under_31_days() {
        let s = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        assert_eq!(pick_granularity(s, s + Duration::days(31)), Granularity::Daily);
    }

    #[test]
    fn granularity_weekly_between_32_and_182() {
        let s = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        assert_eq!(
            pick_granularity(s, s + Duration::days(32)),
            Granularity::Weekly
        );
        assert_eq!(
            pick_granularity(s, s + Duration::days(182)),
            Granularity::Weekly
        );
    }

    #[test]
    fn granularity_monthly_over_182_days() {
        let s = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            pick_granularity(s, s + Duration::days(200)),
            Granularity::Monthly
        );
    }

    fn cat(name: &str, spent: f64, limit: Option<f64>) -> CategoryRow {
        let pct = match limit {
            Some(l) if l > 0.0 => Some(spent / l),
            _ => None,
        };
        CategoryRow {
            category: name.to_string(),
            type_: "expense".to_string(),
            spent,
            limit,
            pct,
        }
    }

    #[test]
    fn template_reports_no_activity_when_empty() {
        let s = Summary::default();
        let msg = template_narrative(&s, &[], "last_month");
        assert_eq!(msg, "No activity recorded for last_month.");
    }

    #[test]
    fn template_includes_totals_and_flags_over_80pct() {
        let s = Summary {
            income: 3000.0,
            expense: 1850.0,
            savings: 400.0,
            net: 750.0,
        };
        let cats = vec![cat("Food", 420.0, Some(500.0))]; // 84%
        let msg = template_narrative(&s, &cats, "last_month");
        assert!(msg.contains("$1850.00"));
        assert!(msg.contains("Food is at 84% of its limit"));
    }

    // #283: proves the narrative call's #249-class hang is bounded by a real 20s timeout —
    // not just that a Duration was passed to Client::builder(). A local mock Gemini endpoint
    // (via GEMINI_API_BASE) sleeps past the configured timeout before responding; the call
    // must fall back to the exact template_narrative output well before the mock ever answers.
    // nels-oss#3: generate_narrative now takes a resolved provider and a pool; the failure
    // path for a Nels-sourced credential touches no DB (usage is recorded only on success,
    // and `observe` is a no-op for Nels keys), so a lazy, never-connected pool suffices and
    // this test still runs in the default `cargo test`. GEMINI_API_BASE is test-only (never
    // legitimately set outside a test run), so unconditional removal on drop matches rag.rs's
    // own GeminiApiBaseGuard precedent.
    struct GeminiEnvGuard;
    impl Drop for GeminiEnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("GEMINI_API_BASE");
        }
    }

    // Dropping a tokio::JoinHandle detaches rather than cancels the task, so an early panic
    // (e.g. a scheduler-jitter-induced assertion failure below) would otherwise leave the mock
    // server running for the rest of the test binary process. Aborting it via a Drop guard
    // makes cleanup panic-safe, matching GeminiEnvGuard's own rationale for the env vars.
    struct AbortOnDrop(tokio::task::JoinHandle<()>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    #[tokio::test]
    async fn narrative_falls_back_to_template_on_timeout() {
        let _env = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _mock_handle = AbortOnDrop(tokio::spawn(async move {
            let mock = axum::Router::new().fallback(|| async {
                tokio::time::sleep(std::time::Duration::from_secs(25)).await;
                axum::Json(serde_json::json!({}))
            });
            if let Err(e) = axum::serve(listener, mock).await {
                eprintln!("mock Gemini server error: {e}");
            }
        }));
        let _guard = GeminiEnvGuard;
        std::env::set_var("GEMINI_API_BASE", format!("http://{addr}"));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")
            .expect("lazy pool");
        let ai = crate::llm::Resolved::Llm(crate::llm::LlmCredentials::new(
            crate::llm::Provider::Gemini,
            "dummy".into(),
            crate::llm::KeySource::Nels,
        ));

        let s = Summary {
            income: 1000.0,
            expense: 400.0,
            savings: 100.0,
            net: 500.0,
        };
        let cats: Vec<CategoryRow> = vec![];
        let expected_template = template_narrative(&s, &cats, "this_month");

        let started = std::time::Instant::now();
        let narrative = generate_narrative(&pool, uuid::Uuid::new_v4(), &ai, &s, &cats, "this_month", true).await;
        let elapsed = started.elapsed();

        assert_eq!(
            narrative, expected_template,
            "a timed-out Gemini call must fall back to the template narrative"
        );
        // Lower bound proves the real ~20s timeout elapsed (not an instant connection error).
        // Upper bound is the actual discriminator between fixed/unfixed: with NO client
        // timeout, the call is bounded only by the mock's own 25s sleep and elapsed would land
        // at ~25s+, failing this assertion.
        assert!(
            elapsed >= std::time::Duration::from_secs(19)
                && elapsed < std::time::Duration::from_secs(23),
            "expected the 20s timeout to fire (19s <= elapsed < 23s), got {elapsed:?}"
        );
    }

    // nels-oss#3: a BYO Anthropic narrative goes through the resolved provider, is
    // recorded in llm_usage as call_type='narrative' / key_source='byo', and an auth
    // failure degrades to the template (never an error, never the Nels key).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn narrative_uses_resolved_provider_records_usage_and_falls_back() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(v) => std::env::set_var("ANTHROPIC_API_BASE", v),
                    None => std::env::remove_var("ANTHROPIC_API_BASE"),
                }
            }
        }
        let _restore = Restore(std::env::var("ANTHROPIC_API_BASE").ok());
        let server = wiremock::MockServer::start().await;
        std::env::set_var("ANTHROPIC_API_BASE", server.uri());
        wiremock::Mock::given(wiremock::matchers::header("x-api-key", "good"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "Nice work this month."}],
                "usage": {"input_tokens": 3, "output_tokens": 4}
            })))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::header("x-api-key", "bad"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let db = sqlx::PgPool::connect(&url).await.unwrap();
        let uid = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1,$2)")
            .bind(uid)
            .bind(format!("nar-{uid}@t.example"))
            .execute(&db)
            .await
            .unwrap();
        let s = Summary { income: 100.0, expense: 50.0, savings: 10.0, net: 40.0 };
        let ok = crate::llm::Resolved::Llm(crate::llm::LlmCredentials::new(
            crate::llm::Provider::Anthropic,
            "good".into(),
            crate::llm::KeySource::Byo,
        ));
        let out = generate_narrative(&db, uid, &ok, &s, &[], "this_month", true).await;
        assert_eq!(out, "Nice work this month.");
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM llm_usage WHERE user_id=$1 AND call_type='narrative' AND key_source='byo'",
        )
        .bind(uid)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(n, 1);
        let bad = crate::llm::Resolved::Llm(crate::llm::LlmCredentials::new(
            crate::llm::Provider::Anthropic,
            "bad".into(),
            crate::llm::KeySource::Byo,
        ));
        let out = generate_narrative(&db, uid, &bad, &s, &[], "this_month", true).await;
        assert_eq!(out, template_narrative(&s, &[], "this_month"));
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(uid).execute(&db).await;
    }
}

#[cfg(test)]
mod exclude_tests {
    use super::*;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        PgPool::connect(&url).await.expect("connect to test db")
    }

    async fn seed_user(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(id).bind(format!("excl-{id}@example.test")).execute(pool).await.expect("seed user");
        id
    }

    // Monthly budget, one expense + one income category, one $100 expense tx and
    // one $500 income tx dated now. Returns (budget_id, expense_tx_id).
    async fn seed_budget(pool: &PgPool, owner: Uuid) -> (Uuid, Uuid) {
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) VALUES ($1,$2,'Excl','monthly','time_based')")
            .bind(bid).bind(owner).execute(pool).await.unwrap();
        let exp_cat = Uuid::new_v4();
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type) VALUES ($1,$2,'Food','expense')")
            .bind(exp_cat).bind(bid).execute(pool).await.unwrap();
        let inc_cat = Uuid::new_v4();
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type) VALUES ($1,$2,'Pay','income')")
            .bind(inc_cat).bind(bid).execute(pool).await.unwrap();
        let exp_tx = Uuid::new_v4();
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1,$2,$3,100.0,'card payoff', now())")
            .bind(exp_tx).bind(bid).bind(exp_cat).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1,$2,$3,500.0,'salary', now())")
            .bind(Uuid::new_v4()).bind(bid).bind(inc_cat).execute(pool).await.unwrap();
        (bid, exp_tx)
    }

    async fn cleanup(pool: &PgPool, user: Uuid) {
        sqlx::query("DELETE FROM budgets WHERE owner_id = $1").bind(user).execute(pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(pool).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn excluded_transaction_omitted_from_expense_total_and_unexclude_restores() {
        let pool = test_pool().await;
        let user = seed_user(&pool).await;
        let (bid, exp_tx) = seed_budget(&pool, user).await;
        let end = chrono::Utc::now() + chrono::Duration::days(1);

        let s0 = type_summary(&pool, bid, None, end).await.unwrap();
        assert!((s0.expense - 100.0).abs() < 1e-6);
        assert!((s0.income - 500.0).abs() < 1e-6);

        sqlx::query("UPDATE transactions SET excluded_from_budget = true WHERE id = $1")
            .bind(exp_tx).execute(&pool).await.unwrap();
        let s1 = type_summary(&pool, bid, None, end).await.unwrap();
        assert!(s1.expense.abs() < 1e-6, "excluded expense removed");
        assert!((s1.income - 500.0).abs() < 1e-6, "income unaffected");

        sqlx::query("UPDATE transactions SET excluded_from_budget = false WHERE id = $1")
            .bind(exp_tx).execute(&pool).await.unwrap();
        let s2 = type_summary(&pool, bid, None, end).await.unwrap();
        assert!((s2.expense - 100.0).abs() < 1e-6, "unexclude restores expense");

        cleanup(&pool, user).await;
    }

    // Two expenses in the SAME category ($100 + $30); exclude only the $100 one.
    // The expense total must be exactly 30 — proving exclusion is per-row (not
    // per-group, which would zero the whole category, and not inverted, which
    // would leave 130 or drop the $30 instead).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn partial_exclusion_removes_only_the_excluded_transaction() {
        let pool = test_pool().await;
        let user = seed_user(&pool).await;
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) VALUES ($1,$2,'PartExcl','monthly','time_based')")
            .bind(bid).bind(user).execute(&pool).await.unwrap();
        let cat = Uuid::new_v4();
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type) VALUES ($1,$2,'Food','expense')")
            .bind(cat).bind(bid).execute(&pool).await.unwrap();
        let big_tx = Uuid::new_v4();
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1,$2,$3,100.0,'big grocery run', now())")
            .bind(big_tx).bind(bid).bind(cat).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1,$2,$3,30.0,'small snack', now())")
            .bind(Uuid::new_v4()).bind(bid).bind(cat).execute(&pool).await.unwrap();
        let end = chrono::Utc::now() + chrono::Duration::days(1);

        let s0 = type_summary(&pool, bid, None, end).await.unwrap();
        assert!((s0.expense - 130.0).abs() < 1e-6, "both expenses counted before exclusion");

        sqlx::query("UPDATE transactions SET excluded_from_budget = true WHERE id = $1")
            .bind(big_tx).execute(&pool).await.unwrap();
        let s1 = type_summary(&pool, bid, None, end).await.unwrap();
        assert!(
            (s1.expense - 30.0).abs() < 1e-6,
            "only the $100 tx is excluded; the $30 remains — got {}",
            s1.expense
        );

        cleanup(&pool, user).await;
    }
}
