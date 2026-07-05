//! Capability decisions for translating IR requests into concrete protocols.
//!
//! This module is the central table for lossy `IR -> protocol` feature handling:
//! a feature is either passed through, dropped, emulated, or rejected with 400.

use serde_json::{Map, Value, json};

use crate::{
    error::{ProxyError, Result},
    ir::request::ToolDef,
};

/// Synthetic Anthropic tool used to emulate Responses/Chat structured output.
pub const ANTHROPIC_STRUCTURED_OUTPUT_TOOL_NAME: &str = "llm_proxy_structured_output";

const ANTHROPIC_STRUCTURED_OUTPUT_DESCRIPTION: &str =
    "Return the final answer by calling this tool with JSON matching the requested schema.";

/// Target protocol for an IR request encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrTargetProtocol {
    OpenAiChat,
    AnthropicMessages,
    OpenAiResponses,
}

impl IrTargetProtocol {
    fn as_error_protocol(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic",
            Self::OpenAiResponses => "responses",
        }
    }
}

/// The selected compatibility action for a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureDecision {
    PassThrough,
    Drop,
    Emulate,
    Reject,
}

/// A rule-table decision for a request feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeaturePolicy {
    pub decision: FeatureDecision,
    pub feature: String,
}

impl FeaturePolicy {
    fn new(decision: FeatureDecision, feature: impl Into<String>) -> Self {
        Self {
            decision,
            feature: feature.into(),
        }
    }
}

/// Returns the table decision for a preserved IR extra field in a target protocol.
pub fn extra_field_policy(target: IrTargetProtocol, field: &str, value: &Value) -> FeaturePolicy {
    match target {
        IrTargetProtocol::OpenAiChat => openai_chat_extra_policy(field, value),
        IrTargetProtocol::AnthropicMessages => anthropic_extra_policy(field, value),
        IrTargetProtocol::OpenAiResponses => responses_extra_policy(field, value),
    }
}

/// Filters IR extra fields according to the target protocol's capability table.
pub fn passthrough_extra_fields(
    target: IrTargetProtocol,
    extra: &Map<String, Value>,
    target_core_fields: &[&str],
    provider_blocklist: &[&str],
) -> Result<Map<String, Value>> {
    let mut forwarded = Map::new();

    for (field, value) in extra {
        if target_core_fields.contains(&field.as_str())
            || provider_blocklist.contains(&field.as_str())
        {
            continue;
        }

        let policy = extra_field_policy(target, field, value);
        match policy.decision {
            FeatureDecision::PassThrough => {
                forwarded.insert(field.clone(), value.clone());
            }
            FeatureDecision::Drop | FeatureDecision::Emulate => {}
            FeatureDecision::Reject => return Err(unsupported(policy.feature, target)),
        }
    }

    Ok(forwarded)
}

/// Converts Responses `text.format` into a Chat `response_format` when possible.
pub fn chat_response_format_from_extra(extra: &Map<String, Value>) -> Result<Option<Value>> {
    let Some(text) = extra.get("text") else {
        return Ok(None);
    };
    let Some(format) = parse_responses_text_format(text, "request.extra.text")? else {
        return Ok(None);
    };

    Ok(match format {
        StructuredOutputFormat::JsonObject => Some(json!({ "type": "json_object" })),
        StructuredOutputFormat::JsonSchema(schema) => {
            Some(json!({ "type": "json_schema", "json_schema": schema.into_chat_json_schema() }))
        }
    })
}

/// Converts Chat `response_format` into Responses `text.format` when possible.
pub fn responses_text_format_from_extra(extra: &Map<String, Value>) -> Result<Option<Value>> {
    let Some(response_format) = extra.get("response_format") else {
        return Ok(None);
    };
    let Some(format) =
        parse_chat_response_format(response_format, "request.extra.response_format")?
    else {
        return Ok(None);
    };

    Ok(match format {
        StructuredOutputFormat::JsonObject => Some(json!({ "format": { "type": "json_object" } })),
        StructuredOutputFormat::JsonSchema(schema) => Some(json!({
            "format": schema.into_responses_json_schema_format()
        })),
    })
}

