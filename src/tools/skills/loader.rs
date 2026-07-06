use crate::Result;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use walkdir::WalkDir;

use super::{executor::JsonExecutableSkillExecutor, manifest::SkillManifest};
use crate::tools::{
    descriptor::{InvocationMode, RiskLevel, ToolDescriptor},
    loader::{LoadedTool, ToolLoader},
};

#[derive(Debug, Clone)]
pub struct SkillToolLoader {
    roots: Vec<PathBuf>,
}

impl SkillToolLoader {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn for_workspace(home_dir: &Path, cwd: &Path) -> Self {
        Self::new(discover_skill_roots(home_dir, cwd))
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

pub fn discover_skill_roots(home_dir: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(home_dir.join(".agents/skills"));
    roots.push(home_dir.join(".claude/skills"));

    for ancestor in cwd.ancestors() {
        roots.push(ancestor.join(".agents/skills"));
        roots.push(ancestor.join(".claude/skills"));
        if ancestor.join(".git").exists() {
            break;
        }
    }

    roots
}

impl ToolLoader for SkillToolLoader {
    fn load(&self) -> Result<Vec<LoadedTool>> {
        let mut loaded = Vec::new();

        for root in &self.roots {
            if !root.is_dir() {
                continue;
            }

            for entry in WalkDir::new(root).min_depth(2).max_depth(2) {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                if entry.file_name().to_string_lossy() != "SKILL.md" {
                    continue;
                }

                let Some(skill_dir) = entry.path().parent() else {
                    continue;
                };

                let manifest = SkillManifest::from_skill_dir(skill_dir)?;
                let mut descriptor = ToolDescriptor::prompt_skill(
                    manifest.tool_id(),
                    manifest.name.clone(),
                    manifest.description.clone(),
                    manifest.path.clone(),
                    manifest.tool_metadata(),
                );

                let executor = if let Some(executable) = manifest.executable.clone() {
                    descriptor.invocation_mode = InvocationMode::ExecutableJson;
                    descriptor.risk_level = RiskLevel::Medium;
                    descriptor.capabilities.push("skill.executable".to_string());
                    let executor: Arc<dyn crate::tools::ToolExecutor> =
                        Arc::new(JsonExecutableSkillExecutor::new(executable));
                    Some(executor)
                } else {
                    None
                };

                loaded.push(LoadedTool {
                    descriptor,
                    executor,
                });
            }
        }

        Ok(loaded)
    }
}
