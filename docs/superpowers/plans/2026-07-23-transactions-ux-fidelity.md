# Transactions UX Fidelity (P5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `TransactionsView` up to the approved mockup's visual quality, make duplicate matching fire in either logging order, and surface per-row detail so users rarely need their bank app.

**Architecture:** Frontend is a Svelte 5 component (`TransactionsView.svelte`) backed by pure helpers (`transactionsView.js`). Backend is Axum + sqlx (Postgres/pgvector). Duplicate matching lives in `duplicate_match.rs`; today it only runs at bank-import time, so this plan adds the reverse (log-time) direction. All visual work maps the mockup's bespoke palette onto existing daisyUI theme tokens.

**Tech Stack:** Rust (Axum, sqlx, tokio, chrono, uuid), Svelte 5 (runes), daisyUI/Tailwind, `svelte-i18n`, lucide-svelte, Vitest (node env), `cargo test` with `#[ignore]` Postgres tests.

## Global Constraints

- No new DB migration — every field used already exists (`provider_transaction_id`, `matched_transaction_id`, `review_status`, `currency`, `source`).
- All list/mutation queries stay scoped to the viewer's **active budget**; mutations keep Owner/Edit + audit as today.
- Duplicate matcher is heuristic and **non-destructive** — it only *sets a link*; it never deletes or auto-excludes. Error in the matcher must NEVER abort a create/import.
- The `matched_transaction_id` link always lives on the **imported** row, pointing at the `ai`/`manual` twin — both matching directions and the existing `resolve-match` endpoint rely on this.
- Amounts are unsigned magnitudes; direction/tone derives from category **type**, never the amount sign.
- i18n keys must be added to **all 6 locales**: `en, es, fr, de, it, pt` (an existing `i18nLocales.test.js` enforces key parity).
- No Claude self-attribution in any commit/PR/comment/artifact.
- Backend `#[ignore]` DB tests run against local pgvector (podman, port 6153) via `cargo test -- --ignored`.
- Branch: `feat/transactions-ux-fidelity` (already created; the spec commit `e0e2c70` is its first commit).

---

### Task 1: `isPending` = needs-review OR uncategorized (pure helper)

**Files:**
- Modify: `frontend/src/lib/transactionsView.js` (the `isPending` function, ~line 310)
- Test: `frontend/src/lib/transactionsView.test.js`

**Interfaces:**
- Consumes: existing `needsReview(tx)`, `isUncategorized(tx)` (already exported).
- Produces: `isPending(tx)` now returns `true` when a row is `needs_review` **or** has no category. `filterPending` and `applyTransactionFilters` are unchanged (they delegate to `isPending`).

- [ ] **Step 1: Update the failing tests first**

In `transactionsView.test.js`, find the existing `describe("isPending"...)` (and any `filterPending`/`applyTransactionFilters` cases that assert the old "coincides exactly with needsReview" behavior) and replace/extend with:

```javascript
describe("isPending", () => {
  it("is true for needs_review rows", () => {
    expect(isPending({ review_status: "needs_review", category_id: "c1" })).toBe(true);
  });
  it("is true for uncategorized rows even when reviewed", () => {
    expect(isPending({ review_status: "reviewed", category_id: null })).toBe(true);
  });
  it("is false for a reviewed, categorized row", () => {
    expect(isPending({ review_status: "reviewed", category_id: "c1" })).toBe(false);
  });
  it("is false/robust for null-ish input", () => {
    expect(isPending(null)).toBe(false);
    expect(isPending(undefined)).toBe(false);
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/transactionsView.test.js -t isPending`
Expected: FAIL — the "uncategorized even when reviewed" case returns `false` under the current definition.

- [ ] **Step 3: Update `isPending`**

Replace the body and update the docblock to reflect the new rule:

```javascript
/**
 * "Pending" predicate for the Pending filter chip (#403 P4 / P5). A row is
 * pending when it still needs attention: either a freshly bank-synced row
 * awaiting approval (`needs_review`) OR a row with no category assigned yet
 * (`category_id` null). This is the app's notion of "not yet settled" until a
 * provider-level pending/hold flag is modeled (own follow-up).
 * @param {{review_status?: string, category_id?: string|null}|null|undefined} tx
 * @returns {boolean}
 */
export function isPending(tx) {
  return needsReview(tx) || isUncategorized(tx);
}
```

Note: `isUncategorized` is declared later in the file but function declarations hoist, so calling it here is fine.

- [ ] **Step 4: Run tests to verify pass**

Run: `cd frontend && npx vitest run src/lib/transactionsView.test.js`
Expected: PASS (all, including grouping/search/filter suites).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/transactionsView.js frontend/src/lib/transactionsView.test.js
git commit -m "feat(#403): pending = needs-review OR uncategorized"
```

---

### Task 2: `findTwin` + `filterCounts` helpers (pure)

**Files:**
- Modify: `frontend/src/lib/transactionsView.js` (append new exports at end)
- Test: `frontend/src/lib/transactionsView.test.js`

**Interfaces:**
- Consumes: existing `needsReview`, `isPending`, `isUncategorized`, `possibleDuplicate`.
- Produces:
  - `findTwin(transactions, tx)` → the row whose `id === tx.matched_transaction_id`, or `null`. Used by the stitched-duplicate UI (Task 9) to render the imported row next to its Nels twin.
  - `filterCounts(transactions)` → `{ all, needsReview, pending, uncategorized, duplicates }` counts over the full array. Used for chip counts + the alert strip (Task 8).

- [ ] **Step 1: Write failing tests**

Add to `transactionsView.test.js` (and add `findTwin, filterCounts` to the import list at the top of the file):

```javascript
describe("findTwin", () => {
  const rows = [
    { id: "a", matched_transaction_id: "b" },
    { id: "b", matched_transaction_id: null },
  ];
  it("returns the row referenced by matched_transaction_id", () => {
    expect(findTwin(rows, rows[0])).toBe(rows[1]);
  });
  it("returns null when there is no link or no match", () => {
    expect(findTwin(rows, rows[1])).toBe(null);
    expect(findTwin(rows, { id: "a", matched_transaction_id: "zzz" })).toBe(null);
    expect(findTwin(null, rows[0])).toBe(null);
  });
});

