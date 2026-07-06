use crate::{
    audit::{token::TokenUsage, AuditEvent, AuditRecorder},
    context::ContextPack,
    policy::{PolicyDecision, PolicyGuard, UserApproval},
    tools::{
        model::{
            ModelClient, ModelEvent, ModelMessage, ModelRequest, ModelRole, ModelToolCall,
            ModelToolSpec, ThinkingConfig,
        },
        ToolCall, ToolDescriptor, ToolExecutionContext, ToolExecutionRuntime, ToolRegistrySnapshot,
        ToolResult, ToolRuntimeEvent,
    },
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use super::{
    hooks::GraphHook,
    state::{GraphInterrupt, GraphInterruptKind, GraphMessage, GraphState, GraphStatus},
};

pub struct GraphExecutor {
    pub(crate) max_rounds: u32,
    pub(crate) hooks: Vec<Arc<dyn GraphHook>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphEvent {
    GraphStarted,
    ContextBuilt {
        context_id: String,
    },
    ToolSelectionStarted,
    ToolSelected {
        tool_id: String,
    },
    ModelStarted,
    ModelCompleted {
        output_chars: usize,
    },
    ToolCallStarted {
        call_id: String,
        tool_id: String,
        name: String,
    },
    ToolPolicyDecision {
        call_id: String,
        tool_id: String,
        name: String,
        decision: PolicyDecision,
    },
    ToolCompleted {
        call_id: String,
        tool_id: String,
        name: String,
        is_error: bool,
        content_preview: String,
        metadata: serde_json::Value,
    },
    ToolRuntimeEvent {
        call_id: String,
        event: ToolRuntimeEvent,
    },
    UserApprovalRequested {
        prompt: String,
    },
    GraphInterrupted {
        interrupt: GraphInterrupt,
    },
    GraphCompleted,
}

pub trait GraphEventSink {
    fn emit(&mut self, event: GraphEvent);
}

pub struct NoopGraphEventSink;

impl GraphEventSink for NoopGraphEventSink {
    fn emit(&mut self, _event: GraphEvent) {}
}

pub struct GraphRunServices<'a> {
    pub policy: &'a dyn PolicyGuard,
    pub audit: &'a dyn AuditRecorder,
    pub approval: Option<&'a dyn UserApproval>,
    pub events: &'a mut dyn GraphEventSink,
    pub tool_context: ToolExecutionContext,
}

impl<'a> GraphRunServices<'a> {
    pub fn new(
        policy: &'a dyn PolicyGuard,
        audit: &'a dyn AuditRecorder,
        approval: Option<&'a dyn UserApproval>,
        events: &'a mut dyn GraphEventSink,
    ) -> Self {
        Self {
            policy,
            audit,
            approval,
            events,
            tool_context: ToolExecutionContext::default(),
        }
    }

    pub fn with_tool_context(mut self, tool_context: ToolExecutionContext) -> Self {
        self.tool_context = tool_context;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRunRequest {
    pub user_input: String,
    pub context_pack: ContextPack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRunSummary {
    pub registry_version: u64,
    pub state: GraphState,
}

#[derive(Debug, Clone)]
pub enum GraphStep {
    Respond(String),
    RespondWithUsage {
        content: String,
        token_usage: TokenUsage,
    },
    CallTools(Vec<ToolCall>),
    AskUser {
        prompt: String,
    },
    Interrupt(GraphInterrupt),
    Stop,
}

enum ToolExecutionOutcome {
    Completed(ToolResult),
    Interrupted(GraphInterrupt),
}

pub trait GraphPlanner {
    fn next_step(
        &mut self,
        state: &GraphState,
        request: &GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
    ) -> Result<GraphStep>;
}

#[derive(Debug, Clone)]
pub struct PlannedToolCallPlanner {
    calls: Vec<ToolCall>,
    emitted: bool,
}

impl PlannedToolCallPlanner {
    pub fn new(calls: Vec<ToolCall>) -> Self {
        Self {
            calls,
            emitted: false,
        }
    }
}

impl GraphPlanner for PlannedToolCallPlanner {
    fn next_step(
        &mut self,
        state: &GraphState,
        _request: &GraphRunRequest,
        _snapshot: &ToolRegistrySnapshot,
    ) -> Result<GraphStep> {
        if !self.emitted {
            self.emitted = true;
            return Ok(GraphStep::CallTools(self.calls.clone()));
        }

        if let Some(last) = state.tool_results.last() {
            return Ok(GraphStep::Respond(last.content.clone()));
        }

        Ok(GraphStep::Respond(state.last_content.clone()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct NoopPlanner;

impl GraphPlanner for NoopPlanner {
    fn next_step(
        &mut self,
        _state: &GraphState,
        request: &GraphRunRequest,
        _snapshot: &ToolRegistrySnapshot,
    ) -> Result<GraphStep> {
        Ok(GraphStep::Respond(format!(
            "No model/tool planner is configured. Context: {}",
            request.context_pack.summary
        )))
    }
}

pub struct StreamingModelPlanner<'a> {
    pub client: &'a dyn ModelClient,
    pub model: String,
    pub system: Option<String>,
    pub thinking: ThinkingConfig,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub on_event: Box<dyn FnMut(ModelEvent) + 'a>,
    terminal_response_emitted: bool,
}

impl<'a> StreamingModelPlanner<'a> {
    pub fn new(
        client: &'a dyn ModelClient,
        model: impl Into<String>,
        thinking: ThinkingConfig,
        on_event: impl FnMut(ModelEvent) + 'a,
    ) -> Self {
        Self {
            client,
            model: model.into(),
            system: None,
            thinking,
            max_tokens: Some(4096),
            stream: true,
            on_event: Box::new(on_event),
            terminal_response_emitted: false,
        }
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

impl GraphPlanner for StreamingModelPlanner<'_> {
    fn next_step(
        &mut self,
        state: &GraphState,
        request: &GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
    ) -> Result<GraphStep> {
        if self.terminal_response_emitted {
            return Ok(GraphStep::Stop);
        }

        let mut messages = Vec::new();
        messages.push(ModelMessage {
            role: ModelRole::User,
            content: format!(
                "{}\n\nContext:\n{}",
                request.user_input, request.context_pack.summary
            ),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
        });
        let current_user_index = state
            .messages
            .iter()
            .rposition(|message| matches!(message, GraphMessage::User(_)));
        for (idx, message) in state.messages.iter().enumerate() {
            match message {
                GraphMessage::User(content) if Some(idx) != current_user_index => {
                    messages.push(ModelMessage {
                        role: ModelRole::User,
                        content: content.clone(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_calls: Vec::new(),
                    })
                }
                GraphMessage::Assistant(content) => messages.push(ModelMessage {
                    role: ModelRole::Assistant,
                    content: content.clone(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: Vec::new(),
                }),
                GraphMessage::AssistantToolCalls { calls } => messages.push(ModelMessage {
                    role: ModelRole::Assistant,
                    content: String::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: calls
                        .iter()
                        .map(|call| ModelToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        })
                        .collect(),
                }),
                GraphMessage::Tool {
                    call_id,
                    name,
                    content,
                    ..
                } => messages.push(ModelMessage {
                    role: ModelRole::Tool,
                    content: content.clone(),
                    tool_call_id: Some(call_id.clone()),
                    tool_name: (!name.trim().is_empty()).then(|| name.clone()),
                    tool_calls: Vec::new(),
                }),
                GraphMessage::System(content) => messages.push(ModelMessage {
                    role: ModelRole::System,
                    content: content.clone(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: Vec::new(),
                }),
                GraphMessage::User(_) => {}
            }
        }

        let tool_specs =
            selected_model_tool_specs(&request.context_pack.selected_tool_ids, snapshot);
        let request_system = model_system_prompt(self.system.as_deref(), &tool_specs);
        let model_request = ModelRequest {
            model: self.model.clone(),
            messages,
            system: request_system,
            thinking: self.thinking.clone(),
            max_tokens: self.max_tokens,
            stream: self.stream,
            tools: tool_specs,
        };
        let mut output_gate = ModelOutputGate::default();
        let response = {
            let on_event = &mut self.on_event;
            self.client.stream(&model_request, &mut |event| {
                output_gate.handle(event, on_event);
            })?
        };

        let parsed_tool_calls = if response.tool_calls.is_empty() {
            parse_text_tool_calls(
                &response.content,
                &request.context_pack.selected_tool_ids,
                snapshot,
            )
        } else {
            response
                .tool_calls
                .iter()
                .map(|call| {
                    model_tool_call_to_graph_call(
                        call,
                        &request.context_pack.selected_tool_ids,
                        snapshot,
                    )
                })
                .collect::<Vec<_>>()
        };

        if !parsed_tool_calls.is_empty() {
            return Ok(GraphStep::CallTools(parsed_tool_calls));
        }

        output_gate.flush(&mut self.on_event);
        self.terminal_response_emitted = true;
        Ok(GraphStep::RespondWithUsage {
            content: response.content,
            token_usage: TokenUsage {
                input_tokens: response.usage.input_tokens.unwrap_or_default(),
                output_tokens: response.usage.output_tokens.unwrap_or_default(),
            },
        })
    }
}

#[derive(Default)]
struct ModelOutputGate {
    pending: Vec<ModelEvent>,
    prefix: String,
    decided: bool,
    stream_directly: bool,
}

impl ModelOutputGate {
    fn handle(&mut self, event: ModelEvent, on_event: &mut Box<dyn FnMut(ModelEvent) + '_>) {
        if self.stream_directly {
            on_event(event);
            return;
        }

        if let ModelEvent::TextDelta(delta) | ModelEvent::ThinkingDelta(delta) = &event {
            if !self.decided {
                self.prefix.push_str(delta);
                if let Some(first) = self.prefix.chars().find(|ch| !ch.is_whitespace()) {
                    self.decided = true;
                    self.stream_directly = !matches!(first, '<' | '{' | '[');
                }
            }
        }

        self.pending.push(event);
        if self.stream_directly {
            self.flush(on_event);
        }
    }

    fn flush(&mut self, on_event: &mut Box<dyn FnMut(ModelEvent) + '_>) {
        for event in std::mem::take(&mut self.pending) {
            on_event(event);
        }
    }
}

fn model_system_prompt(base: Option<&str>, tools: &[ModelToolSpec]) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(base) = base.filter(|base| !base.trim().is_empty()) {
        parts.push(base.trim().to_string());
    }
    if !tools.is_empty() {
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Available tool names for this turn: {names}. Prefer native tool calling when the provider supports it. If native tool calling is unavailable, emit exactly one XML block and no other text: <tool_call>{{\"name\":\"tool_name\",\"arguments\":{{}}}}</tool_call>. After tool results are provided, continue reasoning and either call another tool or produce the final answer."
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn selected_model_tool_specs(
    selected_tool_ids: &[String],
    snapshot: &ToolRegistrySnapshot,
) -> Vec<ModelToolSpec> {
    selected_tool_ids
        .iter()
        .filter_map(|id| snapshot.get(id))
        .map(|registered| descriptor_to_model_tool_spec(&registered.descriptor))
        .collect()
}

fn descriptor_to_model_tool_spec(descriptor: &ToolDescriptor) -> ModelToolSpec {
    ModelToolSpec {
        name: model_tool_name(descriptor),
        description: format!(
            "{}\nTool id: {}. Use this tool only when it directly helps the task.",
            descriptor.description, descriptor.id
        ),
        input_schema: descriptor.input_schema.clone(),
    }
}

fn model_tool_call_to_graph_call(
    call: &ModelToolCall,
    selected_tool_ids: &[String],
    snapshot: &ToolRegistrySnapshot,
) -> ToolCall {
    if let Some(registered) = resolve_model_tool_name(&call.name, selected_tool_ids, snapshot) {
        return ToolCall {
            id: call.id.clone(),
            tool_id: registered.id,
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        };
    }
    ToolCall {
        id: call.id.clone(),
        tool_id: unavailable_model_tool_id(&call.name),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
    }
}

fn unavailable_model_tool_id(name: &str) -> String {
    format!("__unavailable_model_tool__{}", sanitize_tool_name(name))
}

#[derive(Debug, Clone)]
struct ResolvedToolName {
    id: String,
}

fn resolve_model_tool_name(
    name: &str,
    selected_tool_ids: &[String],
    snapshot: &ToolRegistrySnapshot,
) -> Option<ResolvedToolName> {
    selected_tool_ids
        .iter()
        .filter_map(|id| snapshot.get(id))
        .map(|registered| registered.descriptor.clone())
        .find(|descriptor| {
            descriptor.id == name
                || descriptor.name == name
                || model_tool_name(descriptor) == name
                || sanitize_tool_name(&descriptor.id) == name
                || sanitize_tool_name(&descriptor.name) == name
        })
        .map(|descriptor| ResolvedToolName { id: descriptor.id })
}

fn model_tool_name(descriptor: &ToolDescriptor) -> String {
    let sanitized = sanitize_tool_name(&descriptor.id);
    if sanitized.is_empty() {
        sanitize_tool_name(&descriptor.name)
    } else {
        sanitized
    }
}

fn sanitize_tool_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').chars().take(64).collect()
}

fn parse_text_tool_calls(
    content: &str,
    selected_tool_ids: &[String],
    snapshot: &ToolRegistrySnapshot,
) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for value in extract_tool_call_values(content) {
        let Some(name) = value
            .get("name")
            .or_else(|| value.get("tool"))
            .or_else(|| value.get("tool_name"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let arguments = value
            .get("arguments")
            .or_else(|| value.get("args"))
            .or_else(|| value.get("input"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        let call = ModelToolCall {
            id: value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("text_tool_call_{}", calls.len() + 1)),
            name: name.to_string(),
            arguments,
        };
        calls.push(model_tool_call_to_graph_call(
            &call,
            selected_tool_ids,
            snapshot,
        ));
    }
    calls
}

fn extract_tool_call_values(content: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    for body in extract_tag_bodies(content, "tool_call")
        .into_iter()
        .chain(extract_tag_bodies(content, "tool_calls"))
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(array) = value.as_array() {
                values.extend(array.iter().cloned());
            } else {
                values.push(value);
            }
        }
    }
    if values.is_empty() {
        if let Some(value) = extract_json_tool_call(content) {
            if let Some(array) = value.as_array() {
                values.extend(array.iter().cloned());
            } else {
                values.push(value);
            }
        }
    }
    values
}

fn extract_tag_bodies(content: &str, tag: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = content;
    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        bodies.push(after_open[..end].trim().to_string());
        rest = &after_open[end + close.len()..];
    }
    bodies
}

fn extract_json_tool_call(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    if value.is_array()
        || value.get("tool_calls").is_some()
        || value.get("tool_call").is_some()
        || value.get("name").is_some()
    {
        return value
            .get("tool_calls")
            .or_else(|| value.get("tool_call"))
            .cloned()
            .or(Some(value));
    }
    None
}

impl GraphExecutor {
    pub fn builder() -> crate::graph::GraphBuilder {
        crate::graph::GraphBuilder::new()
    }

    pub fn run_with_planner(
        &self,
        request: GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
        planner: &mut dyn GraphPlanner,
        policy: &dyn PolicyGuard,
        audit: &dyn AuditRecorder,
        approval: Option<&dyn UserApproval>,
    ) -> Result<GraphRunSummary> {
        let mut events = NoopGraphEventSink;
        let mut services = GraphRunServices::new(policy, audit, approval, &mut events);
        self.run_with_planner_and_events(request, snapshot, planner, &mut services)
    }

    pub fn run_with_planner_and_events(
        &self,
        request: GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
        planner: &mut dyn GraphPlanner,
        services: &mut GraphRunServices<'_>,
    ) -> Result<GraphRunSummary> {
        self.run_with_seed_messages_and_events(request, snapshot, planner, services, Vec::new())
    }

    pub fn run_with_seed_messages_and_events(
        &self,
        request: GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
        planner: &mut dyn GraphPlanner,
        services: &mut GraphRunServices<'_>,
        seed_messages: Vec<GraphMessage>,
    ) -> Result<GraphRunSummary> {
        let mut state = GraphState {
            messages: seed_messages,
            ..GraphState::default()
        };
        state
            .messages
            .push(GraphMessage::User(request.user_input.clone()));
        for hook in &self.hooks {
            hook.before_run(&mut state, &request)?;
        }

        services.audit.record(AuditEvent::GraphRunStarted {
            registry_version: snapshot.version(),
            user_input: request.user_input.clone(),
        });
        services.events.emit(GraphEvent::GraphStarted);
        services.events.emit(GraphEvent::ContextBuilt {
            context_id: request.context_pack.id.clone(),
        });
        services.events.emit(GraphEvent::ToolSelectionStarted);
        services.audit.record(AuditEvent::ToolsSelected {
            tool_ids: request.context_pack.selected_tool_ids.clone(),
        });
        for tool_id in &request.context_pack.selected_tool_ids {
            services.events.emit(GraphEvent::ToolSelected {
                tool_id: tool_id.clone(),
            });
        }

        while state.round_count < self.max_rounds {
            state.round_count += 1;
            services.events.emit(GraphEvent::ModelStarted);
            let step = planner.next_step(&state, &request, snapshot)?;
            for hook in &self.hooks {
                hook.after_step(&mut state, &step)?;
            }
            match step {
                GraphStep::Interrupt(interrupt) => {
                    return self.interrupt_run(state, snapshot, services, interrupt);
                }
                GraphStep::Respond(content) => {
                    let output_chars = content.chars().count();
                    state.last_content = content.clone();
                    state.messages.push(GraphMessage::Assistant(content));
                    state.status = GraphStatus::Completed;
                    services
                        .events
                        .emit(GraphEvent::ModelCompleted { output_chars });
                    for hook in self.hooks.iter().rev() {
                        hook.after_run(&mut state)?;
                    }
                    services.audit.record(AuditEvent::GraphRunCompleted);
                    services.events.emit(GraphEvent::GraphCompleted);
                    return Ok(GraphRunSummary {
                        registry_version: snapshot.version(),
                        state,
                    });
                }
                GraphStep::RespondWithUsage {
                    content,
                    token_usage,
                } => {
                    let output_chars = content.chars().count();
                    state.token_usage.input_tokens += token_usage.input_tokens;
                    state.token_usage.output_tokens += token_usage.output_tokens;
                    state.last_content = content.clone();
                    state.messages.push(GraphMessage::Assistant(content));
                    state.status = GraphStatus::Completed;
                    services
                        .events
                        .emit(GraphEvent::ModelCompleted { output_chars });
                    for hook in self.hooks.iter().rev() {
                        hook.after_run(&mut state)?;
                    }
                    services.audit.record(AuditEvent::TokenUsageRecorded {
                        input_tokens: state.token_usage.input_tokens,
                        output_tokens: state.token_usage.output_tokens,
                    });
                    services.audit.record(AuditEvent::GraphRunCompleted);
                    services.events.emit(GraphEvent::GraphCompleted);
                    return Ok(GraphRunSummary {
                        registry_version: snapshot.version(),
                        state,
                    });
                }
                GraphStep::CallTools(calls) => {
                    state.pending_tool_calls = calls;
                    let calls = std::mem::take(&mut state.pending_tool_calls);
                    if !calls.is_empty() {
                        state.messages.push(GraphMessage::AssistantToolCalls {
                            calls: calls.clone(),
                        });
                    }
                    for call in calls {
                        let outcome =
                            self.execute_tool_call(&mut state, &call, snapshot, services)?;
                        let mut result = match outcome {
                            ToolExecutionOutcome::Completed(result) => result,
                            ToolExecutionOutcome::Interrupted(interrupt) => {
                                return self.interrupt_run(state, snapshot, services, interrupt);
                            }
                        };
                        annotate_tool_result_call(&mut result, &call);
                        state.messages.push(GraphMessage::Tool {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                        });
                        state.tool_results.push(result);
                    }
                }
                GraphStep::AskUser { prompt } => {
                    services.events.emit(GraphEvent::UserApprovalRequested {
                        prompt: prompt.clone(),
                    });
                    let synthetic_call = ToolCall {
                        id: "graph-user-confirmation".to_string(),
                        tool_id: "graph.user".to_string(),
                        name: "user_confirmation".to_string(),
                        arguments: serde_json::json!({ "prompt": prompt }),
                    };
                    let Some(approval) = services.approval else {
                        let interrupt = GraphInterrupt {
                            id: synthetic_call.id.clone(),
                            kind: GraphInterruptKind::Clarification,
                            prompt,
                            reason: "graph requested user clarification".to_string(),
                            tool_call: Some(synthetic_call),
                        };
                        return self.interrupt_run(state, snapshot, services, interrupt);
                    };
                    let allowed = approval.ask_user(&prompt, &synthetic_call);
                    if !allowed {
                        state.status = GraphStatus::Cancelled;
                        state.last_content =
                            "user confirmation was rejected or unavailable".to_string();
                        for hook in self.hooks.iter().rev() {
                            hook.after_run(&mut state)?;
                        }
                        services.audit.record(AuditEvent::GraphRunCompleted);
                        services.events.emit(GraphEvent::GraphCompleted);
                        return Ok(GraphRunSummary {
                            registry_version: snapshot.version(),
                            state,
                        });
                    }
                    state.messages.push(GraphMessage::System(
                        "user confirmed graph prompt".to_string(),
                    ));
                }
                GraphStep::Stop => {
                    state.status = GraphStatus::Completed;
                    for hook in self.hooks.iter().rev() {
                        hook.after_run(&mut state)?;
                    }
                    services.audit.record(AuditEvent::GraphRunCompleted);
                    services.events.emit(GraphEvent::GraphCompleted);
                    return Ok(GraphRunSummary {
                        registry_version: snapshot.version(),
                        state,
                    });
                }
            }
        }

        state.status = GraphStatus::MaxRoundsReached;
        for hook in self.hooks.iter().rev() {
            hook.after_run(&mut state)?;
        }
        services.audit.record(AuditEvent::GraphRunCompleted);
        services.events.emit(GraphEvent::GraphCompleted);
        Ok(GraphRunSummary {
            registry_version: snapshot.version(),
            state,
        })
    }

    fn interrupt_run(
        &self,
        mut state: GraphState,
        snapshot: &ToolRegistrySnapshot,
        services: &mut GraphRunServices<'_>,
        interrupt: GraphInterrupt,
    ) -> Result<GraphRunSummary> {
        state.status = GraphStatus::Interrupted;
        state.last_content = interrupt.prompt.clone();
        state.interrupt = Some(interrupt.clone());
        for hook in self.hooks.iter().rev() {
            hook.after_run(&mut state)?;
        }
        services.audit.record(AuditEvent::GraphRunCompleted);
        services
            .events
            .emit(GraphEvent::GraphInterrupted { interrupt });
        services.events.emit(GraphEvent::GraphCompleted);
        Ok(GraphRunSummary {
            registry_version: snapshot.version(),
            state,
        })
    }

    pub fn run(
        &self,
        request: GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
        policy: &dyn PolicyGuard,
        audit: &dyn AuditRecorder,
        approval: Option<&dyn UserApproval>,
    ) -> Result<GraphRunSummary> {
        let mut planner = NoopPlanner;
        self.run_with_planner(request, snapshot, &mut planner, policy, audit, approval)
    }

    fn execute_tool_call(
        &self,
        state: &mut GraphState,
        call: &ToolCall,
        snapshot: &ToolRegistrySnapshot,
        services: &mut GraphRunServices<'_>,
    ) -> Result<ToolExecutionOutcome> {
        services.audit.record(AuditEvent::ToolCallRequested {
            call_id: call.id.clone(),
            tool_id: call.tool_id.clone(),
            name: call.name.clone(),
        });
        services.events.emit(GraphEvent::ToolCallStarted {
            call_id: call.id.clone(),
            tool_id: call.tool_id.clone(),
            name: call.name.clone(),
        });

        for hook in &self.hooks {
            hook.before_tool(state, call)?;
        }

        let Some(registered) = snapshot.get(&call.tool_id) else {
            let result = ToolResult::error(format!(
                "unknown tool '{}'. The tool is not available in this turn's registry snapshot.",
                call.tool_id
            ));
            services.audit.record(AuditEvent::ToolResultRecorded {
                call_id: call.id.clone(),
                is_error: true,
                metadata: result.metadata.clone(),
            });
            services.events.emit(GraphEvent::ToolCompleted {
                call_id: call.id.clone(),
                tool_id: call.tool_id.clone(),
                name: call.name.clone(),
                is_error: true,
                content_preview: preview_chars(&result.content, 1200),
                metadata: result.metadata.clone(),
            });
            return Ok(ToolExecutionOutcome::Completed(result));
        };
        let descriptor = registered.descriptor.clone();

        let decision = services.policy.check(&descriptor, call);
        services.audit.record(AuditEvent::PolicyDecisionRecorded {
            call_id: call.id.clone(),
            decision: decision.clone(),
        });
        services.events.emit(GraphEvent::ToolPolicyDecision {
            call_id: call.id.clone(),
            tool_id: call.tool_id.clone(),
            name: call.name.clone(),
            decision: decision.clone(),
        });

        let result = match decision {
            PolicyDecision::Allow => execute_snapshot_tool_with_events(snapshot, call, services)?,
            PolicyDecision::Deny { reason } => ToolResult::error(reason),
            PolicyDecision::AskUser { prompt } => {
                services.events.emit(GraphEvent::UserApprovalRequested {
                    prompt: prompt.clone(),
                });
                let Some(approval) = services.approval else {
                    return Ok(ToolExecutionOutcome::Interrupted(GraphInterrupt {
                        id: call.id.clone(),
                        kind: GraphInterruptKind::ToolApproval,
                        prompt,
                        reason: "tool call requires user approval".to_string(),
                        tool_call: Some(call.clone()),
                    }));
                };
                if approval.ask_user(&prompt, call) {
                    execute_snapshot_tool_with_events(snapshot, call, services)?
                } else {
                    ToolResult::error("user rejected tool call")
                }
            }
        };

        services.audit.record(AuditEvent::ToolResultRecorded {
            call_id: call.id.clone(),
            is_error: result.is_error,
            metadata: result.metadata.clone(),
        });
        services.events.emit(GraphEvent::ToolCompleted {
            call_id: call.id.clone(),
            tool_id: call.tool_id.clone(),
            name: call.name.clone(),
            is_error: result.is_error,
            content_preview: preview_chars(&result.content, 1200),
            metadata: result.metadata.clone(),
        });

        for hook in self.hooks.iter().rev() {
            hook.after_tool(state, call, &result)?;
        }

        Ok(ToolExecutionOutcome::Completed(result))
    }
}

fn preview_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn execute_snapshot_tool_with_events(
    snapshot: &ToolRegistrySnapshot,
    call: &ToolCall,
    services: &mut GraphRunServices<'_>,
) -> Result<ToolResult> {
    let call_id = call.id.clone();
    let context = services.tool_context.clone();
    let events = &mut services.events;
    let mut runtime = ToolExecutionRuntime::new(context, move |event| {
        events.emit(GraphEvent::ToolRuntimeEvent {
            call_id: call_id.clone(),
            event,
        });
    });
    snapshot.execute_with_runtime(call, &mut runtime)
}

fn annotate_tool_result_call(result: &mut ToolResult, call: &ToolCall) {
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
