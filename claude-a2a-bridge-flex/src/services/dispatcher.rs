// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Orchestrates a `message/send` turn against Claude Managed Agents, including
//! the HITL tool-confirmation gate.
//!
//! - New turn: create/reuse session → send `user.message` → poll to a terminal state.
//! - On `requires_action`: resolve each pending tool via `tool_gate`. allow/deny
//!   are relayed automatically (`user.tool_confirmation`); deferred tools are
//!   persisted on the session and surfaced as `INPUT_REQUIRED`.
//! - Resume turn: a session with pending deferred tools treats the next message
//!   as the human's approve/deny and relays it, then polls to completion.
//!
//! ## Turn scoping (multi-turn correctness)
//!
//! The Managed Agents events list (`GET …/events`) returns the FULL append-only
//! session history. A session is reused across turns on the same A2A `contextId`,
//! so a follow-up `message/send` lands on a session that is already `idle` from
//! the prior turn. Two failure modes follow if the whole history is interpreted
//! naively: (1) the first poll can observe the PRIOR turn's terminal
//! `session.status_idle` before this turn's events are visible and return the
//! stale answer; (2) token usage sums every turn's `model_usage`. Both are fixed
//! by anchoring on the `user.message` event id returned by the POST that opened
//! the turn and interpreting only events *after* it (see [`current_turn`]).

use std::collections::HashSet;
use std::time::Duration;

use pdk::data_storage::DataStorage;
use pdk::hl::timer::Timer;
use pdk::hl::{HttpClient, Service};
use pdk::logger;

use crate::access_log;

use crate::session_store::{SessionEntry, SessionStore};
use crate::task_store::{Message, Part, Role, TaskEntry, TaskState, TaskStatus, TaskStore};

use crate::contracts::message::{Event, SendEventsRequest};
use crate::contracts::session::{ClaudeSessionData, CreateSessionRequest, PendingToolState};
use crate::generated::config::ToolConfirmationConfig;
use crate::services::claude_client::{self, ClaudeApiError, Confirmation};
use crate::services::response_mapper::{self, MappedResponse};
use crate::services::tool_gate::{self, Action, PendingTool};

