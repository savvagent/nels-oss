# Spec: Category-scoped transaction listing (LIST_TRANSACTIONS)

## Brief (verbatim from ticket savvagent/nels#229)

> Root cause: NOT a dropped-filter bug — a missing read path. No transaction-read
> query anywhere in the backend filters by category_id. Writes store the category
> correctly; only reads ignore it.
>
> Proposed fix: Add a deterministic category-scoped read — a new LIST_TRANSACTIONS
> action that resolves the user's named category to its `categories.id` scoped to
> the active budget, runs an exact/exhaustive filter (`budget_id = $1 AND
> category_id = $2`, no embedding ordering, no LIMIT), and routes "transactions
> in/for category X" phrasings to it instead of the unfiltered RECENT
> TRANSACTIONS context or the budget-only SEARCH_TRANSACTIONS semantic query.
>
> Acceptance criteria:
> - Asking "show the transactions for category X" returns only transactions with
>   that category_id in the active budget.
> - A transaction assigned to category A never appears when listing category B.
> - Categories with zero transactions return an explicit empty result, not the
>   month's unfiltered list.
> - An unknown/ambiguous category name yields a clear message, not a silent
>   fallback to all transactions.
> - The result is complete (all matching rows, date-ordered) — not a 5-row
>   semantic sample.
> - A regression test asserts a category-scoped read returns only that
>   category's rows.

## Assumptions

1. **No frontend changes.** The ticket's integration points list `backend/src/rag.rs`
   only. `LIST_CATEGORIES`'s HTML-table field (`categories_table_html`) is, as of
   the recent router-outlet change (#233), consumed by `frontend/src/App.svelte`
   only as a *navigation signal* to a dedicated `categories` view that self-fetches
   — reusing that pattern for transactions would require a new frontend route/view,
   which is out of the stated integration points and would edit
   `frontend/src/App.svelte`, the exact file a concurrent, unrelated job
   (frontend/src/App.svelte) is also editing. Instead, `LIST_TRANSACTIONS` mirrors
   `SEARCH_TRANSACTIONS`'s existing pattern: a markdown-formatted addendum
   (`transactions_list_md: Option<String>`) appended to `response_text`, requiring
   zero `ChatResponse`/frontend changes. This still satisfies every AC (complete,
   category-scoped, date-ordered, clear empty/unknown-category messaging) — the
   ticket's "mirror the LIST_CATEGORIES table rendering" is read as "build a
   deterministic, complete tabular/list representation using an existing render
   helper pattern," not as a literal requirement to reuse the HTML-table +
   frontend-navigation mechanism.
2. **Category resolution mirrors `DELETE_CATEGORY`'s pattern**: a single
   case-insensitive, budget-scoped lookup (`WHERE budget_id = $1 AND
   LOWER(name) = LOWER($2)`), find-only (no create-on-miss, unlike
   `EDIT_TRANSACTION`'s find-or-create — a *listing* action must never silently
   create a category). A miss produces the clear "no such category" message the
   AC requires, not a fallback to the unfiltered list.
3. **`LIST_TRANSACTIONS` is read-only**, so it does not go through
   `mutation_log`/`mutation_error` (which render as a `**System Update:**
   ✅/⚠️` banner meant for mutations). It uses its own
   `transactions_list_md: Option<String>` variable, appended plainly — exactly
   mirroring `search_results_md`'s existing precedent (including for its own
   "no active budget" / lookup-failure messages).
4. **An offline-router fallback is added**, matching the codebase's established
   convention (every other read/list action — `LIST_CATEGORIES`, `OPEN_INSIGHTS`,
   privacy actions — has one; `SEARCH_TRANSACTIONS` is the sole exception because
   it's inherently embedding-based and can't run offline). `LIST_TRANSACTIONS` is
   a deterministic filter with no embedding dependency, so it can run fully
   offline. A category name is extracted from phrases like "transactions in/for
   <category>" via a small pure helper (`offline_list_transactions_category`),
   mirroring `offline_category_rename`/`offline_set_category_limit`'s existing
   extraction-helper pattern. If no category name can be extracted, the offline
   router does not claim the message (falls through to other arms/NONE) rather
   than guessing.
