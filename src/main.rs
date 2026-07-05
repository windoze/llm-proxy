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
        responses::{
            decode::{responses_request_to_ir, responses_response_to_ir},
            encode::{ir_request_to_responses, ir_response_to_responses},
            stream::ir_events_to_responses_sse,
        },
    },
    provider::{CapabilityProfile, deepseek::DeepSeek, responses_backend::ResponsesBackendClient},
    stream::{
        chat_decoder::chat_sse_to_ir_events,
        responses_decoder::responses_sse_to_ir_events,
        sse::{parse_openai_chat_sse, parse_reqwest_sse},
    },
};

mod config;
pub mod error;
mod ir;
mod protocol;
mod provider;
mod reasoning;
mod stream;

const DEFAULT_ADDR: &str = "127.0.0.1:8080";
const PASSTHROUGH_UPSTREAM_URL_ENV: &str = "LLM_PROXY_UPSTREAM_URL";
const CHAT_COMPLETIONS_URL_ENV: &str = "LLM_PROXY_CHAT_COMPLETIONS_URL";
const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const OPENAI_API_ENDPOINT_ENV: &str = "OPENAI_API_ENDPOINT";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const ANTHROPIC_MESSAGES_BACKEND_ENV: &str = "LLM_PROXY_ANTHROPIC_MESSAGES_BACKEND";
const ANTHROPIC_DEFAULT_MAX_TOKENS_ENV: &str = "LLM_PROXY_ANTHROPIC_DEFAULT_MAX_TOKENS";
const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = 4096;

