# fetchApi Request Timeout — Design Spec

Source: `savvagent/nels#246` — "fetchApi has no request timeout — a hung request can wedge
isChatLoading/isDraining forever"

## Brief (verbatim from the issue)

> `fetchApi` (`frontend/src/App.svelte`) wraps a bare `fetch()` with no `AbortController`/timeout.
> If a request never settles (e.g. a stuck backend/LLM call), any `isChatLoading`-style busy flag
> gated on its `try/finally` never resets.
>
> Found during review of #230 (FIFO chat queue + auto-send): that PR increased the blast radius of
> this pre-existing gap — a hung `/chat` call now leaves `isDraining` stuck `true` too, so the queue
> accepts up to 10 messages that will never send, with a misleading "Queue full — wait for a reply"
> hint that will never resolve. Previously a hung call only blocked one composer, not a queue
> behind it.
>
> Suggested fix: add an `AbortController`-based timeout to `fetchApi` (or at minimum the `/chat`
> call path), so a hung request eventually rejects and the existing `catch`/`finally` handling
> (error bubble, `isChatLoading = false`) actually fires.
>
> Scope note: out of scope for #230, which explicitly deferred streaming/`AbortController`/
> stop-generation work. This is a separate, pre-existing gap that predates that feature and affects
> every `fetchApi` call site in the app, not just chat.

## Current state (verified fresh against `frontend/src/App.svelte` on this branch, `wc -l` = 2446)

- `fetchApi` (`:307-333`) is a single shared authenticated request helper: sets
  `Content-Type`/`Authorization` headers, calls bare `fetch(...)`, force-`res.json()`s any non-204
  response, and treats a 401 as a logout trigger. It is used directly by ~20 call sites in
  `App.svelte` and is also handed down as a prop to `Notifications.svelte`, `CategoriesView.svelte`,
  and `Insights.svelte`, which call it identically. No caller passes `options.signal` today.
