//! End-to-end CLI tests for `wx feedback`, the one-time star notice, and exit-1 error hints.
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    path::Path,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct MockState {
    send_response: Arc<Mutex<Value>>,
    status_raw: Arc<Mutex<Option<String>>>,
}

async fn start_mock_server(state: MockState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/v1/status",
            get(|State(state): State<MockState>| async move {
                if let Some(raw) = state.status_raw.lock().unwrap().clone() {
                    return (StatusCode::OK, raw).into_response();
                }
                Json(json!({"serverId": "synthetic", "version": {"backend": "native"}}))
                    .into_response()
            }),
        )
        .route(
            "/v1/accounts",
            get(|| async { Json(json!({"items": [{"id": "synthetic-acc"}]})) }),
        )
        .route(
            "/v1/accounts/synthetic-acc/threads",
            get(|| async { Json(json!({"items": []})) }),
        )
        .route(
            "/v1/accounts/synthetic-acc/sends",
            post(|State(state): State<MockState>| async move {
                Json(state.send_response.lock().unwrap().clone())
            }),
        )
        .route(
            "/v1/accounts/synthetic-acc/sends/by-idempotency-key",
            get(|State(state): State<MockState>| async move {
                Json(state.send_response.lock().unwrap().clone())
            }),
        )
        .with_state(state);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

async fn wx_cmd_async(
    home: &Path,
    xdg_config: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Output {
    let home = home.to_owned();
    let xdg_config = xdg_config.to_owned();
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let extra_env: Vec<(String, String)> = extra_env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let env_refs: Vec<(&str, &str)> = extra_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        wx_cmd(&home, &xdg_config, &arg_refs, &env_refs)
    })
    .await
    .unwrap()
}

