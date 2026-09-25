//! Non-destructive duplicate reconciliation (#403 P3).
//!
//! When account sync imports a charge that matches a transaction the user
//! already logged through Nels (`source` `ai`/`manual`), we do NOT silently
//! create a second budget hit and we NEVER auto-delete anything. Instead the
//! sync path runs [`find_duplicate_candidate`] and, on a hit, links the freshly
//! imported row to its Nels-logged twin via `matched_transaction_id`. The
//! transactions list then surfaces a resolvable "possible duplicate" the user
//! settles with merge (exclude the Nels twin) or dismiss (keep both).
//!
//! The matcher is a heuristic, so it is deliberately conservative (equal amount
//! magnitude, compatible currency, a tight date window, matching category type
//! when both are categorized) and its only effect is to *offer* a resolution —
//! a false positive is a visible, dismissable prompt, never a lost row.

use sqlx::PgPool;
use uuid::Uuid;

use crate::db::Transaction;

/// Default +/- window (days) around the imported transaction's date within which
/// a Nels-logged row is considered the same charge. Bank posting lag vs. the day
/// the user logged it is usually 1-3 days; 4 gives a little slack without
/// inviting unrelated same-amount charges from a different week.
pub const DEFAULT_WINDOW_DAYS: i64 = 4;

/// Find the best pre-existing Nels-logged (`ai`/`manual`) transaction in the same
/// budget that the just-`imported` row likely duplicates, or `None`.
///
/// Match criteria (all required):
/// - same `budget_id`, `source IN ('ai','manual')`, and not the imported row itself;
/// - equal **amount magnitude** within half a cent (amounts are unsigned `f64`);
/// - **compatible currency**: `COALESCE(UPPER(currency),'USD')` equal on both
///   sides (a NULL-currency Nels row is treated as the budget default = USD);
/// - `transaction_date` within `window_days` on either side;
/// - the candidate Nels row is not already claimed by another import;
/// - when **both** rows are categorized, matching **category type** (so a refund
///   / income row is never matched to a charge / expense of the same magnitude).
///
/// Ties break by nearest date, then a case-insensitive exact description match,
/// then the older row — deterministic, and without a `pg_trgm` dependency.
pub async fn find_duplicate_candidate(
    pool: &PgPool,
    budget_id: Uuid,
    imported: &Transaction,
    window_days: i64,
) -> Result<Option<Uuid>, sqlx::Error> {
    let window_secs = (window_days.max(0) as f64) * 86_400.0;
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT t.id \
         FROM transactions t \
         LEFT JOIN categories tc ON tc.id = t.category_id \
         WHERE t.budget_id = $1 \
           AND t.source IN ('ai', 'manual') \
           AND t.id <> $2 \
           AND ABS(t.amount - $3) < 0.005 \
           AND COALESCE(UPPER(t.currency), 'USD') = COALESCE(UPPER($4), 'USD') \
           AND ABS(EXTRACT(EPOCH FROM (t.transaction_date - $5))) <= $6 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM transactions m WHERE m.matched_transaction_id = t.id \
               ) \
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
    .bind(imported.id)
    .bind(imported.amount)
    .bind(imported.currency.as_deref())
    .bind(imported.transaction_date)
    .bind(window_secs)
    .bind(imported.category_id)
    .bind(&imported.description)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Fail-safe wrapper called from every fresh-import sync insert site after a
