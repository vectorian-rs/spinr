//! Type definitions for load testing configuration and status

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hdrhistogram::Histogram;
use hdrhistogram::serialization::Serializer as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;

// ---------------------------------------------------------------------------
// Engine contract types (shared between orchestration and engine)
// ---------------------------------------------------------------------------

/// Engine operating mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EngineMode {
    /// No rate limiting — send as fast as possible
    MaxThroughput,
    /// Constant-rate with optional coordinated-omission correction
    RateLimited {
        requests_per_second: u64,
        latency_correction: bool,
    },
}

/// Configuration passed to each engine worker process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Worker identifier (0-based)
    pub worker_id: u32,
    /// Resolved target address
    pub remote_addr: SocketAddr,
    /// HTTP method (needed by engine for HEAD response handling)
    pub method: HttpMethod,
    /// Number of persistent TCP connections this worker manages
    pub connections: u32,
    /// Measurement duration in seconds (excludes warmup)
    pub duration_seconds: u32,
    /// Warmup duration in seconds (requests sent but not recorded)
    pub warmup_seconds: u32,
    /// Operating mode
    pub mode: EngineMode,
    /// Per-connection read buffer size in bytes
    pub read_buffer_size: usize,
}

/// Raw metrics returned by a single engine worker process.
///
/// Uses `[u64; 600]` for status counters to avoid `HashMap` in the hot path.
/// Conversion to the public `HashMap<u16, u64>` shape happens in the
/// orchestration layer at shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawWorkerMetrics {
    /// Worker identifier
    pub worker_id: u32,
    /// Total requests completed
    pub request_count: u64,
    /// Requests that received a 2xx status
    pub success_count: u64,
    /// Non-2xx responses + transport/parse/connect failures
    pub error_count: u64,
    /// Status code counts indexed by code (0–599).
    /// Serialized as a sparse map of non-zero entries for compact JSON.
    #[serde(
        serialize_with = "serialize_status_counts",
        deserialize_with = "deserialize_status_counts"
    )]
    pub status_counts: [u64; 600],
    /// Uncorrected latency (measured from actual send to response)
    pub latency_uncorrected: HdrLatencyHistogram,
    /// Corrected latency (measured from scheduled send time). Present
    /// only in rate-limited mode with latency correction enabled.
    pub latency_corrected: Option<HdrLatencyHistogram>,
    /// Actual measurement duration in seconds
    pub duration_secs: f64,
    /// Total decoded payload bytes received across all responses.
    #[serde(default, alias = "total_bytes")]
    pub payload_bytes: u64,
    /// Total wire-level body bytes consumed across all responses.
    ///
    /// For fixed-length responses this matches `payload_bytes`. For chunked
    /// responses it includes framing and trailers.
    #[serde(default)]
    pub wire_bytes: u64,
}

impl RawWorkerMetrics {
    /// Convert hot-path `[u64; 600]` counters to the public `HashMap<u16, u64>`.
    pub fn status_codes_as_map(&self) -> HashMap<u16, u64> {
        let mut map = HashMap::new();
        for (code, &count) in self.status_counts.iter().enumerate() {
            if count > 0 {
                map.insert(code as u16, count);
            }
        }
        map
    }

    /// Convert to the public `WorkerMetrics` shape used by output and
    /// `MergedMetrics::from_workers`, preferring corrected latency when it is
    /// available for rate-limited runs.
    pub fn into_worker_metrics(self) -> WorkerMetrics {
        let status_codes = self.status_codes_as_map();
        let latency = self.latency_corrected.unwrap_or(self.latency_uncorrected);

        WorkerMetrics {
            worker_id: self.worker_id,
            request_count: self.request_count,
            success_count: self.success_count,
            error_count: self.error_count,
            status_codes,
            latency,
            duration_secs: self.duration_secs,
            payload_bytes: self.payload_bytes,
            wire_bytes: self.wire_bytes,
        }
    }
}

/// Serialize `[u64; 600]` as a sparse JSON map of `{ "200": 1234, "404": 5 }`.
fn serialize_status_counts<S: serde::Serializer>(
    counts: &[u64; 600],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let non_zero = counts.iter().filter(|&&c| c > 0).count();
    let mut map = serializer.serialize_map(Some(non_zero))?;
    for (code, &count) in counts.iter().enumerate() {
        if count > 0 {
            map.serialize_entry(&(code as u16), &count)?;
        }
    }
    map.end()
}

