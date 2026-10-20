# Offline official-profile library

`wechat-profile` reads an explicitly selected stopped Linux account profile. It
runs without an official executable, agent-server, network connection or native
login. It does not discover passwords or acquire memory from a process.

```rust,no_run
use wechat_profile::{KeyFile, ListQuery, ProfileLocation, ProfileReader, ReadOptions};
# fn main() -> wechat_profile::Result<()> {
let location = ProfileLocation::new("/private/account")?;
let keys = KeyFile::load("/private/keys/profile.keys.json")?;
let reader = ProfileReader::open(location, keys, ReadOptions {
    stopped_or_consistent_snapshot: true,
    ..Default::default()
})?;
let first = reader.contacts(&ListQuery::default())?;
# Ok(()) }
```

Select one account root containing `db_storage`, or that directory itself. A
whole home containing several accounts requires explicit account selection by
the caller. `accountBinding` in a keys file binds the directory name and file
salts; it does not authenticate the account with WeChat. Network writes require
an independently verified authenticated-username binding.

Every database query opens a private temporary main/WAL/SHM copy. Source paths
are never opened by SQLite. A double inventory and SHA-256 scan detects changed
or added files during copying. This is **not** a live cross-file snapshot
algorithm: the caller must stop the writer or supply a consistent snapshot.
Direct mode and managed-copy mode have the same ownership requirement. SQLite
read-only/immutable flags alone do not make a live WAL snapshot coherent.

The reader discovers every actual ordinary/business message shard and uses its
own `Name2Id`; local message IDs include relative database path and table. Raw
SQLite fields preserve NULL, signed 64-bit integers as decimal strings, BLOBs,
invalid UTF-8 TEXT, compression flags and unknown columns. Server IDs expose the
full unsigned 64-bit decimal representation. Contact/member catalogs preserve
cached system, official, group and member rows; they are not a friends count.
Group membership is checked against the retained protobuf member identities.

List limits are 1–200. Opaque local cursors bind the selected snapshot generation,
query and complete ordering tuple, including cross-shard ties. They are not API
tokens, daemon event cursors or native SyncKeys. Changed source generations
expire continuation instead of mixing snapshots. Zstandard, SQLite BLOB size,
query duration, source size/count and metadata parsing have explicit bounds.
Schema fingerprints use definitions, columns, indexes and triggers. SQLite's
schema counter is diagnostic, not an official release number. Custom FTS
virtual tables remain inventoried but their tokenizer behavior is unsupported.

## Keys and local media

The version-1 private JSON key file uses this shape (placeholder values are not
usable keys):

```json
{
  "schemaVersion": 1,
  "format": "official-linux",
  "accountBinding": "selected-account-directory",
  "sqlcipher": {
    "compatibility": 4,
    "pageSize": 4096,
    "kdfIterations": 256000
  },
  "databases": [
    {
      "relativePath": "contact/contact.db",
      "saltHex": "<16 bytes as hexadecimal>",
      "rawPageKeyBase64": "<32 raw bytes as base64>"
    }
  ],
  "imageDat": {
    "formatVersion": 2,
    "rawAes128KeyBase64": "<16 raw bytes as base64>",
    "xorByte": 0
  }
}
```

`KeyFile::load` requires an owner-only regular file. Secrets implement redacted
Debug and are zeroized on release. `write_new` requires a private parent and
atomically publishes a new 0600 file, never stdout or an overwritten file. The
calling CLI must also reject output inside the official profile.

`keys_from_password` accepts an explicitly supplied existing 32-byte account
password, derives against each discovered file's salt using the documented
SQLCipher-4 parameters, validates first-page HMAC and schema opening, and returns
only per-file keys. This is not password recovery or a full logical-page audit.
`keys_from_agent_cache` reads selected key records from a disposable coherent
copy of an explicitly selected agent database using its existing unlock token;
it never calls an initializer, migrator, server, or extraction helper. Multiple
cached sessions require explicit selection. Missing cache entries remain gaps.
An available `_image_aes` plus `_image_xor` pair is normalized separately; absent
XOR is not guessed. The legacy AES representation uses its first 16 ASCII bytes.

Cached avatars are validated raw JPEG/PNG blobs inside SQLCipher. Encoded voice
bytes retain their media-shard namespace. V2 DAT encode/decode independently
preserves the observed header/control/partition policy and is bounded to 64 MiB.
Other DAT versions/control values and the observed encoding policy below 1024
plaintext bytes are unsupported. ECB/XOR wrapping is not authenticated encryption.

