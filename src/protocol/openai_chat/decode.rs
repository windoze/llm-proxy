//! Decoding for OpenAI Chat Completions-compatible requests.

// Later milestones wire this staged decoder into HTTP routing and encoders.
#![allow(dead_code)]

use serde_json::{Map, Number, Value};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, EchoPolicy, ImageSource, Message, Provider, Role, Thinking},
        request::{IrRequest, IrResponse, StopReason, ToolChoice, ToolDef, Usage},
    },
    provider::CapabilityProfile,
};

const PROTOCOL: &str = "openai_chat";
const CORE_REQUEST_FIELDS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop",
    "stream",
    "n",
    "reasoning_effort",
];

/// Converts an OpenAI Chat Completions request body into the provider-neutral IR.
pub fn chat_request_to_ir(body: &Value, profile: &dyn CapabilityProfile) -> Result<IrRequest> {
    let request = body
        .as_object()
        .ok_or_else(|| mapping_error("request body must be a JSON object"))?;
    let requested_model = required_string(request, "model", "request.model")?;
    let model = profile.map_model_name(requested_model);
    let blocklist = profile.param_blocklist(&model);

    let messages_value = request
        .get("messages")
        .ok_or_else(|| mapping_error("request.messages is required"))?;
    let message_values = messages_value
        .as_array()
        .ok_or_else(|| mapping_error("request.messages must be an array"))?;

    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();
    for (index, message_value) in message_values.iter().enumerate() {
        decode_message(
            message_value,
            index,
            profile,
            &model,
            &mut system_blocks,
            &mut messages,
        )?;
    }

    validate_choice_count(request, profile, blocklist)?;
    let max_tokens = match optional_u32_field(request, blocklist, "max_tokens")? {
        Some(value) => Some(value),
        None => optional_u32_field(request, blocklist, "max_completion_tokens")?,
    };

    Ok(IrRequest {
        model,
        system: (!system_blocks.is_empty()).then_some(system_blocks),
        messages,
        tools: decode_tools(request.get("tools"))?,
        tool_choice: decode_tool_choice(request.get("tool_choice"))?,
        max_tokens,
        temperature: optional_f32_field(request, blocklist, "temperature")?,
        top_p: optional_f32_field(request, blocklist, "top_p")?,
        top_k: optional_u32_field(request, blocklist, "top_k")?,
        stop: decode_stop(request.get("stop"))?,
        stream: optional_bool(request, "stream")?.unwrap_or(false),
        extra: collect_extra(request, profile, blocklist)?,
    })
}

/// Converts a non-streaming OpenAI Chat Completions response into the provider-neutral IR.
pub fn chat_response_to_ir(body: &Value) -> Result<IrResponse> {
    let response = body
        .as_object()
        .ok_or_else(|| mapping_error("response body must be a JSON object"))?;
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| mapping_error("response.choices must be an array"))?;
    let first_choice = choices
        .first()
        .ok_or_else(|| mapping_error("response.choices must contain at least one choice"))?;
    let first_choice = first_choice
        .as_object()
        .ok_or_else(|| mapping_error("response.choices[0] must be an object"))?;
    let message = first_choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error("response.choices[0].message must be an object"))?;

    Ok(IrResponse {
        id: required_string(response, "id", "response.id")?.to_owned(),
        model: required_string(response, "model", "response.model")?.to_owned(),
        content: decode_response_message_content(message)?,
        stop_reason: decode_finish_reason(first_choice.get("finish_reason"))?,
        usage: decode_usage(response.get("usage"))?,
    })
}

