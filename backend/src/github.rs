//! Minimal, authenticated server-side GitHub integration for the chat command
//! palette (#56).
//!
//! Two endpoints, both gated by the existing Nels session middleware (so the
//! caller must be a logged-in Nels user) and by a single, server-held GitHub
//! token. The token is **never** sent to the client: the browser cannot run
//! `gh` and must not see a credential, so all GitHub API calls happen here.
//!
//! Configuration (least privilege):
//! - `GITHUB_TOKEN` — a fine-grained PAT (or classic token) scoped to *Issues:
//!   read & write* on the target repo only. If unset, the GitHub-backed
//!   commands degrade gracefully (HTTP 503 + a clear message) rather than
//!   failing with a 500 or leaking that a token is/ isn't present beyond
//!   "not configured".
//! - `GITHUB_REPO` — `owner/repo`, defaults to `savvagent/nels`. Scopes every
//!   call to the project repo.

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use chrono::Utc;
use sqlx::PgPool;

use crate::auth::AppState;

const GITHUB_API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "nels-app";
const DEFAULT_REPO: &str = "savvagent/nels";

/// How many issues `/issues-list` returns (most-recent first).
const LIST_LIMIT: u8 = 20;

/// Default per-user issue-filing quota and rolling window (seconds), overridable
/// via `GITHUB_ISSUE_RATE_LIMIT` / `GITHUB_ISSUE_RATE_WINDOW_SECS`.
const DEFAULT_RATE_LIMIT: i64 = 5;
const DEFAULT_WINDOW_SECS: i64 = 3600;

/// User-facing message when a user exceeds their own issue-filing quota.
const RATE_LIMITED_MSG: &str =
    "You've filed several reports recently. Please wait a bit before filing another.";

/// User-facing message when GitHub itself is rate-limiting us (header backstop).
const GITHUB_RATE_LIMITED_MSG: &str =
    "GitHub is temporarily rate-limiting issue creation. Please try again later.";

/// Resolved GitHub configuration. Absent token => integration disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConfig {
    pub token: String,
    /// `owner/repo`.
    pub repo: String,
}

impl GithubConfig {
    /// Read configuration from the environment.
    ///
    /// Returns `None` when `GITHUB_TOKEN` is unset or blank — the signal that
    /// the integration is not configured and callers should degrade
    /// gracefully. `GITHUB_REPO` falls back to the project repo.
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("GITHUB_TOKEN").ok()?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return None;
        }
        let repo = std::env::var("GITHUB_REPO")
            .ok()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| DEFAULT_REPO.to_string());
        Some(Self { token, repo })
    }
}

/// Resolve the per-user issue-filing quota `(limit, window_secs)` from the
/// environment, falling back to the defaults. Values below 1 (and unparseable
/// values) are rejected and replaced by the default — so the effective floor is
/// the default and the limit cannot be disabled.
pub fn rate_limit_config() -> (i64, i64) {
    fn read(name: &str, default: i64) -> i64 {
        match std::env::var(name) {
            Ok(raw) => match raw.trim().parse::<i64>() {
                Ok(n) if n >= 1 => n,
                _ => {
                    tracing::warn!(
                        var = name,
                        value = %raw,
                        default,
                        "ignoring invalid issue rate-limit env value; using default"
                    );
                    default
                }
            },
            Err(_) => default, // unset is normal; no warning
        }
    }
    (
        read("GITHUB_ISSUE_RATE_LIMIT", DEFAULT_RATE_LIMIT),
        read("GITHUB_ISSUE_RATE_WINDOW_SECS", DEFAULT_WINDOW_SECS),
    )
}

/// Base URL for the GitHub REST API. Overridable via `GITHUB_API_BASE` so tests
/// can point at a local mock server; unset in production, where it defaults to
/// the real API. Whitespace- and trailing-slash-trimmed (so an override like
/// `http://localhost:3001/` doesn't produce `//` in constructed paths); a blank
/// value falls back to the default.
fn api_base() -> String {
    std::env::var("GITHUB_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| GITHUB_API.to_string())
}

/// GitHub-header backstop: detect that GitHub itself is rate-limiting us, so we
/// surface a clear 429 rather than a generic 502. True when the response is a
/// 429, or a 403 indicating either the primary rate limit
/// (`x-ratelimit-remaining: 0`) or a secondary / abuse rate limit (a
/// `retry-after` header, which GitHub returns often with a non-zero remaining).
pub fn is_github_rate_limited(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> bool {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        // Primary rate limit: the remaining budget is exhausted.
        let remaining_exhausted = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim() == "0")
            .unwrap_or(false);
        // Secondary / abuse rate limit: GitHub returns 403 with a `retry-after`
        // header (often with a non-zero remaining), so treat that as a rate
        // limit too rather than a generic upstream error.
        let has_retry_after = headers.contains_key("retry-after");
        return remaining_exhausted || has_retry_after;
    }
    false
}

