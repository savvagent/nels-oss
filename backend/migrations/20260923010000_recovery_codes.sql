-- One-time recovery codes (nels#551). Issued in a batch at registration (and
-- rotated by an admin-assisted reset); each is redeemable exactly once to
-- re-enroll a passkey when a user has no surviving credential for an RP.
-- Codes are high-entropy random tokens, not user-chosen passwords, so a fast
-- hash (SHA-256) is sufficient — no need for a slow KDF.
CREATE TABLE IF NOT EXISTS recovery_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    used_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS recovery_codes_user_hash_idx
    ON recovery_codes (user_id, code_hash);
CREATE INDEX IF NOT EXISTS recovery_codes_unused_idx
    ON recovery_codes (user_id) WHERE used_at IS NULL;
