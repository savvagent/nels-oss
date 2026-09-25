# Fund Categories (#228) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `is_fund` flag to expense categories with a materialized, hourly-advanced,
cumulative, bidirectional `fund_balance`, surfaced through the category table, JSON API, RAG
context, and a new `SET_CATEGORY_FUND` chat action.

**Architecture:** A migration adds `categories.is_fund` / `fund_balance` / `fund_advanced_through`
(the idempotency marker, mirroring `budgets.next_renewal_at` from #51). A pure helper
`fund_effective_limit` computes `limit + fund_balance`. A new background job
`budget::advance_fund_categories`, called from the same hourly ticker as `renew_due_budgets`,
walks each due fund category one period-boundary increment at a time (bounded catch-up loop) and
applies a guarded, idempotent UPDATE. The read path (category table HTML, `CategoryResponse`, RAG
context) and a new chat action (`SET_CATEGORY_FUND`, plus `CREATE_CATEGORY` accepting `is_fund`)
surface it, following the exact structural precedent of #49 (per-category rollover) and #51
(auto-renew).

**Tech Stack:** Rust, axum, sqlx (Postgres), `backend/src/budget.rs` / `backend/src/rag.rs` /
`backend/src/main.rs` / `backend/src/db.rs`; the frontend needs no changes (verified in Task 12 —
`CategoriesView.svelte` `{@html}`-injects the backend-built table verbatim).

**Spec:** `docs/superpowers/specs/2026-07-03-fund-categories-design.md` (posted to
`savvagent/nels#228` as a `[Spec]` comment).

---

### Task 1: Migration + `Category` struct fields

**Files:**
- Create: `backend/migrations/20260703000000_fund_categories.sql`
- Modify: `backend/src/db.rs:62-78` (`Category` struct)
- Test: `backend/src/budget.rs` (compile-level — no new test file; `cargo check` proves the struct
  matches the new columns via `sqlx::FromRow` at compile time for macro-checked queries, and
  `cargo test` for the full suite)

- [ ] **Step 1: Write the migration**

```sql
-- Fund categories (#228): a category can be marked as a fund (envelope /
-- sinking-fund) so its unused amount carries forward CUMULATIVELY and
-- BIDIRECTIONALLY across periods — a running balance across ALL periods since
-- the fund was created, not a single look-back like #47/#49's rollover, and
-- it can go NEGATIVE after sustained overspend (unlike rollover, which clamps
-- at zero). is_fund/rollover_enabled are independent flags; when both are on
-- for the same category, the read path uses the fund balance instead of the
-- #49 one-period carry (see budget::category_response_with_carry) to avoid
-- double-counting the same accumulated credit under two labels.
--
-- fund_balance is MATERIALIZED (not computed-on-read) — a deliberate
-- departure from the "computed on read, no scheduler" principle #47/#49/#51
-- follow (AGENTS.md). The effective amount depends on the base limit AS IT
-- WAS in each past period, and limits change over time; a computed-on-read
-- sum would retroactively rewrite history whenever a limit is edited. See
-- AGENTS.md "Fund Categories (#228)" for the full rationale.
--
-- fund_advanced_through is the idempotency marker (mirrors #51's
-- budgets.next_renewal_at, but inverted: it marks the boundary UP TO WHICH
-- fund_balance already reflects completed periods, not a future due time).
-- NULL means "never a fund" / "not currently tracking". Enabling is_fund sets
-- it to the CURRENT period's start (funds accrue going forward only).
ALTER TABLE categories ADD COLUMN is_fund BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE categories ADD COLUMN fund_balance DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE categories ADD COLUMN fund_advanced_through TIMESTAMPTZ;

-- Funds only make sense for expense categories (leftover = limit - spent);
-- income/savings have no comparable "spent against a limit" semantics in this
-- codebase. Mirrors budgets_auto_renew_only_time_based_check's pattern (#51).
ALTER TABLE categories ADD CONSTRAINT categories_is_fund_expense_only_check
    CHECK (NOT is_fund OR category_type = 'expense');

-- Mirrors budgets_auto_renew_due_idx (#51): a partial index scoped to the
-- subset the hourly job actually scans.
CREATE INDEX categories_fund_due_idx
    ON categories (fund_advanced_through)
    WHERE is_fund;
```

- [ ] **Step 2: Update the `Category` struct**

In `backend/src/db.rs`, the `Category` struct (currently lines 62-78) gains three fields after
`linked_budget_id`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Category {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub category_type: String, // 'income', 'savings', 'expense'
    pub category_limit: Option<f64>,
    /// Whether this category carries its unused remainder forward under rollover
    /// (#49). Master-gated by the budget's own `rollover_enabled`. DEFAULT TRUE.
    pub rollover_enabled: bool,
    /// When set, this category is a live mirror of another budget's total under
    /// issue #52's linked rollup; its amount is computed-on-read from the linked
    /// (source) budget. NULL = an ordinary category. ON DELETE CASCADE removes the
    /// mirror when the source budget is deleted.
    pub linked_budget_id: Option<Uuid>,
    /// Whether this category is a "fund" (envelope / sinking-fund, #228): its
    /// unused amount accumulates CUMULATIVELY and BIDIRECTIONALLY in
    /// `fund_balance` across all periods, unlike #49's single-period,
    /// clamped-at-zero rollover. Expense categories only (data-layer CHECK).
    /// DEFAULT FALSE.
    pub is_fund: bool,
    /// The materialized, running fund balance (#228). Advanced by the hourly
    /// `advance_fund_categories` job: `+= (period_limit - period_spent)` per
    /// completed period. Can go negative after sustained overspend. Frozen
    /// (not reset) when `is_fund` is disabled, so re-enabling is
    /// non-destructive. DEFAULT 0.
    pub fund_balance: f64,
    /// The period boundary up to which `fund_balance` already reflects
    /// completed periods (#228's idempotency marker, mirroring #51's
    /// `next_renewal_at` but inverted in direction). NULL when never a fund.
    pub fund_advanced_through: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
```

- [ ] **Step 3: Verify the migration applies cleanly**

Run: `cd backend && podman-compose -f ../docker-compose.yml up -d && cargo run` (or just
`cargo check` if the dev DB is already running — `sqlx::migrate!` auto-runs on server start per
AGENTS.md §3).
Expected: server starts with no migration error; `psql` (or any client) against
`postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag` shows `categories` has the three
new columns. Stop the server after confirming (`Ctrl+C`).

- [ ] **Step 4: Run the existing suite to confirm no regression**

Run: `cd backend && cargo test`
Expected: PASS (unignored suite; the struct change is additive so no existing test should break).

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/20260703000000_fund_categories.sql backend/src/db.rs
git commit -m "feat(#228): add categories.is_fund/fund_balance/fund_advanced_through"
```

---

### Task 2: Pure helper `fund_effective_limit` + unit tests

**Files:**
- Modify: `backend/src/budget.rs` — add the function immediately after `category_carried`
  (currently ending at line 543, i.e. right after the closing brace of `category_carried`, before
  the `/// Guard a budget against mutation once it has been closed` doc comment at line 545)
- Test: same file, `#[cfg(test)] mod tests` — insert immediately after
  `per_category_equals_budget_when_all_underspent` (currently ending at line 4635) and before
  `prev_window_monthly` (currently starting at line 4638)

- [ ] **Step 1: Write the failing tests**

Insert into the `#[cfg(test)] mod tests` block, right after `per_category_equals_budget_when_all_underspent`'s closing brace:

```rust
    // Fund categories (#228): effective_limit = category_limit + fund_balance,
    // no floor. Non-fund categories are unaffected (returns category_limit
    // unchanged, ignoring whatever fund_balance happens to hold).
    #[test]
    fn fund_effective_limit_non_fund_ignores_balance() {
        assert_eq!(fund_effective_limit(false, 100.0, 9999.0), 100.0);
    }

    #[test]
    fn fund_effective_limit_adds_positive_balance() {
        assert_eq!(fund_effective_limit(true, 100.0, 50.0), 150.0);
    }

    #[test]
    fn fund_effective_limit_allows_negative_result() {
        // Sustained overspend can push the effective limit below zero — no floor.
        assert_eq!(fund_effective_limit(true, 100.0, -180.0), -80.0);
    }

    #[test]
    fn fund_effective_limit_zero_balance_is_plain_limit() {
        assert_eq!(fund_effective_limit(true, 100.0, 0.0), 100.0);
    }

    // Reproduces the ticket's worked example exactly (nels#228): a $100
    // monthly fund category, chained Jan -> Apr. Each period's balance-after
    // and effective-limit-for-the-NEXT-period must match the ticket's table.
    #[test]
    fn fund_worked_example_jan_through_apr() {
        let limit = 100.0;

        // Jan: spent 50. Effective limit for Jan itself is just the base (no
        // balance has accrued yet at the start of the fund's first period).
        let jan_effective = fund_effective_limit(true, limit, 0.0);
        assert_eq!(jan_effective, 100.0, "Jan effective limit");
        let balance_after_jan = 0.0 + (limit - 50.0);
        assert_eq!(balance_after_jan, 50.0, "balance after Jan");

        // Feb: spent 70. Effective limit for Feb reflects Jan's carried balance.
        let feb_effective = fund_effective_limit(true, limit, balance_after_jan);
        assert_eq!(feb_effective, 150.0, "Feb effective limit (100 + 50)");
        let balance_after_feb = balance_after_jan + (limit - 70.0);
        assert_eq!(balance_after_feb, 80.0, "balance after Feb");

        // Mar: spent 200 (overspend). Effective limit for Mar reflects Feb's
        // balance; the overspend then drives the balance negative.
        let mar_effective = fund_effective_limit(true, limit, balance_after_feb);
        assert_eq!(mar_effective, 180.0, "Mar effective limit (100 + 80)");
        let balance_after_mar = balance_after_feb + (limit - 200.0);
        assert_eq!(balance_after_mar, -20.0, "balance after Mar (overspend pushes negative)");

        // Apr: no spend yet. Effective limit for Apr reflects Mar's deficit —
        // a genuine reduction below the base limit.
        let apr_effective = fund_effective_limit(true, limit, balance_after_mar);
        assert_eq!(apr_effective, 80.0, "Apr effective limit (100 - 20)");
    }
```

- [ ] **Step 2: Run tests to verify they fail (function doesn't exist yet)**

Run: `cd backend && cargo test fund_effective_limit fund_worked_example`
Expected: FAIL with `cannot find function fund_effective_limit`.

- [ ] **Step 3: Implement the helper**

Insert into `backend/src/budget.rs`, immediately after `category_carried`'s closing brace (after
line 543):

```rust
/// Effective limit for a fund category (#228): the base limit plus the
/// materialized, CUMULATIVE, BIDIRECTIONAL running balance. Unlike
/// `category_carried`/`carried_amount` (#47/#49), there is NO floor — a
/// sustained overspend can leave a fund category with a genuine deficit that
/// reduces future periods' effective limit until repaid. Non-fund categories
/// are unaffected: this returns `category_limit` unchanged regardless of
/// whatever `fund_balance` happens to hold (a disabled fund's balance is
/// frozen but inert on the read path — see `chat_set_category_fund`).
pub fn fund_effective_limit(is_fund: bool, category_limit: f64, fund_balance: f64) -> f64 {
    if is_fund {
        category_limit + fund_balance
    } else {
        category_limit
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test fund_effective_limit fund_worked_example`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#228): add fund_effective_limit pure helper + worked-example tests"
```

---

### Task 3: Background job `advance_fund_categories` + wire into `main.rs`

**Files:**
- Modify: `backend/src/budget.rs` — add the job function near `renew_due_budgets` (currently
  ending at line 3596, i.e. insert immediately after its closing brace, before the
  `// Categories HTML table in chat (#176)` section comment at line 3598)
- Modify: `backend/src/main.rs:145-149` (hourly ticker — add a sibling call after the existing
  `renew_due_budgets` match arm)

- [ ] **Step 1: Implement the job (no test-first here — this is DB-only logic; Task 4 adds the
  `#[ignore]`-marked DB tests that exercise it end-to-end, following the same order
  `renew_due_budgets` itself used)**

Insert into `backend/src/budget.rs` immediately after `renew_due_budgets`'s closing brace (after
line 3596):

```rust
/// Advance every eligible fund category's `fund_balance` through every
/// COMPLETED period boundary its `fund_advanced_through` marker has not yet
/// crossed (#228). Unlike `renew_due_budgets` (#51), which jumps straight to
/// the next FUTURE boundary in one step, this walks one period increment at a
/// time — funds accumulate PER PERIOD, so downtime spanning several
/// boundaries must apply one `+= (limit - spent)` per crossed period, not a
/// single catch-up jump. Each increment uses the category's CURRENT
/// `category_limit` (there is no historical per-period limit table in this
/// schema — see AGENTS.md "Fund Categories (#228)" for the documented
/// limit-change-fidelity limitation this implies for catch-up after
/// downtime).
///
/// Eligible: `is_fund = TRUE`, has a marker (`fund_advanced_through IS NOT
/// NULL` — always true once enabled), on a `time_based` budget that is
/// neither closed nor archived (mirrors `renew_due_budgets`'s exclusions;
/// project budgets have no periods, so they are excluded by the
/// `budget_type = 'time_based'` filter alone — there is no CHECK forbidding
/// `is_fund` on a project-budget category since project-ness is budget-level,
/// not category-level).
///
/// Idempotent within a period: each per-period UPDATE is guarded by
/// `fund_advanced_through = $old_marker`, so a concurrent/repeated tick can
/// advance a category at most once per boundary (mirrors `renew_due_budgets`'s
/// guarded UPDATE). Bounded to 24 iterations per category per tick (a full
/// year of missed monthly periods, with margin) to guard against a
/// pathological loop.
///
/// Returns the number of categories that advanced through at least one
/// period this tick.
pub async fn advance_fund_categories(db: &PgPool) -> Result<u64, sqlx::Error> {
    let now = Utc::now();

    let due = sqlx::query(
        "SELECT c.id, c.budget_id, c.category_limit, c.fund_advanced_through, \
                b.time_frame, b.owner_id, b.name AS budget_name \
         FROM categories c \
         JOIN budgets b ON b.id = c.budget_id \
         WHERE c.is_fund = TRUE \
           AND c.fund_advanced_through IS NOT NULL \
           AND b.budget_type = 'time_based' \
           AND b.closed_at IS NULL \
           AND b.archived_at IS NULL",
    )
    .fetch_all(db)
    .await?;

    let mut advanced_count: u64 = 0;
    for row in due {
        let category_id: Uuid = row.get("id");
        let budget_id: Uuid = row.get("budget_id");
        let category_limit: f64 = row.get::<Option<f64>, _>("category_limit").unwrap_or(0.0);
        let time_frame: String = row.get("time_frame");
        let owner_id: Uuid = row.get("owner_id");
        let budget_name: String = row.get("budget_name");
        let mut marker: DateTime<Utc> = row.get("fund_advanced_through");

        let mut this_category_advanced = false;
        for _ in 0..24 {
            let boundary = next_period_boundary(&time_frame, marker);
            if boundary > now {
                break;
            }

            let spent: f64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(amount), 0)::float8 FROM transactions \
                 WHERE category_id = $1 AND transaction_date >= $2 AND transaction_date < $3",
            )
            .bind(category_id)
            .bind(marker)
            .bind(boundary)
            .fetch_one(db)
            .await?;

            let delta = category_limit - spent;

            // Guard the UPDATE with the marker equality so a concurrent tick
            // that already advanced this category past `marker` cannot
            // double-apply this increment.
            let res = sqlx::query(
                "UPDATE categories SET fund_balance = fund_balance + $1, fund_advanced_through = $2 \
                 WHERE id = $3 AND fund_advanced_through = $4",
            )
            .bind(delta)
            .bind(boundary)
            .bind(category_id)
            .bind(marker)
            .execute(db)
            .await?;

            if res.rows_affected() == 0 {
                // Another tick already advanced this category past `marker` —
                // stop; do not re-derive state from a marker we no longer own.
                break;
            }

            marker = boundary;
            this_category_advanced = true;
        }

        if this_category_advanced {
            advanced_count += 1;
            log_audit(
                db,
                budget_id,
                owner_id,
                "ADVANCE_FUND_CATEGORY",
                &format!(
                    "Advanced fund balance for a category in budget '{}' through {}",
                    budget_name, marker
                ),
            )
            .await;
        }
    }

    Ok(advanced_count)
}
```

- [ ] **Step 2: Wire into the hourly ticker**

In `backend/src/main.rs`, immediately after the existing `renew_due_budgets` match arm (currently
lines 145-149, ending right before the `github::purge_old_issue_filings` call), add:

```rust
        // Advance fund categories' running balances (#228). Idempotent and
        // safe to run hourly (see budget::advance_fund_categories doc
        // comment); independent of auto_renew — runs for ANY fund category
        // on an eligible time-based, non-closed, non-archived budget.
        match budget::advance_fund_categories(&cleanup_pool).await {
            Ok(n) if n > 0 => tracing::info!(advanced = n, "Advanced fund category balances"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "Failed to advance fund category balances"),
        }
