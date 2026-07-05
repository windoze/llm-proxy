//! Streaming encoder for OpenAI Responses API SSE responses.

// Later M3 tasks wire this staged encoder into HTTP routing.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::{Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        event::{BlockKind, IrEvent},
        message::Provider,
        request::{StopReason, Usage},
    },
    reasoning::envelope::{SourceBlock, wrap},
};

use super::encode::{encode_incomplete_details, encode_status, encode_usage, item_id};

const RESPONSE_CREATED: &str = "response.created";
const RESPONSE_IN_PROGRESS: &str = "response.in_progress";
const RESPONSE_OUTPUT_ITEM_ADDED: &str = "response.output_item.added";
const RESPONSE_CONTENT_PART_ADDED: &str = "response.content_part.added";
const RESPONSE_OUTPUT_TEXT_DELTA: &str = "response.output_text.delta";
const RESPONSE_OUTPUT_TEXT_DONE: &str = "response.output_text.done";
const RESPONSE_REASONING_TEXT_DELTA: &str = "response.reasoning_text.delta";
const RESPONSE_REASONING_TEXT_DONE: &str = "response.reasoning_text.done";
const RESPONSE_CONTENT_PART_DONE: &str = "response.content_part.done";
const RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA: &str = "response.function_call_arguments.delta";
const RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE: &str = "response.function_call_arguments.done";
const RESPONSE_OUTPUT_ITEM_DONE: &str = "response.output_item.done";
const RESPONSE_COMPLETED: &str = "response.completed";

/// Boxed byte stream containing Responses-compatible SSE frames.
pub type ResponsesSseStream = BoxStream<'static, Result<Bytes>>;

/// Converts provider-neutral IR events into OpenAI Responses API SSE frames.
pub fn ir_events_to_responses_sse<S>(events: S) -> ResponsesSseStream
where
    S: Stream<Item = Result<IrEvent>> + Send + 'static,
{
    async_stream::try_stream! {
        let mut encoder = ResponsesStreamEncoder::new();
        futures_util::pin_mut!(events);

        while let Some(event) = events.next().await {
            let event = event?;
            for frame in encoder.encode_event(&event)? {
                yield frame;
            }
        }
    }
    .boxed()
}

/// Stateful encoder that validates IR stream ordering while emitting Responses SSE frames.
#[derive(Debug, Default)]
pub struct ResponsesStreamEncoder {
    response: Option<ResponseState>,
    message_stopped: bool,
    next_block_index: usize,
    next_sequence_number: u64,
    open_blocks: BTreeMap<usize, BlockState>,
    completed_items: BTreeMap<usize, Value>,
    terminal_stop_reason: Option<StopReason>,
    usage: Option<Usage>,
}

#[derive(Debug)]
struct ResponseState {
    id: String,
    model: String,
    created_at: u64,
}

#[derive(Debug)]
enum BlockState {
    Text(TextBlockState),
    Reasoning(ReasoningBlockState),
    ToolUse(ToolUseBlockState),
}

#[derive(Debug)]
struct TextBlockState {
    output_index: usize,
    item_id: String,
    text: String,
}

#[derive(Debug)]
struct ReasoningBlockState {
    output_index: usize,
    item_id: String,
    text: String,
    encrypted_content: Option<String>,
}

