//! Encoding for OpenAI Responses API responses.

// Later M3 tasks wire this staged encoder into HTTP routing and streaming.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Number, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, ImageSource, Provider, Role, Thinking},
        request::{IrRequest, IrResponse, StopReason, ToolChoice, ToolDef, Usage},
    },
    protocol::tool_ids::{ToolIdMap, responses_tool_id_map_from_request},
    reasoning::envelope::{SourceBlock, wrap_as_responses_reasoning_item},
};

use super::reasoning::preserved_reasoning_item_from_thinking;

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
// IR extras may originate from a different frontend protocol; only pass fields
// that are valid Responses request options across this boundary.
const RESPONSES_EXTRA_FIELDS: &[&str] = &[
    "background",
    "include",
    "max_tool_calls",
    "metadata",
    "parallel_tool_calls",
    "previous_response_id",
    "prompt",
    "reasoning",
    "service_tier",
    "store",
    "stream_options",
    "text",
    "truncation",
    "user",
];

/// Converts a provider-neutral request into an OpenAI Responses request body.
pub fn ir_request_to_responses(request: &IrRequest) -> Result<Value> {
    let tool_ids = responses_tool_id_map_from_request(request)?;
    let mut body = Map::new();

    body.insert("model".to_owned(), Value::String(request.model.clone()));
    body.insert(
        "input".to_owned(),
        Value::Array(encode_request_input(request, &tool_ids)?),
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

    insert_optional_u32(&mut body, "max_output_tokens", request.max_tokens);
    insert_optional_f32(&mut body, "temperature", request.temperature)?;
    insert_optional_f32(&mut body, "top_p", request.top_p)?;
    insert_optional_u32(&mut body, "top_k", request.top_k);

    if !request.stop.is_empty() {
        body.insert("stop".to_owned(), encode_request_stop(&request.stop));
    }

    body.insert("stream".to_owned(), Value::Bool(request.stream));
    insert_request_extra(&mut body, request)?;

    Ok(Value::Object(body))
}

fn encode_request_input(request: &IrRequest, tool_ids: &ToolIdMap) -> Result<Vec<Value>> {
    let mut input = Vec::new();

    if let Some(system) = &request.system
        && !system.is_empty()
    {
        input.push(encode_request_message_item(
            "system",
            encode_message_content(system, "system", false, "request.system")?,
        ));
    }

    for (message_index, message) in request.messages.iter().enumerate() {
        encode_request_message(message, message_index, tool_ids, &mut input)?;
    }

    if input.is_empty() {
        return Err(mapping_error("Responses request input must not be empty"));
    }

    Ok(input)
}

fn encode_request_message(
    message: &crate::ir::message::Message,
    message_index: usize,
    tool_ids: &ToolIdMap,
    input: &mut Vec<Value>,
) -> Result<()> {
    match message.role {
        Role::System => encode_user_like_message(
            "system",
            &message.content,
            message_index,
            false,
            tool_ids,
            input,
        ),
        Role::User => encode_user_like_message(
            "user",
            &message.content,
            message_index,
            true,
            tool_ids,
            input,
        ),
        Role::Assistant => encode_assistant_input_message(message, message_index, tool_ids, input),
        Role::Tool => encode_tool_input_message(message, message_index, tool_ids, input),
    }
}

fn encode_user_like_message(
    role: &'static str,
    content: &[ContentBlock],
    message_index: usize,
    allow_images: bool,
    tool_ids: &ToolIdMap,
    input: &mut Vec<Value>,
) -> Result<()> {
    let mut pending_content = Vec::new();

    for (block_index, block) in content.iter().enumerate() {
        let path = format!("messages[{message_index}].content[{block_index}]");
        match block {
            ContentBlock::Text { .. } | ContentBlock::Image(_) => {
                pending_content.push(encode_message_content_part(
                    block,
                    role,
                    allow_images,
                    &path,
                )?);
            }
            ContentBlock::ToolResult { .. } if role == "user" => {
                flush_request_message_item(role, &mut pending_content, input);
                input.push(encode_function_call_output_item(block, tool_ids, path)?);
            }
            ContentBlock::ToolResult { .. } => {
                return Err(mapping_error(format!(
                    "{path} is a tool result but message role is {role}"
                )));
            }
            ContentBlock::ToolUse { .. } => {
                return Err(mapping_error(format!(
                    "{path} is a tool call but message role is {role}"
                )));
            }
            ContentBlock::Thinking(_) => {
                return Err(mapping_error(format!(
                    "{path} is a thinking block but message role is {role}"
                )));
            }
        }
    }

    flush_request_message_item(role, &mut pending_content, input);
    Ok(())
}

fn encode_assistant_input_message(
    message: &crate::ir::message::Message,
    message_index: usize,
    tool_ids: &ToolIdMap,
    input: &mut Vec<Value>,
) -> Result<()> {
    let mut pending_content = Vec::new();

    for (block_index, block) in message.content.iter().enumerate() {
        let path = format!("messages[{message_index}].content[{block_index}]");
        match block {
            ContentBlock::Text { .. } => {
                pending_content.push(encode_message_content_part(
                    block,
                    "assistant",
                    false,
                    &path,
                )?);
            }
            ContentBlock::Thinking(thinking) => {
                flush_request_message_item("assistant", &mut pending_content, input);
                input.push(encode_reasoning_input_item(thinking, path)?);
            }
            ContentBlock::ToolUse { .. } => {
                flush_request_message_item("assistant", &mut pending_content, input);
                input.push(encode_function_call_item(block, tool_ids, path)?);
            }
            ContentBlock::Image(_) => {
                return Err(mapping_error(format!(
                    "{path} is an image block but message role is assistant"
                )));
            }
            ContentBlock::ToolResult { .. } => {
                return Err(mapping_error(format!(
                    "{path} is a tool result but message role is assistant"
                )));
            }
        }
    }

    flush_request_message_item("assistant", &mut pending_content, input);
    Ok(())
}

fn encode_tool_input_message(
    message: &crate::ir::message::Message,
    message_index: usize,
    tool_ids: &ToolIdMap,
    input: &mut Vec<Value>,
) -> Result<()> {
    for (block_index, block) in message.content.iter().enumerate() {
        let path = format!("messages[{message_index}].content[{block_index}]");
        match block {
            ContentBlock::ToolResult { .. } => {
                input.push(encode_function_call_output_item(block, tool_ids, path)?);
            }
            _ => {
                return Err(mapping_error(format!(
                    "{path} is not a tool result but message role is tool"
                )));
            }
        }
    }

    Ok(())
}

fn encode_message_content(
    content: &[ContentBlock],
    role: &'static str,
    allow_images: bool,
    path: impl Into<String>,
) -> Result<Vec<Value>> {
    let path = path.into();
    if content.is_empty() {
        return Err(mapping_error(format!("{path} must not be empty")));
    }

    content
        .iter()
        .enumerate()
        .map(|(index, block)| {
            encode_message_content_part(block, role, allow_images, &format!("{path}[{index}]"))
        })
        .collect()
}

fn encode_message_content_part(
    block: &ContentBlock,
    role: &'static str,
    allow_images: bool,
    path: &str,
) -> Result<Value> {
    match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": if role == "assistant" { "output_text" } else { "input_text" },
            "text": text,
        })),
        ContentBlock::Image(source) if allow_images => Ok(encode_input_image_part(source)),
        ContentBlock::Image(_) => Err(mapping_error(format!(
            "{path} is an image block, which is not allowed in a {role} Responses message"
        ))),
        ContentBlock::Thinking(_) => Err(mapping_error(format!(
            "{path} is a thinking block, which must be encoded as a Responses reasoning item"
        ))),
        ContentBlock::ToolUse { .. } => Err(mapping_error(format!(
            "{path} is a tool call, which must be encoded as a Responses function_call item"
        ))),
        ContentBlock::ToolResult { .. } => Err(mapping_error(format!(
            "{path} is a tool result, which must be encoded as a Responses function_call_output item"
        ))),
    }
}

