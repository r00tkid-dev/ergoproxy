use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock};
use anyhow::Result;
use thiserror::Error;
use tracing::{debug, warn};
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum PoolError {
    #[error("Pool is full")]
    PoolFull,
    #[error("Connection timed out")]
    ConnectionTimeout,
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Invalid connection state")]
    InvalidState,
}

/// Connection states for lifecycle management
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Active,
    Idle,
    Checking,
    Failed,
}

/// Connection metrics for health and performance monitoring
#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    created_at: Instant,
    last_used: Instant,
    error_count: u32,
    total_requests: u64,
    avg_response_time: Duration,
    last_error: Option<String>,
}

impl ConnectionMetrics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            created_at: now,
            last_used: now,
            error_count: 0,
            total_requests: 0,
            avg_response_time: Duration::from_secs(0),
            last_error: None,
        }
    }

    fn record_request(&mut self, duration: Duration) {
        self.total_requests += 1;
        self.last_used = Instant::now();
        
        // Update average response time
        self.avg_response_time = Duration::from_nanos(
            ((self.avg_response_time.as_nanos() * (self.total_requests - 1) as u128
              + duration.as_nanos())
             / self.total_requests as u128) as u64
        );
    }

    fn record_error(&mut self, error: String) {
        self.error_count += 1;
        self.last_error = Some(error);
    }
}

/// Core connection wrapper
#[derive(Debug)]
pub struct PooledConnection {
    id: String,
    stream: TcpStream,
    state: ConnectionState,
    metrics: ConnectionMetrics,
    target: SocketAddr,
}

impl PooledConnection {
    fn new(stream: TcpStream, target: SocketAddr) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            stream,
            state: ConnectionState::Active,
            metrics: ConnectionMetrics::new(),
            target,
        }
    }

    fn mark_used(&mut self) {
        self.metrics.last_used = Instant::now();
        self.state = ConnectionState::Active;
    }

    fn mark_idle(&mut self) {
        self.state = ConnectionState::Idle;
    }

    fn mark_failed(&mut self, error: String) {
        self.state = ConnectionState::Failed;
        self.metrics.record_error(error);
    }

    async fn check_health(&mut self) -> bool {
        self.state = ConnectionState::Checking;
        
        // Basic health check - try to write/read
        match self.stream.try_write(&[0u8]) {
            Ok(_) => {
                self.state = ConnectionState::Idle;
                true
            }
            Err(e) => {
                self.mark_failed(e.to_string());
                false
            }
        }
    }
}

/// Pool configuration
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_size: usize,
    pub max_idle_time: Duration,
    pub health_check_interval: Duration,
    pub connection_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 50,
            max_idle_time: Duration::from_secs(60),
            health_check_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(5),
        }
    }
}

/// Main connection pool
pub struct ConnectionPool {
    connections: RwLock<HashMap<String, Mutex<PooledConnection>>>,
    config: PoolConfig,
}

