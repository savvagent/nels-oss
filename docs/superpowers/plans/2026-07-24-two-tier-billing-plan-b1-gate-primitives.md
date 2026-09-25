# Two-Tier Billing — Plan B1: Gate Primitives Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three DB-backed access helpers — `budget_entitlement`, `require_tier`, `require_writable_budget` — that resolve a budget's entitlement from its **owner** and gate on it, without wiring them into any handler yet (no behavior change).

**Architecture:** A new module `backend/src/access.rs` bridges the existing permission logic (`budget::check_permission` → `Permission`) and the shipped entitlement resolver (`entitlement::resolve`). Each public helper reads the price catalog from env via a thin wrapper over an inner `*_with(catalog)` form, so the DB tests stay hermetic (no process-env races) — the same pure-core + `from_env` wrapper pattern Plan A used. No call sites change in this plan; the helpers are dead code until Plan B2/B3 wire them.

**Tech Stack:** Rust, axum, sqlx (Postgres). Tests are `#[ignore]` DB-integration tests run against local pgvector.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 2). **Depends on:** Plan A (shipped as backend 0.35.0) — `crate::entitlement::{Tier, Entitlement, PriceCatalog, resolve}`.

## Plan B decomposition (context; only B1 is in this document)

- **B1 (this doc):** gate primitives + tests. No behavior change.
- **B2:** migrate the ~6 provider `require_pro(pool, user_id)` sites to owner-derived `require_tier(pool, budget_id, Tier::Pro)`. Enforcement scoped to bank features.
- **B3:** insert `require_writable_budget` at every budget-scoped write site (budget.rs inline Edit/Owner checks, goals.rs `require_edit`, ignore_rules.rs, provider `require_edit_or_owner`). The enforcement-turns-on step.
- **B4:** stop auto-creating the first budget (`auth.rs`); trial-on-first-budget trigger (three-way rule); checkout tier+cadence; 4-price env config. Gated behind the seed-prices precondition.

## Global Constraints

