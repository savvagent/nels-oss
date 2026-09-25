# Rollup Type/Strategy Edit Guard (nels#317) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the `budget_type`/`budget_strategy` rollup-compatibility invariant that
`validate_rollup_link` already enforces at LINK time so it also holds across ordinary edits to
an already-linked budget — on REST `PUT /budgets/:id`, chat `UPDATE_BUDGET`'s
`budget_strategy` path, and the frontend's `BudgetDetails.svelte` edit affordances.

**Architecture:** One new pure guard (`ensure_rollup_type_or_strategy_unchanged`) and one new
shared async helper (`is_rollup_parent`, extracted from `link_rollup`'s existing inline query)
in `backend/src/budget.rs`, wired into `update_budget` (REST) and `apply_budget_strategy_update`
(chat, `backend/src/rag.rs`). A matching frontend pure helper (`isRollupLinked`) in
`frontend/src/lib/budgetDetails.js`, wired into `BudgetDetails.svelte`'s Type/Strategy edit
affordances. No schema change, no migration.

**Tech Stack:** Rust (axum, sqlx/PostgreSQL) backend; Svelte 5 + vitest frontend. No new
dependencies.

Full design rationale: `docs/superpowers/specs/2026-07-04-rollup-type-strategy-edit-guard-design.md`.

---

## File Structure

- **Modified `backend/src/budget.rs`**:
  - New `pub fn ensure_rollup_type_or_strategy_unchanged(...)` — pure guard, placed
    immediately after `validate_rollup_link` (before `rollup_category_name`).
  - New `pub async fn is_rollup_parent(pool, budget_id)` — placed immediately after
    `ensure_rollup_type_or_strategy_unchanged`.
  - `link_rollup`: its inline `child_is_parent` query replaced with a call to
    `is_rollup_parent`.
  - `update_budget`: new pre-write guard block using both new functions.
  - New unit tests for `ensure_rollup_type_or_strategy_unchanged` in the existing pure-test
    section (next to `validate_rollup_link_*` tests).
  - New DB-backed (`--ignored`) tests for `update_budget`'s rollup guard, in the "Rollup (#52)
    DB-backed test helpers" section (reusing `rollup_test_setup`/`seed_rollup_budget`/
    `rollup_cleanup`).
- **Modified `backend/src/rag.rs`**:
  - `apply_budget_strategy_update`: restructured to SELECT current state + check rollup role
    before writing.
  - New DB-backed (`--ignored`) test for the chat path's rollup guard, next to
    `chat_update_budget_changes_budget_strategy`.
- **Modified `frontend/src/lib/budgetDetails.js`**: new `export function isRollupLinked(budget)`,
  placed after `isReadOnly`.
- **Modified `frontend/src/lib/budgetDetails.test.js`**: new `describe("isRollupLinked", ...)`
  block, after the existing `isReadOnly` block.
- **Modified `frontend/src/lib/BudgetDetails.svelte`**: `rollupLocked` derived value,
  `startEdit` guard, Type/Strategy button `disabled`/`title`/pencil-icon conditions.
- **Modified `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json`**: new
  `budgetDetails.rollupLockedHint` key.
- **Created** (this plan + its paired spec) — already committed:
  `docs/superpowers/specs/2026-07-04-rollup-type-strategy-edit-guard-design.md`,
  `docs/superpowers/plans/2026-07-04-rollup-type-strategy-edit-guard.md`.

---

## Task 1: Backend guard — pure function, shared helper, `update_budget`, `link_rollup` refactor

**Files:**
- Modify: `backend/src/budget.rs`

- [ ] **Step 1: Read the current exact code before editing**

Read `backend/src/budget.rs` around `validate_rollup_link` (lines 839-907),
`link_rollup` (lines 2626-2782, focusing on 2678-2698), and `update_budget` (lines
1863-2012, focusing on 1893-1905) to confirm line numbers haven't drifted since this plan
was written (they were verified current as of this plan's writing, but re-verify — a
plan-critique/implementer must never blind-apply line numbers without reading first).

- [ ] **Step 2: Add the pure guard function**

Add immediately after `validate_rollup_link`'s closing `}` (currently line 907) and before
`rollup_category_name`'s doc comment:

