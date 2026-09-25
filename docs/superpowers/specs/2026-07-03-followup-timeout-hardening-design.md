# Follow-Up Timeout Hardening (raw fetch + generateContent) — Design Spec

Source: `savvagent/nels#249` — "Same hang class as #246 remains in two raw fetch() sites and
backend's generateContent calls"

## Brief (verbatim from the issue)

> While fixing #246 (`fetchApi` had no request timeout, wedging `isChatLoading`/`isDraining`
> forever on a hung request — fixed in #248), I found three related, un-fixed instances of the
> identical hang class. Filing as a follow-up per scope discipline rather than widening #246/#248,
> since none of these are literally "a `fetchApi` call site" and none carry #246's queue-wedging
> blast radius.
>
> 1. `downloadExport` — raw `fetch()`, no timeout. `frontend/src/App.svelte` (`downloadExport`,
>    ~line 516) calls `fetch()` directly instead of `fetchApi` — it bypasses `fetchApi` because the
>    endpoint returns a `Blob` (a file attachment), not JSON, and `fetchApi` force-parses JSON. A
>    hung `GET /account/export` leaves this promise pending forever with no timeout.
> 2. `setDefaultBudget` — raw `fetch()`, no timeout. `frontend/src/App.svelte`
>    (`setDefaultBudget`, ~line 714) also calls `fetch()` directly — it bypasses `fetchApi` because
>    `POST /budgets/:id/default` returns a non-204 empty 200 body that `fetchApi`'s forced
>    `res.json()` can't parse. Same hang risk.
> 3. Backend: Gemini `generateContent` calls have no `reqwest` client timeout.
>    `backend/src/rag.rs` has three call sites (`generateContent`, ~1423, ~4817, ~5086) that
>    construct `reqwest::Client::new()` with no explicit timeout — unlike `get_gemini_embedding` in
>    the same file (~360), which sets a 20s timeout. A backend-side hang here is plausibly the
>    literal "stuck backend/LLM call" #246 described — #248 bounds the frontend's wait, but the
>    backend request itself is still unbounded, tying up a server-side task/connection indefinitely.
>
> Suggested fix: apply `fetchWithTimeout` (`frontend/src/lib/fetchTimeout.js`, added in #248) at the
> two raw-fetch sites; give each of the three `generateContent` `reqwest::Client` builders an
> explicit `.timeout(...)`, mirroring `get_gemini_embedding`'s 20s pattern (or a value tuned to
> generation calls, which may legitimately run longer than an embedding call).
>
> No AC/timeline attached — filed for visibility, not urgency.

The filer flagged that line numbers were stale at filing time (`nels#238`, `#239`, `#240`, `#241`,
`#257` have all touched `App.svelte`/`rag.rs`/`budget.rs` since) and specifically that
`setDefaultBudget` "has since been modified by two merged tickets" (`#238` added an Owner-only
permission check + `canSetAsDefault` client-side guard; `#241` added a budgets-list page that also
calls it). Everything below is re-verified against the current source on this branch, not the
issue body's line numbers.

## Current state (verified fresh against this branch)

- **`downloadExport`** — `frontend/src/App.svelte:536-561`. Unchanged in shape from the issue:
  bare `fetch(`${API_BASE}/account/export`, { headers: token ? {...} : {} })`, then
  `res.blob()` → object URL → anchor download. No timeout. `try/catch` around the whole body
  surfaces `e.message || $_("account.exportFailed")` via `triggerError`.
- **`setDefaultBudget`** — `frontend/src/App.svelte:772-787`. Still a raw
  `fetch(`${API_BASE}/budgets/${budgetId}/default`, { method: "POST", headers })` with no
  timeout, no explicit `try/catch` of its own (callers catch). `#238`/`#241` did **not** touch
  this function's body — they added:
  - `canSetAsDefault` (imported from `./lib/commands.js`, `App.svelte:49`), a client-side
    Owner-only guard checked by **both** current callers before `setDefaultBudget` is ever
    invoked: the `/budgets-switch` chat command handler (`App.svelte:1024`,
    `if (!canSetAsDefault(target)) { ...; return; }`) and `BudgetsView.svelte`'s `handleSwitch`
    (`BudgetsView.svelte:70-98`, guarded by the sibling `canSwitchTo` from `./budgetsView.js`,
    called before its own `onSwitch(budget.id)` — `onSwitch` is wired to `setDefaultBudget`
    itself at `App.svelte:2180`).
  - A second caller: `BudgetsView.svelte` (the `#241` budgets-list page) calls `setDefaultBudget`
    indirectly via the `onSwitch` prop, not by importing it directly.
  - The server-side Owner-only permission check (`#238`) lives entirely in the backend handler for
    `POST /budgets/:id/default` — no frontend change needed or made here.
  - **Conclusion**: the fix is scoped to `setDefaultBudget`'s own body only; none of the
    surrounding gating logic in either caller needs to change.
