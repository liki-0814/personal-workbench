use crate::Result;

use super::{
    agent_cli::{agent_cli_schema, AgentCliExecutor, AgentCliKind},
    anysearch::{anysearch_schema, AnySearchExecutor},
    browser::{browser_automation_schema, BrowserAutomationExecutor},
    github::{github_schema, GitHubExecutor},
    local_index::{local_file_index_schema, LocalFileIndexExecutor},
    mineru::{mineru_parse_schema, MineruExecutor},
    sql::{sql_dry_run_schema, SqlDryRunExecutor},
    ssh::{ssh_exec_schema, SshExecExecutor},
    web_fetch::{web_fetch_schema, WebFetchExecutor},
    InvocationMode, LoadedTool, RiskLevel, ToolDescriptor, ToolLoader, ToolSource,
};
use serde_json::json;
use std::sync::Arc;

pub struct BuiltinToolLoader;

impl ToolLoader for BuiltinToolLoader {
    fn load(&self) -> Result<Vec<LoadedTool>> {
        Ok(vec![
            agent_cli_tool(AgentCliKind::Codex),
            agent_cli_tool(AgentCliKind::Claude),
            agent_cli_tool(AgentCliKind::Agy),
            agent_cli_tool(AgentCliKind::QoderCli),
            anysearch_tool(),
            mineru_parse_tool(),
            local_file_index_tool(),
            web_fetch_tool(),
            browser_automation_tool(),
            github_tool(),
            sql_dry_run_tool(),
            ssh_exec_tool(),
        ])
    }
}

fn local_file_index_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.local_file_index".to_string(),
            name: "local_file_index".to_string(),
            description: "Index, search, and preview local project files with bounded scanning. Use before reading local project context or finding relevant files.".to_string(),
            input_schema: local_file_index_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "local_file_index".to_string(),
                "local_context".to_string(),
                "filesystem".to_string(),
                "file_search".to_string(),
                "search".to_string(),
            ],
            metadata: json!({
                "storage": "~/.pwcli/index/local_files.json",
                "read_only": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(LocalFileIndexExecutor::new())),
    }
}

fn web_fetch_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.web_fetch".to_string(),
            name: "web_fetch".to_string(),
            description: "Fetch one HTTP/HTTPS page and optionally extract readable text. Use after search results or when the user gives a URL.".to_string(),
            input_schema: web_fetch_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "web_fetch".to_string(),
                "page_extract".to_string(),
                "web_extract".to_string(),
                "url_read".to_string(),
            ],
            metadata: json!({
                "network": true,
                "read_only": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(WebFetchExecutor::new())),
    }
}

fn browser_automation_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.browser_automation".to_string(),
            name: "browser_automation".to_string(),
            description: "Use local Playwright browser automation for dynamic pages, screenshots, and rendered text extraction. Requires Node.js and Playwright installed locally.".to_string(),
            input_schema: browser_automation_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "browser".to_string(),
                "browser_automation".to_string(),
                "screenshot".to_string(),
                "page_extract".to_string(),
                "dynamic_web".to_string(),
            ],
            metadata: json!({
                "requires_binary": "node",
                "requires_node_module": "playwright",
                "network": true,
                "read_only": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(BrowserAutomationExecutor::new())),
    }
}

fn github_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.github".to_string(),
            name: "github".to_string(),
            description: "Read GitHub repositories, issues, pull requests, workflow runs, and file metadata through the GitHub REST API.".to_string(),
            input_schema: github_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "github".to_string(),
                "code_hosting".to_string(),
                "issues".to_string(),
                "pull_requests".to_string(),
                "ci".to_string(),
            ],
            metadata: json!({
                "provider": "github",
                "requires_config_optional": "github.token",
                "network": true,
                "read_only": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(GitHubExecutor::new())),
    }
}

fn sql_dry_run_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.sql_dry_run".to_string(),
            name: "sql_dry_run".to_string(),
            description: "Validate and explain read-only SQL queries against sqlite/postgres/mysql using local database CLIs. Rejects mutating statements.".to_string(),
            input_schema: sql_dry_run_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "sql".to_string(),
                "database".to_string(),
                "dry_run".to_string(),
                "explain".to_string(),
                "verification".to_string(),
            ],
            metadata: json!({
                "read_only": true,
                "requires_binary_one_of": ["sqlite3", "psql", "mysql"]
            }),
            enabled: true,
        },
        executor: Some(Arc::new(SqlDryRunExecutor::new())),
    }
}

