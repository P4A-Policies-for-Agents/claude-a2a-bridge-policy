// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Agent card discovery path helper.
//!
//! Agent card discovery is a bodyless `GET`; requests to this path bypass
//! normal conversion and flow untouched.

/// A2A agent card discovery path.
pub const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

/// True when `path` is the agent card discovery endpoint.
///
/// Matches only the canonical path (`/.well-known/agent-card.json`), optionally
/// preceded by a routing prefix (e.g. `/my-api/.well-known/agent-card.json`).
/// The query string is stripped before matching, so a path that merely mentions
/// the card in a parameter (`/api/execute?file=agent-card.json`) does NOT match.
/// The leading `/.well-known/` segment in the constant keeps the `ends_with`
/// check from being fooled by a sibling like `/malicious-agent-card.json`.
#[inline]
pub fn is_agent_card_path(path: &str) -> bool {
    let normalized = path.split('?').next().unwrap_or(path);
    normalized == AGENT_CARD_PATH || normalized.ends_with(AGENT_CARD_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_card_path_matches_canonical_and_prefixed() {
        // Canonical and routing-prefixed variants are agent card requests.
        assert!(is_agent_card_path("/.well-known/agent-card.json"));
        assert!(is_agent_card_path("/my-api/.well-known/agent-card.json"));
        assert!(is_agent_card_path("/api/v1/.well-known/agent-card.json"));
        // Query strings are stripped before matching.
        assert!(is_agent_card_path("/.well-known/agent-card.json?cache=1"));
        assert!(is_agent_card_path(
            "/prefix/.well-known/agent-card.json?token=abc"
        ));
    }

    #[test]
    fn agent_card_path_rejects_lookalikes() {
        // A sibling that merely ends in "agent-card.json" must NOT bypass.
        assert!(!is_agent_card_path("/malicious-agent-card.json"));
        // A query parameter mentioning the card must NOT bypass.
        assert!(!is_agent_card_path("/api/execute?file=agent-card.json"));
        assert!(!is_agent_card_path("/?next=agent-card.json"));
        // Unrelated conversion paths still require conversion.
        assert!(!is_agent_card_path("/v1/agent/ask"));
        assert!(!is_agent_card_path("/message:send"));
    }
}
