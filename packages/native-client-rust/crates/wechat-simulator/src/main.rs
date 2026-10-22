use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use wechat_core::Core;
use wechat_simulator::{adapter::SimulatorUpstream, engine::Engine, service, workspace};

#[derive(Parser)]
#[command(
    name = "wx-dev",
    about = "Local synthetic upstream and real hub development environment"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Start the simulator and real hub. No production WeChat connection is made.
    Dev {
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        keep_state: bool,
        #[arg(long)]
        reset: bool,
        #[arg(long, default_value = "basic")]
        scenario: String,
    },
    /// Run only the local scenario service, for regression/process tests.
    Upstream {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: SocketAddr,
    },
    /// Run the actual hub with the explicit local simulator adapter.
    Hub {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        upstream: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: SocketAddr,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Dev {
            data_dir,
            keep_state,
            reset,
            scenario,
        } => dev(data_dir, keep_state, reset, &scenario).await,
        Commands::Upstream { data_dir, listen } => upstream(&data_dir, listen).await,
        Commands::Hub {
            data_dir,
            upstream,
            listen,
        } => hub(&data_dir, upstream, listen).await,
    }
}
fn loopback(address: SocketAddr) -> Result<()> {
    ensure!(address.ip().is_loopback(), "simulator_requires_loopback");
    Ok(())
}
async fn upstream(root: &Path, listen: SocketAddr) -> Result<()> {
    loopback(listen)?;
    let root = workspace::prepare(root)?;
    let _lease = workspace::lease(&root, "upstream")?;
    let engine = Arc::new(Engine::open(&root.join("upstream.sqlite"))?);
    let token = workspace::token(&root, "simulator-token")?;
    let (router, stopping) = service::router(engine, &token)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!(
        "{}",
        json!({"state":"ready","upstream":listener.local_addr()?,"controlTokenFile":root.join("simulator-token")})
    );
    axum::serve(listener,router).with_graceful_shutdown(async move {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>{},
            _=async {while !stopping.load(Ordering::Acquire) {tokio::time::sleep(Duration::from_millis(50)).await;}}=>{},
        }
    }).await?;
    Ok(())
}
async fn hub(root: &Path, upstream: SocketAddr, listen: SocketAddr) -> Result<()> {
    loopback(listen)?;
    loopback(upstream)?;
    let root = workspace::prepare(root)?;
    let _lease = workspace::lease(&root, "hub")?;
    let hub = workspace::hub_directory(&root)?;
    let token = workspace::token(&root, "hub-token")?;
    let upstream = Arc::new(SimulatorUpstream::new(
        upstream,
        workspace::token(&root, "simulator-token")?,
    )?);
    let store = Arc::new(wechat_store::Store::open(&hub)?);
    let core = Core::with_upstream(store, None, upstream)?;
    core.configure_profile_format_key(&[9; 16])?;
    core.start_workers()?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!(
        "{}",
        json!({"state":"ready","endpoint":format!("http://{}",listener.local_addr()?),"tokenFile":root.join("hub-token"),"serverId":core.store.server_id,"pid":std::process::id(),"ownershipGeneration":core.ownership_generation})
    );
    let stopping = core.clone();
    axum::serve(listener,wechat_api::router(core,&token)).with_graceful_shutdown(async move {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>{stopping.shutdown();},
            _=async {while !stopping.stopping.load(Ordering::Acquire) {tokio::time::sleep(Duration::from_millis(50)).await;}}=>{},
        }
    }).await?;
    Ok(())
}
struct Process {
    child: Child,
    ready: Value,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
async fn start(args: &[&std::ffi::OsStr]) -> Result<Process> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .context("simulator_child_stdout_unavailable")?;
    let mut lines = BufReader::new(stdout).lines();
    let ready: Result<Value> = async {
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await??
            .context("simulator_child_stopped_before_ready")?;
        let ready: Value = serde_json::from_str(&line)?;
        ensure!(ready["state"] == "ready", "simulator_child_not_ready");
        let endpoint = ready["endpoint"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                ready["upstream"]
                    .as_str()
                    .map(|address| format!("http://{address}"))
            })
            .context("simulator_child_endpoint_unavailable")?;
        let response = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?
            .get(format!("{endpoint}/health"))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        ensure!(response["status"] == "ok", "simulator_child_health_failed");
        Ok(ready)
    }
    .await;
    match ready {
        Ok(ready) => Ok(Process { child, ready }),
        Err(error) => {
            let _ = child.kill().await;
            Err(error)
        }
    }
}
async fn stop(process: &mut Process, url: &str, token: &str, path: &str) -> Result<()> {
    if process.child.try_wait()?.is_some() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?;
    let _ = client
        .post(format!("{url}{path}"))
        .bearer_auth(token)
        .json(&process.ready)
        .send()
        .await;
    if tokio::time::timeout(Duration::from_secs(5), process.child.wait())
        .await
        .is_err()
    {
        process.child.kill().await?;
    }
    Ok(())
}
async fn dev(data_dir: Option<PathBuf>, keep: bool, reset: bool, scenario: &str) -> Result<()> {
    ensure!(scenario == "basic", "unknown_simulator_scenario");
    let scratch = if data_dir.is_none() {
        Some(wechat_platform::private_tempdir("agent-wechat-dev-")?)
    } else {
        None
    };
    let root = workspace::prepare(
        data_dir
            .as_deref()
            .or_else(|| scratch.as_ref().map(|t| t.path()))
            .context("simulator_data_directory_required")?,
    )?;
    let _lease = workspace::lease(&root, "launcher")?;
    if reset {
        workspace::reset(&root)?;
    }
    let control_token = workspace::token(&root, "simulator-token")?;
    let hub_token = workspace::token(&root, "hub-token")?;
    let mut remote = start(&["upstream".as_ref(), "--data-dir".as_ref(), root.as_os_str()]).await?;
    let address = remote.ready["upstream"]
        .as_str()
        .context("simulator_address_unavailable")?
        .to_owned();
    let local = start(&[
        "hub".as_ref(),
        "--data-dir".as_ref(),
        root.as_os_str(),
        "--upstream".as_ref(),
        address.as_ref(),
    ])
    .await;
    let mut local = match local {
        Ok(local) => local,
        Err(error) => {
            let _ = stop(
                &mut remote,
                &format!("http://{address}"),
                &control_token,
                "/shutdown",
            )
            .await;
            return Err(error);
        }
    };
    let endpoint = local.ready["endpoint"]
        .as_str()
        .context("hub_endpoint_unavailable")?
        .to_owned();
    println!(
        "{}",
        json!({"state":"ready","scenario":scenario,"dataDir":root,"endpoint":endpoint,"tokenFile":root.join("hub-token"),"upstream":address,"controlTokenFile":root.join("simulator-token")})
    );
    eprintln!("Local simulator and Rust hub ready. Use the existing SDK/API with the token files above. Press Ctrl+C to stop.");
    if keep {
        if let Some(scratch) = scratch {
            scratch.keep();
        }
    } else if let Some(scratch) = scratch {
        return run_until_stop(
            &mut local,
            &mut remote,
            &endpoint,
            &address,
            &hub_token,
            &control_token,
            Some(scratch),
        )
        .await;
    }
    run_until_stop(
        &mut local,
        &mut remote,
        &endpoint,
        &address,
        &hub_token,
        &control_token,
        None,
    )
    .await
}
async fn run_until_stop(
    local: &mut Process,
    remote: &mut Process,
    endpoint: &str,
    address: &str,
    hub_token: &str,
    control_token: &str,
    _scratch: Option<wechat_platform::PrivateTempDir>,
) -> Result<()> {
    let outcome = tokio::select! {
        result=tokio::signal::ctrl_c()=>result.map_err(Into::into),
        status=local.child.wait()=>Err(anyhow::anyhow!("hub_child_stopped: {:?}",status)),
        status=remote.child.wait()=>Err(anyhow::anyhow!("simulator_child_stopped: {:?}",status)),
    };
    let local_stopped = stop(local, endpoint, hub_token, "/v1/daemon/shutdown").await;
    let remote_stopped = stop(
        remote,
        &format!("http://{address}"),
        control_token,
        "/shutdown",
    )
    .await;
    outcome.and(local_stopped).and(remote_stopped)
}
