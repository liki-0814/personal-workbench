use crate::{
    policy::{PolicyDecision, PolicyGuard, UserApproval},
    tools::{ToolCall, ToolRegistrySnapshot, ToolResult},
    PwError, Result,
};

pub struct ToolNode<'a> {
    snapshot: &'a ToolRegistrySnapshot,
    policy: &'a dyn PolicyGuard,
    approval: Option<&'a dyn UserApproval>,
}

impl<'a> ToolNode<'a> {
    pub fn new(
        snapshot: &'a ToolRegistrySnapshot,
        policy: &'a dyn PolicyGuard,
        approval: Option<&'a dyn UserApproval>,
    ) -> Self {
        Self {
            snapshot,
            policy,
            approval,
        }
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let descriptor = self
            .snapshot
            .get(&call.tool_id)
            .ok_or_else(|| PwError::ToolNotFound(call.tool_id.clone()))?
            .descriptor
            .clone();

        match self.policy.check(&descriptor, call) {
            PolicyDecision::Allow => self.snapshot.execute(call),
            PolicyDecision::Deny { reason } => Ok(ToolResult::error(reason)),
            PolicyDecision::AskUser { prompt } => {
                if self
                    .approval
                    .map(|approval| approval.ask_user(&prompt, call))
                    .unwrap_or(false)
                {
                    self.snapshot.execute(call)
                } else {
                    Ok(ToolResult::error("user rejected tool call"))
                }
            }
        }
    }
}
