use super::*;
pub async fn requests(
    r: &Remote,
    config: &settings::File,
    command: ContactRequestCommand,
) -> Result<Value> {
    match command {
        ContactRequestCommand::List {
            direction,
            actionable,
            search,
            limit,
            cursor,
        } => {
            r.get(&query(
                r.path("contact-requests")?,
                &[
                    ("direction", direction),
                    ("actionable", actionable.then(|| "true".into())),
                    ("search", search),
                    ("limit", limit.map(|n| n.to_string())),
                    ("cursor", cursor),
                ],
            ))
            .await
        }
        ContactRequestCommand::Get { id } => {
            r.get(&r.path(&format!("contact-requests/{}", encode(&id)))?)
                .await
        }
        ContactRequestCommand::Send {
            recipient_ref,
            message,
            privacy,
            options,
        } => {
            let mut body = json!({"recipientRef":recipient_ref,"message":message});
            if let Some(value) = privacy_value(privacy) {
                body["privacy"] = value;
            }
            client_actions::mutation(
                r,
                config,
                reqwest::Method::POST,
                "contact-requests",
                body,
                options,
            )
            .await
        }
        ContactRequestCommand::Accept {
            id,
            expected_version,
            privacy,
            options,
        } => {
            let mut body = json!({"expectedVersion":expected_version});
            if let Some(value) = privacy_value(privacy) {
                body["privacy"] = value;
            }
            client_actions::mutation(
                r,
                config,
                reqwest::Method::POST,
                &format!("contact-requests/{}/accept", encode(&id)),
                body,
                options,
            )
            .await
        }
        ContactRequestCommand::Delete {
            id,
            expected_version,
            options,
        } => {
            client_actions::mutation(
                r,
                config,
                reqwest::Method::DELETE,
                &format!("contact-requests/{}", encode(&id)),
                json!({"expectedVersion":expected_version}),
                options,
            )
            .await
        }
    }
}

fn partial_privacy(options: ContactPrivacyOptions) -> Option<Value> {
    let mut privacy = serde_json::Map::new();
    for (name, value) in [
        ("chatsOnly", options.chats_only),
        ("hideMyPosts", options.hide_my_posts),
        ("hideTheirPosts", options.hide_their_posts),
    ] {
        if let Some(value) = value {
            privacy.insert(name.into(), json!(value));
        }
    }
    (!privacy.is_empty()).then_some(Value::Object(privacy))
}
fn privacy_value(options: ContactPrivacyOptions) -> Option<Value> {
    partial_privacy(options).map(|_| {
        json!({
            "chatsOnly":options.chats_only.unwrap_or(false),
            "hideMyPosts":options.hide_my_posts.unwrap_or(false),
            "hideTheirPosts":options.hide_their_posts.unwrap_or(false),
        })
    })
}

fn checked_privacy(value: &Value) -> Result<Value> {
    let mut privacy = serde_json::Map::new();
    for name in ["chatsOnly", "hideMyPosts", "hideTheirPosts"] {
        privacy.insert(
            name.into(),
            json!(value[name]
                .as_bool()
                .context("incomplete contact privacy snapshot")?),
        );
    }
    Ok(Value::Object(privacy))
}
/// The remark part of the caller's input: a trimmed string, or `null` to clear.
fn remark_input(options: ContactRemarkOptions) -> Option<Value> {
    if options.clear_remark {
        return Some(json!({"remark":null}));
    }
    options.remark.map(|text| {
        let text = text.trim();
        json!({"remark":if text.is_empty() { Value::Null } else { json!(text) }})
    })
}
fn checked_remark(value: &Value) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::String(text)
            if text.len() <= wechat_core::MAX_CONTACT_REMARK_BYTES
                && !text.chars().any(char::is_control)
                && text.trim() == text =>
        {
            Ok(json!(text))
        }
        _ => bail!("invalid contact remark"),
    }
}
fn checked_update_body(value: &Value) -> Result<Value> {
    let version = value["expectedVersion"]
        .as_str()
        .filter(|value| !value.is_empty())
        .context("missing contact version")?;
    if version.len() > 4096 || version.chars().any(char::is_control) {
        bail!("invalid contact version");
    }
    let body = if value.get("remark").is_some() {
        json!({"remark":checked_remark(&value["remark"])?,"expectedVersion":version})
    } else {
        json!({"privacy":checked_privacy(&value["privacy"])?,"expectedVersion":version})
    };
    if &body != value {
        bail!("invalid saved contact update");
    }
    Ok(body)
}
async fn intent_lock(
    config: &settings::File,
    r: &Remote,
    key: &str,
) -> Result<(PathBuf, std::fs::File)> {
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        bail!("invalid idempotency key");
    }
    let directory = config
        .path
        .parent()
        .context("client config location missing")?
        .join("receipts");
    std::fs::create_dir_all(&directory)?;
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || !wechat_platform::owned_path(&directory)? {
        bail!("receipt directory must be owned by this user");
    }
    wechat_platform::protect_path(&directory, true)?;
    use sha2::{Digest, Sha256};
    let identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}\0{key}", r.base, r.account()?).as_bytes())
    );
    let path = directory.join(format!("contact-update-{identity}.json"));
    let lock_path = directory.join(format!("contact-update-{identity}.lock"));
    let guard = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
        let file = wechat_platform::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        if !file.metadata()?.is_file() || !wechat_platform::private_path(&lock_path)? {
            bail!("contact update lock must be a private regular file");
        }
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await??;
    Ok((path, guard))
}

