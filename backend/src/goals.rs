use axum::{
    extract::{Path, State, Extension},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use chrono::{DateTime, Utc, NaiveDate};

use crate::auth::AppState;
use crate::budget::{check_permission, Permission, log_audit};
use crate::db::{Goal, GoalContribution};
use crate::error::{internal_error, internal_error_message};

/// A goal plus its computed progress fields (serialized to the client / AI context).
#[derive(Serialize)]
pub struct GoalWithProgress {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub goal_type: String,
    pub target_amount: f64,
    pub target_date: Option<NaiveDate>,
    pub linked_category_id: Option<Uuid>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    // computed:
    pub current_amount: f64,
    pub remaining: f64,
    pub percent: f64,
    pub is_achieved: bool,
    pub monthly_needed: Option<f64>,
}

/// A goal detail view: progress fields flattened, plus its contribution history.
#[derive(Serialize)]
pub struct GoalDetail {
    #[serde(flatten)]
    pub goal: GoalWithProgress,
    pub contributions: Vec<GoalContribution>,
}

/// Percentage of a goal completed, clamped to [0, 100] for display.
pub fn goal_percent(current: f64, target: f64) -> f64 {
    if target <= 0.0 {
        return 0.0;
    }
    (current / target * 100.0).clamp(0.0, 100.0)
}

/// Monthly amount needed to cover `remaining` by `target_date`, given `today`.
/// Returns None when nothing remains, there is no target date, or the date is past.
pub fn monthly_needed(remaining: f64, target_date: Option<NaiveDate>, today: NaiveDate) -> Option<f64> {
    if remaining <= 0.0 {
        return None;
    }
    let target = target_date?;
    if target <= today {
        return None;
    }
    let days = (target - today).num_days();
    let months = ((days as f64) / 30.0).ceil().max(1.0);
    Some(remaining / months)
}

/// Compute progress for a single goal (hybrid: contributions + optional linked category).
pub async fn goal_progress(db: &PgPool, g: &Goal, today: NaiveDate) -> Result<GoalWithProgress, String> {
    let contrib_sum: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0)::float8 FROM goal_contributions WHERE goal_id = $1"
    )
    .bind(g.id)
    .fetch_one(db)
    .await
    .map_err(internal_error_message)?;

    // Sum only transactions whose category belongs to THIS goal's budget. The
    // budget-scoped JOIN is defense-in-depth: even if a linked_category_id from
    // another budget were ever stored, it cannot leak that tenant's totals.
    let cat_sum: f64 = match g.linked_category_id {
        Some(cid) => sqlx::query_scalar(
            "SELECT COALESCE(SUM(t.amount), 0)::float8 FROM transactions t \
             JOIN categories c ON c.id = t.category_id \
             WHERE t.category_id = $1 AND c.budget_id = $2 AND NOT t.excluded_from_budget"
        )
        .bind(cid)
        .bind(g.budget_id)
        .fetch_one(db)
        .await
        .map_err(internal_error_message)?,
        None => 0.0,
    };

    let current = contrib_sum + cat_sum;
    let remaining = (g.target_amount - current).max(0.0);

    Ok(GoalWithProgress {
        id: g.id,
        budget_id: g.budget_id,
        name: g.name.clone(),
        goal_type: g.goal_type.clone(),
        target_amount: g.target_amount,
        target_date: g.target_date,
        linked_category_id: g.linked_category_id,
        status: g.status.clone(),
        created_at: g.created_at,
        current_amount: current,
        remaining,
        percent: goal_percent(current, g.target_amount),
        is_achieved: current >= g.target_amount,
        monthly_needed: monthly_needed(remaining, g.target_date, today),
    })
}

