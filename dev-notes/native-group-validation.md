# Native group validation, 2026-10-23

This change supports a subset of ordinary group operations. Protocol mapping,
server acceptance, saved-profile writes and another device's UI are separate
checks. No complete official-profile handoff or universal platform claim follows.

## Reference and method

The serializers were cross-checked against the matching Linux 4.1.13.26 AMD64
artifact (SHA-256
`2749535f299ba67feb8ef80cab898bb0531fa8f376b16f8a72db706811c84132`),
retained controlled official-client exchanges and protobuf field evidence.
Cross-platform schema names were comparison evidence only. Raw private evidence,
account IDs, credentials and profile paths are excluded from this repository.

The built native image ran in an isolated container with an exclusively owned,
preserved test profile. The official executable stayed stopped. A coherent backup
preceded the sidecar migration. The account owner approved the test contacts and
phone-side edits. Production containers were unchanged.

## Native execution

| Operation | Server result | Separate verification |
| --- | --- | --- |
| Create with two selected test contacts | BaseRet0, one member status0, the other status3 | Returned group ID retained; only accepted members saved. Status3 was not bypassed or automatically retried |
| Create with one selected contact | BaseRet-2 for both named and empty-topic controls | No group identity returned; admission now requires at least two selected contacts |
| Rename | CGI681/op27, outer0/command0 | Saved shared title changed; subsequent phone rename retrieved independently |
| Own group display name | CGI681/op48, outer0/command0 | Separate CGI551 read returned the exact new display name and saved it |
| Announcement clear | CGI993, BaseRet0 | Phone-created nonempty announcement read first; a separate CGI223 after clearing returned empty text/XML |
| Leave | CGI681/op4,7,16 results -1,0,0 | Extra named-create group marked not joined; saved group/history retained |
| Mute/unmute | CGI681/op20, outer0/command0 | Final-image native mute and unmute each passed separate server contact/detail readback |
| Remove member | CGI179, outer0, matching returned username | Separate full identity list contained only the actor |
| Add member back | CGI120, outer0/member status0 | Separate full identity list restored two members |

Both empty-topic and named two-contact creation returned a partial success; the named control returned its requested shared title. The extra named group was left after validation.

The first successful creation reply contained present, zero-byte SKString
wrappers. A decoder gap initially retained it as `needs_review`. After correcting
the decoder, startup resolved the original operation using its sealed reply,
without repeating the create request. Missing required wrappers remain errors.

The final image restarted online with saved authentication. Reusing the completed
unmute key through the SDK returned the original operation ID. A fresh native
mute emitted a correlated `group.updated` over real SDK SSE; a group read at event
receipt already showed the saved muted flag. Independent server reads confirmed
both mute and restoration to unmuted. The owner-only group-info tool was exercised
as a harness against the real daemon, not as a new full Gateway deployment.

## Other-device receiving

A phone announcement was also absent from the cache before refresh and correctly read afterwards, preserving its blank line. Two owner-performed phone rename controls were checked before any direct group
refresh. The second also enabled mute on the phone. Neither observation window
provided a matching ModContact command; the batches contained status messages.
`wx groups refresh` independently retrieved the correct title and notification
state and saved them. This is a demonstrated automatic-sync gap, not a successful
push test.

The implemented incoming path handles present ModContact name/notification/
membership fields, queues read-only CGI551 for roster version signals and CGI223
for announcement version signals, and emits `group.updated` after saved changes.
Synthetic tests cover this pipeline, delta rows plus a separate full member list,
replay/event deduplication, controlled fields, stale receipt rejection and local
repair that cannot fetch or resend. Delivery of every other-device edit remains
unverified. Unknown control ordering, masks and system messages remain retained.

## Profile and recovery limits

Supported SQL and packed roster fields preserve other members, unknown chunks and
history. A count of changed rows does not establish a full roster. Complete
replacement requires the separate count-checked identity list and enough cached
member material. Per-member failures and omitted members stay distinct.

Local notices, search indexes, some opaque room metadata and full contact-control
version merging are not reconstructed. `profileProjection: partial` reports this
limit. Local repair consumes owned receipts only. Interrupted or uncertain network
steps stop at `needs_review`; they are never automatically submitted again.

Announcement publication needs a distinct HTML-resource upload path whose full
mapping/readback is incomplete. Direct-thread mute and personal remarks need more
complete contact reconstruction. These operations are not exposed as supported.