- **Backend `generateContent` sites**, `backend/src/rag.rs`, confirmed at:
  - `~1444` — the main `/chat` handler's LLM call (the request the user is actively waiting on).
  - `~4901` — `generate_conversation_title` (fires after the chat reply is already prepared, but
    is still `await`ed synchronously before the HTTP response is returned — `rag.rs:3531-3538`
    calls it inline in the same request handler, so it still occupies the request/connection).
  - `~5170` — `generate_suggested_question` (its own endpoint, `/suggested-question`).
  - All three currently do `let client = reqwest::Client::new();` with zero timeout config.
  - `get_gemini_embedding` (`rag.rs:354-367`) is the existing precedent: `reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(20)).build().unwrap_or_else(|_| reqwest::Client::new())`
    — fully-qualified `std::time::Duration` inline, no top-level `use` import in this file.
  - Existing error handling already degrades gracefully on any `send()` failure (including what a
    timeout will now produce): the main chat call's `match res { Err(e) => ... }` branch returns a
    canned "communications link is down" `AiStructuredResponse`; both `generate_conversation_title`
    and `generate_suggested_question` use `.ok()?` to short-circuit to `None`, which their callers
    already turn into a `fallback_title(...)` / `DEFAULT_SUGGESTED_QUESTION` fallback. No new
    error-handling code is needed at any of the three sites — only the client construction changes.
  - Confirmed via the `rag.rs` test-module comment (`:5236-5239`): "there is no Gemini test harness
    in this repo" — no wiremock/mockito dependency exists in `backend/Cargo.toml`, and no
    `GEMINI_API_BASE` test seam exists (unlike `GITHUB_API_BASE`/`STRIPE_API_BASE`).

## Assumptions

1. **Frontend fix mechanism**: swap the raw `fetch()` for the already-imported, already-tested
   `fetchWithTimeout` (`App.svelte:59`, currently used only inside `fetchApi`) at both call sites,
   using its default timeout — no new helper, matches the issue's preferred suggested fix.
2. **`setDefaultBudget`'s current shape and both its callers' gating logic are exactly as verified
   above** — the fix touches only the function body (the `fetch` call itself); `canSetAsDefault`/
   `canSwitchTo` and both call sites (`App.svelte:1024`, `BudgetsView.svelte`'s `handleSwitch`) are
   untouched.
3. **`downloadExport` keeps its Blob-handling logic unchanged** — only the `fetch()` call is
   swapped for `fetchWithTimeout()` with identical options; a timeout surfaces through the existing
   `catch` exactly like today's network-error case (`fetchWithTimeout` throws a plain `Error`, same
   shape as a normal fetch rejection).
4. **`setDefaultBudget` needs no new `try/catch`** — a `fetchWithTimeout` timeout throws a plain
   `Error`, which propagates identically to today's rejection shape; both existing callers already
   catch and surface it.
