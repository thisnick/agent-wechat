//! Generates `interfaces/cli/wx.txt`, the reviewed listing of every command,
//! argument, flag and alias. See `interfaces/README.md` for the line format.
use super::command;
use clap::{Arg, ArgAction, Command};
use std::{fs, path::PathBuf};

const UPDATE: &str = "AGENT_WECHAT_UPDATE_INTERFACES";
const HEADER: &str = "# wx command-line interface. Generated from the clap definition; do not edit.\n# Run `pnpm interfaces` after changing the CLI. Format: ITEM | ATTRIBUTES | HELP\n";

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn help(text: Option<String>) -> String {
    text.map(|text| one_line(&text))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "MISSING HELP".to_owned())
}

fn row(item: &str, attributes: &[String], doc: &str) -> String {
    let attributes = if attributes.is_empty() {
        "-".to_owned()
    } else {
        attributes.join(", ")
    };
    format!("{item} | {attributes} | {doc}")
}

fn is_builtin(arg: &Arg) -> bool {
    matches!(
        arg.get_action(),
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
    )
}

fn arg_item(path: &str, arg: &Arg) -> String {
    if let Some(long) = arg.get_long() {
        format!("{path} --{long}")
    } else if let Some(short) = arg.get_short() {
        format!("{path} -{short}")
    } else {
        let name = arg
            .get_value_names()
            .and_then(|names| names.first())
            .map(|name| name.to_string())
            .unwrap_or_else(|| arg.get_id().as_str().to_uppercase());
        format!("{path} <{name}>")
    }
}

fn arg_attributes(arg: &Arg) -> Vec<String> {
    let mut attributes = Vec::new();
    if let Some(index) = arg.get_index() {
        attributes.push(format!("position {index}"));
    }
    if arg.is_required_set() {
        attributes.push("required".to_owned());
    }
    match arg.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse => attributes.push("switch".to_owned()),
        ArgAction::Count => attributes.push("count".to_owned()),
        action if action.takes_values() && !arg.is_positional() => {
            let names = arg
                .get_value_names()
                .map(|names| {
                    names
                        .iter()
                        .map(|name| format!("<{name}>"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_else(|| format!("<{}>", arg.get_id().as_str().to_uppercase()));
            attributes.push(format!("takes {names}"));
        }
        _ => {}
    }
    let many = arg
        .get_num_args()
        .is_some_and(|range| range.max_values() > 1);
    if matches!(arg.get_action(), ArgAction::Append) || many {
        attributes.push("repeatable".to_owned());
    }
    if arg.is_positional() {
        // Positional values are identified by position; the long/short forms below do not apply.
    } else {
        if let (Some(short), Some(_)) = (arg.get_short(), arg.get_long()) {
            attributes.push(format!("short -{short}"));
        }
        for alias in arg.get_all_aliases().unwrap_or_default() {
            attributes.push(format!("alias --{alias}"));
        }
        for alias in arg.get_all_short_aliases().unwrap_or_default() {
            attributes.push(format!("alias -{alias}"));
        }
    }
    let defaults = arg
        .get_default_values()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let switch = matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse);
    if !defaults.is_empty() && !switch {
        attributes.push(format!("default {}", defaults.join(",")));
    }
    let values = arg
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();
    if !values.is_empty() && !switch {
        attributes.push(format!("values {}", values.join("|")));
    }
    if arg.is_global_set() {
        attributes.push("global".to_owned());
    }
    if arg.is_hide_set() {
        attributes.push("hidden".to_owned());
    }
    attributes
}

fn walk(command: &Command, path: &str, inherited: &[String], rows: &mut Vec<String>) {
    let mut attributes = Vec::new();
    if command.is_hide_set() {
        attributes.push("hidden".to_owned());
    }
    let about = help(command.get_about().map(|about| about.to_string()));
    rows.push(row(path, &attributes, &about));
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str().to_owned();
        if is_builtin(arg) || (arg.is_global_set() && inherited.contains(&id)) {
            continue;
        }
        let doc = help(
            arg.get_help()
                .or_else(|| arg.get_long_help())
                .map(|text| text.to_string()),
        );
        rows.push(row(&arg_item(path, arg), &arg_attributes(arg), &doc));
    }
    let mut inherited = inherited.to_vec();
    inherited.extend(
        command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().as_str().to_owned()),
    );
    for child in command.get_subcommands() {
        if child.get_name() == "help" {
            continue;
        }
        let child_path = format!("{path} {}", child.get_name());
        let visible = child.get_visible_aliases().collect::<Vec<_>>();
        for alias in child.get_all_aliases() {
            let kind = if visible.contains(&alias) {
                "visible alias"
            } else {
                "hidden alias"
            };
            rows.push(row(
                &format!("{path} {alias}"),
                &[kind.to_owned()],
                &format!("Alias of {child_path}"),
            ));
        }
        walk(child, &child_path, &inherited, rows);
    }
}

pub(super) fn listing() -> String {
    let mut root = command();
    root.build();
    let mut rows = Vec::new();
    walk(&root, "wx", &[], &mut rows);
    // Sort by item so a command's row precedes its arguments and subcommands.
    rows.sort_by(|a, b| a.split(" | ").next().cmp(&b.split(" | ").next()));
    format!("{HEADER}{}\n", rows.join("\n"))
}

fn listing_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../interfaces/cli/wx.txt")
}

#[test]
fn cli_interface_listing_is_current() {
    let expected = listing();
    let path = listing_path();
    if std::env::var_os(UPDATE).is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &expected).unwrap();
        return;
    }
    let actual = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if actual != expected {
        let old = actual.lines().collect::<std::collections::BTreeSet<_>>();
        let new = expected.lines().collect::<std::collections::BTreeSet<_>>();
        let changes = old
            .difference(&new)
            .map(|line| format!("- {line}"))
            .chain(new.difference(&old).map(|line| format!("+ {line}")))
            .take(40)
            .collect::<Vec<_>>()
            .join("\n");
        panic!("interfaces/cli/wx.txt does not match the CLI. Run `pnpm interfaces` and review the diff:\n{changes}");
    }
}

#[test]
fn listing_rows_have_unique_items() {
    let listing = listing();
    let mut items = listing
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split(" | ").next().unwrap())
        .collect::<Vec<_>>();
    let total = items.len();
    items.dedup();
    assert_eq!(items.len(), total, "two listing rows share an item");
}
