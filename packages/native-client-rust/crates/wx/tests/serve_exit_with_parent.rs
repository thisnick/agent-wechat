//! `wx serve --exit-with-parent` stops once the process that started it exits.
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn running(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

/// Starts `wx serve` from a shell that stays open until its stdin closes, and
/// returns the shell with the daemon's PID once the daemon is listening.
fn serve_under_shell(root: &Path, port: u16, flags: &[&str]) -> (std::process::Child, i32) {
    let data = root.join(format!("hub-{port}"));
    let token = root.join(format!("token-{port}"));
    std::fs::write(&token, "a".repeat(64)).unwrap();
    // A simple background command is forked by this shell, so the shell is the
    // daemon's parent. `read` keeps the shell alive until the test closes stdin.
    let script = r#"wx="$1"; shift; "$wx" "$@" <"$TOKEN_FILE" >/dev/null 2>&1 & echo $!; read _"#;
    let mut shell = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_wx"))
        .arg("--data-dir")
        .arg(&data)
        .arg("serve")
        .args(flags)
        .args(["--listen", &format!("127.0.0.1:{port}"), "--token-stdin"])
        .env("TOKEN_FILE", &token)
        .env_remove("AGENT_WECHAT_TOKEN")
        .env_remove("AGENT_WECHAT_TOKEN_FILE")
        .env_remove("AGENT_WECHAT_SERVER_CONFIG")
        .env_remove("AGENT_WECHAT_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid: i32 = line.trim().parse().unwrap();
    assert!(
        wait_until(Duration::from_secs(60), || TcpStream::connect((
            Ipv4Addr::LOCALHOST,
            port
        ))
        .is_ok()),
        "the daemon did not start listening"
    );
    (shell, pid)
}

#[test]
fn serve_stops_when_its_parent_exits_only_with_the_flag() {
    let root = wechat_platform::private_tempdir("wx-parent-").unwrap();

    let port = free_port();
    let (mut shell, pid) = serve_under_shell(root.path(), port, &["--exit-with-parent"]);
    drop(shell.stdin.take());
    shell.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(30), || !running(pid)),
        "the daemon kept running after its parent exited"
    );
    assert!(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
        "the daemon's port is still in use"
    );

    // Without the flag the daemon keeps running on its own, as before.
    let port = free_port();
    let (mut shell, pid) = serve_under_shell(root.path(), port, &[]);
    drop(shell.stdin.take());
    shell.wait().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let kept = running(pid);
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert!(wait_until(Duration::from_secs(30), || !running(pid)));
    assert!(kept, "a daemon without the flag stopped with its parent");
}
