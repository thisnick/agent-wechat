//! Supported push controls queue reconciliation; their cursors wait for readback.
use crate::Core;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wechat_protocol::wire::AddMessage;
const PREFIX: &str = "sticker-refresh:";
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StickerRefresh {
    pub key: String,
    pub value: Vec<u8>,
    pub receipt: Option<String>,
}
/// Recognize only recorded self-addressed service envelopes. Unknown extensions
/// remain unprojected even if a separate startup refresh obtains current lists.
pub(crate) fn supported_sticker_control(message: &AddMessage, own: &str) -> bool {
    let parse =
        || -> Result<()> {
            ensure!(
                message.message_type == 10002
                    && message.from == "weixin"
                    && message.to == own
                    && message.created_at > 0,
                "unsupported_sticker_control"
            );
            let xml = std::str::from_utf8(&message.content)?;
            ensure!(
                xml.len() <= 65536
                    && !xml.to_ascii_uppercase().contains("<!DOCTYPE")
                    && !xml.to_ascii_uppercase().contains("<!ENTITY"),
                "unsupported_sticker_control"
            );
            let doc = roxmltree::Document::parse_with_options(
                xml,
                roxmltree::ParsingOptions {
                    allow_dtd: false,
                    nodes_limit: 128,
                    ..Default::default()
                },
            )?;
            ensure!(
                !doc.descendants().any(|n| n.is_comment() || n.is_pi()),
                "unsupported_sticker_control"
            );
            let root = doc.root_element();
            ensure!(
                root.namespaces().len() == 0
                    && root.children().all(|n| n.is_element()
                        || n.is_text() && n.text().is_some_and(|s| s.trim().is_empty())),
                "unsupported_sticker_control"
            );
            let family = root.attribute("type").unwrap_or_default();
            ensure!(
                root.tag_name().name() == "sysmsg"
                    && root.tag_name().namespace().is_none()
                    && root.attributes().len() == 1
                    && matches!(family, "EmojiBackup" | "EmotionBackup"),
                "unsupported_sticker_control"
            );
            let children = root
                .children()
                .filter(|n| n.is_element())
                .collect::<Vec<_>>();
            ensure!(
                children.len() == 1
                    && children[0].tag_name().name() == family
                    && children[0].tag_name().namespace().is_none()
                    && children[0].attributes().len() == 0,
                "unsupported_sticker_control"
            );
            let fields = children[0]
                .children()
                .filter(|n| n.is_element())
                .collect::<Vec<_>>();
            ensure!(
                children[0].children().all(|n| n.is_element()
                    || n.is_text() && n.text().is_some_and(|s| s.trim().is_empty())),
                "unsupported_sticker_control"
            );
            let get = |name: &str| -> Result<roxmltree::Node<'_, '_>> {
                let found = fields
                    .iter()
                    .filter(|n| n.tag_name().name() == name && n.tag_name().namespace().is_none())
                    .collect::<Vec<_>>();
                ensure!(found.len() == 1, "unsupported_sticker_control");
                Ok(*found[0])
            };
            let opcode = get("OpCode")?;
            let device = get("DeviceID")?;
            ensure!(
                opcode.attributes().len() == 0
                    && device.attributes().len() == 0
                    && !opcode.children().any(|n| n.is_element())
                    && !device.children().any(|n| n.is_element()),
                "unsupported_sticker_control"
            );
            let device = device.text().unwrap_or_default();
            ensure!(
                device.len() == 32
                    && device
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "unsupported_sticker_control"
            );
            match (family, opcode.text().unwrap_or_default()) {
                ("EmojiBackup", "3") => ensure!(fields.len() == 2, "unsupported_sticker_control"),
                ("EmojiBackup", "2") => {
                    ensure!(fields.len() == 3, "unsupported_sticker_control");
                    let id = get("EmojiMd5")?;
                    let text = id.text().unwrap_or_default();
                    ensure!(
                        !id.children().any(|n| n.is_element())
                            && text.len() == 32
                            && text
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                        "unsupported_sticker_control"
                    );
                    let allowed = [
                        "aeskey",
                        "attachedtext",
                        "attachedtextcolor",
                        "cdnurl",
                        "designerid",
                        "encrypturl",
                        "externmd5",
                        "externurl",
                        "lensid",
                        "productid",
                        "thumburl",
                    ];
                    ensure!(
                        id.attributes().len() == allowed.len()
                            && id.attributes().all(|a| a.namespace().is_none()
                                && allowed.contains(&a.name())
                                && a.value().is_empty()),
                        "unsupported_sticker_control"
                    );
                }
                ("EmotionBackup", "1" | "2") => {
                    ensure!(fields.len() == 3, "unsupported_sticker_control");
                    let product = get("ProductID")?;
                    let id = product.text().unwrap_or_default();
                    ensure!(
                        product.attributes().len() == 0
                            && !product.children().any(|n| n.is_element())
                            && !id.is_empty()
                            && id.len() <= 1024
                            && !id.chars().any(char::is_control),
                        "unsupported_sticker_control"
                    );
                }
                _ => anyhow::bail!("unsupported_sticker_control"),
            }
            Ok(())
        };
    parse().is_ok()
}
impl Core {
    pub(crate) fn retain_sticker_refresh(&self, account: &str, receipt: &str) -> Result<()> {
        self.store.put_runtime(
            account,
            &format!("{PREFIX}{}", crate::media::digest(receipt.as_bytes())),
            receipt.as_bytes(),
        )
    }
    pub(crate) fn sticker_reconnected(self: &Arc<Self>, account: &str) -> Result<()> {
        if self.upstream.kind() != crate::upstream::UpstreamKind::Native {
            return Ok(());
        }
        self.store.put_runtime(
            account,
            &format!("{PREFIX}connection"),
            uuid::Uuid::new_v4().as_bytes(),
        )?;
        self.schedule_sticker_reconciliation(account)
    }
    pub(crate) fn schedule_sticker_reconciliation(self: &Arc<Self>, account: &str) -> Result<()> {
        let pending = self.store.runtime_prefix(account, PREFIX, 100)?;
        if pending.is_empty() {
            return Ok(());
        }
        if let Some(id) = self.store.runtime(account, "sticker-refresh-job")? {
            if self
                .store
                .media_job(account, std::str::from_utf8(&id)?)
                .is_ok_and(|j| matches!(j.state.as_str(), "queued" | "running"))
            {
                return Ok(());
            }
        }
        let controls = pending
            .into_iter()
            .map(|(key, value)| StickerRefresh {
                receipt: if key == format!("{PREFIX}connection") {
                    None
                } else {
                    std::str::from_utf8(&value).ok().map(str::to_string)
                },
                key,
                value,
            })
            .collect::<Vec<_>>();
        let key = format!(
            "refresh:{}",
            crate::media::digest(&serde_json::to_vec(&controls)?)
        );
        let job = self.queue_sticker_control_job(account, &key, controls)?;
        self.store
            .put_runtime(account, "sticker-refresh-job", job.id.as_bytes())?;
        Ok(())
    }
    pub(crate) fn finish_sticker_controls(
        &self,
        account: &str,
        controls: &[StickerRefresh],
    ) -> Result<()> {
        let mut completed_receipt = false;
        for control in controls {
            if self.store.runtime(account, &control.key)?.as_deref()
                == Some(control.value.as_slice())
            {
                if let Some(receipt) = &control.receipt {
                    self.store.put_runtime(account, receipt, b"1")?;
                    completed_receipt = true;
                }
                self.store.clear_runtime(account, &control.key)?;
            }
        }
        if completed_receipt {
            self.project_pending_with_media(account, false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(xml: &str) -> AddMessage {
        use wechat_protocol::protobuf as p;
        let raw = p::message(&[
            p::uint(1, 0),
            p::bytes(2, &p::bytes(1, b"weixin")),
            p::bytes(3, &p::bytes(1, b"self")),
            p::uint(4, 10002),
            p::bytes(5, &p::bytes(1, xml.as_bytes())),
            p::uint(6, 0),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(b"")),
            p::uint(9, 100),
            p::uint(12, 42),
            p::uint(13, 1),
        ])
        .unwrap();
        wechat_protocol::wire::decode_add_message(&raw, "self").unwrap()
    }
    #[test]
    fn only_recorded_service_envelopes_trigger_reconciliation() {
        let xml="<sysmsg type='EmojiBackup'><EmojiBackup><OpCode>3</OpCode><DeviceID>00000000000000000000000000000000</DeviceID></EmojiBackup></sysmsg>";
        assert!(supported_sticker_control(&message(xml), "self"));
        assert!(!supported_sticker_control(&message(xml), "other"));
        let mut impostor = message(xml);
        impostor.from = "peer".into();
        assert!(!supported_sticker_control(&impostor, "self"));
        for unknown in [
            xml.replace("<OpCode>3", "<OpCode>1"),
            xml.replace("</EmojiBackup>", "<Extra/></EmojiBackup>"),
            xml.replace("<EmojiBackup>", "<EmojiBackup xmlns='other'>"),
            xml.replace("<EmojiBackup>", "unmapped<EmojiBackup>"),
        ] {
            assert!(!supported_sticker_control(&message(&unknown), "self"));
        }
        let pack="<sysmsg type='EmotionBackup'><EmotionBackup><OpCode>1</OpCode><DeviceID>00000000000000000000000000000000</DeviceID><ProductID>pack.fixture</ProductID></EmotionBackup></sysmsg>";
        assert!(supported_sticker_control(&message(pack), "self"));
        assert!(!supported_sticker_control(
            &message(&pack.replace("<OpCode>1", "<OpCode>3")),
            "self"
        ));
    }
    #[test]
    fn a_completed_read_does_not_consume_a_newer_reconnect_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let refresh = StickerRefresh {
            key: format!("{PREFIX}connection"),
            value: b"before".to_vec(),
            receipt: None,
        };
        core.store
            .put_runtime("one", &refresh.key, b"after")
            .unwrap();
        core.finish_sticker_controls("one", std::slice::from_ref(&refresh))
            .unwrap();
        assert_eq!(
            core.store.runtime("one", &refresh.key).unwrap(),
            Some(b"after".to_vec())
        );
    }
}
