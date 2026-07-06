use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

use super::{loader::SkillToolLoader, manifest::SkillManifest};
use crate::tools::{ToolLoader, ToolSource};

#[derive(Debug, Clone)]
pub struct SkillWatchPlan {
    pub roots: Vec<PathBuf>,
    pub poll_interval_ms: u64,
}

impl SkillWatchPlan {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            poll_interval_ms: 1_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInventory {
    pub fingerprint: u64,
    pub roots: Vec<PathBuf>,
    pub tool_ids: Vec<String>,
    pub conflicts: Vec<SkillConflict>,
    pub health: Vec<SkillHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillConflict {
    pub tool_id: String,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHealth {
    pub path: PathBuf,
    pub status: SkillHealthStatus,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillHealthStatus {
    Ok,
    Warn,
    Fail,
}

pub fn scan_skill_roots(roots: &[PathBuf]) -> Result<SkillInventory> {
    let mut fingerprint = SkillFingerprint::default();
    let mut health = Vec::new();
    let mut ids_by_path = BTreeMap::<String, Vec<PathBuf>>::new();

    for root in roots {
        root.hash(&mut fingerprint);
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root).min_depth(1).max_depth(3) {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            fingerprint_path(path, &mut fingerprint);
            if entry.file_name().to_string_lossy() != "SKILL.md" {
                continue;
            }
            let Some(skill_dir) = path.parent() else {
                continue;
            };
            match SkillManifest::from_skill_dir(skill_dir) {
                Ok(manifest) => {
                    let tool_id = manifest.tool_id();
                    ids_by_path
                        .entry(tool_id.clone())
                        .or_default()
                        .push(manifest.path.clone());
                    let status = if manifest.warnings.is_empty() {
                        SkillHealthStatus::Ok
                    } else {
                        SkillHealthStatus::Warn
                    };
                    health.push(SkillHealth {
                        path: manifest.path,
                        status,
                        message: if manifest.warnings.is_empty() {
                            "loaded".to_string()
                        } else {
                            manifest.warnings.join("; ")
                        },
                        tool_id: Some(tool_id),
                    });
                }
                Err(err) => health.push(SkillHealth {
                    path: skill_dir.to_path_buf(),
                    status: SkillHealthStatus::Fail,
                    message: err.to_string(),
                    tool_id: None,
                }),
            }
        }
    }

    let loaded = SkillToolLoader::new(roots.to_vec()).load()?;
    for tool in &loaded {
        if let ToolSource::Skill { path } = &tool.descriptor.source {
            ids_by_path
                .entry(tool.descriptor.id.clone())
                .or_default()
                .push(path.clone());
        }
    }
    let mut tool_ids = loaded
        .iter()
        .map(|tool| tool.descriptor.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    tool_ids.sort();

    let conflicts = ids_by_path
        .into_iter()
        .filter_map(|(tool_id, mut paths)| {
            paths.sort();
            paths.dedup();
            (paths.len() > 1).then_some(SkillConflict { tool_id, paths })
        })
        .collect();

    Ok(SkillInventory {
        fingerprint: fingerprint.finish(),
        roots: roots.to_vec(),
        tool_ids,
        conflicts,
        health,
    })
}

#[derive(Default)]
struct SkillFingerprint {
    state: std::collections::hash_map::DefaultHasher,
}

impl SkillFingerprint {
    fn finish(self) -> u64 {
        self.state.finish()
    }
}

impl Hasher for SkillFingerprint {
    fn finish(&self) -> u64 {
        self.state.finish()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.state.write(bytes);
    }
}

fn fingerprint_path(path: &std::path::Path, hasher: &mut SkillFingerprint) {
    path.hash(hasher);
    if let Ok(metadata) = fs::metadata(path) {
        metadata.len().hash(hasher);
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                duration.as_nanos().hash(hasher);
            }
        }
    }
}
