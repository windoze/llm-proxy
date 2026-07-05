//! Streaming encoder for Anthropic Messages API SSE responses.

// Later M2 tasks wire this staged encoder into HTTP routing.
#![allow(dead_code)]

use std::collections::BTreeSet;

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::{Map, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::{
        event::{BlockKind, IrEvent},
        request::{StopReason, Usage},
    },
};

const PROTOCOL: &str = "anthropic";
const MESSAGE_START: &str = "message_start";
const CONTENT_BLOCK_START: &str = "content_block_start";
const CONTENT_BLOCK_DELTA: &str = "content_block_delta";
const CONTENT_BLOCK_STOP: &str = "content_block_stop";
const MESSAGE_DELTA: &str = "message_delta";
const MESSAGE_STOP: &str = "message_stop";

/// Boxed byte stream containing Anthropic-compatible SSE frames.
pub type AnthropicSseStream = BoxStream<'static, Result<Bytes>>;

/// Converts provider-neutral IR events into Anthropic Messages API SSE frames.
pub fn ir_events_to_anthropic_sse<S>(events: S) -> AnthropicSseStream
where
    S: Stream<Item = Result<IrEvent>> + Send + 'static,
{
    async_stream::try_stream! {
        let mut encoder = AnthropicStreamEncoder::new();
        futures_util::pin_mut!(events);

        while let Some(event) = events.next().await {
            let event = event?;
            yield encoder.encode_event(&event)?;
        }
    }
    .boxed()
}

/// Stateful encoder that validates IR stream ordering while emitting SSE frames.
#[derive(Debug, Default)]
pub struct AnthropicStreamEncoder {
    message_started: bool,
    message_stopped: bool,
    next_block_index: usize,
    open_blocks: BTreeSet<usize>,
    emitted_stop_reason: bool,
}

impl AnthropicStreamEncoder {
    /// Creates an encoder ready to consume the first IR stream event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Encodes one IR event into a complete `event:`/`data:` SSE frame.
    pub fn encode_event(&mut self, event: &IrEvent) -> Result<Bytes> {
        self.ensure_not_stopped()?;

        let (event_type, data) = match event {
            IrEvent::MessageStart { id, model } => self.encode_message_start(id, model)?,
            IrEvent::BlockStart { index, block } => self.encode_block_start(*index, block)?,
            IrEvent::TextDelta { index, text } => self.encode_delta(
                *index,
                json!({
                    "type": "text_delta",
                    "text": text,
                }),
            )?,
            IrEvent::ThinkingDelta { index, text } => self.encode_delta(
                *index,
                json!({
                    "type": "thinking_delta",
                    "thinking": text,
                }),
            )?,
            IrEvent::ToolUseDelta {
                index,
                partial_json,
            } => self.encode_delta(
                *index,
                json!({
                    "type": "input_json_delta",
                    "partial_json": partial_json,
                }),
            )?,
            IrEvent::BlockStop { index } => self.encode_block_stop(*index)?,
            IrEvent::MessageDelta { stop_reason, usage } => {
                self.encode_message_delta(stop_reason.as_ref(), usage.as_ref())?
            }
            IrEvent::MessageStop => self.encode_message_stop()?,
        };

        format_sse_frame(event_type, &data)
    }

