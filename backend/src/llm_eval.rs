//! Per-provider eval of the chat action schema (nels-oss#3, spec §7).
//!
//! The scorer and the fixture checks always run. The live eval is `#[ignore]`d,
//! costs money, and needs `EVAL_PROVIDER` + `EVAL_API_KEY`. It uses the exact
//! production system prompt (`rag::build_system_instructions`), the production
//! user-message framing (`USER MESSAGE: ...`), the production adapter
//! (`llm::generate_json`) and the production 30s chat timeout, so a pass here
//! measures what users would actually get.

use serde::Deserialize;
use serde_json::{Map, Value};

const PARSE_GATE: f64 = 0.95;
const ACTION_GATE: f64 = 0.90;
/// Retries per fixture when the provider rate-limits the eval (backoff 20s, 40s, 60s).
const RATE_LIMIT_RETRIES: u32 = 3;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Fixture {
    pub id: String,
    pub message: String,
    pub expect_action: String,
    #[serde(default)]
    pub expect_params: Map<String, Value>,
}

#[derive(Deserialize)]
pub(crate) struct EvalContext {
    user_email: String,
    name_context: String,
    language_context: String,
    budgets_context: String,
    budget_context: String,
    semantic_context: String,
    history_context: String,
    usage_context: String,
}

