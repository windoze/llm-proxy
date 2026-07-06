//! Error types shared across the proxy.

use axum::{
    Json,
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

/// Convenience result type used throughout the proxy.
pub type Result<T> = std::result::Result<T, ProxyError>;

/// Unified error type for request decoding, protocol mapping, and upstream calls.
#[derive(Debug, Error)]
pub enum ProxyError {
    /// Failure while sending a request to an upstream provider or reading its response.
    #[error("upstream HTTP request failed: {0}")]
    UpstreamHttp(#[from] reqwest::Error),

    /// Failure while decoding or encoding JSON payloads.
    #[error("failed to deserialize payload: {0}")]
    Deserialize(#[from] serde_json::Error),

    /// A requested protocol feature is not supported by the selected backend.
    #[error("unsupported feature `{feature}` for protocol `{protocol}`")]
    UnsupportedFeature { feature: String, protocol: String },

    /// Protocol conversion failed because the input cannot be represented faithfully.
    #[error("protocol mapping failed: {0}")]
    ProtocolMapping(String),

    /// Invalid or incomplete proxy configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// The downstream client failed proxy-level API key authentication.
    #[error("authentication failed: {0}")]
    Unauthorized(String),

    /// The upstream provider returned a non-success HTTP response.
    #[error("upstream returned status {status}: {body}")]
    UpstreamStatus {
        status: StatusCode,
        body: String,
        headers: Box<HeaderMap>,
    },
}

/// Frontend protocol whose public error schema should be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorProtocol {
    Generic,
    Anthropic,
    Responses,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorPayload,
}

#[derive(Debug, Serialize)]
struct ErrorPayload {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct AnthropicErrorBody {
    r#type: &'static str,
    error: AnthropicErrorPayload,
}

#[derive(Debug, Serialize)]
struct AnthropicErrorPayload {
    r#type: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ResponsesErrorBody {
    error: ResponsesErrorPayload,
}

#[derive(Debug, Serialize)]
struct ResponsesErrorPayload {
    message: String,
    r#type: &'static str,
    param: Option<&'static str>,
    code: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    InvalidRequest,
    Authentication,
    Permission,
    NotFound,
    RateLimit,
    Server,
}

impl ProxyError {
    /// Creates an upstream status error while preserving headers needed for frontend translation.
    pub fn upstream_status(status: StatusCode, headers: &HeaderMap, body: String) -> Self {
        Self::UpstreamStatus {
            status,
            body,
            headers: Box::new(headers.clone()),
        }
    }

    /// Formats this error as an Anthropic Messages API error response.
    pub fn into_anthropic_response(self) -> Response {
        self.into_protocol_response(ErrorProtocol::Anthropic)
    }

    /// Formats this error as an OpenAI Responses API error response.
    pub fn into_responses_response(self) -> Response {
        self.into_protocol_response(ErrorProtocol::Responses)
    }

    /// Formats this error using the selected frontend protocol's public error schema.
    pub fn into_protocol_response(self, protocol: ErrorProtocol) -> Response {
        let status = self.status_code();
        let mut response = match protocol {
            ErrorProtocol::Generic => self.generic_response(status),
            ErrorProtocol::Anthropic => self.anthropic_response(status),
            ErrorProtocol::Responses => self.responses_response(status),
        };

        for (name, value) in self.forwarded_headers(protocol) {
            if let Some(name) = name {
                response.headers_mut().insert(name, value);
            }
        }

        response
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::UpstreamHttp(_) => StatusCode::BAD_GATEWAY,
            Self::Deserialize(_) => StatusCode::BAD_REQUEST,
            Self::UnsupportedFeature { .. } => StatusCode::BAD_REQUEST,
            Self::ProtocolMapping(_) => StatusCode::BAD_REQUEST,
            Self::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::UpstreamStatus { status, .. } => *status,
        }
    }

    fn error_code(&self) -> &'static str {
        match self {
            Self::UpstreamHttp(_) => "upstream_http",
            Self::Deserialize(_) => "deserialize",
            Self::UnsupportedFeature { .. } => "unsupported_feature",
            Self::ProtocolMapping(_) => "protocol_mapping",
            Self::Config(_) => "config",
            Self::Unauthorized(_) => "unauthorized",
            Self::UpstreamStatus { status, .. } if *status == StatusCode::TOO_MANY_REQUESTS => {
                "upstream_rate_limit"
            }
            Self::UpstreamStatus { status, .. } if status.is_server_error() => "upstream_5xx",
            Self::UpstreamStatus { .. } => "upstream_4xx",
        }
    }

    fn error_kind(&self) -> ErrorKind {
        match self {
            Self::Deserialize(_) | Self::UnsupportedFeature { .. } | Self::ProtocolMapping(_) => {
                ErrorKind::InvalidRequest
            }
            Self::Config(_) | Self::UpstreamHttp(_) => ErrorKind::Server,
            Self::Unauthorized(_) => ErrorKind::Authentication,
            Self::UpstreamStatus { status, .. } => match *status {
                StatusCode::UNAUTHORIZED => ErrorKind::Authentication,
                StatusCode::FORBIDDEN => ErrorKind::Permission,
                StatusCode::NOT_FOUND => ErrorKind::NotFound,
                StatusCode::TOO_MANY_REQUESTS => ErrorKind::RateLimit,
                status if status.is_server_error() => ErrorKind::Server,
                _ => ErrorKind::InvalidRequest,
            },
        }
    }

    fn message(&self) -> String {
        match self {
            Self::UpstreamStatus { status, body, .. } => {
                let upstream_message = upstream_error_message(body);
                if upstream_message.is_empty() {
                    format!("upstream returned status {status}")
                } else {
                    format!("upstream returned status {status}: {upstream_message}")
                }
            }
            _ => self.to_string(),
        }
    }

    fn generic_response(&self, status: StatusCode) -> Response {
        let body = ErrorBody {
            error: ErrorPayload {
                code: self.error_code(),
                message: self.message(),
            },
        };

        (status, Json(body)).into_response()
    }

    fn anthropic_response(&self, status: StatusCode) -> Response {
        let body = AnthropicErrorBody {
            r#type: "error",
            error: AnthropicErrorPayload {
                r#type: self.anthropic_error_type(),
                message: self.message(),
            },
        };

        (status, Json(body)).into_response()
    }

    fn responses_response(&self, status: StatusCode) -> Response {
        let body = ResponsesErrorBody {
            error: ResponsesErrorPayload {
                message: self.message(),
                r#type: self.responses_error_type(),
                param: None,
                code: Some(self.error_code()),
            },
        };

        (status, Json(body)).into_response()
    }

    fn anthropic_error_type(&self) -> &'static str {
        match self.error_kind() {
            ErrorKind::InvalidRequest => "invalid_request_error",
            ErrorKind::Authentication => "authentication_error",
            ErrorKind::Permission => "permission_error",
            ErrorKind::NotFound => "not_found_error",
            ErrorKind::RateLimit => "rate_limit_error",
            ErrorKind::Server => "api_error",
        }
    }

    fn responses_error_type(&self) -> &'static str {
        match self.error_kind() {
            ErrorKind::InvalidRequest => "invalid_request_error",
            ErrorKind::Authentication => "authentication_error",
            ErrorKind::Permission => "permission_error",
            ErrorKind::NotFound => "not_found_error",
            ErrorKind::RateLimit => "rate_limit_error",
            ErrorKind::Server => "server_error",
        }
    }

    fn forwarded_headers(&self, protocol: ErrorProtocol) -> HeaderMap {
        let mut forwarded = HeaderMap::new();
        let Self::UpstreamStatus { headers, .. } = self else {
            return forwarded;
        };

        copy_if_present(
            headers,
            &mut forwarded,
            header::RETRY_AFTER,
            header::RETRY_AFTER,
        );
        match protocol {
            ErrorProtocol::Generic => {
                copy_native_headers(headers, &mut forwarded, OPENAI_RATE_LIMIT_HEADERS);
                copy_native_headers(headers, &mut forwarded, ANTHROPIC_RATE_LIMIT_HEADERS);
            }
            ErrorProtocol::Anthropic => {
                copy_native_headers(headers, &mut forwarded, ANTHROPIC_RATE_LIMIT_HEADERS);
                copy_translated_headers(headers, &mut forwarded, OPENAI_TO_ANTHROPIC_RATE_LIMIT);
            }
            ErrorProtocol::Responses => {
                copy_native_headers(headers, &mut forwarded, OPENAI_RATE_LIMIT_HEADERS);
                copy_translated_headers(headers, &mut forwarded, ANTHROPIC_TO_OPENAI_RATE_LIMIT);
            }
        }

        forwarded
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        self.into_protocol_response(ErrorProtocol::Generic)
    }
}

