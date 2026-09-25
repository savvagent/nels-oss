# Per-Budget Budgeting Strategy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two global per-user "show zero-based / show limit-spent-remaining"
toggles with a single per-budget `budget_strategy` column (`zero_based` |
`limit_spent_remaining`), threaded through the data model, REST API, chat
create/update flow (with a mandatory clarifying question when chat creates a budget
without a strategy), rollup-compatibility validation, and the frontend.

**Architecture:** A new `budgets.budget_strategy TEXT NOT NULL` column (CHECK-constrained,
migration-backfilled from the two legacy `users` toggles, which are then dropped along with
their dedicated `PATCH /user/preferences` endpoint) is threaded through `BudgetPayload`/
`BudgetListItem`/`db::Budget`/`create_budget`/`update_budget` exactly like the existing
`budget_type`/`amount_mode` columns. `validate_rollup_link` gains type+strategy equality
checks shared by both the REST and chat rollup-link call sites. Chat `CREATE_BUDGET` gets a
new `AiActionParams.budget_strategy` field, a system-prompt directive asking the LLM to solicit
it conversationally, and a server-side guard that blocks the insert and asks (rather than
defaulting) whenever it's absent — covering both the online-LLM and offline-router paths,
which share one handler arm. The frontend drops the Settings toggle panel, re-gates
`App.svelte`'s status strip off the active budget's `budget_strategy`, and adds a
`BudgetDetails.svelte` editable Strategy field mirroring the existing Type field.

**Tech Stack:** Rust (axum, sqlx/Postgres), Svelte 5 (Vite), vitest, `cargo test` (`#[ignore]`
for DB-backed tests).

