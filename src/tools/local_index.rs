use crate::{
    settings::Settings,
    tools::{
        ToolArtifact, ToolArtifactKind, ToolArtifactProvenance, ToolCall, ToolExecutionMode,
        ToolExecutionRuntime, ToolResult, ToolRuntimeEvent,
    },
    PwError, Result,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use walkdir::{DirEntry, WalkDir};

const DEFAULT_MAX_FILES: usize = 2000;
const DEFAULT_MAX_FILE_BYTES: u64 = 128 * 1024;
const DEFAULT_MAX_RESULTS: usize = 30;

#[derive(Debug, Clone)]
pub struct LocalFileIndexExecutor;

impl LocalFileIndexExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalFileIndexExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for LocalFileIndexExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut runtime = ToolExecutionRuntime::noop();
        self.execute_with_runtime(call, &mut runtime)
    }

    fn execute_with_runtime(
        &self,
        call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> Result<ToolResult> {
        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Sync,
        });
        let settings = Settings::load()?;
        let args = LocalIndexArgs::from_value(&call.arguments)?;
        let result = match args.action.as_str() {
            "index" => index_files(&settings, &args, runtime, call),
            "search" => search_files(&args, runtime),
            "read" => read_file_preview(&args),
            "stats" => stats_files(&args, runtime),
            other => Err(PwError::ToolExecution(format!(
                "unknown local_file_index action '{other}'"
            ))),
        }?;
        runtime.emit(ToolRuntimeEvent::Completed {
            is_error: result.is_error,
        });
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct LocalIndexArgs {
    action: String,
    root: PathBuf,
    query: Option<String>,
    path: Option<PathBuf>,
    extensions: Vec<String>,
    include_hidden: bool,
    max_files: usize,
    max_file_bytes: u64,
    max_results: usize,
}

impl LocalIndexArgs {
    fn from_value(value: &Value) -> Result<Self> {
        Ok(Self {
            action: optional_string(value, "action").unwrap_or_else(|| "search".to_string()),
            root: optional_string(value, "root")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            query: optional_string(value, "query"),
            path: optional_string(value, "path").map(PathBuf::from),
            extensions: string_array(value, "extensions"),
            include_hidden: optional_bool(value, "include_hidden").unwrap_or(false),
            max_files: optional_u64(value, "max_files")
                .unwrap_or(DEFAULT_MAX_FILES as u64)
                .min(20_000) as usize,
            max_file_bytes: optional_u64(value, "max_file_bytes")
                .unwrap_or(DEFAULT_MAX_FILE_BYTES)
                .min(2 * 1024 * 1024),
            max_results: optional_u64(value, "max_results")
                .unwrap_or(DEFAULT_MAX_RESULTS as u64)
                .min(200) as usize,
        })
    }
}

pub fn local_file_index_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["index", "search", "read", "stats"],
                "description": "Operation. index writes ~/.pwcli/index/local_files.json, search scans matching files, read previews one file."
            },
            "root": { "type": "string", "description": "Root directory. Defaults to current directory." },
            "query": { "type": "string", "description": "Text query for action=search." },
            "path": { "type": "string", "description": "File path for action=read." },
            "extensions": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional extension filter without dots, e.g. [\"rs\", \"md\"]."
            },
            "include_hidden": { "type": "boolean" },
            "max_files": { "type": "integer" },
            "max_file_bytes": { "type": "integer" },
            "max_results": { "type": "integer" }
        }
    })
}

fn index_files(
    settings: &Settings,
    args: &LocalIndexArgs,
    runtime: &mut ToolExecutionRuntime<'_>,
    call: &ToolCall,
) -> Result<ToolResult> {
    let entries = collect_entries(args, runtime)?;
    let index = json!({
        "generated_at": Utc::now().to_rfc3339(),
        "root": args.root,
        "file_count": entries.len(),
        "files": entries,
    });
    let dir = settings.pwcli_home.join("index");
    fs::create_dir_all(&dir)?;
    let path = dir.join("local_files.json");
    fs::write(&path, serde_json::to_vec_pretty(&index)?)?;
    let mut result = ToolResult::ok(format!(
        "Indexed {} files under {}",
        index["file_count"].as_u64().unwrap_or_default(),
        args.root.display()
    ));
    result.metadata = index;
    let file_count = result.metadata["file_count"].clone();
    result = result.add_artifact(ToolArtifact {
        path: path.clone(),
        kind: ToolArtifactKind::Dataset,
        title: Some("Local file index".to_string()),
        media_type: Some("application/json".to_string()),
        preview: Some(format!("{file_count} files")),
        full_content_ref: Some(path.display().to_string()),
        provenance: Some(ToolArtifactProvenance {
            source: "builtin.local_file_index".to_string(),
            uri: None,
            tool_call_id: Some(call.id.clone()),
            metadata: json!({ "root": args.root }),
        }),
    });
    Ok(result)
}

