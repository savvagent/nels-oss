# Shared-Budget Activation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user switch to a budget shared with them from the Budgets list page, and make the whole app (chat writes, LLM context, insights strip) honor that per-viewer active budget.

**Architecture:** #255 already shipped the per-viewer preference (`users.active_budget_id`, `POST /budgets/:id/activate`, `is_active` on `GET /budgets`). Three backend sites and the list-page UI were never migrated off the owner-scoped `budgets.is_default`. Backend first (Tasks 1–3) so the switch lands on a working app, then the UI gate (Task 4) and error copy (Task 5).

**Tech Stack:** Rust / axum / sqlx / Postgres (backend); Svelte 5 runes / vitest / svelte-i18n (frontend).

## Global Constraints

- `POST /budgets/:id/activate` (`set_active_budget`, `backend/src/budget.rs:2323`) and `POST /budgets/:id/default` (`set_default_budget`, `:2271`) semantics are **unchanged** by this plan. A sharee must never mutate an owner's `is_default`.
- No DB migration. `users.active_budget_id` already exists (`backend/migrations/20260703013000_user_active_budget.sql`).
- Every user-visible string goes through `$_()` and must be added to **all six** locales: `frontend/src/lib/i18n/locales/{de,en,es,fr,it,pt}.json`. `i18nLocales.test.js` enforces key parity — a missing locale fails CI. Write real translations, not English placeholders.
- Backend integration tests are `#[ignore]`d and need local pgvector on port 6153. Run with `cargo test -- --ignored`.
- **Where backend DB tests live** (corrected 2026-07-21, supersedes the version posted on #391): each module already has its own DB test harness, so **new tests go in the module they cover** and **no visibility is widened** — `resolve_active_budget_id` and `owned_active_budgets` both stay private.
  - Task 1's tests → `backend/src/rag.rs`'s `mod tests`, which already holds `#[ignore]`d Postgres tests (`:9735-10100`) with inline seed/cleanup. Follow that file's existing seeding style.
  - Task 3's tests → `backend/src/insights.rs`'s `mod tests`, using its existing helpers `test_pool()` (`:604`), `seed_user()` (`:612`), and `seed_budget_with_spend()` (`:624`).
  - `budget.rs`'s `rollup_test_setup`/`seed_rollup_budget` are private to *its* test module and are **not** used by this plan. Any test code below written against them is illustrative — port it to the host module's own helpers.
- **`seed_rollup_budget(pool, owner, cat_limit, spent, archived)`** — the 5th argument is `archived`, **not** `is_default`, and the helper never sets `is_default` (schema default `FALSE`). Always pass `false` unless you genuinely want an archived budget, and set a default explicitly with `UPDATE budgets SET is_default = TRUE WHERE id = $1`.
- Frontend tests: `cd frontend && npm test`. `frontend/vitest.config.js` sets `environment: "node"` and the repo has **no** jsdom/testing-library dependency and no `.svelte` component tests. Spec §7's "BudgetsView component tests" and "App.svelte wiring assertion" are therefore **not buildable here** and are deliberately omitted: the ticket AC "tests cover: sharee can switch from the list page" is satisfied at the `canSwitchTo`/`switchErrorKey` pure-helper level plus the manual walkthrough in Task 6. Do not add a jsdom dependency to satisfy them.
- No self-attribution in commits.

---

### Task 1: Chat's null-`budget_id` fallback honors the viewer's active budget

**Files:**
- Modify: `backend/src/rag.rs:920-948` (the `if active_budget_id.is_none()` block in `chat_endpoint`)
- Test: `backend/src/rag.rs` `mod tests` (its existing `#[ignore]`d Postgres tests, `:9735-10100` — see Global Constraints)

**Interfaces:**
- Consumes: `check_permission(&PgPool, Uuid, Uuid) -> Result<Permission, String>` (`backend/src/budget.rs:372`), `Permission::{None, View, Edit, Owner}` (derives `PartialEq` at `budget.rs:363`).
- Produces: private `async fn resolve_active_budget_id(db: &PgPool, user_id: Uuid, requested: Option<Uuid>) -> Result<Option<Uuid>, (StatusCode, String)>` in `rag.rs`. `chat_endpoint`'s resolution order becomes: explicit `payload.budget_id` → `users.active_budget_id` (permission-checked) → owned `is_default` → first owned.

- [ ] **Step 1: Write the failing test**

Add to `backend/src/rag.rs`'s `mod tests`, alongside its existing `#[ignore]`d Postgres tests. The snippets below use `rollup_test_setup`/`seed_rollup_budget` illustratively — **port them to rag.rs's own inline seeding style** (see the tests at `:9735+` for the pattern), and call `resolve_active_budget_id` directly (same module, no path prefix):

```rust
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn chat_fallback_prefers_active_shared_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, sharee) = rollup_test_setup().await;

        // 5th arg is `archived`, NOT is_default — set is_default explicitly below.
        let shared = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let own = seed_rollup_budget(&pool, sharee, 50.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
            .bind(own).execute(&pool).await.expect("set sharee default");

        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(sharee).fetch_one(&pool).await.expect("sharee email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'edit')",
        )
        .bind(Uuid::new_v4()).bind(shared).bind(&email)
        .execute(&pool).await.expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared).bind(sharee)
            .execute(&pool).await.expect("set preference");

        let resolved = resolve_active_budget_id(&pool, sharee, None)
            .await.expect("resolve");
        assert_eq!(resolved, Some(shared), "must prefer the active shared budget over own is_default");
        assert_ne!(resolved, Some(own));
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn chat_fallback_ignores_revoked_active_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, sharee) = rollup_test_setup().await;

        let shared = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let own = seed_rollup_budget(&pool, sharee, 50.0, 0.0, false).await;
        sqlx::query("UPDATE budgets SET is_default = TRUE WHERE id = $1")
            .bind(own).execute(&pool).await.expect("set sharee default");

        // Preference set, but NO budget_shares row → access was revoked.
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared).bind(sharee)
            .execute(&pool).await.expect("set preference");

        let resolved = resolve_active_budget_id(&pool, sharee, None)
            .await.expect("resolve");
        assert_eq!(resolved, Some(own), "revoked share must fall through to the sharee's own default");
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn chat_fallback_honors_an_explicit_budget_id() {
        let (pool, user) = rollup_test_setup().await;
        let a = seed_rollup_budget(&pool, user, 10.0, 0.0, false).await;
        let b = seed_rollup_budget(&pool, user, 20.0, 0.0, false).await;
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(a).bind(user).execute(&pool).await.expect("set preference");

        let resolved = resolve_active_budget_id(&pool, user, Some(b))
            .await.expect("resolve");
        assert_eq!(resolved, Some(b), "an explicit request must win over the preference");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test chat_fallback -- --ignored`
Expected: FAIL to compile — `cannot find function 'resolve_active_budget_id' in this scope`.

- [ ] **Step 3: Extract and implement the resolver**

Add to `backend/src/rag.rs` above `chat_endpoint`:

```rust
/// Resolve which budget a chat request targets.
///
/// Order: the caller's explicit `requested` id → the caller's per-viewer
/// preference (`users.active_budget_id`, #255) when they still have access →
/// their own `is_default` budget → any budget they own.
///
/// The permission re-check on the preference is load-bearing: `active_budget_id`'s
/// FK is `ON DELETE SET NULL`, so it only clears when the BUDGET is deleted — a
/// revoked `budget_shares` row leaves the preference dangling, and without this
/// check chat would 403 on every message instead of falling back.
async fn resolve_active_budget_id(
    db: &PgPool,
    user_id: Uuid,
    requested: Option<Uuid>,
) -> Result<Option<Uuid>, (StatusCode, String)> {
    if requested.is_some() {
        return Ok(requested);
    }

    let preferred: Option<Uuid> =
        sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(db)
            .await
            .map_err(internal_error)?
            .flatten();

    if let Some(bid) = preferred {
        let perm = check_permission(db, user_id, bid)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if perm != Permission::None {
            return Ok(Some(bid));
        }
    }

    let default_row =
        sqlx::query("SELECT id FROM budgets WHERE owner_id = $1 AND is_default = TRUE")
            .bind(user_id)
            .fetch_optional(db)
            .await
            .map_err(internal_error)?;

    if let Some(row) = default_row {
        return Ok(Some(row.get("id")));
    }

    let first_row = sqlx::query("SELECT id FROM budgets WHERE owner_id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(internal_error)?;

    Ok(first_row.map(|row| row.get("id")))
}
```

Then replace the whole `let mut active_budget_id = payload.budget_id; … }` block at `rag.rs:920-948` with:

```rust
    // 1. Resolve Active Budget (#391: honors the per-viewer preference from #255)
    let mut active_budget_id =
        resolve_active_budget_id(&state.db, user_id, payload.budget_id).await?;
```

Leave the `active_time_frame` declaration and the `check_permission` block at `rag.rs:951+` exactly as they are. Add this comment above that existing block:

```rust
    // Verify access if budget was resolved. Redundant for the preference branch of
    // resolve_active_budget_id (which already checked), but NOT removable: the
    // explicit-payload.budget_id branch relies solely on this check.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test chat_fallback -- --ignored`
Expected: PASS (3 tests).

- [ ] **Step 5: Run the full backend suite**

Run: `cd backend && cargo test && cargo clippy -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#391): chat budget fallback honors the viewer's active_budget_id"
```

---

### Task 2: LLM prompt context marks the viewer's real active budget

**Files:**
- Modify: `backend/src/rag.rs:758-766` (`BudgetContextRow`'s `#[allow(dead_code)]` on `id`), `:787-800` (`format_budgets_context`), its one production call site (`:1030`), and **all sixteen existing test call sites** — nine pure-unit at `:8691, 8719, 8736, 8754, 8771, 8788, 8807, 8831, 8851` and seven `#[ignore]`d DB-backed at `:9764, 9825, 9868, 9919, 9949, 10028, 10085`
- Test: `backend/src/rag.rs`'s existing pure-unit test module (these are non-DB tests — they belong here, unlike Task 1's)

**Interfaces:**
- Consumes: `BudgetContextRow { id: uuid::Uuid, name: String, is_default: bool, archived_at: Option<DateTime<Utc>>, is_owner: bool, permission_level: Option<String>, owner_name: Option<String> }` (`rag.rs:758-766`) — exactly seven fields, and the struct does **not** derive `Default`, so every field must be written out. The `active_budget_id` resolved in Task 1.
- Produces: `fn format_budgets_context(rows: &[BudgetContextRow], active_budget_id: Option<Uuid>) -> String` — one extra parameter.

- [ ] **Step 1: Write the failing test**

Add to `rag.rs`'s existing `format_budgets_context` test module, next to the tests at `:8689+`:

```rust
    fn budget_context_row(
        id: Uuid,
        name: &str,
        is_owner: bool,
        is_default: bool,
    ) -> BudgetContextRow {
        // BudgetContextRow does NOT derive Default — every field is written out.
        BudgetContextRow {
            id,
            name: name.to_string(),
            is_default,
            archived_at: None,
            is_owner,
            permission_level: if is_owner { None } else { Some("edit".to_string()) },
            owner_name: if is_owner { None } else { Some("Owner".to_string()) },
        }
    }

    #[test]
    fn format_budgets_context_marks_active_shared_budget() {
        let shared_id = Uuid::new_v4();
        let own_id = Uuid::new_v4();
        let rows = vec![
            budget_context_row(own_id, "My Budget", true, true), // owned, is_default
            budget_context_row(shared_id, "Family Budget", false, false), // shared
        ];

        let out = format_budgets_context(&rows, Some(shared_id));

        assert!(out.contains("Family Budget (active)"), "active shared budget must be marked: {out}");
        assert!(!out.contains("My Budget (active)"), "owner's is_default must NOT be marked: {out}");
    }

    #[test]
    fn format_budgets_context_marks_active_owned_budget() {
        let own_id = Uuid::new_v4();
        let rows = vec![budget_context_row(own_id, "My Budget", true, true)];
        let out = format_budgets_context(&rows, Some(own_id));
        assert!(out.contains("My Budget (active)"), "{out}");
    }

    #[test]
    fn format_budgets_context_marks_nothing_when_no_active_budget() {
        let rows = vec![budget_context_row(Uuid::new_v4(), "My Budget", true, true)];
        let out = format_budgets_context(&rows, None);
        assert!(!out.contains("(active)"), "{out}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test format_budgets_context`
Expected: FAIL to compile — `this function takes 1 argument but 2 arguments were supplied`, reported at **seventeen** sites (the three new tests plus the sixteen pre-existing... i.e. the arity error fires at every call site once the signature changes in Step 3). That is expected; Step 5 fixes them all.

- [ ] **Step 3: Change the signature and the marker logic**

In `backend/src/rag.rs`, change the signature and replace the `is_owner`/`is_default` marker branch:

```rust
fn format_budgets_context(
    rows: &[BudgetContextRow],
    active_budget_id: Option<Uuid>,
) -> String {
    if rows.is_empty() {
        return "USER'S BUDGETS: (none yet)".to_string();
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut label = r.name.clone();
            // #391: "(active)" is the VIEWER's active budget (users.active_budget_id,
            // #255) — never `is_owner && is_default`, which mislabels a sharee's own
            // default as active while she is working in a shared budget. The id is
            // passed in (already resolved by resolve_active_budget_id) so the model's
            // view and the write target can never disagree; do not re-resolve here.
            if active_budget_id == Some(r.id) {
                label.push_str(" (active)");
            }
            if !r.is_owner {
                let owner = r
                    .owner_name
```

Keep the entire existing `else`-branch body (the shared-by/permission-label rendering) — it now runs under `if !r.is_owner` instead of as the `else` of the `is_owner` test. Re-indent, do not rewrite.

Update the doc comment above the function: replace any wording that says "(active)" tracks the owner's default with the per-viewer rule.

Also remove the now-false attribute and comment on the `id` field (`rag.rs:759`) — the formatter *does* read `id` now:

```rust
struct BudgetContextRow {
    id: uuid::Uuid,
```

- [ ] **Step 4: Update the production call site**

Find the single `format_budgets_context(` call in `chat_endpoint` and pass the resolved id:

```rust
    let budgets_context = format_budgets_context(&budget_context_rows, active_budget_id);
```

(`active_budget_id` is in scope from Task 1; use the variable name the surrounding code actually binds.)

- [ ] **Step 5: Update all sixteen pre-existing test call sites**

There are two groups. Do **not** blanket-append `, None` — five of the sixteen assert the old `is_default`-derived `(active)` output and would then fail (three of them only under `--ignored`, i.e. silently, until Task 6).

**Group A — pure-unit tests (`rag.rs:8691, 8719, 8736, 8754, 8771, 8788, 8807, 8831, 8851`).** Mechanical `, None` **except** these two, which expect `(active)`:
- `format_budgets_context_owned_active_and_archived` (`:8694`) — expects `"Vacation (active), Old Project (archived)"`.
- `format_budgets_context_owned_default_and_archived_same_row` (`:8832`) — expects `"Retired Default (active) (archived)"`.

**Group B — `#[ignore]`d DB-backed tests (`rag.rs:9764, 9825, 9868, 9919, 9949, 10028, 10085`).** Mechanical `, None` **except** these three, which expect `(active)`:
- `:9825` — `assert!(ctx.contains("My Budget (active)"))`
- `:9868` — `assert_eq!(ctx, "USER'S BUDGETS: Solo Budget (active)")`
- `:10028` — `assert_eq!(ctx, "…Zzz Mine (active)")`

These three already bind the owned budget's id in a local (`own_budget_id` or similar), so pass `Some(<that local>)`.

For the two Group-A exceptions, hoist the id of the row that should read as active into a local first:

```rust
        let vacation_id = Uuid::new_v4();
        let rows = vec![
            // …the row for "Vacation" now uses `vacation_id` instead of Uuid::new_v4()…
        ];
        let out = format_budgets_context(&rows, Some(vacation_id));
```

Keep every expected-output string byte-identical — all five should still pass, now for the right reason. **Do not weaken an assertion to make it compile or go green.**

- [ ] **Step 6: Run to verify it passes**

Run: `cd backend && cargo test format_budgets_context && cargo test format_budgets_context -- --ignored && cargo test && cargo clippy -- -D warnings`
Expected: PASS (19 call sites' worth: 16 existing + 3 new), no warnings. The `--ignored` run is **required here**, not deferred to Task 6 — it is the only thing that exercises the three Group-B semantic fixes.

- [ ] **Step 7: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#391): LLM context marks the viewer's active budget, not the owner's default"
```

---

### Task 3: Insights includes the viewer's active shared budget

**Files:**
- Modify: `backend/src/insights.rs:1-16` (module doc), `:267-293` (`owned_active_budgets`)
- Test: `backend/src/insights.rs` `mod tests`, using its existing `test_pool()` (`:604`), `seed_user()` (`:612`), `seed_budget_with_spend()` (`:624`) helpers — see Global Constraints

**Interfaces:**
- Consumes: `users.active_budget_id`, `budget_shares.shared_with_email`, `users.email`.
- Produces: `owned_active_budgets` keeps its name, signature, return type, and **private** visibility. Only its row set widens — by **at most one** row.

- [ ] **Step 1: Write the failing test**

Add to `backend/src/insights.rs`'s `mod tests`, using that module's own helpers. The snippets below use `rollup_test_setup`/`seed_rollup_budget` illustratively — **port them to `test_pool()`/`seed_user()`/`seed_budget_with_spend()`**, and match the surrounding tests' `#[ignore]` attribute (or absence of one) so the new tests run in the same pass as their neighbours:

```rust
    #[tokio::test]
    async fn owned_active_budgets_includes_only_the_active_shared_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;

        // 5th arg is `archived` — pass false, or the new archived_at filter excludes it.
        let own = seed_rollup_budget(&pool, viewer, 50.0, 0.0, false).await;
        let active_shared = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let other_shared = seed_rollup_budget(&pool, owner, 200.0, 0.0, false).await;

        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer).fetch_one(&pool).await.expect("viewer email");
        for bid in [active_shared, other_shared] {
            sqlx::query(
                "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
                 VALUES ($1, $2, $3, 'view')",
            )
            .bind(Uuid::new_v4()).bind(bid).bind(&email)
            .execute(&pool).await.expect("seed share");
        }

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(active_shared).bind(viewer)
            .execute(&pool).await.expect("set preference");

        let rows = owned_active_budgets(&pool, viewer).await.expect("query");
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

        assert!(ids.contains(&own), "own budget must still be included");
        assert!(ids.contains(&active_shared), "the ACTIVE shared budget must be included");
        assert!(
            !ids.contains(&other_shared),
            "a merely-shared budget must NOT leak into the viewer's insights totals"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn owned_active_budgets_excludes_an_archived_active_shared_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;

        let archived_shared = seed_rollup_budget(&pool, owner, 100.0, 0.0, true).await;
        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer).fetch_one(&pool).await.expect("viewer email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4()).bind(archived_shared).bind(&email)
        .execute(&pool).await.expect("seed share");
        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(archived_shared).bind(viewer)
            .execute(&pool).await.expect("set preference");

        let rows = owned_active_budgets(&pool, viewer).await.expect("query");
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        assert!(!ids.contains(&archived_shared), "archived_at IS NULL must apply to the shared row too");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test owned_active_budgets -- --ignored`
Expected: FAIL on `the ACTIVE shared budget must be included`.

- [ ] **Step 3: Widen the query**

Replace the SQL and doc comment in `owned_active_budgets` (visibility unchanged — it stays private):

```rust
/// Fetch the budgets that feed this user's insights: their own non-archived
/// budgets, plus — at most one extra row — the budget they have explicitly made
/// active (`users.active_budget_id`, #255) when that budget is shared with them
/// and non-archived (#391).
///
/// Deliberately NOT "every budget shared with me": this row set feeds
/// `budget_rollup`'s KPI aggregates, so a full union would fold everyone else's
/// budgets into the viewer's totals. The `archived_at IS NULL` filter is applied
/// to the shared row too — `list_budgets`' own share join does not filter archived.
async fn owned_active_budgets(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<OwnedBudgetRow>, (StatusCode, String)> {
    let rows = sqlx::query(
        "SELECT b.id, b.name, b.time_frame, b.budget_type, b.rollover_enabled, \
                b.created_at, b.closed_at \
         FROM budgets b \
         WHERE b.archived_at IS NULL \
           AND ( \
             b.owner_id = $1 \
             OR ( \
               b.id = (SELECT u.active_budget_id FROM users u WHERE u.id = $1) \
               AND EXISTS ( \
                 SELECT 1 FROM budget_shares bs \
                 JOIN users u2 ON u2.id = $1 \
                 WHERE bs.budget_id = b.id AND bs.shared_with_email = u2.email \
               ) \
             ) \
           ) \
         ORDER BY b.created_at",
    )
    .bind(user_id)
```

Leave the `.fetch_all(db)` / row-mapping tail unchanged.

- [ ] **Step 4: Update the module doc**

In `backend/src/insights.rs:1-16`, replace:

> Insights aggregates across every budget the user **owns** (`budgets.owner_id = user_id`); shared budgets are intentionally excluded so "a user sees only their own data" is unambiguous.

with:

```rust
//! Insights aggregates across every budget the user **owns**
//! (`budgets.owner_id = user_id`), plus the single budget they have explicitly
//! made active (`users.active_budget_id`, #255) when that budget is shared with
//! them (#391) — otherwise a sharee working in a shared budget would see an empty
//! status strip. Other budgets merely shared with the user remain excluded, so
//! their KPI totals stay "my data plus the one budget I chose to work in".
//!
//! Note the deliberate asymmetry: the trend and top-category aggregations below
//! remain strictly owner-scoped (`budgets.owner_id = $1`). Widening those raises
//! separate cross-tenant questions and is out of scope for #391 — so with a shared
//! budget active, the modal lists it while those two panels omit its history.
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd backend && cargo test owned_active_budgets -- --ignored && cargo test && cargo clippy -- -D warnings`
Expected: PASS (2 new tests), no warnings.

- [ ] **Step 6: Commit**

```bash
git add backend/src/insights.rs
git commit -m "fix(#391): insights include the viewer's active shared budget"
```

---

### Task 4: Budgets list page can switch to any accessible budget

**Files:**
- Modify: `frontend/src/lib/budgetsView.js:22-37` (`canSwitchTo` + its doc block), `:8` (import)
- Modify: `frontend/src/App.svelte:2630` (`onSwitch` wiring), `:877`, `:1307` (stale comments)
- Modify: `frontend/src/lib/BudgetsView.svelte:73-79` (stale comment), `:204-207` (dead hint branch)
- Modify: `frontend/src/lib/commands.js:104-111` (stale note on `canSetAsDefault`)
- Modify: `frontend/src/lib/i18n/locales/{de,en,es,fr,it,pt}.json` (remove `budgetsList.notSwitchableHint`)
- Test: `frontend/src/lib/budgetsView.test.js`

**Interfaces:**
- Consumes: `setActiveBudget(budgetId)` (`App.svelte:902`) — `POST /budgets/:id/activate`, resolves on 200, throws otherwise.
- Produces: `canSwitchTo(budget, activeBudget) -> boolean`, now access-agnostic.

- [ ] **Step 1: Update the failing tests**

In `frontend/src/lib/budgetsView.test.js`, replace the two shared-budget cases in `describe("canSwitchTo")`:

```js
  it("returns true for a shared (non-owned) budget that is not active", () => {
    const budget = { id: "other-id", is_owner: false };
    expect(canSwitchTo(budget, active)).toBe(true);
  });

  it("returns false for a shared budget that IS the active one", () => {
    const budget = { id: "active-id", is_owner: false };
    expect(canSwitchTo(budget, active)).toBe(false);
  });

  it("returns true for a shared budget when there is no active budget yet", () => {
    const budget = { id: "other-id", is_owner: false };
    expect(canSwitchTo(budget, null)).toBe(true);
  });
```

Keep every other existing case (owned/active/null-budget/archived/closed) unchanged — but the regression-lock comment above the archived/closed cases at `budgetsView.test.js:48-53` cites `canSetAsDefault` and `set_default_budget` as the rationale and is now stale. Replace its middle sentence with:

```js
  // Regression lock-in: canSwitchTo deliberately does NOT gate on archived_at/closed_at —
  // the backend's set_active_budget has no such restriction beyond `check_permission != None`
  // (#255/#391), and archiving is documented as explicitly not read-only (AGENTS.md
  // #50). This is a considered decision, not an oversight (see the comment above
  // canSwitchTo's definition) — this test exists so a future "fix" based on intuition alone
  // would fail here first.
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd frontend && npm test -- budgetsView`
Expected: FAIL — 3 assertions expecting `true` receive `false`.

- [ ] **Step 3: Rewrite the gate**

In `frontend/src/lib/budgetsView.js`, drop the `canSetAsDefault` import at line 8 and replace the `canSwitchTo` doc block + body:

```js
// Whether `budget` may be switched to the caller's active budget FROM this list page.
//
// #391: this is deliberately NOT an ownership gate. Switching writes the caller's own
// per-viewer preference (users.active_budget_id, #255) via POST /budgets/:id/activate,
// which the backend allows for ANY access level and which never touches the owner's
// is_default row — so a budget merely SHARED with the viewer is a valid target. Every
// row in the caller's list is, by construction, one they can access (list_budgets
// returns owned + shared-with-you only); the backend's `check_permission != None` gate
// on /activate is the real authority.
//
// "Active" is determined by matching id against the App-resolved `activeBudget` — NOT
// by the row's own raw `is_default` flag, which can be true on a SHARED row reflecting
// the REMOTE owner's default rather than the viewer's (#266).
//
// Deliberately does NOT special-case a closed or archived budget: set_active_budget has
// no closed/archived guard, and archiving is documented as explicitly NOT read-only
// (AGENTS.md #50), so this predicate matches the backend's actual capability.
export function canSwitchTo(budget, activeBudget) {
  if (!budget) return false;
  return budget.id !== activeBudget?.id;
}
```

- [ ] **Step 4: Rewire the list page**

In `frontend/src/App.svelte`, change line 2630 from `onSwitch={setDefaultBudget}` to:

```svelte
              onSwitch={setActiveBudget}
```

- [ ] **Step 5: Remove the now-unreachable hint branch**

In `frontend/src/lib/BudgetsView.svelte`, delete the `{:else if !budget.is_owner && !isActive}` branch and its `<span>` (lines 204-207), leaving:

```svelte
            {#if switchable}
              <button
                type="button"
                class="btn btn-sm btn-primary shrink-0"
                disabled={switchingId !== null}
                onclick={(e) => { e.stopPropagation(); handleSwitch(budget); }}
              >
                {switchingId === budget.id
                  ? $_("budgetsList.switching")
                  : $_("budgetsList.switchAction")}
              </button>
            {/if}
```

Then delete the `budgetsList.notSwitchableHint` key from all six locale files.

- [ ] **Step 6: Fix every stale comment**

Four comments now assert the removed wiring. Correct each:

- `frontend/src/lib/BudgetsView.svelte:75` — "App.svelte always wires onSwitch={setDefaultBudget} today" → `onSwitch={setActiveBudget}`.
- `frontend/src/lib/commands.js:104-111` — replace the `NOTE (#255)` paragraph on `canSetAsDefault` with:

```js
// NOTE (#255, #391): nothing in the UI calls this today. Both the /budgets-switch
// chat command and the budgets-list-page Switch button now use the any-access
// POST /budgets/:id/activate (setActiveBudget). This helper is retained,
// intentionally unused, as the correct owner-only gate for any FUTURE surface that
// genuinely writes budgets.is_default via POST /budgets/:id/default (#238).
```

- `frontend/src/App.svelte:877` (`setDefaultBudget`'s doc comment) — append:

```js
  // #391: intentionally unused in the UI today — the budgets-list Switch button moved
  // to setActiveBudget. Kept as the only correct caller shape for POST /budgets/:id/default
  // should an explicit "set my default budget" surface return.
```

- `frontend/src/App.svelte:1307` — the parenthetical "(contrast the owner-only BudgetsView.svelte list-page switch, which still uses canSetAsDefault/setDefaultBudget unchanged)" is now false; delete that parenthetical.

- [ ] **Step 7: Run the frontend suite**

Run: `cd frontend && npm test`
Expected: PASS, including `i18nLocales.test.js` (key parity after the removal) and `commands.test.js`.

- [ ] **Step 8: Commit**

```bash
git add frontend/src/lib/budgetsView.js frontend/src/lib/budgetsView.test.js \
        frontend/src/lib/BudgetsView.svelte frontend/src/lib/commands.js \
        frontend/src/App.svelte frontend/src/lib/i18n/locales
git commit -m "fix(#391): budgets list can switch to a shared budget via /activate"
```

---

### Task 5: Distinguish 403/404 switch failures

**Files:**
- Modify: `frontend/src/App.svelte:896-916` (`setActiveBudget`)
- Modify: `frontend/src/lib/BudgetsView.svelte:83-97` (`handleSwitch` catch)
- Modify: `frontend/src/lib/i18n/locales/{de,en,es,fr,it,pt}.json`
- Test: `frontend/src/lib/budgetsView.test.js` (new `switchErrorKey` describe block)

**Interfaces:**
- Consumes: an error thrown by `setActiveBudget` carrying a numeric `.status`.
- Produces: `switchErrorKey(status) -> string` exported from `budgetsView.js` — returns `"budgetsList.switchErrorRevoked"` for 403, `"budgetsList.switchErrorDeleted"` for 404, `null` otherwise.

- [ ] **Step 1: Write the failing test**

Append to `frontend/src/lib/budgetsView.test.js`:

```js
import { switchErrorKey } from "./budgetsView.js";

describe("switchErrorKey", () => {
  it("maps 403 to the access-revoked message", () => {
    expect(switchErrorKey(403)).toBe("budgetsList.switchErrorRevoked");
  });

  it("maps 404 to the budget-deleted message", () => {
    expect(switchErrorKey(404)).toBe("budgetsList.switchErrorDeleted");
  });

  it("returns null for other statuses so the caller keeps its generic copy", () => {
    expect(switchErrorKey(500)).toBe(null);
    expect(switchErrorKey(undefined)).toBe(null);
    expect(switchErrorKey(null)).toBe(null);
  });
});
```

(Merge the import into the existing top-of-file import block rather than adding a second one.)

- [ ] **Step 2: Run to verify it fails**

Run: `cd frontend && npm test -- budgetsView`
Expected: FAIL — `switchErrorKey is not a function`.

- [ ] **Step 3: Implement the mapper**

Append to `frontend/src/lib/budgetsView.js`:

```js
// #391 (#255 follow-up): map a failed POST /budgets/:id/activate to a specific i18n key.
// set_active_budget distinguishes exactly two states worth naming — 403 (check_permission
// returned None: the share was revoked between this list load and the click) and 404 (the
// TOCTOU branch: the budget was deleted mid-write). Everything else stays on the caller's
// existing generic copy, so an unmapped status can never render a misleading cause.
export function switchErrorKey(status) {
  if (status === 403) return "budgetsList.switchErrorRevoked";
  if (status === 404) return "budgetsList.switchErrorDeleted";
  return null;
}
```

- [ ] **Step 4: Attach the status in `setActiveBudget`**

In `frontend/src/App.svelte`, in `setActiveBudget`, replace the `if (!res.ok)` block:

```js
    if (!res.ok) {
      const errText = await res.text();
      // #391: carry the status so BudgetsView can name the cause (403 revoked /
      // 404 deleted) instead of falling back to the generic "couldn't switch".
      const err = new Error(errText || "API error");
      err.status = res.status;
      throw err;
    }
```

Leave `setDefaultBudget` untouched.

- [ ] **Step 5: Branch the message in `handleSwitch`**

In `frontend/src/lib/BudgetsView.svelte`, add `switchErrorKey` to the existing `budgetsView.js` import, then replace the body of the `catch (e)` block at `:83-97`:

```js
    } catch (e) {
      // Only a genuine switch failure gets the "couldn't switch" message — see the
      // separate try/finally below for the post-switch refresh, which must never be
      // mislabeled as a failed switch. A 403/404 gets copy naming the specific cause
      // (#391); anything else keeps the interpolated-or-generic fallback.
      console.error(`BudgetsView: switch failed for budget ${budget.id}`, e);
      const specific = switchErrorKey(e?.status);
      if (specific) {
        switchError = $_(specific);
      } else {
        switchError = e?.message
          ? $_("budgetsList.switchError", { values: { error: e.message } })
          : $_("budgetsList.switchErrorGeneric");
      }
      switchingId = null;
      return;
    }
```

- [ ] **Step 6: Add the i18n keys to all six locales**

Alongside `switchErrorGeneric` in each of `de,en,es,fr,it,pt`.json. English:

```json
    "switchErrorRevoked": "You no longer have access to that budget",
    "switchErrorDeleted": "That budget no longer exists",
```

Translate genuinely for de/es/fr/it/pt — match each file's existing tone and formality.

- [ ] **Step 7: Run the frontend suite**

Run: `cd frontend && npm test`
Expected: PASS, including `i18nLocales.test.js`.

- [ ] **Step 8: Commit**

```bash
git add frontend/src/lib/budgetsView.js frontend/src/lib/budgetsView.test.js \
        frontend/src/lib/BudgetsView.svelte frontend/src/App.svelte \
        frontend/src/lib/i18n/locales
git commit -m "fix(#391): name 403/404 causes when switching the active budget fails"
```

---

### Task 6: End-to-end verification

**Files:** none modified — this is the acceptance gate.

- [ ] **Step 1: Confirm `is_default` is untouched by a sharee's switch**

`set_active_budget_allows_any_sharee` (`backend/src/budget.rs:8484`) already proves a sharee may call `/activate`. Add an assertion to it if it does not already check that the target budget's `is_default` is byte-identical before and after:

```rust
    let before: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
        .bind(owner_budget).fetch_one(&pool).await.expect("before");
    // … perform the sharee's /activate …
    let after: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
        .bind(owner_budget).fetch_one(&pool).await.expect("after");
    assert_eq!(before, after, "a sharee's /activate must never mutate the owner's is_default");
```

Run: `cd backend && cargo test set_active_budget -- --ignored`
Expected: PASS.

- [ ] **Step 2: Full suites**

Run: `cd backend && cargo test && cargo test -- --ignored && cargo clippy -- -D warnings`
Run: `cd frontend && npm test && npm run build`
Expected: all PASS, build clean.

- [ ] **Step 3: Manual two-account walkthrough**

1. Account A creates "Family Budget", shares it with account B (Edit).
2. Account B → Budgets page → the shared row shows a **Switch** button. Click it.
3. Confirm: header shows "Family Budget"; the status strip is populated (Task 3); the Active badge moved.
4. Account B adds a category and logs a transaction via chat.
5. Account A opens "Family Budget" → both are visible.
6. Account B's own budget is unchanged; account A's `is_default` is unchanged.

- [ ] **Step 4: Verify each ticket AC**

Walk the AC checklist on #391 and tick each box in the issue.

- [ ] **Step 5: Open the PR**

```bash
git push -u origin <branch>
gh pr create --repo savvagent/nels --title "fix(#391): let a sharee make a shared budget active" --body "Closes #391"
```

## Known Plan Gaps

- No Svelte component tests exist in this repo (`environment: "node"`, no jsdom), so the 403/404 message *rendering* and the `onSwitch={setActiveBudget}` wiring are covered only by the pure-helper tests plus Task 6's manual walkthrough. Adding a jsdom harness is out of scope for #391.
- Task 4 removes `budgetsList.notSwitchableHint`. Verified today it is referenced only by `BudgetsView.svelte:213` and the six locale files — re-grep before deleting in case another surface has since adopted it.
- The i18n strings in Task 5 are English source copy; the de/es/fr/it/pt translations must be written during implementation, not machine-placeholdered.
- Test placement was corrected on 2026-07-21 after the plan was posted to #391: both `rag.rs` and `insights.rs` already have DB test harnesses, so tests live in the module they cover and no visibility is widened. The `#391` comment still shows the superseded `pub(crate)` approach.