/// Builds the synthetic Anthropic tool used to emulate structured output.
pub fn anthropic_structured_output_tool(extra: &Map<String, Value>) -> Result<Option<ToolDef>> {
    let Some(format) = structured_output_format_from_extra(extra)? else {
        return Ok(None);
    };

    let (input_schema, description) = match format {
        StructuredOutputFormat::JsonObject => (
            json!({
                "type": "object",
                "additionalProperties": true
            }),
            ANTHROPIC_STRUCTURED_OUTPUT_DESCRIPTION.to_owned(),
        ),
        StructuredOutputFormat::JsonSchema(schema) => {
            let description = schema
                .description
                .as_deref()
                .map(|description| {
                    format!("{ANTHROPIC_STRUCTURED_OUTPUT_DESCRIPTION}\n\n{description}")
                })
                .unwrap_or_else(|| ANTHROPIC_STRUCTURED_OUTPUT_DESCRIPTION.to_owned());
            (schema.schema, description)
        }
    };

    Ok(Some(ToolDef {
        name: ANTHROPIC_STRUCTURED_OUTPUT_TOOL_NAME.to_owned(),
        description: Some(description),
        input_schema,
    }))
}

/// Extracts a reasoning effort carried by richer protocol-specific settings.
pub fn reasoning_effort_from_extra(extra: &Map<String, Value>) -> Result<Option<&str>> {
    let reasoning_effort =
        effort_from_object_field(extra.get("reasoning"), "request.extra.reasoning")?;
    let output_config_effort =
        effort_from_object_field(extra.get("output_config"), "request.extra.output_config")?;

    match (reasoning_effort, output_config_effort) {
        (Some(reasoning), Some(output_config)) if reasoning != output_config => {
            Err(ProxyError::ProtocolMapping(
                "request.extra.reasoning.effort conflicts with request.extra.output_config.effort"
                    .to_owned(),
            ))
        }
        (Some(effort), _) | (_, Some(effort)) => Ok(Some(effort)),
        (None, None) => Ok(None),
    }
}

fn openai_chat_extra_policy(field: &str, _value: &Value) -> FeaturePolicy {
    match field {
        "text" | "reasoning" | "output_config" => {
            FeaturePolicy::new(FeatureDecision::Emulate, format!("request.extra.{field}"))
        }
        "previous_response_id" | "prompt" | "truncation" | "background" => {
            FeaturePolicy::new(FeatureDecision::Reject, format!("request.extra.{field}"))
        }
        "include"
        | "max_tool_calls"
        | "parallel_tool_calls"
        | "container"
        | "context_management"
        | "mcp_servers"
        | "thinking" => FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}")),
        _ => FeaturePolicy::new(
            FeatureDecision::PassThrough,
            format!("request.extra.{field}"),
        ),
    }
}

fn anthropic_extra_policy(field: &str, value: &Value) -> FeaturePolicy {
    match field {
        "metadata" | "service_tier" | "thinking" | "output_config" | "context_management"
        | "container" | "mcp_servers" => FeaturePolicy::new(
            FeatureDecision::PassThrough,
            format!("request.extra.{field}"),
        ),
        "text" | "response_format" => {
            FeaturePolicy::new(FeatureDecision::Emulate, format!("request.extra.{field}"))
        }
        "store" => drop_or_reject_disabled_bool(field, value),
        "background" => drop_or_reject_disabled_bool(field, value),
        "previous_response_id" | "prompt" => reject_if_not_null(field, value),
        "truncation" => drop_disabled_truncation_or_reject(value),
        "reasoning"
        | "include"
        | "max_tool_calls"
        | "parallel_tool_calls"
        | "stream_options"
        | "user" => FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}")),
        _ => FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}")),
    }
}