/// Freeze a partial edit against one snapshot before submitting its keyed mutation.
pub async fn set_contact(
    r: &Remote,
    config: &settings::File,
    id: String,
    privacy: ContactPrivacyOptions,
    remark: ContactRemarkOptions,
    mut options: MutationOptions,
) -> Result<Value> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        bail!("invalid contact identity");
    }
    // Privacy and the remark are separate WeChat requests, so one command
    // changes one of them.
    let caller_input = match (partial_privacy(privacy), remark_input(remark)) {
        (Some(privacy), None) => privacy,
        (None, Some(remark)) => remark,
        (Some(_), Some(_)) => {
            return Err(outcome::failure(
                2,
                "one_contact_change_per_request",
                "change privacy and the remark in separate commands",
            ))
        }
        (None, None) => bail!("supply a privacy setting or a remark"),
    };
    if options.wait > 900 {
        bail!("wait must be at most 900 seconds");
    }
    let key = options
        .idempotency_key
        .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
        .clone();
    let request_path = format!("contacts/{}", encode(&id));
    let (path, _guard) = intent_lock(config, r, &key).await?;
    let expected = json!({"schemaVersion":1,"kind":"contact_update_intent","endpoint":r.base.as_str(),"accountId":r.account()?,"idempotencyKey":key,"contactId":id,"requestPath":request_path,"callerInput":caller_input});
    let mut intent = if path.exists() {
        let existing: Value = serde_json::from_slice(&private_input(&path, 65536)?)?;
        for field in [
            "schemaVersion",
            "kind",
            "endpoint",
            "accountId",
            "idempotencyKey",
            "contactId",
            "requestPath",
            "callerInput",
        ] {
            if existing[field] != expected[field] {
                bail!("idempotency_conflict: contact update input changed");
            }
        }
        existing
    } else {
        private_output(&path, &serde_json::to_vec(&expected)?)?;
        expected
    };
    eprintln!("Contact update intent: {}", path.display());
    let body = if let Some(body) = intent.get("resolvedBody") {
        checked_update_body(body)?
    } else {
        let contact = r.get(&r.path(&request_path)?).await?;
        if contact["relation"] != "friend" {
            return Err(outcome::failure(
                9,
                "ordinary_contact_required",
                "only a friend's privacy and remark can be changed",
            ));
        }
        if contact["privacyPending"].as_bool().unwrap_or(false) {
            return Err(outcome::failure(
                9,
                "contact_privacy_pending",
                "an earlier privacy change is still settling; try again shortly",
            ));
        }
        let Some(version) = contact["privacyVersion"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            // The contact says why it can't be edited; keep that code.
            const KNOWN: [&str; 3] = [
                "ordinary_contact_required",
                "profile_identity_unavailable",
                "contact_flags_unresolved",
            ];
            let code = contact["privacyUnavailable"]
                .as_str()
                .filter(|code| KNOWN.contains(code))
                .unwrap_or("contact_privacy_unresolved");
            let message = if code == "ordinary_contact_required" {
                "this contact's privacy and remark can't be changed here".to_owned()
            } else {
                format!("this contact can't be edited yet ({code}); run `wx contacts refresh {id}` and try again")
            };
            return Err(outcome::failure(9, code, &message));
        };
        let body = if caller_input.get("remark").is_some() {
            checked_update_body(
                &json!({"remark":caller_input["remark"],"expectedVersion":version}),
            )?
        } else {
            let mut merged = checked_privacy(&contact["privacy"])?;
            for (name, value) in caller_input
                .as_object()
                .context("invalid contact update input")?
            {
                merged[name] = value.clone();
            }
            checked_update_body(&json!({"privacy":merged,"expectedVersion":version}))?
        };
        intent["resolvedBody"] = body.clone();
        client_actions::write_replace(&path, &intent)?;
        body
    };
    client_actions::mutation(
        r,
        config,
        reqwest::Method::PATCH,
        &request_path,
        body,
        options,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::mpsc,
    };

    fn parse(args: &[&str]) -> std::result::Result<ContactCommand, clap::Error> {
        let cli = Cli::try_parse_from(["wx", "contacts"].into_iter().chain(args.iter().copied()))?;
        let Command::Contacts {
            command: Some(command),
            ..
        } = cli.command
        else {
            panic!("contact command required")
        };
        Ok(command)
    }
    #[test]
    fn contact_list_requires_explicit_all_for_cached_people() {
        assert!(matches!(
            parse(&["list"]).unwrap(),
            ContactCommand::List { all: false, .. }
        ));
        assert!(matches!(
            parse(&["list", "--all", "--limit", "2", "--cursor", "next"]).unwrap(),
            ContactCommand::List {
                all: true,
                limit: Some(2),
                cursor: Some(_),
                ..
            }
        ));
        assert!(Cli::try_parse_from(["wx", "groups", "list", "--all"]).is_err());
    }
    #[test]
    fn parser_accepts_partial_edits_but_requires_a_valued_boolean_and_nonempty_set() {
        assert!(parse(&["privacy", "get", "peer"]).is_err());
        assert!(parse(&["set", "peer", "--chats-only", "false"]).is_ok());
        assert!(parse(&["set", "peer", "--remark", "Monster"]).is_ok());
        assert!(parse(&["set", "peer", "--clear-remark"]).is_ok());
        // Privacy and the remark are changed by separate commands.
        assert!(parse(&["set", "peer", "--remark", "Monster", "--chats-only", "true"]).is_err());
        assert!(parse(&["set", "peer", "--clear-remark", "--hide-my-posts", "true"]).is_err());
        assert!(parse(&["set", "peer", "--remark", "Monster", "--clear-remark"]).is_err());
        assert!(parse(&[
            "set",
            "peer",
            "--hide-my-posts",
            "true",
            "--hide-their-posts",
            "false"
        ])
        .is_ok());
        assert!(parse(&[
            "requests",
            "send",
            "reference",
            "--message",
            "hello",
            "--hide-my-posts",
            "true"
        ])
        .is_ok());
        assert!(parse(&[
            "requests",
            "accept",
            "request",
            "--expected-version",
            "v1",
            "--chats-only",
            "false"
        ])
        .is_ok());
        for args in [
            vec!["set", "peer"],
            vec!["privacy", "set", "peer"],
            vec!["set", "peer", "--chats-only"],
            vec!["set", "peer", "--chats-only", "yes"],
            vec![
                "set",
                "peer",
                "--chats-only",
                "true",
                "--expected-version",
                "v1",
            ],
        ] {
            assert!(parse(&args).is_err());
        }
        assert_eq!(privacy_value(ContactPrivacyOptions::default()), None);
        assert_eq!(
            privacy_value(ContactPrivacyOptions {
                hide_my_posts: Some(true),
                ..Default::default()
            }),
            Some(json!({"chatsOnly":false,"hideMyPosts":true,"hideTheirPosts":false}))
        );
    }
    #[derive(Debug)]
    struct Captured {
        method: String,
        path: String,
        key: Option<String>,
        body: Option<Value>,
        saved_intent: Option<Value>,
    }
    struct Mock {
        remote: Remote,
        captured: mpsc::UnboundedReceiver<Captured>,
        gets: Arc<AtomicUsize>,
        patches: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }
    fn operation() -> Value {
        json!({"id":"op-privacy","accountId":"test-account","kind":"contact_mutation","state":"running"})
    }
    async fn mock_daemon(directory: PathBuf, drop_first: bool, stale: bool) -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, captured) = mpsc::unbounded_channel();
        let gets = Arc::new(AtomicUsize::new(0));
        let patches = Arc::new(AtomicUsize::new(0));
        let server_gets = gets.clone();
        let server_patches = patches.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0; 2048];
                let header_end = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break None;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    assert!(raw.len() < 16384);
                    if let Some(index) = raw.windows(4).position(|value| value == b"\r\n\r\n") {
                        break Some(index + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let header = std::str::from_utf8(&raw[..header_end]).unwrap();
                let mut lines = header.lines();
                let request_line = lines.next().unwrap();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap().to_owned();
                let path = parts.next().unwrap().to_owned();
                let headers: std::collections::HashMap<_, _> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect();
                assert_eq!(
                    headers.get("authorization").map(String::as_str),
                    Some("Bearer synthetic-bearer")
                );
                let length = headers
                    .get("content-length")
                    .map(|value| value.parse::<usize>().unwrap())
                    .unwrap_or(0);
                assert!(length <= 8192);
                while raw.len() < header_end + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0);
                    raw.extend_from_slice(&chunk[..n]);
                }
                let body: Option<Value> = if length == 0 {
                    None
                } else {
                    Some(serde_json::from_slice(&raw[header_end..header_end + length]).unwrap())
                };
                let key = headers.get("idempotency-key").cloned();
                let is_patch = method == "PATCH";
                let saved_intent = if is_patch {
                    std::fs::read_dir(directory.join("receipts"))
                        .unwrap()
                        .filter_map(|entry| {
                            let path = entry.unwrap().path();
                            if !path
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with("contact-update-")
                                || path.extension().and_then(|value| value.to_str()) != Some("json")
                            {
                                return None;
                            }
                            let value: Value =
                                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                            (value["idempotencyKey"].as_str() == key.as_deref()).then_some(value)
                        })
                        .next()
                } else {
                    None
                };
                sender
                    .send(Captured {
                        method: method.clone(),
                        path: path.clone(),
                        key,
                        body,
                        saved_intent,
                    })
                    .unwrap();
                let (status, value) = if method == "GET" && path.contains("/contacts/") {
                    server_gets.fetch_add(1, Ordering::SeqCst);
                    if server_patches.load(Ordering::SeqCst) == 0 {
                        (
                            "200 OK",
                            json!({"id":"peer value","relation":"friend","privacy":{"chatsOnly":false,"hideMyPosts":true,"hideTheirPosts":false},"privacyVersion":"v1"}),
                        )
                    } else {
                        (
                            "200 OK",
                            json!({"id":"peer value","relation":"friend","privacy":{"chatsOnly":false,"hideMyPosts":false,"hideTheirPosts":true},"privacyVersion":"v2"}),
                        )
                    }
                } else if is_patch {
                    let prior = server_patches.fetch_add(1, Ordering::SeqCst);
                    if drop_first && prior == 0 {
                        stream.shutdown().await.unwrap();
                        continue;
                    }
                    if stale {
                        (
                            "409 Conflict",
                            json!({"error":{"code":"stale_contact_privacy"}}),
                        )
                    } else {
                        ("200 OK", operation())
                    }
                } else {
                    ("200 OK", operation())
                };
                let bytes = serde_json::to_vec(&value).unwrap();
                let response = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",bytes.len());
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&bytes).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        Mock {
            remote: Remote {
                http: reqwest::Client::new(),
                base: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
                token: "synthetic-bearer".into(),
                account: Some("test-account".into()),
            },
            captured,
            gets,
            patches,
            server,
        }
    }
    fn config(directory: &Path) -> settings::File {
        settings::File {
            path: directory.join("client.toml"),
            settings: Default::default(),
        }
    }
    async fn dispatch(remote: &Remote, config: &settings::File, args: &[&str]) -> Result<Value> {
        match parse(args).unwrap() {
            ContactCommand::Requests { command } => requests(remote, config, command).await,
            ContactCommand::Set {
                id,
                privacy,
                remark,
                options,
            } => set_contact(remote, config, id, privacy, remark, options).await,
            _ => panic!("privacy or request command required"),
        }
    }
    fn edit_args() -> [&'static str; 8] {
        [
            "set",
            "peer value",
            "--chats-only",
            "true",
            "--idempotency-key",
            "edit-key",
            "--detach",
            "--json",
        ]
    }
    fn expected_body() -> Value {
        json!({"privacy":{"chatsOnly":true,"hideMyPosts":true,"hideTheirPosts":false},"expectedVersion":"v1"})
    }
    #[tokio::test]
    async fn set_merges_only_supplied_flags_and_persists_caller_and_resolved_body_before_patch() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mut mock = mock_daemon(temp.path().into(), false, false).await;
        let result = dispatch(&mock.remote, &config, &edit_args()).await.unwrap();
        let get = mock.captured.recv().await.unwrap();
        assert_eq!(get.method, "GET");
        assert_eq!(get.path, "/v1/accounts/test-account/contacts/peer%20value");
        assert!(get.key.is_none());
        assert!(get.body.is_none());
        let patch = mock.captured.recv().await.unwrap();
        assert_eq!(patch.method, "PATCH");
        assert_eq!(
            patch.path,
            "/v1/accounts/test-account/contacts/peer%20value"
        );
        assert_eq!(patch.key.as_deref(), Some("edit-key"));
        assert_eq!(patch.body, Some(expected_body()));
        let intent = patch.saved_intent.unwrap();
        assert_eq!(intent["callerInput"], json!({"chatsOnly":true}));
        assert_eq!(intent["resolvedBody"], expected_body());
        assert_eq!(intent["accountId"], "test-account");
        assert_eq!(intent["endpoint"], mock.remote.base.as_str());
        assert!(PathBuf::from(result["receiptPath"].as_str().unwrap()).is_file());
        mock.server.abort();
    }
    #[tokio::test]
    async fn set_remark_sends_the_trimmed_remark_with_the_contact_version() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mut mock = mock_daemon(temp.path().into(), false, false).await;
        let args = [
            "set",
            "peer value",
            "--remark",
            "  Monster  ",
            "--idempotency-key",
            "remark-key",
            "--detach",
        ];
        dispatch(&mock.remote, &config, &args).await.unwrap();
        let get = mock.captured.recv().await.unwrap();
        assert_eq!(get.method, "GET");
        let patch = mock.captured.recv().await.unwrap();
        assert_eq!(patch.method, "PATCH");
        assert_eq!(patch.key.as_deref(), Some("remark-key"));
        assert_eq!(
            patch.body,
            Some(json!({"remark":"Monster","expectedVersion":"v1"}))
        );
        let intent = patch.saved_intent.unwrap();
        assert_eq!(intent["callerInput"], json!({"remark":"Monster"}));
        mock.server.abort();
    }
    #[test]
    fn saved_update_bodies_accept_privacy_or_a_remark_but_not_both() {
        let privacy = json!({"chatsOnly":false,"hideMyPosts":true,"hideTheirPosts":false});
        assert!(checked_update_body(&json!({"privacy":privacy,"expectedVersion":"v"})).is_ok());
        assert!(checked_update_body(&json!({"remark":"Name","expectedVersion":"v"})).is_ok());
        assert!(checked_update_body(&json!({"remark":null,"expectedVersion":"v"})).is_ok());
        for bad in [
            json!({"remark":"Name","privacy":privacy,"expectedVersion":"v"}),
            json!({"remark":" padded","expectedVersion":"v"}),
            json!({"remark":42,"expectedVersion":"v"}),
            json!({"remark":"Name"}),
        ] {
            assert!(checked_update_body(&bad).is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn lost_response_replay_keeps_original_merge_and_version_and_conflicts_on_changed_input()
    {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mut mock = mock_daemon(temp.path().into(), true, false).await;
        assert!(dispatch(&mock.remote, &config, &edit_args()).await.is_err());
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.patches.load(Ordering::SeqCst), 1);
        let get = mock.captured.recv().await.unwrap();
        assert_eq!(get.method, "GET");
        let first = mock.captured.recv().await.unwrap();
        assert_eq!(first.body, Some(expected_body()));
        assert_eq!(first.saved_intent.unwrap()["resolvedBody"], expected_body());
        dispatch(&mock.remote, &config, &edit_args()).await.unwrap();
        let replay = mock.captured.recv().await.unwrap();
        assert_eq!(replay.method, "PATCH");
        assert_eq!(replay.body, first.body);
        assert_eq!(replay.key, first.key);
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.patches.load(Ordering::SeqCst), 2);
        dispatch(&mock.remote, &config, &edit_args()).await.unwrap();
        let operation = mock.captured.recv().await.unwrap();
        assert_eq!(operation.method, "GET");
        assert_eq!(
            operation.path,
            "/v1/accounts/test-account/operations/op-privacy"
        );
        let changed = [
            "set",
            "peer value",
            "--chats-only",
            "false",
            "--idempotency-key",
            "edit-key",
            "--detach",
        ];
        assert!(dispatch(&mock.remote, &config, &changed)
            .await
            .unwrap_err()
            .to_string()
            .contains("idempotency_conflict"));
        let changed_presence = [
            "set",
            "peer value",
            "--chats-only",
            "true",
            "--hide-my-posts",
            "true",
            "--idempotency-key",
            "edit-key",
            "--detach",
        ];
        assert!(dispatch(&mock.remote, &config, &changed_presence)
            .await
            .unwrap_err()
            .to_string()
            .contains("idempotency_conflict"));
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.patches.load(Ordering::SeqCst), 2);
        assert!(mock.captured.try_recv().is_err());
        mock.server.abort();
    }
    #[tokio::test]
    async fn stale_version_does_not_refresh_or_retry_and_generated_key_is_recorded_before_submission(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mut mock = mock_daemon(temp.path().into(), false, true).await;
        let args = [
            "set",
            "peer value",
            "--hide-their-posts",
            "true",
            "--detach",
        ];
        assert!(dispatch(&mock.remote, &config, &args).await.is_err());
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.patches.load(Ordering::SeqCst), 1);
        mock.captured.recv().await.unwrap();
        let patch = mock.captured.recv().await.unwrap();
        let intent = patch.saved_intent.unwrap();
        assert!(!patch.key.as_ref().unwrap().is_empty());
        assert_eq!(intent["idempotencyKey"].as_str(), patch.key.as_deref());
        assert_eq!(intent["callerInput"], json!({"hideTheirPosts":true}));
        assert_eq!(intent["resolvedBody"], patch.body.unwrap());
        assert!(mock.captured.try_recv().is_err());
        mock.server.abort();
    }
    #[tokio::test]
    async fn concurrent_same_key_uses_one_snapshot_and_one_submission() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mock = mock_daemon(temp.path().into(), false, false).await;
        let args = edit_args();
        let (a, b) = tokio::join!(
            dispatch(&mock.remote, &config, &args),
            dispatch(&mock.remote, &config, &args)
        );
        assert!(a.is_ok());
        assert!(b.is_ok());
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.patches.load(Ordering::SeqCst), 1);
        mock.server.abort();
    }
    #[tokio::test]
    async fn send_and_accept_partial_privacy_use_fresh_defaults_and_omission_preserves_legacy_bodies(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let mut mock = mock_daemon(temp.path().into(), false, false).await;
        for (action, prefix, path, body) in [
            (
                "send",
                vec!["requests", "send", "reference", "--message", "hello"],
                "/v1/accounts/test-account/contact-requests",
                json!({"recipientRef":"reference","message":"hello"}),
            ),
            (
                "accept",
                vec![
                    "requests",
                    "accept",
                    "request value",
                    "--expected-version",
                    "v1",
                ],
                "/v1/accounts/test-account/contact-requests/request%20value/accept",
                json!({"expectedVersion":"v1"}),
            ),
        ] {
            for include in [false, true] {
                let key = format!("{action}-{include}");
                let mut args = prefix.clone();
                args.extend(["--idempotency-key", key.as_str(), "--detach"]);
                if include {
                    args.extend(["--hide-my-posts", "true"]);
                }
                dispatch(&mock.remote, &config, &args).await.unwrap();
                let request = mock.captured.recv().await.unwrap();
                assert_eq!(request.method, "POST");
                assert_eq!(request.path, path);
                assert_eq!(request.key.as_deref(), Some(key.as_str()));
                let mut expected = body.clone();
                if include {
                    expected["privacy"] =
                        json!({"chatsOnly":false,"hideMyPosts":true,"hideTheirPosts":false});
                }
                assert_eq!(request.body, Some(expected));
            }
        }
        mock.server.abort();
    }
}
