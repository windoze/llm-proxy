//! Anthropic Messages API protocol adapter.
//!
//! Request decoding, response encoding, and streaming response encoding are
//! added incrementally across the M2 milestone.

pub mod decode;
pub mod encode;
pub mod stream;
