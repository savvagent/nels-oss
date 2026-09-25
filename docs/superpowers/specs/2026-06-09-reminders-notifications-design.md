# Pillar 2 — Reminders & Budget-Limit Notifications (Design Spec)

**Date:** 2026-06-09
**Status:** Approved, ready for implementation planning
**Scope:** Complete Pillar 2 ("Track Expenses") by adding (a) event-driven budget-limit
notifications fired when logging an expense crosses a category/budget threshold, and
(b) user-defined recurring reminders to log expenses, both delivered through an in-app
notification feed and surfaced in the chat. Backend: new migration + `notifications.rs`
module + a reminder scheduler + integration into both transaction-logging paths.
Frontend: chat-only (one discoverability prompt-chip).

## 1. Goals & Non-Goals

**Goals**
- When a user logs an expense that pushes a category (vs `category_limit`) or the budget
  (vs `budget_limit`) to ≥80% (approaching) or ≥100% (exceeded), create an in-app
  notification and surface it immediately in the logging response (REST + chat).
- Let users set recurring reminders ("remind me daily to log expenses") that a background
  task fires into the notification feed on schedule.
- Expose a REST notification feed (list + mark-read) and reminder CRUD (create/list/delete).
- Let the chat assistant report unread notifications and create reminders conversationally.

**Non-Goals (YAGNI)**
- No email or web-push delivery (no such infrastructure exists). In-app feed only.
- No notifications UI components (bell/badge/panel) — chat-only + REST, per decision.
- No per-collaborator fan-out: limit alerts go to the acting user only (the one who
  logged the transaction), not every collaborator on a shared budget.
