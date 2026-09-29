use super::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use metriken_query::BufferPool;

/// MCP protocol methods
#[derive(Debug)]
enum McpMethod {
    Initialize,
    ToolsList,
    ToolsCall,
    ResourcesList,
    ResourcesRead,
    PromptsList,
    NotificationsInitialized,
    Unknown(String),
}

impl From<&str> for McpMethod {
    fn from(s: &str) -> Self {
        match s {
            "initialize" => McpMethod::Initialize,
            "tools/list" => McpMethod::ToolsList,
            "tools/call" => McpMethod::ToolsCall,
            "resources/list" => McpMethod::ResourcesList,
            "resources/read" => McpMethod::ResourcesRead,
            "prompts/list" => McpMethod::PromptsList,
            "notifications/initialized" => McpMethod::NotificationsInitialized,
            other => McpMethod::Unknown(other.to_string()),
        }
    }
}

/// Available MCP tools
#[derive(Debug)]
enum McpTool {
    DescribeRecording,
    AnalyzeCorrelation,
    DescribeMetrics,
    DetectAnomalies,
    Query,
    ExtractFeatures,
    AddEvent,
    RemoveEvents,
    RunChecks,
    Unknown(String),
}

impl From<&str> for McpTool {
    fn from(s: &str) -> Self {
        match s {
            "describe_recording" => McpTool::DescribeRecording,
            "analyze_correlation" => McpTool::AnalyzeCorrelation,
            "describe_metrics" => McpTool::DescribeMetrics,
            "detect_anomalies" => McpTool::DetectAnomalies,
            "query" => McpTool::Query,
            "extract_features" => McpTool::ExtractFeatures,
            "add_event" => McpTool::AddEvent,
            "remove_events" => McpTool::RemoveEvents,
            "run_checks" => McpTool::RunChecks,
            other => McpTool::Unknown(other.to_string()),
        }
    }
}

/// Default buffer pool budget for the MCP server: 500 MB.
///
/// Multiple parquet files may be queried in a single MCP session; the
/// shared pool means row groups decoded for one file stay warm for the
/// next tool call against the same file.
const MCP_CACHE_SIZE_BYTES: usize = 500 * 1024 * 1024;

/// A cached reader, tagged with the recording it was opened from.
struct CachedReader {
    /// The chosen recording's label set — the recording's own identity, so
    /// two selectors that name the same recording of the same file share one
    /// reader rather than each retaining a copy of the archive.
    ///
    /// The `BTreeMap` itself, NOT a flattened string. A `\u{1}`-joined `k=v`
    /// render aliases: `{a: "\u{1}b=c"}` and `{a: "", b: "c"}` flatten to the
    /// same bytes, so the second lookup would hand back the first's reader and
    /// answer about the wrong recording. Comparing the maps cannot alias — the
    /// same shape of bug this whole selector arc kept closing.
    identity: std::collections::BTreeMap<String, String>,
    source: Arc<dyn metriken_query::MetricsSource>,
    /// What the analysis layer may trust this recording's metric names to
    /// mean, as the OPEN determined it. Cached alongside the reader because
    /// the reader cannot be asked afterwards: a recording selected out of a
    /// `.rez` reports the endpoint name the caller chose as its `source`, so
    /// re-deriving provenance from the reader would answer "foreign" for data
    /// that is unambiguously the agent's.
    provenance: crate::analysis::extract::Provenance,
}

/// The six read tools, as the server has always listed them.
fn read_tools() -> Vec<Value> {
    json!([
        {
            "name": "describe_recording",
            "description": "Describe a Rezolus performance recording with version and duration information",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the parquet file"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    }
                },
                "required": ["parquet_file"]
            }
        },
        {
            "name": "analyze_correlation",
            "description": "Analyze correlation between two metrics using PromQL",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the parquet file"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    },
                    "metric1": {
                        "type": "string",
                        "description": "First metric PromQL expression"
                    },
                    "metric2": {
                        "type": "string",
                        "description": "Second metric PromQL expression"
                    }
                },
                "required": ["parquet_file", "metric1", "metric2"]
            }
        },
        {
            "name": "describe_metrics",
            "description": "List and describe all metrics available in a Rezolus recording, organized by type",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the parquet file"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    }
                },
                "required": ["parquet_file"]
            }
        },
        {
            "name": "detect_anomalies",
            "description": "Detect anomalies in time series data using MAD, CUSUM, and FFT analysis. IMPORTANT: Call describe_metrics first to see available metrics and labels before constructing your query. The query must result in a SINGLE time series - use sum() to aggregate multiple series.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the parquet file"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    },
                    "query": {
                        "type": "string",
                        "description": "PromQL query that produces a SINGLE time series. For COUNTERS (monotonically increasing), use rate() to get per-second rates, e.g., 'sum(rate(cpu_usage[1m]))'. For GAUGES (point-in-time values), query directly, e.g., 'sum(memory_available)'. For HISTOGRAMS, use histogram_quantile(), e.g., 'histogram_quantile(0.99, scheduler_runqueue_latency)'. ALWAYS use sum() or other aggregation to collapse multiple series into one. DO NOT use label selectors like {state=\"busy\"} unless you've confirmed those labels exist in describe_metrics output."
                    }
                },
                "required": ["parquet_file", "query"]
            }
        },
        {
            "name": "query",
            "description": "Execute a PromQL query and return results as JSON. Returns Prometheus-compatible format with resultType (vector/matrix/scalar) and result data. Use describe_metrics first to see available metrics and their types. Results can be used programmatically by other tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the parquet file"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    },
                    "query": {
                        "type": "string",
                        "description": "PromQL query expression. For COUNTERS use rate(metric[1m]), for GAUGES query directly, for HISTOGRAMS use histogram_quantile(0.99, metric). Use sum(), avg(), etc. to aggregate multiple series."
                    }
                },
                "required": ["parquet_file", "query"]
            }
        },
        {
            "name": "extract_features",
            "description": "Extract a deterministic, versioned overview record of a recording's Rezolus-native features (per-metric stats, noise classification, anomalies, regime shifts, acquisition-window uncertainty, top-N correlations, resource rankings, subsystem coverage) as JSON. The record is the structured input for bottleneck assessment. Requires a recording of at least 10 seconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {
                        "type": "string",
                        "description": "Path to the recording (parquet or .rez)"
                    },
                    "recording": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
                    }
                },
                "required": ["parquet_file"]
            }
        }
    ])
    .as_array()
    .cloned()
    .unwrap_or_default()
}

/// The `recording` property every tool schema carries.
fn recording_property() -> Value {
    json!({
        "type": "object",
        "additionalProperties": {"type": "string"},
        "description": "Which recording to read from a multi-recording .rez, as label key/value pairs (e.g. {\"source\": \"redis\"}). Must name exactly one. Call describe_recording without it first to list them."
    })
}

