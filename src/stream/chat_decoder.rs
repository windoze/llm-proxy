//! OpenAI Chat-compatible stream decoding into provider-neutral IR events.

// Later M2 tasks wire this staged decoder into HTTP routing and protocol-specific encoders.
#![allow(dead_code)]

use std::collections::BTreeMap;

use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::{
        event::{BlockKind, IrEvent},
        request::{StopReason, Usage},
    },
    stream::sse::SseEvent,
};

const PROTOCOL: &str = "openai_chat";

/// Boxed stream of decoded IR events using the proxy's shared error type.
pub type IrEventStream = BoxStream<'static, Result<IrEvent>>;

/// Converts normalized OpenAI Chat SSE events into provider-neutral streaming IR events.
pub fn chat_sse_to_ir_events<S>(events: S) -> IrEventStream
where
    S: Stream<Item = Result<SseEvent>> + Send + 'static,
{
    async_stream::try_stream! {
        let mut decoder = ChatStreamDecoder::new();
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

/// Stateful decoder for OpenAI Chat Completions stream chunks.
#[derive(Debug, Default)]
pub struct ChatStreamDecoder {
    message: Option<MessageState>,
    next_block_index: usize,
    text_block_index: Option<usize>,
    thinking_block_index: Option<usize>,
    tool_blocks: BTreeMap<u64, ToolState>,
    open_blocks: Vec<usize>,
    saw_finish_reason: bool,
    emitted_message_stop: bool,
}

#[derive(Debug)]
struct MessageState {
    id: String,
    model: String,
}

#[derive(Debug, Default)]
struct ToolState {
    id: Option<String>,
    name: Option<String>,
    block_index: Option<usize>,
    pending_arguments: Vec<String>,
}

impl ChatStreamDecoder {
    /// Creates an empty decoder ready to consume the first stream chunk.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes one normalized OpenAI Chat SSE event.
    pub fn decode_sse_event(&mut self, event: &SseEvent) -> Result<Vec<IrEvent>> {
        let chunk = serde_json::from_str(&event.data)?;
        self.decode_chunk(&chunk)
    }

    /// Decodes one OpenAI Chat stream JSON chunk.
    pub fn decode_chunk(&mut self, chunk: &Value) -> Result<Vec<IrEvent>> {
        let chunk = chunk
            .as_object()
            .ok_or_else(|| mapping_error("stream chunk must be a JSON object"))?;
        let mut events = Vec::new();

        self.ensure_message_started(chunk, &mut events)?;

        let usage = decode_usage(chunk.get("usage"), "chunk.usage")?;
        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| mapping_error("chunk.choices must be an array"))?;

        if choices.is_empty() {
            if let Some(usage) = usage {
                self.ensure_not_stopped()?;
                events.push(IrEvent::MessageDelta {
                    stop_reason: None,
                    usage: Some(usage),
                });
            }
            return Ok(events);
        }

        if choices.len() != 1 {
            return Err(ProxyError::UnsupportedFeature {
                feature: "streaming multiple choices".to_owned(),
                protocol: PROTOCOL.to_owned(),
            });
        }

        self.ensure_not_stopped()?;
        let choice = choices[0]
            .as_object()
            .ok_or_else(|| mapping_error("chunk.choices[0] must be an object"))?;
        validate_choice_index(choice)?;

        if let Some(delta) = optional_object(choice, "delta", "chunk.choices[0].delta")? {
            self.decode_delta(delta, &mut events)?;
        }

        if let Some(stop_reason) = decode_finish_reason(choice.get("finish_reason"))? {
            if self.saw_finish_reason {
                return Err(mapping_error("stream received more than one finish_reason"));
            }
            self.validate_pending_tools()?;
            events.extend(self.close_open_blocks());
            self.saw_finish_reason = true;
            events.push(IrEvent::MessageDelta {
                stop_reason: Some(stop_reason),
                usage,
            });
        } else if let Some(usage) = usage {
            events.push(IrEvent::MessageDelta {
                stop_reason: None,
                usage: Some(usage),
            });
        }

        Ok(events)
    }

    /// Finishes the stream after the upstream `[DONE]` marker or end-of-stream.
    pub fn finish(&mut self) -> Result<Vec<IrEvent>> {
        if self.message.is_none() {
            return Err(mapping_error("stream ended before the first message chunk"));
        }
        if !self.saw_finish_reason {
            return Err(mapping_error("stream ended before finish_reason"));
        }
        if self.emitted_message_stop {
            return Ok(Vec::new());
        }

        self.emitted_message_stop = true;
        Ok(vec![IrEvent::MessageStop])
    }

    fn ensure_message_started(
        &mut self,
        chunk: &Map<String, Value>,
        events: &mut Vec<IrEvent>,
    ) -> Result<()> {
        let id = required_string(chunk, "id", "chunk.id")?;
        let model = required_string(chunk, "model", "chunk.model")?;

        match &self.message {
            Some(message) if message.id != id || message.model != model => Err(mapping_error(
                "stream chunk id/model changed after message_start",
            )),
            Some(_) => Ok(()),
            None => {
                self.message = Some(MessageState {
                    id: id.to_owned(),
                    model: model.to_owned(),
                });
                events.push(IrEvent::MessageStart {
                    id: id.to_owned(),
                    model: model.to_owned(),
                });
                Ok(())
            }
        }
    }

    fn ensure_not_stopped(&self) -> Result<()> {
        if self.emitted_message_stop {
            return Err(mapping_error("stream chunk received after message_stop"));
        }
        Ok(())
    }

    fn decode_delta(
        &mut self,
        delta: &Map<String, Value>,
        events: &mut Vec<IrEvent>,
    ) -> Result<()> {
        if let Some(reasoning) = optional_string(
            delta,
            "reasoning_content",
            "chunk.choices[0].delta.reasoning_content",
        )? && !reasoning.is_empty()
        {
            let index = self.ensure_thinking_block(events);
            events.push(IrEvent::ThinkingDelta {
                index,
                text: reasoning.to_owned(),
            });
        }

        if let Some(content) = optional_string(delta, "content", "chunk.choices[0].delta.content")?
            && !content.is_empty()
        {
            let index = self.ensure_text_block(events);
            events.push(IrEvent::TextDelta {
                index,
                text: content.to_owned(),
            });
        }

        if let Some(tool_calls) =
            optional_array(delta, "tool_calls", "chunk.choices[0].delta.tool_calls")?
        {
            for (index, tool_call) in tool_calls.iter().enumerate() {
                self.decode_tool_call_delta(
                    tool_call,
                    format!("chunk.choices[0].delta.tool_calls[{index}]"),
                    events,
                )?;
            }
        }

        Ok(())
    }

    fn ensure_thinking_block(&mut self, events: &mut Vec<IrEvent>) -> usize {
        match self.thinking_block_index {
            Some(index) => index,
            None => {
                let index = self.start_block(BlockKind::Thinking, events);
                self.thinking_block_index = Some(index);
                index
            }
        }
    }

    fn ensure_text_block(&mut self, events: &mut Vec<IrEvent>) -> usize {
        match self.text_block_index {
            Some(index) => index,
            None => {
                let index = self.start_block(BlockKind::Text, events);
                self.text_block_index = Some(index);
                index
            }
        }
    }

    fn decode_tool_call_delta(
        &mut self,
        value: &Value,
        path: String,
        events: &mut Vec<IrEvent>,
    ) -> Result<()> {
        let tool_call = value
            .as_object()
            .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
        let tool_index = required_u64(tool_call, "index", format!("{path}.index"))?;

        if let Some(tool_type) = optional_string(tool_call, "type", format!("{path}.type"))?
            && tool_type != "function"
        {
            return Err(ProxyError::UnsupportedFeature {
                feature: format!("streaming tool call type `{tool_type}`"),
                protocol: PROTOCOL.to_owned(),
            });
        }

        let function = optional_object(tool_call, "function", format!("{path}.function"))?;
        let name = match function {
            Some(function) => optional_string(function, "name", format!("{path}.function.name"))?,
            None => None,
        };
        let arguments = match function {
            Some(function) => {
                optional_string(function, "arguments", format!("{path}.function.arguments"))?
            }
            None => None,
        };
        let id = optional_string(tool_call, "id", format!("{path}.id"))?;

        self.update_tool_metadata(tool_index, id, name, &path)?;
        self.start_tool_if_ready(tool_index, events);

        if let Some(arguments) = arguments
            && !arguments.is_empty()
        {
            self.emit_or_buffer_tool_arguments(tool_index, arguments, events)?;
        }

        Ok(())
    }

    fn update_tool_metadata(
        &mut self,
        tool_index: u64,
        id: Option<&str>,
        name: Option<&str>,
        path: &str,
    ) -> Result<()> {
        let tool = self.tool_blocks.entry(tool_index).or_default();
        set_once(&mut tool.id, id, format!("{path}.id"))?;
        set_once(&mut tool.name, name, format!("{path}.function.name"))?;
        Ok(())
    }

    fn start_tool_if_ready(&mut self, tool_index: u64, events: &mut Vec<IrEvent>) {
        let Some(tool) = self.tool_blocks.get(&tool_index) else {
            return;
        };
        if tool.block_index.is_some() || tool.id.is_none() || tool.name.is_none() {
            return;
        }

        let id = tool.id.clone().expect("tool id was checked above");
        let name = tool.name.clone().expect("tool name was checked above");
        let block_index = self.start_block(BlockKind::ToolUse { id, name }, events);
        let tool = self
            .tool_blocks
            .get_mut(&tool_index)
            .expect("tool state exists after start");
        tool.block_index = Some(block_index);

        for partial_json in std::mem::take(&mut tool.pending_arguments) {
            events.push(IrEvent::ToolUseDelta {
                index: block_index,
                partial_json,
            });
        }
    }

    fn emit_or_buffer_tool_arguments(
        &mut self,
        tool_index: u64,
        arguments: &str,
        events: &mut Vec<IrEvent>,
    ) -> Result<()> {
        let tool = self
            .tool_blocks
            .get_mut(&tool_index)
            .ok_or_else(|| mapping_error(format!("missing tool state for index {tool_index}")))?;

        match tool.block_index {
            Some(index) => events.push(IrEvent::ToolUseDelta {
                index,
                partial_json: arguments.to_owned(),
            }),
            None => tool.pending_arguments.push(arguments.to_owned()),
        }

        Ok(())
    }

    fn start_block(&mut self, block: BlockKind, events: &mut Vec<IrEvent>) -> usize {
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.open_blocks.push(index);
        events.push(IrEvent::BlockStart { index, block });
        index
    }

    fn close_open_blocks(&mut self) -> Vec<IrEvent> {
        self.open_blocks
            .drain(..)
            .map(|index| IrEvent::BlockStop { index })
            .collect()
    }

    fn validate_pending_tools(&self) -> Result<()> {
        for (tool_index, tool) in &self.tool_blocks {
            if tool.block_index.is_none() {
                return Err(mapping_error(format!(
                    "stream ended before tool call index {tool_index} included both id and function.name"
                )));
            }
        }
        Ok(())
    }
}

fn validate_choice_index(choice: &Map<String, Value>) -> Result<()> {
    let index = required_u64(choice, "index", "chunk.choices[0].index")?;
    if index != 0 {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("streaming choice index {index}"),
            protocol: PROTOCOL.to_owned(),
        });
    }
    Ok(())
}

