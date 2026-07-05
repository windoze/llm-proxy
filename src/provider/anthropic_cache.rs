//! Stateless prompt-cache marker injection for Anthropic Messages requests.

use serde_json::{Map, Value, json};

use crate::error::{ProxyError, Result};

const CACHE_CONTROL_FIELD: &str = "cache_control";
const MAX_CACHE_CONTROL_BREAKPOINTS: usize = 4;

/// Optional prompt-cache marker injection for Anthropic backend requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnthropicCacheControlInjection {
    /// Preserve the outgoing request body exactly.
    #[default]
    Disabled,
    /// Add up to four explicit ephemeral cache breakpoints to cacheable prompt blocks.
    EphemeralBreakpoints,
}

/// Validates the outgoing Anthropic request body without changing provider-specific fields.
pub fn prepare_anthropic_request_body(body: Value) -> Result<Value> {
    prepare_anthropic_request_body_with_cache_control(
        body,
        AnthropicCacheControlInjection::Disabled,
    )
}

/// Validates and optionally injects stateless Anthropic prompt-cache breakpoints.
pub fn prepare_anthropic_request_body_with_cache_control(
    body: Value,
    cache_control: AnthropicCacheControlInjection,
) -> Result<Value> {
    match cache_control {
        AnthropicCacheControlInjection::Disabled => validate_anthropic_request_body(body),
        AnthropicCacheControlInjection::EphemeralBreakpoints => {
            inject_cache_control_breakpoints(body)
        }
    }
}

/// Adds explicit ephemeral cache-control breakpoints using only the request structure.
pub fn inject_cache_control_breakpoints(body: Value) -> Result<Value> {
    let mut body = expect_request_object(body)?;
    inject_cache_control_breakpoints_into(&mut body)?;
    Ok(Value::Object(body))
}

fn validate_anthropic_request_body(body: Value) -> Result<Value> {
    expect_request_object(body).map(Value::Object)
}

fn expect_request_object(body: Value) -> Result<Map<String, Value>> {
    match body {
        Value::Object(body) => Ok(body),
        _ => Err(ProxyError::ProtocolMapping(
            "Anthropic backend request body must be a JSON object".to_owned(),
        )),
    }
}

