-- Fund categories (#228): a category can be marked as a fund (envelope /
-- sinking-fund) so its unused amount carries forward CUMULATIVELY and
-- BIDIRECTIONALLY across periods — a running balance across ALL periods since
-- the fund was created, not a single look-back like #47/#49's rollover, and
-- it can go NEGATIVE after sustained overspend (unlike rollover, which clamps
-- at zero). is_fund/rollover_enabled are independent flags; when both are on
-- for the same category, the read path uses the fund balance instead of the
-- #49 one-period carry (see budget::category_response_with_carry) to avoid
-- double-counting the same accumulated credit under two labels.
--
-- fund_balance is MATERIALIZED (not computed-on-read) — a deliberate
-- departure from the "computed on read, no scheduler" principle #47/#49/#51
-- follow (AGENTS.md). The effective amount depends on the base limit AS IT
-- WAS in each past period, and limits change over time; a computed-on-read
-- sum would retroactively rewrite history whenever a limit is edited. See
-- AGENTS.md "Fund Categories (#228)" for the full rationale.
--
-- fund_advanced_through is the idempotency marker (mirrors #51's
-- budgets.next_renewal_at, but inverted: it marks the boundary UP TO WHICH
-- fund_balance already reflects completed periods, not a future due time).
-- NULL means "never a fund" / "not currently tracking". Enabling is_fund sets
-- it to the CURRENT period's start (funds accrue going forward only).
ALTER TABLE categories ADD COLUMN is_fund BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE categories ADD COLUMN fund_balance DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE categories ADD COLUMN fund_advanced_through TIMESTAMPTZ;

-- Funds only make sense for expense categories (leftover = limit - spent);
-- income/savings have no comparable "spent against a limit" semantics in this
-- codebase. Mirrors budgets_auto_renew_only_time_based_check's pattern (#51).
ALTER TABLE categories ADD CONSTRAINT categories_is_fund_expense_only_check
    CHECK (NOT is_fund OR category_type = 'expense');

-- Mirrors budgets_auto_renew_due_idx (#51): a partial index scoped to the
-- subset the hourly job actually scans.
CREATE INDEX categories_fund_due_idx
    ON categories (fund_advanced_through)
    WHERE is_fund;