const OPENAI_RATE_LIMIT_HEADERS: &[&str] = &[
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-reset-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "x-ratelimit-reset-tokens",
    "x-ratelimit-limit-input-tokens",
    "x-ratelimit-remaining-input-tokens",
    "x-ratelimit-reset-input-tokens",
    "x-ratelimit-limit-output-tokens",
    "x-ratelimit-remaining-output-tokens",
    "x-ratelimit-reset-output-tokens",
];

const ANTHROPIC_RATE_LIMIT_HEADERS: &[&str] = &[
    "anthropic-ratelimit-requests-limit",
    "anthropic-ratelimit-requests-remaining",
    "anthropic-ratelimit-requests-reset",
    "anthropic-ratelimit-tokens-limit",
    "anthropic-ratelimit-tokens-remaining",
    "anthropic-ratelimit-tokens-reset",
    "anthropic-ratelimit-input-tokens-limit",
    "anthropic-ratelimit-input-tokens-remaining",
    "anthropic-ratelimit-input-tokens-reset",
    "anthropic-ratelimit-output-tokens-limit",
    "anthropic-ratelimit-output-tokens-remaining",
    "anthropic-ratelimit-output-tokens-reset",
];

const OPENAI_TO_ANTHROPIC_RATE_LIMIT: &[(&str, &str)] = &[
    (
        "x-ratelimit-limit-requests",
        "anthropic-ratelimit-requests-limit",
    ),
    (
        "x-ratelimit-remaining-requests",
        "anthropic-ratelimit-requests-remaining",
    ),
    (
        "x-ratelimit-reset-requests",
        "anthropic-ratelimit-requests-reset",
    ),
    (
        "x-ratelimit-limit-tokens",
        "anthropic-ratelimit-tokens-limit",
    ),
    (
        "x-ratelimit-remaining-tokens",
        "anthropic-ratelimit-tokens-remaining",
    ),
    (
        "x-ratelimit-reset-tokens",
        "anthropic-ratelimit-tokens-reset",
    ),
    (
        "x-ratelimit-limit-input-tokens",
        "anthropic-ratelimit-input-tokens-limit",
    ),
    (
        "x-ratelimit-remaining-input-tokens",
        "anthropic-ratelimit-input-tokens-remaining",
    ),
    (
        "x-ratelimit-reset-input-tokens",
        "anthropic-ratelimit-input-tokens-reset",
    ),
    (
        "x-ratelimit-limit-output-tokens",
        "anthropic-ratelimit-output-tokens-limit",
    ),
    (
        "x-ratelimit-remaining-output-tokens",
        "anthropic-ratelimit-output-tokens-remaining",
    ),
    (
        "x-ratelimit-reset-output-tokens",
        "anthropic-ratelimit-output-tokens-reset",
    ),
];

