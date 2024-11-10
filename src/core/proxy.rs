use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use bytes::BytesMut;
use anyhow::Result;
use thiserror::Error;
use tracing::{debug, error, info, warn, instrument};

use super::pool::{ConnectionPool, PoolError};
use crate::security::manager::SecurityManager;

#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Failed to bind to address: {0}")]
    BindError(#[from] std::io::Error),
    
    #[error("Connection pool error: {0}")]
    PoolError(#[from] PoolError),
    
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    
    #[error("Security error: {0}")]
    SecurityError(String),

    #[error("HTTP error: {0}")]
    HttpError(String),
}

/// Protocol types supported by the proxy
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Http1,
    Http2,
    WebSocket,
    Unknown,
}

/// Connection context containing metadata about the connection
#[derive(Debug)]
pub struct ConnectionContext {
    pub protocol: Protocol,
    pub client_addr: SocketAddr,
    pub target_addr: SocketAddr,
    pub secure: bool,
}

/// Main proxy structure
pub struct ErgoProxy {
    pool: Arc<ConnectionPool>,
    security: SecurityManager,
    http2_enabled: bool,
    websocket_enabled: bool,
}

impl From<anyhow::Error> for ProxyError {
    fn from(error: anyhow::Error) -> Self {
        ProxyError::ProtocolError(error.to_string())
    }
}

impl ErgoProxy {
    pub fn new(pool: ConnectionPool, security: SecurityManager) -> Self {
        Self {
            pool: Arc::new(pool),
            security,
            http2_enabled: false,
            websocket_enabled: false,
        }
    }

    pub fn with_http2(mut self, enabled: bool) -> Self {
        self.http2_enabled = enabled;
        self
    }

    pub fn with_websocket(mut self, enabled: bool) -> Self {
        self.websocket_enabled = enabled;
        self
    }

    #[instrument(skip(self))]
    pub async fn start(&self, bind_addr: SocketAddr) -> Result<(), ProxyError> {
        info!("Starting proxy server on {}", bind_addr);
        
        // Start the connection pool's health checks
        self.pool.clone().start_health_checks().await;
        
        // Create TCP listener
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| ProxyError::BindError(e))?;
        
        info!("Proxy server started successfully");
        
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let pool = Arc::clone(&self.pool);
                    let security = self.security.clone();
                    
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(pool, security, stream, addr).await {
                            error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    #[instrument(skip(pool, security, stream))]
    async fn handle_connection<T>(
        pool: Arc<ConnectionPool>,
        security: SecurityManager,
        mut stream: T,
        client_addr: SocketAddr,
    ) -> Result<(), ProxyError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        // Initialize buffer for reading
        let mut buf = BytesMut::with_capacity(4096);
        
        // Detect protocol
        let protocol = Self::detect_protocol(&mut stream, &mut buf).await?;
        debug!("Detected protocol: {:?}", protocol);
        
        // Create connection context
        let mut ctx = ConnectionContext {
            protocol,
            client_addr,
            target_addr: client_addr, // Will be updated based on the request
            secure: false,
        };

        // Extract target address from the request if HTTP
        if matches!(protocol, Protocol::Http1 | Protocol::Http2) {
            if let Some(host) = Self::extract_host_from_buffer(&buf) {
                // Parse host into SocketAddr
                if let Ok(addr) = Self::resolve_host(&host).await {
                    ctx.target_addr = addr;
                }
            }
        }

        // Apply security measures
        security.apply_security_measures(&ctx).await?;

        match protocol {
            Protocol::Http2 if !buf.is_empty() => {
                Self::handle_http2(pool, stream, ctx, buf).await?;
            }
            Protocol::WebSocket => {
                Self::handle_websocket(pool, stream, ctx, buf).await?;
            }
            Protocol::Http1 | Protocol::Http2 => {
                Self::handle_http1(pool, stream, ctx, buf).await?;
            }
            Protocol::Unknown => {
                warn!("Unknown protocol, closing connection");
                return Err(ProxyError::ProtocolError("Unknown protocol".into()));
            }
        }

        Ok(())
    }

    async fn resolve_host(host: &str) -> Result<SocketAddr, ProxyError> {
        // Parse host and port
        let (host, port) = match host.split_once(':') {
            Some((h, p)) => (h, p.parse().unwrap_or(80)),
            None => (host, 80),
        };

        // Resolve host to IP
        let addr = tokio::net::lookup_host(format!("{}:{}", host, port))
            .await
            .map_err(|e| ProxyError::ProtocolError(format!("Failed to resolve host: {}", e)))?
            .next()
            .ok_or_else(|| ProxyError::ProtocolError("No addresses found for host".into()))?;

        Ok(addr)
    }

