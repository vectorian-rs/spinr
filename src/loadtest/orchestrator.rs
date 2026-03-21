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
use std::process::{Child, Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Engine(#[from] EngineError),
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
    for (worker_id, mut child) in children {
        let status = child.wait()?;
        if !status.success() {
            eprintln!(
                "[orchestrator] worker {} exited with status: {:?}",
                worker_id, status
            );
            continue;
        }

        let mut stdout = child.stdout.take().expect("stdout is piped");
        match read_frame(&mut stdout) {
            Ok(metrics_json) => match serde_json::from_slice::<RawWorkerMetrics>(&metrics_json) {
                Ok(metrics) => {
                    eprintln!(
                        "[orchestrator] worker {} reported {} requests",
                        worker_id, metrics.request_count
                    );
                    results.push(metrics);
                }
                Err(e) => {
                    eprintln!(
                        "[orchestrator] worker {} metrics parse error: {}",
                        worker_id, e
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "[orchestrator] worker {} metrics read error: {}",
                    worker_id, e
                );
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
