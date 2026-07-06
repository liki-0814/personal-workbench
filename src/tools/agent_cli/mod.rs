use crate::{
    runtime::{RuntimeTaskKind, RuntimeTaskSpec},
    tools::{
        ToolCall, ToolExecutionMode, ToolExecutionRuntime, ToolExecutor, ToolResult,
        ToolRuntimeEvent,
    },
    PwError, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCliKind {
    Codex,
    Claude,
    Agy,
    QoderCli,
}

impl AgentCliKind {
    pub const fn all() -> &'static [Self] {
        &[Self::Codex, Self::Claude, Self::Agy, Self::QoderCli]
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "agy" => Some(Self::Agy),
            "qodercli" => Some(Self::QoderCli),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Agy => "agy",
            Self::QoderCli => "qodercli",
        }
    }

    pub fn binary(self) -> &'static str {
        self.id()
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::Claude => "Claude Code",
            Self::Agy => "Agy",
            Self::QoderCli => "Qoder CLI",
        }
    }

    pub fn best_model_hint(self) -> &'static str {
        match self {
            Self::Codex => "Use the configured default unless caller passes model; prefer the most capable available Codex model.",
            Self::Claude => "Use opus/fable class when available for hard work; sonnet class is acceptable for routine edits.",
            Self::Agy => "Use the configured default unless caller passes model.",
            Self::QoderCli => "Use the configured default or newest advanced coding model unless caller passes model.",
        }
    }

    pub fn usage_hints(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &[
                "codex exec <prompt> runs non-interactively.",
                "codex exec --json emits event JSONL for richer future integration.",
                "codex exec --sandbox workspace-write is the normal safe default.",
                "codex exec --dangerously-bypass-approvals-and-sandbox is only for externally sandboxed YOLO execution.",
                "codex review can be used for code review workflows.",
            ],
            Self::Claude => &[
                "claude -p <prompt> prints a non-interactive result.",
                "claude --permission-mode plan is useful for planning without edits.",
                "claude --permission-mode bypassPermissions or --dangerously-skip-permissions is only for YOLO execution.",
                "claude --effort high/xhigh/max controls reasoning effort.",
                "claude --output-format stream-json can support richer future progress parsing.",
            ],
            Self::Agy => &[
                "agy --print <prompt> runs a single non-interactive prompt.",
                "agy --print-timeout controls wait time.",
                "agy --dangerously-skip-permissions is only for YOLO execution.",
                "agy --prompt-interactive starts interactively when needed outside this tool.",
            ],
            Self::QoderCli => &[
                "qodercli -p <prompt> prints non-interactive output.",
                "qodercli --permission-mode auto/default is safer; bypass_permissions is YOLO.",
                "qodercli --reasoning-effort high is a good default for complex coding.",
                "qodercli -w <dir> changes working directory.",
                "qodercli --agent and --agents expose native sub-agent capabilities.",
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentCliExecutor {
    kind: AgentCliKind,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentCliArgs {
    pub prompt: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_effort")]
    pub effort: String,
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub background: bool,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl AgentCliExecutor {
    pub fn new(kind: AgentCliKind) -> Self {
        Self { kind }
    }
}

impl ToolExecutor for AgentCliExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut runtime = ToolExecutionRuntime::noop();
        self.execute_with_runtime(call, &mut runtime)
    }

    fn execute_with_runtime(
        &self,
        call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> Result<ToolResult> {
        let args: AgentCliArgs = serde_json::from_value(call.arguments.clone())?;
        if args.prompt.trim().is_empty() {
            return Ok(ToolResult::error("agent_cli prompt is required"));
        }

        if args.background {
            if let Some(manager) = runtime.runtime_tasks().cloned() {
                runtime.emit(ToolRuntimeEvent::Started {
                    mode: ToolExecutionMode::Background,
                });
                let spec = build_runtime_task_spec(self.kind, None, args, None);
                let handle = manager.spawn(spec)?;
                runtime.emit(ToolRuntimeEvent::BackgroundTaskStarted {
                    task_id: handle.task_id.clone(),
                    task_dir: handle.task_dir.clone(),
                });
                runtime.emit(ToolRuntimeEvent::Completed { is_error: false });
                let mut result = ToolResult::ok(format!(
                    "{} started in background as task {}",
                    self.kind.display_name(),
                    handle.task_id
                ));
                result.metadata = json!({
                    "agent_cli": self.kind.id(),
                    "execution_mode": "background",
                    "task_id": handle.task_id,
                    "task_dir": handle.task_dir,
                    "resumable": true
                });
                result.audit_hints = json!({
                    "background_task": true,
                    "requires_completion_callback": true
                });
                return Ok(result);
            }
            runtime.emit(ToolRuntimeEvent::Progress {
                message: "background requested but no RuntimeTaskManager is available; falling back to streaming foreground execution".to_string(),
            });
        }

        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Streaming,
        });
        let command = build_command(self.kind, &args);
        let mut process = Command::new(self.kind.binary());
        process.args(&command.args);
        if let Some(cwd) = &args.cwd {
            process.current_dir(cwd);
        }
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let mut child = process.spawn().map_err(|err| {
            PwError::ToolExecution(format!(
                "failed to start {}: {err}. Is it installed and on PATH?",
                self.kind.binary()
            ))
        })?;
        runtime.emit(ToolRuntimeEvent::Progress {
            message: format!("started {}", self.kind.display_name()),
        });

        let (tx, rx) = mpsc::channel::<(String, String)>();
        if let Some(stdout) = child.stdout.take() {
            spawn_stream_reader("stdout", stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_stream_reader("stderr", stderr, tx.clone());
        }
        drop(tx);

        let timeout = Duration::from_secs(args.timeout_seconds.max(1));
        let mut stdout = String::new();
        let mut stderr = String::new();
        loop {
            drain_output_events(runtime, &rx, &mut stdout, &mut stderr);
            if runtime.cancellation().is_cancelled() {
                runtime.emit(ToolRuntimeEvent::CancelRequested);
                let _ = child.kill();
                let _ = child.wait();
                runtime.emit(ToolRuntimeEvent::Cancelled);
                let mut result =
                    ToolResult::error(format!("{} cancelled", self.kind.display_name()));
                result.metadata = agent_cli_metadata(
                    self.kind,
                    &command,
                    &args,
                    None,
                    started.elapsed().as_millis() as u64,
                    "cancelled",
                );
                return Ok(result);
            }

            if let Some(status) = child.try_wait()? {
                drain_output_events(runtime, &rx, &mut stdout, &mut stderr);
                let _ = child.wait();
                let mut result = if status.success() {
                    ToolResult::ok(stdout.clone())
                } else {
                    ToolResult::error(if stderr.trim().is_empty() {
                        stdout.clone()
                    } else {
                        stderr.clone()
                    })
                };
                result.metadata = agent_cli_metadata(
                    self.kind,
                    &command,
                    &args,
                    status.code(),
                    started.elapsed().as_millis() as u64,
                    "streaming",
                );
                result.audit_hints = json!({
                    "requires_completion_callback": true,
                    "review_recommended_for_complex_changes": true
                });
                runtime.emit(ToolRuntimeEvent::Completed {
                    is_error: result.is_error,
                });
                return Ok(result);
            }

            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                runtime.emit(ToolRuntimeEvent::TimedOut {
                    timeout_seconds: args.timeout_seconds,
                });
                let mut result = ToolResult::error(format!(
                    "{} timed out after {}s",
                    self.kind.display_name(),
                    args.timeout_seconds
                ));
                result.metadata = agent_cli_metadata(
                    self.kind,
                    &command,
                    &args,
                    None,
                    started.elapsed().as_millis() as u64,
                    "timed_out",
                );
                runtime.emit(ToolRuntimeEvent::Completed { is_error: true });
                return Ok(result);
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn agent_cli_metadata(
    kind: AgentCliKind,
    command: &BuiltCommand,
    args: &AgentCliArgs,
    status_code: Option<i32>,
    duration_ms: u64,
    execution_mode: &str,
) -> Value {
    json!({
        "agent_cli": kind.id(),
        "command": command.audit_command,
        "mode": args.mode,
        "cwd": args.cwd,
        "model": args.model,
        "effort": args.effort,
        "yolo": args.yolo,
        "background": args.background,
        "execution_mode": execution_mode,
        "status_code": status_code,
        "duration_ms": duration_ms
    })
}

fn spawn_stream_reader(
    stream: &'static str,
    reader: impl Read + Send + 'static,
    tx: mpsc::Sender<(String, String)>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = tx.send((stream.to_string(), line));
                }
                Err(_) => break,
            }
        }
    });
}

