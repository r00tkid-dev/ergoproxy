use clap::Parser;

/// Main command enum for ErgoProxy CLI
#[derive(Parser, Debug)]
pub enum Command {
    /// Start the proxy server
    Start(StartArgs),
    
    /// Stop the proxy server
    Stop,
    
    /// Show proxy status
    Status,
    
    /// Manage security features
    Security(SecurityArgs),
    
    /// Show or modify configuration
    Config(ConfigArgs),
}

/// Arguments for the start command
#[derive(Parser, Debug)]
pub struct StartArgs {
    /// Host address to bind to
    #[clap(long, default_value = "127.0.0.1")]
    pub host: String,
    
    /// Port to listen on
    #[clap(long, default_value = "8080")]
    pub port: u16,
    
    /// Maximum number of connections in the pool
    #[clap(long, default_value = "50")]
    pub pool_size: usize,
    
    /// Enable HTTP/2 support
    #[clap(long)]
    pub http2: bool,
    
    /// Enable WebSocket support
    #[clap(long)]
    pub websocket: bool,
    
    /// IP rotation interval in seconds
    #[clap(long)]
    pub ip_rotation: Option<u64>,
    
    /// Enable verbose logging
    #[clap(long)]
    pub verbose: bool,
}

/// Arguments for security-related commands
#[derive(Parser, Debug)]
pub struct SecurityArgs {
    #[clap(subcommand)]
    pub command: SecurityCommand,
}

/// Security management commands
#[derive(Parser, Debug)]
pub enum SecurityCommand {
    /// IP rotation management
    Ip {
        #[clap(subcommand)]
        action: IpAction,
    },
    /// User-Agent rotation management
    Ua {
        #[clap(subcommand)]
        action: UaAction,
    },
}

/// IP management actions
#[derive(Parser, Debug)]
pub enum IpAction {
    /// Import IP list from file
    Import {
        /// Path to file containing IPs
        path: String,
    },
    /// Add single IP
    Add {
        /// IP address to add
        ip: String,
    },
    /// Show IP rotation status
    Status,
}

/// User-Agent management actions
#[derive(Parser, Debug)]
pub enum UaAction {
    /// Import User-Agent list from file
    Import {
        /// Path to file containing User-Agents
        path: String,
    },
    /// Add single User-Agent
    Add {
        /// User-Agent string to add
        ua: String,
    },
    /// Show User-Agent rotation status
    Status,
}

/// Configuration related arguments
#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[clap(subcommand)]
    pub action: ConfigAction,
}

/// Configuration actions
#[derive(Parser, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Set configuration value
    Set {
        /// Configuration key
        key: String,
        /// Configuration value
        value: String,
    },
}

// Module to contain error types if needed
pub mod error {
    use thiserror::Error;

    #[derive(Error, Debug)]
    pub enum CliError {
        #[error("Invalid configuration value")]
        InvalidConfig,
        #[error("Invalid IP address format")]
        InvalidIp,
        #[error("File not found: {0}")]
        FileNotFound(String),
    }
}

// Command validation helpers
impl StartArgs {
    pub fn validate(&self) -> Result<(), error::CliError> {
        // Validate host/port combination
        if self.port < 1024 && !cfg!(debug_assertions) {
            // In release mode, require root for privileged ports
            // This is just an example validation
            return Err(error::CliError::InvalidConfig);
        }
        Ok(())
    }
}

impl IpAction {
    pub fn validate(&self) -> Result<(), error::CliError> {
        match self {
            IpAction::Import { path } => {
                if !std::path::Path::new(path).exists() {
                    return Err(error::CliError::FileNotFound(path.clone()));
                }
            }
            IpAction::Add { ip } => {
                // Basic IP validation
                if ip.split('.').count() != 4 {
                    return Err(error::CliError::InvalidIp);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_start_args_validation() {
        let args = StartArgs {
            host: "127.0.0.1".to_string(),
            port: 8080,
            pool_size: 50,
            http2: false,
            websocket: false,
            ip_rotation: None,
            verbose: false,
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_ip_action_validation() {
        let action = IpAction::Add {
            ip: "192.168.1.1".to_string(),
        };
        assert!(action.validate().is_ok());

        let action = IpAction::Add {
            ip: "invalid.ip".to_string(),
        };
        assert!(action.validate().is_err());
    }
}