describe("filterCounts", () => {
  it("counts each chip bucket and duplicates", () => {
    const rows = [
      { id: "1", review_status: "needs_review", category_id: null, matched_transaction_id: "9" }, // review + uncat + dup
      { id: "2", review_status: "reviewed", category_id: "c1" },                                  // none
      { id: "3", review_status: "reviewed", category_id: null },                                  // uncat + pending
    ];
    expect(filterCounts(rows)).toEqual({
      all: 3, needsReview: 1, pending: 2, uncategorized: 2, duplicates: 1,
    });
  });
  it("is all-zero for empty/null", () => {
    expect(filterCounts([])).toEqual({ all: 0, needsReview: 0, pending: 0, uncategorized: 0, duplicates: 0 });
    expect(filterCounts(null)).toEqual({ all: 0, needsReview: 0, pending: 0, uncategorized: 0, duplicates: 0 });
  });
});
```

- [ ] **Step 2: Run to verify fail**

Run: `cd frontend && npx vitest run src/lib/transactionsView.test.js -t "findTwin|filterCounts"`
Expected: FAIL — `findTwin`/`filterCounts` are not defined.

- [ ] **Step 3: Implement the helpers**

Append to `transactionsView.js`:

```javascript
/**
 * Locate the row a duplicate link points at (#403 P5). Given a row `tx` with a
 * `matched_transaction_id`, return the row in `transactions` with that id, or
 * `null` (no link, no match, or nullish input). Used to render an imported row
 * beside its Nels-logged twin in the stitched-duplicate card.
 * @param {Array<{id: string}>} transactions
 * @param {{matched_transaction_id?: string|null}|null|undefined} tx
 * @returns {object|null}
 */
export function findTwin(transactions, tx) {
  const target = tx?.matched_transaction_id;
  if (!transactions || target == null) return null;
  return transactions.find((t) => t.id === target) ?? null;
}

/**
 * Live counts for the filter chips + alert strip (#403 P5), computed over the
 * FULL loaded set (not the already-filtered view) so each chip shows how many
 * rows it would match. `duplicates` counts resolvable possible-duplicate rows.
 * @param {Array} transactions
 * @returns {{all:number, needsReview:number, pending:number, uncategorized:number, duplicates:number}}
 */
