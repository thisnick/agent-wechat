# Contact update mapping status

**Updated 2026-10-26.** The official Linux client automatically applied a phone
mute/unmute change through an independent contact stream. The current branch
implements that stream. Standalone direct-chat phone mute/unmute and outgoing
mute/unmute followed by independent server refresh passed. Group-stream fixtures
pass; a standalone phone group toggle remains unverified.

## Baseline and status

| Item | Status |
| --- | --- |
| Official reference | Linux 4.1.13.26 AMD64; SHA-256 `2749535f299ba67feb8ef80cab898bb0531fa8f376b16f8a72db706811c84132` |
| Released/source comparison | `thisnick/agent-wechat` at `27a8b54`, v1.0.0-beta.15 |
| New implementation | Branch `codex/thread-mute-state`, following `0527289`: nullable mute reads, independent contact sync, and native desktop notifications |
| Code verification | Pinned formatting and warning-denying Clippy; metadata, contract and TypeScript checks. 634 workspace tests passed, one benchmark ignored; tests cover uncertain mute reads, direct mutation recovery and group contact-stream mute changes |
| Isolated runtime verification | Debian-built hub/CLI and real SDK/session simulator smoke passed, including thread detail/list mute assertions after group mute/unmute |
| Live daemon acceptance of new code | Deployed Linux x64 validation daemon: command356, contact drain, saved mute, SSE and API/CLI state observed |
| Automatic phone updates | Direct mute and unmute passed without refresh; group mapping fixture tested, phone group toggle pending |

## Delivery and synchronization

| Mapping | Evidence | Implementation status | Remaining work |
| --- | --- | --- | --- |
| Command 24 notification, four-byte big-endian selector | Existing official/native push experiments; observed again in this test | Daemon handles selectors with low three bits set and invokes NewSync CGI138 | This is not the complete incoming notification surface |
| Message NewSync command list | Audited native message batches contained message commands, with no matching contact update | Encrypted received batches and unresolved commands are retained | Absence of ModContact in this window does not prove absence of all upstream pushes |
| Unsolicited command356 | Official phone trace; namespace11 / notice2048 / selector0x20004 | Implemented, coalesced separately from message hints | Direct-chat live acceptance passed; phone group toggle pending |
| Follow-up CGI8674 | Official trace pairs command356 with CGI8674 | Implemented with account/session/logout fences | Direct-chat live acceptance passed; phone group toggle pending |
| CGI8674 endpoint | Matching-build implementation and request capture | Uses `/cgi-bin/micromsg-bin/contactsync` | The separate `bypcontactsync` route is not used |
| Contact-sync request fields | Matching-build drain and captured requests | Implemented: BaseRequest1, scene2=3 initially / 4 for continuation, repeated uint64 selector3, contact cursor4, message context5 | Two captured initial requests serialize byte-for-byte. Continuation has matching static evidence; live multipage acceptance pending |
| Contact-sync response fields | Matching drain and captured final response | Implemented: repeated uint64 continuation selectors2, unwrapped cursor3, repeated raw CmdItems4 | Nonzero selectors require another page. Errors or stalled cursors do not establish catch-up |
| Contact payload | Response4 contains field1 command2 and raw ModContact in field2 | Routed through sealed receipts and the existing partial contact mapper | This stream uses raw CmdItem payloads; NewSync uses SKBuffer. Known fields apply once; unsupported remainder stays held |
| Additional sync keys | Matching-build namespace11 and exact saved cursor match | Uses `config/rs_config`, key `sync_11` | Independent of message SyncKey and login-store `mmkv_key_byp_sync_key`; official cursor waits for all commands to apply |
| SSE notification | Existing contact mapper emits after supported writes | Integrated with receipt deduplication; UI coalesces contact-stream bursts into catalog/thread-page refreshes | Phone-to-SSE/API/CLI passed; UI burst handling fixture tested |

The native event stream alone does not establish complete push coverage.
Contact updates use an independent delivery path; message-only observations do
not prove that a phone change was absent from the server.

## Contact field mapping

“Supported” below refers to the existing ModContact mapper once it receives a
record. The delivery path and live acceptance have separate statuses above.
Fields absent from an update must remain distinct from present empty/zero values.