5. **Result cap**: "complete" is interpreted per the ticket's explicit language
   ("all matching rows, date-ordered") — no `LIMIT`. A defensive hard cap is
   out of scope per the ticket's explicit "Out of scope: optional date-range
   filtering... follow-up" — an unbounded read is what's asked for.
6. **"Ambiguous" category names (AC wording)**: `categories` has a DB-level
   `CONSTRAINT unique_category_name_per_budget UNIQUE (budget_id, name)`
   (confirmed at `backend/migrations/20260609000000_init.sql:34`) — but it's a
   plain btree constraint on the literal `name` column, so it is
   **case-sensitive**: "Food" and "food" could in principle coexist as two
   distinct rows in the same budget (every `INSERT INTO categories` call site
   in `rag.rs`/`budget.rs` relies on this constraint firing a unique-violation
   DB error, confirmed via `db_err.is_unique_violation()` handling at
   `rag.rs:2162`, rather than pre-checking case-insensitively). A case-variant
   duplicate would make a bare `LOWER(name) = LOWER($2) LIMIT 1` resolution
   query's pick non-deterministic between the two rows, so this ticket's own
   resolution query adds `ORDER BY name` before `LIMIT 1` — deterministic,
   not a full fix (that would need a case-insensitive unique index). This is
   **not a new risk introduced by this ticket** — `DELETE_CATEGORY`, `EDIT_TRANSACTION`'s
   category resolve/create, and `SET_CATEGORY_ROLLOVER` all already resolve a
   category via the identical `LOWER(name) = LOWER($2)` pattern, so this
   ticket inherits an existing, codebase-wide ambiguity boundary rather than
   creating one; fixing it broadly (e.g. a case-insensitive unique index) is
   out of scope for this ticket. The AC's "ambiguous" is read as
   **"`category_name` omitted/blank"** (handled by the dedicated "which
   category would you like to see transactions for?" prompt) — the
   practically-reachable ambiguous case.
7. **Dispatch-arm message-branch test coverage**: the five dispatch-arm
   branches (no active budget / missing category_name / unknown category /
   empty category / DB error) are verified by code review + the DB-level test
   (which proves the *data* correctness the messages are built from: empty
   rows for a zero-transaction category, correct rows otherwise) + the
   offline-router unit tests (which cover the offline-path message text
   directly). No other action arm in this file (`LIST_CATEGORIES`,
   `DELETE_CATEGORY`, `SEARCH_TRANSACTIONS`) has arm-level HTTP-mocked tests
   either — `chat_endpoint` requires a live/mocked Gemini call that the
   codebase does not currently scaffold for any action, online-path arm-level
   testing is out of reach without adding that scaffolding for the whole file,
   which is out of this ticket's scope. This is a deliberate, existing-pattern
   scope boundary, not an oversight.
8. **Concurrency**: this ticket's regions in `backend/src/rag.rs` (action enum
   ~1226, prompt rules ~1291-1294, dispatch near `LIST_CATEGORIES`/
   `SEARCH_TRANSACTIONS` ~3057/3097, RECENT TRANSACTIONS context ~761-772/
   1103-1104) are disjoint from nels#231's regions (context query ~659-683,
   rule 11 ~1284, the `_ => {}` dispatch arm ~3168-3170) — confirmed by direct
   inspection of current `rag.rs`. A rebase may still be needed if #231 lands
   first (both tickets touch the same giant `format!()` prompt-instructions
   string and the same `match` block); if so, keep both tickets' hunks.

## Goal & Success Criteria

Add a deterministic, category-scoped transaction-listing chat action so that
asking "show transactions for category X" returns exactly — and only — that
category's transactions in the active budget, replacing the current behavior
where the request silently falls through to the unfiltered RECENT TRANSACTIONS
prompt context (budget-only, 15-row cap) or the budget-only semantic search.

- [ ] A new `LIST_TRANSACTIONS` action exists in the action enum + JSON schema
      the model is instructed to return.
- [ ] A new deterministic SQL query filters `WHERE t.budget_id = $1 AND
      t.category_id = $2`, ordered by `transaction_date DESC, created_at DESC`,
      no `LIMIT`, no embedding ordering.
- [ ] The dispatch arm resolves `category_name` to a category id scoped to the
      active budget (case-insensitive, find-only); on no match, produces a
      clear "no such category" message; on a match with zero transactions,
      produces an explicit "no transactions in this category" message (never
      the unfiltered list).
