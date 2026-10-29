# Official-profile storage library

Read and update supported official profile files. Start with `ProfileReader`
for queries; checked writers serve applications that exclusively own the profile.
The format adapter preserves supported WeChat storage behavior. The embedding
application owns network operations, account policy and crash recovery.

`stage_*` prepares bytes without changing files. Both `apply_*` and `publish_*`
perform writes: the former usually names a logical record update, while the
latter usually installs bytes at local paths. These names do not define different
transaction levels. For example, `publish_saved_key_info` can update a database
and a legacy file. An MMKV update stages new encrypted data and matching CRC
metadata in memory, then publishes those bytes into the existing profile paths.
No server upload is implied.

New profile mutations use `apply_*`. Existing `publish_*` APIs retain their names
for compatibility.

The existing public API also exposes coordination hooks such as
`WriteOptions::durable_intent_id`. This is a reference to the caller's previously
saved recovery record; the library checks the supplied reference, but cannot
prove the record exists or another process stopped. Those hooks do not add a
new official profile format or supply the daemon's recovery journal.

See the [Rust libraries guide](https://thisnick.github.io/agent-wechat/reference/rust-libraries/#profile-writes)
for the API boundary and write-phase terminology.

## Read a stopped profile

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
An available `_image_aes` plus `_image_xor` pair is normalized separately.
`ImageDatKey::derive(uin, username)` implements the inspected native rule: the
first 16 ASCII bytes of lowercase MD5(unsigned-decimal UIN concatenated with
username), with the native UIN shift/XOR selector. This is independent of the
SQLCipher password and does not authenticate a network account.

Cached avatars are validated raw JPEG/PNG blobs inside SQLCipher. Encoded voice
bytes retain their media-shard namespace. V2 DAT encode/decode independently
preserves the observed header/control/partition policy and is bounded to 64 MiB.
Other DAT versions/control values and the observed encoding policy below 1024
plaintext bytes are unsupported. ECB/XOR wrapping is not authenticated encryption.

`cached_image` resolves a selected message through original-content MD5 and its
hardlink directory maps, rejects ambiguous groups and validates decoded bytes.
Original requires an MD5 match; a filename suffix alone is insufficient.
`cached_voice` retains encoded voice bytes. `cached_hardlink_file` checks the
requested file/video MD5 and rejects ambiguous bindings. Network downloads and
sticker CDN jobs belong to the daemon, not this crate.

## Fresh layout and explicit projections

`initialize_profile` creates seven encrypted ordinary databases using the
inspected Linux 4.1.13.26 schemas. `initialize_official_home_with_login` also
creates known full-HOME identity/global MMKV records, the selected account's
`config/login_configv2`, and the native legacy key-info fallback. It publishes
only into a new or empty target, preserves failed staging for inspection, and
never overwrites a populated profile. The password must be the server-returned
account database password; a newly generated unrelated password cannot unlock
an existing profile. The account basename and image key are derived from the
bound native identity. A same-build official startup/rendering test remains
separate from schema creation and encrypted reopen tests.

Generic `require_writable()` remains unsupported. Explicit message/session,
contact/group/member, avatar, media-cache, saved-login and SyncKey projection
functions require exclusive ownership, account/key binding, exact supported
schema declarations and an **already durable** intent ID. The caller must hold
its profile lease and serialize all account writes. Advisory locks cannot stop
an unmanaged official app. Additive readable columns do not authorize writes.

Message IDs include database/table/local ID; server IDs retain all 64 bits.
Message writers currently require one ordinary message shard. New per-chat IDs
use AUTOINCREMENT and same-second sort allocation is bounded and collision
checked. Idempotent retries compare the full supported row. Native AddMsg
sequence is supplied explicitly; accepted outgoing messages use their separately
identified local pending/acknowledged policy rather than inventing an incoming
sequence. Message/session transactions preserve unrelated drafts/options and
read-counter exceptions. New nullable unknown fields are explicitly initialized
to SQL NULL; this policy is not evidence of every native constructor default.

Sparse contact updates distinguish an absent field from explicit empty text.
Roster patches map proved member identity/display fields, retain unknown root
and nested protobuf fields, and remove absent members only for a validated
complete roster. Group catalog entries do not fabricate a conversation, owner,
membership or complete remote group list. Custom native FTS tokenizers/index
writers are unsupported; source index files and backups must be preserved.

Image cache projection encodes V2 DAT and writes supported hardlink/resource
records for original, standard and thumbnail roles. The hardlink MD5 hash uses
the native 32-bit SuperFastHash of the ASCII MD5 string. Voice projection writes
encoded bytes in the bound media shard. File and video projections bind plain
cache bytes and supported ResourceInfo/Detail metadata to an existing message,
validate lengths/digests, and refuse unknown packed metadata. All cache file
publication is bounded, no-clobber/idempotent and separate from network transfer.
This does not imply every media subtype or stock UI transition is covered.

Cross-database/file crash atomicity is not provided by a single SQLite
transaction. The daemon must retain an encrypted replayable intent, reconcile
all rows/files, and publish the native cursor and committed event only after all
required projections complete. A profile-only export cannot carry sidecar
recovery state or promise a completed saved-login generation.

`mark_read_messages` removes only selected source-qualified unread observations
in one session transaction. It decrements by actual removed rows and preserves
pre-existing counter/list exceptions, unrelated arrivals and unrelated fields.
Retries are idempotent. This bounded writer remains an internal recovery
primitive for older durable intents.

`mark_read_thread` clears a whole conversation through a frozen server timestamp,
advances `last_clear_unread_timestamp`, and preserves unread rows after that
timestamp. When the session has no newer history, it also clears legacy unread
counter contributions absent from the list. When newer history exists, unknown
counter contributions are preserved. Drafts, session options and other peers
are preserved; a missing session is a no-op. It requires the same explicit
ownership, account binding, supported schema and durable intent as other writes.
The stored cutoff has whole-second precision. A later arrival with the same
timestamp shares that boundary; its separate ordering needs live validation.
Server message IDs are never numerically sorted to infer chronology.

The daemon's public read operation freezes its latest valid server identity and
time in local profile order, checks the expected version, applies this local
projection and separately journals a cross-device read notification. That
deliberately includes earlier messages in the same thread, including ones that
were not supplied to a model. The profile crate performs no network requests;
the daemon also uses this writer for strictly recognized incoming read controls.
Consumer acknowledgements remain a separate operation. The caller owns its
model-handoff policy, receipt persistence and concurrency/version checks.

## Validation

Run `cargo test -p wechat-profile` and `cargo clippy -p wechat-profile --all-targets
-- -D warnings` from the native Rust workspace. Synthetic tests cover SQLCipher,
key-file protection, key cache export, WAL preservation, all-shard mappings,
uint64 identities, cursors, Zstandard limits, schema drift, DAT round trips,
media bindings, sparse projections, saved-login CAS/replay and text/read
retry/recovery. Core tests separately exercise encrypted multi-file checkpoint
and accepted-credential restart journals. Unit tests do not prove stock-client
startup or another build/architecture.

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

`read_mmkv_identity` decodes only the inspected v4 framing with a caller-supplied 16-byte format key. It bounds sizes/records, verifies ciphertext CRC, applies overwrite/tombstone semantics and reads the exact username/UIN/device fields. Unknown versions and nonzero expiration/flags metadata are rejected. CRC is not authentication; the daemon compares stored identity against its independently authenticated account before enabling writers and rechecks it before each write. The global format key must be explicitly supplied; it is not discovered here.
Account `login_configv2` uses the separately derived account key. MMKV append
publication preserves unrelated records and the old ciphertext prefix, verifies
CRC/metadata generations and supports replay after data-first interruption. A
full data file grows to the next 4096-byte page, bounded at 4 MiB; unknown records
are never compacted or discarded. Exhausting that bound returns a resource error
and leaves the accepted generation pending for recovery. New-pair publication never
overwrites existing/partial foreign pairs. CRC is corruption detection, not
cryptographic authentication.

`stage_account_sync_key` targets only the account SyncKey store. Native SyncKey,
received/applied sidecar checkpoints, local pagination cursors and login cookies
are distinct. The daemon coordinator must block cursor advancement across any
unapplied intent, compare received generations and retire its encrypted journal
with its public commit event atomically.

Saved-login helpers preserve launch policy and unknown settings. Only the
explicit server-accepted staging API clears the desktop logout lock and may
clear a cookie. `plan_saved_key_info` records hash-only expected generations;
publication requires the known LoginKeyInfoTable schema/unique index and writes
the inspected native empty `key_md5`, while retaining unrelated/history rows.
Native latest-row selection and legacy fallback remain distinct. An unknown
authoritative schema is a pending compatibility gap, never permission to replace
it with a legacy file. SQLCipher keys, saved login tokens and active session
keys are not interchangeable.

A coordinator must bind the publication journal to an accepted credential
generation and logout epoch, recheck the fence before each durable file write,
then verify the stored result before retiring the journal. A logout can occur
between separate file commits; that window retains a visible pending journal,
not a claim of atomic rollback. Imported credentials alone are not acceptance
proof, and recovery does not authenticate or perform network requests.
