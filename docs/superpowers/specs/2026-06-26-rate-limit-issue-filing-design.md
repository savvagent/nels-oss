# Rate-limit GitHub issue filing (#191) — Design

## Goal
Prevent a single user from flooding the project repo with issues via either the REST `/issues-create` path (`POST /api/github/issues`) or the chat `REPORT_ISSUE` action, with an honest user-facing message when the quota is hit.

## Key decisions (assumptions)
- **Per-user** quota keyed by the authenticated `user_id` (available on both paths: REST `Extension<Uuid>`, chat `user_id`).
- **DB-backed** state: new `github_issue_filings` table (consistent with sessions/auth_flows/llm_usage; survives Fly machine restarts/scale-up).
- Defaults: **5 issues / 3600s rolling window**, overridable via `GITHUB_ISSUE_RATE_LIMIT` and `GITHUB_ISSUE_RATE_WINDOW_SECS` (optional-env convention like `GITHUB_TOKEN`). Floor of 1 each (limit cannot be disabled — intentional, always-on throttle).
- Quota enforced **inside `create_issue_core()`** (the single chokepoint), whose signature gains `db: &PgPool, user_id: Uuid`. Both callers already have both → impossible to bypass.
- Only **successful** filings count (a row is recorded after GitHub returns success). Recording is **fail-safe** (insert failure → warn log, issue still succeeds).
- **GitHub-header backstop**: upstream 429, or 403 with `x-ratelimit-remaining: 0` → 429 with a clear message instead of the generic 502 (pure, unit-tested detector).
- Enforcement order in `create_issue_core`: (1) empty title → 400; (2) per-user quota → 429; (3) missing `GITHUB_TOKEN` → 503; (4) network POST; (5) on success, record. Quota precedes config/network so it short-circuits before any GitHub call and is testable without a token.

## Architecture
- Migration `backend/migrations/20260626000000_github_issue_filings.sql`: `github_issue_filings(id UUID PK, user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW())` + index `(user_id, created_at DESC)`.
- `backend/src/github.rs`: `rate_limit_config()` (pure), `is_github_rate_limited(status, headers)` (pure), `check_issue_quota(db, user_id) -> Result<(),(StatusCode,String)>`, `record_issue_filing(db, user_id)` (fail-safe), updated `create_issue_core(db, user_id, ...)`, updated `create_issue` REST handler (pass `&state.db`, `user_id`).
- `backend/src/rag.rs` REPORT_ISSUE arm: pass `&state.db, user_id`. Existing `Err((_status, msg)) => mutation_error = Some(format!("I couldn't file that with the team: {}", msg))` surfaces the 429 honestly — no other change.
- Docs: `.env.example` + `fly.toml` comment for the two new optional vars.

## Success criteria (all test-covered)
1. (N+1)th filing within window from `create_issue_core` → 429 (covers both paths).
2. REST handler returns HTTP 429 + message when over quota.
3. Chat path surfaces the 429 via `mutation_error` (no false "filed!").
4. Out-of-window rows don't count; other users isolated.
5. Covered by pure unit tests + DB-backed `--ignored` tests.

## Tests
- Pure: `rate_limit_config` defaults/overrides/clamp; `is_github_rate_limited` 429 & 403+remaining:0 vs negatives; empty-title-400 (lazy pool).
- DB `--ignored`: `check_issue_quota` window enforcement + isolation + out-of-window exclusion; `create_issue_core` 429 over quota via `CHAT_FOOTER`; REST `create_issue` oneshot 429; `record_issue_filing` round-trip.

## Risks
- TOCTOU: concurrent requests may overshoot by at most (concurrency−1). Accepted — abuse throttle, not a hard limit.
- `--ignored` tests require the new table in the local test DB (apply migration before running).
- `create_issue_core` signature change updates both in-repo callers in the same change (private module, no external callers).
