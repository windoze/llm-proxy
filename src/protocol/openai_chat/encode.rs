//! Encoding for OpenAI Chat Completions-compatible requests.

// M2-08 wires this staged encoder into the Anthropic endpoint route.
#![allow(dead_code)]

use serde_json::{Map, Number, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, EchoPolicy, ImageSource, Message, Role, Thinking},
        request::{IrRequest, ToolChoice, ToolDef},
    },
    protocol::{
        capability::{
            IrTargetProtocol, chat_response_format_from_extra, passthrough_extra_fields,
            reasoning_effort_from_extra,
        },
        tool_ids::{ToolIdMap, tool_id_map_from_request},
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

/// Converts a provider-neutral IR request into an OpenAI Chat-compatible request.
pub fn ir_request_to_chat(request: &IrRequest, profile: &dyn CapabilityProfile) -> Result<Value> {
    let model = profile.map_model_name(&request.model);
    let blocklist = profile.param_blocklist(&model);
    validate_choice_count(request.extra.get("n"), profile, blocklist)?;

    let tool_ids = tool_id_map_from_request(request)?;
    let mut body = Map::new();

    body.insert("model".to_owned(), Value::String(model));
    body.insert(
        "messages".to_owned(),
        Value::Array(encode_messages(request, &tool_ids)?),
    );

    if !request.tools.is_empty() {
        body.insert("tools".to_owned(), encode_tools(&request.tools));
    }

    if !request.tools.is_empty() || request.tool_choice != ToolChoice::Auto {
        body.insert(
            "tool_choice".to_owned(),
            encode_tool_choice(&request.tool_choice),
        );
    }

    insert_optional_u32(&mut body, blocklist, "max_tokens", request.max_tokens);
    insert_optional_f32(&mut body, blocklist, "temperature", request.temperature)?;
    insert_optional_f32(&mut body, blocklist, "top_p", request.top_p)?;
    insert_optional_u32(&mut body, blocklist, "top_k", request.top_k);

    if !request.stop.is_empty() && !is_blocklisted(blocklist, "stop") {
        body.insert("stop".to_owned(), Value::Array(encode_stop(&request.stop)));
    }

    if !is_blocklisted(blocklist, "stream") {
        body.insert("stream".to_owned(), Value::Bool(request.stream));
    }

    insert_extra(&mut body, request, profile, blocklist)?;

    Ok(Value::Object(body))
}

fn encode_messages(request: &IrRequest, tool_ids: &ToolIdMap) -> Result<Vec<Value>> {
    let mut messages = Vec::new();

    if let Some(system) = &request.system
        && !system.is_empty()
    {
        messages.push(json!({
            "role": "system",
            "content": encode_chat_content(system, false, "request.system")?,
        }));
    }

    for (index, message) in normalize_messages(&request.messages).iter().enumerate() {
        encode_message(message, index, tool_ids, &mut messages)?;
    }

    Ok(messages)
}

fn normalize_messages(messages: &[Message]) -> Vec<Message> {
    let mut normalized: Vec<Message> = Vec::new();

    for message in messages {
        if let Some(previous) = normalized.last_mut()
            && previous.role == message.role
        {
            previous.content.extend(message.content.clone());
            continue;
        }

        normalized.push(message.clone());
    }

    normalized
}

fn encode_message(
    message: &Message,
    index: usize,
    tool_ids: &ToolIdMap,
    output: &mut Vec<Value>,
) -> Result<()> {
    match message.role {
        Role::System => output.push(json!({
            "role": "system",
            "content": encode_chat_content(
                &message.content,
                false,
                format!("messages[{index}].content"),
            )?,
        })),
        Role::User => encode_user_message(message, index, tool_ids, output)?,
        Role::Assistant => {
            output.push(encode_assistant_message(message, index, tool_ids)?);
        }
        Role::Tool => encode_tool_role_message(message, index, tool_ids, output)?,
    }

    Ok(())
}

fn encode_user_message(
    message: &Message,
    index: usize,
    tool_ids: &ToolIdMap,
    output: &mut Vec<Value>,
) -> Result<()> {
    let mut pending_user_content = Vec::new();

    for (block_index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text { .. } | ContentBlock::Image(_) => {
                pending_user_content.push(block.clone());
            }
            ContentBlock::ToolResult { .. } => {
                flush_user_content(&mut pending_user_content, index, output)?;
                output.push(encode_tool_result_message(
                    block,
                    tool_ids,
                    format!("messages[{index}].content[{block_index}]"),
                )?);
            }
            ContentBlock::ToolUse { .. } => {
                return Err(mapping_error(format!(
                    "messages[{index}].content[{block_index}] is a tool call but message role is user"
                )));
            }
            ContentBlock::Thinking(_) => {
                return Err(mapping_error(format!(
                    "messages[{index}].content[{block_index}] is a thinking block but message role is user"
                )));
            }
        }
    }

    flush_user_content(&mut pending_user_content, index, output)
}

