// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Builds a canonical A2A v0.3.0 `AgentCard` JSON from Claude managed-agent
//! introspection, with safe fallbacks (see `docs/agent-onboarding.md`). The shape
//! matches the public A2A spec (top-level `protocolVersion`/`url`/`preferredTransport`)
//! — the dialect the brokered agent network's working agents serve.
//!
//! Card skills are assembled from, in order and combined (deduped by id):
//!   1. the agent's declared Agent Skills (`agent.skills`) — real, highest-fidelity;
//!   2. one synthesized skill per MCP server (`agent.mcp_servers`);
//!   3. a single `general-assistance` skill if neither produced anything.
//! The agent `system` prompt is never used. A pasted `agentCard` override (when
//! `agentCardSource: derive`) shallow-merges on top — its top-level keys win
//! (e.g. supply a curated `skills` array while deriving name/description).

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::contracts::agent::AgentInfo;

pub fn build_card(
    agent_id: &str,
    agent: &AgentInfo,
    override_card: Option<&str>,
    authority: Option<&str>,
    exclude_mcp: &[String],
) -> Value {
    let name = non_empty(agent.name.as_deref()).unwrap_or_else(|| agent_id.to_string());
    let description = non_empty(agent.description.as_deref())
        .unwrap_or_else(|| format!("{} — Claude managed agent", name));
    let version = match &agent.version {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "1.0.0".to_string(),
    };
    let url = match authority {
        Some(a) if !a.trim().is_empty() => format!("https://{}/", a.trim().trim_end_matches('/')),
        _ => "/".to_string(),
    };
    let skills = build_skills(agent, &description, exclude_mcp);

    let mut card = json!({
        "protocolVersion": "0.3.0",
        "name": name,
        "description": description,
        "url": url,
        "preferredTransport": "JSONRPC",
        "version": version,
        "capabilities": { "streaming": false },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": skills
    });

    if let Some(ov) = override_card {
        if let (Ok(Value::Object(ovm)), Value::Object(base)) =
            (serde_json::from_str::<Value>(ov), &mut card)
        {
            for (k, v) in ovm {
                base.insert(k, v);
            }
        }
    }
    card
}

fn build_skills(agent: &AgentInfo, description: &str, exclude_mcp: &[String]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 1) Real Agent Skills (refs {skill_id,type,version} or any resolved fields).
    for s in &agent.skills {
        // id: explicit id -> (resolved) name -> skill_id; name prefers the resolved name.
        let id = non_empty(s.id.as_deref())
            .or_else(|| non_empty(s.name.as_deref()))
            .or_else(|| non_empty(s.skill_id.as_deref()));
        let id = match id {
            Some(x) => slug(&x),
            None => continue,
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = non_empty(s.name.as_deref())
            .or_else(|| non_empty(s.id.as_deref()))
            .or_else(|| non_empty(s.skill_id.as_deref()))
            .unwrap_or_else(|| "skill".to_string());
        let desc = non_empty(s.description.as_deref()).unwrap_or_else(|| match s.skill_type.as_deref() {
            Some(t) => format!("{} Agent Skill \"{}\".", t, name),
            None => name.clone(),
        });
        let mut tags = s.tags.clone().unwrap_or_default();
        if let Some(t) = non_empty(s.skill_type.as_deref()) {
            if !tags.iter().any(|x| x == &t) {
                tags.push(t);
            }
        }
        out.push(json!({ "id": id, "name": name, "description": desc, "tags": tags }));
    }

    // 2) Synthesize one skill per MCP server (alongside real skills).
    for m in &agent.mcp_servers {
        if let Some(n) = non_empty(m.name.as_deref()) {
            // skip infrastructure MCP servers (plumbing, not user-facing capabilities)
            if exclude_mcp
                .iter()
                .any(|pat| crate::services::tool_gate::glob_match(&n, pat))
            {
                continue;
            }
            let id = slug(&n);
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(json!({
                "id": id,
                "name": n,
                "description": format!("Access to the {} MCP server.", n),
                "tags": ["mcp"]
            }));
        }
    }

    // 3) Default single skill if nothing was produced.
    if out.is_empty() {
        return json!([{
            "id": "general-assistance",
            "name": "General assistance",
            "description": description,
            "tags": []
        }]);
    }
    Value::Array(out)
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|x| !x.is_empty()).map(|x| x.to_string())
}

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