fn drain_output_events(
    runtime: &mut ToolExecutionRuntime<'_>,
    rx: &mpsc::Receiver<(String, String)>,
    stdout: &mut String,
    stderr: &mut String,
) {
    while let Ok((stream, chunk)) = rx.try_recv() {
        if stream == "stdout" {
            stdout.push_str(&chunk);
        } else {
            stderr.push_str(&chunk);
        }
        runtime.emit(ToolRuntimeEvent::Output { stream, chunk });
    }
}

#[derive(Debug, Clone)]
pub struct BuiltCommand {
    pub args: Vec<String>,
    pub audit_command: Vec<String>,
}

pub fn build_command(kind: AgentCliKind, args: &AgentCliArgs) -> BuiltCommand {
    let prompt = mode_prompt(&args.mode, &args.prompt);
    let mut command = match kind {
        AgentCliKind::Codex => codex_args(args, &prompt),
        AgentCliKind::Claude => claude_args(args, &prompt),
        AgentCliKind::Agy => agy_args(args, &prompt),
        AgentCliKind::QoderCli => qoder_args(args, &prompt),
    };
    let mut audit_command = vec![kind.binary().to_string()];
    audit_command.extend(command.clone());
    BuiltCommand {
        args: std::mem::take(&mut command),
        audit_command,
    }
}

