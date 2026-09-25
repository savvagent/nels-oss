# Spec: Rollup type/strategy compatibility enforced on edit, not just at link time (nels#317)

## 1. Brief (verbatim from the issue)

> **Background**: `validate_rollup_link` (extended by #300) rejects linking two budgets via
> `POST /budgets/:id/rollup` / chat `ROLLUP_BUDGET` unless they share the same `budget_type`
> AND `budget_strategy`. This check only runs at **link time**.
>
> Nothing re-validates the invariant when a budget that is **already** part of a rollup
> relationship later has its `budget_type` or `budget_strategy` changed via an ordinary edit:
> - `update_budget` (REST `PUT /budgets/:id`) validates `budget_type`/`budget_strategy` in
>   isolation but never checks `rollup_parent_id` (its own, or whether it has children) before
>   writing.
> - Chat `UPDATE_BUDGET` (`apply_budget_strategy_update`, and the pre-existing `budget_type`
>   update path) has the same gap.
> - The frontend's `BudgetDetails.svelte` "Type"/"Strategy" editors aren't disabled for a
>   budget that's a rollup parent or child (`isReadOnly()` only checks
>   `closed_at`/`archived_at`).
>
> **Repro**: link budget A (parent) and budget B (child) via rollup — both
> `time_based`/`limit_spent_remaining`, succeeds. Then `PUT /budgets/B` with
> `budget_type: "project"` (or, since #300, `budget_strategy: "zero_based"`) — succeeds
> silently, recreating the exact mismatched state `validate_rollup_link` was written to
> reject, with no error, no warning, and no audit note about the now-inconsistent pair.
>
> **Scope note**: this is pre-existing for `budget_type` — it has been possible since the
> rollup feature shipped (#52), independent of #300. #300 only extends the same,
> already-imperfect pattern to the new `budget_strategy` axis (consistent with existing
> behavior, not a regression). A proper fix should cover **both** axes symmetrically.
>
> **Suggested direction**: Before persisting a `budget_type`/`budget_strategy` change in
> `update_budget` and the chat update helpers, check whether the target budget has
> `rollup_parent_id IS NOT NULL` or is itself a parent
> (`EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $id)`) and reject with the same 409
> message `validate_rollup_link` already uses if so.
>
> Found during code review of #316 (#300's implementation) by the silent-failure-hunter agent.

Ref: https://github.com/savvagent/nels/issues/317

## 2. Assumptions

1. **Reject-on-edit, not auto-unlink** — the ticket itself flags this as an open product
   question. Reject-on-edit is chosen because it mirrors `validate_rollup_link`'s existing,
   already-shipped behavior at link time (a 409 telling the user why, not a silent structural
   change to their data), is the lower-risk default (auto-unlink would silently sever a rollup
   relationship the user may not have intended to break, with no way to recover the link
   without redoing it), and requires no new UX (a 409 with a clear message vs. designing and
   building an "are you sure you want to unlink?" confirmation flow, which is out of
   proportion for closing a validation gap). Auto-unlink is left as a documented open question
   for a future issue if product later prefers it (see Risks).
2. **Only reject an ACTUAL change, not any presence of the field.** The guard compares the
   requested value against the budget's CURRENTLY STORED value and only rejects when they
   differ. Resubmitting the budget's current, unchanged value is always a no-op, exactly
   mirroring two existing precedents in this codebase: `link_rollup`'s own idempotent
   re-link-to-the-same-parent (no error), and the frontend's `buildEditPatch` (already skips
   the PUT entirely client-side when `draftValue === budget?.field`, per
   `frontend/src/lib/budgetDetails.js`). This is more forgiving to any caller (tests, a future
   API integration, or a resubmission of a full form) than blocking on the field's mere
   presence in the payload, while still fully closing the reported gap — the exact repro in
   the issue (`budget_type: "project"` on a budget currently `"time_based"`) is a real change
   and is rejected.
3. **Reject applies to a rollup PARENT as well as a CHILD, unconditionally on any
   participation** (not "only if the new value would mismatch some specific counterpart").
   This matters for a parent with multiple children: because pre-#300 rollup links were never
   validated for `budget_type`, and pre-#317 links are not retroactively re-validated (per
   #300's own spec, Error Handling §), a parent's existing children are not guaranteed to
   already agree with each other or with the parent on these two fields. Given that, "does the
   new value match child X" is not a well-defined single check for a parent with
   heterogeneous, grandfathered children — so the guard blocks *any* actual change to a
   parent's own `budget_type`/`budget_strategy` while it has at least one child (archived or
   not), matching the ticket's own suggested direction
   (`EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $id)`, no archived filter — the
   same predicate `link_rollup`'s existing `child_is_parent` guard already uses). For a CHILD,
   the same blanket rule applies (rather than comparing against the one parent's current
   value) for symmetry and to keep one guard function/one rule for both roles, rather than two
   asymmetric behaviors that would need separately justifying and testing.
4. **Chat has no existing `budget_type` update path today — verified directly, not assumed
   from the ticket text.** The ticket's phrasing groups "`apply_budget_strategy_update`, and
   the pre-existing `budget_type` update path" under one "Chat `UPDATE_BUDGET`" bullet, which
   could be read as implying chat can already change an existing budget's `budget_type`. A
   full grep of `rag.rs` for `budget_type` (every occurrence, all ~40 hits) shows it is read
   in several places (`CREATE_BUDGET`, the rollup mismatch check, budget-lookup-by-name) but
   is **never written** inside the `"UPDATE_BUDGET" =>` dispatch arm (`rag.rs:3025-3212`) —
   only `CREATE_BUDGET` (`rag.rs:2027-2107`) sets it, at budget-creation time. AGENTS.md §13
   independently confirms this: "Chat `UPDATE_BUDGET` also accepts `budget_strategy`" is
   listed as new with #300, with no analogous claim for `budget_type`. So the chat-side gap
   this issue can actually close is scoped to `budget_strategy` only (via
   `apply_budget_strategy_update`) — there is no `budget_type` chat mutation to guard because
   none exists. Adding NEW chat support for changing an existing budget's type is a feature
   request, not a bug fix, and is explicitly out of scope (seeAScope).
5. **Status code and message convention**: `409 Conflict`, matching `validate_rollup_link`'s
   own mismatch responses (a conflict between an existing resource's attribute and an existing
   relationship, not a malformed request shape — that stays `400`). The message does not need
   to restate the OTHER budget's specific value (unlike `validate_rollup_link`'s link-time
   message, which names both sides because it's comparing two freshly-fetched rows in the same
   call) — at edit time we only need the acting budget's own current/requested values and its
   rollup role, which is simpler to fetch (no extra query for the counterpart) and still gives
   the user an actionable, specific message (which field, current vs. attempted value, and
   that unlinking first is the way to change it).
