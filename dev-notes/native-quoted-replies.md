# Native quoted text replies

Quoted replies use the ordinary send queue and a type-49 application message
with subtype 57. This implementation has static and synthetic validation;
server acceptance and recipient rendering/navigation are not yet validated.

## Interfaces

The text send request accepts `quote: { messageId, serverId }`. Both values are
strings; the server ID is a canonical positive unsigned 64-bit decimal. A token
needs both `read` and `send`. The SDK adds a final optional reference argument to
`sendText`; the CLI exposes paired `--quote-message-id` and `--quote-server-id`
flags. Other outgoing kinds reject quote fields.

The server resolves the source in the selected account and destination chat,
checks its server ID and rejects ambiguous or recalled sources. It creates the
reference metadata from saved bytes. Supported ordinary source forms are text,
image, voice, video, link (app subtype 5), file (6), and quoted reply (57).
Sticker, forwarded-record, other card, system and partial-selection forms are
outside this initial implementation. Announcement mention metadata is rejected.

## Static cross-check

The serializer was checked against WeChat Linux 4.1.13.26 AMD64. Its reference
builder and common SendAppMsg builder establish the XML fields, subtype 57,
optional outgoing source metadata and the separate outer empty metadata field.
The synthetic wire fixture records the expected field numbers, presence and
bytes with synthetic identities. Cross-platform schema comparisons were used
only as supplementary evidence.

The reference uses the selected message's raw type; app subtypes remain inside
its content. Own messages use the account as `fromusr`; incoming group messages
use the chat. `chatusr` identifies the sender. Ordinary media references retain
the source content; incoming group text removes the sender envelope. Quoting a
quote keeps the immediate reply's own comment without copying its older
reference chain. Embedded XML is escaped as text.

## Offline checks

Fixtures cover full request bytes and XML round trips, 64-bit IDs, bounds,
malformed references, source binding, quote-of-quote pruning, and local lookup
of each supported original kind. An encrypted-profile test exercises the real
send admission path with workers held offline, retained XML, same-key replay,
conflicts, cancellation, a recall before submission, a deleted source, accepted
history writeback and replay of local repair.

The final source check before submission uses the existing claim transaction,
so cancellation/logout can win. Accepted repair uses the sealed XML and receipt;
it does not submit a send. Existing submitting jobs retain the common
`needs_review` restart behavior and are not automatically resent.

Full workspace checks, generated contracts and an isolated image/CLI check are
recorded in the implementation PR. Live validation must separately establish
server acceptance, recipient rendering, reference navigation and profile parity.
A quote does not prove that the recipient can download its original attachment.
