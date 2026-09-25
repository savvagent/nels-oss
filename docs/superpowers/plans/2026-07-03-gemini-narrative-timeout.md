# gemini_narrative Timeout + gemini_api_base() Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound `backend/src/reports.rs`'s `gemini_narrative` `reqwest` call with an explicit
20s timeout (closing the last of the #249 hang-class call sites) and thread its URL
construction through `rag.rs`'s `gemini_api_base()` test seam, adding a genuine
timeout-trigger regression test.

**Architecture:** Two small, additive visibility changes in `backend/src/rag.rs`
(`gemini_api_base()` → `pub(crate)`, `GEMINI_ENV_LOCK` relocated one level up + `pub(crate)`),
one behavior change + one URL-threading change in `backend/src/reports.rs::gemini_narrative`,
one new non-`#[ignore]`d test reusing the relocated lock and the existing mock-server idiom,
and a one-line `AGENTS.md` doc update.

**Tech Stack:** Rust, `reqwest` 0.12 (`json` feature), `tokio` 1.38 (full), `axum` 0.7.5
(router + serve), `sqlx` (unused by this change — `generate_narrative` is pool-free).

**Source:** `savvagent/nels#283`. Spec: `docs/superpowers/specs/2026-07-03-gemini-narrative-timeout-design.md`.

**Repo-specific reminders (from AGENTS.md / repo conventions):**
- Test command: `cd backend && cargo test` (default suite must include the new test, no
  Postgres needed for this ticket). Lint: `cd backend && cargo clippy --all-targets --all-features`.
  Build: `cd backend && cargo build`.
- Commit format observed in this repo's history: `<type>(#<issue>): <subject>` (e.g.
  `feat(#285): ...`). Use `fix(#283): ...` for this ticket's commits.
- No migrations, no config/secrets, no infra-as-code, no feature flags in this change —
  vacuously satisfied, not applicable.
- Deploy is release-gated (release-please batches backend commits into a release PR; merging
  to `main` does not deploy on its own) — no per-ticket deploy action needed beyond merging.

---

### Task 1: Widen `gemini_api_base()` visibility and relocate `GEMINI_ENV_LOCK`

**Files:**
- Modify: `backend/src/rag.rs:361-367` (the `gemini_api_base()` function)
- Modify: `backend/src/rag.rs:5758-5777` (the `GEMINI_ENV_LOCK` static + its doc comment,
  currently inside `#[cfg(test)] mod tests { ... }`)

This task only changes visibility/location, not logic — no new test is needed for it in
isolation (Task 2's test exercises `gemini_api_base()` cross-module, Task 3's test exercises
`GEMINI_ENV_LOCK` cross-module; both are the regression coverage for this task).

- [ ] **Step 1: Widen `gemini_api_base()` to `pub(crate)`**

Read the current function first to confirm the exact text to replace:

```bash
cd backend && sed -n '357,368p' src/rag.rs
```

Expected output (comment + function, currently a bare private `fn`):

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

Replace the `fn gemini_api_base() -> String {` line with:

```rust
/// Base URL for the Gemini API. Overridable via `GEMINI_API_BASE` so tests can point at a
/// local mock server; unset in production, where it defaults to the real API.
/// Whitespace- and trailing-slash-trimmed (mirrors `github.rs`'s `api_base()`); a blank
/// value falls back to the default.
///
/// `pub(crate)` (widened by #283) so `reports.rs`'s `gemini_narrative` can thread its URL
/// construction through the same seam — see AGENTS.md §2.
pub(crate) fn gemini_api_base() -> String {
    std::env::var("GEMINI_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string())
}
```

(Only the doc comment gained 2 lines and `fn` became `pub(crate) fn`; the body is byte-for-byte
unchanged.)

- [ ] **Step 2: Relocate `GEMINI_ENV_LOCK` one level up, out of `mod tests`**

Read the current declaration to confirm exact text:

```bash
grep -n "GEMINI_ENV_LOCK\|^mod tests\|^#\[cfg(test)\]" src/rag.rs | sed -n '1,3p'
```

You should see `#[cfg(test)]` immediately followed by `mod tests {` around line 5758-5759, and
the `static GEMINI_ENV_LOCK` declaration a few lines inside that module (~line 5777), preceded
by a multi-paragraph doc comment starting "Serializes tests that mutate GEMINI_API_BASE...".

