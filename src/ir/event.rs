//! Streaming event IR definitions.

// Later streaming tasks wire these staged IR events into protocol decoders and encoders.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::{
    message::Provider,
    request::{StopReason, Usage},
};

/// Provider-neutral streaming event emitted by decoders and consumed by encoders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrEvent {
    MessageStart {
        id: String,
        model: String,
    },
    BlockStart {
        index: usize,
        block: BlockKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    ThinkingMetadata {
        index: usize,
        source: Provider,
        opaque: Vec<u8>,
    },
    ToolUseDelta {
        index: usize,
        partial_json: String,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<StopReason>,
        usage: Option<Usage>,
    },
    MessageStop,
}

/// Kind of content block opened by a streaming `BlockStart` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn block_start_serializes_expected_shape() {
        let event = IrEvent::BlockStart {
            index: 2,
            block: BlockKind::ToolUse {
                id: "call_123".to_owned(),
                name: "lookup".to_owned(),
            },
        };

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "block_start": {
                    "index": 2,
                    "block": {
                        "tool_use": {
                            "id": "call_123",
                            "name": "lookup"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn message_delta_preserves_stop_reason_and_usage() {
        let event = IrEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(Usage {
                input_tokens: 11,
                output_tokens: 5,
                cache_read: Some(3),
                cache_write: None,
            }),
        };

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "message_delta": {
                    "stop_reason": "tool_use",
                    "usage": {
                        "input_tokens": 11,
                        "output_tokens": 5,
                        "cache_read": 3,
                        "cache_write": null
                    }
                }
            })
        );
    }

    #[test]
    fn thinking_metadata_preserves_source_and_opaque_bytes() {
        let event = IrEvent::ThinkingMetadata {
            index: 0,
            source: Provider::Responses,
            opaque: b"encrypted_content".to_vec(),
        };

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "thinking_metadata": {
                    "index": 0,
                    "source": "responses",
                    "opaque": [
                        101, 110, 99, 114, 121, 112, 116, 101, 100, 95,
                        99, 111, 110, 116, 101, 110, 116
                    ]
                }
            })
        );
    }
}
