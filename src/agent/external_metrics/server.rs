use super::binary;
use super::line::{self, ParseResult};
use super::store::ExternalMetricsStore;
use super::types::ConnectionContext;
use crate::{debug, error, info, warn};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Protocol detection mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Binary,
    Line,
    Auto,
}

impl Protocol {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "binary" => Some(Protocol::Binary),
            "line" => Some(Protocol::Line),
            "auto" => Some(Protocol::Auto),
            _ => None,
        }
    }
}

/// Server state for tracking connections
pub struct ServerState {
    store: Arc<ExternalMetricsStore>,
    protocol: Protocol,
    active_connections: AtomicUsize,
    max_connections: usize,
    max_metrics_per_connection: usize,
}

impl ServerState {
    pub fn new(
        store: Arc<ExternalMetricsStore>,
        protocol: Protocol,
        max_connections: usize,
        max_metrics_per_connection: usize,
    ) -> Self {
        Self {
            store,
            protocol,
            active_connections: AtomicUsize::new(0),
            max_connections,
            max_metrics_per_connection,
        }
    }

    #[allow(dead_code)]
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }
}

/// Start the Unix domain socket server.
pub async fn serve(socket_path: &Path, state: Arc<ServerState>) -> Result<(), std::io::Error> {
    // Remove existing socket file if present
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    info!("external metrics server listening on {:?}", socket_path);

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let current = state.active_connections.fetch_add(1, Ordering::Relaxed);

                if current >= state.max_connections {
                    state.active_connections.fetch_sub(1, Ordering::Relaxed);
                    warn!("max connections reached, rejecting new connection");
                    continue;
                }

                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &state).await {
                        debug!("connection error: {}", e);
                    }
                    state.active_connections.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) => {
                error!("accept error: {}", e);
            }
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    state: &Arc<ServerState>,
) -> Result<(), std::io::Error> {
    let mut ctx = ConnectionContext::default();
    match state.protocol {
        Protocol::Binary => {
            handle_binary(stream, &state.store, &mut ctx, state.max_metrics_per_connection).await
        }
        Protocol::Line => {
            handle_line(stream, &state.store, &mut ctx, state.max_metrics_per_connection).await
        }
        Protocol::Auto => {
            handle_auto(stream, &state.store, &mut ctx, state.max_metrics_per_connection).await
        }
    }
}

