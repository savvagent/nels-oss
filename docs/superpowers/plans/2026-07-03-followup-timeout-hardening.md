# Follow-Up Timeout Hardening (raw fetch + generateContent) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the two remaining un-timeout-bounded raw `fetch()` sites in
`frontend/src/App.svelte` (`downloadExport`, `setDefaultBudget`) and the three un-timed-out
`reqwest::Client` sites behind Gemini `generateContent` in `backend/src/rag.rs`, so none of them
can hang a frontend promise or a backend request/connection indefinitely (`savvagent/nels#249`,
the explicit follow-up to `#246`/`#248`).

**Architecture:** Frontend: swap each raw `fetch()` for the already-existing, already-unit-tested
`fetchWithTimeout` helper (`frontend/src/lib/fetchTimeout.js`, added in `#248`), no new module.
Backend: give each of the three `reqwest::Client::new()` builders an explicit `.timeout(...)`,
mirroring `get_gemini_embedding`'s existing pattern in the same file verbatim.

**Tech Stack:** Svelte 5 (Vite/Vitest) for the frontend; Rust (`axum`/`reqwest`/`cargo test`) for
the backend.

---

## Design doc

The full spec (brief, current-state re-verification against `#238`/`#241`'s changes, assumptions,
architecture, error handling, testing approach, risks) is committed at
`docs/superpowers/specs/2026-07-03-followup-timeout-hardening-design.md`. Read it before starting
— this plan implements it task-by-task; it does not repeat the rationale.

## File Structure

- Modify: `frontend/src/App.svelte` — `downloadExport` (~536-561) and `setDefaultBudget`
  (~772-787) each swap `fetch(...)` → `fetchWithTimeout(...)`. `fetchWithTimeout` is already
  imported at `App.svelte:59`; no new import needed.
- Modify: `backend/src/rag.rs` — three `let client = reqwest::Client::new();` sites (~1444, ~4901,
  ~5170) each become a timeout-bounded builder, mirroring `get_gemini_embedding` (`:364-367`).

No new files, no new tests (see design doc's Testing Approach — neither stack has a harness to
exercise these specific call sites, matching the precedent both `get_gemini_embedding` and
`fetchApi`'s own `fetchWithTimeout` wiring set).

---

### Task 1: Frontend — `downloadExport` and `setDefaultBudget` use `fetchWithTimeout`

**Files:**
- Modify: `frontend/src/App.svelte:536-561` (`downloadExport`), `frontend/src/App.svelte:772-787`
  (`setDefaultBudget`)

- [ ] **Step 1: Confirm current state matches the plan's assumptions**

Read `frontend/src/App.svelte:536-561` and `:772-787` fresh (line numbers may have drifted by a
handful of lines from a merge since this plan was written — search for `function downloadExport`
and `function setDefaultBudget` if so). Confirm:
- `downloadExport` still does `await fetch(\`${API_BASE}/account/export\`, { headers: token ? { Authorization: \`Bearer ${token}\` } : {} })`.
- `setDefaultBudget` still does `await fetch(\`${API_BASE}/budgets/${budgetId}/default\`, { method: "POST", headers })`.
- `fetchWithTimeout` is still imported at the top of the file (`import { fetchWithTimeout } from "./lib/fetchTimeout.js";`).

If the shape has changed beyond a line-number drift (e.g. a new caller, a changed signature),
stop and treat as `NEEDS_CONTEXT` — do not guess past a genuine structural change.

- [ ] **Step 2: Edit `downloadExport`**

Current (`frontend/src/App.svelte:536-561`):

```js
  // Download the account data export. fetchApi force-parses JSON, so it can't be
  // used here — the backend returns a JSON file attachment we save via a Blob.
  async function downloadExport() {
    try {
      const res = await fetch(`${API_BASE}/account/export`, {
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (res.status === 401) {
        handleLogout();
        return;
      }
      if (!res.ok) {
        throw new Error((await res.text()) || "Export failed");
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `nels-export-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      triggerSuccess($_("account.exportStarted"));
    } catch (e) {
      triggerError(e.message || $_("account.exportFailed"));
    }
  }
