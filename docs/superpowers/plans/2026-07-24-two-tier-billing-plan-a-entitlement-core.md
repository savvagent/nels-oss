# Two-Tier Billing — Plan A: Entitlement Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a single typed entitlement resolver (`status + price_id → {tier, payment_warning}`) and expose it on the subscription API — additive only, nothing enforced yet.

**Architecture:** A new pure module `backend/src/entitlement.rs` defines `Tier`, `Entitlement`, a `PriceCatalog` (four Stripe price ids → tier, resolved from env once), and a pure `resolve()` function. `billing::get_subscription` gains `tier` and `payment_warning` response fields alongside the existing `is_pro` (kept intact so admin, gates, and frontend keep working until Plans B and C swap them). No gating logic changes in this plan.

**Tech Stack:** Rust, axum, sqlx (Postgres), serde. Tests are pure `#[test]` units (no DB, no `#[ignore]`), run via `cargo test --bin backend`.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 1).

## Global Constraints

- Backend is a **bin-only crate**: run tests with `cargo test --bin backend`, never `--lib`. (Pure unit tests here need no DB / no podman.)
- No self-attribution in commits: no `Co-Authored-By: Claude`, no "Generated with" footer, no emoji trailers.
- `resolve()` MUST be a pure function (no DB, no env reads inside it) — env access lives only in `PriceCatalog::from_env()`. This keeps the status/price table unit-testable without a database or environment.
- Tier ordering is `None < Basic < Pro` (derive `PartialOrd`, declare variants in that order).
- Grace set (entitled statuses) = `trialing`, `active`, `past_due`. Every other status, and a missing row, is `Tier::None`.
- `past_due` sets `payment_warning = true`; all other entitled statuses set it `false`.
- Fail-safe: an entitled status with an **unrecognized** `price_id` resolves to `Tier::Basic` and logs `tracing::error!` (never Pro, never locked out).
- `trialing` always resolves to `Tier::Pro` regardless of `price_id` (the trial grants Pro; the price may not yet be readable on the event that created the row).
- Price env var names (exact): `STRIPE_PRICE_BASIC_MONTHLY`, `STRIPE_PRICE_BASIC_ANNUAL`, `STRIPE_PRICE_PRO_MONTHLY`, `STRIPE_PRICE_PRO_ANNUAL`.
- This plan is **additive and non-enforcing**: `billing::user_is_pro` stays as-is; existing gates and `admin.rs` are untouched.

---

## File Structure

- **Create:** `backend/src/entitlement.rs` — `Tier`, `Entitlement`, `PriceCatalog`, `resolve()`, and all their unit tests. Single responsibility: entitlement derivation. No I/O except `PriceCatalog::from_env()`.
- **Modify:** `backend/src/main.rs:40` (module list) — register `mod entitlement;`.
- **Modify:** `backend/src/billing.rs:438-466` — extend `SubscriptionResponse` and `get_subscription` to include `tier` + `payment_warning`.

---

### Task 1: `Tier` enum + `PriceCatalog` (env → tier, fail-safe)

**Files:**
- Create: `backend/src/entitlement.rs`
- Modify: `backend/src/main.rs:40`

**Interfaces:**
- Produces:
  - `pub enum Tier { None, Basic, Pro }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]`, `#[serde(rename_all = "lowercase")]`.
  - `pub struct PriceCatalog { basic: Vec<String>, pro: Vec<String> }`
  - `impl PriceCatalog { pub fn from_env() -> Self; pub fn new(basic: Vec<String>, pro: Vec<String>) -> Self; pub fn tier_for(&self, price_id: Option<&str>) -> Option<Tier> }`
    where `tier_for` returns `Some(Tier::Pro)` / `Some(Tier::Basic)` for a known id, and `None` for an unknown or absent id (the "unrecognized price" signal `resolve()` interprets). `new()` is the test seam; `from_env()` reads the four env vars (skipping any unset) and delegates to `new()`.

- [ ] **Step 1: Register the module**

In `backend/src/main.rs`, after line 40 (`mod duplicate_match;`), add:

```rust
mod entitlement;
```

- [ ] **Step 2: Write the failing tests for `PriceCatalog`**

Create `backend/src/entitlement.rs` with:

