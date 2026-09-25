//! Account-level, owner-scoped budget & spending analytics (#57).
//!
//! This module powers the frontend "Insights" page. It is a READ-ONLY,
//! cross-budget view of a single user's finances — distinct from the per-budget
//! `reports.rs` report (`/budgets/:id/report`), which is scoped to one budget and
//! gated by `check_permission`. Insights aggregates across every budget the user
//! **owns** (`budgets.owner_id = user_id`), plus the single budget they have
//! explicitly made active (`users.active_budget_id`, #255) when that budget is
//! shared with them (#391) — otherwise a sharee working in a shared budget would
//! see an empty status strip. Other budgets merely shared with the user remain
//! excluded, so their KPI totals stay "my data plus the one budget I chose to
//! work in".
//!
//! Note the deliberate asymmetry: the trend and top-category aggregations below
//! remain strictly owner-scoped (`budgets.owner_id = $1`). Widening those raises
//! separate cross-tenant questions and is out of scope for #391 — so with a
//! shared budget active, the modal lists it while those two panels omit its
//! history.
//!
//! Reuse, not duplication: period/granularity resolution comes from `reports`
//! (`resolve_period`, `Granularity`, `PeriodMeta`); the per-budget effective
//! budget and current-period spend reuse the computed-on-read helpers in
//! `budget` (`computed_budget_totals`, `period_expense_spent_many`,
//! `current_period_window`, `previous_period_window`, `effective_carried`). The
//! trend and top-category aggregations are done in SQL (joined through
//! `budgets.owner_id`), avoiding N+1 round-trips.

use axum::{extract::Query, extract::State, http::StatusCode, Extension, Json};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::auth::AppState;
use crate::budget::{
    computed_budget_totals, current_period_window, effective_carried, period_expense_spent_many,
    previous_period_window, project_span,
};
use crate::error::internal_error;
use crate::reports::{resolve_period, Granularity, PeriodMeta, ReportQuery, TrendPoint};

/// Current-period headline figures, summed across the user's active (non-archived)
/// owned budgets.
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct Kpis {
    /// Sum of each active budget's EFFECTIVE amount (base + rollover carry).
    pub total_budgeted: f64,
    /// Sum of each active budget's current-period expense spend.
    pub total_spent: f64,
    /// `total_budgeted - total_spent` (may be negative if collectively overspent).
    pub total_remaining: f64,
    /// How many active budgets are individually overspent this period.
    pub overspent_count: i64,
    /// How many active budgets the user owns.
    pub budget_count: i64,
}

/// Per-budget spend-vs-budget for the current period.
#[derive(Serialize, Debug, PartialEq)]
pub struct BudgetSpend {
    pub id: Uuid,
    pub name: String,
    /// Effective budget (base + carry).
    pub budgeted: f64,
    pub spent: f64,
    pub remaining: f64,
    pub overspent: bool,
    /// Fraction of the effective budget used. `None` when the budget is 0 (no
    /// meaningful percentage). Can exceed 1.0 when overspent.
    pub pct: Option<f64>,
}

/// One row of the top-spending-categories comparison, aggregated across all
/// owned budgets within the resolved period.
#[derive(Serialize, Debug, PartialEq)]
pub struct CategoryAgg {
    pub category: String,
    pub spent: f64,
}

