-- #403 P3 (non-destructive duplicate reconciliation): link a bank-imported
-- transaction to the pre-existing Nels-logged (`ai`/`manual`) transaction it
-- likely duplicates, so the pair can be surfaced as a resolvable "possible
-- duplicate" instead of silently double-counting the budget.
--
-- The FK is set on the IMPORTED row and points at the Nels-logged row (the
-- newcomer references the row it was found to match). It is populated only by
-- the duplicate matcher that runs in the sync insert path; every other insert
-- path leaves it NULL.
--
-- ON DELETE SET NULL (not CASCADE): reconciliation is NON-DESTRUCTIVE — deleting
-- either twin must never delete the other, only clear the link. This also keeps
-- the existing delete_transaction path safe: deleting the referenced Nels row
-- simply nulls the imported row's link (the "possible duplicate" affordance then
-- disappears), and deleting the imported row drops its own outbound link.
ALTER TABLE transactions
  ADD COLUMN matched_transaction_id UUID NULL
  REFERENCES transactions(id) ON DELETE SET NULL;

-- The matcher looks up "is this Nels row already claimed by an import?" via
-- matched_transaction_id; index it so that NOT EXISTS check stays cheap.
CREATE INDEX IF NOT EXISTS transactions_matched_transaction_id_idx
  ON transactions (matched_transaction_id);
