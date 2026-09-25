//! Model-provider layer (nels-oss#3). Every LLM and embedding HTTP call in the
//! backend goes through this module. See AGENTS.md "Model providers & BYO-key".
//!
//! Invariants:
//! - A key is only ever sent in a request header, never in a URL, so a logged
//!   `reqwest::Error` can't leak it (and we still log errors via `without_url()`).
//! - `LlmCredentials`'s Debug output redacts the key.

use serde::Deserialize;
use std::fmt;
use std::time::Duration;

pub const EMBEDDING_MODEL: &str = "gemini-embedding-001";
pub const EMBEDDING_DIM: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Gemini,
    OpenAi,
    Anthropic,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenAi => "openai",
            Provider::Anthropic => "anthropic",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gemini" => Some(Provider::Gemini),
            "openai" => Some(Provider::OpenAi),
            "anthropic" => Some(Provider::Anthropic),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Provider::Gemini => "Google Gemini",
            Provider::OpenAi => "OpenAI",
            Provider::Anthropic => "Anthropic",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini-2.5-flash",
            Provider::OpenAi => "gpt-4.1-mini",
            Provider::Anthropic => "claude-haiku-4-5",
        }
    }

    fn model_env_var(self) -> &'static str {
        match self {
            Provider::Gemini => "LLM_MODEL_GEMINI",
            Provider::OpenAi => "LLM_MODEL_OPENAI",
            Provider::Anthropic => "LLM_MODEL_ANTHROPIC",
        }
    }

    /// The model this provider's calls use: the server-wide env override, or the
    /// pinned default. Not stored per user (spec A4).
    pub fn model(self) -> String {
        std::env::var(self.model_env_var())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.default_model().to_string())
    }

    /// Base URL, overridable for tests. Trimmed; a trailing slash is stripped;
    /// blank means the default.
    pub fn api_base(self) -> String {
        let (var, default) = match self {
            Provider::Gemini => ("GEMINI_API_BASE", "https://generativelanguage.googleapis.com"),
            Provider::OpenAi => ("OPENAI_API_BASE", "https://api.openai.com"),
            Provider::Anthropic => ("ANTHROPIC_API_BASE", "https://api.anthropic.com"),
        };
        std::env::var(var)
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// Only Gemini's vectors share a space with the existing `vector(768)`
    /// columns (spec A6).
    pub fn can_embed(self) -> bool {
        matches!(self, Provider::Gemini)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Nels,
    Byo,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Nels => "nels",
            KeySource::Byo => "byo",
        }
    }
}

#[derive(Clone)]
pub struct LlmCredentials {
    pub provider: Provider,
    pub model: String,
    pub source: KeySource,
    api_key: String,
}

impl LlmCredentials {
    pub fn new(provider: Provider, api_key: String, source: KeySource) -> Self {
        Self { provider, model: provider.model(), source, api_key }
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }
}

impl fmt::Debug for LlmCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmCredentials")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("source", &self.source)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum Resolved {
    Llm(LlmCredentials),
    /// Nels-hosted with no GEMINI_API_KEY: the existing offline router. No data
    /// leaves the server.
    Offline,
}

impl Resolved {
    pub fn creds(&self) -> Option<&LlmCredentials> {
        match self {
            Resolved::Llm(c) => Some(c),
            Resolved::Offline => None,
        }
    }

    /// Credentials usable for embeddings, or None when this user's provider
    /// can't embed into the shared 768-d Gemini space (spec A6).
    pub fn embedder(&self) -> Option<&LlmCredentials> {
        self.creds().filter(|c| c.provider.can_embed())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: i32,
    pub output: i32,
    pub thinking: i32,
    pub total: i32,
}

#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub text: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmError {
    Auth,
    RateLimited,
    Upstream(u16),
    Timeout,
    Transport,
    BadResponse,
    /// A BYO row exists but is unusable (the key won't decrypt, or the stored
    /// provider is unrecognized). The user must re-enter their key.
    KeyUnavailable,
    /// The `user_ai_providers` lookup itself failed, so we can't tell whether
    /// the user is BYO. Never falls back to Nels's key (spec A8).
    ConfigUnavailable,
}

impl LlmError {
    pub fn from_status(status: u16) -> Self {
        match status {
            401 | 403 => LlmError::Auth,
            429 => LlmError::RateLimited,
            s => LlmError::Upstream(s),
        }
    }

    /// Stable code for `ChatResponse.ai_provider_error` and `last_error`.
    pub fn code(self) -> &'static str {
        match self {
            LlmError::Auth => "auth_rejected",
            LlmError::RateLimited => "rate_limited",
            LlmError::Upstream(_)
            | LlmError::Timeout
            | LlmError::Transport
            | LlmError::BadResponse
            | LlmError::ConfigUnavailable => "unavailable",
            LlmError::KeyUnavailable => "key_unavailable",
        }
    }
}

/// Nels's own Gemini key (empty when unset). The only place outside tests that
/// reads GEMINI_API_KEY.
pub(crate) fn nels_gemini_key() -> String {
    std::env::var("GEMINI_API_KEY").unwrap_or_default().trim().to_string()
}

fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder().timeout(timeout).build().unwrap_or_else(|e| {
        tracing::error!("llm client build failed, using an unbounded client: {}", e);
        reqwest::Client::new()
    })
}

fn map_send_error(provider: Provider, e: reqwest::Error) -> LlmError {
    let timeout = e.is_timeout();
    tracing::error!(provider = provider.as_str(), "llm request failed: {}", e.without_url());
    if timeout { LlmError::Timeout } else { LlmError::Transport }
}

