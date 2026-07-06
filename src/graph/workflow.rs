use crate::{PwError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type WorkflowNodeId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphWorkflow {
    pub name: String,
    pub start: WorkflowNodeId,
    pub nodes: BTreeMap<WorkflowNodeId, WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    pub max_steps: usize,
}

impl GraphWorkflow {
    pub fn builder(name: impl Into<String>, start: impl Into<String>) -> WorkflowBuilder {
        WorkflowBuilder {
            workflow: Self {
                name: name.into(),
                start: start.into(),
                nodes: BTreeMap::new(),
                edges: Vec::new(),
                max_steps: 64,
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !self.nodes.contains_key(&self.start) {
            return Err(PwError::Message(format!(
                "workflow '{}' start node '{}' is missing",
                self.name, self.start
            )));
        }
        for edge in &self.edges {
            if !self.nodes.contains_key(&edge.from) {
                return Err(PwError::Message(format!(
                    "workflow '{}' edge source '{}' is missing",
                    self.name, edge.from
                )));
            }
            if !self.nodes.contains_key(&edge.to) {
                return Err(PwError::Message(format!(
                    "workflow '{}' edge target '{}' is missing",
                    self.name, edge.to
                )));
            }
        }
        Ok(())
    }

    pub fn next_node(
        &self,
        from: &str,
        condition: WorkflowEdgeCondition,
    ) -> Option<WorkflowNodeId> {
        next_workflow_node(self, from, condition)
    }

    pub fn code_agent_plan_execute_review(goal: impl Into<String>) -> Self {
        Self::code_agent_plan_execute_review_with_agent(goal, "codex")
    }

    pub fn code_agent_plan_execute_review_with_agent(
        goal: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        let goal = goal.into();
        let agent = agent.into();
        Self::builder("code_agent_plan_execute_review", "plan")
            .node(WorkflowNode::agent_task(
                "plan",
                "Plan",
                agent.clone(),
                "plan",
                format!("Discuss and write a plan for this goal:\n{goal}"),
            ))
            .node(WorkflowNode::approval(
                "approve_plan",
                "Approve Plan",
                "Review the generated plan before execution.",
            ))
            .node(WorkflowNode::agent_task(
                "execute",
                "Execute",
                agent.clone(),
                "execute",
                format!("Execute the approved plan for this goal:\n{goal}"),
            ))
            .node(WorkflowNode::tool_call(
                "verify",
                "Verify",
                "verification.project_check",
                serde_json::json!({}),
            ))
            .node(WorkflowNode::agent_task(
                "review",
                "Review",
                agent,
                "review",
                format!("Review the implementation and verification result for:\n{goal}"),
            ))
            .node(WorkflowNode::end("end", "End"))
            .edge("plan", "approve_plan", WorkflowEdgeCondition::OnSuccess)
            .edge("approve_plan", "execute", WorkflowEdgeCondition::OnSuccess)
            .edge("execute", "verify", WorkflowEdgeCondition::OnSuccess)
            .edge("execute", "review", WorkflowEdgeCondition::OnFailure)
            .edge("verify", "end", WorkflowEdgeCondition::OnSuccess)
            .edge("verify", "review", WorkflowEdgeCondition::OnFailure)
            .edge("review", "end", WorkflowEdgeCondition::Always)
            .build()
            .expect("built-in workflow is valid")
    }

    pub fn planned(
        goal: impl Into<String>,
        agent: impl Into<String>,
        kind: WorkflowPlanKind,
    ) -> Self {
        let goal = goal.into();
        let agent = agent.into();
        match kind.resolve(&goal) {
            WorkflowPlanKind::Code => Self::code_agent_plan_execute_review_with_agent(goal, agent),
            WorkflowPlanKind::Research => Self::research(goal, agent),
            WorkflowPlanKind::Ops => Self::ops(goal, agent),
            WorkflowPlanKind::General => Self::general(goal, agent),
            WorkflowPlanKind::Auto => unreachable!("WorkflowPlanKind::resolve never returns Auto"),
        }
    }

    pub fn research(goal: impl Into<String>, _agent: impl Into<String>) -> Self {
        let goal = goal.into();
        let search_query = research_search_query(&goal);
        Self::builder("research_collect_synthesize_verify", "plan")
            .node(WorkflowNode::model_turn(
                "plan",
                "Plan Research",
                format!(
                    "Plan a focused research task for this goal. Do not answer the goal yet and do not invent paper titles. Return a concise research plan with: research questions, 3 search queries, inclusion/exclusion criteria, risks, and success criteria. Keep it under 900 chars:\n{goal}"
                ),
            ))
            .node(WorkflowNode::adaptive_loop(
                "adaptive_research",
                "Adaptive Research",
                format!(
                    "Run adaptive research for this goal:\n{goal}\n\nSuggested initial academic query: {search_query}\n\nUse tools only when useful. Decide after each tool result whether to search again, extract a page, parse a PDF with MinerU, inspect local context, ask for approval, or stop. For papers, prefer primary sources. If a source is arXiv/html/pdf, use the PDF/MinerU path before claiming a full-paper read. If MinerU cannot run or is rejected, label the evidence as snippet-only or extracted-page-only. Replace numeric arXiv-style titles such as 2605.19457 with the real paper title when available. Do not use code-agent tools for this research route."
                ),
            ))
            .node(WorkflowNode::model_turn(
                "final_report",
                "Finalize Report",
                "Produce the final user-facing answer from the adaptive research output. Preserve evidence labels such as full_pdf_mineru, web_extract, and snippet-only. Remove unsupported claims, mention uncertainty, and do not include internal tool logs.",
            ))
            .node(WorkflowNode::end("end", "End"))
            .edge("plan", "adaptive_research", WorkflowEdgeCondition::OnSuccess)
            .edge("adaptive_research", "final_report", WorkflowEdgeCondition::OnSuccess)
            .edge("adaptive_research", "final_report", WorkflowEdgeCondition::OnFailure)
            .edge("final_report", "end", WorkflowEdgeCondition::Always)
            .build()
            .expect("built-in workflow is valid")
    }

    pub fn ops(goal: impl Into<String>, _agent: impl Into<String>) -> Self {
        let goal = goal.into();
        Self::builder("ops_plan_execute_verify", "plan")
            .node(WorkflowNode::model_turn(
                "plan",
                "Plan Ops",
                format!(
                    "Plan an operations task for this goal. Identify affected systems, rollback paths, and checks. Do not change systems in this step:\n{goal}"
                ),
            ))
            .node(WorkflowNode::approval(
                "approve_plan",
                "Approve Plan",
                "Review the operations plan before execution.",
            ))
            .node(WorkflowNode::model_turn(
                "execute",
                "Execute",
                format!("Execute the approved operations plan for this goal:\n{goal}"),
            ))
            .node(WorkflowNode::tool_call(
                "verify",
                "Verify",
                "verification.project_check",
                serde_json::json!({}),
            ))
            .node(WorkflowNode::model_turn(
                "review",
                "Review",
                format!("Review the operations result, logs, and verification for:\n{goal}"),
            ))
            .node(WorkflowNode::end("end", "End"))
            .edge("plan", "approve_plan", WorkflowEdgeCondition::OnSuccess)
            .edge("approve_plan", "execute", WorkflowEdgeCondition::OnSuccess)
            .edge("execute", "verify", WorkflowEdgeCondition::OnSuccess)
            .edge("execute", "review", WorkflowEdgeCondition::OnFailure)
            .edge("verify", "end", WorkflowEdgeCondition::OnSuccess)
            .edge("verify", "review", WorkflowEdgeCondition::OnFailure)
            .edge("review", "end", WorkflowEdgeCondition::Always)
            .build()
            .expect("built-in workflow is valid")
    }

    pub fn general(goal: impl Into<String>, _agent: impl Into<String>) -> Self {
        let goal = goal.into();
        Self::builder("general_plan_execute_review", "plan")
            .node(WorkflowNode::model_turn(
                "plan",
                "Plan",
                format!("Plan a concise task graph for this goal:\n{goal}"),
            ))
            .node(WorkflowNode::approval(
                "approve_plan",
                "Approve Plan",
                "Review the generated plan before execution.",
            ))
            .node(WorkflowNode::model_turn(
                "execute",
                "Execute",
                format!("Execute the approved plan for this goal:\n{goal}"),
            ))
            .node(WorkflowNode::model_turn(
                "review",
                "Review",
                format!("Review the result and identify remaining risks for:\n{goal}"),
            ))
            .node(WorkflowNode::end("end", "End"))
            .edge("plan", "approve_plan", WorkflowEdgeCondition::OnSuccess)
            .edge("approve_plan", "execute", WorkflowEdgeCondition::OnSuccess)
            .edge("execute", "review", WorkflowEdgeCondition::Always)
            .edge("review", "end", WorkflowEdgeCondition::Always)
            .build()
            .expect("built-in workflow is valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPlanKind {
    Auto,
    Code,
    Research,
    Ops,
    General,
}

impl WorkflowPlanKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "code" | "coding" | "implementation" | "implement" => Some(Self::Code),
            "research" | "deep-research" | "deep_research" | "调研" => Some(Self::Research),
            "ops" | "operation" | "operations" | "infra" | "ssh" | "运维" => Some(Self::Ops),
            "general" | "task" => Some(Self::General),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Code => "code",
            Self::Research => "research",
            Self::Ops => "ops",
            Self::General => "general",
        }
    }

    pub fn resolve(self, goal: &str) -> Self {
        if self != Self::Auto {
            return self;
        }
        let lower = goal.to_ascii_lowercase();
        if contains_any(
            goal,
            &["调研", "研究", "资料", "竞品", "论文", "搜索", "对比"],
        ) || contains_any_word(
            &lower,
            &[
                "research",
                "investigate",
                "latest",
                "compare",
                "survey",
                "paper",
                "papers",
                "source",
                "sources",
                "market",
            ],
        ) {
            return Self::Research;
        }
        if contains_any(
            goal,
            &[
                "服务器",
                "部署",
                "远程",
                "日志",
                "运维",
                "巡检",
                "告警",
                "监控",
                "发布",
                "维护",
                "ssh",
            ],
        ) || contains_any_word(
            &lower,
            &[
                "ssh",
                "server",
                "servers",
                "deploy",
                "deployment",
                "ops",
                "operation",
                "operations",
                "infra",
                "infrastructure",
                "incident",
                "log",
                "logs",
                "remote",
                "maintenance",
                "runbook",
                "monitor",
                "monitoring",
                "restart",
                "service",
                "production",
            ],
        ) {
            return Self::Ops;
        }
        if contains_any(
            goal,
            &[
                "代码",
                "代码库",
                "仓库",
                "前端",
                "后端",
                "组件",
                "接口",
                "函数",
                "模块",
                "编译",
                "构建",
                "单测",
                "报错",
                "bug",
                "调试",
                "diff",
                "patch",
            ],
        ) || contains_any(&lower, &["pull request", "code review"])
            || contains_any_word(
                &lower,
                &[
                    "code",
                    "codebase",
                    "repo",
                    "repository",
                    "frontend",
                    "backend",
                    "react",
                    "rust",
                    "vite",
                    "refactor",
                    "test",
                    "tests",
                    "testing",
                    "build",
                    "compile",
                    "lint",
                    "typecheck",
                    "bug",
                    "pr",
                    "function",
                    "module",
                    "component",
                    "diff",
                    "patch",
                ],
            )
            || contains_code_action(goal, &lower)
        {
            return Self::Code;
        }
        Self::General
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn contains_any_word(value: &str, needles: &[&str]) -> bool {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| needles.contains(&word))
}

fn contains_code_action(goal: &str, lower: &str) -> bool {
    let chinese_action = contains_any(
        goal,
        &[
            "阅读", "理解", "解释", "分析", "检查", "审查", "修改", "修复", "重构", "实现", "新增",
            "优化",
        ],
    ) && contains_any(
        goal,
        &[
            "代码",
            "代码库",
            "仓库",
            "文件",
            "组件",
            "函数",
            "模块",
            "接口",
            "页面",
            "网页",
            "前端",
            "后端",
            "UI",
            "diff",
            "patch",
        ],
    );
    let english_action = contains_any_word(
        lower,
        &[
            "implement",
            "fix",
            "change",
            "update",
            "add",
            "review",
            "analyze",
            "explain",
            "read",
        ],
    ) && contains_any_word(
        lower,
        &[
            "code",
            "codebase",
            "repo",
            "repository",
            "file",
            "function",
            "module",
            "component",
            "api",
            "bug",
            "test",
            "build",
            "ui",
            "page",
            "frontend",
            "backend",
            "diff",
            "patch",
            "pr",
        ],
    );
    chinese_action || english_action
}

fn research_search_query(goal: &str) -> String {
    let mut query = goal.to_string();
    let mut chinese_noise = [
        "并产出调研报告",
        "并产出中文的调研报告",
        "并输出中文的调研报告",
        "产出中文的调研报告",
        "输出中文的调研报告",
        "中文调研报告",
        "中文的调研报告",
        "中文报告",
        "用中文",
        "中文的",
        "并输出调研报告",
        "产出调研报告",
        "输出调研报告",
        "的相关论文",
        "相关论文",
        "论文列表",
        "帮我",
        "麻烦",
        "请",
        "一下",
        "调研一下",
        "调研",
        "研究",
        "相关",
        "产出",
        "输出",
        "一份",
        "报告",
        "调研报告",
        "论文",
        "有哪些",
    ]
    .to_vec();
    chinese_noise.sort_by_key(|item| std::cmp::Reverse(item.chars().count()));
    for noise in chinese_noise {
        query = query.replace(noise, " ");
    }
    for noise in [
        "，并",
        ", and",
        " and produce",
        " and write",
        "并产出",
        "并输出",
    ] {
        query = query.replace(noise, " ");
    }
    for separator in ["，", "。", "、", "；", "：", "？", ",", ";", ":", "?"] {
        query = query.replace(separator, " ");
    }
    let mut english_noise = [
        "please",
        "help me",
        "help",
        "survey",
        "research",
        "investigate",
        "report",
        "write",
        "produce",
        "related papers",
        "papers about",
        "papers",
        "paper",
    ]
    .to_vec();
    english_noise.sort_by_key(|item| std::cmp::Reverse(item.len()));
    for noise in english_noise {
        query = query.replace(noise, " ");
    }
    let collapsed = query
        .split_whitespace()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let topic = if collapsed.trim().is_empty() {
        goal.trim()
    } else {
        collapsed.trim()
    };
    format!("{topic} paper arxiv academic")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: WorkflowNodeId,
    pub label: String,
    pub kind: WorkflowNodeKind,
}

impl WorkflowNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: WorkflowNodeKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
        }
    }

    pub fn agent_task(
        id: impl Into<String>,
        label: impl Into<String>,
        agent: impl Into<String>,
        mode: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::AgentTask {
                agent: agent.into(),
                mode: mode.into(),
                prompt: prompt.into(),
            },
        )
    }

    pub fn tool_call(
        id: impl Into<String>,
        label: impl Into<String>,
        tool_id: impl Into<String>,
        arguments: Value,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::ToolCall {
                tool_id: tool_id.into(),
                arguments,
            },
        )
    }

    pub fn approval(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::Approval {
                prompt: prompt.into(),
            },
        )
    }

    pub fn model_turn(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::ModelTurn {
                prompt: prompt.into(),
            },
        )
    }

    pub fn research_read_papers(
        id: impl Into<String>,
        label: impl Into<String>,
        max_papers: usize,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::ResearchReadPapers { max_papers },
        )
    }

    pub fn adaptive_loop(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            label,
            WorkflowNodeKind::AdaptiveLoop {
                prompt: prompt.into(),
            },
        )
    }

    pub fn end(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(id, label, WorkflowNodeKind::End)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeKind {
    ModelTurn {
        prompt: String,
    },
    AgentTask {
        agent: String,
        mode: String,
        prompt: String,
    },
    ToolCall {
        tool_id: String,
        arguments: Value,
    },
    ResearchReadPapers {
        max_papers: usize,
    },
    AdaptiveLoop {
        prompt: String,
    },
    Approval {
        prompt: String,
    },
    Join,
    SubWorkflow {
        workflow: Box<GraphWorkflow>,
    },
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    pub from: WorkflowNodeId,
    pub to: WorkflowNodeId,
    pub condition: WorkflowEdgeCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEdgeCondition {
    Always,
    OnSuccess,
    OnFailure,
    OnInterrupt,
}

#[derive(Debug, Clone)]
pub struct WorkflowBuilder {
    workflow: GraphWorkflow,
}

impl WorkflowBuilder {
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.workflow.max_steps = max_steps;
        self
    }

    pub fn node(mut self, node: WorkflowNode) -> Self {
        self.workflow.nodes.insert(node.id.clone(), node);
        self
    }

    pub fn edge(
        mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        condition: WorkflowEdgeCondition,
    ) -> Self {
        self.workflow.edges.push(WorkflowEdge {
            from: from.into(),
            to: to.into(),
            condition,
        });
        self
    }

    pub fn build(self) -> Result<GraphWorkflow> {
        self.workflow.validate()?;
        Ok(self.workflow)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowContext {
    pub variables: BTreeMap<String, Value>,
    pub outputs: BTreeMap<WorkflowNodeId, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunSummary {
    pub workflow_name: String,
    pub status: WorkflowStatus,
    pub visited: Vec<WorkflowNodeId>,
    pub outputs: BTreeMap<WorkflowNodeId, Value>,
    pub interrupt: Option<WorkflowInterrupt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Completed,
    Failed,
    Interrupted,
    MaxStepsReached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInterrupt {
    pub node_id: WorkflowNodeId,
    pub prompt: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub enum WorkflowStepOutcome {
    Success(Value),
    Failure(String),
    Interrupt { prompt: String, reason: String },
    Stop,
}

impl WorkflowStepOutcome {
    fn condition(&self) -> WorkflowEdgeCondition {
        match self {
            Self::Success(_) | Self::Stop => WorkflowEdgeCondition::OnSuccess,
            Self::Failure(_) => WorkflowEdgeCondition::OnFailure,
            Self::Interrupt { .. } => WorkflowEdgeCondition::OnInterrupt,
        }
    }
}

pub trait WorkflowNodeRunner {
    fn run_node(
        &mut self,
        workflow: &GraphWorkflow,
        node: &WorkflowNode,
        context: &WorkflowContext,
    ) -> Result<WorkflowStepOutcome>;
}

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    max_steps: usize,
}

impl Default for WorkflowExecutor {
    fn default() -> Self {
        Self { max_steps: 128 }
    }
}

impl WorkflowExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn run(
        &self,
        workflow: &GraphWorkflow,
        runner: &mut dyn WorkflowNodeRunner,
    ) -> Result<WorkflowRunSummary> {
        self.run_from(
            workflow,
            workflow.start.clone(),
            WorkflowContext::default(),
            runner,
        )
    }

    pub fn run_from(
        &self,
        workflow: &GraphWorkflow,
        start: WorkflowNodeId,
        mut context: WorkflowContext,
        runner: &mut dyn WorkflowNodeRunner,
    ) -> Result<WorkflowRunSummary> {
        workflow.validate()?;
        let mut visited = Vec::new();
        let mut current = start;
        let max_steps = self.max_steps.min(workflow.max_steps.max(1));

        for _ in 0..max_steps {
            let node = workflow.nodes.get(&current).ok_or_else(|| {
                PwError::Message(format!(
                    "workflow '{}' current node '{}' is missing",
                    workflow.name, current
                ))
            })?;
            visited.push(node.id.clone());
            if matches!(node.kind, WorkflowNodeKind::End) {
                return Ok(WorkflowRunSummary {
                    workflow_name: workflow.name.clone(),
                    status: WorkflowStatus::Completed,
                    visited,
                    outputs: context.outputs,
                    interrupt: None,
                });
            }

            let outcome = runner.run_node(workflow, node, &context)?;
            match &outcome {
                WorkflowStepOutcome::Success(value) => {
                    context.outputs.insert(node.id.clone(), value.clone());
                }
                WorkflowStepOutcome::Failure(error) => {
                    context.outputs.insert(
                        node.id.clone(),
                        serde_json::json!({ "error": error, "ok": false }),
                    );
                }
                WorkflowStepOutcome::Interrupt { prompt, reason } => {
                    let next = next_workflow_node(workflow, &node.id, outcome.condition());
                    if let Some(next) = next {
                        current = next;
                        continue;
                    }
                    return Ok(WorkflowRunSummary {
                        workflow_name: workflow.name.clone(),
                        status: WorkflowStatus::Interrupted,
                        visited,
                        outputs: context.outputs,
                        interrupt: Some(WorkflowInterrupt {
                            node_id: node.id.clone(),
                            prompt: prompt.clone(),
                            reason: reason.clone(),
                        }),
                    });
                }
                WorkflowStepOutcome::Stop => {}
            }

            let condition = outcome.condition();
            let Some(next) = next_workflow_node(workflow, &node.id, condition) else {
                return Ok(WorkflowRunSummary {
                    workflow_name: workflow.name.clone(),
                    status: if matches!(outcome, WorkflowStepOutcome::Failure(_)) {
                        WorkflowStatus::Failed
                    } else {
                        WorkflowStatus::Completed
                    },
                    visited,
                    outputs: context.outputs,
                    interrupt: None,
                });
            };
            current = next;
        }

        Ok(WorkflowRunSummary {
            workflow_name: workflow.name.clone(),
            status: WorkflowStatus::MaxStepsReached,
            visited,
            outputs: context.outputs,
            interrupt: None,
        })
    }
}

fn next_workflow_node(
    workflow: &GraphWorkflow,
    from: &str,
    condition: WorkflowEdgeCondition,
) -> Option<WorkflowNodeId> {
    workflow
        .edges
        .iter()
        .find(|edge| {
            edge.from == from
                && (edge.condition == WorkflowEdgeCondition::Always || edge.condition == condition)
        })
        .map(|edge| edge.to.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_route_keeps_general_questions_out_of_code_agent() {
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("介绍一下你自己，以及你能帮我做什么？"),
            WorkflowPlanKind::General
        );
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("帮我总结一下今天的计划"),
            WorkflowPlanKind::General
        );
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("搜索一下最近的 AI 新闻"),
            WorkflowPlanKind::Research
        );
    }

    #[test]
    fn auto_route_uses_code_agent_for_code_work() {
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("阅读这个代码库并解释核心模块"),
            WorkflowPlanKind::Code
        );
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("fix the React component test failure"),
            WorkflowPlanKind::Code
        );
        assert_eq!(
            WorkflowPlanKind::Auto.resolve("运行测试并修复报错"),
            WorkflowPlanKind::Code
        );
    }

    #[test]
    fn non_code_workflows_do_not_contain_agent_cli_tasks() {
        for workflow in [
            GraphWorkflow::research("帮我调研生成式出价相关论文", "codex"),
            GraphWorkflow::general("帮我整理一个执行计划", "codex"),
            GraphWorkflow::ops("检查远程服务日志", "codex"),
        ] {
            assert!(workflow
                .nodes
                .values()
                .all(|node| !matches!(node.kind, WorkflowNodeKind::AgentTask { .. })));
        }
    }

    #[test]
    fn research_workflow_uses_adaptive_loop_and_cleans_query() {
        let workflow =
            GraphWorkflow::research("帮我调研一下生成式出价的相关论文，并产出调研报告", "codex");
        assert!(workflow.nodes.contains_key("adaptive_research"));
        assert!(workflow.nodes.contains_key("final_report"));
        let adaptive = workflow.nodes.get("adaptive_research").unwrap();
        let WorkflowNodeKind::AdaptiveLoop { prompt } = &adaptive.kind else {
            panic!("adaptive_research should be an adaptive loop");
        };
        assert!(
            prompt.contains("Suggested initial academic query: 生成式出价 paper arxiv academic")
        );
        assert!(prompt.contains("use the PDF/MinerU path"));
    }
}