- [ ] A new prompt rule routes "transactions in/for category X" phrasings to
      `LIST_TRANSACTIONS`, and an explicit guardrail tells the model NOT to
      answer category-scoped transaction questions from the RECENT
      TRANSACTIONS context.
- [ ] An offline-router fallback recognizes the same phrasing pattern when no
      `GEMINI_API_KEY` is set.
- [ ] Regression tests: a DB-backed integration test (mirroring
      `transactions_semantic_search_orders_by_distance_and_scopes_to_budget`)
      proving the new query returns only the matching category's rows,
      excludes a same-named-vector-adjacent other category and another budget,
      and is complete/date-ordered; plus pure unit tests for the offline
      category-name extraction helper.

## Scope

**In scope**: `backend/src/rag.rs` — action enum, prompt instructions/rules,
a new SQL query const, the `LIST_TRANSACTIONS` dispatch arm, an offline-router
fallback + its extraction helper, and inline `#[cfg(test)]` tests (unit +
`--ignored` DB-backed).

**Out of scope** (per ticket): optional date-range filtering; any data
migration/backfill; reworking `SEARCH_TRANSACTIONS`; any frontend change.

## Architecture

- **Schema facts confirmed** (`backend/migrations/20260609000000_init.sql`):
  `transactions.created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()` exists (line
  46), so `ORDER BY t.transaction_date DESC, t.created_at DESC` is valid.
  Cascade chain confirmed: `budgets.owner_id → users.id ON DELETE CASCADE`,
  `categories.budget_id → budgets.id ON DELETE CASCADE`,
  `transactions.budget_id → budgets.id ON DELETE CASCADE` — a test's
  `DELETE FROM users WHERE id = $1` cleanup cascades through budgets →
  categories → transactions, matching the sibling
  `transactions_semantic_search_orders_by_distance_and_scopes_to_budget` test's
  existing cleanup pattern.

- **SQL** (new const, alongside `TRANSACTIONS_SEMANTIC_SEARCH_QUERY` /
  `TRANSACTION_LOOKUP_BY_EMBEDDING` near line ~460). Selects only the columns
  the dispatch arm actually consumes (description/amount/date) — no
  `categories` join, since the category name is already known from the
  resolution step, mirroring `TRANSACTIONS_SEMANTIC_SEARCH_QUERY`'s own
  select-only-what's-needed style rather than the ticket's illustrative
  `SELECT t.*`:
  ```sql
  SELECT t.description, t.amount, t.transaction_date
  FROM transactions t
  WHERE t.budget_id = $1 AND t.category_id = $2
  ORDER BY t.transaction_date DESC, t.created_at DESC
  ```
  Bind order: `$1 = budget_id`, `$2 = category_id`. No `LIMIT`, no embedding
  ordering — satisfies the ticket's explicit "complete" requirement.

- **Category resolution** (inline in the dispatch arm, mirroring
  `DELETE_CATEGORY`, with one addition — `ORDER BY name` before `LIMIT 1`,
  since the `(budget_id, name)` UNIQUE constraint is case-sensitive and so
  does not rule out a case-variant duplicate matching this case-insensitive
  lookup; the `ORDER BY` makes the pick deterministic rather than
  nondeterministic in that edge case, per Assumption 6):
  ```sql
  SELECT id, name FROM categories WHERE budget_id = $1 AND LOWER(name) = LOWER($2) ORDER BY name LIMIT 1
  ```

