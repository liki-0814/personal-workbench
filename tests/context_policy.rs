use pwcli::{
    context::ContextBuilder,
    policy::{DefaultPolicyGuard, PolicyDecision, PolicyGuard},
    tools::{
        builtin::BuiltinToolLoader, verification::VerificationToolLoader, InvocationMode,
        LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolLoader, ToolRegistry, ToolSource,
    },
};
use serde_json::json;
use std::path::PathBuf;

fn skill_tool(name: &str, description: &str) -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: format!("skill.{name}"),
            name: name.to_string(),
            description: description.to_string(),
            input_schema: json!({ "type": "object" }),
            source: ToolSource::Skill {
                path: PathBuf::from(format!("/tmp/{name}")),
            },
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Prompt,
            capabilities: vec!["skill.prompt".to_string()],
            metadata: json!({ "frontmatter": {} }),
            enabled: true,
        },
        executor: None,
    }
}

#[test]
fn context_selects_explicit_skill() {
    let mut registry = ToolRegistry::new();
    registry.register(skill_tool("researcher", "Use for research reports."));
    let snapshot = registry.snapshot();

    let pack = ContextBuilder::new().build("$researcher prepare a report", &snapshot);
    assert_eq!(pack.explicit_skill_ids, vec!["skill.researcher"]);
    assert!(pack
        .selected_tool_ids
        .contains(&"skill.researcher".to_string()));
}

#[test]
fn context_selects_verification_tool_for_chinese_test_intent() {
    let mut registry = ToolRegistry::new();
    registry.register_many(VerificationToolLoader.load().unwrap());
    let snapshot = registry.snapshot();

    let pack = ContextBuilder::new().build("帮我跑测试并验证项目是否通过", &snapshot);
    assert!(pack
        .selected_tool_ids
        .contains(&"verification.project_check".to_string()));
}

#[test]
fn context_selects_search_pdf_and_explicit_code_agent_intents() {
    let mut registry = ToolRegistry::new();
    registry.register_many(BuiltinToolLoader.load().unwrap());
    let snapshot = registry.snapshot();

    let search = ContextBuilder::new().build("帮我联网搜索一下最新资料", &snapshot);
    assert!(search
        .selected_tool_ids
        .contains(&"builtin.anysearch".to_string()));

    let pdf = ContextBuilder::new().build("解析这个 PDF 论文", &snapshot);
    assert!(pdf
        .selected_tool_ids
        .contains(&"builtin.mineru_parse_document".to_string()));

    let codex = ContextBuilder::new().build("调用 codex 帮我改代码", &snapshot);
    assert!(codex
        .selected_tool_ids
        .contains(&"agent_cli.codex".to_string()));
    assert!(!codex
        .selected_tool_ids
        .contains(&"agent_cli.claude".to_string()));
}

