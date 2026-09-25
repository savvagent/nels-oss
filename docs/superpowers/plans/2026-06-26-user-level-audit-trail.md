# User-level audit trail for non-budget-scoped actions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give non-budget-scoped actions (chat `REPORT_ISSUE`, REST `/issues-create`, and the named optional backfills) a first-class audit trail attributable to the user, even when there is no active budget.

**Architecture:** Make `audit_logs.budget_id` nullable (a NULL value means "user-scoped, not tied to a budget"), add a `log_user_audit` helper next to the existing `log_audit` (both delegating to one private writer), and call it on the success branch of the issue-filing paths plus the two named backfills. Audit writes stay fail-safe (warn-and-continue).

**Tech Stack:** Rust, axum, sqlx (PostgreSQL), tokio test, follow-up to PR #189 / #194 (#191).

**Source:** savvagent/nels#192. **Branch:** `issue-192-user-audit-trail` (off `origin/main` @ #194).

**Repo conventions (Phase 0.5):** Tests `cd backend && cargo check` / `cargo test` / `cargo test -- --ignored` (DB-backed tests connect directly to the local pgvector DB at `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`; they do NOT run `sqlx::migrate!`, so a new migration must be applied to that DB before running them). Migrations auto-apply on server start. Conventional-commit subjects with the issue ref: `feat(#192): …`. Backend deploys to Fly.io on merge to `main` via path-filtered GitHub Actions.

---

### Task 1: Nullable `budget_id` migration + `log_user_audit` helper

**Files:**
- Create: `backend/migrations/20260626010000_audit_logs_nullable_budget.sql`
- Modify: `backend/src/budget.rs` (the `log_audit` fn ~line 322; add tests in the `#[cfg(test)] mod tests` block)

- [ ] **Step 1: Write the migration**

Create `backend/migrations/20260626010000_audit_logs_nullable_budget.sql`:

```sql
-- User-level (non-budget-scoped) audit trail (#192). REPORT_ISSUE and other
-- actions that aren't tied to a budget (e.g. chat REPORT_ISSUE with no active
-- budget, the REST /issues-create path) need an audit row attributable to the
-- user. `log_audit` required a non-null budget_id; making the column nullable
-- lets `log_user_audit` write a user-scoped row (NULL budget_id). The existing
-- ON DELETE CASCADE FK is unaffected (a NULL FK simply never cascades); a
-- deleted user's rows are still removed explicitly by account deletion
-- (account.rs delete_user_data: DELETE FROM audit_logs WHERE user_id = $1).
ALTER TABLE audit_logs ALTER COLUMN budget_id DROP NOT NULL;
```

- [ ] **Step 2: Apply the migration to the local test DB so the DB-backed tests can run**

The `#[ignore]` tests connect directly (no `sqlx::migrate!`). Apply it manually once:

Run:
```bash
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag \
  -c "ALTER TABLE audit_logs ALTER COLUMN budget_id DROP NOT NULL;"
```
Expected: `ALTER TABLE`. (Idempotent: re-running on an already-nullable column also prints `ALTER TABLE`.)

- [ ] **Step 3: Write the failing tests** in `backend/src/budget.rs` inside `mod tests`

Add (uses the same direct-pool pattern as other DB tests in this crate):

