# Category-Balance Navigation Gate Fix (nels#301) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `CATEGORY_BALANCE` chat answers from navigating the user to the Categories page,
while `LIST_CATEGORIES` keeps navigating there as designed (#233), by adding a dedicated advisory
flag (`open_categories_list`, mirroring `open_insights`/`open_budgets_list`) and keying the
frontend's navigation gate off it instead of off the shared `categories_table_html` field.

**Architecture:** One backend change (`backend/src/rag.rs`): add `ChatResponse.open_categories_list:
Option<bool>`, set to `Some(true)` only in the `LIST_CATEGORIES` match arm's non-empty-rows branch.
One frontend change (`frontend/src/App.svelte`): swap the navigation gate's condition from
`chatRes.categories_table_html` to `chatRes.open_categories_list`. Both mirror the existing
`open_insights`/`open_budgets_list` pattern exactly — no new plumbing, no new route.

**Tech Stack:** Rust (axum, sqlx/PostgreSQL+pgvector) backend; Svelte 5 frontend. No new
dependencies, no migration, no REST surface change.

---

## File Structure

- **Modify `backend/src/rag.rs`**:
  - `ChatResponse` struct (~line 93-121): add `open_categories_list: Option<bool>` field, after
    `open_budgets_list` and before `categories_table_html`.
  - Local variable declarations before the action `match` (~line 1901-1909): add
    `let mut open_categories_list: Option<bool> = None;` after the `open_budgets_list` local.
  - `"LIST_CATEGORIES"` match arm (~line 3234-3269): set `open_categories_list = Some(true);`
    alongside the existing `categories_table_html = Some(...)` assignment in the `Ok(rows) if
    !rows.is_empty()` branch.
  - Final `ChatResponse { ... }` literal (~line 3612-3621): add `open_categories_list,` after
    `open_budgets_list,`.
  - Tests (inside `#[cfg(test)] mod tests`): extend
    `chat_category_balance_reports_current_period_not_lifetime` (~line 7552) and add a new test
    `chat_list_categories_sets_open_categories_list_flag`.
- **Modify `frontend/src/App.svelte`** (~line 1291-1298): change the navigation gate's condition.
- **Create `docs/superpowers/specs/2026-07-04-category-balance-nav-gate-design.md`** (already
  written) and **`docs/superpowers/plans/2026-07-04-category-balance-nav-gate.md`** (this file) —
  committed spec/plan docs, matching this repo's established convention (see e.g.
  `docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md`).

No new source files — both changes live in existing modules.

---

## Task 1: Add `open_categories_list` to `ChatResponse` and set it in the `LIST_CATEGORIES` arm

**Files:**
- Modify: `backend/src/rag.rs:93-121` (`ChatResponse` struct)
- Modify: `backend/src/rag.rs:1901-1909` (local declarations before the action match)
- Modify: `backend/src/rag.rs:3234-3269` (`"LIST_CATEGORIES"` match arm)
- Modify: `backend/src/rag.rs:3612-3621` (final `ChatResponse { ... }` literal)
- Test: `backend/src/rag.rs` inside `mod tests` — extend the test at ~line 7552
  (`chat_category_balance_reports_current_period_not_lifetime`) and add a new test near it

- [ ] **Step 1: Read the current exact code before editing**

Run: `sed -n '90,122p' backend/src/rag.rs`, `sed -n '1895,1912p' backend/src/rag.rs`,
`sed -n '3230,3270p' backend/src/rag.rs`, and `sed -n '3608,3622p' backend/src/rag.rs` — confirm
the struct/locals/match-arm/literal are still at (or near) these lines before editing. Verify
against the live file, not this plan's line numbers, since other work may have landed since this
plan was written.

- [ ] **Step 2: Write the failing/new assertions first (extend the existing DB-backed test)**

