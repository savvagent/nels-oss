# Category-Balance Period-Awareness Fix (nels#282) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix chat's single-category balance answers to report current-period spend (matching
LIST_CATEGORIES) instead of lifetime spend, by making `budget::category_table_rows` the one
period-aware source both paths read from, and add a deterministic `CATEGORY_BALANCE` chat action so
single-category questions never depend on LLM arithmetic.

**Architecture:** Two backend-only changes in `backend/src/budget.rs` and `backend/src/rag.rs`. (1)
Extend `CategoryTableRow`/`category_table_rows` with `id`/`rollover_enabled`/`linked_budget_id`, then
replace rag.rs's un-windowed ad-hoc `cats` SQL query with a call to `category_table_rows`, extracting
the render loop into a new **pure, synchronous** helper `render_categories_context` (a refinement of
the spec's originally-proposed async `build_categories_context`: splitting DB fetch — already
period-aware and already tested — from pure rendering makes the rollover/fund non-regression testable
with fast, no-Postgres unit tests instead of only DB-backed ones). (2) Add a new `CATEGORY_BALANCE`
chat action: JSON schema entry, system-prompt rule, a `resolve_category_balance` helper reusing
`category_table_rows`, a match arm, a `category_balance_md` response addendum channel, and a scoped
offline-router keyword matcher for local/offline-mode parity and testability.

**Tech Stack:** Rust (axum, sqlx/PostgreSQL+pgvector). No new dependencies, no migration, no REST
surface change.

---

## File Structure

- **Modify `backend/src/budget.rs`** (~budget.rs:3941-3999): `CategoryTableRow` struct + `category_table_rows` query/mapping. Also ~budget.rs:4626-4705: extend the existing DB-backed test.
- **Modify `backend/src/rag.rs`** (12,800+ lines, the chat/RAG endpoint — the single largest file in the backend; this plan makes a surgical, minimal-diff change, not a restructuring):
  - Delete the un-windowed `cats` query (rag.rs:853-867) and its dependent loop (rag.rs:964-1082), replacing with a call through `category_table_rows` + a new pure `render_categories_context` function.
  - Add `CATEGORY_BALANCE`: JSON action-schema entry (rag.rs:1395), new prompt rule `20c` (after rag.rs:1466), a new `resolve_category_balance` helper, a new match arm (after the `LIST_CATEGORIES` arm, rag.rs:3289-3324), a new `category_balance_md: Option<String>` response-addendum local (declared alongside rag.rs:1979, appended alongside rag.rs:3540-3541).
  - Add an offline-router matcher: refactor `offline_list_transactions_category`'s (rag.rs:5136-5238) preposition-scan into a shared `extract_trailing_category_name` helper, and a new `offline_category_balance_action` built on it, wired into the offline if/else chain immediately after the existing `offline_list_transactions_category` branch (rag.rs:~1667-1671).
- **Create `docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md`** and **`docs/superpowers/plans/2026-07-03-category-balance-period-fix.md`** — the committed spec/plan docs, matching this repo's established convention (see e.g. `docs/superpowers/specs/2026-07-03-fund-categories-design.md`).

No new files for source code — both changes live in existing modules, matching this codebase's
convention of large, cohesive `rag.rs`/`budget.rs` files rather than fine-grained splitting.

---

## Task 1: Extend `CategoryTableRow` with `id`, `rollover_enabled`, `linked_budget_id`

**Files:**
- Modify: `backend/src/budget.rs:3941-3999` (`CategoryTableRow` struct + `category_table_rows` fn)
- Test: `backend/src/budget.rs` inside `mod categories_table_tests` (~budget.rs:4229), extending `category_table_rows_sums_current_period` (~budget.rs:4626-4705)

- [ ] **Step 1: Read the current exact code before editing**

Run: `sed -n '3935,4000p' backend/src/budget.rs` and `sed -n '4600,4710p' backend/src/budget.rs` — confirm the struct/function/test are still at (or near) these lines. Other in-flight PRs touch `rag.rs`, not `budget.rs`, so drift here should be minimal, but always verify against the live file, not this plan's line numbers.

- [ ] **Step 2: Extend the struct and query (write the change directly — no separate failing-test step first, since this is a additive, backward-compatible struct/query extension with no new branchy logic to TDD; the existing test in Step 3 is the regression check)**

Replace the struct and function body with:

```rust
#[derive(Debug, Clone)]
pub struct CategoryTableRow {
    pub id: Uuid,
    pub name: String,
    pub category_type: String,
    pub category_limit: Option<f64>,
    pub spent: f64,
    /// Fund status and materialized balance (#228). `fund_balance` is always
    /// present (defaults to 0) but only meaningful when `is_fund` is true.
    pub is_fund: bool,
    pub fund_balance: f64,
    /// Per-category rollover toggle (#49) — needed by the chat CATEGORIES
    /// context render (nels#282) alongside the period-windowed `spent`.
    pub rollover_enabled: bool,
    /// Set when this category is a rollup mirror (#52) pointing at another
    /// budget's own total, rather than an ordinary category — needed by the
    /// chat CATEGORIES context render (nels#282) to render the "linked rollup
    /// budget" annotation instead of a bare limit.
    pub linked_budget_id: Option<Uuid>,
}

/// Aggregate categories for `budget_id` with current-period spending.
///
/// Uses the budget's `time_frame` to compute the active period window via
/// `current_period_window`, then sums transactions per category within that
/// window (across all category types). Categories with no transactions in the
/// window appear with `spent = 0` (LEFT JOIN). Ordered by name to give a stable
/// table layout, matching `list_categories`. This is the SOLE period-aware
/// source for per-category current-period spend in the backend — both the
/// LIST_CATEGORIES table and the chat CATEGORIES context (nels#282) read from
/// it, so they cannot drift out of sync again.
pub async fn category_table_rows(
    pool: &sqlx::PgPool,
    budget_id: Uuid,
) -> Result<Vec<CategoryTableRow>, sqlx::Error> {
    let bud = sqlx::query("SELECT time_frame FROM budgets WHERE id = $1")
        .bind(budget_id)
        .fetch_one(pool)
        .await?;
    let time_frame: String = bud.get("time_frame");
    let (start, end) = current_period_window(&time_frame, Utc::now());

    let rows = sqlx::query(
        "SELECT c.id, c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance, \
                c.rollover_enabled, c.linked_budget_id, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM categories c \
         LEFT JOIN transactions t \
           ON c.id = t.category_id \
          AND t.transaction_date >= $2 AND t.transaction_date < $3 \
         WHERE c.budget_id = $1 \
         GROUP BY c.id, c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance, \
                  c.rollover_enabled, c.linked_budget_id \
         ORDER BY c.name ASC",
    )
    .bind(budget_id)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| CategoryTableRow {
            id: r.get("id"),
            name: r.get("name"),
            category_type: r.get("category_type"),
            category_limit: r.get("category_limit"),
            spent: r.get("spent"),
            is_fund: r.get("is_fund"),
            fund_balance: r.get("fund_balance"),
            rollover_enabled: r.get("rollover_enabled"),
            linked_budget_id: r.get("linked_budget_id"),
        })
        .collect())
}
```

