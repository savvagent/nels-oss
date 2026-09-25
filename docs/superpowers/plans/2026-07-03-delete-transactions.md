# Delete Transactions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user delete a transaction they entered by mistake — via chat (confirm-then-delete, matching DELETE_CATEGORY/DELETE_BUDGET) and via a new REST endpoint the frontend confirmation modal calls.

**Architecture:** A new `DELETE /budgets/:id/transactions/:transaction_id` REST handler (`delete_transaction`, mirroring `delete_category`'s permission/closed/scoped-delete/audit shape) is the only place a transaction is actually deleted. A new `DELETE_TRANSACTION` chat action resolves the target via the same embedding-locator machinery `EDIT_TRANSACTION` already uses (extracted into a small shared helper) and returns a `PendingDeletion{kind:"transaction",...}` for the user to confirm — it performs no delete itself. The frontend's existing generic confirmation modal and `confirmDeletion()` gain a third `kind` branch.

**Tech Stack:** Rust (axum, sqlx/Postgres+pgvector), Svelte 5 (runes), svelte-i18n locale JSON files.

---

## Task 1: Backend REST — `delete_transaction` handler + route

**Files:**
- Modify: `backend/src/budget.rs` (add `delete_transaction` after `delete_category`, i.e. after line 2955; add test helpers/tests in the `#[cfg(test)] mod tests` block)
- Modify: `backend/src/main.rs:38` (import), `backend/src/main.rs:251` (route)

- [ ] **Step 1: Write the failing tests**

Add to `backend/src/budget.rs`'s `mod tests` block, near the other `update_transaction_*` tests (after `update_transaction_rejects_foreign_category`, i.e. after line ~6969-7030 — insert immediately following that function's closing brace; find it with `grep -n "async fn update_transaction_rejects_foreign_category" backend/src/budget.rs` and insert after its test body ends):

```rust
    // DELETE /budgets/:id/transactions/:transaction_id (#258): permission,
    // closed-budget, and cross-budget-scope guards mirror delete_category's
    // shape; a successful delete removes the row and writes an audit entry.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_removes_row_and_audits() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 12.5, "coffee").await;

        let status = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
        )
        .await
        .expect("delete_transaction Ok");
        assert_eq!(status, StatusCode::NO_CONTENT);

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(!exists, "transaction row must be gone after delete");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE budget_id = $1 AND action = 'DELETE_TRANSACTION'",
        )
        .bind(budget_id)
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(audit_count, 1, "delete must write exactly one DELETE_TRANSACTION audit row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // Cross-budget / unknown-id guard: deleting budget A's transaction through
    // budget B (owned by the same user) 404s and leaves the row untouched.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_cross_budget_id_is_404() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_a = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_a = only_category_id(&pool, budget_a).await;
        let tx_a = make_transaction(&state, user, budget_a, Some(cat_a), 10.0, "in A").await;

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_b, tx_a)),
            Extension(user),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::NOT_FOUND, _))),
            "cross-budget id -> 404",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_a)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 404'd delete must not remove the row");

        rollup_cleanup(&pool, &[user]).await;
    }

    // A viewer (share with 'view') cannot delete transactions -> 403.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_requires_edit_permission() {
        let (pool, owner) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, owner, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, owner, budget_id, Some(cat_id), 10.0, "owned").await;

        let viewer = Uuid::new_v4();
        let viewer_email = format!("viewer-{viewer}@example.test");
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(viewer)
            .bind(&viewer_email)
            .execute(&pool)
            .await
            .expect("seed viewer");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed view share");

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(viewer),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::FORBIDDEN, _))),
            "view permission -> 403",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 403'd delete must not remove the row");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // A closed budget is read-only for deletes too (mirrors update_transaction's
    // implicit ensure_not_closed guard).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn delete_transaction_rejects_on_closed_budget() {
        let (pool, user) = rollup_test_setup().await;
        let state = test_state(&pool);
        let budget_id = seed_rollup_budget(&pool, user, 1000.0, 0.0, false).await;
        let cat_id = only_category_id(&pool, budget_id).await;
        let tx_id = make_transaction(&state, user, budget_id, Some(cat_id), 10.0, "pre-close").await;

        sqlx::query("UPDATE budgets SET closed_at = NOW() WHERE id = $1")
            .bind(budget_id)
            .execute(&pool)
            .await
            .expect("close budget");

        let result = delete_transaction(
            State(state.clone()),
            Path((budget_id, tx_id)),
            Extension(user),
        )
        .await;
        assert!(
            matches!(result, Err((StatusCode::CONFLICT, _))),
            "closed budget -> 409",
        );

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)",
        )
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("existence check");
        assert!(exists, "a 409'd delete must not remove the row");

        rollup_cleanup(&pool, &[user]).await;
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile (handler doesn't exist yet)**

Run: `cd backend && cargo test --lib delete_transaction -- --ignored 2>&1 | head -40`
Expected: compile error, `cannot find function 'delete_transaction' in this scope`.

- [ ] **Step 3: Implement `delete_transaction`**

In `backend/src/budget.rs`, insert immediately after `delete_category`'s closing brace (after line 2955, before the `// --- TRANSACTION HANDLERS ---` comment stays where it is — place the new handler AFTER `update_transaction`'s closing brace instead, i.e. after the existing transaction handlers, so the file's handler-per-route grouping for `/transactions/:transaction_id` stays together; find the end of `update_transaction` — it returns `Ok(Json(response))` and its closing brace — and insert directly after it):

