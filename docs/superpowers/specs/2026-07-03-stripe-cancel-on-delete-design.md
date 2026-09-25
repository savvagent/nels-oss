# Account deletion: cancel the Stripe subscription + no-refund copy (#262) — Design

## 1. Brief (verbatim AC, from #262)

As a user deleting my account, I want to be told clearly that deleting my account cancels my
subscription and that any unused/prepaid amount will not be refunded, and I want the subscription
to actually be cancelled at the billing provider — so I'm not surprised by continued charges or a
lost balance.

Today: the delete-account confirmation UI warns about permanence and shared-budget deletion but
says nothing about subscriptions/billing/refunds. The backend (`account::delete_user_data`)
deletes only the local `subscriptions` DB row — it makes no Stripe API call, so an active Stripe
subscription for that customer keeps billing after the local account is gone.

Acceptance criteria:
- The delete-account confirmation UI clearly states that deletion cancels the subscription and
  unused/prepaid amount is not refunded.
- Copy added to all six locales (en, de, es, fr, it, pt) under `deleteAccount`.
- On deletion the backend cancels the user's Stripe subscription (`DELETE v1/subscriptions/{id}`)
  so billing stops — no orphaned active Stripe subscription remains.
- Cancellation failure is handled sensibly and tested.
- Tests cover: Stripe cancellation on delete, and the local cascade still occurs.

## 2. Source-of-truth correction (deviation from the ticket's file references)

The ticket (filed before nels#261/PR #274 merged) points at `App.svelte:1612-1678` — an inline
delete-confirmation **modal**. That modal no longer exists: #261 moved it to a full-page route,
**`frontend/src/lib/DeleteAccount.svelte`**, rendered by `App.svelte` (which now only retains
`canDeleteAccount`/`confirmDeleteAccount` logic and passes props into the page). Verified fresh
from source. This design targets `DeleteAccount.svelte`, not the removed modal. The backend paths
(`backend/src/account.rs:363-415` `delete_user_data`, `:421-489` `delete_account`) and
`backend/src/billing.rs` are unchanged from the ticket's references (line numbers shifted by a few
lines only).

## 3. Assumptions

- Billing provider is Stripe; cancellation = `DELETE v1/subscriptions/{id}` (immediate cancel), not
  `cancel_at_period_end` — the account and its data are being destroyed now, so there is no future
  period to serve, and this matches the "no refund for unused time" copy (immediate cancel forfeits
  remaining prepaid time).
- Only users with a `subscriptions` row holding a non-null/non-empty `stripe_subscription_id` need a
  Stripe call; free/never-subscribed users skip it. Confirmed 1:1: `subscriptions.user_id` is the
  table's PRIMARY KEY and `stripe_subscription_id` is UNIQUE (`backend/migrations/20260630120000_subscriptions.sql`),
  so a subscription can never be shared across users — cancelling it for the deleting user cannot
  affect anyone else's billing.
- Stripe HTTP calls stay in `backend/src/billing.rs` (all `stripe_post`/Stripe HTTP logic already
  lives there); `account.rs` calls a new `billing::cancel_subscription`, preserving the existing
  module boundary.
- No refund is issued under any circumstance (explicit product decision in the ticket).
- Locale copy for de/es/fr/it/pt is added as reasonable translations mirroring the English string;
  native review is out of scope, matching how the existing `deleteAccount` keys were added.
- On a Stripe cancel failure, **abort the deletion** (return an error, account and all data
  untouched) rather than delete locally anyway — this never orphans a paid Stripe subscription with
  no local owner who could reach the Customer Portal to cancel it themselves.

## 4. Goal & Success Criteria

Make account deletion truthfully cancel billing and disclose the no-refund consequence up front.

- Deleting an account with an active Stripe subscription results in that subscription being
  `canceled` at Stripe, not merely removed locally.
- `DeleteAccount.svelte` shows subscription-cancellation + no-refund language before the user can
  confirm.
- All six locales carry the new copy under `deleteAccount`.
- A Stripe cancel failure does not silently leave an active paid subscription — the account is not
  deleted, and this is covered by a test.
- Existing local cascade behavior (sessions, shares, subscriptions row, user) is unchanged.

## 5. Scope

**In:**
- New `deleteAccount.billingWarning` copy, all 6 locales.
- Backend: cancel the Stripe subscription during account deletion, before the local delete.
- Error handling for the Stripe cancel call (abort deletion on failure).
- Tests: Stripe cancel invoked on delete; cascade preserved; failure path aborts deletion;
  no-subscription path is a no-op.

