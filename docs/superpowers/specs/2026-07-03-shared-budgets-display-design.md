# Spec: Display budgets shared with user (chat LIST_BUDGETS) — savvagent/nels#231

## Brief (verbatim from ticket)

> Summary: User asked Nels in chat to "Show budgets that have been shared with me." Nels
> replied it has no way to display budgets shared with the user by others. This is a
> follow-up to enable that: display, on request, the budgets other users have shared with
> the current user, including who shared it and what access level was granted.
>
> Root cause: Sharing already works end-to-end on the backend — this is a display gap only.
> `budget_shares` already records budget_id / shared_with_email / permission_level
> (view/edit), and `check_permission` already authorizes reads/writes on a shared budget
> correctly. `GET /budgets` (`list_budgets`) already merges the caller's owned budgets with
> budgets shared with their email, tagging each with `is_owner` and `permission_level`. But
> the AI chat path never sees shared budgets: the context query that feeds Nels' prompt
> (`budget_rows`, rag.rs) is owner-scoped only. No amount of prompt wording can make the LLM
> enumerate budgets it was never given.
>
> Acceptance criteria (verbatim):
> - Asking Nels "show budgets that have been shared with me" (or similar phrasing) when at
>   least one budget has been shared with the user's email returns a list naming each shared
>   budget, who shared it (owner name), and the permission level (view/edit).
> - If nothing has been shared with the user, Nels says so plainly (no shared budgets),
>   rather than the current flat refusal.
> - Asking "list my budgets" continues to list owned budgets, and now also surfaces shared
>   ones in a clearly separate section — it does not conflate the two.
> - Archived shared budgets are annotated "(archived)" consistent with how owned archived
>   budgets are handled today (rule 11's existing carve-out).
> - A shared budget that happens to be is_default = TRUE for its owner is not mislabeled
>   "(active)" from the viewing user's perspective — "(active)" continues to reflect only the
>   viewing user's own is_default budget.
> - Existing LIST_BUDGETS behavior for owned-only users (no shares involved) is unchanged.
> - Unit/integration test coverage (following the existing DB-integration-test pattern in
>   rag.rs, e.g. near `chat_share_budget_is_owner_only`) for: a user with only shared
>   budgets, a user with both owned and shared, and a user with none shared.
>
> Out of scope: no new frontend UI; not fixing SWITCH_BUDGET's owner-scoped name resolution;
> not addressing the separately-discovered `POST /budgets/:id/default` owner_id-guard bug
> (flagged for a standalone ticket).

Fresh line numbers as of this spec (`backend/src/rag.rs`, current `main`):
- Context query: line 659 (`budget_rows`)
- `budgets_context` formatting: lines 665–683
- Prompt rule 11: line 1284
- `_ => {}` dispatch catch-all: line 3168–3169
- Reference test pattern: `chat_share_budget_is_owner_only`, line 5619 (DB-integration, `#[ignore]`, real Postgres via `podman-compose up -d` / `DATABASE_URL`)
- Reference merge pattern (owned+shared): `list_budgets`, `backend/src/budget.rs:1379+`

## Assumptions

1. **No offline-router route for LIST_BUDGETS exists today** (confirmed by grep — the mock
   NLP fallback never sets `action = "LIST_BUDGETS"`; that action is LLM-only, reached when
   `GEMINI_API_KEY` is set). Consequently the acceptance criteria's "asking Nels ... returns
   a list" behavior is inherently LLM-driven (Gemini reads the prompt's USER'S BUDGETS
   context + rule 11 instructions and free-generates the enumeration) — it is not
   deterministically testable end-to-end without a live/mocked Gemini call. Per the ticket's
   own guidance ("Unit/integration test coverage... following the existing DB-integration-test
   pattern"), the testable surface is the **context-building logic** (the SQL query + the
   string formatting that becomes the "USER'S BUDGETS" prompt block), not the LLM's free-text
   response. *Rationale:* this mirrors how `budgets_context` is already the sole deterministic
   input the LLM depends on — get that right and correct enumeration follows from rule 11's
   instructions, which is a documentation/prompt change, not something unit-testable in Rust.
