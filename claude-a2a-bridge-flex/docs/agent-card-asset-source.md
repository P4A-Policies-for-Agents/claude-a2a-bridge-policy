# Thought experiment: `agentCardSource: asset` — serving the card from the linked Exchange asset

**Status: parked / research.** Not implemented. Captured 2026-06-19 so the analysis isn't lost.
Likely superseded later by a product-supported linked-asset metadata injection mechanism; when that
lands, prefer it over the connected-app callback below.

## Why this is interesting

A2A consumers in a brokered agent network expect **canonical A2A v0.3.0** cards. The Exchange `agent` asset we publish is *also* canonical (and is the catalog's
source of truth). An `asset` card source would let the live endpoint serve **exactly the published
card, verbatim, auto-syncing on re-publish** — a governance / single-source-of-truth win that neither
`paste` nor `derive` gives.

The catch: API Manager links the API instance to the Exchange asset only at the **control plane**.
The instance record carries the full coordinate (`groupId` + `assetId` + `assetVersion` +
`metadata.{assetType,assetPlatform}`), but the PDK `Metadata` injectable handed to the policy at
request time does **not** include it — it exposes `apiMetadata.{id,name,version}`,
`platformMetadata.{organization_id,environment_id,root_organization_id}`, flex + policy identity, and
SLAs. No `groupId`/`assetId`/`assetVersion`. So the running policy cannot natively reach the asset.

## The proposed mechanism (connected-app callback + cache)

Config (both `security:sensitive`, prefer Flex secret references):
`anypointClientId`, `anypointClientSecret` — a connected app scoped **minimally** (Exchange Viewer;
+ API Manager Viewer only if step 2 is kept).

On a card request, cache-miss path:
1. **Token** — `POST accounts/api/v2/oauth2/token`, `grant_type=client_credentials` → bearer
   (cache the bearer too; it is long-lived relative to a card fetch).
2. **Resolve coords** — `GET apimanager/api/v1/organizations/{org}/environments/{env}/apis/{apiId}`
   using `apiMetadata.id` + `platformMetadata.{organization_id,environment_id}` → `groupId`/`assetId`/
   `assetVersion`. *Optional:* skip this entirely by configuring the coords directly on the policy
   (drops the call and the APIM scope — but at that point you're close to just pasting the card).
3. **Fetch card** — `GET exchange/api/v2/assets/{groupId}/{assetId}/{version}` → `files[]` → the
   `a2a-card` `externalLink` (presigned S3) → `GET` that → the canonical card JSON.
4. **Cache + serve** — store the assembled card under `DataStorage` with a TTL (in-memory for
   single-replica, shared/Redis for multi-replica). ~4 hops cold, ~0 warm.

## Effort assessment

| Piece | Difficulty | Notes |
|---|---|---|
| Sensitive creds in config | trivial | same pattern as `apiKey` |
| Caching | easy | `DataStorage` (see `pdk-data-storage`/`pdk-caching`) — card is tiny |
| Token + control-plane + Exchange calls | moderate | standard HTTP; auth handling |
| **Egress to new hosts** | **the real gate** | see below |

**The genuinely hard part — egress.** Today the bridge reaches Claude by *reusing the inbound API's
upstream cluster* (`api.anthropic.com`); it has no general outbound capability (this is why the
hosting instance's upstream URI must be Anthropic). Steps 1–3 hit **different hosts**
(`anypoint.mulesoft.com`, `*.s3.amazonaws.com`). That requires additional egress — `format: service`
config entries (Omni Gateway auto-creates the clusters) **and** the gateway's network actually being
permitted to reach those hosts (private-space firewall/egress). De-risk this with a spike *first*;
everything else is routine.

## Security / cost reality

- A connected-app secret with org-wide Exchange (+APIM) read sits in **every** agent instance's policy
  config — far larger blast radius than the per-agent Anthropic key. Scope minimally; use secret refs.
- Redundant on **content** with `paste` and `derive→canonical` (both produce a canonical card with zero
  extra calls and no new creds/egress). Its unique value is purely governance: served == published,
  auto-syncing.

## Recommendation

Get on-dialect now via **`derive → canonical`** (rebuild `card_builder` output as canonical v0.3.0) and
**`paste` accepting canonical**. Keep `asset` as a tracked v-next, gated on either (a) a product-supported
linked-asset injection appearing, or (b) a green egress spike — whichever comes first. If a supported
method ships, it almost certainly beats the connected-app callback on both security and ops.

## Comparison

| Source | Calls/req | New creds | New egress | Canonical? | Unique value |
|---|---|---|---|---|---|
| `paste` | 0 | none | none | yes (you supply) | simplest |
| `derive→canonical` | 0 extra | none | none | yes (built) | no per-instance card authoring |
| `asset` (this) | 0 warm / ~4 cold | connected app | anypoint + S3 | yes (verbatim) | served == published, auto-sync |
