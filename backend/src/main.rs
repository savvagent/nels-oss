use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use axum::{
    http::StatusCode,
    middleware,
    routing::{get, post, put, patch, delete},
    Router,
};
use dotenvy::dotenv;
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{Any, AllowOrigin, CorsLayer};
use tracing_subscriber::prelude::*;

mod db;
mod error;
mod crypto;
mod passkeys;
mod auth;
mod admin;
mod account;
mod budget;
mod goals;
mod notifications;
mod insights;
mod reports;
mod r#rag;
mod github;
mod usage;
mod llm;
mod backfill;
mod billing;
mod financial_connections;
mod bank_provider;
mod gocardless;
mod belvo;
mod basiq;
mod akahu;
mod plaid;
mod bank_linking;
mod ignore_rules;
mod duplicate_match;
mod entitlement;
mod access;
mod assets;
mod retirement;
mod social_security;
mod retirement_projection;
mod plaid_investments;
mod mcp;

use ignore_rules::{create_ignore_rule, list_ignore_rules, delete_ignore_rule};
use auth::{
    register_start, register_finish, login_start, login_finish,
    recovery_start, recovery_finish, me, logout, auth_middleware, AppState,
};
use budget::{
    create_budget, list_budgets, get_budget, update_budget, delete_budget, set_default_budget,
    set_active_budget,
    close_budget, archive_budget, unarchive_budget,
    link_rollup, unlink_rollup,
    create_category, list_categories, categories_table, categories_view, update_category, delete_category,
    create_transaction, list_transactions, update_transaction, delete_transaction, finalize_transaction,
    approve_transaction, resolve_match,
    share_budget, list_shares, revoke_share,
    list_audit_logs
};
use goals::{
    create_goal, list_goals, get_goal, update_goal, delete_goal,
    create_contribution, list_contributions,
};
use r#rag::{
    chat_endpoint, list_chat_history,
    list_conversations, get_conversation_messages, rename_conversation, delete_conversation,
    suggested_question,
};
use notifications::{
    list_notifications_handler, mark_notification_read,
    unread_count_handler, mark_all_read_handler, dismiss_notification_handler,
    create_reminder_handler, list_reminders_handler, delete_reminder_handler,
};
use financial_connections::{start_link, complete_link};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize Logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 2. Load Environment Variables
    let _ = dotenv();

    // Load the AES key for at-rest secret encryption (bank-provider access
    // tokens — see crypto.rs; login no longer uses this, it uses WebAuthn
    // passkeys). Refuse to start if it is missing or malformed — never run
    // with the ability to write plaintext credentials. Generate one with:
    // openssl rand -base64 32
    let cipher = Arc::new(crypto::SecretCipher::from_env().map_err(|e| {
        tracing::error!("Cannot start: {e}. Set TOTP_ENC_KEY (base64 of 32 random bytes).");
        e
    })?);
    tracing::info!("Secret-at-rest encryption enabled.");

    // Two WebAuthn relying parties (nels#551): the app and admin console are
    // different effective domains under the pages.dev public suffix, so each
    // needs its own RP config — see passkeys.rs module docs.
    let webauthn = Arc::new(passkeys::WebauthnRegistry::from_env());

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string());

    tracing::info!("Connecting to PostgreSQL database...");

    // 3. Establish Database Pool
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await?;

    // 4. Run SQL Migrations automatically on start
    tracing::info!("Running database migrations...");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await?;
    tracing::info!("Database migrations completed successfully.");

    // 5. Build App State (all auth state is persisted in the database)
    let state = AppState { db: pool, cipher, webauthn };

    // 6. Background task: hourly purge of expired sessions / auth flows,
    //    stale chat embeddings, old audit logs, and old notifications. The first
    //    tick fires immediately, cleaning up any leftovers on startup.
    let cleanup_pool = state.db.clone();
    let (retention_days, keep_recent) = r#rag::embedding_retention_config();
    tracing::info!(
        retention_days,
        keep_recent,
        "Embedding retention configured"
    );
    let audit_retention_days = budget::audit_log_retention_days();
    let (notif_read_days, notif_unread_days) = notifications::notification_retention_days();
    tracing::info!(
        audit_retention_days,
        notif_read_days,
        notif_unread_days,
        "Audit-log and notification retention configured"
    );
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            auth::purge_expired_auth_state(&cleanup_pool).await;
            match r#rag::purge_stale_embeddings(&cleanup_pool, retention_days, keep_recent).await {
                Ok(n) if n > 0 => tracing::info!(cleared = n, "Purged stale chat embeddings"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to purge stale chat embeddings"),
            }
            match budget::purge_old_audit_logs(&cleanup_pool, audit_retention_days).await {
                Ok(n) if n > 0 => tracing::info!(cleared = n, "Purged old audit logs"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to purge old audit logs"),
            }
            match notifications::purge_old_notifications(
                &cleanup_pool,
                notif_read_days,
                notif_unread_days,
            )
            .await
            {
                Ok(n) if n > 0 => tracing::info!(cleared = n, "Purged old notifications"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to purge old notifications"),
            }
            // Auto-renew due recurring budgets (#51). Idempotent and safe to run
            // hourly: a renewal advances each budget's marker past `now`, so a
            // re-run within the same period renews nothing.
            match budget::renew_due_budgets(&cleanup_pool).await {
                Ok(n) if n > 0 => tracing::info!(renewed = n, "Auto-renewed recurring budgets"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to auto-renew recurring budgets"),
            }
            // Advance fund categories' running balances (#228). Idempotent and
            // safe to run hourly (see budget::advance_fund_categories doc
            // comment); independent of auto_renew — runs for ANY fund category
            // on an eligible time-based, non-closed, non-archived budget.
            match budget::advance_fund_categories(&cleanup_pool).await {
                Ok(n) if n > 0 => tracing::info!(advanced = n, "Advanced fund category balances"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to advance fund category balances"),
            }
            match github::purge_old_issue_filings(&cleanup_pool).await {
                Ok(n) if n > 0 => tracing::info!(cleared = n, "Purged old issue-filing rate-limit rows"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to purge old issue-filing rate-limit rows"),
            }
        }
    });

    // One-shot backfill of transaction embeddings (#195), opt-in via
    // BACKFILL_TRANSACTION_EMBEDDINGS=1. Spawned non-blocking so a normal boot is
    // unaffected; it walks rows with a NULL embedding and fills them. Idempotent —
    // a re-run after a full pass updates nothing. Needs GEMINI_API_KEY to embed.
    if std::env::var("BACKFILL_TRANSACTION_EMBEDDINGS").as_deref() == Ok("1") {
        let backfill_pool = state.db.clone();
        let backfill_api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
        tokio::spawn(async move {
            tracing::info!("Starting one-shot transaction embedding backfill...");
            match backfill::backfill_transaction_embeddings(&backfill_pool, &backfill_api_key).await {
                Ok(n) => tracing::info!(updated = n, "Transaction embedding backfill complete"),
                Err(e) => tracing::warn!(error = %e, "Transaction embedding backfill failed"),
            }
        });
    }

    // Background task: fire due reminders into the notification feed every 60s.
    let reminder_pool = state.db.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            notifications::fire_due_reminders(&reminder_pool).await;
        }
    });

    // Background task: poll active GoCardless-linked accounts for new
    // transactions (nels#320 — GoCardless has no transaction webhook, unlike
    // Stripe Financial Connections). A SEPARATE ticker from the hourly
    // cleanup one above since its cadence is independently configurable.
    {
        let gc_pool = state.db.clone();
        // `.max(1)`: an explicit `GOCARDLESS_POLL_INTERVAL_HOURS=0` (a plausible
        // "disable polling" attempt) would otherwise panic `tokio::time::interval`,
        // which rejects a zero period — clamp to the smallest valid cadence instead.
        let interval_hours: u64 = std::env::var("GOCARDLESS_POLL_INTERVAL_HOURS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(8).max(1);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_hours * 3600));
            loop {
                ticker.tick().await;
                gocardless::poll_active_accounts(&gc_pool).await;
            }
        });
    }

    // Background task: poll active Plaid Investments-linked accounts for fresh
    // holdings/balances (nels#468). Plaid has no reliable push we depend on for
    // v1, so a scheduled poll (mirroring #320's GoCardless poll — GoCardless
    // has no webhook; investments here has no feed we consume) is the refresh
    // mechanism, alongside synchronous manual refresh. Its cadence is
    // independently configurable via INVESTMENTS_POLL_INTERVAL_HOURS.
    {
        let inv_pool = state.db.clone();
        let inv_cipher = state.cipher.clone();
        let interval_hours = plaid_investments::investments_poll_interval_hours();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_hours * 3600));
            loop {
                ticker.tick().await;
                plaid_investments::poll_investment_accounts(&inv_pool, inv_cipher.clone()).await;
            }
        });
    }

    // 7. Setup Router and Routes
    //
    // Auth uses `Authorization: Bearer` tokens (not cookies), so a permissive
    // origin policy is not directly exploitable today. We still fail closed in
    // production: when `CORS_ALLOWED_ORIGINS` is set (comma-separated) we
    // restrict to that allowlist; when it is unset we deny all cross-origin
    // requests in production and only fall back to allow-any in local dev.
    // Production is detected via `FLY_APP_NAME`, which Fly sets automatically.
    let cors_allowed_origins = std::env::var("CORS_ALLOWED_ORIGINS").ok();
    let is_production = std::env::var("FLY_APP_NAME").is_ok();
    let allow_origin = match decide_cors_policy(cors_allowed_origins.as_deref(), is_production) {
        CorsPolicy::Allowlist(origins) => {
            let parsed: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
            tracing::info!("CORS restricted to {} origin(s)", parsed.len());
            AllowOrigin::list(parsed)
        }
        CorsPolicy::AnyOrigin => {
            tracing::info!("CORS allowing any origin (local dev)");
            AllowOrigin::any()
        }
        CorsPolicy::DenyAll => {
            tracing::warn!(
                "CORS_ALLOWED_ORIGINS is unset in production; denying all cross-origin requests. \
                 Set it via `fly secrets set CORS_ALLOWED_ORIGINS=...` to allow your app origin(s)."
            );
            AllowOrigin::list(Vec::new())
        }
    };
    let cors = CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_headers(Any)
        .allow_methods(Any);

    // Public auth routes
    let auth_routes = Router::new()
        .route("/register/start", post(register_start))
        .route("/register/finish", post(register_finish))
        .route("/login/start", post(login_start))
        .route("/login/finish", post(login_finish))
        .route("/recovery/start", post(recovery_start))
        .route("/recovery/finish", post(recovery_finish));

    // Protected routes requiring authentication
    let protected_routes = Router::new()
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))

        // Authenticated user's lifetime LLM token totals (#174). user_id is
        // derived from the auth Extension, never the client → /api/user/token-stats.
        .route("/user/token-stats", get(usage::token_stats))

        .route("/account/export", get(account::export_account))
        .route("/account/delete-challenge", post(account::delete_challenge))
        .route("/account", delete(account::delete_account))

        // Retirement balance sheet (#464). Deliberately NOT nested under
        // /budgets/:id — assets are scoped to the caller's own user_id, and a
        // budget-param route would let a budget collaborator reach them.
        .route("/assets", get(assets::list_assets))

        .route("/budgets", post(create_budget).get(list_budgets))
        .route("/budgets/:id", get(get_budget).put(update_budget).delete(delete_budget))
        .route("/budgets/:id/default", post(set_default_budget))
        .route("/budgets/:id/activate", post(set_active_budget))
        .route("/budgets/:id/close", post(close_budget))
        .route("/budgets/:id/archive", post(archive_budget))
        .route("/budgets/:id/unarchive", post(unarchive_budget))
        .route("/budgets/:id/rollup", post(link_rollup))
        .route("/budgets/:id/rollup/:child_id", delete(unlink_rollup))
        
        .route("/budgets/:id/categories", post(create_category).get(list_categories))
        // Deprecated (#426): superseded by /categories-view below. Retained one
        // release so a stale cached PWA client does not 404 into an error state.
        // The HTML BUILDER it uses is NOT deprecated - rag.rs still renders it
        // into chat bubbles for LIST_CATEGORIES and CATEGORY_BALANCE.
        .route("/budgets/:id/categories-table", get(categories_table))
        .route("/budgets/:id/categories-view", get(categories_view))
        .route("/budgets/:id/categories/:category_id", put(update_category).delete(delete_category))

        .route("/budgets/:id/transactions", post(create_transaction).get(list_transactions))
        .route("/budgets/:id/transactions/finalize", post(finalize_transaction))
        .route("/budgets/:id/transactions/:transaction_id", put(update_transaction).delete(delete_transaction))
        .route("/budgets/:id/transactions/:transaction_id/approve", post(approve_transaction))
        .route("/budgets/:id/transactions/:transaction_id/resolve-match", post(resolve_match))

        .route("/budgets/:id/ignore-rules", post(create_ignore_rule).get(list_ignore_rules))
        .route("/budgets/:id/ignore-rules/:rule_id", delete(delete_ignore_rule))

        .route("/budgets/:id/linked-accounts/session", post(start_link))
        .route("/budgets/:id/linked-accounts/complete", post(complete_link))
        .route("/budgets/:id/linked-accounts", get(bank_linking::list_linked_accounts_handler))
        .route("/budgets/:id/linked-accounts/:account_id/refresh", post(bank_linking::refresh_linked_account_handler))
        .route("/budgets/:id/linked-accounts/:account_id", delete(bank_linking::disconnect_linked_account_handler))
        .route("/budgets/:id/gocardless/session", post(bank_linking::gc_start_link_handler))
        .route("/budgets/:id/gocardless/complete", post(bank_linking::gc_complete_link_handler))
        .route("/gocardless/institutions", get(bank_linking::gc_institutions_handler))
        .route("/budgets/:id/belvo/session", post(bank_linking::belvo_start_link_handler))
        .route("/budgets/:id/belvo/complete", post(bank_linking::belvo_complete_link_handler))
        .route("/budgets/:id/basiq/session", post(bank_linking::basiq_start_link_handler))
        .route("/budgets/:id/basiq/complete", post(bank_linking::basiq_complete_link_handler))
        .route("/budgets/:id/akahu/session", post(bank_linking::akahu_start_link_handler))
        .route("/budgets/:id/akahu/complete", post(bank_linking::akahu_complete_link_handler))
        .route("/budgets/:id/plaid/link-token", post(bank_linking::plaid_link_token_handler))
        .route("/budgets/:id/plaid/complete", post(bank_linking::plaid_complete_link_handler))

        .route("/budgets/:id/report", get(reports::get_report_handler))

        // Account-level, owner-scoped cross-budget analytics (#57). Distinct from
        // the per-budget report above; aggregates over every budget the user owns.
        .route("/insights", get(insights::insights_handler))

        .route("/budgets/:id/goals", post(create_goal).get(list_goals))
        .route("/budgets/:id/goals/:goal_id", get(get_goal).put(update_goal).delete(delete_goal))
        .route("/budgets/:id/goals/:goal_id/contributions", post(create_contribution).get(list_contributions))

        .route("/budgets/:id/share", post(share_budget))
        .route("/budgets/:id/shares", get(list_shares))
        .route("/budgets/:id/shares/:share_id", delete(revoke_share))
        
        .route("/budgets/:id/logs", get(list_audit_logs))
        
        .route("/chat", post(chat_endpoint))
        .route("/chat/:budget_id/history", get(list_chat_history))

        .route("/conversations", get(list_conversations))
        .route("/conversations/:id", patch(rename_conversation).delete(delete_conversation))
        .route("/conversations/:id/messages", get(get_conversation_messages))
        .route("/suggested-question", get(suggested_question))

        // GitHub command-palette integration (#56). Kept in its own module and
        // registered distinctly to minimize collision with concurrent backend
        // work. Both endpoints are auth-gated (protected_routes) and call
        // GitHub server-side; the token is never exposed to the client.
        .route("/github/issues", get(github::list_issues).post(github::create_issue))

        .route("/notifications", get(list_notifications_handler))
        // Static segments registered before the `:id` param routes so they are
        // matched as literals (axum routing is order-independent here, but this
        // keeps the unread-count / mark-all endpoints visually grouped).
        .route("/notifications/unread_count", get(unread_count_handler))
        .route("/notifications/read_all", post(mark_all_read_handler))
        .route("/notifications/:id/read", post(mark_notification_read))
        .route("/notifications/:id", delete(dismiss_notification_handler))
        .route("/reminders", post(create_reminder_handler).get(list_reminders_handler))
        .route("/reminders/:id", delete(delete_reminder_handler))
        .route("/mcp", axum::routing::any(mcp::mcp_handler))
        .route("/billing/checkout", post(billing::checkout))
        .route("/billing/portal", post(billing::portal))
        .route("/billing/subscription", get(billing::get_subscription))
        .route("/retirement/profile", get(retirement::get_profile_handler).put(retirement::put_profile_handler))
        // User-scoped like the profile above — no `:budget_id`, because
        // retirement data belongs to a person, not a budget. The Pro gate lives
        // in the handlers via `require_caller_tier` (the caller's own tier),
        // matching the `PUT /retirement/profile` pattern. The POST is a
        // what-if with overrides and persists nothing.
        .route("/retirement/projection", get(retirement::get_projection_handler).post(retirement::post_projection_handler))

        // Retirement asset linking — Plaid Investments (nels#468). User-scoped
        // with NO `:budget_id`, matching /assets: retirement data belongs to a
        // person, not a budget (§20). Pro-gated inside each handler via
        // `require_caller_tier` (the caller's own tier). Disconnect is the one
        // ungated action (§14 precedent — a lapsed user must still be able to
        // remove their own linked data).
        .route(
            "/investments/link-token",
            post(plaid_investments::investments_link_token_handler),
        )
        .route(
            "/investments/complete",
            post(plaid_investments::investments_complete_handler),
        )
        .route(
            "/investments/classify",
            post(plaid_investments::investments_classify_handler),
        )
        .route(
            "/investments/accounts",
            get(plaid_investments::investments_list_handler),
        )
        .route(
            "/investments/accounts/:account_id/refresh",
            post(plaid_investments::investments_refresh_handler),
        )
        .route(
            "/investments/accounts/:account_id",
            delete(plaid_investments::investments_disconnect_handler),
        )
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // Admin-only routes (Admin PWA, #140). Layer order is bottom-up: the LAST
    // `.layer()` added runs OUTERMOST/FIRST, so `auth_middleware` is added last
    // to run before `admin_middleware` — it must insert `user_id` into the
    // request extensions before `admin_middleware` reads it to check is_admin.
    let admin_routes = Router::new()
        .route("/admin/users", get(admin::list_users))
        .route("/admin/users/:id/reset-credentials", post(admin::reset_credentials))
        .layer(middleware::from_fn_with_state(state.clone(), admin::admin_middleware))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // Stripe webhook (#25). PUBLIC — authenticated by Stripe signature, NOT a
    // session — so it is mounted OUTSIDE the auth-protected nest.
    let billing_public_routes = Router::new()
        .route("/billing/webhook", post(billing::webhook));

    // Financial Connections webhook (#303). PUBLIC — authenticated by Stripe
    // signature, NOT a session — mounted OUTSIDE the auth nest, same as
    // billing's webhook.
    let financial_connections_public_routes = Router::new()
        .route("/financial-connections/webhook", post(financial_connections::webhook));

    // Belvo webhook (#322). PUBLIC — authenticated by Basic-auth header
    // (spec Assumption 7), NOT a session — mounted OUTSIDE the auth nest,
    // same as the other two providers' webhook/no-webhook precedent.
    let belvo_public_routes = Router::new()
        .route("/belvo/webhook", post(belvo::webhook));

    // Basiq/Akahu/Plaid webhooks (#323, #321). PUBLIC — Basiq/Akahu
    // authenticated by HMAC signature, Plaid by a JWT/JWK (Plaid-Verification
    // header) — none of them session-based — all mounted OUTSIDE the auth
    // nest, same as Financial Connections's webhook.
    let bank_webhook_public_routes = Router::new()
        .route("/basiq/webhook", post(basiq::webhook))
        .route("/akahu/webhook", post(akahu::webhook))
        .route("/plaid/webhook", post(plaid::webhook));

    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .nest("/api/auth", auth_routes)
        .nest("/api", protected_routes)
        .nest("/api", admin_routes)
        .nest("/api", billing_public_routes)
        .nest("/api", financial_connections_public_routes)
        .nest("/api", belvo_public_routes)
        .nest("/api", bank_webhook_public_routes)
        .with_state(state)
        .layer(cors);

    // 8. Launch Server
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Server starting at http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// CORS origin policy decided from environment inputs.
///
/// Extracted as a pure function so the fail-closed behavior is unit-testable;
/// the router setup in `main` reads real env vars and cannot be tested directly.
#[derive(Debug, PartialEq, Eq)]
enum CorsPolicy {
    /// Restrict to an explicit comma-separated allowlist of origins.
    Allowlist(Vec<String>),
    /// Allow any origin (local-dev convenience).
    AnyOrigin,
    /// Deny all cross-origin requests (fail-closed production default).
    DenyAll,
}

