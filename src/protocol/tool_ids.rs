//! Tool-call ID mapping and pairing checks shared by protocol adapters.
//!
//! Chat backends expose tool calls as `tool_call_id`; Anthropic clients see the
//! same edge as `tool_use_id`, and Responses clients see it as `call_id`. The
//! proxy is stateless, so this module records a request-local bidirectional map
//! and verifies that every returned tool result points back to the prior
//! assistant tool call it answers.

// M2-07 wires this staged helper into the Chat request encoder.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use crate::{
    error::{ProxyError, Result},
    ir::{
        message::{ContentBlock, Message, Role},
        request::IrRequest,
    },
};

/// One lossless edge between a Chat tool-call ID and the client-visible tool ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolIdPair {
    pub chat_tool_call_id: String,
    pub client_tool_call_id: String,
}

/// Bidirectional request-local map for Chat and client protocol tool IDs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolIdMap {
    chat_to_client: HashMap<String, String>,
    client_to_chat: HashMap<String, String>,
}

impl ToolIdMap {
    /// Creates an empty ID map for one decoded request or response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an explicit Chat `tool_call_id` ↔ client protocol tool ID mapping.
    pub fn insert_pair(
        &mut self,
        chat_tool_call_id: impl Into<String>,
        client_tool_call_id: impl Into<String>,
    ) -> Result<()> {
        let chat_tool_call_id = chat_tool_call_id.into();
        let client_tool_call_id = client_tool_call_id.into();
        reject_empty_id(&chat_tool_call_id, "chat tool_call_id")?;
        reject_empty_id(&client_tool_call_id, "client tool id")?;
        self.ensure_chat_id_available(&chat_tool_call_id, &client_tool_call_id)?;
        self.ensure_client_id_available(&client_tool_call_id, &chat_tool_call_id)?;

        self.chat_to_client
            .insert(chat_tool_call_id.clone(), client_tool_call_id.clone());
        self.client_to_chat
            .insert(client_tool_call_id, chat_tool_call_id);
        Ok(())
    }

    /// Records the stateless identity mapping used for Chat ↔ client-protocol turns.
    pub fn insert_identity(&mut self, id: impl Into<String>) -> Result<()> {
        let id = id.into();
        self.insert_pair(id.clone(), id)
    }

    /// Returns the Anthropic `tool_use_id` that should expose a Chat tool call.
    pub fn anthropic_tool_use_id(&self, chat_tool_call_id: &str) -> Result<&str> {
        self.client_tool_id(chat_tool_call_id, "Anthropic tool_use_id")
    }

    /// Returns the Chat `tool_call_id` answered by an Anthropic tool result.
    pub fn chat_tool_call_id(&self, anthropic_tool_use_id: &str) -> Result<&str> {
        self.chat_id_for_client_tool_id(anthropic_tool_use_id, "Anthropic tool_use_id")
    }

    /// Records an explicit Chat `tool_call_id` ↔ Responses `call_id` mapping.
    pub fn insert_responses_pair(
        &mut self,
        chat_tool_call_id: impl Into<String>,
        responses_call_id: impl Into<String>,
    ) -> Result<()> {
        self.insert_pair(chat_tool_call_id, responses_call_id)
    }

    /// Returns the Responses `call_id` that should expose a Chat tool call.
    pub fn responses_call_id(&self, chat_tool_call_id: &str) -> Result<&str> {
        self.client_tool_id(chat_tool_call_id, "Responses call_id")
    }

    /// Returns the Chat `tool_call_id` answered by a Responses function output.
    pub fn chat_tool_call_id_for_responses(&self, responses_call_id: &str) -> Result<&str> {
        self.chat_id_for_client_tool_id(responses_call_id, "Responses call_id")
    }

    /// Returns all known ID pairs in deterministic Chat-ID order.
    pub fn pairs(&self) -> Vec<ToolIdPair> {
        let mut pairs = self
            .chat_to_client
            .iter()
            .map(|(chat_tool_call_id, client_tool_call_id)| ToolIdPair {
                chat_tool_call_id: chat_tool_call_id.clone(),
                client_tool_call_id: client_tool_call_id.clone(),
            })
            .collect::<Vec<_>>();
        pairs.sort_by(|left, right| left.chat_tool_call_id.cmp(&right.chat_tool_call_id));
        pairs
    }