fn flush_user_content(
    pending_user_content: &mut Vec<ContentBlock>,
    message_index: usize,
    output: &mut Vec<Value>,
) -> Result<()> {
    if pending_user_content.is_empty() {
        return Ok(());
    }

    output.push(json!({
        "role": "user",
        "content": encode_chat_content(
            pending_user_content,
            true,
            format!("messages[{message_index}].content"),
        )?,
    }));
    pending_user_content.clear();
    Ok(())
}

fn encode_tool_role_message(
    message: &Message,
    index: usize,
    tool_ids: &ToolIdMap,
    output: &mut Vec<Value>,
) -> Result<()> {
    for (block_index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::ToolResult { .. } => output.push(encode_tool_result_message(
                block,
                tool_ids,
                format!("messages[{index}].content[{block_index}]"),
            )?),
            _ => {
                return Err(mapping_error(format!(
                    "messages[{index}].content[{block_index}] is not a tool result but message role is tool"
                )));
            }
        }
    }

    Ok(())
}

fn encode_assistant_message(
    message: &Message,
    index: usize,
    tool_ids: &ToolIdMap,
) -> Result<Value> {
    let has_tool_calls = message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
    let mut text = String::new();
    let mut saw_text = false;
    let mut reasoning = String::new();
    let mut saw_reasoning = false;
    let mut tool_calls = Vec::new();

    for (block_index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text: block_text } => {
                saw_text = true;
                text.push_str(block_text);
            }
            ContentBlock::Thinking(thinking) => {
                if should_echo_reasoning(thinking, has_tool_calls) {
                    let thinking_text = thinking.text.as_deref().ok_or_else(|| {
                        mapping_error(format!(
                            "messages[{index}].content[{block_index}] cannot be encoded as Chat reasoning_content without text"
                        ))
                    })?;
                    saw_reasoning = true;
                    reasoning.push_str(thinking_text);
                }
            }
            ContentBlock::ToolUse { .. } => tool_calls.push(encode_tool_call(
                block,
                tool_ids,
                format!("messages[{index}].content[{block_index}]"),
            )?),
            ContentBlock::Image(_) => {
                return Err(mapping_error(format!(
                    "messages[{index}].content[{block_index}] is an image block but message role is assistant"
                )));
            }
            ContentBlock::ToolResult { .. } => {
                return Err(mapping_error(format!(
                    "messages[{index}].content[{block_index}] is a tool result but message role is assistant"
                )));
            }
        }
    }

    let mut encoded = Map::new();
    encoded.insert("role".to_owned(), json!("assistant"));

    if saw_text {
        encoded.insert("content".to_owned(), Value::String(text));
    } else if !tool_calls.is_empty() || saw_reasoning {
        encoded.insert("content".to_owned(), Value::Null);
    } else {
        encoded.insert("content".to_owned(), Value::String(String::new()));
    }

    if saw_reasoning {
        encoded.insert("reasoning_content".to_owned(), Value::String(reasoning));
    }

    if !tool_calls.is_empty() {
        encoded.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }

    Ok(Value::Object(encoded))
}

fn should_echo_reasoning(thinking: &Thinking, has_tool_calls: bool) -> bool {
    match thinking.echo_policy {
        EchoPolicy::Always => true,
        EchoPolicy::OnlyWithToolCall => has_tool_calls,
        EchoPolicy::Never => false,
    }
}

fn encode_tool_call(block: &ContentBlock, tool_ids: &ToolIdMap, path: String) -> Result<Value> {
    let ContentBlock::ToolUse { id, name, input } = block else {
        return Err(mapping_error(format!("{path} must be a tool call")));
    };
    let chat_tool_call_id = tool_ids.chat_tool_call_id(id)?;

    Ok(json!({
        "id": chat_tool_call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": serde_json::to_string(input)?,
        },
    }))
}

