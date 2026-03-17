//! Type definitions for load testing configuration and status

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hdrhistogram::Histogram;
use hdrhistogram::serialization::Serializer as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// HTTP method for requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    GET,
    POST,
    PUT,
    DELETE,
    PATCH,
    HEAD,
    OPTIONS,
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpMethod::GET => write!(f, "GET"),
            HttpMethod::POST => write!(f, "POST"),
            HttpMethod::PUT => write!(f, "PUT"),
            HttpMethod::DELETE => write!(f, "DELETE"),
            HttpMethod::PATCH => write!(f, "PATCH"),
            HttpMethod::HEAD => write!(f, "HEAD"),
            HttpMethod::OPTIONS => write!(f, "OPTIONS"),
        }
    }
}

impl std::str::FromStr for HttpMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "GET" => Ok(HttpMethod::GET),
            "POST" => Ok(HttpMethod::POST),
            "PUT" => Ok(HttpMethod::PUT),
            "DELETE" => Ok(HttpMethod::DELETE),
            "PATCH" => Ok(HttpMethod::PATCH),
            "HEAD" => Ok(HttpMethod::HEAD),
            "OPTIONS" => Ok(HttpMethod::OPTIONS),
            _ => Err(format!("Unknown HTTP method: {}", s)),
        }
    }
}

/// Configuration for max-throughput (closed-loop) benchmark mode
#[derive(Debug, Clone)]
pub struct MaxThroughputConfig {
    pub target_url: String,
    pub method: HttpMethod,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    /// Number of concurrent connections (async tasks)
    pub connections: u32,
    /// Test duration in seconds
    pub duration_seconds: u32,
    /// Warmup duration in seconds (requests sent but not recorded)
    pub warmup_seconds: u32,
}

/// Configuration for a load test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Target URL to test
    pub target_url: String,

    /// HTTP method (GET, POST, etc.)
    #[serde(default)]
    pub method: HttpMethod,

    /// HTTP headers to include in requests
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Request body (for POST, PUT, PATCH)
    #[serde(default)]
    pub body: Option<String>,

    /// Total requests per second across all workers
    pub total_rate: u32,

    /// Number of worker processes
    pub process_count: u32,

    /// Test duration in seconds
    pub duration_seconds: u32,

    /// Warmup duration in seconds (requests sent but not recorded)
    #[serde(default)]
    pub warmup_seconds: u32,

    /// Directory for metrics files (set by MCP server)
    #[serde(default)]
    pub metrics_dir: Option<String>,
}

/// Current status of a load test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStatus {
    /// Whether a test is currently running
    pub running: bool,

    /// Whether the test completed naturally (vs manually stopped)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,

    /// Process ID of the manager (if running)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,

    /// When the test started (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,

    /// When the test ended (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,

    /// Configuration of the running/last test
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<TestConfig>,

    /// Directory containing metrics files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_dir: Option<String>,

    /// Merged metrics from all workers (populated after completion)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MergedMetrics>,
}

impl Default for TestStatus {
    fn default() -> Self {
        Self {
            running: false,
            completed: None,
            pid: None,
            start_time: None,
            end_time: None,
            config: None,
            metrics_dir: None,
            metrics: None,
        }
    }
}

/// Arguments for start_load_test tool
#[derive(Debug, Clone, Deserialize)]
pub struct StartLoadTestArgs {
    /// Target URL to test
    pub target_url: String,

    /// HTTP method (default: GET)
    #[serde(default)]
    pub method: Option<String>,

    /// HTTP headers as key-value pairs
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,

    /// Request body for POST/PUT/PATCH
    #[serde(default)]
    pub body: Option<String>,

    /// Total requests per second
    pub total_rate: u32,

    /// Number of worker processes (default: CPU count)
    #[serde(default)]
    pub process_count: Option<u32>,

    /// Test duration in seconds
    pub duration_seconds: u32,
}

