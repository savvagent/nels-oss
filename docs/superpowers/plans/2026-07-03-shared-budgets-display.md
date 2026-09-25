# Display budgets shared with user (chat LIST_BUDGETS) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Teach the chat context-builder in `backend/src/rag.rs` to include budgets shared
with the current user (not just owned ones) in the "USER'S BUDGETS" prompt context, and
update the LIST_BUDGETS prompt instruction (rule 11) so Nels can answer "what's shared with
me" and "list my budgets" (now including shared ones, in a separate section) correctly.

**Architecture:** Extract the inline owner-scoped query + string-formatting block in
`chat_endpoint` into two directly-testable functions — `fetch_budgets_context_rows` (a single
`UNION ALL` SQL query returning owned ∪ shared budgets, tagged `is_owner` +
`permission_level` + `owner_name`) and `format_budgets_context` (a pure formatter). Update the
`chat_endpoint` call site to use them, and rewrite rule 11's prompt text to explain the new
`(shared by X, Y access)` annotation and the "shared with me" vs. "list my budgets" branching.
No REST/API/migration changes — this is chat-context-only, mirroring `list_budgets`'s
owned+shared merge behaviorally (not literally — that function uses two Rust-merged queries,
this uses one `UNION ALL`).

**Tech Stack:** Rust, axum, sqlx (PostgreSQL), no new dependencies.

**Full spec:** `docs/superpowers/specs/2026-07-03-shared-budgets-display-design.md`

---

## Repo-specific reminders (apply to every task below)

- Test command: `cd backend && cargo test` (offline/unit tests run with no DB). DB-integration
  tests are `#[ignore]`d and need Postgres up first: `podman-compose up -d` (from repo root),
  then `cd backend && cargo test -- --ignored` (or a name filter, e.g. `cargo test
  budgets_context -- --ignored`).
- Compile check: `cd backend && cargo check`. No `clippy` step is documented for this repo —
  don't add one.