fn encode_tool_result_message(
    block: &ContentBlock,
    tool_ids: &ToolIdMap,
    path: String,
) -> Result<Value> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        ..
    } = block
    else {
        return Err(mapping_error(format!("{path} must be a tool result")));
    };
    let chat_tool_call_id = tool_ids.chat_tool_call_id(tool_use_id)?;

    Ok(json!({
        "role": "tool",
        "tool_call_id": chat_tool_call_id,
        "content": encode_chat_content(content, true, format!("{path}.content"))?,
    }))
}

fn encode_chat_content(
    content: &[ContentBlock],
    allow_images: bool,
    path: impl Into<String>,
) -> Result<Value> {
    let path = path.into();
    if content.is_empty() {
        return Err(mapping_error(format!("{path} must not be empty")));
    }

    if content.len() == 1
        && let ContentBlock::Text { text } = &content[0]
    {
        return Ok(Value::String(text.clone()));
    }

    content
        .iter()
        .enumerate()
        .map(|(index, block)| encode_content_part(block, allow_images, format!("{path}[{index}]")))
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

fn encode_content_part(block: &ContentBlock, allow_images: bool, path: String) -> Result<Value> {
    match block {
        ContentBlock::Text { text } => Ok(json!({
            "type": "text",
            "text": text,
        })),
        ContentBlock::Image(source) if allow_images => encode_image_part(source),
        ContentBlock::Image(_) => Err(mapping_error(format!(
            "{path} is an image block, which is not allowed in this Chat message"
        ))),
        ContentBlock::ToolUse { .. } => Err(mapping_error(format!(
            "{path} is a tool call, which must be encoded on an assistant message"
        ))),
        ContentBlock::ToolResult { .. } => Err(mapping_error(format!(
            "{path} is a tool result, which must be encoded as a Chat tool message"
        ))),
        ContentBlock::Thinking(_) => Err(mapping_error(format!(
            "{path} is a thinking block, which must be encoded as assistant reasoning_content"
        ))),
    }
}

fn encode_image_part(source: &ImageSource) -> Result<Value> {
    let url = match source {
        ImageSource::Url(url) => url.clone(),
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
    };

    Ok(json!({
        "type": "image_url",
        "image_url": {
            "url": url,
        },
    }))
}

fn encode_tools(tools: &[ToolDef]) -> Value {
    Value::Array(tools.iter().map(encode_tool_def).collect())
}

fn encode_tool_def(tool: &ToolDef) -> Value {
    let mut function = Map::new();
    function.insert("name".to_owned(), Value::String(tool.name.clone()));
    if let Some(description) = &tool.description {
        function.insert("description".to_owned(), Value::String(description.clone()));
    }
    function.insert("parameters".to_owned(), tool.input_schema.clone());

    json!({
        "type": "function",
        "function": Value::Object(function),
    })
}

fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({
            "type": "function",
            "function": {
                "name": name,
            },
        }),
    }
}

fn encode_stop(stop: &[String]) -> Vec<Value> {
    stop.iter().cloned().map(Value::String).collect()
}

fn insert_optional_u32(
    body: &mut Map<String, Value>,
    blocklist: &[&str],
    field: &'static str,
    value: Option<u32>,
) {
    if let Some(value) = value
        && !is_blocklisted(blocklist, field)
    {
        body.insert(
            field.to_owned(),
            Value::Number(Number::from(u64::from(value))),
        );
    }
}

fn insert_optional_f32(
    body: &mut Map<String, Value>,
    blocklist: &[&str],
    field: &'static str,
    value: Option<f32>,
) -> Result<()> {
    if let Some(value) = value
        && !is_blocklisted(blocklist, field)
    {
        let number = Number::from_f64(f64::from(value)).ok_or_else(|| {
            mapping_error(format!("request.{field} must be a finite JSON number"))
        })?;
        body.insert(field.to_owned(), Value::Number(number));
    }

    Ok(())
}