/// Enforce the per-user rolling-window issue-filing quota. Counts the user's
/// filings within the configured window; returns `Err((429, RATE_LIMITED_MSG))`
/// when the user is at or over the limit, otherwise `Ok(())`. A DB failure
/// fails closed (we do not allow unbounded filing when the quota can't be
/// checked).
pub async fn check_issue_quota(
    db: &PgPool,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let (limit, window_secs) = rate_limit_config();
    // try_seconds guards against an absurdly large env-configured window that
    // would otherwise panic chrono's Duration::seconds.
    let window = chrono::Duration::try_seconds(window_secs)
        .unwrap_or_else(|| chrono::Duration::seconds(DEFAULT_WINDOW_SECS));
    let cutoff = Utc::now() - window;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_issue_filings WHERE user_id = $1 AND created_at >= $2",
    )
    .bind(user_id)
    .bind(cutoff)
    .fetch_one(db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "issue rate-limit count query failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not check the issue rate limit right now. Please try again later.".to_string(),
        )
    })?;

    if count >= limit {
        return Err((StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED_MSG.to_string()));
    }
    Ok(())
}

/// Record one successful issue filing for `user_id` so it counts toward the
/// rolling-window quota. Fail-safe: a write failure is logged at `warn` and
/// never fails an already-created issue (mirrors the `llm_usage` write).
pub async fn record_issue_filing(db: &PgPool, user_id: Uuid) {
    let res = sqlx::query("INSERT INTO github_issue_filings (id, user_id) VALUES ($1, $2)")
        .bind(Uuid::new_v4())
        .bind(user_id)
        .execute(db)
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, user_id = %user_id, "failed to record issue filing for rate limiting");
    }
}

/// Delete issue-filing rows older than the configured rolling window — once
/// outside the window they can never count toward the quota again, so they are
/// dead weight. Runs on the hourly background ticker alongside the other
/// retention purges (auth flows, embeddings, audit logs, notifications).
/// Returns the number of rows removed.
pub async fn purge_old_issue_filings(db: &PgPool) -> Result<u64, sqlx::Error> {
    let (_, window_secs) = rate_limit_config();
    let cutoff = Utc::now()
        - chrono::Duration::try_seconds(window_secs)
            .unwrap_or_else(|| chrono::Duration::seconds(DEFAULT_WINDOW_SECS));
    let res = sqlx::query("DELETE FROM github_issue_filings WHERE created_at < $1")
        .bind(cutoff)
        .execute(db)
        .await?;
    Ok(res.rows_affected())
}

/// Redact obvious secrets / tokens / financial identifiers from free text
/// before it is sent to GitHub (AC: exclude secrets, tokens, PII, financial
/// data). Best-effort heuristic, documented as such — not a guarantee.
///
/// Rules, applied per whitespace-delimited word:
/// - Known credential prefixes (`ghp_`, `gho_`, `ghs_`, `github_pat_`, `sk-`,
///   `AKIA`, `xoxb-`/`xoxp-`) => the whole word becomes `[redacted]`.
/// - A bearer literal (`Bearer <x>`) collapses the following token.
/// - Any run of >= 12 digits (after stripping `-` and spaces inside the word)
///   => `[redacted]` (card / account / routing numbers).
/// - Anything shaped like an email address => `[redacted]`. Issues land in a
///   GitHub repo, and the model has written reporters' and household members'
///   addresses into issue text before (#566), so an address is never sent.
pub fn redact_sensitive(input: &str) -> String {
    const PREFIXES: [&str; 8] = [
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "sk-",
        "AKIA",
        "xoxb-",
        "xoxp-",
    ];

    let mut out: Vec<String> = Vec::new();
    let mut redact_next = false;

    for word in input.split_whitespace() {
        if redact_next {
            out.push("[redacted]".to_string());
            redact_next = false;
            continue;
        }

        // "Bearer <token>" — drop the following word.
        if word.eq_ignore_ascii_case("bearer") {
            out.push(word.to_string());
            redact_next = true;
            continue;
        }

        // Match credential prefixes ANYWHERE in the word, not just at the
        // start, so embedded secrets like `token=ghp_…` or `key:AKIA…` are
        // still caught.
        let has_prefix = PREFIXES.iter().any(|p| word.contains(p));

        // Count digits ignoring separators commonly used in card/account numbers.
        let digit_count = word.chars().filter(|c| c.is_ascii_digit()).count();
        let non_digit_sep = word
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == ' ');
        let long_digit_run = digit_count >= 12 && non_digit_sep;

        // High-entropy-looking opaque token: a single long unbroken run of
        // letters+digits (>= 30 chars, containing at least one digit). Catches
        // generic API keys/secrets that lack a known prefix. Ordinary prose
        // words are far shorter, so this rarely touches legitimate text.
        let opaque_token = word.len() >= 30
            && word.chars().all(|c| c.is_ascii_alphanumeric())
            && word.chars().any(|c| c.is_ascii_digit())
            && word.chars().any(|c| c.is_ascii_alphabetic());

        if has_prefix || long_digit_run || opaque_token || looks_like_email(word) {
            out.push("[redacted]".to_string());
        } else {
            out.push(word.to_string());
        }
    }

    out.join(" ")
}

