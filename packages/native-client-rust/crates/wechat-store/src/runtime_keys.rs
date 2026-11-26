//! Every kind of note an account keeps in its `runtime` table, with how long it
//! lives and how it grows. Retention reads its lifetimes from this list, and the
//! simulator soak test fails on any note that matches no kind here.
//!
//! A new kind of note needs an entry: its key shape, its retention, how it grows
//! and why it exists. A kind that grows with traffic and is never removed must
//! say so: [`Growth::Archive`] when that is intended, or [`Growth::Leak`] with
//! the issue that tracks removing it.
use crate::retention::{RuntimeRetention, RAW_DAYS, READ_NOTIFICATION, RECEIPT_DAYS};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeMap;

/// Longest runtime key [`crate::Store::put_runtime`] accepts. Every kind's
/// largest key, a message ID of up to 8 KiB, fits well within it; a key that
/// grows past it is a bug.
pub const MAX_RUNTIME_KEY: usize = 16 * 1024;

/// How the number of notes of one kind grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Growth {
    /// One note, or a fixed few, per account.
    Singleton,
    /// One note per contact, group, thread or other account entity.
    PerEntity,
    /// One note per unit of queued work, removed when the work finishes.
    PerJob,
    /// One note per message, command, batch or operation; retention removes it.
    PerTraffic,
    /// Grows with traffic and is removed by another cleanup, named in the reason.
    OtherCleanup,
    /// Grows with traffic and is kept on purpose; the reason names the decision.
    Archive,
    /// Grows with traffic and nothing removes it yet. The issue tracks the fix.
    Leak(&'static str),
}
impl Growth {
    pub fn label(self) -> &'static str {
        match self {
            Self::Singleton => "singleton",
            Self::PerEntity => "perEntity",
            Self::PerJob => "perJob",
            Self::PerTraffic => "perTraffic",
            Self::OtherCleanup => "otherCleanup",
            Self::Archive => "archive",
            Self::Leak(_) => "leak",
        }
    }
}

/// What follows a kind's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// Nothing: the key is the prefix.
    Empty,
    /// A lowercase SHA-256 digest in hex.
    Sha256,
    /// A SHA-256 digest followed by `:` and more.
    Sha256Then,
    /// Any text containing `:`.
    Suffixed,
    /// Any text without control characters.
    Text,
}
impl Tail {
    fn fits(self, tail: &str) -> bool {
        let hex =
            |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        match self {
            Self::Empty => tail.is_empty(),
            Self::Sha256 => hex(tail),
            Self::Sha256Then => {
                tail.len() > 65
                    && tail.get(..64).is_some_and(hex)
                    && tail.as_bytes().get(64) == Some(&b':')
            }
            Self::Suffixed => tail.contains(':') && !tail.chars().any(char::is_control),
            Self::Text => !tail.is_empty() && !tail.chars().any(char::is_control),
        }
    }
    fn placeholder(self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Sha256 => "<sha256>",
            Self::Sha256Then => "<sha256>:<…>",
            Self::Suffixed => "<…:…>",
            Self::Text => "<…>",
        }
    }
}

/// One kind of runtime note.
#[derive(Debug)]
pub struct RuntimeKind {
    pub prefix: &'static str,
    pub tail: Tail,
    /// Longest key of this kind, prefix included.
    pub max_key: usize,
    pub retention: RuntimeRetention,
    pub growth: Growth,
    /// What the note is for, and for kept kinds why it is kept.
    pub reason: &'static str,
}
impl RuntimeKind {
    /// The kind's key shape, such as `incoming-group-pending:<sha256>`.
    pub fn pattern(&self) -> String {
        format!("{}{}", self.prefix, self.tail.placeholder())
    }
}