    /// Returns true when no mappings have been recorded.
    pub fn is_empty(&self) -> bool {
        self.chat_to_client.is_empty()
    }

    fn contains_chat_tool_call_id(&self, id: &str) -> bool {
        self.chat_to_client.contains_key(id)
    }

    fn client_tool_id(&self, chat_tool_call_id: &str, client_label: &str) -> Result<&str> {
        self.chat_to_client
            .get(chat_tool_call_id)
            .map(String::as_str)
            .ok_or_else(|| {
                mapping_error(format!(
                    "missing {client_label} for Chat tool_call_id `{chat_tool_call_id}`"
                ))
            })
    }

    fn chat_id_for_client_tool_id(
        &self,
        client_tool_call_id: &str,
        client_label: &str,
    ) -> Result<&str> {
        self.client_to_chat
            .get(client_tool_call_id)
            .map(String::as_str)
            .ok_or_else(|| {
                mapping_error(format!(
                    "missing Chat tool_call_id for {client_label} `{client_tool_call_id}`"
                ))
            })
    }

    fn ensure_chat_id_available(
        &self,
        chat_tool_call_id: &str,
        client_tool_call_id: &str,
    ) -> Result<()> {
        match self.chat_to_client.get(chat_tool_call_id) {
            Some(existing) if existing != client_tool_call_id => Err(mapping_error(format!(
                "Chat tool_call_id `{chat_tool_call_id}` is already mapped to client tool id `{existing}`, not `{client_tool_call_id}`"
            ))),
            _ => Ok(()),
        }
    }

    fn ensure_client_id_available(
        &self,
        client_tool_call_id: &str,
        chat_tool_call_id: &str,
    ) -> Result<()> {
        match self.client_to_chat.get(client_tool_call_id) {
            Some(existing) if existing != chat_tool_call_id => Err(mapping_error(format!(
                "client tool id `{client_tool_call_id}` is already mapped to Chat tool_call_id `{existing}`, not `{chat_tool_call_id}`"
            ))),
            _ => Ok(()),
        }
    }
}

/// Builds and validates the complete tool ID map implied by an IR request history.
pub fn tool_id_map_from_request(request: &IrRequest) -> Result<ToolIdMap> {
    tool_id_map_from_request_with_label(request, "client tool id")
}

/// Builds and validates the Chat ↔ Responses `call_id` map implied by an IR request history.
pub fn responses_tool_id_map_from_request(request: &IrRequest) -> Result<ToolIdMap> {
    tool_id_map_from_request_with_label(request, "Responses call_id")
}

fn tool_id_map_from_request_with_label(
    request: &IrRequest,
    client_id_label: &'static str,
) -> Result<ToolIdMap> {
    let mut tracker = ToolPairingTracker::new(client_id_label);
    for (message_index, message) in request.messages.iter().enumerate() {
        tracker.record_message(message, message_index)?;
    }
    tracker.finish()
}

/// Verifies that every tool call/result pair in a request is complete and ordered.
pub fn validate_tool_result_pairs(request: &IrRequest) -> Result<()> {
    tool_id_map_from_request(request).map(|_| ())
}

/// Verifies every Responses `function_call_output.call_id` answers a prior `function_call.call_id`.
pub fn validate_responses_tool_result_pairs(request: &IrRequest) -> Result<()> {
    responses_tool_id_map_from_request(request).map(|_| ())
}

#[derive(Debug)]
struct ToolPairingTracker {
    ids: ToolIdMap,
    unresolved_chat_ids: HashSet<String>,
    resolved_chat_ids: HashSet<String>,
    client_id_label: &'static str,
}

impl Default for ToolPairingTracker {
    fn default() -> Self {
        Self::new("client tool id")
    }
}

impl ToolPairingTracker {
    fn new(client_id_label: &'static str) -> Self {
        Self {
            ids: ToolIdMap::new(),
            unresolved_chat_ids: HashSet::new(),
            resolved_chat_ids: HashSet::new(),
            client_id_label,
        }
    }

