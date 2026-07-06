use crate::{PwError, Result};

mod report;

pub use report::{
    build_skipped_verification_report, build_verification_report, legacy_verification_report,
    render_verification_report_markdown, verification_report_from_metadata, VerificationCheck,
    VerificationEvidence, VerificationGate, VerificationGateDecision, VerificationNextAction,
    VerificationReport, VerificationSeverity, VerificationStatus, VerificationSuiteReport,
};

use super::{
    InvocationMode, LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolExecutor, ToolLoader,
    ToolResult, ToolSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const TOOL_ID: &str = "verification.project_check";
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_MAX_OUTPUT_CHARS: usize = 12_000;

pub struct VerificationToolLoader;

impl ToolLoader for VerificationToolLoader {
    fn load(&self) -> Result<Vec<LoadedTool>> {
        Ok(vec![project_check_tool()])
    }
}

fn project_check_tool() -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: TOOL_ID.to_string(),
            name: "project_check".to_string(),
            description: "Run deterministic local project verification commands such as cargo check/test, npm lint/typecheck/test, or explicitly supplied shell commands.".to_string(),
            input_schema: project_check_schema(),
            source: ToolSource::Verification,
            risk_level: RiskLevel::Medium,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "verification".to_string(),
                "test".to_string(),
                "lint".to_string(),
                "typecheck".to_string(),
                "deterministic_check".to_string(),
            ],
            metadata: json!({
                "runs_local_commands": true,
                "policy_gate_required": true,
                "auto_detects": ["rust", "node", "python"],
                "manual_commands_supported": true
            }),
            enabled: true,
        },
        executor: Some(Arc::new(ProjectCheckExecutor)),
    }
}

pub fn project_check_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cwd": {
                "type": "string",
                "description": "Working directory. Defaults to the current process directory."
            },
            "commands": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional shell commands to run. If omitted, pwcli auto-detects project checks."
            },
            "timeout_seconds": {
                "type": "integer",
                "minimum": 1,
                "description": "Per-command timeout in seconds. Defaults to 300."
            },
            "max_output_chars": {
                "type": "integer",
                "minimum": 100,
                "description": "Maximum stdout/stderr characters kept per command. Defaults to 12000."
            }
        }
    })
}

#[derive(Debug, Deserialize)]
struct ProjectCheckArgs {
    cwd: Option<PathBuf>,
    #[serde(default)]
    commands: Vec<String>,
    timeout_seconds: Option<u64>,
    max_output_chars: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ProjectCheckExecutor;

impl ToolExecutor for ProjectCheckExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let args = serde_json::from_value::<ProjectCheckArgs>(call.arguments.clone())?;
        let cwd = match args.cwd {
            Some(path) => path,
            None => std::env::current_dir()?,
        };
        if !cwd.is_dir() {
            return Ok(ToolResult::error(format!(
                "Verification failed: cwd is not a directory: {}",
                cwd.display()
            )));
        }

        let manual_commands = !args.commands.is_empty();
        let commands = normalize_commands(if manual_commands {
            args.commands
        } else {
            auto_detect_commands(&cwd)
        });
        if commands.is_empty() {
            let report = build_skipped_verification_report(
                TOOL_ID,
                cwd.display().to_string(),
                format!(
                    "No verification commands detected in {}. Pass explicit commands, for example: {{\"commands\":[\"cargo check\"]}}",
                    cwd.display()
                ),
            );
            return Ok(tool_result_from_report(report, Vec::new()));
        }

        let timeout = Duration::from_secs(args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS));
        let max_output_chars = args.max_output_chars.unwrap_or(DEFAULT_MAX_OUTPUT_CHARS);
        let mut reports = Vec::with_capacity(commands.len());
        for command in commands {
            reports.push(run_shell_command(
                &command,
                command_suite(&command, manual_commands),
                &cwd,
                timeout,
                max_output_chars,
            )?);
        }

        let checks = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| command_report_to_check(idx, &cwd, report))
            .collect::<Vec<_>>();
        let report = build_verification_report(
            TOOL_ID,
            cwd.display().to_string(),
            checks,
            json!({
                "manual_commands": manual_commands,
                "timeout_seconds": timeout.as_secs(),
            }),
        );
        Ok(tool_result_from_report(report, reports))
    }
}

