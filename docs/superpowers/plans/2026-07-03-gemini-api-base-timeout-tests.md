# GEMINI_API_BASE Test Seam + Timeout-Triggers Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `gemini_api_base()` test seam (mirroring `github_api_base()`/`stripe_api_base()`)
to `backend/src/rag.rs`, thread it through all 4 hardcoded Gemini URL constructions, and add
timeout-triggers tests for the three `generateContent` call sites so the `#249` timeout fix has
a real end-to-end regression guard (`savvagent/nels#269`).

**Architecture:** A single private `fn gemini_api_base() -> String` reading `GEMINI_API_BASE`
(trim/trailing-slash/empty-string handling identical to `github.rs`'s `api_base()`), substituted
into the literal URL prefix at all 4 call sites. Tests use the existing
`TcpListener::bind("127.0.0.1:0")` + throwaway `axum::Router` mock idiom already used elsewhere
in this file/`github.rs`, with a handler that sleeps past the configured timeout before
responding.

**Tech Stack:** Rust (`axum`/`reqwest`/`sqlx`/`cargo test`), backend only — no frontend changes.

---

## Design doc

The full spec (brief, current-state re-verification, assumptions, architecture, error handling,
testing approach, risks) is committed at
`docs/superpowers/specs/2026-07-03-gemini-api-base-timeout-tests-design.md`. Read it before
starting — this plan implements it task-by-task; it does not repeat the rationale. In
particular, re-read Assumptions #3–#6 and #9 before writing the tests (ignored vs non-ignored
split, env-lock serialization, sleep margins).

## File Structure

- Modify: `backend/src/rag.rs` only.
  - Add `gemini_api_base()` (new private fn, placed immediately above `get_gemini_embedding`,
    currently starting at line 358).
  - Thread it into all 4 `format!(...)` URL constructions currently at (approximate, re-verify
    live) lines 373, 1500, 5397, 5678.
  - Add `static GEMINI_API_BASE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());` to
    the `#[cfg(test)] mod tests` block (currently starting at line 5747).
  - Add 3 new tests to that same test module (2 non-ignored, 1 `#[ignore]`d).

No new files, no dependency changes (no wiremock — the existing raw `TcpListener`+`axum::Router`
idiom is reused), no frontend changes.

---

### Task 1: Add `gemini_api_base()` and thread it through all 4 URL sites

**Files:** Modify `backend/src/rag.rs`

- [ ] **Step 1: Re-verify current state**

Run `grep -n "generativelanguage.googleapis.com" backend/src/rag.rs` fresh. Confirm there are
exactly 4 matches (if a 5th has appeared from other merged work since this plan was written,
treat it as in-scope too — the spec's intent is "all hardcoded Gemini URLs", not literally
"exactly 4"). Note the current line numbers; they may have drifted slightly from the spec's
figures (373, 1500, 5397, 5678) due to unrelated intervening commits.

- [ ] **Step 2: Add the `gemini_api_base()` helper**

Immediately above `get_gemini_embedding` (search for `pub(crate) async fn get_gemini_embedding`
if the line number drifted), add:

```rust
/// Base URL for the Gemini API. Overridable via `GEMINI_API_BASE` so tests can point at a
/// local mock server; unset in production, where it defaults to the real API.
/// Whitespace- and trailing-slash-trimmed (mirrors `github.rs`'s `api_base()`); a blank
/// value falls back to the default.
fn gemini_api_base() -> String {
    std::env::var("GEMINI_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string())
}
```

- [ ] **Step 3: Thread it into all 4 URL constructions**

For each of the 4 sites, change the hardcoded prefix to interpolate `gemini_api_base()`. Example
(`get_gemini_embedding`, current shape):

```rust
// before
let url = format!(
    "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent?key={}",
    api_key
);
// after
let url = format!(
    "{}/v1beta/models/gemini-embedding-001:embedContent?key={}",
    gemini_api_base(), api_key
);
```

Apply the identical transform (only the model/action segment differs) at the main chat call site,
`generate_conversation_title`, and `generate_suggested_question` — each currently constructs:
`"https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}"`.

Do not touch anything else in these functions — this is a pure string-construction substitution.

- [ ] **Step 4: Build check**

```bash
cd backend && cargo build
```

Must compile clean. `cargo clippy --all-targets --all-features` should also be run here to catch
an unused-import or similar before moving to Task 2 (no new imports are expected — `std::env` is
already imported at the top of the file).

- [ ] **Step 5: Commit**

```bash
git add backend/src/rag.rs
git commit -m "test(#269): add GEMINI_API_BASE test seam, thread through Gemini call sites"
```

