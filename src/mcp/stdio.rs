//! Consolidated stdio JSON-RPC server for MCP
//!
//! Handles both trace and loadtest tools over a single stdio JSON-RPC loop.

use crate::loadtest::types::{MergedMetrics, StartLoadTestArgs, TestStatus};
use crate::mcp::{
    JsonRpcRequest, JsonRpcResponse, McpCapabilities, McpServerInfo, McpTool, PROTOCOL_VERSION,
};
use crate::trace;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;
use tokio::runtime::Runtime;

#[derive(Debug, thiserror::Error)]
pub enum McpStdioError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

/// Which tool set(s) this server exposes
#[derive(Debug, Clone, Copy)]
pub enum ToolMode {
    /// Only trace tools
    TraceOnly,
    /// Only load test tools
    LoadTestOnly,
    /// All tools
    All,
}

/// State shared across the MCP server (for load test tracking)
pub struct ServerState {
    /// Join handle for the load test thread
    pub join_handle: Mutex<Option<JoinHandle<Result<MergedMetrics, String>>>>,
    /// Status of current/last test
    pub status: Mutex<TestStatus>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            join_handle: Mutex::new(None),
            status: Mutex::new(TestStatus::default()),
        }
    }
}

/// Run the MCP stdio server loop
pub fn run(mode: ToolMode) -> Result<(), McpStdioError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let state = Arc::new(ServerState::new());

    // Create tokio runtime for async trace operations
    let runtime = Runtime::new()?;

    tracing::info!("MCP server ready (mode: {:?}), waiting for requests", mode);

    for line in stdin.lock().lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }

        tracing::debug!(request = %line, "Received JSON-RPC request");

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => runtime.block_on(handle_request(&state, mode, request)),
            Err(e) => {
                tracing::error!(error = %e, "Failed to parse JSON-RPC request");
                Some(JsonRpcResponse::parse_error(format!("Parse error: {}", e)))
            }
        };

        if let Some(response) = response {
            let output = serde_json::to_string(&response)?;
            writeln!(stdout, "{}", output)?;
            stdout.flush()?;
        }
    }

    Ok(())
}

/// Handle a single JSON-RPC request.
///
/// Returns `None` for JSON-RPC notifications (requests without an `id`),
/// which must not receive a response per the spec.
async fn handle_request(
    state: &Arc<ServerState>,
    mode: ToolMode,
    request: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    tracing::info!(method = %request.method, id = ?request.id, "Processing request");

    // JSON-RPC notifications (no id) must not receive a response
    if request.method.starts_with("notifications/") {
        if let Some(id) = request.id {
            // Has an id — treat as a regular request, respond with success
            return Some(JsonRpcResponse::success(Some(id), json!({})));
        }
        tracing::debug!(method = %request.method, "Notification received, no response sent");
        return None;
    }

    Some(match request.method.as_str() {
        "initialize" => handle_initialize(request.id),
        "tools/list" => handle_tools_list(mode, request.id),
        "tools/call" => handle_tools_call(state, mode, request.id, request.params).await,
        "ping" => JsonRpcResponse::success(request.id, json!({})),
        _ => JsonRpcResponse::method_not_found(request.id),
    })
}

/// Handle initialize request
fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    let server_info = McpServerInfo::new("spinr", env!("CARGO_PKG_VERSION"));
    let capabilities = McpCapabilities::with_tools();

    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": capabilities,
            "serverInfo": server_info
        }),
    )
}

/// Handle tools/list request
fn handle_tools_list(mode: ToolMode, id: Option<Value>) -> JsonRpcResponse {
    let mut tools = Vec::new();

    if matches!(mode, ToolMode::TraceOnly | ToolMode::All) {
        tools.push(trace_tool_definition());
    }

    if matches!(mode, ToolMode::LoadTestOnly | ToolMode::All) {
        tools.extend(loadtest_tool_definitions());
    }

    JsonRpcResponse::success(id, json!({ "tools": tools }))
}

