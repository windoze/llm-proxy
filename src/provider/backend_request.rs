//! Shared controls for outbound backend HTTP requests.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};
use serde::de::DeserializeOwned;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::sleep,
};
use tracing::warn;

use crate::{
    config::BackendRequestConfig,
    error::{ProxyError, Result},
};

/// Cloneable runtime controls applied to every backend request.
#[derive(Clone, Debug)]
pub struct BackendRequestControls {
    policy: BackendRequestPolicy,
    concurrency: Option<Arc<Semaphore>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BackendRequestPolicy {
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    timeout: Option<Duration>,
}

/// A successful upstream response plus the concurrency permit protecting its body stream.
pub struct BackendResponse {
    response: reqwest::Response,
    permit: Option<OwnedSemaphorePermit>,
}

impl fmt::Debug for BackendResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendResponse")
            .field("status", &self.response.status())
            .field("url", &self.response.url())
            .field("permit_held", &self.permit.is_some())
            .finish()
    }
}

impl BackendRequestControls {
    /// Builds backend request controls from validated runtime configuration.
    pub fn from_config(config: &BackendRequestConfig) -> Self {
        Self {
            policy: BackendRequestPolicy {
                max_retries: config.max_retries,
                initial_backoff: Duration::from_millis(config.initial_backoff_ms),
                max_backoff: Duration::from_millis(config.max_backoff_ms),
                timeout: config.timeout_ms.map(Duration::from_millis),
            },
            concurrency: config
                .concurrency_limit
                .map(|limit| Arc::new(Semaphore::new(limit))),
        }
    }

    /// Sends a backend request, retrying transient failures according to the configured policy.
    pub async fn send<F>(&self, build_request: F) -> Result<BackendResponse>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let permit = self.acquire_permit().await?;
        let response = self.send_with_retries(build_request).await?;
        Ok(BackendResponse { response, permit })
    }

    async fn acquire_permit(&self) -> Result<Option<OwnedSemaphorePermit>> {
        let Some(concurrency) = self.concurrency.as_ref() else {
            return Ok(None);
        };

        concurrency
            .clone()
            .acquire_owned()
            .await
            .map(Some)
            .map_err(|_| ProxyError::Config("backend concurrency limiter is closed".to_owned()))
    }

    async fn send_with_retries<F>(&self, build_request: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let mut attempt = 0;

        loop {
            let response = self.send_once(&build_request).await;
            match response {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    let retry_after = retry_after_delay(&headers);
                    let should_retry =
                        is_retryable_status(status) && attempt < self.policy.max_retries;
                    let delay = self.retry_delay(attempt, retry_after);
                    let body = response.text().await;

                    if should_retry {
                        if let Err(error) = body {
                            warn!(
                                attempt,
                                max_retries = self.policy.max_retries,
                                %status,
                                error = %error,
                                "failed to read retryable upstream error body before retry"
                            );
                        }
                        warn!(
                            attempt,
                            max_retries = self.policy.max_retries,
                            %status,
                            delay_ms = delay.as_millis(),
                            "retrying backend request after retryable status"
                        );
                        sleep(delay).await;
                        attempt += 1;
                        continue;
                    }

                    return Err(ProxyError::upstream_status(status, &headers, body?));
                }
                Err(error)
                    if is_retryable_transport_error(&error)
                        && attempt < self.policy.max_retries =>
                {
                    let delay = self.retry_delay(attempt, None);
                    warn!(
                        attempt,
                        max_retries = self.policy.max_retries,
                        error = %error,
                        delay_ms = delay.as_millis(),
                        "retrying backend request after transport error"
                    );
                    sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => return Err(ProxyError::UpstreamHttp(error)),
            }
        }
    }

    async fn send_once<F>(
        &self,
        build_request: &F,
    ) -> std::result::Result<reqwest::Response, reqwest::Error>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let request = build_request();
        let request = if let Some(timeout) = self.policy.timeout {
            request.timeout(timeout)
        } else {
            request
        };
        request.send().await
    }

    fn retry_delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        retry_after
            .unwrap_or_else(|| self.exponential_backoff(attempt))
            .min(self.policy.max_backoff)
    }

    fn exponential_backoff(&self, attempt: u32) -> Duration {
        let multiplier = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.policy
            .initial_backoff
            .saturating_mul(multiplier)
            .min(self.policy.max_backoff)
    }
}

impl Default for BackendRequestControls {
    fn default() -> Self {
        Self::from_config(&BackendRequestConfig::default())
    }
}

impl BackendResponse {
    /// Deserializes a non-streaming JSON response while holding the concurrency permit.
    pub async fn json<T: DeserializeOwned>(self) -> Result<T> {
        Ok(self.response.json::<T>().await?)
    }

