//! Decoding for Anthropic Messages API requests.

// Later M2 tasks wire this staged decoder into HTTP routing and encoders.
#![allow(dead_code)]

use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, EchoPolicy, ImageSource, Message, Provider, Role, Thinking},
        request::{IrRequest, ToolChoice, ToolDef},
    },
    reasoning::envelope::{is_wrapped_signature, unwrap_from_signature},
};

const PROTOCOL: &str = "anthropic";
const CORE_REQUEST_FIELDS: &[&str] = &[
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

/// Converts an Anthropic Messages request body into the provider-neutral IR.
pub fn anthropic_request_to_ir(body: &Value) -> Result<IrRequest> {
    let request = body
        .as_object()
        .ok_or_else(|| mapping_error("request body must be a JSON object"))?;
    let messages_value = request
        .get("messages")
        .ok_or_else(|| mapping_error("request.messages is required"))?;
    let message_values = messages_value
        .as_array()
        .ok_or_else(|| mapping_error("request.messages must be an array"))?;

    Ok(IrRequest {
        model: required_string(request, "model", "request.model")?.to_owned(),
        system: decode_optional_content(request.get("system"), "request.system")?,
        messages: decode_messages(message_values)?,
        tools: decode_tools(request.get("tools"))?,
        tool_choice: decode_tool_choice(request.get("tool_choice"))?,
        max_tokens: optional_u32(request, "max_tokens", "request.max_tokens")?,
        temperature: optional_f32(request, "temperature", "request.temperature")?,
        top_p: optional_f32(request, "top_p", "request.top_p")?,
        top_k: optional_u32(request, "top_k", "request.top_k")?,
        stop: decode_stop_sequences(request.get("stop_sequences"))?,
        stream: optional_bool(request, "stream", "request.stream")?.unwrap_or(false),
        extra: collect_extra(request),
    })
}

fn decode_messages(message_values: &[Value]) -> Result<Vec<Message>> {
    message_values
        .iter()
        .enumerate()
        .map(|(index, message_value)| decode_message(message_value, index))
        .collect()
}

fn decode_message(value: &Value, index: usize) -> Result<Message> {
    let message = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("messages[{index}] must be an object")))?;
    let role = required_string(message, "role", format!("messages[{index}].role"))?;
    let role = match role {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("message role `{other}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }
    };

    Ok(Message {
        role,
        content: decode_required_content(
            message.get("content"),
            format!("messages[{index}].content"),
        )?,
    })
}

fn decode_optional_content(value: Option<&Value>, path: &str) -> Result<Option<Vec<ContentBlock>>> {
    match value {
        Some(Value::Null) | None => Ok(None),
        Some(_) => {
            let content = decode_required_content(value, path.to_owned())?;
            Ok((!content.is_empty()).then_some(content))
        }
    }
}

fn decode_required_content(
    value: Option<&Value>,
    path: impl Into<String>,
) -> Result<Vec<ContentBlock>> {
    let path = path.into();
    match value {
        Some(Value::String(text)) => Ok(vec![ContentBlock::Text { text: text.clone() }]),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .enumerate()
            .map(|(index, block)| decode_content_block(block, format!("{path}[{index}]")))
            .collect(),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!(
            "{path} must be a string or content block array"
        ))),
    }
}