```

- [ ] **Step 3: Compile-check**

Run: `cd backend && cargo check`
Expected: PASS (no test yet for this function's behavior — Task 4 adds those; this step just
confirms it compiles and `main.rs` wiring is syntactically correct).

- [ ] **Step 4: Commit**

```bash
git add backend/src/budget.rs backend/src/main.rs
git commit -m "feat(#228): add advance_fund_categories background job on the hourly ticker"
```

---

### Task 4: DB-backed tests for the background job

**Files:**
- Modify: `backend/src/budget.rs` — add test helpers near `seed_budget`/`marker_of`/`cleanup`
  (currently lines 5228-5291; insert the new helper right after `cleanup`, before the `// ---
  Rollup (#52) DB-backed test helpers ---` comment at line 5293) and add the three tests near
  `renew_due_budgets_excludes_project_closed_archived` (currently ending at line 6539 — insert
  immediately after)

- [ ] **Step 1: Write the test helper**

Insert into `backend/src/budget.rs`'s `#[cfg(test)] mod tests`, right after `cleanup` (after line
5291, before the `// --- Rollup (#52) DB-backed test helpers ---` comment):

```rust
    /// Insert an expense category on `budget_id` with explicit fund state.
    /// Returns its id. Mirrors `seed_budget`'s shape for fund-category tests.
    #[cfg(test)]
    async fn seed_fund_category(
        pool: &PgPool,
        budget_id: Uuid,
        limit: f64,
        fund_balance: f64,
        fund_advanced_through: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories \
             (id, budget_id, name, category_type, category_limit, is_fund, fund_balance, fund_advanced_through) \
             VALUES ($1, $2, $3, 'expense', $4, TRUE, $5, $6)",
        )
        .bind(id)
        .bind(budget_id)
        .bind(format!("fund-cat-{id}"))
        .bind(limit)
        .bind(fund_balance)
        .bind(fund_advanced_through)
        .execute(pool)
        .await
        .expect("seed fund category");
        id
    }

    #[cfg(test)]
    async fn fund_state_of(pool: &PgPool, id: Uuid) -> (f64, DateTime<Utc>) {
        let row = sqlx::query("SELECT fund_balance, fund_advanced_through FROM categories WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch fund state");
        (
            row.get("fund_balance"),
            row.get::<Option<DateTime<Utc>>, _>("fund_advanced_through").expect("marker set"),
        )
    }
```

- [ ] **Step 2: Write the idempotency test**

Insert immediately after `renew_due_budgets_excludes_project_closed_archived`'s closing brace
(after line 6539):

