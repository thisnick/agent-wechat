use super::*;
use futures_util::StreamExt;
use std::io::Write;

#[derive(Default)]
struct Frames {
    buffer: Vec<u8>,
    data: Vec<String>,
    size: usize,
    event_type: String,
    id: String,
}
impl Frames {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 1024 * 1024 {
            bail!("event frame exceeds bound");
        }
        let mut output = vec![];
        while let Some(index) = self.buffer.iter().position(|b| *b == b'\n') {
            let bytes = self.buffer.drain(..=index).collect::<Vec<_>>();
            let line = std::str::from_utf8(&bytes[..bytes.len() - 1])
                .context("invalid UTF8 event")?
                .trim_end_matches('\r');
            if line.is_empty() {
                if !self.data.is_empty() {
                    let mut value: Value = serde_json::from_str(&self.data.join("\n"))?;
                    if self.event_type == "error" {
                        bail!(
                            "event stream requires recovery: {}",
                            value["error"]["code"].as_str().unwrap_or("stream_failed")
                        );
                    }
                    if value["cursor"].is_null() {
                        value["cursor"] = json!(self.id);
                    }
                    let cursor = value["cursor"]
                        .as_str()
                        .context("event cursor must be a decimal string")?;
                    cursor.parse::<u64>().context("invalid event cursor")?;
                    if !cursor.bytes().all(|b| b.is_ascii_digit()) {
                        bail!("invalid event cursor");
                    }
                    output.push(value);
                }
                self.data.clear();
                self.size = 0;
                self.event_type.clear();
                self.id.clear();
            } else if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                self.size += data.len();
                if self.size > 1024 * 1024 {
                    bail!("event frame exceeds bound");
                }
                self.data.push(data.into());
            } else if let Some(id) = line.strip_prefix("id:") {
                self.id = id.trim_start_matches(' ').into();
            } else if let Some(kind) = line.strip_prefix("event:") {
                self.event_type = kind.trim_start_matches(' ').into();
            }
        }
        Ok(output)
    }
}
fn expired_cursor(error: &anyhow::Error) -> bool {
    error.to_string().contains("event_cursor_expired")
}
/// An expired cursor stays an error: the events after it are gone, so a silent
/// restart from the newest event would hide the gap. `saved_in` names the
/// cursor file when it holds `after`; `explicit` means `after` is still `--after`.
fn expired(after: &str, saved_in: Option<&Path>, explicit: bool) -> anyhow::Error {
    let message = match saved_in {
        Some(path) => format!(
            "event cursor {after} saved in {} is older than the oldest event the daemon keeps; remove the file to start from the newest event",
            path.display()
        ),
        None if explicit => format!(
            "event cursor {after} is older than the oldest event the daemon keeps; omit --after to start from the newest event"
        ),
        None => format!(
            "events after cursor {after} are no longer kept by the daemon; run the watch again without --after to start from the newest event"
        ),
    };
    failure(9, "event_cursor_expired", &message)
}
/// Read the most recent events. Its `nextCursor` is the newest event's cursor.
async fn newest(r: &Remote, limit: Option<usize>) -> Result<Value> {
    let path = query(
        r.path("events")?,
        &[
            ("newest", Some("true".into())),
            ("limit", limit.map(|v| v.to_string())),
        ],
    );
    r.get(&path).await.map_err(|error| {
        // Daemons without `newest` reject the unknown query field with a plain-text 400.
        if outcome::classify(&error) == (9, "request_failed".into()) {
            failure(
                9,
                "request_failed",
                "the daemon cannot read the newest events; upgrade it to match this CLI, or pass --after CURSOR",
            )
        } else {
            error
        }
    })
}
/// Read one page of events: those after `after`, or the most recent ones without it.
pub async fn list(r: &Remote, after: Option<String>, limit: Option<usize>) -> Result<Value> {
    let Some(after) = after else {
        return newest(r, limit).await;
    };
    let path = query(
        r.path("events")?,
        &[
            ("after", Some(after.clone())),
            ("limit", limit.map(|v| v.to_string())),
        ],
    );
    r.get(&path).await.map_err(|error| {
        if expired_cursor(&error) {
            failure(
                9,
                "event_cursor_expired",
                &format!("event cursor {after} is older than the oldest event the daemon keeps; omit --after to list the most recent events"),
            )
        } else {
            error
        }
    })
}
pub async fn watch(
    r: &Remote,
    after: Option<String>,
    cursor_file: Option<PathBuf>,
    messages_only: bool,
    json: bool,
) -> Result<()> {
    watch_with(r, after, cursor_file, messages_only, |event| {
        let mut stdout = std::io::stdout().lock();
        if json {
            writeln!(stdout, "{}", serde_json::to_string(event)?)?;
        } else {
            writeln!(stdout, "{}", super::output::details(event))?;
        }
        stdout.flush()
    })
    .await
}
/// Follow the event stream, passing each event to `emit`. A broken pipe from
/// `emit` ends the watch successfully.
async fn watch_with(
    r: &Remote,
    explicit_cursor: Option<String>,
    cursor_file: Option<PathBuf>,
    messages_only: bool,
    mut emit: impl FnMut(&Value) -> std::io::Result<()>,
) -> Result<()> {
    if let Some(after) = &explicit_cursor {
        if after.parse::<u64>().is_err() || !after.bytes().all(|b| b.is_ascii_digit()) {
            bail!("invalid event cursor");
        }
    }
    let status = r.get("v1/status").await?;
    let server = status["serverId"]
        .as_str()
        .context("daemon identity missing")?;
    let account = r.account()?;
    let mut resumed = None;
    if let Some(path) = &cursor_file {
        if path.exists() {
            let saved: Value = serde_json::from_slice(&private_input(path, 16384)?)?;
            if saved["serverId"] != server
                || saved["accountId"] != account
                || saved["progress"] != "emission"
            {
                bail!("event cursor file belongs to another daemon/account or progress type");
            }
            let cursor = saved["cursor"]
                .as_str()
                .context("invalid event cursor file")?;
            if explicit_cursor
                .as_deref()
                .is_some_and(|after| after != "0" && after != cursor)
            {
                bail!("explicit cursor conflicts with emission checkpoint");
            }
            cursor.parse::<u64>().context("invalid event cursor file")?;
            resumed = Some(cursor.to_string());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || !wechat_platform::private_path(parent)? {
            bail!("cursor file requires a private owned directory");
        }
    }
    let checkpoint = |after: &str| -> Result<()> {
        match &cursor_file {
            Some(path) => client_actions::write_replace(
                path,
                &json!({"schemaVersion":1,"serverId":server,"accountId":account,"cursor":after,"progress":"emission"}),
            ),
            None => Ok(()),
        }
    };
    // Whether the cursor file holds `after`. A new file records where the watch
    // began once the daemon accepts that cursor, so a later run resumes there
    // even if no event arrived.
    let mut saved = resumed.is_some();
    let mut explicit = resumed.is_none() && explicit_cursor.is_some();
    let mut after = match resumed.or(explicit_cursor) {
        Some(cursor) => cursor,
        None => {
            let page = newest(r, Some(1)).await?;
            page["nextCursor"]
                .as_str()
                .filter(|v| v.parse::<u64>().is_ok() && v.bytes().all(|b| b.is_ascii_digit()))
                .context("invalid newest event cursor")?
                .to_string()
        }
    };
    let saved_in = |saved: bool| cursor_file.as_deref().filter(|_| saved);
    let mut backoff = 250;
    loop {
        let path = query(r.path("events/stream")?, &[("after", Some(after.clone()))]);
        let response = tokio::select! {_=tokio::signal::ctrl_c()=>{super::set_exit(130);return Ok(())},response=r.response(reqwest::Method::GET,&path,None,None)=>response};
        let response = match response {
            Ok(value) => value,
            Err(error) => {
                if expired_cursor(&error) {
                    return Err(expired(&after, saved_in(saved), explicit));
                }
                let message = error.to_string();
                if message.contains("401") || message.contains("403") {
                    return Err(error);
                }
                eprintln!("Event connection unavailable; retained cursor will resume");
                tokio::select! {_=tokio::signal::ctrl_c()=>{super::set_exit(130);return Ok(())},_=tokio::time::sleep(Duration::from_millis(backoff))=>{}}
                backoff = (backoff * 2).min(30000);
                continue;
            }
        };
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            bail!("event endpoint did not return SSE");
        }
        if cursor_file.is_some() && !saved {
            checkpoint(&after)?;
            saved = true;
        }
        let mut stream = response.bytes_stream();
        let mut parser = Frames::default();
        loop {
            let next = tokio::select! {_=tokio::signal::ctrl_c()=>{super::set_exit(130);return Ok(())},next=stream.next()=>next};
            let Some(chunk) = next else {
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => break,
            };
            let events = match parser.feed(&chunk) {
                Ok(events) => events,
                Err(error) if expired_cursor(&error) => {
                    return Err(expired(&after, saved_in(saved), explicit));
                }
                Err(error) => return Err(error),
            };
            for mut event in events {
                if event.get("accountId").is_some_and(|v| v != account) {
                    bail!("cross-account event rejected");
                }
                event["accountId"] = json!(account);
                let cursor = event["cursor"]
                    .as_str()
                    .context("missing event cursor")?
                    .to_string();
                if cursor.parse::<u64>()? <= after.parse::<u64>()? {
                    bail!("event cursor did not advance; retained checkpoint requires review");
                }
                if !messages_only
                    || event["kind"]
                        .as_str()
                        .is_some_and(|v| v.starts_with("message."))
                {
                    if let Err(error) = emit(&event) {
                        if error.kind() == std::io::ErrorKind::BrokenPipe {
                            return Ok(());
                        }
                        return Err(error.into());
                    }
                }
                after = cursor;
                explicit = false;
                backoff = 250;
                checkpoint(&after)?;
            }
        }
        tokio::select! {_=tokio::signal::ctrl_c()=>{super::set_exit(130);return Ok(())},_=tokio::time::sleep(Duration::from_millis(backoff))=>{}}
        backoff = (backoff * 2).min(30000);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_parser_handles_chunked_unicode_and_emits_json_not_sse_framing() {
        let bytes="id: 9007199254740999\nevent: message\ndata: {\"kind\":\"message.committed\",\"data\":{\"body\":\"测试\"}}\n\n".as_bytes();
        let mut parser = Frames::default();
        let mut events = vec![];
        for chunk in bytes.chunks(3) {
            events.extend(parser.feed(chunk).unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["cursor"], "9007199254740999");
        assert_eq!(events[0]["data"]["body"], "测试");
    }
    /// A daemon whose account holds one more event than it keeps, so the
    /// oldest kept event is 2 and cursor 0 has expired.
    async fn trimmed_daemon() -> (tempfile::TempDir, Arc<wechat_store::Store>, Remote) {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&temp.path().join("data")).unwrap());
        store.create_account("busy").unwrap();
        for n in 0..=wechat_store::EVENT_LIMIT {
            store
                .append_event(
                    "busy",
                    "message.committed",
                    &json!({"messageId":format!("old-{n}")}),
                )
                .unwrap();
        }
        assert!(store.events("busy", 0, 1).is_err());
        let token = "0123456789abcdef0123456789abcdef";
        let app = wechat_api::router(Core::new(store.clone(), None).unwrap(), token);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let remote = Remote {
            http: reqwest::Client::new(),
            base: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            token: token.into(),
            account: Some("busy".into()),
        };
        (temp, store, remote)
    }
    /// Seeding the account is slow, so one test covers both the default start
    /// and the cursors that must still fail.
    #[tokio::test]
    async fn watch_without_after_starts_at_the_newest_event_when_cursor_0_has_expired() {
        let (temp, store, remote) = trimmed_daemon().await;
        let private = |name: &str| {
            let directory = temp.path().join(name);
            wechat_platform::private_dir(&directory).unwrap();
            directory.join("events.cursor")
        };

        // Without --after, a listing shows the newest events.
        let head = store.newest_events("busy", 1).unwrap()[0].cursor.clone();
        let page = list(&remote, None, Some(2)).await.unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert_eq!(page["items"][1]["cursor"], head);
        assert_eq!(page["nextCursor"], head);

        // Without --after, the watch starts at the newest event and gets the next one.
        let file = private("new");
        let mut seen = vec![];
        let watch = watch_with(&remote, None, Some(file.clone()), true, |event| {
            seen.push(event.clone());
            Err(std::io::ErrorKind::BrokenPipe.into())
        });
        let feed = async {
            // A new cursor file is written once the daemon accepts the starting cursor.
            while !file.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(saved["cursor"], head);
            store
                .append_event("busy", "message.committed", &json!({"messageId":"new"}))
                .unwrap();
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(watch, feed) })
                .await
                .expect("the watch received the new event");
        result.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["data"]["messageId"], "new", "no old event replayed");
        assert!(
            seen[0]["cursor"].as_str().unwrap().parse::<u64>().unwrap() > head.parse().unwrap()
        );

        // An explicit expired cursor still fails, and says how to start from now.
        let expired = (9, "event_cursor_expired".to_string());
        let error = watch_with(&remote, Some("0".into()), None, false, |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(outcome::classify(&error), expired);
        assert_eq!(
            error.to_string(),
            "event cursor 0 is older than the oldest event the daemon keeps; omit --after to start from the newest event"
        );
        let error = list(&remote, Some("0".into()), None).await.unwrap_err();
        assert_eq!(outcome::classify(&error), expired);
        assert!(error
            .to_string()
            .contains("omit --after to list the most recent events"));

        // An expired saved cursor names its file instead.
        let file = private("saved");
        let server = remote.get("v1/status").await.unwrap()["serverId"].clone();
        client_actions::write_replace(
            &file,
            &json!({"schemaVersion":1,"serverId":server,"accountId":"busy","cursor":"0","progress":"emission"}),
        )
        .unwrap();
        let error = watch_with(&remote, None, Some(file.clone()), false, |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(outcome::classify(&error), expired);
        assert_eq!(
            error.to_string(),
            format!("event cursor 0 saved in {} is older than the oldest event the daemon keeps; remove the file to start from the newest event", file.display())
        );
    }
    #[test]
    fn expired_event_cursor_never_becomes_a_silent_restart() {
        assert!(Frames::default()
            .feed(b"event: error\ndata: {\"error\":{\"code\":\"event_cursor_expired\"}}\n\n")
            .unwrap_err()
            .to_string()
            .contains("event_cursor_expired"));
    }
}
