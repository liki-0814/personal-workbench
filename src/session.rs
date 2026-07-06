use crate::{
    graph::{GraphMessage, GraphRunSummary, GraphStatus},
    storage::write_json,
    PwError, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub path: PathBuf,
    pub modified_at_ms: u64,
    pub status: GraphStatus,
    pub round_count: u32,
    pub user_preview: String,
    pub assistant_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub entry: SessionEntry,
    pub summary: GraphRunSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFolder {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFolderState {
    pub folders: Vec<SessionFolder>,
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
}

impl SessionRecord {
    pub fn seed_messages(&self) -> Vec<GraphMessage> {
        self.summary.state.messages.clone()
    }
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn new(pwcli_home: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: pwcli_home.into().join("sessions"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.sessions_dir)?;
        Ok(())
    }

    pub fn save(&self, id: &str, summary: &GraphRunSummary) -> Result<PathBuf> {
        self.ensure()?;
        let path = self.sessions_dir.join(format!("{id}.json"));
        write_json(&path, summary)?;
        Ok(path)
    }

    pub fn list(&self) -> Result<Vec<SessionEntry>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(summary) = read_summary(&path) else {
                continue;
            };
            entries.push(session_entry(path, &summary)?);
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified_at_ms));
        Ok(entries)
    }

    pub fn get(&self, selector: &str) -> Result<Option<SessionRecord>> {
        let Some(entry) = self.resolve_entry(selector)? else {
            return Ok(None);
        };
        let summary = read_summary(&entry.path)?;
        Ok(Some(SessionRecord { entry, summary }))
    }

    pub fn delete(&self, selector: &str) -> Result<Option<SessionEntry>> {
        let Some(entry) = self.resolve_entry(selector)? else {
            return Ok(None);
        };
        fs::remove_file(&entry.path)?;
        let mut folders = self.folder_state()?;
        if folders.assignments.remove(&entry.id).is_some() {
            self.save_folder_state(&folders)?;
        }
        Ok(Some(entry))
    }

    pub fn folder_state(&self) -> Result<SessionFolderState> {
        self.ensure()?;
        let path = self.folders_path();
        let mut state = if path.is_file() {
            serde_json::from_slice::<SessionFolderState>(&fs::read(path)?)?
        } else {
            default_folder_state()
        };
        normalize_folder_state(&mut state);
        Ok(state)
    }

    pub fn create_folder(&self, name: &str) -> Result<SessionFolderState> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PwError::Message("folder name cannot be empty".to_string()));
        }
        let mut state = self.folder_state()?;
        let id = unique_folder_id(name, &state.folders);
        state.folders.push(SessionFolder {
            id,
            name: name.to_string(),
        });
        self.save_folder_state(&state)?;
        Ok(state)
    }

    pub fn assign_folder(&self, selector: &str, folder_id: &str) -> Result<SessionFolderState> {
        let entry = self
            .resolve_entry(selector)?
            .ok_or_else(|| PwError::Message(format!("unknown session '{selector}'")))?;
        let mut state = self.folder_state()?;
        if !state.folders.iter().any(|folder| folder.id == folder_id) {
            return Err(PwError::Message(format!(
                "unknown session folder '{folder_id}'"
            )));
        }
        state.assignments.insert(entry.id, folder_id.to_string());
        self.save_folder_state(&state)?;
        Ok(state)
    }

    fn resolve_entry(&self, selector: &str) -> Result<Option<SessionEntry>> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Ok(None);
        }
        let entries = self.list()?;
        if selector == "last" {
            return Ok(entries.into_iter().next());
        }
        Ok(entries
            .into_iter()
            .find(|entry| entry.id == selector || entry.id.starts_with(selector)))
    }

    fn folders_path(&self) -> PathBuf {
        self.sessions_dir.join("folders.json")
    }

    fn save_folder_state(&self, state: &SessionFolderState) -> Result<()> {
        self.ensure()?;
        write_json(&self.folders_path(), state)
    }
}

