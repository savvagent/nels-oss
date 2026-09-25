# Data Privacy: Export & Account Deletion (#58)

Nels lets a user export all of their data and permanently delete their account,
to satisfy data-protection obligations (e.g. GDPR right of access / right to
erasure, CCPA). Both actions live in the sidebar account menu (Settings) and are
also recognized by the chat assistant.

## Endpoints

| Action | Method / Path | Auth | Confirmation |
|---|---|---|---|
| Export | `GET /api/account/export` | Bearer session | none (read-only, owner-scoped) |
| Delete | `DELETE /api/account` | Bearer session | typed email **and** a current TOTP code |

Both endpoints are owner-scoped to the authenticated user (`Extension<Uuid>`
inserted by `auth_middleware`). A user can only ever export or delete **their
own** data — there is no cross-user access.

## Export

`GET /api/account/export` returns a single JSON attachment
(`Content-Disposition: attachment; filename="nels-export-<date>.json"`)
containing:

- **account**: id, email, name, created_at (the TOTP secret is **never**
  included).
- **owned_budgets**: each budget the user owns, with its categories,
  transactions, goals, goal contributions, shares, and audit logs.
- **chat_messages**: the user's chat history (`message_text`). The vector
  `embedding` is a derived value and is **not** exported — the source text is.
- **conversations**, **notifications**, **reminders**: the user's direct rows.
- **shared_with_me**: read-only references (budget name, owner email, permission)
  to budgets owned by *other* users that are shared with this user. Only the
  reference is included — never the other owner's categories/transactions/etc.,
  so the export contains no other user's private data.

Excluded by design: passkey credentials, recovery-code hashes, session tokens,
in-progress auth flows, and chat embedding vectors.

Note: the exported `audit_logs` of a budget the user owns may contain the opaque
`user_id` (a UUID, never an email or name) of a *collaborator* who acted on that
shared budget. This is the minimum needed to keep the budget's own history
intact in the owner's export; no other identifying field of the collaborator is
included.

## Deletion scope & ordering

`DELETE /api/account` requires the caller to re-type their account email **and**
complete a fresh passkey assertion. The client first calls
`POST /api/account/delete-challenge` to get a single-use WebAuthn challenge, then
sends the signed assertion with the delete request. The email is matched
case-insensitively; the assertion is verified against the user's registered
passkeys. This makes deletion a deliberate, re-authenticated action and makes it
resistant to CSRF (a forged cross-site request cannot produce a valid passkey
assertion).

**Before the transaction begins**, if the user has an active Stripe subscription
(`subscriptions.stripe_subscription_id` set), the backend cancels it immediately
at Stripe (`billing::cancel_subscription`, `DELETE v1/subscriptions/{id}` — no
proration/refund of unused or prepaid time). If that call fails, deletion
**aborts** before the transaction opens: the account and all local data are left
untouched rather than deleting locally while an active paid subscription is
orphaned at Stripe. A user with no subscription (or one already canceled at
Stripe) is unaffected — this step is a no-op for them. The delete-account page
discloses this cancellation and the no-refund policy before the user confirms.

On success the deletion then runs as a **single database transaction** (all-or-nothing
— any failure rolls back and nothing is removed), in this order:

1. `sessions` — invalidate the caller's own bearer token first, before any data
   is touched.
2. `audit_logs WHERE user_id = <user>` — the user's audit entries on budgets
   owned by *others* (their own-budget audit logs are removed by the cascade in
   step 6).
3. `goal_contributions WHERE user_id = <user>` — the user's contributions to
   goals owned by *others* (their own cascade in step 6).
4. `budget_shares WHERE shared_with_email = <user email>` — grants giving this
   user access to *other* people's budgets.
5. `auth_flows WHERE user_id = <user>` — in-progress auth challenges (this table
   has no foreign key to `users`, so it does not cascade).
6. `DELETE FROM users` — cascades all of the user's **owned** data: budgets and
   their categories, transactions, goals, goal contributions, shares, and audit
   logs; plus chat messages **and their embeddings** (the embedding lives in the
   `chat_messages` row, so deleting the row deletes the vector and removes it
   from the pgvector/HNSW index), conversations, notifications, and reminders.

