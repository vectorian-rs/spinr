//! Worker process: performs rate-limited HTTP requests
//!
//! Each worker runs in its own process with a dedicated rate limiter.
//! Uses blocking reqwest for simplicity in tight request loops.
//! Writes metrics to a file on completion for aggregation by the manager.

use crate::loadtest::types::{HdrLatencyHistogram, HttpMethod, WorkerConfig, WorkerMetrics};
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Run the worker loop until duration expires
pub fn run(config: WorkerConfig) -> anyhow::Result<()> {
    let rate = NonZeroU32::new(config.rate).unwrap_or(nonzero!(1u32));
    let limiter = RateLimiter::direct(Quota::per_second(rate));

    // Build client with connection pooling and keepalive
    let client = reqwest::blocking::Client::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .timeout(Duration::from_secs(30))
        .build()?;

    // Calculate sleep interval for rate limiting
    let sleep_micros = 1_000_000 / config.rate as u64;

    eprintln!(
        "[worker {}] Starting: {} {} at {} RPS for {}s",
        config.worker_id, config.method, config.target_url, config.rate, config.duration_seconds
    );

    let start_time = Instant::now();
    let warmup_deadline = start_time + Duration::from_secs(config.warmup_seconds as u64);
    let measurement_deadline =
        warmup_deadline + Duration::from_secs(config.duration_seconds as u64);

    // Phase 1: Warmup
    if config.warmup_seconds > 0 {
        eprintln!(
            "[worker {}] Warming up ({}s)...",
            config.worker_id, config.warmup_seconds
        );
        while Instant::now() < warmup_deadline {
            while limiter.check().is_err() {
                thread::sleep(Duration::from_micros(sleep_micros / 10));
                if Instant::now() >= warmup_deadline {
                    break;
                }
            }
            if Instant::now() >= warmup_deadline {
                break;
            }
            let _ = send_request(&client, &config);
        }
    }

    // Phase 2: Measurement
    let measurement_start = Instant::now();
    let mut metrics = WorkerMetrics {
        worker_id: config.worker_id,
        request_count: 0,
        success_count: 0,
        error_count: 0,
        status_codes: std::collections::HashMap::new(),
        latency: HdrLatencyHistogram::default(),
        duration_secs: 0.0,
        total_bytes: 0,
    };

    while Instant::now() < measurement_deadline {
        while limiter.check().is_err() {
            thread::sleep(Duration::from_micros(sleep_micros / 10));
            if Instant::now() >= measurement_deadline {
                break;
            }
        }

        if Instant::now() >= measurement_deadline {
            break;
        }

        let request_start = Instant::now();
        let result = send_request(&client, &config);
        let latency_us = request_start.elapsed().as_micros() as u64;

        metrics.request_count += 1;
        metrics.latency.record(latency_us);

        match result {
            Ok((status_code, is_success, body_len)) => {
                *metrics.status_codes.entry(status_code).or_insert(0) += 1;
                metrics.total_bytes += body_len;

                if is_success {
                    metrics.success_count += 1;
                } else {
                    metrics.error_count += 1;
                }
            }
            Err(e) => {
                metrics.error_count += 1;
                if metrics.error_count <= 5 {
                    eprintln!("[worker {}] Request error: {}", config.worker_id, e);
                }
            }
        }

        if metrics.request_count % 1000 == 0 {
            eprintln!(
                "[worker {}] Progress: {} requests ({} ok, {} err), avg latency: {:.2}ms",
                config.worker_id,
                metrics.request_count,
                metrics.success_count,
                metrics.error_count,
                metrics.latency.mean_ms()
            );
        }
    }

    metrics.duration_secs = measurement_start.elapsed().as_secs_f64();

    eprintln!(
        "[worker {}] Finished: {} requests ({} ok, {} err) in {:.2}s, avg latency: {:.2}ms",
        config.worker_id,
        metrics.request_count,
        metrics.success_count,
        metrics.error_count,
        metrics.duration_secs,
        metrics.latency.mean_ms()
    );

    if let Some(ref metrics_dir) = config.metrics_dir {
        write_metrics(&metrics, metrics_dir)?;
    }

    Ok(())
}

/// Write worker metrics to a JSON file
fn write_metrics(metrics: &WorkerMetrics, metrics_dir: &str) -> anyhow::Result<()> {
    let path = Path::new(metrics_dir).join(format!("worker_{}.json", metrics.worker_id));
    let json = serde_json::to_string_pretty(metrics)?;
    fs::write(&path, json)?;
    eprintln!(
        "[worker {}] Wrote metrics to {}",
        metrics.worker_id,
        path.display()
    );
    Ok(())
}

/// Send a single HTTP request
fn send_request(
    client: &reqwest::blocking::Client,
    config: &WorkerConfig,
) -> Result<(u16, bool, u64), reqwest::Error> {
    let mut builder = match config.method {
        HttpMethod::GET => client.get(&config.target_url),
        HttpMethod::POST => client.post(&config.target_url),
        HttpMethod::PUT => client.put(&config.target_url),
        HttpMethod::DELETE => client.delete(&config.target_url),
        HttpMethod::PATCH => client.patch(&config.target_url),
        HttpMethod::HEAD => client.head(&config.target_url),
        HttpMethod::OPTIONS => client.request(reqwest::Method::OPTIONS, &config.target_url),
    };

    for (key, value) in &config.headers {
        builder = builder.header(key, value);
    }

    if let Some(ref body) = config.body {
        builder = builder.body(body.clone());
    }

    let response = builder.send()?;
    let status_code = response.status().as_u16();
    let is_success = response.status().is_success();

    let body = response.bytes()?;

    Ok((status_code, is_success, body.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_worker_config_creation() {
        let config = WorkerConfig {
            target_url: "http://localhost:8080".to_string(),
            method: HttpMethod::POST,
            headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            body: Some(r#"{"test": 1}"#.to_string()),
            rate: 100,
            duration_seconds: 1,
            warmup_seconds: 0,
            worker_id: 0,
            metrics_dir: None,
        };

        assert_eq!(config.rate, 100);
        assert_eq!(config.method, HttpMethod::POST);
        assert_eq!(config.worker_id, 0);
    }
}
