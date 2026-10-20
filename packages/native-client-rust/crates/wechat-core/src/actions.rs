use super::*;
impl Core {
    pub(super) async fn recover_read_projections(self: &Arc<Self>, account: &str) -> Result<usize> {
        let mut completed = 0;
        for (thread, intent) in self.store.pending_read_intents(account)? {
            let Some(expected) = intent["expectedVersion"].as_str() else {
                continue;
            };
            let Ok(ids) = serde_json::from_value::<Vec<String>>(intent["messageIds"].clone())
            else {
                continue;
            };
            if self
                .mark_read_set(account, &thread, ids, expected)
                .await
                .is_ok()
            {
                completed += 1;
            }
        }
        Ok(completed)
    }
    /// Admission persists the receipt before the HTTP task can disconnect.
    /// Unfinished read/sync operations are reported as interrupted after restart;
    /// already-staged sync batches are independently recovered before checkpointing.
    pub(crate) fn start_operation<F, Fut>(
        self: &Arc<Self>,
        account: &str,
        kind: &str,
        task: F,
    ) -> Result<Operation>
    where
        F: FnOnce(Arc<Core>, String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        self.store.account(account)?;
        let stamp = now();
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.into(),
            kind: kind.into(),
            state: "queued".into(),
            result: json!({}),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        self.store.save_operation(&op)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?
            .insert(op.id.clone(), cancel.clone());
        let core = self.clone();
        let worker = op.clone();
        let account = account.into();
        tokio::spawn(async move {
            if cancel.load(Ordering::SeqCst) || core.stopping.load(Ordering::SeqCst) {
                let _ = core.progress(&worker, "cancelled", json!({}));
            } else {
                let _ = core.progress(&worker, "running", json!({}));
                let result = task(core.clone(), account).await;
                if !cancel.load(Ordering::SeqCst) {
                    match result {
                        Ok(value) => {
                            let _ = core.progress(&worker, "succeeded", value);
                        }
                        Err(error) => {
                            let code = if error.to_string().contains("auth_required") {
                                "auth_required"
                            } else if error.to_string().contains("identity_mismatch") {
                                "account_identity_mismatch"
                            } else {
                                "operation_failed"
                            };
                            let _ = core.progress(&worker, "failed", json!({"code":code}));
                        }
                    }
                }
            }
            if let Ok(mut map) = core.cancels.lock() {
                map.remove(&worker.id);
            }
        });
        Ok(op)
    }
    pub fn sync_operation(self: &Arc<Self>, account: &str) -> Result<Operation> {
        self.start_operation(account, "sync", |core, account| async move {
            core.sync(&account).await
        })
    }
    /// Reconcile already accepted/cached bytes into supported official files.
    /// This operation never submits a send or requests a network transfer.
    pub fn repair_media_writeback_operation(
        self: &Arc<Self>,
        account: &str,
        limit: usize,
    ) -> Result<Operation> {
        if !(1..=100).contains(&limit) {
            bail!("invalid_media_repair_limit");
        }
        self.start_operation(
            account,
            "media-writeback",
            move |core, account| async move {
                let guard = core.lock_account(&account).await?;
                blocking(move || {
                    let _guard = guard;
                    core.repair_media_writeback(&account, limit)
                })
                .await
            },
        )
    }
    pub async fn connect(self: &Arc<Self>, account: &str) -> Result<Operation> {
        self.login(
            account,
            LoginRequest {
                allow_qr: false,
                ..Default::default()
            },
        )
        .await
    }
}
