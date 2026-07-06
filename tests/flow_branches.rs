use pwcli::{
    audit::InMemoryAuditRecorder,
    context::ContextBuilder,
    graph::{
        GraphExecutor, GraphInterrupt, GraphInterruptKind, GraphMessage, GraphRunRequest,
        GraphRunServices, GraphStatus, GraphStep, NoopGraphEventSink, PlannedToolCallPlanner,
        StreamingModelPlanner,
    },
    policy::{PolicyDecision, PolicyGuard, UserApproval},
    tools::{
        model::{
            ModelClient, ModelEvent, ModelRequest, ModelResponse, ModelToolCall, ThinkingConfig,
        },
        InvocationMode, LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolExecutor,
        ToolRegistry, ToolResult, ToolSource,
    },
};
use serde_json::json;
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct StaticExecutor(&'static str);

impl ToolExecutor for StaticExecutor {
    fn execute(&self, _call: &ToolCall) -> pwcli::Result<ToolResult> {
        Ok(ToolResult::ok(self.0))
    }
}

struct AskPolicy;

impl PolicyGuard for AskPolicy {
    fn check(&self, descriptor: &ToolDescriptor, _call: &ToolCall) -> PolicyDecision {
        if descriptor.risk_level >= RiskLevel::Medium {
            PolicyDecision::AskUser {
                prompt: "approve?".to_string(),
            }
        } else {
            PolicyDecision::Allow
        }
    }
}

struct Approval(bool);

impl UserApproval for Approval {
    fn ask_user(&self, _prompt: &str, _call: &ToolCall) -> bool {
        self.0
    }
}

struct InterruptPlanner;

impl pwcli::graph::GraphPlanner for InterruptPlanner {
    fn next_step(
        &mut self,
        _state: &pwcli::graph::GraphState,
        _request: &GraphRunRequest,
        _snapshot: &pwcli::tools::ToolRegistrySnapshot,
    ) -> pwcli::Result<GraphStep> {
        Ok(GraphStep::Interrupt(GraphInterrupt {
            id: "clarify-1".to_string(),
            kind: GraphInterruptKind::Clarification,
            prompt: "Which target should I use?".to_string(),
            reason: "missing target".to_string(),
            tool_call: None,
        }))
    }
}

struct SequencedModel {
    responses: Mutex<Vec<ModelResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl SequencedModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().rev().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ModelClient for SequencedModel {
    fn stream(
        &self,
        request: &ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> pwcli::Result<ModelResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let response = self.responses.lock().unwrap().pop().unwrap();
        if !response.content.is_empty() {
            on_event(ModelEvent::TextDelta(response.content.clone()));
        }
        for call in &response.tool_calls {
            on_event(ModelEvent::ToolCall(call.clone()));
        }
        on_event(ModelEvent::Done);
        Ok(response)
    }
}

fn tool(id: &str, source: ToolSource, risk_level: RiskLevel, output: &'static str) -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: id.to_string(),
            name: id.replace('.', "-"),
            description: format!("test tool {id}"),
            input_schema: json!({ "type": "object" }),
            source,
            risk_level,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![id.to_string()],
            metadata: json!({}),
            enabled: true,
        },
        executor: Some(Arc::new(StaticExecutor(output))),
    }
}

#[test]
fn context_reads_memory_rules_and_local_files() {
    let temp = tempfile::tempdir().unwrap();
    let pwcli_home = temp.path().join(".pwcli");
    fs::create_dir_all(pwcli_home.join("memory")).unwrap();
    fs::create_dir_all(pwcli_home.join("rules")).unwrap();
    fs::write(
        pwcli_home.join("memory/profile.md"),
        "memory: prefer concise output",
    )
    .unwrap();
    fs::write(
        pwcli_home.join("rules/safety.md"),
        "rule: ask before risky tools",
    )
    .unwrap();
    let local = temp.path().join("README.md");
    fs::write(&local, "local project context").unwrap();

    let registry = ToolRegistry::new();
    let pack = ContextBuilder::new().build_with_sources(
        "hello",
        &registry.snapshot(),
        Some(pwcli_home),
        vec![local],
    );

    assert!(pack
        .memory_items
        .iter()
        .any(|item| item.contains("prefer concise")));
    assert!(pack
        .rule_items
        .iter()
        .any(|item| item.contains("risky tools")));
    assert!(pack
        .local_items
        .iter()
        .any(|item| item.contains("local project context")));
}

