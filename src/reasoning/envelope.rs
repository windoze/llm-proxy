//! Base64 envelope for carrying provider reasoning payloads without server state.

// Later M4 tasks wire these adapters into the rich protocol bridges.
#![allow(dead_code)]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::message::Provider,
};

const ENVELOPE_VERSION: u8 = 1;
const CHECKSUM_DOMAIN: &[u8] = b"llm-proxy.reasoning-envelope";
const ANTHROPIC_SIGNATURE_PREFIX: &str = "llm_proxy_sig_v1:";
const RESPONSES_REASONING_TYPE: &str = "reasoning";
const RESPONSES_REASONING_ID_PREFIX: &str = "rs_llm_proxy";
pub const DEFAULT_MAX_OPAQUE_FIELD_BYTES: usize = 512 * 1024;

/// Maximum opaque field size allowed before the optional stateful fallback is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeLimits {
    max_opaque_field_bytes: usize,
}

impl EnvelopeLimits {
    /// Creates a size limit for final `encrypted_content` or `signature` field values.
    pub const fn new(max_opaque_field_bytes: usize) -> Self {
        Self {
            max_opaque_field_bytes,
        }
    }

    /// Returns the maximum byte length for the final opaque field value.
    pub const fn max_opaque_field_bytes(&self) -> usize {
        self.max_opaque_field_bytes
    }

    fn max_envelope_bytes(self, reserved_prefix_bytes: usize, field_name: &str) -> Result<usize> {
        self.max_opaque_field_bytes
            .checked_sub(reserved_prefix_bytes)
            .ok_or_else(|| {
                mapping_error(format!(
                    "{field_name} prefix length {reserved_prefix_bytes} exceeds configured reasoning opaque field limit {}",
                    self.max_opaque_field_bytes
                ))
            })
    }
}

impl Default for EnvelopeLimits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_OPAQUE_FIELD_BYTES)
    }
}

/// Optional stateful fallback used only when an encoded envelope exceeds its limit.
pub trait ReasoningStore {
    /// Stores a source block under an id chosen by the envelope layer.
    fn put(&self, id: &str, block: SourceBlock) -> Result<()>;

    /// Loads a source block previously stored under the given id.
    fn get(&self, id: &str) -> Result<SourceBlock>;
}

/// Store implementation used by default to preserve stateless behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopStore;

impl ReasoningStore for NoopStore {
    fn put(&self, _id: &str, _block: SourceBlock) -> Result<()> {
        Err(mapping_error(
            "reasoning envelope exceeded configured length limit and no stateful ReasoningStore is enabled",
        ))
    }

    fn get(&self, _id: &str) -> Result<SourceBlock> {
        Err(mapping_error(
            "stateful reasoning envelope reference cannot be resolved because no ReasoningStore is enabled",
        ))
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_ref: Option<String>,
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
            store_ref: None,
        }
    }

    /// Creates a compact envelope that points to a stateful store entry.
    pub fn new_store_ref(source_block: &SourceBlock, store_ref: impl Into<String>) -> Self {
        Self {
            version: ENVELOPE_VERSION,
            source: source_block.source.clone(),
            payload: Vec::new(),
            checksum: checksum(
                ENVELOPE_VERSION,
                &source_block.source,
                &source_block.payload,
            ),
            store_ref: Some(store_ref.into()),
        }
    }

    /// Verifies integrity and converts the envelope back to its raw source block.
    pub fn into_source_block(self) -> Result<SourceBlock> {
        if self.store_ref.is_some() {
            return Err(mapping_error(
                "stateful reasoning envelope requires a configured ReasoningStore",
            ));
        }

        let source_block = SourceBlock::new(self.source.clone(), self.payload.clone());
        self.verify_source_block(&source_block)?;

        Ok(source_block)
    }

    /// Verifies an envelope, resolving a store reference when the fallback was used.
    pub fn into_source_block_with_store<S: ReasoningStore + ?Sized>(
        self,
        store: &S,
    ) -> Result<SourceBlock> {
        let Some(store_ref) = self.store_ref.clone() else {
            return self.into_source_block();
        };

        if !self.payload.is_empty() {
            return Err(mapping_error(
                "stateful reasoning envelope must not also carry an inline payload",
            ));
        }

        let source_block = store.get(&store_ref)?;
        self.verify_source_block(&source_block)?;

        Ok(source_block)
    }

    fn verify_source_block(&self, source_block: &SourceBlock) -> Result<()> {
        if source_block.source != self.source {
            return Err(mapping_error(
                "reasoning envelope store reference returned the wrong provider source",
            ));
        }

        let expected = checksum(self.version, &self.source, &source_block.payload);
        if self.checksum != expected {
            return Err(mapping_error("reasoning envelope checksum mismatch"));
        }

        if self.version != ENVELOPE_VERSION {
            return Err(mapping_error(format!(
                "unsupported reasoning envelope version {}",
                self.version
            )));
        }

        Ok(())
    }
}

