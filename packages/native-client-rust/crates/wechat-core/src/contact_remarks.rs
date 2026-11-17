//! Personal remarks arrive with per-field controls (field 89) whose stored-version
//! merge is not implemented. A pushed update whose only controlled fields are the
//! remark fields is therefore not applied from its own copy, which could be older
//! than the saved value, and is not held either: it queues an exact lookup of the
//! contact, and the lookup reply supplies the service's current remark.
//!
//! The queue is operational state only (one sealed record per contact, removed
//! once the lookup settles); the remark itself is saved in the official profile.
use super::*;

/// Retry delays grow from 30 seconds to an hour; a refresh is dropped after this
/// many failed attempts. A later change or a manual refresh queues it again.
const MAX_ATTEMPTS: u8 = 8;

#[derive(Serialize, Deserialize)]
struct RemarkRefresh {
    contact: String,
    source: String,
    epoch: Option<Vec<u8>>,
    attempts: u8,
    #[serde(default)]
    not_before: i64,
}

fn key(contact: &str) -> String {
    format!(
        "contact-remark-refresh:{:x}",
        Sha256::digest(contact.as_bytes())
    )
}

/// Whether every controlled field is a personal remark field.
pub(super) fn remark_only_controls(controlled: &[u32]) -> bool {
    !controlled.is_empty()
        && controlled
            .iter()
            .all(|id| metadata::REMARK_FIELDS.iter().any(|(n, _)| n == id))
}

impl Core {
    /// Queue (or re-queue) an exact lookup for a contact whose remark changed.
    /// Repeated changes to one contact share a single queued lookup.
    pub(super) fn queue_remark_refresh(
        &self,
        account: &str,
        contact: &str,
        source: &str,
    ) -> Result<()> {
        let key = key(contact);
        let job = RemarkRefresh {
            contact: contact.to_owned(),
            source: source.to_owned(),
            epoch: self.store.runtime(account, "logout-epoch")?,
            attempts: 0,
            not_before: 0,
        };
        self.store.put_runtime(
            account,
            &key,
            &self.vault.seal(account, &key, &serde_json::to_vec(&job)?)?,
        )
    }

    /// Run due remark lookups. Local repair passes `allow_fetch = false` and does
    /// nothing; the normal worker fetches. Network failures leave the lookup
    /// queued with a delay and stop this pass.
    pub(super) fn refresh_contact_remarks(
        &self,
        account: &str,
        allow_fetch: bool,
    ) -> Result<usize> {
        if !allow_fetch {
            return Ok(0);
        }
        let now = chrono::Utc::now().timestamp();
        let mut settled = 0;
        for (key, sealed) in self.store.contact_remark_refreshes(account)? {
            let mut job: RemarkRefresh =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            if job.epoch != self.store.runtime(account, "logout-epoch")? {
                // Queued under an earlier authorization; never use it after logout.
                self.store.clear_runtime(account, &key)?;
                continue;
            }
            if job.not_before > now {
                continue;
            }
            let source = job.source.clone();
            match self.refresh_contact_snapshot(account, &job.contact) {
                Ok(true) => {
                    self.clear_remark_refresh(account, &key, &source)?;
                    settled += 1;
                }
                Ok(false) | Err(_) => {
                    job.attempts = job.attempts.saturating_add(1);
                    if job.attempts >= MAX_ATTEMPTS {
                        self.clear_remark_refresh(account, &key, &source)?;
                    } else {
                        job.not_before = now + (30i64 << (job.attempts - 1)).min(3600);
                        self.store.put_runtime(
                            account,
                            &key,
                            &self.vault.seal(account, &key, &serde_json::to_vec(&job)?)?,
                        )?;
                    }
                    break;
                }
            }
        }
        Ok(settled)
    }

