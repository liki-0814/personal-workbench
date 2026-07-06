use crate::{
    settings::{ModelSettings, OpenAiApiKind, ProviderProtocol},
    PwError, Result,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    env,
    io::{BufRead, BufReader},
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct ModelProviderRef {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ModelToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<ModelMessage>,
    pub system: Option<String>,
    pub thinking: ThinkingConfig,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    #[serde(default)]
    pub tools: Vec<ModelToolSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall(ModelToolCall),
    Usage(ModelUsage),
    Done,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelResponse {
    pub content: String,
    pub thinking: String,
    pub usage: ModelUsage,
    pub tool_calls: Vec<ModelToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

pub trait ModelClient {
    fn stream(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<ModelResponse>;

    fn complete(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let mut response = ModelResponse::default();
        self.stream(request, &mut |event| match event {
            ModelEvent::TextDelta(delta) => response.content.push_str(&delta),
            ModelEvent::ThinkingDelta(delta) => response.thinking.push_str(&delta),
            ModelEvent::ToolCall(call) => response.tool_calls.push(call),
            ModelEvent::Usage(usage) => response.usage = usage,
            ModelEvent::Done => {}
        })?;
        Ok(response)
    }
}

pub enum AnyModelClient {
    OpenAi(OpenAiClient),
    Anthropic(AnthropicClient),
    Nvidia(OpenAiClient),
}

impl AnyModelClient {
    pub fn from_settings(settings: &ModelSettings) -> Result<Self> {
        match settings.provider {
            ProviderProtocol::OpenAi => Ok(Self::OpenAi(OpenAiClient::from_settings(settings)?)),
            ProviderProtocol::Anthropic => {
                Ok(Self::Anthropic(AnthropicClient::from_settings(settings)?))
            }
            ProviderProtocol::Nvidia => Ok(Self::Nvidia(OpenAiClient::new(
                configured_api_key(settings.api_key.as_deref(), &settings.api_key_env)?,
                settings.base_url.clone(),
                OpenAiApiKind::ChatCompletions,
                settings.extra_body.clone(),
                settings.request_timeout_seconds,
            )?)),
        }
    }
}

impl ModelClient for AnyModelClient {
    fn stream(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<ModelResponse> {
        match self {
            Self::OpenAi(client) => client.stream(request, on_event),
            Self::Anthropic(client) => client.stream(request, on_event),
            Self::Nvidia(client) => client.stream(request, on_event),
        }
    }
}

pub struct OpenAiClient {
    api_key: String,
    base_url: String,
    api_kind: OpenAiApiKind,
    extra_body: Value,
    http: Client,
}

impl OpenAiClient {
    pub fn from_settings(settings: &ModelSettings) -> Result<Self> {
        let api_key = configured_api_key(settings.api_key.as_deref(), &settings.api_key_env)?;
        Self::new(
            api_key,
            settings.base_url.clone(),
            settings.api,
            settings.extra_body.clone(),
            settings.request_timeout_seconds,
        )
    }

    pub fn new(
        api_key: String,
        base_url: String,
        api_kind: OpenAiApiKind,
        extra_body: Value,
        timeout_seconds: u64,
    ) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|err| {
                PwError::Message(format!(
                    "failed to build OpenAI-compatible HTTP client: {err}"
                ))
            })?;
        Ok(Self {
            api_key,
            base_url,
            api_kind,
            extra_body,
            http,
        })
    }
}

impl ModelClient for OpenAiClient {
    fn stream(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<ModelResponse> {
        let attempts = max_token_fallback_attempts(request.max_tokens);
        let mut last_error = None;
        for max_tokens in attempts {
            let mut attempt = request.clone();
            attempt.max_tokens = max_tokens;
            let result = self.stream_once(&attempt, on_event);
            match result {
                Ok(response) => return Ok(response),
                Err(err) if should_retry_with_lower_max_tokens(&err, max_tokens) => {
                    last_error = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            PwError::Message("OpenAI-compatible model request failed".to_string())
        }))
    }
}

impl OpenAiClient {
    fn stream_once(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<ModelResponse> {
        let (url, payload, parser): (_, _, fn(&Value) -> Vec<ModelEvent>) = match self.api_kind {
            OpenAiApiKind::Responses => (
                format!("{}/responses", self.base_url.trim_end_matches('/')),
                openai_responses_payload(request, &self.extra_body),
                parse_openai_responses_event,
            ),
            OpenAiApiKind::ChatCompletions => (
                format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
                openai_chat_completions_payload(request, &self.extra_body),
                parse_openai_chat_event,
            ),
        };
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header(
                "Accept",
                if request.stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .json(&payload)
            .send()
            .map_err(|err| model_request_error("OpenAI-compatible", &url, err))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(PwError::Message(format!(
                "OpenAI-compatible provider returned HTTP {status} from {url}. {}",
                truncate_error_body(&body)
            )));
        }

        if request.stream {
            match self.api_kind {
                OpenAiApiKind::ChatCompletions => parse_openai_chat_sse(response, on_event),
                OpenAiApiKind::Responses => parse_sse(response, on_event, parser),
            }
        } else {
            parse_json_response(response, on_event, parser)
        }
    }
}

pub struct AnthropicClient {
    api_key: String,
    base_url: String,
    version: String,
    extra_body: Value,
    http: Client,
}

impl AnthropicClient {
    pub fn from_settings(settings: &ModelSettings) -> Result<Self> {
        let api_key = configured_api_key(settings.api_key.as_deref(), &settings.api_key_env)?;
        Self::new(
            api_key,
            settings.base_url.clone(),
            "2023-06-01".to_string(),
            settings.extra_body.clone(),
            settings.request_timeout_seconds,
        )
    }

    pub fn new(
        api_key: String,
        base_url: String,
        version: String,
        extra_body: Value,
        timeout_seconds: u64,
    ) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|err| {
                PwError::Message(format!("failed to build Anthropic HTTP client: {err}"))
            })?;
        Ok(Self {
            api_key,
            base_url,
            version,
            extra_body,
            http,
        })
    }
}

impl ModelClient for AnthropicClient {
    fn stream(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<ModelResponse> {
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let payload = anthropic_payload(request, &self.extra_body);
        let response = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(&payload)
            .send()
            .map_err(|err| model_request_error("Anthropic-compatible", &url, err))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(PwError::Message(format!(
                "Anthropic-compatible provider returned HTTP {status} from {url}. {}",
                truncate_error_body(&body)
            )));
        }

        if request.stream {
            parse_anthropic_sse(response, on_event)
        } else {
            parse_json_response(response, on_event, parse_anthropic_json_event)
        }
    }
}

fn configured_api_key(configured: Option<&str>, env_name: &str) -> Result<String> {
    if let Some(api_key) = configured.filter(|value| !value.is_empty()) {
        return Ok(api_key.to_string());
    }

    env::var(env_name).map_err(|_| PwError::Message(format!("{env_name} is not set")))
}

fn model_request_error(provider: &str, url: &str, err: reqwest::Error) -> PwError {
    let message = if err.is_connect() {
        format!(
            "{provider} provider is unreachable at {url}. Check that the provider server is running and base_url is correct."
        )
    } else if err.is_timeout() {
        format!("{provider} provider request timed out at {url}.")
    } else if err.is_request() {
        format!("{provider} provider request could not be sent to {url}: {err}")
    } else {
        format!("{provider} provider request failed at {url}: {err}")
    };
    PwError::Message(message)
}

fn truncate_error_body(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "No response body.".to_string();
    }
    const LIMIT: usize = 500;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    format!("{}...", &body[..LIMIT])
}

fn max_token_fallback_attempts(requested: Option<u32>) -> Vec<Option<u32>> {
    const TIERS: [u32; 4] = [128_000, 64_000, 32_000, 16_000];
    let Some(requested) = requested else {
        return vec![None];
    };
    let mut attempts = vec![Some(requested)];
    for tier in TIERS {
        if tier < requested && !attempts.contains(&Some(tier)) {
            attempts.push(Some(tier));
        }
    }
    attempts
}

fn should_retry_with_lower_max_tokens(err: &PwError, attempted: Option<u32>) -> bool {
    let Some(attempted) = attempted else {
        return false;
    };
    if attempted <= 16_000 {
        return false;
    }
    let message = err.to_string().to_lowercase();
    message.contains("invalid argument")
        || (message.contains("max") && message.contains("token"))
        || message.contains("max_tokens")
        || message.contains("max_output_tokens")
}

fn openai_responses_payload(request: &ModelRequest, extra_body: &Value) -> Value {
    let mut input = Vec::new();
    if let Some(system) = &request.system {
        input.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        if matches!(message.role, ModelRole::Tool) {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.as_deref().unwrap_or("tool_call"),
                "output": message.content
            }));
        } else {
            input.push(json!({
                "role": openai_role(&message.role),
                "content": message.content
            }));
        }
    }

    let mut payload = json!({
        "model": request.model,
        "input": input,
        "stream": request.stream
    });
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = json!("auto");
    }

    if request.thinking.enabled {
        payload["reasoning"] = json!({
            "effort": "medium",
            "summary": "auto"
        });
    }
    if let Some(max_tokens) = request.max_tokens {
        payload["max_output_tokens"] = json!(max_tokens);
    }

    merge_extra_body(payload, extra_body)
}

