// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Claude Managed Agents session API request/response types.

use serde::{Deserialize, Serialize};

// ── create session: POST /v1/sessions ────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct CreateSessionRequest {
    /// Managed agent id (agent_...).
    pub agent: String,
    /// Managed-agent environment id (env_...).
    pub environment_id: String,
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
