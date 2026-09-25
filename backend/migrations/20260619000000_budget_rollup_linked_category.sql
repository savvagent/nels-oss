-- Issue #52 (linked-rollup mechanism): a category with `linked_budget_id` set is
-- not a normal expense category but a LIVE MIRROR of another budget's total.
-- Rolling a "shed" budget up into a "family" budget creates one such mirror
-- expense category in the family budget; its amount is computed-on-read from the
-- linked (source) budget's own total (consistent with #46 computed totals — there
-- is no denormalized value). NULL = an ordinary category (the default), so every
-- existing category keeps its current behavior — backward compatible, no backfill.
--
-- ON DELETE CASCADE: when the source (linked) budget is deleted, the mirror
-- category is removed with it. The mirror has no meaning without its source, so
-- it is cleaned up automatically rather than left dangling.
ALTER TABLE categories ADD COLUMN linked_budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE;

-- The mirror-resolving read filters categories down to the linked subset. A
-- partial index over only those rows keeps that lookup cheap as the categories
-- table grows (it holds just the small set of mirror categories, not every one).
CREATE INDEX categories_linked_budget_idx
    ON categories (linked_budget_id)
    WHERE linked_budget_id IS NOT NULL;

-- Enforces one mirror category per (parent, source); also makes the rollup link
-- idempotent at the data layer.
CREATE UNIQUE INDEX categories_one_mirror_per_source_idx
    ON categories (budget_id, linked_budget_id)
    WHERE linked_budget_id IS NOT NULL;
