use crate::Result;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    pub pwcli_home: PathBuf,
    pub audit_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub tasks_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub rules_dir: PathBuf,
    pub models_dir: PathBuf,
}

impl WorkspacePaths {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        let pwcli_home = home_dir.into().join(".pwcli");
        Self {
            audit_dir: pwcli_home.join("audit"),
            sessions_dir: pwcli_home.join("sessions"),
            tasks_dir: pwcli_home.join("tasks"),
            cache_dir: pwcli_home.join("cache"),
            memory_dir: pwcli_home.join("memory"),
            rules_dir: pwcli_home.join("rules"),
            models_dir: pwcli_home.join("models"),
            pwcli_home,
        }
    }

    pub fn from_pwcli_home(pwcli_home: impl Into<PathBuf>) -> Self {
        let pwcli_home = pwcli_home.into();
        Self {
            audit_dir: pwcli_home.join("audit"),
            sessions_dir: pwcli_home.join("sessions"),
            tasks_dir: pwcli_home.join("tasks"),
            cache_dir: pwcli_home.join("cache"),
            memory_dir: pwcli_home.join("memory"),
            rules_dir: pwcli_home.join("rules"),
            models_dir: pwcli_home.join("models"),
            pwcli_home,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.pwcli_home)?;
        fs::create_dir_all(&self.audit_dir)?;
        fs::create_dir_all(&self.sessions_dir)?;
        fs::create_dir_all(&self.tasks_dir)?;
        fs::create_dir_all(&self.cache_dir)?;
        fs::create_dir_all(&self.memory_dir)?;
        fs::create_dir_all(&self.rules_dir)?;
        fs::create_dir_all(&self.models_dir)?;
        Ok(())
    }
}

pub fn append_jsonl(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    file.write_all(&line)?;
    Ok(())
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = temp_json_path(path);
    let bytes = serde_json::to_vec_pretty(value)?;
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn temp_json_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("value.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce))
}