#[test]
fn context_truncates_large_local_files_before_packing() {
    let temp = tempfile::tempdir().unwrap();
    let pwcli_home = temp.path().join(".pwcli");
    fs::create_dir_all(&pwcli_home).unwrap();
    let local = temp.path().join("large.txt");
    fs::write(&local, format!("{}TAIL_MARKER", "a".repeat(70 * 1024))).unwrap();

    let registry = ToolRegistry::new();
    let pack = ContextBuilder::new().build_with_sources(
        "hello",
        &registry.snapshot(),
        Some(pwcli_home),
        vec![local],
    );

    assert_eq!(pack.local_items.len(), 1);
    assert!(!pack.local_items[0].contains("TAIL_MARKER"));
    assert!(pack
        .warnings
        .iter()
        .any(|warning| warning.contains("was truncated")));
}

#[test]
fn graph_executes_builtin_skill_mcp_and_verification_tool_sources() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.echo",
        ToolSource::Builtin,
        RiskLevel::ReadOnly,
        "builtin-ok",
    ));
    registry.register(tool(
        "skill.mock",
        ToolSource::Skill {
            path: PathBuf::from("/tmp/mock-skill"),
        },
        RiskLevel::ReadOnly,
        "skill-ok",
    ));
    registry.register(tool(
        "mcp.mock",
        ToolSource::Mcp {
            server: "mock-server".to_string(),
        },
        RiskLevel::ReadOnly,
        "mcp-ok",
    ));
    registry.register(tool(
        "verification.mock",
        ToolSource::Verification,
        RiskLevel::ReadOnly,
        "verification-ok",
    ));

    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run all tools", &snapshot);
    let mut planner = PlannedToolCallPlanner::new(vec![
        call("1", "builtin.echo"),
        call("2", "skill.mock"),
        call("3", "mcp.mock"),
        call("4", "verification.mock"),
    ]);
    let audit = InMemoryAuditRecorder::default();
    let graph = GraphExecutor::builder().build();
    let summary = graph
        .run_with_planner(
            GraphRunRequest {
                user_input: "run all tools".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &audit,
            None,
        )
        .unwrap();

    let outputs = summary
        .state
        .tool_results
        .iter()
        .map(|result| result.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        outputs,
        vec!["builtin-ok", "skill-ok", "mcp-ok", "verification-ok"]
    );
}

#[test]
fn policy_ask_user_branch_allows_or_rejects_tool_execution() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.risky",
        ToolSource::Builtin,
        RiskLevel::Medium,
        "allowed",
    ));
    let snapshot = registry.snapshot();

    let approved = run_one_risky_tool(&snapshot, Approval(true));
    assert_eq!(approved.state.tool_results[0].content, "allowed");
    assert!(!approved.state.tool_results[0].is_error);

    let rejected = run_one_risky_tool(&snapshot, Approval(false));
    assert_eq!(
        rejected.state.tool_results[0].content,
        "user rejected tool call"
    );
    assert!(rejected.state.tool_results[0].is_error);
}

#[test]
fn graph_can_interrupt_for_clarification_without_cancelling() {
    let registry = ToolRegistry::new();
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("ambiguous task", &snapshot);
    let mut planner = InterruptPlanner;

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "ambiguous task".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert_eq!(summary.state.status, GraphStatus::Interrupted);
    let interrupt = summary.state.interrupt.unwrap();
    assert_eq!(interrupt.kind, GraphInterruptKind::Clarification);
    assert_eq!(interrupt.prompt, "Which target should I use?");
}

