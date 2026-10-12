// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Claude Managed Agents session API request/response types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── create session: POST /v1/sessions ────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct CreateSessionRequest {
    /// Managed agent id (agent_...).
    pub agent: String,
    /// Managed-agent environment id (env_...).
    pub environment_id: String,
    /// Key/value metadata stamped on the session. The bridge records the A2A
    /// `contextId` (and `taskId`) here so a conversation can be found again on the
    /// Claude side after the gateway's hot cache expires. Omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, Debug)]
pub struct CreateSessionResponse {
    /// Server-assigned session id.
    pub id: String,
}

// ── Claude session data stored in SessionEntry<T>.platform ────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClaudeSessionData {
    pub session_id: String,
    /// Tools deferred to the human, awaiting an approve/deny on the next turn.
    /// Empty for a normal turn; non-empty means the next message/send is a decision.
    #[serde(default)]
    pub pending: Vec<PendingToolState>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PendingToolState {
    pub tool_use_id: String,
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub message: Option<String>,
    /// Custom human-facing approval prompt shown on INPUT_REQUIRED.
    #[serde(default)]
    pub prompt: Option<String>,
}

// ── list sessions: GET /v1/sessions ───────────────────────────────────────────

/// Envelope for the paginated sessions list (used for recovery-by-contextId).
#[derive(Deserialize, Debug, Default)]
pub struct SessionsListResponse {
    #[serde(default)]
    pub data: Vec<SessionSummary>,
}

/// One session in the list. Only the fields recovery needs are modeled; the
/// `metadata` carries the A2A `contextId` the bridge stamped at create time.
#[derive(Deserialize, Debug)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub metadata: Option<BTreeMap<String, String>>,
}