const KEEP: RuntimeRetention = RuntimeRetention::Keep;
const R30: RuntimeRetention = RuntimeRetention::Remove(RECEIPT_DAYS);
const R7: RuntimeRetention = RuntimeRetention::Remove(RAW_DAYS);
/// A sync receipt, `projected:<batch>:<index>`, where a batch is a digest or
/// `contact:<digest>`, plus room for a short suffix.
const RECEIPT: usize = 192;
/// A receipt or operation ID, then a username of up to 256 characters.
const RECEIPT_USER: usize = 512;
/// A username of up to 256 characters.
const USER: usize = 320;
/// A message ID of up to 8 KiB, plus room for a suffix.
const MESSAGE: usize = 8 * 1024 + 192;
/// A thread ID of up to 512 bytes.
const THREAD: usize = 576;
/// A call ID of up to 200 characters, plus an ordinal.
const CALL: usize = 256;
/// An operation, job, metadata or asset ID.
const ID: usize = 128;
/// Up to 2 KiB of free text.
const TEXT: usize = 2048;

const fn kind(
    prefix: &'static str,
    tail: Tail,
    max_key: usize,
    retention: RuntimeRetention,
    growth: Growth,
    reason: &'static str,
) -> RuntimeKind {
    RuntimeKind {
        prefix,
        tail,
        max_key,
        retention,
        growth,
        reason,
    }
}
const fn singleton(prefix: &'static str, reason: &'static str) -> RuntimeKind {
    kind(
        prefix,
        Tail::Empty,
        prefix.len(),
        KEEP,
        Growth::Singleton,
        reason,
    )
}
/// A once-only receipt for an event about one message, command, operation or
/// minute; consulted only while that work runs.
const fn event(prefix: &'static str, max_key: usize) -> RuntimeKind {
    kind(
        prefix,
        Tail::Text,
        max_key,
        R30,
        Growth::PerTraffic,
        "Once-only event receipt.",
    )
}
const LEAK: Growth = Growth::Leak("#730");

