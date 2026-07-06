use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSource {
    Builtin,
    AgentCli { cli: String },
    Skill { path: PathBuf },
    Mcp { server: String },
    Verification,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationMode {
    Prompt,
    ExecutableJson,
    AgentCli,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    ReadOnly,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub source: ToolSource,
    pub risk_level: RiskLevel,
    pub invocation_mode: InvocationMode,
    pub capabilities: Vec<String>,
    pub metadata: Value,
    pub enabled: bool,
}

impl ToolDescriptor {
    pub fn prompt_skill(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        path: PathBuf,
        metadata: Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" }
                },
                "required": ["prompt"]
            }),
            source: ToolSource::Skill { path },
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Prompt,
            capabilities: vec!["skill.prompt".to_string()],
            metadata,
            enabled: true,
        }
    }
}