/// Encodes a provider source block into a base64 JSON envelope.
pub fn wrap(source_block: &SourceBlock) -> Result<String> {
    wrap_with_store(source_block, EnvelopeLimits::default(), &NoopStore)
}

/// Encodes a source block, using `store` only when the inline envelope is too large.
pub fn wrap_with_store<S: ReasoningStore + ?Sized>(
    source_block: &SourceBlock,
    limits: EnvelopeLimits,
    store: &S,
) -> Result<String> {
    let max_envelope_bytes = limits.max_envelope_bytes(0, "reasoning envelope")?;
    encode_envelope_with_limit(source_block, max_envelope_bytes, store)
}

/// Decodes and verifies a base64 envelope, returning the original provider payload bytes.
pub fn unwrap(encoded: &str) -> Result<SourceBlock> {
    unwrap_with_store(encoded, &NoopStore)
}

/// Returns true when a string decodes to this gateway's envelope structure.
pub fn is_reasoning_envelope(encoded: &str) -> bool {
    let Ok(bytes) = STANDARD.decode(encoded) else {
        return false;
    };

    serde_json::from_slice::<Envelope>(&bytes).is_ok()
}

/// Decodes an envelope, resolving stateful fallback references through `store`.
pub fn unwrap_with_store<S: ReasoningStore + ?Sized>(
    encoded: &str,
    store: &S,
) -> Result<SourceBlock> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|err| mapping_error(format!("reasoning envelope is not valid base64: {err}")))?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|err| mapping_error(format!("reasoning envelope is not valid JSON: {err}")))?;

    envelope.into_source_block_with_store(store)
}

/// Wraps a source block as a Responses-compatible reasoning item.
pub fn wrap_as_responses_reasoning_item(source_block: &SourceBlock) -> Result<Value> {
    wrap_as_responses_reasoning_item_with_store(source_block, EnvelopeLimits::default(), &NoopStore)
}

/// Wraps a source block as a Responses reasoning item with optional store fallback.
pub fn wrap_as_responses_reasoning_item_with_store<S: ReasoningStore + ?Sized>(
    source_block: &SourceBlock,
    limits: EnvelopeLimits,
    store: &S,
) -> Result<Value> {
    let envelope = Envelope::new(source_block);
    let encrypted_content = wrap_with_store(source_block, limits, store)?;

    let mut item = Map::new();
    item.insert(
        "id".to_owned(),
        Value::String(responses_reasoning_item_id(&envelope)),
    );
    item.insert(
        "type".to_owned(),
        Value::String(RESPONSES_REASONING_TYPE.to_owned()),
    );
    item.insert("summary".to_owned(), Value::Array(Vec::new()));
    item.insert(
        "encrypted_content".to_owned(),
        Value::String(encrypted_content),
    );

    Ok(Value::Object(item))
}