6. **New shared pure guard function + a new shared async DB helper**, so REST `update_budget`
   and chat's `apply_budget_strategy_update` cannot drift on wording or on the "is this budget
   a parent" query — `link_rollup`'s existing inline
   `SELECT EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $1)` is extracted into a
   reusable `is_rollup_parent` helper (used by `link_rollup` itself, `update_budget`, and
   `apply_budget_strategy_update`) rather than copy-pasted a third time.
7. **No DB migration.** This is a pure validation-logic fix — no new column, no schema change.
   `docs/superpowers/specs`/`plans` are still committed per this repo's established convention
   (every prior fix/feature in `docs/superpowers/{specs,plans}/` has one), even though there's
   no migration file to pair with it this time.
8. **No atomic single-query close of the check-then-write race.** Between the pre-write SELECT
   (current value + rollup role) and the actual `UPDATE`, a concurrent rollup link/unlink
   could change the budget's role. This mirrors an EXISTING, already-accepted pattern in this
   exact function: `update_budget` already calls `ensure_not_closed` (a SELECT) before its own
   later `UPDATE`, non-atomically, and that gap has never been treated as a bug in this
   codebase. The blast radius here is narrower still (budgets are single-owner; the only
   realistic race is the same user acting from two devices at once), so closing it with a
   single conditional `UPDATE ... WHERE ...` (mirroring `link_rollup`'s own TOCTOU-closing
   guarded UPDATE for the link/relink case) is not proportionate for this issue and is called
   out explicitly as an accepted, precedented tradeoff rather than a silent gap.
