//! Configuration loading for proxy endpoints and runtime settings.

use std::{collections::BTreeMap, env, fs, path::Path};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::error::{ProxyError, Result};

/// Environment variable pointing to a TOML/YAML config file.
pub const CONFIG_PATH_ENV: &str = "LLM_PROXY_CONFIG";
/// Environment variable overriding the HTTP listen address.
pub const LISTEN_ADDR_ENV: &str = "LLM_PROXY_ADDR";
/// Default address used when neither a file nor the environment provides one.
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
/// Environment variable for the temporary byte-for-byte passthrough route.
pub const PASSTHROUGH_UPSTREAM_URL_ENV: &str = "LLM_PROXY_UPSTREAM_URL";
/// Environment variable replacing the whole backend list with JSON.
pub const BACKENDS_ENV: &str = "LLM_PROXY_BACKENDS";
/// Environment variable replacing the whole model alias map with JSON.
pub const MODEL_ALIASES_ENV: &str = "LLM_PROXY_MODEL_ALIASES";
/// Environment variable enabling/disabling Anthropic cache-control injection.
pub const CACHE_INJECTION_ENV: &str = "LLM_PROXY_ANTHROPIC_CACHE_INJECTION";
/// Environment variable enabling the optional reasoning-store fallback.
pub const REASONING_STORE_ENV: &str = "LLM_PROXY_REASONING_STORE";
/// Environment variable enabling redacted request/response body dumps in debug logs.
pub const OBSERVABILITY_DUMP_ENV: &str = "LLM_PROXY_OBSERVABILITY_DUMP";
/// Environment variable overriding backend retry count.
pub const BACKEND_MAX_RETRIES_ENV: &str = "LLM_PROXY_BACKEND_MAX_RETRIES";
/// Environment variable overriding the first backend retry delay in milliseconds.
pub const BACKEND_INITIAL_BACKOFF_MS_ENV: &str = "LLM_PROXY_BACKEND_INITIAL_BACKOFF_MS";
/// Environment variable overriding the maximum backend retry delay in milliseconds.
pub const BACKEND_MAX_BACKOFF_MS_ENV: &str = "LLM_PROXY_BACKEND_MAX_BACKOFF_MS";
/// Environment variable overriding the per-attempt backend request timeout in milliseconds.
pub const BACKEND_TIMEOUT_MS_ENV: &str = "LLM_PROXY_BACKEND_TIMEOUT_MS";
/// Environment variable overriding the maximum number of simultaneous backend requests.
pub const BACKEND_CONCURRENCY_LIMIT_ENV: &str = "LLM_PROXY_BACKEND_CONCURRENCY_LIMIT";
/// Environment variable overriding the Chat Completions endpoint.
pub const CHAT_COMPLETIONS_URL_ENV: &str = "LLM_PROXY_CHAT_COMPLETIONS_URL";
/// Environment variable with the DeepSeek/OpenAI-compatible Chat API key.
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
/// Environment variable with the OpenAI Responses-compatible endpoint.
pub const OPENAI_API_ENDPOINT_ENV: &str = "OPENAI_API_ENDPOINT";
/// Environment variable with the OpenAI Responses-compatible API key.
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
/// Environment variable selecting `/v1/messages` backend behavior before M7-02 routing.
pub const ANTHROPIC_MESSAGES_BACKEND_ENV: &str = "LLM_PROXY_ANTHROPIC_MESSAGES_BACKEND";
/// Environment variable selecting `/v1/responses` backend behavior before M7-02 routing.
pub const RESPONSES_BACKEND_ENV: &str = "LLM_PROXY_RESPONSES_BACKEND";
/// Environment variable with the Anthropic-compatible backend base URL.
pub const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";
/// Environment variable with the Anthropic-compatible backend credential.
pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
/// Environment variable with the Anthropic API version header value.
pub const ANTHROPIC_VERSION_ENV: &str = "ANTHROPIC_VERSION";
/// Environment variable with the default Anthropic backend model for Responses clients.
pub const ANTHROPIC_DEFAULT_OPUS_MODEL_ENV: &str = "ANTHROPIC_DEFAULT_OPUS_MODEL";
/// Environment variable with the default Anthropic max token limit.
pub const ANTHROPIC_DEFAULT_MAX_TOKENS_ENV: &str = "LLM_PROXY_ANTHROPIC_DEFAULT_MAX_TOKENS";
/// Default Anthropic version used by current Messages-compatible backends.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Default max token limit used when Anthropic clients omit `max_tokens`.
pub const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = 4096;