pub fn build_runtime_task_spec(
    kind: AgentCliKind,
    task_id: Option<String>,
    mut args: AgentCliArgs,
    prior_summary: Option<String>,
) -> RuntimeTaskSpec {
    let stable_session_id = task_id
        .as_deref()
        .map(|task_id| stable_session_id(kind, task_id));
    if args.session_id.is_none() && kind != AgentCliKind::Codex {
        args.session_id = stable_session_id.clone();
    }
    if let Some(summary) = prior_summary.filter(|summary| !summary.trim().is_empty()) {
        args.prompt = format!(
            "Continue the same pwcli task using this compacted prior context:\n\n{}\n\nNew request:\n{}",
            summary, args.prompt
        );
    }
    let command = build_command(kind, &args);
    let mut full_command = vec![kind.binary().to_string()];
    full_command.extend(command.args);
    let cwd = args
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    RuntimeTaskSpec {
        task_id,
        kind: RuntimeTaskKind::AgentCli,
        title: format!("{} {}", kind.display_name(), args.mode),
        cwd,
        command: full_command,
        timeout_seconds: args.timeout_seconds,
        auto_compact_threshold_chars: Some(64_000),
        metadata: json!({
            "agent_cli": kind.id(),
            "mode": args.mode,
            "model": args.model,
            "effort": args.effort,
            "yolo": args.yolo,
            "session": {
                "task_id_scoped": true,
                "cli": kind.id(),
                "resume_args": resume_args_for_metadata(kind, args.session_id.as_deref()),
                "native_session_id": args.session_id,
                "stable_session_id": stable_session_id,
                "native_session_supported": kind != AgentCliKind::Codex
            },
            "audit_command": command.audit_command
        }),
    }
}

fn codex_args(args: &AgentCliArgs, prompt: &str) -> Vec<String> {
    let mut out = vec!["exec".to_string(), "--sandbox".to_string()];
    out.push(if args.yolo {
        "danger-full-access".to_string()
    } else {
        "workspace-write".to_string()
    });
    if args.yolo {
        out.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    }
    if !args
        .extra_args
        .iter()
        .any(|arg| arg == "--skip-git-repo-check")
    {
        out.push("--skip-git-repo-check".to_string());
    }
    if let Some(cwd) = &args.cwd {
        out.push("--cd".to_string());
        out.push(cwd.display().to_string());
    }
    if let Some(model) = &args.model {
        out.push("--model".to_string());
        out.push(model.clone());
    }
    if let Some(effort) = codex_reasoning_effort(&args.effort) {
        if !args
            .extra_args
            .iter()
            .any(|arg| arg.contains("model_reasoning_effort"))
        {
            out.push("-c".to_string());
            out.push(format!("model_reasoning_effort=\"{effort}\""));
        }
    }
    if !args.extra_args.iter().any(|arg| arg == "--json") {
        out.push("--json".to_string());
    }
    out.extend(args.extra_args.clone());
    out.push(prompt.to_string());
    out
}

