-- #374: per-transaction exclude-from-budget flag + standing "ignore" rules.

-- (1) The primitive: mark a transaction as a non-spending internal transfer so
-- it never contributes to spending / income / savings / category-limit / goal
-- aggregates. Additive + backfilled to false; existing rows are unaffected.
ALTER TABLE transactions
    ADD COLUMN IF NOT EXISTS excluded_from_budget BOOLEAN NOT NULL DEFAULT false;

-- (2) Standing ignore rules: a lightweight substring matcher applied at bank
-- import time (sync_account_transactions) to auto-set excluded_from_budget on
-- future matching imports. match_text is matched case-insensitively as a
-- substring of the transaction description. external_account_id, when set,
-- scopes the rule to imports from that one linked account (NULL = any account
-- in the budget). Rules are budget-scoped and cascade-deleted with the budget;
-- an account-scoped rule is cleared to budget-wide if its account is deleted
-- (ON DELETE SET NULL), matching transactions.external_account_id's own policy.
CREATE TABLE IF NOT EXISTS transaction_ignore_rules (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    match_text TEXT NOT NULL,
    external_account_id UUID REFERENCES linked_accounts(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT transaction_ignore_rules_match_text_not_blank CHECK (btrim(match_text) <> '')
);

CREATE INDEX IF NOT EXISTS transaction_ignore_rules_budget_id_idx
    ON transaction_ignore_rules (budget_id);
