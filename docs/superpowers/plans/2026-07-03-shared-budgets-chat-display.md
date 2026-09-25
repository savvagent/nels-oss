# Display Budgets Shared With User (Chat) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Nels answer "show budgets that have been shared with me" (and continue to correctly answer "list my budgets") by including budgets shared with the caller's email in the chat prompt context, tagged with who shared them and at what permission level.

**Architecture:** Extract the inline budget-listing query in `chat_endpoint` (`backend/src/rag.rs:659-683`) into a standalone, unit-testable async function `build_budgets_context` that unions the caller's owned budgets with budgets shared with their email (joined to `budget_shares` and `users` for the sharer's name), then formats them into two labeled sections ("USER'S BUDGETS" / "SHARED WITH USER"). Update system prompt rule 11 so the LLM knows to read the new section. No new chat action, no dispatch/mutation arm, no frontend change — this is a read-only context/prompt change, matching how `LIST_BUDGETS` already works today.

**Tech Stack:** Rust, Axum, sqlx (Postgres + pgvector), `#[ignore]`d integration tests against a local `podman-compose` Postgres.

---

## Reference: GitHub issues

- Feature: https://github.com/savvagent/nels/issues/231
- Related (explicitly out of scope, filed separately): https://github.com/savvagent/nels/issues/238

## Scope Check

Single subsystem (the chat context builder in `rag.rs`), single file touched for production code, one new test. Not decomposed further.

---

### Task 1: Extend chat context to include shared budgets

**Files:**
- Modify: `backend/src/rag.rs:655-685` (replace inline query/formatting with a call to a new function)
- Modify: `backend/src/rag.rs:1284` (prompt rule 11 text)
- Create (new fn in same file): `backend/src/rag.rs` — `build_budgets_context`
- Test: `backend/src/rag.rs` (`mod tests`, alongside `chat_share_budget_is_owner_only`)

- [ ] **Step 1: Write the failing test**

Add this test inside `mod tests` in `backend/src/rag.rs` (place it near `chat_share_budget_is_owner_only`, e.g. directly after it):

```rust
    // Verifies build_budgets_context (#231) surfaces budgets shared with the
    // caller's email in a distinct "SHARED WITH USER" section, tagged with the
    // sharer's name and permission level, and never leaks a shared budget's
    // owner-scoped is_default flag as "(active)" to the recipient.
    //
    // Runs only on demand against the local docker-compose Postgres:
    //   podman-compose up -d
    //   cd backend && cargo test -- --ignored
    #[tokio::test]
    #[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
    async fn build_budgets_context_shows_shared_budgets() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let suffix = uuid::Uuid::new_v4();
        let owner_id = uuid::Uuid::new_v4();
        let recipient_id = uuid::Uuid::new_v4();
        let owner_email = format!("owner-{suffix}@example.test");
        let recipient_email = format!("recipient-{suffix}@example.test");

        async fn seed_user(pool: &sqlx::PgPool, id: uuid::Uuid, email: &str, name: &str) {
            sqlx::query(
                "INSERT INTO users (id, email, totp_secret, name) VALUES ($1, $2, 'test-secret', $3)",
            )
            .bind(id)
            .bind(email)
            .bind(name)
            .execute(pool)
            .await
            .expect("seed user");
        }
        seed_user(&pool, owner_id, &owner_email, "Alice").await;
        seed_user(&pool, recipient_id, &recipient_email, "Bob").await;

        // Recipient has no budgets of their own yet, and nothing shared: falls
        // back to the original "(none yet)" message unchanged.
        let ctx = build_budgets_context(&pool, recipient_id, &recipient_email)
            .await
            .expect("build context (empty)");
        assert_eq!(ctx, "USER'S BUDGETS: (none yet)");

        // Recipient's own budget, marked active for them.
        let own_budget_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'My Budget', 'monthly', 500.0, TRUE)",
        )
        .bind(own_budget_id)
        .bind(recipient_id)
        .execute(&pool)
        .await
        .expect("seed recipient's own budget");

        // Owner's budget, shared with the recipient as 'edit'. ALSO marked
        // is_default = TRUE for the owner, to prove the recipient never sees
        // it labeled "(active)" — that flag is scoped to the owner, not the
        // viewer.
        let shared_budget_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
             VALUES ($1, $2, 'Team Trip', 'monthly', 2000.0, TRUE)",
        )
        .bind(shared_budget_id)
        .bind(owner_id)
        .execute(&pool)
        .await
        .expect("seed owner's shared budget");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(shared_budget_id)
        .bind(&recipient_email)
        .execute(&pool)
        .await
        .expect("seed share");

        let ctx = build_budgets_context(&pool, recipient_id, &recipient_email)
            .await
            .expect("build context (owned + shared)");

        assert!(
            ctx.contains("My Budget (active)"),
            "recipient's own default budget must be marked active, got: {ctx}"
        );
        assert!(
            ctx.contains("SHARED WITH USER: Team Trip (shared by Alice, edit access)"),
            "shared budget must be listed with owner name + permission level, got: {ctx}"
        );
        assert!(
            !ctx.contains("Team Trip (active)"),
            "a shared budget's owner-scoped is_default must never render as active to the recipient, got: {ctx}"
        );

        // Cleanup in FK order: shares -> budgets -> users.
        sqlx::query("DELETE FROM budget_shares WHERE budget_id = $1")
            .bind(shared_budget_id)
            .execute(&pool)
            .await
            .expect("cleanup shares");
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(own_budget_id)
            .execute(&pool)
            .await
            .expect("cleanup own budget");
        sqlx::query("DELETE FROM budgets WHERE id = $1")
            .bind(shared_budget_id)
            .execute(&pool)
            .await
            .expect("cleanup shared budget");
        for id in [owner_id, recipient_id] {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .expect("cleanup user");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test build_budgets_context_shows_shared_budgets -- --ignored`