impl StartLoadTestArgs {
    /// Convert to TestConfig with defaults applied
    pub fn into_config(self) -> Result<TestConfig, String> {
        let method = match &self.method {
            Some(m) => m.parse()?,
            None => HttpMethod::GET,
        };

        let process_count = self.process_count.unwrap_or_else(|| num_cpus::get() as u32);

        if process_count == 0 {
            return Err("process_count must be at least 1".to_string());
        }

        if self.total_rate == 0 {
            return Err("total_rate must be at least 1".to_string());
        }

        if self.duration_seconds == 0 {
            return Err("duration_seconds must be at least 1".to_string());
        }

        Ok(TestConfig {
            target_url: self.target_url,
            method,
            headers: self.headers.unwrap_or_default(),
            body: self.body,
            total_rate: self.total_rate,
            process_count,
            duration_seconds: self.duration_seconds,
            warmup_seconds: 0,
            metrics_dir: None,
        })
    }
}

/// Worker configuration passed via command line
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Target URL
    pub target_url: String,

    /// HTTP method
    pub method: HttpMethod,

    /// HTTP headers
    pub headers: HashMap<String, String>,

    /// Request body
    pub body: Option<String>,

    /// Requests per second for this worker
    pub rate: u32,

    /// Duration in seconds
    pub duration_seconds: u32,

    /// Warmup duration in seconds (requests sent but not recorded)
    #[serde(default)]
    pub warmup_seconds: u32,

    /// Worker ID for metrics file naming
    pub worker_id: u32,

    /// Directory to write metrics to
    #[serde(default)]
    pub metrics_dir: Option<String>,
}

/// High-precision latency histogram using HdrHistogram
///
/// Records latencies in microseconds internally for sub-millisecond precision.
/// Range: 1us to 60s, 3 significant digits (matches wrk precision).
#[derive(Debug, Clone)]
pub struct HdrLatencyHistogram {
    inner: Histogram<u64>,
}

impl Default for HdrLatencyHistogram {
    fn default() -> Self {
        Self {
            // 1us to 60s, 3 significant digits
            inner: Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
                .expect("valid histogram bounds"),
        }
    }
}

impl HdrLatencyHistogram {
    /// Record a latency sample in microseconds
    pub fn record(&mut self, latency_us: u64) {
        let value = latency_us.min(self.inner.high());
        self.inner.record(value).ok();
    }

    /// Get exact percentile value in milliseconds
    pub fn percentile_ms(&self, p: f64) -> f64 {
        self.inner.value_at_percentile(p) as f64 / 1000.0
    }

    /// Get minimum recorded value in milliseconds
    pub fn min_ms(&self) -> f64 {
        if self.inner.is_empty() {
            0.0
        } else {
            self.inner.min() as f64 / 1000.0
        }
    }

    /// Get maximum recorded value in milliseconds
    pub fn max_ms(&self) -> f64 {
        if self.inner.is_empty() {
            0.0
        } else {
            self.inner.max() as f64 / 1000.0
        }
    }

    /// Get mean value in milliseconds
    pub fn mean_ms(&self) -> f64 {
        if self.inner.is_empty() {
            0.0
        } else {
            self.inner.mean() / 1000.0
        }
    }

    /// Get total number of recorded samples
    #[cfg(test)]
    pub fn count(&self) -> u64 {
        self.inner.len()
    }

    /// Merge another histogram into this one
    pub fn merge(&mut self, other: &HdrLatencyHistogram) {
        self.inner.add(&other.inner).ok();
    }

    /// Serialize to base64 string for JSON transport
    pub fn to_base64(&self) -> String {
        let mut buf = Vec::new();
        hdrhistogram::serialization::V2Serializer::new()
            .serialize(&self.inner, &mut buf)
            .expect("histogram serialization should not fail");
        BASE64.encode(&buf)
    }

    /// Deserialize from base64 string
    pub fn from_base64(encoded: &str) -> Result<Self, String> {
        let bytes = BASE64
            .decode(encoded)
            .map_err(|e| format!("base64 decode error: {}", e))?;
        let mut deserializer = hdrhistogram::serialization::Deserializer::new();
        let inner: Histogram<u64> = deserializer
            .deserialize(&mut &bytes[..])
            .map_err(|e| format!("histogram deserialize error: {}", e))?;
        Ok(Self { inner })
    }
}

impl Serialize for HdrLatencyHistogram {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for HdrLatencyHistogram {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        Self::from_base64(&encoded).map_err(serde::de::Error::custom)
    }
}

