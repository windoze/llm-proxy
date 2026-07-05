//! Encoding for OpenAI Responses API responses.

// Later M3 tasks wire this staged encoder into HTTP routing and streaming.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::ir::{
    message::{ContentBlock, Thinking},
    request::{IrResponse, StopReason, Usage},
};

/// Converts a provider-neutral non-streaming response into a Responses object.
pub fn ir_response_to_responses(resp: &IrResponse) -> Value {
    let status = encode_status(&resp.stop_reason);

    json!({
        "id": resp.id,
        "object": "response",
        "created_at": unix_timestamp(),
        "status": status,
        "error": null,
        "incomplete_details": encode_incomplete_details(&resp.stop_reason),
        "model": resp.model,
        "output": encode_output(&resp.id, &resp.content, status),
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "store": false,
        "usage": encode_usage(&resp.usage),
    })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must not be before the Unix epoch")
        .as_secs()
}

fn encode_output(response_id: &str, content: &[ContentBlock], status: &str) -> Value {
    let mut output = Vec::new();
    let mut pending_text = Vec::new();

    for block in content {
        match block {
            ContentBlock::Text { text } => pending_text.push(json!({
                "type": "output_text",
                "text": text,
                "annotations": [],
            })),
            ContentBlock::Thinking(thinking) => {
                flush_message_item(response_id, status, &mut pending_text, &mut output);
                output.push(encode_reasoning_item(
                    response_id,
                    output.len(),
                    thinking,
                    status,
                ));
            }
            ContentBlock::ToolUse { id, name, input } => {
                flush_message_item(response_id, status, &mut pending_text, &mut output);
                output.push(json!({
                    "id": item_id("fc", response_id, output.len()),
                    "type": "function_call",
                    "status": status,
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input)
                        .expect("serde_json::Value must serialize to JSON"),
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush_message_item(response_id, status, &mut pending_text, &mut output);
                output.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": encode_tool_output(content),
                    "is_error": is_error,
                }));
            }
            ContentBlock::Image(_) => {
                panic!("Responses assistant output does not support image content blocks");
            }
        }
    }

    flush_message_item(response_id, status, &mut pending_text, &mut output);
    Value::Array(output)
}

fn flush_message_item(
    response_id: &str,
    status: &str,
    pending_text: &mut Vec<Value>,
    output: &mut Vec<Value>,
) {
    if pending_text.is_empty() {
        return;
    }

    output.push(json!({
        "id": item_id("msg", response_id, output.len()),
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": std::mem::take(pending_text),
    }));
}

fn encode_reasoning_item(
    response_id: &str,
    output_index: usize,
    thinking: &Thinking,
    status: &str,
) -> Value {
    let mut item = Map::new();
    item.insert(
        "id".to_owned(),
        json!(item_id("rs", response_id, output_index)),
    );
    item.insert("type".to_owned(), json!("reasoning"));
    item.insert("status".to_owned(), json!(status));
    item.insert("summary".to_owned(), encode_reasoning_summary(thinking));

    if let Some(opaque) = &thinking.opaque {
        let encrypted_content = std::str::from_utf8(opaque)
            .expect("Responses reasoning encrypted_content must be valid UTF-8");
        item.insert("encrypted_content".to_owned(), json!(encrypted_content));
    }

    Value::Object(item)
}

fn encode_reasoning_summary(thinking: &Thinking) -> Value {
    let Some(text) = thinking.text.as_ref().filter(|text| !text.is_empty()) else {
        return Value::Array(Vec::new());
    };

    json!([{
        "type": "summary_text",
        "text": text,
    }])
}

fn encode_tool_output(content: &[ContentBlock]) -> Value {
    if content.len() == 1
        && let ContentBlock::Text { text } = &content[0]
    {
        return Value::String(text.clone());
    }

    Value::Array(
        content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => json!({
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                }),
                ContentBlock::Thinking(_) => {
                    panic!("Responses function_call_output cannot contain reasoning blocks");
                }
                ContentBlock::ToolUse { .. } => {
                    panic!("Responses function_call_output cannot contain function_call blocks");
                }
                ContentBlock::ToolResult { .. } => {
                    panic!("Responses function_call_output cannot contain nested tool results");
                }
                ContentBlock::Image(_) => {
                    panic!("Responses function_call_output cannot contain image blocks");
                }
            })
            .collect(),
    )
}

pub(super) fn item_id(prefix: &str, response_id: &str, output_index: usize) -> String {
    format!("{prefix}_{response_id}_{output_index}")
}

pub(super) fn encode_status(stop_reason: &StopReason) -> &'static str {
    match stop_reason {
        StopReason::MaxTokens | StopReason::Other(_) => "incomplete",
        StopReason::EndTurn | StopReason::StopSequence | StopReason::ToolUse => "completed",
    }
}

pub(super) fn encode_incomplete_details(stop_reason: &StopReason) -> Value {
    match stop_reason {
        StopReason::MaxTokens => json!({ "reason": "max_output_tokens" }),
        StopReason::Other(reason) => json!({ "reason": reason }),
        StopReason::EndTurn | StopReason::StopSequence | StopReason::ToolUse => Value::Null,
    }
}