/// Map a non-2xx response to an error. Gemini reports an invalid key as 400 with
/// reason API_KEY_INVALID rather than 401, so that case is special-cased.
async fn map_status_error(provider: Provider, res: reqwest::Response) -> LlmError {
    let status = res.status().as_u16();
    let body = res.text().await.unwrap_or_default();
    tracing::error!(provider = provider.as_str(), status, "llm call returned non-2xx");
    if provider == Provider::Gemini && status == 400 && body.contains("API_KEY_INVALID") {
        return LlmError::Auth;
    }
    // Anthropic reports an unfunded account as 400, not 402/429. Treat it as a
    // quota problem so the user is told to check their account, not "status 400".
    if provider == Provider::Anthropic && status == 400 && body.contains("credit balance") {
        return LlmError::RateLimited;
    }
    LlmError::from_status(status)
}

// ---------- Gemini ----------

#[derive(Deserialize)]
struct GemUsage {
    #[serde(rename = "promptTokenCount")]
    prompt: Option<i32>,
    #[serde(rename = "candidatesTokenCount")]
    candidates: Option<i32>,
    #[serde(rename = "thoughtsTokenCount")]
    thoughts: Option<i32>,
    #[serde(rename = "totalTokenCount")]
    total: Option<i32>,
}

impl GemUsage {
    fn to_usage(u: Option<GemUsage>) -> TokenUsage {
        u.map(|u| TokenUsage {
            input: u.prompt.unwrap_or(0),
            output: u.candidates.unwrap_or(0),
            thinking: u.thoughts.unwrap_or(0),
            total: u.total.unwrap_or(0),
        })
        .unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct GemPart {
    text: Option<String>,
}
#[derive(Deserialize)]
struct GemContent {
    #[serde(default)]
    parts: Vec<GemPart>,
}
#[derive(Deserialize)]
struct GemCandidate {
    content: Option<GemContent>,
}
#[derive(Deserialize)]
struct GemResponse {
    #[serde(default)]
    candidates: Vec<GemCandidate>,
    #[serde(rename = "usageMetadata")]
    usage: Option<GemUsage>,
}

async fn gemini_generate(
    creds: &LlmCredentials,
    parts: Vec<&str>,
    mime: &str,
    timeout: Duration,
) -> Result<LlmOutput, LlmError> {
    let url = format!(
        "{}/v1beta/models/{}:generateContent",
        Provider::Gemini.api_base(),
        creds.model
    );
    let body = serde_json::json!({
        "contents": [{ "parts": parts.iter().map(|t| serde_json::json!({"text": t})).collect::<Vec<_>>() }],
        "generationConfig": { "responseMimeType": mime }
    });
    let res = client(timeout)
        .post(&url)
        .header("x-goog-api-key", creds.api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| map_send_error(Provider::Gemini, e))?;
    if !res.status().is_success() {
        return Err(map_status_error(Provider::Gemini, res).await);
    }
    let parsed: GemResponse = res.json().await.map_err(|e| {
        tracing::error!(provider = "gemini", "llm response parse failed: {}", e.without_url());
        LlmError::BadResponse
    })?;
    let usage = GemUsage::to_usage(parsed.usage);
    let text = parsed
        .candidates
        .into_iter()
        .next()
        .and_then(|c| c.content)
        .and_then(|c| c.parts.into_iter().next())
        .and_then(|p| p.text)
        .ok_or_else(|| {
            tracing::error!(provider = "gemini", "llm response parse failed: no candidate text");
            LlmError::BadResponse
        })?;
    Ok(LlmOutput { text, usage })
}

#[derive(Deserialize)]
struct GemEmbedValues {
    values: Vec<f32>,
}
#[derive(Deserialize)]
struct GemEmbedResponse {
    embedding: GemEmbedValues,
    #[serde(rename = "usageMetadata")]
    usage: Option<GemUsage>,
}

async fn gemini_embed(creds: &LlmCredentials, text: &str) -> Result<(Vec<f32>, TokenUsage), LlmError> {
    let url = format!(
        "{}/v1beta/models/{}:embedContent",
        Provider::Gemini.api_base(),
        EMBEDDING_MODEL
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "outputDimensionality": EMBEDDING_DIM
    });
    let res = client(Duration::from_secs(20))
        .post(&url)
        .header("x-goog-api-key", creds.api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| map_send_error(Provider::Gemini, e))?;
    if !res.status().is_success() {
        return Err(map_status_error(Provider::Gemini, res).await);
    }
    let parsed: GemEmbedResponse = res.json().await.map_err(|e| {
        tracing::error!(provider = "gemini", "llm response parse failed (embedding): {}", e.without_url());
        LlmError::BadResponse
    })?;
    Ok((parsed.embedding.values, GemUsage::to_usage(parsed.usage)))
}

// ---------- OpenAI ----------

#[derive(Deserialize)]
struct OaMsg {
    content: Option<String>,
}
#[derive(Deserialize)]
struct OaChoice {
    message: OaMsg,
}
#[derive(Deserialize)]
struct OaDetails {
    reasoning_tokens: Option<i32>,
}
#[derive(Deserialize)]
struct OaUsage {
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
    completion_tokens_details: Option<OaDetails>,
}
#[derive(Deserialize)]
struct OaResponse {
    #[serde(default)]
    choices: Vec<OaChoice>,
    usage: Option<OaUsage>,
}

async fn openai_chat(
    creds: &LlmCredentials,
    messages: serde_json::Value,
    json_mode: bool,
    timeout: Duration,
) -> Result<LlmOutput, LlmError> {
    let mut body = serde_json::json!({ "model": creds.model, "messages": messages });
    if json_mode {
        // Requires the word "JSON" in the messages; Nels's system prompt has it (rule 22).
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    let res = client(timeout)
        .post(format!("{}/v1/chat/completions", Provider::OpenAi.api_base()))
        .bearer_auth(creds.api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| map_send_error(Provider::OpenAi, e))?;
    if !res.status().is_success() {
        return Err(map_status_error(Provider::OpenAi, res).await);
    }
    let parsed: OaResponse = res.json().await.map_err(|e| {
        tracing::error!(provider = "openai", "llm response parse failed: {}", e.without_url());
        LlmError::BadResponse
    })?;
    let usage = parsed
        .usage
        .map(|u| TokenUsage {
            input: u.prompt_tokens.unwrap_or(0),
            output: u.completion_tokens.unwrap_or(0),
            thinking: u.completion_tokens_details.and_then(|d| d.reasoning_tokens).unwrap_or(0),
            total: u.total_tokens.unwrap_or(0),
        })
        .unwrap_or_default();
    let text = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| {
            tracing::error!(provider = "openai", "llm response parse failed: no message content");
            LlmError::BadResponse
        })?;
    Ok(LlmOutput { text, usage })
}

// ---------- Anthropic ----------

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Deserialize)]
struct AnBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct AnUsage {
    input_tokens: Option<i32>,
    output_tokens: Option<i32>,
}
#[derive(Deserialize)]
struct AnResponse {
    #[serde(default)]
    content: Vec<AnBlock>,
    usage: Option<AnUsage>,
}

