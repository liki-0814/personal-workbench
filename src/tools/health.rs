use crate::{
    settings::Settings,
    tools::{
        builtin::BuiltinToolLoader,
        config::apply_tool_settings,
        mcp::{probe_mcp_server, McpToolLoader},
        skills::{watcher::scan_skill_roots, SkillToolLoader},
        verification::VerificationToolLoader,
        LoadedTool, ToolLoader, ToolSource,
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    process::{Command, Stdio},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolHealthStatus {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHealthCheck {
    pub status: ToolHealthStatus,
    pub label: String,
    pub detail: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHealthReport {
    pub generated_at: String,
    pub checks: Vec<ToolHealthCheck>,
}

impl ToolHealthReport {
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut ok = 0;
        let mut warn = 0;
        let mut fail = 0;
        let mut info = 0;
        for check in &self.checks {
            match check.status {
                ToolHealthStatus::Ok => ok += 1,
                ToolHealthStatus::Warn => warn += 1,
                ToolHealthStatus::Fail => fail += 1,
                ToolHealthStatus::Info => info += 1,
            }
        }
        (ok, warn, fail, info)
    }
}

pub fn build_tool_health_report(settings: &Settings) -> ToolHealthReport {
    let mut checks = Vec::new();
    checks.extend(integration_checks(settings));
    checks.extend(agent_cli_checks());
    checks.extend(ssh_checks(settings));
    checks.extend(skill_checks(settings));
    checks.extend(mcp_checks(settings));
    checks.extend(schema_checks(settings));

    ToolHealthReport {
        generated_at: Utc::now().to_rfc3339(),
        checks,
    }
}

fn integration_checks(settings: &Settings) -> Vec<ToolHealthCheck> {
    let mut checks = Vec::new();
    checks.push(
        if settings
            .mineru
            .token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
        {
            ok("mineru", "token configured")
        } else {
            warn("mineru", "token missing; PDF parsing API will not work")
        },
    );
    checks.push(
        if settings
            .anysearch
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            ok("anysearch", "api key configured")
        } else {
            warn(
                "anysearch",
                "api key missing; web search tool will not work",
            )
        },
    );
    checks.push(
        if settings
            .github
            .token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
            || env::var("GITHUB_TOKEN")
                .ok()
                .is_some_and(|token| !token.trim().is_empty())
        {
            ok("github", "token configured")
        } else {
            info(
                "github",
                "token missing; public GitHub API reads may still work with lower rate limits",
            )
        },
    );
    checks
}

fn ssh_checks(settings: &Settings) -> Vec<ToolHealthCheck> {
    if settings.ssh.hosts.is_empty() {
        return vec![info("ssh", "no SSH hosts configured")];
    }
    settings
        .ssh
        .hosts
        .iter()
        .map(|host| {
            if host.host.trim().is_empty() {
                return fail(format!("ssh {}", host.name), "host is empty");
            }
            let has_key = host
                .private_key_path
                .as_ref()
                .is_some_and(|path| path.is_file());
            let has_password_env = host
                .password_env
                .as_deref()
                .and_then(|name| env::var(name).ok())
                .is_some_and(|value| !value.trim().is_empty());
            if has_key || has_password_env {
                ok(
                    format!("ssh {}", host.name),
                    format!(
                        "{}:{} auth={} host_key_policy={}",
                        host.host,
                        host.port,
                        if has_key { "key" } else { "password_env" },
                        if host.accept_unknown_host_key {
                            "accept_unknown"
                        } else {
                            "known_hosts"
                        }
                    ),
                )
            } else {
                warn(
                    format!("ssh {}", host.name),
                    "missing usable private_key_path or password_env",
                )
            }
        })
        .collect()
}

fn agent_cli_checks() -> Vec<ToolHealthCheck> {
    [
        "codex", "claude", "agy", "qodercli", "node", "sqlite3", "psql", "mysql",
    ]
    .into_iter()
    .map(|binary| {
        if let Some(path) = binary_on_path(binary) {
            let version = command_version(binary).unwrap_or_else(|| "version unknown".to_string());
            let label = if matches!(binary, "node" | "sqlite3" | "psql" | "mysql") {
                format!("tool binary {binary}")
            } else {
                format!("agent cli {binary}")
            };
            ToolHealthCheck {
                status: ToolHealthStatus::Ok,
                label,
                detail: format!("available at {} ({version})", path.display()),
                metadata: json!({ "path": path, "version": version }),
            }
        } else {
            let label = if matches!(binary, "node" | "sqlite3" | "psql" | "mysql") {
                format!("tool binary {binary}")
            } else {
                format!("agent cli {binary}")
            };
            info(
                label,
                "not found on PATH; related tool will fail until installed",
            )
        }
    })
    .collect()
}

fn skill_checks(settings: &Settings) -> Vec<ToolHealthCheck> {
    let mut checks = Vec::new();
    match scan_skill_roots(&settings.skill_roots) {
        Ok(inventory) => {
            checks.push(ok(
                "skills",
                format!(
                    "loaded={} conflicts={} roots={}",
                    inventory.tool_ids.len(),
                    inventory.conflicts.len(),
                    inventory.roots.len()
                ),
            ));
            for conflict in inventory.conflicts {
                checks.push(warn(
                    format!("skill conflict {}", conflict.tool_id),
                    format!(
                        "duplicate tool id at {}",
                        conflict
                            .paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            for skill in inventory.health {
                let detail = format!("{}: {}", skill.path.display(), skill.message);
                checks.push(match skill.status {
                    crate::tools::skills::watcher::SkillHealthStatus::Ok => ok("skill", detail),
                    crate::tools::skills::watcher::SkillHealthStatus::Warn => warn("skill", detail),
                    crate::tools::skills::watcher::SkillHealthStatus::Fail => fail("skill", detail),
                });
            }
        }
        Err(err) => checks.push(fail("skills", err.to_string())),
    }
    checks
}

fn mcp_checks(settings: &Settings) -> Vec<ToolHealthCheck> {
    if settings.mcp.servers.is_empty() {
        return vec![info("mcp", "no MCP servers configured")];
    }
    settings
        .mcp
        .servers
        .iter()
        .map(|server| {
            if !server.enabled {
                return info(format!("mcp {}", server.name), "disabled");
            }
            match probe_mcp_server(server, &settings.pwcli_home) {
                Ok(count) => ok(
                    format!("mcp {}", server.name),
                    format!("reachable tools={count}"),
                ),
                Err(err) => warn(
                    format!("mcp {}", server.name),
                    format!("not reachable: {err}"),
                ),
            }
        })
        .collect()
}

fn schema_checks(settings: &Settings) -> Vec<ToolHealthCheck> {
    let mut tools = Vec::<LoadedTool>::new();
    let mut checks = Vec::new();
    match BuiltinToolLoader.load() {
        Ok(loaded) => tools.extend(loaded),
        Err(err) => checks.push(fail("builtin tools", err.to_string())),
    }
    match VerificationToolLoader.load() {
        Ok(loaded) => tools.extend(loaded),
        Err(err) => checks.push(fail("verification tools", err.to_string())),
    }
    match SkillToolLoader::new(settings.skill_roots.clone()).load() {
        Ok(loaded) => tools.extend(loaded),
        Err(err) => checks.push(fail("skill tools", err.to_string())),
    }
    match McpToolLoader::new(settings.mcp.clone(), settings.pwcli_home.clone()).load() {
        Ok(loaded) => tools.extend(loaded),
        Err(err) => checks.push(warn("mcp tools", err.to_string())),
    }
    let tools = apply_tool_settings(tools, &settings.tools);
    for tool in tools {
        if tool.descriptor.id.trim().is_empty() || tool.descriptor.name.trim().is_empty() {
            checks.push(fail("tool schema", "tool id/name cannot be empty"));
            continue;
        }
        if !tool.descriptor.input_schema.is_object() {
            checks.push(warn(
                format!("tool schema {}", tool.descriptor.id),
                "input_schema is not an object",
            ));
        }
        if matches!(tool.descriptor.source, ToolSource::Skill { .. })
            && tool.descriptor.description.trim().is_empty()
        {
            checks.push(warn(
                format!("tool schema {}", tool.descriptor.id),
                "skill description is empty",
            ));
        }
    }
    checks.push(ok("tool schema", "registered descriptors validated"));
    checks
}

fn ok(label: impl Into<String>, detail: impl Into<String>) -> ToolHealthCheck {
    ToolHealthCheck {
        status: ToolHealthStatus::Ok,
        label: label.into(),
        detail: detail.into(),
        metadata: json!({}),
    }
}

fn warn(label: impl Into<String>, detail: impl Into<String>) -> ToolHealthCheck {
    ToolHealthCheck {
        status: ToolHealthStatus::Warn,
        label: label.into(),
        detail: detail.into(),
        metadata: json!({}),
    }
}

fn fail(label: impl Into<String>, detail: impl Into<String>) -> ToolHealthCheck {
    ToolHealthCheck {
        status: ToolHealthStatus::Fail,
        label: label.into(),
        detail: detail.into(),
        metadata: json!({}),
    }
}

fn info(label: impl Into<String>, detail: impl Into<String>) -> ToolHealthCheck {
    ToolHealthCheck {
        status: ToolHealthStatus::Info,
        label: label.into(),
        detail: detail.into(),
        metadata: json!({}),
    }
}

fn binary_on_path(binary: &str) -> Option<std::path::PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn command_version(binary: &str) -> Option<String> {
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let text = text.lines().next()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}
