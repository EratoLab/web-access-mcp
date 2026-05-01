//! Browser-use MCP Server
//!
//! This binary provides a Model Context Protocol (MCP) server for browser automation.
//! It exposes browser automation tools that can be used by AI assistants and other MCP clients.

use browser_use::browser::LaunchOptions;
use browser_use::mcp::BrowserServer;
use clap::{Parser, ValueEnum};
use log::{debug, info};
use rmcp::{ServiceExt, transport::stdio};
use std::io::{stdin, stdout};

#[cfg(feature = "mcp-server")]
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::{LocalSessionManager, SessionConfig},
};

#[cfg(feature = "mcp-server")]
use std::time::Duration;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Transport {
    /// Standard input/output transport (default)
    Stdio,
    /// HTTP streamable transport
    Http,
}

#[derive(Parser)]
#[command(name = "browser-use")]
#[command(version)]
#[command(about = "Browser automation MCP server", long_about = None)]
struct Cli {
    /// Launch browser in headed mode (default: headless)
    #[arg(long, short = 'H')]
    headed: bool,

    /// Path to custom browser executable
    #[arg(long, value_name = "PATH")]
    executable_path: Option<String>,

    /// CDP endpoint URL for remote browser connection
    #[arg(long, value_name = "URL")]
    cdp_endpoint: Option<String>,

    /// WebSocket endpoint URL for remote browser connection
    #[arg(long, value_name = "URL")]
    ws_endpoint: Option<String>,

    /// Persistent browser profile directory
    #[arg(long, value_name = "DIR")]
    user_data_dir: Option<String>,

    /// Transport type to use
    #[arg(long, short = 't', value_enum, default_value = "stdio")]
    transport: Transport,

    /// Port for HTTP transport (default: 3000)
    #[arg(long, short = 'p', default_value = "3000")]
    port: u16,

    /// Host address to bind to (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP streamable endpoint path (default: /mcp)
    #[arg(long, default_value = "/mcp")]
    http_path: String,

    /// Allowed Host header for HTTP transport. Repeat to allow multiple hosts.
    #[arg(long = "allowed-host", value_name = "HOST")]
    allowed_hosts: Vec<String>,

    /// Skip Host header verification for HTTP transport.
    #[arg(long)]
    skip_host_header_verification: bool,

    /// Session inactivity timeout in seconds for HTTP streamable transport (default: 1800). Set to 0 to disable automatic cleanup.
    #[arg(long, default_value = "1800")]
    session_inactivity_timeout_secs: u64,

    /// Log file path for stdio mode (default: browser-use-mcp.log)
    #[arg(long, default_value = "browser-use-mcp.log")]
    log_file: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Configure browser launch options
    let options = LaunchOptions {
        headless: !cli.headed,
        chrome_path: cli.executable_path.clone().map(Into::into),
        user_data_dir: cli.user_data_dir.clone().map(Into::into),
        ..Default::default()
    };

    info!("Browser-use MCP Server v{}", env!("CARGO_PKG_VERSION"));
    info!(
        "Browser mode: {}",
        if options.headless {
            "headless"
        } else {
            "headed"
        }
    );

    if let Some(ref path) = cli.executable_path {
        info!("Browser executable: {}", path);
    }

    if let Some(ref endpoint) = cli.cdp_endpoint {
        info!("CDP endpoint: {}", endpoint);
    }

    if let Some(ref endpoint) = cli.ws_endpoint {
        info!("WebSocket endpoint: {}", endpoint);
    }

    if let Some(ref dir) = cli.user_data_dir {
        info!("User data directory: {}", dir);
    }

    // Route to appropriate transport
    match cli.transport {
        Transport::Stdio => {
            info!("Transport: stdio");
            info!("Ready to accept MCP connections via stdio");
            let (_read, _write) = (stdin(), stdout());
            let service = BrowserServer::with_options(options.clone())
                .map_err(|e| format!("Failed to create browser server: {}", e))?;
            let server = service.serve(stdio()).await?;

            // Set up signal handler for graceful shutdown
            #[cfg(unix)]
            {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                let mut sigint =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

                tokio::select! {
                    quit_reason = server.waiting() => {
                        debug!("Server quit with reason: {:?}", quit_reason);
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, shutting down gracefully...");
                    }
                    _ = sigint.recv() => {
                        info!("Received SIGINT (Ctrl+C), shutting down gracefully...");
                    }
                }
            }

            #[cfg(windows)]
            {
                let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
                let mut ctrl_break = tokio::signal::windows::ctrl_break()?;

                tokio::select! {
                    quit_reason = server.waiting() => {
                        debug!("Server quit with reason: {:?}", quit_reason);
                    }
                    _ = ctrl_c.recv() => {
                        info!("Received Ctrl+C, shutting down gracefully...");
                    }
                    _ = ctrl_break.recv() => {
                        info!("Received Ctrl+Break, shutting down gracefully...");
                    }
                }
            }

            #[cfg(not(any(unix, windows)))]
            {
                let quit_reason = server.waiting().await;
                debug!("Server quit with reason: {:?}", quit_reason);
            }
        }
        Transport::Http => {
            info!("Transport: HTTP streamable");
            info!("Host: {}", cli.host);
            info!("Port: {}", cli.port);
            info!("HTTP path: {}", cli.http_path);
            info!(
                "Host header verification: {}",
                if cli.skip_host_header_verification {
                    "disabled".to_string()
                } else if cli.allowed_hosts.is_empty() {
                    "default".to_string()
                } else {
                    format!("allowed hosts: {}", cli.allowed_hosts.join(", "))
                }
            );
            info!(
                "Session inactivity timeout: {}",
                if cli.session_inactivity_timeout_secs == 0 {
                    "disabled".to_string()
                } else {
                    format!("{}s", cli.session_inactivity_timeout_secs)
                }
            );

            let bind_addr = format!("{}:{}", cli.host, cli.port);

            // Create service factory closure
            let service_factory = move || {
                BrowserServer::with_options(options.clone())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            };

            let mut session_manager = LocalSessionManager::default();
            let mut session_config = SessionConfig::default();
            session_config.keep_alive = if cli.session_inactivity_timeout_secs == 0 {
                None
            } else {
                Some(Duration::from_secs(cli.session_inactivity_timeout_secs))
            };
            session_manager.session_config = session_config;

            let http_config = if cli.skip_host_header_verification {
                StreamableHttpServerConfig::default().disable_allowed_hosts()
            } else if cli.allowed_hosts.is_empty() {
                StreamableHttpServerConfig::default()
            } else {
                StreamableHttpServerConfig::default().with_allowed_hosts(cli.allowed_hosts)
            };

            let http_service = StreamableHttpService::new(
                service_factory,
                session_manager.into(),
                http_config,
            );

            let router = axum::Router::new().nest_service(&cli.http_path, http_service);

            info!(
                "Ready to accept MCP connections at http://{}{}",
                bind_addr, cli.http_path
            );

            let listener = tokio::net::TcpListener::bind(bind_addr).await?;
            axum::serve(listener, router).await?;
        }
    }

    Ok(())
}
