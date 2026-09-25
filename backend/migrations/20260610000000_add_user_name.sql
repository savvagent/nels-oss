-- Add a nullable display name for users, captured conversationally by Nels.
ALTER TABLE users ADD COLUMN IF NOT EXISTS name TEXT;
