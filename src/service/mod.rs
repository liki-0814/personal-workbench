use crate::{
    audit::{
        format_audit_summary, read_audit_events, summarize_events, AuditEvent, AuditRecorder,
        JsonlAuditRecorder,
    },
    context::{ContextBuilder, ContextPack},
    graph::{
        GraphEvent, GraphEventSink, GraphExecutor, GraphMessage, GraphRunRequest, GraphRunServices,
        GraphStatus, GraphWorkflow, StreamingModelPlanner, WorkflowContext, WorkflowExecutor,
        WorkflowNode, WorkflowNodeKind, WorkflowNodeRunner, WorkflowPlanKind, WorkflowRunSummary,
        WorkflowStatus, WorkflowStepOutcome,
    },
    memory::{
        MemoryStore, SemanticFactDraft, SemanticHypothesisDraft, SemanticInferenceDraft,
        SemanticLogicChainDraft, SemanticMemoryExtraction,
    },
    policy::{DefaultPolicyGuard, PolicyDecision, PolicyGuard, UserApproval},
    runtime::{
        CompactScope, RuntimeTask, RuntimeTaskEvent, RuntimeTaskKind, RuntimeTaskManager,
        RuntimeTaskStatus, VerificationRecord,
    },
    session::SessionStore,
    settings::Settings,
    storage::WorkspacePaths,
    tools::{
        agent_cli::{build_runtime_task_spec, AgentCliArgs, AgentCliKind},
        builtin::BuiltinToolLoader,
        config::apply_tool_settings,
        health::build_tool_health_report,
        mcp::McpToolLoader,
        model::{
            AnyModelClient, ModelClient, ModelEvent, ModelMessage, ModelRequest, ModelRole,
            ThinkingConfig,
        },
        skills::{watcher::scan_skill_roots, SkillToolLoader},
        verification::{
            legacy_verification_report, verification_report_from_metadata,
            VerificationGateDecision, VerificationToolLoader,
        },
        ToolArtifact, ToolArtifactKind, ToolArtifactProvenance, ToolCall, ToolExecutionContext,
        ToolExecutionRuntime, ToolLoader, ToolRegistry, ToolRegistrySnapshot, ToolResult,
    },
    PwError, Result as PwResult,
};
use async_stream::stream;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::Infallible,
    fs,
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

const EVENT_BACKLOG_LIMIT: usize = 1000;
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8791;
const PERSONAL_TASK_PARSE_SYSTEM_PROMPT: &str = r#"你是个人任务输入解析器。用户会描述一件要做的事，你必须返回一个 JSON 对象:
{
  "title": "简洁任务标题",
  "notes": "可选备注，说明背景/目标/产出",
  "priority": "high|medium|low",
  "dueDate": "YYYY-MM-DD，可选",
  "scheduledStart": "HH:mm，可选",
  "scheduledEnd": "HH:mm，可选",
  "subTasks": [{ "title": "具体可执行子步骤" }]
}
规则:
1. 不要输出 JSON 以外的内容。
2. title 必须简洁直接。
3. priority 缺省用 medium。
4. dueDate 只有用户提到日期/相对日期时填写。
5. scheduledStart/scheduledEnd 只有用户提到具体日内时间时填写。
6. subTasks 3-7 个为宜；如果任务很简单可以为空数组。"#;

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub host: String,
    pub port: u16,
    pub open: bool,
    pub no_ui: bool,
    pub reload_skills: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            open: false,
            no_ui: false,
            reload_skills: true,
        }
    }
}

#[derive(Clone)]
pub struct ServiceRuntime {
    inner: Arc<ServiceState>,
}

struct ServiceState {
    settings: Mutex<Settings>,
    runtime_tasks: RuntimeTaskManager,
    registry: Mutex<RegistryState>,
    events: EventBus,
    approvals: Mutex<HashMap<String, ApprovalTicket>>,
}

struct ApprovalTicket {
    sender: mpsc::Sender<bool>,
    run_id: Option<String>,
    task_id: Option<String>,
}

#[derive(Clone)]
struct RegistryState {
    snapshot: ToolRegistrySnapshot,
    loaded_skills: usize,
    refreshed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEventEnvelope {
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Clone)]
struct EventBus {
    next_seq: Arc<AtomicU64>,
    tx: broadcast::Sender<ServiceEventEnvelope>,
    backlog: Arc<Mutex<VecDeque<ServiceEventEnvelope>>>,
}

impl EventBus {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_BACKLOG_LIMIT);
        Self {
            next_seq: Arc::new(AtomicU64::new(1)),
            tx,
            backlog: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn emit(
        &self,
        kind: impl Into<String>,
        run_id: Option<String>,
        task_id: Option<String>,
        data: Value,
    ) -> ServiceEventEnvelope {
        let event = ServiceEventEnvelope {
            seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            ts: Utc::now().to_rfc3339(),
            kind: kind.into(),
            run_id,
            task_id,
            data,
        };
        let mut backlog = self
            .backlog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        backlog.push_back(event.clone());
        while backlog.len() > EVENT_BACKLOG_LIMIT {
            backlog.pop_front();
        }
        drop(backlog);
        let _ = self.tx.send(event.clone());
        event
    }

    fn subscribe(&self) -> broadcast::Receiver<ServiceEventEnvelope> {
        self.tx.subscribe()
    }

    fn since(&self, cursor: u64) -> Vec<ServiceEventEnvelope> {
        self.backlog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|event| event.seq > cursor)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    provider: String,
    model: String,
    thinking: bool,
    show_thinking: bool,
    model_max_input_tokens: u32,
    model_max_output_tokens: u32,
    providers: Vec<ConfiguredProvider>,
    registry_version: u64,
    tool_count: usize,
    loaded_skills: usize,
    refreshed_at: String,
    task_count: usize,
    run_profile: Value,
    agent_profiles: Value,
    ssh_hosts: Value,
    health_counts: Value,
}

#[derive(Debug, Serialize)]
struct ConfiguredProvider {
    name: String,
    protocol: String,
    base_url: String,
    api_key_env: Option<String>,
    api_key_configured: bool,
    request_timeout_seconds: u64,
    stream: bool,
    extra_body: Value,
    models: Vec<ConfiguredModel>,
}

