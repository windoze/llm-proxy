//! Encoding for Anthropic Messages API responses.

// Later M2 tasks wire this staged encoder into HTTP routing and streaming.
#![allow(dead_code)]

use serde_json::{Map, Number, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, ImageSource, Message, Provider, Role, Thinking},
        request::{IrRequest, IrResponse, StopReason, ToolChoice, ToolDef, Usage},
    },
    protocol::tool_ids::validate_tool_result_pairs,
    reasoning::envelope::{SourceBlock, wrap_as_signature},
};

const ANTHROPIC_REQUEST_CORE_FIELDS: &[&str] = &[
    "model",
    "system",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
];
const ANTHROPIC_REQUEST_EXTRA_FIELDS: &[&str] = &[
    "metadata",
    "service_tier",
    "thinking",
    "output_config",
    "context_management",
    "container",
    "mcp_servers",
];

/// Converts a provider-neutral request into an Anthropic Messages request body.
pub fn ir_request_to_anthropic(request: &IrRequest) -> Result<Value> {
    validate_tool_result_pairs(request)?;

    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(request.model.clone()));
    body.insert(
        "max_tokens".to_owned(),
        Value::Number(Number::from(u64::from(request.max_tokens.ok_or_else(
            || mapping_error("Anthropic request max_tokens is required"),
        )?))),
    );

    if let Some(system) = &request.system
        && !system.is_empty()
    {
        body.insert(
            "system".to_owned(),
            Value::Array(encode_request_system_content(system, "request.system")?),
        );
    }

    body.insert(
        "messages".to_owned(),
        Value::Array(encode_request_messages(&request.messages)?),
    );

    if !request.tools.is_empty() {
        body.insert("tools".to_owned(), encode_request_tools(&request.tools));
    }

    if !request.tools.is_empty() || request.tool_choice != ToolChoice::Auto {
        body.insert(
            "tool_choice".to_owned(),
            encode_request_tool_choice(&request.tool_choice),
        );
    }

    insert_optional_f32(&mut body, "temperature", request.temperature)?;
    insert_optional_f32(&mut body, "top_p", request.top_p)?;
    insert_optional_u32(&mut body, "top_k", request.top_k);

    if !request.stop.is_empty() {
        body.insert(
            "stop_sequences".to_owned(),
            Value::Array(request.stop.iter().cloned().map(Value::String).collect()),
        );
    }

    body.insert("stream".to_owned(), Value::Bool(request.stream));
    insert_request_extra(&mut body, request);

    Ok(Value::Object(body))
}

fn encode_request_messages(messages: &[Message]) -> Result<Vec<Value>> {
    if messages.is_empty() {
        return Err(mapping_error(
            "Anthropic request messages must not be empty",
        ));
    }

    normalize_request_messages(messages)?
        .iter()
        .enumerate()
        .map(|(index, message)| encode_request_message(message, index))
        .collect()
}

/// Coalesces adjacent IR messages that Anthropic sees as the same role.
fn normalize_request_messages(messages: &[Message]) -> Result<Vec<Message>> {
    let mut normalized: Vec<Message> = Vec::new();

    for (index, message) in messages.iter().enumerate() {
        let role = match message.role {
            Role::User | Role::Tool => Role::User,
            Role::Assistant => Role::Assistant,
            Role::System => {
                return Err(mapping_error(format!(
                    "messages[{index}] has system role, which must be encoded in request.system for Anthropic"
                )));
            }
        };

        if let Some(last) = normalized.last_mut()
            && last.role == role
        {
            last.content.extend(message.content.clone());
            continue;
        }

        normalized.push(Message {
            role,
            content: message.content.clone(),
        });
    }

    Ok(normalized)
}