/// Extracts and verifies an envelope carried by a Responses reasoning item.
pub fn unwrap_from_responses_reasoning_item(item: &Value) -> Result<SourceBlock> {
    unwrap_from_responses_reasoning_item_with_store(item, &NoopStore)
}

/// Extracts a Responses reasoning envelope, resolving store references when configured.
pub fn unwrap_from_responses_reasoning_item_with_store<S: ReasoningStore + ?Sized>(
    item: &Value,
    store: &S,
) -> Result<SourceBlock> {
    let object = item
        .as_object()
        .ok_or_else(|| mapping_error("Responses reasoning item must be a JSON object"))?;

    let item_type = required_string(object, "type", "Responses reasoning item.type")?;
    if item_type != RESPONSES_REASONING_TYPE {
        return Err(mapping_error(format!(
            "Responses reasoning item.type must be `{RESPONSES_REASONING_TYPE}`"
        )));
    }

    // Codex 0.142.5 omits id/status on the next turn, so id is validated only when present.
    if let Some(id) = optional_string(object, "id", "Responses reasoning item.id")?
        && !id.starts_with("rs_")
    {
        return Err(mapping_error(
            "Responses reasoning item.id must start with `rs_` when present",
        ));
    }

    let encrypted_content = required_string(
        object,
        "encrypted_content",
        "Responses reasoning item.encrypted_content",
    )?;
    unwrap_with_store(encrypted_content, store)
}

/// Wraps a source block as an Anthropic-compatible thinking signature string.
pub fn wrap_as_signature(source_block: &SourceBlock) -> Result<String> {
    wrap_as_signature_with_store(source_block, EnvelopeLimits::default(), &NoopStore)
}

/// Returns true when an Anthropic signature carries a gateway-owned envelope.
pub fn is_wrapped_signature(signature: &str) -> bool {
    signature.starts_with(ANTHROPIC_SIGNATURE_PREFIX)
}

/// Wraps a source block as an Anthropic signature with optional store fallback.
pub fn wrap_as_signature_with_store<S: ReasoningStore + ?Sized>(
    source_block: &SourceBlock,
    limits: EnvelopeLimits,
    store: &S,
) -> Result<String> {
    let max_envelope_bytes = limits.max_envelope_bytes(
        ANTHROPIC_SIGNATURE_PREFIX.len(),
        "Anthropic thinking signature",
    )?;
    let encoded = encode_envelope_with_limit(source_block, max_envelope_bytes, store)?;

    Ok(format!("{ANTHROPIC_SIGNATURE_PREFIX}{}", encoded))
}

/// Extracts and verifies an envelope carried by an Anthropic thinking signature.
pub fn unwrap_from_signature(signature: &str) -> Result<SourceBlock> {
    unwrap_from_signature_with_store(signature, &NoopStore)
}

/// Extracts an Anthropic signature envelope, resolving store references when configured.
pub fn unwrap_from_signature_with_store<S: ReasoningStore + ?Sized>(
    signature: &str,
    store: &S,
) -> Result<SourceBlock> {
    let encoded = signature
        .strip_prefix(ANTHROPIC_SIGNATURE_PREFIX)
        .ok_or_else(|| {
            mapping_error(format!(
                "Anthropic thinking signature must start with `{ANTHROPIC_SIGNATURE_PREFIX}`"
            ))
        })?;

    if encoded.is_empty() {
        return Err(mapping_error(
            "Anthropic thinking signature is missing its envelope payload",
        ));
    }

    unwrap_with_store(encoded, store)
}

fn encode_envelope(envelope: &Envelope) -> Result<String> {
    let bytes = serde_json::to_vec(envelope)?;

    Ok(STANDARD.encode(bytes))
}