fn openai_chat_completions_payload(request: &ModelRequest, extra_body: &Value) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        if matches!(message.role, ModelRole::Tool) {
            let mut tool_message = json!({
                "role": "tool",
                "tool_call_id": message.tool_call_id.as_deref().unwrap_or("tool_call"),
                "content": message.content
            });
            if openai_chat_tool_messages_need_name(request) {
                if let Some(name) = message
                    .tool_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                {
                    tool_message["name"] = json!(name);
                }
            }
            messages.push(tool_message);
        } else {
            let mut chat_message = json!({
                "role": openai_role(&message.role),
                "content": message.content
            });
            if matches!(message.role, ModelRole::Assistant) && !message.tool_calls.is_empty() {
                chat_message["tool_calls"] = Value::Array(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments.to_string(),
                                }
                            })
                        })
                        .collect(),
                );
            }
            messages.push(chat_message);
        }
    }

    let mut payload = json!({
        "model": request.model,
        "messages": messages,
        "stream": request.stream
    });
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema
                        }
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = json!("auto");
    }

    if request.stream {
        payload["stream_options"] = json!({ "include_usage": true });
    }

    if request.thinking.enabled {
        payload["enable_thinking"] = json!(true);
    }
    if let Some(max_tokens) = request.max_tokens {
        payload["max_tokens"] = json!(max_tokens);
    }

    merge_extra_body(payload, extra_body)
}

