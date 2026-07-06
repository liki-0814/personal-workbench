use crate::{PwError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableSkill {
    pub command: Vec<String>,
    pub protocol: ExecutableProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutableProtocol {
    JsonStdinStdout,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillResources {
    pub scripts_dir: Option<PathBuf>,
    pub references_dir: Option<PathBuf>,
    pub assets_dir: Option<PathBuf>,
    pub agents_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
    pub metadata: Value,
    pub resources: SkillResources,
    pub executable: Option<ExecutableSkill>,
    pub warnings: Vec<String>,
}

impl SkillManifest {
    pub fn from_skill_dir(skill_dir: impl Into<PathBuf>) -> Result<Self> {
        let skill_dir = skill_dir.into();
        let skill_md = skill_dir.join("SKILL.md");
        let raw = fs::read_to_string(&skill_md).map_err(|err| PwError::InvalidSkill {
            path: skill_dir.display().to_string(),
            message: format!("failed to read SKILL.md: {err}"),
        })?;

        let (frontmatter, body) = split_frontmatter(&raw);
        let metadata = parse_frontmatter(frontmatter)?;
        let mut warnings = Vec::new();

        let name = metadata
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                warnings.push("missing frontmatter name; using directory name".to_string());
                skill_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unnamed-skill")
                    .to_string()
            });

        let description = metadata
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                warnings.push(
                    "missing frontmatter description; using first markdown paragraph".to_string(),
                );
                first_markdown_paragraph(body)
                    .unwrap_or_else(|| "No description provided.".to_string())
            });

        Ok(Self {
            name,
            description,
            body: body.trim().to_string(),
            path: skill_dir.clone(),
            metadata: metadata.clone(),
            resources: SkillResources {
                scripts_dir: existing_dir(skill_dir.join("scripts")),
                references_dir: existing_dir(skill_dir.join("references")),
                assets_dir: existing_dir(skill_dir.join("assets")),
                agents_dir: existing_dir(skill_dir.join("agents")),
            },
            executable: parse_executable(&metadata, &skill_dir),
            warnings,
        })
    }

    pub fn tool_id(&self) -> String {
        format!("skill.{}", self.name)
    }

    pub fn tool_metadata(&self) -> Value {
        json!({
            "skill_path": self.path,
            "body": self.body,
            "resources": self.resources,
            "frontmatter": self.metadata,
            "warnings": self.warnings,
        })
    }
}

fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return (None, raw);
    }

    let rest = &raw[4..];
    if let Some(end) = rest.find("\n---") {
        let frontmatter = &rest[..end];
        let body_start = end + "\n---".len();
        let body = rest[body_start..].trim_start_matches(['\n', '\r']);
        (Some(frontmatter), body)
    } else {
        (None, raw)
    }
}

fn parse_frontmatter(frontmatter: Option<&str>) -> Result<Value> {
    let Some(frontmatter) = frontmatter else {
        return Ok(Value::Object(Map::new()));
    };
    let yaml: serde_yaml::Value = serde_yaml::from_str(frontmatter)?;
    Ok(serde_json::to_value(yaml)?)
}

fn first_markdown_paragraph(body: &str) -> Option<String> {
    body.split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with('#'))
        .map(str::to_string)
}

fn existing_dir(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}

fn parse_executable(metadata: &Value, skill_dir: &std::path::Path) -> Option<ExecutableSkill> {
    let executable = metadata.get("executable")?;
    let command = executable
        .get("command")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(|arg| expand_skill_path(arg, skill_dir))
        .collect::<Vec<_>>();

    if command.is_empty() {
        return None;
    }

    Some(ExecutableSkill {
        command,
        protocol: ExecutableProtocol::JsonStdinStdout,
    })
}

fn expand_skill_path(arg: &str, skill_dir: &std::path::Path) -> String {
    arg.replace("${skill_dir}", &skill_dir.display().to_string())
}