async fn anthropic_messages(
    creds: &LlmCredentials,
    body: serde_json::Value,
    timeout: Duration,
) -> Result<AnResponse, LlmError> {
    let res = client(timeout)
        .post(format!("{}/v1/messages", Provider::Anthropic.api_base()))
        .header("x-api-key", creds.api_key())
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&body)
        .send()
        .await
        .map_err(|e| map_send_error(Provider::Anthropic, e))?;
    if !res.status().is_success() {
        return Err(map_status_error(Provider::Anthropic, res).await);
    }
    res.json().await.map_err(|e| {
        tracing::error!(provider = "anthropic", "llm response parse failed: {}", e.without_url());
        LlmError::BadResponse
    })
}

fn anthropic_usage(u: Option<AnUsage>) -> TokenUsage {
    u.map(|u| {
        let (i, o) = (u.input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0));
        TokenUsage { input: i, output: o, thinking: 0, total: i + o }
    })
    .unwrap_or_default()
}

async fn anthropic_json(creds: &LlmCredentials, system: &str, user: &str, timeout: Duration) -> Result<LlmOutput, LlmError> {
    let body = serde_json::json!({
        "model": creds.model,
        "max_tokens": 4096,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
        "tools": [{
            "name": "respond",
            "description": "Return your complete reply as the JSON object described in the system prompt.",
            "input_schema": { "type": "object" }
        }],
        "tool_choice": { "type": "tool", "name": "respond" }
    });
    let parsed = anthropic_messages(creds, body, timeout).await?;
    let usage = anthropic_usage(parsed.usage);
    let input = parsed
        .content
        .into_iter()
        .find(|b| b.kind == "tool_use" && b.name.as_deref() == Some("respond"))
        .and_then(|b| b.input)
        .filter(|v| v.is_object())
        .ok_or_else(|| {
            tracing::error!(provider = "anthropic", "llm response parse failed: no respond tool_use block");
            LlmError::BadResponse
        })?;
    let text = serde_json::to_string(&input).map_err(|e| {
        tracing::error!(provider = "anthropic", "llm response parse failed: tool input not serializable: {}", e);
        LlmError::BadResponse
    })?;
    Ok(LlmOutput { text, usage })
}

async fn anthropic_text(creds: &LlmCredentials, prompt: &str, timeout: Duration) -> Result<LlmOutput, LlmError> {
    let body = serde_json::json!({
        "model": creds.model,
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let parsed = anthropic_messages(creds, body, timeout).await?;
    let usage = anthropic_usage(parsed.usage);
    let text: String = parsed
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text)
        .collect();
    if text.is_empty() {
        tracing::error!(provider = "anthropic", "llm response parse failed: no text block");
        return Err(LlmError::BadResponse);
    }
    Ok(LlmOutput { text, usage })
}

/// Cheap, token-free liveness check of a user-supplied key: list models.
pub async fn validate_key(provider: Provider, key: &str) -> Result<(), LlmError> {
    let c = client(Duration::from_secs(10));
    let req = match provider {
        Provider::Gemini => c
            .get(format!("{}/v1beta/models", provider.api_base()))
            .header("x-goog-api-key", key),
        Provider::OpenAi => c.get(format!("{}/v1/models", provider.api_base())).bearer_auth(key),
        Provider::Anthropic => c
            .get(format!("{}/v1/models", provider.api_base()))
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION),
    };
    let res = req.send().await.map_err(|e| map_send_error(provider, e))?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(map_status_error(provider, res).await)
    }
}

// ---------- public entry points ----------

