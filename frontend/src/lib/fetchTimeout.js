// Wraps the global `fetch` with an AbortController-driven timeout so a request that never
// settles (e.g. a stuck backend/LLM call) always eventually rejects instead of hanging forever.
// Extracted as a pure, framework-free module (mirrors the chatQueue.js/chatWindow.js precedent)
// so it's unit-testable with a mocked `global.fetch` — this repo's vitest runs `environment:
// "node"` and there is no component-test harness for App.svelte itself (see
// fetchTimeout.test.js). App.svelte's fetchApi is the sole caller today. See #246.

// Default timeout (ms) applied by fetchApi (App.svelte) to every request unless the caller
// passes its own `options.timeoutMs`. 45s is a judgment call, not a derived SLA: the one
// concrete data point is backend/src/rag.rs::get_gemini_embedding's 20s reqwest timeout, but
// the backend's /chat generateContent calls are themselves untimed today, so there's no
// end-to-end bound to size this against. 45s just aims to comfortably outlast a live turn while
// still bounding a truly hung request well under a minute, so a caller's try/finally (e.g.
// isChatLoading/isDraining in App.svelte) actually fires. See
// docs/superpowers/specs/2026-07-03-fetchapi-timeout-design.md for the full rationale and open
// questions.
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
