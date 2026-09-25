# Per-viewer active-budget preference — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a budget viewer make ANY budget they can access (owned or shared) their "active" chat view via `/budgets-switch`, without ever mutating another owner's `is_default` row — reversing the UX narrowing #251 introduced as a side effect of closing #238's broken-access-control bug.

**Architecture:** Add a nullable `users.active_budget_id` FK (`ON DELETE SET NULL`) as a per-viewer preference, decoupled from the owner-scoped `budgets.is_default`. A new `POST /budgets/:id/activate` endpoint (gated on ANY access level, not Owner-only) writes it. `GET /budgets` (`list_budgets`) surfaces a new `is_active: bool` on `BudgetListItem`, computed only there (every other construction site defaults it `false`, mirroring the existing `owner_name` precedent). The frontend's `fetchBudgets()` checks `is_active` first, then falls through to its existing (untouched) `is_default` fallback expression — so this composes cleanly whether nels#266 (a concurrent, unrelated fix to that same fallback expression) lands before or after this PR. `/budgets-switch` is rewired to call the new endpoint unconditionally instead of the owner-only `/default`.

**Tech Stack:** Rust, axum, sqlx (PostgreSQL, pgvector), tokio test; Svelte 5 (Vite), vitest, `svelte-i18n`.

**Source:** savvagent/nels#255. **Branch:** `nels-255-active-budget-pref` (off `origin/main` @ 534fb8c).

