//! Durable remote metadata refresh with sparse official-file projection.
use super::*;
use wechat_profile::{ContactProjection, RawFields, SqlValue};
use wechat_protocol::{
    protobuf as p, ContactLookup, ContactRecord, GroupMetadataPage, InitContactPage, MetadataReply,
    RoomRoster,
};
#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MetadataAction {
    Contact { contact_id: String },
    Group { group_id: String },
    Members { group_id: String, version: u32 },
    ContactSync { max_pages: u32 },
    GroupSync { max_pages: u32 },
    Avatar { contact_id: String },
}
#[derive(Serialize, Deserialize)]
enum ReceiptBody {
    Contacts {
        requested: Vec<String>,
        reply: MetadataReply<ContactLookup>,
    },
    Roster {
        reply: MetadataReply<RoomRoster>,
        complete: bool,
    },
    ContactCatalog(MetadataReply<InitContactPage>),
    GroupCatalog(GroupMetadataPage),
    Avatar {
        username: String,
        source_url: String,
        bytes: Vec<u8>,
    },
}
#[derive(Serialize, Deserialize)]
struct MetadataReceipt {
    body: ReceiptBody,
}
fn valid_name(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("invalid_metadata_identity")
    };
    Ok(())
}
pub(crate) fn contact_patch(record: &ContactRecord) -> Result<ContactProjection> {
    let fields = p::decode(&record.raw)?;
    let mut out = RawFields::new();
    let sk = |number| -> Result<Option<String>> {
        p::optional_data(&fields, number)?
            .map(|v| {
                let f = p::decode(v)?;
                Ok(std::str::from_utf8(p::optional_data(&f, 1)?.unwrap_or_default())?.to_string())
            })
            .transpose()
    };
    for (n, name) in [
        (2, "nick_name"),
        (3, "pin_yin_initial"),
        (4, "quan_pin"),
        (10, "remark"),
        (11, "remark_pin_yin_initial"),
        (12, "remark_quan_pin"),
    ] {
        if let Some(v) = sk(n)? {
            out.insert(name.into(), SqlValue::Text(v));
        }
    }
    for (n, name) in [
        (30, "alias"),
        (39, "big_head_url"),
        (40, "small_head_url"),
        (45, "encrypt_username"),
    ] {
        if let Some(v) = p::optional_data(&fields, n)? {
            out.insert(
                name.into(),
                SqlValue::Text(std::str::from_utf8(v)?.to_string()),
            );
        }
    }
    if let Some(p::Value::Varint(v)) = p::optional(&fields, 24)? {
        out.insert(
            "verify_flag".into(),
            SqlValue::Integer(u32::try_from(*v)?.to_string()),
        );
    }
    if let (Some(p::Value::Varint(mask)), Some(p::Value::Varint(value))) =
        (p::optional(&fields, 7)?, p::optional(&fields, 8)?)
    {
        let flag = u32::try_from(*mask)? & u32::try_from(*value)?;
        out.insert("flag".into(), SqlValue::Integer(flag.to_string()));
        if flag & 1 != 0 {
            out.insert("local_type".into(), SqlValue::Integer("1".into()));
        } else if flag & 2 != 0 && record.username.ends_with("@chatroom") {
            out.insert("local_type".into(), SqlValue::Integer("2".into()));
        }
    }
    valid_name(&record.username)?;
    Ok(ContactProjection {
        username: record.username.clone(),
        fields: out,
    })
}
fn bounded_catalog_result(count: usize, pages: u32, coverage: &str) -> Value {
    json!({"recordCount":count,"pages":pages,"complete":false,"reason":"time_bound","coverage":coverage,"resumeAvailable":true,"profileProjection":"pending"})
}
impl Core {
    pub fn start_metadata(
        self: &Arc<Self>,
        account: &str,
        action: MetadataAction,
    ) -> Result<Operation> {
        match &action {
            MetadataAction::Contact { contact_id } | MetadataAction::Avatar { contact_id } => {
                valid_name(contact_id)?
            }
            MetadataAction::Group { group_id } | MetadataAction::Members { group_id, .. } => {
                valid_name(group_id)?;
                if !group_id.ends_with("@chatroom") {
                    bail!("invalid_group_identity")
                }
            }
            MetadataAction::ContactSync { max_pages } | MetadataAction::GroupSync { max_pages } => {
                if !(1..=50).contains(max_pages) {
                    bail!("metadata_page_limit")
                }
            }
        }
        self.account_state(account)?;
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let op = self.start_operation(account, "metadata", move |core, account| async move {
            let operation = rx.await.context("metadata_operation_interrupted")?;
            let _guard = core.lock_account(&account).await?;
            blocking(move || core.run_metadata(&account, &operation, action)).await
        })?;
        let _ = tx.send(op.id.clone());
        Ok(op)
    }
    fn metadata_check(&self, account: &str, operation: &str, deadline: Instant) -> Result<()> {
        if self.stopping.load(Ordering::Acquire) || Instant::now() >= deadline {
            bail!("metadata_time_bound")
        }
        if self
            .cancels
            .lock()
            .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?
            .get(operation)
            .is_some_and(|f| f.load(Ordering::Acquire))
        {
            bail!("metadata_cancelled")
        };
        self.account_state(account)?;
        Ok(())
    }
    fn stage_metadata_body(&self, account: &str, body: ReceiptBody) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = serde_json::to_vec(&MetadataReceipt { body })?;
        self.store.stage_metadata(
            account,
            &id,
            &self
                .vault
                .seal(account, &format!("metadata:{id}"), &bytes)?,
        )?;
        Ok(id)
    }
    fn metadata_receipt(&self, account: &str, id: &str) -> Result<MetadataReceipt> {
        let rows = self.store.pending_metadata(account)?;
        let (_, sealed) = rows
            .into_iter()
            .find(|(key, _)| key == id)
            .context("metadata_receipt_missing")?;
        Ok(serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("metadata:{id}"),
            &sealed,
        )?)?)
    }
    fn apply_metadata_receipt(
        &self,
        account: &str,
        id: &str,
        finish_catalog: bool,
    ) -> Result<usize> {
        let receipt = self.metadata_receipt(account, id)?;
        if !finish_catalog && matches!(receipt.body, ReceiptBody::ContactCatalog(_)) {
            return Ok(0);
        }
        let options = self.write_options(account, id)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let mut cursor: Option<(&str, Vec<u8>)> = None;
        let mut count = 0;
        let mut apply = |record: &ContactRecord| -> Result<()> {
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &contact_patch(record)?,
                &options,
            )?;
            if record.username.ends_with("@chatroom") {
                // Explicit native group metadata supplies catalog identity.
                // Unknown membership/owner/opaque ext fields stay SQL NULL on
                // new rows and unchanged on existing rows; no thread is made.
                wechat_profile::apply_group_patch(
                    &location,
                    &keys,
                    &wechat_profile::GroupProjection {
                        username: record.username.clone(),
                        fields: RawFields::new(),
                        detail_fields: None,
                        members: vec![],
                        complete_roster: false,
                    },
                    &options,
                )?;
            }
            count += 1;
            Ok(())
        };
        let kind = match receipt.body {
            ReceiptBody::Contacts { requested, reply } => {
                for record in &reply.value.contacts {
                    let index = requested
                        .iter()
                        .position(|id| id == &record.username)
                        .context("unrequested_metadata_identity")?;
                    if reply.value.results.is_empty() || reply.value.results.get(index) == Some(&0)
                    {
                        apply(record)?;
                    }
                }
                "contacts"
            }
            ReceiptBody::GroupCatalog(page) => {
                for record in &page.contacts {
                    apply(record)?;
                }
                if let Some(next) = page.next_cursor {
                    cursor = Some((
                        "metadata-groups-cursor",
                        self.vault.seal(account, "metadata-groups-cursor", &next)?,
                    ));
                }
                "group_catalog"
            }
            ReceiptBody::ContactCatalog(reply) => {
                cursor = Some((
                    "metadata-contacts-cursor",
                    self.vault.seal(
                        account,
                        "metadata-contacts-cursor",
                        &serde_json::to_vec(&(
                            reply.value.contact_sequence,
                            reply.value.room_sequence,
                        ))?,
                    )?,
                ));
                "contact_catalog"
            }
            ReceiptBody::Roster { reply, complete } => {
                self.project_metadata_roster(&location, &keys, &reply, &options, complete)?;
                count = reply.value.members.len();
                "members"
            }
            ReceiptBody::Avatar {
                username,
                source_url,
                bytes,
            } => {
                wechat_protocol::media::inspect_image(&bytes)?;
                wechat_profile::apply_avatar_projection(
                    &location,
                    &keys,
                    &wechat_profile::AvatarProjection {
                        username: &username,
                        source_url_md5: &wechat_protocol::crypto::md5(source_url.as_bytes())
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>(),
                        bytes: &bytes,
                        update_time: wechat_protocol::wire::unix_time()? as i64,
                    },
                    &options,
                )?;
                count = 1;
                "avatar"
            }
        };
        self.store.finish_metadata(
            account,
            id,
            cursor.as_ref().map(|(k, v)| (*k, v.as_slice())),
            &json!({"receiptId":id,"kind":kind,"recordCount":count,"profileProjection":"complete"}),
        )?;
        Ok(count)
    }
    pub fn recover_metadata(&self, account: &str) -> Result<usize> {
        let pending = self.store.pending_metadata(account)?;
        if pending.is_empty() {
            return Ok(0);
        }
        if self.account_state(account).is_err()
            || self.write_options(account, "metadata-recovery").is_err()
        {
            return Ok(0);
        };
        let mut n = 0;
        for (id, _) in pending {
            let Ok(receipt) = self.metadata_receipt(account, &id) else {
                continue;
            };
            if matches!(receipt.body, ReceiptBody::ContactCatalog(_)) {
                continue;
            }
            if self.apply_metadata_receipt(account, &id, false).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
    fn metadata_contact_lookup(&self, account: &str, ids: &[String]) -> Result<(usize, usize)> {
        let mut state = self.account_state(account)?;
        let native = client()?;
        let reply = match native.lookup_contacts_receipt(&mut state, ids) {
            Ok(r) => r,
            Err(e) if auth_rejected(&e) => {
                state = native.renew_session(&state, "agent-wechat metadata")?;
                native.lookup_contacts_receipt(&mut state, ids)?
            }
            Err(e) => return Err(e),
        };
        self.save_state(account, &state)?;
        let failed = reply.value.results.iter().filter(|n| **n != 0).count().max(
            ids.iter()
                .filter(|id| !reply.value.contacts.iter().any(|r| &r.username == *id))
                .count(),
        );
        let id = self.stage_metadata_body(
            account,
            ReceiptBody::Contacts {
                requested: ids.to_vec(),
                reply,
            },
        )?;
        Ok((self.apply_metadata_receipt(account, &id, false)?, failed))
    }
    fn run_metadata(
        &self,
        account: &str,
        operation: &str,
        action: MetadataAction,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(120);
        self.metadata_check(account, operation, deadline)?;
        self.recover_metadata(account)?;
        match action {
            MetadataAction::Contact { contact_id }
            | MetadataAction::Group {
                group_id: contact_id,
            } => {
                let (n, failed) = self.metadata_contact_lookup(account, &[contact_id])?;
                Ok(
                    json!({"recordCount":n,"failedItems":failed,"profileProjection":"complete","coverage":"known_identity"}),
                )
            }
            MetadataAction::Members { group_id, version } => {
                self.metadata_contact_lookup(account, std::slice::from_ref(&group_id))?;
                let mut state = self.account_state(account)?;
                let reply = client()?.lookup_room_receipt(&mut state, &group_id, version)?;
                self.save_state(account, &state)?;
                let fields = p::decode(&reply.raw)?;
                let members = p::decode(p::data(&fields, 4)?)?;
                let complete =
                    version == 0 && matches!(p::optional(&members, 1)?, Some(p::Value::Varint(_)));
                let id =
                    self.stage_metadata_body(account, ReceiptBody::Roster { reply, complete })?;
                let count = self.apply_metadata_receipt(account, &id, false)?;
                Ok(
                    json!({"memberCount":count,"profileProjection":"complete","completeRoster":complete}),
                )
            }
            MetadataAction::Avatar { contact_id } => {
                let mut state = self.account_state(account)?;
                let reply = client()?
                    .lookup_contacts_receipt(&mut state, std::slice::from_ref(&contact_id))?;
                self.save_state(account, &state)?;
                let source = reply
                    .value
                    .contacts
                    .iter()
                    .find(|r| r.username == contact_id)
                    .and_then(|r| r.small_avatar_url.clone())
                    .filter(|s| !s.is_empty())
                    .context("avatar_not_advertised")?;
                let id = self.stage_metadata_body(
                    account,
                    ReceiptBody::Contacts {
                        requested: vec![contact_id.clone()],
                        reply,
                    },
                )?;
                self.apply_metadata_receipt(account, &id, false)?;
                self.metadata_check(account, operation, deadline)?;
                let bytes = wechat_protocol::media::download_advertised_avatar(&source)?;
                let id = self.stage_metadata_body(
                    account,
                    ReceiptBody::Avatar {
                        username: contact_id,
                        source_url: source,
                        bytes,
                    },
                )?;
                self.apply_metadata_receipt(account, &id, false)?;
                Ok(
                    json!({"recordCount":1,"profileProjection":"complete","representation":"advertised_small"}),
                )
            }
            MetadataAction::ContactSync { max_pages } => {
                self.sync_metadata_contacts(account, operation, max_pages, deadline)
            }
            MetadataAction::GroupSync { max_pages } => {
                self.sync_metadata_groups(account, operation, max_pages, deadline)
            }
        }
    }
    fn sync_metadata_contacts(
        &self,
        account: &str,
        operation: &str,
        max_pages: u32,
        deadline: Instant,
    ) -> Result<Value> {
        let mut sequences = match self.store.runtime(account, "metadata-contacts-cursor")? {
            Some(v) => serde_json::from_slice::<(u32, u32)>(&self.vault.open_record(
                account,
                "metadata-contacts-cursor",
                &v,
            )?)?,
            None => (0, 0),
        };
        let mut count = 0;
        for page_index in 0..max_pages {
            if Instant::now() >= deadline {
                return Ok(bounded_catalog_result(
                    count,
                    page_index,
                    "contact_inventory",
                ));
            }
            self.metadata_check(account, operation, deadline)?;
            let mut pending = None;
            for (id, _) in self.store.pending_metadata(account)? {
                if let ReceiptBody::ContactCatalog(reply) =
                    self.metadata_receipt(account, &id)?.body
                {
                    pending = Some((id, reply));
                    break;
                }
            }
            let (id, reply) = if let Some(pending) = pending {
                pending
            } else {
                let mut state = self.account_state(account)?;
                let reply =
                    client()?.init_contacts_receipt(&mut state, sequences.0, sequences.1)?;
                self.save_state(account, &state)?;
                let id =
                    self.stage_metadata_body(account, ReceiptBody::ContactCatalog(reply.clone()))?;
                (id, reply)
            };
            let checkpoint_key = format!("metadata-hydrated:{id}");
            let mut done = match self.store.runtime(account, &checkpoint_key)? {
                Some(v) => serde_json::from_slice::<Vec<String>>(&self.vault.open_record(
                    account,
                    &checkpoint_key,
                    &v,
                )?)?,
                None => vec![],
            };
            let remaining = reply
                .value
                .usernames
                .iter()
                .filter(|name| !done.contains(name))
                .cloned()
                .collect::<Vec<_>>();
            for names in remaining.chunks(2) {
                if Instant::now() >= deadline {
                    return Ok(bounded_catalog_result(
                        count,
                        page_index + 1,
                        "contact_inventory",
                    ));
                }
                self.metadata_check(account, operation, deadline)?;
                let (n, failed) = self.metadata_contact_lookup(account, names)?;
                count += n;
                if failed > 0 {
                    return Ok(
                        json!({"recordCount":count,"pages":page_index+1,"complete":false,"reason":"partial_contact_results","profileProjection":"pending"}),
                    );
                }
                done.extend(names.iter().cloned());
                self.store.put_runtime(
                    account,
                    &checkpoint_key,
                    &self
                        .vault
                        .seal(account, &checkpoint_key, &serde_json::to_vec(&done)?)?,
                )?;
            }
            self.apply_metadata_receipt(account, &id, true)?;
            self.store.clear_runtime(account, &checkpoint_key)?;
            sequences = (reply.value.contact_sequence, reply.value.room_sequence);
            if !reply.value.more {
                return Ok(
                    json!({"recordCount":count,"pages":page_index+1,"complete":true,"coverage":"contact_inventory","profileProjection":"complete"}),
                );
            }
        }
        Ok(
            json!({"recordCount":count,"pages":max_pages,"complete":false,"reason":"page_bound","profileProjection":"complete"}),
        )
    }
    fn sync_metadata_groups(
        &self,
        account: &str,
        operation: &str,
        max_pages: u32,
        deadline: Instant,
    ) -> Result<Value> {
        let mut cursor = match self.store.runtime(account, "metadata-groups-cursor")? {
            Some(v) => self
                .vault
                .open_record(account, "metadata-groups-cursor", &v)?
                .to_vec(),
            None => vec![0],
        };
        let mut count = 0;
        for index in 0..max_pages {
            if Instant::now() >= deadline {
                return Ok(bounded_catalog_result(
                    count,
                    index,
                    "group_metadata_partial",
                ));
            }
            self.metadata_check(account, operation, deadline)?;
            let mut state = self.account_state(account)?;
            let page = client()?.group_inventory(&mut state, &cursor)?;
            self.save_state(account, &state)?;
            let more = page.more;
            let wait = page.wait_seconds;
            let next = page.next_cursor.clone();
            let id = self.stage_metadata_body(account, ReceiptBody::GroupCatalog(page))?;
            count += self.apply_metadata_receipt(account, &id, false)?;
            if !more {
                return Ok(
                    json!({"recordCount":count,"pages":index+1,"complete":true,"coverage":"group_metadata_partial","profileProjection":"complete","absentGroupsRemoved":false}),
                );
            }
            cursor = next.context("group_cursor_missing")?;
            if wait > 10 {
                return Ok(
                    json!({"recordCount":count,"pages":index+1,"complete":false,"reason":"server_wait_bound","waitSeconds":wait,"profileProjection":"complete"}),
                );
            }
            for _ in 0..wait * 10 {
                if Instant::now() >= deadline {
                    return Ok(bounded_catalog_result(
                        count,
                        index + 1,
                        "group_metadata_partial",
                    ));
                }
                self.metadata_check(account, operation, deadline)?;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(
            json!({"recordCount":count,"pages":max_pages,"complete":false,"reason":"page_bound","profileProjection":"complete"}),
        )
    }
    fn project_metadata_roster(
        &self,
        location: &wechat_profile::ProfileLocation,
        keys: &wechat_profile::KeyFile,
        reply: &MetadataReply<RoomRoster>,
        options: &wechat_profile::WriteOptions,
        complete: bool,
    ) -> Result<()> {
        wechat_profile::apply_contact_patch(
            location,
            keys,
            &ContactProjection {
                username: reply.value.room_id.clone(),
                fields: RawFields::new(),
            },
            options,
        )?;
        for member in &reply.value.members {
            let mut fields = RawFields::new();
            if let Some(nickname) = &member.nickname {
                fields.insert("nick_name".into(), SqlValue::Text(nickname.clone()));
            }
            wechat_profile::apply_contact_patch(
                location,
                keys,
                &ContactProjection {
                    username: member.username.clone(),
                    fields,
                },
                options,
            )?;
        }
        let patch = wechat_profile::NativeRosterPatch {
            username: reply.value.room_id.clone(),
            members: reply
                .value
                .members
                .iter()
                .map(|m| wechat_profile::NativeMemberPatch {
                    username: m.username.clone(),
                    display: m.room_display_name.clone(),
                })
                .collect(),
            complete_roster: complete,
        };
        wechat_profile::apply_native_roster_patch(location, keys, &patch, options)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_catalog_retains_explicit_resume_and_incomplete_projection() {
        let value = bounded_catalog_result(4, 1, "contact_inventory");
        assert_eq!(value["recordCount"], 4);
        assert_eq!(value["complete"], false);
        assert_eq!(value["resumeAvailable"], true);
        assert_eq!(value["profileProjection"], "pending");
    }
    #[test]
    fn metadata_action_contract_uses_tag_and_camel_case_fields() {
        use ts_rs::TS;
        let action: MetadataAction =
            serde_json::from_value(json!({"kind":"contact_sync","maxPages":2})).unwrap();
        assert!(matches!(
            action,
            MetadataAction::ContactSync { max_pages: 2 }
        ));
        let types = MetadataAction::decl();
        assert!(types.contains("contact_sync"));
        assert!(types.contains("maxPages"));
        assert!(!types.contains("max_pages"));
    }
    #[test]
    fn sparse_contacts_preserve_absence_and_explicit_empty() {
        let raw = p::message(&[
            p::bytes(1, &p::bytes(1, b"synthetic")),
            p::bytes(2, &p::bytes(1, b"")),
            p::uint(7, 3),
            p::uint(8, 1),
            p::uint(24, 4),
        ])
        .unwrap();
        let record = wechat_protocol::wire::decode_contact_record(&raw).unwrap();
        let patch = contact_patch(&record).unwrap();
        assert_eq!(patch.fields["nick_name"], SqlValue::Text(String::new()));
        assert!(!patch.fields.contains_key("remark"));
        assert_eq!(patch.fields["flag"], SqlValue::Integer("1".into()));
        assert_eq!(patch.fields["local_type"], SqlValue::Integer("1".into()));
    }
}