```rust
    // Running the hourly tick twice within a period advances a due fund
    // category AT MOST ONCE: the first run advances the marker past `now`, so
    // the second run finds nothing left to cross. Mirrors
    // renew_due_budgets_is_idempotent_within_a_period (#51's test shape).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_is_idempotent_within_a_period() {
        let (pool, user_id) = renew_test_setup().await;
        let budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;

        // Marker one month in the past -> that period is complete and due.
        let one_month_ago = next_period_boundary("monthly", Utc::now() - chrono::Duration::days(45))
            .checked_sub_signed(chrono::Duration::days(0))
            .unwrap();
        // Simpler, deterministic: derive "one full period ago" from the
        // CURRENT period's start via calendar month arithmetic.
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let marker = Utc.with_ymd_and_hms(py, pm, 1, 0, 0, 0).unwrap();
        let _ = one_month_ago; // silence unused-var if the calendar branch above is what's used

        let cat = seed_fund_category(&pool, budget, 100.0, 0.0, marker).await;

        // Spend 60 in that one completed period -> delta = 100 - 60 = 40.
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, $3, 60.0, $4, 'seed spend')",
        )
        .bind(Uuid::new_v4())
        .bind(budget)
        .bind(cat)
        .bind(marker + chrono::Duration::days(5))
        .execute(&pool)
        .await
        .expect("seed transaction");

        let n1 = advance_fund_categories(&pool).await.expect("first advance");
        assert!(n1 >= 1, "the due category advances on the first tick");
        let (balance_after_first, marker_after_first) = fund_state_of(&pool, cat).await;
        assert_eq!(balance_after_first, 40.0, "balance = limit(100) - spent(60)");
        assert_eq!(marker_after_first, cur_start, "marker advanced to the current period start");

        // Second tick within the same period: nothing left to cross.
        let _ = advance_fund_categories(&pool).await.expect("second advance");
        let (balance_after_second, marker_after_second) = fund_state_of(&pool, cat).await;
        assert_eq!(balance_after_second, balance_after_first, "second tick did not re-advance");
        assert_eq!(marker_after_second, marker_after_first, "marker unchanged on second tick");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'ADVANCE_FUND_CATEGORY'",
        )
        .bind(budget)
        .fetch_one(&pool)
        .await
        .expect("count audits");
        assert_eq!(audit_count, 1, "advanced exactly once");

        cleanup(&pool, user_id).await;
    }

    // A fund category whose marker is 3 whole months stale (simulated
    // downtime) advances through 3 SEPARATE increments — not a single
    // catch-up jump like #51's renewal — landing on the correct cumulative
    // balance. This directly exercises the ticket's Jan->Apr worked example
    // as a catch-up scenario.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_multi_period_catchup() {
        let (pool, user_id) = renew_test_setup().await;
        let budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;

        // Marker 3 months before the current period's start.
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let mut yy = y;
        let mut mm = m as i32 - 3;
        while mm <= 0 {
            mm += 12;
            yy -= 1;
        }
        let marker = Utc.with_ymd_and_hms(yy, mm as u32, 1, 0, 0, 0).unwrap();

        let cat = seed_fund_category(&pool, budget, 100.0, 0.0, marker).await;

        // Three completed periods' worth of spend: 50, 70, 200 (mirroring the
        // ticket's Jan/Feb/Mar rows). Each transaction is dated 5 days into
        // its respective month so it lands inside that period's window.
        for (i, spent) in [50.0_f64, 70.0, 200.0].iter().enumerate() {
            let mut yy2 = yy;
            let mut mm2 = mm + i as i32;
            while mm2 > 12 {
                mm2 -= 12;
                yy2 += 1;
            }
            let tx_date = Utc.with_ymd_and_hms(yy2, mm2 as u32, 5, 0, 0, 0).unwrap();
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
                 VALUES ($1, $2, $3, $4, $5, 'seed spend')",
            )
            .bind(Uuid::new_v4())
            .bind(budget)
            .bind(cat)
            .bind(*spent)
            .bind(tx_date)
            .execute(&pool)
            .await
            .expect("seed transaction");
        }

        let n = advance_fund_categories(&pool).await.expect("catch-up advance");
        assert!(n >= 1, "the stale category advances");

        // Matches the ticket's worked example: balance after Jan(+50) Feb(+30->80) Mar(-100->-20).
        let (balance, marker_after) = fund_state_of(&pool, cat).await;
        let expected = (100.0 - 50.0) + (100.0 - 70.0) + (100.0 - 200.0);
        assert_eq!(balance, expected, "cumulative balance across 3 caught-up periods");
        assert_eq!(balance, -20.0, "matches the ticket's worked-example Mar balance");
        assert_eq!(marker_after, cur_start, "marker caught up to the current period start");

        cleanup(&pool, user_id).await;
    }

    // Project, closed, and archived budgets' fund categories are EXCLUDED
    // from advancement even when their marker is past-due; a plain
    // time-based active budget's fund category IS advanced. Mirrors
    // renew_due_budgets_excludes_project_closed_archived's shape.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn advance_fund_categories_excludes_project_closed_archived() {
        let (pool, user_id) = renew_test_setup().await;
        let cur_start = current_period_window("monthly", Utc::now()).0;
        let (y, m) = (cur_start.year(), cur_start.month());
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let marker = Utc.with_ymd_and_hms(py, pm, 1, 0, 0, 0).unwrap();

        let eligible_budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, false).await;
        let eligible = seed_fund_category(&pool, eligible_budget, 100.0, 0.0, marker).await;

        let archived_budget = seed_budget(&pool, user_id, "monthly", "time_based", false, None, false, true).await;
        let archived = seed_fund_category(&pool, archived_budget, 100.0, 0.0, marker).await;

        let project_budget = seed_budget(&pool, user_id, "monthly", "project", false, None, false, false).await;
        let project = seed_fund_category(&pool, project_budget, 100.0, 0.0, marker).await;

        let closed_budget = seed_budget(&pool, user_id, "monthly", "project", false, None, true, false).await;
        let closed = seed_fund_category(&pool, closed_budget, 100.0, 0.0, marker).await;

        let _ = advance_fund_categories(&pool).await.expect("advance");

        let (eligible_balance, eligible_marker) = fund_state_of(&pool, eligible).await;
        assert_eq!(eligible_balance, 100.0, "eligible category advanced (0 spend -> +100)");
        assert_eq!(eligible_marker, cur_start, "eligible marker advanced");

        let unchanged = |m: DateTime<Utc>| m == marker;
        let (archived_balance, archived_marker) = fund_state_of(&pool, archived).await;
        assert_eq!(archived_balance, 0.0, "archived excluded (balance untouched)");
        assert!(unchanged(archived_marker), "archived excluded (marker not advanced)");

        let (project_balance, project_marker) = fund_state_of(&pool, project).await;
        assert_eq!(project_balance, 0.0, "project excluded (balance untouched)");
        assert!(unchanged(project_marker), "project excluded (marker not advanced)");

        let (closed_balance, closed_marker) = fund_state_of(&pool, closed).await;
        assert_eq!(closed_balance, 0.0, "closed excluded (balance untouched)");
        assert!(unchanged(closed_marker), "closed excluded (marker not advanced)");

        cleanup(&pool, user_id).await;
    }
```

- [ ] **Step 3: Run the DB-backed tests**

Requires the local pgvector DB running: `podman-compose up -d` (from repo root, if not already
running).
Run: `cd backend && cargo test advance_fund_categories -- --ignored`
Expected: PASS (3 tests). If the idempotency test's first attempt (`one_month_ago`) causes an
unused-variable warning treated as an error under `-D warnings` (check `cargo test` output), remove
the dead `one_month_ago` line entirely — it was left in only to show the calendar-arithmetic
approach is deliberate; the `marker` computed via `current_period_window` + manual month rollback
is the one actually used. (Implementer: clean this up before committing — don't leave dead code.)

- [ ] **Step 4: Run the full unignored suite to confirm no regression**

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/src/budget.rs
git commit -m "test(#228): DB-backed idempotency/catch-up/exclusion tests for advance_fund_categories"
```

---

### Task 5: Category table (`CategoryTableRow` / `category_table_rows` / `build_categories_table_html`)

**Files:**
- Modify: `backend/src/budget.rs` — `CategoryTableRow` struct (currently lines 3608-3615),
  `category_table_rows` (currently lines 3624-3661), `push_category_row` (currently lines
  3809-3824), `build_categories_table_html`'s totals accumulation (currently lines 3734-3741 and
  3789-3799)
- Test: same file — a new unit test for `push_category_row`/`build_categories_table_html` fund
  rendering, placed near the existing categories-table tests (search
  `build_categories_table_html` in the test module for the existing cluster; insert alongside it)

- [ ] **Step 1: Write the failing test**

Find the existing test cluster for `build_categories_table_html` (search
`fn build_categories_table_html` or `fn categories_table_html` inside `#[cfg(test)] mod tests` —
there is an existing cluster of tests exercising grouping/money formatting; add this test in that
same cluster):

```rust
    #[test]
    fn categories_table_renders_fund_balance_and_effective_limit() {
        let rows = vec![CategoryTableRow {
            name: "Groceries".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            spent: 200.0,
            is_fund: true,
            fund_balance: 80.0,
        }];
        let html = build_categories_table_html(&rows);
        // Effective limit = 100 + 80 = 180; remaining = 180 - 200 = -20 (a
        // deficit, must render with the money() helper's "-$" sign).
        assert!(html.contains("$180"), "shows the effective limit, not the bare category_limit");
        assert!(html.contains("-$20"), "remaining computed against the effective limit, signed");
        assert!(html.contains("Fund"), "a fund indicator is present in the row markup");
        // Styling must reuse EXISTING daisyUI/Tailwind utility classes already
        // scanned from frontend source (e.g. frontend/src/lib/BudgetsView.svelte
        // uses "badge badge-warning badge-sm" and "text-error") — a novel class
        // name invented only in backend-generated HTML gets NO CSS, since
        // Tailwind's JIT scanner only emits rules for classes it finds in
        // frontend source files. See Step 4 below for the exact classes to use.
        assert!(html.contains("badge-warning"), "fund badge uses an existing, styled daisyUI badge class");
        assert!(html.contains("text-error"), "deficit remaining uses the existing text-error utility class");
    }

    #[test]
    fn categories_table_non_fund_row_unaffected() {
        let rows = vec![CategoryTableRow {
            name: "Rent".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(1000.0),
            spent: 400.0,
            is_fund: false,
            fund_balance: 0.0,
        }];
        let html = build_categories_table_html(&rows);
        assert!(html.contains("$1,000"), "non-fund limit is the bare category_limit");
        assert!(html.contains("$600"), "non-fund remaining is limit - spent");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test categories_table_renders_fund categories_table_non_fund`
