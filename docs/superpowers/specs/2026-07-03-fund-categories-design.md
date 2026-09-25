# Fund Categories — Cumulative, Bidirectional Carry-Over — Design Spec

Source: `savvagent/nels#228` — "Fund categories: cumulative, bidirectional carry-over of unused category amounts"

## 1. Brief (verbatim from ticket)

> Add fund categories: a budget category can be marked as a fund (envelope / sinking-fund) so
> its unused amount carries forward cumulatively and bidirectionally across periods. Each
> period, the category's leftover (limit - spent) is added to a running fund balance; overspend
> subtracts from it. The category's effective limit for the current period is limit +
> fund_balance, which can grow over many under-spent periods or go negative after sustained
> overspend.
>
> Worked example (monthly, fund on, base limit $100):
> | Period | Base limit | Spent | Balance after | Effective limit |
> |--------|-----------|-------|---------------|-----------------|
> | Jan    | $100      | $50   | +$50          | Jan: $100       |
> | Feb    | $100      | $70   | +$80          | Feb: $150 (100 + 50) |
> | Mar    | $100      | $200  | -$20          | Mar: $180 (100 + 80) |
> | Apr    | $100      | -     | -$20          | Apr: $80 (100 - 20) |
>
> Not a duplicate of #47/#49 (per-budget / per-category rollover): those look at ONE previous
> period and clamp at zero. Funds are cumulative (running balance across ALL periods since the
> fund was created) and bidirectional (overspend reduces the next period's effective limit and
> can push it negative).

Acceptance criteria are reproduced from the ticket verbatim (see "Goal & Success Criteria" below
— each ticket AC bullet is mapped 1:1 to a measurable bullet).

## 2. Assumptions (decisions on every "confirm" open question)

Each assumption below resolves one of the ticket's explicitly-flagged open questions. No human
is available to confirm live, so the ticket's own "recommend" language is treated as the
decision, and the rationale is recorded here as the durable record.

