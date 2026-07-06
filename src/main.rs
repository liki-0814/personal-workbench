use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use std::time::{SystemTime, UNIX_EPOCH};

use pwcli::{
    audit::{
        format_audit_summary, format_audit_tail, read_audit_events, summarize_events, AuditEvent,
        AuditRecorder, JsonlAuditRecorder,
    },
    context::ContextBuilder,
    diagnostics::{build_doctor_report, build_status_report},
    graph::{
        GraphEvent, GraphEventSink, GraphExecutor, GraphRunRequest, GraphRunServices,
        GraphWorkflow, PlannedToolCallPlanner, StreamingModelPlanner, WorkflowContext,
        WorkflowEdgeCondition, WorkflowExecutor, WorkflowNode, WorkflowNodeKind,
        WorkflowNodeRunner, WorkflowPlanKind, WorkflowRunSummary, WorkflowStatus,
        WorkflowStepOutcome,
    },
    memory::{MemoryStore, SemanticMemoryExtraction},
    policy::{DefaultPolicyGuard, PolicyDecision, PolicyGuard, UserApproval},
    runtime::{
        format_task_next, review_required, verification_gate, verification_passed, CompactScope,
        RuntimeTask, RuntimeTaskEvent, RuntimeTaskKind, RuntimeTaskManager, RuntimeTaskStatus,
        VerificationRecord,
    },
    service::{serve as serve_web_workbench, ServeOptions},
    session::{format_session_list, format_session_record, SessionStore},
    settings::{
        McpServerSettings, McpTransportKind, ModelDefinition, OpenAiApiKind, ProviderProtocol,
        ProviderSettings, Settings,
    },
    storage::{write_json, WorkspacePaths},
    tools::{
        agent_cli::{build_runtime_task_spec, AgentCliArgs, AgentCliKind},
        builtin::BuiltinToolLoader,
        config::apply_tool_settings,
        health::build_tool_health_report,
        mcp::McpToolLoader,
        model::{
            AnyModelClient, ModelClient, ModelEvent, ModelMessage, ModelRequest, ModelRole,
            ThinkingConfig,
        },
        skills::{watcher::scan_skill_roots, SkillToolLoader},
        verification::{
            legacy_verification_report, verification_report_from_metadata,
            VerificationGateDecision, VerificationToolLoader,
        },
        InvocationMode, LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolExecutionContext,
        ToolExecutionMode, ToolExecutionRuntime, ToolExecutor, ToolLoader, ToolRegistry,
        ToolRegistrySnapshot, ToolResult, ToolRuntimeEvent, ToolSource,
    },
    tui::{StdinPrompter, TerminalUi},
    PwError, Result,
};
use serde::{Deserialize, Serialize};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command = args.first().map(String::as_str).unwrap_or("interactive");

    match command {
        "__runtime-task" => run_runtime_task_worker(&args[1..]),
        "interactive" => interactive(),
        "init" => init(),
        "tools" | "tool" => run_tools_cli(&args[1..]),
        "mcp" => run_mcp_cli(&args[1..]),
        "skill" | "skills" => run_skill_cli(&args[1..]),
        "context" => run_context_cli(&args[1..]),
        "audit" => run_audit_cli(&args[1..]),
        "verify" => run_verify_cli(&args[1..]),
        "memory" => run_memory_cli(&args[1..]),
        "rules" | "rule" => run_rules_cli(&args[1..]),
        "agent" => run_agent_cli(&args[1..]),
        "goal" => run_goal_cli(&args[1..]),
        "plan" => run_agent_mode_cli("plan", false, &args[1..]),
        "loop" => run_agent_mode_cli("execute", true, &args[1..]),
        "review" => run_agent_mode_cli("review", false, &args[1..]),
        "next" => run_task_next_cli(&args[1..]),
        "task" => run_task_cli(&args[1..]),
        "workflow" | "wf" => run_workflow_cli(&args[1..]),
        "session" | "sessions" => run_session_cli(&args[1..]),
        "serve" | "server" => run_serve_cli(&args[1..]),
        "status" => print_status(),
        "doctor" => print_doctor(),
        "config" => run_config_cli(&args[1..]),
        "providers" => run_providers_cli(),
        "provider" => run_provider_cli(&args[1..]),
        "models" => run_models_cli(),
        "model" => run_model_cli(&args[1..]),
        "thinking" | "think" => run_thinking_cli(&args[1..]),
        "self-test" if args.get(1).map(String::as_str) == Some("ask-user") => self_test_ask_user(),
        "run" => run_prompt_cli(&args[1..]),
        "help" | "--help" | "-h" => {
            print_help(args.get(1).map(String::as_str));
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            print_help(None);
            Ok(())
        }
    }
}

fn run_runtime_task_worker(args: &[String]) -> Result<()> {
    let Some(pwcli_home) = args.first() else {
        return Err(pwcli::PwError::Message(
            "usage: pwcli __runtime-task <pwcli_home> <task_id>".to_string(),
        ));
    };
    let Some(task_id) = args.get(1) else {
        return Err(pwcli::PwError::Message(
            "usage: pwcli __runtime-task <pwcli_home> <task_id>".to_string(),
        ));
    };
    let runtime = RuntimeTaskManager::new(pwcli_home);
    runtime.run_persisted_task(task_id)
}

fn run_serve_cli(args: &[String]) -> Result<()> {
    let options = parse_serve_options(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            pwcli::PwError::Message(format!("failed to start service runtime: {err}"))
        })?;
    runtime.block_on(serve_web_workbench(options))
}

fn parse_serve_options(args: &[String]) -> Result<ServeOptions> {
    let mut options = ServeOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--host" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli serve [--host <host>] [--port <port>] [--open] [--no-ui]"
                            .to_string(),
                    ));
                };
                options.host = value.clone();
                idx += 2;
            }
            value if value.starts_with("--host=") => {
                options.host = value.trim_start_matches("--host=").to_string();
                idx += 1;
            }
            "--port" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli serve --port <port>".to_string(),
                    ));
                };
                options.port = value
                    .parse()
                    .map_err(|_| pwcli::PwError::Message("port must be an integer".to_string()))?;
                idx += 2;
            }
            value if value.starts_with("--port=") => {
                options.port = value
                    .trim_start_matches("--port=")
                    .parse()
                    .map_err(|_| pwcli::PwError::Message("port must be an integer".to_string()))?;
                idx += 1;
            }
            "--open" => {
                options.open = true;
                idx += 1;
            }
            "--no-ui" => {
                options.no_ui = true;
                idx += 1;
            }
            "--no-reload-skills" => {
                options.reload_skills = false;
                idx += 1;
            }
            "--help" | "-h" => {
                return Err(pwcli::PwError::Message(serve_help_text().to_string()));
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown serve option '{other}'\n{}",
                    serve_help_text()
                )));
            }
        }
    }
    Ok(options)
}

fn interactive() -> Result<()> {
    let mut settings = Settings::load()?;
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    let mut ui = TerminalUi::start()?;
    ui.push_status("Tip: /help for commands. Click inside the input box to move the cursor.");

    let mut suggestion_engine = SuggestionEngine::new();
    let mut suggest = |input: &str, settings: &Settings| suggestion_engine.suggest(input, settings);
    let mut active_task: Option<String> = runtime.active_task_id().ok().flatten();
    let mut active_session: Option<String> = None;
    let mut runtime_event_tailer = RuntimeEventTailer::default();
    let mut skill_inventory_tailer = SkillInventoryTailer::default();
    let skill_roots = settings.skill_roots.clone();
    let mut on_idle = || {
        let mut messages = runtime_status_messages(&runtime, &mut runtime_event_tailer);
        messages.extend(skill_status_messages(
            &skill_roots,
            &mut skill_inventory_tailer,
        ));
        messages
    };
    while let Some(input) = ui.read_line(&settings, &mut suggest, &mut on_idle)? {
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "/exit" | "/quit" => break,
            "/help" => {
                ui.push_status(interactive_help_text());
                continue;
            }
            "/status" => {
                ui.push_status(build_status_report(&settings));
                continue;
            }
            "/doctor" => {
                ui.push_status(build_doctor_report(&settings));
                continue;
            }
            "/config" => {
                ui.push_status(config_command_text(&[], &mut settings)?);
                continue;
            }
            "/audit" => {
                handle_interactive_audit_command("", &settings, &mut ui);
                continue;
            }
            "/verify" => {
                ui.push_status(
                    "use /verify to auto-detect project checks, or /verify --cmd \"cargo check\"",
                );
                continue;
            }
            "/sessions" => {
                handle_interactive_session_command("list", &settings, &mut active_session, &mut ui);
                continue;
            }
            "/models" => {
                ui.push_status(models_text(&settings));
                continue;
            }
            "/agent" | "/agents" => {
                ui.push_status(agent_inventory_text());
                continue;
            }
            "/providers" => {
                ui.push_status(providers_text(&settings));
                continue;
            }
            "/provider" => {
                ui.push_status(format!(
                    "current provider: {}\nuse /provider <name> to switch",
                    settings.provider
                ));
                continue;
            }
            "/model" => {
                ui.push_status(format!(
                    "current model: {}\nuse /model <name> to switch",
                    settings.model
                ));
                continue;
            }
            "/context" => {
                ui.push_status(format!(
                    "context max input tokens: {}\nuse /context <tokens> to change",
                    settings.context.max_input_tokens
                ));
                continue;
            }
            "/next" => {
                match task_next_text(&runtime, active_task.as_deref()) {
                    Ok(text) => ui.push_status(text),
                    Err(err) => ui.push_status(format!("task next failed: {err}")),
                }
                continue;
            }
            "/context pack" => {
                ui.push_status("usage: /context pack <prompt>");
                continue;
            }
            "/compact" => {
                if let Some(task_id) = active_task.as_deref() {
                    match runtime.compact(task_id, CompactScope::Both) {
                        Ok(summary) => {
                            let mut status = format!(
                                "compacted task {} -> {}",
                                task_id,
                                summary.summary_path.display()
                            );
                            if let Ok(Some(candidate_id)) = create_memory_candidate_from_text(
                                &settings,
                                &summary.content,
                                "runtime compact summary",
                            ) {
                                status.push_str(&format!(
                                    "\nmemory candidate created: {candidate_id}\nuse /memory inbox then /memory accept {candidate_id}"
                                ));
                            }
                            ui.push_status(status);
                        }
                        Err(err) => ui.push_status(format!("compact failed: {err}")),
                    }
                } else {
                    ui.push_status("no active task; use /goal <objective> first");
                }
                continue;
            }
            "/thinking" | "/think" => {
                ui.push_status(format!(
                    "thinking: {}\nuse /thinking on or /thinking off",
                    if settings.thinking { "on" } else { "off" }
                ));
                continue;
            }
            _ => {}
        }

        if let Some(topic) = input.strip_prefix("/help ") {
            ui.push_status(interactive_topic_help_text(topic.trim()));
            continue;
        }

        if let Some(goal) = input.strip_prefix("/goal ") {
            handle_interactive_goal_command(goal.trim(), &runtime, &mut active_task, &mut ui);
            continue;
        }

        if input == "/goal" {
            ui.push_status("usage: /goal <objective>");
            continue;
        }

        if let Some(rest) = input.strip_prefix("/plan ") {
            handle_interactive_agent_mode(
                "plan",
                false,
                rest,
                &settings,
                &runtime,
                &mut active_task,
                &mut ui,
            );
            continue;
        }
        if input == "/plan" {
            ui.push_status("usage: /plan [codex|claude|agy|qodercli] <prompt>");
            continue;
        }

        if let Some(rest) = input.strip_prefix("/loop ") {
            handle_interactive_agent_mode(
                "execute",
                true,
                rest,
                &settings,
                &runtime,
                &mut active_task,
                &mut ui,
            );
            continue;
        }
        if input == "/loop" {
            ui.push_status("usage: /loop [codex|claude|agy|qodercli] <prompt>");
            continue;
        }

        if let Some(rest) = input.strip_prefix("/review ") {
            handle_interactive_agent_mode(
                "review",
                false,
                rest,
                &settings,
                &runtime,
                &mut active_task,
                &mut ui,
            );
            continue;
        }
        if input == "/review" {
            ui.push_status("usage: /review [codex|claude|agy|qodercli] <prompt>");
            continue;
        }

        if let Some(rest) = input.strip_prefix("/context pack ") {
            handle_interactive_context_pack(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/audit ") {
            handle_interactive_audit_command(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/config ") {
            match config_command_text(&split_command_line(rest), &mut settings) {
                Ok(text) => ui.push_status(text),
                Err(err) => ui.push_status(format!("config failed: {err}")),
            }
            continue;
        }

        if input == "/tools" {
            handle_interactive_tools_command("", &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/tools ") {
            handle_interactive_tools_command(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/task ") {
            handle_interactive_task_command(rest, &settings, &runtime, &mut active_task, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/workflow ") {
            handle_interactive_workflow_command(
                rest,
                &settings,
                &runtime,
                &mut active_task,
                &mut ui,
            );
            continue;
        }
        if input == "/workflow" {
            ui.push_status(workflow_help_text().replace("pwcli ", "/"));
            continue;
        }

        if let Some(rest) = input.strip_prefix("/session ") {
            handle_interactive_session_command(rest, &settings, &mut active_session, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/memory") {
            handle_interactive_memory_command(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/rules") {
            handle_interactive_rules_command(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/verify") {
            handle_interactive_verify_command(rest.trim(), &settings, &mut ui);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/agent ") {
            let mut parts = rest
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if let Some(task_id) = active_task.as_ref().filter(|_| {
                !parts
                    .iter()
                    .any(|part| part == "--task" || part.starts_with("--task="))
            }) {
                parts.insert(1.min(parts.len()), task_id.clone());
                parts.insert(1, "--task".to_string());
            }
            match spawn_agent_task_with_runtime(&parts, &settings, &runtime, false, false) {
                Ok(task_id) => {
                    active_task = Some(task_id.clone());
                    ui.push_status(format!("agent task started: {task_id}"));
                }
                Err(err) => ui.push_status(format!("agent task failed: {err}")),
            }
            continue;
        }

        if let Some(provider) = input.strip_prefix("/provider ") {
            let provider = provider.trim();
            if provider.is_empty() {
                println!("usage: /provider <name>");
                continue;
            }
            match settings.set_provider(provider) {
                Ok(()) => {
                    save_normalized_settings(&mut settings)?;
                    ui.push_status(format!(
                        "provider switched to {}\nmodel: {}",
                        settings.provider, settings.model
                    ));
                }
                Err(err) => {
                    ui.push_status(format!("{err}\n{}", providers_text(&settings)));
                }
            }
            continue;
        }

        if let Some(model) = input.strip_prefix("/model ") {
            let model = model.trim();
            if model.is_empty() {
                println!("usage: /model <name>");
                continue;
            }
            match settings.set_model(model) {
                Ok(()) => {
                    save_normalized_settings(&mut settings)?;
                    ui.push_status(format!("model switched to {}", settings.model));
                }
                Err(err) => {
                    ui.push_status(format!("{err}\n{}", models_text(&settings)));
                }
            }
            continue;
        }

        if let Some(tokens) = input.strip_prefix("/context ") {
            match tokens.trim().parse::<u32>() {
                Ok(tokens) => {
                    settings.set_context_max_input_tokens(tokens);
                    save_normalized_settings(&mut settings)?;
                    ui.push_status(format!(
                        "context max input tokens set to {}",
                        settings.context.max_input_tokens
                    ));
                }
                Err(_) => ui.push_status("usage: /context <tokens>"),
            }
            continue;
        }

        if let Some(value) = input
            .strip_prefix("/thinking ")
            .or_else(|| input.strip_prefix("/think "))
        {
            match value.trim() {
                "on" | "true" | "1" | "yes" => {
                    settings.set_thinking(true);
                    save_normalized_settings(&mut settings)?;
                    ui.push_status("thinking enabled");
                }
                "off" | "false" | "0" | "no" => {
                    settings.set_thinking(false);
                    save_normalized_settings(&mut settings)?;
                    ui.push_status("thinking disabled");
                }
                _ => ui.push_status("usage: /thinking on|off"),
            }
            continue;
        }

        let prompt = input.strip_prefix("/run ").unwrap_or(input).trim();
        if prompt.is_empty() {
            continue;
        }
        ui.push_user(prompt);
        ui.start_working(&settings)?;
        let (event_tx, event_rx) = mpsc::channel::<PromptWorkerEvent>();
        let worker_prompt = prompt.to_string();
        let worker_settings = settings.clone();
        let worker_session = active_session.clone();
        thread::spawn(move || {
            let approval = TuiApproval {
                event_tx: event_tx.clone(),
            };
            let output_tx = event_tx.clone();
            let mut output = |delta: &str| {
                let _ = output_tx.send(PromptWorkerEvent::Delta(delta.to_string()));
            };
            match run_prompt_with_settings_output_with_approval(
                worker_prompt,
                &worker_settings,
                worker_session,
                &mut output,
                &approval,
            ) {
                Ok(session_id) => {
                    let _ = event_tx.send(PromptWorkerEvent::Done { session_id });
                }
                Err(err) => {
                    let _ = event_tx.send(PromptWorkerEvent::Error(err.to_string()));
                }
            }
        });

        let mut done = false;
        let mut has_output = false;
        while !done {
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    PromptWorkerEvent::Delta(delta) => {
                        if !has_output {
                            ui.stop_working(&settings)?;
                            ui.start_assistant_message();
                            has_output = true;
                        }
                        ui.append_assistant_delta(&delta, &settings)?;
                    }
                    PromptWorkerEvent::Done { session_id } => {
                        active_session = Some(session_id.clone());
                        ui.stop_working(&settings)?;
                        ui.push_status(format!("session saved: {session_id}"));
                        done = true;
                    }
                    PromptWorkerEvent::AskUser {
                        prompt,
                        tool_name,
                        response_tx,
                    } => {
                        ui.stop_working(&settings)?;
                        let allowed =
                            ui.confirm(&format!("{prompt}\n\ntool: {tool_name}"), &settings)?;
                        let _ = response_tx.send(allowed);
                        ui.push_status(if allowed {
                            "approval: allowed"
                        } else {
                            "approval: rejected"
                        });
                        if !has_output {
                            ui.start_working(&settings)?;
                        } else {
                            ui.draw(&settings)?;
                        }
                    }
                    PromptWorkerEvent::Error(err) => {
                        ui.stop_working(&settings)?;
                        ui.push_status(format!("error: {err}"));
                        done = true;
                    }
                }
            }
            if done {
                break;
            }
            if ui.poll_working_interrupt()? {
                ui.stop_working(&settings)?;
                ui.push_status("interrupted: response hidden; provider request may still finish in the background");
                break;
            }
            ui.tick_working(&settings)?;
        }
    }

    Ok(())
}

fn init() -> Result<()> {
    let mut settings = Settings::load()?;
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    save_normalized_settings(&mut settings)?;
    println!("initialized {}", settings.pwcli_home.display());
    Ok(())
}

fn list_tools() -> Result<()> {
    let settings = Settings::load()?;
    let (registry, _) = build_registry(&settings)?;
    let snapshot = registry.snapshot();
    print!("{}", format_tools_list(&snapshot));
    Ok(())
}

fn run_tools_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") | Some("ls") => list_tools(),
        Some("show") | Some("describe") => show_tool(&args[1..]),
        Some("call") => call_tool(&args[1..]),
        Some("enable") => set_tool_enabled(&args[1..], true),
        Some("disable") => set_tool_enabled(&args[1..], false),
        Some("doctor") | Some("health") => tool_doctor(),
        Some("reload") => tool_reload(),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{}", help_text(Some("tools")));
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown tools command: {other}");
            eprintln!("{}", help_text(Some("tools")));
            Ok(())
        }
    }
}

fn set_tool_enabled(args: &[String], enabled: bool) -> Result<()> {
    let Some(tool_id) = args.first() else {
        eprintln!(
            "usage: pwcli tools {} <tool_id-or-pattern>",
            if enabled { "enable" } else { "disable" }
        );
        return Ok(());
    };
    let mut settings = Settings::load()?;
    set_tool_enabled_in_settings(&mut settings, tool_id, enabled);
    save_normalized_settings(&mut settings)?;
    println!(
        "tool {} {}",
        tool_id,
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn set_tool_enabled_in_settings(settings: &mut Settings, tool_id: &str, enabled: bool) {
    if enabled {
        settings.tools.disabled.retain(|item| item != tool_id);
        settings.tools.denylist.retain(|item| item != tool_id);
    } else if !settings.tools.disabled.iter().any(|item| item == tool_id) {
        settings.tools.disabled.push(tool_id.to_string());
    }
}

fn tool_doctor() -> Result<()> {
    let settings = Settings::load()?;
    println!("{}", format_tool_health_report(&settings));
    Ok(())
}

fn tool_reload() -> Result<()> {
    let settings = Settings::load()?;
    let (registry, loaded_skills) = build_registry(&settings)?;
    let snapshot = registry.snapshot();
    println!(
        "tools reloaded: registry_version={} tools={} skills={}",
        snapshot.version(),
        snapshot.descriptors().len(),
        loaded_skills
    );
    let inventory = scan_skill_roots(&settings.skill_roots)?;
    if !inventory.conflicts.is_empty() {
        println!("skill conflicts:");
        for conflict in inventory.conflicts {
            println!(
                "- {} {}",
                conflict.tool_id,
                conflict
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

fn format_tool_health_report(settings: &Settings) -> String {
    let report = build_tool_health_report(settings);
    let (ok, warn, fail, info) = report.counts();
    let mut out = format!(
        "Tool Health\nok={ok} warn={warn} fail={fail} info={info} generated_at={}\n",
        report.generated_at
    );
    for check in report.checks {
        out.push_str(&format!(
            "- {:?}\t{}\t{}\n",
            check.status, check.label, check.detail
        ));
    }
    out
}

fn show_tool(args: &[String]) -> Result<()> {
    let Some(tool_id) = args.first() else {
        eprintln!("usage: pwcli tools show <tool_id>");
        return Ok(());
    };
    let settings = Settings::load()?;
    let (registry, _) = build_registry(&settings)?;
    let snapshot = registry.snapshot();
    let descriptor = snapshot
        .get(tool_id)
        .ok_or_else(|| pwcli::PwError::ToolNotFound(tool_id.clone()))?
        .descriptor
        .clone();
    println!("{}", format_tool_descriptor(&descriptor));
    Ok(())
}

fn format_tools_list(snapshot: &ToolRegistrySnapshot) -> String {
    let mut descriptors = snapshot.descriptors();
    descriptors.sort_by(|a, b| a.id.cmp(&b.id));
    if descriptors.is_empty() {
        return "no tools\n".to_string();
    }
    let mut out = String::new();
    for descriptor in descriptors {
        out.push_str(&format!(
            "{}\t{}\t{:?}\trisk={:?}\n",
            descriptor.id, descriptor.name, descriptor.source, descriptor.risk_level
        ));
    }
    out
}

fn format_tool_descriptor(descriptor: &ToolDescriptor) -> String {
    format!(
        "id: {}\nname: {}\ndescription: {}\nsource: {:?}\nrisk: {:?}\ninvocation: {:?}\nenabled: {}\ncapabilities: {}\n\ninput_schema:\n{}\n\nmetadata:\n{}",
        descriptor.id,
        descriptor.name,
        descriptor.description,
        descriptor.source,
        descriptor.risk_level,
        descriptor.invocation_mode,
        descriptor.enabled,
        if descriptor.capabilities.is_empty() {
            "-".to_string()
        } else {
            descriptor.capabilities.join(", ")
        },
        serde_json::to_string_pretty(&descriptor.input_schema)
            .unwrap_or_else(|_| descriptor.input_schema.to_string()),
        serde_json::to_string_pretty(&descriptor.metadata)
            .unwrap_or_else(|_| descriptor.metadata.to_string())
    )
}

fn call_tool(args: &[String]) -> Result<()> {
    let Some(tool_id) = args.first() else {
        eprintln!("{}", help_text(Some("tools")));
        return Ok(());
    };
    let arguments = if args.len() > 1 {
        serde_json::from_str::<serde_json::Value>(&args[1..].join(" "))?
    } else {
        serde_json::json!({})
    };
    let settings = Settings::load()?;
    let (registry, _) = build_registry(&settings)?;
    let snapshot = registry.snapshot();
    let descriptor = snapshot
        .get(tool_id)
        .ok_or_else(|| pwcli::PwError::ToolNotFound(tool_id.clone()))?
        .descriptor
        .clone();
    let call = ToolCall {
        id: format!(
            "manual-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default()
        ),
        tool_id: tool_id.clone(),
        name: descriptor.name,
        arguments,
    };
    let result = execute_manual_tool_call(&settings, &snapshot, &call)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_mcp_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("list") | Some("ls") => mcp_list(),
        Some("add") => mcp_add(&args[1..]),
        Some("remove") | Some("rm") | Some("delete") => mcp_remove(&args[1..]),
        Some("doctor") | Some("health") => tool_doctor(),
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", mcp_help_text());
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown mcp command: {other}");
            eprintln!("{}", mcp_help_text());
            Ok(())
        }
    }
}

fn mcp_list() -> Result<()> {
    let settings = Settings::load()?;
    if settings.mcp.servers.is_empty() {
        println!("no MCP servers configured");
        return Ok(());
    }
    for server in settings.mcp.servers {
        println!(
            "{}\tenabled={}\ttransport={:?}\tendpoint={}",
            server.name,
            server.enabled,
            server.transport,
            server
                .url
                .as_deref()
                .or(server.command.as_deref())
                .unwrap_or("-")
        );
    }
    Ok(())
}

fn mcp_add(args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        eprintln!("{}", mcp_help_text());
        return Ok(());
    };
    let mut server = McpServerSettings {
        name: name.clone(),
        ..McpServerSettings::default()
    };
    let mut idx = 1;
    while idx < args.len() {
        match args[idx].as_str() {
            "--transport" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --transport <stdio|http|sse>".to_string(),
                    ));
                };
                server.transport = parse_mcp_transport(value)?;
                idx += 2;
            }
            "--command" | "--cmd" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --command <binary>".to_string(),
                    ));
                };
                server.command = Some(value.clone());
                server.transport = McpTransportKind::Stdio;
                idx += 2;
            }
            "--arg" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --arg <value>".to_string(),
                    ));
                };
                server.args.push(value.clone());
                idx += 2;
            }
            "--url" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --url <url>".to_string(),
                    ));
                };
                server.url = Some(value.clone());
                if matches!(server.transport, McpTransportKind::Stdio) {
                    server.transport = McpTransportKind::Http;
                }
                idx += 2;
            }
            "--env" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --env KEY=VALUE".to_string(),
                    ));
                };
                let (key, val) = split_key_value(value)?;
                server.env.insert(key, val);
                idx += 2;
            }
            "--header" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --header KEY=VALUE".to_string(),
                    ));
                };
                let (key, val) = split_key_value(value)?;
                server.headers.insert(key, val);
                idx += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --timeout <seconds>".to_string(),
                    ));
                };
                server.timeout_seconds = value.parse().map_err(|_| {
                    pwcli::PwError::Message("MCP timeout must be an integer".to_string())
                })?;
                idx += 2;
            }
            "--risk" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli mcp add <name> --risk <read_only|low|medium|high>"
                            .to_string(),
                    ));
                };
                server.risk_level = Some(value.clone());
                idx += 2;
            }
            "--disabled" => {
                server.enabled = false;
                idx += 1;
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown mcp add option '{other}'"
                )));
            }
        }
    }

    if server.command.is_none() && server.url.is_none() {
        return Err(pwcli::PwError::Message(
            "mcp add requires --command for stdio or --url for http/sse".to_string(),
        ));
    }

    let mut settings = Settings::load()?;
    settings
        .mcp
        .servers
        .retain(|existing| existing.name != server.name);
    settings.mcp.servers.push(server);
    save_normalized_settings(&mut settings)?;
    println!("mcp server '{}' saved", name);
    Ok(())
}

fn mcp_remove(args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        eprintln!("usage: pwcli mcp remove <name>");
        return Ok(());
    };
    let mut settings = Settings::load()?;
    let before = settings.mcp.servers.len();
    settings.mcp.servers.retain(|server| server.name != *name);
    save_normalized_settings(&mut settings)?;
    if settings.mcp.servers.len() == before {
        println!("mcp server '{}' was not configured", name);
    } else {
        println!("mcp server '{}' removed", name);
    }
    Ok(())
}

fn parse_mcp_transport(value: &str) -> Result<McpTransportKind> {
    match value {
        "stdio" => Ok(McpTransportKind::Stdio),
        "http" => Ok(McpTransportKind::Http),
        "sse" => Ok(McpTransportKind::Sse),
        other => Err(pwcli::PwError::Message(format!(
            "unknown MCP transport '{other}'"
        ))),
    }
}

fn split_key_value(value: &str) -> Result<(String, String)> {
    let Some((key, val)) = value.split_once('=') else {
        return Err(pwcli::PwError::Message(format!(
            "expected KEY=VALUE, got '{value}'"
        )));
    };
    Ok((key.to_string(), val.to_string()))
}

fn run_skill_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("list") | Some("ls") => skill_list(),
        Some("reload") => skill_reload(),
        Some("doctor") | Some("health") => skill_doctor(),
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", skill_help_text());
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown skill command: {other}");
            eprintln!("{}", skill_help_text());
            Ok(())
        }
    }
}

fn skill_list() -> Result<()> {
    let settings = Settings::load()?;
    let inventory = scan_skill_roots(&settings.skill_roots)?;
    if inventory.tool_ids.is_empty() {
        println!("no skills found in {}", settings.skill_roots[0].display());
    } else {
        for tool_id in inventory.tool_ids {
            println!("{tool_id}");
        }
    }
    Ok(())
}

fn skill_reload() -> Result<()> {
    let settings = Settings::load()?;
    let inventory = scan_skill_roots(&settings.skill_roots)?;
    println!(
        "skills reloaded: tools={} conflicts={} health={}",
        inventory.tool_ids.len(),
        inventory.conflicts.len(),
        inventory.health.len()
    );
    Ok(())
}

