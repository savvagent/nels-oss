# Spec: Allow selecting a budgeting strategy per budget (nels#300)

## 1. Brief (verbatim from the issue)

> **User Story**: As a user managing multiple budgets with different purposes, I want to
> choose a budgeting strategy (e.g. zero-based, 50/30/20, envelope, pay-yourself-first) for
> each budget individually, so that each budget's summary reflects the methodology that
> actually fits it, instead of every budget being forced through the same global toggle.
>
> **Background**: Today, Settings > Budget Summary exposes two global, per-user toggles that
> apply to every budget: "Show zero-based summary" and "Show limit/spent/remaining summary".
> These are stored as `show_zero_based_summary` / `show_limit_spent_remaining_summary` columns
> on the `users` table (per-user, not per-budget) and drive the status strip in the chat view
> (`App.svelte`). There are several recognized budgeting methodologies ... and a single global
> on/off pair can't express that different budgets legitimately follow different strategies.
>
> **Acceptance Criteria**:
> - [ ] Each budget can be assigned a strategy (at minimum: zero-based, traditional
>   limit/spent/remaining; extensible to 50/30/20, envelope, pay-yourself-first later)
>   instead of relying on the two global user-level toggles.
> - [ ] The Budget Summary panel is removed from the Settings page.
> - [ ] The chat status strip reflects the strategy chosen for the active budget rather than
>   a global user preference.
> - [ ] Budgets can only be rolled up together (via `rollup_parent_id`) when they share BOTH
>   the same `budget_type` (time_based/project) AND the same `budget_strategy`; a mismatch in
>   either is rejected with a clear error.
> - [ ] Existing budgets are migrated to a sensible default strategy so current behavior
>   doesn't regress.
> - [ ] When a user creates a budget via chat without specifying a strategy, the assistant
>   asks which strategy to use and presents the available options before creating the budget
>   (rather than silently defaulting).
>
> **Depends on #298** (already merged into `main` as of PR #305, per the issue's own note) —
> the rollup mirror-aggregation fix this issue's rollup-validation tests build on.

Ref: https://github.com/savvagent/nels/issues/300

## 2. Assumptions

