//! The hindsight HTTP endpoints.
//!
//! Every one of them is stated in v3 terms. `/status` used to report the ring's
//! geometry — slot count, writes so far, and the fill fraction derived from
//! them — and none of that survives: the buffer is a `.rez` file, not a fixed
//! array of slots. What replaces it is what the file's own catalog can answer
//! (`buffer::summarize`), which is both truer and cheaper: no segment or WAL
//! payload is read to produce it.

use super::buffer;
use super::state::{DumpToFileRequest, SharedState, TimeRange};
use tracing::info;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Application state for HTTP handlers
pub struct AppState {
    pub shared: Arc<SharedState>,
    pub dump_tx: mpsc::Sender<DumpToFileRequest>,
}

/// Query parameters for dump endpoints
#[derive(Debug, Deserialize)]
pub struct DumpParams {
    /// Start time as Unix timestamp (seconds) or RFC 3339 datetime
    pub start: Option<String>,
    /// End time as Unix timestamp (seconds) or RFC 3339 datetime
    pub end: Option<String>,
    /// Relative time range (e.g., "60m", "2h")
    pub last: Option<String>,
}

/// Parse a timestamp string as either Unix epoch seconds or RFC 3339 datetime
fn parse_timestamp(s: &str) -> Result<SystemTime, String> {
    // Try parsing as Unix timestamp first (integer seconds)
    if let Ok(ts) = s.parse::<u64>() {
        return Ok(UNIX_EPOCH + Duration::from_secs(ts));
    }

    // Try parsing as RFC 3339 datetime
    use chrono::{DateTime, Utc};
    let dt: DateTime<Utc> = s
        .parse()
        .map_err(|_| format!("invalid timestamp '{}': expected Unix seconds or RFC 3339 datetime (e.g., 2024-01-01T12:00:00Z)", s))?;

    Ok(SystemTime::from(dt))
}

impl DumpParams {
    /// Resolve the time range from query parameters.
    ///
    /// The range still selects, but it selects whole segments: a `.rez` segment
    /// is an immutable parquet BLOB, so a dump keeps any segment that overlaps
    /// the range rather than rewriting one. The response reports the span
    /// actually written, which is how a caller learns it got more than it asked
    /// for — the alternative, silently honoring the request in the reply while
    /// shipping something wider, is the one thing this must not do.
    pub fn resolve_time_range(&self) -> Result<TimeRange, String> {
        // "last" takes precedence over start/end
        if let Some(last) = &self.last {
            let duration: humantime::Duration = last
                .parse()
                .map_err(|e| format!("invalid duration '{}': {}", last, e))?;
            let now = SystemTime::now();
            let start = now
                .checked_sub(*duration)
                .ok_or_else(|| "duration too large".to_string())?;
            return Ok(TimeRange::new(Some(start), Some(now)));
        }

        let start = self
            .start
            .as_ref()
            .map(|s| parse_timestamp(s))
            .transpose()?;
        let end = self.end.as_ref().map(|s| parse_timestamp(s)).transpose()?;

        // Validate start <= end if both are specified
        if let (Some(s), Some(e)) = (start, end) {
            if s > e {
                return Err("start time must be before end time".to_string());
            }
        }

        Ok(TimeRange::new(start, end))
    }
}

/// Response for GET /status.
///
/// v3 shape. The ring fields it replaces (`snapshot_count`, and the
/// `buffer_utilization` computed from it) described a fixed array of slots and
/// have no analogue here — a `.rez` buffer is bounded by TIME, not by slots, so
/// what it is doing is described by the span it retains and the size it has
/// reached.
#[derive(Serialize)]
pub struct StatusResponse {
    /// The configured lookback — `[general] duration`.
    pub lookback_secs: u64,
    pub sampling_interval_ms: u64,
    /// Snapshots pulled and ingested since startup.
    pub ticks_recorded: u64,
    /// The span actually retained. It reaches at least the lookback once the
    /// buffer is full, and typically a little further: retention drops whole
    /// segments rather than rewriting them.
    pub oldest_timestamp: Option<u64>,
    pub newest_timestamp: Option<u64>,
    pub retained_secs: Option<u64>,
    /// Whether retention has begun — the buffer is now dropping as much as it
    /// takes in rather than still filling. Note this becomes true while
    /// `retained_secs` still reads just *under* `lookback_secs`: the oldest
    /// surviving row sits one tick inside the cutoff, by construction.
    pub at_retention_bound: bool,
    /// Rows a reader would see across every table.
    pub rows: u64,
    /// Bytes on disk, `-wal`/`-shm` sidecars included.
    pub bytes: u64,
    /// Pages on the free list against the file's total. Near zero in steady
    /// state — freed pages are reused in place — and the signal for whether
    /// the incremental reclaim is keeping up after a spike.
    pub free_pages: u32,
    pub pages: u32,
    pub tables: Vec<TableStatus>,
}

#[derive(Serialize)]
pub struct TableStatus {
    pub sampler: String,
    pub rows: u64,
    pub segments: u64,
    /// Rows committed but not yet sealed. Recoverable after a kill; for a quiet
    /// table this may be all of its rows.
    pub live_wal_rows: u64,
}

