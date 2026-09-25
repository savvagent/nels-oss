-- #431: Model budget currency instead of inferring it from transactions.
-- Adds a modeled currency column to budgets so display currency comes from
-- a stored value rather than a modal-frequency query over transactions.currency.
--
-- NOT NULL DEFAULT 'USD': every existing budget gets USD (backward-compatible
-- with today's hardcoded display behavior). The CHECK constraint enforces a
-- 3-letter uppercase ISO 4217 shape at the database level.

ALTER TABLE budgets
    ADD COLUMN currency TEXT NOT NULL DEFAULT 'USD';

ALTER TABLE budgets
    ADD CONSTRAINT budgets_currency_check CHECK (currency ~ '^[A-Z]{3}$');
