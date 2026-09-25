# gemini_narrative reqwest timeout + gemini_api_base() parity — Design Spec

Source: `savvagent/nels#283` — "backend/src/reports.rs: gemini_narrative has no reqwest
timeout (same #249 hang class)". Follow-up to #269 (PR #288, merged), which added
`gemini_api_base()` + timeout tests for the 3 `rag.rs` `generateContent` sites (and threaded
`get_gemini_embedding`'s `embedContent` site through it too), but was scoped to `rag.rs` only —
#283 was filed during #269's plan-critique as the 5th, still-unfixed hardcoded-URL / no-timeout
call site.

## Brief (verbatim from the issue)

> backend/src/reports.rs's gemini_narrative (called from generate_narrative, ~line 503-510)
> builds a bare reqwest::Client::new() with no .timeout() at all, and hardcodes
> https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}
> directly. This is the identical hang class #249 fixed for the three rag.rs call sites... a
> stalled connection here can hang indefinitely, with no bound.
>
> Suggested fix: give gemini_narrative's reqwest::Client::builder() an explicit .timeout(...),
> mirroring get_gemini_embedding's precedent in rag.rs; and thread it through gemini_api_base()
> (or an equivalent) once #269 lands, for URL-construction test-seam parity.
>
> Suggested scope:
> 1. Timeout on gemini_narrative's Client::builder().
> 2. Thread reports.rs's URL construction through gemini_api_base().
> 3. Consider a timeout-trigger test mirroring #269's pattern, if it fits cleanly.

## Current state (verified fresh against `origin/main`, this branch)

- `backend/src/reports.rs:503-508` — `gemini_narrative(facts: &str, api_key: &str) -> Option<String>`:
  `let client = reqwest::Client::new();` (no `.timeout()`), and
  `format!("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}", api_key)`.
  Called only from `generate_narrative` (`reports.rs:454-467`). `generate_narrative` reads
  `GEMINI_API_KEY` directly (`reports.rs:461`) and early-returns the deterministic `template`
  when `!use_ai || api_key.is_empty()` (`reports.rs:462-464`) — confirmed at those exact lines.
  Neither function takes a `pool`/DB argument — `gemini_narrative`'s whole call chain is
  entirely DB-free (unlike the 3 sites #269 fixed in `rag.rs`, none of which call
  `record_llm_usage` on this path either since `generate_narrative` has no pool/user_id args).
  On any `None` return, `generate_narrative` falls back to `template` — the same tolerant
  fallback shape #249/#269 preserved for the `rag.rs` sites.
- `backend/src/rag.rs:361-367` (merged by #269) — `gemini_api_base() -> String`, a **private**
  `fn` (no `pub`/`pub(crate)`), reading `GEMINI_API_BASE`, trimming, defaulting to
  `"https://generativelanguage.googleapis.com"`. Not currently callable from `reports.rs`
  (sibling module) — needs visibility widened.
- `backend/src/rag.rs:355-367`'s doc comment and #269's spec both establish the
  `get_gemini_embedding` precedent for the timeout builder shape:
  ```rust
  let client = reqwest::Client::builder()
      .timeout(std::time::Duration::from_secs(20))
      .build()
      .unwrap_or_else(|_| reqwest::Client::new());
  ```
  `get_gemini_embedding` (an `embedContent` call) and `rag.rs`'s `generate_conversation_title`/
  `generate_suggested_question` (both single-shot `generateContent` calls, structurally the
  closest analogue to `gemini_narrative`) all use **20s**. Only the heavier main `chat_endpoint`
  call (which does multi-step context loading before the Gemini call) uses 30s.
- `backend/src/rag.rs:5758-5777` (merged by #269) — inside `#[cfg(test)] mod tests`, a
  process-global `static GEMINI_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());`
  serializes every test in `rag.rs` that mutates `GEMINI_API_BASE`/`GEMINI_API_KEY`, since
  `#[tokio::test]`s run concurrently by default in the same binary and both are process-global
  env vars. It is currently declared **inside** `rag::tests` (private module, private static) —
  not reachable from `reports.rs`'s own `#[cfg(test)] mod tests` as-is.
- `backend/src/reports.rs:621-790` already has a `#[cfg(test)] mod tests { use super::*; ... }`
  with only pure unit tests (no async/DB/mock-server tests yet).
- `backend/Cargo.toml` already depends on `tokio` (full), `axum` (router+serve), `reqwest`
  (json feature) — no new dependency needed; same mock-server idiom (`TcpListener::bind` +
  throwaway `axum::Router` + `axum::serve`) #269 used in `rag.rs` applies verbatim.
- `AGENTS.md` §2 documents the `GEMINI_API_BASE` seam as covering "All 4 hardcoded Gemini call
  sites in rag.rs" — this is now inaccurate once `reports.rs` is threaded through the same
  helper and should be updated to mention the 5th site.
- A concurrent, unrelated ticket (#282) is being worked in a separate worktree and touches
  `rag.rs` (a different, non-overlapping bug in single-category balance reporting) — this
  spec's `rag.rs` touch is limited to two small, additive visibility changes (below), to
  minimize rebase/conflict surface.

## Assumptions

1. **Timeout duration: 20s**, matching `get_gemini_embedding`/`generate_conversation_title`/
   `generate_suggested_question` — `gemini_narrative` is a single lightweight `generateContent`
   call (a short "give a 2-3 sentence narrative" prompt), structurally identical in shape/cost
   to those three, not to the heavier 30s main-chat call. Builder failure falls back to
   `reqwest::Client::new()` (unbounded), mirroring `get_gemini_embedding`'s exact `unwrap_or_else`
   shape — accepted as the established precedent's own tradeoff.
2. **`gemini_api_base()` is widened to `pub(crate)` in place, not moved to a new shared module.**
   The issue explicitly suggests this. Moving it would touch `rag.rs` call sites that already
   reference it as a bare (unqualified) `fn` call — a larger, riskier diff for no behavioral
   gain. `reports.rs` calls it as `crate::rag::gemini_api_base()`.
3. **`GEMINI_ENV_LOCK` is relocated one level up** — from a declaration inside `rag::tests` to a
   top-level `#[cfg(test)] pub(crate) static GEMINI_ENV_LOCK` in `rag.rs` proper (immediately
   above the `#[cfg(test)] mod tests` block), keeping its exact type/initializer. Every existing
   reference inside `rag::tests` continues to resolve unchanged (that module already does
   `use super::*;`). This is a smaller, more surgical change than making the entire `tests`
   module `pub(crate)` (which would also leak `GeminiApiBaseGuard` and other rag-test-only
   helpers crate-wide) — minimizes both the blast radius of exposing test-only internals and
   the conflict surface with #282's concurrent `rag.rs` work. `reports.rs`'s test references it
   as `crate::rag::GEMINI_ENV_LOCK`.
4. **`gemini_narrative` gets a new, non-`#[ignore]`d timeout-trigger test.** Unlike the `rag.rs`
   sites, `gemini_narrative`/`generate_narrative` are entirely pool/DB-free, so there is no
   Postgres dependency to gate on here — it runs in the default `cargo test`, following the
   exact same mock-server idiom and the same `GEMINI_ENV_LOCK`-guarded env-var-mutation
   discipline #269 established, reusing the widened lock from Assumption #3 to avoid
   reintroducing the cross-test env-var flake class #269 fixed. Mock sleeps 25s (5s past the
   20s timeout, matching #269's own margin choice). Test asserts `generate_narrative(...).await`
   returns the exact `template_narrative` output (the `None`-triggered fallback), not merely
   "not None," and that elapsed time falls in `[19s, 23s)` — a two-sided window, not a bare
   lower bound. The lower bound proves the real ~20s timeout elapsed (not an instant connection
   error); the upper bound is the actual discriminator between fixed/unfixed behavior: with no
   client timeout, the request is bounded only by the mock's own 25s sleep and lands at ~25s+,
   failing the upper-bound check. A lower-bound-only assertion would keep passing even if the
   `.timeout(20s)` call were later silently deleted — caught during plan critique.
5. **Threading is a pure string substitution, no behavior change to production URL
   construction.** With `GEMINI_API_BASE` unset (prod/default-dev), `gemini_api_base()` returns
   the exact literal `reports.rs` already hardcodes today. Only the *timeout* is a genuine
   behavior change (bounding a previously-unbounded call) — the entire point of this ticket.
6. **AGENTS.md §2's `GEMINI_API_BASE` bullet is updated** to mention `reports.rs`'s
   `gemini_narrative` as a 5th threaded call site (small doc-only addition), mirroring #269's
   own PR, which added this documentation when it introduced the seam.
7. **No change to `gemini_narrative`'s error-handling/fallback shape.** The existing
   `Ok(resp) if success` / `Ok(resp)` (non-2xx) / `Err(e)` match arms are untouched; a timeout
   surfaces as `Err(e)`, which already returns `None` via the existing
   `Err(e) => { tracing::error!(...); None }` arm — no new branch needed.

## Goal & Success Criteria

Close the last of the 5 hardcoded/no-timeout Gemini `generateContent` call sites the
#249/#268/#269 line of fixes has been working through, and give `reports.rs` the same
`GEMINI_API_BASE` test-seam parity `rag.rs` already has.

- [ ] `gemini_narrative`'s `reqwest::Client` is built via
      `Client::builder().timeout(Duration::from_secs(20)).build()...`, mirroring
      `get_gemini_embedding`.
- [ ] `gemini_narrative`'s URL construction is threaded through `crate::rag::gemini_api_base()`;
      with `GEMINI_API_BASE` unset, the constructed URL is byte-for-byte identical to today's
      hardcoded literal.
- [ ] `gemini_api_base()` is `pub(crate)` in `rag.rs`; no other `rag.rs` call site's behavior
      changes.
- [ ] A new non-`#[ignore]`d test in `reports.rs` proves `generate_narrative` falls back to
      `template_narrative`'s output when the mocked Gemini endpoint holds the connection past
      the 20s timeout, and that ≥19s actually elapsed.
- [ ] `GEMINI_ENV_LOCK` is reused (not duplicated) across `rag.rs` and `reports.rs` tests.
- [ ] `AGENTS.md` §2 mentions the 5th (`reports.rs`) call site.
- [ ] `cd backend && cargo build && cargo test && cargo clippy --all-targets --all-features` all
      pass.
- [ ] No other call site, error-handling branch, or fallback text is modified.

## Scope

**In scope:** `backend/src/reports.rs` (the timeout + URL threading + new test);
`backend/src/rag.rs` (two small, additive visibility changes: `gemini_api_base()` →
`pub(crate)`, `GEMINI_ENV_LOCK` relocated one level up + `pub(crate)`); `AGENTS.md` §2 (doc
addition).

**Out of scope:** any change to `rag.rs`'s own call sites/tests beyond the two visibility
changes; an env-var seam for the timeout *duration* itself; retry/circuit-breaker logic; the
frontend; `GITHUB_API_BASE`/`STRIPE_API_BASE`.

## Architecture

```rust
// reports.rs — production code
async fn gemini_narrative(facts: &str, api_key: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!(
        "{}/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        crate::rag::gemini_api_base(), api_key
    );
    // ...unchanged below (identical match arms)...
}
```

```rust
// rag.rs — visibility only, no logic change
pub(crate) fn gemini_api_base() -> String { /* unchanged body */ }

#[cfg(test)]
pub(crate) static GEMINI_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
// moved from inside `mod tests`; doc comment kept, extended to note reports.rs also
// reuses this lock (same GEMINI_API_BASE/GEMINI_API_KEY race class)

#[cfg(test)]
mod tests {
    use super::*; // GEMINI_ENV_LOCK still resolves here unchanged
    // ...existing tests unchanged...
}
```

```rust
// reports.rs test module — one complete test, both env vars covered by one guard
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
    // Both vars set under the same lock; both cleared by one Drop guard (covers the
    // panic-mid-assert case, matching rag.rs's GeminiApiBaseGuard rationale).
    std::env::set_var("GEMINI_API_BASE", format!("http://{addr}"));
    std::env::set_var("GEMINI_API_KEY", "dummy-test-key"); // non-empty: takes the live-call branch
    let _guard = GeminiEnvGuard;

    let s = Summary { income: 1000.0, expense: 400.0, savings: 100.0, net: 500.0 };
    let cats: Vec<CategoryRow> = vec![];
    let expected_template = template_narrative(&s, &cats, "this_month");

    let started = std::time::Instant::now();
    let narrative = generate_narrative(&s, &cats, "this_month", true).await;
    let elapsed = started.elapsed();

    assert_eq!(narrative, expected_template, "a timed-out Gemini call must fall back to the template narrative");
    // Two-sided window: the lower bound proves the real ~20s timeout elapsed (not an instant
    // connection error); the upper bound is the actual discriminator between fixed/unfixed —
    // with NO client timeout the call would be bounded only by the mock's own 25s sleep and
    // land at ~25s+, failing this assertion. A lower-bound-only check would keep passing even
    // if `.timeout(20s)` were later accidentally removed.
    assert!(
        elapsed >= std::time::Duration::from_secs(19) && elapsed < std::time::Duration::from_secs(23),
        "expected the 20s timeout to fire (19s <= elapsed < 23s), got {elapsed:?}"
    );
    mock_handle.abort();
}
```

## Error Handling & Edge Cases

- A timeout manifests as `Err(e)` in `gemini_narrative`'s existing match — already handled,
  already logs at `tracing::error!` and returns `None`. No new error path.
- `Client::builder().build()` failure (near-impossible in this codebase) falls back to an
  unbounded `Client::new()`, matching `get_gemini_embedding`'s own accepted precedent/tradeoff.
- Env var hygiene: the new test's `Drop`-based guard restores/removes **both**
  `GEMINI_API_BASE` and `GEMINI_API_KEY` even on assertion panic, avoiding leaking state into
  whichever test acquires `GEMINI_ENV_LOCK` next.

## Testing Approach

- `cd backend && cargo test` (default suite) exercises the new non-ignored timeout test plus the
  full existing suite (unchanged, since no other behavior changed).
- `cd backend && cargo clippy --all-targets --all-features` and `cargo build` must stay clean.
- No frontend changes, no migrations — no other test/build step needed.

## Risks & Open Questions

- **Wall-clock cost**: the new test adds ~25s to `cargo test` (a real, un-shortened timeout
  wait), same accepted tradeoff #269 already established for its sibling tests.
- **Cross-module test lock reuse (`GEMINI_ENV_LOCK`) widens rag.rs's test-only surface to
  `pub(crate)`.** Deliberate, minimal exception to normal test-privacy — the alternative (a
  second, parallel lock) would silently reintroduce exactly the cross-test-env-var race class
  #269 was created to close, the moment both test suites run concurrently under one
  `cargo test` invocation. Limiting the exposed surface to the lock alone (not the whole `tests`
  module) keeps the blast radius small.
- **Possible rebase**: a concurrent, unrelated ticket (#282) is also touching `rag.rs` in this
  timeframe. Since this spec's `rag.rs` touch is two small, additive, non-overlapping visibility
  changes near `gemini_api_base()`/the test module boundary, a conflict is unlikely but a rebase
  may still be needed depending on exactly what #282 lands.