#[derive(Debug, Serialize)]
struct ConfiguredModel {
    name: String,
    supports_image_input: bool,
    supports_thinking: bool,
    is_image_generation: bool,
    max_input_tokens: u32,
    max_output_tokens: u32,
    extra_body: Value,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    cursor: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ChatRunRequestBody {
    prompt: String,
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowPlanRequestBody {
    goal: String,
    agent: Option<String>,
    kind: Option<WorkflowPlanKind>,
}

#[derive(Debug, Serialize)]
struct WorkflowPlanResponse {
    requested_kind: WorkflowPlanKind,
    resolved_kind: WorkflowPlanKind,
    workflow: GraphWorkflow,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunRequestBody {
    goal: String,
    agent: Option<String>,
    kind: Option<WorkflowPlanKind>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    auto_approve: bool,
}

#[derive(Debug, Serialize)]
struct WorkflowRunCreated {
    run_id: String,
    task_id: String,
    requested_kind: WorkflowPlanKind,
    resolved_kind: WorkflowPlanKind,
    workflow: GraphWorkflow,
}

#[derive(Debug, Serialize)]
struct ChatRunCreated {
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct ApprovalBody {
    approved: bool,
}

#[derive(Debug, Deserialize)]
struct ConfigSwitchBody {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<bool>,
    show_thinking: Option<bool>,
    context_max_input_tokens: Option<u32>,
    model_max_input_tokens: Option<u32>,
    model_max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ToolCallBody {
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    auto_approve: bool,
}

#[derive(Debug, Deserialize)]
struct VerifyBody {
    cwd: Option<PathBuf>,
    #[serde(default)]
    commands: Vec<String>,
    timeout_seconds: Option<u64>,
    max_output_chars: Option<usize>,
    #[serde(default)]
    auto_approve: bool,
}

#[derive(Debug, Deserialize)]
struct CreateTaskBody {
    title: String,
    cwd: Option<PathBuf>,
    kind: Option<RuntimeTaskKind>,
    #[serde(default)]
    metadata: Value,
    #[serde(default = "default_true")]
    active: bool,
}

#[derive(Debug, Deserialize)]
struct TaskListQuery {
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionFolderBody {
    name: String,
}

#[derive(Debug, Deserialize)]
struct AssignSessionFolderBody {
    folder_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskEventsQuery {
    offset: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TaskLogQuery {
    stream: Option<String>,
    tail_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TaskCompactBody {
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskDecomposeBody {
    goal: Option<String>,
    #[serde(default)]
    kind: Option<WorkflowPlanKind>,
    agent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskParserBody {
    input: String,
}

#[derive(Debug, Clone, Serialize)]
struct TaskDecomposition {
    goal: String,
    requested_kind: WorkflowPlanKind,
    resolved_kind: WorkflowPlanKind,
    workflow_name: String,
    node_count: usize,
    edge_count: usize,
    generated_at: u64,
    steps: Vec<TaskDecompositionStep>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskDecompositionStep {
    id: String,
    label: String,
    kind: String,
    summary: String,
    to: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RuleBody {
    text: String,
}

#[derive(Debug, Deserialize)]
struct MemorySearchQuery {
    q: Option<String>,
    limit: Option<usize>,
}

fn default_true() -> bool {
    true
}

fn is_user_created_task(task: &RuntimeTask) -> bool {
    task.kind == RuntimeTaskKind::Internal
        || task
            .metadata
            .get("user_task")
            .and_then(Value::as_object)
            .is_some()
}

fn default_user_task_metadata() -> Value {
    json!({
        "schema_version": 1,
        "source": "web",
        "type": "personal_task",
        "status": "todo",
        "priority": "normal",
        "steps": []
    })
}

fn ensure_user_task_metadata(metadata: Value, kind: RuntimeTaskKind) -> Value {
    if kind != RuntimeTaskKind::Internal {
        return metadata;
    }
    match metadata {
        Value::Object(mut map) => {
            map.entry("user_task".to_string())
                .or_insert_with(default_user_task_metadata);
            Value::Object(map)
        }
        other => json!({
            "user_task": default_user_task_metadata(),
            "payload": other,
        }),
    }
}

fn normalize_personal_task_parse(content: &str) -> PwResult<Value> {
    let json_text = extract_json_object(content)
        .ok_or_else(|| PwError::Message("task parser returned no JSON object".to_string()))?;
    let parsed: Value = serde_json::from_str(json_text)
        .map_err(|err| PwError::Message(format!("task parser returned invalid JSON: {err}")))?;
    let title = parsed
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PwError::Message("task parser returned no title".to_string()))?;
    let notes = parsed
        .get("notes")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let priority = match parsed.get("priority").and_then(Value::as_str) {
        Some("high") => "high",
        Some("low") => "low",
        _ => "medium",
    };
    let due_date = optional_pattern_string(&parsed, "dueDate", is_iso_date);
    let scheduled_start = optional_pattern_string(&parsed, "scheduledStart", is_hh_mm);
    let scheduled_end = optional_pattern_string(&parsed, "scheduledEnd", is_hh_mm);
    let sub_tasks = parsed
        .get("subTasks")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("title")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|title| !title.is_empty())
                        .map(|title| json!({ "title": title }))
                })
                .take(12)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({
        "title": title,
        "notes": notes,
        "priority": priority,
        "dueDate": due_date,
        "scheduledStart": scheduled_start,
        "scheduledEnd": scheduled_end,
        "subTasks": sub_tasks,
    }))
}

fn extract_json_object(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&content[start..=end])
}

fn optional_pattern_string(value: &Value, key: &str, valid: fn(&str) -> bool) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| valid(text))
        .map(ToString::to_string)
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, byte)| idx == 4 || idx == 7 || byte.is_ascii_digit())
}

fn is_hh_mm(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    if ![bytes[0], bytes[1], bytes[3], bytes[4]]
        .iter()
        .all(u8::is_ascii_digit)
    {
        return false;
    }
    let hour = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    let minute = (bytes[3] - b'0') * 10 + (bytes[4] - b'0');
    hour < 24 && minute < 60
}

fn graph_message_for_ui(index: usize, message: &GraphMessage) -> Value {
    match message {
        GraphMessage::User(content) => json!({
            "id": format!("history-{index}-user"),
            "role": "user",
            "content": content,
        }),
        GraphMessage::Assistant(content) => json!({
            "id": format!("history-{index}-assistant"),
            "role": "assistant",
            "content": content,
        }),
        GraphMessage::AssistantToolCalls { calls } => json!({
            "id": format!("history-{index}-assistant-tools"),
            "role": "tool",
            "content": format!("assistant requested {} tool call{}", calls.len(), if calls.len() == 1 { "" } else { "s" }),
            "tool_calls": calls,
            "is_error": false,
        }),
        GraphMessage::Tool {
            call_id,
            name,
            content,
            is_error,
        } => json!({
            "id": format!("history-{index}-tool"),
            "role": "tool",
            "content": content,
            "tool_call_id": call_id,
            "tool_name": name,
            "is_error": is_error,
        }),
        GraphMessage::System(content) => json!({
            "id": format!("history-{index}-system"),
            "role": "system",
            "content": content,
        }),
    }
}

fn config_for_ui(settings: &Settings) -> PwResult<Value> {
    let mut value = serde_json::to_value(settings)?;
    if let Value::Object(map) = &mut value {
        let available_agents = available_agent_ids(settings);
        map.insert(
            "providers".to_string(),
            Value::Array(
                settings
                    .providers
                    .iter()
                    .map(provider_for_ui)
                    .collect::<Vec<_>>(),
            ),
        );
        map.insert("mineru".to_string(), mineru_for_ui(settings));
        map.insert("anysearch".to_string(), anysearch_for_ui(settings));
        map.insert("github".to_string(), github_for_ui(settings));
        map.insert(
            "available_agents".to_string(),
            Value::Array(
                available_agents
                    .iter()
                    .map(|agent| json!({ "id": agent, "label": agent }))
                    .collect(),
            ),
        );
        map.insert(
            "agent_model_options".to_string(),
            agent_model_options_for_ui(settings, &available_agents),
        );
        map.insert(
            "skills".to_string(),
            json!({
                "roots": settings.skill_roots,
            }),
        );
        map.insert(
            "health".to_string(),
            serde_json::to_value(build_tool_health_report(settings))?,
        );
    }
    Ok(value)
}

fn provider_for_ui(provider: &crate::settings::ProviderSettings) -> Value {
    json!({
        "name": provider.name,
        "protocol": provider.protocol.as_str(),
        "base_url": provider.base_url,
        "api_key": provider.api_key.clone(),
        "api_key_env": provider.api_key_env,
        "api_key_configured": provider.api_key.as_deref().is_some_and(|key| !key.trim().is_empty()),
        "api": provider.api,
        "request_timeout_seconds": provider.request_timeout_seconds,
        "stream": provider.stream,
        "extra_body": provider.extra_body,
        "models": provider.models.iter().map(|model| {
            json!({
                "name": model.name,
                "supports_image_input": model.supports_image_input,
                "supports_thinking": model.supports_thinking,
                "is_image_generation": model.is_image_generation,
                "max_input_tokens": model.max_input_tokens,
                "max_output_tokens": model.max_output_tokens,
                "extra_body": model.extra_body,
            })
        }).collect::<Vec<_>>(),
    })
}

fn mineru_for_ui(settings: &Settings) -> Value {
    json!({
        "base_url": settings.mineru.base_url,
        "token": settings.mineru.token,
        "token_configured": settings.mineru.token.as_deref().is_some_and(|token| !token.trim().is_empty()),
        "request_timeout_seconds": settings.mineru.request_timeout_seconds,
    })
}

fn anysearch_for_ui(settings: &Settings) -> Value {
    json!({
        "endpoint": settings.anysearch.endpoint,
        "api_key": settings.anysearch.api_key,
        "api_key_configured": settings.anysearch.api_key.as_deref().is_some_and(|key| !key.trim().is_empty()),
        "request_timeout_seconds": settings.anysearch.request_timeout_seconds,
        "rate_limit": settings.anysearch.rate_limit,
    })
}

fn github_for_ui(settings: &Settings) -> Value {
    json!({
        "api_url": settings.github.api_url,
        "token": settings.github.token,
        "token_configured": settings.github.token.as_deref().is_some_and(|token| !token.trim().is_empty()),
        "request_timeout_seconds": settings.github.request_timeout_seconds,
    })
}

fn available_agent_ids(settings: &Settings) -> Vec<String> {
    ["codex", "claude", "agy", "qodercli"]
        .into_iter()
        .filter(|agent| agent_available_for_ui(settings, agent))
        .map(ToString::to_string)
        .collect()
}

fn agent_available_for_ui(settings: &Settings, agent: &str) -> bool {
    let profile = settings.agents.profiles.get(agent);
    let binary = profile
        .map(|profile| profile.binary.as_str())
        .unwrap_or(agent);
    if !binary_on_path_for_ui(binary) {
        return false;
    }
    match agent {
        "codex" => {
            settings.home_dir.join(".codex/auth.json").is_file()
                && settings.home_dir.join(".codex/models_cache.json").is_file()
        }
        "claude" => {
            settings.home_dir.join(".claude/settings.json").is_file()
                || settings
                    .home_dir
                    .join(".claude/settings.local.json")
                    .is_file()
        }
        "qodercli" => has_nonempty_auth_file(&settings.home_dir.join(".qoder/.auth"), "machine_id"),
        "agy" => {
            has_nonempty_auth_file(&settings.home_dir.join(".agy"), "")
                || has_nonempty_auth_file(&settings.home_dir.join(".config/agy"), "")
        }
        _ => false,
    }
}

fn has_nonempty_auth_file(dir: &Path, ignored_name: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(std::result::Result::ok).any(|entry| {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        name != ignored_name && path.is_file() && path.metadata().is_ok_and(|meta| meta.len() > 0)
    })
}

fn binary_on_path_for_ui(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn agent_model_options_for_ui(settings: &Settings, available_agents: &[String]) -> Value {
    let provider_models = settings
        .providers
        .iter()
        .flat_map(|provider| provider.models.iter().map(|model| model.name.clone()))
        .collect::<HashSet<_>>();
    let codex = codex_model_options(settings);
    let claude = claude_model_options(settings);
    let mut qoder = vec![
        "auto".to_string(),
        "sonnet".to_string(),
        "opus".to_string(),
        "gemini-3-flash-agent".to_string(),
        "claude-opus-4-6-thinking".to_string(),
    ];
    let mut agy = vec![
        "auto".to_string(),
        "gemini-3-flash-agent".to_string(),
        "gemini-pro-agent".to_string(),
        "claude-opus-4-6-thinking".to_string(),
    ];
    qoder.extend(provider_models.iter().cloned());
    agy.extend(provider_models);
    let mut map = serde_json::Map::new();
    for agent in available_agents {
        let options = match agent.as_str() {
            "codex" => unique_sorted_model_options(codex.clone()),
            "claude" => unique_sorted_model_options(claude.clone()),
            "qodercli" => unique_sorted_model_options(qoder.clone()),
            "agy" => unique_sorted_model_options(agy.clone()),
            _ => Vec::new(),
        };
        map.insert(agent.clone(), Value::Array(options));
    }
    Value::Object(map)
}

fn codex_model_options(settings: &Settings) -> Vec<String> {
    let path = settings.home_dir.join(".codex/models_cache.json");
    let Ok(text) = fs::read_to_string(path) else {
        return vec![
            "gpt-5.5".to_string(),
            "gpt-5.4".to_string(),
            "gpt-5.4-mini".to_string(),
            "gpt-5.3-codex-spark".to_string(),
        ];
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("slug")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .or_else(|| item.get("name").and_then(Value::as_str))
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn claude_model_options(settings: &Settings) -> Vec<String> {
    let mut out = vec![
        "haiku".to_string(),
        "sonnet".to_string(),
        "opus".to_string(),
        "fable".to_string(),
        "claude-opus-4-6-thinking".to_string(),
    ];
    for path in [
        settings.home_dir.join(".claude/settings.json"),
        settings.home_dir.join(".claude/settings.local.json"),
    ] {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            out.push(model.to_string());
        }
        if let Some(env) = value.get("env").and_then(Value::as_object) {
            for (key, model) in env {
                if key.contains("MODEL") {
                    if let Some(model) = model.as_str() {
                        out.push(model.trim_end_matches("[1M]").to_string());
                        out.push(model.to_string());
                    }
                }
            }
        }
    }
    out
}

fn unique_sorted_model_options(values: Vec<String>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect::<Vec<_>>();
    out.sort_by_key(|value| (value == "auto", value.clone()));
    out.into_iter()
        .map(|value| json!({ "value": value, "label": value }))
        .collect()
}

fn preserve_secret_fields(current: &Settings, next: &mut Settings) {
    if next.mineru.token.is_none() {
        next.mineru.token = current.mineru.token.clone();
    }
    if next.anysearch.api_key.is_none() {
        next.anysearch.api_key = current.anysearch.api_key.clone();
    }
    if next.github.token.is_none() {
        next.github.token = current.github.token.clone();
    }
    for provider in &mut next.providers {
        if provider.api_key.is_none() {
            provider.api_key = current
                .providers
                .iter()
                .find(|existing| existing.name == provider.name)
                .and_then(|existing| existing.api_key.clone());
        }
    }
}

fn normalize_ui_secret_fields(settings: &mut Settings) {
    normalize_secret(&mut settings.mineru.token);
    normalize_secret(&mut settings.anysearch.api_key);
    normalize_secret(&mut settings.github.token);
    for provider in &mut settings.providers {
        normalize_secret(&mut provider.api_key);
    }
}

fn normalize_secret(secret: &mut Option<String>) {
    if secret
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        *secret = None;
    }
}

fn agent_profiles_for_ui(settings: &Settings) -> Value {
    Value::Object(
        settings
            .agents
            .profiles
            .iter()
            .map(|(agent, profile)| {
                (
                    agent.clone(),
                    json!({
                        "enabled": profile.enabled,
                        "binary": profile.binary,
                        "model": profile.model,
                        "effort": profile.effort,
                        "timeout_seconds": profile.timeout_seconds,
                        "background": profile.background,
                    }),
                )
            })
            .collect(),
    )
}

fn ssh_hosts_for_ui(settings: &Settings) -> Value {
    Value::Array(
        settings
            .ssh
            .hosts
            .iter()
            .map(|host| {
                json!({
                    "name": host.name,
                    "host": host.host,
                    "port": host.port,
                    "username": host.username,
                    "has_private_key_path": host.private_key_path.is_some(),
                    "has_password_env": host.password_env.is_some(),
                    "default_cwd": host.default_cwd,
                    "timeout_seconds": host.timeout_seconds,
                    "accept_unknown_host_key": host.accept_unknown_host_key,
                    "learn_unknown_host_key": host.learn_unknown_host_key,
                })
            })
            .collect(),
    )
}

fn build_run_profile_for_status(
    settings: &Settings,
    route: &str,
    explicit_agent: Option<&str>,
) -> Value {
    let resolved_route = if route == "auto" {
        settings.workflow.default_kind.as_str()
    } else {
        route
    };
    let agent = explicit_agent
        .map(ToString::to_string)
        .unwrap_or_else(|| settings.agent_for_route(resolved_route));
    let profile = settings.agent_profile(&agent);
    let model = profile.and_then(|profile| profile.model.clone());
    json!({
        "route": resolved_route,
        "provider": settings.provider,
        "model": settings.model,
        "context_max_input_tokens": settings.context.max_input_tokens,
        "thinking": settings.thinking,
        "show_thinking": settings.show_thinking,
        "stream": settings.resolved_model_settings().map(|model| model.stream).unwrap_or(false),
        "agent": agent,
        "agent_model": model,
        "agent_effort": profile.map(|profile| profile.effort.clone()).unwrap_or_else(|| "high".to_string()),
        "agent_timeout_seconds": profile.map(|profile| profile.timeout_seconds).unwrap_or(900),
        "agent_background": profile.map(|profile| profile.background).unwrap_or(false),
        "ssh_default_host": settings.ssh.hosts.first().map(|host| host.name.clone()),
        "profile_source": if explicit_agent.is_some() { "request" } else { "settings" },
    })
}

fn workflow_agent_args_from_settings(
    settings: &Settings,
    agent: &str,
    mode: &str,
    prompt: String,
    cwd: PathBuf,
) -> AgentCliArgs {
    let profile = settings.agent_profile(agent);
    let mode_override = profile.and_then(|profile| profile.mode_overrides.get(mode));
    let mut extra_args = profile
        .map(|profile| profile.extra_args.clone())
        .unwrap_or_default();
    if let Some(mode_override) = mode_override {
        extra_args.extend(mode_override.extra_args.clone());
    }
    AgentCliArgs {
        prompt,
        mode: mode.to_string(),
        cwd: Some(cwd),
        model: mode_override
            .and_then(|override_settings| override_settings.model.clone())
            .or_else(|| profile.and_then(|profile| profile.model.clone())),
        session_id: None,
        effort: mode_override
            .and_then(|override_settings| override_settings.effort.clone())
            .or_else(|| profile.map(|profile| profile.effort.clone()))
            .unwrap_or_else(|| "high".to_string()),
        yolo: mode_override
            .and_then(|override_settings| override_settings.yolo)
            .unwrap_or(mode == "execute"),
        background: profile.map(|profile| profile.background).unwrap_or(false),
        timeout_seconds: mode_override
            .and_then(|override_settings| override_settings.timeout_seconds)
            .or_else(|| profile.map(|profile| profile.timeout_seconds))
            .unwrap_or(900)
            .max(1),
        extra_args,
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl From<PwError> for ApiError {
    fn from(value: PwError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message
            })),
        )
            .into_response()
    }
}

impl ServiceRuntime {
    pub fn new(settings: Settings) -> PwResult<Self> {
        WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
        let runtime_tasks = RuntimeTaskManager::new(settings.pwcli_home.clone());
        runtime_tasks.ensure()?;
        let (snapshot, loaded_skills) = build_service_registry(&settings)?;
        let runtime = Self {
            inner: Arc::new(ServiceState {
                settings: Mutex::new(settings),
                runtime_tasks,
                registry: Mutex::new(RegistryState {
                    snapshot,
                    loaded_skills,
                    refreshed_at: Utc::now().to_rfc3339(),
                }),
                events: EventBus::new(),
                approvals: Mutex::new(HashMap::new()),
            }),
        };
        runtime.start_task_event_forwarder();
        Ok(runtime)
    }

    pub fn settings(&self) -> Settings {
        self.inner
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn parse_personal_task(&self, input: String) -> PwResult<Value> {
        let settings = self.settings();
        let model_settings = settings.resolved_model_settings()?;
        let model_client = AnyModelClient::from_settings(&model_settings)?;
        let now = Utc::now();
        let system = format!(
            "{}\n\n当前日期时间: {} UTC. 相对日期如今天、明天、下周一必须据此推断。只返回 JSON。",
            PERSONAL_TASK_PARSE_SYSTEM_PROMPT,
            now.format("%Y-%m-%d %H:%M")
        );
        let response = model_client.complete(&ModelRequest {
            model: model_settings.model.clone(),
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: input,
                tool_call_id: None,
                tool_name: None,
                tool_calls: Vec::new(),
            }],
            system: Some(system),
            thinking: ThinkingConfig {
                enabled: false,
                budget_tokens: None,
            },
            max_tokens: Some(model_settings.max_output_tokens.min(4096)),
            stream: false,
            tools: Vec::new(),
        })?;
        normalize_personal_task_parse(&response.content)
    }

    fn postprocess_task_memory(
        &self,
        task_id: &str,
        summary: &WorkflowRunSummary,
    ) -> PwResult<Value> {
        let settings = self.settings();
        if !settings.memory.enabled || !settings.memory.semantic_extraction.enabled {
            let status = json!({
                "task_id": task_id,
                "enabled": false,
                "reason": "memory semantic extraction is disabled",
                "updated_at": Utc::now().to_rfc3339(),
                "papers": [],
            });
            write_memory_extraction_status(&self.inner.runtime_tasks, task_id, &status)?;
            return Ok(status);
        }

        let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
        let mut status = read_memory_extraction_status(&self.inner.runtime_tasks, task_id)
            .unwrap_or_else(|_| json!({ "papers": [] }));
        let already_done = status
            .get("papers")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        item.get("status").and_then(Value::as_str) == Some("accepted")
                            || item.get("status").and_then(Value::as_str) == Some("skipped")
                    })
                    .filter_map(|item| item.get("artifact_id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();

        let _ = summary;
        ensure_task_material_archive(&self.inner.runtime_tasks, task_id)?;
        let papers = list_mineru_materials(&self.inner.runtime_tasks, task_id)?;
        let mut paper_statuses = Vec::new();
        let mut accepted_fact_count = 0usize;
        let mut accepted_inference_count = 0usize;
        let mut accepted_hypothesis_count = 0usize;

        for paper in papers {
            let artifact_id = paper
                .get("artifact_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if artifact_id.is_empty() {
                continue;
            }
            if already_done.contains(&artifact_id) {
                paper_statuses.push(json!({
                    "artifact_id": artifact_id,
                    "title": paper.get("canonical_title").cloned().unwrap_or(Value::Null),
                    "status": "skipped",
                    "reason": "already postprocessed for this task",
                }));
                continue;
            }
            let evidence_level = paper
                .get("evidence_level")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if evidence_level != "full_pdf_mineru" {
                paper_statuses.push(json!({
                    "artifact_id": artifact_id,
                    "title": paper.get("canonical_title").cloned().unwrap_or(Value::Null),
                    "status": "skipped",
                    "reason": "not a full_pdf_mineru artifact",
                    "evidence_level": evidence_level,
                }));
                continue;
            }
            let document_path = paper
                .pointer("/artifact_paths/document_md")
                .and_then(Value::as_str)
                .or_else(|| paper.get("document_path").and_then(Value::as_str))
                .unwrap_or_default();
            if document_path.is_empty() {
                paper_statuses.push(json!({
                    "artifact_id": artifact_id,
                    "title": paper.get("canonical_title").cloned().unwrap_or(Value::Null),
                    "status": "failed",
                    "error": "artifact has no document markdown path",
                }));
                continue;
            }
            let markdown = match fs::read_to_string(document_path) {
                Ok(markdown) => markdown,
                Err(err) => {
                    paper_statuses.push(json!({
                        "artifact_id": artifact_id,
                        "title": paper.get("canonical_title").cloned().unwrap_or(Value::Null),
                        "status": "failed",
                        "error": format!("failed to read markdown: {err}"),
                    }));
                    continue;
                }
            };
            let inferred_title = infer_title_from_text(&markdown);
            let title = paper
                .get("canonical_title")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .or(inferred_title.as_deref())
                .unwrap_or("Untitled paper")
                .to_string();
            let extraction = match extract_paper_memory_with_model(
                &settings,
                task_id,
                &artifact_id,
                &title,
                &markdown,
            ) {
                Ok(Some(extraction)) => extraction,
                Ok(None) => {
                    paper_statuses.push(json!({
                        "artifact_id": artifact_id,
                        "title": title,
                        "status": "skipped",
                        "reason": "model returned no durable source-backed memory",
                    }));
                    continue;
                }
                Err(err) => {
                    paper_statuses.push(json!({
                        "artifact_id": artifact_id,
                        "title": title,
                        "status": "failed",
                        "error": err.to_string(),
                    }));
                    continue;
                }
            };
            let fact_count = extraction.facts.len();
            let inference_count = extraction.inferences.len();
            let hypothesis_count = extraction.hypotheses.len();
            let source = format!(
                "research paper '{}' artifact={} task={} path={}",
                title, artifact_id, task_id, document_path
            );
            match store.generate_candidate_from_semantic_extraction(extraction, source) {
                Ok(Some(candidate)) => {
                    let candidate_id = candidate.id.clone();
                    store.add_candidate(&candidate)?;
                    if store.get_candidate(&candidate_id)?.is_some() {
                        let facts = store.accept_candidate(&candidate_id)?;
                        accepted_fact_count += facts.len();
                        accepted_inference_count += inference_count;
                        accepted_hypothesis_count += hypothesis_count;
                        paper_statuses.push(json!({
                            "artifact_id": artifact_id,
                            "title": title,
                            "status": "accepted",
                            "candidate_id": candidate_id,
                            "facts": facts,
                            "fact_count": fact_count,
                            "accepted_fact_count": facts.len(),
                            "inference_count": inference_count,
                            "hypothesis_count": hypothesis_count,
                            "document_path": document_path,
                        }));
                    } else {
                        paper_statuses.push(json!({
                            "artifact_id": artifact_id,
                            "title": title,
                            "status": "skipped",
                            "reason": "candidate was redundant or already decided",
                            "fact_count": fact_count,
                            "inference_count": inference_count,
                            "hypothesis_count": hypothesis_count,
                        }));
                    }
                }
                Ok(None) => {
                    paper_statuses.push(json!({
                        "artifact_id": artifact_id,
                        "title": title,
                        "status": "skipped",
                        "reason": "candidate review ignored low-value or duplicate extraction",
                    }));
                }
                Err(err) => {
                    paper_statuses.push(json!({
                        "artifact_id": artifact_id,
                        "title": title,
                        "status": "failed",
                        "error": err.to_string(),
                    }));
                }
            }
        }

        status = json!({
            "task_id": task_id,
            "enabled": true,
            "updated_at": Utc::now().to_rfc3339(),
            "paper_count": paper_statuses.len(),
            "accepted_fact_count": accepted_fact_count,
            "accepted_inference_count": accepted_inference_count,
            "accepted_hypothesis_count": accepted_hypothesis_count,
            "papers": paper_statuses,
        });
        write_memory_extraction_status(&self.inner.runtime_tasks, task_id, &status)?;
        Ok(status)
    }

    fn maybe_update_user_preference_hypotheses(
        &self,
        run_id: &str,
        user_input: &str,
        assistant_output: &str,
    ) -> PwResult<()> {
        let settings = self.settings();
        if !settings.memory.enabled || !is_high_signal_preference_turn(user_input) {
            return Ok(());
        }
        let mut state = read_user_preference_state(&settings).unwrap_or_else(|_| {
            json!({
                "namespace": "user_preferences",
                "high_signal_count": 0,
                "hypotheses": [],
                "supporting_conversations": []
            })
        });
        let count = state
            .get("high_signal_count")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            + 1;
        state["high_signal_count"] = json!(count);
        push_json_array_limited(
            &mut state,
            "supporting_conversations",
            json!({
                "run_id": run_id,
                "at": Utc::now().to_rfc3339(),
                "user": preview_text(user_input, 700),
                "assistant": preview_text(assistant_output, 500),
            }),
            30,
        );
        if count <= 3 || count % 5 == 0 {
            let inferred = infer_user_preference_hypotheses(user_input);
            for hypothesis in inferred {
                upsert_user_preference_hypothesis(&mut state, hypothesis, run_id);
            }
            state["last_updated_at"] = json!(Utc::now().to_rfc3339());
        }
        write_user_preference_state(&settings, &state)
    }

    pub fn snapshot(&self) -> ToolRegistrySnapshot {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot
            .clone()
    }

    pub fn refresh_registry(&self) -> PwResult<()> {
        let settings = self.settings();
        let (snapshot, loaded_skills) = build_service_registry(&settings)?;
        let version = snapshot.version();
        let tool_count = snapshot.descriptors().len();
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *registry = RegistryState {
            snapshot,
            loaded_skills,
            refreshed_at: Utc::now().to_rfc3339(),
        };
        drop(registry);
        self.emit(
            "registry_updated",
            None,
            None,
            json!({
                "registry_version": version,
                "tool_count": tool_count,
                "loaded_skills": loaded_skills,
            }),
        );
        Ok(())
    }

    fn start_task_event_forwarder(&self) {
        let manager = self.inner.runtime_tasks.clone();
        let events = self.inner.events.clone();
        thread::spawn(move || loop {
            for event in manager.poll_events() {
                let task_id = runtime_event_task_id(&event);
                events.emit(
                    "task_event",
                    None,
                    Some(task_id.clone()),
                    json!({
                        "task_id": task_id,
                        "event": event,
                    }),
                );
            }
            thread::sleep(Duration::from_millis(250));
        });
    }

    pub fn start_skill_watcher(&self) {
        let runtime = self.clone();
        thread::spawn(move || {
            let roots = runtime.settings().skill_roots;
            let mut last_fingerprint = scan_skill_roots(&roots)
                .map(|inventory| inventory.fingerprint)
                .unwrap_or_default();
            loop {
                thread::sleep(Duration::from_secs(1));
                let settings = runtime.settings();
                match scan_skill_roots(&settings.skill_roots) {
                    Ok(inventory) => {
                        if inventory.fingerprint == last_fingerprint {
                            continue;
                        }
                        last_fingerprint = inventory.fingerprint;
                        match runtime.refresh_registry() {
                            Ok(()) => runtime.emit(
                                "skills_reloaded",
                                None,
                                None,
                                json!({
                                    "fingerprint": inventory.fingerprint,
                                    "tool_ids": inventory.tool_ids,
                                    "conflicts": inventory.conflicts,
                                    "health": inventory.health,
                                }),
                            ),
                            Err(err) => runtime.emit(
                                "skills_reload_failed",
                                None,
                                None,
                                json!({ "error": err.to_string() }),
                            ),
                        }
                    }
                    Err(err) => runtime.emit(
                        "skills_watch_failed",
                        None,
                        None,
                        json!({ "error": err.to_string() }),
                    ),
                }
            }
        });
    }

    fn status(&self) -> PwResult<StatusResponse> {
        let settings = self.settings();
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let active_model = settings
            .active_model()
            .ok()
            .map(|model| (model.max_input_tokens, model.max_output_tokens));
        let providers = settings
            .providers
            .iter()
            .map(|provider| ConfiguredProvider {
                name: provider.name.clone(),
                protocol: provider.protocol.as_str().to_string(),
                base_url: provider.base_url.clone(),
                api_key_env: provider.api_key_env.clone(),
                api_key_configured: provider
                    .api_key
                    .as_deref()
                    .is_some_and(|key| !key.trim().is_empty()),
                request_timeout_seconds: provider.request_timeout_seconds,
                stream: provider.stream,
                extra_body: provider.extra_body.clone(),
                models: provider
                    .models
                    .iter()
                    .map(|model| ConfiguredModel {
                        name: model.name.clone(),
                        supports_image_input: model.supports_image_input,
                        supports_thinking: model.supports_thinking,
                        is_image_generation: model.is_image_generation,
                        max_input_tokens: model.max_input_tokens,
                        max_output_tokens: model.max_output_tokens,
                        extra_body: model.extra_body.clone(),
                    })
                    .collect(),
            })
            .collect();
        let health = build_tool_health_report(&settings);
        let (health_ok, health_warn, health_fail, health_info) = health.counts();
        Ok(StatusResponse {
            provider: settings.provider.clone(),
            model: settings.model.clone(),
            thinking: settings.thinking,
            show_thinking: settings.show_thinking,
            model_max_input_tokens: active_model.map(|(input, _)| input).unwrap_or_default(),
            model_max_output_tokens: active_model.map(|(_, output)| output).unwrap_or_default(),
            providers,
            registry_version: registry.snapshot.version(),
            tool_count: registry.snapshot.descriptors().len(),
            loaded_skills: registry.loaded_skills,
            refreshed_at: registry.refreshed_at,
            task_count: self.inner.runtime_tasks.list()?.len(),
            run_profile: build_run_profile_for_status(&settings, "auto", None),
            agent_profiles: agent_profiles_for_ui(&settings),
            ssh_hosts: ssh_hosts_for_ui(&settings),
            health_counts: json!({
                "ok": health_ok,
                "warn": health_warn,
                "fail": health_fail,
                "info": health_info,
            }),
        })
    }

    fn emit(
        &self,
        kind: impl Into<String>,
        run_id: Option<String>,
        task_id: Option<String>,
        data: Value,
    ) {
        self.inner.events.emit(kind, run_id, task_id, data);
    }

    fn start_chat_run(&self, body: ChatRunRequestBody) -> PwResult<ChatRunCreated> {
        let prompt = body.prompt.trim().to_string();
        if prompt.is_empty() {
            return Err(PwError::Message("prompt cannot be empty".to_string()));
        }
        let run_id = new_id("run");
        let service = self.clone();
        let session = body.session;
        self.emit(
            "run_started",
            Some(run_id.clone()),
            None,
            json!({ "prompt": prompt }),
        );
        let thread_run_id = run_id.clone();
        thread::spawn(move || {
            if let Err(err) = service.run_chat_thread(thread_run_id.clone(), prompt, session) {
                service.emit(
                    "run_failed",
                    Some(thread_run_id),
                    None,
                    json!({ "error": err.to_string() }),
                );
            }
        });
        Ok(ChatRunCreated { run_id })
    }

    fn run_chat_thread(
        &self,
        run_id: String,
        prompt: String,
        session: Option<String>,
    ) -> PwResult<()> {
        let settings = self.settings();
        WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
        if let Err(err) = self.refresh_registry() {
            self.emit(
                "registry_refresh_failed",
                Some(run_id.clone()),
                None,
                json!({ "error": err.to_string() }),
            );
        }
        let snapshot = self.snapshot();
        let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));
        let model_settings = settings.resolved_model_settings()?;
        audit.record(AuditEvent::RuntimeInitialized);
        audit.record(AuditEvent::ConfigLoaded {
            provider: model_settings.provider_name.clone(),
            model: model_settings.model.clone(),
        });
        audit.record(AuditEvent::ToolRegistryBuilt {
            registry_version: snapshot.version(),
            tool_count: snapshot.descriptors().len(),
        });
        let mut context_pack = ContextBuilder::new().build_with_sources_and_memory(
            prompt.clone(),
            &snapshot,
            Some(settings.pwcli_home.clone()),
            default_local_context_paths(),
            &settings.memory,
        );
        remove_agent_cli_tools_from_context_pack(&mut context_pack);
        audit.record(AuditEvent::ContextPackBuilt {
            context_id: context_pack.id.clone(),
            selected_tool_ids: context_pack.selected_tool_ids.clone(),
        });
        self.emit_context(&run_id, &context_pack);

