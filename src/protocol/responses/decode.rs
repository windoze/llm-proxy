//! Decoding for OpenAI Responses API requests.

// Later M3 tasks wire this staged decoder into HTTP routing and encoders.
#![allow(dead_code)]

use serde_json::{Map, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, EchoPolicy, ImageSource, Message, Provider, Role, Thinking},
        request::{IrRequest, IrResponse, StopReason, ToolChoice, ToolDef, Usage},
    },
    protocol::tool_ids::validate_responses_tool_result_pairs,
    reasoning::envelope::{
        SourceBlock, is_reasoning_envelope, unwrap_from_responses_reasoning_item,
    },
};

use super::reasoning::{
    encode_preserved_reasoning_item, encrypted_content, normalize_reasoning_item,
};

const PROTOCOL: &str = "responses";
const GATEWAY_REASONING_ID_PREFIX: &str = "rs_llm_proxy";
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

    let ir_request = IrRequest {
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
    };
    validate_responses_tool_result_pairs(&ir_request)?;
    Ok(ir_request)
}

/// Converts a non-streaming OpenAI Responses response body into the provider-neutral IR.
pub fn responses_response_to_ir(body: &Value) -> Result<IrResponse> {
    let response = body
        .as_object()
        .ok_or_else(|| mapping_error("response body must be a JSON object"))?;
    let content = decode_response_output(response.get("output"))?;

    Ok(IrResponse {
        id: required_string(response, "id", "response.id")?.to_owned(),
        model: required_string(response, "model", "response.model")?.to_owned(),
        stop_reason: decode_response_stop_reason(response, &content)?,
        usage: decode_response_usage(response.get("usage"))?,
        content,
    })
}

fn decode_response_output(value: Option<&Value>) -> Result<Vec<ContentBlock>> {
    let output = value
        .and_then(Value::as_array)
        .ok_or_else(|| mapping_error("response.output must be an array"))?;
    let mut content = Vec::new();

    for (index, item) in output.iter().enumerate() {
        decode_response_output_item(item, index, &mut content)?;
    }

    Ok(content)
}

fn decode_response_output_item(
    value: &Value,
    index: usize,
    content: &mut Vec<ContentBlock>,
) -> Result<()> {
    let item = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("response.output[{index}] must be an object")))?;
    let path = format!("response.output[{index}]");
    let item_type = required_string(item, "type", format!("{path}.type"))?;

    match item_type {
        "message" => decode_response_message_item(item, path, content),
        "reasoning" => {
            content.push(decode_response_reasoning_item(item, path)?);
            Ok(())
        }
        "function_call" => {
            content.push(decode_response_function_call_item(item, path)?);
            Ok(())
        }
        "function_call_output" => {
            content.push(decode_response_function_call_output_item(item, path)?);
            Ok(())
        }
        other => Err(ProxyError::UnsupportedFeature {
            feature: format!("output item type `{other}`"),
            protocol: PROTOCOL.to_owned(),
        }),
    }
}

