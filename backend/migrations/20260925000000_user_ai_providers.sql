-- nels-oss#3: per-user model provider choice and bring-your-own-key.
-- One row per user who has chosen BYO. No row means Nels-hosted Gemini.
CREATE TABLE IF NOT EXISTS user_ai_providers (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL CHECK (provider IN ('gemini','openai','anthropic')),
  encrypted_key TEXT NOT NULL,          -- SecretCipher 'encv1:' blob; never returned to clients
  key_last4 TEXT NOT NULL CHECK (char_length(key_last4) <= 4),
  last_verified_at TIMESTAMPTZ NOT NULL,
  last_error TEXT CHECK (last_error IN ('auth_rejected','key_unavailable')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE llm_usage ADD COLUMN IF NOT EXISTS provider TEXT NOT NULL DEFAULT 'gemini';
ALTER TABLE llm_usage ADD COLUMN IF NOT EXISTS key_source TEXT NOT NULL DEFAULT 'nels';
-- llm_usage is the highest-write table and migrations run automatically during
-- rolling restarts. A plain ADD CONSTRAINT ... CHECK holds ACCESS EXCLUSIVE while
-- it scans every row, blocking all usage writes. NOT VALID adds the constraint
-- (enforced for new rows) without the scan; VALIDATE then checks existing rows
-- under only a SHARE UPDATE EXCLUSIVE lock, which does not block inserts.
ALTER TABLE llm_usage ADD CONSTRAINT llm_usage_key_source_check
  CHECK (key_source IN ('nels','byo')) NOT VALID;
ALTER TABLE llm_usage VALIDATE CONSTRAINT llm_usage_key_source_check;
ALTER TABLE llm_usage ADD CONSTRAINT llm_usage_provider_check
  CHECK (provider IN ('gemini','openai','anthropic')) NOT VALID;
ALTER TABLE llm_usage VALIDATE CONSTRAINT llm_usage_provider_check;

-- Rolling-window limiter for PUT /api/user/ai-provider (10 per hour).
CREATE TABLE IF NOT EXISTS ai_key_validation_attempts (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS ai_key_validation_attempts_user_idx
  ON ai_key_validation_attempts (user_id, created_at DESC);