```rust
/// Whether an in-place `budget_type` or `budget_strategy` change is allowed on a budget that
/// participates in a rollup relationship (#317). `validate_rollup_link` (#52/#300) already
/// rejects LINKING two budgets whose `budget_type` or `budget_strategy` differ; without this
/// guard, an ordinary edit to an ALREADY-linked budget could silently recreate that exact
/// mismatch after the link exists (neither REST `update_budget` nor chat's UPDATE_BUDGET
/// re-checked the rollup relationship before this issue).
///
/// `is_child`/`is_parent` describe the budget BEING EDITED's own rollup role, derived from the
/// DB by the caller (`is_child` = its `rollup_parent_id IS NOT NULL`; `is_parent` = via
/// `is_rollup_parent`, below). `field_label` is the human label used in the error message
/// ("budget type" / "budgeting strategy"). Only an ACTUAL change (`requested != current`) is
/// rejected — resubmitting the budget's current value is always a no-op, mirroring how
/// re-linking to the SAME parent is idempotent rather than an error (see `link_rollup`) and
/// how the frontend's `buildEditPatch` already treats an unchanged value as nothing-to-save.
///
/// A budget that is a rollup PARENT is blocked from changing its OWN `budget_type`/
/// `budget_strategy` while it has ANY child (archived or not) — not just when the new value
/// would conflict with a specific child — because pre-existing rollup links are never
/// retroactively re-validated (a parent's children are not guaranteed to already agree with
/// each other), so "does this match child X" is not well-defined for a parent in general.
/// The same blanket rule applies to a CHILD for symmetry (one rule, one function, one message
/// shape for both roles).
pub fn ensure_rollup_type_or_strategy_unchanged(
    is_child: bool,
    is_parent: bool,
    field_label: &str,
    current: &str,
    requested: &str,
) -> Result<(), (StatusCode, String)> {
    if requested == current || (!is_child && !is_parent) {
        return Ok(());
    }
    let role = if is_child { "child" } else { "parent" };
    Err((
        StatusCode::CONFLICT,
        format!(
            "Budgets can only be rolled up together when they share the same {field_label}. \
             This budget is a rollup {role}; changing its {field_label} from '{current}' to \
             '{requested}' would break that. Unlink it first if you need to change this."
        ),
    ))
}

/// Whether ANY budget (archived or not) is currently rolled up into `budget_id` — i.e.
/// whether `budget_id` is a rollup PARENT (#317). Extracted from `link_rollup`'s own
/// `child_is_parent` guard (identical SQL) so `update_budget`, chat's
/// `apply_budget_strategy_update`, and `link_rollup` itself share one definition instead of
/// three copies drifting apart.
pub async fn is_rollup_parent(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $1)")
        .bind(budget_id)
        .fetch_one(pool)
        .await
        .map_err(internal_error)
}
```

- [ ] **Step 3: Add unit tests for the pure guard**

Find the existing `validate_rollup_link_*` unit tests (search for
`fn validate_rollup_link_allows_matching_type_and_strategy`, around line 5486). Add a new test
block immediately after the last `validate_rollup_link_*` test in that group (after
`validate_rollup_link_chain_violation_still_wins_over_strategy_mismatch`, before whatever pure
test follows it):

```rust
#[test]
fn ensure_rollup_type_or_strategy_unchanged_allows_non_participant_change() {
    assert!(ensure_rollup_type_or_strategy_unchanged(
        false, false, "budget type", "time_based", "project"
    )
    .is_ok());
}

#[test]
fn ensure_rollup_type_or_strategy_unchanged_allows_unchanged_value_on_child() {
    assert!(ensure_rollup_type_or_strategy_unchanged(
        true, false, "budget type", "time_based", "time_based"
    )
    .is_ok());
}

#[test]
fn ensure_rollup_type_or_strategy_unchanged_rejects_change_on_child() {
    let err = ensure_rollup_type_or_strategy_unchanged(
        true, false, "budget type", "time_based", "project",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("child"));
    assert!(err.1.contains("time_based"));
    assert!(err.1.contains("project"));
}

#[test]
fn ensure_rollup_type_or_strategy_unchanged_rejects_change_on_parent() {
    let err = ensure_rollup_type_or_strategy_unchanged(
        false, true, "budgeting strategy", "limit_spent_remaining", "zero_based",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("parent"));
}

#[test]
fn ensure_rollup_type_or_strategy_unchanged_rejects_change_when_both_flags_set() {
    // Structurally shouldn't happen (validate_rollup_link prevents a budget from becoming
    // both), but the guard must still reject rather than silently allow.
    let err = ensure_rollup_type_or_strategy_unchanged(
        true, true, "budget type", "time_based", "project",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
}
```

- [ ] **Step 4: Run the new unit tests**

Run: `cd backend && cargo test ensure_rollup_type_or_strategy_unchanged`
Expected: 5 tests pass (they'll fail to compile until Step 2's function exists — if you're
following TDD strictly, write Step 3 first, confirm a compile failure, then add Step 2's
function and re-run to green; either order is fine here since this is a pure function with no
external dependencies).