fn inject_cache_control_breakpoints_into(body: &mut Map<String, Value>) -> Result<usize> {
    let targets = collect_cache_targets(body)?;
    let existing_count = existing_cache_control_count(body, &targets);

    if existing_count > MAX_CACHE_CONTROL_BREAKPOINTS {
        return Err(ProxyError::ProtocolMapping(format!(
            "Anthropic request already has {existing_count} cache_control breakpoints; maximum is {MAX_CACHE_CONTROL_BREAKPOINTS}"
        )));
    }

    let mut remaining = MAX_CACHE_CONTROL_BREAKPOINTS - existing_count;
    if remaining == 0 {
        return Ok(0);
    }

    let mut selected = Vec::new();
    for target in targets.iter().rev() {
        if target.has_cache_control {
            continue;
        }
        selected.push(target.target.clone());
        remaining -= 1;
        if remaining == 0 {
            break;
        }
    }

    selected.reverse();
    let selected_count = selected.len();
    for target in selected {
        apply_cache_control_target(body, &target)?;
    }

    Ok(selected_count)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheTargetInfo {
    target: CacheTarget,
    has_cache_control: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CacheTarget {
    Tool {
        index: usize,
    },
    SystemString,
    SystemBlock {
        index: usize,
    },
    MessageString {
        message_index: usize,
    },
    MessageBlock {
        message_index: usize,
        block_index: usize,
    },
}

fn existing_cache_control_count(body: &Map<String, Value>, targets: &[CacheTargetInfo]) -> usize {
    usize::from(body.contains_key(CACHE_CONTROL_FIELD))
        + targets
            .iter()
            .filter(|target| target.has_cache_control)
            .count()
}

fn collect_cache_targets(body: &Map<String, Value>) -> Result<Vec<CacheTargetInfo>> {
    let mut targets = Vec::new();
    collect_tool_cache_targets(body.get("tools"), &mut targets)?;
    collect_system_cache_targets(body.get("system"), &mut targets)?;
    collect_message_cache_targets(body.get("messages"), &mut targets)?;
    Ok(targets)
}

fn collect_tool_cache_targets(
    value: Option<&Value>,
    targets: &mut Vec<CacheTargetInfo>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let Value::Array(tools) = value else {
        return Err(mapping_error("request.tools must be an array"));
    };

    for (index, tool) in tools.iter().enumerate() {
        let tool = tool
            .as_object()
            .ok_or_else(|| mapping_error(format!("request.tools[{index}] must be an object")))?;
        targets.push(CacheTargetInfo {
            target: CacheTarget::Tool { index },
            has_cache_control: tool.contains_key(CACHE_CONTROL_FIELD),
        });
    }

    Ok(())
}

fn collect_system_cache_targets(
    value: Option<&Value>,
    targets: &mut Vec<CacheTargetInfo>,
) -> Result<()> {
    match value {
        Some(Value::String(text)) => {
            if !text.is_empty() {
                targets.push(CacheTargetInfo {
                    target: CacheTarget::SystemString,
                    has_cache_control: false,
                });
            }
            Ok(())
        }
        Some(Value::Array(blocks)) => {
            for (index, block) in blocks.iter().enumerate() {
                let block = block.as_object().ok_or_else(|| {
                    mapping_error(format!("request.system[{index}] must be an object"))
                })?;
                if is_cacheable_content_block(block) {
                    targets.push(CacheTargetInfo {
                        target: CacheTarget::SystemBlock { index },
                        has_cache_control: block.contains_key(CACHE_CONTROL_FIELD),
                    });
                }
            }
            Ok(())
        }
        Some(Value::Null) | None => Ok(()),
        Some(_) => Err(mapping_error(
            "request.system must be a string or content block array",
        )),
    }
}

fn collect_message_cache_targets(
    value: Option<&Value>,
    targets: &mut Vec<CacheTargetInfo>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let Value::Array(messages) = value else {
        return Err(mapping_error("request.messages must be an array"));
    };

    for (message_index, message) in messages.iter().enumerate() {
        let message = message.as_object().ok_or_else(|| {
            mapping_error(format!(
                "request.messages[{message_index}] must be an object"
            ))
        })?;
        let content = message.get("content").ok_or_else(|| {
            mapping_error(format!(
                "request.messages[{message_index}].content is required"
            ))
        })?;
        collect_message_content_cache_targets(content, message_index, targets)?;
    }

    Ok(())
}

fn collect_message_content_cache_targets(
    content: &Value,
    message_index: usize,
    targets: &mut Vec<CacheTargetInfo>,
) -> Result<()> {
    match content {
        Value::String(text) => {
            if !text.is_empty() {
                targets.push(CacheTargetInfo {
                    target: CacheTarget::MessageString { message_index },
                    has_cache_control: false,
                });
            }
            Ok(())
        }
        Value::Array(blocks) => {
            for (block_index, block) in blocks.iter().enumerate() {
                let block = block.as_object().ok_or_else(|| {
                    mapping_error(format!(
                        "request.messages[{message_index}].content[{block_index}] must be an object"
                    ))
                })?;
                if is_cacheable_content_block(block) {
                    targets.push(CacheTargetInfo {
                        target: CacheTarget::MessageBlock {
                            message_index,
                            block_index,
                        },
                        has_cache_control: block.contains_key(CACHE_CONTROL_FIELD),
                    });
                }
            }
            Ok(())
        }
        _ => Err(mapping_error(format!(
            "request.messages[{message_index}].content must be a string or content block array"
        ))),
    }
}

fn is_cacheable_content_block(block: &Map<String, Value>) -> bool {
    match block.get("type").and_then(Value::as_str) {
        Some("thinking") => false,
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some(_) => true,
        None => false,
    }
}

fn apply_cache_control_target(body: &mut Map<String, Value>, target: &CacheTarget) -> Result<()> {
    match target {
        CacheTarget::Tool { index } => {
            let tools = body
                .get_mut("tools")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| mapping_error("request.tools cache target disappeared"))?;
            let tool = tools
                .get_mut(*index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| mapping_error("request.tools cache target disappeared"))?;
            insert_ephemeral_cache_control(tool);
        }
        CacheTarget::SystemString => {
            let text = body
                .get("system")
                .and_then(Value::as_str)
                .ok_or_else(|| mapping_error("request.system cache target disappeared"))?
                .to_owned();
            body.insert(
                "system".to_owned(),
                Value::Array(vec![text_cache_block(text)]),
            );
        }
        CacheTarget::SystemBlock { index } => {
            let blocks = body
                .get_mut("system")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| mapping_error("request.system cache target disappeared"))?;
            let block = blocks
                .get_mut(*index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| mapping_error("request.system cache target disappeared"))?;
            insert_ephemeral_cache_control(block);
        }
        CacheTarget::MessageString { message_index } => {
            let messages = body
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| mapping_error("request.messages cache target disappeared"))?;
            let message = messages
                .get_mut(*message_index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| mapping_error("request.messages cache target disappeared"))?;
            let content = message.get_mut("content").ok_or_else(|| {
                mapping_error("request.messages content cache target disappeared")
            })?;
            let text = content
                .as_str()
                .ok_or_else(|| mapping_error("request.messages content cache target disappeared"))?
                .to_owned();
            *content = Value::Array(vec![text_cache_block(text)]);
        }
        CacheTarget::MessageBlock {
            message_index,
            block_index,
        } => {
            let messages = body
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| mapping_error("request.messages cache target disappeared"))?;
            let message = messages
                .get_mut(*message_index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| mapping_error("request.messages cache target disappeared"))?;
            let blocks = message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    mapping_error("request.messages content cache target disappeared")
                })?;
            let block = blocks
                .get_mut(*block_index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    mapping_error("request.messages content block cache target disappeared")
                })?;
            insert_ephemeral_cache_control(block);
        }
    }

    Ok(())
}

fn text_cache_block(text: String) -> Value {
    json!({
        "type": "text",
        "text": text,
        "cache_control": ephemeral_cache_control(),
    })
}

