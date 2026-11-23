# @agent-wechat/deepseek-harness-extension

WeChat channel plugin for DeepSeek Harness (Cordis) hosts — the DSH counterpart of
[`@agent-wechat/agent-wechat`](../openclaw-extension) (OpenClaw). Design and
milestones: [DESIGN.md](./DESIGN.md).

**Status: M0–M5 delivered** (bridge, routing, replies, tools, admission, media
annotations, voice calls). The gateway facade (M6) is not yet wired.

## What it does

- **Inbound bridge** — consumes the native daemon's committed event stream with
  `committed-live-only` semantics, a 24h/100-message catch-up window on
  reconnect, crash-safe cursor journaling, and exponential backoff. A missing
  or unreachable daemon never crashes the host: the channel parks and retries
  in the background.
- **Admission before everything** — DM policy (`disabled|allowlist|open`),
  group policy (`disabled|allowlist`), group sender gate, and real-mention
  gating are enforced before any media download or model dispatch. Denied
  messages cost zero media bytes.
- **Durable sessions** — one Agent session per chat route, persisted in the
  plugin storage domain; idle eviction; resumed sessions keep their history.
- **Reliable replies** — only committed assistant turns are collected (a
  session projection), delivered through a durable outbox with idempotency
  keys, canonical readback verification, and crash recovery. Never
  auto-resends: unconfirmed sends park as `needs_review` until the daemon
  confirms them by key.
- **Seventeen scoped tools** — see `interfaces/dsh/tools.txt` (generated):
  - *Current chat* (any admitted agent): `wechat_history`, `wechat_search`,
    `wechat_message`, `wechat_media`, `wechat_recall`, `wechat_send_share`.
  - *Owner DM only* (`tools.ownerAllowFrom`): `wechat_stickers`,
    `wechat_login`, `wechat_contacts`, `wechat_contact_request`,
    `wechat_group_info`.
  - *Opt-in archive* (default deny): `wechat_archive_threads`,
    `wechat_archive_search`, `wechat_archive_history`,
    `wechat_archive_message`, `wechat_archive_media` (metadata only — never
    downloads bytes).
  - Visibility is enforced **model-side** per step via the tool registry's
    activation hook: an agent that is not granted a tool never sees its
    declaration, and every execution re-validates the live route/grant.
- **Voice calls (M5, opt-in)** — a `calls.voice` config arms a seventeenth
  tool (`wechat_call`) plus an inbound call coordinator on the daemon's call
  socket. Calls fail closed: without a resolvable speech API key the
  coordinator stays offline and inbound offers time out unanswered.
  - *Inbound* — `inboundPolicy: 'allowlist'` answers only exact caller wxids;
    group invitations auto-join only when the group rule enables `autoJoin`
    and the inviter matches `autoJoinAllowFrom` (exact `agent-wechat:<id>`
    owner entries).
  - *Outbound* — `wechat_call` dial/invite actions additionally require the
    caller to be an owner DM listed in `outboundToolAllowFrom`, and every
    callee to be listed in `outboundAllowTo`.
  - *Speech* — `mode: 'agent'` runs a consultation turn per utterance
    (PCM16 24 kHz, VAD-gated, barge-in aware) through a harness Agent session
    keyed per contact (`agent:<id>:wechat-call:<contact>[:read-only]`); `mode:
    'bidi'` bridges OpenAI Realtime directly. Transcription and speech use the
    OpenAI REST/realtime APIs; the OpenClaw mulaw/8 kHz telephone downsample
    is intentionally not reproduced.
  - *Policies* — per-call tool scope is derived the same way as OpenClaw
    (`none` / `safe-read-only` / `owner`, only ever downgraded mid-call);
    owner scope requires an exact `agent-wechat:<wxid>` owner match.
  - *Memory* — call summaries (opt-in via `consultation.summary`) persist in a
    dedicated storage domain (`agent_wechat_calls`), capped at 3 per contact /
    500 contacts, and recaps are prepended to a contact's first consultation
    turn.
- **Media pipeline** — inbound media is described by trusted metadata in the
  followup text; bytes are fetched on demand by `wechat_media`, which
  re-validates the chat scope before touching attachments and reports
  pending/expired states verbatim (never fabricates content).
- **Message push (opt-in, default off)** — `push.enabled: true` with at least
  one `push.targets` entry arms an `wechat_push` tool that sends one-way
  notification messages to the configured WeChat ids / group ids (group ids
  end with `@chatroom`). Unlike the scoped tools above, the whitelist IS the
  authorization: the tool is visible to and callable from **every DSH
  session** (including non-WeChat agents such as CLI sessions), unknown
  targets are rejected, and delivery rides the same durable outbox
  (at-most-once POST, canonical readback, `needs_review` on ambiguity —
  never auto-resends).

## Known limitations

- **`readPolicy` is reserved.** The native daemon exposes no read-receipt API,
  so the OpenClaw read-pump semantics (`on_inbound` / `on_model_dispatch`)
  cannot be honored yet; the setting is accepted but has no effect today.
- **Voice transcription is unavailable** — inbound voice messages are
  annotated with an explicit `transcription unavailable` note; the call-time
  speech provider is separate and does not transcribe chat voice messages.