**Repo conventions (Phase 0.5):**
- Backend: `cd backend && cargo check` / `cargo test` (offline-safe, uses mocks/stubs where needed) / `cargo test -- --ignored` (DB-backed; connects directly to the local pgvector DB at `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag` — start it with `podman-compose up -d` from repo root if not already running). DB-backed tests do **not** run `sqlx::migrate!`, so a new migration must be applied to the local test DB manually (via `psql`) before those tests will pass against it — the real server auto-runs migrations on start, this is only a test-DB gap.
- Frontend: `cd frontend && pnpm test` (vitest) and `cd frontend && pnpm run build` (Vite build — also a type/syntax check for `.svelte`).
- Commit format: conventional commits with the issue ref, e.g. `feat(#255): …`.
- Deploy: path-filtered GitHub Actions deploy backend → Fly.io (`nels-api`) and frontend → Cloudflare Pages (`nels`) on push to `main`. No manual deploy step for this change.
- Out-of-band artifact: **one migration** (this plan's Task 1). No config/secrets/flags/infra changes.
- **Migration timestamp note:** originally filed as `20260703000000_user_active_budget.sql`, but this repo's shared local Postgres test DB (`127.0.0.1:6153/budget_rag`, used across concurrently active worktrees) already had a *different*, concurrently-in-flight migration (nels#228, "fund categories") applied under that identical version number. Renamed to `20260703013000_user_active_budget.sql` to avoid a version-number collision — `sqlx::migrate!` treats the numeric filename prefix as a global version and errors (`VersionMismatch`/`VersionMissing`) if two migration files share one. This is purely a filename/version choice; the SQL content is unaffected. Residual `VersionMissing(20260703000000)` failures in `billing::tests::`/`rag::tests::` when running the FULL `cargo test -- --ignored` suite are pre-existing noise from that concurrent job's migration being applied to the *shared* local test DB (not to this worktree's `backend/migrations/`) — unrelated to any file this plan touches, and not reproducible in isolation (`cargo test -- --ignored budget::tests::`, the module this plan actually modifies, passes cleanly). This repo has no CI test-gating workflow (only release-gated deploy workflows), so this local-only DB-sharing artifact does not affect the actual PR/merge gate.

---

### Task 1: Migration — `users.active_budget_id` + `db::User` struct field

**Files:**
- Create: `backend/migrations/20260703013000_user_active_budget.sql`
- Modify: `backend/src/db.rs` (`User` struct, ~line 6-26)

- [ ] **Step 1: Write the migration**

Create `backend/migrations/20260703013000_user_active_budget.sql`:

```sql
-- Per-viewer active-budget preference (#255), decoupled from the owner-scoped
-- budgets.is_default column (which represents "which of my OWN budgets is my
-- default", not "which budget — owned or shared — is currently active for
-- me"). NULL means "no override set — fall back to the existing is_default
-- resolution". ON DELETE SET NULL (mirroring budgets.rollup_parent_id's
-- existing idiom) so deleting the referenced budget cannot leave a dangling
-- preference or block the delete.
ALTER TABLE users ADD COLUMN IF NOT EXISTS active_budget_id UUID REFERENCES budgets(id) ON DELETE SET NULL;
```

- [ ] **Step 2: Apply the migration to the local test DB so DB-backed tests can run**

Ensure the DB is up, then apply the migration directly (the `#[ignore]` tests connect straight to Postgres, bypassing `sqlx::migrate!`):

```bash
podman-compose up -d
PGPASSWORD=postgrespassword psql -h 127.0.0.1 -p 6153 -U postgres -d budget_rag \
  -c "ALTER TABLE users ADD COLUMN IF NOT EXISTS active_budget_id UUID REFERENCES budgets(id) ON DELETE SET NULL;"
```
Expected: `ALTER TABLE`. (Idempotent — `IF NOT EXISTS` — safe to re-run.)

- [ ] **Step 3: Add the field to `db::User`**

In `backend/src/db.rs`, add to the `User` struct (after `show_limit_spent_remaining_summary`, before the closing `}` at line 26):

```rust
    /// Per-viewer active-budget preference (#255): the budget this user wants
    /// to see as their "active" view, decoupled from the owner-scoped
    /// `budgets.is_default` column. NULL means no override — callers fall back
    /// to the is_default-based resolution. Required here because `auth::me`
    /// and `account::update_preferences` decode `SELECT *` from `users`.
    pub active_budget_id: Option<Uuid>,
```

- [ ] **Step 4: Verify the existing `User`-decoding paths still compile and pass**

Run: `cd backend && cargo check`
Expected: no errors (the field is additive; `SELECT *` call sites decode it automatically via `FromRow`).

Run: `cd backend && cargo test -- --ignored account::tests::update_preferences_scoped_to_user_and_defaults_true`
Expected: `test result: ok. 1 passed` — this is the existing test that exercises `SELECT * FROM users` decode via `query_as::<_, User>`; it must still pass with the new column present.

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/20260703013000_user_active_budget.sql backend/src/db.rs
git commit -m "feat(#255): add users.active_budget_id preference column"
```

---

### Task 2: `BudgetListItem.is_active` field, defaulted `false` at every non-`list_budgets` site

**Files:**
- Modify: `backend/src/budget.rs` — `BudgetListItem` struct (~line 53-133), and every `BudgetListItem { ... }` literal EXCEPT the two inside `list_budgets` (~line 1394-1613): `create_budget` (~1351), `get_budget` (~1671), `update_budget` (~1814), `close_budget` (~1989 and ~2037 — two sites, idempotent + just-closed paths), `archive_budget` (~2111 and ~2162 — two sites), `unarchive_budget` (~2230 and ~2279 — two sites), `budget_item_for` (~2342, shared by `link_rollup`/`unlink_rollup`).

- [ ] **Step 1: Add the field to the struct**

In `backend/src/budget.rs`, add to `BudgetListItem` (after `pub amount_mode: String,` at line 132, before the closing `}`):

```rust
    /// Whether this budget is the viewer's currently-resolved active budget
    /// (#255): their explicit per-viewer preference (`users.active_budget_id`)
    /// points at this budget, decoupled from the owner-scoped `is_default`
    /// flag. Populated ONLY by `list_budgets` — every other construction site
    /// in this file (create/get/update/close/archive/unarchive/
    /// link_rollup/unlink_rollup, via `budget_item_for`) unconditionally
    /// leaves it `false`, mirroring the existing `owner_name` field's
    /// documented precedent above. Do not read `is_active` from any response
    /// other than `list_budgets`'s as meaningful.
    pub is_active: bool,
```

- [ ] **Step 2: Run the build to find every construction site that needs updating**

Run: `cd backend && cargo check 2>&1 | grep "missing field"`
Expected: a list of `error[E0063]: missing field \`is_active\` in initializer of \`BudgetListItem\`` — one per non-`list_budgets` construction site (10 sites: `create_budget`, `get_budget`, `update_budget`, `close_budget`×2, `archive_budget`×2, `unarchive_budget`×2, `budget_item_for`). This compiler-driven list is authoritative over the line numbers above, which are approximate.

- [ ] **Step 3: Add `is_active: false,` to each of the 10 non-`list_budgets` sites**

For each `BudgetListItem { ... }` literal reported in Step 2 (every one EXCEPT the two inside `list_budgets`), add a line:

```rust
        is_active: false,
```

placed next to the existing `is_default: ...,` line in that same literal, for readability (the two related flags read together).

- [ ] **Step 4: Verify it compiles**

Run: `cd backend && cargo check`
Expected: no errors. (Two remaining `missing field` errors are expected and correct at this point — the two `list_budgets` sites, handled in Task 3.)

- [ ] **Step 5: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#255): add BudgetListItem.is_active, defaulted false outside list_budgets"
```

(This commit will not compile standalone — `list_budgets`'s two sites still lack the field. That's fine; Task 3's commit immediately follows and the branch as a whole is what matters. If you prefer a compiling-at-every-commit history, fold Task 2 and Task 3 into one commit — either is acceptable here since both are one logical change split for reviewability.)

---

### Task 3: `list_budgets` computes `is_active` + backend tests

**Files:**
- Modify: `backend/src/budget.rs` — `list_budgets` (~line 1394-1613)
- Test: `backend/src/budget.rs` — `mod tests` block (add near the existing rollup/permission tests, ~line 5690+)

- [ ] **Step 1: Extend the user-row query and compute `is_active` per row**

In `list_budgets`, change:

```rust
    // Fetch user email
    let user_row = sqlx::query("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;

    let email: String = user_row.get("email");
```

to:

```rust
    // Fetch user email + active-budget preference (#255) in one query.
    let user_row = sqlx::query("SELECT email, active_budget_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;

    let email: String = user_row.get("email");
    let active_budget_id: Option<Uuid> = user_row.get("active_budget_id");
```

Then, in the owned-rows loop, change the `is_default: r.get("is_default"),` line's containing literal to also set:

```rust
            is_default: r.get("is_default"),
            is_active: active_budget_id == Some(budget_id),
```

and identically in the shared-rows loop (the second `is_default: r.get("is_default"),` occurrence within `list_budgets`).

- [ ] **Step 2: Verify it compiles**

Run: `cd backend && cargo check`
Expected: no errors — this was the last of the 12 `BudgetListItem` construction sites.

- [ ] **Step 3: Write the failing tests**

Add to the `mod tests` block in `backend/src/budget.rs` (near `set_default_budget_rejects_non_owner_share`, reusing `rollup_test_setup`/`seed_rollup_budget`/`rollup_cleanup`/`test_state`):

```rust
    // #255: list_budgets surfaces is_active on exactly the row matching the
    // caller's active_budget_id preference — owned case.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_marks_owned_active_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let budget_a = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;
        let budget_b = seed_rollup_budget(&pool, owner, 50.0, 0.0, false).await;

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(budget_b)
            .bind(owner)
            .execute(&pool)
            .await
            .expect("seed preference");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(owner),
        )
        .await
        .expect("list_budgets must succeed");

        let a = list.iter().find(|b| b.id == budget_a).expect("budget_a present");
        let b = list.iter().find(|b| b.id == budget_b).expect("budget_b present");
        assert!(!a.is_active, "non-preferred owned budget must not be marked active");
        assert!(b.is_active, "preferred owned budget must be marked active");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255: list_budgets surfaces is_active on a SHARED budget when the
    // preference points at it — the entire point of the ticket (decoupled
    // from is_default, which stays owner-scoped and untouched here).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_marks_shared_active_budget() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let shared_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let viewer_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("fetch viewer email");
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(Uuid::new_v4())
        .bind(shared_budget)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared_budget)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("seed preference");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed");

        let shared = list.iter().find(|b| b.id == shared_budget).expect("shared budget present");
        assert!(shared.is_active, "shared budget matching the preference must be marked active");
        assert!(!shared.is_owner, "sanity: this row is shared, not owned, by the viewer");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }

    // #255: no preference set (active_budget_id NULL) -> every row is_active: false,
    // is_default/list semantics are completely unaffected (regression guard against
    // this feature changing existing default-resolution behavior).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_all_inactive_when_no_preference_set() {
        let (pool, owner) = rollup_test_setup().await;
        let budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(owner),
        )
        .await
        .expect("list_budgets must succeed");

        let item = list.iter().find(|b| b.id == budget).expect("budget present");
        assert!(!item.is_active, "no preference set -> is_active must be false");

        rollup_cleanup(&pool, &[owner]).await;
    }

    // #255 edge case: a preference pointing at a budget whose SHARE was later
    // revoked (not deleted -> no FK cascade fires) must not surface is_active
    // anywhere, and must not error. The stale preference silently stops
    // resolving rather than leaking a budget the viewer can no longer see.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_budgets_ignores_preference_after_share_revoked() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, viewer) = rollup_test_setup().await;
        let shared_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let viewer_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(viewer)
            .fetch_one(&pool)
            .await
            .expect("fetch viewer email");
        let share_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, 'view')",
        )
        .bind(share_id)
        .bind(shared_budget)
        .bind(&viewer_email)
        .execute(&pool)
        .await
        .expect("seed share");

        sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
            .bind(shared_budget)
            .bind(viewer)
            .execute(&pool)
            .await
            .expect("seed preference");

        // Revoke the share -> the budget drops out of the viewer's accessible list,
        // but active_budget_id (no FK to budget_shares) stays set.
        sqlx::query("DELETE FROM budget_shares WHERE id = $1")
            .bind(share_id)
            .execute(&pool)
            .await
            .expect("revoke share");

        let state = test_state(&pool);
        let Json(list) = list_budgets(
            State(state),
            Query(ListBudgetsQuery { archived: None }),
            Extension(viewer),
        )
        .await
        .expect("list_budgets must succeed even with a dangling preference");

        assert!(list.is_empty(), "viewer has no accessible budgets after the share was revoked");

        rollup_cleanup(&pool, &[owner, viewer]).await;
    }