fn insert_extra(
    body: &mut Map<String, Value>,
    request: &IrRequest,
    profile: &dyn CapabilityProfile,
    blocklist: &[&str],
) -> Result<()> {
    if !is_blocklisted(blocklist, "reasoning_effort")
        && let Some(reasoning_effort) = request.extra.get("reasoning_effort")
    {
        let reasoning_effort = reasoning_effort
            .as_str()
            .ok_or_else(|| mapping_error("request.extra.reasoning_effort must be a string"))?;
        body.insert(
            "reasoning_effort".to_owned(),
            Value::String(
                profile
                    .normalize_reasoning_effort(reasoning_effort)
                    .to_owned(),
            ),
        );
    }

    if !body.contains_key("reasoning_effort")
        && !is_blocklisted(blocklist, "reasoning_effort")
        && let Some(reasoning_effort) = reasoning_effort_from_extra(&request.extra)?
    {
        body.insert(
            "reasoning_effort".to_owned(),
            Value::String(
                profile
                    .normalize_reasoning_effort(reasoning_effort)
                    .to_owned(),
            ),
        );
    }

    if !is_blocklisted(blocklist, "n")
        && let Some(choice_count) = request.extra.get("n")
    {
        body.insert(
            "n".to_owned(),
            Value::Number(Number::from(u64::from(choice_count_as_u32(
                choice_count,
                "request.extra.n",
            )?))),
        );
    }

    if !body.contains_key("response_format")
        && !is_blocklisted(blocklist, "response_format")
        && let Some(response_format) = chat_response_format_from_extra(&request.extra)?
    {
        body.insert("response_format".to_owned(), response_format);
    }

    for (key, value) in passthrough_extra_fields(
        IrTargetProtocol::OpenAiChat,
        &request.extra,
        CORE_REQUEST_FIELDS,
        blocklist,
    )? {
        body.insert(key, value);
    }

    Ok(())
}

fn validate_choice_count(
    value: Option<&Value>,
    profile: &dyn CapabilityProfile,
    blocklist: &[&str],
) -> Result<()> {
    if is_blocklisted(blocklist, "n") {
        return Ok(());
    }

    if let Some(value) = value
        && choice_count_as_u32(value, "request.extra.n")? > 1
        && !profile.supports_multiple_choices()
    {
        return Err(ProxyError::UnsupportedFeature {
            feature: "n > 1".to_owned(),
            protocol: PROTOCOL.to_owned(),
        });
    }

    Ok(())
}

fn choice_count_as_u32(value: &Value, path: &str) -> Result<u32> {
    let value = value
        .as_u64()
        .ok_or_else(|| mapping_error(format!("{path} must be an unsigned integer")))?;
    u32::try_from(value).map_err(|_| mapping_error(format!("{path} is too large for u32")))
}

