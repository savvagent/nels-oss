// Per-user LLM token totals — the single source-of-truth aggregate (#174).
//
// `token_sums` sums every `llm_usage` row for one user into a `TokenStats`.
// Postgres `SUM(int)` widens to bigint, and the four token columns are `INT`
// (i32), so each `SUM(...)` is cast to `int8` and decoded as `i64`. The three
// surfaces that expose these totals (Settings, the `/tokens` command, and NL
// chat) all call this one helper so their numbers never drift.

use axum::{extract::State, http::StatusCode, Extension, Json};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AppState;

// Summed token counts for a single user. `Default` yields all-zeros, which is
// also what `token_sums` returns for a user with no usage rows (the COALESCEs
// collapse the empty aggregate to 0).
#[derive(Debug, Default, serde::Serialize)]
pub struct TokenStats {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub thinking_tokens: i64,
    pub total_tokens: i64,
}

// Sum all `llm_usage` rows for `user_id`. Returns all-zeros when the user has no
// usage. `total_tokens` is summed from the stored per-row totals rather than
// recomputed, so it reflects exactly what was recorded at call time.
pub async fn token_sums(db: &sqlx::PgPool, user_id: Uuid) -> Result<TokenStats, sqlx::Error> {
    let row = sqlx::query(
        "SELECT \
           COALESCE(SUM(input_tokens),0)::int8    AS input_tokens, \
           COALESCE(SUM(output_tokens),0)::int8   AS output_tokens, \
           COALESCE(SUM(thinking_tokens),0)::int8 AS thinking_tokens, \
           COALESCE(SUM(total_tokens),0)::int8    AS total_tokens \
         FROM llm_usage WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await?;
    Ok(TokenStats {
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        thinking_tokens: row.get("thinking_tokens"),
        total_tokens: row.get("total_tokens"),
    })
}

/// Render the labeled token-usage context block injected into the chat system
/// instructions so the LLM answers usage questions from real, stored numbers.
/// Pure (no DB) so it can be unit-tested and so the instruction text is pinned.
pub fn build_usage_context(stats: &TokenStats) -> String {
    format!(
        "USER TOKEN USAGE (lifetime, recorded BEFORE this message): input={input}, thinking={thinking}, output={output}, total={total}. If the user asks how many tokens they have used (input/thinking/output/total), answer with EXACTLY these numbers; never invent or estimate token counts.",
        input = stats.input_tokens,
        thinking = stats.thinking_tokens,
        output = stats.output_tokens,
        total = stats.total_tokens,
    )
}

// GET /api/user/token-stats — the authenticated user's lifetime token totals.
// `user_id` comes ONLY from the auth `Extension` (injected by `auth_middleware`),
// never from the client, so a caller can only ever see their own totals.
pub async fn token_stats(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<TokenStats>, StatusCode> {
    token_sums(&state.db, user_id)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("token_stats query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
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
    use uuid::Uuid;

    fn test_state(pool: PgPool) -> AppState {
        AppState {
            db: pool,
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        }
    }

    // build_usage_context renders the labeled NL block from a TokenStats. Pure
    // (no DB), so it runs in CI without Postgres. Pins field->label mapping
    // (guards the input/thinking/output swap hazard) and the instruction text.
    #[test]
    fn build_usage_context_renders_labeled_totals() {
        let stats = TokenStats {
            input_tokens: 1234,
            output_tokens: 56,
            thinking_tokens: 7,
            total_tokens: 1297,
        };
        let ctx = build_usage_context(&stats);
        assert!(ctx.contains("input=1234"), "input label/value: {ctx}");
        assert!(ctx.contains("thinking=7"), "thinking label/value: {ctx}");
        assert!(ctx.contains("output=56"), "output label/value: {ctx}");
        assert!(ctx.contains("total=1297"), "total label/value: {ctx}");
        assert!(
            ctx.contains("answer with EXACTLY these numbers"),
            "instruction text present: {ctx}"
        );
    }

    // The fail-safe path (token_sums failure -> Default) must render all-zeros.
    #[test]
    fn build_usage_context_renders_zeros_for_default() {
        let ctx = build_usage_context(&TokenStats::default());
        assert!(ctx.contains("input=0"), "zero input: {ctx}");
        assert!(ctx.contains("total=0"), "zero total: {ctx}");
    }

    // token_sums aggregates per user: user A's two rows sum correctly, user B's
    // rows are excluded from A's total, and a usage-free user C reports zeros.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn token_sums_aggregates_per_user() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user_a = Uuid::new_v4(); // two usage rows
        let user_b = Uuid::new_v4(); // one usage row, must not leak into A
        let user_c = Uuid::new_v4(); // no usage rows -> all zeros

        for id in [user_a, user_b, user_c] {
            sqlx::query(
                "INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(format!("usage-{id}@example.test"))
            .bind(false)
            .execute(&pool)
            .await
            .expect("seed user");
        }

        // user_a: (10,20,5,total 35) + (1,2,0,total 3) => input 11, output 22,
        // thinking 5, total 38.
        let a_rows = [(10i32, 20i32, 5i32, 35i32), (1i32, 2i32, 0i32, 3i32)];
        // user_b: one nonzero row that must be excluded from user_a's sums.
        let b_rows = [(100i32, 200i32, 50i32, 350i32)];

        for (uid, rows) in [(user_a, &a_rows[..]), (user_b, &b_rows[..])] {
            for &(inp, out, think, tot) in rows {
                sqlx::query(
                    "INSERT INTO llm_usage (id, user_id, model, call_type, input_tokens, output_tokens, thinking_tokens, total_tokens) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(Uuid::new_v4())
                .bind(uid)
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
        }

        let a = token_sums(&pool, user_a).await.expect("token_sums for A");
        assert_eq!(a.input_tokens, 11, "A summed input tokens");
        assert_eq!(a.output_tokens, 22, "A summed output tokens");
        assert_eq!(a.thinking_tokens, 5, "A summed thinking tokens");
        assert_eq!(a.total_tokens, 38, "A summed total tokens");

        // user_b's nonzero row must not bleed into user_a's totals (already
        // asserted above), and B's own sums reflect only its own row.
        let b = token_sums(&pool, user_b).await.expect("token_sums for B");
        assert_eq!(b.input_tokens, 100, "B isolated input tokens");
        assert_eq!(b.output_tokens, 200, "B isolated output tokens");
        assert_eq!(b.thinking_tokens, 50, "B isolated thinking tokens");
        assert_eq!(b.total_tokens, 350, "B isolated total tokens");

        // user_c has no rows -> all zeros via COALESCE.
        let c = token_sums(&pool, user_c).await.expect("token_sums for C");
        assert_eq!(c.input_tokens, 0, "C zero input tokens");
        assert_eq!(c.output_tokens, 0, "C zero output tokens");
        assert_eq!(c.thinking_tokens, 0, "C zero thinking tokens");
        assert_eq!(c.total_tokens, 0, "C zero total tokens");

        let ids = vec![user_a, user_b, user_c];
        let _ = sqlx::query("DELETE FROM llm_usage WHERE user_id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await;
    }

    // Routes a real GET through the `token_stats` handler with `user_id` injected
    // exactly as `auth_middleware` would, and asserts the JSON body equals the
    // seeded user's summed totals. Mirrors admin.rs's oneshot/from_fn harness.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn token_stats_handler_returns_user_sums() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = test_state(pool.clone());

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(user_id)
            .bind(format!("stats-{user_id}@example.test"))
            .bind(false)
            .execute(&pool)
            .await
            .expect("seed user");

        // (10,20,5,35) + (1,2,0,3) => input 11, output 22, thinking 5, total 38.
        let rows = [(10i32, 20i32, 5i32, 35i32), (1i32, 2i32, 0i32, 3i32)];
        for &(inp, out, think, tot) in &rows {
            sqlx::query(
                "INSERT INTO llm_usage (id, user_id, model, call_type, input_tokens, output_tokens, thinking_tokens, total_tokens) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::new_v4())
            .bind(user_id)
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

        // Router that mimics production wiring: a from_fn middleware inserts the
        // user_id into request extensions, standing in for auth_middleware.
        let app = Router::new()
            .route("/user/token-stats", get(token_stats))
            .layer(middleware::from_fn(move |mut req: axum::http::Request<Body>, next: axum::middleware::Next| {
                async move {
                    req.extensions_mut().insert(user_id);
                    next.run(req).await
                }
            }))
            .with_state(state);

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/user/token-stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "handler returns 200");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let stats: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
        assert_eq!(stats["input_tokens"], 11, "summed input tokens");
        assert_eq!(stats["output_tokens"], 22, "summed output tokens");
        assert_eq!(stats["thinking_tokens"], 5, "summed thinking tokens");
        assert_eq!(stats["total_tokens"], 38, "summed total tokens");

        let _ = sqlx::query("DELETE FROM llm_usage WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
    }
}
