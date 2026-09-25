-- One row per Nels user who has ever started a Basiq consent flow, so
-- create_consent_session can genuinely REUSE an existing Basiq user
-- instead of minting a new one on every call (code review fix, #323) —
-- required for disconnect_linked_account to reliably resolve the Basiq
-- user id for connections created in EARLIER calls, since a user_id can
-- accumulate multiple Basiq connections over time.
CREATE TABLE IF NOT EXISTS basiq_users (
    user_id        UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    basiq_user_id  TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
