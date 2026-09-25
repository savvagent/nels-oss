# Two-Tier Billing — Plan B3: Write-Gate Rollout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a budget **read-only for everyone** once its owner's subscription lapses — insert an owner-entitlement gate at every budget-scoped write site, gated behind an enforcement flag so it ships **inert** and is flipped on deliberately after prices are seeded and rows healed.

**Architecture:** A new `access::require_owner_entitled(pool, budget_id)` returns `Ok(())` unless `BILLING_WRITE_ENFORCEMENT` is set (default off), in which case it 402s when the budget's owner resolves to `Tier::None` (lapsed). It is inserted **after** the existing permission check at each mutation site — the permission logic (Edit/Owner or Owner-only, which varies per site) is left untouched; only one uniform entitlement line is added. Reads are never gated. Because the flag defaults off, the ~47 insertions are inert in tests and in prod until explicitly enabled — so existing write tests are unaffected (no test ripple), and the mass-lockout risk is a deliberate, reversible flip rather than a release side-effect.

**Tech Stack:** Rust, axum, sqlx. Tests: `#[ignore]` DB unit tests + flag-on integration checks.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 2, Gate 1). **Depends on:** Plan A + B1 (merged) — `access::budget_entitlement_with`, `entitlement::{Tier, PriceCatalog}`.

## Decisions baked in (from brainstorming)