#[derive(Debug)]
struct ToolUseBlockState {
    output_index: usize,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl ResponsesStreamEncoder {
    /// Creates an encoder ready to consume the first IR stream event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Encodes one IR event into zero or more complete `event:`/`data:` SSE frames.
    pub fn encode_event(&mut self, event: &IrEvent) -> Result<Vec<Bytes>> {
        self.ensure_not_stopped()?;

        let frames = match event {
            IrEvent::MessageStart { id, model } => self.encode_message_start(id, model)?,
            IrEvent::BlockStart { index, block } => self.encode_block_start(*index, block)?,
            IrEvent::TextDelta { index, text } => self.encode_text_delta(*index, text)?,
            IrEvent::ThinkingDelta { index, text } => self.encode_reasoning_delta(*index, text)?,
            IrEvent::ThinkingMetadata {
                index,
                source,
                opaque,
            } => self.encode_reasoning_metadata(*index, source, opaque)?,
            IrEvent::ToolUseDelta {
                index,
                partial_json,
            } => self.encode_tool_use_delta(*index, partial_json)?,
            IrEvent::BlockStop { index } => self.encode_block_stop(*index)?,
            IrEvent::MessageDelta { stop_reason, usage } => {
                self.encode_message_delta(stop_reason.as_ref(), usage.as_ref())?
            }
            IrEvent::MessageStop => self.encode_message_stop()?,
        };

        frames
            .into_iter()
            .map(|(event_type, data)| format_sse_frame(event_type, &data))
            .collect()
    }

    fn encode_message_start(
        &mut self,
        id: &str,
        model: &str,
    ) -> Result<Vec<(&'static str, Value)>> {
        if self.response.is_some() {
            return Err(mapping_error("message_start received more than once"));
        }

        self.response = Some(ResponseState {
            id: id.to_owned(),
            model: model.to_owned(),
            created_at: unix_timestamp(),
        });

        let created_response =
            self.encode_response("in_progress", None, Value::Array(Vec::new()), None)?;
        let in_progress_response = created_response.clone();

        Ok(vec![
            self.wrap_event(RESPONSE_CREATED, json!({ "response": created_response })),
            self.wrap_event(
                RESPONSE_IN_PROGRESS,
                json!({ "response": in_progress_response }),
            ),
        ])
    }

    fn encode_block_start(
        &mut self,
        index: usize,
        block: &BlockKind,
    ) -> Result<Vec<(&'static str, Value)>> {
        let response_id = self
            .response_state("response.output_item.added")?
            .id
            .clone();
        if self.terminal_stop_reason.is_some() {
            return Err(mapping_error(
                "response.output_item.added received after terminal message_delta",
            ));
        }
        if index != self.next_block_index {
            return Err(mapping_error(format!(
                "block_start index {index} does not match expected index {}",
                self.next_block_index
            )));
        }

        let output_index = self.next_block_index;
        self.next_block_index += 1;

        let (state, added_item, content_part_added) = match block {
            BlockKind::Text => {
                let item_id = item_id("msg", &response_id, output_index);
                let state = BlockState::Text(TextBlockState {
                    output_index,
                    item_id: item_id.clone(),
                    text: String::new(),
                });
                (
                    state,
                    encode_text_item(&item_id, "in_progress", None),
                    Some((
                        RESPONSE_CONTENT_PART_ADDED,
                        json!({
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": encode_output_text_part(""),
                        }),
                    )),
                )
            }
            BlockKind::Thinking => {
                let item_id = item_id("rs", &response_id, output_index);
                let state = BlockState::Reasoning(ReasoningBlockState {
                    output_index,
                    item_id: item_id.clone(),
                    text: String::new(),
                    encrypted_content: None,
                });
                (
                    state,
                    encode_reasoning_item(&item_id, "in_progress", None, None),
                    Some((
                        RESPONSE_CONTENT_PART_ADDED,
                        json!({
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": encode_reasoning_text_part(""),
                        }),
                    )),
                )
            }
            BlockKind::ToolUse { id, name } => {
                let item_id = item_id("fc", &response_id, output_index);
                let state = BlockState::ToolUse(ToolUseBlockState {
                    output_index,
                    item_id: item_id.clone(),
                    call_id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                (
                    state,
                    encode_tool_use_item(&item_id, "in_progress", id, name, ""),
                    None,
                )
            }
        };

        if self.open_blocks.insert(index, state).is_some() {
            return Err(mapping_error(format!(
                "block_start received for already-open index {index}"
            )));
        }

        let mut events = vec![self.wrap_event(
            RESPONSE_OUTPUT_ITEM_ADDED,
            json!({
                "output_index": output_index,
                "item": added_item,
            }),
        )];
        if let Some((event_type, data)) = content_part_added {
            events.push(self.wrap_event(event_type, data));
        }
        Ok(events)
    }

    fn encode_text_delta(
        &mut self,
        index: usize,
        text: &str,
    ) -> Result<Vec<(&'static str, Value)>> {
        let (item_id, output_index) = {
            let state = self.text_block_mut(index, RESPONSE_OUTPUT_TEXT_DELTA)?;
            state.text.push_str(text);
            (state.item_id.clone(), state.output_index)
        };

        Ok(vec![self.wrap_event(
            RESPONSE_OUTPUT_TEXT_DELTA,
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": text,
                "logprobs": [],
            }),
        )])
    }

    fn encode_reasoning_delta(
        &mut self,
        index: usize,
        text: &str,
    ) -> Result<Vec<(&'static str, Value)>> {
        let (item_id, output_index) = {
            let state = self.reasoning_block_mut(index, RESPONSE_REASONING_TEXT_DELTA)?;
            state.text.push_str(text);
            (state.item_id.clone(), state.output_index)
        };

        Ok(vec![self.wrap_event(
            RESPONSE_REASONING_TEXT_DELTA,
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": text,
            }),
        )])
    }

    fn encode_reasoning_metadata(
        &mut self,
        index: usize,
        source: &Provider,
        opaque: &[u8],
    ) -> Result<Vec<(&'static str, Value)>> {
        let encrypted_content = encode_reasoning_encrypted_content(source, opaque)?;
        let state = self.reasoning_block_mut(index, "thinking_metadata")?;

        match &state.encrypted_content {
            Some(existing) if existing != &encrypted_content => Err(mapping_error(format!(
                "thinking metadata changed for reasoning block index {index}"
            ))),
            Some(_) => Ok(Vec::new()),
            None => {
                state.encrypted_content = Some(encrypted_content);
                Ok(Vec::new())
            }
        }
    }

    fn encode_tool_use_delta(
        &mut self,
        index: usize,
        partial_json: &str,
    ) -> Result<Vec<(&'static str, Value)>> {
        let (item_id, output_index) = {
            let state = self.tool_use_block_mut(index, RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA)?;
            state.arguments.push_str(partial_json);
            (state.item_id.clone(), state.output_index)
        };

        Ok(vec![self.wrap_event(
            RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "delta": partial_json,
            }),
        )])
    }

    fn encode_block_stop(&mut self, index: usize) -> Result<Vec<(&'static str, Value)>> {
        let state = self.open_blocks.remove(&index).ok_or_else(|| {
            mapping_error(format!("block_stop received for unopened index {index}"))
        })?;

        match state {
            BlockState::Text(state) => self.encode_text_block_stop(state),
            BlockState::Reasoning(state) => self.encode_reasoning_block_stop(state),
            BlockState::ToolUse(state) => self.encode_tool_use_block_stop(state),
        }
    }

    fn encode_text_block_stop(
        &mut self,
        state: TextBlockState,
    ) -> Result<Vec<(&'static str, Value)>> {
        let completed_item = encode_text_item(&state.item_id, "completed", Some(&state.text));
        self.remember_completed_item(state.output_index, completed_item.clone())?;

        Ok(vec![
            self.wrap_event(
                RESPONSE_OUTPUT_TEXT_DONE,
                json!({
                    "item_id": state.item_id,
                    "output_index": state.output_index,
                    "content_index": 0,
                    "text": state.text,
                    "logprobs": [],
                }),
            ),
            self.wrap_event(
                RESPONSE_CONTENT_PART_DONE,
                json!({
                    "item_id": completed_item["id"].clone(),
                    "output_index": state.output_index,
                    "content_index": 0,
                    "part": completed_item["content"][0].clone(),
                }),
            ),
            self.wrap_event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "output_index": state.output_index,
                    "item": completed_item,
                }),
            ),
        ])
    }

    fn encode_reasoning_block_stop(
        &mut self,
        state: ReasoningBlockState,
    ) -> Result<Vec<(&'static str, Value)>> {
        let completed_item = encode_reasoning_item(
            &state.item_id,
            "completed",
            Some(&state.text),
            state.encrypted_content.as_deref(),
        );
        self.remember_completed_item(state.output_index, completed_item.clone())?;

        Ok(vec![
            self.wrap_event(
                RESPONSE_REASONING_TEXT_DONE,
                json!({
                    "item_id": state.item_id,
                    "output_index": state.output_index,
                    "content_index": 0,
                    "text": state.text,
                }),
            ),
            self.wrap_event(
                RESPONSE_CONTENT_PART_DONE,
                json!({
                    "item_id": completed_item["id"].clone(),
                    "output_index": state.output_index,
                    "content_index": 0,
                    "part": completed_item["content"][0].clone(),
                }),
            ),
            self.wrap_event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "output_index": state.output_index,
                    "item": completed_item,
                }),
            ),
        ])
    }

    fn encode_tool_use_block_stop(
        &mut self,
        state: ToolUseBlockState,
    ) -> Result<Vec<(&'static str, Value)>> {
        let completed_item = encode_tool_use_item(
            &state.item_id,
            "completed",
            &state.call_id,
            &state.name,
            &state.arguments,
        );
        self.remember_completed_item(state.output_index, completed_item.clone())?;

        Ok(vec![
            self.wrap_event(
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE,
                json!({
                    "item_id": state.item_id,
                    "output_index": state.output_index,
                    "name": state.name,
                    "arguments": state.arguments,
                }),
            ),
            self.wrap_event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "output_index": state.output_index,
                    "item": completed_item,
                }),
            ),
        ])
    }

    fn encode_message_delta(
        &mut self,
        stop_reason: Option<&StopReason>,
        usage: Option<&Usage>,
    ) -> Result<Vec<(&'static str, Value)>> {
        self.response_state("message_delta")?;
        if stop_reason.is_none() && usage.is_none() {
            return Err(mapping_error(
                "message_delta must include stop_reason or usage",
            ));
        }
        if self.terminal_stop_reason.is_some() {
            return Err(mapping_error(
                "message_delta received after terminal stop_reason",
            ));
        }
        if let Some(usage) = usage {
            self.usage = Some(usage.clone());
        }
        if let Some(stop_reason) = stop_reason {
            if !self.open_blocks.is_empty() {
                return Err(mapping_error(
                    "terminal message_delta received before all output items were done",
                ));
            }
            self.terminal_stop_reason = Some(stop_reason.clone());
        }

        Ok(Vec::new())
    }

    fn encode_message_stop(&mut self) -> Result<Vec<(&'static str, Value)>> {
        self.response_state("message_stop")?;
        if !self.open_blocks.is_empty() {
            return Err(mapping_error(
                "message_stop received before all output items were done",
            ));
        }
        let stop_reason = self
            .terminal_stop_reason
            .clone()
            .ok_or_else(|| mapping_error("message_stop received before terminal message_delta"))?;
        let output = self.completed_output()?;
        let response = self.encode_response(
            encode_status(&stop_reason),
            Some(&stop_reason),
            output,
            self.usage.as_ref(),
        )?;

        self.message_stopped = true;
        Ok(vec![self.wrap_event(
            RESPONSE_COMPLETED,
            json!({ "response": response }),
        )])
    }

    fn text_block_mut(&mut self, index: usize, event_type: &str) -> Result<&mut TextBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::Text(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_type} received for non-text block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_type} received for unopened index {index}"
            ))),
        }
    }

    fn reasoning_block_mut(
        &mut self,
        index: usize,
        event_type: &str,
    ) -> Result<&mut ReasoningBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::Reasoning(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_type} received for non-reasoning block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_type} received for unopened index {index}"
            ))),
        }
    }

    fn tool_use_block_mut(
        &mut self,
        index: usize,
        event_type: &str,
    ) -> Result<&mut ToolUseBlockState> {
        match self.open_blocks.get_mut(&index) {
            Some(BlockState::ToolUse(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_type} received for non-tool block index {index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_type} received for unopened index {index}"
            ))),
        }
    }

    fn remember_completed_item(&mut self, output_index: usize, item: Value) -> Result<()> {
        if self.completed_items.insert(output_index, item).is_some() {
            return Err(mapping_error(format!(
                "output item index {output_index} was completed more than once"
            )));
        }
        Ok(())
    }

    fn completed_output(&self) -> Result<Value> {
        let mut output = Vec::new();
        for output_index in 0..self.next_block_index {
            let item = self.completed_items.get(&output_index).ok_or_else(|| {
                mapping_error(format!(
                    "response completed before output item index {output_index} was done"
                ))
            })?;
            output.push(item.clone());
        }
        Ok(Value::Array(output))
    }

    fn encode_response(
        &self,
        status: &str,
        stop_reason: Option<&StopReason>,
        output: Value,
        usage: Option<&Usage>,
    ) -> Result<Value> {
        let response = self.response_state("response")?;
        Ok(json!({
            "id": response.id,
            "object": "response",
            "created_at": response.created_at,
            "status": status,
            "error": null,
            "incomplete_details": stop_reason
                .map(encode_incomplete_details)
                .unwrap_or(Value::Null),
            "model": response.model,
            "output": output,
            "parallel_tool_calls": true,
            "previous_response_id": null,
            "store": false,
            "usage": usage.map(encode_usage).unwrap_or(Value::Null),
        }))
    }

    fn response_state(&self, event_type: &str) -> Result<&ResponseState> {
        self.response
            .as_ref()
            .ok_or_else(|| mapping_error(format!("{event_type} received before message_start")))
    }

    fn ensure_not_stopped(&self) -> Result<()> {
        if self.message_stopped {
            return Err(mapping_error("event received after message_stop"));
        }
        Ok(())
    }

    fn wrap_event(&mut self, event_type: &'static str, mut data: Value) -> (&'static str, Value) {
        let object = data
            .as_object_mut()
            .expect("Responses stream events must be JSON objects");
        object.insert("type".to_owned(), json!(event_type));
        object.insert(
            "sequence_number".to_owned(),
            json!(self.next_sequence_number),
        );
        self.next_sequence_number += 1;
        (event_type, data)
    }
}