export function filterCounts(transactions) {
  const rows = transactions || [];
  return {
    all: rows.length,
    needsReview: rows.filter(needsReview).length,
    pending: rows.filter(isPending).length,
    uncategorized: rows.filter(isUncategorized).length,
    duplicates: rows.filter(possibleDuplicate).length,
  };
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd frontend && npx vitest run src/lib/transactionsView.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/transactionsView.js frontend/src/lib/transactionsView.test.js
git commit -m "feat(#403): findTwin + filterCounts helpers"
```

---

### Task 3: Surface `provider_transaction_id` in the API

**Files:**
- Modify: `backend/src/budget.rs` — `TransactionResponse` struct (~line 296); the `list_transactions` mapping (~4577); the three single-row response builders that construct a `TransactionResponse` (create ~3768, finalize ~3870, update ~4180 — every literal `TransactionResponse { … matched_transaction_id: … }`).
- Test: `backend/src/budget.rs` test module — the existing `list_transactions_surfaces_provenance_and_account_label` (~11453).

**Interfaces:**
- Consumes: `Transaction.provider_transaction_id` (already on the struct, already selected by `t.*`).
- Produces: `TransactionResponse.provider_transaction_id: Option<String>` in JSON — the frontend detail panel (Task 10) reads it.

- [ ] **Step 1: Extend the response DB-test assertion (failing)**

In the `list_transactions_surfaces_provenance_and_account_label` test, after it seeds an imported row, set a provider id on that row and assert it round-trips. Add near the seeding (adapt the row's id variable name to the test's existing one, e.g. `imported_id`):

```rust
sqlx::query("UPDATE transactions SET provider_transaction_id = 'fctxn_test123' WHERE id = $1")
    .bind(imported_id)
    .execute(&pool)
    .await
    .unwrap();
```

and in the assertions for that imported row:

```rust
assert_eq!(
    imported_resp.provider_transaction_id.as_deref(),
    Some("fctxn_test123"),
    "imported row surfaces its bank provider transaction id"
);
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cd backend && cargo test --lib budget::tests::list_transactions_surfaces_provenance_and_account_label -- --ignored`
Expected: FAIL — `no field provider_transaction_id on TransactionResponse`.

- [ ] **Step 3: Add the field + all mappings**

In the `TransactionResponse` struct, after `matched_transaction_id`:

```rust
    /// The bank provider's own transaction id (#403 P5), present only on
    /// imported rows. Surfaced so the detail view can show it for cross-
    /// referencing against the bank app; NULL on ai/manual rows.
    pub provider_transaction_id: Option<String>,
```

In `list_transactions`'s `resp.push(TransactionResponse { … })`, add:

```rust
            provider_transaction_id: r.get("provider_transaction_id"),
```

In each single-row builder (`create_transaction`, `finalize_transaction`, `update_transaction`) that builds from a `Transaction` value (commonly named `transaction` or `tx`), add:

```rust
        provider_transaction_id: transaction.provider_transaction_id.clone(),
```

(use `tx.provider_transaction_id.clone()` where the local is named `tx`; `.clone()` because `description`/other owned fields may already be moved — match the surrounding style, cloning if the value is used after).

- [ ] **Step 4: Run to verify pass + compile**

Run: `cd backend && cargo test --lib budget::tests::list_transactions_surfaces_provenance_and_account_label -- --ignored`
Expected: PASS. Then `cargo build` to confirm all three builders compile.

- [ ] **Step 5: Commit**

```bash
git add backend/src/budget.rs
git commit -m "feat(#403): expose provider_transaction_id on TransactionResponse"
```

---

### Task 4: Reverse (log-time) duplicate candidate finder

**Files:**
- Modify: `backend/src/duplicate_match.rs` (add function + tests)

**Interfaces:**
- Consumes: `Transaction`, `DEFAULT_WINDOW_DAYS`, the same amount/currency/window/category-type predicates as `find_duplicate_candidate`.
- Produces: `find_import_duplicate_candidate(pool, budget_id, logged: &Transaction, window_days) -> Result<Option<Uuid>, sqlx::Error>` — returns the id of the best **imported**, not-yet-linked, not-excluded row that the just-logged `ai`/`manual` `logged` row duplicates. (The link itself is set by Task 5.)

- [ ] **Step 1: Write failing DB tests**

Add to the `tests` module in `duplicate_match.rs`:

```rust
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
#[serial_test::serial]
async fn reverse_finder_matches_import_logged_after_it() {
    let db = test_pool().await;
    let user = mk_user(&db).await;
    let budget = mk_budget(&db, user).await;
    let now = Utc::now();
    // Bank imported the charge FIRST (the common ordering).
    let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE #14").await;
    // User logs it later; same amount, a day off (posting lag).
    let logged = mk_tx(&db, budget, "ai", 15.0, None, now - chrono::Duration::days(1), None, "coffee").await;

    let got = find_import_duplicate_candidate(&db, budget, &load(&db, logged).await, DEFAULT_WINDOW_DAYS)
        .await
        .unwrap();
    assert_eq!(got, Some(imported), "logged row finds the earlier import within the window");
    cleanup(&db, user).await;
}

#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
#[serial_test::serial]
async fn reverse_finder_skips_already_linked_or_out_of_window() {
    let db = test_pool().await;
    let user = mk_user(&db).await;
    let budget = mk_budget(&db, user).await;
    let now = Utc::now();

    // Already-linked import must not be offered again.
    let other_nels = mk_tx(&db, budget, "ai", 15.0, None, now, None, "old").await;
    let linked_import = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "X").await;
    sqlx::query("UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2")
        .bind(other_nels).bind(linked_import).execute(&db).await.unwrap();
    // Out-of-window import (5 days > 4).
    let far_import = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now - chrono::Duration::days(5), None, "Y").await;

    let logged = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;
    let got = find_import_duplicate_candidate(&db, budget, &load(&db, logged).await, DEFAULT_WINDOW_DAYS)
        .await
        .unwrap();
    assert_eq!(got, None, "linked and out-of-window imports are both skipped");
    let _ = (linked_import, far_import);
    cleanup(&db, user).await;
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cd backend && cargo test --lib duplicate_match::tests::reverse_finder -- --ignored`
Expected: FAIL — `find_import_duplicate_candidate` not defined.

- [ ] **Step 3: Implement the reverse finder**

Add to `duplicate_match.rs` (below `find_duplicate_candidate`):

```rust
/// Reverse of [`find_duplicate_candidate`] (#403 P5): given a just-logged
/// `ai`/`manual` row, find the best pre-existing **imported** row in the same
/// budget it likely duplicates, or `None`. Handles the common ordering where
/// the bank imported the charge BEFORE the user logged it in Nels.
///
/// Match criteria (all required), symmetric with the forward matcher:
/// - same `budget_id`, candidate `source = 'imported'`, not the logged row;
/// - equal amount magnitude within half a cent;
/// - compatible currency (`COALESCE(UPPER(currency),'USD')` equal on both sides);
/// - `transaction_date` within `window_days`;
/// - candidate import is **not already linked** (`matched_transaction_id IS NULL`)
///   and **not excluded** from the budget;
/// - when both rows are categorized, matching category type.
///
/// Ties break by nearest date, exact (case-insensitive) description match, then
/// the older row. The caller sets `imported.matched_transaction_id = logged.id`.
pub async fn find_import_duplicate_candidate(
    pool: &PgPool,
    budget_id: Uuid,
    logged: &Transaction,
    window_days: i64,
) -> Result<Option<Uuid>, sqlx::Error> {
    let window_secs = (window_days.max(0) as f64) * 86_400.0;
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT t.id \
         FROM transactions t \
         LEFT JOIN categories tc ON tc.id = t.category_id \
         WHERE t.budget_id = $1 \
           AND t.source = 'imported' \
           AND t.id <> $2 \
           AND t.matched_transaction_id IS NULL \
           AND t.excluded_from_budget = false \
           AND ABS(t.amount - $3) < 0.005 \
           AND COALESCE(UPPER(t.currency), 'USD') = COALESCE(UPPER($4), 'USD') \
           AND ABS(EXTRACT(EPOCH FROM (t.transaction_date - $5))) <= $6 \
           AND ( \
                 t.category_id IS NULL \
                 OR $7::uuid IS NULL \
                 OR tc.category_type = (SELECT category_type FROM categories WHERE id = $7) \
               ) \
         ORDER BY ABS(EXTRACT(EPOCH FROM (t.transaction_date - $5))) ASC, \
                  (lower(btrim(t.description)) = lower(btrim($8))) DESC, \
                  t.created_at ASC \
         LIMIT 1",
    )
    .bind(budget_id)
    .bind(logged.id)
    .bind(logged.amount)
    .bind(logged.currency.as_deref())
    .bind(logged.transaction_date)
    .bind(window_secs)
    .bind(logged.category_id)
    .bind(&logged.description)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd backend && cargo test --lib duplicate_match::tests::reverse_finder -- --ignored`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add backend/src/duplicate_match.rs
git commit -m "feat(#403): reverse duplicate finder (import matched when logged after)"
```

