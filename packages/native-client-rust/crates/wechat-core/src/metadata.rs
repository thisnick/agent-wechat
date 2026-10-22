//! Durable remote metadata refresh with sparse official-file projection.
use super::*;
use anyhow::ensure;
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
        #[serde(default)]
        profile_revision: Option<Vec<u8>>,
    },
    ContactCatalog(MetadataReply<InitContactPage>),
    GroupCatalog(GroupMetadataPage),
    GroupDetail {
        group_id: String,
        raw: Vec<u8>,
        #[serde(default)]
        profile_revision: Option<Vec<u8>>,
    },
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
#[derive(Serialize, Deserialize)]
struct ContactWireReceipt {
    requested: Vec<String>,
    raw: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogRejection {
    username: String,
    service_code: i32,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogHydration {
    schema: u32,
    settled: Vec<String>,
    rejections: Vec<CatalogRejection>,
}
struct ContactHydration {
    applied: usize,
    failed: usize,
    unresolved: usize,
    settled: Vec<String>,
    rejections: Vec<CatalogRejection>,
}
fn contact_hydration(
    requested: &[String],
    reply: &ContactLookup,
    applied: usize,
) -> ContactHydration {
    let mut out = ContactHydration {
        applied,
        failed: 0,
        unresolved: 0,
        settled: vec![],
        rejections: vec![],
    };
    for (index, username) in requested.iter().enumerate() {
        let present = reply
            .contacts
            .iter()
            .any(|record| &record.username == username);
        match reply.results.get(index).copied() {
            Some(0) | None if present => out.settled.push(username.clone()),
            Some(1) => {
                out.failed += 1;
                out.settled.push(username.clone());
                out.rejections.push(CatalogRejection {
                    username: username.clone(),
                    service_code: 1,
                });
            }
            _ => {
                out.failed += 1;
                out.unresolved += 1;
            }
        }
    }
    out
}
fn catalog_hydration(bytes: Option<&[u8]>, catalog: &[String]) -> Result<CatalogHydration> {
    let mut out = match bytes {
        Some(bytes) => match serde_json::from_slice::<Vec<String>>(bytes) {
            Ok(settled) => CatalogHydration {
                schema: 1,
                settled,
                rejections: vec![],
            },
            Err(_) => serde_json::from_slice(bytes)?,
        },
        None => CatalogHydration {
            schema: 1,
            ..Default::default()
        },
    };
    ensure!(
        out.schema == 1 && out.settled.len() <= catalog.len(),
        "invalid_metadata_hydration_checkpoint"
    );
    let mut settled = std::collections::HashSet::new();
    ensure!(
        out.settled
            .iter()
            .all(|name| catalog.contains(name) && settled.insert(name.clone())),
        "invalid_metadata_hydration_checkpoint"
    );
    let mut rejected = std::collections::HashSet::new();
    ensure!(
        out.rejections.iter().all(|r| r.service_code == 1
            && settled.contains(&r.username)
            && rejected.insert(r.username.clone())),
        "invalid_metadata_hydration_checkpoint"
    );
    // Keep stable serialization and the existing catalog order on replay.
    out.settled
        .sort_by_key(|name| catalog.iter().position(|candidate| candidate == name));
    Ok(out)
}
fn valid_name(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("invalid_metadata_identity")
    };
    Ok(())
}
pub(crate) fn contact_patch(record: &ContactRecord) -> Result<ContactProjection> {
    let fields = p::decode(&record.raw)?;
    let controlled = wechat_protocol::controlled_contact_fields(&record.raw)?;
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
        if controlled.contains(&n) {
            continue;
        }
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
        let mask = u32::try_from(*mask)?;
        if mask == u32::MAX && !controlled.contains(&8) {
            let flag = mask & u32::try_from(*value)?;
            out.insert("flag".into(), SqlValue::Integer(flag.to_string()));
            if flag & 1 != 0 {
                out.insert("local_type".into(), SqlValue::Integer("1".into()));
            } else if flag & 2 != 0 && record.username.ends_with("@chatroom") {
                out.insert("local_type".into(), SqlValue::Integer("2".into()));
            }
        }
    }
    for (field, name) in [(17, "chat_room_notify"), (85, "is_in_chat_room")] {
        if let Some(value) = p::optional(&fields, field)? {
            let p::Value::Varint(value) = value else {
                bail!("invalid_group_metadata");
            };
            out.insert(
                name.into(),
                SqlValue::Integer(u32::try_from(*value)?.to_string()),
            );
        }
    }
    valid_name(&record.username)?;
    Ok(ContactProjection {
        username: record.username.clone(),
        fields: out,
    })
}
fn contact_catalog_result(
    count: usize,
    pages: u32,
    catalog_complete: bool,
    unavailable: usize,
) -> Value {
    json!({"recordCount":count,"failedItems":unavailable,"pages":pages,"complete":catalog_complete&&unavailable==0,"catalogComplete":catalog_complete,"unavailableCount":unavailable,"reason":if !catalog_complete{json!("page_bound")}else if unavailable>0{json!("contact_lookup_unavailable")}else{Value::Null},"resumeAvailable":!catalog_complete||unavailable>0,"coverage":"contact_inventory","profileProjection":if unavailable==0{"complete"}else{"partial"}})
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
        let mut rejected_slots = 0;
        let mut apply = |record: &ContactRecord| -> Result<()> {
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &contact_patch(record)?,
                &options,
            )?;
            if record.username.ends_with("@chatroom") {
                self.mark_group_revision(account, &record.username, id)?;
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
                let hydration_key = format!("metadata-hydrated:{id}");
                if let Some(sealed) = self.store.runtime(account, &hydration_key)? {
                    let plain = self.vault.open_record(account, &hydration_key, &sealed)?;
                    let progress = catalog_hydration(Some(&plain), &reply.value.usernames)?;
                    rejected_slots = progress.rejections.len();
                    if rejected_slots > 0 {
                        let key = format!("metadata-unavailable:{id}");
                        self.store.put_runtime(
                            account,
                            &key,
                            &self.vault.seal(
                                account,
                                &key,
                                &serde_json::to_vec(&progress.rejections)?,
                            )?,
                        )?;
                    }
                }
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
            ReceiptBody::Roster {
                reply,
                complete,
                profile_revision,
            } => {
                let current = self.group_revision(account, &reply.value.room_id)?;
                ensure!(
                    current == profile_revision || current.as_deref() == Some(id.as_bytes()),
                    "group_projection_superseded"
                );
                self.project_metadata_roster(&location, &keys, &reply, &options, complete)?;
                self.mark_group_revision(account, &reply.value.room_id, id)?;
                self.store.append_event_once(account,&format!("group-members:{id}"),"group.updated",&json!({"groupId":reply.value.room_id,"source":"metadata_refresh","membersRefreshed":true,"profileProjection":"partial"}))?;
                count = reply.value.members.len();
                "members"
            }
            ReceiptBody::GroupDetail {
                group_id,
                raw,
                profile_revision,
            } => {
                let current = self.group_revision(account, &group_id)?;
                ensure!(
                    current == profile_revision || current.as_deref() == Some(id.as_bytes()),
                    "group_projection_superseded"
                );
                self.project_group_detail(account, &group_id, &raw, 0, id)?;
                self.mark_group_revision(account, &group_id, id)?;
                self.store.append_event_once(account,&format!("group-detail:{id}"),"group.updated",&json!({"groupId":group_id,"source":"metadata_refresh","profileProjection":"partial"}))?;
                count = 1;
                "group_detail"
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
            &json!({"receiptId":id,"kind":kind,"recordCount":count,"failedItems":rejected_slots,"profileProjection":if rejected_slots==0{"complete"}else{"partial"}}),
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
    fn retain_contact_wire(&self, account: &str, ids: &[String], raw: &[u8]) -> Result<String> {
        let key = format!("metadata-wire:{}", uuid::Uuid::new_v4());
        let body = ContactWireReceipt {
            requested: ids.to_vec(),
            raw: raw.to_vec(),
        };
        let sealed = self
            .vault
            .seal(account, &key, &serde_json::to_vec(&body)?)?;
        self.store.put_runtime(account, &key, &sealed)?;
        Ok(key)
    }
    fn metadata_contact_reply(
        &self,
        account: &str,
        ids: &[String],
    ) -> Result<MetadataReply<ContactLookup>> {
        let mut state = self.account_state(account)?;
        let native = self.upstream.native(NativeOptions::default())?;
        let capture =
            |state: &mut wechat_protocol::AccountState| -> Result<MetadataReply<ContactLookup>> {
                let raw = native.lookup_contacts_raw(state, ids)?;
                // Persist the verified body before any schema/identity decoding. A
                // decoder failure can be investigated offline without repeating RPC.
                self.retain_contact_wire(account, ids, &raw)?;
                let value = wechat_protocol::wire::decode_contacts(&raw, ids)?;
                self.save_state(account, state)?;
                Ok(MetadataReply { value, raw })
            };
        match capture(&mut state) {
            Ok(reply) => Ok(reply),
            Err(error) if auth_rejected(&error) => {
                state = native.renew_session(&state, "agent-wechat metadata")?;
                capture(&mut state)
            }
            Err(error) => Err(error),
        }
    }
    fn contact_unavailable(&self, account: &str) -> Result<Vec<CatalogRejection>> {
        const KEY: &str = "metadata-contacts-unavailable";
        let values = match self.store.runtime(account, KEY)? {
            Some(sealed) => serde_json::from_slice::<Vec<CatalogRejection>>(
                &self.vault.open_record(account, KEY, &sealed)?,
            )?,
            None => vec![],
        };
        let mut seen = std::collections::HashSet::new();
        ensure!(
            values.len() <= 10000
                && values.iter().all(|v| v.service_code == 1
                    && valid_name(&v.username).is_ok()
                    && seen.insert(v.username.clone())),
            "invalid_contact_unavailable_ledger"
        );
        Ok(values)
    }
    fn record_contact_hydration(&self, account: &str, outcome: &ContactHydration) -> Result<usize> {
        const KEY: &str = "metadata-contacts-unavailable";
        let mut ledger = self.contact_unavailable(account)?;
        // A successful explicit refresh clears an earlier rejection. Still
        // unavailable entries rotate behind other entries for the next pass.
        ledger.retain(|r| !outcome.settled.contains(&r.username));
        ledger.extend(outcome.rejections.clone());
        ensure!(ledger.len() <= 10000, "contact_unavailable_ledger_limit");
        self.store.put_runtime(
            account,
            KEY,
            &self
                .vault
                .seal(account, KEY, &serde_json::to_vec(&ledger)?)?,
        )?;
        Ok(ledger.len())
    }
    fn metadata_contact_lookup(&self, account: &str, ids: &[String]) -> Result<ContactHydration> {
        let reply = self.metadata_contact_reply(account, ids)?;
        let mut hydration = contact_hydration(ids, &reply.value, 0);
        let id = self.stage_metadata_body(
            account,
            ReceiptBody::Contacts {
                requested: ids.to_vec(),
                reply,
            },
        )?;
        hydration.applied = self.apply_metadata_receipt(account, &id, false)?;
        self.record_contact_hydration(account, &hydration)?;
        Ok(hydration)
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
            MetadataAction::Contact { contact_id } => {
                let outcome = self.metadata_contact_lookup(account, &[contact_id])?;
                Ok(
                    json!({"recordCount":outcome.applied,"failedItems":outcome.failed,"profileProjection":if outcome.failed==0{"complete"}else{"partial"},"coverage":"known_identity"}),
                )
            }
            MetadataAction::Group { group_id } => {
                let outcome =
                    self.metadata_contact_lookup(account, std::slice::from_ref(&group_id))?;
                let profile_revision = self.group_revision(account, &group_id)?;
                let epoch = self.store.runtime(account, "logout-epoch")?;
                let raw = self.group_read(account, epoch.as_deref(), |state| {
                    client()?.group_detail_raw(state, &group_id)
                })?;
                let id = self.stage_metadata_body(
                    account,
                    ReceiptBody::GroupDetail {
                        group_id,
                        raw,
                        profile_revision,
                    },
                )?;
                self.apply_metadata_receipt(account, &id, false)?;
                Ok(
                    json!({"recordCount":outcome.applied+1,"failedItems":outcome.failed,"profileProjection":"partial","coverage":"group_metadata"}),
                )
            }
            MetadataAction::Members { group_id, version } => {
                self.metadata_contact_lookup(account, std::slice::from_ref(&group_id))?;
                let mut state = self.account_state(account)?;
                let profile_revision = self.group_revision(account, &group_id)?;
                let reply = client()?.lookup_room_receipt(&mut state, &group_id, version)?;
                self.save_state(account, &state)?;
                let complete = reply.value.full_member_ids.is_some();
                let id = self.stage_metadata_body(
                    account,
                    ReceiptBody::Roster {
                        reply,
                        complete,
                        profile_revision,
                    },
                )?;
                let count = self.apply_metadata_receipt(account, &id, false)?;
                Ok(
                    json!({"memberCount":count,"profileProjection":"complete","completeRoster":complete}),
                )
            }
            MetadataAction::Avatar { contact_id } => {
                let reply =
                    self.metadata_contact_reply(account, std::slice::from_ref(&contact_id))?;
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
                crate::upstream::require_native(self.upstream.kind())?;
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
        // Explicit catalog passes retry at most one batch of earlier code1
        // rejections, then continue inventory. No permanent/deletion inference.
        let retry = self
            .contact_unavailable(account)?
            .into_iter()
            .take(2)
            .map(|r| r.username)
            .collect::<Vec<_>>();
        if !retry.is_empty() {
            self.metadata_check(account, operation, deadline)?;
            let outcome = self.metadata_contact_lookup(account, &retry)?;
            count += outcome.applied;
            if outcome.unresolved > 0 {
                return Ok(
                    json!({"recordCount":count,"failedItems":outcome.failed,"complete":false,"catalogComplete":false,"unavailableCount":self.contact_unavailable(account)?.len(),"reason":"unresolved_contact_results","resumeAvailable":true,"profileProjection":"partial"}),
                );
            }
        }
        let mut rejected_count = self.contact_unavailable(account)?.len();
        for page_index in 0..max_pages {
            if Instant::now() >= deadline {
                let mut result = bounded_catalog_result(count, page_index, "contact_inventory");
                result["failedItems"] = rejected_count.into();
                result["unavailableCount"] = rejected_count.into();
                result["catalogComplete"] = false.into();
                return Ok(result);
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
                let reply = self
                    .upstream
                    .native(NativeOptions::default())?
                    .init_contacts_receipt(&mut state, sequences.0, sequences.1)?;
                self.save_state(account, &state)?;
                let id =
                    self.stage_metadata_body(account, ReceiptBody::ContactCatalog(reply.clone()))?;
                (id, reply)
            };
            let checkpoint_key = format!("metadata-hydrated:{id}");
            let sealed_progress = self.store.runtime(account, &checkpoint_key)?;
            let plain_progress = sealed_progress
                .map(|sealed| self.vault.open_record(account, &checkpoint_key, &sealed))
                .transpose()?;
            let mut progress = catalog_hydration(
                plain_progress.as_deref().map(|v| v.as_slice()),
                &reply.value.usernames,
            )?;
            rejected_count = self.contact_unavailable(account)?.len();
            let remaining = reply
                .value
                .usernames
                .iter()
                .filter(|name| !progress.settled.contains(name))
                .cloned()
                .collect::<Vec<_>>();
            for names in remaining.chunks(2) {
                if Instant::now() >= deadline {
                    let mut result =
                        bounded_catalog_result(count, page_index + 1, "contact_inventory");
                    result["failedItems"] = rejected_count.into();
                    result["unavailableCount"] = rejected_count.into();
                    result["catalogComplete"] = false.into();
                    return Ok(result);
                }
                self.metadata_check(account, operation, deadline)?;
                let outcome = self.metadata_contact_lookup(account, names)?;
                count += outcome.applied;
                rejected_count = self.contact_unavailable(account)?.len();
                progress.settled.extend(outcome.settled);
                progress.rejections.extend(outcome.rejections);
                self.store.put_runtime(
                    account,
                    &checkpoint_key,
                    &self
                        .vault
                        .seal(account, &checkpoint_key, &serde_json::to_vec(&progress)?)?,
                )?;
                if outcome.unresolved > 0 {
                    return Ok(
                        json!({"recordCount":count,"failedItems":rejected_count+outcome.unresolved,"pages":page_index+1,"complete":false,"reason":"unresolved_contact_results","catalogComplete":false,"unavailableCount":rejected_count,"resumeAvailable":true,"profileProjection":"partial"}),
                    );
                }
            }
            self.apply_metadata_receipt(account, &id, true)?;
            self.store.clear_runtime(account, &checkpoint_key)?;
            sequences = (reply.value.contact_sequence, reply.value.room_sequence);
            if !reply.value.more {
                return Ok(contact_catalog_result(
                    count,
                    page_index + 1,
                    true,
                    rejected_count,
                ));
            }
        }
        Ok(contact_catalog_result(
            count,
            max_pages,
            false,
            rejected_count,
        ))
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
            let page = self
                .upstream
                .native(NativeOptions::default())?
                .group_inventory(&mut state, &cursor)?;
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
    pub(super) fn project_metadata_roster(
        &self,
        location: &wechat_profile::ProfileLocation,
        keys: &wechat_profile::KeyFile,
        reply: &MetadataReply<RoomRoster>,
        options: &wechat_profile::WriteOptions,
        _complete: bool,
    ) -> Result<()> {
        let decoded = wechat_protocol::wire::decode_room(&reply.raw, &reply.value.room_id)?;
        let value = &decoded;
        wechat_profile::apply_contact_patch(
            location,
            keys,
            &ContactProjection {
                username: value.room_id.clone(),
                fields: RawFields::new(),
            },
            options,
        )?;
        for member in &value.members {
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
        // f4 contains changed member rows. A separate, count-checked f6/f7
        // list establishes the whole roster, even when f4 is a delta.
        let mut members = value
            .members
            .iter()
            .map(|m| wechat_profile::NativeMemberPatch {
                flags: Some(m.flags),
                opaque: m.opaque.clone(),
                username: m.username.clone(),
                display: m.room_display_name.clone(),
            })
            .collect::<Vec<_>>();
        let complete = if let Some(ids) = &value.full_member_ids {
            let reader = wechat_profile::ProfileReader::open(
                location.clone(),
                keys.clone(),
                wechat_profile::ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    allow_plaintext: options.allow_plaintext,
                    ..Default::default()
                },
            )?;
            let mut old = std::collections::BTreeSet::new();
            let mut cursor = None;
            loop {
                let page = match reader.group_members(
                    &value.room_id,
                    &wechat_profile::ListQuery {
                        limit: Some(200),
                        cursor,
                        search: None,
                    },
                ) {
                    Ok(page) => page,
                    Err(wechat_profile::ProfileError::NotCached) => break,
                    Err(error) => return Err(error.into()),
                };
                old.extend(page.items.into_iter().map(|m| m.username));
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            ensure!(
                ids.iter()
                    .all(|id| old.contains(id) || members.iter().any(|m| &m.username == id)),
                "group_roster_base_incomplete"
            );
            members = ids
                .iter()
                .map(|id| {
                    members
                        .iter()
                        .find(|m| &m.username == id)
                        .cloned()
                        .unwrap_or(wechat_profile::NativeMemberPatch {
                            flags: None,
                            opaque: None,
                            username: id.clone(),
                            display: None,
                        })
                })
                .collect();
            true
        } else {
            false
        };
        let patch = wechat_profile::NativeRosterPatch {
            username: value.room_id.clone(),
            members,
            complete_roster: complete,
        };
        wechat_profile::apply_native_roster_patch(location, keys, &patch, options)?;
        if let Some(owner) = &value.owner {
            let mut fields = RawFields::new();
            fields.insert("owner".into(), SqlValue::Text(owner.clone()));
            wechat_profile::apply_group_patch(
                location,
                keys,
                &wechat_profile::GroupProjection {
                    username: value.room_id.clone(),
                    fields,
                    detail_fields: None,
                    members: vec![],
                    complete_roster: false,
                },
                options,
            )?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn synthetic_record(name: &str) -> ContactRecord {
        wechat_protocol::wire::decode_contact_record(&p::bytes(1, &p::bytes(1, name.as_bytes())))
            .unwrap()
    }
    #[test]
    fn recognized_rejection_settles_only_current_pass_and_unknown_results_remain_pending() {
        let requested = vec!["unavailable".into(), "available".into()];
        let reply = ContactLookup {
            contacts: vec![synthetic_record("available")],
            results: vec![1, 0],
        };
        let outcome = contact_hydration(&requested, &reply, 1);
        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.unresolved, 0);
        assert_eq!(outcome.settled, requested);
        assert_eq!(outcome.rejections[0].service_code, 1);
        let reply = ContactLookup {
            contacts: vec![synthetic_record("available")],
            results: vec![2, 0],
        };
        let outcome = contact_hydration(&requested, &reply, 1);
        assert_eq!(outcome.unresolved, 1);
        assert_eq!(outcome.settled, vec!["available"]);
        assert!(outcome.rejections.is_empty());
    }
    #[test]
    fn catalog_checkpoint_accepts_legacy_progress_and_preserves_explicit_rejections() {
        let catalog = vec!["one".into(), "two".into()];
        let legacy = catalog_hydration(Some(br#"["one"]"#), &catalog).unwrap();
        assert_eq!(legacy.settled, vec!["one"]);
        assert!(legacy.rejections.is_empty());
        let mut progress = legacy;
        progress.settled.push("two".into());
        progress.rejections.push(CatalogRejection {
            username: "two".into(),
            service_code: 1,
        });
        let bytes = serde_json::to_vec(&progress).unwrap();
        let restored = catalog_hydration(Some(&bytes), &catalog).unwrap();
        assert_eq!(restored.rejections.len(), 1);
        progress.rejections[0].service_code = 2;
        assert!(
            catalog_hydration(Some(&serde_json::to_vec(&progress).unwrap()), &catalog).is_err()
        );
        assert!(catalog_hydration(Some(br#"["foreign"]"#), &catalog).is_err());
    }
    #[test]
    fn catalog_drain_is_separate_from_successful_contact_hydration() {
        let result = contact_catalog_result(99, 1, true, 1);
        assert_eq!(result["catalogComplete"], true);
        assert_eq!(result["complete"], false);
        assert_eq!(result["unavailableCount"], 1);
        assert_eq!(result["profileProjection"], "partial");
        assert_eq!(result["resumeAvailable"], true);
        assert_eq!(contact_catalog_result(100, 1, true, 0)["complete"], true);
    }
    #[test]
    fn malformed_raw_contact_receipt_and_rejection_rotation_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Arc::new(wechat_store::Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let ids = vec!["synthetic".into()];
        let raw = b"synthetic malformed contact response";
        let key = core.retain_contact_wire("one", &ids, raw).unwrap();
        assert!(wechat_protocol::wire::decode_contacts(raw, &ids).is_err());
        let failed = ContactHydration {
            applied: 0,
            failed: 3,
            unresolved: 0,
            settled: vec!["a".into(), "b".into(), "c".into()],
            rejections: ["a", "b", "c"]
                .into_iter()
                .map(|username| CatalogRejection {
                    username: username.into(),
                    service_code: 1,
                })
                .collect(),
        };
        core.record_contact_hydration("one", &failed).unwrap();
        let retry = contact_hydration(
            &["a".into()],
            &ContactLookup {
                contacts: vec![],
                results: vec![1],
            },
            0,
        );
        core.record_contact_hydration("one", &retry).unwrap();
        assert_eq!(
            core.contact_unavailable("one")
                .unwrap()
                .iter()
                .map(|r| r.username.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
        let success = contact_hydration(
            &["b".into()],
            &ContactLookup {
                contacts: vec![synthetic_record("b")],
                results: vec![0],
            },
            1,
        );
        core.record_contact_hydration("one", &success).unwrap();
        drop(core);
        let store = Arc::new(wechat_store::Store::open_existing(&root).unwrap());
        let core = Core::new(store, None).unwrap();
        let sealed = core.store.runtime("one", &key).unwrap().unwrap();
        assert!(!sealed.windows(raw.len()).any(|w| w == raw));
        let body: ContactWireReceipt =
            serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap()).unwrap();
        assert_eq!(body.requested, ids);
        assert_eq!(body.raw, raw);
        assert_eq!(
            core.contact_unavailable("one")
                .unwrap()
                .iter()
                .map(|r| r.username.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "a"]
        );
    }
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
            p::uint(7, u64::from(u32::MAX)),
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
    #[test]
    fn partial_masks_do_not_replace_flags_or_hide_independent_group_fields() {
        let raw = p::message(&[
            p::bytes(1, &p::bytes(1, b"synthetic@chatroom")),
            p::uint(7, 3),
            p::uint(8, 1),
            p::uint(17, 0),
            p::uint(85, 2),
        ])
        .unwrap();
        let record = wechat_protocol::wire::decode_contact_record(&raw).unwrap();
        let patch = contact_patch(&record).unwrap();
        assert!(!patch.fields.contains_key("flag"));
        assert!(!patch.fields.contains_key("local_type"));
        assert_eq!(
            patch.fields["chat_room_notify"],
            SqlValue::Integer("0".into())
        );
        assert_eq!(
            patch.fields["is_in_chat_room"],
            SqlValue::Integer("2".into())
        );
    }
}
