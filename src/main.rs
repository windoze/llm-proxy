//! HTTP server entry point for the proxy.

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State, rejection::JsonRejection},
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
    observability::{RequestObservation, observe_ir_event_stream},
    protocol::{
        anthropic::{
            decode::{anthropic_request_to_ir, anthropic_response_to_ir},
            encode::{ir_request_to_anthropic, ir_response_to_anthropic},
            stream::ir_events_to_anthropic_sse,
        },
        openai_chat::{decode::chat_response_to_ir, encode::ir_request_to_chat},
        responses::{
            decode::{responses_request_to_ir, responses_response_to_ir},
            encode::{ir_request_to_responses, ir_response_to_responses},
            stream::ir_events_to_responses_sse,
        },
    },
    provider::{
        anthropic_backend::AnthropicBackendClient,
        anthropic_cache::AnthropicCacheControlInjection,
        responses_backend::ResponsesBackendClient,
        router::{FrontendEndpoint, ModelRouter},
    },
    stream::{
        anthropic_decoder::anthropic_sse_to_ir_events,
        chat_decoder::chat_sse_to_ir_events,
        responses_decoder::responses_sse_to_ir_events,
        sse::{parse_openai_chat_sse, parse_reqwest_sse},
    },
};

mod config;
pub mod error;
mod ir;
mod observability;
mod protocol;
mod provider;
mod reasoning;
mod stream;

const PASSTHROUGH_UPSTREAM_URL_ENV: &str = config::PASSTHROUGH_UPSTREAM_URL_ENV;
const DEEPSEEK_API_KEY_ENV: &str = config::DEEPSEEK_API_KEY_ENV;
#[cfg(test)]
const DEFAULT_ANTHROPIC_VERSION: &str = config::DEFAULT_ANTHROPIC_VERSION;
const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = config::DEFAULT_ANTHROPIC_MAX_TOKENS;

/// Shared HTTP clients and runtime configuration used by request handlers.
#[derive(Clone)]
struct AppState {
    http_client: reqwest::Client,
    passthrough_upstream_url: Option<String>,
    router: ModelRouter,
    anthropic_default_max_tokens: Option<u32>,
    anthropic_cache_control: AnthropicCacheControlInjection,
    observability_dump: bool,
}