impl EvalContext {
    pub(crate) fn as_prompt(&self) -> crate::rag::PromptContext<'_> {
        crate::rag::PromptContext {
            user_email: &self.user_email,
            name_context: &self.name_context,
            language_context: &self.language_context,
            budgets_context: &self.budgets_context,
            budget_context: &self.budget_context,
            semantic_context: &self.semantic_context,
            history_context: &self.history_context,
            usage_context: &self.usage_context,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Outcome {
    pub parsed: bool,
    pub action_ok: bool,
    pub params_ok: bool,
}

pub(crate) fn load_fixtures() -> Vec<Fixture> {
    let raw = include_str!("../evals/chat_actions.jsonl");
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad fixture line {l}: {e}")))
        .collect()
}

pub(crate) fn load_context() -> EvalContext {
    serde_json::from_str(include_str!("../evals/context.json")).expect("evals/context.json")
}

/// Strings compare case-insensitively after trimming, numbers within 0.01,
/// booleans exactly. An expected object matches when every expected key
/// matches recursively in the actual object (extra actual keys are ignored),
/// so nested params such as `retirement_profile` can be checked. Anything
/// else (including a missing param) is a miss.
fn value_matches(want: &Value, got: Option<&Value>) -> bool {
    match (want, got) {
        (Value::Object(w), Some(Value::Object(g))) => w.iter().all(|(k, wv)| value_matches(wv, g.get(k))),
        (Value::String(w), Some(Value::String(g))) => w.trim().to_lowercase() == g.trim().to_lowercase(),
        (Value::Number(w), Some(Value::Number(g))) => {
            (w.as_f64().unwrap_or(f64::NAN) - g.as_f64().unwrap_or(f64::NAN)).abs() < 0.01
        }
        (Value::Bool(w), Some(Value::Bool(g))) => w == g,
        _ => false,
    }
}

/// `parsed` means the output deserializes as the production `AiStructuredResponse`,
/// so the chat path would NOT hit `malformed_response_fallback`.
pub(crate) fn score(fx: &Fixture, raw: &str) -> Outcome {
    let parsed = serde_json::from_str::<crate::rag::AiStructuredResponse>(raw.trim()).is_ok();
    let v: Value = match serde_json::from_str(raw.trim()) {
        Ok(v) if parsed => v,
        _ => return Outcome { parsed: false, action_ok: false, params_ok: false },
    };
    let action_ok = v.get("action").and_then(Value::as_str) == Some(fx.expect_action.as_str());
    let params = v.get("action_params").and_then(Value::as_object);
    let params_ok = action_ok
        && fx
            .expect_params
            .iter()
            .all(|(k, want)| value_matches(want, params.and_then(|p| p.get(k))));
    Outcome { parsed, action_ok, params_ok }
}

pub(crate) fn report(provider: &str, model: &str, outcomes: &[(String, Outcome)]) -> String {
    let n = outcomes.len().max(1) as f64;
    let pct = |f: &dyn Fn(&Outcome) -> bool| outcomes.iter().filter(|(_, o)| f(o)).count() as f64 / n;
    let (p, a, pa) = (pct(&|o| o.parsed), pct(&|o| o.action_ok), pct(&|o| o.params_ok));
    let gate = if p >= PARSE_GATE && a >= ACTION_GATE { "PASS" } else { "FAIL" };
    let mut md = format!(
        "# Chat action eval: {provider} / {model}\n\n\
         - Fixtures: {}\n- Parse rate: {:.1}%\n- Action accuracy: {:.1}%\n- Action + params accuracy: {:.1}%\n\
         - Gate: {gate} (parse ≥ {:.0}%, action ≥ {:.0}%)\n\n## Failures\n\n| id | parsed | action | params |\n|---|---|---|---|\n",
        outcomes.len(),
        p * 100.0,
        a * 100.0,
        pa * 100.0,
        PARSE_GATE * 100.0,
        ACTION_GATE * 100.0
    );
    for (id, o) in outcomes.iter().filter(|(_, o)| !o.params_ok) {
        md.push_str(&format!("| {id} | {} | {} | {} |\n", o.parsed, o.action_ok, o.params_ok));
    }
    md
}

#[tokio::test]
#[ignore = "live, paid: EVAL_PROVIDER=openai EVAL_API_KEY=... cargo test --bin backend live_chat_action_eval -- --ignored --nocapture"]
async fn live_chat_action_eval() {
    use crate::llm::{generate_json, KeySource, LlmCredentials, Provider};
    let set = |v: &str| std::env::var(v).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (Some(provider_raw), Some(key)) = (set("EVAL_PROVIDER"), set("EVAL_API_KEY")) else {
        eprintln!("live_chat_action_eval skipped: set EVAL_PROVIDER and EVAL_API_KEY to run it");
        return;
    };
    let provider = Provider::parse(&provider_raw)
        .unwrap_or_else(|| panic!("EVAL_PROVIDER={provider_raw:?} is not one of gemini|openai|anthropic"));
    let creds = LlmCredentials::new(provider, key, KeySource::Byo);
    let ctx = load_context();
    let system = crate::rag::build_system_instructions(&ctx.as_prompt());
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let dir = format!("{}/../docs/evals", env!("CARGO_MANIFEST_DIR"));
    let model_slug = creds.model.replace(['/', ':'], "-");
    let path = format!("{dir}/{date}-{}-{model_slug}.md", provider.as_str());
    std::fs::create_dir_all(&dir).unwrap();
    // EVAL_ONLY=id1,id2 reruns just those fixtures to diagnose failures. It is a
    // diagnostic mode: it prints full raw replies and never writes a results file,
    // so a partial run can't overwrite or pass for a full one.
    let only: Option<Vec<String>> =
        set("EVAL_ONLY").map(|v| v.split(',').map(|s| s.trim().to_string()).collect());
    let fixtures: Vec<Fixture> = load_fixtures()
        .into_iter()
        .filter(|f| only.as_ref().map_or(true, |ids| ids.contains(&f.id)))
        .collect();
    let mut outcomes = Vec::new();
    for fx in fixtures {
        // Same user framing and 30s timeout as rag.rs's chat_endpoint. A 429 says
        // nothing about model quality (new accounts have low rate limits), so retry
        // it with backoff before scoring; any other error counts as a miss.
        let mut attempt = 0;
        let raw = loop {
            match generate_json(
                &creds,
                &system,
                &format!("USER MESSAGE: {}", fx.message),
                std::time::Duration::from_secs(30),
            )
            .await
            {
                Ok(out) => break out.text,
                Err(crate::llm::LlmError::RateLimited) if attempt < RATE_LIMIT_RETRIES => {
                    attempt += 1;
                    eprintln!("{}: rate limited, retry {attempt}/{RATE_LIMIT_RETRIES}", fx.id);
                    tokio::time::sleep(std::time::Duration::from_secs(20 * attempt as u64)).await;
                }
                Err(e) => {
                    // LlmError carries no key material; safe to print.
                    eprintln!("{}: call failed: {:?}", fx.id, e);
                    break String::new();
                }
            }
        };
        let o = score(&fx, &raw);
        if !o.params_ok || only.is_some() {
            let limit = if only.is_some() { usize::MAX } else { 400 };
            eprintln!("{} {} {:?}\n  raw: {}", fx.id, if o.params_ok { "ok" } else { "FAIL" }, o,
                      raw.chars().take(limit).collect::<String>());
        }
        outcomes.push((fx.id.clone(), o));
        if only.is_none() {
            // Rewrite the report after every paid call, so an interrupted run keeps
            // the results it already paid for (the file then covers fewer fixtures).
            std::fs::write(&path, report(provider.as_str(), &creds.model, &outcomes)).unwrap();
        }
    }
    let md = report(provider.as_str(), &creds.model, &outcomes);
    if only.is_some() {
        println!("{md}\n(EVAL_ONLY diagnostic run: no results file written)");
    } else {
        println!("{md}\nwritten to {path}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(action: &str, params: serde_json::Value) -> Fixture {
        Fixture {
            id: "t".into(),
            message: "m".into(),
            expect_action: action.into(),
            expect_params: params.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn scores_parse_action_and_params() {
        let f = fx("ADD_TRANSACTION", serde_json::json!({"amount": 23.4, "category_name": "Groceries"}));
        let ok = r#"{"thought":"","action":"ADD_TRANSACTION","action_params":{"amount":23.40,"category_name":" groceries "},"response_text":"ok"}"#;
        assert_eq!(score(&f, ok), Outcome { parsed: true, action_ok: true, params_ok: true });
        let wrong_amt = ok.replace("23.40", "24.00");
        assert_eq!(score(&f, &wrong_amt), Outcome { parsed: true, action_ok: true, params_ok: false });
        let wrong_action = ok.replace("\"ADD_TRANSACTION\"", "\"NONE\"");
        assert!(!score(&f, &wrong_action).action_ok);
        assert_eq!(score(&f, "not json"), Outcome { parsed: false, action_ok: false, params_ok: false });
    }

    #[test]
    fn nested_expected_params_match_recursively() {
        let f = fx(
            "SET_RETIREMENT_PROFILE",
            serde_json::json!({"retirement_profile": {"country": "US", "birth_date": "1985-06-15", "current_gross_income": 120000}}),
        );
        let ok = r#"{"action":"SET_RETIREMENT_PROFILE","action_params":{"retirement_profile":{"country":"us","birth_date":"1985-06-15","current_gross_income":120000.0,"target_retirement_age":62}},"response_text":"ok"}"#;
        assert_eq!(score(&f, ok), Outcome { parsed: true, action_ok: true, params_ok: true });
        // A nested value mismatch fails.
        let wrong = ok.replace("120000.0", "12000.0");
        assert_eq!(score(&f, &wrong), Outcome { parsed: true, action_ok: true, params_ok: false });
        // A missing nested key fails.
        let missing = ok.replace(r#""birth_date":"1985-06-15","#, "");
        assert!(!score(&f, &missing).params_ok);
        // The nested fields at the top level instead of inside the object fail.
        let flat = r#"{"action":"SET_RETIREMENT_PROFILE","action_params":{"country":"US","birth_date":"1985-06-15","current_gross_income":120000},"response_text":"ok"}"#;
        assert!(!score(&f, flat).params_ok);
    }

    #[test]
    fn empty_expected_params_pass_when_action_matches() {
        let f = fx("LIST_BUDGETS", serde_json::json!({}));
        let raw = r#"{"action":"LIST_BUDGETS","response_text":"here"}"#;
        assert_eq!(score(&f, raw), Outcome { parsed: true, action_ok: true, params_ok: true });
    }

    #[test]
    fn valid_json_that_is_not_the_chat_schema_does_not_parse() {
        // Missing response_text: production would hit malformed_response_fallback.
        let f = fx("LIST_BUDGETS", serde_json::json!({}));
        let raw = r#"{"action":"LIST_BUDGETS"}"#;
        assert_eq!(score(&f, raw), Outcome { parsed: false, action_ok: false, params_ok: false });
    }

    #[test]
    fn fixtures_file_loads_and_covers_every_prompt_action() {
        let fixtures = load_fixtures();
        assert!(fixtures.len() >= 45);
        let prompt = crate::rag::build_system_instructions(&load_context().as_prompt());
        let line = prompt.lines().find(|l| l.contains("\"action\": \"NONE\"")).expect("action enum line");
        let enum_part = line.split_once("\"action\":").expect("action key").1;
        let actions: Vec<&str> = enum_part
            .trim_end_matches(',')
            .split('|')
            .map(|a| a.trim().trim_matches('"'))
            .filter(|a| !a.is_empty())
            .collect();
        assert!(actions.len() >= 39, "parsed {} actions: {:?}", actions.len(), actions);
        for a in &actions {
            assert!(fixtures.iter().any(|f| f.expect_action == *a), "no fixture for action {a}");
        }
        // The reverse: no fixture may expect an action the prompt does not offer.
        for f in &fixtures {
            assert!(actions.contains(&f.expect_action.as_str()), "fixture {} expects unknown action {}", f.id, f.expect_action);
        }
        // Fixture ids are unique, so the report's failure table is unambiguous.
        let mut ids: Vec<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), fixtures.len(), "duplicate fixture id");
    }

    #[test]
    fn report_computes_rates_and_gate() {
        let outs = vec![
            ("a".to_string(), Outcome { parsed: true, action_ok: true, params_ok: true }),
            ("b".to_string(), Outcome { parsed: true, action_ok: false, params_ok: false }),
        ];
        let md = report("openai", "gpt-4.1-mini", &outs);
        assert!(md.contains("Parse rate: 100.0%"));
        assert!(md.contains("Action accuracy: 50.0%"));
        assert!(md.contains("Gate: FAIL"));
        assert!(md.contains("| b |"));
        assert!(!md.contains("| a |"), "passing fixtures must not be listed as failures");
    }
}
