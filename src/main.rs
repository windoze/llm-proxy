//! HTTP server entry point for the proxy.

use std::env;

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::{
    protocol::{
        anthropic::{
            decode::anthropic_request_to_ir, encode::ir_response_to_anthropic,
            stream::ir_events_to_anthropic_sse,
        },
        openai_chat::{decode::chat_response_to_ir, encode::ir_request_to_chat},
    },
    provider::{CapabilityProfile, deepseek::DeepSeek},
    stream::{chat_decoder::chat_sse_to_ir_events, sse::parse_openai_chat_sse},
};

mod config;
pub mod error;
mod ir;
mod protocol;
mod provider;
mod stream;

const DEFAULT_ADDR: &str = "127.0.0.1:8080";
const PASSTHROUGH_UPSTREAM_URL_ENV: &str = "LLM_PROXY_UPSTREAM_URL";
const CHAT_COMPLETIONS_URL_ENV: &str = "LLM_PROXY_CHAT_COMPLETIONS_URL";
const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const ANTHROPIC_DEFAULT_MAX_TOKENS_ENV: &str = "LLM_PROXY_ANTHROPIC_DEFAULT_MAX_TOKENS";
const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = 4096;

/// Shared HTTP clients and runtime configuration used by request handlers.
#[derive(Clone)]
struct AppState {
    http_client: reqwest::Client,
    passthrough_upstream_url: Option<String>,
    chat_completions_url: String,
    chat_api_key: Option<String>,
    anthropic_default_max_tokens: Option<String>,
}

impl AppState {
    fn from_env() -> Self {
        let deepseek = DeepSeek;

        Self {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url: env::var(PASSTHROUGH_UPSTREAM_URL_ENV).ok(),
            chat_completions_url: env::var(CHAT_COMPLETIONS_URL_ENV)
                .unwrap_or_else(|_| default_chat_completions_url(&deepseek)),
            chat_api_key: env::var(DEEPSEEK_API_KEY_ENV)
                .ok()
                .filter(|key| !key.trim().is_empty()),
            anthropic_default_max_tokens: env::var(ANTHROPIC_DEFAULT_MAX_TOKENS_ENV).ok(),
        }
    }
}

/// Health-check payload returned by `GET /health`.
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// Starts the proxy HTTP server.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let configured_addr = env::var("LLM_PROXY_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let listener = TcpListener::bind(&configured_addr)
        .await
        .with_context(|| format!("failed to bind LLM_PROXY_ADDR `{configured_addr}`"))?;
    let local_addr = listener
        .local_addr()
        .context("failed to read bound listener address")?;

    info!(%local_addr, %configured_addr, "starting llm-proxy");

    axum::serve(listener, app())
        .await
        .context("axum server failed")?;

    Ok(())
}

/// Builds the router shared by the binary and route tests.
fn app() -> Router {
    app_with_state(AppState::from_env())
}

/// Builds the router with explicit state for tests and future configuration loading.
fn app_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/passthrough", post(passthrough))
        .route("/v1/messages", post(anthropic_messages))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Reports whether the process is alive and able to serve HTTP requests.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Streams the incoming request body to the configured upstream URL and streams the response back.
async fn passthrough(
    State(state): State<AppState>,
    request: Request<Body>,
) -> error::Result<Response> {
    let upstream_url = state.passthrough_upstream_url.as_deref().ok_or_else(|| {
        error::ProxyError::Config(format!("missing {PASSTHROUGH_UPSTREAM_URL_ENV}"))
    })?;
    let upstream_url = reqwest::Url::parse(upstream_url).map_err(|err| {
        error::ProxyError::Config(format!(
            "invalid {PASSTHROUGH_UPSTREAM_URL_ENV} `{upstream_url}`: {err}"
        ))
    })?;

    let (parts, body) = request.into_parts();
    let mut upstream_request = state
        .http_client
        .post(upstream_url)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));
    upstream_request = copy_content_type(&parts.headers, upstream_request);

    let upstream_response = upstream_request.send().await?;
    let status = upstream_response.status();
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let response_stream = upstream_response.bytes_stream();

    let mut response = Response::new(Body::from_stream(response_stream));
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }

    Ok(response)
}

/// Handles Anthropic Messages API requests by proxying them to a Chat-compatible backend.
async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> error::Result<Response> {
    let profile = DeepSeek;
    let mut ir_request = anthropic_request_to_ir(&body)?;
    apply_anthropic_defaults(&mut ir_request, &state)?;
    let chat_body = ir_request_to_chat(&ir_request, &profile)?;
    let upstream_response = send_chat_request(&state, &headers, chat_body).await?;

    if ir_request.stream {
        chat_stream_to_anthropic_response(upstream_response).await
    } else {
        chat_json_to_anthropic_response(upstream_response).await
    }
}