fn encode_request_message(message: &Message, index: usize) -> Result<Value> {
    match message.role {
        Role::User => Ok(json!({
            "role": "user",
            "content": encode_request_user_content(
                &message.content,
                format!("messages[{index}].content"),
            )?,
        })),
        Role::Assistant => Ok(json!({
            "role": "assistant",
            "content": encode_request_assistant_content(
                &message.content,
                format!("messages[{index}].content"),
            )?,
        })),
        Role::Tool => Ok(json!({
            "role": "user",
            "content": encode_request_tool_role_content(
                &message.content,
                format!("messages[{index}].content"),
            )?,
        })),
        Role::System => Err(mapping_error(format!(
            "messages[{index}] has system role, which must be encoded in request.system for Anthropic"
        ))),
    }
}

fn encode_request_system_content(content: &[ContentBlock], path: &str) -> Result<Vec<Value>> {
    encode_non_empty_request_content(content, path, |block, block_path| match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        _ => Err(mapping_error(format!(
            "{block_path} is not a text block; Anthropic system content only supports text"
        ))),
    })
}

fn encode_request_user_content(content: &[ContentBlock], path: String) -> Result<Vec<Value>> {
    encode_non_empty_request_content(content, &path, |block, block_path| match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::Image(source) => Ok(encode_image(source)),
        ContentBlock::ToolResult { .. } => encode_request_tool_result(block, block_path),
        ContentBlock::ToolUse { .. } => Err(mapping_error(format!(
            "{block_path} is a tool_use block but message role is user"
        ))),
        ContentBlock::Thinking(_) => Err(mapping_error(format!(
            "{block_path} is a thinking block but message role is user"
        ))),
    })
}

fn encode_request_tool_role_content(content: &[ContentBlock], path: String) -> Result<Vec<Value>> {
    encode_non_empty_request_content(content, &path, |block, block_path| match block {
        ContentBlock::ToolResult { .. } => encode_request_tool_result(block, block_path),
        _ => Err(mapping_error(format!(
            "{block_path} is not a tool_result block but message role is tool"
        ))),
    })
}

fn encode_request_assistant_content(content: &[ContentBlock], path: String) -> Result<Vec<Value>> {
    encode_non_empty_request_content(content, &path, |block, block_path| match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::ToolUse { id, name, input } => Ok(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::Thinking(thinking) => encode_backend_thinking(thinking, block_path),
        ContentBlock::Image(_) => Err(mapping_error(format!(
            "{block_path} is an image block but message role is assistant"
        ))),
        ContentBlock::ToolResult { .. } => Err(mapping_error(format!(
            "{block_path} is a tool_result block but message role is assistant"
        ))),
    })
}

fn encode_non_empty_request_content<F>(
    content: &[ContentBlock],
    path: &str,
    mut encode: F,
) -> Result<Vec<Value>>
where
    F: FnMut(&ContentBlock, String) -> Result<Value>,
{
    if content.is_empty() {
        return Err(mapping_error(format!("{path} must not be empty")));
    }

    content
        .iter()
        .enumerate()
        .map(|(index, block)| encode(block, format!("{path}[{index}]")))
        .collect()
}

fn encode_request_tool_result(block: &ContentBlock, path: String) -> Result<Value> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = block
    else {
        return Err(mapping_error(format!("{path} must be a tool_result block")));
    };

    Ok(json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": encode_request_tool_result_content(content, format!("{path}.content"))?,
        "is_error": is_error,
    }))
}

fn encode_request_tool_result_content(
    content: &[ContentBlock],
    path: String,
) -> Result<Vec<Value>> {
    encode_non_empty_request_content(content, &path, |block, block_path| match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::Image(source) => Ok(encode_image(source)),
        ContentBlock::Thinking(_) => Err(mapping_error(format!(
            "{block_path} is a thinking block but tool_result content cannot contain thinking"
        ))),
        ContentBlock::ToolUse { .. } => Err(mapping_error(format!(
            "{block_path} is a tool_use block but tool_result content cannot contain tool_use"
        ))),
        ContentBlock::ToolResult { .. } => Err(mapping_error(format!(
            "{block_path} is a nested tool_result block"
        ))),
    })
}

