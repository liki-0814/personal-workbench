use crate::{memory::MemorySettings, PwError, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(skip)]
    pub home_dir: PathBuf,
    #[serde(skip)]
    pub pwcli_home: PathBuf,
    #[serde(skip)]
    pub skill_roots: Vec<PathBuf>,
    #[serde(skip)]
    pub max_rounds: u32,
    #[serde(default = "default_provider_name")]
    pub provider: String,
    #[serde(default = "default_local_model_name")]
    pub model: String,
    #[serde(default = "default_true")]
    pub thinking: bool,
    #[serde(default)]
    pub show_thinking: bool,
    #[serde(default)]
    pub context: ContextSettings,
    #[serde(default)]
    pub agents: AgentSettings,
    #[serde(default)]
    pub workflow: WorkflowSettings,
    #[serde(default)]
    pub mineru: MineruSettings,
    #[serde(default)]
    pub anysearch: AnySearchSettings,
    #[serde(default)]
    pub github: GitHubSettings,
    #[serde(default)]
    pub ssh: SshSettings,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub tools: ToolSettings,
    #[serde(default = "default_providers")]
    pub providers: Vec<ProviderSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSettings {
    #[serde(default = "default_context_max_input_tokens")]
    pub max_input_tokens: u32,
    #[serde(default = "default_context_keep_recent_turns")]
    pub keep_recent_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSettings {
    #[serde(default = "default_agent_name")]
    pub default_agent: String,
    #[serde(default = "default_agent_route_defaults")]
    pub route_defaults: BTreeMap<String, String>,
    #[serde(default = "default_agent_profiles")]
    pub profiles: BTreeMap<String, AgentProfileSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfileSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub binary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_agent_effort")]
    pub effort: String,
    #[serde(default = "default_agent_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub mode_overrides: BTreeMap<String, AgentModeOverrideSettings>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentModeOverrideSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yolo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSettings {
    #[serde(default = "default_workflow_kind")]
    pub default_kind: String,
    #[serde(default = "default_true")]
    pub show_planned_graph: bool,
    #[serde(default = "default_true")]
    pub simple_chat_bypass_workflow: bool,
    #[serde(default = "default_auto_route_threshold")]
    pub auto_route_threshold: f32,
    #[serde(default = "default_workflow_max_steps")]
    pub max_steps: usize,
    #[serde(default = "default_true")]
    pub auto_save_runtime_tasks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MineruSettings {
    #[serde(default = "default_mineru_base_url")]
    pub base_url: String,
    #[serde(default, alias = "api_key", skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default = "default_mineru_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnySearchSettings {
    #[serde(default = "default_anysearch_endpoint")]
    pub endpoint: String,
    #[serde(default, alias = "token", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_anysearch_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default)]
    pub rate_limit: AnySearchRateLimitSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubSettings {
    #[serde(default = "default_github_api_url")]
    pub api_url: String,
    #[serde(default, alias = "api_key", skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default = "default_github_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SshSettings {
    #[serde(default)]
    pub hosts: Vec<SshHostSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshHostSettings {
    pub name: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default, alias = "user", skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_passphrase_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_hosts_path: Option<PathBuf>,
    #[serde(default)]
    pub accept_unknown_host_key: bool,
    #[serde(default)]
    pub learn_unknown_host_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    #[serde(default = "default_ssh_timeout_seconds")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnySearchRateLimitSettings {
    #[serde(default = "default_anysearch_max_per_minute")]
    pub max_per_minute: u32,
    #[serde(default = "default_anysearch_max_parallel")]
    pub max_parallel: u32,
    #[serde(default = "default_true")]
    pub retry_on_429: bool,
    #[serde(default = "default_anysearch_max_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpSettings {
    #[serde(default)]
    pub servers: Vec<McpServerSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSettings {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub transport: McpTransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_mcp_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSettings {
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub risk_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub approval_overrides: BTreeMap<String, ToolApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub timeout_seconds: BTreeMap<String, u64>,
    #[serde(default)]
    pub rate_limits: BTreeMap<String, ToolRateLimitSettings>,
    #[serde(default)]
    pub retry: BTreeMap<String, ToolRetrySettings>,
    #[serde(default)]
    pub secrets: BTreeMap<String, ToolSecretRef>,
    #[serde(default)]
    pub network_policy: ToolNetworkPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalMode {
    #[default]
    Policy,
    Always,
    Never,
    Deny,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolNetworkPolicy {
    #[default]
    Allow,
    Deny,
    LocalOnly,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolRateLimitSettings {
    #[serde(default)]
    pub max_per_minute: u32,
    #[serde(default)]
    pub max_parallel: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRetrySettings {
    #[serde(default)]
    pub max_retries: u32,
    #[serde(default)]
    pub backoff_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSecretRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    #[default]
    Stdio,
    Http,
    Sse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub name: String,
    #[serde(default = "default_provider_protocol", alias = "format")]
    pub protocol: ProviderProtocol,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api: OpenAiApiKind,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default)]
    pub extra_body: serde_json::Value,
    #[serde(default)]
    pub models: Vec<ModelDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    pub name: String,
    #[serde(default)]
    pub supports_image_input: bool,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub is_image_generation: bool,
    #[serde(default = "default_context_max_input_tokens")]
    pub max_input_tokens: u32,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub extra_body: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub provider: ProviderProtocol,
    pub provider_name: String,
    pub model: String,
    pub api_key: Option<String>,
    pub api_key_env: String,
    pub base_url: String,
    pub api: OpenAiApiKind,
    pub stream: bool,
    pub max_output_tokens: u32,
    pub request_timeout_seconds: u64,
    pub supports_image_input: bool,
    pub supports_thinking: bool,
    pub thinking_enabled: bool,
    pub is_image_generation: bool,
    pub max_input_tokens: u32,
    pub show_thinking: bool,
    pub extra_body: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[serde(rename = "openai", alias = "open_ai")]
    #[default]
    OpenAi,
    #[serde(rename = "anthropic", alias = "anthrophic")]
    Anthropic,
    #[serde(rename = "nvidia")]
    Nvidia,
}

impl ProviderProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Nvidia => "nvidia",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiApiKind {
    Responses,
    #[default]
    ChatCompletions,
}

impl Settings {
    pub fn from_home(home_dir: impl Into<PathBuf>) -> Self {
        let home_dir = home_dir.into();
        Self {
            pwcli_home: home_dir.join(".pwcli"),
            skill_roots: vec![home_dir.join(".agents/skills")],
            max_rounds: default_max_rounds(),
            provider: default_provider_name(),
            model: default_local_model_name(),
            thinking: true,
            show_thinking: false,
            context: ContextSettings::default(),
            agents: AgentSettings::default(),
            workflow: WorkflowSettings::default(),
            mineru: MineruSettings::default(),
            anysearch: AnySearchSettings::default(),
            github: GitHubSettings::default(),
            ssh: SshSettings::default(),
            memory: MemorySettings::default(),
            mcp: McpSettings::default(),
            tools: ToolSettings::default(),
            providers: default_providers(),
            home_dir,
        }
    }

    pub fn load() -> Result<Self> {
        let home_dir = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::load_from_home(home_dir)
    }

    pub fn load_from_home(home_dir: impl Into<PathBuf>) -> Result<Self> {
        let home_dir = home_dir.into();
        let config_path = home_dir.join(".pwcli/config.json");
        if !config_path.is_file() {
            let mut settings = Self::from_home(home_dir);
            settings.apply_env_overrides();
            settings.normalize();
            return Ok(settings);
        }

        let mut settings: Self = serde_json::from_slice(&fs::read(config_path)?)?;
        let defaults = Self::from_home(&home_dir);
        settings.home_dir = defaults.home_dir;
        settings.pwcli_home = defaults.pwcli_home;
        settings.skill_roots = defaults.skill_roots;
        settings.max_rounds = default_max_rounds();
        if settings.providers.is_empty() {
            settings.providers = defaults.providers;
        }
        settings.apply_env_overrides();
        settings.normalize();
        Ok(settings)
    }

    pub fn save_default(&self) -> Result<()> {
        fs::create_dir_all(&self.pwcli_home)?;
        let path = self.pwcli_home.join("config.json");
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn active_provider(&self) -> Result<&ProviderSettings> {
        self.providers
            .iter()
            .find(|provider| provider.name == self.provider)
            .ok_or_else(|| PwError::Message(format!("unknown provider '{}'", self.provider)))
    }

    pub fn active_provider_mut(&mut self) -> Result<&mut ProviderSettings> {
        let index = self
            .providers
            .iter()
            .position(|provider| provider.name == self.provider)
            .ok_or_else(|| PwError::Message(format!("unknown provider '{}'", self.provider)))?;
        Ok(&mut self.providers[index])
    }

    pub fn active_model(&self) -> Result<&ModelDefinition> {
        let provider = self.active_provider()?;
        provider
            .models
            .iter()
            .find(|model| model.name == self.model)
            .ok_or_else(|| {
                PwError::Message(format!(
                    "unknown model '{}' for provider '{}'",
                    self.model, self.provider
                ))
            })
    }

    pub fn resolved_model_settings(&self) -> Result<ModelSettings> {
        let provider = self.active_provider()?;
        let model = self.active_model()?;
        let thinking_enabled = self.thinking && model.supports_thinking;
        let mut extra_body = merge_json_objects(&provider.extra_body, &model.extra_body);
        apply_thinking_extra_body(provider.protocol, thinking_enabled, &mut extra_body);

        Ok(ModelSettings {
            provider: provider.protocol,
            provider_name: provider.name.clone(),
            model: model.name.clone(),
            api_key: provider.api_key.clone(),
            api_key_env: provider
                .api_key_env
                .clone()
                .unwrap_or_else(|| default_api_key_env(provider.protocol)),
            base_url: provider.base_url.clone(),
            api: provider.api,
            stream: provider.stream,
            max_output_tokens: model.max_output_tokens,
            request_timeout_seconds: provider.request_timeout_seconds,
            supports_image_input: model.supports_image_input,
            supports_thinking: model.supports_thinking,
            thinking_enabled,
            is_image_generation: model.is_image_generation,
            max_input_tokens: model.max_input_tokens.min(self.context.max_input_tokens),
            show_thinking: self.show_thinking,
            extra_body,
        })
    }

    pub fn set_provider(&mut self, name: &str) -> Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.name == name)
            .ok_or_else(|| PwError::Message(format!("unknown provider '{name}'")))?;
        self.provider = provider.name.clone();
        if !provider.models.iter().any(|model| model.name == self.model) {
            self.model = provider
                .models
                .first()
                .map(|model| model.name.clone())
                .ok_or_else(|| PwError::Message(format!("provider '{name}' has no models")))?;
        }
        Ok(())
    }

    pub fn set_model(&mut self, name: &str) -> Result<()> {
        let provider = self.active_provider()?;
        if !provider.models.iter().any(|model| model.name == name) {
            return Err(PwError::Message(format!(
                "unknown model '{name}' for provider '{}'",
                self.provider
            )));
        }
        self.model = name.to_string();
        Ok(())
    }

    pub fn set_active_model_max_input_tokens(&mut self, value: u32) -> Result<()> {
        let model = self.active_model_mut()?;
        model.max_input_tokens = value;
        Ok(())
    }

    pub fn set_active_model_max_output_tokens(&mut self, value: u32) -> Result<()> {
        let model = self.active_model_mut()?;
        model.max_output_tokens = value;
        Ok(())
    }

    pub fn set_context_max_input_tokens(&mut self, value: u32) {
        self.context.max_input_tokens = value.max(1024);
    }

    fn active_model_mut(&mut self) -> Result<&mut ModelDefinition> {
        let model_name = self.model.clone();
        let provider_name = self.provider.clone();
        let provider = self.active_provider_mut()?;
        let index = provider
            .models
            .iter()
            .position(|model| model.name == model_name)
            .ok_or_else(|| {
                PwError::Message(format!(
                    "unknown model '{}' for provider '{}'",
                    model_name, provider_name
                ))
            })?;
        Ok(&mut provider.models[index])
    }

    pub fn set_thinking(&mut self, enabled: bool) {
        self.thinking = enabled;
    }

    pub fn set_show_thinking(&mut self, enabled: bool) {
        self.show_thinking = enabled;
    }

    pub fn agent_for_route(&self, route: &str) -> String {
        self.agents
            .route_defaults
            .get(route)
            .cloned()
            .filter(|agent| self.agents.profiles.contains_key(agent))
            .unwrap_or_else(|| {
                if self
                    .agents
                    .profiles
                    .contains_key(&self.agents.default_agent)
                {
                    self.agents.default_agent.clone()
                } else {
                    default_agent_name()
                }
            })
    }

    pub fn agent_profile(&self, agent: &str) -> Option<&AgentProfileSettings> {
        self.agents.profiles.get(agent)
    }

    pub fn validate_for_save(&self) -> Result<()> {
        let valid_agents = ["codex", "claude", "agy", "qodercli"];
        let valid_routes = ["code", "research", "ops", "general"];
        let valid_modes = ["direct", "goal", "plan", "execute", "review"];
        if !valid_agents.contains(&self.agents.default_agent.as_str()) {
            return Err(PwError::Message(format!(
                "unknown default agent '{}'",
                self.agents.default_agent
            )));
        }
        for (route, agent) in &self.agents.route_defaults {
            if !valid_routes.contains(&route.as_str()) {
                return Err(PwError::Message(format!(
                    "unknown workflow route '{route}'"
                )));
            }
            if !valid_agents.contains(&agent.as_str()) {
                return Err(PwError::Message(format!(
                    "unknown agent '{agent}' for route '{route}'"
                )));
            }
        }
        for (agent, profile) in &self.agents.profiles {
            if !valid_agents.contains(&agent.as_str()) {
                return Err(PwError::Message(format!("unknown agent profile '{agent}'")));
            }
            if profile.timeout_seconds == 0 {
                return Err(PwError::Message(format!(
                    "agent profile '{agent}' timeout_seconds must be positive"
                )));
            }
            for (mode, override_settings) in &profile.mode_overrides {
                if !valid_modes.contains(&mode.as_str()) {
                    return Err(PwError::Message(format!(
                        "unknown mode override '{mode}' for agent '{agent}'"
                    )));
                }
                if override_settings.timeout_seconds == Some(0) {
                    return Err(PwError::Message(format!(
                        "agent profile '{agent}' mode '{mode}' timeout_seconds must be positive"
                    )));
                }
            }
        }
        if !["auto", "chat", "code", "research", "ops", "general"]
            .contains(&self.workflow.default_kind.as_str())
        {
            return Err(PwError::Message(format!(
                "unknown workflow default_kind '{}'",
                self.workflow.default_kind
            )));
        }
        let mut ssh_aliases = BTreeMap::new();
        for host in &self.ssh.hosts {
            if host.name.trim().is_empty() {
                return Err(PwError::Message(
                    "ssh host alias cannot be empty".to_string(),
                ));
            }
            if host.host.trim().is_empty() {
                return Err(PwError::Message(format!(
                    "ssh host '{}' target cannot be empty",
                    host.name
                )));
            }
            if ssh_aliases.insert(host.name.clone(), true).is_some() {
                return Err(PwError::Message(format!(
                    "duplicate ssh host alias '{}'",
                    host.name
                )));
            }
        }
        for server in &self.mcp.servers {
            if server.name.trim().is_empty() {
                return Err(PwError::Message(
                    "mcp server name cannot be empty".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn normalize(&mut self) {
        if self.providers.is_empty() {
            self.providers = default_providers();
        }
        self.agents.normalize();
        self.workflow.normalize();
        for provider in &mut self.providers {
            apply_provider_runtime_defaults(provider);
        }
        for server in &mut self.mcp.servers {
            server.name = server.name.trim().to_string();
            if server.timeout_seconds == 0 {
                server.timeout_seconds = default_mcp_timeout_seconds();
            }
        }
        for host in &mut self.ssh.hosts {
            host.name = host.name.trim().to_string();
            host.host = host.host.trim().to_string();
            if host.port == 0 {
                host.port = default_ssh_port();
            }
            if host.timeout_seconds == 0 {
                host.timeout_seconds = default_ssh_timeout_seconds();
            }
        }
        if !self
            .providers
            .iter()
            .any(|provider| provider.name == self.provider)
        {
            if let Some(provider) = self.providers.first() {
                self.provider = provider.name.clone();
            }
        }
        if self.active_model().is_err() {
            if let Some(provider) = self
                .providers
                .iter()
                .find(|provider| provider.name == self.provider)
            {
                if let Some(model) = provider.models.first() {
                    self.model = model.name.clone();
                }
            }
        }
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(provider) =
            env::var("PWCLI_PROVIDER").or_else(|_| env::var("PWCLI_MODEL_PROVIDER"))
        {
            self.provider = provider;
        }
        if let Ok(model) = env::var("PWCLI_MODEL") {
            self.model = model;
        }
        if let Ok(value) = env::var("PWCLI_CONTEXT_MAX_INPUT_TOKENS") {
            if let Ok(tokens) = value.parse::<u32>() {
                self.context.max_input_tokens = tokens;
            }
        }
        if let Ok(value) = env::var("PWCLI_THINKING") {
            self.thinking = matches!(value.as_str(), "1" | "true" | "yes" | "on");
        }
        if let Ok(value) = env::var("PWCLI_SHOW_THINKING") {
            self.show_thinking = matches!(value.as_str(), "1" | "true" | "yes" | "on");
        }
        if let Ok(token) = env::var("PWCLI_MINERU_TOKEN") {
            self.mineru.token = Some(token);
        }
        if let Ok(base_url) = env::var("PWCLI_MINERU_BASE_URL") {
            self.mineru.base_url = base_url;
        }
        if let Ok(api_key) =
            env::var("ANYSEARCH_API_KEY").or_else(|_| env::var("PWCLI_ANYSEARCH_API_KEY"))
        {
            self.anysearch.api_key = Some(api_key);
        }
        if let Ok(endpoint) = env::var("PWCLI_ANYSEARCH_ENDPOINT") {
            self.anysearch.endpoint = endpoint;
        }
        if let Ok(token) = env::var("PWCLI_GITHUB_TOKEN").or_else(|_| env::var("GITHUB_TOKEN")) {
            self.github.token = Some(token);
        }
        if let Ok(api_url) = env::var("PWCLI_GITHUB_API_URL") {
            self.github.api_url = api_url;
        }
        if let Ok(base_url) = env::var("PWCLI_OPENAI_BASE_URL") {
            if let Some(provider) = self
                .providers
                .iter_mut()
                .find(|provider| provider.protocol == ProviderProtocol::OpenAi)
            {
                provider.base_url = base_url;
            }
        }
        if let Ok(api_key) = env::var("PWCLI_OPENAI_API_KEY") {
            if let Some(provider) = self
                .providers
                .iter_mut()
                .find(|provider| provider.protocol == ProviderProtocol::OpenAi)
            {
                provider.api_key = Some(api_key);
            }
        }
        if let Ok(base_url) = env::var("PWCLI_NVIDIA_BASE_URL") {
            if let Some(provider) = self
                .providers
                .iter_mut()
                .find(|provider| provider.protocol == ProviderProtocol::Nvidia)
            {
                provider.base_url = base_url;
            }
        }
        if let Ok(api_key) = env::var("PWCLI_NVIDIA_API_KEY") {
            if let Some(provider) = self
                .providers
                .iter_mut()
                .find(|provider| provider.protocol == ProviderProtocol::Nvidia)
            {
                provider.api_key = Some(api_key);
            }
        }
    }
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            max_input_tokens: default_context_max_input_tokens(),
            keep_recent_turns: default_context_keep_recent_turns(),
        }
    }
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            default_agent: default_agent_name(),
            route_defaults: default_agent_route_defaults(),
            profiles: default_agent_profiles(),
        }
    }
}

impl AgentSettings {
    fn normalize(&mut self) {
        if self.default_agent.trim().is_empty() {
            self.default_agent = default_agent_name();
        } else {
            self.default_agent = self.default_agent.trim().to_ascii_lowercase();
        }
        let defaults = default_agent_profiles();
        for (agent, profile) in defaults {
            self.profiles.entry(agent).or_insert(profile);
        }
        for (agent, profile) in &mut self.profiles {
            if profile.binary.trim().is_empty() {
                profile.binary = agent.clone();
            }
            if profile.effort.trim().is_empty() {
                profile.effort = default_agent_effort();
            }
            profile.effort = normalize_agent_effort(agent, &profile.effort)
                .unwrap_or_else(|| default_agent_effort());
            for mode in profile.mode_overrides.values_mut() {
                if let Some(effort) = mode.effort.as_deref() {
                    mode.effort = normalize_agent_effort(agent, effort);
                }
            }
            if profile.timeout_seconds == 0 {
                profile.timeout_seconds = default_agent_timeout_seconds();
            }
        }
        let route_defaults = default_agent_route_defaults();
        for (route, agent) in route_defaults {
            self.route_defaults.entry(route).or_insert(agent);
        }
    }
}

impl Default for AgentProfileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: String::new(),
            model: None,
            effort: default_agent_effort(),
            timeout_seconds: default_agent_timeout_seconds(),
            background: false,
            extra_args: Vec::new(),
            mode_overrides: BTreeMap::new(),
        }
    }
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        Self {
            default_kind: default_workflow_kind(),
            show_planned_graph: true,
            simple_chat_bypass_workflow: true,
            auto_route_threshold: default_auto_route_threshold(),
            max_steps: default_workflow_max_steps(),
            auto_save_runtime_tasks: true,
        }
    }
}

impl WorkflowSettings {
    fn normalize(&mut self) {
        if self.default_kind.trim().is_empty() {
            self.default_kind = default_workflow_kind();
        } else {
            self.default_kind = self.default_kind.trim().to_ascii_lowercase();
        }
        if self.max_steps == 0 {
            self.max_steps = default_workflow_max_steps();
        }
        if self.auto_route_threshold <= 0.0 {
            self.auto_route_threshold = default_auto_route_threshold();
        }
    }
}

impl Default for MineruSettings {
    fn default() -> Self {
        Self {
            base_url: default_mineru_base_url(),
            token: None,
            request_timeout_seconds: default_mineru_request_timeout_seconds(),
        }
    }
}

impl Default for AnySearchSettings {
    fn default() -> Self {
        Self {
            endpoint: default_anysearch_endpoint(),
            api_key: None,
            request_timeout_seconds: default_anysearch_request_timeout_seconds(),
            rate_limit: AnySearchRateLimitSettings::default(),
        }
    }
}

impl Default for GitHubSettings {
    fn default() -> Self {
        Self {
            api_url: default_github_api_url(),
            token: None,
            request_timeout_seconds: default_github_timeout_seconds(),
        }
    }
}

impl Default for SshHostSettings {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: default_ssh_port(),
            username: None,
            private_key_path: None,
            key_passphrase_env: None,
            password_env: None,
            known_hosts_path: None,
            accept_unknown_host_key: false,
            learn_unknown_host_key: false,
            default_cwd: None,
            timeout_seconds: default_ssh_timeout_seconds(),
        }
    }
}

impl Default for AnySearchRateLimitSettings {
    fn default() -> Self {
        Self {
            max_per_minute: default_anysearch_max_per_minute(),
            max_parallel: default_anysearch_max_parallel(),
            retry_on_429: true,
            max_retries: default_anysearch_max_retries(),
        }
    }
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            transport: McpTransportKind::Stdio,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            timeout_seconds: default_mcp_timeout_seconds(),
            risk_level: None,
        }
    }
}