fn responses_extra_policy(field: &str, _value: &Value) -> FeaturePolicy {
    match field {
        "background"
        | "include"
        | "max_tool_calls"
        | "metadata"
        | "parallel_tool_calls"
        | "previous_response_id"
        | "prompt"
        | "reasoning"
        | "service_tier"
        | "store"
        | "stream_options"
        | "text"
        | "truncation"
        | "user" => FeaturePolicy::new(
            FeatureDecision::PassThrough,
            format!("request.extra.{field}"),
        ),
        "output_config" | "response_format" => {
            FeaturePolicy::new(FeatureDecision::Emulate, format!("request.extra.{field}"))
        }
        _ => FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}")),
    }
}

fn drop_or_reject_disabled_bool(field: &str, value: &Value) -> FeaturePolicy {
    match value {
        Value::Null | Value::Bool(false) => {
            FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}"))
        }
        _ => FeaturePolicy::new(FeatureDecision::Reject, format!("request.extra.{field}")),
    }
}

fn reject_if_not_null(field: &str, value: &Value) -> FeaturePolicy {
    match value {
        Value::Null => FeaturePolicy::new(FeatureDecision::Drop, format!("request.extra.{field}")),
        _ => FeaturePolicy::new(FeatureDecision::Reject, format!("request.extra.{field}")),
    }
}

fn drop_disabled_truncation_or_reject(value: &Value) -> FeaturePolicy {
    match value {
        Value::Null => FeaturePolicy::new(FeatureDecision::Drop, "request.extra.truncation"),
        Value::String(value) if value == "disabled" => {
            FeaturePolicy::new(FeatureDecision::Drop, "request.extra.truncation")
        }
        _ => FeaturePolicy::new(FeatureDecision::Reject, "request.extra.truncation"),
    }
}

