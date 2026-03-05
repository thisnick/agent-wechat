#![allow(dead_code)]

mod context;
mod db;
mod effects;
mod execution;
mod ia;
mod tools;
mod plans;
mod router;
mod sessions;

use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let port: u16 = std::env::var("AGENT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6174);
    let host = std::env::var("AGENT_HOST").unwrap_or_else(|_| "0.0.0.0".into());

    // Log environment
    tracing::info!("Environment:");
    tracing::info!("  DISPLAY: {:?}", std::env::var("DISPLAY").ok());
    tracing::info!(
        "  DBUS_SESSION_BUS_ADDRESS: {:?}",
        std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()
    );

    // Initialize auth token (panics if no token found)
    router::auth::init_token();
    tracing::info!("Auth token loaded");

    // Initialize database
    tracing::info!("Initializing database...");
    db::init_db().expect("Failed to initialize database");

    // Initialize sessions
    tracing::info!("Initializing sessions...");
    sessions::manager::initialize_sessions()
        .await
        .expect("Failed to initialize sessions");

    // Start background health monitor
    sessions::health_monitor::spawn_health_monitor();
    tracing::info!("WeChat health monitor started");

    // Build router
    let app = router::build_router();

    let addr: SocketAddr = format!("{host}:{port}").parse().expect("Invalid address");
    tracing::info!("agent-server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl+c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutting down...");
}
