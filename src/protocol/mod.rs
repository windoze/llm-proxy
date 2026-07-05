//! Protocol adapters for translating external APIs to and from IR.
//!
//! Each protocol family owns its decode, encode, and streaming logic.

pub mod anthropic;
pub mod openai_chat;
pub mod responses;