fn normalize_commands(commands: Vec<String>) -> Vec<String> {
    commands
        .into_iter()
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty())
        .collect()
}

fn auto_detect_commands(cwd: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if cwd.join("Cargo.toml").is_file() {
        commands.push("cargo fmt --check".to_string());
        commands.push("cargo check".to_string());
        commands.push("cargo test".to_string());
    }
    if cwd.join("package.json").is_file() {
        commands.push("npm run lint --if-present".to_string());
        commands.push("npm run typecheck --if-present".to_string());
        commands.push("npm run test --if-present".to_string());
    }
    if cwd.join("pyproject.toml").is_file() || cwd.join("setup.py").is_file() {
        commands.push("python -m compileall .".to_string());
        if cwd.join("tests").is_dir() {
            commands.push("python -m pytest".to_string());
        }
    }
    commands
}

#[derive(Debug, Clone, Serialize)]
struct CommandReport {
    suite: String,
    command: String,
    exit_code: Option<i32>,
    duration_ms: u128,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

fn run_shell_command(
    command: &str,
    suite: String,
    cwd: &Path,
    timeout: Duration,
    max_output_chars: usize,
) -> Result<CommandReport> {
    let start = Instant::now();
    let mut child = shell_command(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            PwError::ToolExecution(format!(
                "failed to start verification command `{command}`: {err}"
            ))
        })?;

    let mut timed_out = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let output = child.wait_with_output()?;
    Ok(CommandReport {
        suite,
        command: command.to_string(),
        exit_code: output.status.code(),
        duration_ms: start.elapsed().as_millis(),
        timed_out,
        stdout: truncate_text(&String::from_utf8_lossy(&output.stdout), max_output_chars),
        stderr: truncate_text(&String::from_utf8_lossy(&output.stderr), max_output_chars),
    })
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.args(["/C", command]);
    shell
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("/bin/sh");
    shell.args(["-lc", command]);
    shell
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(16);
    let mut out = text.chars().take(keep).collect::<String>();
    out.push_str("\n...[truncated]");
    out
}

fn command_suite(command: &str, manual_commands: bool) -> String {
    if manual_commands {
        return "custom".to_string();
    }
    let command = command.trim();
    if command.starts_with("cargo ") {
        "rust".to_string()
    } else if command.starts_with("npm ")
        || command.starts_with("pnpm ")
        || command.starts_with("yarn ")
    {
        "node".to_string()
    } else if command.starts_with("python ")
        || command.starts_with("python3 ")
        || command.starts_with("pytest")
    {
        "python".to_string()
    } else {
        "custom".to_string()
    }
}

