# Pillar 2 — Reminders & Budget-Limit Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Alert users in-app when logging an expense crosses a category/budget limit (80%/100%), and let them set recurring reminders that a background task fires into the same feed — all surfaced via REST and the chat.

**Architecture:** A new `notifications.rs` module owns an in-app notification feed and a reminders table. Limit checks run after each transaction insert (REST + chat) and return alert strings; a 60s tokio task fires due reminders into the feed. Notifications/reminders are user-scoped. Pure threshold/cadence logic is unit-tested; everything else is integration-verified against the live `bacon` backend.

**Tech Stack:** Rust (Axum, SQLx runtime queries, chrono, tokio), PostgreSQL, Svelte 5. Dev server runs under `bacon` on `:3000`; DB (pgvector) on `:6153`; `python3`+`pyotp` for TOTP. Reference spec: `docs/superpowers/specs/2026-06-09-reminders-notifications-design.md`.

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `backend/migrations/20260609030000_notifications.sql` | `notifications` + `reminders` tables | Create |
| `backend/src/db.rs` | `Notification`, `Reminder` row structs | Modify |
| `backend/src/notifications.rs` | pure threshold/cadence fns, limit checking, CRUD, scheduler fn, REST handlers | Create |
| `backend/src/main.rs` | `mod notifications;` + routes + 60s scheduler task | Modify |
| `backend/src/budget.rs` | `create_transaction` returns alerts + runs limit check | Modify |
| `backend/src/rag.rs` | notifications context, ADD_TRANSACTION alert append, CREATE_REMINDER action | Modify |
| `frontend/src/App.svelte` | one reminder prompt-chip | Modify |
| `Goal.md` | mark Pillar 2 status | Modify |

**Verification reality:** No DB-backed test harness exists; runtime SQLx queries mean no compile-time DB. Pure functions (`limit_level`, `advance`) get real unit tests (Task 3). Everything else is integration-verified via `cargo check` + curl + chat round-trips against the live `bacon` server (touch a backend source file → wait ~8s for relink). The chat HTTP response JSON field is **`response`** (not `response_text`).

---

## Task 1: Migration for notifications + reminders

**Files:**
- Create: `backend/migrations/20260609030000_notifications.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Pillar 2: in-app notification feed (budget-limit alerts + fired reminders)
-- and user-defined recurring reminders.

CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE,
    kind VARCHAR(20) NOT NULL, -- 'limit_warning' | 'limit_exceeded' | 'reminder'
    message TEXT NOT NULL,
    is_read BOOLEAN NOT NULL DEFAULT FALSE,
    dedup_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS notifications_user_id_idx ON notifications (user_id, created_at DESC);
-- Each (user, dedup_key) fires once; reminder rows use NULL dedup_key (always inserted).
CREATE UNIQUE INDEX IF NOT EXISTS notifications_dedup_idx
    ON notifications (user_id, dedup_key) WHERE dedup_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS reminders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE,
    message TEXT NOT NULL,
    cadence VARCHAR(20) NOT NULL, -- 'daily' | 'weekly' | 'monthly'
    next_fire_at TIMESTAMPTZ NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS reminders_due_idx ON reminders (next_fire_at) WHERE is_active;
```

- [ ] **Step 2: Apply via bacon restart and verify tables exist**

Run:
```bash
cd /home/robhicks/dev/nels/backend && touch src/main.rs && sleep 8
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('notifications','reminders') ORDER BY table_name;"
```
Expected output includes both `notifications` and `reminders`.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/migrations/20260609030000_notifications.sql
git commit -m "feat(notifications): add notifications and reminders tables"
```

---

## Task 2: Row structs in db.rs

**Files:**
- Modify: `backend/src/db.rs`

- [ ] **Step 1: Append the two structs to the END of `backend/src/db.rs`** (the file already imports `DateTime`, `Utc`, `NaiveDate`, `Uuid`, serde, `FromRow`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub is_read: bool,
    pub dedup_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Reminder {
    pub id: Uuid,
    pub user_id: Uuid,
    pub budget_id: Option<Uuid>,
    pub message: String,
    pub cadence: String,
    pub next_fire_at: DateTime<Utc>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -5`
Expected: `Finished`, no errors. "never constructed" warnings on the new structs are acceptable until later tasks.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/db.rs
git commit -m "feat(notifications): add Notification and Reminder row structs"
```

---

## Task 3: Pure threshold + cadence helpers (TDD)

**Files:**
- Create: `backend/src/notifications.rs`
- Modify: `backend/src/main.rs` (add `mod notifications;`)

- [ ] **Step 1: Register the module in `backend/src/main.rs`**

Find the module declarations near the top:
```rust
mod db;
mod auth;
mod budget;
mod goals;
mod r#rag;
```
Add `mod notifications;` after `mod goals;`:
```rust
mod db;
mod auth;
mod budget;
mod goals;
mod notifications;
mod r#rag;
```

- [ ] **Step 2: Create `backend/src/notifications.rs` with pure fns + failing tests**

```rust
use chrono::{DateTime, Utc, Duration};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LimitLevel {
    Approaching,
    Exceeded,
}