- #230's FIFO queue (`chatQueue.js` / `drainQueue` in `App.svelte`) **has already landed and merged**
  (PR #243). Current shipped shape, confirmed by reading the code fresh (not the pre-#230
  description in the issue body):
  - `performSend(rawInput)` (`:1028-1158`) is the per-item send routine: sets `isChatLoading = true`,
    `await fetchApi("/chat", ...)` inside `try`, appends an error bubble in `catch`, and resets
    `isChatLoading = false` in `finally`. It never throws (both branches self-catch), by design
    (#230 comment at `:1024-1027`).
  - `drainQueue()` (`:1178-1194`) sets `isDraining = true`, `await runDrain(() => pendingQueue,
    performSend, ...)` (from `chatQueue.js`) in `try`, and resets `isDraining = false` in `finally`.
  - `sendChatMessage` (`:1202-1210`) is gated by `canEnqueue(pendingQueue)` — `chatQueue.js`'s
    `QUEUE_CAP = 10`. The "Queue full" hint (`chat.queueFull` in every locale file) renders when
    the cap is hit.
  - **Confirms the issue's core claim still holds against current code**: a `fetchApi("/chat", ...)`
    call that never settles leaves `await fetchApi(...)` in `performSend` permanently pending →
    `performSend`'s `finally` never runs → `isChatLoading` stays `true` forever. Because
    `performSend` also never resolves, `drainQueue`'s `await runDrain(...)` also never resolves →
    `isDraining` stays `true` forever → the queue fills to `QUEUE_CAP` (10) and the "Queue full —
    wait for a reply" hint renders permanently, exactly as the issue describes.
- Two other raw `fetch()` call sites exist in `App.svelte` that duplicate `fetchApi`'s auth-header +
  401 logic but bypass it (each has an explicit comment explaining why): `downloadExport` (`:516-541`,
  bypasses because it needs a `Blob`, not forced JSON) and `setDefaultBudget` (`:714-729`, bypasses
  because the endpoint returns a non-204 empty 200 body that `fetchApi` can't parse). Both are
  susceptible to the identical hang class as `fetchApi`, but neither is a "fetchApi call site" per
  the issue's literal title/scope, and neither gates a busy-flag with the queue-wedging blast radius
  `/chat` has. **Left out of scope** here (see Scope) — flagged as a follow-up.
- On the backend, the three Gemini `generateContent` call sites in `backend/src/rag.rs` (the actual
  `/chat` LLM call) construct `reqwest::Client::new()` with **no explicit timeout** — unlike the
  Gemini *embedding* call (`get_gemini_embedding`, same file, 20s `reqwest` client timeout). A
  backend-side hang here is plausibly the literal trigger the issue describes ("a stuck backend/LLM
  call"). This is a `backend/src/rag.rs` change, outside this issue's stated scope
  (`frontend/src/App.svelte`) and outside this PR's worktree discipline (a concurrent job,
  nels#238, is independently working in `backend/src/budget.rs` — a different file, so no direct
  conflict, but widening into `backend/src` here is still scope creep for a frontend-scoped ticket).
  **Left out of scope** — flagged as a follow-up alongside the two raw-fetch sites above.
- Existing precedent for pure, framework-free helpers extracted out of `App.svelte` into
  `frontend/src/lib/*.js` + a co-located `*.test.js`: `chatQueue.js`, `chatWindow.js`, `router.js`,
  `budgetDisplay.js`, `money.js`. This repo's `vitest` runs `environment: "node"` (confirmed in
  `frontend/vitest.config.js`) — there is no component-test harness, so `App.svelte` itself is never
  directly unit-tested; only its extracted pure logic is.

## Assumptions

1. **Fix lives in `fetchApi` itself, not just the `/chat` call path.** The issue explicitly says
   the gap "affects every `fetchApi` call site in the app, not just chat" and offers `fetchApi`
   as the primary suggested fix location. Fixing the shared helper covers every current and future
   caller (including the three child components that receive it as a prop) with one change,
   matching the extraction precedent (`chatQueue.js` et al.) of pulling shared logic out of
   `App.svelte` into a small, unit-tested `frontend/src/lib/*.js` module.
2. **New pure module `frontend/src/lib/fetchTimeout.js`** exporting `DEFAULT_FETCH_TIMEOUT_MS` and
   `fetchWithTimeout(url, options, timeoutMs, timeoutMessage)`. `fetchApi` becomes a thin wrapper
   that adds auth headers / 401 handling / JSON parsing around a call to `fetchWithTimeout`. This
   mirrors the `chatWindow.js`/`chatQueue.js` precedent exactly and — critically — makes the
   timeout/abort logic unit-testable with a mocked `global.fetch`, which a change made only inline
   inside `App.svelte`'s `<script>` block could not be (no component-test harness in this repo).
3. **Default timeout = 45 000 ms (45s).** No explicit SLA is documented anywhere in the repo. The
   one comparable backend value found (`get_gemini_embedding`'s `reqwest` client timeout,
   `backend/src/rag.rs:360`) is 20s, but that's a single embedding call — the `/chat` round trip can
   involve a heavier generation call (system prompt + RAG context + potential tool-call handling)
   that legitimately runs longer than an embedding call. 45s is chosen as a value generous enough
   to avoid false-positive timeouts on a slow-but-live `/chat` turn, while still bounding "hung
   forever" down to "unstuck within well under a minute" — closing the issue's actual complaint
   (queue-wedging, not "make it fast"). Every non-chat call (`/budgets`, `/insights`, `/auth/me`,
   etc.) normally resolves in well under a second, so 45s never fires on the happy path for them
   either. Exposed as a named export (not hardcoded inline) and overridable per-call via a new
   `options.timeoutMs`, so a future call site with different latency characteristics doesn't need
   a second helper.
4. **A caller-supplied `options.signal` disables the timeout wrapper entirely** (pass-through to
   bare `fetch`) rather than trying to compose two abort sources. No current call site passes
   `signal`, so this is a forward-compatibility guard for a future caller (e.g. a future
   stop-generation feature, explicitly deferred by #230) that wants to own cancellation itself, not
   a behavior any current code exercises.
5. **Timeout rejects with `new Error("Request timed out")`**, not the browser/Node `AbortError`'s
   opaque native message (`"The user aborted a request."`/`"This operation was aborted"`, which
   varies by runtime). Every existing `fetchApi` catch site surfaces `e.message` directly to the
   user (e.g. `performSend`'s chat error bubble: `` `⚠️ I was unable to connect to my AI node.
   Error: ${e.message}...` ``) or via `.message || <i18n key>` fallbacks — a clear, stable message
   here is user-facing, not just a debug string.
6. **No i18n key added.** None of `fetchApi`'s other thrown messages ("Unauthorized", "API error",
   raw `errText`) are localized today; `"Request timed out"` follows that existing precedent instead
   of introducing new localization scope.
7. **The two other raw-`fetch()` call sites (`downloadExport`, `setDefaultBudget`) and the backend
   `generateContent` calls' missing `reqwest` timeout are explicitly out of scope for this PR** —
   filed as a follow-up issue instead of silently widened into this ticket (see Scope + Risks).

## Goal & Success Criteria

Bound every `fetchApi` call to a finite timeout so a hung request always eventually rejects,
letting the existing `catch`/`finally` handling at every call site (chat's error bubble +
`isChatLoading = false`, `drainQueue`'s `isDraining = false`, every other caller's own
`catch`/`finally`) actually fire instead of hanging forever.

- [ ] A `fetchApi` call whose underlying `fetch` never settles rejects after
      `DEFAULT_FETCH_TIMEOUT_MS` with a clear `Error("Request timed out")` instead of hanging.
- [ ] `performSend`'s `finally` (`isChatLoading = false`) and `drainQueue`'s `finally`
      (`isDraining = false`) both fire after a hung `/chat` call times out — the queue stops
      accepting phantom messages and the "Queue full" hint eventually clears.
- [ ] Every other existing `fetchApi` call site (auth, budgets, insights, conversations, billing,
      notifications, categories) is protected by the same default with zero call-site changes.
- [ ] A normal (fast, successful) request is byte-for-byte unaffected — no behavior change on the
      happy path.
- [ ] A genuine network error (not a timeout — e.g. connection refused) still propagates its own
      distinct error, not `"Request timed out"`.
- [ ] `fetchApi`'s own `options.timeoutMs` override (Assumption 3) is actually wired through to
      `fetchWithTimeout`'s `timeoutMs` parameter — not merely present at the `fetchWithTimeout`
      module level with no caller ever reaching it.

## Scope

**In scope:** `frontend/src/App.svelte` (`fetchApi` refactored to delegate to the new helper), new
`frontend/src/lib/fetchTimeout.js` + `frontend/src/lib/fetchTimeout.test.js`.

**Out of scope:** the `/chat`-specific queue/drain mechanics (already shipped, #230/#243 — this PR
does not touch `chatQueue.js`); `downloadExport`'s and `setDefaultBudget`'s raw `fetch()` calls in
`App.svelte` (same hang class, but not literally "a `fetchApi` call site" and lower blast radius —
follow-up); the backend's un-timed-out `generateContent` `reqwest::Client` calls in
`backend/src/rag.rs` (backend change, outside this ticket's stated frontend scope — follow-up);
streaming / stop-generation / a user-facing "cancel this request" affordance (explicitly deferred
by #230, unrelated to this fix); any change to `chatQueue.js`'s `QUEUE_CAP` or drain semantics.
Assumption 4's `options.signal` pass-through is speculative forward-compat for that deferred
stop-generation work — no current call site exercises it beyond the one unit test listed under
Testing Approach.

## Architecture

```
frontend/src/lib/fetchTimeout.js   (new, pure, unit-tested)
  export const DEFAULT_FETCH_TIMEOUT_MS = 45000

  export async function fetchWithTimeout(url, options = {}, timeoutMs = DEFAULT_FETCH_TIMEOUT_MS,
                                          timeoutMessage = "Request timed out")
    - if options.signal is already set: return fetch(url, options) unchanged (caller owns
      cancellation; no timeout applied)
    - else: create an AbortController, setTimeout(() => controller.abort(), timeoutMs), call
      fetch(url, { ...options, signal: controller.signal })
        - on success: clearTimeout, return the Response
        - on a rejection whose err.name === "AbortError": clearTimeout, throw
          new Error(timeoutMessage)
        - on any other rejection (genuine network error): clearTimeout, rethrow unchanged

App.svelte
  import { fetchWithTimeout } from "./lib/fetchTimeout.js"

  async function fetchApi(endpoint, options = {}) {
    // `timeoutMs` is fetchApi's own extension to `options` (not a native RequestInit key), so it
    // must be destructured out before the rest is spread into fetchWithTimeout/fetch — otherwise
    // it would ride along as an unrecognized key on the native fetch() call.
    const { timeoutMs, ...fetchOptions } = options;
    const headers = { "Content-Type": "application/json", ...fetchOptions.headers };
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetchWithTimeout(
      `${API_BASE}${endpoint}`,
      { ...fetchOptions, headers },
      timeoutMs, // undefined -> fetchWithTimeout's own default parameter (DEFAULT_FETCH_TIMEOUT_MS)
    );
    // unchanged from here: 401 -> handleLogout + throw; !res.ok -> throw errText; 204 -> null;
    // else res.json()
  }
```

No current call site passes `options.timeoutMs` — this wiring exists so Assumption 3's
per-call override is actually reachable through `fetchApi` (the only path every real call site
uses), not just through a direct `fetchWithTimeout` call that nothing in the app makes.

No change to `fetchApi`'s call signature or return contract — every existing call site (direct in
`App.svelte`, and via the `fetchApi` prop in `Notifications.svelte`/`CategoriesView.svelte`/
`Insights.svelte`) is unaffected by construction.

## Error Handling & Edge Cases

- **Timeout fires**: `fetchWithTimeout` rejects with `Error("Request timed out")`. In `fetchApi`,
  this propagates out of the `await fetchWithTimeout(...)` call before the 401/`!res.ok` checks are
  ever reached (there is no `Response` to inspect) — matches how a genuine network failure behaves
  today (also propagates before those checks). Every existing caller's own `catch` handles it
  exactly like any other `fetchApi` rejection; no new catch logic needed anywhere.
- **Success just under the wire**: `clearTimeout` runs in the success path before the timer can
  fire — no race where a just-barely-in-time response is still discarded.
- **Genuine (non-abort) network error** (e.g. offline, DNS failure, connection refused): rethrown
  unchanged — `err.name` is not `"AbortError"` for these in both browser `fetch` and Node's
  `undici`-backed global `fetch` (confirmed by testing distinct mock rejections, see Testing
  Approach), so the message a user sees for "genuinely can't reach the server" stays whatever it
  was before this change, not conflated with "timed out."
- **A caller passes its own `options.signal`** (no current call site does): `fetchWithTimeout`
  passes through to bare `fetch` untouched — no timeout applied, no double-abort-controller
  composition. Forward-compatible, not exercised by any code in this PR.
- **`clearTimeout` always runs**: both the success and failure branches clear the pending timer
  before returning/throwing, so a resolved/rejected-for-another-reason call never leaves a stray
  timer that fires later (irrelevant once already settled, but avoided for cleanliness/testability
  — fake timers in the test suite would otherwise need to account for it).

## Testing Approach

- **Unit (Vitest, new `frontend/src/lib/fetchTimeout.test.js`, TDD)**, using `vi.useFakeTimers()`
  and a mocked `global.fetch`:
  - a `fetch` that never resolves: advancing fake timers past `timeoutMs` causes the returned
    promise to reject with `Error("Request timed out")`; `fetch` was called with a `signal` that
    reports `aborted === true` after the advance.
  - a `fetch` that resolves before the timeout: resolves with that `Response`, and the internal
    timer is cleared (assert via `vi.getTimerCount()` returning to 0, or that advancing timers
    further causes no further rejection/state change).
  - a `fetch` that rejects with a distinct network error (not an abort): rejects with that exact
    error, unchanged — proves timeouts and genuine network failures aren't conflated.
  - a custom `timeoutMs`/`timeoutMessage` override is honored.
  - when `options.signal` is already present: `fetch` is called with that exact signal (no
    substitution), and no timer is scheduled at all.
- **Existing suite**: `cd frontend && pnpm test` must stay green — no regressions to
  `chatQueue.test.js`, `chatWindow.test.js`, `budgetDisplay.test.js`, `router.test.js`.
- **Build**: `cd frontend && pnpm run build` must succeed (confirms `App.svelte`'s import of the
  new module resolves cleanly).
- **`fetchApi`'s `timeoutMs` destructure/forward** (the App.svelte wiring itself, not
  `fetchWithTimeout`'s own logic) has no component-test harness to exercise it automatically —
  verified by code review (the diff makes the destructure-then-spread-then-3rd-arg wiring visible
  in ~5 lines) plus the manual pass below temporarily passing an explicit short `timeoutMs` to a
  `fetchApi` call and confirming it fires before the 45s default would.
- **Manual** (no component-test harness in this repo, same carve-out prior plans for this codebase
  take): run `pnpm run dev` against a real/local backend, temporarily lower
  `DEFAULT_FETCH_TIMEOUT_MS` (or block the backend's `/chat` route) to force a timeout, and confirm:
  the chat error bubble renders with the "Request timed out" message, `isChatLoading` resets (input
  re-enables), a queued message behind the timed-out one still drains, and `isDraining` resets
  (the "Queue full" hint clears once under the cap). Then restore the real default and confirm a
  normal chat turn is unaffected.

## Risks & Open Questions

- **45s is a judgment call**, not derived from a documented SLA (Assumption 3) — if real-world
  `/chat` turns routinely run longer (e.g. multi-hop tool-calling loops), 45s could false-positive.
  Mitigated by making it a named, easily-tunable constant and per-call-overridable rather than a
  hardcoded magic number.
- **The two raw-`fetch()` sites and the backend's un-timed-out `generateContent` calls are the same
  bug pattern, left unfixed** (Assumption 7 / Scope) — per the general-development workflow's
  guidance not to silently widen an issue's scope, this will be filed as a follow-up GitHub issue
  once this PR ships, not fixed inline here.
- **No component-test harness** means the `App.svelte` wiring itself (the `fetchApi` refactor) is
  verified by build success + manual testing, not an automated test — consistent with this repo's
  established practice (same carve-out the #230 plan and others took for `App.svelte` changes).
