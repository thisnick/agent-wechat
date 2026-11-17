//! Operational freshness guards for privacy derived from canonical contact rows.
use super::*;
use wechat_profile::{ContactProjection, SqlValue};
use wechat_protocol::{protobuf as p, wire::ContactRecord};

const PRIVACY_MASK: u32 = 0x810100;

#[derive(Serialize, Deserialize)]
struct PrivacyInvalidation {
    previous_revision: Option<Vec<u8>>,
}

fn key(kind: &str, username: &str) -> String {
    format!("contact-privacy-{kind}:{username}")
}

fn full_flags(record: &ContactRecord) -> Result<Option<u32>> {
    let fields = p::decode(&record.raw)?;
    match (p::optional(&fields, 7)?, p::optional(&fields, 8)?) {
        (Some(p::Value::Varint(mask)), Some(p::Value::Varint(value)))
            if *mask == u64::from(u32::MAX) =>
        {
            Ok(Some(u32::try_from(*value)?))
        }
        _ => Ok(None),
    }
}

fn relevant(record: &ContactRecord) -> Result<bool> {
    if record.username.contains('@') {
        return Ok(false);
    }
    let controlled = match wechat_protocol::controlled_contact_fields(&record.raw) {
        Ok(fields) => fields,
        Err(_) => return Ok(true),
    };
    if controlled.contains(&8) {
        return Ok(true);
    }
    let fields = p::decode(&record.raw)?;
    Ok(match p::optional(&fields, 7)? {
        Some(p::Value::Varint(mask)) => {
            u32::try_from(*mask).map_or(true, |mask| mask & PRIVACY_MASK != 0)
        }
        None => false,
        _ => true,
    })
}

impl Core {
    pub(super) fn mark_partial_contact_privacy_pending(
        &self,
        account: &str,
        username: &str,
        operation: &str,
    ) -> Result<()> {
        self.store
            .put_runtime(account, &key("revision", username), operation.as_bytes())?;
        self.store
            .put_runtime(account, &key("unresolved", username), operation.as_bytes())
    }

    pub(super) fn contact_privacy_resolved(&self, account: &str, username: &str) -> Result<bool> {
        Ok(self
            .store
            .runtime(account, &key("unresolved", username))?
            .is_none())
    }

    /// Keep only receipt ordering and an unresolved marker, never a privacy-state copy.
    pub(super) fn invalidate_contact_privacy(
        &self,
        account: &str,
        record: &ContactRecord,
        receipt: &str,
    ) -> Result<()> {
        if !relevant(record)? {
            return Ok(());
        }
        let revision_key = key("revision", &record.username);
        let phase_key = format!("contact-privacy-observation:{receipt}:{}", record.username);
        let current = self.store.runtime(account, &revision_key)?;
        let prior = if let Some(raw) = self.store.runtime(account, &phase_key)? {
            serde_json::from_slice::<PrivacyInvalidation>(&raw)?
        } else {
            let prior = PrivacyInvalidation {
                previous_revision: current.clone(),
            };
            self.store
                .put_runtime(account, &phase_key, &serde_json::to_vec(&prior)?)?;
            prior
        };
        if current != prior.previous_revision && current.as_deref() != Some(receipt.as_bytes()) {
            return Ok(());
        }
        self.store
            .put_runtime(account, &revision_key, receipt.as_bytes())?;
        // Even a full replacement stays unresolved until the owned profile write
        // completes. This also prevents a failed write from exposing old flags.
        self.store.put_runtime(
            account,
            &key("unresolved", &record.username),
            receipt.as_bytes(),
        )
    }

    /// Clear freshness only after the received full flag value is actually canonical.
    pub(super) fn settle_contact_privacy_projection(
        &self,
        account: &str,
        record: &ContactRecord,
        patch: &ContactProjection,
        receipt: &str,
    ) -> Result<()> {
        if !relevant(record)?
            || self
                .store
                .runtime(account, &key("revision", &record.username))?
                .as_deref()
                != Some(receipt.as_bytes())
        {
            return Ok(());
        }
        let Some(expected) = full_flags(record)? else {
            return Ok(());
        };
        let Some(SqlValue::Integer(applied)) = patch.fields.get("flag") else {
            return Ok(());
        };
        if applied.parse::<u32>()? != expected {
            return Ok(());
        }
        let current = self
            .profile_reader_owned(account)?
            .contact(&record.username)?;
        if !matches!(current.raw_fields.get("flag"), Some(SqlValue::Integer(value)) if value.parse::<u32>().ok() == Some(expected))
        {
            return Ok(());
        }
        self.store
            .clear_runtime(account, &key("unresolved", &record.username))?;
        self.settle_contact_relationship(account, &record.username, expected & 1 != 0, receipt)?;
        self.settle_partial_contact_privacy(account, &record.username)
    }
}