/// Which limit threshold `spent` has reached against an optional `limit`.
/// None when there is no positive limit or spending is below 80%.
pub fn limit_level(spent: f64, limit: Option<f64>) -> Option<LimitLevel> {
    let lim = limit?;
    if lim <= 0.0 {
        return None;
    }
    if spent >= lim {
        Some(LimitLevel::Exceeded)
    } else if spent >= 0.8 * lim {
        Some(LimitLevel::Approaching)
    } else {
        None
    }
}

/// Advance a fire time by one cadence step (daily/weekly/monthly; default daily).
pub fn advance(from: DateTime<Utc>, cadence: &str) -> DateTime<Utc> {
    let days = match cadence {
        "weekly" => 7,
        "monthly" => 30,
        _ => 1,
    };
    from + Duration::days(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn level_none_without_limit() {
        assert_eq!(limit_level(500.0, None), None);
    }

    #[test]
    fn level_none_for_nonpositive_limit() {
        assert_eq!(limit_level(50.0, Some(0.0)), None);
    }

    #[test]
    fn level_none_below_80pct() {
        assert_eq!(limit_level(50.0, Some(100.0)), None);
    }

    #[test]
    fn level_approaching_at_80pct() {
        assert_eq!(limit_level(80.0, Some(100.0)), Some(LimitLevel::Approaching));
    }

    #[test]
    fn level_exceeded_at_limit() {
        assert_eq!(limit_level(100.0, Some(100.0)), Some(LimitLevel::Exceeded));
        assert_eq!(limit_level(120.0, Some(100.0)), Some(LimitLevel::Exceeded));
    }

    #[test]
    fn advance_steps() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(advance(t, "daily"), t + Duration::days(1));
        assert_eq!(advance(t, "weekly"), t + Duration::days(7));
        assert_eq!(advance(t, "monthly"), t + Duration::days(30));
        assert_eq!(advance(t, "bogus"), t + Duration::days(1));
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cd /home/robhicks/dev/nels/backend && cargo test --color never notifications:: 2>&1 | tail -8`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/notifications.rs backend/src/main.rs
git commit -m "feat(notifications): add limit-threshold and cadence helpers with tests"
```

---

## Task 4: DB helpers in notifications.rs

**Files:**
- Modify: `backend/src/notifications.rs`

- [ ] **Step 1: Add imports + helper functions** — insert the imports at the TOP of `backend/src/notifications.rs` (above the existing `use chrono::...` line; REPLACE that line so chrono is imported once), and add the functions below `advance` (above the `#[cfg(test)]` module).

Replace the existing first line:
```rust
use chrono::{DateTime, Utc, Duration};
```
with:
```rust
use axum::{
    extract::{Path, State, Extension, Query},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;
use chrono::{DateTime, Utc, Duration};

use crate::auth::AppState;
use crate::budget::{check_permission, Permission};
use crate::db::{Notification, Reminder};
```

Add these functions below `advance` (above `#[cfg(test)]`):
```rust
/// Insert a notification. Limit alerts pass a dedup_key (deduped per user);
/// reminders pass None. Returns Some only when a row was actually inserted.
pub async fn create_notification(
    db: &PgPool,
    user_id: Uuid,
    budget_id: Option<Uuid>,
    kind: &str,
    message: &str,
    dedup_key: Option<&str>,
) -> Option<Notification> {
    sqlx::query_as::<_, Notification>(
        "INSERT INTO notifications (id, user_id, budget_id, kind, message, dedup_key) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (user_id, dedup_key) WHERE dedup_key IS NOT NULL DO NOTHING \
         RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(budget_id)
    .bind(kind)
    .bind(message)
    .bind(dedup_key)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

pub async fn list_notifications(db: &PgPool, user_id: Uuid, unread_only: bool) -> Vec<Notification> {
    let q = if unread_only {
        "SELECT * FROM notifications WHERE user_id = $1 AND is_read = FALSE ORDER BY created_at DESC"
    } else {
        "SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at DESC"
    };
    sqlx::query_as::<_, Notification>(q)
        .bind(user_id)
        .fetch_all(db)
        .await
        .unwrap_or_default()
}

pub async fn mark_read(db: &PgPool, user_id: Uuid, notification_id: Uuid) -> bool {
    sqlx::query("UPDATE notifications SET is_read = TRUE WHERE id = $1 AND user_id = $2")
        .bind(notification_id)
        .bind(user_id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

pub async fn create_reminder(
    db: &PgPool,
    user_id: Uuid,
    budget_id: Option<Uuid>,
    message: &str,
    cadence: &str,
) -> Result<Reminder, String> {
    let cad = match cadence {
        "daily" | "weekly" | "monthly" => cadence,
        _ => "daily",
    };
    let next = advance(Utc::now(), cad);
    sqlx::query_as::<_, Reminder>(
        "INSERT INTO reminders (id, user_id, budget_id, message, cadence, next_fire_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(budget_id)
    .bind(message)
    .bind(cad)
    .bind(next)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())
}

pub async fn list_reminders(db: &PgPool, user_id: Uuid) -> Vec<Reminder> {
    sqlx::query_as::<_, Reminder>(
        "SELECT * FROM reminders WHERE user_id = $1 ORDER BY created_at DESC"
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

pub async fn delete_reminder(db: &PgPool, user_id: Uuid, reminder_id: Uuid) -> bool {
    sqlx::query("DELETE FROM reminders WHERE id = $1 AND user_id = $2")
        .bind(reminder_id)
        .bind(user_id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

/// Fire all due reminders into the notification feed and advance their schedules.
/// Best-effort: errors are ignored (consistent with the auth-cleanup task).
pub async fn fire_due_reminders(db: &PgPool) {
    let due = sqlx::query_as::<_, Reminder>(
        "SELECT * FROM reminders WHERE is_active AND next_fire_at <= NOW()"
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let now = Utc::now();
    for r in due {
        let _ = create_notification(db, r.user_id, r.budget_id, "reminder", &r.message, None).await;
        // Clamp the base to now so a long-overdue reminder fires once and jumps to the
        // next FUTURE slot, instead of burst-firing every 60s tick until it catches up.
        let base = if r.next_fire_at < now { now } else { r.next_fire_at };
        let next = advance(base, &r.cadence);
        let _ = sqlx::query("UPDATE reminders SET next_fire_at = $1 WHERE id = $2")
            .bind(next)
            .bind(r.id)
            .execute(db)
            .await;
    }
}

/// After a transaction is logged, check the category and budget against their
/// limits and create deduped alerts. Returns the messages of NEWLY created
/// alerts (so the caller can surface them immediately).
pub async fn check_and_notify_limits(
    db: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    category_id: Option<Uuid>,
) -> Vec<String> {
    use sqlx::Row;
    let mut alerts = Vec::new();

    // Category-level check
    if let Some(cid) = category_id {
        if let Ok(Some(r)) = sqlx::query(
            "SELECT c.name, c.category_limit, COALESCE(SUM(t.amount), 0)::float8 AS spent \
             FROM categories c LEFT JOIN transactions t ON t.category_id = c.id \
             WHERE c.id = $1 AND c.budget_id = $2 AND c.category_type = 'expense' \
             GROUP BY c.name, c.category_limit"
        )
        .bind(cid)
        .bind(budget_id)
        .fetch_optional(db)
        .await
        {
            let name: String = r.get("name");
            let limit: Option<f64> = r.get("category_limit");
            let spent: f64 = r.get("spent");
            if let Some(level) = limit_level(spent, limit) {
                let lim = limit.unwrap_or(0.0);
                let (kind, dedup, msg) = match level {
                    LimitLevel::Exceeded => (
                        "limit_exceeded",
                        format!("limit:{}:cat:{}:exceeded", budget_id, cid),
                        format!("🚫 Category '{}' has exceeded its ${:.2} limit (${:.2} spent).", name, lim, spent),
                    ),
                    LimitLevel::Approaching => (
                        "limit_warning",
                        format!("limit:{}:cat:{}:approaching", budget_id, cid),
                        format!("⚠️ Category '{}' is at {:.0}% of its ${:.2} limit (${:.2} spent).", name, spent / lim * 100.0, lim, spent),
                    ),
                };
                if create_notification(db, user_id, Some(budget_id), kind, &msg, Some(&dedup)).await.is_some() {
                    alerts.push(msg);
                }
            }
        }
    }

    // Budget-level check (sum only expense-category transactions)
    if let Ok(Some(r)) = sqlx::query(
        "SELECT b.name, b.budget_limit, COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM budgets b \
         LEFT JOIN categories c ON c.budget_id = b.id AND c.category_type = 'expense' \
         LEFT JOIN transactions t ON t.category_id = c.id \
         WHERE b.id = $1 \
         GROUP BY b.name, b.budget_limit"
    )
    .bind(budget_id)
    .fetch_optional(db)
    .await
    {
        let name: String = r.get("name");
        let limit: f64 = r.get("budget_limit");
        let spent: f64 = r.get("spent");
        if let Some(level) = limit_level(spent, Some(limit)) {
            let (kind, dedup, msg) = match level {
                LimitLevel::Exceeded => (
                    "limit_exceeded",
                    format!("limit:{}:budget:exceeded", budget_id),
                    format!("🚫 Budget '{}' has exceeded its ${:.2} limit (${:.2} spent).", name, limit, spent),
                ),
                LimitLevel::Approaching => (
                    "limit_warning",
                    format!("limit:{}:budget:approaching", budget_id),
                    format!("⚠️ Budget '{}' is at {:.0}% of its ${:.2} limit (${:.2} spent).", name, spent / limit * 100.0, limit, spent),
                ),
            };
            if create_notification(db, user_id, Some(budget_id), kind, &msg, Some(&dedup)).await.is_some() {
                alerts.push(msg);
            }
        }
    }

    alerts
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -8`
Expected: `Finished`, no errors. Unused-import warnings for `Path`/`State`/`Extension`/`Query`/`Json`/`StatusCode`/`Deserialize`/`AppState` are EXPECTED here (the REST handlers in Task 5 use them).

- [ ] **Step 3: Confirm unit tests still pass**

Run: `cd /home/robhicks/dev/nels/backend && cargo test --color never notifications:: 2>&1 | tail -3`
Expected: `6 passed`.

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/notifications.rs
git commit -m "feat(notifications): add feed/reminder DB helpers and limit checking"
```

---

## Task 5: REST handlers in notifications.rs

**Files:**
- Modify: `backend/src/notifications.rs`

- [ ] **Step 1: Add payloads + handlers** below the helpers from Task 4 (above `#[cfg(test)]`):

```rust
#[derive(Deserialize)]
pub struct NotifQuery {
    pub unread: Option<bool>,
}

pub async fn list_notifications_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Query(q): Query<NotifQuery>,
) -> Result<Json<Vec<Notification>>, (StatusCode, String)> {
    Ok(Json(list_notifications(&state.db, user_id, q.unread.unwrap_or(false)).await))
}

pub async fn mark_notification_read(
    State(state): State<AppState>,
    Path(notification_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    if mark_read(&state.db, user_id, notification_id).await {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::NOT_FOUND, "Notification not found".to_string()))
    }
}

#[derive(Deserialize)]
pub struct CreateReminderPayload {
    pub message: String,
    pub cadence: String,
    pub budget_id: Option<Uuid>,
}

pub async fn create_reminder_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CreateReminderPayload>,
) -> Result<Json<Reminder>, (StatusCode, String)> {
    match payload.cadence.as_str() {
        "daily" | "weekly" | "monthly" => {}
        _ => return Err((StatusCode::BAD_REQUEST, "cadence must be daily, weekly, or monthly".to_string())),
    }
    // If the reminder references a budget, the caller must have access to it — never
    // let a user plant another tenant's budget id on their reminder (no cross-tenant FK).
    if let Some(bid) = payload.budget_id {
        let perm = check_permission(&state.db, user_id, bid)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if perm == Permission::None {
            return Err((StatusCode::FORBIDDEN, "No access to that budget".to_string()));
        }
    }
    let rem = create_reminder(&state.db, user_id, payload.budget_id, &payload.message, &payload.cadence)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create reminder: {}", e)))?;
    Ok(Json(rem))
}

pub async fn list_reminders_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<Reminder>>, (StatusCode, String)> {
    Ok(Json(list_reminders(&state.db, user_id).await))
}

pub async fn delete_reminder_handler(
    State(state): State<AppState>,
    Path(reminder_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    if delete_reminder(&state.db, user_id, reminder_id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "Reminder not found".to_string()))
    }
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -6`
Expected: `Finished`, no errors. Handlers warn "never used" until routed in Task 6 — acceptable.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/notifications.rs
git commit -m "feat(notifications): add REST handlers for notifications and reminders"
```

---

## Task 6: Wire routes + reminder scheduler in main.rs

**Files:**
- Modify: `backend/src/main.rs`

- [ ] **Step 1: Import the handlers** — after the existing `use goals::{ ... };` block in `backend/src/main.rs`, add:
```rust
use notifications::{
    list_notifications_handler, mark_notification_read,
    create_reminder_handler, list_reminders_handler, delete_reminder_handler,
};
```

- [ ] **Step 2: Register routes** — in the `protected_routes` Router, after the existing chat routes line `.route("/chat/:budget_id/history", get(list_chat_history))`, add (before the `.layer(...)`):
```rust

        .route("/notifications", get(list_notifications_handler))
        .route("/notifications/:id/read", post(mark_notification_read))
        .route("/reminders", post(create_reminder_handler).get(list_reminders_handler))
        .route("/reminders/:id", delete(delete_reminder_handler))
```

- [ ] **Step 3: Add the 60s reminder scheduler** — find the existing hourly auth-cleanup `tokio::spawn` block (it spawns a task using `auth::purge_expired_auth_state`). Immediately AFTER that block, add:
```rust
    // Background task: fire due reminders into the notification feed every 60s.
    let reminder_pool = state.db.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            notifications::fire_due_reminders(&reminder_pool).await;
        }
    });
