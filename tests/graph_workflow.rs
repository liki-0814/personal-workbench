use pwcli::graph::{
    GraphWorkflow, WorkflowContext, WorkflowEdgeCondition, WorkflowExecutor, WorkflowNode,
    WorkflowNodeKind, WorkflowNodeRunner, WorkflowPlanKind, WorkflowStatus, WorkflowStepOutcome,
};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Default)]
struct ScriptedRunner {
    outcomes: BTreeMap<String, WorkflowStepOutcome>,
}

impl ScriptedRunner {
    fn with(mut self, node_id: &str, outcome: WorkflowStepOutcome) -> Self {
        self.outcomes.insert(node_id.to_string(), outcome);
        self
    }
}

impl WorkflowNodeRunner for ScriptedRunner {
    fn run_node(
        &mut self,
        _workflow: &GraphWorkflow,
        node: &WorkflowNode,
        _context: &WorkflowContext,
    ) -> pwcli::Result<WorkflowStepOutcome> {
        Ok(self
            .outcomes
            .remove(&node.id)
            .unwrap_or_else(|| WorkflowStepOutcome::Success(json!({ "ok": true }))))
    }
}

#[test]
fn workflow_executes_linear_agent_tool_chain() {
    let workflow = GraphWorkflow::builder("linear", "agent")
        .node(WorkflowNode::agent_task(
            "agent",
            "Agent",
            "codex",
            "plan",
            "plan this",
        ))
        .node(WorkflowNode::tool_call(
            "verify",
            "Verify",
            "verification.project_check",
            json!({ "commands": ["cargo test"] }),
        ))
        .node(WorkflowNode::end("end", "End"))
        .edge("agent", "verify", WorkflowEdgeCondition::OnSuccess)
        .edge("verify", "end", WorkflowEdgeCondition::OnSuccess)
        .build()
        .unwrap();

    let mut runner = ScriptedRunner::default()
        .with(
            "agent",
            WorkflowStepOutcome::Success(json!({ "plan": "ok" })),
        )
        .with(
            "verify",
            WorkflowStepOutcome::Success(json!({ "passed": true })),
        );
    let summary = WorkflowExecutor::new().run(&workflow, &mut runner).unwrap();

    assert_eq!(summary.status, WorkflowStatus::Completed);
    assert_eq!(summary.visited, vec!["agent", "verify", "end"]);
    assert_eq!(summary.outputs["verify"]["passed"], true);
}

#[test]
fn workflow_routes_failure_to_review_agent() {
    let workflow = GraphWorkflow::code_agent_plan_execute_review("implement feature");
    let mut runner = ScriptedRunner::default()
        .with(
            "plan",
            WorkflowStepOutcome::Success(json!({ "plan": "ok" })),
        )
        .with(
            "approve_plan",
            WorkflowStepOutcome::Success(json!({ "approved": true })),
        )
        .with(
            "execute",
            WorkflowStepOutcome::Failure("tests failed".to_string()),
        )
        .with(
            "review",
            WorkflowStepOutcome::Success(json!({ "reviewed": true })),
        );

    let summary = WorkflowExecutor::new().run(&workflow, &mut runner).unwrap();

    assert_eq!(summary.status, WorkflowStatus::Completed);
    assert_eq!(
        summary.visited,
        vec!["plan", "approve_plan", "execute", "review", "end"]
    );
    assert!(!summary.visited.contains(&"verify".to_string()));
}

#[test]
fn workflow_interrupts_when_user_approval_is_needed() {
    let workflow = GraphWorkflow::builder("approval", "ask")
        .node(WorkflowNode::approval("ask", "Ask", "Approve execution?"))
        .node(WorkflowNode::end("end", "End"))
        .edge("ask", "end", WorkflowEdgeCondition::OnSuccess)
        .build()
        .unwrap();
    let mut runner = ScriptedRunner::default().with(
        "ask",
        WorkflowStepOutcome::Interrupt {
            prompt: "Approve execution?".to_string(),
            reason: "approval required".to_string(),
        },
    );

    let summary = WorkflowExecutor::new().run(&workflow, &mut runner).unwrap();

    assert_eq!(summary.status, WorkflowStatus::Interrupted);
    assert_eq!(summary.visited, vec!["ask"]);
    assert_eq!(
        summary.interrupt.unwrap().prompt,
        "Approve execution?".to_string()
    );
}

#[test]
fn planned_workflow_auto_routes_by_goal() {
    let research = GraphWorkflow::planned(
        "调研 runtime task 和 deep research 的差异",
        "codex",
        WorkflowPlanKind::Auto,
    );
    assert_eq!(research.name, "research_collect_synthesize_verify");
    assert!(research.nodes.contains_key("web_search"));
    assert!(research.nodes.contains_key("local_context"));

    let latest = GraphWorkflow::planned(
        "find the latest Rust async runtime notes",
        "codex",
        WorkflowPlanKind::Auto,
    );
    assert_eq!(latest.name, "research_collect_synthesize_verify");

    let ops = GraphWorkflow::planned(
        "ssh 到远程机器检查部署日志",
        "codex",
        WorkflowPlanKind::Auto,
    );
    assert_eq!(ops.name, "ops_plan_execute_verify");

    let operations = GraphWorkflow::planned(
        "clean up project tasks and operations",
        "codex",
        WorkflowPlanKind::Auto,
    );
    assert_eq!(operations.name, "ops_plan_execute_verify");

    let code = GraphWorkflow::planned(
        "实现 recipe 保存功能并补测试",
        "codex",
        WorkflowPlanKind::Auto,
    );
    assert_eq!(code.name, "code_agent_plan_execute_review");

    let general = GraphWorkflow::planned("整理明天的工作优先级", "codex", WorkflowPlanKind::Auto);
    assert_eq!(general.name, "general_plan_execute_review");
}

#[test]
fn planned_workflow_uses_requested_agent_for_agent_nodes() {
    let workflow = GraphWorkflow::planned("实现一个功能", "claude", WorkflowPlanKind::Code);

    for node in workflow.nodes.values() {
        if let WorkflowNodeKind::AgentTask { agent, .. } = &node.kind {
            assert_eq!(agent, "claude");
        }
    }
}
