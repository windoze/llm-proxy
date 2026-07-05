//! Anthropic Messages stream decoding into provider-neutral IR events.

// M6 wires this decoder into the rich Anthropic → Responses streaming bridge.
#![allow(dead_code)]

use std::collections::BTreeMap;

use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::{
        event::{BlockKind, IrEvent},
        message::Provider,
        request::{StopReason, Usage},
    },
    stream::sse::SseEvent,
};

const PROTOCOL: &str = "anthropic";
const MESSAGE_START: &str = "message_start";
const CONTENT_BLOCK_START: &str = "content_block_start";
const CONTENT_BLOCK_DELTA: &str = "content_block_delta";
const CONTENT_BLOCK_STOP: &str = "content_block_stop";
const MESSAGE_DELTA: &str = "message_delta";
const MESSAGE_STOP: &str = "message_stop";

/// Boxed stream of decoded IR events using the proxy's shared error type.
pub type IrEventStream = BoxStream<'static, Result<IrEvent>>;

/// Converts normalized Anthropic Messages SSE events into provider-neutral streaming IR events.
pub fn anthropic_sse_to_ir_events<S>(events: S) -> IrEventStream
where
    S: Stream<Item = Result<SseEvent>> + Send + 'static,
{
    async_stream::try_stream! {
        let mut decoder = AnthropicStreamDecoder::new();
        futures_util::pin_mut!(events);

        while let Some(event) = events.next().await {
            let event = event?;
            for ir_event in decoder.decode_sse_event(&event)? {
                yield ir_event;
            }
        }

        for ir_event in decoder.finish()? {
            yield ir_event;
        }
    }
    .boxed()
}

/// Stateful decoder for Anthropic Messages API stream events.
#[derive(Debug, Default)]
pub struct AnthropicStreamDecoder {
    message: Option<MessageState>,
    next_block_index: usize,
    open_blocks: BTreeMap<usize, BlockState>,
    usage: UsageState,
    saw_tool_use: bool,
    saw_terminal_delta: bool,
    emitted_message_stop: bool,
}

#[derive(Debug)]
struct MessageState {
    id: String,
    model: String,
}

#[derive(Debug)]
enum BlockState {
    Text(TextBlockState),
    Thinking(ThinkingBlockState),
    ToolUse(ToolUseBlockState),
}

#[derive(Debug, Default)]
struct TextBlockState {
    text: String,
}

#[derive(Debug, Default)]
struct ThinkingBlockState {
    text: String,
    signature: Option<Vec<u8>>,
}

#[derive(Debug)]
struct ToolUseBlockState {
    id: String,
    name: String,
    partial_json: String,
}

#[derive(Debug, Default)]
struct UsageState {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cache_read: Option<u32>,
    cache_write: Option<u32>,
}

impl AnthropicStreamDecoder {
    /// Creates an empty decoder ready to consume the first Anthropic stream event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes one normalized Anthropic SSE event.
    pub fn decode_sse_event(&mut self, event: &SseEvent) -> Result<Vec<IrEvent>> {
        let data = serde_json::from_str(&event.data)?;
        let event_type = normalized_event_type(event, &data)?;
        self.decode_event(event_type, &data)
    }

    /// Decodes one Anthropic stream JSON event payload.
    pub fn decode_event(&mut self, event_type: &str, data: &Value) -> Result<Vec<IrEvent>> {
        let event = data
            .as_object()
            .ok_or_else(|| mapping_error("stream event data must be a JSON object"))?;

        match event_type {
            MESSAGE_START => self.decode_message_start(event),
            CONTENT_BLOCK_START => self.decode_content_block_start(event),
            CONTENT_BLOCK_DELTA => self.decode_content_block_delta(event),
            CONTENT_BLOCK_STOP => self.decode_content_block_stop(event),
            MESSAGE_DELTA => self.decode_message_delta(event),
            MESSAGE_STOP => self.decode_message_stop(),
            other => Err(ProxyError::UnsupportedFeature {
                feature: format!("Anthropic stream event `{other}`"),
                protocol: PROTOCOL.to_owned(),
            }),
        }
    }

    /// Finishes the decoder after the upstream SSE stream ends.
    pub fn finish(&mut self) -> Result<Vec<IrEvent>> {
        if self.message.is_none() {
            return Err(mapping_error("stream ended before message_start"));
        }
        if !self.emitted_message_stop {
            return Err(mapping_error("stream ended before message_stop"));
        }

        Ok(Vec::new())
    }