const ANTHROPIC_TO_OPENAI_RATE_LIMIT: &[(&str, &str)] = &[
    (
        "anthropic-ratelimit-requests-limit",
        "x-ratelimit-limit-requests",
    ),
    (
        "anthropic-ratelimit-requests-remaining",
        "x-ratelimit-remaining-requests",
    ),
    (
        "anthropic-ratelimit-requests-reset",
        "x-ratelimit-reset-requests",
    ),
    (
        "anthropic-ratelimit-tokens-limit",
        "x-ratelimit-limit-tokens",
    ),
    (
        "anthropic-ratelimit-tokens-remaining",
        "x-ratelimit-remaining-tokens",
    ),
    (
        "anthropic-ratelimit-tokens-reset",
        "x-ratelimit-reset-tokens",
    ),
    (
        "anthropic-ratelimit-input-tokens-limit",
        "x-ratelimit-limit-input-tokens",
    ),
    (
        "anthropic-ratelimit-input-tokens-remaining",
        "x-ratelimit-remaining-input-tokens",
    ),
    (
        "anthropic-ratelimit-input-tokens-reset",
        "x-ratelimit-reset-input-tokens",
    ),
    (
        "anthropic-ratelimit-output-tokens-limit",
        "x-ratelimit-limit-output-tokens",
    ),
    (
        "anthropic-ratelimit-output-tokens-remaining",
        "x-ratelimit-remaining-output-tokens",
    ),
    (
        "anthropic-ratelimit-output-tokens-reset",
        "x-ratelimit-reset-output-tokens",
    ),
];

