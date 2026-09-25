-- Pillar 4: financial goals (savings & debt payoff) scoped to a budget,
-- with a contributions ledger. Progress is computed at read time (hybrid:
-- contributions + optional linked-category transactions).

CREATE TABLE IF NOT EXISTS goals (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    goal_type VARCHAR(20) NOT NULL, -- 'savings' | 'debt'
    target_amount DOUBLE PRECISION NOT NULL,
    target_date DATE,
    linked_category_id UUID REFERENCES categories(id) ON DELETE SET NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'active', -- 'active' | 'archived'
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_goal_name_per_budget UNIQUE (budget_id, name)
);

CREATE INDEX IF NOT EXISTS goals_budget_id_idx ON goals (budget_id);

CREATE TABLE IF NOT EXISTS goal_contributions (
    id UUID PRIMARY KEY,
    goal_id UUID NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    amount DOUBLE PRECISION NOT NULL,
    note TEXT,
    contributed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS goal_contributions_goal_id_idx ON goal_contributions (goal_id);