#[test]
fn policy_ask_user_without_handler_interrupts_graph() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.risky",
        ToolSource::Builtin,
        RiskLevel::Medium,
        "allowed",
    ));
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run risky", &snapshot);
    let mut planner = PlannedToolCallPlanner::new(vec![call("1", "builtin.risky")]);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run risky".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert_eq!(summary.state.status, GraphStatus::Interrupted);
    assert!(summary.state.tool_results.is_empty());
    let interrupt = summary.state.interrupt.unwrap();
    assert_eq!(interrupt.kind, GraphInterruptKind::ToolApproval);
    assert_eq!(interrupt.tool_call.unwrap().id, "1");
}

#[test]
fn streaming_model_planner_runs_native_tool_calls_until_final_answer() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.echo",
        ToolSource::Builtin,
        RiskLevel::ReadOnly,
        "echo-result",
    ));
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run echo", &snapshot);
    let model = SequencedModel::new(vec![
        ModelResponse {
            tool_calls: vec![ModelToolCall {
                id: "call-1".to_string(),
                name: "builtin_echo".to_string(),
                arguments: json!({}),
            }],
            ..Default::default()
        },
        ModelResponse {
            content: "final answer after tool".to_string(),
            ..Default::default()
        },
    ]);
    let deltas = RefCell::new(String::new());
    let mut planner = StreamingModelPlanner::new(
        &model,
        "test-model",
        ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        |event| {
            if let ModelEvent::TextDelta(delta) = event {
                deltas.borrow_mut().push_str(&delta);
            }
        },
    )
    .stream(false);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run echo".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert_eq!(summary.state.tool_results[0].content, "echo-result");
    assert_eq!(summary.state.last_content, "final answer after tool");
    assert_eq!(model.requests().len(), 2);
    assert_eq!(model.requests()[0].tools[0].name, "builtin_echo");
    assert!(model.requests()[1]
        .messages
        .iter()
        .any(|message| message.content.contains("echo-result")));
    assert_eq!(&*deltas.borrow(), "final answer after tool");
}

#[test]
fn streaming_model_planner_preserves_seeded_session_user_messages() {
    let registry = ToolRegistry::new();
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("current question", &snapshot);
    let model = SequencedModel::new(vec![ModelResponse {
        content: "current answer".to_string(),
        ..Default::default()
    }]);
    let mut planner = StreamingModelPlanner::new(
        &model,
        "test-model",
        ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        |_| {},
    )
    .stream(false);
    let audit = InMemoryAuditRecorder::default();
    let mut events = NoopGraphEventSink;
    let mut services = GraphRunServices::new(&AskPolicy, &audit, None, &mut events);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_seed_messages_and_events(
            GraphRunRequest {
                user_input: "current question".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &mut services,
            vec![
                GraphMessage::User("previous question".to_string()),
                GraphMessage::Assistant("previous answer".to_string()),
            ],
        )
        .unwrap();

    assert_eq!(summary.state.last_content, "current answer");
    let request = model.requests().remove(0);
    assert!(request
        .messages
        .iter()
        .any(|message| message.content == "previous question"));
    assert!(request
        .messages
        .iter()
        .any(|message| message.content == "previous answer"));
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|message| message.content.contains("current question"))
            .count(),
        1
    );
}

#[test]
fn streaming_model_planner_supports_xml_tool_call_fallback() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.echo",
        ToolSource::Builtin,
        RiskLevel::ReadOnly,
        "xml-tool-result",
    ));
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run echo", &snapshot);
    let model = SequencedModel::new(vec![
        ModelResponse {
            content: r#"<tool_call>{"name":"builtin_echo","arguments":{}}</tool_call>"#.to_string(),
            ..Default::default()
        },
        ModelResponse {
            content: "done".to_string(),
            ..Default::default()
        },
    ]);
    let deltas = RefCell::new(String::new());
    let mut planner = StreamingModelPlanner::new(
        &model,
        "test-model",
        ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        |event| {
            if let ModelEvent::TextDelta(delta) = event {
                deltas.borrow_mut().push_str(&delta);
            }
        },
    )
    .stream(false);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run echo".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert_eq!(summary.state.tool_results[0].content, "xml-tool-result");
    assert_eq!(summary.state.last_content, "done");
    assert_eq!(model.requests().len(), 2);
    assert_eq!(&*deltas.borrow(), "done");
}