1. **Exactly two `budget_strategy` values ship now**: `zero_based` and
   `limit_spent_remaining`. The AC says "at minimum" these two, "extensible... later" to
   50/30/20, envelope, pay-yourself-first. Those three have no defined computation or UI
   anywhere in this codebase today, so inventing placeholder logic for them would be
   speculative scope creep. This mirrors the codebase's own established pattern:
   `budget_type` shipped with exactly `'time_based'` (nels#46) and gained `'project'` as a
   **separate follow-up** issue (nels#48); `amount_mode` shipped with `'derived'`/`'fixed'`
   only. The new column is a plain `TEXT` + `CHECK (... IN (...))`, so adding a third value
   later is a pure additive migration — "extensible" is a property of the design, not a
   requirement to pre-build unused values now.
2. **Migration default resolves per-owner from the two legacy toggles, not a single hardcoded
   value**: for each existing budget, if the owner had `show_zero_based_summary = TRUE`
   (regardless of the other toggle), the budget migrates to `'zero_based'`; otherwise (owner
   had it off, including the "both off" edge case) it migrates to `'limit_spent_remaining'`
   (also the column's own `DEFAULT`). Both legacy toggles defaulted to `TRUE`, so most
   existing users currently see BOTH strips; collapsing to one is an unavoidable, disclosed
   product change (the ticket's own premise — "a single global on/off pair can't express
   this" — implies the two-strips-at-once UI goes away), and prioritizing `zero_based` when
   the owner had it on preserves the richer of the two views rather than picking arbitrarily.
3. **The two legacy `users` columns and the `PATCH /user/preferences` endpoint are removed
   outright**, not just unused. `PreferencesPayload`/`update_preferences` in
   `backend/src/account.rs:496-532` exist **solely** to toggle these two fields — with both
   fields gone the endpoint has zero remaining purpose, and the issue title says "**replace**
   global summary toggles." Leaving dead columns/endpoints around contradicts this
   codebase's demonstrated practice of fully retiring superseded mechanisms (contrast with a
   genuinely orthogonal toggle like `rollover_enabled`, which stays). **Confirmed exhaustive
   consumer list** (grepped both `backend/src` and `frontend/src` for
   `show_zero_based_summary`/`show_limit_spent_remaining_summary`/`showZeroBased`/
   `showLimitSpent` before writing this spec — no other reader/writer exists anywhere in the
   codebase, including `account.rs`'s `AccountExport`, so removal is safe and complete):
   - `backend/src/db.rs:22,25` — `User` struct fields.
   - `backend/src/auth.rs:73-74,86-87` — `MeResponse` fields + the `From<db::User>` mapping;
     `auth.rs:549-558` — tests asserting the round trip.
   - `backend/src/account.rs:502,504,517-522` — `PreferencesPayload` + `update_preferences`;
     `account.rs:645-725` — its tests.
   - `frontend/src/lib/Settings.svelte:30-31,54-55,109-110,121-122,211,217,224,230` — the
     panel's local state, seeding, save payload, and template.
   - `frontend/src/App.svelte:825,828` — the two `showZb`/`showTrad` `$derived`s.
   - `frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json` — the
     `settings.showZeroBased`/`settings.showLimitSpentRemaining` keys (and
     `settings.budgetSummaryTitle`/`settings.prefsSaveFailed` if they become unused once the
     panel is deleted — verify at implementation time).
   All of the above are touched by this issue; nothing is left half-migrated.
4. **REST `POST/PUT /budgets` silently defaults `budget_strategy` when omitted** (to
   `'limit_spent_remaining'`, matching the column default), mirroring exactly how
   `budget_type`/`amount_mode` already default silently on the REST path. The AC's "ask
   before creating" requirement is explicitly scoped to **chat-driven** creation — there is
   no programmatic REST caller in this codebase today (grep confirms the frontend never
   calls `POST /budgets`; all budget creation is chat-driven per `AGENTS.md`), so this only
   affects tests/future integrations, which can always pass the field explicitly.
5. **The chat "ask before creating" requirement is enforced server-side, not only via prompt
   engineering.** The system prompt gets a new directive instructing the LLM to ask (best
   effort, and it can hold the answer across turns via existing chat history the way rule 2b's
   "offer starter categories, create on the next turn" already works). But an LLM can still
   emit `CREATE_BUDGET` without `budget_strategy` despite instructions, so the
   `CREATE_BUDGET` handler **itself** (the single arm both the online-LLM path and the
   offline-router path funnel into — verified at `rag.rs:2003`) blocks the insert and returns
   a clarifying question with the two available options whenever `budget_strategy` is absent
   *or invalid* — deterministically satisfying the AC regardless of model compliance. This
   mirrors the existing `type_valid`/`mode_valid` guards in the same arm, which already block
   the insert on bad input.
6. **Offline router (no `GEMINI_API_KEY`) needs an explicit-keyword-in-the-same-message
   matcher, no silent default.** A new `offline_budget_strategy(msg_lower)` helper mirrors
   `offline_amount_mode`: it recognizes explicit phrases ("zero-based budget", "zero based
   budget" → `zero_based`; "limit budget", "traditional budget", "limit spent remaining
   budget", "spending limit budget" → `limit_spent_remaining`) in the SAME message. If no
   phrase matches, the same server-side guard (Assumption 5) blocks the create and asks — the
   offline caller must resend with an explicit strategy phrase, since the offline router is
   single-message/stateless and cannot hold a pending question across turns (a pre-existing,
   documented limitation of that path, e.g. it also cannot infer `budget_type: project`
   without the literal words "project budget").
7. **Chat `UPDATE_BUDGET` also accepts `budget_strategy`** (not required by the AC, but
   mirrors `amount_mode`/`budget_type`'s existing UPDATE_BUDGET support for consistency, at
   near-zero extra cost, and gives users a chat-native way to change strategy on an existing
   budget rather than only at creation time or via `BudgetDetails.svelte`).
8. **Frontend edit surface**: `BudgetDetails.svelte` already has an inline-editable "Type"
   field (`budget_type`, a `<select>` bound the same way as `time_frame`). Strategy gets an
   identical new editable section (`budgetDetails.js`'s `TIME_FRAMES`-style
   `BUDGET_STRATEGIES` constant + `buildEditPatch`/`buildUpdatePayload` cases), giving a
   direct REST-backed way to set/change strategy without chat — satisfying "each budget can
   be assigned a strategy" independent of the chat flow.
9. **Rollup mismatch error status code**: `409 Conflict`, matching the existing
   already-a-parent/already-a-child violations in `validate_rollup_link`, since a
   type/strategy mismatch is a conflict between two existing resources' attributes, not a
   malformed request shape (that stays `400`, reserved for the self-link case).
10. **Status strip for the two shipped strategies mirrors today's two independent blocks
    exactly** (zero-based: Available/Allocated/Left from `aggregated_effective_amount`
    /`aggregated_base_amount`; traditional: Budgeted/Spent/Remaining from `activeInsight`) —
    only ONE renders now, chosen by `activeBudget.budget_strategy`, instead of both
    independently toggleable. No new computation; `App.svelte`'s existing `$derived`s
    (`zbAvailable`/`zbAllocated`/`zbLeft`, the traditional block's insight-derived values)
    are reused as-is, just re-gated.
11. **No REST/DB test-suite requires a live Postgres for the default `cargo test` run** —
    following the codebase's established `#[ignore = "requires Postgres..."]` convention;
    pure-function tests (validators, offline matchers, the extended `validate_rollup_link`)
    run in the default suite; DB-backed behavior (migration backfill, REST/chat rollup
    rejection, chat CREATE_BUDGET clarification persistence) is `--ignored`.

## 3. Goal & Success Criteria

**Goal**: Replace the two global per-user "show X summary" toggles with a single per-budget
`budget_strategy` enum (`zero_based` | `limit_spent_remaining` today, additively extensible),
threaded through the data model, REST API, chat creation/update flow, rollup-compatibility
validation, and the frontend (Settings removal, status-strip re-gating, a new editable
Strategy field on the budget details page).

Success criteria:
- [x] A budget's strategy is a real column (`budgets.budget_strategy`), settable via REST create/
  update, the `BudgetDetails.svelte` edit UI, and chat CREATE_BUDGET/UPDATE_BUDGET.
  (Tasks 1, 2, 8, 5.)
- [x] The Settings page no longer shows the "Budget summary" panel or the two toggles; the
  `PATCH /user/preferences` endpoint, `PreferencesPayload`, and the two `users` columns are
  gone. (Tasks 1, 6.)
- [x] `App.svelte`'s status strip shows exactly one block (zero-based or traditional) driven by
  the ACTIVE budget's `budget_strategy`, never both, and — once `/insights` has resolved —
  never neither, for a budget with a recognized strategy. (A brief transient gap where a
  `limit_spent_remaining` budget shows nothing while `/insights` is still in flight is a
  **pre-existing** timing characteristic of reusing `activeInsight` for the traditional row,
  not a new regression — see Edge Cases.) (Task 7.)
- [x] Linking a rollup (`POST /budgets/:id/rollup`, chat `ROLLUP_BUDGET`) is rejected with a
  clear message when parent/child `budget_type` differ OR `budget_strategy` differ (today
  neither is checked — the type gap is new-found and closed by this issue). (Task 3.)
- [x] All existing budgets get a real, deterministic `budget_strategy` value from the migration;
  no budget is left with an ambiguous/null state. (Task 1.)
- [x] Chat CREATE_BUDGET without an explicit strategy never silently creates a budget with a
  default strategy — it always asks first, in both the online-LLM and offline-router paths.
  (Task 4, hardened by the follow-up UX fix so the question renders as plain text, not a
  warning.)

## 4. Scope

**In scope**
- New `budgets.budget_strategy` column + CHECK constraint + migration backfill.
- Removal of `users.show_zero_based_summary` / `show_limit_spent_remaining_summary` and the
  `PATCH /user/preferences` endpoint end-to-end (backend + any frontend caller).
- `validate_budget_strategy` + `ALLOWED_BUDGET_STRATEGIES`, threaded into
  `BudgetPayload`/`BudgetListItem`/`db::Budget`/REST create+update handlers.
- `validate_rollup_link` extended to accept and check `budget_type` (previously entirely
  unchecked — a latent gap this issue closes) and `budget_strategy` for both the parent/child
  pair, applied at both call sites (`link_rollup` REST handler, `chat_rollup_budget`).
- Chat: `ActionParams.budget_strategy`, `CREATE_BUDGET`/`UPDATE_BUDGET` handler changes,
  system prompt directive, `offline_budget_strategy` offline-router matcher.
- Frontend: remove Settings "Budget summary" panel; re-gate `App.svelte`'s status strip off
  `activeBudget.budget_strategy` instead of `user.show_*`; add an editable Strategy field to
  `BudgetDetails.svelte` (+ `budgetDetails.js` helpers) mirroring the existing Type field;
  i18n key updates/removals across all 6 locales.
- Tests: unit tests for all new pure functions (validators, offline matcher, extended
  `validate_rollup_link`), `--ignored` DB-backed tests for migration backfill, REST/chat
  rollup rejection, and chat CREATE_BUDGET clarification-then-create; frontend vitest unit
  tests for `budgetDetails.js` additions and the `App.svelte` status-strip derivation logic
  (via existing `budgetDisplay.js`/`budgetDisplay.test.js`-style pure-function extraction if
  the strip's gating logic is presently inline — see Architecture).

**Out of scope**
- Implementing 50/30/20, envelope, or pay-yourself-first as real, computed strategies (no
  allowed values, no UI, no computation — Assumption 1).
- Any change to `computed_budget_total`/`effective_carried`/rollover/fund/auto-renew
  computation logic — `budget_strategy` only changes what's *displayed* and what's
  *rollup-compatible*, never how amounts are computed.
- A dedicated multi-turn "pending clarification" UI mechanism (e.g. the `pending_deletion`
  confirm-dialog pattern) — the clarifying question is plain conversational text via
  `response_text`, matching the existing "ask a clarifying question, wait for the next
  message" precedent used by rules 12/20c.
- Any change to `budget_type`'s own semantics — this issue only adds `budget_strategy`
  alongside it in the rollup-compatibility check.
- Data export (`account.rs` `AccountExport`) — it does not currently surface the two legacy
  toggles and needs no new surfacing of `budget_strategy` (it's already on each exported
  budget row implicitly via whatever budget-serialization path export uses, if any; if
  export doesn't serialize full budget rows today, no new gap is introduced here — verified
  not to regress since the toggles aren't in scope of export either way).

## 5. Architecture

### 5.1 Data model
`backend/migrations/20260704000000_budget_strategy.sql`:
```sql
ALTER TABLE budgets
  ADD COLUMN budget_strategy TEXT NOT NULL DEFAULT 'limit_spent_remaining';

ALTER TABLE budgets
  ADD CONSTRAINT budgets_budget_strategy_check
  CHECK (budget_strategy IN ('zero_based', 'limit_spent_remaining'));

-- Backfill (Assumption 2): budgets owned by a user who had the zero-based
-- toggle on migrate to 'zero_based'; everyone else keeps the column DEFAULT
-- ('limit_spent_remaining') already applied by ADD COLUMN.
UPDATE budgets b
SET budget_strategy = 'zero_based'
FROM users u
WHERE b.owner_id = u.id
  AND u.show_zero_based_summary = TRUE;

-- Retire the superseded global per-user toggles (Assumption 3).
ALTER TABLE users
  DROP COLUMN show_zero_based_summary,
  DROP COLUMN show_limit_spent_remaining_summary;
```
Column order matters: the `UPDATE` backfill must run **before** the `users` columns are
dropped in the same migration (single transaction — sqlx migrations run each file
transactionally), which the ordering above already respects.

### 5.2 Backend — budget.rs
- `pub const ALLOWED_BUDGET_STRATEGIES: [&str; 2] = ["zero_based", "limit_spent_remaining"];`
  and `pub fn validate_budget_strategy(s: &str) -> Result<(), (StatusCode, String)>`, placed
  beside `validate_budget_type`/`validate_amount_mode` (same file, same shape, same error
  style: `"budget_strategy must be 'zero_based' or 'limit_spent_remaining'"`).
- `BudgetPayload.budget_strategy: Option<String>` (`#[serde(default)]`), same
  absent-on-create-defaults / absent-on-update-preserves doc-comment convention as
  `budget_type`/`amount_mode`.
- `BudgetListItem.budget_strategy: String`, threaded through every construction site
  (`create_budget`, `list_budgets`, `get_budget`, `update_budget`, `close_budget`,
  `archive_budget`, `unarchive_budget`, `link_rollup`/`unlink_rollup`'s shared
  `budget_item_for`-equivalent path) — same mechanical thread as `amount_mode` (#116).
- `db::Budget.budget_strategy: String` (`FromRow`).
- `create_budget`: validate (if present) before insert; `let budget_strategy =
  payload.budget_strategy.as_deref().unwrap_or("limit_spent_remaining").to_string();`
  bound into the `INSERT`.
- `update_budget`: validate if present; `COALESCE($n, budget_strategy)` in the `UPDATE`,
  matching `budget_type`'s COALESCE pattern (not `amount_mode`'s bare-bind pattern, since
  budget_strategy has no "clearing" side effect the way project-conversion clears
  `auto_renew` — a plain preserve-on-absent is correct here).
- `validate_rollup_link` signature extended:
  ```rust
  pub fn validate_rollup_link(
      parent_id: Uuid, child_id: Uuid,
      parent_is_child: bool, child_is_parent: bool,
      parent_type: &str, child_type: &str,
      parent_strategy: &str, child_strategy: &str,
  ) -> Result<(), (StatusCode, String)>
  ```
  New checks (after the existing self-link/chain checks, so a self-link or chain violation
  is still reported first — same priority order as today):
  - `parent_type != child_type` → `409`, `"Budgets can only be rolled up together when they
    share the same budget type (both time-based or both project)."`
  - `parent_strategy != child_strategy` → `409`, `"Budgets can only be rolled up together
    when they share the same budgeting strategy."`
  Both call sites (`link_rollup` REST handler at `budget.rs:~2540-2603`, `chat_rollup_budget`
  at `rag.rs:~4110-4296`) already fetch/select the parent+child rows — extend their existing
  `SELECT`s to include `budget_strategy` (mirroring how they already select `budget_type`)
  and pass both new args through. No new DB round-trip.

### 5.3 Backend — rag.rs (chat)
- `ActionParams.budget_strategy: Option<String>` (next to `budget_type`/`amount_mode`,
  `rag.rs:~277-282`), plus the system-prompt JSON-shape doc line (`rag.rs:~1351-1353`):
  `"budget_strategy": "zero_based" | "limit_spent_remaining" (for CREATE_BUDGET / UPDATE_BUDGET; if the user hasn't said which for a NEW budget, do NOT set action to CREATE_BUDGET yet — ask first, see rule 2n),`
- New system-prompt sub-rule **2n** (inserted after 2m, following the existing lettered
  sub-rule convention for budget-family rules):
  > "2n. BUDGETING STRATEGY: Every budget has a strategy — 'zero_based' (tracks how much of
  > the allocated total is still available to spend) or 'limit_spent_remaining' (tracks
  > spending against a limit, shown as Budgeted/Spent/Remaining). When the user asks to
  > CREATE a new budget and has NOT told you which strategy they want, do NOT set 'action' to
  > 'CREATE_BUDGET' yet: set 'action' to 'NONE' and, in 'response_text', ask them to pick one
  > and briefly describe both options. Once they answer (in this message or a later one),
  > proceed with 'action':'CREATE_BUDGET' and 'budget_strategy' set to their choice. To
  > change an existing budget's strategy, set 'action' to 'UPDATE_BUDGET' and set
  > 'budget_strategy'."
- `CREATE_BUDGET` handler (`rag.rs:2003-2094`), the single arm both the online-LLM and
  offline-router paths share: add a `budget_strategy` resolve+validate step alongside the
  existing `type_valid`/`mode_valid` guards:
  ```rust
  let budget_strategy = params.budget_strategy.clone();
  match budget_strategy.as_deref() {
      None => {
          mutation_error = Some(
              "Before I create this budget, which budgeting strategy would you like — \
               'zero-based' (tracks how much of your allocated total is still available \
               to spend) or 'limit/spent/remaining' (tracks spending against a limit)? \
               Let me know and I'll create it.".to_string(),
          );
          // skip insert — same early-continue shape as the existing type_valid/mode_valid arms
      }
      Some(s) => match crate::budget::validate_budget_strategy(s) { ... bind into INSERT ... }
  }
  ```
  This is placed so it short-circuits the insert exactly like the existing
  `type_valid`/`mode_valid` `if let Err` arms (same `else { ... insert ... }` nesting), i.e.
  it's a peer condition in the same guard chain, not a separate code path.
- `UPDATE_BUDGET` handler: add a `budget_strategy` arm mirroring the existing `amount_mode`
  update arm (validate-then-`UPDATE ... SET budget_strategy = $n`), audited under the
  existing `AI_UPDATE_BUDGET` action name (no new audit action needed, matching how
  `auto_renew`/`amount_mode` changes are already folded into that same audit label).
- `offline_budget_strategy(msg_lower: &str) -> Option<&'static str>` mirroring
  `offline_amount_mode`'s shape/tests (`rag.rs:~5177`, tests near `6511-6539`): matches
  `"zero-based budget"` / `"zero based budget"` / `"zero-based strategy"` → `zero_based`;
  `"limit budget"` / `"limit/spent/remaining budget"` / `"limit spent remaining budget"` /
  `"traditional budget"` / `"traditional strategy"` → `limit_spent_remaining`. Wired into the
  offline CREATE_BUDGET branch (`rag.rs:~1734-1763`) the same way `offline_amount_mode` is
  wired at `~1813-1820`.

### 5.4 Frontend
- **Settings.svelte**: delete the "Budget summary" panel (lines ~205-239) and its bound
  state (`showZeroBased`/`showLimitSpent`, the `$effect` seeding them, the `savePrefs()` PATCH
  call if it does nothing else — check for any other field `savePrefs` touches before
  deleting the function wholesale; if it's dual-purpose, remove only the two fields, not the
  function). Delete now-unused i18n keys (`settings.budgetSummaryTitle`,
  `settings.showZeroBased`, `settings.showLimitSpentRemaining`, `settings.prefsSaveFailed` if
  it becomes unused) from all 6 locale files.
- **App.svelte** (`~482-488`, `~794-830`, `~2136-2169`): the `user` fetch at login no longer
  carries the two prefs (removed server-side); the four `$derived`s
  (`zbAvailable`/`zbAllocated`/`zbLeft`/`showZb`/`showTrad`) collapse: `showZb =
  !!activeBudget && activeBudget.budget_strategy === "zero_based"`, `showTrad = !!activeBudget
  && activeBudget.budget_strategy === "limit_spent_remaining" && !!activeInsight`. The two
  render blocks (2144-2158, 2159-2168) are unchanged internally — only their gating condition
  changes. `activeBudget` must carry `budget_strategy` — confirm it comes from the
  `BudgetListItem` the frontend already holds for the active budget (it does, once 5.2's
  threading lands).
- **BudgetDetails.svelte** / **budgetDetails.js**: add `export const BUDGET_STRATEGIES =
  ["zero_based", "limit_spent_remaining"];`, extend `buildEditPatch`/`buildUpdatePayload` with
  a `budget_strategy` case (patched the same way as `budget_type` — included only when
  explicitly changed, server-COALESCEd otherwise), and add a new editable "Strategy" section
  in the template immediately after the existing "Type" section, same `<select>`/edit-affordance
  pattern, with a translated label per option (not raw `zero_based`/`limit_spent_remaining`
  strings).
- i18n: add `budgetDetails.strategyLabel`, `budgetDetails.strategyZeroBased`,
  `budgetDetails.strategyLimitSpentRemaining` (and any status-strip label reuse — check
  whether the strip already has its own zero-based/traditional labels it can reuse rather
  than duplicating) to all 6 locale files.

### 5.5 Call graph summary (who calls what, so nothing is missed)
`create_budget` (REST) / chat `CREATE_BUDGET` arm / `update_budget` (REST) / chat
`UPDATE_BUDGET` arm all independently validate+persist `budget_strategy` — no shared helper
today for budget_type/amount_mode validation-and-bind either, so this is consistent with
existing duplication, not a new pattern to refactor away in this issue.

## 6. Error Handling & Edge Cases

- **Invalid `budget_strategy` string on REST create/update** → `400` with
  `validate_budget_strategy`'s message, before any DB write (mirrors `budget_type`).
- **Invalid `budget_strategy` string via chat** → the same validation error surfaces as a
  `mutation_error` (⚠️ warning notice), insert/update skipped (mirrors `amount_mode`).
- **Missing `budget_strategy` on chat CREATE_BUDGET** → NOT an error; a clarifying question
  (Assumption 5), insert skipped, no audit row written (nothing happened yet).
- **Rollup type/strategy mismatch** → `409` from both REST and chat, insert/link skipped;
  existing self-link (`400`) and chain-violation (`409`) checks still take priority if
  multiple conditions are true simultaneously (checked in the existing order, new checks
  appended after).
- **Existing rollup links at migration time**: the migration does **not** retroactively
  validate/unlink any `rollup_parent_id` pairs that predate this column (a parent+child pair
  linked before this issue might now have diverged `budget_strategy`, e.g. parent defaulted
  to `limit_spent_remaining` while the child's owner had zero-based on). This is a
  **deliberate, disclosed non-goal**: the new check only gates *future* link attempts
  (`link_rollup`/`chat_rollup_budget`), never unwinds existing state — consistent with every
  prior additive-constraint migration in this codebase (e.g. #52's own CHECK constraints
  never retroactively broke pre-existing rows). Flagged explicitly under Risks below.
- **Transient "neither row shows" gap for a `limit_spent_remaining` budget while
  `/insights` is still loading**: `showTrad` depends on `activeInsight`, which is only
  populated after `loadInsights()`'s async fetch resolves. Today, with both toggles
  independently true by default, this gap is invisible for most users because the
  zero-based row (which has no such dependency) is usually still showing during that window.
  Post-migration, a budget whose *sole* strategy is `limit_spent_remaining` has no fallback
  row during that same window — a brief (one fetch round-trip) empty strip, not a crash or
  stuck state, and it resolves itself as soon as `/insights` returns (same failure-non-fatal
  contract the existing code comment at `App.svelte:794-799` already documents for this
  fetch). Accepted as a pre-existing characteristic of the reused `/insights` dependency,
  not a new defect this issue introduces — no loading spinner is added for this narrow
  window, matching the proportionate-effort bar for this issue.
- **A budget with no active status-strip match** (a future third strategy value reaching
  `App.svelte` before the frontend knows how to render it) — out of scope today since only
  two values exist, but the frontend gating uses exact string equality per strategy (not an
  `else`), so an unrecognized value simply renders neither strip rather than crashing — a
  safe default for forward-compatibility.
- **`PATCH /user/preferences` removal is a breaking API change** — no versioning/deprecation
  window is used elsewhere in this codebase for removed endpoints (grep of git history shows
  prior removals, e.g. superseded rollup REST shape in #52, were direct cuts), so a direct
  removal matches convention; the frontend caller is removed in the same PR so no client ever
  calls the dead route in production.

## 7. Testing Approach

- **Unit (default `cargo test`, no DB/network)**:
  - `validate_budget_strategy`: valid values pass, invalid value → 400 with expected message.
  - `validate_rollup_link` (extended): every existing case still passes with matching
    type/strategy; new cases — same type+diff strategy → 409, diff type+same strategy → 409,
    diff type+diff strategy → 409, verify self-link/chain-violation still short-circuit before
    the new checks fire (ordering test).
  - `offline_budget_strategy`: positive matches for both values' phrasings, negative
    (unrelated messages) → `None`, near-miss phrasing (e.g. "compare budgets" containing
    "budget" but not a strategy phrase) → `None` (mirrors `offline_amount_mode`'s existing
    false-positive-avoidance test shape).
  - `BudgetPayload`/serde round-trip: `budget_strategy` present/absent deserialize correctly
    (mirrors existing `amount_mode` Option-semantics tests).
- **DB-backed (`--ignored`, local Postgres)**:
  - Migration backfill: seed users with each of the 4 toggle combinations, run migration
    (or assert against a freshly-migrated test DB, per this repo's existing pattern of
    testing backfill logic via direct SQL assertions against `sqlx::migrate!`'s already-run
    schema — confirm the exact idiom used by a prior migration's test, e.g. #52's), assert
    each resulting budget's `budget_strategy`.
  - `create_budget`/`update_budget` REST: persist + return `budget_strategy` correctly,
    reject invalid value.
  - `link_rollup` REST: same-type/same-strategy succeeds (mirror-creation still works, #52
    regression guard); type-mismatch and strategy-mismatch both rejected with 409 and no
    mirror category created.
  - `chat_rollup_budget`: same coverage as REST, chat-flavored (name resolution, audit log
    NOT written on rejection).
  - Chat `CREATE_BUDGET`: message without a strategy phrase → clarifying `response_text`,
    zero budgets exist in the DB afterward; message with an explicit online JSON
    `budget_strategy` → budget created with that value; offline message with an explicit
    strategy phrase → same; offline message without one → clarifying response, zero budgets
    created.
  - Chat `UPDATE_BUDGET budget_strategy`: existing budget's strategy changes, audited.
- **Frontend (vitest)**:
  - `budgetDetails.test.js`: extend for `BUDGET_STRATEGIES`, `buildEditPatch`/
    `buildUpdatePayload`'s new `budget_strategy` case (unchanged-value → null, changed → patch).
  - Status-strip gating: if the `showZb`/`showTrad` derivation is simple enough to stay
    inline in `App.svelte` (no extracted pure function today), verify via reading the actual
    current code structure at implementation time whether it's already a candidate for
    extraction to a testable pure helper (`budgetDisplay.js`) — if App.svelte's existing
    `$derived`s are trivially inline and untested today (confirm at implementation time), no
    new test infrastructure is invented purely for this issue beyond what's proportionate;
    if a pure-helper extraction is cheap and mirrors an existing convention, do it and test
    it, otherwise document the decision inline.

## 8. Risks & Open Questions

- **Two-strips-become-one is a visible, disclosed UX regression for any user who currently
  has BOTH legacy toggles on** (the pre-existing default) — accepted per the ticket's own
  premise (Assumption 2); no rollback path needed since this is intentional product
  direction, not a bug.
- **Pre-existing rollup links that would now be rejected if re-validated are left alone**
  (Error Handling section) — a follow-up cleanup issue could audit existing
  `rollup_parent_id` pairs for divergent `budget_strategy` post-migration, but that's a data
  hygiene task orthogonal to this issue's validation-on-write scope.
- **Prompt-only enforcement is inherently probabilistic**; mitigated by the deterministic
  server-side guard (Assumption 5) so the acceptance criterion holds regardless of what the
  LLM actually emits.
- **Whether `savePrefs()` in Settings.svelte does anything beyond the two removed toggles**
  needs a fresh read at implementation time before deciding to delete vs. trim the function
  (flagged in 5.4 rather than assumed).
- **The migration's cross-table backfill logic (§7's planned DB-backed test) could not be
  empirically exercised against a live migration run, by design of this repo's harness — a
  deliberate, disclosed deferral, not an oversight.** `sqlx::migrate!` applies every migration
  file exactly once, in order, the first time a pool connects to a given database; this
  migration ALSO drops the two `users` toggle columns it reads from in the same file/
  transaction as the backfill. There is no hook in this repo's test harness to pause
  mid-`migrate!` and seed toggle values on the four users/no-budgets-yet combinations
  *between* migrations — by the time any test can run against the shared local Postgres, this
  migration (and the column drop) has already executed once, so the toggle columns are simply
  gone and cannot be re-seeded to re-exercise the backfill's `WHERE
  u.show_zero_based_summary = TRUE` branch against fresh data. Building a from-scratch
  isolated-schema harness to pause between migration files has no precedent anywhere in this
  codebase (every prior migration with default-only, no-cross-table backfill logic — #47/#48/
  #50/#51 — never needed one), so it was judged disproportionate to invent solely for this
  one-time migration. Mitigating factors: (1) the backfill SQL is a single, simple three-line
  `UPDATE ... FROM ... WHERE` reviewed line-by-line in the Task 1 code-quality review and
  matched, statement-for-statement, against this spec's §5.1 and AGENTS.md §13's description
  of the same logic; (2) it ran successfully with no SQL error against this repo's real local
  dev/test Postgres (which already carries pre-existing seeded data) as part of every
  subsequent task's `cargo test -- --ignored` run; (3) the blast radius of a hypothetically
  wrong backfill decision is a budget showing the wrong (but still valid, still-functional)
  status-strip strategy — user-correctable in one click via the new `BudgetDetails.svelte`
  editable Strategy field (Task 8), never data loss, a crash, or an unrecoverable state.
