//! Asset handles authorize their captured source, never a later local-ID occupant.
use super::{digest, DownloadPlan};
use crate::{message_id, Core};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use wechat_store::MediaAssetRecord;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct MessageIdentity {
    message_id: String,
    thread_id: String,
    server_id: String,
    created_at: i64,
    pending_own: bool,
    content_hash: String,
    quoted_server_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthorizedPlan {
    #[serde(default)]
    pub(super) identity: Option<MessageIdentity>,
    #[serde(flatten)]
    pub(super) plan: DownloadPlan,
}
fn canonical(value: &str) -> bool {
    value
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .map(|number| number.to_string())
        .as_deref()
        == Some(value)
}
pub(super) fn capture(message: &wechat_profile::Message) -> Result<MessageIdentity> {
    let thread = message.peer.clone().context("media_identity_unavailable")?;
    let server = message
        .server_message_id
        .clone()
        .filter(|id| canonical(id))
        .context("media_identity_unavailable")?;
    Ok(MessageIdentity {
        message_id: message_id(&message.id)?,
        thread_id: thread,
        server_id: server,
        created_at: message.create_time,
        pending_own: message.outgoing == Some(true)
            && matches!(message.raw_fields.get("status"),Some(wechat_profile::SqlValue::Integer(n)) if n=="7"),
        content_hash: digest(
            message
                .content
                .text
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        ),
        quoted_server_id: crate::quotes::message_app(message)
            .and_then(|app| app.reference.map(|reference| reference.0)),
    })
}
impl Core {
    pub(super) fn asset_plan(
        &self,
        account: &str,
        row: &MediaAssetRecord,
    ) -> Result<AuthorizedPlan> {
        let bytes = self
            .vault
            .open_record(account, &format!("plan:{}", row.id), &row.plan)
            .map_err(|_| anyhow::anyhow!("media_identity_unavailable"))?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("media_identity_unavailable"))
    }
    pub(super) fn authorize_media_record(
        &self,
        account: &str,
        row: &MediaAssetRecord,
    ) -> Result<()> {
        let Some(source) = row.message_id.as_deref() else {
            return Ok(());
        };
        let binding = self
            .asset_plan(account, row)?
            .identity
            .context("media_identity_unavailable")?;
        ensure!(
            binding.message_id == source
                && canonical(&binding.server_id)
                && binding
                    .quoted_server_id
                    .as_ref()
                    .is_none_or(|id| canonical(id)),
            "media_identity_unavailable"
        );
        if self
            .store
            .recall_marker(account, &binding.thread_id, &binding.server_id)?
            .is_some()
        {
            bail!("message_recalled");
        }
        if binding.pending_own {
            if let Some(marker) =
                self.store
                    .recall_by_source(account, source, binding.created_at)?
            {
                ensure!(
                    marker.thread_id == binding.thread_id,
                    "media_identity_unavailable"
                );
                bail!("message_recalled");
            }
        }
        if let Some(quoted) = &binding.quoted_server_id {
            if self
                .store
                .recall_marker(account, &binding.thread_id, quoted)?
                .is_some()
            {
                bail!("message_recalled");
            }
        }
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        let mut current = match reader.message(&crate::source_message_id(source)?, own.as_deref()) {
            Ok(message) => message,
            Err(
                wechat_profile::ProfileError::NotCached
                | wechat_profile::ProfileError::InvalidQuery,
            ) => bail!("media_source_changed"),
            Err(error) => return Err(error.into()),
        };
        use md5::Digest;
        let table = format!("Msg_{:x}", md5::Md5::digest(binding.thread_id.as_bytes()));
        ensure!(
            current.id.table.eq_ignore_ascii_case(&table)
                && current
                    .peer
                    .as_ref()
                    .is_none_or(|thread| thread == &binding.thread_id)
                && current.server_message_id.as_deref() == Some(binding.server_id.as_str())
                && current.create_time == binding.created_at,
            "media_source_changed"
        );
        // The sealed thread and table hash remain authoritative if mutable
        // name metadata no longer resolves the current source's peer.
        current.peer = Some(binding.thread_id.clone());
        if self.observe_profile_recall(account, &current)?.is_some() {
            bail!("message_recalled");
        }
        ensure!(
            digest(
                current
                    .content
                    .text
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes()
            ) == binding.content_hash,
            "media_source_changed"
        );
        if let Some(quoted) = binding.quoted_server_id {
            if self.quote_is_recalled(
                account,
                &binding.thread_id,
                &quoted,
                &reader,
                own.as_deref(),
            )? {
                bail!("message_recalled");
            }
        }
        Ok(())
    }
}