/// Every kind of runtime note. Order doesn't matter: a key belongs to the kind
/// with the longest matching prefix whose tail fits.
pub static RUNTIME_KINDS: &[RuntimeKind] = &[
    // Account state, cursors and publication journals.
    singleton("account-state", "Sealed account credentials."),
    singleton("account-sync-store-create", "Sealed parameters for creating the sync store; removed once it exists."),
    singleton("auto-connect", "Whether the account connects when the daemon starts."),
    singleton("contact-cursor-publication", "Sealed contact cursor journal while it is published."),
    singleton("contact-projected-cursor", "Contact sync cursor already projected into the profile."),
    singleton("contact-projected-received-generation", "Generation of the projected contact cursor."),
    singleton("contact-received-cursor", "Contact sync cursor received from the server."),
    singleton("contact-search-receipt", "Sealed reply of the last contact search."),
    singleton("contact-sync-ready", "The contact sync store has been created."),
    singleton("contact-sync-store-create", "Sealed parameters for creating the contact sync store; removed once it exists."),
    singleton("cursor-projection-needed", "The received cursor still needs publishing to the profile."),
    singleton("cursor-publication", "Sealed message cursor journal while it is published."),
    singleton("desired", "Whether the account should be connected."),
    singleton("device", "Sealed device identity."),
    singleton("history-caught-up", "Catch-up after sign-in has finished."),
    singleton("logged-out", "The account was signed out."),
    singleton("logout-epoch", "Generation fence for sign-outs."),
    singleton("logout-pending", "A sign-out is still being submitted."),
    singleton("media-writeback-cursor", "Progress of writing media back into the profile."),
    singleton("metadata-contacts-cursor", "Contact catalog paging cursor."),
    singleton("metadata-groups-cursor", "Group catalog paging cursor."),
    singleton("metadata-contacts-unavailable", "Contacts the catalog could not return (at most 10,000 entries)."),
    singleton("native-profile-origin", "How the account's profile was created."),
    singleton("official-profile-selected", "The attached official profile."),
    singleton("profile-bootstrap", "Profile creation in progress."),
    singleton("profile-identity-binding", "Which profile identity this account owns."),
    singleton("profile-recovery-pending", "Profile recovery still has to run."),
    singleton("profile-write-binding", "The profile writer this account is bound to."),
    singleton("projected-cursor", "Message cursor already projected into the profile."),
    singleton("projected-cursor-hash", "Digest of the projected message cursor."),
    singleton("projected-received-generation", "Generation of the projected message cursor."),
    singleton("received-cursor", "Message sync cursor received from the server."),
    singleton("saved-login-accepted-state-generation", "Accepted saved-login generation."),
    singleton("saved-login-projected-generation", "Saved-login generation projected into the profile."),
    singleton("saved-login-projected-state-generation", "Saved-login state generation projected into the profile."),
    singleton("saved-login-projection-needed", "Saved-login state still needs projecting."),
    singleton("saved-login-publication", "Sealed saved-login journal while it is published."),
    singleton("sticker-pack-projection-latest", "Latest projected sticker pack catalog."),
    singleton("sticker-projection-latest", "Latest projected sticker catalog."),
    singleton("sticker-refresh-job", "The sticker refresh in progress."),
    singleton("validated-generation", "Credential generation the server last validated."),
    // Sync command receipts and their phase markers.
    kind("projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Receipt for one applied sync command."),
    kind("apply-origin:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Live or catch-up label for a sync batch."),
    kind("blocked-control:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Blocked-command marker from earlier releases; nothing writes it now."),
    kind("contact-fields:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a contact command applies."),
    kind("contact-mute:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a contact command applies."),
    kind("contact-pin:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a contact command applies."),
    kind("partial-thread-entry-read:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a read control applies."),
    kind("partial-thread-exit-read:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a read control applies."),
    kind("friend-message-stage:projected:", Tail::Text, RECEIPT, R30, Growth::PerTraffic, "Phase marker while a friend-request message applies."),
    kind("friend-attention-stage:projected:", Tail::Text, RECEIPT + 72, R30, Growth::PerTraffic, "Phase marker while a friend-request attention flag applies."),
    kind("friend-attention-stage:", Tail::Text, ID + 72, KEEP, LEAK, "Phase marker for an attention flag applied from an operation or metadata receipt."),
    kind("contact-notifications:", Tail::Text, RECEIPT_USER, KEEP, LEAK, "Phase marker while a contact record's notification setting applies."),
    kind("contact-pinning:", Tail::Text, RECEIPT_USER, KEEP, LEAK, "Phase marker while a contact record's pin applies."),
    kind("contact-pinning-time:", Tail::Text, RECEIPT, KEEP, LEAK, "Phase marker while a record's pin time applies."),
    kind("contact-privacy-observation:", Tail::Text, RECEIPT_USER, KEEP, LEAK, "Privacy setting observed in one contact record."),
    kind("sync-control:", Tail::Text, RECEIPT, KEEP, Growth::Archive, "Sealed raw bytes of a control command, kept so a later build can apply what this one skipped (owner decision 2026-11-19)."),
    kind("skipped-control:", Tail::Text, RECEIPT, KEEP, Growth::Archive, "Kind of a skipped command, kept as long as its raw bytes under sync-control: (owner decision 2026-11-19)."),
    kind("recall-control:", Tail::Sha256, 15 + 64, KEEP, Growth::Archive, "Evidence of one incoming recall, kept with its raw command."),
    // Per-contact, per-group and per-thread state.
    kind("contact-notifications-unresolved:", Tail::Text, USER, KEEP, Growth::PerEntity, "A contact's notification setting is still unknown."),
    kind("contact-pinning-unresolved:", Tail::Text, USER, KEEP, Growth::PerEntity, "A contact's pin is still unknown."),
    kind("contact-privacy-revision:", Tail::Text, USER, KEEP, Growth::PerEntity, "Latest privacy revision seen for a contact."),
    kind("contact-privacy-unresolved:", Tail::Text, USER, KEEP, Growth::PerEntity, "A contact's privacy setting is still unknown."),
    kind("group-detail-version:", Tail::Sha256, 21 + 64, KEEP, Growth::PerEntity, "Announcement version projected for a group."),
    kind("group-revision:", Tail::Sha256, 15 + 64, KEEP, Growth::PerEntity, "Last change applied to a contact or group, to refuse older ones."),
    kind("group-roster-version:", Tail::Sha256, 21 + 64, KEEP, Growth::PerEntity, "Member list version projected for a group."),
    kind("read-version:", Tail::Text, THREAD, KEEP, Growth::PerEntity, "A thread's read version."),
    // Queued work, removed when it finishes.
    kind("big-xml-expand:", Tail::Sha256, 15 + 64, KEEP, Growth::PerJob, "Sealed BigXml expansion job."),
    kind("contact-remark-refresh:", Tail::Sha256, 23 + 64, KEEP, Growth::PerJob, "Queued contact remark lookup."),
    kind("incoming-group-pending:", Tail::Sha256, 23 + 64, KEEP, Growth::PerJob, "Sealed group refresh job."),
    kind("metadata-hydrated:", Tail::Text, ID, KEEP, Growth::PerJob, "Catalog hydration in progress for a metadata receipt."),
    kind("metadata-pending:", Tail::Text, ID, KEEP, Growth::PerJob, "Metadata receipt still to apply."),
    kind("read-pending:", Tail::Text, THREAD, KEEP, Growth::PerJob, "Read state still to submit for a thread."),
    kind("received-media:", Tail::Text, ID, KEEP, Growth::PerJob, "Automatic download queued for a received attachment."),
    kind("received-voice:", Tail::Text, ID, KEEP, Growth::PerJob, "Automatic download queued for a received voice note."),
    kind("sticker-refresh:", Tail::Text, 16 + 64, KEEP, Growth::PerJob, "Queued sticker catalog refresh."),
    kind("streaming-voice:", Tail::Text, MESSAGE, KEEP, Growth::PerJob, "Voice note still streaming in (90 seconds or less)."),
    // Group refresh results.
    kind("incoming-group-done:", Tail::Sha256, 20 + 64, KEEP, LEAK, "A group refresh for this receipt has finished."),
    kind("incoming-group-held:", Tail::Sha256, 20 + 64, KEEP, LEAK, "Sealed group refresh that failed three times."),
    kind("incoming-group-pending:", Tail::Suffixed, MAX_RUNTIME_KEY, R7, Growth::PerTraffic, "Superseded refresh copy that earlier releases saved under the queue's names (#722)."),
    kind("incoming-group-held:", Tail::Suffixed, MAX_RUNTIME_KEY, R7, Growth::PerTraffic, "Failed superseded refresh copy from earlier releases (#722)."),
    kind("incoming-group-receipt:", Tail::Sha256, 23 + 64, R7, Growth::PerTraffic, "Sealed response bodies of a finished group refresh, kept for investigation."),
    kind("incoming-group-receipt:", Tail::Suffixed, MAX_RUNTIME_KEY, R7, Growth::PerTraffic, "Receipt of a superseded refresh copy that earlier releases ran as a job (#722)."),
    kind("incoming-group-superseded:", Tail::Sha256Then, 26 + 64 + 4, R7, Growth::PerTraffic, "Sealed response bodies a refresh replaced, kept for investigation."),
    // Metadata, media and send receipts.
    kind("metadata-receipt:", Tail::Text, ID, RuntimeRetention::RemoveFinishedMetadata, Growth::PerTraffic, "Sealed metadata reply until it has been applied."),
    kind("metadata-unavailable:", Tail::Text, ID, R7, Growth::PerTraffic, "Catalog receipt that returned nothing usable."),
    kind("metadata-wire:", Tail::Text, ID, R7, Growth::PerTraffic, "Raw metadata reply, kept for investigation."),
    kind("job-event:", Tail::Text, ID, R30, Growth::PerTraffic, "Newest progress event of a media job."),
    kind("send-projected:", Tail::Text, ID, KEEP, Growth::OtherCleanup, "A text send is projected; removed with its send after 90 days."),
    kind("send-projection:", Tail::Text, ID, KEEP, Growth::OtherCleanup, "Profile write receipt for an accepted send; removed with its send after 90 days."),
    kind("media-projection:", Tail::Text, ID, KEEP, Growth::OtherCleanup, "Profile write receipt for an accepted media send; removed with its job after 90 days."),
    kind("sticker-send:", Tail::Text, ID, KEEP, Growth::OtherCleanup, "Profile write receipt for an accepted sticker send; removed with its job after 90 days."),
    kind("thread-read-notification:", Tail::Sha256, 25 + 64, RuntimeRetention::Retire(READ_NOTIFICATION, RECEIPT_DAYS), Growth::PerTraffic, "A read notification was submitted; its key is retired so it is never sent twice."),
    kind("retention-day:", Tail::Text, 32, KEEP, Growth::OtherCleanup, "Row-ID marker for one day; retention removes markers after 400 days."),
    // Sign-in.
    kind("login-key:", Tail::Sha256, 10 + 64, KEEP, LEAK, "Idempotency key of a login request."),
    kind("qr:", Tail::Text, ID, KEEP, LEAK, "Sealed QR image of a login; removed when credentials are forgotten."),
    kind("saved-login-superseded:", Tail::Sha256, 23 + 64, KEEP, LEAK, "Sealed saved-login journal replaced before it finished."),
    // Calls, kept for diagnostics.
    kind("call-conference-failure:", Tail::Text, CALL, R7, Growth::PerTraffic, "Conference failure details for a call."),
    kind("call-conference-phase:", Tail::Text, CALL, R7, Growth::PerTraffic, "Conference phase reached by a call."),
    kind("call-conference-profile:", Tail::Text, CALL, R7, Growth::PerTraffic, "Conference media profile of a call."),
    kind("call-conference-receive:", Tail::Text, CALL, R7, Growth::PerTraffic, "Conference receive statistics of a call."),
    kind("call-host-diagnostic:", Tail::Text, CALL, R7, Growth::PerTraffic, "Host diagnostics for one call action."),
    kind("call-media-diagnostic:", Tail::Text, CALL, R7, Growth::PerTraffic, "Media diagnostics of a call."),
    kind("call-media-error:", Tail::Text, CALL, R7, Growth::PerTraffic, "Media error of a call."),
    // Once-only event receipts.
    event("event:auto-media-job:", 22 + ID),
    event("event:auto-media-unavailable:", MESSAGE),
    event("event:blocked:", 14 + RECEIPT),
    event("event:blocked-v2:", 17 + RECEIPT),
    event("event:contact:", 14 + RECEIPT),
    event("event:contact-delete:", 21 + RECEIPT),
    event("event:contact-mutation:", 23 + ID),
    event("event:contact-mutation-event:", 29 + ID),
    event("event:contact-pin:", 18 + RECEIPT_USER),
    event("event:contact-pin-event:", 24 + RECEIPT),
    event("event:control-no-local-effect:", 30 + RECEIPT),
    event("event:control-observed:", 23 + RECEIPT),
    event("event:control-skipped:", 22 + RECEIPT),
    event("event:friend-attention:", 23 + RECEIPT + 72),
    event("event:friend-message-event:", 27 + RECEIPT),
    event("event:group-detail:", 19 + ID),
    event("event:group-fields:", 19 + RECEIPT),
    event("event:group-members:", 20 + ID),
    event("event:group-mutation:", 21 + ID),
    event("event:group-refresh-held:", 25 + RECEIPT),
    event("event:group-refreshed:", 22 + RECEIPT),
    event("event:message:", MESSAGE),
    event("event:message-expanded:", MESSAGE),
    event("event:partial-thread-entry-read:", 32 + RECEIPT),
    event("event:partial-thread-exit-read:", 31 + RECEIPT),
    event("event:push:", 64),
    event("event:read-thread:", 18 + 64),
    event("event:received:", 15 + RECEIPT),
    event("event:remote-thread-read:", 25 + RECEIPT),
    event("event:thread-pin:", 17 + ID),
    event("event:voice-updated:", MESSAGE + 72),
    event("event:worker:", 128),
    singleton("event:profile-recovery-pending", "Once-only event: profile recovery is pending."),
    kind("event:profile-wal-conversion:", Tail::Sha256, 29 + 64, KEEP, Growth::Singleton, "Once-only event per distinct profile WAL conversion report; a few per account."),
    kind("event:profile-wal-conversion-skipped:", Tail::Text, TEXT, KEEP, LEAK, "Once-only event per distinct error text of a skipped WAL conversion; the text is unbounded."),
];

