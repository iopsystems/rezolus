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
    Unknown(String),
}

impl From<&str> for McpTool {
    fn from(s: &str) -> Self {
        match s {
            "describe_recording" => McpTool::DescribeRecording,
            "analyze_correlation" => McpTool::AnalyzeCorrelation,
            other => McpTool::Unknown(other.to_string()),
        }
    }
}

/// MCP server state
pub struct Server {
    config: Arc<Config>,
    tsdb_cache: Arc<RwLock<HashMap<String, Arc<Tsdb>>>>,
    query_engine_cache: Arc<RwLock<HashMap<String, Arc<QueryEngine>>>>,
}

impl Server {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
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
                    debug!("Received message: {}", line);
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
                    warn!("Failed to parse JSON: {}", e);
                    continue;
                }
            };

            // Handle the message and get response
            if let Some(response) = self.handle_message(message).await? {
                let response_str = serde_json::to_string(&response)?;
                debug!("Sending response: {}", response_str);
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
                debug!("Unknown method: {}", method_name);
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
            return Err(format!("Parquet file not found: {}", parquet_file).into());
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
            return Err(format!("Parquet file not found: {}", parquet_file).into());
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

        // Get time range
        let (start, end) = engine.get_time_range();

        // Use the TSDB's native sampling interval
        let step = tsdb.interval();

        // Use the shared correlation module
        use crate::mcp::correlation::{calculate_correlation, format_correlation_result};

        let result = calculate_correlation(&engine, metric1, metric2, start, end, step)?;
        Ok(format_correlation_result(&result))
    }
}
