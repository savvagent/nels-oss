# Category-scoped transaction listing (LIST_TRANSACTIONS) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic `LIST_TRANSACTIONS` chat action to `backend/src/rag.rs` so "show transactions for category X" returns only that category's transactions in the active budget, instead of falling through to the unfiltered RECENT TRANSACTIONS context or the budget-only SEARCH_TRANSACTIONS semantic query.

**Architecture:** One new SQL query const (exact `budget_id`+`category_id` filter, no embedding/no LIMIT), one new dispatch arm mirroring `SEARCH_TRANSACTIONS`'s markdown-addendum pattern (no `ChatResponse`/frontend changes), one new prompt rule + guardrail, one offline-router fallback, and DB-backed + pure unit regression tests — all confined to `backend/src/rag.rs`.

**Tech Stack:** Rust, axum, sqlx (PostgreSQL + pgvector), `cargo test` (`--ignored` for DB-backed tests against the local `budget-rag-db` pgvector container at `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`).

---

## File Structure

- Modify: `backend/src/rag.rs` only (per ticket integration points). No new files.

## Task 1: SQL query const + DB-backed regression test (TDD: test first)

**Confirmed schema facts** (verified directly against
`backend/migrations/20260609000000_init.sql` before writing this task — do
NOT re-derive):
- `transactions.created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()` exists (line 46
  of that migration), so `ORDER BY t.transaction_date DESC, t.created_at DESC`
  is valid and the test's `seed_tx` (which never sets `created_at` explicitly)
  gets a valid default.
- Cascade chain: `budgets.owner_id → users.id ON DELETE CASCADE` (line 14),
  `categories.budget_id → budgets.id ON DELETE CASCADE` (line 30),
  `transactions.budget_id → budgets.id ON DELETE CASCADE` (line 43). So the
  test's `DELETE FROM users WHERE id = $1` cleanup cascades through budgets →
  categories → transactions automatically — matching the sibling
  `transactions_semantic_search_orders_by_distance_and_scopes_to_budget`
  test's identical cleanup pattern.
- `categories` has `CONSTRAINT unique_category_name_per_budget UNIQUE
  (budget_id, name)` (line 34) — but it's a plain btree constraint on the
  literal `name` column, so it is **case-sensitive**: it does NOT guarantee
  uniqueness against the case-*insensitive* `LOWER(name) = LOWER($2)` lookup
  Task 2's category resolution uses (e.g. "Food" and "FOOD" could coexist in
  the same budget without violating it). Without a tie-break, `LIMIT 1` alone
  would pick nondeterministically between such rows. Task 2's resolution
  query therefore adds `ORDER BY name` before `LIMIT 1` so the pick is at
  least deterministic (not silently random) if this edge case is ever hit —
  it does not fully disambiguate (that would need a case-insensitive unique
  index, out of scope), but it removes the nondeterminism. This exact
  ambiguity boundary already exists, unaddressed, in `DELETE_CATEGORY`'s and
  `EDIT_TRANSACTION`'s identical `LOWER(name) = LOWER($2)` (no `ORDER BY`)
  lookups — this ticket does not need to fix it system-wide, only avoid
  making its own new query nondeterministic.
- `AiActionParams.category_name: Option<String>` already exists (confirmed at
  `rag.rs:244`) and is reused by `CREATE_CATEGORY`/`DELETE_CATEGORY`/
  `EDIT_TRANSACTION`/`SET_CATEGORY_ROLLOVER` — Task 2 needs NO new struct
  field, just to read this existing one.