```rust
pub async fn delete_transaction(
    State(state): State<AppState>,
    Path((budget_id, transaction_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "Insufficient permissions to delete transactions".to_string()));
    }

    ensure_not_closed(&state.db, budget_id).await?;

    // Scope the lookup/delete to this budget so a transaction can never be
    // removed via a URL naming a different budget (mirrors delete_category /
    // update_transaction's cross-budget guard).
    let row = sqlx::query("SELECT description, amount FROM transactions WHERE id = $1 AND budget_id = $2")
        .bind(transaction_id)
        .bind(budget_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_error)?;

    let (description, amount): (String, f64) = match row {
        Some(r) => (r.get("description"), r.get("amount")),
        None => return Err((StatusCode::NOT_FOUND, "Transaction not found".to_string())),
    };

    sqlx::query("DELETE FROM transactions WHERE id = $1 AND budget_id = $2")
        .bind(transaction_id)
        .bind(budget_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    log_audit(
        &state.db, budget_id, user_id, "DELETE_TRANSACTION",
        &format!("Deleted transaction: {} - ${:.2}", description, amount),
    ).await;

    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 4: Wire the route in `backend/src/main.rs`**

Change the import block (line 38):

```rust
    create_transaction, list_transactions, update_transaction, delete_transaction,
```

Change the route (line 251):

```rust
        .route("/budgets/:id/transactions/:transaction_id", put(update_transaction).delete(delete_transaction))
```

- [ ] **Step 5: Run the new tests**

Run: `cd backend && cargo test --lib delete_transaction -- --ignored --test-threads=1`
Expected: 4 tests pass (`delete_transaction_removes_row_and_audits`, `delete_transaction_cross_budget_id_is_404`, `delete_transaction_requires_edit_permission`, `delete_transaction_rejects_on_closed_budget`).

- [ ] **Step 6: Run the full default (non-ignored) suite to confirm nothing else broke**

Run: `cd backend && cargo check && cargo test --lib`
Expected: builds clean, all non-`#[ignore]`d tests pass (this handler introduces no change to any existing path).

- [ ] **Step 7: Commit**

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-258-delete-transactions
git add backend/src/budget.rs backend/src/main.rs
git commit -m "feat(#258): add DELETE /budgets/:id/transactions/:transaction_id"
```

---

## Task 2: Backend chat — `DELETE_TRANSACTION` action

**Files:**
- Modify: `backend/src/rag.rs` (multiple regions — see steps)

- [ ] **Step 1: Write the failing tests**

Add to `backend/src/rag.rs`'s `#[cfg(test)] mod tests` block, directly after `chat_edit_transaction_guards_without_mutating` (after its closing brace, before `transaction_lookup_and_update_sql_round_trip`; find the insertion point with `grep -n "async fn chat_edit_transaction_guards_without_mutating" backend/src/rag.rs` and insert after that test function's closing brace at the line before the `// EDIT_TRANSACTION (#199) resolution + update building blocks` comment):