impl ConnectionPool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            config,
        }
    }
    pub async fn get_stream(&self, conn_id: &str) -> Result<TcpStream, PoolError> {
        let connections = self.connections.read().await;
        if let Some(conn) = connections.get(conn_id) {
            let conn = conn.lock().await;
            // For now, create a new connection - we'll improve this later
            let stream = TcpStream::connect(conn.target)
                .await
                .map_err(|e| PoolError::ConnectionFailed(e.to_string()))?;
            Ok(stream)
        } else {
            Err(PoolError::InvalidState)
        }
    }

    /// Get a connection from the pool or create a new one
    pub async fn get_connection(&self, target: SocketAddr) -> Result<String, PoolError> {
        // First, try to find an idle connection
        if let Some(conn_id) = self.find_idle_connection(&target).await {
            debug!("Reusing idle connection: {}", conn_id);
            return Ok(conn_id);
        }

        // If pool is not full, create a new connection
        let connections = self.connections.read().await;
        if connections.len() < self.config.max_size {
            drop(connections); // Release the read lock
            return self.create_new_connection(target).await;
        }

        // Pool is full, try to clean up and retry
        drop(connections);
        self.cleanup_expired().await?;
        self.create_new_connection(target).await
    }

    /// Create a new connection
    async fn create_new_connection(&self, target: SocketAddr) -> Result<String, PoolError> {
    let stream = tokio::time::timeout(
        self.config.connection_timeout,
        TcpStream::connect(target)
    ).await
    .map_err(|_| PoolError::ConnectionTimeout)?
    .map_err(|e| PoolError::ConnectionFailed(e.to_string()))?;

    let conn = PooledConnection::new(stream, target);
    let conn_id = conn.id.clone();

    let mut connections = self.connections.write().await;
    connections.insert(conn_id.clone(), Mutex::new(conn));

    debug!("Created new connection: {}", conn_id);
    Ok(conn_id)
    }
    /// Find an idle connection for the target
    async fn find_idle_connection(&self, target: &SocketAddr) -> Option<String> {
        let connections = self.connections.read().await;
        for (id, conn) in connections.iter() {
            if let Ok(conn) = conn.try_lock() {
                if conn.target == *target && conn.state == ConnectionState::Idle {
                    return Some(id.clone());
                }
            }
        }
        None
    }

    /// Cleanup expired connections
    async fn cleanup_expired(&self) -> Result<(), PoolError> {
        let now = Instant::now();
        let mut connections = self.connections.write().await;
        
        let mut to_remove = Vec::new();
        
        for (id, conn) in connections.iter() {
            if let Ok(conn) = conn.try_lock() {
                if now.duration_since(conn.metrics.last_used) > self.config.max_idle_time
                    || conn.state == ConnectionState::Failed {
                    to_remove.push(id.clone());
                }
            }
        }

        for id in to_remove {
            connections.remove(&id);
            debug!("Removed expired connection: {}", id);
        }

        Ok(())
    }

    /// Start background health checks
    pub async fn start_health_checks(self: Arc<Self>) {
        let pool = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                debug!("Starting health check cycle");
                pool.check_all_connections().await;
                tokio::time::sleep(pool.config.health_check_interval).await;
            }
        });
    }

    /// Check all connections health
    async fn check_all_connections(&self) {
        let connections = self.connections.read().await;
        for (id, conn) in connections.iter() {
            if let Ok(mut conn) = conn.try_lock() {
                if conn.state != ConnectionState::Active {
                    if !conn.check_health().await {
                        warn!("Connection {} failed health check", id);
                    }
                }
            }
        }
    }

    /// Get pool metrics
    pub async fn get_metrics(&self) -> PoolMetrics {
        let connections = self.connections.read().await;
        let mut metrics = PoolMetrics::default();

        for conn in connections.values() {
            if let Ok(conn) = conn.try_lock() {
                match conn.state {
                    ConnectionState::Active => metrics.active_connections += 1,
                    ConnectionState::Idle => metrics.idle_connections += 1,
                    ConnectionState::Failed => metrics.failed_connections += 1,
                    ConnectionState::Checking => metrics.checking_connections += 1,
                }
            }
        }

        metrics.total_connections = connections.len();
        metrics
    }
}

#[derive(Debug, Default)]
pub struct PoolMetrics {
    pub total_connections: usize,
    pub active_connections: usize,
    pub idle_connections: usize,
    pub failed_connections: usize,
    pub checking_connections: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_connection_creation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = ConnectionPool::new(PoolConfig::default());
        let conn_id = pool.get_connection(addr).await.unwrap();
        
        let metrics = pool.get_metrics().await;
        assert_eq!(metrics.total_connections, 1);
        assert_eq!(metrics.active_connections, 1);
    }

    #[tokio::test]
    async fn test_connection_reuse() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = ConnectionPool::new(PoolConfig::default());
        
        // Create first connection
        let conn_id1 = pool.get_connection(addr).await.unwrap();
        
        // Should reuse the same connection
        let conn_id2 = pool.get_connection(addr).await.unwrap();
        
        assert_eq!(conn_id1, conn_id2);
    }
}
