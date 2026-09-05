// Copyright 2026 Salesforce, Inc. All rights reserved.
mod contracts;
mod generated;
mod services;

mod a2a;
mod access_log;
mod errors;
mod jsonrpc;
// Replica-registry helpers, reached only via task_store; this policy never
// registers or lists replicas directly, so some symbols are unused here.
#[allow(dead_code)]
mod replica_registry;
mod router;
// Session store; `delete` is part of the store API but unused on this policy's
// code path.
#[allow(dead_code)]
mod session_store;
// Task store; some helper methods/index structs are part of the store API but
// unused on this policy's code path.
#[allow(dead_code)]
mod task_store;
// Time helpers, reached only via the store's persist() path.
#[allow(dead_code)]
mod time;
mod upstream;
mod util;

#[cfg(test)]
mod tests;

use crate::generated::config::Config;

use crate::task_store::TaskState;

use crate::a2a::schemas::MessageSendParams;
use crate::a2a::schemas::Part;
use crate::a2a::MESSAGE_SEND_FUNCTION_NAME;

use serde::Serialize;

use anyhow::{anyhow, Result};
use pdk::data_storage::{DataStorage, DataStorageBuilder};
use pdk::hl::timer::{Clock, Timer};
use pdk::hl::*;
use pdk::logger;
use std::time::Duration;
use uuid::Uuid;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

const DEFAULT_VERSION: &str = "2023-06-01";
const DEFAULT_BETA: &str = "managed-agents-2026-04-01";
const DEFAULT_TIMEOUT_MS: i64 = 60_000;
const DEFAULT_POLL_INTERVAL_MS: i64 = 2_000;
const DEFAULT_MAX_POLLS: i64 = 150;

// ── A2A wire response shapes ──────────────────────────────────────────────────

// Canonical A2A v0.3.0 shapes: every union member carries its `kind` discriminator
// (`text`/`message`/`task`) and TaskState is the lowercase-hyphenated enum — this is
// what the A2A spec and the `io.a2a.spec` SDK (used by the broker) deserialize.
#[derive(Serialize, Clone, Debug)]
struct A2aTextPart {
    kind: &'static str, // "text"
    text: String,
}

impl A2aTextPart {
    fn text(text: String) -> Self {
        A2aTextPart { kind: "text", text }
    }
}

#[derive(Serialize, Clone)]
struct A2aMessage {
    kind: &'static str, // "message"
    #[serde(rename = "messageId")]
    message_id: String,
    role: &'static str,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    parts: Vec<A2aTextPart>,
}

#[derive(Serialize)]
struct A2aTaskStatus {
    state: &'static str, // canonical lowercase, e.g. "completed", "input-required"
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<A2aMessage>,
}

#[derive(Serialize)]
struct A2aArtifact {
    #[serde(rename = "artifactId")]
    artifact_id: String,
    parts: Vec<A2aTextPart>,
}

#[derive(Serialize)]
struct A2aTask {
    kind: &'static str, // "task"
    id: String,
    #[serde(rename = "contextId")]
    context_id: String,
    status: A2aTaskStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<A2aArtifact>,
    /// Per-turn usage/tool-call stats (token usage, tool-call count). Omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

/// Map the internal (proto-named) `TaskState` to the canonical A2A v0.3.0 state string.
fn canonical_task_state(s: &TaskState) -> &'static str {
    match s {
        TaskState::Submitted => "submitted",
        TaskState::Working => "working",
        TaskState::InputRequired => "input-required",
        TaskState::AuthRequired => "auth-required",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Canceled => "canceled",
        TaskState::Rejected => "rejected",
        TaskState::Unknown => "unknown",
    }
}

// ── request handling ──────────────────────────────────────────────────────────