/// Deserialize the sparse JSON map back into `[u64; 600]`.
fn deserialize_status_counts<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[u64; 600], D::Error> {
    let map: HashMap<u16, u64> = HashMap::deserialize(deserializer)?;
    let mut counts = [0u64; 600];
    for (code, count) in map {
        if (code as usize) < 600 {
            counts[code as usize] = count;
        }
    }
    Ok(counts)
}

/// HTTP method for requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
#[allow(clippy::upper_case_acronyms)]
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

/// Current status of a load test
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestStatus {
    /// Whether a test is currently running
    pub running: bool,

    /// Whether the test completed naturally
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,

    /// When the test started (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,

    /// When the test ended (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,

    /// Merged metrics from all workers (populated after completion)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MergedMetrics>,
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

    /// Number of concurrent connections (default: 1)
    #[serde(default)]
    pub connections: Option<u32>,

    /// Number of worker threads (default: CPU count)
    #[serde(default)]
    pub threads: Option<u32>,

    /// Test duration in seconds
    pub duration_seconds: u32,
}

impl StartLoadTestArgs {
    /// Convert to LoadTestParams with defaults applied
    pub fn into_load_test_params(self) -> Result<crate::bench::LoadTestParams, String> {
        let method = match &self.method {
            Some(m) => m.parse()?,
            None => HttpMethod::GET,
        };
        let connections = self.connections.unwrap_or(1);

        if self.total_rate == 0 {
            return Err("total_rate must be at least 1".to_string());
        }

        if connections == 0 {
            return Err("connections must be at least 1".to_string());
        }

        if self.duration_seconds == 0 {
            return Err("duration_seconds must be at least 1".to_string());
        }

        Ok(crate::bench::LoadTestParams {
            url: self.target_url,
            method,
            headers: self.headers.unwrap_or_default(),
            body: self.body,
            connections,
            duration: self.duration_seconds,
            warmup: 0,
            max_throughput: false,
            rate: self.total_rate,
            threads: self.threads,
            hdr_log: None,
        })
    }
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
    /// Latency histogram used for reporting (corrected when available)
    pub latency: HdrLatencyHistogram,
    /// Actual test duration in seconds
    pub duration_secs: f64,
    /// Total decoded payload bytes received in response bodies.
    #[serde(default, alias = "total_bytes")]
    pub payload_bytes: u64,
    /// Total wire-level body bytes consumed.
    #[serde(default)]
    pub wire_bytes: u64,
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
            payload_bytes: 0,
            wire_bytes: 0,
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
    /// Total decoded payload bytes received.
    #[serde(default, alias = "total_bytes")]
    pub payload_bytes: u64,
    /// Total wire-level body bytes consumed.
    #[serde(default)]
    pub wire_bytes: u64,
    /// Payload transfer rate in bytes per second.
    #[serde(default, alias = "transfer_per_sec")]
    pub payload_transfer_per_sec: f64,
    /// Wire transfer rate in bytes per second.
    #[serde(default)]
    pub wire_transfer_per_sec: f64,
    /// Full merged latency histogram for export
    #[serde(default)]
    pub latency_histogram: HdrLatencyHistogram,
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
        let mut payload_bytes: u64 = 0;
        let mut wire_bytes: u64 = 0;

        for w in workers {
            total_requests += w.request_count;
            success_count += w.success_count;
            error_count += w.error_count;
            payload_bytes += w.payload_bytes;
            wire_bytes += w.wire_bytes;
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

        let payload_transfer_per_sec = if max_duration > 0.0 {
            payload_bytes as f64 / max_duration
        } else {
            0.0
        };

        let wire_transfer_per_sec = if max_duration > 0.0 {
            wire_bytes as f64 / max_duration
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
            payload_bytes,
            wire_bytes,
            payload_transfer_per_sec,
            wire_transfer_per_sec,
            latency_histogram: merged_latency,
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
            payload_bytes: 0,
            wire_bytes: 0,
            payload_transfer_per_sec: 0.0,
            wire_transfer_per_sec: 0.0,
            latency_histogram: HdrLatencyHistogram::default(),
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
    fn test_start_args_to_load_test_params() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: Some("POST".to_string()),
            headers: Some(HashMap::from([(
                "Content-Type".to_string(),
                "application/json".to_string(),
            )])),
            body: Some(r#"{"test": true}"#.to_string()),
            total_rate: 1000,
            connections: Some(8),
            threads: Some(4),
            duration_seconds: 30,
        };

        let params = args.into_load_test_params().unwrap();
        assert_eq!(params.method, HttpMethod::POST);
        assert_eq!(params.connections, 8);
        assert_eq!(params.threads, Some(4));
        assert_eq!(params.rate, 1000);
        assert_eq!(params.duration, 30);
        assert_eq!(
            params.headers.get("Content-Type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_load_test_params_defaults() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: None,
            headers: None,
            body: None,
            total_rate: 100,
            connections: None,
            threads: None,
            duration_seconds: 10,
        };

        let params = args.into_load_test_params().unwrap();
        assert_eq!(params.method, HttpMethod::GET);
        assert_eq!(params.connections, 1);
        assert_eq!(params.threads, None);
        assert!(params.headers.is_empty());
    }

    #[test]
    fn test_invalid_load_test_params() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: None,
            headers: None,
            body: None,
            total_rate: 0,
            connections: None,
            threads: None,
            duration_seconds: 10,
        };

        assert!(args.into_load_test_params().is_err());
    }