const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// Runtime configuration after file parsing and environment overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen_addr: String,
    pub passthrough_upstream_url: Option<String>,
    pub anthropic_default_max_tokens: Option<u32>,
    pub backends: Vec<BackendConfig>,
    pub model_aliases: BTreeMap<String, ModelAlias>,
    #[serde(alias = "features")]
    pub switches: SwitchConfig,
    #[serde(alias = "upstream_request")]
    pub backend_request: BackendRequestConfig,
    #[serde(alias = "route_overrides")]
    pub routing: RoutingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_LISTEN_ADDR.to_owned(),
            passthrough_upstream_url: None,
            anthropic_default_max_tokens: None,
            backends: Vec::new(),
            model_aliases: BTreeMap::new(),
            switches: SwitchConfig::default(),
            backend_request: BackendRequestConfig::default(),
            routing: RoutingConfig::default(),
        }
    }
}

impl Config {
    /// Loads configuration from `LLM_PROXY_CONFIG` plus environment overrides.
    pub fn load() -> Result<Self> {
        Self::load_with_env(env::vars())
    }

    /// Loads configuration from a specific TOML/YAML file without process env overrides.
    #[cfg(test)]
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Self::read_config_path(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Parses a TOML config string and validates the resulting structure.
    #[cfg(test)]
    pub fn from_toml_str(source: &str) -> Result<Self> {
        let mut config = parse_toml_config(source, "inline TOML config")?;
        config.validate()?;
        Ok(config)
    }

    /// Parses a YAML config string and validates the resulting structure.
    #[cfg(test)]
    pub fn from_yaml_str(source: &str) -> Result<Self> {
        let mut config = parse_yaml_config(source, "inline YAML config")?;
        config.validate()?;
        Ok(config)
    }

    /// Returns a backend by name.
    pub fn backend(&self, name: &str) -> Option<&BackendConfig> {
        self.backends.iter().find(|backend| backend.name == name)
    }

    /// Returns the first configured backend of the requested type.
    pub fn first_backend_of_kind(&self, kind: BackendKind) -> Option<&BackendConfig> {
        self.backends
            .iter()
            .find(|backend| backend.kind == Some(kind))
    }

    fn load_with_env<I, K, V>(env: I) -> Result<Self>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let env = env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<BTreeMap<_, _>>();
        let mut config = if let Some(path) = env_value(&env, CONFIG_PATH_ENV) {
            Self::read_config_path(path)?
        } else {
            Self::default()
        };
        apply_env_overrides(&mut config, &env)?;
        config.validate()?;
        Ok(config)
    }

    fn read_config_path(path: impl AsRef<Path>) -> Result<Config> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|err| {
            ProxyError::Config(format!(
                "failed to read config file `{}`: {err}",
                path.display()
            ))
        })?;
        parse_config_source(path, &source)
    }

    fn validate(&mut self) -> Result<()> {
        self.listen_addr = required_trimmed("listen_addr", &self.listen_addr)?;
        if self.anthropic_default_max_tokens == Some(0) {
            return Err(config_error(
                "anthropic_default_max_tokens must be greater than zero",
            ));
        }
        self.backend_request.validate()?;

        let mut backend_names = std::collections::BTreeSet::new();
        for backend in &mut self.backends {
            backend.validate()?;
            if !backend_names.insert(backend.name.clone()) {
                return Err(config_error(format!(
                    "duplicate backend name `{}`",
                    backend.name
                )));
            }
        }

        for (alias, target) in &mut self.model_aliases {
            if alias.trim().is_empty() {
                return Err(config_error("model alias name must not be empty"));
            }
            target.validate(alias, &backend_names)?;
        }

        validate_route_override(
            "routing.anthropic_messages_backend",
            self.routing.anthropic_messages_backend.as_deref(),
            &[
                "auto",
                "chat",
                "deepseek",
                "deepseek-chat",
                "responses",
                "openai",
                "openai-responses",
            ],
        )?;
        validate_route_override(
            "routing.responses_backend",
            self.routing.responses_backend.as_deref(),
            &[
                "auto",
                "chat",
                "deepseek",
                "deepseek-chat",
                "anthropic",
                "claude",
                "anthropic-messages",
            ],
        )?;

        Ok(())
    }
}

