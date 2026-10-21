# OpenClaw WeChat channel

The plugin uses the standalone native daemon. External mode connects by endpoint/token to an independently operated daemon, including a native server container; the Gateway needs neither `wx` nor Docker tools. Managed mode starts an owned local daemon and requires a separately installed `wx`. Plugin installation/startup never downloads or repairs a binary. Old container API configurations require explicit migration.

Once published, install the native beta with `openclaw plugins install @agent-wechat/agent-wechat@beta` or pin its exact version. The stable `latest` channel continues to select the old container plugin during the beta series.

## Native external mode

Configure plugin defaults under `plugins.entries.agent-wechat.config` (or the same `native` configuration in the channel):

```json
{
  "native": {
    "mode": "external",
    "accountId": "example",
    "endpoint": "http://127.0.0.1:6174",
    "token": { "source": "env", "provider": "default", "id": "AGENT_WECHAT_TOKEN" },
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "on_inbound"
  }
}
```

The external service remains independently owned. Plugin stop closes its stream and never stops/logs out the daemon or cancels its sends. Remote endpoints require verified HTTPS or an encrypted tunnel terminating at loopback. Set `AGENT_WECHAT_TOKEN` in the Gateway process environment, or use a configured OpenClaw secret provider. The plugin resolves `native.token` through the pinned OpenClaw SDK; `native.tokenFile` is removed. Tokens contain 32–1024 printable ASCII bytes without spaces. An explicit unresolved reference fails startup without falling back to another token. The API token needs read/media/send/login for the selected account, plus read-state only when automatic whole-thread read marking is enabled; do not grant admin solely to run the plugin. Normal sender/DM/group/mention policy remains in `channels.agent-wechat`; defaults remain closed.

## Native managed mode

```json
{
  "native": {
    "mode": "managed",
    "accountId": "example",
    "executable": "/opt/agent-wechat/bin/wx",
    "listen": "127.0.0.1:6174",
    "dataDir": "/private/native-store",
    "token": { "source": "env", "provider": "default", "id": "AGENT_WECHAT_TOKEN" },
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "never"
  }
}
```

Provision the native executable and account registration separately. Omitting `executable` resolves `wx` once from absolute trusted PATH entries. The plugin supports the package's tested npm launcher layout by selecting its already installed checksum-verified ELF payload directly. It never invokes an installer or shell. Unknown wrappers/old container CLIs are rejected. Managed startup refuses an occupied port and requires authenticated readiness matching the owned child PID, store path, server identity and process generation; it never adopts or terminates another process. Stop sends a graceful termination to its own child with bounded escalation. An unexpected child/stream failure is reported; automatic process crash restart is not implemented in this increment.

Managed startup sends the resolved token through a private stdin pipe; it creates no token file and does not put the token in arguments or the daemon environment. A managed `serverConfig` must omit `tokenFile`. Reload/restart the Gateway to change credentials or secret-provider configuration.

Keep `dataDir` and `consumerStateDir` on separate explicit persistent mounts. The daemon owns official-format profile/sidecar data; the consumer directory holds private intake/deduplication/model-handoff/read receipts bound to server and account identities. Executable upgrades do not remove either. One configured OpenClaw account maps to one selected native daemon account in this increment, while the daemon itself supports independent account slots.

## Native messaging lifecycle and validation limits

The adapter consumes account-scoped SSE using bearer headers and decimal-string journal cursors. It persists custody before acknowledging a cursor; acknowledgement is independent of unread state. Only explicitly live, incoming, committed resources can enter the model. Supported images, short voice, video, files and eligible stickers pass through bounded media retrieval and hash verification into OpenClaw-managed storage. Pending media retains custody without model dispatch or a read receipt. Expired/unsupported attachments retain an explicit status; raw XML never becomes an attachment. Catch-up/import/raw pending sync batches, unknown origin and outgoing messages do not become automatic model inputs. Group admission uses structured exact self-mention facts; unknown metadata or displayed `@names` never satisfy a mention gate. Contacts and groups use their separate catalogs.

Every ready attachment supplies its original OpenClaw-managed path in the message body and `Attachments` metadata, including images. File references wrap paths in angle brackets, such as `[Image file: <path>]` or `[Local file: <path>]`, so OpenClaw does not automatically load an original or a file with an image extension when scanning prompt text. A separately prepared image preview may occupy `MediaPath` while the original file remains accessible by its own path. Files, voice and video retain explicit local references; an available path does not claim that the model analyzed the file.

In groups requiring a mention, sender-admitted unmentioned messages are retained as context without retrieval, transcription, a model turn or a read notification. The next admitted non-command message includes that context after checking the current sender policy again. `messages.groupChat.historyLimit` controls the per-group window (default 50, maximum 200; zero disables it). The consumer store retains at most 1,000 buffered group messages overall. Context survives a Gateway restart. At dispatch, the selected context is consumed atomically with the durable turn record, so an uncertain turn is not offered again automatically. Original mention facts remain unchanged; history is recorded separately from the triggering message.