---

### Task 5: Wire the reverse matcher into the logging paths

**Files:**
- Modify: `backend/src/duplicate_match.rs` — add `link_duplicate_for_logged`
- Modify: `backend/src/budget.rs` — call it in `create_transaction` (~after line 3748) and inside `insert_transaction_with_embedding` (~before `Ok(id)` at line 4520; this covers `finalize_transaction` and the chat add-transaction path in `rag.rs:3249` for free)
- Test: `backend/src/duplicate_match.rs` tests

**Interfaces:**
- Consumes: `find_import_duplicate_candidate` (Task 4).
- Produces: `link_duplicate_for_logged(pool, budget_id, logged_id)` — loads the logged row; if it is excluded, no-op; else find an import candidate and, on a hit, `UPDATE transactions SET matched_transaction_id = logged_id WHERE id = <candidate import>`. Error-swallowing (warn-log only) so it never fails a create.

- [ ] **Step 1: Write failing wrapper DB test**

Add to the `tests` module in `duplicate_match.rs`:

```rust
#[tokio::test]
#[ignore = "requires Postgres; run via: cargo test -- --ignored"]
#[serial_test::serial]
async fn link_for_logged_links_the_import_to_the_new_row() {
    let db = test_pool().await;
    let user = mk_user(&db).await;
    let budget = mk_budget(&db, user).await;
    let now = Utc::now();
    let imported = mk_tx(&db, budget, "imported", 42.18, Some("USD"), now, None, "TRADER JOE'S").await;
    let logged = mk_tx(&db, budget, "ai", 42.18, None, now, None, "groceries").await;

    link_duplicate_for_logged(&db, budget, logged).await;

    assert_eq!(
        load(&db, imported).await.matched_transaction_id,
        Some(logged),
        "the import is linked to the row the user just logged"
    );
    cleanup(&db, user).await;
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cd backend && cargo test --lib duplicate_match::tests::link_for_logged -- --ignored`
Expected: FAIL — `link_duplicate_for_logged` not defined.

- [ ] **Step 3: Implement the wrapper**

Add to `duplicate_match.rs`:

```rust
/// Fail-safe wrapper called after an `ai`/`manual` transaction is created
/// (#403 P5). Runs [`find_import_duplicate_candidate`] and, on a hit, sets the
/// matched IMPORTED row's `matched_transaction_id` to the just-logged row's id
/// (the link always lives on the import). Mirrors `link_duplicate_for_import`'s
/// error-swallowing contract: a matcher hiccup must never fail the create.
pub async fn link_duplicate_for_logged(pool: &PgPool, budget_id: Uuid, logged_id: Uuid) {
    let logged = match sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(logged_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("duplicate matcher: failed to load logged row {logged_id}: {e}");
            return;
        }
    };

    // An excluded logged row contributes nothing to the budget, so there is
    // nothing to de-double-count — and linking could later exclude a real import.
    if logged.excluded_from_budget {
        return;
    }

    match find_import_duplicate_candidate(pool, budget_id, &logged, DEFAULT_WINDOW_DAYS).await {
        Ok(Some(import_id)) => {
            if let Err(e) = sqlx::query(
                "UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2",
            )
            .bind(logged_id)
            .bind(import_id)
            .execute(pool)
            .await
            {
                tracing::warn!("duplicate matcher: failed to link import {import_id} -> {logged_id}: {e}");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("duplicate matcher: reverse candidate query failed for {logged_id}: {e}");
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd backend && cargo test --lib duplicate_match::tests::link_for_logged -- --ignored`
Expected: PASS.

- [ ] **Step 5: Wire into the two logging entry points**

In `budget.rs::create_transaction`, immediately after the `log_audit(... "ADD_TRANSACTION" ...)` call (~line 3750), add:

```rust
    // Reverse duplicate match (#403 P5): if the bank already imported this charge,
    // link that import to this row so the user gets a resolvable duplicate instead
    // of a silent double-count. Non-destructive, error-swallowing.
    crate::duplicate_match::link_duplicate_for_logged(&state.db, budget_id, transaction.id).await;
```

In `budget.rs::insert_transaction_with_embedding`, replace the final `Ok(id)` (line ~4520) with:

```rust
    // Reverse duplicate match (#403 P5) — covers finalize_transaction and the
    // chat add-transaction path, which both insert through this helper.
    crate::duplicate_match::link_duplicate_for_logged(db, budget_id, id).await;
    Ok(id)
```

- [ ] **Step 6: Verify the whole backend compiles + full matcher suite passes**

Run: `cd backend && cargo build && cargo test --lib duplicate_match:: -- --ignored`
Expected: build OK; all duplicate_match tests PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/duplicate_match.rs backend/src/budget.rs
git commit -m "feat(#403): match duplicates when a charge is logged after the bank imports it"
```

---

### Task 6: i18n keys for alert strip, All chip, stitch header, detail labels

**Files:**
- Modify: `frontend/src/lib/i18n/locales/{en,es,fr,de,it,pt}.json` (each `transactions` block)
- Test: `frontend/src/lib/i18nLocales.test.js` (key-parity guard — no edit needed, it will catch a missing locale)

**Interfaces:**
- Produces: new `transactions.*` keys consumed by Tasks 8–10.

- [ ] **Step 1: Add keys to English**

Add these into the `transactions` object of `en.json` (keep existing keys):

```json
  "all": "All",
  "reviewSummary": "{count} imported transactions need review",
  "duplicateSummary": "{count} look like duplicates",
  "duplicateStitchHeader": "You logged this, then your bank imported it",
  "showDetails": "Details",
  "hideDetails": "Hide details",
  "detailRawDescription": "Bank description",
  "detailAccount": "Account",
  "detailDate": "Date",
  "detailAmount": "Amount",
  "detailCategory": "Category",
  "detailSource": "Source",
  "detailProviderId": "Bank reference"