fn skill_doctor() -> Result<()> {
    let settings = Settings::load()?;
    let inventory = scan_skill_roots(&settings.skill_roots)?;
    println!("Skill Doctor");
    for root in &inventory.roots {
        println!("root: {}", root.display());
    }
    for health in inventory.health {
        println!(
            "- {:?}\t{}\t{}",
            health.status,
            health.path.display(),
            health.message
        );
    }
    for conflict in inventory.conflicts {
        println!(
            "- conflict {}\t{}",
            conflict.tool_id,
            conflict
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn run_context_cli(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("usage: pwcli context <prompt>");
        return Ok(());
    }
    let settings = Settings::load()?;
    println!("{}", context_preview_text(&args.join(" "), &settings)?);
    Ok(())
}

fn context_preview_text(prompt: &str, settings: &Settings) -> Result<String> {
    let (registry, loaded_skills) = build_registry(settings)?;
    let snapshot = registry.snapshot();
    let local_paths = default_local_context_paths();
    let context_pack = ContextBuilder::new().build_with_sources_and_memory(
        prompt.to_string(),
        &snapshot,
        Some(settings.pwcli_home.clone()),
        local_paths,
        &settings.memory,
    );
    Ok(format_context_pack(&context_pack, &snapshot, loaded_skills))
}

fn default_local_context_paths() -> Vec<std::path::PathBuf> {
    ["AGENTS.md", "README.md"]
        .iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
        .collect()
}

fn format_context_pack(
    context_pack: &pwcli::context::ContextPack,
    snapshot: &pwcli::tools::ToolRegistrySnapshot,
    loaded_skills: usize,
) -> String {
    let descriptors = snapshot.descriptors();
    let mut out = String::new();
    out.push_str("Context Pack\n");
    out.push_str(&format!("id: {}\n", context_pack.id));
    out.push_str(&format!("registry_version: {}\n", snapshot.version()));
    out.push_str(&format!("loaded_skills: {loaded_skills}\n"));
    out.push_str(&format!("prompt: {}\n\n", context_pack.user_input));

    out.push_str(&format!(
        "selected_tools ({}):\n",
        context_pack.selected_tool_ids.len()
    ));
    if context_pack.selected_tool_ids.is_empty() {
        out.push_str("- none\n");
    } else {
        for tool_id in &context_pack.selected_tool_ids {
            if let Some(descriptor) = descriptors
                .iter()
                .find(|descriptor| descriptor.id.as_str() == tool_id.as_str())
            {
                out.push_str(&format!(
                    "- {}\tname={}\tsource={:?}\trisk={:?}\n",
                    descriptor.id, descriptor.name, descriptor.source, descriptor.risk_level
                ));
            } else {
                out.push_str(&format!("- {tool_id}\tmissing descriptor\n"));
            }
        }
    }

    if !context_pack.explicit_skill_ids.is_empty() {
        out.push_str("\nexplicit_skills:\n");
        for skill_id in &context_pack.explicit_skill_ids {
            out.push_str(&format!("- {skill_id}\n"));
        }
    }

    if !context_pack.tool_selection_plan.details.is_empty() {
        out.push_str("\ntool_selection_plan:\n");
        out.push_str(&format!(
            "task_type: {}\n",
            context_pack.tool_selection_plan.task_type
        ));
        for step in &context_pack.tool_selection_plan.steps {
            out.push_str(&format!(
                "- stage={} tools={} rationale={}\n",
                step.stage,
                step.tool_ids.join(" -> fallback "),
                step.rationale
            ));
        }
        out.push_str("details:\n");
        for detail in &context_pack.tool_selection_plan.details {
            out.push_str(&format!(
                "- {} score={} stage={} source={} risk={} reason={}",
                detail.tool_id,
                detail.score,
                detail.stage,
                detail.source,
                detail.risk,
                detail.reason
            ));
            if let Some(primary) = &detail.fallback_for {
                out.push_str(&format!(" fallback_for={primary}"));
            }
            out.push('\n');
        }
    }

    if !context_pack.missing.is_empty() {
        out.push_str("\nmissing:\n");
        for item in &context_pack.missing {
            out.push_str(&format!("- {item}\n"));
        }
    }

    if !context_pack.warnings.is_empty() {
        out.push_str("\nwarnings:\n");
        for warning in &context_pack.warnings {
            out.push_str(&format!("- {warning}\n"));
        }
    }

    out.push_str(&format!(
        "\nitems: memory={} rules={} local={}\n\n",
        context_pack.memory_items.len(),
        context_pack.rule_items.len(),
        context_pack.local_items.len()
    ));
    out.push_str(&context_pack.summary);
    out
}

fn run_audit_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    println!("{}", audit_report_text(args, &settings)?);
    Ok(())
}

fn audit_report_text(args: &[String], settings: &Settings) -> Result<String> {
    let path = settings.pwcli_home.join("audit/events.jsonl");
    let (events, malformed_lines) = read_audit_events(&path)?;
    match args.first().map(String::as_str).unwrap_or("summary") {
        "summary" | "status" => {
            let summary = summarize_events(&events, malformed_lines);
            Ok(format_audit_summary(&summary))
        }
        "tail" | "last" => {
            let limit = args
                .get(1)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(30);
            Ok(format_audit_tail(&events, limit))
        }
        other => Ok(format!(
            "unknown audit command: {other}\nusage: pwcli audit summary|tail [n]"
        )),
    }
}

fn run_verify_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let result = execute_verify_args(args, &settings)?;
    println!("{}", result.content);
    if result.is_error {
        Err(pwcli::PwError::ToolExecution(
            "verification failed".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn execute_verify_args(args: &[String], settings: &Settings) -> Result<ToolResult> {
    let arguments = parse_verify_arguments(args)?;
    let (registry, _) = build_registry(settings)?;
    let snapshot = registry.snapshot();
    let descriptor = snapshot
        .get("verification.project_check")
        .ok_or_else(|| pwcli::PwError::ToolNotFound("verification.project_check".to_string()))?
        .descriptor
        .clone();
    let call = ToolCall {
        id: format!(
            "manual-verification-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default()
        ),
        tool_id: descriptor.id,
        name: descriptor.name,
        arguments,
    };
    execute_manual_tool_call(settings, &snapshot, &call)
}

fn execute_manual_tool_call(
    settings: &Settings,
    snapshot: &ToolRegistrySnapshot,
    call: &ToolCall,
) -> Result<ToolResult> {
    execute_tool_call_with_approval(settings, snapshot, call, false)
}

fn execute_tool_call_with_approval(
    settings: &Settings,
    snapshot: &ToolRegistrySnapshot,
    call: &ToolCall,
    auto_approve: bool,
) -> Result<ToolResult> {
    let registered = snapshot
        .get(&call.tool_id)
        .ok_or_else(|| pwcli::PwError::ToolNotFound(call.tool_id.clone()))?;
    let descriptor = registered.descriptor.clone();
    let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));
    audit.record(AuditEvent::ToolCallRequested {
        call_id: call.id.clone(),
        tool_id: call.tool_id.clone(),
        name: call.name.clone(),
    });
    let policy = DefaultPolicyGuard::default().with_rules(load_rule_texts(settings));
    let decision = policy.check(&descriptor, call);
    audit.record(AuditEvent::PolicyDecisionRecorded {
        call_id: call.id.clone(),
        decision: decision.clone(),
    });

    let result = match decision {
        PolicyDecision::Allow => execute_snapshot_tool_with_runtime(settings, snapshot, call)?,
        PolicyDecision::Deny { reason } => ToolResult::error(reason),
        PolicyDecision::AskUser { prompt } => {
            let allowed = if auto_approve {
                true
            } else {
                StdinPrompter.ask_user(&prompt, call)
            };
            if allowed {
                execute_snapshot_tool_with_runtime(settings, snapshot, call)?
            } else {
                ToolResult::error("user rejected tool call")
            }
        }
    };
    audit.record(AuditEvent::ToolResultRecorded {
        call_id: call.id.clone(),
        is_error: result.is_error,
        metadata: result.metadata.clone(),
    });
    Ok(result)
}

fn execute_snapshot_tool_with_runtime(
    settings: &Settings,
    snapshot: &ToolRegistrySnapshot,
    call: &ToolCall,
) -> Result<ToolResult> {
    let context = ToolExecutionContext {
        runtime_tasks: Some(RuntimeTaskManager::new(settings.pwcli_home.clone())),
        ..ToolExecutionContext::default()
    };
    let mut runtime = ToolExecutionRuntime::new(context, |_| {});
    snapshot.execute_with_runtime(call, &mut runtime)
}

fn parse_verify_arguments(args: &[String]) -> Result<serde_json::Value> {
    let mut cwd: Option<String> = None;
    let mut timeout_seconds: Option<u64> = None;
    let mut max_output_chars: Option<usize> = None;
    let mut commands = Vec::new();
    let mut trailing_command = Vec::new();
    let mut idx = 0;

    while idx < args.len() {
        match args[idx].as_str() {
            "--cwd" | "-C" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli verify [--cwd <dir>] [--timeout <sec>] [--cmd <command>]"
                            .to_string(),
                    ));
                };
                cwd = Some(value.clone());
                idx += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli verify --timeout <sec>".to_string(),
                    ));
                };
                timeout_seconds = Some(value.parse::<u64>().map_err(|_| {
                    pwcli::PwError::Message(
                        "timeout must be an integer number of seconds".to_string(),
                    )
                })?);
                idx += 2;
            }
            "--max-output" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli verify --max-output <chars>".to_string(),
                    ));
                };
                max_output_chars = Some(value.parse::<usize>().map_err(|_| {
                    pwcli::PwError::Message("max-output must be an integer".to_string())
                })?);
                idx += 2;
            }
            "--cmd" | "-c" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli verify --cmd <command>".to_string(),
                    ));
                };
                commands.push(value.clone());
                idx += 2;
            }
            "--" => {
                if idx + 1 < args.len() {
                    commands.push(args[idx + 1..].join(" "));
                }
                break;
            }
            value => {
                trailing_command.push(value.to_string());
                idx += 1;
            }
        }
    }

    if !trailing_command.is_empty() {
        commands.push(trailing_command.join(" "));
    }

    let mut object = serde_json::Map::new();
    if let Some(cwd) = cwd {
        object.insert("cwd".to_string(), serde_json::json!(cwd));
    }
    if !commands.is_empty() {
        object.insert("commands".to_string(), serde_json::json!(commands));
    }
    if let Some(timeout_seconds) = timeout_seconds {
        object.insert(
            "timeout_seconds".to_string(),
            serde_json::json!(timeout_seconds),
        );
    }
    if let Some(max_output_chars) = max_output_chars {
        object.insert(
            "max_output_chars".to_string(),
            serde_json::json!(max_output_chars),
        );
    }
    Ok(serde_json::Value::Object(object))
}

fn run_memory_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    store.ensure()?;
    match args.first().map(String::as_str) {
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", help_text(Some("memory")));
            Ok(())
        }
        Some("inbox") | None => print_memory_inbox(&store),
        Some("facts") | Some("list") => print_memory_facts(&store),
        Some("search") => {
            let query = args[1..].join(" ");
            if query.trim().is_empty() {
                eprintln!("usage: pwcli memory search <query>");
                return Ok(());
            }
            for scored in store.search(&query, 12)? {
                println!(
                    "{}\t{:.2}\t{}\n  source: {}\n",
                    scored.fact.id, scored.score, scored.fact.statement, scored.fact.source
                );
            }
            Ok(())
        }
        Some("add") => {
            if args.get(1).map(String::as_str) != Some("fact") {
                eprintln!("usage: pwcli memory add fact <statement>");
                return Ok(());
            }
            let statement = args[2..].join(" ");
            if statement.trim().is_empty() {
                eprintln!("usage: pwcli memory add fact <statement>");
                return Ok(());
            }
            let source = format!(
                "{} 用户通过 pwcli memory add 手动写入",
                chrono::Local::now().format("%Y-%m-%d")
            );
            let fact = store.add_fact(statement, source)?;
            println!("{}", fact.id);
            Ok(())
        }
        Some("accept") => {
            let Some(candidate_id) = args.get(1) else {
                eprintln!("usage: pwcli memory accept <candidate_id>");
                return Ok(());
            };
            let facts = store.accept_candidate(candidate_id)?;
            for fact in facts {
                println!("accepted {}\t{}", fact.id, fact.statement);
            }
            Ok(())
        }
        Some("show") => {
            let Some(candidate_id) = args.get(1) else {
                eprintln!("usage: pwcli memory show <candidate_id>");
                return Ok(());
            };
            match store.get_candidate(candidate_id)? {
                Some(candidate) => println!("{}", format_memory_candidate(&candidate)),
                None => eprintln!("unknown memory candidate '{candidate_id}'"),
            }
            Ok(())
        }
        Some("reject") => {
            let Some(candidate_id) = args.get(1) else {
                eprintln!("usage: pwcli memory reject <candidate_id>");
                return Ok(());
            };
            store.reject_candidate(candidate_id)?;
            println!("rejected {candidate_id}");
            Ok(())
        }
        Some("extract") => run_memory_extract_cli(&args[1..], &settings),
        Some("graph") | Some("stats") => {
            let stats = store.graph_stats()?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
            Ok(())
        }
        Some("events") | Some("timeline") => {
            println!("{}", format_memory_events(&store.lifecycle_events()?));
            Ok(())
        }
        Some("derive") => {
            let query = args[1..].join(" ");
            let candidate = store
                .derive_candidate_from_graph((!query.trim().is_empty()).then_some(query.trim()))?;
            if let Some(candidate) = candidate {
                let id = candidate.id.clone();
                store.add_candidate(&candidate)?;
                print_memory_extract_result(Some(id));
            } else {
                print_memory_extract_result(None);
            }
            Ok(())
        }
        Some("rebuild") => {
            let stats = store.rebuild_graph_index()?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
            Ok(())
        }
        Some("embedder") if args.get(1).map(String::as_str) == Some("ensure") => {
            let path = store.ensure_embedding_model()?;
            println!("{}", path.display());
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown memory command: {other}");
            eprintln!("{}", help_text(Some("memory")));
            Ok(())
        }
    }
}

fn run_memory_extract_cli(args: &[String], settings: &Settings) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("task") | None => {
            let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
            runtime.ensure()?;
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            let text = memory_task_source_text(&runtime, &task_id)?;
            print_memory_extract_result(create_memory_candidate_from_text(
                settings,
                &text,
                "runtime task summary",
            )?);
            Ok(())
        }
        Some("file") => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: pwcli memory extract file <path>");
                return Ok(());
            };
            let text = std::fs::read_to_string(path)?;
            print_memory_extract_result(create_memory_candidate_from_text(
                settings,
                &text,
                &format!("file {path}"),
            )?);
            Ok(())
        }
        Some("text") => {
            let text = args[1..].join(" ");
            if text.trim().is_empty() {
                eprintln!("usage: pwcli memory extract text <text>");
                return Ok(());
            }
            print_memory_extract_result(create_memory_candidate_from_text(
                settings,
                &text,
                "manual text",
            )?);
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown memory extract target: {other}");
            eprintln!("usage: pwcli memory extract task [id]|file <path>|text <text>");
            Ok(())
        }
    }
}

fn print_memory_extract_result(candidate_id: Option<String>) {
    if let Some(candidate_id) = candidate_id {
        println!("memory candidate created: {candidate_id}");
        println!("review with: pwcli memory inbox");
        println!("write with:  pwcli memory accept {candidate_id}");
    } else {
        println!("no memory candidate created");
    }
}

fn print_memory_inbox(store: &MemoryStore) -> Result<()> {
    let candidates = store.list_candidates()?;
    if candidates.is_empty() {
        println!("memory inbox is empty");
        return Ok(());
    }
    for candidate in candidates {
        println!("{}", format_memory_candidate(&candidate));
        println!();
    }
    Ok(())
}

fn print_memory_facts(store: &MemoryStore) -> Result<()> {
    let facts = store.list_facts()?;
    if facts.is_empty() {
        println!("no facts");
        return Ok(());
    }
    for fact in facts {
        println!(
            "{}\t{:?}\t{}\n  source: {}\n",
            fact.id, fact.status, fact.statement, fact.source
        );
    }
    Ok(())
}

fn run_rules_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    match args.first().map(String::as_str) {
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", help_text(Some("rules")));
        }
        Some("list") | Some("ls") | None => {
            let rules = list_rule_files(&settings)?;
            if rules.is_empty() {
                println!("no rules");
            } else {
                for (name, path) in rules {
                    println!("{name}\t{}", path.display());
                }
            }
        }
        Some("show") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: pwcli rules show <name>");
                return Ok(());
            };
            let path = rule_path(&settings, name)?;
            if !path.is_file() {
                eprintln!("rule not found: {}", normalize_rule_name(name)?);
                return Ok(());
            }
            println!("{}", std::fs::read_to_string(path)?);
        }
        Some("add") | Some("set") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: pwcli rules add <name> <text>");
                return Ok(());
            };
            let text = args[2..].join(" ");
            if text.trim().is_empty() {
                eprintln!("usage: pwcli rules add <name> <text>");
                return Ok(());
            }
            let path = rule_path(&settings, name)?;
            std::fs::write(&path, format!("{}\n", text.trim()))?;
            println!(
                "rule saved: {}\n{}",
                normalize_rule_name(name)?,
                path.display()
            );
        }
        Some("rm") | Some("remove") | Some("delete") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: pwcli rules rm <name>");
                return Ok(());
            };
            let path = rule_path(&settings, name)?;
            if path.is_file() {
                std::fs::remove_file(&path)?;
                println!("rule removed: {}", normalize_rule_name(name)?);
            } else {
                eprintln!("rule not found: {}", normalize_rule_name(name)?);
            }
        }
        Some(other) => {
            eprintln!("unknown rules command: {other}");
            eprintln!("{}", help_text(Some("rules")));
        }
    }
    Ok(())
}

fn list_rule_files(settings: &Settings) -> Result<Vec<(String, std::path::PathBuf)>> {
    let dir = settings.pwcli_home.join("rules");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut rules = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("md" | "txt")
        ) {
            rules.push((stem.to_string(), path));
        }
    }
    rules.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rules)
}

fn load_rule_texts(settings: &Settings) -> Vec<String> {
    list_rule_files(settings)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, path)| std::fs::read_to_string(path).ok())
        .collect()
}

fn rule_path(settings: &Settings, name: &str) -> Result<std::path::PathBuf> {
    Ok(settings
        .pwcli_home
        .join("rules")
        .join(format!("{}.md", normalize_rule_name(name)?)))
}

fn normalize_rule_name(name: &str) -> Result<String> {
    let normalized = name
        .trim()
        .trim_end_matches(".md")
        .trim_end_matches(".txt")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if normalized.is_empty() || normalized == "." || normalized == ".." {
        return Err(pwcli::PwError::Message(
            "rule name must contain letters, numbers, '-' or '_'".to_string(),
        ));
    }
    Ok(normalized)
}

fn run_agent_cli(args: &[String]) -> Result<()> {
    if matches!(
        args.first().map(String::as_str),
        Some("recommend" | "rec" | "choose" | "route")
    ) {
        println!("{}", agent_recommendation_text(&args[1..]));
        return Ok(());
    }
    if args.is_empty()
        || matches!(
            args.first().map(String::as_str),
            Some("list" | "ls" | "status" | "doctor")
        )
    {
        println!("{}", agent_inventory_text());
        return Ok(());
    }
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        println!("{}", help_text(Some("agent")));
        return Ok(());
    }
    let settings = Settings::load()?;
    let wait = agent_args_wait(args);
    let task_id = spawn_agent_task(args, &settings, wait, !wait)?;
    if !wait {
        println!("started task: {task_id}");
    }
    Ok(())
}

fn run_agent_mode_cli(mode: &str, default_yolo: bool, args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    let agent_args = agent_mode_args_for_runtime(&runtime, mode, default_yolo, args)?;
    let wait = agent_args_wait(&agent_args);
    let task_id = spawn_agent_task_with_runtime(&agent_args, &settings, &runtime, wait, !wait)?;
    if !wait {
        println!("started task: {task_id}");
    }
    Ok(())
}

fn agent_args_wait(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--wait")
}

fn agent_inventory_text() -> String {
    let mut out = String::from("pwcli code agents\n\n");
    for kind in AgentCliKind::all() {
        let path = executable_on_path(kind.binary());
        out.push_str(&format!(
            "{}\n  status: {}\n  binary: {}\n  path: {}\n  model: {}\n",
            kind.id(),
            if path.is_some() {
                "installed"
            } else {
                "missing"
            },
            kind.binary(),
            path.as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not found on PATH".to_string()),
            kind.best_model_hint()
        ));
        out.push_str("  hints:\n");
        for hint in kind.usage_hints() {
            out.push_str(&format!("    - {hint}\n"));
        }
        out.push('\n');
    }
    out.push_str(
        "Usage:\n  pwcli agent <codex|claude|agy|qodercli> [--wait] [--mode direct|goal|plan|execute|review] [--model <model>] [--effort high] [--yolo] <prompt>\n  pwcli plan [agent] [--wait] <prompt>\n  pwcli loop [agent] [--wait] <prompt>\n  pwcli review [agent] [--wait] <prompt>",
    );
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeAgentRecommendation {
    kind: AgentCliKind,
    mode: &'static str,
    reason: &'static str,
}

fn agent_recommendation_text(args: &[String]) -> String {
    let (mode, prompt) = parse_agent_recommendation_args(args);
    if prompt.trim().is_empty() {
        return "usage: pwcli agent recommend [--mode <goal|plan|execute|review>] <prompt>"
            .to_string();
    }
    let installed = AgentCliKind::all()
        .iter()
        .copied()
        .filter(|kind| executable_on_path(kind.binary()).is_some())
        .collect::<Vec<_>>();
    let recommendation = recommend_code_agent(mode.as_deref(), &prompt, &installed);
    let installed_status = if executable_on_path(recommendation.kind.binary()).is_some() {
        "installed"
    } else {
        "missing on PATH"
    };
    let command = recommended_agent_command(recommendation, &prompt);
    format!(
        "pwcli agent recommendation\n\nmode: {}\nagent: {} ({})\nreason: {}\nmodel: {}\n\ncommand:\n  {}",
        recommendation.mode,
        recommendation.kind.id(),
        installed_status,
        recommendation.reason,
        recommendation.kind.best_model_hint(),
        command
    )
}

fn parse_agent_recommendation_args(args: &[String]) -> (Option<String>, String) {
    let mut mode = None;
    let mut prompt_parts = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--mode" => {
                if let Some(value) = args.get(idx + 1) {
                    mode = Some(normalize_agent_mode(value));
                    idx += 2;
                } else {
                    idx += 1;
                }
            }
            value if value.starts_with("--mode=") => {
                mode = Some(normalize_agent_mode(value.trim_start_matches("--mode=")));
                idx += 1;
            }
            "--" => {
                prompt_parts.extend(args[idx + 1..].iter().cloned());
                break;
            }
            value
                if mode.is_none()
                    && matches!(
                        value,
                        "goal" | "plan" | "execute" | "exec" | "loop" | "review"
                    ) =>
            {
                mode = Some(normalize_agent_mode(value));
                idx += 1;
            }
            value => {
                prompt_parts.push(value.to_string());
                idx += 1;
            }
        }
    }
    (mode, prompt_parts.join(" "))
}

fn normalize_agent_mode(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "exec" | "loop" => "execute".to_string(),
        "goal" => "goal".to_string(),
        "plan" => "plan".to_string(),
        "review" => "review".to_string(),
        "execute" => "execute".to_string(),
        _ => "direct".to_string(),
    }
}

fn recommend_code_agent(
    requested_mode: Option<&str>,
    prompt: &str,
    installed: &[AgentCliKind],
) -> CodeAgentRecommendation {
    let mode = requested_mode
        .map(normalize_agent_mode)
        .filter(|mode| mode != "direct")
        .unwrap_or_else(|| infer_agent_mode(prompt).to_string());
    let preferred = preferred_agents_for_mode(&mode);
    let kind = preferred
        .iter()
        .copied()
        .find(|kind| installed.contains(kind))
        .unwrap_or(preferred[0]);
    let reason = match mode.as_str() {
        "review" => {
            "review work benefits from a code-review oriented agent and conservative risk scanning"
        }
        "plan" => "planning benefits from broad context synthesis before file edits",
        "goal" => "goal clarification should happen before planning or execution",
        "execute" => {
            "implementation work benefits from a strong local coding agent with workspace tooling"
        }
        _ => "direct coding assistance is best handled by the default strong local agent",
    };
    CodeAgentRecommendation {
        kind,
        mode: match mode.as_str() {
            "goal" => "goal",
            "plan" => "plan",
            "review" => "review",
            "execute" => "execute",
            _ => "direct",
        },
        reason,
    }
}

fn infer_agent_mode(prompt: &str) -> &'static str {
    let lower = prompt.to_ascii_lowercase();
    if contains_any(prompt, &["评审", "审查", "检查", "风险", "回归", "漏洞"])
        || contains_any(&lower, &["review", "audit", "regression", "risk", "bug"])
    {
        "review"
    } else if contains_any(prompt, &["计划", "方案", "拆解", "设计", "架构"])
        || contains_any(&lower, &["plan", "design", "architecture", "proposal"])
    {
        "plan"
    } else {
        "execute"
    }
}

fn preferred_agents_for_mode(mode: &str) -> &'static [AgentCliKind] {
    match mode {
        "goal" | "plan" => &[
            AgentCliKind::Claude,
            AgentCliKind::Codex,
            AgentCliKind::QoderCli,
            AgentCliKind::Agy,
        ],
        "review" => &[
            AgentCliKind::Codex,
            AgentCliKind::Claude,
            AgentCliKind::QoderCli,
            AgentCliKind::Agy,
        ],
        "execute" => &[
            AgentCliKind::Codex,
            AgentCliKind::QoderCli,
            AgentCliKind::Claude,
            AgentCliKind::Agy,
        ],
        _ => &[
            AgentCliKind::Codex,
            AgentCliKind::Claude,
            AgentCliKind::QoderCli,
            AgentCliKind::Agy,
        ],
    }
}

fn recommended_agent_command(recommendation: CodeAgentRecommendation, prompt: &str) -> String {
    let quoted = shell_quote(prompt);
    match recommendation.mode {
        "goal" | "plan" => {
            format!("pwcli plan {} --wait {quoted}", recommendation.kind.id())
        }
        "review" => format!("pwcli review {} --wait {quoted}", recommendation.kind.id()),
        "execute" => format!("pwcli loop {} --wait {quoted}", recommendation.kind.id()),
        _ => format!(
            "pwcli agent {} --wait --mode direct {quoted}",
            recommendation.kind.id()
        ),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn executable_on_path(binary: &str) -> Option<PathBuf> {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return executable_file(candidate).then(|| candidate.to_path_buf());
    }

    let path_var = std::env::var_os("PATH")?;
    executable_candidates(binary, path_var)
        .into_iter()
        .find(|path| executable_file(path))
}

fn executable_candidates(binary: &str, path_var: OsString) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for dir in std::env::split_paths(&path_var) {
        candidates.push(dir.join(binary));
        #[cfg(windows)]
        {
            let has_ext = Path::new(binary).extension().is_some();
            if !has_ext {
                for ext in ["exe", "cmd", "bat"] {
                    candidates.push(dir.join(format!("{binary}.{ext}")));
                }
            }
        }
    }
    candidates
}

fn executable_file(path: &Path) -> bool {
    path.is_file()
}

fn agent_mode_args_for_runtime(
    runtime: &RuntimeTaskManager,
    mode: &str,
    default_yolo: bool,
    args: &[String],
) -> Result<Vec<String>> {
    let mut args = args.to_vec();
    if !agent_mode_has_prompt(&args) {
        args.push(default_agent_mode_prompt(mode, runtime)?);
    }
    Ok(agent_mode_args(mode, default_yolo, &args))
}

fn agent_mode_args(mode: &str, default_yolo: bool, args: &[String]) -> Vec<String> {
    let mut idx = 0;
    let agent = args
        .first()
        .and_then(|arg| AgentCliKind::from_id(arg))
        .map(|kind| {
            idx = 1;
            kind.id().to_string()
        })
        .unwrap_or_else(|| "codex".to_string());

    let mut out = vec![
        agent,
        "--mode".to_string(),
        mode.to_string(),
        "--effort".to_string(),
        "high".to_string(),
    ];
    if default_yolo && !args.iter().any(|arg| arg == "--yolo") {
        out.push("--yolo".to_string());
    }
    out.extend(args[idx..].iter().cloned());
    out
}

fn agent_mode_has_prompt(args: &[String]) -> bool {
    let mut idx = 0;
    if args
        .first()
        .is_some_and(|arg| AgentCliKind::from_id(arg).is_some())
    {
        idx = 1;
    }
    while idx < args.len() {
        match args[idx].as_str() {
            "--task" | "--session-id" | "--mode" | "--model" | "-m" | "--effort" | "--cwd"
            | "-C" | "-w" | "--timeout" => idx += 2,
            "--wait" | "--yolo" => idx += 1,
            "--" => return idx + 1 < args.len(),
            value
                if value.starts_with("--task=")
                    || value.starts_with("--session-id=")
                    || value.starts_with("--cli-arg=") =>
            {
                idx += 1;
            }
            _ => return true,
        }
    }
    false
}

fn default_agent_mode_prompt(mode: &str, runtime: &RuntimeTaskManager) -> Result<String> {
    let task_id = runtime.active_task_id()?.ok_or_else(|| {
        pwcli::PwError::Message(
            "no active task; use `pwcli goal <objective>` before running plan/loop/review without a prompt".to_string(),
        )
    })?;
    let task = runtime.get(&task_id)?;
    let goal = task
        .metadata
        .get("goal")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&task.title);
    let log = task_log_text(runtime, &task_id);
    let recent = preview(&log, 2400);
    let action = match mode {
        "plan" => "Create a concrete implementation plan for the active task. Include scope, steps, risks, and validation. Do not edit files.",
        "execute" => "Execute the active task according to the current plan/context. Keep changes scoped, run relevant validation, and report what changed.",
        "review" => "Review the active task output and current work. Prioritize bugs, regressions, missing tests, and risky assumptions.",
        _ => "Continue the active task using the current task context.",
    };
    Ok(format!(
        "{action}\n\nActive task id: {task_id}\nGoal: {goal}\nStatus: {:?}\n\nRecent task context:\n{recent}",
        task.status
    ))
}

fn run_goal_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    let goal = args.join(" ");
    if goal.trim().is_empty() {
        eprintln!("usage: pwcli goal <objective>");
        return Ok(());
    }
    let task = runtime.create_task(
        RuntimeTaskKind::Internal,
        goal.clone(),
        std::env::current_dir()?,
        serde_json::json!({ "goal": goal, "created_by": "pwcli goal" }),
    )?;
    runtime.set_active(&task.task_id)?;
    println!("active task: {}\n{}", task.task_id, task.title);
    Ok(())
}

fn spawn_agent_task(
    args: &[String],
    settings: &Settings,
    wait: bool,
    detach: bool,
) -> Result<String> {
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    spawn_agent_task_with_runtime(args, settings, &runtime, wait, detach)
}

fn spawn_agent_task_with_runtime(
    args: &[String],
    _settings: &Settings,
    runtime: &RuntimeTaskManager,
    wait: bool,
    detach: bool,
) -> Result<String> {
    let Some(agent) = args.first() else {
        return Err(pwcli::PwError::Message("usage: pwcli agent <codex|claude|agy|qodercli> [--task <task_id>] [--wait] [--mode direct|goal|plan|execute|review] [--model <model>] [--effort <effort>] [--cwd <dir>] [--yolo] <prompt>".to_string()));
    };
    let kind = AgentCliKind::from_id(agent)
        .ok_or_else(|| pwcli::PwError::Message(format!("unknown agent cli: {agent}")))?;

    let mut task_id: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut mode = "direct".to_string();
    let mut model: Option<String> = None;
    let mut effort = "high".to_string();
    let mut cwd: Option<std::path::PathBuf> = None;
    let mut yolo = false;
    let mut timeout_seconds = 900_u64;
    let mut extra_args = Vec::new();
    let mut prompt_parts = Vec::new();

    let mut iter = args[1..].iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--task" => {
                if let Some(value) = iter.next() {
                    task_id = Some(value.clone());
                }
            }
            value if value.starts_with("--task=") => {
                task_id = Some(value.trim_start_matches("--task=").to_string());
            }
            "--session-id" => {
                if let Some(value) = iter.next() {
                    session_id = Some(value.clone());
                }
            }
            value if value.starts_with("--session-id=") => {
                session_id = Some(value.trim_start_matches("--session-id=").to_string());
            }
            "--wait" => {}
            "--mode" => {
                if let Some(value) = iter.next() {
                    mode = value.clone();
                }
            }
            "--model" | "-m" => {
                if let Some(value) = iter.next() {
                    model = Some(value.clone());
                }
            }
            "--effort" => {
                if let Some(value) = iter.next() {
                    effort = value.clone();
                }
            }
            "--cwd" | "-C" | "-w" => {
                if let Some(value) = iter.next() {
                    cwd = Some(std::path::PathBuf::from(value));
                }
            }
            "--timeout" => {
                if let Some(value) = iter.next() {
                    if let Ok(parsed) = value.parse::<u64>() {
                        timeout_seconds = parsed;
                    }
                }
            }
            "--yolo" => yolo = true,
            "--" => {
                prompt_parts.extend(iter.cloned());
                break;
            }
            value if value.starts_with("--cli-arg=") => {
                extra_args.push(value.trim_start_matches("--cli-arg=").to_string());
            }
            value => prompt_parts.push(value.to_string()),
        }
    }

    let prompt = prompt_parts.join(" ");
    if prompt.trim().is_empty() {
        return Err(pwcli::PwError::Message(format!(
            "usage: pwcli agent {agent} <prompt>"
        )));
    }

    if task_id.is_none() {
        task_id = runtime.active_task_id()?;
    }
    let prior_summary = task_id
        .as_ref()
        .and_then(|id| std::fs::read_to_string(runtime.task_dir(id).join("summary.md")).ok());
    let args = AgentCliArgs {
        prompt,
        mode,
        cwd,
        model,
        session_id,
        effort,
        yolo,
        background: false,
        timeout_seconds,
        extra_args,
    };
    let spec = build_runtime_task_spec(kind, task_id, args, prior_summary);
    let handle = if detach {
        runtime.spawn_detached(spec, std::env::current_exe()?)?
    } else {
        runtime.spawn(spec)?
    };
    runtime.set_active(&handle.task_id)?;
    if wait {
        println!("started task: {}", handle.task_id);
        let _ = std::io::stdout().flush();
        wait_for_runtime_task(runtime, &handle.task_id)?;
    }
    Ok(handle.task_id)
}

