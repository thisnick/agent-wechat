# Sync-control classifications and validation

The daemon keeps a received batch until every command has a recorded result.
An unfinished batch can contain both completed messages and unsupported controls;
its pending status does not mean all its messages are missing. Completing specific
controls does not authorize skipping other entries or publishing a complete
official checkpoint past them.

## Evidence boundary

The classifications below come from static analysis of WeChat Linux x64
4.1.13.26, SHA-256
`2749535f299ba67feb8ef80cab898bb0531fa8f376b16f8a72db706811c84132`,
plus offline parsing of saved bytes. This is not observed execution of those
handlers, a live replay result, or a claim about another platform/build. Private
payloads, identities, binary extracts and analysis exports are not checked in.

## Effects established

| Control | Established effect and current policy |
| --- | --- |
| Status 1 / supported `lastMessage` | Apply the supported remote unread clear. |
| Status 2 | Reaches unread/session updates and observers. Keep blocked until the complete effect is mapped. |
| Status 3 / self-to-self, own username only | No registered handler in this build. Complete only this exact form. |
| Status 4 | Reads history settings and schedules work with chat/read maps. Keep blocked. |
| Status 5 / supported `lastMessage` | Apply the existing once-only unread component. Session-exit observers and auto-dismiss work remain unresolved, so the complete control stays blocked. |
| Status 7 / 9 | Reach Moments state services. Keep blocked. |
| Status 11 | Only the previously verified empty argument or empty inner-opcode-4 list is supported. Other handoff operations and populated lists remain blocked. |
| Status 15 / own-to-peer, zero/zero `lastMessage`, no username child | No registered handler. Complete only the exact two-string zero/zero shape. |
| Status 16 / own-to-peer, positive `lastMessage`, no username child | No registered handler. Require canonical positive unsigned 64-bit strings for both JSON fields. |
| Status 23 / supported `packagesUpdated` | Previously verified no-handler form; other shapes remain blocked. |
| System `ClientCheckGetExtInfo` | No matching handler; complete only `weixin` to self with the three supported text fields. |
| System root `appmsg`, subtype 35 | Dedicated log-only branch. Require type 10002, self-to-self and one exact subtype field; ordinary type-49 cards are unaffected. |
| System `mmchatroombarannouncememt` | No matching handler; complete only self-to-chatroom with the three supported text fields. The spelling is intentional. |
| System `dynacfg` | Publishes config and invokes an MMKV storage writer. Keep blocked. |
| System `functionmsg` | Queues work. The observed command 614 skips a CGI branch supporting 825, but callbacks are not fully mapped. Keep blocked. |
| Group-prefixed recall / system template / pat | Valid XML after a group sender prefix. These have real handlers; never classify them all as no-effect. Only supported recall shapes use the existing recall writer. |

The status-table constructor registers exactly ten IDs, and its lookup/dispatcher
advances without calling a null handler. Each added absent-handler classification
is limited to a particular opcode and recorded shape. The system dispatcher has
a separate predicate set and a dedicated subtype-35 branch. There is no general
unknown-control skip rule.

Opcode 2 reads and normalizes the target chat, obtains or prepares session state,
emits an entered-session observer notification, and reaches the shared unread/session
update path using the outer AddMsg timestamp. It does not parse `lastMessage` JSON.
A notification within about 30 seconds also enables recent-session handling;
special peers have separate branches. A read-only subset would still leave the
rest of the control unfinished, so opcode 2 currently remains blocked.

## Timestamp fix

The JSON `MsgCreateTime` string can exceed 32 bits. Rejecting it as a `u32`
prevented otherwise matching opcode-5 entries from reaching the existing partial
unread writer. It is now validated as a canonical positive `u64` without converting
its units. The actual read cutoff remains AddMsg's separate notification timestamp.
Zero, signed, noncanonical and overflowing values remain unsupported. Recovery
still honors the previous partial-attempt marker, including uncertain attempts.

## Verification

Synthetic encrypted-profile tests cover unchanged unread/session/draft state for
no-effect controls, encrypted original-byte retention, event/receipt deduplication,
restart, mixed unknown batches, and progress of later messages. Wide timestamp
fixtures verify the notification cutoff and ensure an opcode-5 partial read never
finishes its full control or clears newer state during recovery.

Group recall tests verify the incoming account/chat/sender envelope, explicit target
server identity, preservation of the original message identity, suppression after
restart, and no extra chat row for the control. The ordinary recall form without
`revoketime` also accepts one empty `announcement_id` without attributes or nested elements;
the matching parser does not consume that field. Populated, attributed, nested,
duplicate and otherwise unsupported fields remain blocked.

No daemon or official app is needed for these tests. Existing blocked batches
require a separate, authorized recovery after deployment. Parser recognition alone
does not establish successful database reconciliation or full backlog drainage.