```

- [ ] **Step 2: Run the parity test to verify it fails for other locales**

Run: `cd frontend && npx vitest run src/lib/i18nLocales.test.js`
Expected: FAIL — `es/fr/de/it/pt` are missing the new keys.

- [ ] **Step 3: Add the translated keys to the other five locales**

Add the same keys with locale-appropriate values to `es.json, fr.json, de.json, it.json, pt.json`. Use these translations:

```
es: all "Todas" · reviewSummary "{count} transacciones importadas necesitan revisión" · duplicateSummary "{count} parecen duplicados" · duplicateStitchHeader "Lo registraste tú y luego lo importó tu banco" · showDetails "Detalles" · hideDetails "Ocultar detalles" · detailRawDescription "Descripción del banco" · detailAccount "Cuenta" · detailDate "Fecha" · detailAmount "Importe" · detailCategory "Categoría" · detailSource "Origen" · detailProviderId "Referencia del banco"
fr: all "Toutes" · reviewSummary "{count} transactions importées à vérifier" · duplicateSummary "{count} semblent être des doublons" · duplicateStitchHeader "Vous l’avez saisie, puis votre banque l’a importée" · showDetails "Détails" · hideDetails "Masquer les détails" · detailRawDescription "Libellé bancaire" · detailAccount "Compte" · detailDate "Date" · detailAmount "Montant" · detailCategory "Catégorie" · detailSource "Origine" · detailProviderId "Référence bancaire"
de: all "Alle" · reviewSummary "{count} importierte Transaktionen benötigen eine Prüfung" · duplicateSummary "{count} sehen wie Duplikate aus" · duplicateStitchHeader "Du hast sie erfasst, dann hat deine Bank sie importiert" · showDetails "Details" · hideDetails "Details ausblenden" · detailRawDescription "Bankbeschreibung" · detailAccount "Konto" · detailDate "Datum" · detailAmount "Betrag" · detailCategory "Kategorie" · detailSource "Quelle" · detailProviderId "Bankreferenz"
it: all "Tutte" · reviewSummary "{count} transazioni importate da rivedere" · duplicateSummary "{count} sembrano duplicati" · duplicateStitchHeader "L’hai registrata tu, poi la tua banca l’ha importata" · showDetails "Dettagli" · hideDetails "Nascondi dettagli" · detailRawDescription "Descrizione della banca" · detailAccount "Conto" · detailDate "Data" · detailAmount "Importo" · detailCategory "Categoria" · detailSource "Origine" · detailProviderId "Riferimento bancario"
pt: all "Todas" · reviewSummary "{count} transações importadas precisam de revisão" · duplicateSummary "{count} parecem duplicadas" · duplicateStitchHeader "Você registrou e depois seu banco importou" · showDetails "Detalhes" · hideDetails "Ocultar detalhes" · detailRawDescription "Descrição do banco" · detailAccount "Conta" · detailDate "Data" · detailAmount "Valor" · detailCategory "Categoria" · detailSource "Origem" · detailProviderId "Referência do banco"
```

- [ ] **Step 4: Run parity test to verify pass**

Run: `cd frontend && npx vitest run src/lib/i18nLocales.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/i18n/locales
git commit -m "feat(#403): i18n keys for alert strip, All chip, duplicate stitch + detail"
```

---

### Task 7: Row visual system — provenance rail, avatar tile, ledger amount

**Files:**
- Modify: `frontend/src/lib/TransactionsView.svelte` (the per-row `{#each g.rows as tx}` block, ~lines 311–437; imports at top)

**Interfaces:**
- Consumes: `provenanceKind`, `needsReview`, `isPending`, `displayAmount`, `amountClass` (existing).
- Produces: the restyled collapsed row that Tasks 8–10 build on (chips/strip above it, stitched card + detail panel within/after it).

No Vitest gate (node-env only). Verified by `npm run build` + a browser smoke.

- [ ] **Step 1: Add the source→rail/tile styling helper**

At the top `<script>`, after the existing imports, add a small mapping used by the markup:

```javascript
  // Map provenance → daisyUI accent classes for the left rail + avatar tile
  // (#403 P5). Keeps the mockup's "indigo=Nels / teal=bank / neutral=you" idea
  // on theme tokens so light/dark still work.
  const railClass = (tx) =>
    provenanceKind(tx) === "ai"
      ? "border-l-primary"
      : provenanceKind(tx) === "imported"
        ? "border-l-secondary"
        : "border-l-base-300";
  const tileClass = (tx) =>
    provenanceKind(tx) === "ai"
      ? "bg-primary/15 text-primary"
      : provenanceKind(tx) === "imported"
        ? "bg-secondary/15 text-secondary"
        : "bg-base-200 text-base-content/60";
```

- [ ] **Step 2: Replace the row container + add rail/avatar/ledger layout**

Replace the row wrapper `<div class="rounded-xl bg-base-100 border border-base-300 p-3 flex flex-wrap items-center justify-between gap-2" …>` and its immediate children down to the amount line, with:

```svelte
        <div
          class={`rounded-xl bg-base-100 border border-base-300 border-l-4 ${railClass(tx)} p-3`}
          class:opacity-60={tx.excluded_from_budget}
          class:opacity-70={needsReview(tx) && !tx.excluded_from_budget}
        >
          <div class="flex items-center gap-3">
            <!-- Source-tinted avatar tile (decorative) -->
            <span class={`shrink-0 w-8 h-8 rounded-lg grid place-items-center ${tileClass(tx)}`} aria-hidden="true">
              {#if provenanceKind(tx) === "ai"}
                <Sparkles class="w-4 h-4" />
              {:else if provenanceKind(tx) === "imported"}
                <Link2 class="w-4 h-4" />
              {:else}
                <Tag class="w-4 h-4" />
              {/if}
            </span>

            <div class="min-w-0 flex-1">
              <div class="font-medium truncate">{tx.description}</div>
              <!-- Provenance + state sub-line (unchanged badges) -->
              <div class="flex flex-wrap items-center gap-1.5 mt-0.5">
                <!-- KEEP the existing provenance/needsReview/excluded badge block here -->
              </div>
            </div>

            <!-- Ledger amount: right-aligned, tabular monospace -->
            <div class="shrink-0 text-right">
              <div class={`font-mono tabular-nums ${amountClass(tx)}`}>{displayAmount(tx)}</div>
              <div class="text-xs text-base-content/50">{fmtDate(tx.transaction_date)}</div>
            </div>
          </div>

          <!-- Row controls (approve + category select) move onto their own line -->
          <div class="flex items-center gap-2 flex-wrap mt-2 pl-11">
            <!-- KEEP the existing needsReview Approve button + category <select> block here -->
          </div>

          <!-- KEEP the existing {#if possibleDuplicate(tx)} banner block here for now
               (Task 9 replaces it with the stitched card). -->
        </div>
```

Move the existing provenance/needs-review/excluded badge markup into the "sub-line" slot, and the existing Approve button + category `<select>` markup into the "Row controls" slot. Do not delete them — relocate.

- [ ] **Step 3: Build to verify it compiles + renders**

Run: `cd frontend && npm run build`
Expected: build succeeds (no Svelte/type errors).

- [ ] **Step 4: Browser smoke**

Launch the app (use the project `run` skill or existing dev command) and open the transactions list via the `/transactions-list` command. Confirm: each row shows a colored left rail (indigo/teal/neutral), an avatar tile, a right-aligned monospace amount, and that excluded/needs-review rows are dimmed. No console errors.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/TransactionsView.svelte
git commit -m "feat(#403): provenance rail, avatar tile, ledger amounts on rows"
```

---

### Task 8: Compact scrollable chips + All + counts + alert strip

**Files:**
- Modify: `frontend/src/lib/TransactionsView.svelte` (the chips block ~lines 234–279; add the alert strip above the list; import `filterCounts`)

**Interfaces:**
- Consumes: `filterCounts`, `isPending`, `needsReview`, `possibleDuplicate`, i18n keys from Task 6.
- Produces: a chip row that never wraps, an "All" reset, live counts, and a clickable alert strip.

- [ ] **Step 1: Add counts to script**

Add `filterCounts` to the `transactionsView.js` import list, and a derived count:

```javascript
  const counts = $derived(filterCounts(transactions));
  const anyActiveFilter = $derived(
    showNeedsReviewOnly || showPendingOnly || showUncategorizedOnly || search.trim() !== "",
  );
  function clearFilters() {
    showNeedsReviewOnly = false;
    showPendingOnly = false;
    showUncategorizedOnly = false;
    search = "";
  }
```

- [ ] **Step 2: Replace the chips block with compact, scrollable chips + All**

Replace the `<div class="flex flex-wrap items-center gap-2">…</div>` chip container with:

```svelte
      <div class="flex items-center gap-2 overflow-x-auto pb-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        <button type="button"
          class="btn btn-xs rounded-full shrink-0"
          class:btn-neutral={!anyActiveFilter}
          class:btn-ghost={anyActiveFilter}
          aria-pressed={!anyActiveFilter}
          onclick={clearFilters}>
          {$_("transactions.all")} <span class="opacity-70">{counts.all}</span>
        </button>
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showNeedsReviewOnly}
          class:btn-ghost={!showNeedsReviewOnly}
          aria-pressed={showNeedsReviewOnly}
          onclick={() => (showNeedsReviewOnly = !showNeedsReviewOnly)}>
          <Clock class="w-3 h-3" />{$_("transactions.needsReview")} <span class="opacity-70">{counts.needsReview}</span>
        </button>
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showPendingOnly}
          class:btn-ghost={!showPendingOnly}
          aria-pressed={showPendingOnly}
          onclick={() => (showPendingOnly = !showPendingOnly)}>
          <Clock class="w-3 h-3" />{$_("transactions.pending")} <span class="opacity-70">{counts.pending}</span>
        </button>
        <button type="button"
          class="btn btn-xs rounded-full shrink-0 gap-1"
          class:btn-warning={showUncategorizedOnly}
          class:btn-ghost={!showUncategorizedOnly}
          aria-pressed={showUncategorizedOnly}
          onclick={() => (showUncategorizedOnly = !showUncategorizedOnly)}>
          <Tag class="w-3 h-3" />{$_("transactions.uncategorized")} <span class="opacity-70">{counts.uncategorized}</span>
        </button>
      </div>
