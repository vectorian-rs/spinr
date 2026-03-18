mod cli;
mod loadtest;
mod mcp;
mod output;
mod trace;

use cli::{LoadTestCommand, SpinrArgs, SubCommand, TraceCommand};
use loadtest::types::{HttpMethod, MergedMetrics};
use mcp::stdio::ToolMode;
use std::collections::HashMap;
use std::net::SocketAddr;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: SpinrArgs = argh::from_env();

    // Engine child mode: read config from stdin pipe, run mio event loop, write metrics to stdout
    if args.run_engine {
        loadtest::orchestrator::run_engine_child()?;
        return Ok(());
    }

    // Worker mode: run worker loop directly (no async runtime needed)
    if let Some(config_json) = args.run_worker {
        let config: loadtest::types::WorkerConfig = serde_json::from_str(&config_json)?;
        loadtest::worker::run(config)?;
        return Ok(());
    }

    // Manager mode: spawn workers and coordinate (no async runtime needed)
    if let Some(config_json) = args.run_manager {
        init_tracing();
        let config: loadtest::types::TestConfig = serde_json::from_str(&config_json)?;
        loadtest::manager::run(config)?;
        return Ok(());
    }

    // Top-level --mcp: expose all tools via stdio
    if args.mcp {
        init_tracing();
        // Install ring crypto provider for trace tool
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install ring crypto provider");
        mcp::stdio::run(ToolMode::All)?;
        return Ok(());
    }

    // Determine thread count for runtime
    let threads = match &args.command {
        Some(SubCommand::LoadTest(cmd)) if cmd.max_throughput => cmd.threads,
        _ => None,
    };

    let mut rt = tokio::runtime::Builder::new_multi_thread();
    rt.enable_all();
    if let Some(t) = threads {
        rt.worker_threads(t as usize);
    }
    let rt = rt.build()?;
    rt.block_on(async_main(args))
}

async fn async_main(args: SpinrArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match args.command {
        Some(SubCommand::Trace(cmd)) => {
            if cmd.mcp {
                init_tracing();
                rustls::crypto::ring::default_provider()
                    .install_default()
                    .expect("Failed to install ring crypto provider");
                run_mcp_server(ToolMode::TraceOnly, &cmd.transport, &cmd.host, cmd.port).await
            } else {
                rustls::crypto::ring::default_provider()
                    .install_default()
                    .expect("Failed to install ring crypto provider");
                run_trace_cli(cmd).await
            }
        }
        Some(SubCommand::LoadTest(cmd)) => {
            if cmd.mcp {
                init_tracing();
                run_mcp_server(
                    ToolMode::LoadTestOnly,
                    &cmd.transport_type,
                    &cmd.host,
                    cmd.port,
                )
                .await
            } else {
                run_loadtest_cli(cmd)
            }
        }
        None => {
            // No subcommand and no --mcp: show help
            eprintln!("Usage: spinr <command> [options]");
            eprintln!();
            eprintln!("Commands:");
            eprintln!("  trace      Trace HTTP requests with detailed timing");
            eprintln!("  load-test  Run HTTP load tests (wrk2-style)");
            eprintln!();
            eprintln!("Flags:");
            eprintln!("  --mcp      Start as MCP server (all tools)");
            eprintln!();
            eprintln!("Run 'spinr <command> --help' for more information.");
            std::process::exit(1);
        }
    }
}