Group catch-up retrieves media with at most three workers, keeps every verified original path and selects only the latest ready image's prepared representation for vision. Pending or unavailable historical attachments remain explicit statuses and do not block the current mention. The triggering attachment, when still pending, continues to hold its own dispatch. Voice transcription runs only after admission and activation. Commands do not drain buffered context.

By default, `readPolicy: "on_inbound"` marks a whole thread read after its incoming message has been durably stored, passed sender/group/mention checks and completed normal OpenClaw inbound handling. Built-in commands also qualify when no model is called. This policy does not inspect model inputs or require `hooks.allowConversationAccess`. Ignored messages, pending media and failed handling do not initiate a read. Read means handled by the integration; it does not claim that a model analyzed an attachment.

A whole-thread read also clears earlier unread messages in that thread, including messages the model did not receive. The daemon freezes the latest local message boundary, checks the expected version, updates local unread state and submits one native read-status notification. Server acceptance alone does not prove that a phone badge changed. A lost notification response remains `needs_review` and is never automatically resubmitted. Pending reads survive a Gateway restart and retry independently of model execution or new SSE traffic. `readPolicy: "never"` disables both local and remote marking, including previously queued reads. Crashes during an unfinished dispatch retain `needs_review`; the plugin does not automatically duplicate model turns or uncertain outgoing sends.

The optional `readPolicy: "on_model_dispatch"` instead requires a confirmed model handoff. OpenClaw **2026.10.2** lacks `onPreparedAgentRunStart`, so its released public hooks provide a bounded compatibility path. Before dispatch, the adapter persists an immutable manifest binding the exact new message ID/version/content hash, policy hash, source and supplied asset hashes, and context roles. Text requires the exact current input and host-created message/account/session facts plus a matching `model_call_started`. Media additionally waits for the same run's `agent_end` snapshot and source-turn identity. Image bytes/MIME/length must match the frozen supplied representation; file/video/voice references must appear in the exact current body. Image count, model selection and reply delivery alone cannot confirm a model handoff. Unknown, omitted, rewritten, merged or ambiguous inputs remain unconfirmed. Host/provider history formats can prevent confirmation even when a reply was delivered; use `on_inbound` for ordinary bot read behavior.

The model-handoff compatibility path requires the operator's explicit host permission, `plugins.entries.agent-wechat.hooks.allowConversationAccess: true`. It observes only run IDs issued by this adapter and never enables that permission itself. Without the grant, or on an unsupported host, the explicit `on_model_dispatch` policy leaves messages unread and status reports `handoffUnconfirmed`. The older `onAgentRunStart` also reports direct tools and early lifecycle phases and is never used as proof. Access and mention gates still precede model dispatch; a rejected message does not itself trigger a read.

Images exceeding the host's bounded representation, or formats requiring conversion, receive a separately stored JPEG still preview using the public SDK image processor. The original stays intact; both hashes are recorded and the model input labels the preview. Prepared images fit within 5 MiB and at most 1200 pixels per side (or a smaller configured limit). Files and videos can be supplied as available local references; this does not claim their contents were analyzed. Voice retains its verified file and includes configured transcription output or an explicit unavailable status. A placeholder does not confirm a model handoff under `on_model_dispatch`; default `on_inbound` can mark a thread after successfully handling an explicit expired/unsupported status.

## Stored-history tools

The agent can look beyond the recent inbound buffer using the selected native account's local profile:

| Tool | Inputs (`?` means optional) | Output |
| --- | --- | --- |
| `wechat_history` | `{ limit?, cursor? }` | Stored messages, newest first, with `nextCursor`, `sourceGeneration` and `coverage` |
| `wechat_search` | `{ query, limit?, cursor? }` | Same page shape, filtered by case-insensitive substring |
| `wechat_message` | `{ messageId }` | One stored message |
| `wechat_media` | `{ messageId, download?, quality? }` | Media status, optional verified file path and image content |

A stored message includes `id`, `threadId`, nullable `serverId`, `senderId`, `direction` and `createdAt`, numeric `type`, bounded `body`, and `bodyTruncated`. Its optional `attachment` descriptor contains `kind`, `messageId` and `retrievalTool: "wechat_media"`. Message IDs and server IDs remain strings; use the opaque `id` for exact lookup and media retrieval.

A media result includes `messageId`, `threadId`, `status` and `retryable`, with optional `attachment`, actual `variant`, `jobId`, `reason` and display `body`. A ready attachment has `kind`, `filename`, verified local `path` and `mime`; image quality may be `full`, `standard` or `thumbnail`. Unavailable results never claim a local path. OpenClaw wraps structured outputs in `details` and text `content`; a ready image additionally supplies an image content block.

For example, the agent can call `wechat_search` with `{"query":"project notes"}` in the current conversation. Every tool is restricted to the current WeChat account and thread derived from trusted OpenClaw context. Owners have the same restriction in direct and group conversations. Tools accept no thread/account selector, and the plugin rejects messages or assets from another conversation or account. Requester and historical sender policy are rechecked. Missing or conflicting trusted conversation context hides the tools; webchat and other channels cannot use them. Broader operator lookup remains available through the CLI and authenticated daemon API.

