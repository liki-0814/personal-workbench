use crate::{
    audit::token::TokenUsage,
    tools::{ToolCall, ToolResult},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphMessage {
    User(String),
    Assistant(String),
    AssistantToolCalls {
        calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        #[serde(default)]
        name: String,
        content: String,
        is_error: bool,
    },
    System(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphStatus {
    Running,
    Completed,
    Cancelled,
    Interrupted,
    MaxRoundsReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphInterruptKind {
    Clarification,
    ToolApproval,
    RuntimeTask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphInterrupt {
    pub id: String,
    pub kind: GraphInterruptKind,
    pub prompt: String,
    pub reason: String,
    pub tool_call: Option<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphState {
    pub round_count: u32,
    pub token_usage: TokenUsage,
    pub messages: Vec<GraphMessage>,
    pub pending_tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolResult>,
    pub last_content: String,
    pub status: GraphStatus,
    pub interrupt: Option<GraphInterrupt>,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            round_count: 0,
            token_usage: TokenUsage::default(),
            messages: Vec::new(),
            pending_tool_calls: Vec::new(),
            tool_results: Vec::new(),
            last_content: String::new(),
            status: GraphStatus::Running,
            interrupt: None,
        }
    }
}