- [ ] **Step 5: Refactor `link_rollup` to use `is_rollup_parent`**

In `link_rollup` (currently lines 2681-2688), replace:

```rust
    let parent_is_child = parent.rollup_parent_id.is_some();
    let child_is_parent: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $1)",
    )
    .bind(child_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
```

with:

```rust
    let parent_is_child = parent.rollup_parent_id.is_some();
    let child_is_parent = is_rollup_parent(&state.db, child_id).await?;
```

- [ ] **Step 6: Run the existing rollup test suite to confirm the refactor is behavior-preserving**

Run: `cd backend && cargo test -- --ignored link_rollup`
Expected: all `link_rollup_*` tests still pass (this is a pure extraction — same SQL, same
binding, same error mapping). Requires local Postgres (`podman-compose up -d` if not already
running; check with `pg_isready -h 127.0.0.1 -p 6153`).

- [ ] **Step 7: Wire the guard into `update_budget`**

In `update_budget`, immediately after the `existing` fetch (currently ends at line 1905,
`.ok_or((StatusCode::NOT_FOUND, "Budget not found".to_string()))?;`) and BEFORE the
`resolved_type` computation, insert:

```rust
    // #317: a budget in a rollup relationship (parent or child) must keep the same
    // budget_type/budget_strategy as its counterpart(s) — validate_rollup_link enforces this
    // at LINK time; this closes the gap where an ordinary edit could silently recreate the
    // exact mismatch link time rejects. Only an ACTUAL change is blocked (see
    // ensure_rollup_type_or_strategy_unchanged).
    if payload.budget_type.is_some() || payload.budget_strategy.is_some() {
        let is_child = existing.rollup_parent_id.is_some();
        let is_parent = is_rollup_parent(&state.db, budget_id).await?;
        if let Some(bt) = &payload.budget_type {
            ensure_rollup_type_or_strategy_unchanged(
                is_child, is_parent, "budget type", &existing.budget_type, bt,
            )?;
        }
        if let Some(bs) = &payload.budget_strategy {
            ensure_rollup_type_or_strategy_unchanged(
                is_child, is_parent, "budgeting strategy", &existing.budget_strategy, bs,
            )?;
        }
    }
```

- [ ] **Step 8: `cargo check`**

Run: `cd backend && cargo check`
Expected: clean compile (same pre-existing warning count as before this task; no new warnings).

- [ ] **Step 9: Write DB-backed tests for `update_budget`'s rollup guard**

Find the "Rollup (#52) DB-backed test helpers" section (`rollup_test_setup`,
`seed_rollup_budget`, `rollup_cleanup`, around lines 6522-6610) and the existing
`update_budget_preserves_budget_strategy_when_absent_and_updates_when_present` test (around
line 7025). Add the following new tests immediately after that existing test:

