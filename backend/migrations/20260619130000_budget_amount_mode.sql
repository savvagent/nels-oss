ALTER TABLE budgets
  ADD COLUMN amount_mode VARCHAR(20) NOT NULL DEFAULT 'derived';
ALTER TABLE budgets
  ADD CONSTRAINT budgets_amount_mode_chk CHECK (amount_mode IN ('derived','fixed'));