async fn send_chat_request(
    state: &AppState,
    headers: &HeaderMap,
    body: Value,
) -> error::Result<reqwest::Response> {
    let upstream_url = parse_chat_completions_url(&state.chat_completions_url)?;
    let bearer_token = upstream_bearer_token(state, headers)?;
    let upstream_response = state
        .http_client
        .post(upstream_url)
        .bearer_auth(bearer_token)
        .json(&body)
        .send()
        .await?;

    ensure_upstream_success(upstream_response).await
}

async fn chat_json_to_anthropic_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let chat_response = upstream_response.json::<Value>().await?;
    let ir_response = chat_response_to_ir(&chat_response)?;
    Ok(Json(ir_response_to_anthropic(&ir_response)).into_response())
}

async fn chat_stream_to_anthropic_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let chat_sse = parse_openai_chat_sse(upstream_response.bytes_stream());
    let ir_events = chat_sse_to_ir_events(chat_sse);
    let anthropic_sse = ir_events_to_anthropic_sse(ir_events);

    let mut response = Response::new(Body::from_stream(anthropic_sse));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

async fn ensure_upstream_success(
    upstream_response: reqwest::Response,
) -> error::Result<reqwest::Response> {
    let status = upstream_response.status();
    if status.is_success() {
        return Ok(upstream_response);
    }

    let body = upstream_response.text().await?;
    Err(error::ProxyError::Upstream4xx { status, body })
}

fn apply_anthropic_defaults(
    request: &mut ir::request::IrRequest,
    state: &AppState,
) -> error::Result<()> {
    if request.max_tokens.is_none() {
        request.max_tokens = Some(default_anthropic_max_tokens(state)?);
    }
    Ok(())
}

fn default_anthropic_max_tokens(state: &AppState) -> error::Result<u32> {
    let Some(configured) = &state.anthropic_default_max_tokens else {
        return Ok(DEFAULT_ANTHROPIC_MAX_TOKENS);
    };
    let value = configured.parse::<u32>().map_err(|err| {
        error::ProxyError::Config(format!(
            "invalid {ANTHROPIC_DEFAULT_MAX_TOKENS_ENV} `{configured}`: {err}"
        ))
    })?;
    if value == 0 {
        return Err(error::ProxyError::Config(format!(
            "{ANTHROPIC_DEFAULT_MAX_TOKENS_ENV} must be greater than zero"
        )));
    }
    Ok(value)
}

fn upstream_bearer_token<'a>(
    state: &'a AppState,
    headers: &'a HeaderMap,
) -> error::Result<&'a str> {
    if let Some(api_key) = state.chat_api_key.as_deref() {
        return Ok(api_key);
    }

    match headers.get("x-api-key") {
        Some(api_key) => api_key.to_str().map_err(|err| {
            error::ProxyError::ProtocolMapping(format!("x-api-key header is invalid: {err}"))
        }),
        None => Err(error::ProxyError::Config(format!(
            "missing {DEEPSEEK_API_KEY_ENV}"
        ))),
    }
}

fn parse_chat_completions_url(url: &str) -> error::Result<reqwest::Url> {
    reqwest::Url::parse(url).map_err(|err| {
        error::ProxyError::Config(format!("invalid {CHAT_COMPLETIONS_URL_ENV} `{url}`: {err}"))
    })
}

fn default_chat_completions_url(profile: &dyn CapabilityProfile) -> String {
    format!(
        "{}/chat/completions",
        profile.base_url().trim_end_matches('/')
    )
}

/// Copies request content type to the upstream request when the client supplied one.
fn copy_content_type(
    headers: &HeaderMap,
    request: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    match headers.get(header::CONTENT_TYPE) {
        Some(content_type) => request.header(header::CONTENT_TYPE, content_type.clone()),
        None => request,
    }
}

