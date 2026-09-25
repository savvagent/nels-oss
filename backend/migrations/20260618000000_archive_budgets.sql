-- Issue #50: archive inactive budgets. `archived_at` marks a budget as archived,
-- which hides it from the main budget list while preserving all of its data and
-- history — NULL means active/unarchived. The default NULL keeps every existing
-- budget unarchived and requires no backfill — backward compatible.
--
-- Archiving is distinct from #48's `closed_at`: closing is part of the project
-- budget lifecycle (read-only, project budgets only), whereas archiving is a
-- reversible visibility flag that may apply to ANY budget. Because any budget may
-- be archived, no CHECK constraint is needed.
ALTER TABLE budgets ADD COLUMN archived_at TIMESTAMPTZ;
