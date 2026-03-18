//! Human-readable output formatters
//!
//! TODO: Implement trace result formatter
//! TODO: Implement load test metrics formatter

use crate::loadtest::types::{MergedMetrics, format_bytes};
use std::io::Write;
use std::path::Path;

/// Print load test metrics in human-readable format
pub fn print_metrics(metrics: &MergedMetrics) {
    println!();
    println!("Results:");
    println!("  Total requests:    {}", metrics.total_requests);
    println!("  Successful:        {}", metrics.successful_requests);
    println!("  Failed:            {}", metrics.failed_requests);

    if !metrics.status_codes.is_empty() {
        println!();
        println!("  Status codes:");
        let mut codes: Vec<_> = metrics.status_codes.iter().collect();
        codes.sort_by_key(|(code, _)| *code);
        for (code, count) in codes {
            println!("    {}: {}", code, count);
        }
    }

    println!();
    println!("  Actual RPS:        {:.2}", metrics.rps);
    println!(
        "  Transfer/sec:      {}",
        format_bytes(metrics.transfer_per_sec)
    );
    println!("  Actual RPM:        {:.2}", metrics.rpm);
    println!();
    println!("  Latency avg:       {:.2}ms", metrics.latency_avg_ms);
    println!("  Latency min:       {:.2}ms", metrics.latency_min_ms);
    println!("  Latency max:       {:.2}ms", metrics.latency_max_ms);
    println!("  Latency p50:       {:.2}ms", metrics.latency_p50_ms);
    println!("  Latency p90:       {:.2}ms", metrics.latency_p90_ms);
    println!("  Latency p95:       {:.2}ms", metrics.latency_p95_ms);
    println!("  Latency p99:       {:.2}ms", metrics.latency_p99_ms);
    println!("  Latency p99.9:     {:.2}ms", metrics.latency_p999_ms);
    println!("  Latency p99.99:    {:.2}ms", metrics.latency_p9999_ms);
    println!();
    println!("  Duration:          {:.2}s", metrics.duration_secs);
    println!("  Workers:           {}", metrics.worker_count);
}