`cached_image` resolves a selected message through original-content MD5 and its
hardlink directory maps, rejects multiple cache groups and validates decrypted
JPEG/PNG with decoding limits. Best returns the best available candidate with
qualified quality. Original requires a decoded MD5 match to the advertised
original; `_h` alone is insufficient. Standard/thumbnail role selection, missing
or ambiguous bindings, generic file/video/sticker caches and uncached downloads
remain explicit gaps. No media method fetches from the network.

## Narrow profile writes

Generic `require_writable()` remains unsupported. Additive schema changes may be
readable but never authorize generic writes. The separate `apply_incoming_text`
operation implements only an initialized direct-chat incoming plain-text family
validated against Linux 4.1.13.26. It requires confirmed exclusive ownership,
keys/account binding and a caller-provided reference to an **already durable**
application intent. The caller must hold its profile lease and coordinate all
account writes. An advisory lease cannot exclude an unmanaged official app.

The writer requires exactly one ordinary message shard, existing sender/receiver
and session maps, the exact known column/type/default/PK declarations, four
message indexes and no relevant triggers. It never creates chats, maps, schemas,
shards, FTS indexes, credentials or native cursor projections. Incoming status,
origin and compression constants apply only to this verified family. Source
metadata and full server identity must be supplied exactly; missing server
sequence is not replaced by zero. Retained native AddMsg field 13 matched stored
`server_seq` in 319 exact same-account message joins; fields 6/7 did not.

New local IDs use the table's AUTOINCREMENT. The supported sort recipe is
`create_time * 1000` only when collision-free; collisions are rejected. A retry
compares all supported stored values under the same full server ID. A different
row is an error. Session updates preserve unrelated draft/options/clear fields.

Message and session transactions are separate: **cross-database crash atomicity
is not established**. An error can follow a committed message row. Retain the
intent, retry/reconcile the same payload, and do not advance native SyncKey or
publish committed events until all required profile/cursor projections complete.
This narrow writer alone does not implement that account-wide barrier. Group,
outgoing, media and unrecognized native commands remain pending/unsupported.
A profile-only export cannot include our sidecar recovery state or guarantee a
compatible saved-login projection.

`mark_read_messages` removes only selected source-qualified unread observations
in one session transaction. It decrements by actual removed rows and preserves
pre-existing counter/list exceptions, unrelated arrivals and unrelated fields.
Retries are idempotent. Read state remains desktop-local and separate from phone
state, native synchronization and consumer acknowledgements. The caller owns
its model-handoff policy, receipt persistence and concurrency/version checks.

## Validation

Run `cargo test -p wechat-profile` and `cargo clippy -p wechat-profile --all-targets
-- -D warnings` from the native Rust workspace. Synthetic tests cover SQLCipher,
key-file protection, key cache export, WAL preservation, all-shard mappings,
uint64 identities, cursors, Zstandard limits, schema drift, DAT round trips,
media binding and narrow text/read retry/recovery.

Independent network-disabled validation opened 20 databases in an authorized
stopped encrypted snapshot: 2,736 contacts, 104 threads, 32 groups, 2,918 member
records, 44 decoded cached avatars and 2,692 messages with no unresolved peers.
The 17 prior plaintext exports returned the same counts. All 60 source encrypted
main/WAL/SHM files and the 17 plaintext exports retained their hashes.

A separate disposable encrypted clone accepted one retained incoming text,
reopened with exact body/source/full server identity, retried without duplicate,
and projected exact-ID read state from 1 to 0 with an idempotent read retry.
This validation used no network, official runtime, login or message send. It is
not a fresh official-client handoff, complete server-history proof, or generic
multi-file recovery certification. The examples print only aggregate outcomes;
private input data and keys are never shipped with the crate.

## Stored identity

`read_mmkv_identity` decodes only the inspected v4 framing with a caller-supplied 16-byte format key. It bounds sizes/records, verifies ciphertext CRC, applies overwrite/tombstone semantics and reads the exact username/UIN/device fields. Unknown versions and nonzero expiration/flags metadata are rejected. CRC is not authentication; the daemon compares stored identity against its independently authenticated account before enabling writers and rechecks it before each write. No format key, private store, official binary reader or MMKV writer is included.
