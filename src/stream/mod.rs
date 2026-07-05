//! Streaming infrastructure shared by protocol adapters.
//!
//! SSE parsing is available in `sse`; protocol-specific state machines convert provider streams into IR events.

pub mod chat_decoder;
pub mod sse;
