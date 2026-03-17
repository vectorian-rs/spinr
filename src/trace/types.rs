//! Type definitions for HTTP trace MCP server
//!
//! Request arguments and response types for HTTP timing analysis.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Supported HTTP versions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum HttpVersion {
    /// HTTP/1.0
    #[serde(rename = "1.0")]
    Http10,
    /// HTTP/1.1 (default)
    #[serde(rename = "1.1")]
    #[default]
    Http11,
    /// HTTP/2
    #[serde(rename = "2")]
    Http2,
}

impl std::fmt::Display for HttpVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpVersion::Http10 => write!(f, "HTTP/1.0"),
            HttpVersion::Http11 => write!(f, "HTTP/1.1"),
            HttpVersion::Http2 => write!(f, "HTTP/2"),
        }
    }
}

/// Arguments for trace_request tool
#[derive(Debug, Deserialize)]
pub struct TraceRequestArgs {
    /// URL to request
    pub url: String,
    /// HTTP method (default: GET)
    #[serde(default = "default_method")]
    pub method: String,
    /// Custom headers
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Request body for POST/PUT
    #[serde(default)]
    pub body: Option<String>,
    /// Total timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// HTTP version to use (default: 1.1)
    #[serde(default)]
    pub http_version: HttpVersion,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_timeout() -> u64 {
    30
}

/// Timing breakdown for each phase of the request
#[derive(Debug, Serialize, Default)]
pub struct TimingInfo {
    /// DNS lookup time in milliseconds
    pub dns_lookup_ms: u64,
    /// TCP connection time in milliseconds
    pub tcp_connect_ms: u64,
    /// TLS handshake time in milliseconds (0 for HTTP)
    pub tls_handshake_ms: u64,
    /// Time to first byte in milliseconds
    pub time_to_first_byte_ms: u64,
    /// Content transfer time in milliseconds
    pub content_transfer_ms: u64,
    /// Total request time in milliseconds
    pub total_ms: u64,
}

/// Response information
#[derive(Debug, Serialize)]
pub struct ResponseInfo {
    /// HTTP status code
    pub status: u16,
    /// Redirect target URL (for 3xx responses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
    /// Response headers
    pub headers: HashMap<String, String>,
    /// Response headers size in bytes
    pub headers_size: usize,
    /// Response body size in bytes
    pub body_size: usize,
    /// Preview of response body (first 500 chars)
    pub body_preview: String,
}

/// Connection information
#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    /// Remote IP address
    pub remote_ip: String,
    /// HTTP protocol version
    pub protocol: String,
    /// TLS version (if HTTPS)
    pub tls_version: Option<String>,
}

/// Complete trace result
#[derive(Debug, Serialize)]
pub struct TraceResult {
    /// Requested URL
    pub url: String,
    /// HTTP method used
    pub method: String,
    /// Request size in bytes (headers + body)
    pub request_size: usize,
    /// Timing breakdown
    pub timing: TimingInfo,
    /// Response information
    pub response: ResponseInfo,
    /// Connection details
    pub connection: ConnectionInfo,
}

/// Error result when trace fails
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct TraceError {
    /// Requested URL
    pub url: String,
    /// Error message
    pub error: String,
    /// Phase where error occurred
    pub phase: String,
    /// Partial timing (up to failure point)
    pub partial_timing: TimingInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_request_args_defaults() {
        let json = r#"{"url": "https://example.com"}"#;
        let args: TraceRequestArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.url, "https://example.com");
        assert_eq!(args.method, "GET");
        assert_eq!(args.timeout_secs, 30);
        assert!(args.headers.is_empty());
    }

    #[test]
    fn test_trace_request_args_full() {
        let json = r#"{
            "url": "https://api.example.com/data",
            "method": "POST",
            "headers": {"Authorization": "Bearer token"},
            "body": "{\"key\": \"value\"}",
            "timeout_secs": 10
        }"#;
        let args: TraceRequestArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.method, "POST");
        assert_eq!(args.timeout_secs, 10);
        assert!(args.headers.contains_key("Authorization"));
    }
}