fn encode_envelope_with_limit<S: ReasoningStore + ?Sized>(
    source_block: &SourceBlock,
    max_envelope_bytes: usize,
    store: &S,
) -> Result<String> {
    let inline_envelope = Envelope::new(source_block);
    let encoded = encode_envelope(&inline_envelope)?;
    if encoded.len() <= max_envelope_bytes {
        return Ok(encoded);
    }

    let store_ref = store_ref_id(&inline_envelope);
    let reference_envelope = Envelope::new_store_ref(source_block, store_ref.clone());
    let reference_encoded = encode_envelope(&reference_envelope)?;
    if reference_encoded.len() > max_envelope_bytes {
        return Err(mapping_error(format!(
            "reasoning envelope store reference length {} exceeds configured limit {}",
            reference_encoded.len(),
            max_envelope_bytes
        )));
    }

    store.put(&store_ref, source_block.clone())?;
    Ok(reference_encoded)
}

fn responses_reasoning_item_id(envelope: &Envelope) -> String {
    format!(
        "{RESPONSES_REASONING_ID_PREFIX}_v{}_{}_{:08x}",
        envelope.version,
        source_tag(&envelope.source),
        envelope.checksum
    )
}

fn store_ref_id(envelope: &Envelope) -> String {
    format!(
        "rb_llm_proxy_v{}_{}_{:016x}_{:08x}",
        envelope.version,
        source_tag(&envelope.source),
        envelope.payload.len(),
        envelope.checksum
    )
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use serde_json::json;

    use super::*;

    const TEST_OPAQUE_LIMIT_BYTES: usize = 1024;

    #[derive(Default)]
    struct MemoryStore {
        blocks: Mutex<HashMap<String, SourceBlock>>,
    }

    impl MemoryStore {
        fn len(&self) -> usize {
            self.blocks.lock().unwrap().len()
        }
    }

    impl ReasoningStore for MemoryStore {
        fn put(&self, id: &str, block: SourceBlock) -> Result<()> {
            self.blocks.lock().unwrap().insert(id.to_owned(), block);
            Ok(())
        }

        fn get(&self, id: &str) -> Result<SourceBlock> {
            self.blocks
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| mapping_error(format!("missing test source block `{id}`")))
        }
    }

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
    fn wraps_envelope_as_responses_reasoning_item() {
        let source = SourceBlock::from_json(
            Provider::Anthropic,
            &json!({
                "type": "thinking",
                "thinking": "Need a tool.",
                "signature": "sig_anthropic_123"
            }),
        )
        .unwrap();

        let item = wrap_as_responses_reasoning_item(&source).unwrap();

        assert_eq!(item["type"], "reasoning");
        assert_eq!(item["summary"], json!([]));
        assert!(item["id"].as_str().is_some_and(|id| id.starts_with("rs_")));
        assert!(item["encrypted_content"].as_str().unwrap().len() > 64);
        assert_eq!(unwrap_from_responses_reasoning_item(&item).unwrap(), source);
    }

    #[test]
    fn wraps_envelope_as_anthropic_signature() {
        let source = SourceBlock::from_json(
            Provider::Responses,
            &json!({
                "type": "reasoning",
                "summary": [],
                "encrypted_content": "enc_resp_123",
                "status": "completed"
            }),
        )
        .unwrap();

        let signature = wrap_as_signature(&source).unwrap();

        assert!(signature.starts_with(ANTHROPIC_SIGNATURE_PREFIX));
        assert!(signature.len() > ANTHROPIC_SIGNATURE_PREFIX.len() + 64);
        assert_eq!(unwrap_from_signature(&signature).unwrap(), source);
    }

    #[test]
    fn responses_encrypted_content_client_echo_round_trips_tool_use_payload_bytes() {
        let payload = br#"{"type":"thinking","thinking":"Need weather before final answer.","signature":"sig_real_anthropic_tool_1","tool_use":{"type":"tool_use","id":"toolu_weather_1","name":"lookup_weather","input":{"city":"Paris"}}}"#;
        let source = SourceBlock::new(Provider::Anthropic, payload.to_vec());

        let item = wrap_as_responses_reasoning_item(&source).unwrap();
        let echoed_item = json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": item["encrypted_content"].as_str().unwrap()
        });

        let decoded = unwrap_from_responses_reasoning_item(&echoed_item).unwrap();

        assert_eq!(decoded.source, Provider::Anthropic);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn anthropic_signature_client_echo_round_trips_tool_use_payload_bytes() {
        let payload = br#"{"type":"reasoning","id":"rs_weather_1","summary":[{"type":"summary_text","text":"Need current weather."}],"encrypted_content":"enc_weather_tool_opaque","status":"completed","function_call":{"type":"function_call","call_id":"call_weather_1","name":"lookup_weather","arguments":"{\"city\":\"Paris\"}"}}"#;
        let source = SourceBlock::new(Provider::Responses, payload.to_vec());

        let signature = wrap_as_signature(&source).unwrap();
        let echoed_block = json!({
            "type": "thinking",
            "thinking": "Need weather before final answer.",
            "signature": signature
        });

        let decoded = unwrap_from_signature(echoed_block["signature"].as_str().unwrap()).unwrap();

        assert_eq!(decoded.source, Provider::Responses);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn inline_envelope_does_not_use_store_when_under_limit() {
        let store = MemoryStore::default();
        let limits = EnvelopeLimits::new(TEST_OPAQUE_LIMIT_BYTES);
        let source = SourceBlock::new(
            Provider::Responses,
            br#"{"type":"reasoning","encrypted_content":"small"}"#.to_vec(),
        );

        let encoded = wrap_with_store(&source, limits, &store).unwrap();

        assert_eq!(store.len(), 0);
        assert_eq!(unwrap(&encoded).unwrap(), source);
    }

    #[test]
    fn oversized_envelope_requires_explicit_store() {
        let limits = EnvelopeLimits::new(TEST_OPAQUE_LIMIT_BYTES);
        let source = SourceBlock::new(Provider::Responses, vec![b'x'; 4096]);

        let err = wrap_with_store(&source, limits, &NoopStore).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("ReasoningStore"))
        );
    }

    #[test]
    fn oversized_envelope_round_trips_through_configured_store() {
        let store = MemoryStore::default();
        let limits = EnvelopeLimits::new(TEST_OPAQUE_LIMIT_BYTES);
        let source = SourceBlock::new(Provider::Responses, vec![b'x'; 4096]);

        let encoded = wrap_with_store(&source, limits, &store).unwrap();

        assert!(encoded.len() <= limits.max_opaque_field_bytes());
        assert_eq!(store.len(), 1);
        assert!(
            matches!(unwrap(&encoded).unwrap_err(), ProxyError::ProtocolMapping(message) if message.contains("ReasoningStore"))
        );
        assert_eq!(unwrap_with_store(&encoded, &store).unwrap(), source);
    }

    #[test]
    fn responses_reasoning_item_store_fallback_respects_limit() {
        let store = MemoryStore::default();
        let limits = EnvelopeLimits::new(TEST_OPAQUE_LIMIT_BYTES);
        let source = SourceBlock::new(Provider::Anthropic, vec![b't'; 4096]);

        let item = wrap_as_responses_reasoning_item_with_store(&source, limits, &store).unwrap();
        let encrypted_content = item["encrypted_content"].as_str().unwrap();

        assert!(encrypted_content.len() <= limits.max_opaque_field_bytes());
        assert_eq!(
            unwrap_from_responses_reasoning_item_with_store(&item, &store).unwrap(),
            source
        );
    }

    #[test]
    fn anthropic_signature_store_fallback_respects_prefixed_limit() {
        let store = MemoryStore::default();
        let limits = EnvelopeLimits::new(TEST_OPAQUE_LIMIT_BYTES);
        let source = SourceBlock::new(Provider::Responses, vec![b's'; 4096]);

        let signature = wrap_as_signature_with_store(&source, limits, &store).unwrap();

        assert!(signature.len() <= limits.max_opaque_field_bytes());
        assert_eq!(
            unwrap_from_signature_with_store(&signature, &store).unwrap(),
            source
        );
    }

    #[test]
    fn anthropic_signature_preserves_payload_bytes() {
        let payload =
            br#"{"type":"reasoning","summary":[],"encrypted_content":"opaque-tool-use-history"}"#;
        let source = SourceBlock::new(Provider::Responses, payload.to_vec());

        let decoded = unwrap_from_signature(&wrap_as_signature(&source).unwrap()).unwrap();

        assert_eq!(decoded.source, Provider::Responses);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn rejects_non_proxy_anthropic_signature() {
        let err = unwrap_from_signature("sig_real_anthropic_opaque").unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("signature"))
        );
    }

    #[test]
    fn rejects_empty_anthropic_signature_payload() {
        let err = unwrap_from_signature(ANTHROPIC_SIGNATURE_PREFIX).unwrap_err();

        assert!(matches!(err, ProxyError::ProtocolMapping(message) if message.contains("missing")));
    }

    #[test]
    fn anthropic_signature_detects_tampered_payload() {
        let source = SourceBlock::new(
            Provider::Responses,
            br#"{"type":"reasoning","encrypted_content":"enc"}"#.to_vec(),
        );
        let signature = wrap_as_signature(&source).unwrap();
        let encoded = signature.strip_prefix(ANTHROPIC_SIGNATURE_PREFIX).unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        envelope.payload[0] ^= 0xff;
        let tampered = format!(
            "{ANTHROPIC_SIGNATURE_PREFIX}{}",
            STANDARD.encode(serde_json::to_vec(&envelope).unwrap())
        );

        let err = unwrap_from_signature(&tampered).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("checksum"))
        );
    }

    #[test]
    fn unwraps_codex_echoed_reasoning_item_without_id_or_status() {
        let source = SourceBlock::new(
            Provider::Anthropic,
            br#"{"type":"thinking","thinking":"x","signature":"sig"}"#.to_vec(),
        );
        let encrypted_content = wrap(&source).unwrap();
        let echoed_item = json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": encrypted_content
        });

        let decoded = unwrap_from_responses_reasoning_item(&echoed_item).unwrap();

        assert_eq!(decoded, source);
    }

    #[test]
    fn rejects_non_reasoning_responses_item() {
        let item = json!({
            "type": "message",
            "encrypted_content": "not-used"
        });

        let err = unwrap_from_responses_reasoning_item(&item).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("item.type"))
        );
    }

    #[test]
    fn rejects_non_rs_reasoning_id_when_present() {
        let source = SourceBlock::new(
            Provider::Anthropic,
            br#"{"type":"thinking","thinking":"x","signature":"sig"}"#.to_vec(),
        );
        let item = json!({
            "id": "bad_123",
            "type": "reasoning",
            "summary": [],
            "encrypted_content": wrap(&source).unwrap()
        });

        let err = unwrap_from_responses_reasoning_item(&item).unwrap_err();

        assert!(matches!(err, ProxyError::ProtocolMapping(message) if message.contains("rs_")));
    }

    #[test]
    fn responses_reasoning_item_detects_tampered_encrypted_content() {
        let source = SourceBlock::new(
            Provider::Anthropic,
            br#"{"type":"thinking","thinking":"x","signature":"sig"}"#.to_vec(),
        );
        let mut item = wrap_as_responses_reasoning_item(&source).unwrap();
        let encrypted_content = item["encrypted_content"].as_str().unwrap();
        let bytes = STANDARD.decode(encrypted_content).unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        envelope.payload[0] ^= 0xff;
        item["encrypted_content"] = json!(STANDARD.encode(serde_json::to_vec(&envelope).unwrap()));

        let err = unwrap_from_responses_reasoning_item(&item).unwrap_err();

        assert!(
            matches!(err, ProxyError::ProtocolMapping(message) if message.contains("checksum"))
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
