//! HTTP server entry point for the proxy.

use std::env;

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, header},
    response::Response,
    routing::{get, post},
};
use serde::Serialize;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod config;
pub mod error;
mod ir;
mod protocol;
mod provider;
mod stream;

const DEFAULT_ADDR: &str = "127.0.0.1:8080";
const PASSTHROUGH_UPSTREAM_URL_ENV: &str = "LLM_PROXY_UPSTREAM_URL";

/// Shared HTTP clients and runtime configuration used by request handlers.
#[derive(Clone)]
struct AppState {
    http_client: reqwest::Client,
    passthrough_upstream_url: Option<String>,
}

impl AppState {
    fn from_env() -> Self {
        Self {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url: env::var(PASSTHROUGH_UPSTREAM_URL_ENV).ok(),
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
        matchers::{body_string, header as header_is, method, path},
    };

    fn test_app(passthrough_upstream_url: Option<String>) -> Router {
        app_with_state(AppState {
            http_client: reqwest::Client::new(),
            passthrough_upstream_url,
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
}
