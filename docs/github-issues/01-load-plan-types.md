Title: Refactor load-test planning into typed per-worker and per-connection allocation

Problem

The current planner in `src/bench.rs` mixes global intent and worker-local
execution config in one place. That allowed the current bug where the global
rate is cloned into every worker config, and it also allows the actual socket
count to diverge from the reported connection count.

Relevant code

- `src/bench.rs:261`
- `src/bench.rs:269`
- `src/bench.rs:281`
- `src/bench.rs:291`
- `src/bench.rs:300`
- `src/loadtest/engine.rs:132`
- `src/loadtest/engine.rs:139`

Goal

Introduce an explicit, pure planning layer that turns user intent into
worker-local execution plans before `EngineConfig` is built.

Proposed design

- Add newtypes for domain-level quantities:
  - `TotalRps`
  - `WorkerRps`
  - `TotalConnections`
  - `WorkerConnections`
  - `WorkerCount`
- Introduce a pure `LoadPlan::build(...) -> Vec<WorkerPlan>` function.
- Make `EngineConfig` accept only worker-local values.
- Keep the global request rate out of `EngineConfig`.
- Make the actual total connections derive from the returned plan, not from a
  second independently computed scalar.

Acceptance criteria

- The planner returns per-worker rate and connection assignments.
- `sum(worker.rate) == total_rps`.
- `sum(worker.connections) == total_connections`.
- CLI output and preflight use the same planned totals that the workers use.
- `src/bench.rs` no longer computes worker-local distribution inline.

Out of scope

- Kani proofs
- TLA+ specs
- Event-loop changes in `src/loadtest/engine.rs`

Suggested follow-up

Use this refactor as the base for Kani proofs on planner invariants.
