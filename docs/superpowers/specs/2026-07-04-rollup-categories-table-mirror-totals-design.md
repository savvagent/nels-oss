# Spec: Rolled-up categories table shows child's real aggregate limit/spent/remaining (nels#298)

## 1. Brief (verbatim from the ticket)

Repro: create two budgets sharing the same budget_type (both project or both time_based) —
Parent and Child. Roll up Child into Parent, creating a mirror category in Parent
(categories.linked_budget_id = Child.id). Give Child its own category limits and post
transactions against Child's own categories. View Parent's categories table (GET
/budgets/:id/categories-table, or the chat-rendered categories context - both share the same
backend source).

Expected: the mirror row representing Child in Parent's categories table shows Child's real
aggregate position - Limit = Child's aggregate budget total, Spent = Child's aggregate
current-period spend across its own categories, Remaining = Limit - Spent.

Actual: the mirror row's Limit/Remaining render "—" and Spent renders "$0", regardless of
Child's real activity.

Root cause: category_table_rows (backend/src/budget.rs:3972) — the sole source feeding both
the GET /budgets/:id/categories-table HTML table and the chat categories context (nels#282) —
selects c.category_limit and sums transactions directly against each category's own id.
Mirror categories never carry their own category_limit (NULL by construction) and never
receive transactions directly (all activity lives on the child budget's own categories). The
row's linked_budget_id is fetched but never resolved to the child's aggregate totals anywhere
in this path.

This is distinct from the list_categories REST endpoint (backend/src/budget.rs:2810), which
does resolve linked_budget_id for base_amount/effective_amount via
computed_budget_total/computed_budget_totals (backend/src/budget.rs:1016, :1084) - but that
endpoint has no spent field at all, so it doesn't help the table view either.

Acceptance criteria (verbatim):
- When a rolled-up child budget shares the same budget_type as its parent, the parent's
  categories table (both the HTML endpoint and the chat-rendered categories context, since
  both share category_table_rows) shows, for the mirror row representing that child: Limit =
  the child's aggregate budget total (computed_budget_total-equivalent), Spent = the child's
  aggregate current-period spend across its own categories, Remaining = Limit − Spent.
- Existing behavior for ordinary (non-mirror) categories is unchanged.
- Existing category_table_rows / build_categories_table_html tests continue to pass; new tests
  cover a same-type rollup's mirror row showing the child's real aggregate
  limit/spent/remaining.

References: Rollup design #52; categories table shared source #176, #282.

## 2. Assumptions

1. **The "same budget_type" qualifier in the AC describes the repro, not a behavioral
   condition.** The existing per-source resolution helpers this fix reuses
   (`computed_budget_total`/`computed_budget_totals` for limit, and the
   project-span-vs-current-period windowing already used by `linked_budgets_spent` for spend)
   already resolve strictly from the SOURCE (child) budget's own `budget_type`/`time_frame`,
   never the parent's. They are unconditionally correct for a same-type OR cross-type rollup
   (e.g. a project child rolled into a time_based parent). Implementing the general case (not
   gating on `parent.budget_type == child.budget_type`) satisfies the literal AC (which only
   requires the same-type case to work) without adding a special case that would leave the
   cross-type case silently wrong. Rationale: the codebase's rollup design (#52, AGENTS.md §8)
   never conditions rollup resolution on matching budget_type anywhere else (link validation
   only forbids cycles/self-links), so gating this one path on type-match would be an
   inconsistent, arbitrary restriction.
2. **Archived sources resolve to Limit = $0 / Spent = $0 for their mirror row**, matching the
   established, documented precedent (AGENTS.md §8: "archived sources are EXCLUDED from a
   rollup parent on BOTH sides... its mirror resolves to 0") already implemented in
   `computed_budget_total` (limit-side) and `linked_budgets_spent` (spend-side). This is an
   edge case beyond the AC's literal repro, but resolving it any other way (e.g. leaving "—")
   would contradict the existing invariant and look like a regression relative to already-
   shipped rollup behavior.
3. **"Child's aggregate current-period spend across its own categories" means the child's own
   expense-category spend for the child's own current window** (its current period for
   time_based, its full project span for project — exactly `linked_budgets_spent`'s per-source
   window derivation), not the parent's window. This matches the phrase "across its own
   categories" and is the only reading consistent with a project-type child (which has no
   "current period").
4. **The chat categories context's Limit line (`rag.rs::render_categories_context`) already
   resolves a mirror's limit correctly** via a separately-computed `linked_source_totals` map
   (added under nels#282, calling the same `computed_budget_totals`). This fix does not need to
   touch `rag.rs` for Limit — only `category_table_rows`'s own `category_limit`/`spent` fields
   need correcting, since `render_categories_context`'s "Spent/Earned" line already reads
   `c.spent` directly, so fixing `category_table_rows`'s `spent` field is enough to fix that
   surface's Spent automatically, without any `rag.rs` edit. Leaving `rag.rs`'s existing
   (already-correct, if slightly redundant) Limit computation alone keeps this change minimal
   and avoids touching a working code path / its existing unit tests.
5. **`CATEGORY_BALANCE`'s single-row HTML render** (`resolve_category_balance` ->
   `build_categories_table_html`) is fixed for free by fixing `category_table_rows`, since it's
   the same underlying row source. No separate work item.

## 3. Goal & Success Criteria

**Goal:** Fix `category_table_rows` so a rollup mirror row (`linked_budget_id IS NOT NULL`)
carries the SOURCE (child) budget's own resolved aggregate `category_limit` and
current-window `spent`, instead of the mirror category's own (always-empty) columns — so every
consumer of that shared function (the HTML categories table, the chat CATEGORIES context, and
the CATEGORY_BALANCE single-category chat render) shows the child's real position.

Success criteria:
- [x] A mirror row's `category_limit` resolves to the child's aggregate budget total
      (mode-aware: `'fixed'` uses `budget_limit`, `'derived'` sums the child's own expense
      category limits) — the same value `computed_budget_total(child_id)` would return.
- [x] A mirror row's `spent` resolves to the child's own expense-category spend over the
      child's own current window (current period for `time_based`, full project span for
      `project`).
