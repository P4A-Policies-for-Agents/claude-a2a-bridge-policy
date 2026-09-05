// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Claude Managed Agents — agent introspection (`GET /v1/agents/{id}`), used to
//! derive an A2A agent card. Only the fields the card needs are modeled; unknown
//! fields are ignored. The agent `system` prompt is intentionally NOT modeled —
//! it must never be surfaced in the public card.

use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
pub struct AgentInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// The API returns a number (e.g. 2); kept as a Value and stringified by the card builder.
    #[serde(default)]
    pub version: Option<serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerRef>,
    /// Reserved for future card tag mapping.
    #[serde(default)]
    #[allow(dead_code)]
    pub metadata: Option<serde_json::Value>,
}

/// An agent skill. Real Agent Skills are *references* (`skill_id` + `type` +
/// `version`); `id`/`name`/`description`/`tags` are also accepted in case a
/// resolved/richer shape is returned or a future API exposes one.
#[derive(Deserialize, Debug, Default, Clone)]
pub struct AgentSkill {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default, rename = "type")]
    pub skill_type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub version: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct McpServerRef {
    #[serde(default)]
    pub name: Option<String>,
}
