use std::net::SocketAddr;
use anyhow::Result;
use clap::Parser;
use tracing::{info, error, warn};  
use tracing_subscriber::fmt::format::FmtSpan;

mod core;
mod security;
mod cli;

use core::pool::{ConnectionPool, PoolConfig};
use core::proxy::ErgoProxy;
use security::manager::SecurityManager;
use cli::commands::Command;

#[derive(Parser, Debug)]
#[clap(name = "ergoproxy", version)]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

/// Initialize logging based on verbosity
fn setup_logging(verbose: bool) {
    let builder = tracing_subscriber::fmt()
        .with_span_events(FmtSpan::CLOSE)
        .with_target(false);

    if verbose {
        builder
            .with_max_level(tracing::Level::DEBUG)
            .init();
    } else {
        builder
            .with_max_level(tracing::Level::INFO)
            .init();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start(args) => {
            // Setup logging
            setup_logging(args.verbose);
            
            info!("Initializing ErgoProxy...");
            
            // Create pool configuration
            let pool_config = PoolConfig {
                max_size: args.pool_size,
                ..Default::default()
            };
            
            // Initialize connection pool
            let pool = ConnectionPool::new(pool_config);
            
            // Initialize security manager
            let security = SecurityManager::new(
                args.ip_rotation.map(std::time::Duration::from_secs)
            );
            
            // Create proxy instance
            let proxy = ErgoProxy::new(pool, security)
                .with_http2(args.http2)
                .with_websocket(args.websocket);
            
            // Parse bind address
            let addr: SocketAddr = format!("{}:{}", args.host, args.port)
                .parse()
                .expect("Invalid address");
            
            info!("Starting proxy server on {}", addr);
            
            // Start the proxy
            if let Err(e) = proxy.start(addr).await {
                error!("Failed to start proxy: {}", e);
                std::process::exit(1);
            }
        }
        
        Command::Stop => {
            info!("Stopping proxy server...");
            warn!("Stop command not yet implemented");
        }
        
        Command::Status => {
            info!("Checking proxy status...");
            warn!("Status command not yet implemented");
        }
        
        Command::Security(args) => {
            match args.command {
                cli::commands::SecurityCommand::Ip { action } => {
                    match action {
                        cli::commands::IpAction::Import { path } => {
                            info!("Importing IPs from {}", path);
                        }
                        cli::commands::IpAction::Add { ip } => {
                            info!("Adding IP: {}", ip);
                        }
                        cli::commands::IpAction::Status => {
                            info!("Checking IP rotation status");
                        }
                    }
                }
                cli::commands::SecurityCommand::Ua { action } => {
                    match action {
                        cli::commands::UaAction::Import { path } => {
                            info!("Importing UAs from {}", path);
                        }
                        cli::commands::UaAction::Add { ua } => {
                            info!("Adding UA: {}", ua);
                        }
                        cli::commands::UaAction::Status => {
                            info!("Checking UA rotation status");
                        }
                    }
                }
            }
        }
        
        Command::Config(args) => {
            match args.action {
                cli::commands::ConfigAction::Show => {
                    info!("Showing current configuration");
                }
                cli::commands::ConfigAction::Set { key, value } => {
                    info!("Setting config {}={}", key, value);
                }
            }
        }
    }

    Ok(())
}
