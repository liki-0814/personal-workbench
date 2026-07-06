use crate::{
    settings::{Settings, SshSettings},
    tools::{
        ToolCall, ToolExecutionMode, ToolExecutionRuntime, ToolExecutor, ToolResult,
        ToolRuntimeEvent,
    },
    PwError, Result,
};
use russh::{
    client,
    keys::{
        known_hosts::{
            check_known_hosts, check_known_hosts_path, learn_known_hosts, learn_known_hosts_path,
        },
        load_secret_key,
        ssh_key::PublicKey,
        PrivateKeyWithHashAlg,
    },
    ChannelMsg, Disconnect,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct SshExecExecutor;

#[derive(Debug, Clone, Deserialize)]
pub struct SshExecArgs {
    pub host: String,
    pub command: String,
    #[serde(default, alias = "user")]
    pub username: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub private_key_path: Option<PathBuf>,
    #[serde(default)]
    pub key_passphrase_env: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub known_hosts_path: Option<PathBuf>,
    #[serde(default)]
    pub accept_unknown_host_key: Option<bool>,
    #[serde(default)]
    pub learn_unknown_host_key: Option<bool>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSshTarget {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub cwd: Option<String>,
    pub private_key_path: Option<PathBuf>,
    pub key_passphrase_env: Option<String>,
    pub password_env: Option<String>,
    pub known_hosts_path: Option<PathBuf>,
    pub accept_unknown_host_key: bool,
    pub learn_unknown_host_key: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
struct SshClient {
    host: String,
    port: u16,
    known_hosts_path: Option<PathBuf>,
    accept_unknown_host_key: bool,
    learn_unknown_host_key: bool,
}

#[derive(Debug)]
struct SshCommandOutput {
    stdout: String,
    stderr: String,
    exit_status: Option<u32>,
    duration_ms: u64,
}

impl SshExecExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SshExecExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor for SshExecExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut runtime = ToolExecutionRuntime::noop();
        self.execute_with_runtime(call, &mut runtime)
    }

    fn execute_with_runtime(
        &self,
        call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> Result<ToolResult> {
        let args: SshExecArgs = serde_json::from_value(call.arguments.clone())?;
        let settings = Settings::load()?.ssh;
        let target = resolve_ssh_target(&settings, &args)?;
        validate_ssh_command(&args.command)?;
        let command = remote_command(target.cwd.as_deref(), &args.command);
        let timeout_seconds = target.timeout_seconds.max(1);

        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Streaming,
        });
        runtime.emit(ToolRuntimeEvent::Progress {
            message: format!(
                "connecting to {}@{}:{}",
                target.username, target.host, target.port
            ),
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| {
                PwError::ToolExecution(format!("failed to create SSH runtime: {err}"))
            })?;

        let started = Instant::now();
        let output = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(timeout_seconds),
                run_ssh_command(target.clone(), command.clone(), runtime),
            )
            .await
        });

        match output {
            Ok(Ok(output)) => {
                let success = output.exit_status == Some(0);
                let mut content = String::new();
                if !output.stdout.trim().is_empty() {
                    content.push_str(&output.stdout);
                }
                if !output.stderr.trim().is_empty() {
                    if !content.is_empty() {
                        content.push_str("\n\nstderr:\n");
                    }
                    content.push_str(&output.stderr);
                }
                if content.trim().is_empty() {
                    content = format!("ssh command exited with status {:?}", output.exit_status);
                }
                let mut result = if success {
                    ToolResult::ok(content)
                } else {
                    ToolResult::error(content)
                };
                result.metadata =
                    ssh_metadata(&target, &command, output.exit_status, output.duration_ms);
                result.audit_hints = json!({
                    "remote_execution": true,
                    "requires_completion_callback": true,
                    "host": target.alias,
                    "command_risk": ssh_command_risk(&args.command)
                });
                runtime.emit(ToolRuntimeEvent::Completed {
                    is_error: result.is_error,
                });
                Ok(result)
            }
            Ok(Err(err)) => {
                runtime.emit(ToolRuntimeEvent::Completed { is_error: true });
                let mut result = ToolResult::error(err.to_string());
                result.metadata = json!({
                    "host": target.alias,
                    "remote": format!("{}@{}:{}", target.username, target.host, target.port),
                    "command": command,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "status": "failed"
                });
                Ok(result)
            }
            Err(_) => {
                runtime.emit(ToolRuntimeEvent::TimedOut { timeout_seconds });
                runtime.emit(ToolRuntimeEvent::Completed { is_error: true });
                let mut result =
                    ToolResult::error(format!("ssh command timed out after {timeout_seconds}s"));
                result.metadata = json!({
                    "host": target.alias,
                    "remote": format!("{}@{}:{}", target.username, target.host, target.port),
                    "command": command,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "status": "timed_out"
                });
                Ok(result)
            }
        }
    }
}