        let policy = DefaultPolicyGuard::default().with_rules(load_rule_texts(&settings));
        let session_store = SessionStore::new(settings.pwcli_home.clone());
        let seed_messages = if let Some(selector) = session.as_deref() {
            session_store
                .get(selector)?
                .map(|record| record.seed_messages())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let model_client = AnyModelClient::from_settings(&model_settings)?;
        let show_thinking = model_settings.show_thinking;
        let event_runtime = self.clone();
        let event_run_id = run_id.clone();
        let mut planner = StreamingModelPlanner::new(
            &model_client,
            model_settings.model.clone(),
            ThinkingConfig {
                enabled: model_settings.thinking_enabled,
                budget_tokens: Some(1024),
            },
            move |event| {
                emit_model_event(&event_runtime, &event_run_id, event, show_thinking);
            },
        )
        .max_tokens(model_settings.max_output_tokens)
        .stream(model_settings.stream)
        .system(
            "You are pwcli, a local personal workbench agent runtime. The current request is \
             served through pwcli service/web frontend. Use selected tools only when useful and \
             respect policy interruptions.",
        );
        let graph = GraphExecutor::builder()
            .max_rounds(settings.max_rounds)
            .build();
        let mut graph_events = ServiceGraphEventSink {
            runtime: self.clone(),
            run_id: run_id.clone(),
        };
        let approval = ServiceApproval {
            runtime: self.clone(),
            run_id: Some(run_id.clone()),
            task_id: None,
        };
        let tool_context = ToolExecutionContext {
            runtime_tasks: Some(self.inner.runtime_tasks.clone()),
            ..ToolExecutionContext::default()
        };
        let mut services =
            GraphRunServices::new(&policy, &audit, Some(&approval), &mut graph_events)
                .with_tool_context(tool_context);
        let summary = graph.run_with_seed_messages_and_events(
            GraphRunRequest {
                user_input: prompt.clone(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &mut services,
            seed_messages,
        )?;
        let session_path = session_store.save(&run_id, &summary)?;
        audit.record(AuditEvent::SessionSaved {
            path: session_path.display().to_string(),
        });
        if let Err(err) = self.maybe_update_user_preference_hypotheses(
            &run_id,
            &prompt,
            &summary.state.last_content,
        ) {
            self.emit(
                "memory_preference_update_failed",
                Some(run_id.clone()),
                None,
                json!({ "error": err.to_string() }),
            );
        }
        self.emit(
            "run_completed",
            Some(run_id),
            None,
            json!({
                "session_id": session_path.file_stem().and_then(|stem| stem.to_str()).unwrap_or(""),
                "session_path": session_path,
                "status": summary.state.status,
                "round_count": summary.state.round_count,
                "content": summary.state.last_content,
            }),
        );
        Ok(())
    }

    fn start_workflow_run(&self, body: WorkflowRunRequestBody) -> PwResult<WorkflowRunCreated> {
        let goal = body.goal.trim().to_string();
        if goal.is_empty() {
            return Err(PwError::Message("goal cannot be empty".to_string()));
        }
        let requested_kind = body.kind.unwrap_or(WorkflowPlanKind::Auto);
        let resolved_kind = requested_kind.resolve(&goal);
        let settings = self.settings();
        let agent = body
            .agent
            .unwrap_or_else(|| settings.agent_for_route(resolved_kind.as_str()));
        if resolved_kind == WorkflowPlanKind::Code && AgentCliKind::from_id(&agent).is_none() {
            return Err(PwError::Message(format!(
                "workflow agent '{agent}' is not supported by local pwcli; expected codex, claude, agy, or qodercli"
            )));
        }
        if resolved_kind == WorkflowPlanKind::Code && !agent_available_for_ui(&settings, &agent) {
            return Err(PwError::Message(format!(
                "workflow agent '{agent}' is not available or not logged in on this machine"
            )));
        }
        let workflow = GraphWorkflow::planned(goal.clone(), agent.clone(), requested_kind);
        let cwd = match body.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir().map_err(PwError::from)?,
        };
        let task = self.inner.runtime_tasks.create_task(
            RuntimeTaskKind::Workflow,
            format!("Workflow: {}", preview_text(&goal, 80)),
            cwd,
            json!({
                "workflow": {
                    "goal": goal,
                    "agent": agent,
                    "name": workflow.name.clone(),
                    "requested_kind": requested_kind,
                    "resolved_kind": resolved_kind,
                    "auto_approve": body.auto_approve
                }
            }),
        )?;
        self.inner.runtime_tasks.set_active(&task.task_id)?;
        persist_workflow_artifacts(&self.inner.runtime_tasks, &task.task_id, &workflow, None)?;
        self.emit(
            "workflow_run_started",
            Some(task.task_id.clone()),
            Some(task.task_id.clone()),
            json!({
                "task_id": task.task_id.clone(),
                "requested_kind": requested_kind,
                "resolved_kind": resolved_kind,
                "workflow": compact_workflow_for_ui(workflow.clone()),
            }),
        );

        let service = self.clone();
        let thread_task_id = task.task_id.clone();
        let thread_workflow = workflow.clone();
        thread::spawn(move || {
            if let Err(err) = service.run_workflow_thread(
                thread_task_id.clone(),
                thread_workflow,
                body.auto_approve,
            ) {
                let _ = service.inner.runtime_tasks.mark_task_status(
                    &thread_task_id,
                    RuntimeTaskStatus::Failed,
                    json!({
                        "review_recommendation": {
                            "required": true,
                            "reason": err.to_string()
                        }
                    }),
                );
                service.emit(
                    "workflow_run_failed",
                    Some(thread_task_id.clone()),
                    Some(thread_task_id.clone()),
                    json!({
                        "task_id": thread_task_id,
                        "error": err.to_string(),
                    }),
                );
            }
        });

        Ok(WorkflowRunCreated {
            run_id: task.task_id.clone(),
            task_id: task.task_id,
            requested_kind,
            resolved_kind,
            workflow: compact_workflow_for_ui(workflow),
        })
    }

    fn run_workflow_thread(
        &self,
        task_id: String,
        workflow: GraphWorkflow,
        auto_approve: bool,
    ) -> PwResult<()> {
        self.inner.runtime_tasks.mark_task_status(
            &task_id,
            RuntimeTaskStatus::Running,
            json!({
                "current_step": {
                    "title": workflow.name.clone(),
                    "kind": RuntimeTaskKind::Workflow
                }
            }),
        )?;
        let mut runner = ServiceWorkflowRunner {
            runtime: self.clone(),
            task_id: task_id.clone(),
            auto_approve,
        };
        let summary = WorkflowExecutor::new().run(&workflow, &mut runner)?;
        persist_workflow_artifacts(
            &self.inner.runtime_tasks,
            &task_id,
            &workflow,
            Some(&summary),
        )?;
        if summary.status == WorkflowStatus::Completed {
            match persist_workflow_report(
                &self.settings(),
                &self.inner.runtime_tasks,
                &task_id,
                &summary,
            ) {
                Ok(Some(report)) => {
                    self.emit(
                        "workflow_report_persisted",
                        Some(task_id.clone()),
                        Some(task_id.clone()),
                        json!({ "task_id": task_id.clone(), "report": report }),
                    );
                }
                Ok(None) => {}
                Err(err) => {
                    self.emit(
                        "workflow_report_persist_failed",
                        Some(task_id.clone()),
                        Some(task_id.clone()),
                        json!({ "task_id": task_id.clone(), "error": err.to_string() }),
                    );
                }
            }
        }
        if summary.status == WorkflowStatus::Completed && workflow_is_research(&summary) {
            match self.postprocess_task_memory(&task_id, &summary) {
                Ok(status) => {
                    self.emit(
                        "memory_postprocess_completed",
                        Some(task_id.clone()),
                        Some(task_id.clone()),
                        json!({ "task_id": task_id.clone(), "status": status }),
                    );
                }
                Err(err) => {
                    self.emit(
                        "memory_postprocess_failed",
                        Some(task_id.clone()),
                        Some(task_id.clone()),
                        json!({ "task_id": task_id.clone(), "error": err.to_string() }),
                    );
                }
            }
        }
        finalize_workflow_task(&self.inner.runtime_tasks, &task_id, &summary)?;
        let kind = if summary.status == WorkflowStatus::Completed {
            "workflow_run_completed"
        } else {
            "workflow_run_failed"
        };
        self.emit(
            kind,
            Some(task_id.clone()),
            Some(task_id.clone()),
            json!({
                "task_id": task_id,
                "summary": summary,
            }),
        );
        Ok(())
    }

    fn emit_context(&self, run_id: &str, context_pack: &ContextPack) {
        self.emit(
            "context_built",
            Some(run_id.to_string()),
            None,
            json!({
                "context_id": context_pack.id,
                "selected_tool_ids": context_pack.selected_tool_ids,
                "missing": context_pack.missing,
                "warnings": context_pack.warnings,
                "summary": context_pack.summary,
            }),
        );
    }

    fn execute_tool_call(
        &self,
        call: ToolCall,
        auto_approve: bool,
        run_id: Option<String>,
        task_id: Option<String>,
    ) -> PwResult<ToolResult> {
        let settings = self.settings();
        let snapshot = self.snapshot();
        let registered = snapshot
            .get(&call.tool_id)
            .ok_or_else(|| PwError::ToolNotFound(call.tool_id.clone()))?;
        let descriptor = registered.descriptor.clone();
        let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));
        audit.record(AuditEvent::ToolCallRequested {
            call_id: call.id.clone(),
            tool_id: call.tool_id.clone(),
            name: call.name.clone(),
        });
        self.emit(
            "tool_started",
            run_id.clone(),
            task_id.clone(),
            json!({
                "call_id": call.id.clone(),
                "tool_id": call.tool_id.clone(),
                "name": call.name.clone(),
            }),
        );
        let policy = DefaultPolicyGuard::default().with_rules(load_rule_texts(&settings));
        let decision = policy.check(&descriptor, &call);
        audit.record(AuditEvent::PolicyDecisionRecorded {
            call_id: call.id.clone(),
            decision: decision.clone(),
        });
        self.emit(
            "tool_policy_decision",
            run_id.clone(),
            task_id.clone(),
            json!({ "call_id": call.id.clone(), "tool_id": call.tool_id.clone(), "decision": decision }),
        );
        let mut result = match decision {
            PolicyDecision::Allow => {
                self.execute_snapshot_tool(&snapshot, &call, run_id.clone(), task_id.clone())?
            }
            PolicyDecision::Deny { reason } => ToolResult::error(reason),
            PolicyDecision::AskUser { prompt } => {
                let approved = if auto_approve {
                    true
                } else {
                    ServiceApproval {
                        runtime: self.clone(),
                        run_id: run_id.clone(),
                        task_id: task_id.clone(),
                    }
                    .ask_user(&prompt, &call)
                };
                if approved {
                    self.execute_snapshot_tool(&snapshot, &call, run_id.clone(), task_id.clone())?
                } else {
                    ToolResult::error("user rejected tool call")
                }
            }
        };
        annotate_service_tool_result(&mut result, &call);
        audit.record(AuditEvent::ToolResultRecorded {
            call_id: call.id.clone(),
            is_error: result.is_error,
            metadata: result.metadata.clone(),
        });
        self.emit(
            "tool_completed",
            run_id,
            task_id,
            json!({
                "call_id": call.id,
                "tool_id": call.tool_id,
                "name": call.name,
                "is_error": result.is_error,
                "content_preview": preview_text(&result.content, 1200),
                "metadata": result.metadata,
                "artifacts": result.artifacts,
            }),
        );
        Ok(result)
    }

    fn execute_snapshot_tool(
        &self,
        snapshot: &ToolRegistrySnapshot,
        call: &ToolCall,
        run_id: Option<String>,
        task_id: Option<String>,
    ) -> PwResult<ToolResult> {
        let context = ToolExecutionContext {
            runtime_tasks: Some(self.inner.runtime_tasks.clone()),
            ..ToolExecutionContext::default()
        };
        let runtime = self.clone();
        let call_id = call.id.clone();
        let tool_id = call.tool_id.clone();
        let mut tool_runtime = ToolExecutionRuntime::new(context, move |event| {
            runtime.emit(
                "tool_runtime_event",
                run_id.clone(),
                task_id.clone(),
                json!({
                    "call_id": call_id.clone(),
                    "tool_id": tool_id.clone(),
                    "event": event,
                }),
            );
        });
        snapshot.execute_with_runtime(call, &mut tool_runtime)
    }
}

pub async fn serve(options: ServeOptions) -> PwResult<()> {
    if options.host != DEFAULT_HOST
        && options.host.parse::<IpAddr>().ok() != Some(IpAddr::from([127, 0, 0, 1]))
    {
        eprintln!(
            "warning: pwcli serve is intended for local use. Binding to {} exposes local tools over HTTP.",
            options.host
        );
    }
    let settings = Settings::load()?;
    let runtime = ServiceRuntime::new(settings)?;
    if options.reload_skills {
        runtime.start_skill_watcher();
    }
    let app = router(runtime.clone(), !options.no_ui);
    let bind = format!("{}:{}", options.host, options.port);
    let listener = tokio::net::TcpListener::bind(&bind).await.map_err(|err| {
        PwError::Message(format!("failed to bind pwcli service at {bind}: {err}"))
    })?;
    let local_addr = listener
        .local_addr()
        .map_err(|err| PwError::Message(format!("failed to read pwcli service address: {err}")))?;
    let url = format!("http://{local_addr}");
    runtime.emit(
        "service_started",
        None,
        None,
        json!({
            "url": url,
            "ui_enabled": !options.no_ui,
            "reload_skills": options.reload_skills,
        }),
    );
    println!("pwcli service listening on {url}");
    if options.open && !options.no_ui {
        let _ = open::that(&url);
    }
    axum::serve(listener, app)
        .await
        .map_err(|err| PwError::Message(format!("pwcli service failed: {err}")))
}

pub fn router(runtime: ServiceRuntime, serve_ui: bool) -> Router {
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/config", get(show_config).put(update_config))
        .route("/api/events", get(events))
        .route("/api/workflows/plan", post(plan_workflow))
        .route("/api/workflows/runs", post(create_workflow_run))
        .route("/api/chat/runs", post(create_chat_run))
        .route("/api/chat/runs/:run_id/events", get(run_events))
        .route("/api/approvals/:approval_id", post(resolve_approval))
        .route("/api/tools", get(list_tools))
        .route("/api/tools/health", get(tools_health))
        .route("/api/tools/:tool_id", get(show_tool))
        .route("/api/tools/:tool_id/call", post(call_tool))
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route(
            "/api/tasks/:task_id",
            get(show_task).delete(delete_task).post(delete_task),
        )
        .route("/api/tasks/:task_id/activate", post(activate_task))
        .route("/api/tasks/:task_id/decompose", post(decompose_task))
        .route("/api/tasks/:task_id/cancel", post(cancel_task))
        .route("/api/tasks/:task_id/compact", post(compact_task))
        .route("/api/tasks/:task_id/events", get(task_events))
        .route("/api/tasks/:task_id/log", get(task_log))
        .route("/api/tasks/:task_id/verify", post(verify_task))
        .route("/api/tasks/:task_id/materials", get(task_materials))
        .route("/api/task-parser", post(parse_personal_task))
        .route("/api/rules", get(list_rules))
        .route("/api/rules/:name", get(show_rule).post(upsert_rule))
        .route("/api/memory/inbox", get(memory_inbox))
        .route("/api/memory/facts", get(memory_facts))
        .route("/api/memory/layers", get(memory_layers))
        .route("/api/memory/search", get(memory_search))
        .route("/api/memory/events", get(memory_events))
        .route("/api/memory/graph", get(memory_graph))
        .route(
            "/api/memory/postprocess/task/:task_id",
            post(memory_postprocess_task),
        )
        .route("/api/memory/extractions/:task_id", get(memory_extractions))
        .route(
            "/api/memory/candidates/:candidate_id/accept",
            post(memory_accept),
        )
        .route(
            "/api/memory/candidates/:candidate_id/reject",
            post(memory_reject),
        )
        .route("/api/config/provider-model", post(update_provider_model))
        .route("/api/materials/:artifact_id", get(read_material_artifact))
        .route("/api/sessions", get(list_sessions))
        .route(
            "/api/session-folders",
            get(list_session_folders).post(create_session_folder),
        )
        .route(
            "/api/sessions/:session_id/folder",
            post(assign_session_folder),
        )
        .route(
            "/api/sessions/:session_id",
            get(show_session)
                .delete(delete_session)
                .post(delete_session),
        )
        .route("/api/audit/summary", get(audit_summary))
        .layer(CorsLayer::permissive())
        .with_state(runtime);

    if serve_ui {
        app.fallback_service(
            ServeDir::new("web/dist").not_found_service(ServeFile::new("web/dist/index.html")),
        )
    } else {
        app
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "pwcli",
    })
}