    fn encode_message_start(&mut self, id: &str, model: &str) -> Result<(&'static str, Value)> {
        if self.message_started {
            return Err(mapping_error("message_start received more than once"));
        }

        self.message_started = true;
        Ok((
            MESSAGE_START,
            json!({
                "type": MESSAGE_START,
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                    },
                },
            }),
        ))
    }

    fn encode_block_start(
        &mut self,
        index: usize,
        block: &BlockKind,
    ) -> Result<(&'static str, Value)> {
        self.ensure_message_started("content_block_start")?;
        if self.emitted_stop_reason {
            return Err(mapping_error(
                "content_block_start received after terminal message_delta",
            ));
        }
        if index != self.next_block_index {
            return Err(mapping_error(format!(
                "content_block_start index {index} does not match expected index {}",
                self.next_block_index
            )));
        }

        self.next_block_index += 1;
        self.open_blocks.insert(index);
        Ok((
            CONTENT_BLOCK_START,
            json!({
                "type": CONTENT_BLOCK_START,
                "index": index,
                "content_block": encode_content_block_start(block),
            }),
        ))
    }

    fn encode_delta(&self, index: usize, delta: Value) -> Result<(&'static str, Value)> {
        self.ensure_message_started("content_block_delta")?;
        self.ensure_block_open(index, "content_block_delta")?;

        Ok((
            CONTENT_BLOCK_DELTA,
            json!({
                "type": CONTENT_BLOCK_DELTA,
                "index": index,
                "delta": delta,
            }),
        ))
    }

    fn encode_block_stop(&mut self, index: usize) -> Result<(&'static str, Value)> {
        self.ensure_message_started("content_block_stop")?;
        if !self.open_blocks.remove(&index) {
            return Err(mapping_error(format!(
                "content_block_stop received for unopened index {index}"
            )));
        }

        Ok((
            CONTENT_BLOCK_STOP,
            json!({
                "type": CONTENT_BLOCK_STOP,
                "index": index,
            }),
        ))
    }

    fn encode_message_delta(
        &mut self,
        stop_reason: Option<&StopReason>,
        usage: Option<&Usage>,
    ) -> Result<(&'static str, Value)> {
        self.ensure_message_started("message_delta")?;
        if stop_reason.is_none() && usage.is_none() {
            return Err(mapping_error(
                "message_delta must include stop_reason or usage",
            ));
        }
        if let Some(stop_reason) = stop_reason {
            if self.emitted_stop_reason {
                return Err(mapping_error(
                    "terminal message_delta received more than once",
                ));
            }
            if !self.open_blocks.is_empty() {
                return Err(mapping_error(
                    "terminal message_delta received before all content blocks stopped",
                ));
            }
            self.emitted_stop_reason = true;

            Ok((
                MESSAGE_DELTA,
                json!({
                    "type": MESSAGE_DELTA,
                    "delta": {
                        "stop_reason": encode_stop_reason(stop_reason),
                        "stop_sequence": null,
                    },
                    "usage": usage.map(encode_streaming_usage),
                }),
            ))
        } else {
            Ok((
                MESSAGE_DELTA,
                json!({
                    "type": MESSAGE_DELTA,
                    "delta": {},
                    "usage": usage.map(encode_streaming_usage),
                }),
            ))
        }
    }

    fn encode_message_stop(&mut self) -> Result<(&'static str, Value)> {
        self.ensure_message_started("message_stop")?;
        if !self.open_blocks.is_empty() {
            return Err(mapping_error(
                "message_stop received before all content blocks stopped",
            ));
        }

        self.message_stopped = true;
        Ok((
            MESSAGE_STOP,
            json!({
                "type": MESSAGE_STOP,
            }),
        ))
    }

    fn ensure_message_started(&self, event_type: &str) -> Result<()> {
        if !self.message_started {
            return Err(mapping_error(format!(
                "{event_type} received before message_start"
            )));
        }
        Ok(())
    }

    fn ensure_not_stopped(&self) -> Result<()> {
        if self.message_stopped {
            return Err(mapping_error("event received after message_stop"));
        }
        Ok(())
    }

    fn ensure_block_open(&self, index: usize, event_type: &str) -> Result<()> {
        if !self.open_blocks.contains(&index) {
            return Err(mapping_error(format!(
                "{event_type} received for unopened index {index}"
            )));
        }
        Ok(())
    }
}

fn encode_content_block_start(block: &BlockKind) -> Value {
    match block {
        BlockKind::Text => json!({
            "type": "text",
            "text": "",
        }),
        BlockKind::Thinking => json!({
            "type": "thinking",
            "thinking": "",
            "signature": "",
        }),
        BlockKind::ToolUse { id, name } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": {},
        }),
    }
}

fn encode_stop_reason(stop_reason: &StopReason) -> &str {
    match stop_reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        StopReason::Other(reason) => reason,
    }
}

fn encode_streaming_usage(usage: &Usage) -> Value {
    let mut value = Map::new();
    value.insert("input_tokens".to_owned(), json!(usage.input_tokens));
    value.insert("output_tokens".to_owned(), json!(usage.output_tokens));

    if let Some(cache_read) = usage.cache_read {
        value.insert("cache_read_input_tokens".to_owned(), json!(cache_read));
    }
    if let Some(cache_write) = usage.cache_write {
        value.insert("cache_creation_input_tokens".to_owned(), json!(cache_write));
    }

    Value::Object(value)
}