fn encode_backend_thinking(thinking: &Thinking, path: String) -> Result<Value> {
    if thinking.source != Provider::Anthropic {
        return Err(mapping_error(format!(
            "{path} must be Anthropic-origin thinking to send to an Anthropic backend"
        )));
    }

    let text = thinking
        .text
        .as_deref()
        .ok_or_else(|| mapping_error(format!("{path}.thinking text is required")))?;
    let opaque = thinking
        .opaque
        .as_deref()
        .ok_or_else(|| mapping_error(format!("{path}.opaque Anthropic signature is required")))?;
    let signature = std::str::from_utf8(opaque).map_err(|err| {
        mapping_error(format!(
            "{path}.opaque Anthropic signature must be valid UTF-8: {err}"
        ))
    })?;

    Ok(json!({
        "type": "thinking",
        "thinking": text,
        "signature": signature,
    }))
}

fn encode_request_tools(tools: &[ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut item = Map::new();
                item.insert("name".to_owned(), Value::String(tool.name.clone()));
                if let Some(description) = &tool.description {
                    item.insert("description".to_owned(), Value::String(description.clone()));
                }
                item.insert("input_schema".to_owned(), tool.input_schema.clone());
                Value::Object(item)
            })
            .collect(),
    )
}

fn encode_request_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Tool(name) => json!({
            "type": "tool",
            "name": name,
        }),
    }
}

fn insert_optional_u32(body: &mut Map<String, Value>, field: &'static str, value: Option<u32>) {
    if let Some(value) = value {
        body.insert(
            field.to_owned(),
            Value::Number(Number::from(u64::from(value))),
        );
    }
}

fn insert_optional_f32(
    body: &mut Map<String, Value>,
    field: &'static str,
    value: Option<f32>,
) -> Result<()> {
    if let Some(value) = value {
        let number = Number::from_f64(f64::from(value))
            .ok_or_else(|| mapping_error(format!("request.{field} must be a finite number")))?;
        body.insert(field.to_owned(), Value::Number(number));
    }

    Ok(())
}

