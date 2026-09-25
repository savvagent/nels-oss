-- Reshape auth_flows to carry WebAuthn ceremony state instead of a TOTP
-- secret (nels#551). This column is written/read only by auth.rs/account.rs,
-- so it's safe to change in place rather than adding alongside.
ALTER TABLE auth_flows DROP COLUMN IF EXISTS totp_secret;
ALTER TABLE auth_flows ADD COLUMN IF NOT EXISTS rp TEXT NOT NULL DEFAULT 'app';
ALTER TABLE auth_flows ADD COLUMN IF NOT EXISTS ceremony_state JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE auth_flows ADD COLUMN IF NOT EXISTS recovery_code_id UUID REFERENCES recovery_codes(id);

-- flow_type is now one of: 'register' | 'login' | 'recovery' | 'delete'.
