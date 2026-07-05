//! Error types shared across the proxy.

use axum::{
    Json,
    http::StatusCode,
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

    /// The upstream provider rejected the request with a client error.
    #[error("upstream rejected request with status {status}: {body}")]
    Upstream4xx { status: StatusCode, body: String },
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

impl ProxyError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::UpstreamHttp(_) => StatusCode::BAD_GATEWAY,
            Self::Deserialize(_) => StatusCode::BAD_REQUEST,
            Self::UnsupportedFeature { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::ProtocolMapping(_) => StatusCode::BAD_REQUEST,
            Self::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Upstream4xx { status, .. } => *status,
        }
    }

    fn error_code(&self) -> &'static str {
        match self {
            Self::UpstreamHttp(_) => "upstream_http",
            Self::Deserialize(_) => "deserialize",
            Self::UnsupportedFeature { .. } => "unsupported_feature",
            Self::ProtocolMapping(_) => "protocol_mapping",
            Self::Config(_) => "config",
            Self::Upstream4xx { .. } => "upstream_4xx",
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            error: ErrorPayload {
                code: self.error_code(),
                message: self.to_string(),
            },
        };

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
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
        let response = ProxyError::Upstream4xx {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".to_owned(),
        }
        .into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
