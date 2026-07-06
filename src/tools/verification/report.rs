use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Warning,
    Skipped,
}

impl VerificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Warning => "warning",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationGateDecision {
    Pass,
    Block,
    NeedsReview,
}

impl VerificationGateDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Block => "block",
            Self::NeedsReview => "needs_review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub label: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationCheck {
    pub id: String,
    pub suite: String,
    pub name: String,
    pub status: VerificationStatus,
    pub severity: VerificationSeverity,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub timed_out: bool,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<VerificationEvidence>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSuiteReport {
    pub id: String,
    pub name: String,
    pub status: VerificationStatus,
    pub summary: String,
    pub check_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationGate {
    pub decision: VerificationGateDecision,
    pub reason: String,
    #[serde(default)]
    pub blocked_check_ids: Vec<String>,
    #[serde(default)]
    pub review_check_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationNextAction {
    pub label: String,
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub id: String,
    pub tool_id: String,
    pub created_at: String,
    pub cwd: String,
    pub status: VerificationStatus,
    pub gate: VerificationGate,
    pub summary: String,
    #[serde(default)]
    pub suites: Vec<VerificationSuiteReport>,
    #[serde(default)]
    pub checks: Vec<VerificationCheck>,
    pub next_action: VerificationNextAction,
    #[serde(default)]
    pub metadata: Value,
}

impl VerificationReport {
    pub fn passed(&self) -> bool {
        self.status == VerificationStatus::Passed
            && self.gate.decision == VerificationGateDecision::Pass
    }

    pub fn failed_check_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == VerificationStatus::Failed)
            .count()
    }
}

pub fn build_verification_report(
    tool_id: impl Into<String>,
    cwd: impl Into<String>,
    checks: Vec<VerificationCheck>,
    metadata: Value,
) -> VerificationReport {
    let tool_id = tool_id.into();
    let cwd = cwd.into();
    let status = aggregate_status(checks.iter().map(|check| check.status));
    let gate = build_gate(status, &checks);
    let suites = build_suites(&checks);
    let summary = build_summary(status, gate.decision, &checks);
    let next_action = build_next_action(gate.decision, &checks);
    VerificationReport {
        id: new_report_id(),
        tool_id,
        created_at: Utc::now().to_rfc3339(),
        cwd,
        status,
        gate,
        summary,
        suites,
        checks,
        next_action,
        metadata,
    }
}

pub fn build_skipped_verification_report(
    tool_id: impl Into<String>,
    cwd: impl Into<String>,
    reason: impl Into<String>,
) -> VerificationReport {
    let cwd = cwd.into();
    let reason = reason.into();
    let check = VerificationCheck {
        id: "check_no_commands".to_string(),
        suite: "discovery".to_string(),
        name: "No verification commands detected".to_string(),
        status: VerificationStatus::Skipped,
        severity: VerificationSeverity::Medium,
        command: None,
        cwd: Some(cwd.clone()),
        exit_code: None,
        duration_ms: 0,
        timed_out: false,
        summary: reason,
        evidence: Vec::new(),
        metadata: json!({}),
    };
    build_verification_report(tool_id, cwd, vec![check], json!({ "auto_detected": true }))
}

pub fn verification_report_from_metadata(metadata: &Value) -> Option<VerificationReport> {
    serde_json::from_value(metadata.get("report")?.clone()).ok()
}

pub fn legacy_verification_report(
    tool_id: impl Into<String>,
    cwd: impl Into<String>,
    passed: bool,
    content: impl Into<String>,
    metadata: Value,
) -> VerificationReport {
    let status = if passed {
        VerificationStatus::Passed
    } else {
        VerificationStatus::Failed
    };
    let check = VerificationCheck {
        id: "check_legacy".to_string(),
        suite: "legacy".to_string(),
        name: "Legacy verification result".to_string(),
        status,
        severity: if passed {
            VerificationSeverity::Info
        } else {
            VerificationSeverity::High
        },
        command: None,
        cwd: None,
        exit_code: None,
        duration_ms: 0,
        timed_out: false,
        summary: if passed {
            "legacy verification record passed".to_string()
        } else {
            "legacy verification record failed".to_string()
        },
        evidence: vec![VerificationEvidence {
            label: "content".to_string(),
            content: content.into(),
        }],
        metadata: metadata.clone(),
    };
    build_verification_report(tool_id, cwd, vec![check], metadata)
}

