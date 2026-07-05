//! Request-scoped structured logging and redacted debug dumps.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use axum::http::{HeaderMap, HeaderName};
use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use crate::{
    config::BackendKind,
    error,
    ir::{self, event::IrEvent},
    provider::router::{FrontendEndpoint, ModelRoute},
};

const REDACTED: &str = "******";
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Per-request observability context used for structured logs and optional body dumps.
#[derive(Clone, Debug)]
pub(crate) struct RequestObservation {
    request_id: u64,
    frontend: &'static str,
    started: Instant,
    dump_bodies: bool,
    route: Option<RouteObservation>,
}

#[derive(Clone, Debug)]
struct RouteObservation {
    chain: &'static str,
    backend_name: String,
    backend_kind: &'static str,
    requested_model: String,
    backend_model: String,
    stream: bool,
}

impl RequestObservation {
    /// Creates a new request-scoped log context.
    pub(crate) fn new(frontend: FrontendEndpoint, dump_bodies: bool) -> Self {
        Self {
            request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            frontend: frontend_path(frontend),
            started: Instant::now(),
            dump_bodies,
            route: None,
        }
    }

    /// Records the selected upstream route once model routing has completed.
    pub(crate) fn set_route(&mut self, route: &ModelRoute<'_>, stream: bool) {
        self.route = Some(RouteObservation {
            chain: chain_label(route.endpoint(), route.backend_kind()),
            backend_name: route.backend().name.clone(),
            backend_kind: backend_kind_label(route.backend_kind()),
            requested_model: route.requested_model().to_owned(),
            backend_model: route.backend_model().to_owned(),
            stream,
        });
    }

    /// Writes a redacted frontend request dump when debug dumps are enabled.
    pub(crate) fn dump_frontend_request(&self, headers: &HeaderMap, body: &Value) {
        if !self.dump_bodies {
            return;
        }

        debug!(
            request_id = self.request_id,
            frontend = self.frontend,
            headers = %redacted_json_string(&redacted_headers(headers)),
            body = %redacted_json_string(body),
            "frontend request dump"
        );
    }

    /// Writes a redacted JSON body dump for upstream or frontend payloads.
    pub(crate) fn dump_json(&self, direction: &'static str, body: &Value) {
        if !self.dump_bodies {
            return;
        }

        debug!(
            request_id = self.request_id,
            frontend = self.frontend,
            chain = self.chain(),
            backend = self.backend_name(),
            direction,
            body = %redacted_json_string(body),
            "observability body dump"
        );
    }

    /// Logs successful completion with normalized token usage when available.
    pub(crate) fn log_success(&self, usage: Option<&ir::request::Usage>) {
        let token_usage = TokenUsageLog::from_usage(usage);
        info!(
            request_id = self.request_id,
            frontend = self.frontend,
            chain = self.chain(),
            backend = self.backend_name(),
            backend_kind = self.backend_kind(),
            requested_model = self.requested_model(),
            backend_model = self.backend_model(),
            stream = self.stream(),
            elapsed_ms = self.elapsed_ms(),
            usage_available = token_usage.available,
            input_tokens = token_usage.input_tokens,
            output_tokens = token_usage.output_tokens,
            cache_read_tokens = token_usage.cache_read_tokens,
            cache_write_tokens = token_usage.cache_write_tokens,
            "request completed"
        );
    }

    /// Logs a request failure before an HTTP error response is formatted.
    pub(crate) fn log_error(&self, error: &error::ProxyError) {
        warn!(
            request_id = self.request_id,
            frontend = self.frontend,
            chain = self.chain(),
            backend = self.backend_name(),
            backend_kind = self.backend_kind(),
            requested_model = self.requested_model(),
            backend_model = self.backend_model(),
            stream = self.stream(),
            elapsed_ms = self.elapsed_ms(),
            error = %error,
            "request failed"
        );
    }