async fn status(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<StatusResponse>> {
    Ok(Json(runtime.status()?))
}

async fn show_config(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    Ok(Json(config_for_ui(&runtime.settings())?))
}

async fn update_config(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let mut next: Settings =
        serde_json::from_value(body).map_err(|err| ApiError::bad_request(err.to_string()))?;
    let mut settings = runtime
        .inner
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    preserve_secret_fields(&settings, &mut next);
    next.home_dir = settings.home_dir.clone();
    next.pwcli_home = settings.pwcli_home.clone();
    next.skill_roots = settings.skill_roots.clone();
    next.max_rounds = settings.max_rounds;
    normalize_ui_secret_fields(&mut next);
    next.normalize();
    next.validate_for_save()
        .map_err(|err| ApiError::bad_request(err.to_string()))?;
    next.save_default()?;
    *settings = next.clone();
    drop(settings);
    runtime.refresh_registry()?;
    let response = config_for_ui(&next)?;
    runtime.emit("config_updated", None, None, response.clone());
    Ok(Json(response))
}

async fn events(
    State(runtime): State<ServiceRuntime>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    event_stream(runtime, query.cursor.unwrap_or(0), None)
}

async fn run_events(
    State(runtime): State<ServiceRuntime>,
    AxumPath(run_id): AxumPath<String>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    event_stream(runtime, query.cursor.unwrap_or(0), Some(run_id))
}

fn event_stream(
    runtime: ServiceRuntime,
    cursor: u64,
    run_filter: Option<String>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    let backlog = runtime.inner.events.since(cursor);
    let mut rx = runtime.inner.events.subscribe();
    let stream = stream! {
        for event in backlog {
            if event_matches_run(&event, run_filter.as_deref()) {
                yield Ok(sse_event(event));
            }
        }
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event_matches_run(&event, run_filter.as_deref()) {
                        yield Ok(sse_event(event));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn event_matches_run(event: &ServiceEventEnvelope, run_filter: Option<&str>) -> bool {
    run_filter
        .map(|run_id| event.run_id.as_deref() == Some(run_id))
        .unwrap_or(true)
}

fn sse_event(event: ServiceEventEnvelope) -> Event {
    Event::default()
        .id(event.seq.to_string())
        .event(event.kind.clone())
        .json_data(event)
        .unwrap_or_else(|_| Event::default().event("serialization_error"))
}

async fn create_chat_run(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<ChatRunRequestBody>,
) -> ApiResult<Json<ChatRunCreated>> {
    Ok(Json(runtime.start_chat_run(body)?))
}

async fn plan_workflow(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<WorkflowPlanRequestBody>,
) -> ApiResult<Json<WorkflowPlanResponse>> {
    let goal = body.goal.trim();
    if goal.is_empty() {
        return Err(ApiError::bad_request("goal cannot be empty"));
    }
    let requested_kind = body.kind.unwrap_or(WorkflowPlanKind::Auto);
    let resolved_kind = requested_kind.resolve(goal);
    let settings = runtime.settings();
    let agent = body
        .agent
        .unwrap_or_else(|| settings.agent_for_route(resolved_kind.as_str()));
    if resolved_kind == WorkflowPlanKind::Code && AgentCliKind::from_id(&agent).is_none() {
        return Err(ApiError::bad_request(format!(
            "workflow agent '{agent}' is not supported by local pwcli; expected codex, claude, agy, or qodercli"
        )));
    }
    if resolved_kind == WorkflowPlanKind::Code && !agent_available_for_ui(&settings, &agent) {
        return Err(ApiError::bad_request(format!(
            "workflow agent '{agent}' is not available or not logged in on this machine"
        )));
    }
    let workflow = GraphWorkflow::planned(goal.to_string(), agent, requested_kind);
    Ok(Json(WorkflowPlanResponse {
        requested_kind,
        resolved_kind,
        workflow: compact_workflow_for_ui(workflow),
    }))
}

async fn create_workflow_run(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<WorkflowRunRequestBody>,
) -> ApiResult<Json<WorkflowRunCreated>> {
    runtime
        .start_workflow_run(body)
        .map(Json)
        .map_err(|err| ApiError::bad_request(err.to_string()))
}

async fn resolve_approval(
    State(runtime): State<ServiceRuntime>,
    AxumPath(approval_id): AxumPath<String>,
    Json(body): Json<ApprovalBody>,
) -> ApiResult<Json<Value>> {
    let ticket = runtime
        .inner
        .approvals
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&approval_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown approval {approval_id}")))?;
    ticket
        .sender
        .send(body.approved)
        .map_err(|_| ApiError::bad_request("approval receiver is no longer waiting"))?;
    runtime.emit(
        "approval_resolved",
        ticket.run_id.clone(),
        ticket.task_id.clone(),
        json!({
            "approval_id": approval_id,
            "approved": body.approved,
            "run_id": ticket.run_id,
            "task_id": ticket.task_id,
        }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn list_tools(State(runtime): State<ServiceRuntime>) -> Json<Value> {
    let snapshot = runtime.snapshot();
    Json(json!({
        "registry_version": snapshot.version(),
        "tools": snapshot.descriptors(),
    }))
}

async fn tools_health(State(runtime): State<ServiceRuntime>) -> Json<Value> {
    Json(json!(build_tool_health_report(&runtime.settings())))
}

async fn show_tool(
    State(runtime): State<ServiceRuntime>,
    AxumPath(tool_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let snapshot = runtime.snapshot();
    let tool = snapshot
        .get(&tool_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown tool {tool_id}")))?;
    Ok(Json(json!({
        "descriptor": tool.descriptor,
        "executable": tool.executor.is_some(),
    })))
}

async fn call_tool(
    State(runtime): State<ServiceRuntime>,
    AxumPath(tool_id): AxumPath<String>,
    Json(body): Json<ToolCallBody>,
) -> ApiResult<Json<ToolResult>> {
    let snapshot = runtime.snapshot();
    let descriptor = snapshot
        .get(&tool_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown tool {tool_id}")))?
        .descriptor
        .clone();
    let call = ToolCall {
        id: new_id("toolcall"),
        tool_id,
        name: descriptor.name,
        arguments: body.arguments,
    };
    let runtime_clone = runtime.clone();
    let result = tokio::task::spawn_blocking(move || {
        runtime_clone.execute_tool_call(call, body.auto_approve, None, None)
    })
    .await
    .map_err(|err| ApiError::bad_request(format!("tool task failed: {err}")))??;
    Ok(Json(result))
}

async fn list_tasks(
    State(runtime): State<ServiceRuntime>,
    Query(query): Query<TaskListQuery>,
) -> ApiResult<Json<Value>> {
    let active = runtime.inner.runtime_tasks.active_task_id()?;
    let scope = query.scope.as_deref().unwrap_or("all");
    let mut tasks = runtime.inner.runtime_tasks.list()?;
    if scope == "user" {
        tasks.retain(is_user_created_task);
    }
    let active = active.filter(|active_id| tasks.iter().any(|task| task.task_id == *active_id));
    Ok(Json(json!({
        "active_task_id": active,
        "tasks": tasks,
    })))
}

async fn create_task(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<CreateTaskBody>,
) -> ApiResult<Json<Value>> {
    let title = body.title.trim();
    if title.is_empty() {
        return Err(ApiError::bad_request("task title is required"));
    }
    let cwd = match body.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(PwError::from)?,
    };
    let kind = body.kind.unwrap_or(RuntimeTaskKind::Internal);
    let metadata = if body.metadata.is_null() {
        json!({})
    } else {
        body.metadata
    };
    let metadata = ensure_user_task_metadata(metadata, kind);
    let task = runtime
        .inner
        .runtime_tasks
        .create_task(kind, title, cwd, metadata)?;
    if body.active {
        runtime.inner.runtime_tasks.set_active(&task.task_id)?;
    }
    runtime.emit(
        "task_created",
        None,
        Some(task.task_id.clone()),
        json!({ "task": task }),
    );
    Ok(Json(json!({ "task": task })))
}

async fn show_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let task = runtime.inner.runtime_tasks.get(&task_id)?;
    let workflow_summary = load_workflow_summary(&runtime.inner.runtime_tasks, &task_id).ok();
    Ok(Json(json!({
        "task": task,
        "next": crate::runtime::format_task_next(&task),
        "workflow_summary": workflow_summary,
    })))
}

async fn delete_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.delete(&task_id)?;
    runtime.emit(
        "task_deleted",
        None,
        Some(task_id.clone()),
        json!({ "task_id": task_id }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn activate_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.set_active(&task_id)?;
    runtime.emit(
        "task_activated",
        None,
        Some(task_id.clone()),
        json!({ "task_id": task_id }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn cancel_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.cancel(&task_id)?;
    runtime.emit(
        "task_cancel_requested",
        None,
        Some(task_id.clone()),
        json!({ "task_id": task_id }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn compact_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
    Json(body): Json<TaskCompactBody>,
) -> ApiResult<Json<Value>> {
    let scope = parse_compact_scope(body.scope.as_deref())?;
    let summary = runtime.inner.runtime_tasks.compact(&task_id, scope)?;
    runtime.emit(
        "task_compacted",
        None,
        Some(task_id.clone()),
        json!({
            "task_id": task_id,
            "summary_path": summary.summary_path,
        }),
    );
    Ok(Json(json!({ "summary": summary })))
}

async fn parse_personal_task(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<TaskParserBody>,
) -> ApiResult<Json<Value>> {
    let input = body.input.trim().to_string();
    if input.is_empty() {
        return Err(ApiError::bad_request("task parser input is required"));
    }
    let runtime_clone = runtime.clone();
    let parsed = tokio::task::spawn_blocking(move || runtime_clone.parse_personal_task(input))
        .await
        .map_err(|err| ApiError::bad_request(format!("task parser failed: {err}")))??;
    Ok(Json(parsed))
}

async fn decompose_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
    Json(body): Json<TaskDecomposeBody>,
) -> ApiResult<Json<Value>> {
    let task = runtime.inner.runtime_tasks.get(&task_id)?;
    let goal = body
        .goal
        .unwrap_or_else(|| task.title.clone())
        .trim()
        .to_string();
    if goal.is_empty() {
        return Err(ApiError::bad_request("decompose goal cannot be empty"));
    }
    let requested_kind = body.kind.unwrap_or(WorkflowPlanKind::Auto);
    let agent = body.agent.unwrap_or_else(|| "codex".to_string());
    if AgentCliKind::from_id(&agent).is_none() {
        return Err(ApiError::bad_request(format!(
            "workflow agent '{agent}' is not supported by local pwcli; expected codex, claude, agy, or qodercli"
        )));
    }
    let resolved_kind = requested_kind.resolve(&goal);
    let workflow =
        compact_workflow_for_ui(GraphWorkflow::planned(goal.clone(), agent, requested_kind));
    let decomposition =
        task_decomposition_from_workflow(&goal, requested_kind, resolved_kind, &workflow);
    let user_steps: Vec<Value> = decomposition
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            json!({
                "id": step.id.clone(),
                "title": step.label.clone(),
                "summary": step.summary.clone(),
                "kind": step.kind.clone(),
                "status": "todo",
                "order": index + 1,
                "to": step.to.clone(),
            })
        })
        .collect();
    runtime.inner.runtime_tasks.merge_task_metadata(
        &task_id,
        json!({
            "decomposition": decomposition.clone(),
            "user_task": {
                "schema_version": 1,
                "source": task
                    .metadata
                    .pointer("/user_task/source")
                    .and_then(Value::as_str)
                    .unwrap_or("web"),
                "type": "personal_task",
                "status": task
                    .metadata
                    .pointer("/user_task/status")
                    .and_then(Value::as_str)
                    .unwrap_or("todo"),
                "priority": task
                    .metadata
                    .pointer("/user_task/priority")
                    .and_then(Value::as_str)
                    .unwrap_or("normal"),
                "steps": user_steps,
                "decomposed_at": service_now_millis(),
                "decomposition_kind": resolved_kind,
            }
        }),
    )?;
    runtime.emit(
        "task_decomposed",
        None,
        Some(task_id.clone()),
        json!({
            "task_id": task_id,
            "goal": goal,
            "resolved_kind": resolved_kind,
            "steps": decomposition.steps.len(),
        }),
    );
    Ok(Json(
        json!({ "task_id": task_id, "decomposition": decomposition }),
    ))
}

async fn task_events(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<TaskEventsQuery>,
) -> ApiResult<Json<Value>> {
    let (events, next_offset) = runtime
        .inner
        .runtime_tasks
        .read_events_from(&task_id, query.offset.unwrap_or(0))?;
    Ok(Json(json!({
        "task_id": task_id,
        "next_offset": next_offset,
        "events": events,
    })))
}

async fn task_log(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<TaskLogQuery>,
) -> ApiResult<Json<Value>> {
    let stream = query.stream.unwrap_or_else(|| "stdout".to_string());
    let file_name = match stream.as_str() {
        "stdout" => "stdout.log",
        "stderr" => "stderr.log",
        "events" => "events.jsonl",
        "summary" => "summary.md",
        _ => {
            return Err(ApiError::bad_request(
                "stream must be stdout, stderr, events, or summary",
            ));
        }
    };
    let path = runtime
        .inner
        .runtime_tasks
        .task_dir(&task_id)
        .join(file_name);
    let content = fs::read_to_string(&path).unwrap_or_default();
    Ok(Json(json!({
        "task_id": task_id,
        "stream": stream,
        "path": path,
        "content": tail_chars(&content, query.tail_chars.unwrap_or(12000)),
    })))
}

async fn verify_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
    Json(body): Json<VerifyBody>,
) -> ApiResult<Json<Value>> {
    let task = runtime.inner.runtime_tasks.get(&task_id)?;
    let mut args = serde_json::Map::new();
    args.insert(
        "cwd".to_string(),
        json!(body.cwd.unwrap_or_else(|| task.cwd.clone())),
    );
    if !body.commands.is_empty() {
        args.insert("commands".to_string(), json!(body.commands));
    }
    if let Some(timeout_seconds) = body.timeout_seconds {
        args.insert("timeout_seconds".to_string(), json!(timeout_seconds));
    }
    if let Some(max_output_chars) = body.max_output_chars {
        args.insert("max_output_chars".to_string(), json!(max_output_chars));
    }
    let call = ToolCall {
        id: new_id("verify"),
        tool_id: "verification.project_check".to_string(),
        name: "project_check".to_string(),
        arguments: Value::Object(args),
    };
    let runtime_clone = runtime.clone();
    let task_cwd = task.cwd.clone();
    let verify_task_id = task_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        runtime_clone.execute_tool_call(
            call,
            body.auto_approve,
            Some(new_id("verify_run")),
            Some(verify_task_id),
        )
    })
    .await
    .map_err(|err| ApiError::bad_request(format!("verification task failed: {err}")))??;
    let record = verification_record_from_tool_result(result.clone(), &task_cwd);
    let verification_path = runtime
        .inner
        .runtime_tasks
        .record_verification(&task_id, record.clone())?;
    runtime.emit(
        "verification_report",
        None,
        Some(task_id.clone()),
        json!({
            "task_id": task_id,
            "path": verification_path,
            "report": record.report,
        }),
    );
    Ok(Json(json!({
        "path": verification_path,
        "result": result,
    })))
}

async fn list_rules(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let rules_dir = settings.pwcli_home.join("rules");
    fs::create_dir_all(&rules_dir).map_err(PwError::from)?;
    let mut rules = Vec::new();
    for entry in fs::read_dir(&rules_dir).map_err(PwError::from)? {
        let entry = entry.map_err(PwError::from)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let text = fs::read_to_string(&path).unwrap_or_default();
        rules.push(json!({
            "name": name,
            "path": path,
            "chars": text.chars().count(),
            "preview": tail_chars(&text, 600),
        }));
    }
    rules.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    Ok(Json(json!({ "rules": rules })))
}

async fn show_rule(
    State(runtime): State<ServiceRuntime>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let path = safe_rule_path(&runtime.settings(), &name)?;
    if !path.is_file() {
        return Err(ApiError::not_found(format!("unknown rule {name}")));
    }
    Ok(Json(json!({
        "name": name,
        "path": path,
        "text": fs::read_to_string(&path).map_err(PwError::from)?,
    })))
}

async fn upsert_rule(
    State(runtime): State<ServiceRuntime>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<RuleBody>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let path = safe_rule_path(&settings, &name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(PwError::from)?;
    }
    fs::write(&path, body.text).map_err(PwError::from)?;
    runtime.emit(
        "rule_updated",
        None,
        None,
        json!({ "name": name, "path": path }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn memory_inbox(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    Ok(Json(json!({ "candidates": store.list_candidates()? })))
}

async fn memory_facts(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    Ok(Json(json!({ "facts": store.list_facts()? })))
}

async fn memory_layers(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    Ok(Json(json!({
        "facts": store.list_facts()?,
        "inferences": store.list_inferences()?,
        "hypotheses": store.list_hypotheses()?,
        "user_preferences": read_user_preference_state(&settings).unwrap_or_else(|_| json!({
            "namespace": "user_preferences",
            "hypotheses": []
        })),
    })))
}

async fn memory_search(
    State(runtime): State<ServiceRuntime>,
    Query(query): Query<MemorySearchQuery>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    let q = query.q.unwrap_or_default();
    Ok(Json(json!({
        "query": q,
        "recall": store.recall(&q, query.limit.unwrap_or(8))?,
    })))
}

async fn memory_events(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    Ok(Json(json!({ "events": store.lifecycle_events()? })))
}

async fn memory_graph(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    Ok(Json(json!({ "graph": store.graph_stats()? })))
}

async fn task_materials(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.get(&task_id)?;
    let runtime_clone = runtime.clone();
    let task_id_clone = task_id.clone();
    let value = tokio::task::spawn_blocking(move || {
        let settings = runtime_clone.settings();
        collect_task_materials(
            &settings,
            &runtime_clone.inner.runtime_tasks,
            &task_id_clone,
        )
    })
    .await
    .map_err(|err| ApiError::bad_request(format!("failed to load task materials: {err}")))??;
    Ok(Json(value))
}

async fn read_material_artifact(
    State(runtime): State<ServiceRuntime>,
    AxumPath(artifact_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let runtime_clone = runtime.clone();
    let artifact_id_clone = artifact_id.clone();
    let value = tokio::task::spawn_blocking(move || {
        read_material_artifact_by_id(&runtime_clone.inner.runtime_tasks, &artifact_id_clone)
    })
    .await
    .map_err(|err| ApiError::bad_request(format!("failed to read material artifact: {err}")))??;
    Ok(Json(value))
}

async fn memory_postprocess_task(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.get(&task_id)?;
    let summary = load_workflow_summary(&runtime.inner.runtime_tasks, &task_id)?;
    let status = tokio::task::spawn_blocking({
        let runtime = runtime.clone();
        let task_id = task_id.clone();
        move || runtime.postprocess_task_memory(&task_id, &summary)
    })
    .await
    .map_err(|err| ApiError::bad_request(format!("memory postprocess failed: {err}")))??;
    runtime.emit(
        "memory_postprocess_completed",
        Some(task_id.clone()),
        Some(task_id.clone()),
        json!({ "task_id": task_id, "status": status.clone() }),
    );
    Ok(Json(status))
}

async fn memory_extractions(
    State(runtime): State<ServiceRuntime>,
    AxumPath(task_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    runtime.inner.runtime_tasks.get(&task_id)?;
    Ok(Json(
        read_memory_extraction_status(&runtime.inner.runtime_tasks, &task_id).unwrap_or_else(
            |_| {
                json!({
                    "task_id": task_id,
                    "papers": [],
                })
            },
        ),
    ))
}

async fn memory_accept(
    State(runtime): State<ServiceRuntime>,
    AxumPath(candidate_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    let facts = store.accept_candidate(&candidate_id)?;
    runtime.emit(
        "memory_candidate_accepted",
        None,
        None,
        json!({ "candidate_id": candidate_id, "facts": facts }),
    );
    Ok(Json(json!({ "facts": facts })))
}

async fn memory_reject(
    State(runtime): State<ServiceRuntime>,
    AxumPath(candidate_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    store.reject_candidate(&candidate_id)?;
    runtime.emit(
        "memory_candidate_rejected",
        None,
        None,
        json!({ "candidate_id": candidate_id }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn update_provider_model(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<ConfigSwitchBody>,
) -> ApiResult<Json<Value>> {
    let mut settings = runtime
        .inner
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(provider) = body.provider.as_deref() {
        settings.set_provider(provider)?;
    }
    if let Some(model) = body.model.as_deref() {
        settings.set_model(model)?;
    }
    if let Some(thinking) = body.thinking {
        settings.set_thinking(thinking);
    }
    if let Some(show_thinking) = body.show_thinking {
        settings.set_show_thinking(show_thinking);
    }
    if let Some(tokens) = body.model_max_input_tokens {
        settings.set_active_model_max_input_tokens(tokens)?;
    }
    if let Some(tokens) = body.model_max_output_tokens {
        settings.set_active_model_max_output_tokens(tokens)?;
    }
    if let Some(tokens) = body.context_max_input_tokens {
        settings.set_context_max_input_tokens(tokens);
    }
    settings.save_default()?;
    let response = json!({
        "provider": settings.provider,
        "model": settings.model,
        "thinking": settings.thinking,
        "show_thinking": settings.show_thinking,
        "model_max_input_tokens": settings
            .active_model()
            .map(|model| model.max_input_tokens)
            .unwrap_or_default(),
        "model_max_output_tokens": settings
            .active_model()
            .map(|model| model.max_output_tokens)
            .unwrap_or_default(),
        "context": settings.context,
    });
    drop(settings);
    runtime.emit("config_updated", None, None, response.clone());
    Ok(Json(response))
}

async fn list_sessions(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    Ok(Json(json!({ "sessions": store.list()? })))
}

async fn show_session(
    State(runtime): State<ServiceRuntime>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    let record = store
        .get(&session_id)?
        .ok_or_else(|| ApiError::not_found(format!("unknown session {session_id}")))?;
    let messages = record
        .summary
        .state
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| graph_message_for_ui(index, message))
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "entry": record.entry,
        "messages": messages,
        "status": record.summary.state.status,
        "round_count": record.summary.state.round_count,
    })))
}

async fn list_session_folders(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    Ok(Json(json!(store.folder_state()?)))
}

async fn create_session_folder(
    State(runtime): State<ServiceRuntime>,
    Json(body): Json<CreateSessionFolderBody>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    let state = store.create_folder(&body.name)?;
    runtime.emit(
        "session_folder_created",
        None,
        None,
        json!({ "name": body.name, "folders": state.folders }),
    );
    Ok(Json(json!(state)))
}

async fn assign_session_folder(
    State(runtime): State<ServiceRuntime>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<AssignSessionFolderBody>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    let state = store.assign_folder(&session_id, &body.folder_id)?;
    runtime.emit(
        "session_folder_assigned",
        None,
        None,
        json!({ "session_id": session_id, "folder_id": body.folder_id }),
    );
    Ok(Json(json!(state)))
}

async fn delete_session(
    State(runtime): State<ServiceRuntime>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let store = SessionStore::new(settings.pwcli_home);
    let deleted = store.delete(&session_id)?;
    match deleted {
        Some(entry) => {
            runtime.emit(
                "session_deleted",
                None,
                None,
                json!({
                    "session_id": entry.id,
                }),
            );
            Ok(Json(json!({ "ok": true, "session": entry })))
        }
        None => Err(ApiError::not_found(format!(
            "session '{session_id}' not found"
        ))),
    }
}

async fn audit_summary(State(runtime): State<ServiceRuntime>) -> ApiResult<Json<Value>> {
    let settings = runtime.settings();
    let (events, malformed_lines) =
        read_audit_events(&settings.pwcli_home.join("audit/events.jsonl"))?;
    let summary = summarize_events(&events, malformed_lines);
    Ok(Json(json!({
        "summary": summary,
        "text": format_audit_summary(&summary),
    })))
}

#[derive(Clone)]
struct ServiceApproval {
    runtime: ServiceRuntime,
    run_id: Option<String>,
    task_id: Option<String>,
}

impl UserApproval for ServiceApproval {
    fn ask_user(&self, prompt: &str, call: &ToolCall) -> bool {
        let approval_id = new_id("approval");
        let (tx, rx) = mpsc::channel();
        self.runtime
            .inner
            .approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                approval_id.clone(),
                ApprovalTicket {
                    sender: tx,
                    run_id: self.run_id.clone(),
                    task_id: self.task_id.clone(),
                },
            );
        self.runtime.emit(
            "approval_required",
            self.run_id.clone(),
            self.task_id.clone(),
            json!({
                "approval_id": approval_id,
                "prompt": prompt,
                "call": call,
            }),
        );
        rx.recv().unwrap_or(false)
    }
}

struct ServiceWorkflowRunner {
    runtime: ServiceRuntime,
    task_id: String,
    auto_approve: bool,
}

impl WorkflowNodeRunner for ServiceWorkflowRunner {
    fn run_node(
        &mut self,
        _workflow: &GraphWorkflow,
        node: &WorkflowNode,
        context: &WorkflowContext,
    ) -> PwResult<WorkflowStepOutcome> {
        self.runtime
            .inner
            .runtime_tasks
            .record_workflow_node_started(&self.task_id, &node.id, &node.label)?;
        let outcome = match &node.kind {
            WorkflowNodeKind::AgentTask {
                agent,
                mode,
                prompt,
            } => self.run_agent_node(agent, mode, prompt, context),
            WorkflowNodeKind::ToolCall { tool_id, arguments } => {
                self.run_tool_node(tool_id, arguments.clone())
            }
            WorkflowNodeKind::ResearchReadPapers { max_papers } => {
                self.run_research_read_papers(*max_papers, context)
            }
            WorkflowNodeKind::AdaptiveLoop { prompt } => self.run_adaptive_loop(prompt, context),
            WorkflowNodeKind::Approval { prompt } => self.run_approval_node(prompt),
            WorkflowNodeKind::ModelTurn { prompt } => self.run_model_node(prompt, context),
            WorkflowNodeKind::Join => Ok(WorkflowStepOutcome::Success(json!({ "ok": true }))),
            WorkflowNodeKind::SubWorkflow { workflow } => {
                let mut nested = ServiceWorkflowRunner {
                    runtime: self.runtime.clone(),
                    task_id: self.task_id.clone(),
                    auto_approve: self.auto_approve,
                };
                match WorkflowExecutor::new().run(workflow, &mut nested) {
                    Ok(summary) if summary.status == WorkflowStatus::Completed => {
                        Ok(WorkflowStepOutcome::Success(serde_json::to_value(summary)?))
                    }
                    Ok(summary) => Ok(WorkflowStepOutcome::Failure(format!(
                        "subworkflow ended with {:?}",
                        summary.status
                    ))),
                    Err(err) => Ok(WorkflowStepOutcome::Failure(err.to_string())),
                }
            }
            WorkflowNodeKind::End => Ok(WorkflowStepOutcome::Stop),
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) => WorkflowStepOutcome::Failure(err.to_string()),
        };
        let status = match &outcome {
            WorkflowStepOutcome::Success(_) | WorkflowStepOutcome::Stop => "success",
            WorkflowStepOutcome::Failure(_) => "failure",
            WorkflowStepOutcome::Interrupt { .. } => "interrupt",
        };
        self.runtime
            .inner
            .runtime_tasks
            .record_workflow_node_completed(&self.task_id, &node.id, status)?;
        Ok(outcome)
    }
}

impl ServiceWorkflowRunner {
    fn run_agent_node(
        &self,
        agent: &str,
        mode: &str,
        prompt: &str,
        context: &WorkflowContext,
    ) -> PwResult<WorkflowStepOutcome> {
        let Some(kind) = AgentCliKind::from_id(agent) else {
            return Ok(WorkflowStepOutcome::Failure(format!(
                "workflow agent '{agent}' is not supported by local pwcli"
            )));
        };
        let task = self.runtime.inner.runtime_tasks.get(&self.task_id)?;
        let prompt = workflow_agent_prompt(prompt, context);
        let args = workflow_agent_args_from_settings(
            &self.runtime.settings(),
            agent,
            mode,
            prompt,
            task.cwd,
        );
        let child_task_id = new_id("agent");
        let metadata_args = args.clone();
        let mut spec = build_runtime_task_spec(kind, Some(child_task_id.clone()), args, None);
        if let Value::Object(metadata) = &mut spec.metadata {
            metadata.insert(
                "parent_workflow_task_id".to_string(),
                json!(self.task_id.clone()),
            );
            metadata.insert("workflow_agent_mode".to_string(), json!(mode));
            metadata.insert(
                "workflow_agent_profile_source".to_string(),
                json!("settings"),
            );
        }
        let handle = match self.runtime.inner.runtime_tasks.spawn(spec) {
            Ok(handle) => handle,
            Err(err) => return Ok(WorkflowStepOutcome::Failure(err.to_string())),
        };
        self.runtime.emit(
            "workflow_agent_task_started",
            Some(self.task_id.clone()),
            Some(self.task_id.clone()),
            json!({
                "parent_task_id": self.task_id.clone(),
                "child_task_id": handle.task_id.clone(),
                "agent": agent,
                "mode": mode,
                "model": metadata_args.model,
                "effort": metadata_args.effort,
                "timeout_seconds": metadata_args.timeout_seconds,
                "yolo": metadata_args.yolo,
                "cwd": metadata_args.cwd,
                "profile_source": "settings",
                "task_dir": handle.task_dir.clone(),
            }),
        );
        self.wait_for_agent_task(&handle.task_id, agent, mode)
    }

    fn wait_for_agent_task(
        &self,
        child_task_id: &str,
        agent: &str,
        mode: &str,
    ) -> PwResult<WorkflowStepOutcome> {
        loop {
            if self
                .runtime
                .inner
                .runtime_tasks
                .get(&self.task_id)
                .map(|task| task.status == RuntimeTaskStatus::Cancelled)
                .unwrap_or(false)
            {
                let _ = self.runtime.inner.runtime_tasks.cancel(child_task_id);
                return Ok(WorkflowStepOutcome::Failure(
                    "workflow task was cancelled".to_string(),
                ));
            }

            let child = match self.runtime.inner.runtime_tasks.get(child_task_id) {
                Ok(task) => task,
                Err(err) => return Ok(WorkflowStepOutcome::Failure(err.to_string())),
            };
            match child.status {
                RuntimeTaskStatus::Completed => {
                    let result =
                        read_task_result_value(&self.runtime.inner.runtime_tasks, child_task_id);
                    self.runtime.emit(
                        "workflow_agent_task_completed",
                        Some(self.task_id.clone()),
                        Some(self.task_id.clone()),
                        json!({
                            "parent_task_id": self.task_id.clone(),
                            "child_task_id": child_task_id,
                            "agent": agent,
                            "mode": mode,
                            "status": "completed",
                            "result": result,
                        }),
                    );
                    return Ok(WorkflowStepOutcome::Success(json!({
                        "task_id": child_task_id,
                        "agent": agent,
                        "mode": mode,
                        "result": result,
                    })));
                }
                RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Cancelled
                | RuntimeTaskStatus::TimedOut => {
                    let error =
                        task_failure_preview(&self.runtime.inner.runtime_tasks, child_task_id);
                    self.runtime.emit(
                        "workflow_agent_task_completed",
                        Some(self.task_id.clone()),
                        Some(self.task_id.clone()),
                        json!({
                            "parent_task_id": self.task_id.clone(),
                            "child_task_id": child_task_id,
                            "agent": agent,
                            "mode": mode,
                            "status": format!("{:?}", child.status),
                            "error": error,
                        }),
                    );
                    return Ok(WorkflowStepOutcome::Failure(error));
                }
                RuntimeTaskStatus::Pending | RuntimeTaskStatus::Running => {
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn run_tool_node(&self, tool_id: &str, arguments: Value) -> PwResult<WorkflowStepOutcome> {
        let snapshot = self.runtime.snapshot();
        let Some(registered) = snapshot.get(tool_id) else {
            return Ok(WorkflowStepOutcome::Failure(format!(
                "unknown workflow tool {tool_id}"
            )));
        };
        let call = ToolCall {
            id: format!("workflow-{}-{}", self.task_id, tool_id.replace('.', "_")),
            tool_id: tool_id.to_string(),
            name: registered.descriptor.name.clone(),
            arguments,
        };
        let result = match self.runtime.execute_tool_call(
            call,
            self.auto_approve,
            Some(self.task_id.clone()),
            Some(self.task_id.clone()),
        ) {
            Ok(result) => result,
            Err(err) => return Ok(WorkflowStepOutcome::Failure(err.to_string())),
        };
        if tool_id == "verification.project_check" {
            let task = self.runtime.inner.runtime_tasks.get(&self.task_id)?;
            let record = verification_record_from_tool_result(result.clone(), &task.cwd);
            let gate = record
                .report
                .as_ref()
                .map(|report| report.gate.decision)
                .unwrap_or(if record.passed {
                    VerificationGateDecision::Pass
                } else {
                    VerificationGateDecision::Block
                });
            self.runtime
                .inner
                .runtime_tasks
                .record_verification(&self.task_id, record.clone())?;
            return Ok(match gate {
                VerificationGateDecision::Pass => WorkflowStepOutcome::Success(json!({
                    "tool_id": tool_id,
                    "metadata": result.metadata,
                    "gate": "pass"
                })),
                VerificationGateDecision::Block => WorkflowStepOutcome::Failure(
                    record
                        .report
                        .as_ref()
                        .map(|report| report.summary.clone())
                        .unwrap_or(result.content),
                ),
                VerificationGateDecision::NeedsReview => WorkflowStepOutcome::Failure(
                    record
                        .report
                        .as_ref()
                        .map(|report| format!("verification needs review: {}", report.summary))
                        .unwrap_or_else(|| "verification needs review".to_string()),
                ),
            });
        }
        if result.is_error {
            Ok(WorkflowStepOutcome::Failure(result.content))
        } else {
            Ok(WorkflowStepOutcome::Success(json!({
                "tool_id": tool_id,
                "content": result.content,
                "metadata": result.metadata
            })))
        }
    }

    fn run_research_read_papers(
        &self,
        max_papers: usize,
        context: &WorkflowContext,
    ) -> PwResult<WorkflowStepOutcome> {
        let search_content = context
            .outputs
            .get("web_search")
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let candidates = extract_research_paper_candidates(search_content, max_papers.max(1));
        if candidates.is_empty() {
            return Ok(WorkflowStepOutcome::Success(json!({
                "content": "No PDF/arXiv paper candidates were found in the web search results. Later synthesis must treat web results as snippets only.",
                "papers": [],
                "read_level": "snippets_only"
            })));
        }

        let mut papers = Vec::new();
        let mut notes = Vec::new();
        for (index, candidate) in candidates.into_iter().enumerate() {
            let mut record = json!({
                "title": candidate.title,
                "url": candidate.url,
                "pdf_url": candidate.pdf_url,
                "read_level": "not_read",
                "tool": Value::Null,
            });
            if let Some(pdf_url) = candidate.pdf_url.clone() {
                let call = ToolCall {
                    id: format!("workflow-{}-read_papers-mineru-{}", self.task_id, index + 1),
                    tool_id: "builtin.mineru_parse_document".to_string(),
                    name: "mineru_parse_document".to_string(),
                    arguments: json!({
                        "url": pdf_url,
                        "model_version": "vlm",
                        "wait": true,
                        "timeout_seconds": 240
                    }),
                };
                match self.runtime.execute_tool_call(
                    call,
                    self.auto_approve,
                    Some(self.task_id.clone()),
                    Some(self.task_id.clone()),
                ) {
                    Ok(result) if !result.is_error => {
                        let markdown = result
                            .metadata
                            .get("markdown")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if title_needs_replacement(record["title"].as_str().unwrap_or_default()) {
                            if let Some(title) = infer_title_from_text(markdown) {
                                record["title"] = json!(title);
                            }
                        }
                        record["read_level"] = json!(if markdown.trim().is_empty() {
                            "mineru_parsed_no_markdown"
                        } else {
                            "full_pdf_mineru"
                        });
                        record["tool"] = json!("builtin.mineru_parse_document");
                        record["mineru"] = result.metadata.clone();
                        if !markdown.trim().is_empty() {
                            record["content"] = json!(preview_text(markdown, 12000));
                            notes.push(format!(
                                "{}. full PDF parsed with MinerU: {}",
                                index + 1,
                                record["title"].as_str().unwrap_or("paper")
                            ));
                        } else {
                            notes.push(format!(
                                "{}. MinerU parsed PDF but no markdown was returned yet: {}",
                                index + 1,
                                record["title"].as_str().unwrap_or("paper")
                            ));
                        }
                    }
                    Ok(result) => {
                        record["read_level"] = json!("mineru_failed");
                        record["tool"] = json!("builtin.mineru_parse_document");
                        record["error"] = json!(result.content);
                        notes.push(format!(
                            "{}. MinerU failed for {}: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper"),
                            preview_text(&result.content, 180)
                        ));
                    }
                    Err(err) => {
                        record["read_level"] = json!("mineru_failed");
                        record["tool"] = json!("builtin.mineru_parse_document");
                        record["error"] = json!(err.to_string());
                        notes.push(format!(
                            "{}. MinerU failed for {}: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper"),
                            err
                        ));
                    }
                }
            } else {
                let call = ToolCall {
                    id: format!(
                        "workflow-{}-read_papers-extract-{}",
                        self.task_id,
                        index + 1
                    ),
                    tool_id: "builtin.anysearch".to_string(),
                    name: "anysearch".to_string(),
                    arguments: json!({
                        "action": "extract",
                        "url": record["url"].as_str().unwrap_or_default()
                    }),
                };
                match self.runtime.execute_tool_call(
                    call,
                    self.auto_approve,
                    Some(self.task_id.clone()),
                    Some(self.task_id.clone()),
                ) {
                    Ok(result) if !result.is_error => {
                        if title_needs_replacement(record["title"].as_str().unwrap_or_default()) {
                            if let Some(title) = infer_title_from_text(&result.content) {
                                record["title"] = json!(title);
                            }
                        }
                        record["read_level"] = json!("web_extract");
                        record["tool"] = json!("builtin.anysearch.extract");
                        record["content"] = json!(preview_text(&result.content, 8000));
                        notes.push(format!(
                            "{}. extracted web page text, not a PDF full read: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper")
                        ));
                    }
                    Ok(result) => {
                        record["read_level"] = json!("extract_failed");
                        record["tool"] = json!("builtin.anysearch.extract");
                        record["error"] = json!(result.content);
                    }
                    Err(err) => {
                        record["read_level"] = json!("extract_failed");
                        record["tool"] = json!("builtin.anysearch.extract");
                        record["error"] = json!(err.to_string());
                    }
                }
            }
            papers.push(record);
        }

        Ok(WorkflowStepOutcome::Success(json!({
            "content": notes.join("\n"),
            "papers": papers,
            "read_level": if notes.iter().any(|note| note.contains("full PDF parsed")) { "full_or_partial_pdf" } else { "extract_or_snippets_only" }
        })))
    }

    fn run_adaptive_loop(
        &self,
        prompt: &str,
        context: &WorkflowContext,
    ) -> PwResult<WorkflowStepOutcome> {
        let settings = self.runtime.settings();
        if let Err(err) = self.runtime.refresh_registry() {
            self.runtime.emit(
                "registry_refresh_failed",
                Some(self.task_id.clone()),
                Some(self.task_id.clone()),
                json!({ "error": err.to_string() }),
            );
        }
        let snapshot = self.runtime.snapshot();
        let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));
        let model_settings = settings.resolved_model_settings()?;
        let model_client = AnyModelClient::from_settings(&model_settings)?;
        let user_input = workflow_model_prompt(prompt, context);
        let mut context_pack = ContextBuilder::new().build_with_sources_and_memory(
            user_input.clone(),
            &snapshot,
            Some(settings.pwcli_home.clone()),
            default_local_context_paths(),
            &settings.memory,
        );
        remove_agent_cli_tools_from_context_pack(&mut context_pack);
        restrict_context_pack_to_tool_ids(
            &mut context_pack,
            &[
                "builtin.anysearch",
                "builtin.web_fetch",
                "builtin.mineru_parse_document",
                "builtin.local_file_index",
            ],
        );
        ensure_route_tool_ids(
            &mut context_pack,
            &snapshot,
            &[
                "builtin.anysearch",
                "builtin.web_fetch",
                "builtin.mineru_parse_document",
                "builtin.local_file_index",
            ],
        );
        audit.record(AuditEvent::ContextPackBuilt {
            context_id: context_pack.id.clone(),
            selected_tool_ids: context_pack.selected_tool_ids.clone(),
        });
        self.runtime.emit_context(&self.task_id, &context_pack);

        let show_thinking = model_settings.show_thinking;
        let event_runtime = self.runtime.clone();
        let event_task_id = self.task_id.clone();
        let mut planner = StreamingModelPlanner::new(
            &model_client,
            model_settings.model.clone(),
            ThinkingConfig {
                enabled: model_settings.thinking_enabled,
                budget_tokens: Some(1024),
            },
            move |event| {
                emit_model_event(&event_runtime, &event_task_id, event, show_thinking);
            },
        )
        .max_tokens(model_settings.max_output_tokens)
        .stream(model_settings.stream)
        .system(adaptive_loop_system_prompt());

        let policy = DefaultPolicyGuard::default().with_rules(load_rule_texts(&settings));
        let approval = ServiceApproval {
            runtime: self.runtime.clone(),
            run_id: Some(self.task_id.clone()),
            task_id: Some(self.task_id.clone()),
        };
        let mut graph_events = ServiceGraphEventSink {
            runtime: self.runtime.clone(),
            run_id: self.task_id.clone(),
        };
        let tool_context = ToolExecutionContext {
            runtime_tasks: Some(self.runtime.inner.runtime_tasks.clone()),
            ..ToolExecutionContext::default()
        };
        let mut services =
            GraphRunServices::new(&policy, &audit, Some(&approval), &mut graph_events)
                .with_tool_context(tool_context);
        let graph = GraphExecutor::builder()
            .max_rounds(settings.max_rounds.max(4))
            .build();
        let summary = graph.run_with_planner_and_events(
            GraphRunRequest {
                user_input,
                context_pack,
            },
            &snapshot,
            &mut planner,
            &mut services,
        )?;

        if summary.state.status == GraphStatus::Interrupted {
            if let Some(interrupt) = summary.state.interrupt {
                return Ok(WorkflowStepOutcome::Interrupt {
                    prompt: interrupt.prompt,
                    reason: interrupt.reason,
                });
            }
            return Ok(WorkflowStepOutcome::Failure(
                "adaptive loop was interrupted".to_string(),
            ));
        }
        if summary.state.status == GraphStatus::Cancelled {
            return Ok(WorkflowStepOutcome::Failure(
                "adaptive loop was cancelled".to_string(),
            ));
        }
        let content = summary.state.last_content.trim().to_string();
        if content.is_empty() {
            return Ok(WorkflowStepOutcome::Failure(
                "adaptive loop returned no content".to_string(),
            ));
        }
        let mut graph_tool_results = summary.state.tool_results.clone();
        let material_artifacts = archive_workflow_tool_results(
            &self.runtime.inner.runtime_tasks,
            &self.task_id,
            &mut graph_tool_results,
        )?;
        let tool_results = graph_tool_results
            .iter()
            .map(|result| {
                json!({
                    "is_error": result.is_error,
                    "content": preview_text(&result.content, 800),
                    "metadata": result.metadata,
                    "artifacts": result.artifacts,
                })
            })
            .collect::<Vec<_>>();
        Ok(WorkflowStepOutcome::Success(json!({
            "content": content,
            "status": format!("{:?}", summary.state.status),
            "round_count": summary.state.round_count,
            "tool_result_count": tool_results.len(),
            "tool_results": tool_results,
            "materials": material_artifacts,
        })))
    }

    fn run_model_node(
        &self,
        prompt: &str,
        context: &WorkflowContext,
    ) -> PwResult<WorkflowStepOutcome> {
        let settings = self.runtime.settings();
        let model_settings = settings.resolved_model_settings()?;
        let model_client = AnyModelClient::from_settings(&model_settings)?;
        let show_thinking = model_settings.show_thinking;
        let event_runtime = self.runtime.clone();
        let task_id = self.task_id.clone();
        self.runtime.emit(
            "model_started",
            Some(task_id.clone()),
            Some(task_id.clone()),
            json!({}),
        );
        let response = model_client.stream(
            &ModelRequest {
                model: model_settings.model.clone(),
                messages: vec![ModelMessage {
                    role: ModelRole::User,
                    content: workflow_model_prompt(prompt, context),
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: Vec::new(),
                }],
                system: Some(
                    "You are pwcli's built-in runtime agent. Use the provided workflow context, \
                     do not delegate to external code-agent CLIs, and produce a concrete useful \
                     result for the current workflow node."
                        .to_string(),
                ),
                thinking: ThinkingConfig {
                    enabled: model_settings.thinking_enabled,
                    budget_tokens: Some(1024),
                },
                max_tokens: Some(model_settings.max_output_tokens),
                stream: model_settings.stream,
                tools: Vec::new(),
            },
            &mut |event| {
                emit_model_event(&event_runtime, &task_id, event, show_thinking);
            },
        )?;
        let content = response.content.trim().to_string();
        if content.is_empty() {
            return Ok(WorkflowStepOutcome::Failure(
                "model turn returned no content".to_string(),
            ));
        }
        Ok(WorkflowStepOutcome::Success(json!({
            "content": content,
            "usage": response.usage,
        })))
    }

    fn run_approval_node(&self, prompt: &str) -> PwResult<WorkflowStepOutcome> {
        if self.auto_approve {
            return Ok(WorkflowStepOutcome::Success(json!({ "approved": true })));
        }
        let call = ToolCall {
            id: format!("workflow-approval-{}", self.task_id),
            tool_id: "workflow.approval".to_string(),
            name: "workflow_approval".to_string(),
            arguments: json!({ "prompt": prompt }),
        };
        let approved = ServiceApproval {
            runtime: self.runtime.clone(),
            run_id: Some(self.task_id.clone()),
            task_id: Some(self.task_id.clone()),
        }
        .ask_user(prompt, &call);
        if approved {
            Ok(WorkflowStepOutcome::Success(json!({ "approved": true })))
        } else {
            Ok(WorkflowStepOutcome::Interrupt {
                prompt: prompt.to_string(),
                reason: "workflow approval was not granted".to_string(),
            })
        }
    }
}

struct ServiceGraphEventSink {
    runtime: ServiceRuntime,
    run_id: String,
}

impl GraphEventSink for ServiceGraphEventSink {
    fn emit(&mut self, event: GraphEvent) {
        let (kind, data) = match event {
            GraphEvent::GraphStarted => ("graph_started", json!({})),
            GraphEvent::ContextBuilt { context_id } => {
                ("context_built", json!({ "context_id": context_id }))
            }
            GraphEvent::ToolSelectionStarted => ("tool_selection_started", json!({})),
            GraphEvent::ToolSelected { tool_id } => {
                ("tool_selected", json!({ "tool_id": tool_id }))
            }
            GraphEvent::ModelStarted => ("model_started", json!({})),
            GraphEvent::ModelCompleted { output_chars } => {
                ("model_completed", json!({ "output_chars": output_chars }))
            }
            GraphEvent::ToolCallStarted {
                call_id,
                tool_id,
                name,
            } => (
                "tool_started",
                json!({ "call_id": call_id, "tool_id": tool_id, "name": name }),
            ),
            GraphEvent::ToolPolicyDecision {
                call_id,
                tool_id,
                name,
                decision,
            } => (
                "tool_policy_decision",
                json!({
                    "call_id": call_id,
                    "tool_id": tool_id,
                    "name": name,
                    "decision": decision
                }),
            ),
            GraphEvent::ToolCompleted {
                call_id,
                tool_id,
                name,
                is_error,
                content_preview,
                metadata,
            } => (
                "tool_completed",
                json!({
                    "call_id": call_id,
                    "tool_id": tool_id,
                    "name": name,
                    "is_error": is_error,
                    "content": content_preview,
                    "metadata": metadata,
                }),
            ),
            GraphEvent::ToolRuntimeEvent { call_id, event } => (
                "tool_runtime_event",
                json!({ "call_id": call_id, "event": event }),
            ),
            GraphEvent::UserApprovalRequested { prompt } => {
                ("user_approval_requested", json!({ "prompt": prompt }))
            }
            GraphEvent::GraphInterrupted { interrupt } => {
                ("run_interrupted", json!({ "interrupt": interrupt }))
            }
            GraphEvent::GraphCompleted => ("graph_completed", json!({})),
        };
        self.runtime
            .emit(kind, Some(self.run_id.clone()), None, data);
    }
}

fn emit_model_event(
    runtime: &ServiceRuntime,
    run_id: &str,
    event: ModelEvent,
    show_thinking: bool,
) {
    match event {
        ModelEvent::TextDelta(delta) => runtime.emit(
            "model_delta",
            Some(run_id.to_string()),
            None,
            json!({ "delta": delta }),
        ),
        ModelEvent::ThinkingDelta(delta) if show_thinking => runtime.emit(
            "thinking_delta",
            Some(run_id.to_string()),
            None,
            json!({ "delta": delta }),
        ),
        ModelEvent::ToolCall(call) => runtime.emit(
            "model_tool_call",
            Some(run_id.to_string()),
            None,
            json!({ "tool_call": call }),
        ),
        ModelEvent::Usage(usage) => runtime.emit(
            "model_usage",
            Some(run_id.to_string()),
            None,
            json!({ "usage": usage }),
        ),
        ModelEvent::Done => runtime.emit("model_done", Some(run_id.to_string()), None, json!({})),
        ModelEvent::ThinkingDelta(_) => {}
    }
}

fn runtime_event_task_id(event: &RuntimeTaskEvent) -> String {
    match event {
        RuntimeTaskEvent::Started { task_id }
        | RuntimeTaskEvent::Progress { task_id, .. }
        | RuntimeTaskEvent::Output { task_id, .. }
        | RuntimeTaskEvent::Structured { task_id, .. }
        | RuntimeTaskEvent::Completed { task_id, .. }
        | RuntimeTaskEvent::Failed { task_id, .. }
        | RuntimeTaskEvent::Cancelled { task_id }
        | RuntimeTaskEvent::TimedOut { task_id }
        | RuntimeTaskEvent::CompactCompleted { task_id, .. }
        | RuntimeTaskEvent::VerificationRecorded { task_id, .. }
        | RuntimeTaskEvent::WorkflowNodeStarted { task_id, .. }
        | RuntimeTaskEvent::WorkflowNodeCompleted { task_id, .. } => task_id.clone(),
    }
}

fn parse_compact_scope(scope: Option<&str>) -> ApiResult<CompactScope> {
    match scope.unwrap_or("both") {
        "pwcli" | "pwcli_only" => Ok(CompactScope::PwcliOnly),
        "agent" | "agent_only" => Ok(CompactScope::AgentOnly),
        "both" => Ok(CompactScope::Both),
        other => Err(ApiError::bad_request(format!(
            "unknown compact scope {other}; expected pwcli, agent, or both"
        ))),
    }
}

fn task_decomposition_from_workflow(
    goal: &str,
    requested_kind: WorkflowPlanKind,
    resolved_kind: WorkflowPlanKind,
    workflow: &GraphWorkflow,
) -> TaskDecomposition {
    let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    for edge in &workflow.edges {
        outgoing
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }

    queue.push_back(workflow.start.clone());
    while let Some(node_id) = queue.pop_front() {
        if !visited.insert(node_id.clone()) {
            continue;
        }
        order.push(node_id.clone());
        for next in outgoing.get(&node_id).into_iter().flatten() {
            if !visited.contains(next) {
                queue.push_back(next.clone());
            }
        }
    }

    for node_id in workflow.nodes.keys() {
        if !visited.contains(node_id) {
            order.push(node_id.clone());
        }
    }

    let steps = order
        .into_iter()
        .filter_map(|node_id| workflow.nodes.get(&node_id).map(|node| (node_id, node)))
        .map(|(node_id, node)| TaskDecompositionStep {
            id: node_id.clone(),
            label: node.label.clone(),
            kind: task_decomposition_step_kind(&node.kind).to_string(),
            summary: task_node_summary(&node.kind),
            to: outgoing.get(&node_id).cloned().unwrap_or_default(),
        })
        .collect();

    TaskDecomposition {
        goal: goal.to_string(),
        requested_kind,
        resolved_kind,
        workflow_name: workflow.name.clone(),
        node_count: workflow.nodes.len(),
        edge_count: workflow.edges.len(),
        generated_at: service_now_millis(),
        steps,
    }
}

fn task_decomposition_step_kind(kind: &WorkflowNodeKind) -> &'static str {
    match kind {
        WorkflowNodeKind::AgentTask { .. } => "agent_task",
        WorkflowNodeKind::ToolCall { .. } => "tool_call",
        WorkflowNodeKind::Approval { .. } => "approval",
        WorkflowNodeKind::ModelTurn { .. } => "model_turn",
        WorkflowNodeKind::ResearchReadPapers { .. } => "research_read_papers",
        WorkflowNodeKind::AdaptiveLoop { .. } => "adaptive_loop",
        WorkflowNodeKind::Join => "join",
        WorkflowNodeKind::SubWorkflow { .. } => "sub_workflow",
        WorkflowNodeKind::End => "end",
    }
}

fn task_node_summary(kind: &WorkflowNodeKind) -> String {
    match kind {
        WorkflowNodeKind::AgentTask {
            agent,
            mode,
            prompt,
        } => {
            format!("{} / {}: {}", agent, mode, preview_text(prompt, 120))
        }
        WorkflowNodeKind::ToolCall { tool_id, arguments } => {
            format!(
                "tool {} {}",
                tool_id,
                preview_text(&arguments.to_string(), 120)
            )
        }
        WorkflowNodeKind::Approval { prompt } => {
            format!("approval: {}", preview_text(prompt, 120))
        }
        WorkflowNodeKind::ModelTurn { prompt } => {
            format!("model_turn: {}", preview_text(prompt, 120))
        }
        WorkflowNodeKind::ResearchReadPapers { max_papers } => {
            format!("read up to {max_papers} paper candidates")
        }
        WorkflowNodeKind::AdaptiveLoop { prompt } => {
            format!("adaptive_loop: {}", preview_text(prompt, 120))
        }
        WorkflowNodeKind::SubWorkflow { workflow } => {
            format!("sub_workflow: {}", workflow.name)
        }
        WorkflowNodeKind::Join => "join".to_string(),
        WorkflowNodeKind::End => "end".to_string(),
    }
}

fn service_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn safe_rule_path(settings: &Settings, name: &str) -> ApiResult<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(ApiError::bad_request(
            "rule name must be a single file name under ~/.pwcli/rules",
        ));
    }
    Ok(settings.pwcli_home.join("rules").join(trimmed))
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let start = len.saturating_sub(keep);
    let tail: String = text.chars().skip(start).collect();
    format!("…{tail}")
}

fn preview_text(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    let mut out = text.chars().take(max_chars).collect::<String>();
    if len > max_chars {
        out.push_str("...");
    }
    out
}

fn workflow_agent_prompt(prompt: &str, context: &WorkflowContext) -> String {
    if context.outputs.is_empty() {
        return prompt.to_string();
    }
    let outputs =
        serde_json::to_string_pretty(&context.outputs).unwrap_or_else(|_| "{}".to_string());
    format!(
        "{prompt}\n\nPrior workflow node outputs follow. Use them as evidence and do not pretend unavailable data was collected:\n```json\n{}\n```",
        preview_text(&outputs, 12000)
    )
}

fn workflow_model_prompt(prompt: &str, context: &WorkflowContext) -> String {
    if context.outputs.is_empty() {
        return prompt.to_string();
    }
    let outputs =
        serde_json::to_string_pretty(&context.outputs).unwrap_or_else(|_| "{}".to_string());
    format!(
        "{prompt}\n\nPrior workflow node outputs follow. Use them as evidence, cite uncertainty, and do not pretend unavailable data was collected:\n```json\n{}\n```",
        preview_text(&outputs, 16000)
    )
}

#[derive(Debug, Clone)]
struct ResearchPaperCandidate {
    title: String,
    url: String,
    pdf_url: Option<String>,
}

fn extract_research_paper_candidates(
    content: &str,
    max_papers: usize,
) -> Vec<ResearchPaperCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let lines = content.lines().collect::<Vec<_>>();
    for (idx, line) in lines.iter().enumerate() {
        let Some(title) = line.trim().strip_prefix("### ") else {
            continue;
        };
        let title = title
            .split_once(". ")
            .map(|(_, title)| title)
            .unwrap_or(title)
            .trim();
        let mut url = None;
        for lookahead in lines.iter().skip(idx + 1).take(5) {
            let trimmed = lookahead.trim();
            if let Some(found) = trimmed.strip_prefix("- **URL**:") {
                url = Some(found.trim().to_string());
                break;
            }
        }
        let Some(url) = url else {
            continue;
        };
        if !looks_like_paper_result(title, &url) {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        candidates.push(ResearchPaperCandidate {
            title: title.to_string(),
            pdf_url: paper_pdf_url(&url),
            url,
        });
        if candidates.len() >= max_papers {
            break;
        }
    }
    candidates
}

fn looks_like_paper_result(title: &str, url: &str) -> bool {
    let haystack = format!("{} {}", title, url).to_ascii_lowercase();
    haystack.contains("arxiv.org")
        || haystack.contains(".pdf")
        || haystack.contains("paper")
        || haystack.contains("论文")
        || haystack.contains("proceedings")
        || haystack.contains("acm.org")
        || haystack.contains("openreview.net")
}

fn paper_pdf_url(url: &str) -> Option<String> {
    let clean = url.trim().trim_end_matches([')', '.', ',', ';']);
    let lower = clean.to_ascii_lowercase();
    if lower.ends_with(".pdf") || lower.contains(".pdf?") {
        return Some(clean.to_string());
    }
    if let Some(id) = arxiv_id(clean) {
        return Some(format!("https://arxiv.org/pdf/{id}.pdf"));
    }
    None
}

fn arxiv_id(url: &str) -> Option<String> {
    let marker = "arxiv.org/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];
    let rest = rest
        .strip_prefix("abs/")
        .or_else(|| rest.strip_prefix("html/"))
        .or_else(|| rest.strip_prefix("pdf/"))?;
    let mut id = rest
        .split(['?', '#', '/'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if let Some(stripped) = id.strip_suffix(".pdf") {
        id = stripped.to_string();
    }
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn title_needs_replacement(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }
    let normalized = trimmed
        .trim_end_matches(".pdf")
        .trim_end_matches(".zip")
        .trim_matches(|ch: char| ch == '[' || ch == ']' || ch == '(' || ch == ')');
    let has_letter = normalized.chars().any(|ch| ch.is_alphabetic());
    let mostly_id = normalized
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | 'v' | 'V' | '-' | '_'));
    let uuid_like = normalized.len() >= 24
        && normalized
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-')
        && normalized.chars().filter(|ch| *ch == '-').count() >= 2;
    let hex_blob_like = normalized.len() >= 16
        && normalized
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-');
    !has_letter || mostly_id || uuid_like || hex_blob_like
}

fn infer_title_from_text(text: &str) -> Option<String> {
    for line in text.lines().take(80) {
        let title = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_matches(['*', '_']);
        if title.len() < 8 || title.len() > 220 {
            continue;
        }
        if title_needs_replacement(title) {
            continue;
        }
        if title.to_ascii_lowercase().contains("abstract") {
            continue;
        }
        return Some(title.to_string());
    }
    None
}

fn remove_agent_cli_tools_from_context_pack(context_pack: &mut ContextPack) {
    context_pack
        .selected_tool_ids
        .retain(|tool_id| !tool_id.starts_with("agent_cli."));
    context_pack
        .tool_selection_plan
        .details
        .retain(|detail| !detail.tool_id.starts_with("agent_cli."));
    for step in &mut context_pack.tool_selection_plan.steps {
        step.tool_ids
            .retain(|tool_id| !tool_id.starts_with("agent_cli."));
    }
}

fn restrict_context_pack_to_tool_ids(context_pack: &mut ContextPack, tool_ids: &[&str]) {
    context_pack
        .selected_tool_ids
        .retain(|tool_id| tool_ids.contains(&tool_id.as_str()));
    context_pack
        .tool_selection_plan
        .details
        .retain(|detail| tool_ids.contains(&detail.tool_id.as_str()));
    for step in &mut context_pack.tool_selection_plan.steps {
        step.tool_ids
            .retain(|tool_id| tool_ids.contains(&tool_id.as_str()));
    }
    context_pack
        .tool_selection_plan
        .steps
        .retain(|step| !step.tool_ids.is_empty());
}

fn ensure_route_tool_ids(
    context_pack: &mut ContextPack,
    snapshot: &ToolRegistrySnapshot,
    tool_ids: &[&str],
) {
    let mut added = Vec::new();
    for tool_id in tool_ids {
        if snapshot.get(tool_id).is_none() {
            continue;
        }
        if context_pack
            .selected_tool_ids
            .iter()
            .any(|selected| selected == tool_id)
        {
            continue;
        }
        context_pack.selected_tool_ids.push((*tool_id).to_string());
        added.push((*tool_id).to_string());
    }
    if !added.is_empty() {
        context_pack.summary.push_str(&format!(
            "\n\nRoute-provided tools available for adaptive execution: {}",
            added.join(", ")
        ));
    }
}

fn adaptive_loop_system_prompt() -> &'static str {
    "You are pwcli's adaptive workflow agent. You are inside a larger visible workflow node, \
     but you may call available tools over multiple rounds. Choose tools based on evidence, \
     not on a fixed recipe. After each tool result, decide whether another tool is needed, \
     whether a different query is needed, whether approval is needed, or whether you can answer. \
     Never call code-agent/delegation tools unless they are explicitly selected and the task is \
     code work. For research, prefer primary sources, use search before extraction, use PDF/MinerU \
     for arXiv or PDF papers before claiming a full read, and label evidence as full_pdf_mineru, \
     web_extract, or snippet_only. If a tool fails, explain the fallback and continue when useful."
}

fn read_task_result_value(runtime: &RuntimeTaskManager, task_id: &str) -> Value {
    let path = runtime.task_dir(task_id).join("result.json");
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}

fn task_failure_preview(runtime: &RuntimeTaskManager, task_id: &str) -> String {
    let dir = runtime.task_dir(task_id);
    let stderr = fs::read_to_string(dir.join("stderr.log")).unwrap_or_default();
    if !stderr.trim().is_empty() {
        return tail_chars(&stderr, 2400);
    }
    let stdout = fs::read_to_string(dir.join("stdout.log")).unwrap_or_default();
    if !stdout.trim().is_empty() {
        return tail_chars(&stdout, 2400);
    }
    format!("runtime task {task_id} did not complete successfully")
}

fn annotate_service_tool_result(result: &mut ToolResult, call: &ToolCall) {
    let call_info = json!({
        "call_id": call.id,
        "tool_id": call.tool_id,
        "name": call.name,
        "arguments": call.arguments,
    });
    match &mut result.metadata {
        Value::Object(map) => {
            map.insert("_pwcli_call".to_string(), call_info);
        }
        other => {
            result.metadata = json!({
                "value": other.clone(),
                "_pwcli_call": call_info,
            });
        }
    }
}

fn archive_workflow_tool_results(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    results: &mut [ToolResult],
) -> PwResult<Vec<Value>> {
    let mut artifacts = Vec::new();
    for result in results {
        if should_archive_mineru_result(result) {
            match archive_mineru_tool_result(runtime, task_id, result) {
                Ok(Some(metadata)) => artifacts.push(metadata),
                Ok(None) => {}
                Err(err) => {
                    let failure = write_failed_mineru_archive(runtime, task_id, result, &err)?;
                    artifacts.push(failure);
                }
            }
        }
    }
    Ok(artifacts)
}

fn ensure_task_material_archive(runtime: &RuntimeTaskManager, task_id: &str) -> PwResult<()> {
    if !list_mineru_materials(runtime, task_id)?.is_empty() {
        return Ok(());
    }
    let state_path = runtime.task_dir(task_id).join("workflow_state.json");
    if !state_path.is_file() {
        return Ok(());
    }
    let mut summary = load_workflow_summary(runtime, task_id)?;
    let mut changed = false;
    let mut legacy_counter = 0usize;
    for output in summary.outputs.values_mut() {
        archive_legacy_mineru_values(runtime, task_id, output, &mut changed, &mut legacy_counter)?;
    }
    if changed {
        fs::write(state_path, serde_json::to_string_pretty(&summary)?)?;
    }
    Ok(())
}

fn archive_legacy_mineru_values(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    value: &mut Value,
    changed: &mut bool,
    legacy_counter: &mut usize,
) -> PwResult<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                archive_legacy_mineru_values(runtime, task_id, item, changed, legacy_counter)?;
            }
        }
        Value::Object(map) => {
            if let Some(metadata) = map.get("metadata").cloned() {
                if legacy_mineru_metadata(&metadata) {
                    let mut result = ToolResult {
                        content: map
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        is_error: map
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        preview: None,
                        full_content_ref: None,
                        metadata: ensure_legacy_call_info(metadata, legacy_counter),
                        artifacts: Vec::new(),
                        audit_hints: json!({}),
                    };
                    let _ = archive_mineru_tool_result(runtime, task_id, &mut result)?;
                    map.insert("metadata".to_string(), result.metadata.clone());
                    map.insert("artifacts".to_string(), json!(result.artifacts));
                    map.insert(
                        "content".to_string(),
                        json!(preview_text(&result.content, 800)),
                    );
                    *changed = true;
                }
            } else {
                let self_metadata = Value::Object(map.clone());
                if !legacy_mineru_metadata(&self_metadata) {
                    for child in map.values_mut() {
                        archive_legacy_mineru_values(
                            runtime,
                            task_id,
                            child,
                            changed,
                            legacy_counter,
                        )?;
                    }
                    return Ok(());
                }
                let mut result = ToolResult {
                    content: String::new(),
                    is_error: false,
                    preview: None,
                    full_content_ref: None,
                    metadata: ensure_legacy_call_info(self_metadata, legacy_counter),
                    artifacts: Vec::new(),
                    audit_hints: json!({}),
                };
                let _ = archive_mineru_tool_result(runtime, task_id, &mut result)?;
                if let Value::Object(next) = result.metadata {
                    *map = next;
                }
                *changed = true;
                return Ok(());
            }
            for child in map.values_mut() {
                archive_legacy_mineru_values(runtime, task_id, child, changed, legacy_counter)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn legacy_mineru_metadata(value: &Value) -> bool {
    value.get("artifact_id").and_then(Value::as_str).is_none()
        && (value.get("markdown").and_then(Value::as_str).is_some()
            || value.get("full_zip_url").and_then(Value::as_str).is_some()
            || value
                .pointer("/_pwcli_call/tool_id")
                .and_then(Value::as_str)
                == Some("builtin.mineru_parse_document"))
}

fn ensure_legacy_call_info(mut metadata: Value, counter: &mut usize) -> Value {
    let has_call = metadata
        .pointer("/_pwcli_call/call_id")
        .and_then(Value::as_str)
        .is_some();
    if has_call {
        return metadata;
    }
    *counter += 1;
    let call_info = json!({
        "call_id": format!("legacy-mineru-{counter}"),
        "tool_id": "builtin.mineru_parse_document",
        "name": "mineru_parse_document",
        "arguments": {
            "url": metadata.get("source_url")
                .or_else(|| metadata.get("url"))
                .or_else(|| metadata.get("pdf_url"))
                .cloned()
                .unwrap_or(Value::Null)
        }
    });
    match &mut metadata {
        Value::Object(map) => {
            map.insert("_pwcli_call".to_string(), call_info);
            metadata
        }
        other => json!({
            "value": other.clone(),
            "_pwcli_call": call_info,
        }),
    }
}

fn should_archive_mineru_result(result: &ToolResult) -> bool {
    result
        .metadata
        .pointer("/_pwcli_call/tool_id")
        .and_then(Value::as_str)
        == Some("builtin.mineru_parse_document")
        || result.metadata.get("full_zip_url").is_some()
        || result.metadata.get("markdown").is_some()
}

fn archive_mineru_tool_result(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    result: &mut ToolResult,
) -> PwResult<Option<Value>> {
    let original = result.metadata.clone();
    if original
        .get("artifact_id")
        .and_then(Value::as_str)
        .is_some()
    {
        return Ok(Some(original));
    }
    let call_id = original
        .pointer("/_pwcli_call/call_id")
        .and_then(Value::as_str)
        .unwrap_or("mineru-call");
    let safe_call_id = safe_path_segment(call_id);
    let artifact_id = format!("{task_id}--mineru--{safe_call_id}");
    let base_dir = runtime
        .task_dir(task_id)
        .join("materials/mineru")
        .join(&safe_call_id);
    let raw_dir = base_dir.join("raw");
    let extracted_dir = base_dir.join("extracted");
    let images_dir = extracted_dir.join("images");
    let tables_dir = extracted_dir.join("tables");
    let others_dir = extracted_dir.join("others");
    for dir in [&raw_dir, &images_dir, &tables_dir, &others_dir] {
        fs::create_dir_all(dir)?;
    }

    let source = mineru_source_json(task_id, &artifact_id, &original);
    write_pretty_json(base_dir.join("source.json"), &source)?;
    write_pretty_json(raw_dir.join("result.json"), &original)?;

    let zip_path = raw_dir.join("result.zip");
    if let Some(url) = original.get("full_zip_url").and_then(Value::as_str) {
        if !url.trim().is_empty() && !zip_path.is_file() {
            download_file(url, &zip_path)?;
        }
    }

    let mut markdown = original
        .get("markdown")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut image_count = 0usize;
    let mut table_count = 0usize;
    let mut other_count = 0usize;
    if zip_path.is_file() {
        let extracted = extract_mineru_zip(&zip_path, &extracted_dir)?;
        if extracted.markdown.chars().count() > markdown.chars().count() {
            markdown = extracted.markdown;
        }
        image_count = extracted.image_count;
        table_count = extracted.table_count;
        other_count = extracted.other_count;
    }

    let document_path = extracted_dir.join("document.md");
    if !markdown.trim().is_empty() {
        fs::write(&document_path, &markdown)?;
    }

    let inferred_title = infer_title_from_text(&markdown);
    let original_title = original
        .get("canonical_title")
        .and_then(Value::as_str)
        .or_else(|| original.get("title").and_then(Value::as_str))
        .or_else(|| source.get("title").and_then(Value::as_str))
        .unwrap_or_default();
    let canonical_title = if title_needs_replacement(original_title) {
        inferred_title.unwrap_or_else(|| fallback_material_title(&original))
    } else {
        original_title.to_string()
    };
    let markdown_chars = markdown.chars().count();
    let evidence_level = if markdown.trim().is_empty() {
        if result.is_error {
            "mineru_failed"
        } else {
            "mineru_parsed_no_markdown"
        }
    } else {
        "full_pdf_mineru"
    };
    let metadata = json!({
        "artifact_id": artifact_id,
        "kind": "mineru",
        "task_id": task_id,
        "tool_call_id": call_id,
        "tool_id": original.pointer("/_pwcli_call/tool_id").and_then(Value::as_str).unwrap_or("builtin.mineru_parse_document"),
        "mineru_task_id": original.get("task_id").and_then(Value::as_str),
        "canonical_title": canonical_title,
        "markdown_chars": markdown_chars,
        "image_count": image_count,
        "table_count": table_count,
        "other_count": other_count,
        "read_level": evidence_level,
        "evidence_level": evidence_level,
        "source": source,
        "created_at": Utc::now().to_rfc3339(),
        "artifact_paths": {
            "base_dir": path_string(&base_dir),
            "source_json": path_string(&base_dir.join("source.json")),
            "raw_zip": if zip_path.is_file() { Value::String(path_string(&zip_path)) } else { Value::Null },
            "raw_result_json": path_string(&raw_dir.join("result.json")),
            "document_md": if document_path.is_file() { Value::String(path_string(&document_path)) } else { Value::Null },
            "images_dir": path_string(&images_dir),
            "tables_dir": path_string(&tables_dir),
            "others_dir": path_string(&others_dir),
        }
    });
    write_pretty_json(base_dir.join("metadata.json"), &metadata)?;

    let mut sanitized = original.clone();
    if let Value::Object(map) = &mut sanitized {
        map.remove("markdown");
        map.remove("result");
        map.insert("artifact_id".to_string(), metadata["artifact_id"].clone());
        map.insert(
            "canonical_title".to_string(),
            metadata["canonical_title"].clone(),
        );
        map.insert("markdown_chars".to_string(), json!(markdown_chars));
        map.insert("image_count".to_string(), json!(image_count));
        map.insert("table_count".to_string(), json!(table_count));
        map.insert("read_level".to_string(), json!(evidence_level));
        map.insert("evidence_level".to_string(), json!(evidence_level));
        map.insert(
            "artifact_paths".to_string(),
            metadata["artifact_paths"].clone(),
        );
        map.insert(
            "metadata_path".to_string(),
            json!(path_string(&base_dir.join("metadata.json"))),
        );
    } else {
        sanitized = metadata.clone();
    }

    result.metadata = sanitized.clone();
    result.content = serde_json::to_string(&sanitized)?;
    result.preview = Some(format!(
        "{} · {} chars · {} images · {}",
        metadata["canonical_title"]
            .as_str()
            .unwrap_or("MinerU document"),
        markdown_chars,
        image_count,
        evidence_level
    ));
    result.full_content_ref = document_path.is_file().then(|| path_string(&document_path));
    result.artifacts.push(ToolArtifact {
        path: base_dir.join("metadata.json"),
        kind: ToolArtifactKind::Other,
        title: Some(format!(
            "{} metadata",
            metadata["canonical_title"].as_str().unwrap_or("MinerU")
        )),
        media_type: Some("application/json".to_string()),
        preview: Some("MinerU artifact metadata".to_string()),
        full_content_ref: Some(path_string(&base_dir.join("metadata.json"))),
        provenance: Some(ToolArtifactProvenance {
            source: "mineru_parse_document".to_string(),
            uri: source
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_call_id: Some(call_id.to_string()),
            metadata: metadata.clone(),
        }),
    });
    if document_path.is_file() {
        result.artifacts.push(ToolArtifact {
            path: document_path.clone(),
            kind: ToolArtifactKind::Report,
            title: Some(
                metadata["canonical_title"]
                    .as_str()
                    .unwrap_or("document")
                    .to_string(),
            ),
            media_type: Some("text/markdown".to_string()),
            preview: Some(preview_text(&markdown, 500)),
            full_content_ref: Some(path_string(&document_path)),
            provenance: Some(ToolArtifactProvenance {
                source: "mineru_parse_document".to_string(),
                uri: source
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tool_call_id: Some(call_id.to_string()),
                metadata: metadata.clone(),
            }),
        });
    }
    Ok(Some(metadata))
}

fn write_failed_mineru_archive(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    result: &mut ToolResult,
    err: &dyn std::fmt::Display,
) -> PwResult<Value> {
    let original = result.metadata.clone();
    let call_id = original
        .pointer("/_pwcli_call/call_id")
        .and_then(Value::as_str)
        .unwrap_or("mineru-call");
    let safe_call_id = safe_path_segment(call_id);
    let artifact_id = format!("{task_id}--mineru--{safe_call_id}");
    let base_dir = runtime
        .task_dir(task_id)
        .join("materials/mineru")
        .join(&safe_call_id);
    fs::create_dir_all(base_dir.join("raw"))?;
    let source = mineru_source_json(task_id, &artifact_id, &original);
    write_pretty_json(base_dir.join("source.json"), &source)?;
    write_pretty_json(base_dir.join("raw/result.json"), &original)?;
    let metadata = json!({
        "artifact_id": artifact_id,
        "kind": "mineru",
        "task_id": task_id,
        "tool_call_id": call_id,
        "canonical_title": fallback_material_title(&original),
        "read_level": "archive_failed",
        "evidence_level": "archive_failed",
        "markdown_chars": 0,
        "image_count": 0,
        "error": err.to_string(),
        "source": source,
        "artifact_paths": {
            "base_dir": path_string(&base_dir),
            "source_json": path_string(&base_dir.join("source.json")),
            "raw_result_json": path_string(&base_dir.join("raw/result.json")),
        }
    });
    write_pretty_json(base_dir.join("metadata.json"), &metadata)?;
    if let Value::Object(map) = &mut result.metadata {
        map.remove("markdown");
        map.remove("result");
        map.insert("artifact_id".to_string(), metadata["artifact_id"].clone());
        map.insert("archive_error".to_string(), json!(err.to_string()));
        map.insert(
            "artifact_paths".to_string(),
            metadata["artifact_paths"].clone(),
        );
    }
    Ok(metadata)
}

struct ExtractedMineruZip {
    markdown: String,
    image_count: usize,
    table_count: usize,
    other_count: usize,
}

fn extract_mineru_zip(zip_path: &Path, extracted_dir: &Path) -> PwResult<ExtractedMineruZip> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| PwError::Message(format!("failed to open MinerU zip: {err}")))?;
    let mut best_markdown = String::new();
    let mut image_count = 0usize;
    let mut table_count = 0usize;
    let mut other_count = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| PwError::Message(format!("failed to read MinerU zip entry: {err}")))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        let file_name = Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            .map(safe_path_segment)
            .unwrap_or_else(|| format!("entry-{index}"));
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt") {
            let text = String::from_utf8_lossy(&bytes).to_string();
            if text.chars().count() > best_markdown.chars().count() {
                best_markdown = text;
            }
        } else if is_image_file(&lower) {
            image_count += 1;
            fs::write(
                unique_child_path(&extracted_dir.join("images"), &file_name),
                bytes,
            )?;
        } else if is_table_file(&lower) {
            table_count += 1;
            fs::write(
                unique_child_path(&extracted_dir.join("tables"), &file_name),
                bytes,
            )?;
        } else {
            other_count += 1;
            fs::write(
                unique_child_path(&extracted_dir.join("others"), &file_name),
                bytes,
            )?;
        }
    }
    Ok(ExtractedMineruZip {
        markdown: best_markdown,
        image_count,
        table_count,
        other_count,
    })
}

