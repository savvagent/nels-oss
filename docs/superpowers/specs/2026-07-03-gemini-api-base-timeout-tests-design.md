# GEMINI_API_BASE Test Seam + Timeout-Triggers Tests — Design Spec

Source: `savvagent/nels#269` — "Add GEMINI_API_BASE test seam + timeout-triggers tests for
Gemini call sites". Follow-up to `#249`/`#268`, originating from the aggregated automated
review on PR #268 (Important finding #4:
https://github.com/savvagent/nels/pull/268#issuecomment-4879418088).

## Brief (verbatim from the issue)

> Context: #249/#268 added explicit reqwest client timeouts to the three backend Gemini
> generateContent call sites in backend/src/rag.rs (main chat, generate_conversation_title,
> generate_suggested_question) and bounded the two frontend raw-fetch() sites in App.svelte
> (downloadExport, setDefaultBudget) via fetchWithTimeout. None of the five call sites have a
> test that actually triggers a timeout end-to-end — the backend timeout builders are exercised
> only indirectly (via existing chat-flow tests that don't stall), and there's no test asserting
> a slow/hanging Gemini response is bounded by the configured Duration.
>
> The pr-test-analyzer automated review on #268 found this repo already has a precedented
> pattern for exactly this:
> - GITHUB_API_BASE (backend/src/github.rs) and STRIPE_API_BASE (backend/src/billing.rs) —
>   env-var-overridable base URLs so tests can redirect outbound HTTP calls to a local mock.
> - A local mock-server test idiom already used in backend/src/rag.rs
>   (chat_report_issue_writes_user_scoped_audit_with_no_active_budget, ~line 10241) and
>   backend/src/github.rs (~line 973): tokio::net::TcpListener::bind("127.0.0.1:0") + a
>   throwaway axum::Router + axum::serve, with the base URL env var pointed at it.
>
> The same shape would let a test stand up a mock Gemini endpoint that never responds (or
> responds after a controlled delay), point a GEMINI_API_BASE env var at it, and assert the
> call actually returns/errors within the configured timeout window instead of hanging — a real
> regression guard for the #249 fix, not just a static assertion that a Duration was passed to
> Client::builder().
>
> Suggested scope:
> 1. Add a gemini_api_base() helper (mirroring github_api_base()/stripe_api_base()) and thread
>    it through all Gemini call sites' URL construction.
> 2. Add timeout-triggers tests for at least the three generateContent call sites, using the
>    TcpListener::bind("127.0.0.1:0") + axum::Router mock idiom, with a handler that
>    intentionally holds the connection open past the configured timeout — asserting the call
>    site's existing fallback behavior (fallback_title, DEFAULT_SUGGESTED_QUESTION, the main
>    chat "communications link is down" response) fires instead of the request hanging.

