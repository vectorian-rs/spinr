Title: Specify single-connection request and response lifecycle in TLA+

Problem

The engine's per-connection state machine is subtle enough that a small TLA+
model would add real value, especially around keep-alive reuse and response
completion boundaries.

Relevant code

- `src/loadtest/engine.rs:514`
- `src/loadtest/engine.rs:577`
- `src/loadtest/engine.rs:836`
- `src/loadtest/engine.rs:997`

Goal

Write a TLA+ spec for one persistent connection and verify that connection reuse
never occurs before the previous response is fully consumed.

Suggested model

States

- `Connecting`
- `Idle`
- `Writing`
- `ReadingHead`
- `DrainingContentLength`
- `DrainingChunked`
- `Closed`

Events

- `StartRequest`
- `ReadHead`
- `ReadBody`
- `ReadTrailer`
- `FinishResponse`
- `Reconnect`
- `Fail`

Core invariants

- A connection cannot be `Idle` while response bytes remain unconsumed.
- A new request cannot start while a previous response is still in progress.
- Chunked responses cannot finish before trailer termination.
- `Closed` and `Idle` are mutually exclusive terminal conditions for one request.

Acceptance criteria

- A spec exists under `spec/`.
- The spec checks the no-reuse-before-completion invariant.
- The spec is simple enough to run in ordinary developer workflows.

Out of scope

- Byte-accurate HTTP parsing
- Proving the Rust decoder implementation directly

Notes

This spec should stay abstract. Use Kani for the byte-level decoder and TLA+
for lifecycle ordering.