fn download_file(url: &str, path: &Path) -> PwResult<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|err| PwError::Message(format!("failed to build download client: {err}")))?;
    let response = client
        .get(url)
        .send()
        .map_err(|err| PwError::Message(format!("failed to download {url}: {err}")))?;
    if !response.status().is_success() {
        return Err(PwError::Message(format!(
            "failed to download {url}: HTTP {}",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .map_err(|err| PwError::Message(format!("failed to read download {url}: {err}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

fn mineru_source_json(task_id: &str, artifact_id: &str, metadata: &Value) -> Value {
    let args = metadata
        .pointer("/_pwcli_call/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "task_id": task_id,
        "artifact_id": artifact_id,
        "tool_call_id": metadata.pointer("/_pwcli_call/call_id").and_then(Value::as_str),
        "tool_id": metadata.pointer("/_pwcli_call/tool_id").and_then(Value::as_str),
        "mineru_task_id": metadata.get("task_id").and_then(Value::as_str),
        "url": args.get("url").and_then(Value::as_str),
        "path": args.get("path").and_then(Value::as_str),
        "full_zip_url": metadata.get("full_zip_url").and_then(Value::as_str),
        "title": fallback_material_title(metadata),
        "source_type": if args.get("url").is_some() { "url" } else if args.get("path").is_some() { "path" } else { "unknown" },
        "created_at": Utc::now().to_rfc3339(),
    })
}

fn fallback_material_title(metadata: &Value) -> String {
    for pointer in [
        "/canonical_title",
        "/title",
        "/_pwcli_call/arguments/title",
        "/_pwcli_call/arguments/url",
        "/_pwcli_call/arguments/path",
        "/full_zip_url",
    ] {
        if let Some(value) = metadata.pointer(pointer).and_then(Value::as_str) {
            let candidate = value
                .split('/')
                .next_back()
                .unwrap_or(value)
                .trim_end_matches(".pdf")
                .trim_end_matches(".zip")
                .trim();
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    "MinerU document".to_string()
}

fn write_pretty_json(path: impl AsRef<Path>, value: &Value) -> PwResult<()> {
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn safe_path_segment(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(['-', '.', '_']).to_string();
    if out.is_empty() {
        "artifact".to_string()
    } else {
        out.chars().take(120).collect()
    }
}

fn unique_child_path(dir: &Path, file_name: &str) -> PathBuf {
    let mut path = dir.join(file_name);
    if !path.exists() {
        return path;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let ext = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    for index in 2..1000 {
        let candidate = if ext.is_empty() {
            dir.join(format!("{stem}-{index}"))
        } else {
            dir.join(format!("{stem}-{index}.{ext}"))
        };
        if !candidate.exists() {
            path = candidate;
            break;
        }
    }
    path
}

fn is_image_file(lower_name: &str) -> bool {
    [".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".bmp"]
        .iter()
        .any(|ext| lower_name.ends_with(ext))
}

fn is_table_file(lower_name: &str) -> bool {
    [".csv", ".tsv", ".xlsx", ".xls", ".html", ".htm"]
        .iter()
        .any(|ext| lower_name.ends_with(ext))
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

fn collect_task_materials(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    task_id: &str,
) -> PwResult<Value> {
    ensure_task_material_archive(runtime, task_id)?;
    ensure_task_report_archive(settings, runtime, task_id)?;
    let root = runtime.task_dir(task_id).join("materials");
    let mineru = list_mineru_materials(runtime, task_id)?;
    let reports = collect_task_reports(runtime, task_id);
    let search = collect_search_materials_from_workflow_state(runtime, task_id);
    let images = mineru
        .iter()
        .filter_map(|metadata| {
            metadata
                .pointer("/artifact_paths/images_dir")
                .and_then(Value::as_str)
        })
        .flat_map(|dir| list_dir_files(Path::new(dir)).unwrap_or_default())
        .collect::<Vec<_>>();
    let markdown = mineru
        .iter()
        .filter(|metadata| {
            metadata
                .pointer("/artifact_paths/document_md")
                .and_then(Value::as_str)
                .is_some()
        })
        .cloned()
        .collect::<Vec<_>>();
    let memory_extraction = read_memory_extraction_status(runtime, task_id).unwrap_or_else(|_| {
        json!({
            "task_id": task_id,
            "papers": [],
        })
    });
    Ok(json!({
        "task_id": task_id,
        "root": path_string(&root),
        "groups": {
            "search": search,
            "pdf": mineru,
            "markdown": markdown,
            "images": images,
            "reports": reports,
            "memory_extraction": memory_extraction,
        }
    }))
}

fn list_mineru_materials(runtime: &RuntimeTaskManager, task_id: &str) -> PwResult<Vec<Value>> {
    let mineru_dir = runtime.task_dir(task_id).join("materials/mineru");
    if !mineru_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for entry in fs::read_dir(mineru_dir)? {
        let entry = entry?;
        let path = entry.path().join("metadata.json");
        if !path.is_file() {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) {
                repair_material_title_from_markdown(&path, &mut value);
                values.push(value);
            }
        }
    }
    values.sort_by(|a, b| {
        a.get("canonical_title")
            .and_then(Value::as_str)
            .cmp(&b.get("canonical_title").and_then(Value::as_str))
    });
    Ok(values)
}

fn repair_material_title_from_markdown(metadata_path: &Path, metadata: &mut Value) {
    let current = metadata
        .get("canonical_title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !title_needs_replacement(current) {
        return;
    }
    let Some(document_path) = metadata
        .pointer("/artifact_paths/document_md")
        .and_then(Value::as_str)
    else {
        return;
    };
    let Ok(markdown) = fs::read_to_string(document_path) else {
        return;
    };
    let Some(title) = infer_title_from_text(&markdown) else {
        return;
    };
    if let Value::Object(map) = metadata {
        map.insert("canonical_title".to_string(), json!(title));
        let _ = write_pretty_json(metadata_path, metadata);
    }
}

fn collect_search_materials_from_workflow_state(
    runtime: &RuntimeTaskManager,
    task_id: &str,
) -> Vec<Value> {
    let Ok(summary) = load_workflow_summary(runtime, task_id) else {
        return Vec::new();
    };
    let mut searches = Vec::new();
    for output in summary.outputs.values() {
        collect_search_materials_from_value(output, &mut searches);
    }
    searches
}

fn collect_search_materials_from_value(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_search_materials_from_value(item, out);
            }
        }
        Value::Object(map) => {
            let tool_id = value
                .pointer("/metadata/_pwcli_call/tool_id")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .pointer("/_pwcli_call/tool_id")
                        .and_then(Value::as_str)
                });
            if matches!(tool_id, Some("builtin.anysearch" | "builtin.web_fetch")) {
                out.push(json!({
                    "tool_id": tool_id,
                    "query": value.pointer("/metadata/_pwcli_call/arguments/query")
                        .or_else(|| value.pointer("/metadata/_pwcli_call/arguments/q"))
                        .or_else(|| value.pointer("/metadata/_pwcli_call/arguments/url"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    "result_count": value.pointer("/metadata/result_count").or_else(|| value.pointer("/metadata/count")).cloned().unwrap_or(Value::Null),
                    "preview": value.get("content").and_then(Value::as_str).map(|text| preview_text(text, 500)).unwrap_or_default(),
                    "metadata": value.get("metadata").cloned().unwrap_or(Value::Null),
                }));
            }
            for child in map.values() {
                collect_search_materials_from_value(child, out);
            }
        }
        _ => {}
    }
}

fn list_dir_files(dir: &Path) -> PwResult<Vec<Value>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            files.push(json!({
                "name": path.file_name().and_then(|value| value.to_str()).unwrap_or_default(),
                "path": path_string(&path),
                "bytes": entry.metadata().map(|metadata| metadata.len()).unwrap_or_default(),
            }));
        }
    }
    Ok(files)
}

fn read_material_artifact_by_id(
    runtime: &RuntimeTaskManager,
    artifact_id: &str,
) -> PwResult<Value> {
    let tasks_dir = runtime.task_dir("__scan__");
    let tasks_dir = tasks_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| runtime.task_dir("__scan__"));
    for task_entry in fs::read_dir(tasks_dir)? {
        let task_entry = task_entry?;
        if !task_entry.path().is_dir() {
            continue;
        }
        let mineru_dir = task_entry.path().join("materials/mineru");
        if !mineru_dir.is_dir() {
            continue;
        }
        for material_entry in fs::read_dir(mineru_dir)? {
            let material_entry = material_entry?;
            let metadata_path = material_entry.path().join("metadata.json");
            if !metadata_path.is_file() {
                continue;
            }
            let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path)?)?;
            if metadata.get("artifact_id").and_then(Value::as_str) != Some(artifact_id) {
                continue;
            }
            let document_path = metadata
                .pointer("/artifact_paths/document_md")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content = if document_path.is_empty() {
                String::new()
            } else {
                fs::read_to_string(document_path).unwrap_or_default()
            };
            return Ok(json!({
                "artifact_id": artifact_id,
                "metadata": metadata,
                "content": content,
                "media_type": "text/markdown",
                "path": document_path,
            }));
        }
    }
    Err(PwError::Message(format!(
        "unknown material artifact '{artifact_id}'"
    )))
}

fn read_memory_extraction_status(runtime: &RuntimeTaskManager, task_id: &str) -> PwResult<Value> {
    let path = runtime
        .task_dir(task_id)
        .join("materials/memory_extractions.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_memory_extraction_status(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    status: &Value,
) -> PwResult<()> {
    let path = runtime
        .task_dir(task_id)
        .join("materials/memory_extractions.json");
    write_pretty_json(path, status)
}

fn workflow_is_research(summary: &WorkflowRunSummary) -> bool {
    summary
        .workflow_name
        .to_ascii_lowercase()
        .contains("research")
        || summary
            .outputs
            .values()
            .any(|value| value.get("materials").is_some())
}

fn persist_workflow_report(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    task_id: &str,
    summary: &WorkflowRunSummary,
) -> PwResult<Option<Value>> {
    let Some(content) =
        workflow_report_content(summary).filter(|content| !content.trim().is_empty())
    else {
        return Ok(None);
    };
    let now = Local::now();
    let date_dir = settings
        .pwcli_home
        .join("reports")
        .join(now.format("%Y-%m-%d").to_string());
    fs::create_dir_all(&date_dir)?;
    let title = workflow_report_title(summary, &content);
    let file_name = format!(
        "{}--{}--{}.md",
        now.format("%H%M%S"),
        safe_path_segment(task_id),
        safe_path_segment(&title)
    );
    let path = date_dir.join(file_name);
    fs::write(&path, normalize_report_markdown(&content, &title))?;
    let report = json!({
        "task_id": task_id,
        "title": title,
        "path": path_string(&path),
        "date": now.format("%Y-%m-%d").to_string(),
        "created_at": now.to_rfc3339(),
        "chars": content.chars().count(),
        "workflow_name": summary.workflow_name.clone(),
    });
    let metadata_path = path.with_extension("json");
    write_pretty_json(&metadata_path, &report)?;
    runtime.merge_task_metadata(
        task_id,
        json!({
            "report": {
                "title": report["title"].clone(),
                "path": report["path"].clone(),
                "metadata_path": path_string(&metadata_path),
                "date": report["date"].clone(),
                "created_at": report["created_at"].clone(),
                "chars": report["chars"].clone(),
            }
        }),
    )?;
    Ok(Some(report))
}

fn workflow_report_content(summary: &WorkflowRunSummary) -> Option<String> {
    let outputs = &summary.outputs;
    for key in ["final_report", "adaptive_research", "synthesize", "report"] {
        if let Some(content) = outputs.get(key).and_then(text_output_from_value) {
            return Some(content);
        }
    }
    let synthesize = outputs.get("synthesize").and_then(text_output_from_value);
    let verification = outputs
        .get("verify_sources")
        .and_then(text_output_from_value);
    if let (Some(synthesize), Some(verification)) = (synthesize, verification) {
        return Some(format!(
            "## Research synthesis\n\n{synthesize}\n\n## Verification notes\n\n{verification}"
        ));
    }
    for node_id in summary.visited.iter().rev() {
        if node_id == "end" {
            continue;
        }
        if let Some(content) = outputs.get(node_id).and_then(text_output_from_value) {
            return Some(content);
        }
    }
    None
}

fn text_output_from_value(value: &Value) -> Option<String> {
    value
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(str::to_string)
}

fn workflow_report_title(summary: &WorkflowRunSummary, content: &str) -> String {
    markdown_title_from_content(content).unwrap_or_else(|| {
        let name = summary.workflow_name.replace('_', " ");
        if name.trim().is_empty() {
            "pwcli report".to_string()
        } else {
            name
        }
    })
}

fn markdown_title_from_content(content: &str) -> Option<String> {
    for line in content.lines().take(24) {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.chars().take(96).collect());
            }
        }
    }
    None
}