- [ ] **Step 3: Run `cargo check` to confirm nothing else in the codebase does exhaustive field destructuring on `CategoryTableRow`**

Run: `cd backend && cargo check 2>&1 | tail -60`
Expected: succeeds, OR fails with a specific "missing fields `id`, `rollover_enabled`, `linked_budget_id` in initializer" / non-exhaustive pattern error at a named call site. If it fails, fix the named call site (add `..` to a struct pattern, or the missing field names to a literal construction) and re-run until clean. Per the spec's Assumption 1, `build_categories_table_html`/`push_category_row` (budget.rs:4065-4192) already use field access (`r.name`, `r.spent`, etc.), not destructuring, so this should be clean — but verify, don't assume.

- [ ] **Step 4: Extend the existing DB-backed test to assert the new fields — including the TRUE/`Some` cases, not just the defaults**

Open `backend/src/budget.rs`, find `category_table_rows_sums_current_period` (search `fn category_table_rows_sums_current_period`). Add these assertions immediately after the existing `assert_eq!(groceries.category_limit, Some(200.0));` line, before the cleanup block. The first three cover `Groceries` (the existing seeded row, which never sets `rollover_enabled`/`linked_budget_id` so both stay at their column defaults); the rest seed a SECOND category with `rollover_enabled = TRUE` and a `linked_budget_id` pointing at a second budget, so the test proves `category_table_rows` correctly reads back the TRUE/`Some` cases too, not only the FALSE/`None` defaults (a plan-review finding: asserting only defaults would leave a real gap — a broken `r.get("rollover_enabled")`/`r.get("linked_budget_id")` could still pass if it always produced a false/None default regardless of the underlying column):

```rust
        assert_eq!(groceries.id, groceries_id, "row carries its own category id");
        assert!(!groceries.rollover_enabled, "rollover_enabled defaults to false");
        assert_eq!(groceries.linked_budget_id, None, "an ordinary category has no linked_budget_id");

        // A second budget to link against, and a second category that actually
        // sets rollover_enabled = TRUE and a real linked_budget_id — proving
        // category_table_rows reads back the TRUE/Some cases correctly, not
        // just the FALSE/None defaults asserted above.
        let linked_budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'cat table test - linked source', 'monthly', 500.0)",
        )
        .bind(linked_budget_id).bind(user_id).execute(&pool).await.expect("seed linked budget");
        let mirror_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, rollover_enabled, linked_budget_id) \
             VALUES ($1, $2, 'Mirror', 'expense', TRUE, $3)",
        )
        .bind(mirror_id).bind(budget_id).bind(linked_budget_id).execute(&pool).await.expect("seed mirror category");

        let rows2 = category_table_rows(&pool, budget_id).await.expect("aggregate rows (2nd read)");
        let mirror = rows2.iter().find(|r| r.name == "Mirror").expect("mirror row present");
        assert!(mirror.rollover_enabled, "rollover_enabled TRUE must read back as true");
        assert_eq!(mirror.linked_budget_id, Some(linked_budget_id), "linked_budget_id must read back as Some(...)");

        sqlx::query("DELETE FROM categories WHERE id = $1").bind(mirror_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(linked_budget_id).execute(&pool).await.ok();
```

- [ ] **Step 5: Run the DB-backed test**

Run: `podman-compose up -d` (if not already running), then `cd backend && cargo test category_table_rows_sums_current_period -- --ignored`
Expected: PASS (1 test), including all the new assertions (the FALSE/`None` defaults on `Groceries` AND the TRUE/`Some` cases on the new `Mirror` category).

- [ ] **Step 6: Run the full offline suite to confirm no regression**

Run: `cd backend && cargo test`
Expected: PASS, same pass count as before this task (plus/minus nothing — no new offline tests added in this task).

- [ ] **Step 7: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#282): extend CategoryTableRow with id/rollover_enabled/linked_budget_id

Prerequisite for making category_table_rows the single period-aware
source for both LIST_CATEGORIES and the chat CATEGORIES context."
```

---

## Task 2: Replace rag.rs's un-windowed `cats` query with `category_table_rows` + a pure render helper

**Files:**
- Modify: `backend/src/rag.rs` (the `chat_endpoint` function, roughly lines 841-1253 — re-verify exact lines via Read before editing, since #269 may have landed and shifted things)
- Test: `backend/src/rag.rs`, new `#[cfg(test)] mod` tests near the new `render_categories_context` function, plus a new DB-backed integration test

- [ ] **Step 1: Read the current exact code before editing**

