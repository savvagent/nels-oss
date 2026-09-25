// Admin authorization + admin-only aggregation endpoints (Admin PWA, #140).
//
// `admin_middleware` gates the `/api/admin/*` route group: it runs *after*
// `auth_middleware` (which resolves the bearer token and inserts the `user_id`
// into request extensions) and rejects any request whose user is not an admin
// with `403 Forbidden`.

use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use crate::auth::AppState;
use crate::billing::user_is_pro;
use crate::passkeys;

// Reject non-admin requests with 403. Reads the `user_id` that `auth_middleware`
// inserted into the request extensions, then checks `users.is_admin`. Any
// failure mode — missing extension, no such user, `is_admin = false`, or a DB
// error — denies access (fail closed).
pub async fn admin_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let user_id = match request.extensions().get::<Uuid>() {
        Some(id) => *id,
        None => return StatusCode::FORBIDDEN.into_response(),
    };

    let is_admin = sqlx::query_scalar::<_, bool>("SELECT is_admin FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await;

    match is_admin {
        Ok(Some(true)) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

// One row of the admin user list: identity + lifetime LLM token usage + activity
// bounds. Token sums are bigint (i64) because Postgres `SUM(int)` widens to
// bigint. `first_activity`/`last_activity` are derived from `created_at` plus
// chat/session timestamps via LEAST/GREATEST (which ignore NULLs).
// `u.created_at` is the NOT NULL anchor in both LEAST/GREATEST, guaranteeing a
// non-NULL result; do not remove it or the non-Optional first_activity/
// last_activity fields will fail to decode.
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub thinking_tokens: i64,
    pub total_tokens: i64,
    pub first_activity: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
    // Raw Stripe subscription status (#25's `subscriptions` table). NULL when
    // no subscription webhook has ever landed for this user — this covers both
    // "never started checkout" (no `subscriptions` row) and "started checkout,
    // webhook not yet received/abandoned" (`billing::ensure_customer` inserts
    // the row with `status` unset at the START of checkout; it stays NULL until
    // the first `customer.subscription.*` webhook). Both cases render "Free"
    // here since neither implies Pro entitlement; surfaced as-is (not collapsed
    // to a bool) for admin support triage.
    pub subscription_status: Option<String>,
    // Derived entitlement flag. NOT selected by SQL — computed in Rust via
    // `billing::user_is_pro` after the fetch (see `list_users`) so the
    // trialing/active entitlement rule lives in exactly one place in the
    // codebase. Always overwritten before the row is returned; the `#[sqlx(skip)]`
    // default keeps `#[derive(sqlx::FromRow)]` mechanical for every other field.
    #[sqlx(skip)]
    pub is_pro: bool,
}

// GET /api/admin/users — every user with summed llm_usage and activity bounds.
// Gated by `admin_middleware`, so reaching this handler already implies admin.
pub async fn list_users(
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminUserRow>>, StatusCode> {
    let mut rows = sqlx::query_as::<_, AdminUserRow>(
        r#"
        SELECT
          u.id, u.email, u.name, u.created_at,
          COALESCE(usage.input_tokens, 0)::bigint    AS input_tokens,
          COALESCE(usage.output_tokens, 0)::bigint   AS output_tokens,
          COALESCE(usage.thinking_tokens, 0)::bigint AS thinking_tokens,
          COALESCE(usage.total_tokens, 0)::bigint    AS total_tokens,
          LEAST(
            u.created_at,
            (SELECT MIN(cm.created_at) FROM chat_messages cm WHERE cm.user_id = u.id)
          ) AS first_activity,
          GREATEST(
            u.created_at,
            (SELECT MAX(cm.created_at) FROM chat_messages cm WHERE cm.user_id = u.id),
            (SELECT MAX(s.created_at)  FROM sessions s      WHERE s.user_id = u.id)
          ) AS last_activity,
          sub.status AS subscription_status
        FROM users u
        LEFT JOIN (
          SELECT user_id,
                 SUM(input_tokens)    AS input_tokens,
                 SUM(output_tokens)   AS output_tokens,
                 SUM(thinking_tokens) AS thinking_tokens,
                 SUM(total_tokens)    AS total_tokens
          FROM llm_usage GROUP BY user_id
        ) usage ON usage.user_id = u.id
        LEFT JOIN subscriptions sub ON sub.user_id = u.id
        ORDER BY u.created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("list_users query failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Derive `is_pro` from the raw status via the single source of truth
    // (billing::user_is_pro), rather than duplicating the trialing/active rule
    // in SQL — see AdminUserRow's `is_pro` doc comment.
    for row in rows.iter_mut() {
        row.is_pro = user_is_pro(row.subscription_status.as_deref());
    }

    Ok(Json(rows))
}

#[derive(serde::Serialize)]
pub struct ResetCredentialsResponse {
    pub recovery_codes: Vec<String>,
}

// POST /api/admin/users/:id/reset-credentials — last-resort account recovery
// (nels#551): clears every WebAuthn credential a user holds (both RPs) and
// rotates their recovery codes, returning the fresh codes once so the admin
// can hand them to the locked-out user out of band. Re-enrollment afterward
// is the normal /auth/recovery/start+finish flow — the `users` row itself is
// never touched, so no separate "re-register" path is needed.
// Gated by `admin_middleware`, so reaching this handler already implies admin.
pub async fn reset_credentials(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<ResetCredentialsResponse>, StatusCode> {
    passkeys::delete_all_credentials(&state.db, user_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let recovery_codes = passkeys::rotate_recovery_codes(&state.db, user_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ResetCredentialsResponse { recovery_codes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        middleware,
        routing::get,
        Router,
    };
    use sqlx::PgPool;
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    fn test_state(pool: PgPool) -> AppState {
        AppState {
            db: pool,
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()),
            webauthn: Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    // Minimal downstream handler used to prove the middleware let the request
    // through (it only runs when admin_middleware calls `next`).
    async fn ok_handler() -> StatusCode {
        StatusCode::OK
    }

    // A non-admin user must get 403 and an admin user must pass through. We build
    // a tiny router that mimics the production wiring: a middleware that inserts
    // `user_id` (standing in for auth_middleware), then admin_middleware.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn admin_middleware_blocks_non_admin_allows_admin() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let normal_id = Uuid::new_v4();
        let admin_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
        )
        .bind(normal_id)
        .bind(format!("normal-{normal_id}@example.test"))
        .bind(false)
        .execute(&pool)
        .await
        .expect("seed normal user");
        sqlx::query(
            "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
        )
        .bind(admin_id)
        .bind(format!("admin-{admin_id}@example.test"))
        .bind(true)
        .execute(&pool)
        .await
        .expect("seed admin user");

        // Build a router gated by admin_middleware. A preceding middleware inserts
        // the given user_id into extensions, exactly as auth_middleware would.
        fn router(state: AppState, user_id: Uuid) -> Router {
            Router::new()
                .route("/admin/users", get(ok_handler))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    admin_middleware,
                ))
                .layer(middleware::from_fn(move |mut req: Request, next: Next| {
                    let uid = user_id;
                    async move {
                        req.extensions_mut().insert(uid);
                        next.run(req).await
                    }
                }))
                .with_state(state)
        }

        let resp_normal = router(state.clone(), normal_id)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp_normal.status(),
            StatusCode::FORBIDDEN,
            "non-admin must be rejected with 403"
        );

        let resp_admin = router(state.clone(), admin_id)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp_admin.status(),
            StatusCode::OK,
            "admin must pass through the middleware"
        );

        let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(vec![normal_id, admin_id])
            .execute(&pool)
            .await;
    }

    // list_users aggregates token usage and activity bounds correctly: the user
    // with usage gets the summed totals; the user with none gets zeros; and
    // first_activity <= last_activity for both.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn list_users_aggregates_usage_and_activity() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let user_a = Uuid::new_v4(); // has usage + chat messages
        let user_b = Uuid::new_v4(); // admin, no usage
        let user_c = Uuid::new_v4(); // active (non-trialing) subscriber

        sqlx::query(
            "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
        )
        .bind(user_a)
        .bind(format!("usage-a-{user_a}@example.test"))
        .bind(false)
        .execute(&pool)
        .await
        .expect("seed user_a");
        sqlx::query(
            "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
        )
        .bind(user_b)
        .bind(format!("usage-b-{user_b}@example.test"))
        .bind(true)
        .execute(&pool)
        .await
        .expect("seed user_b");
        sqlx::query(
            "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
        )
        .bind(user_c)
        .bind(format!("usage-c-{user_c}@example.test"))
        .bind(false)
        .execute(&pool)
        .await
        .expect("seed user_c");

        // Two llm_usage rows for user_a: sums => input 30, output 12, thinking 9,
        // total 51.
        for (inp, out, think, tot) in [(10i32, 4i32, 3i32, 17i32), (20i32, 8i32, 6i32, 34i32)] {
            sqlx::query(
                "INSERT INTO llm_usage (id, user_id, model, call_type, input_tokens, output_tokens, thinking_tokens, total_tokens) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::new_v4())
            .bind(user_a)
            .bind("test-model")
            .bind("chat")
            .bind(inp)
            .bind(out)
            .bind(think)
            .bind(tot)
            .execute(&pool)
            .await
            .expect("seed llm_usage");
        }

        // A chat message for user_a so the activity-bounds subqueries are exercised.
        sqlx::query(
            "INSERT INTO chat_messages (id, user_id, sender, message_text) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(user_a)
        .bind("user")
        .bind("hello")
        .execute(&pool)
        .await
        .expect("seed chat_message");

        // Drive the LEAST/GREATEST math with explicit out-of-band timestamps for
        // user_a: a chat message a year in the PAST (must become the
        // first_activity floor, below users.created_at ~= NOW()) and a session a
        // year in the FUTURE (must become the last_activity ceiling). Plain
        // <=/>= against NOW() would be tautological since all default-seeded
        // timestamps are ~NOW(), so these bounds prove the arms are honored.
        let past_ts = chrono::Utc::now() - chrono::Duration::days(365);
        let future_ts = chrono::Utc::now() + chrono::Duration::days(365);
        let past_msg_id = Uuid::new_v4();
        let future_session_token = format!("admin-test-session-{user_a}");

        sqlx::query(
            "INSERT INTO chat_messages (id, user_id, sender, message_text, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(past_msg_id)
        .bind(user_a)
        .bind("user")
        .bind("ancient message")
        .bind(past_ts)
        .execute(&pool)
        .await
        .expect("seed past chat_message");

        sqlx::query(
            "INSERT INTO sessions (token, user_id, created_at, expires_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&future_session_token)
        .bind(user_a)
        .bind(future_ts)
        .bind(future_ts + chrono::Duration::days(1))
        .execute(&pool)
        .await
        .expect("seed future session");

        // Seed a `trialing` subscription for user_a and an `active` subscription
        // for user_c, exercising both entitled statuses through the real SQL
        // round-trip; user_b stays a free user (no subscriptions row) to
        // exercise the LEFT JOIN's NULL arm.
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) \
             VALUES ($1, $2, $3)",
        )
        .bind(user_a)
        .bind(format!("cus_test_{user_a}"))
        .bind("trialing")
        .execute(&pool)
        .await
        .expect("seed subscription for user_a");
        sqlx::query(
            "INSERT INTO subscriptions (user_id, stripe_customer_id, status) \
             VALUES ($1, $2, $3)",
        )
        .bind(user_c)
        .bind(format!("cus_test_{user_c}"))
        .bind("active")
        .execute(&pool)
        .await
        .expect("seed subscription for user_c");

        let resp = list_users(State(state.clone()))
            .await
            .expect("list_users should succeed");
        let rows = resp.0;

        let row_a = rows
            .iter()
            .find(|r| r.id == user_a)
            .expect("user_a present in results");
        let row_b = rows
            .iter()
            .find(|r| r.id == user_b)
            .expect("user_b present in results");
        let row_c = rows
            .iter()
            .find(|r| r.id == user_c)
            .expect("user_c present in results");

        assert_eq!(row_a.input_tokens, 30, "user_a summed input tokens");
        assert_eq!(row_a.output_tokens, 12, "user_a summed output tokens");
        assert_eq!(row_a.thinking_tokens, 9, "user_a summed thinking tokens");
        assert_eq!(row_a.total_tokens, 51, "user_a summed total tokens");
        assert!(
            row_a.first_activity <= row_a.last_activity,
            "user_a first_activity must be <= last_activity"
        );

        // The past chat message must drive first_activity (LEAST floor), proving
        // the chat-message MIN is honored below users.created_at. Allow a small
        // tolerance for TZ/precision; the floor must be at the past timestamp and
        // clearly before the user's created_at (~NOW()).
        let delta = chrono::Duration::seconds(1);
        assert!(
            (row_a.first_activity - past_ts).abs() <= delta,
            "user_a first_activity ({}) must equal the past chat timestamp ({past_ts})",
            row_a.first_activity
        );
        assert!(
            row_a.first_activity < row_a.created_at,
            "user_a first_activity ({}) must be before users.created_at ({})",
            row_a.first_activity,
            row_a.created_at
        );

        // The future session must drive last_activity (GREATEST ceiling), proving
        // the sessions arm is honored above NOW().
        assert!(
            (row_a.last_activity - future_ts).abs() <= delta,
            "user_a last_activity ({}) must equal the future session timestamp ({future_ts})",
            row_a.last_activity
        );
        assert!(
            row_a.last_activity > row_a.created_at,
            "user_a last_activity ({}) must be after users.created_at ({})",
            row_a.last_activity,
            row_a.created_at
        );

        assert_eq!(row_b.input_tokens, 0, "user_b has no usage");
        assert_eq!(row_b.output_tokens, 0, "user_b has no usage");
        assert_eq!(row_b.thinking_tokens, 0, "user_b has no usage");
        assert_eq!(row_b.total_tokens, 0, "user_b has no usage");
        assert!(
            row_b.first_activity <= row_b.last_activity,
            "user_b first_activity must be <= last_activity"
        );

        assert_eq!(
            row_a.subscription_status,
            Some("trialing".to_string()),
            "user_a has a trialing subscription"
        );
        assert!(row_a.is_pro, "trialing status must be Pro-entitled");
        assert_eq!(
            row_b.subscription_status, None,
            "user_b has no subscriptions row"
        );
        assert!(!row_b.is_pro, "no subscription must not be Pro-entitled");

        assert_eq!(
            row_c.subscription_status,
            Some("active".to_string()),
            "user_c has an active subscription"
        );
        assert!(row_c.is_pro, "active status must be Pro-entitled");

        // Cleanup (llm_usage + chat_messages cascade on user delete, but be explicit).
        let _ = sqlx::query("DELETE FROM llm_usage WHERE user_id = ANY($1)")
            .bind(vec![user_a, user_b, user_c])
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM chat_messages WHERE user_id = ANY($1)")
            .bind(vec![user_a, user_b, user_c])
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM sessions WHERE user_id = ANY($1)")
            .bind(vec![user_a, user_b, user_c])
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM subscriptions WHERE user_id = ANY($1)")
            .bind(vec![user_a, user_b, user_c])
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(vec![user_a, user_b, user_c])
            .execute(&pool)
            .await;
    }

    // admin_middleware must fail closed when no `user_id` extension is present
    // (i.e. when auth_middleware did not run). Build a router mounting a route
    // behind ONLY admin_middleware — no preceding layer inserts a user_id — and
    // assert the request is rejected with 403.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn admin_middleware_rejects_missing_user_id_extension() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let router = Router::new()
            .route("/admin/users", get(ok_handler))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                admin_middleware,
            ))
            .with_state(state);

        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "missing user_id extension must be rejected with 403"
        );
    }

    // Admin-assisted reset (nels#551, last-resort account recovery): clears
    // EVERY credential a user holds (both RPs) and rotates their recovery
    // codes, invalidating the old unused batch.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn reset_credentials_clears_all_credentials_and_rotates_codes() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("reset-creds-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        // Fake credentials for BOTH RPs — the reset must clear both, not just
        // the one the operator happens to be looking at.
        for rp in ["app", "admin"] {
            sqlx::query(
                "INSERT INTO webauthn_credentials (id, user_id, rp_id, credential_id, passkey) \
                 VALUES ($1, $2, $3, $4, '{}'::jsonb)",
            )
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(rp)
            .bind(Uuid::new_v4().as_bytes().to_vec())
            .execute(&pool)
            .await
            .expect("seed fake credential");
        }

        let old_codes = crate::passkeys::issue_recovery_codes(&pool, user_id)
            .await
            .expect("issue initial codes");

        let resp = reset_credentials(State(state), Path(user_id))
            .await
            .expect("reset_credentials should succeed");

        let remaining_creds: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM webauthn_credentials WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining_creds, 0, "all credentials for both RPs must be cleared");

        assert!(!resp.0.recovery_codes.is_empty(), "must return a fresh batch of codes");
        assert!(
            resp.0.recovery_codes.iter().all(|c| !old_codes.contains(c)),
            "the fresh batch must not reuse any old code"
        );

        for old in &old_codes {
            let stale = passkeys::find_unused_code(&pool, &format!("reset-creds-{user_id}@example.test"), old)
                .await
                .unwrap();
            assert!(stale.is_none(), "old unused code must be invalidated by the reset");
        }

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }
}
