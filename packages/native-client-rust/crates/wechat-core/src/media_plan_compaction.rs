//! Shrinking sealed plans in place.
//!
//! The store cannot open sealed plans, so the core rewrites two kinds that
//! hold bytes the account already keeps elsewhere:
//!
//! | Plan | Kept whole while | Then keeps |
//! | --- | --- | --- |
//! | Voice asset plan | the official profile does not return the note's encoded audio | server ID and length; a download reads the profile's copy first |
//! | Finished sticker acquisition, among the newest of its pack | unfinished, or awaiting a profile write; for a day | pack, item IDs, captions and archive locator |
//!
//! Each rewrite is one compare-and-swap row update, so a crash leaves the
//! old plan or the new one. A rewrite reads only the sealed plan and the
//! official profile; it never sends a request or starts a transfer. Running it
//! again finds nothing more to do.
use super::*;

/// Sealed voice plans smaller than this hold no audio.
const VOICE_PLAN_MIN: usize = 1024;
/// Plans examined per guarded step.
pub(super) const PLAN_STEP: usize = 20;
/// Upper bound on steps in one pass; the next pass continues.
const PLAN_STEPS: usize = 10_000;

/// Byte fields of sealed job plans. New plans write base64 text. Plans written
/// by earlier versions hold JSON number arrays, about 3.6 times the size of
/// the bytes, and still read.
pub(super) mod plan_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    struct Text<'a>(&'a [u8]);
    impl Serialize for Text<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_str(&STANDARD.encode(self.0))
        }
    }
    struct Bytes(Vec<u8>);
    impl<'de> Deserialize<'de> for Bytes {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visit;
            impl<'de> de::Visitor<'de> for Visit {
                type Value = Bytes;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("base64 text or an array of bytes")
                }
                fn visit_str<E: de::Error>(self, value: &str) -> Result<Bytes, E> {
                    STANDARD.decode(value).map(Bytes).map_err(E::custom)
                }
                fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Bytes, E> {
                    Ok(Bytes(value.to_vec()))
                }
                fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Bytes, A::Error> {
                    let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
                    while let Some(byte) = seq.next_element::<u8>()? {
                        bytes.push(byte);
                    }
                    Ok(Bytes(bytes))
                }
            }
            deserializer.deserialize_any(Visit)
        }
    }
    /// `Option<Vec<u8>>`.
    pub(crate) mod option {
        use super::*;
        pub(crate) fn serialize<S: Serializer>(
            value: &Option<Vec<u8>>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(bytes) => serializer.serialize_some(&Text(bytes)),
                None => serializer.serialize_none(),
            }
        }
        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Vec<u8>>, D::Error> {
            Ok(Option::<Bytes>::deserialize(deserializer)?.map(|bytes| bytes.0))
        }
    }
    /// `Vec<Vec<u8>>`.
    pub(crate) mod list {
        use super::*;
        pub(crate) fn serialize<S: Serializer>(
            value: &[Vec<u8>],
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serializer.collect_seq(value.iter().map(|bytes| Text(bytes)))
        }
        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Vec<Vec<u8>>, D::Error> {
            Ok(Vec::<Bytes>::deserialize(deserializer)?
                .into_iter()
                .map(|bytes| bytes.0)
                .collect())
        }
    }
}

/// The encoded audio the official profile holds for a voice message, if any.
pub(super) fn profile_voice(
    reader: &wechat_profile::ProfileReader,
    message: &wechat_profile::Message,
) -> Option<Vec<u8>> {
    let chat = message.peer.as_deref()?;
    let server = message.server_message_id.as_deref()?;
    let shards = reader
        .schemas()
        .iter()
        .filter(|schema| schema.database.rsplit('/').next() == Some("media_0.db"))
        .collect::<Vec<_>>();
    if shards.len() != 1 {
        return None;
    }
    reader
        .cached_voice(
            &shards[0].database,
            chat,
            &message.id.local_id,
            Some(server),
        )
        .ok()
        .map(|cached| cached.bytes)
}

/// Plans rewritten smaller in one pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanCompaction {
    /// Voice asset plans that dropped audio the profile holds.
    pub(super) voice: usize,
    /// Finished sticker acquisitions reduced to what reuse reads.
    pub(super) sticker_acquisitions: usize,
}
/// One bounded step: plans rewritten and where the next step starts.
#[derive(Debug, Default)]
pub(super) struct PlanStep {
    pub(super) rewritten: usize,
    /// The last ID examined, or `None` when nothing is left.
    pub(super) next: Option<String>,
}
fn step(rewritten: usize, examined: &[String], limit: usize) -> PlanStep {
    PlanStep {
        rewritten,
        next: (examined.len() >= limit)
            .then(|| examined.last().cloned())
            .flatten(),
    }
}