fn insert_request_extra(body: &mut Map<String, Value>, request: &IrRequest) {
    for (key, value) in &request.extra {
        if ANTHROPIC_REQUEST_CORE_FIELDS.contains(&key.as_str())
            || !ANTHROPIC_REQUEST_EXTRA_FIELDS.contains(&key.as_str())
        {
            continue;
        }
        body.insert(key.clone(), value.clone());
    }
}

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
    use serde_json::{Map, json};

    use super::*;
    use crate::ir::message::{EchoPolicy, Provider};

    #[test]
    fn encodes_request_with_restored_anthropic_thinking_signature() {
        let request = IrRequest {
            model: "claude-sonnet-4-5".to_owned(),
            system: Some(vec![ContentBlock::Text {
                text: "Use tools when required.".to_owned(),
            }]),
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "What is the weather in Paris?".to_owned(),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking(Thinking {
                            text: Some("Need the weather tool.".to_owned()),
                            opaque: Some(b"sig_real_anthropic_123".to_vec()),
                            source: Provider::Anthropic,
                            echo_policy: EchoPolicy::Always,
                        }),
                        ContentBlock::ToolUse {
                            id: "toolu_weather".to_owned(),
                            name: "lookup_weather".to_owned(),
                            input: json!({ "city": "Paris" }),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "toolu_weather".to_owned(),
                        content: vec![ContentBlock::Text {
                            text: "sunny".to_owned(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            tools: vec![ToolDef {
                name: "lookup_weather".to_owned(),
                description: Some("Fetch weather".to_owned()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }),
            }],
            tool_choice: ToolChoice::Tool("lookup_weather".to_owned()),
            max_tokens: Some(256),
            temperature: Some(0.25),
            top_p: Some(0.5),
            top_k: Some(40),
            stop: vec!["DONE".to_owned()],
            stream: false,
            extra: Map::from_iter([
                ("metadata".to_owned(), json!({ "session": "s_1" })),
                ("output_config".to_owned(), json!({ "effort": "high" })),
                ("store".to_owned(), json!(false)),
            ]),
        };

        let encoded = ir_request_to_anthropic(&request).unwrap();

        assert_eq!(
            encoded,
            json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 256,
                "system": [{
                    "type": "text",
                    "text": "Use tools when required."
                }],
                "messages": [
                    {
                        "role": "user",
                        "content": [{
                            "type": "text",
                            "text": "What is the weather in Paris?"
                        }]
                    },
                    {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "thinking",
                                "thinking": "Need the weather tool.",
                                "signature": "sig_real_anthropic_123"
                            },
                            {
                                "type": "tool_use",
                                "id": "toolu_weather",
                                "name": "lookup_weather",
                                "input": { "city": "Paris" }
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": "toolu_weather",
                            "content": [{
                                "type": "text",
                                "text": "sunny"
                            }],
                            "is_error": false
                        }]
                    }
                ],
                "tools": [{
                    "name": "lookup_weather",
                    "description": "Fetch weather",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        }
                    }
                }],
                "tool_choice": {
                    "type": "tool",
                    "name": "lookup_weather"
                },
                "temperature": 0.25,
                "top_p": 0.5,
                "top_k": 40,
                "stop_sequences": ["DONE"],
                "stream": false,
                "metadata": { "session": "s_1" },
                "output_config": { "effort": "high" }
            })
        );
    }

    #[test]
    fn coalesces_adjacent_messages_by_anthropic_role() {
        let request = IrRequest {
            model: "claude-sonnet-4-5".to_owned(),
            system: None,
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "What is the weather in Paris?".to_owned(),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Thinking(Thinking {
                        text: Some("Need the weather tool.".to_owned()),
                        opaque: Some(b"sig_real_anthropic_1".to_vec()),
                        source: Provider::Anthropic,
                        echo_policy: EchoPolicy::Always,
                    })],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_weather_1".to_owned(),
                        name: "lookup_weather".to_owned(),
                        input: json!({ "city": "Paris" }),
                    }],
                },
                Message {
                    role: Role::Tool,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "toolu_weather_1".to_owned(),
                        content: vec![ContentBlock::Text {
                            text: "sunny and 21C".to_owned(),
                        }],
                        is_error: false,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "Should I bring an umbrella?".to_owned(),
                    }],
                },
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(256),
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::new(),
        };

        let encoded = ir_request_to_anthropic(&request).unwrap();

        assert_eq!(
            encoded["messages"],
            json!([
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "What is the weather in Paris?"
                    }]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "Need the weather tool.",
                            "signature": "sig_real_anthropic_1"
                        },
                        {
                            "type": "tool_use",
                            "id": "toolu_weather_1",
                            "name": "lookup_weather",
                            "input": { "city": "Paris" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_weather_1",
                            "content": [{
                                "type": "text",
                                "text": "sunny and 21C"
                            }],
                            "is_error": false
                        },
                        {
                            "type": "text",
                            "text": "Should I bring an umbrella?"
                        }
                    ]
                }
            ])
        );
    }

    #[test]
    fn rejects_responses_origin_thinking_for_anthropic_backend_request() {
        let request = IrRequest {
            model: "claude-sonnet-4-5".to_owned(),
            system: None,
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking(Thinking {
                    text: Some("Responses reasoning cannot be sent to Anthropic raw.".to_owned()),
                    opaque: Some(b"enc_payload_from_responses".to_vec()),
                    source: Provider::Responses,
                    echo_policy: EchoPolicy::Always,
                })],
            }],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(256),
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::new(),
        };

        let error = ir_request_to_anthropic(&request).unwrap_err();

        assert!(matches!(error, ProxyError::ProtocolMapping(message)
                if message.contains("Anthropic-origin thinking")));
    }

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
