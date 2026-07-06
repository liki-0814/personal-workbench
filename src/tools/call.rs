use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub tool_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactKind {
    #[default]
    File,
    Report,
    Image,
    Dataset,
    Diff,
    Verification,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArtifactProvenance {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArtifact {
    pub path: PathBuf,
    #[serde(default)]
    pub kind: ToolArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_content_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ToolArtifactProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_content_ref: Option<String>,
    pub metadata: Value,
    pub artifacts: Vec<ToolArtifact>,
    pub audit_hints: Value,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            preview: None,
            full_content_ref: None,
            metadata: Value::Object(Default::default()),
            artifacts: Vec::new(),
            audit_hints: Value::Object(Default::default()),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            preview: None,
            full_content_ref: None,
            metadata: Value::Object(Default::default()),
            artifacts: Vec::new(),
            audit_hints: Value::Object(Default::default()),
        }
    }

    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub fn with_full_content_ref(mut self, full_content_ref: impl Into<String>) -> Self {
        self.full_content_ref = Some(full_content_ref.into());
        self
    }

    pub fn add_artifact(mut self, artifact: ToolArtifact) -> Self {
        self.artifacts.push(artifact);
        self
    }
}