    fn record_message(&mut self, message: &Message, message_index: usize) -> Result<()> {
        for (block_index, block) in message.content.iter().enumerate() {
            let path = format!("messages[{message_index}].content[{block_index}]");
            match block {
                ContentBlock::ToolUse { id, .. } if message.role == Role::Assistant => {
                    self.record_tool_use(id, &path)?
                }
                ContentBlock::ToolUse { .. } => {
                    return Err(mapping_error(format!(
                        "{path} is a tool call but message role is not assistant"
                    )));
                }
                ContentBlock::ToolResult { tool_use_id, .. }
                    if matches!(message.role, Role::User | Role::Tool) =>
                {
                    self.record_tool_result(tool_use_id, &path)?
                }
                ContentBlock::ToolResult { .. } => {
                    return Err(mapping_error(format!(
                        "{path} is a tool result but message role is not user/tool"
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn record_tool_use(&mut self, id: &str, path: &str) -> Result<()> {
        if self.ids.contains_chat_tool_call_id(id) {
            return Err(mapping_error(format!(
                "{path}.id duplicates prior assistant tool call id `{id}`"
            )));
        }

        self.ids.insert_identity(id.to_owned())?;
        self.unresolved_chat_ids.insert(id.to_owned());
        Ok(())
    }

    fn record_tool_result(&mut self, tool_use_id: &str, path: &str) -> Result<()> {
        let chat_tool_call_id = self
            .ids
            .chat_id_for_client_tool_id(tool_use_id, self.client_id_label)?
            .to_owned();

        if !self.unresolved_chat_ids.remove(&chat_tool_call_id) {
            if self.resolved_chat_ids.contains(&chat_tool_call_id) {
                return Err(mapping_error(format!(
                    "{path}.tool_use_id duplicates the result for tool call `{chat_tool_call_id}`"
                )));
            }

            return Err(mapping_error(format!(
                "{path}.tool_use_id `{tool_use_id}` does not match an unresolved prior tool call"
            )));
        }

        self.resolved_chat_ids.insert(chat_tool_call_id);
        Ok(())
    }

    fn finish(self) -> Result<ToolIdMap> {
        if let Some(unresolved_id) = self.unresolved_chat_ids.iter().min() {
            return Err(mapping_error(format!(
                "assistant tool call `{unresolved_id}` has no matching tool result"
            )));
        }

        Ok(self.ids)
    }
}

fn reject_empty_id(id: &str, label: &str) -> Result<()> {
    if id.is_empty() {
        return Err(mapping_error(format!("{label} must not be empty")));
    }
    Ok(())
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::ir::{
        message::{ContentBlock, Message, Role},
        request::{ToolChoice, ToolDef},
    };

    #[test]
    fn maps_explicit_pairs_in_both_directions() {
        let mut ids = ToolIdMap::new();

        ids.insert_pair("call_chat", "toolu_anthropic").unwrap();

        assert_eq!(
            ids.anthropic_tool_use_id("call_chat").unwrap(),
            "toolu_anthropic"
        );
        assert_eq!(
            ids.chat_tool_call_id("toolu_anthropic").unwrap(),
            "call_chat"
        );
        assert_eq!(
            ids.pairs(),
            vec![ToolIdPair {
                chat_tool_call_id: "call_chat".to_owned(),
                client_tool_call_id: "toolu_anthropic".to_owned(),
            }]
        );
    }

    #[test]
    fn maps_responses_call_ids_in_both_directions() {
        let mut ids = ToolIdMap::new();

        ids.insert_responses_pair("chat_call_1", "resp_call_1")
            .unwrap();

        assert_eq!(ids.responses_call_id("chat_call_1").unwrap(), "resp_call_1");
        assert_eq!(
            ids.chat_tool_call_id_for_responses("resp_call_1").unwrap(),
            "chat_call_1"
        );
        assert_eq!(
            ids.pairs(),
            vec![ToolIdPair {
                chat_tool_call_id: "chat_call_1".to_owned(),
                client_tool_call_id: "resp_call_1".to_owned(),
            }]
        );
    }

    #[test]
    fn rejects_conflicting_pairs() {
        let mut ids = ToolIdMap::new();
        ids.insert_pair("call_1", "toolu_1").unwrap();

        let chat_conflict = ids.insert_pair("call_1", "toolu_2").unwrap_err();
        let anthropic_conflict = ids.insert_pair("call_2", "toolu_1").unwrap_err();

        assert!(
            matches!(chat_conflict, ProxyError::ProtocolMapping(message) if message.contains("already mapped"))
        );
        assert!(
            matches!(anthropic_conflict, ProxyError::ProtocolMapping(message) if message.contains("already mapped"))
        );
    }

    #[test]
    fn builds_identity_map_for_multi_turn_tool_pairs() {
        let request = request_with_messages(vec![
            assistant_with_tools(&["call_weather", "call_time"]),
            user_with_results(&["call_time", "call_weather"]),
            assistant_with_tools(&["call_news"]),
            user_with_results(&["call_news"]),
        ]);

        let ids = tool_id_map_from_request(&request).unwrap();

        assert_eq!(
            ids.pairs(),
            vec![
                ToolIdPair {
                    chat_tool_call_id: "call_news".to_owned(),
                    client_tool_call_id: "call_news".to_owned(),
                },
                ToolIdPair {
                    chat_tool_call_id: "call_time".to_owned(),
                    client_tool_call_id: "call_time".to_owned(),
                },
                ToolIdPair {
                    chat_tool_call_id: "call_weather".to_owned(),
                    client_tool_call_id: "call_weather".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn rejects_tool_result_without_prior_call() {
        let request = request_with_messages(vec![user_with_results(&["missing_call"])]);

        let error = tool_id_map_from_request(&request).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("missing Chat tool_call_id"))
        );
    }

    #[test]
    fn rejects_duplicate_tool_results_for_one_call() {
        let request = request_with_messages(vec![
            assistant_with_tools(&["call_weather"]),
            user_with_results(&["call_weather", "call_weather"]),
        ]);

        let error = tool_id_map_from_request(&request).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("duplicates the result"))
        );
    }

    #[test]
    fn rejects_duplicate_tool_use_ids() {
        let request = request_with_messages(vec![assistant_with_tools(&["call_1", "call_1"])]);

        let error = tool_id_map_from_request(&request).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("duplicates prior assistant tool call id"))
        );
    }

    #[test]
    fn rejects_unanswered_tool_uses() {
        let request = request_with_messages(vec![assistant_with_tools(&["call_pending"])]);

        let error = tool_id_map_from_request(&request).unwrap_err();

        assert!(
            matches!(error, ProxyError::ProtocolMapping(message) if message.contains("has no matching tool result"))
        );
    }

    #[test]
    fn rejects_tool_blocks_on_wrong_roles() {
        let user_tool_call = request_with_messages(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolUse {
                id: "call_wrong".to_owned(),
                name: "lookup".to_owned(),
                input: json!({}),
            }],
        }]);
        let assistant_tool_result = request_with_messages(vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_wrong".to_owned(),
                content: Vec::new(),
                is_error: false,
            }],
        }]);

        let user_error = tool_id_map_from_request(&user_tool_call).unwrap_err();
        let assistant_error = tool_id_map_from_request(&assistant_tool_result).unwrap_err();

        assert!(
            matches!(user_error, ProxyError::ProtocolMapping(message) if message.contains("message role is not assistant"))
        );
        assert!(
            matches!(assistant_error, ProxyError::ProtocolMapping(message) if message.contains("message role is not user/tool"))
        );
    }

    fn request_with_messages(messages: Vec<Message>) -> IrRequest {
        IrRequest {
            model: "deepseek-reasoner".to_owned(),
            system: None,
            messages,
            tools: Vec::<ToolDef>::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(128),
            temperature: None,
            top_p: None,
            top_k: None,
            stop: Vec::new(),
            stream: false,
            extra: Map::new(),
        }
    }

    fn assistant_with_tools(ids: &[&str]) -> Message {
        Message {
            role: Role::Assistant,
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolUse {
                    id: (*id).to_owned(),
                    name: "lookup".to_owned(),
                    input: json!({ "id": id }),
                })
                .collect(),
        }
    }

    fn user_with_results(ids: &[&str]) -> Message {
        Message {
            role: Role::User,
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolResult {
                    tool_use_id: (*id).to_owned(),
                    content: vec![ContentBlock::Text {
                        text: format!("result for {id}"),
                    }],
                    is_error: false,
                })
                .collect(),
        }
    }
}