/// All goals for a budget, each with computed progress.
pub async fn list_goals_with_progress(db: &PgPool, budget_id: Uuid) -> Result<Vec<GoalWithProgress>, String> {
    let goals = sqlx::query_as::<_, Goal>(
        "SELECT * FROM goals WHERE budget_id = $1 ORDER BY created_at ASC"
    )
    .bind(budget_id)
    .fetch_all(db)
    .await
    .map_err(internal_error_message)?;

    let today = Utc::now().date_naive();
    let mut out = Vec::with_capacity(goals.len());
    for g in &goals {
        out.push(goal_progress(db, g, today).await?);
    }
    Ok(out)
}

/// Insert a goal, returning the created row.
pub async fn insert_goal(
    db: &PgPool,
    budget_id: Uuid,
    name: &str,
    goal_type: &str,
    target_amount: f64,
    target_date: Option<NaiveDate>,
    linked_category_id: Option<Uuid>,
) -> Result<Goal, String> {
    sqlx::query_as::<_, Goal>(
        "INSERT INTO goals (id, budget_id, name, goal_type, target_amount, target_date, linked_category_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(budget_id)
    .bind(name)
    .bind(goal_type)
    .bind(target_amount)
    .bind(target_date)
    .bind(linked_category_id)
    .fetch_one(db)
    .await
    .map_err(internal_error_message)
}

/// Insert a contribution toward a goal, returning the created row.
pub async fn insert_contribution(
    db: &PgPool,
    goal_id: Uuid,
    user_id: Uuid,
    amount: f64,
    note: Option<&str>,
) -> Result<GoalContribution, String> {
    sqlx::query_as::<_, GoalContribution>(
        "INSERT INTO goal_contributions (id, goal_id, user_id, amount, note) \
         VALUES ($1, $2, $3, $4, $5) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(goal_id)
    .bind(user_id)
    .bind(amount)
    .bind(note)
    .fetch_one(db)
    .await
    .map_err(internal_error_message)
}

/// Case-insensitive lookup of a goal by name within a budget.
pub async fn find_goal_by_name(db: &PgPool, budget_id: Uuid, name: &str) -> Result<Option<Goal>, String> {
    sqlx::query_as::<_, Goal>(
        "SELECT * FROM goals WHERE budget_id = $1 AND LOWER(name) = LOWER($2) LIMIT 1"
    )
    .bind(budget_id)
    .bind(name)
    .fetch_optional(db)
    .await
    .map_err(internal_error_message)
}

/// Render active goals as a context block for the conversational AI prompt.
pub async fn format_goals_context(db: &PgPool, budget_id: Uuid) -> String {
    let goals = match list_goals_with_progress(db, budget_id).await {
        Ok(g) => g,
        Err(_) => return String::new(),
    };
    let active: Vec<&GoalWithProgress> = goals.iter().filter(|g| g.status == "active").collect();
    if active.is_empty() {
        return "GOALS:\n- (none yet)\n".to_string();
    }
    let mut s = String::from("GOALS:\n");
    for g in active {
        let date_str = g.target_date.map(|d| format!(" by {}", d)).unwrap_or_default();
        let plan = g.monthly_needed.map(|m| format!(" | need ${:.2}/mo", m)).unwrap_or_default();
        let achieved = if g.is_achieved { " ACHIEVED" } else { "" };
        s.push_str(&format!(
            "- {} [{}] ${:.2}/${:.2} ({:.0}%{}){}{}\n",
            g.name, g.goal_type, g.current_amount, g.target_amount, g.percent, achieved, date_str, plan
        ));
    }
    s
}

#[derive(Deserialize)]
pub struct CreateGoalPayload {
    pub name: String,
    pub goal_type: String, // 'savings' | 'debt'
    pub target_amount: f64,
    pub target_date: Option<NaiveDate>,
    pub linked_category_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct UpdateGoalPayload {
    pub name: Option<String>,
    pub target_amount: Option<f64>,
    pub target_date: Option<NaiveDate>,
    pub status: Option<String>,
    pub linked_category_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct ContributionPayload {
    pub amount: f64,
    pub note: Option<String>,
}

fn require_edit(perm: &Permission) -> Result<(), (StatusCode, String)> {
    if *perm != Permission::Owner && *perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions".to_string()));
    }
    Ok(())
}

/// Ensure a supplied `linked_category_id` (if any) belongs to this budget.
/// Without this, a user with edit access to budget A could link a category
/// owned by another tenant's budget B and read B's spending totals back
/// through the goal's computed progress (IDOR / cross-tenant leak).
async fn validate_linked_category(
    db: &PgPool,
    budget_id: Uuid,
    linked_category_id: Option<Uuid>,
) -> Result<(), (StatusCode, String)> {
    if let Some(cid) = linked_category_id {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM categories WHERE id = $1 AND budget_id = $2"
        )
        .bind(cid)
        .bind(budget_id)
        .fetch_optional(db)
        .await
        .map_err(internal_error)?;
        if found.is_none() {
            return Err((StatusCode::BAD_REQUEST, "linked_category_id must belong to this budget".to_string()));
        }
    }
    Ok(())
}

pub async fn create_goal(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CreateGoalPayload>,
) -> Result<Json<GoalWithProgress>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    require_edit(&perm)?;
    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let gtype = match payload.goal_type.as_str() {
        "savings" | "debt" => payload.goal_type.clone(),
        _ => return Err((StatusCode::BAD_REQUEST, "goal_type must be 'savings' or 'debt'".to_string())),
    };

    validate_linked_category(&state.db, budget_id, payload.linked_category_id).await?;

    let goal = insert_goal(
        &state.db, budget_id, &payload.name, &gtype,
        payload.target_amount, payload.target_date, payload.linked_category_id,
    )
    .await
    .map_err(|e| internal_error(format!("Failed to create goal: {}", e)))?;

    log_audit(&state.db, budget_id, user_id, "CREATE_GOAL",
        &format!("Created {} goal '{}' targeting ${:.2}", gtype, goal.name, goal.target_amount)).await;

    let with = goal_progress(&state.db, &goal, Utc::now().date_naive()).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(with))
}