- **Dispatch arm** `"LIST_TRANSACTIONS" => { ... }` inserted directly after the
  existing `"SEARCH_TRANSACTIONS" => { ... }` arm (~line 3097-3157), before the
  `"EDIT_TRANSACTION"` arm. Logic:
  1. No `active_budget_id` → warn-log + `transactions_list_md = Some("I
     couldn't list your transactions right now.")` (mirrors
     `SEARCH_TRANSACTIONS`'s `(None, _)` arm).
  2. `active_budget_id` present but `category_name` missing/blank in
     `action_params` → `transactions_list_md = Some("Which category would you
     like to see transactions for?")`.
  3. Category name present, resolves to no row → `transactions_list_md =
     Some(category_not_found_message(&cn))` — a **pure, unit-tested** helper
     (`fn category_not_found_message(category_name: &str) -> String`), not an
     inline `format!` — so this AC-mandated message has coverage independent
     of a live DB/Gemini call.
  4. Category resolves → run the new query; the result (rows or empty) is
     rendered by a second **pure, unit-tested** helper —
     `fn format_transactions_list_message(category_name: &str, rows: &[(String,
     f64, chrono::DateTime<chrono::Utc>)]) -> String` — which returns the
     explicit "No transactions found in the '<Category>' category." message
     for an empty slice, or a markdown bullet list (mirroring
     `SEARCH_TRANSACTIONS`'s `- {desc} — ${amount} ({date})` per-row format,
     with a header line stating the category and count) otherwise. Extracting
     this as a pure function (taking plain tuples, not `sqlx::Row`) makes both
     of the ticket's explicit "empty result" and "complete, date-ordered"
     message-text ACs unit-testable without a database.
  5. A DB error at any query step → warn-log + generic "I couldn't list your
     transactions right now." message (never a raw DB error, never a silent
     fallback to an unfiltered list).

- **`transactions_list_md: Option<String>`** declared alongside
  `search_results_md` (~line 1763), appended to `final_response_text` in the
  same style, after the `search_results_md` append (~line 3189-3191).

- **Prompt schema** (~line 1226): append `| "LIST_TRANSACTIONS"` to the action
  enum string.

- **Prompt rule** (new rule, numbered `23` — the next free integer after the
  existing `22.` "ALWAYS produce perfectly clean... JSON" rule; rule 22 is
  renumbered to `24`, OR the new rule is inserted as `20b` immediately after
  rule `20` (SEARCH_TRANSACTIONS) to avoid renumbering the trailing "always
  valid JSON" rule — the plan will pick whichever is a smaller, purely-additive
  diff after re-reading the current numbering at implementation time). Content:
  "If the user asks to see, show, or list the transactions IN or FOR a specific
  category (e.g. 'show me transactions in Food', 'what did I spend on
  Groceries', 'list transactions for the Utilities category'), set 'action' to
  'LIST_TRANSACTIONS' and populate 'category_name' with the named category.
  This returns an exact, complete list of that category's transactions in the
  active budget — do NOT try to enumerate them yourself in 'response_text' from
  the RECENT TRANSACTIONS context (that list is NOT filtered by category and
  will include transactions from every category); give a brief natural
  transition instead, e.g. \"Here are your Food transactions.\" If the category
  doesn't exist, LIST_TRANSACTIONS will tell the user so — do not guess or
  make one up."
  Also amend rule `5` (or add a short clause) making explicit that
  category-scoped transaction questions must NOT be answered from ACTIVE
  BUDGET DETAILS / RECENT TRANSACTIONS.

- **Offline fallback**: a new pure helper
  `offline_list_transactions_category(msg_lower: &str) -> Option<String>`
  colocated with `offline_category_rename`/`offline_set_category_limit`
  (~line 4216+). Matches phrases containing a transactions/spending noun
  ("transactions", "spent", "purchases") plus "in "/"for " followed by a
  category-name-shaped tail, similar in spirit to `offline_category_rename`'s
  extraction. Wired into the offline branch (~line 1477, alongside
  `offline_categories_action`) — routed BEFORE the generic categories/insights
  arms so "show me my Food transactions" doesn't fall into
  `offline_categories_action`'s "categories" keyword match (it won't, since
  that requires the literal word "categories"/"category", but ordering is
  still made explicit and tested).

## Error Handling & Edge Cases

- No active budget → clear "can't list right now" message (never crashes,
  never silently shows all transactions).
- `category_name` omitted by the model → ask which category, never guess.
- Category not found (typo/nonexistent) → clear "no such category" message,
  never a fallback to the unfiltered list (this is the AC's explicit
  anti-regression case).
- Category exists, zero transactions → explicit "no transactions in this
  category" message, distinguished from a query failure.
- DB query failure at any step → generic user-facing message + `tracing::warn!`
  with context, never a raw error, never a fallback to an unfiltered list.
- Category name matches case-insensitively but the SQL LIKE/equality must not
  match a *different* budget's identically-named category — enforced by the
  `budget_id = $1` clause in the resolution query (this is the exact class of
  bug the ticket is about, so the regression test asserts a same-named-but-
  wrong-budget category is excluded, and a same-budget-different-category is
  excluded).

