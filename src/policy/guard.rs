use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::tools::{InvocationMode, RiskLevel, ToolCall, ToolDescriptor, ToolSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    AskUser { prompt: String },
}

pub trait PolicyGuard: Send + Sync {
    fn check(&self, descriptor: &ToolDescriptor, call: &ToolCall) -> PolicyDecision;
}

#[derive(Default)]
pub struct AllowAllPolicy;

impl PolicyGuard for AllowAllPolicy {
    fn check(&self, _descriptor: &ToolDescriptor, _call: &ToolCall) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

#[derive(Debug, Clone)]
pub struct DefaultPolicyGuard {
    pub yolo: bool,
    pub protected_paths: Vec<PathBuf>,
    pub ask_above: RiskLevel,
    pub rules: Vec<String>,
}

impl Default for DefaultPolicyGuard {
    fn default() -> Self {
        Self {
            yolo: false,
            protected_paths: vec![
                PathBuf::from("/"),
                PathBuf::from("/System"),
                PathBuf::from("/etc"),
            ],
            ask_above: RiskLevel::Medium,
            rules: Vec::new(),
        }
    }
}

impl DefaultPolicyGuard {
    pub fn with_rules(mut self, rules: Vec<String>) -> Self {
        self.rules = rules;
        self
    }
}

impl PolicyGuard for DefaultPolicyGuard {
    fn check(&self, descriptor: &ToolDescriptor, call: &ToolCall) -> PolicyDecision {
        if secret_like_arguments(&call.arguments)
            && (source_can_exfiltrate(&descriptor.source) || remote_execution_tool(descriptor))
        {
            return PolicyDecision::Deny {
                reason: format!(
                    "tool {} may receive secret-like arguments through an external source",
                    descriptor.id
                ),
            };
        }

        if network_policy_denies(descriptor) {
            return PolicyDecision::Deny {
                reason: format!("tool {} is blocked by network policy", descriptor.id),
            };
        }

        match descriptor
            .metadata
            .pointer("/tool_config/approval")
            .and_then(serde_json::Value::as_str)
        {
            Some("deny") => {
                return PolicyDecision::Deny {
                    reason: format!("tool {} is denied by tool config", descriptor.id),
                }
            }
            Some("never") => return PolicyDecision::Allow,
            Some("always") => {
                return PolicyDecision::AskUser {
                    prompt: format!(
                        "Tool config requires approval for {}. Allow?",
                        descriptor.name
                    ),
                }
            }
            _ => {}
        }

        if touches_protected_path(call, &self.protected_paths) {
            return PolicyDecision::Deny {
                reason: "tool call touches a protected path".to_string(),
            };
        }

        if destructive_action(call) {
            return PolicyDecision::AskUser {
                prompt: format!(
                    "Destructive action requested by {}. This may delete or overwrite data. Confirm?",
                    descriptor.name
                ),
            };
        }

        if rule_requires_delete_confirmation(&self.rules) && is_delete_like_call(call) {
            return PolicyDecision::AskUser {
                prompt: "A configured rule requires confirmation before deleting files. Allow this delete-like action?".to_string(),
            };
        }

        if let Some(decision) = source_specific_decision(descriptor, call) {
            return decision;
        }

        if self.yolo {
            return PolicyDecision::Allow;
        }

        if descriptor.risk_level >= self.ask_above {
            return PolicyDecision::AskUser {
                prompt: format!(
                    "Allow {} to run with {:?} risk?",
                    descriptor.name, descriptor.risk_level
                ),
            };
        }

        PolicyDecision::Allow
    }
}

fn source_specific_decision(
    descriptor: &ToolDescriptor,
    call: &ToolCall,
) -> Option<PolicyDecision> {
    match &descriptor.source {
        ToolSource::Skill { .. }
            if descriptor.invocation_mode == InvocationMode::ExecutableJson =>
        {
            Some(PolicyDecision::AskUser {
                prompt: format!(
                    "Executable skill {} wants to run. Confirm external skill execution?",
                    descriptor.name
                ),
            })
        }
        ToolSource::Skill { .. } if shell_like_arguments(&call.arguments) => {
            Some(PolicyDecision::AskUser {
                prompt: format!(
                    "Skill {} includes shell-like arguments. Confirm execution?",
                    descriptor.name
                ),
            })
        }
        ToolSource::Mcp { server } if touches_any_local_path(call) => {
            Some(PolicyDecision::AskUser {
                prompt: format!(
                    "MCP server {server} may access local paths. Confirm this tool call?"
                ),
            })
        }
        ToolSource::AgentCli { cli } if agent_cli_yolo_requested(call) => {
            Some(PolicyDecision::AskUser {
                prompt: format!(
                    "Agent CLI {cli} requested YOLO/dangerous permission mode. Confirm?"
                ),
            })
        }
        ToolSource::AgentCli { cli } => Some(PolicyDecision::AskUser {
            prompt: format!("Start local code agent {cli} for this task?"),
        }),
        ToolSource::Builtin
            if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "ssh" || cap == "remote_exec" || cap == "remote_shell") =>
        {
            Some(PolicyDecision::AskUser {
                prompt: format!(
                    "Run remote SSH command through {}? Remote execution is outside the local sandbox.",
                    descriptor.name
                ),
            })
        }
        _ => None,
    }
}

