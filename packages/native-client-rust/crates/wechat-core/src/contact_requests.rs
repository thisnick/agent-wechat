//! Account-bound contact discovery, canonical request views and one-attempt mutations.
use super::*;
use crate::CatalogQuery;
use anyhow::ensure;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use wechat_profile::{ContactRequestMessage, ContactRequestProjection, StagedContactRequest};
use wechat_protocol::{AcceptContactRequest, ContactRequestTarget, ContactRpc, SendContactRequest};

/// Account contact/request mutation; requests refer to exact opaque handles and versions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContactMutation {
    /// Send one greeting using an account-bound discovery handle.
    SendRequest {
        recipient_ref: String,
        message: String,
        /// Optional relationship privacy selections for this request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        privacy: Option<ContactPrivacySettings>,
    },
    /// Accept exactly the inspected request version.
    AcceptRequest {
        id: String,
        expected_version: String,
        /// Optional relationship privacy selections for acceptance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        privacy: Option<ContactPrivacySettings>,
    },
    /// Delete local per-peer history without a server rejection.
    DeleteRequest {
        id: String,
        expected_version: String,
    },
    /// Remove membership while preserving chat history.
    RemoveContact { username: String },
    /// Change the inspected canonical contact's relationship privacy selections.
    UpdatePrivacy {
        /// Exact saved ordinary contact identity.
        username: String,
        /// Desired relationship privacy selections.
        privacy: ContactPrivacySettings,
        /// Version returned by the canonical privacy read.
        expected_version: String,
    },
}
impl ContactMutation {
    fn name(&self) -> &'static str {
        match self {
            Self::SendRequest { .. } => "send_request",
            Self::AcceptRequest { .. } => "accept_request",
            Self::DeleteRequest { .. } => "delete_request",
            Self::RemoveContact { .. } => "remove_contact",
            Self::UpdatePrivacy { .. } => "update_privacy",
        }
    }
}

/// Supported updates to an existing contact, checked against its canonical version.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactUpdate {
    /// Complete desired relationship privacy selections.
    pub privacy: ContactPrivacySettings,
    /// Version returned by the canonical privacy read.
    pub expected_version: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Filters and retained-page continuation for canonical request conversations.
pub struct ContactRequestQuery {
    /// Incoming or outgoing direction; absent includes both.
    pub direction: Option<String>,
    /// Filter current acceptance candidates when specified.
    pub actionable: Option<bool>,
    /// Local label, identity or greeting search.
    pub search: Option<String>,
    /// Page size from 1 through 200.
    pub limit: Option<usize>,
    /// Opaque continuation from the same account and filters.
    pub cursor: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Recipient {
    target: ContactRequestTarget,
    query: String,
    session: String,
    epoch: Option<Vec<u8>>,
    expires: i64,
}
#[derive(Serialize, Deserialize)]
struct Attempt {
    action: ContactMutation,
    rpc: Option<ContactRpc>,
    target: Option<ContactRequestTarget>,
    own: String,
    uin: u32,
    generation: String,
    owner: String,
    epoch: Option<Vec<u8>>,
}
#[derive(Default, Serialize, Deserialize)]
struct Receipts {
    reply: Option<Vec<u8>>,
    stage: Option<StagedContactRequest>,
    time: Option<i64>,
}
fn digest(v: &[u8]) -> String {
    format!("{:x}", Sha256::digest(v))
}
fn purpose(id: &str, kind: &str) -> String {
    format!("contact-mutation:{id}:{kind}")
}
fn request_id(username: &str) -> String {
    format!("fr_{}", URL_SAFE_NO_PAD.encode(username))
}
fn request_username(id: &str) -> Result<String> {
    ensure!(id.len() <= 4096, "invalid_contact_request_id");
    let value = String::from_utf8(
        URL_SAFE_NO_PAD.decode(
            id.strip_prefix("fr_")
                .context("invalid_contact_request_id")?,
        )?,
    )?;
    wechat_protocol::ordinary_identity(&value)?;
    Ok(value)
}
fn flag(contact: &wechat_profile::Contact) -> bool {
    matches!(contact.raw_fields.get("flag"),Some(wechat_profile::SqlValue::Integer(v)) if v.parse::<u64>().is_ok_and(|v|v&1!=0))
        && matches!(
            contact.raw_fields.get("delete_flag"),
            None | Some(wechat_profile::SqlValue::Null)
        )
        || matches!((contact.raw_fields.get("flag"),contact.raw_fields.get("delete_flag")),(Some(wechat_profile::SqlValue::Integer(v)),Some(wechat_profile::SqlValue::Integer(d))) if v.parse::<u64>().is_ok_and(|v|v&1!=0)&&d=="0")
}

/// Relationship evidence from the retained canonical row, not presence in the contact table.
pub(crate) fn contact_relation(contact: &wechat_profile::Contact) -> &'static str {
    if flag(contact) {
        "friend"
    } else if matches!(contact.raw_fields.get("flag"), Some(wechat_profile::SqlValue::Integer(v)) if v.parse::<u32>().is_ok())
    {
        "non_contact"
    } else {
        "unknown"
    }
}

#[derive(Debug, Serialize)]
struct ContactPrivacySnapshot {
    privacy: ContactPrivacySettings,
    version: String,
}