    fn extract_host_from_buffer(buf: &[u8]) -> Option<String> {
        if let Ok(data) = std::str::from_utf8(buf) {
            // Look for Host header
            for line in data.lines() {
                if line.starts_with("Host: ") {
                    return Some(line[6..].trim().to_string());
                }
            }
        }
        None
    }

    async fn detect_protocol<T>(stream: &mut T, buf: &mut BytesMut) -> Result<Protocol, ProxyError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        // Read initial bytes into buffer
        let bytes_read = stream.read_buf(buf).await
            .map_err(|e| ProxyError::ProtocolError(format!("Failed to read from stream: {}", e)))?;

        if bytes_read == 0 {
            return Err(ProxyError::ProtocolError("Connection closed before protocol detection".into()));
        }

        // Check for HTTP/2 preface
        if buf.starts_with(b"PRI * HTTP/2.0\r\n") {
            return Ok(Protocol::Http2);
        }

        // Define methods with fixed sizes
        const GET: &[u8] = b"GET ";
        const POST: &[u8] = b"POST";
        const PUT: &[u8] = b"PUT ";
        const DELETE: &[u8] = b"DELETE";
        const HEAD: &[u8] = b"HEAD";
        const OPTIONS: &[u8] = b"OPTIONS";

        let methods = [GET, POST, PUT, DELETE, HEAD, OPTIONS];

        for &method in methods.iter() {
            if buf.starts_with(method) {
                // Check for WebSocket upgrade
                if method == GET {
                    let data = String::from_utf8_lossy(&buf[..]);
                    if data.contains("Upgrade: websocket") {
                        return Ok(Protocol::WebSocket);
                    }
                }
                return Ok(Protocol::Http1);
            }
        }

        Ok(Protocol::Unknown)
    }

    async fn handle_http1<T>(
        pool: Arc<ConnectionPool>,
        client_stream: T,  // Remove mut
        ctx: ConnectionContext,
        initial_data: BytesMut,  // Remove mut
    ) -> Result<(), ProxyError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        debug!("Handling HTTP/1.1 connection from {}", ctx.client_addr);
        let conn_id = pool.get_connection(ctx.target_addr).await?;
        let target_stream = pool.get_stream(&conn_id).await?;

        let (mut client_read, mut client_write) = tokio::io::split(client_stream);
        let (mut target_read, mut target_write) = tokio::io::split(target_stream);

        // If we have initial data, forward it first
        if !initial_data.is_empty() {
            target_write.write_all(&initial_data).await
                .map_err(|e| ProxyError::ProtocolError(format!("Failed to write initial data: {}", e)))?;
        }

        // Create two tasks for bidirectional forwarding
        let client_to_target = async {
            let mut buffer = BytesMut::with_capacity(8192);
            loop {
                match client_read.read_buf(&mut buffer).await {
                    Ok(0) => break Ok(()), // EOF
                    Ok(_n) => {
                        if let Err(e) = target_write.write_all(&buffer).await {
                            break Err(ProxyError::ProtocolError(format!("Write to target failed: {}", e)));
                        }
                        buffer.clear();
                    }
                    Err(e) => break Err(ProxyError::ProtocolError(format!("Read from client failed: {}", e))),
                }
            }
        };

        let target_to_client = async {
            let mut buffer = BytesMut::with_capacity(8192);
            loop {
                match target_read.read_buf(&mut buffer).await {
                    Ok(0) => break Ok(()), // EOF
                    Ok(_n) => {
                        if let Err(e) = client_write.write_all(&buffer).await {
                            break Err(ProxyError::ProtocolError(format!("Write to client failed: {}", e)));
                        }
                        buffer.clear();
                    }
                    Err(e) => break Err(ProxyError::ProtocolError(format!("Read from target failed: {}", e))),
                }
            }
        };

        // Run both forwarding directions concurrently
        tokio::select! {
            result = client_to_target => result?,
            result = target_to_client => result?,
        }

        Ok(())
    }

    async fn handle_http2<T>(
        _pool: Arc<ConnectionPool>,
        _stream: T,
        _ctx: ConnectionContext,
        _initial_data: BytesMut,
    ) -> Result<(), ProxyError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        // TODO: Implement HTTP/2 handling
        todo!("HTTP/2 handling not implemented yet")
    }

    async fn handle_websocket<T>(
        _pool: Arc<ConnectionPool>,
        _stream: T,
        _ctx: ConnectionContext,
        _initial_data: BytesMut,
    ) -> Result<(), ProxyError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        // TODO: Implement WebSocket handling
        todo!("WebSocket handling not implemented yet")
    }
}
