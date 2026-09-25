-- #403 P2 (Needs Review): give bank-synced transactions a review lifecycle so a
-- freshly imported row can be surfaced as "needs your review" and approved from
-- the transactions list, without touching Nels-logged (`ai`) or `manual` rows.
--
--   'needs_review' — set at the sync insert sites for newly imported rows.
--   'reviewed'     — the terminal state; set on approve, and the DEFAULT so that
--                    every non-import insert path (chat ADD / finalize) and every
--                    existing row is already "reviewed" with no code change.
--
-- DEFAULT 'reviewed' is deliberately correct for ALL existing rows: historical
-- imports are NOT retro-flagged (the review nag is only for rows imported AFTER
-- this ships), so there is intentionally NO backfill UPDATE here.
ALTER TABLE transactions
  ADD COLUMN review_status TEXT NOT NULL DEFAULT 'reviewed'
  CHECK (review_status IN ('needs_review', 'reviewed'));
