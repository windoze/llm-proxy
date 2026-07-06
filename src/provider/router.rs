//! Model-to-backend routing based on configured aliases and frontend endpoint.

use std::{fmt, sync::Arc, time::Instant};

use tracing::warn;

use crate::{
    config::{BackendConfig, BackendKind, Config, ModelAlias, ProfileKind},
    error::{ProxyError, Result},
};

use super::{CapabilityProfile, GenericOpenAi, deepseek::DeepSeek, failover::FailoverRegistry};

/// Client-facing endpoint family used to constrain valid upstream routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendEndpoint {
    AnthropicMessages,
    OpenAiResponses,
}

impl FrontendEndpoint {
    fn path(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "/v1/messages",
            Self::OpenAiResponses => "/v1/responses",
        }
    }

    fn supports_backend(self, kind: BackendKind) -> bool {
        matches!(
            (self, kind),
            (
                Self::AnthropicMessages,
                BackendKind::Chat | BackendKind::Responses
            ) | (
                Self::OpenAiResponses,
                BackendKind::Chat | BackendKind::Anthropic
            )
        )
    }

    fn supported_backend_label(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "chat or responses",
            Self::OpenAiResponses => "chat or anthropic",
        }
    }

    fn implicit_preference(self, requested_model: &str) -> &'static [BackendKind] {
        match (self, is_deepseek_model(requested_model)) {
            (_, true) => &[BackendKind::Chat],
            (Self::AnthropicMessages, false) => &[BackendKind::Responses, BackendKind::Chat],
            (Self::OpenAiResponses, false) => &[BackendKind::Anthropic, BackendKind::Chat],
        }
    }
}

impl fmt::Display for FrontendEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.path())
    }
}

/// Router owning the validated runtime configuration used for model lookup.
#[derive(Debug, Clone)]
pub struct ModelRouter {
    config: Config,
    failover: Arc<FailoverRegistry>,
}

impl ModelRouter {
    /// Creates a router and preserves the legacy DeepSeek default when no chat backend is configured.
    pub fn new(mut config: Config) -> Self {
        ensure_default_chat_backend(&mut config);
        let failover = Arc::new(FailoverRegistry::from_config(&config));
        Self { config, failover }
    }

    /// Returns the names of all configured backends (including the implicit DeepSeek default).
    pub fn backend_names(&self) -> Vec<String> {
        self.config
            .backends
            .iter()
            .map(|backend| backend.name.clone())
            .collect()
    }

    /// Returns each model alias with its currently active backend target (reflecting failover).
    ///
    /// Aliases are returned sorted by name for stable output.
    pub fn active_alias_targets(&self) -> Vec<(String, String, String)> {
        self.config
            .model_aliases
            .iter()
            .filter_map(|(alias, model_alias)| {
                let targets = model_alias.targets();
                let last_index = targets.len().checked_sub(1)?;
                let active_index = self.failover.current_index(alias).min(last_index);
                let target = &targets[active_index];
                Some((alias.clone(), target.backend.clone(), target.model.clone()))
            })
            .collect()
    }

    /// Resolves a client model for a specific frontend endpoint.
    pub fn route(
        &self,
        endpoint: FrontendEndpoint,
        requested_model: &str,
    ) -> Result<ModelRoute<'_>> {
        let requested_model = requested_model.trim();
        if requested_model.is_empty() {
            return Err(config_error(format!(
                "model route for {endpoint} requires a non-empty model name"
            )));
        }

        if let Some(alias) = self.config.model_aliases.get(requested_model) {
            return self.route_alias(endpoint, requested_model, alias);
        }

        if let Some(kind) = self.legacy_override_kind(endpoint)? {
            let backend = self
                .config
                .first_backend_of_kind(kind)
                .ok_or_else(|| missing_override_backend_error(endpoint, requested_model, kind))?;
            return self.build_route(
                endpoint,
                requested_model,
                implicit_model(backend, requested_model),
                backend,
            );
        }

        for kind in endpoint.implicit_preference(requested_model) {
            if let Some(backend) = self.config.first_backend_of_kind(*kind) {
                return self.build_route(
                    endpoint,
                    requested_model,
                    implicit_model(backend, requested_model),
                    backend,
                );
            }
        }