/// The additive tools: they can only add to a recording, through the same
/// manifest update `recording annotate` uses, so a wrong call is an extra
/// event and not a lost one. On by default.
fn additive_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "add_event",
            "description": "Mark an instant or a range in a recording with an event the viewer draws on its timeline. Additive: it never removes or changes an existing event. Returns the event id, which remove_events accepts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {"type": "string", "description": "Path to the recording (parquet or .rez)"},
                    "recording": recording_property(),
                    "timestamp": {"type": ["string", "number"], "description": "When the event starts: an RFC 3339 string (e.g. 2026-09-28T14:03:11Z), or Unix seconds as a JSON number (never as a string: a digit-only string is refused as ambiguous)."},
                    "description": {"type": "string", "description": "One line, what happened. Shown on the timeline."},
                    "kind": {"type": "string", "description": "A short category such as deploy, incident, spike, or finding. Alignment and filters key on it."},
                    "duration": {"type": ["string", "number"], "description": "Makes the event a range: humantime (30s, 2m) or seconds as a JSON number (never a digit-only string). Omit for an instant."},
                    "details": {"type": "string", "description": "Longer text shown when the event is opened: what was observed, the query that found it."},
                    "source": {"type": "string", "description": "Who or what authored the event. Defaults to \"mcp\" so agent-written events can be filtered later."},
                    "node": {"type": "string", "description": "Scope the event to one node of a multi-node recording."},
                    "instance": {"type": "string", "description": "Scope the event to one service instance."},
                    "id": {"type": "string", "description": "A stable id. Adding an event whose id is already present is a no-op (except that a kind=check event replaces a stored check event with the same id). Minted as mcp:<uuid> when omitted."}
                },
                "required": ["parquet_file", "timestamp", "description"]
            }
        }),
        json!({
            "name": "run_checks",
            "description": "Evaluate the recording's KPI checks (the `check` blocks embedded by `recording annotate --queries`, or the ones in `queries`) and return each verdict with its violation windows. With annotate=true the violation windows are also written into the recording as kind=check events, the same as `rezolus recording check --annotate`. On a multi-recording .rez with no `recording` selector every recording is evaluated and each gets its own verdicts, as the CLI does.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parquet_file": {"type": "string", "description": "Path to the recording (parquet or .rez)"},
                    "recording": recording_property(),
                    "queries": {"type": "object", "description": "A ServiceExtension object evaluated instead of the recording's embedded KPIs: {\"service_name\": \"...\", \"kpis\": [{\"role\": \"...\", \"title\": \"...\", \"query\": \"<PromQL producing ONE series>\", \"type\": \"gauge\"|\"counter\"|\"histogram\", \"check\": {\"above\": N} | {\"below\": N}, optional \"quantile\", \"for\" (seconds), \"severity\": \"fail\"|\"warn\"}]}."},
                    "annotate": {"type": "boolean", "description": "Write the violation windows into the recording as events. Default false."}
                },
                "required": ["parquet_file"]
            }
        }),
    ]
}

/// The mutating tools: they can remove or replace what is stored, so they
/// stay behind `rezolus mcp --allow-mutating`. A server started without the
/// flag neither lists them nor runs them.
fn mutating_tools() -> Vec<Value> {
    vec![json!({
        "name": "remove_events",
        "description": "Remove events from a recording by id, kind, and/or source (every given field must match). Mutating: available only when the server was started with --allow-mutating. An empty filter is refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "parquet_file": {"type": "string", "description": "Path to the recording (parquet or .rez)"},
                "recording": recording_property(),
                "ids": {"type": "array", "items": {"type": "string"}, "description": "Event ids to remove (as returned by add_event, or check:<hash> for verdicts)."},
                "kind": {"type": "string", "description": "Remove events of this kind."},
                "source": {"type": "string", "description": "Remove events with this source, e.g. mcp for every agent-written event."}
            },
            "required": ["parquet_file"]
        }
    })]
}

/// The message a mutating call gets from a server started without the flag.
pub(crate) fn mutating_refused(tool: &str) -> String {
    format!(
        "{tool} is a mutating tool and this server was started without --allow-mutating; \
         restart it as `rezolus mcp --allow-mutating` to enable removals"
    )
}

/// A tool-call reply: the text on success, a JSON-RPC error otherwise.
fn tool_reply(
    id: Option<Value>,
    label: &str,
    result: Result<String, Box<dyn std::error::Error>>,
) -> Value {
    match result {
        Ok(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"content": [{"type": "text", "text": text}]}
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": format!("{label}: {e}")}
        }),
    }
}

/// Unix seconds past this are milliseconds or nanoseconds sent by mistake:
/// 1e11 s is the year 5138.
const MAX_UNIX_SECONDS: f64 = 1e11;
/// A duration past this (about 31 years) is nanoseconds sent as seconds.
const MAX_DURATION_SECONDS: f64 = 1e9;

/// `timestamp` as a tool argument: an RFC 3339 string, or Unix seconds as
/// a number. A digit-only string is refused rather than read as
/// nanoseconds (`annotate --event`'s convention): the same digits an agent
/// sends as a number mean seconds, and the two readings are a billion
/// apart with no error in between.
fn timestamp_ns_of(v: &Value) -> Result<u64, String> {
    match v {
        Value::String(s) if s.trim().chars().all(|c| c.is_ascii_digit()) => Err(format!(
            "timestamp {s:?} is a digit-only string, which is ambiguous (seconds or \
             nanoseconds?); send Unix seconds as a number or an RFC 3339 string"
        )),
        Value::String(s) => crate::parquet_tools::events::parse_timestamp_str(s),
        Value::Number(n) => {
            let secs = n.as_f64().ok_or("timestamp is not a finite number")?;
            if !secs.is_finite() || secs < 0.0 {
                return Err("timestamp must be non-negative Unix seconds".into());
            }
            if secs > MAX_UNIX_SECONDS {
                return Err(format!(
                    "timestamp {secs} is past the year 5138; send Unix seconds, not \
                     milliseconds or nanoseconds"
                ));
            }
            Ok((secs * 1e9).round() as u64)
        }
        _ => Err("timestamp must be an RFC 3339 string or Unix seconds".into()),
    }
}

/// `duration` as a tool argument: humantime (`30s`, `2m`) or seconds as a
/// number. A digit-only string is refused for the same reason as in
/// `timestamp_ns_of`.
fn duration_ns_of(v: &Value) -> Result<u64, String> {
    match v {
        Value::String(s) if s.trim().chars().all(|c| c.is_ascii_digit()) => Err(format!(
            "duration {s:?} is a digit-only string, which is ambiguous (seconds or \
             nanoseconds?); send seconds as a number or humantime such as \"30s\""
        )),
        Value::String(s) => crate::parquet_tools::events::parse_duration_str(s),
        Value::Number(n) => {
            let secs = n.as_f64().ok_or("duration is not a finite number")?;
            if !secs.is_finite() || secs <= 0.0 {
                return Err("duration must be a positive number of seconds".into());
            }
            if secs > MAX_DURATION_SECONDS {
                return Err(format!(
                    "duration {secs} s is over 31 years; send seconds, not nanoseconds"
                ));
            }
            Ok((secs * 1e9).round() as u64)
        }
        _ => Err("duration must be a humantime string or seconds".into()),
    }
}

/// The reader cache's path key: canonical when the file exists, so a
/// write through one spelling of the path evicts a reader opened through
/// another. `ParquetSource` keeps its footer offsets from open time, and
/// the footer path rewrites the file in place, so a stale reader would
/// decode the new bytes at the old offsets.
fn cache_path(parquet_file: &str) -> String {
    std::fs::canonicalize(parquet_file)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| parquet_file.to_string())
}

fn opt_str(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// What the operator enabled at `rezolus mcp` startup.
///
/// Tools come in tiers by what they can destroy: the read tools and the
/// additive ones (`add_event`, `run_checks`) are always on, since the worst
/// a wrong call does is add an event; the mutating ones (`remove_events`)
/// can take another person's events out of a shared recording, so they
/// need `--allow-mutating`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerOptions {
    pub allow_mutating: bool,
}

/// MCP server state
pub struct Server {
    options: ServerOptions,
    /// Keyed by (path, selector) — a TYPED pair, never a formatted string.
    ///
    /// A multi-recording `.rez` yields a DIFFERENT reader per recording from
    /// ONE path, so keying on the path alone serves the second request the
    /// first's reader and answers about the wrong arm, silently. Keying on
    /// `format!("{path}\u{{1}}{selector}")` has a narrower version of the same
    /// bug: label values are free text, so two different selectors can render
    /// identical key bytes. The tuple cannot alias.
    reader_cache: Arc<RwLock<HashMap<(String, crate::mcp::RecordingSelector), CachedReader>>>,
    /// Shared LRU row-group cache for all readers opened by this server.
    pool: Arc<BufferPool>,
}

