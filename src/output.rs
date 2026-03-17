//! Human-readable output formatters
//!
//! TODO: Implement trace result formatter
//! TODO: Implement load test metrics formatter

use crate::loadtest::types::{MergedMetrics, format_bytes};

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
