-- Extends #320's provider/bank_link_sessions generalization to support two
-- more providers (Basiq/Australia, Akahu/New Zealand — nels#323). Unlike
-- GoCardless, both Basiq and Akahu use a HOSTED consent flow (Basiq Connect /
-- Akahu Connect): Nels never picks an institution on its own side, so
-- bank_link_sessions.institution_id/institution_name/country (NOT NULL today,
-- populated by GoCardless's own institution-picker flow) must become
-- nullable for these two providers. See spec Assumptions 2-4.

ALTER TABLE linked_accounts DROP CONSTRAINT linked_accounts_provider_check;
ALTER TABLE linked_accounts ADD CONSTRAINT linked_accounts_provider_check
    CHECK (provider IN ('stripe', 'gocardless', 'basiq', 'akahu'));

ALTER TABLE bank_link_sessions ADD COLUMN provider TEXT NOT NULL DEFAULT 'gocardless'
    CHECK (provider IN ('gocardless', 'basiq', 'akahu'));

ALTER TABLE bank_link_sessions ALTER COLUMN institution_id DROP NOT NULL;
ALTER TABLE bank_link_sessions ALTER COLUMN institution_name DROP NOT NULL;
ALTER TABLE bank_link_sessions ALTER COLUMN country DROP NOT NULL;

CREATE INDEX IF NOT EXISTS bank_link_sessions_provider_idx ON bank_link_sessions (provider);
