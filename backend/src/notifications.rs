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
use crate::error::{internal_error, internal_error_message, INTERNAL_ERROR_MESSAGE};

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
    if spent > lim {
        Some(LimitLevel::Exceeded)
    } else if spent >= 0.8 * lim {
        Some(LimitLevel::Approaching)
    } else {
        None
    }
}

/// One of a limit scope's two dedup keys (a scope = one category, or the whole
/// budget). Each scope has exactly one `exceeded` and one `approaching` alert key.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LimitKey {
    Exceeded,
    Approaching,
}

/// DB-free core of limit reconciliation: given the current level (None when the
/// scope is back under threshold), return which of the scope's dedup keys are now
/// STALE and must be deleted. The active level's own key is never returned here —
/// it is upserted (refreshed) separately.
///   None        -> both keys stale (clear the scope entirely)
///   Exceeded    -> the approaching key is stale
///   Approaching -> the exceeded key is stale (a downgrade)
pub fn stale_limit_keys(level: Option<LimitLevel>) -> Vec<LimitKey> {
    match level {
        None => vec![LimitKey::Exceeded, LimitKey::Approaching],
        Some(LimitLevel::Exceeded) => vec![LimitKey::Approaching],
        Some(LimitLevel::Approaching) => vec![LimitKey::Exceeded],
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
    let result = sqlx::query_as::<_, Notification>(
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
    .await;
    match result {
        Ok(row) => row,
        Err(e) => {
            // A dedup conflict is Ok(None), not Err, so this only fires on a genuine
            // DB failure — log it instead of silently dropping the notification (#204).
            tracing::error!(error = %e, %user_id, %kind, "failed to insert notification");
            None
        }
    }
}

/// Delete the given limit-alert dedup rows for one user. Owner-scoped by
/// `user_id`; matches only the exact dedup keys passed. Best-effort: a genuine DB
/// error is logged (not swallowed silently) and never aborts a transaction write.
/// Returns `Some(rows_deleted)` on success (`Some(0)` when nothing matched), or
/// `None` on a DB error. The caller distinguishes these because a genuine
/// zero-rows delete and a failed delete must drive the downgrade decision
/// differently (see [`reconcile_limit_notification`]).
pub async fn delete_limit_notifications(db: &PgPool, user_id: Uuid, dedup_keys: &[&str]) -> Option<u64> {
    if dedup_keys.is_empty() {
        return Some(0);
    }
    // Bind as text[] for `= ANY($2)`. Collect to Vec<String> so the array bind is
    // unambiguous to sqlx regardless of the &str lifetime.
    let keys: Vec<String> = dedup_keys.iter().map(|s| s.to_string()).collect();
    match sqlx::query("DELETE FROM notifications WHERE user_id = $1 AND dedup_key = ANY($2)")
        .bind(user_id)
        .bind(&keys)
        .execute(db)
        .await
    {
        Ok(r) => Some(r.rows_affected()),
        Err(e) => {
            tracing::error!(error = %e, %user_id, keys = ?keys, "failed to delete stale limit notifications");
            None
        }
    }
}

/// Insert a limit alert, or — if one already exists for `(user_id, dedup_key)` —
/// refresh its `message` (and `budget_id`/`kind`) IN PLACE, deliberately leaving
/// `is_read` and `created_at` untouched so a figure update never re-surfaces an
/// already-seen alert. Returns `true` iff a brand-new row was INSERTed (detected
/// via the `xmax = 0` upsert idiom), `false` on an in-place refresh or a DB error.
/// The caller uses the `true` result to decide whether to surface the alert now.
pub async fn upsert_limit_notification(
    db: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    kind: &str,
    dedup_key: &str,
    message: &str,
) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "INSERT INTO notifications (id, user_id, budget_id, kind, message, dedup_key) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (user_id, dedup_key) WHERE dedup_key IS NOT NULL \
         DO UPDATE SET message = EXCLUDED.message, budget_id = EXCLUDED.budget_id, kind = EXCLUDED.kind \
         RETURNING (xmax = 0) AS inserted"
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(Some(budget_id))
    .bind(kind)
    .bind(message)
    .bind(dedup_key)
    .fetch_one(db)
    .await
    {
        Ok(inserted) => inserted,
        Err(e) => {
            tracing::error!(error = %e, %user_id, %kind, "failed to upsert limit notification");
            false
        }
    }
}

/// Reconcile ONE limit scope's two notifications against its current level.
/// `exceeded_key` / `approaching_key` are the scope's dedup keys; `active` is the
/// currently-tripped `(level, rendered_message)`, or `None` when the scope is back
/// under threshold. Deletes whichever keys are now stale (`stale_limit_keys`) and
/// upserts (refreshes) the active alert. Returns `Some(message)` ONLY when the
/// active alert is a genuinely-new trip that should be surfaced now — an insert
/// that is NOT a downgrade from a higher severity (an improvement stays silent).
async fn reconcile_limit_notification(
    db: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    exceeded_key: &str,
    approaching_key: &str,
    active: Option<(LimitLevel, String)>,
) -> Option<String> {
    let level = active.as_ref().map(|(l, _)| *l);
    let stale: Vec<&str> = stale_limit_keys(level)
        .into_iter()
        .map(|k| match k {
            LimitKey::Exceeded => exceeded_key,
            LimitKey::Approaching => approaching_key,
        })
        .collect();
    let removed = delete_limit_notifications(db, user_id, &stale).await;

    match active {
        None => None,
        Some((LimitLevel::Exceeded, msg)) => {
            // Escalation or first trip: always surface a genuinely-new exceeded alert.
            let inserted =
                upsert_limit_notification(db, user_id, budget_id, "limit_exceeded", exceeded_key, &msg).await;
            inserted.then_some(msg)
        }
        Some((LimitLevel::Approaching, msg)) => {
            // If an exceeded alert was just cleared, this is a DOWNGRADE — refresh the
            // approaching alert but keep the improvement silent. `removed` is the
            // stale-exceeded delete result: `Some(0)` means no exceeded row existed
            // (a genuine first-time approaching → surface), `Some(n>0)` is a real
            // downgrade (suppress), and `None` (delete errored) is treated as a
            // possible downgrade — err toward silence so a transient DB hiccup never
            // spuriously re-surfaces an improvement alert.
            let downgraded = removed != Some(0);
            let inserted =
                upsert_limit_notification(db, user_id, budget_id, "limit_warning", approaching_key, &msg).await;
            (inserted && !downgraded).then_some(msg)
        }
    }
}

pub async fn list_notifications(db: &PgPool, user_id: Uuid, unread_only: bool) -> Vec<Notification> {
    let q = if unread_only {
        "SELECT * FROM notifications WHERE user_id = $1 AND is_read = FALSE ORDER BY created_at DESC"
    } else {
        "SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at DESC"
    };
    match sqlx::query_as::<_, Notification>(q)
        .bind(user_id)
        .fetch_all(db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, %user_id, "failed to list notifications");
            Vec::new()
        }
    }
}

pub async fn mark_read(db: &PgPool, user_id: Uuid, notification_id: Uuid) -> Result<bool, String> {
    sqlx::query("UPDATE notifications SET is_read = TRUE WHERE id = $1 AND user_id = $2")
        .bind(notification_id)
        .bind(user_id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| {
            tracing::error!(error = %e, %user_id, %notification_id, "failed to mark notification read");
            INTERNAL_ERROR_MESSAGE.to_string()
        })
}

