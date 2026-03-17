//! CLI argument definitions using argh

use argh::FromArgs;

/// HTTP performance & debugging tool
#[derive(FromArgs, Debug)]
pub struct SpinrArgs {
    #[argh(subcommand)]
    pub command: Option<SubCommand>,

    /// run as MCP server (all tools, stdio transport)
    #[argh(switch)]
    pub mcp: bool,

    /// run as manager process (internal use)
    #[argh(option)]
    pub run_manager: Option<String>,

    /// run as worker process (internal use)
    #[argh(option)]
    pub run_worker: Option<String>,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum SubCommand {
    Trace(TraceCommand),
    LoadTest(LoadTestCommand),
}

/// Trace HTTP requests with detailed timing breakdown
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "trace")]
pub struct TraceCommand {
    /// URL(s) to trace
    #[argh(positional)]
    pub url: Vec<String>,

    /// HTTP method (default: GET)
    #[argh(option, short = 'm', default = "String::from(\"GET\")")]
    pub method: String,

    /// header in "Key: Value" format (repeatable)
    #[argh(option, short = 'H')]
    pub header: Vec<String>,

    /// request body
    #[argh(option, short = 'd')]
    pub data: Option<String>,

    /// HTTP version: 1.0, 1.1, or 2 (default: 1.1)
    #[argh(option, default = "String::from(\"1.1\")")]
    pub http_version: String,

    /// total timeout in seconds (default: 30)
    #[argh(option, default = "30")]
    pub timeout: u64,

    /// output as JSON
    #[argh(switch, short = 'j')]
    pub json: bool,

    /// run as MCP server instead of CLI
    #[argh(switch)]
    pub mcp: bool,

    /// MCP transport: "stdio" or "http" (default: stdio)
    #[argh(option, short = 't', default = "String::from(\"stdio\")")]
    pub transport: String,

    /// HTTP port for MCP HTTP transport (default: 3000)
    #[argh(option, short = 'p', default = "3000")]
    pub port: u16,

    /// HTTP host for MCP HTTP transport (default: 127.0.0.1)
    #[argh(option, default = "String::from(\"127.0.0.1\")")]
    pub host: String,
}

/// Run HTTP load tests (wrk2-style)
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "load-test")]
pub struct LoadTestCommand {
    /// target URL to test
    #[argh(positional)]
    pub url: String,

    /// requests per second
    #[argh(option, short = 'R', default = "100")]
    pub rate: u32,

    /// test duration in seconds (default: 10)
    #[argh(option, short = 'd', default = "10")]
    pub duration: u32,

    /// number of runtime threads (default: CPU count)
    #[argh(option, short = 't')]
    pub threads: Option<u32>,

    /// number of concurrent connections (default: 1, for --max-throughput)
    #[argh(option, short = 'c', default = "1")]
    pub connections: u32,

    /// HTTP method (default: GET)
    #[argh(option, short = 'm', default = "String::from(\"GET\")")]
    pub method: String,

    /// header in "Key: Value" format (repeatable)
    #[argh(option, short = 'H')]
    pub header: Vec<String>,

    /// request body
    #[argh(option, short = 'b')]
    pub body: Option<String>,

    /// maximum throughput mode (no rate limiting, wrk-style)
    #[argh(switch)]
    pub max_throughput: bool,

    /// warmup duration in seconds (default: 0)
    #[argh(option, short = 'w', default = "0")]
    pub warmup: u32,

    /// show latency distribution
    #[argh(switch)]
    #[allow(dead_code)]
    pub latency: bool,

    /// output as JSON
    #[argh(switch, short = 'j')]
    pub json: bool,

    /// run as MCP server instead of CLI
    #[argh(switch)]
    pub mcp: bool,

    /// MCP transport: "stdio" or "http" (default: stdio)
    #[argh(option, default = "String::from(\"stdio\")")]
    pub transport_type: String,

    /// HTTP port for MCP HTTP transport (default: 3000)
    #[argh(option, short = 'p', default = "3000")]
    pub port: u16,

    /// HTTP host for MCP HTTP transport (default: 127.0.0.1)
    #[argh(option, default = "String::from(\"127.0.0.1\")")]
    pub host: String,
}
