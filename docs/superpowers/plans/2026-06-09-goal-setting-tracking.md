# Pillar 4 — Goal Setting & Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users create savings/debt-payoff goals, contribute toward them, and track progress — all through the existing conversational interface, backed by new database tables and REST routes.

**Architecture:** A new `goals.rs` backend module owns goal/contribution persistence and progress computation, reusing `budget::check_permission` and `budget::log_audit`. Progress is computed at read time (hybrid: contributions ledger + optional linked-category transactions). The conversational layer (`rag.rs`) gains a GOALS context block and two new actions (`CREATE_GOAL`, `ADD_GOAL_CONTRIBUTION`) on both the Gemini and offline paths. Frontend gets one discoverability prompt-chip (chat-only, no new UI).

**Tech Stack:** Rust (Axum, SQLx runtime queries, chrono), PostgreSQL + pgvector, Svelte 5. Dev server runs under `bacon` on `:3000`; DB (pgvector) on `:6153`. Reference spec: `docs/superpowers/specs/2026-06-09-goal-setting-tracking-design.md`.

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `backend/migrations/20260609020000_goals.sql` | `goals` + `goal_contributions` tables | Create |
| `backend/src/db.rs` | `Goal`, `GoalContribution` row structs | Modify |
| `backend/src/goals.rs` | Progress math, DB helpers, REST handlers, context formatter | Create |
| `backend/src/main.rs` | `mod goals;` + route registration | Modify |
| `backend/src/rag.rs` | GOALS context block + 2 new AI actions | Modify |
| `frontend/src/App.svelte` | One goal prompt-chip | Modify |
| `Goal.md` | Mark Pillar 4 status | Modify |

**Verification reality:** This codebase has no automated test harness and uses runtime SQLx queries (no compile-time DB). The only pure, unit-testable logic is the progress math (Task 3 — real TDD). Everything else is integration-verified via `cargo check` + curl + chat round-trips, matching the codebase's established verification style. The backend auto-rebuilds and restarts under `bacon`; "restart" below means "save a file and wait for bacon to relink," and `cargo check` can be run directly without conflicting.

---

## Task 1: Migration for goals + goal_contributions

**Files:**
- Create: `backend/migrations/20260609020000_goals.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Pillar 4: financial goals (savings & debt payoff) scoped to a budget,
-- with a contributions ledger. Progress is computed at read time (hybrid:
-- contributions + optional linked-category transactions).

CREATE TABLE IF NOT EXISTS goals (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    goal_type VARCHAR(20) NOT NULL, -- 'savings' | 'debt'
    target_amount DOUBLE PRECISION NOT NULL,
    target_date DATE,
    linked_category_id UUID REFERENCES categories(id) ON DELETE SET NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'active', -- 'active' | 'archived'
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_goal_name_per_budget UNIQUE (budget_id, name)
);

CREATE INDEX IF NOT EXISTS goals_budget_id_idx ON goals (budget_id);

CREATE TABLE IF NOT EXISTS goal_contributions (
    id UUID PRIMARY KEY,
    goal_id UUID NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    amount DOUBLE PRECISION NOT NULL,
    note TEXT,
    contributed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS goal_contributions_goal_id_idx ON goal_contributions (goal_id);
```

- [ ] **Step 2: Apply the migration (bacon restart) and verify tables exist**

Touch a source file so bacon rebuilds and runs migrations on startup, then check:

Run:
```bash
cd /home/robhicks/dev/nels/backend && touch src/main.rs && sleep 8
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('goals','goal_contributions') ORDER BY table_name;"
```
Expected output:
```
goals
goal_contributions
```
(order may be `goal_contributions` then `goals` — both present is what matters)

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/migrations/20260609020000_goals.sql
git commit -m "feat(goals): add goals and goal_contributions tables"
```

---

## Task 2: Row structs in db.rs

**Files:**
- Modify: `backend/src/db.rs`

- [ ] **Step 1: Add NaiveDate to the chrono import**

Change the existing import line at the top of `backend/src/db.rs`:
```rust
use chrono::{DateTime, Utc};
```
to:
```rust
use chrono::{DateTime, Utc, NaiveDate};
```

- [ ] **Step 2: Append the two structs to the end of `backend/src/db.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Goal {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub goal_type: String, // 'savings' | 'debt'
    pub target_amount: f64,
    pub target_date: Option<NaiveDate>,
    pub linked_category_id: Option<Uuid>,
    pub status: String, // 'active' | 'archived'
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GoalContribution {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub user_id: Option<Uuid>,
    pub amount: f64,
    pub note: Option<String>,
    pub contributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
```

- [ ] **Step 3: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -5`
Expected: `Finished` with no errors (pre-existing warnings in rag.rs/db.rs are fine; `Goal`/`GoalContribution` may warn "never constructed" until later tasks — acceptable).

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/db.rs
git commit -m "feat(goals): add Goal and GoalContribution row structs"
```

---

## Task 3: Pure progress-math helpers (TDD)

**Files:**
- Create: `backend/src/goals.rs`
- Modify: `backend/src/main.rs` (add `mod goals;` so the new file compiles & tests run)

- [ ] **Step 1: Register the module in `backend/src/main.rs`**

Find the module declarations near the top:
```rust
mod db;
mod auth;
mod budget;
mod r#rag;
```
Add `mod goals;` after `mod budget;`:
```rust
mod db;
mod auth;
mod budget;
mod goals;
mod r#rag;
```

- [ ] **Step 2: Write `backend/src/goals.rs` with pure math + failing tests**

```rust
use chrono::NaiveDate;

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
}
```

- [ ] **Step 3: Run the tests to verify they pass (functions are implemented alongside)**

Run: `cd /home/robhicks/dev/nels/backend && cargo test --color never goals:: 2>&1 | tail -15`
Expected: `test result: ok. 7 passed`

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/goals.rs backend/src/main.rs
git commit -m "feat(goals): add progress-math helpers with unit tests"
```

---

## Task 4: DB helpers + progress structs in goals.rs

**Files:**
- Modify: `backend/src/goals.rs`

- [ ] **Step 1: Add imports and response structs at the TOP of `backend/src/goals.rs`** (above the existing `goal_percent` fn)

```rust
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
```

- [ ] **Step 2: Add DB helper functions to `backend/src/goals.rs`** (below the existing `monthly_needed` fn, above the `#[cfg(test)]` module)

```rust
/// Compute progress for a single goal (hybrid: contributions + optional linked category).
pub async fn goal_progress(db: &PgPool, g: &Goal, today: NaiveDate) -> Result<GoalWithProgress, String> {
    let contrib_sum: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0)::float8 FROM goal_contributions WHERE goal_id = $1"
    )
    .bind(g.id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let cat_sum: f64 = match g.linked_category_id {
        Some(cid) => sqlx::query_scalar(
            "SELECT COALESCE(SUM(amount), 0)::float8 FROM transactions WHERE category_id = $1"
        )
        .bind(cid)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?,
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
    .map_err(|e| e.to_string())?;

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
    .map_err(|e| e.to_string())
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
    .map_err(|e| e.to_string())
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
    .map_err(|e| e.to_string())
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
```

- [ ] **Step 3: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -8`
Expected: `Finished` with no errors. (Handlers don't exist yet; helpers may warn "never used" — acceptable until Task 5/6/7 consume them.)

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/goals.rs
git commit -m "feat(goals): add progress structs and DB helpers"
```

---

## Task 5: REST handlers in goals.rs

**Files:**
- Modify: `backend/src/goals.rs`

- [ ] **Step 1: Add request payloads + handlers to `backend/src/goals.rs`** (below the helpers from Task 4, above `#[cfg(test)]`)

```rust
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

pub async fn create_goal(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CreateGoalPayload>,
) -> Result<Json<GoalWithProgress>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    require_edit(&perm)?;

    let gtype = match payload.goal_type.as_str() {
        "savings" | "debt" => payload.goal_type.clone(),
        _ => return Err((StatusCode::BAD_REQUEST, "goal_type must be 'savings' or 'debt'".to_string())),
    };

    let goal = insert_goal(
        &state.db, budget_id, &payload.name, &gtype,
        payload.target_amount, payload.target_date, payload.linked_category_id,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create goal: {}", e)))?;

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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Goal not found".to_string()))?;

    let with = goal_progress(&state.db, &goal, Utc::now().date_naive()).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let contributions = sqlx::query_as::<_, GoalContribution>(
        "SELECT * FROM goal_contributions WHERE goal_id = $1 ORDER BY contributed_at DESC"
    )
    .bind(goal_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
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

    let res = sqlx::query("DELETE FROM goals WHERE id = $1 AND budget_id = $2")
        .bind(goal_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

    let goal = sqlx::query_as::<_, Goal>("SELECT * FROM goals WHERE id = $1 AND budget_id = $2")
        .bind(goal_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Goal not found".to_string()))?;

    insert_contribution(&state.db, goal.id, user_id, payload.amount, payload.note.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to add contribution: {}", e)))?;

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
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -8`
Expected: `Finished` with no errors. (Handlers warn "never used" until routes are wired in Task 6 — acceptable.)

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/goals.rs
git commit -m "feat(goals): add REST handlers for goals and contributions"
```

---

## Task 6: Wire routes in main.rs + verify REST round-trip

**Files:**
- Modify: `backend/src/main.rs`

- [ ] **Step 1: Import the goal handlers**

In `backend/src/main.rs`, after the `use budget::{ ... };` block, add:
```rust
use goals::{
    create_goal, list_goals, get_goal, update_goal, delete_goal,
    create_contribution, list_contributions,
};
```

- [ ] **Step 2: Register the routes**

In the `protected_routes` router, immediately after the existing transactions route line:
```rust
        .route("/budgets/:id/transactions", post(create_transaction).get(list_transactions))
```
add:
```rust

        .route("/budgets/:id/goals", post(create_goal).get(list_goals))
        .route("/budgets/:id/goals/:goal_id", get(get_goal).put(update_goal).delete(delete_goal))
        .route("/budgets/:id/goals/:goal_id/contributions", post(create_contribution).get(list_contributions))
```

- [ ] **Step 3: Type-check, then let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -5`
Expected: `Finished` with no errors. Then `touch src/main.rs && sleep 8` to ensure bacon is serving the new routes.

- [ ] **Step 4: Verify REST round-trip via curl** (registers a throwaway user, creates a goal, contributes, checks progress, and checks the linked-category sum)

Run this script:
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="goal-test-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
F=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}")
TOKEN=$(echo "$F" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
BUDGET=$(curl -s $BASE/api/budgets -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")
echo "budget=$BUDGET"

echo "--- create savings goal (target 1000) ---"
G=$(curl -s -X POST $BASE/api/budgets/$BUDGET/goals -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"Vacation","goal_type":"savings","target_amount":1000}')
echo "$G"
GID=$(echo "$G" | python3 -c "import sys,json;print(json.load(sys.stdin)['id'])")

echo "--- contribute 250, then 250 ---"
curl -s -X POST $BASE/api/budgets/$BUDGET/goals/$GID/contributions -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"amount":250}' >/dev/null
curl -s -X POST $BASE/api/budgets/$BUDGET/goals/$GID/contributions -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"amount":250,"note":"tax refund"}' >/dev/null