fn encode_input_image_part(source: &ImageSource) -> Value {
    let image_url = match source {
        ImageSource::Url(url) => url.clone(),
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
    };

    json!({
        "type": "input_image",
        "image_url": image_url,
    })
}

fn flush_request_message_item(
    role: &'static str,
    pending_content: &mut Vec<Value>,
    input: &mut Vec<Value>,
) {
    if pending_content.is_empty() {
        return;
    }

    input.push(encode_request_message_item(
        role,
        std::mem::take(pending_content),
    ));
}

fn encode_request_message_item(role: &'static str, content: Vec<Value>) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn encode_reasoning_input_item(thinking: &Thinking, path: String) -> Result<Value> {
    if thinking.source != Provider::Responses {
        return Err(mapping_error(format!(
            "{path} must be Responses-origin thinking to encode as a Responses reasoning input item"
        )));
    }

    if let Some(item) = preserved_reasoning_item_from_thinking(thinking, path.as_str())? {
        return Ok(Value::Object(item));
    }

    let encrypted_content = reasoning_encrypted_content_from_opaque(thinking, &path)?;
    Ok(json!({
        "type": "reasoning",
        "summary": encode_reasoning_summary(thinking),
        "encrypted_content": encrypted_content,
    }))
}

fn encode_function_call_item(
    block: &ContentBlock,
    tool_ids: &ToolIdMap,
    path: String,
) -> Result<Value> {
    let ContentBlock::ToolUse { id, name, input } = block else {
        return Err(mapping_error(format!("{path} must be a tool call")));
    };
    let call_id = tool_ids.responses_call_id(id)?;

    Ok(json!({
        "type": "function_call",
        "status": "completed",
        "call_id": call_id,
        "name": name,
        "arguments": serde_json::to_string(input)?,
    }))
}

