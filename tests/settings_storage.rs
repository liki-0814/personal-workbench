use pwcli::{
    settings::{McpTransportKind, Settings, ToolApprovalMode, ToolNetworkPolicy},
    storage::WorkspacePaths,
};
use std::fs;

#[test]
fn workspace_paths_ensure_directories() {
    let temp = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_pwcli_home(temp.path().join(".pwcli"));
    paths.ensure().unwrap();
    assert!(paths.pwcli_home.is_dir());
    assert!(paths.audit_dir.is_dir());
    assert!(paths.sessions_dir.is_dir());
    assert!(paths.tasks_dir.is_dir());
    assert!(paths.cache_dir.is_dir());
    assert!(paths.memory_dir.is_dir());
    assert!(paths.rules_dir.is_dir());
    assert!(paths.models_dir.is_dir());
}

#[test]
fn settings_save_default_writes_config() {
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings::from_home(temp.path());
    settings.save_default().unwrap();
    assert!(settings.pwcli_home.join("config.json").is_file());
    let saved = fs::read_to_string(settings.pwcli_home.join("config.json")).unwrap();
    assert!(saved.contains("\"memory\""));
    assert!(saved.contains("BAAI/bge-small-zh-v1.5"));
}

#[test]
fn settings_loads_configured_key_and_model_list() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "provider": "local-openai",
          "model": "test-chat-model",
          "providers": [{
            "name": "local-openai",
            "protocol": "openai",
            "base_url": "http://localhost:1234/v1",
            "api_key": "test-key",
            "models": [
              {
                "name": "test-chat-model",
                "supports_image_input": true,
                "supports_thinking": true,
                "is_image_generation": false,
                "max_input_tokens": 1000000,
                "max_output_tokens": 4096
              }
            ]
          }]
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.provider, "local-openai");
    assert_eq!(settings.model, "test-chat-model");
    assert_eq!(settings.providers.len(), 1);
    let resolved = settings.resolved_model_settings().unwrap();
    assert_eq!(resolved.api_key.as_deref(), Some("test-key"));
    assert_eq!(resolved.base_url, "http://localhost:1234/v1");
    assert!(resolved.supports_image_input);
    assert!(resolved.supports_thinking);
}

#[test]
fn settings_loads_mineru_token() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "mineru": {
            "base_url": "https://mineru.example",
            "token": "mineru-test-token"
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.mineru.base_url, "https://mineru.example");
    assert_eq!(settings.mineru.token.as_deref(), Some("mineru-test-token"));
}

#[test]
fn settings_accepts_mineru_api_key_alias() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "mineru": {
            "api_key": "mineru-alias-token"
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.mineru.token.as_deref(), Some("mineru-alias-token"));
}

#[test]
fn settings_loads_anysearch_api_key() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "anysearch": {
            "endpoint": "https://anysearch.example/mcp",
            "api_key": "anysearch-test-key"
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.anysearch.endpoint, "https://anysearch.example/mcp");
    assert_eq!(
        settings.anysearch.api_key.as_deref(),
        Some("anysearch-test-key")
    );
}

#[test]
fn settings_loads_github_token() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "github": {
            "api_url": "https://api.github.example",
            "token": "ghp-test",
            "request_timeout_seconds": 12
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.github.api_url, "https://api.github.example");
    assert_eq!(settings.github.token.as_deref(), Some("ghp-test"));
    assert_eq!(settings.github.request_timeout_seconds, 12);
}

#[test]
fn settings_loads_ssh_hosts() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "ssh": {
            "hosts": [{
              "name": "dev",
              "host": "dev.example.com",
              "port": 2222,
              "username": "ubuntu",
              "private_key_path": "/tmp/id_ed25519",
              "key_passphrase_env": "SSH_KEY_PASSPHRASE",
              "known_hosts_path": "/tmp/known_hosts",
              "accept_unknown_host_key": true,
              "learn_unknown_host_key": true,
              "default_cwd": "/srv/app",
              "timeout_seconds": 45
            }]
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.ssh.hosts.len(), 1);
    let host = &settings.ssh.hosts[0];
    assert_eq!(host.name, "dev");
    assert_eq!(host.host, "dev.example.com");
    assert_eq!(host.port, 2222);
    assert_eq!(host.username.as_deref(), Some("ubuntu"));
    assert_eq!(host.default_cwd.as_deref(), Some("/srv/app"));
    assert_eq!(host.timeout_seconds, 45);
    assert!(host.accept_unknown_host_key);
    assert!(host.learn_unknown_host_key);
}

