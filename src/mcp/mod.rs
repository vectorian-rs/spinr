//! MCP (Model Context Protocol) types and JSON-RPC 2.0 support
//!
//! Combined protocol types for MCP server implementations.

#![allow(dead_code)]

pub mod stdio;
pub mod transport;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 ──

/// Standard JSON-RPC 2.0 error codes
pub mod error_codes {
    /// Parse error - Invalid JSON was received
    pub const PARSE_ERROR: i32 = -32700;
    /// Invalid Request - The JSON sent is not a valid Request object
    pub const INVALID_REQUEST: i32 = -32600;
    /// Method not found - The method does not exist / is not available
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid params - Invalid method parameter(s)
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal error - Internal JSON-RPC error
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// JSON-RPC 2.0 request
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version (always "2.0")
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Request ID (can be null for notifications)
    pub id: Option<Value>,
    /// Method name
    pub method: String,
    /// Method parameters
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Protocol version (always "2.0")
    pub jsonrpc: &'static str,
    /// Request ID (matches the request)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Result (mutually exclusive with error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error (mutually exclusive with result)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code
    pub code: i32,
    /// Error message
    pub message: String,
    /// Additional error data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// Create a successful response
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Create an error response with additional data
    pub fn error_with_data(
        id: Option<Value>,
        code: i32,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }

    /// Create a parse error response
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::error(None, error_codes::PARSE_ERROR, message)
    }

    /// Create a method not found error response
    pub fn method_not_found(id: Option<Value>) -> Self {
        Self::error(id, error_codes::METHOD_NOT_FOUND, "Method not found")
    }

    /// Create an invalid params error response
    pub fn invalid_params(id: Option<Value>, message: impl Into<String>) -> Self {
        Self::error(id, error_codes::INVALID_PARAMS, message)
    }

    /// Create an internal error response
    pub fn internal_error(id: Option<Value>, message: impl Into<String>) -> Self {
        Self::error(id, error_codes::INTERNAL_ERROR, message)
    }
}

impl JsonRpcError {
    /// Create a new error
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

// ── MCP Protocol ──

/// MCP tool definition for tools/list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// JSON Schema for tool input
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl McpTool {
    /// Create a new tool definition
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// MCP server info for initialize response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    /// Server name
    pub name: String,
    /// Server version
    pub version: String,
}

impl McpServerInfo {
    /// Create server info from package metadata
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// MCP server capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCapabilities {
    /// Tool capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Resource capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Value>,
    /// Prompt capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Value>,
}

impl McpCapabilities {
    /// Create capabilities with tools enabled
    pub fn with_tools() -> Self {
        Self {
            tools: Some(serde_json::json!({})),
            resources: None,
            prompts: None,
        }
    }
}

/// Content item in tool result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContent {
    /// Content type (usually "text")
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content
    pub text: String,
}

impl ToolContent {
    /// Create text content
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content_type: "text".to_string(),
            text: content.into(),
        }
    }
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content array
    pub content: Vec<ToolContent>,
    /// Whether this is an error result
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ToolResult {
    /// Create a successful text result
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::text(text)],
            is_error: None,
        }
    }

    /// Create an error result
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::text(format!("Error: {}", message.into()))],
            is_error: Some(true),
        }
    }

    /// Convert to JSON Value for response
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// MCP protocol version
pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_creation() {
        let tool = McpTool::new(
            "test_tool",
            "A test tool",
            json!({
                "type": "object",
                "properties": {}
            }),
        );
        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");
    }

    #[test]
    fn test_server_info() {
        let info = McpServerInfo::new("my-server", "1.0.0");
        assert_eq!(info.name, "my-server");
        assert_eq!(info.version, "1.0.0");
    }

    #[test]
    fn test_capabilities() {
        let caps = McpCapabilities::with_tools();
        assert!(caps.tools.is_some());
        assert!(caps.resources.is_none());
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("Operation completed");
        assert!(result.is_error.is_none());
        assert_eq!(result.content[0].text, "Operation completed");
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("Something went wrong");
        assert_eq!(result.is_error, Some(true));
        assert!(result.content[0].text.contains("Error:"));
    }

    #[test]
    fn test_success_response() {
        let response = JsonRpcResponse::success(Some(json!(1)), json!({"result": "ok"}));
        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.error.is_none());
        assert!(response.result.is_some());
    }

    #[test]
    fn test_error_response() {
        let response = JsonRpcResponse::error(Some(json!(1)), -32600, "Invalid Request");
        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.result.is_none());
        let error = response.error.unwrap();
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "Invalid Request");
    }

    #[test]
    fn test_parse_error() {
        let response = JsonRpcResponse::parse_error("Unexpected token");
        assert!(response.id.is_none());
        assert_eq!(
            response.error.as_ref().unwrap().code,
            error_codes::PARSE_ERROR
        );
    }

    #[test]
    fn test_method_not_found() {
        let response = JsonRpcResponse::method_not_found(Some(json!(42)));
        assert_eq!(
            response.error.as_ref().unwrap().code,
            error_codes::METHOD_NOT_FOUND
        );
    }

    #[test]
    fn test_request_deserialization() {
        let json_str = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{}}"#;
        let request: JsonRpcRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(request.method, "test");
        assert_eq!(request.id, Some(json!(1)));
    }

    #[test]
    fn test_request_without_params() {
        let json_str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request: JsonRpcRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(request.method, "ping");
        assert_eq!(request.params, Value::Null);
    }
}