```
(`Duration` is already imported via `use std::time::Duration;`.)

- [ ] **Step 4: Type-check + let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -5`
Expected: `Finished`, no errors. Then `touch src/main.rs && sleep 8`; confirm `ss -tlnp | grep :3000`.

- [ ] **Step 5: Verify reminders CRUD + scheduler end-to-end** (this test waits ~65s for one scheduler tick):
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="rem-test-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
TOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")

echo "--- create reminder ---"
REM=$(curl -s -X POST $BASE/api/reminders -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"message":"log your expenses","cadence":"daily"}')
echo "$REM"
RID=$(echo "$REM" | python3 -c "import sys,json;print(json.load(sys.stdin)['id'])")

echo "--- list reminders (expect 1) ---"
curl -s $BASE/api/reminders -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print('count', len(json.load(sys.stdin)))"

echo "--- backdate next_fire_at by 1 minute and wait ~65s for the scheduler ---"
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "UPDATE reminders SET next_fire_at = NOW() - INTERVAL '1 minute' WHERE id='$RID';"
sleep 65

echo "--- notifications now include the fired reminder ---"
curl -s "$BASE/api/notifications" -H "Authorization: Bearer $TOKEN" \
  | python3 -c "import sys,json;n=json.load(sys.stdin);print('reminder fired:', any(x['kind']=='reminder' and 'log your expenses' in x['message'] for x in n))"

