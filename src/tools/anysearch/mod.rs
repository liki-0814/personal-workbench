use crate::{
    settings::{AnySearchRateLimitSettings, AnySearchSettings, Settings},
    tools::{ToolCall, ToolResult},
    PwError, Result,
};
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

const MAX_SEARCH_RESULTS: u64 = 10;
const MAX_BATCH_QUERIES: usize = 5;

#[derive(Debug, Clone)]
pub struct AnySearchExecutor;

impl AnySearchExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnySearchExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for AnySearchExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let settings = Settings::load()?.anysearch;
        let request = AnySearchRequest::from_value(&call.arguments)?;
        let response = execute_anysearch(&settings, &request)?;
        let content = extract_content_text(&response).unwrap_or_else(|| {
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| response.to_string())
        });
        let mut result = ToolResult::ok(content);
        result.metadata = response;
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AnySearchAction {
    Search,
    BatchSearch,
    Extract,
    GetSubDomains,
}

impl AnySearchAction {
    fn tool_name(&self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::BatchSearch => "batch_search",
            Self::Extract => "extract",
            Self::GetSubDomains => "get_sub_domains",
        }
    }
}

#[derive(Debug, Clone)]
struct AnySearchRequest {
    action: AnySearchAction,
    arguments: Value,
}

impl AnySearchRequest {
    fn from_value(value: &Value) -> Result<Self> {
        let action = optional_string(value, "action").unwrap_or_else(|| "search".to_string());
        match action.as_str() {
            "search" => Ok(Self {
                action: AnySearchAction::Search,
                arguments: build_search_args(value)?,
            }),
            "batch_search" => Ok(Self {
                action: AnySearchAction::BatchSearch,
                arguments: build_batch_search_args(value)?,
            }),
            "extract" => Ok(Self {
                action: AnySearchAction::Extract,
                arguments: build_extract_args(value)?,
            }),
            "get_sub_domains" => Ok(Self {
                action: AnySearchAction::GetSubDomains,
                arguments: build_get_sub_domains_args(value)?,
            }),
            other => Err(PwError::ToolExecution(format!(
                "unknown AnySearch action '{other}'"
            ))),
        }
    }
}

pub fn anysearch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["search", "batch_search", "extract", "get_sub_domains"],
                "description": "AnySearch operation. Defaults to search."
            },
            "query": {
                "type": "string",
                "description": "Search query. Required for action=search. For batch_search, can be repeated through queries instead."
            },
            "queries": {
                "type": "array",
                "description": "Batch query objects for action=batch_search. Supports query, domain, sub_domain, sub_domain_params, max_results.",
                "items": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "domain": { "type": "string" },
                        "sub_domain": { "type": "string" },
                        "sub_domain_params": {
                            "description": "Object or key=value string."
                        },
                        "max_results": { "type": "integer" }
                    },
                    "required": ["query"]
                }
            },
            "url": {
                "type": "string",
                "description": "URL to extract for action=extract."
            },
            "domain": {
                "type": "string",
                "description": "Vertical domain, e.g. finance, academic, legal, code, social_media."
            },
            "domains": {
                "description": "Comma-separated string or array of up to 5 domains for action=get_sub_domains."
            },
            "sub_domain": {
                "type": "string",
                "description": "Vertical sub-domain discovered by get_sub_domains."
            },
            "sub_domain_params": {
                "description": "Object or key=value string. Include all required params from get_sub_domains."
            },
            "max_results": {
                "type": "integer",
                "description": "1-10, default decided by AnySearch."
            }
        }
    })
}

fn execute_anysearch(settings: &AnySearchSettings, request: &AnySearchRequest) -> Result<Value> {
    let client = Client::builder()
        .timeout(Duration::from_secs(settings.request_timeout_seconds.max(1)))
        .build()
        .map_err(|err| {
            PwError::ToolExecution(format!("failed to build AnySearch client: {err}"))
        })?;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": request.action.tool_name(),
            "arguments": request.arguments
        }
    });

    let mut attempt = 0_u32;
    let response = loop {
        let _permit = acquire_rate_permit(&settings.rate_limit);
        let mut builder = client.post(&settings.endpoint).json(&payload);
        if let Some(api_key) = settings.api_key.as_deref().filter(|key| !key.is_empty()) {
            builder = builder.bearer_auth(api_key);
        }
        let response = builder
            .send()
            .map_err(|err| PwError::ToolExecution(format!("AnySearch request failed: {err}")))?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            && settings.rate_limit.retry_on_429
            && attempt < settings.rate_limit.max_retries
        {
            attempt += 1;
            let backoff = Duration::from_secs(2_u64.saturating_pow(attempt - 1));
            drop(_permit);
            thread::sleep(backoff);
            continue;
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(PwError::ToolExecution(format!(
                "AnySearch request failed: HTTP {status}: {body}"
            )));
        }
        break response
            .json::<Value>()
            .map_err(|err| PwError::ToolExecution(format!("invalid AnySearch response: {err}")))?;
    };

    if !response["error"].is_null() {
        return Err(PwError::ToolExecution(format!(
            "AnySearch API error: {}",
            response["error"]
        )));
    }
    Ok(response)
}

