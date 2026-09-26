//! Retirement balance-sheet allocation helpers (#464).
//!
//! # Security property: this data is USER-scoped, not BUDGET-scoped
//!
//! The `assets` table these helpers describe is keyed on `user_id`, with no
//! `budget_id` column at all. That is a deliberate break from the pattern every
//! other financial table in this schema follows (`linked_accounts.budget_id`,
//! `goals.budget_id`, ...), and it is load-bearing rather than incidental.
//!
//! Budgets are shareable, and the per-module `require_edit_or_owner` guards
//! grant a budget's collaborators access to everything beneath that budget --
//! which is the correct meaning of sharing for categories, transactions and
//! goals. Retirement balances are not that kind of data. A budget-scoped assets
//! table would mean that sharing a household budget with a spouse, roommate or
//! financial coach silently hands them the full contents of one person's
//! retirement accounts, and the existing guards would authorize it as correct.
//! Scoping on `user_id` removes the share edge entirely.
//!
//! Practical consequence for anything built on this module: reach these rows by
//! `user_id` only. Never filter on `budget_id`, and never join through
//! `budgets` to find them. See #464 and the access-control decision recorded on
//! the epic #454.
//!
//! # What is in here
//!
//! Pure arithmetic over an asset's holdings: bucket each position into a coarse
//! allocation class and add it up. No database, no HTTP, no clock -- everything
//! here is a total function of its arguments, which is why the whole module is
//! testable without Postgres.

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::internal_error;

// Pure domain rules live in nels-core (core/, spec savvagent/nels-oss#5).
// Re-exported so existing `crate::<module>::…` paths keep resolving.
// Add NEW pure rules to core/, not here.
pub use nels_core::assets::{AssetType, HoldingValue, SecurityType, TaxTreatment};

// =========================================================================
// Persistence
//
// EVERY function below takes `user_id` and filters on it. That is not a
// convenience parameter -- it is the only access control this data has. No
// query here mentions `budget_id`, and none ever should: see the module docs
// above and the migration's `assets` comment for why a budget-scoped query
// would hand a collaborator someone else's retirement accounts. The route is
// `/assets`, deliberately not `/budgets/:id/assets`, for the same reason.
// =========================================================================

/// One row of `assets` -- an account or holding-container on a balance sheet.
#[derive(sqlx::FromRow, Serialize, Debug)]
pub struct Asset {
    pub id: Uuid,
    pub user_id: Uuid,
    pub owner_member_id: Option<Uuid>,
    pub linked_account_id: Option<Uuid>,
    pub name: String,
    pub asset_type: AssetType,
    pub tax_treatment: TaxTreatment,
    pub institution_name: Option<String>,
    /// `None` means NO balance has ever been reported for this asset, which is
    /// a different fact from a reported balance of zero -- exactly the
    /// distinction `balance_as_of` draws for the date half of the same fact.
    /// See [`allocate_reconciled`] for why conflating the two fires a false
    /// stale-balance signal on every newly synced asset.
    pub current_balance: Option<f64>,
    pub currency: String,
    pub balance_as_of: Option<DateTime<Utc>>,
    pub is_manual: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of `asset_holdings`.
///
/// This is the full persisted row, needed verbatim by the GDPR account export.
/// It is deliberately NOT the same type as [`HoldingValue`], which is the
/// trimmed-down projection the allocation maths consumes.
#[derive(sqlx::FromRow, Serialize, Debug)]
pub struct AssetHolding {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub security_id: Uuid,
    pub quantity: f64,
    pub cost_basis: Option<f64>,
    pub market_value: f64,
    pub as_of: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of `asset_balance_history`. Also part of the GDPR account export.
#[derive(sqlx::FromRow, Serialize, Debug)]
pub struct AssetBalanceHistory {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub as_of: NaiveDate,
    pub balance: f64,
    pub created_at: DateTime<Utc>,
}

/// Every asset belonging to `user_id`, oldest first.
pub async fn list_assets_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<Asset>, sqlx::Error> {
    sqlx::query_as::<_, Asset>("SELECT * FROM assets WHERE user_id = $1 ORDER BY created_at")
        .bind(user_id)
        .fetch_all(pool)
        .await
}

/// Insert one asset owned by `user_id`.
///
/// `linked_account_id` must belong to the same user; the composite FK
/// `assets_linked_account_same_user_fkey` rejects the insert otherwise rather
/// than filing one person's holdings under another's id.
#[allow(clippy::too_many_arguments)]
pub async fn insert_asset(
    pool: &PgPool,
    user_id: Uuid,
    owner_member_id: Option<Uuid>,
    name: &str,
    asset_type: AssetType,
    tax_treatment: TaxTreatment,
    linked_account_id: Option<Uuid>,
    current_balance: Option<f64>,
    currency: &str,
) -> Result<Asset, sqlx::Error> {
    sqlx::query_as::<_, Asset>(
        "INSERT INTO assets \
         (id, user_id, owner_member_id, linked_account_id, name, asset_type, \
          tax_treatment, current_balance, currency) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(owner_member_id)
    .bind(linked_account_id)
    .bind(name)
    .bind(asset_type)
    .bind(tax_treatment)
    .bind(current_balance)
    .bind(currency)
    .fetch_one(pool)
    .await
}

/// The outcome of a balance-history write. `#[must_use]` so a caller cannot
/// discard a `NotOwned` rejection the way a bare `u64` allows.
///
/// The type this replaced was `u64` -- the raw `rows_affected()`. `u64` is not
/// `#[must_use]`, so `upsert_balance_history(...).await?;` compiled cleanly
/// while throwing away the one bit that said the write had been REFUSED. On a
/// table whose entire access control is the `user_id` filter, an
/// access-control denial that is shaped exactly like success is the worst
/// available shape.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceWrite {
    /// Inserted, or overwrote that day's existing snapshot.
    Written,
    /// `asset_id` does not belong to `user_id`. Nothing was written. This is an
    /// ACCESS-CONTROL rejection, not a benign no-op -- callers must surface it.
    NotOwned,
}

/// Record `balance` for `asset_id` on `as_of`, overwriting that day's existing
/// snapshot if there is one. Returns [`BalanceWrite::Written`] on success and
/// [`BalanceWrite::NotOwned`] when `asset_id` does not belong to `user_id`.
///
/// The ownership check rides inside the statement as `WHERE EXISTS (... AND
/// user_id = $5)` rather than as a separate SELECT, so there is no window
/// between checking and writing, and a foreign asset id yields `INSERT 0 0` --
/// no row, no error, nothing leaked to the caller about whether that asset
/// exists. The rejection IS logged server-side at WARN, because a client
/// learning nothing and an operator learning nothing are very different
/// requirements: silence toward the caller is the security property, silence in
/// the logs is just a blind spot.
///
/// # A future batch variant needs care
///
/// If this is ever generalized to insert many snapshots from one `SELECT`, that
/// select must yield at most one row per `(asset_id, as_of)` conflict key.
/// PostgreSQL raises "ON CONFLICT DO UPDATE command cannot affect row a second
/// time" if a single statement tries to update the same conflicting row twice.
pub async fn upsert_balance_history<'e, E>(executor: E, user_id: Uuid, asset_id: Uuid, as_of: NaiveDate, balance: f64) -> Result<BalanceWrite, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let result = sqlx::query(
        "INSERT INTO asset_balance_history (id, asset_id, as_of, balance) \
         SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT 1 FROM assets WHERE id = $2 AND user_id = $5) \
         ON CONFLICT (asset_id, as_of) DO UPDATE SET balance = EXCLUDED.balance",
    )
    .bind(Uuid::new_v4())
    .bind(asset_id)
    .bind(as_of)
    .bind(balance)
    .bind(user_id)
    .execute(executor)
    .await?;
    if result.rows_affected() == 0 {
        tracing::warn!(
            %user_id,
            %asset_id,
            %as_of,
            "balance-history write rejected: asset is not owned by this user"
        );
        return Ok(BalanceWrite::NotOwned);
    }
    Ok(BalanceWrite::Written)
}

