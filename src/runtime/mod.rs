use crate::{
    audit::AuditEvent,
    storage::{append_jsonl, write_json},
    tools::verification::{
        legacy_verification_report, render_verification_report_markdown,
        verification_report_from_metadata, VerificationReport,
    },
    PwError, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeTaskKind {
    AgentCli,
    Shell,
    Verification,
    Workflow,
    Model,
    Mcp,
    Skill,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTask {
    pub task_id: String,
    pub kind: RuntimeTaskKind,
    pub status: RuntimeTaskStatus,
    pub title: String,
    pub cwd: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub metadata: Value,
}

pub fn format_task_next(task: &RuntimeTask) -> String {
    let mut out = format!(
        "task: {}\nstatus: {:?}\ntitle: {}\n",
        task.task_id, task.status, task.title
    );

    if let Some(step) = task
        .metadata
        .pointer("/current_step/title")
        .and_then(Value::as_str)
        .filter(|step| !step.trim().is_empty())
    {
        out.push_str(&format!("current_step: {step}\n"));
    }

    if let Some(line) = agent_session_summary_line(task) {
        out.push_str(&line);
        out.push('\n');
    }

    if let Some(line) = verification_summary_line(task) {
        out.push_str(&line);
        out.push('\n');
    }

    if let Some(line) = review_recommendation_line(task) {
        out.push_str(&line);
        out.push('\n');
    }

    let next = match task.status {
        RuntimeTaskStatus::Pending => {
            "next: pwcli plan --wait\nreason: task has been created but no agent work has started"
        }
        RuntimeTaskStatus::Running => {
            "next: pwcli task status or pwcli task log\nreason: task is still running"
        }
        RuntimeTaskStatus::Completed => {
            if let Some(gate) = verification_gate(task) {
                match gate.as_str() {
                    "pass" => "next: pwcli memory extract task\nreason: verification gate passed",
                    "block" => "next: pwcli task log, then pwcli loop --wait\nreason: verification gate blocked progress",
                    "needs_review" => {
                        "next: pwcli review --wait\nreason: verification gate needs review"
                    }
                    _ => "next: pwcli task verify\nreason: verification gate is unknown",
                }
            } else if review_required(task).unwrap_or(false) {
                "next: pwcli review --wait\nreason: completed task has review risk markers"
            } else if verification_passed(task) == Some(true) {
                "next: pwcli memory extract task\nreason: task completed and verification passed"
            } else if verification_passed(task) == Some(false) {
                "next: pwcli task log, then pwcli loop --wait\nreason: attached verification failed"
            } else {
                "next: pwcli task verify\nreason: completed task has no obvious review risk markers"
            }
        }
        RuntimeTaskStatus::Failed => {
            "next: pwcli task log, then pwcli review --wait or pwcli loop --wait\nreason: task failed"
        }
        RuntimeTaskStatus::TimedOut => {
            "next: pwcli task compact, then pwcli loop --wait\nreason: task timed out and may need context compaction"
        }
        RuntimeTaskStatus::Cancelled => {
            "next: pwcli task log or pwcli loop --wait\nreason: task was cancelled before completion"
        }
    };
    out.push_str(next);
    out
}

fn agent_session_summary_line(task: &RuntimeTask) -> Option<String> {
    let agent = task.metadata.get("agent_cli").and_then(Value::as_str)?;
    let mode = task
        .metadata
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("direct");
    let effort = task.metadata.get("effort").and_then(Value::as_str);
    let model = task
        .metadata
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty());
    let yolo = task
        .metadata
        .get("yolo")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut line = format!("agent: {agent} mode={mode}");
    if let Some(model) = model {
        line.push_str(&format!(" model={model}"));
    }
    if let Some(effort) = effort.filter(|effort| !effort.trim().is_empty()) {
        line.push_str(&format!(" effort={effort}"));
    }
    if yolo {
        line.push_str(" yolo=true");
    }

    let Some(session) = task.metadata.get("session") else {
        return Some(line);
    };
    let native_session_id = session
        .get("native_session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let stable_session_id = session
        .get("stable_session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let native_session_supported = session
        .get("native_session_supported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let resume_args = session
        .get("resume_args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|value| !value.trim().is_empty());

    if let Some(native_session_id) = native_session_id {
        line.push_str(&format!(" session={native_session_id}"));
    } else if let Some(stable_session_id) = stable_session_id {
        line.push_str(&format!(" stable_session={stable_session_id}"));
    } else if native_session_supported {
        line.push_str(" session=supported");
    }
    if let Some(resume_args) = resume_args {
        line.push_str(&format!(" resume=\"{resume_args}\""));
    }
    Some(line)
}

pub fn review_recommendation_line(task: &RuntimeTask) -> Option<String> {
    let required = review_required(task)?;
    let reason = task
        .metadata
        .pointer("/review_recommendation/reason")
        .and_then(Value::as_str)
        .unwrap_or("no reason recorded");
    let label = if required {
        "review: required"
    } else {
        "review: not required"
    };
    Some(format!("{label}\nreview_reason: {reason}"))
}

pub fn review_required(task: &RuntimeTask) -> Option<bool> {
    task.metadata
        .pointer("/review_recommendation/required")
        .and_then(Value::as_bool)
}

pub fn verification_passed(task: &RuntimeTask) -> Option<bool> {
    task.metadata
        .pointer("/verification/passed")
        .and_then(Value::as_bool)
}

pub fn verification_gate(task: &RuntimeTask) -> Option<String> {
    task.metadata
        .pointer("/verification/gate")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn verification_summary_line(task: &RuntimeTask) -> Option<String> {
    let passed = verification_passed(task)?;
    let path = task
        .metadata
        .pointer("/verification/report_path")
        .or_else(|| task.metadata.pointer("/verification/path"))
        .and_then(Value::as_str)
        .unwrap_or("not recorded");
    let status = task
        .metadata
        .pointer("/verification/status")
        .and_then(Value::as_str)
        .unwrap_or(if passed { "passed" } else { "failed" });
    let gate = task
        .metadata
        .pointer("/verification/gate")
        .and_then(Value::as_str)
        .unwrap_or(if passed { "pass" } else { "block" });
    Some(format!(
        "verification: {status}\nverification_gate: {gate}\nverification_path: {path}",
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeTaskEvent {
    Started {
        task_id: String,
    },
    Progress {
        task_id: String,
        message: String,
    },
    Output {
        task_id: String,
        stream: String,
        chunk: String,
    },
    Structured {
        task_id: String,
        stream: String,
        event: Value,
    },
    Completed {
        task_id: String,
        result: Value,
    },
    Failed {
        task_id: String,
        error: String,
    },
    Cancelled {
        task_id: String,
    },
    TimedOut {
        task_id: String,
    },
    CompactCompleted {
        task_id: String,
        summary_path: PathBuf,
    },
    VerificationRecorded {
        task_id: String,
        passed: bool,
        verification_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate: Option<String>,
        #[serde(default)]
        failed_check_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_path: Option<PathBuf>,
    },
    WorkflowNodeStarted {
        task_id: String,
        node_id: String,
        label: String,
    },
    WorkflowNodeCompleted {
        task_id: String,
        node_id: String,
        status: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTaskSpec {
    pub task_id: Option<String>,
    pub kind: RuntimeTaskKind,
    pub title: String,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub timeout_seconds: u64,
    pub auto_compact_threshold_chars: Option<usize>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTaskHandle {
    pub task_id: String,
    pub task_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactScope {
    PwcliOnly,
    AgentOnly,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactSummary {
    pub task_id: String,
    pub summary_path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub passed: bool,
    pub content: String,
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<VerificationReport>,
}

#[derive(Clone)]
pub struct RuntimeTaskManager {
    pwcli_home: PathBuf,
    tasks_dir: PathBuf,
    event_tx: mpsc::Sender<RuntimeTaskEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<RuntimeTaskEvent>>>,
    cancellations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl RuntimeTaskManager {
    pub fn new(pwcli_home: impl Into<PathBuf>) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let pwcli_home = pwcli_home.into();
        Self {
            tasks_dir: pwcli_home.join("tasks"),
            pwcli_home,
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.tasks_dir)?;
        Ok(())
    }

    pub fn create_task(
        &self,
        kind: RuntimeTaskKind,
        title: impl Into<String>,
        cwd: impl Into<PathBuf>,
        metadata: Value,
    ) -> Result<RuntimeTask> {
        self.ensure()?;
        let now = now_millis();
        let task = RuntimeTask {
            task_id: new_task_id(),
            kind,
            status: RuntimeTaskStatus::Pending,
            title: title.into(),
            cwd: cwd.into(),
            created_at: now,
            updated_at: now,
            metadata,
        };
        self.persist_task(&task)?;
        Ok(task)
    }

    pub fn set_active(&self, task_id: &str) -> Result<()> {
        self.ensure()?;
        let task = if task_id.trim() == "last" {
            self.list()?.into_iter().next()
        } else if task_id.trim() == "active" {
            self.active_task_id()?
                .and_then(|active| self.read_task(&active).ok())
        } else {
            self.resolve_task(task_id)?
        }
        .ok_or_else(|| PwError::Message(format!("task not found: {task_id}")))?;
        fs::write(self.active_task_path(), task.task_id)?;
        Ok(())
    }

    pub fn active_task_id(&self) -> Result<Option<String>> {
        let path = self.active_task_path();
        if !path.is_file() {
            return Ok(None);
        }
        let task_id = fs::read_to_string(path)?.trim().to_string();
        if task_id.is_empty() {
            return Ok(None);
        }
        if self.read_task(&task_id).is_ok() {
            Ok(Some(task_id))
        } else {
            Ok(None)
        }
    }

    pub fn resolve_task_id(&self, selector: Option<&str>) -> Result<Option<String>> {
        let selector = selector.unwrap_or("active").trim();
        if selector.is_empty() || selector == "active" {
            return self.active_task_id();
        }
        if selector == "last" {
            return Ok(self.list()?.into_iter().next().map(|task| task.task_id));
        }
        Ok(self.resolve_task(selector)?.map(|task| task.task_id))
    }

    pub fn spawn(&self, spec: RuntimeTaskSpec) -> Result<RuntimeTaskHandle> {
        let (task, handle, cancel) = self.prepare_task(&spec)?;

        let manager = self.clone();
        thread::spawn(move || manager.run_process(task, spec, cancel));

        Ok(handle)
    }

    pub fn spawn_detached(
        &self,
        spec: RuntimeTaskSpec,
        runner_exe: impl AsRef<Path>,
    ) -> Result<RuntimeTaskHandle> {
        let (mut task, handle, _cancel) = self.prepare_task(&spec)?;
        write_json(&handle.task_dir.join("spec.json"), &spec)?;
        let pwcli_home = self.tasks_dir.parent().ok_or_else(|| {
            PwError::ToolExecution("runtime tasks directory has no parent".to_string())
        })?;
        let spawn_result = Command::new(runner_exe.as_ref())
            .arg("__runtime-task")
            .arg(pwcli_home)
            .arg(&handle.task_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match spawn_result {
            Ok(_) => Ok(handle),
            Err(err) => {
                task.status = RuntimeTaskStatus::Failed;
                task.updated_at = now_millis();
                task.metadata = merge_metadata(
                    Some(&task.metadata),
                    &json!({
                        "review_recommendation": {
                            "required": true,
                            "reason": "detached runtime worker failed to start"
                        },
                        "runtime_error": err.to_string()
                    }),
                );
                self.persist_task(&task)?;
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: format!("failed to start detached runtime worker: {err}"),
                    },
                );
                Err(PwError::ToolExecution(format!(
                    "failed to start detached runtime worker: {err}"
                )))
            }
        }
    }

    pub fn run_persisted_task(&self, task_id: &str) -> Result<()> {
        self.ensure()?;
        let task = self.read_task(task_id)?;
        let spec = read_json::<RuntimeTaskSpec>(&self.task_dir(task_id).join("spec.json"))?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.run_process(task, spec, cancel);
        Ok(())
    }

    pub fn cancel(&self, task_id: &str) -> Result<()> {
        let mut task = self.read_task(task_id)?;
        if matches!(
            task.status,
            RuntimeTaskStatus::Completed
                | RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Cancelled
                | RuntimeTaskStatus::TimedOut
        ) {
            return Err(PwError::Message(format!(
                "task {task_id} is already {:?}",
                task.status
            )));
        }
        if let Some(cancel) = self
            .cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
        {
            cancel.store(true, Ordering::SeqCst);
        }
        task.status = RuntimeTaskStatus::Cancelled;
        task.updated_at = now_millis();
        self.persist_task(&task)?;
        self.emit(
            task_id,
            RuntimeTaskEvent::Cancelled {
                task_id: task_id.to_string(),
            },
        )?;
        Ok(())
    }

    pub fn delete(&self, task_id: &str) -> Result<()> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return Err(PwError::Message("task id cannot be empty".to_string()));
        }

        // Ensure task exists and validate requested selector before removing files.
        let _ = self.read_task(task_id)?;

        if let Some(cancel) = self
            .cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
        {
            cancel.store(true, Ordering::SeqCst);
        }

        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(task_id);

        let active_task_path = self.active_task_path();
        if let Ok(current_active) = fs::read_to_string(&active_task_path) {
            if current_active.trim() == task_id {
                let _ = fs::remove_file(active_task_path);
            }
        }

        let task_dir = self.task_dir(task_id);
        if task_dir.exists() {
            fs::remove_dir_all(&task_dir).map_err(|err| {
                PwError::ToolExecution(format!("failed to delete task {task_id}: {err}"))
            })?;
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<RuntimeTask>> {
        self.ensure()?;
        let mut tasks = Vec::new();
        for entry in fs::read_dir(&self.tasks_dir)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            if let Ok(task) = read_json::<RuntimeTask>(&entry.path().join("task.json")) {
                tasks.push(task);
            }
        }
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
        Ok(tasks)
    }

    pub fn get(&self, task_id: &str) -> Result<RuntimeTask> {
        self.read_task(task_id)
    }

    pub fn merge_task_metadata(&self, task_id: &str, metadata: Value) -> Result<()> {
        let mut task = self.read_task(task_id)?;
        task.metadata = merge_metadata(Some(&task.metadata), &metadata);
        task.updated_at = now_millis();
        self.persist_task(&task)
    }

    pub fn mark_task_status(
        &self,
        task_id: &str,
        status: RuntimeTaskStatus,
        metadata: Value,
    ) -> Result<()> {
        let mut task = self.read_task(task_id)?;
        task.status = status;
        task.metadata = merge_metadata(Some(&task.metadata), &metadata);
        task.updated_at = now_millis();
        self.persist_task(&task)
    }

    pub fn record_workflow_node_started(
        &self,
        task_id: &str,
        node_id: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<()> {
        self.emit(
            task_id,
            RuntimeTaskEvent::WorkflowNodeStarted {
                task_id: task_id.to_string(),
                node_id: node_id.into(),
                label: label.into(),
            },
        )
    }

    pub fn record_workflow_node_completed(
        &self,
        task_id: &str,
        node_id: impl Into<String>,
        status: impl Into<String>,
    ) -> Result<()> {
        self.emit(
            task_id,
            RuntimeTaskEvent::WorkflowNodeCompleted {
                task_id: task_id.to_string(),
                node_id: node_id.into(),
                status: status.into(),
            },
        )
    }

    pub fn poll_events(&self) -> Vec<RuntimeTaskEvent> {
        let rx = self
            .event_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn read_events_from(
        &self,
        task_id: &str,
        offset: u64,
    ) -> Result<(Vec<RuntimeTaskEvent>, u64)> {
        let path = self.task_dir(task_id).join("events.jsonl");
        if !path.is_file() {
            return Ok((Vec::new(), offset));
        }
        let mut file = fs::File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let new_offset = file.stream_position()?;
        let events = buf
            .lines()
            .filter_map(|line| serde_json::from_str::<RuntimeTaskEvent>(line).ok())
            .collect();
        Ok((events, new_offset))
    }

    pub fn compact(&self, task_id: &str, _scope: CompactScope) -> Result<CompactSummary> {
        let task = self.read_task(task_id)?;
        let task_dir = self.task_dir(task_id);
        let stdout = read_optional(&task_dir.join("stdout.log"));
        let stderr = read_optional(&task_dir.join("stderr.log"));
        let events = read_optional(&task_dir.join("events.jsonl"));
        let agent_compact = if matches!(_scope, CompactScope::AgentOnly | CompactScope::Both) {
            agent_compact_prompt(&task)
        } else {
            String::new()
        };
        let content = format!(
            "# Task {task_id} Summary\n\nStatus: {:?}\nKind: {:?}\nTitle: {}\nCWD: {}\n\n## Metadata\n{}\n\n## Agent Session Compact Prompt\n{}\n\n## Recent stdout\n{}\n\n## Recent stderr\n{}\n\n## Recent events\n{}\n",
            task.status,
            task.kind,
            task.title,
            task.cwd.display(),
            serde_json::to_string_pretty(&task.metadata)?,
            agent_compact,
            tail_chars(&stdout, 8000),
            tail_chars(&stderr, 4000),
            tail_chars(&events, 4000)
        );
        let summary_path = task_dir.join("summary.md");
        fs::write(&summary_path, &content)?;
        self.emit(
            task_id,
            RuntimeTaskEvent::CompactCompleted {
                task_id: task_id.to_string(),
                summary_path: summary_path.clone(),
            },
        )?;
        Ok(CompactSummary {
            task_id: task_id.to_string(),
            summary_path,
            content,
        })
    }

    pub fn record_verification(
        &self,
        task_id: &str,
        record: VerificationRecord,
    ) -> Result<PathBuf> {
        let mut task = self.read_task(task_id)?;
        let task_dir = self.task_dir(task_id);
        let report = record.report.clone().unwrap_or_else(|| {
            verification_report_from_metadata(&record.metadata).unwrap_or_else(|| {
                legacy_verification_report(
                    "verification.legacy",
                    task.cwd.display().to_string(),
                    record.passed,
                    record.content.clone(),
                    record.metadata.clone(),
                )
            })
        });
        let markdown = render_verification_report_markdown(&report);
        let report_path = task_dir.join("verification_report.md");
        let report_json_path = task_dir.join("verification_report.json");
        let verification_path = task_dir.join("verification.md");
        fs::write(&report_path, &markdown)?;
        write_json(&report_json_path, &report)?;
        fs::write(&verification_path, &markdown)?;
        write_json(&task_dir.join("verification.json"), &record)?;
        let passed = report.passed();
        let status = report.status.as_str().to_string();
        let gate = report.gate.decision.as_str().to_string();
        let failed_check_count = report.failed_check_count();
        task.metadata = merge_metadata(
            Some(&task.metadata),
            &json!({
                "verification": {
                    "passed": passed,
                    "status": status,
                    "gate": gate,
                    "path": verification_path.display().to_string(),
                    "report_path": report_path.display().to_string(),
                    "report_json_path": report_json_path.display().to_string(),
                    "summary": report.summary,
                    "failed_check_count": failed_check_count,
                    "metadata": record.metadata,
                    "updated_at": now_millis()
                }
            }),
        );
        task.updated_at = now_millis();
        self.persist_task(&task)?;
        self.emit(
            task_id,
            RuntimeTaskEvent::VerificationRecorded {
                task_id: task_id.to_string(),
                passed,
                verification_path: verification_path.clone(),
                status: Some(status),
                gate: Some(gate),
                failed_check_count,
                report_path: Some(report_path),
            },
        )?;
        Ok(verification_path)
    }

    pub fn task_dir(&self, task_id: &str) -> PathBuf {
        self.tasks_dir.join(task_id)
    }

    fn active_task_path(&self) -> PathBuf {
        self.tasks_dir.join("active")
    }

    fn resolve_task(&self, selector: &str) -> Result<Option<RuntimeTask>> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Ok(None);
        }
        for task in self.list()? {
            if task.task_id == selector || task.task_id.starts_with(selector) {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }

    fn prepare_task(
        &self,
        spec: &RuntimeTaskSpec,
    ) -> Result<(RuntimeTask, RuntimeTaskHandle, Arc<AtomicBool>)> {
        self.ensure()?;
        if spec.command.is_empty() {
            return Err(PwError::ToolExecution(
                "runtime task command cannot be empty".to_string(),
            ));
        }

        let task_id = spec.task_id.clone().unwrap_or_else(new_task_id);
        let task_dir = self.task_dir(&task_id);
        fs::create_dir_all(&task_dir)?;

        let existing = self.read_task(&task_id).ok();
        let now = now_millis();
        let title = existing
            .as_ref()
            .map(|task| task.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| spec.title.clone());
        let task = RuntimeTask {
            task_id: task_id.clone(),
            kind: spec.kind,
            status: RuntimeTaskStatus::Running,
            title,
            cwd: spec.cwd.clone(),
            created_at: existing.as_ref().map(|task| task.created_at).unwrap_or(now),
            updated_at: now,
            metadata: merge_metadata(
                existing.as_ref().map(|task| &task.metadata),
                &merge_metadata(
                    Some(&spec.metadata),
                    &json!({
                        "current_step": {
                            "title": spec.title.clone(),
                            "kind": spec.kind
                        }
                    }),
                ),
            ),
        };
        self.persist_task(&task)?;

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id.clone(), Arc::clone(&cancel));

        self.emit(
            &task_id,
            RuntimeTaskEvent::Started {
                task_id: task_id.clone(),
            },
        )?;

        Ok((task, RuntimeTaskHandle { task_id, task_dir }, cancel))
    }

    fn run_process(&self, mut task: RuntimeTask, spec: RuntimeTaskSpec, cancel: Arc<AtomicBool>) {
        let result = self.run_process_inner(&task.task_id, &spec, cancel);
        task.updated_at = now_millis();

        match result {
            Ok(ProcessOutcome::Completed { result }) => {
                task.status = RuntimeTaskStatus::Completed;
                task.metadata = merge_metadata(
                    Some(&task.metadata),
                    &json!({
                        "review_recommendation": result
                            .get("review_recommendation")
                            .cloned()
                            .unwrap_or_else(|| json!({ "required": false }))
                    }),
                );
                let _ = self.persist_task(&task);
                let _ = write_json(&self.task_dir(&task.task_id).join("result.json"), &result);
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::Completed {
                        task_id: task.task_id.clone(),
                        result,
                    },
                );
                let _ = self.maybe_auto_compact(&task.task_id, &spec);
            }
            Ok(ProcessOutcome::Failed { error }) => {
                task.status = RuntimeTaskStatus::Failed;
                task.metadata = merge_metadata(
                    Some(&task.metadata),
                    &json!({
                        "review_recommendation": {
                            "required": true,
                            "reason": "task failed"
                        }
                    }),
                );
                let _ = self.persist_task(&task);
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::Failed {
                        task_id: task.task_id.clone(),
                        error,
                    },
                );
                let _ = self.maybe_auto_compact(&task.task_id, &spec);
            }
            Ok(ProcessOutcome::Cancelled) => {
                task.status = RuntimeTaskStatus::Cancelled;
                let _ = self.persist_task(&task);
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::Cancelled {
                        task_id: task.task_id.clone(),
                    },
                );
            }
            Ok(ProcessOutcome::TimedOut) => {
                task.status = RuntimeTaskStatus::TimedOut;
                let _ = self.persist_task(&task);
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::TimedOut {
                        task_id: task.task_id.clone(),
                    },
                );
            }
            Err(err) => {
                task.status = RuntimeTaskStatus::Failed;
                let _ = self.persist_task(&task);
                let _ = self.emit(
                    &task.task_id,
                    RuntimeTaskEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: err.to_string(),
                    },
                );
            }
        }

        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&task.task_id);
    }

    fn run_process_inner(
        &self,
        task_id: &str,
        spec: &RuntimeTaskSpec,
        cancel: Arc<AtomicBool>,
    ) -> Result<ProcessOutcome> {
        let task_dir = self.task_dir(task_id);
        let stdout_path = task_dir.join("stdout.log");
        let stderr_path = task_dir.join("stderr.log");
        let started = SystemTime::now();

        let (program, args) = spec
            .command
            .split_first()
            .ok_or_else(|| PwError::ToolExecution("runtime command is empty".to_string()))?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                PwError::ToolExecution(format!(
                    "failed to start runtime task command '{}': {err}",
                    program
                ))
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PwError::ToolExecution("failed to capture stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| PwError::ToolExecution("failed to capture stderr".to_string()))?;
        let stdout_reader = self.spawn_stream_reader(
            task_id.to_string(),
            "stdout".to_string(),
            stdout_path.clone(),
            stdout,
        );
        let stderr_reader = self.spawn_stream_reader(
            task_id.to_string(),
            "stderr".to_string(),
            stderr_path.clone(),
            stderr,
        );

        let timeout = Duration::from_secs(spec.timeout_seconds.max(1));
        loop {
            let cancelled_on_disk = self
                .read_task(task_id)
                .map(|task| task.status == RuntimeTaskStatus::Cancelled)
                .unwrap_or(false);
            if cancel.load(Ordering::SeqCst) || cancelled_on_disk {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Ok(ProcessOutcome::Cancelled);
            }

            if let Some(status) = child.try_wait()? {
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                let stdout = read_optional(&stdout_path);
                let stderr = read_optional(&stderr_path);

                let duration_ms = started.elapsed().map(|d| d.as_millis()).unwrap_or(0);
                let result = json!({
                    "status_code": status.code(),
                    "success": status.success(),
                    "duration_ms": duration_ms as u64,
                    "stdout_path": stdout_path,
                    "stderr_path": stderr_path,
                    "stdout_preview": tail_chars(&stdout, 4000),
                    "stderr_preview": tail_chars(&stderr, 4000),
                    "metadata": spec.metadata,
                    "review_recommendation": review_recommendation(status.success(), &stdout, &stderr, &spec.metadata)
                });
                if let Some(native_session_id) = extract_uuid_like(&format!("{stdout}\n{stderr}")) {
                    self.update_session_metadata(task_id, &native_session_id)?;
                }
                return if status.success() {
                    Ok(ProcessOutcome::Completed { result })
                } else {
                    Ok(ProcessOutcome::Failed {
                        error: if stderr.trim().is_empty() {
                            stdout
                        } else {
                            stderr
                        },
                    })
                };
            }

            if started.elapsed().unwrap_or_default() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Ok(ProcessOutcome::TimedOut);
            }

            thread::sleep(Duration::from_millis(100));
        }
    }

    fn spawn_stream_reader<R: std::io::Read + Send + 'static>(
        &self,
        task_id: String,
        stream: String,
        path: PathBuf,
        reader: R,
    ) -> thread::JoinHandle<()> {
        let manager = self.clone();
        thread::spawn(move || {
            let mut file = match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => file,
                Err(_) => return,
            };
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                let bytes = match reader.read_line(&mut line) {
                    Ok(bytes) => bytes,
                    Err(_) => break,
                };
                if bytes == 0 {
                    break;
                }
                let _ = file.write_all(line.as_bytes());
                let chunk = line.clone();
                if let Ok(event) = serde_json::from_str::<Value>(chunk.trim()) {
                    let _ = manager.emit(
                        &task_id,
                        RuntimeTaskEvent::Structured {
                            task_id: task_id.clone(),
                            stream: stream.clone(),
                            event,
                        },
                    );
                } else {
                    let _ = manager.emit(
                        &task_id,
                        RuntimeTaskEvent::Output {
                            task_id: task_id.clone(),
                            stream: stream.clone(),
                            chunk,
                        },
                    );
                }
            }
        })
    }

    fn read_task(&self, task_id: &str) -> Result<RuntimeTask> {
        read_json(&self.task_dir(task_id).join("task.json"))
    }

    fn persist_task(&self, task: &RuntimeTask) -> Result<()> {
        write_json(&self.task_dir(&task.task_id).join("task.json"), task)
    }

    fn maybe_auto_compact(&self, task_id: &str, spec: &RuntimeTaskSpec) -> Result<()> {
        let Some(threshold) = spec.auto_compact_threshold_chars else {
            return Ok(());
        };
        let task_dir = self.task_dir(task_id);
        let size = ["stdout.log", "stderr.log", "events.jsonl"]
            .iter()
            .filter_map(|name| fs::metadata(task_dir.join(name)).ok())
            .map(|metadata| metadata.len() as usize)
            .sum::<usize>();
        if size >= threshold {
            let _ = self.compact(task_id, CompactScope::Both)?;
        }
        Ok(())
    }

    fn update_session_metadata(&self, task_id: &str, native_session_id: &str) -> Result<()> {
        let mut task = self.read_task(task_id)?;
        if task
            .metadata
            .pointer("/session/native_session_id")
            .and_then(Value::as_str)
            .is_some()
        {
            return Ok(());
        }
        if !task.metadata.is_object() {
            task.metadata = json!({});
        }
        if let Some(metadata) = task.metadata.as_object_mut() {
            let session = metadata.entry("session").or_insert_with(|| json!({}));
            if !session.is_object() {
                *session = json!({});
            }
            if let Some(session) = session.as_object_mut() {
                session.insert("native_session_id".to_string(), json!(native_session_id));
            }
        }
        task.updated_at = now_millis();
        self.persist_task(&task)
    }

    fn emit(&self, task_id: &str, event: RuntimeTaskEvent) -> Result<()> {
        append_jsonl(&self.task_dir(task_id).join("events.jsonl"), &event)?;
        self.mirror_event_to_audit(task_id, &event);
        let _ = self.event_tx.send(event);
        Ok(())
    }

    fn mirror_event_to_audit(&self, task_id: &str, event: &RuntimeTaskEvent) {
        let audit = match event {
            RuntimeTaskEvent::Started { .. } => {
                self.read_task(task_id)
                    .ok()
                    .map(|task| AuditEvent::RuntimeTaskStarted {
                        task_id: task.task_id,
                        kind: format!("{:?}", task.kind),
                        title: task.title,
                    })
            }
            RuntimeTaskEvent::Completed { result, .. } => Some(AuditEvent::RuntimeTaskCompleted {
                task_id: task_id.to_string(),
                review_required: result
                    .pointer("/review_recommendation/required")
                    .and_then(Value::as_bool),
            }),
            RuntimeTaskEvent::Failed { error, .. } => Some(AuditEvent::RuntimeTaskFailed {
                task_id: task_id.to_string(),
                error: error.clone(),
            }),
            RuntimeTaskEvent::Cancelled { .. } => Some(AuditEvent::RuntimeTaskCancelled {
                task_id: task_id.to_string(),
            }),
            RuntimeTaskEvent::TimedOut { .. } => Some(AuditEvent::RuntimeTaskTimedOut {
                task_id: task_id.to_string(),
            }),
            RuntimeTaskEvent::CompactCompleted { summary_path, .. } => {
                Some(AuditEvent::RuntimeTaskCompactCompleted {
                    task_id: task_id.to_string(),
                    summary_path: summary_path.display().to_string(),
                })
            }
            RuntimeTaskEvent::VerificationRecorded {
                passed,
                verification_path,
                status,
                gate,
                failed_check_count,
                report_path,
                ..
            } => Some(AuditEvent::RuntimeTaskVerificationRecorded {
                task_id: task_id.to_string(),
                passed: *passed,
                verification_path: verification_path.display().to_string(),
                status: status.clone(),
                gate: gate.clone(),
                failed_check_count: *failed_check_count,
                report_path: report_path.as_ref().map(|path| path.display().to_string()),
            }),
            RuntimeTaskEvent::WorkflowNodeStarted { .. }
            | RuntimeTaskEvent::WorkflowNodeCompleted { .. } => None,
            RuntimeTaskEvent::Progress { .. }
            | RuntimeTaskEvent::Output { .. }
            | RuntimeTaskEvent::Structured { .. } => None,
        };
        if let Some(audit) = audit {
            let _ = append_jsonl(&self.pwcli_home.join("audit/events.jsonl"), &audit);
        }
    }
}

enum ProcessOutcome {
    Completed { result: Value },
    Failed { error: String },
    Cancelled,
    TimedOut,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn merge_metadata(existing: Option<&Value>, next: &Value) -> Value {
    let mut merged = existing.cloned().unwrap_or_else(|| json!({}));
    merge_value(&mut merged, next);
    merged
}

fn merge_value(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(dst), Value::Object(src)) => {
            for (key, value) in src {
                merge_value(dst.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (dst, src) => *dst = src.clone(),
    }
}

fn read_optional(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn tail_chars(input: &str, max_chars: usize) -> String {
    let len = input.chars().count();
    input.chars().skip(len.saturating_sub(max_chars)).collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn new_task_id() -> String {
    format!("task_{}", now_millis())
}

fn agent_compact_prompt(task: &RuntimeTask) -> String {
    if task.kind != RuntimeTaskKind::AgentCli {
        return "No agent session compact needed for this task kind.".to_string();
    }
    "Ask the same code_agent session to summarize: current goal, decisions made, files touched, validation run, known risks, pending work, and the exact next action. Store the answer as this task's agent-side compact state.".to_string()
}

fn review_recommendation(success: bool, stdout: &str, stderr: &str, metadata: &Value) -> Value {
    let yolo = metadata
        .get("yolo")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return json!({ "required": true, "reason": "task exit status was non-zero" });
    }
    if yolo {
        return json!({ "required": true, "reason": "task used yolo/dangerous permissions" });
    }
    if !stderr.trim().is_empty() {
        return json!({ "required": true, "reason": "stderr was non-empty" });
    }
    let lower = stdout.to_lowercase();
    if lower.contains("error") || lower.contains("failed") || lower.contains("todo") {
        return json!({ "required": true, "reason": "output contains error/failure/todo markers" });
    }
    json!({ "required": false, "reason": "completed without obvious risk markers" })
}

fn extract_uuid_like(text: &str) -> Option<String> {
    for token in text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-')) {
        if is_uuid_like(token) {
            return Some(token.to_string());
        }
    }
    None
}

fn is_uuid_like(token: &str) -> bool {
    let parts = token.split('-').map(str::len).collect::<Vec<_>>();
    parts == [8, 4, 4, 4, 12] && token.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}