#[derive(Default)]
struct RateState {
    inflight: u32,
    starts: VecDeque<Instant>,
}

static RATE_STATE: OnceLock<(Mutex<RateState>, Condvar)> = OnceLock::new();

struct RatePermit;

impl Drop for RatePermit {
    fn drop(&mut self) {
        let (mutex, cvar) =
            RATE_STATE.get_or_init(|| (Mutex::new(RateState::default()), Condvar::new()));
        let mut state = mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.inflight = state.inflight.saturating_sub(1);
        cvar.notify_all();
    }
}

fn acquire_rate_permit(config: &AnySearchRateLimitSettings) -> RatePermit {
    let (mutex, cvar) =
        RATE_STATE.get_or_init(|| (Mutex::new(RateState::default()), Condvar::new()));
    let mut state = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        let now = Instant::now();
        while state
            .starts
            .front()
            .is_some_and(|started| now.duration_since(*started) >= Duration::from_secs(60))
        {
            state.starts.pop_front();
        }

        let parallel_ok = config.max_parallel == 0 || state.inflight < config.max_parallel;
        let minute_ok =
            config.max_per_minute == 0 || state.starts.len() < config.max_per_minute as usize;
        if parallel_ok && minute_ok {
            state.inflight = state.inflight.saturating_add(1);
            state.starts.push_back(now);
            return RatePermit;
        }

        if !minute_ok {
            let wait_for = state
                .starts
                .front()
                .map(|started| {
                    Duration::from_secs(60)
                        .saturating_sub(now.duration_since(*started))
                        .max(Duration::from_millis(50))
                })
                .unwrap_or_else(|| Duration::from_millis(50));
            let (next_state, _) = cvar
                .wait_timeout(state, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
        } else {
            state = cvar
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

fn build_search_args(value: &Value) -> Result<Value> {
    let query = required_string(value, "query")?;
    let mut args = Map::new();
    args.insert("query".to_string(), Value::String(query));
    insert_optional_string(&mut args, value, "domain");
    insert_optional_string(&mut args, value, "sub_domain");
    if let Some(params) = sub_domain_params(value)? {
        args.insert("sub_domain_params".to_string(), params);
    }
    insert_max_results(&mut args, value);
    Ok(Value::Object(args))
}

fn build_batch_search_args(value: &Value) -> Result<Value> {
    let queries = value
        .get("queries")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PwError::ToolExecution("AnySearch batch_search requires 'queries' array".to_string())
        })?;
    if queries.is_empty() || queries.len() > MAX_BATCH_QUERIES {
        return Err(PwError::ToolExecution(format!(
            "AnySearch batch_search supports 1-{MAX_BATCH_QUERIES} queries"
        )));
    }

    let shared_domain = optional_string(value, "domain");
    let shared_sub_domain = optional_string(value, "sub_domain");
    let shared_params = sub_domain_params(value)?;
    let mut normalized = Vec::new();
    for query in queries {
        let mut item = query
            .as_object()
            .cloned()
            .ok_or_else(|| PwError::ToolExecution("batch query must be an object".to_string()))?;
        if item
            .get("query")
            .and_then(Value::as_str)
            .is_none_or(|query| query.trim().is_empty())
        {
            return Err(PwError::ToolExecution(
                "each batch query requires non-empty 'query'".to_string(),
            ));
        }
        if !item.contains_key("domain") {
            if let Some(domain) = &shared_domain {
                item.insert("domain".to_string(), Value::String(domain.clone()));
            }
        }
        if !item.contains_key("sub_domain") {
            if let Some(sub_domain) = &shared_sub_domain {
                item.insert("sub_domain".to_string(), Value::String(sub_domain.clone()));
            }
        }
        if let Some(params) = item.get("sub_domain_params").cloned() {
            item.insert(
                "sub_domain_params".to_string(),
                normalize_sub_domain_params(params)?,
            );
        } else if let Some(params) = &shared_params {
            item.insert("sub_domain_params".to_string(), params.clone());
        }
        if let Some(max_results) = item.get("max_results").and_then(Value::as_u64) {
            item.insert(
                "max_results".to_string(),
                json!(max_results.clamp(1, MAX_SEARCH_RESULTS)),
            );
        }
        normalized.push(Value::Object(item));
    }

    Ok(json!({ "queries": normalized }))
}

fn build_extract_args(value: &Value) -> Result<Value> {
    Ok(json!({ "url": required_string(value, "url")? }))
}

fn build_get_sub_domains_args(value: &Value) -> Result<Value> {
    if let Some(domains) = value.get("domains") {
        let domains = normalize_domains(domains)?;
        if domains.is_empty() || domains.len() > MAX_BATCH_QUERIES {
            return Err(PwError::ToolExecution(format!(
                "AnySearch get_sub_domains supports 1-{MAX_BATCH_QUERIES} domains"
            )));
        }
        return Ok(json!({ "domains": domains }));
    }
    Ok(json!({ "domain": required_string(value, "domain")? }))
}

fn extract_content_text(response: &Value) -> Option<String> {
    let content = response.get("result")?.get("content")?.as_array()?;
    content
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|item| item.get("text").and_then(Value::as_str))
        .map(str::to_string)
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).ok_or_else(|| {
        PwError::ToolExecution(format!("AnySearch action requires non-empty '{key}'"))
    })
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn insert_optional_string(args: &mut Map<String, Value>, value: &Value, key: &str) {
    if let Some(item) = optional_string(value, key) {
        args.insert(key.to_string(), Value::String(item));
    }
}