fn normalize_report_markdown(content: &str, title: &str) -> String {
    let trimmed = content.trim();
    if trimmed.starts_with("# ") {
        format!("{trimmed}\n")
    } else {
        format!("# {title}\n\n{trimmed}\n")
    }
}

fn collect_task_reports(runtime: &RuntimeTaskManager, task_id: &str) -> Vec<Value> {
    let task = match runtime.get(task_id) {
        Ok(task) => task,
        Err(_) => return Vec::new(),
    };
    let Some(report) = task.metadata.get("report") else {
        return Vec::new();
    };
    vec![report.clone()]
}

fn ensure_task_report_archive(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    task_id: &str,
) -> PwResult<()> {
    let task = runtime.get(task_id)?;
    if task
        .metadata
        .pointer("/report/path")
        .and_then(Value::as_str)
        .is_some()
    {
        return Ok(());
    }
    let summary = match load_workflow_summary(runtime, task_id) {
        Ok(summary) => summary,
        Err(_) => return Ok(()),
    };
    if summary.status == WorkflowStatus::Completed {
        let _ = persist_workflow_report(settings, runtime, task_id, &summary)?;
    }
    Ok(())
}

fn extract_paper_memory_with_model(
    settings: &Settings,
    task_id: &str,
    artifact_id: &str,
    title: &str,
    markdown: &str,
) -> PwResult<Option<SemanticMemoryExtraction>> {
    let mut model_settings = match settings.resolved_model_settings() {
        Ok(settings) => settings,
        Err(_) => return Ok(None),
    };
    if model_settings.is_image_generation {
        return Ok(None);
    }
    model_settings.request_timeout_seconds = model_settings.request_timeout_seconds.min(90);
    let client = AnyModelClient::from_settings(&model_settings)?;
    let extraction_settings = &settings.memory.semantic_extraction;
    let input = trim_chars(
        markdown,
        extraction_settings.max_input_chars.max(16000).min(48000),
    );
    let prompt = format!(
        "Analyze this research paper markdown and extract durable memory for pwcli.\n\
         Paper title: {title}\n\
         Source task id: {task_id}\n\
         Source artifact id: {artifact_id}\n\n\
         Return JSON only with shape:\n\
         {{\"facts\":[{{\"ref_id\":\"f1\",\"statement\":\"...\",\"source_note\":\"source_doc_id={artifact_id}; section/page_hint=...; evidence_quote=...\"}}],\
         \"logic_chains\":[{{\"ref_id\":\"l1\",\"premises\":[\"f1\"],\"explanation\":\"...\"}}],\
         \"inferences\":[{{\"statement\":\"...\",\"logic_chain\":\"l1\"}}],\
         \"hypotheses\":[{{\"statement\":\"...\",\"supporting_facts\":[\"f1\"],\"confidence\":0.6}}],\"reason\":\"...\"}}.\n\
         Rules:\n\
         - Facts must be directly supported by the markdown and include source_doc_id, section/page_hint, and a short evidence_quote in source_note.\n\
         - Inferences must cite logic_chains whose premises are fact ref_ids.\n\
         - Hypotheses must be useful but uncertain, with confidence and supporting_facts.\n\
         - Do not invent paper titles, claims, numbers, or methods.\n\
         - Limits: facts <= {}, logic_chains <= {}, inferences <= {}, hypotheses <= {}.\n\n\
         Markdown:\n{input}",
        extraction_settings.max_facts.max(3),
        extraction_settings.max_logic_chains,
        extraction_settings.max_inferences,
        extraction_settings.max_hypotheses,
    );
    let response = client.complete(&ModelRequest {
        model: model_settings.model.clone(),
        messages: vec![ModelMessage {
            role: ModelRole::User,
            content: prompt,
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
        }],
        system: Some(
            "You extract source-grounded long-term memory from research papers. Return valid JSON only."
                .to_string(),
        ),
        thinking: ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        max_tokens: Some(model_settings.max_output_tokens.min(4096)),
        stream: false,
        tools: Vec::new(),
    })?;
    let mut extraction = parse_semantic_memory_json(&response.content)?;
    extraction
        .facts
        .truncate(extraction_settings.max_facts.max(3));
    extraction
        .logic_chains
        .truncate(extraction_settings.max_logic_chains);
    extraction
        .inferences
        .truncate(extraction_settings.max_inferences);
    extraction
        .hypotheses
        .truncate(extraction_settings.max_hypotheses);
    extraction.facts = extraction
        .facts
        .into_iter()
        .filter(|fact| {
            fact.source_note
                .as_deref()
                .is_some_and(|note| note.contains("evidence") || note.contains("source_doc_id"))
        })
        .collect();
    if extraction.facts.is_empty()
        && extraction.logic_chains.is_empty()
        && extraction.inferences.is_empty()
        && extraction.hypotheses.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(extraction))
}

