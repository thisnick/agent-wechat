use super::command;
use clap::{ArgMatches, Command};

#[test]
fn every_help_level_sorts_commands_and_explains_actions() {
    fn check(mut command: Command) {
        let name = command.get_name().to_owned();
        if name == "help" {
            return;
        }
        assert!(command.get_about().is_some(), "missing description: {name}");
        assert!(
            command.get_after_help().is_some(),
            "missing guidance: {name}"
        );
        let help = command.render_long_help().to_string();
        assert!(!help.contains("[aliases:"), "visible aliases: {help}");
        let mut names = command
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
            .map(|child| child.get_name().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        let positions = names
            .iter()
            .map(|name| {
                help.find(&format!("\n  {name} "))
                    .unwrap_or_else(|| panic!("missing {name} in {help}"))
            })
            .collect::<Vec<_>>();
        assert!(
            positions.windows(2).all(|p| p[0] < p[1]),
            "unsorted: {help}"
        );
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
        {
            check(child.clone());
        }
    }
    let mut root = command();
    root.build();
    check(root);
}

fn path(matches: &ArgMatches) -> Vec<String> {
    matches
        .subcommand()
        .map(|(name, child)| {
            let mut result = vec![name.to_owned()];
            result.extend(path(child));
            result
        })
        .unwrap_or_default()
}

#[test]
fn hidden_legacy_spellings_resolve_to_the_canonical_commands() {
    let pairs: &[(&[&str], &[&str])] = &[
        (&["account", "status"], &["accounts", "status"]),
        (&["account", "connect"], &["accounts", "connect"]),
        (&["account", "disconnect"], &["accounts", "disconnect"]),
        (&["account", "capabilities"], &["accounts", "capabilities"]),
        (
            &["account", "settings", "--auto-download-max-mb", "10"],
            &["accounts", "settings", "--auto-download-max-mb", "10"],
        ),
        (
            &["accounts", "default", "personal"],
            &["accounts", "select", "personal"],
        ),
        (&["profile", "list"], &["profiles", "list"]),
        (
            &["profile", "use", "archive"],
            &["profiles", "select", "archive"],
        ),
        (
            &["profiles", "use", "archive"],
            &["profiles", "select", "archive"],
        ),
        (
            &["config", "server", "list"],
            &["config", "servers", "list"],
        ),
        (
            &["config", "server", "use", "box"],
            &["config", "servers", "select", "box"],
        ),
        (
            &["config", "servers", "use", "box"],
            &["config", "servers", "select", "box"],
        ),
    ];
    for (legacy, canonical) in pairs {
        let parse = |args: &[&str]| {
            command()
                .try_get_matches_from(
                    ["wx", "--account", "personal"]
                        .into_iter()
                        .chain(args.iter().copied()),
                )
                .unwrap()
        };
        let old = parse(legacy);
        let new = parse(canonical);
        assert_eq!(path(&old), path(&new));
        assert_eq!(
            old.get_one::<String>("account"),
            new.get_one::<String>("account")
        );
        assert_eq!(old.get_one::<String>("account").unwrap(), "personal");
    }
}

#[test]
fn help_advertises_only_plural_groups_and_select() {
    for (args, included, excluded) in [
        (
            vec![],
            vec!["accounts", "profiles"],
            vec!["account", "profile"],
        ),
        (
            vec!["accounts"],
            vec!["select", "status", "settings", "connect"],
            vec!["default"],
        ),
        (vec!["profiles"], vec!["select"], vec!["use"]),
        (vec!["config"], vec!["servers"], vec!["server"]),
        (vec!["config", "servers"], vec!["select"], vec!["use"]),
    ] {
        let error = command()
            .try_get_matches_from(["wx"].into_iter().chain(args).chain(["--help"]))
            .unwrap_err();
        assert_eq!(error.exit_code(), 0);
        let help = error.to_string();
        for name in included {
            assert!(help.contains(&format!("\n  {name} ")), "{help}");
        }
        for name in excluded {
            assert!(!help.contains(&format!("\n  {name} ")), "{help}");
        }
    }
    for args in [
        vec!["accounts", "select"],
        vec!["profiles", "select"],
        vec!["config", "servers", "select"],
    ] {
        let help = command()
            .try_get_matches_from(["wx"].into_iter().chain(args).chain(["--help"]))
            .unwrap_err()
            .to_string();
        assert!(help.contains("Example: wx"));
        assert!(help.contains("saves"));
    }
}