        Err(no_route_error(endpoint, requested_model))
    }

    fn route_alias(
        &self,
        endpoint: FrontendEndpoint,
        requested_model: &str,
        alias: &ModelAlias,
    ) -> Result<ModelRoute<'_>> {
        let targets = alias.targets();
        let active_index = self.failover.current_index(requested_model).min(
            targets
                .len()
                .checked_sub(1)
                .expect("validated alias always has at least one target"),
        );
        let target = &targets[active_index];
        let backend = self.config.backend(&target.backend).ok_or_else(|| {
            config_error(format!(
                "model alias `{requested_model}` references missing backend `{}`",
                target.backend
            ))
        })?;
        let mut route = self.build_route(endpoint, requested_model, target.model.clone(), backend)?;
        if targets.len() > 1 {
            route.failover = Some(FailoverReport {
                alias: requested_model.to_owned(),
                active_index,
                registry: Arc::clone(&self.failover),
            });
        }
        Ok(route)
    }

    fn legacy_override_kind(&self, endpoint: FrontendEndpoint) -> Result<Option<BackendKind>> {
        let configured = match endpoint {
            FrontendEndpoint::AnthropicMessages => {
                self.config.routing.anthropic_messages_backend.as_deref()
            }
            FrontendEndpoint::OpenAiResponses => self.config.routing.responses_backend.as_deref(),
        };
        let Some(configured) = configured else {
            return Ok(None);
        };

        match configured.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(None),
            "chat" | "deepseek" | "deepseek-chat" => Ok(Some(BackendKind::Chat)),
            "responses" | "openai" | "openai-responses"
                if endpoint == FrontendEndpoint::AnthropicMessages =>
            {
                Ok(Some(BackendKind::Responses))
            }
            "anthropic" | "claude" | "anthropic-messages"
                if endpoint == FrontendEndpoint::OpenAiResponses =>
            {
                Ok(Some(BackendKind::Anthropic))
            }
            _ => Err(config_error(format!(
                "routing override `{configured}` cannot serve {endpoint}; expected {}",
                endpoint.supported_backend_label()
            ))),
        }
    }

    fn build_route<'a>(
        &'a self,
        endpoint: FrontendEndpoint,
        requested_model: &str,
        backend_model: String,
        backend: &'a BackendConfig,
    ) -> Result<ModelRoute<'a>> {
        let kind = backend.kind.ok_or_else(|| {
            config_error(format!(
                "backend `{}` selected for model `{requested_model}` is missing `type`",
                backend.name
            ))
        })?;
        if !endpoint.supports_backend(kind) {
            return Err(config_error(format!(
                "model `{requested_model}` resolves to {} backend `{}`, which cannot serve {endpoint}; expected {} backend",
                backend_kind_label(kind),
                backend.name,
                endpoint.supported_backend_label()
            )));
        }

        let profile = backend.profile.ok_or_else(|| {
            config_error(format!(
                "backend `{}` selected for model `{requested_model}` is missing `profile`",
                backend.name
            ))
        })?;
        let backend_model = backend_model.trim();
        if backend_model.is_empty() {
            return Err(config_error(format!(
                "backend model for `{requested_model}` on backend `{}` must not be empty",
                backend.name
            )));
        }

        Ok(ModelRoute {
            endpoint,
            requested_model: requested_model.to_owned(),
            backend_model: backend_model.to_owned(),
            backend,
            kind,
            profile,
            failover: None,
        })
    }
}

/// Handle for reporting a failed request against a multi-target alias.
#[derive(Debug, Clone)]
struct FailoverReport {
    alias: String,
    active_index: usize,
    registry: Arc<FailoverRegistry>,
}

/// Resolved model route with the selected backend and rewritten upstream model name.
#[derive(Debug)]
pub struct ModelRoute<'a> {
    endpoint: FrontendEndpoint,
    requested_model: String,
    backend_model: String,
    backend: &'a BackendConfig,
    kind: BackendKind,
    profile: ProfileKind,
    failover: Option<FailoverReport>,
}