fn structured_output_format_from_extra(
    extra: &Map<String, Value>,
) -> Result<Option<StructuredOutputFormat>> {
    let text_format = extra
        .get("text")
        .map(|value| parse_responses_text_format(value, "request.extra.text"))
        .transpose()?
        .flatten();
    let response_format = extra
        .get("response_format")
        .map(|value| parse_chat_response_format(value, "request.extra.response_format"))
        .transpose()?
        .flatten();

    match (text_format, response_format) {
        (Some(_), Some(_)) => Err(ProxyError::ProtocolMapping(
            "request.extra.text and request.extra.response_format both request structured output"
                .to_owned(),
        )),
        (Some(format), None) | (None, Some(format)) => Ok(Some(format)),
        (None, None) => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum StructuredOutputFormat {
    JsonObject,
    JsonSchema(JsonSchemaFormat),
}

#[derive(Debug, Clone, PartialEq)]
struct JsonSchemaFormat {
    name: Option<String>,
    description: Option<String>,
    schema: Value,
    strict: Option<bool>,
}

impl JsonSchemaFormat {
    fn into_chat_json_schema(self) -> Value {
        let mut json_schema = Map::new();
        if let Some(name) = self.name {
            json_schema.insert("name".to_owned(), Value::String(name));
        }
        if let Some(description) = self.description {
            json_schema.insert("description".to_owned(), Value::String(description));
        }
        json_schema.insert("schema".to_owned(), self.schema);
        if let Some(strict) = self.strict {
            json_schema.insert("strict".to_owned(), Value::Bool(strict));
        }
        Value::Object(json_schema)
    }

    fn into_responses_json_schema_format(self) -> Value {
        let mut format = Map::new();
        format.insert("type".to_owned(), Value::String("json_schema".to_owned()));
        if let Some(name) = self.name {
            format.insert("name".to_owned(), Value::String(name));
        }
        if let Some(description) = self.description {
            format.insert("description".to_owned(), Value::String(description));
        }
        format.insert("schema".to_owned(), self.schema);
        if let Some(strict) = self.strict {
            format.insert("strict".to_owned(), Value::Bool(strict));
        }
        Value::Object(format)
    }
}

fn parse_responses_text_format(
    value: &Value,
    path: &str,
) -> Result<Option<StructuredOutputFormat>> {
    let text = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let Some(format) = text.get("format") else {
        return Ok(None);
    };
    parse_responses_format(format, format!("{path}.format"))
}

fn parse_responses_format(
    value: &Value,
    path: impl Into<String>,
) -> Result<Option<StructuredOutputFormat>> {
    let path = path.into();
    let format = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let format_type = required_string(format, "type", format!("{path}.type"))?;

    match format_type {
        "text" => Ok(None),
        "json_object" => Ok(Some(StructuredOutputFormat::JsonObject)),
        "json_schema" => Ok(Some(StructuredOutputFormat::JsonSchema(
            json_schema_from_responses_format(format, path)?,
        ))),
        other => Err(unsupported(
            format!("Responses text.format `{other}`"),
            IrTargetProtocol::OpenAiResponses,
        )),
    }
}

fn parse_chat_response_format(value: &Value, path: &str) -> Result<Option<StructuredOutputFormat>> {
    let format = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object")))?;
    let format_type = required_string(format, "type", format!("{path}.type"))?;

    match format_type {
        "text" => Ok(None),
        "json_object" => Ok(Some(StructuredOutputFormat::JsonObject)),
        "json_schema" => {
            let json_schema = format
                .get("json_schema")
                .and_then(Value::as_object)
                .ok_or_else(|| mapping_error(format!("{path}.json_schema must be an object")))?;
            Ok(Some(StructuredOutputFormat::JsonSchema(
                json_schema_from_chat_response_format(json_schema, format!("{path}.json_schema"))?,
            )))
        }
        other => Err(unsupported(
            format!("Chat response_format `{other}`"),
            IrTargetProtocol::OpenAiChat,
        )),
    }
}

fn json_schema_from_responses_format(
    format: &Map<String, Value>,
    path: String,
) -> Result<JsonSchemaFormat> {
    Ok(JsonSchemaFormat {
        name: optional_string(format, "name", format!("{path}.name"))?.map(str::to_owned),
        description: optional_string(format, "description", format!("{path}.description"))?
            .map(str::to_owned),
        schema: format
            .get("schema")
            .cloned()
            .ok_or_else(|| mapping_error(format!("{path}.schema is required")))?,
        strict: optional_bool(format, "strict", format!("{path}.strict"))?,
    })
}

fn json_schema_from_chat_response_format(
    json_schema: &Map<String, Value>,
    path: String,
) -> Result<JsonSchemaFormat> {
    Ok(JsonSchemaFormat {
        name: optional_string(json_schema, "name", format!("{path}.name"))?.map(str::to_owned),
        description: optional_string(json_schema, "description", format!("{path}.description"))?
            .map(str::to_owned),
        schema: json_schema
            .get("schema")
            .cloned()
            .ok_or_else(|| mapping_error(format!("{path}.schema is required")))?,
        strict: optional_bool(json_schema, "strict", format!("{path}.strict"))?,
    })
}

fn effort_from_object_field<'a>(value: Option<&'a Value>, path: &str) -> Result<Option<&'a str>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| mapping_error(format!("{path} must be an object when present")))?;
    optional_string(object, "effort", format!("{path}.effort"))
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

fn optional_bool(
    object: &Map<String, Value>,
    field: &str,
    path: impl Into<String>,
) -> Result<Option<bool>> {
    let path = path.into();
    match object.get(field) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(mapping_error(format!("{path} must be a boolean"))),
    }
}

