use crate::{
    graph::{GraphPlanner, GraphRunRequest, GraphState, GraphStep},
    tools::ToolRegistrySnapshot,
    Result,
};

pub struct ModelNode<'a> {
    planner: &'a mut dyn GraphPlanner,
}

impl<'a> ModelNode<'a> {
    pub fn new(planner: &'a mut dyn GraphPlanner) -> Self {
        Self { planner }
    }

    pub fn execute(
        &mut self,
        state: &GraphState,
        request: &GraphRunRequest,
        snapshot: &ToolRegistrySnapshot,
    ) -> Result<GraphStep> {
        self.planner.next_step(state, request, snapshot)
    }
}