```rust
    // DELETE_TRANSACTION (#258) guards: every refusal path returns
    // (None, Some(_)) with no PendingDeletion, and never touches the row.
    // Runnable without GEMINI_API_KEY — the no-key path is itself one of the
    // guards asserted (mirrors chat_edit_transaction_guards_without_mutating).
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn chat_delete_transaction_guards_without_pending_deletion() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        sqlx::migrate!("./migrations").run(&pool).await.expect("apply migrations");
        let cipher = std::sync::Arc::new(
            crate::crypto::SecretCipher::new(&[7u8; 32]).expect("build test cipher"),
        );
        let state = AppState { db: pool.clone(), cipher };

        let owner_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let tx_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(owner_id)
            .bind(format!("delete-guard-{owner_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'delete guard test', 'monthly', 1000.0, FALSE)",
        )
        .bind(budget_id)
        .bind(owner_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description) \
             VALUES ($1, $2, NULL, $3, NOW(), $4)",
        )
        .bind(tx_id)
        .bind(budget_id)
        .bind(5.0_f64)
        .bind("coffee")
        .execute(&pool)
        .await
        .expect("seed transaction");

        async fn assert_row_intact(pool: &sqlx::PgPool, tx_id: uuid::Uuid) {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM transactions WHERE id = $1)")
                .bind(tx_id)
                .fetch_one(pool)
                .await
                .expect("existence check");
            assert!(exists, "a guarded refusal must never delete the row");
        }

        // 1. No active budget.
        let mut p = blank_action_params();
        p.transaction_match = Some("coffee".to_string());
        let (pd, err) = chat_delete_transaction(&state, owner_id, None, Some(&p)).await;
        assert!(pd.is_none() && err.is_some(), "no active budget must refuse: {err:?}");
        assert_row_intact(&pool, tx_id).await;

        // 2. Empty locator.
        let mut p = blank_action_params();
        p.transaction_match = Some("   ".to_string());
        let (pd, err) = chat_delete_transaction(&state, owner_id, Some(budget_id), Some(&p)).await;
        assert!(pd.is_none() && err.is_some(), "empty locator must refuse: {err:?}");
        assert_row_intact(&pool, tx_id).await;

        // 3. No locator at all (None params field).
        let p = blank_action_params();
        let (pd, err) = chat_delete_transaction(&state, owner_id, Some(budget_id), Some(&p)).await;
        assert!(pd.is_none() && err.is_some(), "missing locator must refuse: {err:?}");
        assert_row_intact(&pool, tx_id).await;

        // 4. Locator set, but no GEMINI_API_KEY locally -> can't resolve the target.
        let mut p = blank_action_params();
        p.transaction_match = Some("coffee".to_string());
        let (pd, err) = chat_delete_transaction(&state, owner_id, Some(budget_id), Some(&p)).await;
        assert!(pd.is_none() && err.is_some(), "no embedding key must refuse: {err:?}");
        assert_row_intact(&pool, tx_id).await;

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner_id)
            .execute(&pool)
            .await
            .expect("cleanup");
    }

    // DELETE_TRANSACTION (#258) happy-path coverage for the two pieces of
    // behavior that are actually NEW here: TRANSACTION_LOOKUP_BY_EMBEDDING now
    // selects transaction_date, and PendingDeletion.name is built from it. A
    // full end-to-end run through chat_delete_transaction (which embeds the
    // locator via get_gemini_embedding) needs a real GEMINI_API_KEY and is not
    // driveable locally — the same pre-existing limitation
    // transaction_lookup_and_update_sql_round_trip works around for
    // EDIT_TRANSACTION by seeding embeddings directly and querying
    // TRANSACTION_LOOKUP_BY_EMBEDDING via raw SQL instead of going through the
    // API-key-gated helper. This test does the same, then re-derives the exact
    // PendingDeletion.name string chat_delete_transaction would build from the
    // returned row, so the date-column addition and name format are both
    // exercised without requiring network access.
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn transaction_lookup_resolves_date_for_pending_deletion_name() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
        sqlx::migrate!("./migrations").run(&pool).await.expect("apply migrations");

        fn axis_vec(idx: usize, value: f32) -> Vec<f32> {
            let mut v = vec![0.0_f32; 768];
            v[idx] = value;
            v
        }

        let owner_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let tx_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(owner_id)
            .bind(format!("delete-roundtrip-{owner_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'delete roundtrip test', 'monthly', 1000.0, FALSE)",
        )
        .bind(budget_id)
        .bind(owner_id)
        .execute(&pool)
        .await
        .expect("seed budget");
        sqlx::query(
            "INSERT INTO transactions (id, budget_id, category_id, amount, transaction_date, description, embedding) \
             VALUES ($1, $2, NULL, $3, '2026-07-01T00:00:00Z', $4, $5::vector)",
        )
        .bind(tx_id)
        .bind(budget_id)
        .bind(5.0_f64)
        .bind("coffee")
        .bind(vector_to_string(&axis_vec(0, 1.0)))
        .execute(&pool)
        .await
        .expect("seed transaction");

        let mut q = vec![0.0_f32; 768];
        q[0] = 1.0;
        let resolved = sqlx::query(TRANSACTION_LOOKUP_BY_EMBEDDING)
            .bind(budget_id)
            .bind(vector_to_string(&q))
            .fetch_optional(&pool)
            .await
            .expect("run lookup")
            .expect("a row");

        let description: String = resolved.get("description");
        let amount: f64 = resolved.get("amount");
        let transaction_date: chrono::DateTime<chrono::Utc> = resolved.get("transaction_date");
        let name = format!(
            "{} — ${:.2} ({})",
            description,
            amount,
            transaction_date.format("%Y-%m-%d"),
        );
        assert_eq!(name, "coffee — $5.00 (2026-07-01)");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner_id)
            .execute(&pool)
            .await
            .expect("cleanup");
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd backend && cargo test --lib chat_delete_transaction -- --ignored 2>&1 | head -40`
Expected: compile error, `cannot find function 'chat_delete_transaction'` / `blank_action_params` already exists so that part compiles.