/// Where a key belongs.
#[derive(Debug)]
pub enum RuntimeLookup {
    Kind(&'static RuntimeKind),
    /// A kind's prefix matched, but no matching kind's tail fits.
    Malformed(&'static RuntimeKind),
    Unknown,
}

/// The kind a key belongs to: the longest matching prefix whose tail fits.
pub fn runtime_kind(key: &str) -> RuntimeLookup {
    // The longest matching prefix decides, as prefixes alone did before this
    // list. Among kinds sharing it, the first whose tail fits; otherwise the
    // first listed, reported as malformed.
    let Some(longest) = RUNTIME_KINDS
        .iter()
        .filter(|kind| key.starts_with(kind.prefix))
        .map(|kind| kind.prefix.len())
        .max()
    else {
        return RuntimeLookup::Unknown;
    };
    let mut candidates = RUNTIME_KINDS
        .iter()
        .filter(|kind| kind.prefix.len() == longest && key.starts_with(kind.prefix));
    let first = candidates
        .clone()
        .next()
        .expect("a kind has the longest prefix");
    match candidates.find(|kind| kind.tail.fits(&key[longest..])) {
        Some(kind) => RuntimeLookup::Kind(kind),
        None => RuntimeLookup::Malformed(first),
    }
}

/// Notes of one kind in an account's runtime table.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindUsage {
    pub rows: u64,
    pub bytes: u64,
    pub max_key: usize,
    pub growth: &'static str,
    pub retention: String,
}

/// What an account's runtime table holds, by kind.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAudit {
    pub rows: u64,
    pub bytes: u64,
    pub max_key: usize,
    /// Keys longer than their kind allows.
    pub oversized_rows: u64,
    pub oversized_samples: Vec<String>,
    /// Keys whose prefix matches a kind but whose tail doesn't fit it.
    pub malformed_rows: u64,
    pub malformed_samples: Vec<String>,
    /// Keys of no kind.
    pub unknown_rows: u64,
    pub unknown_samples: Vec<String>,
    /// Keyed by [`RuntimeKind::pattern`].
    pub families: BTreeMap<String, KindUsage>,
}

