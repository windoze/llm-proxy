//! HTTP client for OpenAI Responses-compatible upstream backends.

// The route layer uses this client for the rich Responses backend bridge.
#![allow(dead_code)]

use serde_json::Value;

use crate::error::{ProxyError, Result};

const REQUIRED_REASONING_INCLUDE: &str = "reasoning.encrypted_content";

/// Client for sending already-encoded Responses API requests to an upstream backend.
#[derive(Clone, Debug)]
pub struct ResponsesBackendClient {
    http_client: reqwest::Client,
    endpoint: reqwest::Url,
    api_key: String,
}

impl ResponsesBackendClient {
    /// Creates a Responses backend client using a default reqwest client.
    pub fn new(endpoint: impl AsRef<str>, api_key: impl Into<String>) -> Result<Self> {
        Self::with_http_client(reqwest::Client::new(), endpoint, api_key)
    }

    /// Creates a Responses backend client with an explicit reqwest client for tests/configuration.
    pub fn with_http_client(
        http_client: reqwest::Client,
        endpoint: impl AsRef<str>,
        api_key: impl Into<String>,
    ) -> Result<Self> {
        let endpoint = endpoint.as_ref();
        let endpoint = reqwest::Url::parse(endpoint).map_err(|err| {
            ProxyError::Config(format!("invalid Responses backend URL `{endpoint}`: {err}"))
        })?;

        let api_key = api_key.into();
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ProxyError::Config(
                "Responses backend API key must not be empty".to_owned(),
            ));
        }

        Ok(Self {
            http_client,
            endpoint,
            api_key: api_key.to_owned(),
        })
    }

    /// Sends a Responses request and leaves the response body available as `bytes_stream()`.
    pub async fn send(&self, body: Value) -> Result<reqwest::Response> {
        let body = prepare_responses_request_body(body)?;
        let response = self
            .http_client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        ensure_upstream_success(response).await
    }

    /// Returns the configured Responses API endpoint.
    pub fn endpoint(&self) -> &reqwest::Url {
        &self.endpoint
    }
}

/// Forces the request flags required for stateless reasoning-token round trips.
pub fn prepare_responses_request_body(body: Value) -> Result<Value> {
    let mut object = body
        .as_object()
        .cloned()
        .ok_or_else(|| mapping_error("Responses backend request body must be a JSON object"))?;

    object.insert("store".to_owned(), Value::Bool(false));
    ensure_reasoning_include(&mut object)?;

    Ok(Value::Object(object))
}

fn ensure_reasoning_include(object: &mut serde_json::Map<String, Value>) -> Result<()> {
    let include = match object.remove("include") {
        Some(Value::Null) | None => Vec::new(),
        Some(Value::Array(include)) => include,
        Some(_) => {
            return Err(mapping_error(
                "Responses backend request include must be an array when present",
            ));
        }
    };

    let mut has_reasoning_include = false;
    let mut normalized = Vec::with_capacity(include.len() + 1);
    for (index, item) in include.into_iter().enumerate() {
        let Value::String(include_value) = item else {
            return Err(mapping_error(format!(
                "Responses backend request include[{index}] must be a string"
            )));
        };
        if include_value == REQUIRED_REASONING_INCLUDE {
            has_reasoning_include = true;
        }
        normalized.push(Value::String(include_value));
    }

    if !has_reasoning_include {
        normalized.push(Value::String(REQUIRED_REASONING_INCLUDE.to_owned()));
    }
    object.insert("include".to_owned(), Value::Array(normalized));

    Ok(())
}

async fn ensure_upstream_success(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let headers = response.headers().clone();
    let body = response.text().await?;
    Err(ProxyError::upstream_status(status, &headers, body))
}

fn mapping_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ProtocolMapping(message.into())
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
    fn forces_store_false_and_reasoning_include() {
        let prepared = prepare_responses_request_body(json!({
            "model": "gpt-5.1",
            "input": "hello",
            "store": true,
            "include": ["file_search_call.results"]
        }))
        .unwrap();

        assert_eq!(prepared["store"], json!(false));
        assert_eq!(
            prepared["include"],
            json!(["file_search_call.results", REQUIRED_REASONING_INCLUDE])
        );
    }

    #[test]
    fn does_not_duplicate_existing_reasoning_include() {
        let prepared = prepare_responses_request_body(json!({
            "model": "gpt-5.1",
            "input": "hello",
            "include": [REQUIRED_REASONING_INCLUDE]
        }))
        .unwrap();

        assert_eq!(prepared["include"], json!([REQUIRED_REASONING_INCLUDE]));
    }

    #[test]
    fn rejects_invalid_include_shape() {
        let err = prepare_responses_request_body(json!({
            "model": "gpt-5.1",
            "input": "hello",
            "include": [false]
        }))
        .unwrap_err();

        match err {
            ProxyError::ProtocolMapping(message) => {
                assert!(message.contains("include[0] must be a string"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn rejects_empty_api_key() {
        let err =
            ResponsesBackendClient::new("https://api.openai.com/v1/responses", "   ").unwrap_err();

        match err {
            ProxyError::Config(message) => {
                assert!(message.contains("API key must not be empty"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn posts_authenticated_json_and_keeps_response_streaming() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer responses-secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("event: response.completed\n\ndata: done\n\n"),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let client = ResponsesBackendClient::new(
            format!("{}/v1/responses", upstream.uri()),
            " responses-secret ",
        )
        .unwrap();
        let response = client
            .send(json!({
                "model": "gpt-5.1",
                "input": "hello",
                "store": true
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
            "event: response.completed\n\ndata: done\n\n"
        );

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request_body: Value = requests[0].body_json().unwrap();
        assert_eq!(request_body["store"], json!(false));
        assert_eq!(request_body["include"], json!([REQUIRED_REASONING_INCLUDE]));
    }

    #[tokio::test]
    async fn surfaces_upstream_error_body() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .expect(1)
            .mount(&upstream)
            .await;

        let client =
            ResponsesBackendClient::new(format!("{}/v1/responses", upstream.uri()), "key").unwrap();
        let err = client
            .send(json!({
                "model": "gpt-5.1",
                "input": "hello"
            }))
            .await
            .unwrap_err();

        match err {
            ProxyError::UpstreamStatus {
                status,
                body,
                headers: _,
            } => {
                assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(body, "rate limited");
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
