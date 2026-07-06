use crate::{
    audit::{read_audit_events, AuditEvent},
    memory::{MemoryEmbeddingSettings, MemorySettings, MemoryStore},
    tools::{RiskLevel, ToolDescriptor, ToolRegistrySnapshot, ToolSource},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::PathBuf,
};

const CONTEXT_FILE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPack {
    pub id: String,
    pub user_input: String,
    pub summary: String,
    pub selected_tool_ids: Vec<String>,
    #[serde(default)]
    pub tool_selection_plan: ToolSelectionPlan,
    pub explicit_skill_ids: Vec<String>,
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
    pub memory_items: Vec<String>,
    pub rule_items: Vec<String>,
    pub local_items: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSelectionPlan {
    pub task_type: String,
    pub steps: Vec<ToolSelectionStep>,
    pub details: Vec<ToolSelectionDetail>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSelectionStep {
    pub stage: String,
    pub tool_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSelectionDetail {
    pub tool_id: String,
    pub stage: String,
    pub reason: String,
    pub score: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_for: Option<String>,
    pub source: String,
    pub risk: String,
}

#[derive(Debug, Clone)]
pub struct ContextBuilder {
    max_implicit_skills: usize,
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self {
            max_implicit_skills: 6,
        }
    }
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(
        &self,
        user_input: impl Into<String>,
        snapshot: &ToolRegistrySnapshot,
    ) -> ContextPack {
        self.build_with_sources(user_input, snapshot, None, Vec::new())
    }

    pub fn build_with_sources(
        &self,
        user_input: impl Into<String>,
        snapshot: &ToolRegistrySnapshot,
        pwcli_home: Option<PathBuf>,
        local_paths: Vec<PathBuf>,
    ) -> ContextPack {
        self.build_with_sources_and_memory(
            user_input,
            snapshot,
            pwcli_home,
            local_paths,
            &MemorySettings {
                enabled: true,
                auto_consider_write: false,
                semantic_extraction: Default::default(),
                embedding: MemoryEmbeddingSettings::default(),
            },
        )
    }

    pub fn build_with_sources_and_memory(
        &self,
        user_input: impl Into<String>,
        snapshot: &ToolRegistrySnapshot,
        pwcli_home: Option<PathBuf>,
        local_paths: Vec<PathBuf>,
        memory_settings: &MemorySettings,
    ) -> ContextPack {
        let user_input = user_input.into();
        let descriptors = snapshot.descriptors();
        let explicit_skill_ids = explicit_skill_ids(&user_input, &descriptors);
        let history = pwcli_home
            .as_ref()
            .map(|home| load_tool_history(home))
            .unwrap_or_default();
        let tool_selection_plan =
            self.select_tools(&user_input, &descriptors, &explicit_skill_ids, &history);
        let selected_tool_ids = tool_selection_plan
            .details
            .iter()
            .map(|detail| detail.tool_id.clone())
            .collect::<Vec<_>>();
        let mut warnings = Vec::new();
        let missing = if selected_tool_ids.is_empty() {
            warnings
                .push("no matching tools selected; graph will run without tool calls".to_string());
            vec!["No tool matched the user request.".to_string()]
        } else {
            Vec::new()
        };
        let mut memory_items = Vec::new();
        if let Some(home) = pwcli_home.as_ref() {
            if memory_settings.enabled {
                match MemoryStore::new(home, memory_settings.embedding.clone())
                    .recall(&user_input, 8)
                {
                    Ok(recall) => {
                        memory_items.extend(recall.facts.into_iter().map(|scored| {
                            format!(
                                "[fact score={:.2}] {}\nsource: {}\nobserved_at: {}",
                                scored.score,
                                scored.fact.statement,
                                scored.fact.source,
                                scored.fact.observed_at
                            )
                        }));
                        memory_items.extend(recall.inferences.into_iter().map(|inference| {
                            format!(
                                "[inference] {}\nlogic_chain: {}",
                                inference.statement, inference.logic_chain
                            )
                        }));
                        memory_items.extend(recall.hypotheses.into_iter().map(|hypothesis| {
                            format!(
                                "[hypothesis confidence={:.2}] {}",
                                hypothesis.confidence, hypothesis.statement
                            )
                        }));
                    }
                    Err(err) => warnings.push(format!("failed to recall memory: {err}")),
                }
            }
            memory_items.extend(read_context_dir(&home.join("memory"), &mut warnings));
        }
        let rule_items = pwcli_home
            .as_ref()
            .map(|home| read_context_dir(&home.join("rules"), &mut warnings))
            .unwrap_or_default();
        let local_items = read_local_paths(&local_paths, &mut warnings);
        let summary = context_summary(
            snapshot.version(),
            selected_tool_ids.len(),
            &tool_selection_plan,
            &memory_items,
            &rule_items,
            &local_items,
        );

        ContextPack {
            id: stable_context_id(&user_input, snapshot.version()),
            summary,
            user_input,
            selected_tool_ids,
            tool_selection_plan,
            explicit_skill_ids,
            missing,
            warnings,
            memory_items,
            rule_items,
            local_items,
        }
    }

    fn select_tools(
        &self,
        user_input: &str,
        descriptors: &[ToolDescriptor],
        explicit_skill_ids: &[String],
        history: &BTreeMap<String, ToolHistory>,
    ) -> ToolSelectionPlan {
        let input = user_input.to_lowercase();
        let task_type = classify_task(&input).to_string();
        let mut candidates = Vec::new();
        for descriptor in descriptors {
            if !descriptor.enabled {
                continue;
            }

            if matches!(
                descriptor
                    .metadata
                    .get("frontmatter")
                    .and_then(|m| m.get("disable-model-invocation"))
                    .and_then(|v| v.as_bool()),
                Some(true)
            ) {
                continue;
            }

            let explicit = explicit_skill_ids
                .iter()
                .any(|id| id.as_str() == descriptor.id.as_str());
            if let Some(candidate) = score_tool_candidate(
                &input,
                &task_type,
                descriptor,
                explicit,
                history.get(&descriptor.id),
            ) {
                candidates.push(candidate);
            }
        }

        candidates.sort_by(|a, b| {
            stage_rank(&a.stage)
                .cmp(&stage_rank(&b.stage))
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.tool_id.cmp(&b.tool_id))
        });

        let mut selected = Vec::new();
        let mut stage_counts = BTreeMap::<String, usize>::new();
        let mut implicit_skills = 0usize;
        for candidate in candidates {
            if matches!(candidate.source, ToolSource::Skill { .. }) && !candidate.explicit {
                if implicit_skills >= self.max_implicit_skills {
                    continue;
                }
                implicit_skills += 1;
            }
            let limit = stage_limit(&candidate.stage);
            let count = stage_counts.entry(candidate.stage.clone()).or_default();
            if *count >= limit && !candidate.explicit {
                continue;
            }
            *count += 1;
            selected.push(candidate);
        }

        let mut primary_by_stage = BTreeMap::<String, String>::new();
        let mut details = Vec::new();
        for candidate in selected {
            let fallback_for = primary_by_stage
                .entry(candidate.stage.clone())
                .or_insert_with(|| candidate.tool_id.clone());
            let fallback_for =
                (fallback_for.as_str() != candidate.tool_id.as_str()).then(|| fallback_for.clone());
            details.push(ToolSelectionDetail {
                tool_id: candidate.tool_id,
                stage: candidate.stage,
                reason: candidate.reason,
                score: candidate.score,
                fallback_for,
                source: source_label(&candidate.source).to_string(),
                risk: format!("{:?}", candidate.risk),
            });
        }

        ToolSelectionPlan {
            task_type,
            steps: build_selection_steps(&details),
            details,
        }
    }
}

fn context_summary(
    registry_version: u64,
    selected_tool_count: usize,
    tool_selection_plan: &ToolSelectionPlan,
    memory_items: &[String],
    rule_items: &[String],
    local_items: &[String],
) -> String {
    let mut sections = vec![format!(
        "Context for request using registry v{registry_version} with {selected_tool_count} selected tools."
    )];
    append_tool_plan(&mut sections, tool_selection_plan);
    append_section(&mut sections, "Relevant memory", memory_items, 6);
    append_section(&mut sections, "Rules", rule_items, 6);
    append_section(&mut sections, "Local project context", local_items, 4);
    sections.join("\n\n")
}

fn append_tool_plan(sections: &mut Vec<String>, plan: &ToolSelectionPlan) {
    if plan.details.is_empty() {
        return;
    }
    let mut lines = vec![format!("task_type: {}", plan.task_type)];
    for step in &plan.steps {
        lines.push(format!(
            "- {}: {} ({})",
            step.stage,
            step.tool_ids.join(" -> fallback "),
            step.rationale
        ));
    }
    sections.push(format!("Tool selection plan:\n{}", lines.join("\n")));
}

fn append_section(sections: &mut Vec<String>, title: &str, items: &[String], limit: usize) {
    if items.is_empty() {
        return;
    }
    let body = items
        .iter()
        .take(limit)
        .enumerate()
        .map(|(idx, item)| format!("{}. {}", idx + 1, truncate_chars(item, 1200)))
        .collect::<Vec<_>>()
        .join("\n");
    sections.push(format!("{title}:\n{body}"));
}

fn read_context_dir(dir: &std::path::Path, warnings: &mut Vec<String>) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            continue;
        }
        match read_text_preview(&path, CONTEXT_FILE_MAX_BYTES) {
            Ok((content, truncated)) => {
                if truncated {
                    warnings.push(format!(
                        "context file {} was truncated to {} bytes",
                        path.display(),
                        CONTEXT_FILE_MAX_BYTES
                    ));
                }
                items.push(format!("{}:\n{}", path.display(), content));
            }
            Err(err) => {
                warnings.push(format!(
                    "failed to read context file {}: {err}",
                    path.display()
                ));
            }
        }
    }
    items
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn read_local_paths(paths: &[PathBuf], warnings: &mut Vec<String>) -> Vec<String> {
    let mut items = Vec::new();
    for path in paths {
        match read_text_preview(path, CONTEXT_FILE_MAX_BYTES) {
            Ok((content, truncated)) => {
                if truncated {
                    warnings.push(format!(
                        "local context {} was truncated to {} bytes",
                        path.display(),
                        CONTEXT_FILE_MAX_BYTES
                    ));
                }
                items.push(format!("{}:\n{}", path.display(), content));
            }
            Err(err) => {
                warnings.push(format!(
                    "failed to read local context {}: {err}",
                    path.display()
                ));
            }
        }
    }
    items
}

