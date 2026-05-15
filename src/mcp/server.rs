use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use metriken_query_sql::DuckDbBackend;

use crate::viewer::sql_capture::SqlCapture;

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
            other => McpTool::Unknown(other.to_string()),
        }
    }
}

/// MCP server state.
///
/// One shared [`DuckDbBackend`] for the lifetime of the server — it
/// maintains its own per-source connection pool keyed by parquet
/// path, so we don't need a separate cache.
///
/// `capture_cache` memoises the per-parquet metadata (interval, time
/// range, catalog) so repeated tool calls against the same file
/// don't re-read parquet KV. `Arc<SqlCapture>` is cheap to clone
/// across handler invocations.
pub struct Server {
    backend: Arc<DuckDbBackend>,
    capture_cache: Arc<RwLock<HashMap<String, Arc<SqlCapture>>>>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            backend: Arc::new(DuckDbBackend::new()),
            capture_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Run the MCP server using stdio
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

            // Try to parse as JSON-RPC message
            let message: Value = match serde_json::from_str(&line) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!("Failed to parse JSON: {e}");
                    continue;
                }
            };

            // Handle the message and get response
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
                                        }
                                    },
                                    "required": ["parquet_file"]
                                }
                            },
                            {
                                "name": "analyze_correlation",
                                "description": "Analyze correlation between two metrics. Each metric is either a bare metric name (auto-resolved to the canonical rate/sum/quantile SQL based on its kind) or a full DuckDB SQL string projecting `t DOUBLE, v DOUBLE`.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "parquet_file": {
                                            "type": "string",
                                            "description": "Path to the parquet file"
                                        },
                                        "metric1": {
                                            "type": "string",
                                            "description": "First metric name or DuckDB SQL"
                                        },
                                        "metric2": {
                                            "type": "string",
                                            "description": "Second metric name or DuckDB SQL"
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
                                        }
                                    },
                                    "required": ["parquet_file"]
                                }
                            },
                            {
                                "name": "detect_anomalies",
                                "description": "Detect anomalies using MAD, CUSUM, and Allan/Hadamard stability analysis. Call describe_metrics first to discover available metric names. The query collapses to a single time series.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "parquet_file": {
                                            "type": "string",
                                            "description": "Path to the parquet file"
                                        },
                                        "query": {
                                            "type": "string",
                                            "description": "Bare metric name (auto-resolved to canonical SQL — counter → sum of irate_1s, gauge → sum, histogram → p99) or full DuckDB SQL projecting `t DOUBLE, v DOUBLE`."
                                        }
                                    },
                                    "required": ["parquet_file", "query"]
                                }
                            },
                            {
                                "name": "query",
                                "description": "Execute a DuckDB SQL query against the recording (exposed as `_src`). Returns Prometheus-shaped matrix JSON when the projection has `t`/`v` columns; otherwise returns the empty-matrix response. Shared macros (irate_1s, rate_5m, hist_p99, …) are pre-registered.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "parquet_file": {
                                            "type": "string",
                                            "description": "Path to the parquet file"
                                        },
                                        "query": {
                                            "type": "string",
                                            "description": "DuckDB SQL string projecting `t DOUBLE, v <numeric>, labels...`. The parquet is exposed as `_src`."
                                        }
                                    },
                                    "required": ["parquet_file", "query"]
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
        let capture = self.get_capture(parquet_file).await?;
        Ok(super::format_recording_info_sql(parquet_file, &capture))
    }

    /// Load or get cached `SqlCapture` for a parquet path.
    /// Centralises the existence check + the eager metadata read, so
    /// every handler can rely on a warm capture.
    async fn get_capture(
        &self,
        parquet_file: &str,
    ) -> Result<Arc<SqlCapture>, Box<dyn std::error::Error>> {
        {
            let cache = self.capture_cache.read().unwrap();
            if let Some(cap) = cache.get(parquet_file) {
                return Ok(Arc::clone(cap));
            }
        }
        let path = Path::new(parquet_file);
        if !path.exists() {
            return Err(format!("Parquet file not found: {parquet_file}").into());
        }
        // Reuse the shared backend so the parquet's per-source pool
        // warms once and stays warm across handlers.
        let capture = SqlCapture::open(path, &self.backend)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        let arc = Arc::new(capture);
        {
            let mut cache = self.capture_cache.write().unwrap();
            cache.insert(parquet_file.to_string(), Arc::clone(&arc));
        }
        Ok(arc)
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

        let capture = self.get_capture(parquet_file).await?;
        let sql1 = super::resolve_query_to_sql(&capture, metric1).ok_or_else(|| {
            format!("metric1 '{metric1}' is not a recognised metric and doesn't look like SQL")
        })?;
        let sql2 = super::resolve_query_to_sql(&capture, metric2).ok_or_else(|| {
            format!("metric2 '{metric2}' is not a recognised metric and doesn't look like SQL")
        })?;

        use crate::mcp::correlation::{calculate_correlation_sql, format_correlation_result};
        let result = calculate_correlation_sql(&self.backend, &capture, &sql1, &sql2)?;
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
        let capture = self.get_capture(parquet_file).await?;
        Ok(crate::mcp::describe_metrics::format_metrics_description_sql(&capture))
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

        let capture = self.get_capture(parquet_file).await?;
        let result = super::detect_anomalies_for_input(&self.backend, &capture, query)?;
        use crate::mcp::anomaly_detection::format_anomaly_detection_result;
        Ok(format_anomaly_detection_result(&result))
    }

    /// Execute a DuckDB SQL query and return results as Prometheus-
    /// matrix JSON. The SQL is expected to project `t DOUBLE, v
    /// <numeric>, labels...` — `crates/prom-matrix` converts the
    /// Arrow output to the canonical resultType-shaped JSON the AI
    /// agent reads.
    async fn execute_query(&self, arguments: &Value) -> Result<String, Box<dyn std::error::Error>> {
        let parquet_file = arguments
            .get("parquet_file")
            .and_then(|f| f.as_str())
            .ok_or("Missing parquet_file")?;
        let query = arguments
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("Missing query")?;

        let capture = self.get_capture(parquet_file).await?;
        let path_str = capture.parquet_path().to_string_lossy().to_string();
        let batches = self.backend.run_sql(query, &path_str)?;
        Ok(prom_matrix::arrow_to_prom_matrix(&batches))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn demo_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join("viewer")
            .join("data")
            .join("demo.parquet")
    }

    #[test]
    fn test_mcp_tool_from_str_query() {
        assert!(matches!(McpTool::from("query"), McpTool::Query));
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing parquet_file")
        );
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
            "query": "SELECT 1"
        });
        let result = server.execute_query(&args).await;
        assert!(result.is_err());
    }

    /// `execute_query` returns Prometheus-shaped matrix JSON when the
    /// SQL projection has `t`/`v` columns. Pinned to lock the
    /// contract between the MCP server and AI agent consumers:
    /// `resultType` is `matrix`, and the `result` array carries one
    /// entry per series.
    #[tokio::test]
    async fn test_execute_query_returns_prom_matrix_json() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let server = Server::new();
        let args = json!({
            "parquet_file": path.to_string_lossy(),
            "query": "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, \"cpu_cores\"::DOUBLE AS v FROM _src LIMIT 3",
        });
        let response = server.execute_query(&args).await.expect("execute query");
        assert!(
            response.contains("\"resultType\":\"matrix\""),
            "expected resultType:matrix in: {response}",
        );
        // The single-series projection should have one `result` entry.
        assert!(
            response.contains("\"result\""),
            "no result field: {response}"
        );
    }

    /// Empty result projection still returns valid Prometheus matrix
    /// JSON (the canonical empty-matrix response) rather than
    /// erroring. Pinned to match the file-mode viewer's
    /// `/api/v1/query_range` UX.
    #[tokio::test]
    async fn test_execute_query_empty_result_returns_empty_matrix() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let server = Server::new();
        let args = json!({
            "parquet_file": path.to_string_lossy(),
            "query": "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, NULL AS v FROM _src WHERE FALSE",
        });
        let response = server.execute_query(&args).await.expect("execute query");
        assert!(
            response.contains("\"resultType\":\"matrix\""),
            "expected resultType:matrix even when empty: {response}",
        );
    }

    /// describe_recording produces the standard "Recording Information"
    /// header for an existing parquet. End-to-end check that the
    /// SqlCapture-backed handler path returns sensible output.
    #[tokio::test]
    async fn test_describe_recording_returns_metadata() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let server = Server::new();
        let args = json!({ "parquet_file": path.to_string_lossy() });
        let response = server
            .describe_recording(&args)
            .await
            .expect("describe_recording");
        assert!(
            response.contains("Recording Information"),
            "missing header: {response}"
        );
        assert!(
            response.contains("Source: rezolus"),
            "missing source: {response}"
        );
    }
}