Expected: FAIL (compile error — `CategoryTableRow` has no `is_fund`/`fund_balance` fields yet).

- [ ] **Step 3: Update `CategoryTableRow` and `category_table_rows`**

```rust
/// One row of the categories table: a category plus its current-period spend.
#[derive(Debug, Clone)]
pub struct CategoryTableRow {
    pub name: String,
    pub category_type: String,
    pub category_limit: Option<f64>,
    pub spent: f64,
    /// Fund status and materialized balance (#228). `fund_balance` is always
    /// present (defaults to 0) but only meaningful when `is_fund` is true.
    pub is_fund: bool,
    pub fund_balance: f64,
}
```

Update `category_table_rows`'s SQL and mapping (replace the existing `SELECT`/`GROUP BY`/mapping
block):

```rust
    let rows = sqlx::query(
        "SELECT c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance, \
                COALESCE(SUM(t.amount), 0)::float8 AS spent \
         FROM categories c \
         LEFT JOIN transactions t \
           ON c.id = t.category_id \
          AND t.transaction_date >= $2 AND t.transaction_date < $3 \
         WHERE c.budget_id = $1 \
         GROUP BY c.id, c.name, c.category_type, c.category_limit, c.is_fund, c.fund_balance \
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
            name: r.get("name"),
            category_type: r.get("category_type"),
            category_limit: r.get("category_limit"),
            spent: r.get("spent"),
            is_fund: r.get("is_fund"),
            fund_balance: r.get("fund_balance"),
        })
        .collect())
```

- [ ] **Step 4: Update `push_category_row` and the totals accumulation in
  `build_categories_table_html`**

**Styling note (important):** `frontend/src/App.svelte` is where `.cat-table`'s CSS actually
lives (as `:global(...)` rules — its own comment explains scoped Svelte styles don't apply to
`{@html}`-injected content, so `CategoriesView.svelte`, which merely injects the backend HTML,
needs no edit either way). Do NOT invent new bespoke class names (e.g. `cat-fund-badge`,
`cat-deficit`) — they would carry zero CSS, since Tailwind's JIT scanner only emits rules for
class names it finds by scanning frontend *source* files, and a class that appears only inside a
Rust format string is invisible to that scan. Instead, reuse the exact daisyUI/Tailwind utility
class strings already used elsewhere in scanned frontend source — confirmed present in
`frontend/src/lib/BudgetsView.svelte` (`badge badge-warning badge-sm` for a status badge,
`text-error` for a warning-colored text state) — so CSS for them already exists in the compiled
stylesheet and this backend HTML gets it for free with zero frontend edits.

Replace `push_category_row`:

```rust
fn push_category_row(out: &mut String, r: &CategoryTableRow) {
    let effective_limit = r.category_limit.map(|l| fund_effective_limit(r.is_fund, l, r.fund_balance));
    let (limit_cell, remaining_cell) = match effective_limit {
        Some(eff) => (money(eff), money(eff - r.spent)),
        None => ("—".to_string(), "—".to_string()),
    };
    // Reuse EXISTING styled daisyUI/Tailwind classes (see the Styling note
    // above) — badge-warning for the fund indicator, text-error for a
    // negative (deficit) balance/remaining. Never invent a bespoke class name
    // here; it would render completely unstyled.
    let name_cell = if r.is_fund {
        let balance_class = if r.fund_balance < 0.0 { " text-error" } else { "" };
        format!(
            "{} <span class=\"badge badge-warning badge-sm\">Fund</span><br>\
             <small class=\"{}\">Balance: {}</small>",
            html_escape(&r.name),
            balance_class.trim_start(),
            money(r.fund_balance),
        )
    } else {
        html_escape(&r.name)
    };
    let remaining_class = match effective_limit {
        Some(eff) if eff - r.spent < 0.0 => " class=\"text-error\"",
        _ => "",
    };
    out.push_str(&format!(
        "<tr><td data-label=\"Category\">{}</td>\
         <td data-label=\"Limit\">{}</td>\
         <td data-label=\"Spent\">{}</td>\
         <td data-label=\"Remaining\"{}>{}</td></tr>",
        name_cell,
        limit_cell,
        money(r.spent),
        remaining_class,
        remaining_cell
    ));
}
```

**Implementer note:** if `balance_class.trim_start()` yields an empty string when `fund_balance >=
0.0`, the `<small class="">` attribute is harmless (an empty class list) but slightly untidy;
acceptable as-is, or tidy it to omit the `class` attribute entirely when empty — implementer's
choice, not a correctness issue either way.

Replace the totals accumulation loop in `build_categories_table_html` (currently lines 3734-3741)
so it sums the effective limit for fund rows instead of the bare `category_limit`:

```rust
    // Totals accumulate over ALL rows regardless of grouping (expense only).
    // A fund row's contribution is its EFFECTIVE limit (limit + fund_balance)
    // — the true available headroom this period — not the bare category_limit
    // (#228). For a non-fund row these are numerically identical.
    let (mut tot_limit, mut tot_spent) = (0.0_f64, 0.0_f64);
    for r in rows {
        if r.category_type == "expense" {
            let base = r.category_limit.unwrap_or(0.0);
            tot_limit += fund_effective_limit(r.is_fund, base, r.fund_balance);
            tot_spent += r.spent;
        }
    }
```

The totals-row rendering block (currently lines 3789-3799) is unchanged — it already renders
`money(tot_limit - tot_spent)` and `money()` already signs negative values, so a fund-driven
negative total renders correctly with no further change.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd backend && cargo test categories_table_renders_fund categories_table_non_fund`
Expected: PASS.

- [ ] **Step 6: Run the full existing categories-table test cluster + full suite**

Run: `cd backend && cargo test categories_table && cargo test`
Expected: PASS. (If any existing test constructs a `CategoryTableRow` literal directly, it will
fail to compile until `is_fund: false, fund_balance: 0.0` is added to that literal — fix each
call site found by the compiler error, following the exact pattern above.)

- [ ] **Step 7: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#228): surface fund status/balance/effective-limit in the category table"
```

---

### Task 6: REST read path (`CategoryResponse` / `category_response_with_carry`)

**Files:**
- Modify: `backend/src/budget.rs` — `CategoryResponse` struct (currently lines 196-220),
  `category_response_with_carry` (currently lines 2593-2627)