    /// Remove a queued lookup unless a newer change re-queued it meanwhile.
    fn clear_remark_refresh(&self, account: &str, key: &str, source: &str) -> Result<()> {
        if let Some(sealed) = self.store.runtime(account, key)? {
            let current: RemarkRefresh =
                serde_json::from_slice(&self.vault.open_record(account, key, &sealed)?)?;
            if current.source == source {
                self.store.clear_runtime(account, key)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::{protobuf as p, wire::SyncCommand};

    /// A pushed or looked-up `ModContact` for "peer" carrying only names, with
    /// field 89 controlling the remark fields (scope 1, the given version).
    fn record(remark: &str, version: u64, controls: &[u64]) -> Vec<u8> {
        let mut fields = vec![
            p::bytes(1, &p::bytes(1, b"peer")),
            p::bytes(2, &p::bytes(1, b"Peer nickname")),
        ];
        for n in [10, 11, 12] {
            fields.push(p::bytes(n, &p::bytes(1, remark.as_bytes())));
        }
        let mut control = vec![
            p::uint(1, 1),
            p::uint(2, version),
            p::uint(3, controls.len() as u64),
        ];
        for id in controls {
            control.push(p::bytes(4, &p::uint(1, *id)));
        }
        let control = p::message(&control).unwrap();
        fields.push(p::bytes(
            89,
            &p::message(&[p::uint(1, 1), p::bytes(2, &control)]).unwrap(),
        ));
        p::message(&fields).unwrap()
    }
    fn push(remark: &str, version: u64, controls: &[u64]) -> SyncCommand {
        SyncCommand {
            id: 2,
            payload: record(remark, version, controls),
        }
    }
    struct CurrentRemark(&'static str);
    impl crate::upstream::Upstream for CurrentRemark {
        fn kind(&self) -> crate::upstream::UpstreamKind {
            crate::upstream::UpstreamKind::Native
        }
        fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
            assert_eq!(ids, ["peer"]);
            p::message(&[
                p::bytes(1, &p::int32(1, 0)),
                p::uint(2, 1),
                p::bytes(3, &record(self.0, 13, &[10, 11, 12])),
                p::uint(4, 0),
            ])
        }
    }
    fn saved_remark(core: &Core) -> Option<String> {
        core.profile_reader_owned("one")
            .unwrap()
            .contact("peer")
            .unwrap()
            .remark
    }
    fn queued(core: &Core) -> usize {
        core.store.contact_remark_refreshes("one").unwrap().len()
    }

    #[test]
    fn controlled_remark_updates_queue_one_lookup_that_saves_the_current_remark() {
        let (_temp, mut core) = super::super::groups::tests::fixture();
        // Two phone edits (A -> B -> A) arrive before the lookup runs. Neither
        // pushed copy is written, the batch is not held, and one lookup is queued.
        for (receipt, remark, version) in [("edit-1", "Name B", 12), ("edit-2", "Name A", 13)] {
            core.apply_sync_command(
                "one",
                &push(remark, version, &[10, 11, 12]),
                receipt,
                "live",
            )
            .unwrap();
            assert!(core.store.runtime("one", receipt).unwrap().is_some());
        }
        assert_eq!(saved_remark(&core), None);
        assert_eq!(queued(&core), 1);

        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(CurrentRemark("Name A"));
        assert_eq!(core.refresh_contact_remarks("one", false).unwrap(), 0);
        assert_eq!(queued(&core), 1);
        assert_eq!(core.refresh_contact_remarks("one", true).unwrap(), 1);
        assert_eq!(saved_remark(&core).as_deref(), Some("Name A"));
        assert_eq!(queued(&core), 0);
    }

    #[test]
    fn a_lookup_queued_before_logout_is_dropped_without_fetching() {
        let (_temp, mut core) = super::super::groups::tests::fixture();
        core.apply_sync_command("one", &push("Name B", 12, &[10, 11, 12]), "edit", "live")
            .unwrap();
        assert_eq!(queued(&core), 1);
        core.store
            .put_runtime("one", "logout-epoch", b"later-generation")
            .unwrap();
        struct NoLookup;
        impl crate::upstream::Upstream for NoLookup {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
        }
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(NoLookup);
        assert_eq!(core.refresh_contact_remarks("one", true).unwrap(), 0);
        assert_eq!(queued(&core), 0);
        assert_eq!(saved_remark(&core), None);
    }

    #[test]
    fn updates_controlling_other_fields_stay_held() {
        let (_temp, core) = super::super::groups::tests::fixture();
        let error = core
            .apply_sync_command("one", &push("Name B", 12, &[8, 10]), "edit", "live")
            .unwrap_err();
        assert_eq!(error.to_string(), "contact_update_control_unresolved");
        assert!(core.store.runtime("one", "edit").unwrap().is_none());
        assert_eq!(queued(&core), 0);
    }

    #[test]
    fn remark_only_controls_require_every_id_to_be_a_remark_field() {
        assert!(remark_only_controls(&[10, 11, 12]));
        assert!(remark_only_controls(&[10]));
        assert!(!remark_only_controls(&[]));
        assert!(!remark_only_controls(&[8, 10]));
        assert!(!remark_only_controls(&[27]));
    }
}
