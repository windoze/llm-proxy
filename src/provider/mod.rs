//! Provider integration and capability profiles.
//!
//! Concrete provider behavior is added after the core IR is defined.

// Later M1 tasks wire provider profiles into request decoding and routing.
#![allow(dead_code)]

use crate::ir::message::EchoPolicy;

pub mod anthropic_backend;
pub mod anthropic_cache;
pub mod deepseek;
pub mod responses_backend;

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const EMPTY_PARAM_BLOCKLIST: &[&str] = &[];

/// Provider-specific behavior for OpenAI-compatible upstream APIs.
pub trait CapabilityProfile {
    /// Returns request parameter names that should be silently dropped.
    fn param_blocklist(&self, model: &str) -> &[&str];

    /// Canonicalizes a requested reasoning effort for the upstream provider.
    fn normalize_reasoning_effort<'a>(&self, effort: &'a str) -> &'a str;

    /// Returns the policy for echoing reasoning content from this provider.
    fn reasoning_echo_policy(&self, model: &str) -> EchoPolicy;

    /// Reports whether the provider supports `n > 1` response choices.
    fn supports_multiple_choices(&self) -> bool;

    /// Returns the upstream API base URL.
    fn base_url(&self) -> &str;

    /// Maps a client-requested model name to the upstream model name.
    fn map_model_name(&self, requested: &str) -> String;

    /// Reports whether the model should use provider thinking/reasoning mode.
    fn thinking_model(&self, model: &str) -> bool;
}

/// Default profile for standard OpenAI-compatible chat providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericOpenAi {
    base_url: String,
}

impl GenericOpenAi {
    /// Creates a generic OpenAI-compatible profile with a custom base URL.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for GenericOpenAi {
    fn default() -> Self {
        Self::new(DEFAULT_OPENAI_BASE_URL)
    }
}

impl CapabilityProfile for GenericOpenAi {
    fn param_blocklist(&self, _model: &str) -> &[&str] {
        EMPTY_PARAM_BLOCKLIST
    }

    fn normalize_reasoning_effort<'a>(&self, effort: &'a str) -> &'a str {
        effort
    }

    fn reasoning_echo_policy(&self, _model: &str) -> EchoPolicy {
        EchoPolicy::Never
    }

    fn supports_multiple_choices(&self) -> bool {
        true
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn map_model_name(&self, requested: &str) -> String {
        requested.to_owned()
    }

    fn thinking_model(&self, _model: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_openai_uses_neutral_defaults() {
        let profile = GenericOpenAi::default();

        assert!(profile.param_blocklist("gpt-4.1").is_empty());
        assert_eq!(profile.normalize_reasoning_effort("medium"), "medium");
        assert_eq!(profile.reasoning_echo_policy("gpt-4.1"), EchoPolicy::Never);
        assert!(profile.supports_multiple_choices());
        assert_eq!(profile.base_url(), DEFAULT_OPENAI_BASE_URL);
        assert_eq!(profile.map_model_name("gpt-4.1"), "gpt-4.1");
        assert!(!profile.thinking_model("gpt-4.1"));
    }

    #[test]
    fn generic_openai_supports_custom_base_url() {
        let profile = GenericOpenAi::new("https://compatible.example/v1");

        assert_eq!(profile.base_url(), "https://compatible.example/v1");
        assert_eq!(profile.map_model_name("custom-model"), "custom-model");
    }
}
