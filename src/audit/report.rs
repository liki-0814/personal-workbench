use crate::{policy::PolicyDecision, Result};

use super::AuditEvent;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total_events: usize,
    pub malformed_lines: usize,
    pub runs_started: usize,
    pub runs_completed: usize,
    pub runs_failed: usize,
    pub sessions_saved: usize,
    pub runtime_tasks_started: usize,
    pub runtime_tasks_completed: usize,
    pub runtime_tasks_failed: usize,
    pub runtime_task_verifications: usize,
    pub runtime_task_verification_passed: usize,
    pub runtime_task_verification_blocked: usize,
    pub runtime_task_verification_needs_review: usize,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub policy_allow: usize,
    pub policy_ask_user: usize,
    pub policy_deny: usize,
    pub input_tokens_total: u64,
    pub output_tokens_total: u64,
    pub last_provider: Option<String>,
    pub last_model: Option<String>,
    pub last_context_id: Option<String>,
    pub last_selected_tools: Vec<String>,
    pub last_user_input: Option<String>,
    pub last_session_path: Option<String>,
}

pub fn read_audit_events(path: &Path) -> Result<(Vec<AuditEvent>, usize)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let content = fs::read_to_string(path)?;
    let mut events = Vec::new();
    let mut malformed_lines = 0;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let mut parsed_any = false;
        let stream = serde_json::Deserializer::from_str(line).into_iter::<AuditEvent>();
        for item in stream {
            match item {
                Ok(event) => {
                    parsed_any = true;
                    events.push(event);
                }
                Err(_) => {
                    malformed_lines += 1;
                    break;
                }
            }
        }
        if !parsed_any {
            continue;
        }
    }
    Ok((events, malformed_lines))
}

pub fn summarize_events(events: &[AuditEvent], malformed_lines: usize) -> AuditSummary {
    let mut summary = AuditSummary {
        total_events: events.len(),
        malformed_lines,
        ..Default::default()
    };

    for event in events {
        match event {
            AuditEvent::ConfigLoaded { provider, model } => {
                summary.last_provider = Some(provider.clone());
                summary.last_model = Some(model.clone());
            }
            AuditEvent::ContextPackBuilt {
                context_id,
                selected_tool_ids,
            } => {
                summary.last_context_id = Some(context_id.clone());
                summary.last_selected_tools = selected_tool_ids.clone();
            }
            AuditEvent::GraphRunStarted { user_input, .. } => {
                summary.runs_started += 1;
                summary.last_user_input = Some(preview(user_input, 180));
            }
            AuditEvent::GraphRunCompleted => summary.runs_completed += 1,
            AuditEvent::GraphRunFailed { .. } => summary.runs_failed += 1,
            AuditEvent::TokenUsageRecorded {
                input_tokens,
                output_tokens,
            } => {
                summary.input_tokens_total += input_tokens;
                summary.output_tokens_total += output_tokens;
            }
            AuditEvent::SessionSaved { path } => {
                summary.sessions_saved += 1;
                summary.last_session_path = Some(path.clone());
            }
            AuditEvent::RuntimeTaskStarted { .. } => summary.runtime_tasks_started += 1,
            AuditEvent::RuntimeTaskCompleted { .. } => summary.runtime_tasks_completed += 1,
            AuditEvent::RuntimeTaskVerificationRecorded { gate, passed, .. } => {
                summary.runtime_task_verifications += 1;
                match gate.as_deref() {
                    Some("pass") => summary.runtime_task_verification_passed += 1,
                    Some("block") => summary.runtime_task_verification_blocked += 1,
                    Some("needs_review") => summary.runtime_task_verification_needs_review += 1,
                    _ if *passed => summary.runtime_task_verification_passed += 1,
                    _ => summary.runtime_task_verification_blocked += 1,
                }
            }
            AuditEvent::RuntimeTaskFailed { .. }
            | AuditEvent::RuntimeTaskCancelled { .. }
            | AuditEvent::RuntimeTaskTimedOut { .. } => summary.runtime_tasks_failed += 1,
            AuditEvent::ToolCallRequested { .. } => summary.tool_calls += 1,
            AuditEvent::ToolResultRecorded { is_error, .. } => {
                if *is_error {
                    summary.tool_errors += 1;
                }
            }
            AuditEvent::PolicyDecisionRecorded { decision, .. } => match decision {
                PolicyDecision::Allow => summary.policy_allow += 1,
                PolicyDecision::AskUser { .. } => summary.policy_ask_user += 1,
                PolicyDecision::Deny { .. } => summary.policy_deny += 1,
            },
            AuditEvent::RuntimeInitialized
            | AuditEvent::ToolDiscoveryStarted
            | AuditEvent::SkillsScanned { .. }
            | AuditEvent::ToolRegistryBuilt { .. }
            | AuditEvent::RegistrySnapshotCreated { .. }
            | AuditEvent::ModelNodeStarted { .. }
            | AuditEvent::ModelNodeCompleted { .. }
            | AuditEvent::ModelNodeFailed { .. }
            | AuditEvent::FinalOutputProduced
            | AuditEvent::RuntimeTaskCompactCompleted { .. }
            | AuditEvent::ToolsSelected { .. } => {}
        }
    }

    summary
}