(A `test:` prefix is deliberate: this step alone changes no production behavior when the env
var is unset — matching this repo's existing `test:`-prefixed commits for test-infra-only
changes. Task 2 will use its own commit for the actual new tests.)

---

### Task 2: Add timeout-triggers tests for the three `generateContent` sites

**Files:** Modify `backend/src/rag.rs` (`#[cfg(test)] mod tests` block only)

- [ ] **Step 1: Add the env lock**

In the test module (search `#[cfg(test)]\nmod tests {`), add near the top (after `use super::*;`
and other `use`s):

```rust
// Serializes tests that mutate GEMINI_API_BASE (a process-global env var), mirroring
// github.rs's ISSUE_RATE_ENV_LOCK for the identical risk class — #[tokio::test] tests in this
// binary run concurrently by default, so without this lock two env-mutating tests could race
// each other's set/read. Hold it for the whole test body, not just the set_var call.
static GEMINI_API_BASE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
```

- [ ] **Step 2 (RED): Write `generate_conversation_title_falls_back_to_fallback_title_on_timeout`**

```rust
// #269: proves generate_conversation_title's #249 timeout (20s) actually fires end-to-end —
// not just that a Duration was passed to Client::builder(). A local mock Gemini endpoint
// (via GEMINI_API_BASE) sleeps past the configured timeout before responding; the call must
// return None (its caller's existing fallback is fallback_title) well before the mock ever
// answers. A lazy pool is safe here: record_llm_usage is only reached on the success path,
// which this test never takes.
#[tokio::test]
async fn generate_conversation_title_falls_back_to_fallback_title_on_timeout() {
    let _env = GEMINI_API_BASE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mock = axum::Router::new().fallback(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(25)).await;
            axum::Json(serde_json::json!({}))
        });
        let _ = axum::serve(listener, mock).await;
    });
    std::env::set_var("GEMINI_API_BASE", format!("http://{addr}"));

    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")
        .expect("lazy pool");

    let result = generate_conversation_title(
        "how much did I spend on food",
        "You spent $120 on Food this month.",
        "dummy-test-key",
        &pool,
        uuid::Uuid::new_v4(),
    )
    .await;

    assert!(result.is_none(), "a timed-out Gemini call must yield None, triggering fallback_title");

    std::env::remove_var("GEMINI_API_BASE");
}
```

Run it: `cargo test generate_conversation_title_falls_back_to_fallback_title_on_timeout`
(this is a binary-only crate — no `lib.rs` — so `cargo test --lib` fails with "no library
targets found"; use plain `cargo test <name-filter>`, matching AGENTS.md's documented
`cd backend && cargo test`).
It should compile and pass, taking ~25s (this is a real assertion against real behavior, not a
red/green TDD cycle in the strict sense — the "test" is validating existing production code, so
there's no code change expected to make it pass; if it fails, that's a real finding, not an
expected-red state — see Step 4 below for what a failure would mean).

- [ ] **Step 3: Write `generate_suggested_question_falls_back_to_default_on_timeout`**

Same shape as Step 2, calling `generate_suggested_question(&context, "dummy-test-key", None,
&pool, uuid::Uuid::new_v4())` instead (check the real current signature — it takes a `context:
&str`, `api_key: &str`, `lang: Option<&'static str>`, `pool`, `user_id`). Assert
`result.is_none()`. Use any short context string as the `context` argument.

Run: `cargo test generate_suggested_question_falls_back_to_default_on_timeout`.

- [ ] **Step 4: Run both together to confirm the shared lock serializes them safely**

```bash
cd backend && cargo test timeout
```

