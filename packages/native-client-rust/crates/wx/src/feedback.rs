//! `wx feedback` bug-report helper and one-time GitHub star notice.
use super::*;
use std::{
    ffi::OsStr,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

pub const GITHUB_REPO_URL: &str = "https://github.com/thisnick/agent-wechat";
pub const GITHUB_ISSUE_TEMPLATE: &str = "bug_report.yml";

pub const DAYS_BEFORE_ASKING: usize = 3;
pub const SENDS_BEFORE_ASKING: u64 = 15;
pub const SUPPORT_PROMPT_FILENAME: &str = "support-prompt.json";

const MAX_SUPPORT_PROMPT_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackEnvironment {
    pub version: String,
    pub install_method: Option<String>,
    pub os: String,
    pub target: String,
}

/// Optional sanitized diagnostic summaries for future integration with
/// `wx status --json` (#749) and `wx daemon doctor --json` (#752).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticSummaries {
    pub status_summary: Option<String>,
    pub doctor_summary: Option<String>,
}

impl DiagnosticSummaries {
    fn format_logs(&self) -> Option<String> {
        let mut sections = Vec::new();
        if let Some(status) = self
            .status_summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sections.push(format!("wx status --json (sanitized):\n{status}"));
        }
        if let Some(doctor) = self
            .doctor_summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sections.push(format!("wx daemon doctor --json (sanitized):\n{doctor}"));
        }
        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackOutput {
    pub repository: &'static str,
    pub issue_url: String,
    pub environment: FeedbackEnvironment,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportPromptState {
    #[serde(default)]
    pub days: Vec<String>,
    #[serde(default)]
    pub sends: u64,
    #[serde(default)]
    pub shown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeMode {
    None,
    Terminal { dim: bool },
    Json,
}

#[derive(Debug, serde::Serialize)]
struct JsonNotice {
    event: &'static str,
    code: &'static str,
    audience: &'static str,
    message: &'static str,
}

pub fn os_label(os: &str) -> &str {
    match os {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

pub fn platform_line(os: &str, target: &str) -> String {
    let os = os.trim();
    let target = target.trim();
    match (os.is_empty(), target.is_empty()) {
        (false, false) => format!("{os} ({target})"),
        (false, true) => os.to_owned(),
        (true, false) => target.to_owned(),
        (true, true) => String::new(),
    }
}

/// Detects the installation method from non-sensitive runtime signals.
/// Returns `None` when the method cannot be determined reliably.
pub fn detect_install_method(
    exe_path: Option<&Path>,
    in_official_docker_image: bool,
    in_container: bool,
    has_npm_manifest_ancestor: bool,
    cargo_env: bool,
    debug_build: bool,
) -> Option<&'static str> {
    if in_official_docker_image {
        return Some("Docker");
    }
    if let Some(path) = exe_path {
        if is_desktop_app_path(path) {
            return Some("desktop app");
        }
        if has_npm_manifest_ancestor || is_npm_package_path(path) {
            return Some("npm");
        }
        if is_source_build_path(path) {
            return Some("source");
        }
        if is_release_binary_name(path) {
            return Some("release binary");
        }
    }
    if cargo_env || debug_build {
        return Some("source");
    }
    if in_container {
        return Some("Docker");
    }
    None
}

fn is_desktop_app_path(path: &Path) -> bool {
    let parts = lower_components(path);
    if parts
        .iter()
        .any(|part| part.ends_with(".app") && part.contains("agentwechat"))
    {
        return true;
    }
    parts.windows(2).any(|window| {
        matches!(window[0].as_str(), "agentwechat" | "agent-wechat-desktop")
            && matches!(
                window[1].as_str(),
                "bin" | "resources" | "binaries" | "contents" | "wx" | "wx.exe"
            )
    })
}

fn is_npm_package_path(path: &Path) -> bool {
    let parts = lower_components(path);
    parts.windows(3).any(|window| {
        window[0] == "node_modules" && window[1] == "@agent-wechat" && window[2] == "cli"
    })
}

fn has_npm_release_manifest(exe_path: &Path) -> bool {
    // packages/cli/scripts/native-runtime.mjs installs to:
    // <pkgRoot>/native/<version>/<targetKey>/wx(.exe)
    let Some(target_dir) = exe_path.parent() else {
        return false;
    };
    let Some(version_dir) = target_dir.parent() else {
        return false;
    };
    let Some(native_dir) = version_dir.parent() else {
        return false;
    };
    if native_dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_none_or(|name| !name.eq_ignore_ascii_case("native"))
    {
        return false;
    }
    let Some(pkg_root) = native_dir.parent() else {
        return false;
    };
    pkg_root.join("native-release.json").is_file()
}

fn is_source_build_path(path: &Path) -> bool {
    let parts = lower_components(path);
    if parts
        .windows(2)
        .any(|window| window[0] == ".cargo" && window[1] == "bin")
    {
        return true;
    }
    for (index, part) in parts.iter().enumerate() {
        if part == "target" {
            let next = parts.get(index + 1).map(String::as_str);
            let after = parts.get(index + 2).map(String::as_str);
            if matches!(next, Some("debug" | "release" | "test"))
                || matches!(after, Some("debug" | "release" | "deps"))
            {
                return true;
            }
        }
    }
    false
}

fn is_release_binary_name(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("wx-v"))
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn detect_official_docker_image() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/usr/share/doc/agent-wechat-native/THIRD-PARTY-NOTICES.txt").is_file()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn detect_in_container() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub fn collect_environment() -> FeedbackEnvironment {
    let version_info = serde_json::to_value(wechat_api::version()).unwrap_or(Value::Null);
    let version = version_info["version"]
        .as_str()
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_owned();
    let target = version_info["target"].as_str().unwrap_or("").to_owned();
    let os = os_label(std::env::consts::OS).to_owned();

    let exe_path = std::env::current_exe()
        .ok()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    let has_npm_manifest = exe_path.as_deref().is_some_and(has_npm_release_manifest);
    let install_method = detect_install_method(
        exe_path.as_deref(),
        detect_official_docker_image(),
        detect_in_container(),
        has_npm_manifest,
        std::env::var_os("CARGO").is_some_and(|value| !value.is_empty()),
        cfg!(debug_assertions),
    )
    .map(str::to_owned);

    FeedbackEnvironment {
        version,
        install_method,
        os,
        target,
    }
}

/// Builds the GitHub Bug Report environment textarea content aligned with
/// `packages/client-ui/src/support-prompt.ts` and `.github/ISSUE_TEMPLATE/bug_report.yml`.
pub fn build_environment_text(env: &FeedbackEnvironment) -> String {
    let installed_with = env.install_method.as_deref().unwrap_or("");
    let platform = platform_line(&env.os, &env.target);
    [
        format!("- agent-wechat version (`wx version`): {}", env.version.trim()),
        format!(
            "- Installed with (npm, release binary, Docker, desktop app, OpenClaw managed mode, source): {installed_with}"
        ),
        format!("- OS and architecture: {platform}"),
        "- Logged in with (`wx auth login`, or `--official`): ".to_owned(),
    ]
    .join("\n")
}

/// Builds the pre-filled GitHub Bug Report URL with non-sensitive metadata only.
/// Never includes account IDs, paths, addresses, or message data.
pub fn build_issue_url(env: &FeedbackEnvironment, diagnostics: &DiagnosticSummaries) -> String {
    let mut url = reqwest::Url::parse(&format!("{GITHUB_REPO_URL}/issues/new"))
        .expect("static GitHub issue URL");
    let environment = build_environment_text(env);
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("template", GITHUB_ISSUE_TEMPLATE);
        pairs.append_pair("environment", &environment);
        if let Some(logs) = diagnostics.format_logs() {
            pairs.append_pair("logs", &logs);
        }
    }
    url.into()
}

pub fn build_feedback_output(
    env: FeedbackEnvironment,
    diagnostics: &DiagnosticSummaries,
) -> FeedbackOutput {
    let issue_url = build_issue_url(&env, diagnostics);
    FeedbackOutput {
        repository: GITHUB_REPO_URL,
        issue_url,
        environment: env,
    }
}

pub fn format_feedback_human(output: &FeedbackOutput) -> String {
    let install_method = output
        .environment
        .install_method
        .as_deref()
        .unwrap_or("unknown");
    let platform = platform_line(&output.environment.os, &output.environment.target);
    format!(
        "Repository: {}\nIssue URL: {}\nEnvironment:\n  Version: {}\n  Install method: {}\n  OS and target: {}\n",
        output.repository,
        output.issue_url,
        output.environment.version,
        install_method,
        if platform.is_empty() { "—" } else { &platform },
    )
}

pub fn run(open: bool, json: bool) -> Result<()> {
    let output = build_feedback_output(collect_environment(), &DiagnosticSummaries::default());
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    print!("{}", format_feedback_human(&output));
    if open {
        open_browser(&output.issue_url).map_err(|error| {
            outcome::failure(
                9,
                "browser_open_failed",
                &format!("could not open browser: {error}"),
            )
        })?;
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    if !url.starts_with(&format!("{GITHUB_REPO_URL}/")) {
        bail!("refusing to open unexpected URL");
    }
    #[cfg(target_os = "macos")]
    {
        ProcessCommand::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to launch open")?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        ProcessCommand::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to launch rundll32")?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if !has_display {
            bail!("no graphical display detected (DISPLAY and WAYLAND_DISPLAY are unset)");
        }
        ProcessCommand::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to launch xdg-open")?;
        Ok(())
    }
}

pub fn is_valid_date_key(value: &str) -> bool {
    value.len() == 10
        && value.is_ascii()
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

pub fn local_date_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub fn is_suppressed(no_prompts: Option<&OsStr>, ci: Option<&OsStr>) -> bool {
    if no_prompts.is_some_and(|value| value == OsStr::new("1")) {
        return true;
    }
    if let Some(ci_value) = ci {
        if !ci_value.is_empty()
            && ci_value != OsStr::new("0")
            && !ci_value
                .to_str()
                .is_some_and(|text| text.eq_ignore_ascii_case("false"))
        {
            return true;
        }
    }
    false
}

pub fn select_notice_mode(
    json: bool,
    stderr_is_tty: bool,
    suppressed: bool,
    no_color: bool,
) -> NoticeMode {
    if suppressed {
        NoticeMode::None
    } else if json {
        NoticeMode::Json
    } else if stderr_is_tty {
        NoticeMode::Terminal { dim: !no_color }
    } else {
        NoticeMode::None
    }
}

pub fn format_notice(mode: NoticeMode) -> Option<String> {
    match mode {
        NoticeMode::None => None,
        NoticeMode::Terminal { dim } => {
            let line1 =
                "Enjoying wx? A star helps others find it: https://github.com/thisnick/agent-wechat";
            let line2 =
                "Something wrong? `wx feedback` opens a bug report with your version filled in.";
            if dim {
                Some(format!("\x1b[2m{line1}\x1b[0m\n\x1b[2m{line2}\x1b[0m"))
            } else {
                Some(format!("{line1}\n{line2}"))
            }
        }
        NoticeMode::Json => {
            let payload = JsonNotice {
                event: "notice",
                code: "support_invite",
                audience: "user",
                message: "If agent-wechat is useful, a GitHub star helps: https://github.com/thisnick/agent-wechat. Report problems with `wx feedback`.",
            };
            serde_json::to_string(&payload).ok()
        }
    }
}

pub fn state_path_for_client_config(client_config_path: &Path) -> Option<PathBuf> {
    client_config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(SUPPORT_PROMPT_FILENAME))
}

/// Returns whether a send command's JSON result counts as an accepted send.
/// Matches commands that exit 0 (`sent`, `accepted`, `queued` with `--detach`, etc.)
/// and excludes failed, `needs_review`, uncertain, timed-out, or interrupted results.
pub fn is_accepted_send(result: &Value) -> bool {
    outcome::result_exit(result) == 0
}

fn read_state(path: &Path) -> Result<Option<SupportPromptState>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || wechat_platform::is_link(&metadata)
                || metadata.len() > MAX_SUPPORT_PROMPT_BYTES
                || !wechat_platform::owned_path(path)?
            {
                bail!("support-prompt state file must be a bounded owned regular file");
            }
            let file = wechat_platform::OpenOptions::new()
                .read(true)
                .custom_flags(wechat_platform::NOFOLLOW)
                .open(path)?;
            let mut bytes = Vec::new();
            file.take(MAX_SUPPORT_PROMPT_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_SUPPORT_PROMPT_BYTES {
                bail!("support-prompt state file exceeds size limit");
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            if !value.is_object() {
                bail!("support-prompt state must be a JSON object");
            }
            let state: SupportPromptState = serde_json::from_value(value)?;
            Ok(Some(state))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_state_atomic(path: &Path, state: &SupportPromptState) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("support-prompt parent directory missing")?;
    if !parent.exists() {
        wechat_platform::private_dir(parent)?;
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || !wechat_platform::owned_path(parent)? {
        bail!("support-prompt directory must be owned by this user");
    }
    if let Ok(existing) = std::fs::symlink_metadata(path) {
        if !existing.is_file()
            || wechat_platform::is_link(&existing)
            || !wechat_platform::owned_path(path)?
        {
            bail!("support-prompt path is not an owned regular file");
        }
    }
    let bytes = serde_json::to_vec(state)?;
    let mut temp = wechat_platform::private_tempfile_in(parent, ".wx-support-prompt-")?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}

/// Records one accepted send in `path` and returns the notice text to print to stderr
/// if the threshold is met, the notice has not been shown before, output gating allows it,
/// and the updated state was durably written.
///
/// Fails closed: if the file cannot be read or written, returns `None` and never shows a notice.
pub fn record_accepted_send(path: &Path, today: &str, mode: NoticeMode) -> Option<String> {
    if !is_valid_date_key(today) {
        return None;
    }
    let current = match read_state(path) {
        Ok(Some(state)) => state,
        Ok(None) => SupportPromptState::default(),
        Err(_) => return None,
    };
    if current.shown {
        return None;
    }

    let mut days = Vec::new();
    for day in current.days {
        if is_valid_date_key(&day) && !days.contains(&day) {
            days.push(day);
        }
    }
    if days.len() > DAYS_BEFORE_ASKING {
        days.drain(0..days.len() - DAYS_BEFORE_ASKING);
    }
    if !days.iter().any(|day| day == today) {
        days.push(today.to_owned());
        if days.len() > DAYS_BEFORE_ASKING {
            days.drain(0..days.len() - DAYS_BEFORE_ASKING);
        }
    }
    let sends = current.sends.saturating_add(1);
    let eligible = days.len() >= DAYS_BEFORE_ASKING && sends >= SENDS_BEFORE_ASKING;
    let will_show = eligible && !matches!(mode, NoticeMode::None);
    let next = SupportPromptState {
        days,
        sends,
        shown: will_show,
    };
    if write_state_atomic(path, &next).is_err() {
        return None;
    }
    if will_show {
        format_notice(mode)
    } else {
        None
    }
}

pub fn maybe_record_send_and_emit_notice(client_config_path: &Path, result: &Value, json: bool) {
    if outcome::exit() != 0 || !is_accepted_send(result) {
        return;
    }
    let Some(state_path) = state_path_for_client_config(client_config_path) else {
        return;
    };
    let suppressed = is_suppressed(
        std::env::var_os("AGENT_WECHAT_NO_PROMPTS").as_deref(),
        std::env::var_os("CI").as_deref(),
    );
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let mode = select_notice_mode(json, std::io::stderr().is_terminal(), suppressed, no_color);
    let today = local_date_key();
    if let Some(notice) = record_accepted_send(&state_path, &today, mode) {
        eprintln!("{notice}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_saved(path: &Path) -> SupportPromptState {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn issue_url_matches_desktop_template_and_omits_sensitive_data() {
        let env = FeedbackEnvironment {
            version: "3.2.1".into(),
            install_method: Some("npm".into()),
            os: "Linux".into(),
            target: "x86_64-unknown-linux-gnu".into(),
        };
        let output = build_feedback_output(env, &DiagnosticSummaries::default());
        assert_eq!(output.repository, GITHUB_REPO_URL);
        let parsed = reqwest::Url::parse(&output.issue_url).unwrap();
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("github.com"));
        assert_eq!(parsed.path(), "/thisnick/agent-wechat/issues/new");
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("template").map(String::as_str),
            Some("bug_report.yml")
        );
        let env_text = params.get("environment").unwrap();
        assert_eq!(
            env_text,
            "- agent-wechat version (`wx version`): 3.2.1\n\
             - Installed with (npm, release binary, Docker, desktop app, OpenClaw managed mode, source): npm\n\
             - OS and architecture: Linux (x86_64-unknown-linux-gnu)\n\
             - Logged in with (`wx auth login`, or `--official`): "
        );
        assert!(!params.contains_key("logs"));

        // JSON shape matches {repository, issueUrl, environment}.
        let json_value = serde_json::to_value(&output).unwrap();
        assert_eq!(json_value["repository"], GITHUB_REPO_URL);
        assert_eq!(json_value["issueUrl"], output.issue_url);
        assert_eq!(json_value["environment"]["version"], "3.2.1");
        assert_eq!(json_value["environment"]["installMethod"], "npm");
        assert_eq!(json_value["environment"]["os"], "Linux");
        assert_eq!(
            json_value["environment"]["target"],
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn issue_url_leaves_install_method_blank_when_unknown_and_supports_diagnostics_seam() {
        let env = FeedbackEnvironment {
            version: "3.2.1".into(),
            install_method: None,
            os: "macOS".into(),
            target: "aarch64-apple-darwin".into(),
        };
        let url_plain = build_issue_url(&env, &DiagnosticSummaries::default());
        let parsed = reqwest::Url::parse(&url_plain).unwrap();
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert!(params["environment"].contains(
            "- Installed with (npm, release binary, Docker, desktop app, OpenClaw managed mode, source): \n"
        ));
        assert!(!params.contains_key("logs"));

        let diagnostics = DiagnosticSummaries {
            status_summary: Some("daemon: reachable".into()),
            doctor_summary: Some("checks: ok".into()),
        };
        let url_with_diag = build_issue_url(&env, &diagnostics);
        let parsed_diag = reqwest::Url::parse(&url_with_diag).unwrap();
        let diag_params: std::collections::HashMap<_, _> =
            parsed_diag.query_pairs().into_owned().collect();
        assert!(diag_params["logs"].contains("wx status --json (sanitized):\ndaemon: reachable"));
        assert!(diag_params["logs"].contains("wx daemon doctor --json (sanitized):\nchecks: ok"));
    }

    #[test]
    fn install_method_detection_covers_expected_layouts() {
        // Official Docker image and generic container with release binary at /usr/local/bin/wx.
        assert_eq!(
            detect_install_method(
                Some(Path::new("/usr/local/bin/wx")),
                true,
                false,
                false,
                false,
                false
            ),
            Some("Docker")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/usr/local/bin/wx")),
                false,
                true,
                false,
                false,
                false
            ),
            Some("Docker")
        );
        // Source or npm inside a generic dev/CI container should not be mislabelled as Docker.
        assert_eq!(
            detect_install_method(
                Some(Path::new("/workspace/target/debug/wx")),
                false,
                true,
                false,
                false,
                true
            ),
            Some("source")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new(
                    "/usr/lib/node_modules/@agent-wechat/cli/native/3.2.1/linux-x64-gnu/wx"
                )),
                false,
                true,
                false,
                false,
                false
            ),
            Some("npm")
        );
        // Desktop app on macOS, Linux .deb, and Windows.
        for desktop_path in [
            "/Applications/AgentWechat.app/Contents/MacOS/wx",
            "/usr/lib/AgentWechat/bin/wx",
            "/Users/u/AppData/Local/AgentWechat/wx.exe",
            "/Users/u/AppData/Local/AgentWechat/bin/wx.exe",
        ] {
            assert_eq!(
                detect_install_method(
                    Some(Path::new(desktop_path)),
                    false,
                    false,
                    false,
                    false,
                    false
                ),
                Some("desktop app"),
                "failed to detect desktop app for {desktop_path}"
            );
        }
        assert_eq!(
            detect_install_method(
                Some(Path::new(
                    "/usr/lib/node_modules/@agent-wechat/cli/native/3.2.1/linux-x64-gnu/wx"
                )),
                false,
                false,
                false,
                false,
                false
            ),
            Some("npm")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/opt/custom/native/3.2.1/linux-x64-gnu/wx")),
                false,
                false,
                true,
                false,
                false
            ),
            Some("npm")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/home/user/AgentWechat/target/release/wx")),
                false,
                false,
                false,
                false,
                false
            ),
            Some("source")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/home/user/.cargo/bin/wx")),
                false,
                false,
                false,
                false,
                false
            ),
            Some("source")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/opt/bin/wx-v3.2.1-linux-x64-gnu")),
                false,
                false,
                false,
                false,
                false
            ),
            Some("release binary")
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/usr/local/bin/wx")),
                false,
                false,
                false,
                false,
                false
            ),
            None
        );
        assert_eq!(
            detect_install_method(
                Some(Path::new("/usr/local/bin/wx")),
                false,
                false,
                false,
                false,
                true
            ),
            Some("source")
        );
    }

    #[test]
    fn requires_three_distinct_local_days_and_fifteen_sends_before_showing() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(SUPPORT_PROMPT_FILENAME);

        // 16 sends across only 2 days: never eligible.
        for _ in 0..10 {
            assert_eq!(
                record_accepted_send(
                    &state_path,
                    "2026-11-01",
                    NoticeMode::Terminal { dim: true }
                ),
                None
            );
        }
        for _ in 0..6 {
            assert_eq!(
                record_accepted_send(
                    &state_path,
                    "2026-11-03",
                    NoticeMode::Terminal { dim: true }
                ),
                None
            );
        }
        let mid = read_saved(&state_path);
        assert_eq!(mid.days, vec!["2026-11-01", "2026-11-03"]);
        assert_eq!(mid.sends, 16);
        assert!(!mid.shown);

        // Non-consecutive 3rd day reaches threshold and shows once.
        let shown = record_accepted_send(
            &state_path,
            "2026-11-09",
            NoticeMode::Terminal { dim: true },
        );
        assert!(shown.is_some());
        let text = shown.unwrap();
        assert!(text.contains("\x1b[2mEnjoying wx? A star helps others find it: https://github.com/thisnick/agent-wechat\x1b[0m"));
        assert!(text.contains("\x1b[2mSomething wrong? `wx feedback` opens a bug report with your version filled in.\x1b[0m"));
        assert!(read_saved(&state_path).shown);

        // Subsequent sends in either mode never show again.
        assert_eq!(
            record_accepted_send(
                &state_path,
                "2026-11-10",
                NoticeMode::Terminal { dim: true }
            ),
            None
        );
        assert_eq!(
            record_accepted_send(&state_path, "2026-11-10", NoticeMode::Json),
            None
        );
    }

    #[test]
    fn three_days_before_fifteen_sends_waits_until_fifteenth_send() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(SUPPORT_PROMPT_FILENAME);

        assert_eq!(
            record_accepted_send(&state_path, "2026-11-01", NoticeMode::Json),
            None
        );
        assert_eq!(
            record_accepted_send(&state_path, "2026-11-02", NoticeMode::Json),
            None
        );
        for _ in 0..12 {
            assert_eq!(
                record_accepted_send(&state_path, "2026-11-05", NoticeMode::Json),
                None
            );
        }
        // 15th send triggers JSON notice on stderr.
        let notice = record_accepted_send(&state_path, "2026-11-05", NoticeMode::Json).unwrap();
        assert_eq!(
            notice,
            r#"{"event":"notice","code":"support_invite","audience":"user","message":"If agent-wechat is useful, a GitHub star helps: https://github.com/thisnick/agent-wechat. Report problems with `wx feedback`."}"#
        );
        let saved = read_saved(&state_path);
        assert_eq!(saved.sends, 15);
        assert_eq!(saved.days.len(), 3);
        assert!(saved.shown);

        // Shared flag prevents terminal mode from showing afterwards.
        assert_eq!(
            record_accepted_send(
                &state_path,
                "2026-11-06",
                NoticeMode::Terminal { dim: false }
            ),
            None
        );
    }

    #[test]
    fn piped_or_suppressed_runs_keep_counting_without_showing_or_marking_shown() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(SUPPORT_PROMPT_FILENAME);

        // Reach threshold while piped / suppressed (NoticeMode::None).
        assert_eq!(
            record_accepted_send(&state_path, "2026-11-01", NoticeMode::None),
            None
        );
        assert_eq!(
            record_accepted_send(&state_path, "2026-11-02", NoticeMode::None),
            None
        );
        for _ in 0..13 {
            assert_eq!(
                record_accepted_send(&state_path, "2026-11-03", NoticeMode::None),
                None
            );
        }
        let saved = read_saved(&state_path);
        assert_eq!(saved.sends, 15);
        assert_eq!(saved.days.len(), 3);
        assert!(!saved.shown);

        // Next interactive send displays the notice and marks shown = true.
        let notice = record_accepted_send(
            &state_path,
            "2026-11-04",
            NoticeMode::Terminal { dim: false },
        );
        assert!(notice.is_some());
        assert!(!notice.unwrap().contains("\x1b["));
        let after = read_saved(&state_path);
        assert!(after.shown);
        assert_eq!(after.days.len(), DAYS_BEFORE_ASKING);
        assert_eq!(after.days, vec!["2026-11-02", "2026-11-03", "2026-11-04"]);
    }

    #[test]
    fn output_gating_respects_tty_json_no_prompts_ci_and_no_color() {
        assert!(!is_suppressed(None, None));
        assert!(!is_suppressed(Some(OsStr::new("0")), Some(OsStr::new("0"))));
        assert!(!is_suppressed(
            Some(OsStr::new("")),
            Some(OsStr::new("false"))
        ));
        assert!(is_suppressed(Some(OsStr::new("1")), None));
        assert!(is_suppressed(None, Some(OsStr::new("true"))));
        assert!(is_suppressed(None, Some(OsStr::new("1"))));

        assert_eq!(
            select_notice_mode(false, true, false, false),
            NoticeMode::Terminal { dim: true }
        );
        assert_eq!(
            select_notice_mode(false, true, false, true),
            NoticeMode::Terminal { dim: false }
        );
        // JSON mode emits stderr notice even when stderr is not a TTY.
        assert_eq!(
            select_notice_mode(true, false, false, false),
            NoticeMode::Json
        );
        assert_eq!(
            select_notice_mode(true, true, false, false),
            NoticeMode::Json
        );
        // Piped non-JSON run suppresses notice.
        assert_eq!(
            select_notice_mode(false, false, false, false),
            NoticeMode::None
        );
        // Suppressed overrides both TTY and JSON.
        assert_eq!(
            select_notice_mode(true, true, true, false),
            NoticeMode::None
        );
        assert_eq!(
            select_notice_mode(false, true, true, false),
            NoticeMode::None
        );
    }

    #[test]
    fn fails_closed_when_state_file_is_corrupt_symlink_or_unwritable() {
        let dir = tempfile::tempdir().unwrap();

        // Corrupt JSON: fails closed, never shows, never clobbers file.
        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "{not valid json").unwrap();
        assert_eq!(
            record_accepted_send(&corrupt, "2026-11-01", NoticeMode::Json),
            None
        );
        assert_eq!(
            std::fs::read_to_string(&corrupt).unwrap(),
            "{not valid json"
        );

        // Non-object JSON: fails closed.
        let array_json = dir.path().join("array.json");
        std::fs::write(&array_json, "[]").unwrap();
        assert_eq!(
            record_accepted_send(&array_json, "2026-11-01", NoticeMode::Json),
            None
        );

        // Symlink at state path: fails closed.
        #[cfg(unix)]
        {
            let target = dir.path().join("real.json");
            std::fs::write(
                &target,
                r#"{"days":["2026-11-01","2026-11-02"],"sends":14,"shown":false}"#,
            )
            .unwrap();
            let link = dir.path().join("link.json");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(
                record_accepted_send(&link, "2026-11-03", NoticeMode::Json),
                None
            );
        }

        // Unwritable directory: even if threshold would be crossed, never shows notice.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = dir.path().join("readonly");
            std::fs::create_dir(&locked).unwrap();
            let state_file = locked.join(SUPPORT_PROMPT_FILENAME);
            std::fs::write(
                &state_file,
                r#"{"days":["2026-11-01","2026-11-02"],"sends":14,"shown":false}"#,
            )
            .unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
            let result = record_accepted_send(&state_file, "2026-11-03", NoticeMode::Json);
            // Restore permissions before asserting so tempdir cleanup always succeeds.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
            if unsafe { libc::geteuid() } != 0 {
                assert_eq!(result, None);
                assert!(!read_saved(&state_file).shown);
            }
        }
    }

    #[test]
    fn synthetic_send_results_count_only_accepted_exit_zero_outcomes() {
        for accepted in [
            json!({"id": "s1", "state": "sent"}),
            json!({"id": "s2", "state": "accepted"}),
            json!({"id": "s3", "state": "succeeded"}),
            json!({"id": "s4", "state": "queued"}), // --detach accepted by daemon
        ] {
            assert!(is_accepted_send(&accepted), "should count: {accepted}");
        }
        for excluded in [
            json!({"id": "s5", "state": "failed"}),
            json!({"id": "s6", "state": "needs_review"}),
            json!({"id": "s7", "state": "unknown"}),
            json!({"id": "s8", "state": "rejected"}),
            json!({"id": "s9", "state": "cancelled"}),
            json!({"id": "s10", "state": "needs_login"}),
            json!({"id": "s11", "state": "queued", "clientWaitExpired": true}),
            json!({"id": "s12", "state": "queued", "clientInterrupted": true}),
        ] {
            assert!(!is_accepted_send(&excluded), "should not count: {excluded}");
        }
    }

    #[test]
    fn state_path_resolves_next_to_client_toml() {
        let dir = tempfile::tempdir().unwrap();
        let client_toml = dir.path().join("agent-wechat/client.toml");
        assert_eq!(
            state_path_for_client_config(&client_toml),
            Some(dir.path().join("agent-wechat/support-prompt.json"))
        );
    }

    #[test]
    fn date_key_validation_accepts_only_real_iso_calendar_days() {
        assert!(is_valid_date_key("2026-11-27"));
        assert!(!is_valid_date_key("2026-13-01"));
        assert!(!is_valid_date_key("2026-02-30"));
        assert!(!is_valid_date_key("2026-1-1"));
        assert!(!is_valid_date_key("not-a-date"));
    }
}