fn codex_reasoning_effort(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("xhigh"),
        _ => None,
    }
}

fn claude_args(args: &AgentCliArgs, prompt: &str) -> Vec<String> {
    let mut out = vec![
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
    ];
    if let Some(model) = &args.model {
        out.push("--model".to_string());
        out.push(model.clone());
    }
    if let Some(session_id) = &args.session_id {
        out.push("--session-id".to_string());
        out.push(session_id.clone());
    }
    if !args.effort.trim().is_empty() {
        out.push("--effort".to_string());
        out.push(args.effort.clone());
    }
    if args.yolo {
        out.push("--dangerously-skip-permissions".to_string());
    }
    out.extend(args.extra_args.clone());
    out.push(prompt.to_string());
    out
}

fn agy_args(args: &AgentCliArgs, prompt: &str) -> Vec<String> {
    let mut out = vec!["--print".to_string(), "--print-timeout".to_string()];
    out.push(format!("{}s", args.timeout_seconds.max(1)));
    if let Some(model) = &args.model {
        out.push("--model".to_string());
        out.push(model.clone());
    }
    if let Some(session_id) = &args.session_id {
        out.push("--conversation".to_string());
        out.push(session_id.clone());
    }
    if args.yolo {
        out.push("--dangerously-skip-permissions".to_string());
    }
    out.extend(args.extra_args.clone());
    out.push(prompt.to_string());
    out
}

fn qoder_args(args: &AgentCliArgs, prompt: &str) -> Vec<String> {
    let mut out = vec!["--print".to_string()];
    if let Some(session_id) = &args.session_id {
        out.push("--session-id".to_string());
        out.push(session_id.clone());
    }
    if let Some(cwd) = &args.cwd {
        out.push("--cwd".to_string());
        out.push(cwd.display().to_string());
    }
    if let Some(model) = &args.model {
        out.push("--model".to_string());
        out.push(model.clone());
    }
    if !args.effort.trim().is_empty() {
        out.push("--reasoning-effort".to_string());
        out.push(args.effort.clone());
    }
    if args.yolo {
        out.push("--permission-mode".to_string());
        out.push("bypass_permissions".to_string());
        out.push("--dangerously-skip-permissions".to_string());
    }
    out.extend(args.extra_args.clone());
    out.push(prompt.to_string());
    out
}

fn mode_prompt(mode: &str, prompt: &str) -> String {
    let guidance = match mode {
        "goal" => {
            "Clarify and lock the goal first. Do not edit files unless the user explicitly asks."
        }
        "plan" => {
            "Create a concrete implementation plan with risks and validation. Do not modify files."
        }
        "execute" => {
            "Execute the requested coding task. Keep changes scoped and run relevant validation."
        }
        "review" => {
            "Review the current work for bugs, regressions, missing tests, and risky assumptions."
        }
        _ => "Handle the task directly using your normal CLI workflow.",
    };
    format!("{guidance}\n\nTask:\n{prompt}")
}

fn default_mode() -> String {
    "direct".to_string()
}

fn default_effort() -> String {
    "high".to_string()
}

fn default_timeout_seconds() -> u64 {
    900
}

fn resume_args_for_metadata(kind: AgentCliKind, session_id: Option<&str>) -> Vec<String> {
    let Some(session_id) = session_id else {
        return Vec::new();
    };
    match kind {
        AgentCliKind::Codex => Vec::new(),
        AgentCliKind::Claude => vec!["--session-id".to_string(), session_id.to_string()],
        AgentCliKind::Agy => vec!["--conversation".to_string(), session_id.to_string()],
        AgentCliKind::QoderCli => vec!["--session-id".to_string(), session_id.to_string()],
    }
}