/// Run trace in CLI mode
async fn run_trace_cli(cmd: TraceCommand) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if cmd.url.is_empty() {
        eprintln!("Error: at least one URL is required");
        std::process::exit(1);
    }

    // Parse headers
    let mut headers = HashMap::new();
    for h in &cmd.header {
        if let Some((name, value)) = h.split_once(':') {
            headers.insert(name.trim().to_string(), value.trim().to_string());
        } else {
            eprintln!("Invalid header format: '{}'. Use 'Name: Value'", h);
            std::process::exit(1);
        }
    }

    // Parse HTTP version
    let http_version = match cmd.http_version.as_str() {
        "1.0" => trace::types::HttpVersion::Http10,
        "1.1" => trace::types::HttpVersion::Http11,
        "2" => trace::types::HttpVersion::Http2,
        other => {
            eprintln!("Unknown HTTP version: {}. Use 1.0, 1.1, or 2", other);
            std::process::exit(1);
        }
    };

    for url in &cmd.url {
        let args = trace::types::TraceRequestArgs {
            url: url.clone(),
            method: cmd.method.clone(),
            headers: headers.clone(),
            body: cmd.data.clone(),
            timeout_secs: cmd.timeout,
            http_version,
        };

        match trace::tracer::trace_request(&args).await {
            Ok(result) => {
                if cmd.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("URL:            {}", result.url);
                    println!("Method:         {}", result.method);
                    println!("Status:         {}", result.response.status);
                    println!("Protocol:       {}", result.connection.protocol);
                    if let Some(ref tls) = result.connection.tls_version {
                        println!("TLS:            {}", tls);
                    }
                    println!();
                    println!("Timing:");
                    println!("  DNS lookup:   {}ms", result.timing.dns_lookup_ms);
                    println!("  TCP connect:  {}ms", result.timing.tcp_connect_ms);
                    println!("  TLS shake:    {}ms", result.timing.tls_handshake_ms);
                    println!("  TTFB:         {}ms", result.timing.time_to_first_byte_ms);
                    println!("  Transfer:     {}ms", result.timing.content_transfer_ms);
                    println!("  Total:        {}ms", result.timing.total_ms);
                    println!();
                    println!("Size:");
                    println!("  Request:      {} bytes", result.request_size);
                    println!("  Headers:      {} bytes", result.response.headers_size);
                    println!("  Body:         {} bytes", result.response.body_size);
                }
            }
            Err(e) => {
                eprintln!("Error tracing {}: {}", url, e);
            }
        }
    }

    Ok(())
}

/// Run load test in CLI mode (unified: both max-throughput and rate-limited)
fn run_loadtest_cli(cmd: LoadTestCommand) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::net::ToSocketAddrs;

    let method: HttpMethod = cmd.method.parse().map_err(|e: String| anyhow::anyhow!(e))?;

    // Parse headers
    let mut headers = HashMap::new();
    for h in &cmd.header {
        if let Some((name, value)) = h.split_once(':') {
            headers.insert(name.trim().to_string(), value.trim().to_string());
        } else {
            return Err(format!("Invalid header format: '{}'. Use 'Name: Value'", h).into());
        }
    }

    // Build pre-baked request bytes
    let prepared =
        loadtest::request::build_request_bytes(&cmd.url, method, &headers, cmd.body.as_deref())
            .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Resolve target address
    let authority = &prepared.remote_addr_authority;
    let addr_with_port = if authority.contains(':') {
        authority.clone()
    } else {
        format!("{}:80", authority)
    };
    let remote_addr: SocketAddr = addr_with_port
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("DNS resolution failed for {}", authority))?;

    let worker_count = cmd.threads.unwrap_or_else(|| num_cpus::get() as u32).max(1);
    let json = cmd.json;
    let hdr_log = cmd.hdr_log.clone();

    let mode = if cmd.max_throughput {
        loadtest::types::EngineMode::MaxThroughput
    } else {
        loadtest::types::EngineMode::RateLimited {
            requests_per_second: cmd.rate as u64,
            latency_correction: true,
        }
    };

    // Distribute connections across workers
    let total_connections = if cmd.max_throughput {
        cmd.connections.max(worker_count)
    } else {
        cmd.connections.max(1)
    };

    let mut configs = Vec::with_capacity(worker_count as usize);
    for worker_id in 0..worker_count {
        let base = total_connections / worker_count;
        let extra = if worker_id < (total_connections % worker_count) {
            1
        } else {
            0
        };
        let conns = (base + extra).max(1);

        configs.push(loadtest::types::EngineConfig {
            worker_id,
            remote_addr,
            method,
            connections: conns,
            duration_seconds: cmd.duration,
            warmup_seconds: cmd.warmup,
            mode: mode.clone(),
            read_buffer_size: 8192,
            verify_body: cmd.verify_body,
        });
    }

    // Print test info
    macro_rules! out {
        ($($arg:tt)*) => {
            if json { eprintln!($($arg)*); } else { println!($($arg)*); }
        };
    }
    if cmd.max_throughput {
        out!("Starting max-throughput test:");
    } else {
        out!("Starting load test:");
        out!("  Rate:        {} RPS", cmd.rate);
    }
    out!("  URL:         {}", cmd.url);
    out!("  Method:      {}", method);
    out!("  Target:      {}", remote_addr);
    out!("  Connections: {}", total_connections);
    out!("  Workers:     {}", worker_count);
    if cmd.warmup > 0 {
        out!("  Warmup:      {}s", cmd.warmup);
    }
    out!("  Duration:    {}s", cmd.duration);
    out!();

    // Run the test
    let raw_results = loadtest::orchestrator::run_workers(configs, &prepared.bytes)?;

    // Convert to public metrics
    let worker_metrics: Vec<_> = raw_results
        .into_iter()
        .map(|r| r.into_worker_metrics())
        .collect();
    let metrics = MergedMetrics::from_workers(&worker_metrics);

    if json {
        println!("{}", serde_json::to_string_pretty(&metrics).unwrap());
    } else {
        output::print_metrics(&metrics);
    }

    if let Some(ref path) = hdr_log {
        output::write_hdr_log(std::path::Path::new(path), &metrics)?;
        eprintln!("HDR Histogram log written to {}", path);
    }

    Ok(())
}