impl client::Handler for SshClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let known = if let Some(path) = &self.known_hosts_path {
            check_known_hosts_path(&self.host, self.port, server_public_key, path)
        } else {
            check_known_hosts(&self.host, self.port, server_public_key)
        };

        match known {
            Ok(true) => Ok(true),
            Ok(false) if self.accept_unknown_host_key => {
                if self.learn_unknown_host_key {
                    if let Some(path) = &self.known_hosts_path {
                        learn_known_hosts_path(&self.host, self.port, server_public_key, path)?;
                    } else {
                        learn_known_hosts(&self.host, self.port, server_public_key)?;
                    }
                }
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(err) => Err(err.into()),
        }
    }
}

async fn run_ssh_command(
    target: ResolvedSshTarget,
    command: String,
    runtime: &mut ToolExecutionRuntime<'_>,
) -> Result<SshCommandOutput> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(target.timeout_seconds.max(1))),
        ..Default::default()
    });
    let handler = SshClient {
        host: target.host.clone(),
        port: target.port,
        known_hosts_path: target.known_hosts_path.clone(),
        accept_unknown_host_key: target.accept_unknown_host_key,
        learn_unknown_host_key: target.learn_unknown_host_key,
    };
    let started = Instant::now();
    let mut session = client::connect(config, (target.host.as_str(), target.port), handler)
        .await
        .map_err(|err| PwError::ToolExecution(format!("ssh connect failed: {err}")))?;

    authenticate(&mut session, &target).await?;
    runtime.emit(ToolRuntimeEvent::Progress {
        message: "ssh authenticated; running command".to_string(),
    });

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|err| PwError::ToolExecution(format!("ssh open session failed: {err}")))?;
    channel
        .exec(true, command.as_bytes())
        .await
        .map_err(|err| PwError::ToolExecution(format!("ssh exec failed: {err}")))?;

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit_status = None;
    loop {
        if runtime.cancellation().is_cancelled() {
            runtime.emit(ToolRuntimeEvent::CancelRequested);
            let _ = channel.close().await;
            let _ = session
                .disconnect(Disconnect::ByApplication, "cancelled", "en")
                .await;
            runtime.emit(ToolRuntimeEvent::Cancelled);
            return Err(PwError::ToolExecution("ssh command cancelled".to_string()));
        }

        let msg = tokio::time::timeout(Duration::from_millis(250), channel.wait()).await;
        let Some(msg) = (match msg {
            Ok(msg) => msg,
            Err(_) => continue,
        }) else {
            break;
        };

        match msg {
            ChannelMsg::Data { data } => {
                let chunk = String::from_utf8_lossy(data.as_ref()).to_string();
                stdout.push_str(&chunk);
                runtime.emit(ToolRuntimeEvent::Output {
                    stream: "stdout".to_string(),
                    chunk,
                });
            }
            ChannelMsg::ExtendedData { data, .. } => {
                let chunk = String::from_utf8_lossy(data.as_ref()).to_string();
                stderr.push_str(&chunk);
                runtime.emit(ToolRuntimeEvent::Output {
                    stream: "stderr".to_string(),
                    chunk,
                });
            }
            ChannelMsg::ExitStatus { exit_status: code } => {
                exit_status = Some(code);
            }
            _ => {}
        }
    }

    let _ = session
        .disconnect(Disconnect::ByApplication, "finished", "en")
        .await;

    Ok(SshCommandOutput {
        stdout,
        stderr,
        exit_status,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

async fn authenticate(
    session: &mut client::Handle<SshClient>,
    target: &ResolvedSshTarget,
) -> Result<()> {
    if let Some(path) = &target.private_key_path {
        let passphrase = target
            .key_passphrase_env
            .as_deref()
            .and_then(|name| env::var(name).ok());
        let key_pair = load_secret_key(path, passphrase.as_deref())
            .map_err(|err| PwError::ToolExecution(format!("failed to load SSH key: {err}")))?;
        let rsa_hash = session
            .best_supported_rsa_hash()
            .await
            .map_err(|err| {
                PwError::ToolExecution(format!("ssh RSA hash negotiation failed: {err}"))
            })?
            .flatten();
        let result = session
            .authenticate_publickey(
                target.username.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key_pair), rsa_hash),
            )
            .await
            .map_err(|err| PwError::ToolExecution(format!("ssh public key auth failed: {err}")))?;
        if result.success() {
            return Ok(());
        }
    }

    if let Some(env_name) = &target.password_env {
        let password = env::var(env_name).map_err(|_| {
            PwError::ToolExecution(format!(
                "SSH password env '{env_name}' is not set for host {}",
                target.alias
            ))
        })?;
        let result = session
            .authenticate_password(target.username.clone(), password)
            .await
            .map_err(|err| PwError::ToolExecution(format!("ssh password auth failed: {err}")))?;
        if result.success() {
            return Ok(());
        }
    }

    Err(PwError::ToolExecution(format!(
        "SSH authentication failed for {}@{}:{}",
        target.username, target.host, target.port
    )))
}

