# Rate-limit GitHub issue filing (#191) — Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development. Steps use `- [ ]`.

**Goal:** Add a per-user rolling-window quota inside `create_issue_core()` so both the REST `/issues-create` path and the chat `REPORT_ISSUE` action are throttled, with an honest 429 / `mutation_error` message on exceed.

**Architecture:** New DB table `github_issue_filings` records one row per successfully filed issue, keyed by user. `create_issue_core` (the single chokepoint, now taking `db` + `user_id`) checks the rolling count before any GitHub call and records on success. Limit/window are env-configurable with defaults; a GitHub-header backstop maps upstream rate-limit responses to a clear 429.

**Tech Stack:** Rust, axum 0.7, sqlx 0.7 (Postgres), chrono, reqwest.

**Repo conventions (Phase 0.5):** Test: `cd backend && cargo test`; DB tests `cargo test -- --ignored` (local pg :6153). Build: `cargo check`. Migrations auto-applied on server start via `sqlx::migrate!`; insert rows bind `id` explicitly (`Uuid::new_v4()`). Commit format: conventional commits `feat(#191): …`. DB-backed tests are `#[ignore = "requires Postgres; run via: cargo test -- --ignored"]` and connect to `DATABASE_URL` (fallback `postgres://postgres:postgrespassword@localhost:6153/budget_rag`), seed `users`, assert, clean up.

**Determinism note:** `cargo test` (default) skips `#[ignore]`, and `cargo test -- --ignored` runs ONLY them — separate invocations. So the env-mutating pure `rate_limit_config` test (not ignored) never shares a process with the DB tests (ignored). All DB tests set identical env values (`GITHUB_ISSUE_RATE_LIMIT=3`, `GITHUB_ISSUE_RATE_WINDOW_SECS=3600`) and use unique `user_id`s, so they don't race each other.

---

### Task 1: Schema, config, and pure/DB helpers in `github.rs`

**Files:**
- Create: `backend/migrations/20260626000000_github_issue_filings.sql`
- Modify: `backend/src/github.rs` (add imports, constants, helpers, tests)

- [ ] **Step 1: Add the migration**

Create `backend/migrations/20260626000000_github_issue_filings.sql`:

```sql
-- Per-user rolling-window rate limit for GitHub issue filing (#191).
-- One row per SUCCESSFULLY created issue (from either the REST /issues-create
-- path or the chat REPORT_ISSUE action), keyed by the authenticated user.
-- create_issue_core counts rows within the configured window to enforce the
-- quota. ON DELETE CASCADE so a deleted user's rows go with them.
CREATE TABLE IF NOT EXISTS github_issue_filings (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS github_issue_filings_user_created_idx
  ON github_issue_filings (user_id, created_at DESC);
```

Apply it to the local test DB so `--ignored` tests can run:
```bash
PGPASSWORD=postgrespassword psql -h localhost -p 6153 -U postgres -d budget_rag \
  -f backend/migrations/20260626000000_github_issue_filings.sql
```

- [ ] **Step 2: Add imports + constants to `github.rs`**

At the top of `backend/src/github.rs`, after the existing `use uuid::Uuid;`, add:
```rust
use chrono::Utc;
use sqlx::PgPool;
```

After the existing `const LIST_LIMIT: u8 = 20;`, add:
```rust
/// Default per-user issue-filing quota and rolling window (seconds), overridable
/// via `GITHUB_ISSUE_RATE_LIMIT` / `GITHUB_ISSUE_RATE_WINDOW_SECS`.
const DEFAULT_RATE_LIMIT: i64 = 5;
const DEFAULT_WINDOW_SECS: i64 = 3600;

/// User-facing message when a user exceeds their own issue-filing quota.
const RATE_LIMITED_MSG: &str =
    "You've filed several reports recently. Please wait a bit before filing another.";

/// User-facing message when GitHub itself is rate-limiting us (header backstop).
const GITHUB_RATE_LIMITED_MSG: &str =
    "GitHub is temporarily rate-limiting issue creation. Please try again later.";
```

- [ ] **Step 3: Write failing unit tests for the pure helpers**

