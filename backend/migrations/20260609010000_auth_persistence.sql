-- Persist auth state in the database so it survives backend restarts.
-- Previously sessions and in-progress register/login challenges lived in
-- in-memory HashMaps in AppState, which meant every server restart logged
-- everyone out and killed any in-progress auth flow.

-- Active login sessions. Token is the opaque bearer value sent by the client.
CREATE TABLE IF NOT EXISTS sessions (
    token TEXT PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_user_id_idx ON sessions (user_id);
CREATE INDEX IF NOT EXISTS sessions_expires_at_idx ON sessions (expires_at);

-- In-progress, two-step auth challenges (register or login). A row is created
-- on /start and consumed on /finish. No FK on user_id: during registration the
-- user row does not exist yet (it is only created once the TOTP code verifies).
CREATE TABLE IF NOT EXISTS auth_flows (
    flow_id TEXT PRIMARY KEY,
    flow_type VARCHAR(20) NOT NULL, -- 'register' or 'login'
    user_id UUID NOT NULL,
    email VARCHAR(255) NOT NULL,
    totp_secret VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS auth_flows_expires_at_idx ON auth_flows (expires_at);
