-- #300: replace the two global per-user "show X summary" toggles with a per-budget
-- budgeting strategy. New budgets column, backfilled from the owner's existing toggles,
-- then the superseded users columns (and their dedicated PATCH /user/preferences endpoint,
-- removed in application code alongside this migration) are dropped.

ALTER TABLE budgets
  ADD COLUMN budget_strategy TEXT NOT NULL DEFAULT 'limit_spent_remaining';

ALTER TABLE budgets
  ADD CONSTRAINT budgets_budget_strategy_check
  CHECK (budget_strategy IN ('zero_based', 'limit_spent_remaining'));

-- Backfill: a budget whose owner had the zero-based toggle on migrates to 'zero_based'
-- (preserves the richer of the two views when both were on, the pre-existing default);
-- every other budget keeps the column DEFAULT already applied by ADD COLUMN above.
UPDATE budgets b
SET budget_strategy = 'zero_based'
FROM users u
WHERE b.owner_id = u.id
  AND u.show_zero_based_summary = TRUE;

-- Retire the superseded global per-user toggles.
ALTER TABLE users
  DROP COLUMN show_zero_based_summary,
  DROP COLUMN show_limit_spent_remaining_summary;
