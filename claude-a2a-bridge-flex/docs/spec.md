# Claude A2A Bridge

Bridges A2A (Agent-to-Agent) JSON-RPC requests to the Anthropic **Claude Managed Agents** API (`https://api.anthropic.com/v1`). Translates an A2A `message/send` call into a Claude session lifecycle — `POST /v1/sessions` (create), `POST /v1/sessions/{id}/events` (submit a `user.message`), then polling `GET /v1/sessions/{id}/events` until the turn reaches a terminal state — and maps the resulting events back to a canonical A2A `Task`. JSON-RPC routing, task lifecycle, durable session/task storage, and outbound-service resolution are handled by this policy's own local `router`, `task_store`, `session_store`, and `upstream` modules.

Claude managed-agent turns are **asynchronous** (submit an event, then poll an event log to completion), so the bridge runs a **poll-to-idle loop** (paced by a PDK `Timer`).

## Configuration

### `agentId`
**Type**: `string` · **Required**: yes

The Claude managed agent id (`agent_…`), sent as `agent` in the `POST /v1/sessions` body. Also scopes this policy's storage bucket (`claude-bridge-{agentId}`).

### `environmentId`
**Type**: `string` · **Required**: yes

The managed-agent environment id (`env_…`), sent as `environment_id` in the create-session body.

### `apiKey`
**Type**: `string` · **Required**: yes

Anthropic API key, sent as the `x-api-key` header on every Claude API call. Use a Flex secret reference in production.