#[derive(Serialize, Debug)]
pub struct InsightsResponse {
    pub period: PeriodMeta,
    pub kpis: Kpis,
    pub trend: Vec<TrendPoint>,
    pub top_categories: Vec<CategoryAgg>,
    pub budgets: Vec<BudgetSpend>,
    pub recommendations: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure recommendation rules (framed as guidance, per the issue AC)
// ---------------------------------------------------------------------------

/// Build a short, deterministic list of guidance-framed recommendations from the
/// already-computed figures. Pure (no DB / no clock / no LLM) so it is fully
/// unit-tested and the dashboard load stays fast and offline-safe. The issue
/// notes an LLM could enrich these later; the wording here is intentionally
/// advisory ("Consider…", "You're…"), never imperative or alarming.
///
/// At most 4 lines, ordered most-actionable first.
pub fn build_recommendations(
    kpis: &Kpis,
    budgets: &[BudgetSpend],
    top_categories: &[CategoryAgg],
) -> Vec<String> {
    // No activity at all -> a single onboarding nudge.
    if kpis.budget_count == 0 {
        return vec![
            "Create your first budget to start tracking spending and unlock insights.".to_string(),
        ];
    }
    if kpis.total_spent == 0.0 && kpis.total_budgeted == 0.0 {
        return vec![
            "No spending or budget amounts recorded yet this period — add category limits and \
             transactions to see trends here."
                .to_string(),
        ];
    }

    let mut out: Vec<String> = Vec::new();

    // 1. Overspent budgets are the most urgent signal.
    let overspent: Vec<&BudgetSpend> = budgets.iter().filter(|b| b.overspent).collect();
    if !overspent.is_empty() {
        if overspent.len() == 1 {
            out.push(format!(
                "Consider reviewing \"{}\" — it is over budget by ${:.2} this period.",
                overspent[0].name,
                overspent[0].spent - overspent[0].budgeted
            ));
        } else {
            out.push(format!(
                "Consider reviewing {} budgets that are over their limit this period.",
                overspent.len()
            ));
        }
    }

    // 2. Near-limit budgets (>=80% used but not yet overspent) are an early warning.
    if let Some(near) = budgets
        .iter()
        .filter(|b| !b.overspent)
        .filter(|b| b.pct.map(|p| p >= 0.8).unwrap_or(false))
        .max_by(|a, b| a.pct.unwrap().partial_cmp(&b.pct.unwrap()).unwrap())
    {
        out.push(format!(
            "You're at {:.0}% of your \"{}\" budget — pace your remaining spending to stay on track.",
            near.pct.unwrap() * 100.0,
            near.name
        ));
    }

    // 3. Highlight the biggest spending category for awareness.
    if let Some(top) = top_categories.first() {
        if top.spent > 0.0 {
            out.push(format!(
                "Your largest spending category is \"{}\" at ${:.2}. Review it for savings opportunities.",
                top.category, top.spent
            ));
        }
    }

    // 4. Positive reinforcement when collectively under budget and nothing overspent.
    if overspent.is_empty() && kpis.total_remaining > 0.0 {
        out.push(format!(
            "Nice work — you're ${:.2} under budget across your budgets this period.",
            kpis.total_remaining
        ));
    }

    out.truncate(4);
    out
}

// ---------------------------------------------------------------------------
// Owner-scoped SQL aggregations (expense-only, aggregated in SQL — no N+1)
// ---------------------------------------------------------------------------

/// Spend over time, bucketed by `gran`, summed across EVERY budget the user owns
/// (expense categories only) within the resolved period. Aggregated in a single
/// SQL query joined through `budgets.owner_id` so other users' data can never be
/// returned.
pub async fn owner_spending_trend(
    db: &PgPool,
    user_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    gran: Granularity,
) -> Result<Vec<TrendPoint>, sqlx::Error> {
    // gran.pg_field() comes from the fixed enum (never user text) so the format!
    // interpolation here is injection-safe — same pattern as reports::spending_trend.
    let sql = format!(
        "SELECT date_trunc('{}', t.transaction_date) AS bucket, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM transactions t \
         JOIN categories c ON t.category_id = c.id \
         JOIN budgets b ON t.budget_id = b.id \
         WHERE b.owner_id = $1 AND c.category_type = 'expense' \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date < $3 \
           AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         GROUP BY bucket ORDER BY bucket",
        gran.pg_field_public()
    );
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .bind(start)
        .bind(end)
        .fetch_all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let bucket: DateTime<Utc> = r.get("bucket");
            TrendPoint {
                bucket: bucket.format("%Y-%m-%d").to_string(),
                spent: r.get("spent"),
            }
        })
        .collect())
}

/// Top expense categories by spend across all owned budgets within the period.
/// Categories are grouped by NAME so the same conceptual category across budgets
/// (e.g. "Groceries" in two budgets) is compared as one. Owner-scoped via the
/// `budgets.owner_id` join.
pub async fn owner_top_categories(
    db: &PgPool,
    user_id: Uuid,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<CategoryAgg>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT c.name AS name, COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM transactions t \
         JOIN categories c ON t.category_id = c.id \
         JOIN budgets b ON t.budget_id = b.id \
         WHERE b.owner_id = $1 AND c.category_type = 'expense' \
           AND NOT t.excluded_from_budget \
           AND t.transaction_date < $3 \
           AND ($2::timestamptz IS NULL OR t.transaction_date >= $2) \
         GROUP BY c.name HAVING COALESCE(SUM(t.amount), 0) > 0 \
         ORDER BY spent DESC LIMIT $4",
    )
    .bind(user_id)
    .bind(start)
    .bind(end)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| CategoryAgg {
            category: r.get("name"),
            spent: r.get("spent"),
        })
        .collect())
}