```rust
// #317: update_budget rejects an ACTUAL budget_type change on a rollup CHILD, leaving the
// row unchanged — closing the gap where an ordinary edit could silently recreate the
// mismatch validate_rollup_link rejects at link time.
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_rejects_budget_type_change_on_rollup_child() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed (matching type/strategy)");

    let err = update_budget(
        State(state.clone()),
        Path(child),
        Extension(owner),
        Json(BudgetPayload {
            name: "child-renamed".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: Some("project".to_string()),
            auto_renew: None,
            amount_mode: None,
            budget_strategy: None,
        }),
    )
    .await
    .expect_err("changing budget_type on a rollup child must be rejected");
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("budget type"));

    let stored_type: String = sqlx::query_scalar("SELECT budget_type FROM budgets WHERE id = $1")
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("fetch budget_type");
    assert_eq!(stored_type, "time_based", "budget_type must remain unchanged after the rejected edit");

    rollup_cleanup(&pool, &[owner]).await;
}

// #317: same guard, budget_strategy axis, on a rollup CHILD.
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_rejects_budget_strategy_change_on_rollup_child() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed");

    let err = update_budget(
        State(state.clone()),
        Path(child),
        Extension(owner),
        Json(BudgetPayload {
            name: "child-renamed".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: None,
            auto_renew: None,
            amount_mode: None,
            budget_strategy: Some("zero_based".to_string()),
        }),
    )
    .await
    .expect_err("changing budget_strategy on a rollup child must be rejected");
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("budgeting strategy"));

    let stored_strategy: String =
        sqlx::query_scalar("SELECT budget_strategy FROM budgets WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .expect("fetch budget_strategy");
    assert_eq!(stored_strategy, "limit_spent_remaining");

    rollup_cleanup(&pool, &[owner]).await;
}

// #317: the guard also blocks the PARENT's own type/strategy from changing while it has a
// child — both axes, one test each.
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_rejects_budget_type_change_on_rollup_parent() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed");

    let err = update_budget(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(BudgetPayload {
            name: "parent-renamed".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: Some("project".to_string()),
            auto_renew: None,
            amount_mode: None,
            budget_strategy: None,
        }),
    )
    .await
    .expect_err("changing budget_type on a rollup parent must be rejected");
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("parent"));

    rollup_cleanup(&pool, &[owner]).await;
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_rejects_budget_strategy_change_on_rollup_parent() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed");

    let err = update_budget(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(BudgetPayload {
            name: "parent-renamed".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: None,
            auto_renew: None,
            amount_mode: None,
            budget_strategy: Some("zero_based".to_string()),
        }),
    )
    .await
    .expect_err("changing budget_strategy on a rollup parent must be rejected");
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("parent"));

    rollup_cleanup(&pool, &[owner]).await;
}

// #317: resubmitting the SAME budget_type/budget_strategy on a rollup child is a no-op, not
// an error (Assumption 2 of the spec) — an update that merely echoes the current value back
// must still succeed.
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_allows_unchanged_budget_type_and_strategy_on_rollup_child() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed");

    let updated = update_budget(
        State(state.clone()),
        Path(child),
        Extension(owner),
        Json(BudgetPayload {
            name: "child-renamed".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: Some("time_based".to_string()),
            auto_renew: None,
            amount_mode: None,
            budget_strategy: Some("limit_spent_remaining".to_string()),
        }),
    )
    .await
    .expect("resubmitting the current budget_type/budget_strategy must succeed as a no-op");
    assert_eq!(updated.0.name, "child-renamed", "the unrelated name change must still apply");

    rollup_cleanup(&pool, &[owner]).await;
}

// #317: an update that omits both budget_type and budget_strategy (e.g. a plain rename) on a
// rollup child/parent must be entirely unaffected by the new guard.
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_allows_unrelated_field_change_on_rollup_child() {
    let (pool, owner) = rollup_test_setup().await;
    let state = test_state(&pool);

    let parent = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
    let child = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;
    link_rollup(
        State(state.clone()),
        Path(parent),
        Extension(owner),
        Json(RollupPayload { child_budget_id: child }),
    )
    .await
    .expect("initial link must succeed");

    let updated = update_budget(
        State(state.clone()),
        Path(child),
        Extension(owner),
        Json(BudgetPayload {
            name: "renamed-without-touching-type-or-strategy".to_string(),
            description: None,
            time_frame: "monthly".to_string(),
            budget_limit: None,
            rollover_enabled: None,
            budget_type: None,
            auto_renew: None,
            amount_mode: None,
            budget_strategy: None,
        }),
    )
    .await
    .expect("a rename with budget_type/budget_strategy both absent must succeed");
    assert_eq!(updated.0.name, "renamed-without-touching-type-or-strategy");

    rollup_cleanup(&pool, &[owner]).await;
}
```

- [ ] **Step 10: Run the new DB-backed tests**

Run: `cd backend && cargo test -- --ignored update_budget_rejects update_budget_allows`
Expected: 6 tests pass (2 child-reject + 2 parent-reject + 2 allow). Requires local Postgres
(`pg_isready -h 127.0.0.1 -p 6153`; if not running, `podman-compose up -d` from the repo
root first).

- [ ] **Step 11: Run the full existing rollup + budget test suites to confirm no regression**

Run: `cd backend && cargo test -- --ignored rollup` and `cd backend && cargo test -- --ignored budget_strategy`
Expected: all pre-existing tests (link/unlink, mismatch rejection, `update_budget_preserves_budget_strategy_when_absent_and_updates_when_present`, etc.) still pass unchanged.

- [ ] **Step 12: `cargo fmt` and `cargo clippy`**

Run: `cd backend && cargo fmt && cargo clippy --all-targets`
Expected: `cargo fmt` makes no unexpected changes beyond this task's new code; `cargo clippy`
shows no new warnings introduced by this task (pre-existing warnings, if any, are unaffected).

- [ ] **Step 13: Commit**

```bash
cd backend
git add src/budget.rs
git commit -m "fix(#317): reject budget_type/budget_strategy edits on a linked rollup budget (REST)"
```

---

## Task 2: Chat guard — `apply_budget_strategy_update`

