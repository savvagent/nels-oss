// Parses a successful backend response body for App.svelte's `fetchApi`, tolerating any
// bodiless 2xx (not just `204 No Content`). Extracted as a pure, framework-free module
// (mirrors the fetchTimeout.js precedent from #246) so it's unit-testable with a plain
// Response-like stub — this repo's vitest runs `environment: "node"` and there is no
// component-test harness for App.svelte itself. `fetchApi` is the sole caller today.
//
// WHY this exists (#366): the backend's `refresh_linked_account_handler` returns
// `202 Accepted` with an EMPTY body. fetchApi previously special-cased only `204`, then
// unconditionally called `res.json()` for every other 2xx — so the empty `202` body made
// `res.json()` throw `DOMException: Unexpected end of JSON input`, and that message
// propagated straight to the user as a bogus "error" on an otherwise-successful Refresh.
//
// WHY text-then-parse instead of `res.json()`: `res.json()` gives us no chance to inspect
// an empty body before it blows up. Reading the body as text first lets us treat an empty
// string as "no content" (return null) and only `JSON.parse` a non-empty body — which is
// behavior-equivalent to `res.json()` for every response that actually carries JSON.
//
// WHY generic rather than just adding `202` to the `204` check: any bodiless 2xx success
// (a future 200/201/202/205 with no body) should be handled the same way. Keying off the
// actual body content ("is it empty?") rather than a hard-coded status allowlist hardens
// ALL of fetchApi's callers against this class of parse error, not just today's refresh.
export async function parseApiResponse(res) {
  // 204 No Content is defined to carry no body — short-circuit without touching it.
  if (res.status === 204) return null;

  const text = await res.text();
  // Empty body on any other 2xx (e.g. the 202 from a linked-account refresh) → null,
  // exactly as if it were a 204, instead of feeding "" to JSON.parse.
  if (text === "") return null;

  return JSON.parse(text);
}