fn encode_function_call_output_item(
    block: &ContentBlock,
    tool_ids: &ToolIdMap,
    path: String,
) -> Result<Value> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error: _,
    } = block
    else {
        return Err(mapping_error(format!("{path} must be a tool result")));
    };
    let call_id = tool_ids.responses_call_id(tool_use_id)?;

    Ok(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": encode_tool_output(content)?,
    }))
}

fn encode_request_tools(tools: &[ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut function = Map::new();
                function.insert("type".to_owned(), json!("function"));
                function.insert("name".to_owned(), Value::String(tool.name.clone()));
                if let Some(description) = &tool.description {
                    function.insert("description".to_owned(), Value::String(description.clone()));
                }
                function.insert("parameters".to_owned(), tool.input_schema.clone());
                Value::Object(function)
            })
            .collect(),
    )
}

fn encode_request_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({
            "type": "function",
            "name": name,
        }),
    }
}

fn encode_request_stop(stop: &[String]) -> Value {
    Value::Array(stop.iter().cloned().map(Value::String).collect())
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

fn insert_request_extra(body: &mut Map<String, Value>, request: &IrRequest) -> Result<()> {
    for (key, value) in &request.extra {
        if CORE_REQUEST_FIELDS.contains(&key.as_str())
            || !RESPONSES_EXTRA_FIELDS.contains(&key.as_str())
        {
            continue;
        }
        body.insert(key.clone(), value.clone());
    }

    if !body.contains_key("reasoning") {
        insert_reasoning_from_output_config(body, request.extra.get("output_config"))?;
    }

    Ok(())
}

fn insert_reasoning_from_output_config(
    body: &mut Map<String, Value>,
    output_config: Option<&Value>,
) -> Result<()> {
    let Some(output_config) = output_config else {
        return Ok(());
    };
    let Some(output_config) = output_config.as_object() else {
        return Err(mapping_error(
            "request.extra.output_config must be an object when present",
        ));
    };
    let Some(effort) = output_config.get("effort") else {
        return Ok(());
    };
    let effort = effort.as_str().ok_or_else(|| {
        mapping_error("request.extra.output_config.effort must be a string when present")
    })?;

    body.insert("reasoning".to_owned(), json!({ "effort": effort }));
    Ok(())
}

/// Converts a provider-neutral non-streaming response into a Responses object.
pub fn ir_response_to_responses(resp: &IrResponse) -> Result<Value> {
    let status = encode_status(&resp.stop_reason);
    let output = encode_output(&resp.id, &resp.content, status)?;

    Ok(json!({
        "id": resp.id,
        "object": "response",
        "created_at": unix_timestamp(),
        "status": status,
        "error": null,
        "incomplete_details": encode_incomplete_details(&resp.stop_reason),
        "model": resp.model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "store": false,
        "usage": encode_usage(&resp.usage),
    }))
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must not be before the Unix epoch")
        .as_secs()
}

fn encode_output(response_id: &str, content: &[ContentBlock], status: &str) -> Result<Value> {
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
                )?);
            }
            ContentBlock::ToolUse { id, name, input } => {
                flush_message_item(response_id, status, &mut pending_text, &mut output);
                let arguments = serde_json::to_string(input)?;
                output.push(json!({
                    "id": item_id("fc", response_id, output.len()),
                    "type": "function_call",
                    "status": status,
                    "call_id": id,
                    "name": name,
                    "arguments": arguments,
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: _,
            } => {
                flush_message_item(response_id, status, &mut pending_text, &mut output);
                let tool_output = encode_tool_output(content)?;
                output.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": tool_output,
                }));
            }
            ContentBlock::Image(_) => {
                return Err(mapping_error(
                    "Responses assistant output does not support image content blocks",
                ));
            }
        }
    }

    flush_message_item(response_id, status, &mut pending_text, &mut output);
    Ok(Value::Array(output))
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
) -> Result<Value> {
    if let Some(item) =
        preserved_reasoning_item_from_thinking(thinking, format!("output[{output_index}]"))?
    {
        return Ok(Value::Object(item));
    }

    if thinking.source == Provider::Anthropic {
        return encode_anthropic_reasoning_item(output_index, thinking, status);
    }

    let mut item = Map::new();
    item.insert(
        "id".to_owned(),
        json!(item_id("rs", response_id, output_index)),
    );
    item.insert("type".to_owned(), json!("reasoning"));
    item.insert("status".to_owned(), json!(status));
    item.insert("summary".to_owned(), encode_reasoning_summary(thinking));

    if thinking.opaque.is_some() {
        item.insert(
            "encrypted_content".to_owned(),
            json!(reasoning_encrypted_content_from_opaque(
                thinking,
                format!("output[{output_index}]")
            )?),
        );
    }

    Ok(Value::Object(item))
}

