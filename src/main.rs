mod backend;
mod capture;
mod config;
mod error;
mod input;
mod process;
mod server;
mod tools;
mod ui_tree;

use clap::Parser;
use config::Config;
use rmcp::ServiceExt;
use server::RustSightServer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();

    // MCP uses stdout for JSON-RPC messages; all logs MUST go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let capture = capture::create_capture_backend();
    capture.init()?;
    let input = input::create_input_backend();
    let ui_tree = ui_tree::create_ui_tree_backend();

    let server = RustSightServer::new(config, capture, input, ui_tree);
    let shutdown_handle = server.clone();

    info!("Starting rustsight MCP server on stdio");

    let transport = rmcp::transport::stdio();
    let running = server.serve(transport).await.map_err(|e| {
        error!("Failed to start MCP server: {e}");
        e
    })?;

    tokio::select! {
        res = running.waiting() => {
            if let Err(e) = res {
                error!("MCP server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl-C, shutting down");
        }
    }

    shutdown_handle.shutdown();
    info!("rustsight exited cleanly");
    Ok(())
}