```

- [ ] **Step 4: Run the tests**

Run: `cd backend && cargo test -- --ignored list_budgets_marks_owned_active_budget list_budgets_marks_shared_active_budget list_budgets_all_inactive_when_no_preference_set list_budgets_ignores_preference_after_share_revoked`
Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 5: Run the full ignored suite to confirm no regression**

Run: `cd backend && cargo test -- --ignored`
Expected: all tests pass (same pass count as before Task 1, plus the 4 new ones).

- [ ] **Step 6: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#255): list_budgets surfaces per-viewer is_active preference"
```

---

### Task 4: `POST /budgets/:id/activate` endpoint + route wiring + backend tests

**Files:**
- Modify: `backend/src/budget.rs` (new `set_active_budget` handler, placed after `set_default_budget`, ~line 1935)
- Modify: `backend/src/main.rs` (import + route, ~line 33-34, ~239)
- Test: `backend/src/budget.rs` — `mod tests` block

- [ ] **Step 1: Write the handler**

In `backend/src/budget.rs`, add immediately after `set_default_budget` (after its closing `}` around line 1935):

```rust
/// Set the caller's per-viewer active-budget preference (#255), decoupled
/// from the owner-scoped `is_default` column set by `set_default_budget`.
/// Unlike that endpoint, this one is gated on ANY access level (View, Edit,
/// or Owner) — it never mutates the `budgets` table, only the caller's own
/// `users` row, so there is no cross-owner mutation risk: a collaborator can
/// make a shared budget their own active view without touching the owner's
/// `is_default` flag.
pub async fn set_active_budget(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }

    sqlx::query("UPDATE users SET active_budget_id = $1 WHERE id = $2")
        .bind(budget_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    log_audit(&state.db, budget_id, user_id, "SET_ACTIVE_BUDGET", "Set budget as active view").await;

    Ok(StatusCode::OK)
}
```