**Files:**
- Modify: `backend/src/rag.rs`

- [ ] **Step 1: Read the current exact code before editing**

Read `apply_budget_strategy_update` in full (currently `backend/src/rag.rs:5251-5281`) and
`chat_update_budget_changes_budget_strategy` (around line 9782) to confirm nothing drifted.

- [ ] **Step 2: Write the failing DB-backed test first**

Add this test immediately after `chat_update_budget_changes_budget_strategy` (ends around line
9860):

```rust
// #317: apply_budget_strategy_update rejects an ACTUAL strategy change on a budget that is a
// rollup CHILD or PARENT, leaving the row and audit log untouched — mirrors the REST
// update_budget guard for the one axis chat can change on an existing budget.
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_update_budget_rejects_strategy_change_on_rollup_participant() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
    });
    let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

    let suffix = uuid::Uuid::new_v4();
    let owner_id = uuid::Uuid::new_v4();
    let parent_id = uuid::Uuid::new_v4();
    let child_id = uuid::Uuid::new_v4();

    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
        .bind(owner_id)
        .bind(format!("owner-{suffix}@example.test"))
        .execute(&pool)
        .await
        .expect("seed user");
    sqlx::query(
        "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default, budget_strategy) \
         VALUES ($1, $2, 'Parent', 'monthly', 0.0, TRUE, 'limit_spent_remaining')",
    )
    .bind(parent_id)
    .bind(owner_id)
    .execute(&pool)
    .await
    .expect("seed parent");
    sqlx::query(
        "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default, budget_strategy, rollup_parent_id) \
         VALUES ($1, $2, 'Child', 'monthly', 0.0, FALSE, 'limit_spent_remaining', $3)",
    )
    .bind(child_id)
    .bind(owner_id)
    .bind(parent_id)
    .execute(&pool)
    .await
    .expect("seed child linked to parent");

    async fn strategy_of(pool: &sqlx::PgPool, id: uuid::Uuid) -> String {
        sqlx::query_scalar("SELECT budget_strategy FROM budgets WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch budget_strategy")
    }

    // --- CHILD: an actual change is rejected, row unchanged, no audit row written ---
    let err = apply_budget_strategy_update(&pool, child_id, owner_id, "zero_based")
        .await
        .expect_err("changing strategy on a rollup child must be rejected");
    assert!(err.contains("rollup"), "error should mention the rollup relationship, got: {:?}", err);
    assert_eq!(strategy_of(&pool, child_id).await, "limit_spent_remaining");
    let child_audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'AI_UPDATE_BUDGET'",
    )
    .bind(child_id)
    .fetch_one(&pool)
    .await
    .expect("count child audit rows");
    assert_eq!(child_audit_count, 0, "a rejected change must not write an audit row");

    // --- PARENT: same rejection ---
    let err = apply_budget_strategy_update(&pool, parent_id, owner_id, "zero_based")
        .await
        .expect_err("changing strategy on a rollup parent must be rejected");
    assert!(err.contains("rollup"));
    assert_eq!(strategy_of(&pool, parent_id).await, "limit_spent_remaining");

    // --- Resubmitting the SAME value on the child is a no-op success ---
    let msg = apply_budget_strategy_update(&pool, child_id, owner_id, "limit_spent_remaining")
        .await
        .expect("resubmitting the unchanged strategy must succeed");
    assert!(msg.contains("limit_spent_remaining"));

    // Cleanup.
    sqlx::query("DELETE FROM audit_logs WHERE budget_id = ANY($1)")
        .bind(&[parent_id, child_id][..])
        .execute(&pool)
        .await
        .expect("cleanup audit");
    sqlx::query("DELETE FROM budgets WHERE owner_id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .expect("cleanup budgets");
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .expect("cleanup user");
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && cargo test -- --ignored chat_update_budget_rejects_strategy_change_on_rollup_participant`
Expected: FAIL (currently the change on the child/parent both succeed silently — the first
`expect_err` panics because the call returns `Ok`).

- [ ] **Step 4: Implement the guard in `apply_budget_strategy_update`**

Replace the full function body (currently lines 5251-5281) with:

```rust
async fn apply_budget_strategy_update(
    db: &sqlx::PgPool,
    bid: Uuid,
    user_id: Uuid,
    strategy: &str,
) -> Result<String, String> {
    if let Err((_, msg)) = crate::budget::validate_budget_strategy(strategy) {
        return Err(format!("I couldn't update that budget: {}", msg));
    }

    // #317: a budget in a rollup relationship (parent or child) must keep the same
    // budget_strategy as its counterpart(s) — validate_rollup_link already enforces this at
    // LINK time; this closes the gap where an ordinary chat edit could silently recreate that
    // exact mismatch. Only an ACTUAL change is blocked (see
    // ensure_rollup_type_or_strategy_unchanged).
    let row = sqlx::query("SELECT budget_strategy, rollup_parent_id FROM budgets WHERE id = $1")
        .bind(bid)
        .fetch_optional(db)
        .await;
    let (current, is_child) = match row {
        Ok(Some(r)) => {
            let current: String = r.get("budget_strategy");
            let is_child = r.get::<Option<Uuid>, _>("rollup_parent_id").is_some();
            (current, is_child)
        }
        Ok(None) => {
            tracing::warn!(%bid, "AI budget_strategy update: budget row not found");
            return Err("I couldn't update that budget. Please try again.".to_string());
        }
        Err(e) => {
            tracing::error!(error = %e, %bid, "AI budget_strategy update: pre-check lookup failed");
            return Err("I couldn't update that budget right now due to a temporary problem — please try again.".to_string());
        }
    };
    let is_parent = match crate::budget::is_rollup_parent(db, bid).await {
        Ok(v) => v,
        Err((_, msg)) => {
            tracing::error!(%bid, "AI budget_strategy update: is_rollup_parent lookup failed: {}", msg);
            return Err("I couldn't update that budget right now due to a temporary problem — please try again.".to_string());
        }
    };
    if let Err((_, msg)) = crate::budget::ensure_rollup_type_or_strategy_unchanged(
        is_child, is_parent, "budgeting strategy", &current, strategy,
    ) {
        return Err(format!("I can't update that budget: {}", msg));
    }

    match sqlx::query("UPDATE budgets SET budget_strategy = $1 WHERE id = $2 RETURNING name")
        .bind(strategy)
        .bind(bid)
        .fetch_optional(db)
        .await
    {
        Ok(Some(row)) => {
            let bname: String = row.get("name");
            let strategy_msg = format!("set '{}' to use the {} strategy", bname, strategy);
            log_audit(db, bid, user_id, "AI_UPDATE_BUDGET", &strategy_msg).await;
            Ok(strategy_msg)
        }
        Ok(None) => {
            tracing::warn!(%bid, "AI budget_strategy update: budget row not found");
            Err("I couldn't update that budget. Please try again.".to_string())
        }
        Err(e) => {
            tracing::error!(error = %e, %bid, "AI budget_strategy update failed");
            Err("I couldn't update that budget. Please try again.".to_string())
        }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd backend && cargo test -- --ignored chat_update_budget_rejects_strategy_change_on_rollup_participant`
Expected: PASS.

- [ ] **Step 6: Run the pre-existing chat budget_strategy test to confirm no regression**

Run: `cd backend && cargo test -- --ignored chat_update_budget_changes_budget_strategy`
Expected: PASS unchanged (a non-rollup budget's strategy change still succeeds exactly as
before — `is_child`/`is_parent` both resolve `false` for it, so the new guard is a no-op).

- [ ] **Step 7: `cargo fmt`, `cargo clippy`, full non-ignored suite**

Run: `cd backend && cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt clean, no new clippy warnings, full default (non-DB) suite green.

- [ ] **Step 8: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "fix(#317): reject budget_strategy edits on a linked rollup budget (chat)"
```

---

## Task 3: Frontend — disable Type/Strategy editors for a rollup-linked budget

**Files:**
- Modify: `frontend/src/lib/budgetDetails.js`
- Modify: `frontend/src/lib/budgetDetails.test.js`
- Modify: `frontend/src/lib/BudgetDetails.svelte`
- Modify: `frontend/src/lib/i18n/locales/en.json`, `de.json`, `es.json`, `fr.json`, `it.json`, `pt.json`

- [ ] **Step 1: Read the current exact code before editing**

Re-read `frontend/src/lib/budgetDetails.js` (full file, ~95 lines), the `isReadOnly` test block
in `frontend/src/lib/budgetDetails.test.js` (lines 208-231), and
`frontend/src/lib/BudgetDetails.svelte` lines 115-400 to confirm nothing drifted.

- [ ] **Step 2: Write the failing test for `isRollupLinked`**

In `frontend/src/lib/budgetDetails.test.js`, add `isRollupLinked` to the import list at the
top:

```js
import {
  TIME_FRAMES,
  BUDGET_STRATEGIES,
  buildUpdatePayload,
  buildEditPatch,
  rollupSummary,
  isReadOnly,
  isRollupLinked,
} from "./budgetDetails.js";
```

