use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use wechat_core::Core;

#[derive(Parser)]
#[command(
    name = "wx",
    version,
    about = "Standalone native WeChat daemon and authenticated client"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    server_config: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    #[arg(long, global = true)]
    listen: Option<SocketAddr>,
    #[arg(long, global = true)]
    client_config: Option<PathBuf>,
    #[arg(long, global = true)]
    url: Option<String>,
    #[arg(long, global = true)]
    account: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Version,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the daemon in the foreground. Use the supplied service unit for background ownership.
    Serve,
    Status,
    LookupContacts {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    LookupGroup {
        room: String,
        #[arg(long, default_value_t = 0)]
        version: u32,
    },
    Accounts {
        #[command(subcommand)]
        command: AccountCommand,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    Threads {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    Contacts {
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
    },
    Groups {
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
    },
    Messages {
        #[command(subcommand)]
        command: MessageCommand,
    },
    Events {
        #[arg(long, default_value = "0")]
        after: String,
        #[arg(long)]
        watch: bool,
    },
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
    Show,
    Client,
}
#[derive(Subcommand)]
enum AccountCommand {
    List,
    Create { id: String },
    Get,
}
#[derive(Subcommand)]
enum AuthCommand {
    Login {
        #[arg(long)]
        force_qr: bool,
        #[arg(long)]
        no_qr: bool,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        qr_output: Option<PathBuf>,
    },
    Operation {
        id: String,
    },
    Cancel {
        id: String,
    },
    Qr {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
    Disconnect,
    LogoutLocal,
}
#[derive(Subcommand)]
enum MessageCommand {
    List {
        thread: String,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    Get {
        id: String,
    },
    Send {
        thread: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    SendStatus {
        id: String,
    },
    Read {
        thread: String,
        #[arg(long, required = true)]
        message_id: Vec<String>,
        #[arg(long)]
        expected_version: String,
    },
    Sync,
}
#[derive(Subcommand)]
enum TokenCommand {
    List,
    Create {
        name: String,
        #[arg(long, required = true)]
        accounts: Vec<String>,
        #[arg(long, required = true)]
        scopes: Vec<String>,
        #[arg(long)]
        expires_at: Option<i64>,
    },
    Revoke {
        id: String,
    },
}
#[derive(Subcommand)]
enum StoreCommand {
    List,
    Create {
        id: String,
    },
    Attach {
        #[arg(long)]
        path: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    Import {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    Export {
        #[arg(long)]
        destination: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
        #[arg(long)]
        allow_incomplete: bool,
    },
    BindKeys {
        #[arg(long)]
        keys_file: PathBuf,
    },
    DeriveKeys {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        password_file: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    RestoreCredentials {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        cursor_file: Option<PathBuf>,
    },
    EnableWrites {
        #[arg(long)]
        own_username: String,
        #[arg(long)]
        mmkv_store: PathBuf,
        #[arg(long)]
        format_key_file: PathBuf,
        #[arg(long, required = true)]
        official_client_stopped: bool,
    },
    Schema,
    RecoverProjection,
}
fn output(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn private_input(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let m = f.metadata()?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.len() > max
    {
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
    let temp = tempfile::Builder::new()
        .prefix(".wx-output-")
        .tempfile_in(parent)?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    temp.as_file().write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("output already exists or cannot be installed"))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli).await {
        if json {
            eprintln!(
                "{}",
                json!({"error":{"code":"operation_failed","message":error.to_string()}})
            );
        } else {
            eprintln!("{error:#}");
        }
        std::process::exit(1);
    }
}
async fn shutdown(core: Arc<Core>) {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal registration");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    core.shutdown();
}
struct Remote {
    http: reqwest::Client,
    base: reqwest::Url,
    token: String,
    account: Option<String>,
}
impl Remote {
    fn new(config: wechat_config::ClientConfig) -> Result<Self> {
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
        Ok(Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(65))
                .build()?,
            base,
            token: wechat_api::read_token(&config.token_file)?,
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
        let mut request = self
            .http
            .request(method, self.base.join(path)?)
            .bearer_auth(&self.token);
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
            bail!(
                "daemon request failed ({code}): {}",
                error["error"]["code"].as_str().unwrap_or("request_failed")
            );
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
        let bytes = response.bytes().await?;
        if bytes.len() > 32 * 1024 * 1024 {
            bail!("response exceeds limit");
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None, None).await
    }
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, Some(body), None)
            .await
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
    if matches!(cli.command, Command::Version) {
        return output(&wechat_api::version());
    }
    let env = wechat_config::Environment::capture();
    let cwd = std::env::current_dir()?;
    let config = wechat_config::resolve(
        &wechat_config::Overrides {
            server_config: cli.server_config.clone(),
            data_dir: cli.data_dir.clone(),
            token_file: cli.token_file.clone(),
            listen: cli.listen,
        },
        &env,
        &cwd,
    )?;
    if matches!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Show
        }
    ) {
        return output(&config);
    }
    if matches!(cli.command, Command::Serve) {
        let store = Arc::new(wechat_store::Store::open(&config.data_dir)?);
        let token = wechat_api::provision_token(&config.token_file, !config.token_explicit)?;
        let core = Core::new(store.clone(), config.storage_key_file.as_deref())?;
        let listener = tokio::net::TcpListener::bind(config.listen).await?;
        core.start_workers()?;
        eprintln!(
            "{}",
            json!({"state":"ready","listen":listener.local_addr()?.to_string(),"serverId":store.server_id,"nativeLogin":true})
        );
        axum::serve(listener, wechat_api::router(core.clone(), &token))
            .with_graceful_shutdown(shutdown(core))
            .await?;
        return Ok(());
    }
    if let Command::Store { command } = cli.command {
        return offline(config, cli.account.unwrap_or_default(), command);
    }
    let mut client =
        wechat_config::resolve_client(cli.client_config.as_deref(), &env, &cwd, &config)?;
    if let Some(url) = cli.url {
        client.endpoint = url;
    }
    if let Some(path) = cli.token_file {
        client.token_file = path;
    }
    if let Some(account) = cli.account {
        client.account = Some(account);
    }
    if matches!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Client
        }
    ) {
        return output(&client);
    }
    let r = Remote::new(client)?;
    use reqwest::Method;
    let result = match cli.command {
        Command::Status => r.get("v1/status").await?,
        Command::LookupContacts { ids } => {
            r.post(&r.path("contacts/lookup")?, json!({"ids":ids}))
                .await?
        }
        Command::LookupGroup { room, version } => {
            r.get(&format!(
                "{}?version={version}",
                r.path(&format!("groups/{}/lookup", encode(&room)))?
            ))
            .await?
        }
        Command::Accounts { command } => match command {
            AccountCommand::List => r.get("v1/accounts").await?,
            AccountCommand::Create { id } => r.post("v1/accounts", json!({"id":id})).await?,
            AccountCommand::Get => r.get(r.path("")?.trim_end_matches('/')).await?,
        },
        Command::Threads { cursor, limit } => {
            r.get(&query(
                r.path("threads")?,
                &[("cursor", cursor), ("limit", limit.map(|v| v.to_string()))],
            ))
            .await?
        }
        Command::Contacts { search, cursor } => {
            r.get(&query(
                r.path("contacts")?,
                &[("cursor", cursor), ("search", search)],
            ))
            .await?
        }
        Command::Groups { search, cursor } => {
            r.get(&query(
                r.path("groups")?,
                &[("cursor", cursor), ("search", search)],
            ))
            .await?
        }
        Command::Auth { command } => match command {
            AuthCommand::Login {
                force_qr,
                no_qr,
                timeout,
                detach,
                qr_output,
            } => {
                let mut op = r
                    .post(
                        &r.path("auth/login")?,
                        json!({"forceQr":force_qr,"allowQr":!no_qr,"timeoutSeconds":timeout}),
                    )
                    .await?;
                if detach {
                    op
                } else {
                    let id = op["id"]
                        .as_str()
                        .context("invalid login operation")?
                        .to_owned();
                    let start = std::time::Instant::now();
                    let mut shown = 0;
                    while !matches!(
                        op["state"].as_str(),
                        Some(
                            "succeeded"
                                | "failed"
                                | "cancelled"
                                | "expired"
                                | "rejected"
                                | "needs_login"
                        )
                    ) {
                        if start.elapsed() > Duration::from_secs(timeout + 35) {
                            bail!("login wait ended; inspect operation {id}");
                        }
                        let generation = op["result"]["qrGeneration"].as_u64().unwrap_or(0);
                        if op["result"]["qrAvailable"] == true && generation > shown {
                            if let Some(path) = &qr_output {
                                let path = if generation == 1 {
                                    path.clone()
                                } else {
                                    path.with_file_name(format!(
                                        "{}-{generation}.png",
                                        path.file_stem().unwrap_or_default().to_string_lossy()
                                    ))
                                };
                                let response = r
                                    .response(
                                        Method::GET,
                                        &r.path(&format!("operations/{}/qr", encode(&id)))?,
                                        None,
                                        None,
                                    )
                                    .await?;
                                private_output(&path, &response.bytes().await?)?;
                                eprintln!("QR available: {}", path.display());
                            } else {
                                eprintln!("QR available: wx --account {} auth qr {} --output /absolute/path/login.png",r.account()?,id);
                            }
                            shown = generation;
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        op = r
                            .get(&r.path(&format!("operations/{}", encode(&id)))?)
                            .await?;
                    }
                    op
                }
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
                let bytes = r
                    .response(
                        Method::GET,
                        &r.path(&format!("operations/{}/qr", encode(&id)))?,
                        None,
                        None,
                    )
                    .await?
                    .bytes()
                    .await?;
                private_output(&output, &bytes)?;
                json!({"path":std::fs::canonicalize(output)?})
            }
            AuthCommand::Disconnect => r.post(&r.path("auth/disconnect")?, json!({})).await?,
            AuthCommand::LogoutLocal => r.post(&r.path("auth/logout-local")?, json!({})).await?,
        },
        Command::Messages { command } => match command {
            MessageCommand::List {
                thread,
                cursor,
                limit,
            } => {
                r.get(&query(
                    r.path(&format!("threads/{}/messages", encode(&thread)))?,
                    &[("cursor", cursor), ("limit", limit.map(|v| v.to_string()))],
                ))
                .await?
            }
            MessageCommand::Get { id } => {
                r.get(&r.path(&format!("messages/{}", encode(&id)))?)
                    .await?
            }
            MessageCommand::Send {
                thread,
                text,
                idempotency_key,
            } => {
                let key = idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                r.request(
                    Method::POST,
                    &r.path("sends")?,
                    Some(json!({"threadId":thread,"text":text})),
                    Some(&key),
                )
                .await?
            }
            MessageCommand::SendStatus { id } => {
                r.get(&r.path(&format!("sends/{}", encode(&id)))?).await?
            }
            MessageCommand::Read {
                thread,
                message_id,
                expected_version,
            } => {
                r.post(
                    &r.path(&format!("threads/{}/read-state", encode(&thread)))?,
                    json!({"messageIds":message_id,"expectedVersion":expected_version}),
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
        Command::Events { after, watch } => {
            if watch {
                use futures_util::StreamExt;
                let path = query(r.path("events/stream")?, &[("after", Some(after))]);
                let mut stream = r
                    .response(Method::GET, &path, None, None)
                    .await?
                    .bytes_stream();
                use std::io::Write;
                while let Some(chunk) = stream.next().await {
                    std::io::stdout().write_all(&chunk?)?;
                    std::io::stdout().flush()?;
                }
                return Ok(());
            }
            r.get(&query(r.path("events")?, &[("after", Some(after))]))
                .await?
        }
        Command::Version | Command::Config { .. } | Command::Serve | Command::Store { .. } => {
            unreachable!()
        }
    };
    output(&result)
}
fn offline(
    config: wechat_config::ServerConfig,
    account: String,
    command: StoreCommand,
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
    match command {
        StoreCommand::List => output(&store.accounts()?),
        StoreCommand::Create { id } => output(&store.create_account(&id)?),
        StoreCommand::Attach {
            path,
            source_stopped,
        } => output(&store.attach(&account, &path, source_stopped)?),
        StoreCommand::Import {
            source,
            source_stopped,
        } => output(&store.import(&account, &source, source_stopped)?),
        StoreCommand::Export {
            destination,
            source_stopped,
            allow_incomplete,
        } => {
            let pending = store.pending_sync_count(&account)?;
            let pending_reads = store.pending_reads(&account)?;
            if (pending > 0 || pending_reads > 0) && !allow_incomplete {
                bail!("unprojected native sync remains; --allow-incomplete exports only current official files");
            }
            let manifest = store.export(&account, &destination, source_stopped)?;
            output(
                &json!({"state":"copied","entries":manifest.entries.len(),"pendingProjectionBatches":pending,"pendingReadProjections":pending_reads,"nativeCredentialsIncluded":false,"officialTakeover":"unverified","syncCheckpointProjected":false}),
            )
        }
        StoreCommand::BindKeys { keys_file } => {
            wechat_profile::KeyFile::load(&keys_file)?;
            store.set_keys_file(&account, &keys_file)?;
            output(&json!({"state":"bound"}))
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
            output(&json!({"state":"derived","keysFile":path}))
        }
        StoreCommand::RestoreCredentials { file, cursor_file } => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            core.restore_credentials(&account, &private_input(&file, 1024 * 1024)?)?;
            if let Some(cursor_file) = cursor_file {
                core.restore_cursor(&account, &private_input(&cursor_file, 65536)?)?;
            }
            output(
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
            output(&json!({"state":"enabled","coverage":"verified_existing_schema_families"}))
        }
        StoreCommand::RecoverProjection => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            output(&core.recover_projection(&account)?)
        }
        StoreCommand::Schema => {
            let core = Core::new(store, config.storage_key_file.as_deref())?;
            output(&core.profile_schema(&account)?)
        }
    }
}
