# Two-Tier Billing — Plan B4a: Price Config & Tiered Checkout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Teach checkout the two tiers — map `(tier, cadence)` to the four Stripe price env vars and let `POST /billing/checkout` accept a `tier`, so a caller can start a Basic **or** Pro subscription.

**Architecture:** Extend the existing pure `resolve_price_env_var` from `(cadence, country)` to `(tier, cadence, country)` returning one of four `STRIPE_PRICE_{BASIC,PRO}_{MONTHLY,ANNUAL}` env var names; default an absent/unknown tier to **Pro** (back-compat with the cadence-only marketing deep-links that exist today). Add an optional `tier` field to `CheckoutRequest`. No new Stripe API surface, no DB changes — the checkout flow is otherwise unchanged.

**Tech Stack:** Rust, axum, serde. Tests are pure `#[test]` units (no DB, no Stripe), run via `cargo test --bin backend`.

**Reference spec:** `docs/superpowers/specs/2026-07-23-two-tier-billing-design.md` (Section 4). **Depends on:** Plan A (shipped). Independent of B1/B2/B3.

## Plan B decomposition (context; only B4a is in this document)

- B1 (merged): gate primitives.
- B2: provider `require_pro` → owner `require_tier(Pro)`.
- B3: `require_writable_budget` at write sites (enforcement on).
- **B4a (this doc):** 4-price mapping + checkout `tier`. Satisfies B2's "prices exist in code" precondition (the Fly secrets themselves are seeded operationally — see Deployment note).
- B4b: stop auto-create + trial-on-first-budget (server-side Stripe trial).

## Global Constraints

- Backend is a **bin-only crate**: run tests with `cargo test --bin backend`, never `--lib`. These tests are pure (no `#[ignore]`, no DB).
- No self-attribution in commits: no `Co-Authored-By: Claude`, no "Generated with" footer, no emoji trailers.
- The four price env var names (exact): `STRIPE_PRICE_BASIC_MONTHLY`, `STRIPE_PRICE_BASIC_ANNUAL`, `STRIPE_PRICE_PRO_MONTHLY`, `STRIPE_PRICE_PRO_ANNUAL`. These MATCH the names `entitlement::PriceCatalog::from_env()` already reads (Plan A) — do not rename.
- Tier strings are lowercase `"basic"` / `"pro"`; comparison is case-insensitive (normalize with `to_lowercase`). An absent or unrecognized tier resolves to **Pro** (never a 400 on tier alone — only an unrecognized *cadence* is a 400, preserving the current contract).
- `resolve_price_env_var` stays a **pure** function (no env reads, no DB) so its table is unit-testable; `env_opt(...)` on the returned name happens in the `checkout` handler exactly as today.
- Prices ($3/$30 Basic, $5/$50 Pro) live only in Stripe + Fly secrets, never in code — this plan maps to env var *names*, not amounts.

## Deployment note (not a code task)

Before this ships to prod, the four `STRIPE_PRICE_*` secrets must be set on Fly app `nels-api` (create the 4 Stripe prices first). Until then, `env_opt` returns `None` → checkout 503 "Billing is not configured" for the missing tier — the same failure mode as an unconfigured price today. The legacy `STRIPE_PRICE_MONTHLY/ANNUAL` secrets can be removed once no code references them (after this plan).

---

## File Structure

- **Modify:** `backend/src/billing.rs`:
  - `resolve_price_env_var` (currently ~line 361) — new `(tier, cadence, country)` signature + 4-price match.
  - `CheckoutRequest` (currently ~line 340) — add `tier: Option<String>`.
  - `checkout` handler (currently ~line 375) — pass `req.tier` into `resolve_price_env_var`.
  - `#[cfg(test)] mod tests` — replace the existing `resolve_price_env_var_*` tests with tier-aware ones.

---

### Task 1: Extend `resolve_price_env_var` to `(tier, cadence, country)`

**Files:**
- Modify: `backend/src/billing.rs` (`resolve_price_env_var` + its unit tests)

**Interfaces:**
- Produces: `fn resolve_price_env_var(tier: Option<&str>, cadence: &str, country: Option<&str>) -> Option<&'static str>` — returns one of the four `STRIPE_PRICE_*` names, or `None` only for an unrecognized `cadence`.

- [ ] **Step 1: Replace the existing `resolve_price_env_var` tests with tier-aware ones**

In `backend/src/billing.rs`, find the existing `resolve_price_env_var_*` tests inside `#[cfg(test)] mod tests` (there are several: `_monthly_us`, `_annual_us`, `_defaults_to_us_when_country_absent`, `_falls_back_to_us_for_unknown_country`, `_annual_falls_back_to_us_for_unknown_country`, `_country_is_case_insensitive`, `_annual_country_is_case_insensitive`, `_unknown_cadence_is_none`, `_unknown_cadence_is_none_regardless_of_country`). **Delete all of them** and replace with the following block (same location):

