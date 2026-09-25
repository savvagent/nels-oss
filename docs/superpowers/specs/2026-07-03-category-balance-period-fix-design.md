# Spec: Single-category chat balance reports lifetime spend instead of current-period (nels#282)

## 1. Brief (verbatim from the ticket)

When a user asks in chat about a single category's balance — e.g. "Show me what remains in the
entertainment category" — the assistant reports the wrong spent figure, and therefore the wrong
remaining figure. It sums that category's spend across all time instead of the current budget
period. Listing the categories (via LIST_CATEGORIES) shows the correct, period-scoped numbers, so
the two views of the same category disagree.

Example (Entertainment, $250 limit): actual chat answer reports Spent $1,147.82 / over by $897.82
(lifetime sum); expected/matches-the-table: Spent $66 / Remaining $184 (current-period sum).

Root cause: (1) the chat-context CATEGORIES block is built from an un-windowed `cats` query in
`backend/src/rag.rs:853-867` that sums ALL transactions ever per category, and (2) there is no
dedicated action arm for a single-category balance question, so the LLM answers with `action:
"NONE"` and does `limit - spent` arithmetic itself off that (wrong) block, per system-prompt rule 5.

Acceptance criteria (verbatim):
- Asking "what remains in the entertainment category" returns the current-period spent/remaining,
  matching the LIST_CATEGORIES table exactly (e.g. $66 spent / $184 remaining, not $1,147.82).
- The chat CATEGORIES prompt block and the LIST_CATEGORIES table are sourced from one period-aware
  query; there is no remaining un-windowed per-category spend query in rag.rs.
- Income, savings, fund, and rollover figures in the CATEGORIES block are unchanged by the fix (no
  regression to rollover/fund math).
- A new CATEGORY_BALANCE action arm answers single-category balance questions deterministically (no
  LLM arithmetic), including a friendly response for an unknown category name and graceful handling
  of no active budget.
- Regression test: a category with an in-window txn and an out-of-window txn reports only the
  in-window spend through both the chat context path and the CATEGORY_BALANCE arm.
- Zero-spend-this-period category still appears with spent = 0 (LEFT JOIN preserved).

## 2. Assumptions

