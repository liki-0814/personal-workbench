use crate::{
    memory::MemoryStore,
    runtime::{format_task_next, RuntimeTaskManager, RuntimeTaskStatus},
    settings::{ProviderSettings, Settings},
    storage::WorkspacePaths,
    tools::{
        builtin::BuiltinToolLoader,
        config::apply_tool_settings,
        health::{build_tool_health_report, ToolHealthStatus},
        mcp::McpToolLoader,
        skills::SkillToolLoader,
        verification::VerificationToolLoader,
        LoadedTool, ToolLoader, ToolSource,
    },
};
use std::{
    collections::BTreeMap,
    env,
    fmt::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
struct ToolInventory {
    total: usize,
    builtin: usize,
    agent_cli: usize,
    skill: usize,
    mcp: usize,
    verification: usize,
    model: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckLevel {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, Clone)]
struct DiagnosticCheck {
    level: CheckLevel,
    label: String,
    detail: String,
}

impl DiagnosticCheck {
    fn ok(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Ok,
            label: label.into(),
            detail: detail.into(),
        }
    }

    fn warn(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Warn,
            label: label.into(),
            detail: detail.into(),
        }
    }

    fn fail(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Fail,
            label: label.into(),
            detail: detail.into(),
        }
    }

    fn info(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Info,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

pub fn build_status_report(settings: &Settings) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "pwcli status");
    let _ = writeln!(out, "home: {}", settings.pwcli_home.display());
    let _ = writeln!(out, "config: {}", config_path(settings).display());

    match settings.resolved_model_settings() {
        Ok(model) => {
            let _ = writeln!(
                out,
                "provider: {} ({})",
                model.provider_name,
                model.provider.as_str()
            );
            let _ = writeln!(
                out,
                "model: {} | thinking={} | image_input={} | image_generation={}",
                model.model,
                model.thinking_enabled,
                model.supports_image_input,
                model.is_image_generation
            );
            let _ = writeln!(
                out,
                "context: input={} output={} keep_recent_turns={}",
                model.max_input_tokens, model.max_output_tokens, settings.context.keep_recent_turns
            );
            let key_state = if provider_has_key(settings.active_provider().ok()) {
                "configured"
            } else {
                "missing"
            };
            let _ = writeln!(out, "provider_key: {key_state} ({})", model.api_key_env);
        }
        Err(err) => {
            let _ = writeln!(out, "provider: not ready ({err})");
            let _ = writeln!(out, "model: not ready");
            let _ = writeln!(
                out,
                "context: input={} keep_recent_turns={}",
                settings.context.max_input_tokens, settings.context.keep_recent_turns
            );
        }
    }

    match collect_tool_inventory(settings) {
        Ok(inventory) => {
            let _ = writeln!(
                out,
                "tools: total={} builtin={} agent_cli={} skill={} mcp={} verification={} model={}",
                inventory.total,
                inventory.builtin,
                inventory.agent_cli,
                inventory.skill,
                inventory.mcp,
                inventory.verification,
                inventory.model
            );
        }
        Err(err) => {
            let _ = writeln!(out, "tools: error={err}");
        }
    }

    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    match store.graph_stats() {
        Ok(stats) => {
            let candidates = store
                .list_candidates()
                .map(|candidates| candidates.len())
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "memory: enabled={} facts={} candidates={} hnsw_nodes={} hnsw_edges={} vectors={}",
                settings.memory.enabled,
                stats.facts,
                candidates,
                stats.hnsw_nodes,
                stats.hnsw_edges,
                stats.vectors
            );
        }
        Err(err) => {
            let _ = writeln!(out, "memory: error={err}");
        }
    }

    let rules_dir = settings.pwcli_home.join("rules");
    match count_rule_files(&rules_dir) {
        Ok(count) => {
            let _ = writeln!(out, "rules: count={} dir={}", count, rules_dir.display());
        }
        Err(err) => {
            let _ = writeln!(out, "rules: error={err}");
        }
    }

    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    match runtime.list() {
        Ok(tasks) => {
            let counts = task_counts(&tasks);
            let _ = writeln!(
                out,
                "tasks: total={} running={} completed={} failed={} cancelled={} timed_out={}",
                tasks.len(),
                counts.get("running").copied().unwrap_or_default(),
                counts.get("completed").copied().unwrap_or_default(),
                counts.get("failed").copied().unwrap_or_default(),
                counts.get("cancelled").copied().unwrap_or_default(),
                counts.get("timed_out").copied().unwrap_or_default()
            );
            if let Ok(Some(task_id)) = runtime.active_task_id() {
                match runtime.get(&task_id) {
                    Ok(task) => {
                        let _ = writeln!(out, "active_task:");
                        for line in format_task_next(&task).lines() {
                            let _ = writeln!(out, "  {line}");
                        }
                    }
                    Err(err) => {
                        let _ = writeln!(out, "active_task: error={err}");
                    }
                }
            }
        }
        Err(err) => {
            let _ = writeln!(out, "tasks: error={err}");
        }
    }

    out
}