**Files:**
- Modify: `backend/src/rag.rs:474` (insert new const after `TRANSACTION_LOOKUP_BY_EMBEDDING`)
- Test: `backend/src/rag.rs` test module (append after `transactions_semantic_search_orders_by_distance_and_scopes_to_budget`, currently ending at the file's last line, ~9794)

- [ ] **Step 1: Write the failing DB-backed test**

Append this test to the `#[cfg(test)] mod tests { ... }` block at the end of `backend/src/rag.rs` (after the final `}` that closes `transactions_semantic_search_orders_by_distance_and_scopes_to_budget`, but still inside `mod tests`):

```rust
    // LIST_TRANSACTIONS query (#229): the deterministic category-scoped filter
    // returns ONLY the matching category's transactions in the matching budget —
    // ordered by date desc, complete (no LIMIT) — and excludes a same-named
    // category in a different budget and a different category in the same
    // budget. This is the exact class of bug the ticket reports (category
    // bleed), so every exclusion case is asserted explicitly.
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn transactions_by_category_scopes_to_budget_and_category() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        sqlx::migrate!("./migrations").run(&pool).await.expect("apply migrations");

        let owner_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let other_budget_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(owner_id)
            .bind(format!("tx-by-cat-{owner_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        for (bid, name) in [(budget_id, "cat filter test"), (other_budget_id, "other budget")] {
            sqlx::query(
                "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
                 VALUES ($1, $2, $3, 'monthly', 1000.0, FALSE)",
            )
            .bind(bid)
            .bind(owner_id)
            .bind(name)
            .execute(&pool)
            .await
            .expect("seed budget");
        }

        // Two categories in the target budget, sharing a name with a category
        // in the OTHER budget — proves the bug class (category bleed across
        // budgets sharing a name) cannot happen.
        async fn seed_category(pool: &sqlx::PgPool, bid: uuid::Uuid, name: &str) -> uuid::Uuid {
            let id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO categories (id, budget_id, name, category_type) VALUES ($1, $2, $3, 'expense')",
            )
            .bind(id)
            .bind(bid)
            .bind(name)
            .execute(pool)
            .await
            .expect("seed category");
            id
        }
        let cat_food = seed_category(&pool, budget_id, "Food").await;
        let cat_travel = seed_category(&pool, budget_id, "Travel").await;
        let other_budget_cat_food = seed_category(&pool, other_budget_id, "Food").await;

        async fn seed_tx(
            pool: &sqlx::PgPool,
            bid: uuid::Uuid,
            cat_id: uuid::Uuid,
            desc: &str,
            date: chrono::DateTime<chrono::Utc>,
        ) {
            sqlx::query(
                "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
                 VALUES ($1, $2, $3, 1.0, $4, $5)",
            )
            .bind(uuid::Uuid::new_v4())
            .bind(bid)
            .bind(cat_id)
            .bind(date)
            .bind(desc)
            .execute(pool)
            .await
            .expect("seed transaction");
        }

        let now = chrono::Utc::now();
        // In target category (Food, target budget) — the rows the query must
        // return, oldest-to-newest date so the ORDER BY assertion is meaningful.
        seed_tx(&pool, budget_id, cat_food, "older food", now - chrono::Duration::days(2)).await;
        seed_tx(&pool, budget_id, cat_food, "newer food", now - chrono::Duration::days(1)).await;
        // A DIFFERENT category in the SAME budget — must be excluded.
        seed_tx(&pool, budget_id, cat_travel, "travel expense", now).await;
        // The SAME category NAME in a DIFFERENT budget — must be excluded (this
        // is exactly the bleed the ticket reports if budget scoping were
        // dropped).
        seed_tx(&pool, other_budget_id, other_budget_cat_food, "other budget food", now).await;

        let rows = sqlx::query(TRANSACTIONS_BY_CATEGORY_QUERY)
            .bind(budget_id)
            .bind(cat_food)
            .fetch_all(&pool)
            .await
            .expect("run category-scoped listing");

        let descs: Vec<String> = rows.iter().map(|r| r.get::<String, _>("description")).collect();
        assert_eq!(
            descs,
            vec!["newer food".to_string(), "older food".to_string()],
            "must return only the target category's rows in the target budget, newest first, complete (no LIMIT)"
        );
        assert!(!descs.contains(&"travel expense".to_string()), "a different category in the same budget must never appear");
        assert!(!descs.contains(&"other budget food".to_string()), "a same-named category in a different budget must never appear");

        // A category with zero transactions returns an empty result, not an error.
        let empty_cat = seed_category(&pool, budget_id, "Empty").await;
        let empty_rows = sqlx::query(TRANSACTIONS_BY_CATEGORY_QUERY)
            .bind(budget_id)
            .bind(empty_cat)
            .fetch_all(&pool)
            .await
            .expect("run category-scoped listing for empty category");
        assert!(empty_rows.is_empty(), "a category with zero transactions must return an explicit empty result");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner_id)
            .execute(&pool)
            .await
            .expect("cleanup");
    }
```

- [ ] **Step 2: Run test to verify it fails to compile (const doesn't exist yet)**

Run: `cd backend && cargo test transactions_by_category_scopes_to_budget_and_category -- --ignored`
Expected: compile error `cannot find value \`TRANSACTIONS_BY_CATEGORY_QUERY\` in this scope`

- [ ] **Step 3: Add the SQL query const**

Insert immediately after `TRANSACTION_LOOKUP_BY_EMBEDDING`'s closing `;` (currently ending around line 476, right before the `// Maximum cosine distance` comment at line ~478):

```rust
// Deterministic, exhaustive category-scoped transaction listing for the
// read-only LIST_TRANSACTIONS chat action (#229). Unlike
// TRANSACTIONS_SEMANTIC_SEARCH_QUERY this is NOT embedding-ordered and has NO
// LIMIT — "list all transactions in category X" must be complete, not a
// nearest-neighbor sample. Scoped by BOTH budget_id AND category_id so a
// same-named category in a different budget, or a different category in the
// same budget, can never bleed into the result (the exact bug class #229
// reports). Selects only the columns the dispatch arm renders — no
// `categories` join, since the caller already knows the resolved category
// name from the lookup that ran before this query. Bind order:
// $1 = budget_id, $2 = category_id.
const TRANSACTIONS_BY_CATEGORY_QUERY: &str =
    "SELECT t.description, t.amount, t.transaction_date \
     FROM transactions t \
     WHERE t.budget_id = $1 AND t.category_id = $2 \
     ORDER BY t.transaction_date DESC, t.created_at DESC";
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test transactions_by_category_scopes_to_budget_and_category -- --ignored`
Expected: PASS (requires the local pgvector container from `podman-compose up -d` / `AGENTS.md`; already running at `localhost:6153` in this environment)

- [ ] **Step 5: Run the full fast (non-ignored) suite to confirm nothing else broke**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean, existing tests still pass (this task only adds a new const + a new `#[ignore]`d test, so the default suite is unaffected)

- [ ] **Step 6: Commit**

```bash
git add backend/src/rag.rs
git commit -m "$(cat <<'EOF'
feat(#229): add deterministic category-scoped transaction query

Add TRANSACTIONS_BY_CATEGORY_QUERY — filtered by budget_id AND category_id,
ordered by date, no LIMIT — plus a DB-backed regression test proving it
excludes a same-named category in another budget and a different category
in the same budget, and returns an explicit empty result for a
zero-transaction category.
EOF
)"
```

## Task 2: LIST_TRANSACTIONS dispatch arm + response wiring

**Files:**
- Modify: `backend/src/rag.rs:1763` (declare `transactions_list_md`)
- Modify: `backend/src/rag.rs` (new pure message-formatting helpers, colocated near `format_mutation_notice` — re-grep `"fn format_mutation_notice"` before editing)
- Modify: `backend/src/rag.rs` (new dispatch arm, inserted between the `"SEARCH_TRANSACTIONS"` arm's closing `}` at line 3162 and the `"EDIT_TRANSACTION" => {` arm at line 3163 — re-grep `'"EDIT_TRANSACTION" =>'` immediately before editing, since Task 1 shifts line numbers)
- Modify: `backend/src/rag.rs:3189` (append `transactions_list_md` to `final_response_text`, after the existing `search_results_md` append block)
- Test: `backend/src/rag.rs` test module (pure unit tests for the two new formatter helpers)

The dispatch arm's *wiring* (which branch runs when) has no isolated unit test — it's exercised end-to-end only via the full `chat_endpoint`, which requires a live Gemini call or the offline router (out of scope to add mocking for). But the *message text* each branch produces — including the two ACs "zero-transaction category returns an explicit empty result" and "unknown category yields a clear message" — is extracted into pure, unit-tested helper functions (Steps 1-4 below), so those specific ACs get real regression coverage without a DB or network call. TDD ordering applies to the helpers; the dispatch-arm wiring itself is verified by `cargo check` + `cargo test` (no regressions) + code review against the five-branch logic.

- [ ] **Step 1: Write the failing unit tests for the two pure formatter helpers**

Add near the existing tests for `format_mutation_notice` (find via `grep -n "fn format_mutation_notice" backend/src/rag.rs`, then locate its test in `mod tests` via `grep -n "fn format_mutation_notice_" backend/src/rag.rs` — insert this new test after whichever existing formatter test is closest, or simply append inside `mod tests` near the other pure-formatter tests):

```rust
    #[test]
    fn category_not_found_message_names_the_category() {
        assert_eq!(
            category_not_found_message("Foo"),
            "I couldn't find a category named 'Foo' in the active budget."
        );
    }

    #[test]
    fn format_transactions_list_message_empty_vs_populated() {
        // Zero transactions -> an explicit, distinguishable empty-result
        // message (AC: "Categories with zero transactions return an explicit
        // empty result, not the month's unfiltered list").
        assert_eq!(
            format_transactions_list_message("Empty", &[]),
            "No transactions found in the 'Empty' category."
        );

        // Populated -> a header naming the category + count, then one
        // date-ordered bullet per row (AC: "complete, date-ordered").
        let d1 = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let d2 = chrono::DateTime::parse_from_rfc3339("2026-06-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows = vec![
            ("newer food".to_string(), 12.5_f64, d2),
            ("older food".to_string(), 3.0_f64, d1),
        ];
        let msg = format_transactions_list_message("Food", &rows);
        assert!(msg.starts_with("Transactions in 'Food' (2 total):\n"));
        assert!(msg.contains("- newer food — $12.50 (2026-06-02)"));
        assert!(msg.contains("- older food — $3.00 (2026-06-01)"));
        // Preserves caller-supplied row order (the SQL query owns ordering).
        let newer_pos = msg.find("newer food").unwrap();
        let older_pos = msg.find("older food").unwrap();
        assert!(newer_pos < older_pos, "message must preserve the rows' input order");
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cd backend && cargo test category_not_found_message_names_the_category format_transactions_list_message_empty_vs_populated`
Expected: compile errors — `cannot find function \`category_not_found_message\`\` / \`format_transactions_list_message\`` in this scope.

- [ ] **Step 3: Implement the two pure formatter helpers**

Insert near `format_mutation_notice` (re-grep `"fn format_mutation_notice"` — insert immediately after its closing `}`):

```rust
/// The read-only "no such category" message for LIST_TRANSACTIONS (#229),
/// used when `category_name` doesn't resolve to a category in the active
/// budget. Pure so this AC-mandated message ("unknown category yields a
/// clear message, not a silent fallback to all transactions") is
/// unit-testable without a database.
fn category_not_found_message(category_name: &str) -> String {
    format!(
        "I couldn't find a category named '{}' in the active budget.",
        category_name
    )
}

/// Render the LIST_TRANSACTIONS (#229) read-only addendum from already-fetched
/// rows (description, amount, date) for a resolved category. An empty slice
/// renders the AC-mandated explicit empty-result message rather than being
/// silently indistinguishable from "no query ran". A non-empty slice renders
/// a header (category + count) followed by one date-ordered bullet per row —
/// the caller is responsible for ordering (TRANSACTIONS_BY_CATEGORY_QUERY
/// orders by date desc), this function only formats. Pure (plain tuples, not
/// `sqlx::Row`) so it's unit-testable without a database.
fn format_transactions_list_message(
    category_name: &str,
    rows: &[(String, f64, chrono::DateTime<chrono::Utc>)],
) -> String {
    if rows.is_empty() {
        return format!(
            "No transactions found in the '{}' category.",
            category_name
        );
    }
    let mut md = format!(
        "Transactions in '{}' ({} total):\n",
        category_name,
        rows.len()
    );
    for (desc, amount, date) in rows {
        md.push_str(&format!(
            "- {} — ${:.2} ({})\n",
            desc,
            amount,
            date.format("%Y-%m-%d")
        ));
    }
    md
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test category_not_found_message_names_the_category format_transactions_list_message_empty_vs_populated`
Expected: PASS

- [ ] **Step 5: Declare the response variable**

Immediately after the line `let mut search_results_md: Option<String> = None;` (currently line 1763), add:

```rust
    // Markdown list of a category-scoped transaction listing for the read-only
    // LIST_TRANSACTIONS action (#229). Mirrors search_results_md: appended as a
    // plain addendum to response_text, never routed through
    // mutation_log/mutation_error (this is a read, not a mutation).
    let mut transactions_list_md: Option<String> = None;
```

- [ ] **Step 6: Add the action enum entry to the JSON schema**

Re-grep the current line: `grep -n '"action":' backend/src/rag.rs` — find the line containing `\"action\": \"NONE\" | ... | \"SEARCH_TRANSACTIONS\" | \"DELETE_ACCOUNT\" | \"REPORT_ISSUE\",`. Change:

```
| \"LIST_CATEGORIES\" | \"SEARCH_TRANSACTIONS\" | \"DELETE_ACCOUNT\" | \"REPORT_ISSUE\",\n\
```

to:

```
| \"LIST_CATEGORIES\" | \"SEARCH_TRANSACTIONS\" | \"LIST_TRANSACTIONS\" | \"DELETE_ACCOUNT\" | \"REPORT_ISSUE\",\n\
```

- [ ] **Step 7: Add the prompt rule + RECENT TRANSACTIONS guardrail**

Re-grep `grep -n "20\. SEARCH_TRANSACTIONS:" backend/src/rag.rs` to find the current line of rule 20. Immediately after that rule's line (which ends `...never to log a new one (that is ADD_TRANSACTION).\n\`), insert a new rule `20b`:

```
         20b. LIST_TRANSACTIONS: If the user asks to see, show, or list the transactions IN or FOR a SPECIFIC category (e.g. 'show me transactions in Food', 'what did I spend on Groceries', 'list transactions for the Utilities category'), set 'action' to 'LIST_TRANSACTIONS' and populate 'category_name' with the named category. This runs an EXACT, COMPLETE, category-filtered database query and appends the full list to your reply automatically — do NOT try to enumerate them yourself in 'response_text', and CRITICALLY do NOT answer from the RECENT TRANSACTIONS context above (that list is filtered by budget ONLY, not by category, and will silently include transactions from every category — using it for a category-scoped question is the exact bug this action exists to fix). Give a brief natural transition instead, e.g. \"Here are your Food transactions.\" If the named category doesn't exist in the active budget, LIST_TRANSACTIONS will tell the user so automatically — never guess or invent a category, and never fall back to listing everything.\n\
```

(Using the `20b` sub-letter convention already established by `2b`-`2l` avoids renumbering rules 21/22, keeping the diff minimal and additive.)

- [ ] **Step 8: Add the dispatch arm**

Re-grep `grep -n '"EDIT_TRANSACTION" =>' backend/src/rag.rs` to find the current insertion point. Insert this arm immediately BEFORE that line (i.e., right after the `"SEARCH_TRANSACTIONS"` arm's closing `}`). Note this calls the `category_not_found_message`/`format_transactions_list_message` helpers from Step 3, not inline `format!`s:

```rust
        // LIST_TRANSACTIONS (#229): read-only, deterministic (NOT semantic)
        // listing of a single category's transactions in the active budget.
        // Exists because no prior read path filters by category_id: the
        // RECENT TRANSACTIONS prompt context and SEARCH_TRANSACTIONS are both
        // budget-only, so a category-scoped question would otherwise be
        // answered from the wrong (unfiltered) data. Never mutates; not
        // routed through mutation_log/mutation_error.
        "LIST_TRANSACTIONS" => {
            match active_budget_id {
                None => {
                    tracing::warn!("LIST_TRANSACTIONS dispatched with no active budget");
                    transactions_list_md = Some(
                        "I couldn't list your transactions right now.".to_string(),
                    );
                }
                Some(bid) => {
                    let cat_name = parsed_ai_res
                        .action_params
                        .as_ref()
                        .and_then(|p| p.category_name.clone());
                    match cat_name {
                        None => {
                            transactions_list_md = Some(
                                "Which category would you like to see transactions for?".to_string(),
                            );
                        }
                        Some(cn) => {
                            // ORDER BY name before LIMIT 1: the (budget_id, name)
                            // UNIQUE constraint is case-SENSITIVE, so it does not
                            // rule out a case-variant duplicate (e.g. "Food" and
                            // "FOOD" in the same budget) matching this
                            // case-INsensitive lookup. Without a tie-break the
                            // pick would be nondeterministic; ORDER BY makes it
                            // deterministic instead (this pre-existing ambiguity
                            // class is shared with DELETE_CATEGORY/
                            // EDIT_TRANSACTION's identical lookup, not new here).
                            let cat_row = sqlx::query(
                                "SELECT id, name FROM categories WHERE budget_id = $1 AND LOWER(name) = LOWER($2) ORDER BY name LIMIT 1",
                            )
                            .bind(bid)
                            .bind(cn.trim())
                            .fetch_optional(&state.db)
                            .await
                            .unwrap_or(None);

                            match cat_row {
                                None => {
                                    transactions_list_md = Some(category_not_found_message(&cn));
                                }
                                Some(row) => {
                                    let resolved_cat_id: Uuid = row.get("id");
                                    let resolved_cat_name: String = row.get("name");
                                    let rows = sqlx::query(TRANSACTIONS_BY_CATEGORY_QUERY)
                                        .bind(bid)
                                        .bind(resolved_cat_id)
                                        .fetch_all(&state.db)
                                        .await;
                                    match rows {
                                        Ok(matches) => {
                                            let tuples: Vec<(String, f64, chrono::DateTime<chrono::Utc>)> = matches
                                                .iter()
                                                .map(|m| {
                                                    (
                                                        m.get::<String, _>("description"),
                                                        m.get::<f64, _>("amount"),
                                                        m.get::<chrono::DateTime<chrono::Utc>, _>("transaction_date"),
                                                    )
                                                })
                                                .collect();
                                            transactions_list_md = Some(format_transactions_list_message(
                                                &resolved_cat_name,
                                                &tuples,
                                            ));
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = ?e,
                                                budget_id = %bid,
                                                category_id = %resolved_cat_id,
                                                "LIST_TRANSACTIONS: failed to run category-scoped query"
                                            );
                                            transactions_list_md = Some(
                                                "I couldn't list your transactions right now.".to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
```

- [ ] **Step 9: Append `transactions_list_md` to the final response**

Re-grep `grep -n "Append semantic transaction search results" backend/src/rag.rs` to find the current block. Immediately after the existing block:

```rust
    // Append semantic transaction search results (#195) as a read-only addendum.
    if let Some(results) = &search_results_md {
        final_response_text = format!("{}\n\n{}", final_response_text, results);
    }
```

add:

```rust
    // Append the category-scoped transaction listing (#229) as a read-only addendum.
    if let Some(results) = &transactions_list_md {
        final_response_text = format!("{}\n\n{}", final_response_text, results);
    }
```

- [ ] **Step 10: Compile-check**

Run: `cd backend && cargo check`
Expected: clean build. The `match parsed_ai_res.action.as_str() { ... }` block is not exhaustively typed (it already ends in a `_ => {}` wildcard per the existing code at what is now a shifted line number), so a missing arm would NOT be a compile error — instead, verify by reading the diff that the new `"LIST_TRANSACTIONS" => { ... }` arm is present and syntactically closes correctly (matching brace count) before relying on `cargo check` alone.

- [ ] **Step 11: Run the fast test suite**

Run: `cd backend && cargo test`
Expected: PASS, including the two new formatter unit tests from Step 1/4.

- [ ] **Step 12: Commit**

```bash
git add backend/src/rag.rs
git commit -m "$(cat <<'EOF'
feat(#229): wire LIST_TRANSACTIONS action into the chat dispatcher

Add the LIST_TRANSACTIONS action to the model's JSON schema and a new
prompt rule (20b) routing "transactions in/for category X" phrasings to it,
with an explicit guardrail against answering from the unfiltered RECENT
TRANSACTIONS context. The dispatch arm resolves category_name to a
budget-scoped category (case-insensitive, find-only — never create-on-miss),
runs TRANSACTIONS_BY_CATEGORY_QUERY, and renders the result via two new pure,
unit-tested helpers (category_not_found_message /
format_transactions_list_message) appended to response_text — mirroring
SEARCH_TRANSACTIONS's existing addendum pattern, so no ChatResponse or
frontend change is needed.
EOF
)"
```

## Task 3: Offline-router fallback + pure unit tests (TDD: tests first)

**Files:**
- Modify: `backend/src/rag.rs` (new pure helper, colocated with `offline_category_rename`/`offline_set_category_limit`, currently ~line 4216-4310 — re-grep before editing)
- Modify: `backend/src/rag.rs` (wire into the offline branch, currently ~line 1477 — re-grep `"else if let Some(categories_action) = offline_categories_action"` before editing)
- Test: `backend/src/rag.rs` test module (near the existing `offline_category_rename_*`/`offline_set_category_limit_*` tests, currently ~line 5089-5205 — re-grep before editing)

- [ ] **Step 1: Write the failing unit tests**

Add this test function near the existing `offline_category_rename_*` tests (find via `grep -n "fn offline_set_category_limit_extracts_name_and_amount" backend/src/rag.rs` and insert after that test's closing `}`):

```rust
    #[test]
    fn offline_list_transactions_category_extracts_name_and_ignores_mutations() {
        assert_eq!(
            offline_list_transactions_category("show me transactions in Food"),
            Some("Food".to_string())
        );
        assert_eq!(
            offline_list_transactions_category("what did I spend in Groceries"),
            Some("Groceries".to_string())
        );
        assert_eq!(
            offline_list_transactions_category("list transactions for the Utilities category"),
            Some("Utilities".to_string())
        );
        assert_eq!(
            offline_list_transactions_category("list my transactions for Travel"),
            Some("Travel".to_string())
        );
        // No transactions/spending noun -> None (not this action's intent).
        assert_eq!(offline_list_transactions_category("show my categories"), None);
        // No category name after the preposition -> None, not a bogus match.
        assert_eq!(offline_list_transactions_category("show me my transactions"), None);
        // Mutation phrasings must NOT be swallowed by this read-only fallback.
        assert_eq!(
            offline_list_transactions_category("log $15 spent on Food for dinner"),
            None
        );
        assert_eq!(
            offline_list_transactions_category("recategorize my Uber transaction as Travel"),
            None
        );
        assert_eq!(
            offline_list_transactions_category("change my $5 coffee transaction to $7"),
            None
        );
    }
```

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cd backend && cargo test offline_list_transactions_category_extracts_name_and_ignores_mutations`
Expected: compile error `cannot find function \`offline_list_transactions_category\` in this scope`

- [ ] **Step 3: Implement the pure extraction helper**

Insert immediately after `offline_set_category_limit`'s closing `}` (re-grep `"fn offline_set_category_limit(msg: &str)"` to find the current end of that function):

```rust
/// Offline-router intent+extraction for LIST_TRANSACTIONS (#229). Recognizes a
/// transactions/spending noun combined with a trailing "in "/"for " + category
/// name, e.g. "show me transactions in Food", "what did I spend in Groceries",
/// "list transactions for the Utilities category". Returns the extracted
/// category name (original casing, a trailing "category" word and surrounding
/// punctuation/articles stripped), or None when no such read-only phrasing is
/// found. Pure so the extraction is unit-testable in isolation. Routed BEFORE
/// `offline_categories_action` in the offline branch so a category-scoped
/// transaction question (which may also contain the word "category") is never
/// swallowed by the generic categories-table match.
fn offline_list_transactions_category(msg: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    let has_tx_noun = lower.contains("transaction") || lower.contains("spent") || lower.contains("spending");
    if !has_tx_noun {
        return None;
    }
    // Exclude mutation phrasings (ADD_TRANSACTION / EDIT_TRANSACTION) so this
    // read-only fallback never shadows them.
    let is_mutation = lower.contains("log ")
        || lower.contains("change")
        || lower.contains("edit")
        || lower.contains("correct")
        || lower.contains("recategorize");
    if is_mutation {
        return None;
    }

    // Find the LAST " in " or " for " in the ORIGINAL bytes (ASCII
    // case-insensitive). The last occurrence is used because the category name
    // itself sits closest to the end of these phrasings; a byte-safe scan
    // avoids indexing a separately-lowercased copy (which can change byte
    // length for multibyte input), matching the safety discipline of
    // `offline_category_rename`'s " to " scan.
    let b = msg.as_bytes();
    let mut best: Option<usize> = None;
    let mut i = 0usize;
    while i + 4 <= b.len() {
        let is_in = b[i] == b' ' && b[i + 1].eq_ignore_ascii_case(&b'i') && b[i + 2].eq_ignore_ascii_case(&b'n') && b[i + 3] == b' ';
        let is_for = i + 5 <= b.len()
            && b[i] == b' '
            && b[i + 1].eq_ignore_ascii_case(&b'f')
            && b[i + 2].eq_ignore_ascii_case(&b'o')
            && b[i + 3].eq_ignore_ascii_case(&b'r')
            && b[i + 4] == b' ';
        if is_in || is_for {
            best = Some(i);
        }
        i += 1;
    }
    let prep_idx = best?;
    let after = if b[prep_idx + 2] == b' ' {
        // " in " — the word starting 3 bytes after the space.
        &msg[prep_idx + 3..]
    } else {
        // " for " — the word starting 4 bytes after the space.
        &msg[prep_idx + 4..]
    };

    let mut name = after.trim().trim_matches(['"', '\'', '.', '?', '!']);
    // Strip a leading article ("the "/"my ") and a trailing "category" word,
    // e.g. "the Utilities category" -> "Utilities".
    for article in ["the ", "my "] {
        if name.len() > article.len() && name[..article.len()].eq_ignore_ascii_case(article) {
            name = &name[article.len()..];
        }
    }
    if let Some(stripped) = name.strip_suffix("category").or_else(|| name.strip_suffix("Category")) {
        name = stripped.trim();
    }
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test offline_list_transactions_category_extracts_name_and_ignores_mutations`
Expected: PASS

- [ ] **Step 5: Wire the fallback into the offline branch**

Re-grep `grep -n "else if let Some(categories_action) = offline_categories_action" backend/src/rag.rs` to find the current line. Insert a new `else if` arm immediately BEFORE it (so the transactions-specific match is tried first, per the ordering rationale in the helper's doc comment):

```rust
        } else if let Some(cat_name) = offline_list_transactions_category(&payload.message) {
            // Offline fallback for LIST_TRANSACTIONS (#229): deterministic
            // category filter, no embedding required, so it can run fully
            // offline (unlike SEARCH_TRANSACTIONS). Routed before
            // offline_categories_action so a category-scoped transaction
            // question is never mistaken for a plain "show my categories".
            action = "LIST_TRANSACTIONS".to_string();
            action_params.category_name = Some(cat_name.clone());
            response_text = format!("(Mock AI Offline Mode) Here are your {} transactions.", cat_name);

        } else if let Some(categories_action) = offline_categories_action(&msg_lower) {
```

(Note: this keeps the existing `} else if let Some(categories_action) = ...` line intact as the next arm — only a new arm is inserted above it, so the rest of the `if/else if` chain is untouched.)

- [ ] **Step 6: Run the fast test suite**

Run: `cd backend && cargo test`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add backend/src/rag.rs
git commit -m "$(cat <<'EOF'
feat(#229): add offline-router fallback for LIST_TRANSACTIONS

offline_list_transactions_category extracts a category name from phrasings
like "show me transactions in Food" / "what did I spend in Groceries", pure
and unit-tested (including negative cases so ADD_TRANSACTION/EDIT_TRANSACTION
mutation phrasings are never swallowed). Wired into the offline branch ahead
of offline_categories_action so a category-scoped transaction question is
never mistaken for a plain categories-list request. Unlike
SEARCH_TRANSACTIONS (embedding-based, no offline path), LIST_TRANSACTIONS is
a deterministic filter and can run fully offline, matching the convention
every other read/list action in this router already follows.
EOF
)"
```

## Task 4: Full-suite verification + spec/plan docs

**Files:**
- Create: `docs/superpowers/specs/2026-07-03-category-scoped-transactions-design.md`
- Create: `docs/superpowers/plans/2026-07-03-category-scoped-transactions.md`

- [ ] **Step 1: Run the full backend suite (fast + ignored)**

Run: `cd backend && cargo check && cargo test && cargo test -- --ignored`
Expected: all green. The `--ignored` run needs the local pgvector container (`podman-compose up -d` per `AGENTS.md`; already running in this environment on port 6153).

- [ ] **Step 2: Run `cargo fmt --check` (or `cargo fmt` if the repo doesn't gate on it — check `backend/` for a rustfmt config/CI step first) and `cargo clippy` if either is part of this repo's CI**

Run: `cd backend && cargo clippy --all-targets 2>&1 | tail -50`
Expected: no new warnings introduced by this change (pre-existing warnings, if any, are out of scope).

- [ ] **Step 3: Write the spec doc to `docs/superpowers/specs/2026-07-03-category-scoped-transactions-design.md`**

(Full spec text — the finalized, critique-approved version from Phase 1 — goes here verbatim.)

- [ ] **Step 4: Write this plan doc to `docs/superpowers/plans/2026-07-03-category-scoped-transactions.md`**

(This plan's full text, critique-approved, goes here verbatim.)

- [ ] **Step 5: Commit the docs**

```bash
git add docs/superpowers/specs/2026-07-03-category-scoped-transactions-design.md docs/superpowers/plans/2026-07-03-category-scoped-transactions.md
git commit -m "$(cat <<'EOF'
docs(#229): add spec and plan for category-scoped transaction listing
EOF
)"
```

---

## Spec Coverage Check

- New `LIST_TRANSACTIONS` action in enum + schema → Task 2 Step 6
- Deterministic SQL, no LIMIT, no embedding order → Task 1
- Category resolution scoped to active budget, case-insensitive, find-only → Task 2 Step 8
- Clear "no such category" message (not fallback to unfiltered list), pure + unit-tested → Task 2 Steps 1-4 (`category_not_found_message`) + Step 8 (wiring)
- Explicit empty-result message for a zero-transaction category, pure + unit-tested → Task 1 (query-level, DB-backed) + Task 2 Steps 1-4 (`format_transactions_list_message`, unit-tested) + Step 8 (wiring)
- New prompt rule + RECENT TRANSACTIONS guardrail → Task 2 Step 7
- Offline-router fallback → Task 3
- DB-backed regression test proving category-scoped exclusivity (budget AND category, both directions) → Task 1
- Confirmed schema facts (created_at exists, cascade delete chain, case-sensitive UNIQUE(budget_id,name)) → Task 1 preamble

## Execution Log (post-hoc — what actually happened)

All four tasks executed via dispatched implementer subagents, each followed by
a spec-compliance review and a code-quality review (`code-reviewer`), per
`general-development`'s Phase 3. Three issues surfaced and were fixed before
merge:

- **Task 2 quality review** flagged that the category-resolution query's
  `.unwrap_or(None)` silently converted a genuine DB error into "category not
  found" with no `tracing::warn!`. Fixed in commit `9543860` — now matches
  `Ok(None)`/`Err(e)`/`Ok(Some(row))` explicitly.
- **Task 3 implementer** found the plan's own `has_tx_noun` code didn't match
  its own test case (`"spend"` vs `"spent"` substring mismatch). Fixed inline
  by adding a `"spend"` check (superset, verified against all assertions),
  reported as `DONE_WITH_CONCERNS` and accepted as a correct, low-risk fix.
- **Task 3 quality review** (`code-reviewer`) found a Critical bug: the
  article-strip loop in `offline_list_transactions_category` raw-byte-sliced
  `name[..article.len()]`, panicking on a multibyte category name reachable
  directly from unsanitized chat input (confirmed via reproduction:
  `byte index 4 is not a char boundary`). Also found an Important issue: the
  " in "/" for " word-start discrimination checked a byte that's never a
  space in either pattern, silently relying on `.trim()` to mask the
  resulting off-by-one. Both fixed in commit `8fe1cb5`, with two new
  regression tests (multibyte name, explicit in-vs-for case).

Final commits (in order): `12e5fed` (Task 1), `1be86ac` (Task 2),
`9543860` (Task 2 fix), `bad7af6` (Task 3), `8fe1cb5` (Task 3 fix).

Final verification: `cargo test` → 250 passed, 0 failed, 99 ignored.
`cargo test -- --ignored` → 99 passed, 0 failed (local pgvector container on
port 6153). `cargo clippy --all-targets` → clean on every line this ticket
touched (pre-existing warnings elsewhere in the file are unrelated and
untouched).
- No frontend changes, no `ChatResponse` field changes → confirmed throughout (search for `pub struct ChatResponse` — untouched by this plan)