fn decode_response_message_item(
    item: &Map<String, Value>,
    path: String,
    content: &mut Vec<ContentBlock>,
) -> Result<()> {
    let role = required_string(item, "role", format!("{path}.role"))?;
    if role != "assistant" {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("response message role `{role}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }

    content.extend(decode_required_content(
        item.get("content"),
        format!("{path}.content"),
    )?);
    Ok(())
}

fn decode_response_reasoning_item(item: &Map<String, Value>, path: String) -> Result<ContentBlock> {
    let reasoning_item = normalize_reasoning_item(item, &path)?;
    let encrypted_content =
        encrypted_content(&reasoning_item, format!("{path}.encrypted_content"))?;

    Ok(ContentBlock::Thinking(Thinking {
        text: decode_reasoning_summary(reasoning_item.get("summary"), format!("{path}.summary"))?,
        opaque: Some(encrypted_content.as_bytes().to_vec()),
        source: Provider::Responses,
        echo_policy: EchoPolicy::Always,
    }))
}

fn decode_response_function_call_item(
    item: &Map<String, Value>,
    path: String,
) -> Result<ContentBlock> {
    let arguments = required_string(item, "arguments", format!("{path}.arguments"))?;
    let input = serde_json::from_str(arguments)
        .map_err(|err| mapping_error(format!("{path}.arguments must be valid JSON: {err}")))?;

    Ok(ContentBlock::ToolUse {
        id: required_string(item, "call_id", format!("{path}.call_id"))?.to_owned(),
        name: required_string(item, "name", format!("{path}.name"))?.to_owned(),
        input,
    })
}

fn decode_response_function_call_output_item(
    item: &Map<String, Value>,
    path: String,
) -> Result<ContentBlock> {
    Ok(ContentBlock::ToolResult {
        tool_use_id: required_string(item, "call_id", format!("{path}.call_id"))?.to_owned(),
        content: decode_tool_output(item.get("output"), format!("{path}.output"))?,
        is_error: optional_bool(item, "is_error", format!("{path}.is_error"))?.unwrap_or(false),
    })
}

fn decode_response_stop_reason(
    response: &Map<String, Value>,
    content: &[ContentBlock],
) -> Result<StopReason> {
    let status = required_string(response, "status", "response.status")?;
    match status {
        "completed" => {
            if content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
            {
                Ok(StopReason::ToolUse)
            } else {
                Ok(StopReason::EndTurn)
            }
        }
        "incomplete" => decode_incomplete_reason(response.get("incomplete_details")),
        other => Ok(StopReason::Other(other.to_owned())),
    }
}

fn decode_incomplete_reason(value: Option<&Value>) -> Result<StopReason> {
    let Some(value) = value else {
        return Ok(StopReason::Other("incomplete".to_owned()));
    };
    let Some(details) = value.as_object() else {
        return Err(mapping_error(
            "response.incomplete_details must be an object when present",
        ));
    };
    let reason = optional_string(details, "reason", "response.incomplete_details.reason")?
        .unwrap_or("incomplete");

    Ok(match reason {
        "max_output_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_owned()),
    })
}

fn decode_response_usage(value: Option<&Value>) -> Result<Usage> {
    let usage = value
        .and_then(Value::as_object)
        .ok_or_else(|| mapping_error("response.usage must be an object"))?;
    let cache_read = usage
        .get("input_tokens_details")
        .and_then(Value::as_object)
        .map(|details| {
            optional_u32(
                details,
                "cached_tokens",
                "response.usage.input_tokens_details.cached_tokens",
            )
        })
        .transpose()?
        .flatten();

    Ok(Usage {
        input_tokens: required_u32(usage, "input_tokens", "response.usage.input_tokens")?,
        output_tokens: required_u32(usage, "output_tokens", "response.usage.output_tokens")?,
        cache_read,
        cache_write: None,
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
    let reasoning_item = normalize_reasoning_item(item, &path)?;
    let encrypted_content =
        encrypted_content(&reasoning_item, format!("{path}.encrypted_content"))?;
    if let Some(thinking) =
        decode_gateway_anthropic_reasoning_item(&reasoning_item, encrypted_content, &path)?
    {
        return Ok(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking(thinking)],
        });
    }

    Ok(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Thinking(Thinking {
            text: decode_reasoning_summary(
                reasoning_item.get("summary"),
                format!("{path}.summary"),
            )?,
            opaque: Some(encode_preserved_reasoning_item(reasoning_item)?),
            source: Provider::Responses,
            echo_policy: EchoPolicy::Always,
        })],
    })
}

fn decode_gateway_anthropic_reasoning_item(
    reasoning_item: &Map<String, Value>,
    encrypted_content: &str,
    path: &str,
) -> Result<Option<Thinking>> {
    let item_value = Value::Object(reasoning_item.clone());
    let source_block = match unwrap_from_responses_reasoning_item(&item_value) {
        Ok(source_block) => source_block,
        Err(err)
            if has_gateway_reasoning_id(reasoning_item)
                || is_reasoning_envelope(encrypted_content) =>
        {
            return Err(mapping_error(format!(
                "{path}.encrypted_content gateway envelope is invalid: {err}"
            )));
        }
        Err(_) => return Ok(None),
    };

    anthropic_thinking_from_source_block(source_block, path).map(Some)
}

fn has_gateway_reasoning_id(reasoning_item: &Map<String, Value>) -> bool {
    reasoning_item
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with(GATEWAY_REASONING_ID_PREFIX))
}