impl Default for ToolRetrySettings {
    fn default() -> Self {
        Self {
            max_retries: 0,
            backoff_ms: 500,
        }
    }
}

fn merge_json_objects(base: &serde_json::Value, overlay: &serde_json::Value) -> serde_json::Value {
    let mut merged = base.clone();
    let Some(overlay_obj) = overlay.as_object() else {
        return merged;
    };
    if !merged.is_object() {
        merged = serde_json::json!({});
    }
    let Some(merged_obj) = merged.as_object_mut() else {
        return merged;
    };
    for (key, value) in overlay_obj {
        merged_obj.insert(key.clone(), value.clone());
    }
    merged
}

fn apply_thinking_extra_body(
    protocol: ProviderProtocol,
    thinking_enabled: bool,
    extra_body: &mut serde_json::Value,
) {
    if !extra_body.is_object() {
        *extra_body = serde_json::json!({});
    }

    match protocol {
        ProviderProtocol::Nvidia => {
            extra_body["chat_template_kwargs"] = serde_json::json!({
                "thinking_mode": if thinking_enabled { "enabled" } else { "disabled" }
            });
        }
        ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => {}
    }
}

fn default_providers() -> Vec<ProviderSettings> {
    Vec::new()
}

fn apply_provider_runtime_defaults(provider: &mut ProviderSettings) {
    if provider.api_key_env.is_none() {
        provider.api_key_env = Some(default_api_key_env(provider.protocol));
    }
    if provider.request_timeout_seconds == 0 {
        provider.request_timeout_seconds = match provider.protocol {
            ProviderProtocol::Nvidia => 120,
            ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => {
                default_request_timeout_seconds()
            }
        };
    }

    match provider.protocol {
        ProviderProtocol::OpenAi => {
            if provider.extra_body.is_null() {
                provider.extra_body = serde_json::json!({});
            }
        }
        ProviderProtocol::Anthropic => {
            if provider.extra_body.is_null() {
                provider.extra_body = serde_json::json!({});
            }
        }
        ProviderProtocol::Nvidia => {
            provider.stream = false;
            if provider.extra_body.is_null() {
                provider.extra_body = serde_json::json!({
                    "temperature": 1.0,
                    "top_p": 0.95
                });
            }
        }
    }

    for model in &mut provider.models {
        if model.extra_body.is_null() {
            model.extra_body = serde_json::json!({});
        }
    }
}