Expected: compile error — `error[E0425]: cannot find function `build_budgets_context` in this scope` (the function doesn't exist yet).

- [ ] **Step 3: Implement `build_budgets_context`**

Add this function in `backend/src/rag.rs`, directly above `pub async fn chat_endpoint`:

```rust
/// Builds the "USER'S BUDGETS: ..." context string injected into the chat
/// prompt. Includes the caller's own budgets plus any budgets shared with
/// their email (#231), in a separate "SHARED WITH USER" section labeled with
/// who shared each one and their permission level. A shared budget's
/// `is_default` flag is owner-scoped (see `unique_default_budget_per_user`)
/// and is intentionally never surfaced as "(active)" to a non-owner viewer.
async fn build_budgets_context(
    db: &sqlx::PgPool,
    user_id: Uuid,
    user_email: &str,
) -> Result<String, sqlx::Error> {
    let budget_rows = sqlx::query(
        "SELECT b.name, b.is_default, b.archived_at, \
                FALSE AS is_shared, NULL::text AS owner_name, NULL::text AS permission_level \
         FROM budgets b \
         WHERE b.owner_id = $1 \
         UNION ALL \
         SELECT b.name, b.is_default, b.archived_at, \
                TRUE AS is_shared, u.name AS owner_name, bs.permission_level AS permission_level \
         FROM budgets b \
         JOIN budget_shares bs ON bs.budget_id = b.id \
         JOIN users u ON u.id = b.owner_id \
         WHERE bs.shared_with_email = $2 \
         ORDER BY name ASC",
    )
    .bind(user_id)
    .bind(user_email)
    .fetch_all(db)
    .await?;

    let mut owned_parts: Vec<String> = Vec::new();
    let mut shared_parts: Vec<String> = Vec::new();

    for r in &budget_rows {
        let n: String = r.get("name");
        let is_default: bool = r.get("is_default");
        let archived: Option<DateTime<Utc>> = r.get("archived_at");
        let is_shared: bool = r.get("is_shared");
        let mut label = n;
        if !is_shared && is_default {
            label.push_str(" (active)");
        }
        if archived.is_some() {
            label.push_str(" (archived)");
        }
        if is_shared {
            let owner_name: Option<String> = r.get("owner_name");
            let permission_level: Option<String> = r.get("permission_level");
            let owner_label = owner_name.unwrap_or_else(|| "someone".to_string());
            let perm_label = permission_level.unwrap_or_else(|| "view".to_string());
            label.push_str(&format!(" (shared by {owner_label}, {perm_label} access)"));
            shared_parts.push(label);
        } else {
            owned_parts.push(label);
        }
    }

    if owned_parts.is_empty() && shared_parts.is_empty() {
        return Ok("USER'S BUDGETS: (none yet)".to_string());
    }

    let owned_str = if owned_parts.is_empty() {
        "(none owned)".to_string()
    } else {
        owned_parts.join(", ")
    };
    let mut s = format!("USER'S BUDGETS: {owned_str}");
    if !shared_parts.is_empty() {
        s.push_str(&format!(". SHARED WITH USER: {}", shared_parts.join(", ")));
    }
    Ok(s)
}
```

- [ ] **Step 4: Replace the inline query/formatting with a call to the new function**

In `backend/src/rag.rs`, replace lines 659-683 (the `let budget_rows = ...` through the closing `};` of `budgets_context`) with:

```rust
    // Archived budgets stay in this list so the user can still name-match them
    // (e.g. "unarchive my vacation budget"), but are annotated "(archived)" so
    // the assistant doesn't present them as if they were on the main list (#50).
    // Shared budgets (owned by someone else, shared with this user's email) are
    // also included, in a separate "SHARED WITH USER" section, so Nels can
    // answer "what's been shared with me" (#231).
    let budgets_context = build_budgets_context(&state.db, user_id, &user_email)
        .await
        .map_err(internal_error)?;
```

(Keep the surrounding comment block above line 655 and the `// 3. Collect Database Context for RAG Prompt` line below unchanged — only the query/formatting body is replaced.)

- [ ] **Step 5: Run test to verify it passes**

Run: `podman-compose up -d && cd backend && cargo test build_budgets_context_shows_shared_budgets -- --ignored`
Expected: `test rag::tests::build_budgets_context_shows_shared_budgets ... ok`

- [ ] **Step 6: Update prompt rule 11 to describe the new section**

In `backend/src/rag.rs:1284`, replace the existing rule 11 line with:

```rust
         11. If the user asks what budgets they have or to list their budgets, set 'action' to 'LIST_BUDGETS' and enumerate the budgets under 'USER'S BUDGETS' in 'response_text' (mark which is active, and mark which are archived); if a 'SHARED WITH USER' section is also present, list those too under a clear separate heading (e.g. 'Shared with you:'), naming who shared each one and their access level exactly as given — do not call a shared budget 'active' even if it happens to be the sharer's own active budget. If the user specifically asks what has been shared with them (e.g. 'show budgets shared with me', 'what's been shared with me'), set 'action' to 'LIST_BUDGETS' but enumerate ONLY the entries in the 'SHARED WITH USER' section (if that section is absent, say plainly that nothing has been shared with them yet). If the user specifically asks to see their ARCHIVED budgets (e.g. 'show my archived budgets', 'what have I archived', 'list archived budgets'), still set 'action' to 'LIST_BUDGETS' but enumerate ONLY the budgets annotated '(archived)' in the USER'S BUDGETS context (say none are archived if the list has no '(archived)' entries); these are hidden from the main list but their data is preserved and they can be unarchived.\n\
```

- [ ] **Step 7: Confirm the whole crate still builds**

Run: `cd backend && cargo build`
Expected: clean build, no errors or new warnings.

- [ ] **Step 8: Run the full ignored test suite to check for regressions**

Run: `podman-compose up -d && cd backend && cargo test -- --ignored`
Expected: all tests pass, including the pre-existing `chat_share_budget_is_owner_only` and the new `build_budgets_context_shows_shared_budgets`.

- [ ] **Step 9: Manual end-to-end smoke test (real LLM, not the offline router)**

The offline/mock router (used when `GEMINI_API_KEY` is unset) has no keyword branch for budget listing, so the prompt-rule change can only be observed against the real Gemini model. With the backend running locally with a valid `GEMINI_API_KEY` and `DATABASE_URL` pointing at the podman-compose DB:

1. Seed two users via the normal signup flow (or reuse existing dev accounts), call them A (owner) and B (recipient).
2. As A, share one of A's budgets with B's email at `view` level (via chat: "share my budget with `<B's email>`" or `POST /budgets/:id/share`).
3. As B, send a chat message: `"show budgets that have been shared with me"`.
   Expected: response names A's budget, credits it to A by name, and states "view access" (or equivalent wording) — not the old refusal ("I don't have a way to display budgets that have been shared with you...").
4. As B, send: `"list my budgets"`.
   Expected: response lists B's own budgets, and separately mentions the budget shared by A.

- [ ] **Step 10: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(#231): surface budgets shared with the user in chat context"
```