In the `#[cfg(test)] mod tests` block in `github.rs`, add:
```rust
    // --- rate_limit_config ---
    // Mutates process env; kept in one test to avoid cross-test races. Only runs
    // under the default `cargo test` (this test is NOT #[ignore]).
    #[test]
    fn rate_limit_config_defaults_and_overrides() {
        std::env::remove_var("GITHUB_ISSUE_RATE_LIMIT");
        std::env::remove_var("GITHUB_ISSUE_RATE_WINDOW_SECS");
        assert_eq!(rate_limit_config(), (5, 3600));

        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "10");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "60");
        assert_eq!(rate_limit_config(), (10, 60));

        // Malformed / out-of-range values fall back to the defaults (floor of 1).
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "0");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "abc");
        assert_eq!(rate_limit_config(), (5, 3600));

        std::env::remove_var("GITHUB_ISSUE_RATE_LIMIT");
        std::env::remove_var("GITHUB_ISSUE_RATE_WINDOW_SECS");
    }

    // --- is_github_rate_limited ---
    #[test]
    fn detects_github_rate_limit_responses() {
        use reqwest::header::HeaderMap;
        use reqwest::StatusCode as RStatus;

        // 429 is always a rate-limit signal regardless of headers.
        assert!(is_github_rate_limited(RStatus::TOO_MANY_REQUESTS, &HeaderMap::new()));

        // 403 with exhausted remaining => rate limited.
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        assert!(is_github_rate_limited(RStatus::FORBIDDEN, &h));

        // 403 with remaining left => NOT a rate limit (some other 403).
        let mut h2 = HeaderMap::new();
        h2.insert("x-ratelimit-remaining", "57".parse().unwrap());
        assert!(!is_github_rate_limited(RStatus::FORBIDDEN, &h2));

        // 403 with no header => not treated as rate limited.
        assert!(!is_github_rate_limited(RStatus::FORBIDDEN, &HeaderMap::new()));

        // Unrelated failure.
        assert!(!is_github_rate_limited(RStatus::BAD_GATEWAY, &HeaderMap::new()));
    }
```

- [ ] **Step 4: Run the new unit tests to verify they fail to compile (helpers undefined)**

Run: `cd backend && cargo test rate_limit_config_defaults_and_overrides detects_github_rate_limit_responses`
Expected: FAIL — `cannot find function rate_limit_config` / `is_github_rate_limited`.

- [ ] **Step 5: Implement the pure helpers**

In `github.rs`, after the constants from Step 2 (above `redact_sensitive`), add:
```rust
/// Resolve the per-user issue-filing quota `(limit, window_secs)` from the
/// environment, falling back to the defaults. Both values are clamped to a
/// minimum of 1 — the limit cannot be disabled; this is an always-on abuse
/// throttle. A malformed or out-of-range value falls back to its default.
pub fn rate_limit_config() -> (i64, i64) {
    fn read(name: &str, default: i64) -> i64 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(default)
    }
    (
        read("GITHUB_ISSUE_RATE_LIMIT", DEFAULT_RATE_LIMIT),
        read("GITHUB_ISSUE_RATE_WINDOW_SECS", DEFAULT_WINDOW_SECS),
    )
}

/// GitHub-header backstop: detect that GitHub itself is rate-limiting us, so we
/// surface a clear 429 rather than a generic 502. True when the response is a
/// 429, or a 403 whose `x-ratelimit-remaining` header is exhausted (`0`).
pub fn is_github_rate_limited(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> bool {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim() == "0")
            .unwrap_or(false);
    }
    false
}
```

- [ ] **Step 6: Run unit tests to verify they pass**

Run: `cd backend && cargo test rate_limit_config_defaults_and_overrides detects_github_rate_limit_responses`
Expected: PASS (2 tests).

- [ ] **Step 7: Implement the DB helpers `check_issue_quota` + `record_issue_filing`**

In `github.rs`, after the pure helpers, add:
```rust
/// Enforce the per-user rolling-window issue-filing quota. Counts the user's
/// filings within the configured window; returns `Err((429, RATE_LIMITED_MSG))`
/// when the user is at or over the limit, otherwise `Ok(())`. A DB failure
/// fails closed (we do not allow unbounded filing when the quota can't be
/// checked).
pub async fn check_issue_quota(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let (limit, window_secs) = rate_limit_config();
    let cutoff = Utc::now() - chrono::Duration::seconds(window_secs);

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_issue_filings WHERE user_id = $1 AND created_at >= $2",
    )
    .bind(user_id)
    .bind(cutoff)
    .fetch_one(db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "issue rate-limit count query failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not check the issue rate limit right now. Please try again later.".to_string(),
        )
    })?;

    if count >= limit {
        return Err((StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED_MSG.to_string()));
    }
    Ok(())
}

/// Record one successful issue filing for `user_id` so it counts toward the
/// rolling-window quota. Fail-safe: a write failure is logged at `warn` and
/// never fails an already-created issue (mirrors the `llm_usage` write).
pub async fn record_issue_filing(db: &PgPool, user_id: Uuid) {
    let res = sqlx::query("INSERT INTO github_issue_filings (id, user_id) VALUES ($1, $2)")
        .bind(Uuid::new_v4())
        .bind(user_id)
        .execute(db)
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "failed to record issue filing for rate limiting");
    }
}
```

