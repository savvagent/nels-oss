//! Per-user model provider choice and bring-your-own-key (nels-oss#3).
//! User-scoped routes, no budget in scope. The stored key is never returned.

use axum::{extract::State, http::StatusCode, Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::internal_error;
use crate::llm::{self, LlmError, Provider};

const MAX_KEY_LEN: usize = 512;
const VALIDATIONS_PER_HOUR: i64 = 10;

pub fn enabled_byo_providers() -> Vec<Provider> {
    match std::env::var("ENABLED_BYO_PROVIDERS") {
        Ok(v) if !v.trim().is_empty() => v.split(',').filter_map(Provider::parse).collect(),
        _ => Provider::ALL.to_vec(),
    }
}

pub fn key_last4(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

#[derive(Serialize, Debug)]
pub struct AiProviderView {
    pub mode: &'static str, // "nels" | "byo" | "offline"
    pub provider: Option<String>,
    pub model: String,
    pub key_last4: Option<String>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub embeddings_enabled: bool,
    pub available_providers: Vec<String>,
}

#[derive(Deserialize)]
pub struct PutAiProvider {
    pub provider: String,
    pub api_key: String,
}

#[derive(Serialize, Debug)]
pub struct AiProviderExport {
    pub provider: String,
    pub model: String,
    pub key_last4: String,
    pub last_verified_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn load_view(db: &sqlx::PgPool, user_id: Uuid) -> Result<AiProviderView, (StatusCode, String)> {
    let available = enabled_byo_providers().iter().map(|p| p.as_str().to_string()).collect();
    let row = sqlx::query("SELECT provider, key_last4, last_verified_at, last_error FROM user_ai_providers WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(internal_error)?;
    Ok(match row {
        Some(r) => {
            let p_str: String = r.get("provider");
            let p = Provider::parse(&p_str);
            AiProviderView {
                mode: "byo",
                model: p.map(|p| p.model()).unwrap_or_default(),
                embeddings_enabled: p.map(|p| p.can_embed()).unwrap_or(false),
                provider: Some(p_str),
                key_last4: Some(r.get("key_last4")),
                last_verified_at: Some(r.get("last_verified_at")),
                last_error: r.get("last_error"),
                available_providers: available,
            }
        }
        None => {
            let nels = !llm::nels_gemini_key().is_empty();
            AiProviderView {
                mode: if nels { "nels" } else { "offline" },
                provider: nels.then(|| Provider::Gemini.as_str().to_string()),
                model: if nels { Provider::Gemini.model() } else { String::new() },
                key_last4: None,
                last_verified_at: None,
                last_error: None,
                embeddings_enabled: nels,
                available_providers: available,
            }
        }
    })
}

pub async fn get_ai_provider(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<AiProviderView>, (StatusCode, String)> {
    Ok(Json(load_view(&state.db, user_id).await?))
}

pub async fn put_ai_provider(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(body): Json<PutAiProvider>,
) -> Result<Json<AiProviderView>, (StatusCode, String)> {
    let key = body.api_key.trim().to_string();
    if key.is_empty() || key.chars().count() > MAX_KEY_LEN {
        return Err((StatusCode::BAD_REQUEST, "Enter an API key (up to 512 characters).".into()));
    }
    let provider = Provider::parse(&body.provider)
        .filter(|p| enabled_byo_providers().contains(p))
        .ok_or((StatusCode::BAD_REQUEST, "That AI provider isn't available.".to_string()))?;

    // Rate limit (fails closed on a DB error), then record this attempt. The
    // count and the insert run in one transaction under a per-user advisory
    // lock, so two concurrent PUTs cannot both pass the check. The transaction
    // commits BEFORE the outbound validation call, so the lock is never held
    // across network I/O.
    let mut tx = state.db.begin().await.map_err(internal_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ai_key_validation:' || $1::text))")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    let recent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ai_key_validation_attempts WHERE user_id = $1 AND created_at >= NOW() - INTERVAL '1 hour'",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal_error)?;
    if recent >= VALIDATIONS_PER_HOUR {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Too many key checks. Try again in an hour.".into()));
    }
    sqlx::query("INSERT INTO ai_key_validation_attempts (id, user_id) VALUES ($1, $2)")
        .bind(Uuid::new_v4())
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;

    match llm::validate_key(provider, &key).await {
        Ok(()) => {}
        Err(LlmError::Auth) => {
            return Err((StatusCode::BAD_REQUEST, format!("{} rejected that key.", provider.display_name())))
        }
        Err(LlmError::RateLimited) => {
            return Err((StatusCode::BAD_REQUEST, format!("{} says that key is out of quota or rate-limited.", provider.display_name())))
        }
        Err(_) => {
            return Err((StatusCode::BAD_GATEWAY, format!("Couldn't reach {}. Please try again.", provider.display_name())))
        }
    }

    let blob = state.cipher.encrypt(&key).map_err(internal_error)?;
    sqlx::query(
        "INSERT INTO user_ai_providers (user_id, provider, encrypted_key, key_last4, last_verified_at, last_error) \
         VALUES ($1, $2, $3, $4, NOW(), NULL) \
         ON CONFLICT (user_id) DO UPDATE SET provider = EXCLUDED.provider, encrypted_key = EXCLUDED.encrypted_key, \
           key_last4 = EXCLUDED.key_last4, last_verified_at = NOW(), last_error = NULL, updated_at = NOW()",
    )
    .bind(user_id)
    .bind(provider.as_str())
    .bind(&blob)
    .bind(key_last4(&key))
    .execute(&state.db)
    .await
    .map_err(internal_error)?;
    crate::budget::log_user_audit(&state.db, user_id, "SET_AI_PROVIDER", &format!("provider={}", provider.as_str())).await;
    Ok(Json(load_view(&state.db, user_id).await?))
}

pub async fn delete_ai_provider(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let deleted = sqlx::query("DELETE FROM user_ai_providers WHERE user_id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?
        .rows_affected();
    if deleted > 0 {
        crate::budget::log_user_audit(&state.db, user_id, "REMOVE_AI_PROVIDER", "").await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Data export (spec A15): provider, the model currently configured for it,
/// last4 and timestamps. Never the ciphertext.
pub async fn export_for(db: &sqlx::PgPool, user_id: Uuid) -> Result<Option<AiProviderExport>, sqlx::Error> {
    let row = sqlx::query("SELECT provider, key_last4, last_verified_at, created_at, updated_at FROM user_ai_providers WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| {
        let provider: String = r.get("provider");
        AiProviderExport {
            model: Provider::parse(&provider).map(|p| p.model()).unwrap_or_default(),
            provider,
            key_last4: r.get("key_last4"),
            last_verified_at: r.get("last_verified_at"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AppState;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn last4_handles_short_and_unicode_keys() {
        assert_eq!(key_last4("sk-abcdef1234"), "1234");
        assert_eq!(key_last4("ab"), "ab");
        assert_eq!(key_last4("ключ-éé"), "ч-éé");
    }

    #[test]
    fn enabled_providers_env_parsing() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ENABLED_BYO_PROVIDERS").ok();
        std::env::remove_var("ENABLED_BYO_PROVIDERS");
        assert_eq!(enabled_byo_providers().len(), 3);
        std::env::set_var("ENABLED_BYO_PROVIDERS", " openai , bogus,GEMINI ");
        assert_eq!(enabled_byo_providers(), vec![Provider::OpenAi, Provider::Gemini]);
        match prev { Some(v) => std::env::set_var("ENABLED_BYO_PROVIDERS", v), None => std::env::remove_var("ENABLED_BYO_PROVIDERS") }
    }

    async fn state() -> AppState {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into());
        let db = sqlx::postgres::PgPoolOptions::new().max_connections(3).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        AppState { db, cipher: Arc::new(crate::auth::test_cipher()),
                   webauthn: Arc::new(crate::passkeys::WebauthnRegistry::for_test()) }
    }
    async fn mk_user(db: &sqlx::PgPool) -> Uuid {
        let uid = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1,$2)").bind(uid).bind(format!("aip-{uid}@t.example")).execute(db).await.unwrap();
        uid
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn put_get_delete_round_trip_encrypts_and_never_returns_key() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let prev = std::env::var("OPENAI_API_BASE").ok();
        std::env::set_var("OPENAI_API_BASE", server.uri());
        Mock::given(method("GET")).and(path("/v1/models")).and(header("authorization", "Bearer sk-live-9876"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/v1/models")).and(header("authorization", "Bearer sk-bad"))
            .respond_with(ResponseTemplate::new(401)).mount(&server).await;
        let st = state().await;
        let uid = mk_user(&st.db).await;

        let bad = put_ai_provider(State(st.clone()), Extension(uid),
            Json(PutAiProvider { provider: "openai".into(), api_key: "sk-bad".into() })).await.unwrap_err();
        assert_eq!(bad.0, StatusCode::BAD_REQUEST);

        let view = put_ai_provider(State(st.clone()), Extension(uid),
            Json(PutAiProvider { provider: "openai".into(), api_key: "  sk-live-9876 ".into() })).await.unwrap().0;
        assert_eq!(view.mode, "byo");
        assert_eq!(view.key_last4.as_deref(), Some("9876"));
        assert!(!view.embeddings_enabled);
        let body = serde_json::to_string(&view).unwrap();
        assert!(!body.contains("sk-live"), "{body}");

        let blob: String = sqlx::query_scalar("SELECT encrypted_key FROM user_ai_providers WHERE user_id=$1").bind(uid).fetch_one(&st.db).await.unwrap();
        assert!(blob.starts_with("encv1:") && !blob.contains("sk-live"));
        let audits: Vec<String> = sqlx::query_scalar("SELECT details FROM audit_logs WHERE user_id=$1").bind(uid).fetch_all(&st.db).await.unwrap();
        assert!(audits.iter().all(|d| !d.contains("sk-live")));

        let got = get_ai_provider(State(st.clone()), Extension(uid)).await.unwrap().0;
        assert_eq!(got.provider.as_deref(), Some("openai"));

        assert_eq!(delete_ai_provider(State(st.clone()), Extension(uid)).await.unwrap(), StatusCode::NO_CONTENT);
        assert_eq!(delete_ai_provider(State(st.clone()), Extension(uid)).await.unwrap(), StatusCode::NO_CONTENT);
        let removes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE user_id=$1 AND action='REMOVE_AI_PROVIDER'").bind(uid).fetch_one(&st.db).await.unwrap();
        assert_eq!(removes, 1, "second DELETE removed nothing and must not audit");
        let got = get_ai_provider(State(st.clone()), Extension(uid)).await.unwrap().0;
        assert_ne!(got.mode, "byo");
        match prev { Some(v) => std::env::set_var("OPENAI_API_BASE", v), None => std::env::remove_var("OPENAI_API_BASE") }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn put_validation_and_rate_limit() {
        let st = state().await;
        let uid = mk_user(&st.db).await;
        for (p, k) in [("openai", ""), ("openai", &"x".repeat(513)), ("mistral", "k")] {
            let e = put_ai_provider(State(st.clone()), Extension(uid),
                Json(PutAiProvider { provider: p.into(), api_key: k.to_string() })).await.unwrap_err();
            assert_eq!(e.0, StatusCode::BAD_REQUEST, "{p}");
        }
        for _ in 0..10 {
            sqlx::query("INSERT INTO ai_key_validation_attempts (id, user_id) VALUES ($1,$2)").bind(Uuid::new_v4()).bind(uid).execute(&st.db).await.unwrap();
        }
        let e = put_ai_provider(State(st.clone()), Extension(uid),
            Json(PutAiProvider { provider: "openai".into(), api_key: "sk-x".into() })).await.unwrap_err();
        assert_eq!(e.0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn put_rate_limit_is_atomic_under_concurrency() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let prev = std::env::var("OPENAI_API_BASE").ok();
        std::env::set_var("OPENAI_API_BASE", server.uri());
        Mock::given(method("GET")).and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401)).mount(&server).await;
        let st = state().await;
        let uid = mk_user(&st.db).await;
        for _ in 0..9 {
            sqlx::query("INSERT INTO ai_key_validation_attempts (id, user_id) VALUES ($1,$2)").bind(Uuid::new_v4()).bind(uid).execute(&st.db).await.unwrap();
        }
        let put = || put_ai_provider(State(st.clone()), Extension(uid),
            Json(PutAiProvider { provider: "openai".into(), api_key: "sk-bad".into() }));
        let (a, b, c, d, e) = tokio::join!(put(), put(), put(), put(), put());
        let codes: Vec<StatusCode> = [a, b, c, d, e].into_iter().map(|r| r.unwrap_err().0).collect();
        let limited = codes.iter().filter(|c| **c == StatusCode::TOO_MANY_REQUESTS).count();
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_key_validation_attempts WHERE user_id=$1")
            .bind(uid).fetch_one(&st.db).await.unwrap();
        match prev { Some(v) => std::env::set_var("OPENAI_API_BASE", v), None => std::env::remove_var("OPENAI_API_BASE") }
        assert_eq!(total, 10, "exactly one request may pass the limiter: {codes:?}");
        assert!(limited >= 4, "{codes:?}");
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn export_has_no_key_and_account_deletion_cascades() {
        let st = state().await;
        let uid = mk_user(&st.db).await;
        sqlx::query("INSERT INTO user_ai_providers (user_id, provider, encrypted_key, key_last4, last_verified_at) VALUES ($1,'gemini',$2,'abcd',NOW())")
            .bind(uid).bind(st.cipher.encrypt("AIza-secret-abcd").unwrap()).execute(&st.db).await.unwrap();
        let export = crate::account::fetch_account_export(&st.db, uid).await.unwrap();
        let json = serde_json::to_string(&export).unwrap();
        assert!(json.contains("\"ai_provider\""));
        assert!(!json.contains("AIza-secret") && !json.contains("encv1:"));
        sqlx::query("DELETE FROM users WHERE id=$1").bind(uid).execute(&st.db).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_ai_providers WHERE user_id=$1").bind(uid).fetch_one(&st.db).await.unwrap();
        assert_eq!(n, 0);
    }
}