fn encode_text_item(item_id: &str, status: &str, text: Option<&str>) -> Value {
    let content = text
        .map(|text| Value::Array(vec![encode_output_text_part(text)]))
        .unwrap_or_else(|| Value::Array(Vec::new()));

    json!({
        "id": item_id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": content,
    })
}

fn encode_output_text_part(text: &str) -> Value {
    json!({
        "type": "output_text",
        "text": text,
        "annotations": [],
    })
}

fn encode_reasoning_item(
    item_id: &str,
    status: &str,
    text: Option<&str>,
    encrypted_content: Option<&str>,
) -> Value {
    let content = text
        .map(|text| Value::Array(vec![encode_reasoning_text_part(text)]))
        .unwrap_or_else(|| Value::Array(Vec::new()));

    let mut item = json!({
        "id": item_id,
        "type": "reasoning",
        "status": status,
        "summary": [],
        "content": content,
    });

    if let Some(encrypted_content) = encrypted_content {
        item.as_object_mut()
            .expect("reasoning item must be an object")
            .insert(
                "encrypted_content".to_owned(),
                Value::String(encrypted_content.to_owned()),
            );
    }

    item
}

fn encode_reasoning_text_part(text: &str) -> Value {
    json!({
        "type": "reasoning_text",
        "text": text,
    })
}