impl<'a> ModelRoute<'a> {
    /// Returns the selected backend.
    pub fn backend(&self) -> &'a BackendConfig {
        self.backend
    }

    /// Returns the selected upstream protocol family.
    pub fn backend_kind(&self) -> BackendKind {
        self.kind
    }

    /// Returns the selected capability profile kind.
    pub fn profile_kind(&self) -> ProfileKind {
        self.profile
    }

    /// Returns the client-facing endpoint that was routed.
    pub fn endpoint(&self) -> FrontendEndpoint {
        self.endpoint
    }

    /// Returns the client-requested model name.
    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    /// Returns the model name that should be sent to the selected backend.
    pub fn backend_model(&self) -> &str {
        &self.backend_model
    }

    /// Reports that the request on this route failed, possibly advancing the alias failover target.
    ///
    /// No-op for single-target aliases and implicit/legacy routes. When the alias's failover policy
    /// is satisfied, the shared registry advances to the next backend for subsequent requests; the
    /// current request is unaffected.
    pub fn report_failure(&self) {
        let Some(report) = self.failover.as_ref() else {
            return;
        };
        if let Some(new_index) = report.registry.record_failure(&report.alias, Instant::now()) {
            warn!(
                alias = %report.alias,
                from_index = report.active_index,
                to_index = new_index,
                "failover switched model alias to next backend target"
            );
        }
    }

    /// Builds the OpenAI Chat-compatible capability profile for this route.
    pub fn chat_profile(&self) -> Result<ChatProfile> {
        if self.kind != BackendKind::Chat {
            return Err(config_error(format!(
                "{:?} backend `{}` does not use an OpenAI Chat capability profile",
                self.kind, self.backend.name
            )));
        }

        match self.profile {
            ProfileKind::DeepSeek => Ok(ChatProfile::DeepSeek(DeepSeek)),
            ProfileKind::GenericOpenAi => Ok(ChatProfile::GenericOpenAi(
                self.backend
                    .base_url
                    .clone()
                    .map(GenericOpenAi::new)
                    .unwrap_or_default(),
            )),
            ProfileKind::Anthropic => Err(config_error(format!(
                "anthropic profile is not valid for chat backend `{}`",
                self.backend.name
            ))),
        }
    }
}

/// Capability profile selected for an OpenAI Chat-compatible route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatProfile {
    DeepSeek(DeepSeek),
    GenericOpenAi(GenericOpenAi),
}

impl CapabilityProfile for ChatProfile {
    fn param_blocklist(&self, model: &str) -> &[&str] {
        match self {
            Self::DeepSeek(profile) => profile.param_blocklist(model),
            Self::GenericOpenAi(profile) => profile.param_blocklist(model),
        }
    }

    fn normalize_reasoning_effort<'a>(&self, effort: &'a str) -> &'a str {
        match self {
            Self::DeepSeek(profile) => profile.normalize_reasoning_effort(effort),
            Self::GenericOpenAi(profile) => profile.normalize_reasoning_effort(effort),
        }
    }

    fn reasoning_echo_policy(&self, model: &str) -> crate::ir::message::EchoPolicy {
        match self {
            Self::DeepSeek(profile) => profile.reasoning_echo_policy(model),
            Self::GenericOpenAi(profile) => profile.reasoning_echo_policy(model),
        }
    }

    fn supports_multiple_choices(&self) -> bool {
        match self {
            Self::DeepSeek(profile) => profile.supports_multiple_choices(),
            Self::GenericOpenAi(profile) => profile.supports_multiple_choices(),
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::DeepSeek(profile) => profile.base_url(),
            Self::GenericOpenAi(profile) => profile.base_url(),
        }
    }

    fn map_model_name(&self, requested: &str) -> String {
        match self {
            Self::DeepSeek(profile) => profile.map_model_name(requested),
            Self::GenericOpenAi(profile) => profile.map_model_name(requested),
        }
    }

    fn thinking_model(&self, model: &str) -> bool {
        match self {
            Self::DeepSeek(profile) => profile.thinking_model(model),
            Self::GenericOpenAi(profile) => profile.thinking_model(model),
        }
    }
}

fn ensure_default_chat_backend(config: &mut Config) {
    if config
        .backends
        .iter()
        .any(|backend| backend.kind == Some(BackendKind::Chat))
    {
        return;
    }

    let deepseek = DeepSeek;
    config.backends.push(BackendConfig {
        name: "deepseek".to_owned(),
        kind: Some(BackendKind::Chat),
        base_url: Some(deepseek.base_url().to_owned()),
        profile: Some(ProfileKind::DeepSeek),
        ..BackendConfig::default()
    });
}

fn implicit_model(backend: &BackendConfig, requested_model: &str) -> String {
    backend
        .default_model
        .clone()
        .unwrap_or_else(|| requested_model.to_owned())
}

fn is_deepseek_model(model: &str) -> bool {
    model.starts_with("deepseek-")
}

fn missing_override_backend_error(
    endpoint: FrontendEndpoint,
    requested_model: &str,
    kind: BackendKind,
) -> ProxyError {
    config_error(format!(
        "routing override for {endpoint} selects {} for model `{requested_model}`, but no {} backend is configured",
        backend_kind_label(kind),
        backend_kind_label(kind)
    ))
}