    /// Logs an error that occurs after a streaming HTTP response has started.
    fn log_stream_error(&self, error: &error::ProxyError, usage: Option<&ir::request::Usage>) {
        let token_usage = TokenUsageLog::from_usage(usage);
        warn!(
            request_id = self.request_id,
            frontend = self.frontend,
            chain = self.chain(),
            backend = self.backend_name(),
            backend_kind = self.backend_kind(),
            requested_model = self.requested_model(),
            backend_model = self.backend_model(),
            stream = true,
            elapsed_ms = self.elapsed_ms(),
            usage_available = token_usage.available,
            input_tokens = token_usage.input_tokens,
            output_tokens = token_usage.output_tokens,
            cache_read_tokens = token_usage.cache_read_tokens,
            cache_write_tokens = token_usage.cache_write_tokens,
            error = %error,
            "stream request failed"
        );
    }

    /// Logs a stream that ended without a terminal IR event.
    fn log_stream_end_without_terminal(&self, usage: Option<&ir::request::Usage>) {
        let token_usage = TokenUsageLog::from_usage(usage);
        warn!(
            request_id = self.request_id,
            frontend = self.frontend,
            chain = self.chain(),
            backend = self.backend_name(),
            backend_kind = self.backend_kind(),
            requested_model = self.requested_model(),
            backend_model = self.backend_model(),
            stream = true,
            elapsed_ms = self.elapsed_ms(),
            usage_available = token_usage.available,
            input_tokens = token_usage.input_tokens,
            output_tokens = token_usage.output_tokens,
            cache_read_tokens = token_usage.cache_read_tokens,
            cache_write_tokens = token_usage.cache_write_tokens,
            "stream ended without terminal message_stop"
        );
    }

    fn chain(&self) -> &str {
        self.route
            .as_ref()
            .map(|route| route.chain)
            .unwrap_or("unresolved")
    }

    fn backend_name(&self) -> &str {
        self.route
            .as_ref()
            .map(|route| route.backend_name.as_str())
            .unwrap_or("unresolved")
    }

    fn backend_kind(&self) -> &str {
        self.route
            .as_ref()
            .map(|route| route.backend_kind)
            .unwrap_or("unresolved")
    }

    fn requested_model(&self) -> &str {
        self.route
            .as_ref()
            .map(|route| route.requested_model.as_str())
            .unwrap_or("unknown")
    }

    fn backend_model(&self) -> &str {
        self.route
            .as_ref()
            .map(|route| route.backend_model.as_str())
            .unwrap_or("unknown")
    }

    fn stream(&self) -> bool {
        self.route
            .as_ref()
            .map(|route| route.stream)
            .unwrap_or(false)
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

#[derive(Debug)]
struct TokenUsageLog {
    available: bool,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
}

impl TokenUsageLog {
    /// Normalizes optional token usage into concrete fields suitable for structured logs.
    fn from_usage(usage: Option<&ir::request::Usage>) -> Self {
        match usage {
            Some(usage) => Self {
                available: true,
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read.unwrap_or_default(),
                cache_write_tokens: usage.cache_write.unwrap_or_default(),
            },
            None => Self {
                available: false,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        }
    }
}

/// Observes streaming IR events for token usage and terminal completion logging.
pub(crate) fn observe_ir_event_stream<S>(
    events: S,
    observation: RequestObservation,
) -> BoxStream<'static, error::Result<IrEvent>>
where
    S: Stream<Item = error::Result<IrEvent>> + Send + 'static,
{
    async_stream::stream! {
        let mut usage = None;
        let mut terminal_seen = false;
        let mut logged_failure = false;
        futures_util::pin_mut!(events);

        while let Some(event) = events.next().await {
            match &event {
                Ok(IrEvent::MessageDelta {
                    usage: Some(event_usage),
                    ..
                }) => {
                    usage = Some(event_usage.clone());
                }
                Ok(IrEvent::MessageStop) => {
                    terminal_seen = true;
                }
                Err(error) => {
                    observation.log_stream_error(error, usage.as_ref());
                    logged_failure = true;
                }
                _ => {}
            }

            yield event;
        }

        if logged_failure {
            return;
        }

        if terminal_seen {
            observation.log_success(usage.as_ref());
        } else {
            observation.log_stream_end_without_terminal(usage.as_ref());
        }
    }
    .boxed()
}

/// Returns the public path for a frontend endpoint.
fn frontend_path(endpoint: FrontendEndpoint) -> &'static str {
    match endpoint {
        FrontendEndpoint::AnthropicMessages => "/v1/messages",
        FrontendEndpoint::OpenAiResponses => "/v1/responses",
    }
}

/// Returns a stable chain label for route observability.
fn chain_label(endpoint: FrontendEndpoint, backend: BackendKind) -> &'static str {
    match (endpoint, backend) {
        (FrontendEndpoint::AnthropicMessages, BackendKind::Chat) => "anthropic_messages_to_chat",
        (FrontendEndpoint::AnthropicMessages, BackendKind::Responses) => {
            "anthropic_messages_to_responses"
        }
        (FrontendEndpoint::AnthropicMessages, BackendKind::Anthropic) => {
            "anthropic_messages_to_anthropic"
        }
        (FrontendEndpoint::OpenAiResponses, BackendKind::Chat) => "responses_to_chat",
        (FrontendEndpoint::OpenAiResponses, BackendKind::Responses) => "responses_to_responses",
        (FrontendEndpoint::OpenAiResponses, BackendKind::Anthropic) => "responses_to_anthropic",
    }
}

/// Returns a stable backend-kind label for structured logs.
fn backend_kind_label(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Chat => "chat",
        BackendKind::Responses => "responses",
        BackendKind::Anthropic => "anthropic",
    }
}

