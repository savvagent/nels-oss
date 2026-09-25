-- Retirement asset linking — Plaid Investments (nels#468). The integration that
-- lets a US Pro subscriber link investment accounts so their balances and
-- holdings flow into the retirement balance sheet (#464), the projection engine
-- (#467), and the dashboard (#469).
--
-- See docs/superpowers/specs/2026-08-04-retirement-plaid-investments-design.md.
--
-- WHAT THIS MIGRATION DOES, AND WHY:
--
-- 1. Widens linked_accounts.budget_id from NOT NULL to NULLABLE + adds an
--    is_investments marker.
--
--    Investment accounts are BUDGET-LESS by design (§20 of AGENTS.md — assets
--    are user-scoped, and a budget-param route/link would let a budget
--    collaborator reach someone else's retirement accounts). But an
--    investment account still MUST live as a row in `linked_accounts`, because
--    `assets.linked_account_id` is a composite FK on (user_id, linked_account_id)
--    referencing linked_accounts (user_id, id) — the same-user invariant that
--    makes "an asset's linked account belongs to the same user as the asset" a
--    database-enforced fact. No separate table can satisfy that FK; the row the
--    FK resolves to has to exist in linked_accounts itself.
--
--    So budget_id becomes nullable. A NULL is how an investment account
--    expresses "has no budget". The FK on budget_id is already ON DELETE
--    CASCADE — a NULL never cascades, so that is unaffected. Every existing
--    row keeps its budget_id (cash connectors), and every cash-path query
--    filters on budget_id, so investment rows (NULL budget_id, is_investments
--    = TRUE) are simply invisible to the cash bank-linking surface. The
--    is_investments marker lets the investments module select exactly its own
--    rows and lets #469 list them without touching the cash provider rows.
--
-- 2. Adds investment_link_sessions, a NEW anti-replay pending-session table,
--    deliberately NOT a reuse/dup of plaid_link_sessions (which has a NOT NULL
--    budget_id). Same shape as belvo_link_sessions (#322) — a raw Plaid
--    public_token carries no user binding at exchange time, so the session row
--    is what binds the eventual exchange to a specific user_id. Completion is
--    claimed via the same atomic `UPDATE ... WHERE status='pending'` guard the
--    cash Plaid flow uses, closing the TOCTOU double-completion race.

-- (1a) budget_id becomes nullable for investment accounts.
ALTER TABLE linked_accounts ALTER COLUMN budget_id DROP NOT NULL;

-- (1b) is_investments flags budget-less investment-linked rows. DEFAULT FALSE
-- keeps every existing row (all cash connectors) as-is — backward compatible,
-- no backfill.
ALTER TABLE linked_accounts ADD COLUMN is_investments BOOLEAN NOT NULL DEFAULT FALSE;

-- Index the subset of investment rows so the poll job / list surface can
-- select them without scanning every cash account.
CREATE INDEX IF NOT EXISTS linked_accounts_is_investments_idx
    ON linked_accounts (is_investments) WHERE is_investments;

-- (2) Anti-replay pending session table, user-scoped and budget-less.
CREATE TABLE IF NOT EXISTS investment_link_sessions (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS investment_link_sessions_user_id_idx
    ON investment_link_sessions (user_id);