fn ssh_exec_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.ssh_exec".to_string(),
            name: "ssh_exec".to_string(),
            description: "Execute a remote command over SSH using the pure-Rust russh client. Use for remote diagnostics, tests, logs, and remote agent CLI invocation when configured.".to_string(),
            input_schema: ssh_exec_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::High,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "ssh".to_string(),
                "remote_shell".to_string(),
                "remote_exec".to_string(),
                "remote_agent".to_string(),
                "infrastructure".to_string(),
            ],
            metadata: json!({
                "transport": "russh",
                "network": true,
                "remote_execution": true,
                "requires_config_optional": "ssh.hosts",
                "supports_streaming": true,
                "supports_cancellation": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(SshExecExecutor::new())),
    }
}

fn anysearch_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.anysearch".to_string(),
            name: "anysearch".to_string(),
            description: "Real-time web search, vertical domain search, batch search, and URL content extraction through AnySearch JSON-RPC API. Use get_sub_domains before vertical searches.".to_string(),
            input_schema: anysearch_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "web_search".to_string(),
                "search".to_string(),
                "web_extract".to_string(),
                "vertical_search".to_string(),
                "anysearch".to_string(),
            ],
            metadata: json!({
                "provider": "anysearch",
                "endpoint": "https://api.anysearch.com/mcp",
                "api_key_optional": true,
                "actions": ["search", "batch_search", "extract", "get_sub_domains"],
                "vertical_domains": [
                    "general", "resource", "social_media", "finance", "academic", "legal",
                    "health", "business", "security", "ip", "code", "energy",
                    "environment", "agriculture", "travel", "film", "gaming"
                ],
                "source_skill": "../anysearch-skill"
            }),
            enabled: true,
        },
        executor: Some(Arc::new(AnySearchExecutor::new())),
    }
}

fn mineru_parse_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.mineru_parse_document".to_string(),
            name: "mineru_parse_document".to_string(),
            description: "Parse PDF or office documents with MinerU precise API. Supports remote URL or local file upload, then returns task state and full_zip_url for Markdown/JSON outputs.".to_string(),
            input_schema: mineru_parse_schema(),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "pdf".to_string(),
                "document_parse".to_string(),
                "mineru".to_string(),
                "ocr".to_string(),
                "markdown".to_string(),
            ],
            metadata: json!({
                "provider": "mineru",
                "api": "precise",
                "requires_config": "mineru.token",
                "supports_url": true,
                "supports_local_file": true,
                "source_docs": "https://mineru.net/apiManage/docs"
            }),
            enabled: true,
        },
        executor: Some(Arc::new(MineruExecutor::new())),
    }
}

fn agent_cli_tool(kind: AgentCliKind) -> LoadedTool {
    let usage_hints = kind.usage_hints();
    LoadedTool {
        descriptor: ToolDescriptor {
            id: format!("agent_cli.{}", kind.id()),
            name: format!("{} agent", kind.id()),
            description: format!(
                "Delegate coding work to local {}. Use for specialist code-agent subtask execution, planning, or review when pwcli decides local delegation is useful.",
                kind.display_name()
            ),
            input_schema: agent_cli_schema(),
            source: ToolSource::AgentCli {
                cli: kind.id().to_string(),
            },
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::AgentCli,
            capabilities: vec![
                "agent_cli".to_string(),
                "code_agent".to_string(),
                "subagent".to_string(),
                "coding".to_string(),
                "planning".to_string(),
                "review".to_string(),
                kind.id().to_string(),
            ],
            metadata: json!({
                "binary": kind.binary(),
                "best_model_hint": kind.best_model_hint(),
                "usage_hints": usage_hints,
                "supports_background_callback": true,
                "supports_yolo": true,
                "mode_is_hint_not_workflow": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(AgentCliExecutor::new(kind))),
    }
}
