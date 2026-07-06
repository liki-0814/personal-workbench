use crate::{PwError, Result};
use std::{collections::BTreeMap, sync::Arc};

use super::{
    LoadedTool, ToolCall, ToolDescriptor, ToolExecutionMode, ToolExecutionRuntime, ToolResult,
    ToolRuntimeEvent,
};

pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult>;

    fn execute_with_runtime(
        &self,
        call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> Result<ToolResult> {
        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Sync,
        });
        let result = self.execute(call);
        if let Ok(result) = &result {
            runtime.emit(ToolRuntimeEvent::Completed {
                is_error: result.is_error,
            });
        }
        result
    }
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub descriptor: ToolDescriptor,
    pub executor: Option<Arc<dyn ToolExecutor>>,
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
    version: u64,
}

#[derive(Clone)]
pub struct ToolRegistrySnapshot {
    version: u64,
    tools: BTreeMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn register(&mut self, tool: LoadedTool) {
        self.tools.insert(
            tool.descriptor.id.clone(),
            RegisteredTool {
                descriptor: tool.descriptor,
                executor: tool.executor,
            },
        );
        self.version += 1;
    }

    pub fn register_many(&mut self, tools: impl IntoIterator<Item = LoadedTool>) {
        for tool in tools {
            self.register(tool);
        }
    }

    pub fn unregister(&mut self, id: &str) -> Option<RegisteredTool> {
        let removed = self.tools.remove(id);
        if removed.is_some() {
            self.version += 1;
        }
        removed
    }

    pub fn snapshot(&self) -> ToolRegistrySnapshot {
        ToolRegistrySnapshot {
            version: self.version,
            tools: self.tools.clone(),
        }
    }
}

impl ToolRegistrySnapshot {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .map(|tool| tool.descriptor.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&RegisteredTool> {
        self.tools.get(id)
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut runtime = ToolExecutionRuntime::noop();
        self.execute_with_runtime(call, &mut runtime)
    }

    pub fn execute_with_runtime(
        &self,
        call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(&call.tool_id)
            .ok_or_else(|| PwError::ToolNotFound(call.tool_id.clone()))?;

        let executor = tool.executor.as_ref().ok_or_else(|| {
            PwError::ToolExecution(format!("tool {} is not executable", call.tool_id))
        })?;

        executor.execute_with_runtime(call, runtime)
    }
}