/// True when `word`, ignoring surrounding punctuation such as `(`, `'` or a
/// trailing `.`, has a non-empty local part, an `@`, and a dotted domain
/// ending in a label of two or more letters. A bare `@handle` or `a@b` is not
/// an address and is left alone.
fn looks_like_email(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let Some((local, domain)) = w.split_once('@') else {
        return false;
    };
    let tld = domain.rsplit('.').next().unwrap_or("");
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
}

// --- DTOs ---

/// One issue as surfaced to the client (a safe subset of GitHub's payload).
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubIssueItem {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
}

/// GitHub's create-issue / list-issue response shape (the fields we keep).
#[derive(Debug, Deserialize)]
struct GithubApiIssue {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    /// Present on issues, absent on pull requests. Used to exclude PRs from the
    /// issue list (the GitHub "issues" endpoint includes PRs).
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

impl From<GithubApiIssue> for GithubIssueItem {
    fn from(i: GithubApiIssue) -> Self {
        Self {
            number: i.number,
            title: i.title,
            state: i.state,
            html_url: i.html_url,
        }
    }
}

/// Request body for `/issues-create`.
#[derive(Debug, Deserialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
}

/// Body footer appended to issues created from the command palette, for provenance.
const APP_FOOTER: &str = "\n\n---\n_Filed from the Nels app via the command palette._";

/// Body footer for issues Nels files on a user's behalf from the chat assistant.
/// Distinct from `APP_FOOTER` so the development team can tell at a glance whether
/// a report came from a deliberate `/issues-create` command or from the AI acting
/// on a user's reported problem.
pub const CHAT_FOOTER: &str = "\n\n---\n_Filed by Nels (AI assistant) on a user's behalf from chat._";

// --- Handlers ---

const NOT_CONFIGURED: &str = "GitHub integration is not configured.";

fn not_configured() -> (StatusCode, String) {
    (StatusCode::SERVICE_UNAVAILABLE, NOT_CONFIGURED.to_string())
}

/// Generic upstream-failure response. Never leaks the GitHub response body or
/// the token state beyond a generic message (logged server-side).
fn upstream_error<E: std::fmt::Display>(ctx: &str, err: E) -> (StatusCode, String) {
    tracing::error!(error = %err, "GitHub API error: {ctx}");
    (
        StatusCode::BAD_GATEWAY,
        "Could not reach GitHub right now. Please try again later.".to_string(),
    )
}