/// Metrics collected by a single worker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerMetrics {
    /// Worker ID
    pub worker_id: u32,
    /// Total requests made
    pub request_count: u64,
    /// Successful requests (2xx responses)
    pub success_count: u64,
    /// Failed requests (errors or non-2xx)
    pub error_count: u64,
    /// HTTP status code counts (status_code -> count)
    #[serde(default)]
    pub status_codes: HashMap<u16, u64>,
    /// Latency histogram (base64-encoded HdrHistogram)
    pub latency: HdrLatencyHistogram,
    /// Actual test duration in seconds
    pub duration_secs: f64,
    /// Total bytes received in response bodies
    #[serde(default)]
    pub total_bytes: u64,
}

impl Default for WorkerMetrics {
    fn default() -> Self {
        Self {
            worker_id: 0,
            request_count: 0,
            success_count: 0,
            error_count: 0,
            status_codes: HashMap::new(),
            latency: HdrLatencyHistogram::default(),
            duration_secs: 0.0,
            total_bytes: 0,
        }
    }
}

/// Merged metrics from all workers (included in status response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedMetrics {
    /// Total requests across all workers
    pub total_requests: u64,
    /// Successful requests
    pub successful_requests: u64,
    /// Failed requests
    pub failed_requests: u64,
    /// HTTP status code counts (status_code -> count)
    #[serde(default)]
    pub status_codes: HashMap<u16, u64>,

    /// Requests per minute
    pub rpm: f64,
    /// Requests per second (actual achieved)
    pub rps: f64,

    /// Average latency in milliseconds
    pub latency_avg_ms: f64,
    /// Minimum latency in milliseconds
    pub latency_min_ms: f64,
    /// Maximum latency in milliseconds
    pub latency_max_ms: f64,

    /// Exact percentiles from HdrHistogram
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub latency_p999_ms: f64,
    pub latency_p9999_ms: f64,

    /// Total test duration in seconds
    pub duration_secs: f64,
    /// Number of workers that reported
    pub worker_count: u32,
    /// Total bytes transferred (received)
    #[serde(default)]
    pub total_bytes: u64,
    /// Transfer rate in bytes per second
    #[serde(default)]
    pub transfer_per_sec: f64,
}