pub fn build_doctor_report(settings: &Settings) -> String {
    let mut checks = Vec::new();
    let paths = WorkspacePaths::from_pwcli_home(&settings.pwcli_home);
    match paths.ensure() {
        Ok(()) => checks.push(DiagnosticCheck::ok(
            "workspace",
            format!("ready at {}", paths.pwcli_home.display()),
        )),
        Err(err) => checks.push(DiagnosticCheck::fail(
            "workspace",
            format!("cannot prepare {}: {err}", paths.pwcli_home.display()),
        )),
    }

    let config_path = config_path(settings);
    if config_path.is_file() {
        checks.push(DiagnosticCheck::ok(
            "config",
            format!("found {}", config_path.display()),
        ));
    } else {
        checks.push(DiagnosticCheck::warn(
            "config",
            format!(
                "{} not found; run `pwcli init` to persist defaults",
                config_path.display()
            ),
        ));
    }

    match settings.active_provider() {
        Ok(provider) => {
            checks.push(DiagnosticCheck::ok(
                "provider",
                format!("{} ({})", provider.name, provider.protocol.as_str()),
            ));
            if provider_has_key(Some(provider)) {
                checks.push(DiagnosticCheck::ok(
                    "provider api key",
                    provider_key_source(provider),
                ));
            } else {
                checks.push(DiagnosticCheck::warn(
                    "provider api key",
                    format!(
                        "missing; set provider.api_key or {}",
                        provider
                            .api_key_env
                            .clone()
                            .unwrap_or_else(|| default_key_env_name(provider))
                    ),
                ));
            }
        }
        Err(err) => checks.push(DiagnosticCheck::warn("provider", err.to_string())),
    }

    match settings.active_model() {
        Ok(model) => checks.push(DiagnosticCheck::ok(
            "model",
            format!(
                "{} input={} output={} thinking={} image_input={}",
                model.name,
                model.max_input_tokens,
                model.max_output_tokens,
                model.supports_thinking,
                model.supports_image_input
            ),
        )),
        Err(err) => checks.push(DiagnosticCheck::warn("model", err.to_string())),
    }

    for root in &settings.skill_roots {
        if root.is_dir() {
            checks.push(DiagnosticCheck::ok(
                "skill root",
                format!("found {}", root.display()),
            ));
        } else {
            checks.push(DiagnosticCheck::info(
                "skill root",
                format!("{} does not exist yet", root.display()),
            ));
        }
    }

    match collect_tool_inventory(settings) {
        Ok(inventory) => checks.push(DiagnosticCheck::ok(
            "tools",
            format!(
                "total={} builtin={} agent_cli={} skill={} mcp={} verification={}",
                inventory.total,
                inventory.builtin,
                inventory.agent_cli,
                inventory.skill,
                inventory.mcp,
                inventory.verification
            ),
        )),
        Err(err) => checks.push(DiagnosticCheck::fail("tools", err.to_string())),
    }

    checks.extend(tool_health_checks(settings));

    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    match store.graph_stats() {
        Ok(stats) => checks.push(DiagnosticCheck::ok(
            "memory",
            format!(
                "enabled={} facts={} hnsw_nodes={} vectors={} embedding_model={}",
                settings.memory.enabled,
                stats.facts,
                stats.hnsw_nodes,
                stats.vectors,
                settings.memory.embedding.model
            ),
        )),
        Err(err) => checks.push(DiagnosticCheck::fail("memory", err.to_string())),
    }

    match count_rule_files(&settings.pwcli_home.join("rules")) {
        Ok(count) if count > 0 => checks.push(DiagnosticCheck::ok(
            "rules",
            format!("{count} rule file(s) loaded from ~/.pwcli/rules"),
        )),
        Ok(_) => checks.push(DiagnosticCheck::info(
            "rules",
            "no rule files yet; use `pwcli rules add <name> <text>`",
        )),
        Err(err) => checks.push(DiagnosticCheck::warn("rules", err.to_string())),
    }

    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    match runtime.list() {
        Ok(tasks) => checks.push(DiagnosticCheck::ok(
            "runtime",
            format!(
                "tasks={} running={}",
                tasks.len(),
                running_task_count(&tasks)
            ),
        )),
        Err(err) => checks.push(DiagnosticCheck::fail("runtime", err.to_string())),
    }

    format_doctor_checks(&checks)
}

