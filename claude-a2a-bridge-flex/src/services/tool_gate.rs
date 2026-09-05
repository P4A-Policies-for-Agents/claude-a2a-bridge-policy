// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Tool-confirmation policy resolver.
//!
//! Given the tools a managed agent paused to use, decide allow / deny / defer
//! for each by matching its canonical key against the configured ordered rules
//! (first match wins), falling back to `defaultAction`. Pure logic — no I/O.
//!
//! Canonical key (matched case-insensitively, '*' globs):
//!   built-in tool : `tool_use:<name>`              e.g. tool_use:bash
//!   MCP tool      : `mcp_tool_use:<server>/<name>` e.g. mcp_tool_use:finance/set_card_limit

use crate::generated::config::ToolConfirmationConfig;

/// A tool the agent paused to call (extracted from the requires_action events).
#[derive(Clone, Debug)]
pub struct PendingTool {
    pub tool_use_id: String,
    /// "agent.tool_use" or "agent.mcp_tool_use".
    pub event_type: String,
    pub name: String,
    pub server: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Allow,
    Deny,
    Defer,
}

impl Action {
    fn parse(s: &str) -> Action {
        match s.trim().to_lowercase().as_str() {
            "allow" => Action::Allow,
            "deny" => Action::Deny,
            _ => Action::Defer,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub tool_use_id: String,
    pub key: String,
    pub name: String,
    pub action: Action,
    /// Optional deny_message (only meaningful for Deny).
    pub message: Option<String>,
    /// Optional human-facing approval prompt (only meaningful for Defer).
    pub prompt: Option<String>,
}

/// Canonical key for a pending tool.
pub fn tool_key(t: &PendingTool) -> String {
    if t.event_type == "agent.mcp_tool_use" {
        format!(
            "mcp_tool_use:{}/{}",
            t.server.as_deref().unwrap_or("_").to_lowercase(),
            t.name.to_lowercase()
        )
    } else {
        format!("tool_use:{}", t.name.to_lowercase())
    }
}

/// Case-insensitive glob match supporting '*' wildcards anywhere.
pub fn glob_match(text: &str, pattern: &str) -> bool {
    let t = text.to_lowercase();
    let p = pattern.to_lowercase();
    if !p.contains('*') {
        return t == p;
    }
    let parts: Vec<&str> = p.split('*').collect();
    let n = parts.len();
    let mut pos = 0usize;

    // Leading part must be a prefix (empty if pattern starts with '*').
    let first = parts[0];
    if !t[pos..].starts_with(first) {
        return false;
    }
    pos += first.len();

    // Middle parts must appear in order.
    for (i, part) in parts.iter().enumerate() {
        if i == 0 || i == n - 1 || part.is_empty() {
            continue;
        }
        match t[pos..].find(part) {
            Some(idx) => pos += idx + part.len(),
            None => return false,
        }
    }

    // Trailing part must be a suffix (empty if pattern ends with '*').
    let last = parts[n - 1];
    if !last.is_empty() {
        if t.len() < pos + last.len() {
            return false;
        }
        if !t.ends_with(last) {
            return false;
        }
    }
    true
}

/// Substitute `${{...}}` placeholders in a rule message/prompt with this tool's
/// details. Supported keys: `tool_name`, `tool_key`, `mcp_server` (empty for
/// built-in tools). Unknown placeholders are left untouched.
fn render(template: &str, t: &PendingTool) -> String {
    if !template.contains("${{") {
        return template.to_string();
    }
    let key = tool_key(t);
    let server = t.server.clone().unwrap_or_default();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        match after.find("}}") {
            Some(end) => {
                let val = match after[..end].trim() {
                    "tool_name" => t.name.as_str(),
                    "tool_key" => key.as_str(),
                    "mcp_server" => server.as_str(),
                    _ => {
                        // unknown placeholder: keep it literally
                        out.push_str(&rest[start..start + 3 + end + 2]);
                        rest = &after[end + 2..];
                        continue;
                    }
                };
                out.push_str(val);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(rest);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Resolve a decision for each pending tool.
pub fn resolve(pending: &[PendingTool], cfg: Option<&ToolConfirmationConfig>) -> Vec<Decision> {
    let default_action = cfg
        .and_then(|c| c.default_action.as_deref())
        .map(Action::parse)
        .unwrap_or(Action::Defer);

    let no_rules = Vec::new();
    let rules = cfg.and_then(|c| c.rules.as_ref()).unwrap_or(&no_rules);

    pending
        .iter()
        .map(|t| {
            let key = tool_key(t);
            let mut action = default_action;
            let mut message = None;
            let mut prompt = None;
            for r in rules {
                if glob_match(&key, &r.tool) {
                    action = Action::parse(&r.action);
                    message = r.message.as_deref().map(|m| render(m, t));
                    prompt = r.prompt.as_deref().map(|p| render(p, t));
                    break;
                }
            }
            Decision {
                tool_use_id: t.tool_use_id.clone(),
                key,
                name: t.name.clone(),
                action,
                message,
                prompt,
            }
        })
        .collect()
}