/// A configured upstream backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BackendConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: Option<BackendKind>,
    pub base_url: Option<String>,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub profile: Option<ProfileKind>,
    pub anthropic_version: Option<String>,
    pub default_model: Option<String>,
    pub default_max_tokens: Option<u32>,
}

impl BackendConfig {
    /// Returns the concrete Chat Completions endpoint for a chat backend.
    pub fn chat_completions_url(&self) -> Option<String> {
        self.endpoint.clone().or_else(|| {
            self.base_url
                .as_ref()
                .map(|base_url| append_path(base_url, "chat/completions"))
        })
    }

    /// Returns the concrete Responses endpoint for a Responses backend.
    pub fn responses_endpoint(&self) -> Option<String> {
        self.endpoint.clone().or_else(|| {
            self.base_url
                .as_ref()
                .map(|base_url| append_path(base_url, "responses"))
        })
    }

    /// Returns the Anthropic base URL or concrete Messages endpoint.
    pub fn anthropic_endpoint_base(&self) -> Option<String> {
        self.endpoint.clone().or_else(|| self.base_url.clone())
    }

    fn validate(&mut self) -> Result<()> {
        self.name = required_trimmed("backend.name", &self.name)?;
        let kind = self.kind.ok_or_else(|| {
            config_error(format!(
                "backend `{}` is missing required `type`",
                self.name
            ))
        })?;
        let profile = self.profile.ok_or_else(|| {
            config_error(format!(
                "backend `{}` is missing required `profile`",
                self.name
            ))
        })?;
        validate_profile_for_backend(self.name.as_str(), kind, profile)?;

        self.base_url = normalize_optional_string(self.base_url.take());
        self.endpoint = normalize_optional_string(self.endpoint.take());
        self.api_key = normalize_optional_string(self.api_key.take());
        self.anthropic_version = normalize_optional_string(self.anthropic_version.take());
        self.default_model = normalize_optional_string(self.default_model.take());

        if let Some(base_url) = self.base_url.as_deref() {
            validate_url(format!("backend `{}` base_url", self.name), base_url)?;
        }
        if let Some(endpoint) = self.endpoint.as_deref() {
            validate_url(format!("backend `{}` endpoint", self.name), endpoint)?;
        }

        match kind {
            BackendKind::Chat => {
                if self.chat_completions_url().is_none() {
                    return Err(config_error(format!(
                        "chat backend `{}` requires `base_url` or `endpoint`",
                        self.name
                    )));
                }
            }
            BackendKind::Responses => {
                if self.responses_endpoint().is_none() {
                    return Err(config_error(format!(
                        "responses backend `{}` requires `base_url` or `endpoint`",
                        self.name
                    )));
                }
                require_backend_api_key(self)?;
            }
            BackendKind::Anthropic => {
                if self.anthropic_endpoint_base().is_none() {
                    return Err(config_error(format!(
                        "anthropic backend `{}` requires `base_url` or `endpoint`",
                        self.name
                    )));
                }
                require_backend_api_key(self)?;
                if self.anthropic_version.is_none() {
                    self.anthropic_version = Some(DEFAULT_ANTHROPIC_VERSION.to_owned());
                }
            }
        }

        if self.default_max_tokens == Some(0) {
            return Err(config_error(format!(
                "backend `{}` default_max_tokens must be greater than zero",
                self.name
            )));
        }

        Ok(())
    }
}

/// Supported upstream protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[serde(alias = "openai_chat", alias = "chat_completions")]
    Chat,
    #[serde(alias = "openai_responses")]
    Responses,
    #[serde(alias = "anthropic_messages")]
    Anthropic,
}

/// Capability profile selected for a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    #[serde(alias = "deepseek")]
    DeepSeek,
    #[serde(alias = "generic", alias = "openai", alias = "generic_openai")]
    GenericOpenAi,
    #[serde(alias = "anthropic_messages")]
    Anthropic,
}

/// Client-facing model alias to backend target mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelAlias {
    pub backend: String,
    #[serde(alias = "rename", alias = "upstream_model")]
    pub model: String,
}

impl ModelAlias {
    fn validate(
        &mut self,
        alias: &str,
        backend_names: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        self.backend = required_trimmed(format!("model_aliases.{alias}.backend"), &self.backend)?;
        self.model = required_trimmed(format!("model_aliases.{alias}.model"), &self.model)?;
        if !backend_names.contains(&self.backend) {
            return Err(config_error(format!(
                "model alias `{alias}` references unknown backend `{}`",
                self.backend
            )));
        }
        Ok(())
    }
}