impl AppState {
    fn from_config(config: config::Config) -> Self {
        let anthropic_cache_control = if config.switches.anthropic_cache_injection {
            AnthropicCacheControlInjection::EphemeralBreakpoints
        } else {
            AnthropicCacheControlInjection::Disabled
        };
        let passthrough_upstream_url = config.passthrough_upstream_url.clone();
        let anthropic_default_max_tokens = config.anthropic_default_max_tokens;
        let observability_dump = config.switches.observability_dump;

        Self {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url,
            router: ModelRouter::new(config),
            anthropic_default_max_tokens,
            anthropic_cache_control,
            observability_dump,
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

    let config = config::Config::load().context("failed to load proxy configuration")?;
    let configured_addr = config.listen_addr.clone();
    let state = AppState::from_config(config);
    let listener = TcpListener::bind(&configured_addr).await.with_context(|| {
        format!(
            "failed to bind {} `{configured_addr}`",
            config::LISTEN_ADDR_ENV
        )
    })?;
    let local_addr = listener
        .local_addr()
        .context("failed to read bound listener address")?;

    info!(%local_addr, %configured_addr, "starting llm-proxy");

    axum::serve(listener, app_with_state(state))
        .await
        .context("axum server failed")?;

    Ok(())
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
    body: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    let mut observation = RequestObservation::new(
        FrontendEndpoint::AnthropicMessages,
        state.observability_dump,
    );
    let result = match body {
        Ok(Json(body)) => {
            observation.dump_frontend_request(&headers, &body);
            anthropic_messages_inner(&state, headers, body, &mut observation).await
        }
        Err(rejection) => Err(json_rejection_error(rejection)),
    };

    match result {
        Ok(response) => response,
        Err(error) => {
            observation.log_error(&error);
            error.into_anthropic_response()
        }
    }
}

async fn anthropic_messages_inner(
    state: &AppState,
    headers: HeaderMap,
    body: Value,
    observation: &mut RequestObservation,
) -> error::Result<Response> {
    let mut ir_request = anthropic_request_to_ir(&body)?;
    let route = state
        .router
        .route(FrontendEndpoint::AnthropicMessages, &ir_request.model)?;
    observation.set_route(&route, ir_request.stream);
    ir_request.model = route.backend_model().to_owned();
    apply_anthropic_defaults(&mut ir_request, state, route.backend())?;

    match route.backend_kind() {
        config::BackendKind::Chat => {
            let profile = route.chat_profile()?;
            let chat_body = ir_request_to_chat(&ir_request, &profile)?;
            let upstream_response =
                send_chat_request(state, &headers, route.backend(), chat_body, observation).await?;

            if ir_request.stream {
                chat_stream_to_anthropic_response(upstream_response, observation.clone()).await
            } else {
                chat_json_to_anthropic_response(upstream_response, observation).await
            }
        }
        config::BackendKind::Responses => {
            let responses_body = ir_request_to_responses(&ir_request)?;
            let upstream_response =
                send_responses_request(state, route.backend(), responses_body, observation).await?;

            if ir_request.stream {
                responses_stream_to_anthropic_response(upstream_response, observation.clone()).await
            } else {
                responses_json_to_anthropic_response(upstream_response, observation).await
            }
        }
        config::BackendKind::Anthropic => Err(error::ProxyError::Config(
            "Anthropic Messages frontend cannot route directly to an Anthropic backend".to_owned(),
        )),
    }
}

/// Handles OpenAI Responses API requests by proxying them to the selected backend protocol.
async fn openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    let mut observation =
        RequestObservation::new(FrontendEndpoint::OpenAiResponses, state.observability_dump);
    let result = match body {
        Ok(Json(body)) => {
            observation.dump_frontend_request(&headers, &body);
            openai_responses_inner(&state, headers, body, &mut observation).await
        }
        Err(rejection) => Err(json_rejection_error(rejection)),
    };

    match result {
        Ok(response) => response,
        Err(error) => {
            observation.log_error(&error);
            error.into_responses_response()
        }
    }
}

async fn openai_responses_inner(
    state: &AppState,
    headers: HeaderMap,
    body: Value,
    observation: &mut RequestObservation,
) -> error::Result<Response> {
    let mut ir_request = responses_request_to_ir(&body)?;
    let route = state
        .router
        .route(FrontendEndpoint::OpenAiResponses, &ir_request.model)?;
    observation.set_route(&route, ir_request.stream);
    ir_request.model = route.backend_model().to_owned();

    match route.backend_kind() {
        config::BackendKind::Chat => {
            let profile = route.chat_profile()?;
            let chat_body = ir_request_to_chat(&ir_request, &profile)?;
            let upstream_response =
                send_chat_request(state, &headers, route.backend(), chat_body, observation).await?;

            if ir_request.stream {
                chat_stream_to_responses_response(upstream_response, observation.clone()).await
            } else {
                chat_json_to_responses_response(upstream_response, observation).await
            }
        }
        config::BackendKind::Anthropic => {
            apply_anthropic_defaults(&mut ir_request, state, route.backend())?;
            let anthropic_body = ir_request_to_anthropic(&ir_request)?;
            let upstream_response =
                send_anthropic_request(state, route.backend(), anthropic_body, observation).await?;

            if ir_request.stream {
                anthropic_stream_to_responses_response(upstream_response, observation.clone()).await
            } else {
                anthropic_json_to_responses_response(upstream_response, observation).await
            }
        }

        config::BackendKind::Responses => Err(error::ProxyError::Config(
            "OpenAI Responses frontend cannot route directly to a Responses backend".to_owned(),
        )),
    }
}

fn json_rejection_error(rejection: JsonRejection) -> error::ProxyError {
    error::ProxyError::ProtocolMapping(format!("invalid JSON request body: {rejection}"))
}

async fn send_chat_request(
    state: &AppState,
    headers: &HeaderMap,
    backend: &config::BackendConfig,
    body: Value,
    observation: &RequestObservation,
) -> error::Result<reqwest::Response> {
    observation.dump_json("upstream_request", &body);
    let upstream_url = parse_chat_completions_url(backend)?;
    let bearer_token = upstream_bearer_token(backend, headers)?;
    let upstream_response = state
        .http_client
        .post(upstream_url)
        .bearer_auth(bearer_token)
        .json(&body)
        .send()
        .await?;

    ensure_upstream_success(upstream_response).await
}

async fn send_responses_request(
    state: &AppState,
    backend: &config::BackendConfig,
    body: Value,
    observation: &RequestObservation,
) -> error::Result<reqwest::Response> {
    let prepared_body = provider::responses_backend::prepare_responses_request_body(body.clone())?;
    observation.dump_json("upstream_request", &prepared_body);
    responses_backend_client(state, backend)?.send(body).await
}

/// Sends an already-encoded Anthropic Messages request to the configured rich backend.
async fn send_anthropic_request(
    state: &AppState,
    backend: &config::BackendConfig,
    body: Value,
    observation: &RequestObservation,
) -> error::Result<reqwest::Response> {
    let prepared_body =
        provider::anthropic_cache::prepare_anthropic_request_body_with_cache_control(
            body.clone(),
            state.anthropic_cache_control,
        )?;
    observation.dump_json("upstream_request", &prepared_body);
    anthropic_backend_client(state, backend)?.send(body).await
}

async fn chat_json_to_anthropic_response(
    upstream_response: reqwest::Response,
    observation: &RequestObservation,
) -> error::Result<Response> {
    let chat_response = upstream_response.json::<Value>().await?;
    observation.dump_json("upstream_response", &chat_response);
    let ir_response = chat_response_to_ir(&chat_response)?;
    let response_body = ir_response_to_anthropic(&ir_response)?;
    observation.dump_json("frontend_response", &response_body);
    observation.log_success(Some(&ir_response.usage));
    Ok(Json(response_body).into_response())
}

async fn responses_json_to_anthropic_response(
    upstream_response: reqwest::Response,
    observation: &RequestObservation,
) -> error::Result<Response> {
    let responses_response = upstream_response.json::<Value>().await?;
    observation.dump_json("upstream_response", &responses_response);
    let ir_response = responses_response_to_ir(&responses_response)?;
    let response_body = ir_response_to_anthropic(&ir_response)?;
    observation.dump_json("frontend_response", &response_body);
    observation.log_success(Some(&ir_response.usage));
    Ok(Json(response_body).into_response())
}

async fn chat_json_to_responses_response(
    upstream_response: reqwest::Response,
    observation: &RequestObservation,
) -> error::Result<Response> {
    let chat_response = upstream_response.json::<Value>().await?;
    observation.dump_json("upstream_response", &chat_response);
    let ir_response = chat_response_to_ir(&chat_response)?;
    let response_body = ir_response_to_responses(&ir_response)?;
    observation.dump_json("frontend_response", &response_body);
    observation.log_success(Some(&ir_response.usage));
    Ok(Json(response_body).into_response())
}

/// Converts a non-streaming Anthropic backend response into a Responses JSON response.
async fn anthropic_json_to_responses_response(
    upstream_response: reqwest::Response,
    observation: &RequestObservation,
) -> error::Result<Response> {
    let anthropic_response = upstream_response.json::<Value>().await?;
    observation.dump_json("upstream_response", &anthropic_response);
    let ir_response = anthropic_response_to_ir(&anthropic_response)?;
    let response_body = ir_response_to_responses(&ir_response)?;
    observation.dump_json("frontend_response", &response_body);
    observation.log_success(Some(&ir_response.usage));
    Ok(Json(response_body).into_response())
}

async fn chat_stream_to_anthropic_response(
    upstream_response: reqwest::Response,
    observation: RequestObservation,
) -> error::Result<Response> {
    let chat_sse = parse_openai_chat_sse(upstream_response.bytes_stream());
    let ir_events = observe_ir_event_stream(chat_sse_to_ir_events(chat_sse), observation);
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
    observation: RequestObservation,
) -> error::Result<Response> {
    let responses_sse = parse_reqwest_sse(upstream_response.bytes_stream());
    let ir_events = observe_ir_event_stream(responses_sse_to_ir_events(responses_sse), observation);
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
    observation: RequestObservation,
) -> error::Result<Response> {
    let chat_sse = parse_openai_chat_sse(upstream_response.bytes_stream());
    let ir_events = observe_ir_event_stream(chat_sse_to_ir_events(chat_sse), observation);
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

/// Converts Anthropic backend SSE into Responses SSE through streaming IR events.
async fn anthropic_stream_to_responses_response(
    upstream_response: reqwest::Response,
    observation: RequestObservation,
) -> error::Result<Response> {
    let anthropic_sse = parse_reqwest_sse(upstream_response.bytes_stream());
    let ir_events = observe_ir_event_stream(anthropic_sse_to_ir_events(anthropic_sse), observation);
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

    let headers = upstream_response.headers().clone();
    let body = upstream_response.text().await?;
    Err(error::ProxyError::upstream_status(status, &headers, body))
}

fn apply_anthropic_defaults(
    request: &mut ir::request::IrRequest,
    state: &AppState,
    backend: &config::BackendConfig,
) -> error::Result<()> {
    if request.max_tokens.is_none() {
        request.max_tokens = Some(default_anthropic_max_tokens(state, backend));
    }
    Ok(())
}

fn default_anthropic_max_tokens(state: &AppState, backend: &config::BackendConfig) -> u32 {
    state
        .anthropic_default_max_tokens
        .or(backend.default_max_tokens)
        .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS)
}

fn responses_backend_client(
    state: &AppState,
    backend: &config::BackendConfig,
) -> error::Result<ResponsesBackendClient> {
    let endpoint = backend.responses_endpoint().ok_or_else(|| {
        error::ProxyError::Config(format!(
            "responses backend `{}` requires `base_url` or `endpoint`",
            backend.name
        ))
    })?;
    let api_key = backend.api_key.as_deref().ok_or_else(|| {
        error::ProxyError::Config(format!(
            "responses backend `{}` requires `api_key`",
            backend.name
        ))
    })?;

    ResponsesBackendClient::with_http_client(state.http_client.clone(), endpoint, api_key)
}

/// Builds the configured Anthropic Messages backend client used by chain 2.
fn anthropic_backend_client(
    state: &AppState,
    backend: &config::BackendConfig,
) -> error::Result<AnthropicBackendClient> {
    let api_key = backend.api_key.as_deref().ok_or_else(|| {
        error::ProxyError::Config(format!(
            "anthropic backend `{}` requires `api_key`",
            backend.name
        ))
    })?;
    let endpoint = anthropic_messages_endpoint(backend)?;
    let anthropic_version = backend
        .anthropic_version
        .as_deref()
        .unwrap_or(config::DEFAULT_ANTHROPIC_VERSION);

    Ok(AnthropicBackendClient::with_http_client(
        state.http_client.clone(),
        endpoint,
        api_key,
        anthropic_version,
    )?
    .with_cache_control_injection(state.anthropic_cache_control))
}

/// Converts an Anthropic base URL into the concrete Messages API endpoint.
fn anthropic_messages_endpoint(backend: &config::BackendConfig) -> error::Result<String> {
    let base_url = backend.anthropic_endpoint_base().ok_or_else(|| {
        error::ProxyError::Config(format!(
            "anthropic backend `{}` requires `base_url` or `endpoint`",
            backend.name
        ))
    })?;
    let trimmed = base_url.trim().trim_end_matches('/');
    let endpoint = if trimmed.ends_with("/v1/messages") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/v1/messages")
    };
    reqwest::Url::parse(&endpoint).map_err(|err| {
        error::ProxyError::Config(format!(
            "invalid anthropic backend `{}` URL `{base_url}`: {err}",
            backend.name
        ))
    })?;
    Ok(endpoint)
}