/// Turn-level inputs that don't change between calls in one dispatch.
pub struct ClaudeCtx<'a> {
    pub http_client: &'a HttpClient,
    pub service: &'a Service,
    pub api_key: &'a str,
    pub version: &'a str,
    pub beta: &'a str,
    pub timeout_ms: u64,
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatch<S: DataStorage>(
    cx: &ClaudeCtx<'_>,
    storage: &S,
    timer: &Timer,
    agent_id: &str,
    environment_id: &str,
    context_id: &str,
    user_id: &str,
    task_id: &str,
    text: &str,
    poll_interval_ms: u64,
    max_poll_attempts: u32,
    tool_cfg: Option<&ToolConfirmationConfig>,
) -> Result<MappedResponse, ClaudeApiError> {
    let session_store = SessionStore::new(storage);
    let task_store = TaskStore::new(storage);

    // Load or create session.
    let mut entry: SessionEntry<ClaudeSessionData> =
        match session_store.load::<ClaudeSessionData>(context_id).await {
            Some(e) => e,
            None => {
                let req = CreateSessionRequest {
                    agent: agent_id.to_string(),
                    environment_id: environment_id.to_string(),
                };
                let resp = claude_client::create_session(
                    cx.http_client, cx.service, cx.api_key, cx.version, cx.beta, &req, cx.timeout_ms,
                )
                .await?;
                let new = SessionEntry {
                    created_at: 0,
                    platform: ClaudeSessionData { session_id: resp.id, pending: vec![] },
                };
                session_store.create(context_id, &new).await;
                new
            }
        };
    let session_id = entry.platform.session_id.clone();
    let audit = tool_cfg.and_then(|c| c.log_decisions).unwrap_or(false);

    // Already-confirmed tool ids in this dispatch, so repeated polls of the same
    // requires_action don't re-confirm before the session advances.
    let mut confirmed: HashSet<String> = HashSet::new();

    // Turn anchor: the `user.message` event id that opens this turn. Status,
    // answer, and usage are read only from events AFTER it (see module docs).
    // A resume relays `user.tool_confirmation` (no new user.message), so it has
    // no anchor and falls back to the last user.message in the history.
    let mut anchor: Option<String> = None;

    if !entry.platform.pending.is_empty() {
        // RESUME: this message is the human's approve/deny for the deferred tools,
        // possibly relayed/paraphrased by a broker. Classify intent with a one-shot
        // LLM call instead of brittle word-matching; fail CLOSED (deny) on any
        // classifier failure.
        let (approved, reason) = claude_client::classify_approval(
            cx.http_client, cx.service, cx.api_key, cx.version, text, cx.timeout_ms,
        )
        .await
        .unwrap_or((false, "classifier unavailable; failed closed".to_string()));
        let result = if approved { "allow" } else { "deny" };
        let confirmations: Vec<Confirmation> = entry
            .platform
            .pending
            .iter()
            .map(|p| (p.tool_use_id.clone(), result, p.message.clone()))
            .collect();
        logger::debug!(
            "[claude-bridge] resume: relaying '{}' for {} deferred tool(s)",
            result,
            confirmations.len()
        );
        if audit {
            access_log::info(format!(
                "tool-confirmation resume: relaying '{}' for {} deferred tool(s) — {} (context_id={}, task_id={}, user_id={})",
                result, confirmations.len(), reason, context_id, task_id, user_id
            ));
        }
        for c in &confirmations {
            confirmed.insert(c.0.clone());
        }
        claude_client::send_confirmations(
            cx.http_client, cx.service, cx.api_key, cx.version, cx.beta, &session_id, &confirmations, cx.timeout_ms,
        )
        .await?;
        entry.platform.pending.clear();
        session_store.save(context_id, &entry).await;
    } else {
        let send_req = SendEventsRequest::user_message(text);
        anchor = claude_client::send_events(
            cx.http_client, cx.service, cx.api_key, cx.version, cx.beta, &session_id, &send_req, cx.timeout_ms,
        )
        .await?;
    }

    // Poll to a terminal state, gating tools along the way.
    let mut outcome: Option<MappedResponse> = None;
    for _ in 0..max_poll_attempts.max(1) {
        let list = claude_client::list_events(
            cx.http_client, cx.service, cx.api_key, cx.version, cx.beta, &session_id, cx.timeout_ms,
        )
        .await?;
        // Restrict to the current turn. If the just-sent message isn't visible
        // yet (POST→GET lag), this is empty → no status → we poll again rather
        // than read the prior turn's terminal status.
        let turn = current_turn(&list.data, anchor.as_deref());

        match last_status(turn) {
            Some(Status::RequiresAction(event_ids)) => {
                let to_handle: Vec<String> =
                    event_ids.into_iter().filter(|id| !confirmed.contains(id)).collect();
                if to_handle.is_empty() {
                    timer.sleep(Duration::from_millis(poll_interval_ms)).await;
                    continue;
                }
                let pending = extract_pending(turn, &to_handle);
                let decisions = tool_gate::resolve(&pending, tool_cfg);

                let mut autos: Vec<Confirmation> = Vec::new();
                let mut deferred: Vec<PendingToolState> = Vec::new();
                for d in decisions {
                    if audit {
                        let line = format!(
                            "tool-confirmation {:?} for {} (context_id={}, task_id={}, user_id={})",
                            d.action, d.key, context_id, task_id, user_id
                        );
                        match d.action {
                            Action::Deny => access_log::warn(line),
                            _ => access_log::info(line),
                        }
                    }
                    match d.action {
                        Action::Allow => autos.push((d.tool_use_id, "allow", None)),
                        Action::Deny => autos.push((d.tool_use_id, "deny", d.message)),
                        Action::Defer => deferred.push(PendingToolState {
                            tool_use_id: d.tool_use_id,
                            key: d.key,
                            name: d.name,
                            message: d.message,
                            prompt: d.prompt,
                        }),
                    }
                }

                if !autos.is_empty() {
                    for c in &autos {
                        confirmed.insert(c.0.clone());
                    }
                    claude_client::send_confirmations(
                        cx.http_client, cx.service, cx.api_key, cx.version, cx.beta, &session_id, &autos, cx.timeout_ms,
                    )
                    .await?;
                }

                if !deferred.is_empty() {
                    // Per-rule approval prompt when set, else a generic per-tool line; the
                    // reply instruction is always appended so approve/deny stays discoverable.
                    let lines: Vec<String> = deferred
                        .iter()
                        .map(|p| match p.prompt.as_deref() {
                            Some(pr) if !pr.is_empty() => pr.to_string(),
                            _ => format!("The agent needs approval to use: {}.", p.name),
                        })
                        .collect();
                    entry.platform.pending = deferred;
                    session_store.save(context_id, &entry).await;
                    let msg = format!(
                        "{} Reply \"approve\" to allow or \"deny\" to reject.",
                        lines.join(" ")
                    );
                    outcome = Some(MappedResponse {
                        task_state: TaskState::InputRequired,
                        artifacts: vec![],
                        text: Some(msg),
                        metadata: response_mapper::usage_metadata(turn),
                    });
                    break;
                }
                // Auto-confirmations relayed — re-poll immediately for the next events
                // (the waiting/running paths below handle the sleep when nothing changed).
            }
            Some(Status::Done) => {
                outcome = Some(response_mapper::completed(turn));
                break;
            }
            Some(Status::Failed) => {
                outcome = Some(MappedResponse {
                    task_state: TaskState::Failed,
                    artifacts: vec![],
                    text: Some("The agent session reported an error.".to_string()),
                    metadata: response_mapper::usage_metadata(turn),
                });
                break;
            }
            _ => {
                timer.sleep(Duration::from_millis(poll_interval_ms)).await;
            }
        }
    }

    let mapped = outcome.unwrap_or(MappedResponse {
        task_state: TaskState::Working,
        artifacts: vec![],
        text: Some("The agent did not complete within the configured poll budget.".to_string()),
        metadata: None,
    });

    // Persist task entry (status.message kept for non-completed states).
    let status_message = match (&mapped.task_state, &mapped.text) {
        (TaskState::Completed, _) => None,
        (_, Some(t)) => Some(Message {
            role: Role::Agent,
            message_id: task_id.to_string(),
            parts: vec![Part::Text { text: t.clone() }],
            ts: 0,
        }),
        _ => None,
    };
    let task_entry = TaskEntry {
        task_id: task_id.to_string(),
        context_id: context_id.to_string(),
        user_id: user_id.to_string(),
        message_id: task_id.to_string(),
        status: TaskStatus {
            state: mapped.task_state.clone(),
            message: status_message,
            timestamp: 0,
        },
        artifacts: mapped.artifacts.clone(),
        history: vec![],
        created_at: 0,
        updated_at: 0,
    };
    if !task_store.create(&task_entry).await {
        task_store.update(&task_entry).await;
    }

    session_store.save(context_id, &entry).await;
    Ok(mapped)
}