fn openai_chat_tool_messages_need_name(request: &ModelRequest) -> bool {
    let model = request.model.to_ascii_lowercase();
    model.contains("gemini") || model.contains("antigravity")
}

fn merge_extra_body(mut payload: Value, extra_body: &Value) -> Value {
    let Some(extra) = extra_body.as_object() else {
        return payload;
    };
    let Some(payload_obj) = payload.as_object_mut() else {
        return payload;
    };

    for (key, value) in extra {
        payload_obj.insert(key.clone(), value.clone());
    }

    payload
}

fn anthropic_payload(request: &ModelRequest, extra_body: &Value) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        if matches!(message.role, ModelRole::System) {
            continue;
        }
        if matches!(message.role, ModelRole::Tool) {
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.as_deref().unwrap_or("tool_use"),
                    "content": message.content
                }]
            }));
        } else if matches!(message.role, ModelRole::Assistant) && !message.tool_calls.is_empty() {
            let mut content = Vec::new();
            if !message.content.trim().is_empty() {
                content.push(json!({ "type": "text", "text": message.content }));
            }
            content.extend(message.tool_calls.iter().map(|call| {
                json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": call.arguments
                })
            }));
            messages.push(json!({
                "role": "assistant",
                "content": content
            }));
        } else {
            messages.push(json!({
                "role": anthropic_role(&message.role),
                "content": message.content
            }));
        }
    }

    let mut payload = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens.unwrap_or(4096),
        "stream": request.stream
    });
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema
                    })
                })
                .collect(),
        );
    }

    if let Some(system) = &request.system {
        payload["system"] = json!(system);
    }
    if request.thinking.enabled {
        payload["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": request.thinking.budget_tokens.unwrap_or(1024)
        });
    }

    merge_extra_body(payload, extra_body)
}

fn openai_role(role: &ModelRole) -> &'static str {
    match role {
        ModelRole::System => "system",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    }
}

fn anthropic_role(role: &ModelRole) -> &'static str {
    match role {
        ModelRole::Assistant => "assistant",
        _ => "user",
    }
}

fn parse_sse(
    response: reqwest::blocking::Response,
    on_event: &mut dyn FnMut(ModelEvent),
    parser: fn(&Value) -> Vec<ModelEvent>,
) -> Result<ModelResponse> {
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    let mut final_response = ModelResponse::default();
    let mut done_sent = false;

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data == "[DONE]" {
            on_event(ModelEvent::Done);
            done_sent = true;
            break;
        }
        let value: Value = serde_json::from_str(data)?;
        if let Some(err) = model_response_error(&value) {
            return Err(err);
        }
        for event in parser(&value) {
            match &event {
                ModelEvent::TextDelta(delta) => final_response.content.push_str(delta),
                ModelEvent::ThinkingDelta(delta) => final_response.thinking.push_str(delta),
                ModelEvent::ToolCall(call) => final_response.tool_calls.push(call.clone()),
                ModelEvent::Usage(usage) => final_response.usage = usage.clone(),
                ModelEvent::Done => {}
            }
            on_event(event);
        }
    }

    if !done_sent {
        on_event(ModelEvent::Done);
    }
    Ok(final_response)
}

