//! Server-owned native queue submission. Media bytes remain owned by WeChat.
use std::{collections::HashMap, process::Stdio, sync::OnceLock, time::{Duration, Instant}};
use serde_json::{json, Value};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin, ChildStdout, Command}, sync::Mutex};
use super::wechat_db::{find_account_dir, find_wechat_pid};
use crate::sessions::manager::get_session;

struct Worker {
    identity: String,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

#[derive(Default)]
struct Queue {
    worker: Option<Worker>,
    // Remember uncertain submissions too: an HTTP cancellation is not a retry.
    submitted: HashMap<String, Instant>,
}
static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
const RETRY_AFTER: Duration = Duration::from_secs(300);

pub fn current_process(account: &str) -> Option<(i64, String)> {
    if get_session("default")?.logged_in_user.as_deref()? != account { return None; }
    let pid = find_wechat_pid()?;
    if find_account_dir(pid).as_deref()? != account { return None; }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some((pid, format!("{pid}:{start}:{account}")))
}

impl Worker {
    fn start(identity: String) -> Option<Self> {
        let mut child = Command::new("python3")
            .arg("/opt/tools/media-download.py")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true).spawn().ok()?;
        let input = child.stdin.take()?;
        let output = BufReader::new(child.stdout.take()?);
        Some(Self { identity, child, input, output })
    }

    async fn submit(&mut self, pid: i64, metadata: Value) -> Option<bool> {
        let mut request = serde_json::to_vec(&json!({"pid": pid, "metadata": metadata})).ok()?;
        request.push(b'\n');
        self.input.write_all(&request).await.ok()?;
        self.input.flush().await.ok()?;
        let mut line = String::new();
        if self.output.read_line(&mut line).await.ok()? == 0 { return None; }
        let response: Value = serde_json::from_str(&line).ok()?;
        Some(response["status"] == "queued")
    }
}

/// A detached task owns the entire IPC exchange even if the HTTP caller leaves.
pub async fn ensure_queued(account: String, metadata: Value) -> bool {
    let task = tokio::spawn(async move {
        let queue = QUEUE.get_or_init(|| Mutex::new(Queue::default()));
        let Ok(mut queue) = tokio::time::timeout(Duration::from_secs(10), queue.lock()).await else { return false; };
        let Some((pid, identity)) = current_process(&account) else { return false; };
        queue.submitted.retain(|_, time| time.elapsed() < RETRY_AFTER);
        // Full and thumbnail requests share the native full-image transfer.
        let key = format!("{}:{}:{}:{}", identity, metadata["chatId"], metadata["local_id"], metadata["server_id"]);
        if queue.submitted.contains_key(&key) { return true; }
        if queue.submitted.len() >= 256 { return false; }
        if queue.worker.as_ref().map(|w| w.identity.as_str()) != Some(identity.as_str()) {
            if let Some(mut old) = queue.worker.take() { let _ = old.child.kill().await; }
            queue.worker = Worker::start(identity);
        }
        if queue.worker.is_none() { return false; }
        queue.submitted.insert(key, Instant::now());
        // The helper cancels undispatched UI sources itself. Keep owning this
        // exchange even if attach or a native callback stalls: killing the
        // helper here could unload a callback still owned by WeChat's loop.
        let outcome = queue.worker.as_mut().unwrap().submit(pid, metadata).await;
        if matches!(outcome, Some(true)) {
            tracing::info!("[media] native transfer queued");
            true
        } else {
            tracing::warn!("[media] native transfer unavailable; retaining retry cooldown");
            if let Some(mut worker) = queue.worker.take() { let _ = worker.child.kill().await; }
            false
        }
    });
    // Dropping a JoinHandle detaches, rather than cancels, the owning task.
    tokio::time::timeout(Duration::from_secs(20), task).await
        .ok().and_then(Result::ok).unwrap_or(false)
}
