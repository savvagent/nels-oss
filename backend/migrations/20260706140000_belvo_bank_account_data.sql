-- Adds Belvo (Mexico, Brazil) as a third linked_accounts provider (nels#322),
-- against the linked_accounts.provider/provider_account_id generalization
-- #320 already built. See docs/superpowers/specs/2026-07-06-belvo-bank-account-data-design.md
-- Assumptions 3 and 13.

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'belvo'));

-- One row per pending Belvo widget-token mint (spec Assumption 3). Simpler
-- than GoCardless's bank_link_sessions: Belvo's widget handles institution
-- selection AND authentication entirely client-side, so there's no
-- requisition/agreement bookkeeping to persist — just enough to
-- anti-replay-verify who completed it (see belvo.rs::complete_link_session).
CREATE TABLE IF NOT EXISTS belvo_link_sessions (
    id          UUID PRIMARY KEY,
    budget_id   UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    country     TEXT NOT NULL CHECK (country IN ('MX', 'BR')),
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS belvo_link_sessions_budget_id_idx ON belvo_link_sessions (budget_id);
