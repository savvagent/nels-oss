# Spec: Chat category-balance answers must not navigate away from chat (nels#301)

## 1. Brief (verbatim from the ticket)

> Repro: in a budget with an "Entertainment" category, in the chat ask either "Show me the
> entertainment category amounts." or "How much do I have left in the entertainment category?"
>
> Expected: the assistant answers the question directly inside the chat transcript and the user
> stays on the chat view.
>
> Actual: the assistant's chat bubble reads "Here's your Entertainment balance", but the app then
> navigates away to display the full Categories page, sweeping the user out of chat.
>
> Root cause: the backend correctly selects the CATEGORY_BALANCE action (backend/src/rag.rs,
> prompt rule 20c, match arm ~line 3270-3310) and appends a single-category HTML table via the
> categories_table_html response field - the same field the LIST_CATEGORIES action uses for its
> full-list table (rag.rs ~line 3249-3250).
>
> In the frontend, frontend/src/App.svelte (~line 1291-1298) navigates to the Categories page
> whenever categories_table_html is present at all:
> `if (chatRes.categories_table_html) { navigate("categories"); }`
>
> This gate doesn't distinguish LIST_CATEGORIES (meant to navigate, per #233) from
> CATEGORY_BALANCE (meant to answer inline, per rule 20c) - both actions populate
> categories_table_html, so both trigger navigation.
>
> Acceptance criteria:
> - Asking about a single category's remaining balance or amounts answers inline in the chat
>   transcript and does NOT navigate to the Categories page.
> - Asking to "show/list my categories" (LIST_CATEGORIES) continues to navigate to the Categories
>   page as designed - no regression to #233.
> - The frontend gate keys off the actual action taken (e.g. chatRes.action === "LIST_CATEGORIES",
>   or an equivalent dedicated flag mirroring open_insights/open_budgets_list) rather than the mere
>   presence of categories_table_html.
>
> Note (out of scope): "Can I spend $X on category Y" (affordability) has no dedicated backend
> action — do not fold into this PR; file a follow-up ticket if worth tracking.

## 2. Assumptions

- **A1**: `ChatResponse` has no raw `action` field surfaced to the frontend today (only
  `action_taken`, a human-readable mutation-log string, which stays `None` for both
  `LIST_CATEGORIES` and `CATEGORY_BALANCE` since neither mutates data). The ticket explicitly
  allows "an equivalent dedicated flag mirroring open_insights/open_budgets_list" — that precedent
  (`open_insights: Option<bool>`, `open_budgets_list: Option<bool>`) is the established pattern
  for advisory, non-mutating navigation signals in this codebase, so a new flag of the same shape
  is used instead of exposing the raw action string.