    fn decode_message_start(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        if self.message.is_some() {
            return Err(mapping_error("message_start received more than once"));
        }

        let message = required_object(event, "message", "event.message")?;
        validate_type(message, "message", "event.message.type")?;
        validate_role(message, "assistant", "event.message.role")?;
        self.merge_usage(message.get("usage"), "event.message.usage")?;

        let id = required_string(message, "id", "event.message.id")?.to_owned();
        let model = required_string(message, "model", "event.message.model")?.to_owned();
        self.message = Some(MessageState {
            id: id.clone(),
            model: model.clone(),
        });

        Ok(vec![IrEvent::MessageStart { id, model }])
    }

    fn decode_content_block_start(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_message_started("content_block_start")?;
        self.ensure_not_terminal("content_block_start")?;
        let index = required_usize(event, "index", "event.index")?;
        if index != self.next_block_index {
            return Err(mapping_error(format!(
                "content_block_start index {index} does not match expected index {}",
                self.next_block_index
            )));
        }

        let content_block = required_object(event, "content_block", "event.content_block")?;
        let block_type = required_string(content_block, "type", "event.content_block.type")?;
        let mut events = Vec::new();
        let state = match block_type {
            "text" => {
                let text = optional_string(content_block, "text", "event.content_block.text")?
                    .unwrap_or_default()
                    .to_owned();
                events.push(IrEvent::BlockStart {
                    index,
                    block: BlockKind::Text,
                });
                if !text.is_empty() {
                    events.push(IrEvent::TextDelta {
                        index,
                        text: text.clone(),
                    });
                }
                BlockState::Text(TextBlockState { text })
            }
            "thinking" => {
                let text =
                    optional_string(content_block, "thinking", "event.content_block.thinking")?
                        .unwrap_or_default()
                        .to_owned();
                let signature = optional_non_empty_string(
                    content_block,
                    "signature",
                    "event.content_block.signature",
                )?
                .map(|value| value.as_bytes().to_vec());
                events.push(IrEvent::BlockStart {
                    index,
                    block: BlockKind::Thinking,
                });
                if !text.is_empty() {
                    events.push(IrEvent::ThinkingDelta {
                        index,
                        text: text.clone(),
                    });
                }
                if let Some(signature) = &signature {
                    events.push(IrEvent::ThinkingMetadata {
                        index,
                        source: Provider::Anthropic,
                        opaque: signature.clone(),
                    });
                }
                BlockState::Thinking(ThinkingBlockState { text, signature })
            }
            "tool_use" => {
                let id = required_string(content_block, "id", "event.content_block.id")?.to_owned();
                let name =
                    required_string(content_block, "name", "event.content_block.name")?.to_owned();
                self.saw_tool_use = true;
                events.push(IrEvent::BlockStart {
                    index,
                    block: BlockKind::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                    },
                });
                BlockState::ToolUse(ToolUseBlockState {
                    id,
                    name,
                    partial_json: String::new(),
                })
            }
            other => {
                return Err(ProxyError::UnsupportedFeature {
                    feature: format!("Anthropic content block type `{other}`"),
                    protocol: PROTOCOL.to_owned(),
                });
            }
        };

        if self.open_blocks.insert(index, state).is_some() {
            return Err(mapping_error(format!(
                "content_block_start repeated index {index}"
            )));
        }
        self.next_block_index += 1;