- [ ] **Step 8: Write the DB-backed `--ignored` test for the quota helpers**

Do NOT add `PgPoolOptions` here — it is first used in Task 2 and will be imported there. Add the DB test:

> **Determinism (intra-`--ignored`):** these `#[ignore]` DB tests set `GITHUB_ISSUE_RATE_LIMIT=3` at the start but DO NOT `remove_var` it at the end. `cargo test -- --ignored` runs them in parallel; a trailing `remove_var` from one finishing test could race another test's `set_var`→read window and make it read the default limit 5 instead of 3, intermittently failing the 429 assertions. Leaving the identical value set is safe (the pure `rate_limit_config` test runs in a separate, non-`--ignored` process invocation).
```rust
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_issue_quota_enforces_window_and_isolation() {
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        for id in [user, other] {
            sqlx::query("INSERT INTO users (id, email, totp_secret, is_admin) VALUES ($1, $2, $3, $4)")
                .bind(id)
                .bind(format!("rl-{id}@example.test"))
                .bind("s")
                .bind(false)
                .execute(&pool)
                .await
                .expect("seed user");
        }

        // 2 recent filings (< limit) => Ok.
        for _ in 0..2 {
            record_issue_filing(&pool, user).await;
        }
        assert!(check_issue_quota(&pool, user).await.is_ok(), "2 < limit 3 is allowed");

        // A 3rd recent filing => at limit => next check is rejected with 429.
        record_issue_filing(&pool, user).await;
        let err = check_issue_quota(&pool, user)
            .await
            .expect_err("3 >= limit 3 must be rejected");
        assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.1.contains("filed several reports"), "clear message: {}", err.1);

        // An out-of-window filing (2h old) does NOT count.
        let old_user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret, is_admin) VALUES ($1, $2, $3, $4)")
            .bind(old_user)
            .bind(format!("rl-{old_user}@example.test"))
            .bind("s").bind(false)
            .execute(&pool).await.expect("seed old user");
        for _ in 0..5 {
            sqlx::query("INSERT INTO github_issue_filings (id, user_id, created_at) VALUES ($1, $2, NOW() - INTERVAL '2 hours')")
                .bind(Uuid::new_v4())
                .bind(old_user)
                .execute(&pool).await.expect("seed old filing");
        }
        assert!(check_issue_quota(&pool, old_user).await.is_ok(), "out-of-window filings do not count");

        // The `other` user (no recent filings) is unaffected.
        assert!(check_issue_quota(&pool, other).await.is_ok(), "different user is isolated");

        // Cleanup (filings cascade on user delete). Intentionally do NOT
        // remove_var the rate-limit env vars — see the determinism note above.
        let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&vec![user, other, old_user])
            .execute(&pool)
            .await;
    }
```

- [ ] **Step 9: Run the DB test**

Run: `cd backend && cargo test --lib check_issue_quota_enforces_window_and_isolation -- --ignored`
Expected: PASS (1 test).

- [ ] **Step 10: Build check + commit**

Run: `cd backend && cargo check && cargo test rate_limit_config_defaults_and_overrides detects_github_rate_limit_responses`
Expected: builds clean; 2 unit tests pass.

```bash
git add backend/migrations/20260626000000_github_issue_filings.sql backend/src/github.rs
git commit -m "feat(#191): add per-user issue-filing quota schema and helpers"
```

---

### Task 2: Wire the quota into `create_issue_core` + both call sites + docs

**Files:**
- Modify: `backend/src/github.rs` (`create_issue_core`, `create_issue`, update existing empty-title test)
- Modify: `backend/src/rag.rs` (REPORT_ISSUE arm, ~line 2848)
- Modify: `backend/.env.example`, `backend/fly.toml` (document the new vars)