fn decode_message(
    value: &Value,
    index: usize,
    profile: &dyn CapabilityProfile,
    model: &str,
    system_blocks: &mut Vec<ContentBlock>,
    messages: &mut Vec<Message>,
) -> Result<()> {
    let message = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("messages[{index}] must be an object")))?;
    let role = required_string(message, "role", format!("messages[{index}].role"))?;

    match role {
        "system" | "developer" => {
            system_blocks.extend(decode_required_content(
                message.get("content"),
                format!("messages[{index}].content"),
            )?);
        }
        "user" => {
            messages.push(Message {
                role: Role::User,
                content: decode_required_content(
                    message.get("content"),
                    format!("messages[{index}].content"),
                )?,
            });
        }
        "assistant" => {
            messages.push(Message {
                role: Role::Assistant,
                content: decode_assistant_content(message, index, profile, model)?,
            });
        }
        "tool" => {
            messages.push(Message {
                role: Role::Tool,
                content: vec![decode_tool_result(message, index)?],
            });
        }
        other => {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("message role `{other}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }
    }

    Ok(())
}

fn decode_assistant_content(
    message: &Map<String, Value>,
    index: usize,
    profile: &dyn CapabilityProfile,
    model: &str,
) -> Result<Vec<ContentBlock>> {
    let has_tool_calls = message
        .get("tool_calls")
        .is_some_and(|value| !value.is_null());
    let has_reasoning = message
        .get("reasoning_content")
        .is_some_and(|value| !value.is_null());
    let mut content = Vec::new();

    if let Some(reasoning) = decode_reasoning_content(
        message.get("reasoning_content"),
        format!("messages[{index}].reasoning_content"),
        profile,
        model,
    )? {
        content.push(reasoning);
    }

    match message.get("content") {
        Some(Value::Null) | None if has_tool_calls || has_reasoning => {}
        content_value => content.extend(decode_required_content(
            content_value,
            format!("messages[{index}].content"),
        )?),
    }

    content.extend(decode_tool_calls(
        message.get("tool_calls"),
        format!("messages[{index}].tool_calls"),
    )?);

    Ok(content)
}

fn decode_response_message_content(message: &Map<String, Value>) -> Result<Vec<ContentBlock>> {
    let tool_calls = decode_tool_calls(
        message.get("tool_calls"),
        "response.choices[0].message.tool_calls".to_owned(),
    )?;
    let reasoning = decode_deepseek_reasoning_content(
        message.get("reasoning_content"),
        "response.choices[0].message.reasoning_content".to_owned(),
        EchoPolicy::OnlyWithToolCall,
    )?;
    let mut content = Vec::new();
    let has_reasoning = reasoning.is_some();
    let has_tool_calls = !tool_calls.is_empty();

    if let Some(reasoning) = reasoning {
        content.push(reasoning);
    }

    match message.get("content") {
        Some(Value::Null) | None if has_tool_calls || has_reasoning => {}
        content_value => content.extend(decode_required_content(
            content_value,
            "response.choices[0].message.content",
        )?),
    }

    content.extend(tool_calls);
    Ok(content)
}

fn decode_finish_reason(value: Option<&Value>) -> Result<StopReason> {
    let finish_reason = value
        .and_then(Value::as_str)
        .ok_or_else(|| mapping_error("response.choices[0].finish_reason must be a string"))?;

    Ok(match finish_reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_owned()),
    })
}

fn decode_usage(value: Option<&Value>) -> Result<Usage> {
    let usage = value
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error("response.usage must be an object"))?;

    Ok(Usage {
        input_tokens: required_u32(usage, "prompt_tokens", "response.usage.prompt_tokens")?,
        output_tokens: required_u32(
            usage,
            "completion_tokens",
            "response.usage.completion_tokens",
        )?,
        cache_read: optional_u32(
            usage,
            "prompt_cache_hit_tokens",
            "response.usage.prompt_cache_hit_tokens",
        )?,
        cache_write: optional_u32(
            usage,
            "prompt_cache_miss_tokens",
            "response.usage.prompt_cache_miss_tokens",
        )?,
    })
}

fn decode_tool_result(message: &Map<String, Value>, index: usize) -> Result<ContentBlock> {
    let tool_use_id = required_string(
        message,
        "tool_call_id",
        format!("messages[{index}].tool_call_id"),
    )?
    .to_owned();
    let content =
        decode_required_content(message.get("content"), format!("messages[{index}].content"))?;

    Ok(ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error: false,
    })
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