/// Feature switches that do not change protocol routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SwitchConfig {
    pub anthropic_cache_injection: bool,
    pub reasoning_store: bool,
    pub observability_dump: bool,
}

impl Default for SwitchConfig {
    fn default() -> Self {
        Self {
            anthropic_cache_injection: true,
            reasoning_store: false,
            observability_dump: false,
        }
    }
}

/// Runtime controls applied to outbound backend HTTP requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendRequestConfig {
    pub max_retries: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub timeout_ms: Option<u64>,
    pub concurrency_limit: Option<usize>,
}

impl Default for BackendRequestConfig {
    fn default() -> Self {
        Self {
            max_retries: 0,
            initial_backoff_ms: 250,
            max_backoff_ms: 5_000,
            timeout_ms: None,
            concurrency_limit: None,
        }
    }
}

impl BackendRequestConfig {
    fn validate(&self) -> Result<()> {
        if self.initial_backoff_ms == 0 {
            return Err(config_error(
                "backend_request.initial_backoff_ms must be greater than zero",
            ));
        }
        if self.max_backoff_ms == 0 {
            return Err(config_error(
                "backend_request.max_backoff_ms must be greater than zero",
            ));
        }
        if self.max_backoff_ms < self.initial_backoff_ms {
            return Err(config_error(
                "backend_request.max_backoff_ms must be greater than or equal to initial_backoff_ms",
            ));
        }
        if self.timeout_ms == Some(0) {
            return Err(config_error(
                "backend_request.timeout_ms must be greater than zero when set",
            ));
        }
        if self.concurrency_limit == Some(0) {
            return Err(config_error(
                "backend_request.concurrency_limit must be greater than zero when set",
            ));
        }
        Ok(())
    }
}

/// Legacy route-level overrides used when no exact model alias matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoutingConfig {
    pub anthropic_messages_backend: Option<String>,
    pub responses_backend: Option<String>,
}

fn parse_config_source(path: &Path, source: &str) -> Result<Config> {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return Err(config_error(format!(
            "config file `{}` must use .toml, .yaml, or .yml",
            path.display()
        )));
    };

    match extension.to_ascii_lowercase().as_str() {
        "toml" => parse_toml_config(source, path.display().to_string()),
        "yaml" | "yml" => parse_yaml_config(source, path.display().to_string()),
        _ => Err(config_error(format!(
            "config file `{}` must use .toml, .yaml, or .yml",
            path.display()
        ))),
    }
}

fn parse_toml_config(source: &str, label: impl AsRef<str>) -> Result<Config> {
    toml::from_str(source)
        .map_err(|err| config_error(format!("failed to parse {}: {err}", label.as_ref())))
}

fn parse_yaml_config(source: &str, label: impl AsRef<str>) -> Result<Config> {
    serde_yaml::from_str(source)
        .map_err(|err| config_error(format!("failed to parse {}: {err}", label.as_ref())))
}