fn command_report_to_check(idx: usize, cwd: &Path, report: &CommandReport) -> VerificationCheck {
    let passed = !report.timed_out && report.exit_code == Some(0);
    let status = if passed {
        VerificationStatus::Passed
    } else {
        VerificationStatus::Failed
    };
    let mut evidence = Vec::new();
    if !report.stdout.trim().is_empty() {
        evidence.push(VerificationEvidence {
            label: "stdout".to_string(),
            content: report.stdout.clone(),
        });
    }
    if !report.stderr.trim().is_empty() {
        evidence.push(VerificationEvidence {
            label: "stderr".to_string(),
            content: report.stderr.clone(),
        });
    }
    let summary = if report.timed_out {
        format!("command timed out after {}ms", report.duration_ms)
    } else if passed {
        "command exited successfully".to_string()
    } else {
        format!(
            "command exited with code {}",
            report
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    };
    VerificationCheck {
        id: report::stable_id(
            "check",
            &format!("{}:{}:{}", report.suite, idx, report.command),
        ),
        suite: report.suite.clone(),
        name: report.command.clone(),
        status,
        severity: if passed {
            VerificationSeverity::Info
        } else if report.timed_out {
            VerificationSeverity::High
        } else {
            VerificationSeverity::Critical
        },
        command: Some(report.command.clone()),
        cwd: Some(cwd.display().to_string()),
        exit_code: report.exit_code,
        duration_ms: report.duration_ms,
        timed_out: report.timed_out,
        summary,
        evidence,
        metadata: json!({}),
    }
}

fn tool_result_from_report(
    report: VerificationReport,
    command_reports: Vec<CommandReport>,
) -> ToolResult {
    let passed = report.passed();
    let content = render_verification_report_markdown(&report);
    let mut result = if passed {
        ToolResult::ok(content)
    } else {
        ToolResult::error(content)
    };
    result.metadata = json!({
        "passed": passed,
        "status": report.status,
        "gate": report.gate.decision,
        "summary": report.summary.clone(),
        "cwd": report.cwd.clone(),
        "commands": command_reports,
        "report": report,
    });
    result.audit_hints = json!({
        "verification_tool": TOOL_ID,
        "passed": passed,
        "gate": result.metadata.get("gate").cloned().unwrap_or(Value::Null),
        "status": result.metadata.get("status").cloned().unwrap_or(Value::Null),
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loader_registers_project_check() {
        let tools = VerificationToolLoader.load().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].descriptor.id, TOOL_ID);
        assert_eq!(tools[0].descriptor.source, ToolSource::Verification);
    }

    #[test]
    fn project_check_runs_explicit_commands() {
        let dir = tempdir().unwrap();
        let call = ToolCall {
            id: "test".to_string(),
            tool_id: TOOL_ID.to_string(),
            name: "project_check".to_string(),
            arguments: json!({
                "cwd": dir.path(),
                "commands": ["echo VERIFY_OK"],
                "timeout_seconds": 5,
            }),
        };
        let result = ProjectCheckExecutor.execute(&call).unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("VERIFY_OK"));
        assert_eq!(result.metadata["passed"], true);
        assert_eq!(result.metadata["gate"], "pass");
        assert_eq!(result.metadata["status"], "passed");
        let report = verification_report_from_metadata(&result.metadata).unwrap();
        assert_eq!(report.gate.decision, VerificationGateDecision::Pass);
        assert_eq!(report.status, VerificationStatus::Passed);
        assert_eq!(report.checks.len(), 1);
    }

    #[test]
    fn project_check_reports_command_failure() {
        let dir = tempdir().unwrap();
        let call = ToolCall {
            id: "test".to_string(),
            tool_id: TOOL_ID.to_string(),
            name: "project_check".to_string(),
            arguments: json!({
                "cwd": dir.path(),
                "commands": ["exit 7"],
                "timeout_seconds": 5,
            }),
        };
        let result = ProjectCheckExecutor.execute(&call).unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("exit_code: 7"));
        assert_eq!(result.metadata["passed"], false);
        assert_eq!(result.metadata["gate"], "block");
        let report = verification_report_from_metadata(&result.metadata).unwrap();
        assert_eq!(report.gate.decision, VerificationGateDecision::Block);
        assert_eq!(report.failed_check_count(), 1);
    }

    #[test]
    fn project_check_returns_skipped_report_when_no_commands_are_available() {
        let dir = tempdir().unwrap();
        let call = ToolCall {
            id: "test".to_string(),
            tool_id: TOOL_ID.to_string(),
            name: "project_check".to_string(),
            arguments: json!({
                "cwd": dir.path(),
            }),
        };
        let result = ProjectCheckExecutor.execute(&call).unwrap();
        assert!(result.is_error);
        assert_eq!(result.metadata["passed"], false);
        assert_eq!(result.metadata["status"], "skipped");
        assert_eq!(result.metadata["gate"], "needs_review");
        assert!(result.content.contains("No verification commands detected"));
        let report = verification_report_from_metadata(&result.metadata).unwrap();
        assert_eq!(report.status, VerificationStatus::Skipped);
        assert_eq!(report.gate.decision, VerificationGateDecision::NeedsReview);
    }
}
