use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AppState;
use crate::budget::{check_permission, ensure_not_closed, Permission};
use crate::error::internal_error;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IgnoreRule {
    pub id: Uuid,
    pub budget_id: Uuid,
    pub match_text: String,
    pub external_account_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// True if this imported transaction should be auto-excluded from budget totals:
/// some rule's `match_text` is a case-insensitive substring of `description` AND
/// the rule is either budget-wide (`external_account_id` NULL) or scoped to this
/// import's `account_id`. A blank/whitespace `match_text` never matches (also
/// guarded at create time) so a blank rule cannot exclude everything.
pub fn transaction_matches_ignore_rule(description: &str, account_id: Uuid, rules: &[IgnoreRule]) -> bool {
    let desc = description.to_lowercase();
    rules.iter().any(|r| {
        let m = r.match_text.trim().to_lowercase();
        !m.is_empty()
            && desc.contains(&m)
            && r.external_account_id.map_or(true, |a| a == account_id)
    })
}

pub async fn fetch_ignore_rules(pool: &PgPool, budget_id: Uuid) -> Result<Vec<IgnoreRule>, sqlx::Error> {
    sqlx::query_as::<_, IgnoreRule>(
        "SELECT * FROM transaction_ignore_rules WHERE budget_id = $1 ORDER BY created_at",
    ).bind(budget_id).fetch_all(pool).await
}

/// Chat-callable creator (used by rag.rs in Task 5). Returns Err(user_message) on
/// a blank match_text. Account scope is always None for chat-created rules.
pub async fn create_ignore_rule_for_budget(
    pool: &PgPool, budget_id: Uuid, match_text: &str, external_account_id: Option<Uuid>,
) -> Result<IgnoreRule, String> {
    let m = match_text.trim();
    if m.is_empty() { return Err("Tell me what to ignore.".to_string()); }
    sqlx::query_as::<_, IgnoreRule>(
        "INSERT INTO transaction_ignore_rules (id, budget_id, match_text, external_account_id) \
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(Uuid::new_v4()).bind(budget_id).bind(m).bind(external_account_id)
    .fetch_one(pool).await
    .map_err(|e| {
        tracing::error!(error = ?e, budget_id = %budget_id, "CREATE_IGNORE_RULE insert failed");
        "I couldn't save that ignore rule just now — please try again.".to_string()
    })
}

/// GET /budgets/:id/ignore-rules — any share level (View or better) may read.
pub async fn list_ignore_rules(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<Vec<IgnoreRule>>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm == Permission::None {
        return Err((StatusCode::FORBIDDEN, "You do not have permission to view this budget".to_string()));
    }
    let rules = fetch_ignore_rules(&state.db, budget_id).await.map_err(internal_error)?;
    Ok(Json(rules))
}

#[derive(Deserialize)]
pub struct CreateIgnoreRulePayload {
    pub match_text: String,
    #[serde(default)]
    pub external_account_id: Option<Uuid>,
}

/// POST /budgets/:id/ignore-rules — Owner or Edit only, closed budgets rejected.
pub async fn create_ignore_rule(
    State(state): State<AppState>,
    Path(budget_id): Path<Uuid>,
    Extension(user_id): Extension<Uuid>,
    Json(payload): Json<CreateIgnoreRulePayload>,
) -> Result<Json<IgnoreRule>, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "You do not have permission to modify this budget".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    let m = payload.match_text.trim();
    if m.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "match_text is required".to_string()));
    }
    // Reject an account scope that belongs to another budget: guessing a UUID
    // must not create a cross-budget rule (inconsistent data / info leak).
    if let Some(acct_id) = payload.external_account_id {
        let in_budget: bool = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM linked_accounts WHERE id = $1 AND budget_id = $2)",
        )
        .bind(acct_id)
        .bind(budget_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?;
        if !in_budget {
            return Err((StatusCode::NOT_FOUND, "Linked account not found in this budget".to_string()));
        }
    }
    let rule = sqlx::query_as::<_, IgnoreRule>(
        "INSERT INTO transaction_ignore_rules (id, budget_id, match_text, external_account_id) \
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(Uuid::new_v4()).bind(budget_id).bind(m).bind(payload.external_account_id)
    .fetch_one(&state.db).await
    .map_err(internal_error)?;
    Ok(Json(rule))
}