fn upstream_error_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => value
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or(trimmed)
            .to_owned(),
        Err(_) => trimmed.to_owned(),
    }
}

fn copy_native_headers(source: &HeaderMap, target: &mut HeaderMap, names: &[&'static str]) {
    for name in names {
        let name = HeaderName::from_static(name);
        copy_if_present(source, target, name.clone(), name);
    }
}

fn copy_translated_headers(
    source: &HeaderMap,
    target: &mut HeaderMap,
    translations: &[(&'static str, &'static str)],
) {
    for (source_name, target_name) in translations {
        copy_if_present(
            source,
            target,
            HeaderName::from_static(source_name),
            HeaderName::from_static(target_name),
        );
    }
}

fn copy_if_present(
    source: &HeaderMap,
    target: &mut HeaderMap,
    source_name: HeaderName,
    target_name: HeaderName,
) {
    if target.contains_key(&target_name) {
        return;
    }
    if let Some(value) = source.get(&source_name) {
        target.insert(target_name, value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::HeaderValue;
    use serde_json::json;

    #[tokio::test]
    async fn maps_config_errors_to_json_response() {
        let response = ProxyError::Config("missing upstream URL".to_owned()).into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "error": {
                    "code": "config",
                    "message": "configuration error: missing upstream URL"
                }
            })
        );
    }

    #[tokio::test]
    async fn preserves_upstream_client_error_status() {
        let response = ProxyError::upstream_status(
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            "rate limited".to_owned(),
        )
        .into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn formats_anthropic_error_body() {
        let response = ProxyError::UnsupportedFeature {
            feature: "json_schema".to_owned(),
            protocol: "anthropic".to_owned(),
        }
        .into_anthropic_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "unsupported feature `json_schema` for protocol `anthropic`"
                }
            })
        );
    }

    #[tokio::test]
    async fn formats_responses_error_body() {
        let response = ProxyError::Config("missing backend".to_owned()).into_responses_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "error": {
                    "message": "configuration error: missing backend",
                    "type": "server_error",
                    "param": null,
                    "code": "config"
                }
            })
        );
    }

    #[tokio::test]
    async fn translates_openai_rate_limit_headers_for_anthropic_errors() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(header::RETRY_AFTER, HeaderValue::from_static("12"));
        upstream_headers.insert(
            HeaderName::from_static("x-ratelimit-remaining-requests"),
            HeaderValue::from_static("0"),
        );

        let response = ProxyError::upstream_status(
            StatusCode::TOO_MANY_REQUESTS,
            &upstream_headers,
            r#"{"error":{"message":"slow down"}}"#.to_owned(),
        )
        .into_anthropic_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "12");
        assert_eq!(
            response
                .headers()
                .get("anthropic-ratelimit-requests-remaining")
                .unwrap(),
            "0"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "rate_limit_error");
        assert_eq!(
            value["error"]["message"],
            "upstream returned status 429 Too Many Requests: slow down"
        );
    }

    #[tokio::test]
    async fn translates_anthropic_rate_limit_headers_for_responses_errors() {
        let mut upstream_headers = HeaderMap::new();
        upstream_headers.insert(
            HeaderName::from_static("anthropic-ratelimit-tokens-reset"),
            HeaderValue::from_static("2026-07-06T00:00:00Z"),
        );

        let response = ProxyError::upstream_status(
            StatusCode::SERVICE_UNAVAILABLE,
            &upstream_headers,
            r#"{"error":{"message":"overloaded"}}"#.to_owned(),
        )
        .into_responses_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get("x-ratelimit-reset-tokens").unwrap(),
            "2026-07-06T00:00:00Z"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "server_error");
        assert_eq!(value["error"]["code"], "upstream_5xx");
    }
}
