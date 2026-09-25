-- WebAuthn/FIDO2 passkey credentials (nels#551 — replaces TOTP as the login
-- credential). `rp_id` is 'app' or 'admin': the two Cloudflare Pages projects
-- (nels.pages.dev, nels-admin.pages.dev) are different effective domains under
-- the pages.dev public suffix, so a passkey registered for one origin's
-- relying party cannot assert on the other — each user may hold a separate
-- credential per rp_id.
CREATE TABLE IF NOT EXISTS webauthn_credentials (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    rp_id TEXT NOT NULL,
    credential_id BYTEA NOT NULL,
    passkey JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS webauthn_credentials_rp_credential_idx
    ON webauthn_credentials (rp_id, credential_id);
CREATE INDEX IF NOT EXISTS webauthn_credentials_user_rp_idx
    ON webauthn_credentials (user_id, rp_id);