fn insert_max_results(args: &mut Map<String, Value>, value: &Value) {
    if let Some(max_results) = value.get("max_results").and_then(Value::as_u64) {
        args.insert(
            "max_results".to_string(),
            json!(max_results.clamp(1, MAX_SEARCH_RESULTS)),
        );
    }
}

fn sub_domain_params(value: &Value) -> Result<Option<Value>> {
    value
        .get("sub_domain_params")
        .cloned()
        .map(normalize_sub_domain_params)
        .transpose()
}

fn normalize_sub_domain_params(value: Value) -> Result<Value> {
    if value.is_object() {
        return Ok(value);
    }
    let Some(raw) = value.as_str() else {
        return Err(PwError::ToolExecution(
            "sub_domain_params must be an object or key=value string".to_string(),
        ));
    };
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
        if parsed.is_object() {
            return Ok(parsed);
        }
    }
    let mut out = Map::new();
    for pair in raw.split(',') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty() {
            out.insert(key.to_string(), Value::String(value.trim().to_string()));
        }
    }
    if out.is_empty() {
        return Err(PwError::ToolExecution(
            "sub_domain_params must be valid JSON or key=value pairs".to_string(),
        ));
    }
    Ok(Value::Object(out))
}

fn normalize_domains(value: &Value) -> Result<Vec<String>> {
    if let Some(items) = value.as_array() {
        return Ok(items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect());
    }
    if let Some(raw) = value.as_str() {
        return Ok(raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect());
    }
    Err(PwError::ToolExecution(
        "domains must be an array or comma-separated string".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_general_search_args() {
        let request = AnySearchRequest::from_value(&json!({
            "query": "rust async runtime",
            "max_results": 99
        }))
        .unwrap();
        assert_eq!(request.action, AnySearchAction::Search);
        assert_eq!(request.arguments["query"], "rust async runtime");
        assert_eq!(request.arguments["max_results"], 10);
    }

    #[test]
    fn parses_vertical_search_params() {
        let request = AnySearchRequest::from_value(&json!({
            "action": "search",
            "query": "AAPL",
            "domain": "finance",
            "sub_domain": "finance.quote",
            "sub_domain_params": "type=stock,symbol=AAPL,cn_code="
        }))
        .unwrap();
        assert_eq!(request.arguments["sub_domain_params"]["type"], "stock");
        assert_eq!(request.arguments["sub_domain_params"]["symbol"], "AAPL");
        assert_eq!(request.arguments["sub_domain_params"]["cn_code"], "");
    }

    #[test]
    fn injects_batch_shared_params() {
        let request = AnySearchRequest::from_value(&json!({
            "action": "batch_search",
            "domain": "finance",
            "sub_domain": "finance.quote",
            "sub_domain_params": {"type": "stock", "cn_code": ""},
            "queries": [
                {"query": "AAPL", "sub_domain_params": "type=stock,symbol=AAPL,cn_code="},
                {"query": "MSFT"}
            ]
        }))
        .unwrap();
        let queries = request.arguments["queries"].as_array().unwrap();
        assert_eq!(queries[0]["sub_domain_params"]["symbol"], "AAPL");
        assert_eq!(queries[1]["domain"], "finance");
        assert_eq!(queries[1]["sub_domain"], "finance.quote");
        assert_eq!(queries[1]["sub_domain_params"]["type"], "stock");
    }

    #[test]
    fn normalizes_get_sub_domains() {
        let request = AnySearchRequest::from_value(&json!({
            "action": "get_sub_domains",
            "domains": "finance, academic"
        }))
        .unwrap();
        assert_eq!(request.arguments["domains"], json!(["finance", "academic"]));
    }

    #[test]
    fn extracts_text_content_from_json_rpc_response() {
        let response = json!({
            "result": {
                "content": [
                    {"type": "text", "text": "hello"}
                ]
            }
        });
        assert_eq!(extract_content_text(&response).as_deref(), Some("hello"));
    }
}