- [ ] **Step 2: Wire the route**

In `backend/src/main.rs`, change the import (line ~34-35):

```rust
use budget::{
    create_budget, list_budgets, get_budget, update_budget, delete_budget, set_default_budget,
    close_budget, archive_budget, unarchive_budget,
```

to:

```rust
use budget::{
    create_budget, list_budgets, get_budget, update_budget, delete_budget, set_default_budget,
    set_active_budget,
    close_budget, archive_budget, unarchive_budget,
```

Then add the route next to `/default` (line ~239):

```rust
        .route("/budgets/:id/default", post(set_default_budget))
        .route("/budgets/:id/activate", post(set_active_budget))
```

- [ ] **Step 3: Verify it compiles**

Run: `cd backend && cargo check`
Expected: no errors.

- [ ] **Step 4: Write the failing tests**

Add to `mod tests` in `backend/src/budget.rs`:

```rust
    // #255: unlike set_default_budget (Owner-only, #238), set_active_budget
    // is the entire point of the ticket — a View OR Edit sharee CAN set a
    // shared budget as their own active-view preference, without touching
    // the owner's is_default row (that column is untouched by this endpoint).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_allows_any_sharee() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, view_sharee) = rollup_test_setup().await;
        let (_, edit_sharee) = rollup_test_setup().await;

        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        async fn seed_share(pool: &PgPool, budget_id: Uuid, sharee: Uuid, level: &str) {
            let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
                .bind(sharee)
                .fetch_one(pool)
                .await
                .expect("fetch sharee email");
            sqlx::query(
                "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(budget_id)
            .bind(&email)
            .bind(level)
            .execute(pool)
            .await
            .expect("seed share");
        }
        seed_share(&pool, owner_budget, view_sharee, "view").await;
        seed_share(&pool, owner_budget, edit_sharee, "edit").await;

        let state = test_state(&pool);

        for (sharee, label) in [(view_sharee, "view"), (edit_sharee, "edit")] {
            let ok = set_active_budget(
                State(state.clone()),
                Path(owner_budget),
                Extension(sharee),
            )
            .await;
            assert!(ok.is_ok(), "{label}-permission sharee must be able to set a shared budget active");

            let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
                .bind(sharee)
                .fetch_one(&pool)
                .await
                .expect("read preference");
            assert_eq!(prefs, Some(owner_budget), "{label} sharee's preference must now point at the shared budget");

            // The owner's is_default row is completely untouched by this endpoint.
            let owner_default: bool = sqlx::query_scalar("SELECT is_default FROM budgets WHERE id = $1")
                .bind(owner_budget)
                .fetch_one(&pool)
                .await
                .expect("read is_default");
            assert!(!owner_default, "{label} sharee's activate call must never flip the owner's is_default");
        }

        rollup_cleanup(&pool, &[owner, view_sharee, edit_sharee]).await;
    }

    // #255: a user with NO access at all (not owner, not shared) is rejected,
    // and their preference (if they had one) is left untouched.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_denies_no_access() {
        let (pool, owner) = rollup_test_setup().await;
        let (_, stranger) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let err = set_active_budget(State(state), Path(owner_budget), Extension(stranger))
            .await
            .expect_err("a user with no access must be rejected");
        assert_eq!(err.0, StatusCode::FORBIDDEN, "no access -> 403");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(stranger)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(prefs, None, "rejected attempt must not set a preference");

        rollup_cleanup(&pool, &[owner, stranger]).await;
    }

    // #255: the owner can of course set their own budget as their active view too.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn set_active_budget_allows_owner() {
        let (pool, owner) = rollup_test_setup().await;
        let owner_budget = seed_rollup_budget(&pool, owner, 100.0, 0.0, false).await;

        let state = test_state(&pool);
        let ok = set_active_budget(State(state), Path(owner_budget), Extension(owner)).await;
        assert!(ok.is_ok(), "the owner must be able to set their own budget active");

        let prefs: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("read preference");
        assert_eq!(prefs, Some(owner_budget));

        rollup_cleanup(&pool, &[owner]).await;
    }
```