/// Groups an account's runtime notes by kind. Reads only.
pub fn audit_runtime(db: &Connection) -> Result<RuntimeAudit> {
    fn sample(samples: &mut Vec<String>, key: &str) {
        if samples.len() < 20 {
            samples.push(key.chars().take(160).collect());
        }
    }
    let mut audit = RuntimeAudit::default();
    // By kind address; named by pattern once at the end.
    let mut families: BTreeMap<usize, (&'static RuntimeKind, KindUsage)> = BTreeMap::new();
    let mut query = db.prepare("SELECT key,length(value) FROM runtime")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let bytes = key.len() as u64 + row.get::<_, Option<u64>>(1)?.unwrap_or(0);
        audit.rows += 1;
        audit.bytes += bytes;
        audit.max_key = audit.max_key.max(key.len());
        match runtime_kind(&key) {
            RuntimeLookup::Kind(kind) => {
                if key.len() > kind.max_key {
                    audit.oversized_rows += 1;
                    sample(&mut audit.oversized_samples, &key);
                }
                let (_, usage) = families
                    .entry(kind as *const RuntimeKind as usize)
                    .or_insert_with(|| {
                        let usage = KindUsage {
                            rows: 0,
                            bytes: 0,
                            max_key: 0,
                            growth: kind.growth.label(),
                            retention: format!("{:?}", kind.retention),
                        };
                        (kind, usage)
                    });
                usage.rows += 1;
                usage.bytes += bytes;
                usage.max_key = usage.max_key.max(key.len());
            }
            RuntimeLookup::Malformed(_) => {
                audit.malformed_rows += 1;
                sample(&mut audit.malformed_samples, &key);
            }
            RuntimeLookup::Unknown => {
                audit.unknown_rows += 1;
                sample(&mut audit.unknown_samples, &key);
            }
        }
    }
    audit.families = families
        .into_values()
        .map(|(kind, usage)| (kind.pattern(), usage))
        .collect();
    Ok(audit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: &RuntimeKind) -> String {
        let hex = "0123456789abcdef".repeat(4);
        let tail = match kind.tail {
            Tail::Empty => String::new(),
            Tail::Sha256 => hex,
            Tail::Sha256Then => format!("{hex}:1"),
            Tail::Suffixed => format!("{hex}:superseded:1"),
            Tail::Text => "sample".into(),
        };
        format!("{}{tail}", kind.prefix)
    }

    #[test]
    fn every_kind_owns_its_own_keys() {
        for kind in RUNTIME_KINDS {
            let key = sample(kind);
            match runtime_kind(&key) {
                RuntimeLookup::Kind(found) => assert!(
                    std::ptr::eq(found, kind),
                    "{key} belongs to {}",
                    found.pattern()
                ),
                other => panic!("{key}: {other:?}"),
            }
            assert!(key.len() <= kind.max_key, "{key} exceeds its own limit");
            assert!(kind.max_key <= MAX_RUNTIME_KEY, "{}", kind.pattern());
            assert!(!kind.reason.is_empty(), "{}", kind.pattern());
        }
    }

    #[test]
    fn kinds_are_listed_once() {
        let mut seen = std::collections::BTreeSet::new();
        for kind in RUNTIME_KINDS {
            assert!(
                seen.insert(kind.pattern()),
                "{} listed twice",
                kind.pattern()
            );
        }
    }

    #[test]
    fn notes_that_grow_with_traffic_have_a_way_out() {
        for kind in RUNTIME_KINDS {
            match kind.growth {
                // Retention removes these, so their lifetime must be a window it uses.
                Growth::PerTraffic => assert!(
                    matches!(
                        kind.retention,
                        RuntimeRetention::Remove(RAW_DAYS | RECEIPT_DAYS)
                            | RuntimeRetention::Retire(_, RECEIPT_DAYS)
                            | RuntimeRetention::RemoveFinishedMetadata
                    ),
                    "{} grows with traffic but retention keeps it",
                    kind.pattern()
                ),
                Growth::Leak(issue) => {
                    assert!(issue.starts_with('#'), "{}", kind.pattern());
                    assert_eq!(kind.retention, RuntimeRetention::Keep, "{}", kind.pattern());
                }
                Growth::OtherCleanup | Growth::Archive | Growth::PerJob => {
                    assert_eq!(kind.retention, RuntimeRetention::Keep, "{}", kind.pattern())
                }
                Growth::Singleton | Growth::PerEntity => {}
            }
        }
    }

    #[test]
    fn keys_of_no_kind_or_the_wrong_shape_are_reported() {
        assert!(matches!(
            runtime_kind("not-a-kind:1"),
            RuntimeLookup::Unknown
        ));
        assert!(matches!(
            runtime_kind("incoming-group-done:x"),
            RuntimeLookup::Malformed(_)
        ));
        assert!(matches!(
            runtime_kind("logout-epochs"),
            RuntimeLookup::Malformed(_)
        ));
        assert!(
            matches!(runtime_kind("projected-cursor-hash"), RuntimeLookup::Kind(k) if k.prefix == "projected-cursor-hash")
        );
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE runtime(key TEXT PRIMARY KEY, value BLOB)")
            .unwrap();
        let hex = "ab".repeat(32);
        let chained = format!(
            "incoming-group-receipt:{hex}{}",
            ":superseded:255".repeat(10)
        );
        for key in [
            format!("incoming-group-pending:{hex}"),
            format!("incoming-group-done:{hex}"),
            "projected:batch:1".into(),
            "mystery".into(),
            "incoming-group-done:short".into(),
            chained,
            format!("read-version:{}", "t".repeat(THREAD)),
        ] {
            db.execute("INSERT INTO runtime VALUES(?1, x'00')", [key])
                .unwrap();
        }
        let audit = audit_runtime(&db).unwrap();
        assert_eq!(audit.rows, 7);
        assert_eq!(audit.unknown_rows, 1);
        // The chained receipt is a registered legacy shape.
        assert_eq!(audit.malformed_rows, 1, "{:?}", audit.malformed_samples);
        assert_eq!(audit.families["incoming-group-receipt:<…:…>"].rows, 1);
        assert_eq!(audit.oversized_rows, 1, "{:?}", audit.oversized_samples);
        assert_eq!(
            audit.families["incoming-group-pending:<sha256>"].growth,
            "perJob"
        );
        assert_eq!(
            audit.families["incoming-group-done:<sha256>"].growth,
            "leak"
        );
    }

    /// The prefix lists retention used before this list existed.
    fn legacy_group_refresh_copy(key: &str) -> bool {
        ["incoming-group-pending:", "incoming-group-held:"]
            .iter()
            .filter_map(|prefix| key.strip_prefix(prefix))
            .any(|rest| rest.contains(':'))
    }

    fn previous_aged_runtime(key: &str) -> RuntimeRetention {
        // Per-command projection receipts and their phase markers.
        const COMMAND: [&str; 11] = [
            "projected:",
            "blocked-control:",
            // The newest job.updated event of a job, for merging progress.
            "job-event:",
            "apply-origin:",
            "contact-fields:projected:",
            "contact-mute:projected:",
            "contact-pin:projected:",
            "partial-thread-entry-read:projected:",
            "partial-thread-exit-read:projected:",
            "friend-message-stage:projected:",
            "friend-attention-stage:projected:",
        ];
        // Once-only event receipts for one message, command, operation or minute.
        const EVENT: [&str; 32] = [
            "message:",
            "message-expanded:",
            "voice-updated:",
            "remote-thread-read:",
            "control-observed:",
            "control-no-local-effect:",
            "control-skipped:",
            "contact:",
            "contact-pin-event:",
            "contact-pin:",
            "contact-delete:",
            "group-fields:",
            "friend-message-event:",
            "friend-attention:",
            "partial-thread-entry-read:",
            "partial-thread-exit-read:",
            "group-refreshed:",
            "group-refresh-held:",
            "received:",
            "blocked-v2:",
            "blocked:",
            "group-members:",
            "group-detail:",
            "read-thread:",
            "contact-mutation-event:",
            "contact-mutation:",
            "thread-pin:",
            "group-mutation:",
            "auto-media-job:",
            "auto-media-unavailable:",
            "worker:",
            "push:",
        ];
        // Raw replies kept for investigation and call diagnostics. No code reads
        // them back.
        const RAW: [&str; 11] = [
            "metadata-wire:",
            "metadata-unavailable:",
            "incoming-group-receipt:",
            "incoming-group-superseded:",
            "call-host-diagnostic:",
            "call-media-error:",
            "call-media-diagnostic:",
            "call-conference-receive:",
            "call-conference-failure:",
            "call-conference-phase:",
            "call-conference-profile:",
        ];
        if let Some(event) = key.strip_prefix("event:") {
            return if EVENT.iter().any(|prefix| event.starts_with(prefix)) {
                RuntimeRetention::Remove(RECEIPT_DAYS)
            } else {
                RuntimeRetention::Keep
            };
        }
        if COMMAND.iter().any(|p| key.starts_with(p)) {
            RuntimeRetention::Remove(RECEIPT_DAYS)
        } else if RAW.iter().any(|p| key.starts_with(p)) || legacy_group_refresh_copy(key) {
            RuntimeRetention::Remove(RAW_DAYS)
        } else if key.starts_with("metadata-receipt:") {
            RuntimeRetention::RemoveFinishedMetadata
        } else if key.starts_with("thread-read-notification:") {
            RuntimeRetention::Retire(READ_NOTIFICATION, RECEIPT_DAYS)
        } else {
            RuntimeRetention::Keep
        }
    }

    #[test]
    fn every_kind_keeps_the_lifetime_it_had_before_this_list() {
        for kind in RUNTIME_KINDS {
            let well_formed = sample(kind);
            // A tail that doesn't fit its kind keeps its prefix's old lifetime.
            for key in [
                well_formed.clone(),
                kind.prefix.to_owned(),
                format!("{}x", kind.prefix),
                format!("{well_formed}:extra"),
                format!("{}\u{1}", kind.prefix),
                // Multi-byte text across byte 64 of the tail.
                format!("{}{}", kind.prefix, "\u{4e2d}".repeat(30)),
            ] {
                assert_eq!(
                    crate::retention::aged_runtime(&key),
                    previous_aged_runtime(&key),
                    "{key:?}"
                );
            }
        }
    }
}