- [ ] **Step 3: Extend `PendingDeletion.kind` doc comment and add `transaction_date` to the shared lookup query**

In `backend/src/rag.rs`, change the `PendingDeletion` struct doc (around line 126-133):

```rust
/// A deletion proposed by the assistant, awaiting explicit user confirmation.
#[derive(Serialize)]
pub struct PendingDeletion {
    pub kind: String, // "category" | "budget" | "transaction"
    pub id: Uuid,
    pub name: String,
    /// The budget the category/transaction belongs to (needed for the REST
    /// delete URL). `None` for budget deletions.
    pub budget_id: Option<Uuid>,
}
```

Change `TRANSACTION_LOOKUP_BY_EMBEDDING` (around line 473-483) to also select the date, needed for DELETE_TRANSACTION's display name (Assumption 1/4 in the spec — additive column, `chat_edit_transaction` already reads by name via `row.get(...)` so this is non-breaking):

```rust
// Resolve the single transaction in a budget that best matches a locator
// embedding, for the EDIT_TRANSACTION and DELETE_TRANSACTION chat actions
// (#199, #258). Budget-scoped (the active budget is already permission-gated
// upstream). Also returns the cosine `distance` so the caller can reject a
// poor match instead of acting on the nearest unrelated transaction (see
// EDIT_TRANSACTION_MAX_DISTANCE). `transaction_date` is selected so
// DELETE_TRANSACTION can render an identifiable date in its confirmation
// prompt; EDIT_TRANSACTION ignores it. Bind order: $1 = budget_id,
// $2 = locator embedding vector.
const TRANSACTION_LOOKUP_BY_EMBEDDING: &str =
    "SELECT id, category_id, description, amount, transaction_date, (embedding <=> $2::vector) AS distance \
     FROM transactions \
     WHERE budget_id = $1 AND embedding IS NOT NULL \
     ORDER BY embedding <=> $2::vector LIMIT 1";
```

- [ ] **Step 4: Extract the shared locator-resolution helper and refactor `chat_edit_transaction` to use it**

Directly above `chat_edit_transaction` (before its doc comment, around line 3988), add:

```rust
/// A transaction resolved by `resolve_transaction_by_locator`: the fields
/// both EDIT_TRANSACTION and DELETE_TRANSACTION need from the nearest
/// embedding match.
struct ResolvedTransaction {
    id: Uuid,
    category_id: Option<Uuid>,
    description: String,
    amount: f64,
    transaction_date: DateTime<Utc>,
}

/// Resolve a free-text locator to the single closest transaction in `bid` by
/// embedding similarity, shared by EDIT_TRANSACTION (#199) and
/// DELETE_TRANSACTION (#258) so the matching algorithm and ambiguity guard
/// (EDIT_TRANSACTION_MAX_DISTANCE) can never drift between the two actions.
/// Returns `Err(user_facing_message)` on an empty locator, a missing/failed
/// embedding (e.g. no GEMINI_API_KEY), no match, or a match beyond the
/// distance cutoff.
async fn resolve_transaction_by_locator(
    state: &AppState,
    user_id: Uuid,
    bid: Uuid,
    locator: &str,
) -> Result<ResolvedTransaction, String> {
    if locator.trim().is_empty() {
        return Err("Tell me which transaction you mean.".to_string());
    }

    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    let loc_embedding = if !api_key.is_empty() {
        get_gemini_embedding(locator, &api_key, &state.db, user_id).await
    } else {
        None
    };
    let v = match loc_embedding {
        Some(v) => v,
        None => return Err("I couldn't look up that transaction right now.".to_string()),
    };

    let vec_str = vector_to_string(&v);
    let target = match sqlx::query(TRANSACTION_LOOKUP_BY_EMBEDDING)
        .bind(bid)
        .bind(&vec_str)
        .fetch_optional(&state.db)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = ?e, budget_id = %bid, "transaction locator lookup failed");
            None
        }
    };
    let row = match target {
        Some(r) => r,
        None => return Err("I couldn't find a matching transaction.".to_string()),
    };

    let distance: f64 = row.get("distance");
    if distance > EDIT_TRANSACTION_MAX_DISTANCE {
        return Err("I couldn't find a transaction matching that description — can you be more specific about which one you mean?".to_string());
    }

    Ok(ResolvedTransaction {
        id: row.get("id"),
        category_id: row.get("category_id"),
        description: row.get("description"),
        amount: row.get("amount"),
        transaction_date: row.get("transaction_date"),
    })
}
```

