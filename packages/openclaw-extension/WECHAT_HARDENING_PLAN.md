# WeChat Extension Hardening Plan

## Goal

Bring the WeChat plugin to security/behavior parity with OpenClaw WhatsApp/Telegram patterns, specifically:

1. Slash/control commands must be authorization-gated correctly.
2. DM/group policy enforcement must be explicit, consistent, and fail-closed.
3. Group mention behavior must support authorized command bypass without weakening default mention gating.
4. Configuration/docs/onboarding semantics must be aligned (no contradictory meaning of `groupAllowFrom`).
5. Add tests for security-critical logic.

## Current Problems (Observed)

1. `CommandAuthorized` is not set in WeChat inbound payload.
   - OpenClaw defaults missing `CommandAuthorized` to `false`, so control commands are effectively unauthorized.
2. Mention gating is simplistic.
   - Group messages are skipped when `requireMention=true` and last message lacks mention.
   - No authorized command bypass behavior.
3. `groupAllowFrom` semantics are inconsistent.
   - Runtime treats it as sender ID allowlist.
   - Onboarding prompt currently describes it as group ID allowlist.
4. Group batch behavior has a mention bug.
   - Detects mention in whole batch, but final dispatch gate checks only `lastMsg.isMentioned`.
5. Allowlist matching is exact-string only.
   - No wildcard handling, no normalization.

## Security Model (Target)

### DM access

- `dmPolicy=disabled`: deny all DM traffic.
- `dmPolicy=allowlist`: allow only normalized sender IDs in effective allowlist.
- `dmPolicy=open`: allow DM traffic, but commands still separately gated.

### Group access

- `groupEnabled=false` at group override: deny.
- `groupPolicy=disabled`: deny all group traffic.
- `groupPolicy=allowlist`: require non-empty group sender allowlist and sender match.
- `groupPolicy=open`: allow group traffic, subject to mention gating.

### Command authorization

- Compute `CommandAuthorized` only when message has command tokens.
- Use OpenClaw command-gating primitives (`resolveSenderCommandAuthorization` with `useAccessGroups` behavior).
- In groups:
  - Unauthorized control commands are dropped early.
  - Authorized control commands can bypass mention requirement.
- In DMs:
  - Unauthorized control commands do not get command privileges (normal chat handling remains possible under DM policy).

### Mention gating

- Default: `requireMention=true` for groups.
- If group requires mention:
  - process when explicitly/implicitly mentioned, OR
  - process when command bypass conditions are satisfied:
    - message has control command
    - command is authorized
    - text command handling enabled for this surface

## Refactor Plan

## 1) Add dedicated access-control module

Create `src/access-control.ts` with pure, testable logic:

- ID/allowlist normalization
  - `normalizeWeChatId`
  - `normalizeWeChatAllowFrom`
  - wildcard support
- policy context resolution
  - resolves effective DM/group allowlists
  - resolves group-level overrides (`enabled`, `requireMention`, `groupPolicy`, `allowFrom`)
  - uses secure runtime fallback for missing group policy
- inbound access decision
  - explicit allow/block with reason strings
- command authorization helper
  - wraps OpenClaw `resolveSenderCommandAuthorization`
- mention gate helper
  - explicit `shouldBypassMention` + `shouldSkip` outcome

Complex logic pseudocode:

```ts
policy = resolveWeChatPolicyContext(account, cfg, chatId, storeAllowFrom)

if (isGroup) {
  if (!policy.groupEnabled) deny("group-config-disabled")
  if (policy.groupPolicy === "disabled") deny(...)
  if (policy.groupPolicy === "allowlist") {
    if (policy.effectiveGroupAllowFrom is empty) deny(...)
    if (!senderInAllowlist(senderId, policy.effectiveGroupAllowFrom)) deny(...)
  }
  allow(...)
} else {
  if (policy.dmPolicy === "disabled") deny(...)
  if (policy.dmPolicy === "allowlist" && !senderInAllowlist(senderId, policy.effectiveAllowFrom)) deny(...)
  allow(...)
}
```

## 2) Refactor monitor to use access-control module

In `src/monitor.ts`:

- Replace ad-hoc `isMessageAllowed` logic with `resolveWeChatInboundAccessDecision`.
- Resolve scoped store allowlist once per chat-processing pass.
- Compute and set `CommandAuthorized` for ctx payload.
- Add unauthorized group control-command drop.
- Replace mention gate check with `resolveWeChatMentionGate`.
- Fix batch mention handling:
  - use segment-level mention status (`segment.some(pm => pm.isMentioned)`) instead of last-message-only.
  - avoid buffering command-only batches that require mention; allow dispatch path to decide authorization.
- Align batching with WhatsApp/Telegram command behavior:
  - if any message in a dispatch window contains a control command, do **not** use multi-segment `NO_REPLY` batching for that window.
  - command-bearing messages must be dispatched as single command-capable inbound contexts.
  - keep segmentation only for non-command/media-heavy traffic.

Complex dispatch pseudocode:

```ts
allowTextCommands = core.channel.commands.shouldHandleTextCommands({ cfg, surface: "wechat" })
hasControlCommand = core.channel.text.hasControlCommand(commandBody, cfg)
commandAuthorized = resolveWeChatCommandAuthorization(...)

if (isGroup && allowTextCommands && hasControlCommand && commandAuthorized !== true) {
  drop("unauthorized group control command")
}

mentionGate = resolveWeChatMentionGate({
  isGroup,
  requireMention: policy.requireMention,
  wasMentioned: segmentHasMention,
  allowTextCommands,
  hasControlCommand,
  commandAuthorized: commandAuthorized === true,
})
if (mentionGate.shouldSkip) drop("mention required")

ctx = finalizeInboundContext({
  ...,
  CommandBody: commandBody,
  CommandAuthorized: commandAuthorized,
  WasMentioned: mentionGate.effectiveWasMentioned,
})
```

Command batching pseudocode:

```ts
const hasControlCommandInWindow = processed.some(pm =>
  core.channel.text.hasControlCommand(pm.rawBody, cfg)
);

const segments = hasControlCommandInWindow
  ? processed.map(pm => [pm]) // disable NO_REPLY batching for command-bearing windows
  : buildSegments(processed); // existing media-aware segmentation
```

## 3) Align config schema/types/onboarding/docs

Update WeChat group config entry shape:

- add `enabled?: boolean`
- add `groupPolicy?: "open" | "allowlist" | "disabled"`
- add `allowFrom?: string[]`
- keep `requireMention?: boolean`

Clarify `groupAllowFrom` as **group sender** allowlist (not group IDs) everywhere:

- onboarding prompt text
- README config table text

## 4) Add tests (security-critical coverage)

Introduce unit tests for `access-control.ts`:

- normalization and wildcard behavior
- DM policy decisions
- group policy decisions
- group override precedence
- command authorization (authorized/unauthorized, computed/not computed)
- mention gate bypass behavior

## 5) Validate

Run:

- plugin package tests
- typecheck
- build

Then provide final diff summary + behavior matrix.

## Non-goals for this change

- Introducing full native slash command menus for WeChat (Telegram-style native command UX is provider-specific and not required for secure text-command support).
- Multi-account WeChat redesign (current plugin is default-account scoped).