```rust
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn log_user_audit_writes_user_scoped_row() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
    });
    let pool = PgPool::connect(&url).await.expect("connect to test db");

    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
        .bind(user_id)
        .bind(format!("audit-{user_id}@example.test"))
        .execute(&pool)
        .await
        .expect("seed user");

    log_user_audit(&pool, user_id, "AI_REPORT_ISSUE", "Filed issue #1: x").await;

    let row = sqlx::query(
        "SELECT budget_id, user_id, action, details FROM audit_logs \
         WHERE user_id = $1 AND action = 'AI_REPORT_ISSUE'",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("user-scoped audit row written");

    let budget_id: Option<Uuid> = row.get("budget_id");
    let got_user: Uuid = row.get("user_id");
    let action: String = row.get("action");
    let details: String = row.get("details");
    assert!(budget_id.is_none(), "user-scoped row must have NULL budget_id");
    assert_eq!(got_user, user_id);
    assert_eq!(action, "AI_REPORT_ISSUE");
    assert_eq!(details, "Filed issue #1: x");

    // audit_logs.user_id is ON DELETE SET NULL, so delete the row explicitly first.
    let _ = sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(user_id).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn log_audit_still_writes_budget_scoped_row_after_nullable_migration() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
    });
    let pool = PgPool::connect(&url).await.expect("connect to test db");

    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
        .bind(user_id)
        .bind(format!("audit-b-{user_id}@example.test"))
        .execute(&pool)
        .await
        .expect("seed user");
    let budget_id = Uuid::new_v4();
    sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'Audit Test', 'monthly', 0)")
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

    log_audit(&pool, budget_id, user_id, "AI_TEST", "scoped").await;

    let got: Option<Uuid> = sqlx::query_scalar(
        "SELECT budget_id FROM audit_logs WHERE user_id = $1 AND action = 'AI_TEST'",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("budget-scoped audit row written");
    assert_eq!(got, Some(budget_id), "budget-scoped row keeps its budget_id");

    // Clean up (audit_logs cascade on budget delete, but be explicit/order-safe).
    let _ = sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(user_id).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
}
```

NOTE: confirm `Row`/`get` are in scope in `budget.rs` tests (the module already uses `sqlx::Row` elsewhere). If `Uuid` / `PgPool` aren't imported in the test module, add `use sqlx::PgPool;` / `use uuid::Uuid;` / `use sqlx::Row;` as needed (match the existing test imports in this file).