impl Core {
    /// The encoded audio the official profile holds for an asset's message.
    pub(super) fn profile_voice_for(&self, account: &str, asset: &MediaAsset) -> Option<Vec<u8>> {
        let source = source_message_id(asset.message_id.as_deref()?).ok()?;
        let reader = self.profile_reader_owned(account).ok()?;
        let own = self.profile_username(account).ok()?;
        let message = reader.message(&source, own.as_deref()).ok()?;
        profile_voice(&reader, &message)
    }
    /// Rewrite up to `limit` voice asset plans after `after` without encoded
    /// audio that the official profile returns unchanged for the same message,
    /// or that a download would ignore because its length does not match.
    /// Anything else is kept.
    pub(super) fn compact_voice_plans(
        &self,
        account: &str,
        after: &str,
        limit: usize,
    ) -> Result<PlanStep> {
        let rows = self
            .store
            .voice_plans_after(account, after, VOICE_PLAN_MIN, limit)?;
        let examined = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let Ok(reader) = self.profile_reader_owned(account) else {
            return Ok(PlanStep::default());
        };
        let own = self.profile_username(account)?;
        let mut rewritten = 0;
        for row in &rows {
            if let Ok(Some(sealed)) =
                self.voice_plan_without_audio(account, row, &reader, own.as_deref())
            {
                if self
                    .store
                    .replace_media_asset_plan(account, &row.id, &row.plan, &sealed)?
                {
                    rewritten += 1;
                }
            }
        }
        Ok(step(rewritten, &examined, limit))
    }
    fn voice_plan_without_audio(
        &self,
        account: &str,
        row: &MediaAssetRecord,
        reader: &wechat_profile::ProfileReader,
        own: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        let mut authorized = self.asset_plan(account, row)?;
        let DownloadPlan::Voice { length, inline, .. } = &mut authorized.plan else {
            return Ok(None);
        };
        let Some(audio) = inline.as_deref() else {
            return Ok(None);
        };
        if audio.len() == *length as usize {
            // The asset must still name the message it was captured from.
            self.authorize_media_record(account, row)?;
            let source =
                source_message_id(row.message_id.as_deref().context("media_message_missing")?)?;
            let message = reader.message(&source, own)?;
            if profile_voice(reader, &message).as_deref() != Some(audio) {
                return Ok(None);
            }
        }
        *inline = None;
        Ok(Some(self.vault.seal(
            account,
            &format!("plan:{}", row.id),
            &serde_json::to_vec(&authorized)?,
        )?))
    }
    /// Reduce up to `limit` finished acquisitions after `after`, among the
    /// newest of their pack, to what a later pack add reads.
    pub(super) fn compact_sticker_plans(
        &self,
        account: &str,
        after: &str,
        limit: usize,
    ) -> Result<PlanStep> {
        let rows = self.store.reusable_sticker_acquisitions_after(
            account,
            chrono::Utc::now().timestamp(),
            after,
            limit,
        )?;
        let examined = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let mut rewritten = 0;
        for row in &rows {
            if self.compact_acquisition(account, row).unwrap_or(false) {
                rewritten += 1;
            }
        }
        Ok(step(rewritten, &examined, limit))
    }
    /// Run both rewrites for one account until neither has work left.
    pub(super) fn run_plan_compaction(&self, account: &str) -> Result<PlanCompaction> {
        let mut total = PlanCompaction::default();
        let mut after = String::new();
        for _ in 0..PLAN_STEPS {
            let step = self.compact_voice_plans(account, &after, PLAN_STEP)?;
            total.voice += step.rewritten;
            let Some(next) = step.next else { break };
            after = next;
        }
        after.clear();
        for _ in 0..PLAN_STEPS {
            let step = self.compact_sticker_plans(account, &after, PLAN_STEP)?;
            total.sticker_acquisitions += step.rewritten;
            let Some(next) = step.next else { break };
            after = next;
        }
        Ok(total)
    }
    /// [`Self::run_plan_compaction`] one small step at a time under the
    /// account's mutation guard, so other work for the account interleaves.
    pub(super) async fn guarded_plan_compaction(self: &Arc<Self>, account: &str) {
        for voice in [true, false] {
            let mut after = String::new();
            for _ in 0..PLAN_STEPS {
                let from = after.clone();
                let Some(step) = self
                    .guarded_storage_step(account, move |core, id| {
                        if voice {
                            core.compact_voice_plans(id, &from, PLAN_STEP)
                        } else {
                            core.compact_sticker_plans(id, &from, PLAN_STEP)
                        }
                    })
                    .await
                else {
                    break;
                };
                let Some(next) = step.next else { break };
                if self.stopping.load(Ordering::Acquire) {
                    return;
                }
                after = next;
                tokio::task::yield_now().await;
            }
        }
    }
}
