//! Encoding for Anthropic Messages API responses.

// Later M2 tasks wire this staged encoder into HTTP routing and streaming.
#![allow(dead_code)]

use serde_json::{Map, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, ImageSource, Provider, Thinking},
        request::{IrResponse, StopReason, Usage},
    },
    reasoning::envelope::{SourceBlock, wrap_as_signature},
};

/// Converts a provider-neutral non-streaming response into an Anthropic message.
pub fn ir_response_to_anthropic(resp: &IrResponse) -> Result<Value> {
    Ok(json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": encode_content_blocks(&resp.content)?,
        "stop_reason": encode_stop_reason(&resp.stop_reason),
        "stop_sequence": null,
        "usage": encode_usage(&resp.usage),
    }))
}

fn encode_content_blocks(content: &[ContentBlock]) -> Result<Vec<Value>> {
    content.iter().map(encode_content_block).collect()
}

fn encode_content_block(block: &ContentBlock) -> Result<Value> {
    match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::Image(source) => Ok(encode_image(source)),
        ContentBlock::ToolUse { id, name, input } => Ok(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let content = encode_content_blocks(content)?;
            Ok(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }))
        }
        ContentBlock::Thinking(thinking) => encode_thinking(thinking),
    }
}

fn encode_image(source: &ImageSource) -> Value {
    match source {
        ImageSource::Url(url) => json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": url,
            },
        }),
        ImageSource::Base64 { media_type, data } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            },
        }),
    }
}

fn encode_thinking(thinking: &Thinking) -> Result<Value> {
    let mut block = Map::new();
    block.insert("type".to_owned(), json!("thinking"));

    match &thinking.source {
        Provider::Responses => {
            let opaque = thinking.opaque.as_deref().ok_or_else(|| {
                mapping_error(
                    "Responses-origin thinking requires opaque encrypted_content for Anthropic signature envelope",
                )
            })?;
            let signature = wrap_as_signature(&SourceBlock::new(Provider::Responses, opaque))?;
            block.insert(
                "thinking".to_owned(),
                json!(thinking.text.as_deref().unwrap_or("")),
            );
            block.insert("signature".to_owned(), json!(signature));
        }
        _ => {
            if let Some(text) = &thinking.text {
                block.insert("thinking".to_owned(), json!(text));
            }

            if let Some(opaque) = &thinking.opaque {
                let signature = std::str::from_utf8(opaque).map_err(|err| {
                    mapping_error(format!(
                        "Anthropic thinking signature must be valid UTF-8: {err}"
                    ))
                })?;
                block.insert("signature".to_owned(), json!(signature));
            }
        }
    }

    Ok(Value::Object(block))
}

fn encode_stop_reason(stop_reason: &StopReason) -> &str {
    match stop_reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        StopReason::Other(reason) => reason,
    }
}

