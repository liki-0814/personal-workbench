use pwcli::{
    audit::{AuditEvent, InMemoryAuditRecorder},
    context::ContextBuilder,
    graph::{GraphEvent, GraphEventSink, GraphExecutor, GraphRunRequest, GraphRunServices},
    policy::{PolicyDecision, PolicyGuard},
    tools::{
        InvocationMode, LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolExecutor,
        ToolRegistry, ToolResult, ToolSource,
    },
};
use serde_json::json;
use std::sync::Arc;

struct EchoExecutor;

impl ToolExecutor for EchoExecutor {
    fn execute(&self, _call: &ToolCall) -> pwcli::Result<ToolResult> {
        Ok(ToolResult::ok("executed"))
    }
}

struct DenyPolicy;

impl PolicyGuard for DenyPolicy {
    fn check(&self, _descriptor: &ToolDescriptor, _call: &ToolCall) -> PolicyDecision {
        PolicyDecision::Deny {
            reason: "blocked by test policy".to_string(),
        }
    }
}

struct CollectGraphEvents {
    events: Vec<GraphEvent>,
}

impl GraphEventSink for CollectGraphEvents {
    fn emit(&mut self, event: GraphEvent) {
        self.events.push(event);
    }
}

fn loaded_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.echo".to_string(),
            name: "echo".to_string(),
            description: "returns input".to_string(),
            input_schema: json!({"type": "object"}),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![],
            metadata: json!({}),
            enabled: true,
        },
        executor: Some(Arc::new(EchoExecutor)),
    }
}

#[test]
fn graph_uses_snapshot_then_policy_then_audit() {
    let mut registry = ToolRegistry::new();
    registry.register(loaded_tool());
    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("run echo", &snapshot);

    let audit = InMemoryAuditRecorder::default();
    let graph = GraphExecutor::builder().build();
    let mut graph_events = CollectGraphEvents { events: Vec::new() };
    let summary = {
        let mut services = GraphRunServices::new(&DenyPolicy, &audit, None, &mut graph_events);
        graph
            .run_with_planner_and_events(
                GraphRunRequest {
                    user_input: "run echo".to_string(),
                    context_pack,
                },
                &snapshot,
                &mut pwcli::graph::PlannedToolCallPlanner::new(vec![ToolCall {
                    id: "call-1".to_string(),
                    tool_id: "builtin.echo".to_string(),
                    name: "echo".to_string(),
                    arguments: json!({}),
                }]),
                &mut services,
            )
            .unwrap()
    };

    assert_eq!(summary.registry_version, snapshot.version());
    assert_eq!(summary.state.tool_results.len(), 1);
    assert!(summary.state.tool_results[0].is_error);
    assert_eq!(
        summary.state.tool_results[0].content,
        "blocked by test policy"
    );

    let events = audit.events();
    assert!(matches!(events[0], AuditEvent::GraphRunStarted { .. }));
    assert!(events.iter().any(|event| matches!(
        event,
        AuditEvent::PolicyDecisionRecorded {
            decision: PolicyDecision::Deny { .. },
            ..
        }
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, AuditEvent::ToolResultRecorded { is_error: true, .. })));

    assert!(matches!(graph_events.events[0], GraphEvent::GraphStarted));
    assert!(graph_events
        .events
        .iter()
        .any(|event| matches!(event, GraphEvent::ContextBuilt { .. })));
    assert!(graph_events
        .events
        .iter()
        .any(|event| matches!(event, GraphEvent::ToolSelectionStarted)));
    assert!(graph_events.events.iter().any(
        |event| matches!(event, GraphEvent::ToolSelected { tool_id } if tool_id == "builtin.echo")
    ));
    assert!(graph_events.events.iter().any(
        |event| matches!(event, GraphEvent::ToolCallStarted { call_id, .. } if call_id == "call-1")
    ));
    assert!(graph_events.events.iter().any(|event| matches!(
        event,
        GraphEvent::ToolPolicyDecision {
            decision: PolicyDecision::Deny { .. },
            ..
        }
    )));
    assert!(matches!(
        graph_events.events.last(),
        Some(GraphEvent::GraphCompleted)
    ));
}