- Test: same file — extend/add a unit test near any existing test that constructs
  `category_response_with_carry` directly (search the test module for
  `category_response_with_carry(`)

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` (anywhere in the pure-function test region, e.g. right after the
fund worked-example tests from Task 2):

```rust
    #[test]
    fn category_response_with_carry_fund_category_reports_fund_balance() {
        let cat = Category {
            id: Uuid::new_v4(),
            budget_id: Uuid::new_v4(),
            name: "Groceries".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(100.0),
            rollover_enabled: true, // #228 precedence: is_fund wins, rollover ignored
            linked_budget_id: None,
            is_fund: true,
            fund_balance: 80.0,
            fund_advanced_through: Some(Utc::now()),
            created_at: Utc::now(),
        };
        // budget_rollover=true, prev_spent=999 would normally drive a large
        // #49 carry via category_carried — but for a fund category the fund
        // balance must be used INSTEAD, not stacked on top.
        let resp = category_response_with_carry(cat, true, "time_based", 999.0, 0.0);
        assert!(resp.is_fund);
        assert_eq!(resp.fund_balance, 80.0);
        assert_eq!(resp.carried_amount, 80.0, "carried_amount reports the fund balance, not the #49 carry");
        assert_eq!(resp.effective_amount, 180.0, "effective = base(100) + fund_balance(80)");
    }

    #[test]
    fn category_response_with_carry_non_fund_category_unaffected() {
        let cat = Category {
            id: Uuid::new_v4(),
            budget_id: Uuid::new_v4(),
            name: "Rent".to_string(),
            category_type: "expense".to_string(),
            category_limit: Some(1000.0),
            rollover_enabled: true,
            linked_budget_id: None,
            is_fund: false,
            fund_balance: 0.0,
            fund_advanced_through: None,
            created_at: Utc::now(),
        };
        let resp = category_response_with_carry(cat, true, "time_based", 200.0, 0.0);
        assert!(!resp.is_fund);
        // Unaffected: still the plain #49 carry, per category_carried's
        // existing (and unit-tested) semantics — max(0, 1000 - 200) = 800.
        assert_eq!(resp.carried_amount, 800.0);
        assert_eq!(resp.effective_amount, 1800.0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test category_response_with_carry_fund category_response_with_carry_non_fund`
Expected: FAIL (compile error — `Category`/`CategoryResponse` missing `is_fund`/`fund_balance`
fields, or `category_response_with_carry` signature mismatch).

- [ ] **Step 3: Update `CategoryResponse` and `category_response_with_carry`**

```rust
#[derive(Serialize)]
pub struct CategoryResponse {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub name: String,
    pub category_type: String,
    pub category_limit: Option<f64>,
    /// Per-category rollover preference (#49).
    pub rollover_enabled: bool,
    pub created_at: DateTime<Utc>,
    /// Read-only computed fields (#49). `base_amount` is `category_limit` (0 if
    /// unset). `carried_amount` is the per-category carry from the previous
    /// period (0 unless an expense category with both rollover switches on) —
    /// OR, for a fund category (#228), the materialized `fund_balance`
    /// instead (is_fund takes precedence over #49's rollover to avoid
    /// double-counting the same accumulated credit under two labels).
    /// `prev_period_spent` is the source spend for the #49 carry (0 for fund
    /// categories, which don't use a single-period look-back).
    /// `effective_amount` is `base_amount + carried_amount` either way.
    pub base_amount: f64,
    pub carried_amount: f64,
    pub effective_amount: f64,
    pub prev_period_spent: f64,
    /// When set, this category is a live mirror of another budget's total under
    /// issue #52's linked rollup; `base_amount`/`effective_amount` reflect the
    /// source budget's own expense total and `carried_amount` is 0. NULL = an
    /// ordinary category. Additive field — existing consumers ignore it.
    pub linked_budget_id: Option<Uuid>,
    /// Whether this category is a fund (#228) and its materialized running
    /// balance. `fund_balance` is always present (0 when not a fund).
    pub is_fund: bool,
    pub fund_balance: f64,
}
```

```rust
fn category_response_with_carry(
    cat: Category,
    budget_rollover: bool,
    budget_type: &str,
    prev_spent: f64,
    linked_base: f64,
) -> CategoryResponse {
    let is_linked = cat.linked_budget_id.is_some();
    let is_expense = cat.category_type == "expense";
    let base = if is_linked {
        linked_base
    } else {
        cat.category_limit.unwrap_or(0.0)
    };
    // Fund precedence (#228): a fund category's carried_amount/effective_amount
    // come from the materialized fund_balance, NOT the #49 one-period carry —
    // stacking both would double-count the same accumulated credit.
    let (prev, carried) = if is_expense && !is_linked && cat.is_fund {
        (0.0, cat.fund_balance)
    } else if is_expense && !is_linked {
        (prev_spent, category_carried(budget_rollover, cat.rollover_enabled, budget_type, base, prev_spent))
    } else {
        (0.0, 0.0)
    };
    CategoryResponse {
        id: cat.id,
        budget_id: cat.budget_id,
        name: cat.name,
        category_type: cat.category_type,
        category_limit: cat.category_limit,
        rollover_enabled: cat.rollover_enabled,
        created_at: cat.created_at,
        base_amount: base,
        carried_amount: carried,
        effective_amount: base + carried,
        prev_period_spent: prev,
        linked_budget_id: cat.linked_budget_id,
        is_fund: cat.is_fund,
        fund_balance: cat.fund_balance,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test category_response_with_carry_fund category_response_with_carry_non_fund`
Expected: PASS.

- [ ] **Step 5: Fix any other call sites that break**

`create_category`, `list_categories`, `update_category` call `category_response_with_carry`
unchanged (same signature) — they should compile as-is. Run the full suite to confirm:

Run: `cd backend && cargo test`
Expected: PASS. Fix any compile errors from other `Category { .. }` struct literals in the test
module the same way as Task 5 Step 6 (add `is_fund: false, fund_balance: 0.0,
fund_advanced_through: None`).

- [ ] **Step 6: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#228): expose is_fund/fund_balance on CategoryResponse; fund precedence over #49 carry"
```

---

### Task 7: RAG per-category context (`rag.rs`)

**Files:**
- Modify: `backend/src/rag.rs` — the `cats` query (currently lines 828-842) and the per-category
  annotation block (currently lines 1013-1044)

- [ ] **Step 1: Write the failing test**

Search `backend/src/rag.rs`'s test module for an existing test asserting on `cats_context` /
`Rollover:` substring content in a rendered system prompt (there should be at least one test
building the RAG context and asserting on rendered text — if none exists that isolates
`cats_context` construction as a pure function, this step instead adds a narrow assertion to
whichever existing integration-style test already exercises `chat_endpoint`'s category-context
rendering, OR — preferred, since this logic is currently inline in `chat_endpoint` and not
extracted — skip a new automated test for this step specifically and instead verify manually in
Step 4 below via a curl smoke test, documenting that decision in the commit message). Given the
per-category annotation logic is inline (not a separately-testable pure function) and extracting
it purely to add a test would be a larger refactor than this task's scope, this step is a manual
verification step, not a new automated unit test — proceed directly to Step 2.

- [ ] **Step 2: Update the `cats` query**

In `backend/src/rag.rs`, update the query (currently lines 828-842):

```rust
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

- [ ] **Step 3: Update the per-category annotation block**

Replace the `rollover_str` computation (currently lines 1013-1038) with a fund-aware version. Fund
status takes precedence over the rollover annotation for the same category (per Assumption 4 —
showing both would double-narrate the same accumulated credit under two labels):

```rust
            let cat_is_fund: bool = c.get("is_fund");
            let cat_fund_balance: f64 = c.get("fund_balance");

            let rollover_str = if cat_type == "expense" && cat_linked.is_none() {
                if cat_is_fund {
                    // Fund categories (#228) supersede the #49 rollover
                    // annotation for the same category — showing both would
                    // double-narrate the same accumulated credit.
                    let cat_base = cat_limit.unwrap_or(0.0);
                    let eff = crate::budget::fund_effective_limit(true, cat_base, cat_fund_balance);
                    format!(
                        " | Fund: on | Balance: ${:.2} | Effective limit: ${:.2}",
                        cat_fund_balance, eff
                    )
                } else if b_type == "project" {
                    // Project budgets never carry (#48), regardless of either
                    // switch — say so rather than printing "Rollover: on".
                    " | Rollover: n/a (project budget)".to_string()
                } else if !b_rollover {
                    " | Rollover: off (budget rollover disabled)".to_string()
                } else {
                    let cat_base = cat_limit.unwrap_or(0.0);
                    let cat_prev_spent = cat_prev_map.get(&cat_id).copied().unwrap_or(0.0);
                    let cat_carried = crate::budget::category_carried(
                        b_rollover, cat_rollover, &b_type, cat_base, cat_prev_spent,
                    );
                    format!(
                        " | Rollover: {} | Carried: ${:.2}",
                        if cat_rollover { "on" } else { "off" },
                        cat_carried
                    )
                }
            } else {
                String::new()
            };
```

- [ ] **Step 4: Manual smoke verification**

Run the backend locally (`cd backend && cargo run`), create a budget with an expense category, set
`is_fund = TRUE` and `fund_balance` via direct SQL (`UPDATE categories SET is_fund = TRUE,
fund_balance = 42, fund_advanced_through = NOW() WHERE id = '<id>';`), then send a chat message
(e.g. via the frontend or `curl -X POST localhost:3000/api/chat ...` with a valid session) that
triggers `chat_endpoint` and inspect the request Nels received (or add a temporary
`tracing::debug!` around `cats_context` and check server logs) to confirm the line reads `... |
Fund: on | Balance: $42.00 | Effective limit: $142.00`. Remove any temporary debug logging before
committing.

- [ ] **Step 5: Run the full suite to confirm no regression**

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#228): surface fund balance/effective limit in the RAG per-category context"
```

---

### Task 8: Chat action `SET_CATEGORY_FUND`

**Files:**
- Modify: `backend/src/rag.rs`:
  - `AiActionParams` struct (currently lines 244-286) — add `is_fund: Option<bool>`
  - Action doc-comment (currently line 238) — append `"SET_CATEGORY_FUND"`
  - JSON action enum in the prompt (currently line 1357) — append `"SET_CATEGORY_FUND"`
  - `action_params` JSON schema in the prompt (currently lines 1358-1391) — add an `is_fund` line
  - `required_perm_for_action` (currently lines 545-554) — add `"SET_CATEGORY_FUND"` to the
    `Permission::Edit` arm and its doc-comment list
  - New `chat_set_category_fund` async fn — placed near `chat_create_categories` (currently ending
    at line 4365; insert immediately after)
  - `match action` dispatch — a new arm near the existing `"SET_CATEGORY_ROLLOVER"` arm (currently
    starting at line 2120; insert the new arm immediately after `"SET_CATEGORY_ROLLOVER"`'s closing
    brace, i.e. right before `"UPDATE_CATEGORY" =>` currently at line 2241)

- [ ] **Step 1: Add the `is_fund` field to `AiActionParams`**

In the struct (currently lines 244-286), add after `category_rollover`:

```rust
    category_rollover: Option<bool>,   // per-category rollover toggle, for SET_CATEGORY_ROLLOVER (#49)
    #[serde(default)]
    is_fund: Option<bool>,             // per-category fund toggle, for SET_CATEGORY_FUND / CREATE_CATEGORY (#228)
```

- [ ] **Step 2: Add `is_fund` to `AiCategorySpec`**

```rust
#[derive(Deserialize, Serialize, Debug, Clone)]
struct AiCategorySpec {
    category_name: String,
    #[serde(default)]
    category_type: Option<String>, // "income" | "savings" | "expense"
    #[serde(default)]
    category_limit: Option<f64>,
    #[serde(default)]
    is_fund: Option<bool>, // for CREATE_CATEGORY (#228)
}
```

- [ ] **Step 3: Update the doc-comment, permission map, and its own doc-comment**

Line 238 (append to the enumeration string):

```rust
    action: String, // "NONE", "CREATE_BUDGET", "UPDATE_BUDGET" (limit/rollover/auto-renew/rename), "CLOSE_BUDGET", "ARCHIVE_BUDGET", "UNARCHIVE_BUDGET", "CREATE_CATEGORY", "UPDATE_CATEGORY" (rename), "ADD_TRANSACTION", "EDIT_TRANSACTION", "SHARE_BUDGET", "SET_CATEGORY_FUND"
```

`required_perm_for_action` (currently lines 545-554) — add `"SET_CATEGORY_FUND"` to the
`Permission::Edit` arm and to the doc-comment's Edit-or-Owner list:

```rust
/// - **Edit-or-Owner** (`CREATE_CATEGORY`, `SEED_CATEGORIES`, `ADD_TRANSACTION`,
///   `EDIT_TRANSACTION`, `UPDATE_BUDGET`, `CREATE_GOAL`, `ADD_GOAL_CONTRIBUTION`,
///   `DELETE_CATEGORY`, `SET_CATEGORY_ROLLOVER`, `UPDATE_CATEGORY`, `SET_CATEGORY_FUND`)
///   mirrors the `require_edit` / Owner-or-Edit guards.
/// ...
fn required_perm_for_action(action: &str) -> Option<Permission> {
    match action {
        "SHARE_BUDGET" | "DELETE_BUDGET" | "CLOSE_BUDGET" | "ARCHIVE_BUDGET"
        | "UNARCHIVE_BUDGET" | "ROLLUP_BUDGET" | "UNROLLUP_BUDGET" => Some(Permission::Owner),
        "CREATE_CATEGORY" | "SEED_CATEGORIES" | "ADD_TRANSACTION" | "EDIT_TRANSACTION" | "UPDATE_BUDGET"
        | "CREATE_GOAL" | "ADD_GOAL_CONTRIBUTION" | "DELETE_CATEGORY"
        | "SET_CATEGORY_ROLLOVER" | "UPDATE_CATEGORY" | "SET_CATEGORY_FUND" => Some(Permission::Edit),
        _ => None,
    }
}
```

- [ ] **Step 4: Update the LLM-facing JSON schema (action enum + action_params)**

Line 1357 (action enum) — insert `"SET_CATEGORY_FUND"` after `"SET_CATEGORY_ROLLOVER"`:

```rust
           "action": "NONE" | "CREATE_BUDGET" | "UPDATE_BUDGET" | "CLOSE_BUDGET" | "ARCHIVE_BUDGET" | "UNARCHIVE_BUDGET" | "ROLLUP_BUDGET" | "UNROLLUP_BUDGET" | "CREATE_CATEGORY" | "SEED_CATEGORIES" | "DELETE_CATEGORY" | "UPDATE_CATEGORY" | "SET_CATEGORY_ROLLOVER" | "SET_CATEGORY_FUND" | "DELETE_BUDGET" | "ADD_TRANSACTION" | "EDIT_TRANSACTION" | "SHARE_BUDGET" | "CREATE_GOAL" | "ADD_GOAL_CONTRIBUTION" | "CREATE_REMINDER" | "LIST_BUDGETS" | "SWITCH_BUDGET" | "SET_USER_NAME" | "EXPORT_DATA" | "OPEN_INSIGHTS" | "OPEN_BUDGETS_LIST" | "LIST_CATEGORIES" | "SEARCH_TRANSACTIONS" | "LIST_TRANSACTIONS" | "DELETE_ACCOUNT" | "REPORT_ISSUE",\n\
```

`action_params` schema (currently lines 1358-1391) — insert after the `category_rollover` line:

```rust
             \"category_rollover\": boolean (optional, per-category rollover toggle for SET_CATEGORY_ROLLOVER; omit category_name to apply to ALL expense categories),\n\
             \"is_fund\": boolean (optional, per-category FUND toggle for SET_CATEGORY_FUND (omit category_name to apply to ALL expense categories) or CREATE_CATEGORY (create as a fund in one step); funds only apply to expense categories),\n\
```

Also add `is_fund` to the `categories` array element schema (same line, currently):
```
"categories": [{{ "category_name": "string", "category_type": ..., "category_limit": number (optional) }}] (optional, ...)
```
becomes:
```rust
             \"categories\": [{{ \"category_name\": \"string\", \"category_type\": \"income\" | \"savings\" | \"expense\" (optional), \"category_limit\": number (optional), \"is_fund\": boolean (optional) }}] (optional, for creating SEVERAL categories at once with CREATE_CATEGORY),\n\
```

- [ ] **Step 5: Write the failing DB test stub for `chat_set_category_fund` (signature only, to
  drive Step 6's implementation) — full test bodies come in Task 11; this step just proves the
  function doesn't exist yet**

Run: `cd backend && cargo check`
Expected: at this point (before Step 6), `cargo check` still passes — Step 5 is a no-op check
since the function doesn't need to exist for the schema/param changes above to compile. Proceed
directly to Step 6.

- [ ] **Step 6: Implement `chat_set_category_fund`**

Insert into `backend/src/rag.rs` immediately after `chat_create_categories`'s closing brace
(currently after line 4365):

```rust
/// Toggle `is_fund` for a named expense category, or for ALL expense
/// categories in the active budget when no name is given (#228). Extracted
/// (unlike SET_CATEGORY_ROLLOVER's inline arm) so the #[ignore] DB test can
/// drive the real mutation directly, per this ticket's testing requirement.
///
/// Enabling a category that is NOT already a fund resets `fund_balance` to 0
/// and `fund_advanced_through` to the CURRENT period's start (funds accrue
/// going forward only). Enabling a category that IS already a fund is a
/// no-op (idempotent — its balance/marker are left untouched). Disabling
/// preserves `fund_balance`/`fund_advanced_through` (non-destructive,
/// mirroring #50 archive's precedent) — the values simply go inert on the
/// read path and stop accruing (the background job's WHERE excludes
/// `is_fund = FALSE` rows).
///
/// A named target that isn't an expense category is a clear error (unlike
/// CREATE_CATEGORY's silent coercion — see `chat_create_categories` — this is
/// an explicit, single-purpose action, so silently no-op'ing would be
/// confusing).
async fn chat_set_category_fund(
    state: &AppState,
    user_id: Uuid,
    active_budget_id: Option<Uuid>,
    params: Option<&AiActionParams>,
) -> (Option<String>, Option<String>) {
    let Some(bid) = active_budget_id else {
        return (
            None,
            Some("I don't have an active budget to change fund status for. Select or create a budget first.".to_string()),
        );
    };

    if let Err(e) = crate::budget::ensure_not_closed(&state.db, bid).await {
        return (None, Some(crate::budget::closed_or_transient_message(e, "change")));
    }

    let flag = params.and_then(|p| p.is_fund).unwrap_or(true);
    let cat_name = params.and_then(|p| p.category_name.clone());

    let time_frame: String = match sqlx::query_scalar("SELECT time_frame FROM budgets WHERE id = $1")
        .bind(bid)
        .fetch_one(&state.db)
        .await
    {
        Ok(tf) => tf,
        Err(e) => {
            tracing::error!(error = %e, "SET_CATEGORY_FUND: failed to load budget time_frame");
            return (None, Some("I couldn't update the fund setting.".to_string()));
        }
    };
    let (period_start, _) = crate::budget::current_period_window(&time_frame, Utc::now());

    let targets: Vec<(Uuid, bool)> = match &cat_name {
        Some(name) => {
            let row = sqlx::query(
                "SELECT id, category_type, is_fund FROM categories \
                 WHERE budget_id = $1 AND LOWER(name) = LOWER($2)",
            )
            .bind(bid)
            .bind(name.trim())
            .fetch_optional(&state.db)
            .await;
            match row {
                Ok(Some(r)) => {
                    let ctype: String = r.get("category_type");
                    if ctype != "expense" {
                        return (
                            None,
                            Some(format!(
                                "Category '{}' is not an expense category, so it can't be a fund.",
                                name
                            )),
                        );
                    }
                    vec![(r.get::<Uuid, _>("id"), r.get::<bool, _>("is_fund"))]
                }
                Ok(None) => {
                    return (
                        None,
                        Some(format!("I couldn't find a category named '{}' in the active budget.", name)),
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "SET_CATEGORY_FUND: lookup failed");
                    return (None, Some("I couldn't update that category's fund setting.".to_string()));
                }
            }
        }
        None => {
            let rows = sqlx::query(
                "SELECT id, is_fund FROM categories WHERE budget_id = $1 AND category_type = 'expense'",
            )
            .bind(bid)
            .fetch_all(&state.db)
            .await;
            match rows {
                Ok(rows) if !rows.is_empty() => rows
                    .into_iter()
                    .map(|r| (r.get::<Uuid, _>("id"), r.get::<bool, _>("is_fund")))
                    .collect(),
                Ok(_) => {
                    return (
                        None,
                        Some("There are no expense categories in the active budget to change fund status for.".to_string()),
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "SET_CATEGORY_FUND: bulk lookup failed");
                    return (None, Some("I couldn't update the category fund settings.".to_string()));
                }
            }
        }
    };

    let mut changed: u64 = 0;
    for (cid, cur_fund) in &targets {
        let res = if flag && !*cur_fund {
            sqlx::query(
                "UPDATE categories SET is_fund = TRUE, fund_balance = 0, fund_advanced_through = $1 WHERE id = $2",
            )
            .bind(period_start)
            .bind(cid)
            .execute(&state.db)
            .await
        } else if !flag && *cur_fund {
            sqlx::query("UPDATE categories SET is_fund = FALSE WHERE id = $1")
                .bind(cid)
                .execute(&state.db)
                .await
        } else {
            continue; // already in the requested state
        };
        match res {
            Ok(r) if r.rows_affected() > 0 => changed += 1,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, category_id = %cid, "SET_CATEGORY_FUND: update failed");
            }
        }
    }

    let state_word = if flag { "on" } else { "off" };
    if changed > 0 {
        let log = match &cat_name {
            Some(name) => format!("Turned fund {} for category '{}'", state_word, name),
            None => format!(
                "Turned fund {} for {} expense categor{}",
                state_word,
                changed,
                if changed == 1 { "y" } else { "ies" }
            ),
        };
        log_audit(&state.db, bid, user_id, "AI_SET_CATEGORY_FUND", &log).await;
        (Some(log), None)
    } else {
        // Every target was already in the requested state — idempotent
        // no-op, not an error, but say so rather than returning nothing.
        let log = format!(
            "Fund is already {} for {}",
            state_word,
            cat_name.as_deref().unwrap_or("all expense categories")
        );
        (Some(log), None)
    }
}
```

- [ ] **Step 7: Wire the dispatch arm**

Insert into the `match action` block, immediately after `"SET_CATEGORY_ROLLOVER"`'s closing brace
(currently before `"UPDATE_CATEGORY" =>` at line 2241):

```rust
        "SET_CATEGORY_FUND" => {
            let (log, err) = chat_set_category_fund(
                &state,
                user_id,
                active_budget_id,
                parsed_ai_res.action_params.as_ref(),
            )
            .await;
            mutation_log = log;
            mutation_error = err;
        }
```

- [ ] **Step 8: Compile-check**

Run: `cd backend && cargo check`
Expected: PASS.

- [ ] **Step 9: Run the full suite**

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#228): add SET_CATEGORY_FUND chat action"
```

(DB-backed direct tests for `chat_set_category_fund` are added in Task 11, alongside the prompt
rule addition, so the action is both documented to the model and directly testable in the same
review pass.)

---

### Task 9: `CREATE_CATEGORY` accepts `is_fund`

**Files:**
- Modify: `backend/src/rag.rs` — `chat_create_categories` (currently lines 4269-4365, already
  updated for `AiCategorySpec.is_fund` in Task 8 Step 2)

- [ ] **Step 1: Write the failing test**

Find the existing test `chat_create_categories_type_default_and_variety` (search for it in the
test module — currently around line 9625) as the closest structural precedent, and add a new,
narrower test near it:

```rust
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_create_categories_is_fund_initializes_balance_and_marker() {
        // Follow the exact setup pattern of chat_create_categories_creates_all
        // (same test module, connect via DATABASE_URL, seed a user + budget,
        // build AppState via test cipher, call chat_create_categories directly).
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("fund-create-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, 'Fund Create Test', 'monthly', 'time_based')",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()),
        };

        let params = AiActionParams {
            category_name: Some("Vacation Fund".to_string()),
            category_type: Some("expense".to_string()),
            category_limit: Some(200.0),
            is_fund: Some(true),
            ..Default::default()
        };

        let (log, err) = super::chat_create_categories(&state, user_id, Some(budget_id), Some(&params)).await;
        assert!(err.is_none(), "expected no error: {:?}", err);
        assert!(log.is_some());

        let row = sqlx::query(
            "SELECT is_fund, fund_balance, fund_advanced_through FROM categories \
             WHERE budget_id = $1 AND name = 'Vacation Fund'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("fetch created category");
        assert!(row.get::<bool, _>("is_fund"), "category created as a fund");
        assert_eq!(row.get::<f64, _>("fund_balance"), 0.0, "fresh fund starts at 0");
        assert!(
            row.get::<Option<DateTime<Utc>>, _>("fund_advanced_through").is_some(),
            "marker initialized"
        );

        // Cleanup.
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_create_categories_is_fund_silently_coerced_off_for_non_expense() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("fund-coerce-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, 'Fund Coerce Test', 'monthly', 'time_based')",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");

        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()),
        };

        let params = AiActionParams {
            category_name: Some("Salary".to_string()),
            category_type: Some("income".to_string()),
            category_limit: None,
            is_fund: Some(true),
            ..Default::default()
        };

        let (_log, err) = super::chat_create_categories(&state, user_id, Some(budget_id), Some(&params)).await;
        assert!(err.is_none(), "creation itself still succeeds: {:?}", err);

        let row = sqlx::query("SELECT category_type, is_fund FROM categories WHERE budget_id = $1 AND name = 'Salary'")
            .bind(budget_id)
            .fetch_one(&pool)
            .await
            .expect("fetch created category");
        assert_eq!(row.get::<String, _>("category_type"), "income");
        assert!(!row.get::<bool, _>("is_fund"), "is_fund silently coerced to false for a non-expense category");

        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

**Note for the implementer:** `AiActionParams` currently has no `Default` impl (check via
`cargo check` after adding `..Default::default()` above). If it doesn't derive `Default`, add
`#[derive(Default)]` to the struct (alongside its existing `#[derive(Deserialize, Serialize, Debug,
Clone)]`) rather than hand-writing every field in each test literal — every field is already an
`Option<T>` (defaults to `None`), so `Default` is trivially derivable and this is the lowest-risk
fix. Apply the same to `AiCategorySpec` only if a test needs it (it currently isn't used with
`..Default::default()` above; skip unless a compile error demands it).

- [ ] **Step 2: Run to verify they fail**

Run: `cd backend && cargo test chat_create_categories_is_fund -- --ignored`
Expected: FAIL (compile error initially — `is_fund` field missing on `AiCategorySpec`/params
handling in `chat_create_categories`; once compiling, FAIL on the assertion that `is_fund` is set,
since the insert path doesn't handle it yet).

- [ ] **Step 3: Implement — extend `chat_create_categories`'s insert loop**

In `chat_create_categories` (currently lines 4269-4365), after the existing
`CATEGORY_LIMIT_UPSERT_QUERY` call succeeds (`if c_row.is_ok() { ... }` block, currently starting
around line 4323), add a follow-up guarded UPDATE when the spec requests a fund AND the resolved
type is expense:

```rust
                        if let Ok(row) = &c_row {
                            let created_id: Uuid = row.get("id");
                            if spec.is_fund.unwrap_or(false) && c_type == "expense" {
                                // Mirrors chat_set_category_fund's enable path: reset
                                // balance + marker to the current period start. Only
                                // applied on a genuine NEW enable (a re-created/upserted
                                // existing fund category would otherwise reset its
                                // balance on every re-create, which is NOT what "create"
                                // means for an existing category) — guard on is_fund
                                // being currently false.
                                let (period_start, _) = crate::budget::current_period_window(
                                    &brow_time_frame, chrono::Utc::now(),
                                );
                                sqlx::query(
                                    "UPDATE categories SET is_fund = TRUE, fund_balance = 0, \
                                     fund_advanced_through = $1 WHERE id = $2 AND is_fund = FALSE",
                                )
                                .bind(period_start)
                                .bind(created_id)
                                .execute(&state.db)
                                .await
                                .ok();
                            }
                        }
```

This references `brow_time_frame`, which does not currently exist in `chat_create_categories` —
add it once, before the `for spec in &specs` loop (the function currently has no budget row fetch;
add one, mirroring the pattern already used in `create_category`/`update_category` in
`budget.rs`):

```rust
                    let brow_time_frame: String = sqlx::query_scalar(
                        "SELECT time_frame FROM budgets WHERE id = $1",
                    )
                    .bind(bid)
                    .fetch_one(&state.db)
                    .await
                    .unwrap_or_else(|_| "monthly".to_string());

                    let mut created: Vec<String> = Vec::new();
```

(Insert this `brow_time_frame` fetch immediately before the existing `let mut created: Vec<String>
= Vec::new();` line, currently around line 4304.)

Also add `is_fund` handling in the singular-field path (currently around line 4285-4291, where a
top-level `category_name`/`category_type`/`category_limit` is pushed into `specs`):

```rust
            if let Some(name) = &params.category_name {
                specs.push(AiCategorySpec {
                    category_name: name.clone(),
                    category_type: params.category_type.clone(),
                    category_limit: params.category_limit,
                    is_fund: params.is_fund,
                });
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test chat_create_categories_is_fund -- --ignored`
Expected: PASS.

- [ ] **Step 5: Run the full existing `chat_create_categories_*` test cluster to confirm no
  regression**

Run: `cd backend && cargo test chat_create_categories -- --ignored`
Expected: PASS (all existing tests unaffected — `is_fund` defaults to `None`/`false` when absent).

- [ ] **Step 6: Run the full unignored suite**

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#228): CREATE_CATEGORY accepts is_fund"
```

---

### Task 10: LLM prompt rule (FUND CATEGORIES)

**Files:**
- Modify: `backend/src/rag.rs` — insert a new rule after 2l (AMOUNT MODE, currently line 1407) and
  before rule 3 (currently line 1408)

- [ ] **Step 1: Add the rule**

Insert between the existing rule 2l line and rule 3 line:

```rust
         2m. FUND CATEGORIES: A 'fund' (envelope / sinking-fund) category accumulates a running balance CUMULATIVELY across ALL periods since it was made a fund — not just the single previous period. Each period, its unused amount (limit minus spent) is ADDED to the balance; overspending SUBTRACTS from it, and the balance CAN GO NEGATIVE after sustained overspend. This is DIFFERENT from category rollover (rule 2f): rollover only looks at ONE previous period and NEVER goes negative (it clamps at zero), while a fund's balance keeps accumulating and can swing positive or negative indefinitely. If the user says e.g. 'make groceries a fund', 'turn on a running balance for X', 'let unused amount in this category build up over time', or 'let overspending here carry a deficit into next month', set 'action' to 'SET_CATEGORY_FUND', set 'is_fund' to true/false, and populate 'category_name' (or OMIT it to apply to ALL expense categories). You can also set 'is_fund' on CREATE_CATEGORY to create a category as a fund in one step. Funds only apply to EXPENSE categories. The CATEGORIES context shows each fund category's 'Fund: on', 'Balance', and 'Effective limit' — use those exact figures; never invent them.\n\
```

- [ ] **Step 2: Compile-check**

Run: `cd backend && cargo check`
Expected: PASS.

- [ ] **Step 3: Check for an existing prompt-rule-count/format test**

Search the test module for any test asserting on the exact set/count of numbered rules (e.g. a
test that greps `system_instructions` for `"2m."` or counts rule markers) — if one exists, it may
need `2m.` added to its expected list. If none exists, skip this step.

Run: `cd backend && cargo test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#228): add FUND CATEGORIES LLM prompt rule (2m)"
```

---

### Task 11: DB-backed test for the `SET_CATEGORY_FUND` chat write

**Files:**
- Modify: `backend/src/rag.rs` — add tests near the existing `chat_create_categories_*` DB test
  cluster (search for `chat_create_categories_creates_all`, currently around line 9045, for the
  exact setup boilerplate to mirror)

- [ ] **Step 1: Write the tests**

```rust
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_set_category_fund_enables_and_initializes() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("fund-set-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, 'Fund Set Test', 'monthly', 'time_based')",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        let cat_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Groceries', 'expense', 100.0)",
        )
        .bind(cat_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed category");

        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()),
        };
        let params = AiActionParams {
            category_name: Some("Groceries".to_string()),
            is_fund: Some(true),
            ..Default::default()
        };

        let (log, err) = super::chat_set_category_fund(&state, user_id, Some(budget_id), Some(&params)).await;
        assert!(err.is_none(), "expected no error: {:?}", err);
        assert!(log.is_some());

        let row = sqlx::query("SELECT is_fund, fund_balance, fund_advanced_through FROM categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .expect("fetch category");
        assert!(row.get::<bool, _>("is_fund"));
        assert_eq!(row.get::<f64, _>("fund_balance"), 0.0);
        assert!(row.get::<Option<DateTime<Utc>>, _>("fund_advanced_through").is_some());

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'AI_SET_CATEGORY_FUND'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("count audits");
        assert_eq!(audit_count, 1);

        sqlx::query("DELETE FROM audit_logs WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_set_category_fund_disable_preserves_balance() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("fund-disable-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, 'Fund Disable Test', 'monthly', 'time_based')",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        let cat_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories \
             (id, budget_id, name, category_type, category_limit, is_fund, fund_balance, fund_advanced_through) \
             VALUES ($1, $2, 'Groceries', 'expense', 100.0, TRUE, 75.0, NOW())",
        )
        .bind(cat_id)
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed fund category");

        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()),
        };
        let params = AiActionParams {
            category_name: Some("Groceries".to_string()),
            is_fund: Some(false),
            ..Default::default()
        };

        let (log, err) = super::chat_set_category_fund(&state, user_id, Some(budget_id), Some(&params)).await;
        assert!(err.is_none(), "expected no error: {:?}", err);
        assert!(log.is_some());

        let row = sqlx::query("SELECT is_fund, fund_balance FROM categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .expect("fetch category");
        assert!(!row.get::<bool, _>("is_fund"), "disabled");
        assert_eq!(row.get::<f64, _>("fund_balance"), 75.0, "balance preserved (non-destructive, #50 precedent)");

        sqlx::query("DELETE FROM audit_logs WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_set_category_fund_rejects_non_expense_category() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(format!("fund-reject-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        let budget_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) \
             VALUES ($1, $2, 'Fund Reject Test', 'monthly', 'time_based')",
        )
        .bind(budget_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type) VALUES ($1, $2, 'Salary', 'income')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .execute(&pool)
        .await
        .expect("seed category");

        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()),
        };
        let params = AiActionParams {
            category_name: Some("Salary".to_string()),
            is_fund: Some(true),
            ..Default::default()
        };

        let (log, err) = super::chat_set_category_fund(&state, user_id, Some(budget_id), Some(&params)).await;
        assert!(log.is_none());
        assert!(err.is_some(), "expected a clear error for a non-expense target");
        assert!(err.unwrap().contains("not an expense category"));

        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

- [ ] **Step 2: Run the tests**

Run: `cd backend && cargo test chat_set_category_fund -- --ignored`
Expected: PASS (3 tests).

- [ ] **Step 3: Run the full test suite (unignored + ignored)**

Run: `cd backend && cargo test && cargo test -- --ignored`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add backend/src/rag.rs
git commit -m "test(#228): DB-backed tests for the SET_CATEGORY_FUND chat write"
```

---

### Task 12: AGENTS.md documentation + frontend verification

**Files:**
- Modify: `AGENTS.md` — add a new numbered section after §11 (Stripe Subscription Billing,
  currently ending at line 84, before `## Developer Commands` at line 86)
- Verify only (no expected code change): `frontend/src/lib/CategoriesView.svelte`

- [ ] **Step 1: Add the AGENTS.md section**

Insert after §11's last bullet (currently line 84), before `## Developer Commands` (currently line
86):