fn decode_finish_reason(value: Option<&Value>) -> Result<Option<StopReason>> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        Value::String(reason) => Ok(Some(match reason.as_str() {
            "stop" => StopReason::EndTurn,
            "length" => StopReason::MaxTokens,
            "tool_calls" | "function_call" => StopReason::ToolUse,
            "stop_sequence" => StopReason::StopSequence,
            other => StopReason::Other(other.to_owned()),
        })),
        Value::Null => Ok(None),
        _ => Err(mapping_error(
            "chunk.choices[0].finish_reason must be a string or null",
        )),
    }
}

fn decode_usage(value: Option<&Value>, path: &str) -> Result<Option<Usage>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Object(usage) => Ok(Some(Usage {
            input_tokens: required_u32(usage, "prompt_tokens", format!("{path}.prompt_tokens"))?,
            output_tokens: required_u32(
                usage,
                "completion_tokens",
                format!("{path}.completion_tokens"),
            )?,
            cache_read: optional_u32(
                usage,
                "prompt_cache_hit_tokens",
                format!("{path}.prompt_cache_hit_tokens"),
            )?,
            cache_write: optional_u32(
                usage,
                "prompt_cache_miss_tokens",
                format!("{path}.prompt_cache_miss_tokens"),
            )?,
        })),
        Value::Null => Ok(None),
        _ => Err(mapping_error(format!("{path} must be an object or null"))),
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<&'a Map<String, Value>>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be an object"))),
    }
}