fn run_task_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    match args.first().map(String::as_str) {
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", help_text(Some("task")));
        }
        Some("new") => {
            let goal = args[1..].join(" ");
            if goal.trim().is_empty() {
                eprintln!("{}", help_text(Some("task")));
                return Ok(());
            }
            let task = runtime.create_task(
                RuntimeTaskKind::Internal,
                goal.clone(),
                std::env::current_dir()?,
                serde_json::json!({ "goal": goal }),
            )?;
            runtime.set_active(&task.task_id)?;
            println!("active task: {}\n{}", task.task_id, task.title);
        }
        Some("list") | None => {
            let active = runtime.active_task_id()?;
            println!("{}", format_task_list(&runtime.list()?, active.as_deref()));
        }
        Some("use") | Some("active") => {
            let selector = args.get(1).map(String::as_str).unwrap_or("last");
            runtime.set_active(selector)?;
            if let Some(task_id) = runtime.active_task_id()? {
                println!("active task: {task_id}");
            } else {
                eprintln!("no active task");
            }
        }
        Some("status") => {
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            let task = runtime.get(&task_id)?;
            println!("{}", serde_json::to_string_pretty(&task)?);
            println!("\n{}", format_task_next(&task));
        }
        Some("next") => {
            println!(
                "{}",
                task_next_text(&runtime, args.get(1).map(String::as_str))?
            );
        }
        Some("log") => {
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            print_task_log(&runtime, &task_id)?;
        }
        Some("watch") | Some("attach") => {
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            watch_runtime_task(&runtime, &task_id)?;
        }
        Some("cancel") => {
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            runtime.cancel(&task_id)?;
            println!("cancelled {task_id}");
        }
        Some("compact") => {
            let Some(task_id) = runtime.resolve_task_id(args.get(1).map(String::as_str))? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            let summary = runtime.compact(&task_id, CompactScope::Both)?;
            println!("{}", summary.summary_path.display());
            if let Some(candidate_id) = create_memory_candidate_from_text(
                &settings,
                &summary.content,
                "runtime compact summary",
            )? {
                println!("memory candidate created: {candidate_id}");
                println!("review with: pwcli memory inbox");
                println!("write with:  pwcli memory accept {candidate_id}");
            }
        }
        Some("verify") => {
            let (selector, verify_args) = split_task_verify_args(&args[1..]);
            let Some(task_id) = runtime.resolve_task_id(selector.as_deref())? else {
                eprintln!("no active task; use `pwcli goal <objective>` or `pwcli task use last`");
                return Ok(());
            };
            let task = runtime.get(&task_id)?;
            let verify_args = verify_args
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>();
            let record = execute_task_verification(&settings, &task, &verify_args)?;
            let verification_path = runtime.record_verification(&task_id, record.clone())?;
            let gate = record
                .report
                .as_ref()
                .map(|report| report.gate.decision.as_str())
                .unwrap_or(if record.passed { "pass" } else { "block" });
            let status = record
                .report
                .as_ref()
                .map(|report| report.status.as_str())
                .unwrap_or(if record.passed { "passed" } else { "failed" });
            println!(
                "verification {status} gate={gate}: {}\n{}",
                verification_path.display(),
                record.content
            );
            println!("\n{}", format_task_next(&runtime.get(&task_id)?));
        }
        Some(other) => {
            eprintln!("unknown task command: {other}");
            eprintln!("{}", help_text(Some("task")));
        }
    }
    Ok(())
}

fn run_task_next_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    println!(
        "{}",
        task_next_text(&runtime, args.first().map(String::as_str))?
    );
    Ok(())
}

fn run_workflow_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    runtime.ensure()?;
    match args.first().map(String::as_str).unwrap_or("help") {
        "run" => run_workflow_run_cli(&args[1..], &settings, &runtime),
        "resume" => run_workflow_resume_cli(&args[1..], &settings, &runtime),
        "status" | "show" => run_workflow_status_cli(&args[1..], &runtime),
        "plan" | "graph" | "mermaid" => run_workflow_plan_cli(&args[1..], &settings),
        "save" => run_workflow_save_cli(&args[1..], &settings, &runtime),
        "recipes" | "list-recipes" => run_workflow_recipe_list_cli(&settings),
        "recipe" => run_workflow_recipe_cli(&args[1..], &settings, &runtime),
        "help" | "--help" | "-h" => {
            println!("{}", workflow_help_text());
            Ok(())
        }
        other => {
            eprintln!("unknown workflow command: {other}");
            eprintln!("{}", workflow_help_text());
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
struct WorkflowRunOptions {
    goal: String,
    agent: Option<String>,
    yes: bool,
    dry_run: bool,
    kind: WorkflowPlanKind,
    recipe: Option<String>,
}

fn parse_workflow_run_options(args: &[String]) -> WorkflowRunOptions {
    let mut agent = None::<String>;
    let mut yes = false;
    let mut dry_run = false;
    let mut kind = WorkflowPlanKind::Auto;
    let mut recipe = None::<String>;
    let mut goal_parts = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--agent" => {
                if let Some(value) = iter.next() {
                    agent = Some(value.clone());
                }
            }
            value if value.starts_with("--agent=") => {
                agent = Some(value.trim_start_matches("--agent=").to_string());
            }
            "--kind" | "--type" => {
                if let Some(value) = iter.next() {
                    if let Some(parsed) = WorkflowPlanKind::parse(value) {
                        kind = parsed;
                    }
                }
            }
            value if value.starts_with("--kind=") => {
                if let Some(parsed) = WorkflowPlanKind::parse(value.trim_start_matches("--kind=")) {
                    kind = parsed;
                }
            }
            value if value.starts_with("--type=") => {
                if let Some(parsed) = WorkflowPlanKind::parse(value.trim_start_matches("--type=")) {
                    kind = parsed;
                }
            }
            "--recipe" => {
                if let Some(value) = iter.next() {
                    recipe = Some(value.clone());
                }
            }
            value if value.starts_with("--recipe=") => {
                recipe = Some(value.trim_start_matches("--recipe=").to_string());
            }
            "--yes" | "-y" => yes = true,
            "--dry-run" => dry_run = true,
            "--" => {
                goal_parts.extend(iter.cloned());
                break;
            }
            value => goal_parts.push(value.to_string()),
        }
    }
    WorkflowRunOptions {
        goal: goal_parts.join(" "),
        agent,
        yes,
        dry_run,
        kind,
        recipe,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowRecipe {
    name: String,
    description: String,
    goal: String,
    agent: String,
    kind: WorkflowPlanKind,
    created_at: String,
    workflow: GraphWorkflow,
}

fn run_workflow_plan_cli(args: &[String], settings: &Settings) -> Result<()> {
    let options = parse_workflow_run_options(args);
    let workflow = workflow_from_options(settings, &options)?;
    let goal = workflow_goal_for_display(&options);
    let kind = workflow_kind(&workflow).unwrap_or_else(|| options.kind.resolve(&goal));
    println!(
        "workflow plan kind={} agent={}\n\n{}",
        kind.as_str(),
        workflow_agent_label(&workflow).unwrap_or("mixed"),
        workflow_mermaid(&workflow)
    );
    Ok(())
}

fn run_workflow_save_cli(
    args: &[String],
    settings: &Settings,
    runtime: &RuntimeTaskManager,
) -> Result<()> {
    let Some(name) = args.first() else {
        eprintln!("usage: pwcli workflow save <name> [--from id|last|active] [--force]");
        eprintln!("   or: pwcli workflow save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
        return Ok(());
    };
    let (source_task, force, workflow_args) = split_workflow_save_args(&args[1..]);
    let (recipe, path) = if let Some(selector) = source_task {
        save_workflow_recipe_from_task(settings, runtime, name, Some(&selector), force)?
    } else {
        let options = parse_workflow_run_options(&workflow_args);
        if options.goal.trim().is_empty() && options.recipe.is_none() {
            eprintln!("usage: pwcli workflow save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
            return Ok(());
        }
        let workflow = workflow_from_options(settings, &options)?;
        let recipe = workflow_recipe_from_options(name, &options, workflow)?;
        let path = save_workflow_recipe(settings, &recipe)?;
        (recipe, path)
    };
    println!("workflow recipe saved: {}\n{}", recipe.name, path.display());
    Ok(())
}

fn split_workflow_save_args(args: &[String]) -> (Option<String>, bool, Vec<String>) {
    let mut source_task = None::<String>;
    let mut force = false;
    let mut workflow_args = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                source_task = if iter.peek().is_some_and(|next| !next.starts_with("--")) {
                    iter.next().cloned()
                } else {
                    Some("active".to_string())
                };
            }
            value if value.starts_with("--from=") => {
                let selector = value.trim_start_matches("--from=");
                source_task = Some(if selector.is_empty() {
                    "active".to_string()
                } else {
                    selector.to_string()
                });
            }
            "--force" => force = true,
            _ => workflow_args.push(arg.clone()),
        }
    }
    (source_task, force, workflow_args)
}

fn workflow_recipe_from_options(
    name: &str,
    options: &WorkflowRunOptions,
    workflow: GraphWorkflow,
) -> Result<WorkflowRecipe> {
    let goal = workflow_goal_for_display(options);
    Ok(WorkflowRecipe {
        name: sanitize_workflow_recipe_name(name)?,
        description: goal.clone(),
        goal: goal.clone(),
        agent: workflow_agent_label(&workflow)
            .unwrap_or("mixed")
            .to_string(),
        kind: workflow_kind(&workflow).unwrap_or_else(|| options.kind.resolve(&goal)),
        created_at: chrono::Utc::now().to_rfc3339(),
        workflow,
    })
}

fn save_workflow_recipe_from_task(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    name: &str,
    selector: Option<&str>,
    force: bool,
) -> Result<(WorkflowRecipe, PathBuf)> {
    let Some(task_id) = runtime.resolve_task_id(selector)? else {
        return Err(PwError::Message(
            "workflow task not found for recipe save".to_string(),
        ));
    };
    let task = runtime.get(&task_id)?;
    if task.kind != RuntimeTaskKind::Workflow {
        return Err(PwError::Message(format!(
            "task {task_id} is {:?}, not Workflow",
            task.kind
        )));
    }
    let summary = load_workflow_summary(runtime, &task_id).ok();
    if !force {
        let Some(summary) = summary else {
            return Err(PwError::Message(format!(
                "workflow task {task_id} has no workflow_state.json; use --force to save it anyway"
            )));
        };
        if task.status != RuntimeTaskStatus::Completed
            || summary.status != WorkflowStatus::Completed
        {
            return Err(PwError::Message(format!(
                "workflow task {task_id} is not completed; use --force to save it anyway"
            )));
        }
    }
    let workflow = load_workflow(runtime, &task_id)?;
    let goal = task
        .metadata
        .get("goal")
        .and_then(serde_json::Value::as_str)
        .filter(|goal| !goal.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| task.title.clone());
    let recipe = WorkflowRecipe {
        name: sanitize_workflow_recipe_name(name)?,
        description: format!("from task {task_id}: {goal}"),
        goal: goal.clone(),
        agent: workflow_agent_label(&workflow)
            .unwrap_or("mixed")
            .to_string(),
        kind: workflow_kind(&workflow).unwrap_or_else(|| WorkflowPlanKind::Auto.resolve(&goal)),
        created_at: chrono::Utc::now().to_rfc3339(),
        workflow,
    };
    let path = save_workflow_recipe(settings, &recipe)?;
    Ok((recipe, path))
}

fn workflow_kind(workflow: &GraphWorkflow) -> Option<WorkflowPlanKind> {
    match workflow.name.as_str() {
        "code_agent_plan_execute_review" => Some(WorkflowPlanKind::Code),
        "research_collect_synthesize_verify" => Some(WorkflowPlanKind::Research),
        "ops_plan_execute_verify" => Some(WorkflowPlanKind::Ops),
        "general_plan_execute_review" => Some(WorkflowPlanKind::General),
        _ => None,
    }
}

fn run_workflow_recipe_cli(
    args: &[String],
    settings: &Settings,
    runtime: &RuntimeTaskManager,
) -> Result<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => run_workflow_recipe_list_cli(settings),
        "save" => run_workflow_save_cli(&args[1..], settings, runtime),
        "show" => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: pwcli workflow recipe show <name>");
                return Ok(());
            };
            let recipe = load_workflow_recipe(settings, name)?;
            println!("{}", format_workflow_recipe(&recipe));
            println!("\n{}", workflow_mermaid(&recipe.workflow));
            Ok(())
        }
        "run" => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: pwcli workflow recipe run <name> [--yes]");
                return Ok(());
            };
            let yes = args.iter().any(|arg| arg == "--yes" || arg == "-y");
            run_workflow_recipe_task(settings, runtime, name, yes)
        }
        other => {
            eprintln!("unknown workflow recipe command: {other}");
            eprintln!("usage: pwcli workflow recipe list|save|show|run");
            Ok(())
        }
    }
}

fn run_workflow_recipe_list_cli(settings: &Settings) -> Result<()> {
    let recipes = list_workflow_recipes(settings)?;
    if recipes.is_empty() {
        println!("no workflow recipes");
        return Ok(());
    }
    for recipe in recipes {
        println!(
            "{}\t{}\tagent={}\tkind={}",
            recipe.name,
            recipe.description,
            recipe.agent,
            recipe.kind.as_str()
        );
    }
    Ok(())
}

fn run_workflow_run_cli(
    args: &[String],
    settings: &Settings,
    runtime: &RuntimeTaskManager,
) -> Result<()> {
    let options = parse_workflow_run_options(args);
    if options.goal.trim().is_empty() && options.recipe.is_none() {
        eprintln!("usage: pwcli workflow run [--agent codex] [--kind auto|code|research|ops|general] [--yes] [--dry-run] <goal>");
        eprintln!("   or: pwcli workflow run --recipe <name> [--yes]");
        return Ok(());
    }
    let workflow = workflow_from_options(settings, &options)?;
    if options.dry_run {
        println!("{}", workflow_mermaid(&workflow));
        return Ok(());
    }

    let task = runtime.create_task(
        RuntimeTaskKind::Workflow,
        format!("workflow {}", workflow_goal_for_display(&options)),
        std::env::current_dir()?,
        serde_json::json!({
            "goal": workflow_goal_for_display(&options),
            "workflow": {
                "name": workflow.name.clone(),
                "status": "running",
                "recipe": options.recipe
            }
        }),
    )?;
    runtime.set_active(&task.task_id)?;
    persist_workflow_artifacts(runtime, &task.task_id, &workflow, None)?;
    let summary = run_workflow_for_task(
        settings,
        runtime,
        &task.task_id,
        workflow,
        options.yes,
        None,
    )?;
    persist_workflow_artifacts(
        runtime,
        &task.task_id,
        &load_workflow(runtime, &task.task_id)?,
        Some(&summary),
    )?;
    finalize_workflow_task(runtime, &task.task_id, &summary)?;
    println!("{}", format_workflow_summary(&summary));
    println!("\n{}", format_task_next(&runtime.get(&task.task_id)?));
    Ok(())
}

fn run_workflow_recipe_task(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    name: &str,
    yes: bool,
) -> Result<()> {
    let recipe = load_workflow_recipe(settings, name)?;
    let options = WorkflowRunOptions {
        goal: recipe.goal.clone(),
        agent: None,
        yes,
        dry_run: false,
        kind: recipe.kind,
        recipe: Some(recipe.name.clone()),
    };
    let task = runtime.create_task(
        RuntimeTaskKind::Workflow,
        format!("workflow recipe {}", recipe.name),
        std::env::current_dir()?,
        serde_json::json!({
            "goal": recipe.goal,
            "workflow": {
                "name": recipe.workflow.name.clone(),
                "status": "running",
                "recipe": recipe.name
            }
        }),
    )?;
    runtime.set_active(&task.task_id)?;
    persist_workflow_artifacts(runtime, &task.task_id, &recipe.workflow, None)?;
    let summary = run_workflow_for_task(
        settings,
        runtime,
        &task.task_id,
        recipe.workflow,
        options.yes,
        None,
    )?;
    persist_workflow_artifacts(
        runtime,
        &task.task_id,
        &load_workflow(runtime, &task.task_id)?,
        Some(&summary),
    )?;
    finalize_workflow_task(runtime, &task.task_id, &summary)?;
    println!("{}", format_workflow_summary(&summary));
    println!("\n{}", format_task_next(&runtime.get(&task.task_id)?));
    Ok(())
}

fn workflow_from_options(
    settings: &Settings,
    options: &WorkflowRunOptions,
) -> Result<GraphWorkflow> {
    if let Some(recipe_name) = &options.recipe {
        let mut recipe = load_workflow_recipe(settings, recipe_name)?;
        if let Some(agent) = options.agent.as_deref() {
            set_workflow_agent(&mut recipe.workflow, agent);
        }
        return Ok(recipe.workflow);
    }
    let goal = if options.goal.trim().is_empty() {
        "workflow goal"
    } else {
        options.goal.trim()
    };
    Ok(GraphWorkflow::planned(
        goal,
        options.agent.as_deref().unwrap_or("codex"),
        options.kind,
    ))
}

fn workflow_goal_for_display(options: &WorkflowRunOptions) -> String {
    if !options.goal.trim().is_empty() {
        return options.goal.trim().to_string();
    }
    options
        .recipe
        .as_ref()
        .map(|name| format!("recipe:{name}"))
        .unwrap_or_else(|| "workflow goal".to_string())
}

fn workflow_agent_label(workflow: &GraphWorkflow) -> Option<&str> {
    let mut agent = None::<&str>;
    for node in workflow.nodes.values() {
        if let WorkflowNodeKind::AgentTask {
            agent: node_agent, ..
        } = &node.kind
        {
            match agent {
                None => agent = Some(node_agent.as_str()),
                Some(existing) if existing == node_agent => {}
                Some(_) => return None,
            }
        }
    }
    agent
}

fn workflow_recipes_dir(settings: &Settings) -> PathBuf {
    settings.pwcli_home.join("workflow/recipes")
}

fn workflow_recipe_path(settings: &Settings, name: &str) -> Result<PathBuf> {
    let safe = sanitize_workflow_recipe_name(name)?;
    Ok(workflow_recipes_dir(settings).join(format!("{safe}.json")))
}

fn sanitize_workflow_recipe_name(name: &str) -> Result<String> {
    let trimmed = name.trim().trim_end_matches(".json");
    if trimmed.is_empty() {
        return Err(PwError::Message(
            "workflow recipe name is required".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        return Err(PwError::Message(format!(
            "invalid workflow recipe name: {name}"
        )));
    }
    let safe = trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if safe.is_empty() {
        return Err(PwError::Message(format!(
            "invalid workflow recipe name: {name}"
        )));
    }
    Ok(safe)
}

fn save_workflow_recipe(settings: &Settings, recipe: &WorkflowRecipe) -> Result<PathBuf> {
    let path = workflow_recipe_path(settings, &recipe.name)?;
    write_json(&path, recipe)?;
    Ok(path)
}

fn load_workflow_recipe(settings: &Settings, name: &str) -> Result<WorkflowRecipe> {
    let path = workflow_recipe_path(settings, name)?;
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn list_workflow_recipes(settings: &Settings) -> Result<Vec<WorkflowRecipe>> {
    let dir = workflow_recipes_dir(settings);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut recipes = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(recipe) = serde_json::from_slice::<WorkflowRecipe>(&std::fs::read(path)?) {
            recipes.push(recipe);
        }
    }
    recipes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(recipes)
}

fn format_workflow_recipe(recipe: &WorkflowRecipe) -> String {
    format!(
        "recipe: {}\nkind: {}\nagent: {}\ncreated_at: {}\ngoal: {}\nworkflow: {}",
        recipe.name,
        recipe.kind.as_str(),
        recipe.agent,
        recipe.created_at,
        recipe.goal,
        recipe.workflow.name
    )
}

fn run_workflow_resume_cli(
    args: &[String],
    settings: &Settings,
    runtime: &RuntimeTaskManager,
) -> Result<()> {
    let yes = args.iter().any(|arg| arg == "--yes" || arg == "-y");
    let selector = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str);
    let Some(task_id) = runtime.resolve_task_id(selector)? else {
        eprintln!("no workflow task found; use `pwcli workflow run <goal>` first");
        return Ok(());
    };
    let summary = resume_workflow_task(settings, runtime, &task_id, yes)?;
    println!("{}", format_workflow_summary(&summary));
    println!("\n{}", format_task_next(&runtime.get(&task_id)?));
    Ok(())
}

fn run_workflow_status_cli(args: &[String], runtime: &RuntimeTaskManager) -> Result<()> {
    let Some(task_id) = runtime.resolve_task_id(args.first().map(String::as_str))? else {
        eprintln!("no workflow task found");
        return Ok(());
    };
    let task = runtime.get(&task_id)?;
    println!("{}", serde_json::to_string_pretty(&task)?);
    if let Ok(summary) = load_workflow_summary(runtime, &task_id) {
        println!("\n{}", format_workflow_summary(&summary));
    }
    if let Ok(workflow) = load_workflow(runtime, &task_id) {
        println!("\n{}", workflow_mermaid(&workflow));
    }
    Ok(())
}

struct RuntimeWorkflowRunner<'a> {
    settings: &'a Settings,
    runtime: &'a RuntimeTaskManager,
    task_id: String,
    auto_approve: bool,
}

impl WorkflowNodeRunner for RuntimeWorkflowRunner<'_> {
    fn run_node(
        &mut self,
        _workflow: &GraphWorkflow,
        node: &WorkflowNode,
        _context: &WorkflowContext,
    ) -> Result<WorkflowStepOutcome> {
        self.runtime
            .record_workflow_node_started(&self.task_id, &node.id, &node.label)?;
        let outcome = match &node.kind {
            WorkflowNodeKind::AgentTask {
                agent,
                mode,
                prompt,
            } => self.run_agent_node(agent, mode, prompt, _context),
            WorkflowNodeKind::ToolCall { tool_id, arguments } => {
                self.run_tool_node(tool_id, arguments.clone())
            }
            WorkflowNodeKind::ResearchReadPapers { max_papers } => {
                self.run_research_read_papers(*max_papers, _context)
            }
            WorkflowNodeKind::Approval { prompt } => self.run_approval_node(prompt),
            WorkflowNodeKind::Join
            | WorkflowNodeKind::ModelTurn { .. }
            | WorkflowNodeKind::AdaptiveLoop { .. } => Ok(WorkflowStepOutcome::Success(
                serde_json::json!({ "ok": true }),
            )),
            WorkflowNodeKind::SubWorkflow { workflow } => {
                let mut nested = RuntimeWorkflowRunner {
                    settings: self.settings,
                    runtime: self.runtime,
                    task_id: self.task_id.clone(),
                    auto_approve: self.auto_approve,
                };
                let summary = WorkflowExecutor::new().run(workflow, &mut nested)?;
                Ok(if summary.status == WorkflowStatus::Completed {
                    WorkflowStepOutcome::Success(serde_json::to_value(summary)?)
                } else {
                    WorkflowStepOutcome::Failure(format!("{:?}", summary.status))
                })
            }
            WorkflowNodeKind::End => Ok(WorkflowStepOutcome::Stop),
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) => WorkflowStepOutcome::Failure(err.to_string()),
        };
        let status = match &outcome {
            WorkflowStepOutcome::Success(_) | WorkflowStepOutcome::Stop => "success",
            WorkflowStepOutcome::Failure(_) => "failure",
            WorkflowStepOutcome::Interrupt { .. } => "interrupt",
        };
        self.runtime
            .record_workflow_node_completed(&self.task_id, &node.id, status)?;
        Ok(outcome)
    }
}

impl RuntimeWorkflowRunner<'_> {
    fn run_agent_node(
        &self,
        agent: &str,
        mode: &str,
        prompt: &str,
        context: &WorkflowContext,
    ) -> Result<WorkflowStepOutcome> {
        let prompt = workflow_agent_prompt(prompt, context);
        let mut args = vec![
            agent.to_string(),
            "--task".to_string(),
            self.task_id.clone(),
            "--mode".to_string(),
            mode.to_string(),
            "--effort".to_string(),
            "high".to_string(),
            "--wait".to_string(),
        ];
        if mode == "execute" {
            args.push("--yolo".to_string());
        }
        args.push(prompt);
        match spawn_agent_task_with_runtime(&args, self.settings, self.runtime, true, false) {
            Ok(task_id) => Ok(WorkflowStepOutcome::Success(serde_json::json!({
                "task_id": task_id,
                "agent": agent,
                "mode": mode
            }))),
            Err(err) => Ok(WorkflowStepOutcome::Failure(err.to_string())),
        }
    }

    fn run_tool_node(
        &self,
        tool_id: &str,
        arguments: serde_json::Value,
    ) -> Result<WorkflowStepOutcome> {
        let (registry, _) = build_registry(self.settings)?;
        let snapshot = registry.snapshot();
        let Some(registered) = snapshot.get(tool_id) else {
            return Ok(WorkflowStepOutcome::Failure(format!(
                "unknown workflow tool {tool_id}"
            )));
        };
        let call = ToolCall {
            id: format!("workflow-{}-{}", self.task_id, tool_id.replace('.', "_")),
            tool_id: tool_id.to_string(),
            name: registered.descriptor.name.clone(),
            arguments,
        };
        let result =
            execute_tool_call_with_approval(self.settings, &snapshot, &call, self.auto_approve)?;
        if tool_id == "verification.project_check" {
            let task = self.runtime.get(&self.task_id)?;
            let record = verification_record_from_tool_result(result.clone(), &task.cwd);
            let gate = record
                .report
                .as_ref()
                .map(|report| report.gate.decision)
                .unwrap_or(if record.passed {
                    VerificationGateDecision::Pass
                } else {
                    VerificationGateDecision::Block
                });
            self.runtime
                .record_verification(&self.task_id, record.clone())?;
            return Ok(match gate {
                VerificationGateDecision::Pass => WorkflowStepOutcome::Success(serde_json::json!({
                    "tool_id": tool_id,
                    "metadata": result.metadata,
                    "gate": "pass"
                })),
                VerificationGateDecision::Block => WorkflowStepOutcome::Failure(
                    record
                        .report
                        .as_ref()
                        .map(|report| report.summary.clone())
                        .unwrap_or(result.content),
                ),
                VerificationGateDecision::NeedsReview => WorkflowStepOutcome::Failure(
                    record
                        .report
                        .as_ref()
                        .map(|report| format!("verification needs review: {}", report.summary))
                        .unwrap_or_else(|| "verification needs review".to_string()),
                ),
            });
        }
        if result.is_error {
            Ok(WorkflowStepOutcome::Failure(result.content))
        } else {
            Ok(WorkflowStepOutcome::Success(serde_json::json!({
                "tool_id": tool_id,
                "metadata": result.metadata
            })))
        }
    }

    fn run_research_read_papers(
        &self,
        max_papers: usize,
        context: &WorkflowContext,
    ) -> Result<WorkflowStepOutcome> {
        let search_content = context
            .outputs
            .get("web_search")
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let candidates = extract_research_paper_candidates(search_content, max_papers.max(1));
        if candidates.is_empty() {
            return Ok(WorkflowStepOutcome::Success(serde_json::json!({
                "content": "No PDF/arXiv paper candidates were found in the web search results. Later synthesis must treat web results as snippets only.",
                "papers": [],
                "read_level": "snippets_only"
            })));
        }

        let mut papers = Vec::new();
        let mut notes = Vec::new();
        for (index, candidate) in candidates.into_iter().enumerate() {
            let title = candidate.title.clone();
            let url = candidate.url.clone();
            let pdf_url = candidate.pdf_url.clone();
            let mut record = serde_json::json!({
                "title": title,
                "url": url,
                "pdf_url": pdf_url,
                "read_level": "not_read",
                "tool": serde_json::Value::Null,
            });
            if let Some(pdf_url) = candidate.pdf_url {
                let call = ToolCall {
                    id: format!("workflow-{}-read_papers-mineru-{}", self.task_id, index + 1),
                    tool_id: "builtin.mineru_parse_document".to_string(),
                    name: "mineru_parse_document".to_string(),
                    arguments: serde_json::json!({
                        "url": pdf_url,
                        "model_version": "vlm",
                        "wait": true,
                        "timeout_seconds": 240
                    }),
                };
                match execute_tool_call_with_approval(
                    self.settings,
                    &build_registry(self.settings)?.0.snapshot(),
                    &call,
                    self.auto_approve,
                ) {
                    Ok(result) if !result.is_error => {
                        let markdown = result
                            .metadata
                            .get("markdown")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if title_needs_replacement(record["title"].as_str().unwrap_or_default()) {
                            if let Some(title) = infer_title_from_text(markdown) {
                                record["title"] = serde_json::json!(title);
                            }
                        }
                        record["read_level"] = serde_json::json!(if markdown.trim().is_empty() {
                            "mineru_parsed_no_markdown"
                        } else {
                            "full_pdf_mineru"
                        });
                        record["tool"] = serde_json::json!("builtin.mineru_parse_document");
                        record["mineru"] = result.metadata.clone();
                        if !markdown.trim().is_empty() {
                            record["content"] = serde_json::json!(preview(markdown, 12000));
                            notes.push(format!(
                                "{}. full PDF parsed with MinerU: {}",
                                index + 1,
                                record["title"].as_str().unwrap_or("paper")
                            ));
                        } else {
                            notes.push(format!(
                                "{}. MinerU parsed PDF but no markdown was returned yet: {}",
                                index + 1,
                                record["title"].as_str().unwrap_or("paper")
                            ));
                        }
                    }
                    Ok(result) => {
                        record["read_level"] = serde_json::json!("mineru_failed");
                        record["tool"] = serde_json::json!("builtin.mineru_parse_document");
                        record["error"] = serde_json::json!(result.content);
                        notes.push(format!(
                            "{}. MinerU failed for {}: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper"),
                            preview(&result.content, 180)
                        ));
                    }
                    Err(err) => {
                        record["read_level"] = serde_json::json!("mineru_failed");
                        record["tool"] = serde_json::json!("builtin.mineru_parse_document");
                        record["error"] = serde_json::json!(err.to_string());
                        notes.push(format!(
                            "{}. MinerU failed for {}: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper"),
                            err
                        ));
                    }
                }
            } else {
                let call = ToolCall {
                    id: format!(
                        "workflow-{}-read_papers-extract-{}",
                        self.task_id,
                        index + 1
                    ),
                    tool_id: "builtin.anysearch".to_string(),
                    name: "anysearch".to_string(),
                    arguments: serde_json::json!({
                        "action": "extract",
                        "url": record["url"].as_str().unwrap_or_default()
                    }),
                };
                match execute_tool_call_with_approval(
                    self.settings,
                    &build_registry(self.settings)?.0.snapshot(),
                    &call,
                    self.auto_approve,
                ) {
                    Ok(result) if !result.is_error => {
                        if title_needs_replacement(record["title"].as_str().unwrap_or_default()) {
                            if let Some(title) = infer_title_from_text(&result.content) {
                                record["title"] = serde_json::json!(title);
                            }
                        }
                        record["read_level"] = serde_json::json!("web_extract");
                        record["tool"] = serde_json::json!("builtin.anysearch.extract");
                        record["content"] = serde_json::json!(preview(&result.content, 8000));
                        notes.push(format!(
                            "{}. extracted web page text, not a PDF full read: {}",
                            index + 1,
                            record["title"].as_str().unwrap_or("paper")
                        ));
                    }
                    Ok(result) => {
                        record["read_level"] = serde_json::json!("extract_failed");
                        record["tool"] = serde_json::json!("builtin.anysearch.extract");
                        record["error"] = serde_json::json!(result.content);
                    }
                    Err(err) => {
                        record["read_level"] = serde_json::json!("extract_failed");
                        record["tool"] = serde_json::json!("builtin.anysearch.extract");
                        record["error"] = serde_json::json!(err.to_string());
                    }
                }
            }
            papers.push(record);
        }

        Ok(WorkflowStepOutcome::Success(serde_json::json!({
            "content": notes.join("\n"),
            "papers": papers,
            "read_level": if notes.iter().any(|note| note.contains("full PDF parsed")) { "full_or_partial_pdf" } else { "extract_or_snippets_only" }
        })))
    }

    fn run_approval_node(&self, prompt: &str) -> Result<WorkflowStepOutcome> {
        if self.auto_approve {
            return Ok(WorkflowStepOutcome::Success(
                serde_json::json!({ "approved": true }),
            ));
        }
        let call = ToolCall {
            id: format!("workflow-approval-{}", self.task_id),
            tool_id: "workflow.approval".to_string(),
            name: "workflow_approval".to_string(),
            arguments: serde_json::json!({ "prompt": prompt }),
        };
        if StdinPrompter.ask_user(prompt, &call) {
            Ok(WorkflowStepOutcome::Success(
                serde_json::json!({ "approved": true }),
            ))
        } else {
            Ok(WorkflowStepOutcome::Interrupt {
                prompt: prompt.to_string(),
                reason: "workflow approval was not granted".to_string(),
            })
        }
    }
}

