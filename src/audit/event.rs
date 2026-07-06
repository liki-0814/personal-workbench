use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::PolicyDecision;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditEvent {
    RuntimeInitialized,
    ConfigLoaded {
        provider: String,
        model: String,
    },
    ToolDiscoveryStarted,
    SkillsScanned {
        roots: Vec<String>,
        loaded: usize,
    },
    ToolRegistryBuilt {
        registry_version: u64,
        tool_count: usize,
    },
    ContextPackBuilt {
        context_id: String,
        selected_tool_ids: Vec<String>,
    },
    RegistrySnapshotCreated {
        registry_version: u64,
        tool_count: usize,
    },
    ModelNodeStarted {
        provider: String,
        model: String,
    },
    ModelNodeCompleted {
        output_chars: usize,
    },
    TokenUsageRecorded {
        input_tokens: u64,
        output_tokens: u64,
    },
    ModelNodeFailed {
        error: String,
    },
    GraphRunFailed {
        error: String,
    },
    FinalOutputProduced,
    SessionSaved {
        path: String,
    },
    GraphRunStarted {
        registry_version: u64,
        user_input: String,
    },
    ToolsSelected {
        tool_ids: Vec<String>,
    },
    ToolCallRequested {
        call_id: String,
        tool_id: String,
        name: String,
    },
    PolicyDecisionRecorded {
        call_id: String,
        decision: PolicyDecision,
    },
    ToolResultRecorded {
        call_id: String,
        is_error: bool,
        metadata: Value,
    },
    RuntimeTaskStarted {
        task_id: String,
        kind: String,
        title: String,
    },
    RuntimeTaskCompleted {
        task_id: String,
        review_required: Option<bool>,
    },
    RuntimeTaskFailed {
        task_id: String,
        error: String,
    },
    RuntimeTaskCancelled {
        task_id: String,
    },
    RuntimeTaskTimedOut {
        task_id: String,
    },
    RuntimeTaskCompactCompleted {
        task_id: String,
        summary_path: String,
    },
    RuntimeTaskVerificationRecorded {
        task_id: String,
        passed: bool,
        verification_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate: Option<String>,
        #[serde(default)]
        failed_check_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_path: Option<String>,
    },
    GraphRunCompleted,
}