fn read_text_preview(path: &std::path::Path, max_bytes: u64) -> std::io::Result<(String, bool)> {
    let metadata = fs::metadata(path)?;
    let truncated = metadata.len() > max_bytes;
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(max_bytes).read_to_end(&mut bytes)?;
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn explicit_skill_ids(input: &str, descriptors: &[ToolDescriptor]) -> Vec<String> {
    let tokens = input
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('$').or_else(|| token.strip_prefix('/')))
        .map(|token| {
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        })
        .collect::<BTreeSet<_>>();

    descriptors
        .iter()
        .filter(|descriptor| matches!(&descriptor.source, ToolSource::Skill { .. }))
        .filter(|descriptor| tokens.contains(descriptor.name.as_str()))
        .map(|descriptor| descriptor.id.clone())
        .collect()
}

fn text_matches(input_lower: &str, candidate: &str) -> bool {
    candidate
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| part.len() >= 4)
        .any(|part| input_lower.contains(&part.to_lowercase()))
}

fn intent_matches(input_lower: &str, descriptor: &ToolDescriptor) -> bool {
    match &descriptor.source {
        ToolSource::Verification => contains_any(
            input_lower,
            &[
                "verify",
                "verification",
                "validate",
                "check",
                "test",
                "tests",
                "lint",
                "typecheck",
                "cargo check",
                "cargo test",
                "npm test",
                "验证",
                "校验",
                "检查",
                "测试",
                "跑测试",
                "单测",
                "类型检查",
            ],
        ),
        ToolSource::Builtin => {
            if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "local_file_index" || cap == "file_search")
            {
                contains_any(
                    input_lower,
                    &[
                        "file",
                        "local",
                        "index",
                        "find file",
                        "grep",
                        "代码文件",
                        "本地文件",
                        "文件索引",
                        "查文件",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "web_search" || cap == "search" || cap == "anysearch")
            {
                contains_any(
                    input_lower,
                    &[
                        "search",
                        "web",
                        "browse",
                        "extract",
                        "搜索",
                        "检索",
                        "联网",
                        "查资料",
                        "学术搜索",
                        "代码搜索",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "web_fetch" || cap == "url_read")
            {
                contains_any(
                    input_lower,
                    &[
                        "http://",
                        "https://",
                        "url",
                        "网页",
                        "页面提取",
                        "抓取",
                        "fetch",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "github" || cap == "code_hosting")
            {
                contains_any(
                    input_lower,
                    &[
                        "github",
                        "pull request",
                        " pr ",
                        "issue",
                        "workflow",
                        "actions",
                        "代码仓库",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "browser" || cap == "browser_automation")
            {
                contains_any(
                    input_lower,
                    &[
                        "browser",
                        "playwright",
                        "screenshot",
                        "截图",
                        "浏览器",
                        "动态网页",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "ssh" || cap == "remote_shell" || cap == "remote_exec")
            {
                contains_any(
                    input_lower,
                    &[
                        "ssh",
                        "remote",
                        "server",
                        "机器",
                        "服务器",
                        "远程",
                        "远程执行",
                        "远程机器",
                        "日志",
                        "部署",
                        "运维",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "sql" || cap == "database" || cap == "dry_run")
            {
                contains_any(
                    input_lower,
                    &[
                        "sql",
                        "database",
                        "dry run",
                        "explain",
                        "数据库",
                        "查数",
                        "口径",
                    ],
                )
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "pdf" || cap == "document_parse" || cap == "ocr")
            {
                contains_any(
                    input_lower,
                    &["pdf", "paper", "document", "ocr", "解析", "论文", "文档"],
                )
            } else {
                false
            }
        }
        ToolSource::AgentCli { cli } => {
            let explicit_cli_requested = contains_any(
                input_lower,
                &["codex", "claude", "qoder", "qodercli", "agy"],
            );
            if explicit_cli_requested {
                input_lower.contains(cli) || (cli == "qodercli" && input_lower.contains("qoder"))
            } else {
                contains_any(
                    input_lower,
                    &[
                        "agent",
                        "code agent",
                        "subagent",
                        "写代码",
                        "改代码",
                        "重构",
                        "代码审查",
                        "review",
                    ],
                )
            }
        }
        ToolSource::Mcp { server } => {
            input_lower.contains(&server.to_lowercase()) || input_lower.contains("mcp")
        }
        ToolSource::Skill { .. } | ToolSource::Model => false,
    }
}

fn contains_any(input: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| input.contains(pattern))
}

#[derive(Debug, Clone, Default)]
struct ToolHistory {
    calls: u32,
    successes: u32,
}

impl ToolHistory {
    fn success_rate(&self) -> f32 {
        if self.calls == 0 {
            0.5
        } else {
            self.successes as f32 / self.calls as f32
        }
    }
}

#[derive(Debug, Clone)]
struct ToolCandidate {
    tool_id: String,
    stage: String,
    reason: String,
    score: i32,
    source: ToolSource,
    risk: RiskLevel,
    explicit: bool,
}

fn load_tool_history(home: &std::path::Path) -> BTreeMap<String, ToolHistory> {
    let Ok((events, _)) = read_audit_events(&home.join("audit/events.jsonl")) else {
        return BTreeMap::new();
    };
    let mut call_to_tool = BTreeMap::<String, String>::new();
    let mut history = BTreeMap::<String, ToolHistory>::new();
    for event in events {
        match event {
            AuditEvent::ToolCallRequested {
                call_id, tool_id, ..
            } => {
                call_to_tool.insert(call_id, tool_id.clone());
                history.entry(tool_id).or_default().calls += 1;
            }
            AuditEvent::ToolResultRecorded {
                call_id,
                is_error: false,
                ..
            } => {
                if let Some(tool_id) = call_to_tool.get(&call_id) {
                    history.entry(tool_id.clone()).or_default().successes += 1;
                }
            }
            _ => {}
        }
    }
    history
}

fn classify_task(input: &str) -> &'static str {
    if contains_any(input, &["pdf", "paper", "论文", "文档", "ocr", "解析"]) {
        "document"
    } else if contains_any(input, &["sql", "database", "数据库", "dry run", "explain"]) {
        "database"
    } else if contains_any(
        input,
        &["github", "pull request", "pr ", "issue", "workflow"],
    ) {
        "code_hosting"
    } else if contains_any(
        input,
        &["browser", "playwright", "screenshot", "截图", "浏览器"],
    ) {
        "browser"
    } else if contains_any(
        input,
        &[
            "ssh",
            "remote",
            "server",
            "机器",
            "服务器",
            "远程",
            "远程机器",
            "日志",
            "部署",
            "运维",
        ],
    ) {
        "remote"
    } else if contains_any(
        input,
        &[
            "search",
            "web",
            "browse",
            "research",
            "联网",
            "搜索",
            "检索",
            "查资料",
        ],
    ) {
        "research"
    } else if contains_any(
        input,
        &["verify", "test", "lint", "验证", "测试", "检查", "跑测试"],
    ) {
        "verification"
    } else if contains_any(
        input,
        &[
            "code",
            "refactor",
            "review",
            "bug",
            "写代码",
            "改代码",
            "重构",
            "代码审查",
        ],
    ) {
        "code"
    } else {
        "general"
    }
}

fn score_tool_candidate(
    input: &str,
    task_type: &str,
    descriptor: &ToolDescriptor,
    explicit: bool,
    history: Option<&ToolHistory>,
) -> Option<ToolCandidate> {
    let mut score = if explicit { 100 } else { 0 };
    let mut reasons = Vec::new();
    if explicit {
        reasons.push("explicitly requested".to_string());
    }
    if text_matches(input, &descriptor.name) || text_matches(input, &descriptor.description) {
        score += 35;
        reasons.push("text match".to_string());
    }
    if descriptor
        .capabilities
        .iter()
        .any(|cap| text_matches(input, cap))
    {
        score += 25;
        reasons.push("capability match".to_string());
    }
    if intent_matches(input, descriptor) {
        score += 45;
        reasons.push("intent match".to_string());
    }

    let stage = tool_stage(task_type, descriptor);
    let task_bonus = task_stage_bonus(task_type, &stage, descriptor);
    if task_bonus > 0 {
        score += task_bonus;
        reasons.push(format!("{task_type} workflow stage {stage}"));
    }

    let reliability_bonus = source_reliability_bonus(&descriptor.source);
    score += reliability_bonus;
    if reliability_bonus > 0 {
        reasons.push("source reliability".to_string());
    }

    if let Some(history) = history {
        let history_bonus = ((history.success_rate() - 0.5) * 20.0).round() as i32;
        score += history_bonus;
        if history.calls > 0 {
            reasons.push(format!(
                "history {}/{} successful",
                history.successes, history.calls
            ));
        }
    }

    score -= match descriptor.risk_level {
        RiskLevel::ReadOnly => 0,
        RiskLevel::Low => 3,
        RiskLevel::Medium => 10,
        RiskLevel::High => 18,
    };

    if score < 35 && !explicit {
        return None;
    }

    Some(ToolCandidate {
        tool_id: descriptor.id.clone(),
        stage,
        reason: if reasons.is_empty() {
            "available fallback".to_string()
        } else {
            reasons.join(", ")
        },
        score,
        source: descriptor.source.clone(),
        risk: descriptor.risk_level,
        explicit,
    })
}

fn tool_stage(task_type: &str, descriptor: &ToolDescriptor) -> String {
    match &descriptor.source {
        ToolSource::Verification => "verify".to_string(),
        ToolSource::AgentCli { .. } => {
            if task_type == "code" {
                "execute".to_string()
            } else {
                "delegate".to_string()
            }
        }
        ToolSource::Builtin => {
            if descriptor.capabilities.iter().any(|cap| {
                cap == "local_file_index"
                    || cap == "file_search"
                    || cap == "web_search"
                    || cap == "anysearch"
            }) {
                "search".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "web_fetch" || cap == "url_read")
            {
                "parse".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "github" || cap == "code_hosting")
            {
                "external".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "browser" || cap == "browser_automation")
            {
                "browser".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "ssh" || cap == "remote_shell" || cap == "remote_exec")
            {
                "remote".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "sql" || cap == "database" || cap == "dry_run")
            {
                "verify".to_string()
            } else if descriptor
                .capabilities
                .iter()
                .any(|cap| cap == "pdf" || cap == "document_parse" || cap == "ocr")
            {
                "parse".to_string()
            } else {
                "use".to_string()
            }
        }
        ToolSource::Mcp { .. } => "external".to_string(),
        ToolSource::Skill { .. } => "skill".to_string(),
        ToolSource::Model => "model".to_string(),
    }
}

fn task_stage_bonus(task_type: &str, stage: &str, descriptor: &ToolDescriptor) -> i32 {
    match (task_type, stage) {
        ("database", "verify") => 45,
        ("database", "external") => 15,
        ("code_hosting", "external") => 40,
        ("code_hosting", "search") => 10,
        ("browser", "browser") => 45,
        ("browser", "search") => 15,
        ("remote", "remote") => 45,
        ("remote", "verify") => 15,
        ("remote", "delegate") => 10,
        ("research", "search") => 35,
        ("research", "skill") => 20,
        ("research", "verify") => 10,
        ("document", "parse") => 40,
        ("document", "search") => 15,
        ("document", "skill") => 15,
        ("verification", "verify") => 45,
        ("verification", "execute") => 10,
        ("code", "execute") => 40,
        ("code", "verify") => 30,
        ("code", "skill") => 15,
        _ if matches!(descriptor.source, ToolSource::Mcp { .. }) && stage == "external" => 10,
        _ => 0,
    }
}

fn source_reliability_bonus(source: &ToolSource) -> i32 {
    match source {
        ToolSource::Verification => 12,
        ToolSource::Builtin => 10,
        ToolSource::Model => 8,
        ToolSource::AgentCli { .. } => 6,
        ToolSource::Mcp { .. } => 3,
        ToolSource::Skill { .. } => 2,
    }
}

fn stage_rank(stage: &str) -> u8 {
    match stage {
        "search" => 0,
        "parse" => 1,
        "browser" => 2,
        "remote" => 3,
        "skill" => 4,
        "external" => 5,
        "delegate" => 6,
        "execute" => 7,
        "verify" => 8,
        "report" => 9,
        "model" => 10,
        _ => 10,
    }
}

fn stage_limit(stage: &str) -> usize {
    match stage {
        "search" => 2,
        "browser" => 1,
        "remote" => 1,
        "execute" | "delegate" => 2,
        "skill" => 4,
        "external" => 3,
        _ => 1,
    }
}

fn build_selection_steps(details: &[ToolSelectionDetail]) -> Vec<ToolSelectionStep> {
    let mut by_stage = BTreeMap::<String, Vec<String>>::new();
    for detail in details {
        by_stage
            .entry(detail.stage.clone())
            .or_default()
            .push(detail.tool_id.clone());
    }
    let mut stages = by_stage.into_iter().collect::<Vec<_>>();
    stages.sort_by_key(|a| stage_rank(&a.0));
    stages
        .into_iter()
        .map(|(stage, tool_ids)| ToolSelectionStep {
            rationale: stage_rationale(&stage).to_string(),
            stage,
            tool_ids,
        })
        .collect()
}

fn stage_rationale(stage: &str) -> &'static str {
    match stage {
        "search" => "collect external/source material first",
        "parse" => "parse documents before synthesis",
        "browser" => "use a rendered browser for dynamic page state or screenshots",
        "remote" => "run remote SSH diagnostics or remote execution when explicitly requested",
        "skill" => "apply matching reusable skill prompts or executables",
        "external" => "call configured external MCP capability",
        "delegate" => "delegate broad work to a local code agent",
        "execute" => "execute code-agent implementation workflow",
        "verify" => "run deterministic checks before final confidence",
        "report" => "produce a structured deliverable",
        "model" => "use model provider capability",
        _ => "support the request",
    }
}

fn source_label(source: &ToolSource) -> &'static str {
    match source {
        ToolSource::Builtin => "builtin",
        ToolSource::AgentCli { .. } => "agent_cli",
        ToolSource::Skill { .. } => "skill",
        ToolSource::Mcp { .. } => "mcp",
        ToolSource::Verification => "verification",
        ToolSource::Model => "model",
    }
}

fn stable_context_id(input: &str, registry_version: u64) -> String {
    let mut hash = registry_version.wrapping_mul(1_099_511_628_211);
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("ctx_{hash:016x}")
}
