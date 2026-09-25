-- Per-viewer active-budget preference (#255), decoupled from the owner-scoped
-- budgets.is_default column (which represents "which of my OWN budgets is my
-- default", not "which budget — owned or shared — is currently active for
-- me"). NULL means "no override set — fall back to the existing is_default
-- resolution". ON DELETE SET NULL (mirroring budgets.rollup_parent_id's
-- existing idiom) so deleting the referenced budget cannot leave a dangling
-- preference or block the delete.
ALTER TABLE users ADD COLUMN IF NOT EXISTS active_budget_id UUID REFERENCES budgets(id) ON DELETE SET NULL;