echo "--- reminder next_fire_at advanced into the future ---"
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "SELECT next_fire_at > NOW() AS advanced FROM reminders WHERE id='$RID';"

echo "--- delete reminder (expect 204) ---"
curl -s -o /dev/null -w "delete HTTP %{http_code}\n" -X DELETE $BASE/api/reminders/$RID -H "Authorization: Bearer $TOKEN"

PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc "DELETE FROM users WHERE email='$EMAIL';"
```
Expected: `reminder fired: True`, `advanced` = `t`, `delete HTTP 204`. If `reminder fired` is False, confirm the scheduler task is running (it logs nothing; check the server recompiled and that `next_fire_at` was backdated). Do NOT weaken the assertion.

- [ ] **Step 6: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/main.rs
git commit -m "feat(notifications): register routes and reminder scheduler task"
```

---

## Task 7: Budget-limit check in create_transaction (REST)

**Files:**
- Modify: `backend/src/budget.rs`

- [ ] **Step 1: Add the response wrapper struct** — in `backend/src/budget.rs`, immediately after the existing `pub struct TransactionResponse { ... }` definition, add:
```rust
#[derive(Serialize)]
pub struct CreateTransactionResponse {
    pub transaction: TransactionResponse,
    pub alerts: Vec<String>,
}
```