fn canonical_privacy(
    account: &str,
    username: &str,
    contact: &wechat_profile::Contact,
) -> Result<ContactPrivacySnapshot> {
    ensure!(
        contact.username == username,
        "contact_privacy_identity_conflict"
    );
    let flags = match contact.raw_fields.get("flag") {
        Some(wechat_profile::SqlValue::Integer(value)) => {
            value.parse::<u32>().context("contact_flags_unresolved")?
        }
        _ => bail!("contact_flags_unresolved"),
    };
    let member = flag(contact);
    let version = digest(&serde_json::to_vec(&(
        "contact-privacy-v1",
        account,
        username,
        flags,
        member,
    ))?);
    Ok(ContactPrivacySnapshot {
        privacy: ContactPrivacySettings::from_contact_flags(flags),
        version,
    })
}

fn ordinary_privacy_contact(contact: &wechat_profile::Contact, own: &str) -> Result<()> {
    wechat_protocol::ordinary_identity(&contact.username)?;
    let system = crate::peer_classification::peer_classification(&contact.username, None).category
        == crate::PeerCategory::System;
    let verified_service = matches!(contact.raw_fields.get("verify_flag"), Some(wechat_profile::SqlValue::Integer(value)) if value.parse::<u32>().is_ok_and(|value| value & 8 != 0));
    ensure!(
        !contact.is_group
            && !contact.username.contains('@')
            && contact.username != own
            && contact.local_type == Some(1)
            && !system
            && !verified_service
            && flag(contact),
        "ordinary_contact_required"
    );
    Ok(())
}

impl Core {
    /// Read relationship privacy and an account-bound version from the official contact row.
    pub fn contact_privacy(&self, account: &str, username: &str) -> Result<Value> {
        Ok(serde_json::to_value(
            self.current_contact_privacy(account, username, None)?,
        )?)
    }

    /// Update an existing contact once, retaining its original intent and reply.
    pub async fn update_contact(
        self: &Arc<Self>,
        account: &str,
        username: &str,
        update: ContactUpdate,
        key: &str,
    ) -> Result<Operation> {
        self.mutate_contact(
            account,
            ContactMutation::UpdatePrivacy {
                username: username.into(),
                privacy: update.privacy,
                expected_version: update.expected_version,
            },
            key,
        )
        .await
    }

    fn current_contact_privacy(
        &self,
        account: &str,
        username: &str,
        expected_version: Option<&str>,
    ) -> Result<ContactPrivacySnapshot> {
        ensure!(
            self.contact_privacy_resolved(account, username)?,
            "contact_privacy_unresolved"
        );
        self.ensure_no_pending_contact_privacy(account, username)?;
        let contact = self.profile_reader_owned(account)?.contact(username)?;
        let snapshot = canonical_privacy(account, username, &contact)?;
        ensure!(
            expected_version.is_none_or(|expected| expected == snapshot.version),
            "stale_contact_privacy"
        );
        let own = self
            .profile_username(account)?
            .context("profile_identity_unavailable")?;
        ordinary_privacy_contact(&contact, &own)?;
        Ok(snapshot)
    }

    fn ensure_no_pending_contact_privacy(&self, account: &str, username: &str) -> Result<()> {
        let reader = self.profile_reader_owned(account)?;
        let canonical = reader
            .contact_for_request(username)?
            .map(|contact| contact.username)
            .unwrap_or_else(|| username.into());
        for record in self.store.pending_contact_relationships(account)? {
            if record.operation.state != "succeeded" {
                continue;
            }
            let (attempt, _) = self.load_contact_attempt(&record)?;
            let target = match &attempt.action {
                ContactMutation::UpdatePrivacy { username, .. } => Some(username.as_str()),
                ContactMutation::AcceptRequest {
                    privacy: Some(_), ..
                } => attempt
                    .target
                    .as_ref()
                    .map(|target| target.username.as_str()),
                _ => None,
            };
            if let Some(target) = target {
                let target = reader
                    .contact_for_request(target)?
                    .map(|contact| contact.username)
                    .unwrap_or_else(|| target.into());
                ensure!(target != canonical, "contact_privacy_pending");
            }
        }
        Ok(())
    }

    fn ensure_contact_privacy_guard_capacity(
        &self,
        account: &str,
        action: &ContactMutation,
        own_operation: Option<&str>,
    ) -> Result<()> {
        if matches!(
            action,
            ContactMutation::UpdatePrivacy { .. }
                | ContactMutation::SendRequest {
                    privacy: Some(_),
                    ..
                }
                | ContactMutation::AcceptRequest {
                    privacy: Some(_),
                    ..
                }
        ) {
            ensure!(
                !self
                    .store
                    .contact_privacy_guard_overflow(account, own_operation)?,
                "contact_privacy_pending"
            );
        }
        Ok(())
    }

    fn ensure_no_active_contact_privacy(
        &self,
        account: &str,
        username: &str,
        own_operation: Option<&str>,
    ) -> Result<()> {
        let reader = self.profile_reader_owned(account)?;
        let canonical = reader
            .contact_for_request(username)?
            .map(|contact| contact.username)
            .unwrap_or_else(|| username.into());
        let active = self
            .store
            .active_contact_mutations(account, own_operation)?;
        for record in active {
            if own_operation == Some(record.operation.id.as_str()) {
                continue;
            }
            let (attempt, _) = self.load_contact_attempt(&record)?;
            let target = match &attempt.action {
                ContactMutation::UpdatePrivacy { username, .. } => Some(username.as_str()),
                ContactMutation::SendRequest {
                    privacy: Some(_), ..
                }
                | ContactMutation::AcceptRequest {
                    privacy: Some(_), ..
                } => attempt
                    .target
                    .as_ref()
                    .map(|target| target.username.as_str()),
                _ => None,
            };
            if let Some(target) = target {
                let target = reader
                    .contact_for_request(target)?
                    .map(|contact| contact.username)
                    .unwrap_or_else(|| target.into());
                ensure!(target != canonical, "contact_privacy_pending");
            }
        }
        Ok(())
    }