/// Write HDR Histogram log file in the standard .hlog format.
///
/// The format is compatible with HdrHistogramLogAnalyzer and hdrhistogram-plotter.
/// Values are in microseconds; `MaxValueUnitRatio: 1000.0` tells tools to display in ms.
pub fn write_hdr_log(path: &Path, metrics: &MergedMetrics) -> std::io::Result<()> {
    use std::time::SystemTime;

    let epoch_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let mut f = std::fs::File::create(path)?;
    writeln!(f, "#[StartTime: {:.3} (seconds since epoch)]", epoch_secs)?;
    writeln!(f, "#[Histogram log format version 1.2]")?;
    writeln!(f, "#[MaxValueUnitRatio: 1000.0]")?;
    writeln!(
        f,
        "\"StartTimestamp\",\"EndTimestamp\",\"MaxValue\",\"Histogram\""
    )?;

    let max_us = metrics.latency_max_ms * 1000.0;
    let base64 = metrics.latency_histogram.to_base64();
    writeln!(
        f,
        "0.000000,{:.6},{:.3},{}",
        metrics.duration_secs, max_us, base64
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loadtest::types::{HdrLatencyHistogram, WorkerMetrics};
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("spinr_test_{}_{}", std::process::id(), name))
    }

    #[test]
    fn test_write_hdr_log_creates_valid_file() {
        let mut w = WorkerMetrics {
            request_count: 500,
            success_count: 500,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
        for i in 1..=500 {
            w.latency.record(i * 100); // 0.1ms to 50ms
        }
        let metrics = MergedMetrics::from_workers(&[w]);

        let path = temp_path("valid.hlog");
        write_hdr_log(&path, &metrics).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5, "expected 5 lines, got {}", lines.len());

        // Line 0: StartTime header
        assert!(lines[0].starts_with("#[StartTime:"), "line 0: {}", lines[0]);
        assert!(
            lines[0].ends_with("(seconds since epoch)]"),
            "line 0: {}",
            lines[0]
        );

        // Line 1: format version
        assert_eq!(lines[1], "#[Histogram log format version 1.2]");

        // Line 2: unit ratio
        assert_eq!(lines[2], "#[MaxValueUnitRatio: 1000.0]");

        // Line 3: CSV header
        assert_eq!(
            lines[3],
            "\"StartTimestamp\",\"EndTimestamp\",\"MaxValue\",\"Histogram\""
        );

        // Line 4: data row
        let fields: Vec<&str> = lines[4].splitn(4, ',').collect();
        assert_eq!(fields.len(), 4, "expected 4 CSV fields");
        assert_eq!(fields[0], "0.000000");

        let end_ts: f64 = fields[1].parse().expect("EndTimestamp should be f64");
        assert!(
            (end_ts - 10.0).abs() < 0.1,
            "EndTimestamp ~10s, got {}",
            end_ts
        );

        let max_val: f64 = fields[2].parse().expect("MaxValue should be f64");
        let expected_max_us = metrics.latency_max_ms * 1000.0;
        assert!(
            (max_val - expected_max_us).abs() < 1.0,
            "MaxValue ~{}us, got {}",
            expected_max_us,
            max_val
        );

        // Decode base64 histogram
        let decoded = HdrLatencyHistogram::from_base64(fields[3]).unwrap();
        assert_eq!(decoded.count(), 500);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_write_hdr_log_empty_histogram() {
        let metrics = MergedMetrics::default();
        let path = temp_path("empty.hlog");
        write_hdr_log(&path, &metrics).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);

        let fields: Vec<&str> = lines[4].splitn(4, ',').collect();
        let decoded = HdrLatencyHistogram::from_base64(fields[3]).unwrap();
        assert_eq!(decoded.count(), 0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_write_hdr_log_base64_roundtrip_preserves_distribution() {
        let mut w = WorkerMetrics {
            request_count: 10_000,
            success_count: 10_000,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
        for i in 1..=10_000 {
            w.latency.record(i * 10); // 0.01ms to 100ms
        }
        let metrics = MergedMetrics::from_workers(&[w]);

        let path = temp_path("roundtrip.hlog");
        write_hdr_log(&path, &metrics).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let fields: Vec<&str> = content.lines().nth(4).unwrap().splitn(4, ',').collect();
        let decoded = HdrLatencyHistogram::from_base64(fields[3]).unwrap();

        let orig = &metrics.latency_histogram;
        // p50, p99, p99.9 match within 0.1ms
        assert!(
            (orig.percentile_ms(50.0) - decoded.percentile_ms(50.0)).abs() < 0.1,
            "p50 mismatch: {} vs {}",
            orig.percentile_ms(50.0),
            decoded.percentile_ms(50.0)
        );
        assert!(
            (orig.percentile_ms(99.0) - decoded.percentile_ms(99.0)).abs() < 0.1,
            "p99 mismatch: {} vs {}",
            orig.percentile_ms(99.0),
            decoded.percentile_ms(99.0)
        );
        assert!(
            (orig.percentile_ms(99.9) - decoded.percentile_ms(99.9)).abs() < 0.1,
            "p99.9 mismatch: {} vs {}",
            orig.percentile_ms(99.9),
            decoded.percentile_ms(99.9)
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_hdr_log_end_to_end_pipeline() {
        // Worker 1: 1000 requests, low latency (1-10ms)
        let mut w1 = WorkerMetrics {
            worker_id: 0,
            request_count: 1000,
            success_count: 1000,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
        for i in 1..=1000 {
            w1.latency.record(1_000 + i * 9); // ~1ms to ~10ms
        }

        // Worker 2: 1000 requests, higher latency (10-100ms)
        let mut w2 = WorkerMetrics {
            worker_id: 1,
            request_count: 1000,
            success_count: 1000,
            duration_secs: 10.0,
            ..WorkerMetrics::default()
        };
        for i in 1..=1000 {
            w2.latency.record(10_000 + i * 90); // ~10ms to ~100ms
        }

        // Merge
        let metrics = MergedMetrics::from_workers(&[w1, w2]);
        assert_eq!(metrics.latency_histogram.count(), 2000);

        // Write
        let path = temp_path("e2e.hlog");
        write_hdr_log(&path, &metrics).unwrap();

        // Read and parse
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);

        // Verify headers
        assert!(lines[0].starts_with("#[StartTime:"));
        assert_eq!(lines[1], "#[Histogram log format version 1.2]");
        assert_eq!(lines[2], "#[MaxValueUnitRatio: 1000.0]");
        assert_eq!(
            lines[3],
            "\"StartTimestamp\",\"EndTimestamp\",\"MaxValue\",\"Histogram\""
        );

        // Decode and verify
        let fields: Vec<&str> = lines[4].splitn(4, ',').collect();
        let decoded = HdrLatencyHistogram::from_base64(fields[3]).unwrap();
        assert_eq!(decoded.count(), 2000);

        // Percentiles should match original exactly (same serialization)
        let orig = &metrics.latency_histogram;
        assert_eq!(orig.percentile_ms(50.0), decoded.percentile_ms(50.0));
        assert_eq!(orig.percentile_ms(99.0), decoded.percentile_ms(99.0));
        assert_eq!(orig.percentile_ms(99.9), decoded.percentile_ms(99.9));

        std::fs::remove_file(&path).ok();
    }
}