#[test]
fn context_builds_ordered_tool_selection_plan_with_fallbacks() {
    let mut registry = ToolRegistry::new();
    registry.register_many(BuiltinToolLoader.load().unwrap());
    registry.register_many(VerificationToolLoader.load().unwrap());
    registry.register(skill_tool("researcher", "Use for research reports."));
    let snapshot = registry.snapshot();

    let pack = ContextBuilder::new().build("搜索论文资料，解析 PDF，然后验证结论", &snapshot);
    assert_eq!(pack.tool_selection_plan.task_type, "document");
    assert!(pack
        .tool_selection_plan
        .steps
        .iter()
        .any(|step| step.stage == "search"));
    assert!(pack
        .tool_selection_plan
        .steps
        .iter()
        .any(|step| step.stage == "parse"));
    assert!(pack
        .tool_selection_plan
        .steps
        .iter()
        .any(|step| step.stage == "verify"));
    assert_eq!(
        pack.selected_tool_ids,
        pack.tool_selection_plan
            .details
            .iter()
            .map(|detail| detail.tool_id.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn default_policy_denies_protected_paths_and_asks_for_medium_risk() {
    let descriptor = ToolDescriptor {
        id: "builtin.write".to_string(),
        name: "write".to_string(),
        description: "write file".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::Medium,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard::default();

    let protected = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.write".to_string(),
        name: "write".to_string(),
        arguments: json!({ "path": "/etc/passwd" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &protected),
        PolicyDecision::Deny { .. }
    ));

    let normal = ToolCall {
        id: "2".to_string(),
        tool_id: "builtin.write".to_string(),
        name: "write".to_string(),
        arguments: json!({ "path": "/tmp/example" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &normal),
        PolicyDecision::AskUser { .. }
    ));
}

#[test]
fn context_selects_external_capability_matrix_tools() {
    let mut registry = ToolRegistry::new();
    registry.register_many(BuiltinToolLoader.load().unwrap());
    let snapshot = registry.snapshot();

    let github = ContextBuilder::new().build("检查 github pull request 和 workflow", &snapshot);
    assert!(github
        .selected_tool_ids
        .contains(&"builtin.github".to_string()));

    let sql = ContextBuilder::new().build("帮我对这段 SQL 做 dry run 和 explain", &snapshot);
    assert!(sql
        .selected_tool_ids
        .contains(&"builtin.sql_dry_run".to_string()));

    let browser = ContextBuilder::new().build("用浏览器打开页面并截图", &snapshot);
    assert!(browser
        .selected_tool_ids
        .contains(&"builtin.browser_automation".to_string()));

    let fetch = ContextBuilder::new().build("抓取 https://example.com 页面内容", &snapshot);
    assert!(fetch
        .selected_tool_ids
        .contains(&"builtin.web_fetch".to_string()));

    let local = ContextBuilder::new().build("查文件里哪里定义了 registry", &snapshot);
    assert!(local
        .selected_tool_ids
        .contains(&"builtin.local_file_index".to_string()));

    let ssh = ContextBuilder::new().build("ssh 到远程服务器查看日志", &snapshot);
    assert!(ssh
        .selected_tool_ids
        .contains(&"builtin.ssh_exec".to_string()));
}

#[test]
fn default_policy_applies_source_specific_gates_and_secret_leak_protection() {
    let policy = DefaultPolicyGuard::default();
    let agent = ToolDescriptor {
        id: "agent_cli.codex".to_string(),
        name: "codex".to_string(),
        description: "run codex".to_string(),
        input_schema: json!({}),
        source: ToolSource::AgentCli {
            cli: "codex".to_string(),
        },
        risk_level: RiskLevel::Low,
        invocation_mode: InvocationMode::AgentCli,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let yolo_call = ToolCall {
        id: "1".to_string(),
        tool_id: "agent_cli.codex".to_string(),
        name: "codex".to_string(),
        arguments: json!({ "permission_mode": "dangerously-skip-permissions" }),
    };
    assert!(matches!(
        policy.check(&agent, &yolo_call),
        PolicyDecision::AskUser { .. }
    ));

    let mcp = ToolDescriptor {
        id: "mcp.remote.read".to_string(),
        name: "read".to_string(),
        description: "remote read".to_string(),
        input_schema: json!({}),
        source: ToolSource::Mcp {
            server: "remote".to_string(),
        },
        risk_level: RiskLevel::ReadOnly,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let secret_call = ToolCall {
        id: "2".to_string(),
        tool_id: "mcp.remote.read".to_string(),
        name: "read".to_string(),
        arguments: json!({ "api_key": "sk-test-abcdefghijklmnopqrstuvwxyz123456" }),
    };
    assert!(matches!(
        policy.check(&mcp, &secret_call),
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn default_policy_turns_delete_rules_into_confirmation_gate() {
    let descriptor = ToolDescriptor {
        id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        description: "run shell command".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::ReadOnly,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy =
        DefaultPolicyGuard::default().with_rules(vec!["必须在删除文件前询问用户确认".to_string()]);

    let delete_call = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "rm -rf target/tmp" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &delete_call),
        PolicyDecision::AskUser { .. }
    ));

    let safe_call = ToolCall {
        id: "2".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "ls target/tmp" }),
    };
    assert_eq!(policy.check(&descriptor, &safe_call), PolicyDecision::Allow);
}

#[test]
fn default_policy_yolo_does_not_bypass_delete_rules() {
    let descriptor = ToolDescriptor {
        id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        description: "run shell command".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::ReadOnly,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard {
        yolo: true,
        ..DefaultPolicyGuard::default()
    }
    .with_rules(vec!["必须在删除文件前询问用户确认".to_string()]);

    let delete_call = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "rm -rf target/tmp" }),
    };

    assert!(matches!(
        policy.check(&descriptor, &delete_call),
        PolicyDecision::AskUser { .. }
    ));
}

#[test]
fn default_policy_yolo_does_not_bypass_protected_paths() {
    let descriptor = ToolDescriptor {
        id: "builtin.write".to_string(),
        name: "write".to_string(),
        description: "write file".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::ReadOnly,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard {
        yolo: true,
        ..DefaultPolicyGuard::default()
    };
    let protected = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.write".to_string(),
        name: "write".to_string(),
        arguments: json!({ "path": "/etc/passwd" }),
    };

    assert!(matches!(
        policy.check(&descriptor, &protected),
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn default_policy_detects_protected_paths_inside_shell_commands() {
    let descriptor = ToolDescriptor {
        id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        description: "run shell command".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::ReadOnly,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard {
        yolo: true,
        ..DefaultPolicyGuard::default()
    };

    let etc_delete = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "rm -rf /etc/pwcli-test" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &etc_delete),
        PolicyDecision::Deny { .. }
    ));

    let root_delete = ToolCall {
        id: "2".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "rm -rf /" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &root_delete),
        PolicyDecision::Deny { .. }
    ));

    let workspace_delete = ToolCall {
        id: "3".to_string(),
        tool_id: "builtin.shell".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "rm -rf ./target/tmp" }),
    };
    assert!(matches!(
        policy.check(&descriptor, &workspace_delete),
        PolicyDecision::AskUser { .. }
    ));
}