/// Handle tools/call request
async fn handle_tools_call(
    state: &Arc<ServerState>,
    mode: ToolMode,
    id: Option<Value>,
    params: Value,
) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    tracing::info!(tool = %tool_name, "Executing tool");

    match tool_name {
        "trace_request" if matches!(mode, ToolMode::TraceOnly | ToolMode::All) => {
            handle_trace_request(id, arguments).await
        }
        "start_load_test" if matches!(mode, ToolMode::LoadTestOnly | ToolMode::All) => {
            wrap_result(id, handle_start_load_test(state, arguments))
        }
        "stop_load_test" if matches!(mode, ToolMode::LoadTestOnly | ToolMode::All) => {
            wrap_result(id, handle_stop_load_test(state))
        }
        "get_status" if matches!(mode, ToolMode::LoadTestOnly | ToolMode::All) => {
            wrap_result(id, handle_get_status(state))
        }
        _ => tool_error(id, &format!("Unknown tool: {}", tool_name)),
    }
}

// ── Trace tool ──

pub(crate) fn trace_tool_definition() -> McpTool {
    McpTool::new(
        "trace_request",
        "Trace an HTTP request with detailed timing breakdown for each phase: DNS lookup, TCP connect, TLS handshake, time to first byte, and content transfer.",
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to request (http:// or https://)"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "DELETE", "HEAD", "PATCH"],
                    "description": "HTTP method (default: GET)"
                },
                "headers": {
                    "type": "object",
                    "description": "Custom request headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                },
                "body": {
                    "type": "string",
                    "description": "Request body for POST/PUT requests"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Total timeout in seconds (default: 30)"
                },
                "http_version": {
                    "type": "string",
                    "enum": ["1.0", "1.1", "2"],
                    "description": "HTTP version to use: 1.0, 1.1, or 2 (default: 1.1)"
                }
            },
            "required": ["url"]
        }),
    )
}

async fn handle_trace_request(id: Option<Value>, arguments: Value) -> JsonRpcResponse {
    let args: trace::TraceRequestArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return tool_error(id, &format!("Invalid arguments: {}", e)),
    };

    tracing::info!(url = %args.url, method = %args.method, "Tracing HTTP request");

    match trace::tracer::trace_request(&args).await {
        Ok(result) => {
            tracing::info!(
                url = %result.url,
                status = result.response.status,
                total_ms = result.timing.total_ms,
                "Trace completed"
            );
            JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result).unwrap_or_default()
                    }]
                }),
            )
        }
        Err(e) => {
            tracing::error!(url = %args.url, error = %e, "Trace failed");
            tool_error(id, &e.to_string())
        }
    }
}

// ── Load test tools ──

pub(crate) fn loadtest_tool_definitions() -> Vec<McpTool> {
    vec![
        McpTool::new(
            "start_load_test",
            "Start a new HTTP load test. Fails if a test is already running.",
            json!({
                "type": "object",
                "properties": {
                    "target_url": {
                        "type": "string",
                        "description": "The HTTP URL to test"
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"],
                        "description": "HTTP method (default: GET)"
                    },
                    "headers": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "HTTP headers as key-value pairs"
                    },
                    "body": {
                        "type": "string",
                        "description": "Request body for POST/PUT/PATCH requests"
                    },
                    "total_rate": {
                        "type": "integer",
                        "description": "Total requests per second across all workers"
                    },
                    "process_count": {
                        "type": "integer",
                        "description": "Number of worker processes (default: CPU count)"
                    },
                    "duration_seconds": {
                        "type": "integer",
                        "description": "How long to run the test in seconds"
                    }
                },
                "required": ["target_url", "total_rate", "duration_seconds"]
            }),
        ),
        McpTool::new(
            "stop_load_test",
            "Stop the currently running load test. Note: tests run to completion and cannot be cancelled mid-flight.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        McpTool::new(
            "get_status",
            "Get the status of the current or last load test.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
    ]
}

