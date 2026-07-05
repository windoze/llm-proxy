//! Protocol-neutral intermediate representation used by all adapters.
//!
//! Message, request, and streaming event types are split into focused submodules.

pub mod event;
pub mod message;
pub mod request;