fn stats_files(
    args: &LocalIndexArgs,
    runtime: &mut ToolExecutionRuntime<'_>,
) -> Result<ToolResult> {
    let entries = collect_entries(args, runtime)?;
    let total_bytes = entries
        .iter()
        .filter_map(|entry| entry.get("bytes").and_then(Value::as_u64))
        .sum::<u64>();
    let metadata = json!({
        "root": args.root,
        "file_count": entries.len(),
        "total_bytes": total_bytes,
    });
    let mut result = ToolResult::ok(format!(
        "{} files, {} bytes under {}",
        entries.len(),
        total_bytes,
        args.root.display()
    ));
    result.metadata = metadata;
    Ok(result)
}

fn search_files(
    args: &LocalIndexArgs,
    runtime: &mut ToolExecutionRuntime<'_>,
) -> Result<ToolResult> {
    let query = args.query.as_deref().unwrap_or_default().to_lowercase();
    if query.trim().is_empty() {
        return Err(PwError::ToolExecution(
            "local_file_index search requires query".to_string(),
        ));
    }
    let entries = collect_entries(args, runtime)?;
    let mut matches = Vec::new();
    for entry in entries {
        let path = entry["path"].as_str().unwrap_or_default();
        let mut score = if path.to_lowercase().contains(&query) {
            20
        } else {
            0
        };
        let mut snippets = Vec::new();
        if let Ok((content, _)) = read_text_preview(Path::new(path), args.max_file_bytes) {
            for line in content.lines() {
                if line.to_lowercase().contains(&query) {
                    score += 10;
                    snippets.push(truncate(line.trim(), 240));
                    if snippets.len() >= 3 {
                        break;
                    }
                }
            }
        }
        if score > 0 {
            matches.push(json!({
                "path": path,
                "score": score,
                "snippets": snippets,
                "bytes": entry["bytes"],
            }));
        }
    }
    matches.sort_by(|a, b| b["score"].as_i64().cmp(&a["score"].as_i64()));
    matches.truncate(args.max_results);
    let content = matches
        .iter()
        .map(|item| {
            let snippets = item["snippets"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default();
            format!("{} score={} {}", item["path"], item["score"], snippets)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut result = ToolResult::ok(if content.is_empty() {
        "no matches".to_string()
    } else {
        content
    });
    result.metadata = json!({ "matches": matches });
    Ok(result)
}

fn read_file_preview(args: &LocalIndexArgs) -> Result<ToolResult> {
    let path = args
        .path
        .as_ref()
        .ok_or_else(|| PwError::ToolExecution("local_file_index read requires path".to_string()))?;
    let (content, truncated) = read_text_preview(path, args.max_file_bytes)?;
    let mut result = ToolResult::ok(content.clone()).with_preview(truncate(&content, 2000));
    result.metadata = json!({
        "path": path,
        "truncated": truncated,
        "max_file_bytes": args.max_file_bytes,
    });
    Ok(result)
}

fn collect_entries(
    args: &LocalIndexArgs,
    runtime: &mut ToolExecutionRuntime<'_>,
) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    let root = args
        .root
        .canonicalize()
        .unwrap_or_else(|_| args.root.clone());
    for item in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| include_entry(entry, args.include_hidden))
    {
        if runtime.cancellation().is_cancelled() {
            return Err(PwError::ToolExecution(
                "local file scan cancelled".to_string(),
            ));
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !extension_allowed(path, &args.extensions) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > args.max_file_bytes {
            continue;
        }
        entries.push(json!({
            "path": path.display().to_string(),
            "relative_path": path.strip_prefix(&root).unwrap_or(path).display().to_string(),
            "bytes": metadata.len(),
            "modified": metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
        }));
        if entries.len() >= args.max_files {
            break;
        }
    }
    Ok(entries)
}

fn include_entry(entry: &DirEntry, include_hidden: bool) -> bool {
    let name = entry.file_name().to_string_lossy();
    if !include_hidden && name.starts_with('.') && name != "." {
        return false;
    }
    !matches!(
        name.as_ref(),
        ".git" | "target" | "node_modules" | ".pwcli" | ".next" | "dist" | "build"
    )
}

fn extension_allowed(path: &Path, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    extensions
        .iter()
        .any(|candidate| candidate.trim_start_matches('.').eq_ignore_ascii_case(&ext))
}

fn read_text_preview(path: &Path, max_bytes: u64) -> Result<(String, bool)> {
    let metadata = fs::metadata(path)?;
    let truncated = metadata.len() > max_bytes;
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(max_bytes).read_to_end(&mut bytes)?;
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