fn optional_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<&'a [Value]>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Array(value)) => Ok(Some(value.as_slice())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be an array"))),
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

fn required_u64(object: &Map<String, Value>, field: &str, path: impl Into<String>) -> Result<u64> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| mapping_error(format!("{path} must be an unsigned integer"))),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!("{path} must be an unsigned integer"))),
    }
}

fn set_once(slot: &mut Option<String>, value: Option<&str>, path: String) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };

    match slot {
        Some(existing) if existing != value => Err(mapping_error(format!(
            "{path} changed within the same stream"
        ))),
        Some(_) => Ok(()),
        None => {
            *slot = Some(value.to_owned());
            Ok(())
        }
    }
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    fn decode_chunks(chunks: &[Value]) -> Result<Vec<IrEvent>> {
        let mut decoder = ChatStreamDecoder::new();
        let mut events = Vec::new();

        for chunk in chunks {
            events.extend(decoder.decode_chunk(chunk)?);
        }
        events.extend(decoder.finish()?);

        Ok(events)
    }

    fn chunk(delta: Value, finish_reason: Value) -> Value {
        json!({
            "id": "chatcmpl_1",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }]
        })
    }

    #[test]
    fn decodes_reasoning_and_text_blocks_with_usage() {
        let chunks = vec![
            chunk(json!({ "role": "assistant" }), Value::Null),
            chunk(json!({ "reasoning_content": "Think " }), Value::Null),
            chunk(json!({ "reasoning_content": "carefully." }), Value::Null),
            chunk(json!({ "content": "Answer " }), Value::Null),
            json!({
                "id": "chatcmpl_1",
                "model": "deepseek-reasoner",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "now." },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 5,
                    "prompt_cache_hit_tokens": 3,
                    "prompt_cache_miss_tokens": 9
                }
            }),
        ];

        assert_eq!(
            decode_chunks(&chunks).unwrap(),
            vec![
                IrEvent::MessageStart {
                    id: "chatcmpl_1".to_owned(),
                    model: "deepseek-reasoner".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 0,
                    block: BlockKind::Thinking,
                },
                IrEvent::ThinkingDelta {
                    index: 0,
                    text: "Think ".to_owned(),
                },
                IrEvent::ThinkingDelta {
                    index: 0,
                    text: "carefully.".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 1,
                    block: BlockKind::Text,
                },
                IrEvent::TextDelta {
                    index: 1,
                    text: "Answer ".to_owned(),
                },
                IrEvent::TextDelta {
                    index: 1,
                    text: "now.".to_owned(),
                },
                IrEvent::BlockStop { index: 0 },
                IrEvent::BlockStop { index: 1 },
                IrEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage {
                        input_tokens: 12,
                        output_tokens: 5,
                        cache_read: Some(3),
                        cache_write: Some(9),
                    }),
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn decodes_multiple_tool_calls_with_fragmented_arguments() {
        let chunks = vec![
            chunk(
                json!({
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_weather",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city"
                            }
                        },
                        {
                            "index": 1,
                            "id": "call_time",
                            "type": "function",
                            "function": {
                                "name": "lookup_time",
                                "arguments": "{\"tz"
                            }
                        }
                    ]
                }),
                Value::Null,
            ),
            chunk(
                json!({
                    "tool_calls": [
                        {
                            "index": 1,
                            "function": { "arguments": "\":\"UTC\"" }
                        },
                        {
                            "index": 0,
                            "function": { "arguments": "\":\"Paris\"" }
                        }
                    ]
                }),
                Value::Null,
            ),
            chunk(
                json!({
                    "tool_calls": [
                        { "index": 0, "function": { "arguments": "}" } },
                        { "index": 1, "function": { "arguments": "}" } }
                    ]
                }),
                Value::Null,
            ),
            chunk(json!({}), json!("tool_calls")),
        ];

        assert_eq!(
            decode_chunks(&chunks).unwrap(),
            vec![
                IrEvent::MessageStart {
                    id: "chatcmpl_1".to_owned(),
                    model: "deepseek-reasoner".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 0,
                    block: BlockKind::ToolUse {
                        id: "call_weather".to_owned(),
                        name: "lookup_weather".to_owned(),
                    },
                },
                IrEvent::ToolUseDelta {
                    index: 0,
                    partial_json: "{\"city".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 1,
                    block: BlockKind::ToolUse {
                        id: "call_time".to_owned(),
                        name: "lookup_time".to_owned(),
                    },
                },
                IrEvent::ToolUseDelta {
                    index: 1,
                    partial_json: "{\"tz".to_owned(),
                },
                IrEvent::ToolUseDelta {
                    index: 1,
                    partial_json: "\":\"UTC\"".to_owned(),
                },
                IrEvent::ToolUseDelta {
                    index: 0,
                    partial_json: "\":\"Paris\"".to_owned(),
                },
                IrEvent::ToolUseDelta {
                    index: 0,
                    partial_json: "}".to_owned(),
                },
                IrEvent::ToolUseDelta {
                    index: 1,
                    partial_json: "}".to_owned(),
                },
                IrEvent::BlockStop { index: 0 },
                IrEvent::BlockStop { index: 1 },
                IrEvent::MessageDelta {
                    stop_reason: Some(StopReason::ToolUse),
                    usage: None,
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn buffers_tool_arguments_until_id_and_name_are_known() {
        let chunks = vec![
            chunk(
                json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "{\"x\":" }
                    }]
                }),
                Value::Null,
            ),
            chunk(
                json!({
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_late",
                        "type": "function",
                        "function": {
                            "name": "late_tool",
                            "arguments": "1}"
                        }
                    }]
                }),
                Value::Null,
            ),
            chunk(json!({}), json!("tool_calls")),
        ];

        assert_eq!(
            decode_chunks(&chunks).unwrap(),
            vec![
                IrEvent::MessageStart {
                    id: "chatcmpl_1".to_owned(),
                    model: "deepseek-reasoner".to_owned(),
                },
                IrEvent::BlockStart {
                    index: 0,
                    block: BlockKind::ToolUse {
                        id: "call_late".to_owned(),
                        name: "late_tool".to_owned(),
                    },
                },
                IrEvent::ToolUseDelta {
                    index: 0,
                    partial_json: "{\"x\":".to_owned(),
                },
                IrEvent::ToolUseDelta {
                    index: 0,
                    partial_json: "1}".to_owned(),
                },
                IrEvent::BlockStop { index: 0 },
                IrEvent::MessageDelta {
                    stop_reason: Some(StopReason::ToolUse),
                    usage: None,
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[tokio::test]
    async fn stream_wrapper_preserves_usage_chunk_before_message_stop() {
        let events = stream::iter([
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: chunk(json!({ "content": "done" }), Value::Null).to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: chunk(json!({}), json!("stop")).to_string(),
            }),
            Ok(SseEvent {
                event_type: "message".to_owned(),
                data: json!({
                    "id": "chatcmpl_1",
                    "model": "deepseek-reasoner",
                    "choices": [],
                    "usage": {
                        "prompt_tokens": 7,
                        "completion_tokens": 2
                    }
                })
                .to_string(),
            }),
        ]);

        let decoded = chat_sse_to_ir_events(events)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        assert_eq!(decoded.last(), Some(&IrEvent::MessageStop));
        assert!(decoded.iter().any(|event| {
            matches!(
                event,
                IrEvent::MessageDelta {
                    stop_reason: None,
                    usage: Some(Usage {
                        input_tokens: 7,
                        output_tokens: 2,
                        cache_read: None,
                        cache_write: None,
                    })
                }
            )
        }));
    }

    #[test]
    fn rejects_streams_that_end_before_finish_reason() {
        let mut decoder = ChatStreamDecoder::new();

        decoder
            .decode_chunk(&chunk(json!({ "content": "unfinished" }), Value::Null))
            .unwrap();
        let error = decoder.finish().unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("finish_reason"))
        );
    }
}
