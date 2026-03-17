//! Max-throughput (closed-loop) benchmark mode
//!
//! Saturates a server with N concurrent async connections for a fixed duration,
//! measuring exact latency and peak RPS.

use crate::loadtest::types::{HdrLatencyHistogram, HttpMethod, MaxThroughputConfig, WorkerMetrics};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

/// Run a max-throughput benchmark, returning per-task metrics.
pub async fn run(config: MaxThroughputConfig) -> Vec<WorkerMetrics> {
    let client = reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .pool_max_idle_per_host(config.connections as usize)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    let warmup_duration = Duration::from_secs(config.warmup_seconds as u64);
    let measurement_duration = Duration::from_secs(config.duration_seconds as u64);
    let warmup_deadline = Instant::now() + warmup_duration;
    let measurement_deadline = warmup_deadline + measurement_duration;

    if config.warmup_seconds > 0 {
        eprintln!("Warming up ({}s)...", config.warmup_seconds);
    }

    let mut handles = Vec::with_capacity(config.connections as usize);

    for task_id in 0..config.connections {
        let client = client.clone();
        let config = config.clone();

        handles.push(tokio::spawn(async move {
            run_task(
                task_id,
                &client,
                &config,
                warmup_deadline,
                measurement_deadline,
            )
            .await
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(metrics) => results.push(metrics),
            Err(e) => eprintln!("task join error: {}", e),
        }
    }

    results
}

async fn run_task(
    task_id: u32,
    client: &reqwest::Client,
    config: &MaxThroughputConfig,
    warmup_deadline: Instant,
    measurement_deadline: Instant,
) -> WorkerMetrics {
    // Phase 1: Warmup
    while Instant::now() < warmup_deadline {
        let _ = send_request_async(client, config).await;
    }

    // Phase 2: Measurement
    let measurement_start = Instant::now();
    let mut latency = HdrLatencyHistogram::default();
    let mut request_count: u64 = 0;
    let mut success_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut status_codes: HashMap<u16, u64> = HashMap::new();
    let mut total_bytes: u64 = 0;

    while Instant::now() < measurement_deadline {
        let req_start = Instant::now();
        let result = send_request_async(client, config).await;
        let latency_us = req_start.elapsed().as_micros() as u64;

        request_count += 1;
        latency.record(latency_us);

        match result {
            Ok((status, is_success, body_len)) => {
                *status_codes.entry(status).or_insert(0) += 1;
                total_bytes += body_len;
                if is_success {
                    success_count += 1;
                } else {
                    error_count += 1;
                }
            }
            Err(e) => {
                error_count += 1;
                if error_count <= 5 {
                    eprintln!("[task {}] request error: {}", task_id, e);
                }
            }
        }
    }

    WorkerMetrics {
        worker_id: task_id,
        request_count,
        success_count,
        error_count,
        status_codes,
        latency,
        duration_secs: measurement_start.elapsed().as_secs_f64(),
        total_bytes,
    }
}

async fn send_request_async(
    client: &reqwest::Client,
    config: &MaxThroughputConfig,
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

    let response = builder.send().await?;
    let status_code = response.status().as_u16();
    let is_success = response.status().is_success();

    let body = response.bytes().await?;

    Ok((status_code, is_success, body.len() as u64))
}
