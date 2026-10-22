//! Copy receipts describe official files separately from native recovery state.
use super::*;
use sha2::{Digest, Sha256};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionState {
    pub pending_projection_batches: usize,
    pub pending_read_projections: usize,
    pub pending_recall_projections: usize,
    pub pending_message_recalls: usize,
    pub pending_cursor_projection: bool,
    pub pending_saved_login_projection: bool,
    pub pending_metadata_projections: bool,
    pub pending_text_projections: bool,
    pub pending_media_projections: bool,
    pub incomplete_media_writeback: bool,
    pub sync_checkpoint_projected: bool,
    pub sync_checkpoint_evidence: &'static str,
}
impl ProjectionState {
    pub fn read(store: &wechat_store::Store, account: &str) -> Result<Self> {
        let received = store.runtime(account, "received-cursor")?;
        let generation = received
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        let same_generation = store
            .runtime(account, "projected-received-generation")?
            .as_deref()
            == generation.as_deref().map(str::as_bytes);
        let projected = store
            .runtime(account, "projected-cursor")?
            .is_some_and(|bytes| !bytes.is_empty());
        let hash = store
            .runtime(account, "projected-cursor-hash")?
            .is_some_and(|bytes| bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit));
        let recorded = received.is_some() && same_generation && projected && hash;
        let pending_projection_batches = store.pending_sync_count(account)?;
        let pending_recall_projections = store.pending_recall_projections(account)?;
        let pending_message_recalls = store.pending_message_recalls(account)?;
        // An older received cursor without a publication receipt cannot prove
        // that its ordinary files and official cursor reached the same barrier.
        let pending_cursor_projection =
            store.pending_cursor_projection(account)? || (received.is_some() && !recorded);
        let sync_checkpoint_projected = recorded
            && !pending_cursor_projection
            && pending_projection_batches == 0
            && pending_recall_projections == 0
            && pending_message_recalls == 0;
        let accepted_state = store.runtime(account, "saved-login-accepted-state-generation")?;
        let projected_state = store.runtime(account, "saved-login-projected-state-generation")?;
        let actual_state = store.runtime(account, "account-state")?;
        let actual_generation = actual_state
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        let pending_saved_login_projection = store
            .runtime(account, "saved-login-projection-needed")?
            .is_some()
            || store.runtime(account, "saved-login-publication")?.is_some()
            || accepted_state.as_ref().is_some_and(|accepted| {
                projected_state.as_ref() != Some(accepted)
                    || actual_generation.as_deref().map(str::as_bytes) != Some(accepted.as_slice())
            });
        Ok(Self {
            pending_projection_batches,
            pending_read_projections: store.pending_reads(account)?,
            pending_recall_projections,
            pending_message_recalls,
            pending_cursor_projection,
            pending_saved_login_projection,
            pending_metadata_projections: !store.pending_metadata(account)?.is_empty(),
            pending_text_projections: !store.pending_text_projections(account)?.is_empty(),
            pending_media_projections: !store.pending_media_projections(account)?.is_empty(),
            incomplete_media_writeback: store.incomplete_media_writeback(account)?,
            sync_checkpoint_projected,
            sync_checkpoint_evidence: if sync_checkpoint_projected {
                "durable_projection_receipt"
            } else if pending_cursor_projection
                || pending_projection_batches != 0
                || pending_recall_projections != 0
                || pending_message_recalls != 0
            {
                "pending"
            } else {
                "unverified"
            },
        })
    }
    pub fn pending(&self) -> bool {
        self.pending_projection_batches != 0
            || self.pending_read_projections != 0
            || self.pending_recall_projections != 0
            || self.pending_message_recalls != 0
            || self.pending_cursor_projection
            || self.pending_saved_login_projection
            || self.pending_metadata_projections
            || self.pending_text_projections
            || self.pending_media_projections
            || self.incomplete_media_writeback
    }
    pub fn require_copy_permission(&self, allow_incomplete: bool) -> Result<()> {
        if self.pending() && !allow_incomplete {
            return Err(failure(
                9,
                "profile_projection_pending",
                "official profile updates remain pending; --allow-incomplete copies only current official files",
            ));
        }
        Ok(())
    }
    pub fn receipt(&self, entries: usize) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        let object = value.as_object_mut().context("invalid export receipt")?;
        object.insert("state".into(), json!("copied"));
        object.insert("entries".into(), json!(entries));
        object.insert("incompleteCopy".into(), json!(self.pending()));
        object.insert("daemonSidecarIncluded".into(), json!(false));
        object.insert("daemonActiveCredentialsIncluded".into(), json!(false));
        object.insert("officialLoginMaterialMayBeIncluded".into(), json!(true));
        object.insert("officialTakeover".into(), json!("unverified"));
        Ok(value)
    }
}