2. **Extract the context-building code into two small, directly-testable functions** —
   `fetch_budgets_context_rows(pool, user_id, user_email) -> Result<Vec<BudgetContextRow>, sqlx::Error>`
   and `format_budgets_context(rows: &[BudgetContextRow]) -> String` — rather than testing it
   in place inside `chat_endpoint`. *Rationale:* keeps `chat_endpoint` itself unchanged in
   shape (same two-line call site), makes the new logic unit/integration-testable in
   isolation without going through the whole chat turn, and follows the existing pattern of
   small free functions near `chat_endpoint` (e.g. `offline_privacy_action`,
   `offline_archive_action`) already tested directly.
3. **Owner display name fallback**: `users.name` is nullable (the user may not have told Nels
   their name yet, per rule in `name_context`). When the sharing budget's owner has no name
   on file, label the entry `(shared by someone, <level> access)` rather than surfacing the
   owner's email (email wasn't requested in the ticket's proposed columns, and leaking another
   user's email into a third party's LLM prompt context is an unnecessary exposure the ticket
   doesn't ask for). *Rationale:* graceful degradation, no new PII surfaced, matches the
   ticket's exact proposed `SELECT`, which fetches only `u.name`.
4. **Permission-level fallback**: `budget_shares.permission_level` is `NOT NULL` in the
   schema, so no fallback needed in practice — but as defensive coding, default to `"view"`
   if a `None` is ever read, since `check_permission` already treats an unrecognized level as
   the least-privileged interpretation. *Rationale:* fail-closed / least-privilege display,
   consistent with the rest of the sharing feature's fail-closed posture.
5. **Union type-safety**: the two `SELECT` branches of the `UNION ALL` return `NULL` literals
   for `permission_level`/`owner_name` in the owned branch; these are explicitly cast
   (`NULL::VARCHAR(50)`, `NULL::TEXT`) to avoid Postgres "could not determine data type"
   errors. *Rationale:* required for the query to compile against the real schema
   (`budget_shares.permission_level VARCHAR(50)`, `users.name TEXT`).
6. **`is_default` labeling only applies to owned rows.** The shared-budget query still SELECTs
   `b.is_default` (needed to keep column shape parity across the `UNION ALL`), but the
   formatter only reads it (to emit `(active)`) when the row is the caller's own
   (`is_owner == true`) — never for a shared row, even if the *owner's* `is_default` happens
   to be true for that budget. This directly satisfies the AC's "not mislabeled (active)"
   bullet.