/// Owner-scoped count of the user's UNREAD notifications. Drives the in-app
/// unread badge (#55). Note the badge the user actually sees is further filtered
/// client-side by the #54 per-category preferences; this raw server count is the
/// total-unread figure exposed by the REST contract. Returns 0 on error so a
/// transient DB hiccup never surfaces a wrong, alarming badge number. The genuine
/// DB error is logged via `tracing::error!` rather than silently swallowed (#213).
pub async fn unread_count(db: &PgPool, user_id: Uuid) -> i64 {
    match sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND is_read = FALSE",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, %user_id, "failed to count unread notifications");
            0
        }
    }
}

/// Mark ALL of the user's unread notifications read (#55, mark-all-read).
/// Owner-scoped: only the caller's rows are touched. Idempotent — re-running
/// with nothing unread affects zero rows. Returns rows affected.
pub async fn mark_all_read(db: &PgPool, user_id: Uuid) -> u64 {
    match sqlx::query("UPDATE notifications SET is_read = TRUE WHERE user_id = $1 AND is_read = FALSE")
        .bind(user_id)
        .execute(db)
        .await
    {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            tracing::error!(error = %e, %user_id, "failed to mark all notifications read");
            0
        }
    }
}

/// Dismiss (delete) a single notification (#55). Owner-scoped: the `user_id`
/// predicate makes it impossible to delete another tenant's row — a mismatched
/// id deletes nothing and returns `Ok(false)` (→ 404), while a genuine DB error
/// is logged via `tracing::error!` and surfaced as `Err` (→ 500) instead of
/// being conflated with a not-found (#213). Hard delete mirrors the retention
/// purge; there is no soft-delete column.
pub async fn delete_notification(db: &PgPool, user_id: Uuid, notification_id: Uuid) -> Result<bool, String> {
    sqlx::query("DELETE FROM notifications WHERE id = $1 AND user_id = $2")
        .bind(notification_id)
        .bind(user_id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| {
            tracing::error!(error = %e, %user_id, %notification_id, "failed to delete notification");
            INTERNAL_ERROR_MESSAGE.to_string()
        })
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
    .map_err(internal_error_message)
}

