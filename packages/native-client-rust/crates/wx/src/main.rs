use anyhow::Result;
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    name = "wx",
    version,
    about = "Native client foundation: server configuration and official-format profile ownership"
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
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Version,
    /// Resolve server configuration without creating files or contacting a server.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the authenticated foundation API; native login/sync is not implemented yet.
    Serve,
    /// Offline profile registration and same-format copy; local filesystem paths only.
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    Show,
}
#[derive(Subcommand)]
enum StoreCommand {
    List,
    Attach {
        #[arg(long)]
        account: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    Import {
        #[arg(long)]
        account: String,
        #[arg(long)]
        source: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
    Export {
        #[arg(long)]
        account: String,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
}
fn output(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
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
                serde_json::json!({"error":{"code":"operation_failed","message":error.to_string()}})
            );
        } else {
            eprintln!("{error:#}");
        }
        std::process::exit(1);
    }
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal registration");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
async fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Version) {
        return output(&wechat_api::version());
    }
    let config = wechat_config::resolve(
        &wechat_config::Overrides {
            server_config: cli.server_config,
            data_dir: cli.data_dir,
            token_file: cli.token_file,
            listen: cli.listen,
        },
        &wechat_config::Environment::capture(),
        &std::env::current_dir()?,
    )?;
    if matches!(cli.command, Command::Config { .. }) {
        return output(&config);
    }
    match &cli.command {
        Command::Store {
            command: StoreCommand::Attach { path, .. },
        } => wechat_store::validate_source_placement(&config.data_dir, path)?,
        Command::Store {
            command: StoreCommand::Import { source, .. },
        } => wechat_store::validate_source_placement(&config.data_dir, source)?,
        _ => {}
    }
    let initialize = matches!(
        &cli.command,
        Command::Serve
            | Command::Store {
                command: StoreCommand::Attach { .. } | StoreCommand::Import { .. }
            }
    );
    let store = Arc::new(if initialize {
        wechat_store::Store::open(&config.data_dir)?
    } else {
        wechat_store::Store::open_existing(&config.data_dir)?
    });
    match cli.command {
        Command::Serve => {
            let token = wechat_api::provision_token(&config.token_file, !config.token_explicit)?;
            let _profiles = store.profile_leases()?;
            let listener = tokio::net::TcpListener::bind(config.listen).await?;
            eprintln!(
                "{}",
                serde_json::json!({"state":"ready","listen":listener.local_addr()?.to_string(),"serverId":store.server_id,"nativeLogin":false})
            );
            axum::serve(listener, wechat_api::router(store, &token))
                .with_graceful_shutdown(shutdown())
                .await?;
            Ok(())
        }
        Command::Store { command } => match command {
            StoreCommand::List => output(&store.accounts()?),
            StoreCommand::Attach {
                account,
                path,
                source_stopped,
            } => output(&store.attach(&account, &path, source_stopped)?),
            StoreCommand::Import {
                account,
                source,
                source_stopped,
            } => output(&store.import(&account, &source, source_stopped)?),
            StoreCommand::Export {
                account,
                destination,
                source_stopped,
            } => {
                let manifest = store.export(&account, &destination, source_stopped)?;
                output(
                    &serde_json::json!({"state":"copied","entries":manifest.entries.len(),"schemaCompatibility":"unverified"}),
                )
            }
        },
        Command::Version | Command::Config { .. } => unreachable!(),
    }
}
