-- Issue #47: per-budget rollover toggle. When enabled, a budget's unused
-- (base - spent) amount from the previous period carries into the current
-- period (overspend clamps at 0; it does not carry). Default FALSE keeps every
-- existing budget behaving exactly as before — backward compatible.
ALTER TABLE budgets ADD COLUMN rollover_enabled BOOLEAN NOT NULL DEFAULT FALSE;
