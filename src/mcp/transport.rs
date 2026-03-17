//! Streamable HTTP transport for MCP servers
//!
//! Implements the MCP Streamable HTTP transport specification with:
//! - Single endpoint supporting POST and GET
//! - SSE streams for server-to-client messages
//! - Session management with Mcp-Session-Id
//! - Security: Origin validation, localhost binding

use super::{JsonRpcRequest, JsonRpcResponse, McpCapabilities, McpServerInfo, PROTOCOL_VERSION};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap, convert::Infallible, future::Future, net::SocketAddr, pin::Pin,
    sync::Arc, time::Duration,
};
use tokio::sync::{RwLock, broadcast};
use tokio_stream::StreamExt;
use uuid::Uuid;

/// Trait for handling MCP requests over HTTP
pub trait McpHttpHandler: Send + Sync + 'static {
    /// Server name for initialization response
    fn server_name(&self) -> &str;

    /// Server version for initialization response
    fn server_version(&self) -> &str;

    /// Handle tools/list request
    fn handle_tools_list(&self) -> JsonRpcResponse;

    /// Handle tools/call request
    fn handle_tools_call(
        &self,
        id: Option<Value>,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = JsonRpcResponse> + Send + '_>>;
}

/// Session state for a connected client
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub created_at: time::OffsetDateTime,
    /// Broadcast channel for sending SSE events to this session
    pub tx: broadcast::Sender<SseMessage>,
}

/// Message sent over SSE
#[derive(Debug, Clone)]
pub struct SseMessage {
    pub event_id: String,
    pub data: String,
}

/// Shared application state for HTTP transport
pub struct HttpTransportState<H: McpHttpHandler> {
    pub handler: H,
    pub sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Allowed origins for CORS/security (None = allow localhost only)
    pub allowed_origins: Option<Vec<String>>,
}

impl<H: McpHttpHandler> HttpTransportState<H> {
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            allowed_origins: None,
        }
    }

    /// Create a new session and return the session ID
    pub async fn create_session(&self) -> String {
        let session_id = Uuid::new_v4().to_string();
        let (tx, _rx) = broadcast::channel(100);

        let session = Session {
            id: session_id.clone(),
            created_at: time::OffsetDateTime::now_utc(),
            tx,
        };

        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session);
        tracing::info!(session_id = %session_id, "Created new session");
        session_id
    }

    /// Get a session's broadcast sender by ID
    pub async fn get_session(&self, session_id: &str) -> Option<broadcast::Sender<SseMessage>> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|s| s.tx.clone())
    }

    /// Remove a session
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let removed = self.sessions.write().await.remove(session_id).is_some();
        if removed {
            tracing::info!(session_id = %session_id, "Session terminated");
        }
        removed
    }

    /// Check if session exists
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }
}

impl<H: McpHttpHandler> Clone for HttpTransportState<H>
where
    H: Clone,
{
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            sessions: self.sessions.clone(),
            allowed_origins: self.allowed_origins.clone(),
        }
    }
}

/// Custom header names for MCP
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// Run the HTTP transport server
pub async fn run_http_server<H: McpHttpHandler + Clone>(
    handler: H,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = HttpTransportState::new(handler);

    let app = Router::new()
        .route("/mcp", post(handle_post::<H>))
        .route("/mcp", get(handle_get::<H>))
        .route("/mcp", delete(handle_delete::<H>))
        .with_state(state);

    tracing::info!("Starting MCP HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Validate Origin header for security (DNS rebinding protection)
fn validate_origin(headers: &HeaderMap, allowed_origins: &Option<Vec<String>>) -> bool {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    match origin {
        None => true,
        Some(origin) => {
            if let Some(allowed) = allowed_origins {
                allowed.iter().any(|a| a == origin)
            } else {
                origin.starts_with("http://localhost")
                    || origin.starts_with("http://127.0.0.1")
                    || origin.starts_with("https://localhost")
                    || origin.starts_with("https://127.0.0.1")
            }
        }
    }
}

/// Extract session ID from headers
fn get_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(MCP_SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Handle POST requests - client sends JSON-RPC messages
async fn handle_post<H: McpHttpHandler + Clone>(
    State(state): State<HttpTransportState<H>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !validate_origin(&headers, &state.allowed_origins) {
        return (StatusCode::FORBIDDEN, "Invalid origin").into_response();
    }

    tracing::debug!(request = %body, "Received HTTP POST request");

    let request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse JSON-RPC request");
            let error = JsonRpcResponse::parse_error(format!("Parse error: {}", e));
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&error).unwrap_or_default(),
            )
                .into_response();
        }
    };

    tracing::info!(method = %request.method, id = ?request.id, "Processing HTTP request");

    if request.method == "initialize" {
        let session_id = state.create_session().await;
        let response = handle_initialize(&state.handler, request.id);
        let body = serde_json::to_string(&response).unwrap_or_default();

        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(MCP_SESSION_ID_HEADER, &session_id)
            .body(axum::body::Body::from(body))
            .unwrap()
            .into_response();
    }

    let session_id = get_session_id(&headers);
    if let Some(sid) = &session_id {
        if !state.session_exists(sid).await {
            tracing::warn!(session_id = %sid, "Stale session ID");
        }
    }

    let response = handle_json_rpc_request(&state, request).await;

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap_or_default(),
    )
        .into_response()
}