fn encode_tool_use_item(
    item_id: &str,
    status: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> Value {
    json!({
        "id": item_id,
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

fn encode_reasoning_encrypted_content(source: &Provider, opaque: &[u8]) -> Result<String> {
    match source {
        Provider::Responses => std::str::from_utf8(opaque)
            .map(str::to_owned)
            .map_err(|err| {
                mapping_error(format!(
                    "Responses reasoning encrypted_content metadata must be valid UTF-8: {err}"
                ))
            }),
        Provider::Anthropic => wrap(&SourceBlock::new(Provider::Anthropic, opaque)),
        other => Err(mapping_error(format!(
            "thinking metadata from source {other:?} cannot be encoded as Responses encrypted_content"
        ))),
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must not be before the Unix epoch")
        .as_secs()
}

fn format_sse_frame(event_type: &str, data: &Value) -> Result<Bytes> {
    let data = serde_json::to_string(data)?;
    Ok(Bytes::from(format!(
        "event: {event_type}\ndata: {data}\n\n"
    )))
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(format!(
        "Responses stream encoding failed: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    fn encode_events(events: &[IrEvent]) -> Result<Vec<(String, Value)>> {
        let mut encoder = ResponsesStreamEncoder::new();
        let mut encoded = Vec::new();
        for event in events {
            encoded.extend(
                encoder
                    .encode_event(event)?
                    .into_iter()
                    .map(parse_sse_frame),
            );
        }
        Ok(encoded)
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

    #[test]
    fn encodes_response_stream_lifecycle_with_reasoning_text_tool_call_and_usage() {
        let events = vec![
            IrEvent::MessageStart {
                id: "resp_1".to_owned(),
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
                text: "first.".to_owned(),
            },
            IrEvent::BlockStop { index: 0 },
            IrEvent::BlockStart {
                index: 1,
                block: BlockKind::Text,
            },
            IrEvent::TextDelta {
                index: 1,
                text: "Answer.".to_owned(),
            },
            IrEvent::BlockStop { index: 1 },
            IrEvent::BlockStart {
                index: 2,
                block: BlockKind::ToolUse {
                    id: "call_weather".to_owned(),
                    name: "lookup_weather".to_owned(),
                },
            },
            IrEvent::ToolUseDelta {
                index: 2,
                partial_json: "{\"city\"".to_owned(),
            },
            IrEvent::ToolUseDelta {
                index: 2,
                partial_json: ":\"Paris\"}".to_owned(),
            },
            IrEvent::BlockStop { index: 2 },
            IrEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: Some(Usage {
                    input_tokens: 42,
                    output_tokens: 9,
                    cache_read: Some(10),
                    cache_write: Some(3),
                }),
            },
            IrEvent::MessageStop,
        ];

        let encoded = encode_events(&events).unwrap();
        let event_names = encoded
            .iter()
            .map(|(event_type, _)| event_type.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            event_names,
            vec![
                RESPONSE_CREATED,
                RESPONSE_IN_PROGRESS,
                RESPONSE_OUTPUT_ITEM_ADDED,
                RESPONSE_CONTENT_PART_ADDED,
                RESPONSE_REASONING_TEXT_DELTA,
                RESPONSE_REASONING_TEXT_DELTA,
                RESPONSE_REASONING_TEXT_DONE,
                RESPONSE_CONTENT_PART_DONE,
                RESPONSE_OUTPUT_ITEM_DONE,
                RESPONSE_OUTPUT_ITEM_ADDED,
                RESPONSE_CONTENT_PART_ADDED,
                RESPONSE_OUTPUT_TEXT_DELTA,
                RESPONSE_OUTPUT_TEXT_DONE,
                RESPONSE_CONTENT_PART_DONE,
                RESPONSE_OUTPUT_ITEM_DONE,
                RESPONSE_OUTPUT_ITEM_ADDED,
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE,
                RESPONSE_OUTPUT_ITEM_DONE,
                RESPONSE_COMPLETED,
            ]
        );

        for (sequence_number, (_, data)) in encoded.iter().enumerate() {
            assert_eq!(data["sequence_number"], json!(sequence_number));
        }

        assert_eq!(encoded[0].1["response"]["status"], "in_progress");
        assert_eq!(encoded[0].1["response"]["output"], json!([]));
        assert_eq!(
            encoded[2].1,
            json!({
                "type": "response.output_item.added",
                "sequence_number": 2,
                "output_index": 0,
                "item": {
                    "id": "rs_resp_1_0",
                    "type": "reasoning",
                    "status": "in_progress",
                    "summary": [],
                    "content": []
                }
            })
        );
        assert_eq!(
            encoded[4].1,
            json!({
                "type": "response.reasoning_text.delta",
                "sequence_number": 4,
                "item_id": "rs_resp_1_0",
                "output_index": 0,
                "content_index": 0,
                "delta": "Think "
            })
        );
        assert_eq!(
            encoded[11].1,
            json!({
                "type": "response.output_text.delta",
                "sequence_number": 11,
                "item_id": "msg_resp_1_1",
                "output_index": 1,
                "content_index": 0,
                "delta": "Answer.",
                "logprobs": []
            })
        );
        assert_eq!(
            encoded[18].1,
            json!({
                "type": "response.function_call_arguments.done",
                "sequence_number": 18,
                "item_id": "fc_resp_1_2",
                "output_index": 2,
                "name": "lookup_weather",
                "arguments": "{\"city\":\"Paris\"}"
            })
        );

        let completed = &encoded[20].1["response"];
        assert_eq!(completed["id"], "resp_1");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["incomplete_details"], Value::Null);
        assert_eq!(completed["usage"]["input_tokens"], 42);
        assert_eq!(
            completed["usage"]["input_tokens_details"]["cached_tokens"],
            10
        );
        assert_eq!(completed["usage"]["output_tokens"], 9);
        assert_eq!(completed["usage"]["total_tokens"], 51);
        assert_eq!(completed["output"][0]["content"][0]["text"], "Think first.");
        assert_eq!(completed["output"][1]["content"][0]["text"], "Answer.");
        assert_eq!(completed["output"][2]["arguments"], "{\"city\":\"Paris\"}");
    }

    #[test]
    fn encodes_thinking_metadata_as_reasoning_encrypted_content() {
        let events = vec![
            IrEvent::MessageStart {
                id: "resp_reasoning".to_owned(),
                model: "gpt-5.1".to_owned(),
            },
            IrEvent::BlockStart {
                index: 0,
                block: BlockKind::Thinking,
            },
            IrEvent::ThinkingDelta {
                index: 0,
                text: "Think.".to_owned(),
            },
            IrEvent::ThinkingMetadata {
                index: 0,
                source: Provider::Responses,
                opaque: b"enc_stream".to_vec(),
            },
            IrEvent::BlockStop { index: 0 },
            IrEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            },
            IrEvent::MessageStop,
        ];

        let encoded = encode_events(&events).unwrap();
        let item_done = encoded
            .iter()
            .find(|(event_type, _)| event_type == RESPONSE_OUTPUT_ITEM_DONE)
            .unwrap();

        assert_eq!(item_done.1["item"]["encrypted_content"], "enc_stream");
    }

    #[tokio::test]
    async fn stream_wrapper_formats_multiple_sse_frames_per_message_start() {
        let input = stream::iter([
            Ok(IrEvent::MessageStart {
                id: "resp_stream".to_owned(),
                model: "deepseek-chat".to_owned(),
            }),
            Ok(IrEvent::BlockStart {
                index: 0,
                block: BlockKind::Text,
            }),
            Ok(IrEvent::TextDelta {
                index: 0,
                text: "hello".to_owned(),
            }),
            Ok(IrEvent::BlockStop { index: 0 }),
            Ok(IrEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
            Ok(IrEvent::MessageStop),
        ]);

        let frames = ir_events_to_responses_sse(input)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        assert_eq!(frames.len(), 9);
        let first = std::str::from_utf8(&frames[0]).unwrap();
        assert!(first.starts_with("event: response.created\ndata: {"));
        assert!(first.ends_with("\n\n"));
        assert_eq!(parse_sse_frame(frames[1].clone()).0, RESPONSE_IN_PROGRESS);
        assert_eq!(
            parse_sse_frame(frames[8].clone()).1["response"]["usage"],
            Value::Null
        );
    }

    #[test]
    fn rejects_non_sequential_block_indexes() {
        let mut encoder = ResponsesStreamEncoder::new();
        encoder
            .encode_event(&IrEvent::MessageStart {
                id: "resp_1".to_owned(),
                model: "model".to_owned(),
            })
            .unwrap();

        let error = encoder
            .encode_event(&IrEvent::BlockStart {
                index: 1,
                block: BlockKind::Text,
            })
            .unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("expected index 0"))
        );
    }

    #[test]
    fn rejects_delta_for_wrong_block_type() {
        let mut encoder = ResponsesStreamEncoder::new();
        encoder
            .encode_event(&IrEvent::MessageStart {
                id: "resp_1".to_owned(),
                model: "model".to_owned(),
            })
            .unwrap();
        encoder
            .encode_event(&IrEvent::BlockStart {
                index: 0,
                block: BlockKind::ToolUse {
                    id: "call_1".to_owned(),
                    name: "lookup".to_owned(),
                },
            })
            .unwrap();

        let error = encoder
            .encode_event(&IrEvent::TextDelta {
                index: 0,
                text: "wrong".to_owned(),
            })
            .unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("non-text block index 0"))
        );
    }

    #[test]
    fn rejects_message_stop_without_terminal_delta() {
        let mut encoder = ResponsesStreamEncoder::new();
        encoder
            .encode_event(&IrEvent::MessageStart {
                id: "resp_1".to_owned(),
                model: "model".to_owned(),
            })
            .unwrap();

        let error = encoder.encode_event(&IrEvent::MessageStop).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("before terminal message_delta"))
        );
    }
}
