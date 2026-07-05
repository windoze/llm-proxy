//! Helpers for preserving OpenAI Responses reasoning items across conversions.

use serde_json::{Map, Value};

use crate::{
    error::{ProxyError, Result},
    ir::message::{Provider, Thinking},
};

const REASONING_TYPE: &str = "reasoning";

/// Returns a normalized clone of a Responses reasoning item without dropping opaque fields.
pub(super) fn normalize_reasoning_item(
    item: &Map<String, Value>,
    path: impl Into<String>,
) -> Result<Map<String, Value>> {
    let path = path.into();
    let item_type = required_string(item, "type", format!("{path}.type"))?;
    if item_type != REASONING_TYPE {
        return Err(mapping_error(format!(
            "{path}.type must be `{REASONING_TYPE}`"
        )));
    }

    required_string(
        item,
        "encrypted_content",
        format!("{path}.encrypted_content"),
    )?;

    let mut normalized = item.clone();
    match normalized.get("status") {
        Some(Value::Null) => {
            normalized.remove("status");
        }
        Some(Value::String(_)) | None => {}
        Some(_) => {
            return Err(mapping_error(format!(
                "{path}.status must be a string when present"
            )));
        }
    }

    Ok(normalized)
}

/// Extracts the opaque Responses encrypted-content field from a normalized reasoning item.
pub(super) fn encrypted_content(
    item: &Map<String, Value>,
    path: impl Into<String>,
) -> Result<&str> {
    required_string(item, "encrypted_content", path)
}

/// Serializes a normalized reasoning item into the IR thinking opaque payload.
pub(super) fn encode_preserved_reasoning_item(item: Map<String, Value>) -> Result<Vec<u8>> {
    serde_json::to_vec(&Value::Object(item)).map_err(Into::into)
}

/// Decodes an IR thinking opaque payload back into a normalized Responses reasoning item.
pub(super) fn preserved_reasoning_item_from_thinking(
    thinking: &Thinking,
    path: impl Into<String>,
) -> Result<Option<Map<String, Value>>> {
    if thinking.source != Provider::Responses {
        return Ok(None);
    }

    let Some(opaque) = &thinking.opaque else {
        return Ok(None);
    };

    let Ok(value) = serde_json::from_slice::<Value>(opaque) else {
        return Ok(None);
    };

    match value {
        Value::Object(item) => normalize_reasoning_item(&item, path).map(Some),
        _ => Ok(None),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<&'a str> {
    let path = path.into();
    match object.get(field) {
        Some(Value::String(value)) => Ok(value),
        Some(Value::Null) | None => Err(mapping_error(format!("{path} is required"))),
        Some(_) => Err(mapping_error(format!("{path} must be a string"))),
    }
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}