Then add a new `describe` block immediately after the existing `describe("isReadOnly", ...)`
block (after its closing `});`, currently line 231):

```js
describe("isRollupLinked", () => {
  it("is false when neither rollup_parent_id nor rollup_child_ids is set", () => {
    expect(isRollupLinked({ rollup_parent_id: null, rollup_child_ids: [] })).toBe(false);
  });

  it("is true when rollup_parent_id is set (this budget is a CHILD)", () => {
    expect(
      isRollupLinked({ rollup_parent_id: "11111111-1111-1111-1111-111111111111", rollup_child_ids: [] }),
    ).toBe(true);
  });

  it("is true when rollup_child_ids is non-empty (this budget is a PARENT)", () => {
    expect(
      isRollupLinked({
        rollup_parent_id: null,
        rollup_child_ids: ["22222222-2222-2222-2222-222222222222"],
      }),
    ).toBe(true);
  });

  it("is false when rollup_child_ids is present but empty", () => {
    expect(isRollupLinked({ rollup_parent_id: null, rollup_child_ids: [] })).toBe(false);
  });

  it("is false for a null/undefined budget", () => {
    expect(isRollupLinked(null)).toBe(false);
    expect(isRollupLinked(undefined)).toBe(false);
  });

  it("is false when rollup_child_ids is entirely absent from the object", () => {
    expect(isRollupLinked({ rollup_parent_id: null })).toBe(false);
  });
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd frontend && pnpm test -- budgetDetails`
Expected: FAIL with `isRollupLinked is not a function` (or similar import error).

- [ ] **Step 4: Implement `isRollupLinked` in `budgetDetails.js`**

Add immediately after the existing `isReadOnly` function (at the end of the file):

```js
// Whether `budget` participates in a rollup relationship as either a PARENT (has one or more
// linked children) or a CHILD (`rollup_parent_id` set) (#317). Unlike `isReadOnly`, this does
// NOT make the whole budget read-only — only the Type/Strategy fields are locked while linked
// (see BudgetDetails.svelte) — mirroring the backend's #317 reject-on-edit guard: changing
// either field while rolled up would silently recreate the exact type/strategy mismatch
// `validate_rollup_link` rejects at link time.
export function isRollupLinked(budget) {
  return !!(budget?.rollup_parent_id || (budget?.rollup_child_ids?.length ?? 0) > 0);
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd frontend && pnpm test -- budgetDetails`
Expected: PASS, all `isRollupLinked` cases plus the full existing `budgetDetails.test.js` suite
green (no regression).

- [ ] **Step 6: Wire `isRollupLinked` into `BudgetDetails.svelte`**

Update the import (currently lines 9-16):

```svelte
  import {
    TIME_FRAMES,
    BUDGET_STRATEGIES,
    buildUpdatePayload,
    buildEditPatch,
    rollupSummary,
    isReadOnly,
    isRollupLinked,
  } from "./budgetDetails.js";
```

Update `startEdit` (currently lines 123-128) to also guard the two locked fields:

```svelte
  function startEdit(field, currentValue) {
    if (!budget || isReadOnly(budget) || saving) return;
    if ((field === "budget_type" || field === "budget_strategy") && isRollupLinked(budget)) return;
    editingField = field;
    draftValue = currentValue;
    saveError = "";
  }
```

Add a new derived value next to the existing `readOnly`/`rollup` derived values (currently
lines 212-213):

```svelte
  let readOnly = $derived(isReadOnly(budget));
  let rollupLocked = $derived(isRollupLinked(budget));
  let rollup = $derived(rollupSummary(budget, parentName, childNames));
```

- [ ] **Step 7: Update the Type button block**

Replace the Type field's non-editing `<button>` block (currently lines 333-347):

```svelte
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("budget_type", budget.budget_type)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_type === "project"
                ? $_("budgetDetails.typeProject")
                : $_("budgetDetails.typePeriodic")}
            </span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
```

with:

```svelte
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly || rollupLocked}
            onclick={() => startEdit("budget_type", budget.budget_type)}
            title={readOnly
              ? undefined
              : rollupLocked
                ? $_("budgetDetails.rollupLockedHint")
                : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_type === "project"
                ? $_("budgetDetails.typeProject")
                : $_("budgetDetails.typePeriodic")}
            </span>
            {#if !readOnly && !rollupLocked}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
```

- [ ] **Step 8: Update the Strategy button block**

Replace the Strategy field's non-editing `<button>` block (currently lines 379-393) the same
way:

```svelte
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly || rollupLocked}
            onclick={() => startEdit("budget_strategy", budget.budget_strategy)}
            title={readOnly
              ? undefined
              : rollupLocked
                ? $_("budgetDetails.rollupLockedHint")
                : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_strategy === "zero_based"
                ? $_("budgetDetails.strategyZeroBased")
                : $_("budgetDetails.strategyLimitSpentRemaining")}
            </span>
            {#if !readOnly && !rollupLocked}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
```

(This is the same block that begins right after
`{#if editingField === "budget_strategy"}` — only the `{:else}` branch changes; the `<select>`
editing branch above it is untouched, since once `editingField` is already set to
`"budget_strategy"` the edit is already in flight and `rollupLocked` cannot have let it start
in the first place.)

- [ ] **Step 9: Add the new i18n key to all 6 locales**

In each of `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json`, inside the `budgetDetails`
object, add a new `rollupLockedHint` key immediately after the existing `editHint` key. Exact
line to find in each file: `"editHint": "..."` (English text confirmed present at
`en.json:278`; the other 5 locales have the same key at the same relative position in their own
`budgetDetails` block — find it by searching each file, don't assume identical line numbers).

`en.json`:
```json
    "editHint": "Click to edit",
    "rollupLockedHint": "Unlink this budget's rollup to change its type or strategy",
```

`de.json` (German):
```json
    "editHint": "Zum Bearbeiten klicken",
    "rollupLockedHint": "Löse den Rollup-Verbund dieses Budgets, um Typ oder Strategie zu ändern",
```

`es.json` (Spanish):
```json
    "editHint": "Haz clic para editar",
    "rollupLockedHint": "Desvincula el rollup de este presupuesto para cambiar su tipo o estrategia",
```

`fr.json` (French):
```json
    "editHint": "Cliquer pour modifier",
    "rollupLockedHint": "Dissociez le regroupement de ce budget pour changer son type ou sa stratégie",
```

`it.json` (Italian):
```json
    "editHint": "Clicca per modificare",
    "rollupLockedHint": "Scollega il rollup di questo budget per cambiarne il tipo o la strategia",
```

`pt.json` (Portuguese):
```json
    "editHint": "Clique para editar",
    "rollupLockedHint": "Desvincule o agrupamento deste orçamento para alterar seu tipo ou estratégia",
```

For each non-English file, read the existing `editHint` value first to confirm the exact
existing translated string/quoting style before inserting the new line (don't overwrite
`editHint` — only add `rollupLockedHint` after it, matching that file's actual current text,
which may differ slightly from the strings shown above as a starting point).

- [ ] **Step 10: Run the full frontend test suite**

Run: `cd frontend && pnpm test`
Expected: all tests pass, no regressions.

- [ ] **Step 11: Build check**

Run: `cd frontend && pnpm run build`
Expected: builds cleanly (catches any JSON syntax error introduced in the 6 locale files, and
any Svelte template error in `BudgetDetails.svelte`).

- [ ] **Step 12: Commit**

```bash
cd frontend
git add src/lib/budgetDetails.js src/lib/budgetDetails.test.js src/lib/BudgetDetails.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json src/lib/i18n/locales/pt.json
git commit -m "fix(#317): disable Type/Strategy editors for a rollup-linked budget"
```

---

## Task 4: Final verification pass

**Files:** none (verification only)

- [ ] **Step 1: Full backend default suite**

Run: `cd backend && cargo test`
Expected: green, no network/DB required.

- [ ] **Step 2: Full backend ignored (DB) suite**

Run: `cd backend && cargo test -- --ignored`
Expected: green (requires local Postgres at `127.0.0.1:6153`, `podman-compose up -d` if not
running).

- [ ] **Step 3: `cargo fmt --check` and `cargo clippy --all-targets`**

Run: `cd backend && cargo fmt --check && cargo clippy --all-targets`
Expected: no diff, no new warnings.

- [ ] **Step 4: Full frontend suite + build**

Run: `cd frontend && pnpm test && pnpm run build`
Expected: green.

- [ ] **Step 5: Manual sanity check of the exact issue repro**

Using `cargo run` (backend) + a local Postgres, or a targeted `cargo test -- --ignored`, confirm:
link budget A (parent) and B (child), both `time_based`/`limit_spent_remaining` → succeeds.
`PUT /budgets/:B_id` with `budget_type: "project"` → 409, B's `budget_type` still
`time_based`. This is already covered by
`update_budget_rejects_budget_type_change_on_rollup_child` (Task 1) — this step is a final
confirmation the automated test actually captures the issue's literal repro, not a new
manual step requiring a running server.