async fn request_filter<S: DataStorage>(
    request_state: RequestState,
    http_client: HttpClient,
    stream_properties: StreamProperties,
    config: &Config,
    storage: &S,
    timer: &Timer,
) -> Flow<()> {
    // Buffer headers + body in one step so Envoy holds the request in this filter
    // and never forwards it upstream — the bridge answers itself via Claude
    // callouts. Sequential into_headers_state()/into_body_state() would release
    // the headers to the router, which proxies the request while our callouts run.
    let state = request_state.into_headers_body_state().await;

    let method = state.handler().header(":method").unwrap_or_default();
    let path = state.handler().header(":path").unwrap_or_default();

    if method == "GET" && crate::util::is_agent_card_path(&path) {
        let authority = state
            .handler()
            .header(":authority")
            .or_else(|| state.handler().header("host"));
        let body = resolve_agent_card(config, &http_client, &stream_properties, authority).await;
        return Flow::Break(
            Response::new(200)
                .with_headers(vec![(
                    "content-type".to_string(),
                    "application/json".to_string(),
                )])
                .with_body(body),
        );
    }

    if method != "POST" {
        return Flow::Continue(());
    }

    // X-ANYPOINT-USER-ID is injected by the upstream auth layer; used for task ownership.
    let owner = state.handler().header("x-anypoint-user-id").unwrap_or_default();
    if owner.is_empty() {
        logger::warn!("[claude-bridge] X-ANYPOINT-USER-ID header missing; task owner will be empty");
    }

    // Reuse the cluster the inbound route resolved so outbound policies bound to
    // that cluster apply to our Claude calls; host comes from the upstream URL.
    let service = match upstream::build_upstream_service(&stream_properties, ANTHROPIC_API_URL) {
        Ok(svc) => svc,
        Err(e) => {
            logger::error!("[claude-bridge] cannot resolve upstream service: {}", e);
            return Flow::Break(router::error_response(None, errors::internal_error()));
        }
    };

    let body = state.handler().body();

    let rpc = match router::parse_request(&body) {
        Ok(r) => r,
        Err(resp) => return Flow::Break(resp),
    };

    logger::debug!("[claude-bridge] agent_id={} method={}", config.agent_id, rpc.method);

    match rpc.method.as_str() {
        MESSAGE_SEND_FUNCTION_NAME => {
            handle_message_send(rpc, &http_client, &service, config, storage, timer, &owner).await
        }
        other => {
            let err = errors::method_not_found(other, vec![MESSAGE_SEND_FUNCTION_NAME]);
            Flow::Break(router::error_response(rpc.id, err))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_message_send<S: DataStorage>(
    rpc: crate::jsonrpc::JsonRpcRequest,
    http_client: &HttpClient,
    service: &Service,
    config: &Config,
    storage: &S,
    timer: &Timer,
    owner: &str,
) -> Flow<()> {
    let params_raw = match &rpc.params {
        Some(p) => p,
        None => {
            let err = errors::invalid_params("params is required for message/send");
            return Flow::Break(router::error_response(rpc.id, err));
        }
    };

    let params: MessageSendParams = match serde_json::from_str(params_raw.get()) {
        Ok(p) => p,
        Err(e) => {
            let err = errors::invalid_params(&format!("invalid params: {}", e));
            return Flow::Break(router::error_response(rpc.id, err));
        }
    };

    // contextId/taskId from the message, or fresh UUIDs when the client omits them.
    let raw: serde_json::Value =
        serde_json::from_str(params_raw.get()).unwrap_or(serde_json::Value::Null);
    let context_id = raw
        .get("message")
        .and_then(|m| m.get("contextId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let task_id = raw
        .get("message")
        .and_then(|m| m.get("taskId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Concatenate all text parts in order.
    let text: String = params
        .message
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::TextPart(tp) => Some(tp.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    if text.is_empty() {
        let err = errors::invalid_params("message must contain at least one text part");
        return Flow::Break(router::error_response(rpc.id, err));
    }

    let version = config.anthropic_version.as_deref().unwrap_or(DEFAULT_VERSION);
    let beta = config.beta_header.as_deref().unwrap_or(DEFAULT_BETA);
    let timeout_ms = config.timeout.unwrap_or(DEFAULT_TIMEOUT_MS).max(0) as u64;
    let poll_interval_ms = config.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS).max(0) as u64;
    let max_polls = config.max_poll_attempts.unwrap_or(DEFAULT_MAX_POLLS).max(1) as u32;

    let cx = services::dispatcher::ClaudeCtx {
        http_client,
        service,
        api_key: config.api_key.as_str(),
        version,
        beta,
        timeout_ms,
    };
    let result = services::dispatcher::dispatch(
        &cx,
        storage,
        timer,
        &config.agent_id,
        &config.environment_id,
        &context_id,
        owner,
        &task_id,
        &text,
        poll_interval_ms,
        max_polls,
        config.tool_confirmation.as_ref(),
    )
    .await;

    match result {
        Ok(mapped) => {
            let part = mapped.text.as_ref().map(|t| A2aTextPart::text(t.clone()));

            let (status_message, artifacts) =
                match (&mapped.task_state, part, mapped.artifacts.first()) {
                    (TaskState::Completed, Some(p), Some(artifact)) => (
                        None,
                        vec![A2aArtifact {
                            artifact_id: artifact.artifact_id.clone(),
                            parts: vec![p],
                        }],
                    ),
                    (_, Some(p), _) => (
                        Some(A2aMessage {
                            kind: "message",
                            role: "agent",
                            context_id: Some(context_id.clone()),
                            message_id: Uuid::new_v4().to_string(),
                            parts: vec![p],
                            task_id: Some(task_id.clone()),
                        }),
                        vec![],
                    ),
                    _ => (None, vec![]),
                };

            let task = A2aTask {
                kind: "task",
                id: task_id.clone(),
                context_id: context_id.clone(),
                status: A2aTaskStatus {
                    state: canonical_task_state(&mapped.task_state),
                    message: status_message,
                },
                artifacts,
                metadata: mapped.metadata,
            };
            Flow::Break(router::ok_response(rpc.id, &task))
        }
        Err(e) => {
            logger::error!("[claude-bridge] dispatch failed: {}", e);
            Flow::Break(router::error_response(rpc.id, map_dispatch_error(&e)))
        }
    }
}

/// Resolve the agent-card body per `agentCardSource`. "derive" fetches the managed
/// agent config (GET /v1/agents/{id}) and builds the card, failing soft to the pasted
/// card; otherwise the configured `agentCard` is served verbatim.
/// TODO(perf): cache the derived card (data_storage, TTL) instead of fetching per card GET.
async fn resolve_agent_card(
    config: &Config,
    http_client: &HttpClient,
    stream_properties: &StreamProperties,
    authority: Option<String>,
) -> Vec<u8> {
    if config.agent_card_source.as_deref() == Some("derive") {
        let service = match upstream::build_upstream_service(stream_properties, ANTHROPIC_API_URL) {
            Ok(s) => s,
            Err(e) => {
                logger::error!("[claude-bridge] card derive: upstream resolve failed: {}", e);
                return paste_card_body(config);
            }
        };
        let version = config.anthropic_version.as_deref().unwrap_or(DEFAULT_VERSION);
        let beta = config.beta_header.as_deref().unwrap_or(DEFAULT_BETA);
        let timeout_ms = config.timeout.unwrap_or(DEFAULT_TIMEOUT_MS).max(0) as u64;
        match services::claude_client::get_agent(
            http_client, &service, &config.api_key, version, beta, &config.agent_id, timeout_ms,
        )
        .await
        {
            Ok(mut agent) => {
                // Resolve Agent Skill references (skill_id) to real name/description
                // via the Skills API so the card carries rich skill metadata.
                for s in agent.skills.iter_mut() {
                    if s.name.is_some() {
                        continue;
                    }
                    let sid = s.skill_id.clone().or_else(|| s.id.clone());
                    if let Some(sid) = sid {
                        if let Some((nm, desc)) = services::claude_client::resolve_skill(
                            http_client, &service, &config.api_key, version, &sid, timeout_ms,
                        )
                        .await
                        {
                            s.name = Some(nm);
                            if s.description.is_none() {
                                s.description = desc;
                            }
                        }
                    }
                }
                let exclude_mcp = config
                    .card_derivation
                    .as_ref()
                    .and_then(|c| c.exclude_mcp_servers.as_deref())
                    .unwrap_or(&[]);
                let card = services::card_builder::build_card(
                    &config.agent_id,
                    &agent,
                    config.agent_card.as_deref(),
                    authority.as_deref(),
                    exclude_mcp,
                );
                serde_json::to_vec(&card).unwrap_or_else(|_| paste_card_body(config))
            }
            Err(e) => {
                logger::warn!(
                    "[claude-bridge] card derive failed ({}); using configured agentCard",
                    e
                );
                paste_card_body(config)
            }
        }
    } else {
        paste_card_body(config)
    }
}

/// Lightweight validation that a pasted `agentCard` is a canonical A2A v0.3.0 card:
/// it must be a JSON object carrying the required top-level fields. This catches the
/// common paste mistakes (wrong dialect, missing required keys) at configuration time;
/// full structural validation is left to A2A consumers. The required set mirrors the
/// public A2A v0.3.0 AgentCard (note canonical `protocolVersion`/`url`, not the proto
/// `supportedInterfaces` shape).
fn validate_canonical_card(s: &str) -> std::result::Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| format!("invalid JSON: {}", e))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "expected a JSON object".to_string())?;
    const REQUIRED: &[&str] = &[
        "protocolVersion",
        "name",
        "description",
        "url",
        "version",
        "capabilities",
        "skills",
        "defaultInputModes",
        "defaultOutputModes",
    ];
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing required field(s): {}", missing.join(", ")));
    }
    Ok(())
}

fn paste_card_body(config: &Config) -> Vec<u8> {
    match &config.agent_card {
        Some(card) => card.as_bytes().to_vec(),
        None => {
            logger::warn!("[claude-bridge] agentCard not configured; serving empty object");
            b"{}".to_vec()
        }
    }
}

/// Classify a Claude dispatch error into a distinct A2A/JSON-RPC error so callers can
/// tell auth and upstream-availability failures apart from generic internal errors.
/// Full detail is logged by the caller; only a classified, non-leaky error (no upstream
/// body, no secret) reaches the client.
fn map_dispatch_error(e: &services::claude_client::ClaudeApiError) -> crate::jsonrpc::RpcError {
    use services::claude_client::ClaudeApiError as E;
    match e {
        E::Status(401, _) | E::Status(403, _) => crate::jsonrpc::RpcError {
            code: -32010,
            message: "Authentication with the Claude managed agent failed — check the apiKey and managed-agents access."
                .to_string(),
            data: None,
        },
        E::Status(c, _) if *c == 408 || *c == 429 || (500..600).contains(c) => crate::jsonrpc::RpcError {
            code: -32011,
            message: "The Claude managed agent is unavailable or timed out.".to_string(),
            data: Some(serde_json::json!({ "upstreamStatus": *c })),
        },
        E::Http(_) => crate::jsonrpc::RpcError {
            code: -32011,
            message: "The Claude managed agent could not be reached (network error or timeout).".to_string(),
            data: None,
        },
        _ => errors::internal_error(),
    }
}

#[entrypoint]
async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    store_builder: DataStorageBuilder,
    clock: Clock,
) -> Result<()> {
    let config: Config = serde_json::from_slice(&bytes).map_err(|e| {
        // Do not interpolate the raw config bytes into the error: they carry the
        // apiKey, and this message is written to gateway startup logs. The serde
        // error already identifies the offending field/offset without echoing the
        // secret values.
        anyhow!("Failed to parse policy configuration: {}", e)
    })?;

    // In `derive` mode the agentCard is an optional PARTIAL override, so it is not
    // validated as a full card; in `paste` mode it must be a complete canonical A2A card.
    if config.agent_card_source.as_deref() != Some("derive") {
        if let Some(ref card_str) = config.agent_card {
            validate_canonical_card(card_str).map_err(|e| {
                anyhow!(
                    "agentCard is not a valid canonical A2A v0.3.0 AgentCard: {}.",
                    e
                )
            })?;
        }
    }

    logger::debug!("[claude-bridge] loaded — agent_id={}", config.agent_id);

    const SESSION_TTL_MILLIS: u32 = 30 * 60 * 1000; // 30 minutes
    let poll_ms = config.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS).max(1) as u64;
    let timer = clock.period(Duration::from_millis(poll_ms));

    let storage_name = format!("claude-bridge-{}", config.agent_id);
    let storage = store_builder.remote(storage_name, SESSION_TTL_MILLIS);

    let filter = on_request(|rs, http_client, stream_properties| {
        request_filter(rs, http_client, stream_properties, &config, &storage, &timer)
    });
    launcher.launch(filter).await?;
    Ok(())
}