Now refactor `chat_edit_transaction` to call this helper instead of its inline copy. The `let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();` line stays exactly as-is (it is still read later, at the description-change re-embed near the `tx_embedding` line: `description_changed && !api_key.is_empty()`, `&api_key`) — do NOT delete it. Replace ONLY the block that starts at the very next line, `let loc_embedding = ...`, and runs through `let old_cat: Option<Uuid> = row.get("category_id");` inclusive (this is everything between the `api_key` declaration and the `// Resolve new category (find-or-create expense), if asked.` comment that follows):

```rust
    let resolved = match resolve_transaction_by_locator(state, user_id, bid, &locator).await {
        Ok(r) => r,
        Err(msg) => return (None, Some(msg)),
    };

    let tx_id: Uuid = resolved.id;
    let old_desc: String = resolved.description;
    let old_amount: f64 = resolved.amount;
    let old_cat: Option<Uuid> = resolved.category_id;
```

So after this edit, `chat_edit_transaction` reads, in order: the `locator`/`has_new_values` guards (unchanged) → `let api_key = ...;` (unchanged, kept for later reuse) → the new `let resolved = match resolve_transaction_by_locator(...)` block above → `// Resolve new category (find-or-create expense), if asked.` (unchanged, continues to reference `old_cat`/`tx_id`/`old_desc`/`old_amount` exactly as before, now sourced from `resolved` instead of `row`). `EDIT_TRANSACTION_MAX_DISTANCE` and `TRANSACTION_LOOKUP_BY_EMBEDDING` are referenced only inside the new shared helper now — no more direct references from `chat_edit_transaction` itself.

- [ ] **Step 5: Add `chat_delete_transaction`**

Directly after `chat_edit_transaction`'s closing brace (after line ~4160), add:

```rust
// DELETE_TRANSACTION (#258): resolve a transaction to delete via the same
// locator/embedding machinery as EDIT_TRANSACTION, but — mirroring
// DELETE_CATEGORY/DELETE_BUDGET — never delete here. Returns a PendingDeletion
// for the frontend confirmation modal; the actual delete happens only via the
// user-confirmed DELETE /budgets/:id/transactions/:transaction_id REST call
// (which also enforces ensure_not_closed — deliberately NOT checked here,
// matching DELETE_CATEGORY's resolve-only shape).
async fn chat_delete_transaction(
    state: &AppState,
    user_id: Uuid,
    active_budget_id: Option<Uuid>,
    params: Option<&AiActionParams>,
) -> (Option<PendingDeletion>, Option<String>) {
    let bid = match active_budget_id {
        Some(b) => b,
        None => return (None, Some("You don't have an active budget to delete a transaction from.".to_string())),
    };
    let locator = params
        .and_then(|p| p.transaction_match.clone())
        .unwrap_or_default();
    if locator.trim().is_empty() {
        return (None, Some("Tell me which transaction to delete — e.g. \"delete my $5 coffee transaction\".".to_string()));
    }

    match resolve_transaction_by_locator(state, user_id, bid, &locator).await {
        Ok(resolved) => (
            Some(PendingDeletion {
                kind: "transaction".to_string(),
                id: resolved.id,
                name: format!(
                    "{} — ${:.2} ({})",
                    resolved.description,
                    resolved.amount,
                    resolved.transaction_date.format("%Y-%m-%d"),
                ),
                budget_id: Some(bid),
            }),
            None,
        ),
        Err(msg) => (None, Some(msg)),
    }
}
```

- [ ] **Step 6: Wire the `DELETE_TRANSACTION` match arm**

In the big `match effective_action { ... }` block, directly after the `"EDIT_TRANSACTION" => { ... }` arm (around line 3439-3443), add:

```rust
        "DELETE_TRANSACTION" => {
            let (pd, me) = chat_delete_transaction(&state, user_id, active_budget_id, parsed_ai_res.action_params.as_ref()).await;
            pending_deletion = pd;
            mutation_error = me;
        }
```

- [ ] **Step 7: Add `required_perm_for_action("DELETE_TRANSACTION")`**

In `required_perm_for_action` (around line 545-554), add `"DELETE_TRANSACTION"` to the Edit-or-Owner arm:

```rust
        "CREATE_CATEGORY" | "SEED_CATEGORIES" | "ADD_TRANSACTION" | "EDIT_TRANSACTION" | "DELETE_TRANSACTION" | "UPDATE_BUDGET"
        | "CREATE_GOAL" | "ADD_GOAL_CONTRIBUTION" | "DELETE_CATEGORY"
        | "SET_CATEGORY_ROLLOVER" | "UPDATE_CATEGORY" => Some(Permission::Edit),
```

Also update the doc comment above the function (around line 533-536) to list `DELETE_TRANSACTION` alongside `EDIT_TRANSACTION`.

- [ ] **Step 8: Add the action to the system-prompt enum literal and schema doc**

At line 1357, add `"DELETE_TRANSACTION"` to the `action` enum literal, right after `"EDIT_TRANSACTION"`:

```rust
           "action": "NONE" | "CREATE_BUDGET" | "UPDATE_BUDGET" | "CLOSE_BUDGET" | "ARCHIVE_BUDGET" | "UNARCHIVE_BUDGET" | "ROLLUP_BUDGET" | "UNROLLUP_BUDGET" | "CREATE_CATEGORY" | "SEED_CATEGORIES" | "DELETE_CATEGORY" | "UPDATE_CATEGORY" | "SET_CATEGORY_ROLLOVER" | "DELETE_BUDGET" | "ADD_TRANSACTION" | "EDIT_TRANSACTION" | "DELETE_TRANSACTION" | "SHARE_BUDGET" | "CREATE_GOAL" | "ADD_GOAL_CONTRIBUTION" | "CREATE_REMINDER" | "LIST_BUDGETS" | "SWITCH_BUDGET" | "SET_USER_NAME" | "EXPORT_DATA" | "OPEN_INSIGHTS" | "OPEN_BUDGETS_LIST" | "LIST_CATEGORIES" | "SEARCH_TRANSACTIONS" | "LIST_TRANSACTIONS" | "DELETE_ACCOUNT" | "REPORT_ISSUE",
```

At line 1389 (the `transaction_match` field doc in the `action_params` schema), extend the comment to mention both actions:

```rust
             "transaction_match": "string (optional, a short phrase identifying which existing transaction to edit or delete, for EDIT_TRANSACTION / DELETE_TRANSACTION)",
```

- [ ] **Step 9: Add `DELETE_TRANSACTION_RULE` and inject it into the system prompt**

Directly after `EDIT_TRANSACTION_RULE`'s closing `;` (around line 698), add:

```rust
// Rule 21a (DELETE_TRANSACTION) of the chat system prompt's CRITICAL RULES,
// mirroring rule 2c's DELETE_CATEGORY/DELETE_BUDGET confirm-before-delete
// pattern and reusing EDIT_TRANSACTION's transaction_match locator (#258).
const DELETE_TRANSACTION_RULE: &str = "21a. DELETE_TRANSACTION: If the user asks to delete, remove, or undo a transaction they logged (e.g. 'delete my $5 coffee transaction', 'remove that grocery charge, I added it by mistake'), set 'action' to 'DELETE_TRANSACTION'. Put a short locator describing WHICH transaction in 'transaction_match' (e.g. '$5 coffee', 'grocery charge') — the same field EDIT_TRANSACTION uses. IMPORTANT: deletions are NOT performed immediately — the app shows the user a confirmation dialog and only deletes if they confirm. In 'response_text', briefly state which transaction you found and that you'll ask them to confirm before deleting it. Do NOT use this to change a transaction's amount/category/description (that is EDIT_TRANSACTION).";
```

Change the `format!()` call's placeholder list: at line 1427, the format string currently has exactly ONE `{}\n\` placeholder immediately before `22. ALWAYS produce...`, consumed by `EDIT_TRANSACTION_RULE` (the last positional arg today). Add a SECOND `{}\n\` line directly after it (still before `22. ALWAYS produce...`), and add `DELETE_TRANSACTION_RULE` as a new trailing positional argument after `EDIT_TRANSACTION_RULE`. The format string and its args must end up looking like this (note: TWO `{}\n\` lines before rule 22, matching the TWO new/existing trailing args `EDIT_TRANSACTION_RULE, DELETE_TRANSACTION_RULE`):

```rust
         {}\n\
         {}\n\
         22. ALWAYS produce perfectly clean, valid, parseable JSON only.",
        user_email,
        name_context,
        language_context,
        budgets_context,
        budget_context,
        semantic_context,
        history_context,
        usage_context,
        ADD_TRANSACTION_RULE,
        EDIT_TRANSACTION_RULE,
        DELETE_TRANSACTION_RULE
    );
```

(Verify after editing that the number of `{}` placeholders in the whole format string equals the number of trailing positional args — `cargo check` will fail loudly with "argument never used" or "invalid reference" if they mismatch.)

- [ ] **Step 10: Run the new and existing tests**

Run: `cd backend && cargo test --lib chat_delete_transaction -- --ignored --test-threads=1` and `cargo test --lib transaction_lookup_resolves_date_for_pending_deletion_name -- --ignored`
Expected: `chat_delete_transaction_guards_without_pending_deletion` passes (4 sub-assertions) and `transaction_lookup_resolves_date_for_pending_deletion_name` passes.

Run: `cd backend && cargo test --lib chat_edit_transaction -- --ignored --test-threads=1` and `cargo test --lib transaction_lookup_and_update_sql_round_trip -- --ignored`
Expected: both still pass unchanged — confirms the `resolve_transaction_by_locator` extraction was behavior-preserving for the existing EDIT_TRANSACTION path.

- [ ] **Step 11: Add the `required_perm_for_action`/`action_authorized` unit test**

