use super::state::{DumpToFileRequest, SharedState, TimeRange};
use ringlog::info;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use metriken_exposition::{MsgpackToParquet, ParquetOptions, Snapshot};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
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
    /// Resolve the time range from query parameters
    pub fn resolve_time_range(&self) -> Result<TimeRange, String> {
        // "last" takes precedence over start/end
        if let Some(last) = &self.last {
            let duration: humantime::Duration = last
                .parse()
                .map_err(|e| format!("invalid duration '{}': {}", last, e))?;
            let now = SystemTime::now();
            let start = now
                .checked_sub((*duration).into())
                .ok_or_else(|| "duration too large".to_string())?;
            return Ok(TimeRange::new(Some(start), Some(now)));
        }

        let start = self.start.as_ref().map(|s| parse_timestamp(s)).transpose()?;
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

/// Response for GET /status
#[derive(Serialize)]
pub struct StatusResponse {
    pub buffer_duration_secs: u64,
    pub sampling_interval_ms: u64,
    pub snapshot_count: u64,
    pub snapshots_written: u64,
    pub oldest_timestamp: Option<u64>,
    pub newest_timestamp: Option<u64>,
    pub buffer_utilization: f64,
}

/// Response for POST /dump/file
#[derive(Serialize)]
pub struct DumpFileResponse {
    pub path: String,
    pub snapshots: u64,
    pub time_range: Option<TimeRangeResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct TimeRangeResponse {
    pub start: u64,
    pub end: u64,
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
         - GET /dump - Download ring buffer\n\
         - POST /dump/file - Write ring buffer to file\n"
    )
}

async fn status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let shared = &state.shared;
    let snapshots_written = shared.snapshots_written();
    let valid_count = shared.valid_snapshot_count();

    let utilization = if shared.snapshot_count > 0 {
        (valid_count as f64) / (shared.snapshot_count as f64)
    } else {
        0.0
    };

    // Try to get timestamp bounds from the ring buffer
    let (oldest, newest) = get_timestamp_bounds(shared).unwrap_or((None, None));

    Json(StatusResponse {
        buffer_duration_secs: shared.duration.as_secs(),
        sampling_interval_ms: shared.interval.as_millis() as u64,
        snapshot_count: shared.snapshot_count,
        snapshots_written,
        oldest_timestamp: oldest,
        newest_timestamp: newest,
        buffer_utilization: utilization,
    })
}