1. **Single source of truth = `budget::category_table_rows`, extended.** Rather than adding a
   parallel query, extend `CategoryTableRow`/`category_table_rows` (backend/src/budget.rs:3941-3999)
   with the three fields the rag.rs CATEGORIES-block loop needs that it doesn't have today: `id:
   Uuid`, `rollover_enabled: bool`, `linked_budget_id: Option<Uuid>`. This makes it literally the one
   query LIST_CATEGORIES, the chat CATEGORIES context, and CATEGORY_BALANCE all read from — the
   strongest form of "cannot drift again" the ticket asks for. Rationale: `CategoryTableRow` already
   carries `category_type`/`category_limit`/`is_fund`/`fund_balance`, which is 4 of 7 needed fields;
   adding 3 more is additive (no existing field removed/retyped), so any code constructing/reading
   the struct by field access keeps compiling — a destructuring construct that enumerates all fields
   would fail to compile and surface itself immediately (a beneficial forcing function, not a risk).
2. **Reuse `category_table_rows` as-is for CATEGORY_BALANCE's figure lookup — no separate windowed
   query for the single category.** Call it once, then find the matching row by
   case-insensitive name comparison in Rust (`r.name.to_lowercase() ==
   requested.trim().to_lowercase()`). This avoids a second SQL round trip and guarantees the number
   shown for one category via CATEGORY_BALANCE is drawn from the exact same result set LIST_CATEGORIES
   would render. Accepted minor inefficiency: this fetches all categories to answer about one; the
   existing `category_table_rows` call in the CATEGORIES-context path already does this once per chat
   turn, so a second call in the CATEGORY_BALANCE arm is a second round trip only when that action
   fires (not on every turn) — acceptable, not a hot path.
3. **`category_table_rows` keeps its own internal `SELECT time_frame FROM budgets` lookup** even
   though `chat_endpoint` already has the budget's `time_frame` in scope (as `b_time`) — a
   micro-inefficiency (one extra trivial query per chat turn once the CATEGORIES-context path switches
   to calling this function) accepted in favor of not adding a second function signature/overload just
   to shave one query. Out of scope: performance tuning of this path.
4. **CATEGORY_BALANCE reuses `build_categories_table_html` on a one-row slice**, exactly as the ticket
   suggests, for ALL category types (expense/income/savings, fund or not) — not scoped to
   expense-only. `build_categories_table_html`/`push_category_row` already render income/savings rows
   sensibly (limit/remaining show "—" when `category_limit` is `None`, common for those types) and
   already special-case funds (effective limit = limit + fund_balance, deficit styling). This is the
   "answer sensibly for these too" branch of the ticket's open decision, not the "route to prose"
   branch — chosen because reuse is simpler and the renderer already handles every type uniformly. A
   single-row totals footer will duplicate that row's own figures (harmless, matches what the ticket's
   own suggested implementation approach implies).
5. **CATEGORY_BALANCE renders via the SAME `categories_table_html` field on `ChatResponse`** that
   LIST_CATEGORIES already populates (backend/src/rag.rs:121), rather than inventing a new response
   field — the frontend already knows how to render this field, and the ticket explicitly says "reuse
   build_categories_table_html." A resolved-category answer sets `categories_table_html`; it does NOT
   touch `response_text` (mirrors LIST_CATEGORIES's convention: the model's own transition sentence
   stays, the table carries the numbers).
6. **Unknown/missing category name is a NEW addendum channel, `category_balance_md: Option<String>`,
   appended to `final_response_text`** — mirroring the existing `transactions_list_md` convention
   (declared alongside it near rag.rs:1979, appended in the same block as `transactions_list_md` near
   rag.rs:3540-3541). Reuse the existing `category_not_found_message(&cat_name)` helper
   (backend/src/rag.rs:3654, already generic — "I couldn't find a category named '{}' in the active
   budget.") verbatim for the not-found case; a new short "Which category would you like the balance
   for?" string for the missing-name case.
7. **DB read failure routes through `mutation_error`**, per the ticket's explicit instruction, mirroring
   LIST_CATEGORIES's (not LIST_TRANSACTIONS's) convention — `tracing::warn!` + `mutation_error =
   Some("I couldn't load that category's balance right now. Please try again.".to_string())`.
8. **No active budget → no-op**, exactly mirroring LIST_CATEGORIES's `if let Some(bid) =
   active_budget_id { ... }` with no `else` branch — the model's own `response_text` is the graceful
   empty-state answer, consistent with the ticket's "same graceful empty-state as LIST_CATEGORIES."
9. **Extract two small, directly-testable helpers in rag.rs** so the fix is unit/integration testable
   without needing a live/mocked Gemini call or a correct offline-router keyword match:
   - `render_categories_context(cats: &[CategoryTableRow], archived_sources, linked_source_totals,
     cat_prev_map, b_type, b_rollover) -> (String, f64)` — a **pure, synchronous** function (no I/O)
     extracted from the exact CATEGORIES-block-building loop (no logic change beyond the windowing
     fix itself, which lives entirely in `category_table_rows` now being the `cats` source, not in
     this function). Deliberately split from a DB-touching fetch so the rollover/fund/limit rendering
     logic is unit-testable with hand-built `CategoryTableRow` values — fast, no Postgres required —
     while `category_table_rows` (already DB-tested by
     `category_table_rows_sums_current_period`) remains the sole place period-windowing correctness is
     verified against a real database. A separate DB-backed integration test still exercises the full
     `category_table_rows` → `render_categories_context` pipeline together, so the split point itself
     is also covered end-to-end.
   - `resolve_category_balance(pool, budget_id, category_name) -> Result<Option<CategoryTableRow>,
     sqlx::Error>` — calls `category_table_rows` and finds the matching row by case-insensitive name;
     used by the CATEGORY_BALANCE arm and directly testable (mirrors the existing extracted-helper
     convention of `chat_set_category_fund`).
   These are pure extractions/small helpers, not new business logic — the goal is testability without
   inventing a parallel code path, and they match this codebase's existing convention of factoring
   mutation/query logic out of the giant `chat_endpoint` match block into standalone functions.
10. **Offline-router support is added but scoped tight**, matching the ticket's own "broader NL
    phrasings... out of scope" boundary: a new `offline_category_balance_action(msg: &str) ->
    Option<String>` pure keyword matcher recognizing "remain"/"remaining", "left in"/"left for", and
    "balance in"/"balance for"/"balance of" phrasings, extracting the trailing category name via the
    SAME preposition-scan + article/"category"-suffix-strip logic `offline_list_transactions_category`
    already uses (backend/src/rag.rs:5136-5238) — refactored into a shared private helper
    `extract_trailing_category_name(msg: &str) -> Option<String>` to avoid duplicating ~60 lines of
    byte-safe scanning. Routed in the offline if/else chain immediately after the existing
    `offline_list_transactions_category` branch (so "spent"/"transaction"-bearing phrases keep going to
    LIST_TRANSACTIONS unchanged — no reordering of existing arms). Guards against misfiring on
    budget-level (not category-level) questions like "how much is left in my budget" by returning
    `None` when the extracted name case-insensitively equals "budget" (a cheap, targeted exclusion —
    not a general NLP solution, and if it doesn't cover every phrasing, that's within the ticket's own
    "broader phrasings out of scope" carve-out; the fallback is the pre-existing NONE/math LLM path,
    unchanged for those phrasings). This exists to (a) keep offline/dev-mode parity with every other
    read-only action in this codebase (all of which have an offline path) and (b) make the ONE
    additional end-to-end offline-mode-router test straightforward, but is NOT the primary regression
    test vehicle (assumption 9's extracted helpers are).
11. **The `chat_endpoint`-embedded `cats` local variable's usage is fully migrated, not just the render
    loop** — the implementer must `rg 'cats\b'` scoped to `chat_endpoint`'s body to confirm every use
    site (not only the render loop rag.rs:964-1082 identified by research) is updated to the
    `Vec<CategoryTableRow>` shape, since the un-windowed ad-hoc `PgRow` vector may be read in more than
    one place.
12. **JSON action schema + prompt rule additions are additive only** — append `"CATEGORY_BALANCE"` to
    the action enum-of-strings (rag.rs:1395) and a new rule `20c. CATEGORY_BALANCE: ...` immediately
    after rule `20b` (rag.rs:1466), before the two format-placeholder lines and the final rule `22`.
    `category_name` already exists in the schema (rag.rs:1405) — only its doc comment gains a mention
    that CATEGORY_BALANCE also uses it.
13. **`required_perm_for_action`'s default (`_ => None`) already covers CATEGORY_BALANCE** — it's
    read-only, so no permission-table change is needed (matches LIST_CATEGORIES/LIST_TRANSACTIONS,
    both currently uncatalogued there too).

## 3. Goal & Success Criteria

**Goal:** A user's single-category balance question in chat — whether routed to the LLM's own
`action: "NONE"` math (still possible for other numeric questions) or the new deterministic
`CATEGORY_BALANCE` action — reflects the SAME current-period spend the LIST_CATEGORIES table shows,
because both are now sourced from one period-aware query.

Success criteria:
- [ ] `category_table_rows`/`CategoryTableRow` carry `id`, `rollover_enabled`, `linked_budget_id` in
  addition to the existing fields; `category_table_rows_sums_current_period` still passes unmodified
  (assertions on `spent`/`category_limit` untouched) plus a new assertion on at least one new field.
- [ ] rag.rs's chat-context CATEGORIES block (`render_categories_context`) is sourced from
  `category_table_rows`, not the old un-windowed `cats` SQL — that query and its ad-hoc `PgRow` usage
  are deleted entirely from rag.rs.
- [ ] A new `CATEGORY_BALANCE` action arm exists, resolves a category name case-insensitively within
  the active budget, and renders via `build_categories_table_html(&[row])` into
  `ChatResponse.categories_table_html` — no arithmetic performed by the LLM for this action.
- [ ] Unknown/missing category name → friendly clarifying prose via `category_balance_md`; no active
  budget → graceful no-op; DB failure → `mutation_error`.
- [ ] A DB-backed regression test seeds an in-window + out-of-window transaction for one category and
  asserts (a) `render_categories_context`'s rendered text shows only the in-window amount and (b)
  `resolve_category_balance`/the CATEGORY_BALANCE arm reports the same in-window amount — both paths,
  matching `category_table_rows_sums_current_period`'s existing DB-backed test shape.
- [ ] A REQUIRED (not optional) DB-backed end-to-end test drives the full `CATEGORY_BALANCE` arm
  through `chat_endpoint` itself (offline router recognizing a "what remains in X" message, per the
  `EnvGuard`/`chat_set_category_limit_is_case_insensitive_no_phantom_category` pattern), asserting
  `resp.categories_table_html` is `Some` and contains the expected in-period figures — this is the
  only test that exercises the actual match-arm wiring (action-name string, `category_balance_md`
  vs. `mutation_error` assignment, the slice passed to `build_categories_table_html`), which none of
  the direct-helper tests below cover.
- [ ] A DB-backed test asserts AC bullet 3 directly: seed a rollover-enabled category (and/or a fund
  category) alongside the in/out-of-window transactions, and assert `render_categories_context`'s
  rendered "Rollover: on | Carried: $X" / "Fund: on | Balance: $X | Effective limit: $Y" lines are
  byte-for-byte unchanged from what the pre-fix loop would have rendered for that same data (i.e. the
  windowing fix touches ONLY the "Spent/Earned" figure, never the rollover/fund annotations) — not
  just asserted-by-construction via "it's a pure extraction."
- [ ] A zero-spend-this-period category still appears in both the CATEGORIES context and a
  CATEGORY_BALANCE lookup with spent = 0.
- [ ] `cargo test` (offline suite) and `cargo test -- --ignored` (DB-backed suite, Postgres via
  `podman-compose up -d`) both pass; `cargo check`/`cargo clippy` clean on `backend/`.

## 4. Scope

**In scope:**
- Extending `CategoryTableRow`/`category_table_rows` with the three missing fields.
- Replacing the un-windowed `cats` query and its consuming loop in rag.rs with a call through
  `category_table_rows`, extracted into a small testable helper.
- Adding the `CATEGORY_BALANCE` action: JSON schema entry, prompt rule, match arm, resolution helper,
  offline-router matcher (scoped to "remaining/left/balance in X" phrasings), response-channel wiring
  (`categories_table_html` reuse + new `category_balance_md` addendum).
- Tests: extend/add unit tests for the new offline matcher, DB-backed tests for the extracted context
  helper and the CATEGORY_BALANCE resolution helper (mirroring `category_table_rows_sums_current_period`
  and `chat_set_category_fund_enables_and_initializes`'s test patterns), and a small addition to
  `category_table_rows_sums_current_period` or a sibling test asserting the new struct fields.
- A committed spec (this doc) and plan under `docs/superpowers/specs/` / `docs/superpowers/plans/`,
  matching this repo's established convention for feature work.

**Out of scope (per the ticket):**
- Changing how periods/`time_frame` windows are defined — `current_period_window` is reused as-is.
- Rollover / fund carry-over semantics beyond not regressing them.
- Broader NL phrasings for CATEGORY_BALANCE beyond "remaining / left / balance in X".
- Any REST-facing change (`GET /budgets/:id/categories-table` and its handler are unaffected beyond
  the additive struct fields flowing through harmlessly).
- Performance tuning of the extra `SELECT time_frame` round trip (assumption 3).
- nels#269's Gemini `generateContent`/`GEMINI_API_BASE` work — confirmed (via research) to touch
  unrelated line ranges (rag.rs:1500/5397/5678) with no logic overlap; only a line-number rebase is
  expected if it lands first.

## 5. Architecture

**Data layer (backend/src/budget.rs):**
- `CategoryTableRow` gains `id: Uuid`, `rollover_enabled: bool`, `linked_budget_id: Option<Uuid>`.
- `category_table_rows`'s SELECT/GROUP BY gains `c.id`, `c.rollover_enabled`, `c.linked_budget_id`;
  the `ON`-clause date-windowed LEFT JOIN and `ORDER BY name ASC` are unchanged.

**Chat context builder (backend/src/rag.rs, inside `chat_endpoint`):**
- New private helper `render_categories_context` extracts the existing loop (rag.rs:964-1082-ish)
  verbatim, replacing its input from the raw `cats: Vec<PgRow>` (from the deleted un-windowed query)
  to `Vec<CategoryTableRow>` fetched via `crate::budget::category_table_rows(&state.db, bid).await?`.
  Every field read via `.get::<T,_>("col")` becomes a direct struct field read
  (`row.id`/`row.name`/`row.category_type`/`row.category_limit`/`row.rollover_enabled`/
  `row.linked_budget_id`/`row.is_fund`/`row.fund_balance`/`row.spent`). The function's existing
  captures (`archived_sources`, `cat_prev_map`, `b_type`, `b_rollover`) stay as parameters; the loop
  body's logic (limit_str/rollover_str/fund rendering, "Spent/Earned" line, `b_limit` accumulation) is
  otherwise unchanged — only the data source and per-row field access syntax change.
- `chat_endpoint` calls this helper where the old query + loop lived, and propagates its `sqlx::Error`
  through the same error path the old query used (`.map_err(internal_error)?` or equivalent already in
  place at that call site).

**New action (backend/src/rag.rs):**
- JSON schema (rag.rs:1395): append `"CATEGORY_BALANCE"` to the action enum documentation string.
- Prompt rule `20c` inserted after `20b` (rag.rs:1466): describes trigger phrasings ("what remains in
  X", "how much is left in X", "category balance for X"), instructs the LLM to set `category_name` and
  give a brief transition in `response_text` with NO arithmetic (the table carries the numbers), and to
  use `NONE` if it cannot identify a category name to ask a clarifying question instead.
- New helper `resolve_category_balance(pool: &PgPool, budget_id: Uuid, category_name: &str) ->
  Result<Option<CategoryTableRow>, sqlx::Error>`: calls `category_table_rows(pool, budget_id)` and
  returns the first row whose `name` case-insensitively equals the trimmed input, else `None`.
- New match arm `"CATEGORY_BALANCE" => { ... }` (inserted after the existing `LIST_CATEGORIES` arm,
  rag.rs:3289-3324):
  ```
  if let Some(bid) = active_budget_id {
      let cat_name = parsed_ai_res.action_params.as_ref()
          .and_then(|p| p.category_name.clone())
          .filter(|cn| !cn.trim().is_empty());
      match cat_name {
          None => {
              category_balance_md = Some("Which category would you like the balance for?".to_string());
          }
          Some(cn) => match resolve_category_balance(&state.db, bid, cn.trim()).await {
              Ok(Some(row)) => {
                  categories_table_html = Some(crate::budget::build_categories_table_html(std::slice::from_ref(&row)));
              }
              Ok(None) => {
                  category_balance_md = Some(category_not_found_message(&cn));
              }
              Err(e) => {
                  tracing::warn!(error = ?e, budget_id = %bid, "CATEGORY_BALANCE: failed to resolve category balance");
                  mutation_error = Some("I couldn't load that category's balance right now. Please try again.".to_string());
              }
          }
      }
  }
  ```
- New local `category_balance_md: Option<String>` declared alongside `transactions_list_md`
  (rag.rs:1979) and appended into `final_response_text` in the same block as `transactions_list_md`
  (rag.rs:3540-3541), same `format!("{}\n\n{}", final_response_text, results)` pattern.
- Offline router: shared `extract_trailing_category_name(msg: &str) -> Option<String>` factored out of
  `offline_list_transactions_category`'s existing byte-scan logic; new `offline_category_balance_action`
  built on top of it with its own trigger-noun guard ("remain"/"left"/"balance") and the "budget" name
  exclusion (assumption 10); wired into the offline if/else chain (rag.rs:~1668, right after
  `offline_list_transactions_category`'s branch) setting `action = "CATEGORY_BALANCE"`,
  `action_params.category_name = Some(cat_name)`, and a `(Mock AI Offline Mode) ...` `response_text`.

## 6. Error Handling & Edge Cases

- **No active budget:** CATEGORY_BALANCE arm no-ops (mirrors LIST_CATEGORIES); model's own prose is
  the answer.
- **Missing `category_name` param:** clarifying prose via `category_balance_md`, no DB call.
- **Unknown/misspelled category name:** `resolve_category_balance` returns `Ok(None)` →
  `category_not_found_message`, no crash, no partial render.
- **DB failure during resolution:** `mutation_error`, logged via `tracing::warn!`.
- **Zero-spend-this-period category:** `category_table_rows`'s `LEFT JOIN ... ON` (not `WHERE`)
  windowing means it still appears with `spent = 0.0`; `resolve_category_balance` finds it by name
  like any other row.
- **Non-expense (income/savings) and fund categories:** rendered as-is via
  `build_categories_table_html`/`push_category_row`, which already special-case both (assumption 4).
- **Case-insensitive, unique-by-`(budget_id, name)` categories:** a single case-insensitive match is
  unambiguous per the schema's UNIQUE constraint; `resolve_category_balance` takes the first match
  (there can be at most one, modulo an already-existing DB-level case-variant edge case the
  `LIST_TRANSACTIONS` arm's `ORDER BY name LIMIT 1` already accepts as a pre-existing, out-of-scope
  possibility — this ticket does not change that invariant).
- **Period boundary:** `[start, end)` from `current_period_window`, unchanged — a transaction dated
  exactly at `end` belongs to the next period (existing convention, preserved).
- **Offline-router false positive on budget-level questions** (e.g. "how much is left in my budget"):
  guarded by the "budget" name exclusion (assumption 10); any other unhandled phrasing simply falls
  through to the existing `NONE`/math path unchanged — no regression versus current behavior for
  phrasings this ticket doesn't target.

## 7. Testing Approach

Commands: `cd backend && cargo test` (offline/deterministic, no DB) and `cd backend && cargo test --
--ignored` (DB-backed, requires `podman-compose up -d`; `DATABASE_URL` defaults per `AGENTS.md`).

- **`budget.rs`:** extend or add alongside `category_table_rows_sums_current_period`
  (budget.rs:4626-4705) — keep its existing assertions intact, add an assertion that the returned row
  now carries `id`/`rollover_enabled`/`linked_budget_id` correctly (e.g. `rollover_enabled == false` by
  default, `linked_budget_id.is_none()`).
- **`rag.rs`, context path:** a new `#[ignore]` DB-backed test seeds a budget/category/two
  transactions (one now(), one 60 days ago — mirroring the budget.rs test's seed shape) and calls
  `render_categories_context` directly, asserting the rendered string contains the in-window dollar
  figure and not the lifetime sum.
- **`rag.rs`, CATEGORY_BALANCE path:** a new `#[ignore]` DB-backed test seeds the same shape and calls
  `resolve_category_balance` directly, asserting `Ok(Some(row))` with `row.spent` equal to the
  in-window amount only; a second test asserts `Ok(None)` for a non-existent name; a THIRD test
  (REQUIRED, not optional) exercises the full arm end-to-end via `chat_endpoint` with the offline
  router recognizing a "what remains in X" message, asserting `resp.categories_table_html` is `Some`
  and contains the expected figures — using the `EnvGuard`-style `GEMINI_API_KEY`-removal pattern
  already used by `chat_set_category_limit_is_case_insensitive_no_phantom_category`. This third test
  is the only one that actually exercises the match-arm wiring itself (not just the helpers it calls)
  and is required precisely because it is the closest automated proxy to the ticket's literal AC
  ("asking 'what remains in the entertainment category' returns $66/$184 via chat").
- **`rag.rs`, rollover/fund non-regression (AC bullet 3):** extend the context-path DB-backed test (or
  add a sibling) to seed a rollover-enabled category and a fund category in the SAME budget as the
  in/out-of-window transactions, and assert `render_categories_context`'s rendered output still
  contains the correct "Rollover: on | Carried: $X" and "Fund: on | Balance: $X | Effective limit: $Y"
  lines for those categories — proving the windowing fix changed only the "Spent/Earned" figure.
- **Offline matcher unit tests (pure, no DB):** mirror
  `offline_list_transactions_category_extracts_name_and_ignores_mutations`'s shape — assert
  `offline_category_balance_action` extracts the right name for "what remains in Entertainment",
  "how much is left in the Food category", "balance for Utilities"; returns `None` for
  transaction/spend-bearing phrases (deferred to LIST_TRANSACTIONS) and for "how much is left in my
  budget" (the "budget" exclusion).
- **Zero-spend edge case:** covered implicitly by `category_table_rows_sums_current_period`'s existing
  pattern (any category with no txns this period already asserts `spent = 0` via `COALESCE`); add one
  explicit assertion in the new regression test for a THIRD category in the same budget with no
  transactions at all, confirming it still appears with `spent == 0.0` in both the context-builder
  output and a `resolve_category_balance` lookup.

## 8. Risks & Open Questions

- The rag.rs extraction (`render_categories_context`) touches a large, deeply-parameterized loop inside
  a 12,800-line file's biggest function — the implementer must be careful to carry every captured
  variable through as a parameter and not silently change behavior beyond the windowing fix. Mitigated
  by treating it as a pure extraction (copy, don't rewrite) plus the existing render-loop code being
  fully quoted in the research above.
- Whether any OTHER consumer of `CategoryTableRow` in the codebase does exhaustive field
  destructuring (which would fail to compile once fields are added) is unconfirmed beyond
  `build_categories_table_html`/`push_category_row` (both field-access, safe) — `cargo check` will
  surface this immediately if present; treat any resulting compile error as an expected, easily-fixed
  signal, not a blocker.
- The offline-router matcher's "budget" exclusion is a narrow, pragmatic guard, not a general
  disambiguation between "category balance" and "budget balance" questions — accepted per the ticket's
  explicit "broader NL phrasings... out of scope" boundary.
- nels#269 (in flight elsewhere, Gemini call-site/`GEMINI_API_BASE` work) may land in the shared
  `origin/main` before this PR merges, causing a rebase — confirmed no logic overlap by research; a
  standard `git rebase origin/main` (or merge, per this repo's convention — confirm via existing merged
  PR history if a preference exists) is expected to be conflict-free or trivially so.