- [ ] **Step 2: Change `create_transaction` to run the limit check and return the wrapper.** The handler currently ends by returning `Ok(Json(TransactionResponse { ... }))`. Change its return type and final return:

Change the signature return type from:
```rust
) -> Result<Json<TransactionResponse>, (StatusCode, String)> {
```
to:
```rust
) -> Result<Json<CreateTransactionResponse>, (StatusCode, String)> {
```

Change the final `Ok(Json(TransactionResponse { ... }))` block from:
```rust
    Ok(Json(TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
    }))
```
to:
```rust
    let response = TransactionResponse {
        id: transaction.id,
        budget_id: transaction.budget_id,
        category_id: transaction.category_id,
        category_name: cat_name,
        category_type: cat_type,
        amount: transaction.amount,
        description: transaction.description,
        transaction_date: transaction.transaction_date,
        created_at: transaction.created_at,
    };

    let alerts = crate::notifications::check_and_notify_limits(
        &state.db, user_id, budget_id, response.category_id,
    ).await;

    Ok(Json(CreateTransactionResponse { transaction: response, alerts }))
```

- [ ] **Step 3: Type-check + let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -6`
Expected: `Finished`, no errors. Then `touch src/main.rs && sleep 8`.

- [ ] **Step 4: Verify limit alerts + dedup + mark-read via curl:**
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="limit-test-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
TOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
BUDGET=$(curl -s $BASE/api/budgets -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")

# Category with a $100 limit
CAT=$(curl -s -X POST $BASE/api/budgets/$BUDGET/categories -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"Food","category_type":"expense","category_limit":100}' | python3 -c "import sys,json;print(json.load(sys.stdin)['id'])")

logtx() { curl -s -X POST $BASE/api/budgets/$BUDGET/transactions -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"category_id\":\"$CAT\",\"amount\":$1,\"description\":\"x\"}"; }

echo "--- log \$95 (expect an 'approaching' alert) ---"
logtx 95 | python3 -c "import sys,json;a=json.load(sys.stdin)['alerts'];print('alerts:',a);print('approaching:', any('⚠️' in x for x in a))"
echo "--- log \$20 -> total 115 (expect an 'exceeded' alert) ---"
logtx 20 | python3 -c "import sys,json;a=json.load(sys.stdin)['alerts'];print('alerts:',a);print('exceeded:', any('🚫' in x for x in a))"
echo "--- log \$5 -> total 120 (expect NO new alert, dedup) ---"
logtx 5 | python3 -c "import sys,json;a=json.load(sys.stdin)['alerts'];print('alerts:',a);print('empty:', len(a)==0)"

echo "--- notifications feed (expect 2: approaching + exceeded) ---"
curl -s $BASE/api/notifications -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;n=json.load(sys.stdin);print('count', len(n))"

echo "--- mark first notification read (expect 200), then unread count drops ---"
NID=$(curl -s $BASE/api/notifications -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")
curl -s -o /dev/null -w "mark-read HTTP %{http_code}\n" -X POST $BASE/api/notifications/$NID/read -H "Authorization: Bearer $TOKEN"
curl -s "$BASE/api/notifications?unread=true" -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print('unread now', len(json.load(sys.stdin)))"

PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc "DELETE FROM users WHERE email='$EMAIL';"
```
Expected: `approaching: True`, `exceeded: True`, `empty: True`, notifications `count 2`, `mark-read HTTP 200`, `unread now 1`. Do NOT weaken assertions; if a check fails, print raw responses and debug.

- [ ] **Step 5: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/budget.rs
git commit -m "feat(notifications): emit budget-limit alerts on transaction logging"
```

---

## Task 8: Chat integration in rag.rs (context + alerts + CREATE_REMINDER)

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Add `reminder_message` and `cadence` to `AiActionParams`.** The struct currently ends with `note: Option<String>,`. Add after it:
```rust
    note: Option<String>,
    reminder_message: Option<String>,
    cadence: Option<String>,         // "daily", "weekly", "monthly"
