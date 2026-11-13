//! Account-bound metadata reads. No GET path adds membership or acquires originals.
use super::*;
use crate::{
    StickerCollection, StickerCollections, StickerPack, StickerPackList, StickerPackSticker,
    StickerPackStickers,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use wechat_protocol::sticker_packs as wire;
const FEED_QUERY: &str = "sticker-pack-feed-v1";
const BUILTIN_PACK: &str = "com.tencent.xin.emoticon.tusiji";
#[derive(Serialize, Deserialize)]
struct PackCursor {
    view: String,
    expires: i64,
}
#[derive(Serialize, Deserialize)]
struct PackContinuation {
    cursor: Vec<u8>,
    seen: Vec<String>,
}
impl Core {
    /// Derive Saved and all canonical added-pack picker descriptors.
    pub fn sticker_collections(&self, account: &str) -> Result<StickerCollections> {
        let mut items = vec![StickerCollection {
            id: "saved".into(),
            kind: "saved".into(),
            title: "Saved".into(),
            pack_id: None,
        }];
        for pack in self
            .profile_reader_owned(account)?
            .installed_sticker_packs()?
        {
            items.push(StickerCollection {
                id: format!("pack:{}", pack.id),
                kind: "pack".into(),
                title: pack.name.unwrap_or_else(|| pack.id.clone()),
                pack_id: Some(pack.id),
            });
        }
        Ok(StickerCollections { items })
    }
    /// Read the complete account-added list from the official profile. Catalog
    /// pagination or cache coverage cannot remove members from this view.
    pub fn added_sticker_packs(&self, account: &str) -> Result<StickerPackList> {
        let reader = self.profile_reader_owned(account)?;
        let mut items = vec![];
        for pack in reader.installed_sticker_packs()? {
            let mut item = StickerPack {
                id: pack.id.clone(),
                name: pack.name.unwrap_or(pack.id),
                description: None,
                membership: "added".into(),
                content_readiness: "unverified".into(),
                eligible: None,
                sticker_count: None,
                icon: None,
                cover: None,
            };
            let ids = reader
                .sticker_pack_files(&item.id)?
                .into_iter()
                .map(|file| file.id)
                .collect::<Vec<_>>();
            self.pack_media_state(account, &mut item, Some(&ids));
            items.push(item);
        }
        Ok(StickerPackList {
            items,
            next_cursor: None,
            coverage: "account_added".into(),
        })
    }
    /// Read an available-feed page with a bounded opaque continuation. Requires
    /// the account writer gate for authenticated transport/cursor persistence.
    pub fn browse_sticker_packs(
        &self,
        account: &str,
        cursor: Option<&str>,
    ) -> Result<StickerPackList> {
        let mut continuation = if let Some(encoded) = cursor {
            ensure!(encoded.len() <= 8192, "invalid_cursor");
            let payload = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
            let plain = self
                .cursor_vault
                .open_record(account, "sticker-pack-cursor", &payload)
                .map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
            let cursor: PackCursor =
                serde_json::from_slice(&plain).map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
            ensure!(
                cursor.expires > chrono::Utc::now().timestamp(),
                "pagination_cursor_expired"
            );
            let view = self.store.pagination_view(account, &cursor.view)?;
            ensure!(
                view.query_hash == FEED_QUERY && view.expires_at == cursor.expires,
                "invalid_cursor"
            );
            serde_json::from_slice::<PackContinuation>(&self.vault.open_record(
                account,
                &format!("sticker-pack-feed:{}", cursor.view),
                &view.payload,
            )?)?
        } else {
            PackContinuation {
                cursor: vec![],
                seen: vec![],
            }
        };
        let mut state = self.account_state(account)?;
        let raw = self.native_request(account, &mut state, |ctx| {
            ctx.sticker_pack_page_raw(wire::StickerPackFeed::Catalog, &continuation.cursor)
        })?;
        let page = wire::decode_sticker_pack_page(&raw)?;
        ensure!(!page.restart, "pagination_cursor_expired");
        let added: std::collections::BTreeSet<_> = self
            .profile_reader_owned(account)?
            .installed_sticker_packs()?
            .into_iter()
            .map(|pack| pack.id)
            .collect();
        let mut items = vec![];
        for summary in page.items {
            ensure!(
                !continuation.seen.contains(&summary.id) && continuation.seen.len() < 10000,
                "sticker_catalog_not_advancing"
            );
            continuation.seen.push(summary.id.clone());
            if summary.id != BUILTIN_PACK {
                let mut item = public_sticker_pack(&summary, None, added.contains(&summary.id))?;
                self.pack_media_state(account, &mut item, None);
                self.advertise_pack_previews(account, &mut item, &summary)?;
                items.push(item);
            }
        }
        let next_cursor = if page.complete {
            None
        } else {
            ensure!(
                !page.next_cursor.is_empty() && page.next_cursor != continuation.cursor,
                "sticker_cursor_not_advancing"
            );
            continuation.cursor = page.next_cursor;
            let view = uuid::Uuid::new_v4().to_string();
            let expires = chrono::Utc::now().timestamp() + 1800;
            let payload = self.vault.seal(
                account,
                &format!("sticker-pack-feed:{view}"),
                &serde_json::to_vec(&continuation)?,
            )?;
            self.store.save_pagination_view(
                account,
                &view,
                FEED_QUERY,
                &self.store.event_boundary(account)?,
                expires,
                &payload,
            )?;
            Some(URL_SAFE_NO_PAD.encode(self.cursor_vault.seal(
                account,
                "sticker-pack-cursor",
                &serde_json::to_vec(&PackCursor { view, expires })?,
            )?))
        };
        Ok(StickerPackList {
            items,
            next_cursor,
            coverage: "available_feed".into(),
        })
    }
    pub(super) fn remote_sticker_pack_detail(
        &self,
        account: &str,
        pack: &str,
    ) -> Result<wire::StickerPackDetail> {
        let mut state = self.account_state(account)?;
        let raw =
            self.native_request(account, &mut state, |ctx| ctx.sticker_pack_detail_raw(pack))?;
        wire::decode_sticker_pack_detail(&raw, pack)
    }
    /// Read current detail metadata without establishing membership or acquiring media.
    pub fn sticker_pack_detail(&self, account: &str, pack: &str) -> Result<StickerPack> {
        let detail = self.remote_sticker_pack_detail(account, pack)?;
        let mut item = public_sticker_pack(
            &detail.summary,
            Some(detail.items.len() as u32),
            self.profile_reader_owned(account)?
                .installed_sticker_packs()?
                .iter()
                .any(|item| item.id == pack),
        )?;
        let ids = detail
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        self.pack_media_state(account, &mut item, Some(&ids));
        self.advertise_pack_previews(account, &mut item, &detail.summary)?;
        Ok(item)
    }
    /// Read the complete supplied item list and official locale captions. No
    /// media fetch or membership change is started; URLs never enter the result.
    pub fn sticker_pack_stickers(
        &self,
        account: &str,
        pack: &str,
        language: &str,
    ) -> Result<StickerPackStickers> {
        ensure!(
            !language.is_empty() && language.len() <= 64 && !language.chars().any(char::is_control),
            "invalid_language"
        );
        let detail = self.remote_sticker_pack_detail(account, pack)?;
        let mut state = self.account_state(account)?;
        let raw = self.native_request(account, &mut state, |ctx| {
            ctx.sticker_pack_captions_raw(pack)
        })?;
        let captions = wire::decode_sticker_pack_captions(&raw)?;
        let items = detail
            .items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let pairs = captions
                    .items
                    .iter()
                    .find(|row| row.id == item.id)
                    .map(|row| row.captions.clone())
                    .unwrap_or_default();
                let caption = pairs
                    .iter()
                    .find(|(locale, _)| locale == language)
                    .or_else(|| pairs.iter().find(|(locale, _)| locale == "default"))
                    .map(|(_, text)| text.clone());
                Ok(StickerPackSticker {
                    id: item.id.clone(),
                    pack_id: pack.into(),
                    ordinal: index as u32 + 1,
                    caption,
                    captions: pairs,
                    original: self.indexed_pack_asset(account, pack, &item.id, "original"),
                    thumbnail: self
                        .indexed_pack_asset(account, pack, &item.id, "thumbnail")
                        .or(self.advertised_sticker_preview(
                            account,
                            pack,
                            &item.id,
                            &item.thumbnail_url,
                        )?),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StickerPackStickers {
            pack_id: pack.into(),
            items,
        })
    }
    fn advertise_pack_previews(
        &self,
        account: &str,
        pack: &mut StickerPack,
        summary: &wire::StickerPackSummary,
    ) -> Result<()> {
        if pack.icon.is_none() {
            pack.icon =
                self.advertised_sticker_preview(account, &pack.id, "icon", &summary.icon_url)?;
        }
        pack.cover =
            self.advertised_sticker_preview(account, &pack.id, "cover", &summary.cover_url)?;
        Ok(())
    }
    fn advertised_sticker_preview(
        &self,
        account: &str,
        pack: &str,
        role: &str,
        url: &str,
    ) -> Result<Option<MediaAsset>> {
        if url.is_empty() || wechat_protocol::media::validate_link_thumbnail_url(url).is_err() {
            return Ok(None);
        }
        let now = chrono::Utc::now().timestamp();
        let id = format!(
            "sticker_preview_{}",
            digest(&serde_json::to_vec(&(pack, role, url, now / 86400))?)
        );
        if let Ok(asset) = self.media_asset(account, &id) {
            if asset.state != "expired" {
                return Ok(Some(asset));
            }
        }
        let asset: MediaAsset = serde_json::from_value(
            json!({"id":id,"accountId":account,"kind":"image","variant":"thumbnail","state":"pending","retryable":true,"officialWriteback":"not_applicable","expiresAt":now+86400}),
        )?;
        let mut metadata = serde_json::to_value(&asset)?;
        metadata["preparationExpiresAt"] = (now + 86400).into();
        let plan = self.vault.seal(
            account,
            &format!("plan:{id}"),
            &serde_json::to_vec(&identity::AuthorizedPlan {
                identity: None,
                plan: DownloadPlan::LinkThumbnail {
                    source: crate::links::LinkThumbnail::Http(url.into()),
                },
            })?,
        )?;
        self.store.put_media_asset(
            account,
            &MediaAssetRecord {
                id,
                message_id: None,
                kind: "image".into(),
                variant: "thumbnail".into(),
                metadata,
                plan,
                payload: None,
            },
        )?;
        Ok(Some(asset))
    }
    fn pack_media_state(&self, account: &str, pack: &mut StickerPack, ids: Option<&[String]>) {
        pack.icon = self.indexed_pack_asset(account, &pack.id, "icon", "icon");
        if let Some(ids) = ids {
            if !ids.is_empty() {
                pack.sticker_count = Some(ids.len() as u32);
                let available = ids
                    .iter()
                    .filter(|id| {
                        self.indexed_pack_asset(account, &pack.id, id, "original")
                            .is_some()
                    })
                    .count();
                pack.content_readiness = if available == ids.len() {
                    "available"
                } else if available > 0 {
                    "partial"
                } else {
                    "pending"
                }
                .into();
            }
        }
    }
}

pub(super) fn eligible_pack(summary: &wire::StickerPackSummary) -> bool {
    summary.id != BUILTIN_PACK
        && summary.pack_type & 4 != 0
        && summary.flags & 8 == 0
        && (summary.expiry == 0 || i64::from(summary.expiry) > chrono::Utc::now().timestamp())
}
fn public_sticker_pack(
    summary: &wire::StickerPackSummary,
    count: Option<u32>,
    added: bool,
) -> Result<StickerPack> {
    let eligible = eligible_pack(summary);
    Ok(StickerPack {
        id: summary.id.clone(),
        name: if summary.name.is_empty() {
            summary.id.clone()
        } else {
            summary.name.clone()
        },
        description: (!summary.description.is_empty()).then(|| summary.description.clone()),
        membership: if added { "added" } else { "not_added" }.into(),
        content_readiness: "unverified".into(),
        eligible: Some(eligible),
        sticker_count: count,
        icon: None,
        cover: None,
    })
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn advertised_previews_do_not_acquire_media_or_create_membership() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&directory.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let asset = core
            .advertised_sticker_preview(
                "one",
                "synthetic-pack",
                "icon",
                "https://example.invalid/icon.png",
            )
            .unwrap()
            .unwrap();
        assert_eq!(asset.state, "pending");
        assert!(asset.content_url.is_none());
        assert!(asset.sha256.is_none());
        assert!(core.store.queued_media_jobs("one").unwrap().is_empty());
        assert!(core.media_content("one", &asset.id).is_err());
        let serialized = serde_json::to_string(&asset).unwrap();
        assert!(!serialized.contains("example.invalid"));
        assert!(core
            .advertised_sticker_preview(
                "one",
                "synthetic-pack",
                "cover",
                "http://127.0.0.1/private"
            )
            .unwrap()
            .is_none());
    }
}
