//! One-attempt group mutations. Repair only consumes sealed replies; it never
//! submits a mutation, fetches metadata or starts an upload.
use super::*;
use anyhow::ensure;
use wechat_profile::{
    ContactProjection, GroupProjection, NativeGroupEdit, NativeMemberPatch, NativeRosterPatch,
    RawFields, SqlValue,
};
use wechat_protocol::protobuf as p;
use wechat_protocol::{
    decode_group_mutation, group_mutation_requests, observed_created_group, GroupMutationReply,
    GroupRpc,
};
use wechat_store::StoredGroupMutation;

#[derive(Serialize, Deserialize)]
struct Attempt {
    action: GroupMutation,
    requests: Vec<GroupRpc>,
    own: String,
    uin: u32,
    generation: String,
    owner: String,
    epoch: Option<Vec<u8>>,
    #[serde(default)]
    profile_revision: Option<Vec<u8>>,
    #[serde(default)]
    pin_clear_folded_primary: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Receipts {
    replies: Vec<Vec<u8>>,
    #[serde(default)]
    pin_projection_time: Option<u32>,
}
fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn purpose(id: &str, kind: &str) -> String {
    format!("group-mutation:{id}:{kind}")
}
fn requested(action: &GroupMutation) -> &[String] {
    match action {
        GroupMutation::Create { members, .. }
        | GroupMutation::AddMembers { members, .. }
        | GroupMutation::RemoveMembers { members, .. } => members,
        _ => &[],
    }
}
fn action_name(action: &GroupMutation) -> &'static str {
    match action {
        GroupMutation::Create { .. } => "create",
        GroupMutation::Rename { .. } => "rename",
        GroupMutation::Leave { .. } => "leave",
        GroupMutation::AddMembers { .. } => "add_members",
        GroupMutation::RemoveMembers { .. } => "remove_members",
        GroupMutation::MyName { .. } => "my_name",
        GroupMutation::ClearAnnouncement { .. } => "clear_announcement",
        GroupMutation::Mute { .. } => "mute",
        GroupMutation::Pin { .. } => "pin",
    }
}
fn integer(fields: &RawFields, name: &str) -> Option<i64> {
    match fields.get(name) {
        Some(SqlValue::Integer(n)) => n.parse().ok(),
        _ => None,
    }
}