fn workflow_agent_prompt(prompt: &str, context: &WorkflowContext) -> String {
    if context.outputs.is_empty() {
        return prompt.to_string();
    }
    let outputs =
        serde_json::to_string_pretty(&context.outputs).unwrap_or_else(|_| "{}".to_string());
    format!(
        "{prompt}\n\nPrior workflow node outputs follow. Use them as evidence and do not pretend unavailable data was collected:\n```json\n{}\n```",
        preview(&outputs, 12000)
    )
}

fn run_workflow_for_task(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    task_id: &str,
    workflow: GraphWorkflow,
    auto_approve: bool,
    resume: Option<(String, WorkflowContext)>,
) -> Result<WorkflowRunSummary> {
    let mut runner = RuntimeWorkflowRunner {
        settings,
        runtime,
        task_id: task_id.to_string(),
        auto_approve,
    };
    let executor = WorkflowExecutor::new();
    match resume {
        Some((start, context)) => executor.run_from(&workflow, start, context, &mut runner),
        None => executor.run(&workflow, &mut runner),
    }
}

fn resume_workflow_task(
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    task_id: &str,
    auto_approve: bool,
) -> Result<WorkflowRunSummary> {
    let workflow = load_workflow(runtime, task_id)?;
    let prior = load_workflow_summary(runtime, task_id).ok();
    let resume_start = prior
        .as_ref()
        .and_then(|summary| summary.interrupt.as_ref())
        .and_then(|interrupt| {
            workflow.next_node(&interrupt.node_id, WorkflowEdgeCondition::OnSuccess)
        });
    let context = prior.as_ref().map(|summary| WorkflowContext {
        variables: Default::default(),
        outputs: summary.outputs.clone(),
    });
    let summary = run_workflow_for_task(
        settings,
        runtime,
        task_id,
        workflow,
        auto_approve,
        resume_start.zip(context),
    )?;
    let workflow = load_workflow(runtime, task_id)?;
    persist_workflow_artifacts(runtime, task_id, &workflow, Some(&summary))?;
    finalize_workflow_task(runtime, task_id, &summary)?;
    Ok(summary)
}

fn finalize_workflow_task(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    summary: &WorkflowRunSummary,
) -> Result<()> {
    let status = match summary.status {
        WorkflowStatus::Completed => RuntimeTaskStatus::Completed,
        WorkflowStatus::Failed | WorkflowStatus::MaxStepsReached => RuntimeTaskStatus::Failed,
        WorkflowStatus::Interrupted => RuntimeTaskStatus::Pending,
    };
    runtime.mark_task_status(
        task_id,
        status,
        serde_json::json!({
            "workflow": {
                "name": summary.workflow_name.clone(),
                "status": format!("{:?}", summary.status),
                "visited": summary.visited.clone(),
                "interrupt": summary.interrupt.clone()
            },
            "review_recommendation": {
                "required": summary.status != WorkflowStatus::Completed,
                "reason": format!("workflow ended with {:?}", summary.status)
            }
        }),
    )
}

fn persist_workflow_artifacts(
    runtime: &RuntimeTaskManager,
    task_id: &str,
    workflow: &GraphWorkflow,
    summary: Option<&WorkflowRunSummary>,
) -> Result<()> {
    let dir = runtime.task_dir(task_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("workflow.json"),
        serde_json::to_string_pretty(workflow)?,
    )?;
    std::fs::write(dir.join("workflow.mmd"), workflow_mermaid(workflow))?;
    if let Some(summary) = summary {
        std::fs::write(
            dir.join("workflow_state.json"),
            serde_json::to_string_pretty(summary)?,
        )?;
    }
    Ok(())
}

fn load_workflow(runtime: &RuntimeTaskManager, task_id: &str) -> Result<GraphWorkflow> {
    Ok(serde_json::from_slice(&std::fs::read(
        runtime.task_dir(task_id).join("workflow.json"),
    )?)?)
}

fn load_workflow_summary(
    runtime: &RuntimeTaskManager,
    task_id: &str,
) -> Result<WorkflowRunSummary> {
    Ok(serde_json::from_slice(&std::fs::read(
        runtime.task_dir(task_id).join("workflow_state.json"),
    )?)?)
}

fn set_workflow_agent(workflow: &mut GraphWorkflow, agent: &str) {
    for node in workflow.nodes.values_mut() {
        if let WorkflowNodeKind::AgentTask {
            agent: node_agent, ..
        } = &mut node.kind
        {
            *node_agent = agent.to_string();
        }
    }
}

fn workflow_mermaid(workflow: &GraphWorkflow) -> String {
    let mut out = String::from("flowchart TD\n");
    for node in workflow.nodes.values() {
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            sanitize_mermaid_id(&node.id),
            node.label.replace('"', "'")
        ));
    }
    for edge in &workflow.edges {
        out.push_str(&format!(
            "  {} -->|{:?}| {}\n",
            sanitize_mermaid_id(&edge.from),
            edge.condition,
            sanitize_mermaid_id(&edge.to)
        ));
    }
    out
}

fn sanitize_mermaid_id(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn format_workflow_summary(summary: &WorkflowRunSummary) -> String {
    let mut out = format!(
        "workflow: {}\nstatus: {:?}\nvisited: {}\n",
        summary.workflow_name,
        summary.status,
        summary.visited.join(" -> ")
    );
    if let Some(interrupt) = &summary.interrupt {
        out.push_str(&format!(
            "interrupt: {}\nreason: {}\n",
            interrupt.prompt, interrupt.reason
        ));
    }
    out
}

fn split_task_verify_args(args: &[String]) -> (Option<String>, Vec<&str>) {
    let Some(first) = args.first() else {
        return (None, Vec::new());
    };
    if first.starts_with('-') {
        return (None, args.iter().map(String::as_str).collect());
    }
    (
        Some(first.clone()),
        args[1..].iter().map(String::as_str).collect(),
    )
}

fn execute_task_verification(
    settings: &Settings,
    task: &RuntimeTask,
    args: &[String],
) -> Result<VerificationRecord> {
    let mut effective_args = args.to_vec();
    if !verify_args_has_cwd(&effective_args) {
        effective_args.push("--cwd".to_string());
        effective_args.push(task.cwd.display().to_string());
    }
    let result = execute_verify_args(&effective_args, settings)?;
    Ok(verification_record_from_tool_result(result, &task.cwd))
}

fn verify_args_has_cwd(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--cwd" || arg == "-C" || arg.starts_with("--cwd="))
}

fn verification_record_from_tool_result(result: ToolResult, cwd: &Path) -> VerificationRecord {
    let metadata = result.metadata.clone();
    let report = verification_report_from_metadata(&metadata).unwrap_or_else(|| {
        legacy_verification_report(
            "verification.project_check",
            cwd.display().to_string(),
            !result.is_error
                && metadata
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            result.content.clone(),
            metadata.clone(),
        )
    });
    VerificationRecord {
        passed: report.passed(),
        content: result.content,
        metadata,
        report: Some(report),
    }
}

fn run_session_cli(args: &[String]) -> Result<()> {
    let settings = Settings::load()?;
    let store = SessionStore::new(settings.pwcli_home.clone());
    match args.first().map(String::as_str).unwrap_or("list") {
        "help" | "--help" | "-h" => {
            println!("{}", help_text(Some("sessions")));
        }
        "list" | "ls" => {
            let sessions = store.list()?;
            println!("{}", format_session_list(&sessions));
        }
        "show" => {
            let selector = args.get(1).map(String::as_str).unwrap_or("last");
            match store.get(selector)? {
                Some(record) => println!("{}", format_session_record(&record)),
                None => eprintln!("session not found: {selector}"),
            }
        }
        "delete" | "rm" => {
            let Some(selector) = args.get(1) else {
                eprintln!("{}", help_text(Some("sessions")));
                return Ok(());
            };
            match store.delete(selector)? {
                Some(entry) => println!("deleted {}", entry.id),
                None => eprintln!("session not found: {selector}"),
            }
        }
        _ => eprintln!("{}", help_text(Some("sessions"))),
    }
    Ok(())
}

fn run_config_cli(args: &[String]) -> Result<()> {
    let mut settings = Settings::load()?;
    println!("{}", config_command_text(args, &mut settings)?);
    Ok(())
}

fn save_normalized_settings(settings: &mut Settings) -> Result<()> {
    settings.normalize();
    settings.save_default()
}

fn config_command_text(args: &[String], settings: &mut Settings) -> Result<String> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "add-provider" => config_add_provider(&args[1..], settings),
        "add-model" => config_add_model(&args[1..], settings),
        "update-provider" | "set-provider" => config_update_provider(&args[1..], settings),
        "update-model" | "set-model" => config_update_model(&args[1..], settings),
        _ => config_report_text(args, settings),
    }
}

fn config_report_text(args: &[String], settings: &Settings) -> Result<String> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "path" => Ok(settings
            .pwcli_home
            .join("config.json")
            .display()
            .to_string()),
        "show" | "cat" | "json" => {
            let value = redacted_settings_json(settings)?;
            Ok(serde_json::to_string_pretty(&value)?)
        }
        "validate" | "check" => Ok(format_config_validation(settings)),
        "help" | "--help" | "-h" => Ok(help_text(Some("config"))),
        other => Ok(format!(
            "unknown config command: {other}\n\n{}",
            help_text(Some("config"))
        )),
    }
}

#[derive(Default)]
struct AddProviderArgs {
    name: String,
    protocol: Option<ProviderProtocol>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_thinking: bool,
    supports_image_input: bool,
    is_image_generation: bool,
    set_active: bool,
    replace: bool,
}

#[derive(Default)]
struct AddModelArgs {
    provider: String,
    name: String,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_thinking: bool,
    supports_image_input: bool,
    is_image_generation: bool,
    set_active: bool,
    replace: bool,
}

#[derive(Default)]
struct UpdateProviderArgs {
    name: String,
    protocol: Option<ProviderProtocol>,
    base_url: Option<String>,
    api_key: Option<String>,
    clear_api_key: bool,
    set_active: bool,
}

#[derive(Default)]
struct UpdateModelArgs {
    provider: String,
    name: String,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_thinking: Option<bool>,
    supports_image_input: Option<bool>,
    is_image_generation: Option<bool>,
    set_active: bool,
}

fn config_add_provider(args: &[String], settings: &mut Settings) -> Result<String> {
    let parsed = parse_add_provider_args(args)?;
    let provider_name = parsed.name.clone();
    let model_name = parsed.model.clone();
    let provider = ProviderSettings {
        name: parsed.name,
        protocol: parsed.protocol.unwrap_or_default(),
        base_url: parsed.base_url.ok_or_else(|| {
            pwcli::PwError::Message("usage: pwcli config add-provider <name> --protocol <openai|anthropic|nvidia> --base-url <url> [--api-key <key>] [--model <name>]".to_string())
        })?,
        api_key: parsed.api_key,
        api_key_env: None,
        api: OpenAiApiKind::ChatCompletions,
        request_timeout_seconds: 0,
        stream: false,
        extra_body: serde_json::json!({}),
        models: model_name
            .as_ref()
            .map(|name| {
                vec![model_from_args(
                    name,
                    parsed.max_input_tokens,
                    parsed.max_output_tokens,
                    parsed.supports_thinking,
                    parsed.supports_image_input,
                    parsed.is_image_generation,
                )]
            })
            .unwrap_or_default(),
    };

    let existing_idx = settings
        .providers
        .iter()
        .position(|provider| provider.name == provider_name);
    let resulting_provider_count = if existing_idx.is_some() {
        settings.providers.len()
    } else {
        settings.providers.len() + 1
    };
    let will_be_active = parsed.set_active
        || settings.provider.is_empty()
        || resulting_provider_count == 1
        || settings.provider == provider_name;
    if will_be_active && provider.models.is_empty() {
        return Err(pwcli::PwError::Message(format!(
            "provider '{provider_name}' has no models; pass --model <name> or add a model before activating it"
        )));
    }
    match existing_idx {
        Some(idx) if parsed.replace => settings.providers[idx] = provider,
        Some(_) => {
            return Err(pwcli::PwError::Message(format!(
                "provider '{provider_name}' already exists; pass --replace to overwrite"
            )))
        }
        None => settings.providers.push(provider),
    }

    if will_be_active {
        settings.set_provider(&provider_name)?;
        if let Some(model) = model_name.as_deref() {
            settings.set_model(model)?;
        }
    }
    save_normalized_settings(settings)?;
    Ok(format!(
        "provider saved: {provider_name}\nconfig: {}",
        settings.pwcli_home.join("config.json").display()
    ))
}

fn config_add_model(args: &[String], settings: &mut Settings) -> Result<String> {
    let parsed = parse_add_model_args(args)?;
    let provider = settings
        .providers
        .iter_mut()
        .find(|provider| provider.name == parsed.provider)
        .ok_or_else(|| {
            pwcli::PwError::Message(format!("unknown provider '{}'", parsed.provider))
        })?;
    let model = model_from_args(
        &parsed.name,
        parsed.max_input_tokens,
        parsed.max_output_tokens,
        parsed.supports_thinking,
        parsed.supports_image_input,
        parsed.is_image_generation,
    );
    let existing_idx = provider
        .models
        .iter()
        .position(|model| model.name == parsed.name);
    match existing_idx {
        Some(idx) if parsed.replace => provider.models[idx] = model,
        Some(_) => {
            return Err(pwcli::PwError::Message(format!(
                "model '{}' already exists for provider '{}'; pass --replace to overwrite",
                parsed.name, parsed.provider
            )))
        }
        None => provider.models.push(model),
    }

    if parsed.set_active {
        settings.set_provider(&parsed.provider)?;
        settings.set_model(&parsed.name)?;
    }
    save_normalized_settings(settings)?;
    Ok(format!(
        "model saved: {}/{}\nconfig: {}",
        parsed.provider,
        parsed.name,
        settings.pwcli_home.join("config.json").display()
    ))
}

fn config_update_provider(args: &[String], settings: &mut Settings) -> Result<String> {
    let parsed = parse_update_provider_args(args)?;
    {
        let provider = settings
            .providers
            .iter_mut()
            .find(|provider| provider.name == parsed.name)
            .ok_or_else(|| {
                pwcli::PwError::Message(format!("unknown provider '{}'", parsed.name))
            })?;
        if let Some(protocol) = parsed.protocol {
            provider.protocol = protocol;
        }
        if let Some(base_url) = parsed.base_url {
            provider.base_url = base_url;
        }
        if parsed.clear_api_key {
            provider.api_key = None;
        }
        if let Some(api_key) = parsed.api_key {
            provider.api_key = Some(api_key);
        }
    }
    if parsed.set_active {
        settings.set_provider(&parsed.name)?;
    }
    save_normalized_settings(settings)?;
    Ok(format!(
        "provider updated: {}\nconfig: {}",
        parsed.name,
        settings.pwcli_home.join("config.json").display()
    ))
}

fn config_update_model(args: &[String], settings: &mut Settings) -> Result<String> {
    let parsed = parse_update_model_args(args)?;
    let provider = settings
        .providers
        .iter_mut()
        .find(|provider| provider.name == parsed.provider)
        .ok_or_else(|| {
            pwcli::PwError::Message(format!("unknown provider '{}'", parsed.provider))
        })?;
    let model = provider
        .models
        .iter_mut()
        .find(|model| model.name == parsed.name)
        .ok_or_else(|| {
            pwcli::PwError::Message(format!(
                "unknown model '{}' for provider '{}'",
                parsed.name, parsed.provider
            ))
        })?;
    if let Some(value) = parsed.max_input_tokens {
        model.max_input_tokens = value;
    }
    if let Some(value) = parsed.max_output_tokens {
        model.max_output_tokens = value;
    }
    if let Some(value) = parsed.supports_thinking {
        model.supports_thinking = value;
    }
    if let Some(value) = parsed.supports_image_input {
        model.supports_image_input = value;
    }
    if let Some(value) = parsed.is_image_generation {
        model.is_image_generation = value;
    }

    if parsed.set_active {
        settings.set_provider(&parsed.provider)?;
        settings.set_model(&parsed.name)?;
    }
    save_normalized_settings(settings)?;
    Ok(format!(
        "model updated: {}/{}\nconfig: {}",
        parsed.provider,
        parsed.name,
        settings.pwcli_home.join("config.json").display()
    ))
}

fn model_from_args(
    name: &str,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_thinking: bool,
    supports_image_input: bool,
    is_image_generation: bool,
) -> ModelDefinition {
    ModelDefinition {
        name: name.to_string(),
        supports_image_input,
        supports_thinking,
        is_image_generation,
        max_input_tokens: max_input_tokens.unwrap_or(128_000),
        max_output_tokens: max_output_tokens.unwrap_or(4096),
        extra_body: serde_json::json!({}),
    }
}

fn parse_add_provider_args(args: &[String]) -> Result<AddProviderArgs> {
    let mut parsed = AddProviderArgs::default();
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if parsed.name.is_empty() && !arg.starts_with('-') {
            parsed.name = arg.clone();
            idx += 1;
            continue;
        }
        match arg.as_str() {
            "--protocol" | "--format" => {
                parsed.protocol = Some(parse_provider_protocol(required_arg(args, idx, arg)?)?);
                idx += 2;
            }
            "--base-url" | "--base_url" => {
                parsed.base_url = Some(required_arg(args, idx, arg)?.to_string());
                idx += 2;
            }
            "--api-key" | "--api_key" => {
                parsed.api_key = Some(required_arg(args, idx, arg)?.to_string());
                idx += 2;
            }
            "--model" => {
                parsed.model = Some(required_arg(args, idx, arg)?.to_string());
                idx += 2;
            }
            "--input" | "--max-input" | "--max-input-tokens" => {
                parsed.max_input_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--output" | "--max-output" | "--max-output-tokens" => {
                parsed.max_output_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--thinking" => {
                parsed.supports_thinking = true;
                idx += 1;
            }
            "--image-input" => {
                parsed.supports_image_input = true;
                idx += 1;
            }
            "--image-generation" => {
                parsed.is_image_generation = true;
                idx += 1;
            }
            "--set-active" | "--active" => {
                parsed.set_active = true;
                idx += 1;
            }
            "--replace" => {
                parsed.replace = true;
                idx += 1;
            }
            value if value.starts_with("--protocol=") || value.starts_with("--format=") => {
                parsed.protocol = Some(parse_provider_protocol(value_after_equals(value)?)?);
                idx += 1;
            }
            value if value.starts_with("--base-url=") || value.starts_with("--base_url=") => {
                parsed.base_url = Some(value_after_equals(value)?.to_string());
                idx += 1;
            }
            value if value.starts_with("--api-key=") || value.starts_with("--api_key=") => {
                parsed.api_key = Some(value_after_equals(value)?.to_string());
                idx += 1;
            }
            value if value.starts_with("--model=") => {
                parsed.model = Some(value_after_equals(value)?.to_string());
                idx += 1;
            }
            value
                if value.starts_with("--input=")
                    || value.starts_with("--max-input=")
                    || value.starts_with("--max-input-tokens=") =>
            {
                parsed.max_input_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            value
                if value.starts_with("--output=")
                    || value.starts_with("--max-output=")
                    || value.starts_with("--max-output-tokens=") =>
            {
                parsed.max_output_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown add-provider option '{other}'"
                )))
            }
        }
    }
    if parsed.name.trim().is_empty() {
        return Err(pwcli::PwError::Message("usage: pwcli config add-provider <name> --protocol <openai|anthropic|nvidia> --base-url <url> [--api-key <key>] [--model <name>]".to_string()));
    }
    Ok(parsed)
}

fn parse_add_model_args(args: &[String]) -> Result<AddModelArgs> {
    let mut parsed = AddModelArgs::default();
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if parsed.provider.is_empty() && !arg.starts_with('-') {
            parsed.provider = arg.clone();
            idx += 1;
            continue;
        }
        if parsed.name.is_empty() && !arg.starts_with('-') {
            parsed.name = arg.clone();
            idx += 1;
            continue;
        }
        match arg.as_str() {
            "--input" | "--max-input" | "--max-input-tokens" => {
                parsed.max_input_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--output" | "--max-output" | "--max-output-tokens" => {
                parsed.max_output_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--thinking" => {
                parsed.supports_thinking = true;
                idx += 1;
            }
            "--image-input" => {
                parsed.supports_image_input = true;
                idx += 1;
            }
            "--image-generation" => {
                parsed.is_image_generation = true;
                idx += 1;
            }
            "--set-active" | "--active" => {
                parsed.set_active = true;
                idx += 1;
            }
            "--replace" => {
                parsed.replace = true;
                idx += 1;
            }
            value
                if value.starts_with("--input=")
                    || value.starts_with("--max-input=")
                    || value.starts_with("--max-input-tokens=") =>
            {
                parsed.max_input_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            value
                if value.starts_with("--output=")
                    || value.starts_with("--max-output=")
                    || value.starts_with("--max-output-tokens=") =>
            {
                parsed.max_output_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown add-model option '{other}'"
                )))
            }
        }
    }
    if parsed.provider.trim().is_empty() || parsed.name.trim().is_empty() {
        return Err(pwcli::PwError::Message("usage: pwcli config add-model <provider> <model> [--input <tokens>] [--output <tokens>] [--thinking] [--image-input] [--image-generation] [--set-active]".to_string()));
    }
    Ok(parsed)
}

fn parse_update_provider_args(args: &[String]) -> Result<UpdateProviderArgs> {
    let mut parsed = UpdateProviderArgs::default();
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if parsed.name.is_empty() && !arg.starts_with('-') {
            parsed.name = arg.clone();
            idx += 1;
            continue;
        }
        match arg.as_str() {
            "--protocol" | "--format" => {
                parsed.protocol = Some(parse_provider_protocol(required_arg(args, idx, arg)?)?);
                idx += 2;
            }
            "--base-url" | "--base_url" => {
                parsed.base_url = Some(required_arg(args, idx, arg)?.to_string());
                idx += 2;
            }
            "--api-key" | "--api_key" => {
                parsed.api_key = Some(required_arg(args, idx, arg)?.to_string());
                idx += 2;
            }
            "--clear-api-key" | "--clear_api_key" => {
                parsed.clear_api_key = true;
                idx += 1;
            }
            "--set-active" | "--active" => {
                parsed.set_active = true;
                idx += 1;
            }
            value if value.starts_with("--protocol=") || value.starts_with("--format=") => {
                parsed.protocol = Some(parse_provider_protocol(value_after_equals(value)?)?);
                idx += 1;
            }
            value if value.starts_with("--base-url=") || value.starts_with("--base_url=") => {
                parsed.base_url = Some(value_after_equals(value)?.to_string());
                idx += 1;
            }
            value if value.starts_with("--api-key=") || value.starts_with("--api_key=") => {
                parsed.api_key = Some(value_after_equals(value)?.to_string());
                idx += 1;
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown update-provider option '{other}'"
                )))
            }
        }
    }
    if parsed.name.trim().is_empty() {
        return Err(pwcli::PwError::Message("usage: pwcli config update-provider <name> [--protocol <openai|anthropic|nvidia>] [--base-url <url>] [--api-key <key>|--clear-api-key] [--set-active]".to_string()));
    }
    Ok(parsed)
}

fn parse_update_model_args(args: &[String]) -> Result<UpdateModelArgs> {
    let mut parsed = UpdateModelArgs::default();
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if parsed.provider.is_empty() && !arg.starts_with('-') {
            parsed.provider = arg.clone();
            idx += 1;
            continue;
        }
        if parsed.name.is_empty() && !arg.starts_with('-') {
            parsed.name = arg.clone();
            idx += 1;
            continue;
        }
        match arg.as_str() {
            "--input" | "--max-input" | "--max-input-tokens" => {
                parsed.max_input_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--output" | "--max-output" | "--max-output-tokens" => {
                parsed.max_output_tokens = Some(parse_u32_arg(args, idx, arg)?);
                idx += 2;
            }
            "--thinking" => {
                parsed.supports_thinking = Some(true);
                idx += 1;
            }
            "--no-thinking" => {
                parsed.supports_thinking = Some(false);
                idx += 1;
            }
            "--image-input" => {
                parsed.supports_image_input = Some(true);
                idx += 1;
            }
            "--no-image-input" => {
                parsed.supports_image_input = Some(false);
                idx += 1;
            }
            "--image-generation" => {
                parsed.is_image_generation = Some(true);
                idx += 1;
            }
            "--no-image-generation" => {
                parsed.is_image_generation = Some(false);
                idx += 1;
            }
            "--set-active" | "--active" => {
                parsed.set_active = true;
                idx += 1;
            }
            value
                if value.starts_with("--input=")
                    || value.starts_with("--max-input=")
                    || value.starts_with("--max-input-tokens=") =>
            {
                parsed.max_input_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            value
                if value.starts_with("--output=")
                    || value.starts_with("--max-output=")
                    || value.starts_with("--max-output-tokens=") =>
            {
                parsed.max_output_tokens = Some(parse_u32_value(value_after_equals(value)?, arg)?);
                idx += 1;
            }
            other => {
                return Err(pwcli::PwError::Message(format!(
                    "unknown update-model option '{other}'"
                )))
            }
        }
    }
    if parsed.provider.trim().is_empty() || parsed.name.trim().is_empty() {
        return Err(pwcli::PwError::Message("usage: pwcli config update-model <provider> <model> [--input <tokens>] [--output <tokens>] [--thinking|--no-thinking] [--image-input|--no-image-input] [--image-generation|--no-image-generation] [--set-active]".to_string()));
    }
    Ok(parsed)
}

fn required_arg<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str> {
    args.get(idx + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| pwcli::PwError::Message(format!("{flag} requires a value")))
}

fn value_after_equals(value: &str) -> Result<&str> {
    value
        .split_once('=')
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| pwcli::PwError::Message(format!("{value} requires a value after '='")))
}

fn parse_u32_arg(args: &[String], idx: usize, flag: &str) -> Result<u32> {
    parse_u32_value(required_arg(args, idx, flag)?, flag)
}

fn parse_u32_value(value: &str, flag: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| pwcli::PwError::Message(format!("{flag} must be an integer")))
}

fn parse_provider_protocol(value: &str) -> Result<ProviderProtocol> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" | "open_ai" => Ok(ProviderProtocol::OpenAi),
        "anthropic" | "anthrophic" => Ok(ProviderProtocol::Anthropic),
        "nvidia" => Ok(ProviderProtocol::Nvidia),
        other => Err(pwcli::PwError::Message(format!(
            "unknown provider protocol '{other}'; expected openai, anthropic, or nvidia"
        ))),
    }
}

fn redacted_settings_json(settings: &Settings) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(settings)?;
    redact_sensitive_json(&mut value);
    Ok(value)
}

fn redact_sensitive_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if is_sensitive_config_key(key) {
                    if !child.is_null() {
                        *child = serde_json::Value::String("configured".to_string());
                    }
                } else {
                    redact_sensitive_json(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_sensitive_json(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_config_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key"
            | "token"
            | "secret"
            | "password"
            | "access_token"
            | "refresh_token"
            | "authorization"
            | "x-api-key"
    )
}

fn format_config_validation(settings: &Settings) -> String {
    let mut out = String::new();
    out.push_str("pwcli config validate\n");
    out.push_str(&format!(
        "path: {}\n",
        settings.pwcli_home.join("config.json").display()
    ));
    out.push_str(&format!("active_provider: {}\n", settings.provider));
    out.push_str(&format!("active_model: {}\n", settings.model));
    out.push_str(&format!("providers: {}\n", settings.providers.len()));

    if settings.providers.is_empty() {
        out.push_str("[fail] providers: none configured\n");
    }

    match settings.active_provider() {
        Ok(provider) => {
            out.push_str(&format!(
                "[ok] active provider: {} ({})\n",
                provider.name,
                provider.protocol.as_str()
            ));
        }
        Err(err) => out.push_str(&format!("[fail] active provider: {err}\n")),
    }

    match settings.active_model() {
        Ok(model) => {
            out.push_str(&format!(
                "[ok] active model: {} input={} output={} thinking={} image_input={} image_generation={}\n",
                model.name,
                model.max_input_tokens,
                model.max_output_tokens,
                model.supports_thinking,
                model.supports_image_input,
                model.is_image_generation
            ));
        }
        Err(err) => out.push_str(&format!("[fail] active model: {err}\n")),
    }

    match settings.resolved_model_settings() {
        Ok(model) => out.push_str(&format!(
            "[ok] resolved model: provider={} protocol={} base_url={} thinking_enabled={} context_input={}\n",
            model.provider_name,
            model.provider.as_str(),
            model.base_url,
            model.thinking_enabled,
            model.max_input_tokens
        )),
        Err(err) => out.push_str(&format!("[fail] resolved model: {err}\n")),
    }

    for provider in &settings.providers {
        out.push_str(&format!(
            "provider {}: protocol={} base_url={} models={} key={}\n",
            provider.name,
            provider.protocol.as_str(),
            provider.base_url,
            provider.models.len(),
            provider_key_state(provider)
        ));
        if provider.name.trim().is_empty() {
            out.push_str("  [fail] provider name is empty\n");
        }
        if provider.base_url.trim().is_empty() {
            out.push_str("  [fail] base_url is empty\n");
        }
        if provider.models.is_empty() {
            out.push_str("  [warn] no models configured\n");
        }
    }

    out.push_str(&format!(
        "context: max_input_tokens={} keep_recent_turns={}\n",
        settings.context.max_input_tokens, settings.context.keep_recent_turns
    ));
    out.push_str(&format!(
        "memory: enabled={} auto_consider_write={} semantic_extraction={} embedding={} model={}\n",
        settings.memory.enabled,
        settings.memory.auto_consider_write,
        settings.memory.semantic_extraction.enabled,
        settings.memory.embedding.enabled,
        settings.memory.embedding.model
    ));
    out.push_str(&format!(
        "mineru: base_url={} token={}\n",
        settings.mineru.base_url,
        optional_secret_state(settings.mineru.token.as_deref())
    ));
    out.push_str(&format!(
        "anysearch: endpoint={} api_key={} rate_limit={}/min parallel={}\n",
        settings.anysearch.endpoint,
        optional_secret_state(settings.anysearch.api_key.as_deref()),
        settings.anysearch.rate_limit.max_per_minute,
        settings.anysearch.rate_limit.max_parallel
    ));
    out.push_str(&format!(
        "github: api_url={} token={}\n",
        settings.github.api_url,
        optional_secret_state(settings.github.token.as_deref())
    ));
    out.push_str(&format!("ssh: hosts={}\n", settings.ssh.hosts.len()));
    for host in &settings.ssh.hosts {
        out.push_str(&format!(
            "ssh host {}: target={}:{} user={} auth={} timeout={}s host_key_policy={}\n",
            host.name,
            host.host,
            host.port,
            host.username.as_deref().unwrap_or("default"),
            ssh_auth_state(host),
            host.timeout_seconds,
            if host.accept_unknown_host_key {
                "accept_unknown"
            } else {
                "known_hosts"
            }
        ));
    }
    out.push_str(&format!("mcp: servers={}\n", settings.mcp.servers.len()));
    for server in &settings.mcp.servers {
        out.push_str(&format!(
            "mcp server {}: enabled={} transport={:?} timeout={}s endpoint={}\n",
            server.name,
            server.enabled,
            server.transport,
            server.timeout_seconds,
            server
                .url
                .as_deref()
                .or(server.command.as_deref())
                .unwrap_or("missing")
        ));
    }
    out.push_str(&format!(
        "tools: allowlist={} denylist={} disabled={} risk_overrides={} approval_overrides={} network_policy={:?}\n",
        settings.tools.allowlist.len(),
        settings.tools.denylist.len(),
        settings.tools.disabled.len(),
        settings.tools.risk_overrides.len(),
        settings.tools.approval_overrides.len(),
        settings.tools.network_policy
    ));
    out
}

fn ssh_auth_state(host: &pwcli::settings::SshHostSettings) -> &'static str {
    if host.private_key_path.is_some() {
        "key"
    } else if host.password_env.is_some() {
        "password_env"
    } else {
        "missing"
    }
}

