use super::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;

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

/// MCP server state
pub struct Server {
    tsdb_cache: Arc<RwLock<HashMap<String, Arc<Tsdb>>>>,
    query_engine_cache: Arc<RwLock<HashMap<String, Arc<QueryEngine>>>>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            tsdb_cache: Arc::new(RwLock::new(HashMap::new())),
            query_engine_cache: Arc::new(RwLock::new(HashMap::new())),
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
                                "description": "Analyze correlation between two metrics using PromQL",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "parquet_file": {
                                            "type": "string",
                                            "description": "Path to the parquet file"
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
                                        "query": {
                                            "type": "string",
                                            "description": "PromQL query expression. For COUNTERS use rate(metric[1m]), for GAUGES query directly, for HISTOGRAMS use histogram_quantile(0.99, metric). Use sum(), avg(), etc. to aggregate multiple series."
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

        let path = Path::new(parquet_file);
        if !path.exists() {
            return Err(format!("Parquet file not found: {parquet_file}").into());
        }

        // Load the TSDB
        let tsdb = Arc::new(Tsdb::load(path)?);

        // Create query engine
        use crate::viewer::promql::QueryEngine;
        let engine = QueryEngine::new(Arc::clone(&tsdb));

        // Use the shared formatting function
        let output = super::format_recording_info(parquet_file, &tsdb, &engine);
        Ok(output)
    }

    /// Load or get cached TSDB
    async fn get_tsdb(&self, parquet_file: &str) -> Result<Arc<Tsdb>, Box<dyn std::error::Error>> {
        // Check cache first
        {
            let cache = self.tsdb_cache.read().unwrap();
            if let Some(tsdb) = cache.get(parquet_file) {
                return Ok(Arc::clone(tsdb));
            }
        }

        // Load TSDB
        let path = Path::new(parquet_file);
        if !path.exists() {
            return Err(format!("Parquet file not found: {parquet_file}").into());
        }

        let tsdb = Arc::new(Tsdb::load(path)?);

        // Cache it
        {
            let mut cache = self.tsdb_cache.write().unwrap();
            cache.insert(parquet_file.to_string(), Arc::clone(&tsdb));
        }

        Ok(tsdb)
    }

    /// Load or get cached QueryEngine
    async fn get_query_engine(
        &self,
        parquet_file: &str,
    ) -> Result<Arc<QueryEngine>, Box<dyn std::error::Error>> {
        // Check cache first
        {
            let cache = self.query_engine_cache.read().unwrap();
            if let Some(engine) = cache.get(parquet_file) {
                return Ok(Arc::clone(engine));
            }
        }

        // Get or load TSDB
        let tsdb = self.get_tsdb(parquet_file).await?;
        let engine = Arc::new(QueryEngine::new(tsdb));

        // Cache the engine
        {
            let mut cache = self.query_engine_cache.write().unwrap();
            cache.insert(parquet_file.to_string(), Arc::clone(&engine));
        }

        Ok(engine)
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

        // Get cached or load TSDB and engine
        let tsdb = self.get_tsdb(parquet_file).await?;
        let engine = self.get_query_engine(parquet_file).await?;

        // Use the shared correlation module
        use crate::mcp::correlation::{calculate_correlation, format_correlation_result};

        let result = calculate_correlation(&engine, &tsdb, metric1, metric2)?;
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

        // Load the TSDB
        let tsdb = self.get_tsdb(parquet_file).await?;

        // Use the shared formatting function
        use crate::mcp::describe_metrics::format_metrics_description;
        Ok(format_metrics_description(&tsdb))
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

        // Get cached or load TSDB and engine
        let tsdb = self.get_tsdb(parquet_file).await?;
        let engine = self.get_query_engine(parquet_file).await?;

        // Use the anomaly detection module
        use crate::mcp::anomaly_detection::{detect_anomalies, format_anomaly_detection_result};

        let result = detect_anomalies(&engine, &tsdb, query)?;
        Ok(format_anomaly_detection_result(&result))
    }

    /// Execute a PromQL query and return results as JSON
    async fn execute_query(
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

        // Get cached or load TSDB and engine
        let engine = self.get_query_engine(parquet_file).await?;

        // Get time range from the recording
        let (start_time, end_time) = engine.get_time_range();
        let step = 1.0;

        // Execute query
        let result = engine.query_range(query, start_time, end_time, step)?;

        // Return as JSON string
        Ok(serde_json::to_string_pretty(&result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::promql::QueryResult;

    #[test]
    fn test_mcp_tool_from_str_query() {
        assert!(matches!(McpTool::from("query"), McpTool::Query));
    }

    #[test]
    fn test_mcp_tool_from_str_unknown() {
        assert!(matches!(
            McpTool::from("nonexistent"),
            McpTool::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn test_execute_query_missing_parquet_file() {
        let server = Server::new();
        let args = json!({"query": "cpu_cores"});
        let result = server.execute_query(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing parquet_file"));
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
        use std::collections::HashMap;
        use crate::viewer::promql::Sample;

        let mut metric = HashMap::new();
        metric.insert("__name__".to_string(), "cpu_cores".to_string());

        let result = QueryResult::Vector {
            result: vec![Sample {
                metric,
                value: (1704067200.0, 4.0),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"resultType\":\"vector\""));
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"metric\""));
        assert!(json.contains("\"value\""));
    }

    #[test]
    fn test_query_result_matrix_json_format() {
        use std::collections::HashMap;
        use crate::viewer::promql::MatrixSample;

        let mut metric = HashMap::new();
        metric.insert("__name__".to_string(), "cpu_cycles".to_string());

        let result = QueryResult::Matrix {
            result: vec![MatrixSample {
                metric,
                values: vec![(1704067200.0, 2.5e9), (1704067201.0, 2.6e9)],
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"resultType\":\"matrix\""));
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"metric\""));
        assert!(json.contains("\"values\""));
    }
}
