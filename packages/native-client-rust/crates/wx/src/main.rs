use anyhow::{bail, Context, Result};
use clap::{Args, FromArgMatches, Parser, Subcommand};
use serde_json::{json, Value};
#[cfg(all(unix, test))]
use std::os::unix::fs::MetadataExt;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use wechat_core::Core;
mod calls_cli;
mod cli_help;
mod client_actions;
mod daemon;
mod events;
mod login;
mod miniprogram;
mod outcome;
mod output;
mod profile_cli;
mod profile_export;
mod settings;
use outcome::{failure, set_exit};
use output::Output;

#[derive(Parser)]
#[command(
    name = "wx",
    bin_name = "wx",
    version,
    about = "Standalone native WeChat daemon and authenticated client"
)]
struct Cli {
    /// Print machine-readable JSON, including errors and progress.
    #[arg(long, global = true)]
    json: bool,
    /// Read local daemon settings from this TOML file.
    #[arg(long, global = true)]
    server_config: Option<PathBuf>,
    /// Use this local daemon data directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Read the API token from a local private file (AGENT_WECHAT_TOKEN_FILE).
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    /// Read this invocation's API key from a private stdin pipe.
    #[arg(long, global = true, conflicts_with = "token_file")]
    token_stdin: bool,
    /// Bind the local daemon's HTTP listener to this address.
    #[arg(long, global = true)]
    listen: Option<SocketAddr>,
    /// Additional HTTPS listener for other machines. Requires both TLS files.
    #[arg(long, global = true)]
    network_listen: Option<SocketAddr>,
    /// Read the PEM certificate for the daemon's HTTPS listener.
    #[arg(long, global = true)]
    tls_cert_file: Option<PathBuf>,
    /// Read the private PEM key for the daemon's HTTPS listener.
    #[arg(long, global = true)]
    tls_key_file: Option<PathBuf>,
    /// Additional trusted CA for this client invocation; TLS verification stays enabled.
    #[arg(long, global = true)]
    ca_cert_file: Option<PathBuf>,
    /// Read saved CLI servers and defaults from this file (AGENT_WECHAT_CONFIG).
    #[arg(long = "config", alias = "client-config", global = true)]
    client_config: Option<PathBuf>,
    /// Use this daemon URL for this command (AGENT_WECHAT_URL).
    #[arg(
        long = "server",
        alias = "url",
        conflicts_with = "server_name",
        global = true
    )]
    url: Option<String>,
    /// Use a saved server for this command (AGENT_WECHAT_SERVER).
    #[arg(long, global = true)]
    server_name: Option<String>,
    /// Use this registered stopped local profile for offline reads.
    #[arg(long, global = true)]
    local_profile: Option<String>,
    /// Use an account ID or alias for this command (AGENT_WECHAT_ACCOUNT).
    #[arg(long, global = true)]
    account: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Show the CLI version, build target and API capabilities.
    Version,
    /// Show bundled third-party license notices.
    Licenses,
    /// Inspect configuration and manage saved servers.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the daemon in the foreground. Use the supplied service unit for background ownership.
    Serve {},
    /// Start, inspect or stop the local background daemon.
    Daemon {
        #[command(subcommand)]
        command: daemon::DaemonCommand,
    },
    #[command(alias = "profile")]
    /// Register and read stopped local WeChat profiles.
    Profiles {
        #[command(subcommand)]
        command: profile_cli::ProfileCommand,
    },
    /// Show the selected server's health and identity.
    Status,
    /// Observe calls or use this computer's microphone and speakers.
    Calls {
        #[command(subcommand)]
        command: Option<calls_cli::CallsCommand>,
    },
    /// Open a Mini Program from the selected account's verified package cache (experimental).
    #[command(name = "miniprogram")]
    MiniProgram {
        #[command(subcommand)]
        command: miniprogram::MiniProgramCommand,
    },
    /// Look up known contact IDs on the selected account.
    #[command(hide = true)]
    LookupContacts {
        /// Contact IDs to look up on WeChat; give one or more.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Look up a known group ID on the selected account.
    #[command(hide = true)]
    LookupGroup {
        /// Group ID, ending in @chatroom, to fetch the member roster for.
        room: String,
        /// Roster version to request from WeChat; the reply shows the current version.
        #[arg(long, default_value_t = 0)]
        version: u32,
    },
    #[command(alias = "account")]
    /// Manage accounts and inspect or connect the selected account.
    Accounts {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Fetch new messages for the selected account.
    Sync {
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// Find chats and change their read state and settings.
    Chats {
        #[command(subcommand)]
        command: ChatCommand,
    },
    /// Inspect background jobs and repair local profile writes.
    Jobs {
        #[command(subcommand)]
        command: JobsAction,
    },
    /// Inspect, cancel or retry account operations.
    Operations {
        #[command(subcommand)]
        command: ResourceAction,
    },
    /// Sign the selected account in or out of WeChat.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List message threads for the selected account.
    #[command(hide = true)]
    Threads {
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
        /// Return at most this many items.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Find contacts and refresh their saved information.
    Contacts {
        #[command(subcommand)]
        command: Option<ContactCommand>,
        /// Only list contacts whose name or ID contains this text.
        #[arg(long)]
        search: Option<String>,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Find groups and manage their names, settings and members.
    Groups {
        #[command(subcommand)]
        command: Option<GroupCommand>,
        /// Only list groups whose name or ID contains this text.
        #[arg(long)]
        search: Option<String>,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read or send messages and retrieve their attachments.
    Messages {
        #[command(subcommand)]
        command: MessageCommand,
    },
    /// Read account events or watch for new events.
    Events {
        #[command(subcommand)]
        command: Option<EventCommand>,
        /// Start after this event cursor, such as the last one you processed.
        #[arg(long, default_value = "0")]
        after: String,
        /// Keep printing new events as they arrive instead of listing once.
        #[arg(long)]
        watch: bool,
    },
    #[command(alias = "access-tokens")]
    /// Create and revoke API tokens for selected accounts.
    Tokens {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Offline operations require exclusive ownership; stop the daemon and official client first.
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    /// Show the resolved local daemon configuration.
    Show,
    /// Show the resolved client endpoint, token-file path and account.
    Client,
    #[command(alias = "server")]
    /// Register servers and select the default server for this CLI.
    Servers {
        #[command(subcommand)]
        command: ServerCommand,
    },
}
#[derive(Subcommand)]
enum ServerCommand {
    /// Save a server URL and the local path to its API token file.
    Add {
        /// Name to save the server under.
        name: String,
        /// Daemon URL, such as https://wx.example.com.
        #[arg(long)]
        url: String,
        /// File holding this server's API token.
        #[arg(long)]
        token_file: PathBuf,
    },
    /// List saved servers and the current selection.
    List,
    /// Remove a saved server from this CLI's configuration.
    Remove {
        /// Saved server name from wx config servers list.
        name: String,
    },
    #[command(alias = "use")]
    /// Save the server to use for subsequent commands.
    Select {
        /// Saved server name from wx config servers list.
        name: String,
    },
}
#[derive(Subcommand)]
enum AccountCommand {
    /// List accounts on the selected server.
    List,
    /// Create an account slot with an ID or alias.
    Create {
        /// ID for the new account; the daemon picks one if you give only --alias.
        id: Option<String>,
        /// Short name you can use in place of the account ID.
        #[arg(long)]
        alias: Option<String>,
    },
    /// Show one account by ID or alias, or the selected account.
    Get {
        /// Account ID or alias from wx accounts list; omit for the selected account.
        id: Option<String>,
    },
    #[command(alias = "default")]
    /// Save the default account for the selected named server.
    Select {
        /// Account ID or alias from wx accounts list.
        id: String,
    },
    /// Rename an account's alias.
    Alias {
        /// Account ID or current alias from wx accounts list.
        id: String,
        /// New alias for the account.
        alias: String,
    },
    /// Remove a signed-out profile from the hub, retaining its local files.
    Remove {
        /// Account ID from wx accounts list.
        id: String,
        /// Repeat the exact account ID to confirm removal.
        #[arg(long)]
        confirm: String,
    },
    /// Show authentication and connection state for the selected account.
    Status,
    /// Read or update this account's hub-side automatic download policy.
    Settings {
        /// Maximum automatic download size in MiB (0 keeps thumbnails and voice automatic).
        #[arg(long, value_parser = clap::value_parser!(u64).range(0..=512))]
        auto_download_max_mb: Option<u64>,
    },
    /// Show features available to the selected account.
    Capabilities,
    /// Reconnect the selected account using its saved WeChat session.
    Connect {
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// Disconnect the selected account while keeping its saved login.
    Disconnect,
}
#[derive(Subcommand)]
enum JobsAction {
    #[command(flatten)]
    Resource(ResourceAction),
    /// Repair local profile writes using saved bytes and receipts.
    RepairWriteback {
        /// Repair at most this many saved items in one run (1 to 100).
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
}
#[derive(Subcommand)]
enum ResourceAction {
    #[command(alias = "status")]
    /// Show the current result and state of an item by ID.
    Get {
        /// ID printed when the job, operation or login attempt was started.
        id: String,
    },
    /// Request cancellation of an item by ID.
    Cancel {
        /// ID printed when the job, operation or login attempt was started.
        id: String,
    },
    /// Retry an eligible failed item by ID.
    Retry {
        /// ID printed when the job, operation or login attempt was started.
        id: String,
        /// Reuse a completed file upload whose message submission never started.
        #[arg(long)]
        reuse_completed_upload: bool,
    },
}
#[derive(Subcommand)]
enum ContactCommand {
    #[command(flatten)]
    Catalog(CatalogCommand),
    /// Look up one or more known contact IDs on WeChat.
    Lookup {
        /// Contact IDs to look up on WeChat; give one or more.
        #[arg(required = true)]
        ids: Vec<String>,
    },
}

#[derive(Subcommand)]
enum CatalogCommand {
    /// Retrieve a saved avatar or request an avatar refresh.
    Avatar {
        /// ID of the contact whose avatar to retrieve.
        id: String,
        /// Save the avatar to this new file (required unless --refresh).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Ask WeChat for a fresh avatar instead of reading the saved one.
        #[arg(long)]
        refresh: bool,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30)]
        wait: u64,
    },
    /// List saved entries with optional pagination.
    List {
        /// Return at most this many items.
        #[arg(long)]
        limit: Option<usize>,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show a saved entry by ID.
    Get {
        /// ID of the contact or group, from the matching list command.
        id: String,
    },
    /// Find saved entries matching a name or ID.
    Find {
        /// Name or ID text to search for.
        query: String,
    },
    /// Refresh one entry from WeChat.
    Refresh {
        /// ID of the contact or group, from the matching list command.
        id: String,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// List, inspect or update a group's members.
    Members {
        #[command(subcommand)]
        command: MemberCommand,
    },
    /// Refresh the catalog from WeChat.
    Sync {
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
}
#[derive(Subcommand)]
enum ChatCommand {
    /// Pin an ordinary direct chat or group to the top of the thread list.
    Pin {
        /// Chat ID from wx chats list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Unpin an ordinary direct chat or group.
    Unpin {
        /// Chat ID from wx chats list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Mute a direct chat or group.
    Mute {
        /// Chat ID from wx chats list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Unmute a direct chat or group.
    Unmute {
        /// Chat ID from wx chats list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// List chats with optional pagination and kind filtering.
    List {
        /// Return at most this many items.
        #[arg(long)]
        limit: Option<usize>,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
        /// Only list chats of this kind: direct, group or system.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Show one chat and its current read-state version.
    Get {
        /// Chat ID from wx chats list.
        id: String,
    },
    /// Find chats matching a name or ID.
    Find {
        /// Name or ID text to search for.
        query: String,
        /// Only find chats of this kind: direct, group or system.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Mark the whole chat read using its current read-state version.
    Read {
        /// Chat ID from wx chats list.
        id: String,
        /// Read-state version from wx chats get; refused if the chat changed since.
        #[arg(long)]
        expected_version: String,
    },
    /// Retained for compatibility; opening a native GUI is unsupported.
    #[command(hide = true)]
    Open {
        /// Chat ID from wx chats list.
        id: String,
    },
}

#[derive(Args)]
struct MutationOptions {
    /// Reuse this key when retrying so the daemon applies the request only once.
    #[arg(long)]
    idempotency_key: Option<String>,
    /// Return as soon as the work is queued instead of waiting for it.
    #[arg(long)]
    detach: bool,
    /// Seconds to wait for completion; the work keeps running after the wait ends.
    #[arg(long, default_value_t = 30, conflicts_with = "detach")]
    wait: u64,
}
#[derive(Subcommand)]
enum GroupCommand {
    #[command(flatten)]
    Catalog(CatalogCommand),
    /// Fetch the member roster for a known group ID from WeChat.
    Lookup {
        /// Group ID, ending in @chatroom, to fetch the member roster for.
        room: String,
        /// Roster version to request from WeChat; the reply shows the current version.
        #[arg(long, default_value_t = 0)]
        version: u32,
    },
    /// Create a group with two to forty initial members.
    Create {
        /// Contact IDs of the initial members, from wx contacts list; two to forty.
        #[arg(required = true, num_args = 2..=40)]
        members: Vec<String>,
        /// Shared name for the new group.
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Change a group's shared name.
    Rename {
        /// Group ID from wx groups list.
        id: String,
        /// New shared name for the group.
        name: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Leave a group while retaining local history.
    Leave {
        /// Group ID from wx groups list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Change your display name in a group.
    MyName {
        /// Group ID from wx groups list.
        id: String,
        /// Your new display name in this group.
        display_name: String,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Clear a group's announcement when permitted.
    ClearAnnouncement {
        /// Group ID from wx groups list.
        id: String,
        #[command(flatten)]
        options: MutationOptions,
    },
}
#[derive(Subcommand)]
enum MemberCommand {
    /// List saved members of a group.
    List {
        /// Group ID from wx groups list.
        id: String,
        /// Return at most this many items.
        #[arg(long)]
        limit: Option<usize>,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one saved group member.
    Get {
        /// Group ID from wx groups list.
        id: String,
        /// Member's contact ID, from wx groups members list.
        username: String,
    },
    /// Refresh a group's member list from WeChat.
    Refresh {
        /// Group ID from wx groups list.
        id: String,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// Add one or more contacts to a group.
    Add {
        /// Group ID from wx groups list.
        id: String,
        /// Contact IDs to add; give one or more.
        #[arg(required = true)]
        members: Vec<String>,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Remove one or more members from a group.
    Remove {
        /// Group ID from wx groups list.
        id: String,
        /// Member IDs to remove; give one or more.
        #[arg(required = true)]
        members: Vec<String>,
        #[command(flatten)]
        options: MutationOptions,
    },
}
#[derive(Subcommand)]
enum EventCommand {
    /// Stream new events, optionally saving a resume cursor.
    Watch {
        /// Start after this event cursor, such as the last one you processed.
        #[arg(long, default_value = "0")]
        after: String,
        /// Save the last event cursor to this private file and resume from it next time.
        #[arg(long)]
        cursor_file: Option<PathBuf>,
    },
    /// Read events after a cursor.
    List {
        /// Start after this event cursor, such as the last one you processed.
        #[arg(long, default_value = "0")]
        after: String,
        /// Return at most this many events.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Save a consumer's progress through an event cursor.
    Ack {
        /// Name of the consumer whose progress is being saved.
        #[arg(long)]
        consumer: String,
        /// Cursor of the last event this consumer has finished handling.
        #[arg(long)]
        through: String,
    },
}
#[derive(Subcommand)]
enum AuthCommand {
    /// Show the selected account's WeChat login state.
    Status,
    /// Inspect or cancel a WeChat login attempt.
    Attempts {
        #[command(subcommand)]
        command: ResourceAction,
    },
    /// Sign in using saved device trust or a phone-approved QR code.
    Login {
        /// Use supported stopped Linux files in place; optional absolute path on the daemon host, not the CLI host. macOS/Windows profiles are unverified.
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        official: Option<String>,
        /// Reuse this key when retrying so the daemon applies the request only once.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Which sign-in method to use; auto tries saved device trust first, then a QR code.
        #[arg(long,value_parser=["auto","saved-device","qr"],default_value="auto")]
        strategy: String,
        /// Replace an expired, unscanned QR code automatically, or leave it expired.
        #[arg(long,value_parser=["auto","manual"],default_value="auto")]
        qr_refresh: String,
        /// Most times an expired QR code is replaced automatically (0 to 3).
        #[arg(long,default_value_t=3,value_parser=clap::value_parser!(u8).range(0..=3))]
        max_refreshes: u8,
        /// Skip saved device trust and sign in with a QR code.
        #[arg(long, alias = "qr")]
        force_qr: bool,
        /// Never show a QR code; sign in only with saved device trust.
        #[arg(long, alias = "saved-only", conflicts_with = "force_qr")]
        no_qr: bool,
        /// Seconds to allow for the login to finish (30 to 900).
        #[arg(long, alias = "wait", default_value_t = 300)]
        timeout: u64,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Save the login QR code PNG to this path; later refreshes add a -N suffix.
        #[arg(long, alias = "qr-out")]
        qr_output: Option<PathBuf>,
    },
    /// Show a login attempt by ID.
    Operation {
        /// Login attempt ID printed by wx auth login.
        id: String,
    },
    /// Cancel a login attempt by ID.
    Cancel {
        /// Login attempt ID printed by wx auth login.
        id: String,
    },
    /// Show or save the QR code for a login attempt.
    Qr {
        /// Login attempt ID printed by wx auth login.
        id: String,
        /// Save the QR code PNG to this path instead of the default folder.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Disconnect while retaining the saved WeChat login.
    Disconnect,
    /// Sign out locally while retaining saved credentials.
    #[command(hide = true)]
    LogoutLocal,
    /// Sign out of WeChat and disable automatic reconnection.
    Logout {
        /// Sign out on the daemon and keep saved credentials, without contacting WeChat.
        #[arg(long, conflicts_with_all = ["idempotency_key", "detach", "wait"])]
        local: bool,
        /// Reuse this key when retrying so the daemon applies the request only once.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30)]
        wait: u64,
    },
    /// Forget local authentication material while retaining history.
    Forget {
        /// Confirm discarding this account's saved login credentials; history is kept.
        #[arg(long, required = true)]
        yes: bool,
        /// Reuse this key when retrying so the daemon applies the request only once.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
#[derive(Subcommand)]
enum MessageCommand {
    /// Recall an own message using its opaque local ID and exact remote ID.
    Recall {
        /// Message ID from wx messages list --json (the id field).
        id: String,
        /// Exact server ID of the message, from wx messages list --json (serverId).
        #[arg(long)]
        server_id: String,
        /// Reuse this key when retrying so the daemon applies the request only once.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for completion; the work keeps running after the wait ends.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// Stream message events, optionally saving a resume cursor.
    Watch {
        /// Start after this event cursor, such as the last one you processed.
        #[arg(long, default_value = "0")]
        after: String,
        /// Save the last event cursor to this private file and resume from it next time.
        #[arg(long)]
        cursor_file: Option<PathBuf>,
    },
    /// Retrieve a message attachment or report its availability.
    Media {
        /// Chat ID the message belongs to, from wx chats list.
        thread: String,
        /// Message ID from wx messages list.
        id: String,
        /// Copy to retrieve: images have thumbnail, standard and original; videos normal and original; voice encoded. best picks the best available and full means original.
        #[arg(long,default_value="best",value_parser=["best","thumbnail","standard","original","full","normal","encoded"])]
        quality: String,
        /// Save the attachment to this new file; an existing file is not overwritten.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Use only a copy the daemon has already saved; never start a download.
        #[arg(long)]
        cached_only: bool,
        /// Return as soon as the work is queued instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Seconds to wait for the download; it keeps running after the wait ends.
        #[arg(long, default_value_t = 20, conflicts_with = "detach")]
        wait: u64,
    },
    /// Inspect or cancel sends, or find a send by its idempotency key.
    Sends {
        #[command(subcommand)]
        command: SendAction,
    },
    /// Inspect or cancel outgoing voice jobs.
    Voice {
        #[command(subcommand)]
        command: ResourceAction,
    },
    /// List messages in a chat.
    List {
        /// Chat ID from wx chats list.
        thread: String,
        /// Continue from the next-page cursor of the previous result.
        #[arg(long)]
        cursor: Option<String>,
        /// Return at most this many items.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show a message by local ID, optionally qualified by chat.
    Get {
        /// Message ID, or the chat ID when a message ID follows.
        thread_or_id: String,
        /// Message ID from wx messages list, checked to belong to the chat.
        id: Option<String>,
    },
    /// Send text, a URL share, an image, a file, a video or a voice note.
    Send(Box<MessageSendArgs>),
    /// Show an outgoing send by ID.
    SendStatus {
        /// Send ID printed by wx messages send.
        id: String,
    },
    /// Mark a whole chat read using its current read-state version.
    Read {
        /// Chat ID from wx chats list.
        thread: String,
        /// Read-state version from wx chats get; refused if the chat changed since.
        #[arg(long)]
        expected_version: String,
    },
    /// Fetch new messages for the selected account.
    Sync,
}
#[derive(Args)]
struct MessageSendArgs {
    /// Chat ID to send to, from wx chats list.
    thread: String,
    /// Send this URL as a shared link card; --title is required.
    #[arg(long, group = "payload", requires = "title")]
    share: Option<String>,
    /// Title shown on the shared link card.
    #[arg(long, requires = "share")]
    title: Option<String>,
    /// Description shown on the shared link card.
    #[arg(long, requires = "share")]
    description: Option<String>,
    /// Thumbnail image URL shown on the shared link card.
    #[arg(long, requires = "share")]
    thumbnail_url: Option<String>,
    /// Send this text message.
    #[arg(long, group = "payload")]
    text: Option<String>,
    /// Quote a stored message in this chat; also supply its exact server ID.
    #[arg(long, requires_all = ["text", "quote_server_id"], conflicts_with_all = ["share", "image", "file", "video", "voice"])]
    quote_message_id: Option<String>,
    /// Exact server ID of the quoted message; use it with --quote-message-id.
    #[arg(long, requires_all = ["text", "quote_message_id"], conflicts_with_all = ["share", "image", "file", "video", "voice"])]
    quote_server_id: Option<String>,
    /// Mention a group member by account ID; repeat for multiple members.
    #[arg(long, requires = "text", conflicts_with_all = ["mention_all", "share", "image", "file", "video", "voice"])]
    mention: Vec<String>,
    /// Mention all group members (WeChat enforces the account's permissions).
    #[arg(long, requires = "text", conflicts_with_all = ["mention", "share", "image", "file", "video", "voice"])]
    mention_all: bool,
    /// Send this image file.
    #[arg(long, group = "payload")]
    image: Option<PathBuf>,
    /// Send this file as an attachment.
    #[arg(long, group = "payload")]
    file: Option<PathBuf>,
    /// Send this MP4 video file.
    #[arg(long, group = "payload")]
    video: Option<PathBuf>,
    /// Prepare normal playback, or include the unchanged original video too.
    #[arg(long, requires = "video", value_parser = ["normal", "original"])]
    video_quality: Option<String>,
    /// Send this audio file as a voice message.
    #[arg(long, group = "payload")]
    voice: Option<PathBuf>,
    /// Reuse this key when retrying so the daemon applies the request only once.
    #[arg(long)]
    idempotency_key: Option<String>,
    /// Return as soon as the work is queued instead of waiting for it.
    #[arg(long)]
    detach: bool,
    /// Seconds to wait for the send to finish (up to 3600 for attachments, 900 otherwise).
    #[arg(long, default_value_t = 30, conflicts_with = "detach")]
    wait: u64,
}
#[derive(Subcommand)]
enum SendAction {
    /// Show an outgoing send by ID.
    Get {
        /// Send ID printed by wx messages send.
        id: String,
    },
    /// Request cancellation of an outgoing send.
    Cancel {
        /// Send ID printed by wx messages send.
        id: String,
    },
    /// Find an outgoing send by its original idempotency key.
    Lookup {
        /// Key that was given when the send was submitted.
        #[arg(long)]
        idempotency_key: String,
    },
}
#[derive(Subcommand)]
enum TokenCommand {
    /// List API tokens without revealing their secret values.
    List,
    /// Create an API token with account and permission scopes.
    Create {
        /// Label for the new token.
        name: String,
        /// Account ID the token may use, or * for all accounts; repeat for several.
        #[arg(long, required = true)]
        accounts: Vec<String>,
        /// Permission to grant: read, media, send, read-state, login, admin, calls or mini-programs; repeat.
        #[arg(long, required = true)]
        scopes: Vec<String>,
        /// Expire the token at this Unix time in seconds; it never expires if omitted.
        #[arg(long)]
        expires_at: Option<i64>,
    },
    /// Revoke an API token by ID.
    Revoke {
        /// Token ID from wx tokens list.
        id: String,
    },
}
#[derive(Subcommand)]
enum StoreCommand {
    /// List accounts in the local daemon store.
    List,
    /// Create an offline account slot.
    Create {
        /// ID for the new offline account slot.
        id: String,
    },
    /// Attach a supported stopped official profile in place.
    Attach {
        /// Stopped official profile home or account folder to use in place.
        #[arg(long)]
        path: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    /// Copy a supported stopped profile into the local store.
    Import {
        /// Stopped official profile home or account folder to copy into the store.
        #[arg(long)]
        source: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    /// Copy an owned stopped profile to a new destination.
    Export {
        /// New folder to copy the profile into; its parent must exist but it must not.
        #[arg(long)]
        destination: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        source_stopped: bool,
        /// Copy even when saved changes have not reached the official files; gaps are reported.
        #[arg(long)]
        allow_incomplete: bool,
    },
    /// Register a verified key file for the selected account.
    BindKeys {
        /// Private key file for this profile, such as one from wx profiles keys export.
        #[arg(long)]
        keys_file: PathBuf,
    },
    /// Derive profile keys from an authorized password file.
    DeriveKeys {
        /// Stopped account folder, the one containing db_storage, to derive keys for.
        #[arg(long)]
        profile: PathBuf,
        /// Private file holding the account password as exactly 32 raw bytes.
        #[arg(long)]
        password_file: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    /// Restore saved credentials from an authorized local file.
    RestoreCredentials {
        /// Private file with the saved login state to restore.
        #[arg(long)]
        file: PathBuf,
        /// Private file with the raw sync cursor to restore along with the login state.
        #[arg(long)]
        cursor_file: Option<PathBuf>,
    },
    /// Enable supported official-profile writes for a stopped client.
    EnableWrites {
        /// WeChat username of the account; it must match the logged-in account.
        #[arg(long)]
        own_username: String,
        /// Official profile's MMKV identity store file, inside the selected profile.
        #[arg(long)]
        mmkv_store: PathBuf,
        /// Private file holding the 16-byte MMKV format key.
        #[arg(long)]
        format_key_file: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        official_client_stopped: bool,
    },
    /// Bind an account to supported stopped profile identity files.
    BindIdentity {
        /// Official profile's MMKV identity store file, inside the selected profile.
        #[arg(long)]
        mmkv_store: PathBuf,
        /// Private file holding the 16-byte MMKV format key.
        #[arg(long)]
        format_key_file: PathBuf,
        /// Confirm the official client is stopped; wx cannot check this for you.
        #[arg(long, required = true)]
        official_client_stopped: bool,
    },
    /// Inspect the selected account's official database schemas.
    Schema,
    /// Replay saved local profile writes without sending or downloading.
    RecoverProjection,
}
fn private_input(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = wechat_platform::OpenOptions::new()
        .read(true)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(path)?;
    let m = f.metadata()?;
    if !m.is_file() || !wechat_platform::private_file(&f)? || m.len() > max {
        bail!("input must be an owner-only bounded regular file");
    }
    let mut bytes = vec![];
    f.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("input is too large");
    }
    Ok(bytes)
}
fn private_output(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = wechat_platform::private_tempfile_in(parent, ".wx-output-")?;
    temp.as_file().write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("output already exists or cannot be installed"))?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
async fn private_media_output(
    path: &Path,
    response: reqwest::Response,
    limit: usize,
    expected_bytes: Option<u64>,
    expected_sha256: Option<&str>,
) -> Result<()> {
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let expected = expected_bytes
        .filter(|n| *n > 0 && *n <= limit as u64)
        .context("invalid media size")?;
    let expected_sha256 = expected_sha256
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("invalid media digest")?;
    if response.content_length().is_some_and(|n| n != expected) {
        bail!("media integrity check failed");
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = wechat_platform::private_tempfile_in(parent, ".wx-media-output-")?;
    let mut stream = response.bytes_stream();
    let mut hash = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size
            .checked_add(chunk.len() as u64)
            .context("media size overflow")?;
        if size > expected {
            bail!("media integrity check failed");
        }
        hash.update(&chunk);
        temp.write_all(&chunk)?;
    }
    if size != expected || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected_sha256) {
        bail!("media integrity check failed");
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("output already exists or cannot be installed"))?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.len() == 2 && arguments[1] == "__mini-program-host-v1" {
        if miniprogram::machine_host().await.is_err() {
            println!(r#"{{"version":1,"state":"error","code":"mini_program_start_failed"}}"#);
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    let json = arguments.iter().any(|s| s == "--json");
    let cli = match cli_help::command()
        .try_get_matches_from(arguments)
        .and_then(|matches| Cli::from_arg_matches(&matches))
    {
        Ok(cli) => cli,
        Err(error) => {
            if error.exit_code() == 0 {
                let _ = error.print();
                return;
            }
            if json {
                eprintln!(
                    "{}",
                    json!({"error":{"code":"invalid_arguments","message":error.to_string(),"retryable":false}})
                );
            } else {
                let _ = error.print();
            }
            std::process::exit(2);
        }
    };
    if let Err(error) = run(cli).await {
        let (exit, code) = outcome::classify(&error);
        if json {
            eprintln!(
                "{}",
                json!({"error":{"code":code,"message":error.to_string(),"retryable":exit==3}})
            );
        } else {
            eprintln!("{error:#}");
        }
        std::process::exit(exit);
    }
    std::process::exit(outcome::exit());
}
async fn shutdown(core: Arc<Core>) {
    async fn requested(core: &Core) {
        while !core.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal registration");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{}, _=requested(&core)=>{} }
    }
    #[cfg(windows)]
    tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=requested(&core)=>{} }
    core.shutdown_gracefully().await;
}

struct Remote {
    http: reqwest::Client,
    base: reqwest::Url,
    token: String,
    account: Option<String>,
}
impl Remote {
    fn new(config: wechat_config::ClientConfig) -> Result<Self> {
        Self::with_token(config, None)
    }
    fn with_token(config: wechat_config::ClientConfig, token: Option<String>) -> Result<Self> {
        Self::with_connection(config, token, None, false)
    }
    fn with_connection(
        config: wechat_config::ClientConfig,
        token: Option<String>,
        certificate: Option<&str>,
        direct: bool,
    ) -> Result<Self> {
        let base = reqwest::Url::parse(&format!("{}/", config.endpoint.trim_end_matches('/')))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !matches!(base.scheme(), "http" | "https")
        {
            bail!("invalid daemon endpoint");
        }
        if base.scheme() == "http"
            && !base.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
        {
            bail!("remote daemon endpoints require HTTPS");
        }
        let mut http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(65));
        if direct {
            http = http.no_proxy();
        }
        if let Some(certificate) = certificate {
            http =
                http.add_root_certificate(reqwest::Certificate::from_pem(certificate.as_bytes())?);
        }
        Ok(Self {
            http: http.build()?,
            base,
            token: match token {
                Some(value) => {
                    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
                    {
                        return Err(failure(
                            4,
                            "invalid_api_token",
                            "API bearer token is invalid",
                        ));
                    }
                    value
                }
                None => wechat_api::read_token(&config.token_file).map_err(|_| {
                    failure(
                        4,
                        "invalid_api_token",
                        "API bearer token file is missing or invalid",
                    )
                })?,
            },
            account: config.account,
        })
    }
    fn account(&self) -> Result<&str> {
        self.account
            .as_deref()
            .context("select an account with --account or client.toml")
    }
    fn path(&self, suffix: &str) -> Result<String> {
        Ok(format!(
            "v1/accounts/{}/{}",
            encode(self.account()?),
            suffix
        ))
    }
    async fn response(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Result<reqwest::Response> {
        self.response_with_timeout(method, path, body, key, Duration::from_secs(65))
            .await
    }
    async fn response_with_timeout(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
        timeout: Duration,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .http
            .request(method, self.base.join(path)?)
            .bearer_auth(&self.token)
            .timeout(timeout);
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let code = response.status();
            let error: Value = response.json().await.unwrap_or(Value::Null);
            let error_code = error["error"]["code"].as_str().unwrap_or("request_failed");
            let exit = if code.as_u16() == 401 || code.as_u16() == 403 {
                4
            } else if matches!(
                error_code,
                "auth_required"
                    | "needs_login"
                    | "not_authenticated"
                    | "saved_device_unavailable"
                    | "upstream_unavailable"
            ) {
                5
            } else if code.is_client_error() {
                9
            } else {
                3
            };
            return Err(failure(
                exit,
                error_code,
                &format!("daemon request failed ({code}): {error_code}"),
            ));
        }
        Ok(response)
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Result<Value> {
        let response = self.response(method, path, body, key).await?;
        let bytes = bounded_bytes(response, 32 * 1024 * 1024).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None, None).await
    }
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, Some(body), None)
            .await
    }
    async fn upload(&self, bytes: Vec<u8>, name: &str) -> Result<Value> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name.to_string())
            .mime_str("application/octet-stream")?;
        let response = self
            .http
            .post(self.base.join(&self.path("uploads")?)?)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(3600))
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let value: Value = response.json().await.unwrap_or(Value::Null);
            bail!(
                "upload failed({status}): {}",
                value["error"]["code"].as_str().unwrap_or("request_failed")
            );
        }
        Ok(response.json().await?)
    }
}
fn encode(s: &str) -> String {
    reqwest::Url::parse("http://localhost/")
        .unwrap()
        .query_pairs_mut()
        .append_pair("", s)
        .finish()
        .query()
        .unwrap()
        .trim_start_matches('=')
        .replace('+', "%20")
}
fn query(path: String, values: &[(&str, Option<String>)]) -> String {
    let mut url = reqwest::Url::parse(&format!("http://localhost/{path}")).unwrap();
    for (key, value) in values {
        if let Some(value) = value {
            url.query_pairs_mut().append_pair(key, value);
        }
    }
    format!(
        "{}{}",
        url.path().trim_start_matches('/'),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    )
}
async fn run(cli: Cli) -> Result<()> {
    let output = Output::new(cli.json);
    if matches!(
        &cli.command,
        Command::Calls {
            command: Some(calls_cli::CallsCommand::Devices)
        }
    ) {
        return calls_cli::devices(cli.json).await;
    }
    if matches!(&cli.command, Command::Calls { command: None }) {
        let mut command = cli_help::command();
        command.build();
        command
            .find_subcommand_mut("calls")
            .context("missing calls command")?
            .print_help()?;
        println!();
        return Ok(());
    }
    if matches!(
        &cli.command,
        Command::Calls {
            command: Some(calls_cli::CallsCommand::Conference { command: None })
        }
    ) {
        let mut command = cli_help::command();
        command.build();
        command
            .find_subcommand_mut("calls")
            .and_then(|calls| calls.find_subcommand_mut("conference"))
            .context("missing conference command")?
            .print_help()?;
        println!();
        return Ok(());
    }
    if matches!(cli.command, Command::Licenses) {
        return output.print(
            &json!({"silkProject":include_str!("../../wechat-protocol/vendor/silk/LICENSE-MIT"),"silkSdk":include_str!("../../wechat-protocol/vendor/silk/LICENSE-SDK"),"protocol":include_str!("../../wechat-protocol/NOTICE"),"glassEasel":wechat_miniprogram::runner::GLASS_EASEL_LICENSE,"cssTree":wechat_miniprogram::runner::CSS_TREE_LICENSE}),
        );
    }
    if matches!(cli.command, Command::Version) {
        return output.print(&wechat_api::version());
    }
    if cli.token_stdin
        && matches!(
            &cli.command,
            Command::Daemon { .. }
                | Command::Store { .. }
                | Command::Config { .. }
                | Command::Profiles { .. }
                | Command::Version
                | Command::Licenses
        )
    {
        bail!("--token-stdin is supported by serve and HTTP client commands");
    }
    let env = wechat_config::Environment::capture();
    let cwd = std::env::current_dir()?;
    if matches!(
        &cli.command,
        Command::Serve { .. }
            | Command::Daemon { .. }
            | Command::Store { .. }
            | Command::Config {
                command: ConfigCommand::Show
            }
    ) {
        let config = wechat_config::resolve(
            &wechat_config::Overrides {
                server_config: cli.server_config.clone(),
                data_dir: cli.data_dir.clone(),
                token_file: cli.token_file.clone(),
                listen: cli.listen,
                network_listen: cli.network_listen,
                tls_cert_file: cli.tls_cert_file.clone(),
                tls_key_file: cli.tls_key_file.clone(),
            },
            &env,
            &cwd,
        )?;
        return match cli.command {
            Command::Serve {} => serve(config, cli.token_stdin, cli.json).await,
            Command::Daemon { command } => daemon::run(config, command, output).await,
            Command::Store { command } => {
                offline(config, cli.account.unwrap_or_default(), command, output)
            }
            Command::Config {
                command: ConfigCommand::Show,
            } => output.print(&config),
            _ => unreachable!(),
        };
    }
    let create_settings = matches!(
        &cli.command,
        Command::Profiles {
            command: profile_cli::ProfileCommand::Add { .. }
        } | Command::Config {
            command: ConfigCommand::Servers {
                command: ServerCommand::Add { .. }
            }
        }
    );
    let mut settings =
        settings::File::load(cli.client_config.as_deref(), &env, &cwd, create_settings)?;
    if let Command::Config {
        command: ConfigCommand::Servers { command },
    } = cli.command
    {
        match command {
            ServerCommand::Add {
                name,
                url,
                token_file,
            } => {
                settings::valid_name(&name)?;
                if settings.settings.servers.contains_key(&name) {
                    bail!("saved server already exists");
                }
                let endpoint = reqwest::Url::parse(&url)?;
                if !matches!(endpoint.scheme(), "http" | "https")
                    || endpoint.username() != ""
                    || endpoint.password().is_some()
                    || endpoint.query().is_some()
                    || endpoint.fragment().is_some()
                {
                    bail!("invalid server URL");
                }
                let token_file = if token_file.is_absolute() {
                    token_file
                } else {
                    cwd.join(token_file)
                };
                settings.settings.servers.insert(
                    name.clone(),
                    settings::Server {
                        url,
                        token_file,
                        default_account_id: None,
                        local_profile: None,
                    },
                );
                settings.save()?;
                return output.print(&json!({"server":name,"state":"registered"}));
            }
            ServerCommand::Select { name } => {
                if !settings.settings.servers.contains_key(&name) {
                    bail!("saved server not found");
                }
                settings.settings.active_server = Some(name.clone());
                settings.save()?;
                return output.print(&json!({"server":name}));
            }
            ServerCommand::Remove { name } => {
                if settings.settings.servers.remove(&name).is_none() {
                    bail!("saved server not found");
                }
                if settings.settings.active_server.as_deref() == Some(&name) {
                    settings.settings.active_server = None;
                }
                settings.save()?;
                return output.print(&json!({
                    "server": name,
                    "state": "removed",
                    "activeServer": settings.settings.active_server,
                }));
            }
            ServerCommand::List => {
                return output.print(
                    &json!({"items":settings.settings.servers,"activeServer":settings.settings.active_server}),
                )
            }
        }
    }
    if let Command::Profiles { command } = cli.command {
        let env_url = std::env::var("AGENT_WECHAT_URL").ok();
        let env_name = std::env::var("AGENT_WECHAT_SERVER").ok();
        let url = cli.url.as_deref().or_else(|| {
            if cli.server_name.is_none() {
                env_url.as_deref()
            } else {
                None
            }
        });
        let name = cli.server_name.as_deref().or_else(|| {
            if url.is_none() {
                env_name.as_deref()
            } else {
                None
            }
        });
        let server = settings.server_name(name, url)?;
        return profile_cli::run(
            settings,
            cli.local_profile.as_deref(),
            server.as_deref(),
            command,
            output,
        );
    }
    let config = wechat_config::resolve(
        &wechat_config::Overrides {
            server_config: cli.server_config.clone(),
            data_dir: cli.data_dir.clone(),
            token_file: cli.token_file.clone(),
            listen: cli.listen,
            network_listen: cli.network_listen,
            tls_cert_file: cli.tls_cert_file.clone(),
            tls_key_file: cli.tls_key_file.clone(),
        },
        &env,
        &cwd,
    )?;
    let (mut client, selected_server) =
        settings.client(cli.server_name.as_deref(), cli.url.as_deref(), &config)?;
    if let Some(url) = cli.url {
        client.endpoint = url;
    }
    if let Some(path) = &cli.token_file {
        client.token_file = path.clone();
    }
    if let Some(account) = &cli.account {
        client.account = Some(account.clone());
    }
    if matches!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Client
        }
    ) {
        return output.print(&client);
    }
    let env_token = if cli.token_stdin {
        Some(stdin_token().await?)
    } else if cli.token_file.is_none() {
        std::env::var("AGENT_WECHAT_TOKEN").ok()
    } else {
        None
    };
    let mut r = Remote::with_token(client, env_token)?;
    if let Some(path) = cli.ca_cert_file {
        let certificate = reqwest::Certificate::from_pem(&std::fs::read(path)?)
            .context("invalid additional CA certificate")?;
        r.http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(65))
            .add_root_certificate(certificate)
            .build()?;
    }
    if let Command::Auth {
        command: AuthCommand::Login {
            official: Some(path),
            ..
        },
    } = &cli.command
    {
        let selected = r.post("v1/profiles/official", json!({
            "path": if path.is_empty() { None } else { Some(path) },
            "accountId": cli.account,
        })).await.map_err(|error| {
            let message = format!("{error}; --official requires the official app to be closed and a supported Linux profile. Use --official=/absolute/path/to/profile on the daemon host. This uses files in place; it does not copy or convert macOS/Windows profiles.");
            error.context(message)
        })?;
        r.account = Some(
            selected["accountId"]
                .as_str()
                .context("invalid official profile selection")?
                .into(),
        );
        if !cli.json {
            eprintln!("Using official files in place (account {}).", r.account()?);
        }
    }
    let requires_account = !matches!(
        &cli.command,
        Command::Status
            | Command::Accounts {
                command: AccountCommand::List
                    | AccountCommand::Create { .. }
                    | AccountCommand::Select { .. }
                    | AccountCommand::Alias { .. }
                    | AccountCommand::Remove { .. }
                    | AccountCommand::Get { id: Some(_) }
            }
            | Command::Tokens { .. }
            | Command::Config { .. }
    );
    // Calls and Mini Programs can use a selected account without reading the
    // account catalog. The API resolves aliases and checks the relevant scope.
    let selected_account_without_catalog = matches!(
        &cli.command,
        Command::Calls { .. } | Command::MiniProgram { .. }
    ) && r.account.is_some();
    if requires_account && !selected_account_without_catalog {
        let catalog = r.get("v1/accounts").await?;
        let items = catalog["items"]
            .as_array()
            .context("invalid account catalog")?;
        let selected = if let Some(selected) = r.account.as_ref() {
            let matching = items
                .iter()
                .filter(|a| {
                    a["id"].as_str() == Some(selected)
                        || a["alias"].as_str() == Some(selected)
                        || a["aliases"]
                            .as_array()
                            .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(selected)))
                })
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                bail!("account_not_found: selected account or alias is unavailable");
            }
            matching[0]["id"]
                .as_str()
                .context("invalid account identity")?
                .to_string()
        } else {
            if items.is_empty() {
                bail!("account_not_configured: create an account slot first");
            }
            if items.len() != 1 {
                bail!("account_ambiguous: select --account or configure an endpoint default");
            }
            items[0]["id"]
                .as_str()
                .context("invalid account identity")?
                .to_string()
        };
        r.account = Some(selected);
    }
    let chat_list = matches!(
        &cli.command,
        Command::Threads { .. }
            | Command::Chats {
                command: ChatCommand::List { .. } | ChatCommand::Find { .. }
            }
    );
    use reqwest::Method;
    let result = match cli.command {
        Command::Status => r.get("v1/status").await?,
        Command::Calls { command } => {
            return calls_cli::run(&r, command.context("choose a calls command")?, cli.json).await;
        }
        Command::MiniProgram { command } => return miniprogram::run(r, command, cli.json).await,
        Command::LookupContacts { ids }
        | Command::Contacts {
            command: Some(ContactCommand::Lookup { ids }),
            ..
        } => {
            r.post(&r.path("contacts/lookup")?, json!({"ids":ids}))
                .await?
        }
        Command::LookupGroup { room, version } => {
            client_actions::group(&r, &settings, GroupCommand::Lookup { room, version }).await?
        }
        Command::Accounts { command } => match command {
            AccountCommand::List => r.get("v1/accounts").await?,
            AccountCommand::Create { id, alias } => {
                if id.is_none() && alias.is_none() {
                    bail!("choose an account ID or --alias");
                }
                let mut body = json!({});
                if let Some(id) = id {
                    body["id"] = json!(id);
                }
                if let Some(alias) = alias {
                    body["alias"] = json!(alias);
                }
                r.post("v1/accounts", body).await?
            }
            AccountCommand::Get { id } => {
                let path = if let Some(id) = id {
                    format!("v1/accounts/{}", encode(&id))
                } else {
                    r.path("")?.trim_end_matches('/').to_string()
                };
                r.get(&path).await?
            }
            AccountCommand::Alias { id, alias } => {
                r.request(
                    Method::PATCH,
                    &format!("v1/accounts/{}", encode(&id)),
                    Some(json!({"alias":alias})),
                    None,
                )
                .await?
            }
            AccountCommand::Remove { id, confirm } => {
                if id != confirm {
                    bail!("Account confirmation does not match");
                }
                r.request(
                    Method::DELETE,
                    &format!("v1/accounts/{}", encode(&id)),
                    Some(json!({"confirmAccountId":confirm})),
                    None,
                )
                .await?
            }
            AccountCommand::Select { id } => {
                let account = r.get(&format!("v1/accounts/{}", encode(&id))).await?;
                let id = account["id"].as_str().context("invalid account response")?;
                let server = selected_server
                    .context("save a server with wx config servers add, then run wx config servers select NAME before selecting its default account")?;
                settings
                    .settings
                    .servers
                    .get_mut(&server)
                    .context("saved server not found")?
                    .default_account_id = Some(id.into());
                settings.save()?;
                json!({"server":server,"accountId":id})
            }
            AccountCommand::Settings {
                auto_download_max_mb,
            } => {
                let path = r.path("settings")?;
                if let Some(mb) = auto_download_max_mb {
                    r.request(
                        Method::PATCH,
                        &path,
                        Some(json!({"autoDownloadMaxBytes":mb * 1024 * 1024})),
                        None,
                    )
                    .await?
                } else {
                    r.get(&path).await?
                }
            }
            AccountCommand::Status => r.get(r.path("")?.trim_end_matches('/')).await?,
            AccountCommand::Capabilities => r.get(&r.path("capabilities")?).await?,
            AccountCommand::Disconnect => r.post(&r.path("disconnect")?, json!({})).await?,
            AccountCommand::Connect { detach, wait } => {
                let op = r.post(&r.path("connect")?, json!({})).await?;
                wait_operation(&r, op, if detach { 0 } else { wait }).await?
            }
        },
        Command::Threads { cursor, limit } => {
            r.get(&query(
                r.path("threads")?,
                &[("cursor", cursor), ("limit", limit.map(|v| v.to_string()))],
            ))
            .await?
        }
        Command::Contacts {
            command: Some(ContactCommand::Catalog(command)),
            ..
        } => client_actions::catalog(&r, &settings, "contacts", command).await?,
        Command::Contacts {
            command: None,
            search,
            cursor,
        } => {
            r.get(&query(
                r.path("contacts")?,
                &[("cursor", cursor), ("search", search)],
            ))
            .await?
        }
        Command::Groups {
            command: Some(command),
            ..
        } => client_actions::group(&r, &settings, command).await?,
        Command::Groups {
            command: None,
            search,
            cursor,
        } => {
            r.get(&query(
                r.path("groups")?,
                &[("cursor", cursor), ("search", search)],
            ))
            .await?
        }
        Command::Auth { command } => match command {
            AuthCommand::Attempts { command } => {
                client_actions::resource(&r, "operations", command).await?
            }
            AuthCommand::Status => r.get(r.path("")?.trim_end_matches('/')).await?,
            AuthCommand::Login {
                official: _,
                idempotency_key,
                force_qr,
                no_qr,
                timeout,
                detach,
                qr_output,
                strategy,
                qr_refresh,
                max_refreshes,
            } => {
                login::login(
                    &r,
                    &settings,
                    login::Options {
                        idempotency_key,
                        force_qr,
                        no_qr,
                        timeout,
                        detach,
                        qr_output,
                        strategy,
                        qr_refresh,
                        max_refreshes,
                        json: cli.json,
                    },
                )
                .await?
            }
            AuthCommand::Operation { id } => {
                r.get(&r.path(&format!("operations/{}", encode(&id)))?)
                    .await?
            }
            AuthCommand::Cancel { id } => {
                r.request(
                    Method::DELETE,
                    &r.path(&format!("operations/{}", encode(&id)))?,
                    None,
                    None,
                )
                .await?
            }
            AuthCommand::Qr { id, output } => {
                login::qr(&r, &settings, &id, output, cli.json).await?
            }
            AuthCommand::Disconnect => r.post(&r.path("auth/disconnect")?, json!({})).await?,
            AuthCommand::LogoutLocal | AuthCommand::Logout { local: true, .. } => {
                r.post(&r.path("auth/logout-local")?, json!({})).await?
            }
            AuthCommand::Logout {
                local: false,
                idempotency_key,
                detach,
                wait,
            } => {
                let key = idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let op = r
                    .request(
                        Method::POST,
                        &r.path("logout")?,
                        Some(json!({})),
                        Some(&key),
                    )
                    .await?;
                wait_operation(&r, op, if detach { 0 } else { wait }).await?
            }
            AuthCommand::Forget {
                yes: _,
                idempotency_key,
            } => {
                r.request(
                    Method::POST,
                    &r.path("forget-credentials")?,
                    Some(json!({"confirm":true})),
                    Some(&idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
                )
                .await?
            }
        },
        Command::Sync { detach, wait } => {
            let op = r.post(&r.path("sync")?, json!({})).await?;
            wait_operation(&r, op, if detach { 0 } else { wait }).await?
        }
        Command::Chats { command } => match command {
            ChatCommand::Pin { id, options } => {
                client_actions::pin_thread(&r, &settings, &id, true, options).await?
            }
            ChatCommand::Unpin { id, options } => {
                client_actions::pin_thread(&r, &settings, &id, false, options).await?
            }
            ChatCommand::Mute { id, options } => {
                client_actions::mutation(
                    &r,
                    &settings,
                    reqwest::Method::PUT,
                    &format!("threads/{}/notifications", encode(&id)),
                    json!({"muted":true}),
                    options,
                )
                .await?
            }
            ChatCommand::Unmute { id, options } => {
                client_actions::mutation(
                    &r,
                    &settings,
                    reqwest::Method::PUT,
                    &format!("threads/{}/notifications", encode(&id)),
                    json!({"muted":false}),
                    options,
                )
                .await?
            }
            ChatCommand::List {
                limit,
                cursor,
                kind,
            } => {
                r.get(&query(
                    r.path("threads")?,
                    &[
                        ("limit", limit.map(|v| v.to_string())),
                        ("cursor", cursor),
                        ("kind", kind),
                    ],
                ))
                .await?
            }
            ChatCommand::Get { id } => r.get(&r.path(&format!("threads/{}", encode(&id)))?).await?,
            ChatCommand::Find {
                query: search,
                kind,
            } => {
                r.get(&query(
                    r.path("threads")?,
                    &[("query", Some(search)), ("kind", kind)],
                ))
                .await?
            }
            ChatCommand::Read {
                id,
                expected_version,
            } => {
                let body = json!({"expectedVersion":expected_version});
                r.post(
                    &r.path(&format!("threads/{}/read-state", encode(&id)))?,
                    body,
                )
                .await?
            }
            ChatCommand::Open { id: _ } => bail!(
                "unsupported_capability: native has no GUI; use chats get or explicit chats read"
            ),
        },
        Command::Jobs { command } => client_actions::jobs(&r, command).await?,
        Command::Operations { command } => {
            client_actions::resource(&r, "operations", command).await?
        }
        Command::Messages { command } => match command {
            MessageCommand::Recall {
                id,
                server_id,
                idempotency_key,
                detach,
                wait,
            } => {
                client_actions::recall(&r, &settings, id, server_id, idempotency_key, detach, wait)
                    .await?
            }
            MessageCommand::Watch { after, cursor_file } => {
                return events::watch(&r, after, cursor_file, true, cli.json).await
            }
            MessageCommand::Media {
                thread,
                id,
                quality,
                output,
                cached_only,
                detach,
                wait,
            } => {
                client_actions::media(
                    &r,
                    &thread,
                    &id,
                    &quality,
                    output,
                    cached_only,
                    detach,
                    wait,
                )
                .await?
            }
            MessageCommand::Voice { command } => {
                client_actions::resource(&r, "jobs", command).await?
            }
            MessageCommand::Sends { command } => match command {
                SendAction::Get { id } => {
                    r.get(&r.path(&format!("sends/{}", encode(&id)))?).await?
                }
                SendAction::Cancel { id } => {
                    r.post(
                        &r.path(&format!("sends/{}/cancel", encode(&id)))?,
                        json!({}),
                    )
                    .await?
                }
                SendAction::Lookup { idempotency_key } => {
                    r.get(&query(
                        r.path("sends/by-idempotency-key")?,
                        &[("key", Some(idempotency_key))],
                    ))
                    .await?
                }
            },
            MessageCommand::List {
                thread,
                cursor,
                limit,
            } => {
                let page = r
                    .get(&query(
                        r.path(&format!("threads/{}/messages", encode(&thread)))?,
                        &[("cursor", cursor), ("limit", limit.map(|v| v.to_string()))],
                    ))
                    .await?;
                return output.messages(&page);
            }
            MessageCommand::Get { thread_or_id, id } => {
                let message = r
                    .get(&r.path(&format!(
                        "messages/{}",
                        encode(id.as_deref().unwrap_or(&thread_or_id))
                    ))?)
                    .await?;
                if id.is_some() && message["threadId"] != thread_or_id {
                    bail!("message does not belong to selected thread");
                }
                message
            }
            MessageCommand::Send(args) => {
                let MessageSendArgs {
                    thread,
                    share,
                    title,
                    description,
                    thumbnail_url,
                    text,
                    quote_message_id,
                    quote_server_id,
                    mention,
                    mention_all,
                    image,
                    file,
                    video,
                    video_quality,
                    voice,
                    detach,
                    wait,
                    idempotency_key,
                } = *args;
                client_actions::send(
                    &r,
                    &settings,
                    thread,
                    share,
                    title,
                    description,
                    thumbnail_url,
                    text,
                    quote_message_id,
                    quote_server_id,
                    mention,
                    mention_all,
                    image,
                    file,
                    video,
                    video_quality,
                    voice,
                    idempotency_key,
                    detach,
                    wait,
                )
                .await?
            }
            MessageCommand::SendStatus { id } => {
                r.get(&r.path(&format!("sends/{}", encode(&id)))?).await?
            }
            MessageCommand::Read {
                thread,
                expected_version,
            } => {
                r.post(
                    &r.path(&format!("threads/{}/read-state", encode(&thread)))?,
                    json!({"expectedVersion":expected_version}),
                )
                .await?
            }
            MessageCommand::Sync => r.post(&r.path("sync")?, json!({})).await?,
        },
        Command::Tokens { command } => match command {
            TokenCommand::List => r.get("v1/tokens").await?,
            TokenCommand::Create {
                name,
                accounts,
                scopes,
                expires_at,
            } => {
                r.post(
                    "v1/tokens",
                    json!({"name":name,"accounts":accounts,"scopes":scopes,"expiresAt":expires_at}),
                )
                .await?
            }
            TokenCommand::Revoke { id } => {
                r.request(
                    Method::DELETE,
                    &format!("v1/tokens/{}", encode(&id)),
                    None,
                    None,
                )
                .await?
            }
        },
        Command::Events {
            command: Some(command),
            ..
        } => match command {
            EventCommand::Watch { after, cursor_file } => {
                return events::watch(&r, after, cursor_file, false, cli.json).await
            }
            EventCommand::List { after, limit } => {
                r.get(&query(
                    r.path("events")?,
                    &[("after", Some(after)), ("limit", Some(limit.to_string()))],
                ))
                .await?
            }
            EventCommand::Ack { consumer, through } => {
                r.post(
                    &r.path(&format!("consumers/{}/ack", encode(&consumer)))?,
                    json!({"cursor":through}),
                )
                .await?
            }
        },
        Command::Events {
            command: None,
            after,
            watch,
        } => {
            if watch {
                return events::watch(&r, after, None, false, cli.json).await;
            }
            r.get(&query(r.path("events")?, &[("after", Some(after))]))
                .await?
        }
        Command::Version
        | Command::Licenses
        | Command::Config { .. }
        | Command::Serve { .. }
        | Command::Store { .. }
        | Command::Profiles { .. }
        | Command::Daemon { .. } => {
            unreachable!()
        }
    };
    if chat_list {
        output.chats(&result)
    } else {
        output.print(&result)
    }
}
async fn stdin_token() -> Result<String> {
    use std::io::IsTerminal;
    use tokio::io::AsyncReadExt;
    if std::io::stdin().is_terminal() {
        bail!("--token-stdin requires a private pipe");
    }
    let mut bytes = Vec::new();
    let mut input = tokio::io::stdin().take(1027);
    tokio::time::timeout(Duration::from_secs(5), input.read_to_end(&mut bytes))
        .await
        .map_err(|_| failure(2, "invalid_api_token", "API token pipe timed out"))?
        .context("cannot read API token pipe")?;
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid API token pipe"))?;
    let token = text.trim();
    if text.len() > 1026
        || !(32..=1024).contains(&token.len())
        || !token.bytes().all(|b| b.is_ascii_graphic())
    {
        bail!("invalid API token pipe");
    }
    Ok(token.into())
}
async fn serve(config: wechat_config::ServerConfig, token_stdin: bool, json: bool) -> Result<()> {
    let supplied = if token_stdin {
        if config.token_explicit {
            bail!("--token-stdin cannot be combined with a configured tokenFile");
        }
        Some(stdin_token().await?)
    } else {
        None
    };
    let store = Arc::new(wechat_store::Store::open(&config.data_dir)?);
    let token = match supplied {
        Some(token) => token,
        None => wechat_api::provision_token(&config.token_file, !config.token_explicit)?,
    };
    let core = Core::new(store.clone(), config.storage_key_file.as_deref())?;
    if let Some(path) = &config.profile_format_key_file {
        core.configure_profile_format_key(&private_input(path, 16)?)?;
    }
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let network = if let Some(address) = config.network_listen {
        let certificate = private_input(config.tls_cert_file.as_deref().unwrap(), 64 * 1024)?;
        let key = private_input(config.tls_key_file.as_deref().unwrap(), 64 * 1024)?;
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(certificate, key).await?;
        let socket = std::net::TcpListener::bind(address)?;
        socket.set_nonblocking(true)?;
        Some((socket, tls))
    } else {
        None
    };
    core.start_workers()?;
    let listen = listener.local_addr()?;
    if json {
        eprintln!(
            "{}",
            json!({"state":"ready","listen":listen.to_string(),"serverId":store.server_id,"nativeLogin":true})
        );
    } else {
        eprintln!("Daemon ready on {listen} (server ID {}).", store.server_id);
    }
    let router = wechat_api::router(core.clone(), &token);
    let local =
        axum::serve(listener, router.clone()).with_graceful_shutdown(shutdown(core.clone()));
    if let Some((socket, tls)) = network {
        let handle = axum_server::Handle::new();
        let stop_handle = handle.clone();
        let stopping = core.clone();
        let watcher = tokio::spawn(async move {
            while !stopping.stopping.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            stop_handle.graceful_shutdown(Some(Duration::from_secs(30)));
        });
        let result = tokio::try_join!(
            async { local.await },
            axum_server::tls_rustls::from_tcp_rustls(socket, tls)?
                .handle(handle)
                .serve(router.into_make_service())
        );
        watcher.abort();
        core.shutdown();
        result?;
    } else {
        local.await?;
    }
    Ok(())
}
async fn wait_operation(r: &Remote, op: Value, wait: u64) -> Result<Value> {
    wait_operation_checked(r, op, wait, |_| Ok(())).await
}
async fn wait_operation_checked(
    r: &Remote,
    mut op: Value,
    wait: u64,
    validate: impl Fn(&Value) -> Result<()>,
) -> Result<Value> {
    validate(&op)?;
    if wait == 0 {
        return Ok(op);
    }
    if wait > 900 {
        bail!("wait must be at most900seconds");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(wait);
    while matches!(
        op["state"].as_str(),
        Some(
            "queued"
                | "running"
                | "starting"
                | "submitting"
                | "resuming"
                | "awaiting_scan"
                | "awaiting_approval"
                | "authenticating"
        )
    ) {
        if std::time::Instant::now() >= deadline {
            op["clientWaitExpired"] = json!(true);
            return Ok(op);
        }
        let id = op["id"]
            .as_str()
            .context("operation identity missing")?
            .to_string();
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let next = tokio::select! {_=tokio::signal::ctrl_c()=>{set_exit(130);op["clientInterrupted"]=json!(true);return Ok(op);},next=tokio::time::timeout(remaining,async{tokio::time::sleep(Duration::from_millis(250)).await;r.get(&r.path(&format!("operations/{}",encode(&id)))?).await})=>next};
        match next {
            Ok(result) => {
                op = result.with_context(|| {
                    format!("operation {id} remains owned by the daemon; inspect its status")
                })?;
                validate(&op)?;
            }
            Err(_) => {
                op["clientWaitExpired"] = json!(true);
                return Ok(op);
            }
        }
    }
    Ok(op)
}

fn offline(
    config: wechat_config::ServerConfig,
    account: String,
    command: StoreCommand,
    output: Output,
) -> Result<()> {
    match &command {
        StoreCommand::Attach { path, .. } => {
            wechat_store::validate_source_placement(&config.data_dir, path)?
        }
        StoreCommand::Import { source, .. } => {
            wechat_store::validate_source_placement(&config.data_dir, source)?
        }
        _ => {}
    }
    if account.is_empty() && !matches!(&command, StoreCommand::List | StoreCommand::Create { .. }) {
        bail!("--account is required");
    }
    let initialize = matches!(
        &command,
        StoreCommand::Attach { .. } | StoreCommand::Import { .. } | StoreCommand::Create { .. }
    );
    let store = Arc::new(if initialize {
        wechat_store::Store::open(&config.data_dir)?
    } else {
        wechat_store::Store::open_existing(&config.data_dir)?
    });
    let account = if !account.is_empty() && !initialize {
        store.resolve_account(&account)?
    } else {
        account
    };
    match command {
        StoreCommand::List => output.print(&store.accounts()?),
        StoreCommand::Create { id } => output.print(&store.create_account(&id)?),
        StoreCommand::Attach {
            path,
            source_stopped,
        } => output.print(&store.attach(&account, &path, source_stopped)?),
        StoreCommand::Import {
            source,
            source_stopped,
        } => output.print(&store.import(&account, &source, source_stopped)?),
        StoreCommand::Export {
            destination,
            source_stopped,
            allow_incomplete,
        } => {
            let projection = profile_export::ProjectionState::read(&store, &account)?;
            projection.require_copy_permission(allow_incomplete)?;
            let manifest = store.export(&account, &destination, source_stopped)?;
            output.print(&projection.receipt(manifest.entries.len())?)
        }
        StoreCommand::BindKeys { keys_file } => {
            wechat_profile::KeyFile::load(&keys_file)?;
            store.set_keys_file(&account, &keys_file)?;
            output.print(&json!({"state":"bound"}))
        }
        StoreCommand::DeriveKeys {
            profile,
            password_file,
            source_stopped: _,
        } => {
            let input = private_input(&password_file, 32)?;
            let password: [u8; 32] = input
                .try_into()
                .map_err(|_| anyhow::anyhow!("password file must contain exactly 32 raw bytes"))?;
            let _lease = wechat_store::profile_lease(&profile)?;
            let location = wechat_profile::ProfileLocation::new(profile)?;
            let keys = wechat_profile::keys_from_password(
                location,
                &password,
                wechat_profile::ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )?;
            let path = config
                .data_dir
                .join("accounts")
                .join(&account)
                .join("agent/profile-keys.json");
            keys.write_new(&path)?;
            store.set_keys_file(&account, &path)?;
            output.print(&json!({"state":"derived","keysFile":path}))
        }
        StoreCommand::RestoreCredentials { file, cursor_file } => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            core.restore_credentials(&account, &private_input(&file, 1024 * 1024)?)?;
            if let Some(cursor_file) = cursor_file {
                core.restore_cursor(&account, &private_input(&cursor_file, 65536)?)?;
            }
            output.print(
                &json!({"state":"restored","authentication":"saved_unvalidated","connection":"disconnected"}),
            )
        }
        StoreCommand::EnableWrites {
            own_username,
            mmkv_store,
            format_key_file,
            official_client_stopped: _,
        } => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            core.enable_profile_writes(
                &account,
                &own_username,
                &mmkv_store,
                &private_input(&format_key_file, 16)?,
            )?;
            output.print(&json!({"state":"enabled","coverage":"verified_existing_schema_families"}))
        }
        StoreCommand::BindIdentity {
            mmkv_store,
            format_key_file,
            official_client_stopped: _,
        } => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            core.bind_profile_identity(
                &account,
                &mmkv_store,
                &private_input(&format_key_file, 16)?,
            )?;
            output.print(&json!({"state":"identity_bound","officialWritesEnabled":false}))
        }
        StoreCommand::RecoverProjection => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            output.print(&core.recover_projection(&account)?)
        }
        StoreCommand::Schema => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            output.print(&core.profile_schema(&account)?)
        }
    }
}

async fn bounded_bytes(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        bail!("response exceeds bounded size");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            bail!("response exceeds bounded size");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
