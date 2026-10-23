# Recall author, notice and search cleanup

This change uses static analysis and offline fixtures. It does not establish a
new live acceptance or official-client rendering result. No shared daemon,
official app or live account profile was used for this validation.

## Author and notice

The matching WeChat Linux 4.1.13.26 x64 parser reads the target chat, target
server message ID and replacement text from an ordinary recall control. Its
consumer replaces the original with a type-10000/status-4 row, preserving the
message identity and order while clearing the real sender. The control's outer
sender and its own message ID do not identify the original author or target.
The cleared row does not provide an independent recall-initiator identity.

Before clearing the row, the native client saves its original sender and
direction in the account-local recall marker. A late original can fill unknown
values. Contradictory known author metadata fails rather than overwriting the
first observation. An already-cleared imported row retains an unknown author;
the API returns null sender/direction instead of inventing an incoming message.

The server replacement text is limited to 16 KiB and stored as text in the
official recall XML and session preview. XML characters are escaped. A missing
notice uses the existing generic fallback. The API body stays
`[Message recalled]`; optional `originalSenderId`, `originalDirection` and
`replacementText` appear in its `recall` object and in the first
`message.recalled` event when known. Enrichment preserves event deduplication.
The TypeScript SDK validates those fields and OpenClaw preserves them when
redacting cached messages.

## Search cleanup and recovery

Selective message-index deletion must preserve other messages, dictionary and
image-search tables. It must remove postings as well as the stored content row.
An absent index needs no change; an unsupported index cannot be silently marked
complete. Existing recall suppression remains in effect while cleanup is pending.

The reference lookup uses chat, sort sequence and local message ID to find FTS
document IDs, then deletes those IDs from the virtual table and auxiliary table.
The writer additionally checks the source's server ID and creation time, the
index-local chat mapping, the stored creation time and correlated auxiliary
row ID. It requires a single supported message shard and scans the verified
ordinary index partitions; an index partition number is not a message shard.

The supported search layout is contentful FTS5 with seven columns, one indexed
content column, full positional detail, `MMFtsTokenizer disable_pinyin`, and a
three-column auxiliary table. The matching binary also contains a newer
external-content layout. That layout remains unsupported
and keeps cleanup pending. General index creation, retokenization and rebuild
remain outside the writer.

For deletion, the writer reads the exact existing terms and positions through
FTS5's vocabulary interface, checks document-size counts, and arms a private
callback only for the selected stored content. Genuine FTS5 `DELETE` replays
those terms and updates its own postings, content and size records. The callback
rejects unrelated tokenization. It does not approximate the vendor tokenizer or
write shadow tables directly. Resource limits and post-delete checks protect
the transaction; failures roll back all selected index and auxiliary deletions.

The marker is committed before official database updates. Each database uses
its own transaction, so retry after partial completion uses the retained target
identity and local bytes. It does not submit a recall, send a message or fetch
media. The source/server identity, timestamp and ordering fields survive the
cleared row for replay.

## Verification boundary

Synthetic encrypted-profile fixtures cover own and peer messages, group
authors, other-device own messages, absent originals, late binding, restart,
replay, malformed and oversized notices, escaped text, unknown imported authors
and failed local projection. Store migration, event and consumer fixtures check
metadata validation and single-event behavior.

Search fixtures use both encrypted and plaintext synthetic databases. A fixture
tokenizer supplies independently chosen Chinese tokens, colocated synonyms and
repeated/shared terms. Checks inspect actual vocabulary postings and searches,
preserve unrelated documents, and test schema/identity rejection, armed deletion,
rollback, replay and engine integrity after deletion.

Edit-after-recall storage and packed metadata/compression parity are outside
this change. Cached media, encrypted journals, backups and unknown files remain;
this is not secure deletion or complete official-profile parity.