fn network_policy_denies(descriptor: &ToolDescriptor) -> bool {
    let Some(policy) = descriptor
        .metadata
        .pointer("/tool_config/network_policy")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    match policy {
        "deny" => network_capable(descriptor),
        "local_only" => network_capable(descriptor) && !local_only_tool(descriptor),
        _ => false,
    }
}

fn network_capable(descriptor: &ToolDescriptor) -> bool {
    match descriptor.source {
        ToolSource::Mcp { .. } | ToolSource::Model => true,
        ToolSource::Builtin => descriptor.capabilities.iter().any(|cap| {
            matches!(
                cap.as_str(),
                "web_search"
                    | "anysearch"
                    | "web_fetch"
                    | "url_read"
                    | "github"
                    | "code_hosting"
                    | "browser"
                    | "browser_automation"
                    | "ssh"
                    | "remote_exec"
                    | "remote_shell"
                    | "pdf"
                    | "document_parse"
                    | "ocr"
            )
        }),
        ToolSource::Skill { .. } | ToolSource::AgentCli { .. } => {
            descriptor.invocation_mode != InvocationMode::Prompt
        }
        ToolSource::Verification => false,
    }
}

fn remote_execution_tool(descriptor: &ToolDescriptor) -> bool {
    matches!(descriptor.source, ToolSource::Builtin)
        && descriptor
            .capabilities
            .iter()
            .any(|cap| cap == "ssh" || cap == "remote_exec" || cap == "remote_shell")
}

fn local_only_tool(descriptor: &ToolDescriptor) -> bool {
    matches!(descriptor.source, ToolSource::Verification)
        || descriptor
            .capabilities
            .iter()
            .any(|cap| cap == "local" || cap == "filesystem")
}

fn touches_protected_path(call: &ToolCall, protected_paths: &[PathBuf]) -> bool {
    let mut candidates = Vec::new();
    collect_path_candidates(&call.arguments, None, &mut candidates);
    candidates.into_iter().any(|candidate| {
        protected_paths
            .iter()
            .any(|protected| path_matches_protected(&candidate, protected))
    })
}

fn touches_any_local_path(call: &ToolCall) -> bool {
    let mut candidates = Vec::new();
    collect_path_candidates(&call.arguments, None, &mut candidates);
    !candidates.is_empty()
}

fn collect_path_candidates(
    value: &serde_json::Value,
    key: Option<&str>,
    candidates: &mut Vec<PathBuf>,
) {
    match value {
        serde_json::Value::String(text) => {
            if key.is_some_and(is_path_key) && looks_like_path(text) {
                candidates.push(PathBuf::from(text));
            }
            if key.is_some_and(is_command_key) {
                candidates.extend(command_path_tokens(text).map(PathBuf::from));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_candidates(item, key, candidates);
            }
        }
        serde_json::Value::Object(map) => {
            for (nested_key, nested_value) in map {
                collect_path_candidates(nested_value, Some(nested_key.as_str()), candidates);
            }
        }
        _ => {}
    }
}