pub fn ssh_exec_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "host": {
                "type": "string",
                "description": "Configured SSH host alias from ~/.pwcli/config.json ssh.hosts[].name, or a direct hostname."
            },
            "command": {
                "type": "string",
                "description": "Remote shell command to execute. High-risk commands require approval."
            },
            "username": {
                "type": "string",
                "description": "Optional SSH username override."
            },
            "port": {
                "type": "integer",
                "description": "Optional SSH port override. Defaults to configured host port or 22."
            },
            "cwd": {
                "type": "string",
                "description": "Optional remote working directory. pwcli runs cd <cwd> && command."
            },
            "private_key_path": {
                "type": "string",
                "description": "Optional private key path override. Prefer configuring this in ~/.pwcli/config.json."
            },
            "key_passphrase_env": {
                "type": "string",
                "description": "Env var containing the private key passphrase, if needed."
            },
            "password_env": {
                "type": "string",
                "description": "Env var containing the SSH password. Avoid passing passwords directly."
            },
            "known_hosts_path": {
                "type": "string",
                "description": "Optional known_hosts path override."
            },
            "accept_unknown_host_key": {
                "type": "boolean",
                "description": "Allow unknown host keys. High-risk; use only after approval."
            },
            "learn_unknown_host_key": {
                "type": "boolean",
                "description": "Write accepted unknown host key to known_hosts."
            },
            "timeout_seconds": {
                "type": "integer",
                "description": "Maximum command duration. Defaults to host timeout or 900."
            }
        },
        "required": ["host", "command"]
    })
}

pub fn resolve_ssh_target(settings: &SshSettings, args: &SshExecArgs) -> Result<ResolvedSshTarget> {
    let configured = settings.hosts.iter().find(|host| host.name == args.host);
    let host_name = configured
        .map(|host| host.host.clone())
        .unwrap_or_else(|| args.host.clone());
    let alias = configured
        .map(|host| host.name.clone())
        .unwrap_or_else(|| args.host.clone());
    let username = args
        .username
        .clone()
        .or_else(|| configured.and_then(|host| host.username.clone()))
        .or_else(|| env::var("USER").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PwError::ToolExecution("ssh username is required".to_string()))?;
    let timeout_seconds = args
        .timeout_seconds
        .or_else(|| configured.map(|host| host.timeout_seconds))
        .unwrap_or(900)
        .max(1);

    let target = ResolvedSshTarget {
        alias,
        host: host_name,
        port: args
            .port
            .or_else(|| configured.map(|host| host.port))
            .unwrap_or(22),
        username,
        cwd: args
            .cwd
            .clone()
            .or_else(|| configured.and_then(|host| host.default_cwd.clone())),
        private_key_path: args
            .private_key_path
            .clone()
            .or_else(|| configured.and_then(|host| host.private_key_path.clone())),
        key_passphrase_env: args
            .key_passphrase_env
            .clone()
            .or_else(|| configured.and_then(|host| host.key_passphrase_env.clone())),
        password_env: args
            .password_env
            .clone()
            .or_else(|| configured.and_then(|host| host.password_env.clone())),
        known_hosts_path: args
            .known_hosts_path
            .clone()
            .or_else(|| configured.and_then(|host| host.known_hosts_path.clone())),
        accept_unknown_host_key: args
            .accept_unknown_host_key
            .or_else(|| configured.map(|host| host.accept_unknown_host_key))
            .unwrap_or(false),
        learn_unknown_host_key: args
            .learn_unknown_host_key
            .or_else(|| configured.map(|host| host.learn_unknown_host_key))
            .unwrap_or(false),
        timeout_seconds,
    };

    if target.private_key_path.is_none() && target.password_env.is_none() {
        return Err(PwError::ToolExecution(format!(
            "ssh host '{}' needs private_key_path or password_env",
            args.host
        )));
    }
    Ok(target)
}

