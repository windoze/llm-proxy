//! Protocol-neutral message and content-block IR definitions.

// Later M1 tasks wire these staged IR types into request parsing and encoding.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-independent conversation role used by all protocol adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Canonical message content block spanning text, media, tools, and reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image(ImageSource),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },
    Thinking(Thinking),
}

/// Reasoning payload plus the policy that controls whether it is echoed later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thinking {
    pub text: Option<String>,
    pub opaque: Option<Vec<u8>>,
    pub source: Provider,
    pub echo_policy: EchoPolicy,
}

/// Policy for preserving provider reasoning payloads across stateless turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EchoPolicy {
    Always,
    OnlyWithToolCall,
    Never,
}

/// Upstream provider that originated a reasoning payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Provider {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "deepseek")]
    DeepSeek,
}

/// Image payload form accepted by the canonical content-block model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    Url(String),
    Base64 { media_type: String, data: String },
}

/// A single conversation message with one or more canonical content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}