**Out:**
- Refunds / proration of any kind.
- Changing the Customer-Portal cancellation path or the `customer.subscription.deleted` webhook.
- Re-designing the `DeleteAccount.svelte` page layout/gating (email + TOTP gating stays as-is).
- Professional localization review.

## 6. Architecture / Approach

**Backend.** Add a DELETE-capable Stripe helper (`stripe_delete`) in `billing.rs` alongside the
existing `stripe_post` (which is hardcoded to `.post()` — a bare POST to
`v1/subscriptions/{id}` is an *update*, not a cancel, so it cannot be reused as-is). Add
`pub(crate) async fn cancel_subscription(pool: &PgPool, user_id: Uuid) -> Result<(), (StatusCode,
String)>` that looks up the user's `subscriptions.stripe_subscription_id` and, if present and
non-empty, issues `DELETE v1/subscriptions/{id}`; otherwise no-ops. In `account::delete_account`,
call `billing::cancel_subscription` after TOTP verification but **before** `delete_user_data` — a
Stripe failure returns an error and the account/local data are left intact.

**Frontend.** Add a `deleteAccount.billingWarning` key to `en.json` and the five other locale
files. Render it in `DeleteAccount.svelte`, directly below the existing `deleteAccount.warning`
line, matching its markup/emphasis conventions (the existing warning uses an `AlertTriangle` icon +
`text-error`; the new line should read clearly as a continuation of the same warning, not a
separate lower-priority notice — see plan for exact placement).

## 7. Error Handling & Edge Cases

- **No subscription / no `stripe_subscription_id`:** skip the Stripe call; proceed with local
  deletion.
- **Already-cancelled subscription at Stripe:** treat Stripe "already canceled" (404, or 400 with
  `resource_missing`/a "canceled" message) as success (idempotent) — still delete locally.
- **Stripe cancel call fails (network/5xx/other 4xx):** abort the deletion, return an error. The
  frontend already surfaces a generic `deleteAccount.failed` message on a failed `DELETE /account`.
- **Concurrent portal cancellation / webhook race:** cancelling an already-cancelled sub is
  idempotent, so no special handling beyond the "already canceled" case above.
- **The Stripe DELETE call itself fires a `customer.subscription.deleted` webhook** back at the app,
  asynchronously, after the local user/subscription rows may already be gone. Confirmed this is
  already handled gracefully: `billing.rs`'s webhook handler resolves the event by
  `stripe_customer_id` against the `subscriptions` table and, finding no row, logs
  `tracing::error!("stripe webhook for UNKNOWN customer — entitlement update dropped …")` and
  returns `Ok(())` — it does not panic, 500, or retry-loop. No handler change needed; this
  self-cancellation-triggered webhook is just a no-op arrival, same as any webhook for an already
  deleted account.

## 8. Testing Approach

- Backend DB-backed (`#[ignore]`) tests, following the existing `billing.rs`/`account.rs` pattern
  (`test_pool`/raw `AppState` construction, `cascade_deletes_subscription_with_user` as the nearest
  precedent): Stripe DELETE is invoked (mocked via `wiremock`) when a `stripe_subscription_id` is
  present; a no-subscription user deletes cleanly with no Stripe call; a Stripe 5xx aborts the
  deletion (account still exists); the existing cascade test is unaffected.
- This is net-new test infrastructure: `backend/Cargo.toml` gets `wiremock` + `serial_test` dev
  dependencies (no existing HTTP-mock dependency). Every test mutating `STRIPE_API_BASE`/
  `STRIPE_SECRET_KEY` env vars is `#[serial_test::serial]` so Cargo's parallel runner can't clobber
  them across tests.
- Frontend: confirm the new copy key exists and resolves in all 6 locale files (a static/renders
  check is enough; no dedicated frontend test framework coverage of `DeleteAccount.svelte` exists
  today and none is being added as new infra for this change).

## 9. Risks & Open Questions

- **Resolved for this build:** immediate-cancel (forfeit remaining time) is the assumed intended
  behavior, matching the no-refund copy. Flagged as a product assumption in the PR description.
- **Resolved for this build:** on Stripe cancel failure, abort deletion (vs. "delete anyway +
  alert") — the safer default; flagged as a product assumption in the PR description.
- **Risk:** locale translations added without native review may be imperfect — acceptable per
  scope (matches how the existing `deleteAccount` strings were added), flagged.
- **Risk:** ordering the Stripe call before the local transaction means a rare window where Stripe
  is cancelled but the local delete then fails for an unrelated reason (e.g. a DB error) — the user
  keeps their account but has lost their subscription. Mitigated by doing the cancel immediately
  before the transaction and surfacing the error so the user can retry (re-cancelling an
  already-canceled sub is idempotent); accepted given the alternative (orphaned paid subscription)
  is worse.