pub async fn list_goals(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<GoalWithProgress>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }
    let goals = list_goals_with_progress(&state.db, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(goals))
}

pub async fn get_goal(
    State(state): State<AppState>,
    Path((budget_id, goal_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<GoalDetail>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    let goal = sqlx::query_as::<_, Goal>("SELECT * FROM goals WHERE id = $1 AND budget_id = $2")
        .bind(goal_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Goal not found".to_string()))?;

    let with = goal_progress(&state.db, &goal, Utc::now().date_naive()).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let contributions = sqlx::query_as::<_, GoalContribution>(
        "SELECT * FROM goal_contributions WHERE goal_id = $1 ORDER BY contributed_at DESC"
    )
    .bind(goal_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(Json(GoalDetail { goal: with, contributions }))
}

pub async fn update_goal(
    State(state): State<AppState>,
    Path((budget_id, goal_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<UpdateGoalPayload>,
) -> Result<Json<GoalWithProgress>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    require_edit(&perm)?;
    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    validate_linked_category(&state.db, budget_id, payload.linked_category_id).await?;

    // COALESCE keeps existing values when a field is omitted. target_date and
    // linked_category_id can be set/changed but not cleared via this endpoint.
    let goal = sqlx::query_as::<_, Goal>(
        "UPDATE goals SET \
            name = COALESCE($3, name), \
            target_amount = COALESCE($4, target_amount), \
            target_date = COALESCE($5, target_date), \
            status = COALESCE($6, status), \
            linked_category_id = COALESCE($7, linked_category_id) \
         WHERE id = $1 AND budget_id = $2 RETURNING *"
    )
    .bind(goal_id)
    .bind(budget_id)
    .bind(&payload.name)
    .bind(payload.target_amount)
    .bind(payload.target_date)
    .bind(&payload.status)
    .bind(payload.linked_category_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or((StatusCode::NOT_FOUND, "Goal not found".to_string()))?;

    log_audit(&state.db, budget_id, user_id, "UPDATE_GOAL",
        &format!("Updated goal '{}'", goal.name)).await;

    let with = goal_progress(&state.db, &goal, Utc::now().date_naive()).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(with))
}

pub async fn delete_goal(
    State(state): State<AppState>,
    Path((budget_id, goal_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    require_edit(&perm)?;
    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let res = sqlx::query("DELETE FROM goals WHERE id = $1 AND budget_id = $2")
        .bind(goal_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    if res.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Goal not found".to_string()));
    }
    log_audit(&state.db, budget_id, user_id, "DELETE_GOAL",
        &format!("Deleted goal {}", goal_id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_contribution(
    State(state): State<AppState>,
    Path((budget_id, goal_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<ContributionPayload>,
) -> Result<Json<GoalWithProgress>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    require_edit(&perm)?;
    crate::access::require_owner_entitled(&state.db, budget_id).await?;

    let goal = sqlx::query_as::<_, Goal>("SELECT * FROM goals WHERE id = $1 AND budget_id = $2")
        .bind(goal_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Goal not found".to_string()))?;

    insert_contribution(&state.db, goal.id, user_id, payload.amount, payload.note.as_deref())
        .await
        .map_err(|e| internal_error(format!("Failed to add contribution: {}", e)))?;

    log_audit(&state.db, budget_id, user_id, "ADD_GOAL_CONTRIBUTION",
        &format!("Added ${:.2} toward goal '{}'", payload.amount, goal.name)).await;

    let with = goal_progress(&state.db, &goal, Utc::now().date_naive()).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(with))
}

pub async fn list_contributions(
    State(state): State<AppState>,
    Path((budget_id, goal_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<GoalContribution>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }
    let rows = sqlx::query_as::<_, GoalContribution>(
        "SELECT gc.* FROM goal_contributions gc \
         JOIN goals g ON g.id = gc.goal_id \
         WHERE gc.goal_id = $1 AND g.budget_id = $2 ORDER BY gc.contributed_at DESC"
    )
    .bind(goal_id)
    .bind(budget_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn percent_basic() {
        assert!((goal_percent(50.0, 200.0) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn percent_clamps_over_100() {
        assert!((goal_percent(300.0, 200.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn percent_zero_target_is_zero() {
        assert_eq!(goal_percent(10.0, 0.0), 0.0);
    }

    #[test]
    fn monthly_none_when_achieved() {
        assert_eq!(monthly_needed(0.0, Some(d(2026, 12, 1)), d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_none_without_date() {
        assert_eq!(monthly_needed(300.0, None, d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_none_when_date_past() {
        assert_eq!(monthly_needed(300.0, Some(d(2025, 1, 1)), d(2026, 1, 1)), None);
    }

    #[test]
    fn monthly_splits_over_three_months() {
        // 2026-01-01 -> 2026-04-01 is 90 days -> ceil(90/30)=3 months -> 300/3=100
        let v = monthly_needed(300.0, Some(d(2026, 4, 1)), d(2026, 1, 1)).unwrap();
        assert!((v - 100.0).abs() < 1e-9);
    }

    // Write-gate wiring note (Plan B3): create_goal / update_goal / delete_goal /
    // create_contribution each call `crate::access::require_owner_entitled` right
    // after their `require_edit` check. The enforcement OUTCOME (lapsed owner →
    // 402) is pinned hermetically once, at the source, by
    // `access::tests::owner_entitled_enforced_blocks_lapsed_owner_402`; there is
    // deliberately no flag-on handler test here, since flipping the process-global
    // `BILLING_WRITE_ENFORCEMENT` races the parallel non-serial write tests (see
    // commit 10cc33b, which removed exactly that pattern). Presence of the gate is
    // covered by Task 5's mutation-coverage audit.
}