/// One structured-JSON completion. Returns the raw JSON text; the caller parses
/// it (rag.rs keeps its existing AiStructuredResponse + malformed fallback).
pub async fn generate_json(
    creds: &LlmCredentials,
    system: &str,
    user: &str,
    timeout: Duration,
) -> Result<LlmOutput, LlmError> {
    match creds.provider {
        Provider::Gemini => gemini_generate(creds, vec![system, user], "application/json", timeout).await,
        Provider::OpenAi => {
            let msgs = serde_json::json!([
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]);
            openai_chat(creds, msgs, true, timeout).await
        }
        Provider::Anthropic => anthropic_json(creds, system, user, timeout).await,
    }
}

/// One plain-text completion (title, suggested question, narrative).
pub async fn generate_text(
    creds: &LlmCredentials,
    prompt: &str,
    timeout: Duration,
) -> Result<LlmOutput, LlmError> {
    match creds.provider {
        Provider::Gemini => gemini_generate(creds, vec![prompt], "text/plain", timeout).await,
        Provider::OpenAi => {
            openai_chat(creds, serde_json::json!([{"role": "user", "content": prompt}]), false, timeout).await
        }
        Provider::Anthropic => anthropic_text(creds, prompt, timeout).await,
    }
}

/// `Ok(None)` when this provider can't embed (spec A6).
pub async fn embed(
    creds: &LlmCredentials,
    text: &str,
) -> Result<Option<(Vec<f32>, TokenUsage)>, LlmError> {
    if !creds.provider.can_embed() {
        return Ok(None);
    }
    gemini_embed(creds, text).await.map(Some)
}

// ---------- credential resolution & usage accounting ----------

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Pure decision table (spec §5.1). `row` is the lookup result: `Err(())` means
/// the SELECT failed, which must NEVER be treated as "no row" (spec A8); it is
/// `ConfigUnavailable`. A row that exists but can't be used is `KeyUnavailable`.
pub(crate) fn resolve_from(
    row: Result<Option<(String, String)>, ()>,
    cipher: &crate::crypto::SecretCipher,
    nels_key: &str,
) -> Result<Resolved, LlmError> {
    match row {
        Err(()) => Err(LlmError::ConfigUnavailable),
        Ok(None) if nels_key.is_empty() => Ok(Resolved::Offline),
        Ok(None) => Ok(Resolved::Llm(LlmCredentials::new(
            Provider::Gemini,
            nels_key.to_string(),
            KeySource::Nels,
        ))),
        Ok(Some((provider, blob))) => {
            let provider = Provider::parse(&provider).ok_or_else(|| {
                tracing::error!(provider = %provider, "stored BYO provider is unrecognized");
                LlmError::KeyUnavailable
            })?;
            let key = cipher.decrypt(&blob).map_err(|e| {
                tracing::error!(provider = provider.as_str(), "stored BYO key failed to decrypt: {}", e);
                LlmError::KeyUnavailable
            })?;
            Ok(Resolved::Llm(LlmCredentials::new(provider, key, KeySource::Byo)))
        }
    }
}