fn default_provider_name() -> String {
    String::new()
}

fn default_local_model_name() -> String {
    String::new()
}

fn default_api_key_env(protocol: ProviderProtocol) -> String {
    match protocol {
        ProviderProtocol::OpenAi => "OPENAI_API_KEY".to_string(),
        ProviderProtocol::Anthropic => "ANTHROPIC_API_KEY".to_string(),
        ProviderProtocol::Nvidia => "NVIDIA_API_KEY".to_string(),
    }
}

fn default_provider_protocol() -> ProviderProtocol {
    ProviderProtocol::OpenAi
}

fn default_max_rounds() -> u32 {
    100
}

fn default_max_output_tokens() -> u32 {
    4096
}

fn default_request_timeout_seconds() -> u64 {
    600
}

fn default_true() -> bool {
    true
}

fn default_context_max_input_tokens() -> u32 {
    128_000
}

fn default_context_keep_recent_turns() -> u32 {
    8
}

fn default_agent_name() -> String {
    "codex".to_string()
}

fn default_agent_effort() -> String {
    "high".to_string()
}

fn normalize_agent_effort(agent: &str, effort: &str) -> Option<String> {
    let value = effort.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    let normalized = match agent {
        "claude" if matches!(value.as_str(), "low" | "medium" | "high" | "xhigh" | "max") => value,
        "codex" | "qodercli" if matches!(value.as_str(), "low" | "medium" | "high" | "xhigh") => {
            value
        }
        "codex" | "qodercli" if value == "max" => "xhigh".to_string(),
        "agy" => return None,
        _ if matches!(value.as_str(), "low" | "medium" | "high" | "xhigh") => value,
        _ if value == "max" => "xhigh".to_string(),
        _ => default_agent_effort(),
    };
    Some(normalized)
}

