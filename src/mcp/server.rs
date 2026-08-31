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
    /// The chosen recording's label set, canonically rendered by
    /// `recording_stagger_key`. Two selectors that name the same recording of
    /// the same file share the reader under this identity rather than each
    /// retaining their own copy of the archive.
    identity: String,
    source: Arc<dyn metriken_query::MetricsSource>,
}

/// MCP server state
pub struct Server {
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
    pub fn new() -> Self {
        Self {
            reader_cache: Arc::new(RwLock::new(HashMap::new())),
            pool: BufferPool::new(MCP_CACHE_SIZE_BYTES),
        }
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
                        "tools": [
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
                        ]
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

        // Kept ahead of the open so a typo'd path says so plainly; without
        // it the failure arrives as whatever the parquet decoder makes of a
        // missing file.
        let path = Path::new(parquet_file);
        if !path.exists() {
            return Err(format!("Recording file not found: {parquet_file}").into());
        }

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
        // own). It is a metadata read called once or twice a session, so the
        // duplicated open is worth strict parity with the CLI.
        let output = crate::mcp::describe_recording_output(path, &selector)?;
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
            Some(v) => Ok(crate::mcp::RecordingSelector::from_json(v)?),
            None => Ok(crate::mcp::RecordingSelector::default()),
        }
    }

    /// Load or get a cached reader for `parquet_file`, honoring `selector`.
    async fn get_reader_selected(
        &self,
        parquet_file: &str,
        selector: &crate::mcp::RecordingSelector,
    ) -> Result<Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
        let key = (parquet_file.to_string(), selector.clone());
        {
            let cache = self.reader_cache.read().unwrap();
            if let Some(hit) = cache.get(&key) {
                return Ok(Arc::clone(&hit.source));
            }
        }

        let path = Path::new(parquet_file);
        if !path.exists() {
            return Err(format!("Recording file not found: {parquet_file}").into());
        }

        // The same open the one-shot CLI uses — including the selector, and
        // the refusal of a multi-recording archive that names none. Server
        // mode is what an AI agent actually drives, so any behavior added on
        // only one of the two paths diverges exactly as the first
        // multi-recording refusal did: the CLI refused while the server
        // answered `extract_features` with every metric `NoData` over a
        // 2-recording archive. `open_source_with_pool_labeled` also does the
        // `.rez`-vs-parquet dispatch by content, so it replaces the whole
        // branch.
        let (labels, reader) =
            crate::mcp::open_source_with_pool_labeled(path, Arc::clone(&self.pool), selector)?;
        let identity = crate::recorder::seal_policy::recording_stagger_key(&labels);

        let mut cache = self.reader_cache.write().unwrap();
        // Distinct selectors can name the SAME recording (`source=redis` and
        // `host=web-01 source=redis`), and an LLM client will emit both across
        // a session. Collapse them onto one reader by the recording's own
        // identity so the archive is not retained once per spelling. Matching
        // on the path too: an identity is only meaningful within one file, and
        // an archive with no labels at all renders the empty identity.
        let source = cache
            .iter()
            .find(|((p, _), c)| p == parquet_file && c.identity == identity)
            .map(|(_, c)| Arc::clone(&c.source))
            .unwrap_or(reader);
        cache.insert(
            key,
            CachedReader {
                identity,
                source: Arc::clone(&source),
            },
        );

        Ok(source)
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
        let reader = self.get_reader_selected(parquet_file, &selector).await?;
        let record = crate::analysis::extract::extract(reader.as_ref())?;
        Ok(serde_json::to_string_pretty(&record)?)
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
                &crate::mcp::RecordingSelector::parse(["source=redis".to_string()]).unwrap(),
            )
            .await
            .unwrap();
        let valkey = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse(["source=valkey".to_string()]).unwrap(),
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
                &crate::mcp::RecordingSelector::parse(["source=valkey".to_string()]).unwrap(),
            )
            .await
            .unwrap();
        let wide = server
            .get_reader_selected(
                p,
                &crate::mcp::RecordingSelector::parse([
                    "source=valkey".to_string(),
                    "host=web-01".to_string(),
                ])
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            Arc::ptr_eq(&narrow, &wide),
            "two selectors naming one recording must not retain two readers"
        );
    }

    /// A handler that honors `recording` is invisible if the schema never
    /// advertises it: an MCP client sends only what the schema declares, so a
    /// tool missing the property would leave an agent unable to name an arm
    /// and told only that the archive holds two.
    #[tokio::test]
    async fn every_tool_schema_advertises_the_recording_argument() {
        let mut server = Server::new();
        let listing = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await
            .unwrap()
            .expect("tools/list must answer");
        let tools = listing["result"]["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 6, "all six tools must be listed");
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
}