/// Resolve which credentials a request made on behalf of `user_id` must use.
/// Call once per request and pass `&Resolved` down (spec §5.1).
pub async fn resolve_for_user(
    db: &PgPool,
    cipher: &crate::crypto::SecretCipher,
    user_id: Uuid,
) -> Result<Resolved, LlmError> {
    let row = sqlx::query("SELECT provider, encrypted_key FROM user_ai_providers WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map(|o| o.map(|r| (r.get::<String, _>("provider"), r.get::<String, _>("encrypted_key"))))
        .map_err(|e| {
            tracing::error!(%user_id, "ai provider lookup failed: {}", e);
        });
    let resolved = resolve_from(row, cipher, &nels_gemini_key());
    // Only a broken BYO row is marked. A failed lookup (ConfigUnavailable)
    // says nothing about the row, and the DB is likely down anyway.
    if matches!(resolved, Err(LlmError::KeyUnavailable)) {
        mark_key_unavailable(db, user_id).await;
    }
    resolved
}

async fn mark_key_unavailable(db: &PgPool, user_id: Uuid) {
    let _ = sqlx::query(
        "UPDATE user_ai_providers SET last_error = 'key_unavailable', updated_at = NOW() \
         WHERE user_id = $1 AND last_error IS DISTINCT FROM 'key_unavailable'",
    )
    .bind(user_id)
    .execute(db)
    .await
    .inspect_err(|e| tracing::warn!(%user_id, "mark_key_unavailable failed: {e}"));
}

/// Fail-safe llm_usage write (replaces rag::record_llm_usage; AC #6 of #140).
pub async fn record_usage(
    db: &PgPool,
    user_id: Uuid,
    creds: &LlmCredentials,
    model_label: &str,
    call_type: &str,
    usage: TokenUsage,
) {
    let res = sqlx::query(
        "INSERT INTO llm_usage (id, user_id, model, call_type, input_tokens, output_tokens, thinking_tokens, total_tokens, provider, key_source) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(model_label)
    .bind(call_type)
    .bind(usage.input)
    .bind(usage.output)
    .bind(usage.thinking)
    .bind(usage.total)
    .bind(creds.provider.as_str())
    .bind(creds.source.as_str())
    .execute(db)
    .await;
    if let Err(e) = res {
        tracing::warn!("failed to record llm_usage (user={user_id}, call_type={call_type}): {e}");
    }
}

/// Keep `last_error` in step with runtime results for BYO keys only. An auth
/// failure sets it; a success clears it with a guarded UPDATE, so a healthy
/// call adds no write. Transient errors leave it untouched. Fail-safe.
pub async fn observe(db: &PgPool, user_id: Uuid, creds: &LlmCredentials, ok: bool, err: Option<LlmError>) {
    if creds.source != KeySource::Byo {
        return;
    }
    let sql = if ok {
        "UPDATE user_ai_providers SET last_error = NULL, updated_at = NOW() WHERE user_id = $1 AND last_error IS NOT NULL"
    } else if err == Some(LlmError::Auth) {
        "UPDATE user_ai_providers SET last_error = 'auth_rejected', updated_at = NOW() WHERE user_id = $1"
    } else {
        return;
    };
    if let Err(e) = sqlx::query(sql).bind(user_id).execute(db).await {
        tracing::warn!(%user_id, "observe last_error write failed: {e}");
    }
}

/// Label stored in llm_usage.model. Gemini keeps the historical "models/<id>" form.
fn model_label(creds: &LlmCredentials) -> String {
    match creds.provider {
        Provider::Gemini => format!("models/{}", creds.model),
        _ => creds.model.clone(),
    }
}

pub async fn generate_json_for(
    db: &PgPool, user_id: Uuid, creds: &LlmCredentials, call_type: &str,
    system: &str, user: &str, timeout: Duration,
) -> Result<String, LlmError> {
    let res = generate_json(creds, system, user, timeout).await;
    finish(db, user_id, creds, call_type, res).await
}

pub async fn generate_text_for(
    db: &PgPool, user_id: Uuid, creds: &LlmCredentials, call_type: &str,
    prompt: &str, timeout: Duration,
) -> Result<String, LlmError> {
    let res = generate_text(creds, prompt, timeout).await;
    finish(db, user_id, creds, call_type, res).await
}

async fn finish(
    db: &PgPool, user_id: Uuid, creds: &LlmCredentials, call_type: &str,
    res: Result<LlmOutput, LlmError>,
) -> Result<String, LlmError> {
    match res {
        Ok(out) => {
            record_usage(db, user_id, creds, &model_label(creds), call_type, out.usage).await;
            observe(db, user_id, creds, true, None).await;
            Ok(out.text)
        }
        Err(e) => {
            observe(db, user_id, creds, false, Some(e)).await;
            Err(e)
        }
    }
}

/// Best-effort embedding: None when the user's provider can't embed, when
/// resolved Offline, or on any failure. Never errors (callers store NULL).
pub async fn embed_for(db: &PgPool, user_id: Uuid, resolved: &Resolved, text: &str) -> Option<Vec<f32>> {
    let creds = resolved.embedder()?;
    match embed(creds, text).await {
        Ok(Some((v, usage))) => {
            record_usage(db, user_id, creds, &format!("models/{EMBEDDING_MODEL}"), "embedding", usage).await;
            observe(db, user_id, creds, true, None).await;
            Some(v)
        }
        Ok(None) => None,
        Err(e) => {
            observe(db, user_id, creds, false, Some(e)).await;
            None
        }
    }
}

pub async fn purge_old_key_validation_attempts(db: &PgPool) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM ai_key_validation_attempts WHERE created_at < NOW() - INTERVAL '1 hour'")
        .execute(db)
        .await
        .map(|r| r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn gem(key: &str) -> LlmCredentials {
        LlmCredentials::new(Provider::Gemini, key.to_string(), KeySource::Byo)
    }

    struct BaseGuard(&'static str, Option<String>);
    impl Drop for BaseGuard {
        fn drop(&mut self) {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }
    fn set_base(var: &'static str, url: &str) -> BaseGuard {
        let g = BaseGuard(var, std::env::var(var).ok());
        std::env::set_var(var, url);
        g
    }

    #[test]
    fn provider_round_trips_check_literals() {
        for p in [Provider::Gemini, Provider::OpenAi, Provider::Anthropic] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        assert_eq!(Provider::OpenAi.as_str(), "openai");
        assert_eq!(Provider::parse("OpenAI"), Some(Provider::OpenAi));
        assert_eq!(Provider::parse("mistral"), None);
        assert_eq!(KeySource::Nels.as_str(), "nels");
        assert_eq!(KeySource::Byo.as_str(), "byo");
    }

    #[test]
    fn only_gemini_embeds() {
        assert!(Provider::Gemini.can_embed());
        assert!(!Provider::OpenAi.can_embed());
        assert!(!Provider::Anthropic.can_embed());
        let r = Resolved::Llm(LlmCredentials::new(Provider::OpenAi, "k".into(), KeySource::Byo));
        assert!(r.embedder().is_none());
        assert!(Resolved::Offline.embedder().is_none());
        assert!(Resolved::Llm(gem("k")).embedder().is_some());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let c = gem("sk-SECRET-123456");
        let d = format!("{:?}", c);
        assert!(!d.contains("SECRET"), "{d}");
        assert!(d.contains("redacted"));
        let d2 = format!("{:?}", Resolved::Llm(c));
        assert!(!d2.contains("SECRET"));
    }

    #[test]
    fn model_env_override_trims_and_ignores_blank() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = set_base("LLM_MODEL_OPENAI", "  gpt-test  ");
        assert_eq!(Provider::OpenAi.model(), "gpt-test");
        std::env::set_var("LLM_MODEL_OPENAI", "   ");
        assert_eq!(Provider::OpenAi.model(), "gpt-4.1-mini");
    }

    #[test]
    fn status_mapping() {
        assert_eq!(LlmError::from_status(401), LlmError::Auth);
        assert_eq!(LlmError::from_status(403), LlmError::Auth);
        assert_eq!(LlmError::from_status(429), LlmError::RateLimited);
        assert_eq!(LlmError::from_status(500), LlmError::Upstream(500));
        assert_eq!(LlmError::Auth.code(), "auth_rejected");
        assert_eq!(LlmError::KeyUnavailable.code(), "key_unavailable");
        assert_eq!(LlmError::ConfigUnavailable.code(), "unavailable");
    }

    #[tokio::test]
    async fn gemini_json_uses_header_auth_and_parses_usage() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-2.5-flash:generateContent"))
            .and(header("x-goog-api-key", "user-key"))
            .and(body_partial_json(serde_json::json!({
                "contents": [{"parts": [{"text": "SYS"}, {"text": "USER"}]}],
                "generationConfig": {"responseMimeType": "application/json"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{"content": {"parts": [{"text": "{\"a\":1}"}]}}],
                "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3,
                                   "thoughtsTokenCount": 2, "totalTokenCount": 15}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = generate_json(&gem("user-key"), "SYS", "USER", std::time::Duration::from_secs(5))
            .await
            .expect("ok");
        assert_eq!(out.text, "{\"a\":1}");
        assert_eq!(out.usage, TokenUsage { input: 10, output: 3, thinking: 2, total: 15 });
        let reqs = server.received_requests().await.unwrap();
        assert!(reqs[0].url.query().is_none(), "key must not be in the query string");
    }

    #[tokio::test]
    async fn gemini_text_uses_plain_mime() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "contents": [{"parts": [{"text": "P"}]}],
                "generationConfig": {"responseMimeType": "text/plain"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{"content": {"parts": [{"text": "Hello"}]}}]
            })))
            .mount(&server)
            .await;
        let out = generate_text(&gem("k"), "P", std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.text, "Hello");
        assert_eq!(out.usage, TokenUsage::default());
    }

    #[tokio::test]
    async fn gemini_400_api_key_invalid_maps_to_auth() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"code": 400, "status": "INVALID_ARGUMENT",
                          "details": [{"reason": "API_KEY_INVALID"}]}
            })))
            .mount(&server)
            .await;
        let err = generate_text(&gem("bad"), "P", std::time::Duration::from_secs(5)).await.unwrap_err();
        assert_eq!(err, LlmError::Auth);
    }

    #[tokio::test]
    async fn gemini_plain_400_is_upstream_and_empty_candidates_is_bad_response() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST")).and(header("x-goog-api-key", "a"))
            .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
            .mount(&server).await;
        Mock::given(method("POST")).and(header("x-goog-api-key", "b"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"candidates": []})))
            .mount(&server).await;
        let t = std::time::Duration::from_secs(5);
        assert_eq!(generate_text(&gem("a"), "P", t).await.unwrap_err(), LlmError::Upstream(400));
        assert_eq!(generate_text(&gem("b"), "P", t).await.unwrap_err(), LlmError::BadResponse);
    }

    #[tokio::test]
    async fn gemini_embed_requests_768_dims() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .and(header("x-goog-api-key", "k"))
            .and(body_partial_json(serde_json::json!({"outputDimensionality": 768})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": {"values": [0.5, 0.25]}
            })))
            .mount(&server)
            .await;
        let (v, _u) = embed(&gem("k"), "hello").await.unwrap().expect("gemini embeds");
        assert_eq!(v, vec![0.5, 0.25]);
        let oa = LlmCredentials::new(Provider::OpenAi, "k".into(), KeySource::Byo);
        assert!(embed(&oa, "hello").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn timeout_maps_to_timeout() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("GEMINI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3)))
            .mount(&server)
            .await;
        let err = generate_text(&gem("k"), "P", std::time::Duration::from_millis(200)).await.unwrap_err();
        assert_eq!(err, LlmError::Timeout);
    }

    fn oa(key: &str) -> LlmCredentials {
        LlmCredentials::new(Provider::OpenAi, key.to_string(), KeySource::Byo)
    }
    fn an(key: &str) -> LlmCredentials {
        LlmCredentials::new(Provider::Anthropic, key.to_string(), KeySource::Byo)
    }

    #[tokio::test]
    async fn openai_json_mode_request_and_usage() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("OPENAI_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-u"))
            .and(body_partial_json(serde_json::json!({
                "model": "gpt-4.1-mini",
                "response_format": {"type": "json_object"},
                "messages": [{"role": "system", "content": "SYS JSON"}, {"role": "user", "content": "U"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "{\"action\":\"NONE\"}"}}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 4, "total_tokens": 11,
                          "completion_tokens_details": {"reasoning_tokens": 1}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = generate_json(&oa("sk-u"), "SYS JSON", "U", std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.text, "{\"action\":\"NONE\"}");
        assert_eq!(out.usage, TokenUsage { input: 7, output: 4, thinking: 1, total: 11 });
    }

    #[tokio::test]
    async fn openai_text_has_no_response_format_and_null_content_is_bad_response() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("OPENAI_API_BASE", &server.uri());
        Mock::given(method("POST")).and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "Title here"}}]
            })))
            .mount(&server).await;
        Mock::given(method("POST")).and(header("authorization", "Bearer null"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": null}}]
            })))
            .mount(&server).await;
        let t = std::time::Duration::from_secs(5);
        assert_eq!(generate_text(&oa("good"), "P", t).await.unwrap().text, "Title here");
        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(body.get("response_format").is_none());
        assert_eq!(generate_text(&oa("null"), "P", t).await.unwrap_err(), LlmError::BadResponse);
    }

    #[tokio::test]
    async fn anthropic_json_forces_respond_tool_and_serializes_input() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("ANTHROPIC_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "ak"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_partial_json(serde_json::json!({
                "model": "claude-haiku-4-5",
                "system": "SYS",
                "tool_choice": {"type": "tool", "name": "respond"},
                "messages": [{"role": "user", "content": "U"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "tool_use", "name": "respond",
                             "input": {"action": "NONE", "response_text": "hi", "thought": ""}}],
                "usage": {"input_tokens": 20, "output_tokens": 5}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = generate_json(&an("ak"), "SYS", "U", std::time::Duration::from_secs(5)).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["action"], "NONE");
        assert_eq!(out.usage, TokenUsage { input: 20, output: 5, thinking: 0, total: 25 });
    }

    // Anthropic reports an unfunded account as HTTP 400 ("credit balance is too
    // low"), and GET /v1/models still succeeds for it, so a user can save such a
    // key. Map it to RateLimited so chat says "out of quota" rather than a bare 400.
    #[tokio::test]
    async fn anthropic_400_credit_balance_maps_to_rate_limited() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("ANTHROPIC_API_BASE", &server.uri());
        Mock::given(method("POST")).and(header("x-api-key", "broke"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "type": "error",
                "error": {"type": "invalid_request_error",
                          "message": "Your credit balance is too low to access the Anthropic API."}
            })))
            .mount(&server).await;
        Mock::given(method("POST")).and(header("x-api-key", "other"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "type": "error", "error": {"type": "invalid_request_error", "message": "max_tokens: too large"}
            })))
            .mount(&server).await;
        let t = std::time::Duration::from_secs(5);
        assert_eq!(generate_text(&an("broke"), "P", t).await.unwrap_err(), LlmError::RateLimited);
        assert_eq!(generate_text(&an("other"), "P", t).await.unwrap_err(), LlmError::Upstream(400));
    }

    #[tokio::test]
    async fn anthropic_json_without_tool_use_is_bad_response() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("ANTHROPIC_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "I refuse"}]
            })))
            .mount(&server)
            .await;
        let err = generate_json(&an("ak"), "S", "U", std::time::Duration::from_secs(5)).await.unwrap_err();
        assert_eq!(err, LlmError::BadResponse);
    }

    #[tokio::test]
    async fn anthropic_text_joins_text_blocks() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g = set_base("ANTHROPIC_API_BASE", &server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "Grocery "}, {"type": "text", "text": "plan"}],
                "usage": {"input_tokens": 1, "output_tokens": 2}
            })))
            .mount(&server)
            .await;
        let out = generate_text(&an("ak"), "P", std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.text, "Grocery plan");
    }

    #[tokio::test]
    async fn validate_key_per_provider() {
        let _l = crate::rag::GEMINI_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = MockServer::start().await;
        let _g1 = set_base("GEMINI_API_BASE", &server.uri());
        let _g2 = set_base("OPENAI_API_BASE", &server.uri());
        let _g3 = set_base("ANTHROPIC_API_BASE", &server.uri());
        Mock::given(method("GET")).and(path("/v1beta/models")).and(header("x-goog-api-key", "g"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": []})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/v1/models")).and(header("authorization", "Bearer o"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/v1/models")).and(header("x-api-key", "a"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/v1/models")).and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server).await;
        assert_eq!(validate_key(Provider::Gemini, "g").await, Ok(()));
        assert_eq!(validate_key(Provider::OpenAi, "o").await, Ok(()));
        assert_eq!(validate_key(Provider::Anthropic, "a").await, Ok(()));
        assert_eq!(validate_key(Provider::OpenAi, "bad").await, Err(LlmError::Auth));
    }

    fn cipher() -> crate::crypto::SecretCipher {
        crate::auth::test_cipher()
    }

    #[test]
    fn resolve_from_decision_table() {
        let c = cipher();
        // no row + nels key -> Nels Gemini
        match resolve_from(Ok(None), &c, "nels-k").unwrap() {
            Resolved::Llm(cr) => {
                assert_eq!(cr.provider, Provider::Gemini);
                assert_eq!(cr.source, KeySource::Nels);
                assert_eq!(cr.api_key(), "nels-k");
            }
            Resolved::Offline => panic!("expected Llm"),
        }
        // no row + no nels key -> Offline
        assert!(matches!(resolve_from(Ok(None), &c, "").unwrap(), Resolved::Offline));
        // row decrypts -> BYO regardless of nels key
        let blob = c.encrypt("sk-user").unwrap();
        for nels in ["", "nels-k"] {
            match resolve_from(Ok(Some(("openai".into(), blob.clone()))), &c, nels).unwrap() {
                Resolved::Llm(cr) => {
                    assert_eq!(cr.provider, Provider::OpenAi);
                    assert_eq!(cr.source, KeySource::Byo);
                    assert_eq!(cr.api_key(), "sk-user");
                }
                Resolved::Offline => panic!("expected BYO"),
            }
        }
        // decrypt fails -> KeyUnavailable, never Nels
        assert_eq!(
            resolve_from(Ok(Some(("openai".into(), "encv1:garbage".into()))), &c, "nels-k").unwrap_err(),
            LlmError::KeyUnavailable
        );
        // unknown provider literal -> KeyUnavailable
        assert_eq!(
            resolve_from(Ok(Some(("mistral".into(), blob))), &c, "nels-k").unwrap_err(),
            LlmError::KeyUnavailable
        );
    }

    #[test]
    fn resolve_from_lookup_error_is_config_unavailable_never_nels() {
        for nels in ["", "nels-k"] {
            assert_eq!(resolve_from(Err(()), &cipher(), nels).unwrap_err(), LlmError::ConfigUnavailable);
        }
    }

    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".into()
        });
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn mk_user(db: &sqlx::PgPool) -> uuid::Uuid {
        let uid = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(uid).bind(format!("llm-{uid}@test.example"))
            .execute(db).await.unwrap();
        uid
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn resolve_for_user_reads_encrypted_row_and_usage_is_tagged() {
        let db = test_pool().await;
        let c = cipher();
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO user_ai_providers (user_id, provider, encrypted_key, key_last4, last_verified_at) VALUES ($1,'anthropic',$2,'wxyz',NOW())")
            .bind(uid).bind(c.encrypt("ak-wxyz").unwrap())
            .execute(&db).await.unwrap();
        let r = resolve_for_user(&db, &c, uid).await.unwrap();
        let creds = r.creds().unwrap().clone();
        assert_eq!(creds.provider, Provider::Anthropic);
        record_usage(&db, uid, &creds, &creds.model, "chat", TokenUsage { input: 1, output: 2, thinking: 0, total: 3 }).await;
        let (p, s): (String, String) = sqlx::query_as("SELECT provider, key_source FROM llm_usage WHERE user_id=$1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!((p.as_str(), s.as_str()), ("anthropic", "byo"));
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn observe_sets_and_clears_last_error_only_for_byo() {
        let db = test_pool().await;
        let c = cipher();
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO user_ai_providers (user_id, provider, encrypted_key, key_last4, last_verified_at) VALUES ($1,'openai',$2,'1234',NOW())")
            .bind(uid).bind(c.encrypt("sk-1234").unwrap())
            .execute(&db).await.unwrap();
        let byo = LlmCredentials::new(Provider::OpenAi, "sk-1234".into(), KeySource::Byo);
        observe(&db, uid, &byo, false, Some(LlmError::Auth)).await;
        let e: Option<String> = sqlx::query_scalar("SELECT last_error FROM user_ai_providers WHERE user_id=$1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(e.as_deref(), Some("auth_rejected"));
        observe(&db, uid, &byo, false, Some(LlmError::RateLimited)).await; // transient: unchanged
        observe(&db, uid, &byo, true, None).await;
        let e: Option<String> = sqlx::query_scalar("SELECT last_error FROM user_ai_providers WHERE user_id=$1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(e, None);
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn purge_removes_attempts_older_than_an_hour() {
        let db = test_pool().await;
        let uid = mk_user(&db).await;
        sqlx::query("INSERT INTO ai_key_validation_attempts (id, user_id, created_at) VALUES ($1,$2,NOW() - INTERVAL '2 hours'), ($3,$2,NOW())")
            .bind(uuid::Uuid::new_v4()).bind(uid).bind(uuid::Uuid::new_v4())
            .execute(&db).await.unwrap();
        purge_old_key_validation_attempts(&db).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_key_validation_attempts WHERE user_id=$1")
            .bind(uid).fetch_one(&db).await.unwrap();
        assert_eq!(n, 1);
    }

    // Ported from rag::record_llm_usage_persists_one_row (issue #140, AC #2):
    // record_usage persists exactly one row carrying the supplied token counts,
    // now tagged with provider and key_source (nels-oss#3).
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn record_usage_persists_one_row() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("llm-usage-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");

        record_usage(
            &pool,
            user_id,
            &LlmCredentials::new(Provider::Gemini, "k".into(), KeySource::Nels),
            "models/gemini-2.5-flash",
            "chat",
            TokenUsage { input: 10, output: 20, thinking: 5, total: 35 },
        )
        .await;

        #[allow(clippy::type_complexity)]
        let (count, model, call_type, input_t, output_t, thinking_t, total_t, provider, key_source): (
            i64, String, String, i32, i32, i32, i32, String, String,
        ) = sqlx::query_as(
            "SELECT COUNT(*), MAX(model), MAX(call_type), MAX(input_tokens), MAX(output_tokens), \
             MAX(thinking_tokens), MAX(total_tokens), MAX(provider), MAX(key_source) \
             FROM llm_usage WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("query llm_usage");

        assert_eq!(count, 1, "exactly one llm_usage row recorded");
        assert_eq!(model, "models/gemini-2.5-flash");
        assert_eq!(call_type, "chat");
        assert_eq!(input_t, 10);
        assert_eq!(output_t, 20);
        assert_eq!(thinking_t, 5);
        assert_eq!(total_t, 35);
        assert_eq!(provider, "gemini");
        assert_eq!(key_source, "nels");

        sqlx::query("DELETE FROM llm_usage WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup llm_usage");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("cleanup user");
    }

    // Ported from rag::record_llm_usage_swallows_write_failure (AC #6 fail-safe):
    // a usage-write failure (an FK violation from a non-existent user_id) must be
    // swallowed — record_usage returns () without panicking and inserts no row.
    #[tokio::test]
    #[ignore = "requires Postgres; run via: cargo test -- --ignored"]
    async fn record_usage_swallows_write_failure() {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgrespassword@localhost:6153/budget_rag".to_string()
        });
        let pool = sqlx::PgPool::connect(&url).await.expect("connect to test db");

        let bogus_user_id = Uuid::new_v4();
        record_usage(
            &pool,
            bogus_user_id,
            &LlmCredentials::new(Provider::Gemini, "k".into(), KeySource::Nels),
            "m",
            "chat",
            TokenUsage { input: 1, output: 1, thinking: 1, total: 1 },
        )
        .await;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_usage WHERE user_id = $1")
            .bind(bogus_user_id)
            .fetch_one(&pool)
            .await
            .expect("query llm_usage");
        assert_eq!(count, 0, "FK violation must leave no llm_usage row");
    }
}
