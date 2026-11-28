use crate::cli_help;
use anyhow::Result;
use clap::{ArgGroup, Command, ValueEnum};
use clap_complete::Shell;
use std::{
    collections::HashSet,
    io::{self, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell")]
    PowerShell,
}

impl From<CompletionShell> for Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::PowerShell => Self::PowerShell,
        }
    }
}

/// Builds a completion-ready copy of `cmd` without hidden subcommands or hidden
/// arguments.
///
/// `cli_help::command()` keeps legacy commands as `hide(true)` so they remain
/// parseable and appear as hidden in `interfaces/cli/wx.txt`, whereas
/// `clap_complete` walks `Command::get_subcommands()` without skipping hidden
/// entries.
fn visible_command(cmd: &Command) -> Command {
    let mut out = Command::new(cmd.get_name().to_owned()).display_order(None);
    if let Some(bin_name) = cmd.get_bin_name() {
        out = out.bin_name(bin_name.to_owned());
    }
    if let Some(about) = cmd.get_about() {
        out = out.about(about.clone());
    }
    if let Some(long_about) = cmd.get_long_about() {
        out = out.long_about(long_about.clone());
    }
    if let Some(version) = cmd.get_version() {
        out = out.version(version.to_owned());
    }
    out = out
        .disable_help_flag(cmd.is_disable_help_flag_set())
        .disable_help_subcommand(cmd.is_disable_help_subcommand_set())
        .disable_version_flag(cmd.is_disable_version_flag_set())
        .subcommand_required(cmd.is_subcommand_required_set())
        .visible_aliases(cmd.get_visible_aliases().map(str::to_owned));

    let mut kept_args = HashSet::new();
    for arg in cmd.get_arguments() {
        if !arg.is_hide_set() {
            kept_args.insert(arg.get_id().clone());
            out = out.arg(arg.clone());
        }
    }
    for group in cmd.get_groups() {
        if group.get_args().all(|id| kept_args.contains(id)) {
            out = out.group(group.clone());
        } else {
            let members: Vec<_> = group
                .get_args()
                .filter(|id| kept_args.contains(*id))
                .cloned()
                .collect();
            if !members.is_empty() {
                let mut cloned = group.clone();
                let rebuilt = ArgGroup::new(group.get_id().clone())
                    .args(members)
                    .required(group.is_required_set())
                    .multiple(cloned.is_multiple());
                out = out.group(rebuilt);
            }
        }
    }
    let mut subcommands: Vec<_> = cmd
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
        .collect();
    subcommands.sort_by_key(|child| child.get_name());
    for child in subcommands {
        out = out.subcommand(visible_command(child));
    }
    out
}

pub(crate) fn render(shell: CompletionShell) -> Vec<u8> {
    let mut cmd = visible_command(&cli_help::command());
    let mut buf = Vec::new();
    clap_complete::generate(Shell::from(shell), &mut cmd, "wx", &mut buf);
    buf
}