        Ok(events)
    }

    fn decode_content_block_delta(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_message_started("content_block_delta")?;
        self.ensure_not_terminal("content_block_delta")?;
        let index = required_usize(event, "index", "event.index")?;
        let delta = required_object(event, "delta", "event.delta")?;
        let delta_type = required_string(delta, "type", "event.delta.type")?;

        match delta_type {
            "text_delta" => {
                let text = required_string(delta, "text", "event.delta.text")?;
                if text.is_empty() {
                    return Ok(Vec::new());
                }
                self.text_block_mut(index, "content_block_delta")?
                    .text
                    .push_str(text);
                Ok(vec![IrEvent::TextDelta {
                    index,
                    text: text.to_owned(),
                }])
            }
            "thinking_delta" => {
                let text = required_string(delta, "thinking", "event.delta.thinking")?;
                if text.is_empty() {
                    return Ok(Vec::new());
                }
                self.thinking_block_mut(index, "content_block_delta")?
                    .text
                    .push_str(text);
                Ok(vec![IrEvent::ThinkingDelta {
                    index,
                    text: text.to_owned(),
                }])
            }
            "signature_delta" => {
                let signature = required_string(delta, "signature", "event.delta.signature")?;
                if signature.is_empty() {
                    return Err(mapping_error("event.delta.signature must not be empty"));
                }
                let state = self.thinking_block_mut(index, "content_block_delta")?;
                if state.signature.is_some() {
                    return Err(mapping_error(format!(
                        "signature_delta received more than once for thinking block index {index}"
                    )));
                }
                let opaque = signature.as_bytes().to_vec();
                state.signature = Some(opaque.clone());
                Ok(vec![IrEvent::ThinkingMetadata {
                    index,
                    source: Provider::Anthropic,
                    opaque,
                }])
            }
            "input_json_delta" => {
                let partial_json =
                    required_string(delta, "partial_json", "event.delta.partial_json")?;
                if partial_json.is_empty() {
                    return Ok(Vec::new());
                }
                self.tool_block_mut(index, "content_block_delta")?
                    .partial_json
                    .push_str(partial_json);
                Ok(vec![IrEvent::ToolUseDelta {
                    index,
                    partial_json: partial_json.to_owned(),
                }])
            }
            other => Err(ProxyError::UnsupportedFeature {
                feature: format!("Anthropic content block delta type `{other}`"),
                protocol: PROTOCOL.to_owned(),
            }),
        }
    }

    fn decode_content_block_stop(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_message_started("content_block_stop")?;
        let index = required_usize(event, "index", "event.index")?;
        let state = self.open_blocks.remove(&index).ok_or_else(|| {
            mapping_error(format!(
                "content_block_stop received for unopened index {index}"
            ))
        })?;

        if let BlockState::Thinking(state) = state
            && state.signature.is_none()
        {
            return Err(mapping_error(format!(
                "thinking block index {index} stopped before signature_delta"
            )));
        }

        Ok(vec![IrEvent::BlockStop { index }])
    }

    fn decode_message_delta(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_message_started("message_delta")?;
        let delta = required_object(event, "delta", "event.delta")?;
        let stop_reason = optional_string(delta, "stop_reason", "event.delta.stop_reason")?;
        let usage = match event.get("usage") {
            Some(Value::Null) | None => None,
            Some(value) => {
                self.merge_usage(Some(value), "event.usage")?;
                Some(self.usage.to_usage("event.usage")?)
            }
        };

        if stop_reason.is_none() && usage.is_none() {
            return Err(mapping_error(
                "message_delta must include stop_reason or usage",
            ));
        }

        let stop_reason = stop_reason.map(decode_stop_reason);
        if stop_reason.is_some() {
            if self.saw_terminal_delta {
                return Err(mapping_error(
                    "terminal message_delta received more than once",
                ));
            }
            if !self.open_blocks.is_empty() {
                return Err(mapping_error(
                    "terminal message_delta received before all content blocks stopped",
                ));
            }
            self.saw_terminal_delta = true;
        }

        Ok(vec![IrEvent::MessageDelta { stop_reason, usage }])
    }

    fn decode_message_stop(&mut self) -> Result<Vec<IrEvent>> {
        self.ensure_message_started("message_stop")?;
        if !self.open_blocks.is_empty() {
            return Err(mapping_error(
                "message_stop received before all content blocks stopped",
            ));
        }
        if !self.saw_terminal_delta {
            return Err(mapping_error(
                "message_stop received before terminal message_delta",
            ));
        }
        if self.emitted_message_stop {
            return Err(mapping_error("message_stop received more than once"));
        }

        self.emitted_message_stop = true;
        Ok(vec![IrEvent::MessageStop])
    }

    fn merge_usage(&mut self, value: Option<&Value>, path: &str) -> Result<()> {
        let Some(value) = value else {
            return Ok(());
        };
        let usage = match value {
            Value::Object(usage) => usage,
            Value::Null => return Ok(()),
            _ => return Err(mapping_error(format!("{path} must be an object or null"))),
        };

        if let Some(input_tokens) =
            optional_u32(usage, "input_tokens", format!("{path}.input_tokens"))?
        {
            self.usage.input_tokens = Some(input_tokens);
        }
        if let Some(output_tokens) =
            optional_u32(usage, "output_tokens", format!("{path}.output_tokens"))?
        {
            self.usage.output_tokens = Some(output_tokens);
        }
        if let Some(cache_read) = optional_u32(
            usage,
            "cache_read_input_tokens",
            format!("{path}.cache_read_input_tokens"),
        )? {
            self.usage.cache_read = Some(cache_read);
        }
        if let Some(cache_write) = optional_u32(
            usage,
            "cache_creation_input_tokens",
            format!("{path}.cache_creation_input_tokens"),
        )? {
            self.usage.cache_write = Some(cache_write);
        }

        Ok(())
    }

    fn ensure_message_started(&self, event_name: &str) -> Result<()> {
        if self.message.is_none() {
            return Err(mapping_error(format!(
                "{event_name} received before message_start"
            )));
        }
        Ok(())
    }

    fn ensure_not_terminal(&self, event_name: &str) -> Result<()> {
        if self.saw_terminal_delta {
            return Err(mapping_error(format!(
                "{event_name} received after terminal message_delta"
            )));
        }
        Ok(())
    }

    fn text_block_mut(&mut self, index: usize, event_name: &str) -> Result<&mut TextBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::Text(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-text block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened index {index}"
            ))),
        }
    }

    fn thinking_block_mut(
        &mut self,
        index: usize,
        event_name: &str,
    ) -> Result<&mut ThinkingBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::Thinking(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-thinking block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened index {index}"
            ))),
        }
    }

    fn tool_block_mut(&mut self, index: usize, event_name: &str) -> Result<&mut ToolUseBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::ToolUse(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-tool_use block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened index {index}"
            ))),
        }
    }
}