- No per-period reset of limit alerts: spending in this app is cumulative/all-time
  (the existing spend aggregation does not filter by the budget's time-frame window),
  so each threshold alerts exactly once via a dedup key.
- No reminder editing or pause/resume, and no chat-based listing/deleting of reminders
  (REST handles management). Reminders support delete + create only.

## 2. Decisions (from brainstorming)

| Decision | Choice |
|---|---|
| Scope | Both budget-limit notifications AND recurring reminders |
| Delivery | In-app feed (notifications table + REST + chat surfacing) |
| Triggers | Category limit AND budget limit, at 80% (approaching) and 100% (exceeded) |
| Frontend | Chat-only + REST endpoint (plus one prompt-chip) |
| Alert recipient | Acting user only (no collaborator fan-out) |
| Period reset | None — cumulative spending; each threshold alerts once (dedup key) |

## 3. Data Model (new migration `*_notifications.sql`)

### `notifications`
| column | type | constraints |
|---|---|---|
| `id` | UUID | PRIMARY KEY |
| `user_id` | UUID | NOT NULL REFERENCES users(id) ON DELETE CASCADE |
| `budget_id` | UUID | NULL REFERENCES budgets(id) ON DELETE CASCADE |
| `kind` | VARCHAR(20) | NOT NULL — `limit_warning` \| `limit_exceeded` \| `reminder` |
| `message` | TEXT | NOT NULL |
| `is_read` | BOOLEAN | NOT NULL DEFAULT FALSE |
| `dedup_key` | TEXT | NULL |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Indexes / constraints:
- `notifications_user_id_idx` on `(user_id, created_at DESC)`.
- Partial unique index `CREATE UNIQUE INDEX notifications_dedup_idx ON notifications (user_id, dedup_key) WHERE dedup_key IS NOT NULL;` — limit-alert inserts use `ON CONFLICT (user_id, dedup_key) DO NOTHING` so each threshold fires once per user. Reminder notifications use `dedup_key = NULL` (always inserted).

### `reminders`
| column | type | constraints |
|---|---|---|
| `id` | UUID | PRIMARY KEY |
| `user_id` | UUID | NOT NULL REFERENCES users(id) ON DELETE CASCADE |
| `budget_id` | UUID | NULL REFERENCES budgets(id) ON DELETE CASCADE |
| `message` | TEXT | NOT NULL |
| `cadence` | VARCHAR(20) | NOT NULL — `daily` \| `weekly` \| `monthly` |
| `next_fire_at` | TIMESTAMPTZ | NOT NULL |
| `is_active` | BOOLEAN | NOT NULL DEFAULT TRUE |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Index: `reminders_due_idx` on `(next_fire_at) WHERE is_active`.
First fire is `now() + cadence` (a daily reminder created now first fires ~24h later).

## 4. Backend Module `notifications.rs` (new)

DB structs in `db.rs`: `Notification`, `Reminder`.

### Pure functions (unit-tested, no DB)
```
enum LimitLevel { Approaching, Exceeded }
fn limit_level(spent: f64, limit: Option<f64>) -> Option<LimitLevel>
    // None if limit is None or <= 0; Exceeded if spent >= limit;
    // Approaching if spent >= 0.8 * limit (and < limit); else None.
fn advance(from: DateTime<Utc>, cadence: &str) -> DateTime<Utc>
    // daily -> +1 day, weekly -> +7 days, monthly -> +30 days (default daily).
```

### Limit checking
```
async fn check_and_notify_limits(db, user_id, budget_id, category_id: Option<Uuid>) -> Vec<String>
```
- Look up the category's `category_limit` and summed spend (transactions in that category);
  compute `limit_level`. Look up the budget's `budget_limit` and total summed spend across
  the budget; compute `limit_level`.
- For each non-None level, build `(kind, message, dedup_key)`:
  - category scope dedup_key: `limit:{budget_id}:cat:{category_id}:{level}`
  - budget scope dedup_key: `limit:{budget_id}:budget:{level}`
  - `kind`: `limit_exceeded` for Exceeded, `limit_warning` for Approaching.
  - message e.g. `"⚠️ Category 'Food' is at 92% of its $200.00 limit ($184.00 spent)."`
    or `"🚫 Budget 'My First Budget' has exceeded its $1000.00 limit ($1040.00 spent)."`
- Insert each with `ON CONFLICT (user_id, dedup_key) DO NOTHING`; collect the messages of
  rows that were actually inserted (use `RETURNING message` to know which were new) and
  return them. Returns `[]` when nothing newly crossed.

### CRUD / helpers
- `create_notification(db, user_id, budget_id, kind, message, dedup_key) -> Option<Notification>`
  (None when a dedup conflict skipped it).
- `list_notifications(db, user_id, unread_only: bool) -> Vec<Notification>` (newest first).
- `mark_read(db, user_id, notification_id) -> bool` (scoped by user_id; 404 if not theirs).
- `create_reminder(db, user_id, budget_id, message, cadence) -> Reminder`
  (validates cadence ∈ {daily,weekly,monthly}; sets `next_fire_at = advance(now, cadence)`).
- `list_reminders(db, user_id) -> Vec<Reminder>`.
- `delete_reminder(db, user_id, reminder_id) -> bool` (scoped by user_id).
- `fire_due_reminders(db)` — for the scheduler (see §6).

## 5. REST Routes (registered in `main.rs`, user-scoped via `Extension<Uuid>`)

| Method | Path | Handler | Notes |
|---|---|---|---|
| GET | `/api/notifications` | `list_notifications_handler` | `?unread=true` optional filter |
| POST | `/api/notifications/:id/read` | `mark_notification_read` | 404 if not the user's |
| POST | `/api/reminders` | `create_reminder_handler` | body: `{message, cadence, budget_id?}` |
| GET | `/api/reminders` | `list_reminders_handler` | |
| DELETE | `/api/reminders/:id` | `delete_reminder_handler` | 404 if not the user's |

These are user-scoped (recipient = creator), so they need only the authenticated `user_id`
from the auth middleware — no budget-permission checks.

`budget.rs::create_transaction` gains a new response wrapper so the immediate logger sees
alerts without changing `list_transactions`:
```
struct CreateTransactionResponse { transaction: TransactionResponse, alerts: Vec<String> }
```

## 6. Reminder Scheduler (background task in `main.rs`)

A new `tokio::spawn` loop (mirroring the existing hourly auth-cleanup task) ticks every
**60 seconds** and calls `notifications::fire_due_reminders(&pool)`:
- `SELECT * FROM reminders WHERE is_active AND next_fire_at <= NOW()`.
- For each: insert a `kind='reminder'` notification (`dedup_key = NULL`) for its `user_id`
  /`budget_id` with its `message`, then `UPDATE reminders SET next_fire_at = advance(next_fire_at, cadence)`.
- All best-effort (errors logged, not fatal), consistent with the existing cleanup task.

## 7. Conversational AI (`rag.rs`)

- **Context block:** inject an unread-NOTIFICATIONS summary (count + a few recent messages)
  so the assistant can answer "do I have any alerts?".
- **`ADD_TRANSACTION` executor:** after inserting the transaction, call
  `check_and_notify_limits(...)` and append any returned alert lines to `response_text`.
- **New action `CREATE_REMINDER`:** added to the Gemini system prompt, the offline keyword
  matcher ("remind me" + a `daily`/`weekly`/`monthly` keyword, default daily), and an
  executor `match` arm that calls `create_reminder`. Logged to `audit_logs` as `AI_CREATE_REMINDER`
  when a budget is in context (audit is budget-scoped; for a null-budget reminder, skip audit).
- New `AiActionParams` fields: `reminder_message: Option<String>`, `cadence: Option<String>`.

## 8. Frontend (`App.svelte`)
No new components. Add one hint prompt-chip: "Remind me to log expenses daily". Only touch.

## 9. Testing & Verification
1. `cargo check` clean; unit tests for `limit_level` (no-limit, under-80, approaching, exceeded)
   and `advance` (daily/weekly/monthly) — `cargo test`.
2. Curl REST round-trip: create a category with a `$100` limit, log a `$95` expense →
   `create_transaction` returns an "approaching" alert and `GET /api/notifications` shows it;
   log another `$20` (now $115) → "exceeded" alert; log `$5` more → NO duplicate exceeded
   notification (dedup). `POST /api/notifications/:id/read` flips `is_read`.
3. Reminder e2e: `POST /api/reminders {message:"log expenses", cadence:"daily"}`; `psql` set its
   `next_fire_at` into the past; wait ~65s for one scheduler tick; `GET /api/notifications`
   shows a `reminder` notification and the reminder's `next_fire_at` advanced ~1 day. (Slow
   test ~70s by design.)
4. Chat: `CREATE_REMINDER` round-trip ("remind me weekly to review my budget" → reminder created,
   listed via REST); logging an over-limit expense via chat appends the alert to the reply.
5. Clean up all test users from the dev DB afterward.

## 10. Files Touched
- **New:** `backend/migrations/<ts>_notifications.sql`, `backend/src/notifications.rs`, this spec.
- **Modified:** `backend/src/db.rs` (Notification, Reminder structs), `backend/src/main.rs`
  (module decl, routes, scheduler task), `backend/src/budget.rs` (create_transaction wrapper +
  limit check), `backend/src/rag.rs` (notifications context, ADD_TRANSACTION alert append,
  CREATE_REMINDER action), `frontend/src/App.svelte` (one prompt-chip), `Goal.md` (Pillar 2 status).
```
