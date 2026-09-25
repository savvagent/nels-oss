-- Issue #51: recurring budgets that auto-renew on a schedule. `auto_renew`
-- opts a budget into automatic renewal on its existing `time_frame` cadence
-- (monthly/quarterly/yearly — the recurrence reuses the timeframe concept rather
-- than adding a separate interval). `next_renewal_at` is the persisted idempotency
-- marker: the hourly background ticker renews only budgets whose marker has
-- elapsed, then advances it to the next period boundary, so re-running the tick
-- within a period never double-renews. DEFAULT FALSE / NULL keeps every existing
-- budget non-recurring with unchanged behavior — backward compatible, no backfill.
ALTER TABLE budgets ADD COLUMN auto_renew BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE budgets ADD COLUMN next_renewal_at TIMESTAMPTZ;

-- Auto-renew is a time-based concept: project budgets are one-off pools that do
-- not recur (#48), and closed/archived budgets are inactive. Enforce the
-- type invariant at the data layer so an invalid state is unrepresentable even
-- if an out-of-band write bypasses the app-level guard. Existing rows
-- (auto_renew = FALSE) satisfy the constraint trivially.
ALTER TABLE budgets ADD CONSTRAINT budgets_auto_renew_only_time_based_check
    CHECK (NOT auto_renew OR budget_type = 'time_based');

-- The hourly renewal job (`budget::renew_due_budgets`) scans for due budgets with
-- `auto_renew = TRUE AND next_renewal_at <= now() AND budget_type='time_based'
-- AND closed_at IS NULL AND archived_at IS NULL`. A partial index on
-- `next_renewal_at` covering only auto-renewing budgets keeps that hourly scan
-- cheap as the budgets table grows (the index only holds the small recurring
-- subset, not every budget).
CREATE INDEX budgets_auto_renew_due_idx
    ON budgets (next_renewal_at)
    WHERE auto_renew AND closed_at IS NULL AND archived_at IS NULL;