fn encode_usage(usage: &Usage) -> Value {
    let mut value = Map::new();
    value.insert("input_tokens".to_owned(), json!(usage.input_tokens));
    value.insert("output_tokens".to_owned(), json!(usage.output_tokens));

    if let Some(cache_read) = usage.cache_read {
        value.insert("cache_read_input_tokens".to_owned(), json!(cache_read));
    }

    if let Some(cache_write) = usage.cache_write {
        value.insert("cache_creation_input_tokens".to_owned(), json!(cache_write));
    }

    Value::Object(value)
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ir::message::{EchoPolicy, Provider};

    #[test]
    fn encodes_response_with_thinking_text_tool_use_and_usage() {
        let response = IrResponse {
            id: "msg_1".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![
                ContentBlock::Thinking(Thinking {
                    text: Some("I should call the weather tool.".to_owned()),
                    opaque: Some(b"sig_opaque".to_vec()),
                    source: Provider::Anthropic,
                    echo_policy: EchoPolicy::Always,
                }),
                ContentBlock::Text {
                    text: "Calling the weather tool.".to_owned(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".to_owned(),
                    name: "lookup_weather".to_owned(),
                    input: json!({ "city": "Paris" }),
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 42,
                output_tokens: 9,
                cache_read: Some(10),
                cache_write: Some(3),
            },
        };

        assert_eq!(
            ir_response_to_anthropic(&response).unwrap(),
            json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [
                    {
                        "type": "thinking",
                        "thinking": "I should call the weather tool.",
                        "signature": "sig_opaque"
                    },
                    {
                        "type": "text",
                        "text": "Calling the weather tool."
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "lookup_weather",
                        "input": { "city": "Paris" }
                    }
                ],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 42,
                    "output_tokens": 9,
                    "cache_read_input_tokens": 10,
                    "cache_creation_input_tokens": 3
                }
            })
        );
    }

    #[test]
    fn encodes_all_stop_reason_variants() {
        for (stop_reason, expected) in [
            (StopReason::EndTurn, "end_turn"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::StopSequence, "stop_sequence"),
            (StopReason::ToolUse, "tool_use"),
            (StopReason::Other("pause_turn".to_owned()), "pause_turn"),
        ] {
            let response = IrResponse {
                id: format!("msg_{expected}"),
                model: "claude-sonnet-4-5".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "done".to_owned(),
                }],
                stop_reason,
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    cache_read: None,
                    cache_write: None,
                },
            };

            let encoded = ir_response_to_anthropic(&response).unwrap();

            assert_eq!(encoded["stop_reason"], expected);
            assert_eq!(
                encoded["usage"],
                json!({
                    "input_tokens": 7,
                    "output_tokens": 3
                })
            );
        }
    }

    #[test]
    fn encodes_image_and_tool_result_blocks_recursively() {
        let response = IrResponse {
            id: "msg_blocks".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![
                ContentBlock::Image(ImageSource::Base64 {
                    media_type: "image/png".to_owned(),
                    data: "aW1n".to_owned(),
                }),
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "sunny".to_owned(),
                    }],
                    is_error: true,
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read: None,
                cache_write: None,
            },
        };

        let encoded = ir_response_to_anthropic(&response).unwrap();

        assert_eq!(
            encoded["content"],
            json!([
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "aW1n"
                    }
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": [
                        {
                            "type": "text",
                            "text": "sunny"
                        }
                    ],
                    "is_error": true
                }
            ])
        );
    }

    #[test]
    fn wraps_responses_thinking_opaque_as_anthropic_signature() {
        let response = IrResponse {
            id: "msg_reasoning".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![ContentBlock::Thinking(Thinking {
                text: Some("Need the weather tool.".to_owned()),
                opaque: Some(b"enc_payload_from_responses".to_vec()),
                source: Provider::Responses,
                echo_policy: EchoPolicy::Always,
            })],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 12,
                output_tokens: 4,
                cache_read: None,
                cache_write: None,
            },
        };

        let encoded = ir_response_to_anthropic(&response).unwrap();
        let thinking = &encoded["content"][0];

        assert_eq!(thinking["type"], "thinking");
        assert_eq!(thinking["thinking"], "Need the weather tool.");

        let signature = thinking["signature"].as_str().unwrap();
        let source_block = crate::reasoning::envelope::unwrap_from_signature(signature).unwrap();

        assert_eq!(source_block.source, Provider::Responses);
        assert_eq!(
            source_block.payload.as_slice(),
            b"enc_payload_from_responses"
        );
    }

    #[test]
    fn rejects_responses_thinking_without_opaque_payload() {
        let response = IrResponse {
            id: "msg_missing_reasoning".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![ContentBlock::Thinking(Thinking {
                text: Some("Need the weather tool.".to_owned()),
                opaque: None,
                source: Provider::Responses,
                echo_policy: EchoPolicy::Always,
            })],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 12,
                output_tokens: 4,
                cache_read: None,
                cache_write: None,
            },
        };

        let err = ir_response_to_anthropic(&response).unwrap_err();

        assert!(matches!(
            err,
            ProxyError::ProtocolMapping(message)
                if message.contains("Responses-origin thinking requires opaque encrypted_content")
        ));
    }
}