/// Shared HTTP clients and runtime configuration used by request handlers.
#[derive(Clone)]
struct AppState {
    http_client: reqwest::Client,
    passthrough_upstream_url: Option<String>,
    chat_completions_url: String,
    chat_api_key: Option<String>,
    responses_endpoint: Option<String>,
    responses_api_key: Option<String>,
    anthropic_messages_backend: Option<String>,
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
            responses_endpoint: env::var(OPENAI_API_ENDPOINT_ENV)
                .ok()
                .filter(|endpoint| !endpoint.trim().is_empty()),
            responses_api_key: env::var(OPENAI_API_KEY_ENV)
                .ok()
                .filter(|key| !key.trim().is_empty()),
            anthropic_messages_backend: env::var(ANTHROPIC_MESSAGES_BACKEND_ENV)
                .ok()
                .filter(|backend| !backend.trim().is_empty()),
            anthropic_default_max_tokens: env::var(ANTHROPIC_DEFAULT_MAX_TOKENS_ENV).ok(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnthropicMessagesBackend {
    Chat,
    Responses,
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
        .route("/v1/responses", post(openai_responses))
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

/// Handles Anthropic Messages API requests by proxying them to a configured backend.
async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> error::Result<Response> {
    let profile = DeepSeek;
    let mut ir_request = anthropic_request_to_ir(&body)?;
    apply_anthropic_defaults(&mut ir_request, &state)?;
    match select_anthropic_messages_backend(&ir_request, &state)? {
        AnthropicMessagesBackend::Chat => {
            let chat_body = ir_request_to_chat(&ir_request, &profile)?;
            let upstream_response = send_chat_request(&state, &headers, chat_body).await?;

            if ir_request.stream {
                chat_stream_to_anthropic_response(upstream_response).await
            } else {
                chat_json_to_anthropic_response(upstream_response).await
            }
        }
        AnthropicMessagesBackend::Responses => {
            let responses_body = ir_request_to_responses(&ir_request)?;
            let upstream_response = send_responses_request(&state, responses_body).await?;

            if ir_request.stream {
                responses_stream_to_anthropic_response(upstream_response).await
            } else {
                responses_json_to_anthropic_response(upstream_response).await
            }
        }
    }
}

/// Handles OpenAI Responses API requests by proxying them to a Chat-compatible backend.
async fn openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> error::Result<Response> {
    let profile = DeepSeek;
    let ir_request = responses_request_to_ir(&body)?;
    let chat_body = ir_request_to_chat(&ir_request, &profile)?;
    let upstream_response = send_chat_request(&state, &headers, chat_body).await?;

    if ir_request.stream {
        chat_stream_to_responses_response(upstream_response).await
    } else {
        chat_json_to_responses_response(upstream_response).await
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

async fn send_responses_request(state: &AppState, body: Value) -> error::Result<reqwest::Response> {
    responses_backend_client(state)?.send(body).await
}

async fn chat_json_to_anthropic_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let chat_response = upstream_response.json::<Value>().await?;
    let ir_response = chat_response_to_ir(&chat_response)?;
    Ok(Json(ir_response_to_anthropic(&ir_response)?).into_response())
}

async fn responses_json_to_anthropic_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let responses_response = upstream_response.json::<Value>().await?;
    let ir_response = responses_response_to_ir(&responses_response)?;
    Ok(Json(ir_response_to_anthropic(&ir_response)?).into_response())
}

async fn chat_json_to_responses_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let chat_response = upstream_response.json::<Value>().await?;
    let ir_response = chat_response_to_ir(&chat_response)?;
    Ok(Json(ir_response_to_responses(&ir_response)?).into_response())
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

async fn responses_stream_to_anthropic_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let responses_sse = parse_reqwest_sse(upstream_response.bytes_stream());
    let ir_events = responses_sse_to_ir_events(responses_sse);
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

async fn chat_stream_to_responses_response(
    upstream_response: reqwest::Response,
) -> error::Result<Response> {
    let chat_sse = parse_openai_chat_sse(upstream_response.bytes_stream());
    let ir_events = chat_sse_to_ir_events(chat_sse);
    let responses_sse = ir_events_to_responses_sse(ir_events);

    let mut response = Response::new(Body::from_stream(responses_sse));
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

fn select_anthropic_messages_backend(
    request: &ir::request::IrRequest,
    state: &AppState,
) -> error::Result<AnthropicMessagesBackend> {
    match state.anthropic_messages_backend.as_deref() {
        Some(configured) => parse_anthropic_messages_backend(configured, request, state),
        None => Ok(auto_anthropic_messages_backend(request, state)),
    }
}

fn parse_anthropic_messages_backend(
    configured: &str,
    request: &ir::request::IrRequest,
    state: &AppState,
) -> error::Result<AnthropicMessagesBackend> {
    match configured.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(auto_anthropic_messages_backend(request, state)),
        "chat" | "deepseek" | "deepseek-chat" => Ok(AnthropicMessagesBackend::Chat),
        "responses" | "openai" | "openai-responses" => Ok(AnthropicMessagesBackend::Responses),
        _ => Err(error::ProxyError::Config(format!(
            "{ANTHROPIC_MESSAGES_BACKEND_ENV} must be `auto`, `chat`, or `responses`, got `{configured}`"
        ))),
    }
}

fn auto_anthropic_messages_backend(
    request: &ir::request::IrRequest,
    state: &AppState,
) -> AnthropicMessagesBackend {
    if is_deepseek_model(&request.model) {
        return AnthropicMessagesBackend::Chat;
    }

    if state.responses_endpoint.is_some() || state.responses_api_key.is_some() {
        AnthropicMessagesBackend::Responses
    } else {
        AnthropicMessagesBackend::Chat
    }
}

fn is_deepseek_model(model: &str) -> bool {
    model.starts_with("deepseek-")
}

fn responses_backend_client(state: &AppState) -> error::Result<ResponsesBackendClient> {
    let endpoint = state
        .responses_endpoint
        .as_deref()
        .ok_or_else(|| error::ProxyError::Config(format!("missing {OPENAI_API_ENDPOINT_ENV}")))?;
    let api_key = state
        .responses_api_key
        .as_deref()
        .ok_or_else(|| error::ProxyError::Config(format!("missing {OPENAI_API_KEY_ENV}")))?;

    ResponsesBackendClient::with_http_client(state.http_client.clone(), endpoint, api_key)
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
        None => match headers.get(header::AUTHORIZATION) {
            Some(authorization) => bearer_token_from_authorization(authorization),
            None => Err(error::ProxyError::Config(format!(
                "missing {DEEPSEEK_API_KEY_ENV}"
            ))),
        },
    }
}

fn bearer_token_from_authorization(value: &HeaderValue) -> error::Result<&str> {
    let value = value.to_str().map_err(|err| {
        error::ProxyError::ProtocolMapping(format!("authorization header is invalid: {err}"))
    })?;
    let (scheme, token) = value.split_once(' ').ok_or_else(|| {
        error::ProxyError::ProtocolMapping(
            "authorization header must use Bearer authentication".to_owned(),
        )
    })?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(error::ProxyError::ProtocolMapping(
            "authorization header must use Bearer authentication".to_owned(),
        ));
    }
    Ok(token)
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
        http::{
            Request, StatusCode,
            header::{AUTHORIZATION, CONTENT_TYPE},
        },
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
            responses_endpoint: None,
            responses_api_key: None,
            anthropic_messages_backend: None,
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
            responses_endpoint: None,
            responses_api_key: None,
            anthropic_messages_backend: None,
            anthropic_default_max_tokens: anthropic_default_max_tokens.map(ToOwned::to_owned),
        })
    }

    fn test_app_with_responses_backend(
        responses_endpoint: String,
        responses_api_key: Option<&str>,
        anthropic_messages_backend: Option<&str>,
    ) -> Router {
        app_with_state(AppState {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url: None,
            chat_completions_url: "http://127.0.0.1:9/chat/completions".to_owned(),
            chat_api_key: Some("test-backend-key".to_owned()),
            responses_endpoint: Some(responses_endpoint),
            responses_api_key: responses_api_key.map(ToOwned::to_owned),
            anthropic_messages_backend: anthropic_messages_backend.map(ToOwned::to_owned),
            anthropic_default_max_tokens: None,
        })
    }

    /// Posts a Claude Code-style Anthropic Messages request to the in-process router.
    async fn post_messages(app: Router, body: Value) -> Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(CONTENT_TYPE, "application/json")
                .header("x-api-key", "client-placeholder")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    /// Posts a Codex-style OpenAI Responses request to the in-process router.
    async fn post_responses(app: Router, body: Value) -> Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, "Bearer client-placeholder")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    /// Reads a successful Anthropic SSE response body for snapshot assertions.
    async fn anthropic_sse_body(response: Response) -> String {
        let status = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body}");
        assert_eq!(content_type.as_ref().unwrap(), "text/event-stream");

        body
    }

    /// Reads a successful Responses SSE response body for route assertions.
    async fn responses_sse_body(response: Response) -> String {
        let status = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body}");
        assert_eq!(content_type.as_ref().unwrap(), "text/event-stream");

        body
    }

    /// Reads a Responses SSE body and normalizes dynamic timestamps for snapshots.
    async fn responses_sse_snapshot_body(response: Response) -> String {
        normalize_created_at_fields(responses_sse_body(response).await)
    }

    /// Replaces dynamic Responses `created_at` values while preserving SSE and JSON field order.
    fn normalize_created_at_fields(body: String) -> String {
        let marker = "\"created_at\":";
        let mut normalized = String::with_capacity(body.len());
        let mut cursor = 0;

        while let Some(relative_start) = body[cursor..].find(marker) {
            let marker_start = cursor + relative_start;
            let value_start = marker_start + marker.len();
            normalized.push_str(&body[cursor..value_start]);

            let value_end = value_start
                + body[value_start..]
                    .find(|character: char| !character.is_ascii_digit())
                    .unwrap_or(body.len() - value_start);
            normalized.push('0');
            cursor = value_end;
        }

        normalized.push_str(&body[cursor..]);
        normalized
    }

    /// Formats JSON stream chunks as an OpenAI Chat SSE response with the final `[DONE]` marker.
    fn openai_chat_sse(chunks: &[Value]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str("data: ");
            body.push_str(&chunk.to_string());
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    /// Formats named JSON events as an OpenAI Responses SSE response.
    fn responses_sse(events: &[(&str, Value)]) -> String {
        let mut body = String::new();
        for (event, data) in events {
            body.push_str("event: ");
            body.push_str(event);
            body.push('\n');
            body.push_str("data: ");
            body.push_str(&data.to_string());
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    /// Returns the single Chat Completions JSON request captured by the mock backend.
    async fn recorded_chat_request(upstream: &MockServer) -> Value {
        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "expected one upstream request");
        let request = &requests[0];
        assert_eq!(request.method.as_str(), "POST");
        assert_eq!(request.url.path(), "/chat/completions");
        request.body_json().unwrap()
    }

    /// Recorded Claude Code-style text-only request sample for the Anthropic Messages route.
    fn claude_code_plain_text_request() -> Value {
        json!({
            "model": "deepseek-chat",
            "system": "You are Claude Code. Answer concisely.",
            "messages": [{
                "role": "user",
                "content": "Say hello from the proxy."
            }],
            "max_tokens": 64,
            "stream": true
        })
    }

    /// Recorded Claude Code-style reasoning request sample for DeepSeek reasoner streaming.
    fn claude_code_reasoning_request() -> Value {
        json!({
            "model": "deepseek-reasoner",
            "messages": [{
                "role": "user",
                "content": "Think briefly before answering."
            }],
            "max_tokens": 128,
            "stream": true
        })
    }

    /// Recorded Claude Code-style multi-turn tool-use request sample.
    fn claude_code_tool_use_request() -> Value {
        json!({
            "model": "deepseek-chat",
            "system": "Use tools when required.",
            "messages": [
                {
                    "role": "user",
                    "content": "What is the weather in Paris?"
                },
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_weather_1",
                        "name": "lookup_weather",
                        "input": { "city": "Paris" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_weather_1",
                        "content": "sunny and 21C"
                    }]
                },
                {
                    "role": "user",
                    "content": "Should I bring an umbrella?"
                }
            ],
            "tools": [{
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    },
                    "required": ["city"]
                }
            }],
            "tool_choice": { "type": "auto" },
            "max_tokens": 256,
            "stream": true
        })
    }

    /// Codex-style text-only request sample for the OpenAI Responses route.
    fn codex_plain_text_request(stream: bool) -> Value {
        json!({
            "model": "deepseek-chat",
            "instructions": "You are Codex. Answer concisely.",
            "developer": "Prefer direct answers.",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Say hello from the proxy."
                }]
            }],
            "max_output_tokens": 64,
            "temperature": 0.2,
            "top_p": 0.8,
            "reasoning_effort": "low",
            "stream": stream
        })
    }

    /// Codex-style multi-turn tool-use request sample for the OpenAI Responses route.
    fn codex_tool_use_request() -> Value {
        json!({
            "model": "deepseek-chat",
            "instructions": "Use tools when required.",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": "What is the weather in Paris?"
                },
                {
                    "type": "function_call",
                    "call_id": "call_weather_1",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}",
                    "status": "completed"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather_1",
                    "output": "sunny and 21C"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "Should I bring an umbrella?"
                }
            ],
            "tools": [{
                "type": "function",
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    },
                    "required": ["city"]
                }
            }],
            "tool_choice": "auto",
            "max_output_tokens": 256,
            "stream": true
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
    async fn messages_route_round_trips_responses_reasoning_signature_to_responses_backend() {
        let upstream = MockServer::start().await;
        let tool_schema = json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        });
        let first_backend_request = json!({
            "model": "gpt-5.1",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{
                        "type": "input_text",
                        "text": "Use tools when required."
                    }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "What is the weather in Paris?"
                    }]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "parameters": tool_schema.clone()
            }],
            "tool_choice": "auto",
            "max_output_tokens": 256,
            "stream": false,
            "store": false,
            "include": ["reasoning.encrypted_content"]
        });
        let second_backend_request = json!({
            "model": "gpt-5.1",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{
                        "type": "input_text",
                        "text": "Use tools when required."
                    }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "What is the weather in Paris?"
                    }]
                },
                {
                    "type": "reasoning",
                    "summary": [{
                        "type": "summary_text",
                        "text": "Need the weather tool."
                    }],
                    "encrypted_content": "enc-weather-1"
                },
                {
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_weather_1",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather_1",
                    "output": "sunny and 21C",
                    "is_error": false
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "Should I bring an umbrella?"
                    }]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "parameters": tool_schema.clone()
            }],
            "tool_choice": "auto",
            "max_output_tokens": 256,
            "stream": false,
            "store": false,
            "include": ["reasoning.encrypted_content"]
        });

        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header_is("authorization", "Bearer responses-secret"))
            .and(header_is("content-type", "application/json"))
            .and(body_json(first_backend_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_chain4_tool_1",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "model": "gpt-5.1",
                "output": [
                    {
                        "id": "rs_weather_1",
                        "type": "reasoning",
                        "summary": [{
                            "type": "summary_text",
                            "text": "Need the weather tool."
                        }],
                        "encrypted_content": "enc-weather-1",
                        "status": "completed"
                    },
                    {
                        "id": "fc_weather_1",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_weather_1",
                        "name": "lookup_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                ],
                "usage": {
                    "input_tokens": 32,
                    "output_tokens": 12
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header_is("authorization", "Bearer responses-secret"))
            .and(header_is("content-type", "application/json"))
            .and(body_json(second_backend_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_chain4_final",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "model": "gpt-5.1",
                "output": [{
                    "id": "msg_final",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "No umbrella needed.",
                        "annotations": []
                    }]
                }],
                "usage": {
                    "input_tokens": 48,
                    "output_tokens": 5
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let first_response = post_messages(
            test_app_with_responses_backend(
                format!("{}/v1/responses", upstream.uri()),
                Some("responses-secret"),
                None,
            ),
            json!({
                "model": "gpt-5.1",
                "system": "Use tools when required.",
                "messages": [{
                    "role": "user",
                    "content": "What is the weather in Paris?"
                }],
                "tools": [{
                    "name": "lookup_weather",
                    "description": "Look up current weather for a city.",
                    "input_schema": tool_schema
                }],
                "tool_choice": { "type": "auto" },
                "max_tokens": 256,
                "stream": false
            }),
        )
        .await;

        assert_eq!(first_response.status(), StatusCode::OK);
        let first_body = to_bytes(first_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let first_body: Value = serde_json::from_slice(&first_body).unwrap();
        let signature = first_body["content"][0]["signature"].as_str().unwrap();
        let source_block = crate::reasoning::envelope::unwrap_from_signature(signature).unwrap();
        assert_eq!(source_block.source, ir::message::Provider::Responses);
        assert_eq!(source_block.payload, b"enc-weather-1");
        assert_eq!(
            first_body["content"][1],
            json!({
                "type": "tool_use",
                "id": "call_weather_1",
                "name": "lookup_weather",
                "input": { "city": "Paris" }
            })
        );
        assert_eq!(first_body["stop_reason"], json!("tool_use"));

        let second_response = post_messages(
            test_app_with_responses_backend(
                format!("{}/v1/responses", upstream.uri()),
                Some("responses-secret"),
                None,
            ),
            json!({
                "model": "gpt-5.1",
                "system": "Use tools when required.",
                "messages": [
                    {
                        "role": "user",
                        "content": "What is the weather in Paris?"
                    },
                    {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "thinking",
                                "thinking": "Need the weather tool.",
                                "signature": signature
                            },
                            {
                                "type": "tool_use",
                                "id": "call_weather_1",
                                "name": "lookup_weather",
                                "input": { "city": "Paris" }
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": "call_weather_1",
                            "content": "sunny and 21C"
                        }]
                    },
                    {
                        "role": "user",
                        "content": "Should I bring an umbrella?"
                    }
                ],
                "tools": [{
                    "name": "lookup_weather",
                    "description": "Look up current weather for a city.",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }],
                "tool_choice": { "type": "auto" },
                "max_tokens": 256,
                "stream": false
            }),
        )
        .await;

        assert_eq!(second_response.status(), StatusCode::OK);
        let second_body = to_bytes(second_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let second_body: Value = serde_json::from_slice(&second_body).unwrap();
        assert_eq!(second_body["stop_reason"], json!("end_turn"));
        assert_eq!(
            second_body["content"],
            json!([{
                "type": "text",
                "text": "No umbrella needed."
            }])
        );
    }

    #[tokio::test]
    async fn messages_route_streams_responses_backend_sse_as_anthropic_sse() {
        let upstream = MockServer::start().await;
        let backend_request = json!({
            "model": "gpt-5.1",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Call the weather tool."
                }]
            }],
            "tools": [{
                "type": "function",
                "name": "lookup_weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    },
                    "required": ["city"]
                }
            }],
            "tool_choice": "auto",
            "max_output_tokens": 128,
            "stream": true,
            "store": false,
            "include": ["reasoning.encrypted_content"]
        });
        let reasoning_item = json!({
            "id": "rs_stream_1",
            "type": "reasoning",
            "status": "completed",
            "summary": [{
                "type": "summary_text",
                "text": "Need weather."
            }],
            "encrypted_content": "enc-stream-weather"
        });
        let function_item = json!({
            "id": "fc_stream_1",
            "type": "function_call",
            "status": "completed",
            "call_id": "call_weather_stream",
            "name": "lookup_weather",
            "arguments": "{\"city\":\"Paris\"}"
        });
        let upstream_sse = responses_sse(&[
            (
                "response.created",
                json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_chain4_stream",
                        "object": "response",
                        "created_at": 0,
                        "status": "in_progress",
                        "model": "gpt-5.1",
                        "output": [],
                        "usage": null
                    }
                }),
            ),
            (
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "id": "rs_stream_1",
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": [],
                        "content": []
                    }
                }),
            ),
            (
                "response.reasoning_text.delta",
                json!({
                    "type": "response.reasoning_text.delta",
                    "item_id": "rs_stream_1",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "Need weather."
                }),
            ),
            (
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": reasoning_item.clone()
                }),
            ),
            (
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": 1,
                    "item": {
                        "id": "fc_stream_1",
                        "type": "function_call",
                        "status": "in_progress",
                        "call_id": "call_weather_stream",
                        "name": "lookup_weather",
                        "arguments": ""
                    }
                }),
            ),
            (
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": "fc_stream_1",
                    "output_index": 1,
                    "delta": "{\"city\""
                }),
            ),
            (
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": "fc_stream_1",
                    "output_index": 1,
                    "delta": ":\"Paris\"}"
                }),
            ),
            (
                "response.function_call_arguments.done",
                json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": "fc_stream_1",
                    "output_index": 1,
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                }),
            ),
            (
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": 1,
                    "item": function_item.clone()
                }),
            ),
            (
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_chain4_stream",
                        "object": "response",
                        "created_at": 0,
                        "status": "completed",
                        "model": "gpt-5.1",
                        "output": [reasoning_item, function_item],
                        "usage": {
                            "input_tokens": 20,
                            "output_tokens": 8,
                            "total_tokens": 28
                        }
                    }
                }),
            ),
        ]);

        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header_is("authorization", "Bearer responses-secret"))
            .and(header_is("content-type", "application/json"))
            .and(body_json(backend_request))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let response = post_messages(
            test_app_with_responses_backend(
                format!("{}/v1/responses", upstream.uri()),
                Some("responses-secret"),
                None,
            ),
            json!({
                "model": "gpt-5.1",
                "messages": [{
                    "role": "user",
                    "content": "Call the weather tool."
                }],
                "tools": [{
                    "name": "lookup_weather",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }],
                "tool_choice": { "type": "auto" },
                "max_tokens": 128,
                "stream": true
            }),
        )
        .await;
        let body = anthropic_sse_body(response).await;

        assert!(body.contains("event: message_start\n"));
        assert!(body.contains("\"type\":\"thinking_delta\""));
        assert!(body.contains("\"type\":\"signature_delta\""));
        assert!(body.contains("\"type\":\"input_json_delta\""));
        assert!(body.contains("\"stop_reason\":\"tool_use\""));
        let mut signature = None;
        for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["delta"]["type"] == json!("signature_delta") {
                signature = value["delta"]["signature"].as_str().map(ToOwned::to_owned);
                break;
            }
        }
        let signature = signature.expect("expected Anthropic signature_delta");
        let source_block = crate::reasoning::envelope::unwrap_from_signature(&signature).unwrap();
        assert_eq!(source_block.source, ir::message::Provider::Responses);
        assert_eq!(source_block.payload, b"enc-stream-weather");
    }

    #[tokio::test]
    async fn responses_route_translates_non_streaming_request_to_chat_backend() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header_is("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl_response_1",
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
                    "prompt_tokens": 11,
                    "completion_tokens": 4,
                    "prompt_cache_hit_tokens": 2
                }
            })))
            .mount(&upstream)
            .await;

        let response = post_responses(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            codex_plain_text_request(false),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-chat",
                "messages": [
                    {
                        "role": "system",
                        "content": [
                            {
                                "type": "text",
                                "text": "You are Codex. Answer concisely."
                            },
                            {
                                "type": "text",
                                "text": "Prefer direct answers."
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": "Say hello from the proxy."
                    }
                ],
                "max_tokens": 64,
                "stream": false,
                "reasoning_effort": "high"
            })
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["created_at"].as_u64().unwrap() > 0);
        value["created_at"] = json!(0);

        assert_eq!(
            value,
            json!({
                "id": "chatcmpl_response_1",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "model": "deepseek-chat",
                "output": [{
                    "id": "msg_chatcmpl_response_1_0",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "hello from DeepSeek",
                        "annotations": []
                    }]
                }],
                "parallel_tool_calls": true,
                "previous_response_id": null,
                "store": false,
                "usage": {
                    "input_tokens": 11,
                    "input_tokens_details": {
                        "cached_tokens": 2
                    },
                    "output_tokens": 4,
                    "output_tokens_details": {
                        "reasoning_tokens": 0
                    },
                    "total_tokens": 15
                }
            })
        );
    }

    #[tokio::test]
    async fn responses_route_streams_plain_text_chat_sse_as_responses_snapshot() {
        let upstream = MockServer::start().await;
        let upstream_sse = openai_chat_sse(&[
            json!({
                "id": "chatcmpl_responses_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_responses_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "Hello from" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_responses_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "content": " the proxy." },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 5
                }
            }),
        ]);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = post_responses(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            codex_plain_text_request(true),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-chat",
                "messages": [
                    {
                        "role": "system",
                        "content": [
                            {
                                "type": "text",
                                "text": "You are Codex. Answer concisely."
                            },
                            {
                                "type": "text",
                                "text": "Prefer direct answers."
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": "Say hello from the proxy."
                    }
                ],
                "max_tokens": 64,
                "stream": true,
                "reasoning_effort": "high"
            })
        );

        insta::assert_snapshot!(
            "responses_route_plain_text_sse",
            responses_sse_snapshot_body(response).await
        );
    }

    #[tokio::test]
    async fn responses_route_streams_tool_use_chat_sse_as_responses_sse() {
        let upstream = MockServer::start().await;
        let upstream_sse = openai_chat_sse(&[
            json!({
                "id": "chatcmpl_responses_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_responses_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_weather_2",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\""
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_responses_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "arguments": ":\"Paris\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 38,
                    "completion_tokens": 7
                }
            }),
        ]);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = post_responses(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            codex_tool_use_request(),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-chat",
                "messages": [
                    {
                        "role": "system",
                        "content": "Use tools when required."
                    },
                    {
                        "role": "user",
                        "content": "What is the weather in Paris?"
                    },
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_weather_1",
                        "content": "sunny and 21C"
                    },
                    {
                        "role": "user",
                        "content": "Should I bring an umbrella?"
                    }
                ],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "lookup_weather",
                        "description": "Look up current weather for a city.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": { "type": "string" }
                            },
                            "required": ["city"]
                        }
                    }
                }],
                "tool_choice": "auto",
                "max_tokens": 256,
                "stream": true
            })
        );

        insta::assert_snapshot!(
            "responses_route_tool_use_multiturn_sse",
            responses_sse_snapshot_body(response).await
        );
    }

    #[test]
    fn authorization_bearer_header_can_supply_upstream_token() {
        let state = AppState {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url: None,
            chat_completions_url: "http://127.0.0.1:9/chat/completions".to_owned(),
            chat_api_key: None,
            responses_endpoint: None,
            responses_api_key: None,
            anthropic_messages_backend: None,
            anthropic_default_max_tokens: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer client-token"),
        );

        assert_eq!(
            upstream_bearer_token(&state, &headers).unwrap(),
            "client-token"
        );
    }

    #[tokio::test]
    async fn messages_route_streams_plain_text_chat_sse_as_anthropic_snapshot() {
        let upstream = MockServer::start().await;
        let upstream_sse = openai_chat_sse(&[
            json!({
                "id": "chatcmpl_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "Hello from" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_text",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "content": " the proxy." },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 5
                }
            }),
        ]);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = post_messages(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            claude_code_plain_text_request(),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-chat",
                "messages": [
                    {
                        "role": "system",
                        "content": "You are Claude Code. Answer concisely."
                    },
                    {
                        "role": "user",
                        "content": "Say hello from the proxy."
                    }
                ],
                "max_tokens": 64,
                "stream": true
            })
        );

        insta::assert_snapshot!(
            "messages_route_plain_text_anthropic_sse",
            anthropic_sse_body(response).await
        );
    }

    #[tokio::test]
    async fn messages_route_streams_reasoning_chat_sse_as_anthropic_snapshot() {
        let upstream = MockServer::start().await;
        let upstream_sse = openai_chat_sse(&[
            json!({
                "id": "chatcmpl_reasoning",
                "model": "deepseek-reasoner",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_reasoning",
                "model": "deepseek-reasoner",
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": "I should answer directly." },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_reasoning",
                "model": "deepseek-reasoner",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "Done." },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 3
                }
            }),
        ]);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = post_messages(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            claude_code_reasoning_request(),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-reasoner",
                "messages": [{
                    "role": "user",
                    "content": "Think briefly before answering."
                }],
                "max_tokens": 128,
                "stream": true
            })
        );

        insta::assert_snapshot!(
            "messages_route_reasoning_anthropic_sse",
            anthropic_sse_body(response).await
        );
    }

    #[tokio::test]
    async fn messages_route_streams_tool_use_multiturn_chat_sse_as_anthropic_snapshot() {
        let upstream = MockServer::start().await;
        let upstream_sse = openai_chat_sse(&[
            json!({
                "id": "chatcmpl_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "toolu_weather_2",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\""
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl_tool",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "arguments": ":\"Paris\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 38,
                    "completion_tokens": 7
                }
            }),
        ]);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(upstream_sse),
            )
            .mount(&upstream)
            .await;

        let response = post_messages(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            claude_code_tool_use_request(),
        )
        .await;
        assert_eq!(
            recorded_chat_request(&upstream).await,
            json!({
                "model": "deepseek-chat",
                "messages": [
                    {
                        "role": "system",
                        "content": "Use tools when required."
                    },
                    {
                        "role": "user",
                        "content": "What is the weather in Paris?"
                    },
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "toolu_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "toolu_weather_1",
                        "content": "sunny and 21C"
                    },
                    {
                        "role": "user",
                        "content": "Should I bring an umbrella?"
                    }
                ],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "lookup_weather",
                        "description": "Look up current weather for a city.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": { "type": "string" }
                            },
                            "required": ["city"]
                        }
                    }
                }],
                "tool_choice": "auto",
                "max_tokens": 256,
                "stream": true
            })
        );

        insta::assert_snapshot!(
            "messages_route_tool_use_multiturn_anthropic_sse",
            anthropic_sse_body(response).await
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