/// Response for POST /dump/file
#[derive(Serialize)]
pub struct DumpFileResponse {
    pub path: String,
    pub rows: u64,
    pub tables: u64,
    pub bytes: u64,
    /// The span the dump actually covers — not the span requested.
    pub time_range: Option<TimeRangeResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct TimeRangeResponse {
    pub start: u64,
    pub end: u64,
}

impl DumpFileResponse {
    fn failed(error: String) -> Self {
        Self {
            path: String::new(),
            rows: 0,
            tables: 0,
            bytes: 0,
            time_range: None,
            error: Some(error),
        }
    }
}

/// Nanosecond row stamps as Unix seconds, which is what these endpoints have
/// always reported.
fn to_secs(ns: u64) -> u64 {
    ns / 1_000_000_000
}

/// Start the HTTP server
pub async fn serve(
    listen: SocketAddr,
    shared: Arc<SharedState>,
    dump_tx: mpsc::Sender<DumpToFileRequest>,
) {
    let state = Arc::new(AppState { shared, dump_tx });

    let app = Router::new()
        .route("/", get(root))
        .route("/status", get(status))
        .route("/dump", get(dump))
        .route("/dump/file", post(dump_to_file))
        .with_state(state)
        .layer(ServiceBuilder::new().layer(CompressionLayer::new()));

    let listener = TcpListener::bind(listen)
        .await
        .expect("failed to bind HTTP listener");

    info!("HTTP endpoint listening on {}", listen);

    axum::serve(listener, app)
        .await
        .expect("failed to run HTTP server");
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "Rezolus {version} Hindsight\n\
         For information, see: https://rezolus.com\n\n\
         Endpoints:\n\
         - GET /status - Buffer status\n\
         - GET /dump - Download the buffer as a .rez archive\n\
         - POST /dump/file - Write the buffer to the configured output file\n"
    )
}

async fn status(State(state): State<Arc<AppState>>) -> Response {
    let shared = Arc::clone(&state.shared);
    let path = shared.buffer_path.clone();
    // Reading the catalog is file I/O, however cheap: keep it off the runtime
    // thread that is also driving the recording.
    let summary = match tokio::task::spawn_blocking(move || buffer::summarize(&path)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read the buffer: {e}"),
            )
                .into_response()
        }
    };

    Json(StatusResponse {
        lookback_secs: shared.lookback.as_secs(),
        sampling_interval_ms: shared.interval.as_millis() as u64,
        ticks_recorded: shared.ticks(),
        oldest_timestamp: summary.first_ts.map(to_secs),
        newest_timestamp: summary.last_ts.map(to_secs),
        retained_secs: summary.retained().map(|d| d.as_secs()),
        at_retention_bound: shared.at_retention_bound(),
        rows: summary.rows,
        bytes: summary.bytes,
        free_pages: summary.free_pages,
        pages: summary.pages,
        tables: summary
            .tables
            .iter()
            .map(|t| TableStatus {
                sampler: t.sampler.clone(),
                rows: t.rows,
                segments: t.segments,
                live_wal_rows: t.live_wal_rows,
            })
            .collect(),
    })
    .into_response()
}

/// Download the buffer as a standalone `.rez`.
///
/// Built the same way `/dump/file` builds it — a consistent copy taken while
/// the recording continues — into a temporary file that is served and removed.
/// It is a `.rez` archive now, not a parquet file: the buffer holds one table
/// per sampler at its own cadence, which no single parquet schema represents.
async fn dump(State(state): State<Arc<AppState>>, Query(params): Query<DumpParams>) -> Response {
    let time_range = match params.resolve_time_range() {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let shared = Arc::clone(&state.shared);
    let built = tokio::task::spawn_blocking(move || {
        let dir = shared
            .output_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(tempfile::TempDir::new_in)
            .unwrap_or_else(tempfile::TempDir::new)
            .map_err(|e| format!("failed to stage the dump: {e}"))?;
        let staged = dir.path().join("dump.rez");
        buffer::dump(&shared.buffer_path, &staged, &time_range)?;
        std::fs::read(&staged).map_err(|e| format!("failed to read the dump: {e}"))
    })
    .await;

    let bytes = match built {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the dump task failed: {e}"),
            )
                .into_response()
        }
    };

    Response::builder()
        .header("Content-Type", "application/octet-stream")
        .header("Content-Disposition", "attachment; filename=\"dump.rez\"")
        .body(axum::body::Body::from(bytes))
        .unwrap()
}

async fn dump_to_file(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DumpParams>,
) -> Response {
    let time_range = match params.resolve_time_range() {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(DumpFileResponse::failed(e))).into_response()
        }
    };

    let (response_tx, response_rx) = oneshot::channel();
    let request = DumpToFileRequest {
        time_range,
        response_tx,
    };

    if state.dump_tx.send(request).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DumpFileResponse::failed(
                "sampling loop not available".to_string(),
            )),
        )
            .into_response();
    }

    match response_rx.await {
        Ok(response) => {
            if let Some(error) = response.error {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(DumpFileResponse::failed(error)),
                )
                    .into_response();
            }
            let summary = response.summary.unwrap_or_default();
            Json(DumpFileResponse {
                path: response.path.to_string_lossy().to_string(),
                rows: summary.rows,
                tables: summary.tables.len() as u64,
                bytes: summary.bytes,
                time_range: match (summary.first_ts, summary.last_ts) {
                    (Some(start), Some(end)) => Some(TimeRangeResponse {
                        start: to_secs(start),
                        end: to_secs(end),
                    }),
                    _ => None,
                },
                error: None,
            })
            .into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DumpFileResponse::failed(
                "failed to receive response from sampling loop".to_string(),
            )),
        )
            .into_response(),
    }
}