```

Replace the `fetch` line with `fetchWithTimeout`, updating the comment to reflect both reasons it
bypasses `fetchApi` (JSON-forcing AND now the timeout is applied directly, not via `fetchApi`):

```js
  // Download the account data export. fetchApi force-parses JSON, so it can't be
  // used here — the backend returns a JSON file attachment we save via a Blob. Uses
  // fetchWithTimeout directly (same helper fetchApi delegates to) so a hung
  // GET /account/export still eventually rejects instead of leaving this promise
  // pending forever. See #249.
  async function downloadExport() {
    try {
      const res = await fetchWithTimeout(`${API_BASE}/account/export`, {
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (res.status === 401) {
        handleLogout();
        return;
      }
      if (!res.ok) {
        throw new Error((await res.text()) || "Export failed");
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `nels-export-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      triggerSuccess($_("account.exportStarted"));
    } catch (e) {
      triggerError(e.message || $_("account.exportFailed"));
    }
  }
```

- [ ] **Step 3: Edit `setDefaultBudget`**

Current (`frontend/src/App.svelte:772-787`):

```js
  // POST /budgets/:id/default returns 200 with an EMPTY body, so it cannot go
  // through fetchApi (which always res.json()s a non-204 response). Mirror
  // fetchApi's auth header + 401 handling, but do not parse a body.
  async function setDefaultBudget(budgetId) {
    const headers = {};
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetch(`${API_BASE}/budgets/${budgetId}/default`, {
      method: "POST",
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
  }
```

Replace the `fetch` line with `fetchWithTimeout`, updating the comment:

```js
  // POST /budgets/:id/default returns 200 with an EMPTY body, so it cannot go
  // through fetchApi (which always res.json()s a non-204 response). Mirror
  // fetchApi's auth header + 401 handling, but do not parse a body. Uses
  // fetchWithTimeout directly (same helper fetchApi delegates to) so a hung
  // POST /budgets/:id/default still eventually rejects instead of leaving this
  // promise pending forever. See #249. (Owner-only gating happens server-side and
  // via the canSetAsDefault/canSwitchTo checks in this function's callers — both
  // untouched by this change.)
  async function setDefaultBudget(budgetId) {
    const headers = {};
    if (token) headers["Authorization"] = `Bearer ${token}`;
    const res = await fetchWithTimeout(`${API_BASE}/budgets/${budgetId}/default`, {
      method: "POST",
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
  }
```

- [ ] **Step 4: Run the full frontend test suite**

Run: `cd frontend && pnpm test`
Expected: PASS — all existing suites green (`fetchTimeout.test.js`, `commands.test.js`,
`budgetsView.test.js`, `budgetDetails.test.js`, `budgetDisplay.test.js`, `chatQueue.test.js`,
`chatWindow.test.js`, `router.test.js`), no regressions. No new test file is added in this task
(see design doc Testing Approach — no component-test harness exists for `App.svelte`).

- [ ] **Step 5: Run the production build**

Run: `cd frontend && pnpm run build`
Expected: build succeeds — confirms no syntax error was introduced in the `<script>` block edits.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/App.svelte
git commit -m "fix(#249): bound downloadExport/setDefaultBudget raw fetch calls with a timeout"
```

---

### Task 2: Backend — bound all three `generateContent` `reqwest::Client` builders with a timeout

**Files:**
- Modify: `backend/src/rag.rs` (three sites, ~1444 main chat, ~4901
  `generate_conversation_title`, ~5170 `generate_suggested_question`)

- [ ] **Step 1: Confirm current state matches the plan's assumptions**

Read `backend/src/rag.rs` around each of the three sites fresh (search for
`let client = reqwest::Client::new();` — there should be exactly three matches, all followed
within a few lines by a `generativelanguage.googleapis.com/.../generateContent` URL). Also
re-read `get_gemini_embedding` (`:354-367`) to confirm its exact builder pattern is unchanged:

```rust
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
```

If any site's surrounding code has changed shape beyond a line-number drift (e.g. the client is
now shared/passed in rather than constructed inline), stop and treat as `NEEDS_CONTEXT`.

- [ ] **Step 2: Edit the main chat call site (~line 1444)**

Current:

```rust
        let client = reqwest::Client::new();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
            api_key
        );
```

Replace with (30s — this is the call the user is actively waiting on end-to-end; see design doc
Assumption 5 for the reasoning):

```rust
        // Bound the main chat generateContent call: this occupies the request/connection for
        // the synchronous /chat handler, so a stalled Gemini connection must not hang the
        // request indefinitely. 30s leaves room under the frontend's 45s-default fetchApi
        // timeout (#248) so a hang surfaces as this function's existing "communications link is
        // down" fallback response, not a generic client-side timeout. See #249.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
            api_key
        );
```

- [ ] **Step 3: Edit `generate_conversation_title` (~line 4901)**

Current:

```rust
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );
```

Replace with (20s, matching `get_gemini_embedding`'s precedent — a short, single-line,
already-optional output):

```rust
    // Bound the title-generation call: it's awaited inline in the /chat handler (after the
    // reply is prepared, but still before the HTTP response returns), so a stalled connection
    // must not hang the request. 20s matches get_gemini_embedding's existing precedent — this
    // call returns one short line and already degrades gracefully to fallback_title() on any
    // failure. See #249.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );
```

- [ ] **Step 4: Edit `generate_suggested_question` (~line 5170)**

Current:

```rust
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );
```

Replace with (20s, same reasoning as Step 3):

```rust
    // Bound the suggested-question call: same reasoning as generate_conversation_title — a
    // short, single-line, already-optional output (falls back to DEFAULT_SUGGESTED_QUESTION on
    // any failure), 20s matches get_gemini_embedding's existing precedent. See #249.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );
```

- [ ] **Step 5: Build, test, lint**

Run: `cd backend && cargo build`
Expected: builds cleanly, no new warnings.

Run: `cd backend && cargo test`
Expected: PASS — all existing tests in `rag.rs` (125+) and the rest of the backend suite still
green. No new test is added in this task (see design doc Testing Approach — no Gemini test
harness exists in this repo).

Run: `cd backend && cargo clippy --all-targets`
Expected: no new clippy warnings introduced by these three edits.

- [ ] **Step 6: Commit**

```bash
git add backend/src/rag.rs
git commit -m "fix(#249): bound the three generateContent reqwest clients with an explicit timeout"
```

---

### Task 3: Final diff review

**Files:** none (verification only — no code changes in this task).

- [ ] **Step 1: Confirm exactly 5 call sites changed, nothing else**

Run: `git diff origin/main...HEAD --stat`
Expected: exactly two files changed — `frontend/src/App.svelte` and `backend/src/rag.rs` (plus
the two new committed docs from Phase 1/2 of the workflow, if not already committed separately).

Run: `git diff origin/main...HEAD -- frontend/src/App.svelte backend/src/rag.rs`
Expected: exactly 2 `fetch` → `fetchWithTimeout` swaps (plus updated comments) in `App.svelte`,
and exactly 3 `reqwest::Client::new()` → `reqwest::Client::builder()....build()...` swaps (plus
new comments) in `rag.rs`. No other lines touched in either file.

- [ ] **Step 2: Confirm no other raw `fetch()`/unbounded `reqwest::Client` sites were missed or
      accidentally widened into scope**

Run: `grep -n "await fetch(" frontend/src/App.svelte` — expect zero matches (both prior raw
`fetch()` calls are now `fetchWithTimeout`; `fetchApi` itself already uses `fetchWithTimeout`).

Run: `grep -n "reqwest::Client::new()" backend/src/rag.rs` — expect zero matches (all three sites
now use the timeout-bounded builder; `get_gemini_embedding`'s own `unwrap_or_else` fallback branch
still references `reqwest::Client::new()` as its fallback-on-builder-error, which is correct and
unchanged).

If either grep turns up an unexpected site not covered by this plan, do NOT fix it — note it as a
follow-up per the design doc's Risks section and this issue's own scope-discipline instruction.

No commit for this task (verification only).