**Full spec:** `docs/superpowers/specs/2026-07-04-budget-strategy-design.md` (also posted to
https://github.com/savvagent/nels/issues/300 as a comment).

**Local dev commands** (from `AGENTS.md`):
```bash
podman-compose up -d                    # start pgvector Postgres
cd backend && cargo check
cd backend && cargo test                # default suite: no DB, no network
cd backend && cargo test -- --ignored   # DB-backed suite (needs DATABASE_URL / local Postgres)
cd frontend && pnpm install
cd frontend && pnpm test                # vitest, if configured (check package.json scripts)
```
`DATABASE_URL` default: `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`.
Migrations in `backend/migrations` run automatically on `cargo run`/`cargo test` server start
(`sqlx::migrate!`) — no manual `sqlx migrate run` needed, but a fresh `podman-compose up -d`
Postgres has no schema until the backend binary/tests run once against it.

---

## Task 1: Migration + `budget_strategy` core (validator, `db::Budget`, `users` cleanup)

**Files:**
- Create: `backend/migrations/20260704000000_budget_strategy.sql`
- Modify: `backend/src/budget.rs` (add `ALLOWED_BUDGET_STRATEGIES`/`validate_budget_strategy`
  near `validate_amount_mode` — search for `pub fn validate_amount_mode`)
- Modify: `backend/src/db.rs:37-68` (`Budget` struct — add field; `User` struct at lines
  ~6-34 — remove the two legacy fields)
- Modify: `backend/src/auth.rs:67-90` (`MeResponse` struct + `From<db::User>` impl — remove
  the two fields), `backend/src/auth.rs:549-558` (fix the now-broken test)
- Modify: `backend/src/account.rs:496-533` (delete `PreferencesPayload` + `update_preferences`
  entirely), `backend/src/account.rs:645-725` (delete their tests)
- Modify: `backend/src/main.rs:242` (delete the `.route("/user/preferences", ...)` line)
- Test: `backend/src/budget.rs` (new `#[cfg(test)] mod` block near the existing
  `validate_amount_mode` tests)

- [ ] **Step 1: Write the failing unit test for `validate_budget_strategy`**

Find the existing test for `validate_amount_mode` in `backend/src/budget.rs` (search
`fn validate_amount_mode_rejects` or similar) and add a sibling test immediately after it:

```rust
#[test]
fn validate_budget_strategy_accepts_known_and_rejects_unknown() {
    assert!(validate_budget_strategy("zero_based").is_ok());
    assert!(validate_budget_strategy("limit_spent_remaining").is_ok());
    let err = validate_budget_strategy("fifty_thirty_twenty").unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert!(err.1.contains("zero_based"));
    assert!(err.1.contains("limit_spent_remaining"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test validate_budget_strategy_accepts_known_and_rejects_unknown`
Expected: FAIL — `validate_budget_strategy` is not defined.

- [ ] **Step 3: Implement `ALLOWED_BUDGET_STRATEGIES` + `validate_budget_strategy`**

In `backend/src/budget.rs`, immediately after the existing `validate_amount_mode` function
(search `pub fn validate_amount_mode`) and its `ALLOWED_AMOUNT_MODES` const, add:

```rust
/// Every allowed `budget_strategy` value (#300). Ship exactly these two now — the issue's
/// "extensible to 50/30/20, envelope, pay-yourself-first later" is a property of this being
/// a plain TEXT + CHECK column (additive to extend), not a requirement to pre-build
/// unimplemented methodologies today.
pub const ALLOWED_BUDGET_STRATEGIES: [&str; 2] = ["zero_based", "limit_spent_remaining"];

/// Validate a `budget_strategy` against the allowed set, returning a 400 for anything else.
/// Mirrors `validate_budget_type`/`validate_amount_mode`.
pub fn validate_budget_strategy(budget_strategy: &str) -> Result<(), (StatusCode, String)> {
    if ALLOWED_BUDGET_STRATEGIES.contains(&budget_strategy) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "budget_strategy must be 'zero_based' or 'limit_spent_remaining'".to_string(),
        ))
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test validate_budget_strategy_accepts_known_and_rejects_unknown`
Expected: PASS

- [ ] **Step 5: Write the migration**

Create `backend/migrations/20260704000000_budget_strategy.sql`:

```sql
-- #300: replace the two global per-user "show X summary" toggles with a per-budget
-- budgeting strategy. New budgets column, backfilled from the owner's existing toggles,
-- then the superseded users columns (and their dedicated PATCH /user/preferences endpoint,
-- removed in application code alongside this migration) are dropped.

ALTER TABLE budgets
  ADD COLUMN budget_strategy TEXT NOT NULL DEFAULT 'limit_spent_remaining';

ALTER TABLE budgets
  ADD CONSTRAINT budgets_budget_strategy_check
  CHECK (budget_strategy IN ('zero_based', 'limit_spent_remaining'));

-- Backfill: a budget whose owner had the zero-based toggle on migrates to 'zero_based'
-- (preserves the richer of the two views when both were on, the pre-existing default);
-- every other budget keeps the column DEFAULT already applied by ADD COLUMN above.
UPDATE budgets b
SET budget_strategy = 'zero_based'
FROM users u
WHERE b.owner_id = u.id
  AND u.show_zero_based_summary = TRUE;

-- Retire the superseded global per-user toggles.
ALTER TABLE users
  DROP COLUMN show_zero_based_summary,
  DROP COLUMN show_limit_spent_remaining_summary;
```

- [ ] **Step 6: Update `db::Budget` and `db::User` structs**

In `backend/src/db.rs`, add to the `Budget` struct (after the `rollup_parent_id` field, before
`created_at`, matching the file's existing doc-comment style for #300):

```rust
    /// Per-budget budgeting strategy (#300): 'zero_based' (tracks how much of the allocated
    /// total is still available to spend) or 'limit_spent_remaining' (tracks spending against
    /// a limit, shown as Budgeted/Spent/Remaining). Replaces the old global per-user
    /// show_zero_based_summary/show_limit_spent_remaining_summary toggles. DEFAULT
    /// 'limit_spent_remaining' via migration backfill (see 20260704000000_budget_strategy.sql).
    pub budget_strategy: String,
```

In the `User` struct, delete the two lines:
```rust
    pub show_zero_based_summary: bool,
```
and
```rust
    pub show_limit_spent_remaining_summary: bool,
```
(confirm exact line numbers with `grep -n "show_zero_based_summary\|show_limit_spent_remaining_summary" backend/src/db.rs` before editing — they are lines 22 and 25 as of this plan's writing).

- [ ] **Step 7: Fix `auth.rs` (`MeResponse` + `From<db::User>` + test)**

In `backend/src/auth.rs`, delete the two fields from `MeResponse` (lines ~73-74) and the two
assignment lines from `impl From<db::User> for MeResponse` (lines ~86-87). Then fix the test
at lines ~549-558 (search `show_zero_based_summary` in `auth.rs`) — remove the two field
initializers from the test's `db::User { ... }` construction and the two `assert!` lines that
reference `me.show_zero_based_summary`/`me.show_limit_spent_remaining_summary`.

- [ ] **Step 8: Delete `account.rs`'s `PreferencesPayload`/`update_preferences` + tests**

Delete the entire `PreferencesPayload` struct and `update_preferences` function
(`backend/src/account.rs:496-533`, run
`grep -n "PreferencesPayload\|fn update_preferences" backend/src/account.rs` to confirm exact
lines before deleting). Delete their test module content (`backend/src/account.rs:645-725` —
run `grep -n "fn preferences_payload\|fn update_preferences" backend/src/account.rs` to find
the exact test function names/bounds; delete only those functions, not the whole `#[cfg(test)]
mod`).

- [ ] **Step 9: Remove the route from `main.rs`**

In `backend/src/main.rs`, delete the line:
```rust
.route("/user/preferences", patch(account::update_preferences))
```
(line ~242). If `account::update_preferences` was the only reason `patch` is imported from
axum in that file, leave the import — `patch` is used by other routes too (e.g.
`/budgets/:id` — confirm with `grep -n "patch(" backend/src/main.rs` before touching imports;
almost certainly still used elsewhere, so no import cleanup needed).

- [ ] **Step 10: Build and run the full non-DB test suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean, all non-`--ignored` tests pass. This is the first point the whole
crate must compile again after touching three structs simultaneously — fix any remaining
compile errors from missed references (search once more:
`grep -rn "show_zero_based_summary\|show_limit_spent_remaining_summary" backend/src` should
return NOTHING after this step).

- [ ] **Step 11: Commit**

```bash
cd backend
git add migrations/20260704000000_budget_strategy.sql src/budget.rs src/db.rs src/auth.rs src/account.rs src/main.rs
git commit -m "feat(#300): add budgets.budget_strategy, retire global summary toggles"
```

---

## Task 2: Thread `budget_strategy` through `BudgetPayload`/`BudgetListItem`/REST handlers

**Files:**
- Modify: `backend/src/budget.rs` — `BudgetPayload` struct (~lines 16-50), `BudgetListItem`
  struct (~lines 52-143), `create_budget` (~1357-1470), `update_budget` (~1792-1900), and
  every other `BudgetListItem` construction site (`list_budgets`, `get_budget`,
  `close_budget`, `archive_budget`, `unarchive_budget`, `link_rollup`, `unlink_rollup` —
  find them all with `grep -n "BudgetListItem {" backend/src/budget.rs`)
- Test: `backend/src/budget.rs` (`#[cfg(test)]` for payload round-trip; `#[ignore]` DB tests)

- [ ] **Step 1: Write the failing serde round-trip unit test**

Add near the existing `amount_mode` Option-semantics test in `backend/src/budget.rs`:

```rust
#[test]
fn budget_payload_budget_strategy_option_semantics() {
    let absent: BudgetPayload = serde_json::from_str(
        r#"{"name":"X","time_frame":"monthly"}"#,
    )
    .unwrap();
    assert_eq!(absent.budget_strategy, None);

    let present: BudgetPayload = serde_json::from_str(
        r#"{"name":"X","time_frame":"monthly","budget_strategy":"zero_based"}"#,
    )
    .unwrap();
    assert_eq!(present.budget_strategy, Some("zero_based".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test budget_payload_budget_strategy_option_semantics`
Expected: FAIL — `budget_strategy` field does not exist on `BudgetPayload`.

- [ ] **Step 3: Add `budget_strategy` to `BudgetPayload`**

In `backend/src/budget.rs`'s `BudgetPayload` struct, after the `amount_mode` field, add:

```rust
    /// Per-budget budgeting strategy (#300): 'zero_based' or 'limit_spent_remaining'. Absent
    /// on create -> defaults to 'limit_spent_remaining'; absent on update -> the existing
    /// value is preserved (COALESCE). Chat-driven creation has a stricter rule (see rag.rs)
    /// — this silent REST default only applies to direct API callers.
    #[serde(default)]
    pub budget_strategy: Option<String>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test budget_payload_budget_strategy_option_semantics`
Expected: PASS

- [ ] **Step 5: Add `budget_strategy` to `BudgetListItem`**

In `backend/src/budget.rs`'s `BudgetListItem` struct, after the `amount_mode` field, add:

```rust
    pub budget_strategy: String,
```

- [ ] **Step 6: Thread through `create_budget`**

In `create_budget` (`backend/src/budget.rs:~1357-1470`), immediately after the existing
`amount_mode` validation block (`if let Some(am) = &payload.amount_mode { validate_amount_mode(am)?; } let amount_mode = ...`), add:

```rust
    // Reject an unknown budget_strategy up front (#300); absent defaults to
    // 'limit_spent_remaining' (mirrors amount_mode/budget_type).
    if let Some(bs) = &payload.budget_strategy {
        validate_budget_strategy(bs)?;
    }
    let budget_strategy = payload
        .budget_strategy
        .as_deref()
        .unwrap_or("limit_spent_remaining")
        .to_string();
```

Add `budget_strategy` as a new column + bind in the `INSERT INTO budgets (...)` statement
(append to the column list and `VALUES (...)`, add a `.bind(&budget_strategy)` after the
existing `.bind(amount_mode)` call) — renumber the positional `$n` placeholders accordingly.
Add `budget_strategy: budget.budget_strategy.clone(),` to the `BudgetListItem { ... }`
construction at the end of the function (next to the existing `amount_mode:
budget.amount_mode.clone(),` line).

- [ ] **Step 7: Thread through `update_budget`**

In `update_budget` (`backend/src/budget.rs:~1792-1900`), add the same validate-if-present
guard as Step 6 (adapted: no default resolution needed, `update_budget` COALESCEs). Add
`budget_strategy = COALESCE($n, budget_strategy)` to the `UPDATE budgets SET ...` statement
(mirroring the existing `budget_type = COALESCE($7, budget_type)` clause) and
`.bind(&payload.budget_strategy)` in the same relative position as the existing
`.bind(&payload.budget_type)` call. Add `budget_strategy: budget.budget_strategy.clone(),` to
its `BudgetListItem` construction.

- [ ] **Step 8: Thread through every remaining `BudgetListItem` construction site**

Run `grep -n "BudgetListItem {" backend/src/budget.rs` to enumerate every construction site
not yet touched (`list_budgets`, `get_budget`, `close_budget`, `archive_budget`,
`unarchive_budget`, `link_rollup`, `unlink_rollup`). Each already has a `budget_type_str`
or `budget.budget_type` value available in scope (since every one of them either does
`let budget_type_str: String = r.get("budget_type");` from a raw row, or holds a `Budget`
struct) — add the analogous `budget_strategy` line to each struct literal:
- If the function holds a `Budget` struct (has `.budget_type` accessed as a field): add
  `budget_strategy: budget.budget_strategy.clone(),` (or the equivalently-named local
  variable — match whatever identifier that function uses for its `Budget`).
- If the function does raw-row extraction (`let budget_type_str: String = r.get("budget_type");`):
  add a sibling `let budget_strategy_str: String = r.get("budget_strategy");` immediately
  after it, and `budget_strategy: budget_strategy_str,` in the struct literal. Ensure the raw
  `SELECT` feeding that row includes `budget_strategy` — if the query uses `SELECT *`, no
  change needed; if it lists explicit columns, add `budget_strategy` to the column list.

- [ ] **Step 9: Run the full non-DB suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean (this is the step that will surface any missed `BudgetListItem`
construction site — the compiler will error on a struct literal missing a required field),
all non-`--ignored` tests pass.

- [ ] **Step 10: Write DB-backed tests for REST create/update**

Add near existing `--ignored` REST tests in `backend/src/budget.rs` (follow the file's
established seed-user/seed-budget/assert/cleanup idiom — copy the connection-setup boilerplate
from an existing `#[ignore = "requires Postgres..."]` test in the same file, e.g. one testing
`amount_mode`):

```rust
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn create_budget_persists_and_defaults_budget_strategy() {
    // Follow this file's existing DB-test setup convention (DATABASE_URL env var with the
    // local fallback, PgPool::connect, seed a user via direct INSERT).
    // 1. Create a budget via `create_budget` with no `budget_strategy` in the payload;
    //    assert the returned BudgetListItem.budget_strategy == "limit_spent_remaining".
    // 2. Create a second budget with budget_strategy: Some("zero_based".into());
    //    assert it round-trips as "zero_based".
    // 3. Create a third with an invalid budget_strategy; assert 400.
    // Clean up seeded rows at the end (DELETE, no transactional rollback wrapper).
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn update_budget_preserves_budget_strategy_when_absent_and_updates_when_present() {
    // 1. Create a budget with budget_strategy "zero_based".
    // 2. PUT/update_budget with a payload omitting budget_strategy but changing name;
    //    assert budget_strategy is still "zero_based" (COALESCE preserved it).
    // 3. update_budget again with budget_strategy: Some("limit_spent_remaining".into());
    //    assert it changed.
    // Clean up.
}
```

- [ ] **Step 11: Run the DB-backed tests (requires local Postgres)**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored budget_strategy`
Expected: PASS (both new tests). If Postgres isn't reachable in this environment, note that
in the task report as a documented limitation — do not skip writing the tests.

- [ ] **Step 12: Commit**

```bash
cd backend
git add src/budget.rs
git commit -m "feat(#300): thread budget_strategy through BudgetPayload/BudgetListItem/REST"
```

---

## Task 3: Extend `validate_rollup_link` with `budget_type` + `budget_strategy` checks

**Files:**
- Modify: `backend/src/budget.rs` — `validate_rollup_link` (~lines 826-852) and its REST call
  site `link_rollup` (~lines 2540-2603)
- Modify: `backend/src/rag.rs` — `chat_rollup_budget`'s call site (~lines 4110-4296)
- Test: `backend/src/budget.rs` (unit tests near the existing `validate_rollup_link` tests,
  `~5302-5337`); `--ignored` DB tests for both call sites

- [ ] **Step 1: Write the failing unit tests for the extended signature**

Find the existing `validate_rollup_link` tests (search
`grep -n "fn.*validate_rollup_link\|validate_rollup_link(" backend/src/budget.rs`) and add
new cases exercising the extended signature (this will not compile until Step 3 changes the
signature — that's expected, TDD-red):

```rust
#[test]
fn validate_rollup_link_rejects_type_mismatch() {
    let err = validate_rollup_link(
        Uuid::new_v4(), Uuid::new_v4(), false, false,
        "time_based", "project", "zero_based", "zero_based",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
}

#[test]
fn validate_rollup_link_rejects_strategy_mismatch() {
    let err = validate_rollup_link(
        Uuid::new_v4(), Uuid::new_v4(), false, false,
        "time_based", "time_based", "zero_based", "limit_spent_remaining",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
}

#[test]
fn validate_rollup_link_allows_matching_type_and_strategy() {
    assert!(validate_rollup_link(
        Uuid::new_v4(), Uuid::new_v4(), false, false,
        "project", "project", "limit_spent_remaining", "limit_spent_remaining",
    )
    .is_ok());
}

#[test]
fn validate_rollup_link_self_link_still_wins_over_type_mismatch() {
    let id = Uuid::new_v4();
    let err = validate_rollup_link(
        id, id, false, false,
        "time_based", "project", "zero_based", "limit_spent_remaining",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST); // self-link check fires first
}

#[test]
fn validate_rollup_link_chain_violation_still_wins_over_strategy_mismatch() {
    let err = validate_rollup_link(
        Uuid::new_v4(), Uuid::new_v4(), true, false,
        "time_based", "time_based", "zero_based", "limit_spent_remaining",
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(err.1.contains("already") || err.1.to_lowercase().contains("child")); // the existing chain message, not the new strategy message — verify exact existing wording with `grep -n "parent_is_child" backend/src/budget.rs` and match this assertion to it
}
```

- [ ] **Step 2: Run tests to verify they fail (compile error expected)**

Run: `cd backend && cargo test validate_rollup_link`
Expected: FAIL to compile — `validate_rollup_link` takes 4 args, tests pass 8.

- [ ] **Step 3: Extend `validate_rollup_link`'s signature and logic**

Read the current full function body first (`sed -n '820,855p' backend/src/budget.rs`) to
preserve its existing checks and exact error messages/order, then extend it to:

```rust
pub fn validate_rollup_link(
    parent_id: Uuid,
    child_id: Uuid,
    parent_is_child: bool,
    child_is_parent: bool,
    parent_type: &str,
    child_type: &str,
    parent_strategy: &str,
    child_strategy: &str,
) -> Result<(), (StatusCode, String)> {
    if parent_id == child_id {
        return Err((StatusCode::BAD_REQUEST, "A budget cannot be rolled up into itself.".to_string()));
    }
    if parent_is_child {
        return Err((StatusCode::CONFLICT, "The parent budget is itself already a child of another rollup — only one level of rollup is supported.".to_string()));
    }
    if child_is_parent {
        return Err((StatusCode::CONFLICT, "The child budget is already a rollup parent — only one level of rollup is supported.".to_string()));
    }
    // #300: budgets can only be rolled up together when they share the same budget_type
    // AND the same budget_strategy. Checked after the self-link/chain-violation guards so
    // those keep priority (same order convention as this function's other checks).
    if parent_type != child_type {
        return Err((
            StatusCode::CONFLICT,
            "Budgets can only be rolled up together when they share the same budget type (both time-based or both project).".to_string(),
        ));
    }
    if parent_strategy != child_strategy {
        return Err((
            StatusCode::CONFLICT,
            "Budgets can only be rolled up together when they share the same budgeting strategy.".to_string(),
        ));
    }
    Ok(())
}
```

(Preserve the EXACT existing error message text for the first three checks — copy them
verbatim from the current implementation rather than the illustrative text above, which may
not match word-for-word. This matters because Step 1's
`validate_rollup_link_self_link_still_wins_over_type_mismatch`/
`..._chain_violation_still_wins_over_strategy_mismatch` tests assert against that wording.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test validate_rollup_link`
Expected: PASS (all 5 new tests, plus every pre-existing `validate_rollup_link` test — fix
their call sites too, since the signature grew; append matching `budget_type`/
`budget_strategy` args to each pre-existing call so they keep testing exactly what they tested
before, e.g. `"time_based", "time_based", "zero_based", "zero_based"` for a same-type/
same-strategy baseline case).

- [ ] **Step 5: Update the REST `link_rollup` call site**

In `backend/src/budget.rs`'s `link_rollup` handler (~2540-2603), find where `parent`/`child`
`Budget` rows are fetched (they already exist as full `Budget` structs — confirm with
`sed -n '2560,2605p' backend/src/budget.rs`). Extend the `validate_rollup_link` call to pass
`&parent.budget_type, &child.budget_type, &parent.budget_strategy, &child.budget_strategy`
(exact variable names may differ — use whatever the existing code calls its fetched rows).

- [ ] **Step 6: Update the chat `chat_rollup_budget` call site**

In `backend/src/rag.rs`'s `chat_rollup_budget` (~4110-4296), the parent/child rows are
fetched via `SELECT * FROM budgets WHERE ...` (~4126-4134, ~4155-4158) — `budget_strategy`
is already present on both fetched rows once Task 1's migration lands, so **no SQL change is
needed here**. Just extend the `validate_rollup_link` call (~line 4200) with
`&parent.budget_strategy, &child.budget_strategy` (plus the existing `.budget_type` fields),
the same way as Step 5 — using whatever the existing code calls its fetched parent/child
`Budget` structs.

- [ ] **Step 7: Run the full non-DB suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean, all pass.

- [ ] **Step 8: Write DB-backed tests for both call sites**

Add `--ignored` tests near the existing rollup DB tests in `budget.rs`/`rag.rs`:

```rust
// budget.rs, near existing link_rollup DB tests:
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn link_rollup_rejects_budget_type_mismatch() {
    // Seed a time_based parent and a project child (same owner, same budget_strategy).
    // Call link_rollup; assert 409 and that NO mirror category was created in the parent
    // (query categories WHERE budget_id = parent.id AND linked_budget_id = child.id -> empty).
    // Clean up.
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn link_rollup_rejects_budget_strategy_mismatch() {
    // Seed two time_based budgets with different budget_strategy values (same owner).
    // Call link_rollup; assert 409 and no mirror category created.
    // Clean up.
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
async fn link_rollup_succeeds_when_type_and_strategy_match() {
    // Regression guard for #52/#298: same type + same strategy still links successfully
    // and creates exactly one mirror category.
    // Clean up.
}
```

```rust
// rag.rs, near existing chat_rollup_budget DB tests:
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_rollup_budget_rejects_type_or_strategy_mismatch_without_audit_row() {
    // Seed a mismatched parent/child pair (either axis differing). Call chat_rollup_budget.
    // Assert mutation_error is Some(...) mentioning the mismatch, and that NO
    // AI_ROLLUP_BUDGET audit_logs row was written for this attempt.
    // Clean up.
}
```

- [ ] **Step 9: Run the DB-backed tests**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored rollup`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
cd backend
git add src/budget.rs src/rag.rs
git commit -m "feat(#300): reject rollup links with mismatched budget_type or budget_strategy"
```

---

## Task 4: Chat `CREATE_BUDGET` — ask for strategy instead of defaulting

**Files:**
- Modify: `backend/src/rag.rs` — `AiActionParams` struct (~lines 270-290), the system-prompt
  JSON-shape doc block (~1330-1360) and the lettered budget-family rules (~1384-1400,
  inserting a new `2n`), the `CREATE_BUDGET` handler arm (~2003-2094), the offline
  `CREATE_BUDGET` branch (~1734-1820), and a new `offline_budget_strategy` helper (near
  `offline_amount_mode`, ~5177)
- Test: `backend/src/rag.rs` (unit tests near `offline_amount_mode_routes_fixed_and_derived`,
  ~6511; `#[ignore]` DB tests near existing `chat_endpoint`/CREATE_BUDGET DB tests)

- [ ] **Step 1: Write the failing unit tests for `offline_budget_strategy`**

Add near `offline_amount_mode_routes_fixed_and_derived` (`backend/src/rag.rs:~6511-6539`):

```rust
#[test]
fn offline_budget_strategy_routes_known_phrases() {
    assert_eq!(offline_budget_strategy("create a zero-based budget for vacation"), Some("zero_based"));
    assert_eq!(offline_budget_strategy("create a zero based budget"), Some("zero_based"));
    assert_eq!(offline_budget_strategy("make a limit budget for groceries"), Some("limit_spent_remaining"));
    assert_eq!(offline_budget_strategy("create a traditional budget"), Some("limit_spent_remaining"));
    assert_eq!(offline_budget_strategy("create a limit spent remaining budget"), Some("limit_spent_remaining"));

    // No explicit phrase -> None (must ask, never silently default)
    assert_eq!(offline_budget_strategy("create a budget called groceries"), None);
    assert_eq!(offline_budget_strategy("create a monthly budget of $500"), None);
    assert_eq!(offline_budget_strategy("show my budget summary"), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test offline_budget_strategy_routes_known_phrases`
Expected: FAIL — `offline_budget_strategy` is not defined.

- [ ] **Step 3: Implement `offline_budget_strategy`**

In `backend/src/rag.rs`, immediately after `offline_amount_mode` (search
`fn offline_amount_mode`), add:

```rust
/// Offline-router phrase matcher for budget_strategy (#300), mirroring
/// `offline_amount_mode`'s shape. Only explicit phrases in the SAME message match — the
/// offline router is single-message/stateless and cannot hold a pending clarification
/// across turns, so an unmatched message falls through to the same "ask, don't create" guard
/// the online-LLM path uses (see the CREATE_BUDGET handler).
fn offline_budget_strategy(msg_lower: &str) -> Option<&'static str> {
    if msg_lower.contains("zero-based budget")
        || msg_lower.contains("zero based budget")
        || msg_lower.contains("zero-based strategy")
    {
        Some("zero_based")
    } else if msg_lower.contains("limit spent remaining budget")
        || msg_lower.contains("limit/spent/remaining budget")
        || msg_lower.contains("limit budget")
        || msg_lower.contains("traditional budget")
        || msg_lower.contains("traditional strategy")
    {
        Some("limit_spent_remaining")
    } else {
        None
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test offline_budget_strategy_routes_known_phrases`
Expected: PASS

- [ ] **Step 5: Wire `offline_budget_strategy` into the offline CREATE_BUDGET branch**

In the offline router's CREATE_BUDGET branch (`backend/src/rag.rs:~1734-1763`), find where
`action_params.budget_type` is conditionally set (search
`action_params.budget_type = Some("project"` in this range) and add, at the same nesting
level (this branch already builds `action_params` for a matched "create budget"/"setup
budget" message):

```rust
        if let Some(strategy) = offline_budget_strategy(&msg_lower) {
            action_params.budget_strategy = Some(strategy.to_string());
        }
```

- [ ] **Step 6: Add `budget_strategy` to `AiActionParams`**

In `backend/src/rag.rs`'s `AiActionParams` struct (~270-290), after the `amount_mode` field:

```rust
    budget_strategy: Option<String>, // "zero_based" | "limit_spent_remaining", for CREATE_BUDGET / UPDATE_BUDGET (#300)
```

Find the `AiActionParams` default/empty construction (search `budget_type: None,` near
`amount_mode: None,`, `~1573-1577`) and add `budget_strategy: None,` alongside them.

- [ ] **Step 7: Run the full non-DB suite (compile check for the new struct field)**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean (any other `AiActionParams { ... }` literal missing the new field will
error — fix each; there should be very few, since most construction goes through
`Default`/partial-update patterns — confirm with
`grep -n "AiActionParams {" backend/src/rag.rs`), all non-`--ignored` tests pass.

- [ ] **Step 8: Add the system-prompt JSON-shape doc line and rule 2n**

In the system-prompt JSON-shape block (`backend/src/rag.rs:~1351-1353`, right after the
existing `"amount_mode": ...` doc line), add:

```rust
             \"budget_strategy\": \"zero_based\" | \"limit_spent_remaining\" (for CREATE_BUDGET / UPDATE_BUDGET; if the user hasn't said which for a NEW budget, do NOT set action to CREATE_BUDGET yet -- ask first, see rule 2n),\n\
```

Immediately after rule `2m` (search `2m\.` in the numbered-rule block, ~line 1398), insert:

```rust
         2n. BUDGETING STRATEGY: Every budget has a strategy -- 'zero_based' (tracks how much of the allocated total is still available to spend) or 'limit_spent_remaining' (tracks spending against a limit, shown as Budgeted/Spent/Remaining). When the user asks to CREATE a new budget and has NOT told you which strategy they want, do NOT set 'action' to 'CREATE_BUDGET' yet: set 'action' to 'NONE' and, in 'response_text', ask them to pick one and briefly describe both options. Once they answer (in this message or a later one), proceed with 'action':'CREATE_BUDGET' and 'budget_strategy' set to their choice. To change an existing budget's strategy, set 'action' to 'UPDATE_BUDGET' and set 'budget_strategy'.\n\
```

(Match the exact `\n\` line-continuation style already used by the surrounding rules — read
a few lines of context first with `sed -n '1394,1400p' backend/src/rag.rs`.)

- [ ] **Step 9: Add the server-side guard in the `CREATE_BUDGET` handler**

In the `CREATE_BUDGET` arm (`backend/src/rag.rs:~2003-2094`), find the existing chain:
```rust
let type_valid = crate::budget::validate_budget_type(&budget_type);
...
let mode_valid = crate::budget::validate_amount_mode(&amount_mode);
...
if let Err((_, msg)) = type_valid {
    mutation_error = Some(format!("I couldn't create that budget: {}", msg));
} else if let Err((_, msg)) = mode_valid {
    mutation_error = Some(format!("I couldn't create that budget: {}", msg));
} else if auto_renew && budget_type != "time_based" {
    mutation_error = Some(...);
} else {
    // ... INSERT ...
}
```
Read the exact current text first (`sed -n '2010,2050p' backend/src/rag.rs`), then insert a
new peer branch BEFORE the `else { // INSERT }` arm, resolving `budget_strategy` and
validating it, with a distinct "ask" branch for the missing case:

```rust
                    // #300: budget_strategy must be present and valid before a budget is
                    // created via chat -- missing is NOT an error, it's a clarifying
                    // question; blocks the insert either way (deterministic regardless of
                    // whether the LLM followed system-prompt rule 2n).
                    let budget_strategy_result: Result<String, Option<String>> =
                        match params.budget_strategy.as_deref() {
                            None => Err(None),
                            Some(s) => match crate::budget::validate_budget_strategy(s) {
                                Ok(()) => Ok(s.to_string()),
                                Err((_, msg)) => Err(Some(msg)),
                            },
                        };

                    if let Err((_, msg)) = type_valid {
                        mutation_error = Some(format!("I couldn't create that budget: {}", msg));
                    } else if let Err((_, msg)) = mode_valid {
                        mutation_error = Some(format!("I couldn't create that budget: {}", msg));
                    } else if auto_renew && budget_type != "time_based" {
                        mutation_error = Some(
                            "I couldn't create that budget: only time-based budgets can auto-renew.".to_string(),
                        );
                    } else if let Err(invalid_msg) = &budget_strategy_result {
                        mutation_error = Some(match invalid_msg {
                            Some(msg) => format!("I couldn't create that budget: {}", msg),
                            None => "Before I create this budget, which budgeting strategy would you like -- 'zero-based' (tracks how much of your allocated total is still available to spend) or 'limit/spent/remaining' (tracks spending against a limit)? Let me know and I'll create it.".to_string(),
                        });
                    } else {
                    let budget_strategy = budget_strategy_result.unwrap();

                    // ... existing next_renewal_at / INSERT block, unchanged except:
```

Then in the existing `INSERT INTO budgets (...)` statement inside that `else` block, add
`budget_strategy` to the column list and `.bind(&budget_strategy)` after the existing
`.bind(&amount_mode)` call (renumber `$n` placeholders).

- [ ] **Step 10: Run the full non-DB suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean, all pass. Fix any existing test that constructs an `AiActionParams`
or hits `CREATE_BUDGET` and now unexpectedly gets a clarifying-question response instead of
a created budget — search `grep -n "\"action\":\"CREATE_BUDGET\"" backend/src/rag.rs` for
existing test fixtures and add `,"budget_strategy":"zero_based"` (or
`"limit_spent_remaining"`) to each one's JSON so pre-existing tests keep testing what they
tested before.

- [ ] **Step 11: Write DB-backed tests for the full clarification-then-create flow**

`CREATE_BUDGET` is an inline match arm inside `chat_endpoint` itself (rag.rs:~2003), not an
extracted standalone function like `chat_rollup_budget`/`apply_amount_mode_update` — so these
tests drive the real `chat_endpoint` end-to-end. The offline router (no `GEMINI_API_KEY` set)
is enough to exercise the shared guard logic (both the online-LLM and offline paths funnel
into the identical downstream match arm, so this covers the guard itself regardless of who
populated `budget_strategy`); the offline gate requires the literal substring `"create
budget"` (`msg_lower.contains("create budget")`, rag.rs:1734) — NOT `"create a budget"` —
so message wording matters. Add near existing chat `CREATE_BUDGET` DB tests (search
`grep -n "async fn.*create_budget\|CREATE_BUDGET" backend/src/rag.rs` for the nearest
`#[ignore]` test to place these beside):

```rust
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_create_budget_offline_without_strategy_phrase_asks_and_creates_nothing() {
    // Seed a user with no GEMINI_API_KEY set (offline path). Drive chat_endpoint with
    // message "create budget Vacation" (contains the offline gate's literal "create budget"
    // substring, no strategy phrase).
    // Assert: the response mentions "strategy" and both options; a follow-up
    // `SELECT COUNT(*) FROM budgets WHERE owner_id = $1` for the seeded user is 0 (nothing
    // created).
    // Clean up seeded user.
}

#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_create_budget_offline_with_strategy_phrase_creates_it() {
    // Same setup. Drive chat_endpoint with message "create budget Vacation, zero based
    // budget" (contains BOTH "create budget" (the offline CREATE_BUDGET gate) AND "zero
    // based budget" (offline_budget_strategy's match phrase) as substrings).
    // Assert exactly one budget now exists for the seeded user with budget_strategy =
    // 'zero_based', and an AI_CREATE_BUDGET audit row was written.
    // Clean up.
}
```

Optionally, for direct online-LLM-path coverage of the same guard (not required for
correctness — the guard code is identical regardless of which path populated
`action_params.budget_strategy` — but nice-to-have parity with existing coverage), mirror the
`GEMINI_API_BASE` mock-server pattern already used by
`chat_endpoint_falls_back_to_communications_link_down_on_gemini_timeout` (rag.rs:~7709-7730):
spin up a local axum mock bound to `GEMINI_API_BASE`, script its response as
`{"thought":"t","action":"CREATE_BUDGET","action_params":{"budget_name":"Vacation","budget_strategy":"zero_based"},"response_text":"..."}`,
and assert the budget was created with that strategy. Skip this if it doesn't fit in the
task's time budget — the two offline tests above already give deterministic coverage of the
guard itself.

- [ ] **Step 12: Run the DB-backed tests**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored chat_create_budget`
Expected: PASS.

- [ ] **Step 13: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "feat(#300): chat CREATE_BUDGET asks for a strategy instead of defaulting"
```

---

## Task 5: Chat `UPDATE_BUDGET` accepts `budget_strategy`

**Files:**
- Modify: `backend/src/rag.rs` — the `UPDATE_BUDGET` handler arm (search
  `"UPDATE_BUDGET" =>` and the existing `amount_mode` update sub-arm inside it, near
  `apply_amount_mode_update`, ~2990-3070)
- Test: `backend/src/rag.rs` (`--ignored` DB test)

- [ ] **Step 1: Read the existing `amount_mode` UPDATE_BUDGET arm to mirror it exactly**

Run: `sed -n '2990,3070p' backend/src/rag.rs` and read `apply_amount_mode_update`
(`~5118-5155`) in full before writing the new code — the new `budget_strategy` arm must
follow the identical validate-then-`UPDATE`-then-audit shape.

- [ ] **Step 2: Write the failing DB-backed test**

Add near existing `UPDATE_BUDGET amount_mode` DB tests:

```rust
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_update_budget_changes_budget_strategy() {
    // Seed a user + budget with budget_strategy = 'limit_spent_remaining'.
    // Drive UPDATE_BUDGET with action_params.budget_strategy = Some("zero_based".into()).
    // Assert the budget's budget_strategy is now 'zero_based' and an AI_UPDATE_BUDGET audit
    // row was written.
    // Clean up.
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored chat_update_budget_changes_budget_strategy`
Expected: FAIL — no code path updates `budget_strategy` yet.

- [ ] **Step 4: Implement the `budget_strategy` UPDATE_BUDGET arm**

Add a new `apply_budget_strategy_update` function mirroring `apply_amount_mode_update`
exactly (validate, `UPDATE budgets SET budget_strategy = $1 WHERE id = $2 RETURNING name`,
log + return outcome), and wire it into the `UPDATE_BUDGET` match arm the same way
`params.amount_mode` is wired (search `if let Some(mode) = &params.amount_mode` at
`~3001-3010` and add a sibling `if let Some(strategy) = &params.budget_strategy { ... }`
block calling the new function).

- [ ] **Step 5: Run test to verify it passes**

Run: `cd backend && cargo test -- --ignored chat_update_budget_changes_budget_strategy`
Expected: PASS

- [ ] **Step 6: Run the full non-DB suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean, all pass.

- [ ] **Step 7: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "feat(#300): chat UPDATE_BUDGET can change an existing budget's strategy"
```

---

## Task 6: Frontend — remove the Settings "Budget summary" panel

**Files:**
- Modify: `frontend/src/lib/Settings.svelte` (delete lines ~205-239 panel + related state)
- Modify: `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json` (remove
  `settings.showZeroBased`, `settings.showLimitSpentRemaining`, and
  `settings.budgetSummaryTitle` if unused after the panel is gone)

- [ ] **Step 1: Read `Settings.svelte` in full to scope the exact deletion**

Run: `sed -n '1,140p' frontend/src/lib/Settings.svelte` then
`sed -n '190,245p' frontend/src/lib/Settings.svelte` to see the panel, its state
(`showZeroBased`/`showLimitSpent`), the `$effect` seeding them, and `savePrefs()` in full.
Determine whether `savePrefs()` (and the `onPrefsUpdated` callback prop) does ANYTHING else
besides these two fields — if it's dual-purpose (touches other settings too), only remove the
two fields' portion; if it exists solely for these two fields, delete the whole function and
its call sites (e.g. a "Save" button `onclick={savePrefs}` that no longer has a reason to
exist once the panel is gone).

- [ ] **Step 2: Delete the panel markup**

Delete the "Budget summary" `<h4>` + two `<input type="checkbox">` toggle blocks (the exact
range found in Step 1, approximately lines 205-239).

- [ ] **Step 3: Delete the now-dead local state and its seeding**

Delete `let showZeroBased = $state(true);` / `let showLimitSpent = $state(true);`, their
`$effect` seeding lines (`showZeroBased = user?.show_zero_based_summary ?? true;` etc. —
these also now reference removed backend fields, so they'd be dead/undefined-reading code
even if left), and (per Step 1's finding) either trim or delete `savePrefs()` and its call
site.

- [ ] **Step 4: Remove the now-unused i18n keys from all 6 locales**

For each of `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json`, remove the
`showZeroBased` and `showLimitSpentRemaining` keys under the `settings` object. Check whether
`budgetSummaryTitle` and `prefsSaveFailed` are referenced anywhere else in `frontend/src`
after Step 2-3's deletions (`grep -rn "budgetSummaryTitle\|prefsSaveFailed" frontend/src`) —
if no references remain, remove those keys too from all 6 locale files; if `prefsSaveFailed`
is reused by some other save flow, leave it.

- [ ] **Step 5: Verify the frontend still builds**

Run: `cd frontend && pnpm run build`
Expected: builds clean, no reference errors to removed state/keys.

- [ ] **Step 6: Commit**

```bash
cd frontend
git add src/lib/Settings.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json src/lib/i18n/locales/pt.json
git commit -m "feat(#300): remove the global Budget summary toggles from Settings"
```

---

## Task 7: Frontend — re-gate `App.svelte`'s status strip off `activeBudget.budget_strategy`

**Files:**
- Modify: `frontend/src/App.svelte` (~lines 482-488, 794-830, 2136-2169)

- [ ] **Step 1: Read the current status-strip derivation and rendering in full**

Run: `sed -n '780,835p' frontend/src/App.svelte` and
`sed -n '2130,2172p' frontend/src/App.svelte` to confirm exact current line numbers/content
before editing (they may have shifted slightly from the spec's approximate references).

- [ ] **Step 2: Replace the `showZb`/`showTrad` derivations**

Replace:
```javascript
  let showZb = $derived(!!activeBudget && (user?.show_zero_based_summary ?? true));
  let showTrad = $derived(
    !!activeBudget &&
      (user?.show_limit_spent_remaining_summary ?? true) &&
      !!activeInsight,
  );
```
with:
```javascript
  // Driven by the ACTIVE budget's own budget_strategy (#300) instead of the removed
  // global user toggles -- exactly one strip renders (or none, transiently, for a
  // limit_spent_remaining budget until /insights resolves -- see loadInsights()).
  let showZb = $derived(
    !!activeBudget && activeBudget.budget_strategy === "zero_based",
  );
  let showTrad = $derived(
    !!activeBudget &&
      activeBudget.budget_strategy === "limit_spent_remaining" &&
      !!activeInsight,
  );
```
Update or remove the stale comment above the old block (the "#208" / "Default-ON semantics
... DB columns default TRUE" comment no longer applies — replace it with the new comment
above).

- [ ] **Step 3: Confirm `activeBudget` carries `budget_strategy`**

Run: `grep -n "let activeBudget" frontend/src/App.svelte` and read how `activeBudget` (a
`$state(null)`, assigned imperatively elsewhere in the file) is populated — it should already
resolve to a `BudgetListItem`-shaped object from the `/budgets` list fetched earlier in the
file — once Task 2's backend change ships, `budget_strategy` is present on every item with no
frontend fetch change needed. No fetch changes required here; if `activeBudget` is
constructed via some subset/pick of fields rather
than passed through whole, add `budget_strategy` to that subset.

- [ ] **Step 4: Confirm the `/auth/me` fetch removal doesn't break anything else**

`user?.show_zero_based_summary`/`user?.show_limit_spent_remaining_summary` should now have
zero remaining references anywhere in `App.svelte` — confirm with
`grep -n "show_zero_based_summary\|show_limit_spent_remaining_summary" frontend/src/App.svelte`
(expect no output).

- [ ] **Step 5: Verify the two render blocks are otherwise untouched**

Read lines ~2136-2169 again post-edit — the zero-based and traditional `<div>` blocks
themselves (labels, `zbAvailable`/`zbAllocated`/`zbLeft`, `activeInsight`-derived figures)
should be byte-identical to before; only the `{#if showZb}`/`{#if showTrad}` gating variables
changed meaning, not the blocks' internals.

- [ ] **Step 6: Verify the frontend builds**

Run: `cd frontend && pnpm run build`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
cd frontend
git add src/App.svelte
git commit -m "feat(#300): status strip reflects the active budget's own strategy"
```

---

## Task 8: Frontend — editable Strategy field on `BudgetDetails.svelte`

**Files:**
- Modify: `frontend/src/lib/budgetDetails.js` (add `BUDGET_STRATEGIES`, extend
  `buildEditPatch`/`buildUpdatePayload`)
- Modify: `frontend/src/lib/BudgetDetails.svelte` (add a "Strategy" editable section)
- Modify: `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json` (add
  `budgetDetails.strategyLabel`/`strategyZeroBased`/`strategyLimitSpentRemaining`)
- Test: `frontend/src/lib/budgetDetails.test.js`

- [ ] **Step 1: Write the failing tests for `budgetDetails.js`**

Read `frontend/src/lib/budgetDetails.test.js` first to match its existing test style for
`budget_type`, then add:

```javascript
// Add BUDGET_STRATEGIES to the existing top-of-file import from "./budgetDetails.js"
// (the file already imports { describe, it, expect } from "vitest" — match that
// convention, do NOT use a bare `test(...)`, which is not imported in this file).

describe("BUDGET_STRATEGIES", () => {
  it("lists both shipped strategies", () => {
    expect(BUDGET_STRATEGIES).toEqual(["zero_based", "limit_spent_remaining"]);
  });
});

describe("buildEditPatch budget_strategy", () => {
  it("returns null when budget_strategy is unchanged", () => {
    const budget = { budget_strategy: "zero_based" };
    expect(buildEditPatch("budget_strategy", "zero_based", budget)).toBeNull();
  });

  it("returns a patch when budget_strategy changes", () => {
    const budget = { budget_strategy: "zero_based" };
    expect(buildEditPatch("budget_strategy", "limit_spent_remaining", budget)).toEqual({
      budget_strategy: "limit_spent_remaining",
    });
  });
});

describe("buildUpdatePayload budget_strategy", () => {
  it("includes patch.budget_strategy (undefined when not patched)", () => {
    const existing = { name: "X", time_frame: "monthly", budget_limit: null, budget_type: "time_based", budget_strategy: "zero_based" };
    const payload = buildUpdatePayload(existing, { budget_strategy: "limit_spent_remaining" });
    expect(payload.budget_strategy).toBe("limit_spent_remaining");
    const unpatched = buildUpdatePayload(existing, {});
    expect(unpatched.budget_strategy).toBeUndefined();
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd frontend && pnpm test budgetDetails` (or the repo's actual vitest invocation — check
`frontend/package.json`'s `scripts.test`)
Expected: FAIL — `BUDGET_STRATEGIES` not exported, `buildEditPatch`/`buildUpdatePayload`
don't handle `budget_strategy`.

- [ ] **Step 3: Implement in `budgetDetails.js`**

Add, next to `TIME_FRAMES`:
```javascript
// The exact BudgetPayload.budget_strategy domain accepted by the backend (#300).
export const BUDGET_STRATEGIES = ["zero_based", "limit_spent_remaining"];
```

In `buildEditPatch`, add a case mirroring `budget_type`:
```javascript
  if (field === "budget_strategy") {
    if (draftValue === budget?.budget_strategy) return null;
    return { budget_strategy: draftValue };
  }
```

In `buildUpdatePayload`'s returned object, add:
```javascript
    budget_strategy: patch.budget_strategy,
```
(same COALESCE-on-server, omit-when-absent pattern as `budget_type`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd frontend && pnpm test budgetDetails`
Expected: PASS

- [ ] **Step 5: Add the editable Strategy section to `BudgetDetails.svelte`**

Import `BUDGET_STRATEGIES` alongside the existing `TIME_FRAMES` import. Add
`"budget_strategy"` to the `editingField` doc comment
(`null | "name" | "time_frame" | "budget_type"` → add `| "budget_strategy"`). Immediately
after the existing "Type" section (search `<!-- Type -->` in the template), add a new
section, structurally identical to "Type" but for strategy:

```svelte
      <!-- Strategy (#300) -->
      <div class="rounded-xl bg-base-100 border border-base-300 p-3">
        <div class="text-xs uppercase tracking-wide text-base-content/50 mb-1">
          {$_("budgetDetails.strategyLabel")}
        </div>
        {#if editingField === "budget_strategy"}
          <select
            bind:value={draftValue}
            onchange={commitEdit}
            onblur={cancelEdit}
            onkeydown={handleEditKeydown}
            disabled={saving}
            class="w-full rounded-lg px-2.5 py-1.5 text-sm bg-base-100 border border-secondary
                   text-base-content focus:outline-none"
          >
            {#each BUDGET_STRATEGIES as bs (bs)}
              <option value={bs}>
                {bs === "zero_based"
                  ? $_("budgetDetails.strategyZeroBased")
                  : $_("budgetDetails.strategyLimitSpentRemaining")}
              </option>
            {/each}
          </select>
        {:else}
          <button
            type="button"
            class="flex items-center gap-1.5 text-sm text-left disabled:cursor-default"
            disabled={readOnly}
            onclick={() => startEdit("budget_strategy", budget.budget_strategy)}
            title={readOnly ? undefined : $_("budgetDetails.editHint")}
          >
            <span>
              {budget.budget_strategy === "zero_based"
                ? $_("budgetDetails.strategyZeroBased")
                : $_("budgetDetails.strategyLimitSpentRemaining")}
            </span>
            {#if !readOnly}<Pencil class="w-3.5 h-3.5 text-base-content/40" />{/if}
          </button>
        {/if}
        {#if editingField === "budget_strategy" && saveError}
          <p class="text-xs text-error mt-1">
            {$_("budgetDetails.saveError", { values: { error: saveError } })}
          </p>
        {/if}
      </div>
```

- [ ] **Step 6: Add the new i18n keys to all 6 locale files**

Add to `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json` under the `budgetDetails`
object (English wording for `en.json`; translate the other 5 to match this file's existing
translation quality/tone for sibling keys like `typeProject`/`typePeriodic`):
```json
"strategyLabel": "Budgeting strategy",
"strategyZeroBased": "Zero-based",
"strategyLimitSpentRemaining": "Limit / spent / remaining"
```

- [ ] **Step 7: Verify the frontend builds and tests pass**

Run: `cd frontend && pnpm run build && pnpm test`
Expected: both succeed.

- [ ] **Step 8: Commit**

```bash
cd frontend
git add src/lib/budgetDetails.js src/lib/budgetDetails.test.js src/lib/BudgetDetails.svelte src/lib/i18n/locales/en.json src/lib/i18n/locales/de.json src/lib/i18n/locales/es.json src/lib/i18n/locales/fr.json src/lib/i18n/locales/it.json src/lib/i18n/locales/pt.json
git commit -m "feat(#300): editable Strategy field on the budget details page"
```

---

## Task 9: Full-suite verification + spec check-off

**Files:**
- Modify: `docs/superpowers/specs/2026-07-04-budget-strategy-design.md` (check off satisfied
  success criteria, following the `#298` precedent of a dedicated "check off satisfied
  success criteria in the spec" commit)

- [ ] **Step 1: Run the full backend non-DB suite**

Run: `cd backend && cargo check && cargo fmt --all -- --check && cargo test`
Expected: clean build, formatted, all pass. If `cargo fmt --all -- --check` fails, run
`cargo fmt --all` and re-verify.

- [ ] **Step 2: Run the full backend DB-backed suite**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored`
Expected: all pass (this is the first full run of every new `#[ignore]` test together,
including Tasks 2-5's tests plus the full pre-existing `--ignored` suite — a regression in an
existing rollup/budget test here means Task 3's `validate_rollup_link` signature change broke
an existing call site that wasn't updated).

- [ ] **Step 3: Run the full frontend suite**

Run: `cd frontend && pnpm run build && pnpm test`
Expected: clean build, all tests pass.

- [ ] **Step 4: Manually walk the AC list against the shipped code**

For each of the six AC bullets in the issue, cite the specific commit/file that satisfies it
(this becomes the PR description's basis). Check off each satisfied criterion in
`docs/superpowers/specs/2026-07-04-budget-strategy-design.md`'s "Success criteria" (add a
`- [x]` per bullet, or leave `- [ ]` with a one-line note if something is deferred — there
should be none deferred at this point).

- [ ] **Step 5: Commit the spec check-off**

```bash
git add docs/superpowers/specs/2026-07-04-budget-strategy-design.md
git commit -m "docs(#300): check off satisfied success criteria in the spec"
```