fn decode_content_block(value: &Value, path: String) -> Result<ContentBlock> {
    let block = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let block_type = required_string(block, "type", format!("{path}.type"))?;

    match block_type {
        "text" => Ok(ContentBlock::Text {
            text: required_string(block, "text", format!("{path}.text"))?.to_owned(),
        }),
        "image" => decode_image(block, path),
        "tool_use" => decode_tool_use(block, path),
        "tool_result" => decode_tool_result(block, path),
        "thinking" => decode_thinking(block, path),
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("content block type `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_image(block: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let source = block
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error(format!("{path}.source must be an object")))?;
    let source_type = required_string(source, "type", format!("{path}.source.type"))?;

    let image = match source_type {
        "base64" => ImageSource::Base64 {
            media_type: required_string(source, "media_type", format!("{path}.source.media_type"))?
                .to_owned(),
            data: required_string(source, "data", format!("{path}.source.data"))?.to_owned(),
        },
        "url" => ImageSource::Url(
            required_string(source, "url", format!("{path}.source.url"))?.to_owned(),
        ),
        other => {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("image source type `{other}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }
    };

    Ok(ContentBlock::Image(image))
}

fn decode_tool_use(block: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let input = block
        .get("input")
        .cloned()
        .ok_or_else(|| mapping_error(format!("{path}.input is required")))?;

    Ok(ContentBlock::ToolUse {
        id: required_string(block, "id", format!("{path}.id"))?.to_owned(),
        name: required_string(block, "name", format!("{path}.name"))?.to_owned(),
        input,
    })
}

fn decode_tool_result(block: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let is_error = optional_bool(block, "is_error", format!("{path}.is_error"))?.unwrap_or(false);

    Ok(ContentBlock::ToolResult {
        tool_use_id: required_string(block, "tool_use_id", format!("{path}.tool_use_id"))?
            .to_owned(),
        content: decode_required_content(block.get("content"), format!("{path}.content"))?,
        is_error,
    })
}

fn decode_thinking(block: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let signature = required_string(block, "signature", format!("{path}.signature"))?;
    let thinking_text = required_string(block, "thinking", format!("{path}.thinking"))?.to_owned();

    if is_wrapped_signature(signature) {
        let source_block = unwrap_from_signature(signature)?;
        if source_block.source != Provider::Responses {
            return Err(mapping_error(format!(
                "{path}.signature envelope has source {:?}, expected Responses",
                source_block.source
            )));
        }

        return Ok(ContentBlock::Thinking(Thinking {
            text: Some(thinking_text),
            opaque: Some(source_block.payload),
            source: Provider::Responses,
            echo_policy: EchoPolicy::Always,
        }));
    }

    Ok(ContentBlock::Thinking(Thinking {
        text: Some(thinking_text),
        opaque: Some(signature.as_bytes().to_vec()),
        source: Provider::Anthropic,
        echo_policy: EchoPolicy::Always,
    }))
}

fn decode_tools(value: Option<&Value>) -> Result<Vec<ToolDef>> {
    match value {
        Some(Value::Array(tools)) => tools
            .iter()
            .enumerate()
            .map(|(index, tool)| decode_tool(tool, format!("request.tools[{index}]")))
            .collect(),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(mapping_error("request.tools must be an array")),
    }
}

fn decode_tool(value: &Value, path: String) -> Result<ToolDef> {
    let tool = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let description =
        optional_string(tool, "description", format!("{path}.description"))?.map(ToOwned::to_owned);
    let input_schema = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    Ok(ToolDef {
        name: required_string(tool, "name", format!("{path}.name"))?.to_owned(),
        description,
        input_schema,
    })
}

fn decode_tool_choice(value: Option<&Value>) -> Result<ToolChoice> {
    match value {
        Some(Value::Object(choice)) => decode_object_tool_choice(choice),
        Some(Value::Null) | None => Ok(ToolChoice::Auto),
        Some(_) => Err(mapping_error("request.tool_choice must be an object")),
    }
}

fn decode_object_tool_choice(choice: &Map<String, Value>) -> Result<ToolChoice> {
    let choice_type = required_string(choice, "type", "request.tool_choice.type")?;

    match choice_type {
        "auto" => Ok(ToolChoice::Auto),
        "none" => Ok(ToolChoice::None),
        "any" => Ok(ToolChoice::Required),
        "tool" => Ok(ToolChoice::Tool(
            required_string(choice, "name", "request.tool_choice.name")?.to_owned(),
        )),
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("tool_choice `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_stop_sequences(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        Some(Value::Array(stops)) => stops
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                stop.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                    mapping_error(format!("request.stop_sequences[{index}] must be a string"))
                })
            })
            .collect(),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(mapping_error("request.stop_sequences must be an array")),
    }
}

fn collect_extra(request: &Map<String, Value>) -> Map<String, Value> {
    request
        .iter()
        .filter(|(key, _)| !CORE_REQUEST_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn optional_u32(
    object: &Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<u32>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Number(number)) => {
            let value = number
                .as_u64()
                .ok_or_else(|| mapping_error(format!("{path} must be an unsigned integer")))?;
            let value = u32::try_from(value)
                .map_err(|_| mapping_error(format!("{path} is too large for u32")))?;
            Ok(Some(value))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be an unsigned integer"))),
    }
}

fn optional_f32(
    object: &Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<f32>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|value| Some(value as f32))
            .ok_or_else(|| mapping_error(format!("{path} must be a finite number"))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be a number"))),
    }
}

