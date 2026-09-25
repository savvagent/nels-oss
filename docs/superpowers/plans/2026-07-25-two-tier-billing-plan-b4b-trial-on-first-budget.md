# Two-Tier Billing — Plan B4b: Trial-on-First-Budget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop auto-creating a budget at registration; instead, the moment a user creates their **first** budget, start a 7-day no-card **Pro trial** server-side (or block a lapsed user), so being a budget owner is tied to having a subscription — while pure viewers stay free.

**Architecture:** A shared billing helper `ensure_ready_to_own_budget(db, user_id)` runs the three-way gate before either budget-creation path inserts a row: **never-subscribed** → create a trial subscription in Stripe (webhook lands `trialing`); **currently entitled** → allow; **lapsed** → block (402, resubscribe). Trial creation goes through a new `start_trial_subscription` that reuses `ensure_customer` + `stripe_post`. Entitlement status is written **only by the existing webhook** (per the webhook-only decision) — this plan creates the Stripe subscription and lets `customer.subscription.created` flip the row. `auth.rs` stops auto-creating "My First Budget."

**Tech Stack:** Rust, axum, sqlx, wiremock (Stripe mocking). Tests are pure/wiremock unit tests plus `#[ignore]` DB tests.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 2, "Trial trigger"). **Depends on:** Plan A (shipped), B4a (merged — the 4 price env vars + `resolve_price_env_var`). Independent of B1/B2/B3 code, but see the Deploy gate.

## Design decisions baked in (from brainstorming)

- **Webhook-only status:** `start_trial_subscription` creates the Stripe subscription; the existing `customer.subscription.created` webhook writes `status='trialing'`. No optimistic local status write. Accepted cost: a few-seconds lag before the row shows `trialing` (harmless until B3 enforces; a papercut then, self-healing).
- **Idempotency = mirror `ensure_customer`:** no new locking. The trigger only fires when the row is "never subscribed" (`status IS NULL AND stripe_subscription_id IS NULL`); a rare concurrent double-fire creates a second harmless trial (both cancel at trial end) and logs a warning — the same tolerance `ensure_customer` already applies to duplicate customers.
- **Trial price:** `STRIPE_PRICE_PRO_MONTHLY`. Entitlement resolves `trialing → Pro` regardless of price, so the attached price only matters if the user adds a card during the trial.
- **Trial params:** `trial_period_days = <trial_days()>` (existing helper, default 7), `payment_method_collection = if_required` (no card), `trial_settings[end_behavior][missing_payment_method] = cancel` (clean expiry to `canceled`, never `past_due`).

## Global Constraints

- Backend is a **bin-only crate**: `cargo test --bin backend`, never `--lib`. DB tests are `#[ignore]` (`--ignored`, pgvector port 6153); Stripe-touching tests use `wiremock` + `set_stripe_env` (no DB unless the test also needs one).
- No self-attribution in commits.
- The trigger fires only on the user's **first** budget (they own zero). Owning ≥1 already means they passed this gate before — do not re-trigger.
- Three-way rule (exact):
  - owns 0 budgets **and** never subscribed (`status IS NULL AND stripe_subscription_id IS NULL`, or no row) → `start_trial_subscription`, then allow.
  - owns 0 budgets **and** currently entitled (`entitlement::resolve(status, price_id, from_env).tier != None`) → allow (no new trial).
  - owns 0 budgets **and** lapsed (has a row, terminal status, not entitled) → **block** with `402` "Your subscription ended — resubscribe to create a budget." (no second free trial).
  - owns ≥1 budget → allow (normal path).