- **Enforcement flag `BILLING_WRITE_ENFORCEMENT`** (default OFF). Ships inert; flipped in prod after the 4 `STRIPE_PRICE_*` are seeded and every existing owner row resolves to Pro/Basic (not the empty-catalog Basic fail-safe or None). This is the write-gate's kill-switch and eliminates the test ripple.
- **Add entitlement, don't replace permission.** The permission idioms vary (Edit/Owner, Owner-only) and are correct; B3 adds ONE uniform `require_owner_entitled` line after each, rather than swapping to `require_writable_budget` (which would wrongly allow Edit on Owner-only sites). `require_writable_budget` from B1 stays available but is not the mechanism here.
- **Reads and viewer-preference writes are NOT gated:** `get_budget`, `list_*`, `list_shares` (privileged read), and `set_active_budget` (writes the caller's own `users.active_budget_id`, not the budget) stay open so a lapsed budget remains fully viewable.
- **Bank actions already gated (B2):** link/refresh/consent use `require_tier(Pro)` (implies entitled) — do NOT add `require_owner_entitled` there. Bank **disconnect** (`require_edit_or_owner`, not Pro-gated) is a cleanup mutation — **left ungated** (a lapsed user may disconnect/clean up). Noted, not gated.

## Global Constraints

- Backend is a **bin-only crate**: `cargo test --bin backend`, never `--lib`. DB tests `#[ignore]` (`--ignored`, pgvector 6153).
- No self-attribution in commits.
- `require_owner_entitled(pool, budget_id)` is inserted AFTER the existing permission check and BEFORE the mutation (so a permission failure still 403s first, and a 402 leaves no partial write). Insert INSIDE any transaction's guard region but before the write — i.e. same position the permission check occupies.
- The 402 message is exactly: `This budget is read-only — the owner's subscription has lapsed.` (matches B1's `require_writable_budget`).
- Flag read helper: `BILLING_WRITE_ENFORCEMENT` truthy = `"1"` or case-insensitive `"true"`; anything else / unset = off.
- Do NOT gate reads (`perm == Permission::None` idiom sites) or `set_active_budget`.

---

## File Structure

- **Modify:** `backend/src/access.rs` — add `require_owner_entitled` + `require_owner_entitled_enforced` + `billing_write_enforced` + tests.
- **Modify:** `backend/src/budget.rs` — insert the gate at 19 write handlers.
- **Modify:** `backend/src/goals.rs`, `backend/src/ignore_rules.rs` — insert at their write handlers.
- **Modify:** `backend/src/rag.rs` — insert at the chat mutation arms / `chat_*` helpers.

---

### Task 1: `require_owner_entitled` gate + `BILLING_WRITE_ENFORCEMENT` flag

**Files:**
- Modify: `backend/src/access.rs`

**Interfaces:**
- Consumes: `budget_entitlement_with`, `entitlement::{Tier, PriceCatalog}`.
- Produces:
  - `pub async fn require_owner_entitled(pool: &PgPool, budget_id: Uuid) -> Result<(), (StatusCode, String)>`
  - `pub(crate) async fn require_owner_entitled_enforced(pool: &PgPool, budget_id: Uuid, catalog: &PriceCatalog) -> Result<(), (StatusCode, String)>`
  - `pub(crate) fn billing_write_enforced() -> bool`

- [ ] **Step 1: Write the failing tests**

Add to `access::tests` (reuse `test_pool`, `catalog`, `mk_user`, `mk_budget`, `mk_sub`):

```rust
    #[tokio::test]
    #[ignore]
    async fn owner_entitled_enforced_allows_entitled_owner() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await; // Basic is entitled (tier != None)
        let bid = mk_budget(&db, owner).await;
        assert!(require_owner_entitled_enforced(&db, bid, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn owner_entitled_enforced_blocks_lapsed_owner_402() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let err = require_owner_entitled_enforced(&db, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn owner_entitled_flag_off_allows_lapsed_owner() {
        // Flag OFF (default): the gate is inert even for a lapsed owner.
        std::env::remove_var("BILLING_WRITE_ENFORCEMENT");
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        assert!(require_owner_entitled(&db, bid).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn owner_entitled_flag_on_blocks_lapsed_owner() {
        std::env::set_var("BILLING_WRITE_ENFORCEMENT", "1");
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let res = require_owner_entitled(&db, bid).await;
        std::env::remove_var("BILLING_WRITE_ENFORCEMENT");
        assert_eq!(res.unwrap_err().0, StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn billing_write_enforced_reads_flag() {
        // NB: this test mutates a process env var; keep it serial with the others above.
    }
```

(Delete the empty `billing_write_enforced_reads_flag` stub — it's a placeholder note; the flag is covered by the two `flag_*` DB tests. Do not ship an empty test.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin backend access::tests::owner_entitled -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function require_owner_entitled_enforced`.

- [ ] **Step 3: Implement**

Add to `backend/src/access.rs` (after `require_writable_budget_with`):

```rust
/// True iff write-enforcement is enabled (`BILLING_WRITE_ENFORCEMENT` = "1"/"true").
/// Default OFF so the write-gate ships inert and is flipped on deliberately in
/// prod only after prices are seeded and owner rows resolve correctly.
pub(crate) fn billing_write_enforced() -> bool {
    std::env::var("BILLING_WRITE_ENFORCEMENT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Write-gate entitlement check. When enforcement is OFF (default) this is a
/// no-op — the caller's own permission check already ran. When ON, a budget
/// whose OWNER has lapsed (`Tier::None`) is read-only → 402. Insert AFTER the
/// existing permission check at each mutation site; never on read paths.
pub async fn require_owner_entitled(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    if !billing_write_enforced() {
        return Ok(());
    }
    require_owner_entitled_enforced(pool, budget_id, &PriceCatalog::from_env()).await
}

/// The enforcing core (bypasses the flag), for hermetic tests and composition.
pub(crate) async fn require_owner_entitled_enforced(
    pool: &PgPool,
    budget_id: Uuid,
    catalog: &PriceCatalog,
) -> Result<(), (StatusCode, String)> {
    let ent = budget_entitlement_with(pool, budget_id, catalog).await?;
    if ent.tier == Tier::None {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            "This budget is read-only — the owner's subscription has lapsed.".to_string(),
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin backend access::tests::owner_entitled -- --ignored --nocapture`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/access.rs
git commit -m "feat(billing): add require_owner_entitled write-gate behind enforcement flag"
```

---

### Task 2: Insert the gate at budget.rs write handlers (19 sites)

**Files:**
- Modify: `backend/src/budget.rs`

**Interfaces:**
- Consumes: `crate::access::require_owner_entitled`.

- [ ] **Step 1: Insert the gate at each WRITE handler, right after its permission reject**

At each site below, immediately AFTER the existing permission-reject block (`if perm != ... { return Err(403) }`) and before the mutation/transaction, add:

```rust
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
```

The 19 write handlers (add the line to each; the exact `budget_id` variable is already in scope — for the rollup handlers use the specific id each guard protected):

Edit-or-Owner writes: `update_budget`, `create_category`, `update_category`, `delete_category`, `create_transaction`, `finalize_transaction`, `update_transaction`, `approve_transaction`, `resolve_match`, `delete_transaction`.
Owner-only writes: `delete_budget`, `set_default_budget`, `close_budget`, `archive_budget`, `unarchive_budget`, `share_budget`, `revoke_share`, `link_rollup` (gate on the CHILD budget being linked — use `child_id`; a lapsed child owner shouldn't be re-parented), `unlink_rollup` (gate on `budget_id`/the child).

**Do NOT add the gate to** these read / preference handlers: `get_budget`, `list_budgets`, `list_categories`, `list_transactions`, `list_audit_logs`, `list_shares`, `set_active_budget` (writes the caller's own `users.active_budget_id`, not the budget). For `link_rollup`, gate the child only (one line) — the parent check stays permission-only.

- [ ] **Step 2: Build**

Run: `cargo build --bin backend`. Expected: clean (the gate is a no-op at runtime unless the flag is set).

- [ ] **Step 3: Add flag-on integration tests**

Add `#[ignore]` `#[serial_test::serial]` tests in `budget::tests` that, with `BILLING_WRITE_ENFORCEMENT=1`, a **lapsed** owner (seed `status='canceled'`) gets 402 from a representative write handler (`update_budget` and `create_transaction`) and NO write occurs; and that with the flag on, an **entitled** owner (seed `status='trialing'`) still succeeds. Remove the env var at test end. Existing budget tests (flag off) are untouched and must still pass — run `cargo test --bin backend budget::tests -- --ignored` to confirm.

- [ ] **Step 4: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(billing): write-gate on budget/category/transaction mutations"
```

---

### Task 3: Insert the gate at goals.rs and ignore_rules.rs write handlers

**Files:**
- Modify: `backend/src/goals.rs`, `backend/src/ignore_rules.rs`

**Interfaces:**
- Consumes: `crate::access::require_owner_entitled`.

- [x] **Step 1: goals.rs**

After the `require_edit(&perm)?;` line in each WRITE handler — `create_goal`, `update_goal`, `delete_goal`, and the contribution handler (`insert_contribution`'s HTTP entry / `add_contribution`) — add:

```rust
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
```
Do NOT gate `list_goals`/`get_goal` (reads).

- [x] **Step 2: ignore_rules.rs**

After the permission reject in `create_ignore_rule` (~94) and `delete_ignore_rule` (~135), add the same line (with the handler's `budget_id`). Do NOT gate `list_ignore_rules`.

- [x] **Step 3: Build + flag-on test**

Run: `cargo build --bin backend`. Add one `#[ignore]` `#[serial]` flag-on test (in whichever module is easier — `goals::tests`) that a lapsed owner gets 402 from `create_goal`. Run `cargo test --bin backend goals::tests ignore_rules::tests -- --ignored` (per module) — existing tests (flag off) still pass.

- [x] **Step 4: Commit**

```bash
git add backend/src/goals.rs backend/src/ignore_rules.rs
git commit -m "feat(billing): write-gate on goal and ignore-rule mutations"
```

---

### Task 4: Insert the gate at rag.rs chat mutation arms / `chat_*` helpers

**Files:**
- Modify: `backend/src/rag.rs`

**Interfaces:**
- Consumes: `crate::access::require_owner_entitled`.

- [x] **Step 1: Identify each chat mutation site and its budget id**

The mutating chat arms (rag.rs) each resolve a budget id (usually `active_budget_id`/`bid`, or a target budget resolved by name) and then either inline-write or call a `chat_*` helper that writes. The gate must run once per mutation, on the budget being mutated, after the arm's own permission check. The mutating arms: `SEED_CATEGORIES`, `DELETE_CATEGORY`, `SET_CATEGORY_ROLLOVER`, `SET_CATEGORY_FUND`, `UPDATE_CATEGORY`, `DELETE_BUDGET`, `CLOSE_BUDGET`, `ARCHIVE_BUDGET`, `UNARCHIVE_BUDGET`, `ROLLUP_BUDGET`, `UNROLLUP_BUDGET`, `CREATE_CATEGORY`, `ADD_TRANSACTION`, `SHARE_BUDGET`, `CREATE_GOAL`, `ADD_GOAL_CONTRIBUTION`, `UPDATE_BUDGET`, `EDIT_TRANSACTION`, `DELETE_TRANSACTION`, `EXCLUDE_TRANSACTION`, `CREATE_IGNORE_RULE`.
(`CREATE_BUDGET` is already handled by B4b's `ensure_ready_to_own_budget`; do NOT double-gate it. Non-budget arms — `SET_USER_NAME`, `DELETE_ACCOUNT`, `CREATE_REMINDER` if user-scoped, `LIST_*`/`SEARCH_*`/`OPEN_*`/`CATEGORY_BALANCE` reads — are NOT gated.)

**Preferred implementation:** where an arm delegates to a `chat_*` helper (e.g. `chat_edit_transaction`, `chat_delete_transaction`, `chat_exclude_transaction`, `chat_create_ignore_rule`, `chat_create_categories`), add the gate INSIDE that helper, right after its own permission check, returning the arm's error convention (set the `mutation_error`/return the `(_, Some(msg))` tuple the helper already uses) so a lapsed owner gets a clean chat error. Where an arm inlines its mutation, add the gate in the arm after its permission check, mirroring how B4b wrapped `CREATE_BUDGET` (set `mutation_error` and skip the write on `Err`).

Because `require_owner_entitled` is a no-op while the flag is off, each insertion is safe and inert; the goal is COVERAGE (every mutation path gated) so that flipping the flag makes the whole app read-only-on-lapse consistently.

- [x] **Step 2: Build + verify no chat test regresses (flag off)**

Run: `cargo build --bin backend` and `cargo test --bin backend rag::tests -- --ignored` and `cargo test --bin backend rag::tests`. With the flag off, all existing chat tests pass unchanged.

- [x] **Step 3: Add a flag-on chat test**

Add one `#[ignore]` `#[serial]` test exercising a representative `chat_*` helper (e.g. `chat_create_categories` or `chat_edit_transaction`) with `BILLING_WRITE_ENFORCEMENT=1` and a lapsed owner → the helper returns a `mutation_error` (no write). Remove the env var after.

- [x] **Step 4: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(billing): write-gate on chat mutation actions"
```

---

### Task 5: End-to-end flag-on verification + coverage audit

**Files:**
- Modify: `backend/src/rag.rs` or a suitable test module (tests only)

- [x] **Step 1: Coverage audit**

Grep every budget-scoped mutation and confirm each is gated (either by `require_owner_entitled`, or by B2's `require_tier(Pro)`, or by B4b's `ensure_ready_to_own_budget`). Command:
`grep -rnE "INSERT INTO (budgets|categories|transactions|goals|goal_contributions|budget_shares|ignore_rules|budget_rollups)|UPDATE (budgets|categories|transactions|goals)" backend/src/{budget,goals,ignore_rules,rag}.rs`
For each hit, confirm a gate precedes it on that path. List any mutation with NO gate (a coverage gap) in the report; add the gate if a real gap.

- [x] **Step 2: One end-to-end read-only test**

With `BILLING_WRITE_ENFORCEMENT=1` and a lapsed owner who owns a budget with data: assert a READ still works (`get_budget`/`list_transactions` → 200) while a WRITE is blocked (`create_transaction` → 402) — proving "read-only, not locked out." `#[ignore]` `#[serial]`.

- [x] **Step 3: Full suite (flag off) + build**

Run: `cargo test --bin backend` (non-ignored) and `cargo test --bin backend -- --ignored` (full DB suite). All pass with the flag off (the default) — confirming B3 is inert by default and introduced no regression.

- [x] **Step 4: Commit**

```bash
git add -A
git commit -m "test(billing): end-to-end read-only-on-lapse verification"
```

---

## Self-Review

**Spec coverage (Section 2, Gate 1):**
- Lapsed budget → read-only for everyone (owner + editors) → Tasks 2, 3, 4 (gate at every write) ✅
- Reads stay open → Global Constraints + explicit no-gate lists ✅
- Owner-only vs Edit-or-Owner permission preserved (gate added, not replaced) → Task 2 ✅
- Both REST and chat mutation paths gated → Tasks 2/3 (REST) + Task 4 (chat) ✅
- Enforcement is deliberate/reversible (flag) → Task 1 ✅

**Placeholder scan:** the only stub is the deleted `billing_write_enforced_reads_flag` note in Task 1 Step 1 (explicitly instructed to delete, not ship). No other placeholders.

**Type consistency:** `require_owner_entitled(&PgPool, Uuid) -> Result<(), (StatusCode,String)>` inserted identically everywhere; `require_owner_entitled_enforced`/`billing_write_enforced` are the test/flag seams. 402 message identical to B1's write-gate.

**No test ripple (by design):** the flag defaults off, so every existing write test runs with the gate inert — none need reseeding. Only the new flag-on tests set `BILLING_WRITE_ENFORCEMENT` (all `#[serial]` + cleanup).

**Rollout note (operational):** after release + prices seeded + all owner rows healed, set `BILLING_WRITE_ENFORCEMENT=1` on Fly `nels-api` to activate. Before flipping, verify every current owner resolves to a non-None tier (else that owner is locked out of writes). Reversible: unset to disable.

**Out of scope for B3:** bank disconnect gating (left open deliberately); background-sync owner-Pro (B2-sync); frontend read-only banners / resubscribe CTA (Plan C); removing the flag later (optional future cleanup). The hourly `advance_fund_categories` background job is a system (non-user) write and is intentionally NOT gated (same class as B2-sync).

**Implementation deltas (as-built):**
- **Test strategy — hermetic, not flag-on env tests.** Commit 10cc33b (landed after this plan was written) removed every test that toggles the process-global `BILLING_WRITE_ENFORCEMENT`, because `serial_test` does not serialize serial tests against the many *parallel* non-serial write tests, so flipping the flag ON raced them into flaky 402s. Tasks 3–5 therefore do **not** add flag-on env tests (superseding the Step-3/Step-2 wording in those tasks). Enforcement OUTCOME is pinned hermetically by `access::tests::owner_entitled_enforced_*` (the flag-bypassing core) + `enforcement_flag_enabled` (pure parser); the chat write-gate SCOPE is pinned by the pure `rag::tests::write_gate_covers_every_mutating_chat_action_except_bank`. Gate PRESENCE is assured by this coverage audit + `cargo build`.
- **Task 4 — one central gate, not 21 per-arm inserts.** `rag.rs` centralizes chat-mutation authorization (`is_chat_action_authorized`), so the entitlement gate is inserted once, immediately after that permission chokepoint, keyed on the active budget. This is provably equivalent to gating all 21 arms: an Edit action mutates the active budget; an Owner action is only authorized when the caller owns the active budget and every target it resolves is owner-scoped to the caller — so `require_owner_entitled(active_budget_id)` resolves exactly the entity that must be entitled. Bank actions are excluded via the pure `chat_action_is_write_gated` predicate (they gate via B2 `require_tier(Pro)`); `CREATE_BUDGET` is `None`-perm and gated by B4b.
- **Task 5 — coverage audit found no gaps.** Every user-facing budget-scoped mutation in `budget/goals/ignore_rules/rag` is gated by B3 `require_owner_entitled` (REST per-site + chat central), B2 `require_tier(Pro)` (bank), or B4b `ensure_ready_to_own_budget` (`CREATE_BUDGET`). REST category-fund toggles ride inside the gated `update_category`; chat fund paths ride inside the centrally-gated `SET_CATEGORY_FUND`/`CREATE_CATEGORY` arms.
