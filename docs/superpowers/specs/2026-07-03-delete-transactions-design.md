# Spec: Users should be able to delete transactions (nels#258)

## Brief (verbatim from ticket)

Problem: "If a user adds a transaction and then realizes it was a mistake, the
user should be able to delete it." Today there is no way to remove a
transaction once entered, in chat or otherwise.

Acceptance criteria:
- DELETE /budgets/:id/transactions/:transaction_id exists, requires Edit/Owner
  permission, rejects on a closed budget, scopes the lookup by budget_id, and
  writes an audit log entry.
- A DELETE_TRANSACTION chat action exists, resolves the target via the
  existing embedding-based locator, and returns a PendingDeletion for
  confirmation rather than deleting immediately.
- The confirmation modal in App.svelte supports pendingDeletion.kind ===
  "transaction", showing an identifiable description (amount +
  description/date) and calling the new DELETE endpoint on confirm.
- After a confirmed delete, category spend, remaining, and rollover/fund
  carry-forward all reflect the deletion immediately since they're computed
  live from transactions.
- Deleting a transaction in a closed (read-only) budget is rejected, matching
  how edits are rejected.
- Ambiguous/no-match locators in DELETE_TRANSACTION are declined with a
  message asking the user to be more specific.

Design decisions (locked, from ticket):
- Confirm-then-delete, not immediate delete. Matches DELETE_CATEGORY/DELETE_BUDGET.
- Reuse EDIT_TRANSACTION's locator/embedding-match logic rather than building
  a second transaction-resolution path. Apply the same
  EDIT_TRANSACTION_MAX_DISTANCE ambiguity guard.
- Hard delete, no deleted_at. Spend/rollover figures are computed live from transactions.
- Permission model matches category/budget deletion (Owner/Edit, not Viewer)
  and must respect ensure_not_closed.

Out of scope: standalone transaction list/detail UI, bulk delete, soft
delete/undo/trash, editing (already implemented).

## Current-source verification (re-checked 2026-07-03 against HEAD e26b9b4)

All ticket line-number references were re-verified against current `main`
inside the isolated worktree. Findings:

- `backend/src/main.rs:250-251` — exactly as described: `POST/GET
  /budgets/:id/transactions` (create_transaction/list_transactions) and `PUT
  /budgets/:id/transactions/:transaction_id` (update_transaction), no DELETE.
- `backend/src/budget.rs`: `delete_category` at line 2916 (not 2885 — has
  drifted slightly), `create_transaction` at 2959, `update_transaction` at
  3043. All shapes match the ticket's description.
- `backend/src/rag.rs`: `PendingDeletion` struct at line 126 (`kind: String,
  id, name, budget_id: Option<Uuid>`, doc comment says `kind: "category" |
  "budget"`). Action enum list literal at line 1357. `DELETE_CATEGORY` arm at
  2085, `DELETE_BUDGET` arm at 2430. `required_perm_for_action` at 545.
  `TRANSACTION_LOOKUP_BY_EMBEDDING` at 479, `EDIT_TRANSACTION_MAX_DISTANCE` at
  508. `chat_edit_transaction` helper at 3996 (called from the `EDIT_TRANSACTION`
  match arm at 3439). `ADD_TRANSACTION_RULE`/`EDIT_TRANSACTION_RULE` are
  separate `const` strings (692/698) injected into the system-prompt
  `format!()` via two trailing `{}` placeholders (1437-1438); `EDIT_TRANSACTION_RULE`'s
  own text is numbered "21." inside the constant itself.
- `frontend/src/App.svelte`: `pendingDeletion` state at line 85 (comment
  documents shape `{kind: "category"|"budget", id, name, budget_id}`),
  `confirmDeletion()` at 1292 (endpoint ternary at 1297-1299, binary:
  category vs budget), confirmation modal body ternary at ~1556 (binary:
  budget vs category). `cancelDeletion()` is generic (uses `pd.name`, no
  kind-specific branching) — no change needed there.
- `frontend/src/lib/i18n/locales/{en,es,de,fr,it,pt}.json` all define
  `confirmDelete.categoryBody` / `confirmDelete.budgetBody` /
  `confirmDelete.conversationBody` (not mentioned in the ticket, but a
  `transactionBody` key must be added here too, in all 6 locales, to keep
  localization parity — the app is fully localized per AGENTS.md item on
  App Localization).