impl MergedMetrics {
    /// Create merged metrics from multiple worker metrics
    pub fn from_workers(workers: &[WorkerMetrics]) -> Self {
        if workers.is_empty() {
            return Self::default();
        }

        let mut merged_latency = HdrLatencyHistogram::default();
        let mut merged_status_codes: HashMap<u16, u64> = HashMap::new();
        let mut total_requests: u64 = 0;
        let mut success_count: u64 = 0;
        let mut error_count: u64 = 0;
        let mut max_duration: f64 = 0.0;
        let mut total_bytes: u64 = 0;

        for w in workers {
            total_requests += w.request_count;
            success_count += w.success_count;
            error_count += w.error_count;
            total_bytes += w.total_bytes;
            merged_latency.merge(&w.latency);
            max_duration = max_duration.max(w.duration_secs);

            for (&code, &count) in &w.status_codes {
                *merged_status_codes.entry(code).or_insert(0) += count;
            }
        }

        let rps = if max_duration > 0.0 {
            ((total_requests as f64 / max_duration) * 100.0).round() / 100.0
        } else {
            0.0
        };

        let transfer_per_sec = if max_duration > 0.0 {
            total_bytes as f64 / max_duration
        } else {
            0.0
        };

        Self {
            total_requests,
            successful_requests: success_count,
            failed_requests: error_count,
            status_codes: merged_status_codes,
            rpm: (rps * 60.0 * 100.0).round() / 100.0,
            rps,
            latency_avg_ms: round2(merged_latency.mean_ms()),
            latency_min_ms: round2(merged_latency.min_ms()),
            latency_max_ms: round2(merged_latency.max_ms()),
            latency_p50_ms: round2(merged_latency.percentile_ms(50.0)),
            latency_p90_ms: round2(merged_latency.percentile_ms(90.0)),
            latency_p95_ms: round2(merged_latency.percentile_ms(95.0)),
            latency_p99_ms: round2(merged_latency.percentile_ms(99.0)),
            latency_p999_ms: round2(merged_latency.percentile_ms(99.9)),
            latency_p9999_ms: round2(merged_latency.percentile_ms(99.99)),
            duration_secs: round2(max_duration),
            worker_count: workers.len() as u32,
            total_bytes,
            transfer_per_sec,
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

impl Default for MergedMetrics {
    fn default() -> Self {
        Self {
            total_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            status_codes: HashMap::new(),
            rpm: 0.0,
            rps: 0.0,
            latency_avg_ms: 0.0,
            latency_min_ms: 0.0,
            latency_max_ms: 0.0,
            latency_p50_ms: 0.0,
            latency_p90_ms: 0.0,
            latency_p95_ms: 0.0,
            latency_p99_ms: 0.0,
            latency_p999_ms: 0.0,
            latency_p9999_ms: 0.0,
            duration_secs: 0.0,
            worker_count: 0,
            total_bytes: 0,
            transfer_per_sec: 0.0,
        }
    }
}

/// Format bytes per second in human-readable form (B/s, KB/s, MB/s, GB/s)
pub fn format_bytes(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes_per_sec >= GB {
        format!("{:.2}GB/s", bytes_per_sec / GB)
    } else if bytes_per_sec >= MB {
        format!("{:.2}MB/s", bytes_per_sec / MB)
    } else if bytes_per_sec >= KB {
        format!("{:.2}KB/s", bytes_per_sec / KB)
    } else {
        format!("{:.0}B/s", bytes_per_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_parse() {
        assert_eq!("GET".parse::<HttpMethod>().unwrap(), HttpMethod::GET);
        assert_eq!("post".parse::<HttpMethod>().unwrap(), HttpMethod::POST);
        assert_eq!("Put".parse::<HttpMethod>().unwrap(), HttpMethod::PUT);
    }

    #[test]
    fn test_start_args_to_config() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: Some("POST".to_string()),
            headers: Some(HashMap::from([(
                "Content-Type".to_string(),
                "application/json".to_string(),
            )])),
            body: Some(r#"{"test": true}"#.to_string()),
            total_rate: 1000,
            process_count: Some(4),
            duration_seconds: 30,
        };

        let config = args.into_config().unwrap();
        assert_eq!(config.method, HttpMethod::POST);
        assert_eq!(config.process_count, 4);
        assert_eq!(
            config.headers.get("Content-Type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_config_defaults() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: None,
            headers: None,
            body: None,
            total_rate: 100,
            process_count: None,
            duration_seconds: 10,
        };

        let config = args.into_config().unwrap();
        assert_eq!(config.method, HttpMethod::GET);
        assert!(config.process_count > 0);
        assert!(config.headers.is_empty());
    }

    #[test]
    fn test_invalid_config() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: None,
            headers: None,
            body: None,
            total_rate: 0,
            process_count: None,
            duration_seconds: 10,
        };

        assert!(args.into_config().is_err());
    }

    #[test]
    fn test_hdr_histogram_record_and_percentile() {
        let mut hist = HdrLatencyHistogram::default();
        hist.record(500);
        hist.record(3_000);
        hist.record(7_500);
        hist.record(150_000);

        assert_eq!(hist.count(), 4);
        assert!(hist.min_ms() < 1.0);
        assert!(hist.max_ms() >= 149.0);
    }

    #[test]
    fn test_hdr_histogram_merge() {
        let mut hist1 = HdrLatencyHistogram::default();
        hist1.record(1_500);
        hist1.record(2_500);

        let mut hist2 = HdrLatencyHistogram::default();
        hist2.record(500);
        hist2.record(200_000);

        hist1.merge(&hist2);

        assert_eq!(hist1.count(), 4);
        assert!(hist1.min_ms() < 1.0);
        assert!(hist1.max_ms() >= 199.0);
    }

    #[test]
    fn test_hdr_histogram_mean() {
        let mut hist = HdrLatencyHistogram::default();
        hist.record(10_000);
        hist.record(20_000);
        hist.record(30_000);

        let mean = hist.mean_ms();
        assert!(mean > 19.0 && mean < 21.0, "mean was {}", mean);
    }

    #[test]
    fn test_hdr_histogram_exact_percentile() {
        let mut hist = HdrLatencyHistogram::default();
        for _ in 0..100 {
            hist.record(2_000);
        }

        let p50 = hist.percentile_ms(50.0);
        let p99 = hist.percentile_ms(99.0);
        assert!(p50 >= 1.9 && p50 <= 2.1, "p50 should be ~2ms, got {}", p50);
        assert!(p99 >= 1.9 && p99 <= 2.1, "p99 should be ~2ms, got {}", p99);
    }

    #[test]
    fn test_hdr_histogram_serialization_roundtrip() {
        let mut hist = HdrLatencyHistogram::default();
        for i in 1..=1000 {
            hist.record(i * 100);
        }

        let encoded = hist.to_base64();
        let decoded = HdrLatencyHistogram::from_base64(&encoded).unwrap();

        assert_eq!(hist.count(), decoded.count());
        assert_eq!(hist.min_ms(), decoded.min_ms());
        assert_eq!(hist.max_ms(), decoded.max_ms());
        assert_eq!(hist.percentile_ms(50.0), decoded.percentile_ms(50.0));
        assert_eq!(hist.percentile_ms(99.0), decoded.percentile_ms(99.0));
    }

    #[test]
    fn test_hdr_histogram_serde_json_roundtrip() {
        let mut hist = HdrLatencyHistogram::default();
        hist.record(5_000);
        hist.record(10_000);

        let json = serde_json::to_string(&hist).unwrap();
        let decoded: HdrLatencyHistogram = serde_json::from_str(&json).unwrap();

        assert_eq!(hist.count(), decoded.count());
        assert_eq!(hist.percentile_ms(50.0), decoded.percentile_ms(50.0));
    }

    #[test]
    fn test_merged_metrics_from_workers() {
        let mut w1 = WorkerMetrics::default();
        w1.worker_id = 0;
        w1.request_count = 100;
        w1.success_count = 95;
        w1.error_count = 5;
        w1.duration_secs = 10.0;
        w1.latency.record(5_000);
        w1.latency.record(10_000);

        let mut w2 = WorkerMetrics::default();
        w2.worker_id = 1;
        w2.request_count = 100;
        w2.success_count = 100;
        w2.error_count = 0;
        w2.duration_secs = 10.0;
        w2.latency.record(3_000);
        w2.latency.record(7_000);

        let merged = MergedMetrics::from_workers(&[w1, w2]);

        assert_eq!(merged.total_requests, 200);
        assert_eq!(merged.successful_requests, 195);
        assert_eq!(merged.failed_requests, 5);
        assert_eq!(merged.worker_count, 2);
        assert_eq!(merged.rps, 20.0);
        assert_eq!(merged.rpm, 1200.0);
    }

    #[test]
    fn test_merged_metrics_has_tail_percentiles() {
        let mut w = WorkerMetrics::default();
        w.request_count = 10000;
        w.success_count = 10000;
        w.duration_secs = 10.0;
        for i in 1..=10000 {
            w.latency.record(i * 10);
        }

        let merged = MergedMetrics::from_workers(&[w]);

        assert!(merged.latency_p999_ms > 0.0);
        assert!(merged.latency_p9999_ms > 0.0);
        assert!(merged.latency_p999_ms >= merged.latency_p99_ms);
        assert!(merged.latency_p9999_ms >= merged.latency_p999_ms);
    }

    #[test]
    fn test_status_codes_merging() {
        let mut w1 = WorkerMetrics::default();
        w1.worker_id = 0;
        w1.request_count = 100;
        w1.success_count = 90;
        w1.error_count = 10;
        w1.duration_secs = 10.0;
        w1.status_codes.insert(200, 80);
        w1.status_codes.insert(201, 10);
        w1.status_codes.insert(404, 5);
        w1.status_codes.insert(500, 5);

        let mut w2 = WorkerMetrics::default();
        w2.worker_id = 1;
        w2.request_count = 100;
        w2.success_count = 95;
        w2.error_count = 5;
        w2.duration_secs = 10.0;
        w2.status_codes.insert(200, 90);
        w2.status_codes.insert(201, 5);
        w2.status_codes.insert(400, 3);
        w2.status_codes.insert(500, 2);

        let merged = MergedMetrics::from_workers(&[w1, w2]);

        assert_eq!(merged.total_requests, 200);
        assert_eq!(merged.successful_requests, 185);
        assert_eq!(merged.failed_requests, 15);

        assert_eq!(merged.status_codes.get(&200), Some(&170));
        assert_eq!(merged.status_codes.get(&201), Some(&15));
        assert_eq!(merged.status_codes.get(&404), Some(&5));
        assert_eq!(merged.status_codes.get(&400), Some(&3));
        assert_eq!(merged.status_codes.get(&500), Some(&7));
    }
}
