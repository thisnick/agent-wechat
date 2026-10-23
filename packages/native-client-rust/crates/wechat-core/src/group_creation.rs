//! Group creation is enabled only by an upstream with a verified implementation.
use super::*;
use anyhow::ensure;

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupCreateRequest {
    pub name: String,
    pub members: Vec<String>,
}
impl GroupCreateRequest {
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 80
                && !self.name.chars().any(char::is_control)
                && (2..=39).contains(&self.members.len())
                && self.members.iter().all(|member| !member.is_empty()
                    && member.len() <= 256
                    && member != "filehelper"
                    && !member.ends_with("@chatroom")
                    && !member.chars().any(char::is_control)
                    && seen.insert(member)),
            "invalid_group_creation"
        );
        Ok(())
    }
}
fn operation_id(key: &str) -> Result<String> {
    ensure!(
        !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control),
        "invalid_group_creation"
    );
    Ok(format!("group-create-{:x}", Sha256::digest(key.as_bytes())))
}
impl Core {
    pub fn group_creation_by_key(&self, account: &str, key: &str) -> Result<Operation> {
        self.store.operation(account, &operation_id(key)?)
    }
    pub async fn create_group(
        self: &Arc<Self>,
        account: &str,
        mut input: GroupCreateRequest,
        key: &str,
    ) -> Result<Operation> {
        ensure!(
            self.upstream.can_create_groups(),
            "unsupported_group_creation"
        );
        ensure!(!self.stopping.load(Ordering::Acquire), "service_stopping");
        input.name = input.name.trim().into();
        input.members.sort();
        input.validate()?;
        let id = operation_id(key)?;
        let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&input)?));
        let stamp = now();
        let op = Operation {
            id,
            account_id: account.into(),
            kind: "group_create".into(),
            state: "queued".into(),
            result: json!({"requestHash":request_hash,"retryable":false}),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let guard = self.lock_account(account).await?;
        if let Ok(prior) = self.store.operation(account, &op.id) {
            ensure!(
                prior.result["requestHash"] == request_hash,
                "idempotency_conflict"
            );
            return Ok(prior);
        }
        let state = self.account_state(account)?;
        ensure!(
            self.store
                .runtime(account, "validated-generation")?
                .as_deref()
                == Some(self.ownership_generation.as_bytes()),
            "auth_required"
        );
        ensure!(
            !input.members.contains(&state.own_username),
            "invalid_group_creation"
        );
        for member in &input.members {
            self.catalog_item(account, "contacts", member)?;
        }
        let authorization = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let epoch = self.store.runtime(account, "logout-epoch")?;
        let (operation, created) =
            self.store
                .admit_group_creation(&op, &authorization, epoch.as_deref())?;
        drop(guard);
        if created {
            let core = self.clone();
            let mut worker = operation.clone();
            tokio::spawn(async move {
                let account = worker.account_id.clone();
                let outcome = async {
                    let guard = core.lock_account(&account).await?;
                    let core = core.clone();
                    let mut op = worker.clone();
                    blocking(move || {
                        let _guard = guard;
                        let mut state = core.account_state(&account)?;
                        op.updated_at = now();
                        core.store
                            .begin_group_submission(&op, &authorization, epoch.as_deref())?;
                        let response = core.upstream.create_group(&mut state, &input, &op.id);
                        match response {
                            Ok(room) => {
                                ensure!(
                                    room.ends_with("@chatroom") && room.len() <= 256,
                                    "invalid_group_creation_reply"
                                );
                                op.state = "succeeded".into();
                                op.result["roomId"] = json!(room);
                                op.result["authorizationSaved"] =
                                    json!(core.save_state(&account, &state).is_ok());
                            }
                            Err(_) => {
                                op.state = "needs_review".into();
                                op.result["code"] = json!("group_creation_outcome_unknown");
                            }
                        }
                        op.updated_at = now();
                        core.store.save_operation(&op)
                    })
                    .await
                }
                .await;
                if outcome.is_err() {
                    if let Ok(current) = core.store.operation(&worker.account_id, &worker.id) {
                        worker = current;
                    }
                    if matches!(worker.state.as_str(), "queued" | "submitting") {
                        worker.state = if worker.state == "submitting" {
                            "needs_review"
                        } else {
                            "failed"
                        }
                        .into();
                        worker.result["code"] = json!("group_creation_interrupted");
                        worker.updated_at = now();
                        let _ = core.store.save_operation(&worker);
                    }
                }
            });
        }
        Ok(operation)
    }
}