In `backend/src/rag.rs`, inside `chat_category_balance_reports_current_period_not_lifetime`
(the `#[tokio::test] #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]`
test whose "Found" section currently ends with:

```rust
        assert!(!resp.0.response.contains("⚠️"), "a successful lookup must not carry a mutation_error warning");
```

add immediately after that line:

```rust
        assert!(
            resp.0.open_categories_list.is_none(),
            "CATEGORY_BALANCE must never set the LIST_CATEGORIES navigation flag, got: {:?}",
            resp.0.open_categories_list
        );
```

Then add a brand-new test in the same `mod tests` block, right after
`chat_category_balance_no_active_budget_is_graceful` ends (search for its closing `}` around line
7729, just before the blank line and the next `#[tokio::test]`):

```rust
    // nels#301: LIST_CATEGORIES must still set the dedicated navigation flag so the frontend
    // continues to navigate to the Categories page for a full-list request (no regression to
    // #233), while CATEGORY_BALANCE (asserted above) must never set it.
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_list_categories_sets_open_categories_list_flag() {
        // #269: see GEMINI_ENV_LOCK's doc comment — every GEMINI_API_KEY-touching test must hold this.
        let _gemini_env = GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id).bind(format!("chat-list-cat-nav-{suffix}@example.test"))
            .execute(&pool).await.expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'chat list categories nav test', 'monthly', 1000.0, TRUE)",
        )
        .bind(budget_id).bind(user_id).execute(&pool).await.expect("seed budget");
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, 'Entertainment', 'expense', 250.0)",
        )
        .bind(Uuid::new_v4()).bind(budget_id).execute(&pool).await.expect("seed category");

        let resp = chat_endpoint(
            State(state),
            Extension(user_id),
            Json(ChatRequest {
                message: "show my categories".to_string(),
                budget_id: Some(budget_id),
                conversation_id: None,
                locale: None,
            }),
        )
        .await
        .expect("chat call succeeds");

        assert_eq!(
            resp.0.open_categories_list,
            Some(true),
            "LIST_CATEGORIES must set the navigation flag so the frontend still opens the \
             Categories page (no regression to #233), got: {:?}", resp.0.open_categories_list
        );
        let html = resp.0.categories_table_html.as_deref().unwrap_or_default();
        assert!(html.contains("Entertainment"), "table should name the category, got: {html}");

        sqlx::query("DELETE FROM categories WHERE budget_id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await.ok();
    }
```

- [ ] **Step 3: Run the new/extended tests to verify they fail (compile error is acceptable/expected)**

Run: `cd backend && cargo test --lib rag::tests::chat_category_balance_reports_current_period_not_lifetime -- --ignored` (requires `podman-compose up -d` running; if Postgres isn't reachable locally, run `cargo check` instead to confirm the compile failure).
Expected: compile error — `open_categories_list` is not a field of `ChatResponse` yet (`no field
open_categories_list on type ChatResponse` or similar), OR `resp.0.open_categories_list` unknown
field. This confirms the test exercises code that doesn't exist yet.

- [ ] **Step 4: Add the field to `ChatResponse`**

In `backend/src/rag.rs`, in the `ChatResponse` struct, change:

```rust
    /// Advisory signal (#241): when `Some(true)`, the frontend opens the
    /// dedicated budgets list page. Non-mutating — set by the OPEN_BUDGETS_LIST
    /// chat action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_budgets_list: Option<bool>,
    /// Escaped HTML table of the active budget's categories (#176): name, type,
```

to:

```rust
    /// Advisory signal (#241): when `Some(true)`, the frontend opens the
    /// dedicated budgets list page. Non-mutating — set by the OPEN_BUDGETS_LIST
    /// chat action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_budgets_list: Option<bool>,
    /// Advisory signal (nels#301): when `Some(true)`, the frontend navigates to the
    /// dedicated categories page. Non-mutating — set ONLY by the LIST_CATEGORIES chat
    /// action (never by CATEGORY_BALANCE, which also populates `categories_table_html`
    /// but must answer inline in the chat transcript instead of navigating away).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_categories_list: Option<bool>,
    /// Escaped HTML table of the active budget's categories (#176): name, type,
```

- [ ] **Step 5: Add the local variable**

Change:

```rust
    // Advisory signal for the OPEN_BUDGETS_LIST action (#241): drives the
    // frontend to open the dedicated budgets list page. Non-mutating, no audit,
    // no System Update.
    let mut open_budgets_list: Option<bool> = None;
    // Escaped HTML categories table for the LIST_CATEGORIES action (#176).
```

to:

```rust
    // Advisory signal for the OPEN_BUDGETS_LIST action (#241): drives the
    // frontend to open the dedicated budgets list page. Non-mutating, no audit,
    // no System Update.
    let mut open_budgets_list: Option<bool> = None;
    // Advisory signal (nels#301): drives the frontend to navigate to the categories
    // page. Set ONLY in the LIST_CATEGORIES match arm below — CATEGORY_BALANCE must
    // never set this, so its answer stays inline in the chat transcript.
    let mut open_categories_list: Option<bool> = None;
    // Escaped HTML categories table for the LIST_CATEGORIES action (#176).
```

- [ ] **Step 6: Set the flag in the `LIST_CATEGORIES` match arm**

Change:

```rust
                match crate::budget::category_table_rows(&state.db, bid).await {
                    Ok(rows) if !rows.is_empty() => {
                        categories_table_html =
                            Some(crate::budget::build_categories_table_html(&rows));
                    }
```

to:

```rust
                match crate::budget::category_table_rows(&state.db, bid).await {
                    Ok(rows) if !rows.is_empty() => {
                        categories_table_html =
                            Some(crate::budget::build_categories_table_html(&rows));
                        open_categories_list = Some(true);
                    }
```

Do NOT add `open_categories_list = Some(true)` to the `CATEGORY_BALANCE` arm — it must stay
`None` there in every outcome (this is the actual bug fix; the frontend gate in Task 2 keys off
this flag).

- [ ] **Step 7: Thread the field into the final `ChatResponse` literal**

Change:

```rust
    Ok(Json(ChatResponse {
        response: final_response_text,
        action_taken: mutation_log,
        budget_id: action_outcome_budget_id,
        conversation_id,
        pending_deletion,
        open_insights,
        open_budgets_list,
        categories_table_html,
    }))
```

to:

```rust
    Ok(Json(ChatResponse {
        response: final_response_text,
        action_taken: mutation_log,
        budget_id: action_outcome_budget_id,
        conversation_id,
        pending_deletion,
        open_insights,
        open_budgets_list,
        open_categories_list,
        categories_table_html,
    }))
```

- [ ] **Step 8: Run `cargo check` to confirm it compiles**

Run: `cd backend && cargo check`
Expected: no errors.

- [ ] **Step 9: Run the tests to verify they pass (requires local Postgres)**

Run: `podman-compose up -d` (from repo root, if not already running), then:
`cd backend && cargo test --lib rag::tests::chat_category_balance_reports_current_period_not_lifetime -- --ignored`
and
`cd backend && cargo test --lib rag::tests::chat_list_categories_sets_open_categories_list_flag -- --ignored`
Expected: both PASS. If Postgres is unavailable in this environment, run
`cd backend && cargo test` (the unignored/offline suite) and `cd backend && cargo clippy --all-targets`
instead, and note in the task report that the two new `--ignored` DB-backed tests could not be
executed locally (they will be verified against the deployed environment in Phase 5, matching this
test's existing convention for DB-backed tests in this file).

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#301): add open_categories_list flag so CATEGORY_BALANCE never signals navigation"
```

---

## Task 2: Frontend — key the categories-navigation gate off the new flag

**Files:**
- Modify: `frontend/src/App.svelte:1291-1298`

- [ ] **Step 1: Read the current exact code before editing**

Run: `sed -n '1275,1300p' frontend/src/App.svelte` — confirm the `open_insights` /
`open_budgets_list` / `categories_table_html` gate block is still at (or near) these lines.

- [ ] **Step 2: Change the gate condition and its comment**

Change:

```js
      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). The HTML itself is discarded here —
      // CategoriesView re-fetches its own fresh copy on mount, the same
      // convention Insights already uses (self-fetch via fetchApi, no data
      // via props).
      if (chatRes.categories_table_html) {
        navigate("categories");
      }
```

to:

```js
      // Navigate to the categories outlet when the assistant signals a
      // LIST_CATEGORIES turn (#233). Gated on the dedicated open_categories_list
      // flag (nels#301) rather than the mere presence of categories_table_html —
      // CATEGORY_BALANCE also populates that HTML field (a single-category table)
      // but must answer inline in the chat transcript, not navigate away. The
      // HTML itself is discarded here either way — CategoriesView re-fetches its
      // own fresh copy on mount, the same convention Insights already uses
      // (self-fetch via fetchApi, no data via props).
      if (chatRes.open_categories_list) {
        navigate("categories");
      }
```

- [ ] **Step 3: Run the frontend test suite**

Run: `cd frontend && pnpm run test`
Expected: PASS (no existing test references `categories_table_html` or this gate, so none should
regress; this run is a general safety net).

- [ ] **Step 4: Run a production build to catch any syntax issues**

Run: `cd frontend && pnpm run build`
Expected: build succeeds with no errors.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "fix(#301): key categories-page navigation off open_categories_list, not categories_table_html"
```

---

## Task 3: Commit the spec + plan docs and do a final full-suite pass

**Files:**
- Create: `docs/superpowers/specs/2026-07-04-category-balance-nav-gate-design.md` (already written)
- Create: `docs/superpowers/plans/2026-07-04-category-balance-nav-gate.md` (this file, already
  written)

- [ ] **Step 1: Confirm both docs are present and finalized**

Run: `ls docs/superpowers/specs/2026-07-04-category-balance-nav-gate-design.md
docs/superpowers/plans/2026-07-04-category-balance-nav-gate.md` — both should exist (written
earlier in this workflow, matching the naming convention of existing docs, e.g.
`docs/superpowers/specs/2026-07-03-category-balance-period-fix-design.md` /
`docs/superpowers/plans/2026-07-03-category-balance-period-fix.md`).

- [ ] **Step 2: Run the full backend test suite (offline subset) and clippy**

Run: `cd backend && cargo test` and `cd backend && cargo clippy --all-targets`
Expected: both clean (no new warnings/failures introduced by this change).

- [ ] **Step 3: Run the full frontend test suite and build**

Run: `cd frontend && pnpm run test` and `cd frontend && pnpm run build`
Expected: both clean.

- [ ] **Step 4: Commit the docs**

```bash
git add docs/superpowers/specs/2026-07-04-category-balance-nav-gate-design.md \
        docs/superpowers/plans/2026-07-04-category-balance-nav-gate.md
git commit -m "docs(#301): add spec + plan for the category-balance navigation gate fix"
```