fn optional_bool(
    object: &Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<bool>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be a boolean"))),
    }
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<&'a str>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be a string"))),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<&'a str> {
    let path = path.into();
    optional_string(object, field, path.clone())
        .and_then(|value| value.ok_or_else(|| mapping_error(format!("{path} is required"))))
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn decodes_anthropic_request_with_blocks_tools_and_thinking() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "system": [
                { "type": "text", "text": "be concise" }
            ],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "look up weather" },
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": "aW1n"
                            }
                        }
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "I should call the weather tool.",
                            "signature": "sig_opaque"
                        },
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "lookup_weather",
                            "input": { "city": "Paris" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": [
                            { "type": "text", "text": "sunny" }
                        ],
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
            "tool_choice": { "type": "tool", "name": "lookup_weather" },
            "max_tokens": 128,
            "temperature": 0.2,
            "top_p": 0.8,
            "top_k": 40,
            "stop_sequences": ["DONE"],
            "stream": true,
            "metadata": { "user_id": "u_1" }
        });

        let request = anthropic_request_to_ir(&body).unwrap();

        assert_eq!(request.model, "claude-sonnet-4-5");
        assert_eq!(
            request.system,
            Some(vec![ContentBlock::Text {
                text: "be concise".to_owned()
            }])
        );
        assert_eq!(
            request.messages[0],
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "look up weather".to_owned()
                    },
                    ContentBlock::Image(ImageSource::Base64 {
                        media_type: "image/png".to_owned(),
                        data: "aW1n".to_owned()
                    })
                ]
            }
        );
        assert_eq!(
            request.messages[1],
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking(Thinking {
                        text: Some("I should call the weather tool.".to_owned()),
                        opaque: Some(b"sig_opaque".to_vec()),
                        source: Provider::Anthropic,
                        echo_policy: EchoPolicy::Always,
                    }),
                    ContentBlock::ToolUse {
                        id: "toolu_1".to_owned(),
                        name: "lookup_weather".to_owned(),
                        input: json!({ "city": "Paris" })
                    }
                ]
            }
        );
        assert_eq!(
            request.messages[2],
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "sunny".to_owned()
                    }],
                    is_error: false,
                }]
            }
        );
        assert_eq!(
            request.tools,
            vec![ToolDef {
                name: "lookup_weather".to_owned(),
                description: Some("Fetch weather".to_owned()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                })
            }]
        );
        assert_eq!(
            request.tool_choice,
            ToolChoice::Tool("lookup_weather".to_owned())
        );
        assert_eq!(request.max_tokens, Some(128));
        assert_eq!(request.temperature, Some(0.2));
        assert_eq!(request.top_p, Some(0.8));
        assert_eq!(request.top_k, Some(40));
        assert_eq!(request.stop, vec!["DONE"]);
        assert!(request.stream);
        assert_eq!(
            request.extra,
            Map::from_iter([("metadata".to_owned(), json!({ "user_id": "u_1" }))])
        );
    }

    #[test]
    fn decodes_string_system_string_content_and_tool_choice_modes() {
        for (choice, expected) in [
            (json!({ "type": "auto" }), ToolChoice::Auto),
            (json!({ "type": "none" }), ToolChoice::None),
            (json!({ "type": "any" }), ToolChoice::Required),
        ] {
            let body = json!({
                "model": "claude-sonnet-4-5",
                "system": "be helpful",
                "messages": [{ "role": "user", "content": "hello" }],
                "tool_choice": choice
            });

            let request = anthropic_request_to_ir(&body).unwrap();

            assert_eq!(
                request.system,
                Some(vec![ContentBlock::Text {
                    text: "be helpful".to_owned()
                }])
            );
            assert_eq!(
                request.messages,
                vec![Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "hello".to_owned()
                    }]
                }]
            );
            assert_eq!(request.tool_choice, expected);
            assert!(!request.stream);
        }
    }

    #[test]
    fn unwraps_gateway_responses_signature_into_responses_thinking() {
        let signature = crate::reasoning::envelope::wrap_as_signature(
            &crate::reasoning::envelope::SourceBlock::new(
                Provider::Responses,
                b"enc_payload_from_responses".to_vec(),
            ),
        )
        .unwrap();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "thinking",
                    "thinking": "Need the weather tool.",
                    "signature": signature
                }]
            }]
        });

        let request = anthropic_request_to_ir(&body).unwrap();
        let ContentBlock::Thinking(thinking) = &request.messages[0].content[0] else {
            panic!("expected thinking block");
        };

        assert_eq!(thinking.text, Some("Need the weather tool.".to_owned()));
        assert_eq!(
            thinking.opaque,
            Some(b"enc_payload_from_responses".to_vec())
        );
        assert_eq!(thinking.source, Provider::Responses);
        assert_eq!(thinking.echo_policy, EchoPolicy::Always);
    }

    #[test]
    fn rejects_gateway_signature_with_non_responses_source() {
        let signature = crate::reasoning::envelope::wrap_as_signature(
            &crate::reasoning::envelope::SourceBlock::new(
                Provider::Anthropic,
                b"anthropic_signature".to_vec(),
            ),
        )
        .unwrap();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "thinking",
                    "thinking": "Need the weather tool.",
                    "signature": signature
                }]
            }]
        });

        let error = anthropic_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("expected Responses"))
        );
    }

    #[test]
    fn rejects_unknown_content_block_type() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{
                "role": "user",
                "content": [{ "type": "document", "source": {} }]
            }]
        });

        let error = anthropic_request_to_ir(&body).unwrap_err();

        assert!(matches!(
            error,
            ProxyError::UnsupportedFeature { feature, protocol }
                if feature == "content block type `document`" && protocol == PROTOCOL
        ));
    }
}