fn default_agent_timeout_seconds() -> u64 {
    900
}

fn default_agent_route_defaults() -> BTreeMap<String, String> {
    [
        ("code", "codex"),
        ("research", "claude"),
        ("ops", "codex"),
        ("general", "codex"),
    ]
    .into_iter()
    .map(|(route, agent)| (route.to_string(), agent.to_string()))
    .collect()
}

fn default_agent_profiles() -> BTreeMap<String, AgentProfileSettings> {
    ["codex", "claude", "agy", "qodercli"]
        .into_iter()
        .map(|agent| {
            (
                agent.to_string(),
                AgentProfileSettings {
                    binary: agent.to_string(),
                    mode_overrides: default_agent_mode_overrides(agent),
                    ..AgentProfileSettings::default()
                },
            )
        })
        .collect()
}

fn default_agent_mode_overrides(agent: &str) -> BTreeMap<String, AgentModeOverrideSettings> {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "execute".to_string(),
        AgentModeOverrideSettings {
            yolo: Some(true),
            ..AgentModeOverrideSettings::default()
        },
    );
    if agent == "claude" {
        overrides.insert(
            "plan".to_string(),
            AgentModeOverrideSettings {
                effort: Some("high".to_string()),
                ..AgentModeOverrideSettings::default()
            },
        );
    }
    overrides
}

fn default_workflow_kind() -> String {
    "auto".to_string()
}

fn default_auto_route_threshold() -> f32 {
    0.5
}

fn default_workflow_max_steps() -> usize {
    64
}

fn default_mineru_base_url() -> String {
    "https://mineru.net".to_string()
}

fn default_mineru_request_timeout_seconds() -> u64 {
    600
}

fn default_anysearch_endpoint() -> String {
    "https://api.anysearch.com/mcp".to_string()
}

fn default_github_api_url() -> String {
    "https://api.github.com".to_string()
}

fn default_github_timeout_seconds() -> u64 {
    30
}

fn default_ssh_port() -> u16 {
    22
}

fn default_ssh_timeout_seconds() -> u64 {
    900
}

fn default_anysearch_request_timeout_seconds() -> u64 {
    30
}

fn default_anysearch_max_per_minute() -> u32 {
    30
}

fn default_anysearch_max_parallel() -> u32 {
    2
}

fn default_anysearch_max_retries() -> u32 {
    3
}

fn default_mcp_timeout_seconds() -> u64 {
    30
}