#[derive(Debug, Clone, Default)]
struct StreamingToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn parse_openai_chat_sse(
    response: reqwest::blocking::Response,
    on_event: &mut dyn FnMut(ModelEvent),
) -> Result<ModelResponse> {
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    let mut final_response = ModelResponse::default();
    let mut tool_calls: HashMap<usize, StreamingToolCall> = HashMap::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data == "[DONE]" {
            break;
        }
        let value: Value = serde_json::from_str(data)?;
        if let Some(err) = model_response_error(&value) {
            return Err(err);
        }
        for event in parse_openai_chat_event(&value) {
            match &event {
                ModelEvent::TextDelta(delta) => final_response.content.push_str(delta),
                ModelEvent::ThinkingDelta(delta) => final_response.thinking.push_str(delta),
                ModelEvent::ToolCall(call) => final_response.tool_calls.push(call.clone()),
                ModelEvent::Usage(usage) => final_response.usage = usage.clone(),
                ModelEvent::Done => continue,
            }
            on_event(event);
        }
        collect_openai_chat_tool_deltas(&value, &mut tool_calls);
    }

    for (_, call) in sorted_tool_calls(tool_calls) {
        if call.name.is_empty() {
            continue;
        }
        let event = ModelEvent::ToolCall(ModelToolCall {
            id: if call.id.is_empty() {
                format!("tool_call_{}", stable_hash(&call.name))
            } else {
                call.id
            },
            name: call.name,
            arguments: parse_tool_arguments(&call.arguments),
        });
        if let ModelEvent::ToolCall(tool_call) = &event {
            final_response.tool_calls.push(tool_call.clone());
        }
        on_event(event);
    }

    on_event(ModelEvent::Done);
    Ok(final_response)
}

fn collect_openai_chat_tool_deltas(
    value: &Value,
    tool_calls: &mut HashMap<usize, StreamingToolCall>,
) {
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return;
    };
    let Some(deltas) = choice
        .get("delta")
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for delta in deltas {
        let index = delta.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let entry = tool_calls.entry(index).or_default();
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            entry.id = id.to_string();
        }
        let function = delta.get("function").unwrap_or(&Value::Null);
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            entry.name.push_str(name);
        }
        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
            entry.arguments.push_str(arguments);
        }
    }
}

fn parse_anthropic_sse(
    response: reqwest::blocking::Response,
    on_event: &mut dyn FnMut(ModelEvent),
) -> Result<ModelResponse> {
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    let mut final_response = ModelResponse::default();
    let mut tool_calls: HashMap<usize, StreamingToolCall> = HashMap::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data == "[DONE]" {
            break;
        }
        let value: Value = serde_json::from_str(data)?;
        if let Some(err) = model_response_error(&value) {
            return Err(err);
        }
        collect_anthropic_tool_deltas(&value, &mut tool_calls);
        for event in parse_anthropic_event(&value) {
            match &event {
                ModelEvent::TextDelta(delta) => final_response.content.push_str(delta),
                ModelEvent::ThinkingDelta(delta) => final_response.thinking.push_str(delta),
                ModelEvent::ToolCall(call) => final_response.tool_calls.push(call.clone()),
                ModelEvent::Usage(usage) => final_response.usage = usage.clone(),
                ModelEvent::Done => continue,
            }
            on_event(event);
        }
    }

    for (_, call) in sorted_tool_calls(tool_calls) {
        if call.name.is_empty() {
            continue;
        }
        let event = ModelEvent::ToolCall(ModelToolCall {
            id: if call.id.is_empty() {
                format!("tool_use_{}", stable_hash(&call.name))
            } else {
                call.id
            },
            name: call.name,
            arguments: parse_tool_arguments(&call.arguments),
        });
        if let ModelEvent::ToolCall(tool_call) = &event {
            final_response.tool_calls.push(tool_call.clone());
        }
        on_event(event);
    }

    on_event(ModelEvent::Done);
    Ok(final_response)
}