pub(super) fn encode_usage(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.cache_read.unwrap_or(0),
        },
        "output_tokens": usage.output_tokens,
        "output_tokens_details": {
            "reasoning_tokens": 0,
        },
        "total_tokens": u64::from(usage.input_tokens) + u64::from(usage.output_tokens),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ir::message::{EchoPolicy, Provider};

    #[test]
    fn encodes_response_with_reasoning_text_function_call_and_usage() {
        let response = IrResponse {
            id: "resp_1".to_owned(),
            model: "deepseek-reasoner".to_owned(),
            content: vec![
                ContentBlock::Thinking(Thinking {
                    text: Some("Need the weather tool.".to_owned()),
                    opaque: Some(b"enc_payload".to_vec()),
                    source: Provider::Responses,
                    echo_policy: EchoPolicy::Always,
                }),
                ContentBlock::Text {
                    text: "Calling the weather tool.".to_owned(),
                },
                ContentBlock::ToolUse {
                    id: "call_weather".to_owned(),
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

        let mut encoded = ir_response_to_responses(&response);
        assert!(encoded["created_at"].as_u64().unwrap() > 0);
        encoded["created_at"] = json!(0);

        assert_eq!(
            encoded,
            json!({
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "model": "deepseek-reasoner",
                "output": [
                    {
                        "id": "rs_resp_1_0",
                        "type": "reasoning",
                        "status": "completed",
                        "summary": [{
                            "type": "summary_text",
                            "text": "Need the weather tool."
                        }],
                        "encrypted_content": "enc_payload"
                    },
                    {
                        "id": "msg_resp_1_1",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "Calling the weather tool.",
                            "annotations": []
                        }]
                    },
                    {
                        "id": "fc_resp_1_2",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_weather",
                        "name": "lookup_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                ],
                "parallel_tool_calls": true,
                "previous_response_id": null,
                "store": false,
                "usage": {
                    "input_tokens": 42,
                    "input_tokens_details": {
                        "cached_tokens": 10
                    },
                    "output_tokens": 9,
                    "output_tokens_details": {
                        "reasoning_tokens": 0
                    },
                    "total_tokens": 51
                }
            })
        );
    }

    #[test]
    fn groups_contiguous_text_and_omits_absent_encrypted_content() {
        let response = IrResponse {
            id: "resp_text".to_owned(),
            model: "deepseek-chat".to_owned(),
            content: vec![
                ContentBlock::Text {
                    text: "Hello, ".to_owned(),
                },
                ContentBlock::Text {
                    text: "world.".to_owned(),
                },
                ContentBlock::Thinking(Thinking {
                    text: None,
                    opaque: None,
                    source: Provider::DeepSeek,
                    echo_policy: EchoPolicy::Never,
                }),
            ],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read: None,
                cache_write: None,
            },
        };

        let encoded = ir_response_to_responses(&response);

        assert_eq!(
            encoded["output"],
            json!([
                {
                    "id": "msg_resp_text_0",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Hello, ",
                            "annotations": []
                        },
                        {
                            "type": "output_text",
                            "text": "world.",
                            "annotations": []
                        }
                    ]
                },
                {
                    "id": "rs_resp_text_1",
                    "type": "reasoning",
                    "status": "completed",
                    "summary": []
                }
            ])
        );
    }

    #[test]
    fn maps_stop_reasons_to_responses_status() {
        for (stop_reason, expected_status, expected_incomplete_details) in [
            (StopReason::EndTurn, "completed", Value::Null),
            (StopReason::StopSequence, "completed", Value::Null),
            (StopReason::ToolUse, "completed", Value::Null),
            (
                StopReason::MaxTokens,
                "incomplete",
                json!({ "reason": "max_output_tokens" }),
            ),
            (
                StopReason::Other("content_filter".to_owned()),
                "incomplete",
                json!({ "reason": "content_filter" }),
            ),
        ] {
            let response = IrResponse {
                id: format!("resp_{expected_status}"),
                model: "deepseek-chat".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "done".to_owned(),
                }],
                stop_reason,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read: None,
                    cache_write: None,
                },
            };

            let encoded = ir_response_to_responses(&response);

            assert_eq!(encoded["status"], expected_status);
            assert_eq!(encoded["incomplete_details"], expected_incomplete_details);
            assert_eq!(encoded["output"][0]["status"], expected_status);
        }
    }

    #[test]
    fn encodes_tool_result_for_complete_ir_coverage() {
        let response = IrResponse {
            id: "resp_tool_result".to_owned(),
            model: "deepseek-chat".to_owned(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_weather".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "sunny".to_owned(),
                }],
                is_error: true,
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read: None,
                cache_write: None,
            },
        };

        assert_eq!(
            ir_response_to_responses(&response)["output"],
            json!([{
                "type": "function_call_output",
                "call_id": "call_weather",
                "output": "sunny",
                "is_error": true
            }])
        );
    }
}