pub fn remote_command(cwd: Option<&str>, command: &str) -> String {
    match cwd.filter(|value| !value.trim().is_empty()) {
        Some(cwd) => format!("cd {} && {}", shell_quote(cwd), command),
        None => command.to_string(),
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn validate_ssh_command(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(PwError::ToolExecution(
            "ssh command cannot be empty".to_string(),
        ));
    }
    if command.contains('\0') {
        return Err(PwError::ToolExecution(
            "ssh command cannot contain NUL bytes".to_string(),
        ));
    }
    Ok(())
}

pub fn ssh_command_risk(command: &str) -> &'static str {
    let lower = command.to_ascii_lowercase();
    if [
        "rm -rf",
        "mkfs",
        "dd if=",
        "shutdown",
        "reboot",
        "systemctl restart",
        "systemctl stop",
        "docker rm",
        "docker compose down",
        "kubectl delete",
        "drop table",
        "delete from",
        "truncate ",
        "git reset --hard",
        "chmod -r",
        "chown -r",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "destructive"
    } else if lower.contains("sudo")
        || lower.contains("curl ")
        || lower.contains("| sh")
        || lower.contains("| bash")
        || lower.contains("wget ")
    {
        "elevated"
    } else {
        "normal"
    }
}

fn ssh_metadata(
    target: &ResolvedSshTarget,
    command: &str,
    exit_status: Option<u32>,
    duration_ms: u64,
) -> Value {
    json!({
        "host": target.alias,
        "remote": format!("{}@{}:{}", target.username, target.host, target.port),
        "cwd": target.cwd,
        "command": command,
        "exit_status": exit_status,
        "duration_ms": duration_ms,
        "transport": "russh",
        "host_key": {
            "known_hosts_path": target.known_hosts_path,
            "accept_unknown": target.accept_unknown_host_key,
            "learn_unknown": target.learn_unknown_host_key
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SshHostSettings;

    fn settings() -> SshSettings {
        SshSettings {
            hosts: vec![SshHostSettings {
                name: "dev".to_string(),
                host: "10.0.0.8".to_string(),
                port: 2222,
                username: Some("ubuntu".to_string()),
                private_key_path: Some(PathBuf::from("/tmp/id_ed25519")),
                key_passphrase_env: Some("SSH_KEY_PASSPHRASE".to_string()),
                password_env: None,
                known_hosts_path: Some(PathBuf::from("/tmp/known_hosts")),
                accept_unknown_host_key: true,
                learn_unknown_host_key: true,
                default_cwd: Some("/srv/app".to_string()),
                timeout_seconds: 30,
            }],
        }
    }

    #[test]
    fn resolves_configured_ssh_host_with_overrides() {
        let args = SshExecArgs {
            host: "dev".to_string(),
            command: "uptime".to_string(),
            username: None,
            port: Some(2200),
            cwd: None,
            private_key_path: None,
            key_passphrase_env: None,
            password_env: None,
            known_hosts_path: None,
            accept_unknown_host_key: None,
            learn_unknown_host_key: None,
            timeout_seconds: None,
        };
        let target = resolve_ssh_target(&settings(), &args).unwrap();
        assert_eq!(target.alias, "dev");
        assert_eq!(target.host, "10.0.0.8");
        assert_eq!(target.port, 2200);
        assert_eq!(target.username, "ubuntu");
        assert_eq!(target.cwd.as_deref(), Some("/srv/app"));
        assert_eq!(target.timeout_seconds, 30);
        assert!(target.accept_unknown_host_key);
    }

    #[test]
    fn wraps_remote_command_with_quoted_cwd() {
        assert_eq!(
            remote_command(Some("/srv/app's"), "cargo test"),
            "cd '/srv/app'\\''s' && cargo test"
        );
    }

    #[test]
    fn classifies_destructive_remote_commands() {
        assert_eq!(ssh_command_risk("rm -rf target"), "destructive");
        assert_eq!(ssh_command_risk("sudo systemctl status nginx"), "elevated");
        assert_eq!(ssh_command_risk("uptime"), "normal");
    }
}
