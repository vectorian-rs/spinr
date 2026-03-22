use crate::loadtest::types::{EngineConfig, EngineMode, HttpMethod};
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerCount(NonZeroU32);

impl WorkerCount {
    pub fn new(value: u32) -> Self {
        Self(NonZeroU32::new(value.max(1)).expect("worker count is clamped to >= 1"))
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionCount(NonZeroU32);

impl ConnectionCount {
    pub fn new(value: u32) -> Self {
        Self(NonZeroU32::new(value.max(1)).expect("connection count is clamped to >= 1"))
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestsPerSecond(NonZeroU64);

impl RequestsPerSecond {
    pub fn new(value: u64) -> Self {
        Self(NonZeroU64::new(value.max(1)).expect("RPS is clamped to >= 1"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadPlanMode {
    MaxThroughput,
    RateLimited {
        total_requests_per_second: RequestsPerSecond,
        latency_correction: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    MaxThroughput,
    RateLimited {
        requests_per_second: RequestsPerSecond,
        latency_correction: bool,
    },
}

impl WorkerMode {
    fn into_engine_mode(self) -> EngineMode {
        match self {
            WorkerMode::MaxThroughput => EngineMode::MaxThroughput,
            WorkerMode::RateLimited {
                requests_per_second,
                latency_correction,
            } => EngineMode::RateLimited {
                requests_per_second: requests_per_second.get(),
                latency_correction,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPlan {
    pub worker_id: u32,
    pub connections: ConnectionCount,
    pub mode: WorkerMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPlan {
    total_connections: ConnectionCount,
    workers: Vec<WorkerPlan>,
}

impl LoadPlan {
    pub fn build(
        requested_workers: WorkerCount,
        requested_connections: ConnectionCount,
        mode: LoadPlanMode,
    ) -> Self {
        let mut effective_workers = requested_workers.get().min(requested_connections.get());
        if let LoadPlanMode::RateLimited {
            total_requests_per_second,
            ..
        } = mode
        {
            effective_workers = effective_workers
                .min(total_requests_per_second.get().min(u64::from(u32::MAX)) as u32);
        }

        let connection_shares = distribute_u32(requested_connections.get(), effective_workers);
        let rate_shares = match mode {
            LoadPlanMode::MaxThroughput => None,
            LoadPlanMode::RateLimited {
                total_requests_per_second,
                ..
            } => Some(distribute_u64(
                total_requests_per_second.get(),
                effective_workers,
            )),
        };

        let workers = connection_shares
            .into_iter()
            .enumerate()
            .map(|(worker_id, connections)| {
                let mode = match mode {
                    LoadPlanMode::MaxThroughput => WorkerMode::MaxThroughput,
                    LoadPlanMode::RateLimited {
                        latency_correction, ..
                    } => WorkerMode::RateLimited {
                        requests_per_second: RequestsPerSecond::new(
                            rate_shares
                                .as_ref()
                                .expect("rate shares exist in rate-limited mode")[worker_id],
                        ),
                        latency_correction,
                    },
                };

                WorkerPlan {
                    worker_id: worker_id as u32,
                    connections: ConnectionCount::new(connections),
                    mode,
                }
            })
            .collect();

        Self {
            total_connections: requested_connections,
            workers,
        }
    }

    pub fn total_connections(&self) -> ConnectionCount {
        self.total_connections
    }

    pub fn worker_count(&self) -> WorkerCount {
        WorkerCount::new(self.workers.len() as u32)
    }

    pub fn workers(&self) -> &[WorkerPlan] {
        &self.workers
    }

    pub fn engine_configs(
        &self,
        remote_addr: SocketAddr,
        method: HttpMethod,
        duration_seconds: u32,
        warmup_seconds: u32,
        read_buffer_size: usize,
    ) -> Vec<EngineConfig> {
        self.workers
            .iter()
            .map(|worker| EngineConfig {
                worker_id: worker.worker_id,
                remote_addr,
                method,
                connections: worker.connections.get(),
                duration_seconds,
                warmup_seconds,
                mode: worker.mode.into_engine_mode(),
                read_buffer_size,
            })
            .collect()
    }
}

fn distribute_u32(total: u32, slots: u32) -> Vec<u32> {
    let base = total / slots;
    let remainder = total % slots;

    (0..slots)
        .map(|idx| base + u32::from(idx < remainder))
        .collect()
}

fn distribute_u64(total: u64, slots: u32) -> Vec<u64> {
    let slots = u64::from(slots);
    let base = total / slots;
    let remainder = total % slots;

    (0..slots)
        .map(|idx| base + u64::from(idx < remainder))
        .collect()
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(65)]
    fn proof_distribute_u32_sum() {
        let total: u32 = kani::any();
        let slots: u32 = kani::any();
        kani::assume(slots >= 1 && slots <= 64);

        let shares = distribute_u32(total, slots);
        let sum: u32 = shares.iter().sum();
        assert!(sum == total, "distribute_u32 sum mismatch");
    }

    #[kani::proof]
    #[kani::unwind(65)]
    fn proof_distribute_u64_sum() {
        let total: u64 = kani::any();
        let slots: u32 = kani::any();
        kani::assume(slots >= 1 && slots <= 64);

        let shares = distribute_u64(total, slots);
        let sum: u64 = shares.iter().sum();
        assert!(sum == total, "distribute_u64 sum mismatch");
    }

    #[kani::proof]
    #[kani::unwind(65)]
    fn proof_distribute_u32_balanced() {
        let total: u32 = kani::any();
        let slots: u32 = kani::any();
        kani::assume(slots >= 1 && slots <= 64);

        let shares = distribute_u32(total, slots);
        let min = *shares.iter().min().unwrap();
        let max = *shares.iter().max().unwrap();
        assert!(max - min <= 1, "distribute_u32 shares differ by more than 1");
    }

    #[kani::proof]
    #[kani::unwind(65)]
    fn proof_distribute_u64_balanced() {
        let total: u64 = kani::any();
        let slots: u32 = kani::any();
        kani::assume(slots >= 1 && slots <= 64);

        let shares = distribute_u64(total, slots);
        let min = *shares.iter().min().unwrap();
        let max = *shares.iter().max().unwrap();
        assert!(max - min <= 1, "distribute_u64 shares differ by more than 1");
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn proof_loadplan_connection_preservation() {
        let workers_val: u32 = kani::any();
        kani::assume(workers_val >= 1 && workers_val <= 8);
        let conns_val: u32 = kani::any();
        kani::assume(conns_val >= 1 && conns_val <= 64);

        let plan = LoadPlan::build(
            WorkerCount::new(workers_val),
            ConnectionCount::new(conns_val),
            LoadPlanMode::MaxThroughput,
        );

        let total: u32 = plan
            .workers()
            .iter()
            .map(|w| w.connections.get())
            .sum();
        assert!(
            total == conns_val,
            "LoadPlan must preserve total connections"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_workers_to_total_connections() {
        let plan = LoadPlan::build(
            WorkerCount::new(8),
            ConnectionCount::new(2),
            LoadPlanMode::MaxThroughput,
        );

        assert_eq!(plan.worker_count().get(), 2);
        assert_eq!(plan.total_connections().get(), 2);
        assert_eq!(
            plan.workers()
                .iter()
                .map(|worker| worker.connections.get())
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
    }

    #[test]
    fn caps_rate_limited_workers_to_total_rps() {
        let plan = LoadPlan::build(
            WorkerCount::new(4),
            ConnectionCount::new(8),
            LoadPlanMode::RateLimited {
                total_requests_per_second: RequestsPerSecond::new(2),
                latency_correction: true,
            },
        );

        assert_eq!(plan.worker_count().get(), 2);
        assert_eq!(
            plan.workers()
                .iter()
                .map(|worker| match worker.mode {
                    WorkerMode::RateLimited {
                        requests_per_second,
                        ..
                    } => requests_per_second.get(),
                    WorkerMode::MaxThroughput => 0,
                })
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
    }

    #[test]
    fn preserves_total_connections_across_workers() {
        let plan = LoadPlan::build(
            WorkerCount::new(3),
            ConnectionCount::new(10),
            LoadPlanMode::MaxThroughput,
        );

        let shares: Vec<u32> = plan
            .workers()
            .iter()
            .map(|worker| worker.connections.get())
            .collect();

        assert_eq!(shares.iter().sum::<u32>(), 10);
        assert_eq!(shares, vec![4, 3, 3]);
    }

    #[test]
    fn preserves_total_rate_across_workers() {
        let plan = LoadPlan::build(
            WorkerCount::new(4),
            ConnectionCount::new(8),
            LoadPlanMode::RateLimited {
                total_requests_per_second: RequestsPerSecond::new(100),
                latency_correction: true,
            },
        );

        let total_rate: u64 = plan
            .workers()
            .iter()
            .map(|worker| match worker.mode {
                WorkerMode::RateLimited {
                    requests_per_second,
                    ..
                } => requests_per_second.get(),
                WorkerMode::MaxThroughput => 0,
            })
            .sum();

        assert_eq!(total_rate, 100);
    }

    #[test]
    fn engine_configs_use_worker_local_rates() {
        let plan = LoadPlan::build(
            WorkerCount::new(3),
            ConnectionCount::new(6),
            LoadPlanMode::RateLimited {
                total_requests_per_second: RequestsPerSecond::new(10),
                latency_correction: true,
            },
        );

        let configs = plan.engine_configs(
            "127.0.0.1:8080".parse().unwrap(),
            HttpMethod::GET,
            10,
            0,
            8192,
        );

        let total_rate: u64 = configs
            .iter()
            .map(|config| match config.mode {
                EngineMode::RateLimited {
                    requests_per_second,
                    ..
                } => requests_per_second,
                EngineMode::MaxThroughput => 0,
            })
            .sum();

        assert_eq!(configs.len(), 3);
        assert_eq!(
            configs.iter().map(|config| config.connections).sum::<u32>(),
            6
        );
        assert_eq!(total_rate, 10);
    }
}
