-- Enable the pgvector extension for AI RAG embeddings
CREATE EXTENSION IF NOT EXISTS vector;

-- Users table (TOTP passwordless authentication)
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) UNIQUE NOT NULL,
    totp_secret VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Budgets table
CREATE TABLE IF NOT EXISTS budgets (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    time_frame VARCHAR(50) NOT NULL, -- 'monthly', 'quarterly', 'yearly'
    budget_limit DOUBLE PRECISION NOT NULL,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Ensure a user can only have one default budget
CREATE UNIQUE INDEX IF NOT EXISTS unique_default_budget_per_user 
ON budgets (owner_id) 
WHERE is_default = TRUE;

-- Categories table (supports income, savings, expenses)
CREATE TABLE IF NOT EXISTS categories (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    name VARCHAR(100) NOT NULL,
    category_type VARCHAR(50) NOT NULL, -- 'income', 'savings', 'expense'
    category_limit DOUBLE PRECISION, -- Optional budget limit for category
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_category_name_per_budget UNIQUE (budget_id, name)
);

-- Transactions table
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    category_id UUID REFERENCES categories(id) ON DELETE SET NULL,
    amount DOUBLE PRECISION NOT NULL, -- Positive for expense/savings/income; type determined by category
    transaction_date TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    description TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Budget shares for multitenant collaboration
CREATE TABLE IF NOT EXISTS budget_shares (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    shared_with_email VARCHAR(255) NOT NULL,
    permission_level VARCHAR(50) NOT NULL, -- 'view', 'edit'
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_budget_share_per_email UNIQUE (budget_id, shared_with_email)
);

-- Audit logs for activities tracking
CREATE TABLE IF NOT EXISTS audit_logs (
    id UUID PRIMARY KEY,
    budget_id UUID NOT NULL REFERENCES budgets(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    action VARCHAR(255) NOT NULL,
    details TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Chat messages table with vector support for RAG
CREATE TABLE IF NOT EXISTS chat_messages (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    budget_id UUID REFERENCES budgets(id) ON DELETE CASCADE,
    sender VARCHAR(50) NOT NULL, -- 'user', 'ai'
    message_text TEXT NOT NULL,
    embedding vector(768), -- Gemini text-embedding-004 produces 768-dimension vectors
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for vector cosine similarity queries
CREATE INDEX IF NOT EXISTS chat_embeddings_cosine_idx 
ON chat_messages USING hnsw (embedding vector_cosine_ops);