```rust
//! Entitlement derivation (two-tier billing). Pure logic: `resolve()` maps a
//! Stripe (status, price_id) pair to an `Entitlement`. The only I/O is
//! `PriceCatalog::from_env()`, which reads the four price env vars once.

/// Feature tier granted by a subscription. Ordered `None < Basic < Pro`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    None,
    Basic,
    Pro,
}

/// Known Stripe price ids grouped by the tier they grant. Built from env once
/// (`from_env`) or explicitly in tests (`new`).
pub struct PriceCatalog {
    basic: Vec<String>,
    pro: Vec<String>,
}

impl PriceCatalog {
    pub fn new(basic: Vec<String>, pro: Vec<String>) -> Self {
        Self { basic, pro }
    }

    /// Reads STRIPE_PRICE_BASIC_MONTHLY/ANNUAL and STRIPE_PRICE_PRO_MONTHLY/ANNUAL.
    /// Unset vars are simply omitted from the catalog.
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let basic = ["STRIPE_PRICE_BASIC_MONTHLY", "STRIPE_PRICE_BASIC_ANNUAL"]
            .iter().filter_map(|k| get(k)).collect();
        let pro = ["STRIPE_PRICE_PRO_MONTHLY", "STRIPE_PRICE_PRO_ANNUAL"]
            .iter().filter_map(|k| get(k)).collect();
        Self::new(basic, pro)
    }

    /// `Some(tier)` for a known price id; `None` for unknown/absent (the
    /// "unrecognized price" signal `resolve()` handles via the fail-safe rule).
    pub fn tier_for(&self, price_id: Option<&str>) -> Option<Tier> {
        let id = price_id?;
        if self.pro.iter().any(|p| p == id) {
            Some(Tier::Pro)
        } else if self.basic.iter().any(|b| b == id) {
            Some(Tier::Basic)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> PriceCatalog {
        PriceCatalog::new(
            vec!["price_basic_m".into(), "price_basic_a".into()],
            vec!["price_pro_m".into(), "price_pro_a".into()],
        )
    }

    #[test]
    fn tier_ordering_none_lt_basic_lt_pro() {
        assert!(Tier::None < Tier::Basic);
        assert!(Tier::Basic < Tier::Pro);
    }

    #[test]
    fn tier_for_known_pro_price() {
        assert_eq!(catalog().tier_for(Some("price_pro_a")), Some(Tier::Pro));
    }

    #[test]
    fn tier_for_known_basic_price() {
        assert_eq!(catalog().tier_for(Some("price_basic_m")), Some(Tier::Basic));
    }

    #[test]
    fn tier_for_unknown_price_is_none() {
        assert_eq!(catalog().tier_for(Some("price_wat")), None);
    }

    #[test]
    fn tier_for_absent_price_is_none() {
        assert_eq!(catalog().tier_for(None), None);
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --bin backend entitlement::tests -- --nocapture`
Expected: 5 tests pass (`tier_ordering_*`, `tier_for_*`). (These are straight-through; the "failing" state would only appear if the module didn't compile.)

- [ ] **Step 4: Commit**

```bash
git add backend/src/entitlement.rs backend/src/main.rs
git commit -m "feat(billing): add Tier and PriceCatalog for entitlement derivation"
```

---

### Task 2: `Entitlement` struct + `resolve()` with the full status table

**Files:**
- Modify: `backend/src/entitlement.rs`

**Interfaces:**
- Consumes: `Tier`, `PriceCatalog` (Task 1).
- Produces:
  - `pub struct Entitlement { pub tier: Tier, pub payment_warning: bool }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`.
  - `pub fn resolve(status: Option<&str>, price_id: Option<&str>, catalog: &PriceCatalog) -> Entitlement`

- [ ] **Step 1: Write the failing tests**

In `backend/src/entitlement.rs`, add these to the `tests` module (above the closing `}`):

```rust
    #[test]
    fn resolve_no_subscription_is_none() {
        let e = resolve(None, None, &catalog());
        assert_eq!(e, Entitlement { tier: Tier::None, payment_warning: false });
    }

    #[test]
    fn resolve_trialing_is_pro_regardless_of_price() {
        // Trial grants Pro even if the price is a Basic id or absent.
        assert_eq!(resolve(Some("trialing"), None, &catalog()).tier, Tier::Pro);
        assert_eq!(resolve(Some("trialing"), Some("price_basic_m"), &catalog()).tier, Tier::Pro);
        assert!(!resolve(Some("trialing"), None, &catalog()).payment_warning);
    }

    #[test]
    fn resolve_active_uses_price_tier() {
        assert_eq!(resolve(Some("active"), Some("price_pro_a"), &catalog()).tier, Tier::Pro);
        assert_eq!(resolve(Some("active"), Some("price_basic_a"), &catalog()).tier, Tier::Basic);
    }

    #[test]
    fn resolve_active_unknown_price_fails_safe_to_basic() {
        let e = resolve(Some("active"), Some("price_unknown"), &catalog());
        assert_eq!(e.tier, Tier::Basic);
        assert!(!e.payment_warning);
    }

    #[test]
    fn resolve_active_missing_price_fails_safe_to_basic() {
        // Entitled but no price id at all → Basic (never locked out, never Pro).
        assert_eq!(resolve(Some("active"), None, &catalog()).tier, Tier::Basic);
    }

    #[test]
    fn resolve_past_due_keeps_tier_and_warns() {
        let e = resolve(Some("past_due"), Some("price_pro_m"), &catalog());
        assert_eq!(e.tier, Tier::Pro);
        assert!(e.payment_warning, "past_due must set the payment warning");
    }

    #[test]
    fn resolve_past_due_unknown_price_is_basic_and_warns() {
        let e = resolve(Some("past_due"), Some("price_unknown"), &catalog());
        assert_eq!(e.tier, Tier::Basic);
        assert!(e.payment_warning);
    }

    #[test]
    fn resolve_terminal_statuses_are_none() {
        for s in ["canceled", "unpaid", "incomplete", "incomplete_expired", "paused", "bogus"] {
            let e = resolve(Some(s), Some("price_pro_a"), &catalog());
            assert_eq!(e, Entitlement { tier: Tier::None, payment_warning: false }, "{s} must be None");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin backend entitlement::tests::resolve -- --nocapture`
Expected: FAIL to compile — `cannot find function \`resolve\`` and `cannot find struct \`Entitlement\``.

- [ ] **Step 3: Implement `Entitlement` + `resolve()`**

In `backend/src/entitlement.rs`, add after the `PriceCatalog` impl (before the `#[cfg(test)]` module):

```rust
/// Resolved entitlement for a subscription row (or absence of one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entitlement {
    pub tier: Tier,
    /// True only during `past_due` grace — the UI shows "update your card".
    pub payment_warning: bool,
}

const NONE: Entitlement = Entitlement { tier: Tier::None, payment_warning: false };

/// Map a persisted Stripe (status, price_id) pair to an `Entitlement`.
/// Pure — all environment/config arrives via `catalog`. See the module docs and
/// the spec's Section 1 status table.
pub fn resolve(status: Option<&str>, price_id: Option<&str>, catalog: &PriceCatalog) -> Entitlement {
    let (entitled, warning) = match status {
        Some("trialing") => return Entitlement { tier: Tier::Pro, payment_warning: false },
        Some("active") => (true, false),
        Some("past_due") => (true, true),
        _ => (false, false), // canceled/unpaid/incomplete/incomplete_expired/paused/unknown/none
    };
    if !entitled {
        return NONE;
    }
    // Entitled: derive tier from price, failing safe to Basic on an unknown/absent id.
    let tier = match catalog.tier_for(price_id) {
        Some(t) => t,
        None => {
            tracing::error!(
                ?price_id,
                ?status,
                "entitled subscription with unrecognized price_id; failing safe to Basic"
            );
            Tier::Basic
        }
    };
    Entitlement { tier, payment_warning: warning }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --bin backend entitlement::tests -- --nocapture`
Expected: all tests pass (5 from Task 1 + 8 new `resolve_*`).

- [ ] **Step 5: Commit**

```bash
git add backend/src/entitlement.rs
git commit -m "feat(billing): add resolve() entitlement function with full status table"
```

---

### Task 3: Expose `tier` + `payment_warning` on the subscription API (additive)

**Files:**
- Modify: `backend/src/billing.rs:438-466` (`SubscriptionResponse` struct + `get_subscription` handler)

**Interfaces:**
- Consumes: `crate::entitlement::{resolve, PriceCatalog, Tier}` (Tasks 1–2).
- Produces: `GET /billing/subscription` JSON now additionally contains `"tier": "none"|"basic"|"pro"` and `"payment_warning": bool`. The existing `is_pro`, `status`, `price_id`, `current_period_end`, `cancel_at_period_end` fields are unchanged.

- [ ] **Step 1: Write the failing test**

The handler needs a DB (integration-style, `#[ignore]`), but the response *shaping* is pure and worth its own test. Add a pure helper and test it. In `backend/src/billing.rs`, inside the existing `#[cfg(test)] mod tests` block (near line 540), add:

```rust
    #[test]
    fn subscription_response_shapes_tier_and_warning() {
        use crate::entitlement::{PriceCatalog, Tier};
        let cat = PriceCatalog::new(vec!["b".into()], vec!["p".into()]);
        // active + pro price → is_pro true, tier pro, no warning
        let r = build_subscription_response(Some("active".into()), Some("p".into()), None, false, &cat);
        assert!(r.is_pro);
        assert_eq!(r.tier, Tier::Pro);
        assert!(!r.payment_warning);
        // past_due + basic price → is_pro false (legacy), tier basic, warning on
        let r = build_subscription_response(Some("past_due".into()), Some("b".into()), None, false, &cat);
        assert!(!r.is_pro, "legacy is_pro stays false for past_due");
        assert_eq!(r.tier, Tier::Basic);
        assert!(r.payment_warning);
        // no row → none
        let r = build_subscription_response(None, None, None, false, &cat);
        assert_eq!(r.tier, Tier::None);
        assert!(!r.is_pro);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --bin backend billing::tests::subscription_response_shapes -- --nocapture`
Expected: FAIL to compile — `cannot find function \`build_subscription_response\`` and `no field \`tier\``.

- [ ] **Step 3: Extend the struct and extract a pure builder**

In `backend/src/billing.rs`, change `SubscriptionResponse` (currently lines 438-445) to add two fields:

```rust
#[derive(Serialize)]
pub struct SubscriptionResponse {
    pub status: Option<String>,
    pub price_id: Option<String>,
    pub current_period_end: Option<chrono::DateTime<chrono::Utc>>,
    pub cancel_at_period_end: bool,
    pub is_pro: bool,
    pub tier: crate::entitlement::Tier,
    pub payment_warning: bool,
}
```

Add this pure builder just above `get_subscription` (before line 447):

```rust
/// Build the subscription API response from the persisted fields plus a price
/// catalog. Pure (no DB) so the field mapping is unit-testable. `is_pro` is kept
/// for backward compatibility with existing gates/admin/frontend (Plans B & C
/// migrate them); `tier`/`payment_warning` come from the new resolver.
fn build_subscription_response(
    status: Option<String>,
    price_id: Option<String>,
    current_period_end: Option<chrono::DateTime<chrono::Utc>>,
    cancel_at_period_end: bool,
    catalog: &crate::entitlement::PriceCatalog,
) -> SubscriptionResponse {
    let ent = crate::entitlement::resolve(status.as_deref(), price_id.as_deref(), catalog);
    SubscriptionResponse {
        is_pro: user_is_pro(status.as_deref()),
        tier: ent.tier,
        payment_warning: ent.payment_warning,
        status,
        price_id,
        current_period_end,
        cancel_at_period_end,
    }
}
```

- [ ] **Step 4: Rewire `get_subscription` to use the builder**

Replace the body of `get_subscription` (currently lines 447-466) with:

```rust
pub async fn get_subscription(
    State(state): State<AppState>,
    axum::Extension(user_id): axum::Extension<Uuid>,
) -> Result<Json<SubscriptionResponse>, (StatusCode, String)> {
    let row = sqlx::query_as::<_, crate::db::Subscription>(
        "SELECT * FROM subscriptions WHERE user_id = $1")
        .bind(user_id).fetch_optional(&state.db).await.map_err(internal_error)?;
    let catalog = crate::entitlement::PriceCatalog::from_env();
    Ok(Json(match row {
        Some(s) => build_subscription_response(
            s.status, s.price_id, s.current_period_end, s.cancel_at_period_end, &catalog),
        None => build_subscription_response(None, None, None, false, &catalog),
    }))
}
```

- [ ] **Step 5: Run the test and the full billing suite to verify green**

Run: `cargo test --bin backend billing::tests -- --nocapture`
Expected: `subscription_response_shapes_tier_and_warning` passes; all pre-existing `billing::tests` (signature, `user_is_pro_only_trialing_and_active`, `resolve_price_env_var_*`) still pass.

- [ ] **Step 6: Confirm the crate builds**

Run: `cargo build --bin backend`
Expected: builds clean (no warnings about unused `entitlement` items — `resolve`/`Tier`/`PriceCatalog` are now all reachable through `get_subscription`).

- [ ] **Step 7: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(billing): expose tier and payment_warning on subscription API"
```

---

## Self-Review

**Spec coverage (Section 1):**
- `Tier` enum `None<Basic<Pro` → Task 1 ✅
- `Entitlement { tier, payment_warning }` → Task 2 ✅
- `resolve(status, price_id)` pure, grace set incl. `past_due`, terminal→None → Task 2 ✅
- `trialing`→Pro always → Task 2 (`resolve_trialing_is_pro_regardless_of_price`) ✅
- Fail-safe unknown price→Basic + `tracing::error!` → Task 2 ✅
- Four price env vars → tier mapping → Task 1 (`PriceCatalog`) ✅
- Consumer helpers `budget_entitlement`/`require_tier` → **deferred to Plan B** (they need DB owner-lookup + gating; out of scope for the additive core). Noted, not a gap.
- API exposure of tier/warning → Task 3 ✅

**Placeholder scan:** none — every step has concrete code and exact commands.

**Type consistency:** `Tier`, `Entitlement`, `PriceCatalog::new/from_env/tier_for`, `resolve`, `build_subscription_response` names and signatures match across Tasks 1→2→3. `SubscriptionResponse` gains exactly the two documented fields. `serde(rename_all="lowercase")` on `Tier` yields the `"none"/"basic"/"pro"` JSON that Plan C's `route_billing_action` will consume.

**Out of scope for Plan A (by design):** all enforcement (write-gate, Pro-gate on owner), trial-on-first-budget, Stripe price/portal config, migration, and every frontend/marketing change. Those are Plans B and C.