In `mod tests`, extend the `edit_actions_allow_owner_and_edit_but_not_view` test's action list (around line 6379-6389) to include `"DELETE_TRANSACTION"`:

```rust
        for action in [
            "CREATE_CATEGORY",
            "SEED_CATEGORIES",
            "ADD_TRANSACTION",
            "UPDATE_BUDGET",
            "CREATE_GOAL",
            "ADD_GOAL_CONTRIBUTION",
            "DELETE_CATEGORY",
            "SET_CATEGORY_ROLLOVER",
            "UPDATE_CATEGORY",
            "DELETE_TRANSACTION",
        ] {
```

- [ ] **Step 12: Run the full default (non-ignored) suite plus clippy**

Run: `cd backend && cargo check && cargo clippy --all-targets -- -D warnings 2>&1 | tail -60 && cargo test --lib`
Expected: clean build, no new clippy warnings, all non-`#[ignore]`d tests pass.

- [ ] **Step 13: Commit**

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-258-delete-transactions
git add backend/src/rag.rs
git commit -m "feat(#258): add DELETE_TRANSACTION chat action"
```

---

## Task 3: Frontend — support `pendingDeletion.kind === "transaction"`

**Files:**
- Modify: `frontend/src/App.svelte` (lines ~85, ~1292-1307, ~1556-1567 — verify exact numbers at implementation time, they may have shifted by a few lines from Task 1/2's backend-only changes not touching this file)
- Modify: `frontend/src/lib/i18n/locales/en.json`, `es.json`, `de.json`, `fr.json`, `it.json`, `pt.json`

- [ ] **Step 1: Add the `transactionBody` locale key to all 6 locale files**

In each locale file, inside the `"confirmDelete": { ... }` object, add a `"transactionBody"` key immediately after `"categoryBody"`. Use these translations (each file's existing `categoryBody`/`budgetBody` cadence was matched):

`frontend/src/lib/i18n/locales/en.json` (after the `categoryBody` line):
```json
    "transactionBody": "Permanently delete the transaction “{name}”? This can’t be undone.",
```

`frontend/src/lib/i18n/locales/es.json`:
```json
    "transactionBody": "¿Eliminar permanentemente la transacción «{name}»? Esta acción no se puede deshacer.",
```

`frontend/src/lib/i18n/locales/de.json`:
```json
    "transactionBody": "Transaktion „{name}“ dauerhaft löschen? Dies kann nicht rückgängig gemacht werden.",
```

`frontend/src/lib/i18n/locales/fr.json`:
```json
    "transactionBody": "Supprimer définitivement la transaction « {name} » ? Cette action est irréversible.",
```

`frontend/src/lib/i18n/locales/it.json`:
```json
    "transactionBody": "Eliminare definitivamente la transazione «{name}»? Questa azione non può essere annullata.",
```

`frontend/src/lib/i18n/locales/pt.json`:
```json
    "transactionBody": "Excluir permanentemente a transação “{name}”? Essa ação não pode ser desfeita.",
