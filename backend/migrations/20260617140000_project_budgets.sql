-- Issue #48: project-based budgets. `budget_type` distinguishes the existing
-- time-based budgets (default 'time_based') from one-off project budgets
-- ('project'). `closed_at` marks a project budget as closed (read-only) — NULL
-- means open. The defaults keep every existing budget unchanged and require no
-- backfill — backward compatible.
ALTER TABLE budgets ADD COLUMN budget_type TEXT NOT NULL DEFAULT 'time_based';
ALTER TABLE budgets ADD COLUMN closed_at TIMESTAMPTZ;

-- Enforce the two budget_type invariants at the data layer so an invalid state
-- is unrepresentable even if an out-of-band write bypasses the app-level
-- `validate_budget_type` guard: (1) budget_type is one of the known values, and
-- (2) only a project budget may be closed (closed_at set). Existing rows default
-- to ('time_based', NULL), which satisfies both.
ALTER TABLE budgets ADD CONSTRAINT budgets_budget_type_check
    CHECK (budget_type IN ('time_based', 'project'));
ALTER TABLE budgets ADD CONSTRAINT budgets_closed_only_project_check
    CHECK (closed_at IS NULL OR budget_type = 'project');
