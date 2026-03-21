pub(crate) mod bench;
mod cli;
mod loadtest;
mod mcp;
mod output;
mod trace;

use cli::{LoadTestCommand, SpinrArgs, SubCommand, TraceCommand};
use mcp::stdio::ToolMode;
use std::collections::HashMap;
use std::net::SocketAddr;

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: SpinrArgs = argh::from_env();

    // Engine child mode: read config from stdin pipe, run mio event loop, write metrics to stdout
    if args.run_engine {
        loadtest::orchestrator::run_engine_child()?;
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

    // Bench subcommand: no async runtime needed
    if let Some(SubCommand::Bench(ref cmd)) = args.command {
        bench::run_bench(&cmd.config, cmd.json)?;
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
        Some(SubCommand::Bench(_)) => {
            // Handled before tokio runtime is created in main()
            unreachable!()
        }
        None => {
            // No subcommand and no --mcp: show help
            eprintln!("Usage: spinr <command> [options]");
            eprintln!();
            eprintln!("Commands:");
            eprintln!("  trace      Trace HTTP requests with detailed timing");
            eprintln!("  load-test  Run HTTP load tests (wrk2-style)");
            eprintln!("  bench      Run multi-scenario benchmarks from TOML config");
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
    let json = cmd.json;
    let params = bench::LoadTestParams::from_cli(&cmd)?;
    let metrics = bench::run_single_loadtest(&params, json)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&metrics).unwrap());
    } else {
        output::print_metrics(&metrics);
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