fn unsupported(feature: impl Into<String>, target: IrTargetProtocol) -> ProxyError {
    ProxyError::UnsupportedFeature {
        feature: feature.into(),
        protocol: target.as_error_protocol().to_owned(),
    }
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    #[test]
    fn classifies_extra_fields_for_each_ir_target_protocol() {
        assert_eq!(
            extra_field_policy(IrTargetProtocol::AnthropicMessages, "text", &json!({})).decision,
            FeatureDecision::Emulate
        );
        assert_eq!(
            extra_field_policy(IrTargetProtocol::AnthropicMessages, "store", &json!(false))
                .decision,
            FeatureDecision::Drop
        );
        assert_eq!(
            extra_field_policy(IrTargetProtocol::AnthropicMessages, "store", &json!(true)).decision,
            FeatureDecision::Reject
        );
        assert_eq!(
            extra_field_policy(IrTargetProtocol::OpenAiResponses, "metadata", &json!({})).decision,
            FeatureDecision::PassThrough
        );
        assert_eq!(
            extra_field_policy(
                IrTargetProtocol::OpenAiResponses,
                "output_config",
                &json!({})
            )
            .decision,
            FeatureDecision::Emulate
        );
        assert_eq!(
            extra_field_policy(IrTargetProtocol::OpenAiChat, "unknown_vendor", &json!(1)).decision,
            FeatureDecision::PassThrough
        );
    }

    #[test]
    fn filters_extra_fields_with_explicit_drop_emulate_and_reject_actions() {
        let extra = Map::from_iter([
            ("metadata".to_owned(), json!({ "trace": "ok" })),
            ("container".to_owned(), json!({ "id": "anthropic-only" })),
            (
                "text".to_owned(),
                json!({ "format": { "type": "json_object" } }),
            ),
            ("store".to_owned(), json!(false)),
        ]);

        let forwarded =
            passthrough_extra_fields(IrTargetProtocol::AnthropicMessages, &extra, &[], &[])
                .unwrap();

        assert_eq!(
            forwarded,
            Map::from_iter([
                ("metadata".to_owned(), json!({ "trace": "ok" })),
                ("container".to_owned(), json!({ "id": "anthropic-only" }))
            ])
        );

        let mut rejected = extra;
        rejected.insert("previous_response_id".to_owned(), json!("resp_1"));
        let error =
            passthrough_extra_fields(IrTargetProtocol::AnthropicMessages, &rejected, &[], &[])
                .unwrap_err();

        assert!(
            matches!(error, ProxyError::UnsupportedFeature { feature, protocol } if feature == "request.extra.previous_response_id" && protocol == "anthropic")
        );
    }

    #[test]
    fn converts_responses_json_schema_to_chat_response_format() {
        let extra = Map::from_iter([(
            "text".to_owned(),
            json!({
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "description": "Final answer shape",
                    "schema": {
                        "type": "object",
                        "properties": { "answer": { "type": "string" } },
                        "required": ["answer"]
                    },
                    "strict": true
                }
            }),
        )]);

        assert_eq!(
            chat_response_format_from_extra(&extra).unwrap(),
            Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "answer",
                    "description": "Final answer shape",
                    "schema": {
                        "type": "object",
                        "properties": { "answer": { "type": "string" } },
                        "required": ["answer"]
                    },
                    "strict": true
                }
            }))
        );
    }

    #[test]
    fn converts_chat_response_format_to_responses_text_format() {
        let extra = Map::from_iter([(
            "response_format".to_owned(),
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "answer",
                    "schema": { "type": "object" }
                }
            }),
        )]);

        assert_eq!(
            responses_text_format_from_extra(&extra).unwrap(),
            Some(json!({
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object" }
                }
            }))
        );
    }

    #[test]
    fn creates_anthropic_structured_output_tool_from_json_schema() {
        let extra = Map::from_iter([(
            "text".to_owned(),
            json!({
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object", "properties": { "answer": { "type": "string" } } }
                }
            }),
        )]);

        let tool = anthropic_structured_output_tool(&extra).unwrap().unwrap();

        assert_eq!(tool.name, ANTHROPIC_STRUCTURED_OUTPUT_TOOL_NAME);
        assert_eq!(
            tool.input_schema,
            json!({ "type": "object", "properties": { "answer": { "type": "string" } } })
        );
        assert!(
            tool.description
                .unwrap()
                .contains("Return the final answer by calling this tool")
        );
    }
}
