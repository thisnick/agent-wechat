# Sync controls the daemon doesn't handle yet

Observed on 2026-10-22 on a test account during normal use: about 20 minutes, with the phone active and a few messages sent and received.

## What a sync control is

Besides messages, WeChat's sync sends small **control** entries that tell a client about state changes on the account's other devices. They are not chat messages and never appear in a chat. Two kinds showed up:

- **Status notifications** (message type 51). An XML command `<op id="N">…</op>` with a target chat and optional argument. Phones and desktops send these when you open a chat, read it, leave it or switch device.
- **System controls** (message type 10002). Server-side configuration and function messages (`functionmsg`, `dynacfg`).

## What the daemon does with them

For every sync batch, the daemon:

1. saves the raw batch first;
2. applies what it understands to the profile: messages, unread counts, contacts;
3. checks whether anything is left that it doesn't understand.

If a control is left over, the daemon **does not skip it**. It keeps the batch, emits `account.control_observed` and `projection.blocked`, and leaves the batch pending. This is deliberate: skipping an entry it doesn't understand could lose state the official app expects. As a result:

- `pendingProjectionBatches` in `wx account status` grows by one per blocked batch;
- `pendingCursorProjection` stays `true`, because the profile's own sync checkpoint can't be marked complete while earlier batches are pending.

Messages in the same or later batches are still applied right away. In the test, a phone-sent message and an incoming message were each committed about 0.3 s after their push, with 11–13 batches already pending.

## What was observed

| Control | Count | Meaning, as far as known | Handled? |
| --- | --- | --- | --- |
| Type 51, opcode 1 (`lastMessage`) | 0 in this window | "Read up to this message" on another device | Yes: clears local unread |
| Type 51, opcode 5 | 3 | Leaving a chat on another device; includes an unread clear | Partly: the unread clear is applied once, the rest stays blocked |
| Type 51, opcode 2 | 5 | Not established. In WeChat's older web protocol, code 2 meant "entered a chat" | No |
| Type 51, opcode 11 | 5 | Captured name `HandOffMaster`, empty argument; appears to be a device hand-off signal | No |
| Type 51, opcode 23 | 1 | Unknown | No |
| Type 10002 `functionmsg` / `dynacfg` | 3 | Server function and configuration updates | No |
| Type 10002 with unreadable metadata | 1 | Unknown | No |

Opcodes 11 and 2 arrived back to back, about 1.5 minutes before a message was sent from the phone. That fits "phone became the active device, then opened a chat."

## What this affects

- **Works:** receiving and sending messages, history, media, and reads done through the daemon.
- **Grows without limit:** the pending-batch list. Each entry is small, but it never drains while these controls keep arriving.
- **Blocks:** a complete sync checkpoint in the official profile. This matters for `store export` (it refuses unless `--allow-incomplete`) and for handing a profile back to the WeChat app. Neither is part of normal use.
- **Not reflected:** whatever state these controls carry. If opcode 2 really is "entered a chat", the daemon simply doesn't know which chat the phone has open, which it doesn't need. Opcode 5's remaining part (leaving a session) is also not applied.

## What to do

For each unhandled control, find out what the official Linux client does with it. That's the same static analysis already done for opcodes 1 and 5 (handler table, then the handler's effect on the profile). Then do one of the following:

1. **Apply it,** if the handler changes profile state the daemon should match.
2. **Mark it as having no local effect,** if the handler only changes UI or in-memory state. The batch can then be projected and the checkpoint advance. This must be an explicit, tested classification per opcode, not a general "skip unknown" rule.
3. **Keep blocking it,** if the effect can't be established.

Start with opcodes 2 and 11, which arrive most often. Opcode 23 and the type 10002 entries come next.

To check progress live, run `wx account status` before and after a session of normal phone use. Once these are handled, `pendingProjectionBatches` should stop growing and `pendingCursorProjection` should return to `false` when no new unknown controls arrive.