    #[test]
    fn test_invalid_load_test_connections() {
        let args = StartLoadTestArgs {
            target_url: "http://localhost:3000".to_string(),
            method: None,
            headers: None,
            body: None,
            total_rate: 100,
            connections: Some(0),
            threads: None,
            duration_seconds: 10,
        };

        match args.into_load_test_params() {
            Ok(_) => panic!("zero connections should fail"),
            Err(err) => assert!(err.contains("connections must be at least 1")),
        }
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
        assert!(
            (1.9..=2.1).contains(&p50),
            "p50 should be ~2ms, got {}",
            p50
        );
        assert!(
            (1.9..=2.1).contains(&p99),
            "p99 should be ~2ms, got {}",
            p99
        );
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
        let mut w1 = WorkerMetrics {
            worker_id: 0,
            request_count: 100,
            success_count: 95,
            error_count: 5,
            duration_secs: 10.0,
            payload_bytes: 2_000,
            wire_bytes: 2_200,
            ..WorkerMetrics::default()
        };
        w1.latency.record(5_000);
        w1.latency.record(10_000);

        let mut w2 = WorkerMetrics {
            worker_id: 1,
            request_count: 100,
            success_count: 100,
            error_count: 0,
            duration_secs: 10.0,
            payload_bytes: 3_000,
            wire_bytes: 3_300,
            ..WorkerMetrics::default()
        };
        w2.latency.record(3_000);
        w2.latency.record(7_000);

        let merged = MergedMetrics::from_workers(&[w1, w2]);

        assert_eq!(merged.total_requests, 200);
        assert_eq!(merged.successful_requests, 195);
        assert_eq!(merged.failed_requests, 5);
        assert_eq!(merged.worker_count, 2);
        assert_eq!(merged.rps, 20.0);
        assert_eq!(merged.rpm, 1200.0);
        assert_eq!(merged.payload_bytes, 5_000);
        assert_eq!(merged.wire_bytes, 5_500);
        assert_eq!(merged.payload_transfer_per_sec, 500.0);
        assert_eq!(merged.wire_transfer_per_sec, 550.0);
    }

    #[test]
    fn test_into_worker_metrics_prefers_corrected_latency_when_present() {
        let mut status_counts = [0u64; 600];
        status_counts[200] = 2;

        let mut latency_uncorrected = HdrLatencyHistogram::default();
        latency_uncorrected.record(1_000);

        let mut latency_corrected = HdrLatencyHistogram::default();
        latency_corrected.record(50_000);
        latency_corrected.record(60_000);

        let raw = RawWorkerMetrics {
            worker_id: 3,
            request_count: 2,
            success_count: 2,
            error_count: 0,
            status_counts,
            latency_uncorrected,
            latency_corrected: Some(latency_corrected),
            duration_secs: 1.0,
            payload_bytes: 128,
            wire_bytes: 128,
        };

        let worker = raw.into_worker_metrics();

        assert_eq!(worker.latency.count(), 2);
        assert!(worker.latency.min_ms() >= 49.0);
        assert!(worker.latency.max_ms() >= 59.0);
    }