pub fn format_audit_summary(summary: &AuditSummary) -> String {
    let mut out = String::new();
    out.push_str("Audit Summary\n");
    out.push_str(&format!("events: {}\n", summary.total_events));
    if summary.malformed_lines > 0 {
        out.push_str(&format!("malformed_lines: {}\n", summary.malformed_lines));
    }
    out.push_str(&format!(
        "runs: started={} completed={} failed={}\n",
        summary.runs_started, summary.runs_completed, summary.runs_failed
    ));
    out.push_str(&format!("sessions_saved: {}\n", summary.sessions_saved));
    out.push_str(&format!(
        "runtime_tasks: started={} completed={} failed={} verifications={}\n",
        summary.runtime_tasks_started,
        summary.runtime_tasks_completed,
        summary.runtime_tasks_failed,
        summary.runtime_task_verifications
    ));
    out.push_str(&format!(
        "verification_gates: pass={} block={} needs_review={}\n",
        summary.runtime_task_verification_passed,
        summary.runtime_task_verification_blocked,
        summary.runtime_task_verification_needs_review
    ));
    out.push_str(&format!(
        "tokens: input={} output={} total={}\n",
        summary.input_tokens_total,
        summary.output_tokens_total,
        summary.input_tokens_total + summary.output_tokens_total
    ));
    out.push_str(&format!(
        "tools: calls={} errors={}\n",
        summary.tool_calls, summary.tool_errors
    ));
    out.push_str(&format!(
        "policy: allow={} ask_user={} deny={}\n",
        summary.policy_allow, summary.policy_ask_user, summary.policy_deny
    ));
    if let (Some(provider), Some(model)) = (&summary.last_provider, &summary.last_model) {
        out.push_str(&format!("last_model: {provider}/{model}\n"));
    }
    if let Some(context_id) = &summary.last_context_id {
        out.push_str(&format!("last_context: {context_id}\n"));
    }
    if !summary.last_selected_tools.is_empty() {
        out.push_str("last_selected_tools:\n");
        for tool_id in &summary.last_selected_tools {
            out.push_str(&format!("- {tool_id}\n"));
        }
    }
    if let Some(user_input) = &summary.last_user_input {
        out.push_str(&format!("last_user_input: {user_input}\n"));
    }
    if let Some(path) = &summary.last_session_path {
        out.push_str(&format!("last_session: {path}\n"));
    }
    out
}

pub fn format_audit_tail(events: &[AuditEvent], limit: usize) -> String {
    let mut out = String::new();
    out.push_str("Audit Tail\n");
    let start = events.len().saturating_sub(limit);
    for (idx, event) in events.iter().enumerate().skip(start) {
        out.push_str(&format!("{idx}\t{}\n", format_event_line(event)));
    }
    if events.is_empty() {
        out.push_str("no audit events\n");
    }
    out
}