/// Formats JSON for debug dumps after recursively redacting sensitive fields.
fn redacted_json_string(value: &Value) -> String {
    serde_json::to_string(&redact_json_value(value))
        .unwrap_or_else(|_| "\"<unserializable>\"".to_owned())
}

/// Recursively redacts secrets from JSON dump payloads.
fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::Object(object) => {
            let mut redacted = Map::new();
            for (key, value) in object {
                let value = if is_sensitive_key(key) {
                    Value::String(REDACTED.to_owned())
                } else {
                    redact_json_value(value)
                };
                redacted.insert(key.clone(), value);
            }
            Value::Object(redacted)
        }
        other => other.clone(),
    }
}

/// Converts headers into a JSON object with credential-bearing values redacted.
fn redacted_headers(headers: &HeaderMap) -> Value {
    let mut redacted = Map::new();
    for (name, value) in headers {
        let value = if is_sensitive_header(name) {
            REDACTED.to_owned()
        } else {
            value
                .to_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|_| "<non-utf8>".to_owned())
        };
        redacted.insert(name.as_str().to_owned(), Value::String(value));
    }
    Value::Object(redacted)
}

/// Returns true when a header name usually carries credentials.
fn is_sensitive_header(name: &HeaderName) -> bool {
    is_sensitive_key(name.as_str())
}

/// Returns true when a JSON field name usually carries credentials or opaque reasoning tokens.
fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "authorization"
            | "x_api_key"
            | "api_key"
            | "apikey"
            | "token"
            | "access_token"
            | "refresh_token"
            | "auth_token"
            | "bearer_token"
            | "cookie"
            | "set_cookie"
            | "password"
            | "secret"
            | "credential"
            | "encrypted_content"
            | "signature"
    ) || normalized.ends_with("_api_key")
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
}

#[cfg(test)]
mod tests {
    use axum::http::{
        HeaderMap, HeaderValue,
        header::{AUTHORIZATION, CONTENT_TYPE},
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_sensitive_json_fields_recursively() {
        let value = json!({
            "api_key": "secret",
            "input_tokens": 7,
            "nested": {
                "authorization": "Bearer secret",
                "encrypted_content": "opaque-reasoning",
                "signature": "provider-signature"
            },
            "items": [{
                "auth_token": "token-secret",
                "text": "safe"
            }]
        });

        assert_eq!(
            redact_json_value(&value),
            json!({
                "api_key": REDACTED,
                "input_tokens": 7,
                "nested": {
                    "authorization": REDACTED,
                    "encrypted_content": REDACTED,
                    "signature": REDACTED
                },
                "items": [{
                    "auth_token": REDACTED,
                    "text": "safe"
                }]
            })
        );
    }

    #[test]
    fn redacts_sensitive_headers_only() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert("x-api-key", HeaderValue::from_static("client-secret"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        assert_eq!(
            redacted_headers(&headers),
            json!({
                "authorization": REDACTED,
                "x-api-key": REDACTED,
                "content-type": "application/json"
            })
        );
    }
}