7. **Rule 11 prompt rewrite is additive prose, not a new instruction number** — the existing
   numbered list (rules 1–22) is extended in place at rule 11 rather than adding a new rule
   number, since this is explicitly the *same* LIST_BUDGETS action per the ticket's proposed
   solution (#1: "extend the existing LIST_BUDGETS action rather than adding a new one").
8. **No REST/API changes.** `GET /budgets` (`list_budgets` in `budget.rs`) is untouched — it
   already does the right thing; this ticket only teaches the *chat* context-builder the same
   trick. No new migration (the `budget_shares` table and `users.name` column already exist).

## Goal & Success Criteria

Teach the chat context-builder (`rag.rs`) about budgets shared with the current user, so
Nels can answer both "what's been shared with me" and "list my budgets" (now including
shared ones) using only the deterministic context it's given — closing the exact gap the
ticket identifies, without adding a new action, endpoint, or migration.

- [ ] `budgets_context` (the "USER'S BUDGETS" prompt block) includes budgets shared with the
      caller's email, each labeled with the owner's name and permission level.
- [ ] Owned-row labeling (`(active)`, `(archived)`) is byte-for-byte unchanged for a user with
      no shares (regression safety).
- [ ] Shared-row labeling never emits `(active)` (AC: is_default is owner-scoped, not
      viewer-scoped) and does emit `(archived)` when the owner has archived it.
- [ ] Prompt rule 11 gives the LLM explicit, unambiguous instructions to (a) enumerate only
      `(shared by ...)`-tagged entries when asked specifically what's shared with them, saying
      plainly there are none if none exist, and (b) show two separated sections ("Yours: ...",
      "Shared with you: ...") when asked to list budgets generally.
- [ ] DB-integration tests (mirroring `chat_share_budget_is_owner_only`'s harness style)
      directly exercise the new query/formatter for: shared-only, owned+shared, and
      none-shared users.

## Scope

**In scope:**
- `backend/src/rag.rs`: extend the context query, extract+format into two testable
  functions, update rule 11's prompt text.
- New DB-integration tests near the existing sharing tests in `rag.rs`.

**Out of scope** (mirrors the ticket exactly):
- No frontend UI.
- No fix to `SWITCH_BUDGET`'s owner-scoped name resolution.
- No fix to the `POST /budgets/:id/default` missing owner_id guard (flag as a follow-up
  ticket at PR time if not already tracked).
- No new chat action, no REST/API change, no migration.

## Architecture

1. **`BudgetContextRow` struct** (new, private to `rag.rs`): `id: Uuid`, `name: String`,
   `is_default: bool`, `archived_at: Option<DateTime<Utc>>`, `is_owner: bool`,
   `permission_level: Option<String>`, `owner_name: Option<String>`.

2. **`fetch_budgets_context_rows`** (new `async fn`, DB call): runs a single `UNION ALL` query
   (shared branch joins `budget_shares` → `budgets` → `users`; owned branch unchanged from
   today's `budget_rows` query, with explicit `NULL::...` casts for parity), `ORDER BY name
   ASC`, returns `Vec<BudgetContextRow>`. Note: this is **net-new SQL surface** for this
   codebase — a `grep -n "UNION" backend/src/*.rs` on `main` finds zero existing uses of
   `UNION ALL` anywhere. It is *behaviorally* equivalent to (not a mirror of) `list_budgets`'s
   approach in `budget.rs`, which instead runs two separate queries and merges them in Rust.
   The single-query approach is preferred here because the output is one formatted string
   (no need for the richer per-field `BudgetListItem` merge `list_budgets` builds), but the
   plan must treat the `UNION ALL` + explicit casts as new, carefully-tested SQL rather than
   an established idiom to copy from elsewhere in the repo.

3. **`format_budgets_context`** (new pure `fn`): mirrors today's formatting exactly for owned
   rows (`(active)` iff `is_owner && is_default`; `(archived)` iff `archived_at.is_some()`),
   and for shared rows appends `(shared by {owner_name_or_fallback}, {permission_level}
   access)` before the archived tag. Empty input still renders `"USER'S BUDGETS: (none yet)"`
   (unchanged).

4. **`chat_endpoint`** call site (rag.rs ~line 659): replace the inline query + formatting
   block with `let rows = fetch_budgets_context_rows(&state.db, user_id, &user_email).await
   .map_err(internal_error)?; let budgets_context = format_budgets_context(&rows);` — keeping
   `user_email` (already resolved earlier in the function) as the bind param for the shared
   branch and `user_id` for the owned branch.

5. **Rule 11 prompt text** (rag.rs ~line 1284): rewritten to explain the two annotation
   families now present in USER'S BUDGETS (`(shared by X, Y access)` alongside the existing
   `(active)`/`(archived)`), and give the LLM the "shared with me" vs. "list my budgets"
   branching instruction plus the "say none, don't refuse" instruction for the empty-shared
   case. The existing archived-only carve-out sentence stays, extended to note it covers both
   owned and shared archived entries (since both are now `(archived)`-tagged in the same
   list).

6. **No dispatch/mutation change** — `_ => {}` at the bottom of the action-match still covers
   `LIST_BUDGETS` (verified: read-only, no new branch needed).

## Error Handling & Edge Cases

- DB error fetching rows → `internal_error` (matches every other query in `chat_endpoint`;
  no behavior change from today's `budget_rows` error path).
- Owner has no `name` set → falls back to `"someone"` in the shared label (Assumption 3).
- `permission_level` unexpectedded `None` (shouldn't happen; column is `NOT NULL`) → falls
  back to `"view"` (Assumption 4, fail-closed/least-privilege).
- Zero shared AND zero owned budgets → unchanged `"USER'S BUDGETS: (none yet)"`.
- Zero shared, some owned → identical output to today (regression-tested).
- Zero owned (shared-only user, e.g. a newly-invited collaborator with no budgets of their
  own) → the owned branch of the `UNION ALL` naturally returns no rows; only shared rows
  appear; no `(active)` anywhere (correct — this user has no default budget).
- A budget shared with the same email by two different owners (not possible per schema — one
  row per `(budget_id, shared_with_email)`, and a budget has exactly one `owner_id`) — no
  special handling needed.

## Testing Approach

DB-integration tests (`#[ignore]`, real Postgres via `podman-compose up -d` /
`DATABASE_URL`, following `chat_share_budget_is_owner_only`'s exact harness: unique UUIDs per
test run, explicit `INSERT`/cleanup, no shared fixtures), added near that test in
`backend/src/rag.rs`'s `#[cfg(test)] mod tests`:

1. `budgets_context_shared_only` — a user with zero owned budgets and one budget shared with
   them (view access) → the formatted context contains the shared budget name, the owner's
   name, and "view access"; contains no `(active)` tag.
2. `budgets_context_owned_and_shared` — a user who owns one budget (their default) and has one
   budget shared with them (edit access, and that shared budget's *owner* also has it as their
   own default) → formatted context shows the user's own budget tagged `(active)`, the shared
   budget tagged `(shared by ..., edit access)` and specifically NOT tagged `(active)` (proves
   Assumption 6 / the AC's mislabeling bullet).
3. `budgets_context_none_shared` — a user with one or more owned budgets and zero shares →
   output byte-identical in shape to pre-change behavior (regression).
4. `budgets_context_shared_archived` — a budget shared with the user that the owner has
   archived → formatted context shows both `(shared by ...)` and `(archived)` on the same
   entry.
5. `budgets_context_empty` — a user with neither owned nor shared budgets → `"USER'S BUDGETS:
   (none yet)"` (regression; may be a plain `#[test]` against `format_budgets_context(&[])`
   with no DB needed).

Run: `cd backend && cargo test rag:: -- --ignored` for the new DB tests (per repo docs, DB
must be up via `podman-compose up -d`); `cargo test` for the full non-ignored suite
(regression) and any new plain unit tests; `cargo check`/`cargo build` for compile
correctness; `cargo fmt --all` / no dedicated lint step beyond `cargo check` observed in this
repo (no `clippy` in the documented commands).

## Risks & Open Questions

- **LLM enumeration itself is not automatically testable** (Assumption 1) — the prompt-text
  quality is verified by inspection + the deterministic context tests, not by an
  integration test against a live Gemini call. This is a known gap already present for every
  other prompt-driven action in this file (e.g. rule 11's pre-existing archived carve-out has
  no LLM-output test either) — consistent with existing repo practice, not a regression.
- **Owner-name fallback wording** ("someone") is a judgment call (Assumption 3); low risk,
  easily changed by the developer if a different fallback is preferred (e.g. surfacing the
  owner's email instead) — flagged for review.
- **Out-of-scope bug callout**: the ticket explicitly flags a separate `POST
  /budgets/:id/default` owner_id-guard bug as *not* to be bundled here; this spec does not fix
  it and does not open a follow-up ticket automatically (can be done at PR time if desired by
  the human reviewer, but doing so autonomously would exceed this ticket's scope per the
  ticket's own "flagged for a standalone bug ticket, not bundled here" instruction).