#[test]
fn streaming_model_planner_returns_unknown_tool_as_recoverable_tool_error() {
    let registry = ToolRegistry::new();
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run missing tool", &snapshot);
    let model = SequencedModel::new(vec![
        ModelResponse {
            tool_calls: vec![ModelToolCall {
                id: "call-unknown".to_string(),
                name: "missing_tool".to_string(),
                arguments: json!({}),
            }],
            ..Default::default()
        },
        ModelResponse {
            content: "missing tool handled".to_string(),
            ..Default::default()
        },
    ]);
    let mut planner = StreamingModelPlanner::new(
        &model,
        "test-model",
        ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        |_| {},
    )
    .stream(false);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run missing tool".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert!(summary.state.tool_results[0].is_error);
    assert!(summary.state.tool_results[0]
        .content
        .contains("__unavailable_model_tool__missing_tool"));
    assert_eq!(summary.state.last_content, "missing tool handled");
    assert_eq!(model.requests().len(), 2);
}

#[test]
fn streaming_model_planner_cannot_execute_unselected_snapshot_tool() {
    let mut registry = ToolRegistry::new();
    registry.register(tool(
        "builtin.echo",
        ToolSource::Builtin,
        RiskLevel::ReadOnly,
        "echo-result",
    ));
    registry.register(tool(
        "builtin.hidden",
        ToolSource::Builtin,
        RiskLevel::ReadOnly,
        "hidden-result",
    ));
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run echo", &snapshot);
    assert!(context_pack
        .selected_tool_ids
        .contains(&"builtin.echo".to_string()));
    assert!(!context_pack
        .selected_tool_ids
        .contains(&"builtin.hidden".to_string()));

    let model = SequencedModel::new(vec![
        ModelResponse {
            tool_calls: vec![ModelToolCall {
                id: "call-hidden".to_string(),
                name: "builtin.hidden".to_string(),
                arguments: json!({}),
            }],
            ..Default::default()
        },
        ModelResponse {
            content: "unselected tool handled".to_string(),
            ..Default::default()
        },
    ]);
    let mut planner = StreamingModelPlanner::new(
        &model,
        "test-model",
        ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        |_| {},
    )
    .stream(false);

    let summary = GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run echo".to_string(),
                context_pack,
            },
            &snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            None,
        )
        .unwrap();

    assert!(summary.state.tool_results[0].is_error);
    assert!(summary.state.tool_results[0]
        .content
        .contains("__unavailable_model_tool__builtin_hidden"));
    assert_ne!(summary.state.tool_results[0].content, "hidden-result");
    assert_eq!(summary.state.last_content, "unselected tool handled");
}

fn run_one_risky_tool(
    snapshot: &pwcli::tools::ToolRegistrySnapshot,
    approval: impl UserApproval,
) -> pwcli::graph::GraphRunSummary {
    let context_pack = ContextBuilder::new().build("run risky", snapshot);
    let mut planner = PlannedToolCallPlanner::new(vec![call("1", "builtin.risky")]);
    GraphExecutor::builder()
        .build()
        .run_with_planner(
            GraphRunRequest {
                user_input: "run risky".to_string(),
                context_pack,
            },
            snapshot,
            &mut planner,
            &AskPolicy,
            &InMemoryAuditRecorder::default(),
            Some(&approval),
        )
        .unwrap()
}

fn call(id: &str, tool_id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        tool_id: tool_id.to_string(),
        name: tool_id.to_string(),
        arguments: json!({}),
    }
}