- `DATABASE_URL` defaults to `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`
  when unset (matches every existing DB-integration test's fallback).
- No migration needed — `budget_shares` and `users.name` already exist.
- Commit format: Conventional Commits scoped by issue, e.g. `feat(#231): <subject>` (matches
  this repo's git log, e.g. `feat(#233): router-driven main content outlet ... (#236)`).
- All work happens in `backend/src/rag.rs` only.

---

### Task 1: Extract `BudgetContextRow` + `format_budgets_context`, unit-test the formatter

**Files:**
- Modify: `backend/src/rag.rs` (add struct + pure fn near the top-level helper functions,
  e.g. right before `chat_endpoint`, and add unit tests inside the existing `#[cfg(test)] mod
  tests` block starting at `backend/src/rag.rs:4746`)

This task builds the pure, DB-free formatting logic first (fastest TDD loop — no Postgres
needed) so Task 2 can wire real rows through it with confidence.

- [ ] **Step 1: Write the failing unit tests**

Add these tests inside `mod tests` (anywhere among the existing `#[test]` functions, e.g.
right after `owner_only_actions_reject_non_owners` — no DB required, these are plain
`#[test]`, not `#[tokio::test]`):

```rust
    #[test]
    fn format_budgets_context_empty_is_none_yet() {
        let rows: Vec<BudgetContextRow> = vec![];
        assert_eq!(format_budgets_context(&rows), "USER'S BUDGETS: (none yet)");
    }

    #[test]
    fn format_budgets_context_owned_active_and_archived() {
        // Regression: owned-row labeling must stay byte-for-byte identical to
        // today's behavior for a user with no shares.
        let rows = vec![
            BudgetContextRow {
                id: uuid::Uuid::new_v4(),
                name: "Vacation".to_string(),
                is_default: true,
                archived_at: None,
                is_owner: true,
                permission_level: None,
                owner_name: None,
            },
            BudgetContextRow {
                id: uuid::Uuid::new_v4(),
                name: "Old Project".to_string(),
                is_default: false,
                archived_at: Some(chrono::Utc::now()),
                is_owner: true,
                permission_level: None,
                owner_name: None,
            },
        ];
        assert_eq!(
            format_budgets_context(&rows),
            "USER'S BUDGETS: Vacation (active), Old Project (archived)"
        );
    }

    #[test]
    fn format_budgets_context_shared_row_labels_owner_and_permission() {
        let rows = vec![BudgetContextRow {
            id: uuid::Uuid::new_v4(),
            name: "Groceries".to_string(),
            is_default: false,
            archived_at: None,
            is_owner: false,
            permission_level: Some("view".to_string()),
            owner_name: Some("Alice".to_string()),
        }];
        assert_eq!(
            format_budgets_context(&rows),
            "USER'S BUDGETS: Groceries (shared by Alice, view access)"
        );
    }

    #[test]
    fn format_budgets_context_shared_row_never_labeled_active_even_if_is_default_true() {
        // AC: a shared budget that happens to be the OWNER's is_default must not
        // render "(active)" from the viewer's perspective.
        let rows = vec![BudgetContextRow {
            id: uuid::Uuid::new_v4(),
            name: "Household".to_string(),
            is_default: true, // owner's own default flag, NOT the viewer's
            archived_at: None,
            is_owner: false,
            permission_level: Some("edit".to_string()),
            owner_name: Some("Bob".to_string()),
        }];
        let out = format_budgets_context(&rows);
        assert!(!out.contains("(active)"), "shared row must never render (active), got: {out}");
        assert_eq!(out, "USER'S BUDGETS: Household (shared by Bob, edit access)");
    }

    #[test]
    fn format_budgets_context_shared_row_archived_by_owner() {
        let rows = vec![BudgetContextRow {
            id: uuid::Uuid::new_v4(),
            name: "Old Shared".to_string(),
            is_default: false,
            archived_at: Some(chrono::Utc::now()),
            is_owner: false,
            permission_level: Some("view".to_string()),
            owner_name: Some("Alice".to_string()),
        }];
        assert_eq!(
            format_budgets_context(&rows),
            "USER'S BUDGETS: Old Shared (shared by Alice, view access) (archived)"
        );
    }

    #[test]
    fn format_budgets_context_shared_row_fallbacks_for_missing_name_and_level() {
        let rows = vec![BudgetContextRow {
            id: uuid::Uuid::new_v4(),
            name: "Mystery".to_string(),
            is_default: false,
            archived_at: None,
            is_owner: false,
            permission_level: None,
            owner_name: None,
        }];
        assert_eq!(
            format_budgets_context(&rows),
            "USER'S BUDGETS: Mystery (shared by someone, view access)"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail (compile error — the types don't exist yet)**

Run: `cd backend && cargo test format_budgets_context`
Expected: FAIL to compile — `cannot find struct/function 'BudgetContextRow'/'format_budgets_context' in this scope`

- [ ] **Step 3: Add the struct and the pure formatter**

Add this near the top of `backend/src/rag.rs`, close to the other small free functions used
by `chat_endpoint` (e.g. right before the `chat_endpoint` function definition — search for
`pub async fn chat_endpoint` to find it):

```rust
/// One row of the "USER'S BUDGETS" chat context: either a budget the caller owns, or
/// one shared with their email by another user (#231). `permission_level`/`owner_name`
/// are only populated for shared rows (`is_owner == false`); they are `None` for owned
/// rows, where they are meaningless.
struct BudgetContextRow {
    #[allow(dead_code)] // id is fetched for parity with the REST list shape; unused by the formatter today
    id: uuid::Uuid,
    name: String,
    is_default: bool,
    archived_at: Option<DateTime<Utc>>,
    is_owner: bool,
    permission_level: Option<String>,
    owner_name: Option<String>,
}

/// Formats the "USER'S BUDGETS" prompt block from a caller's owned + shared budgets
/// (#231). Owned-row labeling is byte-for-byte identical to the pre-#231 behavior
/// ("(active)" iff it's the caller's own is_default budget, "(archived)" iff archived).
/// Shared rows never render "(active)" — is_default on a shared row reflects the
/// OWNER's own default flag, not the viewer's, so it must never leak into the viewer's
/// context as if it were their active budget. Shared rows instead render
/// "(shared by OWNER, LEVEL access)", falling back to "someone"/"view" if the owner has
/// no name on file or the permission level is unexpectedly missing.
fn format_budgets_context(rows: &[BudgetContextRow]) -> String {
    if rows.is_empty() {
        return "USER'S BUDGETS: (none yet)".to_string();
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut label = r.name.clone();
            if r.is_owner {
                if r.is_default {
                    label.push_str(" (active)");
                }
            } else {
                let owner = r.owner_name.as_deref().unwrap_or("someone");
                let level = r.permission_level.as_deref().unwrap_or("view");
                label.push_str(&format!(" (shared by {owner}, {level} access)"));
            }
            if r.archived_at.is_some() {
                label.push_str(" (archived)");
            }
            label
        })
        .collect();
    format!("USER'S BUDGETS: {}", parts.join(", "))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test format_budgets_context`
Expected: PASS (6 tests: `format_budgets_context_empty_is_none_yet`,
`format_budgets_context_owned_active_and_archived`,
`format_budgets_context_shared_row_labels_owner_and_permission`,
`format_budgets_context_shared_row_never_labeled_active_even_if_is_default_true`,
`format_budgets_context_shared_row_archived_by_owner`,
`format_budgets_context_shared_row_fallbacks_for_missing_name_and_level`)

- [ ] **Step 5: Compile-check the whole crate (the struct has an unused field/dead-code lint risk)**

Run: `cd backend && cargo check`
Expected: no warnings/errors (the `#[allow(dead_code)]` on `id` prevents an unused-field
warning; `id` is intentionally kept for shape-parity with `list_budgets`'s `BudgetListItem`
even though the formatter doesn't read it).

- [ ] **Step 6: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "feat(#231): add BudgetContextRow + pure budgets-context formatter"
```

---

### Task 2: Add `fetch_budgets_context_rows` and wire it into `chat_endpoint`

**Files:**
- Modify: `backend/src/rag.rs:659` (the `budget_rows` query + `budgets_context` block inside
  `chat_endpoint`) and the `mod tests` block (new `#[tokio::test]` `#[ignore]` DB tests)

This task adds the real SQL (a `UNION ALL` of the shared-with-me budgets and the caller's
owned budgets) and replaces the inline block in `chat_endpoint` with calls to
`fetch_budgets_context_rows` + `format_budgets_context` (from Task 1).

- [ ] **Step 1: Write the failing DB-integration tests**

Add these inside `mod tests`, near `chat_share_budget_is_owner_only` (search for that name to
find the right neighborhood, around `backend/src/rag.rs:5619`). Follow its exact harness
style: unique UUIDs per run, explicit seed + cleanup, `#[ignore]`, real Postgres via
`DATABASE_URL` (fallback `postgres://postgres:postgrespassword@localhost:6153/budget_rag`).

```rust
    // DB-integration coverage for #231: the chat context now includes budgets shared
    // with the caller, not just owned ones. Runs only on demand:
    //   podman-compose up -d
    //   cd backend && cargo test budgets_context -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn budgets_context_rows_shared_only_user_sees_no_active_tag() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let owner_id = uuid::Uuid::new_v4();
        let viewer_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let viewer_email = format!("viewer-{suffix}@example.test");

        sqlx::query("INSERT INTO users (id, email, totp_secret, name) VALUES ($1, $2, 'test-secret', 'Alice')")
            .bind(owner_id)
            .bind(format!("owner-{suffix}@example.test"))
            .execute(&pool).await.expect("seed owner");
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(viewer_id)
            .bind(&viewer_email)
            .execute(&pool).await.expect("seed viewer");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'Groceries', 'monthly', 500.0, TRUE)",
        )
        .bind(budget_id).bind(owner_id)
        .execute(&pool).await.expect("seed budget");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(&viewer_email)
        .execute(&pool).await.expect("seed share");

        let rows = fetch_budgets_context_rows(&pool, viewer_id, &viewer_email)
            .await
            .expect("fetch rows");
        let ctx = format_budgets_context(&rows);

        assert_eq!(rows.len(), 1, "viewer owns nothing, sees only the shared budget");
        assert!(ctx.contains("Groceries (shared by Alice, view access)"), "got: {ctx}");
        assert!(!ctx.contains("(active)"), "shared-only viewer must see no (active) tag, got: {ctx}");

        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&[owner_id, viewer_id][..])
            .execute(&pool).await.expect("cleanup");
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn budgets_context_rows_owned_and_shared_separates_active_from_shared() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let owner_id = uuid::Uuid::new_v4();
        let viewer_id = uuid::Uuid::new_v4();
        let shared_budget_id = uuid::Uuid::new_v4();
        let own_budget_id = uuid::Uuid::new_v4();
        let viewer_email = format!("viewer2-{suffix}@example.test");

        sqlx::query("INSERT INTO users (id, email, totp_secret, name) VALUES ($1, $2, 'test-secret', 'Bob')")
            .bind(owner_id)
            .bind(format!("owner2-{suffix}@example.test"))
            .execute(&pool).await.expect("seed owner");
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(viewer_id)
            .bind(&viewer_email)
            .execute(&pool).await.expect("seed viewer");

        // The OWNER's own budget, also the OWNER's own is_default = TRUE. This is the
        // key regression case: the viewer must NOT see this as their own "(active)".
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'Household', 'monthly', 1000.0, TRUE)",
        )
        .bind(shared_budget_id).bind(owner_id)
        .execute(&pool).await.expect("seed shared budget");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(uuid::Uuid::new_v4()).bind(shared_budget_id).bind(&viewer_email)
        .execute(&pool).await.expect("seed share");

        // The VIEWER's own budget, their own default.
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'My Budget', 'monthly', 200.0, TRUE)",
        )
        .bind(own_budget_id).bind(viewer_id)
        .execute(&pool).await.expect("seed own budget");

        let rows = fetch_budgets_context_rows(&pool, viewer_id, &viewer_email)
            .await
            .expect("fetch rows");
        let ctx = format_budgets_context(&rows);

        assert_eq!(rows.len(), 2);
        assert!(ctx.contains("My Budget (active)"), "own budget must show (active), got: {ctx}");
        assert!(ctx.contains("Household (shared by Bob, edit access)"), "got: {ctx}");
        assert!(
            !ctx.contains("Household (shared by Bob, edit access) (active)"),
            "shared row must never carry (active) even though its OWNER has is_default=TRUE, got: {ctx}"
        );

        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&[owner_id, viewer_id][..])
            .execute(&pool).await.expect("cleanup");
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn budgets_context_rows_no_shares_matches_owned_only_shape() {
        // Regression: a user with zero shares gets exactly the pre-#231 shape/content.
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let owner_id = uuid::Uuid::new_v4();
        let owner_email = format!("solo-{suffix}@example.test");
        let budget_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(owner_id)
            .bind(&owner_email)
            .execute(&pool).await.expect("seed owner");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'Solo Budget', 'monthly', 300.0, TRUE)",
        )
        .bind(budget_id).bind(owner_id)
        .execute(&pool).await.expect("seed budget");

        let rows = fetch_budgets_context_rows(&pool, owner_id, &owner_email)
            .await
            .expect("fetch rows");
        let ctx = format_budgets_context(&rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(ctx, "USER'S BUDGETS: Solo Budget (active)");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner_id)
            .execute(&pool).await.expect("cleanup");
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn budgets_context_rows_shared_archived_by_owner() {
        // Exercises the real SQL path for an archived budget shared with the caller:
        // both UNION ALL branches select archived_at identically, but this proves the
        // shared branch's join doesn't drop or misroute it.
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let owner_id = uuid::Uuid::new_v4();
        let viewer_id = uuid::Uuid::new_v4();
        let budget_id = uuid::Uuid::new_v4();
        let viewer_email = format!("viewer3-{suffix}@example.test");

        sqlx::query("INSERT INTO users (id, email, totp_secret, name) VALUES ($1, $2, 'test-secret', 'Carol')")
            .bind(owner_id)
            .bind(format!("owner3-{suffix}@example.test"))
            .execute(&pool).await.expect("seed owner");
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(viewer_id)
            .bind(&viewer_email)
            .execute(&pool).await.expect("seed viewer");
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default, archived_at) \
             VALUES ($1, $2, 'Old Shared', 'monthly', 400.0, FALSE, NOW())",
        )
        .bind(budget_id).bind(owner_id)
        .execute(&pool).await.expect("seed archived shared budget");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(uuid::Uuid::new_v4()).bind(budget_id).bind(&viewer_email)
        .execute(&pool).await.expect("seed share");

        let rows = fetch_budgets_context_rows(&pool, viewer_id, &viewer_email)
            .await
            .expect("fetch rows");
        let ctx = format_budgets_context(&rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(ctx, "USER'S BUDGETS: Old Shared (shared by Carol, view access) (archived)");

        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&[owner_id, viewer_id][..])
            .execute(&pool).await.expect("cleanup");
    }

    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn budgets_context_rows_no_budgets_at_all_is_none_yet() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let user_id = uuid::Uuid::new_v4();
        let email = format!("empty-{suffix}@example.test");
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
            .bind(user_id)
            .bind(&email)
            .execute(&pool).await.expect("seed user");

        let rows = fetch_budgets_context_rows(&pool, user_id, &email)
            .await
            .expect("fetch rows");
        assert!(rows.is_empty());
        assert_eq!(format_budgets_context(&rows), "USER'S BUDGETS: (none yet)");

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool).await.expect("cleanup");
    }
```

- [ ] **Step 2: Run the tests to verify they fail (compile error — `fetch_budgets_context_rows` doesn't exist yet)**

Run: `cd backend && cargo test budgets_context_rows`
Expected: FAIL to compile — `cannot find function 'fetch_budgets_context_rows' in this scope`

- [ ] **Step 3: Add `fetch_budgets_context_rows`**

Add this function directly above `format_budgets_context` (from Task 1) in `backend/src/rag.rs`:

```rust
/// Fetches the caller's owned budgets UNION ALL the budgets shared with their email
/// (#231), each tagged with enough to format the "USER'S BUDGETS" chat context: the
/// shared branch resolves the owner's display name and the granted permission level;
/// the owned branch fills those two columns with an explicit NULL cast (there is no
/// existing UNION ALL elsewhere in this codebase, so both NULL literals are cast to
/// match their counterpart column's real type — Postgres cannot otherwise infer a type
/// for a bare NULL in a UNION branch).
async fn fetch_budgets_context_rows(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    user_email: &str,
) -> Result<Vec<BudgetContextRow>, sqlx::Error> {
    let query_rows = sqlx::query(
        "SELECT b.id, b.name, b.is_default, b.archived_at,
                FALSE AS is_owner, bs.permission_level AS permission_level,
                u.name AS owner_name
         FROM budgets b
         JOIN budget_shares bs ON bs.budget_id = b.id
         JOIN users u ON u.id = b.owner_id
         WHERE bs.shared_with_email = $1
         UNION ALL
         SELECT b.id, b.name, b.is_default, b.archived_at,
                TRUE AS is_owner, NULL::VARCHAR(50) AS permission_level,
                NULL::TEXT AS owner_name
         FROM budgets b WHERE b.owner_id = $2
         ORDER BY name ASC",
    )
    .bind(user_email)
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(query_rows
        .iter()
        .map(|r| BudgetContextRow {
            id: r.get("id"),
            name: r.get("name"),
            is_default: r.get("is_default"),
            archived_at: r.get("archived_at"),
            is_owner: r.get("is_owner"),
            permission_level: r.get("permission_level"),
            owner_name: r.get("owner_name"),
        })
        .collect())
}
```

- [ ] **Step 4: Run the new DB tests to verify they pass (requires Postgres up)**

Run:
```bash
podman-compose up -d
cd backend && cargo test budgets_context_rows -- --ignored
```
Expected: PASS (5 tests: `budgets_context_rows_shared_only_user_sees_no_active_tag`,
`budgets_context_rows_owned_and_shared_separates_active_from_shared`,
`budgets_context_rows_no_shares_matches_owned_only_shape`,
`budgets_context_rows_shared_archived_by_owner`,
`budgets_context_rows_no_budgets_at_all_is_none_yet`)

If Postgres is unavailable in this environment, at minimum confirm `cargo check` compiles
cleanly (Step 6 below) and note in your report that the `--ignored` DB tests could not be
run locally; they will run at the same checkout in any environment with Postgres reachable.

- [ ] **Step 5: Replace the inline query/formatting block in `chat_endpoint` with the new functions**

In `backend/src/rag.rs`, find this block (originally around line 659, comment included):

```rust
    let budget_rows = sqlx::query("SELECT id, name, is_default, archived_at FROM budgets WHERE owner_id = $1 ORDER BY name ASC")
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?;

    let budgets_context = if budget_rows.is_empty() {
        "USER'S BUDGETS: (none yet)".to_string()
    } else {
        let mut s = String::from("USER'S BUDGETS: ");
        let parts: Vec<String> = budget_rows
            .iter()
            .map(|r| {
                let n: String = r.get("name");
                let is_def: bool = r.get("is_default");
                let archived: Option<DateTime<Utc>> = r.get("archived_at");
                let mut label = n;
                if is_def { label.push_str(" (active)"); }
                if archived.is_some() { label.push_str(" (archived)"); }
                label
            })
            .collect();
        s.push_str(&parts.join(", "));
        s
    };
```

Replace it with:

```rust
    // Budgets the caller owns UNION ALL budgets shared with their email (#231) — both
    // annotated in the prompt context so Nels can answer "what's shared with me" and
    // "list my budgets" (which now surfaces shared ones too) from deterministic data.
    let budget_context_rows = fetch_budgets_context_rows(&state.db, user_id, &user_email)
        .await
        .map_err(internal_error)?;
    let budgets_context = format_budgets_context(&budget_context_rows);
```

Note: `user_email` is already resolved earlier in `chat_endpoint` (the `let user_email:
String = user_row.get("email");` line a few lines above this block) — do not re-fetch it.

- [ ] **Step 6: Compile and run the full non-ignored suite**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean; all pre-existing non-`#[ignore]`d tests still pass (this is a
regression check — the call site change must not alter compile-time behavior for any other
code path).

- [ ] **Step 7: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "feat(#231): include shared budgets in chat context via fetch_budgets_context_rows"
```

---

### Task 3: Rewrite prompt rule 11 for shared budgets

**Files:**
- Modify: `backend/src/rag.rs:1284` (inside the big `system_instructions` `format!` call)

No test is possible for LLM free-text generation quality in this repo (confirmed: no
existing rule has an LLM-output test) — this task is a prompt-text edit, verified by
`cargo check` (the `format!` string must still compile) and a careful proofread against the
acceptance criteria.

- [ ] **Step 1: Locate the current rule 11 line**

In `backend/src/rag.rs`, find (originally line 1284):

```rust
         11. If the user asks what budgets they have or to list their budgets, set 'action' to 'LIST_BUDGETS' and enumerate them in 'response_text' using the USER'S BUDGETS context above (mark which is active, and mark which are archived). If the user specifically asks to see their ARCHIVED budgets (e.g. 'show my archived budgets', 'what have I archived', 'list archived budgets'), still set 'action' to 'LIST_BUDGETS' but enumerate ONLY the budgets annotated '(archived)' in the USER'S BUDGETS context (say none are archived if the list has no '(archived)' entries); these are hidden from the main list but their data is preserved and they can be unarchived.\n\
```

- [ ] **Step 2: Replace it with the extended instruction**

```rust
         11. If the user asks what budgets they have or to list their budgets, set 'action' to 'LIST_BUDGETS'. The USER'S BUDGETS context above lists budgets you OWN — annotated '(active)' if it's your own default budget, '(archived)' if archived — and budgets OTHERS have shared with you, annotated '(shared by OWNER_NAME, LEVEL access)' (LEVEL is 'view' or 'edit'), plus '(archived)' too if the owner has archived it. Never label a shared entry '(active)' — that annotation reflects only YOUR OWN default budget, never the owner's. If the user specifically asks what has been SHARED WITH THEM (e.g. 'show budgets shared with me', 'what's been shared with me'), enumerate in 'response_text' ONLY the entries annotated '(shared by ...)' — name each budget, who shared it, and its access level; if none are annotated '(shared by ...)', say plainly that nothing has been shared with them yet (do NOT give a flat refusal — you DO have this capability). Otherwise, when they ask generally to list their budgets, enumerate BOTH groups clearly separated, e.g. 'Yours: ...' then 'Shared with you: ...' (omit the 'Shared with you' section entirely if there are no shared entries). If the user specifically asks to see their ARCHIVED budgets (e.g. 'show my archived budgets', 'what have I archived', 'list archived budgets'), still set 'action' to 'LIST_BUDGETS' but enumerate ONLY the budgets annotated '(archived)' in the USER'S BUDGETS context — this includes both your own and shared archived budgets (say none are archived if the list has no '(archived)' entries); these are hidden from the main list but their data is preserved, and only their owner can unarchive them.\n\
```

(Same trailing `\n\` line-continuation style as every other numbered rule in this `format!`
string — do not drop it, or the next rule's text will be swallowed into this line.)

- [ ] **Step 3: Compile-check**

Run: `cd backend && cargo check`
Expected: builds clean (this confirms the `format!` string literal is still well-formed —
unescaped `{`/`}` or a dropped `\n\` would be a compile error or a visibly malformed prompt).

- [ ] **Step 4: Run the full test suite one more time**

Run: `cd backend && cargo test`
Expected: all non-`#[ignore]`d tests pass (no test asserts on this string's exact content,
confirmed by `grep -n "rule 11" backend/src/rag.rs` returning nothing outside this edit, so
no regression is expected — this run is a final sanity check).

- [ ] **Step 5: Commit**

```bash
cd backend
git add src/rag.rs
git commit -m "feat(#231): extend LIST_BUDGETS prompt rule for shared budgets"
```

---

## Post-plan notes (for the implementer's final report, not a task)

- The out-of-scope `POST /budgets/:id/default` owner_id-guard bug (flagged in the ticket as
  a separate, standalone issue) is NOT part of this plan. Do not fix it. If you want, mention
  it in the PR description as a callout (optional, matches the ticket's own framing), but do
  not open a new GitHub issue autonomously — that decision is left to the human reviewer.
- After Task 3, do a final read-through of the three tasks' combined diff against the spec's
  Goal & Success Criteria checklist before declaring the plan complete.