fn format_event_line(event: &AuditEvent) -> String {
    match event {
        AuditEvent::RuntimeInitialized => "runtime initialized".to_string(),
        AuditEvent::ConfigLoaded { provider, model } => {
            format!("config provider={provider} model={model}")
        }
        AuditEvent::ToolDiscoveryStarted => "tool discovery started".to_string(),
        AuditEvent::SkillsScanned { loaded, .. } => format!("skills scanned loaded={loaded}"),
        AuditEvent::ToolRegistryBuilt {
            registry_version,
            tool_count,
        } => format!("registry built version={registry_version} tools={tool_count}"),
        AuditEvent::ContextPackBuilt {
            context_id,
            selected_tool_ids,
        } => format!(
            "context {context_id} selected_tools={}",
            selected_tool_ids.len()
        ),
        AuditEvent::RegistrySnapshotCreated {
            registry_version,
            tool_count,
        } => format!("snapshot version={registry_version} tools={tool_count}"),
        AuditEvent::ModelNodeStarted { provider, model } => {
            format!("model started {provider}/{model}")
        }
        AuditEvent::ModelNodeCompleted { output_chars } => {
            format!("model completed output_chars={output_chars}")
        }
        AuditEvent::TokenUsageRecorded {
            input_tokens,
            output_tokens,
        } => format!("tokens input={input_tokens} output={output_tokens}"),
        AuditEvent::ModelNodeFailed { error } => format!("model failed {}", preview(error, 120)),
        AuditEvent::GraphRunFailed { error } => format!("graph failed {}", preview(error, 120)),
        AuditEvent::FinalOutputProduced => "final output produced".to_string(),
        AuditEvent::SessionSaved { path } => format!("session saved {path}"),
        AuditEvent::GraphRunStarted { user_input, .. } => {
            format!("graph started input={}", preview(user_input, 120))
        }
        AuditEvent::ToolsSelected { tool_ids } => format!("tools selected {}", tool_ids.join(",")),
        AuditEvent::ToolCallRequested {
            call_id,
            tool_id,
            name,
        } => format!("tool requested call={call_id} id={tool_id} name={name}"),
        AuditEvent::PolicyDecisionRecorded { call_id, decision } => {
            format!("policy call={call_id} decision={decision:?}")
        }
        AuditEvent::ToolResultRecorded {
            call_id, is_error, ..
        } => {
            format!("tool result call={call_id} error={is_error}")
        }
        AuditEvent::RuntimeTaskStarted {
            task_id,
            kind,
            title,
        } => format!(
            "runtime task started {task_id} kind={kind} title={}",
            preview(title, 120)
        ),
        AuditEvent::RuntimeTaskCompleted {
            task_id,
            review_required,
        } => format!("runtime task completed {task_id} review_required={review_required:?}"),
        AuditEvent::RuntimeTaskFailed { task_id, error } => {
            format!("runtime task failed {task_id} {}", preview(error, 120))
        }
        AuditEvent::RuntimeTaskCancelled { task_id } => {
            format!("runtime task cancelled {task_id}")
        }
        AuditEvent::RuntimeTaskTimedOut { task_id } => format!("runtime task timed out {task_id}"),
        AuditEvent::RuntimeTaskCompactCompleted {
            task_id,
            summary_path,
        } => format!("runtime task compacted {task_id} {summary_path}"),
        AuditEvent::RuntimeTaskVerificationRecorded {
            task_id,
            passed,
            verification_path,
            gate,
            status,
            failed_check_count,
            report_path,
        } => format!(
            "runtime task verification {task_id} passed={passed} status={} gate={} failed_checks={} {}",
            status.as_deref().unwrap_or(if *passed { "passed" } else { "failed" }),
            gate.as_deref().unwrap_or(if *passed { "pass" } else { "block" }),
            failed_check_count,
            report_path.as_deref().unwrap_or(verification_path)
        ),
        AuditEvent::GraphRunCompleted => "graph completed".to_string(),
    }
}

fn preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyDecision;

    #[test]
    fn summarizes_audit_events() {
        let events = vec![
            AuditEvent::ConfigLoaded {
                provider: "antigravity".to_string(),
                model: "gemini".to_string(),
            },
            AuditEvent::ContextPackBuilt {
                context_id: "ctx".to_string(),
                selected_tool_ids: vec!["verification.project_check".to_string()],
            },
            AuditEvent::GraphRunStarted {
                registry_version: 1,
                user_input: "run tests".to_string(),
            },
            AuditEvent::PolicyDecisionRecorded {
                call_id: "call".to_string(),
                decision: PolicyDecision::AskUser {
                    prompt: "Allow?".to_string(),
                },
            },
            AuditEvent::ToolCallRequested {
                call_id: "call".to_string(),
                tool_id: "verification.project_check".to_string(),
                name: "project_check".to_string(),
            },
            AuditEvent::ToolResultRecorded {
                call_id: "call".to_string(),
                is_error: false,
                metadata: serde_json::json!({}),
            },
            AuditEvent::TokenUsageRecorded {
                input_tokens: 10,
                output_tokens: 3,
            },
            AuditEvent::GraphRunCompleted,
            AuditEvent::SessionSaved {
                path: "/tmp/session.json".to_string(),
            },
            AuditEvent::RuntimeTaskStarted {
                task_id: "task_1".to_string(),
                kind: "AgentCli".to_string(),
                title: "run codex".to_string(),
            },
            AuditEvent::RuntimeTaskCompleted {
                task_id: "task_1".to_string(),
                review_required: Some(true),
            },
            AuditEvent::RuntimeTaskVerificationRecorded {
                task_id: "task_1".to_string(),
                passed: true,
                verification_path: "/tmp/verification.md".to_string(),
                status: Some("passed".to_string()),
                gate: Some("pass".to_string()),
                failed_check_count: 0,
                report_path: Some("/tmp/verification_report.md".to_string()),
            },
        ];

        let summary = summarize_events(&events, 0);
        assert_eq!(summary.total_events, events.len());
        assert_eq!(summary.policy_ask_user, 1);
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.runtime_tasks_started, 1);
        assert_eq!(summary.runtime_tasks_completed, 1);
        assert_eq!(summary.runtime_task_verifications, 1);
        assert_eq!(summary.runtime_task_verification_passed, 1);
        assert_eq!(summary.input_tokens_total, 10);
        assert_eq!(
            summary.last_selected_tools,
            vec!["verification.project_check"]
        );
        assert!(format_audit_summary(&summary).contains("verifications=1"));
        assert!(format_audit_tail(&events, 20).contains("runtime task verification task_1"));
    }

    #[test]
    fn parses_multiple_json_values_on_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        fs::write(&path, "\"RuntimeInitialized\"\"GraphRunCompleted\"\n").unwrap();

        let (events, malformed) = read_audit_events(&path).unwrap();
        assert_eq!(malformed, 0);
        assert_eq!(events.len(), 2);
    }
}
