# Spec: Per-viewer active-budget preference (nels#255)

## Brief (verbatim from ticket)

> `budgets.is_default` is owner-scoped (`unique_default_budget_per_user` partial
> index on `owner_id`) — it represents "which of my own budgets is my default,"
> not "which budget (owned or shared) is currently active for me." #251
> correctly restricted `POST /budgets/:id/default` to `Permission::Owner`...
>
> Proposal: introduce a proper per-user "active budget" preference — e.g. a
> `user_active_budget` mapping (or a column on `users`) pointing at a `budget_id`
> the viewer wants to see by default, decoupled from the owner-scoped
> `is_default` column. `GET /budgets` (or a new lightweight endpoint) would
> resolve "my active budget" from that preference when set, falling back to the
> owner's own `is_default` budget otherwise. `/budgets-switch` would write to
> this new preference instead of `is_default`, so it works uniformly for owned
> and shared budgets without ever mutating another owner's row.
>
> Scope note: not required ... use judgment on scope — a minimal, clean
> implementation of the proposed per-user preference (schema + read-path
> fallback + `/budgets-switch` wiring) is preferred over gold-plating.

Additional caller constraints:
- Coordinate with #266 (not yet landed as of this writing — confirmed by reading
  current `frontend/src/App.svelte` `fetchBudgets()`, which still has the
  un-scoped `budgets.find((b) => b.is_default) || budgets[0]` bug). This
  ticket's preference must sit ABOVE whatever `fetchBudgets()`'s existing
  fallback resolves to, not replace or fight it, so #266 can land independently
  before/after in either order.
- Build on #251 (owner-only `/default`, `canSetAsDefault`, `switchNotOwned`) and
  #249 (`setDefaultBudget` uses `fetchWithTimeout` directly, not `fetchApi`,
  because `/default` returns 200 with an empty body).

## Assumptions

1. **Column on `users`, not a join table.** The ticket offers both as options
   ("a `user_active_budget` mapping (or a column on `users`)"). A single
   nullable `users.active_budget_id UUID REFERENCES budgets(id) ON DELETE SET
   NULL` is one column, needs no join, and the existing FK `ON DELETE SET NULL`
   idiom (already used for `budgets.rollup_parent_id`) gives automatic,
   race-free cleanup when the referenced budget is deleted — a join table would
   need an explicit cleanup step for the same guarantee. *Rationale: minimal
   schema for a 1:1 "at most one preference per user" relationship.*