fn provider_key_state(provider: &ProviderSettings) -> String {
    if provider
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
    {
        return "config".to_string();
    }
    let env_name = provider
        .api_key_env
        .as_deref()
        .unwrap_or(match provider.protocol {
            pwcli::settings::ProviderProtocol::OpenAi => "OPENAI_API_KEY",
            pwcli::settings::ProviderProtocol::Anthropic => "ANTHROPIC_API_KEY",
            pwcli::settings::ProviderProtocol::Nvidia => "NVIDIA_API_KEY",
        });
    if std::env::var(env_name)
        .ok()
        .is_some_and(|key| !key.trim().is_empty())
    {
        format!("env:{env_name}")
    } else {
        format!("missing:{env_name}")
    }
}

fn optional_secret_state(value: Option<&str>) -> &'static str {
    if value.is_some_and(|value| !value.trim().is_empty()) {
        "configured"
    } else {
        "missing"
    }
}

fn wait_for_runtime_task(runtime: &RuntimeTaskManager, task_id: &str) -> Result<()> {
    loop {
        for event in runtime.poll_events() {
            println!("{}", format_runtime_event(&event));
        }
        let task = runtime.get(task_id)?;
        if matches!(
            task.status,
            RuntimeTaskStatus::Completed
                | RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Cancelled
                | RuntimeTaskStatus::TimedOut
        ) {
            print_task_log(runtime, task_id)?;
            println!("\n{}", format_task_next(&task));
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn watch_runtime_task(runtime: &RuntimeTaskManager, task_id: &str) -> Result<()> {
    let mut offset = 0_u64;
    loop {
        let (events, new_offset) = runtime.read_events_from(task_id, offset)?;
        offset = new_offset;
        for event in events {
            println!("{}", format_runtime_event(&event));
        }

        let task = runtime.get(task_id)?;
        if matches!(
            task.status,
            RuntimeTaskStatus::Completed
                | RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Cancelled
                | RuntimeTaskStatus::TimedOut
        ) {
            println!("\n{}", format_task_next(&task));
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[derive(Default)]
struct RuntimeEventTailer {
    offsets: HashMap<String, u64>,
    seen: HashSet<String>,
}

impl RuntimeEventTailer {
    fn remember(&mut self, event: &RuntimeTaskEvent) -> bool {
        self.seen.insert(runtime_event_key(event))
    }

    fn offset_for(&mut self, runtime: &RuntimeTaskManager, task_id: &str) -> u64 {
        let len = std::fs::metadata(runtime.task_dir(task_id).join("events.jsonl"))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let offset = self.offsets.entry(task_id.to_string()).or_insert(len);
        if len < *offset {
            *offset = 0;
        }
        *offset
    }
}

fn runtime_status_messages(
    runtime: &RuntimeTaskManager,
    tailer: &mut RuntimeEventTailer,
) -> Vec<String> {
    let mut messages = Vec::new();

    for event in runtime.poll_events() {
        if tailer.remember(&event) {
            messages.push(format_runtime_event(&event));
        }
    }

    if let Ok(Some(task_id)) = runtime.active_task_id() {
        let offset = tailer.offset_for(runtime, &task_id);
        if let Ok((events, new_offset)) = runtime.read_events_from(&task_id, offset) {
            tailer.offsets.insert(task_id, new_offset);
            for event in events {
                if tailer.remember(&event) {
                    messages.push(format_runtime_event(&event));
                }
            }
        }
    }

    messages
}

#[derive(Default)]
struct SkillInventoryTailer {
    last_fingerprint: Option<u64>,
    last_error: Option<String>,
    last_check: Option<Instant>,
}

fn skill_status_messages(roots: &[PathBuf], tailer: &mut SkillInventoryTailer) -> Vec<String> {
    let now = Instant::now();
    if tailer
        .last_check
        .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
    {
        return Vec::new();
    }
    tailer.last_check = Some(now);

    match scan_skill_roots(roots) {
        Ok(inventory) => {
            tailer.last_error = None;
            let previous = tailer.last_fingerprint.replace(inventory.fingerprint);
            if previous.is_none() || previous == Some(inventory.fingerprint) {
                return Vec::new();
            }

            let failed = inventory
                .health
                .iter()
                .filter(|health| {
                    matches!(
                        health.status,
                        pwcli::tools::skills::watcher::SkillHealthStatus::Fail
                    )
                })
                .count();
            let mut message = format!(
                "skills reloaded: tools={} conflicts={}",
                inventory.tool_ids.len(),
                inventory.conflicts.len()
            );
            if failed > 0 {
                message.push_str(&format!(" failed={failed}"));
            }
            Vec::from([message])
        }
        Err(err) => {
            let message = err.to_string();
            if tailer.last_error.as_deref() == Some(message.as_str()) {
                return Vec::new();
            }
            tailer.last_error = Some(message.clone());
            Vec::from([format!("skills watch failed: {message}")])
        }
    }
}

fn runtime_event_key(event: &RuntimeTaskEvent) -> String {
    serde_json::to_string(event).unwrap_or_else(|_| format!("{event:?}"))
}

fn format_runtime_event(event: &RuntimeTaskEvent) -> String {
    match event {
        RuntimeTaskEvent::Started { task_id } => format!("task started: {task_id}"),
        RuntimeTaskEvent::Progress { task_id, message } => {
            format!("task progress {task_id}: {message}")
        }
        RuntimeTaskEvent::Output {
            task_id,
            stream,
            chunk,
        } => format!("task output {task_id} {stream}: {}", preview(chunk, 240)),
        RuntimeTaskEvent::Structured {
            task_id,
            stream,
            event,
        } => format!(
            "task event {task_id} {stream}: {}",
            preview(&event.to_string(), 240)
        ),
        RuntimeTaskEvent::Completed { task_id, .. } => format!("task completed: {task_id}"),
        RuntimeTaskEvent::Failed { task_id, error } => {
            format!("task failed {task_id}: {}", preview(error, 240))
        }
        RuntimeTaskEvent::Cancelled { task_id } => format!("task cancelled: {task_id}"),
        RuntimeTaskEvent::TimedOut { task_id } => format!("task timed out: {task_id}"),
        RuntimeTaskEvent::CompactCompleted {
            task_id,
            summary_path,
        } => format!("task compacted {task_id}: {}", summary_path.display()),
        RuntimeTaskEvent::VerificationRecorded {
            task_id,
            passed,
            verification_path,
            status,
            gate,
            failed_check_count,
            report_path,
        } => format!(
            "task verification {task_id}: status={} gate={} failed_checks={} {}",
            status
                .as_deref()
                .unwrap_or(if *passed { "passed" } else { "failed" }),
            gate.as_deref()
                .unwrap_or(if *passed { "pass" } else { "block" }),
            failed_check_count,
            report_path.as_ref().unwrap_or(verification_path).display()
        ),
        RuntimeTaskEvent::WorkflowNodeStarted {
            task_id,
            node_id,
            label,
        } => format!("workflow node started {task_id} {node_id}: {label}"),
        RuntimeTaskEvent::WorkflowNodeCompleted {
            task_id,
            node_id,
            status,
        } => format!("workflow node completed {task_id} {node_id}: {status}"),
    }
}

fn print_task_log(runtime: &RuntimeTaskManager, task_id: &str) -> Result<()> {
    println!("{}", task_log_text(runtime, task_id));
    Ok(())
}

fn task_log_text(runtime: &RuntimeTaskManager, task_id: &str) -> String {
    let dir = runtime.task_dir(task_id);
    let stdout = read_text_preview_or_default(&dir.join("stdout.log"), 4000);
    let stderr = read_text_preview_or_default(&dir.join("stderr.log"), 2000);
    let summary = read_text_preview_or_default(&dir.join("summary.md"), 2000);
    format!(
        "stdout:\n{}\n\nstderr:\n{}\n\nsummary:\n{}",
        stdout, stderr, summary
    )
}

fn memory_task_source_text(runtime: &RuntimeTaskManager, task_id: &str) -> Result<String> {
    let task = runtime.get(task_id)?;
    let dir = runtime.task_dir(task_id);
    let stdout = read_text_preview_or_default(&dir.join("stdout.log"), 8000);
    let stderr = read_text_preview_or_default(&dir.join("stderr.log"), 4000);
    let summary = read_text_preview_or_default(&dir.join("summary.md"), 8000);
    let result = read_text_preview_or_default(&dir.join("result.json"), 4000);
    Ok(format!(
        "任务ID：{}\n标题：{}\n状态：{:?}\n类型：{:?}\n工作目录：{}\n\n元数据：\n{}\n\n任务总结：\n{}\n\n结果：\n{}\n\nstdout：\n{}\n\nstderr：\n{}",
        task.task_id,
        task.title,
        task.status,
        task.kind,
        task.cwd.display(),
        serde_json::to_string_pretty(&task.metadata)?,
        summary,
        result,
        stdout,
        stderr
    ))
}

fn task_next_text(runtime: &RuntimeTaskManager, selector: Option<&str>) -> Result<String> {
    let Some(task_id) = runtime.resolve_task_id(selector)? else {
        return Ok(
            "no active task; use `pwcli goal <objective>` or `pwcli task use last`".to_string(),
        );
    };
    let task = runtime.get(&task_id)?;
    Ok(format_task_next(&task))
}

fn preview(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    let mut out = text.chars().take(max_chars).collect::<String>();
    if len > max_chars {
        out.push_str("\n...");
    }
    out
}

#[derive(Debug, Clone)]
struct ResearchPaperCandidate {
    title: String,
    url: String,
    pdf_url: Option<String>,
}

fn extract_research_paper_candidates(
    content: &str,
    max_papers: usize,
) -> Vec<ResearchPaperCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let lines = content.lines().collect::<Vec<_>>();
    for (idx, line) in lines.iter().enumerate() {
        let Some(title) = line.trim().strip_prefix("### ") else {
            continue;
        };
        let title = title
            .split_once(". ")
            .map(|(_, title)| title)
            .unwrap_or(title)
            .trim();
        let mut url = None;
        for lookahead in lines.iter().skip(idx + 1).take(5) {
            let trimmed = lookahead.trim();
            if let Some(found) = trimmed.strip_prefix("- **URL**:") {
                url = Some(found.trim().to_string());
                break;
            }
        }
        let Some(url) = url else {
            continue;
        };
        if !looks_like_paper_result(title, &url) {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        candidates.push(ResearchPaperCandidate {
            title: title.to_string(),
            pdf_url: paper_pdf_url(&url),
            url,
        });
        if candidates.len() >= max_papers {
            break;
        }
    }
    candidates
}

fn looks_like_paper_result(title: &str, url: &str) -> bool {
    let haystack = format!("{} {}", title, url).to_ascii_lowercase();
    haystack.contains("arxiv.org")
        || haystack.contains(".pdf")
        || haystack.contains("paper")
        || haystack.contains("论文")
        || haystack.contains("proceedings")
        || haystack.contains("acm.org")
        || haystack.contains("openreview.net")
}

fn paper_pdf_url(url: &str) -> Option<String> {
    let clean = url.trim().trim_end_matches([')', '.', ',', ';']);
    let lower = clean.to_ascii_lowercase();
    if lower.ends_with(".pdf") || lower.contains(".pdf?") {
        return Some(clean.to_string());
    }
    if let Some(id) = arxiv_id(clean) {
        return Some(format!("https://arxiv.org/pdf/{id}.pdf"));
    }
    None
}

fn arxiv_id(url: &str) -> Option<String> {
    let marker = "arxiv.org/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];
    let rest = rest
        .strip_prefix("abs/")
        .or_else(|| rest.strip_prefix("html/"))
        .or_else(|| rest.strip_prefix("pdf/"))?;
    let mut id = rest
        .split(['?', '#', '/'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if let Some(stripped) = id.strip_suffix(".pdf") {
        id = stripped.to_string();
    }
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn title_needs_replacement(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }
    let normalized = trimmed
        .trim_end_matches(".pdf")
        .trim_matches(|ch: char| ch == '[' || ch == ']' || ch == '(' || ch == ')');
    let has_letter = normalized.chars().any(|ch| ch.is_alphabetic());
    let mostly_id = normalized
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | 'v' | 'V' | '-' | '_'));
    !has_letter || mostly_id
}

fn infer_title_from_text(text: &str) -> Option<String> {
    for line in text.lines().take(80) {
        let title = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_matches(['*', '_']);
        if title.len() < 8 || title.len() > 220 {
            continue;
        }
        if title_needs_replacement(title) {
            continue;
        }
        if title.to_ascii_lowercase().contains("abstract") {
            continue;
        }
        return Some(title.to_string());
    }
    None
}

fn read_text_preview_or_default(path: &std::path::Path, max_bytes: u64) -> String {
    read_text_preview_from_file(path, max_bytes).unwrap_or_default()
}

fn read_text_preview_from_file(path: &std::path::Path, max_bytes: u64) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    let truncated = metadata.len() > max_bytes;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(max_bytes)
        .read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).to_string();
    if truncated {
        text.push_str("\n...[truncated]");
    }
    Ok(text)
}

fn handle_interactive_task_command(
    rest: &str,
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    active_task: &mut Option<String>,
    ui: &mut TerminalUi,
) {
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.first().copied() {
        Some("new") => {
            let goal = args[1..].join(" ");
            if goal.trim().is_empty() {
                ui.push_status("usage: /task new <goal>");
                return;
            }
            match runtime.create_task(
                RuntimeTaskKind::Internal,
                goal.clone(),
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                serde_json::json!({ "goal": goal }),
            ) {
                Ok(task) => {
                    if let Err(err) = runtime.set_active(&task.task_id) {
                        ui.push_status(format!("task created but active update failed: {err}"));
                        return;
                    }
                    *active_task = Some(task.task_id.clone());
                    ui.push_status(format!("active task: {}", task.task_id));
                }
                Err(err) => ui.push_status(format!("task create failed: {err}")),
            }
        }
        Some("use") => {
            let selector = args.get(1).copied().unwrap_or("last");
            match runtime.set_active(selector) {
                Ok(()) => match runtime.active_task_id() {
                    Ok(Some(task_id)) => {
                        *active_task = Some(task_id.clone());
                        ui.push_status(format!("active task: {task_id}"));
                    }
                    Ok(None) => ui.push_status("no active task"),
                    Err(err) => ui.push_status(format!("active task read failed: {err}")),
                },
                Err(err) => ui.push_status(format!("task not found: {err}")),
            }
        }
        Some("list") | Some("ls") => match runtime.list() {
            Ok(tasks) => ui.push_status(format_task_list(&tasks, active_task.as_deref())),
            Err(err) => ui.push_status(format!("task list failed: {err}")),
        },
        Some("status") => {
            let task_id = args
                .get(1)
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                match runtime.get(&task_id) {
                    Ok(task) => ui.push_status(format_task_next(&task)),
                    Err(err) => ui.push_status(format!("task status failed: {err}")),
                }
            } else {
                ui.push_status("no active task");
            }
        }
        Some("next") => {
            let selector = args.get(1).copied().or(active_task.as_deref());
            match task_next_text(runtime, selector) {
                Ok(text) => ui.push_status(text),
                Err(err) => ui.push_status(format!("task next failed: {err}")),
            }
        }
        Some("log") => {
            let task_id = args
                .get(1)
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                ui.push_status(task_log_text(runtime, &task_id));
            } else {
                ui.push_status("no active task");
            }
        }
        Some("watch") | Some("attach") => {
            let task_id = args
                .get(1)
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                ui.push_status(format!(
                    "watch this task in another terminal:\npwcli task watch {task_id}"
                ));
            } else {
                ui.push_status("no active task");
            }
        }
        Some("cancel") => {
            let task_id = args
                .get(1)
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                match runtime.cancel(&task_id) {
                    Ok(()) => ui.push_status(format!("cancelled {task_id}")),
                    Err(err) => ui.push_status(format!("cancel failed: {err}")),
                }
            } else {
                ui.push_status("no active task");
            }
        }
        Some("compact") => {
            let task_id = args
                .get(1)
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                match runtime.compact(&task_id, CompactScope::Both) {
                    Ok(summary) => ui.push_status(format!(
                        "compacted task {} -> {}",
                        task_id,
                        summary.summary_path.display()
                    )),
                    Err(err) => ui.push_status(format!("compact failed: {err}")),
                }
            } else {
                ui.push_status("no active task");
            }
        }
        Some("verify") => {
            let owned_args = args[1..]
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>();
            let (selector, verify_args) = split_task_verify_args(&owned_args);
            let task_id = selector
                .as_deref()
                .map(|selector| runtime.resolve_task_id(Some(selector)).ok().flatten())
                .unwrap_or_else(|| {
                    runtime
                        .resolve_task_id(active_task.as_deref())
                        .ok()
                        .flatten()
                });
            if let Some(task_id) = task_id {
                let verify_args = verify_args
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>();
                match runtime
                    .get(&task_id)
                    .and_then(|task| execute_task_verification(settings, &task, &verify_args))
                    .and_then(|record| {
                        runtime
                            .record_verification(&task_id, record.clone())
                            .map(|path| (record, path))
                    }) {
                    Ok((record, path)) => {
                        let gate = record
                            .report
                            .as_ref()
                            .map(|report| report.gate.decision.as_str())
                            .unwrap_or(if record.passed { "pass" } else { "block" });
                        let status = record
                            .report
                            .as_ref()
                            .map(|report| report.status.as_str())
                            .unwrap_or(if record.passed { "passed" } else { "failed" });
                        ui.push_status(format!(
                            "verification {status} gate={gate}: {}\n{}\n\n{}",
                            path.display(),
                            preview(&record.content, 2400),
                            task_next_text(runtime, Some(&task_id)).unwrap_or_default()
                        ));
                    }
                    Err(err) => ui.push_status(format!("task verify failed: {err}")),
                }
            } else {
                ui.push_status("no active task");
            }
        }
        _ => {
            ui.push_status("usage: /task new|use|list|status|next|log|watch|cancel|compact|verify")
        }
    }
}

