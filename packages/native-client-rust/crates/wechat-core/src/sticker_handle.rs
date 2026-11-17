//! Account-bound sticker API. The handle owns gating, blocking work and job admission.
use crate::*;
use anyhow::{ensure, Result};
use std::sync::Arc;
pub use wechat_protocol::{StickerId, StickerPackId};

/// Opaque identity of a local preparation, distinct from a Saved content ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickerPreparationId(String);
impl StickerPreparationId {
    /// Validate an existing preparation identity; its job kind is checked on use.
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref();
        ensure!(
            !value.is_empty() && value.len() <= 100 && !value.chars().any(char::is_control),
            "invalid_sticker_preparation_id"
        );
        Ok(Self(value.into()))
    }
    /// Borrow the daemon preparation identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Sticker operations bound to one registered account. Returned jobs use the
/// existing status, cancellation and recovery APIs; file export stays with callers.
#[derive(Clone)]
pub struct StickerAccount {
    core: Arc<Core>,
    account: String,
}
impl Core {
    /// Bind sticker operations to one registered account without starting work.
    pub fn stickers(self: &Arc<Self>, account: &str) -> Result<StickerAccount> {
        self.store.account(account)?;
        Ok(StickerAccount {
            core: self.clone(),
            account: account.into(),
        })
    }
}
impl StickerAccount {
    async fn run<T: Send + 'static>(
        &self,
        action: impl FnOnce(Arc<Core>, String) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let core = self.core.clone();
        let account = self.account.clone();
        let guard = core.lock_account(&account).await?;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            action(core, account)
        })
        .await?
    }
    async fn network<T: Send + 'static>(
        &self,
        action: impl FnOnce(
                Arc<Core>,
                String,
                &mut crate::account_network::AccountNetworkPermit,
            ) -> Result<T>
            + Send
            + 'static,
    ) -> Result<T> {
        let core = self.core.clone();
        let account = self.account.clone();
        let guard = core.lock_account(&account).await?;
        tokio::task::spawn_blocking(move || {
            let mut permit = crate::account_network::AccountNetworkPermit {
                guard: Some(guard),
                runtime: tokio::runtime::Handle::current(),
            };
            action(core, account, &mut permit)
        })
        .await?
    }
    /// Read Saved in canonical picker order, with its collection revision.
    pub async fn list(&self) -> Result<CustomStickerList> {
        let account = self.account.clone();
        self.core
            .read(&self.account, move |core| core.saved_stickers(&account))
            .await
    }
    /// Read one Saved item and index already owned original/preview references.
    pub async fn get(&self, id: &StickerId) -> Result<CustomSticker> {
        let id = id.to_string();
        self.run(move |core, account| core.custom_sticker(&account, &id))
            .await
    }
    /// Read verified bytes through an authenticated media identity; never acquires.
    pub async fn media(&self, asset_id: &str) -> Result<MediaContent> {
        let id = asset_id.to_string();
        self.run(move |core, account| core.media_content(&account, &id))
            .await
    }
    /// Prepare an existing uploaded source outside the account actor.
    pub async fn prepare(&self, key: &str, request: StickerPreparationRequest) -> Result<MediaJob> {
        let key = key.to_string();
        self.run(move |core, account| core.prepare_sticker(&account, &key, request))
            .await
    }
    /// Save validated preparation output without sending it.
    pub async fn add_prepared(&self, key: &str, id: &StickerPreparationId) -> Result<MediaJob> {
        let key = key.to_string();
        let id = id.as_str().to_string();
        self.run(move |core, account| {
            core.add_custom_sticker(
                &account,
                &key,
                AddCustomStickerRequest {
                    source: CustomStickerSource::Prepared { preparation_id: id },
                },
            )
        })
        .await
    }
    /// Save one verified received original belonging to the supplied message.
    pub async fn save_received(&self, key: &str, message: &str, asset: &str) -> Result<MediaJob> {
        let key = key.to_string();
        let message = message.to_string();
        let asset = asset.to_string();
        self.run(move |core, account| {
            core.add_custom_sticker(
                &account,
                &key,
                AddCustomStickerRequest {
                    source: CustomStickerSource::Message {
                        message_id: message,
                        asset_id: asset,
                    },
                },
            )
        })
        .await
    }
    /// Place after another Saved identity, or first when after is None. A stale
    /// revision fails; refresh the list instead of overwriting another device.
    pub async fn move_after(
        &self,
        key: &str,
        id: &StickerId,
        after: Option<&StickerId>,
        revision: &str,
    ) -> Result<MediaJob> {
        let key = key.to_string();
        let id = id.to_string();
        let after = after.map(ToString::to_string);
        let revision = revision.to_string();
        self.run(move |core, account| {
            core.move_custom_sticker(
                &account,
                &key,
                &id,
                &revision,
                MoveCustomStickerRequest {
                    position: CustomStickerPosition { after_id: after },
                },
            )
        })
        .await
    }
    /// Remove Saved membership using its collection revision precondition.
    pub async fn remove(&self, key: &str, id: &StickerId, revision: &str) -> Result<MediaJob> {
        let key = key.to_string();
        let id = id.to_string();
        let revision = revision.to_string();
        self.run(move |core, account| core.remove_custom_sticker(&account, &key, &id, &revision))
            .await
    }
    /// Read every account-added pack independently of catalog coverage.
    pub async fn packs(&self) -> Result<StickerPackList> {
        let account = self.account.clone();
        self.core
            .read(&self.account, move |core| {
                core.added_sticker_packs(&account)
            })
            .await
    }
    /// Read one available-feed page; None starts a new pagination sequence.
    pub async fn browse(&self, cursor: Option<&str>) -> Result<StickerPackList> {
        let cursor = cursor.map(str::to_string);
        self.network(move |core, account, permit| {
            core.browse_sticker_packs_guarded(&account, cursor.as_deref(), Some(permit))
        })
        .await
    }
    /// Read current product metadata, membership and verified content readiness.
    pub async fn pack(&self, id: &StickerPackId) -> Result<StickerPack> {
        let id = id.to_string();
        self.network(move |core, account, permit| {
            core.sticker_pack_detail_guarded(&account, &id, Some(permit))
        })
        .await
    }
    /// Read complete supplied contents and official locale captions; never acquires.
    pub async fn items(&self, id: &StickerPackId, language: &str) -> Result<StickerPackStickers> {
        let id = id.to_string();
        let language = language.to_string();
        self.network(move |core, account, permit| {
            core.sticker_pack_stickers_guarded(&account, &id, &language, Some(permit))
        })
        .await
    }
    /// Establish eligible-free membership without downloading its archive.
    pub async fn add_pack(&self, key: &str, id: &StickerPackId) -> Result<MediaJob> {
        self.membership(key, id, true).await
    }
    /// Remove membership while preserving chat history and its message media.
    pub async fn remove_pack(&self, key: &str, id: &StickerPackId) -> Result<MediaJob> {
        self.membership(key, id, false).await
    }
    async fn membership(&self, key: &str, id: &StickerPackId, added: bool) -> Result<MediaJob> {
        let key = key.to_string();
        let id = id.to_string();
        self.run(move |core, account| core.set_sticker_pack_membership(&account, &key, &id, added))
            .await
    }
    /// Acquire one Saved original, retaining its actual format.
    pub async fn acquire(&self, key: &str, id: &StickerId) -> Result<MediaJob> {
        let key = key.to_string();
        let id = id.to_string();
        self.run(move |core, account| core.download_custom_sticker(&account, &key, &id))
            .await
    }
    /// Acquire exactly one selected pack, or its selected item, without adding it.
    pub async fn acquire_pack(
        &self,
        key: &str,
        pack: &StickerPackId,
        item: Option<&StickerId>,
    ) -> Result<MediaJob> {
        let key = key.to_string();
        let pack = pack.to_string();
        let item = item.map(ToString::to_string);
        self.run(move |core, account| {
            core.download_sticker_pack(&account, &key, &pack, item.as_deref())
        })
        .await
    }
    /// Acquire a daemon-issued pack icon, cover or thumbnail asset reference.
    pub async fn acquire_preview(&self, key: &str, asset_id: &str) -> Result<MediaJob> {
        let key = key.to_string();
        let asset_id = asset_id.to_string();
        self.run(move |core, account| core.download_sticker_preview(&account, &key, &asset_id))
            .await
    }
    /// Send one stable Saved, added-pack or preparation reference with a durable key.
    pub async fn send(&self, key: &str, request: StickerSendRequest) -> Result<MediaJob> {
        let key = key.to_string();
        self.run(move |core, account| core.send_sticker(&account, &key, request))
            .await
    }
    /// Request Saved, added-pack or combined server reconciliation.
    pub async fn sync(&self, key: &str, scope: StickerSyncScope) -> Result<MediaJob> {
        let key = key.to_string();
        self.run(move |core, account| core.sync_stickers(&account, &key, scope))
            .await
    }
    /// Inspect the existing job without submitting another operation.
    pub async fn job(&self, id: &str) -> Result<MediaJob> {
        let id = id.to_string();
        self.run(move |core, account| core.media_job(&account, &id))
            .await
    }
}
