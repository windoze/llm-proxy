//! Decoding for OpenAI Responses API requests.

// Later M3 tasks wire this staged decoder into HTTP routing and encoders.
#![allow(dead_code)]

use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, EchoPolicy, ImageSource, Message, Provider, Role, Thinking},
        request::{IrRequest, ToolChoice, ToolDef},
    },
};

const PROTOCOL: &str = "responses";
const CORE_REQUEST_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "developer",
    "tools",
    "tool_choice",
    "max_output_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop",
    "stream",
];

/// Converts an OpenAI Responses request body into the provider-neutral IR.
pub fn responses_request_to_ir(body: &Value) -> Result<IrRequest> {
    let request = body
        .as_object()
        .ok_or_else(|| mapping_error("request body must be a JSON object"))?;
    let mut system_blocks = Vec::new();

    if let Some(instructions) =
        decode_optional_content(request.get("instructions"), "request.instructions")?
    {
        system_blocks.extend(instructions);
    }
    if let Some(developer) = decode_optional_content(request.get("developer"), "request.developer")?
    {
        system_blocks.extend(developer);
    }

    let messages = decode_input(request.get("input"), &mut system_blocks)?;

    Ok(IrRequest {
        model: required_string(request, "model", "request.model")?.to_owned(),
        system: (!system_blocks.is_empty()).then_some(system_blocks),
        messages,
        tools: decode_tools(request.get("tools"))?,
        tool_choice: decode_tool_choice(request.get("tool_choice"))?,
        max_tokens: optional_u32(request, "max_output_tokens", "request.max_output_tokens")?,
        temperature: optional_f32(request, "temperature", "request.temperature")?,
        top_p: optional_f32(request, "top_p", "request.top_p")?,
        top_k: optional_u32(request, "top_k", "request.top_k")?,
        stop: decode_stop(request.get("stop"))?,
        stream: optional_bool(request, "stream", "request.stream")?.unwrap_or(false),
        extra: collect_extra(request),
    })
}

fn decode_input(
    value: Option<&Value>,
    system_blocks: &mut Vec<ContentBlock>,
) -> Result<Vec<Message>> {
    match value {
        Some(Value::String(text)) => Ok(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.clone() }],
        }]),
        Some(Value::Array(items)) => {
            let mut messages = Vec::new();
            for (index, item) in items.iter().enumerate() {
                decode_input_item(item, index, system_blocks, &mut messages)?;
            }
            Ok(messages)
        }
        Some(Value::Null) | None => Err(mapping_error("request.input is required")),
        Some(_) => Err(mapping_error("request.input must be a string or an array")),
    }
}

fn decode_input_item(
    value: &Value,
    index: usize,
    system_blocks: &mut Vec<ContentBlock>,
    messages: &mut Vec<Message>,
) -> Result<()> {
    let item = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("request.input[{index}] must be an object")))?;
    let path = format!("request.input[{index}]");
    let item_type = optional_string(item, "type", format!("{path}.type"))?.unwrap_or("message");

    match item_type {
        "message" => decode_message_item(item, path, system_blocks, messages),
        "function_call" => {
            messages.push(decode_function_call_item(item, path)?);
            Ok(())
        }
        "function_call_output" => {
            messages.push(decode_function_call_output_item(item, path)?);
            Ok(())
        }
        "reasoning" => {
            messages.push(decode_reasoning_item(item, path)?);
            Ok(())
        }
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("input item type `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_message_item(
    item: &Map<String, Value>,
    path: String,
    system_blocks: &mut Vec<ContentBlock>,
    messages: &mut Vec<Message>,
) -> Result<()> {
    let role = required_string(item, "role", format!("{path}.role"))?;
    let content = decode_required_content(item.get("content"), format!("{path}.content"))?;

    match role {
        "system" | "developer" => system_blocks.extend(content),
        "user" => messages.push(Message {
            role: Role::User,
            content,
        }),
        "assistant" => messages.push(Message {
            role: Role::Assistant,
            content,
        }),
        other => {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("message role `{other}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }
    }

    Ok(())
}

fn decode_function_call_item(item: &Map<String, Value>, path: String) -> Result<Message> {
    let arguments = required_string(item, "arguments", format!("{path}.arguments"))?;
    let input = serde_json::from_str(arguments)
        .map_err(|err| mapping_error(format!("{path}.arguments must be valid JSON: {err}")))?;

    Ok(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: required_string(item, "call_id", format!("{path}.call_id"))?.to_owned(),
            name: required_string(item, "name", format!("{path}.name"))?.to_owned(),
            input,
        }],
    })
}

