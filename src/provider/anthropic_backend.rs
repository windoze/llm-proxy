//! HTTP client for Anthropic Messages-compatible upstream backends.

// Later M6 route assembly wires this client into the Responses-to-Anthropic bridge.
#![allow(dead_code)]

use serde_json::Value;

use crate::{
    error::{ProxyError, Result},
    provider::anthropic_cache::{
        AnthropicCacheControlInjection, prepare_anthropic_request_body_with_cache_control,
    },
};

const X_API_KEY_HEADER: &str = "x-api-key";
const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";

/// Client for sending already-encoded Anthropic Messages API requests upstream.
#[derive(Clone, Debug)]
pub struct AnthropicBackendClient {
    http_client: reqwest::Client,
    endpoint: reqwest::Url,
    api_key: String,
    anthropic_version: String,
    cache_control: AnthropicCacheControlInjection,
}

impl AnthropicBackendClient {
    /// Creates an Anthropic backend client using a default reqwest client.
    pub fn new(
        endpoint: impl AsRef<str>,
        api_key: impl Into<String>,
        anthropic_version: impl Into<String>,
    ) -> Result<Self> {
        Self::with_http_client(reqwest::Client::new(), endpoint, api_key, anthropic_version)
    }

    /// Creates an Anthropic backend client with an explicit reqwest client for tests/configuration.
    pub fn with_http_client(
        http_client: reqwest::Client,
        endpoint: impl AsRef<str>,
        api_key: impl Into<String>,
        anthropic_version: impl Into<String>,
    ) -> Result<Self> {
        let endpoint = endpoint.as_ref();
        let endpoint = reqwest::Url::parse(endpoint).map_err(|err| {
            ProxyError::Config(format!("invalid Anthropic backend URL `{endpoint}`: {err}"))
        })?;

        let api_key = api_key.into();
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ProxyError::Config(
                "Anthropic backend API key must not be empty".to_owned(),
            ));
        }

        let anthropic_version = anthropic_version.into();
        let anthropic_version = anthropic_version.trim();
        if anthropic_version.is_empty() {
            return Err(ProxyError::Config(
                "Anthropic backend version must not be empty".to_owned(),
            ));
        }

        Ok(Self {
            http_client,
            endpoint,
            api_key: api_key.to_owned(),
            anthropic_version: anthropic_version.to_owned(),
            cache_control: AnthropicCacheControlInjection::Disabled,
        })
    }

    /// Enables or disables stateless Anthropic prompt-cache breakpoint injection.
    pub fn with_cache_control_injection(
        mut self,
        cache_control: AnthropicCacheControlInjection,
    ) -> Self {
        self.cache_control = cache_control;
        self
    }

    /// Sends an Anthropic request and leaves the response body available as `bytes_stream()`.
    pub async fn send(&self, body: Value) -> Result<reqwest::Response> {
        let body = prepare_anthropic_request_body_with_cache_control(body, self.cache_control)?;
        let response = self
            .http_client
            .post(self.endpoint.clone())
            .header(X_API_KEY_HEADER, &self.api_key)
            .header(ANTHROPIC_VERSION_HEADER, &self.anthropic_version)
            .json(&body)
            .send()
            .await?;

        ensure_upstream_success(response).await
    }

    /// Returns the configured Anthropic Messages API endpoint.
    pub fn endpoint(&self) -> &reqwest::Url {
        &self.endpoint
    }

    /// Returns the configured Anthropic API version header value.
    pub fn anthropic_version(&self) -> &str {
        &self.anthropic_version
    }
}

async fn ensure_upstream_success(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await?;
    Err(ProxyError::Upstream4xx { status, body })
}

#[cfg(test)]
mod tests {
    use futures_util::TryStreamExt;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;

    #[test]
    fn rejects_invalid_endpoint_url() {
        let err = AnthropicBackendClient::new("not a url", "key", "2023-06-01").unwrap_err();

        match err {
            ProxyError::Config(message) => {
                assert!(message.contains("invalid Anthropic backend URL"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn rejects_empty_api_key() {
        let err = AnthropicBackendClient::new(
            "https://api.anthropic.com/v1/messages",
            "   ",
            "2023-06-01",
        )
        .unwrap_err();

        match err {
            ProxyError::Config(message) => {
                assert!(message.contains("API key must not be empty"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn rejects_empty_anthropic_version() {
        let err =
            AnthropicBackendClient::new("https://api.anthropic.com/v1/messages", "key", "   ")
                .unwrap_err();

        match err {
            ProxyError::Config(message) => {
                assert!(message.contains("version must not be empty"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn posts_anthropic_headers_json_and_keeps_response_streaming() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header(X_API_KEY_HEADER, "test-anthropic-key"))
            .and(header(ANTHROPIC_VERSION_HEADER, "2023-06-01"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("event: message_stop\n\ndata: {}\n\n"),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let client = AnthropicBackendClient::new(
            format!("{}/v1/messages", upstream.uri()),
            " test-anthropic-key ",
            " 2023-06-01 ",
        )
        .unwrap();
        let response = client
            .send(json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 128,
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "stream": true
            }))
            .await
            .unwrap();

        let body = response
            .bytes_stream()
            .try_fold(Vec::new(), |mut body, chunk| async move {
                body.extend_from_slice(&chunk);
                Ok::<Vec<u8>, reqwest::Error>(body)
            })
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "event: message_stop\n\ndata: {}\n\n"
        );

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request_body: Value = requests[0].body_json().unwrap();
        assert_eq!(
            request_body,
            json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 128,
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "stream": true
            })
        );
    }

    #[tokio::test]
    async fn client_can_enable_cache_control_injection() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header(X_API_KEY_HEADER, "test-anthropic-key"))
            .and(header(ANTHROPIC_VERSION_HEADER, "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let client = AnthropicBackendClient::new(
            format!("{}/v1/messages", upstream.uri()),
            "test-anthropic-key",
            "2023-06-01",
        )
        .unwrap()
        .with_cache_control_injection(AnthropicCacheControlInjection::EphemeralBreakpoints);

        client
            .send(json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 128,
                "messages": [
                    {
                        "role": "user",
                        "content": "first turn"
                    },
                    {
                        "role": "assistant",
                        "content": [{
                            "type": "text",
                            "text": "assistant turn"
                        }]
                    },
                    {
                        "role": "user",
                        "content": [{
                            "type": "text",
                            "text": "latest turn"
                        }]
                    }
                ]
            }))
            .await
            .unwrap();

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request_body: Value = requests[0].body_json().unwrap();
        assert_eq!(
            request_body["messages"][0]["content"],
            json!([{
                "type": "text",
                "text": "first turn",
                "cache_control": { "type": "ephemeral" }
            }])
        );
        assert_eq!(
            request_body["messages"][1]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert_eq!(
            request_body["messages"][2]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }

    #[tokio::test]
    async fn surfaces_upstream_error_body() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid request"))
            .expect(1)
            .mount(&upstream)
            .await;

        let client = AnthropicBackendClient::new(
            format!("{}/v1/messages", upstream.uri()),
            "key",
            "2023-06-01",
        )
        .unwrap();
        let err = client
            .send(json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 128,
                "messages": []
            }))
            .await
            .unwrap_err();

        match err {
            ProxyError::Upstream4xx { status, body } => {
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
                assert_eq!(body, "invalid request");
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