fn upstream_bearer_token<'a>(
    backend: &'a config::BackendConfig,
    headers: &'a HeaderMap,
) -> error::Result<&'a str> {
    if let Some(api_key) = backend.api_key.as_deref() {
        return Ok(api_key);
    }

    match headers.get("x-api-key") {
        Some(api_key) => api_key.to_str().map_err(|err| {
            error::ProxyError::ProtocolMapping(format!("x-api-key header is invalid: {err}"))
        }),
        None => match headers.get(header::AUTHORIZATION) {
            Some(authorization) => bearer_token_from_authorization(authorization),
            None if backend.name == "deepseek" => Err(error::ProxyError::Config(format!(
                "missing {DEEPSEEK_API_KEY_ENV}"
            ))),
            None => Err(error::ProxyError::Config(format!(
                "chat backend `{}` requires `api_key` or a client x-api-key/Authorization header",
                backend.name
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

fn parse_chat_completions_url(backend: &config::BackendConfig) -> error::Result<reqwest::Url> {
    let url = backend.chat_completions_url().ok_or_else(|| {
        error::ProxyError::Config(format!(
            "chat backend `{}` requires `base_url` or `endpoint`",
            backend.name
        ))
    })?;
    reqwest::Url::parse(&url).map_err(|err| {
        error::ProxyError::Config(format!(
            "invalid chat backend `{}` URL `{url}`: {err}",
            backend.name
        ))
    })
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
            header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER},
        },
    };
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string, header as header_is, method, path},
    };

    fn test_app(passthrough_upstream_url: Option<String>) -> Router {
        app_with_state(AppState::from_config(config::Config {
            passthrough_upstream_url,
            ..config::Config::default()
        }))
    }

    fn test_app_with_chat_backend(
        chat_completions_url: String,
        chat_api_key: Option<&str>,
        anthropic_default_max_tokens: Option<u32>,
    ) -> Router {
        app_with_state(AppState::from_config(config::Config {
            anthropic_default_max_tokens,
            backends: vec![config::BackendConfig {
                name: "deepseek".to_owned(),
                kind: Some(config::BackendKind::Chat),
                endpoint: Some(chat_completions_url),
                api_key: chat_api_key.map(ToOwned::to_owned),
                profile: Some(config::ProfileKind::DeepSeek),
                ..config::BackendConfig::default()
            }],
            ..config::Config::default()
        }))
    }

    fn test_app_with_responses_backend(
        responses_endpoint: String,
        responses_api_key: Option<&str>,
        anthropic_messages_backend: Option<&str>,
    ) -> Router {
        app_with_state(AppState::from_config(config::Config {
            backends: vec![config::BackendConfig {
                name: "responses".to_owned(),
                kind: Some(config::BackendKind::Responses),
                endpoint: Some(responses_endpoint),
                api_key: responses_api_key.map(ToOwned::to_owned),
                profile: Some(config::ProfileKind::GenericOpenAi),
                ..config::BackendConfig::default()
            }],
            routing: config::RoutingConfig {
                anthropic_messages_backend: anthropic_messages_backend.map(ToOwned::to_owned),
                ..config::RoutingConfig::default()
            },
            ..config::Config::default()
        }))
    }

    fn test_app_with_anthropic_backend(
        anthropic_base_url: String,
        anthropic_auth_token: Option<&str>,
        responses_backend: Option<&str>,
        anthropic_default_model: Option<&str>,
    ) -> Router {
        app_with_state(AppState::from_config(config::Config {
            backends: vec![config::BackendConfig {
                name: "anthropic".to_owned(),
                kind: Some(config::BackendKind::Anthropic),
                base_url: Some(anthropic_base_url),
                api_key: anthropic_auth_token.map(ToOwned::to_owned),
                profile: Some(config::ProfileKind::Anthropic),
                anthropic_version: Some(DEFAULT_ANTHROPIC_VERSION.to_owned()),
                default_model: anthropic_default_model.map(ToOwned::to_owned),
                ..config::BackendConfig::default()
            }],
            routing: config::RoutingConfig {
                responses_backend: responses_backend.map(ToOwned::to_owned),
                ..config::RoutingConfig::default()
            },
            ..config::Config::default()
        }))
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

    /// Formats named JSON events as an Anthropic Messages SSE response.
    fn anthropic_sse(events: &[(&str, Value)]) -> String {
        let mut body = String::new();
        for (event, data) in events {
            body.push_str("event: ");
            body.push_str(event);
            body.push('\n');
            body.push_str("data: ");
            body.push_str(&data.to_string());
            body.push_str("\n\n");
        }
        body
    }

    /// Applies the same Anthropic backend cache-control preparation expected from chain 2.
    fn anthropic_backend_expected_body(body: Value) -> Value {
        crate::provider::anthropic_cache::inject_cache_control_breakpoints(body).unwrap()
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
        let response = test_app(None)
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
            "reasoning": { "effort": "high" },
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
                    "output": "sunny and 21C"
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
            "reasoning": { "effort": "high" },
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
                "output_config": { "effort": "high" },
                "thinking": { "type": "adaptive", "display": "omitted" },
                "context_management": {
                    "edits": [{ "type": "clear_thinking_20251015", "keep": "all" }]
                },
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
                "output_config": { "effort": "high" },
                "thinking": { "type": "adaptive", "display": "omitted" },
                "context_management": {
                    "edits": [{ "type": "clear_thinking_20251015", "keep": "all" }]
                },
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
    async fn responses_route_round_trips_anthropic_reasoning_signature_to_anthropic_backend() {
        let upstream = MockServer::start().await;
        let tool_schema = json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        });
        let source_block = crate::reasoning::envelope::SourceBlock::from_json(
            ir::message::Provider::Anthropic,
            &json!({
                "type": "thinking",
                "thinking": "Need the weather tool.",
                "signature": "sig_real_anthropic_1"
            }),
        )
        .unwrap();
        let gateway_reasoning_item =
            crate::reasoning::envelope::wrap_as_responses_reasoning_item(&source_block).unwrap();
        let reasoning_input_item = json!({
            "type": "reasoning",
            "summary": [{
                "type": "summary_text",
                "text": "Need the weather tool."
            }],
            "encrypted_content": gateway_reasoning_item["encrypted_content"].clone()
        });
        let first_backend_request = anthropic_backend_expected_body(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 256,
            "system": [{
                "type": "text",
                "text": "Use tools when required."
            }],
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "What is the weather in Paris?"
                }]
            }],
            "tools": [{
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "input_schema": tool_schema.clone()
            }],
            "tool_choice": { "type": "auto" },
            "stream": false
        }));
        let second_backend_request = anthropic_backend_expected_body(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 256,
            "system": [{
                "type": "text",
                "text": "Use tools when required."
            }],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "What is the weather in Paris?"
                    }]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "Need the weather tool.",
                            "signature": "sig_real_anthropic_1"
                        },
                        {
                            "type": "tool_use",
                            "id": "toolu_weather_1",
                            "name": "lookup_weather",
                            "input": { "city": "Paris" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_weather_1",
                            "content": [{
                                "type": "text",
                                "text": "sunny and 21C"
                            }],
                            "is_error": false
                        },
                        {
                            "type": "text",
                            "text": "Should I bring an umbrella?"
                        }
                    ]
                }
            ],
            "tools": [{
                "name": "lookup_weather",
                "description": "Look up current weather for a city.",
                "input_schema": tool_schema.clone()
            }],
            "tool_choice": { "type": "auto" },
            "stream": false
        }));

        Mock::given(method("POST"))
            .and(path("/anthropic/v1/messages"))
            .and(header_is("x-api-key", "sk-ant-anthropic-secret"))
            .and(header_is("anthropic-version", DEFAULT_ANTHROPIC_VERSION))
            .and(header_is("content-type", "application/json"))
            .and(body_json(first_backend_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_chain2_tool_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [
                    {
                        "type": "thinking",
                        "thinking": "Need the weather tool.",
                        "signature": "sig_real_anthropic_1"
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_weather_1",
                        "name": "lookup_weather",
                        "input": { "city": "Paris" }
                    }
                ],
                "stop_reason": "tool_use",
                "usage": {
                    "input_tokens": 32,
                    "output_tokens": 12
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/anthropic/v1/messages"))
            .and(header_is("x-api-key", "sk-ant-anthropic-secret"))
            .and(header_is("anthropic-version", DEFAULT_ANTHROPIC_VERSION))
            .and(header_is("content-type", "application/json"))
            .and(body_json(second_backend_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_chain2_final",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [{
                    "type": "text",
                    "text": "No umbrella needed."
                }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 52,
                    "output_tokens": 5
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let first_response = post_responses(
            test_app_with_anthropic_backend(
                format!("{}/anthropic", upstream.uri()),
                Some("sk-ant-anthropic-secret"),
                Some("anthropic"),
                Some("claude-sonnet-4-5"),
            ),
            json!({
                "model": "gpt-5.5",
                "instructions": "Use tools when required.",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": "What is the weather in Paris?"
                }],
                "tools": [{
                    "type": "function",
                    "name": "lookup_weather",
                    "description": "Look up current weather for a city.",
                    "parameters": tool_schema
                }],
                "tool_choice": "auto",
                "max_output_tokens": 256,
                "stream": false
            }),
        )
        .await;

        assert_eq!(first_response.status(), StatusCode::OK);
        let first_body = to_bytes(first_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut first_body: Value = serde_json::from_slice(&first_body).unwrap();
        first_body["created_at"] = json!(0);
        let returned_reasoning = &first_body["output"][0];
        let returned_source_block =
            crate::reasoning::envelope::unwrap_from_responses_reasoning_item(returned_reasoning)
                .unwrap();
        assert_eq!(
            returned_source_block.source,
            ir::message::Provider::Anthropic
        );
        assert_eq!(
            returned_source_block.payload_json().unwrap(),
            json!({
                "type": "thinking",
                "thinking": "Need the weather tool.",
                "signature": "sig_real_anthropic_1"
            })
        );
        assert_eq!(
            first_body["output"][1],
            json!({
                "id": "fc_msg_chain2_tool_1_1",
                "type": "function_call",
                "status": "completed",
                "call_id": "toolu_weather_1",
                "name": "lookup_weather",
                "arguments": "{\"city\":\"Paris\"}"
            })
        );

        let second_response = post_responses(
            test_app_with_anthropic_backend(
                format!("{}/anthropic", upstream.uri()),
                Some("sk-ant-anthropic-secret"),
                Some("anthropic"),
                Some("claude-sonnet-4-5"),
            ),
            json!({
                "model": "gpt-5.5",
                "instructions": "Use tools when required.",
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": "What is the weather in Paris?"
                    },
                    reasoning_input_item,
                    {
                        "type": "function_call",
                        "call_id": "toolu_weather_1",
                        "name": "lookup_weather",
                        "arguments": "{\"city\":\"Paris\"}",
                        "status": "completed"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "toolu_weather_1",
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
                "stream": false
            }),
        )
        .await;

        assert_eq!(second_response.status(), StatusCode::OK);
        let second_body = to_bytes(second_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut second_body: Value = serde_json::from_slice(&second_body).unwrap();
        second_body["created_at"] = json!(0);
        assert_eq!(second_body["status"], json!("completed"));
        assert_eq!(
            second_body["output"],
            json!([{
                "id": "msg_msg_chain2_final_0",
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "No umbrella needed.",
                    "annotations": []
                }]
            }])
        );
    }

    #[tokio::test]
    async fn responses_route_streams_anthropic_backend_sse_as_responses_sse() {
        let upstream = MockServer::start().await;
        let backend_request = anthropic_backend_expected_body(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "Call the weather tool."
                }]
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
            "stream": true
        }));
        let upstream_sse = anthropic_sse(&[
            (
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_chain2_stream",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-5",
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 20
                        }
                    }
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "thinking",
                        "thinking": ""
                    }
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": "Need weather."
                    }
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "signature_delta",
                        "signature": "sig_real_anthropic_stream"
                    }
                }),
            ),
            (
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": 0
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 1,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_weather_stream",
                        "name": "lookup_weather",
                        "input": {}
                    }
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"city\""
                    }
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": ":\"Paris\"}"
                    }
                }),
            ),
            (
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": 1
                }),
            ),
            (
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": "tool_use",
                        "stop_sequence": null
                    },
                    "usage": {
                        "output_tokens": 9
                    }
                }),
            ),
            (
                "message_stop",
                json!({
                    "type": "message_stop"
                }),
            ),
        ]);

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header_is("x-api-key", "sk-ant-anthropic-secret"))
            .and(header_is("anthropic-version", DEFAULT_ANTHROPIC_VERSION))
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

        let response = post_responses(
            test_app_with_anthropic_backend(
                format!("{}/v1/messages", upstream.uri()),
                Some("sk-ant-anthropic-secret"),
                Some("anthropic"),
                Some("claude-sonnet-4-5"),
            ),
            json!({
                "model": "gpt-5.5",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": "Call the weather tool."
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
                "stream": true
            }),
        )
        .await;
        let body = responses_sse_body(response).await;

        assert!(body.contains("event: response.created\n"));
        assert!(body.contains("\"type\":\"response.reasoning_text.delta\""));
        assert!(body.contains("\"type\":\"response.function_call_arguments.delta\""));
        assert!(body.contains("\"status\":\"completed\""));

        let mut reasoning_item = None;
        for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["type"] == json!("response.output_item.done")
                && value["output_index"] == json!(0)
            {
                reasoning_item = Some(value["item"].clone());
                break;
            }
        }
        let reasoning_item = reasoning_item.expect("expected completed reasoning item");
        let source_block =
            crate::reasoning::envelope::unwrap_from_responses_reasoning_item(&reasoning_item)
                .unwrap();
        assert_eq!(source_block.source, ir::message::Provider::Anthropic);
        assert_eq!(
            source_block.payload_json().unwrap(),
            json!({
                "type": "thinking",
                "thinking": "Need weather.",
                "signature": "sig_real_anthropic_stream"
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
        let backend = config::BackendConfig {
            name: "deepseek".to_owned(),
            kind: Some(config::BackendKind::Chat),
            endpoint: Some("http://127.0.0.1:9/chat/completions".to_owned()),
            profile: Some(config::ProfileKind::DeepSeek),
            ..config::BackendConfig::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer client-token"),
        );

        assert_eq!(
            upstream_bearer_token(&backend, &headers).unwrap(),
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
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": "configuration error: missing DEEPSEEK_API_KEY"
                }
            })
        );
    }

    #[tokio::test]
    async fn messages_route_maps_upstream_error_to_anthropic_format() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "17")
                    .insert_header("x-ratelimit-remaining-requests", "0")
                    .set_body_json(json!({
                        "error": {
                            "message": "chat backend rate limit"
                        }
                    })),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let response = post_messages(
            test_app_with_chat_backend(
                format!("{}/chat/completions", upstream.uri()),
                Some("backend-secret"),
                None,
            ),
            json!({
                "model": "deepseek-chat",
                "messages": [{ "role": "user", "content": "hello" }]
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "17");
        assert_eq!(
            response
                .headers()
                .get("anthropic-ratelimit-requests-remaining")
                .unwrap(),
            "0"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "message": "upstream returned status 429 Too Many Requests: chat backend rate limit"
                }
            })
        );
    }

    #[tokio::test]
    async fn responses_route_maps_upstream_error_to_responses_format() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("anthropic-ratelimit-tokens-reset", "2026-07-06T00:00:00Z")
                    .set_body_json(json!({
                        "error": {
                            "message": "anthropic backend overloaded"
                        }
                    })),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let response = post_responses(
            test_app_with_anthropic_backend(
                upstream.uri(),
                Some("anthropic-secret"),
                None,
                Some("claude-sonnet-4-5"),
            ),
            json!({
                "model": "claude-sonnet-4-5",
                "input": "hello"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get("x-ratelimit-reset-tokens").unwrap(),
            "2026-07-06T00:00:00Z"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            json!({
                "error": {
                    "message": "upstream returned status 503 Service Unavailable: anthropic backend overloaded",
                    "type": "server_error",
                    "param": null,
                    "code": "upstream_5xx"
                }
            })
        );
    }
}