    /// Converts this response into a byte stream that holds the concurrency permit until EOF/drop.
    pub fn bytes_stream(self) -> BoxStream<'static, std::result::Result<Bytes, reqwest::Error>> {
        let Self { response, permit } = self;
        let mut stream = response.bytes_stream();

        async_stream::stream! {
            let _permit = permit;
            while let Some(chunk) = stream.next().await {
                yield chunk;
            }
        }
        .boxed()
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::TOO_EARLY
        || status.is_server_error()
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

fn retry_after_delay(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use futures_util::TryStreamExt;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use serde_json::json;
    use tokio::time::timeout;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;

    fn controls(config: BackendRequestConfig) -> BackendRequestControls {
        BackendRequestControls::from_config(&config)
    }

    fn retry_config() -> BackendRequestConfig {
        BackendRequestConfig {
            max_retries: 2,
            initial_backoff_ms: 1,
            max_backoff_ms: 5,
            timeout_ms: None,
            concurrency_limit: None,
        }
    }

    #[tokio::test]
    async fn retries_retryable_status_then_returns_success() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .up_to_n_times(1)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1)
            .mount(&upstream)
            .await;

        let http_client = reqwest::Client::new();
        let endpoint = format!("{}/v1/test", upstream.uri());
        let response = controls(retry_config())
            .send(|| {
                http_client
                    .post(&endpoint)
                    .json(&json!({ "input": "hello" }))
            })
            .await
            .unwrap();

        let body = response.json::<serde_json::Value>().await.unwrap();
        assert_eq!(body, json!({ "ok": true }));

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn retries_rate_limit_with_retry_after_header() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("rate limited"),
            )
            .up_to_n_times(1)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1)
            .mount(&upstream)
            .await;

        let http_client = reqwest::Client::new();
        let endpoint = format!("{}/v1/test", upstream.uri());
        let response = controls(retry_config())
            .send(|| {
                http_client
                    .post(&endpoint)
                    .json(&json!({ "input": "hello" }))
            })
            .await
            .unwrap();

        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["ok"],
            true
        );
    }

    #[tokio::test]
    async fn does_not_retry_invalid_request_status() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .expect(1)
            .mount(&upstream)
            .await;

        let http_client = reqwest::Client::new();
        let endpoint = format!("{}/v1/test", upstream.uri());
        let err = controls(retry_config())
            .send(|| {
                http_client
                    .post(&endpoint)
                    .json(&json!({ "input": "hello" }))
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, ProxyError::UpstreamStatus { status, .. } if status == StatusCode::BAD_REQUEST)
        );
    }

    #[tokio::test]
    async fn per_attempt_timeout_aborts_slow_backend_request() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(json!({ "ok": true })),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let http_client = reqwest::Client::new();
        let endpoint = format!("{}/v1/test", upstream.uri());
        let err = controls(BackendRequestConfig {
            max_retries: 0,
            initial_backoff_ms: 1,
            max_backoff_ms: 1,
            timeout_ms: Some(10),
            concurrency_limit: None,
        })
        .send(|| {
            http_client
                .post(&endpoint)
                .json(&json!({ "input": "hello" }))
        })
        .await
        .unwrap_err();

        assert!(matches!(err, ProxyError::UpstreamHttp(error) if error.is_timeout()));
    }

    #[tokio::test]
    async fn concurrency_permit_is_held_until_stream_response_is_dropped() {
        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(2)
            .mount(&upstream)
            .await;

        let request_controls = controls(BackendRequestConfig {
            max_retries: 0,
            initial_backoff_ms: 1,
            max_backoff_ms: 1,
            timeout_ms: None,
            concurrency_limit: Some(1),
        });
        let http_client = reqwest::Client::new();
        let endpoint = format!("{}/stream", upstream.uri());

        let first = request_controls
            .send(|| http_client.get(&endpoint))
            .await
            .unwrap();
        let second = request_controls.send(|| http_client.get(&endpoint));
        tokio::pin!(second);

        assert!(
            timeout(Duration::from_millis(20), &mut second)
                .await
                .is_err()
        );
        drop(first);

        let second = timeout(Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap();
        let body = second
            .bytes_stream()
            .try_fold(Vec::new(), |mut body, chunk| async move {
                body.extend_from_slice(&chunk);
                Ok::<_, reqwest::Error>(body)
            })
            .await
            .unwrap();
        assert_eq!(body, b"ok");
    }

    #[test]
    fn retry_after_parses_seconds_and_http_dates() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_after_delay(&headers), Some(Duration::from_secs(2)));

        let future = SystemTime::now() + Duration::from_secs(5);
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&httpdate::fmt_http_date(future)).unwrap(),
        );
        assert!(retry_after_delay(&headers).unwrap() <= Duration::from_secs(5));
    }
}