pub fn format_session_list(entries: &[SessionEntry]) -> String {
    if entries.is_empty() {
        return "no sessions".to_string();
    }
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}\t{:?}\trounds={}\t{}\n  assistant: {}",
                entry.id,
                entry.status,
                entry.round_count,
                entry.user_preview,
                entry.assistant_preview
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_session_record(record: &SessionRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "session {}\npath: {}\nstatus: {:?}\nrounds: {}\n\n",
        record.entry.id,
        record.entry.path.display(),
        record.entry.status,
        record.entry.round_count
    ));
    for message in &record.summary.state.messages {
        match message {
            GraphMessage::User(content) => {
                out.push_str("user:\n");
                out.push_str(content);
                out.push_str("\n\n");
            }
            GraphMessage::Assistant(content) => {
                out.push_str("assistant:\n");
                out.push_str(content);
                out.push_str("\n\n");
            }
            GraphMessage::AssistantToolCalls { calls } => {
                out.push_str("assistant tool calls:\n");
                for call in calls {
                    out.push_str(&format!(
                        "- {} ({}) {}\n",
                        call.name, call.id, call.arguments
                    ));
                }
                out.push('\n');
            }
            GraphMessage::Tool {
                call_id,
                name,
                content,
                is_error,
            } => {
                let label = if name.trim().is_empty() {
                    call_id.as_str()
                } else {
                    name.as_str()
                };
                out.push_str(&format!("tool {label} ({call_id}) error={is_error}:\n"));
                out.push_str(content);
                out.push_str("\n\n");
            }
            GraphMessage::System(content) => {
                out.push_str("system:\n");
                out.push_str(content);
                out.push_str("\n\n");
            }
        }
    }
    out
}

fn read_summary(path: &Path) -> Result<GraphRunSummary> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn session_entry(path: PathBuf, summary: &GraphRunSummary) -> Result<SessionEntry> {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();
    let modified_at_ms = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    Ok(SessionEntry {
        id,
        path,
        modified_at_ms,
        status: summary.state.status,
        round_count: summary.state.round_count,
        user_preview: preview(first_user_message(summary), 120),
        assistant_preview: preview(last_assistant_message(summary), 160),
    })
}

fn first_user_message(summary: &GraphRunSummary) -> &str {
    summary
        .state
        .messages
        .iter()
        .find_map(|message| match message {
            GraphMessage::User(content) => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or("")
}

fn last_assistant_message(summary: &GraphRunSummary) -> &str {
    summary
        .state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            GraphMessage::Assistant(content) => Some(content.as_str()),
            _ => None,
        })
        .filter(|content| !content.trim().is_empty())
        .unwrap_or(&summary.state.last_content)
}

fn preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn default_folder_state() -> SessionFolderState {
    SessionFolderState {
        folders: [
            "Product",
            "Research",
            "Infrastructure",
            "Personal",
            "Archive",
        ]
        .into_iter()
        .map(|name| SessionFolder {
            id: slugify_folder(name),
            name: name.to_string(),
        })
        .collect(),
        assignments: BTreeMap::new(),
    }
}

fn normalize_folder_state(state: &mut SessionFolderState) {
    if state.folders.is_empty() {
        state.folders = default_folder_state().folders;
    }
    let ids = state
        .folders
        .iter()
        .map(|folder| folder.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    state
        .assignments
        .retain(|_, folder_id| ids.contains(folder_id));
}

fn unique_folder_id(name: &str, folders: &[SessionFolder]) -> String {
    let base = slugify_folder(name);
    if !folders.iter().any(|folder| folder.id == base) {
        return base;
    }
    for idx in 2.. {
        let candidate = format!("{base}-{idx}");
        if !folders.iter().any(|folder| folder.id == candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded loop returns")
}

fn slugify_folder(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "folder".to_string()
    } else {
        out
    }
}