- [ ] **Step 5: Run the tests**

Run: `cd backend && cargo test -- --ignored set_active_budget_allows_any_sharee set_active_budget_denies_no_access set_active_budget_allows_owner`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 6: Run the full suite (offline + ignored) to confirm no regression**

Run: `cd backend && cargo test`
Expected: all pass (offline suite unaffected by this change).

Run: `cd backend && cargo test -- --ignored`
Expected: all pass, including every test from Task 3 and Task 4.

- [ ] **Step 7: Commit**

```bash
git add backend/src/budget.rs backend/src/main.rs
git commit -m "feat(#255): add POST /budgets/:id/activate for any-access-level preference"
```

---

### Task 5: Frontend — `setActiveBudget()`, `/budgets-switch` wiring, `fetchBudgets()` resolution order

**Files:**
- Modify: `frontend/src/App.svelte` (new `setActiveBudget` function near `setDefaultBudget` ~line 799; `fetchBudgets()` ~line 729; `/budgets-switch` handler ~line 1032)
- Modify: `frontend/src/lib/commands.js` (`canSetAsDefault` doc comment ~line 96-104)

- [ ] **Step 1: Add `setActiveBudget()`**

In `frontend/src/App.svelte`, immediately after the existing `setDefaultBudget` function (after its closing `}` around line 814), add:

```javascript
  // POST /budgets/:id/activate returns 200 with an EMPTY body (mirrors
  // /budgets/:id/default, #249's raw-fetch pattern) — sets the caller's
  // per-viewer active-budget preference (#255), which the backend allows for
  // ANY accessible budget (owned or shared), unlike setDefaultBudget's
  // owner-only /default. Used by /budgets-switch so a shared budget can be
  // made "active" without ever mutating the owner's is_default row.
  async function setActiveBudget(budgetId) {
    const headers = {};
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetchWithTimeout(`${API_BASE}/budgets/${budgetId}/activate`, {
      method: "POST",
      headers,
    });
    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }
    if (!res.ok) {
      const errText = await res.text();
      throw new Error(errText || "API error");
    }
  }
```

- [ ] **Step 2: Update `fetchBudgets()`'s active-budget resolution**

In `frontend/src/App.svelte`, `fetchBudgets()` currently has:

```javascript
      budgets = await fetchApi("/budgets");
      if (budgets.length > 0) {
        const defaultB = budgets.find((b) => b.is_default) || budgets[0];
        activeBudget = defaultB;
```

Change to:

```javascript
      budgets = await fetchApi("/budgets");
      if (budgets.length > 0) {
        // #255: the viewer's explicit per-viewer preference (is_active, from
        // users.active_budget_id) takes priority over the owner-scoped
        // is_default fallback below — this line is deliberately layered on
        // top of, not merged into, the existing fallback expression so it
        // composes cleanly regardless of when #266 (an unrelated fix to that
        // same fallback) lands.
        const preferred = budgets.find((b) => b.is_active);
        const defaultB = preferred || budgets.find((b) => b.is_default) || budgets[0];
        activeBudget = defaultB;
```

- [ ] **Step 3: Rewire `/budgets-switch` to call `setActiveBudget` unconditionally**

In `frontend/src/App.svelte`, the `/budgets-switch` handler currently has:

```javascript
      const target = resolution.budget;
      if (target.id === activeBudget?.id) {
        pushAiMessage(t("commands.switchAlready", { values: { name: target.name } }));
        return;
      }
      if (!canSetAsDefault(target)) {
        pushAiMessage(t("commands.switchNotOwned", { values: { name: target.name } }));
        return;
      }
      isChatLoading = true;
      try {
        await setDefaultBudget(target.id);
```

Change to:

```javascript
      const target = resolution.budget;
      if (target.id === activeBudget?.id) {
        pushAiMessage(t("commands.switchAlready", { values: { name: target.name } }));
        return;
      }
      // #255: any budget in `budgets` is, by construction, one the viewer can
      // already access (list_budgets only returns owned + shared-with-you
      // rows) — the backend's check_permission != None gate on /activate is
      // the actual authority, so no client-side ownership gate is needed
      // here (contrast the owner-only BudgetsView.svelte list-page switch,
      // which still uses canSetAsDefault/setDefaultBudget unchanged).
      isChatLoading = true;
      try {
        await setActiveBudget(target.id);
```

The rest of the `try` block (the `fetchBudgets()` call, the success/refresh-failed messages, the `catch`) is unchanged — leave it exactly as-is.

- [ ] **Step 4: Remove the now-unused `canSetAsDefault` import from `App.svelte`**

Step 3 removed the only call site of `canSetAsDefault` inside `frontend/src/App.svelte` (its remaining occurrence there, line ~797, is just a code comment). Remove it from the import block at line 49 so it doesn't linger as an unused import:

```javascript
    canSetAsDefault,
```
→ delete this line from the import statement.

**Do NOT** remove `canSetAsDefault` from its export in `frontend/src/lib/commands.js` — it is still a live export, imported separately by `frontend/src/lib/budgetsView.js` (via `canSwitchTo`) for `BudgetsView.svelte`'s own list-page switch button, which stays owner-only and unchanged by this ticket (see Task 5 Step 5 below for its doc-comment update, which stays in `commands.js`, not `App.svelte`).

- [ ] **Step 5: Update `canSetAsDefault`'s doc comment in `commands.js` (behavior unchanged)**

In `frontend/src/lib/commands.js`, the comment above `canSetAsDefault` currently says:

```javascript
// Whether the current user may set `budget` as their default via /budgets-switch.
// Only the owner may flip a budget's owner-scoped is_default flag — the backend
// enforces this too (POST /budgets/:id/default requires Permission::Owner, #238) —
// so a budget merely shared with the user (View/Edit) is never a valid target.
// Fails closed: a missing/falsy is_owner, or a null/undefined budget, is rejected.
export function canSetAsDefault(budget) {
  return budget?.is_owner === true;
}
```