/// DELETE /budgets/:id/ignore-rules/:rule_id — Owner or Edit only. Scoped by
/// budget_id so a rule from another budget can't be deleted via a guessed id.
pub async fn delete_ignore_rule(
    State(state): State<AppState>,
    Path((budget_id, rule_id)): Path<(Uuid, Uuid)>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let perm = check_permission(&state.db, user_id, budget_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if perm != Permission::Owner && perm != Permission::Edit {
        return Err((StatusCode::FORBIDDEN, "You do not have permission to modify this budget".to_string()));
    }
    crate::access::require_owner_entitled(&state.db, budget_id).await?;
    ensure_not_closed(&state.db, budget_id).await?;

    let deleted = sqlx::query("DELETE FROM transaction_ignore_rules WHERE id = $1 AND budget_id = $2")
        .bind(rule_id).bind(budget_id)
        .execute(&state.db).await
        .map_err(internal_error)?;
    if deleted.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "Ignore rule not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rule(text: &str, acct: Option<Uuid>) -> IgnoreRule {
        IgnoreRule { id: Uuid::new_v4(), budget_id: Uuid::new_v4(), match_text: text.into(), external_account_id: acct, created_at: chrono::Utc::now() }
    }
    #[test]
    fn matches_case_insensitive_substring() {
        let acct = Uuid::new_v4();
        assert!(transaction_matches_ignore_rule("VISA CARD PAYMENT", acct, &[rule("card payment", None)]));
        assert!(!transaction_matches_ignore_rule("Whole Foods", acct, &[rule("card payment", None)]));
    }
    #[test]
    fn account_scope_limits_match() {
        let a = Uuid::new_v4(); let b = Uuid::new_v4();
        assert!(transaction_matches_ignore_rule("card payment", a, &[rule("card", Some(a))]));
        assert!(!transaction_matches_ignore_rule("card payment", b, &[rule("card", Some(a))]));
    }
    #[test]
    fn blank_rule_never_matches() {
        assert!(!transaction_matches_ignore_rule("anything", Uuid::new_v4(), &[rule("   ", None)]));
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string()
        });
        PgPool::connect(&url).await.expect("connect to test db")
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_ignore_rule_persists_and_rejects_blank() {
        let pool = test_pool().await;
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user).bind(format!("ignore-{user}@example.test")).execute(&pool).await.expect("seed user");
        let bid = Uuid::new_v4();
        sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) VALUES ($1,$2,'IgnoreRules','monthly','time_based')")
            .bind(bid).bind(user).execute(&pool).await.expect("seed budget");

        // Non-blank match_text persists with the trimmed value.
        let created = create_ignore_rule_for_budget(&pool, bid, "card payment", None).await;
        let rule = created.expect("non-blank match_text should be saved");
        assert_eq!(rule.match_text, "card payment");
        assert_eq!(rule.budget_id, bid);

        // Blank match_text is rejected and no row is inserted for it.
        let blank = create_ignore_rule_for_budget(&pool, bid, "   ", None).await;
        assert!(blank.is_err(), "blank match_text must be rejected");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transaction_ignore_rules WHERE budget_id = $1")
            .bind(bid).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1, "only the one valid rule should exist; blank must not persist");

        sqlx::query("DELETE FROM budgets WHERE owner_id = $1").bind(user).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_ignore_rule_rejects_cross_budget_account() {
        use std::sync::Arc;
        let pool = test_pool().await;
        let state = AppState {
            db: pool.clone(),
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user).bind(format!("ignore-xbudget-{user}@example.test")).execute(&pool).await.expect("seed user");
        // Budget A (target of the rule) and budget B (owns the linked account).
        let bid_a = Uuid::new_v4();
        let bid_b = Uuid::new_v4();
        for bid in [bid_a, bid_b] {
            sqlx::query("INSERT INTO budgets (id, owner_id, name, time_frame, budget_type) VALUES ($1,$2,'XBudget','monthly','time_based')")
                .bind(bid).bind(user).execute(&pool).await.expect("seed budget");
        }
        // Linked account belongs to budget B, not A.
        let acct = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO linked_accounts (id, budget_id, user_id, provider_account_id, provider_ref) \
             VALUES ($1, $2, $3, $4, 'ref_test')",
        )
        .bind(acct).bind(bid_b).bind(user).bind(format!("fca_{acct}"))
        .execute(&pool).await.expect("seed linked account");

        // Creating a rule in budget A referencing budget B's account must be rejected.
        let res = create_ignore_rule(
            State(state.clone()),
            Path(bid_a),
            Extension(user),
            Json(CreateIgnoreRulePayload { match_text: "card".to_string(), external_account_id: Some(acct) }),
        ).await;
        let err = res.err().expect("cross-budget account must be rejected");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transaction_ignore_rules WHERE budget_id = $1")
            .bind(bid_a).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "no rule should be inserted when the account is cross-budget");

        sqlx::query("DELETE FROM linked_accounts WHERE user_id = $1").bind(user).execute(&pool).await.ok();
        sqlx::query("DELETE FROM budgets WHERE owner_id = $1").bind(user).execute(&pool).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await.ok();
    }
}