Cut the **entire comment block + the `static GEMINI_ENV_LOCK` line** out of `mod tests` (leave
`use super::*;` and everything after the static, e.g. `GeminiApiBaseGuard`, in place inside
`mod tests`), and paste it **immediately above** the `#[cfg(test)] mod tests {` line, at
top-level `rag.rs` module scope, with `static` changed to `pub(crate) static` and one sentence
appended to the doc comment. The result immediately above `#[cfg(test)] mod tests {` should
read:

```rust
// Serializes tests that mutate GEMINI_API_BASE and/or GEMINI_API_KEY (both process-global env
// vars), mirroring github.rs's ISSUE_RATE_ENV_LOCK for the identical risk class —
// #[tokio::test] tests in this binary run concurrently by default under `cargo test --
// --ignored`, so without a shared lock, two env-mutating tests could race each other's
// set/read. Hold it for the whole test body, not just the set_var call.
//
// #269 added this lock (originally scoped to GEMINI_API_BASE alone) alongside a new
// #[ignore]d test that, uniquely among this file's chat_endpoint tests, sets GEMINI_API_KEY
// to a *non-empty* value for its ~90s body. Every OTHER ignored chat_endpoint test in this
// file (e.g. chat_share_budget_is_owner_only and its siblings below) force-clears
// GEMINI_API_KEY via its own local EnvGuard specifically to guarantee deterministic offline
// routing — so all of them must also take this same lock, or a concurrent run under
// `cargo test -- --ignored` could let one of them observe the new test's truthy override
// mid-flight and silently take the live-call branch instead. Any test in this file that reads
// or mutates either var — new or existing — must acquire this lock as its first statement.
//
// #283 widened this to `pub(crate)` and moved it to module scope (from inside `mod tests`
// below) so `reports.rs`'s own test module can reuse the exact same lock for its
// `gemini_narrative` timeout test — a second, parallel lock would silently reintroduce the
// cross-test env-var race this lock exists to prevent.
#[cfg(test)]
pub(crate) static GEMINI_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    // Panic-safe restore of GEMINI_API_BASE, paired with GEMINI_ENV_LOCK: a plain
    // remove_var() at the end of a test body never runs if an earlier assert! panics, leaking
    // the var (and its now-orphaned mock server) into whichever test acquires the lock next.
    // Dropping this at the end of scope (including on unwind) always clears it.
    struct GeminiApiBaseGuard;
    impl Drop for GeminiApiBaseGuard {
        fn drop(&mut self) {
            std::env::remove_var("GEMINI_API_BASE");
        }
    }
    // ...rest of mod tests unchanged (all existing `GEMINI_ENV_LOCK.lock()` references
    // resolve unchanged via `use super::*;`)...
```

- [ ] **Step 3: Build to confirm the relocation compiles and every existing reference still resolves**

```bash
cd backend && cargo build 2>&1 | tail -30 && cargo test --lib rag:: 2>&1 | tail -20
```

Expected: clean build, all pre-existing `rag.rs` tests still pass (the ones using
`GEMINI_ENV_LOCK` and `gemini_api_base()` are unaffected — same names, same types, just
resolved from one scope higher via the existing glob import).

- [ ] **Step 4: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#283): widen gemini_api_base() + GEMINI_ENV_LOCK to pub(crate) for reports.rs reuse"
```

---

### Task 2: Give `gemini_narrative` an explicit timeout and thread its URL through `gemini_api_base()`

**Files:**
- Modify: `backend/src/reports.rs:503-508` (the `gemini_narrative` function body)

- [ ] **Step 1: Write the failing test first (TDD)**