fn wx_cmd(home: &Path, xdg_config: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wx"));
    cmd.args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", xdg_config)
        .env("APPDATA", xdg_config)
        .env_remove("AGENT_WECHAT_NO_PROMPTS")
        .env_remove("CI")
        .env_remove("AGENT_WECHAT_CONFIG")
        .env_remove("AGENT_WECHAT_SERVER")
        .env_remove("AGENT_WECHAT_URL")
        .env_remove("AGENT_WECHAT_TOKEN")
        .env_remove("AGENT_WECHAT_TOKEN_FILE")
        .env_remove("AGENT_WECHAT_ACCOUNT");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

#[test]
fn feedback_prints_repository_and_prefilled_issue_url_without_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg = temp.path().join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();

    let json_out = wx_cmd(&home, &xdg, &["--json", "feedback"], &[]);
    assert!(
        json_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let payload: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(
        payload["repository"],
        "https://github.com/thisnick/agent-wechat"
    );
    let issue_url = payload["issueUrl"].as_str().unwrap();
    assert!(issue_url.starts_with(
        "https://github.com/thisnick/agent-wechat/issues/new?template=bug_report.yml&environment="
    ));
    assert!(!issue_url.contains(temp.path().to_str().unwrap()));
    assert!(payload["environment"]["version"].is_string());
    assert!(payload["environment"]["os"].is_string());
    assert!(payload["environment"]["target"].is_string());

    let human_out = wx_cmd(&home, &xdg, &["feedback"], &[]);
    assert!(human_out.status.success());
    let text = String::from_utf8(human_out.stdout).unwrap();
    assert!(text.contains("Repository: https://github.com/thisnick/agent-wechat"));
    assert!(text.contains("Issue URL: https://github.com/thisnick/agent-wechat/issues/new?"));

    // `--open` is ignored in `--json` mode, even without a graphical display.
    let json_open = Command::new(env!("CARGO_BIN_EXE_wx"))
        .args(["--json", "feedback", "--open"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(
        json_open.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json_open.stderr)
    );
    let json_open_val: Value = serde_json::from_slice(&json_open.stdout).unwrap();
    assert_eq!(
        json_open_val["repository"],
        "https://github.com/thisnick/agent-wechat"
    );

    // `wx --token-stdin feedback` exits 2 (not exit 1 operation_failed).
    let token_stdin_err = wx_cmd(&home, &xdg, &["--json", "--token-stdin", "feedback"], &[]);
    assert_eq!(token_stdin_err.status.code(), Some(2));
    let token_stdin_val: Value = serde_json::from_slice(&token_stdin_err.stderr).unwrap();
    assert_eq!(
        token_stdin_val["error"]["code"],
        "invalid_configuration_or_input"
    );
    assert!(token_stdin_val["error"].get("report").is_none());

    // On headless Linux/Unix, `--open` prints the URL and exits 9 without the exit-1 hint.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let headless_open = Command::new(env!("CARGO_BIN_EXE_wx"))
            .args(["feedback", "--open"])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        assert_eq!(headless_open.status.code(), Some(9));
        let out_text = String::from_utf8(headless_open.stdout).unwrap();
        assert!(
            out_text.contains("Issue URL: https://github.com/thisnick/agent-wechat/issues/new?")
        );
        let err_text = String::from_utf8(headless_open.stderr).unwrap();
        assert!(err_text.contains("could not open browser"));
        assert!(!err_text.contains("Run `wx feedback`"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_counting_gating_and_unexpected_error_reporting_work_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg = temp.path().join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();

    let state = MockState {
        send_response: Arc::new(Mutex::new(json!({"id": "send-1", "state": "sent"}))),
        status_raw: Arc::new(Mutex::new(None)),
    };
    let (base_url, server) = start_mock_server(state.clone()).await;
    let base_env = [
        ("AGENT_WECHAT_URL", base_url.as_str()),
        (
            "AGENT_WECHAT_TOKEN",
            "synthetic-test-token-0123456789abcdef",
        ),
    ];

    let prompt_file = xdg.join("agent-wechat/support-prompt.json");

    // 1. Reads do not create or increment support-prompt.json.
    let read_out = wx_cmd_async(&home, &xdg, &["--json", "chats", "list"], &base_env).await;
    assert!(
        read_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&read_out.stderr)
    );
    assert!(!prompt_file.exists());

    // 2. Failed and needs_review sends do not count.
    for bad_state in ["failed", "needs_review"] {
        *state.send_response.lock().unwrap() = json!({"id": "bad", "state": bad_state});
        let out = wx_cmd_async(
            &home,
            &xdg,
            &[
                "--json",
                "messages",
                "send",
                "filehelper",
                "--text",
                "synthetic",
                "--detach",
            ],
            &base_env,
        )
        .await;
        assert!(!out.status.success());
        assert!(!prompt_file.exists());
    }

    // 3. Accepted --detach message send and sticker send both count, using isolated XDG_CONFIG_HOME.
    //    Replaying the same --idempotency-key does NOT double-count.
    *state.send_response.lock().unwrap() = json!({"id": "ok-1", "state": "queued"});
    let msg_out = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--idempotency-key",
            "replay-check-key",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(
        msg_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&msg_out.stderr)
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
    assert_eq!(saved["sends"], 1);
    assert_eq!(saved["shown"], false);

    let replay_out = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--idempotency-key",
            "replay-check-key",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(
        replay_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&replay_out.stderr)
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
    assert_eq!(saved["sends"], 1, "idempotent replay must not double-count");
    assert_eq!(saved["shown"], false);

    let sticker_out = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "stickers",
            "send",
            "sticker-1",
            "--to",
            "filehelper",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(
        sticker_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sticker_out.stderr)
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
    assert_eq!(saved["sends"], 2);
    assert_eq!(saved["shown"], false);

    // 4. Seed 2 prior days and 14 sends; verify suppression via AGENT_WECHAT_NO_PROMPTS=1 and CI=1.
    std::fs::write(
        &prompt_file,
        r#"{"days":["2020-01-01","2020-01-02"],"sends":14,"shown":false}"#,
    )
    .unwrap();

    for (k, v) in [("AGENT_WECHAT_NO_PROMPTS", "1"), ("CI", "1")] {
        let suppressed_env = [base_env[0], base_env[1], (k, v)];
        let out = wx_cmd_async(
            &home,
            &xdg,
            &[
                "--json",
                "messages",
                "send",
                "filehelper",
                "--text",
                "synthetic",
                "--detach",
            ],
            &suppressed_env,
        )
        .await;
        assert!(out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("support_invite"),
            "notice should be suppressed when {k}={v}: {stderr}"
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
        assert_eq!(saved["shown"], false);
    }

    // 5. Piped non-JSON run keeps counting without showing or marking shown.
    let piped = wx_cmd_async(
        &home,
        &xdg,
        &[
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(piped.status.success());
    assert!(!String::from_utf8_lossy(&piped.stderr).contains("Enjoying wx?"));
    let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
    assert_eq!(saved["shown"], false);

    // 6. --json send emits the one-time notice on stderr, leaves stdout JSON intact, and sets shown = true.
    let triggered = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(triggered.status.success());
    let stdout_json: Value = serde_json::from_slice(&triggered.stdout).unwrap();
    assert_eq!(stdout_json["state"], "queued");
    let stderr = String::from_utf8(triggered.stderr).unwrap();
    let notice_line = stderr
        .lines()
        .find(|line| line.contains("support_invite"))
        .expect("expected JSON notice line on stderr");
    let notice: Value = serde_json::from_str(notice_line).unwrap();
    assert_eq!(notice["event"], "notice");
    assert_eq!(notice["code"], "support_invite");
    assert_eq!(notice["audience"], "user");
    assert!(notice["message"]
        .as_str()
        .unwrap()
        .contains("https://github.com/thisnick/agent-wechat"));
    let saved: Value = serde_json::from_slice(&std::fs::read(&prompt_file).unwrap()).unwrap();
    assert_eq!(saved["shown"], true);

    // 7. Next --json send does NOT show the notice again.
    let second = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(second.status.success());
    assert!(!String::from_utf8_lossy(&second.stderr).contains("support_invite"));

    // 7b. Explicit --config writes support-prompt.json next to that config file instead of XDG.
    let custom_dir = temp.path().join("custom-cfg");
    std::fs::create_dir_all(&custom_dir).unwrap();
    let custom_cfg = custom_dir.join("custom.toml");
    let custom_prompt = custom_dir.join("support-prompt.json");
    std::fs::write(&custom_cfg, "schemaVersion = 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&custom_cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let custom_send = wx_cmd_async(
        &home,
        &xdg,
        &[
            "--json",
            "--config",
            custom_cfg.to_str().unwrap(),
            "messages",
            "send",
            "filehelper",
            "--text",
            "synthetic",
            "--detach",
        ],
        &base_env,
    )
    .await;
    assert!(
        custom_send.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&custom_send.stderr)
    );
    let custom_saved: Value =
        serde_json::from_slice(&std::fs::read(&custom_prompt).unwrap()).unwrap();
    assert_eq!(custom_saved["sends"], 1);
    assert_eq!(custom_saved["shown"], false);

    // 8. Unexpected exit-1 error (`operation_failed`) includes report field in JSON and hint in human mode.
    *state.status_raw.lock().unwrap() = Some("not-valid-json".into());
    let err_json = wx_cmd_async(&home, &xdg, &["--json", "status"], &base_env).await;
    assert_eq!(err_json.status.code(), Some(1));
    let err_val: Value = serde_json::from_slice(&err_json.stderr).unwrap();
    assert_eq!(err_val["error"]["code"], "operation_failed");
    assert_eq!(err_val["error"]["report"], "wx feedback --json");

    let err_human = wx_cmd_async(&home, &xdg, &["status"], &base_env).await;
    assert_eq!(err_human.status.code(), Some(1));
    let err_text = String::from_utf8(err_human.stderr).unwrap();
    assert!(err_text.contains("wx feedback"));

    server.abort();
}
