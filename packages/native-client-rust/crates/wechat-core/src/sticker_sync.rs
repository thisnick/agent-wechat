//! Shared complete Saved reads and receipt-derived reconciliation.
use super::*;
use wechat_protocol::{custom_stickers as wire, protobuf as pb};
impl Core {
    fn saved_read_with(
        &self,
        account: &str,
        id: &str,
        fence: &MediaAuthorizationFence,
        retain: &mut dyn FnMut(&[u8]) -> Result<()>,
        request: &[u8],
    ) -> Result<wire::CustomStickerPage> {
        self.resume_media_state(account, id, fence)?;
        let mut state = self.account_state(account)?;
        let raw =
            self.native_request(account, &mut state, |ctx| ctx.custom_stickers_raw(request))?;
        ensure!(raw.len() <= 2 * 1024 * 1024, "sticker_receipt_limit");
        retain(&raw)?;
        self.resume_media_state(account, id, fence)?;
        wire::decode_custom_stickers(&raw)
    }
    pub(super) fn read_saved_order_with(
        &self,
        account: &str,
        id: &str,
        fence: &MediaAuthorizationFence,
        retain: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<(Vec<String>, Option<String>)> {
        let mut order = Vec::new();
        let mut cursor = (0, Vec::new());
        for _ in 0..100 {
            let page = self.saved_read_with(
                account,
                id,
                fence,
                retain,
                &wire::custom_sticker_list_request(cursor.0, &cursor.1)?,
            )?;
            for item in &page.ids {
                ensure!(
                    order.len() < 10000 && !order.contains(item),
                    "sticker_list_invalid"
                );
                order.push(item.clone());
            }
            if page.complete {
                return Ok((order, page.version));
            }
            let next = (
                page.next_index.context("sticker_cursor_missing")?,
                page.next_data,
            );
            ensure!(next != cursor, "sticker_cursor_not_advancing");
            cursor = next;
        }
        bail!("sticker_page_limit")
    }
    pub(super) fn reconcile_saved_order_with(
        &self,
        account: &str,
        id: &str,
        fence: &MediaAuthorizationFence,
        retain: &mut dyn FnMut(&[u8]) -> Result<()>,
        order: Vec<String>,
    ) -> Result<()> {
        let before = self.saved_stickers(account)?;
        let reader = self.profile_reader_owned(account)?;
        let mut missing = Vec::new();
        for item in &order {
            match reader.saved_sticker_record(item) {
                Ok(_) => {}
                Err(wechat_profile::ProfileError::NotCached) => missing.push(item.clone()),
                Err(error) => return Err(error.into()),
            }
        }
        let mut records = Vec::new();
        for batch in missing.chunks(100) {
            let page = self.saved_read_with(
                account,
                id,
                fence,
                retain,
                &wire::custom_sticker_metadata_request(batch)?,
            )?;
            for item in page.metadata {
                ensure!(batch.contains(&item.id), "unrequested_sticker_metadata");
                let fields = pb::decode(&item.raw)?;
                let string = |n| -> Result<String> {
                    Ok(
                        std::str::from_utf8(pb::optional_data(&fields, n)?.unwrap_or_default())?
                            .into(),
                    )
                };
                records.push(wechat_profile::SavedStickerRecord {
                    id: item.id,
                    cdn_url: string(2)?,
                    thumb_url: string(3)?,
                    encrypt_url: string(5)?,
                    aes_key: string(6)?,
                    product_id: string(7)?,
                    extern_url: string(8)?,
                    extern_md5: string(9)?,
                    ..Default::default()
                });
            }
        }
        ensure!(
            records.len() == missing.len(),
            "sticker_metadata_incomplete"
        );
        let removed_metadata = before
            .items
            .iter()
            .filter(|item| !order.contains(&item.id))
            .map(|item| item.id.clone())
            .collect();
        self.resume_media_state(account, id, fence)?;
        self.apply_saved_sticker_receipt(
            account,
            before.revision,
            wechat_profile::SavedStickerProjection {
                order,
                removed_metadata,
                records,
            },
        )?;
        Ok(())
    }
}