/// Decide the CORS origin policy from the `CORS_ALLOWED_ORIGINS` value and
/// whether we are running in production.
///
/// - Set & non-empty -> restrict to that comma-separated allowlist.
/// - Unset/blank in production -> fail closed (`DenyAll`).
/// - Unset/blank in local dev -> allow any origin.
fn decide_cors_policy(allowed_origins: Option<&str>, is_production: bool) -> CorsPolicy {
    match allowed_origins.map(str::trim) {
        Some(raw) if !raw.is_empty() => {
            let origins = raw
                .split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_string)
                .collect();
            CorsPolicy::Allowlist(origins)
        }
        _ if is_production => CorsPolicy::DenyAll,
        _ => CorsPolicy::AnyOrigin,
    }
}

#[cfg(test)]
mod cors_tests {
    use super::*;

    #[test]
    fn allowlist_when_origins_set() {
        assert_eq!(
            decide_cors_policy(Some("https://a.example, https://b.example"), true),
            CorsPolicy::Allowlist(vec![
                "https://a.example".to_string(),
                "https://b.example".to_string(),
            ])
        );
    }

    #[test]
    fn deny_all_when_unset_in_production() {
        assert_eq!(decide_cors_policy(None, true), CorsPolicy::DenyAll);
    }

    #[test]
    fn deny_all_when_blank_in_production() {
        assert_eq!(decide_cors_policy(Some("   "), true), CorsPolicy::DenyAll);
    }

    #[test]
    fn any_origin_when_unset_in_local_dev() {
        assert_eq!(decide_cors_policy(None, false), CorsPolicy::AnyOrigin);
    }

    #[test]
    fn allowlist_takes_precedence_in_local_dev() {
        assert_eq!(
            decide_cors_policy(Some("https://a.example"), false),
            CorsPolicy::Allowlist(vec!["https://a.example".to_string()])
        );
    }
}