fn encode_anthropic_reasoning_item(
    output_index: usize,
    thinking: &Thinking,
    status: &str,
) -> Result<Value> {
    let path = format!("output[{output_index}]");
    let source_block = anthropic_source_block_from_thinking(thinking, &path)?;
    let mut item = wrap_as_responses_reasoning_item(&source_block)?
        .as_object()
        .cloned()
        .ok_or_else(|| mapping_error("Responses reasoning envelope item must be an object"))?;

    item.insert("status".to_owned(), json!(status));
    item.insert("summary".to_owned(), encode_reasoning_summary(thinking));

    Ok(Value::Object(item))
}

fn anthropic_source_block_from_thinking(thinking: &Thinking, path: &str) -> Result<SourceBlock> {
    let text = thinking
        .text
        .as_deref()
        .ok_or_else(|| mapping_error(format!("{path}.thinking text is required")))?;
    let signature = thinking
        .opaque
        .as_deref()
        .ok_or_else(|| mapping_error(format!("{path}.opaque Anthropic signature is required")))?;
    let signature = std::str::from_utf8(signature).map_err(|err| {
        mapping_error(format!(
            "{path}.opaque Anthropic signature must be valid UTF-8: {err}"
        ))
    })?;

    SourceBlock::from_json(
        Provider::Anthropic,
        &json!({
            "type": "thinking",
            "thinking": text,
            "signature": signature,
        }),
    )
}