Add this test at the end of `backend/src/reports.rs`'s existing `#[cfg(test)] mod tests { ... }`
block (after the last test, `template_includes_totals_and_flags_over_80pct`, right before the
module's closing `}`):

```rust
    // #283: proves gemini_narrative's #249-class hang is now bounded by a real 20s timeout —
    // not just that a Duration was passed to Client::builder(). A local mock Gemini endpoint
    // (via GEMINI_API_BASE) sleeps past the configured timeout before responding; the call
    // must fall back to the exact template_narrative output well before the mock ever answers.
    // generate_narrative/gemini_narrative are entirely pool/DB-free, so no lazy pool or
    // Postgres is needed — this test runs in the default `cargo test`.
    struct GeminiEnvGuard;
    impl Drop for GeminiEnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("GEMINI_API_BASE");
            std::env::remove_var("GEMINI_API_KEY");
        }
    }

    #[tokio::test]
    async fn gemini_narrative_falls_back_to_template_on_timeout() {
        let _env = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock_handle = tokio::spawn(async move {
            let mock = axum::Router::new().fallback(|| async {
                tokio::time::sleep(std::time::Duration::from_secs(25)).await;
                axum::Json(serde_json::json!({}))
            });
            if let Err(e) = axum::serve(listener, mock).await {
                eprintln!("mock Gemini server error: {e}");
            }
        });
        std::env::set_var("GEMINI_API_BASE", format!("http://{addr}"));
        std::env::set_var("GEMINI_API_KEY", "dummy-test-key");
        let _guard = GeminiEnvGuard;

        let s = Summary {
            income: 1000.0,
            expense: 400.0,
            savings: 100.0,
            net: 500.0,
        };
        let cats: Vec<CategoryRow> = vec![];
        let expected_template = template_narrative(&s, &cats, "this_month");

        let started = std::time::Instant::now();
        let narrative = generate_narrative(&s, &cats, "this_month", true).await;
        let elapsed = started.elapsed();

        assert_eq!(
            narrative, expected_template,
            "a timed-out Gemini call must fall back to the template narrative"
        );
        // Lower bound proves the real ~20s timeout elapsed (not an instant connection error).
        // Upper bound is the actual discriminator between fixed/unfixed: with NO client
        // timeout, the call is bounded only by the mock's own 25s sleep and elapsed would land
        // at ~25s+, failing this assertion — that's what makes this a genuine regression guard
        // rather than a check that would keep passing even if the `.timeout(20s)` call were
        // later accidentally deleted.
        assert!(
            elapsed >= std::time::Duration::from_secs(19)
                && elapsed < std::time::Duration::from_secs(23),
            "expected the 20s timeout to fire (19s <= elapsed < 23s), got {elapsed:?}"
        );
        mock_handle.abort();
    }
```

- [ ] **Step 2: Run the test to confirm it fails**

Before the fix, `gemini_narrative` still builds a bare `reqwest::Client::new()` with no
`.timeout()`, so nothing bounds the request except the mock's own 25s `sleep`. The request
therefore completes in ~25-26s and (since `GenResponse::candidates` fails to deserialize from
the mock's `{}` body either way) `generate_narrative` already falls back to `template` — so
`assert_eq!(narrative, expected_template, ...)` passes regardless. The RED signal is the
**upper-bound** assertion: `elapsed < 23s` fails because the real elapsed time is ~25-26s, not
because the test hangs forever.

Run with a hard wall-clock cap as a safety net in case something is unexpectedly different:

```bash
cd backend && timeout 40 cargo test --lib gemini_narrative_falls_back_to_template_on_timeout -- --nocapture
echo "exit code: $?"
```

Expected: the test itself **FAILS** (`test ... FAILED`, `cargo test` exits non-zero, around
~25-26s in) with the assertion message `expected the 20s timeout to fire (19s <= elapsed < 23s), got ~25.0s` (or similar) — confirming the pre-fix call is bounded only by the mock's sleep,
not by any client-side timeout. This is the "RED" state.

- [ ] **Step 3: Implement the minimal fix — add the timeout and thread the URL through `gemini_api_base()`**

Read the current function to confirm exact text:

```bash
sed -n '503,509p' src/reports.rs
```

Expected:

```rust
async fn gemini_narrative(facts: &str, api_key: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );
```

Replace those 6 lines with:

```rust
async fn gemini_narrative(facts: &str, api_key: &str) -> Option<String> {
    // #283: bound the call — a stalled Gemini connection must not hang generate_narrative
    // indefinitely, mirroring get_gemini_embedding's #249 precedent in rag.rs. A build()
    // failure (near-impossible here) falls back to an unbounded client, same tradeoff as
    // that precedent.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!(
        "{}/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        crate::rag::gemini_api_base(),
        api_key
    );
```

Do not change anything below this (the `prompt`, `body`, and the `match client.post(&url)...`
block stay exactly as they are).

- [ ] **Step 4: Run the test to confirm it passes**

```bash
cd backend && timeout 40 cargo test --lib gemini_narrative_falls_back_to_template_on_timeout -- --nocapture
```

Expected: `test reports::tests::gemini_narrative_falls_back_to_template_on_timeout ... ok`,
completing in ~20-21s (well under the 40s wall-clock cap), confirming the 20s timeout fired and
`generate_narrative` returned the exact template fallback.

- [ ] **Step 5: Run the full backend test suite + clippy + build to confirm nothing else broke**

```bash
cd backend && cargo build 2>&1 | tail -20
cargo test 2>&1 | tail -20
cargo clippy --all-targets --all-features 2>&1 | tail -40
```

Expected: `cargo build` clean; `cargo test` shows one more passing test than before this task
(no `FAILED`); `cargo clippy` has no new errors (pre-existing warning classes are fine, matching
#269's own precedent of accepted `await_holding_lock` warnings on this exact lock).

- [ ] **Step 6: Commit**

```bash
git add backend/src/reports.rs
git commit -m "fix(#283): bound gemini_narrative's reqwest client with a 20s timeout"
```

---

### Task 3: Document the 5th call site in AGENTS.md

**Files:**
- Modify: `AGENTS.md:23` (the `Testability seam (#269)` bullet in §2 "AI & RAG Embeddings")

- [ ] **Step 1: Read the current bullet to confirm exact text**

```bash
grep -n "Testability seam (#269)" AGENTS.md
```

Expected line 23:

```
- **Testability seam (#269)**: `GEMINI_API_BASE` overrides the Gemini API base URL (defaults to `https://generativelanguage.googleapis.com` when unset/blank), mirroring `GITHUB_API_BASE`/`STRIPE_API_BASE`. All 4 hardcoded Gemini call sites in `rag.rs` (`get_gemini_embedding`, the main chat call, `generate_conversation_title`, `generate_suggested_question`) resolve their URL through `gemini_api_base()`, so tests can point them at a local mock server (e.g. to trigger the `#249` `reqwest` client timeouts end-to-end). Must remain unset in production.
```

- [ ] **Step 2: Replace with the updated bullet (adds the 5th, `reports.rs` site + the
      `pub(crate)`/lock-reuse note)**

```
- **Testability seam (#269, extended #283)**: `GEMINI_API_BASE` overrides the Gemini API base URL (defaults to `https://generativelanguage.googleapis.com` when unset/blank), mirroring `GITHUB_API_BASE`/`STRIPE_API_BASE`. All 5 hardcoded Gemini call sites resolve their URL through `rag.rs`'s `gemini_api_base()` (now `pub(crate)`): 4 in `rag.rs` (`get_gemini_embedding`, the main chat call, `generate_conversation_title`, `generate_suggested_question`) plus `reports.rs`'s `gemini_narrative` (#283, which also gained its own explicit 20s `reqwest` timeout — the last of the `#249` hang-class sites). Tests can point any of them at a local mock server; `rag.rs`'s test-only `GEMINI_ENV_LOCK` (also `pub(crate)`, #283) is shared across both files' test modules to serialize env-var-mutating tests. Must remain unset in production.
```

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs(#283): document reports.rs's 5th GEMINI_API_BASE call site"
```

---

### Task 4: Final full-suite verification

**Files:** none (verification only)

- [ ] **Step 1: Full clean verification run**

```bash
cd backend && cargo build 2>&1 | tail -20
cargo test 2>&1 | tail -30
cargo clippy --all-targets --all-features 2>&1 | tail -40
```

Expected: `cargo build` clean; `cargo test` all pass (0 failed), including the new
`gemini_narrative_falls_back_to_template_on_timeout` in the non-ignored count, total wall time
increased by ~20-21s versus pre-change baseline; `cargo clippy` clean (pre-existing warning
classes only, no new errors).

- [ ] **Step 2: Confirm no other Gemini call sites were missed**

```bash
grep -rn "generativelanguage.googleapis.com" backend/src/
```

Expected: the literal URL now appears **only** inside `gemini_api_base()`'s own
`unwrap_or_else` default string in `rag.rs` — zero other hardcoded occurrences in
`reports.rs` or elsewhere.

- [ ] **Step 3: Confirm `git log` shows exactly 3 commits on this branch (Tasks 1-3; Task 4 has
      no commit, verification only)**

```bash
git log --oneline origin/main..HEAD
```

Expected: 3 commits, in order: the `rag.rs` visibility commit, the `reports.rs` timeout+URL
commit, the `AGENTS.md` doc commit.