- Both budget-creation paths (`budget::create_budget` REST, and `rag.rs` `CREATE_BUDGET` chat) call the shared helper **before** inserting, and surface its error (REST → `?` the `(StatusCode,String)`; chat → set `mutation_error` and skip the INSERT, mirroring the arm's existing validation-failure handling).
- If `start_trial_subscription` fails (Stripe down), the helper returns an error and budget creation is **blocked** (retryable) — never create an un-entitled owner.

## Deploy gate (not a code task)

Ships behavior only after the four `STRIPE_PRICE_*` secrets are seeded on Fly `nels-api` (same gate as B4a). `start_trial_subscription` reads `STRIPE_PRICE_PRO_MONTHLY`; without it, first-budget creation 503s. Keep batched with the held release (#438) until prices are seeded.

---

## File Structure

- **Modify:** `backend/src/billing.rs` — add `start_trial_subscription` + `ensure_ready_to_own_budget` + tests.
- **Modify:** `backend/src/auth.rs` — remove the auto-create "My First Budget" block; adjust the register handler + its tests.
- **Modify:** `backend/src/budget.rs` (`create_budget`) — call the helper before the insert transaction.
- **Modify:** `backend/src/rag.rs` (`CREATE_BUDGET` arm) — call the helper before its INSERT.

---

### Task 1: `start_trial_subscription` — create a no-card Pro trial in Stripe

**Files:**
- Modify: `backend/src/billing.rs` (new fn + wiremock tests)

**Interfaces:**
- Consumes: `ensure_customer`, `stripe_post`, `env_opt`, `trial_days`.
- Produces: `pub(crate) async fn start_trial_subscription(db: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)>`

- [ ] **Step 1: Write the failing wiremock test**

In `backend/src/billing.rs`'s `#[cfg(test)] mod tests`, add (the harness helpers `set_stripe_env`/`clear_stripe_env` already exist; this test needs a DB for `ensure_customer`, so it is `#[ignore]` and uses the same `test_pool` pattern used elsewhere — add a local `test_pool` + `mk_user` if not already present in this module; if `billing::tests` has none, include them):

```rust
    #[tokio::test]
    #[ignore]
    async fn start_trial_posts_subscription_with_trial_and_cancel_behavior() {
        use wiremock::matchers::{method, path, body_string_contains};
        let server = MockServer::start().await;
        set_stripe_env(&server);
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        // customer creation (ensure_customer) + subscription creation.
        Mock::given(method("POST")).and(path("v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_trial"})))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("v1/subscriptions"))
            .and(body_string_contains("trial_period_days"))
            .and(body_string_contains("missing_payment_method"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"sub_trial","status":"trialing"})))
            .expect(1)
            .mount(&server).await;

        let db = test_pool().await;
        let uid = mk_user(&db).await;
        let r = start_trial_subscription(&db, uid).await;
        assert!(r.is_ok(), "start_trial should succeed: {r:?}");
        clear_stripe_env(&server);
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
        // wiremock verifies .expect(1) on the subscriptions POST at drop
    }
```

If `billing::tests` lacks `test_pool`/`mk_user`, add them (mirroring `access.rs`):

```rust
    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_|
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }
    async fn mk_user(db: &PgPool) -> Uuid {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'x')")
            .bind(uid).bind(format!("bill-{uid}@test.example")).execute(db).await.unwrap();
        uid
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin backend billing::tests::start_trial -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function start_trial_subscription`.

- [ ] **Step 3: Implement `start_trial_subscription`**

Add to `backend/src/billing.rs` (near `checkout`/`ensure_customer`):

```rust
/// Start a 7-day, no-card **Pro** trial subscription server-side (no Checkout
/// redirect). The `customer.subscription.created` webhook then writes
/// `status='trialing'` — this function does NOT persist status itself
/// (webhook is the single source of entitlement truth). Trial expires cleanly
/// to `canceled` (no card) via `end_behavior.missing_payment_method=cancel`.
pub(crate) async fn start_trial_subscription(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let price = env_opt("STRIPE_PRICE_PRO_MONTHLY")
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
    let customer = ensure_customer(db, user_id).await?;
    let mut form = vec![
        ("customer".to_string(), customer),
        ("items[0][price]".to_string(), price),
        ("items[0][quantity]".to_string(), "1".to_string()),
        ("payment_method_collection".to_string(), "if_required".to_string()),
        ("trial_settings[end_behavior][missing_payment_method]".to_string(), "cancel".to_string()),
    ];
    let trial = trial_days();
    if trial >= 1 {
        form.push(("trial_period_days".to_string(), trial.to_string()));
    }
    // Fire-and-persist-via-webhook: we ignore the returned body beyond success.
    stripe_post("v1/subscriptions", &form).await?;
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin backend billing::tests::start_trial -- --ignored --nocapture`
Expected: PASS (wiremock confirms the `v1/subscriptions` POST fired once with the trial + cancel-behavior fields).

- [ ] **Step 5: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(billing): add start_trial_subscription (no-card Pro trial)"
```

---

### Task 2: `ensure_ready_to_own_budget` — the three-way first-budget gate

**Files:**
- Modify: `backend/src/billing.rs` (new fn + `#[ignore]` DB/wiremock tests)

**Interfaces:**
- Consumes: `start_trial_subscription` (Task 1), `entitlement::{resolve, PriceCatalog}`.
- Produces: `pub(crate) async fn ensure_ready_to_own_budget(db: &PgPool, user_id: Uuid) -> Result<(), (StatusCode, String)>`

- [ ] **Step 1: Write the failing DB tests**

Add to `billing::tests` (reusing `test_pool`/`mk_user` from Task 1; add `mk_budget`/`mk_sub` mirroring `access.rs` if absent):

```rust
    #[tokio::test]
    #[ignore]
    async fn ready_to_own_existing_owner_is_ok_no_stripe() {
        // Owns >=1 budget already → allowed, no Stripe call needed.
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) VALUES ($1,$2,'B','monthly',0)")
            .bind(Uuid::new_v4()).bind(uid).execute(&db).await.unwrap();
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn ready_to_own_lapsed_is_blocked_402() {
        // Owns 0 budgets, has a canceled subscription → blocked (no second trial).
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status, stripe_subscription_id) VALUES ($1,$2,'canceled','sub_old')")
            .bind(uid).bind(format!("cus_{uid}")).execute(&db).await.unwrap();
        let err = ensure_ready_to_own_budget(&db, uid).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    #[ignore]
    async fn ready_to_own_entitled_no_budget_is_ok() {
        // Owns 0 budgets but is entitled (e.g. deleted all budgets while active) → allowed, no new trial.
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, status, price_id, stripe_subscription_id) VALUES ($1,$2,'active','price_pro_m','sub_live')")
            .bind(uid).bind(format!("cus_{uid}")).execute(&db).await.unwrap();
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
    }

    #[tokio::test]
    #[ignore]
    async fn ready_to_own_never_subscribed_starts_trial() {
        // Owns 0 budgets, no subscription row → starts a trial (Stripe mocked).
        use wiremock::matchers::{method, path};
        let server = MockServer::start().await;
        set_stripe_env(&server);
        std::env::set_var("STRIPE_PRICE_PRO_MONTHLY", "price_pro_m");
        Mock::given(method("POST")).and(path("v1/customers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"cus_new"}))).mount(&server).await;
        Mock::given(method("POST")).and(path("v1/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"sub_new","status":"trialing"})))
            .expect(1).mount(&server).await;
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        assert!(ensure_ready_to_own_budget(&db, uid).await.is_ok());
        clear_stripe_env(&server);
        std::env::remove_var("STRIPE_PRICE_PRO_MONTHLY");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin backend billing::tests::ready_to_own -- --ignored --nocapture`
Expected: FAIL to compile — `cannot find function ensure_ready_to_own_budget`.

- [ ] **Step 3: Implement `ensure_ready_to_own_budget`**

```rust
/// Gate run before a user creates a budget. Enforces the two-tier "first budget
/// starts a trial" rule (spec Section 2):
/// - owns >=1 budget → Ok (already an owner; normal path).
/// - owns 0 budgets, never subscribed → start a Pro trial, then Ok.
/// - owns 0 budgets, currently entitled → Ok (no new trial).
/// - owns 0 budgets, lapsed → Err(402) (resubscribe; no second free trial).
/// A Stripe failure while starting the trial returns Err (budget creation is
/// blocked rather than creating an un-entitled owner).
pub(crate) async fn ensure_ready_to_own_budget(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM budgets WHERE owner_id = $1")
        .bind(user_id).fetch_one(db).await.map_err(internal_error)?;
    if owned > 0 {
        return Ok(()); // already an owner
    }
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, price_id, stripe_subscription_id FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(db).await.map_err(internal_error)?;
    let (status, price_id, sub_id) = row.unwrap_or((None, None, None));
    // Never subscribed: no row, or a bare ensure_customer placeholder
    // (status NULL and no subscription id yet).
    if status.is_none() && sub_id.is_none() {
        return start_trial_subscription(db, user_id).await;
    }
    // Has a subscription history: entitled → allow; lapsed → block.
    let ent = crate::entitlement::resolve(status.as_deref(), price_id.as_deref(),
        &crate::entitlement::PriceCatalog::from_env());
    if ent.tier == crate::entitlement::Tier::None {
        return Err((StatusCode::PAYMENT_REQUIRED,
            "Your subscription ended — resubscribe to create a budget.".to_string()));
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin backend billing::tests::ready_to_own -- --ignored --nocapture`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(billing): add ensure_ready_to_own_budget first-budget trial gate"
```

---

### Task 3: Stop auto-creating "My First Budget" at registration

**Files:**
- Modify: `backend/src/auth.rs` (remove the auto-create block; fix affected tests)

**Interfaces:**
- Produces: registration no longer inserts a default budget. Register response shape is unchanged (it already returns `AuthResponse { token, ... }`, not the budget).

- [ ] **Step 1: Find the auto-create block and any test that asserts it**

Run: `grep -n "My First Budget\|Auto-created default budget\|Automatically create a default budget" backend/src/auth.rs`
Expected: the `INSERT INTO budgets (... is_default ...) VALUES (... 'My First Budget' ...)` block (~lines 287-301) and its comment.

Also: `grep -rn "My First Budget\|first_budget\|default budget" backend/src/auth.rs` to find any register test asserting a budget exists after signup.

- [ ] **Step 2: Remove the auto-create block**

Delete the block that creates the default budget (the comment `// Automatically create a default budget for the user` through the end of that `INSERT INTO budgets ... .execute(&state.db).await;`). Registration proceeds directly from creating the session to returning `AuthResponse`. Leave all other registration logic (user insert, challenge consume, session create) intact.

- [ ] **Step 3: Fix affected tests**

If any `auth::tests` test asserts a budget exists after register (e.g. counts budgets, or reads "My First Budget"), update it to assert **zero** budgets after registration (the new contract: a fresh user owns no budgets until they create one). If no such test exists, note that in the report.

- [ ] **Step 4: Run the auth tests + build**

Run: `cargo test --bin backend auth:: -- --nocapture` (and `-- --ignored` if the affected tests are DB tests).
Expected: pass, with any budget-after-register assertion now expecting zero.
Run: `cargo build --bin backend` — clean.

- [ ] **Step 5: Commit**

```bash
git add backend/src/auth.rs
git commit -m "feat(billing): stop auto-creating a budget at registration"
```

---

### Task 4: Wire the gate into the REST `create_budget` handler

**Files:**
- Modify: `backend/src/budget.rs` (`create_budget`, ~line 1746)

**Interfaces:**
- Consumes: `crate::billing::ensure_ready_to_own_budget`.

- [ ] **Step 1: Add the gate call before the insert transaction**

In `create_budget`, after the up-front payload validation (budget_type/amount_mode/budget_strategy/auto_renew checks) and **before** `let mut tx = state.db.begin()...` (~line 1795), insert:

```rust
    // Two-tier billing: a user's FIRST budget starts a Pro trial (or is blocked
    // if their subscription lapsed). No-op for users who already own a budget.
    crate::billing::ensure_ready_to_own_budget(&state.db, user_id).await?;
```

- [ ] **Step 2: Add a DB test for the REST path**

Add an `#[ignore]` test in `budget::tests` that a brand-new user (no budgets, no subscription) creating a budget triggers the trial (Stripe mocked) and the budget is created; and that a lapsed user gets 402 and NO budget row is written. (Mirror the wiremock+DB setup from billing Task 2; call `create_budget` via its handler or a thin wrapper as other budget tests do.) If `budget::tests` has no handler-invocation harness, assert via `ensure_ready_to_own_budget` directly plus a follow-up insert — but prefer exercising `create_budget` if the harness exists.

- [ ] **Step 3: Run + build**

Run: `cargo test --bin backend budget::tests -- --ignored --nocapture` (the new test passes; existing create_budget tests still pass — note: existing tests create budgets for users with no subscription, so they will now hit the trial gate → **mock Stripe or seed an entitled/҂existing-owner subscription in those tests** if they start failing; fix as needed and report).
Run: `cargo build --bin backend`.

- [ ] **Step 4: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(billing): gate REST budget creation on first-budget trial"
```

---

### Task 5: Wire the gate into the chat `CREATE_BUDGET` arm

**Files:**
- Modify: `backend/src/rag.rs` (`CREATE_BUDGET` arm, ~line 2287)

**Interfaces:**
- Consumes: `crate::billing::ensure_ready_to_own_budget`.

- [ ] **Step 1: Add the gate before the chat INSERT**

In the `"CREATE_BUDGET"` arm, after the name/type validation and **before** the `INSERT INTO budgets` (~line 2369), call the gate and, on error, set `mutation_error` and skip the insert (mirroring how the arm handles an invalid `budget_type`):

```rust
                    // Two-tier billing: first budget starts a trial (or blocks a
                    // lapsed user). Mirror the validation-failure path — set
                    // mutation_error and skip the INSERT rather than 500.
                    if let Err((_, msg)) = crate::billing::ensure_ready_to_own_budget(&state.db, user_id).await {
                        mutation_error = Some(msg);
                    } else {
                        // ... existing INSERT INTO budgets + audit log ...
                    }
```

Match the arm's existing control flow exactly (the code already has a `type_valid` guard wrapping the INSERT; nest the entitlement guard analogously so a blocked user gets a clean chat error, not a panic or a partial write).

- [ ] **Step 2: Add a test for the chat path**

Add an `#[ignore]` test (or extend an existing CREATE_BUDGET test) asserting: a lapsed user's chat "create a budget" sets `mutation_error` (no budget row written); a never-subscribed user's chat create triggers the trial (Stripe mocked) and writes the budget. Use the rag test harness that exercises the action dispatch if present; otherwise test `ensure_ready_to_own_budget`'s branch and assert the arm wiring by inspection noted in the report.

- [ ] **Step 3: Run + build**

Run: `cargo test --bin backend rag::tests -- --ignored --nocapture` (new/updated test passes; existing CREATE_BUDGET tests still pass — same caveat as Task 4: existing chat-create tests for unsubscribed users will now hit the gate; seed an entitled subscription or mock Stripe in them as needed).
Run: `cargo build --bin backend`.

- [ ] **Step 4: Commit**

```bash
git add backend/src/rag.rs
git commit -m "feat(billing): gate chat budget creation on first-budget trial"
```

---

## Self-Review

**Spec coverage (Section 2 — trial trigger):**
- Registration no longer auto-creates a budget → Task 3 ✅
- First budget creation starts a Pro trial server-side (no redirect, no card, cancel end-behavior) → Tasks 1, 2, 4, 5 ✅
- Three-way rule (never-subscribed / entitled / lapsed) → Task 2 ✅ (+ Global Constraints, exact)
- Both creation paths (REST + chat) gated → Tasks 4, 5 ✅
- Stripe-down blocks creation (no un-entitled owner) → Task 2 impl (trial failure propagates) ✅
- Pure viewers never create a budget → never gated → by construction (the gate only runs in create paths) ✅
- Webhook-only status → Task 1 (no local status write) ✅

**Placeholder scan:** none — every step has concrete code/commands. Tasks 4/5's test steps describe the harness choice explicitly and require the implementer to fix any existing create-budget tests that now hit the gate (a real, expected consequence, not a placeholder).

**Type consistency:** `ensure_ready_to_own_budget(&PgPool, Uuid) -> Result<(), (StatusCode, String)>` and `start_trial_subscription(&PgPool, Uuid) -> Result<(), (StatusCode,String)>` are used identically across billing, budget, and rag. `entitlement::{resolve, PriceCatalog, Tier}` are the shipped Plan A symbols. The chat arm sets `mutation_error: Option<String>` (existing variable).

**Known consequence to handle in-task (not a gap):** existing `create_budget`/`CREATE_BUDGET` tests create budgets for users with no subscription; once the gate is wired, those users hit the trial path. Tasks 4/5 explicitly require updating those tests (mock Stripe or seed an entitled/owner state). Flag any that can't be cleanly fixed.

**Out of scope for B4b (by design):** the write-gate (`require_writable_budget`) and Pro-gate (`require_tier`) wiring (B2/B3); frontend empty-state for a budget-less new user, and the resubscribe CTA copy (Plan C); hardening the concurrent-double-trial race beyond the accepted `ensure_customer`-style tolerance.
