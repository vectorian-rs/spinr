//! Process orchestration: spawn worker processes, pipe config in, read metrics out.
//!
//! Replaces the old `manager.rs` which used JSON-over-argv config passing and
//! JSON-file-on-disk metrics collection.
//!
//! Pipe protocol (all lengths are little-endian u32):
//!
//! Parent → Child (stdin):
//!   [4 bytes: config_json_len] [config_json_bytes]
//!   [4 bytes: request_bytes_len] [request_bytes]
//!
//! Child → Parent (stdout at shutdown):
//!   [4 bytes: metrics_json_len] [metrics_json_bytes]

use crate::loadtest::engine::EngineError;
use crate::loadtest::types::{EngineConfig, RawWorkerMetrics};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Engine(#[from] EngineError),
    #[error("worker {worker_id} exited unsuccessfully: {status}")]
    WorkerExited { worker_id: u32, status: ExitStatus },
    #[error("worker {worker_id} metrics read error: {source}")]
    WorkerMetricsRead {
        worker_id: u32,
        #[source]
        source: std::io::Error,
    },
    #[error("worker {worker_id} metrics parse error: {source}")]
    WorkerMetricsParse {
        worker_id: u32,
        #[source]
        source: serde_json::Error,
    },
}

/// Spawn N worker processes, wait for completion, collect metrics.
pub fn run_workers(
    configs: Vec<EngineConfig>,
    request_bytes: &[u8],
) -> Result<Vec<RawWorkerMetrics>, OrchestratorError> {
    let exe = std::env::current_exe()?;
    let mut children: Vec<(u32, Child)> = Vec::with_capacity(configs.len());

    for config in &configs {
        let mut child = Command::new(&exe)
            .arg("--run-engine")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        // Write config + request bytes to child stdin
        {
            let stdin = child.stdin.as_mut().expect("stdin is piped");
            write_frame(stdin, &serde_json::to_vec(config)?)?;
            write_frame(stdin, request_bytes)?;
        }
        // Close stdin so the child knows config is complete
        child.stdin.take();

        eprintln!(
            "[orchestrator] spawned worker {} (PID: {})",
            config.worker_id,
            child.id()
        );
        children.push((config.worker_id, child));
    }

    eprintln!("[orchestrator] all {} workers started", children.len());

    // Wait for all children and collect metrics
    let mut results = Vec::with_capacity(children.len());
    let mut first_error = None;
    for (worker_id, mut child) in children {
        match collect_worker_metrics(worker_id, &mut child) {
            Ok(metrics) => {
                eprintln!(
                    "[orchestrator] worker {} reported {} requests",
                    worker_id, metrics.request_count
                );
                results.push(metrics);
            }
            Err(err) => {
                eprintln!("[orchestrator] {}", err);
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(results)
}

/// Child-process entry point: read config and request bytes from stdin,
/// run the engine, write metrics to stdout.
pub fn run_engine_child() -> Result<(), OrchestratorError> {
    let mut stdin = std::io::stdin().lock();

    let config_json = read_frame(&mut stdin)?;
    let config: EngineConfig = serde_json::from_slice(&config_json)?;

    let request_bytes = read_frame(&mut stdin)?;
    drop(stdin);

    eprintln!(
        "[engine {}] starting: {} connections to {} for {}s",
        config.worker_id, config.connections, config.remote_addr, config.duration_seconds
    );

    let metrics = crate::loadtest::engine::run(config, &request_bytes)?;

    let metrics_json = serde_json::to_vec(&metrics)?;
    let mut stdout = std::io::stdout().lock();
    write_frame(&mut stdout, &metrics_json)?;
    stdout.flush()?;

    Ok(())
}

/// Write a length-prefixed frame: [u32 LE length] [bytes]
fn write_frame(w: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    let len = data.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(data)?;
    Ok(())
}

/// Read a length-prefixed frame: [u32 LE length] [bytes]
fn read_frame(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    Ok(data)
}

fn collect_worker_metrics(
    worker_id: u32,
    child: &mut Child,
) -> Result<RawWorkerMetrics, OrchestratorError> {
    let status = child.wait()?;
    ensure_worker_success(worker_id, status)?;

    let mut stdout = child.stdout.take().expect("stdout is piped");
    read_worker_metrics(worker_id, &mut stdout)
}

fn ensure_worker_success(worker_id: u32, status: ExitStatus) -> Result<(), OrchestratorError> {
    if status.success() {
        Ok(())
    } else {
        Err(OrchestratorError::WorkerExited { worker_id, status })
    }
}

fn read_worker_metrics(
    worker_id: u32,
    stdout: &mut impl Read,
) -> Result<RawWorkerMetrics, OrchestratorError> {
    let metrics_json = read_frame(stdout).map_err(|source| OrchestratorError::WorkerMetricsRead {
        worker_id,
        source,
    })?;

    serde_json::from_slice(&metrics_json).map_err(|source| OrchestratorError::WorkerMetricsParse {
        worker_id,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn test_frame_roundtrip() {
        let data = b"hello, pipe protocol";
        let mut buf = Vec::new();
        write_frame(&mut buf, data).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_frame(&mut cursor).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn test_frame_empty() {
        let data = b"";
        let mut buf = Vec::new();
        write_frame(&mut buf, data).unwrap();
        assert_eq!(buf.len(), 4); // just the length prefix

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_frame(&mut cursor).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_frame_large() {
        let data = vec![0xABu8; 100_000];
        let mut buf = Vec::new();
        write_frame(&mut buf, &data).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_frame(&mut cursor).unwrap();
        assert_eq!(result.len(), 100_000);
        assert!(result.iter().all(|&b| b == 0xAB));
    }

    #[cfg(unix)]
    #[test]
    fn test_worker_exit_failure_is_error() {
        let err = ensure_worker_success(7, ExitStatus::from_raw(1 << 8)).unwrap_err();
        match err {
            OrchestratorError::WorkerExited { worker_id, status } => {
                assert_eq!(worker_id, 7);
                assert!(!status.success());
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn test_worker_metrics_read_failure_is_error() {
        let mut cursor = Cursor::new(vec![0u8; 2]);
        let err = read_worker_metrics(3, &mut cursor).unwrap_err();
        match err {
            OrchestratorError::WorkerMetricsRead { worker_id, .. } => {
                assert_eq!(worker_id, 3);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn test_worker_metrics_parse_failure_is_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, br#"{"not":"raw_worker_metrics"}"#).unwrap();

        let mut cursor = Cursor::new(buf);
        let err = read_worker_metrics(5, &mut cursor).unwrap_err();
        match err {
            OrchestratorError::WorkerMetricsParse { worker_id, .. } => {
                assert_eq!(worker_id, 5);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn test_status_counts_conversion() {
        use crate::loadtest::types::{HdrLatencyHistogram, RawWorkerMetrics};

        let mut counts = [0u64; 600];
        counts[200] = 950;
        counts[201] = 30;
        counts[404] = 15;
        counts[500] = 5;

        let raw = RawWorkerMetrics {
            worker_id: 0,
            request_count: 1000,
            success_count: 980,
            error_count: 20,
            status_counts: counts,
            latency_uncorrected: HdrLatencyHistogram::default(),
            latency_corrected: None,
            duration_secs: 10.0,
            payload_bytes: 0,
            wire_bytes: 0,
        };

        let map = raw.status_codes_as_map();
        assert_eq!(map.len(), 4);
        assert_eq!(map[&200], 950);
        assert_eq!(map[&201], 30);
        assert_eq!(map[&404], 15);
        assert_eq!(map[&500], 5);
    }

    #[test]
    fn test_raw_metrics_json_roundtrip() {
        use crate::loadtest::types::{HdrLatencyHistogram, RawWorkerMetrics};

        let mut counts = [0u64; 600];
        counts[200] = 100;
        counts[503] = 3;

        let original = RawWorkerMetrics {
            worker_id: 7,
            request_count: 103,
            success_count: 100,
            error_count: 3,
            status_counts: counts,
            latency_uncorrected: HdrLatencyHistogram::default(),
            latency_corrected: None,
            duration_secs: 5.0,
            payload_bytes: 12345,
            wire_bytes: 12500,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: RawWorkerMetrics = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.worker_id, 7);
        assert_eq!(restored.request_count, 103);
        assert_eq!(restored.status_counts[200], 100);
        assert_eq!(restored.status_counts[503], 3);
        assert_eq!(restored.status_counts[404], 0);
        assert_eq!(restored.payload_bytes, 12345);
        assert_eq!(restored.wire_bytes, 12500);
    }
}