fn handle_interactive_workflow_command(
    rest: &str,
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    active_task: &mut Option<String>,
    ui: &mut TerminalUi,
) {
    let args = split_command_line(rest);
    match args.first().map(String::as_str).unwrap_or("help") {
        "run" => {
            let options = parse_workflow_run_options(&args[1..]);
            if options.goal.trim().is_empty() && options.recipe.is_none() {
                ui.push_status("usage: /workflow run [--agent codex] [--kind auto|code|research|ops|general] [--yes] [--dry-run] <goal>");
                return;
            }
            let workflow = match workflow_from_options(settings, &options) {
                Ok(workflow) => workflow,
                Err(err) => {
                    ui.push_status(format!("workflow planning failed: {err}"));
                    return;
                }
            };
            if options.dry_run {
                ui.push_status(workflow_mermaid(&workflow));
                return;
            }
            match runtime.create_task(
                RuntimeTaskKind::Workflow,
                format!("workflow {}", workflow_goal_for_display(&options)),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                serde_json::json!({
                    "goal": workflow_goal_for_display(&options),
                    "workflow": {
                        "name": workflow.name.clone(),
                        "status": "running",
                        "recipe": options.recipe
                    }
                }),
            ) {
                Ok(task) => {
                    let _ = runtime.set_active(&task.task_id);
                    *active_task = Some(task.task_id.clone());
                    let result =
                        persist_workflow_artifacts(runtime, &task.task_id, &workflow, None)
                            .and_then(|_| {
                                run_workflow_for_task(
                                    settings,
                                    runtime,
                                    &task.task_id,
                                    workflow,
                                    options.yes,
                                    None,
                                )
                            })
                            .and_then(|summary| {
                                let workflow = load_workflow(runtime, &task.task_id)?;
                                persist_workflow_artifacts(
                                    runtime,
                                    &task.task_id,
                                    &workflow,
                                    Some(&summary),
                                )?;
                                finalize_workflow_task(runtime, &task.task_id, &summary)?;
                                Ok(summary)
                            });
                    match result {
                        Ok(summary) => ui.push_status(format!(
                            "{}\n\n{}",
                            format_workflow_summary(&summary),
                            task_next_text(runtime, Some(&task.task_id)).unwrap_or_default()
                        )),
                        Err(err) => ui.push_status(format!("workflow run failed: {err}")),
                    }
                }
                Err(err) => ui.push_status(format!("workflow task create failed: {err}")),
            }
        }
        "plan" => {
            let options = parse_workflow_run_options(&args[1..]);
            match workflow_from_options(settings, &options) {
                Ok(workflow) => ui.push_status(format!(
                    "workflow plan kind={} agent={}\n\n{}",
                    workflow_kind(&workflow)
                        .unwrap_or_else(|| options
                            .kind
                            .resolve(workflow_goal_for_display(&options).as_str()))
                        .as_str(),
                    workflow_agent_label(&workflow).unwrap_or("mixed"),
                    workflow_mermaid(&workflow)
                )),
                Err(err) => ui.push_status(format!("workflow plan failed: {err}")),
            }
        }
        "save" => {
            let Some(name) = args.get(1) else {
                ui.push_status("usage: /workflow save <name> [--from id|last|active] [--force]\n   or: /workflow save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
                return;
            };
            let (source_task, force, workflow_args) = split_workflow_save_args(&args[2..]);
            let result = if let Some(selector) = source_task {
                save_workflow_recipe_from_task(settings, runtime, name, Some(&selector), force)
            } else {
                let options = parse_workflow_run_options(&workflow_args);
                if options.goal.trim().is_empty() && options.recipe.is_none() {
                    ui.push_status("usage: /workflow save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
                    return;
                }
                workflow_from_options(settings, &options).and_then(|workflow| {
                    let recipe = workflow_recipe_from_options(name, &options, workflow)?;
                    save_workflow_recipe(settings, &recipe).map(|path| (recipe, path))
                })
            };
            match result {
                Ok((recipe, path)) => ui.push_status(format!(
                    "workflow recipe saved: {}\n{}",
                    recipe.name,
                    path.display()
                )),
                Err(err) => ui.push_status(format!("workflow recipe save failed: {err}")),
            }
        }
        "recipes" | "list-recipes" => match list_workflow_recipes(settings) {
            Ok(recipes) if recipes.is_empty() => ui.push_status("no workflow recipes"),
            Ok(recipes) => ui.push_status(
                recipes
                    .iter()
                    .map(|recipe| {
                        format!(
                            "{}\t{}\tagent={}\tkind={}",
                            recipe.name,
                            recipe.description,
                            recipe.agent,
                            recipe.kind.as_str()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(err) => ui.push_status(format!("workflow recipe list failed: {err}")),
        },
        "recipe" => match args.get(1).map(String::as_str).unwrap_or("list") {
            "list" | "ls" => match list_workflow_recipes(settings) {
                Ok(recipes) if recipes.is_empty() => ui.push_status("no workflow recipes"),
                Ok(recipes) => ui.push_status(
                    recipes
                        .iter()
                        .map(|recipe| {
                            format!(
                                "{}\t{}\tagent={}\tkind={}",
                                recipe.name,
                                recipe.description,
                                recipe.agent,
                                recipe.kind.as_str()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(err) => ui.push_status(format!("workflow recipe list failed: {err}")),
            },
            "show" => {
                let Some(name) = args.get(2) else {
                    ui.push_status("usage: /workflow recipe show <name>");
                    return;
                };
                match load_workflow_recipe(settings, name) {
                    Ok(recipe) => ui.push_status(format!(
                        "{}\n\n{}",
                        format_workflow_recipe(&recipe),
                        workflow_mermaid(&recipe.workflow)
                    )),
                    Err(err) => ui.push_status(format!("workflow recipe show failed: {err}")),
                }
            }
            "run" => {
                let Some(name) = args.get(2) else {
                    ui.push_status("usage: /workflow recipe run <name> [--yes]");
                    return;
                };
                let yes = args.iter().any(|arg| arg == "--yes" || arg == "-y");
                match run_workflow_recipe_task(settings, runtime, name, yes) {
                    Ok(()) => ui.push_status(format!("workflow recipe started: {name}")),
                    Err(err) => ui.push_status(format!("workflow recipe run failed: {err}")),
                }
            }
            "save" => {
                let Some(name) = args.get(2) else {
                    ui.push_status("usage: /workflow recipe save <name> [--from id|last|active] [--force]\n   or: /workflow recipe save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
                    return;
                };
                let (source_task, force, workflow_args) = split_workflow_save_args(&args[3..]);
                let result = if let Some(selector) = source_task {
                    save_workflow_recipe_from_task(settings, runtime, name, Some(&selector), force)
                } else {
                    let options = parse_workflow_run_options(&workflow_args);
                    if options.goal.trim().is_empty() && options.recipe.is_none() {
                        ui.push_status("usage: /workflow recipe save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>");
                        return;
                    }
                    workflow_from_options(settings, &options).and_then(|workflow| {
                        let recipe = workflow_recipe_from_options(name, &options, workflow)?;
                        save_workflow_recipe(settings, &recipe).map(|path| (recipe, path))
                    })
                };
                match result {
                    Ok((recipe, path)) => ui.push_status(format!(
                        "workflow recipe saved: {}\n{}",
                        recipe.name,
                        path.display()
                    )),
                    Err(err) => ui.push_status(format!("workflow recipe save failed: {err}")),
                }
            }
            other => ui.push_status(format!("unknown workflow recipe command: {other}")),
        },
        "resume" => {
            let yes = args.iter().any(|arg| arg == "--yes" || arg == "-y");
            let selector = args
                .iter()
                .skip(1)
                .find(|arg| !arg.starts_with('-'))
                .map(String::as_str);
            match runtime.resolve_task_id(selector) {
                Ok(Some(task_id)) => match resume_workflow_task(settings, runtime, &task_id, yes) {
                    Ok(summary) => ui.push_status(format_workflow_summary(&summary)),
                    Err(err) => ui.push_status(format!("workflow resume failed: {err}")),
                },
                Ok(None) => ui.push_status("no workflow task found"),
                Err(err) => ui.push_status(format!("workflow resume failed: {err}")),
            }
        }
        "status" | "show" => {
            let selector = args.get(1).map(String::as_str);
            match runtime.resolve_task_id(selector) {
                Ok(Some(task_id)) => {
                    let mut text = runtime
                        .get(&task_id)
                        .and_then(|task| Ok(serde_json::to_string_pretty(&task)?))
                        .unwrap_or_else(|err| format!("workflow status failed: {err}"));
                    if let Ok(summary) = load_workflow_summary(runtime, &task_id) {
                        text.push_str("\n\n");
                        text.push_str(&format_workflow_summary(&summary));
                    }
                    ui.push_status(text);
                }
                Ok(None) => ui.push_status("no workflow task found"),
                Err(err) => ui.push_status(format!("workflow status failed: {err}")),
            }
        }
        "graph" | "mermaid" => {
            let options = parse_workflow_run_options(&args[1..]);
            match workflow_from_options(settings, &options) {
                Ok(workflow) => ui.push_status(workflow_mermaid(&workflow)),
                Err(err) => ui.push_status(format!("workflow graph failed: {err}")),
            }
        }
        _ => ui.push_status(workflow_help_text().replace("pwcli ", "/")),
    }
}

fn handle_interactive_goal_command(
    goal: &str,
    runtime: &RuntimeTaskManager,
    active_task: &mut Option<String>,
    ui: &mut TerminalUi,
) {
    if goal.trim().is_empty() {
        ui.push_status("usage: /goal <objective>");
        return;
    }
    match runtime.create_task(
        RuntimeTaskKind::Internal,
        goal.to_string(),
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        serde_json::json!({ "goal": goal, "created_by": "/goal" }),
    ) {
        Ok(task) => match runtime.set_active(&task.task_id) {
            Ok(()) => {
                *active_task = Some(task.task_id.clone());
                ui.push_status(format!("active task: {}\n{}", task.task_id, task.title));
            }
            Err(err) => ui.push_status(format!("goal created but active update failed: {err}")),
        },
        Err(err) => ui.push_status(format!("goal create failed: {err}")),
    }
}

fn handle_interactive_agent_mode(
    mode: &str,
    default_yolo: bool,
    rest: &str,
    settings: &Settings,
    runtime: &RuntimeTaskManager,
    active_task: &mut Option<String>,
    ui: &mut TerminalUi,
) {
    let raw_args = split_command_line(rest);
    let args = match agent_mode_args_for_runtime(runtime, mode, default_yolo, &raw_args) {
        Ok(args) => args,
        Err(err) => {
            ui.push_status(format!("agent task failed: {err}"));
            return;
        }
    };
    match spawn_agent_task_with_runtime(&args, settings, runtime, false, false) {
        Ok(task_id) => {
            *active_task = Some(task_id.clone());
            ui.push_status(format!("agent task started: {task_id}"));
        }
        Err(err) => ui.push_status(format!("agent task failed: {err}")),
    }
}

fn handle_interactive_session_command(
    rest: &str,
    settings: &Settings,
    active_session: &mut Option<String>,
    ui: &mut TerminalUi,
) {
    let store = SessionStore::new(settings.pwcli_home.clone());
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.first().copied().unwrap_or("list") {
        "list" | "ls" => match store.list() {
            Ok(sessions) => ui.push_status(format_session_list(&sessions)),
            Err(err) => ui.push_status(format!("session list failed: {err}")),
        },
        "show" => {
            let selector = args.get(1).copied().unwrap_or("last");
            match store.get(selector) {
                Ok(Some(record)) => ui.push_status(format_session_record(&record)),
                Ok(None) => ui.push_status(format!("session not found: {selector}")),
                Err(err) => ui.push_status(format!("session show failed: {err}")),
            }
        }
        "resume" | "use" => {
            let selector = args.get(1).copied().unwrap_or("last");
            match store.get(selector) {
                Ok(Some(record)) => {
                    *active_session = Some(record.entry.id.clone());
                    ui.push_status(format!("active session: {}", record.entry.id));
                }
                Ok(None) => ui.push_status(format!("session not found: {selector}")),
                Err(err) => ui.push_status(format!("session resume failed: {err}")),
            }
        }
        "delete" | "rm" => {
            let Some(selector) = args.get(1).copied() else {
                ui.push_status("usage: /session delete <id|prefix|last>");
                return;
            };
            match store.delete(selector) {
                Ok(Some(entry)) => ui.push_status(format!("deleted session {}", entry.id)),
                Ok(None) => ui.push_status(format!("session not found: {selector}")),
                Err(err) => ui.push_status(format!("session delete failed: {err}")),
            }
        }
        _ => {
            ui.push_status("usage: /session list|show <id|last>|resume <id|last>|delete <id|last>")
        }
    }
}

fn handle_interactive_verify_command(rest: &str, settings: &Settings, ui: &mut TerminalUi) {
    let args = split_command_line(rest);
    match execute_verify_args(&args, settings) {
        Ok(result) => {
            if result.is_error {
                ui.push_status(format!("verification failed\n\n{}", result.content));
            } else {
                ui.push_status(result.content);
            }
        }
        Err(err) => ui.push_status(format!("verification failed: {err}")),
    }
}

fn handle_interactive_context_pack(prompt: &str, settings: &Settings, ui: &mut TerminalUi) {
    if prompt.trim().is_empty() {
        ui.push_status("usage: /context pack <prompt>");
        return;
    }
    match context_preview_text(prompt, settings) {
        Ok(text) => ui.push_status(text),
        Err(err) => ui.push_status(format!("context preview failed: {err}")),
    }
}

fn handle_interactive_audit_command(rest: &str, settings: &Settings, ui: &mut TerminalUi) {
    let args = split_command_line(rest);
    match audit_report_text(&args, settings) {
        Ok(text) => ui.push_status(text),
        Err(err) => ui.push_status(format!("audit failed: {err}")),
    }
}

fn handle_interactive_tools_command(rest: &str, settings: &Settings, ui: &mut TerminalUi) {
    let args = rest.split_whitespace().collect::<Vec<_>>();
    let (registry, _) = match build_registry(settings) {
        Ok(value) => value,
        Err(err) => {
            ui.push_status(format!("tools discovery failed: {err}"));
            return;
        }
    };
    let snapshot = registry.snapshot();
    match args.first().copied() {
        None | Some("list") | Some("ls") => ui.push_status(format_tools_list(&snapshot)),
        Some("show") | Some("describe") => {
            let Some(tool_id) = args.get(1) else {
                ui.push_status("usage: /tools show <tool_id>");
                return;
            };
            match snapshot.get(tool_id) {
                Some(tool) => ui.push_status(format_tool_descriptor(&tool.descriptor)),
                None => ui.push_status(format!("tool not found: {tool_id}")),
            }
        }
        Some("help") | Some("--help") | Some("-h") => ui.push_status(interactive_tools_help_text()),
        Some("call") => ui.push_status(
            "direct tool execution is CLI-only for now; use `pwcli tools call <tool_id> '<json-args>'`",
        ),
        Some(other) => ui.push_status(format!(
            "unknown tools command: {other}\n\n{}",
            interactive_tools_help_text()
        )),
    }
}

fn split_command_line(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn handle_interactive_memory_command(rest: &str, settings: &Settings, ui: &mut TerminalUi) {
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    if let Err(err) = store.ensure() {
        ui.push_status(format!("memory init failed: {err}"));
        return;
    }
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.first().copied() {
        None | Some("inbox") => match store.list_candidates() {
            Ok(candidates) if candidates.is_empty() => ui.push_status("memory inbox is empty"),
            Ok(candidates) => ui.push_status(format_memory_inbox(&candidates)),
            Err(err) => ui.push_status(format!("memory inbox failed: {err}")),
        },
        Some("show") => {
            let Some(candidate_id) = args.get(1) else {
                ui.push_status("usage: /memory show <candidate_id>");
                return;
            };
            match store.get_candidate(candidate_id) {
                Ok(Some(candidate)) => ui.push_status(format_memory_candidate(&candidate)),
                Ok(None) => ui.push_status(format!("unknown memory candidate '{candidate_id}'")),
                Err(err) => ui.push_status(format!("memory show failed: {err}")),
            }
        }
        Some("facts") | Some("list") => match store.list_facts() {
            Ok(facts) if facts.is_empty() => ui.push_status("no facts"),
            Ok(facts) => ui.push_status(
                facts
                    .into_iter()
                    .take(20)
                    .map(|fact| format!("{}\t{:?}\t{}", fact.id, fact.status, fact.statement))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(err) => ui.push_status(format!("memory facts failed: {err}")),
        },
        Some("search") => {
            let query = args[1..].join(" ");
            if query.trim().is_empty() {
                ui.push_status("usage: /memory search <query>");
                return;
            }
            match store.search(&query, 10) {
                Ok(facts) if facts.is_empty() => ui.push_status("no matching facts"),
                Ok(facts) => ui.push_status(
                    facts
                        .into_iter()
                        .map(|scored| {
                            format!(
                                "{}\t{:.2}\t{}",
                                scored.fact.id, scored.score, scored.fact.statement
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(err) => ui.push_status(format!("memory search failed: {err}")),
            }
        }
        Some("add") => {
            if args.get(1).copied() != Some("fact") {
                ui.push_status("usage: /memory add fact <statement>");
                return;
            }
            let statement = args[2..].join(" ");
            let source = format!(
                "{} 用户通过 /memory add 手动写入",
                chrono::Local::now().format("%Y-%m-%d")
            );
            match store.add_fact(statement, source) {
                Ok(fact) => ui.push_status(format!("fact added: {}", fact.id)),
                Err(err) => ui.push_status(format!("memory add failed: {err}")),
            }
        }
        Some("accept") => {
            let Some(candidate_id) = args.get(1) else {
                ui.push_status("usage: /memory accept <candidate_id>");
                return;
            };
            match store.accept_candidate(candidate_id) {
                Ok(facts) => ui.push_status(format!(
                    "accepted {} facts:\n{}",
                    facts.len(),
                    facts
                        .into_iter()
                        .map(|fact| format!("{}\t{}", fact.id, fact.statement))
                        .collect::<Vec<_>>()
                        .join("\n")
                )),
                Err(err) => ui.push_status(format!("memory accept failed: {err}")),
            }
        }
        Some("reject") => {
            let Some(candidate_id) = args.get(1) else {
                ui.push_status("usage: /memory reject <candidate_id>");
                return;
            };
            match store.reject_candidate(candidate_id) {
                Ok(()) => ui.push_status(format!("rejected {candidate_id}")),
                Err(err) => ui.push_status(format!("memory reject failed: {err}")),
            }
        }
        Some("extract") => handle_interactive_memory_extract(&args[1..], settings, ui),
        Some("graph") | Some("stats") => match store.graph_stats() {
            Ok(stats) => ui.push_status(
                serde_json::to_string_pretty(&stats)
                    .unwrap_or_else(|_| "memory graph stats unavailable".to_string()),
            ),
            Err(err) => ui.push_status(format!("memory graph failed: {err}")),
        },
        Some("events") | Some("timeline") => match store.lifecycle_events() {
            Ok(events) => ui.push_status(format_memory_events(&events)),
            Err(err) => ui.push_status(format!("memory events failed: {err}")),
        },
        Some("derive") => {
            let query = args[1..].join(" ");
            match store
                .derive_candidate_from_graph((!query.trim().is_empty()).then_some(query.trim()))
            {
                Ok(Some(candidate)) => {
                    let id = candidate.id.clone();
                    match store.add_candidate(&candidate) {
                        Ok(()) => ui.push_status(format!(
                            "memory derived candidate: {id}\nuse /memory show {id}, then /memory accept {id}"
                        )),
                        Err(err) => ui.push_status(format!("memory derive write failed: {err}")),
                    }
                }
                Ok(None) => ui.push_status("no memory derivation candidate created"),
                Err(err) => ui.push_status(format!("memory derive failed: {err}")),
            }
        }
        Some("rebuild") => match store.rebuild_graph_index() {
            Ok(stats) => ui.push_status(format!(
                "memory graph rebuilt:\n{}",
                serde_json::to_string_pretty(&stats).unwrap_or_default()
            )),
            Err(err) => ui.push_status(format!("memory rebuild failed: {err}")),
        },
        Some("embedder") if args.get(1).copied() == Some("ensure") => {
            match store.ensure_embedding_model() {
                Ok(path) => ui.push_status(format!("embedding model cache: {}", path.display())),
                Err(err) => ui.push_status(format!("memory embedder ensure failed: {err}")),
            }
        }
        _ => ui.push_status(
            "usage: /memory inbox|show <id>|facts|search <query>|add fact <text>|extract task|file|text|derive [query]|accept <id>|reject <id>|graph|events|rebuild|embedder ensure",
        ),
    }
}

fn handle_interactive_memory_extract(args: &[&str], settings: &Settings, ui: &mut TerminalUi) {
    let result = match args.first().copied() {
        Some("task") | None => {
            let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
            let task_id = match runtime.resolve_task_id(args.get(1).copied()) {
                Ok(Some(task_id)) => task_id,
                Ok(None) => {
                    ui.push_status("no active task; use /goal <objective> first");
                    return;
                }
                Err(err) => {
                    ui.push_status(format!("memory extract task failed: {err}"));
                    return;
                }
            };
            memory_task_source_text(&runtime, &task_id).and_then(|text| {
                create_memory_candidate_from_text(settings, &text, "runtime task summary")
            })
        }
        Some("file") => {
            let Some(path) = args.get(1) else {
                ui.push_status("usage: /memory extract file <path>");
                return;
            };
            std::fs::read_to_string(path)
                .map_err(pwcli::PwError::from)
                .and_then(|text| {
                    create_memory_candidate_from_text(settings, &text, &format!("file {}", path))
                })
        }
        Some("text") => {
            let text = args[1..].join(" ");
            if text.trim().is_empty() {
                ui.push_status("usage: /memory extract text <text>");
                return;
            }
            create_memory_candidate_from_text(settings, &text, "manual text")
        }
        Some(other) => {
            ui.push_status(format!(
                "unknown memory extract target: {other}\nusage: /memory extract task [id]|file <path>|text <text>"
            ));
            return;
        }
    };

    match result {
        Ok(Some(candidate_id)) => ui.push_status(format!(
            "memory candidate created: {candidate_id}\nuse /memory inbox then /memory accept {candidate_id}"
        )),
        Ok(None) => ui.push_status("no memory candidate created"),
        Err(err) => ui.push_status(format!("memory extract failed: {err}")),
    }
}

fn handle_interactive_rules_command(rest: &str, settings: &Settings, ui: &mut TerminalUi) {
    if let Err(err) = WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure() {
        ui.push_status(format!("rules init failed: {err}"));
        return;
    }
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.first().copied() {
        Some("list") | Some("ls") | None => match list_rule_files(settings) {
            Ok(rules) if rules.is_empty() => ui.push_status("no rules"),
            Ok(rules) => ui.push_status(
                rules
                    .into_iter()
                    .map(|(name, path)| format!("{name}\t{}", path.display()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(err) => ui.push_status(format!("rules list failed: {err}")),
        },
        Some("show") => {
            let Some(name) = args.get(1) else {
                ui.push_status("usage: /rules show <name>");
                return;
            };
            match rule_path(settings, name) {
                Ok(path) if path.is_file() => match std::fs::read_to_string(path) {
                    Ok(text) => ui.push_status(text),
                    Err(err) => ui.push_status(format!("rules show failed: {err}")),
                },
                Ok(_) => ui.push_status(format!("rule not found: {name}")),
                Err(err) => ui.push_status(format!("invalid rule name: {err}")),
            }
        }
        Some("add") | Some("set") => {
            let Some(name) = args.get(1) else {
                ui.push_status("usage: /rules add <name> <text>");
                return;
            };
            let text = args[2..].join(" ");
            if text.trim().is_empty() {
                ui.push_status("usage: /rules add <name> <text>");
                return;
            }
            match rule_path(settings, name).and_then(|path| {
                std::fs::write(&path, format!("{}\n", text.trim()))?;
                Ok(path)
            }) {
                Ok(path) => ui.push_status(format!("rule saved: {}", path.display())),
                Err(err) => ui.push_status(format!("rules add failed: {err}")),
            }
        }
        Some("rm") | Some("remove") | Some("delete") => {
            let Some(name) = args.get(1) else {
                ui.push_status("usage: /rules rm <name>");
                return;
            };
            match rule_path(settings, name) {
                Ok(path) if path.is_file() => match std::fs::remove_file(&path) {
                    Ok(()) => ui.push_status(format!("rule removed: {name}")),
                    Err(err) => ui.push_status(format!("rules rm failed: {err}")),
                },
                Ok(_) => ui.push_status(format!("rule not found: {name}")),
                Err(err) => ui.push_status(format!("invalid rule name: {err}")),
            }
        }
        _ => ui.push_status("usage: /rules list|show <name>|add <name> <text>|rm <name>"),
    }
}

fn format_memory_inbox(candidates: &[pwcli::memory::MemoryCandidate]) -> String {
    candidates
        .iter()
        .map(format_memory_candidate)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_memory_events(events: &[pwcli::memory::MemoryLifecycleEvent]) -> String {
    if events.is_empty() {
        return "no memory events".to_string();
    }
    events
        .iter()
        .rev()
        .take(30)
        .map(|event| {
            let related = if event.related_ids.is_empty() {
                String::new()
            } else {
                format!(" related={}", event.related_ids.join(","))
            };
            format!(
                "{}\t{:?}\t{}{}\n  {}",
                event.created_at, event.kind, event.subject_id, related, event.note
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_memory_candidate(candidate: &pwcli::memory::MemoryCandidate) -> String {
    let mut out = format!(
        "{}\t{}\nsource: {}\nreason: {}\nreview: {:?} score={:.2} related={} strongest={:.2}\nreview_reason: {}",
        candidate.id,
        candidate.created_at,
        candidate.source,
        candidate.reason,
        candidate.review.action,
        candidate.review.score,
        candidate.review.related_fact_count,
        candidate.review.strongest_related_score,
        candidate.review.rationale
    );
    if !candidate.review.signals.is_empty() {
        out.push_str(&format!(
            "\nreview_signals: {}",
            candidate
                .review
                .signals
                .iter()
                .map(|signal| format!("{signal:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if !candidate.facts.is_empty() {
        out.push_str("\n\nfacts:");
        for (idx, fact) in candidate.facts.iter().enumerate() {
            let ref_id = fact
                .ref_id
                .as_deref()
                .map(|id| format!(" [{id}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n  {}.{} {}\n     source: {}",
                idx + 1,
                ref_id,
                fact.statement,
                fact.source
            ));
            if !fact.related_facts.is_empty() {
                out.push_str("\n     related:");
                for related in &fact.related_facts {
                    out.push_str(&format!(
                        "\n       {} {:.2} {} ({})",
                        related.fact_id, related.score, related.statement, related.reason
                    ));
                }
            }
        }
    }

    if !candidate.logic_chains.is_empty() {
        out.push_str("\n\nlogic_chains:");
        for (idx, chain) in candidate.logic_chains.iter().enumerate() {
            let ref_id = chain
                .ref_id
                .as_deref()
                .map(|id| format!(" [{id}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n  {}.{} premises={}\n     {}",
                idx + 1,
                ref_id,
                chain.premises.join(", "),
                chain.explanation
            ));
        }
    }

    if !candidate.inferences.is_empty() {
        out.push_str("\n\ninferences:");
        for (idx, inference) in candidate.inferences.iter().enumerate() {
            out.push_str(&format!(
                "\n  {}. {} (logic_chain={})",
                idx + 1,
                inference.statement,
                inference.logic_chain
            ));
        }
    }

    if !candidate.hypotheses.is_empty() {
        out.push_str("\n\nhypotheses:");
        for (idx, hypothesis) in candidate.hypotheses.iter().enumerate() {
            out.push_str(&format!(
                "\n  {}. {:.2} {} (supporting_facts={})",
                idx + 1,
                hypothesis.confidence,
                hypothesis.statement,
                hypothesis.supporting_facts.join(", ")
            ));
        }
    }

    out
}

fn format_task_list(tasks: &[RuntimeTask], active_task: Option<&str>) -> String {
    if tasks.is_empty() {
        return "no tasks".to_string();
    }
    tasks
        .iter()
        .map(|task| {
            let marker = if active_task == Some(task.task_id.as_str()) {
                "*"
            } else {
                " "
            };
            format!(
                "{} {}\t{:?}\t{:?}\t{}{}{}{}",
                marker,
                task.task_id,
                task.status,
                task.kind,
                task.title,
                task_agent_marker(task),
                task_verification_marker(task),
                task_review_marker(task)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_agent_marker(task: &RuntimeTask) -> String {
    let Some(agent) = task
        .metadata
        .get("agent_cli")
        .and_then(serde_json::Value::as_str)
    else {
        return String::new();
    };
    let mode = task
        .metadata
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("direct");
    format!("\tagent={agent}:{mode}")
}

fn task_verification_marker(task: &RuntimeTask) -> &'static str {
    match verification_gate(task).as_deref() {
        Some("pass") => "\tverify=pass",
        Some("block") => "\tverify=block",
        Some("needs_review") => "\tverify=needs_review",
        _ => match verification_passed(task) {
            Some(true) => "\tverify=passed",
            Some(false) => "\tverify=failed",
            None => "",
        },
    }
}

fn task_review_marker(task: &RuntimeTask) -> &'static str {
    match review_required(task) {
        Some(true) => "\treview=required",
        Some(false) => "\treview=ok",
        None => "",
    }
}

fn run_prompt_cli(args: &[String]) -> Result<()> {
    let mut resume_session = None;
    let mut prompt_parts = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--session" | "--resume" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(pwcli::PwError::Message(
                        "usage: pwcli run [--session <id|last>] <prompt>".to_string(),
                    ));
                };
                resume_session = Some(value.clone());
                idx += 2;
            }
            value if value.starts_with("--session=") => {
                resume_session = Some(value.trim_start_matches("--session=").to_string());
                idx += 1;
            }
            value if value.starts_with("--resume=") => {
                resume_session = Some(value.trim_start_matches("--resume=").to_string());
                idx += 1;
            }
            value => {
                prompt_parts.push(value.to_string());
                idx += 1;
            }
        }
    }
    run_prompt(prompt_parts.join(" "), resume_session)
}

fn run_prompt(prompt: String, resume_session: Option<String>) -> Result<()> {
    let settings = Settings::load()?;
    run_prompt_with_settings(prompt, &settings, resume_session).map(|_| ())
}

fn run_prompt_with_settings(
    prompt: String,
    settings: &Settings,
    resume_session: Option<String>,
) -> Result<String> {
    let mut output = |delta: &str| {
        print!("{delta}");
        let _ = std::io::stdout().flush();
    };
    let session_id =
        run_prompt_with_settings_output(prompt, settings, resume_session, &mut output)?;
    println!();
    Ok(session_id)
}

type SharedOutput<'a> = Rc<RefCell<&'a mut dyn FnMut(&str)>>;

struct OutputGraphEventSink<'a> {
    output: SharedOutput<'a>,
}

impl GraphEventSink for OutputGraphEventSink<'_> {
    fn emit(&mut self, event: GraphEvent) {
        if let Some(line) = graph_event_line(&event) {
            (self.output.borrow_mut())(&line);
        }
    }
}

fn graph_event_line(event: &GraphEvent) -> Option<String> {
    match event {
        GraphEvent::GraphStarted => Some("\n[graph] started\n".to_string()),
        GraphEvent::ContextBuilt { context_id } => Some(format!("[context] built {context_id}\n")),
        GraphEvent::ToolSelectionStarted => Some("[tools] selecting\n".to_string()),
        GraphEvent::ToolSelected { tool_id } => Some(format!("[tools] selected {tool_id}\n")),
        GraphEvent::ModelStarted => Some("[model] streaming\n".to_string()),
        GraphEvent::ToolCallStarted { name, call_id, .. } => {
            Some(format!("\n[tool] {name} started ({call_id})\n"))
        }
        GraphEvent::ToolPolicyDecision { name, decision, .. } => {
            Some(format!("[policy] {name}: {decision:?}\n"))
        }
        GraphEvent::UserApprovalRequested { prompt } => {
            Some(format!("[approval] {}\n", preview(prompt, 160)))
        }
        GraphEvent::GraphInterrupted { interrupt } => Some(format!(
            "[interrupt] {:?}: {}\n",
            interrupt.kind,
            preview(&interrupt.prompt, 160)
        )),
        GraphEvent::ToolCompleted {
            call_id,
            name,
            is_error,
            ..
        } => Some(format!(
            "[tool] {name} completed ({call_id}) error={is_error}\n"
        )),
        GraphEvent::ToolRuntimeEvent { call_id, event } => {
            format_tool_runtime_event(call_id, event)
        }
        GraphEvent::ModelCompleted { .. } | GraphEvent::GraphCompleted => None,
    }
}

fn format_tool_runtime_event(call_id: &str, event: &ToolRuntimeEvent) -> Option<String> {
    match event {
        ToolRuntimeEvent::Started { mode } => Some(format!(
            "[tool-runtime] {call_id} mode={}\n",
            tool_execution_mode_label(mode)
        )),
        ToolRuntimeEvent::Progress { message } => Some(format!(
            "[tool-runtime] {call_id} {}\n",
            preview(message, 160)
        )),
        ToolRuntimeEvent::Output { stream, chunk } => Some(format!(
            "[tool-runtime] {call_id} {stream}: {}\n",
            preview(chunk, 160)
        )),
        ToolRuntimeEvent::BackgroundTaskStarted { task_id, .. } => Some(format!(
            "[tool-runtime] {call_id} background task={task_id}\n"
        )),
        ToolRuntimeEvent::TimedOut { timeout_seconds } => Some(format!(
            "[tool-runtime] {call_id} timed out after {timeout_seconds}s\n"
        )),
        ToolRuntimeEvent::Cancelled => Some(format!("[tool-runtime] {call_id} cancelled\n")),
        ToolRuntimeEvent::CancelRequested => {
            Some(format!("[tool-runtime] {call_id} cancel requested\n"))
        }
        ToolRuntimeEvent::Completed { .. }
        | ToolRuntimeEvent::Artifact { .. }
        | ToolRuntimeEvent::Poll { .. } => None,
    }
}

fn tool_execution_mode_label(mode: &ToolExecutionMode) -> &'static str {
    match mode {
        ToolExecutionMode::Sync => "sync",
        ToolExecutionMode::Streaming => "streaming",
        ToolExecutionMode::Background => "background",
        ToolExecutionMode::Resumable => "resumable",
    }
}

fn run_prompt_with_settings_output(
    prompt: String,
    settings: &Settings,
    resume_session: Option<String>,
    output: &mut dyn FnMut(&str),
) -> Result<String> {
    let approval = StdinPrompter;
    run_prompt_with_settings_output_with_approval(
        prompt,
        settings,
        resume_session,
        output,
        &approval,
    )
}

fn run_prompt_with_settings_output_with_approval(
    prompt: String,
    settings: &Settings,
    resume_session: Option<String>,
    output: &mut dyn FnMut(&str),
    approval: &dyn UserApproval,
) -> Result<String> {
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));
    let model_settings = settings.resolved_model_settings()?;
    audit.record(AuditEvent::RuntimeInitialized);
    audit.record(AuditEvent::ConfigLoaded {
        provider: model_settings.provider_name.clone(),
        model: model_settings.model.clone(),
    });
    audit.record(AuditEvent::ToolDiscoveryStarted);
    let (registry, loaded_skills) = build_registry(settings)?;
    audit.record(AuditEvent::SkillsScanned {
        roots: settings
            .skill_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        loaded: loaded_skills,
    });
    audit.record(AuditEvent::ToolRegistryBuilt {
        registry_version: registry.version(),
        tool_count: registry.snapshot().descriptors().len(),
    });
    let snapshot = registry.snapshot();
    let local_paths = default_local_context_paths();
    let context_pack = ContextBuilder::new().build_with_sources_and_memory(
        prompt.clone(),
        &snapshot,
        Some(settings.pwcli_home.clone()),
        local_paths,
        &settings.memory,
    );
    audit.record(AuditEvent::ContextPackBuilt {
        context_id: context_pack.id.clone(),
        selected_tool_ids: context_pack.selected_tool_ids.clone(),
    });
    audit.record(AuditEvent::RegistrySnapshotCreated {
        registry_version: snapshot.version(),
        tool_count: snapshot.descriptors().len(),
    });
    let graph = GraphExecutor::builder()
        .max_rounds(settings.max_rounds)
        .build();
    let policy = DefaultPolicyGuard::default().with_rules(load_rule_texts(settings));
    let session_store = SessionStore::new(settings.pwcli_home.clone());
    let (seed_messages, resumed_session_id) = if let Some(selector) = resume_session.as_deref() {
        let record = session_store.get(selector)?.ok_or_else(|| {
            pwcli::PwError::Message(format!("session not found for resume: {selector}"))
        })?;
        output(&format!("[session] resumed {}\n", record.entry.id));
        (record.seed_messages(), Some(record.entry.id))
    } else {
        (Vec::new(), None)
    };

    let model_client = AnyModelClient::from_settings(&model_settings)?;
    let show_thinking = model_settings.show_thinking;
    let output_cell = Rc::new(RefCell::new(output));
    audit.record(AuditEvent::ModelNodeStarted {
        provider: model_settings.provider_name.clone(),
        model: model_settings.model.clone(),
    });
    let current_user_input = prompt.clone();
    let model_output = Rc::clone(&output_cell);
    let mut planner = StreamingModelPlanner::new(
        &model_client,
        model_settings.model.clone(),
        ThinkingConfig {
            enabled: model_settings.thinking_enabled,
            budget_tokens: Some(1024),
        },
        move |event| match event {
            ModelEvent::TextDelta(delta) => {
                (model_output.borrow_mut())(&delta);
            }
            ModelEvent::ThinkingDelta(delta) => {
                if show_thinking {
                    (model_output.borrow_mut())(&delta);
                }
            }
            ModelEvent::ToolCall(_) | ModelEvent::Usage(_) | ModelEvent::Done => {}
        },
    )
    .max_tokens(model_settings.max_output_tokens)
    .stream(model_settings.stream)
    .system(
        "You are pwcli, a local personal workbench agent runtime. The model provider call is \
         the internal Model Node of the graph and is not counted as a selected external tool. \
         Selected tools only means builtin, skill, MCP, or verification tools available to the \
         graph after context selection.",
    );
    let mut graph_events = OutputGraphEventSink {
        output: Rc::clone(&output_cell),
    };
    let tool_context = ToolExecutionContext {
        runtime_tasks: Some(RuntimeTaskManager::new(settings.pwcli_home.clone())),
        ..ToolExecutionContext::default()
    };
    let mut services = GraphRunServices::new(&policy, &audit, Some(approval), &mut graph_events)
        .with_tool_context(tool_context);

    let summary = match graph.run_with_seed_messages_and_events(
        GraphRunRequest {
            user_input: prompt,
            context_pack,
        },
        &snapshot,
        &mut planner,
        &mut services,
        seed_messages,
    ) {
        Ok(summary) => summary,
        Err(err) => {
            let error = err.to_string();
            audit.record(AuditEvent::ModelNodeFailed {
                error: error.clone(),
            });
            audit.record(AuditEvent::GraphRunFailed { error });
            return Err(err);
        }
    };
    audit.record(AuditEvent::ModelNodeCompleted {
        output_chars: summary.state.last_content.chars().count(),
    });
    audit.record(AuditEvent::FinalOutputProduced);

    let session_id = session_id_for_save(resumed_session_id);
    let session_path = session_store.save(&session_id, &summary)?;
    audit.record(AuditEvent::SessionSaved {
        path: session_path.display().to_string(),
    });
    {
        let mut output_ref = output_cell.borrow_mut();
        maybe_create_memory_candidate(
            settings,
            &current_user_input,
            &summary.state.last_content,
            &mut **output_ref,
        )?;
    }

    Ok(session_id)
}

fn session_id_for_save(resumed_session_id: Option<String>) -> String {
    resumed_session_id.unwrap_or_else(timestamp_session_id)
}

fn timestamp_session_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
        .to_string()
}

fn build_registry(settings: &Settings) -> Result<(ToolRegistry, usize)> {
    let mut registry = ToolRegistry::new();
    let mut tools = Vec::new();
    tools.extend(BuiltinToolLoader.load()?);
    tools.extend(VerificationToolLoader.load()?);
    tools.extend(McpToolLoader::new(settings.mcp.clone(), settings.pwcli_home.clone()).load()?);
    tools.extend(SkillToolLoader::new(settings.skill_roots.clone()).load()?);
    let tools = apply_tool_settings(tools, &settings.tools);
    let loaded_skills = tools
        .iter()
        .filter(|tool| matches!(tool.descriptor.source, ToolSource::Skill { .. }))
        .count();
    registry.register_many(tools);
    Ok((registry, loaded_skills))
}

fn maybe_create_memory_candidate(
    settings: &Settings,
    user_input: &str,
    last_content: &str,
    output: &mut dyn FnMut(&str),
) -> Result<()> {
    if !settings.memory.enabled || !settings.memory.auto_consider_write {
        return Ok(());
    }
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    store.ensure()?;
    let text = memory_candidate_turn_text(user_input, last_content);
    let source = format!(
        "{} 的 pwcli 对话结束后自动生成的候选记忆，写入前需要用户确认",
        chrono::Local::now().format("%Y-%m-%d")
    );
    if let Some(candidate) = create_memory_candidate(&store, settings, &text, source)? {
        let candidate_id = candidate.id.clone();
        let fact_count = candidate.facts.len();
        store.add_candidate(&candidate)?;
        output(&format!(
            "\n\n[memory] 生成 {fact_count} 条候选事实，尚未写入。使用 /memory inbox 查看，/memory accept {candidate_id} 确认写入。\n"
        ));
    }
    Ok(())
}

fn memory_candidate_turn_text(user_input: &str, last_content: &str) -> String {
    format!("用户输入：\n{user_input}\n\n助手输出：\n{last_content}")
}

fn create_memory_candidate_from_text(
    settings: &Settings,
    text: &str,
    source_label: &str,
) -> Result<Option<String>> {
    if !settings.memory.enabled || !settings.memory.auto_consider_write {
        return Ok(None);
    }
    let store = MemoryStore::new(&settings.pwcli_home, settings.memory.embedding.clone());
    store.ensure()?;
    let source = format!(
        "{} 的 {source_label} 自动生成的候选记忆，写入前需要用户确认",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let Some(candidate) = create_memory_candidate(&store, settings, text, source)? else {
        return Ok(None);
    };
    let id = candidate.id.clone();
    store.add_candidate(&candidate)?;
    Ok(Some(id))
}

fn create_memory_candidate(
    store: &MemoryStore,
    settings: &Settings,
    text: &str,
    source: String,
) -> Result<Option<pwcli::memory::MemoryCandidate>> {
    if settings.memory.semantic_extraction.enabled {
        if let Ok(Some(extraction)) = extract_memory_with_model(settings, text) {
            if let Some(candidate) =
                store.generate_candidate_from_semantic_extraction(extraction, source.clone())?
            {
                return Ok(Some(candidate));
            }
        }
    }
    store.generate_candidate_from_text(text, source)
}

fn extract_memory_with_model(
    settings: &Settings,
    text: &str,
) -> Result<Option<SemanticMemoryExtraction>> {
    let mut model_settings = match settings.resolved_model_settings() {
        Ok(settings) => settings,
        Err(_) => return Ok(None),
    };
    if model_settings.is_image_generation {
        return Ok(None);
    }
    model_settings.request_timeout_seconds = model_settings.request_timeout_seconds.min(45);
    let client = match AnyModelClient::from_settings(&model_settings) {
        Ok(client) => client,
        Err(_) => return Ok(None),
    };
    let extraction_settings = &settings.memory.semantic_extraction;
    let input = trim_chars(text, extraction_settings.max_input_chars);
    let schema_hint = format!(
        "Return JSON only with shape: {{\"facts\":[{{\"ref_id\":\"f1\",\"statement\":\"...\",\"source_note\":\"...\"}}],\"logic_chains\":[{{\"ref_id\":\"l1\",\"premises\":[\"f1\"],\"explanation\":\"...\"}}],\"inferences\":[{{\"statement\":\"...\",\"logic_chain\":\"l1\"}}],\"hypotheses\":[{{\"statement\":\"...\",\"supporting_facts\":[\"f1\"],\"confidence\":0.6}}],\"reason\":\"...\"}}. Limits: facts <= {}, logic_chains <= {}, inferences <= {}, hypotheses <= {}.",
        extraction_settings.max_facts,
        extraction_settings.max_logic_chains,
        extraction_settings.max_inferences,
        extraction_settings.max_hypotheses
    );
    let response = client.complete(&ModelRequest {
        model: model_settings.model.clone(),
        messages: vec![ModelMessage {
            role: ModelRole::User,
            content: format!(
                "Analyze this pwcli conversation or task summary for durable memory candidates.\n\
                 Use the ontology exactly: facts are observed or user-stated facts; logic_chains explain strict causal reasoning; inferences must be supported by logic_chains; hypotheses are useful but unproven and need confidence.\n\
                 Do not invent facts. If nothing is worth remembering, return empty arrays.\n\n\
                 {schema_hint}\n\n\
                 Source text:\n{input}"
            ),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
        }],
        system: Some(
            "You extract durable memory for a personal AI workbench. Return valid JSON only. No markdown."
                .to_string(),
        ),
        thinking: ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        max_tokens: Some(1200),
        stream: false,
        tools: Vec::new(),
    })?;
    let mut extraction = parse_semantic_memory_json(&response.content)?;
    extraction.facts.truncate(extraction_settings.max_facts);
    extraction
        .logic_chains
        .truncate(extraction_settings.max_logic_chains);
    extraction
        .inferences
        .truncate(extraction_settings.max_inferences);
    extraction
        .hypotheses
        .truncate(extraction_settings.max_hypotheses);
    if extraction.facts.is_empty()
        && extraction.logic_chains.is_empty()
        && extraction.inferences.is_empty()
        && extraction.hypotheses.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(extraction))
}

fn parse_semantic_memory_json(raw: &str) -> Result<SemanticMemoryExtraction> {
    let json_text = extract_json_object(raw).ok_or_else(|| {
        pwcli::PwError::Message("memory extraction did not return JSON".to_string())
    })?;
    serde_json::from_str(json_text).map_err(|err| {
        pwcli::PwError::Message(format!("memory extraction JSON parse failed: {err}"))
    })
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if start < end {
        Some(&trimmed[start..=end])
    } else {
        None
    }
}

fn print_status() -> Result<()> {
    let settings = Settings::load()?;
    println!("{}", build_status_report(&settings));
    Ok(())
}

fn print_doctor() -> Result<()> {
    let settings = Settings::load()?;
    println!("{}", build_doctor_report(&settings));
    Ok(())
}

fn run_providers_cli() -> Result<()> {
    let settings = Settings::load()?;
    print!("{}", providers_text(&settings));
    Ok(())
}

fn run_provider_cli(args: &[String]) -> Result<()> {
    let Some(provider) = args.first() else {
        let settings = Settings::load()?;
        println!(
            "current provider: {}\nuse `pwcli provider <name>` to switch\n\n{}",
            settings.provider,
            providers_text(&settings)
        );
        return Ok(());
    };
    let mut settings = Settings::load()?;
    settings.set_provider(provider)?;
    save_normalized_settings(&mut settings)?;
    println!(
        "provider switched to {}\nmodel: {}",
        settings.provider, settings.model
    );
    Ok(())
}

fn run_models_cli() -> Result<()> {
    let settings = Settings::load()?;
    print!("{}", models_text(&settings));
    Ok(())
}

fn run_model_cli(args: &[String]) -> Result<()> {
    let Some(model) = args.first() else {
        let settings = Settings::load()?;
        println!(
            "current model: {}\nuse `pwcli model <name>` to switch\n\n{}",
            settings.model,
            models_text(&settings)
        );
        return Ok(());
    };
    let mut settings = Settings::load()?;
    settings.set_model(model)?;
    save_normalized_settings(&mut settings)?;
    println!("model switched to {}", settings.model);
    Ok(())
}

fn run_thinking_cli(args: &[String]) -> Result<()> {
    let Some(value) = args.first() else {
        let settings = Settings::load()?;
        println!(
            "thinking: {}\nuse `pwcli thinking on` or `pwcli thinking off`",
            if settings.thinking { "on" } else { "off" }
        );
        return Ok(());
    };
    let mut settings = Settings::load()?;
    match value.as_str() {
        "on" | "true" | "1" | "yes" => {
            settings.set_thinking(true);
            save_normalized_settings(&mut settings)?;
            println!("thinking enabled");
        }
        "off" | "false" | "0" | "no" => {
            settings.set_thinking(false);
            save_normalized_settings(&mut settings)?;
            println!("thinking disabled");
        }
        _ => println!("usage: pwcli thinking on|off"),
    }
    Ok(())
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    let mut trimmed = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        trimmed.push_str("\n...[truncated]");
    }
    trimmed
}

fn print_help(topic: Option<&str>) {
    println!("{}", help_text(topic));
}

fn help_text(topic: Option<&str>) -> String {
    let Some(topic) = topic else {
        return main_help_text().to_string();
    };
    match topic {
        "memory" | "mem" => memory_help_text().to_string(),
        "task" | "tasks" | "runtime" => task_help_text().to_string(),
        "workflow" | "wf" => workflow_help_text().to_string(),
        "tools" | "tool" => tools_help_text().to_string(),
        "mcp" => mcp_help_text().to_string(),
        "skill" | "skills" => skill_help_text().to_string(),
        "agent" | "agents" | "code-agent" | "code_agents" => agent_help_text().to_string(),
        "config" | "provider" | "providers" | "model" | "models" | "thinking" => {
            config_help_text().to_string()
        }
        "rules" | "rule" | "policy" => rules_help_text().to_string(),
        "sessions" | "session" => sessions_help_text().to_string(),
        "audit" | "trace" => audit_help_text().to_string(),
        "context" => context_help_text().to_string(),
        "verify" | "verification" => verify_help_text().to_string(),
        "serve" | "server" | "web" => serve_help_text().to_string(),
        _ => format!(
            "unknown help topic: {topic}\n\navailable topics: memory, task, workflow, tools, mcp, skill, agent, config, rules, sessions, audit, context, verify, serve\n\n{}",
            main_help_text()
        ),
    }
}

fn main_help_text() -> &'static str {
    "pwcli

Commands:
  pwcli                  Start interactive session
  init                   Initialize ~/.pwcli
  status                 Show provider/model/tools/memory/task status
  doctor                 Diagnose config, tools, memory, and local code agents
  config <cmd>           Show, validate, or locate ~/.pwcli/config.json
  providers              List configured providers
  provider <name>        Switch provider and save config
  models                 List models for current provider
  model <name>           Switch model and save config
  thinking on|off        Toggle thinking mode and save config
  tools <cmd>            List or call discovered tools
  mcp <cmd>              Add, list, remove, and diagnose MCP servers
  skill <cmd>            List, reload, and diagnose ~/.agents/skills
  context <prompt>       Preview Context Pack and selected tools
  audit summary|tail     Inspect trace, token, tool, and policy events
  verify                 Run deterministic project checks
  memory <cmd>           Manage memory inbox/facts/search/extract/graph/events/embedder
  rules <cmd>            Manage hard project/user rules in ~/.pwcli/rules
  goal <objective>       Create and activate a runtime task goal
  plan [agent] <prompt>  Start code-agent planning in active task
  loop [agent] <prompt>  Start YOLO execute loop in active task
  review [agent] <prompt> Start code-agent review in active task
  sessions <cmd>         List, show, or delete saved graph sessions
  task <cmd>             Manage runtime background tasks
  workflow <cmd>         Run resumable graph workflows over agents/tools/approval
  serve                  Start local web workbench service
  next                   Show recommended next action for active task
  agent list             Show local code-agent availability and usage hints
  agent recommend <task> Recommend a local code-agent and command
  agent <name> <prompt>  Start a code-agent task, reusing active task by default
  run <prompt>           Run one graph turn with discovered context/tools
  self-test ask-user     Exercise Policy AskUser branch

Help topics:
  pwcli help memory
  pwcli help task
  pwcli help workflow
  pwcli help tools
  pwcli help agent
  pwcli help config
  pwcli help mcp
  pwcli help skill
  pwcli help rules
  pwcli help sessions
  pwcli help audit
  pwcli help context
  pwcli help verify
  pwcli help serve"
}

fn memory_help_text() -> &'static str {
    "pwcli memory

Purpose:
  Manage durable memory. Candidates are reviewed before writing facts.

Common workflow:
  pwcli memory inbox
  pwcli memory show <candidate_id>
  pwcli memory accept <candidate_id>
  pwcli memory reject <candidate_id>

Commands:
  pwcli memory inbox
      List pending memory candidates.
  pwcli memory facts
      List accepted facts.
  pwcli memory search <query>
      Recall related facts from the memory graph.
  pwcli memory add fact <statement>
      Manually write one fact with a generated source note.
  pwcli memory extract task [id|last|active]
      Extract memory candidates from a runtime task summary/logs.
  pwcli memory extract file <path>
      Extract memory candidates from a local file.
  pwcli memory extract text <text>
      Extract memory candidates from explicit text.
  pwcli memory derive [query]
      Derive inference/hypothesis candidates from accepted facts and memory graph edges.
  pwcli memory graph
      Show fact/HNSW/vector index stats.
  pwcli memory events
      Show memory lifecycle events: candidates, facts, edge writes, and decisions.
  pwcli memory rebuild
      Rebuild the memory graph index from accepted facts.
  pwcli memory embedder ensure
      Download/check the local BGE embedding model when configured."
}

fn task_help_text() -> &'static str {
    "pwcli task

Purpose:
  Track goals, code-agent background work, callbacks, logs, compaction, and next actions.

Common workflow:
  pwcli goal <objective>
  pwcli plan --wait
  pwcli loop --wait
  pwcli next
  pwcli review --wait

Commands:
  pwcli task new <goal>
      Create and activate a runtime task.
  pwcli task use <id|prefix|last|active>
      Set the active runtime task.
  pwcli task list
      List tasks, marking the active task and review requirements.
  pwcli task status [id|last|active]
      Show task JSON plus recommended next action.
  pwcli task next [id|last|active]
      Show the next recommended command.
  pwcli task log [id|last|active]
      Print stdout/stderr/events for a task.
  pwcli task watch [id|last|active]
      Attach to persisted task events until completion.
  pwcli task cancel [id|last|active]
      Request cancellation.
  pwcli task verify [id|last|active] [--cmd <command>]
      Run deterministic verification in the task workspace and attach the result.
  pwcli task compact [id|last|active]
      Compact task context and optionally create a memory candidate."
}

fn workflow_help_text() -> &'static str {
    "pwcli workflow

Purpose:
  Run resumable task graphs that compose code agents, deterministic tools, approval gates, and review branches.

Commands:
  pwcli workflow plan [--agent codex|claude|agy|qodercli] [--kind auto|code|research|ops|general] <goal>
      Print the auto-planned graph as Mermaid without running it.
  pwcli workflow run [--agent codex|claude|agy|qodercli] [--kind auto|code|research|ops|general] [--yes] [--dry-run] <goal>
      Create an active workflow task and execute the planned graph.
  pwcli workflow run --recipe <name> [--yes]
      Execute a saved workflow recipe as a new active task.
  pwcli workflow resume [id|last|active] [--yes]
      Resume an interrupted workflow from the next edge after the approval/interrupt node.
  pwcli workflow status [id|last|active]
      Show task JSON, workflow state, and Mermaid graph.
  pwcli workflow save <name> [--agent codex] [--kind auto|code|research|ops|general] <goal>
      Save a planned graph as a reusable recipe.
  pwcli workflow save <name> --from [id|last|active] [--force]
      Save a completed workflow task graph as a recipe.
  pwcli workflow recipes
      List saved recipes.
  pwcli workflow recipe show <name>
      Show recipe metadata and Mermaid graph.
  pwcli workflow recipe run <name> [--yes]
      Execute a recipe."
}

fn tools_help_text() -> &'static str {
    "pwcli tools

Purpose:
  Inspect or directly call tools after discovery. Manual calls pass through policy and audit.

Commands:
  pwcli tools list
      List builtin, agent_cli, skill, MCP, verification, and model tools.
  pwcli tools show <tool_id>
      Show one tool's schema, source, risk, invocation mode, capabilities, and metadata.
  pwcli tools call <tool_id> '<json-args>'
      Execute one tool directly.
  pwcli tools enable <tool_id-or-pattern>
      Remove a tool/pattern from the disabled and deny lists.
  pwcli tools disable <tool_id-or-pattern>
      Add a tool/pattern to the disabled list in ~/.pwcli/config.json.
  pwcli tools doctor
      Show health checks for integrations, schemas, MCP, skills, and local CLIs.
  pwcli tools reload
      Rebuild the registry once and print the resulting counts.

Examples:
  pwcli tools list
  pwcli tools show builtin.anysearch_search
  pwcli tools call verification.project_check '{\"commands\":[\"cargo test\"]}'"
}

fn mcp_help_text() -> &'static str {
    "pwcli mcp

Purpose:
  Manage MCP servers in ~/.pwcli/config.json.

Commands:
  pwcli mcp list
  pwcli mcp add <name> --command <binary> [--arg <value>] [--env KEY=VALUE]
  pwcli mcp add <name> --transport http --url <url> [--header KEY=VALUE]
  pwcli mcp add <name> --transport sse --url <url> [--header KEY=VALUE]
  pwcli mcp remove <name>
  pwcli mcp doctor

Examples:
  pwcli mcp add filesystem --command mcp-server-filesystem --arg /tmp
  pwcli mcp add docs --transport http --url https://example.com/mcp --header authorization=Bearer_x"
}

fn skill_help_text() -> &'static str {
    "pwcli skill

Purpose:
  Inspect and reload external skills from ~/.agents/skills.

Commands:
  pwcli skill list
  pwcli skill reload
  pwcli skill doctor"
}

fn agent_help_text() -> &'static str {
    "pwcli agent / plan / loop / review

Purpose:
  Start local code agents as managed runtime tasks. Work in an active task is reused by default.

Agents:
  codex
  claude
  agy
  qodercli

Commands:
  pwcli agent list
  pwcli agent recommend [--mode <goal|plan|execute|review>] <prompt>
  pwcli agent <codex|claude|agy|qodercli> [options] <prompt>
  pwcli plan [agent] [--wait] [prompt]
  pwcli loop [agent] [--wait] [prompt]
  pwcli review [agent] [--wait] [prompt]

Useful options:
  --task <id>            Attach to a specific task.
  --wait                 Run in foreground and stream result.
  --mode direct|goal|plan|execute|review
  --model <model>
  --effort <effort>      Defaults to high for agent modes.
  --cwd <dir>
  --yolo                 Use the agent's dangerous/full-auto mode when supported."
}

fn config_help_text() -> &'static str {
    "pwcli config / providers / models

Config file:
  ~/.pwcli/config.json

Provider shape:
  Providers have a user-chosen name, protocol, base_url, api_key, and models.
  Protocol is one of: openai, anthropic, nvidia.

Commands:
  pwcli config path
      Print the config file path.
  pwcli config show
      Print effective config as JSON with api_key/token fields redacted.
  pwcli config validate
      Validate active provider/model, model limits, optional integrations, and key presence.
  pwcli config add-provider <name> --protocol <openai|anthropic|nvidia> --base-url <url> [--api-key <key>] [--model <name>] [--set-active]
      Add a provider without hand-editing JSON. Pass --replace to overwrite an existing provider.
  pwcli config add-model <provider> <model> [--input <tokens>] [--output <tokens>] [--thinking] [--image-input] [--image-generation] [--set-active]
      Add one model to an existing provider. Pass --replace to overwrite an existing model.
  pwcli config update-provider <name> [--protocol <openai|anthropic|nvidia>] [--base-url <url>] [--api-key <key>|--clear-api-key] [--set-active]
      Update only the specified provider fields, preserving its models.
  pwcli config update-model <provider> <model> [--input <tokens>] [--output <tokens>] [--thinking|--no-thinking] [--image-input|--no-image-input] [--image-generation|--no-image-generation] [--set-active]
      Update only the specified model capability fields.
  pwcli providers
      List configured providers.
  pwcli provider <name>
      Switch active provider.
  pwcli models
      List models for the active provider.
  pwcli model <name>
      Switch active model.
  pwcli thinking on|off
      Toggle global thinking mode. Provider-specific request params are derived internally."
}

fn rules_help_text() -> &'static str {
    "pwcli rules

Purpose:
  Manage hard rules in ~/.pwcli/rules. These are loaded by policy before tool execution.

Commands:
  pwcli rules list
  pwcli rules show <name>
  pwcli rules add <name> <text>
  pwcli rules rm <name>"
}

fn sessions_help_text() -> &'static str {
    "pwcli sessions

Purpose:
  Inspect saved graph sessions under ~/.pwcli/sessions.

Commands:
  pwcli sessions list
  pwcli sessions show <id|prefix|last>
  pwcli sessions delete <id|prefix|last>

Resume:
  pwcli run --session <id|prefix|last> <prompt>"
}

fn audit_help_text() -> &'static str {
    "pwcli audit

Purpose:
  Inspect trace events for graph runs, tools, policy decisions, sessions, and runtime tasks.

Commands:
  pwcli audit summary
  pwcli audit tail [n]"
}

fn context_help_text() -> &'static str {
    "pwcli context

Purpose:
  Preview the Context Pack and selected tool snapshot for a prompt.

Command:
  pwcli context <prompt>"
}

fn verify_help_text() -> &'static str {
    "pwcli verify

Purpose:
  Run deterministic project checks through the verification tool. Calls are audited.
  Use `pwcli task verify` when the result should be attached to the active runtime task.

Commands:
  pwcli verify
  pwcli verify --cmd <command>
  pwcli verify --cwd <dir> --timeout <sec> --max-output <chars> --cmd <command>
  pwcli verify -- cargo test"
}

fn serve_help_text() -> &'static str {
    "pwcli serve

Purpose:
  Start the local web workbench service. It exposes HTTP + SSE APIs and an embedded local UI
  for chat runs, tools, approvals, tasks, memory, sessions, audit, and config.

Commands:
  pwcli serve
  pwcli serve --open
  pwcli serve --host 127.0.0.1 --port 8791
  pwcli serve --no-ui

Safety:
  Default bind address is 127.0.0.1. Binding to another host exposes local tools over HTTP."
}

fn interactive_help_text() -> String {
    "Commands:\n  /help [topic]     Show this help, or topic help: memory/task/tools/agent/config/rules\n  /status           Show current provider/model/tools/memory/task status\n  /doctor           Diagnose config, tools, memory, and local code agents\n  /config           Show redacted effective config\n  /config validate  Validate active provider/model and integration settings\n  /config add-provider <name> --protocol <openai|anthropic|nvidia> --base-url <url> [--model <name>]\n  /config update-provider <name> [--base-url <url>] [--api-key <key>|--clear-api-key]\n  /config add-model <provider> <model> [--input <tokens>] [--output <tokens>] [--thinking]\n  /config update-model <provider> <model> [--input <tokens>] [--output <tokens>] [--no-thinking]\n  /audit            Show trace/token/tool/policy summary\n  /audit tail [n]   Show recent audit events\n  /verify           Run deterministic project checks\n  /tools list       List discovered tools\n  /tools show id    Show one tool schema/risk/source/metadata\n  /providers        List configured providers\n  /provider <name>  Switch provider and save ~/.pwcli/config.json\n  /models           List models for current provider\n  /model <name>     Switch model and save ~/.pwcli/config.json\n  /thinking on|off  Toggle thinking mode\n  /context <tokens> Set context max input tokens\n  /context pack q   Preview Context Pack and selected tools\n  /goal <objective> Create and activate a runtime task goal\n  /plan [agent] q   Start planning in active task\n  /loop [agent] q   Start YOLO execute loop in active task\n  /review [agent] q Start review in active task\n  /next             Show recommended next action\n  /session list     List saved graph sessions\n  /session show id  Show a saved graph session; id can be last\n  /session resume id Continue future prompts from a saved session\n  /session delete id Delete a saved session\n  /memory inbox     Review candidate facts before writing\n  /memory show id   Show full candidate details\n  /memory facts     List accepted facts\n  /memory search q  Search recalled facts\n  /memory extract task Extract active task into memory inbox\n  /memory graph     Show memory graph index stats\n  /memory derive    Derive candidates from memory graph\n  /memory events    Show memory lifecycle timeline\n  /memory rebuild   Rebuild memory graph index\n  /memory embedder ensure Download/check local embedding model\n  /memory accept id Accept a candidate into facts\n  /rules list       List hard rules\n  /rules add n text Add or replace a hard rule\n  /rules show n     Show one hard rule\n  /rules rm n       Remove one hard rule\n  /task new <goal>  Create and activate a runtime task\n  /task use id      Set active runtime task; id can be last\n  /task list        List runtime tasks\n  /task status      Show active task status\n  /task next        Show recommended next action\n  /task log         Show active task logs\n  /task watch       Attach to persisted task events\n  /task verify      Run checks and attach result to active task\n  /task compact     Compact active task context\n  /workflow plan q  Show auto-planned workflow graph\n  /workflow run q   Run resumable agent/tool workflow\n  /workflow save n q Save a workflow recipe\n  /agents           Show local code-agent availability and usage hints\n  /agent <name> ... Start codex/claude/agy/qodercli in the active task\n  /compact          Compact active task context\n  /run <prompt>     Run one prompt\n  /exit             Quit\n\nAnything else is sent as a prompt.".to_string()
}

fn interactive_topic_help_text(topic: &str) -> String {
    if matches!(topic, "tools" | "tool") {
        return interactive_tools_help_text();
    }
    help_text(Some(topic)).replace("pwcli ", "/")
}

fn interactive_tools_help_text() -> String {
    "/tools

Purpose:
  Inspect tools available to the current graph/tool selector.

Commands:
  /tools list
      List discovered builtin, agent_cli, skill, MCP, verification, and model tools.
  /tools show <tool_id>
      Show one tool's schema, source, risk, invocation mode, capabilities, and metadata.

Direct execution:
  Tool execution remains CLI-only here so policy approval can use a clean prompt:
  pwcli tools call <tool_id> '<json-args>'"
        .to_string()
}

fn models_text(settings: &Settings) -> String {
    let Ok(provider) = settings.active_provider() else {
        return format!("unknown provider: {}", settings.provider);
    };
    let mut out = String::new();
    for model in &provider.models {
        let marker = if model.name == settings.model {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            "{marker} {}\timage_input={}\tthinking={}\timage_generation={}\tinput={}\toutput={}",
            model.name,
            model.supports_image_input,
            model.supports_thinking,
            model.is_image_generation,
            model.max_input_tokens,
            model.max_output_tokens
        ));
        out.push('\n');
    }
    out
}

fn providers_text(settings: &Settings) -> String {
    let mut out = String::new();
    for provider in &settings.providers {
        let marker = if provider.name == settings.provider {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            "{marker} {}\tprotocol={}\tbase_url={}\tmodels={}",
            provider.name,
            provider.protocol.as_str(),
            provider.base_url,
            provider.models.len()
        ));
        out.push('\n');
    }
    out
}

enum PromptWorkerEvent {
    Delta(String),
    AskUser {
        prompt: String,
        tool_name: String,
        response_tx: mpsc::Sender<bool>,
    },
    Done {
        session_id: String,
    },
    Error(String),
}

struct TuiApproval {
    event_tx: mpsc::Sender<PromptWorkerEvent>,
}

impl UserApproval for TuiApproval {
    fn ask_user(&self, prompt: &str, call: &ToolCall) -> bool {
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .event_tx
            .send(PromptWorkerEvent::AskUser {
                prompt: prompt.to_string(),
                tool_name: call.name.clone(),
                response_tx,
            })
            .is_err()
        {
            return false;
        }
        response_rx.recv().unwrap_or(false)
    }
}

struct SuggestionEngine {
    request_tx: mpsc::Sender<SuggestionRequest>,
    response_rx: mpsc::Receiver<SuggestionResponse>,
    next_id: u64,
    latest_request_id: u64,
    pending_input: String,
    cached_input: String,
    cached_suggestion: String,
}

struct SuggestionRequest {
    id: u64,
    input: String,
    settings: Settings,
}

struct SuggestionResponse {
    id: u64,
    input: String,
    suggestion: Option<String>,
}

impl SuggestionEngine {
    fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<SuggestionRequest>();
        let (response_tx, response_rx) = mpsc::channel::<SuggestionResponse>();
        thread::spawn(move || suggestion_worker(request_rx, response_tx));
        Self {
            request_tx,
            response_rx,
            next_id: 1,
            latest_request_id: 0,
            pending_input: String::new(),
            cached_input: String::new(),
            cached_suggestion: String::new(),
        }
    }

    fn suggest(&mut self, input: &str, settings: &Settings) -> Option<String> {
        if let Some(local) = local_suggestion_text(input, settings) {
            return Some(local);
        }

        self.drain_responses();
        if !is_model_suggestion_candidate(input) {
            return None;
        }
        if self.cached_input == input {
            return (!self.cached_suggestion.is_empty()).then(|| self.cached_suggestion.clone());
        }
        if self.pending_input != input {
            let id = self.next_id;
            self.next_id += 1;
            self.latest_request_id = id;
            self.pending_input = input.to_string();
            let _ = self.request_tx.send(SuggestionRequest {
                id,
                input: input.to_string(),
                settings: settings.clone(),
            });
        }
        None
    }

    fn drain_responses(&mut self) {
        while let Ok(response) = self.response_rx.try_recv() {
            if response.id == self.latest_request_id {
                self.cached_input = response.input;
                self.cached_suggestion = response.suggestion.unwrap_or_default();
                self.pending_input.clear();
            }
        }
    }
}