- **A2**: New field name: `open_categories_list: Option<bool>`, mirroring `open_budgets_list`'s
  naming (both are "open the list page for X" signals) rather than `open_categories` (mirroring
  `open_insights`, which isn't a list view).
- **A3**: The flag is set **only** inside the same success branch that currently populates
  `categories_table_html` for `LIST_CATEGORIES` (`Ok(rows) if !rows.is_empty()`), not for every
  `LIST_CATEGORIES` invocation regardless of outcome. This exactly preserves today's navigation
  behavior for `LIST_CATEGORIES` (empty-budget and DB-error outcomes never navigated before, via
  `categories_table_html` staying `None`).
- **A4**: `CATEGORY_BALANCE` never sets the new flag, in any of its four outcomes (found,
  unresolvable name, no active budget, DB error). It keeps `categories_table_html` populated on a
  found category (so the existing `resp.0.categories_table_html` test assertions still pass), but
  the frontend no longer navigates because it stops gating on that field.
- **A5**: The frontend gate at `App.svelte` ~1296-1298 changes from
  `if (chatRes.categories_table_html)` to `if (chatRes.open_categories_list)`, mirroring the
  `if (chatRes.open_insights)` / `if (chatRes.open_budgets_list)` blocks immediately above it.
  `categories_table_html` itself stays discarded (unused) exactly as before — `CategoriesView`
  self-fetches on mount.
- **A6 (verified against the running code, not just asserted)**: chat bubbles render only
  `message_text` (sourced from `chatRes.response`); `categories_table_html` is never interpolated
  into the chat transcript by any code path today — its only consumer is the separate
  `CategoriesView` page. The ticket's "answers inline" is satisfied by (a) the existing prose
  `response` text already stating the balance (confirmed by the pre-existing DB-backed test
  `chat_category_balance_reports_current_period_not_lifetime`) and (b) no longer navigating away.
  Rendering the HTML table itself inside the chat transcript is a distinct enhancement with no
  existing scaffolding; out of scope here and not requested by the acceptance criteria, which only
  requires staying on the chat view.
- **A7**: No DB migration, no new REST endpoint, no config/infra changes. Purely an additive
  optional-boolean response field (`skip_serializing_if = "Option::is_none"`, matching
  `open_insights`/`open_budgets_list`) plus a one-line frontend conditional swap.

## 3. Goal & Success Criteria

Fix the chat UI so `CATEGORY_BALANCE` answers stay in the chat transcript while
`LIST_CATEGORIES` continues to navigate to the Categories page, by keying the frontend's
navigation decision off a dedicated advisory flag instead of the shared `categories_table_html`
payload field.

- [ ] `ChatResponse` gains `open_categories_list: Option<bool>`, serialized like
      `open_insights`/`open_budgets_list`.
- [ ] The `LIST_CATEGORIES` match arm sets `open_categories_list = Some(true)` in exactly the
      branch that currently sets `categories_table_html` (non-empty rows found).
- [ ] The `CATEGORY_BALANCE` match arm never sets `open_categories_list`.
- [ ] `App.svelte`'s navigation gate reads `chatRes.open_categories_list` instead of
      `chatRes.categories_table_html`.
- [ ] Backend tests assert: (a) a `LIST_CATEGORIES` turn with non-empty categories returns
      `open_categories_list: Some(true)`; (b) the existing `CATEGORY_BALANCE` found-category test
      asserts `open_categories_list` is `None`.
- [ ] `cargo test` / `cargo clippy` stay clean on the touched file; `pnpm run test` (frontend)
      stays green.

## 4. Scope

**In scope:** `backend/src/rag.rs` (new field + one match-arm assignment), `frontend/src/App.svelte`
(gate condition swap), backend tests covering the new flag for both actions.

**Out of scope:** the affordability-question gap (separate follow-up issue); rendering
`categories_table_html` inline inside the chat transcript bubble (see A6); any other action's use
of `categories_table_html` (there is none today).

## 5. Architecture / Data Flow

`chat_endpoint` already threads three advisory booleans to the frontend as one-shot per-turn
signals: `open_insights`, `open_budgets_list`, and (new) `open_categories_list`. Each is a plain
`Option<bool>` local, initialized to `None` before the `match parsed_ai_res.action.as_str() { ... }`
block, set to `Some(true)` inside exactly the arm(s) meaning "the frontend should navigate," and
threaded unchanged into the final `ChatResponse { ... }` literal. `App.svelte`'s post-response
handler checks these flags in order and calls the shared `navigate(name)` helper — this ticket adds
a fourth flag to that same pattern, changing only the condition gating the existing
`navigate("categories")` call.

## 6. Error Handling & Edge Cases

- `LIST_CATEGORIES` empty budget / DB error / no active budget: `open_categories_list` stays
  `None` (matches today's `categories_table_html` staying `None`) — no navigation change.
- `CATEGORY_BALANCE` found: `categories_table_html` populated, `open_categories_list` stays `None`
  — the bug fix.
- `CATEGORY_BALANCE`'s other three outcomes: already never populate `categories_table_html`;
  `open_categories_list` stays `None` throughout.
- Serialization: `skip_serializing_if = "Option::is_none"` keeps the wire format unchanged for
  every response that never sets the new field.

## 7. Testing Approach

- Extend `chat_category_balance_reports_current_period_not_lifetime` (the "Found" branch) with
  `assert!(resp.0.open_categories_list.is_none(), ...)`.
- Add a new ignored DB-backed test, `chat_list_categories_sets_open_categories_list_flag`, seeding
  a budget + category, sending "show my categories" through `chat_endpoint`, and asserting
  `resp.0.open_categories_list == Some(true)` and `categories_table_html` contains the category
  name.
- No new frontend test: `App.svelte`'s post-response handler has no existing test harness (only
  `src/lib/*` helpers are unit-tested); verification for the frontend relies on the backend tests
  pinning the flag's truth table plus a manual post-deploy smoke check in Phase 5.

## 8. Risks & Open Questions

- **R1 (accepted)**: No automated frontend test proves the navigation gate itself; mitigated by
  backend tests pinning the flag plus a manual post-deploy smoke check.
- **R2 (SUPERSEDED — see Post-Review Correction below)**: originally accepted "answers inline" as
  satisfied by not navigating away plus the existing prose answer alone. PR review found this
  wrong against the ticket's own text.

## 9. Post-Review Correction (found during PR #304 review, folded into this same PR)

Assumption **A6 was incorrect**. Three independent PR review passes (the general code-reviewer,
the comment-analyzer, and cross-checked against the pr-test-analyzer's coverage read) converged on
the same finding: the ticket's "Expected" section literally reads *"the assistant answers the
question directly inside the chat transcript (e.g. 'Here's your Entertainment balance' **followed
by the limit/spent/remaining figures**)"* — a detail this spec's Brief section quoted only
partially. Tracing the code confirmed the figures never reached the user: rule 20c instructs the
model not to state them in `response_text`, and `categories_table_html` was never rendered
anywhere in the chat transcript (only `CategoriesView.svelte`'s separate, unrelated `{@html}`
render existed). So a fixed CATEGORY_BALANCE turn answered with prose alone — no numbers — which is
not what the ticket asked for.

**Fix (same PR, follow-up commit)**: a new pure helper `inlineCategoriesTableHtml(chatRes)`
(`frontend/src/lib/chatMessage.js`, unit-tested in `chatMessage.test.js`) attaches
`categories_table_html` to the pushed AI chat message precisely when the backend did **not**
signal navigation (`open_categories_list` unset) — i.e. exactly the CATEGORY_BALANCE case.
`App.svelte`'s chat bubble renders that markup inline via `{@html}` (safe for the same reason
`CategoriesView.svelte`'s identical comment gives: backend-built table markup only, never model
free-text). `LIST_CATEGORIES`'s table is unaffected — `inlineCategoriesTableHtml` returns `null`
whenever `open_categories_list` is set, so its table stays discarded and navigation is unchanged.

This correction stayed inside the same PR (not deferred to a follow-up ticket) because it directly
serves this ticket's own stated Expected behavior, is a bounded, low-risk addition (one pure
function + one conditional render, reusing markup the backend already builds), and reviewers
found it before merge — fixing it now is cheaper than closing the ticket against an incomplete
reading of its own acceptance criteria and reopening later.
