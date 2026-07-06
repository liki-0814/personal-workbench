use crate::{
    audit::AuditRecorder,
    graph::{GraphExecutor, GraphPlanner, GraphRunRequest, GraphRunSummary},
    policy::{PolicyGuard, UserApproval},
    tools::ToolRegistrySnapshot,
    Result,
};

pub struct SubAgentNode<'a> {
    pub name: String,
    pub executor: GraphExecutor,
    pub planner: Box<dyn GraphPlanner + 'a>,
}

impl<'a> SubAgentNode<'a> {
    pub fn new(
        name: impl Into<String>,
        executor: GraphExecutor,
        planner: Box<dyn GraphPlanner + 'a>,
    ) -> Self {
        Self {
            name: name.into(),
            executor,
            planner,
        }
    }

    pub fn run(
        &mut self,
        request: GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
        policy: &dyn PolicyGuard,
        audit: &dyn AuditRecorder,
        approval: Option<&dyn UserApproval>,
    ) -> Result<GraphRunSummary> {
        self.executor.run_with_planner(
            request,
            snapshot,
            self.planner.as_mut(),
            policy,
            audit,
            approval,
        )
    }
}
