use super::Cli;
use clap::{Command, CommandFactory};

pub(super) fn command() -> Command {
    configure(Cli::command(), &[])
}

fn configure(command: Command, parents: &[&str]) -> Command {
    let name = command.get_name().to_owned();
    let mut path = parents.to_vec();
    path.push(&name);
    let guidance = match path.as_slice() {
        ["wx"] => "Getting started:\n  wx config servers list\n  wx config servers select box\n  wx accounts list\n  wx accounts select personal\n  wx accounts status\n\nUse wx <COMMAND> --help for a command's options and subcommands.\nSet AGENT_WECHAT_TOKEN to supply an API token through the environment.",
        ["wx", "accounts"] => "Examples:\n  wx accounts list\n  wx accounts select personal\n  wx accounts status\n  wx --account personal accounts settings\n\nSelect saves the default account for the current named server.\n--account ID or AGENT_WECHAT_ACCOUNT overrides that choice for a command.\nUse wx accounts <COMMAND> --help for its arguments and options.",
        ["wx", "accounts", "select"] => "Example: wx accounts select personal\n\nChoose an account ID or alias from wx accounts list.\nFirst save and select a named server with wx config servers.\nThis saves a CLI default; it does not sign in or reconnect the account.",
        ["wx", "profiles"] => "Examples:\n  wx profiles add archive --path /absolute/path/to/stopped-profile\n  wx profiles list\n  wx profiles select archive\n  wx profiles inspect\n\nProfiles are local stopped files. --local-profile NAME overrides the saved choice.\nUse wx profiles <COMMAND> --help for its arguments and options.",
        ["wx", "profiles", "select"] => "Example: wx profiles select archive\n\nChoose a name from wx profiles list. With a selected saved server, this saves\nthat server's profile default; otherwise it saves the standalone default.\nThis selects local files; it does not sign an account in to WeChat.",
        ["wx", "auth", "logout"] => "Examples:\n  wx auth logout\n  wx auth logout --local\n\nBoth stop automatic reconnection. Use --local to skip the WeChat logout request and keep saved credentials.",
        ["wx", "contacts", "lookup"] => "Example: wx contacts lookup CONTACT_ID OTHER_CONTACT_ID\n\nUse contacts find to search saved contacts by name.",
        ["wx", "groups", "lookup"] => "Example: wx groups lookup 'room@chatroom'\n\nUse groups find to search saved groups by name.",
        ["wx", "config"] => "Examples:\n  wx config client\n  wx config servers list\n\nUse wx config <COMMAND> --help for its arguments and options.",
        ["wx", "config", "servers"] => "Examples:\n  wx config servers add box --url https://wx.example.com --token-file /absolute/path/to/api-token\n  wx config servers list\n  wx config servers select box\n\n--server-name NAME or AGENT_WECHAT_SERVER overrides the saved server.\nUse wx config servers <COMMAND> --help for its arguments and options.",
        ["wx", "config", "servers", "select"] => "Example: wx config servers select box\n\nChoose a name from wx config servers list. This saves the server used by\nsubsequent commands; it does not start a daemon or sign in to WeChat.",
        ["wx", "config", "servers", "remove"] => "Example: wx config servers remove box\n\nRemoves this CLI's saved server entry and clears the selection if it was active.\nToken files and local profiles are retained.",
        _ if command.has_subcommands() => "Run this command with a subcommand and --help to see its arguments and options.",
        ["wx", "profiles", ..] => "Uses stopped local files. Select them with wx profiles select NAME or --local-profile NAME.\nUse --source-stopped only after stopping the owner or making a consistent snapshot.",
        ["wx", "config", ..] => "CLI server choices are saved locally. Use --config FILE to choose another client configuration.",
        ["wx", "daemon" | "store" | "serve", ..] => "Runs on this computer. Use --data-dir PATH and --server-config FILE for the local daemon store.",
        ["wx", "version" | "licenses"] => "Runs locally without a server connection or API token.",
        _ => "Use --server-name NAME to select a saved server and --account ID to select an account.\nRun wx accounts list to find account IDs and aliases. AGENT_WECHAT_TOKEN supplies an API token.",
    };
    let command = if matches!(
        path.as_slice(),
        ["wx", "contacts", "members"] | ["wx", "groups", "avatar"]
    ) {
        command.hide(true)
    } else {
        command
    };
    command
        // Clear derive's declaration priorities, including around built-in help.
        .display_order(None)
        .after_help(guidance)
        .mut_subcommands(|child| configure(child, &path))
}

#[cfg(test)]
#[path = "cli_help_tests.rs"]
mod tests;