fn suggestion_worker(
    request_rx: mpsc::Receiver<SuggestionRequest>,
    response_tx: mpsc::Sender<SuggestionResponse>,
) {
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(newer) = request_rx.try_recv() {
            request = newer;
        }
        thread::sleep(Duration::from_millis(450));
        while let Ok(newer) = request_rx.try_recv() {
            request = newer;
        }
        let suggestion = generate_model_suggestion(&request.input, &request.settings);
        let _ = response_tx.send(SuggestionResponse {
            id: request.id,
            input: request.input,
            suggestion,
        });
    }
}

fn generate_model_suggestion(input: &str, settings: &Settings) -> Option<String> {
    let mut model_settings = settings.resolved_model_settings().ok()?;
    if model_settings.is_image_generation {
        return None;
    }
    if !model_settings_has_key(&model_settings) {
        return None;
    }
    model_settings.request_timeout_seconds = 5;
    let client = AnyModelClient::from_settings(&model_settings).ok()?;
    let response = client.complete(&ModelRequest {
        model: model_settings.model.clone(),
        messages: vec![ModelMessage {
            role: ModelRole::User,
            content: format!("Current partial user input:\n{input}\n\nReturn only the continuation."),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
        }],
        system: Some(
            "You complete a user's partially typed prompt. Return a short continuation only. Do not repeat the existing text. Do not explain.".to_string(),
        ),
        thinking: ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        max_tokens: Some(24),
        stream: false,
        tools: Vec::new(),
    }).ok()?;
    clean_model_suggestion(input, &response.content)
}

fn model_settings_has_key(model_settings: &pwcli::settings::ModelSettings) -> bool {
    model_settings
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
        || std::env::var(&model_settings.api_key_env)
            .ok()
            .is_some_and(|key| !key.trim().is_empty())
}

fn clean_model_suggestion(input: &str, raw: &str) -> Option<String> {
    let mut text = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .replace(['\n', '\r'], " ");
    while text.contains("  ") {
        text = text.replace("  ", " ");
    }
    if text.starts_with(input) {
        text = text[input.len()..].trim_start().to_string();
    }
    if text.is_empty() {
        return None;
    }
    const LIMIT: usize = 80;
    if text.chars().count() > LIMIT {
        text = text.chars().take(LIMIT).collect();
    }
    Some(text)
}

fn is_model_suggestion_candidate(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.len() >= 4 && !trimmed.starts_with('/')
}

fn local_suggestion_text(input: &str, settings: &Settings) -> Option<String> {
    if input.trim().is_empty() {
        return None;
    }

    if input.starts_with("/provider ") {
        let prefix = input.trim_start_matches("/provider ").trim();
        return settings
            .providers
            .iter()
            .map(|provider| provider.name.as_str())
            .find(|name| name.starts_with(prefix) && *name != prefix)
            .map(|name| name[prefix.len()..].to_string());
    }

    if input.starts_with("/model ") {
        let prefix = input.trim_start_matches("/model ").trim();
        return settings
            .active_provider()
            .ok()?
            .models
            .iter()
            .map(|model| model.name.as_str())
            .find(|name| name.starts_with(prefix) && *name != prefix)
            .map(|name| name[prefix.len()..].to_string());
    }

    if input.starts_with('/') {
        let commands = [
            "/help",
            "/help ",
            "/status",
            "/doctor",
            "/config ",
            "/audit ",
            "/verify ",
            "/tools ",
            "/agents",
            "/agent ",
            "/agent recommend ",
            "/providers",
            "/provider ",
            "/models",
            "/model ",
            "/thinking ",
            "/context ",
            "/context pack ",
            "/goal ",
            "/plan ",
            "/loop ",
            "/review ",
            "/next",
            "/sessions",
            "/session ",
            "/memory ",
            "/rules ",
            "/task ",
            "/agent ",
            "/compact",
            "/run ",
            "/exit",
        ];
        return commands
            .iter()
            .find(|command| command.starts_with(input) && **command != input)
            .map(|command| command[input.len()..].to_string());
    }

    None
}

struct SelfTestExecutor;

impl ToolExecutor for SelfTestExecutor {
    fn execute(&self, _call: &ToolCall) -> Result<ToolResult> {
        Ok(ToolResult::ok("self-test tool executed"))
    }
}

