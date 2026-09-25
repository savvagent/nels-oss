# Rollup Categories-Table Mirror Totals Fix (nels#298) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `budget::category_table_rows` so a rollup mirror row (`linked_budget_id IS NOT
NULL`) carries the SOURCE (child) budget's own resolved aggregate `category_limit` and
current-window `spent`, instead of the mirror category's own (always-empty) columns — fixing
the categories table's Limit/Spent/Remaining for a rolled-up child on both consumers of that
shared function (`GET /budgets/:id/categories-table` and the chat CATEGORIES/CATEGORY_BALANCE
contexts) in one place.

**Architecture:** One backend-only change in `backend/src/budget.rs`. After the existing
per-category query runs, `category_table_rows` collects the distinct `linked_budget_id`s among
its rows (empty for the overwhelming majority of budgets — zero extra queries in that case),
batch-resolves each non-archived source's own aggregate total (reusing the existing, already
mode-aware `computed_budget_totals`) and own current-window spend (a new small helper
`source_spend_window`, factored out of the existing `linked_budgets_spent`'s inline
project-vs-time_based branch so the two call sites cannot drift, feeding the existing
`period_expense_spent_many`), then overwrites each mirror row's `category_limit`/`spent` from
those two maps. No rendering code changes — `build_categories_table_html`/`push_category_row`
and `rag.rs`'s "Spent/Earned" line already read `category_limit`/`spent` directly, so populating
them correctly is sufficient. `rag.rs`'s existing, separately-computed mirror-Limit resolution
(`render_categories_context`'s `linked_source_totals` map) is untouched — already correct, and
out of scope (confirmed by spec review).

**Tech Stack:** Rust (axum, sqlx/PostgreSQL). No new dependencies, no migration, no REST/API
shape change (`CategoryTableRow`'s fields are unchanged; only two existing fields' *values*
change for mirror rows).

---

## File Structure

- **Modified `backend/src/budget.rs`**:
  - `linked_budgets_spent`: extracted its inline per-source window branch into a new private
    pure helper `source_spend_window`, used by both the existing function and the new
    resolution step (no behavior change to `linked_budgets_spent` itself).
  - `category_table_rows`: added the post-processing mirror-resolution step described above,
    plus a new private async helper `resolve_linked_source_totals_and_spend`.
  - The "Rollup (#52) DB-backed test helpers" section (right after
    `mirror_category_includes_source_total_in_parent`): two new DB-backed tests reusing the
    existing `rollup_test_setup`/`seed_rollup_budget`/`rollup_cleanup` helpers.
  - `categories_table_tests` (the `build_categories_table_html` pure unit test module, next to
    `null_limit_renders_dash`): one new pure test.
- **Created `docs/superpowers/specs/2026-07-04-rollup-categories-table-mirror-totals-design.md`**
  and **`docs/superpowers/plans/2026-07-04-rollup-categories-table-mirror-totals.md`** (this
  file) — the committed spec/plan docs, matching this repo's established convention (e.g.
  `docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md`).

No new files for source code, no `rag.rs` changes (its existing Limit resolution for mirrors is
already correct; its Spent line is fixed transitively by this change since it reads
`CategoryTableRow.spent` directly).

---

## Task 1: Extract `source_spend_window` from `linked_budgets_spent`

**Files:**
- Modify: `backend/src/budget.rs` (`linked_budgets_spent`)

- [x] **Step 1: Read the current exact code before editing**
- [x] **Step 2: Add the extracted helper directly above `linked_budgets_spent`**
- [x] **Step 3: Replace `linked_budgets_spent`'s inline window-building loop to call the helper**
- [x] **Step 4: Run the existing rollup/mirror/linked test suite to confirm no behavior change**

  Ran `cargo test rollup -- --ignored`, `cargo test -- --ignored linked`, and `cargo test mirror
  -- --ignored` — all 14 matching DB-backed tests passed (a pure refactor; `linked_budgets_spent`'s
  output is unchanged, only its window-selection code moved into a named helper).

- [x] **Step 5: `cargo check`** — clean (same 7 pre-existing warnings, no new ones).
- [x] **Step 6: Commit** — `refactor(#298): extract source_spend_window from linked_budgets_spent`

---

## Task 2: Resolve mirror rows in `category_table_rows` to the source's own limit + spend

**Files:**
- Modify: `backend/src/budget.rs` (`category_table_rows`)
- Test: `backend/src/budget.rs`, rollup DB-backed test section (right after
  `mirror_category_includes_source_total_in_parent`)

- [x] **Step 1: Read the current exact code before editing**
- [x] **Step 2: Write the failing test first** —
  `category_table_rows_resolves_mirror_to_source_aggregate`, reusing
  `rollup_test_setup`/`seed_rollup_budget`/`rollup_cleanup`.
- [x] **Step 3: Run the test to verify it fails** — failed as expected on
  `assert_eq!(mirror.category_limit, Some(300.0), ...)` (was `None`), confirming the test
  exercises the bug.
- [x] **Step 4: Implement — add the mirror-resolution step to `category_table_rows`** plus the
  new `resolve_linked_source_totals_and_spend` helper.
- [x] **Step 5: Run the test to verify it passes** — PASS.
- [x] **Step 6: Run the pre-existing `category_table_rows_sums_current_period` test** — PASS
  unmodified (its mirror section only asserts `id`/`rollover_enabled`/`linked_budget_id`, never
  `category_limit`/`spent` for the mirror).
- [x] **Step 7: `cargo check` and `cargo build`** — clean.
- [x] **Step 8: Commit** — `fix(#298): rollup mirror rows resolve to the source's own aggregate
  limit/spend` (this commit also includes Task 3's archived-source test, written alongside it
  in the same DB-backed test section before the intervening pure-test commit).

---

## Task 3: Archived-source edge case + pure HTML-rendering test

**Files:**
- Test: `backend/src/budget.rs`, rollup DB-backed test section (alongside Task 2's test)
- Test: `backend/src/budget.rs`, `categories_table_tests` module (next to
  `null_limit_renders_dash`)

- [x] **Step 1: Write the failing DB-backed test** —
  `category_table_rows_archived_mirror_source_resolves_to_zero`.
- [x] **Step 2: Run it** — PASS (Task 2's implementation already excludes archived sources via
  the `WHERE archived_at IS NULL` clause in `resolve_linked_source_totals_and_spend`).
- [x] **Step 3: Add a pure (non-DB) rendering test** —
  `build_categories_table_html_renders_resolved_mirror_limit_and_spent`, confirmed the existing
  `row(name, ty, limit, spent)` helper's signature via `rg` before use (matched exactly).
- [x] **Step 4: Run all new/modified tests together** — full non-DB suite: 298 passed, 0
  failed, 154 ignored. Full DB-backed suite (`-- --ignored`): 154 passed, 0 failed.
- [x] **Step 5: `cargo clippy`** — identical warning count (52 warnings, 16 duplicates) with and
  without this change, confirmed by diffing clippy output with/without the working-tree diff
  stashed. No new warnings.
- [x] **Step 6: Commit** — `test(#298): cover mirror-row HTML rendering for resolved
  limit/spent`.

---

## Task 4: Commit the spec + plan docs

**Files:**
- Create: `docs/superpowers/specs/2026-07-04-rollup-categories-table-mirror-totals-design.md`
- Create: `docs/superpowers/plans/2026-07-04-rollup-categories-table-mirror-totals.md`

- [x] **Step 1: Save the finalized spec and this plan** to the two paths above.
- [x] **Step 2: Commit** — `docs(#298): spec + plan for rollup categories-table mirror totals
  fix`.

---

## Final Verification

- [x] `cd backend && cargo test` — 298 passed, 0 failed, 154 ignored.
- [x] `cd backend && cargo test -- --ignored` — 154 passed, 0 failed.
- [x] `cd backend && cargo check && cargo clippy --all-targets` — clean, no new warnings.
- [ ] Manual smoke — optional; the DB-backed tests exercise the exact repro shape (parent +
  same-type source, mirror category, source's own limit + in-window transaction) end-to-end
  against a real Postgres, so this is treated as covered by the automated DB-backed tests given
  the repo has no CI test gate on PRs (see the spec's Risk section).

## Note carried into the PR description

`rag.rs::render_categories_context`'s internal `b_limit` accumulator sums
`c.category_limit.unwrap_or(0.0)` unconditionally, including mirror rows. Before this fix
that's a no-op (mirror's `category_limit` was always `None`). After this fix, a mirror row's
`category_limit` is a real number, so that accumulator becomes very slightly more accurate in
the rare DB-error fallback path (`rag.rs:1031-1035`, used only when the authoritative
`computed_budget_total` call fails) — a harmless side effect (it was previously *undercounting*
mirrors in that fallback; now it doesn't), not a regression, not in scope to change.