Run these to re-confirm line numbers and exact text before touching anything (this plan's line numbers are a snapshot; #269 may have shifted them — the ticket warns of this explicitly):
```bash
sed -n '840,1000p' backend/src/rag.rs
sed -n '1080,1260p' backend/src/rag.rs
```
Confirm: the un-windowed `cats` query (`SELECT c.id, c.name, ... COALESCE(SUM(t.amount), 0) as spent_sum ... LEFT JOIN transactions t ON c.id = t.category_id` — no `transaction_date` predicate), the `linked_source_ids` computation reading `c.get::<Option<Uuid>, _>("linked_budget_id")`, the `cat_prev_map` computation, the `b_limit`/`cats_context` render loop ending in `cats_context.push_str(&format!("- {} [{}] - Spent/Earned: ${:.2} {}{}\n", ...))`, and the LATER `cats.len() as i64` use inside `budget_maturity_summary(cats.len() as i64, b_limit, transaction_count)` (search `budget_maturity_summary(cats`). If any of these snippets don't match verbatim, STOP and re-read a wider range before proceeding — do not guess.

- [ ] **Step 2: Add the new pure `render_categories_context` function**

Add this new function directly above `chat_endpoint` (or any other convenient top-level spot in `rag.rs` near the other free functions like `category_not_found_message`) — it is a **pure, synchronous** function (no `async`, no I/O) so it can be unit-tested without a database:

```rust
/// Render the chat CATEGORIES context block from already-fetched,
/// current-period-windowed rows (nels#282). Pure — no I/O — so the rendering
/// logic (limit/rollover/fund annotations) is unit-testable without a
/// database; the WINDOWING correctness of `spent` itself is `cats`'
/// responsibility (each row comes from `budget::category_table_rows`, the
/// single period-aware source shared with LIST_CATEGORIES).
///
/// Returns `(rendered_text, expense_limit_sum)` — the second element seeds the
/// caller's authoritative base-amount computation exactly as the inline loop
/// used to (before being overwritten by `computed_budget_total`).
fn render_categories_context(
    cats: &[crate::budget::CategoryTableRow],
    archived_sources: &std::collections::HashSet<Uuid>,
    linked_source_totals: &std::collections::HashMap<Uuid, f64>,
    cat_prev_map: &std::collections::HashMap<Uuid, f64>,
    b_type: &str,
    b_rollover: bool,
) -> (String, f64) {
    let mut b_limit: f64 = 0.0;
    let mut cats_context = String::new();
    for c in cats {
        if c.category_type == "expense" {
            b_limit += c.category_limit.unwrap_or(0.0);
        }

        let limit_str = match c.linked_budget_id {
            Some(linked_id) if archived_sources.contains(&linked_id) => {
                "(Limit: $0.00) (linked rollup budget — source archived, not counted)".to_string()
            }
            Some(linked_id) => {
                let resolved = linked_source_totals.get(&linked_id).copied().unwrap_or(0.0);
                format!("(Limit: ${:.2}) (linked rollup budget)", resolved)
            }
            None => match c.category_limit {
                Some(lim) => format!("(Limit: ${:.2})", lim),
                None => "No limit".to_string(),
            },
        };

        let rollover_str = if c.category_type == "expense" && c.linked_budget_id.is_none() {
            if c.is_fund {
                let cat_base = c.category_limit.unwrap_or(0.0);
                let eff = crate::budget::fund_effective_limit(true, cat_base, c.fund_balance);
                format!(
                    " | Fund: on | Balance: ${:.2} | Effective limit: ${:.2}",
                    c.fund_balance, eff
                )
            } else if b_type == "project" {
                " | Rollover: n/a (project budget)".to_string()
            } else if !b_rollover {
                " | Rollover: off (budget rollover disabled)".to_string()
            } else {
                let cat_base = c.category_limit.unwrap_or(0.0);
                let cat_prev_spent = cat_prev_map.get(&c.id).copied().unwrap_or(0.0);
                let cat_carried = crate::budget::category_carried(
                    b_rollover, c.rollover_enabled, b_type, cat_base, cat_prev_spent,
                );
                format!(
                    " | Rollover: {} | Carried: ${:.2}",
                    if c.rollover_enabled { "on" } else { "off" },
                    cat_carried
                )
            }
        } else {
            String::new()
        };

        cats_context.push_str(&format!(
            "- {} [{}] - Spent/Earned: ${:.2} {}{}\n",
            c.name, c.category_type, c.spent, limit_str, rollover_str
        ));
    }
    (cats_context, b_limit)
}
```

- [ ] **Step 3: Write pure unit tests for `render_categories_context` BEFORE wiring it into `chat_endpoint`**

Add a new test module (or extend an existing `#[cfg(test)] mod tests` block in rag.rs — search for `mod tests` to find the convention) with these tests. They need no database:

```rust
    fn test_row(name: &str, category_type: &str, limit: Option<f64>, spent: f64) -> crate::budget::CategoryTableRow {
        crate::budget::CategoryTableRow {
            id: Uuid::new_v4(),
            name: name.to_string(),
            category_type: category_type.to_string(),
            category_limit: limit,
            spent,
            is_fund: false,
            fund_balance: 0.0,
            rollover_enabled: false,
            linked_budget_id: None,
        }
    }

    #[test]
    fn render_categories_context_shows_period_windowed_spend_not_lifetime() {
        // The whole point of nels#282: the rendered "Spent/Earned" figure must
        // be exactly whatever `spent` the (already period-windowed) row
        // carries — this function must never re-sum or otherwise touch it.
        let rows = vec![test_row("Entertainment", "expense", Some(250.0), 66.0)];
        let (text, limit) = render_categories_context(
            &rows,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            "time_based",
            false,
        );
        assert!(
            text.contains("Entertainment [expense] - Spent/Earned: $66.00"),
            "expected period-windowed $66.00, got: {text}"
        );
        assert!(!text.contains("1147.82"), "must never show a lifetime figure");
        assert_eq!(limit, 250.0);
    }

    #[test]
    fn render_categories_context_rollover_line_unchanged_by_windowing_fix() {
        // AC bullet 3 (nels#282): rollover annotations must be byte-for-byte
        // identical to what the pre-fix inline loop rendered for the same
        // inputs — the windowing fix touches ONLY the Spent/Earned figure.
        let mut row = test_row("Groceries", "expense", Some(200.0), 50.0);
        row.rollover_enabled = true;
        let mut cat_prev_map = std::collections::HashMap::new();
        cat_prev_map.insert(row.id, 30.0); // previous-period spend, for the carry calc
        let (text, _limit) = render_categories_context(
            &[row],
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &cat_prev_map,
            "time_based",
            true, // b_rollover on
        );
        // carried = max(0, base - prev_spent) = max(0, 200 - 30) = 170
        assert!(
            text.contains("Rollover: on | Carried: $170.00"),
            "expected unchanged rollover annotation, got: {text}"
        );
    }

    #[test]
    fn render_categories_context_fund_line_unchanged_by_windowing_fix() {
        // AC bullet 3 (nels#282): fund annotations must be unchanged too.
        let mut row = test_row("Entertainment", "expense", Some(250.0), 66.0);
        row.is_fund = true;
        row.fund_balance = 40.0;
        let (text, _limit) = render_categories_context(
            &[row],
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            "time_based",
            false,
        );
        assert!(
            text.contains("Fund: on | Balance: $40.00 | Effective limit: $290.00"),
            "expected unchanged fund annotation ($250 + $40 = $290 effective), got: {text}"
        );
    }

    #[test]
    fn render_categories_context_zero_spend_category_still_appears() {
        let rows = vec![test_row("Unused", "expense", Some(100.0), 0.0)];
        let (text, _limit) = render_categories_context(
            &rows,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            "time_based",
            false,
        );
        assert!(
            text.contains("Unused [expense] - Spent/Earned: $0.00"),
            "a zero-spend-this-period category must still render, got: {text}"
        );
    }
```

- [ ] **Step 4: Run the new tests to verify they pass against the new (not-yet-wired) function**

Run: `cd backend && cargo test render_categories_context`
Expected: 4 tests PASS (the function is complete and correct as written in Step 2; these tests exercise it directly, independent of `chat_endpoint` wiring).

- [ ] **Step 5: Replace the `cats` query + render loop in `chat_endpoint` with the new function**

Replace the un-windowed query (originally rag.rs:853-867):
```rust
        // Fetch categories and aggregate transaction sums
        let cats = sqlx::query(
            "SELECT c.id, c.name, c.category_type, c.category_limit, c.rollover_enabled,
                    c.linked_budget_id, c.is_fund, c.fund_balance,
                    COALESCE(SUM(t.amount), 0) as spent_sum
             FROM categories c
             LEFT JOIN transactions t ON c.id = t.category_id
             WHERE c.budget_id = $1
             GROUP BY c.id, c.name, c.category_type, c.category_limit, c.rollover_enabled,
                      c.linked_budget_id, c.is_fund, c.fund_balance
             ORDER BY c.name ASC"
        )
        .bind(bid)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;
```
with:
```rust
        // Fetch categories with CURRENT-PERIOD spend (nels#282 — this used to be
        // a hand-rolled, un-windowed (lifetime) query; category_table_rows is
        // the SAME period-aware source LIST_CATEGORIES already uses, so this
        // context block and that table can never disagree again.
        let cats: Vec<crate::budget::CategoryTableRow> =
            crate::budget::category_table_rows(&state.db, bid)
                .await
                .map_err(internal_error)?;
```

Update the `linked_source_ids` computation (originally rag.rs:874-877) from:
```rust
        let linked_source_ids: Vec<Uuid> = cats
            .iter()
            .filter_map(|c| c.get::<Option<Uuid>, _>("linked_budget_id"))
            .collect();
```
to:
```rust
        let linked_source_ids: Vec<Uuid> = cats
            .iter()
            .filter_map(|c| c.linked_budget_id)
            .collect();
```

Replace the render loop (originally rag.rs:993-1082) — everything from `let mut b_limit: f64 = 0.0;` through the closing `}` of the `for c in &cats { ... }` loop — with a single call to the new function:
```rust
        let (cats_context, mut b_limit) = render_categories_context(
            &cats,
            &archived_sources,
            &linked_source_totals,
            &cat_prev_map,
            &b_type,
            b_rollover,
        );
```
(Note: `b_limit` must stay `mut` — the code immediately after this block, at the original rag.rs:1102-1112, reassigns it from `computed_budget_total`. Confirm this reassignment still compiles unchanged; it reads/writes a plain `f64`, unaffected by this refactor.)

Leave everything else in `chat_endpoint` (archived_sources computation, linked_source_totals computation, cat_prev_map computation, the later `computed_budget_total` override, `txs`/`shares`/`goals_context`/`rollup_line`/etc.) completely unchanged — this task's diff should touch ONLY the query, the `linked_source_ids` line, and the render loop replacement.

- [ ] **Step 6: `cargo check` to confirm the wiring compiles**

Run: `cd backend && cargo check 2>&1 | tail -80`
Expected: clean compile. If there's an error about `cats.len()` (used later at `budget_maturity_summary(cats.len() as i64, ...)`), confirm `cats` is still `Vec<CategoryTableRow>` in scope at that point (it should be — Step 5 didn't move or consume `cats`, only replaced how it's built and how the loop reads it) — `.len()` works identically on `Vec<CategoryTableRow>` as it did on `Vec<PgRow>`.

- [ ] **Step 7: Add a DB-backed regression test proving the fix end-to-end through the real query + render pipeline**

This satisfies the spec's requirement for a regression test that mirrors `category_table_rows_sums_current_period`'s shape but exercises BOTH the DB query and the render function together (not just each in isolation). Add near the other DB-backed tests in rag.rs's test module:

```rust
    // Regression test for nels#282: the chat CATEGORIES context must report
    // ONLY the in-window spend for a category, mirroring
    // budget::category_table_rows_sums_current_period's seed shape exactly,
    // but exercising the full category_table_rows -> render_categories_context
    // pipeline rag.rs actually runs (not category_table_rows in isolation).
    //
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_categories_context_reports_only_in_window_spend() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let budget_id = Uuid::new_v4();
        let entertainment_id = Uuid::new_v4();
        let unused_id = Uuid::new_v4();
        // Additional categories closing a plan-review gap: prove rollover AND
        // fund annotations survive the REAL category_table_rows -> DB round
        // trip -> render_categories_context pipeline, not just synthetic
        // CategoryTableRow values built by hand in Step 3's pure unit tests.
        let groceries_id = Uuid::new_v4();
        let fund_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("cat-balance-ctx-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'cat balance ctx test', 'monthly', 1000.0)",
        )
        .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        // NOTE: categories.rollover_enabled DEFAULTS TO TRUE at the column
        // level (20260617150000_category_rollover.sql — existing budgets with
        // rollover on keep every category carrying by default). Entertainment
        // and Unused below explicitly set rollover_enabled = FALSE so their
        // rendered lines stay simple/predictable for THIS test's assertions
        // (which only check their Spent/Earned figures) and so only Groceries
        // exercises the rollover-carry annotation below — avoiding any
        // reliance on incidental non-collision between multiple categories'
        // "Rollover: on | Carried: $X" lines.
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, rollover_enabled) \
             VALUES ($1, $2, 'Entertainment', 'expense', 250.0, FALSE)",
        )
        .bind(entertainment_id).bind(budget_id).execute(&pool).await.expect("seed entertainment");
        // Zero-spend-this-period category (must still appear with spent = 0).
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, rollover_enabled) \
             VALUES ($1, $2, 'Unused', 'expense', 100.0, FALSE)",
        )
        .bind(unused_id).bind(budget_id).execute(&pool).await.expect("seed unused category");
        // Rollover-enabled category (no transactions -> prev-period spend is 0
        // in this test's empty cat_prev_map, so carried == its own limit, 200).
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, rollover_enabled) \
             VALUES ($1, $2, 'Groceries', 'expense', 200.0, TRUE)",
        )
        .bind(groceries_id).bind(budget_id).execute(&pool).await.expect("seed rollover category");
        // Fund category with a materialized balance (#228).
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit, is_fund, fund_balance) \
             VALUES ($1, $2, 'FunFund', 'expense', 250.0, TRUE, 40.0)",
        )
        .bind(fund_id).bind(budget_id).execute(&pool).await.expect("seed fund category");

        // In-window transaction.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 66.0, now(), 'in window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("seed in-window txn");
        // Out-of-window transaction — must NOT be counted (this is the exact bug: the
        // old un-windowed query would sum this into the "Spent/Earned" figure).
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 1081.82, now() - interval '60 days', 'out of window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("seed out-of-window txn");

        let cats = crate::budget::category_table_rows(&pool, budget_id).await.expect("category_table_rows");
        let (text, _limit) = render_categories_context(
            &cats,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            "time_based",
            true, // b_rollover on, so Groceries' rollover annotation renders
        );

        assert!(
            text.contains("Entertainment [expense] - Spent/Earned: $66.00"),
            "expected only the in-window $66.00, got: {text}"
        );
        assert!(!text.contains("1147.82"), "lifetime sum ($66 + $1081.82) must never appear");
        assert!(
            text.contains("Unused [expense] - Spent/Earned: $0.00"),
            "zero-spend-this-period category must still appear with spent = 0, got: {text}"
        );
        assert!(
            text.contains("Rollover: on | Carried: $200.00"),
            "rollover annotation must survive the real category_table_rows round trip, got: {text}"
        );
        assert!(
            text.contains("Fund: on | Balance: $40.00 | Effective limit: $290.00"),
            "fund annotation must survive the real category_table_rows round trip, got: {text}"
        );

        // Cleanup (children first for FK safety).
        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

- [ ] **Step 8: Run all new/changed tests**

Run:
```bash
cd backend && cargo test render_categories_context
cd backend && cargo test -- --ignored chat_categories_context_reports_only_in_window_spend
```
Expected: all PASS.

- [ ] **Step 9: Run the full test suites (offline + DB-backed) to confirm zero regressions from this refactor**

Run:
```bash
cd backend && cargo test
cd backend && cargo test -- --ignored
```
Expected: PASS, same or greater count than before this task; no pre-existing test broken by the render-loop extraction (pay special attention to any existing test asserting on `cats_context`/"Spent/Earned" wording for rollover or fund categories — if one exists and now fails, it means the extraction introduced a behavioral difference; re-diff Step 2's function body against the ORIGINAL loop shown in Step 1's `sed` output line-by-line until it matches exactly, then re-run).

- [ ] **Step 10: `cargo clippy` clean**

Run: `cd backend && cargo clippy --all-targets 2>&1 | tail -60`
Expected: no new warnings introduced by this task's diff (pre-existing warnings elsewhere in this large file, if any, are out of scope).

- [ ] **Step 11: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#282): source the chat CATEGORIES context from category_table_rows

Replaces the hand-rolled, un-windowed (lifetime) cats query with the same
period-aware budget::category_table_rows LIST_CATEGORIES already uses, so
the two views of a category's spend can never disagree again. The render
loop is extracted into a pure render_categories_context function so its
rollover/fund annotation logic is unit-testable without a database."
```

---

## Task 3: Add the deterministic `CATEGORY_BALANCE` chat action

**Files:**
- Modify: `backend/src/rag.rs` — JSON action schema (~line 1395), prompt rule block (~line 1466), response-addendum locals (~line 1979), the match block (after the `LIST_CATEGORIES` arm, ~line 3324), the composition block (~line 3541), the offline-router if/else chain (~line 1667), plus new free functions (`resolve_category_balance`, `extract_trailing_category_name`, `offline_category_balance_action`).
- Test: new unit tests (pure) for the offline matcher, new DB-backed tests for `resolve_category_balance` and the full end-to-end arm via `chat_endpoint`.

- [ ] **Step 1: Read the current exact code before editing**

Run:
```bash
grep -n '"action": "NONE"' backend/src/rag.rs
grep -n "20b\. LIST_TRANSACTIONS" backend/src/rag.rs
grep -n '"LIST_CATEGORIES" =>' backend/src/rag.rs
grep -n "let mut transactions_list_md" backend/src/rag.rs
grep -n "Append the category-scoped transaction listing" backend/src/rag.rs
grep -n "fn offline_list_transactions_category" backend/src/rag.rs
grep -n "offline_list_transactions_category(&payload.message)" backend/src/rag.rs
```
Re-confirm each line is still where this plan expects (Task 2's edits shifted a few line numbers upward inside `chat_endpoint`, and #269 may also have landed by now) before making any edit. Read a wide enough window (`sed -n` +/- 20 lines around each grep hit) to see full context, not just the matched line.

- [ ] **Step 2: Add `resolve_category_balance` and `extract_trailing_category_name` (write + unit test together, TDD-style, before wiring into the match arm)**

First, write the failing tests (add to the test module):

```rust
    #[test]
    fn extract_trailing_category_name_handles_in_on_for_and_article_stripping() {
        assert_eq!(extract_trailing_category_name("what remains in Entertainment"), Some("Entertainment".to_string()));
        assert_eq!(extract_trailing_category_name("how much is left in the Food category"), Some("Food".to_string()));
        assert_eq!(extract_trailing_category_name("balance for Utilities"), Some("Utilities".to_string()));
        assert_eq!(extract_trailing_category_name("no preposition here"), None);
    }

    #[test]
    fn offline_category_balance_action_extracts_name_for_core_phrasings() {
        assert_eq!(offline_category_balance_action("what remains in the entertainment category"), Some("entertainment".to_string()));
        assert_eq!(offline_category_balance_action("how much is left in Food"), Some("Food".to_string()));
        assert_eq!(offline_category_balance_action("what's the balance for Utilities"), Some("Utilities".to_string()));
    }

    #[test]
    fn offline_category_balance_action_defers_to_list_transactions_on_spend_phrasing() {
        // "spent"/"transaction" phrasings must keep routing to LIST_TRANSACTIONS's
        // own offline matcher (unchanged) — this matcher must not shadow it. These
        // phrasings ALSO contain a balance-noun ("left"/"remain") so the guard
        // this test names is actually exercised (a plain "how much have I spent"
        // with no balance-noun would already return None from the has_balance_noun
        // check alone, and wouldn't prove the spent/transaction guard does anything).
        assert_eq!(offline_category_balance_action("how much do I have left after what I've spent in Entertainment"), None);
        assert_eq!(offline_category_balance_action("what's left, show me transactions in Food"), None);
    }

    #[test]
    fn offline_category_balance_action_excludes_budget_level_questions() {
        // "how much is left in my budget" must NOT resolve "budget" as a
        // category name — falls through to the existing NONE/math path.
        assert_eq!(offline_category_balance_action("how much is left in my budget"), None);
        assert_eq!(offline_category_balance_action("what's my budget remaining"), None);
    }
```

Run: `cd backend && cargo test extract_trailing_category_name offline_category_balance_action`
Expected: FAIL to compile (`extract_trailing_category_name`/`offline_category_balance_action` not defined yet).

Now implement. First, refactor `offline_list_transactions_category` (find via `grep -n "fn offline_list_transactions_category"`) to extract its preposition-scan (everything from `let b = msg.as_bytes();` through `Some(name.to_string())`) into a standalone helper placed just above it:

```rust
/// Find the trailing category name in a phrase using the LAST " in ", " on ",
/// or " for " in the ORIGINAL bytes (ASCII case-insensitive), strip a leading
/// article ("the "/"my ") and a trailing "category" word. Byte-safe: never
/// slices mid multibyte-codepoint. Shared by `offline_list_transactions_category`
/// (#229) and `offline_category_balance_action` (nels#282) — both need the
/// same "the category name sits after a preposition near the end" extraction,
/// just with different trigger-noun guards in front of it.
fn extract_trailing_category_name(msg: &str) -> Option<String> {
    let b = msg.as_bytes();
    let mut best: Option<usize> = None;
    let mut i = 0usize;
    while i + 4 <= b.len() {
        let is_in = b[i] == b' ' && b[i + 1].eq_ignore_ascii_case(&b'i') && b[i + 2].eq_ignore_ascii_case(&b'n') && b[i + 3] == b' ';
        let is_on = b[i] == b' ' && b[i + 1].eq_ignore_ascii_case(&b'o') && b[i + 2].eq_ignore_ascii_case(&b'n') && b[i + 3] == b' ';
        let is_for = i + 5 <= b.len()
            && b[i] == b' '
            && b[i + 1].eq_ignore_ascii_case(&b'f')
            && b[i + 2].eq_ignore_ascii_case(&b'o')
            && b[i + 3].eq_ignore_ascii_case(&b'r')
            && b[i + 4] == b' ';
        if is_in || is_on || is_for {
            best = Some(i);
        }
        i += 1;
    }
    let prep_idx = best?;
    let after = if b[prep_idx + 2].eq_ignore_ascii_case(&b'n') {
        &msg[prep_idx + 4..]
    } else {
        &msg[prep_idx + 5..]
    };

    let mut name = after.trim().trim_matches(['"', '\'', '.', '?', '!']);
    for article in ["the ", "my "] {
        if let Some(prefix) = name.get(..article.len()) {
            if prefix.eq_ignore_ascii_case(article) {
                name = &name[article.len()..];
            }
        }
    }
    if let Some(stripped) = name.strip_suffix("category").or_else(|| name.strip_suffix("Category")) {
        name = stripped.trim();
    }
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
```

Then reduce `offline_list_transactions_category` to just its trigger-noun guard + a call to the shared helper — replace its body (everything from `let b = msg.as_bytes();` to the end) with:
```rust
    extract_trailing_category_name(msg)
```
(Its preceding guard logic — `has_tx_noun`, `is_edit_transaction_trigger`, `is_add_transaction_trigger`, the early `return None`s — stays completely unchanged; ONLY the extraction tail is now delegated.)

Now add the new matcher, placed near `offline_list_transactions_category`:

```rust
/// Offline-router intent match for CATEGORY_BALANCE (nels#282). Returns the
/// extracted category name when the message expresses a single-category
/// remaining-balance intent ("what remains in X", "how much is left in X",
/// "balance for X"), else None. Routed AFTER offline_list_transactions_category
/// in chat_endpoint's offline if/else chain so "spent"/"transaction" phrasings
/// keep going to LIST_TRANSACTIONS unchanged (this function's own guard below
/// additionally refuses those nouns directly, so ordering is a belt-and-braces
/// safeguard, not the only defense). Excludes "budget" as an extracted name —
/// a narrow, deliberate guard (not general NLP) so "how much is left in my
/// budget" still falls through to the existing NONE/math path, per the
/// ticket's "broader phrasings out of scope" boundary.
pub(crate) fn offline_category_balance_action(msg: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    let has_balance_noun = lower.contains("remain") || lower.contains("left") || lower.contains("balance");
    if !has_balance_noun {
        return None;
    }
    // Defer to LIST_TRANSACTIONS's own offline matcher for spend/transaction
    // phrasings — never shadow it.
    if lower.contains("spent") || lower.contains("spending") || lower.contains("transaction") {
        return None;
    }
    let name = extract_trailing_category_name(msg)?;
    if name.eq_ignore_ascii_case("budget") {
        return None;
    }
    Some(name)
}
```

- [ ] **Step 3: Run the unit tests to confirm they now pass**

Run: `cd backend && cargo test extract_trailing_category_name offline_category_balance_action`
Expected: 4 tests PASS. Also re-run the PRE-EXISTING `offline_list_transactions_category` tests to confirm the refactor didn't change its behavior:
Run: `cd backend && cargo test offline_list_transactions_category`
Expected: all pre-existing tests for this function still PASS unchanged.

- [ ] **Step 4: Add `resolve_category_balance` and its DB-backed tests**

Add near `category_not_found_message` (search `fn category_not_found_message`):

```rust
/// Resolve a single category's current-period balance by name (nels#282),
/// case-insensitively, within `budget_id`. Reuses `category_table_rows` (the
/// SAME period-aware source LIST_CATEGORIES and the chat CATEGORIES context
/// use) rather than a second windowed query — the CATEGORY_BALANCE action can
/// never disagree with either. Categories are UNIQUE per (budget_id, name),
/// so a case-insensitive match is unambiguous.
async fn resolve_category_balance(
    pool: &sqlx::PgPool,
    budget_id: Uuid,
    category_name: &str,
) -> Result<Option<crate::budget::CategoryTableRow>, sqlx::Error> {
    let rows = crate::budget::category_table_rows(pool, budget_id).await?;
    let needle = category_name.trim().to_lowercase();
    Ok(rows.into_iter().find(|r| r.name.to_lowercase() == needle))
}
```

Add DB-backed tests near the other `#[ignore]` tests in rag.rs's test module:

```rust
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn resolve_category_balance_reports_only_in_window_spend() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        let budget_id = Uuid::new_v4();
        let entertainment_id = Uuid::new_v4();
        // Zero-spend-this-period category (plan-review gap fix): proves the
        // zero-spend case reads correctly through resolve_category_balance
        // too, not just through the context-builder path (Task 2 Step 7).
        let unused_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id).bind(format!("resolve-cat-bal-{user_id}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'resolve cat balance test', 'monthly', 1000.0)",
        )
        .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Entertainment', 'expense', 250.0)",
        )
        .bind(entertainment_id).bind(budget_id).execute(&pool).await.expect("seed category");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Unused', 'expense', 100.0)",
        )
        .bind(unused_id).bind(budget_id).execute(&pool).await.expect("seed zero-spend category");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 66.0, now(), 'in window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("in-window txn");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 1081.82, now() - interval '60 days', 'out of window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("out-of-window txn");

        // Case-insensitive match.
        let found = resolve_category_balance(&pool, budget_id, "entertainment").await.expect("resolve");
        let row = found.expect("category found");
        assert_eq!(row.spent, 66.0, "only in-window spend counted, not the lifetime $1147.82 sum");
        assert_eq!(row.category_limit, Some(250.0));

        // Zero-spend-this-period category resolves with spent == 0.0, not
        // dropped or errored.
        let unused = resolve_category_balance(&pool, budget_id, "Unused").await.expect("resolve");
        assert_eq!(unused.expect("unused category found").spent, 0.0);

        // Unknown name -> None, not an error.
        let missing = resolve_category_balance(&pool, budget_id, "Nonexistent").await.expect("resolve");
        assert!(missing.is_none());

        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

Run: `cd backend && cargo test -- --ignored resolve_category_balance_reports_only_in_window_spend`
Expected: PASS.

- [ ] **Step 5: Add the JSON action-schema entry**

Find the line with the action enum-of-strings (`grep -n '"action": "NONE"'`). Append `| "CATEGORY_BALANCE"` immediately after `"LIST_CATEGORIES"` in that string (keeping every other entry unchanged):
```
... | "LIST_CATEGORIES" | "CATEGORY_BALANCE" | "SEARCH_TRANSACTIONS" | ...
```
Find the `"category_name": "string (optional)",` line in the `action_params` schema block and update its comment to mention the new action:
```
             "category_name": "string (optional, also used by CATEGORY_BALANCE, LIST_TRANSACTIONS, DELETE_CATEGORY, etc.)",
```

- [ ] **Step 6: Add prompt rule `20c`**

Find rule `20b` (`grep -n "20b\. LIST_TRANSACTIONS"`). Immediately after that rule's line (and before the `{}`/`{}` format-placeholder lines and rule `22`), insert a new rule:

```
         20c. CATEGORY_BALANCE: If the user asks about a SINGLE category's remaining balance (e.g. 'what remains in Entertainment', 'how much is left in Groceries', 'what's the balance for Utilities'), set 'action' to 'CATEGORY_BALANCE' and populate 'category_name' with the named category. This runs an EXACT, current-period-scoped lookup and appends the category's limit/spent/remaining as a table to your reply automatically — do NOT compute or state the spent/remaining figures yourself in 'response_text' (that is the exact bug this action exists to fix: never do this math off the CATEGORIES context above for a single named category). Give a brief natural transition instead, e.g. \"Here's your Entertainment balance.\" If you cannot tell which category they mean, set 'action' to 'NONE' and ask a clarifying question instead of guessing.
```

- [ ] **Step 7: Declare the new response-addendum local and wire its append site**

Find `let mut transactions_list_md: Option<String> = None;` (`grep -n "let mut transactions_list_md"`). Add immediately after it:
```rust
    // Friendly clarifying/not-found prose for the read-only CATEGORY_BALANCE
    // action (nels#282). Mirrors transactions_list_md: appended as a plain
    // addendum to response_text, never routed through
    // mutation_log/mutation_error (that channel is reserved for genuine DB
    // read failures, per LIST_CATEGORIES's convention — see the match arm).
    let mut category_balance_md: Option<String> = None;
```

Find the `transactions_list_md` append block near the composition code (`grep -n "Append the category-scoped transaction listing"`). Add immediately after that `if let Some(results) = &transactions_list_md { ... }` block:
```rust
    // Append the CATEGORY_BALANCE clarifying/not-found prose (nels#282) as a
    // read-only addendum, mirroring transactions_list_md above.
    if let Some(results) = &category_balance_md {
        final_response_text = format!("{}\n\n{}", final_response_text, results);
    }
```

- [ ] **Step 8: Add the `CATEGORY_BALANCE` match arm**

Find the `"LIST_CATEGORIES" => { ... }` arm (`grep -n '"LIST_CATEGORIES" =>'`). Insert a new arm immediately after its closing brace, before the next arm (`"SEARCH_TRANSACTIONS"` or whichever the live file shows next):

```rust
        "CATEGORY_BALANCE" => {
            // Deterministic single-category balance answer (nels#282): no LLM
            // arithmetic. Mirrors LIST_CATEGORIES's three-outcome handling
            // (found -> render, empty/no-budget -> graceful no-op, DB failure
            // -> mutation_error) plus a fourth outcome LIST_CATEGORIES doesn't
            // need: an unresolvable category name -> friendly clarifying prose
            // via category_balance_md (never mutation_error — that channel is
            // reserved for genuine read failures, matching LIST_TRANSACTIONS's
            // "not found" convention, not its error convention).
            if let Some(bid) = active_budget_id {
                let cat_name = parsed_ai_res.action_params.as_ref()
                    .and_then(|p| p.category_name.clone())
                    .filter(|cn| !cn.trim().is_empty());
                match cat_name {
                    None => {
                        category_balance_md = Some(
                            "Which category would you like the balance for?".to_string(),
                        );
                    }
                    Some(cn) => match resolve_category_balance(&state.db, bid, cn.trim()).await {
                        Ok(Some(row)) => {
                            categories_table_html = Some(
                                crate::budget::build_categories_table_html(std::slice::from_ref(&row)),
                            );
                        }
                        Ok(None) => {
                            category_balance_md = Some(category_not_found_message(&cn));
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                budget_id = %bid,
                                "CATEGORY_BALANCE: failed to resolve category balance"
                            );
                            mutation_error = Some(
                                "I couldn't load that category's balance right now. Please try again."
                                    .to_string(),
                            );
                        }
                    },
                }
            }
        }
