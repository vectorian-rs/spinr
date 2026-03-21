Title: Specify MCP load-test lifecycle and single-active-run invariant in TLA+

Problem

The MCP load-test path has enough concurrency and lifecycle state that it
deserves a small state-machine spec. This is better expressed in TLA+ than in
Rust-level bounded model checking.

Relevant code

- `src/mcp/stdio.rs:37`
- `src/mcp/stdio.rs:306`
- `src/mcp/stdio.rs:389`

Goal

Write a TLA+ spec for load-test lifecycle and check it with TLC first, then
with `tla-checker` if the spec remains within the supported subset.

Suggested model

State

- `phase in {Idle, Running, Finished}`
- `handle_present`
- `started_at_present`
- `ended_at_present`
- `metrics_present`
- `outcome`

Actions

- `Start`
- `WorkerCompletesSuccess`
- `WorkerCompletesFailure`
- `GetStatus`

Core invariants

- At most one active run
- `Running => handle_present`
- `Running => started_at_present`
- `Finished => ended_at_present`
- `Finished => outcome /= Null`
- `FinishedSuccess => metrics_present`
- `GetStatus` does not create or duplicate a run

Acceptance criteria

- A spec exists under `spec/`.
- The invariants above are checked.
- The spec is written in standard TLA+ and is not coupled exclusively to one
  checker implementation.

Out of scope

- Cancellation
- Full transport/session semantics

Notes

Treat `tla-checker` as an additional runner, not the only semantics source.