- [x] `build_categories_table_html`'s Remaining cell for that row is `Limit − Spent` (falls out
      of the existing `push_category_row` logic once `category_limit`/`spent` are populated —
      no rendering-code change needed).
- [x] An archived child's mirror row resolves to Limit = $0 / Spent = $0 (not "—" / stale data).
- [x] Ordinary (non-mirror) rows are byte-for-byte unchanged.
- [x] A budget with no mirror categories triggers zero additional queries (fast path
      preserved).

## 4. Scope

**In scope:**
- `backend/src/budget.rs`: `category_table_rows` gains a post-processing resolution step for
  mirror rows; a small new private helper (batched lookup); reuse of existing
  `computed_budget_totals` and a small shared per-source-window helper factored out of
  `linked_budgets_spent` (to avoid duplicating the project-vs-time_based window branch in two
  places).
- New tests in `budget.rs`: two DB-backed `category_table_rows` tests (a same-type rollup with
  the source's own limit + in-window transaction; an archived-source edge case resolving to 0)
  reusing the existing `rollup_test_setup`/`seed_rollup_budget`/`rollup_cleanup` fixtures, plus
  one pure `build_categories_table_html` unit test asserting a mirror row with a resolved
  `category_limit`/`spent` renders real numbers (not "—"/$0).
- A committed spec (this doc) and plan under `docs/superpowers/specs/` /
  `docs/superpowers/plans/`, matching this repo's established convention for feature/fix work.

**Out of scope:**
- Any change to `rag.rs` (`render_categories_context`, `resolve_category_balance`,
  `linked_source_totals`/`archived_sources` plumbing) — already correct for Limit, and Spent is
  fixed transitively.
- Any change to `list_categories` (REST) — already correct per the issue's own description.
- De-duplicating `rag.rs`'s now-partially-redundant `linked_source_totals` computation against
  the newly-resolved `category_limit` — a legitimate follow-up simplification, but touching a
  second, currently-correct code path (and its existing unit tests) is unnecessary risk for
  this fix and not requested by the AC.
- Any change to `notifications::check_and_notify_limits` (explicitly out of scope per AGENTS.md
  §8's documented, intentional scope boundary).

## 5. Architecture

`category_table_rows(pool, budget_id)`:
1. Existing query is unchanged: fetch each category row (including `linked_budget_id`) with
   its own current-period `spent` (0 for a mirror, since it never receives transactions
   directly).
2. **New step**: collect the distinct `linked_budget_id`s present among the returned rows. If
   none, return immediately (no behavior/perf change for the common case).
3. If any exist, batch-resolve via a new private helper
   `resolve_linked_source_totals_and_spend`:
   a. Fetch each source's own `budget_type`/`time_frame`/`created_at`/`closed_at` restricted to
      `archived_at IS NULL` (mirrors `linked_budgets_spent`'s existing source query) — a source
      absent from this result (because it's archived) resolves to 0 for both limit and spend
      below.
   b. `computed_budget_totals(pool, &active_source_ids)` → limit-by-source map (reused
      unchanged; already mode-aware and already the function REST's `list_categories` uses for
      the same "child's aggregate budget total" concept).
   c. For spend: derive each active source's own window via a small shared helper
      `source_spend_window(budget_type, time_frame, created_at, closed_at, now)`, extracted out
      of `linked_budgets_spent`'s existing inline branch so the two call sites cannot drift,
      then `period_expense_spent_many(pool, &windows)` → spend-by-source map (reused
      unchanged).
4. Overwrite each mirror row's `category_limit`/`spent` from the two maps (`unwrap_or(0.0)` —
   absent means archived or a zero total/spend, both correctly render as 0 not "—", since
   `category_limit` becomes `Some(0.0)` rather than `None`).

Error handling: `computed_budget_totals`/`period_expense_spent_many` return
`Result<_, (StatusCode, String)>`, but `category_table_rows` returns `Result<_, sqlx::Error>`
(depended on by callers doing `.await?` in an `sqlx::Error` context, e.g.
`rag.rs::resolve_category_balance`). Adapt via `sqlx::Error::Protocol(msg)` at the two call
sites inside the new helper — these functions can only fail on an underlying `sqlx::Error`
themselves (mapped through `internal_error`), so no information is lost, just re-wrapped.

## 6. Error Handling & Edge Cases

- No mirror categories on this budget → unchanged behavior, zero extra queries.
- Mirror's source budget archived → Limit/Spent both resolve to $0 (not "—"), matching the
  documented #52 invariant.
- Mirror's source budget is itself amount_mode `'fixed'` → Limit resolves to the source's
  `budget_limit`, not a re-sum of its categories (via `computed_budget_totals`, unchanged
  semantics).
- Mirror's source budget is `project`-type → Spent uses the full project span
  (`created_at` → `closed_at` or now), not a calendar period.
- Multiple mirrors on the same parent (rolling up 2+ children) → batched in one query each for
  limits and spends, not N+1.
- A source query failure (DB error mid-request) → propagates as `sqlx::Error` (same
  fail-loud convention as the rest of this function; no silent fallback to "—" for a real
  error, since that would misrepresent a data problem as "no rollup").

## 7. Testing Approach

- `cd backend && cargo test` for the non-DB unit tests (pure `build_categories_table_html`
  addition).
- `cd backend && cargo test -- --ignored` (Postgres via `podman-compose up -d`, already running
  locally) for the two new DB-backed `category_table_rows` tests plus the full existing ignored
  suite, to confirm no regression.
- Existing `category_table_rows_sums_current_period` test continues to pass unmodified.

## 8. Risks & Open Questions

- The "same budget_type" qualifier in the AC could, in principle, mean the fix should be
  gated to only apply when types match — Assumption 1 explains why the unconditional
  (type-agnostic) implementation is the correct, non-arbitrary reading and a strict superset of
  what the AC requires. No repo-specific validator rejects mismatched-type rollups today, so
  gating would silently under-fix a currently-representable state.
- No automated CI test workflow runs on PRs in this repo (`.github/workflows/` has only
  release/deploy workflows) — verification is local (`cargo test` + `cargo test -- --ignored`
  against the local Postgres via `podman-compose up -d`) rather than CI-gated.
- `rag.rs::render_categories_context`'s internal `b_limit` accumulator sums
  `c.category_limit.unwrap_or(0.0)` unconditionally, including mirror rows. Before this fix
  that's a no-op (mirror's `category_limit` was always `None`). After this fix, a mirror row's
  `category_limit` is a real number, so that accumulator becomes very slightly more accurate in
  the rare DB-error fallback path (`rag.rs:1031-1035`, used only when the authoritative
  `computed_budget_total` call fails) — a harmless side effect (it was previously
  *undercounting* mirrors in that fallback; now it doesn't), not a regression, not in scope to
  change.