```

- [ ] **Step 9: Wire the offline-router matcher into the offline if/else chain**

Find where `offline_list_transactions_category` is called in the offline chain (`grep -n "offline_list_transactions_category(&payload.message)"`). Immediately after that whole `else if let Some(cat_name) = offline_list_transactions_category(...) { ... }` branch (before the `else if let Some(categories_action) = offline_categories_action(...)` branch), insert:

```rust
        } else if let Some(cat_name) = offline_category_balance_action(&payload.message) {
            // Offline fallback for CATEGORY_BALANCE (nels#282): deterministic
            // single-category balance lookup, no LLM math. Routed AFTER
            // offline_list_transactions_category so "spent"/"transaction"
            // phrasings keep going there unchanged.
            action = "CATEGORY_BALANCE".to_string();
            action_params.category_name = Some(cat_name.clone());
            response_text = format!("(Mock AI Offline Mode) Here's your {} balance.", cat_name);

        } else if let Some(categories_action) = offline_categories_action(&msg_lower) {
```

(This replaces the ORIGINAL `} else if let Some(categories_action) = offline_categories_action(&msg_lower) {` line with the two-branch version above — the `offline_categories_action` branch's own body is unchanged, only a new branch is inserted before it.)

- [ ] **Step 10: `cargo check`**

Run: `cd backend && cargo check 2>&1 | tail -80`
Expected: clean compile.

- [ ] **Step 11: Add the required end-to-end DB-backed test through `chat_endpoint`**

This is the ONLY test that exercises the actual match-arm wiring (not just the helpers it calls) — required per the spec, not optional. Add near `chat_share_budget_is_owner_only` (same `EnvGuard` pattern):

```rust
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_category_balance_reports_current_period_not_lifetime() {
        struct EnvGuard(Option<String>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.0 {
                    Some(v) => std::env::set_var("GEMINI_API_KEY", v),
                    None => std::env::remove_var("GEMINI_API_KEY"),
                }
            }
        }
        let _env_guard = EnvGuard(std::env::var("GEMINI_API_KEY").ok());
        std::env::remove_var("GEMINI_API_KEY");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let cipher = std::sync::Arc::new(
            crate::crypto::SecretCipher::new(&[7u8; 32]).expect("build test cipher"),
        );
        let state = AppState { db: pool.clone(), cipher };

        let suffix = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let budget_id = Uuid::new_v4();
        let entertainment_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id).bind(format!("chat-cat-bal-{suffix}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'chat cat balance test', 'monthly', 1000.0, TRUE)",
        )
        .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Entertainment', 'expense', 250.0)",
        )
        .bind(entertainment_id).bind(budget_id).execute(&pool).await.expect("seed category");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 66.0, now(), 'in window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("in-window txn");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 1081.82, now() - interval '60 days', 'out of window')",
        )
        .bind(Uuid::new_v4()).bind(budget_id).bind(entertainment_id).execute(&pool).await.expect("out-of-window txn");

        // --- Found: reports current-period figures, not lifetime ---
        let resp = chat_endpoint(
            State(state.clone()),
            Extension(user_id),
            Json(ChatRequest {
                message: "what remains in the entertainment category".to_string(),
                budget_id: Some(budget_id),
                conversation_id: None,
                locale: None,
            }),
        )
        .await
        .expect("chat call succeeds");
        let html = resp.0.categories_table_html.as_deref().unwrap_or_default();
        assert!(html.contains("Entertainment"), "table should name the category, got: {html}");
        assert!(html.contains("$66"), "spent should be the in-window $66, got: {html}");
        assert!(html.contains("$184"), "remaining should be $250 - $66 = $184, got: {html}");
        assert!(!html.contains("1147"), "must never show the lifetime $1147.82 sum, got: {html}");
        assert!(!resp.0.response.contains("⚠️"), "a successful lookup must not carry a mutation_error warning");

        // --- Unknown category name: friendly clarifying prose, no crash ---
        let resp2 = chat_endpoint(
            State(state.clone()),
            Extension(user_id),
            Json(ChatRequest {
                message: "what remains in the nonexistent category".to_string(),
                budget_id: Some(budget_id),
                conversation_id: None,
                locale: None,
            }),
        )
        .await
        .expect("chat call succeeds");
        assert!(
            resp2.0.response.contains("couldn't find a category named"),
            "unknown category should get clarifying prose, got: {}", resp2.0.response
        );
        assert!(!resp2.0.response.contains("⚠️"), "an unknown name is not a read failure, so no mutation_error");

        sqlx::query("DELETE FROM transactions WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

- [ ] **Step 12: Run this test**

Run: `podman-compose up -d` (if not already running), then `cd backend && cargo test -- --ignored chat_category_balance_reports_current_period_not_lifetime`
Expected: PASS. If the "found" assertions fail because `offline_category_balance_action` didn't match `"what remains in the entertainment category"`, debug by checking: does `extract_trailing_category_name` correctly strip "the ... category" to get "entertainment"? (It should: preposition scan finds the LAST " in ", takes everything after → "the entertainment category" → strips leading "the " → "entertainment category" → strips trailing "category" → "entertainment".) Fix and re-run before proceeding.

- [ ] **Step 13: Run the full test suites**

Run:
```bash
cd backend && cargo test
cd backend && cargo test -- --ignored
```
Expected: all PASS, no regressions.

- [ ] **Step 14: `cargo clippy` clean**

Run: `cd backend && cargo clippy --all-targets 2>&1 | tail -60`
Expected: no new warnings from this task's diff.

- [ ] **Step 15: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#282): add deterministic CATEGORY_BALANCE chat action

Single-category balance questions ('what remains in X') no longer depend
on the LLM doing limit-minus-spent arithmetic off the CATEGORIES context.
Resolves the category case-insensitively via the same period-aware
category_table_rows source and renders through build_categories_table_html
- found -> table, unknown name -> clarifying prose, no active budget ->
graceful no-op, DB failure -> mutation_error. Includes a scoped offline-
router matcher ('remaining/left/balance in X') for offline-mode parity
and end-to-end testability."
```

---

## Task 4: Commit the spec + plan docs and do a final full-suite pass

**Files:**
- Create: `docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md`
- Create: `docs/superpowers/plans/2026-07-03-category-balance-period-fix.md`

- [ ] **Step 1: Write the finalized spec and plan to their committed locations**

Copy the finalized (approved, and subsequently reconciled with the actual `render_categories_context`
pure-function design landed in Tasks 2-3) spec text into
`docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md`, and this plan's finalized
text into `docs/superpowers/plans/2026-07-03-category-balance-period-fix.md` — matching the naming
convention of existing docs (e.g. `docs/superpowers/specs/2026-07-03-fund-categories-design.md` /
`docs/superpowers/plans/2026-07-03-fund-categories.md`). The committed spec's Assumption 9 already
reflects the pure/synchronous `render_categories_context` name and shape (not the originally-sketched
async `build_categories_context`) — no further edits needed at commit time, just copy the text as-is.

- [ ] **Step 2: Run the complete test matrix one more time**

Run:
```bash
cd backend && cargo fmt --check
cd backend && cargo check
cd backend && cargo clippy --all-targets
cd backend && cargo test
podman-compose up -d
cd backend && cargo test -- --ignored
```
Expected: all clean/passing. If `cargo fmt --check` fails, run `cargo fmt` and re-verify no logic changed (formatting only), then re-run the test suites.

- [ ] **Step 3: Commit the docs**

```bash
git add docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md \
        docs/superpowers/plans/2026-07-03-category-balance-period-fix.md
git commit -m "docs(#282): spec + plan for the category-balance period-awareness fix"
```

---

## Spec Coverage Check

- AC "matching LIST_CATEGORIES exactly" → Task 1 (shared source) + Task 2 (context render) + Task 3 (new action), each with dedicated tests.
- AC "one period-aware query, no un-windowed query remains" → Task 2 Step 5 deletes the old `cats` SQL entirely.
- AC "income/savings/fund/rollover unchanged" → Task 2 Step 3's pure unit tests (byte-for-byte, synthetic rows) AND Task 2 Step 7's DB-backed integration test (a real rollover-enabled category + a real fund category, seeded via SQL, asserted through the actual `category_table_rows` → `render_categories_context` round trip) — closing the plan-review gap that pure tests alone don't prove the real DB columns feed the render loop correctly.
- AC "new CATEGORY_BALANCE arm, deterministic, unknown-name friendly, no-budget graceful" → Task 3 Steps 5-9 (arm) + Step 11 (end-to-end test covering both found and unknown-name outcomes) + Step 8's no-op-on-no-active-budget (the `if let Some(bid) = ...` with no `else`).
- AC "regression test: in-window + out-of-window, both paths" → Task 2 Step 7 (context path) + Task 3 Step 4 (`resolve_category_balance`) + Task 3 Step 11 (the arm itself, end-to-end).
- AC "zero-spend category still appears" → Task 1 Step 4 (existing test's implicit COALESCE coverage), Task 2 Step 3's `render_categories_context_zero_spend_category_still_appears` unit test, Task 2 Step 7's "Unused" category in the integration test, AND Task 3 Step 4's "Unused" category asserted through `resolve_category_balance` directly (closing the plan-review gap that only the context-builder path was covered, not the CATEGORY_BALANCE resolution path).