/// Initializes request logging from `RUST_LOG`, defaulting to `info`.
fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header::CONTENT_TYPE},
    };
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string, header as header_is, method, path},
    };

    fn test_app(passthrough_upstream_url: Option<String>) -> Router {
        app_with_state(AppState {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url,
            chat_completions_url: "http://127.0.0.1:9/chat/completions".to_owned(),
            chat_api_key: Some("test-backend-key".to_owned()),
            anthropic_default_max_tokens: None,
        })
    }

    fn test_app_with_chat_backend(
        chat_completions_url: String,
        chat_api_key: Option<&str>,
        anthropic_default_max_tokens: Option<&str>,
    ) -> Router {
        app_with_state(AppState {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url: None,
            chat_completions_url,
            chat_api_key: chat_api_key.map(ToOwned::to_owned),
            anthropic_default_max_tokens: anthropic_default_max_tokens.map(ToOwned::to_owned),
        })
    }

    #[tokio::test]
    async fn health_route_returns_ok_json() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn passthrough_streams_upstream_body_and_content_type() {
        let upstream = MockServer::start().await;
        let upstream_body = b"data: first\n\ndata: second\n\n".to_vec();

        Mock::given(method("POST"))
            .and(path("/stream"))
            .and(header_is("content-type", "application/json"))
            .and(body_string(r#"{"hello":"world"}"#))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(upstream_body.clone()),
            )
            .mount(&upstream)
            .await;

        let response = test_app(Some(format!("{}/stream", upstream.uri())))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/passthrough")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"hello":"world"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], upstream_body.as_slice());
    }

    #[tokio::test]
    async fn passthrough_requires_upstream_url_configuration() {
        let response = test_app(None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/passthrough")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "error": {
                    "code": "config",
                    "message": "configuration error: missing LLM_PROXY_UPSTREAM_URL"
                }
            })
        );
    }

    #[tokio::test]
    async fn messages_route_translates_non_streaming_anthropic_request_to_chat_backend() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header_is("authorization", "Bearer backend-secret"))
            .and(header_is("content-type", "application/json"))
            .and(body_json(json!({
                "model": "deepseek-chat",
                "messages": [
                    { "role": "system", "content": "be concise" },
                    { "role": "user", "content": "hello" }
                ],
                "max_tokens": DEFAULT_ANTHROPIC_MAX_TOKENS,
                "stream": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl_1",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "hello from DeepSeek"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 8,
                    "completion_tokens": 4
                }
            })))
            .mount(&upstream)
            .await;

        let response = test_app_with_chat_backend(
            format!("{}/chat/completions", upstream.uri()),
            Some("backend-secret"),
            None,
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(CONTENT_TYPE, "application/json")
                .header("x-api-key", "client-placeholder")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    json!({
                        "model": "deepseek-chat",
                        "system": "be concise",
                        "messages": [{ "role": "user", "content": "hello" }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "id": "chatcmpl_1",
                "type": "message",
                "role": "assistant",
                "model": "deepseek-chat",
                "content": [{
                    "type": "text",
                    "text": "hello from DeepSeek"
                }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 8,
                    "output_tokens": 4
                }
            })
        );
    }

    #[tokio::test]
    async fn messages_route_streams_chat_sse_as_anthropic_sse() {
        let upstream = MockServer::start().await;
        let upstream_sse = concat!(
            "data: {\"id\":\"chatcmpl_stream\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_stream\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Think.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_stream\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Answer.\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header_is("authorization", "Bearer backend-secret"))
            .and(body_json(json!({
                "model": "deepseek-reasoner",
                "messages": [{ "role": "user", "content": "think then answer" }],
                "max_tokens": 128,
                "stream": true
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = test_app_with_chat_backend(
            format!("{}/chat/completions", upstream.uri()),
            Some("backend-secret"),
            None,
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "deepseek-reasoner",
                        "messages": [{
                            "role": "user",
                            "content": "think then answer"
                        }],
                        "max_tokens": 128,
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("event: message_start\n"));
        assert!(body.contains(
            "\"content_block\":{\"signature\":\"\",\"thinking\":\"\",\"type\":\"thinking\"}"
        ));
        assert!(body.contains("\"delta\":{\"thinking\":\"Think.\",\"type\":\"thinking_delta\"}"));
        assert!(body.contains("\"delta\":{\"text\":\"Answer.\",\"type\":\"text_delta\"}"));
        assert!(body.contains("\"stop_reason\":\"end_turn\""));
        assert!(body.contains("\"usage\":{\"input_tokens\":10,\"output_tokens\":3}"));
        assert!(body.contains("event: message_stop\n"));
    }

    #[tokio::test]
    async fn messages_route_requires_backend_api_key_configuration() {
        let response = test_app_with_chat_backend(
            "http://127.0.0.1:9/chat/completions".to_owned(),
            None,
            None,
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "deepseek-chat",
                        "messages": [{ "role": "user", "content": "hello" }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "error": {
                    "code": "config",
                    "message": "configuration error: missing DEEPSEEK_API_KEY"
                }
            })
        );
    }
}
