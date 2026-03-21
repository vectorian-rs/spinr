//! Multi-scenario benchmark runner.
//!
//! Reads a TOML config file with one or more `[[scenario]]` entries, runs each
//! sequentially as a load test, and prints a summary table at the end.

use crate::loadtest::orchestrator::OrchestratorError;
use crate::loadtest::preflight::PreflightError;
use crate::loadtest::request::BuildRequestError;
use crate::loadtest::types::{
    EngineConfig, EngineMode, HttpMethod, MergedMetrics, RawWorkerMetrics,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("bench config must have at least one [[scenario]]")]
    EmptyScenarios,
    #[error("scenario #{index}: name is required")]
    MissingName { index: usize },
    #[error("scenario #{index} ({name}): url is required")]
    MissingUrl { index: usize, name: String },
    #[error("scenario #{index} ({name}): {message}")]
    InvalidScenario {
        index: usize,
        name: String,
        message: String,
    },
    #[error("Unknown HTTP method: {0}")]
    InvalidMethod(String),
    #[error("Invalid header format: '{0}'. Use 'Name: Value'")]
    InvalidHeader(String),
    #[error("failed to read bench config: {path}: {source}")]
    ReadConfig {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse bench config TOML: {0}")]
    ParseToml(#[from] toml::de::Error),
    #[error("DNS resolution failed for {host}")]
    DnsResolution { host: String },
    #[error("{0}")]
    BuildRequest(#[from] BuildRequestError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Preflight(#[from] PreflightError),
    #[error("{0}")]
    Orchestrator(#[from] OrchestratorError),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Config types (deserialized from TOML)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BenchConfig {
    pub scenario: Vec<ScenarioConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_connections")]
    pub connections: u32,
    #[serde(default = "default_duration")]
    pub duration: u32,
    #[serde(default)]
    pub warmup: u32,
    #[serde(default)]
    pub max_throughput: bool,
    #[serde(default = "default_rate")]
    pub rate: u32,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub threads: Option<u32>,
    #[serde(default)]
    pub hdr_log: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}
fn default_connections() -> u32 {
    1
}
fn default_duration() -> u32 {
    10
}
fn default_rate() -> u32 {
    100
}

impl BenchConfig {
    pub fn validate(&self) -> Result<(), BenchError> {
        if self.scenario.is_empty() {
            return Err(BenchError::EmptyScenarios);
        }
        for (i, s) in self.scenario.iter().enumerate() {
            let index = i + 1;
            if s.name.is_empty() {
                return Err(BenchError::MissingName { index });
            }
            if s.url.is_empty() {
                return Err(BenchError::MissingUrl {
                    index,
                    name: s.name.clone(),
                });
            }
            s.method
                .parse::<HttpMethod>()
                .map_err(|e| BenchError::InvalidScenario {
                    index,
                    name: s.name.clone(),
                    message: e,
                })?;
            if s.duration == 0 {
                return Err(BenchError::InvalidScenario {
                    index,
                    name: s.name.clone(),
                    message: "duration must be > 0".to_string(),
                });
            }
            if !s.max_throughput && s.rate == 0 {
                return Err(BenchError::InvalidScenario {
                    index,
                    name: s.name.clone(),
                    message: "rate must be > 0 when not max_throughput".to_string(),
                });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bridge type — shared params for running a single load test
// ---------------------------------------------------------------------------

pub struct LoadTestParams {
    pub url: String,
    pub method: HttpMethod,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub connections: u32,
    pub duration: u32,
    pub warmup: u32,
    pub max_throughput: bool,
    pub rate: u32,
    pub threads: Option<u32>,
    pub hdr_log: Option<String>,
}

impl LoadTestParams {
    pub fn from_scenario(s: &ScenarioConfig) -> Result<Self, BenchError> {
        let method: HttpMethod = s.method.parse().map_err(BenchError::InvalidMethod)?;
        Ok(Self {
            url: s.url.clone(),
            method,
            headers: s.headers.clone(),
            body: s.body.clone(),
            connections: s.connections,
            duration: s.duration,
            warmup: s.warmup,
            max_throughput: s.max_throughput,
            rate: s.rate,
            threads: s.threads,
            hdr_log: s.hdr_log.clone(),
        })
    }

    pub fn from_cli(cmd: &crate::cli::LoadTestCommand) -> Result<Self, BenchError> {
        let method: HttpMethod = cmd.method.parse().map_err(BenchError::InvalidMethod)?;
        let mut headers = HashMap::new();
        for h in &cmd.header {
            if let Some((name, value)) = h.split_once(':') {
                headers.insert(name.trim().to_string(), value.trim().to_string());
            } else {
                return Err(BenchError::InvalidHeader(h.clone()));
            }
        }
        Ok(Self {
            url: cmd.url.clone(),
            method,
            headers,
            body: cmd.body.clone(),
            connections: cmd.connections,
            duration: cmd.duration,
            warmup: cmd.warmup,
            max_throughput: cmd.max_throughput,
            rate: cmd.rate,
            threads: cmd.threads,
            hdr_log: cmd.hdr_log.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub name: String,
    pub url: String,
    pub metrics: MergedMetrics,
}

#[derive(Debug, Serialize)]
pub struct BenchSummary {
    pub scenarios: Vec<ScenarioResult>,
    pub total_requests: u64,
    pub total_duration_secs: f64,
}

// ---------------------------------------------------------------------------
// Core: run a single load test from LoadTestParams
// ---------------------------------------------------------------------------

pub fn run_single_loadtest(
    params: &LoadTestParams,
    json: bool,
) -> Result<MergedMetrics, BenchError> {
    let prepared = crate::loadtest::request::build_request_bytes(
        &params.url,
        params.method,
        &params.headers,
        params.body.as_deref(),
    )?;

    let authority = &prepared.remote_addr_authority;
    let addr_with_port = if authority.contains(':') {
        authority.clone()
    } else {
        format!("{}:80", authority)
    };
    let remote_addr: SocketAddr =
        addr_with_port
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| BenchError::DnsResolution {
                host: authority.clone(),
            })?;

    let worker_count = params
        .threads
        .unwrap_or_else(|| num_cpus::get() as u32)
        .max(1);

    let mode = if params.max_throughput {
        EngineMode::MaxThroughput
    } else {
        EngineMode::RateLimited {
            requests_per_second: params.rate as u64,
            latency_correction: true,
        }
    };

    let total_connections = if params.max_throughput {
        params.connections.max(worker_count)
    } else {
        params.connections.max(1)
    };

    crate::loadtest::preflight::run_preflight(total_connections, worker_count, json)?;

    let mut configs = Vec::with_capacity(worker_count as usize);
    for worker_id in 0..worker_count {
        let base = total_connections / worker_count;
        let extra = if worker_id < (total_connections % worker_count) {
            1
        } else {
            0
        };
        let conns = (base + extra).max(1);

        configs.push(EngineConfig {
            worker_id,
            remote_addr,
            method: params.method,
            connections: conns,
            duration_seconds: params.duration,
            warmup_seconds: params.warmup,
            mode: mode.clone(),
            read_buffer_size: 8192,
        });
    }

    macro_rules! out {
        ($($arg:tt)*) => {
            if json { eprintln!($($arg)*); } else { println!($($arg)*); }
        };
    }
    if params.max_throughput {
        out!("Starting max-throughput test:");
    } else {
        out!("Starting load test:");
        out!("  Rate:        {} RPS", params.rate);
    }
    out!("  URL:         {}", params.url);
    out!("  Method:      {}", params.method);
    out!("  Target:      {}", remote_addr);
    out!("  Connections: {}", total_connections);
    out!("  Workers:     {}", worker_count);
    if params.warmup > 0 {
        out!("  Warmup:      {}s", params.warmup);
    }
    out!("  Duration:    {}s", params.duration);
    out!();

    let raw_results: Vec<RawWorkerMetrics> =
        crate::loadtest::orchestrator::run_workers(configs, &prepared.bytes)?;

    let worker_metrics: Vec<_> = raw_results
        .into_iter()
        .map(|r| r.into_worker_metrics())
        .collect();
    let metrics = MergedMetrics::from_workers(&worker_metrics);

    if let Some(ref path) = params.hdr_log {
        crate::output::write_hdr_log(std::path::Path::new(path), &metrics)?;
        eprintln!("HDR Histogram log written to {}", path);
    }

    Ok(metrics)
}

// ---------------------------------------------------------------------------
// Entry point: run_bench
// ---------------------------------------------------------------------------

pub fn run_bench(config_path: &str, json: bool) -> Result<(), BenchError> {
    let toml_str =
        std::fs::read_to_string(config_path).map_err(|source| BenchError::ReadConfig {
            path: config_path.to_string(),
            source,
        })?;
    let config: BenchConfig = toml::from_str::<BenchConfig>(&toml_str)?;
    config.validate()?;

    let mut results: Vec<ScenarioResult> = Vec::with_capacity(config.scenario.len());
    let mut total_duration = 0.0f64;

    for (i, scenario) in config.scenario.iter().enumerate() {
        if !json {
            if i > 0 {
                println!();
                println!("{}", "=".repeat(72));
                println!();
            }
            println!(
                "Scenario {}/{}: {}",
                i + 1,
                config.scenario.len(),
                scenario.name
            );
            println!();
        }

        let params = LoadTestParams::from_scenario(scenario)?;
        let metrics = run_single_loadtest(&params, json)?;

        if !json {
            crate::output::print_metrics(&metrics);
        }

        total_duration += metrics.duration_secs;
        results.push(ScenarioResult {
            name: scenario.name.clone(),
            url: scenario.url.clone(),
            metrics,
        });
    }

    let total_requests: u64 = results.iter().map(|r| r.metrics.total_requests).sum();

    if json {
        let summary = BenchSummary {
            scenarios: results,
            total_requests,
            total_duration_secs: total_duration,
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!();
        println!("{}", "=".repeat(72));
        println!();
        println!("Benchmark Summary");
        println!();
        println!(
            "{:<28} {:>10} {:>10} {:>10} {:>10} {:>8}",
            "Scenario", "Requests", "RPS", "p50 (ms)", "p99 (ms)", "Errors"
        );
        println!("{}", "-".repeat(78));
        for r in &results {
            println!(
                "{:<28} {:>10} {:>10.2} {:>10.2} {:>10.2} {:>8}",
                truncate(&r.name, 28),
                r.metrics.total_requests,
                r.metrics.rps,
                r.metrics.latency_p50_ms,
                r.metrics.latency_p99_ms,
                r.metrics.failed_requests,
            );
        }
        println!("{}", "-".repeat(78));
        println!(
            "{:<28} {:>10} {:>10} {:>10} {:>10} {:>8}",
            "TOTAL",
            total_requests,
            "",
            "",
            "",
            results
                .iter()
                .map(|r| r.metrics.failed_requests)
                .sum::<u64>(),
        );
        println!();
        println!("Total duration: {:.2}s", total_duration);
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[[scenario]]
name = "basic"
url = "http://localhost:8080/"
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.scenario.len(), 1);
        assert_eq!(config.scenario[0].name, "basic");
        assert_eq!(config.scenario[0].method, "GET");
        assert_eq!(config.scenario[0].connections, 1);
        assert_eq!(config.scenario[0].duration, 10);
        assert_eq!(config.scenario[0].rate, 100);
    }

    #[test]
    fn parse_full_toml() {
        let toml_str = r#"
[[scenario]]
name = "post-test"
url = "http://localhost:8080/api"
method = "POST"
connections = 32
duration = 60
warmup = 5
max_throughput = true
rate = 5000
hdr_log = "/tmp/test.hlog"
body = '{"key": "value"}'

[scenario.headers]
Content-Type = "application/json"
Authorization = "Bearer tok123"
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        let s = &config.scenario[0];
        assert_eq!(s.name, "post-test");
        assert_eq!(s.method, "POST");
        assert_eq!(s.connections, 32);
        assert_eq!(s.duration, 60);
        assert_eq!(s.warmup, 5);
        assert!(s.max_throughput);
        assert_eq!(s.rate, 5000);
        assert_eq!(s.hdr_log.as_deref(), Some("/tmp/test.hlog"));
        assert_eq!(s.body.as_deref(), Some(r#"{"key": "value"}"#));
        assert_eq!(s.headers.get("Content-Type").unwrap(), "application/json");
        assert_eq!(s.headers.get("Authorization").unwrap(), "Bearer tok123");
    }

    #[test]
    fn parse_multi_scenario_toml() {
        let toml_str = r#"
[[scenario]]
name = "get-root"
url = "http://localhost:8080/"

[[scenario]]
name = "post-api"
url = "http://localhost:8080/api"
method = "POST"
rate = 500
duration = 30
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.scenario.len(), 2);
        assert_eq!(config.scenario[0].name, "get-root");
        assert_eq!(config.scenario[1].name, "post-api");
        assert_eq!(config.scenario[1].rate, 500);
        assert_eq!(config.scenario[1].duration, 30);
    }

    #[test]
    fn validate_empty_scenarios() {
        let config = BenchConfig { scenario: vec![] };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("at least one"), "got: {}", err);
    }

    #[test]
    fn validate_missing_name() {
        let toml_str = r#"
[[scenario]]
name = ""
url = "http://localhost/"
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("name is required"), "got: {}", err);
    }

    #[test]
    fn validate_bad_method() {
        let toml_str = r#"
[[scenario]]
name = "bad"
url = "http://localhost/"
method = "FROBNICATE"
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("Unknown HTTP method"),
            "got: {}",
            err
        );
    }

    #[test]
    fn validate_zero_duration() {
        let toml_str = r#"
[[scenario]]
name = "zero-dur"
url = "http://localhost/"
duration = 0
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("duration must be > 0"),
            "got: {}",
            err
        );
    }

    #[test]
    fn validate_zero_rate_without_max_throughput() {
        let toml_str = r#"
[[scenario]]
name = "zero-rate"
url = "http://localhost/"
rate = 0
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("rate must be > 0"), "got: {}", err);
    }

    #[test]
    fn validate_zero_rate_with_max_throughput_ok() {
        let toml_str = r#"
[[scenario]]
name = "max-tp"
url = "http://localhost/"
rate = 0
max_throughput = true
"#;
        let config: BenchConfig = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn from_scenario_converts_correctly() {
        let scenario = ScenarioConfig {
            name: "test".to_string(),
            url: "http://localhost:9090/path".to_string(),
            method: "POST".to_string(),
            connections: 16,
            duration: 30,
            warmup: 5,
            max_throughput: false,
            rate: 1000,
            headers: HashMap::from([("X-Key".to_string(), "val".to_string())]),
            body: Some("payload".to_string()),
            threads: Some(4),
            hdr_log: Some("/tmp/h.hlog".to_string()),
        };
        let params = LoadTestParams::from_scenario(&scenario).unwrap();
        assert_eq!(params.method, HttpMethod::POST);
        assert_eq!(params.connections, 16);
        assert_eq!(params.duration, 30);
        assert_eq!(params.warmup, 5);
        assert_eq!(params.rate, 1000);
        assert!(!params.max_throughput);
        assert_eq!(params.headers.get("X-Key").unwrap(), "val");
        assert_eq!(params.body.as_deref(), Some("payload"));
        assert_eq!(params.threads, Some(4));
    }

    #[test]
    fn bench_summary_json_serialization() {
        let summary = BenchSummary {
            scenarios: vec![ScenarioResult {
                name: "test".to_string(),
                url: "http://localhost/".to_string(),
                metrics: MergedMetrics::default(),
            }],
            total_requests: 0,
            total_duration_secs: 0.0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"total_requests\":0"));
        assert!(json.contains("\"total_duration_secs\":0.0"));
    }
}
