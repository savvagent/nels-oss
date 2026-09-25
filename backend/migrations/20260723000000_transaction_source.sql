-- #403 P1 (provenance foundation): make "who logged this" a real, first-class
-- signal instead of inferring it from `external_account_id` or a fallback
-- description string.
--
--   'ai'       — logged through Nels (chat ADD_TRANSACTION / category finalize)
--   'imported' — auto-synced from a linked bank account (the only rows with
--                `external_account_id` set)
--   'manual'   — reserved for the future manual add-transaction form; no current
--                producer (the not-UI-reachable REST create_transaction writes it)
--
-- DEFAULT 'ai' keeps every existing non-import insert site correct with no code
-- change: chat ADD and finalize both persist through the shared insert helper
-- and simply inherit the default.
ALTER TABLE transactions
  ADD COLUMN source TEXT NOT NULL DEFAULT 'ai'
  CHECK (source IN ('ai', 'imported', 'manual'));

-- Backfill: rows tied to a linked account are imports; all other historical
-- rows were created through Nels (chat ADD / finalize are the only non-sync
-- insert paths), so 'ai' is accurate for them.
UPDATE transactions SET source = 'imported' WHERE external_account_id IS NOT NULL;