/// genuine insert. Loads the just-imported row, runs [`find_duplicate_candidate`]
/// at the default window, and on a hit sets `matched_transaction_id`.
///
/// Deliberately swallows every error (logging at `warn`): duplicate detection is
/// an enhancement, and a matcher hiccup (a transient DB error, a since-deleted
/// row) must NEVER abort a bank sync or fail the import that already succeeded.
/// The imported row simply keeps its `NULL` link and behaves like any other.
pub async fn link_duplicate_for_import(pool: &PgPool, budget_id: Uuid, imported_id: Uuid) {
    let imported = match sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1",
    )
    .bind(imported_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return, // row vanished between insert and here — nothing to do
        Err(e) => {
            tracing::warn!("duplicate matcher: failed to load imported row {imported_id}: {e}");
            return;
        }
    };

    // An import auto-excluded by a standing ignore rule (#374) is a non-spending
    // internal transfer that already contributes nothing to any budget total, so
    // there is nothing to de-double-count. Linking it would be worse than a no-op:
    // a later `merge` would also exclude the matched Nels row, dropping a
    // legitimate expense. Skip reconciliation for excluded imports entirely.
    if imported.excluded_from_budget {
        return;
    }

    // The forward matcher only makes sense for a genuinely imported anchor row
    // (the link is set ON the import, pointing at its ai/manual twin).
    if imported.source != "imported" {
        return;
    }

    match find_duplicate_candidate(pool, budget_id, &imported, DEFAULT_WINDOW_DAYS).await {
        Ok(Some(candidate)) => {
            if let Err(e) = sqlx::query(
                "UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2",
            )
            .bind(candidate)
            .bind(imported_id)
            .execute(pool)
            .await
            {
                tracing::warn!("duplicate matcher: failed to link {imported_id} -> {candidate}: {e}");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("duplicate matcher: candidate query failed for {imported_id}: {e}");
        }
    }
}

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

    // The reverse matcher must only run for ai/manual rows: the link lives on
    // the imported row, pointing at an ai/manual twin.
    if logged.source != "ai" && logged.source != "manual" {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn mk_user(db: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(id)
            .bind(format!("dup-{id}@test.example"))
            .execute(db)
            .await
            .unwrap();
        id
    }

    async fn mk_budget(db: &PgPool, owner: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'B', 'monthly', 0)",
        )
        .bind(id)
        .bind(owner)
        .execute(db)
        .await
        .unwrap();
        id
    }

    async fn mk_category(db: &PgPool, budget: Uuid, ctype: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, budget_id, name, category_type, category_limit) \
             VALUES ($1, $2, $3, $4, 0)",
        )
        .bind(id)
        .bind(budget)
        .bind(format!("{ctype}-{id}"))
        .bind(ctype)
        .execute(db)
        .await
        .unwrap();
        id
    }

    #[allow(clippy::too_many_arguments)]
    async fn mk_tx(
        db: &PgPool,
        budget: Uuid,
        source: &str,
        amount: f64,
        currency: Option<&str>,
        date: DateTime<Utc>,
        category_id: Option<Uuid>,
        description: &str,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions \
                (id, budget_id, category_id, amount, transaction_date, description, currency, source) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(budget)
        .bind(category_id)
        .bind(amount)
        .bind(date)
        .bind(description)
        .bind(currency)
        .bind(source)
        .execute(db)
        .await
        .unwrap();
        id
    }

    async fn cleanup(db: &PgPool, user: Uuid) {
        sqlx::query(
            "DELETE FROM transactions WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)",
        )
        .bind(user)
        .execute(db)
        .await
        .ok();
        sqlx::query(
            "DELETE FROM categories WHERE budget_id IN (SELECT id FROM budgets WHERE owner_id = $1)",
        )
        .bind(user)
        .execute(db)
        .await
        .ok();
        sqlx::query("DELETE FROM budgets WHERE owner_id = $1").bind(user).execute(db).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(db).await.ok();
    }

    async fn load(db: &PgPool, id: Uuid) -> Transaction {
        sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    // --- Task 3.1: the self-FK itself ------------------------------------

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn matched_fk_accepts_valid_rejects_bogus_and_nulls_on_delete() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let nels = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE").await;

        // Valid link is accepted.
        sqlx::query("UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2")
            .bind(nels)
            .bind(imported)
            .execute(&db)
            .await
            .expect("valid FK link accepted");
        assert_eq!(load(&db, imported).await.matched_transaction_id, Some(nels));

        // A non-existent target is rejected by the FK.
        let bogus = Uuid::new_v4();
        let err = sqlx::query("UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2")
            .bind(bogus)
            .bind(imported)
            .execute(&db)
            .await;
        assert!(err.is_err(), "FK rejects a non-existent matched id");

        // Deleting the referenced Nels row nulls the link (ON DELETE SET NULL),
        // it does NOT delete the imported row — reconciliation is non-destructive.
        sqlx::query("DELETE FROM transactions WHERE id = $1").bind(nels).execute(&db).await.unwrap();
        let still = load(&db, imported).await;
        assert_eq!(still.matched_transaction_id, None, "link nulled on referenced-row delete");

        cleanup(&db, user).await;
    }

    // --- Task 3.2: the matcher heuristic ---------------------------------

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn matches_same_magnitude_currency_and_date() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let nels = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now + chrono::Duration::days(2), None, "BLUE BOTTLE").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, imported).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, Some(nels), "USD import matches a NULL-currency Nels row of equal amount within window");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn no_match_on_different_currency() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        // The Nels row is explicitly EUR; a USD import must not match it.
        let _nels = mk_tx(&db, budget, "ai", 15.0, Some("EUR"), now, None, "coffee").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, imported).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, None, "different currency -> no match");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn no_match_outside_window() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let _nels = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now + chrono::Duration::days(10), None, "BLUE BOTTLE").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, imported).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, None, "10 days apart is outside the 4-day window");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn no_match_categorized_refund_vs_charge() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let income = mk_category(&db, budget, "income").await; // a refund lands in income
        let expense = mk_category(&db, budget, "expense").await;
        // Nels row is a categorized income (refund); the import is a categorized expense.
        let _nels = mk_tx(&db, budget, "ai", 15.0, None, now, Some(income), "refund").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, Some(expense), "BLUE BOTTLE").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, imported).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, None, "same magnitude but income vs expense -> no match when both categorized");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn nearest_date_candidate_wins() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let far = mk_tx(&db, budget, "ai", 15.0, None, now - chrono::Duration::days(3), None, "coffee far").await;
        let near = mk_tx(&db, budget, "ai", 15.0, None, now - chrono::Duration::days(1), None, "coffee near").await;
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, imported).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, Some(near), "the nearest-date candidate wins");
        assert_ne!(got, Some(far));
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn already_claimed_nels_row_is_not_rematched() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let nels = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;
        // A first import already claimed the Nels row.
        let first_import = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE 1").await;
        sqlx::query("UPDATE transactions SET matched_transaction_id = $1 WHERE id = $2")
            .bind(nels)
            .bind(first_import)
            .execute(&db)
            .await
            .unwrap();
        // A second same-amount import must NOT re-claim the same Nels row.
        let second_import = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE 2").await;

        let got = find_duplicate_candidate(&db, budget, &load(&db, second_import).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, None, "a Nels row already claimed by an import is not re-matched");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn link_duplicate_for_import_sets_the_link() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let nels = mk_tx(&db, budget, "ai", 42.0, None, now, None, "lunch").await;
        let imported = mk_tx(&db, budget, "imported", 42.0, Some("USD"), now + chrono::Duration::days(1), None, "SWEETGREEN").await;

        link_duplicate_for_import(&db, budget, imported).await;
        assert_eq!(load(&db, imported).await.matched_transaction_id, Some(nels), "wrapper links the imported row to its twin");
        cleanup(&db, user).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn link_skips_an_ignore_rule_excluded_import() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();
        let _nels = mk_tx(&db, budget, "ai", 42.0, None, now, None, "lunch").await;
        // An import auto-excluded by an ignore rule is a non-spending transfer —
        // it must NOT be reconciled (merging it would drop the Nels expense).
        let imported = mk_tx(&db, budget, "imported", 42.0, Some("USD"), now, None, "SWEETGREEN").await;
        sqlx::query("UPDATE transactions SET excluded_from_budget = true WHERE id = $1")
            .bind(imported).execute(&db).await.unwrap();

        link_duplicate_for_import(&db, budget, imported).await;
        assert_eq!(load(&db, imported).await.matched_transaction_id, None, "excluded import is not linked");
        cleanup(&db, user).await;
    }

    // --- Task 4: the reverse (log-time) matcher --------------------------

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

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    #[serial_test::serial]
    async fn reverse_finder_skips_excluded_import() {
        let db = test_pool().await;
        let user = mk_user(&db).await;
        let budget = mk_budget(&db, user).await;
        let now = Utc::now();

        // Import matches on amount/currency/date but was auto-excluded (#374) —
        // it must not be offered as a reconciliation candidate.
        let imported = mk_tx(&db, budget, "imported", 15.0, Some("USD"), now, None, "BLUE BOTTLE #14").await;
        sqlx::query("UPDATE transactions SET excluded_from_budget = true WHERE id = $1")
            .bind(imported).execute(&db).await.unwrap();

        let logged = mk_tx(&db, budget, "ai", 15.0, None, now, None, "coffee").await;

        let got = find_import_duplicate_candidate(&db, budget, &load(&db, logged).await, DEFAULT_WINDOW_DAYS)
            .await
            .unwrap();
        assert_eq!(got, None, "excluded import is not offered");
        cleanup(&db, user).await;
    }

    // --- Task 5: the reverse-matcher wrapper ------------------------------

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
}