fn reasoning_encrypted_content_from_opaque(
    thinking: &Thinking,
    path: impl Into<String>,
) -> Result<&str> {
    let path = path.into();
    let opaque = thinking
        .opaque
        .as_deref()
        .ok_or_else(|| mapping_error(format!("{path}.opaque encrypted_content is required")))?;
    std::str::from_utf8(opaque).map_err(|err| {
        mapping_error(format!(
            "{path}.opaque encrypted_content must be valid UTF-8: {err}"
        ))
    })
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

fn encode_tool_output(content: &[ContentBlock]) -> Result<Value> {
    if content.len() == 1
        && let ContentBlock::Text { text } = &content[0]
    {
        return Ok(Value::String(text.clone()));
    }

    Ok(Value::Array(
        content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => Ok(json!({
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                })),
                ContentBlock::Thinking(_) => Err(mapping_error(
                    "Responses function_call_output cannot contain reasoning blocks",
                )),
                ContentBlock::ToolUse { .. } => Err(mapping_error(
                    "Responses function_call_output cannot contain function_call blocks",
                )),
                ContentBlock::ToolResult { .. } => Err(mapping_error(
                    "Responses function_call_output cannot contain nested tool results",
                )),
                ContentBlock::Image(_) => Err(mapping_error(
                    "Responses function_call_output cannot contain image blocks",
                )),
            })
            .collect::<Result<Vec<_>>>()?,
    ))
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

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::ir::message::{EchoPolicy, Message, Provider, Role};

    #[test]
    fn encodes_request_with_restored_responses_reasoning_and_tool_results() {
        let request = IrRequest {
            model: "gpt-5-codex".to_owned(),
            system: Some(vec![ContentBlock::Text {
                text: "Use repository context.".to_owned(),
            }]),
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "look up weather".to_owned(),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking(Thinking {
                            text: Some("Need the weather tool.".to_owned()),
                            opaque: Some(b"enc_payload_from_signature".to_vec()),
                            source: Provider::Responses,
                            echo_policy: EchoPolicy::Always,
                        }),
                        ContentBlock::ToolUse {
                            id: "call_weather".to_owned(),
                            name: "lookup_weather".to_owned(),
                            input: json!({ "city": "Paris" }),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "call_weather".to_owned(),
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
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::from_iter([("metadata".to_owned(), json!({ "session": "s_1" }))]),
        };

        let encoded = ir_request_to_responses(&request).unwrap();

        assert_eq!(encoded["model"], "gpt-5-codex");
        assert_eq!(encoded["max_output_tokens"], 256);
        assert_eq!(encoded["stream"], false);
        assert_eq!(encoded["metadata"], json!({ "session": "s_1" }));
        assert_eq!(
            encoded["tools"],
            json!([{
                "type": "function",
                "name": "lookup_weather",
                "description": "Fetch weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }])
        );
        assert_eq!(
            encoded["tool_choice"],
            json!({ "type": "function", "name": "lookup_weather" })
        );
        assert_eq!(
            encoded["input"],
            json!([
                {
                    "type": "message",
                    "role": "system",
                    "content": [{
                        "type": "input_text",
                        "text": "Use repository context."
                    }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "look up weather"
                    }]
                },
                {
                    "type": "reasoning",
                    "summary": [{
                        "type": "summary_text",
                        "text": "Need the weather tool."
                    }],
                    "encrypted_content": "enc_payload_from_signature"
                },
                {
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_weather",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "sunny"
                }
            ])
        );
    }

    #[test]
    fn request_extra_forwards_only_responses_native_fields() {
        let request = IrRequest {
            model: "gpt-5.5".to_owned(),
            system: None,
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_owned(),
                }],
            }],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(64),
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::from_iter([
                ("metadata".to_owned(), json!({ "trace_id": "m5-live" })),
                ("store".to_owned(), json!(true)),
                ("output_config".to_owned(), json!({ "effort": "high" })),
                ("container".to_owned(), json!({ "id": "anthropic-only" })),
                ("mcp_servers".to_owned(), json!([])),
            ]),
        };

        let encoded = ir_request_to_responses(&request).unwrap();

        assert_eq!(encoded["metadata"], json!({ "trace_id": "m5-live" }));
        assert_eq!(encoded["store"], json!(true));
        assert_eq!(encoded["reasoning"], json!({ "effort": "high" }));
        assert!(encoded.get("output_config").is_none());
        assert!(encoded.get("container").is_none());
        assert!(encoded.get("mcp_servers").is_none());
    }

    #[test]
    fn encodes_request_reasoning_from_preserved_item_without_null_status() {
        let raw_item = json!({
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "Need a tool." }],
            "encrypted_content": "enc_preserved",
            "status": null,
            "provider_metadata": { "kept": true }
        });
        let request = IrRequest {
            model: "gpt-5-codex".to_owned(),
            system: None,
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking(Thinking {
                    text: Some("ignored when preserved item exists".to_owned()),
                    opaque: Some(serde_json::to_vec(&raw_item).unwrap()),
                    source: Provider::Responses,
                    echo_policy: EchoPolicy::Always,
                })],
            }],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::new(),
        };

        let encoded = ir_request_to_responses(&request).unwrap();
        let item = encoded["input"][0].as_object().unwrap();

        assert_eq!(item.get("type").unwrap(), "reasoning");
        assert_eq!(item.get("encrypted_content").unwrap(), "enc_preserved");
        assert_eq!(
            item.get("provider_metadata").unwrap(),
            &json!({ "kept": true })
        );
        assert!(!item.contains_key("status"));
    }

    #[test]
    fn rejects_non_responses_thinking_in_request_input() {
        let request = IrRequest {
            model: "gpt-5-codex".to_owned(),
            system: None,
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking(Thinking {
                    text: Some("Anthropic thinking cannot go to Responses raw.".to_owned()),
                    opaque: Some(b"anthropic_signature".to_vec()),
                    source: Provider::Anthropic,
                    echo_policy: EchoPolicy::Always,
                })],
            }],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::new(),
        };

        let error = ir_request_to_responses(&request).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("Responses-origin thinking"))
        );
    }

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

        let mut encoded = ir_response_to_responses(&response).unwrap();
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

        let encoded = ir_response_to_responses(&response).unwrap();

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
    fn preserves_responses_reasoning_status_when_raw_item_is_available() {
        let raw_item = json!({
            "id": "rs_completed",
            "type": "reasoning",
            "status": "completed",
            "summary": [],
            "encrypted_content": "enc_preserved"
        });
        let response = IrResponse {
            id: "resp_incomplete".to_owned(),
            model: "gpt-5-codex".to_owned(),
            content: vec![ContentBlock::Thinking(Thinking {
                text: None,
                opaque: Some(serde_json::to_vec(&raw_item).unwrap()),
                source: Provider::Responses,
                echo_policy: EchoPolicy::Always,
            })],
            stop_reason: StopReason::MaxTokens,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read: None,
                cache_write: None,
            },
        };

        let encoded = ir_response_to_responses(&response).unwrap();

        assert_eq!(encoded["status"], "incomplete");
        assert_eq!(encoded["output"][0]["id"], "rs_completed");
        assert_eq!(encoded["output"][0]["status"], "completed");
        assert_eq!(encoded["output"][0]["encrypted_content"], "enc_preserved");
    }

    #[test]
    fn wraps_anthropic_thinking_as_responses_reasoning_envelope() {
        let response = IrResponse {
            id: "resp_anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![ContentBlock::Thinking(Thinking {
                text: Some("Need the weather tool.".to_owned()),
                opaque: Some(b"sig_real_anthropic_123".to_vec()),
                source: Provider::Anthropic,
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

        let encoded = ir_response_to_responses(&response).unwrap();
        let item = &encoded["output"][0];

        assert_eq!(item["type"], "reasoning");
        assert_eq!(item["status"], "completed");
        assert_eq!(
            item["summary"],
            json!([{ "type": "summary_text", "text": "Need the weather tool." }])
        );
        assert!(
            item["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("rs_llm_proxy"))
        );
        assert_ne!(item["encrypted_content"], "sig_real_anthropic_123");

        let source_block =
            crate::reasoning::envelope::unwrap_from_responses_reasoning_item(item).unwrap();
        assert_eq!(source_block.source, Provider::Anthropic);
        assert_eq!(
            source_block.payload_json().unwrap(),
            json!({
                "type": "thinking",
                "thinking": "Need the weather tool.",
                "signature": "sig_real_anthropic_123"
            })
        );
    }

    #[test]
    fn rejects_anthropic_thinking_without_signature_for_responses_envelope() {
        let response = IrResponse {
            id: "resp_missing_signature".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            content: vec![ContentBlock::Thinking(Thinking {
                text: Some("Need the weather tool.".to_owned()),
                opaque: None,
                source: Provider::Anthropic,
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

        let error = ir_response_to_responses(&response).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("Anthropic signature is required"))
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

            let encoded = ir_response_to_responses(&response).unwrap();

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
            ir_response_to_responses(&response).unwrap()["output"],
            json!([{
                "type": "function_call_output",
                "call_id": "call_weather",
                "output": "sunny"
            }])
        );
    }
}
