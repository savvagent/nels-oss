-- Per-user rolling-window rate limit for GitHub issue filing (#191).
-- One row per SUCCESSFULLY created issue (from either the REST /issues-create
-- path or the chat REPORT_ISSUE action), keyed by the authenticated user.
-- create_issue_core counts rows within the configured window to enforce the
-- quota. ON DELETE CASCADE so a deleted user's rows go with them.
CREATE TABLE IF NOT EXISTS github_issue_filings (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS github_issue_filings_user_created_idx
  ON github_issue_filings (user_id, created_at DESC);
