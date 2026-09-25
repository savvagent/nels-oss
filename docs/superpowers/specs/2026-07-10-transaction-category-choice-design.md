# Design: Category choice & correction for logged transactions

**Date:** 2026-07-10
**Status:** Approved (brainstorming) — pending implementation plan

## Problem

When a user tells Nels to log a transaction without naming a category (e.g. "log my $35
haircut"), the LLM invents a `category_name` and the backend looks for an **exact,
case-insensitive name match** among the user's categories. On a miss it **silently creates a
new `expense` category** — no fuzzy/semantic matching, no confirmation.

Concrete failure: a user with an existing "Rob" category said "log the haircut." Nels created a
new "Haircut" category instead of using "Rob" or asking. Nels had no signal that grooming belongs
under "Rob," so it guessed and created clutter.

Two aggravating facts:
- Auto-created categories are **indistinguishable** from intentionally-created ones (no
  `source`/`auto_created` field), so they quietly accumulate.
- A transaction can be moved (`EDIT_TRANSACTION` chat action, `PUT` REST) and a category deleted,
  but **only through chat** — there is no transaction-management UI, and the reassign path *also*
  auto-creates a category on an exact-name miss.

## Goals

1. When Nels would create a **brand-new** category for a transaction the user didn't explicitly
   categorize, **ask first** — offer existing categories or confirm the new one.
2. Let the user **correct** a transaction's category after the fact (wrong existing category, or
   skipped the prompt) — via chat *and* a new Transactions UI.
3. **Auto-remove** an auto-created category once it's been emptied by a move, so corrections don't
   leave clutter.

## Non-goals (YAGNI)

- Fuzzy / semantic category matching ("Grocery" vs "Groceries" dedup). The up-front ask covers the
  miss; dedup is a separate concern.
- Bulk recategorization.
- Editing amount/description/date from the new Transactions screen (reassign category only for now).

## Design decisions (resolved)

- **When to intervene:** only at the moment Nels would create a *new* category for an
  un-categorized ADD_TRANSACTION. Everyday transactions that match an existing category are
  unaffected — no new friction.
- **"Both" clarified:** ask up front for the *new-category* case; the Transactions UI + chat move
  handle the *wrong-existing-category* case. We do **not** both ask up front and then offer to move
  the same just-logged transaction (no double-prompt).
- **Chip shortlist:** the user's **expense** categories, most-recently-used first, capped at 6,
  plus a "Create new '<Name>'" chip and a "Something else…" free-type escape.
- **Transactions screen:** a list with a per-row category dropdown (reassign only).
- **Cleanup:** delete an emptied category only if `auto_created = true` AND it now has zero
  transactions. User-created or non-empty categories are never touched.

## Architecture

Nels' chat agent (`backend/src/rag.rs`) is a single-shot JSON-action agent: the LLM returns
`{ action, action_params }` and the backend `match`es on `action`. New behavior is added within the
existing `ADD_TRANSACTION` handling plus a new pending-action payload, mirroring the existing
`PendingDeletion` round-trip.

### A. Schema — mark auto-created categories

New migration adds:

```sql
ALTER TABLE categories ADD COLUMN auto_created BOOLEAN NOT NULL DEFAULT false;
```

Set `true` only when a category is created as a side-effect of logging/editing a transaction
(ADD_TRANSACTION find-or-create, EDIT_TRANSACTION find-or-create, and the chip "create new" path).
Explicit `CREATE_CATEGORY` (chat) and REST `create_category` leave it `false`. Existing rows default
to `false` (treated as user-created — conservative; they will never be auto-removed).

### B. Ask up front — `ADD_TRANSACTION` change

In the `ADD_TRANSACTION` handler (`rag.rs` ~2988), after resolving the proposed `category_name`:

1. Exact case-insensitive match exists → log as today.
2. No match, but the budget has ≥1 existing **expense** category → **do not log.** Return a new
   `pending_category_choice` on the chat response and a `response` text asking the question.
3. No match and no existing expense categories → auto-create (`auto_created = true`) + log, as today
   (nothing to choose from).

Guardrails:
- This applies only when the user did **not** explicitly name a category. If the user's message
  explicitly named the (new) category, honor it (create + log), since that is intentional. The LLM
  distinguishes these; the action params carry an "explicit category" signal (see Open questions).
- The existing ambiguous-amount clarify flow still runs first; we only reach the category step once
  the amount is settled.

### C. `pending_category_choice` payload & round-trip

New struct on `ChatResponse` (mirrors `pending_deletion`, `#[serde(skip_serializing_if = ...)]`):

```rust
pub struct PendingCategoryChoice {
    pub budget_id: Uuid,
    pub amount: f64,
    pub description: String,
    pub currency: Option<String>,
    pub proposed_new_name: String,        // e.g. "Haircut"
    pub candidates: Vec<CategoryChoiceOption>, // existing expense categories, recent-first, ≤6
}
pub struct CategoryChoiceOption { pub id: Uuid, pub name: String }
```

Resolution endpoint (new): `POST /budgets/:id/transactions/finalize` accepting either
`{ amount, description, currency, category_id }` (existing chip) or
`{ amount, description, currency, new_category_name }` (create-new / typed). It **reuses the shared
ADD helper** so the transaction gets an **embedding** and, for `new_category_name`, the created
category is flagged `auto_created = true`. Returns the created transaction (and whether a category
was created) so the frontend can confirm.