- [ ] **Step 1: Update the existing empty-title test to the new signature (failing first)**

In `github.rs` tests, add the import at the top of `mod tests` (after `use super::*;`):
```rust
    use sqlx::postgres::PgPoolOptions;
```
Replace the existing `create_issue_core_rejects_empty_title_before_network` test body with:
```rust
    #[tokio::test]
    async fn create_issue_core_rejects_empty_title_before_network() {
        // Empty/whitespace title is rejected with 400 *before* any quota check,
        // config lookup, or network call. A lazy pool is never actually queried
        // on this path, so this stays a pure unit test (no DB needed).
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")
            .expect("lazy pool");
        let err = create_issue_core(&pool, Uuid::new_v4(), "   ", None, APP_FOOTER)
            .await
            .expect_err("empty title must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 2: Run it to verify it fails to compile (old signature)**

Run: `cd backend && cargo test create_issue_core_rejects_empty_title_before_network`
Expected: FAIL — arity/type mismatch on `create_issue_core`.

- [ ] **Step 3: Update `create_issue_core` signature + enforcement**

Replace the `create_issue_core` function signature and body. New signature:
```rust
pub async fn create_issue_core(
    db: &PgPool,
    user_id: Uuid,
    raw_title: &str,
    raw_body: Option<&str>,
    footer: &str,
) -> Result<GithubIssueItem, (StatusCode, String)> {
    let title = raw_title.trim();
    if title.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "A title is required to create an issue.".to_string(),
        ));
    }

    // Per-user rolling-window quota — enforced for BOTH the REST /issues-create
    // path and the chat REPORT_ISSUE action, before any GitHub call.
    check_issue_quota(db, user_id).await?;

    let cfg = match GithubConfig::from_env() {
        Some(c) => c,
        None => return Err(not_configured()),
    };

    let title = redact_sensitive(title);
    let body = raw_body.map(redact_sensitive).unwrap_or_default();
    let body = format!("{body}{footer}");

    let url = format!("{GITHUB_API}/repos/{}/issues", cfg.repo);
    let payload = serde_json::json!({ "title": title, "body": body });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .bearer_auth(&cfg.token)
        .json(&payload)
        .send()
        .await
        .map_err(|e| upstream_error("create issue", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        // Backstop: if GitHub itself is rate-limiting us, say so clearly (429)
        // rather than returning a generic 502.
        if is_github_rate_limited(status, resp.headers()) {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                GITHUB_RATE_LIMITED_MSG.to_string(),
            ));
        }
        return Err(upstream_error("create issue", format!("status {status}")));
    }

    let issue: GithubApiIssue = resp
        .json()
        .await
        .map_err(|e| upstream_error("decode create", e))?;

    // Record only on success — the quota counts issues actually filed.
    record_issue_filing(db, user_id).await;

    Ok(issue.into())
}
```
Also update the doc-comment above `create_issue_core` to mention the per-user quota (429) in its error list.

- [ ] **Step 4: Update the REST `create_issue` handler to pass db + user_id**

Replace the `create_issue` handler:
```rust
pub async fn create_issue(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<CreateIssueRequest>,
) -> Result<Json<GithubIssueItem>, (StatusCode, String)> {
    let issue =
        create_issue_core(&state.db, user_id, &req.title, req.body.as_deref(), APP_FOOTER).await?;
    Ok(Json(issue))
}
```

- [ ] **Step 5: Update the chat REPORT_ISSUE arm in `rag.rs`**

In `backend/src/rag.rs` (~line 2848), change the call:
```rust
                    match crate::github::create_issue_core(&state.db, user_id, title, body, crate::github::CHAT_FOOTER)
                        .await
```
(The existing `Err((_status, msg)) => mutation_error = Some(format!("I couldn't file that with the team: {}", msg))` already surfaces the 429 message honestly — leave it.)

- [ ] **Step 6: Run the empty-title test to verify it passes; build the crate**

Run: `cd backend && cargo test create_issue_core_rejects_empty_title_before_network`
Expected: PASS.
Run: `cd backend && cargo check`
Expected: builds clean (rag.rs + github.rs compile with the new signature).

- [ ] **Step 7: Add DB-backed `--ignored` tests for the gated core + REST handler 429**

Add test-only imports at the top of `mod tests` (after `use super::*;`) if not already present:
```rust
    use axum::body::Body;
    use axum::routing::post;
    use axum::{middleware, Router};
    use tower::ServiceExt; // oneshot