## Testing Approach

- Pure unit tests (no DB) for `format_transactions_list_message` (non-empty
  rows → correct header/count/bullet-list/order; empty rows → the exact
  empty-result message) and `category_not_found_message` (exact wording) —
  this directly covers the "zero transactions → explicit empty result" and
  "unknown category → clear message" ACs without a live DB or Gemini call,
  closing the coverage gap that a code-review-only verification would leave.
- Pure unit tests (no DB) for `offline_list_transactions_category`, mirroring
  the existing `offline_category_rename_extracts_pair_and_ignores_non_category_rename`-
  style test suite: positive matches, negative/non-matching phrases, and a
  guard against colliding with `offline_categories_action`'s generic
  "categories" phrasing.
- One DB-backed `#[tokio::test] #[ignore = "requires Postgres + pgvector..."]`
  integration test (run via `cargo test -- --ignored`, per repo convention),
  mirroring `transactions_semantic_search_orders_by_distance_and_scopes_to_budget`:
  seed a budget with two categories (A, B) and transactions in each, seed a
  transaction in category-A-named-alike in a DIFFERENT budget, run the new
  query const directly, assert only budget-A's category-A transactions come
  back, ordered by date desc, and that a zero-transaction category returns an
  empty vec.
- Run `cd backend && cargo test` (fast, DB-free suite) and `cargo test --
  --ignored` (needs the local pgvector container per `AGENTS.md`) before
  opening the PR.

## Risks & Open Questions

- **Exact rule numbering**: the prompt's numbered-rule list has an irregular
  scheme (`1, 2, 2b..2l, 3-9, 11-22`, no `10`). The plan will re ­grep the
  live numbering immediately before editing (not trust this spec's guess) to
  insert the new rule as a minimal, non-renumbering diff.
- **Local pgvector availability**: the DB-backed regression test needs
  `podman-compose up -d` per `AGENTS.md`. If no local Postgres is reachable in
  this environment, the ignored test still compiles and is asserted correct by
  code review, and `cargo test` (default set) stays green; the ignored test
  will be exercised if the environment allows, otherwise this is flagged in the
  PR body per the out-of-band checklist.

## Implementation Notes (post-hoc, added after build)

Three issues surfaced during implementation/review and were fixed before
merge, each verified with a passing test:

1. **DB-error masking in category resolution** (quality review, Task 2): the
   category-resolution lookup used `.fetch_optional(...).await.unwrap_or(None)`,
   silently turning a genuine DB error into "no such category" with no
   `tracing::warn!`. Fixed to match `Ok(None)` / `Err(e)` / `Ok(Some(row))`
   explicitly, mirroring the sibling `TRANSACTIONS_BY_CATEGORY_QUERY` error
   handling in the same arm.
2. **Test/logic mismatch in the offline noun-detection guard** (implementer,
   Task 3): the originally-specified `has_tx_noun` check
   (`.contains("transaction") || .contains("spent") || .contains("spending")`)
   did not actually match the spec's own test case `"what did I spend in
   Groceries"` — "spend" is not a substring of "spent". Fixed by adding
   `|| lower.contains("spend")` (a strict superset; verified against all 9
   original test assertions plus the two negative/mutation cases).
3. **Multibyte panic + broken " in "/" for " discrimination** (quality
   review, Task 3): the article-strip loop raw-byte-sliced
   `name[..article.len()]`, reachable-from-chat-input panic on a multibyte
   category name (confirmed via direct repro: `byte index 4 is not a char
   boundary`). Fixed with `name.get(..)` (returns `None` instead of
   panicking on a non-boundary index). Separately, the " in " vs " for "
   word-start offset was discriminated by checking `b[prep_idx+2] == b' '`,
   which is never true for either pattern — every match silently took the
   " for " branch's offset, and only produced correct output for " in "
   matches because `.trim()` absorbed the resulting off-by-one. Fixed to
   discriminate correctly (`'n'` vs not) with the right offset for each.
   Two regression tests added (multibyte name, explicit in-vs-for case).

Final state: 250 fast-suite tests passing (0 failed), 99 `--ignored`
DB-backed tests passing (0 failed), `cargo clippy --all-targets` clean on
every line this ticket touched.
