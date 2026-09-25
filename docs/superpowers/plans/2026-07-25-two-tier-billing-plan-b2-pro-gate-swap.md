# Two-Tier Billing — Plan B2: Pro-Gate Swap (Bank Features) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make bank features require the **Pro tier** of the **budget's owner** (not merely the caller being "entitled"). Replace every per-provider `require_pro(pool, user_id)` (caller-scoped, entitled-or-not) with the owner-derived `access::require_tier(pool, budget_id, Tier::Pro)` from B1, and handle the one budget-less endpoint with a new caller-scoped tier helper.

**Architecture:** All budget-scoped bank actions (link/consent/refresh across GoCardless, Plaid, Basiq, Belvo, Akahu, Financial Connections, plus the three chat bank-link arms) already have `budget_id`/`bid` in scope right beside `user_id` — swap them to `require_tier(budget_id, Tier::Pro)`. The one budget-less endpoint (`gc_institutions_handler`, lists institutions by country) uses a new caller-scoped `access::require_caller_tier(pool, user_id, Tier::Pro)`. The six now-unused provider `require_pro` functions are deleted in a final cleanup task.

**Tech Stack:** Rust, axum, sqlx, wiremock. Tests are `#[ignore]` DB (+ some wiremock) unit tests.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 2, Gate 2). **Depends on:** Plan A + B1 (merged) — `access::require_tier`, `entitlement::{resolve, Tier, PriceCatalog}`.

## Decisions baked in (documented, from investigation)