```
Add the tests:
```rust
    // The gated core returns 429 for BOTH footers (REST APP_FOOTER and chat
    // CHAT_FOOTER) once the user is over quota — short-circuits before any
    // network call, so no GITHUB_TOKEN is required.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_issue_core_rejects_when_over_quota_both_paths() {
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret, is_admin) VALUES ($1, $2, $3, $4)")
            .bind(user).bind(format!("core-{user}@example.test")).bind("s").bind(false)
            .execute(&pool).await.expect("seed user");
        for _ in 0..3 {
            record_issue_filing(&pool, user).await;
        }

        // Chat path footer.
        let chat = create_issue_core(&pool, user, "Bug report", Some("detail"), CHAT_FOOTER)
            .await
            .expect_err("over quota must reject (chat)");
        assert_eq!(chat.0, StatusCode::TOO_MANY_REQUESTS);
        assert!(chat.1.contains("filed several reports"), "msg: {}", chat.1);

        // REST path footer.
        let rest = create_issue_core(&pool, user, "Bug report", Some("detail"), APP_FOOTER)
            .await
            .expect_err("over quota must reject (rest)");
        assert_eq!(rest.0, StatusCode::TOO_MANY_REQUESTS);

        // Do NOT remove_var the rate-limit env vars — see Task 1 Step 8 note.
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await;
    }

    // The REST POST /github/issues handler returns HTTP 429 when over quota,
    // with user_id injected exactly as auth_middleware would (mirrors usage.rs).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_issue_handler_returns_429_when_over_quota() {
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()),
        };

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, totp_secret, is_admin) VALUES ($1, $2, $3, $4)")
            .bind(user_id).bind(format!("h-{user_id}@example.test")).bind("s").bind(false)
            .execute(&pool).await.expect("seed user");
        for _ in 0..3 {
            record_issue_filing(&pool, user_id).await;
        }

        let app = Router::new()
            .route("/github/issues", post(create_issue))
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
                    .method("POST")
                    .uri("/github/issues")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"Bug","body":"detail"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS, "over quota => 429");

        // Do NOT remove_var the rate-limit env vars — see Task 1 Step 8 note.
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }
```

- [ ] **Step 8: Run the new DB tests**

Run: `cd backend && cargo test --lib create_issue_core_rejects_when_over_quota_both_paths create_issue_handler_returns_429_when_over_quota -- --ignored`
Expected: PASS (2 tests).

- [ ] **Step 9: Document the new env vars**

In `backend/.env.example`, near the existing `GITHUB_TOKEN`/`GITHUB_REPO` entries, add:
```
# Per-user GitHub issue-filing rate limit (chat REPORT_ISSUE + /issues-create).
# Optional; defaults shown. Limit/window are clamped to a minimum of 1 (the
# throttle cannot be disabled). Window is in seconds (3600 = 1 hour).
GITHUB_ISSUE_RATE_LIMIT=5
GITHUB_ISSUE_RATE_WINDOW_SECS=3600
```

In `backend/fly.toml`, extend the existing `GITHUB_*` comment block with a note:
```
# GITHUB_ISSUE_RATE_LIMIT / GITHUB_ISSUE_RATE_WINDOW_SECS are OPTIONAL and bound
# how many issues one user can file via chat REPORT_ISSUE or /issues-create in a
# rolling window (defaults: 5 per 3600s). Override via `fly secrets set` if needed.
```

- [ ] **Step 10: Full build + test + commit**

Run: `cd backend && cargo check && cargo test`
Expected: builds clean; all non-ignored tests pass.
Run: `cd backend && cargo test -- --ignored`
Expected: all DB tests pass (local pg running, migration applied).

```bash
git add backend/src/github.rs backend/src/rag.rs backend/.env.example backend/fly.toml
git commit -m "feat(#191): enforce per-user issue quota across REST and chat paths"
```

---

## Spec coverage map
- AC1 (no more than N from either path) → Task 1 quota helper + Task 2 gated core (both footers) + REST handler test.
- AC2 (clear message, no silent drop / false success) → 429 + `RATE_LIMITED_MSG`; chat surfaces via existing `mutation_error`; record only on success.
- AC3 (covered by tests) → pure unit (config, header detector, empty-title) + DB `--ignored` (quota window/isolation, gated core both paths, REST 429).
- "Consider GitHub headers" → `is_github_rate_limited` backstop + unit test.