fn collect_anthropic_tool_deltas(
    value: &Value,
    tool_calls: &mut HashMap<usize, StreamingToolCall>,
) {
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "content_block_start" => {
            let block = value.get("content_block").unwrap_or(&Value::Null);
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let entry = tool_calls.entry(index).or_default();
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    entry.id = id.to_string();
                }
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    entry.name = name.to_string();
                }
                if let Some(input) = block.get("input").filter(|input| !input.is_null()) {
                    entry.arguments.push_str(&input.to_string());
                }
            }
        }
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            if delta.get("type").and_then(Value::as_str) == Some("input_json_delta") {
                if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                    tool_calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(partial);
                }
            }
        }
        _ => {}
    }
}

fn sorted_tool_calls(
    tool_calls: HashMap<usize, StreamingToolCall>,
) -> Vec<(usize, StreamingToolCall)> {
    let mut tool_calls = tool_calls.into_iter().collect::<Vec<_>>();
    tool_calls.sort_by_key(|(index, _)| *index);
    tool_calls
}

fn parse_json_response(
    response: reqwest::blocking::Response,
    on_event: &mut dyn FnMut(ModelEvent),
    parser: fn(&Value) -> Vec<ModelEvent>,
) -> Result<ModelResponse> {
    let value: Value = response
        .json()
        .map_err(|err| PwError::Message(format!("model json response parse failed: {err}")))?;
    if let Some(err) = model_response_error(&value) {
        return Err(err);
    }
    let mut final_response = ModelResponse::default();

    for event in parser(&value) {
        match &event {
            ModelEvent::TextDelta(delta) => final_response.content.push_str(delta),
            ModelEvent::ThinkingDelta(delta) => final_response.thinking.push_str(delta),
            ModelEvent::ToolCall(call) => final_response.tool_calls.push(call.clone()),
            ModelEvent::Usage(usage) => final_response.usage = usage.clone(),
            ModelEvent::Done => {}
        }
        on_event(event);
    }

    on_event(ModelEvent::Done);
    Ok(final_response)
}

fn model_response_error(value: &Value) -> Option<PwError> {
    let error = value.get("error")?;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty());
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty());
    let mut detail = String::new();
    if let Some(error_type) = error_type {
        detail.push_str(&format!(" type={error_type}"));
    }
    if let Some(code) = code {
        detail.push_str(&format!(" code={code}"));
    }
    Some(PwError::Message(format!(
        "model provider returned an error in the response stream: {message}{detail}"
    )))
}

fn parse_openai_responses_event(value: &Value) -> Vec<ModelEvent> {
    if value.get("type").is_none() {
        let mut events = Vec::new();
        if let Some(usage) = value.get("usage") {
            events.push(ModelEvent::Usage(openai_usage(usage)));
        }
        if let Some(output) = value.get("output").and_then(Value::as_array) {
            for item in output {
                if let Some(call) = openai_responses_tool_call(item) {
                    events.push(ModelEvent::ToolCall(call));
                }
                if item.get("type").and_then(Value::as_str) == Some("message") {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                events.push(ModelEvent::TextDelta(text.to_string()));
                            }
                        }
                    }
                }
            }
        }
        events.push(ModelEvent::Done);
        return events;
    }

    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "response.output_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| vec![ModelEvent::TextDelta(delta.to_string())])
            .unwrap_or_default(),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| vec![ModelEvent::ThinkingDelta(delta.to_string())])
            .unwrap_or_default(),
        "response.completed" => value
            .get("response")
            .and_then(|response| response.get("usage"))
            .map(openai_usage)
            .map(|usage| vec![ModelEvent::Usage(usage), ModelEvent::Done])
            .unwrap_or_else(|| vec![ModelEvent::Done]),
        "response.output_item.done" => value
            .get("item")
            .and_then(openai_responses_tool_call)
            .map(|call| vec![ModelEvent::ToolCall(call)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_openai_chat_event(value: &Value) -> Vec<ModelEvent> {
    let mut events = Vec::new();

    if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
        events.push(ModelEvent::Usage(openai_usage(usage)));
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return events;
    };

    let delta = choice.get("delta").unwrap_or(&Value::Null);
    let message = choice.get("message").unwrap_or(&Value::Null);
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            if let Some(call) = openai_chat_tool_call(tool_call) {
                events.push(ModelEvent::ToolCall(call));
            }
        }
    }

    if let Some(thinking) = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .or_else(|| delta.get("thinking"))
        .or_else(|| message.get("reasoning_content"))
        .or_else(|| message.get("reasoning"))
        .or_else(|| message.get("thinking"))
        .and_then(Value::as_str)
    {
        events.push(ModelEvent::ThinkingDelta(thinking.to_string()));
    }

    if let Some(content) = delta
        .get("content")
        .or_else(|| message.get("content"))
        .and_then(Value::as_str)
    {
        events.push(ModelEvent::TextDelta(content.to_string()));
    }

    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some()
    {
        events.push(ModelEvent::Done);
    }

    events
}

