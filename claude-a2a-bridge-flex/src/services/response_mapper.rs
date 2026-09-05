// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Maps Claude Managed Agents events to A2A task content.
//!
//! Turn-state detection (idle / requires_action / terminated / error) lives in
//! the dispatcher, which drives the HITL gate. This module just builds the
//! response shapes from the events of a completed turn.

use serde_json::json;

use crate::task_store::{Part, TaskArtifact, TaskState};

use crate::contracts::message::Event;

/// Output of mapping a turn's events.
pub struct MappedResponse {
    pub task_state: TaskState,
    pub artifacts: Vec<TaskArtifact>,
    /// Plain text joined from the turn's `agent.message` events.
    pub text: Option<String>,
    /// Per-turn usage/tool-call stats for the A2A `Task.metadata` map (None if nothing observed).
    pub metadata: Option<serde_json::Value>,
}

/// Build a `Completed` response: the agent's text becomes a single artifact.
pub fn completed(events: &[Event]) -> MappedResponse {
    let text = collect_answer(events);
    let artifacts = match &text {
        Some(t) => vec![TaskArtifact {
            artifact_id: format!("answer-{}", events.len()),
            parts: vec![Part::Text { text: t.clone() }],
        }],
        None => vec![],
    };
    MappedResponse {
        task_state: TaskState::Completed,
        artifacts,
        text,
        metadata: usage_metadata(events),
    }
}

/// Aggregate token usage (from `model_usage` on span events) and tool-call count
/// across the turn's events, for the A2A `Task.metadata` map. Returns None if
/// neither usage nor tool calls were observed.
pub fn usage_metadata(events: &[Event]) -> Option<serde_json::Value> {
    let (mut input, mut output, mut cache_read, mut cache_creation) = (0i64, 0i64, 0i64, 0i64);
    let mut saw_usage = false;
    let mut tool_calls = 0i64;
    for e in events {
        if e.event_type == "agent.tool_use" || e.event_type == "agent.mcp_tool_use" {
            tool_calls += 1;
        }
        if let Some(mu) = &e.model_usage {
            saw_usage = true;
            let g = |k: &str| mu.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            input += g("input_tokens");
            output += g("output_tokens");
            cache_read += g("cache_read_input_tokens");
            cache_creation += g("cache_creation_input_tokens");
        }
    }
    if !saw_usage && tool_calls == 0 {
        return None;
    }
    Some(json!({
        "tokenUsage": {
            "inputTokens": input,
            "outputTokens": output,
            "cacheReadInputTokens": cache_read,
            "cacheCreationInputTokens": cache_creation
        },
        "toolCalls": tool_calls
    }))
}

/// Join the text of all `agent.message` events. Tolerates `content` being a
/// plain string or an array of `{ "type": "text", "text": ... }` blocks.
pub fn collect_answer(events: &[Event]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for e in events {
        if e.event_type != "agent.message" {
            continue;
        }
        match &e.content {
            Some(serde_json::Value::String(s)) => parts.push(s.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