Change the comment (function body unchanged) to:

```javascript
// Whether the current user may flip `budget`'s owner-scoped is_default flag via
// POST /budgets/:id/default. Only the owner may do this — the backend enforces
// it too (Permission::Owner required, #238) — so a budget merely shared with the
// user (View/Edit) is never a valid target. Fails closed: a missing/falsy
// is_owner, or a null/undefined budget, is rejected.
//
// NOTE (#255): /budgets-switch (the chat slash command) no longer uses this
// gate — it now calls the ANY-access-level POST /budgets/:id/activate via
// setActiveBudget, so a shared budget IS switchable there. This helper still
// gates BudgetsView.svelte's budgets-list-page switch button, which still
// calls the owner-only setDefaultBudget (see canSwitchTo in budgetsView.js).
export function canSetAsDefault(budget) {
  return budget?.is_owner === true;
}
```

- [ ] **Step 6: Run the frontend test suite and build**

Run: `cd frontend && pnpm test`
Expected: all existing tests still pass (`commands.test.js`'s `canSetAsDefault` tests are behavior-unaffected — only the comment changed).

Run: `cd frontend && pnpm run build`
Expected: build succeeds (confirms the `.svelte` change compiles, and that removing the unused import in Step 4 didn't break anything referencing it elsewhere in the file).

- [ ] **Step 7: Commit**

```bash
git add frontend/src/App.svelte frontend/src/lib/commands.js
git commit -m "feat(#255): wire /budgets-switch to the per-viewer active-budget preference"
```

---

### Task 6: Remove the now-dead `switchNotOwned` i18n key

**Files:**
- Modify: `frontend/src/lib/i18n/locales/en.json`
- Modify: `frontend/src/lib/i18n/locales/fr.json`
- Modify: `frontend/src/lib/i18n/locales/de.json`
- Modify: `frontend/src/lib/i18n/locales/it.json`
- Modify: `frontend/src/lib/i18n/locales/pt.json`
- Modify: `frontend/src/lib/i18n/locales/es.json`

- [ ] **Step 1: Confirm no remaining references**

Run: `cd frontend && grep -rn "switchNotOwned" src`
Expected: no output (Task 5 Step 3 removed the only call site).

- [ ] **Step 2: Remove the key from each locale file**

In each of the 6 files above, delete the `"switchNotOwned": "..."` line (each file has exactly one, e.g. in `en.json`: `"switchNotOwned": "“{name}” is shared with you — only its owner can make it the default. Switch to one of your own budgets instead.",`). Leave every other key, ordering, and trailing-comma structure of the surrounding JSON untouched — just delete that one line.

- [ ] **Step 3: Verify the JSON is still valid and the app builds**

Run: `cd frontend && node -e "for (const f of ['en','fr','de','it','pt','es']) JSON.parse(require('fs').readFileSync('src/lib/i18n/locales/'+f+'.json','utf8'))" && echo "all 6 locale files parse OK"`
Expected: `all 6 locale files parse OK`

Run: `cd frontend && pnpm test`
Expected: all tests pass (no test asserts on `switchNotOwned`'s presence — confirmed by the Task 5/6 greps above).

Run: `cd frontend && pnpm run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/lib/i18n/locales/en.json frontend/src/lib/i18n/locales/fr.json frontend/src/lib/i18n/locales/de.json frontend/src/lib/i18n/locales/it.json frontend/src/lib/i18n/locales/pt.json frontend/src/lib/i18n/locales/es.json
git commit -m "chore(#255): remove unused switchNotOwned i18n key"
```

---

## Manual smoke check (pre-PR)

Not a substitute for the tests above, but a quick end-to-end sanity pass before opening the PR:

1. `podman-compose up -d` (if not already running).
2. `cd backend && cargo run` (auto-applies migrations, including Task 1's).
3. `cd frontend && pnpm run dev`.
4. Register/login two accounts (A, B). As A, create a budget "Groceries" and share it with B (View).
5. As B, run `/budgets-switch Groceries` in chat. Expect: `switchDone` message (no `switchNotOwned`), and the header now shows "Groceries" as B's active budget.
6. Refresh the page as B — the active budget should still resolve to "Groceries" (via `is_active` on the next `/budgets` fetch, not a fluke of client-only state).
7. As A, confirm A's own `is_default` budget is unaffected (A's active view did not change; A's `budgets.is_default` row was never touched by B's switch).