#[test]
fn default_policy_always_asks_for_ssh_remote_execution() {
    let descriptor = ToolDescriptor {
        id: "builtin.ssh_exec".to_string(),
        name: "ssh_exec".to_string(),
        description: "run ssh command".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::High,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec!["ssh".to_string(), "remote_exec".to_string()],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard {
        yolo: true,
        ..DefaultPolicyGuard::default()
    };
    let call = ToolCall {
        id: "ssh".to_string(),
        tool_id: "builtin.ssh_exec".to_string(),
        name: "ssh_exec".to_string(),
        arguments: json!({
            "host": "dev",
            "command": "uptime",
            "private_key_path": "/tmp/id_ed25519"
        }),
    };

    assert!(matches!(
        policy.check(&descriptor, &call),
        PolicyDecision::AskUser { .. }
    ));
}

#[test]
fn default_policy_yolo_allows_regular_risk_gate() {
    let descriptor = ToolDescriptor {
        id: "builtin.write".to_string(),
        name: "write".to_string(),
        description: "write file".to_string(),
        input_schema: json!({}),
        source: ToolSource::Builtin,
        risk_level: RiskLevel::Medium,
        invocation_mode: InvocationMode::Internal,
        capabilities: vec![],
        metadata: json!({}),
        enabled: true,
    };
    let policy = DefaultPolicyGuard {
        yolo: true,
        ..DefaultPolicyGuard::default()
    };
    let normal = ToolCall {
        id: "1".to_string(),
        tool_id: "builtin.write".to_string(),
        name: "write".to_string(),
        arguments: json!({ "path": "/tmp/example" }),
    };

    assert_eq!(policy.check(&descriptor, &normal), PolicyDecision::Allow);
}
