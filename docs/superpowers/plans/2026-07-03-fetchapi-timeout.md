# fetchApi Request Timeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound every `fetchApi` call (`frontend/src/App.svelte`) to a finite timeout via a new
`AbortController`-based `fetchWithTimeout` helper, so a hung request (stuck backend/LLM call)
always eventually rejects instead of leaving `isChatLoading`/`isDraining` stuck forever
(`savvagent/nels#246`).

**Architecture:** Extract a pure, framework-free `fetchWithTimeout(url, options, timeoutMs,
timeoutMessage)` into a new `frontend/src/lib/fetchTimeout.js` (unit-tested with a mocked
`global.fetch`, mirroring the existing `chatQueue.js`/`chatWindow.js` extraction precedent — this
repo's `vitest` has no component-test harness for `App.svelte` itself). `fetchApi` becomes a thin
wrapper: it destructures its own `timeoutMs` override out of `options` (so it isn't accidentally
spread into the native `fetch()` call), delegates to `fetchWithTimeout`, and keeps its existing
401/`!res.ok`/204/JSON handling unchanged.

**Tech Stack:** Svelte 5 (Vite), Vitest (`environment: "node"`), `AbortController`/`fetch` (Web
API, also available as Node's global `fetch`/`undici` under Vitest).

---

## Design doc

The full spec (brief, current-state verification against the shipped #230/#243 FIFO queue,
assumptions, architecture, error handling, testing approach, risks) is committed at
`docs/superpowers/specs/2026-07-03-fetchapi-timeout-design.md`. Read it before starting — this
plan implements it task-by-task; it does not repeat the rationale.

## File Structure

- Create: `frontend/src/lib/fetchTimeout.js` — the pure `fetchWithTimeout` helper +
  `DEFAULT_FETCH_TIMEOUT_MS`/`TIMEOUT_MESSAGE` constants. One responsibility: wrap `fetch` with an
  abort-on-timeout.
- Create: `frontend/src/lib/fetchTimeout.test.js` — unit tests for the above, co-located per this
  repo's existing convention (`chatQueue.js`/`chatQueue.test.js`, etc.).
- Modify: `frontend/src/App.svelte` — add one import; replace the body of `fetchApi`
  (currently `:307-333`) to delegate to `fetchWithTimeout` and forward an optional
  `options.timeoutMs`.

---

### Task 1: `fetchTimeout.js` — the pure timeout helper

**Files:**
- Create: `frontend/src/lib/fetchTimeout.js`
- Test: `frontend/src/lib/fetchTimeout.test.js`

- [ ] **Step 1: Write the failing tests**

Create `frontend/src/lib/fetchTimeout.test.js`:

```js
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  fetchWithTimeout,
  DEFAULT_FETCH_TIMEOUT_MS,
  TIMEOUT_MESSAGE,
} from "./fetchTimeout.js";

// A fetch mock that mimics the real fetch/AbortController contract: given an AbortSignal, the
// returned promise stays pending until EITHER the signal aborts (rejects with an AbortError,
// exactly like a real aborted fetch) OR the test's own `settle(resolve, reject)` hook fires.
function mockFetchUntilAbortOr(settle) {
  return vi.fn((url, options) => {
    return new Promise((resolve, reject) => {
      options?.signal?.addEventListener("abort", () => {
        const err = new Error("The operation was aborted.");
        err.name = "AbortError";
        reject(err);
      });
      settle(resolve, reject);
    });
  });
}

describe("fetchWithTimeout", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("rejects with a clear timeout error when fetch never settles, and aborts the signal", async () => {
    const fetchMock = mockFetchUntilAbortOr(() => {}); // never resolves/rejects on its own
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithTimeout("https://example.test/api", {}, 1000);
    const assertion = expect(promise).rejects.toThrow(TIMEOUT_MESSAGE);
    await vi.advanceTimersByTimeAsync(1000);
    await assertion;

    const passedSignal = fetchMock.mock.calls[0][1].signal;
    expect(passedSignal.aborted).toBe(true);
  });

  it("resolves normally when fetch settles before the timeout, and clears the timer", async () => {
    const fakeResponse = { ok: true, status: 200 };
    const fetchMock = mockFetchUntilAbortOr((resolve) => resolve(fakeResponse));
    vi.stubGlobal("fetch", fetchMock);

    const result = await fetchWithTimeout("https://example.test/api", {}, 1000);

    expect(result).toBe(fakeResponse);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("rethrows a genuine (non-abort) network error unchanged", async () => {
    const networkError = new TypeError("Failed to fetch");
    const fetchMock = mockFetchUntilAbortOr((_resolve, reject) => reject(networkError));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchWithTimeout("https://example.test/api", {}, 1000)).rejects.toBe(
      networkError,
    );
  });

  it("honors a custom timeoutMs and timeoutMessage", async () => {
    const fetchMock = mockFetchUntilAbortOr(() => {});
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithTimeout(
      "https://example.test/api",
      {},
      500,
      "Custom timeout message",
    );
    const assertion = expect(promise).rejects.toThrow("Custom timeout message");
    await vi.advanceTimersByTimeAsync(500);
    await assertion;
  });

  it("passes an existing options.signal straight through with no timeout applied", async () => {
    const externalController = new AbortController();
    const fakeResponse = { ok: true, status: 200 };
    const fetchMock = vi.fn(async () => fakeResponse);
    vi.stubGlobal("fetch", fetchMock);

    const result = await fetchWithTimeout(
      "https://example.test/api",
      { signal: externalController.signal },
      1000,
    );

    expect(result).toBe(fakeResponse);
    expect(fetchMock).toHaveBeenCalledWith("https://example.test/api", {
      signal: externalController.signal,
    });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("exports a sane positive default timeout", () => {
    expect(DEFAULT_FETCH_TIMEOUT_MS).toBeGreaterThan(0);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && pnpm test -- fetchTimeout`
Expected: FAIL — `Failed to resolve import "./fetchTimeout.js"` (the module doesn't exist yet).

- [ ] **Step 3: Write the implementation**

Create `frontend/src/lib/fetchTimeout.js`:

```js
// Wraps the global `fetch` with an AbortController-driven timeout so a request that never
// settles (e.g. a stuck backend/LLM call) always eventually rejects instead of hanging forever.
// Extracted as a pure, framework-free module (mirrors the chatQueue.js/chatWindow.js precedent)
// so it's unit-testable with a mocked `global.fetch` — this repo's vitest runs `environment:
// "node"` and there is no component-test harness for App.svelte itself (see
// fetchTimeout.test.js). App.svelte's fetchApi is the sole caller today. See #246.

// Default timeout (ms) applied by fetchApi (App.svelte) to every request unless the caller
// passes its own `options.timeoutMs`. 45s comfortably covers a slow-but-live /chat turn (a
// heavier LLM generation call than the 20s-timed-out embedding call in
// backend/src/rag.rs::get_gemini_embedding) while still bounding a truly hung request to well
// under a minute, so a caller's try/finally (e.g. isChatLoading/isDraining in App.svelte)
// actually fires.
export const DEFAULT_FETCH_TIMEOUT_MS = 45000;

// Message used for the Error thrown when a request times out. Deliberately NOT the browser/Node
// AbortError's native message (varies by runtime, e.g. "The user aborted a request."/"This
// operation was aborted") — callers (fetchApi's catch sites) surface `err.message` directly to
// the user, so it needs to be a clear, stable string.
export const TIMEOUT_MESSAGE = "Request timed out";

// Calls `fetch(url, options)`, aborting and rejecting with `Error(timeoutMessage)` if it hasn't
// settled within `timeoutMs`. A genuine (non-timeout) fetch rejection — e.g. a network failure —
// is rethrown unchanged, never conflated with a timeout. If `options.signal` is already set, the
// caller has taken over cancellation: this function passes the call straight through to `fetch`
// with no timeout applied (forward-compatible with a future caller that wants to own its own
// AbortController, e.g. a stop-generation feature — not exercised by any current call site).
export async function fetchWithTimeout(
  url,
  options = {},
  timeoutMs = DEFAULT_FETCH_TIMEOUT_MS,
  timeoutMessage = TIMEOUT_MESSAGE,
) {
  if (options.signal) {
    return fetch(url, options);
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, { ...options, signal: controller.signal });
  } catch (err) {
    if (err.name === "AbortError") {
      throw new Error(timeoutMessage);
    }
    throw err;
  } finally {
    clearTimeout(timer);
  }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && pnpm test -- fetchTimeout`
Expected: PASS — 6 tests passed.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/fetchTimeout.js frontend/src/lib/fetchTimeout.test.js
git commit -m "feat(#246): add AbortController-based fetch timeout helper"
```

---

### Task 2: Wire `fetchApi` (`App.svelte`) to `fetchWithTimeout`

**Files:**
- Modify: `frontend/src/App.svelte:43-49` (imports), `:307-333` (`fetchApi`)

- [ ] **Step 1: Add the import**

In `frontend/src/App.svelte`, immediately after the existing `chatQueue.js` import block (ends
`:49` with `} from "./lib/chatQueue.js";`), add:

```js
  import { fetchWithTimeout } from "./lib/fetchTimeout.js";
```

So the import block reads:

```js
  import {
    QUEUE_CAP,
    canEnqueue,
    enqueue,
    removeFromQueue,
    runDrain,
  } from "./lib/chatQueue.js";
  import { fetchWithTimeout } from "./lib/fetchTimeout.js";
```

- [ ] **Step 2: Replace `fetchApi`'s body**

Find the existing `fetchApi` function (`frontend/src/App.svelte:307-333`):

```js
  async function fetchApi(endpoint, options = {}) {
    const headers = {
      "Content-Type": "application/json",
      ...options.headers,
    };
    if (token) {
      headers["Authorization"] = `Bearer ${token}`;
    }

    const res = await fetch(`${API_BASE}${endpoint}`, {
      ...options,
      headers,
    });

    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }

    if (!res.ok) {
      const errText = await res.text();
      throw new Error(errText || "API error");
    }

    if (res.status === 204) return null;
    return await res.json();
  }
```

Replace it with:

```js
  async function fetchApi(endpoint, options = {}) {
    // `timeoutMs` is fetchApi's own extension to `options` (not a native RequestInit key), so
    // it must be pulled out before the rest is spread into fetchWithTimeout/fetch — otherwise it
    // would ride along as an unrecognized key on the native fetch() call. See #246.
    const { timeoutMs, ...fetchOptions } = options;
    const headers = {
      "Content-Type": "application/json",
      ...fetchOptions.headers,
    };
    if (token) {
      headers["Authorization"] = `Bearer ${token}`;
    }

    const res = await fetchWithTimeout(
      `${API_BASE}${endpoint}`,
      {
        ...fetchOptions,
        headers,
      },
      timeoutMs,
    );

    if (res.status === 401) {
      handleLogout();
      throw new Error("Unauthorized");
    }

    if (!res.ok) {
      const errText = await res.text();
      throw new Error(errText || "API error");
    }

    if (res.status === 204) return null;
    return await res.json();
  }
```

(`timeoutMs` being `undefined` when no caller supplies it is intentional — `fetchWithTimeout`'s
own default parameter (`DEFAULT_FETCH_TIMEOUT_MS`) applies in that case, matching how a JS
default parameter resolves an explicit `undefined` argument.)

- [ ] **Step 3: Run the full frontend test suite**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing suites (`chatQueue.test.js`, `chatWindow.test.js`,
`budgetDisplay.test.js`, `router.test.js`) plus the new `fetchTimeout.test.js` green, no
regressions.

- [ ] **Step 4: Run the production build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds (confirms the new import resolves and there's no syntax error in the
`<script>` block edit).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "fix(#246): give fetchApi an AbortController-based timeout"
```

---

### Task 3: Manual verification pass

No further automated coverage is possible for the `App.svelte` wiring itself — this repo's
`vitest` runs `environment: "node"` with no component-test harness (confirmed in Task 1's design
doc and every existing `*.test.js`), so `performSend`/`drainQueue`'s reaction to a timed-out
`fetchApi` call can only be exercised by driving the running app.

**Files:** none (verification only — no code changes in this task).

- [ ] **Step 1: Force a timeout and confirm the chat path unsticks**

Start the backend (`cd backend && cargo run`) and the frontend dev server
(`cd frontend && pnpm run dev`). Temporarily lower `DEFAULT_FETCH_TIMEOUT_MS` in
`frontend/src/lib/fetchTimeout.js` to something short (e.g. `2000`), or simpler: stop the backend
process after the frontend has loaded so `POST /chat` never gets a response. Log in, send a chat
message, and confirm:
- After the timeout elapses, a chat error bubble appears containing "Request timed out".
- The composer re-enables (`isChatLoading` reset) — you can type and submit again.
- Send/queue two more messages in quick succession *before* the first timeout resolves (tests the
  drain path): confirm they queue, and once the first item times out, the queue drains the rest
  in order (each either succeeds or times out on its own) rather than staying stuck — the "Queue
  full" hint (visible only once you hit 10 queued) clears once the count drops back under the cap.

Revert the temporary `DEFAULT_FETCH_TIMEOUT_MS` edit (or restart the backend) before moving on.

- [ ] **Step 2: Confirm the happy path and the `timeoutMs` override are both unaffected/reachable**

With the backend running normally, send a normal chat message and confirm it round-trips exactly
as before (no perceptible change, no new console errors). Then, in a scratch browser console call
(or a temporary one-line edit to a `fetchApi` call site, reverted after), pass an explicit short
`timeoutMs` (e.g. `fetchApi("/budgets", { timeoutMs: 1 })`) and confirm it rejects near-instantly
with "Request timed out" — proving `fetchApi`'s destructure-and-forward wiring (Task 2, Step 2)
actually reaches `fetchWithTimeout`, not just the module-level default.

- [ ] **Step 3: Confirm a genuine network error still reads as a network error, not a timeout**

With the backend stopped and `VITE_API_BASE` pointed at an unreachable host (or just leave the
backend down, same effect for `localhost:3000`), send a chat message and confirm the resulting
error bubble is the browser's own connection-refused-style message (e.g. "Failed to fetch"), not
"Request timed out" — confirming the two failure modes aren't conflated.

No commit for this task (verification only).