- **Bank features = Pro tier specifically.** Today `require_pro` → `user_is_pro(status)` = `trialing|active`. Under two-tier, a **Basic** subscriber is `active` and would wrongly pass. So the gate must check `tier == Pro` (via `require_tier(_, Pro)`), not "entitled."
- **Owner-derived** for budget-scoped actions (an editor linking a bank to a shared budget uses the *owner's* Pro entitlement — matches what background sync already does).
- **Caller-scoped** only for `gc_institutions_handler` (no budget in scope): the caller's OWN subscription must be Pro. New helper `require_caller_tier`.
- **DEFERRED to a follow-up (B2-sync), NOT in this plan:** the webhook-driven background-sync gates (`if user_is_pro(owner_status)` in `plaid.rs`/`gocardless.rs`/`belvo.rs`/`basiq.rs`/`akahu.rs`/`financial_connections.rs`) still gate on "owner entitled," not "owner Pro." A Basic owner would still get background sync. Not observable until real Basic subscribers exist, and it needs the sync queries to also fetch owner `price_id`. Tracked as a follow-up; call it out in the final report.

## Global Constraints

- Backend is a **bin-only crate**: `cargo test --bin backend`, never `--lib`. DB tests `#[ignore]` (`--ignored`, pgvector 6153); some also wiremock.
- No self-attribution in commits.
- The 402 status code is preserved (frontend `isProGateError` keys on it). `require_tier`/`require_caller_tier` both return `402 PAYMENT_REQUIRED` on failure.
- `require_tier(pool, budget_id, Tier::Pro)` resolves the budget OWNER; `require_caller_tier(pool, user_id, Tier::Pro)` resolves the caller's OWN subscription.
- Do NOT change `require_edit_or_owner` or `ensure_not_closed` calls — only the `require_pro` line at each site.
- Deploy gate (same as B4a/B4b): needs the 4 `STRIPE_PRICE_*` seeded + the existing prod row healed to a Pro price, else a legacy/`past_due` owner resolves to Basic and gets 402 on bank features. Rides the held release (#438).

## Test ripple (expected, handled per task)

Provider tests that today seed the CALLER's subscription to pass `require_pro` must now seed the budget OWNER's subscription at **Pro tier** to pass `require_tier(budget_id, Pro)`; and "non-Pro → 402" tests must seed a non-Pro owner. Because the test process has no `STRIPE_PRICE_*` env, seed Pro via a price that the test sets in `PriceCatalog`... simpler: seed `status='active'` with a `price_id` that IS a Pro price AND set `STRIPE_PRICE_PRO_MONTHLY` to that price for the test, OR (preferred) set the env price vars in the test so `resolve` classifies the seeded `price_id` as Pro. Each task's step spells this out.

---

## File Structure

- **Modify:** `backend/src/access.rs` — add `require_caller_tier` (+ `caller_entitlement`) and tests.
- **Modify:** `backend/src/{gocardless,plaid,basiq,belvo,akahu,financial_connections}.rs` — swap budget-scoped `require_pro(pool, user_id)` → `crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro)`; update their tests; delete the dead `require_pro` fn (final task).
- **Modify:** `backend/src/rag.rs` — swap the 3 chat bank-link pre-checks to `require_tier(&state.db, bid, Tier::Pro)`.
- **Modify:** `backend/src/bank_linking.rs` — `gc_institutions_handler` → `require_caller_tier`.

---

### Task 1: `access::require_caller_tier` — caller's own subscription tier

**Files:**
- Modify: `backend/src/access.rs`

**Interfaces:**
- Consumes: `entitlement::{resolve, PriceCatalog, Tier}`.
- Produces:
  - `pub async fn require_caller_tier(pool: &PgPool, user_id: Uuid, min: Tier) -> Result<(), (StatusCode, String)>`
  - `pub(crate) async fn require_caller_tier_with(pool: &PgPool, user_id: Uuid, min: Tier, catalog: &PriceCatalog) -> Result<(), (StatusCode, String)>`

- [ ] **Step 1: Write the failing DB tests**

Add to `access::tests` in `backend/src/access.rs` (reuse existing `test_pool`, `catalog`, `mk_user`, `mk_sub` helpers):

```rust
    #[tokio::test]
    #[ignore]
    async fn caller_tier_pro_allows_pro_caller() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_sub(&db, uid, "active", Some("price_pro")).await;
        assert!(require_caller_tier_with(&db, uid, Tier::Pro, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn caller_tier_pro_rejects_basic_caller_402() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        mk_sub(&db, uid, "active", Some("price_basic")).await;
        let err = require_caller_tier_with(&db, uid, Tier::Pro, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn caller_tier_pro_rejects_unsubscribed_caller_402() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let err = require_caller_tier_with(&db, uid, Tier::Pro, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin backend access::tests::caller_tier -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function require_caller_tier_with`.

- [ ] **Step 3: Implement**

Add to `backend/src/access.rs` (after `require_tier_with`):

```rust
/// Caller's OWN entitlement (their own subscription row), for the rare bank
/// endpoints with no budget in scope (institution listing). Budget-scoped
/// actions must use `require_tier` (owner-derived) instead.
pub(crate) async fn caller_entitlement_with(
    pool: &PgPool,
    user_id: Uuid,
    catalog: &PriceCatalog,
) -> Result<Entitlement, (StatusCode, String)> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, price_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(pool).await.map_err(internal_error)?;
    let (status, price_id) = row.unwrap_or((None, None));
    Ok(resolve(status.as_deref(), price_id.as_deref(), catalog))
}

/// 402 unless the CALLER's own subscription is at `min` tier or higher.
pub async fn require_caller_tier(
    pool: &PgPool,
    user_id: Uuid,
    min: Tier,
) -> Result<(), (StatusCode, String)> {
    require_caller_tier_with(pool, user_id, min, &PriceCatalog::from_env()).await
}

pub(crate) async fn require_caller_tier_with(
    pool: &PgPool,
    user_id: Uuid,
    min: Tier,
    catalog: &PriceCatalog,
) -> Result<(), (StatusCode, String)> {
    let ent = caller_entitlement_with(pool, user_id, catalog).await?;
    if ent.tier < min {
        return Err((StatusCode::PAYMENT_REQUIRED,
            "This feature requires Nels Pro.".to_string()));
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin backend access::tests::caller_tier -- --ignored --nocapture`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/access.rs
git commit -m "feat(billing): add require_caller_tier for budget-less bank gates"
```

---

### Task 2: Swap budget-scoped Pro-gates — GoCardless, Plaid, Basiq

**Files:**
- Modify: `backend/src/gocardless.rs`, `backend/src/plaid.rs`, `backend/src/basiq.rs`

**Interfaces:**
- Consumes: `access::require_tier`, `entitlement::Tier`.

- [ ] **Step 1: Swap each budget-scoped call site**

In each of `gocardless.rs`, `plaid.rs`, `basiq.rs`, find every line that is exactly `require_pro(pool, user_id).await?;` inside a function that also has `budget_id` in scope (they all sit right after `require_edit_or_owner(pool, user_id, budget_id).await?;`). Replace each such line with:

```rust
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
```

Do NOT delete the `require_pro` fn yet (Task 6 removes all six after every call site is swapped; a `dead_code`/`unused` warning here is expected mid-plan). Do NOT touch `require_edit_or_owner`/`ensure_not_closed` lines. GoCardless sites: ~235, 303, 570. Plaid: ~134, 174, 557. Basiq: ~177, 289, 516.

- [ ] **Step 2: Fix the affected provider tests**

Each provider's `#[ignore]` tests that exercise these functions currently seed the CALLER as Pro (`mk_pro_subscription`/an `active` sub) to pass `require_pro`. They must now make the budget OWNER resolve to **Pro tier** under `require_tier`. In these tests the acting user IS the budget owner (the tests create a budget owned by the test user), so:
- Set the Pro price env in the test and seed the owner's sub with that price: e.g. at test start `std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_test");` and seed `INSERT INTO subscriptions (user_id, stripe_customer_id, status, price_id) VALUES (owner, 'cus_x', 'active', 'price_pro_test')`. Mark such tests `#[serial_test::serial]` (env mutation) and `std::env::remove_var` at the end.
- Any existing "non-Pro caller → 402" test becomes "non-Pro owner → 402": seed the owner with `status='active', price_id='price_basic_test'` (and set `STRIPE_PRICE_BASIC_MONTHLY=price_basic_test`) OR no subscription — either resolves to non-Pro → `require_tier(Pro)` 402.
- Where a test seeded a DIFFERENT user as owner than the caller (shared-budget edit cases), seed the OWNER's Pro sub (not the caller's) — this is the semantic change; verify the test still asserts the intended pass/deny.

- [ ] **Step 3: Run the three providers' tests + build**

Run: `cargo test --bin backend gocardless::tests plaid::tests -- --ignored --nocapture` then `cargo test --bin backend basiq::tests -- --ignored --nocapture` (run per-module as needed).
Expected: pass. Fix any test still asserting caller-scoped behavior.
Run: `cargo build --bin backend` (expect `dead_code` warnings for the not-yet-removed `require_pro` fns — fine).

- [ ] **Step 4: Commit**

```bash
git add backend/src/gocardless.rs backend/src/plaid.rs backend/src/basiq.rs
git commit -m "feat(billing): owner Pro-gate for GoCardless/Plaid/Basiq bank actions"
```

---

### Task 3: Swap budget-scoped Pro-gates — Belvo, Akahu, Financial Connections

**Files:**
- Modify: `backend/src/belvo.rs`, `backend/src/akahu.rs`, `backend/src/financial_connections.rs`

**Interfaces:**
- Consumes: `access::require_tier`, `entitlement::Tier`.

- [ ] **Step 1: Swap each budget-scoped call site**

Same mechanical change as Task 2, in `belvo.rs` (~266, 313, 617), `akahu.rs` (~100, 164, 383), `financial_connections.rs` (~152, 223, 505): replace each `require_pro(pool, user_id).await?;` (that has `budget_id` in scope, right after `require_edit_or_owner`) with:

```rust
    crate::access::require_tier(pool, budget_id, crate::entitlement::Tier::Pro).await?;
```

Leave the `require_pro` fns in place (removed in Task 6).

- [ ] **Step 2: Fix the affected provider tests**

Same approach as Task 2 Step 2 (seed the OWNER's Pro sub with a Pro price env var, `#[serial]`, cleanup; non-Pro-owner tests seed Basic/none).

- [ ] **Step 3: Run + build**

Run: `cargo test --bin backend belvo::tests akahu::tests -- --ignored --nocapture` then `cargo test --bin backend financial_connections::tests -- --ignored --nocapture`.
Expected: pass. `cargo build --bin backend` (dead_code warnings expected).

- [ ] **Step 4: Commit**

```bash
git add backend/src/belvo.rs backend/src/akahu.rs backend/src/financial_connections.rs
git commit -m "feat(billing): owner Pro-gate for Belvo/Akahu/Financial Connections bank actions"
```

---

### Task 4: Swap the chat bank-link pre-checks

**Files:**
- Modify: `backend/src/rag.rs` (the 3 `require_pro` pre-checks in the bank-link chat arms, ~3350/3383/3395)

**Interfaces:**
- Consumes: `access::require_tier`, `entitlement::Tier`. `bid` (budget id) is in scope in these arms.

- [ ] **Step 1: Swap the three pre-checks**

Replace each `match crate::<provider>::require_pro(&state.db, user_id).await {` (at ~3350 GoCardless, ~3383 Basiq, ~3395 Akahu) with:

```rust
                                match crate::access::require_tier(&state.db, bid, crate::entitlement::Tier::Pro).await {
```

(keep the surrounding `Err((_, msg)) => mutation_error = Some(msg)` / `Ok(()) => { ... }` arms exactly). Confirm `bid` is the resolved budget id in scope at each (it is — the same `bid` passed to `start_link_session`/`create_consent_session` just below).

- [ ] **Step 2: Verify no rag test regresses**

Run: `cargo test --bin backend rag::tests -- --ignored --nocapture` and `cargo test --bin backend rag::tests -- --nocapture`. If any chat bank-link test seeded the caller's Pro sub, update it to seed the OWNER's Pro sub (the `bid` owner) with a Pro price env, mirroring Task 2. If no such test exists, note it.

- [ ] **Step 3: Build + commit**

Run: `cargo build --bin backend`.

```bash
git add backend/src/rag.rs
git commit -m "feat(billing): owner Pro-gate for chat bank-link actions"
```

---

### Task 5: Swap the budget-less institutions endpoint to caller-tier

**Files:**
- Modify: `backend/src/bank_linking.rs` (`gc_institutions_handler`, ~line 202)

**Interfaces:**
- Consumes: `access::require_caller_tier` (Task 1).

- [ ] **Step 1: Swap the gate**

In `gc_institutions_handler`, replace:
```rust
    crate::gocardless::require_pro(&state.db, user_id).await?;
```
with:
```rust
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
```
(This endpoint has no budget; the caller's own subscription must be Pro to browse institutions.)

- [ ] **Step 2: Fix/verify the test**

If a `bank_linking::tests` test asserts `gc_institutions_handler` 402s for a non-Pro caller / 200s for a Pro caller, update it to seed the caller's own sub at Pro (with a Pro price env) for the 200 case and Basic/none for the 402 case. Run: `cargo test --bin backend bank_linking::tests -- --ignored --nocapture`.

- [ ] **Step 3: Build + commit**

Run: `cargo build --bin backend`.

```bash
git add backend/src/bank_linking.rs
git commit -m "feat(billing): caller Pro-gate for gocardless institutions listing"
```

---

### Task 6: Delete the six dead `require_pro` functions

**Files:**
- Modify: `backend/src/{gocardless,plaid,basiq,belvo,akahu,financial_connections}.rs`

- [ ] **Step 1: Confirm every call site is swapped**

Run: `grep -rn "require_pro" backend/src --include=*.rs | grep -v "fn require_pro\|require_pro_" | grep -v test`
Expected: NO remaining non-definition call sites (all swapped in Tasks 2-5). If any remain, swap them first (report which).

- [ ] **Step 2: Delete each `require_pro` fn**

In each of the six provider files, delete the `pub(crate) async fn require_pro(...)` (and, for `financial_connections.rs`, the private `async fn require_pro`) definition and its doc comment. Also delete any test that tested `require_pro` **directly** (as opposed to through a create/refresh fn) if it no longer compiles — but prefer keeping tests that exercise the swapped behavior through the public functions.

- [ ] **Step 3: Build clean (no dead_code from require_pro) + full test**

Run: `cargo build --bin backend` — expect NO `require_pro`-related dead_code warnings now.
Run: `cargo test --bin backend -- --ignored --nocapture` (the bank-provider + access + billing suites) and `cargo test --bin backend` (non-ignored).
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add backend/src/gocardless.rs backend/src/plaid.rs backend/src/basiq.rs backend/src/belvo.rs backend/src/akahu.rs backend/src/financial_connections.rs
git commit -m "refactor(billing): remove dead per-provider require_pro helpers"
```

---

## Self-Review

**Spec coverage (Section 2, Gate 2):**
- Per-provider `require_pro` → owner `require_tier(budget_id, Pro)` → Tasks 2, 3 ✅
- Chat bank-link arms → owner `require_tier(bid, Pro)` → Task 4 ✅
- Budget-less institutions endpoint → caller `require_caller_tier(Pro)` → Tasks 1, 5 ✅
- Bank features require **Pro tier** (not merely entitled — fixes the Basic-subscriber hole) → Global Constraints + all swap tasks ✅
- Dead code removed → Task 6 ✅
- Background-sync owner-tier gating → **DEFERRED (B2-sync follow-up)**, documented; not a gap in B2's stated scope.

**Placeholder scan:** none — each swap is spelled out with the exact replacement line and the site line numbers. Test-fixup steps state the exact seeding approach.

**Type consistency:** `require_tier(pool, budget_id, Tier::Pro)` and `require_caller_tier(pool, user_id, Tier::Pro)` are used identically everywhere; both return `Result<(), (StatusCode, String)>` with 402 on denial, matching the old `require_pro` error shape so callers (`?`, and the chat `match`) are unchanged in structure.

**Known consequence (not a gap):** the semantic shift from caller-entitled to owner-Pro means provider tests must seed the OWNER (not caller) at Pro tier; Tasks 2-5 each require this and call out the Pro-price-env seeding technique. A Basic owner (or lapsed/legacy-price owner) now 402s on bank features — intended, and gated behind the seed-prices/heal-row deploy precondition.

**Out of scope for B2:** the write-gate rollout (B3); background-sync owner-tier (B2-sync follow-up); any frontend/marketing (Plan C).