History/search return opaque string message IDs, a bounded text preview, coverage metadata and `nextCursor`. Pass the cursor back with the same tool, query and thread to continue; the page limit is 1–50 (default 20). Search queries are limited to 256 UTF-8 bytes. Long message bodies are labeled as truncated. A sender-filtered page may be empty and still have a continuation. Search covers locally stored history and does not fetch missing remote history or maintain an official FTS index. Media messages receive a type/status description; these tools do not return raw transport XML or download attachments. History, search and exact-message lookup are read-only. Their attachment descriptions name `wechat_media` and the exact message ID without automatically retrieving media. None of these tools mark a conversation read, send messages or modify the inbound consumer cursor. Lookup requires the selected account's API `read` scope and remains subject to normal OpenClaw tool policy. With OpenClaw's compact tool catalog, the agent discovers history/search/message lookup through `tool_search` and invokes the returned tool with `tool_call`. The media tool stays directly callable as `wechat_media`: the pinned host's generic wrapper serializes nested image blocks as JSON text, while direct invocation preserves image content for vision.

`wechat_media` accepts `{"messageId":"<returned opaque ID>"}`. It verifies message/thread/sender admission before accessing assets, uses cached bytes first, checks identity, hash and size, and saves verified bytes to OpenClaw's managed media store. Files, videos and voice return local paths; images also return a bounded prepared preview as image content for vision. This retrieval does not itself transcribe voice or analyze files/videos. Cached bytes remain usable after CDN expiry.

Missing media requests a durable daemon transfer by default, with a two-second bounded wait. A pending result includes its `jobId` when known and no claimed local path; call the same tool with the same message ID later to check. The daemon owns and deduplicates the transfer, so cancelling a tool invocation does not cancel submitted work. Use `download: false` for cached media only. `quality` defaults to `best` (best cached image, normal video or decoded voice); explicit `original`, `standard`, `thumbnail`, `normal` and `encoded` variants are strict and may be unavailable. Results report the actual variant and explicit pending, expired, unsupported, unavailable or too-large status. Never automatically retry `retryable: false` or confirmed expiry. The media tool requires account-scoped API `read` and `media` permission, including for download requests. Raw XML, signed transport URLs and keys are never returned.

`node scripts/native-history-model-smoke.mjs` exercises the pinned Gateway with local account/media/model fixtures: durable group context, original file paths, one preview with a verified hash, a PNG sent as a file remaining a reference, direct image delivery, actual tool discovery, two paginated search calls, rejection of another thread's attachment before media access, one requested image download whose exact image bytes reach vision, and a cached file path reaching the provider. CI runs this check on both Linux architectures.

Run `node scripts/native-model-smoke.mjs` and `node scripts/native-media-model-smoke.mjs` after installing workspace dependencies. They use the actual pinned Gateway, a local model and synthetic daemon/files. Checks cover default inbound reads without conversation permission, commands without a model call, current provider input, image hashes and prepared previews, reference/transcription delivery, whole-thread read requests, lost-response/reopen recovery, duplicate prevention, omitted images on a text-only model, explicit model-handoff permission, never-read policy and group routing behavior. Transcription uses the real SDK with an explicitly configured local deterministic CLI; it does not test speech recognition accuracy. Concurrent text inputs were serialized into distinct model turns; merged-input refusal has unit coverage for the explicit model policy. These checks establish local runtime behavior, not external provider acceptance, semantic analysis or consumption.

Native text/media sends retain daemon intent/job identity and caller idempotency. Text waits for accepted submission; media jobs preserve partial and uncertain outcomes rather than resending. Ordinary audio remains a file; only explicit `audioAsVoice` requests independently encoded voice. Sender/group/mention gates run before retrieval/transcription. Verified voice may use the configured OpenClaw transcription runtime after admission. Native QR login uses a durable daemon operation and authenticated PNG retrieval. Logout commits local inhibition before at most one authenticated native logout, reporting the remote outcome separately; saved-device revocation remains unverified. Explicitly confirmed forgetting removes local auth while retaining history; disconnect retains reusable trust.

## Optional Gateway facade

Set `native.gatewayFacade: {"enabled": true}` to register `/plugins/agent-wechat/native` on the Gateway. The default is disabled. A custom prefix must remain namespaced under `/plugins/...`; changing daemon configuration requires a Gateway reload.

The actual Gateway authenticates requests before the proxy runs. Its bearer is never forwarded to the daemon; the plugin uses its private daemon token reference. Routes are restricted to the configured account, with no daemon-wide token/account administration. Binary bodies, byte ranges, status codes and SSE are preserved within explicit size limits. Client disconnect/plugin shutdown closes upstream streaming work without cancelling durable sends.

`scripts/native-gateway-smoke.mjs` tests the built plugin on an isolated OpenClaw **2026.10.2** Gateway with synthetic loopback data: auth rejection, separate credentials, account boundaries, Range206, SSE and cancellation. This transport test is separate from the local model-boundary test above; neither uses an external model provider or a real WeChat account.

See [native deployment examples](../../deploy/native/README.md) and [native CLI installation](../cli/README.md).
