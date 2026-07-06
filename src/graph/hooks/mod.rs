use crate::{
    tools::{ToolCall, ToolResult},
    Result,
};

use super::{
    executor::{GraphRunRequest, GraphStep},
    state::GraphState,
};

pub trait GraphHook: Send + Sync {
    fn name(&self) -> &str;

    fn before_run(&self, _state: &mut GraphState, _request: &GraphRunRequest) -> Result<()> {
        Ok(())
    }

    fn after_step(&self, _state: &mut GraphState, _step: &GraphStep) -> Result<()> {
        Ok(())
    }

    fn before_tool(&self, _state: &mut GraphState, _call: &ToolCall) -> Result<()> {
        Ok(())
    }

    fn after_tool(
        &self,
        _state: &mut GraphState,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<()> {
        Ok(())
    }

    fn after_run(&self, _state: &mut GraphState) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct NoopHook {
    name: String,
}

impl NoopHook {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl GraphHook for NoopHook {
    fn name(&self) -> &str {
        &self.name
    }
}
