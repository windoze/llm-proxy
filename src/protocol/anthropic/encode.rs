//! Encoding for Anthropic Messages API responses.

// Later M2 tasks wire this staged encoder into HTTP routing and streaming.
#![allow(dead_code)]

use serde_json::{Map, Value, json};

use crate::ir::{
    message::{ContentBlock, ImageSource, Thinking},
    request::{IrResponse, StopReason, Usage},
};

/// Converts a provider-neutral non-streaming response into an Anthropic message.
pub fn ir_response_to_anthropic(resp: &IrResponse) -> Value {
    json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": resp.content.iter().map(encode_content_block).collect::<Vec<_>>(),
        "stop_reason": encode_stop_reason(&resp.stop_reason),
        "stop_sequence": null,
        "usage": encode_usage(&resp.usage),
    })
}

fn encode_content_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        ContentBlock::Image(source) => encode_image(source),
        ContentBlock::ToolUse { id, name, input } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content.iter().map(encode_content_block).collect::<Vec<_>>(),
            "is_error": is_error,
        }),
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

fn encode_thinking(thinking: &Thinking) -> Value {
    let mut block = Map::new();
    block.insert("type".to_owned(), json!("thinking"));

    if let Some(text) = &thinking.text {
        block.insert("thinking".to_owned(), json!(text));
    }

    if let Some(opaque) = &thinking.opaque {
        let signature =
            std::str::from_utf8(opaque).expect("Anthropic thinking signature must be valid UTF-8");
        block.insert("signature".to_owned(), json!(signature));
    }

    Value::Object(block)
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
            ir_response_to_anthropic(&response),
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

            let encoded = ir_response_to_anthropic(&response);

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

        assert_eq!(
            ir_response_to_anthropic(&response)["content"],
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
}
