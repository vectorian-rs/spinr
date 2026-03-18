//! Legacy worker module — replaced by engine.rs
//!
//! This module is kept as a stub to maintain compilation while the new
//! engine is being built. It will be deleted once the migration is complete.

use crate::loadtest::types::WorkerConfig;

/// Legacy worker entry point — no longer functional.
pub fn run(_config: WorkerConfig) -> anyhow::Result<()> {
    anyhow::bail!("legacy worker is disabled; use the new engine")
}

#[cfg(test)]
mod tests {
    use crate::loadtest::types::{HttpMethod, WorkerConfig};
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
