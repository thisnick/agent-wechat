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
pub async fn watch(
    r: &Remote,
    mut after: String,
    cursor_file: Option<PathBuf>,
    messages_only: bool,
) -> Result<()> {
    if after.parse::<u64>().is_err() || !after.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid event cursor");
    }
    let status = r.get("v1/status").await?;
    let server = status["serverId"]
        .as_str()
        .context("daemon identity missing")?;
    let account = r.account()?;
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
            if after != "0" && after != cursor {
                bail!("explicit cursor conflicts with emission checkpoint");
            }
            cursor.parse::<u64>().context("invalid event cursor file")?;
            after = cursor.into();
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("cursor file requires a private owned directory");
        }
    }
    let mut backoff = 250;
    loop {
        let path = query(r.path("events/stream")?, &[("after", Some(after.clone()))]);
        let response = tokio::select! {_=tokio::signal::ctrl_c()=>{super::set_exit(130);return Ok(())},response=r.response(reqwest::Method::GET,&path,None,None)=>response};
        let response = match response {
            Ok(value) => value,
            Err(error) => {
                let message = error.to_string();
                if message.contains("event_cursor_expired")
                    || message.contains("401")
                    || message.contains("403")
                {
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
            for mut event in parser.feed(&chunk)? {
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
                    let mut stdout = std::io::stdout().lock();
                    if let Err(error) = writeln!(stdout, "{}", serde_json::to_string(&event)?)
                        .and_then(|_| stdout.flush())
                    {
                        if error.kind() == std::io::ErrorKind::BrokenPipe {
                            return Ok(());
                        }
                        return Err(error.into());
                    }
                }
                after = cursor;
                backoff = 250;
                if let Some(path) = &cursor_file {
                    client_actions::write_replace(
                        path,
                        &json!({"schemaVersion":1,"serverId":server,"accountId":account,"cursor":after,"progress":"emission"}),
                    )?;
                }
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
    #[test]
    fn expired_event_cursor_never_becomes_a_silent_restart() {
        assert!(Frames::default()
            .feed(b"event: error\ndata: {\"error\":{\"code\":\"event_cursor_expired\"}}\n\n")
            .unwrap_err()
            .to_string()
            .contains("event_cursor_expired"));
    }
}