> **Phase-2 option**: move the credential out of policy config and onto the upstream cluster via a credential-injection policy (the bridge reuses the inbound route's cluster — see *Outbound routing* — so a credential-injection policy bound to that cluster would apply automatically). Config-held key is the v1 approach.

### `agentCard`
**Type**: `string` · **Required**: yes

The A2A agent card document (JSON string) served at the card path. In `paste` mode it is validated as a canonical A2A v0.3.0 `AgentCard` at configuration time (required top-level fields present); an invalid card fails policy load. In `derive` mode it is an optional **partial override** whose top-level keys win over the derived card. Either way it can still be **overridden** by the A2A Agent Card policy on an agent-network instance.

### `anthropicVersion`
**Type**: `string` · **Required**: no · **Default**: `2023-06-01`

Value for the `anthropic-version` header.

### `betaHeader`
**Type**: `string` · **Required**: no · **Default**: `managed-agents-2026-04-01`

Value for the `anthropic-beta` header that enables the Managed Agents API.

### `timeout`
**Type**: `integer` · **Required**: no · **Default**: `60000`

Maximum time in milliseconds to wait for a **single** Claude API call (create/send/poll). This is *not* the overall turn budget — that is `pollIntervalMs × maxPollAttempts` (see below).

### `pollIntervalMs`
**Type**: `integer` · **Required**: no · **Default**: `2000`

Delay between polls of the events endpoint while a turn is running. With `maxPollAttempts` this sets the overall turn budget (default `2000 × 150 = 300000 ms` / 5 min).

### `maxPollAttempts`
**Type**: `integer` · **Required**: no · **Default**: `150`

Maximum number of event polls before giving up on a turn (returns a non-completed task). With the default `pollIntervalMs` this yields a **~5 minute** overall response budget — sized for real LLM turns, not just trivial prompts.

### `toolConfirmation`
**Type**: `object` · **Required**: no

Human-in-the-loop gating for managed-agent tool use (see *Behavior → Tool-confirmation gate*). Shape:

- `defaultAction` — `allow` | `deny` | `defer` (an **enforced enum** in the schema), applied to tools that match no rule. **Default `defer`** (governance-safe; settable).
- `rules` — an **ordered** list of `{ tool, action, message?, prompt? }`. The **first** rule whose `tool` glob matches the tool key decides. `action` is `allow` | `deny` | `defer` (enum); `message` is an optional `deny_message` relayed *to the agent* on `deny`; `prompt` is an optional human-facing approval message surfaced *to the caller* when the rule `defer`s (TASK_STATE_INPUT_REQUIRED) — use it for a domain-specific ask instead of the generic line. A standard "Reply approve/deny" instruction is appended automatically; with multiple deferred tools each tool contributes its own `prompt` (or a generic line). Both `prompt` and `message` support runtime placeholders substituted per pending tool: `${{tool_name}}`, `${{tool_key}}` (full key, e.g. `tool_use:bash` / `mcp_tool_use:server/name`), and `${{mcp_server}}` (empty for built-in tools).
- `logDecisions` — when `true`, every decision is written to the access log (the `[accessLog]` audit channel). Default `false`.

```yaml
toolConfirmation:
  defaultAction: defer
  logDecisions: true
  rules:
    - tool: "tool_use:bash"
      action: deny
      message: "bash is not permitted"
    - tool: "tool_use:write"
      action: defer
      prompt: "The agent wants to create or modify a file in the workspace. Approve to allow it."
    - tool: "mcp_tool_use:hris/*"
      action: allow
```

When `toolConfirmation` is absent, the default action is `defer` and there are no rules (every tool the agent pauses on is surfaced to the caller).

---

Fields intentionally **not** in config, and why:

| Field | API stage | Source | Reason |
|---|---|---|---|
| session `agent` / `environment_id` | create session | `agentId` / `environmentId` config | Backend identity, injected from the linked asset |
| event `type` | send events | Hardcoded `"user.message"` | Derived from the A2A method, not caller config |
| message `content[].text` | send events | Concatenated from the A2A request | `message.parts` filtered to `kind/type: text`, joined in order |
| Claude API base host | all | Derived from the route — see *Outbound routing* | The bridge reuses the cluster the inbound route resolved (`xds.cluster_name`); the upstream host is owned by the API's route/Service. The `/v1/...` path is hardcoded by the policy (`v2` is a code change, not config) |

## Behavior

### Supported A2A methods

| A2A Method | Supported | Notes |
|---|---|---|
| `message/send` | ✅ Yes | Create session (first message per `contextId`) or reuse it, submit a `user.message` event, poll to idle, map to a `Task` |
| `message/stream` | ❌ Deferred | Claude exposes a streaming events endpoint; relaying it as A2A SSE is future work |
| `tasks/get` | ❌ Deferred | Task state is persisted (via the local `task_store::TaskStore`), so this is straightforward to add next |
| `tasks/list` | ❌ Deferred | Context/user indexes already exist in `TaskStore` |
| `tasks/cancel` | ❌ Deferred | Maps naturally to a `user.interrupt` event; not yet implemented |

Unsupported methods return JSON-RPC `-32601` (method not found).

### Session lifecycle

On the first `message/send` for a `contextId`:
1. `POST /v1/sessions` with `{ "agent": <agentId>, "environment_id": <environmentId> }`.
2. Store `{ session_id }` in the session record keyed by `contextId` (local `session_store::SessionStore`, backed by `data_storage`, TTL 30 min).

On every `message/send` (first or follow-up):
1. Submit `POST /v1/sessions/{sessionId}/events` with `{ "events": [ { "type": "user.message", "content": [ { "type": "text", "text": <joined parts> } ] } ] }`.
2. Poll `GET /v1/sessions/{sessionId}/events` every `pollIntervalMs` (up to `maxPollAttempts`) until a terminal `session.status_*` event appears.

A follow-up `message/send` with the same `contextId` reuses the stored session, so the managed agent continues the conversation naturally — no special resume message type is required.

The events list returned by `GET …/events` is the **full append-only session history**, so a follow-up turn must be scoped to its own events (see *Turn scoping* below).

#### Turn scoping

Because one Claude session is reused across every `message/send` on a `contextId`, the events list grows with each turn and a follow-up message lands on a session that is already `idle` from the prior turn. Interpreting the whole history yields two bugs:

1. **Stale answer** — the first poll can observe the *prior* turn's terminal `session.status_idle(end_turn)` before this turn's events are visible (`POST`→`GET` has a brief propagation lag), and return the previous answer.
2. **Cumulative usage** — `Task.metadata.tokenUsage` would sum every turn's `model_usage` instead of just this turn's.

**Fix:** `POST /v1/sessions/{id}/events` echoes the created event — `{ "data": [ { "id": "sevt_…", "type": "user.message" } ] }`. The dispatcher captures that id as the **turn anchor** and interprets status, answer, and usage only from events *after* it. If the anchor is not yet visible in a poll, the turn slice is empty → no terminal status → the bridge keeps polling rather than reading the prior turn. A resume relays a `user.tool_confirmation` (not a new `user.message`) and so has no anchor; it falls back to the last `user.message` in the history — the message that opened the turn now completing. See `current_turn()` in `services/dispatcher.rs`.

### Tool-confirmation gate (HITL)

When the managed agent pauses to call a tool, the turn reaches `session.status_idle` with `stop_reason.type == "requires_action"` and a list of pending tool-use `event_ids`. The bridge resolves each pending tool against `toolConfirmation`:

- **Canonical key** (matched case-insensitively, `*` globs): `tool_use:<name>` for built-in tools, `mcp_tool_use:<server>/<name>` for MCP tools (server + name read from the `agent.tool_use` / `agent.mcp_tool_use` event referenced by `event_ids`).
- **Resolution**: the first matching `rule` wins; otherwise `defaultAction`.
- **allow** → relay `user.tool_confirmation { tool_use_id, result: "allow" }`, then keep polling (the session resumes and runs the tool).
- **deny** → relay `user.tool_confirmation { tool_use_id, result: "deny", deny_message }`, then keep polling.
- **defer** → persist the pending tool(s) on the session and return `input-required`, naming the tool(s) in `status.message`. The next `message/send` on the same `contextId` is treated as the human's decision. Because that reply is often relayed/paraphrased by a broker (an LLM orchestrator), the bridge does **not** pattern-match the text — it classifies intent with a one-shot forced-tool call to the smallest Claude model (`POST /v1/messages`, `claude-haiku-4-5`, `temperature 0`, treating the reply as untrusted data): a clear approval relays `allow`, anything ambiguous/negative relays `deny`, and **any classifier failure (network, non-2xx, off-schema) fails closed to `deny`**. The bridge then resumes polling to completion.

A single turn may pause multiple times (multi-step tool use); each new pending tool is gated the same way, and already-relayed confirmations are tracked so repeated polls don't double-confirm. When `logDecisions` is `true`, each decision is written to the access log:

```
[accessLog] tool-confirmation Deny for tool_use:bash (context_id=…, task_id=…, user_id=…)
```

### Turn-state mapping

The turn's terminal state is the **last `session.status_*` event** (deterministic, language-independent):

| Event | A2A task state |
|---|---|
| `session.status_idle`, `stop_reason.type == "requires_action"` | gated (above); deferred tools → `TASK_STATE_INPUT_REQUIRED` |
| `session.status_idle` (other stop reasons) | `TASK_STATE_COMPLETED` |
| `session.status_terminated` | `TASK_STATE_COMPLETED` |
| `session.error` | `TASK_STATE_FAILED` |
| only `session.status_running` (or none yet) | not terminal — keep polling |

Answer text is collected from `agent.message` events (tolerating `content` as a string or an array of `{ "type": "text", "text": … }` blocks). Content placement avoids duplication:

| A2A state | `status.message` | `artifacts` |
|---|---|---|
| `TASK_STATE_COMPLETED` | Omitted | Set — `parts[0].text` is the agent's answer |
| `TASK_STATE_INPUT_REQUIRED` | Set — names the deferred tool(s) | Omitted |
| `TASK_STATE_FAILED` | Set — the error text | Omitted |

### Response metadata

Every response carries per-turn stats in the A2A `Task.metadata` map (omitted only if nothing was observed):

```json
"metadata": {
  "tokenUsage": { "inputTokens": 10, "outputTokens": 102, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 6714 },
  "toolCalls": 0
}
```

`tokenUsage` is summed from `model_usage` on `span.model_request_end` events; `toolCalls` counts `agent.tool_use` / `agent.mcp_tool_use` events in the turn. (The events-list envelope, `agent.message` content shape, `requires_action` signal, and `model_usage` shape are all **confirmed against the live Managed Agents API**.)

### Missing context (local mode)

When the inbound cluster (`xds.cluster_name`) can't be resolved (route not wired), `message/send` returns a JSON-RPC internal error rather than panicking. A missing `x-anypoint-user-id` header is logged as a warning (task owner is left empty).

## Architecture

### Request flow

```
A2A message/send (POST, JSON-RPC)
  → request_filter: into_headers_body_state()  (terminating — buffers headers+body together)
    → router::parse_request
      → services::dispatcher::dispatch()
        → SessionStore: load by contextId (resume if pending), or create_session() + store
        → claude_client::send_events (user.message)  |  send_confirmations (resume)
        → poll loop: list_events
            → requires_action → tool_gate::resolve → relay allow/deny  |  defer → INPUT_REQUIRED
            → terminal → response_mapper::completed + usage_metadata
        → TaskStore: persist the task entry
      → router::ok_response  →  Flow::Break(A2A Task + metadata)
```

`GET <cardPath>` short-circuits with the configured agent card. Non-POST, non-card requests `Flow::Continue` to the upstream.

### Terminating filter

The bridge buffers the whole request via `into_headers_body_state()` and answers with `Flow::Break`, so the inbound request is **never proxied upstream**. Buffering headers and body together (rather than sequential `into_headers_state()` → `into_body_state()`) prevents the router from releasing the headers and proxying the request — which would let a bodyless upstream `404`/`405` race the bridge's own response.

### Outbound routing

The bridge takes **no upstream URL as config**. Outbound calls go through the PDK `HttpClient` against a `Service` built by the local `upstream::build_upstream_service`, which:
- reads `xds.cluster_name` (the cluster the inbound route resolved) so the call loops through that cluster's outbound policy chain (e.g. credential-injection) and the gateway egress resolves the real upstream at the edge;
- uses the configured Claude host (`https://api.anthropic.com`) for the request authority.

The API instance's upstream service must therefore point at the Claude API; the policy supplies the `/v1/...` path.

### AgentCard

Served verbatim from the `agentCard` config at the card path. In `paste` mode it is validated as a canonical A2A v0.3.0 `AgentCard` at load time (required top-level fields present). In `derive` mode the policy builds the card in the same canonical shape from the managed agent's configuration. Example:

```json
{
  "protocolVersion": "0.3.0",
  "name": "Claude Bridge",
  "description": "A2A bridge to an Anthropic Claude managed agent.",
  "url": "https://gateway.example.com/",
  "preferredTransport": "JSONRPC",
  "version": "1.0.0",
  "capabilities": { "streaming": false },
  "defaultInputModes": ["text/plain"],
  "defaultOutputModes": ["text/plain"],
  "skills": [{ "id": "chat", "name": "Chat", "description": "General chat.", "tags": [] }]
}
```

## Examples

### `message/send` — completed

**Request**

```bash
curl 'http://127.0.0.1:8186' \
  -H 'Content-Type: application/json' \
  -d '{
    "jsonrpc": "2.0",
    "id": "1",
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "contextId": "ctx-001",
        "parts": [ { "kind": "text", "text": "Summarize the latest order for Acme." } ]
      }
    }
  }'
```

**Response**

```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "result": {
    "kind": "task",
    "id": "task-…",
    "contextId": "ctx-001",
    "status": { "state": "completed" },
    "artifacts": [
      { "artifactId": "answer-…", "parts": [ { "kind": "text", "text": "Acme's latest order …" } ] }
    ],
    "metadata": {
      "tokenUsage": { "inputTokens": 10, "outputTokens": 102, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 6714 },
      "toolCalls": 0
    }
  }
}
```

### `message/send` — agent needs input

When the turn ends `session.status_idle` with `stop_reason.type == "requires_action"`:

```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "result": {
    "kind": "task",
    "id": "task-…",
    "contextId": "ctx-002",
    "status": {
      "state": "input-required",
      "message": {
        "kind": "message",
        "role": "agent",
        "messageId": "…",
        "contextId": "ctx-002",
        "taskId": "task-…",
        "parts": [ { "kind": "text", "text": "Which Acme account did you mean?" } ]
      }
    }
  }
}
```

The caller resumes by re-issuing `message/send` with the same `contextId` (and the requested input as the new message); the bridge reuses the session and the agent continues.

## Error & failure semantics

| Situation | Result |
|---|---|
| Non-POST to a non-card path | pass-through (`Flow::Continue`) — the bridge only handles `GET` card paths + `POST` JSON-RPC |
| JSON-RPC method other than `message/send` | `-32601` method-not-found (lists supported methods) |
| Missing/invalid params, or no text part | `-32602` invalid-params |
| Claude auth failure (HTTP 401/403) | **`-32010`** "authentication … failed" |
| Claude unavailable / timeout (HTTP 408/429/5xx, or network error) | **`-32011`** "unavailable or timed out" (`data.upstreamStatus` when known) |
| Other Claude / internal error (parse, unexpected status, unresolved cluster) | `-32603` internal error |
| Poll budget exhausted before a terminal state | `TASK_STATE_WORKING` ("did not complete within the poll budget") |
| Tool deferred to human (HITL) | `TASK_STATE_INPUT_REQUIRED`; the next message is the decision — affirmative → allow, anything else → **deny (fail-closed)** |
| Card derivation fetch fails (`agentCardSource: derive`) | **fail-soft** — fall back to the pasted `agentCard`, else a minimal object; never blocks card serving |

Full error detail is always written to the gateway log; only a **classified, non-leaky** error envelope (no
upstream body, no secret) is returned to the caller. The `apiKey` is never logged (only its length, in local tooling).

## Deployment (A2A agent-asset path)

This policy targets an **A2A / agent asset** under management — not plain HTTP/REST APIs. `category: A2A` and `metadata/capabilities/assetTypes: a2a` scope it to the agent picker only. Canonical flow:

1. Publish (or reuse) an **A2A agent asset** in Exchange.
2. In API Manager, **Add an agent instance** from that asset.
3. Set the instance **implementation/upstream URI = `https://api.anthropic.com`** (the bridge reuses this cluster for its Claude egress — see *Outbound routing*) and expose the asset.
4. **Apply the Claude A2A Bridge policy** and configure it (`agentId`, `environmentId`, `apiKey` [sensitive], `agentCard`, `toolConfirmation`).
5. The bridged agent is then consumable within **Agent Fabric agent networks**.

> Future: source the agent card from the agent asset / managed-agent config rather than a pasted `agentCard` string (see open decisions).

## Open decisions and validation needed

1. **Multi-turn event scoping** — *resolved.* Each turn is anchored on the `user.message` event id returned by the `POST`, and status/answer/usage are read only from events after it (`current_turn()` in `services/dispatcher.rs`). Validated live (distinct answers + per-turn usage across follow-ups on one `contextId`) and by the `multi_turn_scopes_to_current_turn` unit test.
2. **`message/stream`** — relay Claude's streaming events endpoint as A2A SSE.
3. **`tasks/get` / `tasks/list` / `tasks/cancel`** — task state is already persisted; `cancel` maps to a `user.interrupt` event.
4. **Auth model** — config `apiKey` (v1) vs. credential-injection on the upstream cluster.
5. **Group id** — this repo ships with a placeholder all-zeros Exchange `groupId`; set it to the publishing org before release.
6. **Toolchain** — built against PDK `1.10.0` with crates.io-only dependencies (no private/workspace crates).

Validated end-to-end against the live Managed Agents API (Claude A2A POC agent): `message/send` completion + `Task.metadata` token usage; the event wire shapes; the full tool-confirmation gate — `defer → INPUT_REQUIRED → resume(approve) → COMPLETED`, `read` auto-allow, and `bash` auto-deny; and multi-turn follow-ups on a reused `contextId` (each turn returns its own answer and per-turn usage). 14 unit tests cover mapping, the gate resolver, the auto-allow/deny/defer paths, and turn scoping.