```markdown
### 12. Fund Categories (#228)
- **`is_fund` flag, independent of rollover**: `categories.is_fund BOOLEAN NOT NULL DEFAULT FALSE` marks an expense category as a "fund" (envelope / sinking-fund). It is orthogonal to `rollover_enabled` (#49) and to `category_type` (a data-layer CHECK, `categories_is_fund_expense_only_check`, restricts it to `category_type = 'expense'`). DEFAULT FALSE keeps every existing category unaffected.
- **Cumulative and bidirectional (NOT #47/#49's single-period clamp)**: `categories.fund_balance DOUBLE PRECISION NOT NULL DEFAULT 0` is a running total across ALL periods since the fund was enabled — unlike #47/#49's `carried_amount`, which looks at only the ONE previous period and clamps at zero. Each period, `fund_balance += (category_limit - period_spent)`; an overspend SUBTRACTS from the balance and can drive it negative. The effective limit for the current period is `budget::fund_effective_limit(is_fund, category_limit, fund_balance)` = `category_limit + fund_balance`, with **no floor**.
- **Materialized, NOT computed-on-read (a deliberate departure)**: every other rollover/carry feature in this codebase (#47/#49/#51 — see §4/§7 above) is computed on demand from the CURRENT limit and a period-windowed spend query, with no scheduler and no persisted running figure. Funds break that pattern on purpose: the effective amount depends on the base limit AS IT WAS in each PAST period, and limits change over time, so a computed-on-read sum would retroactively rewrite history every time a limit is edited. `fund_balance` is instead materialized and advanced incrementally, once per completed period, by a background job — the correctness of that advancement (idempotent, exactly-once per period, safe to run hourly) is the central risk this feature manages.
- **Idempotency marker**: `categories.fund_advanced_through TIMESTAMPTZ` (NULL = never a fund) is the boundary up to which `fund_balance` already reflects completed periods — mirroring #51's `next_renewal_at` pattern, but inverted: it marks a *past* boundary already applied, not a *future* due time. Enabling `is_fund` (via `SET_CATEGORY_FUND` or `CREATE_CATEGORY`) resets `fund_balance = 0` and sets the marker to the CURRENT period's start — periods before the fund existed never count. Disabling preserves both `fund_balance` and the marker (non-destructive, matching #50 archive's "toggle preserves data" precedent); re-enabling resumes from the preserved balance with the marker reset to the (new) current period start.
- **Background job, independent of `auto_renew`**: `budget::advance_fund_categories` runs on the SAME hourly ticker as `renew_due_budgets` (`main.rs`) but is a SEPARATE job selecting `categories.is_fund = TRUE` directly — it must fire for ANY fund category regardless of whether its budget has `auto_renew` on, since `renew_due_budgets` only ever selects `auto_renew = TRUE` budgets (§7 above). Unlike `renew_due_budgets`'s single jump-to-the-next-FUTURE-boundary catch-up, this job walks ONE PERIOD INCREMENT AT A TIME through every completed period the marker hasn't crossed yet (bounded to 24 iterations per category per tick) — because a fund's balance must accumulate per period, not skip straight to the present. Each increment's UPDATE is guarded by `fund_advanced_through = $old_marker`, so a concurrent/repeated tick cannot double-apply (idempotent within a period, same contract as `renew_due_budgets`).
- **Known limitation — limit-change fidelity during catch-up**: there is no historical per-period limit table in this schema, so a multi-period catch-up (e.g. after downtime) applies EACH crossed period's increment using the category's CURRENT `category_limit`, not necessarily the limit that was nominally in effect during that specific past period. A limit edited between a missed boundary and the next hourly tick is applied retroactively to that catch-up increment. Since the job runs hourly and periods are typically monthly, this window is small in practice; a true historical-limit ledger is a natural follow-up, out of scope for #228.
- **Excludes project / closed / archived budgets** (consistent with #47/#48/#49/#51): the job's WHERE filter requires `budget_type = 'time_based' AND closed_at IS NULL AND archived_at IS NULL`. A fund category on an excluded budget simply stops advancing (balance frozen) until the exclusion lifts.
- **Precedence over #49 rollover for the SAME category**: `is_fund` and `rollover_enabled` are independent toggles, but when both are on for one category, the read path (`budget::category_response_with_carry`, the RAG per-category context) uses the fund balance INSTEAD of the #49 one-period carry — never both — to avoid double-counting the same accumulated credit under two labels. The underlying `category_carried` pure helper (#49) is unchanged; only its *use* is superseded for fund categories.
- **Surfacing**: REST — `CategoryResponse.is_fund`/`fund_balance` (read-only; no REST-write path in #228's scope, chat is the only write path per this app's chat-first convention). The category table (`build_categories_table_html`) shows a fund badge, the carried balance, and computes Remaining against the effective limit (sums the EFFECTIVE limit, not the bare `category_limit`, in the totals row); negative effective limits/remaining render via the existing `money()` helper's signed formatting, with a deficit CSS class. The RAG per-category context shows `Fund: on | Balance: $X | Effective limit: $Y` (superseding the Rollover/Carried annotation for that category). Chat — `SET_CATEGORY_FUND` toggles a named category or ALL expense categories (audit `AI_SET_CATEGORY_FUND`); `CREATE_CATEGORY` accepts `is_fund` to create a fund in one step; both are documented in prompt rule 2m, distinguishing funds from #49 rollover for the model.
```