```rust
    #[test]
    fn resolve_price_basic_monthly() {
        assert_eq!(resolve_price_env_var(Some("basic"), "monthly", None), Some("STRIPE_PRICE_BASIC_MONTHLY"));
    }

    #[test]
    fn resolve_price_basic_annual() {
        assert_eq!(resolve_price_env_var(Some("basic"), "annual", None), Some("STRIPE_PRICE_BASIC_ANNUAL"));
    }

    #[test]
    fn resolve_price_pro_monthly() {
        assert_eq!(resolve_price_env_var(Some("pro"), "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
    }

    #[test]
    fn resolve_price_pro_annual() {
        assert_eq!(resolve_price_env_var(Some("pro"), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_absent_tier_defaults_to_pro() {
        // Back-compat: cadence-only marketing deep-links (no tier) get Pro.
        assert_eq!(resolve_price_env_var(None, "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(None, "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_unknown_tier_defaults_to_pro() {
        assert_eq!(resolve_price_env_var(Some("gold"), "monthly", None), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some(""), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_tier_is_case_insensitive() {
        assert_eq!(resolve_price_env_var(Some("BASIC"), "monthly", None), Some("STRIPE_PRICE_BASIC_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some("Pro"), "annual", None), Some("STRIPE_PRICE_PRO_ANNUAL"));
    }

    #[test]
    fn resolve_price_unknown_cadence_is_none() {
        // An unrecognized cadence is the ONLY 400 — independent of tier.
        assert_eq!(resolve_price_env_var(Some("basic"), "weekly", None), None);
        assert_eq!(resolve_price_env_var(Some("pro"), "bogus", None), None);
        assert_eq!(resolve_price_env_var(None, "", None), None);
    }

    #[test]
    fn resolve_price_country_does_not_change_tiered_mapping() {
        // Only US prices are configured; country is accepted but does not
        // branch the mapping today (kept in the signature for future markets).
        assert_eq!(resolve_price_env_var(Some("pro"), "monthly", Some("US")), Some("STRIPE_PRICE_PRO_MONTHLY"));
        assert_eq!(resolve_price_env_var(Some("basic"), "annual", Some("gb")), Some("STRIPE_PRICE_BASIC_ANNUAL"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin backend billing::tests::resolve_price -- --nocapture`
Expected: FAIL to compile — the calls pass 3 args but `resolve_price_env_var` still takes 2.

- [ ] **Step 3: Rewrite `resolve_price_env_var`**

Replace the existing function (currently ~lines 361-373) with:

```rust
/// (tier, cadence) -> the Stripe price env var name. `tier` is `"basic"`/`"pro"`
/// (case-insensitive); an absent or unrecognized tier defaults to **Pro** so
/// the existing cadence-only marketing deep-links keep working and err toward
/// the fuller plan. `country` is accepted for future multi-market pricing but
/// does not branch the mapping today (only US prices are configured). `None`
/// is returned ONLY for an unrecognized `cadence` — the same 400 the caller
/// produced before tiers existed.
fn resolve_price_env_var(tier: Option<&str>, cadence: &str, country: Option<&str>) -> Option<&'static str> {
    let tier = tier.map(str::to_lowercase).unwrap_or_else(|| "pro".to_string());
    let _country = country.map(str::to_uppercase).unwrap_or_else(|| "US".to_string());
    match (tier.as_str(), cadence) {
        ("basic", "monthly") => Some("STRIPE_PRICE_BASIC_MONTHLY"),
        ("basic", "annual") => Some("STRIPE_PRICE_BASIC_ANNUAL"),
        ("pro", "monthly") => Some("STRIPE_PRICE_PRO_MONTHLY"),
        ("pro", "annual") => Some("STRIPE_PRICE_PRO_ANNUAL"),
        // Unrecognized tier defaults to Pro (back-compat with cadence-only callers).
        (_, "monthly") => Some("STRIPE_PRICE_PRO_MONTHLY"),
        (_, "annual") => Some("STRIPE_PRICE_PRO_ANNUAL"),
        _ => None, // unrecognized cadence → 400
    }
}
```

- [ ] **Step 4: Update the `checkout` handler call site to compile**

The `checkout` handler (currently ~line 380) calls `resolve_price_env_var(&req.cadence, req.country.as_deref())`. It must become a 3-arg call. For THIS task (before `CheckoutRequest` has a `tier` field), pass `None` for tier to keep it compiling:

Change:
```rust
    let price_env_var = resolve_price_env_var(&req.cadence, req.country.as_deref())
```
to:
```rust
    let price_env_var = resolve_price_env_var(None, &req.cadence, req.country.as_deref())
```
(Task 2 replaces `None` with `req.tier.as_deref()`.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --bin backend billing::tests -- --nocapture`
Expected: the 9 new `resolve_price_*` tests pass; all other pre-existing `billing::tests` (signature, `user_is_pro_*`, `subscription_response_shapes_*`) still pass.

- [ ] **Step 6: Confirm the crate builds**

Run: `cargo build --bin backend`
Expected: builds. (An unused-variable warning for `_country` is avoided by the leading underscore.)

- [ ] **Step 7: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(billing): map (tier, cadence) to four Stripe price env vars"
```

---

### Task 2: Accept `tier` on `CheckoutRequest` and wire it through

**Files:**
- Modify: `backend/src/billing.rs` (`CheckoutRequest` struct + `checkout` handler + a deserialization test)

**Interfaces:**
- Consumes: `resolve_price_env_var` (Task 1).
- Produces: `POST /billing/checkout` accepts an optional `"tier": "basic"|"pro"` in its JSON body; absent → Pro. The checkout session is created on the resolved tier's price.

- [ ] **Step 1: Write the failing deserialization test**

The full `checkout` handler needs Stripe and isn't unit-tested here, but `CheckoutRequest`'s parsing (that `tier` is optional and defaults cleanly) is pure and worth pinning. Add to `#[cfg(test)] mod tests` in `backend/src/billing.rs`:

```rust
    #[test]
    fn checkout_request_tier_is_optional_and_parses() {
        // Absent tier deserializes to None (handler then defaults it to Pro).
        let a: CheckoutRequest = serde_json::from_str(r#"{"cadence":"monthly"}"#).unwrap();
        assert_eq!(a.cadence, "monthly");
        assert_eq!(a.tier, None);
        // Present tier is carried through.
        let b: CheckoutRequest = serde_json::from_str(r#"{"cadence":"annual","tier":"basic"}"#).unwrap();
        assert_eq!(b.tier.as_deref(), Some("basic"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --bin backend billing::tests::checkout_request_tier -- --nocapture`
Expected: FAIL to compile — `no field \`tier\` on type \`CheckoutRequest\``.

- [ ] **Step 3: Add the `tier` field to `CheckoutRequest`**

Change `CheckoutRequest` (currently ~lines 340-348) to:

```rust
#[derive(Deserialize)]
pub struct CheckoutRequest {
    pub cadence: String, // "monthly" | "annual"
    /// "basic" | "pro" (case-insensitive), optional. Absent → Pro (back-compat
    /// with cadence-only marketing deep-links). See `resolve_price_env_var`.
    #[serde(default)]
    pub tier: Option<String>,
    /// ISO 3166-1 alpha-2 country code, optional (#338). Only "US" is
    /// configured today; accepted but does not branch pricing yet.
    #[serde(default)]
    pub country: Option<String>,
}
```

- [ ] **Step 4: Wire `req.tier` into the handler**

In `checkout`, replace the Task-1 placeholder:
```rust
    let price_env_var = resolve_price_env_var(None, &req.cadence, req.country.as_deref())
```
with:
```rust
    let price_env_var = resolve_price_env_var(req.tier.as_deref(), &req.cadence, req.country.as_deref())
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --bin backend billing::tests -- --nocapture`
Expected: `checkout_request_tier_is_optional_and_parses` passes; all other `billing::tests` still pass.

- [ ] **Step 6: Confirm the crate builds**

Run: `cargo build --bin backend`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add backend/src/billing.rs
git commit -m "feat(billing): accept tier on checkout request, default to Pro"
```

---

## Self-Review

**Spec coverage (Section 4 — price config):**
- Four `STRIPE_PRICE_{BASIC,PRO}_{MONTHLY,ANNUAL}` mapped → Task 1 ✅
- Names match `PriceCatalog::from_env()` (Plan A) → Global Constraints ✅ (verbatim)
- Checkout accepts a tier → Task 2 ✅
- Fly-secret seeding → Deployment note (operational, not a code task) ✅
- Portal-based upgrades/downgrades → Plan C (frontend routing); not this plan.

**Placeholder scan:** none — every step has concrete code and exact commands. The Task-1 `None` placeholder in the checkout call is explicitly replaced in Task 2 Step 4 (called out in both tasks).

**Type consistency:** `resolve_price_env_var(Option<&str>, &str, Option<&str>)` is used identically in Task 1's tests, Task 1 Step 4, and Task 2 Step 4. `CheckoutRequest.tier: Option<String>` matches `req.tier.as_deref()`. Env var names are byte-identical to Plan A's `PriceCatalog`.

**Known scope boundary (not a gap):** the `checkout` handler's existing "already subscribed" guard (`billing.rs` ~line 390, `if user_is_pro(status) → 409`) is left unchanged. It still lets a `past_due` user reach checkout (creating a second subscription) — that hole is addressed by Plan C's `route_billing_action` (frontend routes live-sub users, incl. past_due, to the Portal) and can be hardened server-side in a later task. Out of scope for B4a to keep it a pure price-plumbing change.

**Out of scope for B4a (by design):** trial-on-first-budget, `auth.rs` auto-create removal, server-side trial subscription creation (all B4b); any gating (B1/B2/B3); frontend/marketing (Plan C).