9. **Frontend disables ONLY the Type/Strategy editors for a rollup participant, not the whole
   page** — `isReadOnly()` (closed/archived) already makes the ENTIRE `BudgetDetails.svelte`
   page read-only; reusing it for rollup participation would incorrectly also block renaming,
   time-frame edits, etc., which remain fully valid on a rolled-up budget. A new, narrower
   pure helper `isRollupLinked(budget)` (parent OR child) gates only the two affected fields'
   edit affordances, mirroring the granularity the ticket itself asks for ("Type"/"Strategy"
   editors specifically).
10. **`rollup_child_ids` is confirmed present and populated on the exact response
    `BudgetDetails.svelte` reads.** `GET /budgets/:id` (`get_budget`, `budget.rs:1781-1861`)
    calls `.with_rollup(get_child_ids, ...)`, which overwrites the placeholder
    `rollup_child_ids: Vec::new()` with the real, DB-computed list
    (`rollup_child_ids(&state.db, budget_id)`) before the response is serialized — verified by
    reading `get_budget` and `with_rollup` (`budget.rs:177-187`) directly, not assumed.
    `BudgetDetails.svelte` already consumes this exact field today (`loadRollupNames`'s
    `loadedBudget?.rollup_child_ids ?? []`, and the existing, already-tested
    `rollupSummary(budget, ...)` helper's `hasChildren: childIds.length > 0`), so
    `isRollupLinked`'s reliance on `budget?.rollup_child_ids?.length` reuses a field shape this
    component already depends on in production, not a new/unverified assumption about the API
    contract.
11. **REST is still the enforcement boundary, not just the frontend.** The frontend disable is
    a UX nicety (per the ticket's own third bullet) — the backend guard in `update_budget` /
    `apply_budget_strategy_update` is what actually prevents the invariant violation for ANY
    client (a stale page, a direct API call, a future integration), consistent with this
    codebase's general pattern of never treating client-side disabling as the security/data
    boundary (e.g. `ensure_not_closed` similarly guards server-side even though the frontend
    also hides edit affordances for a closed budget).

## 3. Goal & Success Criteria

**Goal**: Extend the budget_type/budget_strategy rollup-compatibility invariant that
`validate_rollup_link` already enforces at LINK time to ALSO hold across ordinary edits to an
already-linked budget, on both the REST and chat update paths, with a matching frontend
affordance change.

Success criteria:
- `PUT /budgets/:id` rejects (409) an actual `budget_type` or `budget_strategy` change on a
  budget that is currently a rollup parent (has ≥1 child, archived or not) or a rollup child
  (`rollup_parent_id IS NOT NULL`); an update that omits both fields, or resubmits their
  current values, still succeeds unaffected.
- Chat `UPDATE_BUDGET`'s `budget_strategy` path (`apply_budget_strategy_update`) has the same
  rejection behavior, surfaced as a normal `mutation_error` chat message (not a crash, not a
  silent no-op).
- `BudgetDetails.svelte`'s Type and Strategy inline editors are disabled (no click-to-edit
  affordance) for a budget that is a rollup parent or child, with a tooltip explaining why;
  all other fields (name, time_frame, rollover, auto-renew) remain editable as before.
- The exact repro in the issue (link A/B, then `PUT /budgets/B` with a different
  `budget_type` or `budget_strategy`) now returns 409 and leaves B's stored value unchanged.
- No regression to any existing rollup, budget_type, or budget_strategy behavior (link/unlink,
  create, unrelated-field updates, non-rollup budgets of either type/strategy).

## 4. Scope

**In scope**
- A new pure guard, `budget::ensure_rollup_type_or_strategy_unchanged`, and a new shared async
  helper, `budget::is_rollup_parent` (extracted from `link_rollup`'s existing inline query).
- `update_budget` (REST): reject an actual `budget_type`/`budget_strategy` change on a rollup
  parent/child.
- `apply_budget_strategy_update` (chat `UPDATE_BUDGET`): same rejection for the
  `budget_strategy` axis (the only axis chat can currently change on an existing budget — see
  Assumption 4).
- `link_rollup`'s existing `child_is_parent` inline query refactored to call the new
  `is_rollup_parent` helper (no behavior change, just de-duplication).
- Frontend: `isRollupLinked(budget)` in `budgetDetails.js`; `BudgetDetails.svelte`'s Type and
  Strategy edit buttons (and `startEdit`'s guard) disabled when linked; a new i18n tooltip key
  across all 6 locales.
- Unit tests (default `cargo test`) for the new pure guard; DB-backed `--ignored` tests for
  `update_budget` and `apply_budget_strategy_update` covering parent/child × type/strategy ×
  reject/no-op-allowed; frontend vitest tests for `isRollupLinked`.

**Out of scope**
- Adding a NEW chat mutation path for changing an EXISTING budget's `budget_type` (none exists
  today — Assumption 4). That is a feature request, not a fix for this issue.
- Auto-unlink-on-mismatch as an alternative UX (Assumption 1) — left as a documented
  open question.
- Retroactively auditing/fixing pre-existing rollup pairs whose `budget_type`/`budget_strategy`
  already diverge (grandfathered before #300, or before this issue) — this issue only gates
  *future* edits, matching #300's own precedent of not revisiting pre-existing links.
- Any change to `validate_rollup_link` itself, or to link/unlink behavior.
- Closing the check-then-write race with an atomic single query (Assumption 8).

## 5. Architecture

### 5.1 Backend — `backend/src/budget.rs`

New pure guard, placed beside `validate_rollup_link`:

```rust
pub fn ensure_rollup_type_or_strategy_unchanged(
    is_child: bool,
    is_parent: bool,
    field_label: &str,
    current: &str,
    requested: &str,
) -> Result<(), (StatusCode, String)> {
    if requested == current || (!is_child && !is_parent) {
        return Ok(());
    }
    let role = if is_child { "child" } else { "parent" };
    Err((
        StatusCode::CONFLICT,
        format!(
            "Budgets can only be rolled up together when they share the same {field_label}. \
             This budget is a rollup {role}; changing its {field_label} from '{current}' to \
             '{requested}' would break that. Unlink it first if you need to change this."
        ),
    ))
}
```

New shared helper (extracted from `link_rollup`'s existing inline query, same SQL):

```rust
pub async fn is_rollup_parent(pool: &PgPool, budget_id: Uuid) -> Result<bool, (StatusCode, String)> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM budgets WHERE rollup_parent_id = $1)")
        .bind(budget_id)
        .fetch_one(pool)
        .await
        .map_err(internal_error)
}
```

`link_rollup` (`budget.rs:~2681-2688`) replaces its inline `child_is_parent` query with a call
to `is_rollup_parent(&state.db, child_id)` — pure refactor, no behavior change.

`update_budget` (`budget.rs:1863-2012`): after the existing `existing` fetch
(`budget.rs:1900-1905`), before building the `UPDATE`:

```rust
if payload.budget_type.is_some() || payload.budget_strategy.is_some() {
    let is_child = existing.rollup_parent_id.is_some();
    let is_parent = is_rollup_parent(&state.db, budget_id).await?;
    if let Some(bt) = &payload.budget_type {
        ensure_rollup_type_or_strategy_unchanged(is_child, is_parent, "budget type", &existing.budget_type, bt)?;
    }
    if let Some(bs) = &payload.budget_strategy {
        ensure_rollup_type_or_strategy_unchanged(is_child, is_parent, "budgeting strategy", &existing.budget_strategy, bs)?;
    }
}
```

Placed after `ensure_not_closed` (closed budgets are already fully read-only, so that check
keeps priority) and before any of the auto-renew/marker resolution logic — the guard is
validate-first, matching the file's existing convention (`validate_budget_type`/
`validate_budget_strategy`/`ensure_not_closed` all run before any row is touched).

### 5.2 Backend — `backend/src/rag.rs` (chat)

`apply_budget_strategy_update` (`rag.rs:5251-5281`) gains a pre-write check. It currently goes
straight from validating the string to `UPDATE ... RETURNING name`; it now first `SELECT`s the
current `budget_strategy` + `rollup_parent_id`, applies the same guard, and only then updates:

```rust
async fn apply_budget_strategy_update(
    db: &sqlx::PgPool,
    bid: Uuid,
    user_id: Uuid,
    strategy: &str,
) -> Result<String, String> {
    if let Err((_, msg)) = crate::budget::validate_budget_strategy(strategy) {
        return Err(format!("I couldn't update that budget: {}", msg));
    }

    let row = sqlx::query("SELECT budget_strategy, rollup_parent_id FROM budgets WHERE id = $1")
        .bind(bid)
        .fetch_optional(db)
        .await;
    let (current, is_child) = match row {
        Ok(Some(r)) => {
            let current: String = r.get("budget_strategy");
            let is_child = r.get::<Option<Uuid>, _>("rollup_parent_id").is_some();
            (current, is_child)
        }
        Ok(None) => {
            tracing::warn!(%bid, "AI budget_strategy update: budget row not found");
            return Err("I couldn't update that budget. Please try again.".to_string());
        }
        Err(e) => {
            tracing::error!(error = %e, %bid, "AI budget_strategy update: pre-check failed");
            return Err("I couldn't update that budget right now due to a temporary problem — please try again.".to_string());
        }
    };
    let is_parent = match crate::budget::is_rollup_parent(db, bid).await {
        Ok(v) => v,
        Err((_, msg)) => {
            tracing::error!(%bid, "AI budget_strategy update: is_rollup_parent failed: {}", msg);
            return Err("I couldn't update that budget right now due to a temporary problem — please try again.".to_string());
        }
    };
    if let Err((_, msg)) = crate::budget::ensure_rollup_type_or_strategy_unchanged(is_child, is_parent, "budgeting strategy", &current, strategy) {
        return Err(format!("I can't update that budget: {}", msg));
    }

    // ... existing UPDATE ... RETURNING name unchanged below ...
}
```

No change to the `"UPDATE_BUDGET" =>` dispatch arm itself — it already delegates to this
function and surfaces `Err` as `mutation_error` unchanged.

### 5.3 Frontend

`frontend/src/lib/budgetDetails.js` — new pure helper, next to `isReadOnly`:

```js
export function isRollupLinked(budget) {
  return !!(budget?.rollup_parent_id || (budget?.rollup_child_ids?.length ?? 0) > 0);
}
```

`frontend/src/lib/BudgetDetails.svelte`:
- `let rollupLocked = $derived(isRollupLinked(budget));`
- `startEdit(field, currentValue)`: add
  `if ((field === "budget_type" || field === "budget_strategy") && isRollupLinked(budget)) return;`
  alongside the existing `isReadOnly` guard (defense in depth, matching how the button's own
  `disabled` attribute is likewise not the only guard for `readOnly` today).
- The Type button (`~334-347`) and Strategy button (`~380-393`): `disabled={readOnly ||
  rollupLocked}`; `title` becomes
  `readOnly ? undefined : rollupLocked ? $_("budgetDetails.rollupLockedHint") : $_("budgetDetails.editHint")`;
  the trailing `{#if !readOnly}<Pencil .../>{/if}` becomes
  `{#if !readOnly && !rollupLocked}<Pencil .../>{/if}` for both fields.
- Name, Period/time_frame, Rollover, Auto-renew sections are UNCHANGED (still gated on
  `readOnly` alone) — only Type and Strategy are additionally locked.

i18n (`frontend/src/lib/i18n/locales/{en,de,es,fr,it,pt}.json`, `budgetDetails` object): new
key `rollupLockedHint`, English: `"Unlink this budget's rollup to change its type or strategy"`.

## 6. Error Handling & Edge Cases

- **Actual change on a rollup child/parent** → 409, row unchanged (REST and chat both).
- **Same value resubmitted on a rollup child/parent** → allowed (Assumption 2); this includes
  a REST caller that echoes the full current object back unchanged.
- **Non-rollup budget** → unaffected; guard is a no-op when `is_child` and `is_parent` are both
  false, identical to today's behavior.
- **A parent whose existing children already disagree with it or each other on
  type/strategy** (grandfathered pre-#300/pre-#317 state) → any further actual change to the
  PARENT's own type/strategy is still rejected while it has ≥1 child (Assumption 3); this does
  not retroactively fix or flag the grandfathered mismatch — it only stops it from getting
  worse via a future edit.
- **A budget that is simultaneously flagged `is_child` and `is_parent`** — structurally should
  not happen (`validate_rollup_link`'s own guards prevent a budget becoming both), but the new
  guard degrades safely either way: it treats `is_child` as taking message-wording priority
  (reports "child") while still rejecting the edit; the important behavior (reject) does not
  depend on which branch's wording is chosen.
- **Concurrent link/unlink racing the edit** — accepted, precedented non-atomicity (Assumption
  8); not treated as a new gap.
- **Closed budget that is also a rollup participant** — `ensure_not_closed` still runs first
  and rejects on closed-budget grounds (409, different message) before this guard is ever
  reached; ordering unchanged from today.
- **Frontend**: a rollup-linked budget's Type/Strategy show as plain (non-clickable) text with
  an explanatory tooltip; no error state is needed since the user is never able to submit an
  invalid change through the UI. The backend guard remains authoritative for any other client.

## 7. Testing Approach

- **Unit (default `cargo test`, no DB)**:
  - `ensure_rollup_type_or_strategy_unchanged`: non-participant + changed value → Ok; child +
    unchanged value → Ok; child + changed value → 409; parent + changed value → 409;
    (child ∧ parent) + changed value → 409 (degenerate case still rejects).
  - `isRollupLinked` (frontend, vitest, `budgetDetails.test.js`): no parent/no children →
    false; parent set → true; children present → true; children present but empty array →
    false.
- **DB-backed (`--ignored`, local Postgres via `podman-compose up -d`)**:
  - `update_budget` rejects a `budget_type` change on a rollup CHILD (409, row unchanged).
  - `update_budget` rejects a `budget_strategy` change on a rollup CHILD (409, row unchanged).
  - `update_budget` rejects a `budget_type` change on a rollup PARENT (409, row unchanged).
  - `update_budget` rejects a `budget_strategy` change on a rollup PARENT (409, row unchanged).
  - `update_budget` ALLOWS resubmitting the SAME `budget_type`/`budget_strategy` on a rollup
    child/parent (200, no-op).
  - `update_budget` ALLOWS an unrelated field change (e.g. rename) on a rollup child/parent
    when `budget_type`/`budget_strategy` are absent from the payload.
  - `apply_budget_strategy_update` rejects an actual strategy change on a rollup child (returns
    `Err`, row unchanged, no audit row written) and a rollup parent.
  - Regression: `link_rollup`'s existing type/strategy-mismatch and matching-link tests still
    pass unchanged after the `is_rollup_parent` extraction (pure refactor).
- **Frontend (vitest)**: `budgetDetails.test.js` extended for `isRollupLinked`.

## 8. Risks & Open Questions

- **Auto-unlink-on-mismatch remains a legitimate alternative UX**, explicitly flagged by the
  ticket itself. This spec chooses reject-on-edit (Assumption 1) as the lower-risk default;
  revisiting this as a product decision is a natural follow-up if reject-on-edit proves too
  restrictive in practice (e.g. users who want to change a budget's strategy AND keep it
  rolled up would currently need to unlink, change, then decide whether to re-link — re-linking
  is itself blocked until both sides match again).
- **Grandfathered mismatched rollup pairs are not surfaced or cleaned up** by this issue — a
  parent/child pair that already diverges (pre-#300, or created before this issue) stays
  silently mismatched until someone unlinks/re-links or a future dedicated cleanup issue
  addresses it directly (mirrors #300's own accepted risk).
- **The narrow, precedented TOCTOU window** (Assumption 8) is disclosed, not fixed.
