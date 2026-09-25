-- #195: track when a transaction was last modified, and make transactions
-- semantically searchable via a pgvector embedding (768-dim to match the
-- chat_messages.embedding column / gemini-embedding-001 output).

-- Add nullable first, backfill existing rows from created_at (their last
-- user-visible change), then set the default + NOT NULL so new inserts get
-- updated_at = created_at (both default NOW()).
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;
UPDATE transactions SET updated_at = created_at WHERE updated_at IS NULL;
ALTER TABLE transactions ALTER COLUMN updated_at SET DEFAULT NOW();
ALTER TABLE transactions ALTER COLUMN updated_at SET NOT NULL;
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS embedding vector(768);

-- BEFORE-UPDATE trigger keeps updated_at current on user-visible modifications.
-- It does NOT fire on INSERT, so a freshly inserted row has updated_at = created_at
-- (both default to NOW()). The WHEN clause restricts firing to changes a user can
-- make, so system-only writes (e.g. the embedding backfill setting `embedding`)
-- do NOT advance updated_at, which must reflect USER modifications only.
CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at = NOW(); RETURN NEW; END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_transactions_updated_at ON transactions;
CREATE TRIGGER trg_transactions_updated_at
  BEFORE UPDATE ON transactions FOR EACH ROW
  WHEN (OLD.description IS DISTINCT FROM NEW.description
     OR OLD.amount IS DISTINCT FROM NEW.amount
     OR OLD.transaction_date IS DISTINCT FROM NEW.transaction_date
     OR OLD.category_id IS DISTINCT FROM NEW.category_id)
  EXECUTE FUNCTION set_updated_at();

-- HNSW index for cosine-distance ANN search over transaction embeddings.
CREATE INDEX IF NOT EXISTS transactions_embeddings_cosine_idx
  ON transactions USING hnsw (embedding vector_cosine_ops);
