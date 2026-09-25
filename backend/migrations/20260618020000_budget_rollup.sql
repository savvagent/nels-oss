-- Issue #52: roll up one or more budgets into another (parent) budget.
-- `rollup_parent_id` links a CHILD budget to its PARENT: the parent aggregates
-- the totals/spend of itself plus its rolled-up children, computed on read
-- (consistent with #46 computed totals and #47/#49 rollover — there is no
-- denormalized total). NULL = standalone (the default), so every existing budget
-- stays standalone with unchanged behavior — backward compatible, no backfill.
--
-- ON DELETE SET NULL: if a parent is deleted, its children survive as standalone
-- budgets (their link is cleared) rather than being cascaded away — rollup is a
-- non-destructive, reversible aggregation, not a merge.
ALTER TABLE budgets ADD COLUMN rollup_parent_id UUID REFERENCES budgets(id) ON DELETE SET NULL;

-- A budget can never roll up into itself. Combined with the app-layer
-- single-level guard (a child cannot also be a parent, and a parent cannot be
-- made a child), this makes rollup cycles structurally impossible. Existing rows
-- (rollup_parent_id NULL) satisfy the constraint trivially.
ALTER TABLE budgets ADD CONSTRAINT budgets_rollup_not_self_check
    CHECK (rollup_parent_id IS NULL OR rollup_parent_id <> id);

-- The aggregated-totals read fans out from a parent to its children
-- (`WHERE rollup_parent_id = $parent AND archived_at IS NULL`). A partial index
-- over only the linked subset keeps that lookup cheap as the budgets table grows
-- (the index holds just the small set of rolled-up children, not every budget).
CREATE INDEX budgets_rollup_parent_idx
    ON budgets (rollup_parent_id)
    WHERE rollup_parent_id IS NOT NULL;
