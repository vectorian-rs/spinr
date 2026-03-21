Title: Add Kani proof suite for load-plan and rate-distribution invariants

Problem

The current rate-planning bug exists because the code has no machine-checked
invariants for planner math. This is a strong fit for Kani once the planner is
extracted into pure functions.

Precondition

Land the typed planner refactor first.

Goal

Add Kani proof harnesses for the core arithmetic and allocation invariants in
the load planner.

Proof targets

- `LoadPlan::build` preserves total rate:
  - `sum(worker.rate) == total_rps`
- `LoadPlan::build` preserves total connections:
  - `sum(worker.connections) == total_connections`
- No worker receives an impossible allocation.
- Shares are balanced within expected bounds:
  - rate shares differ by at most 1
  - connection shares differ by at most 1 when relevant
- `distribute_rate(total_rps, slots)` preserves:
  - length
  - sum
  - near-even distribution

Implementation notes

- Prefer proof-friendly pure helpers over proving imperative `bench.rs` logic in
  place.
- Keep inputs bounded in harnesses.
- Keep proof code under `#[cfg(kani)]`.

Acceptance criteria

- A `cargo kani` run exercises the planner proofs successfully.
- The proof suite covers both total-preservation and balancing invariants.
- The proof suite would fail for the current bug pattern of cloning global rate
  into every worker.

Out of scope

- Network I/O
- Process orchestration
- `mio` event-loop behavior