fn collect_tool_inventory(settings: &Settings) -> crate::Result<ToolInventory> {
    let mut tools = Vec::new();
    tools.extend(BuiltinToolLoader.load()?);
    tools.extend(SkillToolLoader::new(settings.skill_roots.clone()).load()?);
    tools.extend(McpToolLoader::new(settings.mcp.clone(), settings.pwcli_home.clone()).load()?);
    tools.extend(VerificationToolLoader.load()?);
    let tools = apply_tool_settings(tools, &settings.tools);
    Ok(tool_inventory(&tools))
}

fn tool_inventory(tools: &[LoadedTool]) -> ToolInventory {
    let mut inventory = ToolInventory {
        total: tools.len(),
        ..ToolInventory::default()
    };
    for tool in tools {
        match &tool.descriptor.source {
            ToolSource::Builtin => inventory.builtin += 1,
            ToolSource::AgentCli { .. } => inventory.agent_cli += 1,
            ToolSource::Skill { .. } => inventory.skill += 1,
            ToolSource::Mcp { .. } => inventory.mcp += 1,
            ToolSource::Verification => inventory.verification += 1,
            ToolSource::Model => inventory.model += 1,
        }
    }
    inventory
}

fn tool_health_checks(settings: &Settings) -> Vec<DiagnosticCheck> {
    build_tool_health_report(settings)
        .checks
        .into_iter()
        .map(|check| match check.status {
            ToolHealthStatus::Ok => DiagnosticCheck::ok(check.label, check.detail),
            ToolHealthStatus::Warn => DiagnosticCheck::warn(check.label, check.detail),
            ToolHealthStatus::Fail => DiagnosticCheck::fail(check.label, check.detail),
            ToolHealthStatus::Info => DiagnosticCheck::info(check.label, check.detail),
        })
        .collect()
}

fn format_doctor_checks(checks: &[DiagnosticCheck]) -> String {
    let mut out = String::new();
    let mut ok = 0;
    let mut warn = 0;
    let mut fail = 0;
    let mut info = 0;
    let _ = writeln!(out, "pwcli doctor");
    for check in checks {
        let tag = match check.level {
            CheckLevel::Ok => {
                ok += 1;
                "ok"
            }
            CheckLevel::Warn => {
                warn += 1;
                "warn"
            }
            CheckLevel::Fail => {
                fail += 1;
                "fail"
            }
            CheckLevel::Info => {
                info += 1;
                "info"
            }
        };
        let _ = writeln!(out, "[{tag}] {}: {}", check.label, check.detail);
    }
    let _ = writeln!(out, "summary: ok={ok} warn={warn} fail={fail} info={info}");
    out
}

fn provider_has_key(provider: Option<&ProviderSettings>) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    let env_name = provider
        .api_key_env
        .clone()
        .unwrap_or_else(|| default_key_env_name(provider));
    provider
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
        || env::var_os(env_name).is_some()
}

fn provider_key_source(provider: &ProviderSettings) -> String {
    if provider
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
    {
        return "configured in ~/.pwcli/config.json".to_string();
    }
    let env_name = provider
        .api_key_env
        .clone()
        .unwrap_or_else(|| default_key_env_name(provider));
    format!("configured through {env_name}")
}

fn default_key_env_name(provider: &ProviderSettings) -> String {
    match provider.protocol {
        crate::settings::ProviderProtocol::OpenAi => "OPENAI_API_KEY".to_string(),
        crate::settings::ProviderProtocol::Anthropic => "ANTHROPIC_API_KEY".to_string(),
        crate::settings::ProviderProtocol::Nvidia => "NVIDIA_API_KEY".to_string(),
    }
}

fn task_counts(tasks: &[crate::runtime::RuntimeTask]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for task in tasks {
        let key = match task.status {
            RuntimeTaskStatus::Pending => "pending",
            RuntimeTaskStatus::Running => "running",
            RuntimeTaskStatus::Completed => "completed",
            RuntimeTaskStatus::Failed => "failed",
            RuntimeTaskStatus::Cancelled => "cancelled",
            RuntimeTaskStatus::TimedOut => "timed_out",
        };
        *counts.entry(key).or_default() += 1;
    }
    counts
}

fn running_task_count(tasks: &[crate::runtime::RuntimeTask]) -> usize {
    tasks
        .iter()
        .filter(|task| task.status == RuntimeTaskStatus::Running)
        .count()
}

fn count_rule_files(dir: &Path) -> std::io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("md" | "txt")
            )
        {
            count += 1;
        }
    }
    Ok(count)
}

fn config_path(settings: &Settings) -> PathBuf {
    settings.pwcli_home.join("config.json")
}