fn self_test_ask_user() -> Result<()> {
    let settings = Settings::load()?;
    WorkspacePaths::from_pwcli_home(&settings.pwcli_home).ensure()?;
    let audit = JsonlAuditRecorder::new(settings.pwcli_home.join("audit/events.jsonl"));

    let mut registry = ToolRegistry::new();
    registry.register(LoadedTool {
        descriptor: ToolDescriptor {
            id: "builtin.self_test_risky".to_string(),
            name: "self-test-risky".to_string(),
            description: "Risky tool used to test Policy AskUser".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec!["self-test".to_string()],
            metadata: serde_json::json!({}),
            enabled: true,
        },
        executor: Some(Arc::new(SelfTestExecutor)),
    });

    let snapshot = registry.snapshot();
    let context_pack = ContextBuilder::new().build("self-test ask-user", &snapshot);
    let graph = GraphExecutor::builder().build();
    let policy = DefaultPolicyGuard::default();
    let approval = StdinPrompter;
    let mut planner = PlannedToolCallPlanner::new(vec![ToolCall {
        id: "self-test-call".to_string(),
        tool_id: "builtin.self_test_risky".to_string(),
        name: "self-test-risky".to_string(),
        arguments: serde_json::json!({}),
    }]);

    let summary = graph.run_with_planner(
        GraphRunRequest {
            user_input: "self-test ask-user".to_string(),
            context_pack,
        },
        &snapshot,
        &mut planner,
        &policy,
        &audit,
        Some(&approval),
    )?;

    println!("{}", summary.state.last_content);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        agent_inventory_text, agent_mode_args, agent_mode_args_for_runtime, agent_mode_has_prompt,
        clean_model_suggestion, config_command_text, config_report_text, execute_manual_tool_call,
        format_memory_candidate, format_task_list, format_tool_descriptor, format_tools_list,
        help_text, interactive_help_text, interactive_topic_help_text, load_rule_texts,
        memory_candidate_turn_text, memory_task_source_text, normalize_rule_name,
        parse_agent_recommendation_args, parse_workflow_run_options, recommend_code_agent,
        recommended_agent_command, sanitize_workflow_recipe_name, session_id_for_save,
        set_tool_enabled_in_settings, split_command_line, split_workflow_save_args,
        CodeAgentRecommendation, PromptWorkerEvent, TuiApproval, WorkflowPlanKind,
    };
    use pwcli::memory::{
        CandidateFact, CandidateHypothesis, CandidateInference, CandidateLogicChain,
        MemoryCandidate, MemoryCandidateReview,
    };
    use pwcli::runtime::{
        format_task_next, RuntimeTask, RuntimeTaskEvent, RuntimeTaskKind, RuntimeTaskManager,
        RuntimeTaskStatus,
    };
    use pwcli::{
        policy::UserApproval,
        settings::{ModelDefinition, ProviderProtocol, ProviderSettings, Settings},
        tools::{
            agent_cli::AgentCliKind, InvocationMode, LoadedTool, RiskLevel, ToolCall,
            ToolDescriptor, ToolExecutor, ToolRegistry, ToolResult, ToolSource,
        },
    };
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        },
        thread,
    };

    struct CountingExecutor(Arc<AtomicUsize>);

    impl ToolExecutor for CountingExecutor {
        fn execute(&self, _call: &ToolCall) -> pwcli::Result<ToolResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::ok("executed"))
        }
    }

    #[test]
    fn split_command_line_preserves_quoted_verify_command() {
        assert_eq!(
            split_command_line(r#"--cmd "cargo check --all-targets""#),
            vec!["--cmd", "cargo check --all-targets"]
        );
    }

    #[test]
    fn split_command_line_supports_single_quotes_and_escapes() {
        assert_eq!(
            split_command_line(r#"-C '/tmp/work dir' --cmd echo\ OK"#),
            vec!["-C", "/tmp/work dir", "--cmd", "echo OK"]
        );
    }

    #[test]
    fn workflow_run_options_parse_kind_recipe_agent_and_flags() {
        let options = parse_workflow_run_options(&[
            "--agent=claude".to_string(),
            "--kind".to_string(),
            "research".to_string(),
            "--recipe".to_string(),
            "market-scan".to_string(),
            "--yes".to_string(),
            "--dry-run".to_string(),
            "ignored".to_string(),
        ]);

        assert_eq!(options.agent.as_deref(), Some("claude"));
        assert_eq!(options.kind, WorkflowPlanKind::Research);
        assert_eq!(options.recipe.as_deref(), Some("market-scan"));
        assert!(options.yes);
        assert!(options.dry_run);
        assert_eq!(options.goal, "ignored");
    }

    #[test]
    fn workflow_save_args_extract_source_task_and_keep_workflow_args() {
        let (source, force, rest) = split_workflow_save_args(&[
            "--from".to_string(),
            "last".to_string(),
            "--force".to_string(),
            "--agent".to_string(),
            "codex".to_string(),
        ]);

        assert_eq!(source.as_deref(), Some("last"));
        assert!(force);
        assert_eq!(rest, vec!["--agent", "codex"]);

        let (source, force, rest) =
            split_workflow_save_args(&["--from".to_string(), "--agent=claude".to_string()]);
        assert_eq!(source.as_deref(), Some("active"));
        assert!(!force);
        assert_eq!(rest, vec!["--agent=claude"]);
    }

    #[test]
    fn workflow_recipe_names_are_sanitized_and_reject_paths() {
        assert_eq!(
            sanitize_workflow_recipe_name("Research Recipe 1").unwrap(),
            "Research-Recipe-1"
        );
        assert_eq!(sanitize_workflow_recipe_name("ops.json").unwrap(), "ops");
        assert!(sanitize_workflow_recipe_name("../ops").is_err());
        assert!(sanitize_workflow_recipe_name("..").is_err());
    }

    #[test]
    fn agent_mode_args_default_to_codex_plan() {
        let args = agent_mode_args("plan", false, &["write a plan".to_string()]);
        assert_eq!(
            args,
            vec![
                "codex",
                "--mode",
                "plan",
                "--effort",
                "high",
                "write a plan"
            ]
        );
    }

    #[test]
    fn agent_mode_args_support_explicit_agent_and_loop_yolo() {
        let args = agent_mode_args(
            "execute",
            true,
            &[
                "qodercli".to_string(),
                "--wait".to_string(),
                "do it".to_string(),
            ],
        );
        assert_eq!(args[0], "qodercli");
        assert!(args.contains(&"--mode".to_string()));
        assert!(args.contains(&"execute".to_string()));
        assert!(args.contains(&"--yolo".to_string()));
        assert!(args.contains(&"--wait".to_string()));
    }

    #[test]
    fn agent_mode_args_accept_case_insensitive_agent_names() {
        let args = agent_mode_args("plan", false, &["Codex".to_string(), "do it".to_string()]);
        assert_eq!(args[0], "codex");
        assert!(args.contains(&"plan".to_string()));
    }

    #[test]
    fn agent_recommendation_routes_by_mode_and_availability() {
        let review = recommend_code_agent(
            Some("review"),
            "检查这个 patch 的风险",
            &[AgentCliKind::Claude, AgentCliKind::Codex],
        );
        assert_eq!(review.kind, AgentCliKind::Codex);
        assert_eq!(review.mode, "review");

        let plan = recommend_code_agent(
            None,
            "帮我设计一个架构方案",
            &[AgentCliKind::Claude, AgentCliKind::Codex],
        );
        assert_eq!(plan.kind, AgentCliKind::Claude);
        assert_eq!(plan.mode, "plan");

        let execute = recommend_code_agent(
            Some("execute"),
            "实现这个功能",
            &[AgentCliKind::QoderCli, AgentCliKind::Agy],
        );
        assert_eq!(execute.kind, AgentCliKind::QoderCli);
        assert_eq!(execute.mode, "execute");
    }

    #[test]
    fn agent_recommendation_formats_command() {
        let (mode, prompt) = parse_agent_recommendation_args(&[
            "--mode=review".to_string(),
            "检查".to_string(),
            "风险".to_string(),
        ]);
        assert_eq!(mode.as_deref(), Some("review"));
        assert_eq!(prompt, "检查 风险");

        let recommendation = CodeAgentRecommendation {
            kind: AgentCliKind::Codex,
            mode: "review",
            reason: "test",
        };
        let command = recommended_agent_command(recommendation, "检查 John's patch");
        assert_eq!(command, "pwcli review codex --wait '检查 John'\\''s patch'");
    }

    #[test]
    fn agent_mode_prompt_detection_skips_options() {
        assert!(!agent_mode_has_prompt(&[
            "codex".to_string(),
            "--wait".to_string(),
            "--model".to_string(),
            "best".to_string()
        ]));
        assert!(agent_mode_has_prompt(&[
            "codex".to_string(),
            "--wait".to_string(),
            "make a plan".to_string()
        ]));
    }

    #[test]
    fn help_topics_cover_core_workflows() {
        let main = help_text(None);
        assert!(main.contains("Help topics"));
        assert!(main.contains("pwcli help memory"));

        let memory = help_text(Some("memory"));
        assert!(memory.contains("pwcli memory extract task"));
        assert!(memory.contains("pwcli memory accept <candidate_id>"));

        let task = help_text(Some("task"));
        assert!(task.contains("pwcli task compact"));
        assert!(task.contains("pwcli task watch"));

        let agent = help_text(Some("agent"));
        assert!(agent.contains("pwcli agent list"));
        assert!(agent.contains("pwcli agent recommend"));
        assert!(agent.contains("codex"));

        let tools = help_text(Some("tools"));
        assert!(tools.contains("pwcli tools call <tool_id>"));
        assert!(tools.contains("pwcli tools show <tool_id>"));
        assert!(tools.contains("policy and audit"));

        let unknown = help_text(Some("missing"));
        assert!(unknown.contains("unknown help topic"));
        assert!(unknown.contains("available topics"));
    }

    #[test]
    fn interactive_help_topics_and_tool_descriptions_are_discoverable() {
        let main_help = interactive_help_text();
        assert!(main_help.contains("/config add-provider"));
        assert!(main_help.contains("/config update-model"));
        assert!(main_help.contains("/agents"));

        let agents = agent_inventory_text();
        assert!(agents.contains("pwcli code agents"));
        assert!(agents.contains("codex"));
        assert!(agents.contains("qodercli"));
        assert!(agents.contains("Usage:"));

        let tools_help = interactive_topic_help_text("tools");
        assert!(tools_help.contains("/tools show <tool_id>"));
        assert!(tools_help.contains("CLI-only"));

        let descriptor = ToolDescriptor {
            id: "builtin.demo".to_string(),
            name: "demo".to_string(),
            description: "demo tool".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::Low,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec!["demo.capability".to_string()],
            metadata: serde_json::json!({ "owner": "test" }),
            enabled: true,
        };
        let text = format_tool_descriptor(&descriptor);
        assert!(text.contains("id: builtin.demo"));
        assert!(text.contains("risk: Low"));
        assert!(text.contains("\"path\""));
        assert!(text.contains("\"owner\""));

        let mut registry = ToolRegistry::new();
        registry.register(LoadedTool {
            descriptor,
            executor: None,
        });
        let list = format_tools_list(&registry.snapshot());
        assert!(list.contains("builtin.demo"));
        assert!(list.contains("risk=Low"));
    }

    #[test]
    fn model_suggestion_cleanup_truncates_on_char_boundaries() {
        let suggestion = clean_model_suggestion(
            "请帮我",
            &"继续分析这个项目的架构边界和工具调用流程。".repeat(10),
        )
        .unwrap();
        assert!(suggestion.chars().count() <= 80);
        assert!(suggestion.is_char_boundary(suggestion.len()));
    }

    #[test]
    fn config_report_redacts_secrets_and_validates_effective_settings() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());
        settings.provider = "local".to_string();
        settings.model = "model-a".to_string();
        settings.providers = vec![ProviderSettings {
            name: "local".to_string(),
            protocol: ProviderProtocol::OpenAi,
            base_url: "http://127.0.0.1:8046/v1".to_string(),
            api_key: Some("sk-secret-value".to_string()),
            api_key_env: None,
            api: Default::default(),
            request_timeout_seconds: 0,
            stream: false,
            extra_body: serde_json::json!({}),
            models: vec![ModelDefinition {
                name: "model-a".to_string(),
                supports_image_input: true,
                supports_thinking: true,
                is_image_generation: false,
                max_input_tokens: 128000,
                max_output_tokens: 4096,
                extra_body: serde_json::json!({}),
            }],
        }];
        settings.mineru.token = Some("mineru-secret-token".to_string());
        settings.anysearch.api_key = Some("anysearch-secret-key".to_string());

        let shown = config_report_text(&["show".to_string()], &settings).unwrap();
        assert!(shown.contains("\"api_key\": \"configured\""));
        assert!(shown.contains("\"token\": \"configured\""));
        assert!(!shown.contains("sk-secret-value"));
        assert!(!shown.contains("mineru-secret-token"));
        assert!(!shown.contains("anysearch-secret-key"));

        let validation = config_report_text(&["validate".to_string()], &settings).unwrap();
        assert!(validation.contains("[ok] active provider: local (openai)"));
        assert!(validation.contains("provider local: protocol=openai"));
        assert!(validation.contains("key=config"));
        assert!(!validation.contains("sk-secret-value"));

        let path = config_report_text(&["path".to_string()], &settings).unwrap();
        assert!(path.ends_with(".pwcli/config.json"));
    }

    #[test]
    fn config_commands_add_provider_and_model_without_manual_json_editing() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());

        let output = config_command_text(
            &[
                "add-provider".to_string(),
                "antigravity".to_string(),
                "--protocol".to_string(),
                "OpenAI".to_string(),
                "--base-url".to_string(),
                "http://127.0.0.1:8046/v1".to_string(),
                "--api-key".to_string(),
                "sk-local-secret".to_string(),
                "--model".to_string(),
                "gemini-3-flash-agent".to_string(),
                "--input".to_string(),
                "1000000".to_string(),
                "--output".to_string(),
                "4096".to_string(),
                "--thinking".to_string(),
                "--image-input".to_string(),
                "--set-active".to_string(),
            ],
            &mut settings,
        )
        .unwrap();
        assert!(output.contains("provider saved: antigravity"));
        assert_eq!(settings.providers[0].request_timeout_seconds, 600);
        assert!(settings.providers[0].stream);

        let loaded = Settings::load_from_home(temp.path()).unwrap();
        assert_eq!(loaded.provider, "antigravity");
        assert_eq!(loaded.model, "gemini-3-flash-agent");
        assert_eq!(loaded.providers[0].models[0].max_input_tokens, 1_000_000);
        assert!(loaded.providers[0].models[0].supports_thinking);
        assert!(loaded.providers[0].models[0].supports_image_input);

        let mut loaded = loaded;
        let output = config_command_text(
            &[
                "add-model".to_string(),
                "antigravity".to_string(),
                "gemini-pro-agent".to_string(),
                "--input=1000000".to_string(),
                "--output=8192".to_string(),
                "--thinking".to_string(),
                "--image-input".to_string(),
                "--set-active".to_string(),
            ],
            &mut loaded,
        )
        .unwrap();
        assert!(output.contains("model saved: antigravity/gemini-pro-agent"));

        let reloaded = Settings::load_from_home(temp.path()).unwrap();
        assert_eq!(reloaded.model, "gemini-pro-agent");
        assert_eq!(reloaded.providers[0].models.len(), 2);
        let shown = config_report_text(&["show".to_string()], &reloaded).unwrap();
        assert!(shown.contains("\"api_key\": \"configured\""));
        assert!(!shown.contains("sk-local-secret"));
    }

    #[test]
    fn config_commands_update_provider_and_model_preserve_existing_fields() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());
        config_command_text(
            &[
                "add-provider".to_string(),
                "local".to_string(),
                "--protocol=openai".to_string(),
                "--base-url=http://127.0.0.1:8046/v1".to_string(),
                "--api-key=old-secret".to_string(),
                "--model=flash".to_string(),
                "--thinking".to_string(),
                "--image-input".to_string(),
                "--set-active".to_string(),
            ],
            &mut settings,
        )
        .unwrap();

        let output = config_command_text(
            &[
                "update-provider".to_string(),
                "local".to_string(),
                "--base-url".to_string(),
                "http://localhost:9999/v1".to_string(),
                "--api-key".to_string(),
                "new-secret".to_string(),
            ],
            &mut settings,
        )
        .unwrap();
        assert!(output.contains("provider updated: local"));
        let output = config_command_text(
            &[
                "update-provider".to_string(),
                "local".to_string(),
                "--protocol".to_string(),
                "nvidia".to_string(),
            ],
            &mut settings,
        )
        .unwrap();
        assert!(output.contains("provider updated: local"));
        assert_eq!(settings.providers[0].protocol, ProviderProtocol::Nvidia);
        assert_eq!(settings.providers[0].request_timeout_seconds, 120);
        assert!(!settings.providers[0].stream);

        let loaded = Settings::load_from_home(temp.path()).unwrap();
        assert_eq!(loaded.providers[0].base_url, "http://localhost:9999/v1");
        assert_eq!(loaded.providers[0].api_key.as_deref(), Some("new-secret"));
        assert_eq!(loaded.providers[0].models.len(), 1);
        assert_eq!(loaded.providers[0].models[0].name, "flash");

        let mut loaded = loaded;
        let output = config_command_text(
            &[
                "update-model".to_string(),
                "local".to_string(),
                "flash".to_string(),
                "--input".to_string(),
                "200000".to_string(),
                "--output=8192".to_string(),
                "--no-thinking".to_string(),
                "--no-image-input".to_string(),
                "--image-generation".to_string(),
            ],
            &mut loaded,
        )
        .unwrap();
        assert!(output.contains("model updated: local/flash"));

        let updated = Settings::load_from_home(temp.path()).unwrap();
        let model = &updated.providers[0].models[0];
        assert_eq!(model.max_input_tokens, 200_000);
        assert_eq!(model.max_output_tokens, 8192);
        assert!(!model.supports_thinking);
        assert!(!model.supports_image_input);
        assert!(model.is_image_generation);

        let shown = config_report_text(&["show".to_string()], &updated).unwrap();
        assert!(shown.contains("\"api_key\": \"configured\""));
        assert!(!shown.contains("new-secret"));
    }

    #[test]
    fn tool_enable_disable_updates_settings_patterns() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());
        set_tool_enabled_in_settings(&mut settings, "skill.legacy", false);
        assert_eq!(settings.tools.disabled, vec!["skill.legacy".to_string()]);
        set_tool_enabled_in_settings(&mut settings, "skill.legacy", false);
        assert_eq!(settings.tools.disabled, vec!["skill.legacy".to_string()]);
        settings.tools.denylist.push("skill.legacy".to_string());
        set_tool_enabled_in_settings(&mut settings, "skill.legacy", true);
        assert!(settings.tools.disabled.is_empty());
        assert!(settings.tools.denylist.is_empty());
    }

    #[test]
    fn config_add_provider_replace_active_requires_a_model_without_mutating_settings() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_home(temp.path());
        config_command_text(
            &[
                "add-provider".to_string(),
                "local".to_string(),
                "--protocol=openai".to_string(),
                "--base-url=http://127.0.0.1:8046/v1".to_string(),
                "--model=flash".to_string(),
                "--set-active".to_string(),
            ],
            &mut settings,
        )
        .unwrap();
        config_command_text(
            &[
                "add-provider".to_string(),
                "backup".to_string(),
                "--protocol=openai".to_string(),
                "--base-url=http://backup/v1".to_string(),
                "--model=backup-model".to_string(),
            ],
            &mut settings,
        )
        .unwrap();

        let err = config_command_text(
            &[
                "add-provider".to_string(),
                "local".to_string(),
                "--replace".to_string(),
                "--protocol=openai".to_string(),
                "--base-url=http://broken/v1".to_string(),
            ],
            &mut settings,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("has no models"));
        let active = settings.active_provider().unwrap();
        assert_eq!(active.base_url, "http://127.0.0.1:8046/v1");
        assert_eq!(active.models[0].name, "flash");
    }

    #[test]
    fn agent_mode_args_can_use_active_task_as_default_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeTaskManager::new(temp.path().join(".pwcli"));
        runtime.ensure().unwrap();
        let task = runtime
            .create_task(
                RuntimeTaskKind::Internal,
                "default prompt goal",
                temp.path(),
                serde_json::json!({ "goal": "default prompt goal" }),
            )
            .unwrap();
        runtime.set_active(&task.task_id).unwrap();

        let args =
            agent_mode_args_for_runtime(&runtime, "plan", false, &["--wait".to_string()]).unwrap();
        assert_eq!(args[0], "codex");
        assert!(args.contains(&"--wait".to_string()));
        assert!(args
            .last()
            .is_some_and(|prompt| prompt.contains("default prompt goal")));
    }

    #[test]
    fn resumed_graph_session_saves_back_to_same_session_id() {
        assert_eq!(
            session_id_for_save(Some("session_123".to_string())),
            "session_123"
        );
        assert!(!session_id_for_save(None).is_empty());
    }

    #[test]
    fn memory_candidate_turn_text_uses_only_current_turn() {
        let text = memory_candidate_turn_text("current request", "current answer");
        assert!(text.contains("current request"));
        assert!(text.contains("current answer"));
        assert!(!text.contains("previous request"));
    }

    #[test]
    fn tui_approval_round_trips_request_to_ui_thread() {
        let (event_tx, event_rx) = mpsc::channel();
        let approval = TuiApproval { event_tx };
        let call = ToolCall {
            id: "call_1".to_string(),
            tool_id: "builtin.write".to_string(),
            name: "write".to_string(),
            arguments: serde_json::json!({ "path": "README.md" }),
        };

        let handle = thread::spawn(move || approval.ask_user("Allow write?", &call));
        match event_rx.recv().unwrap() {
            PromptWorkerEvent::AskUser {
                prompt,
                tool_name,
                response_tx,
            } => {
                assert_eq!(prompt, "Allow write?");
                assert_eq!(tool_name, "write");
                response_tx.send(true).unwrap();
            }
            _ => panic!("expected AskUser event"),
        }

        assert!(handle.join().unwrap());
    }

    #[test]
    fn manual_tool_call_uses_policy_before_execution() {
        let temp = tempfile::tempdir().unwrap();
        let settings = pwcli::settings::Settings::from_home(temp.path());
        pwcli::storage::WorkspacePaths::from_pwcli_home(&settings.pwcli_home)
            .ensure()
            .unwrap();
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(LoadedTool {
            descriptor: ToolDescriptor {
                id: "builtin.write".to_string(),
                name: "write".to_string(),
                description: "write file".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                source: ToolSource::Builtin,
                risk_level: RiskLevel::ReadOnly,
                invocation_mode: InvocationMode::Internal,
                capabilities: vec![],
                metadata: serde_json::json!({}),
                enabled: true,
            },
            executor: Some(Arc::new(CountingExecutor(Arc::clone(&executions)))),
        });
        let snapshot = registry.snapshot();
        let call = ToolCall {
            id: "manual-test".to_string(),
            tool_id: "builtin.write".to_string(),
            name: "write".to_string(),
            arguments: serde_json::json!({ "path": "/etc/passwd" }),
        };

        let result = execute_manual_tool_call(&settings, &snapshot, &call).unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("protected path"));
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let audit_text =
            std::fs::read_to_string(settings.pwcli_home.join("audit/events.jsonl")).unwrap();
        assert!(audit_text.contains("ToolCallRequested"));
        assert!(audit_text.contains("PolicyDecisionRecorded"));
        assert!(audit_text.contains("ToolResultRecorded"));
    }

    #[test]
    fn agent_mode_args_require_active_task_when_prompt_is_omitted() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeTaskManager::new(temp.path().join(".pwcli"));
        runtime.ensure().unwrap();
        let err = agent_mode_args_for_runtime(&runtime, "plan", false, &[]).unwrap_err();
        assert!(err.to_string().contains("no active task"));
    }

    #[test]
    fn task_next_recommends_review_when_runtime_marks_risk() {
        let task = RuntimeTask {
            task_id: "task_review".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Completed,
            title: "risky execution".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 1,
            updated_at: 2,
            metadata: serde_json::json!({
                "review_recommendation": {
                    "required": true,
                    "reason": "task used yolo/dangerous permissions"
                }
            }),
        };

        let text = format_task_next(&task);
        assert!(text.contains("review: required"));
        assert!(text.contains("pwcli review --wait"));
    }

    #[test]
    fn task_next_shows_agent_and_session_metadata() {
        let task = RuntimeTask {
            task_id: "task_agent_session".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Running,
            title: "agent work".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 0,
            updated_at: 0,
            metadata: serde_json::json!({
                "agent_cli": "claude",
                "mode": "execute",
                "model": "opus",
                "effort": "high",
                "yolo": true,
                "session": {
                    "native_session_id": "11111111-2222-4333-a444-555555555555",
                    "resume_args": ["--session-id", "11111111-2222-4333-a444-555555555555"],
                    "native_session_supported": true
                }
            }),
        };

        let text = format_task_next(&task);
        assert!(text.contains("agent: claude mode=execute model=opus effort=high yolo=true"));
        assert!(text.contains("session=11111111-2222-4333-a444-555555555555"));
        assert!(text.contains("resume=\"--session-id 11111111-2222-4333-a444-555555555555\""));
    }

    #[test]
    fn task_next_recommends_verify_when_review_is_not_required() {
        let task = RuntimeTask {
            task_id: "task_clean".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Completed,
            title: "clean execution".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 1,
            updated_at: 2,
            metadata: serde_json::json!({
                "review_recommendation": {
                    "required": false,
                    "reason": "completed without obvious risk markers"
                }
            }),
        };

        let text = format_task_next(&task);
        assert!(text.contains("review: not required"));
        assert!(text.contains("pwcli task verify"));
    }

    #[test]
    fn task_next_uses_attached_verification_status() {
        let mut task = RuntimeTask {
            task_id: "task_verified".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Completed,
            title: "verified execution".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 1,
            updated_at: 2,
            metadata: serde_json::json!({
                "review_recommendation": {
                    "required": false,
                    "reason": "completed without obvious risk markers"
                },
                "verification": {
                    "passed": true,
                    "path": "/tmp/verification.md"
                }
            }),
        };

        let text = format_task_next(&task);
        assert!(text.contains("verification: passed"));
        assert!(text.contains("pwcli memory extract task"));

        task.metadata["verification"]["passed"] = serde_json::json!(false);
        let text = format_task_next(&task);
        assert!(text.contains("verification: failed"));
        assert!(text.contains("pwcli task log"));
    }

    #[test]
    fn task_list_marks_review_requirement() {
        let task = RuntimeTask {
            task_id: "task_review".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Completed,
            title: "review me".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 1,
            updated_at: 2,
            metadata: serde_json::json!({
                "review_recommendation": {
                    "required": true,
                    "reason": "task used yolo/dangerous permissions"
                }
            }),
        };

        let text = format_task_list(&[task], Some("task_review"));
        assert!(text.contains("* task_review"));
        assert!(!text.contains("agent="));
        assert!(text.contains("review=required"));
    }

    #[test]
    fn task_list_marks_agent_mode() {
        let task = RuntimeTask {
            task_id: "task_agent".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Running,
            title: "agent mode".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 0,
            updated_at: 0,
            metadata: serde_json::json!({
                "agent_cli": "codex",
                "mode": "plan"
            }),
        };

        let text = format_task_list(&[task], Some("task_agent"));
        assert!(text.contains("agent=codex:plan"));
    }

    #[test]
    fn task_list_marks_verification_status() {
        let task = RuntimeTask {
            task_id: "task_verify".to_string(),
            kind: RuntimeTaskKind::AgentCli,
            status: RuntimeTaskStatus::Completed,
            title: "verified".to_string(),
            cwd: std::path::PathBuf::from("."),
            created_at: 0,
            updated_at: 0,
            metadata: serde_json::json!({
                "verification": {
                    "passed": false
                }
            }),
        };

        let text = format_task_list(&[task], Some("task_verify"));
        assert!(text.contains("verify=failed"));
    }

    #[test]
    fn runtime_status_tailer_reads_only_new_persisted_events() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeTaskManager::new(temp.path().join(".pwcli"));
        runtime.ensure().unwrap();
        let task = runtime
            .create_task(
                RuntimeTaskKind::Internal,
                "tail events",
                temp.path(),
                serde_json::json!({ "goal": "tail events" }),
            )
            .unwrap();
        runtime.set_active(&task.task_id).unwrap();
        let events_path = runtime.task_dir(&task.task_id).join("events.jsonl");
        std::fs::write(
            &events_path,
            format!(
                "{}\n",
                serde_json::to_string(&RuntimeTaskEvent::Started {
                    task_id: task.task_id.clone()
                })
                .unwrap()
            ),
        )
        .unwrap();

        let mut tailer = super::RuntimeEventTailer::default();
        assert!(super::runtime_status_messages(&runtime, &mut tailer).is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&RuntimeTaskEvent::Completed {
                task_id: task.task_id.clone(),
                result: serde_json::json!({})
            })
            .unwrap()
        )
        .unwrap();

        let messages = super::runtime_status_messages(&runtime, &mut tailer);
        assert_eq!(messages, vec![format!("task completed: {}", task.task_id)]);
    }

    #[test]
    fn memory_task_source_text_includes_task_metadata_and_logs() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeTaskManager::new(temp.path().join(".pwcli"));
        runtime.ensure().unwrap();
        let task = runtime
            .create_task(
                RuntimeTaskKind::Internal,
                "memory extraction goal",
                temp.path(),
                serde_json::json!({"goal": "memory extraction goal"}),
            )
            .unwrap();
        let task_dir = runtime.task_dir(&task.task_id);
        std::fs::write(
            task_dir.join("stdout.log"),
            "用户明确要求 memory extract task",
        )
        .unwrap();

        let text = memory_task_source_text(&runtime, &task.task_id).unwrap();
        assert!(text.contains("memory extraction goal"));
        assert!(text.contains("用户明确要求 memory extract task"));
        assert!(text.contains("元数据"));
    }

    #[test]
    fn memory_task_source_text_truncates_large_logs_before_extraction() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeTaskManager::new(temp.path().join(".pwcli"));
        runtime.ensure().unwrap();
        let task = runtime
            .create_task(
                RuntimeTaskKind::Internal,
                "large log extraction",
                temp.path(),
                serde_json::json!({"goal": "large log extraction"}),
            )
            .unwrap();
        let task_dir = runtime.task_dir(&task.task_id);
        std::fs::write(
            task_dir.join("stdout.log"),
            format!("{}TAIL_MARKER", "a".repeat(12 * 1024)),
        )
        .unwrap();

        let text = memory_task_source_text(&runtime, &task.task_id).unwrap();
        assert!(text.contains("[truncated]"));
        assert!(!text.contains("TAIL_MARKER"));
    }

    #[test]
    fn memory_candidate_formatter_shows_full_ontology() {
        let candidate = MemoryCandidate {
            id: "memcand_test".to_string(),
            facts: vec![CandidateFact {
                ref_id: Some("f1".to_string()),
                statement: "用户要求 memory 写入前必须可审计。".to_string(),
                source: "测试来源".to_string(),
                related_facts: Vec::new(),
            }],
            logic_chains: vec![CandidateLogicChain {
                ref_id: Some("l1".to_string()),
                premises: vec!["f1".to_string()],
                explanation: "因为写入前可审计，所以候选必须完整展示。".to_string(),
            }],
            inferences: vec![CandidateInference {
                statement: "memory inbox 是审核边界。".to_string(),
                logic_chain: "l1".to_string(),
            }],
            hypotheses: vec![CandidateHypothesis {
                statement: "用户会偏好完整候选展示。".to_string(),
                supporting_facts: vec!["f1".to_string()],
                confidence: 0.7,
            }],
            reason: "测试完整展示".to_string(),
            source: "单元测试".to_string(),
            created_at: "2026-07-02T00:00:00Z".to_string(),
            review: MemoryCandidateReview::default(),
        };

        let text = format_memory_candidate(&candidate);
        assert!(text.contains("facts:"));
        assert!(text.contains("logic_chains:"));
        assert!(text.contains("inferences:"));
        assert!(text.contains("hypotheses:"));
        assert!(text.contains("review:"));
        assert!(text.contains("0.70"));
    }

    #[test]
    fn rule_names_are_sanitized() {
        assert_eq!(
            normalize_rule_name("Safety Checks.md").unwrap(),
            "safety-checks"
        );
        assert!(normalize_rule_name("../bad").is_ok());
        assert!(normalize_rule_name("!!!").is_err());
    }

    #[test]
    fn load_rule_texts_reads_rule_files() {
        let temp = tempfile::tempdir().unwrap();
        let settings = pwcli::settings::Settings::from_home(temp.path());
        std::fs::create_dir_all(settings.pwcli_home.join("rules")).unwrap();
        std::fs::write(
            settings.pwcli_home.join("rules/safety.md"),
            "必须在删除文件前询问用户确认",
        )
        .unwrap();

        let rules = load_rule_texts(&settings);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].contains("删除文件前询问"));
    }
}