- No fund/rollup-specific carry-forward code has landed from #228 in this
  checkout (`git log` shows no #228 merge on this `main`); the AC's "fund
  carry-forward" clause is therefore not yet applicable. Standard rollover
  carry-forward (`budget::carried_amount`, computed live via SUM over
  `transactions`) is the baseline verified — no separate code needed here
  since it already reads live from the `transactions` table with no cached
  counters to unwind.
- `ensure_not_closed` is called inside `delete_category`'s REST handler, but
  NOT inside the `DELETE_CATEGORY` *chat* arm (which only resolves the
  pending-deletion target) — the closed-budget guard is deliberately deferred
  to the REST call that runs on confirm. This is the pattern to mirror for
  DELETE_TRANSACTION: resolve in chat (no closed check), enforce
  `ensure_not_closed` in the REST handler.
- There is no offline (no-`GEMINI_API_KEY`) pattern-router fallback for
  `DELETE_CATEGORY`, `DELETE_BUDGET`, or `EDIT_TRANSACTION` — all three
  require the LLM path. DELETE_TRANSACTION follows the same precedent: no
  offline fallback is added (would be a scope addition beyond the ticket and
  beyond precedent).

## Assumptions

1. **Extract a shared locator-resolution helper.** Rather than duplicating
   the ~30-line "embed locator → `TRANSACTION_LOOKUP_BY_EMBEDDING` →
   distance-guard via `EDIT_TRANSACTION_MAX_DISTANCE`" block, extract a small
   private async helper (e.g. `resolve_transaction_by_locator(state, user_id,
   bid, locator) -> Result<ResolvedTransaction, String>` returning the
   matched row's `id`/`description`/`amount`/`transaction_date`, or an
   `Err(user_facing_message)` on "no key"/"no match"/"too far") used by BOTH
   `chat_edit_transaction` and the new `chat_delete_transaction`.
   `TRANSACTION_LOOKUP_BY_EMBEDDING`'s `SELECT` list gains one additional
   column, `t.transaction_date` (currently selects only `id, category_id,
   description, amount, distance` — DELETE_TRANSACTION's display name needs
   the date too; see Assumption 4). This is purely additive to the constant
   (one more selected column, same `FROM`/`WHERE`/`ORDER BY`/`LIMIT`), so
   `chat_edit_transaction` — which ignores the new column — is unaffected.
   Rationale: the ticket explicitly says "reuse this same locator/lookup
   rather than inventing new matching logic"; extraction is the literal
   reading of "reuse" and avoids duplicating a security-relevant guard (the
   distance cutoff) in two places where they could drift. Risk: touches the
   already-shipped, tested `chat_edit_transaction`. Mitigation:
   `chat_edit_transaction`'s existing `--ignored` DB tests
   (`chat_edit_transaction_guards_without_mutating`,
   `transaction_lookup_and_update_sql_round_trip`) are re-run unchanged after
   the refactor to catch any regression; the refactor is behavior-preserving
   by construction (same query shape plus one additive column, same constant,
   same early-return shape).
2. **`chat_delete_transaction` is a new private helper**, mirroring
   `chat_edit_transaction`'s signature style, returning
   `(Option<PendingDeletion>, Option<String>)` (log/error tuple shape used
   throughout the file), called from a new `"DELETE_TRANSACTION"` match arm.
   It does NOT call `ensure_not_closed` (matching the `DELETE_CATEGORY`
   precedent — the REST handler enforces it on confirm) and does NOT delete
   anything.
3. **`PendingDeletion.kind` becomes `"category" | "budget" | "transaction"`**
   (doc comment on the struct updated); the REST-endpoint-resolution ternary
   in `budget_id` stays `Some(bid)` for a transaction (transactions are
   always budget-scoped, same as categories).
4. **Display name for a transaction pending-deletion**: composed server-side
   as `"{description} — ${amount:.2} ({date})"` (e.g. `"coffee — $5.00
   (2026-07-01)"`), placed in `PendingDeletion.name`. The `date` comes from
   `transaction_date` (`%Y-%m-%d`), added to `TRANSACTION_LOOKUP_BY_EMBEDDING`'s
   `SELECT` list per Assumption 1 — the existing query does not select it
   today, so this is a required, explicit change to that shared constant, not
   an incidental one. This directly satisfies the AC ("showing an
   identifiable description: amount + description/date") without requiring
   the frontend to know about transaction-specific fields beyond the existing
   generic `name` string.
5. **Route wiring**: extend the existing `.route("/budgets/:id/transactions/:transaction_id",
   put(update_transaction))` with `.delete(delete_transaction)` — one route,
   two methods, matching the ticket's instruction and axum's method-chaining
   convention already used for `/budgets/:id` (`get(...).put(...).delete(...)`)
   and `/budgets/:id/categories/:category_id` (`put(...).delete(...)`).
6. **Locale translations**: `transactionBody` is added to all 6 locale files
   (en/es/de/fr/it/pt) as reasonable professional translations following the
   existing `categoryBody`/`budgetBody` phrasing/tone in each file. English:
   `"Permanently delete the transaction “{name}”? This can’t be undone."` —
   mirrors `categoryBody`'s cadence. Translators were not available;
   translations are best-effort but structurally consistent with sibling
   keys (same placeholder `{name}`, same closing "can't be undone" clause
   pattern already used for `budgetBody`/`conversationBody`).
7. **DELETE_TRANSACTION prompt rule**: added as a new `const
   DELETE_TRANSACTION_RULE` (mirroring `ADD_TRANSACTION_RULE`/
   `EDIT_TRANSACTION_RULE`'s pattern — a `const &str` with the rule's own
   number embedded, e.g. `"21a. DELETE_TRANSACTION: ..."`), injected via one
   more trailing `{}` placeholder in the system-prompt `format!()` right
   after `EDIT_TRANSACTION_RULE`'s. It reuses `transaction_match` (documented
   in the `action_params` schema doc-comment, extended to mention
   DELETE_TRANSACTION alongside EDIT_TRANSACTION) and directs the model to
   state in `response_text` what will be deleted and that confirmation is
   required (mirroring rule 2c's DELETE_CATEGORY/DELETE_BUDGET instruction).
8. **`required_perm_for_action("DELETE_TRANSACTION")` → `Some(Permission::Edit)`**,
   added to the `Edit-or-Owner` doc-comment list and match arm alongside
   `EDIT_TRANSACTION`/`ADD_TRANSACTION`/`DELETE_CATEGORY`.
9. **Frontend `confirmDeletion()` endpoint mapping**: the current binary
   ternary (`category` vs implicit-`else`-budget) is restructured into an
   explicit 3-way mapping (`if/else if/else` or equivalent) so a
   `"transaction"` kind builds
   `` `/budgets/${pd.budget_id}/transactions/${pd.id}` ``. The modal body
   ternary is similarly extended to a 3-way branch adding
   `confirmDelete.transactionBody`.
10. **Deploy model (Repo Profile)**: this repo has NO pull_request-triggered
    CI — `.github/workflows/*.yml` only run on `push: [main]`
    (release-please) or `workflow_call`/`workflow_dispatch` (the
    release-gated deploy jobs). "Checks passing" on the PR is therefore
    vacuously true (no required checks configured); the verification burden
    (`cargo test`, `cargo check`, `pnpm build`) is on the implementer/PR
    author before merge, not on CI. Deploy happens only when release-please's
    auto-maintained per-package release PR is later merged — a shared,
    cross-ticket, typically-human-gated action that bundles whatever has
    landed on `main` since the last release. This PR will NOT merge that
    release PR (it would bundle unrelated concurrent work from #249/#228/
    #261/#266); Phase 5 verification for this ticket is local (dev stack:
    already-running `pgvector` container on `127.0.0.1:6153`, `cargo test`,
    `cargo run` + manual REST/chat exercise), not a production smoke test.
11. **DB-backed tests**: a `pgvector/pgvector:pg16` Postgres container
    (`budget-rag-db`) is already running locally on port 6153, matching the
    `DATABASE_URL` default baked into the existing `#[ignore]`d integration
    tests. These `--ignored` tests will be run as part of verification
    (`cargo test -- --ignored`) in addition to the default `cargo test`.

## Goal & Success Criteria

Goal: let a user delete a mistaken transaction, in chat, with the same
confirm-then-delete UX and permission/closed-budget guards already used for
category and budget deletion — with no new UI surface beyond the existing
confirmation modal.

Success criteria:
1. `DELETE /budgets/:id/transactions/:transaction_id` returns 204 on success,
   403 for View-only callers, 404 for a transaction that doesn't belong to
   the named budget, 409 on a closed budget, and writes a `DELETE_TRANSACTION`
   audit row.
2. A chat message like "delete my $5 coffee transaction" produces a
   `pending_deletion` with `kind: "transaction"` in the `ChatResponse`
   instead of deleting anything.
3. An ambiguous/no-match locator produces `mutation_error` (no
   `pending_deletion`), asking the user to be more specific — no silent
   fallback to the nearest unrelated transaction.
4. The frontend confirmation modal renders for `kind === "transaction"` with
   an identifiable amount+description, and confirming calls the new DELETE
   endpoint then refreshes budgets.
5. After a confirmed delete, category spend/remaining and rollover
   carry-forward (verified against `budget::carried_amount`, computed live
   from `SUM` over `transactions`) reflect the deletion on the very next
   read, with no code change needed to "unwind" a cached balance (none
   exists).
6. `cargo test` (default) and `cargo test -- --ignored` (DB-backed) both pass;
   `cargo clippy`/`cargo check` clean; `pnpm run build && pnpm test` (frontend,
   if a test script exists) clean.

## Scope

In scope:
- `backend/src/budget.rs`: `delete_transaction` REST handler.
- `backend/src/main.rs`: route wiring (`.delete(delete_transaction)`).
- `backend/src/rag.rs`: `DELETE_TRANSACTION` action enum entry, prompt spec
  rule, `required_perm_for_action` entry, `PendingDeletion.kind` doc update,
  extracted locator-resolution helper (used by both edit and delete),
  `chat_delete_transaction` helper, new match arm, unit + `--ignored`
  integration tests.
- `frontend/src/App.svelte`: `confirmDeletion()` endpoint mapping,
  confirmation modal body, doc-comment on `pendingDeletion`'s shape.
- `frontend/src/lib/i18n/locales/{en,es,de,fr,it,pt}.json`: new
  `confirmDelete.transactionBody` key.

Out of scope (per ticket): standalone transaction list/detail UI, bulk
delete, soft delete/undo/trash, editing (already shipped), fund-specific
carry-forward (not yet merged in this checkout via #228), offline
(non-LLM) router fallback for DELETE_TRANSACTION (no precedent for
DELETE_CATEGORY/DELETE_BUDGET/EDIT_TRANSACTION either).

## Architecture

REST path (mirrors `delete_category`):
```
DELETE /budgets/:id/transactions/:transaction_id
  -> check_permission (Owner/Edit only, 403 otherwise)
  -> ensure_not_closed (409 on closed budget)
  -> SELECT description, amount FROM transactions WHERE id = $1 AND budget_id = $2
     (404 if missing — cross-budget/unknown-id guard)
  -> DELETE FROM transactions WHERE id = $1 AND budget_id = $2
  -> log_audit(..., "DELETE_TRANSACTION", "Deleted transaction: {description} - ${amount}")
  -> 204 No Content
```

Chat path (mirrors `DELETE_CATEGORY`'s resolve-only shape, reusing
`EDIT_TRANSACTION`'s locator/embedding machinery):
```
"DELETE_TRANSACTION" arm
  -> chat_delete_transaction(state, user_id, active_budget_id, params)
       -> requires active_budget_id (else mutation_error)
       -> requires non-empty params.transaction_match (else mutation_error)
       -> resolve_transaction_by_locator(...) [shared with chat_edit_transaction]
            -> embed locator (GEMINI_API_KEY; None key => mutation_error)
            -> TRANSACTION_LOOKUP_BY_EMBEDDING (budget-scoped nearest row;
               SELECT list extended with t.transaction_date — see Assumption 1)
            -> distance > EDIT_TRANSACTION_MAX_DISTANCE => mutation_error
       -> Ok(row) => pending_deletion = Some(PendingDeletion {
              kind: "transaction", id: row.id,
              name: format!("{} — ${:.2} ({})", description, amount, date),
              budget_id: Some(bid),
          })
       -> Err(msg) => mutation_error = Some(msg)
  -> NO delete performed here, NO ensure_not_closed check here (mirrors
     DELETE_CATEGORY; the REST call on confirm enforces it)
```

Frontend: `pendingDeletion.kind === "transaction"` extends the existing
generic modal (no new component) — endpoint mapping and modal body text
gain a third branch; `confirmDeletion()`'s success/failure/cancelled
messaging is already kind-agnostic (`pd.name`), so no change needed there.

## Error Handling & Edge Cases

- Cross-budget transaction id (belongs to a different budget than the URL) →
  REST 404 (scoped `WHERE id = $1 AND budget_id = $2`, same as
  `update_transaction`/`delete_category`).
- Closed budget → REST 409 (via `ensure_not_closed`); chat resolution still
  succeeds (produces a `PendingDeletion`) but the confirm-time REST call
  fails with 409 — mirrors how `EDIT_TRANSACTION`'s chat mutation itself
  checks `ensure_not_closed` but `DELETE_CATEGORY`'s *resolve* step does not;
  since DELETE_TRANSACTION defers to REST like DELETE_CATEGORY, the same
  gap-then-catch-at-confirm shape is intentional and consistent.
- No active budget → chat `mutation_error`, "You don't have an active budget
  to delete a transaction from" (mirrors `chat_edit_transaction`'s "You don't
  have an active budget to edit a transaction in.").
- Empty/missing `transaction_match` locator → `mutation_error` asking which
  transaction.
- No `GEMINI_API_KEY` → can't embed the locator → `mutation_error`
  ("I couldn't look up that transaction right now."), same degrade-gracefully
  message family as `chat_edit_transaction`.
- Distance beyond `EDIT_TRANSACTION_MAX_DISTANCE` (no confident match) →
  `mutation_error`, "I couldn't find a transaction matching that description
  — can you be more specific about which one to delete?" — directly satisfies
  the AC "ambiguous/no-match locators are declined... asking the user to be
  more specific."
- View-only permission → the outer `required_perm_for_action`/
  `is_chat_action_authorized` gate already refuses before the match arm runs
  (falls to `"NONE"`), consistent with every other Edit-gated action.
- Frontend DELETE call failure (network/permission/409 at confirm time) →
  existing generic `catch` in `confirmDeletion()` already renders
  `confirmDelete.failed` — no transaction-specific handling needed.

## Testing Approach

Backend (`cd backend && cargo test`, `cargo test -- --ignored` for
DB-backed):
- Unit: `required_perm_for_action("DELETE_TRANSACTION")` returns
  `Some(Permission::Edit)` (extend the existing
  `edit_actions_allow_owner_and_edit_but_not_view` table-driven test, or add
  a dedicated test mirroring `edit_transaction_requires_edit_permission`).
- Unit: `resolve_transaction_by_locator`'s pure/guard-shaped pieces (distance
  cutoff, empty-locator) covered by adapting/extending
  `chat_edit_transaction_guards_without_mutating`'s pattern for the new
  `chat_delete_transaction`, `#[ignore]`d (needs Postgres/pgvector).
- `--ignored` integration test: seed a budget + transaction, call
  `chat_delete_transaction` with no `GEMINI_API_KEY` locally (asserts the
  graceful-degrade path) and assert the transaction row is untouched for
  every guard path (no active budget, empty locator, no key) — mirroring
  `chat_edit_transaction_guards_without_mutating`.
- `--ignored` integration test (or extend an existing `budget.rs` handler
  test file/module) for `delete_transaction`: 204 + row gone + audit row
  written on success; 403 for a View collaborator; 404 for a
  wrong-budget/nonexistent id; 409 on a closed budget (transaction NOT
  deleted).
- Re-run `chat_edit_transaction`'s existing tests unchanged after extracting
  the shared locator helper, to confirm no regression.

Frontend: no existing component-test harness was found for `App.svelte`
modal branches (verify at implementation time; if one exists, extend it —
otherwise this is consistent with the file's current test coverage and no
new test infra is introduced, matching precedent for `category`/`budget`
kinds which also have no dedicated Svelte test). Manual verification via the
running dev stack (`pnpm run dev` + backend) is the fallback, exercised in
Phase 5.

## Risks & Open Questions

- The locator-resolution extraction (Assumption 1) touches
  `chat_edit_transaction`, a shipped path. Mitigated by re-running its
  existing tests unchanged and keeping the extraction behavior-preserving
  (same query/constant/early-return shape) — flagged here so a reviewer can
  double-check the diff is a pure refactor for that function.
- Locale translations (Assumption 6) are best-effort, not reviewed by native
  speakers — a reasonable risk for a short, formulaic confirmation string
  that closely mirrors existing sibling keys already in the file.
- The exact wording chosen for `PendingDeletion.name`'s date format
  (`YYYY-MM-DD` via `%Y-%m-%d`, matching `format_transactions_list_message`'s
  existing convention elsewhere in `rag.rs`) is a judgment call with no
  ticket-mandated format; picked for consistency with the one existing
  transaction-date-rendering precedent in the file.
- Spec critique (round 1) caught that `TRANSACTION_LOOKUP_BY_EMBEDDING` does
  not currently select `transaction_date`, which `PendingDeletion.name`
  needs — resolved by adding that column to the shared query (Assumption 1),
  additive and non-behavior-changing for `chat_edit_transaction`.