fn decode_function_call_output_item(item: &Map<String, Value>, path: String) -> Result<Message> {
    Ok(Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: required_string(item, "call_id", format!("{path}.call_id"))?.to_owned(),
            content: decode_tool_output(item.get("output"), format!("{path}.output"))?,
            is_error: optional_bool(item, "is_error", format!("{path}.is_error"))?.unwrap_or(false),
        }],
    })
}

fn decode_reasoning_item(item: &Map<String, Value>, path: String) -> Result<Message> {
    let encrypted_content = required_string(
        item,
        "encrypted_content",
        format!("{path}.encrypted_content"),
    )?;

    Ok(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Thinking(Thinking {
            text: decode_reasoning_summary(item.get("summary"), format!("{path}.summary"))?,
            opaque: Some(encrypted_content.as_bytes().to_vec()),
            source: Provider::Responses,
            echo_policy: EchoPolicy::Always,
        })],
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
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .map(|(index, part)| decode_content_part(part, format!("{path}[{index}]")))
            .collect(),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!(
            "{path} must be a string or content-part array"
        ))),
    }
}

fn decode_tool_output(value: Option<&Value>, path: String) -> Result<Vec<ContentBlock>> {
    match value {
        Some(Value::String(output)) => Ok(vec![ContentBlock::Text {
            text: output.clone(),
        }]),
        Some(Value::Array(_)) => decode_required_content(value, path),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!(
            "{path} must be a string or content-part array"
        ))),
    }
}

fn decode_content_part(value: &Value, path: String) -> Result<ContentBlock> {
    let part = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let part_type = required_string(part, "type", format!("{path}.type"))?;

    match part_type {
        "input_text" | "output_text" | "text" => Ok(ContentBlock::Text {
            text: required_string(part, "text", format!("{path}.text"))?.to_owned(),
        }),
        "input_image" | "image_url" => decode_image_part(part, path),
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("content part type `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_image_part(part: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let image_url = match part.get("image_url") {
        Some(Value::String(url)) => url.as_str(),
        Some(Value::Object(image_url)) => {
            required_string(image_url, "url", format!("{path}.image_url.url"))?
        }
        Some(Value::Null) | None => {
            return Err(mapping_error(format!("{path}.image_url is required")));
        }
        Some(_) => {
            return Err(mapping_error(format!(
                "{path}.image_url must be a string or object"
            )));
        }
    };

    Ok(ContentBlock::Image(parse_image_source(image_url)))
}

fn parse_image_source(url: &str) -> ImageSource {
    const DATA_PREFIX: &str = "data:";
    const BASE64_MARKER: &str = ";base64,";

    if let Some(rest) = url.strip_prefix(DATA_PREFIX)
        && let Some((media_type, data)) = rest.split_once(BASE64_MARKER)
    {
        return ImageSource::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        };
    }

    ImageSource::Url(url.to_owned())
}

fn decode_reasoning_summary(value: Option<&Value>, path: String) -> Result<Option<String>> {
    let summaries = match value {
        Some(Value::String(summary)) => vec![summary.clone()],
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .map(|(index, part)| decode_reasoning_summary_part(part, format!("{path}[{index}]")))
            .collect::<Result<Vec<_>>>()?,
        Some(Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(mapping_error(format!(
                "{path} must be a string or summary array"
            )));
        }
    };

    let summary = summaries
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((!summary.is_empty()).then_some(summary))
}

