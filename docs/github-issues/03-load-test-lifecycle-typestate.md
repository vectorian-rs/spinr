Title: Replace load-test status bools with an explicit lifecycle enum

Problem

The current load-test status model is a bool plus several optional fields. That
permits contradictory states and makes lifecycle reasoning harder than it needs
to be, especially for MCP/HTTP mode.

Relevant code

- `src/loadtest/types.rs:186`
- `src/mcp/stdio.rs:37`
- `src/mcp/stdio.rs:306`
- `src/mcp/stdio.rs:389`

Goal

Represent load-test lifecycle as an explicit sum type rather than a loose bag
of booleans and optional timestamps.

Proposed design

- Replace `TestStatus` with something like:
  - `Idle`
  - `Running { run_id, started_at, config }`
  - `Finished { run_id, started_at, ended_at, outcome, metrics }`
- Keep the thread handle as internal server state, not public status shape.
- Make `get_status` serialize directly from the lifecycle enum.
- Ensure MCP lifecycle transitions are explicit and one-directional.

Acceptance criteria

- No `running: bool` plus `completed: Option<bool>` combination remains.
- Public status output is derived from a single lifecycle enum.
- Starting and completing a run transitions between explicit states.
- The model is simple enough to spec directly in TLA+ later.

Out of scope

- Adding cancellation
- Reworking the transport layer

Suggested follow-up

This is the right precursor for a `LoadTestLifecycle.tla` spec.
