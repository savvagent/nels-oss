# Account deletion: cancel Stripe subscription + no-refund copy (#262) — Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement task-by-task. Steps use `- [ ]` checkboxes.

**Goal:** On account deletion, cancel the user's Stripe subscription (immediate) before destroying
local data — aborting the deletion if the Stripe call fails — and tell the user in
`DeleteAccount.svelte` that deletion cancels their subscription with no refund of unused/prepaid
time.

**Architecture:** Add a DELETE-capable Stripe helper and a `cancel_subscription` function in
`backend/src/billing.rs`; call it from `account::delete_account` before `delete_user_data`, so a
Stripe failure aborts deletion (never orphans a paid subscription). Add no-refund/cancellation copy
to all six locale files and render it in `frontend/src/lib/DeleteAccount.svelte` (the full-page
route that replaced the old inline modal per nels#261/PR #274 — verified fresh from source).
See design doc: `docs/superpowers/specs/2026-07-03-stripe-cancel-on-delete-design.md`.

**Tech Stack:** Rust (axum, sqlx, reqwest), Svelte 5 + svelte-i18n. Backend tests use `wiremock` +
`serial_test` (new dev-dependencies) with `STRIPE_API_BASE` pointed at the mock. Tests that mutate
process-global env vars are `#[serial]` to avoid clobbering under Cargo's parallel test runner.
Backend DB-backed tests run against the local pgvector container (`podman-compose up -d`, port
6153) and are `#[ignore]`d by convention — run explicitly with `cargo test -- --ignored`. There is
no CI test workflow in this repo (`.github/workflows/` only has release-please + release-gated
deploy jobs) — `cargo build`/`cargo test`/`cargo clippy` and `pnpm test` must be run locally as part
of each task, not assumed to run in CI.

---

### Task 1: DELETE-capable Stripe helper + `cancel_subscription` in billing.rs

**Files:**
- Modify: `backend/src/billing.rs` (add `stripe_delete` helper near `stripe_post`, ~line 225; add
  `cancel_subscription` as a `pub(crate)` fn)
- Modify: `backend/Cargo.toml` (add `wiremock = "0.6"` and `serial_test = "3"` under
  `[dev-dependencies]`)
- Test: `backend/src/billing.rs` (test module, alongside `cascade_deletes_subscription_with_user`
  ~line 649)

Context: `stripe_post` (`billing.rs:225`) is hardcoded to `.post()`. Immediate cancel is HTTP
`DELETE v1/subscriptions/{id}`; a POST to that path is an *update*, so `stripe_post` cannot be
reused. The `subscriptions` row may have a NULL `stripe_subscription_id` (customer created but
never checked out) — those users have no Stripe subscription to cancel. `db::Subscription.stripe_subscription_id`
is `Option<String>` (nullable column, confirmed in `backend/src/db.rs`).

- [ ] **Step 1: Add wiremock + serial_test dev-dependencies**

  In `backend/Cargo.toml` under `[dev-dependencies]`:
  ```toml
  wiremock = "0.6"
  serial_test = "3"
  ```
  Run `cd backend && cargo build` (or `cargo check`) once to confirm the crate
  resolves and `Cargo.lock` updates cleanly.

  Every test below that calls `std::env::set_var` MUST be annotated `#[serial_test::serial]`
  (`use serial_test::serial;` + `#[serial]`) so Cargo's parallel runner doesn't let two tests
  clobber each other's `STRIPE_API_BASE`/`STRIPE_SECRET_KEY`.

- [ ] **Step 2: Write the failing test for `cancel_subscription`**

  Add to the `billing.rs` test module (reuse `test_pool()` and the `mk_user()` helper already
  defined there — do not invent parallel scaffolding). Add a small local helper to seed a
  `subscriptions` row with a given `stripe_subscription_id` (or `NULL`), following the existing
  `INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_X')` pattern seen in
  `upsert_inserts_then_updates_same_row`/`cascade_deletes_subscription_with_user`.

  ```rust
  #[tokio::test]
  #[ignore]
  #[serial_test::serial]
  async fn cancel_subscription_calls_stripe_delete() {
      use wiremock::{MockServer, Mock, ResponseTemplate};
      use wiremock::matchers::{method, path};

      let server = MockServer::start().await;
      std::env::set_var("STRIPE_API_BASE", server.uri());
      std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");

      Mock::given(method("DELETE"))
          .and(path("/v1/subscriptions/sub_123"))
          .respond_with(ResponseTemplate::new(200)
              .set_body_json(serde_json::json!({"id":"sub_123","status":"canceled"})))
          .expect(1)
          .mount(&server)
          .await;

      let db = test_pool().await;
      let user_id = mk_user(&db).await;
      sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_123', 'sub_123')")
          .bind(user_id).execute(&db).await.unwrap();

      cancel_subscription(&db, user_id).await.expect("cancel ok");
      // wiremock verifies .expect(1) on drop

      sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
      // Cleanup so this doesn't leak into other tests in the same test binary
      // (matches the existing convention in github.rs/rag.rs).
      std::env::remove_var("STRIPE_API_BASE");
      std::env::remove_var("STRIPE_SECRET_KEY");
  }
  ```

- [ ] **Step 3: Run it to verify it fails**

  Run: `cd backend && cargo test cancel_subscription_calls_stripe_delete -- --ignored --nocapture`
  Expected: FAIL — `cancel_subscription` not found (won't compile).

- [ ] **Step 4: Implement the DELETE helper + `cancel_subscription`**

  Add near `stripe_post` in `billing.rs`:

  ```rust
  /// DELETE against the Stripe API with the secret key as bearer. Used for
  /// immediate subscription cancellation. Treats "already canceled" as success.
  async fn stripe_delete(path: &str) -> Result<(), (StatusCode, String)> {
      let secret = env_opt("STRIPE_SECRET_KEY")
          .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured".to_string()))?;
      let url = format!("{}/{}", stripe_api_base().trim_end_matches('/'), path);
      let resp = http_client()
          .delete(&url)
          .bearer_auth(secret)
          .send().await
          .map_err(|e| internal_error(format!("stripe DELETE {path}: {e}")))?;
      let status = resp.status();
      if status.is_success() {
          return Ok(());
      }
      // Idempotent success: an already-canceled subscription. Stripe returns 404
      // (no such active sub) or 400 (`resource_missing` / "subscription is
      // canceled") depending on state — treat both as already-done, but inspect
      // the body so we don't swallow an unrelated 400.
      let body: serde_json::Value = resp.json().await.unwrap_or_default();
      let already_canceled = status == StatusCode::NOT_FOUND
          || (status == StatusCode::BAD_REQUEST && {
              let code = body.pointer("/error/code").and_then(|c| c.as_str()).unwrap_or("");
              let msg = body.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("");
              code == "resource_missing" || msg.contains("canceled") || msg.contains("cancelled")
          });
      if already_canceled {
          tracing::warn!(?status, ?body, "stripe DELETE {path}: already canceled, treating as success");
          return Ok(());
      }
      tracing::error!(?status, ?body, "stripe API error on DELETE {path}");
      Err((StatusCode::BAD_GATEWAY, "Billing provider error".to_string()))
  }

  /// Cancel the user's Stripe subscription immediately (no proration/refund).
  /// No-op when the user has no `stripe_subscription_id`.
  pub(crate) async fn cancel_subscription(
      pool: &PgPool,
      user_id: Uuid,
  ) -> Result<(), (StatusCode, String)> {
      let sub_id: Option<String> = sqlx::query_scalar(
          "SELECT stripe_subscription_id FROM subscriptions WHERE user_id = $1")
          .bind(user_id)
          .fetch_optional(pool).await.map_err(internal_error)?
          .flatten();
      match sub_id {
          Some(id) if !id.is_empty() => stripe_delete(&format!("v1/subscriptions/{id}")).await,
          _ => Ok(()),
      }
  }
  ```

  `query_scalar` typed as `Option<String>` over a nullable column, `fetch_optional` gives
  `Option<Option<String>>` (no row vs. row-with-NULL) — `.flatten()` collapses both "no row" and
  "NULL column" to `None`, which is exactly the no-op case.

- [ ] **Step 5: Run tests to verify they pass**

  Run: `cd backend && cargo test cancel_subscription_calls_stripe_delete -- --ignored --nocapture`
  Expected: PASS. Also `cd backend && cargo build` clean.

- [ ] **Step 6: Add no-op + failure tests**

  ```rust
  #[tokio::test]
  #[ignore]
  async fn cancel_subscription_noop_without_stripe_id() {
      let db = test_pool().await;
      let user_id = mk_user(&db).await;
      sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id) VALUES ($1, 'cus_noop')")
          .bind(user_id).execute(&db).await.unwrap(); // stripe_subscription_id stays NULL
      // No STRIPE_SECRET_KEY / server needed: must not make any call.
      cancel_subscription(&db, user_id).await.expect("noop ok");
      sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
  }

  #[tokio::test]
  #[ignore]
  #[serial_test::serial]
  async fn cancel_subscription_errors_on_stripe_5xx() {
      use wiremock::{MockServer, Mock, ResponseTemplate};
      use wiremock::matchers::method;
      let server = MockServer::start().await;
      std::env::set_var("STRIPE_API_BASE", server.uri());
      std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
      Mock::given(method("DELETE"))
          .respond_with(ResponseTemplate::new(500))
          .mount(&server).await;
      let db = test_pool().await;
      let user_id = mk_user(&db).await;
      sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_err', 'sub_err')")
          .bind(user_id).execute(&db).await.unwrap();
      let res = cancel_subscription(&db, user_id).await;
      assert!(res.is_err(), "5xx must propagate as error");
      sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&db).await.unwrap();
      std::env::remove_var("STRIPE_API_BASE");
      std::env::remove_var("STRIPE_SECRET_KEY");
  }
  ```

  Run: `cd backend && cargo test cancel_subscription -- --ignored`
  Expected: PASS (all three).

- [ ] **Step 7: Commit**

  ```bash
  git add backend/src/billing.rs backend/Cargo.toml backend/Cargo.lock
  git commit -m "feat(billing): add cancel_subscription that cancels the Stripe subscription"
  ```

---

### Task 2: Cancel the subscription in the account-delete path

**Files:**
- Modify: `backend/src/account.rs` (inside `delete_account`, before the `delete_user_data` call at
  the current `:481`)
- Test: `backend/src/account.rs`

Design: call `billing::cancel_subscription` after TOTP verification but **before**
`delete_user_data`, so a Stripe failure returns an error and leaves the account intact (per the
design doc §7: never destroy the account while an active paid Stripe subscription remains).

- [ ] **Step 1: Write the failing test**

  Drive `delete_account` end-to-end, mirroring the existing
  `delete_account_accepts_legacy_plaintext_secret` test's harness in `account.rs` (raw
  `AppState { db: pool.clone(), cipher }`, `crate::auth::build_totp_strict(&secret,
  &email).unwrap().generate_current().unwrap()` for a valid current code). Seed a
  `subscriptions` row with a `stripe_subscription_id` for the test user.

  ```rust
  #[tokio::test]
  #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
  #[serial_test::serial]
  async fn delete_account_cancels_stripe_then_deletes() {
      use wiremock::{MockServer, Mock, ResponseTemplate};
      use wiremock::matchers::{method, path};
      let server = MockServer::start().await;
      std::env::set_var("STRIPE_API_BASE", server.uri());
      std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
      Mock::given(method("DELETE")).and(path("/v1/subscriptions/sub_del"))
          .respond_with(ResponseTemplate::new(200)
              .set_body_json(serde_json::json!({"id":"sub_del","status":"canceled"})))
          .expect(1).mount(&server).await;

      let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
          "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
      });
      let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
      let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
      let state = AppState { db: pool.clone(), cipher };

      let user_id = Uuid::new_v4();
      let email = format!("del-stripe-{user_id}@example.test");
      let secret = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &[0u8; 16]);
      sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, $3)")
          .bind(user_id).bind(&email).bind(&secret).execute(&pool).await.unwrap();
      sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_del', 'sub_del')")
          .bind(user_id).execute(&pool).await.unwrap();
      let code = crate::auth::build_totp_strict(&secret, &email).unwrap().generate_current().unwrap();

      let resp = delete_account(
          State(state),
          Extension(user_id),
          Json(DeleteAccountRequest { confirm_email: email, totp_code: code }),
      ).await;
      assert!(matches!(resp, Ok(StatusCode::NO_CONTENT)), "got {resp:?}");

      let cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
          .bind(user_id).fetch_one(&pool).await.unwrap();
      assert_eq!(cnt, 0);
      std::env::remove_var("STRIPE_API_BASE");
      std::env::remove_var("STRIPE_SECRET_KEY");
  }
  ```

- [ ] **Step 2: Run it to verify it fails**

  Run: `cd backend && cargo test delete_account_cancels_stripe_then_deletes -- --ignored`
  Expected: FAIL — no DELETE received (`.expect(1)` unmet), because deletion doesn't call Stripe
  yet.

- [ ] **Step 3: Wire in the cancel call**

  In `account.rs`, immediately before the `delete_user_data` call:

  ```rust
  // Cancel the Stripe subscription BEFORE destroying local data. If this fails
  // we abort so we never delete the account while a paid subscription lives on
  // (the user could no longer reach the billing portal to cancel it).
  crate::billing::cancel_subscription(&state.db, user_id).await?;

  let deleted = delete_user_data(&state.db, user_id, &user_email).await?;
  ```

  Ensure `billing` is reachable (`crate::billing::cancel_subscription` — no extra `use` needed if
  called with the full path; add `use crate::billing;` if preferred and not already present).

- [ ] **Step 4: Run tests to verify they pass**

  Run: `cd backend && cargo test delete_account_cancels_stripe_then_deletes -- --ignored`
  Expected: PASS.

- [ ] **Step 5: Add the abort-on-failure test**

  ```rust
  #[tokio::test]
  #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
  #[serial_test::serial]
  async fn delete_account_aborts_when_stripe_cancel_fails() {
      use wiremock::{MockServer, Mock, ResponseTemplate};
      use wiremock::matchers::method;
      let server = MockServer::start().await;
      std::env::set_var("STRIPE_API_BASE", server.uri());
      std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
      Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(500))
          .mount(&server).await;

      let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
          "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
      });
      let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
      let cipher = std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap());
      let state = AppState { db: pool.clone(), cipher };

      let user_id = Uuid::new_v4();
      let email = format!("del-stripe-fail-{user_id}@example.test");
      let secret = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &[0u8; 16]);
      sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, $3)")
          .bind(user_id).bind(&email).bind(&secret).execute(&pool).await.unwrap();
      sqlx::query("INSERT INTO subscriptions (user_id, stripe_customer_id, stripe_subscription_id) VALUES ($1, 'cus_x', 'sub_x')")
          .bind(user_id).execute(&pool).await.unwrap();
      let code = crate::auth::build_totp_strict(&secret, &email).unwrap().generate_current().unwrap();

      let resp = delete_account(
          State(state), Extension(user_id),
          Json(DeleteAccountRequest { confirm_email: email, totp_code: code }),
      ).await;
      assert!(resp.is_err(), "Stripe failure must abort deletion");

      let cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id=$1")
          .bind(user_id).fetch_one(&pool).await.unwrap();
      assert_eq!(cnt, 1, "account must still exist after aborted delete");

      let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
      std::env::remove_var("STRIPE_API_BASE");
      std::env::remove_var("STRIPE_SECRET_KEY");
  }
  ```

  Run: `cd backend && cargo test delete_account_aborts_when_stripe_cancel_fails -- --ignored`
  Expected: PASS.

- [ ] **Step 6: Confirm the existing cascade/delete tests still pass**

  Run: `cd backend && cargo test account -- --ignored` and `cd backend && cargo test billing -- --ignored`
  Expected: PASS — `cascade_deletes_subscription_with_user` seeds a `subscriptions` row with NO
  `stripe_subscription_id` (NULL), so `cancel_subscription` is a no-op there; unaffected.
  `delete_account_accepts_legacy_plaintext_secret`/`account_deletion_removes_all_user_data` seed no
  `subscriptions` row at all for their user, so `cancel_subscription`'s lookup finds no row
  (`fetch_optional` → `None`) and no-ops — unaffected.

- [ ] **Step 7: Commit**

  ```bash
  git add backend/src/account.rs
  git commit -m "feat(#262): cancel Stripe subscription on account deletion"
  ```

---

### Task 3: Delete-confirmation copy — cancellation + no refund (all locales)

**Files:**
- Modify: `frontend/src/lib/i18n/locales/en.json` (`deleteAccount` block, currently ~line 177-184)
- Modify: `frontend/src/lib/i18n/locales/de.json`, `es.json`, `fr.json`, `it.json`, `pt.json`
  (same `deleteAccount` block, currently ~line 163-170 in each)
- Modify: `frontend/src/lib/DeleteAccount.svelte` (the delete-confirmation page — **not**
  `App.svelte`; the old inline modal at `App.svelte:1612-1678` was removed by nels#261/PR #274)

- [ ] **Step 1: Add a `billingWarning` key to en.json**

  In the `deleteAccount` object in `frontend/src/lib/i18n/locales/en.json`, add a new key after
  `warning`:
  ```json
  "billingWarning": "Deleting your account cancels your subscription immediately. Any unused or prepaid time will not be refunded.",
  ```

- [ ] **Step 2: Add the same key to the five other locales**

  Add `deleteAccount.billingWarning` to `de.json`, `es.json`, `fr.json`, `it.json`, `pt.json`,
  each in the same position (after `warning`) with translated copy mirroring the English meaning:

  - de: `"Beim Löschen deines Kontos wird dein Abonnement sofort gekündigt. Bereits bezahlte oder nicht genutzte Zeit wird nicht erstattet."`
  - es: `"Al eliminar tu cuenta se cancela tu suscripción de inmediato. El tiempo no utilizado o pagado por adelantado no se reembolsará."`
  - fr: `"La suppression de votre compte annule immédiatement votre abonnement. Le temps inutilisé ou prépayé ne sera pas remboursé."`
  - it: `"L'eliminazione dell'account annulla immediatamente l'abbonamento. Il tempo non utilizzato o prepagato non verrà rimborsato."`
  - pt: `"Excluir sua conta cancela sua assinatura imediatamente. O tempo não utilizado ou pré-pago não será reembolsado."`

  Validate every locale file is still valid JSON after editing (e.g. `node -e
  "JSON.parse(require('fs').readFileSync('frontend/src/lib/i18n/locales/de.json'))"` per file, or
  equivalent).

- [ ] **Step 3: Render the billing warning in DeleteAccount.svelte**

  In `frontend/src/lib/DeleteAccount.svelte`, the existing warning renders (current source,
  ~line 37-40) as:
  ```svelte
  <div class="flex items-center gap-2 text-error">
    <AlertTriangle class="w-6 h-6 shrink-0" />
    <span class="font-semibold">{$_("deleteAccount.warning")}</span>
  </div>
  ```
  Add the billing warning as a second line inside the same warning block (so it reads as a
  continuation of the one warning, not a separate lower-priority notice), e.g.:
  ```svelte
  <div class="flex items-start gap-2 text-error">
    <AlertTriangle class="w-6 h-6 shrink-0 mt-0.5" />
    <div class="space-y-1">
      <p class="font-semibold">{$_("deleteAccount.warning")}</p>
      <p class="font-medium">{$_("deleteAccount.billingWarning")}</p>
    </div>
  </div>
  ```
  Verify the exact surrounding classes/structure at the real current source before pasting — match
  whatever `DeleteAccount.svelte` actually has, adjusting only as needed to fit two lines instead of
  one `<span>`.

- [ ] **Step 4: Verify in the running app**

  Run the frontend dev server (`cd frontend && pnpm run dev`), navigate to the delete-account page,
  confirm the new sentence renders alongside the existing warning. Run `cd frontend && pnpm test`
  to confirm nothing else broke (no existing test targets `DeleteAccount.svelte` or the
  `deleteAccount` locale keys, so a clean run is the expected signal — no new test is required by
  this task, but nothing should regress).
  Expected: page shows both the existing permanence warning and the new cancellation/no-refund
  sentence; no missing-key fallback text for `deleteAccount.billingWarning`.

- [ ] **Step 5: Commit**

  ```bash
  git add frontend/src/lib/i18n/locales/ frontend/src/lib/DeleteAccount.svelte
  git commit -m "feat(#262): warn that account deletion cancels subscription with no refund"
  ```

---

## Notes for the implementer

- **Test harness reuse:** `test_pool()`/`mk_user()` (billing.rs) and the raw `AppState { db, cipher
  }` + `build_totp_strict` pattern (account.rs, see
  `delete_account_accepts_legacy_plaintext_secret`) are the REAL existing helpers — reuse them, do
  not invent parallel scaffolding.
- **Env-var test isolation:** tests mutate process env (`STRIPE_API_BASE`, `STRIPE_SECRET_KEY`).
  Every such test is `#[serial_test::serial]` so Cargo's parallel runner doesn't clobber them, AND
  must `std::env::remove_var(...)` both vars before returning (matching the existing convention in
  `backend/src/github.rs`/`backend/src/rag.rs`) so it doesn't leak a dead mock-server address into
  any test that runs later in the same test binary.
- **There is no root `Cargo.toml`** — `backend/` is a standalone crate, not a workspace member.
  Every cargo command must be run from inside `backend/` (`cd backend && cargo test ...`), not from
  the repo root with a `-p backend` flag.
- **Run the ignored DB tests** against local pgvector (`podman-compose up -d`, port 6153 — already
  running in this environment). `cd backend && cargo test -- --ignored` runs the full ignored
  suite; scope with a name filter per step above to iterate faster.
- **No CI test gate:** this repo has no test-running CI workflow — `cd backend && cargo build`/
  `cargo test`/`cargo clippy --all-targets` and `cd frontend && pnpm test`/`pnpm build` must be run
  locally before each commit, not assumed to be caught later by CI.
- **Deploy is release-gated:** merging to `main` does not deploy by itself. `release-please` cuts a
  per-package release PR from Conventional Commits; only merging THAT PR triggers
  `deploy-backend`/`deploy-frontend`. This PR's own merge will not itself deploy anything.