fn decode_reasoning_summary_part(value: &Value, path: String) -> Result<String> {
    match value {
        Value::String(summary) => Ok(summary.clone()),
        Value::Object(part) => {
            required_string(part, "text", format!("{path}.text")).map(str::to_owned)
        }
        _ => Err(mapping_error(format!(
            "{path} must be a string or summary object"
        ))),
    }
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
    let tool_type = required_string(tool, "type", format!("{path}.type"))?;
    if tool_type != "function" {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("tool type `{tool_type}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }

    let description =
        optional_string(tool, "description", format!("{path}.description"))?.map(ToOwned::to_owned);
    let input_schema = tool
        .get("parameters")
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
        Some(Value::String(choice)) => match choice.as_str() {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" => Ok(ToolChoice::Required),
            other => Err(ProxyError::UnsupportedFeature {
                feature: format!("tool_choice `{other}`"),
                protocol: PROTOCOL.to_owned(),
            }),
        },
        Some(Value::Object(choice)) => decode_named_tool_choice(choice),
        Some(Value::Null) | None => Ok(ToolChoice::Auto),
        Some(_) => Err(mapping_error(
            "request.tool_choice must be a string or function-choice object",
        )),
    }
}

fn decode_named_tool_choice(choice: &Map<String, Value>) -> Result<ToolChoice> {
    let choice_type = required_string(choice, "type", "request.tool_choice.type")?;
    if choice_type != "function" {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("tool_choice type `{choice_type}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }

    if let Some(name) = optional_string(choice, "name", "request.tool_choice.name")? {
        return Ok(ToolChoice::Tool(name.to_owned()));
    }

    let function = choice
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error("request.tool_choice.function must be an object"))?;
    Ok(ToolChoice::Tool(
        required_string(function, "name", "request.tool_choice.function.name")?.to_owned(),
    ))
}

fn decode_stop(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        Some(Value::String(stop)) => Ok(vec![stop.clone()]),
        Some(Value::Array(stops)) => stops
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                stop.as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| mapping_error(format!("request.stop[{index}] must be a string")))
            })
            .collect(),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(mapping_error(
            "request.stop must be a string or an array of strings",
        )),
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
    fn decodes_codex_request_with_reasoning_and_function_calls() {
        let body = json!({
            "model": "gpt-5-codex",
            "instructions": "Use the repository context.",
            "developer": [
                { "type": "input_text", "text": "Prefer concise answers." }
            ],
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "look up weather" },
                        {
                            "type": "input_image",
                            "image_url": "data:image/png;base64,aW1n"
                        }
                    ]
                },
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{ "type": "summary_text", "text": "Need the weather tool." }],
                    "encrypted_content": "enc_payload",
                    "status": "completed"
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_weather",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}",
                    "status": "completed"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "sunny"
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "It is sunny." }]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "lookup_weather",
                "description": "Fetch weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }],
            "tool_choice": { "type": "function", "name": "lookup_weather" },
            "max_output_tokens": 256,
            "temperature": 0.2,
            "top_p": 0.8,
            "stream": true,
            "stop": ["DONE"],
            "store": false,
            "metadata": { "session": "s_1" }
        });

        let request = responses_request_to_ir(&body).unwrap();

        assert_eq!(request.model, "gpt-5-codex");
        assert_eq!(
            request.system,
            Some(vec![
                ContentBlock::Text {
                    text: "Use the repository context.".to_owned()
                },
                ContentBlock::Text {
                    text: "Prefer concise answers.".to_owned()
                }
            ])
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
                content: vec![ContentBlock::Thinking(Thinking {
                    text: Some("Need the weather tool.".to_owned()),
                    opaque: Some(b"enc_payload".to_vec()),
                    source: Provider::Responses,
                    echo_policy: EchoPolicy::Always,
                })]
            }
        );
        assert_eq!(
            request.messages[2],
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_weather".to_owned(),
                    name: "lookup_weather".to_owned(),
                    input: json!({ "city": "Paris" })
                }]
            }
        );
        assert_eq!(
            request.messages[3],
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_weather".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "sunny".to_owned()
                    }],
                    is_error: false,
                }]
            }
        );
        assert_eq!(
            request.messages[4],
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "It is sunny.".to_owned()
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
        assert_eq!(request.max_tokens, Some(256));
        assert_eq!(request.temperature, Some(0.2));
        assert_eq!(request.top_p, Some(0.8));
        assert_eq!(request.stop, vec!["DONE"]);
        assert!(request.stream);
        assert_eq!(
            request.extra,
            Map::from_iter([
                ("metadata".to_owned(), json!({ "session": "s_1" })),
                ("store".to_owned(), json!(false)),
            ])
        );
    }

    #[test]
    fn decodes_string_input_message_and_tool_choice_modes() {
        for (choice, expected) in [
            (json!("auto"), ToolChoice::Auto),
            (json!("none"), ToolChoice::None),
            (json!("required"), ToolChoice::Required),
            (
                json!({ "type": "function", "function": { "name": "lookup" } }),
                ToolChoice::Tool("lookup".to_owned()),
            ),
        ] {
            let body = json!({
                "model": "gpt-5-codex",
                "input": "hello",
                "tool_choice": choice
            });

            let request = responses_request_to_ir(&body).unwrap();

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
            assert_eq!(request.system, None);
            assert!(!request.stream);
        }
    }

    #[test]
    fn hoists_system_and_developer_input_messages() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [
                { "type": "message", "role": "system", "content": "System rules." },
                { "role": "developer", "content": "Developer rules." },
                { "role": "user", "content": "hi" }
            ]
        });

        let request = responses_request_to_ir(&body).unwrap();

        assert_eq!(
            request.system,
            Some(vec![
                ContentBlock::Text {
                    text: "System rules.".to_owned()
                },
                ContentBlock::Text {
                    text: "Developer rules.".to_owned()
                }
            ])
        );
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn rejects_invalid_function_arguments_json() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "lookup",
                "arguments": "{not-json}"
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("arguments must be valid JSON"))
        );
    }

    #[test]
    fn rejects_reasoning_without_encrypted_content() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "reasoning",
                "id": "rs_1",
                "summary": []
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message == "request.input[0].encrypted_content is required")
        );
    }
}
