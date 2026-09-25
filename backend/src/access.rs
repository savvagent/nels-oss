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

/// Pure parse of the enforcement flag value (no env access) so it's unit-testable
/// without mutating process-global state (which would race concurrent write tests).
fn enforcement_flag_enabled(val: Option<&str>) -> bool {
    matches!(val, Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// True iff write-enforcement is enabled (`BILLING_WRITE_ENFORCEMENT` = "1"/"true").
/// Default OFF so the write-gate ships inert and is flipped on deliberately in
/// prod only after prices are seeded and owner rows resolve correctly.
pub(crate) fn billing_write_enforced() -> bool {
    enforcement_flag_enabled(std::env::var("BILLING_WRITE_ENFORCEMENT").ok().as_deref())
}

/// Write-gate entitlement check. When enforcement is OFF (default) this is a
/// no-op — the caller's own permission check already ran. When ON, a budget
/// whose OWNER has lapsed (`Tier::None`) is read-only → 402. Insert AFTER the
/// existing permission check at each mutation site; never on read paths.
pub async fn require_owner_entitled(
    pool: &PgPool,
    budget_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    if !billing_write_enforced() {
        return Ok(());
    }
    require_owner_entitled_enforced(pool, budget_id, &PriceCatalog::from_env()).await
}

/// The enforcing core (bypasses the flag), for hermetic tests and composition.
pub(crate) async fn require_owner_entitled_enforced(
    pool: &PgPool,
    budget_id: Uuid,
    catalog: &PriceCatalog,
) -> Result<(), (StatusCode, String)> {
    let ent = budget_entitlement_with(pool, budget_id, catalog).await?;
    if ent.tier == Tier::None {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            "This budget is read-only — the owner's subscription has lapsed.".to_string(),
        ));
    }
    Ok(())
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
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
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
    async fn writable_viewer_on_lapsed_is_403() {
        // Permission is checked BEFORE entitlement: a viewer on a lapsed-owner
        // budget must get 403 (not 402) — otherwise an unauthorized user would
        // see the owner's billing state. Pins the check ordering.
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
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
    async fn writable_editor_on_past_due_owner_ok() {
        // past_due is a grace state — the owner stays entitled during Stripe's
        // dunning retries, so an editor can still write. Pins that grace-period
        // writes are NOT blocked (the headline of the past_due decision).
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "past_due", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let editor = mk_user(&db).await;
        mk_share(&db, bid, &email_of(&db, editor).await, "edit").await;
        assert!(require_writable_budget_with(&db, editor, bid, &catalog()).await.is_ok());
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

    #[tokio::test]
    #[ignore]
    async fn owner_entitled_enforced_allows_entitled_owner() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "active", Some("price_basic")).await; // Basic is entitled (tier != None)
        let bid = mk_budget(&db, owner).await;
        assert!(require_owner_entitled_enforced(&db, bid, &catalog()).await.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn owner_entitled_enforced_blocks_lapsed_owner_402() {
        let db = test_pool().await;
        let owner = mk_user(&db).await;
        mk_sub(&db, owner, "canceled", Some("price_pro")).await;
        let bid = mk_budget(&db, owner).await;
        let err = require_owner_entitled_enforced(&db, bid, &catalog()).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn enforcement_flag_parses() {
        assert!(super::enforcement_flag_enabled(Some("1")));
        assert!(super::enforcement_flag_enabled(Some("true")));
        assert!(super::enforcement_flag_enabled(Some("TRUE")));
        assert!(!super::enforcement_flag_enabled(Some("0")));
        assert!(!super::enforcement_flag_enabled(Some("")));
        assert!(!super::enforcement_flag_enabled(Some("yes")));
        assert!(!super::enforcement_flag_enabled(None));
    }
}