pub fn render_verification_report_markdown(report: &VerificationReport) -> String {
    let mut out = String::new();
    out.push_str("# Verification Report\n\n");
    out.push_str(&format!("id: {}\n", report.id));
    out.push_str(&format!("tool: {}\n", report.tool_id));
    out.push_str(&format!("created_at: {}\n", report.created_at));
    out.push_str(&format!("cwd: {}\n", report.cwd));
    out.push_str(&format!("status: {}\n", report.status.as_str()));
    out.push_str(&format!("gate: {}\n", report.gate.decision.as_str()));
    out.push_str(&format!("summary: {}\n\n", report.summary));

    out.push_str("## Gate\n\n");
    out.push_str(&format!("decision: {}\n", report.gate.decision.as_str()));
    out.push_str(&format!("reason: {}\n", report.gate.reason));
    if !report.gate.blocked_check_ids.is_empty() {
        out.push_str(&format!(
            "blocked_checks: {}\n",
            report.gate.blocked_check_ids.join(", ")
        ));
    }
    if !report.gate.review_check_ids.is_empty() {
        out.push_str(&format!(
            "review_checks: {}\n",
            report.gate.review_check_ids.join(", ")
        ));
    }

    if !report.suites.is_empty() {
        out.push_str("\n## Suites\n\n");
        for suite in &report.suites {
            out.push_str(&format!(
                "- {}: {} ({} checks) - {}\n",
                suite.name,
                suite.status.as_str(),
                suite.check_ids.len(),
                suite.summary
            ));
        }
    }

    out.push_str("\n## Checks\n\n");
    if report.checks.is_empty() {
        out.push_str("no checks recorded\n");
    }
    for check in &report.checks {
        out.push_str(&format!(
            "### [{}] {}\n\n",
            check.status.as_str(),
            check.name
        ));
        out.push_str(&format!("id: {}\n", check.id));
        out.push_str(&format!("suite: {}\n", check.suite));
        out.push_str(&format!("severity: {:?}\n", check.severity));
        if let Some(command) = &check.command {
            out.push_str(&format!("command: `{}`\n", command));
        }
        if let Some(cwd) = &check.cwd {
            out.push_str(&format!("cwd: {}\n", cwd));
        }
        if check.timed_out {
            out.push_str(&format!("timed_out: true after {}ms\n", check.duration_ms));
        } else if check.duration_ms > 0 {
            out.push_str(&format!("duration_ms: {}\n", check.duration_ms));
        }
        if let Some(exit_code) = check.exit_code {
            out.push_str(&format!("exit_code: {}\n", exit_code));
        }
        out.push_str(&format!("summary: {}\n", check.summary));
        for evidence in &check.evidence {
            if evidence.content.trim().is_empty() {
                continue;
            }
            out.push_str(&format!("\n{}:\n```text\n", evidence.label));
            out.push_str(&evidence.content);
            if !evidence.content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out.push('\n');
    }

    out.push_str("## Next Action\n\n");
    out.push_str(&format!("label: {}\n", report.next_action.label));
    if !report.next_action.command.trim().is_empty() {
        out.push_str(&format!("command: `{}`\n", report.next_action.command));
    }
    out.push_str(&format!("reason: {}\n", report.next_action.reason));
    out
}

fn aggregate_status(statuses: impl Iterator<Item = VerificationStatus>) -> VerificationStatus {
    let statuses = statuses.collect::<Vec<_>>();
    if statuses.is_empty()
        || statuses
            .iter()
            .all(|status| *status == VerificationStatus::Skipped)
    {
        return VerificationStatus::Skipped;
    }
    if statuses.contains(&VerificationStatus::Failed) {
        return VerificationStatus::Failed;
    }
    if statuses.contains(&VerificationStatus::Warning) {
        return VerificationStatus::Warning;
    }
    VerificationStatus::Passed
}

fn build_gate(status: VerificationStatus, checks: &[VerificationCheck]) -> VerificationGate {
    let blocked_check_ids = checks
        .iter()
        .filter(|check| check.status == VerificationStatus::Failed)
        .map(|check| check.id.clone())
        .collect::<Vec<_>>();
    let review_check_ids = checks
        .iter()
        .filter(|check| {
            matches!(
                check.status,
                VerificationStatus::Warning | VerificationStatus::Skipped
            )
        })
        .map(|check| check.id.clone())
        .collect::<Vec<_>>();

    match status {
        VerificationStatus::Passed => VerificationGate {
            decision: VerificationGateDecision::Pass,
            reason: "all deterministic verification checks passed".to_string(),
            blocked_check_ids,
            review_check_ids,
        },
        VerificationStatus::Failed => VerificationGate {
            decision: VerificationGateDecision::Block,
            reason: "one or more deterministic verification checks failed".to_string(),
            blocked_check_ids,
            review_check_ids,
        },
        VerificationStatus::Warning => VerificationGate {
            decision: VerificationGateDecision::NeedsReview,
            reason: "verification completed with warnings that need review".to_string(),
            blocked_check_ids,
            review_check_ids,
        },
        VerificationStatus::Skipped => VerificationGate {
            decision: VerificationGateDecision::NeedsReview,
            reason: "verification was skipped or no deterministic checks were available"
                .to_string(),
            blocked_check_ids,
            review_check_ids,
        },
    }
}

fn build_suites(checks: &[VerificationCheck]) -> Vec<VerificationSuiteReport> {
    let mut suite_checks = BTreeMap::<String, Vec<&VerificationCheck>>::new();
    for check in checks {
        suite_checks
            .entry(check.suite.clone())
            .or_default()
            .push(check);
    }
    suite_checks
        .into_iter()
        .map(|(suite, checks)| {
            let status = aggregate_status(checks.iter().map(|check| check.status));
            VerificationSuiteReport {
                id: stable_id("suite", &suite),
                name: suite,
                status,
                summary: build_suite_summary(status, checks.len()),
                check_ids: checks.into_iter().map(|check| check.id.clone()).collect(),
            }
        })
        .collect()
}

fn build_summary(
    status: VerificationStatus,
    gate: VerificationGateDecision,
    checks: &[VerificationCheck],
) -> String {
    let failed = checks
        .iter()
        .filter(|check| check.status == VerificationStatus::Failed)
        .count();
    let skipped = checks
        .iter()
        .filter(|check| check.status == VerificationStatus::Skipped)
        .count();
    let suites = checks
        .iter()
        .map(|check| check.suite.clone())
        .collect::<BTreeSet<_>>()
        .len();
    format!(
        "{} checks across {} suites: status={}, gate={}, failed={}, skipped={}",
        checks.len(),
        suites,
        status.as_str(),
        gate.as_str(),
        failed,
        skipped
    )
}

fn build_suite_summary(status: VerificationStatus, count: usize) -> String {
    format!("{count} checks, status={}", status.as_str())
}

fn build_next_action(
    gate: VerificationGateDecision,
    checks: &[VerificationCheck],
) -> VerificationNextAction {
    match gate {
        VerificationGateDecision::Pass => VerificationNextAction {
            label: "continue".to_string(),
            command: "pwcli memory extract task".to_string(),
            reason: "deterministic verification passed".to_string(),
        },
        VerificationGateDecision::Block => {
            let failed_commands = checks
                .iter()
                .filter(|check| check.status == VerificationStatus::Failed)
                .filter_map(|check| check.command.clone())
                .take(3)
                .collect::<Vec<_>>();
            VerificationNextAction {
                label: "fix".to_string(),
                command: "pwcli loop --wait".to_string(),
                reason: if failed_commands.is_empty() {
                    "fix failed verification checks before continuing".to_string()
                } else {
                    format!("fix failed checks: {}", failed_commands.join("; "))
                },
            }
        }
        VerificationGateDecision::NeedsReview => VerificationNextAction {
            label: "review".to_string(),
            command: "pwcli review --wait".to_string(),
            reason: "verification did not produce enough deterministic evidence to pass"
                .to_string(),
        },
    }
}

fn new_report_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("verif_{millis}")
}

pub fn stable_id(prefix: &str, input: &str) -> String {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("{prefix}_{hash:016x}")
}