- Backend is a **bin-only crate**: run tests with `cargo test --bin backend`, never `--lib`. DB integration tests are `#[ignore]` and run with `-- --ignored`; they need local pgvector (podman, port 6153) reachable at `DATABASE_URL` (default `postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag`).
- No self-attribution in commits: no `Co-Authored-By: Claude`, no "Generated with" footer, no emoji trailers.
- **No handler/call-site changes in B1.** The three helpers are added but not invoked from any route. Expect dead-code warnings until B2/B3 — acceptable and expected for this plan (mark the module `#[allow(dead_code)]` at the item level is NOT wanted; a warning is fine and documents the pending wiring).
- Entitlement is always derived from the budget's **owner** (`budgets.owner_id`), never the calling user. A missing budget, or an owner with no subscription row, yields `Entitlement { tier: Tier::None, payment_warning: false }`.
- Testability: the public helpers read the catalog via `PriceCatalog::from_env()`; an inner `*_with(catalog: &PriceCatalog)` form takes it explicitly. Tests exercise the `*_with` forms with an explicit catalog so they never touch process env.
- HTTP contract: insufficient permission → `403 FORBIDDEN`; owner not entitled / tier too low → `402 PAYMENT_REQUIRED`. These status codes are load-bearing (the frontend's `isProGateError` keys on 402).
- `Tier` ordering is `None < Basic < Pro` (shipped in Plan A) — `require_tier` compares with `<`.

---

## File Structure

- **Create:** `backend/src/access.rs` — the three helpers, their inner `*_with` forms, and `#[ignore]` DB tests. Single responsibility: budget-owner entitlement gating. Depends on `budget::{check_permission, Permission}` and `entitlement::*`.
- **Modify:** `backend/src/main.rs` (module list) — register `mod access;`.

---

### Task 1: `budget_entitlement` — resolve a budget's entitlement from its owner

**Files:**
- Create: `backend/src/access.rs`
- Modify: `backend/src/main.rs` (add `mod access;` to the module list, after `mod entitlement;`)

**Interfaces:**
- Consumes: `crate::entitlement::{Tier, Entitlement, PriceCatalog, resolve}`, `crate::error::internal_error`.
- Produces:
  - `pub async fn budget_entitlement(pool: &PgPool, budget_id: Uuid) -> Result<Entitlement, (StatusCode, String)>`
  - `pub(crate) async fn budget_entitlement_with(pool: &PgPool, budget_id: Uuid, catalog: &PriceCatalog) -> Result<Entitlement, (StatusCode, String)>`

- [ ] **Step 1: Register the module**

In `backend/src/main.rs`, immediately after the `mod entitlement;` line (added in Plan A), add:

```rust
mod access;
```

- [ ] **Step 2: Write the module skeleton + failing DB tests**

Create `backend/src/access.rs`:

```rust
//! Budget-owner entitlement gating (two-tier billing, spec Section 2).
//!
//! A budget's entitlement is derived from its OWNER's subscription, not the
//! calling user — so viewers/editors inherit the owner's tier. These helpers
//! bridge `budget::check_permission` (who may act) with `entitlement::resolve`
//! (what the owner pays for). Public helpers read the price catalog from env;
//! the `*_with` forms take it explicitly for hermetic tests.

use axum::http::StatusCode;
use sqlx::PgPool;
use uuid::Uuid;

use crate::budget::{check_permission, Permission};
use crate::entitlement::{resolve, Entitlement, PriceCatalog, Tier};
use crate::error::internal_error;

const NONE: Entitlement = Entitlement { tier: Tier::None, payment_warning: false };

/// Entitlement of `budget_id`, derived from its owner's subscription. A missing
/// budget, or an owner with no subscription row, resolves to `Tier::None`.
pub async fn budget_entitlement(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<Entitlement, (StatusCode, String)> {
    budget_entitlement_with(pool, budget_id, &PriceCatalog::from_env()).await
}

/// `budget_entitlement` with an explicit catalog (hermetic tests / composition).
pub(crate) async fn budget_entitlement_with(
    pool: &PgPool,
    budget_id: Uuid,
    catalog: &PriceCatalog,
) -> Result<Entitlement, (StatusCode, String)> {
    let owner_id: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_id FROM budgets WHERE id = $1")
            .bind(budget_id)
            .fetch_optional(pool)
            .await
            .map_err(internal_error)?;
    let owner_id = match owner_id {
        Some(o) => o,
        None => return Ok(NONE), // no such budget → not entitled
    };
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, price_id FROM subscriptions WHERE user_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    let (status, price_id) = row.unwrap_or((None, None));
    Ok(resolve(status.as_deref(), price_id.as_deref(), catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn catalog() -> PriceCatalog {
        PriceCatalog::new(vec!["price_basic".into()], vec!["price_pro".into()])
    }

    async fn mk_user(db: &PgPool) -> Uuid {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
            .bind(uid)
            .bind(format!("access-{uid}@test.example"))
            .execute(db)
            .await
            .unwrap();
        uid
    }

    async fn mk_budget(db: &PgPool, owner: Uuid) -> Uuid {
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1, $2, 'B', 'monthly', 0)")
            .bind(bid)
            .bind(owner)
            .execute(db)
            .await
            .unwrap();
        bid
    }

    async fn mk_sub(db: &PgPool, user: Uuid, status: &str, price_id: Option<&str>) {
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status, price_id) VALUES ($1, $2, $3, $4)")
            .bind(user)
            .bind(format!("cus_{user}"))
            .bind(status)
            .bind(price_id)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_from_owner_active_pro() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let e = budget_entitlement_with(&db, bid, &catalog()).await.unwrap();
        assert_eq!(e.tier, Tier::Pro);
        assert!(!e.payment_warning);
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_from_owner_active_basic() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await;
        let bid = mk_budget(&db, owner).await;
        assert_eq!(budget_entitlement_with(&db, bid, &catalog()).await.unwrap().tier, Tier::Basic);
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_owner_past_due_keeps_tier_and_warns() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "past_due", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let e = budget_entitlement_with(&db, bid, &catalog()).await.unwrap();
        assert_eq!(e.tier, Tier::Pro);
        assert!(e.payment_warning);
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_owner_canceled_is_none() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        assert_eq!(budget_entitlement_with(&db, bid, &catalog()).await.unwrap().tier, Tier::None);
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_owner_no_subscription_is_none() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        let bid = mk_budget(&db, owner).await;
        assert_eq!(budget_entitlement_with(&db, bid, &catalog()).await.unwrap().tier, Tier::None);
    }

    #[tokio::test]
    #[ignore]
    async fn entitlement_missing_budget_is_none() {
        let db = test_pool().await;
        assert_eq!(budget_entitlement_with(&db, Uuid::new_v4(), &catalog()).await.unwrap().tier, Tier::None);
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Ensure pgvector is up (podman, port 6153), then run:
`cargo test --bin backend access::tests -- --ignored --nocapture`
Expected: 6 tests pass (`entitlement_*`). If the DB is unreachable, start it first (see the project's DB-test memory note).

- [ ] **Step 4: Confirm the crate builds**

Run: `cargo build --bin backend`
Expected: builds. A `dead_code` warning for `budget_entitlement` (public but not yet called) is expected and acceptable in B1.

- [ ] **Step 5: Commit**

```bash
git add backend/src/access.rs backend/src/main.rs
git commit -m "feat(billing): add budget_entitlement owner-derived resolver"
```

---

### Task 2: `require_tier` — 402 unless the budget owner meets a minimum tier

**Files:**
- Modify: `backend/src/access.rs`

**Interfaces:**
- Consumes: `budget_entitlement_with` (Task 1), `Tier`.
- Produces:
  - `pub async fn require_tier(pool: &PgPool, budget_id: Uuid, min: Tier) -> Result<(), (StatusCode, String)>`
  - `pub(crate) async fn require_tier_with(pool: &PgPool, budget_id: Uuid, min: Tier, catalog: &PriceCatalog) -> Result<(), (StatusCode, String)>`

- [ ] **Step 1: Write the failing DB tests**

Add to the `tests` module in `backend/src/access.rs` (before its closing `}`):

```rust
    #[tokio::test]
    #[ignore]
    async fn require_tier_pro_allows_pro_owner() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        assert!(require_tier_with(&db, bid, Tier::Pro, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn require_tier_pro_rejects_basic_owner_with_402() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await;
        let bid = mk_budget(&db, owner).await;
        let err = require_tier_with(&db, bid, Tier::Pro, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn require_tier_pro_rejects_unsubscribed_owner_with_402() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        let bid = mk_budget(&db, owner).await;
        let err = require_tier_with(&db, bid, Tier::Pro, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn require_tier_basic_allows_basic_owner() {
        // A Basic owner clears a Basic-minimum gate (ordering: Basic >= Basic).
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await;
        let bid = mk_budget(&db, owner).await;
        assert!(require_tier_with(&db, bid, Tier::Basic, &catalog()).await.is_ok());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin backend access::tests::require_tier -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function \`require_tier_with\``.

- [ ] **Step 3: Implement `require_tier` + `require_tier_with`**

In `backend/src/access.rs`, add after `budget_entitlement_with` (before the `#[cfg(test)]` module):

```rust
/// 402 unless `budget_id`'s owner is entitled at `min` tier or higher.
/// Used for feature gates above the write floor (e.g. Pro-only bank sync).
pub async fn require_tier(
    pool: &PgPool,
    budget_id: Uuid,
    min: Tier,
) -> Result<(), (StatusCode, String)> {
    require_tier_with(pool, budget_id, min, &PriceCatalog::from_env()).await
}

pub(crate) async fn require_tier_with(
    pool: &PgPool,
    budget_id: Uuid,
    min: Tier,
    catalog: &PriceCatalog,
) -> Result<(), (StatusCode, String)> {
    let ent = budget_entitlement_with(pool, budget_id, catalog).await?;
    if ent.tier < min {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            "This feature requires a higher Nels plan.".to_string(),
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --bin backend access::tests::require_tier -- --ignored --nocapture`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/access.rs
git commit -m "feat(billing): add require_tier owner minimum-tier gate"
```

---

### Task 3: `require_writable_budget` — Edit/Owner AND owner entitled

**Files:**
- Modify: `backend/src/access.rs`

**Interfaces:**
- Consumes: `check_permission`, `Permission`, `budget_entitlement_with`.
- Produces:
  - `pub async fn require_writable_budget(pool: &PgPool, user_id: Uuid, budget_id: Uuid) -> Result<(), (StatusCode, String)>`
  - `pub(crate) async fn require_writable_budget_with(pool: &PgPool, user_id: Uuid, budget_id: Uuid, catalog: &PriceCatalog) -> Result<(), (StatusCode, String)>`

- [ ] **Step 1: Write the failing DB tests**

Add a share helper and tests to the `tests` module in `backend/src/access.rs`:

```rust
    async fn mk_share(db: &PgPool, budget_id: Uuid, email: &str, level: &str) {
        sqlx::query("INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::new_v4())
            .bind(budget_id)
            .bind(email)
            .bind(level)
            .execute(db)
            .await
            .unwrap();
    }

    async fn email_of(db: &PgPool, uid: Uuid) -> String {
        sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(uid).fetch_one(db).await.unwrap()
    }

    #[tokio::test]
    #[ignore]
    async fn writable_owner_entitled_ok() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await;
        let bid = mk_budget(&db, owner).await;
        assert!(require_writable_budget_with(&db, owner, bid, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn writable_editor_on_entitled_ok() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let editor = mk_user(&db).await;
        mk_share(&db, bid, &email_of(&db, editor).await, "edit").await;
        assert!(require_writable_budget_with(&db, editor, bid, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn writable_viewer_forbidden_403() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let viewer = mk_user(&db).await;
        mk_share(&db, bid, &email_of(&db, viewer).await, "view").await;
        let err = require_writable_budget_with(&db, viewer, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[ignore]
    async fn writable_owner_lapsed_is_402() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let err = require_writable_budget_with(&db, owner, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn writable_editor_on_lapsed_is_402() {
        // Editor has Edit permission, but the OWNER lapsed → budget read-only.
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let editor = mk_user(&db).await;
        mk_share(&db, bid, &email_of(&db, editor).await, "edit").await;
        let err = require_writable_budget_with(&db, editor, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn writable_non_member_forbidden_403() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let stranger = mk_user(&db).await;
        let err = require_writable_budget_with(&db, stranger, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin backend access::tests::writable -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function \`require_writable_budget_with\``.

- [ ] **Step 3: Implement `require_writable_budget` + `require_writable_budget_with`**

In `backend/src/access.rs`, add after `require_tier_with` (before the `#[cfg(test)]` module):

```rust
/// Full write-gate: the caller must have Edit/Owner permission AND the budget's
/// owner must be entitled (tier != None). Insufficient permission → 403;
/// lapsed owner → 402 (budget is read-only). Reads must NOT use this — they
/// call `check_permission` directly so a lapsed budget stays viewable.
pub async fn require_writable_budget(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    require_writable_budget_with(pool, user_id, budget_id, &PriceCatalog::from_env()).await
}

pub(crate) async fn require_writable_budget_with(
    pool: &PgPool,
    user_id: Uuid,
    budget_id: Uuid,
    catalog: &PriceCatalog,
) -> Result<(), (StatusCode, String)> {
    let perm = check_permission(pool, user_id, budget_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((
            StatusCode::FORBIDDEN,
            "You do not have permission to modify this budget".to_string(),
        ));
    }
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

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --bin backend access::tests::writable -- --ignored --nocapture`
Expected: 6 tests pass.

- [ ] **Step 5: Run the full access suite + build**

Run: `cargo test --bin backend access::tests -- --ignored --nocapture`
Expected: all 16 tests pass (6 entitlement + 4 require_tier + 6 writable).
Run: `cargo build --bin backend`
Expected: builds (dead-code warnings for the still-unwired public helpers are expected in B1).

- [ ] **Step 6: Commit**

```bash
git add backend/src/access.rs
git commit -m "feat(billing): add require_writable_budget write-gate helper"
```

---

## Self-Review

**Spec coverage (Section 2 primitives):**
- `budget_entitlement(budget_id)` resolving the OWNER's row → Task 1 ✅
- `require_tier(budget_id, Pro)` owner-derived Pro gate → Task 2 ✅
- `require_writable_budget` = Edit/Owner (403) + owner entitled (402), reads excluded → Task 3 ✅
- Owner-derived (not caller), missing budget/owner-no-sub → None → Task 1 tests ✅
- 402/403 status contract → Tasks 2 & 3 tests ✅
- Wiring into handlers → **deferred to B2/B3** (by design; B1 changes no behavior).

**Placeholder scan:** none — every step has concrete code and exact commands.

**Type consistency:** `budget_entitlement[_with]`, `require_tier[_with]`, `require_writable_budget[_with]` signatures are consistent across tasks; all `*_with` forms take `&PriceCatalog` last; helpers return `Result<_, (StatusCode, String)>` matching the existing handler error convention. `check_permission`/`Permission` are used exactly as in the existing provider `require_edit_or_owner` bodies (same `.map_err(|e| (INTERNAL_SERVER_ERROR, e))` shape). `Tier` comparison uses the shipped `None < Basic < Pro` ordering.

**Out of scope for B1 (by design):** all handler wiring, the provider `require_pro` swap (B2), the write-site rollout (B3), trial/checkout/config (B4), and any frontend/marketing change (Plan C).
