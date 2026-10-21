# OpenClaw WeChat channel

The plugin uses the standalone native daemon. Install/provision `wx` separately; plugin installation/startup never downloads or repairs a binary. Choose managed hosting for an owned local daemon or external hosting for an independently operated daemon. Old container API configurations require explicit migration.

## Native external mode

Configure plugin defaults under `plugins.entries.agent-wechat.config` (or the same `native` configuration in the channel):

```json
{
  "native": {
    "mode": "external",
    "accountId": "example",
    "endpoint": "http://127.0.0.1:6174",
    "tokenFile": "/private/native-api-token",
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "on_model_dispatch"
  }
}
```

The external service remains independently owned. Plugin stop closes its stream and never stops/logs out the daemon or cancels its sends. Remote endpoints require verified HTTPS or an encrypted tunnel terminating at loopback. Token files must be private regular files (0600), contain at least 32 bytes, and are references rather than inline credentials. The API token needs read/media/send/login for the selected account, plus read-state only when automatic local read projection is enabled; do not grant admin solely to run the plugin. Normal sender/DM/group/mention policy remains in `channels.agent-wechat`; defaults remain closed.

## Native managed mode

```json
{
  "native": {
    "mode": "managed",
    "accountId": "example",
    "executable": "/opt/agent-wechat/bin/wx",
    "listen": "127.0.0.1:6174",
    "dataDir": "/private/native-store",
    "tokenFile": "/private/native-api-token",
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "never"
  }
}
```

Provision the native executable and account registration separately. Omitting `executable` resolves `wx` once from absolute trusted PATH entries. The plugin supports the package's tested npm launcher layout by selecting its already installed checksum-verified ELF payload directly. It never invokes an installer or shell. Unknown wrappers/old container CLIs are rejected. Managed startup refuses an occupied port and requires authenticated readiness matching the owned child PID, store path, server identity and process generation; it never adopts or terminates another process. Stop sends a graceful termination to its own child with bounded escalation. An unexpected child/stream failure is reported; automatic process crash restart is not implemented in this increment.

Keep `dataDir` and `consumerStateDir` on separate explicit persistent mounts. The daemon owns official-format profile/sidecar data; the consumer directory holds private intake/deduplication/model-handoff/read receipts bound to server and account identities. Executable upgrades do not remove either. One configured OpenClaw account maps to one selected native daemon account in this increment, while the daemon itself supports independent account slots.

## Native messaging lifecycle and validation limits

The adapter consumes account-scoped SSE using bearer headers and decimal-string journal cursors. It persists custody before acknowledging a cursor; acknowledgement is independent of unread state. Only explicitly live, incoming, committed resources can enter the model. Supported images, short voice, video, files and eligible stickers pass through bounded media retrieval and hash verification into OpenClaw-managed storage. Pending media retains custody without model dispatch or a read receipt. Expired/unsupported attachments retain an explicit status; raw XML never becomes an attachment. Catch-up/import/raw pending sync batches, unknown origin and outgoing messages do not become automatic model inputs. Group admission uses structured exact self-mention facts; unknown metadata or displayed `@names` never satisfy a mention gate. Contacts and groups use their separate catalogs.

The prepared-run callback synchronously writes a bounded durable receipt for the exact selected new message IDs before a separate retryable read projection. Read CAS retries preserve the same sparse IDs and never widen to the largest ID or clear concurrent arrivals. `readPolicy: "never"` disables automatic reads. Crashes during an unconfirmed dispatch retain `needs_review`; the plugin does not automatically duplicate model turns or uncertain outgoing sends.

OpenClaw **2026.10.2** lacks `onPreparedAgentRunStart`, so its released public hooks provide a bounded compatibility path. Before dispatch, the adapter persists an immutable manifest binding the exact new message ID/version/content hash, policy hash, source and supplied asset hashes, and context roles. Text requires the exact current input and host-created message/account/session facts plus a matching `model_call_started`. Media additionally waits for the same run's `agent_end` snapshot and source-turn identity. Image bytes/MIME/length must match the frozen supplied representation; file/video/voice references must appear in the exact current body. Image count, model selection and reply delivery alone cannot mark a message read. Unknown, omitted, rewritten, merged or ambiguous inputs remain unconfirmed.

The compatibility path requires the operator's explicit host permission, `plugins.entries.agent-wechat.hooks.allowConversationAccess: true`. It observes only run IDs issued by this adapter and never enables that permission itself. Without the grant, or on an unsupported host, messages remain unread and status reports `handoffUnconfirmed`. The older `onAgentRunStart` also reports direct tools and early lifecycle phases and is never used as proof. `readPolicy: "never"` preserves the handoff receipt while disabling read projection. Late confirmed receipts retry their exact sparse reads independently of new SSE traffic.

Images exceeding the host's bounded representation, or formats requiring conversion, receive a separately stored JPEG still preview using the public SDK image processor. The original stays intact; both hashes are recorded and the model input labels the preview. Prepared images fit within 5 MiB and at most 1200 pixels per side (or a smaller configured limit). Files and videos can be supplied as available local references; this does not claim their contents were analyzed. Voice retains its verified file and includes configured transcription output or an explicit unavailable status. Pending, expired, unsupported and failed-integrity media never gains a read receipt from a placeholder.

Run `node scripts/native-model-smoke.mjs` and `node scripts/native-media-model-smoke.mjs` after installing workspace dependencies. They use the actual pinned Gateway, a local model and synthetic daemon/files. Checks cover current provider input, image hashes and prepared previews, reference/transcription delivery, exact sparse reads, lost-read-response/reopen recovery, duplicate prevention, omitted images on a text-only model, permission, never-read policy and group routing behavior. Transcription uses the real SDK with an explicitly configured local deterministic CLI; it does not test speech recognition accuracy. Concurrent text inputs were serialized into distinct model turns; merged-input refusal has unit coverage. These checks establish the prepared runtime boundary, not external provider acceptance, semantic analysis or consumption.

Native text/media sends retain daemon intent/job identity and caller idempotency. Text waits for accepted submission; media jobs preserve partial and uncertain outcomes rather than resending. Ordinary audio remains a file; only explicit `audioAsVoice` requests independently encoded voice. Sender/group/mention gates run before retrieval/transcription. Verified voice may use the configured OpenClaw transcription runtime after admission. Native QR login uses a durable daemon operation and authenticated PNG retrieval. Logout commits local inhibition before at most one authenticated native logout, reporting the remote outcome separately; saved-device revocation remains unverified. Explicitly confirmed forgetting removes local auth while retaining history; disconnect retains reusable trust.

## Optional Gateway facade

Set `native.gatewayFacade: {"enabled": true}` to register `/plugins/agent-wechat/native` on the Gateway. The default is disabled. A custom prefix must remain namespaced under `/plugins/...`; changing daemon configuration requires a Gateway reload.

The actual Gateway authenticates requests before the proxy runs. Its bearer is never forwarded to the daemon; the plugin uses its private daemon token reference. Routes are restricted to the configured account, with no daemon-wide token/account administration. Binary bodies, byte ranges, status codes and SSE are preserved within explicit size limits. Client disconnect/plugin shutdown closes upstream streaming work without cancelling durable sends.

`scripts/native-gateway-smoke.mjs` tests the built plugin on an isolated OpenClaw **2026.10.2** Gateway with synthetic loopback data: auth rejection, separate credentials, account boundaries, Range206, SSE and cancellation. This transport test is separate from the local model-boundary test above; neither uses an external model provider or a real WeChat account.

See [native deployment examples](../../deploy/native/README.md) and [native CLI installation](../cli/README.md).