pub async fn list_reminders(db: &PgPool, user_id: Uuid) -> Vec<Reminder> {
    match sqlx::query_as::<_, Reminder>(
        "SELECT * FROM reminders WHERE user_id = $1 ORDER BY created_at DESC"
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    {
        Ok(reminders) => reminders,
        Err(e) => {
            tracing::error!(error = %e, %user_id, "failed to list reminders");
            Vec::new()
        }
    }
}

pub async fn delete_reminder(db: &PgPool, user_id: Uuid, reminder_id: Uuid) -> Result<bool, String> {
    sqlx::query("DELETE FROM reminders WHERE id = $1 AND user_id = $2")
        .bind(reminder_id)
        .bind(user_id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| {
            tracing::error!(error = %e, %user_id, %reminder_id, "failed to delete reminder");
            INTERNAL_ERROR_MESSAGE.to_string()
        })
}

/// Fire all due reminders into the notification feed and advance their schedules.
/// Best-effort and non-propagating, like the auth-cleanup task: genuine DB errors
/// are logged via `tracing::error!` rather than swallowed, but never bubble up — a
/// fetch failure defers the whole batch to the next tick, and a per-row advance
/// failure is logged and skipped, so one bad row or a transient failure never
/// stalls the fire-and-forget ticker.
pub async fn fire_due_reminders(db: &PgPool) {
    let due = match sqlx::query_as::<_, Reminder>(
        "SELECT * FROM reminders WHERE is_active AND next_fire_at <= NOW()"
    )
    .fetch_all(db)
    .await
    {
        Ok(due) => due,
        Err(e) => {
            tracing::error!(error = %e, "failed to fetch due reminders");
            return;
        }
    };

    let now = Utc::now();
    for r in due {
        // create_notification logs its own DB errors internally (#204).
        let _ = create_notification(db, r.user_id, r.budget_id, "reminder", &r.message, None).await;
        // Clamp the base to now so a long-overdue reminder fires once and jumps to the
        // next FUTURE slot, instead of burst-firing every 60s tick until it catches up.
        let base = if r.next_fire_at < now { now } else { r.next_fire_at };
        let next = advance(base, &r.cadence);
        // NOTE: this UPDATE-failure branch is reachable only when the fetch above
        // succeeds but the advance fails, which a closed-pool unit test cannot force
        // (the closed pool fails at the fetch first). It is intentionally not
        // unit-tested; it mirrors the tested fetch/list/create error branches.
        if let Err(e) = sqlx::query("UPDATE reminders SET next_fire_at = $1 WHERE id = $2")
            .bind(next)
            .bind(r.id)
            .execute(db)
            .await
        {
            // Best-effort: log and continue so one bad row never stalls the ticker (#204).
            tracing::error!(error = %e, reminder_id = %r.id, user_id = %r.user_id, "failed to advance reminder schedule");
        }
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

    // Category-level check.
    //
    // Spend is sourced from `budget::category_table_rows` — the SAME single,
    // period-windowed helper that backs the Categories display (the LIST_CATEGORIES
    // table, category details, and the chat CATEGORIES context, unified in nels#282).
    // Consuming it here makes the limit alert agree with what the user sees by
    // construction, closing the #401 drift where this path summed ALL-TIME spend
    // (no `transaction_date` bound) against a per-period limit while the display
    // summed only the current period `[start, end)`.
    //
    // Expense-only semantics are preserved by selecting only the matching
    // `category_type == "expense"` row: `category_table_rows`'s `spent` column sums
    // every category type, but a limit check is expense-only, so an income/savings
    // category (or an absent one) yields no row and no alert — exactly like the old
    // `WHERE c.category_type = 'expense'` filter.
    //
    // Cost: reusing the whole display helper (rather than a bespoke windowed query)
    // is a deliberate drift-proofing tradeoff — it guarantees the alert reads the
    // exact figure the Categories list shows. The looked-up `cid` is always a
    // just-logged transaction's own (non-mirror) category, so the helper's
    // mirror-row resolution never changes the row we read. But a rollup-PARENT
    // budget that contains any mirror categories still pays those extra
    // mirror-resolution queries even though our row isn't a mirror; an ordinary
    // budget pays just the one windowed rows query. That is acceptable here because
    // every caller invokes this once per user transaction action (create / update /
    // delete / exclude), never in a bulk per-row loop.
    //
    // Distinguish a real DB error from a genuinely-absent category. Swallowing the
    // `Err` here would silently suppress the category-level limit alert (a financial
    // notification path) with nothing to debug from, so log it — mirroring the
    // budget-level branch below.
    if let Some(cid) = category_id {
        match crate::budget::category_table_rows(db, budget_id).await {
            Ok(rows) => {
                if let Some(row) = rows
                    .iter()
                    .find(|r| r.id == cid && r.category_type == "expense")
                {
                    let name = &row.name;
                    let limit = row.category_limit;
                    let spent = row.spent;
                    let exceeded_key = format!("limit:{}:cat:{}:exceeded", budget_id, cid);
                    let approaching_key = format!("limit:{}:cat:{}:approaching", budget_id, cid);
                    let active = limit_level(spent, limit).map(|level| {
                        let lim = limit.unwrap_or(0.0);
                        let msg = match level {
                            LimitLevel::Exceeded =>
                                format!("🚫 Category '{}' has exceeded its ${:.2} limit (${:.2} spent).", name, lim, spent),
                            LimitLevel::Approaching =>
                                format!("⚠️ Category '{}' is at {:.0}% of its ${:.2} limit (${:.2} spent).", name, spent / lim * 100.0, lim, spent),
                        };
                        (level, msg)
                    });
                    if let Some(msg) = reconcile_limit_notification(
                        db, user_id, budget_id, &exceeded_key, &approaching_key, active,
                    ).await {
                        alerts.push(msg);
                    }
                }
                // No matching expense row (category absent or not an expense
                // category) — nothing to check, matching the old query's filter.
            }
            Err(e) => {
                tracing::error!(error = %e, %budget_id, category_id = %cid, "category-level limit check query failed");
            }
        }
    }

    // Budget-level check (sum only expense-category transactions).
    //
    // Spend is date-scoped to the SAME window the budget DISPLAY uses
    // (`budget::aggregated_budget_amounts::member_amounts`), sourced from the shared
    // `budget::period_expense_spent` helper: a `project` budget tracks spend across
    // its whole lifetime `project_span`, a time_based budget over its
    // `current_period_window`. Consuming the display's own helper/window makes the
    // budget alert agree with what the user sees by construction, closing the #402
    // drift where this path summed ALL-TIME expense spend (no `transaction_date`
    // bound) against a per-period budget limit while the display summed only the
    // current period `[start, end)`. This is the exact sibling of the #401
    // category-level fix.
    //
    // The budget LIMIT is still the SUM of the expense categories' `category_limit`
    // (a single scalar subquery — no join fan-out to double-count). A budget with no
    // category limits totals 0, which limit_level treats as "no limit" and therefore
    // raises no alert.
    //
    // Mirror/rollup categories are intentionally NOT resolved here (AGENTS.md §8
    // known scope caveat): `period_expense_spent` sums only THIS budget's own
    // expense transactions, so a rollup parent's alert reflects its own spend, not
    // its children's — each source budget fires its own alerts. This fix does not
    // change that boundary; it only date-scopes the parent's own spend.
    //
    // Distinguish a real DB error from a genuinely-absent budget. Swallowing the
    // `Err` here would silently suppress every budget-level limit alert (a
    // financial notification path) with nothing to debug from, so log it.
    match sqlx::query(
        "SELECT b.name, b.budget_type, b.time_frame, b.created_at, b.closed_at, \
                COALESCE(( \
                    SELECT SUM(COALESCE(c.category_limit, 0)) FROM categories c \
                    WHERE c.budget_id = b.id AND c.category_type = 'expense' \
                ), 0)::float8 AS budget_total \
         FROM budgets b \
         WHERE b.id = $1"
    )
    .bind(budget_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => {
            let name: String = r.get("name");
            let limit: f64 = r.get("budget_total");
            // Choose the spend window EXACTLY as member_amounts does, keyed off this
            // budget's OWN type: a project spans its lifetime, a time_based budget
            // its current calendar period.
            let budget_type: String = r.get("budget_type");
            let time_frame: String = r.get("time_frame");
            let created_at: DateTime<Utc> = r.get("created_at");
            let closed_at: Option<DateTime<Utc>> = r.get("closed_at");
            let now = Utc::now();
            let (start, end) = if budget_type == "project" {
                crate::budget::project_span(created_at, closed_at, now)
            } else {
                crate::budget::current_period_window(&time_frame, now)
            };
            match crate::budget::period_expense_spent(db, budget_id, start, end).await {
                Ok(spent) => {
                    let exceeded_key = format!("limit:{}:budget:exceeded", budget_id);
                    let approaching_key = format!("limit:{}:budget:approaching", budget_id);
                    let active = limit_level(spent, Some(limit)).map(|level| {
                        let msg = match level {
                            LimitLevel::Exceeded =>
                                format!("🚫 Budget '{}' has exceeded its ${:.2} limit (${:.2} spent).", name, limit, spent),
                            LimitLevel::Approaching =>
                                format!("⚠️ Budget '{}' is at {:.0}% of its ${:.2} limit (${:.2} spent).", name, spent / limit * 100.0, limit, spent),
                        };
                        (level, msg)
                    });
                    if let Some(msg) = reconcile_limit_notification(
                        db, user_id, budget_id, &exceeded_key, &approaching_key, active,
                    ).await {
                        alerts.push(msg);
                    }
                }
                // A DB error computing windowed spend must not silently suppress the
                // budget-level alert — log it and skip this check, mirroring the
                // absent-budget/query-error branches.
                Err(e) => {
                    tracing::error!(error = ?e, %budget_id, "budget-level limit check spend query failed");
                }
            }
        }
        Ok(None) => {} // Budget genuinely absent — nothing to check.
        Err(e) => {
            tracing::error!(error = %e, %budget_id, "budget-level limit check query failed");
        }
    }

    alerts
}

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
    match mark_read(&state.db, user_id, notification_id).await {
        Ok(true) => Ok(StatusCode::OK),
        Ok(false) => Err((StatusCode::NOT_FOUND, "Notification not found".to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

#[derive(serde::Serialize)]
pub struct UnreadCount {
    pub count: i64,
}

/// `GET /api/notifications/unread_count` — owner-scoped unread total (#55).
pub async fn unread_count_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<UnreadCount>, (StatusCode, String)> {
    Ok(Json(UnreadCount {
        count: unread_count(&state.db, user_id).await,
    }))
}

#[derive(serde::Serialize)]
pub struct MarkAllRead {
    pub updated: u64,
}

/// `POST /api/notifications/read_all` — mark all of the caller's unread
/// notifications read (#55). Idempotent; always 200 with the affected count.
pub async fn mark_all_read_handler(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<MarkAllRead>, (StatusCode, String)> {
    Ok(Json(MarkAllRead {
        updated: mark_all_read(&state.db, user_id).await,
    }))
}

/// `DELETE /api/notifications/:id` — dismiss a single notification (#55).
/// Owner-scoped: dismissing a row that is not the caller's (or does not exist)
/// returns 404, never deletes another tenant's row.
pub async fn dismiss_notification_handler(
    State(state): State<AppState>,
    Path(notification_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    match delete_notification(&state.db, user_id, notification_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err((StatusCode::NOT_FOUND, "Notification not found".to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
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
        .map_err(|e| internal_error(format!("Failed to create reminder: {}", e)))?;
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
    match delete_reminder(&state.db, user_id, reminder_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err((StatusCode::NOT_FOUND, "Reminder not found".to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// Default age (days) after which READ notifications are purged.
const DEFAULT_NOTIFICATION_READ_RETENTION_DAYS: i64 = 30;
/// Default age (days) after which UNREAD notifications are purged (kept longer
/// so users do not silently lose unseen alerts).
const DEFAULT_NOTIFICATION_UNREAD_RETENTION_DAYS: i64 = 90;

/// Parse a notification retention day count. Falls back to `default` for
/// missing, unparseable, or out-of-range values. The upper bound (`i32::MAX`)
/// guards the `days as i32` binds in `purge_old_notifications`: a larger value
/// would truncate to an incorrect (often negative) day count and delete the
/// wrong rows, so it is rejected.
// Intentionally takes `default` as a parameter (unlike the hard-coded helpers in
// budget.rs/rag.rs): one parametric helper serves both the read and unread windows.
fn parse_notification_retention(raw: Option<String>, default: i64) -> i64 {
    match raw {
        Some(s) if !s.trim().is_empty() => match s.trim().parse::<i64>() {
            Ok(d) if (1..=i32::MAX as i64).contains(&d) => d,
            _ => {
                tracing::warn!(value = %s.trim(), "notification retention value is invalid; using default {}", default);
                default
            }
        },
        _ => default,
    }
}

/// Read notification retention windows from the environment, returning
/// `(read_days, unread_days)`. Read rows are purged sooner; unread rows are
/// kept longer.
pub fn notification_retention_days() -> (i64, i64) {
    (
        parse_notification_retention(
            std::env::var("NOTIFICATION_READ_RETENTION_DAYS").ok(),
            DEFAULT_NOTIFICATION_READ_RETENTION_DAYS,
        ),
        parse_notification_retention(
            std::env::var("NOTIFICATION_UNREAD_RETENTION_DAYS").ok(),
            DEFAULT_NOTIFICATION_UNREAD_RETENTION_DAYS,
        ),
    )
}

/// Batch size for the retention purge. Large enough that steady-state volumes
/// drain in a single batch, small enough that a first-run backlog (#76) is
/// chunked into bounded, lock-friendly DELETEs instead of one large statement.
const NOTIFICATION_PURGE_BATCH_SIZE: i64 = 5_000;

/// Delete notifications past their retention window: read rows older than
/// `read_days`, unread rows older than `unread_days`. Single OR predicate keyed
/// on `is_read`. Idempotent: once nothing remains beyond the windows, re-running
/// deletes zero rows. Returns rows deleted. Both day counts are clamped to
/// `1..=i32::MAX` before the SQL binds; see [`purge_old_notifications_batched`]
/// for the batching and clamp-safety rationale.
pub async fn purge_old_notifications(
    db: &PgPool,
    read_days: i64,
    unread_days: i64,
) -> Result<u64, sqlx::Error> {
    purge_old_notifications_batched(db, read_days, unread_days, NOTIFICATION_PURGE_BATCH_SIZE).await
}

/// Batched core of [`purge_old_notifications`]: deletes at most `batch_size`
/// rows per statement, looping until a batch deletes fewer than `batch_size`.
/// Returns the total deleted across all batches.
///
/// Termination is guaranteed because the eligible set is finite and not
/// replenished by inserts during the run: a newly-inserted row has
/// `created_at = NOW()`, ineligible under a `>= 1 day` predicate. (Unlike the
/// audit purge, eligibility here is NOT immutable — marking an old unread row
/// read can move it into the read arm of the predicate mid-run — but such flips
/// are bounded by the finite set of existing rows, so the loop still drains in a
/// bounded number of iterations.)
///
/// The `SELECT id … LIMIT $3` has no `ORDER BY` on purpose: every eligible row
/// is equally deletable, so an arbitrary per-batch subset is fine and each batch
/// makes progress (deleted rows vanish from the next subquery via MVCC). The
/// hourly ticker is spawned per backend process, so a horizontally-scaled API
/// could run two purges concurrently. Without an ordering that is benign at this
/// scale: the worst case is a transient row-lock deadlock that Postgres resolves
/// by aborting one transaction, which surfaces as the caller's `Err` arm (a
/// logged `warn`) and is retried on the next hourly tick — no data is lost and
/// the deleted set is unchanged. (If concurrent purges ever became common, a
/// stable `ORDER BY id` or `FOR UPDATE SKIP LOCKED` would remove even that.)
///
/// All three day/size counts are clamped to `1..=i32::MAX` before the `as i32`
/// binds, so a caller bypassing the config helpers cannot pass a value that
/// truncates to a negative interval (inverting the predicate) or a non-positive
/// LIMIT.
async fn purge_old_notifications_batched(
    db: &PgPool,
    read_days: i64,
    unread_days: i64,
    batch_size: i64,
) -> Result<u64, sqlx::Error> {
    let read_days = read_days.clamp(1, i32::MAX as i64) as i32;
    let unread_days = unread_days.clamp(1, i32::MAX as i64) as i32;
    let batch_size = batch_size.clamp(1, i32::MAX as i64) as i32;
    let mut total: u64 = 0;
    loop {
        let result = sqlx::query(
            "DELETE FROM notifications WHERE id IN (\
                 SELECT id FROM notifications WHERE \
                 (is_read = TRUE  AND created_at < NOW() - make_interval(days => $1::int)) OR \
                 (is_read = FALSE AND created_at < NOW() - make_interval(days => $2::int)) \
                 LIMIT $3::int\
             )",
        )
        .bind(read_days)
        .bind(unread_days)
        .bind(batch_size)
        .execute(db)
        .await?;
        let n = result.rows_affected();
        total += n;
        if n < batch_size as u64 {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;

    /// Minimal tracing layer that records (level, message) of every event so a
    /// test can assert a specific error log was emitted (issue #201).
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<(Level, String)>>>);

    struct MessageVisitor(String);
    impl Visit for MessageVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
        // Capture string-valued messages directly (unquoted) so a tightened
        // equality assertion would not trip over Debug's quoting.
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.0 = value.to_owned();
            }
        }
    }

    impl<S: Subscriber> Layer<S> for CapturedLogs {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            self.0.lock().unwrap().push((*event.metadata().level(), visitor.0));
        }
    }

    impl CapturedLogs {
        fn has_error_containing(&self, needle: &str) -> bool {
            self.0.lock().unwrap().iter()
                .any(|(level, msg)| *level == Level::ERROR && msg.contains(needle))
        }
    }

    /// Regression test for issue #201: a DB error in the category-level limit
    /// check must be logged via `tracing::error!`, not silently swallowed.
    ///
    /// It forces a deterministic `PoolClosed` error (a lazy pool that is closed
    /// before use), so it runs in CI without a live Postgres. The test is
    /// explicitly pinned to the current-thread runtime (`flavor = "current_thread"`)
    /// so the thread-local `set_default` subscriber stays active across `.await`.
    /// The log assertion is load-bearing: the empty-`Vec` return cannot
    /// distinguish the old (error-swallowing) code from the fixed code, so only
    /// the captured error log proves the fix.
    #[tokio::test(flavor = "current_thread")]
    async fn category_limit_check_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        // Lazy pool over a syntactically valid URL (no connection attempted),
        // then closed so every query returns Err(PoolClosed).
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;

        let alerts = check_and_notify_limits(
            &pool,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            Some(uuid::Uuid::new_v4()),
        ).await;

        assert!(alerts.is_empty(), "a DB error must not yield alerts");
        assert!(
            captured.has_error_containing("category-level limit check query failed"),
            "category-level DB error must be logged via tracing::error!"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_notification_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = create_notification(&pool, uuid::Uuid::new_v4(), None, "reminder", "hi", None).await;
        assert!(out.is_none(), "a DB error must yield None");
        assert!(captured.has_error_containing("failed to insert notification"),
            "create_notification DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_limit_notifications_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let removed = delete_limit_notifications(&pool, uuid::Uuid::new_v4(), &["k1", "k2"]).await;
        assert_eq!(removed, None, "a DB error must yield None (distinct from a genuine 0-rows delete)");
        assert!(captured.has_error_containing("failed to delete stale limit notifications"),
            "delete_limit_notifications DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upsert_limit_notification_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let inserted = upsert_limit_notification(
            &pool, uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
            "limit_exceeded", "limit:x:cat:y:exceeded", "msg",
        ).await;
        assert!(!inserted, "a DB error must yield false (not a spurious insert)");
        assert!(captured.has_error_containing("failed to upsert limit notification"),
            "upsert_limit_notification DB error must be logged via tracing::error!");
    }

    // Requires Postgres:  podman-compose up -d && cd backend && cargo test -- --ignored
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn upsert_refreshes_in_place_and_reports_insert() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let dedup = format!("limit:upsert-test:{user_id}:exceeded");
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(format!("notif-upsert-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        // notifications.budget_id has a FK to budgets(id); seed a real budget so
        // the upsert binds a valid reference rather than a dangling UUID.
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'upsert test', 'monthly', 0)")
            .bind(budget_id).bind(user_id)
            .execute(&pool).await.expect("seed budget");

        // First upsert INSERTs -> true.
        let first = upsert_limit_notification(&pool, user_id, budget_id,
            "limit_exceeded", &dedup, "🚫 old ($100.00 spent).").await;
        assert!(first, "first upsert must report inserted=true");

        // Mark it read, then upsert again with a NEW message: refresh in place.
        sqlx::query("UPDATE notifications SET is_read = TRUE WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(&dedup).execute(&pool).await.expect("mark read");
        let second = upsert_limit_notification(&pool, user_id, budget_id,
            "limit_exceeded", &dedup, "🚫 new ($150.00 spent).").await;
        assert!(!second, "refresh must report inserted=false");

        let (msg, is_read): (String, bool) = sqlx::query_as(
            "SELECT message, is_read FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(&dedup).fetch_one(&pool).await.expect("read back");
        assert_eq!(msg, "🚫 new ($150.00 spent).", "message refreshed to current figure");
        assert!(is_read, "refresh must NOT reset is_read (no re-surface)");

        // delete_limit_notifications removes it.
        let removed = delete_limit_notifications(&pool, user_id, &[dedup.as_str()]).await;
        assert_eq!(removed, Some(1), "delete removes the row");

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    // Requires Postgres:  podman-compose up -d && cd backend && cargo test -- --ignored
    // Seeds an isolated budget/category and drives check_and_notify_limits directly,
    // simulating the spend changes a delete/exclude/reduce would produce.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn reconcile_clears_alert_when_back_under_limit() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let cat_id = uuid::Uuid::new_v4();
        let tx_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(format!("notif-reconcile-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'reconcile test', 'monthly', 0)")
            .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type, category_limit) VALUES ($1, $2, 'Rob', 'expense', 100)")
            .bind(cat_id).bind(budget_id).execute(&pool).await.expect("seed category");
        // Over-limit transaction -> first check creates the exceeded alert.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 10347.99, 'bad', NOW())")
            .bind(tx_id).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed tx");

        let first = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(first.iter().any(|m| m.contains("exceeded")), "first check surfaces the exceeded alert");
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(cnt, 1, "exceeded alert exists after trip");
        // Budget scope tripped too (budget_total == the one category's limit).
        let budget_exc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:budget:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(budget_exc, 1, "budget-scope exceeded alert exists after trip");

        // Re-check while STILL over limit: the refresh must not re-surface, and must
        // not duplicate the row (idempotent — surfaced exactly once).
        let again = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(!again.iter().any(|m| m.contains("exceeded")),
            "re-checking while still over limit must NOT re-surface the exceeded alert");
        let cnt_again: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(cnt_again, 1, "still exactly one exceeded alert (refresh, not duplicate)");

        // Correct the spend (delete the bad tx) and re-check -> alert cleared.
        sqlx::query("DELETE FROM transactions WHERE id = $1").bind(tx_id).execute(&pool).await.expect("delete tx");
        let second = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(second.is_empty(), "re-check under limit surfaces nothing");
        let cnt_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(cnt_after, 0, "exceeded alert cleared once back under limit");
        let budget_exc_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:budget:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(budget_exc_after, 0, "budget-scope exceeded alert also cleared");

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    // Requires Postgres. Downgrade (Exceeded -> Approaching) removes the exceeded
    // alert, leaves an approaching alert reflecting current spend, and does NOT
    // surface a fresh alert (improvement is silent).
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn reconcile_downgrade_does_not_resurface() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let cat_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(format!("notif-downgrade-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'downgrade test', 'monthly', 0)")
            .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type, category_limit) VALUES ($1, $2, 'Rob', 'expense', 100)")
            .bind(cat_id).bind(budget_id).execute(&pool).await.expect("seed category");

        // Exceeded: 120 spent -> exceeded alert.
        let tx = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 120, 'a', NOW())")
            .bind(tx).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed tx");
        let _ = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;

        // Reduce to 85 (>=80% of 100 -> Approaching, not Exceeded).
        sqlx::query("UPDATE transactions SET amount = 85 WHERE id = $1").bind(tx).execute(&pool).await.expect("reduce");
        let surfaced = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(!surfaced.iter().any(|m| m.contains("is at")),
            "a downgrade to approaching must NOT surface a fresh alert");

        let exc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:exceeded")).fetch_one(&pool).await.unwrap();
        let app: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:approaching")).fetch_one(&pool).await.unwrap();
        assert_eq!(exc, 0, "exceeded alert removed on downgrade");
        assert_eq!(app, 1, "approaching alert now present with current figure");

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    // Requires Postgres. #401: the category-level limit check must be windowed to
    // the CURRENT period (matching the Categories display / `category_table_rows`),
    // not summed all-time. A large PRIOR-period transaction must NOT count toward
    // the current-period limit, so an in-window spend under the limit raises no
    // exceeded alert even though lifetime spend is far over — reproducing the
    // Entertainment $1,219.73-lifetime vs $137.73-in-period discrepancy.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_limit_check_uses_current_period_window() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let cat_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(format!("notif-window-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'window test', 'monthly', 0)")
            .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type, category_limit) VALUES ($1, $2, 'Entertainment', 'expense', 250)")
            .bind(cat_id).bind(budget_id).execute(&pool).await.expect("seed category");

        // Prior-period spend (2 months ago) — OUTSIDE the current window. Large
        // enough that summing it all-time (the old bug) would exceed the 250 limit.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 1082.00, 'old', NOW() - INTERVAL '2 months')")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed prior tx");
        // Current-period spend — under the 250 limit.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 137.73, 'new', NOW())")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed current tx");

        // The alert path must agree with the display: current-period spend 137.73
        // is UNDER the 250 limit, so NO exceeded alert — even though lifetime spend
        // (1219.73) is far over.
        // Scope assertions to the CATEGORY alert: the budget-level query is still
        // all-time (that is sibling #402), so it legitimately trips here — this test
        // owns only the category-level fix.
        let alerts = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(
            !alerts.iter().any(|m| m.contains("Category") && m.contains("exceeded")),
            "windowed category spend is under the limit; no category exceeded alert (got: {alerts:?})"
        );
        let exc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:cat:{cat_id}:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(exc, 0, "no exceeded notification row persisted for an in-window under-limit category");

        // Parity: the figure the alert path sees equals what the Categories display
        // (`category_table_rows`) shows for the same category and period.
        let rows = crate::budget::category_table_rows(&pool, budget_id).await.expect("category_table_rows");
        let display_spent = rows.iter().find(|r| r.id == cat_id).map(|r| r.spent).expect("category present");
        assert!(
            (display_spent - 137.73).abs() < 0.001,
            "display spent should be the windowed 137.73, got {display_spent}"
        );

        // Now push IN-WINDOW spend over the limit -> exceeded must trip, and the
        // reported figure must be the windowed total (137.73 + 200 = 337.73), not
        // the lifetime total.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 200.00, 'over', NOW())")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed over tx");
        let alerts2 = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(
            alerts2.iter().any(|m| m.contains("Category") && m.contains("exceeded") && m.contains("337.73")),
            "in-window over-limit must trip a category exceeded alert with the windowed figure 337.73 (got: {alerts2:?})"
        );

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    // Requires Postgres. #402 (sibling of #401): the BUDGET-level limit check must
    // be windowed to the CURRENT period (matching the budget display /
    // `member_amounts` / `period_expense_spent`), not summed all-time. A large
    // PRIOR-period transaction must NOT count toward the current-period budget
    // limit, so an in-window spend under the summed limit raises no budget-exceeded
    // alert even though lifetime spend is far over — reproducing the Family Budget
    // discrepancy. Once in-window spend crosses the limit the alert trips with the
    // WINDOWED figure, never the lifetime total.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn budget_limit_check_uses_current_period_window() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let cat_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id).bind(format!("notif-budget-window-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        // A time_based (monthly) budget; the budget LIMIT is the SUM of its expense
        // categories' category_limit — here a single 250 expense category => 250.
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'Family Budget', 'monthly', 0)")
            .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query("INSERT INTO categories (id, budget_id, name, category_type, category_limit) VALUES ($1, $2, 'Utilities', 'expense', 250)")
            .bind(cat_id).bind(budget_id).execute(&pool).await.expect("seed category");

        // Prior-period spend (2 months ago) — OUTSIDE the current window. Large
        // enough that summing it all-time (the old bug) would exceed the 250 budget
        // limit.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 1082.00, 'old', NOW() - INTERVAL '2 months')")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed prior tx");
        // Current-period spend — under the 250 budget limit.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 137.73, 'new', NOW())")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed current tx");

        // The budget alert path must agree with the display: current-period spend
        // 137.73 is UNDER the 250 budget limit, so NO budget-exceeded alert — even
        // though lifetime spend (1219.73) is far over.
        let alerts = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(
            !alerts.iter().any(|m| m.contains("Budget") && m.contains("exceeded")),
            "windowed budget spend is under the limit; no budget exceeded alert (got: {alerts:?})"
        );
        let exc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND dedup_key = $2")
            .bind(user_id).bind(format!("limit:{budget_id}:budget:exceeded"))
            .fetch_one(&pool).await.unwrap();
        assert_eq!(exc, 0, "no budget-exceeded notification row persisted for in-window under-limit spend");

        // Parity: the figure the budget alert path sees equals what the budget
        // display's shared helper reports over the SAME window member_amounts uses
        // (a time_based monthly budget => current_period_window).
        let (start, end) = crate::budget::current_period_window("monthly", chrono::Utc::now());
        let display_spent = crate::budget::period_expense_spent(&pool, budget_id, start, end)
            .await.expect("period_expense_spent");
        assert!(
            (display_spent - 137.73).abs() < 0.001,
            "display spent should be the windowed 137.73, got {display_spent}"
        );

        // Now push IN-WINDOW spend over the budget limit -> budget-exceeded must trip,
        // and the reported figure must be the windowed total (137.73 + 200 = 337.73),
        // not the lifetime total.
        sqlx::query("INSERT INTO transactions (id, budget_id, category_id, amount, description, transaction_date) VALUES ($1, $2, $3, 200.00, 'over', NOW())")
            .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(cat_id).execute(&pool).await.expect("seed over tx");
        let alerts2 = check_and_notify_limits(&pool, user_id, budget_id, Some(cat_id)).await;
        assert!(
            alerts2.iter().any(|m| m.contains("Budget") && m.contains("exceeded") && m.contains("337.73")),
            "in-window over-limit must trip a budget exceeded alert with the windowed figure 337.73 (got: {alerts2:?})"
        );

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_reminders_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = list_reminders(&pool, uuid::Uuid::new_v4()).await;
        assert!(out.is_empty(), "a DB error must yield an empty list");
        assert!(captured.has_error_containing("failed to list reminders"),
            "list_reminders DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_reminder_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = delete_reminder(&pool, uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).await;
        // The Err payload is client-facing (the handler returns it as the 500 body),
        // so pin it to the generic constant — a raw sqlx error string here would leak
        // schema detail (CWE-209).
        assert_eq!(
            out.unwrap_err(),
            INTERNAL_ERROR_MESSAGE,
            "a DB error must surface as the generic message, not a leaky sqlx error"
        );
        assert!(captured.has_error_containing("failed to delete reminder"),
            "delete_reminder DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mark_read_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = mark_read(&pool, uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).await;
        // The Err payload is client-facing (the handler returns it as the 500 body),
        // so pin it to the generic constant — a raw sqlx error string here would leak
        // schema detail (CWE-209).
        assert_eq!(
            out.unwrap_err(),
            INTERNAL_ERROR_MESSAGE,
            "a DB error must surface as the generic message, not a leaky sqlx error"
        );
        assert!(captured.has_error_containing("failed to mark notification read"),
            "mark_read DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_notification_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = delete_notification(&pool, uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).await;
        // The Err payload is client-facing (the handler returns it as the 500 body),
        // so pin it to the generic constant — a raw sqlx error string here would leak
        // schema detail (CWE-209).
        assert_eq!(
            out.unwrap_err(),
            INTERNAL_ERROR_MESSAGE,
            "a DB error must surface as the generic message, not a leaky sqlx error"
        );
        assert!(captured.has_error_containing("failed to delete notification"),
            "delete_notification DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_notifications_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = list_notifications(&pool, uuid::Uuid::new_v4(), false).await;
        assert!(out.is_empty(), "a DB error must yield an empty list");
        assert!(captured.has_error_containing("failed to list notifications"),
            "list_notifications DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mark_all_read_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = mark_all_read(&pool, uuid::Uuid::new_v4()).await;
        assert_eq!(out, 0, "a DB error must yield 0");
        assert!(captured.has_error_containing("failed to mark all notifications read"),
            "mark_all_read DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unread_count_logs_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        let out = unread_count(&pool, uuid::Uuid::new_v4()).await;
        assert_eq!(out, 0, "a DB error must yield 0");
        assert!(captured.has_error_containing("failed to count unread notifications"),
            "unread_count DB error must be logged via tracing::error!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fire_due_reminders_logs_fetch_db_error() {
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag")
            .expect("build lazy pool");
        pool.close().await;
        // Must not panic; best-effort returns quietly after logging.
        fire_due_reminders(&pool).await;
        assert!(captured.has_error_containing("failed to fetch due reminders"),
            "fire_due_reminders fetch DB error must be logged via tracing::error!");
    }

    // A genuinely-absent category (no matching row) must be a graceful no-op,
    // distinct from a DB error: no alert AND no error logged (issue #201,
    // criterion 2). Requires Postgres:
    //   podman-compose up -d && cd backend && cargo test -- --ignored
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn category_limit_check_absent_category_is_silent() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let user_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let missing_category = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("notif-absent-cat-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'absent-cat test', 'monthly', 0)")
            .bind(budget_id)
            .bind(user_id)
            .execute(&pool).await.expect("seed budget");

        let alerts = check_and_notify_limits(&pool, user_id, budget_id, Some(missing_category)).await;

        assert!(alerts.is_empty(), "absent category yields no alert");
        assert!(
            !captured.has_error_containing("category-level limit check query failed"),
            "absent category must NOT be logged as a DB error"
        );

        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.expect("cleanup budget");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.expect("cleanup user");
    }

    // A deduplicated insert (ON CONFLICT DO NOTHING) returns Ok(None), which must
    // be treated as a graceful no-op — NOT logged as a DB error. This guards the
    // exact invariant the #204 create_notification fix depends on: only a genuine
    // `Err` is logged, so routine dedup hits never spam tracing::error!. Requires
    // Postgres:
    //   podman-compose up -d && cd backend && cargo test -- --ignored
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_notification_dedup_conflict_is_silent() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let user_id = uuid::Uuid::new_v4();
        let dedup = format!("dedup-test-{user_id}");

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("notif-dedup-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");

        // First insert succeeds and returns the row.
        let first = create_notification(&pool, user_id, None, "limit_warning", "first", Some(&dedup)).await;
        assert!(first.is_some(), "first insert must create a row");

        // Second insert hits ON CONFLICT DO NOTHING -> Ok(None), a benign no-op.
        let second = create_notification(&pool, user_id, None, "limit_warning", "dup", Some(&dedup)).await;
        assert!(second.is_none(), "deduped insert returns None");
        assert!(
            !captured.has_error_containing("failed to insert notification"),
            "a dedup conflict (Ok(None)) must NOT be logged as a DB error"
        );

        sqlx::query("DELETE FROM notifications WHERE user_id = $1").bind(user_id).execute(&pool).await.expect("cleanup notifications");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.expect("cleanup user");
    }

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
    fn level_exceeded_only_above_limit() {
        // spend strictly OVER the limit is Exceeded
        assert_eq!(limit_level(120.0, Some(100.0)), Some(LimitLevel::Exceeded));
        assert_eq!(limit_level(100.01, Some(100.0)), Some(LimitLevel::Exceeded));
    }

    #[test]
    fn level_at_limit_not_exceeded() {
        // spend EQUAL to the limit is at-limit/approaching, never Exceeded (#193)
        assert_eq!(limit_level(100.0, Some(100.0)), Some(LimitLevel::Approaching));
    }

    #[test]
    fn stale_keys_none_clears_both() {
        let mut got = stale_limit_keys(None);
        got.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(got, vec![LimitKey::Approaching, LimitKey::Exceeded]);
    }

    #[test]
    fn stale_keys_exceeded_clears_approaching() {
        assert_eq!(stale_limit_keys(Some(LimitLevel::Exceeded)), vec![LimitKey::Approaching]);
    }

    #[test]
    fn stale_keys_approaching_clears_exceeded() {
        // Approaching clears the (now higher-severity) exceeded key — this removal is
        // what the async layer reads as a "downgrade" to suppress re-surfacing.
        assert_eq!(stale_limit_keys(Some(LimitLevel::Approaching)), vec![LimitKey::Exceeded]);
    }

    #[test]
    fn advance_steps() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(advance(t, "daily"), t + Duration::days(1));
        assert_eq!(advance(t, "weekly"), t + Duration::days(7));
        assert_eq!(advance(t, "monthly"), t + Duration::days(30));
        assert_eq!(advance(t, "bogus"), t + Duration::days(1));
    }

    #[test]
    fn notification_retention_defaults_when_absent() {
        assert_eq!(parse_notification_retention(None, 30), 30);
        assert_eq!(parse_notification_retention(None, 90), 90);
    }

    #[test]
    fn notification_retention_defaults_when_empty() {
        assert_eq!(parse_notification_retention(Some(String::new()), 30), 30);
        assert_eq!(parse_notification_retention(Some(String::new()), 90), 90);
    }

    #[test]
    fn notification_retention_defaults_when_unparseable() {
        assert_eq!(parse_notification_retention(Some("abc".to_string()), 30), 30);
        assert_eq!(parse_notification_retention(Some("abc".to_string()), 90), 90);
    }

    #[test]
    fn notification_retention_defaults_when_below_minimum() {
        // 0 and negatives are nonsensical -> fall back to the passed default.
        assert_eq!(parse_notification_retention(Some("0".to_string()), 30), 30);
        assert_eq!(parse_notification_retention(Some("-5".to_string()), 30), 30);
        assert_eq!(parse_notification_retention(Some("0".to_string()), 90), 90);
        assert_eq!(parse_notification_retention(Some("-5".to_string()), 90), 90);
    }

    #[test]
    fn notification_retention_parses_valid_value() {
        assert_eq!(parse_notification_retention(Some("15".to_string()), 30), 15);
        assert_eq!(parse_notification_retention(Some(" 15 ".to_string()), 30), 15);
        assert_eq!(parse_notification_retention(Some("15".to_string()), 90), 15);
        assert_eq!(parse_notification_retention(Some(" 15 ".to_string()), 90), 15);
    }

    #[test]
    fn notification_retention_rejects_above_i32_max() {
        // Values past i32::MAX would wrap to a negative interval in the
        // `days as i32` binds, so they fall back to the default.
        assert_eq!(parse_notification_retention(Some("2147483647".to_string()), 30), 2147483647);
        assert_eq!(parse_notification_retention(Some("2147483648".to_string()), 30), 30);
        assert_eq!(parse_notification_retention(Some("9999999999".to_string()), 90), 90);
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Seeds an isolated user with a unique UUID and cleans up after.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_notifications_drops_expired_keeps_recent() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();

        // Seed an isolated user (notifications.user_id FK). budget_id is nullable,
        // so notifications pass NULL.
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("notif-retention-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        // Helper to insert one notification at a chosen age and read state.
        async fn seed_notif(
            pool: &PgPool,
            user: uuid::Uuid,
            is_read: bool,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO notifications \
                 (id, user_id, budget_id, kind, message, is_read, dedup_key, created_at) \
                 VALUES ($1, $2, NULL, 'reminder', 'retention test', $3, NULL, \
                         NOW() - make_interval(days => $4::int))",
            )
            .bind(id)
            .bind(user)
            .bind(is_read)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed notification");
            id
        }

        // read + old (40 > 30) -> deleted; read + recent (1 day) -> kept.
        let read_old = seed_notif(&pool, user_id, true, 40).await;
        let read_recent = seed_notif(&pool, user_id, true, 1).await;
        // unread + old (100 > 90) -> deleted; unread + recent (40 < 90) -> kept.
        let unread_old = seed_notif(&pool, user_id, false, 100).await;
        let unread_recent = seed_notif(&pool, user_id, false, 40).await;

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM notifications WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // The table-wide `deleted` count is not asserted: a sibling test calling
        // purge_old_notifications concurrently can delete OUR expired rows first,
        // making even a `>= 2` lower bound racy. The per-user count and per-id
        // checks below are isolated by UUID and hold regardless of which purge
        // did the deleting.
        let _ = purge_old_notifications(&pool, 30, 90).await.expect("purge");

        // Exactly the two recent rows remain for this user.
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .expect("count remaining");
        assert_eq!(remaining, 2, "only the two recent rows remain");

        assert!(!exists(&pool, read_old).await, "read-old row deleted");
        assert!(!exists(&pool, unread_old).await, "unread-old row deleted");
        assert!(exists(&pool, read_recent).await, "read-recent row retained");
        assert!(exists(&pool, unread_recent).await, "unread-recent row retained");

        // Idempotency: a second run must not touch OUR rows. The table-wide
        // return value is fragile (a concurrent test could insert+leave an
        // expired row between the two calls), so assert on our scoped state
        // instead: the same two recent rows still remain for this user.
        let _ = purge_old_notifications(&pool, 30, 90)
            .await
            .expect("purge again");
        let remaining_after_second: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .expect("count remaining after second purge");
        assert_eq!(
            remaining_after_second, 2,
            "second run leaves our two recent rows intact"
        );

        // Cleanup: notifications, then user.
        sqlx::query("DELETE FROM notifications WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup notifications");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Seeds an isolated user with a unique UUID and cleans up after.
    //
    // Pins the read/unread asymmetry (the defining behavior: unread rows are
    // kept LONGER than read rows) AND the strict `<` window boundaries, all by
    // id so the assertions are immune to other rows in the shared DB.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_notifications_respects_readstate_and_boundaries() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("notif-boundary-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        async fn seed_notif(
            pool: &PgPool,
            user: uuid::Uuid,
            is_read: bool,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO notifications \
                 (id, user_id, budget_id, kind, message, is_read, dedup_key, created_at) \
                 VALUES ($1, $2, NULL, 'reminder', 'boundary test', $3, NULL, \
                         NOW() - make_interval(days => $4::int))",
            )
            .bind(id)
            .bind(user)
            .bind(is_read)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed notification");
            id
        }

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM notifications WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // The "gap" rows: 60 days old is > read window (30) but < unread window
        // (90). A read row at 60d MUST be purged; an unread row at 60d MUST
        // survive. This is the read/unread asymmetry no other case exercises.
        let gap_read = seed_notif(&pool, user_id, true, 60).await;
        let gap_unread = seed_notif(&pool, user_id, false, 60).await;

        // Boundary rows pinning the strict `<` comparison against each window.
        // Read window = 30: 29d survives (29 < 30), 31d deleted (31 > 30).
        let read_29 = seed_notif(&pool, user_id, true, 29).await;
        let read_31 = seed_notif(&pool, user_id, true, 31).await;
        // Unread window = 90: 89d survives, 91d deleted.
        let unread_89 = seed_notif(&pool, user_id, false, 89).await;
        let unread_91 = seed_notif(&pool, user_id, false, 91).await;

        // The table-wide `deleted` count is intentionally not asserted here: a
        // sibling test calling `purge_old_notifications` concurrently can delete
        // OUR expired rows first, making even a `>= N` lower bound racy. The
        // per-id assertions below are isolated by UUID and are the meaningful
        // checks — they hold regardless of who issued the deleting purge.
        let _ = purge_old_notifications(&pool, 30, 90).await.expect("purge");

        // Read/unread asymmetry at the 60-day gap.
        assert!(!exists(&pool, gap_read).await, "read gap (60d) deleted: 60 > 30");
        assert!(exists(&pool, gap_unread).await, "unread gap (60d) survives: 60 < 90");

        // Read-window boundary.
        assert!(exists(&pool, read_29).await, "read 29d survives: 29 < 30");
        assert!(!exists(&pool, read_31).await, "read 31d deleted: 31 > 30");

        // Unread-window boundary.
        assert!(exists(&pool, unread_89).await, "unread 89d survives: 89 < 90");
        assert!(!exists(&pool, unread_91).await, "unread 91d deleted: 91 > 90");

        // Cleanup: notifications, then user.
        sqlx::query("DELETE FROM notifications WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup notifications");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    // Proves the batched loop drains MORE than one batch: batch_size = 2 with 5
    // expired rows forces (2 + 2 + 1) iterations. Asserted by id (not the
    // table-wide count) because sibling --ignored tests purge concurrently.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_notifications_batches_until_drained() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("notif-batch-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        async fn seed_notif(
            pool: &PgPool,
            user: uuid::Uuid,
            is_read: bool,
            age_days: i64,
        ) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO notifications (id, user_id, kind, message, is_read, created_at)
             VALUES ($1, $2, 'reminder', 'batch test', $3, NOW() - make_interval(days => $4::int))",
            )
            .bind(id)
            .bind(user)
            .bind(is_read)
            .bind(age_days as i32)
            .execute(pool)
            .await
            .expect("seed notification");
            id
        }

        async fn exists(pool: &PgPool, id: uuid::Uuid) -> bool {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM notifications WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("fetch existence")
        }

        // 5 expired rows spanning BOTH predicate arms — 3 read (read window 30d
        // → 400d expired) + 2 unread (unread window 90d → 400d expired) — so the
        // batched subquery is proven to drain via the read AND unread arms of the
        // relocated OR predicate, not just one. Plus a recent row of each read
        // state that must survive (read: 0 < 30; unread: 0 < 90).
        let mut old_ids = Vec::new();
        for _ in 0..3 {
            old_ids.push(seed_notif(&pool, user_id, true, 400).await);
        }
        for _ in 0..2 {
            old_ids.push(seed_notif(&pool, user_id, false, 400).await);
        }
        let recent_read = seed_notif(&pool, user_id, true, 0).await;
        let recent_unread = seed_notif(&pool, user_id, false, 0).await;

        // batch_size = 2 forces multiple iterations to drain the 5 expired rows.
        let _ = purge_old_notifications_batched(&pool, 30, 90, 2)
            .await
            .expect("batched purge");

        for id in &old_ids {
            assert!(!exists(&pool, *id).await, "expired notification {id} drained across batches");
        }
        assert!(exists(&pool, recent_read).await, "recent read notification retained");
        assert!(exists(&pool, recent_unread).await, "recent unread notification retained");

        // Cleanup: notifications, then user.
        sqlx::query("DELETE FROM notifications WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup notifications");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Owner-scoping for the #55 feed operations: unread_count, list,
    // mark_all_read, mark_read, and delete_notification must each act ONLY on
    // the calling user's rows. Seeds two isolated users (A and B) with their own
    // unread rows and asserts no operation crosses the tenant boundary.
    //
    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn feed_ops_are_owner_scoped() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_a = uuid::Uuid::new_v4();
        let user_b = uuid::Uuid::new_v4();

        for (u, tag) in [(user_a, "a"), (user_b, "b")] {
            sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
                .bind(u)
                .bind(format!("notif-owner-{tag}-{u}@example.test"))
                .execute(&pool)
                .await
                .expect("seed user");
        }

        // Insert an UNREAD notification for a user; return its id.
        async fn seed_unread(pool: &PgPool, user: uuid::Uuid) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO notifications (id, user_id, budget_id, kind, message, is_read, dedup_key) \
                 VALUES ($1, $2, NULL, 'reminder', 'owner-scope test', FALSE, NULL)",
            )
            .bind(id)
            .bind(user)
            .execute(pool)
            .await
            .expect("seed notification");
            id
        }

        // A has 3 unread, B has 2 unread.
        let a1 = seed_unread(&pool, user_a).await;
        let _a2 = seed_unread(&pool, user_a).await;
        let _a3 = seed_unread(&pool, user_a).await;
        let b1 = seed_unread(&pool, user_b).await;
        let _b2 = seed_unread(&pool, user_b).await;

        // unread_count is per-user.
        assert_eq!(unread_count(&pool, user_a).await, 3, "A sees only A's unread");
        assert_eq!(unread_count(&pool, user_b).await, 2, "B sees only B's unread");

        // list is per-user (and only the seeded rows for each).
        assert_eq!(list_notifications(&pool, user_a, false).await.len(), 3);
        assert_eq!(list_notifications(&pool, user_b, false).await.len(), 2);

        // A cannot mark-read B's row: id belongs to B, user_id is A -> no-op.
        assert!(!mark_read(&pool, user_a, b1).await.unwrap(), "A cannot read B's row");
        assert_eq!(unread_count(&pool, user_b).await, 2, "B's count unchanged");

        // A cannot dismiss B's row: id belongs to B -> false, row survives.
        assert!(!delete_notification(&pool, user_a, b1).await.unwrap(), "A cannot dismiss B's row");
        assert_eq!(unread_count(&pool, user_b).await, 2, "B's row survived A's dismiss");

        // A marks one of its own rows read; only A's count drops.
        assert!(mark_read(&pool, user_a, a1).await.unwrap(), "A reads A's own row");
        assert_eq!(unread_count(&pool, user_a).await, 2, "A down to 2");
        assert_eq!(unread_count(&pool, user_b).await, 2, "B untouched");

        // mark_all_read affects ONLY A's remaining unread (2), never B's.
        assert_eq!(mark_all_read(&pool, user_a).await, 2, "A's remaining 2 marked");
        assert_eq!(unread_count(&pool, user_a).await, 0, "A now 0 unread");
        assert_eq!(unread_count(&pool, user_b).await, 2, "B still 2 unread");

        // A dismisses its own (now-read) row -> true.
        assert!(delete_notification(&pool, user_a, a1).await.unwrap(), "A dismisses A's own row");

        // mark_all_read is idempotent (nothing unread for A now).
        assert_eq!(mark_all_read(&pool, user_a).await, 0, "idempotent on A");

        // Cleanup: notifications then users (FK order). B's rows still intact
        // until this point, proving none of A's ops touched them.
        for u in [user_a, user_b] {
            sqlx::query("DELETE FROM notifications WHERE user_id = $1")
                .bind(u)
                .execute(&pool)
                .await
                .expect("cleanup notifications");
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(u)
                .execute(&pool)
                .await
                .expect("cleanup user");
        }

        // Silence unused warnings for the ids we keep only for clarity.
        let _ = (b1,);
    }
}
