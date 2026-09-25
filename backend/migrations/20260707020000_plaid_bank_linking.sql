-- Adds Plaid (nels#321) as a sixth bank-linking provider alongside Stripe
-- (#303), GoCardless (#320), Belvo (#322), and Basiq/Akahu (#323), against
-- the existing generalized linked_accounts/transactions schema and
-- bank_provider::Provider dispatch — no further generalization, just a
-- sixth value. See
-- docs/superpowers/specs/2026-07-06-plaid-canada-bank-linking-design.md
-- Assumptions 4-6.
--
-- Also FIXES a pre-existing bug discovered while writing this migration:
-- 20260707000000_basiq_akahu_bank_linking.sql's DROP+ADD CONSTRAINT
-- silently dropped 'belvo' from the allowed provider list (it chained off
-- 20260706120000's original 2-value list instead of 20260706140000's
-- 3-value ('stripe','gocardless','belvo') list, since both migrations were
-- authored concurrently against an unmerged base and neither saw the
-- other). Any attempt to link/update a 'belvo' linked_accounts row on
-- current `main` fails this CHECK constraint. Restoring 'belvo' here,
-- alongside adding 'plaid', fixes that regression as a side effect of
-- this migration touching the same constraint.
ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'belvo', 'basiq', 'akahu', 'plaid'));

-- Plaid's item_id is the correlation key Plaid's webhook payloads carry (a
-- webhook fires per-Item, not per-account, and one Item can back multiple
-- linked_accounts rows) — distinct from provider_account_id, which is the
-- per-account Plaid account_id. Always NULL for every other provider's rows.
ALTER TABLE linked_accounts ADD COLUMN plaid_item_id TEXT;
CREATE INDEX linked_accounts_plaid_item_id_idx ON linked_accounts (plaid_item_id)
    WHERE plaid_item_id IS NOT NULL;

-- This row's own /transactions/sync cursor (spec Assumption 7: per-row, not
-- per-Item, to keep sync_account_transactions shaped like every other
-- provider's per-linked-account sync function). Always NULL for every other
-- provider's rows and for a Plaid row that has never synced yet.
ALTER TABLE linked_accounts ADD COLUMN plaid_cursor TEXT;

-- Anti-replay pending-session table (spec Assumption 4/5) — a Plaid
-- public_token carries no recoverable client_user_id/budget binding at
-- exchange time, unlike Stripe's client_secret (verified against the
-- caller's own Stripe customer) or GoCardless's bank_link_sessions row
-- (verified against the caller's own budget_id/user_id). One row per
-- POST .../plaid/link-token call; POST .../plaid/complete looks this row up
-- by session_id, 403s on a budget_id/user_id mismatch, 409s if already
-- completed (checked via an atomic completion-claim UPDATE, not a plain
-- read-then-write, to close a TOCTOU race between two concurrent completion
-- attempts for the same session_id).
CREATE TABLE plaid_link_sessions (
    id          UUID PRIMARY KEY,
    budget_id   UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX plaid_link_sessions_budget_id_idx ON plaid_link_sessions (budget_id);