```

- [ ] **Step 2: Initialize the new fields in the offline mock params.** Find the `let mut action_params = AiActionParams { ... };` initializer in the offline branch; it currently ends with `goal_name: None, goal_type: None, target_amount: None, target_date: None, linked_category: None, note: None,`. Change that tail to also set the two new fields:
```rust
            goal_name: None, goal_type: None, target_amount: None, target_date: None,
            linked_category: None, note: None,
            reminder_message: None, cadence: None,
        };
```

- [ ] **Step 3: Add a reminder offline-matcher branch as the FIRST branch.** The offline `if/else if` chain currently begins with the goals branch: `if msg_lower.contains("goal") || msg_lower.contains("save for") ...`. Prepend a reminder branch and turn that goals `if` into `else if`. Change:
```rust
        if msg_lower.contains("goal") || msg_lower.contains("save for") || msg_lower.contains("saving for")
```
to:
```rust
        if msg_lower.contains("remind me") {
            action = "CREATE_REMINDER".to_string();
            let cadence = if msg_lower.contains("week") {
                "weekly"
            } else if msg_lower.contains("month") {
                "monthly"
            } else {
                "daily"
            };
            // reminder text = whatever follows " to ", else a sensible default
            let mut rmsg = "log your expenses".to_string();
            if let Some(idx) = msg_lower.find(" to ") {
                let tail = msg_lower[idx + 4..].trim().trim_matches(['"', '\'']);
                if !tail.is_empty() {
                    rmsg = tail.to_string();
                }
            }
            action_params.reminder_message = Some(rmsg.clone());
            action_params.cadence = Some(cadence.to_string());
            response_text = format!("(Mock AI Offline Mode) Okay — I'll remind you {} to {}.", cadence, rmsg);

        } else if msg_lower.contains("goal") || msg_lower.contains("save for") || msg_lower.contains("saving for")