fn openai_chat_tool_call(value: &Value) -> Option<ModelToolCall> {
    let function = value.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?.to_string();
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .map(parse_tool_arguments)
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ModelToolCall {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("tool_call_{}", stable_hash(&name))),
        name,
        arguments,
    })
}

fn openai_responses_tool_call(value: &Value) -> Option<ModelToolCall> {
    if value.get("type").and_then(Value::as_str)? != "function_call" {
        return None;
    }
    let name = value.get("name").and_then(Value::as_str)?.to_string();
    let arguments = value
        .get("arguments")
        .and_then(Value::as_str)
        .map(parse_tool_arguments)
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ModelToolCall {
        id: value
            .get("call_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("function_call_{}", stable_hash(&name))),
        name,
        arguments,
    })
}

fn anthropic_tool_call(value: &Value) -> Option<ModelToolCall> {
    let name = value.get("name").and_then(Value::as_str)?.to_string();
    Some(ModelToolCall {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("tool_use_{}", stable_hash(&name))),
        name,
        arguments: value
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
    })
}

fn parse_tool_arguments(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({ "raw": raw }))
}

fn stable_hash(value: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn parse_anthropic_json_event(value: &Value) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    if let Some(usage) = value.get("usage") {
        events.push(ModelEvent::Usage(anthropic_usage(usage)));
    }
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        events.push(ModelEvent::TextDelta(text.to_string()));
                    }
                }
                "thinking" => {
                    if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                        events.push(ModelEvent::ThinkingDelta(thinking.to_string()));
                    }
                }
                "tool_use" => {
                    if let Some(call) = anthropic_tool_call(block) {
                        events.push(ModelEvent::ToolCall(call));
                    }
                }
                _ => {}
            }
        }
    }
    events.push(ModelEvent::Done);
    events
}

fn parse_anthropic_event(value: &Value) -> Vec<ModelEvent> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            match delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text_delta" => delta
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| vec![ModelEvent::TextDelta(text.to_string())])
                    .unwrap_or_default(),
                "thinking_delta" => delta
                    .get("thinking")
                    .and_then(Value::as_str)
                    .map(|thinking| vec![ModelEvent::ThinkingDelta(thinking.to_string())])
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        "message_delta" => value
            .get("usage")
            .map(anthropic_usage)
            .map(|usage| vec![ModelEvent::Usage(usage)])
            .unwrap_or_default(),
        "message_stop" => vec![ModelEvent::Done],
        _ => Vec::new(),
    }
}

fn openai_usage(value: &Value) -> ModelUsage {
    ModelUsage {
        input_tokens: value
            .get("input_tokens")
            .or_else(|| value.get("prompt_tokens"))
            .and_then(Value::as_u64),
        output_tokens: value
            .get("output_tokens")
            .or_else(|| value.get("completion_tokens"))
            .and_then(Value::as_u64),
    }
}

