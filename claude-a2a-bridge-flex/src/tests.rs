// Copyright 2026 Salesforce, Inc. All rights reserved.

//! pdk-unit tests for the Claude bridge policy (in-process; no Docker/Flex).

mod tests {
    use pdk_unit::{UnitHttpMessage, UnitHttpRequest, UnitHttpResponse, UnitTestBuilder};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    // ── Mock Claude wire bodies ───────────────────────────────────────────────

    const CREATE_SESSION: &str = r#"{"id":"mock-session-001"}"#;

    const EVENTS_COMPLETED: &str = r#"{"data":[
        {"type":"span.model_request_end","model_usage":{"input_tokens":42,"output_tokens":17}},
        {"type":"agent.message","content":[{"type":"text","text":"Hello from Claude!"}]},
        {"type":"session.status_idle","stop_reason":{"type":"end_turn"}}
    ]}"#;

    const EVENTS_FAILED: &str = r#"{"data":[{"type":"session.error"}]}"#;

    // A turn paused on a tool call (built-in tool "set_card_limit").
    const EVENTS_REQUIRES_TOOL: &str = r#"{"data":[
        {"type":"agent.tool_use","id":"tu-1","name":"set_card_limit"},
        {"type":"session.status_idle","stop_reason":{"type":"requires_action","event_ids":["tu-1"]}}
    ]}"#;

    const AGENT_CARD: &str = r#"{"protocolVersion":"0.3.0","name":"Claude Bridge","description":"x","url":"http://localhost:8186/","preferredTransport":"JSONRPC","version":"1.0.0","capabilities":{"streaming":false},"defaultInputModes":["text/plain"],"defaultOutputModes":["text/plain"],"skills":[{"id":"chat","name":"Chat","description":"c","tags":[]}]}"#;

    // ── Config / request helpers ──────────────────────────────────────────────

    fn config(tool_confirmation: Option<serde_json::Value>, with_card: bool) -> String {
        let mut c = json!({
            "agentId": "agent-test",
            "environmentId": "env-test",
            "apiKey": "sk-test",
            "timeout": 5000,
            "pollIntervalMs": 1,
            "maxPollAttempts": 5
        });
        if let Some(tc) = tool_confirmation {
            c["toolConfirmation"] = tc;
        }
        let _ = with_card; // agentCard is required now; always include a valid card
        c["agentCard"] = json!(AGENT_CARD);
        c.to_string()
    }

    /// Static backend: routes create / send / list by method+path; GET returns `events`.
    fn claude_backend(events: &'static str) -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        move |req: UnitHttpRequest| route(req, events, None)
    }

    /// Stateful backend: GET returns `requires_tool` until a tool_confirmation is
    /// POSTed, then returns `completed` — simulating the session advancing.
    fn gating_backend() -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        let confirmed = Rc::new(Cell::new(false));
        move |req: UnitHttpRequest| route(req, EVENTS_REQUIRES_TOOL, Some(confirmed.clone()))
    }

    fn route(req: UnitHttpRequest, get_events: &str, confirmed: Option<Rc<Cell<bool>>>) -> UnitHttpResponse {
        let method = req.header(":method").unwrap_or_default().to_string();
        let path = req.header(":path").unwrap_or_default().to_string();
        if method == "POST" && path.contains("/events") {
            if let Some(c) = &confirmed {
                if String::from_utf8_lossy(req.body()).contains("user.tool_confirmation") {
                    c.set(true);
                }
            }
            UnitHttpResponse::new(200).with_body("{}")
        } else if method == "POST" {
            UnitHttpResponse::new(200).with_body(CREATE_SESSION)
        } else {
            let done = confirmed.as_ref().map(|c| c.get()).unwrap_or(false);
            UnitHttpResponse::new(200).with_body(if done { EVENTS_COMPLETED } else { get_events })
        }
    }

    // Agent introspection body for card-derive tests (no skills -> synth from MCP servers).
    const AGENT_INFO: &str = r#"{"name":"Derived Agent","description":"Built from the agent.","version":2,"skills":[],"mcp_servers":[{"name":"finance"},{"name":"hris"}],"metadata":{}}"#;

    fn agent_backend() -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        move |req: UnitHttpRequest| {
            let path = req.header(":path").unwrap_or_default();
            if path.contains("/v1/agents/") {
                UnitHttpResponse::new(200).with_body(AGENT_INFO)
            } else {
                UnitHttpResponse::new(404).with_body("{}")
            }
        }
    }

    // Agent with one custom skill ref; resolve_backend serves the Skills API lookups.
    const AGENT_WITH_SKILL: &str = r#"{"name":"Skilled Agent","description":"d","version":1,"skills":[{"skill_id":"skill_x","type":"custom","version":"latest"}],"mcp_servers":[]}"#;

    fn resolve_backend() -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        move |req: UnitHttpRequest| {
            let path = req.header(":path").unwrap_or_default();
            if path.contains("/versions/") {
                UnitHttpResponse::new(200)
                    .with_body(r#"{"name":"Expense Report Helper","description":"Builds expense reports."}"#)
            } else if path.contains("/v1/skills/") {
                UnitHttpResponse::new(200).with_body(r#"{"display_title":"Expense Report Helper","latest_version":"123"}"#)
            } else if path.contains("/v1/agents/") {
                UnitHttpResponse::new(200).with_body(AGENT_WITH_SKILL)
            } else {
                UnitHttpResponse::new(404).with_body("{}")
            }
        }
    }

    fn message_send(context_id: &str, text: &str) -> String {
        json!({
            "jsonrpc": "2.0", "id": "1", "method": "message/send",
            "params": {"message": {"role": "user", "contextId": context_id,
                "parts": [{"type": "text", "text": text}]}}
        })
        .to_string()
    }

    fn a2a_post(body: String) -> UnitHttpRequest {
        UnitHttpRequest::post()
            .with_path("/")
            .with_header("content-type", "application/json")
            .with_property(vec!["xds", "cluster_name"], b"anthropic-cluster".to_vec())
            .with_body(body)
    }

    fn result_state(response: &UnitHttpResponse) -> serde_json::Value {
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        body["result"]["status"]["state"].clone()
    }

    // ── message/send: basic mapping ───────────────────────────────────────────

    #[test]
    fn message_send_completes() {
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, false))
            .with_http_upstream("anthropic-cluster", claude_backend(EVENTS_COMPLETED))
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-1", "hello")));
        assert_eq!(response.status_code(), 200);
        assert_eq!(result_state(&response), "completed");
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        // Canonical A2A v0.3.0 discriminators (what the io.a2a.spec SDK requires).
        assert_eq!(body["result"]["kind"], "task");
        assert_eq!(body["result"]["artifacts"][0]["parts"][0]["kind"], "text");
        assert_eq!(body["result"]["artifacts"][0]["parts"][0]["text"], "Hello from Claude!");
        assert!(body["result"]["artifacts"][0]["parts"][0].get("mediaType").is_none());
        // Task.metadata carries token usage + tool-call count.
        assert_eq!(body["result"]["metadata"]["tokenUsage"]["inputTokens"], 42);
        assert_eq!(body["result"]["metadata"]["tokenUsage"]["outputTokens"], 17);
        assert_eq!(body["result"]["metadata"]["toolCalls"], 0);
    }

    #[test]
    fn message_send_failed() {
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, false))
            .with_http_upstream("anthropic-cluster", claude_backend(EVENTS_FAILED))
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-2", "boom")));
        assert_eq!(result_state(&response), "failed");
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, false))
            .with_entrypoint(crate::configure);
        let body = json!({"jsonrpc":"2.0","id":"2","method":"tasks/get","params":{"id":"t1"}}).to_string();
        let response = tester.request(a2a_post(body));
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[test]
    fn agent_card_is_served() {
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, true))
            .with_entrypoint(crate::configure);
        let response = tester.request(UnitHttpRequest::get().with_path("/a2a/.well-known/agent-card.json"));
        assert_eq!(response.status_code(), 200);
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(body["name"], "Claude Bridge");
    }

    #[test]
    fn local_mode_card_no_panic() {
        let mut tester = UnitTestBuilder::default()
            .local_mode()
            .with_config(config(None, true))
            .with_entrypoint(crate::configure);
        let response = tester.request(UnitHttpRequest::get().with_path("/a2a/.well-known/agent-card.json"));
        assert_eq!(response.status_code(), 200);
    }

    // ── multi-turn: scope to the current turn, not the whole session history ──

    // Two turns of full session history. The events list the API returns is the
    // whole append-only session; turn 1 ends at index 3, turn 2 spans 4..8. Each
    // turn's `user.message` carries the id the POST echoes back (msg-1 / msg-2).
    const TURN1_HISTORY: &str = r#"{"data":[
        {"type":"user.message","id":"msg-1"},
        {"type":"span.model_request_end","model_usage":{"input_tokens":5,"output_tokens":5}},
        {"type":"agent.message","content":[{"type":"text","text":"answer one"}]},
        {"type":"session.status_idle","stop_reason":{"type":"end_turn"}}
    ]}"#;

    const TWO_TURN_HISTORY: &str = r#"{"data":[
        {"type":"user.message","id":"msg-1"},
        {"type":"span.model_request_end","model_usage":{"input_tokens":5,"output_tokens":5}},
        {"type":"agent.message","content":[{"type":"text","text":"answer one"}]},
        {"type":"session.status_idle","stop_reason":{"type":"end_turn"}},
        {"type":"user.message","id":"msg-2"},
        {"type":"span.model_request_end","model_usage":{"input_tokens":7,"output_tokens":9}},
        {"type":"agent.message","content":[{"type":"text","text":"answer two"}]},
        {"type":"session.status_idle","stop_reason":{"type":"end_turn"}}
    ]}"#;

    /// Multi-turn backend: POST .../events echoes a distinct user.message id per
    /// turn (msg-1, then msg-2); GET returns the session history GROWING by turn.
    fn multiturn_backend() -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        let turn = Rc::new(Cell::new(0u32));
        move |req: UnitHttpRequest| {
            let method = req.header(":method").unwrap_or_default().to_string();
            let path = req.header(":path").unwrap_or_default().to_string();
            if method == "POST" && path.contains("/events") {
                let n = turn.get() + 1;
                turn.set(n);
                let id = if n == 1 { "msg-1" } else { "msg-2" };
                UnitHttpResponse::new(200)
                    .with_body(format!(r#"{{"data":[{{"type":"user.message","id":"{id}"}}]}}"#))
            } else if method == "POST" {
                UnitHttpResponse::new(200).with_body(CREATE_SESSION)
            } else if turn.get() <= 1 {
                UnitHttpResponse::new(200).with_body(TURN1_HISTORY)
            } else {
                UnitHttpResponse::new(200).with_body(TWO_TURN_HISTORY)
            }
        }
    }

    #[test]
    fn multi_turn_scopes_to_current_turn() {
        // Two message/send calls on the SAME contextId reuse one Claude session.
        // The second turn's GET returns the FULL history (both turns). The bridge
        // must answer with THIS turn's message + usage, not the prior turn's.
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, false))
            .with_http_upstream("anthropic-cluster", multiturn_backend())
            .with_entrypoint(crate::configure);

        let r1 = tester.request(a2a_post(message_send("ctx-mt", "first question")));
        assert_eq!(result_state(&r1), "completed");
        let b1: serde_json::Value = serde_json::from_slice(r1.body()).unwrap();
        assert_eq!(b1["result"]["artifacts"][0]["parts"][0]["text"], "answer one");
        assert_eq!(b1["result"]["metadata"]["tokenUsage"]["inputTokens"], 5);

        let r2 = tester.request(a2a_post(message_send("ctx-mt", "second question")));
        assert_eq!(result_state(&r2), "completed");
        let b2: serde_json::Value = serde_json::from_slice(r2.body()).unwrap();
        // Anchored on msg-2: this turn's answer, NOT the stale "answer one".
        assert_eq!(b2["result"]["artifacts"][0]["parts"][0]["text"], "answer two");
        // Usage is THIS turn only (7/9), not the 12/14 sum of both turns'.
        assert_eq!(b2["result"]["metadata"]["tokenUsage"]["inputTokens"], 7);
        assert_eq!(b2["result"]["metadata"]["tokenUsage"]["outputTokens"], 9);
    }

    // ── tool-confirmation gate: end-to-end ────────────────────────────────────

    #[test]
    fn tool_defer_surfaces_input_required() {
        // defaultAction defer, no matching rule -> the tool is surfaced to the caller.
        let tc = json!({"defaultAction": "defer"});
        let mut tester = UnitTestBuilder::default()
            .with_config(config(Some(tc), false))
            .with_http_upstream("anthropic-cluster", claude_backend(EVENTS_REQUIRES_TOOL))
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-defer", "raise my card limit")));
        assert_eq!(result_state(&response), "input-required");
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        // Canonical status.message: kind "message", role "agent", text parts.
        assert_eq!(body["result"]["status"]["message"]["kind"], "message");
        assert_eq!(body["result"]["status"]["message"]["role"], "agent");
        assert_eq!(body["result"]["status"]["message"]["parts"][0]["kind"], "text");
        let msg = body["result"]["status"]["message"]["parts"][0]["text"].as_str().unwrap();
        assert!(msg.contains("set_card_limit"), "message should name the tool: {}", msg);
    }

    #[test]
    fn tool_defer_uses_custom_prompt() {
        // a defer rule with a custom prompt -> that prompt is surfaced in INPUT_REQUIRED.
        let tc = json!({"defaultAction":"deny","rules":[
            {"tool":"tool_use:set_card_limit","action":"defer","prompt":"Finance approval needed to run ${{tool_name}}."}
        ]});
        let mut tester = UnitTestBuilder::default()
            .with_config(config(Some(tc), false))
            .with_http_upstream("anthropic-cluster", claude_backend(EVENTS_REQUIRES_TOOL))
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-prompt", "raise my card limit")));
        assert_eq!(result_state(&response), "input-required");
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        let msg = body["result"]["status"]["message"]["parts"][0]["text"].as_str().unwrap();
        assert!(msg.contains("Finance approval needed to run set_card_limit."), "{}", msg);
    }

    #[test]
    fn tool_auto_allow_completes() {
        // allow rule -> confirmation relayed automatically -> session advances -> completed.
        let tc = json!({"defaultAction":"deny","rules":[{"tool":"tool_use:set_card_limit","action":"allow"}]});
        let mut tester = UnitTestBuilder::default()
            .with_config(config(Some(tc), false))
            .with_http_upstream("anthropic-cluster", gating_backend())
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-allow", "raise my card limit")));
        assert_eq!(result_state(&response), "completed");
    }

    #[test]
    fn tool_auto_deny_completes() {
        // deny rule -> auto-denied (NOT surfaced) -> session advances -> completed.
        let tc = json!({"defaultAction":"defer","rules":[{"tool":"tool_use:set_card_limit","action":"deny","message":"needs finance approval"}]});
        let mut tester = UnitTestBuilder::default()
            .with_config(config(Some(tc), false))
            .with_http_upstream("anthropic-cluster", gating_backend())
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-deny", "raise my card limit")));
        assert_eq!(result_state(&response), "completed");
    }

    // ── HITL resume: approve/deny decided by the LLM classifier ───────────────

    // Forced-tool /v1/messages responses the classifier mock returns.
    const CLASSIFY_APPROVE: &str = r#"{"content":[{"type":"tool_use","name":"record_decision","input":{"decision":"approve","reason":"clear approval"}}]}"#;
    const CLASSIFY_DENY: &str = r#"{"content":[{"type":"tool_use","name":"record_decision","input":{"decision":"deny","reason":"not an approval"}}]}"#;

    /// Backend for a two-turn HITL flow: turn 1 defers (requires_tool); turn 2 is the
    /// resume. `/v1/messages` (the classifier) returns `classify_body` when `classify_ok`,
    /// else HTTP 500. Captures the relayed confirmation result ("allow"/"deny") via `relayed`.
    fn hitl_resume_backend(
        classify_body: &'static str,
        classify_ok: bool,
        relayed: Rc<RefCell<String>>,
    ) -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        let confirmed = Rc::new(Cell::new(false));
        move |req: UnitHttpRequest| {
            let method = req.header(":method").unwrap_or_default().to_string();
            let path = req.header(":path").unwrap_or_default().to_string();
            if path.contains("/v1/messages") {
                return if classify_ok {
                    UnitHttpResponse::new(200).with_body(classify_body)
                } else {
                    UnitHttpResponse::new(500).with_body("{}")
                };
            }
            if method == "POST" && path.contains("/events") {
                let body = String::from_utf8_lossy(req.body()).to_string();
                if body.contains("user.tool_confirmation") {
                    confirmed.set(true);
                    if body.contains("\"result\":\"allow\"") {
                        *relayed.borrow_mut() = "allow".to_string();
                    } else if body.contains("\"result\":\"deny\"") {
                        *relayed.borrow_mut() = "deny".to_string();
                    }
                }
                return UnitHttpResponse::new(200).with_body("{}");
            }
            if method == "POST" {
                return UnitHttpResponse::new(200).with_body(CREATE_SESSION);
            }
            UnitHttpResponse::new(200)
                .with_body(if confirmed.get() { EVENTS_COMPLETED } else { EVENTS_REQUIRES_TOOL })
        }
    }

    fn run_hitl(ctx: &str, classify_body: &'static str, classify_ok: bool, resume_text: &str) -> String {
        let relayed = Rc::new(RefCell::new(String::new()));
        let mut tester = UnitTestBuilder::default()
            .with_config(config(Some(json!({ "defaultAction": "defer" })), false))
            .with_http_upstream(
                "anthropic-cluster",
                hitl_resume_backend(classify_body, classify_ok, relayed.clone()),
            )
            .with_entrypoint(crate::configure);
        // turn 1: defer
        let r1 = tester.request(a2a_post(message_send(ctx, "raise my card limit")));
        assert_eq!(result_state(&r1), "input-required", "turn 1 should defer");
        // turn 2: resume
        tester.request(a2a_post(message_send(ctx, resume_text)));
        let out = relayed.borrow().clone();
        out
    }

    #[test]
    fn hitl_resume_clean_approve_relays_allow() {
        assert_eq!(run_hitl("ctx-r1", CLASSIFY_APPROVE, true, "approve"), "allow");
    }

    #[test]
    fn hitl_resume_broker_verbose_approve_relays_allow() {
        // The OLD word-matcher would DENY this (it contains "do not"); the classifier approves,
        // and the bridge relays the classifier's decision rather than pattern-matching the text.
        let resume = "The user approved. Proceed and create it, but do not include any extra commentary.";
        assert_eq!(run_hitl("ctx-r2", CLASSIFY_APPROVE, true, resume), "allow");
    }

    #[test]
    fn hitl_resume_deny_relays_deny() {
        assert_eq!(run_hitl("ctx-r3", CLASSIFY_DENY, true, "no, reject that"), "deny");
    }

    #[test]
    fn hitl_resume_classifier_failure_fails_closed() {
        // classifier call fails (HTTP 500) -> deny, even though the reply says "approve".
        assert_eq!(run_hitl("ctx-r4", CLASSIFY_APPROVE, false, "approve"), "deny");
    }

    // ── tool_gate resolver: pure unit tests ───────────────────────────────────

    fn auth_fail_backend() -> impl Fn(UnitHttpRequest) -> UnitHttpResponse + 'static {
        move |req: UnitHttpRequest| {
            if req.header(":method").unwrap_or_default() == "POST" {
                UnitHttpResponse::new(401)
                    .with_body(r#"{"type":"error","error":{"type":"authentication_error"}}"#)
            } else {
                UnitHttpResponse::new(200).with_body("{}")
            }
        }
    }

    #[test]
    fn message_send_auth_error_maps_to_auth_code() {
        // Claude 401 on create-session -> classified auth error (-32010), not generic -32603.
        let mut tester = UnitTestBuilder::default()
            .with_config(config(None, false))
            .with_http_upstream("anthropic-cluster", auth_fail_backend())
            .with_entrypoint(crate::configure);
        let response = tester.request(a2a_post(message_send("ctx-auth", "hi")));
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(body["error"]["code"], -32010);
    }

    #[test]
    fn card_derive_from_agent() {
        // agentCardSource=derive, no pasted card -> card built from GET /v1/agents/{id};
        // skills synthesized from the agent's MCP servers.
        let cfg = json!({
            "agentId": "agent-test", "environmentId": "env-test", "apiKey": "sk-test",
            "agentCardSource": "derive"
        });
        let mut tester = UnitTestBuilder::default()
            .with_config(cfg.to_string())
            .with_http_upstream("anthropic-cluster", agent_backend())
            .with_entrypoint(crate::configure);
        let req = UnitHttpRequest::get()
            .with_path("/a2a/.well-known/agent-card.json")
            .with_property(vec!["xds", "cluster_name"], b"anthropic-cluster".to_vec());
        let response = tester.request(req);
        assert_eq!(response.status_code(), 200);
        let card: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(card["name"], "Derived Agent");
        let skills = card["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0]["name"], "finance");
        assert_eq!(skills[0]["tags"][0], "mcp");
    }

    #[test]
    fn card_derive_with_partial_override() {
        // derive + a PARTIAL agentCard (skills only): name/description derived, skills overridden.
        // Also proves configure() does not reject a partial card in derive mode.
        let cfg = json!({
            "agentId": "agent-test", "environmentId": "env-test", "apiKey": "sk-test",
            "agentCardSource": "derive",
            "agentCard": "{\"skills\":[{\"id\":\"curated\",\"name\":\"Curated\",\"description\":\"hand authored\",\"tags\":[\"x\"]}]}"
        });
        let mut tester = UnitTestBuilder::default()
            .with_config(cfg.to_string())
            .with_http_upstream("anthropic-cluster", agent_backend())
            .with_entrypoint(crate::configure);
        let req = UnitHttpRequest::get()
            .with_path("/a2a/.well-known/agent-card.json")
            .with_property(vec!["xds", "cluster_name"], b"anthropic-cluster".to_vec());
        let response = tester.request(req);
        assert_eq!(response.status_code(), 200);
        let card: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(card["name"], "Derived Agent");
        assert_eq!(card["skills"].as_array().unwrap().len(), 1);
        assert_eq!(card["skills"][0]["id"], "curated");
    }

    #[test]
    fn card_derive_resolves_skills() {
        // a skill_id ref is resolved (2-call Skills API lookup) to name + description.
        let cfg = json!({
            "agentId": "agent-test", "environmentId": "env-test", "apiKey": "sk-test",
            "agentCardSource": "derive"
        });
        let mut tester = UnitTestBuilder::default()
            .with_config(cfg.to_string())
            .with_http_upstream("anthropic-cluster", resolve_backend())
            .with_entrypoint(crate::configure);
        let req = UnitHttpRequest::get()
            .with_path("/a2a/.well-known/agent-card.json")
            .with_property(vec!["xds", "cluster_name"], b"anthropic-cluster".to_vec());
        let response = tester.request(req);
        assert_eq!(response.status_code(), 200);
        let card: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(card["skills"][0]["name"], "Expense Report Helper");
        assert_eq!(card["skills"][0]["id"], "expense-report-helper");
        assert_eq!(card["skills"][0]["description"], "Builds expense reports.");
    }

    mod card {
        use crate::contracts::agent::{AgentInfo, AgentSkill, McpServerRef};
        use crate::services::card_builder::build_card;

        #[test]
        fn fallbacks_when_agent_is_bare() {
            let a = AgentInfo::default();
            let c = build_card("agent_X", &a, None, Some("host.example"), &[]);
            assert_eq!(c["name"], "agent_X");
            assert!(c["description"].as_str().unwrap().contains("agent_X"));
            assert_eq!(c["skills"][0]["id"], "general-assistance");
            assert_eq!(c["url"], "https://host.example/");
            assert_eq!(c["protocolVersion"], "0.3.0");
            assert_eq!(c["preferredTransport"], "JSONRPC");
            assert_eq!(c["capabilities"]["streaming"], false);
        }

        #[test]
        fn synthesizes_skills_from_mcp_servers() {
            let mut a = AgentInfo::default();
            a.name = Some("Fin".into());
            a.mcp_servers = vec![
                McpServerRef { name: Some("finance".into()) },
                McpServerRef { name: Some("hris".into()) },
            ];
            let c = build_card("id", &a, None, None, &[]);
            let sk = c["skills"].as_array().unwrap();
            assert_eq!(sk.len(), 2);
            assert_eq!(sk[0]["name"], "finance");
        }

        #[test]
        fn real_skills_combined_with_mcp() {
            // real skills ingested AND mcp synthesized (deduped), real first.
            let mut a = AgentInfo::default();
            a.skills = vec![AgentSkill {
                id: Some("s1".into()), name: Some("Skill One".into()), ..Default::default()
            }];
            a.mcp_servers = vec![McpServerRef { name: Some("finance".into()) }];
            let c = build_card("id", &a, None, None, &[]);
            let sk = c["skills"].as_array().unwrap();
            assert_eq!(sk.len(), 2);
            assert_eq!(sk[0]["id"], "s1");
            assert_eq!(sk[0]["description"], "Skill One");
            assert_eq!(sk[1]["id"], "finance");
        }

        #[test]
        fn real_skill_refs_mapped() {
            // managed-agent skills are refs {skill_id,type} -> mapped to card skills.
            let mut a = AgentInfo::default();
            a.skills = vec![AgentSkill {
                skill_id: Some("pdf".into()), skill_type: Some("anthropic".into()), ..Default::default()
            }];
            let c = build_card("id", &a, None, None, &[]);
            let sk = c["skills"].as_array().unwrap();
            assert_eq!(sk.len(), 1);
            assert_eq!(sk[0]["id"], "pdf");
            assert_eq!(sk[0]["tags"][0], "anthropic");
        }

        #[test]
        fn mcp_synth_excludes_infra() {
            let mut a = AgentInfo::default();
            a.mcp_servers = vec![
                McpServerRef { name: Some("finance".into()) },
                McpServerRef { name: Some("team_slack".into()) },
            ];
            let exclude = vec!["*_slack".to_string()];
            let c = build_card("id", &a, None, None, &exclude);
            let ids: Vec<String> = c["skills"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(ids, vec!["finance"]);
        }

        #[test]
        fn pasted_override_wins() {
            let a = AgentInfo::default();
            let c = build_card("id", &a, Some(r#"{"name":"Override Name"}"#), None, &[]);
            assert_eq!(c["name"], "Override Name");
        }
    }

    mod gate {
        use crate::generated::config::{Rules0Config, ToolConfirmationConfig};
        use crate::services::tool_gate::{glob_match, resolve, Action, PendingTool};

        fn tool(name: &str) -> PendingTool {
            PendingTool { tool_use_id: "id".into(), event_type: "agent.tool_use".into(), name: name.into(), server: None }
        }
        fn mcp(server: &str, name: &str) -> PendingTool {
            PendingTool { tool_use_id: "id".into(), event_type: "agent.mcp_tool_use".into(), name: name.into(), server: Some(server.into()) }
        }
        fn rule(t: &str, a: &str) -> Rules0Config {
            Rules0Config { tool: t.into(), action: a.into(), message: None, prompt: None }
        }
        fn cfg(default: &str, rules: Vec<Rules0Config>) -> ToolConfirmationConfig {
            ToolConfirmationConfig { default_action: Some(default.into()), rules: Some(rules), log_decisions: None }
        }

        #[test]
        fn glob_matching() {
            assert!(glob_match("tool_use:bash", "tool_use:bash"));
            assert!(glob_match("mcp_tool_use:finance/set_card_limit", "mcp_tool_use:finance/*"));
            assert!(glob_match("tool_use:delete_file", "*delete*"));
            assert!(!glob_match("tool_use:read", "tool_use:write"));
            assert!(glob_match("anything", "*"));
        }

        #[test]
        fn first_matching_rule_wins() {
            let c = cfg("defer", vec![rule("tool_use:bash", "deny"), rule("tool_use:*", "allow")]);
            assert_eq!(resolve(&[tool("bash")], Some(&c))[0].action, Action::Deny);
            assert_eq!(resolve(&[tool("read")], Some(&c))[0].action, Action::Allow);
        }

        #[test]
        fn unmatched_uses_default() {
            let c = cfg("deny", vec![rule("tool_use:read", "allow")]);
            assert_eq!(resolve(&[tool("write")], Some(&c))[0].action, Action::Deny);
        }

        #[test]
        fn default_is_defer_when_absent() {
            assert_eq!(resolve(&[tool("anything")], None)[0].action, Action::Defer);
        }

        #[test]
        fn mcp_key_format() {
            let c = cfg("defer", vec![rule("mcp_tool_use:hris/*", "allow")]);
            assert_eq!(resolve(&[mcp("hris", "get_employee")], Some(&c))[0].action, Action::Allow);
        }

        #[test]
        fn prompt_placeholders_render() {
            let r = Rules0Config { tool: "tool_use:bash".into(), action: "defer".into(), message: None, prompt: Some("Allow ${{tool_name}} (${{tool_key}}) to run?".into()) };
            let c = cfg("deny", vec![r]);
            let d = resolve(&[tool("bash")], Some(&c));
            assert_eq!(d[0].prompt.as_deref(), Some("Allow bash (tool_use:bash) to run?"));
        }

        #[test]
        fn mcp_server_placeholder_renders() {
            let r = Rules0Config { tool: "mcp_tool_use:hris/*".into(), action: "defer".into(), message: None, prompt: Some("${{mcp_server}} wants ${{tool_name}}".into()) };
            let c = cfg("defer", vec![r]);
            let d = resolve(&[mcp("hris", "get_employee")], Some(&c));
            assert_eq!(d[0].prompt.as_deref(), Some("hris wants get_employee"));
        }
    }
}