impl Server {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_options(ServerOptions::default())
    }

    pub fn with_options(options: ServerOptions) -> Self {
        Self {
            options,
            reader_cache: Arc::new(RwLock::new(HashMap::new())),
            pool: BufferPool::new(MCP_CACHE_SIZE_BYTES),
        }
    }

    /// Every tool this server answers, in tiers: the read tools, then the
    /// additive ones, then the mutating ones only when enabled. A tool that
    /// is not listed is also not callable (`remove_events` without the flag
    /// answers with the flag's name).
    fn tool_list(&self) -> Vec<Value> {
        let mut tools = read_tools();
        tools.extend(additive_tools());
        if self.options.allow_mutating {
            tools.extend(mutating_tools());
        }
        tools
    }

    /// Run the MCP server using stdio.
    ///
    /// Dispatch is strictly serial: one request is read, handled to
    /// completion, and answered before the next is read. CPU-heavy tools
    /// (extract_features, exhaustive detect_anomalies, analyze_correlation)
    /// therefore block only the calling client's next request. If dispatch
    /// ever becomes concurrent, those handlers must move to spawn_blocking.
    pub async fn run_stdio(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        info!("MCP server ready, waiting for messages...");
        loop {
            debug!("Waiting for next line...");
            let line = match lines.next_line().await? {
                Some(line) => {
                    if line.trim().is_empty() {
                        debug!("Received empty line, continuing");
                        continue;
                    }
                    debug!("Received message: {line}");
                    line
                }
                None => {
                    info!("stdin closed, no more messages");
                    break;
                }
            };

            let message: Value = match serde_json::from_str(&line) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!("Failed to parse JSON: {e}");
                    continue;
                }
            };

            if let Some(response) = self.handle_message(message).await? {
                let response_str = serde_json::to_string(&response)?;
                debug!("Sending response: {response_str}");
                stdout.write_all(response_str.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }

        info!("MCP server shutting down");
        Ok(())
    }

    /// Handle a JSON-RPC message
    async fn handle_message(
        &mut self,
        message: Value,
    ) -> Result<Option<Value>, Box<dyn std::error::Error>> {
        let method = message
            .get("method")
            .and_then(|m| m.as_str())
            .map(McpMethod::from);
        let id = message.get("id").cloned();
        let params = message.get("params");

        match method {
            Some(McpMethod::Initialize) => {
                debug!("Received initialize request");
                Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": env!("CARGO_BIN_NAME"),
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }
                })))
            }
            Some(McpMethod::ToolsList) => {
                debug!("Received tools/list request");
                Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": self.tool_list()
                    }
                })))
            }
            Some(McpMethod::ToolsCall) => {
                debug!("Received tools/call request");
                if let Some(params) = params {
                    self.handle_tool_call(id, params).await
                } else {
                    Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32602,
                            "message": "Invalid params"
                        }
                    })))
                }
            }
            Some(McpMethod::ResourcesList) => {
                debug!("Received resources/list request");
                Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "resources": []
                    }
                })))
            }
            Some(McpMethod::ResourcesRead) => {
                debug!("Received resources/read request");
                Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "Resources not implemented"
                    }
                })))
            }
            Some(McpMethod::PromptsList) => {
                debug!("Received prompts/list request");
                Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "prompts": []
                    }
                })))
            }
            Some(McpMethod::NotificationsInitialized) => {
                debug!("Received notifications/initialized (no response needed)");
                Ok(None) // Notifications don't get responses
            }
            Some(McpMethod::Unknown(method_name)) => {
                debug!("Unknown method: {method_name}");
                // Only send error response if this is a request (has id), not a notification
                if id.is_some() {
                    Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": "Method not found"
                        }
                    })))
                } else {
                    Ok(None) // Don't respond to unknown notifications
                }
            }
            None => {
                debug!("Message missing method field");
                if id.is_some() {
                    Ok(Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32600,
                            "message": "Invalid Request: missing method"
                        }
                    })))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Handle a tool call
    async fn handle_tool_call(
        &mut self,
        id: Option<Value>,
        params: &Value,
    ) -> Result<Option<Value>, Box<dyn std::error::Error>> {
        let tool_name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or("Missing tool name")?;

        let tool = McpTool::from(tool_name);
        let arguments = params.get("arguments").ok_or("Missing arguments")?;

        match tool {
            McpTool::DescribeRecording => match self.describe_recording(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Error describing recording: {}", e)
                    }
                }))),
            },
            McpTool::AnalyzeCorrelation => match self.analyze_correlation(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Correlation error: {}", e)
                    }
                }))),
            },
            McpTool::DescribeMetrics => match self.describe_metrics(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Error describing metrics: {}", e)
                    }
                }))),
            },
            McpTool::DetectAnomalies => match self.detect_anomalies(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Anomaly detection error: {}", e)
                    }
                }))),
            },
            McpTool::Query => match self.execute_query(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Query error: {}", e)
                    }
                }))),
            },
            McpTool::ExtractFeatures => match self.execute_extract_features(arguments).await {
                Ok(result) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                }))),
                Err(e) => Ok(Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("Feature extraction error: {}", e)
                    }
                }))),
            },
            McpTool::AddEvent => Ok(Some(tool_reply(
                id,
                "add_event",
                self.add_event(arguments).await,
            ))),
            McpTool::RemoveEvents => Ok(Some(tool_reply(
                id,
                "remove_events",
                self.remove_events(arguments).await,
            ))),
            McpTool::RunChecks => Ok(Some(tool_reply(
                id,
                "run_checks",
                self.run_checks(arguments).await,
            ))),
            McpTool::Unknown(name) => Ok(Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Unknown tool: {}", name)
                }
            }))),
        }
    }

    /// Describe a recording file and return its metadata
    async fn describe_recording(
        &self,
        arguments: &Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        // No `exists()` check here: `open_source_with_pool_labeled` makes it,
        // so both front ends report a missing file with one message instead
        // of the server saying "Recording file not found" and the CLI
        // surfacing the decoder's "No such file or directory (os error 2)".
        let path = Path::new(parquet_file);
        let selector = Self::selector_of(arguments)?;
        // The CLI's own text, not `get_reader_selected` + `format_recording_info`.
        // With no selector a multi-recording archive is LISTED here rather
        // than refused, and `describe_recording` is where an agent starts, so
        // the server answering "pick one, here they are" is the behavior that
        // has to match. Reproducing that branch here instead is how the two
        // paths diverged the first time.
        //
        // The cost is that this one tool bypasses the reader cache and the
        // shared pool (`describe_recording_output` opens with a pool of its
        // own). It is a metadata read called once or twice a session, so a
        // separate open is worth strict parity with the CLI — and it is one
        // open, not two: the listing decision and the reader now come out of
        // the same call.
        // `Json`: every selector this renders is going back to an MCP client,
        // which has no `--recording` flag to type — it sends a `recording`
        // object in the tool call. This tool's schema is what points an agent
        // here to discover the arms, so CLI syntax would end the discovery
        // path in an instruction the client cannot follow.
        let output = crate::mcp::describe_recording_output(
            path,
            &selector,
            crate::mcp::SelectorSyntax::Json,
        )?;
        Ok(output)
    }

    /// Read the optional `recording` object from a tool call's arguments.
    ///
    /// Absent means "no selector", which is not the same as "any recording":
    /// over a multi-recording archive it resolves as ambiguous and the caller
    /// is told to choose. That is the point — the alternative is answering
    /// from an arm nobody named.
    fn selector_of(
        arguments: &Value,
    ) -> Result<crate::mcp::RecordingSelector, Box<dyn std::error::Error>> {
        match arguments.get("recording") {
            // An explicit `null` is an ABSENT selector, not a malformed one.
            // Many MCP clients and LLM tool-call serializers spell an omitted
            // optional property as `"recording": null`, and `get` hands that
            // back as `Some(Value::Null)`; passing it to `from_json` would
            // fail a valid single-recording call with "recording must be an
            // object of label key to value".
            None | Some(serde_json::Value::Null) => Ok(crate::mcp::RecordingSelector::default()),
            Some(v) => Ok(crate::mcp::RecordingSelector::from_json(v)?),
        }
    }

    /// Load or get a cached reader for `parquet_file`, honoring `selector`.
    async fn get_reader_selected(
        &self,
        parquet_file: &str,
        selector: &crate::mcp::RecordingSelector,
    ) -> Result<Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
        self.get_reader_with_provenance(parquet_file, selector)
            .await
            .map(|(reader, _)| reader)
    }

    /// As `get_reader_selected`, also reporting what the open determined
    /// about the recording's provenance — needed only by `extract_features`,
    /// whose sampler attribution depends on it.
    async fn get_reader_with_provenance(
        &self,
        parquet_file: &str,
        selector: &crate::mcp::RecordingSelector,
    ) -> Result<
        (
            Arc<dyn metriken_query::MetricsSource>,
            crate::analysis::extract::Provenance,
        ),
        Box<dyn std::error::Error>,
    > {
        let path_key = cache_path(parquet_file);
        let key = (path_key.clone(), selector.clone());
        {
            let cache = self.reader_cache.read().unwrap();
            if let Some(hit) = cache.get(&key) {
                return Ok((Arc::clone(&hit.source), hit.provenance));
            }
        }

        // A missing file is reported by the opener (one message for both
        // front ends), not pre-checked here.
        let path = Path::new(parquet_file);

        // The same open the one-shot CLI uses — including the selector, and
        // the refusal of a multi-recording archive that names none. Server
        // mode is what an AI agent actually drives, so any behavior added on
        // only one of the two paths diverges exactly as the first
        // multi-recording refusal did: the CLI refused while the server
        // answered `extract_features` with every metric `NoData` over a
        // 2-recording archive. `open_source_with_pool_labeled` also does the
        // `.rez`-vs-parquet dispatch by content, so it replaces the whole
        // branch.
        let opened = crate::mcp::open_source_with_pool_labeled(
            path,
            Arc::clone(&self.pool),
            selector,
            crate::mcp::SelectorSyntax::Json,
        )?;
        let provenance = opened.provenance();
        let reader = opened.reader;
        let identity = opened.labels.clone();

        let mut cache = self.reader_cache.write().unwrap();
        // Distinct selectors can name the SAME recording (`source=redis` and
        // `host=web-01 source=redis`), and an LLM client will emit both across
        // a session. Collapse them onto one reader by the recording's own
        // identity so the archive is not retained once per spelling. Matching
        // on the path too: an identity is only meaningful within one file, and
        // an archive with no labels at all has the empty map as its identity.
        let source = cache
            .iter()
            .find(|((p, _), c)| *p == path_key && c.identity == identity)
            .map(|(_, c)| Arc::clone(&c.source))
            .unwrap_or(reader);
        cache.insert(
            key,
            CachedReader {
                identity,
                source: Arc::clone(&source),
                provenance,
            },
        );

        Ok((source, provenance))
    }

    /// Analyze correlation between two metrics
    async fn analyze_correlation(
        &self,
        arguments: &Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        let metric1 = arguments
            .get("metric1")
            .and_then(|m| m.as_str())
            .ok_or("Missing metric1")?;

        let metric2 = arguments
            .get("metric2")
            .and_then(|m| m.as_str())
            .ok_or("Missing metric2")?;

        let selector = Self::selector_of(arguments)?;
        let reader = self.get_reader_selected(parquet_file, &selector).await?;

        use crate::mcp::correlation::{calculate_correlation, format_correlation_result};

        let result = calculate_correlation(reader.as_ref(), metric1, metric2)?;
        Ok(format_correlation_result(&result))
    }

    /// Describe all metrics available in a parquet file
    async fn describe_metrics(
        &self,
        arguments: &Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        let selector = Self::selector_of(arguments)?;
        let reader = self.get_reader_selected(parquet_file, &selector).await?;

        use crate::mcp::describe_metrics::format_metrics_description;
        Ok(format_metrics_description(reader.as_ref()))
    }

    /// Detect anomalies in time series data
    async fn detect_anomalies(
        &self,
        arguments: &Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        let query = arguments
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("Missing query")?;

        let selector = Self::selector_of(arguments)?;
        let reader = self.get_reader_selected(parquet_file, &selector).await?;

        use crate::mcp::anomaly_detection::{detect_anomalies, format_anomaly_detection_result};

        let result = detect_anomalies(reader.as_ref(), query)?;
        Ok(format_anomaly_detection_result(&result))
    }

    /// Execute a PromQL query and return results as JSON
    async fn execute_query(&self, arguments: &Value) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        let query = arguments
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("Missing query")?;

        let selector = Self::selector_of(arguments)?;
        let reader = self.get_reader_selected(parquet_file, &selector).await?;

        let (start_time, end_time) = reader.time_range().unwrap_or((0.0, 0.0));
        let step = 1.0;

        let result = reader.query_range(query, start_time, end_time, step)?;

        Ok(serde_json::to_string_pretty(&result)?)
    }

    /// Extract structured features from a recording and return the overview
    /// record as JSON
    async fn execute_extract_features(
        &self,
        arguments: &Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;

        let selector = Self::selector_of(arguments)?;
        let (reader, provenance) = self
            .get_reader_with_provenance(parquet_file, &selector)
            .await?;
        let record = crate::analysis::extract::extract(reader.as_ref(), provenance)?;
        Ok(serde_json::to_string_pretty(&record)?)
    }

    /// Drop every cached reader of `parquet_file`: a write changed the
    /// recording's metadata, and a reader opened before it would report the
    /// old events (and KPIs) to the next tool call.
    fn evict(&self, parquet_file: &str) {
        let path = cache_path(parquet_file);
        let mut cache = self.reader_cache.write().unwrap();
        cache.retain(|(p, _), _| *p != path);
    }

    /// `add_event`: build one `Event` from the arguments and append it to
    /// the recording the selector names.
    async fn add_event(&self, arguments: &Value) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;
        let selector = Self::selector_of(arguments)?;
        let timestamp = timestamp_ns_of(arguments.get("timestamp").ok_or("Missing timestamp")?)?;
        let description = opt_str(arguments, "description")?.ok_or("Missing description")?;
        let duration_ns = match arguments.get("duration") {
            None | Some(Value::Null) => None,
            Some(v) => Some(duration_ns_of(v)?),
        };
        let id = opt_str(arguments, "id")?
            .unwrap_or_else(|| format!("mcp:{}", crate::agent::epoch::mint()));
        let event = crate::viewer::Event {
            timestamp,
            description,
            kind: opt_str(arguments, "kind")?,
            details: opt_str(arguments, "details")?,
            // Agent-authored unless the caller says otherwise, so a person can
            // filter (or `remove_events` by source) what the agent wrote.
            source: Some(opt_str(arguments, "source")?.unwrap_or_else(|| "mcp".to_string())),
            node: opt_str(arguments, "node")?,
            instance: opt_str(arguments, "instance")?,
            labels: Default::default(),
            duration_ns,
            id: Some(id.clone()),
            chart_id: None,
        };
        let report = crate::parquet_tools::events::add_events_selected(
            Path::new(parquet_file),
            &selector,
            vec![event],
        )?;
        self.evict(parquet_file);
        let outcome = if report.counts.new > 0 {
            "added"
        } else if report.counts.updated > 0 {
            "updated"
        } else {
            "unchanged (an event with this id was already present)"
        };
        Ok(serde_json::to_string_pretty(&json!({
            "id": id,
            "timestamp_ns": timestamp,
            "duration_ns": duration_ns,
            "outcome": outcome,
            "recording": report.recording,
            "events_in_recording": report.total,
            "file": parquet_file,
            // The writer's own line, which is where a v1/v2 tar archive
            // says it was upgraded to v3 on the way.
            "report": report.report,
        }))?)
    }

    /// `remove_events`: mutating, so only with the flag.
    async fn remove_events(&self, arguments: &Value) -> Result<String, Box<dyn std::error::Error>> {
        if !self.options.allow_mutating {
            return Err(mutating_refused("remove_events").into());
        }
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;
        let selector = Self::selector_of(arguments)?;
        let ids: Vec<String> = match arguments.get("ids") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or("ids must be strings"))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("ids must be an array of strings".into()),
        };
        let filter = crate::parquet_tools::events::RemoveFilter {
            ids,
            kind: opt_str(arguments, "kind")?,
            source: opt_str(arguments, "source")?,
        };
        let report = crate::parquet_tools::events::remove_events_selected(
            Path::new(parquet_file),
            &selector,
            &filter,
        )?;
        self.evict(parquet_file);
        Ok(serde_json::to_string_pretty(&json!({
            "removed": report.removed,
            "recording": report.recording,
            "events_in_recording": report.total,
            "file": parquet_file,
            "report": report.report,
        }))?)
    }

    /// `run_checks`: the same evaluation as `rezolus recording check`,
    /// returned as JSON; with `annotate` the verdicts are written too.
    async fn run_checks(&self, arguments: &Value) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;
        let selector = Self::selector_of(arguments)?;
        let override_ext: Option<crate::viewer::ServiceExtension> = match arguments.get("queries") {
            None | Some(Value::Null) => None,
            Some(v @ Value::Object(_)) => Some(
                serde_json::from_value(v.clone())
                    .map_err(|e| format!("queries is not a ServiceExtension object: {e}"))?,
            ),
            Some(_) => return Err("queries must be a ServiceExtension object".into()),
        };
        let annotate = match arguments.get("annotate") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("annotate must be a boolean".into()),
        };
        let path = Path::new(parquet_file);
        let registry = crate::viewer::load_template_registry(None);
        let run = crate::parquet_tools::check::run_checks(
            path,
            &selector,
            override_ext.as_ref(),
            &registry,
        )?;
        if run.checks_seen == 0 {
            return Ok(serde_json::to_string_pretty(&json!({
                "checks": [],
                "summary": run.summary(),
                "message": run.nothing_to_run(path, override_ext.is_some(), "queries"),
            }))?);
        }
        let mut annotated: Option<String> = None;
        if annotate {
            let n: usize = run.per_recording_events.iter().map(Vec::len).sum();
            annotated = Some(if n == 0 {
                "nothing to annotate: no violation windows".to_string()
            } else {
                let line = crate::parquet_tools::check::annotate_events(
                    path,
                    run.format,
                    run.per_recording_events.clone(),
                )?;
                self.evict(parquet_file);
                line
            });
        }
        Ok(serde_json::to_string_pretty(&json!({
            "checks": run.results,
            "summary": run.summary(),
            "exit_code": run.exit_code(),
            "annotated": annotated,
        }))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::QueryResult;

    #[test]
    fn test_mcp_tool_from_str_query() {
        assert!(matches!(McpTool::from("query"), McpTool::Query));
    }

    #[test]
    fn test_mcp_tool_from_str_extract_features() {
        assert!(matches!(
            McpTool::from("extract_features"),
            McpTool::ExtractFeatures
        ));
    }

    #[test]
    fn test_mcp_tool_from_str_unknown() {
        assert!(matches!(McpTool::from("nonexistent"), McpTool::Unknown(_)));
    }

    #[tokio::test]
    async fn test_execute_query_missing_parquet_file() {
        let server = Server::new();
        let args = json!({"query": "cpu_cores"});
        let result = server.execute_query(&args).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Missing parquet_file"));
    }

    #[tokio::test]
    async fn test_execute_query_missing_query() {
        let server = Server::new();
        let args = json!({"parquet_file": "/some/file.parquet"});
        let result = server.execute_query(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing query"));
    }

    #[tokio::test]
    async fn test_execute_query_nonexistent_file() {
        let server = Server::new();
        let args = json!({
            "parquet_file": "/nonexistent/file.parquet",
            "query": "cpu_cores"
        });
        let result = server.execute_query(&args).await;
        assert!(result.is_err());
    }

    /// Server mode must refuse a multi-recording archive exactly as the
    /// one-shot CLI does.
    ///
    /// This is the half that was missed the first time. The server had its
    /// own `detect_rez_format` + `open_with_pool` branch, so the CLI refused
    /// while the stdio server — the mode an AI agent actually drives —
    /// answered `extract_features` over a 2-recording archive with every
    /// metric `NoData`, empty correlations, and a `duration_s` that was the
    /// union of two unrelated timelines. Both paths now share
    /// `mcp::open_source_with_pool_labeled`.
    #[tokio::test]
    async fn get_reader_refuses_a_multi_recording_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let server = Server::new();
        let err = server
            .get_reader_selected(
                path.to_str().unwrap(),
                &crate::mcp::RecordingSelector::default(),
            )
            .await
            .err()
            .expect("server mode must refuse it, not answer NoData for every metric")
            .to_string();
        assert!(
            err.contains("2 recordings"),
            "and must say why, as the CLI does: {err}"
        );
    }

    /// The server's open must dispatch a v3 (SQLite) `.rez` to `RezReader`, not
    /// `ParquetReader::open_with_pool`. Mutation check: reverting the
    /// `detect_rez_format` check to `is_rez_path` makes this fail — a v3 file
    /// then falls through to `ParquetReader`, which errors on the SQLite
    /// header instead of opening the archive.
    #[tokio::test]
    async fn get_reader_opens_v3_sqlite_rez() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("rec.rez");
        crate::recorder::rez::recorder_tests_support::empty_v3_rez(&rez_path);
        assert_eq!(
            crate::recorder::rez::detect_rez_format(&rez_path).unwrap(),
            crate::recorder::rez::RezFormat::V3Sqlite,
            "fixture sanity: must actually be a v3 SQLite archive"
        );

        let server = Server::new();
        let result = server
            .get_reader_selected(
                rez_path.to_str().unwrap(),
                &crate::mcp::RecordingSelector::default(),
            )
            .await;
        assert!(
            result.is_ok(),
            "the server must accept a v3 .rez: {:?}",
            result.err().map(|e| e.to_string())
        );
    }

    #[test]
    fn test_query_result_scalar_json_format() {
        let result = QueryResult::Scalar {
            result: (1704067200.0, 42.0),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"resultType\":\"scalar\""));
        assert!(json.contains("\"result\":[1704067200.0,42.0]"));
    }

    #[test]
    fn test_query_result_vector_json_format() {
        use metriken_query::Sample;
        use std::collections::HashMap;

        let mut metric = HashMap::new();
        metric.insert("__name__".to_string(), "cpu_cores".to_string());

        let result = QueryResult::Vector {
            result: vec![Sample::new(metric, (1704067200.0, 4.0))],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"resultType\":\"vector\""));
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"metric\""));
        assert!(json.contains("\"value\""));
    }

    #[test]
    fn test_query_result_matrix_json_format() {
        use metriken_query::MatrixSample;
        use std::collections::HashMap;

        let mut metric = HashMap::new();
        metric.insert("__name__".to_string(), "cpu_cycles".to_string());

        let result = QueryResult::Matrix {
            result: vec![MatrixSample::new(
                metric,
                vec![(1704067200.0, 2.5e9), (1704067201.0, 2.6e9)],
            )],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"resultType\":\"matrix\""));
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"metric\""));
        assert!(json.contains("\"values\""));
    }

    #[tokio::test]
    async fn the_server_honors_a_recording_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let server = Server::new();
        let args = serde_json::json!({
            "parquet_file": path.to_str().unwrap(),
            "recording": {"source": "valkey"}
        });
        let out = server
            .describe_recording(&args)
            .await
            .expect("a selector must work in server mode too");
        assert!(out.contains("Recording Information"), "{out}");
        // Not just "it opened something": the report must be ABOUT the arm
        // that was named. A handler that resolved the selector and then read
        // the other recording would still print a well-formed report.
        assert!(
            out.contains("valkey") && !out.contains("redis"),
            "the report must describe the named arm: {out}"
        );
    }

    /// The cache is keyed by path; two recordings from one archive must not
    /// collide. Without the recording in the key, the second request returns
    /// the first's reader and answers about the wrong arm.
    #[tokio::test]
    async fn two_recordings_of_one_archive_do_not_share_a_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let p = path.to_str().unwrap();

        let server = Server::new();
        let redis = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["source=redis".to_string()])
                    .unwrap(),
            )
            .await
            .unwrap();
        let valkey = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["source=valkey".to_string()])
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(redis.metadata_get("source").as_deref(), Some("redis"));
        assert_eq!(
            valkey.metadata_get("source").as_deref(),
            Some("valkey"),
            "the second request must not be served the first's cached reader"
        );
    }

    /// Two selectors that name the SAME recording share one reader.
    ///
    /// The cache is keyed by the resolved recording's identity, not by the
    /// selector text, so `source=valkey` and `host=web-01 source=valkey` —
    /// which an LLM client will realistically emit for the same arm across
    /// two calls — do not each retain their own copy of the archive.
    #[tokio::test]
    async fn equivalent_selectors_share_one_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let p = path.to_str().unwrap();

        let server = Server::new();
        let narrow = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["source=valkey".to_string()])
                    .unwrap(),
            )
            .await
            .unwrap();
        let wide = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse(
                    "--recording",
                    ["source=valkey".to_string(), "host=web-01".to_string()],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            Arc::ptr_eq(&narrow, &wide),
            "two selectors naming one recording must not retain two readers"
        );
    }

    /// Two recordings whose OLD flattened identity would collide must not
    /// share a cached reader.
    ///
    /// The dedup once rendered a recording's labels to a `\u{1}`-joined `k=v`
    /// string, which aliases: `{x: "a\u{1}y=b"}` and `{x: "a", y: "b"}` render
    /// the same bytes, so the second lookup would return the first's reader and
    /// answer about the wrong recording. Keying on the label MAP cannot alias.
    /// Reachable only with an operator-chosen label value carrying a `\u{1}` —
    /// nobody types it by accident, but it is the same wrong-answer-looking-
    /// right species this arc kept closing behind unusual inputs.
    #[tokio::test]
    async fn recordings_whose_flattened_identity_collides_do_not_share_a_reader() {
        use std::collections::BTreeMap;
        let a: BTreeMap<String, String> = [("x".to_string(), "a\u{1}y=b".to_string())]
            .into_iter()
            .collect();
        let b: BTreeMap<String, String> = [
            ("x".to_string(), "a".to_string()),
            ("y".to_string(), "b".to_string()),
        ]
        .into_iter()
        .collect();
        // Precondition: the OLD identity really would have conflated them.
        assert_eq!(
            crate::recorder::seal_policy::recording_stagger_key(&a),
            crate::recorder::seal_policy::recording_stagger_key(&b),
            "fixture must actually exercise the aliasing the fix removes",
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collide.rez");
        crate::mcp::tests::multi_recording_rez_with_labels(&path, &[a, b]);
        let p = path.to_str().unwrap();

        let server = Server::new();
        // Select each uniquely: `y=b` names only B, the whole odd value names A.
        let ra = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["x=a\u{1}y=b".to_string()])
                    .unwrap(),
            )
            .await
            .unwrap();
        let rb = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["y=b".to_string()]).unwrap(),
            )
            .await
            .unwrap();
        assert!(
            !Arc::ptr_eq(&ra, &rb),
            "two distinct recordings must not be conflated by a flattened identity"
        );
    }

    /// Every selector the SERVER renders must be in the syntax an MCP client
    /// can actually send.
    ///
    /// The client has no `--recording` flag — its interface is
    /// `{"recording": {"source": "redis"}}` — so a listing that says
    /// `select with: --recording source=redis` hands an agent an instruction
    /// it cannot follow. `describe_recording` is where the schema explicitly
    /// sends an agent to discover the arms ("Call describe_recording without
    /// it first to list them"), so that path ending in unusable syntax is a
    /// dead end from "I can't read this archive" onward.
    #[tokio::test]
    async fn server_listings_render_the_json_selector_not_the_cli_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let args = json!({"parquet_file": path.to_str().unwrap()});

        let server = Server::new();
        let listing = server
            .describe_recording(&args)
            .await
            .expect("no selector lists the arms");
        assert!(
            !listing.contains("--recording"),
            "the MCP client has no flags to type: {listing}"
        );
        assert!(
            listing.contains(r#"recording {"source": "redis"}"#),
            "it must render the tool argument the client can send: {listing}"
        );
    }

    /// ...and so must every error the server returns, not just the listing.
    #[tokio::test]
    async fn server_errors_render_the_json_selector_not_the_cli_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let p = path.to_str().unwrap();
        let server = Server::new();

        // No selector: ambiguous over both arms.
        let ambiguous = server
            .get_reader_selected(p, &crate::mcp::RecordingSelector::default())
            .await
            .err()
            .expect("a 2-recording archive must be refused")
            .to_string();
        assert!(!ambiguous.contains("--recording"), "{ambiguous}");
        assert!(
            ambiguous.contains(r#"recording {"source": "redis"}"#),
            "{ambiguous}"
        );

        // A selector that names nothing: the echo of the selector itself must
        // be in the client's syntax too.
        let no_match = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse("--recording", ["source=nope".to_string()])
                    .unwrap(),
            )
            .await
            .err()
            .expect("source=nope names no arm")
            .to_string();
        assert!(!no_match.contains("--recording"), "{no_match}");
        assert!(
            no_match.contains(r#"recording {"source": "nope"}"#),
            "the echoed selector must be pasteable as a tool argument: {no_match}"
        );
    }

    /// An explicit `"recording": null` means "no selector", not an error.
    ///
    /// Many MCP clients and LLM tool-call serializers emit an explicit null
    /// for an omitted optional property. `arguments.get("recording")` returns
    /// `Some(Value::Null)` for that, which `from_json` rejects as "must be an
    /// object" — turning a perfectly valid single-recording call into a hard
    /// failure for a client that did nothing wrong.
    #[tokio::test]
    async fn an_explicit_null_recording_means_no_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);

        let server = Server::new();
        let args = json!({"parquet_file": path.to_str().unwrap(), "recording": null});
        let out = server
            .describe_recording(&args)
            .await
            .expect("an explicit null is an omitted selector, not a bad one");
        assert!(out.contains("Recording Information"), "{out}");
    }

    /// The missing-file message is the opener's, so it is the same one the
    /// CLI prints (see `a_missing_file_says_so_rather_than_failing_in_the_decoder`).
    #[tokio::test]
    async fn a_missing_file_reports_the_same_way_as_the_cli() {
        let server = Server::new();
        let err = server
            .get_reader_selected(
                "/nonexistent/does-not-exist.rez",
                &crate::mcp::RecordingSelector::default(),
            )
            .await
            .err()
            .expect("a missing file cannot open")
            .to_string();
        assert!(err.contains("Recording file not found"), "{err}");
    }

    /// A handler that honors `recording` is invisible if the schema never
    /// advertises it: an MCP client sends only what the schema declares, so a
    /// tool missing the property would leave an agent unable to name an arm
    /// and told only that the archive holds two.
    #[tokio::test]
    async fn every_tool_schema_advertises_the_recording_argument() {
        // With the flag, so the mutating tier is under the same check.
        let mut server = Server::with_options(ServerOptions {
            allow_mutating: true,
        });
        let listing = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await
            .unwrap()
            .expect("tools/list must answer");
        let tools = listing["result"]["tools"].as_array().unwrap().clone();
        assert_eq!(
            tools.len(),
            9,
            "six read tools, two additive, one mutating must be listed"
        );
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let props = &tool["inputSchema"]["properties"];
            assert!(
                props.get("recording").is_some(),
                "{name} does not advertise a recording selector"
            );
            // Optional on purpose: a single-recording archive — the common
            // case — must stay callable without one.
            let required = tool["inputSchema"]["required"].as_array().unwrap();
            assert!(
                !required.iter().any(|r| r == "recording"),
                "{name} must not require a selector"
            );
        }
    }

    /// Trap 1, mechanized: EVERY handler that opens a reader has to pass the
    /// selector down. One that quietly dropped it would fall back to "no
    /// selector", which over a 2-recording archive is the ambiguity error —
    /// and that error is exactly what an agent would see instead of an answer.
    ///
    /// Asserting "not the ambiguity error" rather than "Ok" on purpose: some
    /// of these tools legitimately fail on a 3-row fixture (extract_features
    /// wants 10s of data), and that failure is not the one under test.
    #[tokio::test]
    async fn every_handler_honors_the_recording_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let p = path.to_str().unwrap();

        let server = Server::new();
        let args = json!({
            "parquet_file": p,
            "recording": {"source": "valkey"},
            "query": "cpu_cycles",
            "metric1": "cpu_cycles",
            "metric2": "cpu_cycles",
        });

        type Outcome = Result<String, Box<dyn std::error::Error>>;
        let outcomes: Vec<(&str, Outcome)> = vec![
            ("describe_recording", server.describe_recording(&args).await),
            (
                "analyze_correlation",
                server.analyze_correlation(&args).await,
            ),
            ("describe_metrics", server.describe_metrics(&args).await),
            ("detect_anomalies", server.detect_anomalies(&args).await),
            ("query", server.execute_query(&args).await),
            (
                "extract_features",
                server.execute_extract_features(&args).await,
            ),
        ];
        // Collected, not asserted in the loop: a handler-by-handler report is
        // what makes this useful when one of the six is missed, and asserting
        // eagerly would hide the other five behind the first.
        let dropped: Vec<&str> = outcomes
            .into_iter()
            .filter(|(_, outcome)| {
                outcome
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.to_string().contains("recordings with data"))
            })
            .map(|(name, _)| name)
            .collect();
        assert!(
            dropped.is_empty(),
            "these handlers dropped the recording selector: {dropped:?}"
        );
    }

    // ── write tools ──────────────────────────────────────────────────────

    fn tool_names(server: &mut Server) -> Vec<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reply = rt
            .block_on(
                server.handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
            )
            .unwrap()
            .unwrap();
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn stored_events(path: &std::path::Path) -> Vec<Vec<crate::viewer::Event>> {
        let db = crate::recorder::rez_sqlite::RezDb::open(path).unwrap();
        db.read_recordings()
            .unwrap()
            .into_iter()
            .map(|r| {
                r.meta
                    .metadata
                    .get(crate::parquet_metadata::KEY_EVENTS)
                    .map(|s| {
                        serde_json::from_str::<crate::viewer::Events>(s)
                            .unwrap()
                            .events
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn the_flag_decides_whether_mutating_tools_are_listed() {
        let mut off = Server::new();
        let names = tool_names(&mut off);
        assert!(names.contains(&"add_event".to_string()));
        assert!(names.contains(&"run_checks".to_string()));
        assert!(
            !names.contains(&"remove_events".to_string()),
            "a server without --allow-mutating must not advertise removals: {names:?}"
        );
        let mut on = Server::with_options(ServerOptions {
            allow_mutating: true,
        });
        assert!(tool_names(&mut on).contains(&"remove_events".to_string()));
    }

    #[tokio::test]
    async fn a_mutating_call_without_the_flag_is_refused_naming_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let mut server = Server::new();
        let reply = server
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "remove_events", "arguments": {"parquet_file": path.to_str().unwrap(), "kind": "x"}}
            }))
            .await
            .unwrap()
            .unwrap();
        let msg = reply["error"]["message"].as_str().unwrap();
        assert!(msg.contains("--allow-mutating"), "{msg}");
        assert!(reply.get("result").is_none());
    }

    #[tokio::test]
    async fn add_event_lands_in_the_recording_and_is_idempotent_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::new();
        let args = json!({
            "parquet_file": path.to_str().unwrap(),
            "timestamp": "2026-09-28T14:03:11.250Z",
            "description": "p99 spike",
            "kind": "finding",
            "duration": "30s",
            "details": "sum(rate(x[1m])) doubled",
        });
        let out: Value = serde_json::from_str(&server.add_event(&args).await.unwrap()).unwrap();
        let id = out["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("mcp:"), "{id}");
        assert_eq!(out["outcome"], "added");
        assert_eq!(out["events_in_recording"], 1);
        assert_eq!(out["duration_ns"], 30_000_000_000u64);

        let stored = stored_events(&path);
        assert_eq!(stored.len(), 1);
        let e = &stored[0][0];
        assert_eq!(e.id.as_deref(), Some(id.as_str()));
        assert_eq!(
            e.source.as_deref(),
            Some("mcp"),
            "agent-authored by default"
        );
        assert_eq!(e.kind.as_deref(), Some("finding"));
        assert_eq!(e.timestamp, 1_790_604_191_250_000_000);
        assert_eq!(e.duration_ns, Some(30_000_000_000));

        // Same id again: unchanged, not duplicated.
        let mut again = args.clone();
        again["id"] = Value::String(id.clone());
        let out2: Value = serde_json::from_str(&server.add_event(&again).await.unwrap()).unwrap();
        assert!(out2["outcome"].as_str().unwrap().starts_with("unchanged"));
        assert_eq!(stored_events(&path)[0].len(), 1);

        // Unix seconds as a number, and no duration: an instant.
        let mut instant = args.clone();
        instant["timestamp"] = json!(1_790_690_600.5);
        instant.as_object_mut().unwrap().remove("duration");
        let out3: Value = serde_json::from_str(&server.add_event(&instant).await.unwrap()).unwrap();
        assert_eq!(out3["timestamp_ns"], 1_790_690_600_500_000_000u64);
        assert!(out3["duration_ns"].is_null());
        assert_eq!(stored_events(&path)[0].len(), 2);
    }

    /// The same digits mean seconds as a number and nanoseconds as a string
    /// under `annotate --event`'s convention; the tool refuses the string
    /// form and the out-of-range number rather than storing an event fifty
    /// years off with no error.
    #[tokio::test]
    async fn add_event_refuses_ambiguous_timestamps_and_durations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::new();
        let file = path.to_str().unwrap();
        let attempt = |ts: Value, dur: Option<Value>| {
            let mut a = json!({"parquet_file": file, "timestamp": ts, "description": "x"});
            if let Some(d) = dur {
                a["duration"] = d;
            }
            a
        };
        for (ts, dur, needle) in [
            (json!("1776804000"), None, "digit-only"),
            (json!(1_776_804_000_000_000_000u64), None, "year 5138"),
            (json!(1_776_804_000.0), Some(json!("30")), "digit-only"),
            (json!(1_776_804_000.0), Some(json!(2e9)), "31 years"),
            (json!(1_776_804_000.0), Some(json!(0)), "positive"),
        ] {
            let err = server
                .add_event(&attempt(ts.clone(), dur.clone()))
                .await
                .err()
                .unwrap_or_else(|| panic!("{ts} / {dur:?} must be refused"))
                .to_string();
            assert!(err.contains(needle), "{ts} / {dur:?}: {err}");
        }
        assert!(stored_events(&path)[0].is_empty(), "nothing was written");
        // The unambiguous forms still work: RFC 3339, seconds, humantime.
        server
            .add_event(&attempt(
                json!("2026-04-21T20:00:00Z"),
                Some(json!("1m30s")),
            ))
            .await
            .unwrap();
        server
            .add_event(&attempt(json!(1_776_804_000), Some(json!(90))))
            .await
            .unwrap();
        let stored = stored_events(&path);
        assert_eq!(stored[0].len(), 2);
        assert!(stored[0]
            .iter()
            .all(|e| e.duration_ns == Some(90_000_000_000)));
    }

    /// The cache is keyed by canonical path, so a write through one spelling
    /// evicts a reader opened through another.
    #[tokio::test]
    async fn a_write_through_another_spelling_of_the_path_evicts_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::new();
        // `dir/./one.rez` and `dir/one.rez` name one file.
        let dotted = dir.path().join(".").join("one.rez");
        server
            .get_reader_selected(
                dotted.to_str().unwrap(),
                &crate::mcp::RecordingSelector::default(),
            )
            .await
            .unwrap();
        assert_eq!(server.reader_cache.read().unwrap().len(), 1);
        server
            .add_event(&json!({"parquet_file": path.to_str().unwrap(), "timestamp": 1.0, "description": "x"}))
            .await
            .unwrap();
        assert!(server.reader_cache.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_event_writes_only_the_selected_recording_and_refuses_an_ambiguous_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);
        let server = Server::new();
        let base = json!({
            "parquet_file": path.to_str().unwrap(),
            "timestamp": "2026-09-28T14:03:11Z",
            "description": "seen in valkey",
        });
        let err = server.add_event(&base).await.err().unwrap().to_string();
        assert!(
            err.contains("2 recordings"),
            "must list, never stamp every arm: {err}"
        );
        assert!(
            err.contains("\"source\": \"redis\"") || err.contains("source"),
            "{err}"
        );
        assert!(stored_events(&path).iter().all(Vec::is_empty));

        let mut chosen = base.clone();
        chosen["recording"] = json!({"source": "valkey"});
        let out: Value = serde_json::from_str(&server.add_event(&chosen).await.unwrap()).unwrap();
        assert_eq!(out["recording"]["source"], "valkey");
        let stored = stored_events(&path);
        assert!(stored[0].is_empty(), "redis untouched");
        assert_eq!(stored[1].len(), 1);
    }

    #[tokio::test]
    async fn remove_events_filters_by_id_kind_and_source_and_refuses_an_empty_filter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::with_options(ServerOptions {
            allow_mutating: true,
        });
        let file = path.to_str().unwrap();
        let add = |ts: &str, kind: &str, source: Option<&str>, id: &str| {
            let mut a = json!({"parquet_file": file, "timestamp": ts, "description": kind, "kind": kind, "id": id});
            if let Some(s) = source {
                a["source"] = Value::String(s.to_string());
            }
            a
        };
        server
            .add_event(&add("2026-09-28T14:00:00Z", "deploy", Some("ops"), "d1"))
            .await
            .unwrap();
        server
            .add_event(&add("2026-09-28T14:01:00Z", "finding", None, "f1"))
            .await
            .unwrap();
        server
            .add_event(&add("2026-09-28T14:02:00Z", "finding", None, "f2"))
            .await
            .unwrap();
        server
            .add_event(&add("2026-09-28T14:03:00Z", "finding", Some("ops"), "f3"))
            .await
            .unwrap();
        assert_eq!(stored_events(&path)[0].len(), 4);

        let err = server
            .remove_events(&json!({"parquet_file": file}))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("--clear-events"), "{err}");

        // kind AND source: only the agent-written findings.
        let out: Value = serde_json::from_str(
            &server
                .remove_events(&json!({"parquet_file": file, "kind": "finding", "source": "mcp"}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(out["removed"], 2);
        assert_eq!(out["events_in_recording"], 2);
        let ids: Vec<String> = stored_events(&path)[0]
            .iter()
            .map(|e| e.id.clone().unwrap())
            .collect();
        assert_eq!(ids, vec!["d1", "f3"]);

        // By id.
        let out: Value = serde_json::from_str(
            &server
                .remove_events(&json!({"parquet_file": file, "ids": ["d1", "nope"]}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(out["removed"], 1);
        assert_eq!(stored_events(&path)[0].len(), 1);

        // Removing the last one drops the key rather than storing an empty list.
        server
            .remove_events(&json!({"parquet_file": file, "ids": ["f3"]}))
            .await
            .unwrap();
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recs = db.read_recordings().unwrap();
        assert!(!recs[0]
            .meta
            .metadata
            .contains_key(crate::parquet_metadata::KEY_EVENTS));
    }

    #[tokio::test]
    async fn run_checks_reports_nothing_to_run_or_each_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::new();
        let file = path.to_str().unwrap();

        let out: Value = serde_json::from_str(
            &server
                .run_checks(&json!({"parquet_file": file}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(out["checks"].as_array().unwrap().len(), 0);
        assert!(out["message"]
            .as_str()
            .unwrap()
            .contains("no checks to run"));

        // A check over a metric the fixture does not hold: the verdict is an
        // error naming the query, not a pass.
        let queries = json!({
            "service_name": "t",
            "kpis": [{
                "role": "q",
                "title": "absent",
                "query": "sum(rate(no_such_metric[1m]))",
                "type": "gauge",
                "check": {"above": 1.0}
            }]
        });
        let out: Value = serde_json::from_str(
            &server
                .run_checks(&json!({"parquet_file": file, "queries": queries, "annotate": true}))
                .await
                .unwrap(),
        )
        .unwrap();
        let checks = out["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["status"], "error");
        assert_eq!(out["summary"]["error"], 1);
        assert_eq!(out["exit_code"], 2);
        assert!(out["annotated"]
            .as_str()
            .unwrap()
            .contains("nothing to annotate"));

        let err = server
            .run_checks(&json!({"parquet_file": file, "queries": "kpis.json"}))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("ServiceExtension object"), "{err}");
    }

    #[tokio::test]
    async fn a_write_evicts_the_cached_reader_for_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        crate::mcp::tests::multi_recording_rez(&path, &["redis"], &[true]);
        let server = Server::new();
        let file = path.to_str().unwrap();
        server
            .get_reader_selected(file, &crate::mcp::RecordingSelector::default())
            .await
            .unwrap();
        assert_eq!(server.reader_cache.read().unwrap().len(), 1);
        server
            .add_event(&json!({"parquet_file": file, "timestamp": 1.0, "description": "x"}))
            .await
            .unwrap();
        assert!(
            server.reader_cache.read().unwrap().is_empty(),
            "a reader opened before the write would report the old events"
        );
    }
}