fn format_sse_frame(event_type: &str, data: &Value) -> Result<Bytes> {
    let data = serde_json::to_string(data)?;
    Ok(Bytes::from(format!(
        "event: {event_type}\ndata: {data}\n\n"
    )))
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(format!(
        "Anthropic stream encoding failed: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    fn encode_events(events: &[IrEvent]) -> Result<Vec<(String, Value)>> {
        let mut encoder = AnthropicStreamEncoder::new();
        events
            .iter()
            .map(|event| encoder.encode_event(event).map(parse_sse_frame))
            .collect()
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
    fn encodes_message_and_content_block_lifecycle() {
        let events = vec![
            IrEvent::MessageStart {
                id: "msg_1".to_owned(),
                model: "deepseek-reasoner".to_owned(),
            },
            IrEvent::BlockStart {
                index: 0,
                block: BlockKind::Thinking,
            },
            IrEvent::ThinkingDelta {
                index: 0,
                text: "Think first.".to_owned(),
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
                partial_json: "{\"city\":\"Paris\"}".to_owned(),
            },
            IrEvent::BlockStop { index: 2 },
            IrEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: Some(Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                    cache_read: Some(4),
                    cache_write: Some(8),
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
                MESSAGE_START,
                CONTENT_BLOCK_START,
                CONTENT_BLOCK_DELTA,
                CONTENT_BLOCK_STOP,
                CONTENT_BLOCK_START,
                CONTENT_BLOCK_DELTA,
                CONTENT_BLOCK_STOP,
                CONTENT_BLOCK_START,
                CONTENT_BLOCK_DELTA,
                CONTENT_BLOCK_STOP,
                MESSAGE_DELTA,
                MESSAGE_STOP,
            ]
        );
        assert_eq!(
            encoded[0].1,
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "model": "deepseek-reasoner",
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            })
        );
        assert_eq!(
            encoded[1].1,
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "thinking",
                    "thinking": "",
                    "signature": ""
                }
            })
        );
        assert_eq!(
            encoded[2].1,
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": "Think first."
                }
            })
        );
        assert_eq!(
            encoded[7].1,
            json!({
                "type": "content_block_start",
                "index": 2,
                "content_block": {
                    "type": "tool_use",
                    "id": "call_weather",
                    "name": "lookup_weather",
                    "input": {}
                }
            })
        );
        assert_eq!(
            encoded[8].1,
            json!({
                "type": "content_block_delta",
                "index": 2,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": "{\"city\":\"Paris\"}"
                }
            })
        );
        assert_eq!(
            encoded[10].1,
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "tool_use",
                    "stop_sequence": null
                },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 7,
                    "cache_read_input_tokens": 4,
                    "cache_creation_input_tokens": 8
                }
            })
        );
        assert_eq!(encoded[11].1, json!({ "type": "message_stop" }));
    }

    #[tokio::test]
    async fn stream_wrapper_formats_sse_frames() {
        let input = stream::iter([
            Ok(IrEvent::MessageStart {
                id: "msg_stream".to_owned(),
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

        let frames = ir_events_to_anthropic_sse(input)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let first = std::str::from_utf8(&frames[0]).unwrap();
        assert!(first.starts_with("event: message_start\ndata: {"));
        assert!(first.ends_with("\n\n"));
        assert_eq!(parse_sse_frame(frames[2].clone()).0, CONTENT_BLOCK_DELTA);
        assert_eq!(
            parse_sse_frame(frames[4].clone()).1,
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "end_turn",
                    "stop_sequence": null
                },
                "usage": null
            })
        );
    }

    #[test]
    fn rejects_non_sequential_block_indexes() {
        let mut encoder = AnthropicStreamEncoder::new();
        encoder
            .encode_event(&IrEvent::MessageStart {
                id: "msg_1".to_owned(),
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
    fn rejects_deltas_for_unopened_blocks() {
        let mut encoder = AnthropicStreamEncoder::new();
        encoder
            .encode_event(&IrEvent::MessageStart {
                id: "msg_1".to_owned(),
                model: "model".to_owned(),
            })
            .unwrap();

        let error = encoder
            .encode_event(&IrEvent::TextDelta {
                index: 0,
                text: "orphan".to_owned(),
            })
            .unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("unopened index 0"))
        );
    }
}