    #[test]
    fn test_merged_metrics_has_tail_percentiles() {
        let mut w = WorkerMetrics {
            request_count: 10000,
            success_count: 10000,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
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
    fn test_merged_metrics_json_roundtrip_preserves_histogram() {
        let mut w = WorkerMetrics {
            request_count: 1000,
            success_count: 1000,
            duration_secs: 5.0,
            payload_bytes: 4_000,
            wire_bytes: 4_400,
            ..WorkerMetrics::default()
        };
        for i in 1..=1000 {
            w.latency.record(i * 100);
        }

        let original = MergedMetrics::from_workers(&[w]);

        let json = serde_json::to_string(&original).unwrap();
        let restored: MergedMetrics = serde_json::from_str(&json).unwrap();

        assert_eq!(
            original.latency_histogram.to_base64(),
            restored.latency_histogram.to_base64()
        );
        assert_eq!(original.total_requests, restored.total_requests);
        assert_eq!(original.latency_p99_ms, restored.latency_p99_ms);
        assert_eq!(original.payload_bytes, restored.payload_bytes);
        assert_eq!(original.wire_bytes, restored.wire_bytes);
    }

    #[test]
    fn test_merged_metrics_deserializes_legacy_total_bytes_fields() {
        let json = serde_json::json!({
            "total_requests": 10,
            "successful_requests": 10,
            "failed_requests": 0,
            "status_codes": {},
            "rpm": 600.0,
            "rps": 10.0,
            "latency_avg_ms": 1.0,
            "latency_min_ms": 1.0,
            "latency_max_ms": 1.0,
            "latency_p50_ms": 1.0,
            "latency_p90_ms": 1.0,
            "latency_p95_ms": 1.0,
            "latency_p99_ms": 1.0,
            "latency_p999_ms": 1.0,
            "latency_p9999_ms": 1.0,
            "duration_secs": 1.0,
            "worker_count": 1,
            "total_bytes": 1234,
            "transfer_per_sec": 1234.0,
            "latency_histogram": HdrLatencyHistogram::default().to_base64()
        });

        let restored: MergedMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(restored.payload_bytes, 1234);
        assert_eq!(restored.payload_transfer_per_sec, 1234.0);
        assert_eq!(restored.wire_bytes, 0);
        assert_eq!(restored.wire_transfer_per_sec, 0.0);
    }

    #[test]
    fn test_from_workers_empty_slice_gives_default_histogram() {
        let merged = MergedMetrics::from_workers(&[]);
        assert_eq!(merged.total_requests, 0);
        assert_eq!(merged.successful_requests, 0);
        assert_eq!(merged.failed_requests, 0);
        assert_eq!(merged.worker_count, 0);
        assert_eq!(merged.rps, 0.0);
        assert_eq!(merged.latency_avg_ms, 0.0);
        assert_eq!(merged.latency_min_ms, 0.0);
        assert_eq!(merged.latency_max_ms, 0.0);
        assert_eq!(merged.latency_p50_ms, 0.0);
        assert_eq!(merged.latency_p99_ms, 0.0);
        assert_eq!(merged.latency_histogram.count(), 0);
    }

    #[test]
    fn test_from_workers_merges_histogram_data() {
        let mut w1 = WorkerMetrics {
            worker_id: 0,
            request_count: 3,
            success_count: 3,
            duration_secs: 5.0,
            ..WorkerMetrics::default()
        };
        w1.latency.record(1_000); // 1ms
        w1.latency.record(2_000); // 2ms
        w1.latency.record(3_000); // 3ms

        let mut w2 = WorkerMetrics {
            worker_id: 1,
            request_count: 2,
            success_count: 2,
            duration_secs: 5.0,
            ..WorkerMetrics::default()
        };
        w2.latency.record(4_000); // 4ms
        w2.latency.record(100_000); // 100ms

        let merged = MergedMetrics::from_workers(&[w1, w2]);

        assert_eq!(merged.latency_histogram.count(), 5);
        assert!(
            merged.latency_min_ms >= 0.9 && merged.latency_min_ms <= 1.1,
            "min should be ~1ms, got {}",
            merged.latency_min_ms
        );
        assert!(
            merged.latency_max_ms >= 99.0 && merged.latency_max_ms <= 101.0,
            "max should be ~100ms, got {}",
            merged.latency_max_ms
        );

        // base64 roundtrip preserves count
        let encoded = merged.latency_histogram.to_base64();
        let decoded = HdrLatencyHistogram::from_base64(&encoded).unwrap();
        assert_eq!(decoded.count(), 5);
    }

    #[test]
    fn test_status_codes_merging() {
        let mut w1 = WorkerMetrics {
            worker_id: 0,
            request_count: 100,
            success_count: 90,
            error_count: 10,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
        w1.status_codes.insert(200, 80);
        w1.status_codes.insert(201, 10);
        w1.status_codes.insert(404, 5);
        w1.status_codes.insert(500, 5);

        let mut w2 = WorkerMetrics {
            worker_id: 1,
            request_count: 100,
            success_count: 95,
            error_count: 5,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
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