fn anthropic_thinking_from_source_block(source_block: SourceBlock, path: &str) -> Result<Thinking> {
    if source_block.source != Provider::Anthropic {
        return Err(mapping_error(format!(
            "{path}.encrypted_content envelope has source {:?}, expected Anthropic",
            source_block.source
        )));
    }

    let payload = source_block.payload_json()?;
    let block = payload.as_object().ok_or_else(|| {
        mapping_error(format!(
            "{path}.encrypted_content payload must be an object"
        ))
    })?;
    let block_type = required_string(
        block,
        "type",
        format!("{path}.encrypted_content.payload.type"),
    )?;
    if block_type != "thinking" {
        return Err(mapping_error(format!(
            "{path}.encrypted_content payload.type must be `thinking`"
        )));
    }
    let thinking_text = required_string(
        block,
        "thinking",
        format!("{path}.encrypted_content.payload.thinking"),
    )?;
    let signature = required_string(
        block,
        "signature",
        format!("{path}.encrypted_content.payload.signature"),
    )?;

    Ok(Thinking {
        text: Some(thinking_text.to_owned()),
        opaque: Some(signature.as_bytes().to_vec()),
        source: Provider::Anthropic,
        echo_policy: EchoPolicy::Always,
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
        Some(Value::Array(tools)) => {
            let mut decoded_tools = Vec::new();
            for (index, tool) in tools.iter().enumerate() {
                decode_tool(tool, format!("request.tools[{index}]"), &mut decoded_tools)?;
            }
            Ok(decoded_tools)
        }
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(mapping_error("request.tools must be an array")),
    }
}

fn decode_tool(value: &Value, path: String, output: &mut Vec<ToolDef>) -> Result<()> {
    let tool = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let tool_type = required_string(tool, "type", format!("{path}.type"))?;

    match tool_type {
        "function" => output.push(decode_function_tool(tool, path)?),
        "custom" => output.push(decode_custom_tool(tool, path)?),
        "namespace" => decode_namespace_tool(tool, path, output)?,
        "tool_search" => output.push(decode_tool_search_tool(tool, path)?),
        "web_search" => decode_web_search_tool(tool, path)?,
        other => {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("tool type `{other}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }
    }

    Ok(())
}

fn decode_function_tool(tool: &Map<String, Value>, path: String) -> Result<ToolDef> {
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

fn decode_custom_tool(tool: &Map<String, Value>, path: String) -> Result<ToolDef> {
    let description = custom_tool_description(optional_string(
        tool,
        "description",
        format!("{path}.description"),
    )?);

    Ok(ToolDef {
        name: required_string(tool, "name", format!("{path}.name"))?.to_owned(),
        description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Free-form input for the original Responses custom tool."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    })
}

fn custom_tool_description(description: Option<&str>) -> Option<String> {
    const NOTE: &str =
        "Original Responses custom tool; send its free-form payload in the `input` field.";

    Some(match description {
        Some(description) if !description.is_empty() => format!("{description}\n\n{NOTE}"),
        _ => NOTE.to_owned(),
    })
}

fn decode_namespace_tool(
    tool: &Map<String, Value>,
    path: String,
    output: &mut Vec<ToolDef>,
) -> Result<()> {
    let namespace_name = required_string(tool, "name", format!("{path}.name"))?;
    let namespace_description =
        optional_string(tool, "description", format!("{path}.description"))?;
    let tools = tool
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| mapping_error(format!("{path}.tools must be an array")))?;

    for (index, nested_tool) in tools.iter().enumerate() {
        let nested_path = format!("{path}.tools[{index}]");
        let nested_tool = nested_tool
            .as_object()
            .ok_or_else(|| mapping_error(format!("{nested_path} must be an object")))?;
        let nested_type = required_string(nested_tool, "type", format!("{nested_path}.type"))?;
        if nested_type != "function" {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("namespace tool type `{nested_type}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }

        let mut decoded_tool = decode_function_tool(nested_tool, nested_path)?;
        decoded_tool.description = namespace_scoped_description(
            namespace_name,
            namespace_description,
            decoded_tool.description,
        );
        output.push(decoded_tool);
    }

    Ok(())
}

fn decode_tool_search_tool(tool: &Map<String, Value>, path: String) -> Result<ToolDef> {
    let description =
        optional_string(tool, "description", format!("{path}.description"))?.map(ToOwned::to_owned);
    let input_schema = tool
        .get("parameters")
        .cloned()
        .unwrap_or_else(default_tool_search_schema);

    Ok(ToolDef {
        name: "tool_search".to_owned(),
        description,
        input_schema,
    })
}

fn default_tool_search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "limit": { "type": "number" }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn decode_web_search_tool(tool: &Map<String, Value>, path: String) -> Result<()> {
    let external_web_access = optional_bool(
        tool,
        "external_web_access",
        format!("{path}.external_web_access"),
    )?
    .unwrap_or(false);
    if external_web_access {
        return Err(ProxyError::UnsupportedFeature {
            feature: "web_search tool with external_web_access=true".to_owned(),
            protocol: PROTOCOL.to_owned(),
        });
    }
    Ok(())
}

fn namespace_scoped_description(
    namespace_name: &str,
    namespace_description: Option<&str>,
    tool_description: Option<String>,
) -> Option<String> {
    let namespace_prefix = match namespace_description {
        Some(description) if !description.is_empty() => {
            format!("Namespace `{namespace_name}`: {description}")
        }
        _ => format!("Namespace `{namespace_name}`."),
    };

    Some(match tool_description {
        Some(description) if !description.is_empty() => {
            format!("{namespace_prefix}\n\n{description}")
        }
        _ => namespace_prefix,
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
    if choice_type != "function" && choice_type != "custom" {
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

fn required_u32(object: &Map<String, Value>, field: &str, path: impl Into<String>) -> Result<u32> {
    let path = path.into();
    optional_u32(object, field, path.clone())
        .and_then(|value| value.ok_or_else(|| mapping_error(format!("{path} is required"))))
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
    use crate::{
        ir::request::{IrResponse, StopReason, Usage},
        protocol::{
            responses::encode::ir_response_to_responses,
            tool_ids::responses_tool_id_map_from_request,
        },
    };

    #[test]
    fn decodes_response_reasoning_item_to_ir_thinking() {
        let body = json!({
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "model": "gpt-5-codex",
            "output": [
                {
                    "id": "rs_1",
                    "type": "reasoning",
                    "status": "completed",
                    "summary": [{ "type": "summary_text", "text": "Need the weather tool." }],
                    "encrypted_content": "enc_payload",
                    "provider_metadata": { "kept_by_backend": true }
                },
                {
                    "id": "msg_1",
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
                    "id": "fc_1",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_weather",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                }
            ],
            "usage": {
                "input_tokens": 42,
                "input_tokens_details": { "cached_tokens": 10 },
                "output_tokens": 9,
                "output_tokens_details": { "reasoning_tokens": 4 },
                "total_tokens": 51
            }
        });

        let response = responses_response_to_ir(&body).unwrap();

        assert_eq!(response.id, "resp_1");
        assert_eq!(response.model, "gpt-5-codex");
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 42,
                output_tokens: 9,
                cache_read: Some(10),
                cache_write: None,
            }
        );
        let ContentBlock::Thinking(thinking) = &response.content[0] else {
            panic!("expected Responses reasoning output to decode as a thinking block");
        };
        assert_eq!(thinking.text, Some("Need the weather tool.".to_owned()));
        assert_eq!(thinking.opaque, Some(b"enc_payload".to_vec()));
        assert_eq!(thinking.source, Provider::Responses);
        assert_eq!(thinking.echo_policy, EchoPolicy::Always);
        assert_eq!(
            response.content[1],
            ContentBlock::Text {
                text: "Calling the weather tool.".to_owned()
            }
        );
        assert_eq!(
            response.content[2],
            ContentBlock::ToolUse {
                id: "call_weather".to_owned(),
                name: "lookup_weather".to_owned(),
                input: json!({ "city": "Paris" }),
            }
        );
    }

    #[test]
    fn rejects_response_reasoning_without_encrypted_content() {
        let body = json!({
            "id": "resp_missing_reasoning",
            "status": "completed",
            "model": "gpt-5-codex",
            "output": [{
                "id": "rs_1",
                "type": "reasoning",
                "summary": []
            }],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        });

        let error = responses_response_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message == "response.output[0].encrypted_content is required")
        );
    }

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
        assert_eq!(request.messages[1].role, Role::Assistant);
        let ContentBlock::Thinking(thinking) = &request.messages[1].content[0] else {
            panic!("expected Responses reasoning item to decode as a thinking block");
        };
        let preserved_item: Value =
            serde_json::from_slice(thinking.opaque.as_deref().unwrap()).unwrap();
        assert_eq!(thinking.text, Some("Need the weather tool.".to_owned()));
        assert_eq!(thinking.source, Provider::Responses);
        assert_eq!(thinking.echo_policy, EchoPolicy::Always);
        assert_eq!(preserved_item["type"], "reasoning");
        assert_eq!(preserved_item["encrypted_content"], "enc_payload");
        assert_eq!(preserved_item["status"], "completed");
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
    fn unwraps_gateway_anthropic_reasoning_input_to_anthropic_thinking() {
        let source_block = crate::reasoning::envelope::SourceBlock::from_json(
            Provider::Anthropic,
            &json!({
                "type": "thinking",
                "thinking": "Need the weather tool.",
                "signature": "sig_real_anthropic_123"
            }),
        )
        .unwrap();
        let wrapped =
            crate::reasoning::envelope::wrap_as_responses_reasoning_item(&source_block).unwrap();
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "reasoning",
                "summary": [],
                "encrypted_content": wrapped["encrypted_content"]
            }]
        });

        let request = responses_request_to_ir(&body).unwrap();
        let ContentBlock::Thinking(thinking) = &request.messages[0].content[0] else {
            panic!("expected gateway reasoning item to decode as thinking");
        };

        assert_eq!(thinking.text, Some("Need the weather tool.".to_owned()));
        assert_eq!(thinking.opaque, Some(b"sig_real_anthropic_123".to_vec()));
        assert_eq!(thinking.source, Provider::Anthropic);
        assert_eq!(thinking.echo_policy, EchoPolicy::Always);
    }

    #[test]
    fn rejects_tampered_gateway_anthropic_reasoning_input() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let source_block = crate::reasoning::envelope::SourceBlock::from_json(
            Provider::Anthropic,
            &json!({
                "type": "thinking",
                "thinking": "Need the weather tool.",
                "signature": "sig_real_anthropic_123"
            }),
        )
        .unwrap();
        let wrapped =
            crate::reasoning::envelope::wrap_as_responses_reasoning_item(&source_block).unwrap();
        let encrypted_content = wrapped["encrypted_content"].as_str().unwrap();
        let envelope_bytes = STANDARD.decode(encrypted_content).unwrap();
        let mut envelope: Value = serde_json::from_slice(&envelope_bytes).unwrap();
        envelope["checksum"] = json!(0);
        let tampered_encrypted_content = STANDARD.encode(serde_json::to_vec(&envelope).unwrap());
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "reasoning",
                "summary": [],
                "encrypted_content": tampered_encrypted_content
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(matches!(error, ProxyError::ProtocolMapping(message)
                if message.contains("gateway envelope is invalid")));
    }

    #[test]
    fn preserves_reasoning_item_fields_and_omits_null_status_on_round_trip() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "reasoning",
                "id": "rs_original",
                "summary": [{ "type": "summary_text", "text": "Need a tool." }],
                "encrypted_content": "opaque-responses-token",
                "status": null,
                "provider_metadata": { "kept": true }
            }]
        });

        let request = responses_request_to_ir(&body).unwrap();
        let ContentBlock::Thinking(thinking) = &request.messages[0].content[0] else {
            panic!("expected Responses reasoning item to decode as a thinking block");
        };
        let encoded = ir_response_to_responses(&IrResponse {
            id: "resp_preserve".to_owned(),
            model: request.model,
            content: vec![ContentBlock::Thinking(thinking.clone())],
            stop_reason: StopReason::MaxTokens,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read: None,
                cache_write: None,
            },
        })
        .unwrap();
        let item = encoded["output"][0].as_object().unwrap();

        assert_eq!(item.get("id").unwrap(), "rs_original");
        assert_eq!(item.get("type").unwrap(), "reasoning");
        assert_eq!(
            item.get("encrypted_content").unwrap(),
            "opaque-responses-token"
        );
        assert_eq!(item.get("summary").unwrap()[0]["text"], "Need a tool.");
        assert_eq!(
            item.get("provider_metadata").unwrap(),
            &json!({ "kept": true })
        );
        assert!(!item.contains_key("status"));
    }

    #[test]
    fn decodes_codex_namespace_tools_and_ignores_disabled_web_search() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": "run a safe command",
            "tools": [
                {
                    "type": "function",
                    "name": "exec_command",
                    "description": "Execute a command.",
                    "strict": false,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "cmd": { "type": "string" }
                        },
                        "required": ["cmd"],
                        "additionalProperties": false
                    }
                },
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "description": "Tools for spawning and managing sub-agents.",
                    "tools": [{
                        "type": "function",
                        "name": "spawn_agent",
                        "description": "Spawn a sub-agent.",
                        "strict": false,
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string" }
                            },
                            "required": ["message"],
                            "additionalProperties": false
                        }
                    }]
                },
                {
                    "type": "web_search",
                    "external_web_access": false
                },
                {
                    "type": "tool_search",
                    "description": "Search deferred tool metadata.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" },
                            "limit": { "type": "number" }
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }
                }
            ]
        });

        let request = responses_request_to_ir(&body).unwrap();

        assert_eq!(request.tools.len(), 3);
        assert_eq!(request.tools[0].name, "exec_command");
        assert_eq!(
            request.tools[0].input_schema,
            json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string" }
                },
                "required": ["cmd"],
                "additionalProperties": false
            })
        );
        assert_eq!(request.tools[1].name, "spawn_agent");
        assert!(
            request.tools[1]
                .description
                .as_deref()
                .unwrap()
                .contains("Namespace `multi_agent_v1`")
        );
        assert_eq!(
            request.tools[1].input_schema,
            json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"],
                "additionalProperties": false
            })
        );
        assert_eq!(request.tools[2].name, "tool_search");
        assert_eq!(
            request.tools[2].input_schema,
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "number" }
                },
                "required": ["query"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn decodes_codex_custom_tools_as_string_input_tools() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": "apply a patch if needed",
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Use the apply_patch tool to edit files.",
                "format": {
                    "type": "grammar",
                    "syntax": "lark",
                    "definition": "start: /.+/"
                }
            }],
            "tool_choice": {
                "type": "custom",
                "name": "apply_patch"
            }
        });

        let request = responses_request_to_ir(&body).unwrap();

        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "apply_patch");
        assert!(
            request.tools[0]
                .description
                .as_deref()
                .unwrap()
                .contains("Original Responses custom tool")
        );
        assert_eq!(
            request.tools[0].input_schema,
            json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Free-form input for the original Responses custom tool."
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            request.tool_choice,
            ToolChoice::Tool("apply_patch".to_owned())
        );
    }

    #[test]
    fn rejects_enabled_web_search_tool_for_chat_backend() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": "search the web",
            "tools": [{
                "type": "web_search",
                "external_web_access": true
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::UnsupportedFeature { feature, protocol }
                if feature == "web_search tool with external_web_access=true"
                    && protocol == "responses")
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
    fn validates_responses_call_ids_across_multi_turn_agent_loop() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [
                { "type": "message", "role": "user", "content": "check weather and time" },
                {
                    "type": "function_call",
                    "call_id": "call_weather",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "sunny"
                },
                { "type": "message", "role": "assistant", "content": "Weather is sunny." },
                {
                    "type": "function_call",
                    "call_id": "call_time",
                    "name": "lookup_time",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_time",
                    "output": "10:30"
                }
            ]
        });

        let request = responses_request_to_ir(&body).unwrap();
        let ids = responses_tool_id_map_from_request(&request).unwrap();

        assert_eq!(
            ids.responses_call_id("call_weather").unwrap(),
            "call_weather"
        );
        assert_eq!(ids.responses_call_id("call_time").unwrap(), "call_time");
        assert_eq!(
            ids.chat_tool_call_id_for_responses("call_weather").unwrap(),
            "call_weather"
        );
        assert_eq!(
            ids.chat_tool_call_id_for_responses("call_time").unwrap(),
            "call_time"
        );
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
    fn rejects_function_call_output_without_prior_function_call() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "function_call_output",
                "call_id": "missing_call",
                "output": "orphaned"
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("missing Chat tool_call_id for Responses call_id `missing_call`"))
        );
    }

    #[test]
    fn rejects_duplicate_function_call_outputs() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_weather",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "sunny"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "still sunny"
                }
            ]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("duplicates the result"))
        );
    }

    #[test]
    fn rejects_unanswered_function_call_items() {
        let body = json!({
            "model": "gpt-5-codex",
            "input": [{
                "type": "function_call",
                "call_id": "call_weather",
                "name": "lookup_weather",
                "arguments": "{\"city\":\"Paris\"}"
            }]
        });

        let error = responses_request_to_ir(&body).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("assistant tool call `call_weather` has no matching tool result"))
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