fn anthropic_usage(value: &Value) -> ModelUsage {
    ModelUsage {
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ModelRequest {
        ModelRequest {
            model: "test-model".to_string(),
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: "hello".to_string(),
                tool_call_id: None,
                tool_name: None,
                tool_calls: Vec::new(),
            }],
            system: Some("system".to_string()),
            thinking: ThinkingConfig {
                enabled: true,
                budget_tokens: Some(2048),
            },
            max_tokens: Some(123),
            stream: true,
            tools: Vec::new(),
        }
    }

    fn request_with_tools() -> ModelRequest {
        let mut request = request();
        request.tools = vec![ModelToolSpec {
            name: "builtin_echo".to_string(),
            description: "Echo test".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        }];
        request
    }

    fn request_with_tool_result() -> ModelRequest {
        let mut request = request_with_tools();
        request.messages.push(ModelMessage {
            role: ModelRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: vec![ModelToolCall {
                id: "call-1".to_string(),
                name: "builtin_echo".to_string(),
                arguments: json!({ "text": "hello" }),
            }],
        });
        request.messages.push(ModelMessage {
            role: ModelRole::Tool,
            content: "tool-output".to_string(),
            tool_call_id: Some("call-1".to_string()),
            tool_name: Some("builtin_echo".to_string()),
            tool_calls: Vec::new(),
        });
        request
    }

    #[test]
    fn openai_payload_enables_streaming_and_reasoning() {
        let payload = openai_responses_payload(&request(), &json!({}));
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["model"], "test-model");
        assert!(payload.get("reasoning").is_some());
        assert_eq!(payload["max_output_tokens"], 123);
    }

    #[test]
    fn openai_chat_payload_supports_compatible_extra_body() {
        let payload = openai_chat_completions_payload(
            &request(),
            &json!({
                "enable_thinking": true,
                "custom": "value"
            }),
        );
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["model"], "test-model");
        assert_eq!(payload["enable_thinking"], true);
        assert_eq!(payload["custom"], "value");
        assert_eq!(payload["max_tokens"], 123);
    }

    #[test]
    fn openai_chat_payload_includes_tool_schemas() {
        let payload = openai_chat_completions_payload(&request_with_tools(), &json!({}));
        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["function"]["name"], "builtin_echo");
        assert_eq!(payload["tool_choice"], "auto");
    }

    #[test]
    fn openai_chat_payload_uses_native_tool_result_messages() {
        let payload = openai_chat_completions_payload(&request_with_tool_result(), &json!({}));
        let messages = payload["messages"].as_array().unwrap();
        let assistant_message = messages
            .iter()
            .find(|message| message["role"] == "assistant" && message.get("tool_calls").is_some())
            .unwrap();
        assert_eq!(assistant_message["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            assistant_message["tool_calls"][0]["function"]["name"],
            "builtin_echo"
        );
        let tool_message = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(tool_message["tool_call_id"], "call-1");
        assert_eq!(tool_message["content"], "tool-output");
        assert!(tool_message.get("name").is_none());
    }

    #[test]
    fn openai_chat_payload_adds_tool_name_for_gemini_compatible_models() {
        let mut request = request_with_tool_result();
        request.model = "gemini-3-flash-agent".to_string();
        let payload = openai_chat_completions_payload(&request, &json!({}));
        let messages = payload["messages"].as_array().unwrap();
        let tool_message = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(tool_message["tool_call_id"], "call-1");
        assert_eq!(tool_message["name"], "builtin_echo");
    }

    #[test]
    fn openai_responses_payload_uses_function_call_output_items() {
        let payload = openai_responses_payload(&request_with_tool_result(), &json!({}));
        let input = payload["input"].as_array().unwrap();
        let tool_output = input
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(tool_output["call_id"], "call-1");
        assert_eq!(tool_output["output"], "tool-output");
    }

    #[test]
    fn anthropic_payload_includes_tool_schemas() {
        let payload = anthropic_payload(&request_with_tools(), &json!({}));
        assert_eq!(payload["tools"][0]["name"], "builtin_echo");
        assert_eq!(payload["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn anthropic_payload_uses_native_tool_result_blocks() {
        let payload = anthropic_payload(&request_with_tool_result(), &json!({}));
        let messages = payload["messages"].as_array().unwrap();
        let tool_result_message = messages
            .iter()
            .find(|message| message["content"][0]["type"] == "tool_result")
            .unwrap();
        assert_eq!(tool_result_message["role"], "user");
        assert_eq!(tool_result_message["content"][0]["tool_use_id"], "call-1");
        assert_eq!(tool_result_message["content"][0]["content"], "tool-output");
    }

    #[test]
    fn nvidia_defaults_are_openai_compatible_chat_payload() {
        let payload = openai_chat_completions_payload(
            &request(),
            &json!({
                "temperature": 1.0,
                "top_p": 0.95,
                "chat_template_kwargs": {
                    "thinking_mode": "disabled"
                }
            }),
        );
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["temperature"], 1.0);
        assert_eq!(payload["top_p"], 0.95);
        assert_eq!(payload["chat_template_kwargs"]["thinking_mode"], "disabled");
    }

    #[test]
    fn anthropic_payload_enables_streaming_and_thinking() {
        let payload = anthropic_payload(&request(), &json!({"metadata": {"user_id": "u1"}}));
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["model"], "test-model");
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["thinking"]["budget_tokens"], 2048);
        assert_eq!(payload["metadata"]["user_id"], "u1");
    }

    #[test]
    fn parses_openai_text_and_usage_events() {
        let text = parse_openai_responses_event(&json!({
            "type": "response.output_text.delta",
            "delta": "hello"
        }));
        assert!(matches!(&text[0], ModelEvent::TextDelta(delta) if delta == "hello"));

        let usage = parse_openai_responses_event(&json!({
            "type": "response.completed",
            "response": { "usage": { "input_tokens": 1, "output_tokens": 2 } }
        }));
        assert!(matches!(
            &usage[0],
            ModelEvent::Usage(ModelUsage {
                input_tokens: Some(1),
                output_tokens: Some(2)
            })
        ));
    }

    #[test]
    fn parses_openai_chat_text_thinking_and_usage_events() {
        let events = parse_openai_chat_event(&json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "think",
                    "content": "hello"
                },
                "finish_reason": null
            }],
            "usage": { "input_tokens": 3, "output_tokens": 4 }
        }));

        assert!(matches!(
            &events[0],
            ModelEvent::Usage(ModelUsage {
                input_tokens: Some(3),
                output_tokens: Some(4)
            })
        ));
        assert!(matches!(&events[1], ModelEvent::ThinkingDelta(delta) if delta == "think"));
        assert!(matches!(&events[2], ModelEvent::TextDelta(delta) if delta == "hello"));
    }

    #[test]
    fn parses_openai_chat_non_stream_message() {
        let events = parse_openai_chat_event(&json!({
            "choices": [{
                "message": {
                    "reasoning_content": "think",
                    "content": "hello"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "input_tokens": 3, "output_tokens": 4 }
        }));

        assert!(matches!(
            &events[0],
            ModelEvent::Usage(ModelUsage {
                input_tokens: Some(3),
                output_tokens: Some(4)
            })
        ));
        assert!(matches!(&events[1], ModelEvent::ThinkingDelta(delta) if delta == "think"));
        assert!(matches!(&events[2], ModelEvent::TextDelta(delta) if delta == "hello"));
        assert!(matches!(&events[3], ModelEvent::Done));
    }

    #[test]
    fn parses_openai_chat_tool_call() {
        let events = parse_openai_chat_event(&json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "builtin_echo",
                            "arguments": "{\"text\":\"hello\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        assert!(matches!(
            &events[0],
            ModelEvent::ToolCall(call)
                if call.id == "call-1"
                    && call.name == "builtin_echo"
                    && call.arguments["text"] == "hello"
        ));
    }

    #[test]
    fn max_token_fallback_uses_configured_tiers() {
        assert_eq!(
            max_token_fallback_attempts(Some(128_000)),
            vec![Some(128_000), Some(64_000), Some(32_000), Some(16_000)]
        );
        assert_eq!(
            max_token_fallback_attempts(Some(64_000)),
            vec![Some(64_000), Some(32_000), Some(16_000)]
        );
        assert_eq!(max_token_fallback_attempts(None), vec![None]);
    }

    #[test]
    fn max_token_fallback_retries_invalid_argument_errors() {
        let err = PwError::Message(
            "model provider returned an error in the response stream: Request contains an invalid argument. type=upstream_api_error code=400"
                .to_string(),
        );
        assert!(should_retry_with_lower_max_tokens(&err, Some(128_000)));
        assert!(!should_retry_with_lower_max_tokens(&err, Some(16_000)));
        assert!(!should_retry_with_lower_max_tokens(&err, None));
    }

    #[test]
    fn parses_openai_responses_function_call() {
        let events = parse_openai_responses_event(&json!({
            "output": [{
                "type": "function_call",
                "call_id": "call-1",
                "name": "builtin_echo",
                "arguments": "{\"text\":\"hello\"}"
            }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        }));

        assert!(events.iter().any(|event| matches!(
            event,
            ModelEvent::ToolCall(call)
                if call.id == "call-1"
                    && call.name == "builtin_echo"
                    && call.arguments["text"] == "hello"
        )));
    }

    #[test]
    fn parses_anthropic_text_and_thinking_events() {
        let text = parse_anthropic_event(&json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "hello" }
        }));
        assert!(matches!(&text[0], ModelEvent::TextDelta(delta) if delta == "hello"));

        let thinking = parse_anthropic_event(&json!({
            "type": "content_block_delta",
            "delta": { "type": "thinking_delta", "thinking": "think" }
        }));
        assert!(matches!(&thinking[0], ModelEvent::ThinkingDelta(delta) if delta == "think"));
    }

    #[test]
    fn parses_anthropic_tool_use() {
        let events = parse_anthropic_json_event(&json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_1",
                "name": "builtin_echo",
                "input": { "text": "hello" }
            }]
        }));

        assert!(matches!(
            &events[0],
            ModelEvent::ToolCall(call)
                if call.id == "toolu_1"
                    && call.name == "builtin_echo"
                    && call.arguments["text"] == "hello"
        ));
    }
}
