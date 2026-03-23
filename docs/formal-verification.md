# Formal Verification

Spinr uses two formal verification tools to check correctness properties
of the load-test engine: **Kani** (bounded model checking for Rust) and
**TLA+** (state machine specification and model checking).

## Why formal verification?

The load-test engine has subtle invariants that are hard to cover with
example-based tests alone:

- Work distribution must be exact — losing or duplicating a connection
  across workers silently skews throughput numbers.
- The chunked transfer-encoding decoder is a streaming state machine fed
  arbitrary byte slices. Off-by-one errors in framing can miscount bytes
  or cause hangs.
- The connection lifecycle has multiple states and error edges. Reusing a
  connection before its response body is fully drained corrupts the next
  response.

## Kani proofs

Kani exhaustively checks all inputs within declared bounds. Run with
`cargo kani --harness <name>`.

### Load plan distribution (`src/loadtest/plan.rs`)

These proofs verify `distribute_u32` / `distribute_u64`, which split a
total evenly across worker slots (remainder distributed one-per-slot).

| Proof | Property | Bounds |
|-------|----------|--------|
| `proof_distribute_u32_sum` | Sum of shares == original total (u32) | slots 1..8 |
| `proof_distribute_u64_sum` | Sum of shares == original total (u64) | slots 1..8 |
| `proof_distribute_u32_balanced` | Max share - min share <= 1 (u32) | slots 1..8 |
| `proof_distribute_u64_balanced` | Max share - min share <= 1 (u64) | slots 1..8 |
| `proof_loadplan_connection_preservation` | `LoadPlan::build` preserves total connections across workers | workers 1..8, connections 1..64, `MaxThroughput` mode only |

Unwind depth: 9 for all proofs.

Note: `proof_loadplan_connection_preservation` only covers
`LoadPlanMode::MaxThroughput`. Rate-limited mode is not verified.

### Chunked decoder (`src/loadtest/engine.rs`)

These proofs target `ChunkedDecoder::feed()`, the streaming parser for
`Transfer-Encoding: chunked` responses.

| Proof | Property | Bounds |
|-------|----------|--------|
| `proof_decoder_split_feed` | Two-way split feed produces same completion flag and same `payload_bytes` as one-shot feed (does not compare `wire_bytes` or internal state) | arbitrary input up to 32 bytes |
| `proof_decoder_no_early_done` | Messages shorter than 5 bytes (minimum valid terminator `0\r\n\r\n`) never report done | arbitrary input 1..4 bytes |
| `proof_decoder_full_consumption` | For a well-formed single-chunk body, `wire_bytes` == total input length on completion | single chunk, payload 1..15 bytes |
| `proof_decoder_payload_accounting` | `payload_bytes` <= `wire_bytes` for any input | arbitrary input up to 32 bytes |
| `proof_decoder_no_false_done` | One specific input with embedded `0\r\n\r\n` inside payload does not trigger false completion | fixed 15-byte input: `5\r\n0\r\n\r\n\r\n0\r\n\r\n` |

Unwind depth: 33 (except `no_early_done` at 6 and `no_false_done` with
no unwind annotation).

Note: `proof_decoder_full_consumption` only constructs single-chunk
bodies, not multi-chunk. `proof_decoder_no_false_done` is a single
concrete test case, not an exhaustive check.

### Response framing classifier (`src/loadtest/engine.rs`)

These proofs verify `classify_body_kind()`, which determines how to read
a response body based on method, status code, and headers.

| Proof | Property |
|-------|----------|
| `proof_head_always_bodyless` | HEAD responses always produce `BodyKind::None` regardless of headers |
| `proof_1xx_204_304_bodyless` | Status codes 1xx, 204, 304 always produce `BodyKind::None` |
| `proof_chunked_requires_te` | `BodyKind::Chunked` is only selected when `Transfer-Encoding: chunked` is present |
| `proof_content_length_nonneg` | `BodyKind::Fixed(n)` matches the `Content-Length` header value |

## TLA+ specifications

TLA+ models verify state machine invariants across all reachable states.
Specs live in `spec/` and are checked by
[tla-checker](https://github.com/afonsonf/tla-checker) (the `tla` CLI)
with `--allow-deadlock` (the specs model finite lifecycles, not
infinite liveness). Tests are in `tests/formal.rs`.

### Load-test lifecycle (`spec/LoadTestLifecycle.tla`)

Models an idealized MCP load-test lifecycle: Idle → Running → Finished
→ Idle.

| Invariant | Property |
|-----------|----------|
| `InvSingleRun` | Running phase implies `handle_present = TRUE` |
| `InvNoMetricsBeforeFinish` | Metrics only exist in the Finished phase |

**Model vs. implementation gaps:** The model assumes `handle_present`
becomes TRUE atomically with the Idle → Running transition (`StartTest`
action). The implementation (`src/mcp/stdio.rs:340-373`) sets
`status = Running` before storing the join handle, so there is a brief
window where the invariant does not hold. The model also requires an
explicit Finished → Idle reset before the next test; the implementation
skips this — it checks `h.is_finished()` and overwrites the stale handle
directly (`src/mcp/stdio.rs:318-324`). The spec captures the intended
design; the code has not been updated to match.

Related code: `TestPhase` enum in `src/loadtest/types.rs:203`.

### Connection FSM (`spec/ConnectionFSM.tla`)

Models the per-connection state machine with 7 states: Connecting, Idle,
Writing, ReadingHead, DrainingCL, DrainingChunked, Closed.

| Invariant | Property |
|-----------|----------|
| `InvNoReuseBeforeDrain` | Connection returns to Idle only after response body is fully consumed |
| `InvDrainNonNegative` | Byte counter stays non-negative during body draining |

Related code: `ConnectionState` enum and state transitions in
`src/loadtest/engine.rs`.