/// One owned, active (non-archived) budget row needed for the KPI / spend-vs-budget
/// rollup.
struct OwnedBudgetRow {
    id: Uuid,
    name: String,
    time_frame: String,
    budget_type: String,
    rollover_enabled: bool,
    created_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
}

/// Fetch the budgets that feed this user's insights: their own non-archived
/// budgets, plus — at most one extra row — the budget they have explicitly made
/// active (`users.active_budget_id`, #255) when that budget is shared with them
/// and non-archived (#391).
///
/// Deliberately NOT "every budget shared with me": this row set feeds
/// `budget_rollup`'s KPI aggregates, so a full union would fold everyone else's
/// budgets into the viewer's totals. The `archived_at IS NULL` filter is applied
/// to the shared row too — `list_budgets`' own share join does not filter archived.
async fn owned_active_budgets(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<OwnedBudgetRow>, (StatusCode, String)> {
    let rows = sqlx::query(
        "SELECT b.id, b.name, b.time_frame, b.budget_type, b.rollover_enabled, \
                b.created_at, b.closed_at \
         FROM budgets b \
         WHERE b.archived_at IS NULL \
           AND ( \
             b.owner_id = $1 \
             OR ( \
               b.id = (SELECT u.active_budget_id FROM users u WHERE u.id = $1) \
               AND EXISTS ( \
                 SELECT 1 FROM budget_shares bs \
                 JOIN users u2 ON u2.id = $1 \
                 WHERE bs.budget_id = b.id AND bs.shared_with_email = u2.email \
               ) \
             ) \
           ) \
         ORDER BY b.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .map_err(internal_error)?;

    Ok(rows
        .into_iter()
        .map(|r| OwnedBudgetRow {
            id: r.get("id"),
            name: r.get("name"),
            time_frame: r.get("time_frame"),
            budget_type: r.get("budget_type"),
            rollover_enabled: r.get("rollover_enabled"),
            created_at: r.get("created_at"),
            closed_at: r.get("closed_at"),
        })
        .collect())
}

/// Compute the current-period KPIs + per-budget spend-vs-budget for the user's
/// active owned budgets, reusing the computed-on-read helpers from `budget`.
/// `now` is injected for deterministic testing.
///
/// Effective budget = base (sum of expense `category_limit`s, via
/// `computed_budget_totals`) + rollover carry (`effective_carried` over the
/// PREVIOUS-period window). Spend = expense spend over each budget's spend window
/// (`period_expense_spent_many`): a time-based budget uses its CURRENT calendar
/// period (`current_period_window`); a project budget uses its full lifetime span
/// (`project_span`, `created_at -> closed_at`/now). This mirrors
/// `budget::aggregated_budget_amounts`'s per-member logic exactly, so Insights
/// cannot diverge from the standalone per-budget view.
pub async fn budget_rollup(
    db: &PgPool,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(Kpis, Vec<BudgetSpend>), (StatusCode, String)> {
    let budgets = owned_active_budgets(db, user_id).await?;

    let ids: Vec<Uuid> = budgets.iter().map(|b| b.id).collect();
    let totals = computed_budget_totals(db, &ids).await?;

    // Previous-period windows feed the rollover carry; current-period windows feed
    // the spend figure. Each budget's window depends on its own time_frame.
    let prev_windows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>)> = budgets
        .iter()
        .map(|b| {
            let (s, e) = previous_period_window(&b.time_frame, now);
            (b.id, s, e)
        })
        .collect();
    // Spend window per budget: project budgets span their whole lifetime; all
    // others use the current calendar period (matches aggregated_budget_amounts).
    let cur_windows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>)> = budgets
        .iter()
        .map(|b| {
            let (s, e) = if b.budget_type == "project" {
                project_span(b.created_at, b.closed_at, now)
            } else {
                current_period_window(&b.time_frame, now)
            };
            (b.id, s, e)
        })
        .collect();

    let prev_spends = period_expense_spent_many(db, &prev_windows).await?;
    let cur_spends = period_expense_spent_many(db, &cur_windows).await?;

    let mut kpis = Kpis {
        budget_count: budgets.len() as i64,
        ..Default::default()
    };
    let mut out: Vec<BudgetSpend> = Vec::with_capacity(budgets.len());

    for b in &budgets {
        let base = totals.get(&b.id).copied().unwrap_or(0.0);
        let prev_spent = prev_spends.get(&b.id).copied().unwrap_or(0.0);
        let carried = effective_carried(&b.budget_type, b.rollover_enabled, base, prev_spent);
        let budgeted = base + carried;
        let spent = cur_spends.get(&b.id).copied().unwrap_or(0.0);
        let remaining = budgeted - spent;
        let overspent = spent > budgeted && budgeted > 0.0;
        let pct = if budgeted > 0.0 {
            Some(spent / budgeted)
        } else {
            None
        };

        kpis.total_budgeted += budgeted;
        kpis.total_spent += spent;
        if overspent {
            kpis.overspent_count += 1;
        }

        out.push(BudgetSpend {
            id: b.id,
            name: b.name.clone(),
            budgeted,
            spent,
            remaining,
            overspent,
            pct,
        });
    }

    kpis.total_remaining = kpis.total_budgeted - kpis.total_spent;
    // Most-overspent / highest-spend first so the UI leads with what matters.
    out.sort_by(|a, b| {
        b.spent
            .partial_cmp(&a.spent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok((kpis, out))
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn insights_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<InsightsResponse>, (StatusCode, String)> {
    let now = Utc::now();
    let resolved = resolve_period(
        q.period.as_deref(),
        q.start.as_deref(),
        q.end.as_deref(),
        q.granularity.as_deref(),
        now,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // KPI / per-budget rollup is always CURRENT-period (a dashboard of where the
    // user stands now), independent of the trend/category period selector.
    let (kpis, budgets) = budget_rollup(&state.db, user_id, now).await?;

    let trend = owner_spending_trend(
        &state.db,
        user_id,
        resolved.start,
        resolved.end,
        resolved.granularity,
    )
    .await
    .map_err(internal_error)?;

    let top_categories = owner_top_categories(&state.db, user_id, resolved.start, resolved.end, 8)
        .await
        .map_err(internal_error)?;

    let recommendations = build_recommendations(&kpis, &budgets, &top_categories);

    Ok(Json(InsightsResponse {
        period: PeriodMeta {
            label: resolved.label,
            start: resolved.start,
            end: resolved.end,
            granularity: resolved.granularity.label_public().to_string(),
        },
        kpis,
        trend,
        top_categories,
        budgets,
        recommendations,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(name: &str, budgeted: f64, spent: f64) -> BudgetSpend {
        let remaining = budgeted - spent;
        let overspent = spent > budgeted && budgeted > 0.0;
        let pct = if budgeted > 0.0 {
            Some(spent / budgeted)
        } else {
            None
        };
        BudgetSpend {
            id: Uuid::new_v4(),
            name: name.to_string(),
            budgeted,
            spent,
            remaining,
            overspent,
            pct,
        }
    }

    fn cat(name: &str, spent: f64) -> CategoryAgg {
        CategoryAgg {
            category: name.to_string(),
            spent,
        }
    }

    // --- build_recommendations (pure) ---

    #[test]
    fn recommends_onboarding_when_no_budgets() {
        let kpis = Kpis::default();
        let recs = build_recommendations(&kpis, &[], &[]);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("Create your first budget"));
    }

    #[test]
    fn recommends_adding_data_when_budgets_but_no_activity() {
        let kpis = Kpis {
            budget_count: 2,
            ..Default::default()
        };
        let recs = build_recommendations(&kpis, &[], &[]);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("No spending or budget amounts"));
    }

    #[test]
    fn flags_a_single_overspent_budget_with_amount() {
        let kpis = Kpis {
            budget_count: 1,
            total_budgeted: 100.0,
            total_spent: 130.0,
            total_remaining: -30.0,
            overspent_count: 1,
        };
        let budgets = vec![bs("Groceries", 100.0, 130.0)];
        let recs = build_recommendations(&kpis, &budgets, &[]);
        assert!(recs[0].contains("Groceries"));
        assert!(recs[0].contains("$30.00"));
        assert!(recs[0].starts_with("Consider reviewing"));
    }

    #[test]
    fn flags_multiple_overspent_budgets_collectively() {
        let kpis = Kpis {
            budget_count: 2,
            total_budgeted: 200.0,
            total_spent: 260.0,
            total_remaining: -60.0,
            overspent_count: 2,
        };
        let budgets = vec![bs("A", 100.0, 130.0), bs("B", 100.0, 130.0)];
        let recs = build_recommendations(&kpis, &budgets, &[]);
        assert!(recs[0].contains("2 budgets"));
    }

    #[test]
    fn warns_on_near_limit_budget() {
        let kpis = Kpis {
            budget_count: 1,
            total_budgeted: 100.0,
            total_spent: 85.0,
            total_remaining: 15.0,
            overspent_count: 0,
        };
        let budgets = vec![bs("Dining", 100.0, 85.0)]; // 85%
        let recs = build_recommendations(&kpis, &budgets, &[]);
        assert!(recs
            .iter()
            .any(|r| r.contains("85%") && r.contains("Dining")));
    }

    #[test]
    fn highlights_top_category() {
        let kpis = Kpis {
            budget_count: 1,
            total_budgeted: 1000.0,
            total_spent: 200.0,
            total_remaining: 800.0,
            overspent_count: 0,
        };
        let budgets = vec![bs("Main", 1000.0, 200.0)];
        let cats = vec![cat("Rent", 150.0), cat("Food", 50.0)];
        let recs = build_recommendations(&kpis, &budgets, &cats);
        assert!(recs
            .iter()
            .any(|r| r.contains("Rent") && r.contains("$150.00")));
    }

    #[test]
    fn positive_reinforcement_when_under_budget() {
        let kpis = Kpis {
            budget_count: 1,
            total_budgeted: 1000.0,
            total_spent: 200.0,
            total_remaining: 800.0,
            overspent_count: 0,
        };
        let budgets = vec![bs("Main", 1000.0, 200.0)];
        let recs = build_recommendations(&kpis, &budgets, &[]);
        assert!(recs
            .iter()
            .any(|r| r.contains("under budget") && r.contains("$800.00")));
    }

    #[test]
    fn caps_recommendations_at_four() {
        let kpis = Kpis {
            budget_count: 3,
            total_budgeted: 300.0,
            total_spent: 400.0,
            total_remaining: -100.0,
            overspent_count: 3,
        };
        // 3 overspent + a near-limit + a top category would be >4 lines pre-cap.
        let budgets = vec![
            bs("A", 100.0, 130.0),
            bs("B", 100.0, 130.0),
            bs("C", 100.0, 140.0),
        ];
        let cats = vec![cat("X", 90.0)];
        let recs = build_recommendations(&kpis, &budgets, &cats);
        assert!(recs.len() <= 4);
    }

    // --- DB-backed aggregation + owner-scoping (live pgvector DB) ---
    // Mirrors the existing rollup/renew DB tests in budget.rs: connect via
    // DATABASE_URL (falling back to the local compose DB), seed, assert, clean up.

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        PgPool::connect(&url).await.expect("connect to test db")
    }

    async fn seed_user(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(id)
            .bind(format!("insights-{id}@example.test"))
            .execute(pool)
            .await
            .expect("seed user");
        id
    }

    /// Seed a monthly time_based budget with one expense category (limit
    /// `cat_limit`) and one expense transaction dated `now` for `spent`. Returns
    /// the budget id.
    async fn seed_budget_with_spend(
        pool: &PgPool,
        owner: Uuid,
        name: &str,
        cat_limit: f64,
        spent: f64,
    ) -> Uuid {
        let bid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, $3, 'monthly', 'time_based')",
        )
        .bind(bid)
        .bind(owner)
        .bind(name)
        .execute(pool)
        .await
        .expect("seed budget");

        let cid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, $3, 'expense', $4)",
        )
        .bind(cid)
        .bind(bid)
        .bind(format!("{name} Cat"))
        .bind(cat_limit)
        .execute(pool)
        .await
        .expect("seed category");

        if spent > 0.0 {
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) \
                 VALUES ($1, $2, $3, $4, 'seed', NOW())",
            )
            .bind(Uuid::new_v4())
            .bind(bid)
            .bind(cid)
            .bind(spent)
            .execute(pool)
            .await
            .expect("seed transaction");
        }
        bid
    }

    async fn cleanup(pool: &PgPool, user_id: Uuid) {
        sqlx::query("DELETE FROM budgets WHERE owner_id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(pool)
            .await
            .ok();
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn budget_rollup_sums_kpis_across_owned_budgets() {
        let pool = test_pool().await;
        let user = seed_user(&pool).await;
        seed_budget_with_spend(&pool, user, "Alpha", 500.0, 200.0).await;
        seed_budget_with_spend(&pool, user, "Beta", 300.0, 350.0).await; // overspent

        let (kpis, budgets) = budget_rollup(&pool, user, Utc::now())
            .await
            .expect("rollup");

        assert_eq!(kpis.budget_count, 2);
        assert!((kpis.total_budgeted - 800.0).abs() < 1e-6);
        assert!((kpis.total_spent - 550.0).abs() < 1e-6);
        assert!((kpis.total_remaining - 250.0).abs() < 1e-6);
        assert_eq!(kpis.overspent_count, 1);
        // Sorted by spend desc: Beta (350) before Alpha (200).
        assert_eq!(budgets[0].name, "Beta");
        assert!(budgets[0].overspent);
        assert!(!budgets[1].overspent);

        cleanup(&pool, user).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn rollup_is_owner_scoped() {
        let pool = test_pool().await;
        let me = seed_user(&pool).await;
        let other = seed_user(&pool).await;
        seed_budget_with_spend(&pool, me, "Mine", 100.0, 40.0).await;
        seed_budget_with_spend(&pool, other, "Theirs", 999.0, 888.0).await;

        let (kpis, budgets) = budget_rollup(&pool, me, Utc::now()).await.expect("rollup");

        assert_eq!(kpis.budget_count, 1);
        assert_eq!(budgets.len(), 1);
        assert_eq!(budgets[0].name, "Mine");
        // The other user's larger figures must NOT leak in.
        assert!((kpis.total_spent - 40.0).abs() < 1e-6);
        assert!((kpis.total_budgeted - 100.0).abs() < 1e-6);

        cleanup(&pool, me).await;
        cleanup(&pool, other).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn top_categories_is_owner_scoped_and_ranked() {
        let pool = test_pool().await;
        let me = seed_user(&pool).await;
        let other = seed_user(&pool).await;
        seed_budget_with_spend(&pool, me, "Groceries", 100.0, 60.0).await;
        seed_budget_with_spend(&pool, me, "Travel", 100.0, 90.0).await;
        seed_budget_with_spend(&pool, other, "Secret", 100.0, 95.0).await;

        let cats = owner_top_categories(&pool, me, None, Utc::now(), 8)
            .await
            .expect("top categories");

        // Only my two categories, ranked by spend (Travel 90 > Groceries 60).
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].category, "Travel Cat");
        assert_eq!(cats[1].category, "Groceries Cat");
        assert!(cats.iter().all(|c| c.category != "Secret Cat"));

        cleanup(&pool, me).await;
        cleanup(&pool, other).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn spending_trend_is_owner_scoped() {
        let pool = test_pool().await;
        let me = seed_user(&pool).await;
        let other = seed_user(&pool).await;
        seed_budget_with_spend(&pool, me, "Mine", 100.0, 25.0).await;
        seed_budget_with_spend(&pool, other, "Theirs", 100.0, 70.0).await;

        let trend = owner_spending_trend(&pool, me, None, Utc::now(), Granularity::Monthly)
            .await
            .expect("trend");

        let total: f64 = trend.iter().map(|p| p.spent).sum();
        // Only my 25 — never the other user's 70.
        assert!((total - 25.0).abs() < 1e-6);

        cleanup(&pool, me).await;
        cleanup(&pool, other).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn project_budget_spend_uses_full_span_not_current_period() {
        // A project budget tracks spend across its whole lifetime (#48), not the
        // current calendar period. A transaction dated ~45 days ago (outside the
        // current month window) must still count toward the project's spend —
        // mirroring budget::aggregated_budget_amounts. A time-based control budget
        // with the same back-dated transaction must NOT count it this month.
        let pool = test_pool().await;
        let me = seed_user(&pool).await;
        let old = Utc::now() - chrono::Duration::days(45);

        // Project budget created 60 days ago with a 45-day-old transaction.
        let proj = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type, created_at) \
             VALUES ($1, $2, 'Remodel', 'monthly', 'project', $3)",
        )
        .bind(proj)
        .bind(me)
        .bind(Utc::now() - chrono::Duration::days(60))
        .execute(&pool)
        .await
        .expect("seed project budget");
        let pcat = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Materials', 'expense', 1000.0)",
        )
        .bind(pcat)
        .bind(proj)
        .execute(&pool)
        .await
        .expect("seed project category");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) \
             VALUES ($1, $2, $3, 400.0, 'lumber', $4)",
        )
        .bind(Uuid::new_v4())
        .bind(proj)
        .bind(pcat)
        .bind(old)
        .execute(&pool)
        .await
        .expect("seed project transaction");

        let (_, budgets) = budget_rollup(&pool, me, Utc::now()).await.expect("rollup");
        let row = budgets
            .iter()
            .find(|b| b.name == "Remodel")
            .expect("project row");
        // Full-span spend includes the 45-day-old $400.
        assert!((row.spent - 400.0).abs() < 1e-6, "got {}", row.spent);

        cleanup(&pool, me).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn owned_active_budgets_includes_only_the_active_shared_budget() {
        let pool = test_pool().await;
        let owner = seed_user(&pool).await;
        let viewer = seed_user(&pool).await;

        let own = seed_budget_with_spend(&pool, viewer, "Own", 50.0, 0.0).await;
        let active_shared = seed_budget_with_spend(&pool, owner, "ActiveShared", 100.0, 0.0).await;
        let other_shared = seed_budget_with_spend(&pool, owner, "OtherShared", 200.0, 0.0).await;

        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("viewer email");
        for bid in [active_shared, other_shared] {
            sqlx::query(
                "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
                 VALUES ($1, $2, $3, 'view')",
            )
            .bind(Uuid::new_v4())
            .bind(bid)
            .bind(&email)
            .execute(&pool)
            .await
            .expect("seed share");
        }

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(active_shared)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("set preference");

        let rows = owned_active_budgets(&pool, viewer).await.expect("query");
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

        assert!(ids.contains(&own), "own budget must still be included");
        assert!(
            ids.contains(&active_shared),
            "the ACTIVE shared budget must be included"
        );
        assert!(
            !ids.contains(&other_shared),
            "a merely-shared budget must NOT leak into the viewer's insights totals"
        );

        cleanup(&pool, viewer).await;
        cleanup(&pool, owner).await;
    }

    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn owned_active_budgets_excludes_an_archived_active_shared_budget() {
        let pool = test_pool().await;
        let owner = seed_user(&pool).await;
        let viewer = seed_user(&pool).await;

        let archived_shared =
            seed_budget_with_spend(&pool, owner, "ArchivedShared", 100.0, 0.0).await;
        sqlx::query("UPDATE budgets SET archived_at = NOW() WHERE id = $1")
            .bind(archived_shared)
            .execute(&pool)
            .await
            .expect("archive budget");

        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("viewer email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(archived_shared)
        .bind(&email)
        .execute(&pool)
        .await
        .expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(archived_shared)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("set preference");

        let rows = owned_active_budgets(&pool, viewer).await.expect("query");
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        assert!(
            !ids.contains(&archived_shared),
            "archived_at IS NULL must apply to the shared row too"
        );

        cleanup(&pool, viewer).await;
        cleanup(&pool, owner).await;
    }

    // #391 FINDING 3: no test previously covered the load-bearing case for the
    // `EXISTS (SELECT 1 FROM budget_shares ...)` clause in owned_active_budgets --
    // a viewer's active_budget_id preference pointing at a budget that is NOT
    // (or no longer) shared with them. That state is reachable and persistent:
    // ON DELETE SET NULL only clears the preference when the BUDGET itself is
    // deleted, not when a budget_shares row is revoked (see rag.rs's
    // chat_fallback_ignores_revoked_active_budget). Without the EXISTS guard,
    // another user's budget would silently fold into the viewer's
    // total_budgeted/total_spent KPIs.
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[tokio::test]
    async fn owned_active_budgets_excludes_an_active_budget_not_shared_with_the_viewer() {
        let pool = test_pool().await;
        let owner = seed_user(&pool).await;
        let viewer = seed_user(&pool).await;

        // Owned by someone else, and never shared with the viewer at all.
        let unshared = seed_budget_with_spend(&pool, owner, "NeverShared", 100.0, 0.0).await;

        // The viewer's preference nonetheless points at it -- e.g. a revoked
        // budget_shares row (ON DELETE SET NULL does not fire for a share
        // revocation, only a budget deletion).
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(unshared)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("set preference");

        let rows = owned_active_budgets(&pool, viewer).await.expect("query");
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        assert!(
            !ids.contains(&unshared),
            "a budget the viewer neither owns nor has an active share for must never leak into their insights totals"
        );

        cleanup(&pool, viewer).await;
        cleanup(&pool, owner).await;
    }
}
