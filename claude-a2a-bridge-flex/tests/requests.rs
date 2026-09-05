// Copyright 2026 Salesforce, Inc. All rights reserved.

mod common;

use common::*;
use httpmock::{Method, MockServer, Regex};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

use pdk_test::port::Port;
use pdk_test::services::flex::{ApiConfig, Flex, FlexConfig, PolicyConfig};
use pdk_test::services::httpmock::{HttpMock, HttpMockConfig};
use pdk_test::{pdk_test, TestComposite};

const FLEX_PORT: Port = 8081;
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

// ── Mock Claude Managed Agents response bodies ───────────────────────────────

const CREATE_SESSION_RESPONSE: &str = r#"{"id": "mock-session-001"}"#;

// GET /v1/sessions/{id}/events — terminal idle with an agent.message → Completed.
const EVENTS_COMPLETED: &str = r#"{
    "data": [
        {"type": "agent.message", "content": [{"type": "text", "text": "Hello from Claude!"}]},
        {"type": "session.status_idle", "stop_reason": {"type": "end_turn"}}
    ]
}"#;

// idle with stop_reason.requires_action → InputRequired.
const EVENTS_INPUT_REQUIRED: &str = r#"{
    "data": [
        {"type": "agent.message", "content": [{"type": "text", "text": "Which one did you mean?"}]},
        {"type": "session.status_idle", "stop_reason": {"type": "requires_action"}}
    ]
}"#;

// session.error → Failed.
const EVENTS_FAILED: &str = r#"{
    "data": [
        {"type": "session.error"}
    ]
}"#;

const AGENT_CARD: &str = r#"{"protocolVersion":"0.3.0","name":"Claude Bridge","description":"A2A bridge to a Claude managed agent.","url":"http://localhost:8081/","preferredTransport":"JSONRPC","version":"1.0.0","capabilities":{"streaming":false},"defaultInputModes":["text/plain"],"defaultOutputModes":["text/plain"],"skills":[{"id":"chat","name":"Chat","description":"General chat.","tags":[]}]}"#;

// ── Test setup ────────────────────────────────────────────────────────────────

async fn create_test_setup(
    policy_config: PolicyConfig,
    api_name: &str,
) -> anyhow::Result<(TestComposite, String, MockServer)> {
    let httpmock_config = HttpMockConfig::builder()
        .port(80)
        .version("latest")
        .hostname("backend")
        .build();

    let api_config = ApiConfig::builder()
        .name(api_name)
        .upstream(&httpmock_config)
        .port(FLEX_PORT)
        .path("/")
        .policies([policy_config])
        .build();

    let flex_config = FlexConfig::builder()
        .version("1.11.4")
        .hostname("local-flex")
        .with_api(api_config)
        .config_mounts([(POLICY_DIR, "policy"), (COMMON_CONFIG_DIR, "common")])
        .build();

    let composite = TestComposite::builder()
        .with_service(flex_config)
        .with_service(httpmock_config)
        .build()
        .await?;

    let flex: Flex = composite.service()?;
    let flex_url = flex.external_url(FLEX_PORT).unwrap();
    let httpmock: HttpMock = composite.service()?;
    let mock_server = MockServer::connect_async(httpmock.socket()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    Ok((composite, flex_url, mock_server))
}

fn create_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client")
}

fn policy_config(with_card: bool) -> PolicyConfig {
    let mut cfg = json!({
        "agentId": "test-agent-001",
        "environmentId": "test-env-001",
        "apiKey": "sk-test-key",
        "timeout": 10000,
        "pollIntervalMs": 50,
        "maxPollAttempts": 20
    });
    if with_card {
        cfg["agentCard"] = json!(AGENT_CARD);
    }
    PolicyConfig::builder()
        .name(POLICY_NAME)
        .configuration(cfg)
        .build()
}

fn message_send_body(context_id: &str, text: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "message/send",
        "params": {
            "message": {
                "role": "user",
                "contextId": context_id,
                "parts": [{"type": "text", "text": text}]
            }
        }
    })
}