fn parse_semantic_memory_json(raw: &str) -> PwResult<SemanticMemoryExtraction> {
    let json_text = extract_json_object(raw)
        .ok_or_else(|| PwError::Message("memory extraction returned no JSON object".to_string()))?;
    let value: Value = serde_json::from_str(json_text)
        .map_err(|err| PwError::Message(format!("memory extraction JSON parse failed: {err}")))?;
    let facts = value
        .get("facts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let statement = item.get("statement").and_then(Value::as_str)?.trim();
                    if statement.is_empty() {
                        return None;
                    }
                    Some(SemanticFactDraft {
                        ref_id: item
                            .get("ref_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        statement: statement.to_string(),
                        source_note: item
                            .get("source_note")
                            .or_else(|| item.get("source"))
                            .or_else(|| item.get("evidence_quote"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let logic_chains = value
        .get("logic_chains")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let explanation = item.get("explanation").and_then(Value::as_str)?.trim();
                    if explanation.is_empty() {
                        return None;
                    }
                    let premises = item
                        .get("premises")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    Some(SemanticLogicChainDraft {
                        ref_id: item
                            .get("ref_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        premises,
                        explanation: explanation.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let inferences = value
        .get("inferences")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let statement = item.get("statement").and_then(Value::as_str)?.trim();
                    let logic_chain = item.get("logic_chain").and_then(Value::as_str)?.trim();
                    if statement.is_empty() || logic_chain.is_empty() {
                        return None;
                    }
                    Some(SemanticInferenceDraft {
                        statement: statement.to_string(),
                        logic_chain: logic_chain.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let hypotheses = value
        .get("hypotheses")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let statement = item.get("statement").and_then(Value::as_str)?.trim();
                    if statement.is_empty() {
                        return None;
                    }
                    let supporting_facts = item
                        .get("supporting_facts")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    Some(SemanticHypothesisDraft {
                        statement: statement.to_string(),
                        supporting_facts,
                        confidence: item
                            .get("confidence")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.5) as f32,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(SemanticMemoryExtraction {
        facts,
        logic_chains,
        inferences,
        hypotheses,
        reason: value
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn read_user_preference_state(settings: &Settings) -> PwResult<Value> {
    let path = user_preference_state_path(settings);
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_user_preference_state(settings: &Settings, state: &Value) -> PwResult<()> {
    write_pretty_json(user_preference_state_path(settings), state)
}

fn user_preference_state_path(settings: &Settings) -> PathBuf {
    settings
        .pwcli_home
        .join("memory/hypotheses/user_preferences_state.json")
}

fn is_high_signal_preference_turn(user_input: &str) -> bool {
    let lower = user_input.to_ascii_lowercase();
    let zh = user_input;
    [
        "我喜欢",
        "我不喜欢",
        "不要",
        "应该",
        "需要",
        "太丑",
        "太大",
        "信息密度",
        "风格",
        "设置",
        "默认",
        "workflow",
        "agent",
        "tool",
        "memory",
        "下拉框",
        "白底",
        "极简",
        "linear",
        "arc",
    ]
    .iter()
    .any(|needle| zh.contains(needle))
        || [
            "prefer",
            "don't like",
            "do not",
            "should",
            "default",
            "style",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn infer_user_preference_hypotheses(user_input: &str) -> Vec<Value> {
    let mut hypotheses = Vec::new();
    let text = user_input.to_ascii_lowercase();
    let push = |items: &mut Vec<Value>, statement: &str, confidence: f32| {
        items.push(json!({
            "id": safe_path_segment(statement),
            "statement": statement,
            "confidence": confidence,
            "namespace": "user_preferences",
            "decay_policy": "decay unless reinforced or contradicted",
        }));
    };
    if user_input.contains("Linear") || user_input.contains("Arc") || text.contains("linear") {
        push(
            &mut hypotheses,
            "用户偏好 Linear/Arc 式极简、克制、低噪声的产品界面。",
            0.78,
        );
    }
    if user_input.contains("信息密度") || user_input.contains("右侧") {
        push(
            &mut hypotheses,
            "用户不喜欢高信息密度、占空间但低价值的右侧信息面板。",
            0.72,
        );
    }
    if user_input.contains("太丑") || user_input.contains("风格") {
        push(
            &mut hypotheses,
            "用户对 UI 审美非常敏感，偏好精致、留白充足、白底透明玻璃感控件。",
            0.68,
        );
    }
    if user_input.contains("code agent") || user_input.contains("代码") || text.contains("agent")
    {
        push(
            &mut hypotheses,
            "用户希望 code agent 只在代码理解、阅读、修改等场景作为工具被调用，而不是所有 workflow 默认调用。",
            0.76,
        );
    }
    if user_input.contains("memory") || user_input.contains("事实") || user_input.contains("推论")
    {
        push(
            &mut hypotheses,
            "用户希望 memory 明确区分事实层、推论层、猜想层，并保留可追溯来源链。",
            0.74,
        );
    }
    if user_input.contains("下拉框") || user_input.contains("白底") {
        push(
            &mut hypotheses,
            "用户偏好白底黑字、尺寸适中、对齐准确的下拉菜单。",
            0.71,
        );
    }
    if hypotheses.is_empty() {
        push(
            &mut hypotheses,
            "用户倾向于直接指出产品和 agent 链路中的具体问题，并期待本地代码同步修复。",
            0.6,
        );
    }
    hypotheses
}

fn upsert_user_preference_hypothesis(state: &mut Value, hypothesis: Value, run_id: &str) {
    let Some(id) = hypothesis.get("id").and_then(Value::as_str) else {
        return;
    };
    if !state.get("hypotheses").is_some_and(Value::is_array) {
        state["hypotheses"] = json!([]);
    }
    let now = Utc::now().to_rfc3339();
    if let Some(items) = state.get_mut("hypotheses").and_then(Value::as_array_mut) {
        if let Some(existing) = items
            .iter_mut()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        {
            let old_confidence = existing
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let new_confidence = hypothesis
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            existing["confidence"] = json!((old_confidence.max(new_confidence) + 0.03).min(0.95));
            existing["last_seen_at"] = json!(now.clone());
            push_json_array_limited(
                existing,
                "supporting_conversations",
                json!({ "run_id": run_id, "at": now }),
                20,
            );
            return;
        }
        let mut next = hypothesis;
        next["created_at"] = json!(now.clone());
        next["last_seen_at"] = json!(now.clone());
        next["supporting_conversations"] = json!([{ "run_id": run_id, "at": now }]);
        items.push(next);
    }
}

fn push_json_array_limited(value: &mut Value, key: &str, item: Value, limit: usize) {
    if !value.get(key).is_some_and(Value::is_array) {
        value[key] = json!([]);
    }
    if let Some(items) = value.get_mut(key).and_then(Value::as_array_mut) {
        items.push(item);
        if items.len() > limit {
            let remove_count = items.len() - limit;
            items.drain(0..remove_count);
        }
    }
}

fn persist_workflow_artifacts(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    workflow: &GraphWorkflow,
    summary: Option<&WorkflowRunSummary>,
) -> PwResult<()> {
    let dir = runtime.task_dir(task_id);
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("workflow.json"),
        serde_json::to_string_pretty(workflow)?,
    )?;
    fs::write(dir.join("workflow.mmd"), workflow_mermaid(workflow))?;
    if let Some(summary) = summary {
        fs::write(
            dir.join("workflow_state.json"),
            serde_json::to_string_pretty(summary)?,
        )?;
    }
    Ok(())
}

fn load_workflow_summary(
    runtime: &RuntimeTaskManager,
    task_id: &str,
) -> PwResult<WorkflowRunSummary> {
    let bytes = fs::read(runtime.task_dir(task_id).join("workflow_state.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn finalize_workflow_task(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    summary: &WorkflowRunSummary,
) -> PwResult<()> {
    let status = match summary.status {
        WorkflowStatus::Completed => RuntimeTaskStatus::Completed,
        WorkflowStatus::Failed | WorkflowStatus::MaxStepsReached => RuntimeTaskStatus::Failed,
        WorkflowStatus::Interrupted => RuntimeTaskStatus::Cancelled,
    };
    runtime.mark_task_status(
        task_id,
        status,
        json!({
            "workflow": {
                "name": summary.workflow_name.clone(),
                "status": format!("{:?}", summary.status),
                "visited": summary.visited.clone(),
                "interrupt": summary.interrupt.clone()
            },
            "review_recommendation": {
                "required": summary.status != WorkflowStatus::Completed,
                "reason": format!("workflow ended with {:?}", summary.status)
            }
        }),
    )
}

fn workflow_mermaid(workflow: &GraphWorkflow) -> String {
    let mut out = String::from("flowchart TD\n");
    for node in workflow.nodes.values() {
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            sanitize_mermaid_id(&node.id),
            node.label.replace('"', "'")
        ));
    }
    for edge in &workflow.edges {
        out.push_str(&format!(
            "  {} -->|{:?}| {}\n",
            sanitize_mermaid_id(&edge.from),
            edge.condition,
            sanitize_mermaid_id(&edge.to)
        ));
    }
    out
}

fn sanitize_mermaid_id(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn compact_workflow_for_ui(mut workflow: GraphWorkflow) -> GraphWorkflow {
    for node in workflow.nodes.values_mut() {
        match &mut node.kind {
            WorkflowNodeKind::AgentTask { prompt, .. }
            | WorkflowNodeKind::ModelTurn { prompt }
            | WorkflowNodeKind::AdaptiveLoop { prompt } => {
                if prompt.chars().count() > 240 {
                    *prompt = tail_chars(prompt, 240);
                }
            }
            WorkflowNodeKind::ToolCall { arguments, .. } => {
                if arguments.to_string().chars().count() > 240 {
                    *arguments =
                        serde_json::json!({ "preview": tail_chars(&arguments.to_string(), 240) });
                }
            }
            WorkflowNodeKind::Approval { prompt } => {
                if prompt.chars().count() > 240 {
                    *prompt = tail_chars(prompt, 240);
                }
            }
            WorkflowNodeKind::Join
            | WorkflowNodeKind::ResearchReadPapers { .. }
            | WorkflowNodeKind::SubWorkflow { .. }
            | WorkflowNodeKind::End => {}
        }
    }
    workflow
}

fn build_service_registry(settings: &Settings) -> PwResult<(ToolRegistrySnapshot, usize)> {
    let mut registry = ToolRegistry::new();
    let mut tools = Vec::new();
    tools.extend(BuiltinToolLoader.load()?);
    tools.extend(VerificationToolLoader.load()?);
    tools.extend(McpToolLoader::new(settings.mcp.clone(), settings.pwcli_home.clone()).load()?);
    tools.extend(SkillToolLoader::new(settings.skill_roots.clone()).load()?);
    let tools = apply_tool_settings(tools, &settings.tools);
    let loaded_skills = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.descriptor.source,
                crate::tools::ToolSource::Skill { .. }
            )
        })
        .count();
    registry.register_many(tools);
    Ok((registry.snapshot(), loaded_skills))
}

fn verification_record_from_tool_result(result: ToolResult, cwd: &Path) -> VerificationRecord {
    let metadata = result.metadata.clone();
    let report = verification_report_from_metadata(&metadata).unwrap_or_else(|| {
        legacy_verification_report(
            "verification.project_check",
            cwd.display().to_string(),
            !result.is_error
                && metadata
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            result.content.clone(),
            metadata.clone(),
        )
    });
    VerificationRecord {
        passed: report.passed(),
        content: result.content,
        metadata,
        report: Some(report),
    }
}

fn load_rule_texts(settings: &Settings) -> Vec<String> {
    let rules_dir = settings.pwcli_home.join("rules");
    let Ok(entries) = std::fs::read_dir(rules_dir) else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(path) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    rules.push(trimmed.to_string());
                }
            }
        }
    }
    rules
}

fn default_local_context_paths() -> Vec<PathBuf> {
    ["AGENTS.md", "README.md"]
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .collect()
}

fn new_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("{prefix}_{millis}_{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request},
    };
    use tower::ServiceExt;

    #[test]
    fn event_bus_replays_events_after_cursor() {
        let bus = EventBus::new();
        bus.emit("one", None, None, json!({}));
        let first = bus.emit("two", None, None, json!({}));
        let replay = bus.since(first.seq - 1);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].kind, "two");
    }

    #[test]
    fn research_candidates_convert_arxiv_ids_and_replace_numeric_titles() {
        let content = "\
### 1. 2605.19457v1
- **URL**: https://arxiv.org/html/2605.19457v1
";
        let candidates = extract_research_paper_candidates(content, 3);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].pdf_url.as_deref(),
            Some("https://arxiv.org/pdf/2605.19457v1.pdf")
        );
        assert!(title_needs_replacement(&candidates[0].title));
        assert_eq!(
            infer_title_from_text(
                "# Generative Auto-Bidding with Unified Modeling and Exploration"
            )
            .as_deref(),
            Some("Generative Auto-Bidding with Unified Modeling and Exploration")
        );
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, true);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("\"ok\":true"));
    }

    #[tokio::test]
    async fn status_route_exposes_registry() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["tool_count"].as_u64().unwrap() > 0);
        assert!(json["registry_version"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn tools_health_route_returns_checks() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tools/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["checks"].as_array().unwrap().len() > 1);
    }

    #[tokio::test]
    async fn memory_layers_route_returns_all_layers() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());
        settings.memory.embedding.enabled = false;
        let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
        let fact = store
            .add_fact("service memory fact", "service test")
            .unwrap();
        let logic = store
            .add_logic_chain(vec![fact.id.clone()], "service inference chain")
            .unwrap();
        store
            .add_inference("service memory inference", logic.id)
            .unwrap();
        store
            .add_hypothesis("service memory hypothesis", vec![fact.id], 0.64)
            .unwrap();

        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/memory/layers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["facts"].as_array().unwrap().len(), 1);
        assert_eq!(json["inferences"].as_array().unwrap().len(), 1);
        assert_eq!(json["hypotheses"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn task_routes_create_and_compact_task() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "title": "service test task",
                            "cwd": temp.path(),
                            "active": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let task_id = json["task"]["task_id"].as_str().unwrap();

        let compact_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/tasks/{task_id}/compact"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "scope": "both" })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(compact_response.status(), StatusCode::OK);
        let body = to_bytes(compact_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["summary"]["summary_path"].as_str().is_some());
    }

    #[tokio::test]
    async fn task_routes_create_and_decompose() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "title": "Build a tiny task planner",
                            "active": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::OK);
        let create_body = to_bytes(create_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let create_json: Value = serde_json::from_slice(&create_body).unwrap();
        let task_id = create_json["task"]["task_id"].as_str().unwrap();

        let decompose_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/tasks/{task_id}/decompose"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "goal": "Build a CLI helper to split tasks",
                            "kind": "auto",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(decompose_response.status(), StatusCode::OK);
        let decompose_body = to_bytes(decompose_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let decompose_json: Value = serde_json::from_slice(&decompose_body).unwrap();
        let steps_len = decompose_json
            .pointer("/decomposition/steps")
            .and_then(Value::as_array)
            .map(|steps| steps.len())
            .unwrap_or(0);
        assert!(steps_len >= 3);
    }

    #[tokio::test]
    async fn task_routes_delete_task() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({"title": "to delete", "active": true})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::OK);
        let body = to_bytes(create_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let create_json: Value = serde_json::from_slice(&body).unwrap();
        let task_id = create_json["task"]["task_id"].as_str().unwrap();

        let delete_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/tasks/{task_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::OK);
        let body = to_bytes(delete_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let delete_json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(delete_json["ok"], true);

        let list_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_json: Value = serde_json::from_slice(&body).unwrap();
        assert!(list_json["tasks"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rule_routes_write_and_read_rule_file() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::from_home(temp.path());
        let runtime = ServiceRuntime::new(settings).unwrap();
        let app = router(runtime, false);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rules/service-test.md")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "text": "Never run destructive commands without approval."
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/rules/service-test.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["text"].as_str().unwrap(),
            "Never run destructive commands without approval."
        );
    }
}