fn insert_ephemeral_cache_control(block: &mut Map<String, Value>) {
    block
        .entry(CACHE_CONTROL_FIELD.to_owned())
        .or_insert_with(ephemeral_cache_control);
}

fn ephemeral_cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn preserves_json_object_request_body() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "thinking": {
                "type": "enabled",
                "budget_tokens": 1024
            }
        });

        assert_eq!(prepare_anthropic_request_body(body.clone()).unwrap(), body);
    }

    #[test]
    fn rejects_non_object_request_body() {
        let err = prepare_anthropic_request_body(json!("not an object")).unwrap_err();

        match err {
            ProxyError::ProtocolMapping(message) => {
                assert!(message.contains("request body must be a JSON object"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn injects_ephemeral_breakpoints_on_latest_cacheable_blocks() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 256,
            "tools": [
                {
                    "name": "older_tool",
                    "input_schema": { "type": "object" }
                },
                {
                    "name": "newer_tool",
                    "input_schema": { "type": "object" }
                }
            ],
            "system": [{
                "type": "text",
                "text": "Stable system prompt."
            }],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "first user turn"
                    }]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "private thought",
                            "signature": "sig_1"
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
                        "content": "sunny"
                    }]
                },
                {
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": "It is sunny."
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "Should I bring sunglasses?"
                    }]
                }
            ]
        });

        let injected = inject_cache_control_breakpoints(body).unwrap();
        let cache_control = ephemeral_cache_control();

        assert_eq!(
            existing_cache_control_count(
                injected.as_object().unwrap(),
                &collect_cache_targets(injected.as_object().unwrap()).unwrap(),
            ),
            MAX_CACHE_CONTROL_BREAKPOINTS
        );
        assert!(
            injected["tools"][1]
                .as_object()
                .unwrap()
                .get(CACHE_CONTROL_FIELD)
                .is_none()
        );
        assert!(
            injected["system"][0]
                .as_object()
                .unwrap()
                .get(CACHE_CONTROL_FIELD)
                .is_none()
        );
        assert!(
            injected["messages"][1]["content"][0]
                .as_object()
                .unwrap()
                .get(CACHE_CONTROL_FIELD)
                .is_none()
        );
        assert_eq!(
            injected["messages"][1]["content"][1][CACHE_CONTROL_FIELD],
            cache_control
        );
        assert_eq!(
            injected["messages"][2]["content"][0][CACHE_CONTROL_FIELD],
            cache_control
        );
        assert_eq!(
            injected["messages"][3]["content"][0][CACHE_CONTROL_FIELD],
            cache_control
        );
        assert_eq!(
            injected["messages"][4]["content"][0][CACHE_CONTROL_FIELD],
            cache_control
        );
    }

    #[test]
    fn converts_string_content_when_selected_for_cache_control() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "system": "Stable system prompt.",
            "messages": [{
                "role": "user",
                "content": "hello"
            }]
        });

        let injected = inject_cache_control_breakpoints(body).unwrap();

        assert_eq!(
            injected["system"],
            json!([{
                "type": "text",
                "text": "Stable system prompt.",
                "cache_control": { "type": "ephemeral" }
            }])
        );
        assert_eq!(
            injected["messages"][0]["content"],
            json!([{
                "type": "text",
                "text": "hello",
                "cache_control": { "type": "ephemeral" }
            }])
        );
    }

    #[test]
    fn preserves_existing_cache_control_and_is_idempotent() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "system": [{
                "type": "text",
                "text": "Stable system prompt.",
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "first turn"
                    }]
                },
                {
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": "assistant turn"
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "latest turn"
                    }]
                }
            ]
        });

        let injected = inject_cache_control_breakpoints(body).unwrap();
        let reinjected = inject_cache_control_breakpoints(injected.clone()).unwrap();

        assert_eq!(reinjected, injected);
        assert_eq!(
            injected["system"][0][CACHE_CONTROL_FIELD],
            ephemeral_cache_control()
        );
        assert_eq!(
            injected["messages"][0]["content"][0][CACHE_CONTROL_FIELD],
            ephemeral_cache_control()
        );
        assert_eq!(
            injected["messages"][1]["content"][0][CACHE_CONTROL_FIELD],
            ephemeral_cache_control()
        );
        assert_eq!(
            injected["messages"][2]["content"][0][CACHE_CONTROL_FIELD],
            ephemeral_cache_control()
        );
    }

    #[test]
    fn rejects_more_than_four_existing_cache_control_markers() {
        let err = inject_cache_control_breakpoints(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "cache_control": { "type": "ephemeral" },
            "tools": [{
                "name": "tool",
                "input_schema": { "type": "object" },
                "cache_control": { "type": "ephemeral" }
            }],
            "system": [{
                "type": "text",
                "text": "Stable system prompt.",
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "first",
                        "cache_control": { "type": "ephemeral" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "second",
                        "cache_control": { "type": "ephemeral" }
                    }]
                }
            ]
        }))
        .unwrap_err();

        match err {
            ProxyError::ProtocolMapping(message) => {
                assert!(message.contains("already has 5 cache_control breakpoints"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