/// Mock create-session + send-events + the GET events list with `events_body`.
async fn mock_claude(mock_server: &MockServer, events_body: &'static str) {
    mock_server
        .mock_async(|when, then| {
            when.method(Method::POST).path_matches(Regex::new("/sessions$").unwrap());
            then.status(200).body(CREATE_SESSION_RESPONSE);
        })
        .await;
    mock_server
        .mock_async(|when, then| {
            when.method(Method::POST).path_matches(Regex::new("/events$").unwrap());
            then.status(200).body("{}");
        })
        .await;
    mock_server
        .mock_async(|when, then| {
            when.method(Method::GET).path_matches(Regex::new("/events$").unwrap());
            then.status(200).body(events_body);
        })
        .await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Happy path: new contextId → createSession → sendEvents → poll → idle/end_turn.
/// A2A task is COMPLETED and the agent.message text is returned as an artifact.
#[pdk_test]
async fn message_send_completes() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, mock_server) =
            create_test_setup(policy_config(false), "completesApi").await?;
        mock_claude(&mock_server, EVENTS_COMPLETED).await;

        let response = create_client()
            .post(&flex_url)
            .json(&message_send_body("ctx-001", "hello"))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["result"]["status"]["state"], "completed");
        assert_eq!(body["result"]["kind"], "task");
        assert_eq!(
            body["result"]["artifacts"][0]["parts"][0]["text"],
            "Hello from Claude!"
        );
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}

/// idle + stop_reason.requires_action → A2A INPUT_REQUIRED, question in status.message.
#[pdk_test]
async fn message_send_input_required() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, mock_server) =
            create_test_setup(policy_config(false), "inputRequiredApi").await?;
        mock_claude(&mock_server, EVENTS_INPUT_REQUIRED).await;

        let response = create_client()
            .post(&flex_url)
            .json(&message_send_body("ctx-002", "ambiguous"))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["result"]["status"]["state"], "input-required");
        assert_eq!(
            body["result"]["status"]["message"]["parts"][0]["text"],
            "Which one did you mean?"
        );
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}

/// session.error → A2A FAILED.
#[pdk_test]
async fn message_send_failed() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, mock_server) =
            create_test_setup(policy_config(false), "failedApi").await?;
        mock_claude(&mock_server, EVENTS_FAILED).await;

        let response = create_client()
            .post(&flex_url)
            .json(&message_send_body("ctx-003", "boom"))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["result"]["status"]["state"], "failed");
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}

/// Unsupported method → JSON-RPC -32601.
#[pdk_test]
async fn unknown_method_returns_method_not_found() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, _mock_server) =
            create_test_setup(policy_config(false), "unknownMethodApi").await?;

        let response = create_client()
            .post(&flex_url)
            .json(&json!({"jsonrpc": "2.0", "id": "2", "method": "tasks/get", "params": {"id": "t1"}}))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["error"]["code"], -32601);
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}

/// GET on the configured card path → the configured agent card is served by the bridge.
#[pdk_test]
async fn agent_card_is_served() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, _mock_server) =
            create_test_setup(policy_config(true), "agentCardApi").await?;

        let response = create_client()
            .get(format!("{}/.well-known/agent-card.json", flex_url))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["name"], "Claude Bridge");
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}

/// Non-POST, non-card request passes through (Flow::Continue) to the upstream.
#[pdk_test]
async fn non_post_passes_through() -> anyhow::Result<()> {
    let test_future = async {
        let (_composite, flex_url, mock_server) =
            create_test_setup(policy_config(false), "passThroughApi").await?;
        mock_server
            .mock_async(|when, then| {
                when.method(Method::GET).path("/health");
                then.status(200).body("ok");
            })
            .await;

        let response = create_client()
            .get(format!("{}/health", flex_url))
            .send()
            .await?;

        assert_eq!(response.status(), 200);
        Ok::<(), anyhow::Error>(())
    };
    timeout(TEST_TIMEOUT, test_future).await??;
    Ok(())
}