fn stable_session_id(kind: AgentCliKind, task_id: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("{}:{task_id}", kind.id()).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let a = hash;
    let b = hash.rotate_left(17) ^ 0xa0761d6478bd642f;
    let c = hash.rotate_left(31) ^ 0xe7037ed1a0b428db;
    let d = hash.rotate_left(47) ^ 0x8ebc6af09c88c6e3;
    let raw = format!("{a:016x}{b:016x}{c:016x}{d:016x}");
    format!(
        "{}-{}-4{}-a{}-{}",
        &raw[0..8],
        &raw[8..12],
        &raw[13..16],
        &raw[17..20],
        &raw[20..32]
    )
}

pub fn agent_cli_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "Task prompt to delegate to the local coding agent CLI."
            },
            "mode": {
                "type": "string",
                "enum": ["direct", "goal", "plan", "execute", "review"],
                "description": "Intent hint. This is not a fixed workflow; it only shapes the delegated prompt."
            },
            "cwd": {
                "type": "string",
                "description": "Working directory for the sub-agent. Defaults to current pwcli process directory."
            },
            "model": {
                "type": "string",
                "description": "Optional model name/alias for the underlying CLI. Omit to use that CLI's configured best/default model."
            },
            "session_id": {
                "type": "string",
                "description": "Optional native session/conversation id. Normally pwcli derives one per task when supported."
            },
            "effort": {
                "type": "string",
                "description": "Reasoning effort when supported. Defaults to high."
            },
            "yolo": {
                "type": "boolean",
                "description": "Allow dangerous no-approval execution for externally sandboxed runs only."
            },
            "background": {
                "type": "boolean",
                "description": "Start the agent CLI as a pwcli background runtime task when available. Returns a task id immediately."
            },
            "timeout_seconds": {
                "type": "integer",
                "description": "Maximum wait time. Defaults to 900."
            },
            "extra_args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Raw CLI args appended before the prompt for expert escape hatches."
            }
        },
        "required": ["prompt"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(prompt: &str) -> AgentCliArgs {
        AgentCliArgs {
            prompt: prompt.to_string(),
            mode: "plan".to_string(),
            cwd: Some(PathBuf::from("/tmp/work")),
            model: Some("best-model".to_string()),
            session_id: Some("11111111-2222-4333-a444-555555555555".to_string()),
            effort: "high".to_string(),
            yolo: true,
            background: false,
            timeout_seconds: 60,
            extra_args: vec!["--flag".to_string()],
        }
    }

    #[test]
    fn codex_command_uses_exec_and_yolo_flags() {
        let built = build_command(AgentCliKind::Codex, &args("do it"));
        assert!(built.args.starts_with(&[
            "exec".to_string(),
            "--sandbox".to_string(),
            "danger-full-access".to_string()
        ]));
        assert!(built
            .args
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(built.args.contains(&"--json".to_string()));
        assert!(built.args.contains(&"-c".to_string()));
        assert!(built
            .args
            .contains(&"model_reasoning_effort=\"high\"".to_string()));
        assert!(built
            .args
            .last()
            .unwrap()
            .contains("Create a concrete implementation plan"));
    }

    #[test]
    fn codex_command_maps_max_effort_to_xhigh_config() {
        let mut args = args("do it");
        args.effort = "max".to_string();
        let built = build_command(AgentCliKind::Codex, &args);
        assert!(built
            .args
            .contains(&"model_reasoning_effort=\"xhigh\"".to_string()));
    }

    #[test]
    fn claude_command_uses_print_and_effort() {
        let built = build_command(AgentCliKind::Claude, &args("review"));
        assert!(built.args.contains(&"--print".to_string()));
        assert!(built.args.contains(&"--effort".to_string()));
        assert!(built
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn qoder_command_maps_cwd_and_reasoning_effort() {
        let built = build_command(AgentCliKind::QoderCli, &args("execute"));
        assert!(built.args.contains(&"--cwd".to_string()));
        assert!(built.args.contains(&"--reasoning-effort".to_string()));
        assert!(built.args.contains(&"bypass_permissions".to_string()));
    }

    #[test]
    fn runtime_spec_derives_native_session_for_supported_clis() {
        let spec = build_runtime_task_spec(
            AgentCliKind::Claude,
            Some("task_abc".to_string()),
            args("x"),
            None,
        );
        assert_eq!(
            spec.metadata["session"]["native_session_supported"],
            serde_json::json!(true)
        );
        assert!(spec.command.contains(&"--session-id".to_string()));
    }
}