fn decode_content_part(value: &Value, path: String) -> Result<ContentBlock> {
    let part = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let part_type = required_string(part, "type", format!("{path}.type"))?;

    match part_type {
        "text" => Ok(ContentBlock::Text {
            text: required_string(part, "text", format!("{path}.text"))?.to_owned(),
        }),
        "image_url" => decode_image_url(part, path),
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("content part type `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_image_url(part: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let image_url = part
        .get("image_url")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error(format!("{path}.image_url must be an object")))?;
    let url = required_string(image_url, "url", format!("{path}.image_url.url"))?;

    Ok(ContentBlock::Image(parse_image_source(url)))
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

fn decode_reasoning_content(
    value: Option<&Value>,
    path: String,
    profile: &dyn CapabilityProfile,
    model: &str,
) -> Result<Option<ContentBlock>> {
    decode_deepseek_reasoning_content(value, path, profile.reasoning_echo_policy(model))
}

fn decode_deepseek_reasoning_content(
    value: Option<&Value>,
    path: String,
    echo_policy: EchoPolicy,
) -> Result<Option<ContentBlock>> {
    match value {
        Some(Value::String(text)) => Ok(Some(ContentBlock::Thinking(Thinking {
            text: Some(text.clone()),
            opaque: None,
            source: Provider::DeepSeek,
            echo_policy,
        }))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be a string"))),
    }
}

fn decode_tool_calls(value: Option<&Value>, path: String) -> Result<Vec<ContentBlock>> {
    match value {
        Some(Value::Array(tool_calls)) => tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| decode_tool_call(tool_call, format!("{path}[{index}]")))
            .collect(),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(mapping_error(format!("{path} must be an array"))),
    }
}

fn decode_tool_call(value: &Value, path: String) -> Result<ContentBlock> {
    let tool_call = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let tool_type = required_string(tool_call, "type", format!("{path}.type"))?;
    if tool_type != "function" {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("tool call type `{tool_type}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }

    let function = tool_call
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error(format!("{path}.function must be an object")))?;
    let arguments = required_string(function, "arguments", format!("{path}.function.arguments"))?;
    let input = serde_json::from_str(arguments).map_err(|err| {
        mapping_error(format!(
            "{path}.function.arguments must be valid JSON: {err}"
        ))
    })?;

    Ok(ContentBlock::ToolUse {
        id: required_string(tool_call, "id", format!("{path}.id"))?.to_owned(),
        name: required_string(function, "name", format!("{path}.function.name"))?.to_owned(),
        input,
    })
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

    let function = tool
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error(format!("{path}.function must be an object")))?;
    let description = optional_string(
        function,
        "description",
        format!("{path}.function.description"),
    )?
    .map(ToOwned::to_owned);
    let input_schema = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    Ok(ToolDef {
        name: required_string(function, "name", format!("{path}.function.name"))?.to_owned(),
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

fn collect_extra(
    request: &Map<String, Value>,
    profile: &dyn CapabilityProfile,
    blocklist: &[&str],
) -> Result<Map<String, Value>> {
    let mut extra = Map::new();

    for (key, value) in request {
        if CORE_REQUEST_FIELDS.contains(&key.as_str()) || is_blocklisted(blocklist, key) {
            continue;
        }
        extra.insert(key.clone(), value.clone());
    }

    if !is_blocklisted(blocklist, "reasoning_effort")
        && let Some(reasoning_effort) =
            optional_string(request, "reasoning_effort", "request.reasoning_effort")?
    {
        extra.insert(
            "reasoning_effort".to_owned(),
            Value::String(
                profile
                    .normalize_reasoning_effort(reasoning_effort)
                    .to_owned(),
            ),
        );
    }

    if !is_blocklisted(blocklist, "n")
        && let Some(choice_count) = optional_u32(request, "n", "request.n")?
    {
        extra.insert(
            "n".to_owned(),
            Value::Number(Number::from(u64::from(choice_count))),
        );
    }

    Ok(extra)
}

fn validate_choice_count(
    request: &Map<String, Value>,
    profile: &dyn CapabilityProfile,
    blocklist: &[&str],
) -> Result<()> {
    if is_blocklisted(blocklist, "n") {
        return Ok(());
    }

    if let Some(choice_count) = optional_u32(request, "n", "request.n")?
        && choice_count > 1
        && !profile.supports_multiple_choices()
    {
        return Err(ProxyError::UnsupportedFeature {
            feature: "n > 1".to_owned(),
            protocol: PROTOCOL.to_owned(),
        });
    }

    Ok(())
}

fn optional_u32_field(
    request: &Map<String, Value>,
    blocklist: &[&str],
    field: &'static str,
) -> Result<Option<u32>> {
    if is_blocklisted(blocklist, field) {
        return Ok(None);
    }

    optional_u32(request, field, format!("request.{field}"))
}

fn optional_f32_field(
    request: &Map<String, Value>,
    blocklist: &[&str],
    field: &'static str,
) -> Result<Option<f32>> {
    if is_blocklisted(blocklist, field) {
        return Ok(None);
    }

    match request.get(field) {
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|value| Some(value as f32))
            .ok_or_else(|| mapping_error(format!("request.{field} must be a finite number"))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("request.{field} must be a number"))),
    }
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

fn required_u32(object: &Map<String, Value>, field: &str, path: impl Into<String>) -> Result<u32> {
    let path = path.into();
    optional_u32(object, field, path.clone())
        .and_then(|value| value.ok_or_else(|| mapping_error(format!("{path} is required"))))
}

fn optional_bool(object: &Map<String, Value>, field: &str) -> Result<Option<bool>> {
    match object.get(field) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("request.{field} must be a boolean"))),
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

fn is_blocklisted(blocklist: &[&str], field: &str) -> bool {
    blocklist.contains(&field)
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        ir::message::EchoPolicy,
        provider::{GenericOpenAi, deepseek::DeepSeek},
    };

    #[test]
    fn decodes_deepseek_request_with_reasoning_and_tools() {
        let body = json!({
            "model": "deepseek-reasoner",
            "messages": [
                { "role": "system", "content": "be concise" },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "look up weather" },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": "data:image/png;base64,aW1n"
                            }
                        }
                    ]
                },
                {
                    "role": "assistant",
                    "reasoning_content": "I should call the weather tool.",
                    "content": "Checking.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup_weather",
                            "arguments": "{\"city\":\"Paris\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "sunny"
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Fetch weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        }
                    }
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "lookup_weather" }
            },
            "max_tokens": 128,
            "temperature": 0.2,
            "top_p": 0.8,
            "top_k": 40,
            "stop": "DONE",
            "stream": true,
            "reasoning_effort": "low",
            "response_format": { "type": "json_object" }
        });

        let request = chat_request_to_ir(&body, &DeepSeek).unwrap();

        assert_eq!(request.model, "deepseek-reasoner");
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
                        opaque: None,
                        source: Provider::DeepSeek,
                        echo_policy: EchoPolicy::OnlyWithToolCall,
                    }),
                    ContentBlock::Text {
                        text: "Checking.".to_owned()
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_owned(),
                        name: "lookup_weather".to_owned(),
                        input: json!({ "city": "Paris" })
                    }
                ]
            }
        );
        assert_eq!(
            request.messages[2],
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_owned(),
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
        assert_eq!(request.temperature, None);
        assert_eq!(request.top_p, None);
        assert_eq!(request.top_k, Some(40));
        assert_eq!(request.stop, vec!["DONE"]);
        assert!(request.stream);
        assert_eq!(
            request.extra,
            Map::from_iter([
                ("reasoning_effort".to_owned(), json!("high")),
                (
                    "response_format".to_owned(),
                    json!({ "type": "json_object" })
                ),
            ])
        );
    }

    #[test]
    fn decodes_deepseek_response_with_reasoning_tools_and_usage() {
        let body = json!({
            "id": "chatcmpl_1",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "I need the weather tool.",
                    "content": "Calling the weather tool.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup_weather",
                            "arguments": "{\"city\":\"Paris\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 9,
                "total_tokens": 51,
                "prompt_cache_hit_tokens": 10,
                "prompt_cache_miss_tokens": 32
            }
        });

        let response = chat_response_to_ir(&body).unwrap();

        assert_eq!(
            response,
            IrResponse {
                id: "chatcmpl_1".to_owned(),
                model: "deepseek-reasoner".to_owned(),
                content: vec![
                    ContentBlock::Thinking(Thinking {
                        text: Some("I need the weather tool.".to_owned()),
                        opaque: None,
                        source: Provider::DeepSeek,
                        echo_policy: EchoPolicy::OnlyWithToolCall,
                    }),
                    ContentBlock::Text {
                        text: "Calling the weather tool.".to_owned()
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_owned(),
                        name: "lookup_weather".to_owned(),
                        input: json!({ "city": "Paris" })
                    }
                ],
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 42,
                    output_tokens: 9,
                    cache_read: Some(10),
                    cache_write: Some(32),
                },
            }
        );
    }

    #[test]
    fn decodes_text_response_finish_reasons_and_usage_without_cache() {
        for (finish_reason, expected_stop_reason) in [
            ("stop", StopReason::EndTurn),
            ("length", StopReason::MaxTokens),
        ] {
            let body = json!({
                "id": format!("chatcmpl_{finish_reason}"),
                "model": "gpt-4.1",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "done"
                    },
                    "finish_reason": finish_reason
                }],
                "usage": {
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10
                }
            });

            let response = chat_response_to_ir(&body).unwrap();

            assert_eq!(response.id, format!("chatcmpl_{finish_reason}"));
            assert_eq!(response.model, "gpt-4.1");
            assert_eq!(
                response.content,
                vec![ContentBlock::Text {
                    text: "done".to_owned()
                }]
            );
            assert_eq!(response.stop_reason, expected_stop_reason);
            assert_eq!(
                response.usage,
                Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    cache_read: None,
                    cache_write: None,
                }
            );
        }
    }

    #[test]
    fn keeps_standard_openai_sampling_params() {
        let body = json!({
            "model": "gpt-4.1",
            "messages": [{ "role": "user", "content": "hello" }],
            "tool_choice": "required",
            "max_completion_tokens": 64,
            "temperature": 0.7,
            "top_p": 0.9,
            "stop": ["END", "STOP"],
            "n": 1
        });

        let request = chat_request_to_ir(&body, &GenericOpenAi::default()).unwrap();

        assert_eq!(request.model, "gpt-4.1");
        assert_eq!(request.max_tokens, Some(64));
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.top_p, Some(0.9));
        assert_eq!(request.stop, vec!["END", "STOP"]);
        assert_eq!(request.tool_choice, ToolChoice::Required);
        assert_eq!(request.extra, Map::from_iter([("n".to_owned(), json!(1))]));
    }

    #[test]
    fn rejects_multiple_choices_for_deepseek() {
        let body = json!({
            "model": "deepseek-chat",
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 2
        });

        let error = chat_request_to_ir(&body, &DeepSeek).unwrap_err();

        assert!(matches!(
            error,
            ProxyError::UnsupportedFeature { feature, protocol }
                if feature == "n > 1" && protocol == PROTOCOL
        ));
    }

    #[test]
    fn rejects_invalid_tool_call_arguments() {
        let body = json!({
            "model": "gpt-4.1",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "arguments": "{not json"
                    }
                }]
            }]
        });

        let error = chat_request_to_ir(&body, &GenericOpenAi::default()).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("valid JSON"))
        );
    }
}
