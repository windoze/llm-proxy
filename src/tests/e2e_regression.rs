//! M7 end-to-end regression snapshots assembled from recorded client/backend shapes.

use axum::{
    body::to_bytes,
    http::{StatusCode, header::CONTENT_TYPE},
    response::Response,
};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path},
};

use super::*;

/// Reads a successful JSON response, normalizes dynamic timestamps, and formats it for snapshots.
async fn json_response_snapshot_body(response: Response) -> String {
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let mut value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::OK, "unexpected response body: {value}");
    assert_eq!(content_type.as_ref().unwrap(), "application/json");

    normalize_created_at_values(&mut value);
    serde_json::to_string_pretty(&value).unwrap()
}

/// Replaces volatile Responses `created_at` fields anywhere in a JSON response tree.
fn normalize_created_at_values(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "created_at" && child.is_number() {
                    *child = json!(0);
                } else {
                    normalize_created_at_values(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_created_at_values(item);
            }
        }
        _ => {}
    }
}

/// Codex-style reasoning request sample for chain 1 (`Chat/DeepSeek -> Responses`).
fn codex_reasoning_request() -> Value {
    json!({
        "model": "deepseek-reasoner",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "Think briefly before answering."
            }]
        }],
        "max_output_tokens": 128,
        "reasoning_effort": "low",
        "stream": true
    })
}

#[tokio::test]
async fn chain1_chat_to_responses_reasoning_snapshot() {
    let upstream = MockServer::start().await;
    let upstream_sse = openai_chat_sse(&[
        json!({
            "id": "chatcmpl_chain1_reasoning",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl_chain1_reasoning",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": "Need a short arithmetic answer." },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl_chain1_reasoning",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": { "content": "4" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 14,
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
        .expect(1)
        .mount(&upstream)
        .await;

    let response = post_responses(
        test_app_with_chat_backend(
            format!("{}/chat/completions", upstream.uri()),
            Some("backend-secret"),
            None,
        ),
        codex_reasoning_request(),
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
            "stream": true,
            "reasoning_effort": "high"
        })
    );
    insta::assert_snapshot!(
        "m7_chain1_chat_to_responses_reasoning_sse",
        responses_sse_snapshot_body(response).await
    );
}

#[tokio::test]
async fn chain4_responses_to_anthropic_text_snapshot() {
    let upstream = MockServer::start().await;
    let backend_request = json!({
        "model": "gpt-5.1",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "Say hello from the rich backend."
            }]
        }],
        "max_output_tokens": 64,
        "stream": false,
        "store": false,
        "include": ["reasoning.encrypted_content"]
    });

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_json(backend_request))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_chain4_text",
            "object": "response",
            "created_at": 0,
            "status": "completed",
            "model": "gpt-5.1",
            "output": [{
                "id": "msg_chain4_text",
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "Hello from Responses.",
                    "annotations": []
                }]
            }],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 4,
                "total_tokens": 16
            }
        })))
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
                "content": "Say hello from the rich backend."
            }],
            "max_tokens": 64,
            "stream": false
        }),
    )
    .await;

    insta::assert_snapshot!(
        "m7_chain4_responses_to_anthropic_text_json",
        json_response_snapshot_body(response).await
    );
}

#[tokio::test]
async fn chain2_anthropic_to_responses_text_snapshot() {
    let upstream = MockServer::start().await;
    let backend_request = anthropic_backend_expected_body(json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 64,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": "Say hello from the Anthropic backend."
            }]
        }],
        "stream": false
    }));

    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .and(body_json(backend_request))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_chain2_text",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{
                "type": "text",
                "text": "Hello from Anthropic."
            }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 4
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let response = post_responses(
        test_app_with_anthropic_backend(
            format!("{}/anthropic", upstream.uri()),
            Some("anthropic-secret"),
            Some("anthropic"),
            Some("claude-sonnet-4-5"),
        ),
        json!({
            "model": "gpt-5.5",
            "input": [{
                "type": "message",
                "role": "user",
                "content": "Say hello from the Anthropic backend."
            }],
            "max_output_tokens": 64,
            "stream": false
        }),
    )
    .await;

    insta::assert_snapshot!(
        "m7_chain2_anthropic_to_responses_text_json",
        json_response_snapshot_body(response).await
    );
}