echo "--- list goals (expect current_amount=500, percent=50, is_achieved=false) ---"
curl -s $BASE/api/budgets/$BUDGET/goals -H "Authorization: Bearer $TOKEN" \
  | python3 -c "import sys,json;g=json.load(sys.stdin)[0];print('current',g['current_amount'],'percent',g['percent'],'achieved',g['is_achieved'])"
```
Expected final line:
```
current 500.0 percent 50.0 achieved False
```

- [ ] **Step 5: Verify permission enforcement** (view-only collaborator cannot create a goal). Reuse `$BASE`/`$BUDGET`/`$TOKEN` from Step 4:
```bash
# Share budget as view-only with a second user, then try to create a goal as them.
VIEWER="goal-viewer-$(date +%s)@example.com"
curl -s -X POST $BASE/api/budgets/$BUDGET/share -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$VIEWER\",\"permission_level\":\"view\"}" >/dev/null
VS=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$VIEWER\"}")
VFLOW=$(echo "$VS" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
VSECRET=$(echo "$VS" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
VCODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$VSECRET').now())")
VTOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$VFLOW\",\"code\":\"$VCODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
curl -s -o /dev/null -w "viewer create goal HTTP %{http_code}\n" -X POST $BASE/api/budgets/$BUDGET/goals \
  -H "Authorization: Bearer $VTOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"Sneaky","goal_type":"savings","target_amount":50}'
```
Expected: `viewer create goal HTTP 403`

- [ ] **Step 6: Clean up the throwaway users**
```bash
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "DELETE FROM users WHERE email LIKE 'goal-test-%@example.com' OR email LIKE 'goal-viewer-%@example.com';"
```

- [ ] **Step 7: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/main.rs
git commit -m "feat(goals): register goal REST routes"
```

---

## Task 7: GOALS context block in rag.rs

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Build the goals context inside the `if let Some(bid) = active_budget_id` block**

In `backend/src/rag.rs`, the budget-context block ends by assigning `budget_context = format!( ... )`. Immediately BEFORE that `budget_context = format!(` line (after `shares_context` is built, around line 323), add:
```rust
        let goals_context = crate::goals::format_goals_context(&state.db, bid).await;
```

- [ ] **Step 2: Include it in the context `format!`**

In that same `budget_context = format!( ... )`, add a GOALS section. Change the format string's trailing section from:
```rust
             SHARING & COLLABORATION:\n\
             {}",
```
to:
```rust
             SHARING & COLLABORATION:\n\
             {}\n\n\
             {}",
```
and add `goals_context` as the final format argument — change:
```rust
            cats_context,
            txs_context,
            shares_context
        );
```
to:
```rust
            cats_context,
            txs_context,
            shares_context,
            goals_context
        );
```

- [ ] **Step 3: Type-check and let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -5`
Expected: `Finished` with no errors. Then `touch src/main.rs && sleep 8`.

- [ ] **Step 4: Verify the GOALS block reaches the AI context** (offline mode echoes context for breakdown queries). Create a user + goal, then ask for a breakdown:
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="goalctx-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
TOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
BUDGET=$(curl -s $BASE/api/budgets -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")
curl -s -X POST $BASE/api/budgets/$BUDGET/goals -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"name":"Emergency Fund","goal_type":"savings","target_amount":2000}' >/dev/null
curl -s -X POST $BASE/api/chat -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"message\":\"give me my breakdown\",\"budget_id\":\"$BUDGET\"}" | python3 -c "import sys,json;print('GOALS:' in json.load(sys.stdin)['response'])"
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc "DELETE FROM users WHERE email='$EMAIL';"
```
Expected output: `True` (offline breakdown response includes the GOALS block; note this requires `GEMINI_API_KEY` unset so the offline path runs).

- [ ] **Step 5: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/rag.rs
git commit -m "feat(goals): inject goals progress into AI context"
```

---

## Task 8: CREATE_GOAL and ADD_GOAL_CONTRIBUTION actions in rag.rs

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Extend `AiActionParams` with goal fields**

In `backend/src/rag.rs`, the `AiActionParams` struct ends with `permission_level: Option<String>,`. Add the goal fields just before the closing brace:
```rust
    permission_level: Option<String>, // "view", "edit"
    goal_name: Option<String>,
    goal_type: Option<String>,       // "savings", "debt"
    target_amount: Option<f64>,
    target_date: Option<String>,     // "YYYY-MM-DD"
    linked_category: Option<String>, // category name to link
    note: Option<String>,
```

- [ ] **Step 2: Initialize the new fields in the offline mock params**

In the offline branch, the `AiActionParams { ... }` initializer lists every field. Update it from:
```rust
        let mut action_params = AiActionParams {
            budget_name: None, budget_limit: None, time_frame: None, description: None,
            category_name: None, category_type: None, category_limit: None, amount: None,
            email: None, permission_level: None,
        };
```
to:
```rust
        let mut action_params = AiActionParams {
            budget_name: None, budget_limit: None, time_frame: None, description: None,
            category_name: None, category_type: None, category_limit: None, amount: None,
            email: None, permission_level: None,
            goal_name: None, goal_type: None, target_amount: None, target_date: None,
            linked_category: None, note: None,
        };
```

- [ ] **Step 3: Add offline matcher branches for goals**

In the offline `if/else if` chain, the goals branch must come BEFORE the generic `"log"/"spent"` and `"save"` branches so it wins. Insert this as the FIRST branch — change:
```rust
        if msg_lower.contains("create budget") || msg_lower.contains("setup budget") {
```
to:
```rust
        if msg_lower.contains("goal") || msg_lower.contains("save for") || msg_lower.contains("saving for")
            || msg_lower.contains("pay off") || msg_lower.contains("pay down") {
            let words: Vec<&str> = msg_lower.split_whitespace().collect();
            let mut amount = 0.0;
            for w in &words {
                if let Some(stripped) = w.strip_prefix('$') {
                    if let Ok(v) = stripped.parse::<f64>() { amount = v; }
                } else if let Ok(v) = w.parse::<f64>() {
                    amount = v;
                }
            }
            let is_contrib = msg_lower.contains("toward") || msg_lower.contains("towards")
                || msg_lower.contains("contribute") || msg_lower.contains("added")
                || msg_lower.contains("put");

            if is_contrib {
                action = "ADD_GOAL_CONTRIBUTION".to_string();
                // goal name = text after the last of toward/towards/to
                let mut gname = "My Goal".to_string();
                for kw in ["toward", "towards", " to "] {
                    if let Some(idx) = msg_lower.find(kw) {
                        let tail = msg_lower[idx + kw.len()..].trim().trim_matches(['"', '\'']).to_string();
                        if !tail.is_empty() { gname = tail; }
                    }
                }
                action_params.goal_name = Some(gname.clone());
                action_params.amount = Some(amount);
                response_text = format!("(Mock AI Offline Mode) Logged ${:.2} toward your goal '{}'.", amount, gname);
            } else {
                action = "CREATE_GOAL".to_string();
                let gtype = if msg_lower.contains("pay off") || msg_lower.contains("pay down") || msg_lower.contains("debt") {
                    "debt"
                } else {
                    "savings"
                };
                // goal name = word(s) after "for", else fallback
                let mut gname = if gtype == "debt" { "Debt".to_string() } else { "Savings Goal".to_string() };
                if let Some(idx) = words.iter().position(|&w| w == "for") {
                    if idx + 1 < words.len() {
                        gname = words[idx + 1..].join(" ").trim_matches(['"', '\'']).to_string();
                    }
                }
                action_params.goal_name = Some(gname.clone());
                action_params.goal_type = Some(gtype.to_string());
                action_params.target_amount = Some(amount);
                response_text = format!(
                    "(Mock AI Offline Mode) Created a {} goal '{}' targeting ${:.2}.",
                    gtype, gname, amount
                );
            }

        } else if msg_lower.contains("create budget") || msg_lower.contains("setup budget") {
```

- [ ] **Step 4: Add the Gemini prompt instructions**

In the Gemini system-instructions `format!`, update the action enum line from:
```rust
           \"action\": \"NONE\" | \"CREATE_BUDGET\" | \"UPDATE_BUDGET\" | \"CREATE_CATEGORY\" | \"ADD_TRANSACTION\" | \"SHARE_BUDGET\",\n\
```
to:
```rust
           \"action\": \"NONE\" | \"CREATE_BUDGET\" | \"UPDATE_BUDGET\" | \"CREATE_CATEGORY\" | \"ADD_TRANSACTION\" | \"SHARE_BUDGET\" | \"CREATE_GOAL\" | \"ADD_GOAL_CONTRIBUTION\",\n\
```
In the `action_params` JSON shape, after the `permission_level` line:
```rust
             \"permission_level\": \"view\" | \"edit\" (optional)\n\
```
change it to add the goal params:
```rust
             \"permission_level\": \"view\" | \"edit\" (optional),\n\
             \"goal_name\": \"string (optional)\",\n\
             \"goal_type\": \"savings\" | \"debt\" (optional),\n\
             \"target_amount\": number (optional),\n\
             \"target_date\": \"YYYY-MM-DD (optional)\",\n\
             \"linked_category\": \"string (optional)\",\n\
             \"note\": \"string (optional)\"\n\
```
Then add two CRITICAL RULES after rule 5 (renumber is unnecessary; append):
```rust
         6. If the user wants to set a financial goal (e.g. 'Save $3000 for a vacation by 2026-12-01' or 'Pay off my $5000 credit card'), set 'action' to 'CREATE_GOAL'. Populate 'goal_name', 'goal_type' ('savings' or 'debt'), 'target_amount', and 'target_date' if given. Optionally set 'linked_category' to an existing category name.\n\
         7. If the user reports money put toward a goal (e.g. 'I put $200 toward my vacation'), set 'action' to 'ADD_GOAL_CONTRIBUTION'. Populate 'goal_name' and 'amount', and 'note' if relevant. Use the GOALS context to match the goal name.\n\
         8. When discussing goals, be encouraging: celebrate when a goal crosses 25/50/75/100% using the GOALS progress data above.\n\
```
(The final rule about valid JSON stays last.)

- [ ] **Step 5: Add the executor match arms**

In the `match parsed_ai_res.action.as_str()` block, after the `"SHARE_BUDGET" => { ... }` arm closes, add:
```rust
        "CREATE_GOAL" => {
            if let Some(bid) = active_budget_id {
                if let Some(params) = &parsed_ai_res.action_params {
                    if let (Some(g_name), Some(t_amt)) = (&params.goal_name, params.target_amount) {
                        let g_type = match params.goal_type.as_deref() {
                            Some("debt") => "debt",
                            _ => "savings",
                        };
                        let t_date = params.target_date.as_deref()
                            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                        let linked = if let Some(cat) = &params.linked_category {
                            sqlx::query("SELECT id FROM categories WHERE budget_id = $1 AND LOWER(name) = LOWER($2)")
                                .bind(bid)
                                .bind(cat)
                                .fetch_optional(&state.db)
                                .await
                                .ok()
                                .flatten()
                                .map(|r| r.get::<Uuid, &str>("id"))
                        } else {
                            None
                        };
                        if let Ok(goal) = crate::goals::insert_goal(&state.db, bid, g_name, g_type, t_amt, t_date, linked).await {
                            mutation_log = Some(format!("Created {} goal '{}' targeting ${:.2}", g_type, goal.name, t_amt));
                            log_audit(&state.db, bid, user_id, "AI_CREATE_GOAL", &mutation_log.clone().unwrap()).await;
                        }
                    }
                }
            }
        }
        "ADD_GOAL_CONTRIBUTION" => {
            if let Some(bid) = active_budget_id {
                if let Some(params) = &parsed_ai_res.action_params {
                    if let (Some(g_name), Some(amt)) = (&params.goal_name, params.amount) {
                        if let Ok(Some(goal)) = crate::goals::find_goal_by_name(&state.db, bid, g_name).await {
                            let note = params.note.clone();
                            if crate::goals::insert_contribution(&state.db, goal.id, user_id, amt, note.as_deref()).await.is_ok() {
                                mutation_log = Some(format!("Added ${:.2} toward goal '{}'", amt, goal.name));
                                log_audit(&state.db, bid, user_id, "AI_ADD_GOAL_CONTRIBUTION", &mutation_log.clone().unwrap()).await;
                            }
                        }
                    }
                }
            }
        }
```

- [ ] **Step 6: Type-check and let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -8`
Expected: `Finished` with no errors.
Then: `touch src/main.rs && sleep 8`

- [ ] **Step 7: Verify chat round-trip in offline mode** (create a goal and contribute via chat, then confirm progress through REST)
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="goalchat-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
TOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
BUDGET=$(curl -s $BASE/api/budgets -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")

echo "--- chat: create goal ---"
curl -s -X POST $BASE/api/chat -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"message\":\"Save \$1000 for Vacation\",\"budget_id\":\"$BUDGET\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['response'])"
echo "--- chat: contribute ---"
curl -s -X POST $BASE/api/chat -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"message\":\"I put \$400 toward Vacation\",\"budget_id\":\"$BUDGET\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['response'])"
echo "--- REST: confirm goal progress (expect current 400, percent 40) ---"
curl -s $BASE/api/budgets/$BUDGET/goals -H "Authorization: Bearer $TOKEN" \
  | python3 -c "import sys,json;g=[x for x in json.load(sys.stdin) if x['name'].lower()=='vacation'][0];print('current',g['current_amount'],'percent',g['percent'])"
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc "DELETE FROM users WHERE email='$EMAIL';"
```
Expected final line:
```
current 400.0 percent 40.0
```

- [ ] **Step 8: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/rag.rs
git commit -m "feat(goals): add CREATE_GOAL and ADD_GOAL_CONTRIBUTION chat actions"
```

---

## Task 9: Frontend discoverability prompt-chip

**Files:**
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Add a goal hint-chip**

In `frontend/src/App.svelte`, in the "Hint chips" block, after the "How can I save?" button (the `</button>` that closes `handlePromptChip("How can I save more money next month?")`), and before the closing `</div>` of the chips row, add:
```svelte
              <button
                onclick={() =>
                  handlePromptChip("Save $3000 for a vacation by December")}
                class="bg-slate-900 border border-slate-800 text-slate-400 hover:text-white px-2 py-1 rounded-md whitespace-nowrap"
                >"Set a savings goal"</button
              >
```

- [ ] **Step 2: Verify the frontend builds**

Run: `cd /home/robhicks/dev/nels/frontend && pnpm run build 2>&1 | tail -8`
Expected: build completes with no errors (a `dist/` is produced). The Vite dev server on `:5173` hot-reloads automatically; no restart needed.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add frontend/src/App.svelte
git commit -m "feat(goals): add savings-goal prompt chip"
```

---

## Task 10: Update Goal.md status + final verification

**Files:**
- Modify: `Goal.md`

- [ ] **Step 1: Mark Pillar 4 as done in `Goal.md`**

In the status table, change the Pillar 4 row from:
```
| 4. Goal Setting & Tracking | 🔴 Missing | Entirely unbuilt — no goals table/routes/AI actions/UI. (`category_limit` is a spending cap, not a goal.) |
```
to:
```
| 4. Goal Setting & Tracking | ✅ Done (chat-only) | Goals (savings & debt) per budget with hybrid progress (contributions ledger + optional linked category). REST routes + conversational CREATE_GOAL / ADD_GOAL_CONTRIBUTION actions; AI reports progress and celebrates milestones. No dedicated UI yet (chat-only). |
```
And in "Suggested next work", change:
```
2. Build Pillar 4 (Goals) — the only fully-missing pillar. **← next**
```
to:
```
2. ~~Build Pillar 4 (Goals)~~ ✅ done (chat-only; no dedicated UI).
```

- [ ] **Step 2: Full backend test + check**

Run: `cd /home/robhicks/dev/nels/backend && cargo test --color never 2>&1 | tail -6 && cargo check --color never 2>&1 | tail -3`
Expected: goals unit tests pass; `Finished` with no errors.

- [ ] **Step 3: Confirm no stray test data remains**

Run:
```bash
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "SELECT 'test users: '||count(*) FROM users WHERE email LIKE 'goal%@example.com';"
```
Expected: `test users: 0` (if not zero, run `DELETE FROM users WHERE email LIKE 'goal%@example.com';`).

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add Goal.md
git commit -m "docs(goals): mark Pillar 4 complete in Goal.md"
```

- [ ] **Step 5: Push (only if the user has asked to push; otherwise leave local)**

```bash
cd /home/robhicks/dev/nels && git push origin main
```

---

## Spec Coverage Check

- Data model (goals, goal_contributions) → Task 1, 2 ✓
- Hybrid progress (contributions + linked category) → Task 4 `goal_progress` ✓
- Savings & debt types → Task 5 validation + Task 8 offline/Gemini ✓
- Per-budget scope + sharing permissions → Task 5/6 `check_permission` + Task 6 Step 5 ✓
- REST routes (7) → Task 5, 6 ✓
- Reusable helpers for rag → Task 4 ✓
- GOALS context block → Task 7 ✓
- CREATE_GOAL / ADD_GOAL_CONTRIBUTION (Gemini + offline + executors) → Task 8 ✓
- Motivation/milestones (prompt-driven, no table) → Task 8 Step 4 ✓
- Frontend chat-only + one chip → Task 9 ✓
- Audit logging → Task 5/8 `log_audit` ✓
- Testing (cargo check, unit tests, curl REST, chat round-trip, cleanup) → Tasks 3/6/7/8/10 ✓
- Goal.md status → Task 10 ✓
```