fn apply_env_overrides(config: &mut Config, env: &BTreeMap<String, String>) -> Result<()> {
    if let Some(listen_addr) = env_value(env, LISTEN_ADDR_ENV) {
        config.listen_addr = listen_addr;
    }
    if let Some(upstream_url) = env_value(env, PASSTHROUGH_UPSTREAM_URL_ENV) {
        config.passthrough_upstream_url = Some(upstream_url);
    }
    if let Some(backends) = env_value(env, BACKENDS_ENV) {
        config.backends = parse_json_env(BACKENDS_ENV, &backends)?;
    }
    if let Some(model_aliases) = env_value(env, MODEL_ALIASES_ENV) {
        config.model_aliases = parse_json_env(MODEL_ALIASES_ENV, &model_aliases)?;
    }
    if let Some(cache_injection) = env_value(env, CACHE_INJECTION_ENV) {
        config.switches.anthropic_cache_injection =
            parse_bool_env(CACHE_INJECTION_ENV, &cache_injection)?;
    }
    if let Some(reasoning_store) = env_value(env, REASONING_STORE_ENV) {
        config.switches.reasoning_store = parse_bool_env(REASONING_STORE_ENV, &reasoning_store)?;
    }
    if let Some(observability_dump) = env_value(env, OBSERVABILITY_DUMP_ENV) {
        config.switches.observability_dump =
            parse_bool_env(OBSERVABILITY_DUMP_ENV, &observability_dump)?;
    }
    if let Some(max_retries) = env_value(env, BACKEND_MAX_RETRIES_ENV) {
        config.backend_request.max_retries = parse_u32_env(BACKEND_MAX_RETRIES_ENV, &max_retries)?;
    }
    if let Some(initial_backoff_ms) = env_value(env, BACKEND_INITIAL_BACKOFF_MS_ENV) {
        config.backend_request.initial_backoff_ms =
            parse_u64_env(BACKEND_INITIAL_BACKOFF_MS_ENV, &initial_backoff_ms)?;
    }
    if let Some(max_backoff_ms) = env_value(env, BACKEND_MAX_BACKOFF_MS_ENV) {
        config.backend_request.max_backoff_ms =
            parse_u64_env(BACKEND_MAX_BACKOFF_MS_ENV, &max_backoff_ms)?;
    }
    if let Some(timeout_ms) = env_value(env, BACKEND_TIMEOUT_MS_ENV) {
        config.backend_request.timeout_ms =
            Some(parse_u64_env(BACKEND_TIMEOUT_MS_ENV, &timeout_ms)?);
    }
    if let Some(concurrency_limit) = env_value(env, BACKEND_CONCURRENCY_LIMIT_ENV) {
        config.backend_request.concurrency_limit = Some(parse_usize_env(
            BACKEND_CONCURRENCY_LIMIT_ENV,
            &concurrency_limit,
        )?);
    }
    if let Some(backend) = env_value(env, ANTHROPIC_MESSAGES_BACKEND_ENV) {
        config.routing.anthropic_messages_backend = Some(backend);
    }
    if let Some(backend) = env_value(env, RESPONSES_BACKEND_ENV) {
        config.routing.responses_backend = Some(backend);
    }

    apply_legacy_backend_env(config, env)?;

    Ok(())
}

fn apply_legacy_backend_env(config: &mut Config, env: &BTreeMap<String, String>) -> Result<()> {
    let chat_endpoint = env_value(env, CHAT_COMPLETIONS_URL_ENV);
    let chat_api_key = env_value(env, DEEPSEEK_API_KEY_ENV);
    if chat_endpoint.is_some() || chat_api_key.is_some() {
        let backend =
            ensure_named_backend(config, "deepseek", BackendKind::Chat, ProfileKind::DeepSeek)?;
        if backend.base_url.is_none() && backend.endpoint.is_none() {
            backend.base_url = Some(DEFAULT_DEEPSEEK_BASE_URL.to_owned());
        }
        if let Some(endpoint) = chat_endpoint {
            backend.endpoint = Some(endpoint);
        }
        if let Some(api_key) = chat_api_key {
            backend.api_key = Some(api_key);
        }
    }

    let responses_endpoint = env_value(env, OPENAI_API_ENDPOINT_ENV);
    let responses_api_key = env_value(env, OPENAI_API_KEY_ENV);
    if responses_endpoint.is_some() {
        let backend = ensure_named_backend(
            config,
            "responses",
            BackendKind::Responses,
            ProfileKind::GenericOpenAi,
        )?;
        backend.endpoint = responses_endpoint;
        if let Some(api_key) = responses_api_key {
            backend.api_key = Some(api_key);
        }
    } else if let Some(api_key) = responses_api_key
        && let Some(index) = backend_index(config, "responses", BackendKind::Responses)
    {
        config.backends[index].api_key = Some(api_key);
    }

    let anthropic_base_url = env_value(env, ANTHROPIC_BASE_URL_ENV);
    let anthropic_auth_token = env_value(env, ANTHROPIC_AUTH_TOKEN_ENV);
    if anthropic_base_url.is_some() {
        let backend = ensure_named_backend(
            config,
            "anthropic",
            BackendKind::Anthropic,
            ProfileKind::Anthropic,
        )?;
        backend.base_url = anthropic_base_url;
        if let Some(api_key) = anthropic_auth_token {
            backend.api_key = Some(api_key);
        }
    } else if let Some(api_key) = anthropic_auth_token
        && let Some(index) = backend_index(config, "anthropic", BackendKind::Anthropic)
    {
        config.backends[index].api_key = Some(api_key);
    }
    if let Some(max_tokens) = env_value(env, ANTHROPIC_DEFAULT_MAX_TOKENS_ENV) {
        let parsed = max_tokens.parse::<u32>().map_err(|err| {
            config_error(format!(
                "invalid {ANTHROPIC_DEFAULT_MAX_TOKENS_ENV} `{max_tokens}`: {err}"
            ))
        })?;
        config.anthropic_default_max_tokens = Some(parsed);
    }

    if let Some(index) = backend_index(config, "anthropic", BackendKind::Anthropic) {
        if let Some(version) = env_value(env, ANTHROPIC_VERSION_ENV) {
            config.backends[index].anthropic_version = Some(version);
        }
        if let Some(model) = env_value(env, ANTHROPIC_DEFAULT_OPUS_MODEL_ENV) {
            config.backends[index].default_model = Some(model);
        }
    }

    Ok(())
}

