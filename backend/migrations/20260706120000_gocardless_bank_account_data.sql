-- Generalizes #303's linked_accounts/transactions schema to support a second
-- provider (GoCardless Bank Account Data, nels#320) alongside Stripe, and adds
-- the pending-session table GoCardless's redirect-based consent flow needs
-- (Stripe never needs one — its client_secret round-trips via Stripe's own
-- servers). See docs/superpowers/specs/2026-07-06-gocardless-bank-account-data-design.md
-- Assumptions 4 and 9 for the full rationale.

ALTER TABLE linked_accounts ADD COLUMN provider TEXT NOT NULL DEFAULT 'stripe'
    CHECK (provider IN ('stripe', 'gocardless'));

ALTER TABLE linked_accounts RENAME COLUMN stripe_account_id TO provider_account_id;
ALTER INDEX linked_accounts_stripe_account_id_key RENAME TO linked_accounts_provider_account_id_key;

ALTER TABLE linked_accounts RENAME COLUMN stripe_customer_id TO provider_ref;

ALTER TABLE linked_accounts ADD COLUMN consent_expires_at TIMESTAMPTZ;
ALTER TABLE linked_accounts ADD COLUMN country TEXT;
ALTER TABLE linked_accounts ADD COLUMN institution_id TEXT;
ALTER TABLE linked_accounts ADD COLUMN iban TEXT;

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_status_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_status_check
    CHECK (status IN ('active', 'disconnected', 'consent_expired'));

ALTER TABLE transactions RENAME COLUMN stripe_transaction_id TO provider_transaction_id;
ALTER TABLE transactions ADD COLUMN currency TEXT;

-- The old index enforced global uniqueness of stripe_transaction_id alone —
-- correct for Stripe's globally-unique fctxn_... ids, NOT safe to assume for
-- GoCardless's bank-assigned transaction ids. Replace with a composite index
-- scoped per linked account.
DROP INDEX transactions_stripe_transaction_id_idx;
CREATE UNIQUE INDEX transactions_external_account_provider_tx_idx
    ON transactions (external_account_id, provider_transaction_id)
    WHERE provider_transaction_id IS NOT NULL;

-- One row per pending GoCardless consent attempt (nels#320 Assumption 4). The
-- `id` doubles as the `gc_ref` query param embedded in the redirect URL WE
-- construct (see gocardless.rs::start_link_session) — GoCardless redirects
-- verbatim to that URL, appending nothing of its own.
CREATE TABLE IF NOT EXISTS bank_link_sessions (
    id              UUID PRIMARY KEY,
    budget_id       UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    requisition_id  TEXT NOT NULL,
    agreement_id    TEXT,
    institution_id  TEXT NOT NULL,
    country         TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS bank_link_sessions_budget_id_idx ON bank_link_sessions (budget_id);