async fn dump(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DumpParams>,
) -> Response {
    let shared = &state.shared;

    let time_range = match params.resolve_time_range() {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    // Read and filter snapshots
    let data = match read_filtered_snapshots(shared, &time_range) {
        Ok(d) => d,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    if data.is_empty() {
        return (StatusCode::OK, "no snapshots match the specified time range").into_response();
    }

    match convert_to_parquet(&data, shared.interval) {
        Ok(parquet_data) => Response::builder()
            .header("Content-Type", "application/octet-stream")
            .header(
                "Content-Disposition",
                "attachment; filename=\"dump.parquet\"",
            )
            .body(axum::body::Body::from(parquet_data))
            .unwrap(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn dump_to_file(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DumpParams>,
) -> Response {
    let time_range = match params.resolve_time_range() {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(DumpFileResponse {
            path: String::new(),
            snapshots: 0,
            time_range: None,
            error: Some(e),
        })).into_response(),
    };

    // Send request to sampling loop
    let (response_tx, response_rx) = oneshot::channel();
    let request = DumpToFileRequest {
        time_range,
        response_tx,
    };

    if state.dump_tx.send(request).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(DumpFileResponse {
            path: String::new(),
            snapshots: 0,
            time_range: None,
            error: Some("sampling loop not available".to_string()),
        })).into_response();
    }

    // Wait for response
    match response_rx.await {
        Ok(response) => {
            if let Some(error) = response.error {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(DumpFileResponse {
                    path: String::new(),
                    snapshots: 0,
                    time_range: None,
                    error: Some(error),
                })).into_response()
            } else {
                let time_range = match (response.start_time, response.end_time) {
                    (Some(start), Some(end)) => Some(TimeRangeResponse { start, end }),
                    _ => None,
                };
                Json(DumpFileResponse {
                    path: response.path.to_string_lossy().to_string(),
                    snapshots: response.snapshots,
                    time_range,
                    error: None,
                }).into_response()
            }
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(DumpFileResponse {
            path: String::new(),
            snapshots: 0,
            time_range: None,
            error: Some("failed to receive response from sampling loop".to_string()),
        })).into_response(),
    }
}

/// Read snapshots from the ring buffer, optionally filtering by time
fn read_filtered_snapshots(
    shared: &SharedState,
    time_range: &TimeRange,
) -> Result<Vec<u8>, String> {
    let mut file = File::open(&shared.temp_path)
        .map_err(|e| format!("failed to open ring buffer: {}", e))?;

    let idx = shared.idx();
    let valid_count = shared.valid_snapshot_count();

    let mut result = Vec::new();

    // Iterate through snapshots in chronological order
    for offset in 0..valid_count {
        let mut i = idx + offset;
        if i >= shared.snapshot_count {
            i -= shared.snapshot_count;
        }

        // Seek to the start of the snapshot slot
        file.seek(SeekFrom::Start(i * shared.snapshot_len))
            .map_err(|e| format!("failed to seek: {}", e))?;

        // Read the size of the snapshot
        let mut len_bytes = [0u8; 8];
        file.read_exact(&mut len_bytes)
            .map_err(|e| format!("failed to read snapshot length: {}", e))?;

        let size = u64::from_be_bytes(len_bytes) as usize;
        if size == 0 {
            // Empty slot, skip
            continue;
        }

        // Read the snapshot data
        let mut buf = vec![0u8; size];
        file.read_exact(&mut buf)
            .map_err(|e| format!("failed to read snapshot data: {}", e))?;

        // Check time filter
        if time_range.start.is_some() || time_range.end.is_some() {
            if let Some(timestamp) = extract_timestamp(&buf) {
                if !time_range.contains(timestamp) {
                    continue;
                }
            }
        }

        result.extend_from_slice(&buf);
    }

    Ok(result)
}

/// Extract the timestamp from a msgpack snapshot
fn extract_timestamp(data: &[u8]) -> Option<SystemTime> {
    let snapshot: Snapshot = rmp_serde::from_slice(data).ok()?;
    match snapshot {
        Snapshot::V2(s) => Some(s.systemtime),
        _ => None,
    }
}

/// Get the oldest and newest timestamps in the ring buffer
fn get_timestamp_bounds(shared: &SharedState) -> Result<(Option<u64>, Option<u64>), String> {
    let mut file = File::open(&shared.temp_path)
        .map_err(|e| format!("failed to open ring buffer: {}", e))?;

    let idx = shared.idx();
    let valid_count = shared.valid_snapshot_count();

    if valid_count == 0 {
        return Ok((None, None));
    }

    let mut oldest: Option<SystemTime> = None;
    let mut newest: Option<SystemTime> = None;

    // Read first and last valid snapshots
    for offset in [0, valid_count.saturating_sub(1)] {
        let mut i = idx + offset;
        if i >= shared.snapshot_count {
            i -= shared.snapshot_count;
        }

        file.seek(SeekFrom::Start(i * shared.snapshot_len))
            .map_err(|e| format!("failed to seek: {}", e))?;

        let mut len_bytes = [0u8; 8];
        if file.read_exact(&mut len_bytes).is_err() {
            continue;
        }

        let size = u64::from_be_bytes(len_bytes) as usize;
        if size == 0 {
            continue;
        }

        let mut buf = vec![0u8; size];
        if file.read_exact(&mut buf).is_err() {
            continue;
        }

        if let Some(timestamp) = extract_timestamp(&buf) {
            if offset == 0 {
                oldest = Some(timestamp);
            } else {
                newest = Some(timestamp);
            }
        }
    }

    // If only one snapshot, newest == oldest
    if newest.is_none() && oldest.is_some() {
        newest = oldest;
    }

    let oldest_unix = oldest.and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()));
    let newest_unix = newest.and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()));

    Ok((oldest_unix, newest_unix))
}

/// Convert msgpack data to parquet format
fn convert_to_parquet(data: &[u8], interval: Duration) -> Result<Vec<u8>, String> {
    // Write msgpack data to a temp file
    let mut input = tempfile::tempfile()
        .map_err(|e| format!("failed to create temp file: {}", e))?;

    std::io::Write::write_all(&mut input, data)
        .map_err(|e| format!("failed to write temp file: {}", e))?;

    input.seek(SeekFrom::Start(0))
        .map_err(|e| format!("failed to seek temp file: {}", e))?;

    // Create output temp file
    let output = tempfile::tempfile()
        .map_err(|e| format!("failed to create output temp file: {}", e))?;

    // Convert
    MsgpackToParquet::with_options(ParquetOptions::new())
        .metadata(
            "sampling_interval_ms".to_string(),
            interval.as_millis().to_string(),
        )
        .convert_file_handle(input, &output)
        .map_err(|e| format!("failed to convert to parquet: {}", e))?;

    // Read the parquet data
    let mut output = output;
    output.seek(SeekFrom::Start(0))
        .map_err(|e| format!("failed to seek output file: {}", e))?;

    let mut result = Vec::new();
    output.read_to_end(&mut result)
        .map_err(|e| format!("failed to read parquet data: {}", e))?;

    Ok(result)
}
