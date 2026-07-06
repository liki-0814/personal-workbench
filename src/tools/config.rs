use crate::settings::{ToolApprovalMode, ToolNetworkPolicy, ToolSettings};
use serde_json::{json, Value};

use super::{LoadedTool, RiskLevel};

pub fn apply_tool_settings(
    tools: impl IntoIterator<Item = LoadedTool>,
    settings: &ToolSettings,
) -> Vec<LoadedTool> {
    tools
        .into_iter()
        .filter_map(|mut tool| {
            if !settings.allowlist.is_empty()
                && !matches_any(&tool.descriptor.id, &settings.allowlist)
            {
                return None;
            }
            if matches_any(&tool.descriptor.id, &settings.denylist)
                || matches_any(&tool.descriptor.id, &settings.disabled)
            {
                return None;
            }

            if let Some(risk) = matching_value(&tool.descriptor.id, &settings.risk_overrides)
                .and_then(parse_risk_level)
            {
                tool.descriptor.risk_level = risk;
            }

            let config_metadata = tool_config_metadata(&tool.descriptor.id, settings);
            if config_metadata != Value::Null {
                merge_metadata_object(
                    &mut tool.descriptor.metadata,
                    "tool_config",
                    config_metadata,
                );
            }
            Some(tool)
        })
        .collect()
}

pub fn matches_any(value: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern_matches(pattern, value))
}

pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if pattern == "*" || pattern == value {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    false
}

fn matching_value<'a>(
    tool_id: &str,
    values: &'a std::collections::BTreeMap<String, String>,
) -> Option<&'a str> {
    values
        .iter()
        .find(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(_, value)| value.as_str())
}

fn parse_risk_level(value: &str) -> Option<RiskLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "read_only" | "read-only" | "readonly" => Some(RiskLevel::ReadOnly),
        "low" => Some(RiskLevel::Low),
        "medium" => Some(RiskLevel::Medium),
        "high" => Some(RiskLevel::High),
        _ => None,
    }
}

fn tool_config_metadata(tool_id: &str, settings: &ToolSettings) -> Value {
    let timeout = settings
        .timeout_seconds
        .iter()
        .find(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(_, timeout)| *timeout)
        .or(settings.default_timeout_seconds);
    let rate_limit = settings
        .rate_limits
        .iter()
        .find(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(_, rate)| json!(rate));
    let retry = settings
        .retry
        .iter()
        .find(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(_, retry)| json!(retry));
    let approval = settings
        .approval_overrides
        .iter()
        .find(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(_, approval)| approval_label(*approval));
    let secret_refs = settings
        .secrets
        .iter()
        .filter(|(pattern, _)| pattern_matches(pattern, tool_id))
        .map(|(pattern, secret)| {
            json!({
                "pattern": pattern,
                "env": secret.env,
                "config_key": secret.config_key,
                "configured": secret.env.as_deref().is_some_and(|env| {
                    std::env::var(env).ok().is_some_and(|value| !value.trim().is_empty())
                }) || secret.config_key.is_some()
            })
        })
        .collect::<Vec<_>>();

    let mut object = serde_json::Map::new();
    if let Some(timeout) = timeout {
        object.insert("timeout_seconds".to_string(), json!(timeout));
    }
    if let Some(rate_limit) = rate_limit {
        object.insert("rate_limit".to_string(), rate_limit);
    }
    if let Some(retry) = retry {
        object.insert("retry".to_string(), retry);
    }
    if let Some(approval) = approval {
        object.insert("approval".to_string(), json!(approval));
    }
    if !matches!(settings.network_policy, ToolNetworkPolicy::Allow) {
        object.insert(
            "network_policy".to_string(),
            json!(match settings.network_policy {
                ToolNetworkPolicy::Allow => "allow",
                ToolNetworkPolicy::Deny => "deny",
                ToolNetworkPolicy::LocalOnly => "local_only",
            }),
        );
    }
    if !secret_refs.is_empty() {
        object.insert("secret_refs".to_string(), Value::Array(secret_refs));
    }

    if object.is_empty() {
        Value::Null
    } else {
        Value::Object(object)
    }
}

fn approval_label(mode: ToolApprovalMode) -> &'static str {
    match mode {
        ToolApprovalMode::Policy => "policy",
        ToolApprovalMode::Always => "always",
        ToolApprovalMode::Never => "never",
        ToolApprovalMode::Deny => "deny",
    }
}

fn merge_metadata_object(metadata: &mut Value, key: &str, value: Value) {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{InvocationMode, LoadedTool, ToolDescriptor, ToolSource};
    use serde_json::json;

    fn tool(id: &str) -> LoadedTool {
        LoadedTool {
            descriptor: ToolDescriptor {
                id: id.to_string(),
                name: id.to_string(),
                description: "test".to_string(),
                input_schema: json!({ "type": "object" }),
                source: ToolSource::Builtin,
                risk_level: RiskLevel::Low,
                invocation_mode: InvocationMode::Internal,
                capabilities: vec![],
                metadata: json!({}),
                enabled: true,
            },
            executor: None,
        }
    }

    #[test]
    fn applies_allow_deny_and_risk_overrides() {
        let mut settings = ToolSettings {
            allowlist: vec!["builtin.*".to_string()],
            denylist: vec!["builtin.blocked".to_string()],
            ..ToolSettings::default()
        };
        settings
            .risk_overrides
            .insert("builtin.*".to_string(), "high".to_string());
        let tools = apply_tool_settings(
            vec![
                tool("builtin.ok"),
                tool("builtin.blocked"),
                tool("skill.other"),
            ],
            &settings,
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].descriptor.id, "builtin.ok");
        assert_eq!(tools[0].descriptor.risk_level, RiskLevel::High);
    }
}