fn ensure_named_backend<'a>(
    config: &'a mut Config,
    name: &str,
    kind: BackendKind,
    profile: ProfileKind,
) -> Result<&'a mut BackendConfig> {
    if let Some(index) = config
        .backends
        .iter()
        .position(|backend| backend.name == name)
    {
        let backend = &mut config.backends[index];
        validate_env_backend_kind(name, backend.kind, kind)?;
        backend.kind = Some(kind);
        if backend.profile.is_none() {
            backend.profile = Some(profile);
        }
        return Ok(backend);
    }

    config.backends.push(BackendConfig {
        name: name.to_owned(),
        kind: Some(kind),
        profile: Some(profile),
        ..BackendConfig::default()
    });
    Ok(config
        .backends
        .last_mut()
        .expect("backend was just inserted"))
}

fn backend_index(config: &Config, preferred_name: &str, kind: BackendKind) -> Option<usize> {
    config
        .backends
        .iter()
        .position(|backend| backend.name == preferred_name)
        .or_else(|| {
            config
                .backends
                .iter()
                .position(|backend| backend.kind == Some(kind))
        })
}

fn validate_env_backend_kind(
    name: &str,
    existing: Option<BackendKind>,
    expected: BackendKind,
) -> Result<()> {
    if let Some(existing) = existing
        && existing != expected
    {
        return Err(config_error(format!(
            "environment override for `{name}` expected a {expected:?} backend, but existing backend is {existing:?}"
        )));
    }
    Ok(())
}

fn parse_json_env<T: DeserializeOwned>(name: &str, value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|err| config_error(format!("invalid {name}: {err}")))
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(config_error(format!(
            "{name} must be one of true/false/1/0/yes/no/on/off, got `{value}`"
        ))),
    }
}

fn parse_u32_env(name: &str, value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|err| config_error(format!("invalid {name} `{value}`: {err}")))
}

fn parse_u64_env(name: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|err| config_error(format!("invalid {name} `{value}`: {err}")))
}

fn parse_usize_env(name: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|err| config_error(format!("invalid {name} `{value}`: {err}")))
}

fn validate_profile_for_backend(name: &str, kind: BackendKind, profile: ProfileKind) -> Result<()> {
    let valid = matches!(
        (kind, profile),
        (
            BackendKind::Chat,
            ProfileKind::DeepSeek | ProfileKind::GenericOpenAi
        ) | (BackendKind::Responses, ProfileKind::GenericOpenAi)
            | (BackendKind::Anthropic, ProfileKind::Anthropic)
    );
    if valid {
        Ok(())
    } else {
        Err(config_error(format!(
            "backend `{name}` profile `{profile:?}` is not valid for `{kind:?}`"
        )))
    }
}

fn require_backend_api_key(backend: &BackendConfig) -> Result<()> {
    if backend.api_key.is_none() {
        return Err(config_error(format!(
            "{} backend `{}` requires `api_key`",
            backend
                .kind
                .map(|kind| format!("{kind:?}").to_ascii_lowercase())
                .unwrap_or_else(|| "configured".to_owned()),
            backend.name
        )));
    }
    Ok(())
}

fn validate_route_override(label: &str, value: Option<&str>, allowed: &[&str]) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = value.trim().to_ascii_lowercase();
    if allowed.contains(&value.as_str()) {
        Ok(())
    } else {
        Err(config_error(format!(
            "{label} must be one of {}, got `{value}`",
            allowed.join(", ")
        )))
    }
}

fn validate_url(label: impl AsRef<str>, value: &str) -> Result<()> {
    reqwest::Url::parse(value)
        .map(|_| ())
        .map_err(|err| config_error(format!("invalid {} `{value}`: {err}", label.as_ref())))
}

