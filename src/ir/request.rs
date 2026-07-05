//! Unified request and response IR definitions.

// Later M1 tasks wire these staged IR types into protocol parsing and encoding.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::message::{ContentBlock, Message};

/// Provider-neutral request passed between protocol decoders and encoders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrRequest {
    pub model: String,
    pub system: Option<Vec<ContentBlock>>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: ToolChoice,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop: Vec<String>,
    pub stream: bool,
    pub extra: Map<String, Value>,
}

/// Tool definition in the canonical format shared by supported protocols.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Canonical tool-selection mode requested by the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool(String),
}

/// Provider-neutral non-streaming response produced by upstream adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

/// Canonical reason why a model stopped producing output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    Other(String),
}

/// Token accounting normalized across providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read: Option<u32>,
    pub cache_write: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ir::message::{ContentBlock, Role};

    #[test]
    fn ir_request_serializes_expected_fields() {
        let mut extra = Map::new();
        extra.insert("reasoning_effort".to_owned(), json!("high"));

        let request = IrRequest {
            model: "deepseek-reasoner".to_owned(),
            system: Some(vec![ContentBlock::Text {
                text: "be concise".to_owned(),
            }]),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_owned(),
                }],
            }],
            tools: vec![ToolDef {
                name: "lookup".to_owned(),
                description: Some("look up a value".to_owned()),
                input_schema: json!({ "type": "object" }),
            }],
            tool_choice: ToolChoice::Tool("lookup".to_owned()),
            max_tokens: Some(128),
            temperature: Some(0.5),
            top_p: None,
            top_k: Some(20),
            stop: vec!["END".to_owned()],
            stream: true,
            extra,
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "model": "deepseek-reasoner",
                "system": [{ "text": { "text": "be concise" } }],
                "messages": [{
                    "role": "user",
                    "content": [{ "text": { "text": "hello" } }]
                }],
                "tools": [{
                    "name": "lookup",
                    "description": "look up a value",
                    "input_schema": { "type": "object" }
                }],
                "tool_choice": { "tool": "lookup" },
                "max_tokens": 128,
                "temperature": 0.5,
                "top_p": null,
                "top_k": 20,
                "stop": ["END"],
                "stream": true,
                "extra": { "reasoning_effort": "high" }
            })
        );
    }

    #[test]
    fn ir_response_preserves_stop_reason_and_usage() {
        let response = IrResponse {
            id: "msg_1".to_owned(),
            model: "deepseek-chat".to_owned(),
            content: vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 7,
                output_tokens: 3,
                cache_read: Some(2),
                cache_write: None,
            },
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "id": "msg_1",
                "model": "deepseek-chat",
                "content": [{ "text": { "text": "done" } }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 7,
                    "output_tokens": 3,
                    "cache_read": 2,
                    "cache_write": null
                }
            })
        );
    }
}