impl UsageState {
    fn to_usage(&self, path: &str) -> Result<Usage> {
        Ok(Usage {
            input_tokens: self
                .input_tokens
                .ok_or_else(|| mapping_error(format!("{path}.input_tokens is required")))?,
            output_tokens: self
                .output_tokens
                .ok_or_else(|| mapping_error(format!("{path}.output_tokens is required")))?,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        })
    }
}

fn normalized_event_type<'a>(event: &'a SseEvent, data: &'a Value) -> Result<&'a str> {
    let data_type = data
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str);
    let event_type = if event.event_type == "message" {
        data_type.ok_or_else(|| mapping_error("stream event data.type is required"))?
    } else {
        event.event_type.as_str()
    };

    if let Some(data_type) = data_type
        && data_type != event_type
    {
        return Err(mapping_error(format!(
            "SSE event type `{event_type}` does not match data.type `{data_type}`"
        )));
    }

    Ok(event_type)
}

fn validate_type(object: &Map<String, Value>, expected: &str, path: &str) -> Result<()> {
    let actual = required_string(object, "type", path)?;
    if actual != expected {
        return Err(mapping_error(format!("{path} must be `{expected}`")));
    }
    Ok(())
}

fn validate_role(object: &Map<String, Value>, expected: &str, path: &str) -> Result<()> {
    let actual = required_string(object, "role", path)?;
    if actual != expected {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("Anthropic message role `{actual}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }
    Ok(())
}

fn decode_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        other => StopReason::Other(other.to_owned()),
    }
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<&'a Map<String, Value>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Object(value)) => Ok(value),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!("{path} must be an object"))),
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

fn optional_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<&'a str>> {
    Ok(
        optional_string(object, field, path)?
            .and_then(|value| (!value.is_empty()).then_some(value)),
    )
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

fn required_usize(
    object: &Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<usize> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Number(number)) => {
            let value = number
                .as_u64()
                .ok_or_else(|| mapping_error(format!("{path} must be an unsigned integer")))?;
            usize::try_from(value).map_err(|_| mapping_error(format!("{path} is too large")))
        }
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!("{path} must be an unsigned integer"))),
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

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(format!(
        "Anthropic stream decoding failed: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::{Value, json};

    use super::*;
    use crate::protocol::responses::stream::ir_events_to_responses_sse;

    fn decode_events(events: &[(&str, Value)]) -> Result<Vec<IrEvent>> {
        let mut decoder = AnthropicStreamDecoder::new();
        let mut output = Vec::new();
        for (event_type, data) in events {
            output.extend(decoder.decode_event(event_type, data)?);
        }
        output.extend(decoder.finish()?);
        Ok(output)
    }

    fn message_start() -> Value {
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 42,
                    "cache_read_input_tokens": 10,
                    "cache_creation_input_tokens": 3
                }
            }
        })
    }

    fn message_delta() -> Value {
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn",
                "stop_sequence": null
            },
            "usage": {
                "output_tokens": 9
            }
        })
    }

    #[test]
    fn decodes_thinking_signature_stream_to_ir_events() {
        let events = decode_events(&[
            (MESSAGE_START, message_start()),
            (
                CONTENT_BLOCK_START,
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "thinking",
                        "thinking": ""
                    }
                }),
            ),
            (
                CONTENT_BLOCK_DELTA,
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": "I should "
                    }
                }),
            ),
            (
                CONTENT_BLOCK_DELTA,
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": "call a tool."
                    }
                }),
            ),
            (
                CONTENT_BLOCK_DELTA,
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "signature_delta",
                        "signature": "sig_anthropic"
                    }
                }),
            ),
            (
                CONTENT_BLOCK_STOP,
                json!({
                    "type": "content_block_stop",
                    "index": 0
                }),
            ),
            (MESSAGE_DELTA, message_delta()),
            (
                MESSAGE_STOP,
                json!({
                    "type": "message_stop"
                }),
            ),
        ])
        .unwrap();

        assert_eq!(
            events,
            vec![
                IrEvent::MessageStart {
                    id: "msg_1".to_owned(),
                    model: "claude-sonnet-4-5".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 0,
                    block: BlockKind::Thinking,
                },
                IrEvent::ThinkingDelta {
                    index: 0,
                    text: "I should ".to_owned(),
                },
                IrEvent::ThinkingDelta {
                    index: 0,
                    text: "call a tool.".to_owned(),
                },
                IrEvent::ThinkingMetadata {
                    index: 0,
                    source: Provider::Anthropic,
                    opaque: b"sig_anthropic".to_vec(),
                },
                IrEvent::BlockStop { index: 0 },
                IrEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage {
                        input_tokens: 42,
                        output_tokens: 9,
                        cache_read: Some(10),
                        cache_write: Some(3),
                    }),
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn rejects_thinking_stream_without_signature_delta() {
        let mut decoder = AnthropicStreamDecoder::new();
        decoder
            .decode_event(MESSAGE_START, &message_start())
            .unwrap();
        decoder
            .decode_event(
                CONTENT_BLOCK_START,
                &json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "thinking",
                        "thinking": ""
                    }
                }),
            )
            .unwrap();

        let error = decoder
            .decode_event(
                CONTENT_BLOCK_STOP,
                &json!({
                    "type": "content_block_stop",
                    "index": 0
                }),
            )
            .unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("stopped before signature_delta"))
        );
    }

    #[test]
    fn decodes_terminal_delta_with_null_usage() {
        let events = decode_events(&[
            (MESSAGE_START, message_start()),
            (
                CONTENT_BLOCK_START,
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "text",
                        "text": ""
                    }
                }),
            ),
            (
                CONTENT_BLOCK_STOP,
                json!({
                    "type": "content_block_stop",
                    "index": 0
                }),
            ),
            (
                MESSAGE_DELTA,
                json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": "end_turn",
                        "stop_sequence": null
                    },
                    "usage": null
                }),
            ),
            (
                MESSAGE_STOP,
                json!({
                    "type": "message_stop"
                }),
            ),
        ])
        .unwrap();

        assert_eq!(events.last(), Some(&IrEvent::MessageStop),);
        assert!(matches!(
            events.get(events.len() - 2),
            Some(IrEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            })
        ));
    }

    #[tokio::test]
    async fn stream_wrapper_accepts_default_sse_message_event_type() {
        let events = stream::iter([
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: message_start().to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "thinking",
                        "thinking": "",
                        "signature": "sig_anthropic"
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: json!({
                    "type": "content_block_stop",
                    "index": 0
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: message_delta().to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: json!({
                    "type": "message_stop"
                })
                .to_string(),
            }),
        ]);
        let mut decoded = anthropic_sse_to_ir_events(events);
        let mut output = Vec::new();

        while let Some(event) = decoded.next().await {
            output.push(event.unwrap());
        }

        assert_eq!(
            output,
            vec![
                IrEvent::MessageStart {
                    id: "msg_1".to_owned(),
                    model: "claude-sonnet-4-5".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 0,
                    block: BlockKind::Thinking,
                },
                IrEvent::ThinkingMetadata {
                    index: 0,
                    source: Provider::Anthropic,
                    opaque: b"sig_anthropic".to_vec(),
                },
                IrEvent::BlockStop { index: 0 },
                IrEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage {
                        input_tokens: 42,
                        output_tokens: 9,
                        cache_read: Some(10),
                        cache_write: Some(3),
                    }),
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[tokio::test]
    async fn stream_wrapper_feeds_responses_sse_with_anthropic_reasoning_envelope() {
        let events = stream::iter([
            Ok(SseEvent {
                event_type: MESSAGE_START.to_owned(),
                data: message_start().to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_START.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_START,
                    "index": 0,
                    "content_block": {
                        "type": "thinking",
                        "thinking": ""
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": "Need "
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": "weather."
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 0,
                    "delta": {
                        "type": "signature_delta",
                        "signature": "sig_real_anthropic_stream"
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_STOP.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_STOP,
                    "index": 0
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_START.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_START,
                    "index": 1,
                    "content_block": {
                        "type": "text",
                        "text": ""
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 1,
                    "delta": {
                        "type": "text_delta",
                        "text": "Calling the weather tool."
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_STOP.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_STOP,
                    "index": 1
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_START.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_START,
                    "index": 2,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_weather_stream",
                        "name": "lookup_weather",
                        "input": {}
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 2,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"city\""
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_DELTA.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_DELTA,
                    "index": 2,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": ":\"Paris\"}"
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: CONTENT_BLOCK_STOP.to_owned(),
                data: json!({
                    "type": CONTENT_BLOCK_STOP,
                    "index": 2
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: MESSAGE_DELTA.to_owned(),
                data: json!({
                    "type": MESSAGE_DELTA,
                    "delta": {
                        "stop_reason": "tool_use",
                        "stop_sequence": null
                    },
                    "usage": {
                        "output_tokens": 9
                    }
                })
                .to_string(),
            }),
            Ok(SseEvent {
                event_type: MESSAGE_STOP.to_owned(),
                data: json!({
                    "type": MESSAGE_STOP
                })
                .to_string(),
            }),
        ]);

        let ir_events = anthropic_sse_to_ir_events(events);
        let frames = ir_events_to_responses_sse(ir_events)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let parsed = frames.into_iter().map(parse_sse_frame).collect::<Vec<_>>();

        assert_eq!(parsed[0].0, "response.created");
        assert_eq!(parsed[1].0, "response.in_progress");
        assert!(parsed.iter().any(|(event_type, data)| {
            event_type == "response.output_item.added"
                && data["output_index"] == json!(1)
                && data["item"]["type"] == json!("message")
        }));
        assert!(parsed.iter().any(|(event_type, data)| {
            event_type == "response.output_item.added"
                && data["output_index"] == json!(2)
                && data["item"]["type"] == json!("function_call")
                && data["item"]["call_id"] == json!("toolu_weather_stream")
                && data["item"]["name"] == json!("lookup_weather")
        }));

        let reasoning_done = parsed
            .iter()
            .find(|(event_type, data)| {
                event_type == "response.output_item.done" && data["output_index"] == json!(0)
            })
            .expect("expected completed reasoning item");
        let source_block = crate::reasoning::envelope::unwrap_from_responses_reasoning_item(
            &reasoning_done.1["item"],
        )
        .unwrap();

        assert_eq!(source_block.source, Provider::Anthropic);
        assert_eq!(
            source_block.payload_json().unwrap(),
            json!({
                "type": "thinking",
                "thinking": "Need weather.",
                "signature": "sig_real_anthropic_stream"
            })
        );

        let completed = parsed
            .iter()
            .find(|(event_type, _)| event_type == "response.completed")
            .expect("expected completed response");
        assert_eq!(completed.1["response"]["status"], "completed");
        assert_eq!(completed.1["response"]["output"][0]["type"], "reasoning");
        assert_eq!(
            completed.1["response"]["output"][1]["content"][0]["text"],
            "Calling the weather tool."
        );
        assert_eq!(
            completed.1["response"]["output"][2]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(completed.1["response"]["usage"]["input_tokens"], 42);
        assert_eq!(completed.1["response"]["usage"]["output_tokens"], 9);
    }

    fn parse_sse_frame(bytes: Bytes) -> (String, Value) {
        let frame = std::str::from_utf8(&bytes).unwrap();
        let mut event_type = None;
        let mut data = None;

        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("event: ") {
                event_type = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("data: ") {
                data = Some(serde_json::from_str(value).unwrap());
            }
        }

        (event_type.unwrap(), data.unwrap())
    }
}
