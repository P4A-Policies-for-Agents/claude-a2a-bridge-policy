# Making Claude Managed Agents cleanly ingestible (Agent → A2A card → Exchange/Bridge)

Goal: when a Claude managed agent is fronted by the **Claude A2A Bridge** policy and published as an A2A
asset for Agent Fabric networks, its **agent card** should be rich and accurate with as little manual effort
as possible. This doc covers the levers on **all three sides** — the Claude agent, the bridge policy, and
Exchange/API Manager.

## 1. What the bridge can extract from an agent

> Validated live 2026-06-19: `agentCardSource: derive` built a correct card from the POC agent (name +
> description + version + url-from-host; skills fell back to `general-assistance` since that agent has no MCP
> servers). Improving the agent `description` flowed straight into the card.

`GET /v1/agents/{agentId}` (the same key/agentId already in policy config) returns:

| Agent field | A2A card target | Reliably populated today? |
|---|---|---|
| `name` | `card.name` | ✅ yes |
| `description` | `card.description` | ⚠️ **often empty** (3 of 4 org agents) |
| `skills[]` | ⚠️ **not** a card-skills source (see 2.3) | ❌ always empty — it is *functional* Agent-Skill references, not metadata |
| `mcp_servers[]` (e.g. finance, hris) | → synthesize skills / tags | ✅ rich on real agents |
| `tools[]` (toolset, permission_policy) | → capabilities / gate alignment | ✅ yes |
| `metadata` (domain, project, …) | `card` tags / provider | ✅ rich on real agents |
| `model` (id, speed) | informational / tags | ✅ yes |
| `version` | `card.version` | ✅ yes |
| `system` (system prompt) | **DO NOT expose** | ✅ (internal only) |

**The agent-side fidelity lever is `description`** (the agent `skills[]` field is *functional*, not descriptive — see 2.3); card skills come from the bridge. Everything else is present. `card.skills[]` is the
**discoverability surface in agent networks** (how other agents/orchestrators decide what this agent can do),
so an empty skills array = a poorly discoverable agent.

## 2. Agent-author best practices (the highest-leverage side)

Configure the managed agent so the card is great *by construction*:

1. **`name`** — clear, human-facing (e.g. "Vasion Finance Ops"), not an internal codename.
2. **`description`** — a 1–3 sentence **public capability summary**. This is the card's `description`.
   - Do **not** rely on the `system` prompt for this — it's internal, may contain guardrails/secrets, and the
     bridge will **not** expose it. Write a separate, outward-facing description.
3. **`skills[]` is NOT a descriptive channel** (validated against the API, 2026-06-19). The managed-agent
   `skills` field is the **functional Agent Skills** feature — a list of *typed references* (`type: custom |
   anthropic`, each requiring a `skill_id`) to skill *packages* the agent loads. It rejects inline
   `name`/`description`, and attaching one **changes agent behavior**. Do **not** use it to make the card
   descriptive; card skills come from the bridge instead (see 2.5 / section 3).
4. **`metadata`** — set `domain`, `project`, and any tag-like keys consistently. The bridge maps these to card
   tags / provider, and they aid Exchange search and network grouping.
5. **`mcp_servers` — the primary card-skills lever you control on the agent.** The bridge synthesizes one
   card skill per MCP server (e.g. `finance`, `hris` → two skills). An agent with MCP servers gets a meaningful
   skills array for free; one with none falls back to a single `general-assistance` skill. For richer skills,
   hand-author them via the `agentCard` override with `agentCardSource: derive` (section 3) — that, not the
   agent `skills` field, is how you get descriptive skills today.
6. **`version`** — bump it meaningfully; it flows to `card.version`.

> Rule of thumb: a high-fidelity card needs **name + description + ≥1 skill**. Agents that set those three get
> an excellent card with zero policy-side guesswork.

## 3. Bridge-policy best practices (extraction + safe fallbacks)

Proposed policy config knob: **`agentCardSource: paste | derive`** (default `paste` for back-compat).

- **`paste`** — current behavior: serve the configured `agentCard` JSON verbatim.
- **`derive`** — on load (and/or first card request) the policy calls `GET /v1/agents/{agentId}` and builds the
  card. `agentCard` becomes an optional **override** (merge: pasted fields win over derived).

Derivation mapping + fallbacks:

| Card field | Source → fallback chain |
|---|---|
| `name` | `agent.name` → `agentId` |
| `description` | `agent.description` → `"{name} — Claude managed agent"` (never the system prompt) |
| `skills[]` | `agent.skills[]` → synthesize one per `mcp_servers[]` → single `general-assistance` skill |
| `capabilities.streaming` | `false` (until the bridge implements `message/stream`) |
| `defaultInput/OutputModes` | `["text/plain"]` |
| `version` | `agent.version` → `"1.0.0"` |
| tags / provider | `agent.metadata` (domain/project) |
| `protocolVersion` | `"0.3.0"` (canonical A2A) |
| `url` / `preferredTransport` | the APIM instance endpoint (deploy-time) / `"JSONRPC"` |

Guardrails:
- **Never** put `agent.system` (the system prompt) into the card.
- Derivation must **fail soft**: if the agent fetch fails, fall back to the pasted `agentCard` (or a minimal
  valid card) and log a warning — never block card serving.
- Cache the derived card in policy state; don't fetch the agent on every `/.well-known` hit.

## 4. Exchange / API Manager best practices

The agreed deployment path is **agent-asset-first**:
1. Publish/reuse an **A2A agent asset** in Exchange — its card should mirror the managed agent (same name,
   description, skills). If you author the agent well (§2), this stays consistent automatically.
2. **Add an agent instance** from that asset in API Manager.
3. Set the instance **upstream/implementation URI = `https://api.anthropic.com`** (the bridge reuses this
   cluster for its Claude egress) and expose the asset.
4. **Apply the Claude A2A Bridge policy** (`agentId`, `environmentId`, `apiKey` [sensitive], `toolConfirmation`,
   and either a pasted `agentCard` or `agentCardSource: derive`).
5. Use the bridged agent in **Agent Fabric agent networks**.

Naming consistency across **agent ↔ Exchange asset ↔ card** makes the agent discoverable and avoids confusion
in networks. Keep `agentId`/`environmentId` and the asset version documented together.

## 5. Future: card straight from the Exchange asset

Pulling the card from the *linked Exchange asset file* (so the policy needs no `agentId`/key to render the
card) is the cleanest long-term model, but it's **blocked** on an open APIM decision about how linked-asset
metadata is handed off to policies. A PDK policy *can* read linked-asset metadata via the `Metadata` injectable
once APIM finalizes the hand-off — at which point `agentCardSource` gains a third option, `asset`, without
breaking `paste`/`derive`.
