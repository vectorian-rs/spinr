//! Manager process: spawns and coordinates worker processes
//!
//! The manager receives a TestConfig, calculates per-worker rates,
//! spawns N worker processes, waits for them to complete, and merges metrics.

use crate::loadtest::types::{MergedMetrics, TestConfig, WorkerConfig, WorkerMetrics};
use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Instant;

/// Run the manager: spawn workers, wait for completion, merge metrics
pub fn run(config: TestConfig) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let process_count = config.process_count;
    let rate_per_worker = config.total_rate / process_count;
    let remainder = config.total_rate % process_count;

    eprintln!(
        "[manager] Starting {} workers at ~{} RPS each (total: {} RPS)",
        process_count, rate_per_worker, config.total_rate
    );
    eprintln!(
        "[manager] Target: {} {} for {}s",
        config.method, config.target_url, config.duration_seconds
    );

    if let Some(ref dir) = config.metrics_dir {
        eprintln!("[manager] Metrics directory: {}", dir);
    }

    let start = Instant::now();
    let mut children: Vec<Child> = Vec::with_capacity(process_count as usize);
    let mut worker_ids: Vec<u32> = Vec::new();

    // Spawn worker processes
    for i in 0..process_count {
        let worker_rate = if i < remainder {
            rate_per_worker + 1
        } else {
            rate_per_worker
        };

        if worker_rate == 0 {
            continue;
        }

        let worker_config = WorkerConfig {
            target_url: config.target_url.clone(),
            method: config.method,
            headers: config.headers.clone(),
            body: config.body.clone(),
            rate: worker_rate,
            duration_seconds: config.duration_seconds,
            warmup_seconds: config.warmup_seconds,
            worker_id: i,
            metrics_dir: config.metrics_dir.clone(),
        };

        let config_json = serde_json::to_string(&worker_config)?;

        let child = Command::new(&exe)
            .arg("--run-worker")
            .arg(&config_json)
            .spawn()?;

        eprintln!(
            "[manager] Spawned worker {} (PID: {}, rate: {} RPS)",
            i,
            child.id(),
            worker_rate
        );
        children.push(child);
        worker_ids.push(i);
    }

    eprintln!("[manager] All {} workers started", children.len());

    // Wait for all workers to complete
    let mut exit_codes: Vec<Option<i32>> = Vec::new();
    for (i, mut child) in children.into_iter().enumerate() {
        match child.wait() {
            Ok(status) => {
                exit_codes.push(status.code());
                eprintln!("[manager] Worker {} exited with status: {:?}", i, status);
            }
            Err(e) => {
                eprintln!("[manager] Worker {} wait error: {}", i, e);
                exit_codes.push(None);
            }
        }
    }

    let elapsed = start.elapsed();
    let success_count = exit_codes.iter().filter(|c| *c == &Some(0)).count();

    eprintln!(
        "[manager] Test completed in {:.2}s ({}/{} workers succeeded)",
        elapsed.as_secs_f64(),
        success_count,
        exit_codes.len()
    );

    // Merge metrics if metrics_dir is specified
    if let Some(ref metrics_dir) = config.metrics_dir {
        match merge_worker_metrics(metrics_dir, &worker_ids) {
            Ok(merged) => {
                eprintln!(
                    "[manager] Merged metrics: {} requests, {:.2} RPS, avg latency {:.2}ms",
                    merged.total_requests, merged.rps, merged.latency_avg_ms
                );
            }
            Err(e) => {
                eprintln!("[manager] Failed to merge metrics: {}", e);
            }
        }
    }

    Ok(())
}

/// Read and merge metrics from all worker files
fn merge_worker_metrics(metrics_dir: &str, worker_ids: &[u32]) -> anyhow::Result<MergedMetrics> {
    let dir_path = Path::new(metrics_dir);
    let mut worker_metrics: Vec<WorkerMetrics> = Vec::new();

    for &worker_id in worker_ids {
        let file_path = dir_path.join(format!("worker_{}.json", worker_id));

        match fs::read_to_string(&file_path) {
            Ok(content) => match serde_json::from_str::<WorkerMetrics>(&content) {
                Ok(metrics) => {
                    eprintln!(
                        "[manager] Read metrics from worker {}: {} requests",
                        worker_id, metrics.request_count
                    );
                    worker_metrics.push(metrics);
                }
                Err(e) => {
                    eprintln!(
                        "[manager] Failed to parse metrics from worker {}: {}",
                        worker_id, e
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "[manager] Failed to read metrics from worker {}: {}",
                    worker_id, e
                );
            }
        }
    }

    let merged = MergedMetrics::from_workers(&worker_metrics);

    // Write merged metrics
    let merged_path = dir_path.join("merged_metrics.json");
    let json = serde_json::to_string_pretty(&merged)?;
    fs::write(&merged_path, json)?;
    eprintln!(
        "[manager] Wrote merged metrics to {}",
        merged_path.display()
    );

    Ok(merged)
}

/// Calculate rate distribution across workers
#[cfg(test)]
fn calculate_rates(total_rate: u32, process_count: u32) -> Vec<u32> {
    if process_count == 0 {
        return vec![];
    }

    let base_rate = total_rate / process_count;
    let remainder = total_rate % process_count;

    (0..process_count)
        .map(|i| {
            if i < remainder {
                base_rate + 1
            } else {
                base_rate
            }
        })
        .filter(|&r| r > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_distribution_even() {
        let rates = calculate_rates(100, 4);
        assert_eq!(rates, vec![25, 25, 25, 25]);
        assert_eq!(rates.iter().sum::<u32>(), 100);
    }

    #[test]
    fn test_rate_distribution_remainder() {
        let rates = calculate_rates(100, 3);
        assert_eq!(rates, vec![34, 33, 33]);
        assert_eq!(rates.iter().sum::<u32>(), 100);
    }

    #[test]
    fn test_rate_distribution_more_workers_than_rate() {
        let rates = calculate_rates(3, 5);
        assert_eq!(rates, vec![1, 1, 1]);
        assert_eq!(rates.iter().sum::<u32>(), 3);
    }

    #[test]
    fn test_rate_distribution_single_worker() {
        let rates = calculate_rates(1000, 1);
        assert_eq!(rates, vec![1000]);
    }

    #[test]
    fn test_rate_distribution_zero_workers() {
        let rates = calculate_rates(100, 0);
        assert!(rates.is_empty());
    }
}
