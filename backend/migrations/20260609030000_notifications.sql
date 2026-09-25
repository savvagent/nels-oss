-- Pillar 2: in-app notification feed (budget-limit alerts + fired reminders)
-- and user-defined recurring reminders.

CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE,
    kind VARCHAR(20) NOT NULL, -- 'limit_warning' | 'limit_exceeded' | 'reminder'
    message TEXT NOT NULL,
    is_read BOOLEAN NOT NULL DEFAULT FALSE,
    dedup_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS notifications_user_id_idx ON notifications (user_id, created_at DESC);
-- Each (user, dedup_key) fires once; reminder rows use NULL dedup_key (always inserted).
CREATE UNIQUE INDEX IF NOT EXISTS notifications_dedup_idx
    ON notifications (user_id, dedup_key) WHERE dedup_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS reminders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE,
    message TEXT NOT NULL,
    cadence VARCHAR(20) NOT NULL, -- 'daily' | 'weekly' | 'monthly'
    next_fire_at TIMESTAMPTZ NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS reminders_due_idx ON reminders (next_fire_at) WHERE is_active;