/// Run MCP server (stdio or HTTP transport)
async fn run_mcp_server(
    mode: ToolMode,
    transport: &str,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match transport {
        "stdio" => {
            tracing::info!("Using stdio transport");
            mcp::stdio::run(mode)?;
        }
        "http" => {
            let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
            tracing::info!("Using HTTP transport on {}", addr);
            // For HTTP transport, create a handler that dispatches to all tools
            let handler = SpinrHttpHandler::new(mode);
            mcp::transport::run_http_server(handler, addr).await?;
        }
        other => {
            eprintln!("Unknown transport: {}. Use 'stdio' or 'http'.", other);
            std::process::exit(1);
        }
    }
    Ok(())
}

/// HTTP handler that dispatches to trace and/or loadtest tools
#[derive(Clone)]
struct SpinrHttpHandler {
    mode: ToolMode,
    #[allow(dead_code)]
    state: std::sync::Arc<mcp::stdio::ServerState>,
}

impl SpinrHttpHandler {
    fn new(mode: ToolMode) -> Self {
        Self {
            mode,
            state: std::sync::Arc::new(mcp::stdio::ServerState::new()),
        }
    }
}

impl mcp::transport::McpHttpHandler for SpinrHttpHandler {
    fn server_name(&self) -> &str {
        "spinr"
    }

    fn server_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn handle_tools_list(&self) -> mcp::JsonRpcResponse {
        let mut tools = Vec::new();

        if matches!(self.mode, ToolMode::TraceOnly | ToolMode::All) {
            tools.push(mcp::McpTool::new(
                "trace_request",
                "Trace an HTTP request with detailed timing breakdown",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "URL to request" }
                    },
                    "required": ["url"]
                }),
            ));
        }

        if matches!(self.mode, ToolMode::LoadTestOnly | ToolMode::All) {
            tools.push(mcp::McpTool::new(
                "start_load_test",
                "Start a new HTTP load test",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target_url": { "type": "string" },
                        "total_rate": { "type": "integer" },
                        "duration_seconds": { "type": "integer" }
                    },
                    "required": ["target_url", "total_rate", "duration_seconds"]
                }),
            ));
            tools.push(mcp::McpTool::new(
                "stop_load_test",
                "Stop the currently running load test",
                serde_json::json!({ "type": "object", "properties": {} }),
            ));
            tools.push(mcp::McpTool::new(
                "get_status",
                "Get the status of the current or last load test",
                serde_json::json!({ "type": "object", "properties": {} }),
            ));
        }

        mcp::JsonRpcResponse::success(None, serde_json::json!({ "tools": tools }))
    }

    fn handle_tools_call(
        &self,
        id: Option<serde_json::Value>,
        params: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = mcp::JsonRpcResponse> + Send + '_>>
    {
        let tool_name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        Box::pin(async move {
            match tool_name.as_str() {
                "trace_request" if matches!(self.mode, ToolMode::TraceOnly | ToolMode::All) => {
                    let args: trace::TraceRequestArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => {
                            return mcp::JsonRpcResponse::success(
                                id,
                                serde_json::json!({
                                    "content": [{"type": "text", "text": format!("Error: Invalid arguments: {}", e)}],
                                    "isError": true
                                }),
                            );
                        }
                    };
                    match trace::tracer::trace_request(&args).await {
                        Ok(result) => mcp::JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{"type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default()}]
                            }),
                        ),
                        Err(e) => mcp::JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Error: {}", e)}],
                                "isError": true
                            }),
                        ),
                    }
                }
                _ => mcp::JsonRpcResponse::success(
                    id,
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!("Error: Unknown tool: {}", tool_name)}],
                        "isError": true
                    }),
                ),
            }
        })
    }
}

/// Initialize tracing to stderr
fn init_tracing() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spinr=info".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_timer(tracing_subscriber::fmt::time::UtcTime::new(
                    kiters::timestamp::get_utc_formatter(),
                )),
        )
        .init();
}