// ── helpers ───────────────────────────────────────────────────────────────────

enum Status {
    RequiresAction(Vec<String>),
    Done,
    Failed,
    Running,
}

/// Restrict the full session-history event list to the current turn.
///
/// `anchor` is the `user.message` event id that opened this turn (returned by
/// the POST). When present, the turn is everything strictly after that event;
/// if the anchor isn't in the list yet (POST→GET visibility lag) the turn is
/// empty, so the caller keeps polling instead of reading a prior turn's status.
///
/// When there is no anchor (a resume relays a `tool_confirmation`, not a new
/// `user.message`), fall back to the LAST `user.message` in the history — the
/// message that opened the logical turn now completing. With neither (e.g. a
/// mock backend that echoes no events) the whole list is used, preserving the
/// original single-turn behaviour.
fn current_turn<'a>(events: &'a [Event], anchor: Option<&str>) -> &'a [Event] {
    if let Some(id) = anchor {
        return match events.iter().position(|e| e.id.as_deref() == Some(id)) {
            Some(idx) => &events[idx + 1..],
            None => &[],
        };
    }
    match events.iter().rposition(|e| e.event_type == "user.message") {
        Some(idx) => &events[idx + 1..],
        None => events,
    }
}

/// The last status-bearing event determines the turn state.
fn last_status(events: &[Event]) -> Option<Status> {
    let mut status: Option<Status> = None;
    for e in events {
        match e.event_type.as_str() {
            "session.status_idle" => {
                let requires = e
                    .stop_reason
                    .as_ref()
                    .and_then(|s| s.reason_type.as_deref())
                    == Some("requires_action");
                if requires {
                    let ids = e
                        .stop_reason
                        .as_ref()
                        .and_then(|s| s.event_ids.clone())
                        .unwrap_or_default();
                    status = Some(Status::RequiresAction(ids));
                } else {
                    status = Some(Status::Done);
                }
            }
            "session.status_terminated" => status = Some(Status::Done),
            "session.error" => status = Some(Status::Failed),
            "session.status_running" => status = Some(Status::Running),
            _ => {}
        }
    }
    status
}

/// Resolve the tool-use events named by `ids` into `PendingTool`s.
fn extract_pending(events: &[Event], ids: &[String]) -> Vec<PendingTool> {
    ids.iter()
        .map(|id| {
            let ev = events.iter().find(|e| {
                e.id.as_deref() == Some(id.as_str())
                    && (e.event_type == "agent.tool_use" || e.event_type == "agent.mcp_tool_use")
            });
            PendingTool {
                tool_use_id: id.clone(),
                event_type: ev.map(|e| e.event_type.clone()).unwrap_or_else(|| "agent.tool_use".to_string()),
                name: ev.and_then(|e| e.name.clone()).unwrap_or_default(),
                server: ev.and_then(|e| e.mcp_server_name.clone()),
            }
        })
        .collect()
}