#[test]
fn settings_loads_anysearch_rate_limit() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "anysearch": {
            "api_key": "anysearch-test-key",
            "rate_limit": {
              "max_per_minute": 12,
              "max_parallel": 1,
              "retry_on_429": false,
              "max_retries": 0
            }
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.anysearch.rate_limit.max_per_minute, 12);
    assert_eq!(settings.anysearch.rate_limit.max_parallel, 1);
    assert!(!settings.anysearch.rate_limit.retry_on_429);
    assert_eq!(settings.anysearch.rate_limit.max_retries, 0);
}

#[test]
fn settings_accepts_anysearch_token_alias() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "anysearch": {
            "token": "anysearch-token-alias"
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(
        settings.anysearch.api_key.as_deref(),
        Some("anysearch-token-alias")
    );
}

#[test]
fn settings_loads_mcp_servers() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "mcp": {
            "servers": [{
              "name": "filesystem",
              "transport": "stdio",
              "command": "mcp-server-filesystem",
              "args": ["/tmp"],
              "env": { "EXAMPLE_TOKEN": "secret" },
              "timeout_seconds": 10,
              "risk_level": "medium"
            }, {
              "name": "remote-docs",
              "transport": "http",
              "url": "https://mcp.example/mcp",
              "headers": { "authorization": "Bearer token" }
            }]
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.mcp.servers.len(), 2);
    assert_eq!(settings.mcp.servers[0].name, "filesystem");
    assert_eq!(settings.mcp.servers[0].transport, McpTransportKind::Stdio);
    assert_eq!(
        settings.mcp.servers[0].command.as_deref(),
        Some("mcp-server-filesystem")
    );
    assert_eq!(settings.mcp.servers[0].timeout_seconds, 10);
    assert_eq!(settings.mcp.servers[1].transport, McpTransportKind::Http);
    assert_eq!(
        settings.mcp.servers[1]
            .headers
            .get("authorization")
            .map(String::as_str),
        Some("Bearer token")
    );
}

#[test]
fn settings_loads_tool_config() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "tools": {
            "allowlist": ["builtin.*"],
            "denylist": ["builtin.blocked"],
            "disabled": ["skill.legacy"],
            "risk_overrides": { "builtin.anysearch": "read_only" },
            "approval_overrides": { "builtin.mineru_parse_document": "always" },
            "default_timeout_seconds": 60,
            "timeout_seconds": { "mcp.*": 10 },
            "rate_limits": { "builtin.anysearch": { "max_per_minute": 5, "max_parallel": 1 } },
            "retry": { "mcp.*": { "max_retries": 2, "backoff_ms": 250 } },
            "secrets": { "builtin.anysearch": { "env": "ANYSEARCH_API_KEY" } },
            "network_policy": "local_only"
          }
        }"#,
    )
    .unwrap();

    let settings = Settings::load_from_home(temp.path()).unwrap();
    assert_eq!(settings.tools.allowlist, vec!["builtin.*"]);
    assert_eq!(
        settings
            .tools
            .approval_overrides
            .get("builtin.mineru_parse_document"),
        Some(&ToolApprovalMode::Always)
    );
    assert_eq!(settings.tools.network_policy, ToolNetworkPolicy::LocalOnly);
    assert_eq!(
        settings
            .tools
            .rate_limits
            .get("builtin.anysearch")
            .unwrap()
            .max_per_minute,
        5
    );
}

#[test]
fn nvidia_thinking_params_are_derived_from_global_thinking() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".pwcli")).unwrap();
    fs::write(
        temp.path().join(".pwcli/config.json"),
        r#"{
          "provider": "nvidia",
          "model": "thinking-model",
          "thinking": true,
          "providers": [{
            "name": "nvidia",
            "protocol": "nvidia",
            "base_url": "https://integrate.api.nvidia.com/v1",
            "api_key": "test-key",
            "models": [
              {
                "name": "thinking-model",
                "supports_thinking": true,
                "max_input_tokens": 1000,
                "max_output_tokens": 1000
              }
            ]
          }]
        }"#,
    )
    .unwrap();

    let mut settings = Settings::load_from_home(temp.path()).unwrap();
    let resolved = settings.resolved_model_settings().unwrap();
    assert!(resolved.thinking_enabled);
    assert!(!resolved.show_thinking);
    assert_eq!(
        resolved.extra_body["chat_template_kwargs"]["thinking_mode"],
        "enabled"
    );

    settings.set_show_thinking(true);
    let resolved = settings.resolved_model_settings().unwrap();
    assert!(resolved.show_thinking);

    settings.set_thinking(false);
    let resolved = settings.resolved_model_settings().unwrap();
    assert!(!resolved.thinking_enabled);
    assert!(resolved.show_thinking);
    assert_eq!(
        resolved.extra_body["chat_template_kwargs"]["thinking_mode"],
        "disabled"
    );
}