```

- [ ] **Step 3: Add the alert strip above the groups**

Immediately before the `{#if groups.length === 0}` block, add:

```svelte
    {#if counts.needsReview > 0 || counts.duplicates > 0}
      <button type="button"
        class="alert alert-warning py-2 mb-2 text-sm text-left w-full"
        onclick={() => (showNeedsReviewOnly = true)}>
        <AlertTriangle class="w-4 h-4 shrink-0" />
        <span>
          {$_("transactions.reviewSummary", { values: { count: counts.needsReview } })}{#if counts.duplicates > 0}
            &nbsp;·&nbsp;{$_("transactions.duplicateSummary", { values: { count: counts.duplicates } })}{/if}
        </span>
      </button>
    {/if}
```

- [ ] **Step 4: Build + smoke**

Run: `cd frontend && npm run build`
Then in-browser: chips sit in one non-wrapping row that scrolls horizontally on a narrow viewport; each shows a count; "All" clears active filters + search; the alert strip appears only when something needs review/looks duplicate and clicking it activates the Needs Review filter.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/TransactionsView.svelte
git commit -m "feat(#403): compact scrollable chips with All + counts + review alert strip"
```

---

### Task 9: Stitched duplicate pair

**Files:**
- Modify: `frontend/src/lib/TransactionsView.svelte` (import `findTwin`; replace the in-row `possibleDuplicate` banner with a bracketed pair rendered once per imported duplicate; suppress the twin's standalone render)

**Interfaces:**
- Consumes: `findTwin`, `possibleDuplicate`, `resolveDuplicate`, i18n `duplicateStitchHeader`, `mergeKeepImported`, `keepBoth`, `possibleDuplicate`.
- Produces: both twins shown together with Merge/Keep-both.

- [ ] **Step 1: Add a set of twin ids to hide from the normal feed**

In the script, derive the ids of Nels twins that are being shown inside a stitch, so they aren't also rendered as standalone rows:

```javascript
  // Ids of Nels rows currently displayed inside a duplicate stitch (#403 P5) —
  // hidden from their normal date-group position so the pair reads together.
  const stitchedTwinIds = $derived(
    new Set(
      (visibleTransactions || [])
        .filter(possibleDuplicate)
        .map((tx) => tx.matched_transaction_id)
        .filter(Boolean),
    ),
  );
```

- [ ] **Step 2: Skip the twin's standalone render**

Wrap the per-row render so a stitched twin is skipped:

```svelte
            {#each g.rows as tx (tx.id)}
              {#if !stitchedTwinIds.has(tx.id)}
                <!-- existing row markup (Task 7) -->
              {/if}
            {/each}
```

- [ ] **Step 3: Replace the in-row duplicate banner with the stitched card**

Remove the old `{#if possibleDuplicate(tx)}` banner block from inside the row (Task 7 kept it). Instead, render the stitch as a wrapper around the imported row: change the row `{#if !stitchedTwinIds.has(tx.id)}` branch so that when `possibleDuplicate(tx)` is true it renders the twin above it inside a bordered card. Concretely, at the top of the row branch:

```svelte
                {#if possibleDuplicate(tx)}
                  {@const twin = findTwin(transactions, tx)}
                  <div class="rounded-xl border border-warning/50 bg-warning/5 overflow-hidden">
                    <div class="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold text-warning">
                      <Copy class="w-3.5 h-3.5" />
                      {$_("transactions.duplicateStitchHeader")}
                    </div>
                    {#if twin}
                      <!-- render the twin as a compact row -->
                      <div class="flex items-center gap-3 px-3 py-2 border-t border-warning/30">
                        <span class="shrink-0 w-7 h-7 rounded-lg grid place-items-center bg-primary/15 text-primary" aria-hidden="true"><Sparkles class="w-3.5 h-3.5" /></span>
                        <div class="min-w-0 flex-1 truncate text-sm">{twin.description}</div>
                        <div class="font-mono tabular-nums text-sm">{displayAmount(twin)}</div>
                      </div>
                    {/if}
                    <!-- the imported row itself -->
                    <div class="flex items-center gap-3 px-3 py-2 border-t border-warning/30">
                      <span class="shrink-0 w-7 h-7 rounded-lg grid place-items-center bg-secondary/15 text-secondary" aria-hidden="true"><Link2 class="w-3.5 h-3.5" /></span>
                      <div class="min-w-0 flex-1">
                        <div class="truncate text-sm">{tx.description}</div>
                        {#if tx.account_label}<div class="text-xs text-base-content/60">{tx.account_label}</div>{/if}
                      </div>
                      <div class="font-mono tabular-nums text-sm">{displayAmount(tx)}</div>
                    </div>
                    <div class="flex items-center gap-2 px-3 py-2 border-t border-warning/30">
                      <button type="button" class="btn btn-xs btn-warning gap-1" onclick={() => resolveDuplicate(tx, "merge")}>
                        {$_("transactions.mergeKeepImported")}
                      </button>
                      <button type="button" class="btn btn-xs btn-ghost" onclick={() => resolveDuplicate(tx, "dismiss")}>
                        {$_("transactions.keepBoth")}
                      </button>
                    </div>
                  </div>
                {:else}
                  <!-- the normal Task 7 row markup -->
                {/if}
```

- [ ] **Step 4: Build + smoke**

Run: `cd frontend && npm run build`
Then, using a budget with a linked account, log a transaction matching an imported one (same amount, within 4 days) — or seed one — and confirm the two rows render bracketed together under the header, Merge strikes/excludes the Nels twin and clears the card, Keep both clears the card leaving both rows. The twin no longer appears twice.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/TransactionsView.svelte
git commit -m "feat(#403): stitched duplicate pair with merge / keep both"
```

---

### Task 10: Inline transaction detail panel

**Files:**
- Modify: `frontend/src/lib/TransactionsView.svelte` (add an `openId` state + a toggle affordance on the row + the detail panel; move the category `<select>` into the panel)

**Interfaces:**
- Consumes: `provenanceKind`, `formatAmount`, i18n detail keys (Task 6), `tx.provider_transaction_id`, `tx.account_label`.
- Produces: a per-row expandable detail panel.

- [ ] **Step 1: Add open-state + toggle**

In the script:

```javascript
  // One expanded detail panel at a time (#403 P5).
  let openId = $state(null);
  const toggleDetails = (id) => (openId = openId === id ? null : id);
```

- [ ] **Step 2: Add a details toggle to the row controls line**

In the Task-7 "Row controls" block, add a details button (before the category select):

```svelte
            <button type="button" class="btn btn-ghost btn-xs gap-1" onclick={() => toggleDetails(tx.id)}
              aria-expanded={openId === tx.id}>
              <Search class="w-3.5 h-3.5" />
              {openId === tx.id ? $_("transactions.hideDetails") : $_("transactions.showDetails")}
            </button>
```

- [ ] **Step 3: Add the detail panel below the controls**

After the row controls block, inside the row `<div>`:

```svelte
          {#if openId === tx.id}
            <dl class="mt-2 ml-11 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs bg-base-200/60 rounded-lg p-3">
              <dt class="text-base-content/60">{$_("transactions.detailRawDescription")}</dt>
              <dd class="break-words">{tx.description}</dd>
              {#if tx.account_label}
                <dt class="text-base-content/60">{$_("transactions.detailAccount")}</dt>
                <dd>{tx.account_label}</dd>
              {/if}
              <dt class="text-base-content/60">{$_("transactions.detailDate")}</dt>
              <dd>{new Date(tx.transaction_date).toLocaleString()}</dd>
              <dt class="text-base-content/60">{$_("transactions.detailAmount")}</dt>
              <dd class="font-mono tabular-nums">{formatAmount(tx.amount, tx.currency)}</dd>
              <dt class="text-base-content/60">{$_("transactions.detailCategory")}</dt>
              <dd>{tx.category_name ?? $_("transactions.uncategorized")}</dd>
              <dt class="text-base-content/60">{$_("transactions.detailSource")}</dt>
              <dd>{provenanceKind(tx) === "ai" ? $_("transactions.loggedByNels") : provenanceKind(tx) === "imported" ? (tx.account_label ?? $_("transactions.linkedAccount")) : "—"}</dd>
              {#if tx.provider_transaction_id}
                <dt class="text-base-content/60">{$_("transactions.detailProviderId")}</dt>
                <dd class="font-mono break-all">{tx.provider_transaction_id}</dd>
              {/if}
            </dl>
          {/if}
```

- [ ] **Step 4: Build + smoke**

Run: `cd frontend && npm run build`
Then in-browser: expanding an **imported** row shows the full untruncated bank description, account, exact date/time, amount, category, source, and the bank reference (`provider_transaction_id`); expanding a Nels row shows the same minus account/provider id. Only one panel open at a time.

- [ ] **Step 5: Full frontend verification**

Run: `cd frontend && npx vitest run && npm run build`
Expected: all Vitest suites PASS; build clean.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/TransactionsView.svelte
git commit -m "feat(#403): inline transaction detail panel with bank reference"
```

---

## Final verification (before PR)

- [ ] Backend: `cd backend && cargo test --lib -- --ignored` (duplicate_match + budget transaction tests green) and `cargo build`.
- [ ] Frontend: `cd frontend && npx vitest run && npm run build` (helpers + locale parity green; clean build).
- [ ] Manual: log a transaction that matches an existing import (same amount, within 4 days) and confirm the stitched duplicate appears and Merge de-double-counts the budget total.
- [ ] Open a PR from `feat/transactions-ux-fidelity`. Dual backend+frontend release → expect two release-please PRs; merge one, hand-resolve the `.release-please-manifest.json` conflict on the other (keep both versions).
```
