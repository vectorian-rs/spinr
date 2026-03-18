//! Legacy max-throughput module — replaced by engine.rs
//!
//! This module is kept as a stub to maintain compilation while the new
//! engine is being built. It will be deleted once the migration is complete.

use crate::loadtest::types::{MaxThroughputConfig, WorkerMetrics};

/// Legacy max-throughput entry point — no longer functional.
#[allow(dead_code)]
pub async fn run(_config: MaxThroughputConfig) -> Vec<WorkerMetrics> {
    eprintln!("[max_throughput] legacy mode is disabled; use the new engine");
    vec![]
}