- [ ] **Step 2: Verify the frontend needs no change**

Run the backend locally with the DB up (`podman-compose up -d && cd backend && cargo run`), create
a test budget + expense category via the app UI, enable its fund status via direct SQL (`UPDATE
categories SET is_fund = TRUE, fund_balance = 42, fund_advanced_through = NOW() WHERE id =
'<id>';`), then open the frontend (`cd frontend && pnpm run dev`), navigate to that budget's
Categories view, and confirm the rendered table shows the fund badge/balance/effective-limit
markup from Task 5 with no frontend code change needed (`CategoriesView.svelte`
`{@html}`-injects `GET /budgets/:id/categories-table`'s response verbatim). Specifically confirm
the "Fund" badge renders as a colored daisyUI badge (not plain unstyled text) and that a negative
balance/remaining renders in the `text-error` warning color — this is the concrete check that Task
5's class-reuse fix (using `badge badge-warning badge-sm` / `text-error` instead of inventing new,
unstyled class names) actually took effect visually, not just in the unit tests' substring
assertions. Take a screenshot or note the visual confirmation in the PR description.

- [ ] **Step 3: Final full-suite run**

Run: `cd backend && cargo test && cargo test -- --ignored`
Expected: PASS (all tests, including every new one from Tasks 1-11).
Run: `cd backend && cargo clippy --all-targets 2>&1 | tail -50` (if the repo's CI runs clippy —
check `.github/workflows/deploy-backend.yml` for a lint step; if present, fix any new warnings
introduced by this feature before committing).

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#228): add Fund Categories AGENTS.md section"
```

---

## Post-implementation notes for the PR

- REST-write (`CategoryPayload.is_fund` on create/update) was deliberately left out of scope (see
  spec §8's resolved open question) — the ticket's acceptance criteria only require
  `CategoryResponse` read exposure and the chat write path (`SET_CATEGORY_FUND` /
  `CREATE_CATEGORY`), matching this app's chat-first convention (AGENTS.md's framing throughout
  §4-§11). Call this out explicitly in the PR description as a scope decision, not an oversight.
- The multi-period catch-up limit-change-fidelity limitation (Task 12, AGENTS.md) should be
  called out in the PR description too, since it's the one place this implementation's behavior
  could plausibly surprise a future reader who assumes true historical-limit accuracy.