- [ ] **Step 4: Run the tests to verify they fail** (helper doesn't exist yet)

Run: `cd backend && cargo test --no-run 2>&1 | tail -20`
Expected: compile error — `cannot find function log_user_audit`.

- [ ] **Step 5: Implement the helper** — refactor `log_audit` in `backend/src/budget.rs` to share a private writer and add `log_user_audit`:

Replace the existing `log_audit` function body with:

```rust
// Helper to log audit activity (Runtime query). Budget-scoped variant.
pub async fn log_audit(
    pool: &PgPool,
    budget_id: Uuid,
    user_id: Uuid,
    action: &str,
    details: &str,
) {
    log_audit_row(pool, Some(budget_id), user_id, action, details).await;
}

/// User-level (non-budget-scoped) audit write (#192). Writes an audit row with a
/// NULL `budget_id` for actions that aren't tied to a budget (e.g. REPORT_ISSUE
/// — which files to the project repo, not a budget — or SET_USER_NAME). Same
/// fail-safe behavior as `log_audit`.
pub async fn log_user_audit(pool: &PgPool, user_id: Uuid, action: &str, details: &str) {
    log_audit_row(pool, None, user_id, action, details).await;
}

/// Shared audit-row writer. `budget_id` is `None` for user-scoped rows. Fail-safe:
/// a write failure is logged at `warn` (with the identifying fields, not the
/// potentially-large `details`) and never propagated — an audit gap must not
/// break the user-facing action (#168).
async fn log_audit_row(
    pool: &PgPool,
    budget_id: Option<Uuid>,
    user_id: Uuid,
    action: &str,
    details: &str,
) {
    let log_id = Uuid::new_v4();
    if let Err(e) = sqlx::query(
        "INSERT INTO audit_logs (id, budget_id, user_id, action, details) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(log_id)
    .bind(budget_id)
    .bind(user_id)
    .bind(action)
    .bind(details)
    .execute(pool)
    .await
    {
        tracing::warn!(
            error = ?e,
            log_id = %log_id,
            budget_id = ?budget_id,
            user_id = %user_id,
            action = %action,
            "audit log insert failed"
        );
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd backend && cargo test -- --ignored log_user_audit log_audit_still 2>&1 | tail -25`
Expected: both new tests PASS. Also run `cd backend && cargo check` — clean.

- [ ] **Step 7: Commit**

```bash
git add backend/migrations/20260626010000_audit_logs_nullable_budget.sql backend/src/budget.rs
git commit -m "feat(#192): nullable audit budget_id + log_user_audit helper"
```

---

### Task 2: Chat path — always user-scoped `AI_REPORT_ISSUE` + named backfills

**Files:**
- Modify: `backend/src/rag.rs` — `chat_report_issue` (~3209) + its caller (~2842); `CREATE_REMINDER` arm (~2815); `SET_USER_NAME` arm (~2850); existing test `chat_report_issue_surfaces_rate_limit_as_error_not_success` (~8343)

- [ ] **Step 1: Update the existing rate-limit test to the new signature (failing build first)**

In `backend/src/rag.rs`, the existing test calls:
`chat_report_issue(&pool, user_id, Some("Bug report"), Some("detail"), None).await;`
Change it to drop the trailing `active_budget_id` arg:
`chat_report_issue(&pool, user_id, Some("Bug report"), Some("detail")).await;`
(All assertions in that test stay the same — a rate-limited call writes no audit and returns an honest error.)

- [ ] **Step 2: Run to verify the build now fails against the old signature**

Run: `cd backend && cargo test --no-run 2>&1 | tail -20`
Expected: compile error — `chat_report_issue` arity mismatch (the caller at ~2842 still passes 5 args). This confirms the test is wired to the new signature before the impl exists.

- [ ] **Step 3: Update `chat_report_issue` to always write a user-scoped audit row and drop the `active_budget_id` param**

Replace the function signature + success branch in `backend/src/rag.rs`:

```rust
/// REPORT_ISSUE mutation extracted from `chat_endpoint` so the #[ignore] DB
/// tests can drive the real rate-limited / honest-failure path directly
/// (mirrors `chat_share_budget`). Files a GitHub issue on the user's behalf via
/// the shared, per-user-rate-limited `create_issue_core` (#191). Returns
/// `(success_log, error_message)`: on success a user-scoped `AI_REPORT_ISSUE`
/// audit row is written ALWAYS (#192 — REPORT_ISSUE is intentionally not
/// budget-scoped, so it is attributed to the user, never to a happens-to-be
/// active budget); on failure (missing title, the over-quota 429, or an upstream
/// error) the reason is surfaced via the error so the user is never told an
/// issue was filed when it wasn't.
pub async fn chat_report_issue(
    db: &sqlx::PgPool,
    user_id: Uuid,
    issue_title: Option<&str>,
    issue_body: Option<&str>,
) -> (Option<String>, Option<String>) {
    let title = issue_title.map(str::trim).filter(|t| !t.is_empty());
    match title {
        Some(title) => {
            match crate::github::create_issue_core(
                db,
                user_id,
                title,
                issue_body,
                crate::github::CHAT_FOOTER,
            )
            .await
            {
                Ok(issue) => {
                    let log = format!(
                        "Filed issue #{} with the development team: {}",
                        issue.number, issue.title
                    );
                    log_user_audit(db, user_id, "AI_REPORT_ISSUE", &log).await;
                    (Some(log), None)
                }
                Err((_status, msg)) => (
                    None,
                    Some(format!("I couldn't file that with the team: {}", msg)),
                ),
            }
        }
        None => (
            None,
            Some(
                "I need a short summary before I can file this with the team — what should the report say?"
                    .to_string(),
            ),
        ),
    }
}
```

Add `log_user_audit` to the existing budget import at the top of `rag.rs`:
change `use crate::budget::{check_permission, Permission, log_audit};`
to `use crate::budget::{check_permission, Permission, log_audit, log_user_audit};`

- [ ] **Step 4: Update the `chat_report_issue` caller in the `"REPORT_ISSUE"` dispatch arm (~2842)**

Change:
```rust
let (log, err) = chat_report_issue(&state.db, user_id, title, body, active_budget_id).await;
```
to:
```rust
let (log, err) = chat_report_issue(&state.db, user_id, title, body).await;
```

- [ ] **Step 5: Backfill `CREATE_REMINDER` (user-scoped when no active budget)**

In the `"CREATE_REMINDER"` arm (~2821), replace:
```rust
                        if let Some(bid) = active_budget_id {
                            log_audit(&state.db, bid, user_id, "AI_CREATE_REMINDER", &mutation_log.clone().unwrap()).await;
                        }
```
with:
```rust
                        match active_budget_id {
                            Some(bid) => log_audit(&state.db, bid, user_id, "AI_CREATE_REMINDER", &mutation_log.clone().unwrap()).await,
                            None => log_user_audit(&state.db, user_id, "AI_CREATE_REMINDER", &mutation_log.clone().unwrap()).await,
                        }
```

- [ ] **Step 6: Backfill `SET_USER_NAME` (user-scoped audit on a successful rename)**

In the `"SET_USER_NAME"` arm (~2850), replace the fire-and-forget UPDATE:
```rust
                    if !new_name.is_empty() && new_name.chars().count() <= 60 {
                        let _ = sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
                            .bind(new_name)
                            .bind(user_id)
                            .execute(&state.db)
                            .await;
                    }
```
with a success-checked write that logs a user-scoped audit row:
```rust
                    if !new_name.is_empty() && new_name.chars().count() <= 60 {
                        match sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
                            .bind(new_name)
                            .bind(user_id)
                            .execute(&state.db)
                            .await
                        {
                            Ok(r) if r.rows_affected() > 0 => {
                                log_user_audit(
                                    &state.db,
                                    user_id,
                                    "AI_SET_USER_NAME",
                                    &format!("set display name to '{}'", new_name),
                                )
                                .await;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::error!(error = %e, %user_id, "AI set user name failed");
                            }
                        }
                    }
```

- [ ] **Step 7: Run the build + the existing chat test**

Run: `cd backend && cargo check 2>&1 | tail -15`
Expected: clean (no arity errors, no unused-import/var warnings for `log_audit`/`log_user_audit`/`active_budget_id`).
Run: `cd backend && cargo test -- --ignored chat_report_issue_surfaces_rate_limit 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#192): user-scoped audit for chat REPORT_ISSUE + reminder/name backfills"
```

---

### Task 3: REST `/issues-create` writes an audit record

**Files:**
- Modify: `backend/src/github.rs` — `create_issue` handler (~398)

- [ ] **Step 1: Add the audit write to the REST handler**

In `backend/src/github.rs`, replace the `create_issue` handler body:

```rust
pub async fn create_issue(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<CreateIssueRequest>,
) -> Result<Json<GithubIssueItem>, (StatusCode, String)> {
    let issue =
        create_issue_core(&state.db, user_id, &req.title, req.body.as_deref(), APP_FOOTER).await?;
    // User-level audit (#192): the REST /issues-create path is not budget-scoped,
    // so attribute the filing to the user (NULL budget_id). Action `REPORT_ISSUE`
    // (no `AI_` prefix) marks this as the deliberate command-palette path, distinct
    // from the chat assistant's `AI_REPORT_ISSUE` (mirrors APP_FOOTER vs CHAT_FOOTER).
    crate::budget::log_user_audit(
        &state.db,
        user_id,
        "REPORT_ISSUE",
        &format!("Filed issue #{}: {}", issue.number, issue.title),
    )
    .await;
    Ok(Json(issue))
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd backend && cargo check 2>&1 | tail -15`
Expected: clean. (`create_issue_core` already errors out before the audit on empty title / over-quota / missing config / upstream failure, so the audit only runs on a confirmed-successful filing.)

- [ ] **Step 3: Run the full non-ignored suite + the issue/audit DB tests**

Run: `cd backend && cargo test 2>&1 | tail -20`
Expected: all PASS.
Run: `cd backend && cargo test -- --ignored 2>&1 | tail -30`
Expected: all PASS (new audit tests, the #194 rate-limit tests, the updated chat test).

- [ ] **Step 4: Commit**

```bash
git add backend/src/github.rs
git commit -m "feat(#192): audit record for REST /issues-create"
```

---

## Verification checklist (maps to ACs / SCs)
- AC1 / SC1: chat filing with no active budget → `AI_REPORT_ISSUE` user-scoped row — `chat_report_issue` calls `log_user_audit` unconditionally (Task 2); row shape proven by `log_user_audit_writes_user_scoped_row` (Task 1).
- AC2 / SC2: REST `/issues-create` → `REPORT_ISSUE` user-scoped row on success (Task 3) over the same `log_user_audit` mechanism (Task 1 test).
- AC3 / SC4: `cargo test` + `cargo test -- --ignored` green; migration nullability exercised by the NULL-budget insert.
- SC3: migration drops NOT NULL; regression test `log_audit_still_writes_budget_scoped_row_after_nullable_migration` confirms budget-scoped writes still work.
- Out-of-band: one new SQLx migration (auto-applied on backend deploy); applied to the local test DB manually for the `#[ignore]` tests (Task 1 Step 2). No config/secret/IaC/flag changes.