- **Calls are opt-in and agent-mode first** — `calls.voice.enabled: true` is
  required; unconfigured or key-less setups keep the coordinator offline
  (fail closed). Bidi realtime mode is wired but consultation agents are the
  exercised path. Video is not implemented.

## Install

From this package directory, install as a DSH bundle:

```
plugin_manager install_bundle --target <absolute path to packages/deepseek-harness-extension>
```

or in-session: ask the assistant to install the bundle at this directory.

## Configuration

The daemon token never lives in config: point `native.tokenCredentialRef`
(default `WECHAT_DAEMON_TOKEN`) at a credential-store entry. The full config
surface is listed in `interfaces/dsh/config.txt` (generated). Minimal owner-DM
example:

```
native: { endpoint: 'http://127.0.0.1:6790' }
dmPolicy: 'allowlist'
dmAllowFrom: ['wxid_owner']
tools: { ownerAllowFrom: ['wxid_owner'] }
```

Calls stay off until a `calls` block is present with
`calls.voice.enabled: true`. Minimal inbound-only example (store the OpenAI
key under the credential ref `OPENAI_API_KEY`):

```
calls:
  voice:
    enabled: true
    mode: 'agent'
    agentId: 'assistant'            # harness Agent id carrying calls
    inboundPolicy: 'allowlist'
    inboundAllowFrom: ['wxid_owner']
    toolPolicy: 'none'
    allowConsultSessionPersistence: true
    realtime: { apiKeyCredentialRef: 'OPENAI_API_KEY' }
```

Dialing out additionally needs `outboundPolicy: 'allowlist'` +
`outboundAllowTo`, and tool-triggered calls need `outboundToolAllowFrom`
listing the owner DMs allowed to invoke `wechat_call`.

Message push stays off until armed. Minimal example that lets any DSH session
notify two targets:

```
push:
  enabled: true
  targets: ['wxid_owner', '123456@chatroom']
```

Everything else stays closed by default. Groups additionally require
`groupPolicy: 'allowlist'` + `groupAllowFrom` (+ `groupSenderAllowFrom` to
admit specific senders; `requireMentionInGroups` defaults to true).

## Settings UI

The bundle ships a client half (`client/client.js`) that renders a visual
settings page for the 19 most relevant config fields — hub address
(`native.endpoint`), run mode, DM/group admission, owner allowlist, tool
grants, the calls block, and message push — in two host surfaces:

- **Settings → 微信通道** — a dedicated page in the Settings panel nav
  (`settings.section`, id `wechat-channel`).
- **Plugins → this bundle's page** — the same form under the bundle entry
  (`plugins.bundle.config`, keyed by the package name).

Edits go through the host's settings forms service, are validated against the
plugin's Config schema, and land in the profile patch — the UI and a manual
patch edit stay equivalent. Fields the user has overridden show a reset
button; CSV lists are comma-separated; secrets are never displayed (only
credential *names* such as `native.tokenCredentialRef` /
`calls.voice.realtime.apiKeyCredentialRef`). The client half loads on page
load; after installing or upgrading the bundle, refresh the browser once.

wxid / group-id fields (`dmAllowFrom`, `groupAllowFrom`,
`groupSenderAllowFrom`, `tools.ownerAllowFrom`,
`calls.voice.inboundAllowFrom`, `push.targets`) carry a **Pick from
directory** search: it queries the connected hub through a host-side,
read-only route (`GET /plugins/wechat-channel/search`, contact + group names
only — never message content) and clicking a result toggles it in the staged
list. The route answers `503` while the channel is parked, so the picker
shows an inline "not connected" note instead of results.

Both surfaces also show a **live connection status** strip: the plugin host
half registers a read-only route, `GET /plugins/wechat-channel/status`
(registered through `ctx.inject(['webServer'])` so it exists during
park/retry), serving `{running, serverId, attempts, error, endpoint,
accountId}` — no secrets. The client polls it every 5 s and renders a colored
dot: green 已连接 (running with a `serverId`), amber 连接中 (attempt in
flight), red 未连接 (parked, with the error and retry count), gray for
stopped/unknown. The Plugins-page summary line ends with the same status word.

## Build

```
pnpm install
pnpm --filter @agent-wechat/deepseek-harness-extension build   # esbuild → dist/index.js
pnpm --filter @agent-wechat/deepseek-harness-extension typecheck
node --test --experimental-strip-types src/*.test.ts src/tools/*.test.ts   # needs Node ≥ 24
```

Interface listings (`interfaces/dsh/*.txt`) are regenerated by `pnpm
interfaces` via `scripts/interface-listing.mjs` and must be committed with any
tool/config change.

## Safety defaults

Everything is off until configured: DMs and groups require explicit policy
(`dmPolicy` / `groupPolicy` + allowlists), calls require an explicit
`calls.voice` block and a resolvable speech API key, archive tools are
deny-by-default, and the daemon
token lives in the credential store (`native.tokenCredentialRef`), never in
config. `needs_review` outbound records are never resent automatically;
recalls, contact removals and credential resets always require explicit tool
keys or confirmation flags.