pub(crate) fn write(shell: CompletionShell, out: &mut impl Write) -> Result<()> {
    let script = render(shell);
    match out.write_all(&script).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command as CliCommand};
    use clap::FromArgMatches;

    fn collect_paths(cmd: &Command, prefix: &[&str], out: &mut Vec<String>) {
        let mut path = prefix.to_vec();
        path.push(cmd.get_name());
        out.push(path.join(" "));
        for arg in cmd.get_arguments() {
            assert!(
                !arg.is_hide_set(),
                "hidden argument remained in completion tree: {} {}",
                path.join(" "),
                arg.get_id()
            );
        }
        for child in cmd.get_subcommands() {
            if child.get_name() == "help" {
                continue;
            }
            assert!(
                !child.is_hide_set(),
                "hidden subcommand remained in completion tree: {} {}",
                path.join(" "),
                child.get_name()
            );
            assert!(
                child.get_all_aliases().count() == child.get_visible_aliases().count(),
                "hidden alias remained on {} {}",
                path.join(" "),
                child.get_name()
            );
            collect_paths(child, &path, out);
        }
    }

    #[test]
    fn visible_command_tree_omits_hidden_commands_and_aliases() {
        let mut cmd = visible_command(&cli_help::command());
        cmd.build();
        let mut paths = Vec::new();
        collect_paths(&cmd, &[], &mut paths);

        for expected in [
            "wx accounts",
            "wx accounts select",
            "wx auth logout",
            "wx chats list",
            "wx completions",
            "wx config servers select",
            "wx contacts lookup",
            "wx groups lookup",
            "wx groups members",
            "wx profiles select",
            "wx version",
        ] {
            assert!(
                paths.iter().any(|path| path == expected),
                "missing visible command {expected}"
            );
        }
        for hidden in [
            "wx auth logout-local",
            "wx chats open",
            "wx contacts members",
            "wx groups avatar",
            "wx lookup-contacts",
            "wx lookup-group",
            "wx threads",
        ] {
            assert!(
                !paths.iter().any(|path| path == hidden),
                "hidden command leaked into completions: {hidden}"
            );
        }
    }

    #[test]
    fn generates_completions_for_every_supported_shell_without_hidden_commands() {
        let cases: [(CompletionShell, &str, &str, &[&str]); 4] = [
            (
                CompletionShell::Bash,
                "_wx()",
                "wx__subcmd__completions",
                &[
                    "wx__subcmd__threads",
                    "wx__subcmd__chats__subcmd__open",
                    "wx__subcmd__contacts__subcmd__members",
                    "wx__subcmd__groups__subcmd__avatar",
                    "wx__subcmd__auth__subcmd__logout__subcmd__local",
                ],
            ),
            (
                CompletionShell::Zsh,
                "#compdef wx",
                "(completions)",
                &[
                    "(threads)",
                    "_wx__subcmd__contacts__subcmd__members",
                    "_wx__subcmd__groups__subcmd__avatar",
                    "'open:",
                ],
            ),
            (
                CompletionShell::Fish,
                "complete -c wx",
                "-a \"completions\"",
                &[
                    "-a \"threads\"",
                    "-a \"open\"",
                    "-n \"__fish_wx_using_subcommand contacts\" -f -a \"members\"",
                    "-n \"__fish_wx_using_subcommand groups\" -f -a \"avatar\"",
                ],
            ),
            (
                CompletionShell::PowerShell,
                "Register-ArgumentCompleter -Native -CommandName 'wx'",
                "'wx;completions'",
                &[
                    "'wx;threads'",
                    "'wx;chats;open'",
                    "'wx;contacts;members'",
                    "'wx;groups;avatar'",
                    "'wx;auth;logout-local'",
                ],
            ),
        ];

        for (shell, header, completions_marker, shell_forbidden) in cases {
            let script = String::from_utf8(render(shell)).expect("completion output must be UTF-8");
            assert!(
                script.contains(header),
                "{shell:?} script missing header {header:?}"
            );
            assert!(
                script.contains(completions_marker),
                "{shell:?} script missing completions command marker {completions_marker:?}"
            );
            for expected in ["accounts", "daemon", "messages", "status", "--json"] {
                assert!(
                    script.contains(expected),
                    "{shell:?} script missing {expected:?}"
                );
            }
            if matches!(shell, CompletionShell::Bash | CompletionShell::Zsh) {
                for expected in ["bash", "zsh", "fish", "powershell"] {
                    assert!(
                        script.contains(expected),
                        "{shell:?} script missing shell value {expected:?}"
                    );
                }
            }
            for forbidden in [
                "lookup-contacts",
                "lookup-group",
                "logout-local",
                "access-tokens",
            ]
            .into_iter()
            .chain(shell_forbidden.iter().copied())
            {
                assert!(
                    !script.contains(forbidden),
                    "{shell:?} script leaked hidden command {forbidden:?}"
                );
            }
        }
    }

    #[test]
    fn cli_parses_supported_shells_and_rejects_unknown_ones() {
        for (arg, expected) in [
            ("bash", CompletionShell::Bash),
            ("zsh", CompletionShell::Zsh),
            ("fish", CompletionShell::Fish),
            ("powershell", CompletionShell::PowerShell),
        ] {
            let matches = cli_help::command()
                .try_get_matches_from(["wx", "completions", arg])
                .unwrap();
            let cli = Cli::from_arg_matches(&matches).unwrap();
            assert!(matches!(
                cli.command,
                CliCommand::Completions { shell } if shell == expected
            ));
        }

        for invalid in [vec!["wx", "completions"], vec!["wx", "completions", "tcsh"]] {
            let err = cli_help::command()
                .try_get_matches_from(invalid)
                .unwrap_err();
            assert_eq!(err.exit_code(), 2);
        }

        let help = cli_help::command()
            .try_get_matches_from(["wx", "completions", "--help"])
            .unwrap_err();
        assert_eq!(help.exit_code(), 0);
        let text = help.to_string();
        assert!(text.contains("wx completions bash"));
        assert!(text.contains("powershell"));
    }

    #[test]
    fn write_ignores_broken_pipe_and_propagates_other_io_errors() {
        struct FailWriter(io::ErrorKind);
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(self.0, "simulated error"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        write(
            CompletionShell::Bash,
            &mut FailWriter(io::ErrorKind::BrokenPipe),
        )
        .expect("broken pipe should succeed");

        let err = write(
            CompletionShell::Bash,
            &mut FailWriter(io::ErrorKind::PermissionDenied),
        )
        .expect_err("non-BrokenPipe error must propagate");
        assert!(err.to_string().contains("simulated error"));
    }

    #[test]
    fn visible_command_strips_hidden_args_and_rebuilds_partial_arg_groups() {
        let raw = Command::new("demo")
            .arg(clap::Arg::new("visible").long("visible"))
            .arg(clap::Arg::new("secret").long("secret").hide(true))
            .group(
                ArgGroup::new("choice")
                    .args(["visible", "secret"])
                    .required(true),
            );
        let mut cleaned = visible_command(&raw);
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Bash, &mut cleaned, "demo", &mut buf);
        let script = String::from_utf8(buf).unwrap();
        assert!(script.contains("--visible"));
        assert!(!script.contains("--secret"));
    }

    #[tokio::test]
    async fn run_rejects_token_stdin_for_completions() {
        let matches = cli_help::command()
            .try_get_matches_from(["wx", "--token-stdin", "completions", "bash"])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let err = crate::run(cli)
            .await
            .expect_err("--token-stdin must be rejected for completions");
        assert!(
            err.to_string().contains("--token-stdin"),
            "unexpected error: {err:#}"
        );
    }
}