5. **Backend timeout values** (left to judgment by the issue): main chat `generateContent`
   (`rag.rs` ~1444, chained behind the frontend's 45s-default `fetchApi` call, `#248`) → **30s** —
   long enough for a real generation call carrying substantial RAG context, while still leaving the
   backend's existing graceful fallback response to fire well before the frontend's own 45s
   client-side budget elapses (so a hang surfaces as a real backend error message, not a generic
   client timeout). `generate_conversation_title` (~4901) and `generate_suggested_question`
   (~5170) — both short, single-line, already-optional outputs that degrade gracefully to
   `None`/fallback on any failure → **20s**, matching `get_gemini_embedding`'s existing precedent
   exactly (same call shape: one JSON POST, one short parsed response).
6. **No new test infrastructure.** Neither stack has a means to exercise a live network timeout for
   these call sites (see Current State), and neither of the two pre-existing analogous sites
   (`get_gemini_embedding`, `fetchApi`'s `fetchWithTimeout` wiring) added one. The frontend fix
   reuses already-unit-tested `fetchWithTimeout` (`fetchTimeout.test.js`); the backend fix mirrors
   `get_gemini_embedding`'s already-untested `.timeout()` builder pattern verbatim. Verification is
   full build/test/lint passing at each stack, plus manual diff review of all 5 sites.
7. **Scope discipline** (matches the issue's own instruction): touch only the 5 named call sites.
   Any other raw `fetch()`/unbounded `reqwest::Client` discovered is noted as a follow-up
   suggestion in Risks, not fixed here.

## Goal & Success Criteria

Eliminate the remaining un-timeout-bounded hang points (same class as #246/#248) so a hung export
download, default-budget POST, or backend Gemini `generateContent` call can no longer wedge a
frontend promise or backend request/connection indefinitely.

- [ ] `downloadExport` uses `fetchWithTimeout` instead of raw `fetch`; Blob/download behavior on
      the happy path is byte-for-byte unchanged.
- [ ] `setDefaultBudget` uses `fetchWithTimeout` instead of raw `fetch`; both callers'
      `canSetAsDefault`/`canSwitchTo` gating and their own error handling are untouched and still
      behave identically on the happy path.
- [ ] All three `generateContent` call sites in `backend/src/rag.rs` build their `reqwest::Client`
      with an explicit `.timeout(...)`, mirroring `get_gemini_embedding`'s exact builder shape
      (including the `unwrap_or_else(|_| reqwest::Client::new())` fallback).
- [ ] `cd backend && cargo build && cargo test && cargo clippy --all-targets` all pass.
- [ ] `cd frontend && pnpm test && pnpm run build` all pass.
- [ ] No other call sites are modified.

## Scope

**In scope:** the 5 named call sites exactly, re-verified against current source (2 in
`frontend/src/App.svelte`, 3 in `backend/src/rag.rs`).

**Out of scope:** any other `fetch()`/unbounded `reqwest::Client` site not named above (→ follow-up
if found); adding a test harness for either stack; changing `get_gemini_embedding`'s or
`fetchApi`'s own existing timeout defaults; retry logic; a user-facing "cancel this request"
affordance.

## Architecture

Two mechanical, localized edits — no new components, no new files.

```
frontend/src/App.svelte
  downloadExport() (~536):
    fetch(`${API_BASE}/account/export`, {...})
      -> fetchWithTimeout(`${API_BASE}/account/export`, {...})   // same options, default timeout

  setDefaultBudget(budgetId) (~772):
    fetch(`${API_BASE}/budgets/${budgetId}/default`, {...})
      -> fetchWithTimeout(`${API_BASE}/budgets/${budgetId}/default`, {...})  // same options, default timeout

backend/src/rag.rs   (three sites: ~1444 main chat, ~4901 title, ~5170 suggested-question)
  let client = reqwest::Client::new();
    -> let client = reqwest::Client::builder()
           .timeout(std::time::Duration::from_secs(N))   // N=30 (chat), N=20 (title/suggested-question)
           .build()
           .unwrap_or_else(|_| reqwest::Client::new());
```

## Error Handling & Edge Cases

- **Frontend**: a timeout now surfaces as `Error("Request timed out")` from `fetchWithTimeout`,
  the same shape as any other fetch rejection. `downloadExport`'s existing `catch` → `triggerError`
  handles it with no changes. `setDefaultBudget`'s two callers' existing `catch` blocks
  (`App.svelte:1030`'s try/catch around the chat-command flow; `BudgetsView.svelte`'s
  `handleSwitch` try/catch) handle it identically to today's rejection.
- **Backend**: a `reqwest` client-level timeout on `.send()` surfaces as `Err(reqwest::Error)`,
  which all three sites' existing branches already handle gracefully (see Current State) — no new
  code path is introduced, a timeout just becomes one more reason `send()` can fail, already
  covered by the existing `match`/`.ok()?` handling.
- No behavior change on the success path (a fast response) at any of the 5 sites.

## Testing Approach

- **Backend**: `cd backend && cargo build && cargo test && cargo clippy --all-targets` — confirms
  the three edits compile, all existing tests (125+ in `rag.rs`) still pass, no new lint warnings.
  No new test added (Assumption 6 — no Gemini test harness exists in this repo, matching
  `get_gemini_embedding`'s own untested precedent).
- **Frontend**: `cd frontend && pnpm test` (existing `fetchTimeout.test.js` already covers
  `fetchWithTimeout`'s behavior directly; `commands.test.js`/`budgetsView.test.js` cover the
  surrounding gating logic, untouched by this change) and `cd frontend && pnpm run build` to
  confirm the component still compiles. No new test added (no `App.svelte` component-test harness
  exists in this repo, per `fetchTimeout.test.js`'s own header comment).
- **Manual**: read the final diff to confirm each of the 5 call sites now routes through a bounded
  client/fetch, with the correct timeout value, and that no other call site was touched.

## Risks & Open Questions

- The 30s/20s backend values are a judgment call, not a derived SLA (Assumption 5) — could be
  tuned later with real `generateContent` latency telemetry; not blocking for this fix.
- No 4th similar hang site was found beyond the 5 named during this investigation. If one surfaces
  later, per this issue's own scope-discipline instruction, file it as a separate follow-up rather
  than widening this PR.
