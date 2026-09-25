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
    pub const ALL: [Provider; 3] = [Provider::Gemini, Provider::OpenAi, Provider::Anthropic];

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
    KeyUnavailable,
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
            LlmError::Upstream(_) | LlmError::Timeout | LlmError::Transport | LlmError::BadResponse => "unavailable",
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
        tracing::error!("gemini response parse failed: {}", e.without_url());
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
        .ok_or(LlmError::BadResponse)?;
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
        tracing::error!("gemini embed parse failed: {}", e.without_url());
        LlmError::BadResponse
    })?;
    Ok((parsed.embedding.values, GemUsage::to_usage(parsed.usage)))
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
        Provider::OpenAi | Provider::Anthropic => Err(LlmError::BadResponse),
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
        Provider::OpenAi | Provider::Anthropic => Err(LlmError::BadResponse),
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
}
