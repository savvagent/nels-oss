-- Stripe Financial Connections bank-account linking (#303). One row per linked
-- Financial Connections Account, scoped to the budget it was linked into.
-- Disconnecting NEVER deletes this row (or any transactions it produced) — it
-- only flips `status`, preserving history per the ticket's AC.
CREATE TABLE IF NOT EXISTS linked_accounts (
    id                  UUID PRIMARY KEY,
    budget_id           UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    stripe_account_id   TEXT NOT NULL UNIQUE,
    stripe_customer_id  TEXT NOT NULL,
    institution_name    TEXT,
    display_name        TEXT,
    last4               TEXT,
    category            TEXT,
    subcategory         TEXT,
    status              TEXT NOT NULL DEFAULT 'active'
                             CHECK (status IN ('active', 'disconnected')),
    last_synced_at      TIMESTAMPTZ,
    disconnected_at     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS linked_accounts_budget_id_idx ON linked_accounts (budget_id);

-- `disconnected_at` must be set if and only if `status = 'disconnected'` —
-- every writer that changes one always changes the other together
-- (`disconnect_linked_account`'s UPDATE, the webhook's disconnected-event
-- UPDATE, and `complete_link_session`'s re-link ON CONFLICT which resets both
-- to 'active'/NULL), so this is a real invariant, not just convention. Added
-- as a separate ALTER TABLE (not inlined into the CREATE TABLE above) so it
-- is applied even in an environment where `linked_accounts` was already
-- created by an earlier run of this same not-yet-deployed migration.
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_status_disconnected_at_check
    CHECK ((status = 'disconnected') = (disconnected_at IS NOT NULL));

-- Imported transactions attribute to the linked account that produced them.
-- NULL (the default, and every existing row) means "manually entered" — no
-- backfill needed. ON DELETE SET NULL: if a linked_accounts row were ever
-- deleted directly (not the normal disconnect path, which only flips status),
-- the transaction history it produced must survive as ordinary rows, not
-- cascade-delete.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS external_account_id UUID
    REFERENCES linked_accounts(id) ON DELETE SET NULL;

-- Stripe's `fctxn_...` id. The idempotency key: a re-sync (webhook redelivery,
-- manual refresh landing after auto-refresh already ran, etc.) must skip rows
-- already imported rather than duplicate them or clobber a user's edits to an
-- already-imported row. Partial (not a plain UNIQUE) so the index only covers
-- imported rows — a plain UNIQUE on a nullable column already permits multiple
-- NULLs with no collision, so the WHERE clause isn't needed for correctness;
-- it's used here to keep the index smaller by excluding the (majority)
-- manually-entered rows that will never carry a stripe_transaction_id.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS stripe_transaction_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS transactions_stripe_transaction_id_idx
    ON transactions (stripe_transaction_id) WHERE stripe_transaction_id IS NOT NULL;

-- `external_account_id` and `stripe_transaction_id` must always be both-NULL
-- (a manually-entered transaction) or both-set (an imported one) — never
-- mixed. `sync_account_transactions`'s INSERT always sets both together;
-- every manual-transaction insert path (`budget::create_transaction`,
-- `rag.rs`'s ADD_TRANSACTION arm) sets neither, which satisfies `NULL = NULL`
-- (true) under this CHECK.
ALTER TABLE transactions ADD CONSTRAINT transactions_external_account_pairing_check
    CHECK ((external_account_id IS NULL) = (stripe_transaction_id IS NULL));
