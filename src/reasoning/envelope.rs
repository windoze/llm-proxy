//! Base64 envelope for carrying provider reasoning payloads without server state.

// Later M4 tasks wire the core envelope into Responses items and Anthropic signatures.
#![allow(dead_code)]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{ProxyError, Result},
    ir::message::Provider,
};

const ENVELOPE_VERSION: u8 = 1;
const CHECKSUM_DOMAIN: &[u8] = b"llm-proxy.reasoning-envelope";

/// Serialized reasoning block plus the provider that can interpret it later.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceBlock {
    pub source: Provider,
    pub payload: Vec<u8>,
}

impl SourceBlock {
    /// Builds a source block from already-serialized provider payload bytes.
    pub fn new(source: Provider, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            source,
            payload: payload.into(),
        }
    }

    /// Serializes a JSON provider block into byte-preserving envelope payload form.
    pub fn from_json(source: Provider, block: &Value) -> Result<Self> {
        Ok(Self::new(source, serde_json::to_vec(block)?))
    }

    /// Parses the wrapped payload as JSON for callers that need structured blocks.
    pub fn payload_json(&self) -> Result<Value> {
        serde_json::from_slice(&self.payload).map_err(Into::into)
    }
}

/// Wire envelope that protects opaque reasoning payloads during client round trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u8,
    pub source: Provider,
    pub payload: Vec<u8>,
    pub checksum: u32,
}

impl Envelope {
    /// Creates a versioned envelope and checksum for a provider source block.
    pub fn new(source_block: &SourceBlock) -> Self {
        let checksum = checksum(
            ENVELOPE_VERSION,
            &source_block.source,
            &source_block.payload,
        );

        Self {
            version: ENVELOPE_VERSION,
            source: source_block.source.clone(),
            payload: source_block.payload.clone(),
            checksum,
        }
    }

    /// Verifies integrity and converts the envelope back to its raw source block.
    pub fn into_source_block(self) -> Result<SourceBlock> {
        let expected = checksum(self.version, &self.source, &self.payload);
        if self.checksum != expected {
            return Err(mapping_error("reasoning envelope checksum mismatch"));
        }

        if self.version != ENVELOPE_VERSION {
            return Err(mapping_error(format!(
                "unsupported reasoning envelope version {}",
                self.version
            )));
        }

        Ok(SourceBlock::new(self.source, self.payload))
    }
}

/// Encodes a provider source block into a base64 JSON envelope.
pub fn wrap(source_block: &SourceBlock) -> Result<String> {
    let envelope = Envelope::new(source_block);
    let bytes = serde_json::to_vec(&envelope)?;

    Ok(STANDARD.encode(bytes))
}

/// Decodes and verifies a base64 envelope, returning the original provider payload bytes.
pub fn unwrap(encoded: &str) -> Result<SourceBlock> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|err| mapping_error(format!("reasoning envelope is not valid base64: {err}")))?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|err| mapping_error(format!("reasoning envelope is not valid JSON: {err}")))?;

    envelope.into_source_block()
}

fn checksum(version: u8, source: &Provider, payload: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(CHECKSUM_DOMAIN);
    hasher.update(&[version]);
    hasher.update(source_tag(source).as_bytes());
    hasher.update(&(payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    hasher.finalize()
}

fn source_tag(source: &Provider) -> &'static str {
    match source {
        Provider::Anthropic => "anthropic",
        Provider::Responses => "responses",
        Provider::OpenAiChat => "openai_chat",
        Provider::DeepSeek => "deepseek",
    }
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wraps_and_unwraps_responses_reasoning_item_json() {
        let source = SourceBlock::from_json(
            Provider::Responses,
            &json!({
                "id": "rs_resp_123",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Need a tool."}],
                "encrypted_content": "enc_payload",
                "status": "completed"
            }),
        )
        .unwrap();

        let encoded = wrap(&source).unwrap();
        let decoded = unwrap(&encoded).unwrap();

        assert_eq!(decoded, source);
        assert_eq!(
            decoded.payload_json().unwrap()["encrypted_content"],
            "enc_payload"
        );
    }

    #[test]
    fn preserves_anthropic_thinking_block_payload_bytes() {
        let payload = br#"{"type":"thinking","thinking":"Check weather.","signature":"sig_123"}"#;
        let source = SourceBlock::new(Provider::Anthropic, payload.to_vec());

        let encoded = wrap(&source).unwrap();
        let decoded = unwrap(&encoded).unwrap();

        assert_eq!(decoded.source, Provider::Anthropic);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn checksum_detects_payload_tampering() {
        let source = SourceBlock::new(
            Provider::Responses,
            br#"{"type":"reasoning","encrypted_content":"enc"}"#.to_vec(),
        );
        let encoded = wrap(&source).unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        envelope.payload[0] ^= 0xff;
        let tampered = STANDARD.encode(serde_json::to_vec(&envelope).unwrap());

        let err = unwrap(&tampered).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("checksum"))
        );
    }

    #[test]
    fn checksum_detects_source_tampering() {
        let source = SourceBlock::new(
            Provider::Responses,
            br#"{"type":"reasoning","encrypted_content":"enc"}"#.to_vec(),
        );
        let encoded = wrap(&source).unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        envelope.source = Provider::Anthropic;
        let tampered = STANDARD.encode(serde_json::to_vec(&envelope).unwrap());

        let err = unwrap(&tampered).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("checksum"))
        );
    }

    #[test]
    fn rejects_non_base64_input() {
        let err = unwrap("not base64!!!").unwrap_err();

        assert!(matches!(err, ProxyError::ProtocolMapping(message) if message.contains("base64")));
    }

    #[test]
    fn rejects_valid_checksum_with_unsupported_version() {
        let source = SourceBlock::new(
            Provider::Anthropic,
            br#"{"type":"thinking","thinking":"x","signature":"sig"}"#.to_vec(),
        );
        let mut envelope = Envelope::new(&source);
        envelope.version = ENVELOPE_VERSION + 1;
        envelope.checksum = checksum(envelope.version, &envelope.source, &envelope.payload);
        let encoded = STANDARD.encode(serde_json::to_vec(&envelope).unwrap());

        let err = unwrap(&encoded).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("unsupported"))
        );
    }
}
