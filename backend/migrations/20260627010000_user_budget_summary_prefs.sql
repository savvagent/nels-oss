-- Per-user toggles for the budget status-strip summaries (#208). Default TRUE so
-- existing users see both rows; no backfill needed.
ALTER TABLE users ADD COLUMN IF NOT EXISTS show_zero_based_summary BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS show_limit_spent_remaining_summary BOOLEAN NOT NULL DEFAULT TRUE;
