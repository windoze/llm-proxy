//! OpenAI Responses stream decoding into provider-neutral IR events.

// M5 wires this decoder into the rich Responses → Anthropic streaming bridge.
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

const PROTOCOL: &str = "responses";
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
const RESPONSE_INCOMPLETE: &str = "response.incomplete";
const RESPONSE_FAILED: &str = "response.failed";
const DONE_MARKER: &str = "[DONE]";

/// Boxed stream of decoded IR events using the proxy's shared error type.
pub type IrEventStream = BoxStream<'static, Result<IrEvent>>;

/// Converts normalized OpenAI Responses SSE events into provider-neutral streaming IR events.
pub fn responses_sse_to_ir_events<S>(events: S) -> IrEventStream
where
    S: Stream<Item = Result<SseEvent>> + Send + 'static,
{
    async_stream::try_stream! {
        let mut decoder = ResponsesStreamDecoder::new();
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

/// Stateful decoder for OpenAI Responses API stream events.
#[derive(Debug, Default)]
pub struct ResponsesStreamDecoder {
    message: Option<MessageState>,
    next_block_index: usize,
    open_blocks: BTreeMap<usize, BlockState>,
    saw_tool_call: bool,
    saw_terminal_response: bool,
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
    Reasoning(ReasoningBlockState),
    ToolUse(ToolUseBlockState),
}

#[derive(Debug)]
struct TextBlockState {
    item_id: String,
    text: String,
}

#[derive(Debug)]
struct ReasoningBlockState {
    item_id: String,
    text: String,
    encrypted_content: Option<Vec<u8>>,
    output_done: bool,
}

#[derive(Debug)]
struct ToolUseBlockState {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl ResponsesStreamDecoder {
    /// Creates an empty decoder ready to consume the first Responses stream event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes one normalized OpenAI Responses SSE event.
    pub fn decode_sse_event(&mut self, event: &SseEvent) -> Result<Vec<IrEvent>> {
        if event.data.trim() == DONE_MARKER {
            return Ok(Vec::new());
        }

        let data = serde_json::from_str(&event.data)?;
        let event_type = normalized_event_type(event, &data)?;
        self.decode_event(event_type, &data)
    }

    /// Decodes one OpenAI Responses stream JSON event payload.
    pub fn decode_event(&mut self, event_type: &str, data: &Value) -> Result<Vec<IrEvent>> {
        let event = data
            .as_object()
            .ok_or_else(|| mapping_error("stream event data must be a JSON object"))?;

        match event_type {
            RESPONSE_CREATED | RESPONSE_IN_PROGRESS => self.decode_response_lifecycle(event),
            RESPONSE_OUTPUT_ITEM_ADDED => self.decode_output_item_added(event),
            RESPONSE_CONTENT_PART_ADDED => {
                self.validate_content_part_event(event, "response.content_part.added")?;
                Ok(Vec::new())
            }
            RESPONSE_OUTPUT_TEXT_DELTA => self.decode_text_delta(event),
            RESPONSE_OUTPUT_TEXT_DONE => {
                self.validate_text_done(event)?;
                Ok(Vec::new())
            }
            RESPONSE_REASONING_TEXT_DELTA => self.decode_reasoning_delta(event),
            RESPONSE_REASONING_TEXT_DONE => {
                self.validate_reasoning_done(event)?;
                Ok(Vec::new())
            }
            RESPONSE_CONTENT_PART_DONE => {
                self.validate_content_part_event(event, "response.content_part.done")?;
                Ok(Vec::new())
            }
            RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA => self.decode_tool_arguments_delta(event),
            RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE => {
                self.validate_tool_arguments_done(event)?;
                Ok(Vec::new())
            }
            RESPONSE_OUTPUT_ITEM_DONE => self.decode_output_item_done(event),
            RESPONSE_COMPLETED | RESPONSE_INCOMPLETE | RESPONSE_FAILED => {
                self.decode_terminal_response(event)
            }
            other => Err(ProxyError::UnsupportedFeature {
                feature: format!("Responses stream event `{other}`"),
                protocol: PROTOCOL.to_owned(),
            }),
        }
    }

    /// Finishes the decoder after the upstream SSE stream ends.
    pub fn finish(&mut self) -> Result<Vec<IrEvent>> {
        if self.message.is_none() {
            return Err(mapping_error(
                "stream ended before response.created or response.in_progress",
            ));
        }
        if !self.saw_terminal_response {
            return Err(mapping_error("stream ended before response.completed"));
        }
        if self.emitted_message_stop {
            return Ok(Vec::new());
        }

        self.emitted_message_stop = true;
        Ok(vec![IrEvent::MessageStop])
    }

    fn decode_response_lifecycle(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        let response = required_object(event, "response", "event.response")?;
        self.ensure_message_started(response)
    }

    fn decode_output_item_added(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("response.output_item.added")?;
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        if output_index != self.next_block_index {
            return Err(mapping_error(format!(
                "response.output_item.added output_index {output_index} does not match expected index {}",
                self.next_block_index
            )));
        }

        let item = required_object(event, "item", "event.item")?;
        let item_id = required_string(item, "id", "event.item.id")?.to_owned();
        let item_type = required_string(item, "type", "event.item.type")?;
        let (block, state) = match item_type {
            "message" => {
                validate_optional_assistant_role(item, "event.item.role")?;
                (
                    BlockKind::Text,
                    BlockState::Text(TextBlockState {
                        item_id,
                        text: String::new(),
                    }),
                )
            }
            "reasoning" => (
                BlockKind::Thinking,
                BlockState::Reasoning(ReasoningBlockState {
                    item_id,
                    text: String::new(),
                    encrypted_content: optional_string(
                        item,
                        "encrypted_content",
                        "event.item.encrypted_content",
                    )?
                    .map(|value| value.as_bytes().to_vec()),
                    output_done: false,
                }),
            ),
            "function_call" => {
                let call_id = required_string(item, "call_id", "event.item.call_id")?.to_owned();
                let name = required_string(item, "name", "event.item.name")?.to_owned();
                self.saw_tool_call = true;
                (
                    BlockKind::ToolUse {
                        id: call_id.clone(),
                        name: name.clone(),
                    },
                    BlockState::ToolUse(ToolUseBlockState {
                        item_id,
                        call_id,
                        name,
                        arguments: String::new(),
                    }),
                )
            }
            other => {
                return Err(ProxyError::UnsupportedFeature {
                    feature: format!("Responses output item type `{other}`"),
                    protocol: PROTOCOL.to_owned(),
                });
            }
        };

        if self.open_blocks.insert(output_index, state).is_some() {
            return Err(mapping_error(format!(
                "response.output_item.added repeated output_index {output_index}"
            )));
        }
        self.next_block_index += 1;

        Ok(vec![IrEvent::BlockStart {
            index: output_index,
            block,
        }])
    }

    fn decode_text_delta(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("response.output_text.delta")?;
        let output_index =
            self.validate_content_delta_header(event, "response.output_text.delta")?;
        let delta = required_string(event, "delta", "event.delta")?;
        if delta.is_empty() {
            return Ok(Vec::new());
        }

        let state = self.text_block_mut(output_index, "response.output_text.delta")?;
        state.text.push_str(delta);
        Ok(vec![IrEvent::TextDelta {
            index: output_index,
            text: delta.to_owned(),
        }])
    }

    fn decode_reasoning_delta(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("response.reasoning_text.delta")?;
        let output_index =
            self.validate_content_delta_header(event, "response.reasoning_text.delta")?;
        let delta = required_string(event, "delta", "event.delta")?;
        if delta.is_empty() {
            return Ok(Vec::new());
        }

        let state = self.reasoning_block_mut(output_index, "response.reasoning_text.delta")?;
        state.text.push_str(delta);
        Ok(vec![IrEvent::ThinkingDelta {
            index: output_index,
            text: delta.to_owned(),
        }])
    }

    fn decode_tool_arguments_delta(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("response.function_call_arguments.delta")?;
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        let item_id = required_string(event, "item_id", "event.item_id")?;
        let delta = required_string(event, "delta", "event.delta")?;
        if delta.is_empty() {
            return Ok(Vec::new());
        }

        let state = self.tool_block_mut(output_index, "response.function_call_arguments.delta")?;
        validate_item_id(&state.item_id, item_id, "event.item_id")?;
        state.arguments.push_str(delta);
        Ok(vec![IrEvent::ToolUseDelta {
            index: output_index,
            partial_json: delta.to_owned(),
        }])
    }

    fn decode_output_item_done(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("response.output_item.done")?;
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        let item = required_object(event, "item", "event.item")?;
        let state = self.open_blocks.remove(&output_index).ok_or_else(|| {
            mapping_error(format!(
                "response.output_item.done received for unopened output_index {output_index}"
            ))
        })?;

        match state {
            BlockState::Text(state) => {
                validate_text_item(&state, item, "event.item")?;
                Ok(vec![IrEvent::BlockStop {
                    index: output_index,
                }])
            }
            BlockState::ToolUse(state) => {
                validate_tool_item(&state, item, "event.item")?;
                Ok(vec![IrEvent::BlockStop {
                    index: output_index,
                }])
            }
            BlockState::Reasoning(mut state) => {
                if state.output_done {
                    return Err(mapping_error(format!(
                        "response.output_item.done repeated reasoning output_index {output_index}"
                    )));
                }
                merge_reasoning_encrypted_content(&mut state, item, "event.item")?;
                validate_reasoning_item(&state, item, "event.item")?;
                state.output_done = true;

                if let Some(opaque) = state.encrypted_content.take() {
                    Ok(vec![
                        IrEvent::ThinkingMetadata {
                            index: output_index,
                            source: Provider::Responses,
                            opaque,
                        },
                        IrEvent::BlockStop {
                            index: output_index,
                        },
                    ])
                } else {
                    self.open_blocks
                        .insert(output_index, BlockState::Reasoning(state));
                    Ok(Vec::new())
                }
            }
        }
    }

    fn decode_terminal_response(&mut self, event: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        self.ensure_not_terminal("terminal response event")?;
        let response = required_object(event, "response", "event.response")?;
        let mut events = self.ensure_message_started(response)?;
        events.extend(self.close_pending_reasoning_from_response(response)?);

        if let Some((&index, _)) = self.open_blocks.iter().next() {
            return Err(mapping_error(format!(
                "response completed before output item index {index} was done"
            )));
        }

        let stop_reason = decode_response_stop_reason(response, self.saw_tool_call)?;
        let usage = decode_usage(response.get("usage"), "event.response.usage")?;
        self.saw_terminal_response = true;
        self.emitted_message_stop = true;
        events.push(IrEvent::MessageDelta {
            stop_reason: Some(stop_reason),
            usage,
        });
        events.push(IrEvent::MessageStop);
        Ok(events)
    }

    fn ensure_message_started(&mut self, response: &Map<String, Value>) -> Result<Vec<IrEvent>> {
        let id = required_string(response, "id", "event.response.id")?;
        let model = required_string(response, "model", "event.response.model")?;

        match &self.message {
            Some(message) if message.id != id || message.model != model => Err(mapping_error(
                "response stream id/model changed after message_start",
            )),
            Some(_) => Ok(Vec::new()),
            None => {
                self.message = Some(MessageState {
                    id: id.to_owned(),
                    model: model.to_owned(),
                });
                Ok(vec![IrEvent::MessageStart {
                    id: id.to_owned(),
                    model: model.to_owned(),
                }])
            }
        }
    }

    fn validate_content_part_event(
        &self,
        event: &Map<String, Value>,
        event_name: &str,
    ) -> Result<()> {
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        validate_content_index(event, "event.content_index")?;
        let item_id = required_string(event, "item_id", "event.item_id")?;
        let part = required_object(event, "part", "event.part")?;
        let part_type = required_string(part, "type", "event.part.type")?;

        match self.block(output_index, event_name)? {
            BlockState::Text(state) => {
                validate_item_id(&state.item_id, item_id, "event.item_id")?;
                validate_part_type(part_type, "output_text", "event.part.type")
            }
            BlockState::Reasoning(state) => {
                validate_item_id(&state.item_id, item_id, "event.item_id")?;
                validate_part_type(part_type, "reasoning_text", "event.part.type")
            }
            BlockState::ToolUse(_) => Err(mapping_error(format!(
                "{event_name} received for function_call output_index {output_index}"
            ))),
        }
    }

    fn validate_content_delta_header(
        &self,
        event: &Map<String, Value>,
        event_name: &str,
    ) -> Result<usize> {
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        validate_content_index(event, "event.content_index")?;
        let item_id = required_string(event, "item_id", "event.item_id")?;

        match self.block(output_index, event_name)? {
            BlockState::Text(state) => {
                validate_item_id(&state.item_id, item_id, "event.item_id")?;
            }
            BlockState::Reasoning(state) => {
                validate_item_id(&state.item_id, item_id, "event.item_id")?;
            }
            BlockState::ToolUse(_) => {
                return Err(mapping_error(format!(
                    "{event_name} received for function_call output_index {output_index}"
                )));
            }
        }

        Ok(output_index)
    }

    fn validate_text_done(&self, event: &Map<String, Value>) -> Result<()> {
        let output_index =
            self.validate_content_delta_header(event, "response.output_text.done")?;
        let text = required_string(event, "text", "event.text")?;
        let state = self.text_block(output_index, "response.output_text.done")?;
        if state.text != text {
            return Err(mapping_error(format!(
                "response.output_text.done text for output_index {output_index} does not match accumulated delta text"
            )));
        }
        Ok(())
    }

    fn validate_reasoning_done(&self, event: &Map<String, Value>) -> Result<()> {
        let output_index =
            self.validate_content_delta_header(event, "response.reasoning_text.done")?;
        let text = required_string(event, "text", "event.text")?;
        let state = self.reasoning_block(output_index, "response.reasoning_text.done")?;
        if state.text != text {
            return Err(mapping_error(format!(
                "response.reasoning_text.done text for output_index {output_index} does not match accumulated delta text"
            )));
        }
        Ok(())
    }

    fn validate_tool_arguments_done(&self, event: &Map<String, Value>) -> Result<()> {
        let output_index = required_usize(event, "output_index", "event.output_index")?;
        let item_id = required_string(event, "item_id", "event.item_id")?;
        let arguments = required_string(event, "arguments", "event.arguments")?;
        let state = self.tool_block(output_index, "response.function_call_arguments.done")?;
        validate_item_id(&state.item_id, item_id, "event.item_id")?;

        if let Some(name) = optional_string(event, "name", "event.name")?
            && state.name != name
        {
            return Err(mapping_error(format!(
                "event.name changed for function_call output_index {output_index}"
            )));
        }
        if state.arguments != arguments {
            return Err(mapping_error(format!(
                "response.function_call_arguments.done arguments for output_index {output_index} do not match accumulated delta arguments"
            )));
        }
        Ok(())
    }

    fn close_pending_reasoning_from_response(
        &mut self,
        response: &Map<String, Value>,
    ) -> Result<Vec<IrEvent>> {
        let pending_indexes = self
            .open_blocks
            .iter()
            .filter_map(|(index, state)| match state {
                BlockState::Reasoning(state) if state.output_done => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();

        if pending_indexes.is_empty() {
            return Ok(Vec::new());
        }

        let output = response
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                mapping_error(
                    "terminal response.output is required to recover reasoning encrypted_content",
                )
            })?;
        let mut events = Vec::new();

        for output_index in pending_indexes {
            let item = output
                .get(output_index)
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    mapping_error(format!(
                        "terminal response.output[{output_index}] is required to recover reasoning encrypted_content"
                    ))
                })?;
            let mut state = match self.open_blocks.remove(&output_index) {
                Some(BlockState::Reasoning(state)) => state,
                _ => {
                    return Err(mapping_error(format!(
                        "missing pending reasoning block for output_index {output_index}"
                    )));
                }
            };
            merge_reasoning_encrypted_content(
                &mut state,
                item,
                format!("event.response.output[{output_index}]"),
            )?;
            validate_reasoning_item(
                &state,
                item,
                format!("event.response.output[{output_index}]"),
            )?;
            let opaque = state.encrypted_content.take().ok_or_else(|| {
                mapping_error(format!(
                    "event.response.output[{output_index}].encrypted_content is required"
                ))
            })?;
            events.push(IrEvent::ThinkingMetadata {
                index: output_index,
                source: Provider::Responses,
                opaque,
            });
            events.push(IrEvent::BlockStop {
                index: output_index,
            });
        }

        Ok(events)
    }

    fn ensure_not_terminal(&self, event_name: &str) -> Result<()> {
        if self.saw_terminal_response {
            return Err(mapping_error(format!(
                "{event_name} received after terminal response event"
            )));
        }
        Ok(())
    }

    fn block(&self, output_index: usize, event_name: &str) -> Result<&BlockState> {
        self.open_blocks.get(&output_index).ok_or_else(|| {
            mapping_error(format!(
                "{event_name} received for unopened output_index {output_index}"
            ))
        })
    }

    fn text_block(&self, output_index: usize, event_name: &str) -> Result<&TextBlockState> {
        match self.block(output_index, event_name)? {
            BlockState::Text(state) => Ok(state),
            _ => Err(mapping_error(format!(
                "{event_name} received for non-message output_index {output_index}"
            ))),
        }
    }

    fn text_block_mut(
        &mut self,
        output_index: usize,
        event_name: &str,
    ) -> Result<&mut TextBlockState> {
        match self.open_blocks.get_mut(&output_index) {
            Some(BlockState::Text(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-message output_index {output_index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened output_index {output_index}"
            ))),
        }
    }

    fn reasoning_block(
        &self,
        output_index: usize,
        event_name: &str,
    ) -> Result<&ReasoningBlockState> {
        match self.block(output_index, event_name)? {
            BlockState::Reasoning(state) => Ok(state),
            _ => Err(mapping_error(format!(
                "{event_name} received for non-reasoning output_index {output_index}"
            ))),
        }
    }

    fn reasoning_block_mut(
        &mut self,
        output_index: usize,
        event_name: &str,
    ) -> Result<&mut ReasoningBlockState> {
        match self.open_blocks.get_mut(&output_index) {
            Some(BlockState::Reasoning(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-reasoning output_index {output_index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened output_index {output_index}"
            ))),
        }
    }

    fn tool_block(&self, output_index: usize, event_name: &str) -> Result<&ToolUseBlockState> {
        match self.block(output_index, event_name)? {
            BlockState::ToolUse(state) => Ok(state),
            _ => Err(mapping_error(format!(
                "{event_name} received for non-function_call output_index {output_index}"
            ))),
        }
    }

    fn tool_block_mut(
        &mut self,
        output_index: usize,
        event_name: &str,
    ) -> Result<&mut ToolUseBlockState> {
        match self.open_blocks.get_mut(&output_index) {
            Some(BlockState::ToolUse(state)) => Ok(state),
            Some(_) => Err(mapping_error(format!(
                "{event_name} received for non-function_call output_index {output_index}"
            ))),
            None => Err(mapping_error(format!(
                "{event_name} received for unopened output_index {output_index}"
            ))),
        }
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

fn validate_optional_assistant_role(item: &Map<String, Value>, path: &str) -> Result<()> {
    if let Some(role) = optional_string(item, "role", path)?
        && role != "assistant"
    {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("Responses output message role `{role}`"),
            protocol: PROTOCOL.to_owned(),
        });
    }
    Ok(())
}

fn validate_text_item(state: &TextBlockState, item: &Map<String, Value>, path: &str) -> Result<()> {
    validate_item_type(item, "message", format!("{path}.type"))?;
    validate_item_id(
        &state.item_id,
        required_string(item, "id", format!("{path}.id"))?,
        format!("{path}.id"),
    )?;
    validate_optional_assistant_role(item, &format!("{path}.role"))?;
    Ok(())
}

fn validate_reasoning_item(
    state: &ReasoningBlockState,
    item: &Map<String, Value>,
    path: impl Into<String>,
) -> Result<()> {
    let path = path.into();
    validate_item_type(item, "reasoning", format!("{path}.type"))?;
    validate_item_id(
        &state.item_id,
        required_string(item, "id", format!("{path}.id"))?,
        format!("{path}.id"),
    )?;
    Ok(())
}

fn validate_tool_item(
    state: &ToolUseBlockState,
    item: &Map<String, Value>,
    path: &str,
) -> Result<()> {
    validate_item_type(item, "function_call", format!("{path}.type"))?;
    validate_item_id(
        &state.item_id,
        required_string(item, "id", format!("{path}.id"))?,
        format!("{path}.id"),
    )?;

    let call_id = required_string(item, "call_id", format!("{path}.call_id"))?;
    if state.call_id != call_id {
        return Err(mapping_error(format!("{path}.call_id changed")));
    }
    let name = required_string(item, "name", format!("{path}.name"))?;
    if state.name != name {
        return Err(mapping_error(format!("{path}.name changed")));
    }
    let arguments = required_string(item, "arguments", format!("{path}.arguments"))?;
    if state.arguments != arguments {
        return Err(mapping_error(format!(
            "{path}.arguments does not match accumulated delta arguments"
        )));
    }
    Ok(())
}

fn validate_item_type(
    item: &Map<String, Value>,
    expected: &str,
    path: impl Into<String>,
) -> Result<()> {
    let path = path.into();
    let item_type = required_string(item, "type", path.clone())?;
    if item_type != expected {
        return Err(mapping_error(format!("{path} must be `{expected}`")));
    }
    Ok(())
}

fn merge_reasoning_encrypted_content(
    state: &mut ReasoningBlockState,
    item: &Map<String, Value>,
    path: impl Into<String>,
) -> Result<()> {
    let path = path.into();
    let Some(encrypted_content) = optional_string(
        item,
        "encrypted_content",
        format!("{path}.encrypted_content"),
    )?
    else {
        return Ok(());
    };
    let encrypted_content = encrypted_content.as_bytes().to_vec();

    match &state.encrypted_content {
        Some(existing) if existing != &encrypted_content => Err(mapping_error(format!(
            "{path}.encrypted_content changed for reasoning item {}",
            state.item_id
        ))),
        Some(_) => Ok(()),
        None => {
            state.encrypted_content = Some(encrypted_content);
            Ok(())
        }
    }
}

fn validate_part_type(actual: &str, expected: &str, path: &str) -> Result<()> {
    if actual != expected {
        return Err(mapping_error(format!("{path} must be `{expected}`")));
    }
    Ok(())
}

fn validate_item_id(expected: &str, actual: &str, path: impl Into<String>) -> Result<()> {
    if expected != actual {
        return Err(mapping_error(format!(
            "{} `{actual}` does not match output item id `{expected}`",
            path.into()
        )));
    }
    Ok(())
}

fn validate_content_index(event: &Map<String, Value>, path: &str) -> Result<()> {
    let content_index = required_usize(event, "content_index", path)?;
    if content_index != 0 {
        return Err(ProxyError::UnsupportedFeature {
            feature: format!("Responses streaming content_index {content_index}"),
            protocol: PROTOCOL.to_owned(),
        });
    }
    Ok(())
}

fn decode_response_stop_reason(
    response: &Map<String, Value>,
    saw_tool_call: bool,
) -> Result<StopReason> {
    let status = required_string(response, "status", "event.response.status")?;
    match status {
        "completed" => {
            if saw_tool_call {
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
            "event.response.incomplete_details must be an object when present",
        ));
    };
    let reason = optional_string(
        details,
        "reason",
        "event.response.incomplete_details.reason",
    )?
    .unwrap_or("incomplete");

    Ok(match reason {
        "max_output_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_owned()),
    })
}

fn decode_usage(value: Option<&Value>, path: &str) -> Result<Option<Usage>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Object(usage) => {
            let cache_read = usage
                .get("input_tokens_details")
                .and_then(Value::as_object)
                .map(|details| {
                    optional_u32(
                        details,
                        "cached_tokens",
                        format!("{path}.input_tokens_details.cached_tokens"),
                    )
                })
                .transpose()?
                .flatten();

            Ok(Some(Usage {
                input_tokens: required_u32(usage, "input_tokens", format!("{path}.input_tokens"))?,
                output_tokens: required_u32(
                    usage,
                    "output_tokens",
                    format!("{path}.output_tokens"),
                )?,
                cache_read,
                cache_write: None,
            }))
        }
        Value::Null => Ok(None),
        _ => Err(mapping_error(format!("{path} must be an object or null"))),
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
            usize::try_from(value)
                .map_err(|_| mapping_error(format!("{path} is too large for usize")))
        }
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!("{path} must be an unsigned integer"))),
    }
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(format!(
        "Responses stream decoding failed: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::json;

    use super::*;
    use crate::protocol::anthropic::stream::ir_events_to_anthropic_sse;

    fn decode_events(events: &[SseEvent]) -> Result<Vec<IrEvent>> {
        let mut decoder = ResponsesStreamDecoder::new();
        let mut decoded = Vec::new();

        for event in events {
            decoded.extend(decoder.decode_sse_event(event)?);
        }
        decoded.extend(decoder.finish()?);

        Ok(decoded)
    }

    fn event(event_type: &str, data: Value) -> SseEvent {
        SseEvent {
            event_type: event_type.to_owned(),
            data: data.to_string(),
        }
    }

    fn created() -> SseEvent {
        event(
            RESPONSE_CREATED,
            json!({
                "type": RESPONSE_CREATED,
                "response": {
                    "id": "resp_1",
                    "model": "gpt-5.1",
                    "status": "in_progress",
                    "output": [],
                    "usage": null
                }
            }),
        )
    }

    fn completed(output: Value, usage: Value) -> SseEvent {
        event(
            RESPONSE_COMPLETED,
            json!({
                "type": RESPONSE_COMPLETED,
                "response": {
                    "id": "resp_1",
                    "model": "gpt-5.1",
                    "status": "completed",
                    "output": output,
                    "usage": usage
                }
            }),
        )
    }

    fn text_item(item_id: &str, status: &str, text: &str) -> Value {
        json!({
            "id": item_id,
            "type": "message",
            "status": status,
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        })
    }

    fn reasoning_item(item_id: &str, status: &str, text: &str, encrypted_content: &str) -> Value {
        json!({
            "id": item_id,
            "type": "reasoning",
            "status": status,
            "summary": [],
            "content": [{
                "type": "reasoning_text",
                "text": text
            }],
            "encrypted_content": encrypted_content
        })
    }

    fn function_item(item_id: &str, status: &str, arguments: &str) -> Value {
        json!({
            "id": item_id,
            "type": "function_call",
            "status": status,
            "call_id": "call_weather",
            "name": "lookup_weather",
            "arguments": arguments
        })
    }

    #[test]
    fn decodes_reasoning_text_tool_call_and_usage() {
        let events = vec![
            created(),
            event(
                RESPONSE_IN_PROGRESS,
                json!({
                    "type": RESPONSE_IN_PROGRESS,
                    "response": {
                        "id": "resp_1",
                        "model": "gpt-5.1",
                        "status": "in_progress",
                        "output": [],
                        "usage": null
                    }
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 0,
                    "item": {
                        "id": "rs_1",
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            event(
                RESPONSE_CONTENT_PART_ADDED,
                json!({
                    "type": RESPONSE_CONTENT_PART_ADDED,
                    "item_id": "rs_1",
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "reasoning_text", "text": "" }
                }),
            ),
            event(
                RESPONSE_REASONING_TEXT_DELTA,
                json!({
                    "type": RESPONSE_REASONING_TEXT_DELTA,
                    "item_id": "rs_1",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "Think "
                }),
            ),
            event(
                RESPONSE_REASONING_TEXT_DELTA,
                json!({
                    "type": RESPONSE_REASONING_TEXT_DELTA,
                    "item_id": "rs_1",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "first."
                }),
            ),
            event(
                RESPONSE_REASONING_TEXT_DONE,
                json!({
                    "type": RESPONSE_REASONING_TEXT_DONE,
                    "item_id": "rs_1",
                    "output_index": 0,
                    "content_index": 0,
                    "text": "Think first."
                }),
            ),
            event(
                RESPONSE_CONTENT_PART_DONE,
                json!({
                    "type": RESPONSE_CONTENT_PART_DONE,
                    "item_id": "rs_1",
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "reasoning_text", "text": "Think first." }
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 0,
                    "item": reasoning_item("rs_1", "completed", "Think first.", "enc_reasoning")
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 1,
                    "item": text_item("msg_1", "in_progress", "")
                }),
            ),
            event(
                RESPONSE_CONTENT_PART_ADDED,
                json!({
                    "type": RESPONSE_CONTENT_PART_ADDED,
                    "item_id": "msg_1",
                    "output_index": 1,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": "", "annotations": [] }
                }),
            ),
            event(
                RESPONSE_OUTPUT_TEXT_DELTA,
                json!({
                    "type": RESPONSE_OUTPUT_TEXT_DELTA,
                    "item_id": "msg_1",
                    "output_index": 1,
                    "content_index": 0,
                    "delta": "Answer."
                }),
            ),
            event(
                RESPONSE_OUTPUT_TEXT_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_TEXT_DONE,
                    "item_id": "msg_1",
                    "output_index": 1,
                    "content_index": 0,
                    "text": "Answer."
                }),
            ),
            event(
                RESPONSE_CONTENT_PART_DONE,
                json!({
                    "type": RESPONSE_CONTENT_PART_DONE,
                    "item_id": "msg_1",
                    "output_index": 1,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "Answer.",
                        "annotations": []
                    }
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 1,
                    "item": text_item("msg_1", "completed", "Answer.")
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 2,
                    "item": function_item("fc_1", "in_progress", "")
                }),
            ),
            event(
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                json!({
                    "type": RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                    "item_id": "fc_1",
                    "output_index": 2,
                    "delta": "{\"city\""
                }),
            ),
            event(
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                json!({
                    "type": RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
                    "item_id": "fc_1",
                    "output_index": 2,
                    "delta": ":\"Paris\"}"
                }),
            ),
            event(
                RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE,
                json!({
                    "type": RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE,
                    "item_id": "fc_1",
                    "output_index": 2,
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 2,
                    "item": function_item("fc_1", "completed", "{\"city\":\"Paris\"}")
                }),
            ),
            completed(
                json!([
                    reasoning_item("rs_1", "completed", "Think first.", "enc_reasoning"),
                    text_item("msg_1", "completed", "Answer."),
                    function_item("fc_1", "completed", "{\"city\":\"Paris\"}")
                ]),
                json!({
                    "input_tokens": 42,
                    "input_tokens_details": { "cached_tokens": 10 },
                    "output_tokens": 9,
                    "output_tokens_details": { "reasoning_tokens": 3 },
                    "total_tokens": 51
                }),
            ),
        ];

        assert_eq!(
            decode_events(&events).unwrap(),
            vec![
                IrEvent::MessageStart {
                    id: "resp_1".to_owned(),
                    model: "gpt-5.1".to_owned(),
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
                IrEvent::ThinkingMetadata {
                    index: 0,
                    source: Provider::Responses,
                    opaque: b"enc_reasoning".to_vec(),
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
                        cache_write: None,
                    }),
                },
                IrEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn recovers_reasoning_encrypted_content_from_terminal_response() {
        let events = vec![
            created(),
            event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 0,
                    "item": {
                        "id": "rs_late",
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 0,
                    "item": {
                        "id": "rs_late",
                        "type": "reasoning",
                        "status": "completed",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            completed(
                json!([reasoning_item(
                    "rs_late",
                    "completed",
                    "",
                    "enc_from_completed"
                )]),
                Value::Null,
            ),
        ];

        let decoded = decode_events(&events).unwrap();
        assert!(decoded.iter().any(|event| {
            matches!(
                event,
                IrEvent::ThinkingMetadata {
                    index: 0,
                    source: Provider::Responses,
                    opaque
                } if opaque == b"enc_from_completed"
            )
        }));
        assert_eq!(decoded.last(), Some(&IrEvent::MessageStop));
    }

    #[tokio::test]
    async fn stream_wrapper_feeds_anthropic_sse_with_signature_delta() {
        let input = stream::iter([
            Ok(created()),
            Ok(event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 0,
                    "item": {
                        "id": "rs_stream",
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": [],
                        "content": []
                    }
                }),
            )),
            Ok(event(
                RESPONSE_REASONING_TEXT_DELTA,
                json!({
                    "type": RESPONSE_REASONING_TEXT_DELTA,
                    "item_id": "rs_stream",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "Think."
                }),
            )),
            Ok(event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 0,
                    "item": reasoning_item("rs_stream", "completed", "Think.", "enc_stream")
                }),
            )),
            Ok(completed(
                json!([reasoning_item(
                    "rs_stream",
                    "completed",
                    "Think.",
                    "enc_stream"
                )]),
                Value::Null,
            )),
        ]);

        let ir_events = responses_sse_to_ir_events(input);
        let frames = ir_events_to_anthropic_sse(ir_events)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let parsed = frames.into_iter().map(parse_sse_frame).collect::<Vec<_>>();
        assert_eq!(parsed[0].0, "message_start");
        assert_eq!(parsed[1].1["index"], json!(0));
        assert_eq!(parsed[2].1["delta"]["type"], "thinking_delta");
        assert_eq!(parsed[3].1["delta"]["type"], "signature_delta");

        let signature = parsed[3].1["delta"]["signature"].as_str().unwrap();
        let source_block = crate::reasoning::envelope::unwrap_from_signature(signature).unwrap();
        assert_eq!(source_block.source, Provider::Responses);
        assert_eq!(source_block.payload, b"enc_stream".to_vec());
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
    fn rejects_non_sequential_output_indexes() {
        let mut decoder = ResponsesStreamDecoder::new();
        decoder.decode_sse_event(&created()).unwrap();

        let error = decoder
            .decode_sse_event(&event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 1,
                    "item": text_item("msg_gap", "in_progress", "")
                }),
            ))
            .unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("expected index 0"))
        );
    }

    #[test]
    fn rejects_reasoning_done_without_encrypted_content() {
        let events = vec![
            created(),
            event(
                RESPONSE_OUTPUT_ITEM_ADDED,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_ADDED,
                    "output_index": 0,
                    "item": {
                        "id": "rs_missing",
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            event(
                RESPONSE_OUTPUT_ITEM_DONE,
                json!({
                    "type": RESPONSE_OUTPUT_ITEM_DONE,
                    "output_index": 0,
                    "item": {
                        "id": "rs_missing",
                        "type": "reasoning",
                        "status": "completed",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            completed(json!([]), Value::Null),
        ];

        let error = decode_events(&events).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("response.output[0]"))
        );
    }
}