| Data | Wire field(s) | Seen in reverse capture | Profile/API handling | Gap or condition |
| --- | --- | --- | --- | --- |
| Contact identity | 1, nested SKString1 | Yes | Existing contact/name-map identity | Validate before writing |
| Nickname | 2 | Yes | `nick_name`; contact display data | Sparse updates preserve absence |
| Nickname phonetics | 3,4 | Yes | `pin_yin_initial`, `quan_pin` | Independent presence |
| Personal remark | 10 | Present empty, version-controlled | `remark` when uncontrolled | Full version-based merge missing; preserve controlled saved values |
| Remark phonetics | 11,12 | Present empty, version-controlled | `remark_pin_yin_initial`, `remark_quan_pin` when uncontrolled | Same restriction |
| Alias | 30 | Absent | `alias` when present | Not live-tested by this toggle |
| Large/small avatar URLs | 39,40 | Yes | `big_head_url`, `small_head_url` | Saving a URL is separate from refreshing cached image bytes |
| Additional contact identifier | 45 | Yes | `encrypt_username` | Preserve the existing field interpretation |
| Verification flags | 24 | Yes | `verify_flag` | No claim that every badge or contact category is mapped |
| Primary mask/value | 7,8 | Full mask; value3 on unmute | Full mask replaces `flag`; derives supported `local_type` | General partial-mask merge missing |
| Direct-chat mute | Primary bit0x200 | Profile3 → 515 → 3 across official test | Derives nullable `NativeThread.muted`; received narrow mask0x200 merges preserve other bits | Phone3→515→3 observed in standalone daemon without refresh; unresolved mute controls yield API null |
| Other primary bits | Within8 | Yes | Retained in full saved flags; narrow mute edits preserve them | Not every bit is a named feature/API setting |
| Group notifications | 17 | Present0 in direct record | `chat_room_notify`; group0=muted,1=unmuted | Direct `chat_room_notify` stayed0 in both states and must not determine DM mute |
| Group membership state | 85 | Present0 in direct record | `is_in_chat_room` | Does not establish a complete roster |
| Member version/signals | 53,57 | 53=0; 57 present | Known group signals can queue roster reads | Inline data must not replace an unproved complete roster |
| Group detail version | 64 | Present0 | Known positive group version can queue detail reads | No positive version transition in this DM test |
| Secondary mask/value | 82,83 | Full mask; value256 | Raw receipt retained; general profile merge absent | Prior official experiment identified a minimize bit; do not infer all secondary preferences |
| Per-scope field controls | 89 | Scope1/version24, IDs10/11/12 | Known control IDs parsed; controlled known fields preserved; unresolved incoming command stays held | Stored-version comparison and full restoration/merge logic missing |
| Other scalars and buffers | Including5,6,9,13,14,16,18,22,23,26,27,33–37,42,50,55,56,58,62,65–67,70,76,80,81,84,86–88,90 | Many present | Retained in raw records; most not projected by the sparse mapper | Keep meanings and storage mappings unknown until matching evidence establishes them |

The inspected unmute contact had full primary mask/value3, full secondary
mask/value256 and remark controls. The successful flag change is not evidence
that a blind record replacement would preserve every unrelated preference.

## Public interface and storage boundaries

| Surface | Current status | Follow-up |
| --- | --- | --- |
| `GET /threads`, `GET /threads/{id}` | New branch adds nullable `muted`, read in one profile query plus one operational-marker query for the returned page | Direct phone transitions observed; group phone transition pending |
| `wx chats list/get` | Receives the same thread preference; no new upstream read during listing | Keep `null` for missing/incomplete evidence |
| `wx contacts refresh ID` / contact refresh API | Existing targeted lookup updated the profile in both native phone tests | Manual refresh remains available alongside the independent contact drain |
| `wx chats mute/unmute` / `PUT /threads/{id}/notifications` | Ordinary direct contacts and groups; guarded one-attempt mutation journal | Direct lookup supplies the current full ModContact; only bit0x200 changes. Server acceptance and independent contact refresh confirmed both states |
| Official contact database | Owns the settled preference | Do not duplicate mute preferences in a sidecar; unresolved-work markers can make an API value unknown |
| Sidecar receipts | Raw received data, application markers and unresolved work | Separate received/applied cursors, phase receipts, restart/logout guards and profile-export generation checks implemented |
| Desktop notification suppression | Implemented in native listener, settings and click navigation | Requires `contactSyncReady` and known unmuted thread. Unit and Linux service acceptance are recorded separately from installed macOS/Windows validation |

## Evidence and next acceptance

1. **Native observation:** two direct contacts changed3→515 only after targeted
   contact refresh. The second contact also returned515→3 after refresh.
   Message-stream journals showed no contact command in the audited window.
2. **Official observation:** passive test recorded contact3→515→3 without opening
   or refreshing that chat. The reverse request/response capture contains the
   target's ordinary ModContact with primary flag3 and the controls above.
3. **Coverage boundary:** other message notification selectors remain unmapped.
   Contact sync is independently implemented and does not replace message sync.
4. **Implementation acceptance:** request bytes and saved cursor match the
   official capture; synthetic tests cover interruption, stale replay, logout
   fences, retained unknown commands and uncertain mute suppression. Isolated
   container integration and native Linux SSE-to-D-Bus delivery passed. Still
   required: standalone phone group mute/unmute and installed macOS/Windows alerts.
5. **Standalone direct-chat acceptance:** phone mute/unmute emitted command356,
   contact-stream events and API/CLI true/false without a manual refresh. Native
   outgoing mute/unmute were each accepted and independently refreshed from the
   server; the test finished muted. The official reference uses opcode2 with a
   complete ModContact. A flag-only request was rejected; the implementation
   fetches the current contact and preserves every byte except the changed flag.
   Rejected and uncertain operations are never automatically resubmitted.

## Implementation references

- [Protocol push reader](../packages/native-client-rust/crates/wechat-protocol/src/push.rs)
- [Contact drain](../packages/native-client-rust/crates/wechat-core/src/contact_sync.rs)
- [Contact codec](../packages/native-client-rust/crates/wechat-protocol/src/contact_sync.rs)
- [Account push coordinator](../packages/native-client-rust/crates/wechat-core/src/push.rs)
- [Contact projection](../packages/native-client-rust/crates/wechat-core/src/metadata.rs)
- [Official profile cursor publication](../packages/native-client-rust/crates/wechat-core/src/checkpoint.rs)
- [Thread summaries](../packages/native-client-rust/crates/wechat-core/src/thread_summaries.rs)
- [Desktop listener](../packages/desktop/src-tauri/src/notifications.rs)

Supporting captures, artifact analysis and databases remain outside this public
repository. This tracker records implementation status and validation boundaries.