    fn request_view(
        &self,
        account: &str,
        row: &wechat_profile::ContactRequestRecord,
    ) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let contact = reader.contact_for_request(&row.username)?;
        let is_contact = contact.as_ref().is_some_and(flag);
        let aged = row.aged == Some(true)
            || wechat_store::unix_seconds().saturating_sub(row.timestamp) > 259200;
        let actionable = !is_contact
            && !row.username.is_empty()
            && row.waiting == Some(false)
            && row.aged == Some(false)
            && !aged
            && !row.ticket.is_empty();
        let direction = if row.waiting == Some(true) || row.sender {
            "outgoing"
        } else {
            "incoming"
        };
        let display = contact
            .as_ref()
            .map(|c| c.label.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| (!row.remark.is_empty()).then(|| row.remark.clone()))
            .unwrap_or_else(|| row.username.clone());
        Ok(
            json!({"id":request_id(&row.username),"peerId":contact.as_ref().map(|c|c.username.as_str()).unwrap_or(&row.username),"displayName":display,"direction":direction,"message":row.content,"updatedAt":row.timestamp,"version":row.version,"isContact":is_contact,"actionable":actionable,"attention":row.attention==Some(true)&&actionable,"state":if is_contact {"contact"}else if direction=="outgoing" {"request_sent"}else {"request_received"},"history":row.history.iter().map(|m|json!({"serverId":m.server_id,"message":m.content,"createdAt":m.timestamp,"direction":if m.sender {"outgoing"}else{"incoming"}})).collect::<Vec<_>>()}),
        )
    }
    /// Read canonical per-peer request conversations with account-bound cursor validation.
    pub fn contact_requests(&self, account: &str, q: &ContactRequestQuery) -> Result<Value> {
        ensure!(
            q.direction
                .as_ref()
                .is_none_or(|d| matches!(d.as_str(), "incoming" | "outgoing"))
                && q.search.as_ref().is_none_or(|s| s.len() <= 512),
            "invalid_request"
        );
        let limit = q.limit.unwrap_or(50);
        ensure!((1..=200).contains(&limit), "invalid_request");
        let rows = self.profile_reader_owned(account)?.contact_requests()?;
        let mut items = vec![];
        let mut pending = 0;
        for row in rows {
            let v = self.request_view(account, &row)?;
            if v["attention"] == true {
                pending += 1;
            }
            if q.direction
                .as_ref()
                .is_some_and(|d| v["direction"] != d.as_str())
                || q.actionable.is_some_and(|value| v["actionable"] != value)
            {
                continue;
            }
            if q.search.as_ref().is_some_and(|s| {
                let s = s.to_lowercase();
                !v["displayName"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&s)
                    && !row.username.to_lowercase().contains(&s)
                    && !row
                        .history
                        .iter()
                        .any(|h| h.content.to_lowercase().contains(&s))
            }) {
                continue;
            }
            items.push(v);
        }
        let query = digest(&serde_json::to_vec(&(
            "contact_requests",
            account,
            &q.direction,
            q.actionable,
            &q.search,
        ))?);
        let reader = self.profile_reader_owned(account)?;
        let metadata = json!({"sourceGeneration":reader.generation(),"coverage":reader.coverage(),"pendingIncomingCount":pending,"order":items.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()});
        self.paginate_values(
            account,
            &query,
            &CatalogQuery {
                limit: Some(limit),
                cursor: q.cursor.clone(),
                search: None,
                thread_kind: None,
                relation: None,
            },
            items,
            metadata,
        )
    }

    /// Read one exact canonical conversation and its current mutation version.
    pub fn contact_request(&self, account: &str, id: &str) -> Result<Value> {
        let row = self
            .profile_reader_owned(account)?
            .contact_request(&request_username(id)?)?;
        self.request_view(account, &row)
    }
    /// Search remote discovery once; return an account/session-bound opaque recipient handle.
    pub async fn search_contacts(self: &Arc<Self>, account: &str, query: &str) -> Result<Value> {
        let query = query.to_owned();
        self.session_request(account,"contact_search",|fence|async move{
            let guard=self.lock_account(account).await?;let core=self.clone();let account=account.to_owned();blocking(move||{
                let mut permit=crate::account_network::AccountNetworkPermit {guard:Some(guard),runtime:tokio::runtime::Handle::current()};let mut state=core.session_state(&account,&fence)?;let expected=core.store.runtime(&account,"account-state")?.context("auth_required")?;let desired=core.store.runtime(&account,"desired")?;
                let rpc=wechat_protocol::contact_search_request(&state,&query)?;let raw=permit.transfer(&core,&account,||core.native_request(&account,&mut state,|r|r.contact_rpc_raw(&rpc)))?;
                core.store.put_runtime(&account,"contact-search-receipt",&core.vault.seal(&account,"contact-search-receipt",&raw)?)?;
                core.save_network_state(&account,&state,&expected,&fence,desired.as_deref())?;
                let found=wechat_protocol::decode_contact_search(&raw,&query).map_err(|error| {
                    if wechat_protocol::ServiceRejected::find(&error).is_some_and(|e| !e.is_session_rejected()) { anyhow::anyhow!("contact_discovery_rejected") } else { error }
                })?;let expires=wechat_store::unix_seconds()+600;
                let recipient=Recipient {target:found.target,query:query.clone(),session:digest(&state.session_key),epoch:core.store.runtime(&account,"logout-epoch")?,expires};
                let reader=core.profile_reader_owned(&account)?;
                let existing=match reader.contact_request(&recipient.target.username){Ok(row)=>Some(core.request_view(&account,&row)?),Err(wechat_profile::ProfileError::NotCached)=>None,Err(e)=>return Err(e.into())};
                let contact_id=reader.contact_for_request(&recipient.target.username)?.filter(flag).map(|c|c.username);
                let reference=if recipient.target.ticket.is_empty() { None } else { Some(URL_SAFE_NO_PAD.encode(core.vault.seal(&account,"contact-recipient",&serde_json::to_vec(&recipient)?)?)) };
                Ok(json!({"displayName":query,"recipientRef":reference,"expiresAt":expires,"source":"wechat","request":existing,"contactId":contact_id}))
            }).await
        }).await
    }
    fn recipient(&self, account: &str, reference: &str, state: &AccountState) -> Result<Recipient> {
        ensure!(reference.len() <= 131072, "invalid_recipient_reference");
        let decode = || -> Result<Recipient> {
            Ok(serde_json::from_slice(&self.vault.open_record(
                account,
                "contact-recipient",
                &URL_SAFE_NO_PAD.decode(reference)?,
            )?)?)
        };
        let v = decode().map_err(|_| anyhow::anyhow!("invalid_recipient_reference"))?;
        ensure!(
            v.expires >= wechat_store::unix_seconds()
                && v.session == digest(&state.session_key)
                && v.epoch == self.store.runtime(account, "logout-epoch")?,
            "stale_recipient_reference"
        );
        Ok(v)
    }
    fn ensure_request_database(&self, account: &str, intent: &str) -> Result<()> {
        let (location, mut keys) = self.profile_location_keys(account)?;
        let rel = "general/general.db";
        if location.root().join("db_storage").join(rel).exists() && keys.databases.contains_key(rel)
        {
            return Ok(());
        }
        let options = self.write_options(account, intent)?;
        let state = self.account_state(account)?;
        if !location.root().join("db_storage").join(rel).exists() {
            let key = wechat_profile::apply_initialize_contact_requests(
                &location,
                &keys,
                &state.database_password,
                &options,
            )?;
            keys.databases.insert(rel.into(), key);
        } else {
            let mut derived = wechat_profile::keys_from_password(
                location,
                &state.database_password,
                wechat_profile::ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )?;
            derived.image_dat = keys.image_dat;
            keys = derived;
        }
        let path = self
            .store
            .root
            .join("accounts")
            .join(account)
            .join("agent")
            .join(format!(
                "profile-keys-derived-{}.json",
                uuid::Uuid::new_v4()
            ));
        keys.write_new(&path)?;
        self.store.set_keys_file(account, &path)?;
        Ok(())
    }
    /// Admit one contact mutation using existing account and idempotency fences.
    pub async fn mutate_contact(
        self: &Arc<Self>,
        account: &str,
        action: ContactMutation,
        key: &str,
    ) -> Result<Operation> {
        ensure!(!self.stopping.load(Ordering::Acquire), "service_stopping");
        let operation_guard = self.lock_operation(account, "group_recall").await?;
        let guard = self.lock_account(account).await?;
        let core = self.clone();
        let account = account.to_owned();
        let key = key.to_owned();
        let (op, created, guard) = blocking(move || {
            let (op, new) = core.admit_contact(&account, action, &key)?;
            Ok((op, new, guard))
        })
        .await?;
        if created {
            let core = self.clone();
            let account = op.account_id.clone();
            let id = op.id.clone();
            tokio::spawn(async move {
                let worker = core.clone();
                let a = account.clone();
                let i = id.clone();
                let result = blocking(move || {
                    let _operation = operation_guard;
                    let mut permit = crate::account_network::AccountNetworkPermit {
                        guard: Some(guard),
                        runtime: tokio::runtime::Handle::current(),
                    };
                    worker.execute_contact(&a, &i, &mut permit)
                })
                .await;
                if let Err(error) = result {
                    let code = match error.to_string().as_str() {
                        "stale_contact_privacy" => Some("stale_contact_privacy"),
                        "contact_privacy_unresolved" => Some("contact_privacy_unresolved"),
                        "contact_privacy_pending" => Some("contact_privacy_pending"),
                        _ => None,
                    };
                    if let Ok(guard) = core.lock_account(&account).await {
                        let _ = blocking(move || {
                            let _guard = guard;
                            core.hold_contact(&account, &id, code)
                        })
                        .await;
                    }
                }
            });
        }
        Ok(op)
    }
    fn admit_contact(
        &self,
        account: &str,
        action: ContactMutation,
        key: &str,
    ) -> Result<(Operation, bool)> {
        let hash = digest(&serde_json::to_vec(&action)?);
        if let Some(old) = self
            .store
            .contact_mutation_by_key(account, key, Some(&hash))?
        {
            return Ok((old.operation, false));
        }
        self.ensure_contact_privacy_guard_capacity(account, &action, None)?;
        let state = self.account_state(account)?;
        let mut target = None;
        let rpc = match &action {
            ContactMutation::SendRequest {
                recipient_ref,
                message,
                privacy,
            } => {
                let resolved = self.recipient(account, recipient_ref, &state)?;
                if privacy.is_some() {
                    self.ensure_no_pending_contact_privacy(account, &resolved.target.username)?;
                    self.ensure_no_active_contact_privacy(
                        account,
                        &resolved.target.username,
                        None,
                    )?;
                }
                let rpc = wechat_protocol::send_contact_request_rpc(
                    &state,
                    &SendContactRequest {
                        target: resolved.target.clone(),
                        message: message.clone(),
                        privacy: *privacy,
                    },
                )?;
                target = Some(resolved.target);
                Some(rpc)
            }
            ContactMutation::AcceptRequest {
                id,
                expected_version,
                ..
            }
            | ContactMutation::DeleteRequest {
                id,
                expected_version,
            } => {
                let row = self
                    .profile_reader_owned(account)?
                    .contact_request(&request_username(id)?)?;
                ensure!(&row.version == expected_version, "stale_contact_request");
                if matches!(&action, ContactMutation::AcceptRequest { .. }) {
                    if matches!(
                        &action,
                        ContactMutation::AcceptRequest {
                            privacy: Some(_),
                            ..
                        }
                    ) {
                        self.ensure_no_pending_contact_privacy(account, &row.username)?;
                        self.ensure_no_active_contact_privacy(account, &row.username, None)?;
                    }
                    ensure!(
                        self.request_view(account, &row)?["actionable"] == true,
                        "contact_request_not_actionable"
                    );
                    let t = ContactRequestTarget {
                        username: row.username,
                        ticket: row.ticket,
                        scene: row.scene,
                    };
                    let rpc = wechat_protocol::accept_contact_request_rpc(
                        &state,
                        &AcceptContactRequest {
                            target: t.clone(),
                            privacy: match &action {
                                ContactMutation::AcceptRequest { privacy, .. } => *privacy,
                                _ => None,
                            },
                        },
                    )?;
                    target = Some(t);
                    Some(rpc)
                } else {
                    None
                }
            }
            ContactMutation::RemoveContact { username } => {
                let contact = self.profile_reader_owned(account)?.contact(username)?;
                ensure!(
                    !contact.is_group
                        && username != &state.own_username
                        && !matches!(username.as_str(), "filehelper" | "weixin")
                        && flag(&contact),
                    "ordinary_contact_required"
                );
                Some(wechat_protocol::remove_contact_request(username)?)
            }
            ContactMutation::UpdatePrivacy {
                username,
                privacy,
                expected_version,
            } => {
                let before =
                    self.current_contact_privacy(account, username, Some(expected_version))?;
                self.ensure_no_active_contact_privacy(account, username, None)?;
                if before.privacy == *privacy {
                    None
                } else {
                    Some(wechat_protocol::contact_privacy_request(
                        &state,
                        username,
                        &before.privacy,
                        privacy,
                    )?)
                }
            }
        };
        self.write_options(account, "contact-admission")?;
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
        let stamp = now();
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.into(),
            kind: "contact_mutation".into(),
            state: "queued".into(),
            result: json!({"action":action.name(),"remoteOutcome":"not_attempted","profileProjection":"pending","retryable":false}),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let attempt = Attempt {
            action,
            rpc,
            target,
            own: state.own_username.clone(),
            uin: state.uin,
            generation: digest(&cipher),
            owner: self.ownership_generation.clone(),
            epoch,
        };
        self.store.create_contact_mutation(
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
    fn load_contact_attempt(
        &self,
        record: &wechat_store::StoredContactMutation,
    ) -> Result<(Attempt, Receipts)> {
        let account = &record.operation.account_id;
        let id = &record.operation.id;
        let a: Attempt = serde_json::from_slice(&self.vault.open_record(
            account,
            &purpose(id, "request"),
            &record.encrypted_request,
        )?)?;
        let r = serde_json::from_slice(&self.vault.open_record(
            account,
            &purpose(id, "receipts"),
            &record.encrypted_receipts,
        )?)?;
        ensure!(
            record.operation.kind == "contact_mutation"
                && record.operation.result["action"] == a.action.name(),
            "contact_operation_identity_conflict"
        );
        Ok((a, r))
    }
    fn hold_contact(&self, account: &str, id: &str, code: Option<&str>) -> Result<Operation> {
        let record = self.store.contact_mutation(account, id)?;
        let mut op = record.operation;
        if matches!(
            record.phase.as_str(),
            "queued" | "submitting" | "received" | "projecting"
        ) {
            let phase = if record.phase == "queued" {
                "failed"
            } else {
                "needs_review"
            };
            op.state = phase.into();
            op.result["code"] = json!(code.unwrap_or("contact_operation_interrupted"));
            op.updated_at = now();
            self.store
                .advance_contact_mutation(&op, phase, record.step)?;
        }
        Ok(op)
    }
    fn execute_contact(
        &self,
        account: &str,
        id: &str,
        permit: &mut crate::account_network::AccountNetworkPermit,
    ) -> Result<()> {
        let record = self.store.contact_mutation(account, id)?;
        if record.phase != "queued" {
            return Ok(());
        }
        let (attempt, mut receipts) = self.load_contact_attempt(&record)?;
        self.ensure_contact_privacy_guard_capacity(account, &attempt.action, Some(id))?;
        if matches!(attempt.action, ContactMutation::SendRequest { .. }) {
            self.ensure_request_database(account, id)?;
        }
        let mut state = self.account_state(account)?;
        let expected = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        ensure!(
            attempt.owner == self.ownership_generation
                && attempt.generation == digest(&expected)
                && attempt.own == state.own_username
                && attempt.uin == state.uin,
            "authorization_generation_changed"
        );
        let fence = self.session_fence(account)?;
        let desired = self.store.runtime(account, "desired")?;
        // Recheck the request version after queueing and before submitting.
        if let ContactMutation::AcceptRequest {
            id,
            expected_version,
            ..
        }
        | ContactMutation::DeleteRequest {
            id,
            expected_version,
        } = &attempt.action
        {
            let row = self
                .profile_reader_owned(account)?
                .contact_request(&request_username(id)?)?;
            ensure!(&row.version == expected_version, "stale_contact_request");
        }
        if let ContactMutation::UpdatePrivacy {
            username,
            expected_version,
            ..
        } = &attempt.action
        {
            self.current_contact_privacy(account, username, Some(expected_version))?;
            self.ensure_no_active_contact_privacy(account, username, Some(id))?;
        }
        if matches!(
            &attempt.action,
            ContactMutation::SendRequest {
                privacy: Some(_),
                ..
            } | ContactMutation::AcceptRequest {
                privacy: Some(_),
                ..
            }
        ) {
            let username = &attempt
                .target
                .as_ref()
                .context("contact_target_missing")?
                .username;
            self.ensure_no_pending_contact_privacy(account, username)?;
            self.ensure_no_active_contact_privacy(account, username, Some(id))?;
        }
        ensure!(
            self.store.begin_contact_step(
                account,
                id,
                0,
                &expected,
                attempt.epoch.as_deref(),
                &attempt.owner
            )?,
            "contact_step_conflict"
        );
        receipts.reply = if let Some(rpc) = &attempt.rpc {
            Some(permit.transfer(self, account, || {
                self.native_request(account, &mut state, |r| r.contact_rpc_raw(rpc))
            })?)
        } else {
            None
        };
        receipts.time = Some(wechat_store::unix_seconds());
        self.store.receive_contact_step(
            account,
            id,
            0,
            &self.vault.seal(
                account,
                &purpose(id, "receipts"),
                &serde_json::to_vec(&receipts)?,
            )?,
        )?;
        self.save_network_state(account, &state, &expected, &fence, desired.as_deref())?;
        self.finish_contact(account, id, &attempt, &mut receipts)
    }
    fn finish_contact(
        &self,
        account: &str,
        id: &str,
        attempt: &Attempt,
        receipts: &mut Receipts,
    ) -> Result<()> {
        let mut op = self.store.contact_mutation(account, id)?.operation;
        if let Some(rpc) = &attempt.rpc {
            let raw = receipts.reply.as_deref().context("contact_reply_missing")?;
            let mut partial = false;
            let accepted = if rpc.cgi == 3990 {
                let result = wechat_protocol::decode_contact_privacy_reply(rpc, raw)?;
                op.result["serviceCode"] = json!(result.service_code);
                op.result["itemCodes"] = json!(result.item_codes);
                op.result["partial"] = json!(result.partial);
                partial = result.partial;
                result.accepted
            } else if rpc.cgi == 681 {
                let r = wechat_protocol::decode_group_mutation(
                    &wechat_protocol::GroupRpc {
                        cgi: rpc.cgi,
                        path: rpc.path.clone(),
                        body: rpc.body.clone(),
                        opcode: Some(4),
                    },
                    raw,
                    &[],
                )?;
                ensure!(!r.uncertain, "contact_reply_unverified");
                r.accepted
            } else {
                let code =
                    wechat_protocol::protobuf::base_code(&wechat_protocol::protobuf::decode(raw)?)?;
                op.result["serviceCode"] = json!(code);
                code == 0
            };
            op.result["remoteOutcome"] = json!(if partial {
                "partial"
            } else if accepted {
                "accepted"
            } else {
                "rejected"
            });
            if !accepted {
                op.state = "failed".into();
                op.result["profileProjection"] = json!(if partial {
                    "awaiting_contact_sync"
                } else {
                    "not_applicable"
                });
                if partial {
                    op.result["code"] = json!("contact_privacy_partial");
                    if let ContactMutation::UpdatePrivacy { username, .. } = &attempt.action {
                        self.mark_partial_contact_privacy_pending(account, username, id)?;
                    }
                }
                op.updated_at = now();
                self.store.advance_contact_mutation(&op, "failed", 0)?;
                return Ok(());
            }
        } else {
            op.result["remoteOutcome"] = json!("not_applicable");
        }
        self.store.advance_contact_mutation(&op, "projecting", 0)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let options = self.write_options(account, id)?;
        match &attempt.action {
            ContactMutation::SendRequest { message, .. } => {
                if receipts.stage.is_none() {
                    let target = attempt.target.as_ref().context("contact_target_missing")?;
                    let reader = self.profile_reader_owned(account)?;
                    let old = match reader.contact_request(&target.username) {
                        Ok(r) => Some(r),
                        Err(wechat_profile::ProfileError::NotCached) => None,
                        Err(e) => return Err(e.into()),
                    };
                    let time = receipts.time.context("contact_receipt_time_missing")?;
                    let input = ContactRequestProjection {
                        username: target.username.clone(),
                        encrypted_username: target.username.clone(),
                        message_type: 37,
                        message: ContactRequestMessage {
                            server_id: "0".into(),
                            content: message.clone(),
                            timestamp: time,
                            sender: true,
                        },
                        ticket: String::new(),
                        scene: target.scene,
                        attention: true,
                        waiting: true,
                        aged: wechat_store::unix_seconds().saturating_sub(time) > 259200,
                    };
                    receipts.stage = Some(wechat_profile::stage_contact_request_projection(
                        old.as_ref(),
                        &input,
                    )?);
                    self.store.update_contact_receipts(
                        account,
                        id,
                        &self.vault.seal(
                            account,
                            &purpose(id, "receipts"),
                            &serde_json::to_vec(receipts)?,
                        )?,
                    )?;
                }
                wechat_profile::apply_contact_request_projection(
                    &location,
                    &keys,
                    receipts.stage.as_ref().unwrap(),
                    &options,
                )?;
                op.result["requestId"] =
                    json!(request_id(&attempt.target.as_ref().unwrap().username));
            }
            ContactMutation::DeleteRequest {
                id: request,
                expected_version,
            } => {
                wechat_profile::apply_contact_request_deletion(
                    &location,
                    &keys,
                    &request_username(request)?,
                    expected_version,
                    &options,
                )?;
                op.result["requestId"] = json!(request);
                op.result["scope"] = json!("local_history");
            }
            ContactMutation::AcceptRequest { id: request, .. } => {
                op.result["requestId"] = json!(request);
                op.result["relationship"] = json!("awaiting_contact_sync");
            }
            ContactMutation::RemoveContact { username } => {
                op.result["peerId"] = json!(username);
                op.result["relationship"] = json!("awaiting_contact_sync");
            }
            ContactMutation::UpdatePrivacy {
                username, privacy, ..
            } => {
                op.result["peerId"] = json!(username);
                op.result["privacy"] = json!(privacy);
                op.result["changed"] = json!(attempt.rpc.is_some());
            }
        }
        let awaits_contact_sync =
            matches!(
                attempt.action,
                ContactMutation::AcceptRequest { .. } | ContactMutation::RemoveContact { .. }
            ) || matches!(attempt.action, ContactMutation::UpdatePrivacy { .. })
                && attempt.rpc.is_some();
        op.result["profileProjection"] = json!(if awaits_contact_sync {
            "awaiting_contact_sync"
        } else {
            "complete"
        });
        op.state = "succeeded".into();
        op.updated_at = now();
        self.store.advance_contact_mutation(&op, "succeeded", 0)?;
        // Sync can finish while the network permit is released. Reconcile the
        // canonical row after publishing the waiting operation as well.
        let target = match &attempt.action {
            ContactMutation::AcceptRequest { .. } => {
                attempt.target.as_ref().map(|t| t.username.as_str())
            }
            ContactMutation::RemoveContact { username } => Some(username.as_str()),
            ContactMutation::UpdatePrivacy { username, .. } if attempt.rpc.is_some() => {
                Some(username.as_str())
            }
            _ => None,
        };
        if let Some(target) = target {
            if let Some(contact) = self
                .profile_reader_owned(account)?
                .contact_for_request(target)?
            {
                let member = flag(&contact);
                if member
                    == matches!(
                        attempt.action,
                        ContactMutation::AcceptRequest { .. }
                            | ContactMutation::UpdatePrivacy { .. }
                    )
                {
                    self.settle_contact_relationship(account, &contact.username, member, id)?;
                }
            }
        }
        let privacy_update = matches!(attempt.action, ContactMutation::UpdatePrivacy { .. });
        self.store.append_event_once(
            account,
            &format!("contact-mutation-event:{id}"),
            if privacy_update { "contact.updated" } else { "friend_request.updated" },
            &json!({"operationId":id,"action":attempt.action.name(),"requestId":op.result["requestId"],"contactId":op.result["peerId"]}),
        )?;
        Ok(())
    }
    pub(super) fn settle_contact_relationship(
        &self,
        account: &str,
        username: &str,
        member: bool,
        receipt: &str,
    ) -> Result<()> {
        let current = if member {
            self.profile_reader_owned(account)?.contact(username).ok()
        } else {
            None
        };
        let saved_privacy = if self.contact_privacy_resolved(account, username)? {
            current
                .as_ref()
                .and_then(|contact| canonical_privacy(account, username, contact).ok())
                .map(|snapshot| snapshot.privacy)
        } else {
            None
        };
        if member {
            let reader = self.profile_reader_owned(account)?;
            let alias = current
                .as_ref()
                .and_then(|c| match c.raw_fields.get("encrypt_username") {
                    Some(wechat_profile::SqlValue::Text(v)) if !v.is_empty() => Some(v.clone()),
                    _ => None,
                });
            for identity in std::iter::once(username).chain(alias.as_deref()) {
                if let Ok(row) = reader.contact_request(identity) {
                    if row.attention == Some(true) {
                        let key = format!(
                            "friend-attention-stage:{receipt}:{}",
                            digest(row.username.as_bytes())
                        );
                        let stage = if let Some(raw) = self.store.runtime(account, &key)? {
                            serde_json::from_slice(&self.vault.open_record(account, &key, &raw)?)?
                        } else {
                            let stage = wechat_profile::stage_contact_request_attention(&row)?;
                            self.store.put_runtime(
                                account,
                                &key,
                                &self
                                    .vault
                                    .seal(account, &key, &serde_json::to_vec(&stage)?)?,
                            )?;
                            stage
                        };
                        let (location, keys) = self.profile_location_keys(account)?;
                        wechat_profile::apply_contact_request_projection(
                            &location,
                            &keys,
                            &stage,
                            &self.write_options(account, receipt)?,
                        )?;
                        self.store.append_event_once(
                            account,
                            &format!(
                                "friend-attention:{receipt}:{}",
                                digest(row.username.as_bytes())
                            ),
                            "friend_request.updated",
                            &json!({"requestId":request_id(&row.username),"source":"contact_sync"}),
                        )?;
                    }
                }
            }
        }
        for record in self.store.pending_contact_relationships(account)? {
            if record.operation.state != "succeeded" {
                continue;
            }
            let (a, _) = self.load_contact_attempt(&record)?;
            let matches = match &a.action {
                ContactMutation::AcceptRequest { privacy, .. } => {
                    member
                        && privacy.is_none_or(|wanted| saved_privacy == Some(wanted))
                        && a.target.as_ref().is_some_and(|t| {
                            t.username == username
                                || self
                                    .profile_reader_owned(account)
                                    .and_then(|r| Ok(r.contact_for_request(&t.username)?))
                                    .ok()
                                    .flatten()
                                    .is_some_and(|c| c.username == username)
                        })
                }
                ContactMutation::RemoveContact { username: peer } => !member && peer == username,
                ContactMutation::UpdatePrivacy {
                    username: peer,
                    privacy,
                    ..
                } => member && peer == username && saved_privacy == Some(*privacy),
                _ => false,
            };
            if matches {
                let mut op = record.operation;
                op.result["profileProjection"] = json!("complete");
                op.result["relationship"] = json!(if member { "contact" } else { "removed" });
                op.updated_at = now();
                self.store.advance_contact_mutation(&op, "succeeded", 0)?;
            }
        }
        Ok(())
    }

    pub(super) fn settle_partial_contact_privacy(
        &self,
        account: &str,
        username: &str,
    ) -> Result<()> {
        for record in self.store.pending_contact_relationships(account)? {
            if record.operation.state != "failed" || record.operation.result["partial"] != true {
                continue;
            }
            let (attempt, _) = self.load_contact_attempt(&record)?;
            if matches!(&attempt.action, ContactMutation::UpdatePrivacy { username: peer, .. } if peer == username)
            {
                let mut op = record.operation;
                op.result["profileProjection"] = json!("complete");
                op.updated_at = now();
                self.store
                    .advance_contact_mutation(&op, "failed", record.step)?;
            }
        }
        Ok(())
    }
    pub(super) fn apply_received_contact_removal(
        &self,
        account: &str,
        username: &str,
        receipt: &str,
        origin: &str,
    ) -> Result<bool> {
        let own = self.account_state(account)?.own_username.clone();
        if username.ends_with("@chatroom") {
            return Ok(false);
        }
        if username == own || matches!(username, "filehelper" | "weixin") {
            self.store.put_runtime(account, receipt, b"1")?;
            return Ok(true);
        }
        let old = match self.profile_reader_owned(account)?.contact(username) {
            Ok(row) => Some(row),
            Err(wechat_profile::ProfileError::NotCached) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(row) = old {
            let flag = match row.raw_fields.get("flag") {
                Some(wechat_profile::SqlValue::Integer(n)) => n.parse::<u32>()?,
                _ => bail!("contact_flags_unresolved"),
            };
            let mut fields = wechat_profile::RawFields::new();
            fields.insert(
                "flag".into(),
                wechat_profile::SqlValue::Integer((flag & !1).to_string()),
            );
            fields.insert(
                "local_type".into(),
                wechat_profile::SqlValue::Integer(
                    super::contact_classification::deleted_contact_local_type(username).to_string(),
                ),
            );
            let (location, keys) = self.profile_location_keys(account)?;
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &wechat_profile::ContactProjection {
                    username: username.into(),
                    fields,
                },
                &self.write_options(account, receipt)?,
            )?;
        }
        self.settle_contact_relationship(account, username, false, receipt)?;
        self.store.append_event_once(
            account,
            &format!("contact-delete:{receipt}"),
            "contact.updated",
            &json!({"contactId":username,"relationship":"removed","origin":origin}),
        )?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(true)
    }

    pub(super) fn repair_contact_mutations(&self, account: &str) -> Result<()> {
        for record in self.store.projecting_contact_mutations(account)? {
            let (a, mut r) = self.load_contact_attempt(&record)?;
            if a.rpc.is_none() || r.reply.is_some() {
                let _ = self.finish_contact(account, &record.operation.id, &a, &mut r);
            }
        }
        Ok(())
    }
    pub(super) fn apply_received_friend(
        &self,
        account: &str,
        message: &wechat_protocol::wire::AddMessage,
        receipt: &str,
    ) -> Result<bool> {
        let own = self.account_state(account)?.own_username.clone();
        let Some(friend) = wechat_protocol::decode_friend_message(message, &own)? else {
            return Ok(false);
        };
        self.ensure_request_database(account, receipt)?;
        let purpose = format!("friend-message-stage:{receipt}");
        let stage: StagedContactRequest = if let Some(raw) =
            self.store.runtime(account, &purpose)?
        {
            serde_json::from_slice(&self.vault.open_record(account, &purpose, &raw)?)?
        } else {
            let old = match self
                .profile_reader_owned(account)?
                .contact_request(&friend.row_username)
            {
                Ok(r) => Some(r),
                Err(wechat_profile::ProfileError::NotCached) => None,
                Err(e) => return Err(e.into()),
            };
            let waiting = match friend.operation {
                1 | 2 | 5 => friend.sender,
                3 | 4 | 6 => !friend.sender,
                _ => true,
            };
            let input = ContactRequestProjection {
                username: old
                    .as_ref()
                    .map(|r| r.username.clone())
                    .unwrap_or(friend.row_username),
                encrypted_username: friend.encrypted_username,
                message_type: friend.message_type.into(),
                message: ContactRequestMessage {
                    server_id: friend.server_id,
                    content: friend.content,
                    timestamp: friend.timestamp.into(),
                    sender: friend.sender,
                },
                ticket: friend.target.ticket,
                scene: friend.target.scene,
                attention: true,
                waiting,
                aged: wechat_store::unix_seconds().saturating_sub(friend.timestamp.into()) > 259200,
            };
            let stage = wechat_profile::stage_contact_request_projection(old.as_ref(), &input)?;
            self.store.put_runtime(
                account,
                &purpose,
                &self
                    .vault
                    .seal(account, &purpose, &serde_json::to_vec(&stage)?)?,
            )?;
            stage
        };
        let (location, keys) = self.profile_location_keys(account)?;
        wechat_profile::apply_contact_request_projection(
            &location,
            &keys,
            &stage,
            &self.write_options(account, receipt)?,
        )?;
        self.store.append_event_once(
            account,
            &format!("friend-message-event:{receipt}"),
            "friend_request.updated",
            &json!({"requestId":request_id(&stage.row.username),"source":"remote_sync"}),
        )?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "contact_privacy_tests.rs"]
mod tests;