/// The holdings inside `asset_id`, projected down to what [`allocate`] needs.
///
/// # `None` and `Some(vec![])` are DIFFERENT ANSWERS -- do not collapse them
///
/// - `Ok(None)` -- no asset with that id belongs to this user. Either it does
///   not exist or it is somebody else's; the caller is told which of those it
///   is on purpose (nothing), but it is told clearly that it got NOTHING.
/// - `Ok(Some(vec![]))` -- the caller owns the asset and it genuinely has no
///   holdings recorded. An ordinary state: a manual pension, a house, an
///   account whose holdings have not synced yet.
///
/// The previous signature returned `Vec<HoldingValue>` and used the empty vec
/// for both, which made an access-control denial indistinguishable from a real
/// empty portfolio. Downstream that empty vec becomes an all-zero
/// [`Allocation`], which renders as "$0, no allocation" -- a denied read
/// presented to the user as a fact about their money, and fed to #467 as real
/// data to project from.
///
/// # Why an explicit ownership probe AND a scoped holdings query
///
/// The `SELECT id FROM assets WHERE id = $1 AND user_id = $2` probe is what
/// distinguishes the two cases; it cannot be derived from a row count, since a
/// legitimately empty portfolio and a denied read both return zero rows.
///
/// The holdings query nonetheless KEEPS its `INNER JOIN assets ... AND
/// a.user_id = $1`, which is now redundant with the probe. That is defence in
/// depth and should stay: if the probe is ever moved, refactored into a caller,
/// or short-circuited, the join is what still prevents the query from returning
/// another user's positions. The redundant predicate costs an index lookup the
/// query planner was doing anyway.
///
/// # The join onto `securities` is LEFT, but not for the reason you would guess
///
/// It is NOT how the unknown-to-`Other` fallback is reached. That fallback is
/// unreachable through this join, because the schema forbids the state:
/// `asset_holdings.security_id` is `NOT NULL REFERENCES securities(id) ON
/// DELETE RESTRICT`, and `securities.security_type` is `NOT NULL DEFAULT
/// 'other'`. Every holding therefore has a securities row and that row has a
/// type. The real path to `Other` is `SecurityType::from_db` mapping an
/// unrecognised (CHECK-widened) value, plus the explicit `'other'` value
/// itself.
///
/// The LEFT join earns its place as a different safeguard: if referential
/// integrity were ever violated -- a manual DELETE that bypassed the RESTRICT,
/// a restore from an inconsistent dump -- an INNER join would silently DROP
/// that holding from the allocation and understate the portfolio, whereas the
/// LEFT join keeps its market value and books it as unclassified. Undercounting
/// someone's retirement balance without saying so is the worse failure.
pub async fn holdings_for_asset(
    pool: &PgPool,
    user_id: Uuid,
    asset_id: Uuid,
) -> Result<Option<Vec<HoldingValue>>, sqlx::Error> {
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM assets WHERE id = $1 AND user_id = $2")
            .bind(asset_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    if owned.is_none() {
        tracing::warn!(
            %user_id,
            %asset_id,
            "holdings read rejected: asset is not owned by this user"
        );
        return Ok(None);
    }

    let rows: Vec<(Option<String>, f64)> = sqlx::query_as(
        "SELECT s.security_type, ah.market_value \
         FROM asset_holdings ah \
         INNER JOIN assets a ON a.id = ah.asset_id AND a.user_id = $1 \
         LEFT JOIN securities s ON s.id = ah.security_id \
         WHERE ah.asset_id = $2",
    )
    .bind(user_id)
    .bind(asset_id)
    .fetch_all(pool)
    .await?;

    Ok(Some(
        rows.into_iter()
            .map(|(security_type, market_value)| HoldingValue {
                security_type: match security_type {
                    Some(raw) => SecurityType::from_db(&raw),
                    // Schema-unreachable; see the LEFT join note above. Booked
                    // as unclassified rather than dropped.
                    None => {
                        tracing::warn!(
                            %asset_id,
                            "holding has no securities row despite a NOT NULL \
                             RESTRICT foreign key; classifying as Other"
                        );
                        SecurityType::Other
                    }
                },
                market_value,
            })
            .collect(),
    ))
}

/// `GET /api/assets` -- the authenticated caller's own assets.
///
/// Gated on `require_caller_tier`, the CALLER's own subscription, not
/// `require_tier`, which resolves a BUDGET OWNER's subscription. There is no
/// budget in scope here by design, and there must not be one.
pub async fn list_assets(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<Asset>>, (StatusCode, String)> {
    crate::access::require_caller_tier(&state.db, user_id, crate::entitlement::Tier::Pro).await?;
    // No empty-list fallback on error: a failed query must surface as a 500,
    // never as "you own no retirement assets".
    //
    // ONE error event, not two. This previously logged `tracing::error!(%user_id,
    // ...)` and then handed the error to `internal_error`, which logs `error =
    // %err` independently -- producing two uncorrelated ERROR lines, the first
    // carrying the user id but not the error and the second the error but not
    // the user id, with nothing tying them together. Folding the user id into
    // the message `internal_error` already logs gives one line with both.
    let assets = list_assets_for_user(&state.db, user_id)
        .await
        .map_err(|e| internal_error(format!("list_assets for user {user_id}: {e}")))?;
    Ok(Json(assets))
}

// =========================================================================
// Vendor sync support (nels#468)
//
// The four writers below exist for the Plaid Investments sync path
// (`plaid_investments::sync_investment_holdings`) and are the first real
// writers to `securities`/`asset_holdings`. They take `user_id` and filter on
// it exactly like every other query in this module: `user_id` is the only
// access control these tables have.
// =========================================================================

/// A single position to write into `asset_holdings` for one asset.
///
/// This is the WRITE-side input type, the mirror of the read-side
/// [`HoldingValue`]. It carries the DB `security_id` (already resolved through
/// the `securities` cache) rather than the vendor's id, because the caller has
/// already upserted securities and built the id mapping.
#[derive(Debug, Clone)]
pub struct SecurityHolding {
    pub security_id: Uuid,
    pub quantity: f64,
    pub market_value: f64,
    pub as_of: DateTime<Utc>,
}

/// Upsert one instrument into the shared `securities` cache, returning its DB id.
///
/// `securities` is a global, non-personal metadata cache keyed on
/// `(provider, provider_security_id)` (§20) — "VTSAX is a mutual fund" is the
/// same fact for every user, so there are no per-user rows and no `user_id`
/// parameter. `provider` is pinned to `'plaid'` — the only holdings vendor
/// wired up — and the column CHECK is what forces a widening when a second
/// vendor lands. `ticker`/`name`/`currency` are nullable because vendors
/// legitimately omit them; `security_type` is written through
/// [`SecurityType::db_literal`], the strict write-side mirror of the lenient
/// `from_db` read.
///
/// `updated_at` is set in application code: there is no trigger on any of
/// these tables, so without the explicit `updated_at = now()` an upserted row
/// would keep its creation timestamp forever (the §20 "set `updated_at` in
/// application code" obligation, load-bearing now that a real writer exists).
pub async fn upsert_security(
    pool: &PgPool,
    provider_security_id: &str,
    ticker: Option<&str>,
    name: Option<&str>,
    security_type: SecurityType,
    currency: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO securities (id, provider, provider_security_id, ticker, name, \
                                 security_type, currency) \
         VALUES ($1, 'plaid', $2, $3, $4, $5, $6) \
         ON CONFLICT (provider, provider_security_id) DO UPDATE \
           SET ticker = COALESCE(EXCLUDED.ticker, securities.ticker), \
               name = COALESCE(EXCLUDED.name, securities.name), \
               security_type = EXCLUDED.security_type, \
               currency = COALESCE(EXCLUDED.currency, securities.currency), \
               updated_at = now() \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(provider_security_id)
    .bind(ticker)
    .bind(name)
    .bind(security_type.db_literal())
    .bind(currency)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Replace every holding of `asset_id` belonging to `user_id` with `holdings`.
///
/// This is a REPLACE, not an accumulate: the vendor's holdings snapshot is the
/// whole truth for the account, so a position that disappeared from the sync
/// must disappear from the table rather than linger. The whole write runs in
/// one transaction, so a failure leaves the previous snapshot intact.
///
/// Returns `Ok(None)` — never an empty `Ok(Some(vec![]))` — when `asset_id`
/// does not belong to `user_id`. The denial is logged at WARN server-side and
/// is surfaced to the caller as a loud failure (the caller's
/// `plaid_investments::sync_investment_holdings` turns it into a 403), because
/// folding it into "no holdings" would render a denied read as a fact about the
/// user's money. This is the same contract `holdings_for_asset`'s `None` half
/// (§20) established on the read side.
pub async fn replace_holdings_for_asset(
    pool: &PgPool,
    user_id: Uuid,
    asset_id: Uuid,
    holdings: &[SecurityHolding],
) -> Result<Option<()>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Ownership probe, same shape as `holdings_for_asset`'s: the empty `holdings`
    // case (a real but position-less account) and the denial case are
    // distinguished by existence of the owned row, not by row count.
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM assets WHERE id = $1 AND user_id = $2")
        .bind(asset_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(_owned_id) = owned else {
        tracing::warn!(
            %user_id,
            %asset_id,
            "holdings write rejected: asset is not owned by this user"
        );
        return Ok(None);
    };

    sqlx::query("DELETE FROM asset_holdings WHERE asset_id = $1")
        .bind(asset_id)
        .execute(&mut *tx)
        .await?;

    for h in holdings {
        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, \
                                         market_value, as_of) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(asset_id)
        .bind(h.security_id)
        .bind(h.quantity)
        .bind(h.market_value)
        .bind(h.as_of)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Some(()))
}

/// Update `assets.current_balance` for `asset_id` (if owned by `user_id`) and
/// record a same-day `asset_balance_history` snapshot.
///
/// `balance: Option<f64>` follows the §20 "absent is not zero" rule: `None`
/// means "no balance was reported this sync" and skips both writes entirely
/// (a freshly synced asset must read as "never reported", not as 0 — the false
/// stale-balance signal `allocate_reconciled` fires on a 0). The `as_of` date
/// becomes both `assets.balance_as_of` and the history snapshot's date, so the
/// balance and its date stay one fact.
///
/// The history snapshot uses [`upsert_balance_history`] so a same-day
/// re-sync overwrites that day's point rather than appending a duplicate — the
/// same `(asset_id, as_of)` conflict key the table's unique index enforces.
pub async fn update_reported_balance(
    pool: &PgPool,
    user_id: Uuid,
    asset_id: Uuid,
    balance: Option<f64>,
    as_of: NaiveDate,
) -> Result<(), sqlx::Error> {
    let Some(balance) = balance else {
        return Ok(());
    };
    let mut tx = pool.begin().await?;
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM assets WHERE id = $1 AND user_id = $2")
        .bind(asset_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(_owned_id) = owned else {
        tracing::warn!(
            %user_id,
            %asset_id,
            "balance write rejected: asset is not owned by this user"
        );
        return Ok(());
    };
    sqlx::query(
        "UPDATE assets SET current_balance = $1, balance_as_of = $2, updated_at = now() \
         WHERE id = $3 AND user_id = $4",
    )
    .bind(balance)
    .bind(as_of.and_hms_opt(0, 0, 0).map(|d| d.and_utc()))
    .bind(asset_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    // Same-day snapshot; conflict overwrites (one point per day per asset).
    let write = upsert_balance_history(&mut *tx, user_id, asset_id, as_of, balance).await?;
    match write {
        BalanceWrite::Written => {}
        // The ownership probe above already refused; this can only fire on a
        // race, and a race that drops the history snapshot is benign compared
        // to the alternative (writing another user's history).
        BalanceWrite::NotOwned => {
            tracing::warn!(%user_id, %asset_id, "balance-history write lost ownership race");
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Float tolerance. Every assertion below compares sums of f64s, so exact
    /// equality would be a flaky test, not a stricter one.
    const EPS: f64 = 1e-9;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // =====================================================================
    // DB-backed tests (#[ignore]d; require the local Postgres).
    //
    // These cover the security property the whole issue exists for: assets
    // are reachable by `user_id` and by nothing else. A budget collaborator
    // -- who passes every `require_edit_or_owner` guard in the codebase --
    // must still see none of the budget owner's assets.
    // =====================================================================

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{middleware, Extension, Router};
    use chrono::NaiveDate;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;
    use tower::ServiceExt; // for `oneshot`
    use uuid::Uuid;

    /// Connects AND runs the migrations, mirroring `access.rs`'s `test_pool`.
    /// Migrating is required, not optional: `budget.rs`'s rollup helper does
    /// not migrate, and these tests would fail with `relation "assets" does
    /// not exist` on a database that has not seen 20260728163000 yet.
    async fn assets_test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".into()
        });
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn test_state(db: PgPool) -> crate::auth::AppState {
        crate::auth::AppState {
            db,
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[7u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    async fn mk_user(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(id)
            .bind(format!("assets-{id}@example.test"))
            .execute(pool)
            .await
            .unwrap();
        id
    }

    /// Grant `user_id` Pro via a `trialing` subscription with NO `price_id`.
    ///
    /// `trialing` -- not `active` -- on purpose. `entitlement::resolve`
    /// returns `Tier::Pro` for `trialing` and returns it BEFORE the price
    /// catalog is consulted at all. With `active` the tier would come from
    /// `price_catalog_from_env()`, where an absent/unrecognized `price_id`
    /// fails safe to `Tier::Basic` -- below Pro -- and the handler would 402.
    /// Short-circuiting ahead of the catalog also means a concurrent
    /// `STRIPE_PRICE_*` env mutation in another test cannot change our
    /// outcome, so these tests need no `#[serial]`.
    ///
    /// `stripe_customer_id` is per-user because the column is NOT NULL and
    /// uniquely indexed; a constant would fail with 23505 on the second Pro
    /// user, and these tests grant Pro to two of them.
    async fn mk_pro(pool: &PgPool, user_id: Uuid) {
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) \
             VALUES ($1, $2, 'trialing')",
        )
        .bind(user_id)
        .bind(format!("cus_test_{user_id}"))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn mk_budget(pool: &PgPool, owner_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit) \
             VALUES ($1, $2, 'B', 'monthly', 0)",
        )
        .bind(id)
        .bind(owner_id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// `budget_shares` is EMAIL-keyed, and its `id` has no default -- pass the
    /// collaborator's email, never their user id, and supply the id yourself.
    async fn mk_share(pool: &PgPool, budget_id: Uuid, email: &str, level: &str) {
        sqlx::query(
            "INSERT INTO budget_shares (id, budget_id, shared_with_email, permission_level) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(budget_id)
        .bind(email)
        .bind(level)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn email_of(pool: &PgPool, user_id: Uuid) -> String {
        sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn mk_linked_account(pool: &PgPool, budget_id: Uuid, user_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts \
             (id, budget_id, user_id, provider, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, 'plaid', $4, 'ref')",
        )
        .bind(id)
        .bind(budget_id)
        .bind(user_id)
        .bind(format!("acct-{id}"))
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// `provider_security_id` is derived from a fresh UUID so the
    /// `(provider, provider_security_id)` unique index does not fail with
    /// 23505 on the second run of the suite.
    async fn mk_security(pool: &PgPool, security_type: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO securities (id, provider, provider_security_id, security_type) \
             VALUES ($1, 'plaid', $2, $3)",
        )
        .bind(id)
        .bind(format!("sec-{id}"))
        .bind(security_type)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn mk_holding(pool: &PgPool, asset_id: Uuid, security_id: Uuid, market_value: f64) {
        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value) \
             VALUES ($1, $2, $3, 1, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(asset_id)
        .bind(security_id)
        .bind(market_value)
        .execute(pool)
        .await
        .unwrap();
    }

    /// FK-safe teardown. Assets go first because deleting them cascades their
    /// holdings and balance history; only then can `securities` be removed,
    /// since holdings reference it `ON DELETE RESTRICT` and `securities` has
    /// no `user_id`, so deleting the users would never reach it.
    async fn cleanup(pool: &PgPool, users: &[Uuid], securities: &[Uuid]) {
        // Assets first: this cascades their holdings and balance history, which
        // is what frees `securities` from the RESTRICT below.
        sqlx::query("DELETE FROM assets WHERE user_id = ANY($1)")
            .bind(users)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM securities WHERE id = ANY($1)")
            .bind(securities)
            .execute(pool)
            .await
            .unwrap();
        for sql in [
            "DELETE FROM linked_accounts WHERE user_id = ANY($1)",
            "DELETE FROM budget_shares WHERE budget_id IN \
             (SELECT id FROM budgets WHERE owner_id = ANY($1))",
            "DELETE FROM budgets WHERE owner_id = ANY($1)",
            "DELETE FROM subscriptions WHERE user_id = ANY($1)",
            "DELETE FROM users WHERE id = ANY($1)",
        ] {
            sqlx::query(sql).bind(users).execute(pool).await.unwrap();
        }
    }

    fn sorted_ids(assets: &[Asset]) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = assets.iter().map(|a| a.id).collect();
        ids.sort();
        ids
    }

    /// The world the two collaborator isolation tests both need: an owner who
    /// holds two assets, and a collaborator who holds `edit` on the owner's
    /// budget and one asset of their own.
    ///
    /// Both tests previously built this line for line. They are kept as two
    /// distinct tests -- one calls the handler function directly, the other
    /// drives a real axum service -- but there is only one fixture now, so the
    /// seeding can only drift from itself in one place.
    struct CollabScenario {
        pool: PgPool,
        owner: Uuid,
        collaborator: Uuid,
        owner_a: Uuid,
        owner_b: Uuid,
        collab_a: Uuid,
    }

    impl CollabScenario {
        async fn cleanup(&self) {
            cleanup(&self.pool, &[self.owner, self.collaborator], &[]).await;
        }
    }

    async fn seed_collab_scenario() -> CollabScenario {
        let pool = assets_test_pool().await;
        let owner = mk_user(&pool).await;
        let collaborator = mk_user(&pool).await;
        mk_pro(&pool, owner).await;
        mk_pro(&pool, collaborator).await;

        let budget = mk_budget(&pool, owner).await;
        mk_share(&pool, budget, &email_of(&pool, collaborator).await, "edit").await;

        // Fixture sanity FIRST: if the share were dead, every isolation
        // assertion built on this scenario would pass for entirely the wrong
        // reason -- the collaborator would be an unrelated stranger rather than
        // somebody the existing budget guards actively authorize.
        assert_eq!(
            crate::budget::check_permission(&pool, collaborator, budget)
                .await
                .unwrap(),
            crate::budget::Permission::Edit,
            "fixture: collaborator must really hold Edit on the owner's budget"
        );

        let linked = mk_linked_account(&pool, budget, owner).await;
        let owner_a = insert_asset(
            &pool, owner, None, "401k", AssetType::RetirementAccount, TaxTreatment::PreTax,
            Some(linked), Some(1000.0), "USD",
        )
        .await
        .unwrap();
        let owner_b = insert_asset(
            &pool, owner, None, "IRA", AssetType::RetirementAccount, TaxTreatment::Roth,
            None, Some(2000.0), "USD",
        )
        .await
        .unwrap();
        // The collaborator owns an asset too, so an always-empty result cannot
        // masquerade as correct scoping.
        let collab_a = insert_asset(
            &pool, collaborator, None, "Brokerage", AssetType::Brokerage, TaxTreatment::Taxable,
            None, Some(300.0), "USD",
        )
        .await
        .unwrap();

        CollabScenario {
            pool,
            owner,
            collaborator,
            owner_a: owner_a.id,
            owner_b: owner_b.id,
            collab_a: collab_a.id,
        }
    }

    /// THE headline test for #464: a budget collaborator with `edit`
    /// permission on the owner's budget must see none of the owner's assets,
    /// while still seeing their own.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn collaborator_cannot_read_owner_assets() {
        let s = seed_collab_scenario().await;
        let state = test_state(s.pool.clone());

        let seen = list_assets(State(state.clone()), Extension(s.collaborator))
            .await
            .expect("collaborator is Pro, so the read succeeds")
            .0;
        assert_eq!(sorted_ids(&seen), vec![s.collab_a]);
        assert!(!seen.iter().any(|a| a.id == s.owner_a), "leaked owner asset A");
        assert!(!seen.iter().any(|a| a.id == s.owner_b), "leaked owner asset B");

        let mine = list_assets(State(state), Extension(s.owner)).await.unwrap().0;
        let mut expected = vec![s.owner_a, s.owner_b];
        expected.sort();
        assert_eq!(sorted_ids(&mine), expected);

        s.cleanup().await;
    }

    /// `list_assets_for_user` promises "oldest first", and AGENTS.md documents
    /// that ordering as part of the `GET /api/assets` contract -- but every
    /// other assertion in this module runs the rows through `sorted_ids`, which
    /// sorts by UUID and throws the ordering away. Delete the `ORDER BY
    /// created_at` and nothing else here would notice.
    ///
    /// Two halves, because each alone is weak:
    ///
    /// 1. INSERTION ORDER. Each `insert_asset` is its own statement, hence its
    ///    own transaction, so `now()` -- which is transaction-start time, not
    ///    statement time -- genuinely differs between them and no tie is
    ///    possible. Rows inserted a, b, c must come back a, b, c.
    /// 2. EXPLICIT `created_at`, deliberately set to the REVERSE of insertion
    ///    order. This is the half that actually pins the `ORDER BY`: with the
    ///    clause removed, a small unindexed table returns heap order, which is
    ///    insertion order -- so half 1 would still pass and half 2 would fail.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_assets_for_user_orders_by_created_at() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;

        let mut inserted = Vec::new();
        for name in ["first", "second", "third"] {
            let a = insert_asset(
                &pool, user, None, name, AssetType::Brokerage, TaxTreatment::Taxable,
                None, None, "USD",
            )
            .await
            .unwrap();
            inserted.push(a.id);
        }

        let ids: Vec<Uuid> = list_assets_for_user(&pool, user)
            .await
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, inserted, "assets must come back oldest first");

        // Now make `created_at` disagree with insertion order, so the ordering
        // can only come from the column.
        for (i, id) in inserted.iter().enumerate() {
            sqlx::query("UPDATE assets SET created_at = now() - ($1 || ' days')::interval WHERE id = $2")
                .bind(i.to_string())
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let mut reversed = inserted.clone();
        reversed.reverse();

        let ids: Vec<Uuid> = list_assets_for_user(&pool, user)
            .await
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(
            ids, reversed,
            "the order must follow created_at, not insertion order"
        );

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_assets_requires_pro() {
        // No `subscriptions` row at all -> `resolve(None, None, ..)` is
        // `Tier::None` regardless of any price env vars, so this is hermetic.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let err = list_assets(State(test_state(pool.clone())), Extension(user))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn balance_history_upsert_is_idempotent() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let asset = insert_asset(
            &pool, user, None, "IRA", AssetType::RetirementAccount, TaxTreatment::Roth,
            None, None, "USD",
        )
        .await
        .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 7, 28).unwrap();

        assert_eq!(
            upsert_balance_history(&pool, user, asset.id, day, 1000.0).await.unwrap(),
            BalanceWrite::Written
        );
        assert_eq!(
            upsert_balance_history(&pool, user, asset.id, day, 1000.0).await.unwrap(),
            BalanceWrite::Written
        );
        assert_eq!(
            upsert_balance_history(&pool, user, asset.id, day, 1500.0).await.unwrap(),
            BalanceWrite::Written
        );

        let rows: Vec<(NaiveDate, f64)> = sqlx::query_as(
            "SELECT as_of, balance FROM asset_balance_history WHERE asset_id = $1 ORDER BY as_of",
        )
        .bind(asset.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "one snapshot per asset per day");
        assert!(close(rows[0].1, 1500.0), "{rows:?}");

        // A SECOND DAY. This is the half that pins the conflict key as
        // `(asset_id, as_of)` rather than merely "unique on something".
        //
        // Everything above filters on `asset_id` alone and writes a single
        // date, so it passes byte-identically against a unique index on
        // `(asset_id)` -- under which the third write would OVERWRITE the
        // second day instead of appending it, and the whole net-worth-over-time
        // series would collapse to a single point per asset. That series is
        // #467's projection input, so the collapse would surface as a silently
        // flat retirement projection, not as an error.
        let next_day = NaiveDate::from_ymd_opt(2026, 7, 29).unwrap();
        assert_eq!(
            upsert_balance_history(&pool, user, asset.id, next_day, 1600.0).await.unwrap(),
            BalanceWrite::Written
        );

        let rows: Vec<(NaiveDate, f64)> = sqlx::query_as(
            "SELECT as_of, balance FROM asset_balance_history WHERE asset_id = $1 ORDER BY as_of",
        )
        .bind(asset.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "a different day is a NEW point, not an overwrite: {rows:?}"
        );
        assert_eq!(rows[0].0, day, "{rows:?}");
        assert!(close(rows[0].1, 1500.0), "{rows:?}");
        assert_eq!(rows[1].0, next_day, "{rows:?}");
        assert!(close(rows[1].1, 1600.0), "{rows:?}");

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn balance_history_upsert_rejects_foreign_asset() {
        let pool = assets_test_pool().await;
        let owner = mk_user(&pool).await;
        let collaborator = mk_user(&pool).await;
        let asset = insert_asset(
            &pool, owner, None, "401k", AssetType::RetirementAccount, TaxTreatment::PreTax,
            None, None, "USD",
        )
        .await
        .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 7, 28).unwrap();

        let outcome = upsert_balance_history(&pool, collaborator, asset.id, day, 999.0)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            BalanceWrite::NotOwned,
            "a foreign user_id must be REJECTED, and say so -- not return an \
             Ok that is indistinguishable from a successful write"
        );

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM asset_balance_history WHERE asset_id = $1")
                .bind(asset.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);

        cleanup(&pool, &[owner, collaborator], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn holdings_for_asset_is_user_scoped() {
        let pool = assets_test_pool().await;
        let owner = mk_user(&pool).await;
        let collaborator = mk_user(&pool).await;
        let asset = insert_asset(
            &pool, owner, None, "401k", AssetType::RetirementAccount, TaxTreatment::PreTax,
            None, None, "USD",
        )
        .await
        .unwrap();
        let security = mk_security(&pool, "equity").await;
        mk_holding(&pool, asset.id, security, 42.0).await;

        // Positive half FIRST: without it this test would pass against an
        // empty table and prove nothing about scoping.
        let mine = holdings_for_asset(&pool, owner, asset.id)
            .await
            .unwrap()
            .expect("the owner owns this asset, so the read is permitted");
        assert_eq!(mine.len(), 1, "{mine:?}");
        assert_eq!(mine[0].security_type, SecurityType::Equity);
        assert!(close(mine[0].market_value, 42.0), "{mine:?}");

        // A DENIAL is `None`, not an empty vec. The distinction is the point:
        // an empty vec would render downstream as an all-zero allocation --
        // "$0, no allocation" -- presenting a refused read to the user as a
        // fact about their money and feeding #467 a zero to project from.
        let theirs = holdings_for_asset(&pool, collaborator, asset.id).await.unwrap();
        assert_eq!(
            theirs, None,
            "a foreign user_id must get None, never Some(vec![]): {theirs:?}"
        );

        // An asset id that exists for nobody is also `None`, so the caller
        // learns nothing about whether it exists.
        let missing = holdings_for_asset(&pool, owner, Uuid::new_v4()).await.unwrap();
        assert_eq!(missing, None, "an unknown asset id must also be None");

        // And the OTHER side of the contract: a genuinely empty portfolio the
        // caller DOES own is `Some(vec![])`, distinguishable from the denial.
        let empty_asset = insert_asset(
            &pool, owner, None, "Pension", AssetType::Pension, TaxTreatment::Other,
            None, None, "USD",
        )
        .await
        .unwrap();
        let empty = holdings_for_asset(&pool, owner, empty_asset.id).await.unwrap();
        assert_eq!(
            empty,
            Some(Vec::new()),
            "an owned asset with no holdings is Some(vec![]), not None"
        );

        cleanup(&pool, &[owner, collaborator], &[security]).await;
    }

    /// The isolation assertion again, this time driven through a real axum
    /// service instead of by calling the handler function directly.
    ///
    /// # What this does and does NOT prove
    ///
    /// It does NOT exercise the router the binary actually serves.
    /// `protected_routes` is a local `let` binding inside `#[tokio::main] async
    /// fn main()`, not an item, so nothing in an inline `#[cfg(test)]` module
    /// can reach it; this test necessarily re-declares the `/assets` path
    /// literal, and a typo in `main.rs`'s copy of that string would not fail
    /// here. Read the path below as a fixture, not as an assertion about the
    /// shipped route table.
    ///
    /// What it genuinely covers is everything between the socket and the query:
    /// that `list_assets` is a valid axum `Handler` at all, that its
    /// `State<AppState>` and `Extension<Uuid>` extractors resolve against a
    /// request carrying only an injected user id (a missing `Extension` is a
    /// 500 at runtime, not a compile error), that the entitlement check passes
    /// for a Pro caller, and that the `Vec<Asset>` serializes to a 200 with the
    /// right rows in the body. That is the same thing `admin.rs:198` and
    /// `usage.rs:257` establish with the same `oneshot` + `from_fn` harness,
    /// and this follows their precedent rather than inventing a new one.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn assets_route_isolates_collaborator_via_router() {
        // Same fixture as `collaborator_cannot_read_owner_assets` -- including
        // its `check_permission == Edit` pre-assertion -- and a different act.
        let s = seed_collab_scenario().await;
        let pool = s.pool.clone();

        let state = crate::auth::AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[9u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };

        // The `from_fn` layer stands in for `auth_middleware`, which is the only
        // thing that ever populates the `Extension<Uuid>` the handler reads.
        let caller = s.collaborator;
        let app = Router::new()
            .route("/assets", get(list_assets))
            .layer(middleware::from_fn(
                move |mut req: axum::http::Request<Body>, next: axum::middleware::Next| async move {
                    req.extensions_mut().insert(caller);
                    next.run(req).await
                },
            ))
            .with_state(state);

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/assets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "Pro caller gets a 200");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let seen: Vec<serde_json::Value> = serde_json::from_slice(&body).expect("parse json");
        let ids: Vec<String> = seen
            .iter()
            .map(|a| a["id"].as_str().expect("asset id is a string").to_string())
            .collect();

        assert_eq!(
            ids,
            vec![s.collab_a.to_string()],
            "the collaborator sees exactly their own asset"
        );
        assert!(
            !ids.contains(&s.owner_a.to_string()),
            "leaked owner asset A through the route: {ids:?}"
        );
        assert!(
            !ids.contains(&s.owner_b.to_string()),
            "leaked owner asset B through the route: {ids:?}"
        );

        // The WIRE SHAPE, pinned end to end: through sqlx's TEXT decode into
        // `AssetType`/`TaxTreatment` and back out through serde. These were
        // bare `String`s and the typed versions must serialize identically, or
        // every existing consumer of `GET /api/assets` breaks silently.
        let row = &seen[0];
        assert_eq!(
            row["asset_type"], "brokerage",
            "asset_type must stay a bare snake_case string: {row}"
        );
        assert_eq!(
            row["tax_treatment"], "taxable",
            "tax_treatment must stay a bare snake_case string: {row}"
        );
        assert_eq!(
            row["current_balance"], 300.0,
            "a reported balance still serializes as a plain number: {row}"
        );

        s.cleanup().await;
    }

    /// Pins the issue's acceptance criterion "`assets` has no `budget_id`
    /// column" at the SCHEMA level, where it cannot be argued with.
    ///
    /// Every other test in this module asserts the CONSEQUENCE of user-scoping
    /// (a collaborator reads nothing of the owner's). That is the property that
    /// matters, but it stays true for a while even if someone adds a
    /// `budget_id` column back "for convenience" and leaves the queries alone
    /// -- and once the column exists, the next join through it looks natural
    /// and reintroduces the share edge the whole design removes. This test
    /// fails the moment the column reappears, before any query can start using
    /// it.
    ///
    /// It also asserts the positive half: `user_id` is present and NOT NULL. A
    /// nullable `user_id` would make an unowned asset representable, and
    /// `WHERE user_id = $1` would silently never match it -- a row nobody can
    /// read, delete or export.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn assets_table_has_no_budget_id_column() {
        let pool = assets_test_pool().await;

        let columns: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name::text, is_nullable::text \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'assets'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(
            !columns.is_empty(),
            "fixture: the assets table must exist for this assertion to mean anything"
        );
        assert!(
            !columns.iter().any(|(name, _)| name == "budget_id"),
            "assets must have NO budget_id column: {columns:?}"
        );

        let (_, nullable) = columns
            .iter()
            .find(|(name, _)| name == "user_id")
            .expect("assets must have a user_id column");
        assert_eq!(nullable, "NO", "assets.user_id must be NOT NULL");
    }

    /// Pins the composite FK `assets_linked_account_same_user_fkey` in code.
    ///
    /// # What it prevents
    ///
    /// Every existing sync path in this codebase derives ownership from a
    /// linked account's BUDGET. If #468's holdings sync reaches for that same
    /// familiar pattern, then on a household budget shared by two people it
    /// would resolve an asset's owner through the budget and file one member's
    /// retirement holdings under the OTHER member's `user_id`. Nothing
    /// downstream would notice: `WHERE user_id = $1` would then serve the wrong
    /// person's retirement account to the wrong person, faithfully and
    /// silently, and the user-scoping this whole module exists for would be
    /// undone from the inside. The FK turns that into a failed INSERT.
    ///
    /// The same-user half of the assertion is not decoration: without it a
    /// constraint that rejected EVERY insert with a linked account would also
    /// pass the negative half, and the feature would be broken rather than
    /// safe.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn asset_cannot_link_another_users_linked_account() {
        let pool = assets_test_pool().await;
        let a = mk_user(&pool).await;
        let b = mk_user(&pool).await;

        // The budget and the linked account both belong to B.
        let budget = mk_budget(&pool, b).await;
        let linked = mk_linked_account(&pool, budget, b).await;

        let err = insert_asset(
            &pool, a, None, "Not mine", AssetType::RetirementAccount, TaxTreatment::PreTax,
            Some(linked), Some(1000.0), "USD",
        )
        .await
        .expect_err("user A must not be able to claim user B's linked account");
        let db_err = err
            .as_database_error()
            .unwrap_or_else(|| panic!("expected a database error, got: {err:?}"));
        assert_eq!(
            db_err.code().as_deref(),
            Some("23503"),
            "expected foreign_key_violation, got: {db_err:?}"
        );

        // The same insert for the account's real owner must SUCCEED.
        let ok = insert_asset(
            &pool, b, None, "Mine", AssetType::RetirementAccount, TaxTreatment::PreTax,
            Some(linked), Some(1000.0), "USD",
        )
        .await
        .expect("B owns the linked account, so B may reference it");
        assert_eq!(ok.linked_account_id, Some(linked));
        assert_eq!(ok.user_id, b);

        cleanup(&pool, &[a, b], &[]).await;
    }

    /// Executes `ON DELETE SET NULL (linked_account_id)` -- the PG15+
    /// COLUMN-LIST form, and the single most fragile line in the migration.
    ///
    /// Its whole justification is that plain `ON DELETE SET NULL` nulls EVERY
    /// column in the FK's local list, which here includes the `NOT NULL`
    /// `user_id`: an unrelated `DELETE FROM linked_accounts` would then abort
    /// with a not-null violation instead of orphaning the asset cleanly.
    /// Nothing executed that delete before this test, and `cleanup` removes
    /// assets BEFORE linked accounts, so the suite never reached the clause
    /// even incidentally.
    ///
    /// Both surviving columns are asserted: `linked_account_id` really did go
    /// NULL, and `user_id` really did NOT.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn deleting_a_linked_account_nulls_only_the_link() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let budget = mk_budget(&pool, user).await;
        let linked = mk_linked_account(&pool, budget, user).await;

        let asset = insert_asset(
            &pool, user, None, "Synced 401k", AssetType::RetirementAccount, TaxTreatment::PreTax,
            Some(linked), Some(1000.0), "USD",
        )
        .await
        .unwrap();
        assert_eq!(asset.linked_account_id, Some(linked), "fixture: really linked");
        assert!(!asset.is_manual, "fixture: a SYNCED asset, not a manual one");

        // The delete itself must SUCCEED. Under plain `ON DELETE SET NULL` this
        // is the statement that would fail with 23502.
        sqlx::query("DELETE FROM linked_accounts WHERE id = $1")
            .bind(linked)
            .execute(&pool)
            .await
            .expect("deleting a linked account must not be blocked by assets");

        let row: Option<(Option<Uuid>, Uuid, bool)> =
            sqlx::query_as("SELECT linked_account_id, user_id, is_manual FROM assets WHERE id = $1")
                .bind(asset.id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        let (linked_account_id, user_id, is_manual) =
            row.expect("the asset must SURVIVE its linked account, not cascade away with it");
        assert_eq!(linked_account_id, None, "linked_account_id must be nulled");
        assert_eq!(user_id, user, "user_id must be left intact");
        // The one-directional CHECK exists precisely so this state is legal.
        assert!(
            !is_manual,
            "a synced asset stays synced; `is_manual = FALSE` with a NULL link \
             is a legitimate state and must not have been rewritten"
        );

        cleanup(&pool, &[user], &[]).await;
    }

    /// `asset_holdings.security_id` is the ONLY `ON DELETE RESTRICT` in the
    /// schema. `securities` is a shared, global metadata cache that nothing in
    /// the product deletes from; if some future path tries, RESTRICT must
    /// surface it as a failed DELETE rather than either shredding every user's
    /// holdings (CASCADE) or leaving unattributable ones behind (SET NULL).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn securities_in_use_cannot_be_deleted() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let asset = insert_asset(
            &pool, user, None, "Brokerage", AssetType::Brokerage, TaxTreatment::Taxable,
            None, None, "USD",
        )
        .await
        .unwrap();
        let security = mk_security(&pool, "equity").await;
        mk_holding(&pool, asset.id, security, 100.0).await;

        let err = sqlx::query("DELETE FROM securities WHERE id = $1")
            .bind(security)
            .execute(&pool)
            .await
            .expect_err("a security with holdings against it must not be deletable");
        let db_err = err
            .as_database_error()
            .unwrap_or_else(|| panic!("expected a database error, got: {err:?}"));
        assert_eq!(
            db_err.code().as_deref(),
            Some("23503"),
            "expected foreign_key_violation from ON DELETE RESTRICT, got: {db_err:?}"
        );

        // The positive half: once the holding is gone the row IS deletable, so
        // the assertion above pins RESTRICT rather than some blanket refusal.
        sqlx::query("DELETE FROM asset_holdings WHERE asset_id = $1")
            .bind(asset.id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM securities WHERE id = $1")
            .bind(security)
            .execute(&pool)
            .await
            .expect("an unreferenced security deletes fine");

        cleanup(&pool, &[user], &[]).await;
    }

    /// `asset_holdings_asset_security_idx` is the holdings-sync idempotency key
    /// -- the direct sibling of the `(asset_id, as_of)` key the acceptance
    /// criteria name. Without it, a re-sync appends a second copy of a position
    /// the user already holds and the allocation double-counts it: a wrong
    /// number on a retirement balance sheet, arrived at silently.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_holding_is_unique_per_asset_and_security() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let asset = insert_asset(
            &pool, user, None, "Brokerage", AssetType::Brokerage, TaxTreatment::Taxable,
            None, None, "USD",
        )
        .await
        .unwrap();
        let security = mk_security(&pool, "equity").await;
        mk_holding(&pool, asset.id, security, 100.0).await;

        let err = sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value) \
             VALUES ($1, $2, $3, 1, 100)",
        )
        .bind(Uuid::new_v4())
        .bind(asset.id)
        .bind(security)
        .execute(&pool)
        .await
        .expect_err("the same (asset, security) pair must not insert twice");
        let db_err = err
            .as_database_error()
            .unwrap_or_else(|| panic!("expected a database error, got: {err:?}"));
        assert_eq!(
            db_err.code().as_deref(),
            Some("23505"),
            "expected unique_violation, got: {db_err:?}"
        );

        // What a real re-sync does instead: UPSERT onto the existing row.
        sqlx::query(
            "INSERT INTO asset_holdings (id, asset_id, security_id, quantity, market_value) \
             VALUES ($1, $2, $3, 2, 250) \
             ON CONFLICT (asset_id, security_id) \
             DO UPDATE SET quantity = EXCLUDED.quantity, market_value = EXCLUDED.market_value",
        )
        .bind(Uuid::new_v4())
        .bind(asset.id)
        .bind(security)
        .execute(&pool)
        .await
        .expect("the conflict target must be (asset_id, security_id)");

        let rows: Vec<(f64,)> =
            sqlx::query_as("SELECT market_value FROM asset_holdings WHERE asset_id = $1")
                .bind(asset.id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "a re-sync updates in place: {rows:?}");
        assert!(close(rows[0].0, 250.0), "{rows:?}");

        cleanup(&pool, &[user], &[security]).await;
    }

    /// `assets_manual_has_no_linked_account_check` carries the longest
    /// rationale in the migration and, until now, no test.
    ///
    /// It is deliberately ONE-DIRECTIONAL: `NOT is_manual OR linked_account_id
    /// IS NULL`, not the biconditional `is_manual = (linked_account_id IS
    /// NULL)`. All three assertions below matter together -- the rejection
    /// proves the constraint exists, and the two acceptances prove it is the
    /// one-directional version. A biconditional would also reject the second
    /// acceptance, turning an ordinary `DELETE FROM linked_accounts` into a
    /// constraint violation (see `deleting_a_linked_account_nulls_only_the_link`).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_manual_asset_may_not_claim_a_linked_account() {
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let budget = mk_budget(&pool, user).await;
        let linked = mk_linked_account(&pool, budget, user).await;

        // `insert_asset` never sets `is_manual`, so this goes through raw SQL.
        let insert = |is_manual: bool, link: Option<Uuid>| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO assets \
                     (id, user_id, name, asset_type, tax_treatment, is_manual, linked_account_id) \
                     VALUES ($1, $2, 'X', 'pension', 'other', $3, $4)",
                )
                .bind(Uuid::new_v4())
                .bind(user)
                .bind(is_manual)
                .bind(link)
                .execute(&pool)
                .await
            }
        };

        // REJECTED: manual AND linked.
        let err = insert(true, Some(linked))
            .await
            .expect_err("a manual asset must not carry a linked account");
        let db_err = err
            .as_database_error()
            .unwrap_or_else(|| panic!("expected a database error, got: {err:?}"));
        assert_eq!(
            db_err.code().as_deref(),
            Some("23514"),
            "expected check_violation, got: {db_err:?}"
        );
        assert_eq!(
            db_err.constraint(),
            Some("assets_manual_has_no_linked_account_check"),
            "the rejection must come from THAT check, not an unrelated one: {db_err:?}"
        );

        // ACCEPTED: manual with no link -- an ordinary hand-entered pension.
        insert(true, None)
            .await
            .expect("a manual asset with no linked account is legal");

        // ACCEPTED: synced with a link -- an ordinary synced account.
        insert(false, Some(linked))
            .await
            .expect("a synced asset with a linked account is legal");

        // ACCEPTED: synced with NO link -- the state ON DELETE SET NULL leaves
        // behind, and the exact case the biconditional would have rejected.
        insert(false, None)
            .await
            .expect("a synced asset whose link was nulled is legal");

        cleanup(&pool, &[user], &[]).await;
    }

    // =====================================================================
    // `assets_owner_member_same_user_fkey` -- the composite household-member
    // ownership FK added by 20260728174500 (#500).
    //
    // The property under test is that an asset cannot be attributed to a
    // household member belonging to a DIFFERENT user. `assets` is `user_id`
    // scoped and every read filters `WHERE user_id = $1`, so a row carrying
    // user A's `user_id` beside user B's `owner_member_id` would be served to
    // A without complaint while attributing A's balances to B's household.
    // A bare `owner_member_id` FK would not catch it; only the composite does.
    // =====================================================================

    /// Insert a minimal `retirement_profiles` row for `user_id` and return its id.
    ///
    /// Raw SQL rather than `retirement::upsert_profile` on purpose: that path
    /// gates on Pro entitlement and takes a clock, neither of which has anything
    /// to do with referential integrity, and going through it would make these
    /// tests fail for reasons in a different module.
    ///
    /// Every assumption column is `NOT NULL` with NO SQL default -- #465 keeps
    /// the named Rust constants as the single source of truth for the defaults --
    /// so all of them must be supplied here. `country` must satisfy
    /// `CHECK (country ~ '^[A-Z]{2}$')`, and `employer_match_formula` is JSONB.
    /// `ordinal` is a parameter rather than relying on the column's `DEFAULT 1`
    /// so that a test can build a TWO-MEMBER household. v1 only ever writes 1,
    /// but the row-per-member shape is the entire reason `member_ordinal` exists
    /// now instead of later, and a fixture that cannot express the second member
    /// cannot test the case the schema was designed for.
    async fn mk_profile(pool: &PgPool, user_id: Uuid, ordinal: i32) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO retirement_profiles \
             (id, user_id, member_ordinal, country, birth_date, target_retirement_age, \
              current_gross_income, contribution_rate_pre_tax, contribution_rate_roth, \
              employer_match_formula, expected_real_return, inflation_rate, \
              life_expectancy_age, target_replacement_ratio) \
             VALUES ($1, $2, $3, 'US', DATE '1980-01-01', 65, \
                     100000, 0, 0, \
                     '{\"tiers\":[],\"annual_dollar_cap\":null}'::jsonb, 5.0, 2.5, \
                     90, 0.75)",
        )
        .bind(id)
        .bind(user_id)
        .bind(ordinal)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// Insert an asset attributed to `owner_member_id` THROUGH THE PRODUCTION
    /// WRITER, [`insert_asset`], rather than through raw SQL.
    ///
    /// Going through `insert_asset` is deliberate. Its `user_id` and
    /// `owner_member_id` are independent public parameters with nothing
    /// relating them, so `insert_asset(&pool, stranger, Some(member_of_someone_else), ..)`
    /// COMPILES TODAY -- the mis-attributed row is expressible from Rust right
    /// now, and the database constraint is the only thing standing in its way.
    /// Driving the real writer is what proves that protection is actually
    /// reachable from production code instead of only from a hand-written
    /// INSERT, and it matches `asset_cannot_link_another_users_linked_account`,
    /// the #464 test for the sibling composite FK.
    async fn insert_asset_owned_by(
        pool: &PgPool,
        user_id: Uuid,
        owner_member_id: Option<Uuid>,
    ) -> Result<Asset, sqlx::Error> {
        insert_asset(
            pool,
            user_id,
            owner_member_id,
            "X",
            AssetType::RetirementAccount,
            TaxTreatment::PreTax,
            None,
            None,
            "USD",
        )
        .await
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn an_asset_cannot_reference_another_users_household_member() {
        let pool = assets_test_pool().await;
        let owner = mk_user(&pool).await;
        let stranger = mk_user(&pool).await;
        let owner_member = mk_profile(&pool, owner, 1).await;
        // `stranger` gets their OWN household member too. Without it this test
        // would literally prove only "the referenced pair is absent", which a
        // bare `owner_member_id` FK also satisfies. With it, the row being
        // rejected is one where BOTH halves exist and merely belong to
        // different people -- which is the AC's actual scenario, and the one
        // only a composite FK can catch.
        let _stranger_member = mk_profile(&pool, stranger, 1).await;

        let err = insert_asset_owned_by(&pool, stranger, Some(owner_member))
            .await
            .expect_err("an asset must not be attributed to another user's household member");
        let db_err = err
            .as_database_error()
            .unwrap_or_else(|| panic!("expected a database error, got: {err:?}"));
        assert_eq!(
            db_err.code().as_deref(),
            Some("23503"),
            "expected foreign_key_violation, got: {db_err:?}"
        );
        // Naming the constraint matters: `assets_linked_account_same_user_fkey`
        // ALSO raises 23503 on this table, so a bare code assertion would keep
        // passing if this FK were dropped and some unrelated one fired instead.
        assert_eq!(
            db_err.constraint(),
            Some("assets_owner_member_same_user_fkey"),
            "the rejection must come from the composite household-member FK, \
             not an unrelated constraint: {db_err:?}"
        );

        cleanup(&pool, &[owner, stranger], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn an_asset_can_reference_its_own_users_household_member() {
        // The positive control for the test above. Without it, that test would
        // still pass against an FK that rejected EVERY non-NULL value -- e.g. a
        // bare `REFERENCES retirement_profiles (id)` typo'd onto the wrong
        // column -- and the feature would be broken in the opposite direction.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let member = mk_profile(&pool, user, 1).await;

        let asset = insert_asset_owned_by(&pool, user, Some(member))
            .await
            .expect("an asset may be attributed to its own owner's household member");
        // Round-trip through the production writer's OWN return value, not just
        // a re-SELECT: this pins `insert_asset`'s `owner_member_id` argument to
        // the column, so a future argument reorder cannot silently file assets
        // under the wrong member while every FK test still passes.
        assert_eq!(asset.owner_member_id, Some(member));
        assert_eq!(asset.user_id, user);

        let stored: Option<Uuid> =
            sqlx::query_scalar("SELECT owner_member_id FROM assets WHERE id = $1")
                .bind(asset.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, Some(member));

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn an_asset_may_be_attributed_to_the_second_household_member() {
        // The two-member household -- the case the row-per-member schema and
        // `member_ordinal` exist for (#454 decision 6), and the one #466/#469
        // will build on. v1 writes only ordinal 1, so nothing else exercises a
        // second member at all.
        //
        // Both halves matter: the FK must ACCEPT an asset attributed to the
        // household's second member, and must still REJECT one attributed to a
        // different household's member. An FK keyed on `member_ordinal` rather
        // than `id` would pass the first and fail the second in a way no
        // single-member test could distinguish.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let stranger = mk_user(&pool).await;
        let first = mk_profile(&pool, user, 1).await;
        let second = mk_profile(&pool, user, 2).await;
        let stranger_first = mk_profile(&pool, stranger, 1).await;

        for (member, label) in [(first, "first"), (second, "second")] {
            let asset = insert_asset_owned_by(&pool, user, Some(member))
                .await
                .unwrap_or_else(|e| {
                    panic!("the {label} household member must be attributable: {e}")
                });
            assert_eq!(asset.owner_member_id, Some(member));
        }

        let err = insert_asset_owned_by(&pool, user, Some(stranger_first))
            .await
            .expect_err("another household's member must still be rejected");
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some("assets_owner_member_same_user_fkey"),
            "{err:?}"
        );

        cleanup(&pool, &[user, stranger], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn editing_a_profile_preserves_the_id_that_assets_reference() {
        // `retirement::upsert_profile` is the ONLY writer of `retirement_profiles`,
        // and this FK now sits on the referenced side of it with no `ON UPDATE`
        // clause. It is safe today for exactly one reason: `id` is absent from
        // its `ON CONFLICT (user_id, member_ordinal) DO UPDATE SET` list, so an
        // edit never moves the referenced key. NOTHING pinned that, and both
        // ways of losing it are real bugs:
        //
        //   * adding `id = EXCLUDED.id` to the SET list makes every profile EDIT
        //     raise 23503 for any user who has attributed an asset -- profile
        //     editing starts 500ing for precisely the users who used the feature;
        //   * rewriting the upsert as DELETE-then-INSERT fires `ON DELETE SET
        //     NULL` and SILENTLY unassigns every attributed asset -- the exact
        //     mis-attribution this migration exists to prevent, arriving through
        //     the front door.
        //
        // This drives the real `upsert_profile` (unlike `mk_profile`, which
        // deliberately avoids it) because the round-trip through that function
        // IS the property under test.
        use crate::retirement::{upsert_profile, RetirementProfileInput};

        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let today = NaiveDate::from_ymd_opt(2026, 7, 28).unwrap();
        let create = RetirementProfileInput {
            country: Some("US".to_string()),
            birth_date: NaiveDate::from_ymd_opt(1980, 1, 1),
            current_gross_income: Some(100_000.0),
            ..Default::default()
        };
        let (created, created_warn) = upsert_profile(&pool, user, &create, today).await.unwrap();
        assert!(created_warn.is_none(), "explicit-age create, no warning: {created_warn:?}");

        let asset = insert_asset_owned_by(&pool, user, Some(created.id)).await.unwrap();

        // An ordinary edit of an unrelated field.
        let edit = RetirementProfileInput { inflation_rate: Some(3.1), ..Default::default() };
        let (edited, edited_warn) = upsert_profile(&pool, user, &edit, today).await.unwrap();
        assert!(edited_warn.is_none(), "unrelated edit, no warning: {edited_warn:?}");
        assert_eq!(edited.id, created.id, "an edit must UPDATE the row, never replace its id");

        let still: Option<Uuid> =
            sqlx::query_scalar("SELECT owner_member_id FROM assets WHERE id = $1")
                .bind(asset.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            still,
            Some(created.id),
            "editing the profile must not unassign the asset from its household member"
        );

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn a_null_owner_member_id_is_exempt_from_the_composite_fk() {
        // MATCH SIMPLE -- PostgreSQL's default for multi-column foreign keys --
        // skips the check entirely when any referenced column is NULL. This is
        // not a curiosity: it is the state of EVERY asset row that exists today,
        // and of every asset a user creates before household attribution has a
        // UI. If this ever started failing, the FK would have bricked the whole
        // existing table rather than merely constraining a new link.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;

        insert_asset_owned_by(&pool, user, None)
            .await
            .expect("an unattributed asset must remain insertable with no profile in existence");

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn deleting_a_household_member_unassigns_the_asset_rather_than_destroying_it() {
        // The `ON DELETE SET NULL (owner_member_id)` half of the decision.
        //
        // CASCADE would delete the asset here -- silent data loss from what a
        // user reads as an edit. Plain (non-column-list) `SET NULL` would abort
        // with a not-null violation on `user_id`, because plain SET NULL nulls
        // every column in the FK's local list. This test pins the outcome that
        // is neither: the asset survives, still owned, merely unattributed.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let member = mk_profile(&pool, user, 1).await;
        let asset = insert_asset_owned_by(&pool, user, Some(member)).await.unwrap().id;

        sqlx::query("DELETE FROM retirement_profiles WHERE id = $1")
            .bind(member)
            .execute(&pool)
            .await
            .expect("deleting a household member must not be blocked by its assets");

        let row: Option<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT user_id, owner_member_id FROM assets WHERE id = $1")
                .bind(asset)
                .fetch_optional(&pool)
                .await
                .unwrap();
        let (still_user, still_member) =
            row.expect("the asset must SURVIVE the member deletion, not cascade away with it");
        assert_eq!(still_user, user, "`user_id` must be untouched -- the asset is still owned");
        assert_eq!(still_member, None, "the household-member link must be cleared, not dangling");

        cleanup(&pool, &[user], &[]).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn account_deletion_still_removes_assets_and_profiles_with_the_fk_in_place() {
        // The regression guard for why `ON DELETE RESTRICT` was rejected.
        //
        // This MUST go through `account::delete_user_data`, not a bare
        // `DELETE FROM users`. The bare delete does not exercise the statement
        // that breaks: `delete_user_data` issues an EXPLICIT
        // `DELETE FROM retirement_profiles WHERE user_id = $1` before deleting
        // the user, and never deletes `assets` at all (it relies on the users
        // cascade). Under RESTRICT that explicit statement raises 23503 for any
        // user with a non-NULL `owner_member_id` and aborts the whole erasure
        // transaction -- i.e. GDPR deletion would fail for exactly the users who
        // had used the household feature. A bare `DELETE FROM users` can survive
        // RESTRICT by cascade-ordering luck, so it would NOT catch the
        // regression.
        //
        // This does NOT duplicate `account.rs`'s own `delete_user_data` DB test:
        // that one seeds its asset with `owner_member_id` left NULL, so MATCH
        // SIMPLE exempts it and the new FK is never consulted there at all.
        let pool = assets_test_pool().await;
        let user = mk_user(&pool).await;
        let email = email_of(&pool, user).await;
        let member = mk_profile(&pool, user, 1).await;
        insert_asset_owned_by(&pool, user, Some(member)).await.unwrap();

        let deleted = crate::account::delete_user_data(&pool, user, &email)
            .await
            .expect("account deletion must not be blocked by the household-member FK");
        assert!(deleted, "the users row must actually have been removed");

        let assets_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(assets_left, 0, "assets must cascade away with the user");
        let profiles_left: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM retirement_profiles WHERE user_id = $1")
                .bind(user)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(profiles_left, 0, "the retirement profile must go too");
    }
}