/// Handle initialize request
fn handle_initialize<H: McpHttpHandler>(handler: &H, id: Option<Value>) -> JsonRpcResponse {
    let server_info = McpServerInfo::new(handler.server_name(), handler.server_version());
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

/// Handle a JSON-RPC request
async fn handle_json_rpc_request<H: McpHttpHandler>(
    state: &HttpTransportState<H>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    match request.method.as_str() {
        "initialize" => handle_initialize(&state.handler, request.id),
        "tools/list" => {
            let mut response = state.handler.handle_tools_list();
            response.id = request.id;
            response
        }
        "tools/call" => {
            state
                .handler
                .handle_tools_call(request.id, request.params)
                .await
        }
        "ping" => JsonRpcResponse::success(request.id, json!({})),
        _ => JsonRpcResponse::method_not_found(request.id),
    }
}

/// Handle GET requests - opens SSE stream for server-to-client messages
async fn handle_get<H: McpHttpHandler + Clone>(
    State(state): State<HttpTransportState<H>>,
    headers: HeaderMap,
) -> Response {
    if !validate_origin(&headers, &state.allowed_origins) {
        return (StatusCode::FORBIDDEN, "Invalid origin").into_response();
    }

    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("text/event-stream") {
        return (StatusCode::NOT_ACCEPTABLE, "Must accept text/event-stream").into_response();
    }

    let session_id = match get_session_id(&headers) {
        Some(sid) => sid,
        None => return (StatusCode::BAD_REQUEST, "Missing Mcp-Session-Id header").into_response(),
    };

    let tx = match state.get_session(&session_id).await {
        Some(tx) => tx,
        None => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
    };

    let rx = tx.subscribe();

    let stream =
        tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|result| match result {
            Ok(msg) => Some(Ok::<_, Infallible>(
                Event::default().id(msg.event_id).data(msg.data),
            )),
            Err(_) => None,
        });

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(30))
                .text("ping"),
        )
        .into_response()
}

/// Handle DELETE requests - terminates session
async fn handle_delete<H: McpHttpHandler + Clone>(
    State(state): State<HttpTransportState<H>>,
    headers: HeaderMap,
) -> Response {
    if !validate_origin(&headers, &state.allowed_origins) {
        return (StatusCode::FORBIDDEN, "Invalid origin").into_response();
    }

    let session_id = match get_session_id(&headers) {
        Some(sid) => sid,
        None => return (StatusCode::BAD_REQUEST, "Missing Mcp-Session-Id header").into_response(),
    };

    if state.remove_session(&session_id).await {
        (StatusCode::OK, "Session terminated").into_response()
    } else {
        (StatusCode::NOT_FOUND, "Session not found").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_origin_no_header() {
        let headers = HeaderMap::new();
        assert!(validate_origin(&headers, &None));
    }

    #[test]
    fn test_validate_origin_localhost() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://localhost:3000".parse().unwrap());
        assert!(validate_origin(&headers, &None));
    }

    #[test]
    fn test_validate_origin_127() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://127.0.0.1:8080".parse().unwrap());
        assert!(validate_origin(&headers, &None));
    }

    #[test]
    fn test_validate_origin_external_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://evil.com".parse().unwrap());
        assert!(!validate_origin(&headers, &None));
    }

    #[test]
    fn test_validate_origin_allowed_list() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://myapp.com".parse().unwrap());
        let allowed = Some(vec!["https://myapp.com".to_string()]);
        assert!(validate_origin(&headers, &allowed));
    }
}