fn no_route_error(endpoint: FrontendEndpoint, requested_model: &str) -> ProxyError {
    config_error(format!(
        "no backend route for {endpoint} model `{requested_model}`; add a model_aliases entry or configure a compatible {} backend",
        endpoint.supported_backend_label()
    ))
}

fn config_error(message: impl Into<String>) -> ProxyError {
    ProxyError::Config(message.into())
}

fn backend_kind_label(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Chat => "chat",
        BackendKind::Responses => "responses",
        BackendKind::Anthropic => "anthropic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_selects_backend_profile_and_rewrites_model() {
        let config = Config::from_toml_str(
            r#"
[[backends]]
name = "deepseek"
type = "chat"
base_url = "https://api.deepseek.com"
profile = "deep_seek"

[model_aliases."claude-code-default"]
backend = "deepseek"
model = "deepseek-chat"
"#,
        )
        .unwrap();

        let router = ModelRouter::new(config);
        let route = router
            .route(FrontendEndpoint::AnthropicMessages, "claude-code-default")
            .unwrap();

        assert_eq!(route.backend().name, "deepseek");
        assert_eq!(route.backend_kind(), BackendKind::Chat);
        assert_eq!(route.profile_kind(), ProfileKind::DeepSeek);
        assert_eq!(route.backend_model(), "deepseek-chat");
        assert_eq!(route.requested_model(), "claude-code-default");
        assert!(!route.chat_profile().unwrap().supports_multiple_choices());
    }

    #[test]
    fn alias_rejects_backend_that_cannot_serve_endpoint() {
        let config = Config::from_toml_str(
            r#"
[[backends]]
name = "anthropic"
type = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"
profile = "anthropic"

[model_aliases."claude-direct"]
backend = "anthropic"
model = "claude-sonnet-4-5"
"#,
        )
        .unwrap();

        let err = ModelRouter::new(config)
            .route(FrontendEndpoint::AnthropicMessages, "claude-direct")
            .unwrap_err();

        assert!(err.to_string().contains("cannot serve /v1/messages"));
        assert!(err.to_string().contains("chat or responses"));
    }

    #[test]
    fn implicit_responses_endpoint_uses_anthropic_default_model() {
        let config = Config::from_toml_str(
            r#"
[[backends]]
name = "anthropic"
type = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"
profile = "anthropic"
default_model = "claude-sonnet-4-5"
"#,
        )
        .unwrap();

        let router = ModelRouter::new(config);
        let route = router
            .route(FrontendEndpoint::OpenAiResponses, "gpt-5.5")
            .unwrap();

        assert_eq!(route.backend().name, "anthropic");
        assert_eq!(route.backend_kind(), BackendKind::Anthropic);
        assert_eq!(route.backend_model(), "claude-sonnet-4-5");
    }

    #[test]
    fn implicit_anthropic_endpoint_prefers_responses_for_non_deepseek_models() {
        let config = Config::from_toml_str(
            r#"
[[backends]]
name = "responses"
type = "responses"
endpoint = "https://responses.example/v1/responses"
api_key = "responses-key"
profile = "generic_open_ai"
"#,
        )
        .unwrap();

        let router = ModelRouter::new(config);
        let route = router
            .route(FrontendEndpoint::AnthropicMessages, "gpt-5.1")
            .unwrap();

        assert_eq!(route.backend().name, "responses");
        assert_eq!(route.backend_kind(), BackendKind::Responses);
        assert_eq!(route.backend_model(), "gpt-5.1");
    }

    #[test]
    fn legacy_override_reports_missing_selected_backend() {
        let config = Config::from_toml_str(
            r#"
[routing]
anthropic_messages_backend = "responses"

[[backends]]
name = "anthropic"
type = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"
profile = "anthropic"
"#,
        )
        .unwrap();

        let err = ModelRouter::new(config)
            .route(FrontendEndpoint::AnthropicMessages, "gpt-5.1")
            .unwrap_err();

        assert!(err.to_string().contains("selects responses"));
        assert!(
            err.to_string()
                .contains("no responses backend is configured")
        );
    }

    #[test]
    fn default_deepseek_chat_backend_preserves_envless_legacy_routing() {
        let router = ModelRouter::new(Config::default());
        let route = router
            .route(FrontendEndpoint::OpenAiResponses, "deepseek-chat")
            .unwrap();

        assert_eq!(route.backend().name, "deepseek");
        assert_eq!(
            route.backend().chat_completions_url().as_deref(),
            Some("https://api.deepseek.com/chat/completions")
        );
        assert_eq!(route.backend_model(), "deepseek-chat");
    }
}