/// `GET /api/github/issues` — list open issues for the configured repo.
///
/// Auth-gated by the session middleware (the `Extension<Uuid>` extractor fails
/// the request if no authenticated user is present).
pub async fn list_issues(
    State(_state): State<AppState>,
    Extension(_user_id): Extension<Uuid>,
) -> Result<Json<Vec<GithubIssueItem>>, (StatusCode, String)> {
    let cfg = match GithubConfig::from_env() {
        Some(c) => c,
        None => return Err(not_configured()),
    };

    let url = format!(
        "{}/repos/{}/issues?state=open&per_page={LIST_LIMIT}&sort=created&direction=desc",
        api_base(),
        cfg.repo
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .bearer_auth(&cfg.token)
        .send()
        .await
        .map_err(|e| upstream_error("list issues", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        return Err(upstream_error("list issues", format!("status {status}")));
    }

    let issues: Vec<GithubApiIssue> = resp
        .json()
        .await
        .map_err(|e| upstream_error("decode list", e))?;

    // The GitHub issues endpoint also returns PRs; drop them.
    let items: Vec<GithubIssueItem> = issues
        .into_iter()
        .filter(|i| i.pull_request.is_none())
        .map(Into::into)
        .collect();

    Ok(Json(items))
}

/// `POST /api/github/issues` — create an issue in the configured repo.
///
/// Title/body are redacted of obvious secrets before being sent. Empty title
/// is rejected (400). Auth-gated as above.
pub async fn create_issue(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(req): Json<CreateIssueRequest>,
) -> Result<Json<GithubIssueItem>, (StatusCode, String)> {
    let issue =
        create_issue_core(&state.db, user_id, &req.title, req.body.as_deref(), APP_FOOTER).await?;
    // User-level audit (#192): this REST path (POST /github/issues, driven by the
    // /issues-create slash command) is not budget-scoped, so attribute the filing
    // to the user (NULL budget_id). Action `REPORT_ISSUE` (no `AI_` prefix) marks
    // this as the deliberate user-command path, distinct from the chat assistant's
    // `AI_REPORT_ISSUE` (mirrors APP_FOOTER vs CHAT_FOOTER).
    crate::budget::log_user_audit(
        &state.db,
        user_id,
        "REPORT_ISSUE",
        &format!("Filed issue #{}: {}", issue.number, issue.title),
    )
    .await;
    Ok(Json(issue))
}

/// Shared issue-creation core, used by both the `/issues-create` REST handler and
/// the chat `REPORT_ISSUE` action so the AI assistant files real issues through the
/// exact same path (redaction, provenance footer, least-privilege server token).
///
/// `footer` is the provenance line appended to the body (see `APP_FOOTER` /
/// `CHAT_FOOTER`). Returns a user-facing error tuple on empty title (400),
/// per-user quota exceeded (429), missing config (503), or upstream failure
/// (502) — none of which leak the GitHub response body or token state. A 429 is
/// also returned if GitHub itself is rate-limiting us (header backstop).
pub async fn create_issue_core(
    db: &PgPool,
    user_id: Uuid,
    raw_title: &str,
    raw_body: Option<&str>,
    footer: &str,
) -> Result<GithubIssueItem, (StatusCode, String)> {
    let title = raw_title.trim();
    if title.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "A title is required to create an issue.".to_string(),
        ));
    }

    // Per-user rolling-window quota — enforced for BOTH the REST /issues-create
    // path and the chat REPORT_ISSUE action, before any GitHub call. NOTE: the
    // check and the post-success record are separate statements, so highly
    // concurrent requests from one user can overshoot the limit by at most
    // (concurrency - 1). This is an acceptable, documented tradeoff for an
    // abuse throttle (not a hard limit) — see issue #191 / the design doc.
    check_issue_quota(db, user_id).await?;

    let cfg = match GithubConfig::from_env() {
        Some(c) => c,
        None => return Err(not_configured()),
    };

    let title = redact_sensitive(title);
    let body = raw_body.map(redact_sensitive).unwrap_or_default();
    let body = format!("{body}{footer}");

    let url = format!("{}/repos/{}/issues", api_base(), cfg.repo);
    let payload = serde_json::json!({ "title": title, "body": body });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .bearer_auth(&cfg.token)
        .json(&payload)
        .send()
        .await
        .map_err(|e| upstream_error("create issue", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        // Backstop: if GitHub itself is rate-limiting us, say so clearly (429)
        // rather than returning a generic 502.
        if is_github_rate_limited(status, resp.headers()) {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                GITHUB_RATE_LIMITED_MSG.to_string(),
            ));
        }
        return Err(upstream_error("create issue", format!("status {status}")));
    }

    let issue: GithubApiIssue = resp
        .json()
        .await
        .map_err(|e| upstream_error("decode create", e))?;

    // Record only on success — the quota counts issues actually filed.
    record_issue_filing(db, user_id).await;

    Ok(issue.into())
}

