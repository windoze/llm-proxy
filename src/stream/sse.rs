//! Shared Server-Sent Events parsing helpers for upstream provider streams.

// Later streaming tasks wire this staged parser into protocol-specific state machines.
#![allow(dead_code)]

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures_util::{Stream, StreamExt, stream::BoxStream};

use crate::error::{ProxyError, Result};

const OPENAI_DONE_MARKER: &str = "[DONE]";

/// Provider-neutral SSE event data passed to downstream stream decoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

/// Boxed stream of parsed SSE events using the proxy's shared error type.
pub type SseEventStream = BoxStream<'static, Result<SseEvent>>;

/// Parses a `reqwest` byte stream into normalized `(event_type, data)` SSE events.
pub fn parse_reqwest_sse<S, B>(bytes_stream: S) -> SseEventStream
where
    S: Stream<Item = std::result::Result<B, reqwest::Error>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    bytes_stream
        .eventsource()
        .map(|event| event.map(SseEvent::from).map_err(map_sse_error))
        .boxed()
}

/// Parses OpenAI Chat-compatible SSE and treats `data: [DONE]` as normal stream termination.
pub fn parse_openai_chat_sse<S, B>(bytes_stream: S) -> SseEventStream
where
    S: Stream<Item = std::result::Result<B, reqwest::Error>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    let mut events = parse_reqwest_sse(bytes_stream);

    async_stream::try_stream! {
        while let Some(event) = events.next().await {
            let event = event?;
            if is_openai_done(&event) {
                break;
            }
            yield event;
        }
    }
    .boxed()
}

impl From<Event> for SseEvent {
    fn from(event: Event) -> Self {
        Self {
            event_type: event.event,
            data: event.data,
        }
    }
}

fn is_openai_done(event: &SseEvent) -> bool {
    event.data.trim() == OPENAI_DONE_MARKER
}

fn map_sse_error(error: EventStreamError<reqwest::Error>) -> ProxyError {
    match error {
        EventStreamError::Transport(error) => ProxyError::UpstreamHttp(error),
        other => ProxyError::ProtocolMapping(format!("failed to parse SSE event: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::stream;

    use super::*;

    #[tokio::test]
    async fn parses_reqwest_bytes_stream_into_sse_events() {
        let bytes = stream::iter([
            Ok::<_, reqwest::Error>(Bytes::from_static(
                b"event: chat.completion.chunk\ndata: {\"id\":\"chunk_1\"}\n\n",
            )),
            Ok::<_, reqwest::Error>(Bytes::from_static(b"data: line 1\n")),
            Ok::<_, reqwest::Error>(Bytes::from_static(b"data: line 2\n\n")),
        ]);

        let mut events = parse_reqwest_sse(bytes);

        assert_eq!(
            events.next().await.unwrap().unwrap(),
            SseEvent {
                event_type: "chat.completion.chunk".to_owned(),
                data: "{\"id\":\"chunk_1\"}".to_owned(),
            }
        );
        assert_eq!(
            events.next().await.unwrap().unwrap(),
            SseEvent {
                event_type: "message".to_owned(),
                data: "line 1\nline 2".to_owned(),
            }
        );
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn openai_chat_parser_stops_on_done_marker() {
        let bytes = stream::iter([
            Ok::<_, reqwest::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            )),
            Ok::<_, reqwest::Error>(Bytes::from_static(b"data: [DONE]\n\n")),
            Ok::<_, reqwest::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"ignored\"}}]}\n\n",
            )),
        ]);

        let mut events = parse_openai_chat_sse(bytes);

        assert_eq!(
            events.next().await.unwrap().unwrap(),
            SseEvent {
                event_type: "message".to_owned(),
                data: "{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}".to_owned(),
            }
        );
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn parser_errors_are_reported_as_protocol_mapping_failures() {
        let bytes = stream::iter([Ok::<_, reqwest::Error>(Bytes::from_static(&[
            0xff, b'\n', b'\n',
        ]))]);

        let mut events = parse_reqwest_sse(bytes);
        let error = events.next().await.unwrap().unwrap_err();

        assert!(matches!(error, ProxyError::ProtocolMapping(message) if message.contains("UTF8")));
    }
}