fn is_path_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "path"
            | "file"
            | "dir"
            | "directory"
            | "cwd"
            | "root"
            | "target"
            | "source"
            | "destination"
            | "dest"
            | "from"
            | "to"
            | "input"
            | "output"
    )
}

fn is_command_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "command" | "cmd" | "script" | "shell"
    )
}

fn secret_like_arguments(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => secret_like_text(text),
        serde_json::Value::Array(items) => items.iter().any(secret_like_arguments),
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(key, value)| secret_like_key(key) || secret_like_arguments(value)),
        _ => false,
    }
}

fn secret_like_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "api_key_env"
            | "token_env"
            | "password_env"
            | "key_passphrase_env"
            | "private_key_path"
            | "known_hosts_path"
    ) || lower.ends_with("_env")
    {
        return false;
    }
    [
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn secret_like_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("api_key=")
        || lower.contains("authorization:")
        || text
            .split(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'')
            .any(|token| {
                let token =
                    token.trim_matches(|ch: char| matches!(ch, ',' | ';' | ':' | ')' | ']' | '}'));
                token.starts_with("sk-")
                    || token.starts_with("nvapi-")
                    || (token.len() >= 32
                        && token
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
            })
}

fn source_can_exfiltrate(source: &ToolSource) -> bool {
    matches!(
        source,
        ToolSource::Skill { .. }
            | ToolSource::Mcp { .. }
            | ToolSource::AgentCli { .. }
            | ToolSource::Model
    )
}

fn shell_like_arguments(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("bash")
                || lower.contains("sh -c")
                || lower.contains("rm ")
                || lower.contains("curl ")
                || lower.contains("python ")
                || lower.contains("node ")
        }
        serde_json::Value::Array(items) => items.iter().any(shell_like_arguments),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            is_command_key(key) || key.eq_ignore_ascii_case("shell") || shell_like_arguments(value)
        }),
        _ => false,
    }
}

fn agent_cli_yolo_requested(call: &ToolCall) -> bool {
    let text = call.arguments.to_string().to_ascii_lowercase();
    text.contains("yolo")
        || text.contains("dangerously")
        || text.contains("bypass")
        || text.contains("danger-full-access")
        || text.contains("skip-permissions")
}

fn command_path_tokens(command: &str) -> impl Iterator<Item = String> + '_ {
    command
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
                )
            })
        })
        .filter(|token| looks_like_path(token))
        .map(ToString::to_string)
}

fn looks_like_path(value: &str) -> bool {
    value == "/" || value.starts_with('/') || value.starts_with("~/") || value.starts_with("./")
}

fn path_matches_protected(path: &Path, protected: &Path) -> bool {
    if protected == Path::new("/") {
        return path == protected;
    }
    path.starts_with(protected)
}

fn rule_requires_delete_confirmation(rules: &[String]) -> bool {
    rules.iter().any(|rule| {
        let lower = rule.to_lowercase();
        (lower.contains("delete")
            || lower.contains("remove")
            || lower.contains("rm ")
            || rule.contains("删除")
            || rule.contains("移除"))
            && (lower.contains("ask")
                || lower.contains("confirm")
                || lower.contains("confirmation")
                || rule.contains("询问")
                || rule.contains("确认"))
    })
}

fn is_delete_like_call(call: &ToolCall) -> bool {
    let name = call.name.to_lowercase();
    let tool_id = call.tool_id.to_lowercase();
    if [name.as_str(), tool_id.as_str()]
        .iter()
        .any(|value| value.contains("delete") || value.contains("remove") || value.contains("rm"))
    {
        return true;
    }

    let text = call.arguments.to_string().to_lowercase();
    text.contains("rm ")
        || text.contains("rm -")
        || text.contains("delete")
        || text.contains("remove")
        || text.contains("unlink")
        || text.contains("trash")
}

fn destructive_action(call: &ToolCall) -> bool {
    if is_delete_like_call(call) {
        return true;
    }
    let text = call.arguments.to_string().to_ascii_lowercase();
    text.contains("overwrite")
        || text.contains("truncate")
        || text.contains("drop table")
        || text.contains("delete from")
        || text.contains("git reset --hard")
}
