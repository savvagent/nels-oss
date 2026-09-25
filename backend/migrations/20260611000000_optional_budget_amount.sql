-- Make the budget amount optional and derive the budget total from categories.
--
-- Issue #46: budget and category amounts become optional. A budget's total is
-- computed as the SUM of its category amounts (a missing/null category amount
-- counts as 0), so the budget no longer needs a directly entered amount.
--
-- `categories.category_limit` is already nullable (see the init migration), so
-- only `budgets.budget_limit` requires a change: drop its NOT NULL constraint.
-- Existing rows keep their current value and are unaffected.
ALTER TABLE budgets ALTER COLUMN budget_limit DROP NOT NULL;