> Rationale for a dedicated endpoint over reusing `create_transaction`: the chat ADD path generates
> the pgvector embedding; the resolution must too, so chip-created transactions remain
> semantically searchable/editable. Extract a shared `insert_transaction_with_embedding(...)` +
> `find_or_create_category(..., auto_created)` helper used by both chat ADD and this endpoint.

### D. Correct after the fact — Transactions UI

Frontend gains a **Transactions view** fed by the existing `GET /budgets/:id/transactions` (returns
JSON `TransactionResponse` rows with joined `category_name`). Each row shows description, amount,
date, current category, and a **category dropdown**. Selecting a different category calls the
existing `PUT /budgets/:id/transactions/:transaction_id { category_id }`, then refreshes.

- Opened from chat via a new `open_transactions_list` advisory field on `ChatResponse` (mirrors
  `open_categories_list` → `CategoriesView.svelte`), and via a nav entry.
- The dropdown needs a JSON list of the budget's categories (verify an existing JSON source; the
  `categories-table` endpoint is HTML — may need a small JSON categories endpoint or reuse whatever
  `fetchBudgets()` already loads).

### E. Auto-remove emptied auto-created categories

Shared helper `cleanup_orphaned_auto_category(old_category_id)`:
- Runs after any category reassignment: chat `EDIT_TRANSACTION` (`chat_edit_transaction`, rag.rs
  ~4916), REST `update_transaction` (used by the new UI), and transaction deletion
  (`chat_delete_transaction` / REST delete).
- Deletes the old category **iff** `auto_created = true` AND `COUNT(transactions) = 0`.
- Never touches user-created or non-empty categories.

### F. Frontend chip pattern (new)

No inline "tappable chips in a chat reply" pattern exists today (only the deletion modal and the
empty-state suggested-question chip). Add:
- `pendingCategoryChoice` state, populated from `chatRes.pending_category_choice` in `performSend`.
- Inline chips rendered under the AI message: one per candidate + "Create new '<Name>'" + "Something
  else…". Tapping a candidate/create chip → `POST .../transactions/finalize` → push confirmation +
  refresh. "Something else…" focuses the input for a typed name (routes back through `/chat`, or
  through `finalize` with `new_category_name`).

## Data flow (happy paths)

**Ask → pick existing:**
"log my $35 haircut" → LLM ADD_TRANSACTION, name "Haircut", no match, existing categories present →
`pending_category_choice` returned, not logged → user taps "Rob" → `POST .../finalize {amount:35,
description:"haircut", category_id: <Rob>}` → transaction inserted under Rob with embedding → confirm.

**Ask → create new:** same, user taps "Create new 'Haircut'" → `finalize { new_category_name:
"Haircut" }` → category created `auto_created=true`, transaction inserted → confirm.

**Correct later:** Transactions screen → change a row's dropdown from "Haircut" to "Rob" → `PUT` →
cleanup helper sees "Haircut" is `auto_created` & now empty → deletes it → refresh.

## Testing

Backend DB integration tests (the `#[ignore]` pgvector suite, podman pgvector port 6153):
- Pending-choice returned only on no-match-with-existing-expense-categories; not returned on exact
  match; silent auto-create+log when zero categories exist.
- `finalize` with `category_id` inserts under that category with an embedding; with
  `new_category_name` creates an `auto_created=true` category + transaction with embedding.
- Cleanup fires **only** for `auto_created` + empty categories; leaves user-created categories and
  non-empty categories intact, across EDIT_TRANSACTION, REST update, and delete paths.
- Explicit user-named new category still creates + logs without a prompt.

Frontend:
- Chips render from `pending_category_choice`; tapping sends the correct `finalize` payload.
- Transaction-row reassign hits `PUT` and refreshes; the emptied auto-category disappears.

## Open questions to resolve during planning

1. **Explicit-vs-inferred category signal:** confirm how the LLM/action params indicate the user
   explicitly named the category (so we don't prompt when they did). May need a prompt-rule tweak to
   emit an `explicit_category: bool` (or similar) in `action_params`.
2. Whether `create_transaction` REST already generates embeddings (informs whether `finalize` is a
   new endpoint or an extension).
3. Confirm the JSON source for the reassign dropdown's category list.

## Affected files (initial)

- `backend/migrations/<new>.sql` — `auto_created` column.
- `backend/src/rag.rs` — `ChatResponse` field, `PendingCategoryChoice`, ADD_TRANSACTION branch,
  shared find-or-create + insert-with-embedding helpers, cleanup helper wired into EDIT/DELETE,
  `open_transactions_list` advisory.
- `backend/src/budget.rs` — `finalize` endpoint (or extension), `update_transaction` cleanup call,
  possibly a JSON categories list endpoint.
- `backend/src/main.rs` — route registration.
- `frontend/src/App.svelte` — `pending_category_choice` handling, inline chips, transactions nav.
- `frontend/src/lib/TransactionsView.svelte` (new) — list + per-row category dropdown.
- i18n locale files — new strings.