async fn handle_auto(
    stream: UnixStream,
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<(), std::io::Error> {
    // Peek at first 4 bytes to detect protocol
    let mut peek_buf = [0u8; 4];
    let stream = stream.into_std()?;
    stream.set_nonblocking(false)?;

    // Use peek to check without consuming
    let peeked = {
        use std::io::Read;
        let mut stream_ref = &stream;
        stream_ref.read(&mut peek_buf)?
    };

    let stream = UnixStream::from_std(stream)?;

    if peeked >= 4 && peek_buf == binary::MAGIC {
        // Binary protocol - need to reconstruct the full message
        handle_binary_with_prefix(stream, store, &peek_buf[..peeked], ctx, max_metrics_per_connection).await
    } else {
        // Line protocol - treat peeked bytes as start of line
        handle_line_with_prefix(stream, store, &peek_buf[..peeked], ctx, max_metrics_per_connection).await
    }
}

async fn handle_binary(
    mut stream: UnixStream,
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<(), std::io::Error> {
    let mut buf = vec![0u8; binary::MAX_MESSAGE_SIZE];

    loop {
        // Read message
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break; // Connection closed
        }

        match binary::parse_and_ingest(&buf[..n], store, ctx, max_metrics_per_connection) {
            Ok(count) => {
                debug!("ingested {} metrics via binary protocol", count);
            }
            Err(e) => {
                debug!("binary parse error: {}", e);
                store.record_parse_error();
            }
        }
    }

    Ok(())
}

async fn handle_binary_with_prefix(
    mut stream: UnixStream,
    store: &Arc<ExternalMetricsStore>,
    prefix: &[u8],
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<(), std::io::Error> {
    let mut buf = vec![0u8; binary::MAX_MESSAGE_SIZE];

    // Copy prefix into buffer
    buf[..prefix.len()].copy_from_slice(prefix);

    loop {
        // Read rest of message (if any)
        let n = stream.read(&mut buf[prefix.len()..]).await?;
        let total = if !prefix.is_empty() {
            prefix.len() + n
        } else {
            n
        };

        if total == 0 || (n == 0 && prefix.is_empty()) {
            break;
        }

        match binary::parse_and_ingest(&buf[..total], store, ctx, max_metrics_per_connection) {
            Ok(count) => {
                debug!("ingested {} metrics via binary protocol", count);
            }
            Err(e) => {
                debug!("binary parse error: {}", e);
                store.record_parse_error();
            }
        }

        // Only use prefix for first message
        if !prefix.is_empty() {
            return handle_binary(stream, store, ctx, max_metrics_per_connection).await;
        }
    }

    Ok(())
}

async fn handle_line(
    stream: UnixStream,
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<(), std::io::Error> {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        match line::parse_line_with_context(&line, store, ctx, max_metrics_per_connection) {
            Ok(ParseResult::MetricIngested) => {
                debug!("ingested metric via line protocol");
            }
            Ok(ParseResult::SessionSet) => {
                debug!("session labels set");
            }
            Ok(ParseResult::Skipped) | Ok(ParseResult::MetricRejected) => {}
            Err(e) => {
                debug!("line parse error: {}", e);
                store.record_parse_error();
            }
        }
    }

    Ok(())
}

async fn handle_line_with_prefix(
    stream: UnixStream,
    store: &Arc<ExternalMetricsStore>,
    prefix: &[u8],
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<(), std::io::Error> {
    // Convert prefix to string (it's the start of a line)
    let prefix_str = String::from_utf8_lossy(prefix);

    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    // Handle first line with prefix
    if let Some(rest) = lines.next_line().await? {
        let full_line = format!("{}{}", prefix_str, rest);
        match line::parse_line_with_context(&full_line, store, ctx, max_metrics_per_connection) {
            Ok(ParseResult::MetricIngested) => {
                debug!("ingested metric via line protocol");
            }
            Ok(ParseResult::SessionSet) => {
                debug!("session labels set");
            }
            Ok(ParseResult::Skipped) | Ok(ParseResult::MetricRejected) => {}
            Err(e) => {
                debug!("line parse error: {}", e);
                store.record_parse_error();
            }
        }
    }

    // Handle remaining lines normally
    while let Some(line) = lines.next_line().await? {
        match line::parse_line_with_context(&line, store, ctx, max_metrics_per_connection) {
            Ok(ParseResult::MetricIngested) => {
                debug!("ingested metric via line protocol");
            }
            Ok(ParseResult::SessionSet) => {
                debug!("session labels set");
            }
            Ok(ParseResult::Skipped) | Ok(ParseResult::MetricRejected) => {}
            Err(e) => {
                debug!("line parse error: {}", e);
                store.record_parse_error();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Duration;

    #[test]
    fn test_protocol_from_str() {
        assert_eq!(Protocol::from_str("binary"), Some(Protocol::Binary));
        assert_eq!(Protocol::from_str("BINARY"), Some(Protocol::Binary));
        assert_eq!(Protocol::from_str("line"), Some(Protocol::Line));
        assert_eq!(Protocol::from_str("auto"), Some(Protocol::Auto));
        assert_eq!(Protocol::from_str("invalid"), None);
    }

    #[test]
    fn test_server_state_connections() {
        let store = Arc::new(ExternalMetricsStore::new(
            Duration::from_secs(60),
            1000,
            HashSet::new(),
        ));
        let state = ServerState::new(store, Protocol::Auto, 100, 10000);

        assert_eq!(state.active_connections(), 0);
        state.active_connections.fetch_add(1, Ordering::Relaxed);
        assert_eq!(state.active_connections(), 1);
    }
}