The issue flags that line numbers may have drifted from other tickets touching `rag.rs`
(#228 fund categories, #258 delete transactions, #262 billing.rs) since filing. Everything
below is re-verified fresh against the current branch.

## Current state (verified fresh against this branch)

Exactly **4** hardcoded `https://generativelanguage.googleapis.com` URLs exist in
`backend/src/rag.rs` (the issue's "5" is not a literal count — it enumerates the 4 named sites
"plus wherever else" as a safety net; `grep -n generativelanguage.googleapis.com
backend/src/rag.rs` confirms exactly 4, no others):

1. **`get_gemini_embedding`** (`rag.rs:358-419`) — `embedContent`, 20s timeout
   (`rag.rs:368-371`). Called from the create-transaction write path and
   `resolve_transaction_by_locator`. On any failure returns `None` — callers already tolerate a
   `NULL` embedding.
2. **Main chat call**, inline in `chat_endpoint` (`rag.rs:1495-1604`) — `generateContent`, 30s
   timeout (`rag.rs:1495-1498`). On a `send()` error, the `Err(e)` arm (`rag.rs:1595-1603`)
   produces `AiStructuredResponse { response_text: "My communications link is down. Please
   verify internet connectivity.", .. }`.
3. **`generate_conversation_title`** (`rag.rs:5380-5455`) — `generateContent`, 20s timeout
   (`rag.rs:5392-5395`). Uses `.ok()?` (`rag.rs:5417-5423`) so any send failure short-circuits
   to `None`; its only caller falls back to `fallback_title(first_user_msg)`
   (`rag.rs:5368-5376`, first ~6 words of the user's first message).
4. **`generate_suggested_question`** (`rag.rs:5663-5744`) — `generateContent`, 20s timeout
   (`rag.rs:5673-5676`). Same `.ok()?` short-circuit shape; its only caller
   (`suggested_question`, `rag.rs:5624-5659`) falls back to `DEFAULT_SUGGESTED_QUESTION`
   (`rag.rs:5616`, `"How am I tracking against my budget this month?"`).

**Existing test-seam precedent** (both already documented in `AGENTS.md` §10/§11):
- `GITHUB_API_BASE` — `github.rs:109-120`, `api_base()`: `std::env::var(...).ok().map(|s|
  s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).unwrap_or_else(||
  GITHUB_API.to_string())`.
- `STRIPE_API_BASE` — `billing.rs:210-215`, a generic `env_opt(key)` helper
  (`std::env::var(key).ok().filter(|v| !v.trim().is_empty())`) plus `stripe_api_base() ->
  String { env_opt("STRIPE_API_BASE").unwrap_or_else(|| "https://api.stripe.com".to_string()) }`,
  with trailing-slash trimming done at the join call site (`format!("{}/{}",
  stripe_api_base().trim_end_matches('/'), path)`).
- `rag.rs` has neither an `env_opt` helper nor an API-base seam today — it imports `std::env`
  (`rag.rs:9`) and reads `GEMINI_API_KEY` directly at each call site.

**Existing mock-server test idiom** (both `#[ignore]`d, both require Postgres):
`github.rs:967-1058` (`create_issue_handler_writes_audit_on_success`) and
`rag.rs:11393-11452` (`chat_report_issue_writes_user_scoped_audit_with_no_active_budget`) both:
bind `tokio::net::TcpListener::bind("127.0.0.1:0")`, spawn a throwaway `axum::Router::new()`
`.fallback(...)` handler + `axum::serve`, and point the real env var (`GITHUB_API_BASE`) at the
bound address before driving the real handler through the mock.

**Lazy-pool precedent for pool-touching-but-DB-free tests**: `github.rs:794-806`
(`create_issue_core_rejects_empty_title_before_network`) uses
`PgPoolOptions::new().connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")` for a test
whose code path never actually queries the pool — the comment states "A lazy pool is never
actually queried on this path, so this stays a pure unit test (no DB needed)." This is directly
applicable here: `generate_conversation_title` and `generate_suggested_question` only touch
their `pool: &sqlx::PgPool` argument via `record_llm_usage` on the **success** path
(`rag.rs:5430`, `5720`); a timeout/connection-error short-circuits via `.ok()?` before that call
is ever reached, so a lazy (never-connected) pool is safe there.

**No dependency changes needed**: `backend/Cargo.toml` already has `tokio` (full), `axum`
(with the router/serve pieces used by the existing mock idiom), and `reqwest` — no wiremock or
other mocking crate is used or needed anywhere in this codebase (confirmed: only the raw
`TcpListener`+`axum::Router` idiom exists).

**No CI test gate today**: none of `.github/workflows/*.yml` trigger on `pull_request` — only
`push` (to `main`, gating release-please) and `workflow_dispatch`. A PR against this repo shows
an empty `statusCheckRollup`. The quality gate for this change is local `cargo build && cargo
test && cargo clippy` in the branch, not an automated CI check.

## Assumptions

1. **`gemini_api_base()` shape mirrors `github.rs`'s `api_base()` most closely** (not
   `billing.rs`'s generic `env_opt`) since it needs the identical trim/trailing-slash/empty-
   string handling and rag.rs has no existing generic env-var helper to reuse. Signature:
   `fn gemini_api_base() -> String`, reading `GEMINI_API_BASE`, defaulting to
   `"https://generativelanguage.googleapis.com"` (no trailing slash, matching the literal
   prefix every existing URL currently hardcodes).
2. **Threading is a pure string substitution, not a behavior change.** Every one of the 4
   `format!("https://generativelanguage.googleapis.com/v1beta/models/...", ...)` calls becomes
   `format!("{}/v1beta/models/...", gemini_api_base(), ...)`. With `GEMINI_API_BASE` unset (the
   production/default-dev state), `gemini_api_base()` returns the exact literal string every
   call site already hardcodes — so production URL construction is byte-for-byte unchanged.
3. **Only `generate_conversation_title` and `generate_suggested_question` get new,
   non-`#[ignore]`d timeout tests** (using the lazy-pool precedent — no Postgres needed, so
   they run in every default `cargo test`). The **main chat call** lives inside `chat_endpoint`,
   which resolves the active budget, checks permissions, and loads context via several DB
   queries *before* ever reaching the Gemini call — exercising it end-to-end genuinely requires
   a seeded Postgres database, matching every other full-`chat_endpoint` test in this file
   (`rag.rs:7050` `#[ignore = "requires Postgres + pgvector; run via: cargo test -- --ignored"]`
   and its siblings). This new test is therefore `#[ignore]`d, consistent with that established
   convention — it is not skipped, just gated the same way every other `chat_endpoint` test is.
4. **Tests wait out the real configured duration, not a shortened one.** There is no env-var
   override for the timeout `Duration`s themselves (only for the base URL) and adding one is
   out of scope (the issue asks for a base-URL seam, not a timeout-value seam) — so each mock
   handler sleeps a few seconds past its call site's configured timeout (Assumption #9 fixes
   the exact margins) before responding. This is the deliberate cost of asserting the *actual
   configured* timeout fires, not a shortened stand-in — matches the issue's explicit ask
   ("holds the connection open past the configured timeout"). Because both non-ignored tests
   mutate the same process-global `GEMINI_API_BASE` env var, they must hold the same lock
   (Assumption #6) for their whole body and therefore **serialize** — the two ~24s tests add
   ~48s of wall time to the default `cargo test` run, not ~24s. This is accepted as the
   deliberate cost of a real env-var-mutating regression test, matching the existing
   `ISSUE_RATE_ENV_LOCK` precedent's own documented tradeoff.
5. **Mock handler shape**: a hung/never-responding connection is simulated by an axum handler
   that `tokio::time::sleep`s longer than the call site's configured timeout before returning
   any response (rather than literally never returning), so the mock task itself terminates
   cleanly instead of leaking a permanently-blocked task per test run. A body that never
   completes and one that completes just past the client's timeout are observationally
   identical from the *client's* perspective (both manifest as the client's own timeout firing
   first) — the whole point of a client-side `reqwest` timeout is that it does not wait for the
   server.
6. **Env var hygiene**: each new test sets `GEMINI_API_BASE` to the bound mock address and
   removes it in a cleanup step (mirroring the existing `GITHUB_API_BASE` set/remove pattern at
   `github.rs:987/1052`). Because `#[tokio::test]` tests in the same binary run concurrently by
   default, and all 3 new tests mutate the same process-global `GEMINI_API_BASE` var, they need
   a shared lock — exactly the reason `ISSUE_RATE_ENV_LOCK` exists (`github.rs:522`,
   `pub(crate) static ISSUE_RATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());`,
   "Hold it for the whole test body when touching any of these vars"). This spec adds an
   analogous `static GEMINI_API_BASE_ENV_LOCK: std::sync::Mutex<()> =
   std::sync::Mutex::new(());` (plain `static`, no `OnceLock` — matching the real
   `ISSUE_RATE_ENV_LOCK` declaration, not a generic lazy-lock pattern), held for the *entire*
   body of each of the 3 new tests (set env var through the awaited call and its assertions).
   Holding it for the whole body — not just the `set_var` call — is what makes the two
   non-ignored tests **serialize** rather than run concurrently (see Assumption #4's revised
   wall-clock accounting).
7. **No change to any fallback/error-handling code.** The existing `Err`/`.ok()?` branches at
   all three `generateContent` sites already produce the exact fallback the issue names
   (`fallback_title`, `DEFAULT_SUGGESTED_QUESTION`, the "communications link is down" response)
   — the new tests assert this existing behavior, they do not add new behavior.
8. **`get_gemini_embedding` is threaded through `gemini_api_base()` too** (task 1's scope, per
   the issue's explicit list), but does **not** get a new timeout-trigger test in this ticket —
   the issue's test-writing ask ("at least the three generateContent call sites") is scoped to
   the three `generateContent` sites, not the `embedContent` site. `get_gemini_embedding`'s own
   timeout is unchanged; it has at least 5 call sites in `rag.rs` (including
   `resolve_transaction_by_locator` and the create-transaction write path) that are unaffected
   by this change beyond the URL-prefix substitution. Adding a dedicated timeout test for it is
   a reasonable future follow-up, not required here — noted in Risks.
9. **Margins past the configured timeout**: each mock handler sleeps 5s past its call site's
   configured timeout before responding (25s for the two 20s-timeout sites, 35s for the 30s
   main-chat site) — a larger cushion than the bare minimum, to keep the assertion robust
   against scheduler jitter on a loaded CI/dev machine without materially changing the added
   wall-clock cost.

## Goal & Success Criteria

Give every hardcoded Gemini call site in `backend/src/rag.rs` a `GEMINI_API_BASE` test seam
(mirroring the `GITHUB_API_BASE`/`STRIPE_API_BASE` precedent), and add tests that genuinely
trigger the configured timeouts end-to-end for the three `generateContent` sites — turning the
#249 timeout fix from an untested `Duration` assertion into a real regression guard.

- [ ] `gemini_api_base()` exists in `rag.rs`, defaults to the exact literal URL prefix every
      call site already hardcodes, and is overridable via `GEMINI_API_BASE`.
- [ ] All 4 hardcoded `generativelanguage.googleapis.com` URL constructions in `rag.rs` are
      threaded through `gemini_api_base()`; production behavior (unset env var) is unchanged.
- [ ] A new test proves `generate_conversation_title` falls back to `fallback_title(...)`
      when the mock Gemini endpoint holds the connection past its 20s timeout.
- [ ] A new test proves `generate_suggested_question` falls back to `DEFAULT_SUGGESTED_QUESTION`
      under the same condition.
- [ ] A new (`#[ignore]`d, Postgres-backed) test proves the main chat call's "communications
      link is down" fallback fires when the mock Gemini endpoint holds the connection past its
      30s timeout, through the real `chat_endpoint`.
- [ ] `cd backend && cargo build && cargo test && cargo clippy --all-targets --all-features` all
      pass (the two new non-ignored tests run in the default suite; the ignored one is verified
      separately against a local Postgres per the repo's existing convention).
- [ ] No other call sites, error-handling branches, or fallback text are modified.

## Scope

**In scope:** `backend/src/rag.rs` only — the `gemini_api_base()` helper, threading it through
all 4 hardcoded URLs, and the 3 new timeout-trigger tests (2 non-ignored + 1 ignored).

**Out of scope:** the frontend (`App.svelte`'s `fetchWithTimeout` sites are already covered per
#268, not touched by this ticket); adding an env-var seam for the timeout *durations*
themselves; a `get_gemini_embedding` timeout-trigger test (noted as a follow-up, see
Assumptions #8); retry/circuit-breaker logic; any change to `GITHUB_API_BASE`/`STRIPE_API_BASE`
or their tests.

## Architecture

`gemini_api_base()` is a private `fn` in `rag.rs`, placed immediately above `get_gemini_embedding`
(its first use), following `github.rs`'s `api_base()` shape exactly:

```rust
fn gemini_api_base() -> String {
    std::env::var("GEMINI_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string())
}
```

Each of the 4 `format!` URL constructions changes its literal prefix to `{}` bound to
`gemini_api_base()`, e.g.:

```rust
// before
"https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent?key={}"
// after
"{}/v1beta/models/gemini-embedding-001:embedContent?key={}", gemini_api_base(), api_key
```

Tests add `static GEMINI_API_BASE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());`
(a plain `static`, matching `ISSUE_RATE_ENV_LOCK`'s exact declaration in `github.rs:522`) to the
`#[cfg(test)] mod tests` block, and 3 new tests:

1. `generate_conversation_title_falls_back_to_fallback_title_on_timeout` — lazy pool, mock
   server sleeps 25s before responding (5s past the 20s timeout), asserts
   `generate_conversation_title(...).await == None` (its caller's fallback is
   `fallback_title`, itself already unit-tested separately — this test proves the `None` that
   triggers that fallback, which is the seam this function owns).
2. `generate_suggested_question_falls_back_to_default_on_timeout` — same shape, mock server
   sleeps 25s, asserts `generate_suggested_question(...).await == None`.
3. `chat_endpoint_falls_back_to_communications_link_down_on_gemini_timeout` — `#[ignore]`d,
   real Postgres, seeds a user + default budget, sets `GEMINI_API_KEY` to a non-empty dummy
   value (so `chat_endpoint` takes the live-call branch, not the offline router) and
   `GEMINI_API_BASE` to the mock's address, calls `chat_endpoint(...)` directly as an async fn
   with hand-built `State(state)`/`Extension(user_id)`/`Json(payload)` arguments — mirroring
   `chat_share_budget_is_owner_only`'s actual harness shape (direct handler-fn call, no
   `Router`/`oneshot` — that idiom is used by `github.rs`'s REST-handler tests, not by any
   `rag.rs` test) — and asserts the response's `response_text` contains "communications link is
   down" (mock server sleeps 35s, 5s past the 30s timeout).

## Error Handling & Edge Cases

- No new error-handling code — this is a test-and-seam-only change; the assertions target the
  fallback branches that already exist (see Assumptions #7).
- The mock server's `sleep`-then-respond shape (Assumption #5) ensures each spawned mock task
  terminates on its own after the sleep, rather than leaking a handler that blocks forever —
  avoids resource leakage across repeated local `cargo test` runs.
- `gemini_api_base()`'s empty/whitespace-only override falls back to the default exactly like
  `GITHUB_API_BASE`/`STRIPE_API_BASE` do — no new failure mode introduced.

## Testing Approach

- `cd backend && cargo test` (default suite) exercises `gemini_api_base()`'s
  override/default/trim behavior indirectly through the 2 new non-ignored timeout tests, plus
  the full existing suite (unchanged).
- `cd backend && cargo test -- --ignored` (run manually against a local
  `podman-compose up -d` Postgres, per repo convention) additionally exercises the new
  Postgres-backed main-chat timeout test.
- `cargo clippy --all-targets --all-features` and `cargo build` must both stay clean.
- No frontend changes, so no frontend test/build step is needed for this ticket.

## Risks & Open Questions

- **Wall-clock cost**: the 2 new non-ignored tests must serialize on
  `GEMINI_API_BASE_ENV_LOCK` (both mutate the same process-global env var), adding ~48s to
  every default `cargo test` run (~25s each, back-to-back). This is the deliberate cost of
  testing the *actual* configured timeout rather than a stand-in value: a shortened timeout
  would test a different code path than production runs. Accepted per Assumption #4.
- **`get_gemini_embedding` has no new timeout test** (Assumption #8) — the issue's ask is scoped
  to the three `generateContent` sites; a follow-up ticket can add one using the same idiom this
  PR establishes if desired.
- **The main-chat timeout test is `#[ignore]`d** like every other full-`chat_endpoint` test in
  this file — it will only run when a human/agent explicitly runs `cargo test -- --ignored`
  against a live Postgres. This matches 100% of the existing precedent for this exact class of
  test (no full-`chat_endpoint` test in this file runs in the default suite), so it is not a
  gap introduced by this change, but it does mean the main-chat path's regression coverage is
  opt-in, same as it always has been.
- **A genuine 5th hardcoded, un-timed-out Gemini call site exists outside `rag.rs`**:
  `backend/src/reports.rs`'s `gemini_narrative` (called from `generate_narrative`) builds a bare
  `reqwest::Client::new()` (no `.timeout()` at all) and hardcodes
  `https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent`.
  This is the same #249 hang class, entirely unaddressed by #249/#268 (which only touched
  `rag.rs`) and out of scope for this ticket (`#269`'s issue body names `backend/src/rag.rs`
  exclusively). Filed as a follow-up issue rather than silently widened into this PR's scope —
  see the tracker for the filed issue number.