fn required_trimmed(label: impl AsRef<str>, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(config_error(format!(
            "{} must not be empty",
            label.as_ref()
        )))
    } else {
        Ok(trimmed.to_owned())
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

fn env_value(env: &BTreeMap<String, String>, name: &str) -> Option<String> {
    env.get(name).and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

fn append_path(base_url: &str, suffix: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/{suffix}")
    }
}

fn config_error(message: impl Into<String>) -> ProxyError {
    ProxyError::Config(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_config_loads_backends_aliases_and_switches() {
        let config = Config::from_toml_str(
            r#"
listen_addr = "127.0.0.1:19090"
passthrough_upstream_url = "http://127.0.0.1:9100/sse"

[switches]
anthropic_cache_injection = false
reasoning_store = true
observability_dump = true

[backend_request]
max_retries = 2
initial_backoff_ms = 10
max_backoff_ms = 100
timeout_ms = 30000
concurrency_limit = 8

[routing]
anthropic_messages_backend = "responses"
responses_backend = "anthropic"

[[backends]]
name = "deepseek"
type = "chat"
base_url = "https://api.deepseek.com"
api_key = "deepseek-test-key"
profile = "deep_seek"

[[backends]]
name = "responses"
type = "responses"
endpoint = "https://responses.example/v1/responses"
api_key = "responses-test-key"
profile = "generic_open_ai"

[model_aliases."codex-default"]
backend = "responses"
model = "gpt-5.1"
"#,
        )
        .unwrap();

        assert_eq!(config.listen_addr, "127.0.0.1:19090");
        assert_eq!(
            config.passthrough_upstream_url.as_deref(),
            Some("http://127.0.0.1:9100/sse")
        );
        assert!(!config.switches.anthropic_cache_injection);
        assert!(config.switches.reasoning_store);
        assert!(config.switches.observability_dump);
        assert_eq!(config.backend_request.max_retries, 2);
        assert_eq!(config.backend_request.initial_backoff_ms, 10);
        assert_eq!(config.backend_request.max_backoff_ms, 100);
        assert_eq!(config.backend_request.timeout_ms, Some(30_000));
        assert_eq!(config.backend_request.concurrency_limit, Some(8));
        assert_eq!(
            config.routing.anthropic_messages_backend.as_deref(),
            Some("responses")
        );
        assert_eq!(config.backends.len(), 2);
        assert_eq!(
            config.backend("responses").unwrap().responses_endpoint(),
            Some("https://responses.example/v1/responses".to_owned())
        );
        assert_eq!(
            config.model_aliases["codex-default"].model,
            "gpt-5.1".to_owned()
        );
    }

    #[test]
    fn yaml_config_loads_anthropic_backend_defaults() {
        let config = Config::from_yaml_str(
            r#"
backends:
  - name: anthropic
    type: anthropic
    base_url: https://anthropic.example
    api_key: anthropic-test-key
    profile: anthropic
    default_model: claude-opus-test
    default_max_tokens: 2048
"#,
        )
        .unwrap();

        let backend = config.backend("anthropic").unwrap();
        assert_eq!(
            backend.anthropic_version.as_deref(),
            Some(DEFAULT_ANTHROPIC_VERSION)
        );
        assert_eq!(backend.default_model.as_deref(), Some("claude-opus-test"));
        assert_eq!(backend.default_max_tokens, Some(2048));
    }

    #[test]
    fn from_path_detects_toml_extension() {
        let path = std::env::temp_dir().join(format!(
            "llm-proxy-config-test-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        std::fs::write(
            &path,
            r#"
listen_addr = "127.0.0.1:17777"
"#,
        )
        .unwrap();

        let config = Config::from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(config.listen_addr, "127.0.0.1:17777");
    }

    #[test]
    fn env_overrides_file_values_and_creates_legacy_backends() {
        let env = [
            (LISTEN_ADDR_ENV, "127.0.0.1:18080"),
            (DEEPSEEK_API_KEY_ENV, "deepseek-env-key"),
            (
                OPENAI_API_ENDPOINT_ENV,
                "https://responses-env.example/v1/responses",
            ),
            (OPENAI_API_KEY_ENV, "responses-env-key"),
            (ANTHROPIC_BASE_URL_ENV, "https://anthropic-env.example"),
            (ANTHROPIC_AUTH_TOKEN_ENV, "anthropic-env-token"),
            (ANTHROPIC_DEFAULT_OPUS_MODEL_ENV, "claude-env"),
            (ANTHROPIC_DEFAULT_MAX_TOKENS_ENV, "8192"),
            (CACHE_INJECTION_ENV, "off"),
            (REASONING_STORE_ENV, "yes"),
            (OBSERVABILITY_DUMP_ENV, "true"),
            (BACKEND_MAX_RETRIES_ENV, "3"),
            (BACKEND_INITIAL_BACKOFF_MS_ENV, "5"),
            (BACKEND_MAX_BACKOFF_MS_ENV, "50"),
            (BACKEND_TIMEOUT_MS_ENV, "15000"),
            (BACKEND_CONCURRENCY_LIMIT_ENV, "4"),
            (
                MODEL_ALIASES_ENV,
                r#"{"sonnet":{"backend":"anthropic","model":"claude-env"}}"#,
            ),
        ];

        let config = Config::load_with_env(env).unwrap();

        assert_eq!(config.listen_addr, "127.0.0.1:18080");
        assert!(!config.switches.anthropic_cache_injection);
        assert!(config.switches.reasoning_store);
        assert!(config.switches.observability_dump);
        assert_eq!(config.backend_request.max_retries, 3);
        assert_eq!(config.backend_request.initial_backoff_ms, 5);
        assert_eq!(config.backend_request.max_backoff_ms, 50);
        assert_eq!(config.backend_request.timeout_ms, Some(15_000));
        assert_eq!(config.backend_request.concurrency_limit, Some(4));
        assert_eq!(config.anthropic_default_max_tokens, Some(8192));
        assert_eq!(
            config.backend("deepseek").unwrap().api_key.as_deref(),
            Some("deepseek-env-key")
        );
        assert_eq!(
            config.backend("responses").unwrap().api_key.as_deref(),
            Some("responses-env-key")
        );
        let anthropic = config.backend("anthropic").unwrap();
        assert_eq!(
            anthropic.anthropic_endpoint_base().as_deref(),
            Some("https://anthropic-env.example")
        );
        assert_eq!(anthropic.default_model.as_deref(), Some("claude-env"));
        assert_eq!(config.model_aliases["sonnet"].backend, "anthropic");
    }

    #[test]
    fn env_backend_json_replaces_file_backend_list() {
        let env = [
            (
                BACKENDS_ENV,
                r#"[{"name":"deepseek","type":"chat","base_url":"https://api.deepseek.com","profile":"deep_seek"}]"#,
            ),
            (DEEPSEEK_API_KEY_ENV, "deepseek-env-key"),
        ];

        let config = Config::load_with_env(env).unwrap();

        assert_eq!(config.backends.len(), 1);
        assert_eq!(
            config.backend("deepseek").unwrap().profile,
            Some(ProfileKind::DeepSeek)
        );
        assert_eq!(
            config.backend("deepseek").unwrap().api_key.as_deref(),
            Some("deepseek-env-key")
        );
    }

    #[test]
    fn validation_rejects_duplicate_backend_names() {
        let err = Config::from_toml_str(
            r#"
[[backends]]
name = "dup"
type = "chat"
base_url = "https://one.example"
profile = "generic_open_ai"

[[backends]]
name = "dup"
type = "chat"
base_url = "https://two.example"
profile = "generic_open_ai"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate backend name `dup`"));
    }

    #[test]
    fn validation_rejects_alias_to_unknown_backend() {
        let err = Config::from_toml_str(
            r#"
[model_aliases."missing"]
backend = "nope"
model = "gpt-test"
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("model alias `missing` references unknown backend `nope`")
        );
    }

    #[test]
    fn validation_rejects_missing_backend_profile() {
        let err = Config::from_toml_str(
            r#"
[[backends]]
name = "deepseek"
type = "chat"
base_url = "https://api.deepseek.com"
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("backend `deepseek` is missing required `profile`")
        );
    }

    #[test]
    fn validation_rejects_invalid_backend_request_limits() {
        let err = Config::from_toml_str(
            r#"
[backend_request]
initial_backoff_ms = 100
max_backoff_ms = 50
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("max_backoff_ms must be greater than or equal")
        );

        let err = Config::from_toml_str(
            r#"
[backend_request]
concurrency_limit = 0
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("concurrency_limit must be greater than zero")
        );
    }
}