(adjust the filter to match both new test names; or just run `cargo test` for the whole default
suite). Expected: both pass, taking roughly 2×25s back-to-back (~50s total) since they share
`GEMINI_API_BASE_ENV_LOCK`. If either test fails or hangs past ~30s, that is a genuine
regression signal (the #249 timeout not actually firing) — do not "fix" the test to avoid this;
investigate the production code (`generate_conversation_title`/`generate_suggested_question`'s
`.timeout(...)` builder) instead. This is the intended regression-detection behavior.

- [ ] **Step 5: Write the ignored main-chat test**

Add, near the other `#[ignore]`d full-`chat_endpoint` tests (e.g. next to
`chat_share_budget_is_owner_only`):

```rust
// #269: end-to-end proof that the main chat call's #249 30s timeout actually fires — the mock
// Gemini endpoint sleeps past it before responding, and chat_endpoint's existing Err(e) branch
// must produce the "communications link is down" fallback response_text. Requires Postgres
// because chat_endpoint resolves the active budget/permissions via real queries before ever
// reaching the Gemini call (same reason every other full-chat_endpoint test in this file is
// #[ignore]d).
#[tokio::test]
#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]
async fn chat_endpoint_falls_back_to_communications_link_down_on_gemini_timeout() {
    let _env = GEMINI_API_BASE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    struct EnvGuard(Option<String>);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("GEMINI_API_KEY", v),
                None => std::env::remove_var("GEMINI_API_KEY"),
            }
        }
    }
    let _env_guard = EnvGuard(std::env::var("GEMINI_API_KEY").ok());
    std::env::set_var("GEMINI_API_KEY", "dummy-test-key");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mock = axum::Router::new().fallback(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(35)).await;
            axum::Json(serde_json::json!({}))
        });
        let _ = axum::serve(listener, mock).await;
    });
    std::env::set_var("GEMINI_API_BASE", format!("http://{addr}"));

    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
    });
    let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");
    let cipher = std::sync::Arc::new(
        crate::crypto::SecretCipher::new(&[9u8; 32]).expect("build test cipher"),
    );
    let state = AppState { db: pool.clone(), cipher };

    let user_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, totp_secret) VALUES ($1, $2, 'test-secret')")
        .bind(user_id)
        .bind(format!("gemini-timeout-{user_id}@example.test"))
        .execute(&pool)
        .await
        .expect("seed user");
    let budget_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO budgets (id, owner_id, name, time_frame, budget_limit, is_default) \
         VALUES ($1, $2, 'gemini-timeout test', 'monthly', 1000.0, TRUE)",
    )
    .bind(budget_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("seed budget");

    let payload = ChatRequest {
        message: "How am I doing this month?".to_string(),
        budget_id: None,
        conversation_id: None,
        locale: None,
    };
    let result = chat_endpoint(
        State(state),
        Extension(user_id),
        Json(payload),
    )
    .await
    .expect("chat_endpoint must not itself error even when Gemini hangs");

    assert!(
        result.0.response.contains("communications link is down"),
        "expected the timeout fallback response text, got: {}",
        result.0.response
    );

    let _ = sqlx::query("DELETE FROM budgets WHERE id = $1").bind(budget_id).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    std::env::remove_var("GEMINI_API_BASE");
}
```

`ChatRequest` (`rag.rs:86-91`) has exactly 4 fields — `message: String`, `budget_id:
Option<Uuid>`, `conversation_id: Option<Uuid>`, `locale: Option<String>` — all must be supplied
(no `Default`), as shown above. `ChatResponse` (`rag.rs:94`) has field `response: String` — NOT
`response_text` (that name belongs only to the internal `AiStructuredResponse`) — accessed as
`result.0.response` since `chat_endpoint` returns `Result<Json<ChatResponse>, ...>` and `Json`
is a tuple struct (`.0` unwraps it), matching `chat_share_budget_is_owner_only`'s existing
precedent. This crate is binary-only (no `lib.rs`), so build/compile checks use plain `cargo
build`/`cargo check --tests`, not `--lib`.

- [ ] **Step 6: Do NOT run the ignored test in this environment unless Postgres is available**

If `podman-compose up -d` (or an equivalent local Postgres) is available in this environment,
run `cargo test -- --ignored chat_endpoint_falls_back_to_communications_link_down_on_gemini_timeout`
to confirm it passes (budget ~35s+). If no local Postgres is available, it is acceptable to ship
this test unverified-by-execution **provided** `cargo build --tests` (or `cargo check --tests`)
compiles it cleanly — matching the bar every other `#[ignore]`d DB test in this repo is held to
at merge time (they are not all re-run per-PR either, per Assumption #6/CI-gate finding in the
spec). Note in the final report whether this test was actually executed against a live Postgres
or only compile-checked.

- [ ] **Step 7: Full local verification**

```bash
cd backend && cargo build && cargo test && cargo clippy --all-targets --all-features
```

All three must be clean. `cargo test` here runs the default (non-ignored) suite, which now
includes the 2 new ~25s tests — expect the full suite's wall time to grow by ~50s versus before
this change.

- [ ] **Step 8: Commit**

```bash
git add backend/src/rag.rs
git commit -m "test(#269): add timeout-trigger tests for Gemini generateContent call sites"
```

---

## Verification Checklist (final, before opening the PR)

- [ ] `grep -n "generativelanguage.googleapis.com" backend/src/rag.rs` shows the literal URL
      appearing exactly once now (inside `gemini_api_base()`'s default), not 4 times.
- [ ] `cargo build`, `cargo test`, `cargo clippy --all-targets --all-features` all clean from
      `backend/`.
- [ ] No file outside `backend/src/rag.rs` is modified.
- [ ] The 2 new non-ignored tests are present in the default `cargo test` run's output (not
      accidentally marked `#[ignore]`).
- [ ] The new ignored test compiles (`cargo test --no-run` or `cargo check --tests` covers this)
      even if not executed against live Postgres.