2. **New endpoint `POST /budgets/:id/activate`, not a body-based PATCH.** Mirrors
   the existing `POST /budgets/:id/default` shape (path-scoped id, empty 200
   body) so the frontend's existing `fetchWithTimeout`-based raw-fetch pattern
   (#249) can be reused near-verbatim, and the permission gate composes with the
   existing `check_permission` helper. *Rationale: consistency with the
   established sibling endpoint; avoids introducing a second REST shape for the
   same "point activity at a budget" verb.*
3. **Permission gate: any access level (`perm != Permission::None`), not
   Owner-only.** This is the entire point of the ticket — a View/Edit sharee
   must be able to set a shared budget as their own active view without
   touching the owner's row. The endpoint never writes to the `budgets` table,
   only to the caller's own `users` row, so there is no cross-owner mutation
   risk analogous to #238. *Rationale: ticket's explicit goal.*
4. **`GET /budgets` (`list_budgets`) gains a new `is_active: bool` field on
   `BudgetListItem`, populated meaningfully only in `list_budgets`, and
   defaulted `false` at every other `BudgetListItem` construction site**
   (`create_budget`, `get_budget`, `update_budget`, `close_budget`,
   `archive_budget`, `unarchive_budget`, `budget_item_for` used by
   link/unlink-rollup). This exactly mirrors the existing, documented
   `owner_name` field precedent in the same struct ("Populated only by
   `list_budgets`..."). *Rationale: reuse an established, already-understood
   codebase convention rather than inventing a new one; avoids a query fan-out
   into 8 single-budget handlers that don't need the value.*
5. **No new endpoint for "my active budget" alone.** The ticket says "`GET
   /budgets` (or a new lightweight endpoint)" — parenthetical, not required.
   Adding `is_active` to the existing list response is strictly less surface
   area than adding a second endpoint the frontend would then have to
   reconcile against the list. *Rationale: minimal-scope preference stated by
   the ticket itself.*
6. **Frontend resolution order stays additive, layered above the existing
   `fetchBudgets()` fallback verbatim** (whatever it is at HEAD, bug or no bug):
   `budgets.find(b => b.is_active) || <existing fallback expression, untouched>`.
   This ticket does NOT fix #266's bug (out of scope — a separate, concurrently
   assigned ticket) and does not touch the existing fallback line beyond
   prepending the new preference check. *Rationale: ticket's own explicit
   sequencing instruction — "sits ABOVE that fix... rather than conflicting
   with or reverting it."*
7. **`/budgets-switch` unconditionally calls the new endpoint for any resolved
   target** (owned or shared) — `resolveBudgetByName` already only returns
   budgets from the caller's own accessible `budgets` array, so no client-side
   ownership gate is needed before the call; the backend's `perm != None` check
   is the actual authority. The existing owner-only `canSetAsDefault` gate +
   `commands.switchNotOwned` message are removed from this one call site.
   *Rationale: this is the literal UX regression the ticket exists to reverse.*
8. **`BudgetsView.svelte`'s list-page "switch" button is left untouched**,
   still gated by `canSetAsDefault`/`canSwitchTo` and still calling
   `setDefaultBudget` (owner-only, mutates `is_default`). The ticket's minimal
   scope explicitly names only `/budgets-switch` wiring; extending the list
   page's switch button to the new preference is a natural follow-up but adds
   a second call site + a UI-copy decision not covered by the ticket text.
   *Rationale: "avoid gold-plating" per the ticket; flagged as a follow-up
   below.*
9. **Creating a new budget does not clear an existing active-budget
   preference.** If a viewer has set their preference to a shared budget and
   then creates a new (now their-`is_default`) budget, the preference — being
   strictly higher-priority per resolve order — keeps them on the shared
   budget until they explicitly `/budgets-switch` again. This is a corner case
   the ticket doesn't address; documented rather than silently special-cased.
   *Rationale: keeps `create_budget`'s existing transaction untouched; matches
   "preference always wins" as the simplest, most predictable rule.*
10. **Audit trail:** the new endpoint logs a `SET_ACTIVE_BUDGET` audit row via
    the existing budget-scoped `log_audit` helper, mirroring `SET_DEFAULT`.
    *Rationale: every other budget-mutating endpoint in this file logs; keeps
    the audit trail complete for the new write path (even though it doesn't
    mutate the `budgets` table, it does mutate visible per-viewer state).*

## Goal & Success Criteria

Give a budget viewer a way to make ANY budget they can access (owned or
shared) their "active" view, without ever mutating another owner's
`is_default` row — reversing the UX narrowing #251 introduced as an
unavoidable side effect of closing its security hole.

- [ ] `users.active_budget_id` (nullable FK to `budgets`, `ON DELETE SET NULL`)
      exists via a new migration; every existing user row is unaffected
      (NULL default, no backfill).
- [ ] `POST /budgets/:id/activate` sets the caller's `active_budget_id` to
      `:id` when the caller has ANY access (`Permission::View`/`Edit`/`Owner`);
      403 with no access; never touches `budgets.is_default`.
- [ ] `GET /budgets` surfaces `is_active: true` on exactly the one row (if any)
      matching the caller's `active_budget_id`, `false` elsewhere; a stale
      preference (points at a budget the caller no longer has access to, or
      one filtered out by the archived view) yields no `is_active: true` row
      at all (graceful, no error).
- [ ] `frontend/src/App.svelte`'s `/budgets-switch` calls the new endpoint
      for both owned and shared targets and no longer blocks shared targets
      with `switchNotOwned`.
- [ ] `fetchBudgets()`'s `activeBudget` resolution checks `is_active` first,
      then falls through to the existing (untouched) fallback expression.
- [ ] Existing owner-only `/default` endpoint, `is_default` semantics,
      `BudgetsView.svelte`'s switch button, and all #238/#251/#249 guarantees
      are unchanged.

## Scope

**In scope:**
- Migration adding `users.active_budget_id`.
- `db::User` struct field (required for `SELECT *` decode).
- New `budget::set_active_budget` handler + `POST /budgets/:id/activate` route.
- `BudgetListItem.is_active` field, computed in `list_budgets`, defaulted
  `false` elsewhere.
- `frontend/src/App.svelte`: new `setActiveBudget()` helper (mirrors
  `setDefaultBudget`'s raw-fetch/timeout shape), `/budgets-switch` rewired to
  call it unconditionally for any resolved target, `fetchBudgets()`'s
  resolution order updated.
- Removal of the now-dead `canSetAsDefault`/`switchNotOwned` gate from the
  `/budgets-switch` call site only (not from `commands.js`'s export or
  `BudgetsView.svelte`).
- i18n: remove the now-unused `switchNotOwned` key from all 6 locale files
  (en/fr/de/it/pt/es) since its only call site is deleted.
- Backend + frontend tests for the new behavior.

**Out of scope (explicitly, per ticket + judgment):**
- `BudgetsView.svelte`'s list-page switch button (stays owner-only).
- Fixing #266's `fetchBudgets()` owner-scoping bug (separate ticket; this
  change is designed to compose cleanly with it landing before or after).
- Any UI affordance to explicitly clear/unset the preference (setting it to a
  different budget, including one's own default, is the only supported
  "reset" path for v1).
- Any migration/backfill of existing users' preferences.

## Architecture

- **DB**: `backend/migrations/20260703013000_user_active_budget.sql` —
  `ALTER TABLE users ADD COLUMN IF NOT EXISTS active_budget_id UUID REFERENCES
  budgets(id) ON DELETE SET NULL;`
- **`backend/src/db.rs`**: add `pub active_budget_id: Option<Uuid>` to `User`
  (required for the existing `SELECT * FROM users` call sites to keep
  decoding via `FromRow`).
- **`backend/src/budget.rs`**:
  - `BudgetListItem` gains `pub is_active: bool` (doc comment mirrors
    `owner_name`'s "populated only by list_budgets" precedent).
  - `list_budgets`: extend the existing `SELECT email FROM users WHERE id =
    $1` to also select `active_budget_id`; compute `is_active` per row in
    both the owned and shared construction loops by equality against the
    fetched `active_budget_id`.
  - New `pub async fn set_active_budget(State, Path(budget_id),
    Extension(user_id))`: `check_permission` must be `!= Permission::None`;
    `UPDATE users SET active_budget_id = $1 WHERE id = $2`; `log_audit(...,
    "SET_ACTIVE_BUDGET", ...)`; `Ok(StatusCode::OK)`.
  - All other `BudgetListItem` construction sites (`create_budget`,
    `get_budget`, `update_budget`, `close_budget` x2, `archive_budget` x2,
    `unarchive_budget` x2, `budget_item_for`) get `is_active: false,`.
- **`backend/src/main.rs`**: import `set_active_budget`; add
  `.route("/budgets/:id/activate", post(set_active_budget))` next to the
  existing `/default` route.
- **`frontend/src/App.svelte`**:
  - New `async function setActiveBudget(budgetId)` — same shape as
    `setDefaultBudget` (raw `fetchWithTimeout`, 401 → `handleLogout`, non-ok →
    throw), hitting `${API_BASE}/budgets/${budgetId}/activate`.
  - `fetchBudgets()`: `const preferred = budgets.find((b) => b.is_active);
    const defaultB = preferred || budgets.find((b) => b.is_default) ||
    budgets[0];` (the second half is the existing expression, untouched
    verbatim so a concurrent #266 land is a clean, independent diff).
  - `/budgets-switch` handler: drop the `if (!canSetAsDefault(target))` block
    (and its `switchNotOwned` message); call `setActiveBudget(target.id)`
    instead of `setDefaultBudget(target.id)`. The rest of the flow (the
    already-active check, the `fetchBudgets()` + confirm-then-message
    sequence, error handling) is unchanged.
- **`frontend/src/lib/i18n/locales/{en,fr,de,it,pt,es}.json`**: remove
  `commands.switchNotOwned`.
- **`frontend/src/lib/commands.js`**: update `canSetAsDefault`'s doc comment
  (it no longer gates `/budgets-switch`; it now only gates the budgets-list
  page's switch button) — comment-only change, behavior untouched.

## Error Handling & Edge Cases

- Non-existent `:id` on `/activate` → `check_permission` returns `None` (same
  "doesn't exist" == "no access" conflation as every other budget endpoint in
  this file) → 403. Matches existing precedent; not a regression.
- Preference points at a budget later deleted → FK `ON DELETE SET NULL`
  clears it automatically; next `list_budgets` call falls through cleanly.
- Preference points at a budget whose share was later revoked (not deleted) →
  `active_budget_id` stays set (no FK trigger), but the budget no longer
  appears in `owned_rows`/`shared_rows`, so no row gets `is_active: true` —
  `fetchBudgets()` falls through to the existing fallback. No error, no stale
  UI state beyond "silently stops being active," which is correct.
- Preference points at a budget currently filtered out by the `?archived=`
  view toggle → same graceful non-match behavior (consistent with archived
  budgets already being excluded from "active" concerns per AGENTS.md #50).
- Race: two concurrent `/activate` calls for the same user → last `UPDATE`
  wins; no partial-write risk (single-column, single-row `UPDATE`, no
  invariant to protect, unlike `is_default`'s partial unique index).

## Testing Approach

- **Backend** (`cd backend && cargo test`, `-- --ignored` for DB-backed):
  - `set_active_budget` unit/integration test: a View-permission sharee CAN
    set a shared budget active (contrast with the existing
    `set_default_budget_rejects_non_owner_share` test, which proves the
    opposite for `/default` — name the new test to make the contrast
    legible, e.g. `set_active_budget_allows_view_sharee`).
  - No-access user gets 403 and the row is untouched.
  - `list_budgets` surfaces `is_active: true` on exactly the preferred row
    (owned case and shared case), `false` on all others, when
    `active_budget_id` is set; all `false` when it is NULL.
  - A `active_budget_id` pointing at a share-revoked budget yields no
    `is_active: true` row (regression test for the edge case above).
- **Frontend** (`cd frontend && pnpm test`):
  - `resolveBudgetByName` is unaffected (no change) — no new tests needed
    there.
  - If `App.svelte`'s command logic remains untested at the unit level today
    (confirmed: no `App.svelte`-level test file exists in this repo), this
    change does not need to introduce one to stay consistent with existing
    coverage conventions — the backend permission test is the load-bearing
    regression guard analogous to #251's own test strategy. Manual smoke via
    `pnpm run build` + read-back is the existing bar for App.svelte changes.
  - Remove `switchNotOwned` references from any locale-completeness test if
    one iterates all keys are present/used (check for such a test at
    implementation time; if a "some / most-locale-keys used" test exists,
    make sure it doesn't newly flag the removal as breakage. If none exists,
    no action needed.).
- `cd backend && cargo build` / `cargo check` for compile correctness of the
  new field across all 12 construction sites.
- `cd frontend && pnpm run build` to confirm the Svelte change compiles.

## Risks & Open Questions

- **BudgetsView.svelte inconsistency (documented, not fixed):** after this
  ticket, the chat slash command and the budgets-list page will have
  different switch semantics (preference-based vs. owner-only /default) for
  a short window until/unless a follow-up unifies them. This is the ticket's
  own explicit minimal-scope choice, not an oversight.
- **#266 timing:** if #266 lands mid-flight (between this branch being cut
  and this PR merging), a rebase will be needed on the one shared line in
  `fetchBudgets()`. The chosen `preferred || <existing expression>` shape is
  designed to make that rebase a trivial, non-conflicting insertion in the
  common case, but a textual conflict on the exact line is still possible and
  will need manual resolution at rebase time (not now, since #266 has not
  landed as of this read).
