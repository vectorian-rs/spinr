use argh::FromArgs;

/// HTTP performance & debugging tool
#[derive(FromArgs)]
struct Args {
    /// target URL to probe
    #[argh(positional)]
    url: String,

    /// HTTP method (GET, POST, PUT, DELETE, HEAD, OPTIONS)
    #[argh(option, short = 'm', default = "String::from(\"GET\")")]
    method: String,

    /// number of requests to send
    #[argh(option, short = 'n', default = "1")]
    count: u32,

    /// number of concurrent connections
    #[argh(option, short = 'c', default = "1")]
    concurrency: u32,

    /// request body
    #[argh(option, short = 'd')]
    data: Option<String>,

    /// header in "Key: Value" format (repeatable)
    #[argh(option, short = 'H')]
    header: Vec<String>,

    /// show response headers
    #[argh(switch, short = 'v')]
    verbose: bool,
}

fn main() {
    let args: Args = argh::from_env();

    println!("target:      {}", args.url);
    println!("method:      {}", args.method);
    println!("requests:    {}", args.count);
    println!("concurrency: {}", args.concurrency);
    if args.verbose {
        println!("verbose:     on");
    }
    if let Some(ref body) = args.data {
        println!("body:        {} bytes", body.len());
    }
    for h in &args.header {
        println!("header:      {}", h);
    }

    // TODO: implement HTTP client
    // TODO: collect timing metrics (DNS, connect, TLS, TTFB, total)
    // TODO: run concurrent requests
    // TODO: print summary stats (min, max, mean, p50, p95, p99)
}