impl Core {
    pub(super) fn group_revision(&self, account: &str, group: &str) -> Result<Option<Vec<u8>>> {
        self.store.runtime(
            account,
            &format!("group-revision:{:x}", Sha256::digest(group.as_bytes())),
        )
    }
    pub(super) fn mark_group_revision(
        &self,
        account: &str,
        group: &str,
        revision: &str,
    ) -> Result<()> {
        self.store.put_runtime(
            account,
            &format!("group-revision:{:x}", Sha256::digest(group.as_bytes())),
            revision.as_bytes(),
        )
    }
    pub fn group_info(&self, account: &str, id: &str) -> Result<Value> {
        let mut value = self.catalog_item(account, "groups", id)?;
        let contact = self.group_contact(account, id)?;
        let flag = integer(&contact.raw_fields, "flag");
        value["name"] = json!(contact.nickname);
        value["remark"] = json!(contact.remark);
        value["muted"] = json!(integer(&contact.raw_fields, "chat_room_notify").and_then(
            |n| match n {
                0 => Some(true),
                1 => Some(false),
                _ => None,
            }
        ));
        value["minimized"] = json!(flag.map(|f| f & 0x10000000 != 0));
        value["isInGroup"] = json!(
            integer(&contact.raw_fields, "is_in_chat_room").and_then(|n| match n {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            })
        );
        value["source"] = json!("cached");
        value["rosterCoverage"] = json!("partial");
        if let Some(own) = self.profile_username(account)? {
            let member = self.group_member(account, id, &own)?;
            value["myDisplayName"] = member["member"]["roomDisplayName"].clone();
        }
        Ok(value)
    }
    fn group_contact(&self, account: &str, id: &str) -> Result<wechat_profile::Contact> {
        let reader = self.profile_reader_owned(account)?;
        let mut cursor = None;
        loop {
            let page = reader.contacts(&wechat_profile::ListQuery {
                search: Some(id.into()),
                limit: Some(200),
                cursor,
            })?;
            if let Some(contact) = page.items.into_iter().find(|c| c.username == id) {
                return Ok(contact);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                bail!("group_contact_unavailable");
            }
        }
    }
    fn load_group_attempt(&self, record: &StoredGroupMutation) -> Result<(Attempt, Receipts)> {
        let op = &record.operation;
        let attempt: Attempt = serde_json::from_slice(&self.vault.open_record(
            &op.account_id,
            &purpose(&op.id, "request"),
            &record.encrypted_request,
        )?)?;
        let receipts: Receipts = serde_json::from_slice(&self.vault.open_record(
            &op.account_id,
            &purpose(&op.id, "receipts"),
            &record.encrypted_receipts,
        )?)?;
        attempt.action.validate(&attempt.own)?;
        ensure!(
            op.kind == "group_mutation"
                && op.result["action"] == action_name(&attempt.action)
                && receipts.replies.len() <= attempt.requests.len()
                && attempt.requests.len() <= 3,
            "group_operation_identity_conflict"
        );
        Ok((attempt, receipts))
    }
    /// The account actor covers admission, current contact state and the worker.
    pub async fn mutate_group(
        self: &Arc<Self>,
        account: &str,
        action: GroupMutation,
        key: &str,
    ) -> Result<Operation> {
        if self.stopping.load(Ordering::Acquire) {
            bail!("service_stopping");
        }
        let (op, created, guard) = self.admit_group_guarded(account, action, key).await?;
        if created {
            let worker_account = account.to_owned();
            let core = self.clone();
            let id = op.id.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let worker = core.clone();
                    let account = worker_account.clone();
                    let operation = id.clone();
                    blocking(move || {
                        let _guard = guard;
                        worker.execute_group(&account, &operation, |state, rpc| {
                            worker.upstream.group_mutation_raw(state, rpc)
                        })?;
                        Ok(())
                    })
                    .await
                }
                .await;
                if result.is_err() {
                    if let Ok(guard) = core.lock_account(&worker_account).await {
                        let _ = blocking(move || {
                            let _guard = guard;
                            core.hold_group(&worker_account, &id)?;
                            Ok(())
                        })
                        .await;
                    }
                }
            });
        }
        Ok(op)
    }
    async fn admit_group_guarded(
        self: &Arc<Self>,
        account: &str,
        action: GroupMutation,
        key: &str,
    ) -> Result<(Operation, bool, tokio::sync::OwnedMutexGuard<()>)> {
        let account = account.to_owned();
        let key = key.to_owned();
        let core = self.clone();
        let guard = self.lock_account(&account).await?;
        blocking(move || {
            let (operation, created) = core.admit_group(&account, action, &key)?;
            // Carry ownership into the worker instead of leaving a heartbeat-sized
            // gap between admission and the operation's credential-generation check.
            Ok((operation, created, guard))
        })
        .await
    }
    fn admit_group(
        &self,
        account: &str,
        action: GroupMutation,
        key: &str,
    ) -> Result<(Operation, bool)> {
        let hash = digest(&serde_json::to_vec(&action)?);
        if let Some(prior) = self
            .store
            .group_mutation_by_key(account, key, Some(&hash))?
        {
            return Ok((prior.operation, false));
        }
        let mut state = self.account_state(account)?;
        action.validate(&state.own_username)?;
        let mut minimized = false;
        if let Some(target) = action.target() {
            let contact = self.group_contact(account, target)?;
            if matches!(
                action,
                GroupMutation::Mute { .. } | GroupMutation::Pin { .. }
            ) && !target.ends_with("@chatroom")
            {
                ensure!(
                    !contact.is_group
                        && contact.local_type == Some(1)
                        && integer(&contact.raw_fields, "delete_flag") == Some(0)
                        && integer(&contact.raw_fields, "flag").is_some_and(|flag| u32::try_from(
                            flag
                        )
                        .is_ok()
                            && flag & 1 != 0),
                    "ordinary_contact_required"
                );
            } else {
                ensure!(
                    contact.is_group
                        && contact.local_type == Some(2)
                        && integer(&contact.raw_fields, "delete_flag") == Some(0)
                        && integer(&contact.raw_fields, "is_in_chat_room") == Some(1),
                    "ordinary_group_required"
                );
                let flag =
                    integer(&contact.raw_fields, "flag").context("group_state_incomplete")?;
                minimized = flag & 0x10000000 != 0;
            }
            if matches!(action, GroupMutation::Pin { .. }) {
                let reader = self.profile_reader_owned(account)?;
                ensure!(
                    reader
                        .thread_pinned(&[target.to_owned()])?
                        .get(target)
                        .copied()
                        .flatten()
                        .is_some(),
                    "ordinary_thread_required"
                );
                ensure!(reader.thread_exists(target)?, "thread_session_unavailable");
            }
        }
        if matches!(
            action,
            GroupMutation::Create { .. } | GroupMutation::AddMembers { .. }
        ) {
            for member in requested(&action) {
                let contact = self.group_contact(account, member)?;
                ensure!(
                    !contact.is_group
                        && contact.local_type == Some(1)
                        && integer(&contact.raw_fields, "flag").is_some_and(|f| f & 1 != 0)
                        && integer(&contact.raw_fields, "delete_flag") == Some(0),
                    "ordinary_contact_required"
                );
            }
        }
        let mut pin_clear_folded_primary = false;
        let requests = if let GroupMutation::Pin { thread_id, pinned } = &action {
            let reply = self.metadata_contact_reply(account, std::slice::from_ref(thread_id))?;
            ensure!(
                reply.value.contacts.len() == 1
                    && reply.value.results.iter().all(|result| *result == 0),
                "contact_snapshot_unavailable"
            );
            let record = &reply.value.contacts[0];
            let requests = wechat_protocol::contact_pinning_requests(record, *pinned)
                .context("thread_pinning_unavailable")?;
            let fields = wechat_protocol::protobuf::decode(&record.raw)?;
            pin_clear_folded_primary = *pinned
                && if thread_id.ends_with("@chatroom") {
                    wechat_protocol::protobuf::scalar(&fields, 8)? & 0x10000000 != 0
                } else {
                    wechat_protocol::protobuf::scalar(&fields, 83)? & 0x80 != 0
                };
            state = self.account_state(account)?;
            requests
        } else if let GroupMutation::Mute { thread_id, muted } = &action {
            if !thread_id.ends_with("@chatroom") {
                let reply =
                    self.metadata_contact_reply(account, std::slice::from_ref(thread_id))?;
                ensure!(
                    reply.value.contacts.len() == 1
                        && reply.value.results.iter().all(|result| *result == 0),
                    "contact_snapshot_unavailable"
                );
                let request = wechat_protocol::contact_notification_request(
                    &reply.value.contacts[0],
                    *muted,
                )?;
                state = self.account_state(account)?;
                vec![request]
            } else {
                group_mutation_requests(&state, &action, minimized)?
            }
        } else {
            group_mutation_requests(&state, &action, minimized)?
        };
        let cipher = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let epoch = self.store.runtime(account, "logout-epoch")?;
        ensure!(
            self.store
                .saved_login_authorization_current(account, &cipher, epoch.as_deref())?
                && self.store.runtime(account, "desired")?.as_deref() == Some(b"connected")
                && self
                    .store
                    .runtime(account, "validated-generation")?
                    .as_deref()
                    == Some(self.ownership_generation.as_bytes()),
            "auth_required"
        );
        let mut result = json!({"action":action_name(&action),"targetId":action.target(),"groupId":action.target(),"remoteOutcome":"not_attempted","steps":[],"members":[],"unconfirmedMembers":requested(&action),"profileProjection":"pending","retryable":false});
        result["projectionGaps"] = json!([
            "local_notices_and_search_indexes_not_reconstructed",
            "unreturned_member_state_unconfirmed"
        ]);
        // Thread notification operations share the guarded one-attempt journal.
        if let GroupMutation::Mute { thread_id, .. } | GroupMutation::Pin { thread_id, .. } =
            &action
        {
            result["threadId"] = json!(thread_id);
            if !thread_id.ends_with("@chatroom") {
                result["groupId"] = Value::Null;
            }
        }
        if let GroupMutation::Pin { pinned, .. } = &action {
            result["pinned"] = json!(pinned);
            result["projectionGaps"] = json!([]);
            result["crossDatabaseAtomic"] = json!(false);
        }
        let stamp = now();
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.into(),
            kind: "group_mutation".into(),
            state: "queued".into(),
            result,
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let profile_revision = action
            .target()
            .map(|target| self.group_revision(account, target))
            .transpose()?
            .flatten();
        let attempt = Attempt {
            action,
            requests,
            own: state.own_username.clone(),
            uin: state.uin,
            generation: digest(&cipher),
            owner: self.ownership_generation.clone(),
            epoch,
            profile_revision,
            pin_clear_folded_primary,
        };
        self.store.create_group_mutation(
            &op,
            key,
            &hash,
            &self.vault.seal(
                account,
                &purpose(&op.id, "request"),
                &serde_json::to_vec(&attempt)?,
            )?,
            &self.vault.seal(
                account,
                &purpose(&op.id, "receipts"),
                &serde_json::to_vec(&Receipts::default())?,
            )?,
        )
    }
    fn execute_group<F>(&self, account: &str, id: &str, mut remote: F) -> Result<Operation>
    where
        F: FnMut(&mut AccountState, &GroupRpc) -> Result<Vec<u8>>,
    {
        let record = self.store.group_mutation(account, id)?;
        if record.phase != "queued" {
            return Ok(record.operation);
        }
        let (attempt, mut receipts) = self.load_group_attempt(&record)?;
        let mut op = record.operation;
        let preflight = (|| -> Result<(AccountState, Vec<u8>)> {
            let state = self.account_state(account)?;
            let cipher = self
                .store
                .runtime(account, "account-state")?
                .context("auth_required")?;
            ensure!(
                attempt.owner == self.ownership_generation
                    && digest(&cipher) == attempt.generation
                    && state.own_username == attempt.own
                    && state.uin == attempt.uin,
                "authorization_generation_changed"
            );
            Ok((state, cipher))
        })();
        let (mut state, mut expected) = match preflight {
            Ok(value) => value,
            Err(_) => {
                op.state = "failed".into();
                op.result["code"] = json!("group_preflight_failed");
                op.updated_at = now();
                self.store.advance_group_mutation(&op, "failed", 0)?;
                return Ok(op);
            }
        };
        for (step, rpc) in attempt.requests.iter().enumerate() {
            if self.stopping.load(Ordering::Acquire) {
                return self.hold_group(account, id);
            }
            match self.store.begin_group_step(
                account,
                id,
                step,
                &expected,
                attempt.epoch.as_deref(),
                &attempt.owner,
            ) {
                Ok(true) => {}
                Ok(false) => return Ok(self.store.group_mutation(account, id)?.operation),
                Err(_) => return self.hold_group(account, id),
            }
            let raw = match remote(&mut state, rpc) {
                Ok(raw) => raw,
                Err(_) => return self.hold_group(account, id),
            };
            receipts.replies.push(raw);
            if matches!(attempt.action, GroupMutation::Pin { .. }) && rpc.opcode == Some(2) {
                receipts.pin_projection_time = Some(u32::try_from(wechat_store::unix_seconds())?);
            }
            self.store.receive_group_step(
                account,
                id,
                step,
                &self.vault.seal(
                    account,
                    &purpose(id, "receipts"),
                    &serde_json::to_vec(&receipts)?,
                )?,
            )?;
            let raw = &receipts.replies[step];
            if rpc.cgi == 119 {
                if let Some(group) = observed_created_group(raw) {
                    op.result["groupId"] = json!(group);
                }
            }
            let reply = match decode_group_mutation(rpc, raw, requested(&attempt.action)) {
                Ok(reply) if !reply.uncertain => reply,
                _ => {
                    return self.finish_group_review(
                        op,
                        &attempt,
                        &receipts,
                        step,
                        "group_reply_unverified",
                    )
                }
            };
            self.summarize_group_reply(&mut op, rpc, &reply);
            if matches!(rpc.cgi, 119 | 120 | 179) {
                op.result["unconfirmedMembers"] = json!(requested(&attempt.action)
                    .iter()
                    .filter(|name| !reply.members.iter().any(|m| &m.username == *name))
                    .collect::<Vec<_>>());
                op.result["memberOutcome"] = json!(if reply.members.len()
                    == requested(&attempt.action).len()
                    && reply
                        .members
                        .iter()
                        .all(|m| rpc.cgi == 179 || m.status == Some(0))
                {
                    "accepted"
                } else {
                    "partial"
                });
            }
            let saved = (|| -> Result<Vec<u8>> {
                self.check_state_profile(account, &state)?;
                let sealed =
                    self.vault
                        .seal(account, "account-state", &serde_json::to_vec(&state)?)?;
                self.store.save_push_authorization(
                    account,
                    &sealed,
                    &expected,
                    attempt.epoch.as_deref(),
                )?;
                Ok(sealed)
            })();
            let saved = match saved {
                Ok(saved) => saved,
                Err(_) => {
                    return self.finish_group_review(
                        op,
                        &attempt,
                        &receipts,
                        step,
                        "authorization_generation_changed",
                    )
                }
            };
            expected = saved;
            // A known DelContact/DelChatContact rejection does not negate Quit.
            let cleanup = matches!(attempt.action, GroupMutation::Leave { .. }) && step < 2;
            let keep_going = (reply.accepted || cleanup) && step + 1 < attempt.requests.len();
            let projected = self
                .project_group_receipts(&op, &attempt, &receipts)
                .is_ok();
            op.result["profileProjection"] = json!(if projected
                && matches!(attempt.action, GroupMutation::Pin { .. })
                && reply.accepted
                && !keep_going
            {
                "complete"
            } else if projected {
                "partial"
            } else {
                "pending"
            });
            op.updated_at = now();
            if keep_going {
                op.state = "running".into();
                self.store.advance_group_mutation(&op, "queued", step + 1)?;
                continue;
            }
            op.result["remoteOutcome"] = json!(if reply.accepted {
                "accepted"
            } else {
                "rejected"
            });
            op.state = if reply.accepted {
                "succeeded"
            } else {
                "failed"
            }
            .into();
            if reply.accepted {
                self.store.advance_group_mutation(&op, "projecting", step)?;
                self.store.advance_group_mutation(&op, "succeeded", step)?;
            } else {
                self.store.advance_group_mutation(&op, "failed", step)?;
            }
            return Ok(op);
        }
        bail!("invalid_group_operation")
    }
    fn summarize_group_reply(
        &self,
        op: &mut Operation,
        rpc: &GroupRpc,
        reply: &GroupMutationReply,
    ) {
        op.result["steps"].as_array_mut().expect("group steps").push(json!({"cgi":rpc.cgi,"opcode":rpc.opcode,"serviceCode":reply.service_code,"commandResults":reply.command_results,"accepted":reply.accepted}));
        if let Some(group) = &reply.group_id {
            op.result["groupId"] = json!(group);
        }
        if !reply.members.is_empty() || matches!(rpc.cgi, 119 | 120 | 179) {
            op.result["members"] = json!(reply.members);
            op.result["declaredMemberCount"] = json!(reply.declared_count);
        }
    }
    fn finish_group_review(
        &self,
        mut op: Operation,
        attempt: &Attempt,
        receipts: &Receipts,
        step: usize,
        code: &str,
    ) -> Result<Operation> {
        op.state = "needs_review".into();
        op.updated_at = now();
        op.result["remoteOutcome"] = json!("unknown");
        op.result["code"] = json!(code);
        if self.project_group_receipts(&op, attempt, receipts).is_ok() {
            op.result["profileProjection"] = json!("partial");
        }
        self.store
            .advance_group_mutation(&op, "needs_review", step)?;
        Ok(op)
    }
    fn hold_group(&self, account: &str, id: &str) -> Result<Operation> {
        let record = self.store.group_mutation(account, id)?;
        if !matches!(record.phase.as_str(), "queued" | "submitting" | "received") {
            return Ok(record.operation);
        }
        let (attempt, receipts) = self.load_group_attempt(&record)?;
        self.finish_group_review(
            record.operation,
            &attempt,
            &receipts,
            record.step,
            "group_outcome_unknown",
        )
    }
    fn project_group_receipts(
        &self,
        op: &Operation,
        attempt: &Attempt,
        receipts: &Receipts,
    ) -> Result<()> {
        let account = &op.account_id;
        let (location, keys) = self.profile_location_keys(account)?;
        let options = self.write_options(account, &op.id)?;
        let target = op.result["groupId"].as_str().or(attempt.action.target());
        if let Some(target) = target {
            let current = self.group_revision(account, target)?;
            ensure!(
                current == attempt.profile_revision || current.as_deref() == Some(op.id.as_bytes()),
                "group_projection_superseded"
            );
        }
        let mut changed = false;
        for (rpc, raw) in attempt.requests.iter().zip(&receipts.replies) {
            let reply = decode_group_mutation(rpc, raw, requested(&attempt.action))?;
            if !reply.accepted {
                continue;
            }
            let group = reply
                .group_id
                .as_deref()
                .or(attempt.action.target())
                .context("group_reply_identity_missing")?;
            if let GroupMutation::Pin { thread_id, pinned } = &attempt.action {
                if rpc.opcode == Some(74) {
                    wechat_profile::apply_native_group_edit(
                        &location,
                        &keys,
                        thread_id,
                        NativeGroupEdit::Unfold {
                            own: attempt.own.clone(),
                        },
                        &options,
                    )?;
                } else if rpc.opcode == Some(2) {
                    ensure!(
                        wechat_profile::apply_contact_pin_patch(
                            &location,
                            &keys,
                            thread_id,
                            *pinned,
                            attempt.pin_clear_folded_primary,
                            receipts
                                .pin_projection_time
                                .context("pin_projection_timestamp_missing")?,
                            &options
                        )?
                        .is_some(),
                        "contact_state_incomplete"
                    );
                    // Oplog2 accepted the complete primary flag baseline from
                    // the refreshed contact, including unrelated preferences.
                    // Match that accepted record rather than restoring stale
                    // bits from the pre-refresh local cache.
                    let outer = p::decode(&rpc.body)?;
                    let list = p::decode(p::data(&outer, 1)?)?;
                    let item = p::decode(p::data(&list, 2)?)?;
                    ensure!(p::scalar(&item, 1)? == 2, "invalid_pin_receipt");
                    let contact = p::decode(&p::unbuffer(p::data(&item, 2)?)?)?;
                    let flag = u32::try_from(p::scalar(&contact, 8)?)?;
                    wechat_profile::apply_contact_patch(
                        &location,
                        &keys,
                        &ContactProjection {
                            username: thread_id.clone(),
                            fields: [("flag".into(), SqlValue::Integer(flag.to_string()))]
                                .into_iter()
                                .collect(),
                        },
                        &options,
                    )?;
                    self.store.clear_runtime(
                        account,
                        &format!("contact-pinning-unresolved:{thread_id}"),
                    )?;
                } else {
                    bail!("invalid_pin_receipt");
                }
                changed = true;
                continue;
            }
            if let GroupMutation::Mute { thread_id, muted } = &attempt.action {
                if !thread_id.ends_with("@chatroom") {
                    ensure!(
                        wechat_profile::apply_contact_mute_patch(
                            &location, &keys, thread_id, *muted, &options
                        )?
                        .is_some(),
                        "contact_state_incomplete"
                    );
                    self.store.clear_runtime(
                        account,
                        &format!("contact-notifications-unresolved:{thread_id}"),
                    )?;
                    changed = true;
                    continue;
                }
            }
            if rpc.cgi == 119 {
                let mut contact = RawFields::new();
                for (name, value) in [
                    ("nick_name", &reply.name),
                    ("pin_yin_initial", &reply.name_initials),
                    ("quan_pin", &reply.name_pinyin),
                ] {
                    contact.insert(
                        name.into(),
                        SqlValue::Text(value.clone().context("incomplete_group_reply")?),
                    );
                }
                for (name, value) in [
                    ("local_type", 2),
                    ("flag", 2),
                    ("delete_flag", 0),
                    ("is_in_chat_room", 1),
                    ("chat_room_notify", 1),
                ] {
                    contact.insert(name.into(), SqlValue::Integer(value.to_string()));
                }
                wechat_profile::apply_contact_patch(
                    &location,
                    &keys,
                    &ContactProjection {
                        username: group.into(),
                        fields: contact,
                    },
                    &options,
                )?;
                let mut group_fields = RawFields::new();
                group_fields.insert("owner".into(), SqlValue::Text(attempt.own.clone()));
                wechat_profile::apply_group_patch(
                    &location,
                    &keys,
                    &GroupProjection {
                        username: group.into(),
                        fields: group_fields,
                        detail_fields: None,
                        members: vec![],
                        complete_roster: false,
                    },
                    &options,
                )?;
            }
            let accepted: Vec<_> = reply
                .members
                .iter()
                .filter(|m| rpc.cgi == 179 || m.status == Some(0))
                .collect();
            if matches!(rpc.cgi, 119 | 120) {
                changed = true;
                let mut members = Vec::new();
                if rpc.cgi == 119 {
                    wechat_profile::apply_contact_patch(
                        &location,
                        &keys,
                        &ContactProjection {
                            username: attempt.own.clone(),
                            fields: RawFields::new(),
                        },
                        &options,
                    )?;
                    members.push(NativeMemberPatch {
                        flags: None,
                        opaque: None,
                        username: attempt.own.clone(),
                        display: None,
                    });
                }
                for member in &accepted {
                    let mut fields = RawFields::new();
                    if let Some(nick) = &member.nickname {
                        fields.insert("nick_name".into(), SqlValue::Text(nick.clone()));
                    }
                    wechat_profile::apply_contact_patch(
                        &location,
                        &keys,
                        &ContactProjection {
                            username: member.username.clone(),
                            fields,
                        },
                        &options,
                    )?;
                    members.push(NativeMemberPatch {
                        flags: None,
                        opaque: None,
                        username: member.username.clone(),
                        display: None,
                    });
                }
                wechat_profile::apply_native_roster_patch(
                    &location,
                    &keys,
                    &NativeRosterPatch {
                        username: group.into(),
                        members,
                        complete_roster: false,
                    },
                    &options,
                )?;
            } else if rpc.cgi == 179 {
                changed = true;
                wechat_profile::apply_native_group_edit(
                    &location,
                    &keys,
                    group,
                    NativeGroupEdit::RemoveMembers(
                        &accepted
                            .iter()
                            .map(|m| m.username.clone())
                            .collect::<Vec<_>>(),
                    ),
                    &options,
                )?;
            } else {
                let edit = match &attempt.action {
                    GroupMutation::Rename { name, .. } => {
                        changed = true;
                        let mut fields = RawFields::new();
                        fields.insert("nick_name".into(), SqlValue::Text(name.clone()));
                        wechat_profile::apply_contact_patch(
                            &location,
                            &keys,
                            &ContactProjection {
                                username: group.into(),
                                fields,
                            },
                            &options,
                        )?;
                        continue;
                    }
                    GroupMutation::Leave { .. } if rpc.opcode == Some(16) => {
                        NativeGroupEdit::Leave { own: &attempt.own }
                    }
                    GroupMutation::MyName { display_name, .. } => NativeGroupEdit::MyName {
                        own: &attempt.own,
                        display: display_name,
                    },
                    GroupMutation::Mute { muted, .. } => NativeGroupEdit::Notifications {
                        muted: *muted,
                        clear_minimized: rpc.opcode == Some(74),
                        own: &attempt.own,
                    },
                    GroupMutation::ClearAnnouncement { .. } => NativeGroupEdit::ClearAnnouncement,
                    _ => continue,
                };
                wechat_profile::apply_native_group_edit(&location, &keys, group, edit, &options)?;
                changed = true;
            }
        }
        if changed {
            if let Some(target) = target {
                self.mark_group_revision(account, target, &op.id)?;
            }
            self.group_mutation_event(op, attempt, receipts.replies.len())?;
        }
        Ok(())
    }
    fn group_mutation_event(
        &self,
        op: &Operation,
        attempt: &Attempt,
        receipt_count: usize,
    ) -> Result<()> {
        if let GroupMutation::Pin { thread_id, .. } = &attempt.action {
            self.store.append_event_once(&op.account_id, &format!("thread-pin:{}:{receipt_count}", op.id), "thread.updated", &json!({"operationId":op.id,"threadId":thread_id,"origin":"native_mutation","refetchRequired":true}))?;
            return Ok(());
        }
        if let GroupMutation::Mute { thread_id, .. } = &attempt.action {
            if !thread_id.ends_with("@chatroom") {
                self.store.append_event_once(
                    &op.account_id,
                    &format!("contact-mutation:{}:{receipt_count}", op.id),
                    "contacts.updated",
                    &json!({"operationId":op.id,"contactId":thread_id,"origin":"native_mutation"}),
                )?;
                return Ok(());
            }
        }
        self.store.append_event_once(&op.account_id, &format!("group-mutation:{}:{receipt_count}", op.id), "group.updated", &json!({"operationId":op.id,"action":action_name(&attempt.action),"groupId":op.result["groupId"],"source":"native_mutation","remoteOutcome":"accepted_steps","receiptCount":receipt_count,"profileProjection":"partial"}))?;
        Ok(())
    }
    pub fn repair_group_mutations(&self, account: &str) -> Result<usize> {
        let mut repaired = 0;
        for record in self.store.projecting_group_mutations(account)? {
            let (attempt, receipts) = self.load_group_attempt(&record)?;
            let mut op = record.operation;
            op.result["steps"] = json!([]);
            let mut fully_accepted = receipts.replies.len() == attempt.requests.len();
            for (rpc, raw) in attempt.requests.iter().zip(&receipts.replies) {
                if rpc.cgi == 119 {
                    if let Some(id) = observed_created_group(raw) {
                        op.result["groupId"] = json!(id);
                    }
                }
                if let Ok(reply) = decode_group_mutation(rpc, raw, requested(&attempt.action)) {
                    self.summarize_group_reply(&mut op, rpc, &reply);
                    let cleanup = matches!(attempt.action, GroupMutation::Leave { .. })
                        && matches!(rpc.opcode, Some(4 | 7));
                    fully_accepted &= !reply.uncertain && (reply.accepted || cleanup);
                    if matches!(rpc.cgi, 119 | 120 | 179) {
                        op.result["unconfirmedMembers"] = json!(requested(&attempt.action)
                            .iter()
                            .filter(|name| !reply.members.iter().any(|m| &m.username == *name))
                            .collect::<Vec<_>>());
                        op.result["memberOutcome"] = json!(if reply.members.len()
                            == requested(&attempt.action).len()
                            && reply
                                .members
                                .iter()
                                .all(|m| rpc.cgi == 179 || m.status == Some(0))
                        {
                            "accepted"
                        } else {
                            "partial"
                        });
                    }
                } else {
                    fully_accepted = false;
                }
            }
            if self
                .project_group_receipts(&op, &attempt, &receipts)
                .is_ok()
            {
                op.result["profileProjection"] = json!(if fully_accepted
                    && matches!(attempt.action, GroupMutation::Pin { .. })
                {
                    "complete"
                } else {
                    "partial"
                });
                op.updated_at = now();
                if fully_accepted && record.phase == "needs_review" {
                    // A retained verified response can resolve a decoder gap.
                    // This branch performs local writes only, with no RPC.
                    op.state = "succeeded".into();
                    op.result["remoteOutcome"] = json!("accepted");
                    op.result.as_object_mut().map(|r| r.remove("code"));
                    self.store
                        .advance_group_mutation(&op, "projecting", record.step)?;
                    self.store
                        .advance_group_mutation(&op, "succeeded", record.step)?;
                    repaired += 1;
                    continue;
                }
                let phase = if record.phase == "projecting" {
                    "succeeded"
                } else {
                    &record.phase
                };
                self.store.advance_group_mutation(&op, phase, record.step)?;
                repaired += 1;
            } else {
                // Even an unsupported local layout must not hide a created
                // group identity recovered from a retained response.
                self.store
                    .advance_group_mutation(&op, &record.phase, record.step)?;
            }
        }
        Ok(repaired)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    pub(crate) fn fixture() -> (tempfile::TempDir, Arc<Core>) {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let state = AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![b'A'; 16],
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 7,
            own_username: "synthetic_self".into(),
            cookie: vec![1; 15],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: p::message(&[
                p::bytes(1, &p::buffer(b"a")),
                p::bytes(2, &p::buffer(b"b")),
            ])
            .unwrap(),
            key_info: vec![4; 180],
            database_password: vec![42; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        let (loc, keys) = core.profile_location_keys("one").unwrap();
        let o = core.write_options("one", "fixture").unwrap();
        for name in ["peer", "other"] {
            let fields = [
                ("local_type", SqlValue::Integer("1".into())),
                ("flag", SqlValue::Integer("1".into())),
                ("delete_flag", SqlValue::Integer("0".into())),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
            wechat_profile::apply_contact_patch(
                &loc,
                &keys,
                &ContactProjection {
                    username: name.into(),
                    fields,
                },
                &o,
            )
            .unwrap();
        }
        (temp, core)
    }
    fn create() -> GroupMutation {
        GroupMutation::Create {
            members: vec!["peer".into(), "other".into()],
            name: Some("Synthetic group".into()),
        }
    }
    fn created(malformed: bool) -> Vec<u8> {
        let sk = |s: &str| p::bytes(1, s.as_bytes());
        let mut member = vec![
            p::bytes(1, &sk("peer")),
            p::uint(2, 0),
            p::int32(6, 0),
            p::uint(12, 1),
        ];
        for n in [3, 4, 5, 9, 10, 11] {
            member.push(p::bytes(n, &sk("")));
        }
        let mut f = vec![
            p::bytes(1, &p::int32(1, 0)),
            p::uint(5, 2),
            p::bytes(6, &p::message(&member).unwrap()),
            p::bytes(7, &sk("synthetic@chatroom")),
            p::bytes(8, &p::uint(1, 0)),
        ];
        for n in [2, 3, 4] {
            f.push(p::bytes(n, &sk("Synthetic")));
        }
        if malformed {
            f.push(p::bytes(6, &p::bytes(1, &sk("other"))));
        }
        p::message(&f).unwrap()
    }
    #[test]
    fn thread_pinning_uses_one_attempt_and_repairs_without_resending_or_overwriting_newer_state() {
        let (_temp, mut core) = fixture();
        struct Snapshot(u32);
        impl crate::upstream::Upstream for Snapshot {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
                let mut f = vec![
                    p::bytes(1, &p::bytes(1, ids[0].as_bytes())),
                    p::uint(7, u32::MAX.into()),
                    p::uint(8, self.0.into()),
                    p::uint(82, u64::MAX),
                    p::uint(83, 0),
                ];
                for n in [2, 3, 4, 10, 11, 12] {
                    f.push(p::bytes(n, &p::bytes(1, b"")));
                }
                for n in [5, 9, 13, 14] {
                    f.push(p::uint(n, 0));
                }
                f.push(p::bytes(6, &p::buffer(b"")));
                p::message(&[
                    p::bytes(1, &p::int32(1, 0)),
                    p::uint(2, 1),
                    p::bytes(3, &p::message(&f)?),
                    p::uint(4, 0),
                ])
            }
        }
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(Snapshot(0x201));
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "pin-fixture").unwrap();
        let reader = core.profile_reader("one").unwrap();
        let table = reader
            .schemas()
            .iter()
            .flat_map(|schema| &schema.tables)
            .find(|table| table.name == "SessionTable")
            .unwrap();
        let mut session: RawFields = table
            .columns
            .iter()
            .filter(|column| column.name != "username")
            .map(|column| (column.name.clone(), SqlValue::Null))
            .collect();
        session.insert("last_timestamp".into(), SqlValue::Integer("100".into()));
        session.insert("sort_timestamp".into(), SqlValue::Integer("100".into()));
        wechat_profile::apply_session_projection(&location, &keys, "peer", &session, &options)
            .unwrap();
        let flag = |core: &Core| {
            integer(
                &core.group_contact("one", "peer").unwrap().raw_fields,
                "flag",
            )
            .unwrap()
        };
        for pinned in [true, false] {
            Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
                .unwrap()
                .upstream = Arc::new(Snapshot(if pinned { 0x201 } else { 0xa01 }));
            let action = GroupMutation::Pin {
                thread_id: "peer".into(),
                pinned,
            };
            let key = if pinned { "pin" } else { "unpin" };
            let (operation, created) = core.admit_group("one", action.clone(), key).unwrap();
            assert!(created);
            let done = core
                .execute_group("one", &operation.id, |_, rpc| {
                    assert_eq!(rpc.opcode, Some(2));
                    Ok(oplog(0))
                })
                .unwrap();
            assert_eq!(done.state, "succeeded");
            assert_eq!(done.result["profileProjection"], "complete");
            assert_eq!(done.result["crossDatabaseAtomic"], false);
            assert_eq!(flag(&core), if pinned { 0xa01 } else { 0x201 });
            assert_eq!(
                core.catalog_item("one", "threads", "peer").unwrap()["pinned"],
                pinned
            );
            let (replay, new) = core.admit_group("one", action, key).unwrap();
            assert!(!new);
            assert_eq!(replay.id, operation.id);
            core.execute_group("one", &replay.id, |_, _| panic!("replay must not resubmit"))
                .unwrap();
            assert!(core
                .admit_group(
                    "one",
                    GroupMutation::Pin {
                        thread_id: "peer".into(),
                        pinned: !pinned
                    },
                    key
                )
                .is_err());
        }
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(Snapshot(0x201));
        let action = GroupMutation::Pin {
            thread_id: "peer".into(),
            pinned: true,
        };
        let (uncertain, _) = core
            .admit_group("one", action.clone(), "unknown-pin")
            .unwrap();
        let done = core
            .execute_group("one", &uncertain.id, |_, _| bail!("response lost"))
            .unwrap();
        assert_eq!(done.state, "needs_review");
        assert_eq!(flag(&core), 0x201);
        core.execute_group("one", &uncertain.id, |_, _| {
            panic!("must not retry unknown pin")
        })
        .unwrap();
        let (superseded, _) = core.admit_group("one", action, "old-pin").unwrap();
        let done = core
            .execute_group("one", &superseded.id, |_, _| {
                core.mark_group_revision("one", "peer", "newer-phone-receipt")?;
                Ok(oplog(0))
            })
            .unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.result["profileProjection"], "pending");
        core.repair_group_mutations("one").unwrap();
        assert_eq!(flag(&core), 0x201);
        assert!(core
            .store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .any(|event| event.kind == "thread.updated"));
    }
    #[test]
    fn direct_mute_and_unmute_preserve_other_flags_and_share_one_attempt_receipts() {
        let (_temp, mut core) = fixture();
        let flag = |core: &Core| {
            let contact = core.group_contact("one", "peer").unwrap();
            integer(&contact.raw_fields, "flag").unwrap()
        };
        let baseline = flag(&core);
        struct ContactSnapshot(u64);
        impl crate::upstream::Upstream for ContactSnapshot {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
                let mut fields = vec![
                    p::bytes(1, &p::bytes(1, ids[0].as_bytes())),
                    p::uint(7, u64::from(u32::MAX)),
                    p::uint(8, self.0),
                    p::bytes(123, b"preserve unknown"),
                ];
                for n in [2, 3, 4, 10, 11, 12] {
                    fields.push(p::bytes(n, &p::bytes(1, b"current metadata")));
                }
                for n in [5, 9, 13, 14] {
                    fields.push(p::uint(n, 0));
                }
                fields.push(p::bytes(6, &p::buffer(b"")));
                p::message(&[
                    p::bytes(1, &p::int32(1, 0)),
                    p::uint(2, 1),
                    p::bytes(3, &p::message(&fields)?),
                    p::uint(4, 0),
                ])
            }
        }
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(ContactSnapshot(baseline as u64));
        for muted in [true, false] {
            let key = if muted {
                "direct-mute"
            } else {
                "direct-unmute"
            };
            let action = GroupMutation::Mute {
                thread_id: "peer".into(),
                muted,
            };
            let (operation, created) = core.admit_group("one", action.clone(), key).unwrap();
            assert!(created);
            let done = core
                .execute_group("one", &operation.id, |_, rpc| {
                    assert_eq!(rpc.cgi, 681);
                    assert_eq!(rpc.opcode, Some(2));
                    let outer = p::decode(&rpc.body).unwrap();
                    let list = p::decode(p::data(&outer, 1).unwrap()).unwrap();
                    let item = p::decode(p::data(&list, 2).unwrap()).unwrap();
                    let payload =
                        p::decode(&p::unbuffer(p::data(&item, 2).unwrap()).unwrap()).unwrap();
                    assert_eq!(payload.len(), 15);
                    assert_eq!(p::data(&payload, 123).unwrap(), b"preserve unknown");
                    assert_eq!(p::scalar(&payload, 7).unwrap(), u64::from(u32::MAX));
                    assert_eq!(
                        p::scalar(&payload, 8).unwrap(),
                        (baseline as u64 & !0x200) | if muted { 0x200 } else { 0 }
                    );
                    Ok(oplog(0))
                })
                .unwrap();
            assert_eq!(done.state, "succeeded");
            assert!(done.result["groupId"].is_null());
            assert_eq!(
                flag(&core),
                (baseline & !0x200) | if muted { 0x200 } else { 0 }
            );
            let (again, new) = core.admit_group("one", action, key).unwrap();
            assert!(!new);
            assert_eq!(again.id, operation.id);
            core.execute_group("one", &again.id, |_, _| panic!("must not resubmit"))
                .unwrap();
        }
        let (operation, _) = core
            .admit_group(
                "one",
                GroupMutation::Mute {
                    thread_id: "peer".into(),
                    muted: true,
                },
                "uncertain-mute",
            )
            .unwrap();
        let done = core
            .execute_group("one", &operation.id, |_, _| bail!("lost response"))
            .unwrap();
        assert_eq!(done.state, "needs_review");
        assert_eq!(flag(&core), baseline & !0x200);
        core.execute_group("one", &operation.id, |_, _| {
            panic!("uncertain mutations must not resubmit")
        })
        .unwrap();
    }
    #[test]
    fn ordinary_request_shapes_keep_linux_wrappers_and_separate_commands() {
        let (_temp, core) = fixture();
        let state = core.account_state("one").unwrap();
        let room = "synthetic@chatroom";
        let create = group_mutation_requests(
            &state,
            &GroupMutation::Create {
                members: vec!["peer".into(), "other".into()],
                name: None,
            },
            false,
        )
        .unwrap();
        let fields = p::decode(&create[0].body).unwrap();
        assert_eq!(create[0].cgi, 119);
        assert_eq!(p::scalar(&fields, 3).unwrap(), 2);
        assert_eq!(
            p::data(&p::decode(p::data(&fields, 2).unwrap()).unwrap(), 1).unwrap(),
            b""
        );
        let add = group_mutation_requests(
            &state,
            &GroupMutation::AddMembers {
                group_id: room.into(),
                members: vec!["peer".into()],
            },
            false,
        )
        .unwrap();
        let delete = group_mutation_requests(
            &state,
            &GroupMutation::RemoveMembers {
                group_id: room.into(),
                members: vec!["peer".into()],
            },
            false,
        )
        .unwrap();
        let a = p::decode(&add[0].body).unwrap();
        let d = p::decode(&delete[0].body).unwrap();
        assert_eq!(
            p::data(&p::decode(p::data(&a, 4).unwrap()).unwrap(), 1).unwrap(),
            room.as_bytes()
        );
        assert!(p::optional(&a, 5).unwrap().is_none());
        assert_eq!(p::data(&d, 4).unwrap(), room.as_bytes());
        assert_eq!(p::scalar(&d, 5).unwrap(), 0);
        let my = group_mutation_requests(
            &state,
            &GroupMutation::MyName {
                group_id: room.into(),
                display_name: "Display".into(),
            },
            false,
        )
        .unwrap();
        let outer = p::decode(&my[0].body).unwrap();
        assert_eq!(outer.len(), 1);
        let list = p::decode(p::data(&outer, 1).unwrap()).unwrap();
        assert_eq!(p::scalar(&list, 1).unwrap(), 1);
        let item = p::decode(p::data(&list, 2).unwrap()).unwrap();
        assert_eq!(p::scalar(&item, 1).unwrap(), 48);
        let raw = p::unbuffer(p::data(&item, 2).unwrap()).unwrap();
        let payload = p::decode(&raw).unwrap();
        assert_eq!(p::data(&payload, 1).unwrap(), room.as_bytes());
        assert_eq!(p::data(&payload, 2).unwrap(), state.own_username.as_bytes());
        assert_eq!(p::data(&payload, 3).unwrap(), b"Display");
        let unmute = group_mutation_requests(
            &state,
            &GroupMutation::Mute {
                thread_id: room.into(),
                muted: false,
            },
            true,
        )
        .unwrap();
        assert_eq!(
            unmute.iter().map(|r| r.opcode).collect::<Vec<_>>(),
            [Some(20), Some(74)]
        );
        let clear = group_mutation_requests(
            &state,
            &GroupMutation::ClearAnnouncement {
                group_id: room.into(),
            },
            false,
        )
        .unwrap();
        let f = p::decode(&clear[0].body).unwrap();
        assert_eq!(clear[0].cgi, 993);
        assert_eq!(p::data(&f, 3).unwrap(), b"");
        assert_eq!(p::data(&f, 5).unwrap(), b"");
        assert_eq!(p::scalar(&f, 8).unwrap(), 1);
        for n in [4, 6, 7] {
            assert!(p::optional(&f, n).unwrap().is_none());
        }
    }
    fn oplog(code: i32) -> Vec<u8> {
        p::message(&[
            p::int32(1, 0),
            p::bytes(
                2,
                &p::message(&[p::uint(1, 1), p::bytes(2, &p::varint(code as i64 as u64))]).unwrap(),
            ),
        ])
        .unwrap()
    }
    pub(crate) fn make_group(core: &Core) {
        let (op, _) = core.admit_group("one", create(), "setup").unwrap();
        let done = core
            .execute_group("one", &op.id, |_, _| Ok(created(false)))
            .unwrap();
        assert_eq!(done.state, "succeeded");
    }
    #[tokio::test]
    async fn admission_guard_prevents_credential_updates_before_group_execution() {
        let (_temp, core) = fixture();
        let (operation, created_operation, guard) = core
            .admit_group_guarded("one", create(), "held-admission")
            .await
            .unwrap();
        assert!(created_operation);
        let changed = Arc::new(AtomicUsize::new(0));
        let other_core = core.clone();
        let other_changed = changed.clone();
        let (began, waiting) = tokio::sync::oneshot::channel();
        let update = tokio::spawn(async move {
            began.send(()).unwrap();
            let _guard = other_core.lock_account("one").await.unwrap();
            let state = other_core.account_state("one").unwrap();
            other_core.save_state("one", &state).unwrap();
            other_changed.store(1, Ordering::SeqCst);
        });
        waiting.await.unwrap();
        assert_eq!(changed.load(Ordering::SeqCst), 0);
        let completed = core
            .execute_group("one", &operation.id, |_, _| {
                assert_eq!(changed.load(Ordering::SeqCst), 0);
                Ok(created(false))
            })
            .unwrap();
        assert_eq!(completed.state, "succeeded");
        drop(guard);
        update.await.unwrap();
        assert_eq!(changed.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn partial_create_retains_identity_and_never_invents_an_omitted_success() {
        let (_temp, core) = fixture();
        let (op, yes) = core.admit_group("one", create(), "key").unwrap();
        assert!(yes);
        let calls = AtomicUsize::new(0);
        let done = core
            .execute_group("one", &op.id, |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    core.store.group_mutation("one", &op.id).unwrap().phase,
                    "submitting"
                );
                Ok(created(false))
            })
            .unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.result["memberOutcome"], "partial");
        assert_eq!(done.result["groupId"], "synthetic@chatroom");
        assert_eq!(done.result["unconfirmedMembers"], json!(["other"]));
        assert_eq!(
            core.group_member("one", "synthetic@chatroom", "peer")
                .unwrap()["state"],
            "present"
        );
        assert_eq!(
            core.group_member("one", "synthetic@chatroom", "other")
                .unwrap()["state"],
            "unconfirmed"
        );
        let (same, new) = core.admit_group("one", create(), "key").unwrap();
        assert!(!new);
        assert_eq!(same.id, op.id);
        core.execute_group("one", &op.id, |_, _| panic!("must never resubmit"))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(core
            .admit_group(
                "one",
                GroupMutation::Create {
                    members: vec!["peer".into()],
                    name: None
                },
                "key"
            )
            .is_err());
    }
    #[test]
    fn lost_or_malformed_reply_stops_and_preserves_any_created_identity() {
        for malformed in [false, true] {
            let (_temp, core) = fixture();
            let (op, _) = core.admit_group("one", create(), "key").unwrap();
            let done = core
                .execute_group("one", &op.id, |_, _| {
                    if malformed {
                        Ok(created(true))
                    } else {
                        bail!("synthetic loss")
                    }
                })
                .unwrap();
            assert_eq!(done.state, "needs_review");
            if malformed {
                assert_eq!(done.result["groupId"], "synthetic@chatroom");
            }
            core.execute_group("one", &op.id, |_, _| panic!("uncertain cannot retry"))
                .unwrap();
            assert!(core.cancel_operation("one", &op.id).is_err());
        }
    }
    #[test]
    fn leave_keeps_known_cleanup_rejection_but_stops_at_an_uncertain_step() {
        for uncertain in [false, true] {
            let (_temp, core) = fixture();
            make_group(&core);
            let (op, _) = core
                .admit_group(
                    "one",
                    GroupMutation::Leave {
                        group_id: "synthetic@chatroom".into(),
                    },
                    "leave",
                )
                .unwrap();
            let mut calls = vec![];
            let done = core
                .execute_group("one", &op.id, |_, rpc| {
                    calls.push(rpc.opcode.unwrap());
                    Ok(oplog(if calls.len() == 1 {
                        -1
                    } else if uncertain {
                        -433
                    } else {
                        0
                    }))
                })
                .unwrap();
            assert_eq!(
                calls,
                if uncertain {
                    vec![4, 7]
                } else {
                    vec![4, 7, 16]
                }
            );
            assert_eq!(
                done.state,
                if uncertain {
                    "needs_review"
                } else {
                    "succeeded"
                }
            );
            let own = core
                .group_member("one", "synthetic@chatroom", "synthetic_self")
                .unwrap();
            assert_eq!(
                own["state"],
                if uncertain { "present" } else { "unconfirmed" }
            );
            core.execute_group("one", &op.id, |_, _| panic!("no leave replay"))
                .unwrap();
        }
    }
    #[test]
    fn cancelled_or_changed_authorization_never_calls_transport() {
        for cancel in [false, true] {
            let (_temp, core) = fixture();
            let (op, _) = core.admit_group("one", create(), "key").unwrap();
            if cancel {
                core.cancel_operation("one", &op.id).unwrap();
            } else {
                core.store
                    .put_runtime("one", "logout-epoch", b"new-generation")
                    .unwrap();
            }
            let done = core
                .execute_group("one", &op.id, |_, _| panic!("must not call"))
                .unwrap();
            assert!(matches!(
                done.state.as_str(),
                "cancelled" | "needs_review" | "failed"
            ));
        }
    }
    #[test]
    fn interrupted_received_create_recovers_identity_without_network() {
        let (_temp, core) = fixture();
        let (op, _) = core.admit_group("one", create(), "key").unwrap();
        let cipher = core.store.runtime("one", "account-state").unwrap().unwrap();
        core.store
            .begin_group_step("one", &op.id, 0, &cipher, None, &core.ownership_generation)
            .unwrap();
        let receipts = Receipts {
            replies: vec![created(true)],
            ..Default::default()
        };
        core.store
            .receive_group_step(
                "one",
                &op.id,
                0,
                &core
                    .vault
                    .seal(
                        "one",
                        &purpose(&op.id, "receipts"),
                        &serde_json::to_vec(&receipts).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
        core.store.recover_group_mutations("one").unwrap();
        core.repair_group_mutations("one").unwrap();
        let done = core.store.operation("one", &op.id).unwrap();
        assert_eq!(done.state, "needs_review");
        assert_eq!(done.result["groupId"], "synthetic@chatroom");
        core.execute_group("one", &op.id, |_, _| panic!("recovery cannot send"))
            .unwrap();
    }
}