fn is_blocklisted(blocklist: &[&str], field: &str) -> bool {
    blocklist.contains(&field)
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::{
        ir::message::{Provider, Thinking},
        provider::{GenericOpenAi, deepseek::DeepSeek},
    };

    #[test]
    fn encodes_deepseek_request_with_strict_ordering_and_tool_results() {
        let request = request_with_messages(vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "look up the weather".to_owned(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "and the local time".to_owned(),
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking(Thinking {
                        text: Some("I need two tool calls.".to_owned()),
                        opaque: None,
                        source: Provider::DeepSeek,
                        echo_policy: EchoPolicy::OnlyWithToolCall,
                    }),
                    ContentBlock::Text {
                        text: "Checking now.".to_owned(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_weather".to_owned(),
                        name: "lookup_weather".to_owned(),
                        input: json!({ "city": "Paris" }),
                    },
                    ContentBlock::ToolUse {
                        id: "call_time".to_owned(),
                        name: "lookup_time".to_owned(),
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
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_time".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "10:30".to_owned(),
                    }],
                    is_error: false,
                }],
            },
        ]);

        let encoded = ir_request_to_chat(&request, &DeepSeek).unwrap();

        assert_eq!(
            encoded["messages"],
            json!([
                {
                    "role": "system",
                    "content": "be concise"
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "look up the weather" },
                        { "type": "text", "text": "and the local time" }
                    ]
                },
                {
                    "role": "assistant",
                    "content": "Checking now.",
                    "reasoning_content": "I need two tool calls.",
                    "tool_calls": [
                        {
                            "id": "call_weather",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        },
                        {
                            "id": "call_time",
                            "type": "function",
                            "function": {
                                "name": "lookup_time",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_weather",
                    "content": "sunny"
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_time",
                    "content": "10:30"
                }
            ])
        );
    }

    #[test]
    fn applies_profile_blocklist_and_normalizes_extra_parameters() {
        let mut request = request_with_messages(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
        }]);
        request.temperature = Some(0.7);
        request.top_p = Some(0.9);
        request.top_k = Some(40);
        request.extra = Map::from_iter([
            ("presence_penalty".to_owned(), json!(0.5)),
            ("frequency_penalty".to_owned(), json!(0.25)),
            ("logprobs".to_owned(), json!(true)),
            ("top_logprobs".to_owned(), json!(3)),
            ("reasoning_effort".to_owned(), json!("low")),
            ("metadata".to_owned(), json!({ "trace_id": "abc" })),
        ]);

        let encoded = ir_request_to_chat(&request, &DeepSeek).unwrap();

        assert_eq!(encoded["top_k"], 40);
        assert_eq!(encoded["reasoning_effort"], "high");
        assert_eq!(encoded["metadata"], json!({ "trace_id": "abc" }));
        for dropped in [
            "temperature",
            "top_p",
            "presence_penalty",
            "frequency_penalty",
            "logprobs",
            "top_logprobs",
        ] {
            assert!(
                encoded.get(dropped).is_none(),
                "{dropped} should be dropped"
            );
        }
    }

    #[test]
    fn emulates_responses_structured_output_and_reasoning_controls() {
        let mut request = request_with_messages(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "return JSON".to_owned(),
            }],
        }]);
        request.extra = Map::from_iter([
            (
                "text".to_owned(),
                json!({
                    "format": {
                        "type": "json_schema",
                        "name": "answer",
                        "schema": {
                            "type": "object",
                            "properties": {
                                "answer": { "type": "string" }
                            },
                            "required": ["answer"]
                        }
                    }
                }),
            ),
            ("output_config".to_owned(), json!({ "effort": "low" })),
        ]);

        let encoded = ir_request_to_chat(&request, &DeepSeek).unwrap();

        assert_eq!(encoded["reasoning_effort"], "high");
        assert_eq!(
            encoded["response_format"],
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "answer",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "answer": { "type": "string" }
                        },
                        "required": ["answer"]
                    }
                }
            })
        );
        assert!(encoded.get("text").is_none());
        assert!(encoded.get("output_config").is_none());
    }

    #[test]
    fn rejects_multiple_choices_for_deepseek_but_preserves_them_for_generic_openai() {
        let mut request = request_with_messages(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
        }]);
        request.extra.insert("n".to_owned(), json!(2));

        let deepseek_error = ir_request_to_chat(&request, &DeepSeek).unwrap_err();
        assert!(
            matches!(deepseek_error, ProxyError::UnsupportedFeature { feature, protocol } if feature == "n > 1" && protocol == "openai_chat")
        );

        let generic = ir_request_to_chat(&request, &GenericOpenAi::default()).unwrap();
        assert_eq!(generic["n"], 2);
    }

    #[test]
    fn drops_reasoning_without_tool_calls_when_policy_allows() {
        let request = request_with_messages(vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking(Thinking {
                text: Some("droppable".to_owned()),
                opaque: None,
                source: Provider::DeepSeek,
                echo_policy: EchoPolicy::OnlyWithToolCall,
            })],
        }]);

        let encoded = ir_request_to_chat(&request, &DeepSeek).unwrap();

        assert_eq!(
            encoded["messages"],
            json!([
                {
                    "role": "system",
                    "content": "be concise"
                },
                {
                    "role": "assistant",
                    "content": ""
                }
            ])
        );
    }

    fn request_with_messages(messages: Vec<Message>) -> IrRequest {
        IrRequest {
            model: "deepseek-reasoner".to_owned(),
            system: Some(vec![ContentBlock::Text {
                text: "be concise".to_owned(),
            }]),
            messages,
            tools: vec![ToolDef {
                name: "lookup_weather".to_owned(),
                description: Some("Fetch weather".to_owned()),
                input_schema: json!({ "type": "object" }),
            }],
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(128),
            temperature: None,
            top_p: None,
            top_k: None,
            stop: vec!["DONE".to_owned()],
            stream: true,
            extra: Map::new(),
        }
    }
}