### Why explicit deletes (steps 2–5) in addition to the cascade

Four foreign keys do **not** remove the user's footprint on cascade alone:
`audit_logs.user_id` and `goal_contributions.user_id` are `ON DELETE SET NULL`
(a plain cascade would only *de-attribute* the row, leaving it behind);
`auth_flows.user_id` has no FK at all; and `budget_shares` references the user
only by email string. For a right-to-erasure these rows must be **removed**, not
nulled, so they are deleted explicitly and deliberately. A
deletion-completeness integration test
(`account_deletion_removes_all_user_data`) seeds a row in every user-owned table
— including a chat message with a non-null embedding and the user's footprint on
a second user's data — and asserts zero rows remain for the deleted user across
all tables (including `embedding IS NOT NULL`), while the second user's data
survives.

## Shared budgets

- **Deleting an owner deletes the budget entirely.** If a user owns a budget
  that they have shared with others, deleting their account removes the budget
  and all of its data, which revokes access for every collaborator. The
  account-deletion confirmation page warns the user of this before they
  confirm. (Transferring ownership to a co-owner before deletion is out of scope.)
- **Deleting a user who was shared *with* removes only their grant.** If the
  deleted user had access to someone else's budget (via a `budget_shares` row
  keyed on their email), that grant is removed; the other owner's budget is
  untouched.
- The deleted user's contributions and audit entries on *other* owners' budgets
  are removed as part of erasure (steps 2–3). This is a deliberate consequence
  of right-to-erasure: a financial contribution the deleted user made to another
  person's shared goal is removed along with the rest of their footprint.

## Audit logging

Export and deletion each emit a structured `tracing::info!` log line (with the
`user_id` and record counts for export; `user_id` for deletion). They do **not**
write to the `audit_logs` table because:

- `audit_logs.budget_id` is `NOT NULL`, so a global, non-budget-scoped event has
  no natural row there; and
- a deletion cannot write its own audit row into a table that the same
  transaction is deleting.

The `audit_logs` table is itself retention-bounded (purged on a schedule, #45),
so it is not a durable compliance ledger regardless. Platform logs (Fly.io)
retain the structured export/deletion events per the platform's operational
retention.

A separate, durable, queryable "account X deleted at T" ledger (outside the
deleted user's own data, retained beyond the `audit_logs` window for
dispute/compliance evidence) is intentionally **out of scope** for this change.
The structured platform log is the current record; a dedicated retained deletion
ledger is a possible future follow-up.

## Backups, retention & legal hold

- **Immediate removal.** All of the user's live rows — and their chat embeddings
  — are removed from the primary PostgreSQL database immediately, in the single
  transaction described above.
- **Backups / point-in-time recovery.** The backend runs on Fly.io against a Fly
  Managed Postgres cluster, which keeps automated backups / point-in-time-restore
  snapshots. A just-deleted user's data may still exist inside those snapshots
  until they age out of the provider's retention window (on the order of days),
  after which they expire automatically. Nels does not (and a tenant generally
  cannot) surgically edit provider-managed snapshots; reliance is on automatic
  expiry. A restore from a pre-deletion snapshot would re-introduce the data —
  an operational event that would require re-running the deletion.
- **Operational logs.** The structured export/deletion log lines persist in
  platform logging per its retention policy.
- **Legal hold.** There is no automated legal-hold mechanism today, and none is
  currently active. If a legal hold were required for a specific account, the
  deletion for that account would need to be deferred via a manual operational
  process until the hold is lifted; this document is the placeholder for that
  procedure.

## Chat surface

The chat assistant recognizes `EXPORT_DATA` and `DELETE_ACCOUNT` intents (online
via the LLM action schema, offline via `offline_privacy_action`). Neither chat
action performs the operation: `EXPORT_DATA` routes the user to the Settings
"Export my data" affordance, and `DELETE_ACCOUNT` routes them to the Settings
typed-email + TOTP confirmation flow. The irreversible deletion is intentionally
only reachable through that confirmed Settings flow, never from a chat
parse. A short "DATA & PRIVACY" line in the chat context makes the assistant
aware the features exist.