pub(crate) fn handle_start_load_test(
    state: &Arc<ServerState>,
    args: Value,
) -> Result<String, String> {
    // Check if a test is already running
    {
        let handle = state.join_handle.lock().map_err(|e| e.to_string())?;
        if let Some(ref h) = *handle
            && !h.is_finished()
        {
            return Err(
                "A load test is already running. Stop it first with stop_load_test.".to_string(),
            );
        }
    }

    let args: StartLoadTestArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let target_url = args.target_url.clone();
    let method_str = args.method.clone().unwrap_or_else(|| "GET".to_string());
    let total_rate = args.total_rate;
    let duration_seconds = args.duration_seconds;
    let process_count = args.process_count;

    let params = args.into_load_test_params()?;

    let start_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| format_iso8601(d.as_secs()))
        .unwrap_or_else(|_| "unknown".to_string());

    // Update status to running
    {
        let mut status = state.status.lock().map_err(|e| e.to_string())?;
        *status = TestStatus {
            running: true,
            completed: None,
            start_time: Some(start_time.clone()),
            end_time: None,
            metrics: None,
        };
    }

    // Spawn a thread to run the load test
    let state_clone = Arc::clone(state);
    let handle = std::thread::spawn(move || {
        let result = crate::bench::run_single_loadtest(&params, true).map_err(|e| e.to_string());

        // Update status on completion
        let end_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| format_iso8601(d.as_secs()))
            .unwrap_or_else(|_| "unknown".to_string());

        if let Ok(mut status) = state_clone.status.lock() {
            status.running = false;
            status.completed = Some(result.is_ok());
            status.end_time = Some(end_time);
            if let Ok(ref metrics) = result {
                status.metrics = Some(metrics.clone());
            }
        }

        result
    });

    // Store the join handle
    {
        let mut handle_guard = state.join_handle.lock().map_err(|e| e.to_string())?;
        *handle_guard = Some(handle);
    }

    let response = json!({
        "started": true,
        "start_time": start_time,
        "config": {
            "target_url": target_url,
            "method": method_str,
            "total_rate": total_rate,
            "process_count": process_count,
            "duration_seconds": duration_seconds
        }
    });

    Ok(serde_json::to_string_pretty(&response).unwrap())
}

pub(crate) fn handle_stop_load_test(state: &Arc<ServerState>) -> Result<String, String> {
    let handle = state.join_handle.lock().map_err(|e| e.to_string())?;

    match *handle {
        Some(ref h) if !h.is_finished() => Err(
            "Load tests run to completion and cannot be cancelled mid-flight. \
             Wait for the test to finish or check status with get_status."
                .to_string(),
        ),
        Some(_) => {
            Err("No load test is currently running (last test already completed)".to_string())
        }
        None => Err("No load test is currently running".to_string()),
    }
}

pub(crate) fn handle_get_status(state: &Arc<ServerState>) -> Result<String, String> {
    // Check if the thread has finished and update status accordingly
    {
        let handle = state.join_handle.lock().map_err(|e| e.to_string())?;
        if let Some(ref h) = *handle
            && h.is_finished()
        {
            let mut status = state.status.lock().map_err(|e| e.to_string())?;
            status.running = false;
        }
    }

    let status = state.status.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::to_string_pretty(&*status).unwrap())
}

// ── Helpers ──

pub(crate) fn tool_error(id: Option<Value>, message: &str) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": format!("Error: {}", message)
            }],
            "isError": true
        }),
    )
}

pub(crate) fn wrap_result(id: Option<Value>, result: Result<String, String>) -> JsonRpcResponse {
    match result {
        Ok(content) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": content
                }]
            }),
        ),
        Err(e) => tool_error(id, &e),
    }
}

/// Format timestamp as ISO 8601
fn format_iso8601(secs: u64) -> String {
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;

    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut year = 1970u64;
    let mut remaining_days = days_since_epoch;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let (month, day) = day_of_year_to_month_day(remaining_days as u32, is_leap_year(year));

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn is_leap_year(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn day_of_year_to_month_day(day_of_year: u32, leap: bool) -> (u32, u32) {
    let days_in_months: [u32; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut remaining = day_of_year;
    for (i, &days) in days_in_months.iter().enumerate() {
        if remaining < days {
            return (i as u32 + 1, remaining + 1);
        }
        remaining -= days;
    }
    (12, 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_initialize() {
        let response = handle_initialize(Some(json!(1)));
        let result = response.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "spinr");
    }

    #[test]
    fn test_handle_tools_list_all() {
        let response = handle_tools_list(ToolMode::All, Some(json!(2)));
        let result = response.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4); // trace_request + 3 loadtest tools

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"trace_request"));
        assert!(names.contains(&"start_load_test"));
        assert!(names.contains(&"stop_load_test"));
        assert!(names.contains(&"get_status"));
    }

    #[test]
    fn test_handle_tools_list_trace_only() {
        let response = handle_tools_list(ToolMode::TraceOnly, Some(json!(3)));
        let result = response.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "trace_request");
    }

    #[test]
    fn test_handle_tools_list_loadtest_only() {
        let response = handle_tools_list(ToolMode::LoadTestOnly, Some(json!(4)));
        let result = response.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn test_iso8601_format() {
        let formatted = format_iso8601(1705321845);
        assert!(formatted.starts_with("2024-01-15"));
        assert!(formatted.contains("T"));
        assert!(formatted.ends_with("Z"));
    }
}