```

Before editing, run `grep -n '"categoryBody"' frontend/src/lib/i18n/locales/*.json` to get the exact per-file line/quote-style (curly vs straight quotes, `«»` vs `“”`) to match each file's existing punctuation convention precisely — do not introduce a quote-style inconsistent with that file's other `confirmDelete.*` keys.

- [ ] **Step 2: Update `pendingDeletion`'s shape doc comment**

In `frontend/src/App.svelte`, change (around line 83-85):

```javascript
  // A deletion proposed by the assistant, awaiting confirmation in a custom
  // modal. Shape: { kind: "category"|"budget"|"transaction", id, name, budget_id }.
  let pendingDeletion = $state(null);
```

- [ ] **Step 3: Extend `confirmDeletion()`'s endpoint mapping**

Change (around line 1292-1307), replacing the binary ternary with a 3-way `if/else`:

```javascript
  async function confirmDeletion() {
    const pd = pendingDeletion;
    pendingDeletion = null;
    if (!pd) return;
    try {
      let endpoint;
      if (pd.kind === "category") {
        endpoint = `/budgets/${pd.budget_id}/categories/${pd.id}`;
      } else if (pd.kind === "transaction") {
        endpoint = `/budgets/${pd.budget_id}/transactions/${pd.id}`;
      } else {
        endpoint = `/budgets/${pd.id}`;
      }
      await fetchApi(endpoint, { method: "DELETE" });
      await fetchBudgets();
      pushAiMessage(t("confirmDelete.success", { values: { name: pd.name } }));
    } catch (e) {
      pushAiMessage(t("confirmDelete.failed", { values: { name: pd.name } }));
    }
  }
```

- [ ] **Step 4: Extend the confirmation modal body**

Change (around line 1556-1567):

```svelte
      <p class="py-4 text-base-content/80">
        {pendingDeletion.kind === "budget"
          ? $_("confirmDelete.budgetBody", {
              values: { name: pendingDeletion.name },
            })
          : pendingDeletion.kind === "transaction"
            ? $_("confirmDelete.transactionBody", {
                values: { name: pendingDeletion.name },
              })
            : $_("confirmDelete.categoryBody", {
                values: { name: pendingDeletion.name },
              })}
      </p>
```

- [ ] **Step 5: Build-check the frontend**

Run: `cd frontend && pnpm run build`
Expected: builds clean (Svelte/Vite compile succeeds, no template errors).

Run: `cd frontend && pnpm test 2>&1 | tail -40` (only if a `test` script exists in `frontend/package.json` — check with `cat frontend/package.json | grep '"test"'` first; if absent, skip this and note it in the commit as vacuous)
Expected: existing test suite (if any) passes unchanged.

- [ ] **Step 6: Commit**

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-258-delete-transactions
git add frontend/src/App.svelte frontend/src/lib/i18n/locales/en.json frontend/src/lib/i18n/locales/es.json frontend/src/lib/i18n/locales/de.json frontend/src/lib/i18n/locales/fr.json frontend/src/lib/i18n/locales/it.json frontend/src/lib/i18n/locales/pt.json
git commit -m "feat(#258): support transaction kind in the delete-confirmation modal"
```

---

## Task 4: Design docs

**Files:**
- Create: `docs/superpowers/specs/2026-07-03-delete-transactions-design.md` (the finalized spec text)
- Create: `docs/superpowers/plans/2026-07-03-delete-transactions.md` (this file — already created as part of planning; verify it's staged)

- [ ] **Step 1: Verify both docs are present and commit them**

This repo's convention (see `docs/superpowers/specs/` and `docs/superpowers/plans/` — every prior feature has a paired spec+plan doc landed in the SAME PR as its implementation, e.g. `390e405 feat(#191): rate-limit GitHub issue filing ... ` committed both `docs/superpowers/specs/2026-06-26-rate-limit-issue-filing-design.md` and `docs/superpowers/plans/2026-06-26-rate-limit-issue-filing.md` alongside the code) is to commit the spec+plan alongside the feature. This plan file is already at
`docs/superpowers/plans/2026-07-03-delete-transactions.md`; write the finalized spec (the text converged after the 2-round critique loop) to `docs/superpowers/specs/2026-07-03-delete-transactions-design.md`.

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-258-delete-transactions
git add docs/superpowers/specs/2026-07-03-delete-transactions-design.md docs/superpowers/plans/2026-07-03-delete-transactions.md
git commit -m "docs(#258): add delete-transactions spec + plan"
```

---

## Task 5: Rebase onto latest main and final verification

**Files:** none (integration/verification task)

- [ ] **Step 1: Rebase onto current `origin/main`**

Other concurrent work (nels#249, #228, #261, #266) may have merged to `main` since this worktree was branched. Fetch and rebase:

```bash
cd /home/robhicks/dev/nels/.worktrees/issue-258-delete-transactions
git fetch origin
git rebase origin/main
```

If conflicts appear in `backend/src/rag.rs`, `backend/src/budget.rs`, `backend/src/main.rs`, or `frontend/src/App.svelte`, they will most likely be line-shift-only (the ticket already flagged #228/#249/#261/#266 as touching different regions of these same files). Resolve by keeping BOTH sides' changes (this ticket's DELETE_TRANSACTION additions + the concurrent ticket's changes) — re-verify line numbers/context for every step in Tasks 1-3 against the post-rebase source before assuming the diff still applies cleanly, since the plan's line numbers were captured pre-rebase.

- [ ] **Step 2: Full verification pass**

```bash
cd backend && cargo check && cargo clippy --all-targets -- -D warnings && cargo test --lib && cargo test --lib -- --ignored --test-threads=1
cd ../frontend && pnpm run build
```

Expected: all green. If `cargo test -- --ignored` fails because the local pgvector container isn't reachable, verify with `pg_isready -h 127.0.0.1 -p 6153` and start it if needed (`podman-compose up -d` from the repo root) before retrying — do not skip the DB-backed tests silently.

- [ ] **Step 3: Manual smoke of the chat path (requires a running backend + a seeded transaction)**

This step is exercised in Phase 5 of the outer workflow (post-PR verification), not required to complete this task, but the commands are recorded here for that phase:

```bash
cd backend && cargo run &   # starts on :3000, auto-migrates
# separately, once a user/session/budget/transaction exist (via the normal
# app flow), POST to /api/chat with a message like "delete my $5 coffee
# transaction" and confirm the response's pending_deletion.kind == "transaction",
# then DELETE the returned id/budget_id pair and confirm 204 + the transaction
# is gone from a subsequent GET /budgets/:id/transactions.
```
