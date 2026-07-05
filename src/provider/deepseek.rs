//! DeepSeek provider capability profile.

use crate::ir::message::EchoPolicy;

use super::CapabilityProfile;

const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const DEEPSEEK_REASONER_MODEL: &str = "deepseek-reasoner";
const DEEPSEEK_PARAM_BLOCKLIST: &[&str] = &[
    "temperature",
    "top_p",
    "presence_penalty",
    "frequency_penalty",
    "logprobs",
    "top_logprobs",
];

/// Capability profile for DeepSeek's OpenAI-compatible Chat API.
///
/// DESIGN section 5 notes DeepSeek's official docs are inconsistent: the old
/// `reasoning_model` page differs from the newer `thinking_mode` page. These
/// rules intentionally follow `thinking_mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeepSeek;

impl CapabilityProfile for DeepSeek {
    fn param_blocklist(&self, _model: &str) -> &[&str] {
        DEEPSEEK_PARAM_BLOCKLIST
    }

    fn normalize_reasoning_effort<'a>(&self, effort: &'a str) -> &'a str {
        match effort {
            "xhigh" | "max" => "max",
            "low" | "medium" | "high" => "high",
            _ => "high",
        }
    }

    fn reasoning_echo_policy(&self, _model: &str) -> EchoPolicy {
        EchoPolicy::OnlyWithToolCall
    }

    fn supports_multiple_choices(&self) -> bool {
        false
    }

    fn base_url(&self) -> &str {
        DEEPSEEK_BASE_URL
    }

    fn map_model_name(&self, requested: &str) -> String {
        requested.to_owned()
    }

    fn thinking_model(&self, model: &str) -> bool {
        model == DEEPSEEK_REASONER_MODEL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_deepseek_api_base_url_and_identity_model_mapping() {
        let profile = DeepSeek;

        assert_eq!(profile.base_url(), "https://api.deepseek.com");
        assert_eq!(profile.map_model_name("deepseek-chat"), "deepseek-chat");
        assert_eq!(
            profile.map_model_name("deepseek-reasoner"),
            "deepseek-reasoner"
        );
    }

    #[test]
    fn drops_unsupported_deepseek_parameters() {
        let profile = DeepSeek;

        assert_eq!(
            profile.param_blocklist("deepseek-reasoner"),
            [
                "temperature",
                "top_p",
                "presence_penalty",
                "frequency_penalty",
                "logprobs",
                "top_logprobs",
            ]
        );
    }

    #[test]
    fn normalizes_reasoning_effort_to_deepseek_levels() {
        let profile = DeepSeek;

        assert_eq!(profile.normalize_reasoning_effort("low"), "high");
        assert_eq!(profile.normalize_reasoning_effort("medium"), "high");
        assert_eq!(profile.normalize_reasoning_effort("high"), "high");
        assert_eq!(profile.normalize_reasoning_effort("xhigh"), "max");
        assert_eq!(profile.normalize_reasoning_effort("max"), "max");
        assert_eq!(profile.normalize_reasoning_effort("unsupported"), "high");
    }

    #[test]
    fn uses_conditional_reasoning_echo_policy() {
        let profile = DeepSeek;

        assert_eq!(
            profile.reasoning_echo_policy("deepseek-reasoner"),
            EchoPolicy::OnlyWithToolCall
        );
        assert_eq!(
            profile.reasoning_echo_policy("deepseek-chat"),
            EchoPolicy::OnlyWithToolCall
        );
    }

    #[test]
    fn disables_multiple_choices_and_detects_thinking_model() {
        let profile = DeepSeek;

        assert!(!profile.supports_multiple_choices());
        assert!(profile.thinking_model("deepseek-reasoner"));
        assert!(!profile.thinking_model("deepseek-chat"));
        assert!(!profile.thinking_model("deepseek-v4-pro"));
    }
}
