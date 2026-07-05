//! HTTP server entry point for the proxy.

use std::env;

use anyhow::Context;
use axum::{Json, Router, routing::get};
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
    Router::new()
        .route("/health", get(health))
        .layer(TraceLayer::new_for_http())
}

/// Reports whether the process is alive and able to serve HTTP requests.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
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
        http::{Request, StatusCode},
    };
    use serde_json::json;
    use tower::ServiceExt;

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
}