```

- [ ] **Step 4: Add CREATE_REMINDER to the Gemini system prompt.** In the system-instructions `format!`, update the action enum line — change:
```rust
           \"action\": \"NONE\" | \"CREATE_BUDGET\" | \"UPDATE_BUDGET\" | \"CREATE_CATEGORY\" | \"ADD_TRANSACTION\" | \"SHARE_BUDGET\" | \"CREATE_GOAL\" | \"ADD_GOAL_CONTRIBUTION\",\n\
```
to:
```rust
           \"action\": \"NONE\" | \"CREATE_BUDGET\" | \"UPDATE_BUDGET\" | \"CREATE_CATEGORY\" | \"ADD_TRANSACTION\" | \"SHARE_BUDGET\" | \"CREATE_GOAL\" | \"ADD_GOAL_CONTRIBUTION\" | \"CREATE_REMINDER\",\n\
```
In the `action_params` JSON shape, find the `note` line (`\"note\": \"string (optional)\"\n\`) and add the reminder params after it. Change:
```rust
             \"note\": \"string (optional)\"\n\
```
to:
```rust
             \"note\": \"string (optional)\",\n\
             \"reminder_message\": \"string (optional)\",\n\
             \"cadence\": \"daily\" | \"weekly\" | \"monthly\" (optional)\n\
```
Then add a reminder rule. The prompt's current LAST numbered rule is already `9. ALWAYS produce perfectly clean, valid, parseable JSON only.` — so FIRST renumber that existing rule from 9 to 10. Change:
```rust
         9. ALWAYS produce perfectly clean, valid, parseable JSON only.",
```
to:
```rust
         10. ALWAYS produce perfectly clean, valid, parseable JSON only.",
```
Then insert the new reminder rule as rule `9.` immediately before it (after the goal-celebration rule `8.`):
```rust
         9. If the user wants a recurring reminder (e.g. 'remind me every day to log expenses'), set 'action' to 'CREATE_REMINDER'. Populate 'reminder_message' with what to remind them, and 'cadence' ('daily', 'weekly', or 'monthly').\n\
```

- [ ] **Step 5: Append limit alerts after ADD_TRANSACTION, and add a CREATE_REMINDER executor.**

First, declare an `alerts` accumulator. Find the line that declares the mutation accumulator before the `match` (it reads `let mut mutation_log = None;` followed by `let mut action_outcome_budget_id = active_budget_id;`). Immediately after `let mut mutation_log = None;` add:
```rust
    let mut alerts: Vec<String> = Vec::new();
```

In the `"ADD_TRANSACTION" => { ... }` arm, the success block currently reads:
```rust
                        if tx_row.is_ok() {
                            mutation_log = Some(format!("Logged expense of ${:.2} under '{}' for '{}'", amt, c_name, desc));
                            log_audit(&state.db, bid, user_id, "AI_ADD_TRANSACTION", &mutation_log.clone().unwrap()).await;
                        }
```
Change it to also run the limit check:
```rust
                        if tx_row.is_ok() {
                            mutation_log = Some(format!("Logged expense of ${:.2} under '{}' for '{}'", amt, c_name, desc));
                            log_audit(&state.db, bid, user_id, "AI_ADD_TRANSACTION", &mutation_log.clone().unwrap()).await;
                            alerts.extend(crate::notifications::check_and_notify_limits(&state.db, user_id, bid, category_id).await);
                        }
```

Add a new executor arm. Find the `"UPDATE_BUDGET" => { ... }` arm and the `_ => {}` that follows it. Insert the CREATE_REMINDER arm BEFORE `_ => {}`:
```rust
        "CREATE_REMINDER" => {
            if let Some(params) = &parsed_ai_res.action_params {
                if let Some(msg) = &params.reminder_message {
                    let cadence = params.cadence.clone().unwrap_or_else(|| "daily".to_string());
                    if let Ok(rem) = crate::notifications::create_reminder(&state.db, user_id, active_budget_id, msg, &cadence).await {
                        mutation_log = Some(format!("Set a {} reminder: {}", rem.cadence, rem.message));
                        if let Some(bid) = active_budget_id {
                            log_audit(&state.db, bid, user_id, "AI_CREATE_REMINDER", &mutation_log.clone().unwrap()).await;
                        }
                    }
                }
            }
        }
```

Finally, append alerts to the response. The post-match block currently reads:
```rust
    let mut final_response_text = parsed_ai_res.response_text;
    if let Some(log) = &mutation_log {
        final_response_text = format!("{}\n\n**System Update:** ✅ {}", final_response_text, log);
    }
```
Add the alert lines right after it:
```rust
    for a in &alerts {
        final_response_text = format!("{}\n{}", final_response_text, a);
    }
```

- [ ] **Step 6: Inject unread notifications into the AI context.** Find where `history_context` is fully built (the block that pushes "RECENT CHAT HISTORY:" lines, ending the `if let Some(bid) = active_budget_id { ... }` for history). Immediately AFTER that block (and before the `// 6. Assembly System Instructions` comment / the `system_instructions = format!(` call), add:
```rust
    // Unread notifications (user-scoped) prepended to history context so the AI can mention them.
    let unread = crate::notifications::list_notifications(&state.db, user_id, true).await;
    if !unread.is_empty() {
        let mut nblock = format!("UNREAD NOTIFICATIONS ({}):\n", unread.len());
        for n in unread.iter().take(5) {
            nblock.push_str(&format!("- {}\n", n.message));
        }
        history_context = format!("{}\n{}", nblock, history_context);
    }
```
Note: this requires `history_context` to be mutable. It is declared `let mut history_context = String::new();` earlier, so reassigning works. If the compiler complains it is not mutable, change its declaration to `let mut history_context`.

- [ ] **Step 7: Type-check + let bacon restart**

Run: `cd /home/robhicks/dev/nels/backend && cargo check --color never 2>&1 | tail -8`
Expected: `Finished`, no errors. Then `touch src/main.rs && sleep 8`.

- [ ] **Step 8: Verify chat round-trips** (offline mode):
```bash
cd /home/robhicks/dev/nels/backend
BASE=http://127.0.0.1:3000
EMAIL="chatrem-$(date +%s)@example.com"
S=$(curl -s -X POST $BASE/api/auth/register/start -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\"}")
FLOW=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['flow_id'])")
SECRET=$(echo "$S" | python3 -c "import sys,json;print(json.load(sys.stdin)['secret'])")
CODE=$(python3 -c "import pyotp;print(pyotp.TOTP('$SECRET').now())")
TOKEN=$(curl -s -X POST $BASE/api/auth/register/finish -H 'Content-Type: application/json' -d "{\"flow_id\":\"$FLOW\",\"code\":\"$CODE\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
BUDGET=$(curl -s $BASE/api/budgets -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")

echo "--- chat: create reminder ---"
curl -s -X POST $BASE/api/chat -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"message\":\"remind me weekly to review my budget\",\"budget_id\":\"$BUDGET\"}" | python3 -c "import sys,json;print(json.load(sys.stdin)['response'])"
echo "--- reminder persisted (expect a weekly reminder) ---"
curl -s $BASE/api/reminders -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json;r=json.load(sys.stdin);print('weekly reminder:', any(x['cadence']=='weekly' for x in r))"

echo "--- chat: log an over-limit expense, expect alert appended to reply ---"
curl -s -X POST $BASE/api/budgets/$BUDGET/categories -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"Food","category_type":"expense","category_limit":50}' >/dev/null
curl -s -X POST $BASE/api/chat -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"message\":\"log \$80 spent on food for dinner\",\"budget_id\":\"$BUDGET\"}" \
  | python3 -c "import sys,json;t=json.load(sys.stdin)['response'];print('alert in reply:', ('🚫' in t or '⚠️' in t))"

PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc "DELETE FROM users WHERE email='$EMAIL';"
```
Expected: the reminder reply mentions a weekly reminder; `weekly reminder: True`; `alert in reply: True`. If `alert in reply` is False, print the full `response` and check the offline matcher routed the message to ADD_TRANSACTION and the category limit was applied. Do NOT weaken assertions.

- [ ] **Step 9: Commit**

```bash
cd /home/robhicks/dev/nels
git add backend/src/rag.rs
git commit -m "feat(notifications): surface alerts and reminders in chat"
```

---

## Task 9: Frontend reminder prompt-chip

**Files:**
- Modify: `frontend/src/App.svelte`

- [ ] **Step 1: Add a reminder hint-chip.** In `frontend/src/App.svelte`, in the "Hint chips" block, after the most recently added "Set a savings goal" chip button (the `<button>` whose `onclick` calls `handlePromptChip("Save $3000 for a vacation by December")`), and before the `</div>` that closes the chips row, add:
```svelte
              <button
                onclick={() =>
                  handlePromptChip("Remind me to log expenses daily")}
                class="bg-slate-900 border border-slate-800 text-slate-400 hover:text-white px-2 py-1 rounded-md whitespace-nowrap"
                >"Set a reminder"</button
              >
```
Match the surrounding buttons' indentation and class string.

- [ ] **Step 2: Verify the frontend builds**

Run: `cd /home/robhicks/dev/nels/frontend && pnpm run build 2>&1 | tail -6`
Expected: build completes, writes `dist/`, no errors.

- [ ] **Step 3: Commit**

```bash
cd /home/robhicks/dev/nels
git add frontend/src/App.svelte
git commit -m "feat(notifications): add reminder prompt chip"
```

---

## Task 10: Update Goal.md + final verification

**Files:**
- Modify: `Goal.md`

- [ ] **Step 1: Mark Pillar 2 done in `Goal.md`.** Change the Pillar 2 status-table row from:
```
| 2. Track Expenses | 🟡 Mostly done | Conversational logging + AI auto-categorization (Gemini + offline keyword fallback). **Missing: reminders & budget-limit notifications.** |
```
to:
```
| 2. Track Expenses | ✅ Done | Conversational logging + AI auto-categorization, plus budget-limit notifications (category & budget at 80%/100%, deduped) and recurring reminders (daily/weekly/monthly) fired by a 60s scheduler into an in-app feed. REST: /api/notifications, /api/reminders. Chat: CREATE_REMINDER + inline limit alerts. |
```
And in "Suggested next work", change:
```
3. Round out Pillar 2 (reminders/notifications) and Pillar 3 (structured reports).
```
to:
```
3. ~~Round out Pillar 2 (reminders/notifications)~~ ✅ done. Remaining: Pillar 3 (structured reports/charts).
```
If either exact string isn't found, read Goal.md and make the equivalent change; report what you changed.

- [ ] **Step 2: Full backend test + check**

Run: `cd /home/robhicks/dev/nels/backend && cargo test --color never 2>&1 | tail -6 && cargo check --color never 2>&1 | tail -3`
Expected: all unit tests pass (goals: 7, notifications: 6 = 13 total) and `Finished`, no errors.

- [ ] **Step 3: Confirm no stray test data**

Run:
```bash
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag -tAc \
  "SELECT 'test users: '||count(*) FROM users WHERE email LIKE 'rem-test-%@example.com' OR email LIKE 'limit-test-%@example.com' OR email LIKE 'chatrem-%@example.com';"
```
If not `test users: 0`, delete them with the same WHERE clause via `DELETE FROM users WHERE ...`.

- [ ] **Step 4: Commit**

```bash
cd /home/robhicks/dev/nels
git add Goal.md
git commit -m "docs(notifications): mark Pillar 2 complete in Goal.md"
```

- [ ] **Step 5: Push (only if the user asked to push; otherwise leave local)**

```bash
cd /home/robhicks/dev/nels && git push origin main
```

---

## Spec Coverage Check

- notifications + reminders tables → Task 1, 2 ✓
- pure limit_level / advance + unit tests → Task 3 ✓
- check_and_notify_limits (category + budget, 80/100, dedup) → Task 4, verified Task 7 ✓
- notification feed CRUD + reminder CRUD → Task 4, 5 ✓
- REST routes (user-scoped) → Task 5, 6 ✓
- 60s reminder scheduler → Task 6 ✓
- create_transaction returns alerts → Task 7 ✓
- chat: notifications context + ADD_TRANSACTION alert append + CREATE_REMINDER (Gemini + offline + executor) → Task 8 ✓
- frontend chip → Task 9 ✓
- Goal.md status → Task 10 ✓
- acting-user-only alerts, no period reset (dedup) → encoded in Task 4 ✓

## Accepted Limitations (reviewed & intentional)

These were surfaced by adversarial review and accepted (consistent with the spec's non-goals):
- **Dedup is permanent.** Once a category/budget crosses 80%/100%, that exact threshold never alerts again — even if the user later raises the limit, or deletes transactions so spend drops back under and re-crosses. This follows directly from "no per-period reset" (cumulative spending). If per-period or per-limit-change re-alerting is ever wanted, it's a future enhancement.
- **`create_notification` collapses any DB error into "not inserted"** (`.ok().flatten()`), indistinguishable from a dedup conflict. On a transient DB error during a first crossing, that one logging response omits the alert; the next qualifying transaction re-attempts and succeeds. Acceptable best-effort behavior.
- **NULL-category transactions** count toward neither category nor budget totals (a small undercount of "total spend"). Acceptable — an uncategorized transaction has no expense type.
- **Single-process scheduler.** `fire_due_reminders` has no row-claim/`SKIP LOCKED`; running multiple backend processes would double-fire reminders. Fine for the current single-process deployment; revisit if horizontally scaled.
```