/// Test-only lock serializing any test that mutates GitHub-related env vars
/// (`GITHUB_TOKEN`, `GITHUB_REPO`, `GITHUB_API_BASE`, `GITHUB_ISSUE_RATE_LIMIT`,
/// `GITHUB_ISSUE_RATE_WINDOW_SECS`). Shared across modules (github.rs + rag.rs
/// tests) so that under `cargo test -- --include-ignored` — where the pure
/// config test and the DB tests share one process — their env mutations cannot
/// race. Hold it for the whole test body when touching any of these vars. Poison
/// handled at the lock sites via `unwrap_or_else(|e| e.into_inner())`.
#[cfg(test)]
pub(crate) static ISSUE_RATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Row;

    use axum::body::Body;
    use axum::routing::post;
    use axum::{middleware, Router};
    use tower::ServiceExt; // oneshot

    // --- redact_sensitive ---

    #[test]
    fn redacts_github_token_prefixes() {
        let s = redact_sensitive("my token is ghp_ABCdef1234567890 ok");
        assert!(s.contains("[redacted]"));
        assert!(!s.contains("ghp_ABCdef"));
    }

    #[test]
    fn redacts_classic_and_pat_and_aws_and_openai_and_slack() {
        for tok in [
            "github_pat_11ABC",
            "AKIAIOSFODNN7EXAMPLE",
            "sk-abcdEFGH",
            "xoxb-123-abc",
        ] {
            let s = redact_sensitive(&format!("secret {tok} end"));
            assert!(
                s.contains("[redacted]"),
                "expected redaction for {tok}: {s}"
            );
            assert!(!s.contains(tok), "leaked {tok}: {s}");
        }
    }

    #[test]
    fn redacts_bearer_token() {
        let s = redact_sensitive("auth Bearer eyJhbGciFooBar more");
        assert!(s.contains("Bearer [redacted]"), "{s}");
        assert!(!s.contains("eyJhbGciFooBar"));
    }

    #[test]
    fn redacts_long_digit_runs() {
        // 16-digit card-like number, with and without dashes.
        let plain = redact_sensitive("card 4111111111111111 done");
        assert!(plain.contains("[redacted]"), "{plain}");
        let dashed = redact_sensitive("card 4111-1111-1111-1111 done");
        assert!(dashed.contains("[redacted]"), "{dashed}");
    }

    #[test]
    fn redacts_embedded_credential_not_at_word_start() {
        let s = redact_sensitive("set token=ghp_ABCdef123 now"); // gitleaks:allow (fake token)
        assert!(s.contains("[redacted]"), "{s}");
        assert!(!s.contains("ghp_ABCdef123"), "{s}");
    }

    #[test]
    fn redacts_long_opaque_token_without_known_prefix() {
        let s = redact_sensitive("apikey aB3xQ9zK7mN2pL5rT8wV1yU4hG6jD0sF here");
        assert!(s.contains("[redacted]"), "{s}");
        assert!(!s.contains("aB3xQ9zK7mN2pL5rT8wV1yU4hG6jD0sF"), "{s}");
    }

    #[test]
    fn redacts_email_addresses_including_wrapped_in_punctuation() {
        let s = redact_sensitive(
            "User Rob (rob@example.org) shared it with 'jo.smith+nels@mail.example.co.uk'.",
        );
        assert!(!s.contains('@'), "{s}");
        assert_eq!(s, "User Rob [redacted] shared it with [redacted]");
    }

    #[test]
    fn keeps_handles_and_at_signs_that_are_not_addresses() {
        let s = redact_sensitive("cc @robhicks, meet @ 5pm, a@b and user@localhost");
        assert_eq!(s, "cc @robhicks, meet @ 5pm, a@b and user@localhost");
    }

    #[test]
    fn keeps_ordinary_text_and_short_numbers() {
        let s = redact_sensitive("Fix bug in issue 56 affecting 2024 reports");
        assert_eq!(s, "Fix bug in issue 56 affecting 2024 reports");
    }

    #[test]
    fn keeps_normal_words_unchanged() {
        let s = redact_sensitive("Add a command palette to the chat input");
        assert_eq!(s, "Add a command palette to the chat input");
    }

    // --- GithubConfig::from_env ---
    // These mutate process env; keep them in one test to avoid cross-test races.

    #[test]
    fn config_from_env_present_and_absent() {
        // Holds ISSUE_RATE_ENV_LOCK because the #192 happy-path DB tests mutate
        // GITHUB_TOKEN/GITHUB_API_BASE; under `cargo test -- --include-ignored`
        // this pure test shares the process with them.
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Absent token => None.
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GITHUB_REPO");
        assert_eq!(GithubConfig::from_env(), None);

        // Blank token => None.
        std::env::set_var("GITHUB_TOKEN", "   ");
        assert_eq!(GithubConfig::from_env(), None);

        // Present token, default repo.
        std::env::set_var("GITHUB_TOKEN", "ghp_test");
        let cfg = GithubConfig::from_env().expect("config present");
        assert_eq!(cfg.token, "ghp_test");
        assert_eq!(cfg.repo, "savvagent/nels");

        // Present token, explicit repo.
        std::env::set_var("GITHUB_REPO", "acme/widgets");
        let cfg = GithubConfig::from_env().expect("config present");
        assert_eq!(cfg.repo, "acme/widgets");

        // Cleanup so we don't leak into other tests in this binary.
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GITHUB_REPO");
    }

    // --- DTO (de)serialization ---

    #[test]
    fn issue_item_parses_github_payload_and_drops_pr() {
        let json = r#"[
            {"number": 56, "title": "Palette", "state": "open", "html_url": "https://x/56"},
            {"number": 88, "title": "PR", "state": "open", "html_url": "https://x/88", "pull_request": {"url": "u"}}
        ]"#;
        let parsed: Vec<GithubApiIssue> = serde_json::from_str(json).unwrap();
        let items: Vec<GithubIssueItem> = parsed
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .map(Into::into)
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 56);
        assert_eq!(items[0].title, "Palette");
    }

    #[test]
    fn create_request_parses_with_and_without_body() {
        let with: CreateIssueRequest = serde_json::from_str(r#"{"title":"T","body":"B"}"#).unwrap();
        assert_eq!(with.title, "T");
        assert_eq!(with.body.as_deref(), Some("B"));

        let without: CreateIssueRequest = serde_json::from_str(r#"{"title":"T"}"#).unwrap();
        assert_eq!(without.body, None);
    }

    // --- rate_limit_config ---
    // Mutates process env; takes ISSUE_RATE_ENV_LOCK because under
    // `cargo test -- --include-ignored` it shares the process with the
    // env-mutating DB tests.
    #[test]
    fn rate_limit_config_defaults_and_overrides() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("GITHUB_ISSUE_RATE_LIMIT");
        std::env::remove_var("GITHUB_ISSUE_RATE_WINDOW_SECS");
        assert_eq!(rate_limit_config(), (5, 3600));

        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "10");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "60");
        assert_eq!(rate_limit_config(), (10, 60));

        // Malformed / out-of-range values fall back to the defaults (values < 1 are rejected and replaced by the default).
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "0");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "abc");
        assert_eq!(rate_limit_config(), (5, 3600));

        std::env::remove_var("GITHUB_ISSUE_RATE_LIMIT");
        std::env::remove_var("GITHUB_ISSUE_RATE_WINDOW_SECS");
    }

    // --- is_github_rate_limited ---
    #[test]
    fn detects_github_rate_limit_responses() {
        use reqwest::header::HeaderMap;
        use reqwest::StatusCode as RStatus;

        // 429 is always a rate-limit signal regardless of headers.
        assert!(is_github_rate_limited(RStatus::TOO_MANY_REQUESTS, &HeaderMap::new()));

        // 403 with exhausted remaining => rate limited.
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        assert!(is_github_rate_limited(RStatus::FORBIDDEN, &h));

        // 403 with remaining left => NOT a rate limit (some other 403).
        let mut h2 = HeaderMap::new();
        h2.insert("x-ratelimit-remaining", "57".parse().unwrap());
        assert!(!is_github_rate_limited(RStatus::FORBIDDEN, &h2));

        // 403 with no header => not treated as rate limited.
        assert!(!is_github_rate_limited(RStatus::FORBIDDEN, &HeaderMap::new()));

        // 403 secondary/abuse limit: retry-after present => rate limited.
        let mut h3 = HeaderMap::new();
        h3.insert("retry-after", "60".parse().unwrap());
        assert!(is_github_rate_limited(RStatus::FORBIDDEN, &h3));

        // Unrelated failure.
        assert!(!is_github_rate_limited(RStatus::BAD_GATEWAY, &HeaderMap::new()));
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn check_issue_quota_enforces_window_and_isolation() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // ISSUE_RATE_ENV_LOCK (held for the whole test) is what makes these env mutations
        // safe — including under `cargo test -- --include-ignored`, where the
        // pure rate_limit_config test shares this process. We intentionally do
        // NOT remove_var at the end: every env-mutating test sets the same
        // values, and the pure test removes them itself (under the same lock)
        // before asserting defaults, so leaving them set cannot mislead it.
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        for id in [user, other] {
            sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(format!("rl-{id}@example.test"))
                .bind(false)
                .execute(&pool)
                .await
                .expect("seed user");
        }

        // 2 recent filings (< limit) => Ok.
        for _ in 0..2 {
            record_issue_filing(&pool, user).await;
        }
        assert!(check_issue_quota(&pool, user).await.is_ok(), "2 < limit 3 is allowed");

        // A 3rd recent filing => at limit => next check is rejected with 429.
        record_issue_filing(&pool, user).await;
        let err = check_issue_quota(&pool, user)
            .await
            .expect_err("3 >= limit 3 must be rejected");
        assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.1.contains("filed several reports"), "clear message: {}", err.1);

        // An out-of-window filing (2h old) does NOT count.
        let old_user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(old_user)
            .bind(format!("rl-{old_user}@example.test"))
            .bind(false)
            .execute(&pool).await.expect("seed old user");
        for _ in 0..5 {
            sqlx::query("INSERT INTO github_issue_filings (id, user_id, created_at) VALUES ($1, $2, NOW() - INTERVAL '2 hours')")
                .bind(Uuid::new_v4())
                .bind(old_user)
                .execute(&pool).await.expect("seed old filing");
        }
        assert!(check_issue_quota(&pool, old_user).await.is_ok(), "out-of-window filings do not count");

        // The `other` user (no recent filings) is unaffected.
        assert!(check_issue_quota(&pool, other).await.is_ok(), "different user is isolated");

        // Cleanup (filings cascade on user delete). Intentionally do NOT
        // remove_var the rate-limit env vars — see the determinism note above.
        let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&vec![user, other, old_user])
            .execute(&pool)
            .await;
    }

    // --- create_issue_core validation ---

    #[tokio::test]
    async fn create_issue_core_rejects_empty_title_before_network() {
        // Empty/whitespace title is rejected with 400 *before* any quota check,
        // config lookup, or network call. A lazy pool is never actually queried
        // on this path, so this stays a pure unit test (no DB needed).
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")
            .expect("lazy pool");
        let err = create_issue_core(&pool, Uuid::new_v4(), "   ", None, APP_FOOTER)
            .await
            .expect_err("empty title must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    // A DB failure must FAIL CLOSED (500), never silently allow an unbounded
    // filing. Uses a lazy pool to an unreachable address so the COUNT query
    // errors without a real database.
    #[tokio::test]
    async fn check_issue_quota_fails_closed_on_db_error() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/none")
            .expect("lazy pool");
        let err = check_issue_quota(&pool, Uuid::new_v4())
            .await
            .expect_err("DB error must fail closed");
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Proves the gated core rejects an over-quota user with 429 regardless of
    // the footer (REST APP_FOOTER vs chat CHAT_FOOTER) — it short-circuits before
    // any network call, so no GITHUB_TOKEN is required. The chat caller itself is
    // covered separately by the rag.rs `chat_report_issue` test.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_issue_core_rejects_when_over_quota_regardless_of_footer() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // ISSUE_RATE_ENV_LOCK (held for the whole test) is what makes these env mutations
        // safe — including under `cargo test -- --include-ignored`, where the
        // pure rate_limit_config test shares this process. We intentionally do
        // NOT remove_var at the end: every env-mutating test sets the same
        // values, and the pure test removes them itself (under the same lock)
        // before asserting defaults, so leaving them set cannot mislead it.
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");

        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(user).bind(format!("core-{user}@example.test")).bind(false)
            .execute(&pool).await.expect("seed user");
        for _ in 0..3 {
            record_issue_filing(&pool, user).await;
        }

        // Chat path footer.
        let chat = create_issue_core(&pool, user, "Bug report", Some("detail"), CHAT_FOOTER)
            .await
            .expect_err("over quota must reject (chat)");
        assert_eq!(chat.0, StatusCode::TOO_MANY_REQUESTS);
        assert!(chat.1.contains("filed several reports"), "msg: {}", chat.1);

        // REST path footer.
        let rest = create_issue_core(&pool, user, "Bug report", Some("detail"), APP_FOOTER)
            .await
            .expect_err("over quota must reject (rest)");
        assert_eq!(rest.0, StatusCode::TOO_MANY_REQUESTS);
        assert!(rest.1.contains("filed several reports"), "rest msg: {}", rest.1);

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await;
    }

    // The REST POST /github/issues handler returns HTTP 429 when over quota,
    // with user_id injected exactly as auth_middleware would (mirrors usage.rs).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_issue_handler_returns_429_when_over_quota() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(user_id).bind(format!("h-{user_id}@example.test")).bind(false)
            .execute(&pool).await.expect("seed user");
        for _ in 0..3 {
            record_issue_filing(&pool, user_id).await;
        }

        let app = Router::new()
            .route("/github/issues", post(create_issue))
            .layer(middleware::from_fn(move |mut req: axum::http::Request<Body>, next: axum::middleware::Next| {
                async move {
                    req.extensions_mut().insert(user_id);
                    next.run(req).await
                }
            }))
            .with_state(state);

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/github/issues")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"Bug","body":"detail"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "over quota => 429");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("filed several reports"), "clear message over HTTP: {text}");

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_old_issue_filings_drops_old_keeps_recent() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "3");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(user).bind(format!("purge-{user}@example.test")).bind(false)
            .execute(&pool).await.expect("seed user");

        // One recent filing (kept) + two 2h-old filings (purged under a 1h window).
        record_issue_filing(&pool, user).await;
        for _ in 0..2 {
            sqlx::query("INSERT INTO github_issue_filings (id, user_id, created_at) VALUES ($1, $2, NOW() - INTERVAL '2 hours')")
                .bind(Uuid::new_v4()).bind(user).execute(&pool).await.expect("seed old filing");
        }

        let removed = purge_old_issue_filings(&pool).await.expect("purge");
        assert!(removed >= 2, "purges the two out-of-window rows, got {removed}");

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM github_issue_filings WHERE user_id = $1")
            .bind(user).fetch_one(&pool).await.expect("count");
        assert_eq!(remaining, 1, "keeps the in-window filing");

        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await;
    }

    // End-to-end happy path (#192): a SUCCESSFUL REST /issues-create writes a
    // user-scoped `REPORT_ISSUE` audit row (NULL budget_id). A local mock server
    // stands in for api.github.com via GITHUB_API_BASE, so this exercises the real
    // path: the `create_issue` handler calls `create_issue_core` (which on success
    // records the filing), then on `Ok` the handler calls `log_user_audit`.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn create_issue_handler_writes_audit_on_success() {
        let _env = super::ISSUE_RATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Local mock GitHub API: any POST returns a created issue.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mock = Router::new().fallback(|| async {
                axum::Json(serde_json::json!({
                    "number": 4242,
                    "title": "Mocked issue title",
                    "state": "open",
                    "html_url": "https://github.com/savvagent/nels/issues/4242"
                }))
            });
            let _ = axum::serve(listener, mock).await;
        });

        std::env::set_var("GITHUB_API_BASE", format!("http://{addr}"));
        std::env::set_var("GITHUB_TOKEN", "ghp_test_mock");
        std::env::set_var("GITHUB_REPO", "savvagent/nels");
        std::env::set_var("GITHUB_ISSUE_RATE_LIMIT", "100");
        std::env::set_var("GITHUB_ISSUE_RATE_WINDOW_SECS", "3600");

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = PgPool::connect(&url).await.expect("connect to test db");
        let state = AppState {
            db: pool.clone(),
            cipher: std::sync::Arc::new(crate::crypto::SecretCipher::new(&[42u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email, is_admin) VALUES ($1, $2, $3)")
            .bind(user_id)
            .bind(format!("rest-aud-{user_id}@example.test"))
            .bind(false)
            .execute(&pool)
            .await
            .expect("seed user");

        let app = Router::new()
            .route("/github/issues", post(create_issue))
            .layer(middleware::from_fn(move |mut req: axum::http::Request<Body>, next: axum::middleware::Next| {
                async move {
                    req.extensions_mut().insert(user_id);
                    next.run(req).await
                }
            }))
            .with_state(state);

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/github/issues")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"My bug","body":"detail"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "successful filing => 200");

        let row = sqlx::query(
            "SELECT budget_id, user_id, action FROM audit_logs \
             WHERE user_id = $1 AND action = 'REPORT_ISSUE'",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("REST success must write a REPORT_ISSUE audit row");
        let budget_id: Option<Uuid> = row.get("budget_id");
        let got_user: Uuid = row.get("user_id");
        assert!(budget_id.is_none(), "REST audit row must be user-scoped (NULL budget_id)");
        assert_eq!(got_user, user_id);

        // Cleanup (filings + audit rows). audit_logs.user_id is ON DELETE SET NULL,
        // so delete audit rows explicitly before the user.
        let _ = sqlx::query("DELETE FROM audit_logs WHERE user_id = $1").bind(user_id).execute(&pool).await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
        std::env::remove_var("GITHUB_API_BASE");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GITHUB_REPO");
        // Intentionally do NOT remove the rate-limit env vars (same convention as
        // the other rate-limit tests): every env-mutating test re-sets them under
        // ISSUE_RATE_ENV_LOCK before use, and the pure config test removes them.
    }
}