1. **Limit changed mid-life → future accrual only, no retroactive recompute.** This is
   restated as the *reason* funds are materialized rather than computed-on-read (design decision
   #2, locked in the ticket). Confirmed as final: editing `category_limit` today never rewrites
   `fund_balance` history. Rationale: the ticket itself frames this as "the whole reason for
   materializing" — reopening it would contradict the locked design.

2. **Fund toggled off then on preserves `fund_balance` (non-destructive).** Matches archive's
   precedent (#50, AGENTS.md §6) of preserving flags/data across a hide/show toggle. Disabling
   `is_fund` freezes `fund_balance` at its current value (no reset) and stops further advancement
   (the advancement job's WHERE filter excludes non-fund categories, so a disabled fund simply
   stops accruing — no explicit "pause" bit needed). Re-enabling resumes accrual from the
   preserved balance, with `fund_advanced_through` reset to the *current* period start (mirroring
   initial-enable semantics — periods while the fund was off do not retroactively accrue,
   consistent with "periods before the fund existed do not count").

3. **`is_fund` allowed on `expense` categories only**, enforced asymmetrically by call site
   (matching the codebase's existing "coerce on compound create, error on explicit single-purpose
   action" tone — see §5.5): the explicit, single-purpose `SET_CATEGORY_FUND` chat action rejects
   a non-expense target with a clear chat error (the user's sole intent was "make this a fund";
   silently no-op'ing would be confusing). `CREATE_CATEGORY`'s compound create silently coerces
   `is_fund` to `false` when the resolved `category_type` isn't `expense` (mirroring
   `resolve_category_type`'s existing silent-coercion tone for that same call site). A data-layer
   CHECK constraint (§5.1) makes the invariant unrepresentable regardless of entry point. A
   REST-write path for `is_fund` is not committed in this spec (§8) — this assumption governs
   the paths this spec actually builds (chat + the CHECK constraint), not a REST write that may
   or may not exist. Rationale: the worked example and every read-path integration point
   (spend-vs-limit, `CategoryTableRow` Remaining column, RAG per-category expense framing) are
   expense-shaped ("leftover = limit − spent"). Income/savings categories have no comparable
   "spent against a limit" semantics in this codebase (`category_response_with_carry` already
   zeroes `carried_amount`/`prev_period_spent` for non-expense categories) — extending funds
   there would require inventing new semantics out of scope for this ticket. This exactly mirrors
   how per-category rollover's `carried` is already computed only for `category_type ==
   "expense"` in `category_response_with_carry`.

4. **Precedence when both `is_fund` and `rollover_enabled` are on: `is_fund` wins; the plain
   #49 per-category carry is suppressed for a fund category to avoid double-counting.**
   Concretely: `category_carried` (the #49 pure helper) is *not* changed — for a fund category,
   callers on the read path use `fund_effective_limit` instead of `category_limit +
   category_carried(...)` when computing the category's effective limit, i.e. the fund balance
   *replaces* the one-period rollover contribution rather than stacking with it.
   `category_response_with_carry`'s `carried_amount` field, for a fund category, reports the
   fund balance (not the #49 one-period carry) so REST/RAG expose one coherent number. This
   avoids a category accruing credit twice (once via #49's next-read carry, once via the fund's
   materialized balance) for the same underspend.

5. **Idempotency-marker column name and shape: `fund_advanced_through TIMESTAMPTZ` (nullable —
   NULL means "never advanced / not currently a fund"), set to the current period start the
   moment `is_fund` flips to TRUE** (on create or update), mirroring `budgets.next_renewal_at`'s
   pattern (#51) but inverted in direction: `next_renewal_at` marks a *future* due time; the fund
   marker instead marks the boundary *up to which* `fund_balance` already reflects completed
   periods (the ticket's own phrasing). The background job advances it forward one period-boundary
   increment at a time (see #6 below) rather than jumping straight to the next future boundary —
   this is the key structural difference from `renew_due_budgets`, driven by the requirement to
   apply one `+= (period_limit - period_spent)` increment per crossed period.

6. **Background job: new sibling function `advance_fund_categories`, called from the same
   hourly ticker as `renew_due_budgets` in `main.rs`, immediately after it.** Per the ticket's
   explicit recommendation (option (a) over widening `renew_due_budgets`). It operates per
   *category* (not per budget), because `fund_advanced_through` lives on `categories`, and
   because a budget can have some fund categories and some non-fund categories with independent
   accrual timing is not a concern here — but the query still must join `budgets` to apply the
   `budget_type != 'project'` and `closed_at IS NULL AND archived_at IS NULL` exclusions and to
   read `time_frame` for period-boundary math.

7. **Catch-up after downtime: loop one period increment at a time, bounded.** The job repeatedly
   computes the next period boundary after `fund_advanced_through` via the existing
   `next_period_boundary(time_frame, fund_advanced_through)` helper; while that boundary is `<=
   now()`, it (a) computes that completed period's window via
   `(fund_advanced_through, boundary)`, (b) fetches spend in that window via
   `period_category_expense_spent_many`-shaped logic scoped to the single category, (c) applies
   `fund_balance += category_limit_at_the_time − period_spent`, and (d) advances
   `fund_advanced_through = boundary`, then repeats. **Limit-change fidelity**: the job uses the
   category's *current* `category_limit` for every crossed-period increment (there is no
   historical limit table in this schema — `categories.category_limit` is the only source of
   truth for "the limit," and the ticket's "then-current limit" language, read together with
   design decision #2's framing that editing a limit affects "future accrual only," is
   interpreted as: at the moment the job *runs* and processes a given period, the limit in effect
   *at that time* (i.e., current `category_limit`) is what's used — there is no way to
   reconstruct a truly historical per-period limit without a new table, which is out of scope).
   This is documented as a known limitation in the new AGENTS.md section (see Architecture §7
   below): a limit edited *between* a fund's period boundary and the next hourly tick will be
   applied to that catch-up increment using the *new* limit, not the limit that was nominally in
   effect during that period. Since the job runs hourly and periods are typically monthly, this
   window is small in practice; a per-period historical-limit ledger is flagged as a natural
   follow-up but is out of scope for #228. A bound of 24 iterations per category per tick
   (matching "the job runs hourly; even a full year of missed monthly periods is 12" with margin)
   guards against a pathological infinite loop from a misconfigured `time_frame`.

8. **Enabling `is_fund` via REST/chat validates `category_type == 'expense'`** (assumption #3)
   and otherwise behaves as design decision #2 states: `fund_balance` reset to 0,
   `fund_advanced_through` set to the current period's start (via `current_period_window`'s
   first element), on the transition FALSE→TRUE. A create-as-fund (`CREATE_CATEGORY` /
   `is_fund` param) behaves identically (fresh category, so "current period start" is simply
   "now, floored to period start").

9. **Migration file name**: `backend/migrations/20260703000000_fund_categories.sql`, following
   the existing `YYYYMMDDHHmmss_description.sql` convention and dated after the most recent
   migration (`20260630120000_subscriptions.sql`) using today's date (2026-07-03) per repo
   convention (each ticket's migration is dated to when it was authored, not backdated to
   ticket-filing time — confirmed by comparing e.g. `20260626000000_github_issue_filings.sql`
   against issue #192's filing history).

10. **Negative-limit rendering**: reuses the existing `money()` helper unchanged (it already
    signs negative values, per the ticket's own citation) for `fund_balance` and the effective
    limit; "Remaining" for a fund category is `effective_limit - spent` (can be more negative
    than a non-fund category's remaining) and renders with the same `money()` helper — no new
    formatting primitive needed. Deficit *styling* (e.g. a red/warn CSS class in the table HTML)
    is scoped narrowly to a conditional CSS class change in `build_categories_table_html`'s
    per-row HTML — no new component.

11. **Totals row**: sums `effective_limit` (not `category_limit`) for fund categories in the
    existing totals row, so the totals row's "Limit" column reflects the true effective budget
    picture (a fund category's negative balance genuinely reduces available headroom this
    period). This is the natural generalization of the existing totals-row sum, which already
    sums `category_limit` per row — for fund rows the effective limit *is* "the limit that
    applies this period."

## 3. Goal & Success Criteria

Add an independent, orthogonal "fund" concept to expense categories: an opt-in
`categories.is_fund` flag with a materialized, hourly-advanced `fund_balance` that accumulates
`limit − spent` every period (can go negative), surfaced through the category table, JSON API,
RAG context, and a new chat action, with full test coverage per the ticket's acceptance criteria.

Measurable success criteria (mapped from the ticket's Acceptance Criteria section):
- Migration adds `is_fund` (default FALSE), `fund_balance` (default 0), and
  `fund_advanced_through` (nullable marker); `cargo sqlx migrate run` (auto-run at server start)
  applies cleanly against the existing dev DB with zero behavior change for non-fund categories.
- A new `budget::advance_fund_categories` job runs on the existing hourly ticker in `main.rs`,
  independent of `auto_renew`, excludes `project`/closed/archived budgets, and is idempotent
  within a period (two `--ignored` DB tests prove this, mirroring `renew_due_budgets`'s test
  pair).
- `budget::fund_effective_limit(is_fund, category_limit, fund_balance) -> f64` is a pure,
  unit-tested helper, callable with no floor (verified negative-output test case).
- `CategoryTableRow`/`category_table_rows`/`build_categories_table_html` show fund status,
  balance, and effective limit; Remaining is computed against the effective limit; totals row
  sums signed effective limits; negative values render with the `-$` sign via `money()`.
- `CategoryResponse` and the RAG per-category context text both expose `is_fund`/`fund_balance`
  (and, per Assumption 4, `carried_amount` reports the fund balance for fund categories instead
  of the one-period #49 carry).
- `SET_CATEGORY_FUND` chat action (named-category or all-expense-categories), `CREATE_CATEGORY`
  accepts `is_fund`, both wired into the action enum/doc-comment/permission map, plus a new LLM
  prompt rule distinguishing funds from #49 rollover.
- Pure-helper unit tests reproduce the ticket's Jan–Apr worked example exactly (accumulation,
  sign flip, deficit).
- `#[ignore]`-marked DB tests cover: job idempotency-within-a-period, multi-period catch-up,
  project/closed/archived exclusion, and the `SET_CATEGORY_FUND` chat write.
- `AGENTS.md` gains a "Fund Categories (#228)" §12 documenting the materialized-balance
  departure, mirroring the style of §4/§7.

## 4. Scope

**In scope:** everything enumerated in the ticket's Proposed Design + Acceptance Criteria
sections (migration, pure helper, background job, REST/JSON surfacing, RAG context, chat action,
category table HTML, frontend display, tests, AGENTS.md doc).

**Out of scope** (verbatim from ticket, restated):
- Cross-category fund transfers or moving a balance between categories.
- Changing #47/#49 per-budget/per-category rollover semantics (the `category_carried` pure
  helper itself is untouched; only its *use* on the read path is superseded for fund categories
  per Assumption 4).
- Fund behavior on project budgets (excluded, like #47/#49/#51).
- Any budget-switcher UI (chat-driven only, per existing convention).
- A historical per-period limit ledger (flagged as a known limitation / follow-up, Assumption 7).
- Retroactive recompute of `fund_balance` on a limit edit (Assumption 1 — explicitly locked by
  the ticket).

## 5. Architecture

### 5.1 Data model (migration `20260703000000_fund_categories.sql`)

```sql
ALTER TABLE categories ADD COLUMN is_fund BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE categories ADD COLUMN fund_balance DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE categories ADD COLUMN fund_advanced_through TIMESTAMPTZ;

ALTER TABLE categories ADD CONSTRAINT categories_is_fund_expense_only_check
    CHECK (NOT is_fund OR category_type = 'expense');

CREATE INDEX categories_fund_due_idx
    ON categories (fund_advanced_through)
    WHERE is_fund;
```
The CHECK constraint makes Assumption 3 (expense-only) a data-layer invariant, mirroring
`budgets_auto_renew_only_time_based_check`'s pattern exactly. The partial index mirrors
`budgets_auto_renew_due_idx`. `Category` struct (`db.rs`) gains three fields
(`is_fund: bool`, `fund_balance: f64`, `fund_advanced_through: Option<DateTime<Utc>>`).

### 5.2 Pure helper — `budget::fund_effective_limit`

```rust
/// Effective limit for a fund category (#228): base limit plus the materialized,
/// cumulative, bidirectional running balance. No floor — a sustained overspend can
/// leave a fund category with a genuine deficit that must be repaid from future
/// periods. Non-fund categories are unaffected: this returns `category_limit` unchanged.
pub fn fund_effective_limit(is_fund: bool, category_limit: f64, fund_balance: f64) -> f64 {
    if is_fund { category_limit + fund_balance } else { category_limit }
}
```
Placed immediately after the `category_carried`/rollover pure-helper test cluster
(`budget.rs:4546-4635`, per the research report), i.e. new fund pure-helper tests slot in right
after `per_category_equals_budget_when_all_underspent` and before `prev_window_*` at line 4638.

### 5.3 Background job — `budget::advance_fund_categories`

Signature: `pub async fn advance_fund_categories(pool: &PgPool) -> Result<u64, sqlx::Error>`
(same `Result<u64, sqlx::Error>` shape as `renew_due_budgets`, per AGENTS.md §7's contract).

Algorithm:
1. `SELECT c.id, c.budget_id, c.category_limit, c.fund_balance, c.fund_advanced_through,
   b.time_frame FROM categories c JOIN budgets b ON b.id = c.budget_id WHERE c.is_fund = TRUE
   AND c.fund_advanced_through IS NOT NULL AND b.budget_type = 'time_based' AND b.closed_at IS
   NULL AND b.archived_at IS NULL` — mirrors `renew_due_budgets`'s WHERE-clause style
   (two-clause `closed_at`/`archived_at` form) plus the project-budget exclusion via
   `budget_type = 'time_based'`. Project budgets have no periods, so funds are excluded by this
   filter alone, matching #48's precedent — there is no CHECK forbidding `is_fund` on a
   project-budget category (project-ness is a budget-level, not category-level, property), so
   this is intentionally a query-level exclusion, not a constraint-level one, exactly like
   `renew_due_budgets`'s existing `budget_type = 'time_based'` clause.
2. For each selected category, loop (bounded to 24 iterations, Assumption 7):
   a. `boundary = next_period_boundary(time_frame, fund_advanced_through)`
   b. if `boundary > now()`, break (nothing more to advance this tick)
   c. `spent = period_category_expense_spent_many`-equivalent single-category spend over
      `[fund_advanced_through, boundary)` (reuse the existing batched helper with a
      single-element window, or call a lighter single-category variant — implementer's choice,
      documented in the plan)
   d. `delta = category_limit.unwrap_or(0.0) - spent`
   e. Guarded UPDATE: `UPDATE categories SET fund_balance = fund_balance + $delta,
      fund_advanced_through = $boundary WHERE id = $1 AND fund_advanced_through = $old_marker`
      — the `WHERE ... AND fund_advanced_through = $old_marker` guard is the idempotency/race
      guard (mirrors `renew_due_budgets`'s guarded UPDATE using the due predicate); if
      `rows_affected() == 0` (a concurrent tick already advanced this category), stop the loop
      for this category.
   f. `fund_advanced_through = boundary` locally, loop.
3. Count categories with at least one successful advance in `advanced` (the `u64` return),
   write an audit row (`AI_ADVANCE_FUND_CATEGORY` or a plain `ADVANCE_FUND_CATEGORY` — following
   the `renew_due_budgets` precedent of a system-driven, non-`AI_`-prefixed action since it's a
   background job not an assistant action; `budget::log_audit` per category advanced, using the
   *budget's owner* as `user_id`, matching `renew_due_budgets`'s existing pattern of auditing to
   the budget owner for a system-driven mutation).

`main.rs`: add a `match budget::advance_fund_categories(&cleanup_pool).await { Ok(n) if n > 0 =>
info!(...), Ok(_) => {}, Err(e) => warn!(...) }` block immediately after the existing
`renew_due_budgets` block (after line 149, before `github::purge_old_issue_filings`), matching
the three-arm log pattern exactly.

**Tests** (mirroring `renew_due_budgets`'s test pair and helper fns at `budget.rs:5210-5291`):
- `advance_fund_categories_is_idempotent_within_a_period` — re-running the tick twice within
  one period advances the balance exactly once.
- `advance_fund_categories_multi_period_catchup` — a category whose marker is 3 months stale
  (simulated downtime) advances through 3 separate increments, each using the category's
  current limit, landing on the correct cumulative balance (this directly exercises the Jan→Apr
  worked example as a catch-up scenario, giving a second angle of coverage beyond the pure
  helper's single-shot check).
- `advance_fund_categories_excludes_project_closed_archived` — three categories, each `is_fund
  = TRUE`, on a project / closed / archived budget respectively, none advance.
- New `#[cfg(test)]` helpers analogous to `seed_budget`/`marker_of`/`cleanup`: `seed_fund_category`
  (budget_id, limit, fund_balance, fund_advanced_through) and reuse `renew_test_setup`/`cleanup`
  where the shape matches (extending `cleanup` if categories need explicit deletion beyond the
  budget cascade — `categories.budget_id` is `ON DELETE CASCADE` per the init migration, so
  deleting the seeded budget already cleans up its categories; confirm no separate delete
  needed).

### 5.4 Read path — table, JSON, RAG

**`CategoryTableRow`** gains three fields: `is_fund: bool`, `fund_balance: f64`,
`effective_limit: Option<f64>` (`None` when there's no limit to be a fund of, matching
`category_limit: Option<f64>`'s existing nullability — a fund category's effective limit is only
meaningful when `category_limit.is_some()`; in practice `is_fund` implies a limit was set at
creation but the struct should stay defensive). `category_table_rows`'s SQL SELECT gains
`c.is_fund, c.fund_balance` alongside the existing columns; the row-building step computes
`effective_limit = category_limit.map(|l| fund_effective_limit(is_fund, l, fund_balance))`.

**`build_categories_table_html`** (via `push_category_row`, `budget.rs:3809-3824`): for a fund
row, render an additional "Fund" indicator (e.g. a small badge/icon in the Category cell,
matching the existing convention of inline badges — check current HTML structure in
`push_category_row` for the closest existing precedent, e.g. how `linked_budget_id` categories
are annotated, if at all) plus a "Balance: $X" sub-line, and compute **Remaining** as
`effective_limit - spent` instead of `category_limit - spent` for fund rows. A negative
effective limit or negative remaining gets a distinct CSS class (e.g. `text-error` /
`text-warning`, matching whatever daisyUI utility classes the existing table already uses for
negative/warning states — implementer greps the existing file for the closest precedent, e.g.
how overspend is currently flagged if at all). **Totals row**: sum `effective_limit` (falling
back to `category_limit` for non-fund rows, which are numerically identical) per Assumption 11.

**`CategoryResponse`** gains `is_fund: bool` and `fund_balance: f64`. Per Assumption 4,
`category_response_with_carry` computes, for a fund category: `carried_amount = fund_balance`,
`effective_amount = fund_effective_limit(true, base, fund_balance)` (bypassing the #49
`category_carried` call entirely for fund categories); non-fund categories are byte-for-byte
unchanged (still call `category_carried` as today).

**RAG per-category context** (`rag.rs:1013-1043`): extend the per-category line-building logic
with a fund-aware branch parallel to the existing `rollover_str` branch — when
`cat_is_fund && cat_type == "expense" && cat_linked.is_none()`, render
`" | Fund: on | Balance: ${:.2} | Effective limit: ${:.2}"` (using `fund_balance` and
`fund_effective_limit`) INSTEAD OF the existing `Rollover: .../Carried: $...` suffix (since,
per Assumption 4, funds supersede rollover display for that category — showing both would
double-narrate the same accumulated credit under two different labels). Non-fund categories'
`rollover_str` branch is unchanged.

### 5.5 Chat action — `SET_CATEGORY_FUND`

Following the extracted-helper convention identified in the research report (needed for direct
DB testability, matching `chat_create_categories`'s pattern rather than
`SET_CATEGORY_ROLLOVER`'s inline pattern):

```rust
async fn chat_set_category_fund(
    state: &AppState,
    user_id: Uuid,
    budget_id: Option<Uuid>,
    action_params: Option<&AiActionParams>,
) -> (Option<String>, Option<String>) { ... }
```

Mirrors `SET_CATEGORY_ROLLOVER`'s two-branch shape (named category vs. all expense categories)
but:
- Scoped to `category_type = 'expense'` (matching Assumption 3's constraint — a request to fund
  a non-expense category, or a mismatch when name resolution finds a non-expense category,
  returns a clear chat error rather than a silent no-op, matching the CHECK constraint's
  intent).
- On enabling (`flag = true`) for a category not already a fund: reset `fund_balance = 0`,
  `fund_advanced_through = current period start` (Assumption 8).
- On disabling (`flag = false`): `is_fund = FALSE` only — `fund_balance` and
  `fund_advanced_through` untouched (Assumption 2 — non-destructive; the value is simply inert
  and ignored on the read path while `is_fund = false`, and the background job's WHERE excludes
  it from further advancement).
- Logs `AI_SET_CATEGORY_FUND` via `log_audit`.

Registered:
- Action enum (`rag.rs:1357`): add `"SET_CATEGORY_FUND"`.
- `action_params` JSON schema (`rag.rs:1358-1391`) and `AiActionParams` struct
  (`rag.rs:244-286`): add an `is_fund: Option<bool>` field (reusing the existing
  `category_name`/`category_rollover`-shaped param naming convention — likely reusing
  `category_name` for targeting and adding `is_fund` as the new flag field, parallel to how
  `category_rollover: Option<bool>` already works for `SET_CATEGORY_ROLLOVER`).
- Permission map (`rag.rs:551`): add `"SET_CATEGORY_FUND"` to the existing `Permission::Edit`
  arm alongside `"SET_CATEGORY_ROLLOVER" | "UPDATE_CATEGORY"`.
- Action doc-comment (`rag.rs:238`): append `"SET_CATEGORY_FUND"` to the enumeration (noting
  it's already a partial/non-exhaustive list per the research report — append for
  completeness/consistency with how other actions are listed there, without over-claiming
  exhaustiveness).
- `match action` dispatch (near the existing `SET_CATEGORY_ROLLOVER`/`UPDATE_CATEGORY` arms,
  `rag.rs:2120+`): add a `"SET_CATEGORY_FUND" => { let (log, err) =
  chat_set_category_fund(&state, user_id, active_budget_id,
  parsed_ai_res.action_params.as_ref()).await; mutation_log = log; mutation_error = err; }` arm,
  matching `CREATE_CATEGORY`'s thin-dispatcher shape exactly.

**`CREATE_CATEGORY` gains `is_fund`**: `AiCategorySpec` (used by `chat_create_categories`) gains
an `is_fund: Option<bool>` field; `chat_create_categories`'s per-spec insert additionally sets
`is_fund` (defaulting to `false`) via an extended INSERT (the current
`CATEGORY_LIMIT_UPSERT_QUERY` const needs a 6th bound param, or a fund-aware variant/second
UPDATE step — implementer's choice, documented in the plan) and, when `is_fund = true` AND
`category_type` resolves to `'expense'`, additionally sets `fund_balance = 0` and
`fund_advanced_through = current period start` in the same statement (or a follow-up guarded
UPDATE, matching the `SET_CATEGORY_FUND`-enable code path so there is one shared "enable fund"
helper, not two divergent implementations — recommend factoring a private
`fn build_fund_enable_fields(now, time_frame) -> (f64, DateTime<Utc>)` or similar tiny helper
used by both call sites to avoid drift). A `CREATE_CATEGORY` request for `is_fund: true` on a
non-expense `category_type` silently sets `is_fund = false` and proceeds (mirroring how
`resolve_category_type` already silently coerces LLM-supplied junk types to `"expense"` — but
in this case category_type is user-directed/explicit, so a clear case: if the LLM explicitly
requests `category_type: "income"` AND `is_fund: true` together, resolve `category_type` first
per existing logic, then apply Assumption 3's expense-only gate to `is_fund`, silently dropping
`is_fund` to `false` rather than erroring — consistent with the existing "coerce, don't reject"
tone of category creation, whereas `SET_CATEGORY_FUND` — an explicit, single-purpose action
distinct from a compound create — DOES surface a clear chat error for the same mismatch, since
there the user's sole intent is "make this a fund" and silently no-op'ing would be confusing).

### 5.6 Prompt rule

New rule, placed as **2m** immediately after 2l (AMOUNT MODE, `rag.rs:1407`) and before rule 3
(`rag.rs:1408`), following the exact prose shape of rule 2f (CATEGORY ROLLOVER):

> **2m. FUND CATEGORIES (#228)**: A "fund" (envelope / sinking-fund) category accumulates a
> running balance across ALL periods — not just one. Each period, its unused amount (limit −
> spent) is ADDED to the balance; overspending SUBTRACTS from it, and the balance can go
> negative after sustained overspend. This is DIFFERENT from category rollover (rule 2f), which
> only looks at the single previous period and never goes negative. If the user says "make
> groceries a fund", "turn on a running balance for X", "let unused amount build up over time",
> or "let overspending carry a deficit forward", use SET_CATEGORY_FUND with `category_name` (or
> omit for all expense categories) and `is_fund: true`/`false`. Funds only apply to expense
> categories. Never invent the fund balance or effective limit yourself — always read the exact
> figures from the CATEGORIES context (`Fund: on | Balance: $X | Effective limit: $Y`).

### 5.7 Frontend

`CategoriesView.svelte` needs NO code change beyond what the backend HTML injection already
provides (per the ticket: "the backend table changes above are what make it appear" — the file
`{@html}`-injects `build_categories_table_html`'s output verbatim). This spec's frontend work is
entirely inside `build_categories_table_html` (§5.4). Verify post-implementation by fetching
`GET /budgets/:id/categories-table` and confirming the returned HTML contains the new fund
markup — no `.svelte` file edit is anticipated. (If review finds the existing markup needs a
CSS class not already defined globally, add a minimal inline `style` attribute or reuse an
existing daisyUI utility class already present in the file — no new stylesheet.)

### 5.8 AGENTS.md

New `### 12. Fund Categories (#228)` section, following the exact prose/structure convention of
§4 (Per-Budget Rollover) and §7 (Recurring Budgets): toggle description, the materialized-vs-
computed-on-read departure (explicitly cross-referencing §4's "computed on read, no scheduler"
principle and stating why funds break it), the background job's schedule/idempotency contract,
exclusions, and surfacing bullets (REST/RAG/chat/frontend), matching the terse, precise,
citation-heavy style of every existing numbered section.

## 6. Error Handling & Edge Cases

- **Category deleted while a fund**: `fund_balance` dies with the row (no FK to preserve,
  ticket confirms out of scope for cross-category transfer).
- **Budget archived/closed while a fund category exists**: the job's WHERE excludes it from
  further advancement (balance freezes, matching #47/#49/#51 precedent) — no explicit code
  needed beyond the WHERE clause itself.
- **`category_limit` is NULL on a fund category**: `fund_effective_limit` treats `category_limit`
  as `f64` (not `Option`), so callers must resolve `category_limit.unwrap_or(0.0)` before calling
  it — same convention as `category_carried`'s existing `base: f64` parameter. A fund category
  with a NULL limit simply has `effective_limit = 0 + fund_balance`.
- **Time-frame changed on a budget with fund categories**: `next_period_boundary` is
  cadence-sensitive; changing `time_frame` mid-life changes future boundary math from that point
  forward (same as it already does for #47/#49/#51 — no special handling needed, consistent
  with existing precedent of not backfilling on a cadence change).
- **Job runs with zero eligible categories**: returns `Ok(0)`, logged as a no-op tick (matching
  `renew_due_budgets`'s `Ok(_) => {}` arm).
- **Concurrent hourly ticks (should not happen with a single `tokio::spawn` loop, but the guard
  exists defensively)**: guarded UPDATE with the marker-equality predicate prevents double-apply,
  matching `renew_due_budgets`'s exact idempotency mechanism.

## 7. Testing Approach

- **Unit (pure helpers, no DB)**: `fund_effective_limit` — positive accumulation, negative
  (deficit), zero-balance no-op for non-fund categories, the exact Jan–Apr worked-example
  sequence applied step-by-step as a chained assertion.
- **`#[ignore]` DB tests** (`cargo test -- --ignored`, against local pgvector per
  `DATABASE_URL`): `advance_fund_categories_is_idempotent_within_a_period`,
  `advance_fund_categories_multi_period_catchup`,
  `advance_fund_categories_excludes_project_closed_archived`, and a `SET_CATEGORY_FUND` chat
  write test calling `chat_set_category_fund` directly (mirroring the `chat_create_categories`
  test pattern with an inline `AppState`).
- **Existing suite regression**: `cargo test` (unignored) must stay green — no behavior change
  for `is_fund = false` categories (the overwhelming default case).
- Commands: `cd backend && cargo test` (fast/unignored), `cd backend && cargo test -- --ignored`
  (DB-backed, requires `podman-compose up -d` running pgvector locally).

## 8. Risks & Open Questions

- **Limit-change fidelity during multi-period catch-up** (Assumption 7): a limit edited between
  a missed period boundary and the next hourly tick is applied retroactively to that catch-up
  increment using the *new* limit rather than a true historical snapshot. Documented as a known
  limitation in AGENTS.md §12; a historical per-period limit ledger is flagged as a follow-up,
  out of scope for #228.
- **Whether `is_fund` on a non-expense category should 400 or silently coerce** differs between
  `CREATE_CATEGORY` (silently coerces, Assumption 3/§5.5) and `SET_CATEGORY_FUND` (explicit chat
  error) — this asymmetry is intentional (matching existing `resolve_category_type`'s
  "coerce on compound create, error on explicit single-purpose action" tone) but is called out
  here in case a reviewer prefers one uniform behavior.
- **REST field naming for `is_fund` on category create/update payloads**: the ticket's
  Acceptance Criteria mention `CREATE_CATEGORY accepts is_fund` only for the *chat* action; the
  ticket doesn't explicitly ask for a REST `CategoryPayload.is_fund` settable field (only
  `CategoryResponse.is_fund`/`fund_balance` as *read* fields). This spec treats REST-write as
  out of scope unless the plan reviewer flags it as required for API symmetry with existing
  fields like `rollover_enabled` (which IS REST-settable) — recommendation: add
  `CategoryPayload.is_fund: Option<bool>` for symmetry (low cost, matches `rollover_enabled`'s
  existing REST-settable precedent) even though the ticket's chat-first framing doesn't
  explicitly demand it. Flagged for the plan phase to decide with a one-line rationale either
  way.
