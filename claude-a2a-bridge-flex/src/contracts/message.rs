// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Claude Managed Agents events API request/response types.
//!
//! A turn is driven by POSTing a `user.message` event and then polling the
//! session events list until a terminal `session.status_*` event appears.
//! Tool-use events and the `requires_action` stop reason drive HITL gating.

use serde::{Deserialize, Serialize};

// ── send events: POST /v1/sessions/{id}/events ────────────────────────────────

#[derive(Serialize, Debug)]
pub struct SendEventsRequest {
    pub events: Vec<InputEvent>,
}

#[derive(Serialize, Debug)]
pub struct InputEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub content: Vec<ContentBlock>,
}

#[derive(Serialize, Debug)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: &'static str,
    pub text: String,
}

impl SendEventsRequest {
    /// Build a single `user.message` event carrying one text block.
    pub fn user_message(text: &str) -> Self {
        SendEventsRequest {
            events: vec![InputEvent {
                event_type: "user.message",
                content: vec![ContentBlock {
                    block_type: "text",
                    text: text.to_string(),
                }],
            }],
        }
    }
}

// ── list events: GET /v1/sessions/{id}/events ─────────────────────────────────

/// Envelope for the paginated events list. The API returns events under `data`
/// (confirmed against the live API); `events` is accepted as an alias for resilience.
#[derive(Deserialize, Debug, Default)]
pub struct EventsListResponse {
    #[serde(default, alias = "events")]
    pub data: Vec<Event>,
}

/// A single session event. Only the fields the bridge needs are modeled; unknown
/// fields are ignored. For tool-use events, `id` is the tool_use id referenced by
/// the `requires_action` stop reason's `event_ids`.
#[derive(Deserialize, Debug, Clone)]
pub struct Event {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "server")]
    pub mcp_server_name: Option<String>,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    /// Token usage, present on `span.model_request_end` events.
    #[serde(default)]
    pub model_usage: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StopReason {
    #[serde(rename = "type", default)]
    pub reason_type: Option<String>,
    /// Tool-use event ids awaiting confirmation (present when type == requires_action).
    #[serde(default)]
    pub event_ids: Option<Vec<String>>,
}
