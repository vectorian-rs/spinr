# Performance Fix Plan

## Goal

Break `1,000,000 req/s` with `spinr` in a controlled benchmark, then keep that throughput reproducible.

The `1M req/s` claim only means something if the workload is tightly defined. For this plan, the primary barrier benchmark is:

- Client: `spinr load-test --max-throughput`
- Transport: plain HTTP/1.1 keep-alive only
- Target: local in-memory server returning `204 No Content` or `Content-Length: 0`
- Payload handling: status-code-only fast path
- Runtime: release build, no debug logging, no live progress output
- Warmup: connections and DNS already warm before measurement starts
- Scope: plain HTTP/1.1 over TCP only

For non-empty bodies we will support a second fast path that drains frames without buffering, but the `1M req/s` milestone should be claimed against the empty-response benchmark. Anything else mixes client cost with body-transfer cost.

## Architecture: Process-Per-Core with mio Event Loops

This is the core design decision. Everything else follows from it.

`wrk2` proves the model: one OS thread per core, each running its own event loop (`epoll`/`kqueue`) with its own set of connections, its own rate schedule, its own histogram. No shared mutable state. No task scheduler overhead. No work-stealing. The OS schedules the processes; the event loop schedules the I/O. Nothing else runs.

`spinr` adopts this model with processes instead of threads for stronger isolation and future NUMA pinning.

### Why processes, not async tasks

| Property | Process per core | Tokio multi-thread runtime |
|---|---|---|
| Scheduling | OS scheduler, one process per core, deterministic | Tokio work-stealing across thread pool, non-deterministic |
| Shared state | None, each process is fully isolated | Tasks share heap, need `Arc`/atomics for anything cross-task |
| Context switch cost | Kernel context switch only when process blocks on I/O (rare with `mio`) | Tokio polls futures, drives wakers, manages queues on every `.await` |
| Failure isolation | One process crashes, others keep running | One panic can poison shared state or crash the runtime |
| NUMA | Pin process to NUMA node, all its memory is local | Tokio moves tasks between threads, no NUMA awareness |
| Tail latency | No scheduler jitter from other tasks | Work-stealing adds p99 jitter under contention |
| Profiling | `perf`/`dtrace` per process, clean stacks | Async state machines obscure call stacks, harder to attribute |

Tokio is the right tool for servers handling heterogeneous concurrent work. For a load generator doing one thing as fast as possible, it is overhead.

### Why mio, not tokio

`mio` is a thin wrapper around `kqueue` (macOS) and `epoll` (Linux). It does one thing: wait for file descriptors to become ready. No task scheduler, no future polling, no waker system, no timer wheel, no work-stealing queue. The event loop is a single `poll()` syscall returning a batch of ready events.

For the barrier benchmark, the hot loop is:

1. `poll()` — one syscall, returns all ready connections
2. For each ready connection: write request bytes or read response bytes
3. Record latency, increment counter
4. Loop

There is no step where a task scheduler, future combinator, or waker chain adds value. Every cycle spent there is a cycle not spent on I/O.

### Why not hyper as the HTTP client

`hyper` requires an async executor to drive its connection futures. Even `hyper::client::conn::http1` expects `tokio::io::AsyncRead + AsyncWrite` and returns futures that must be polled by a runtime. Using hyper means using tokio (or a compatible runtime), which contradicts the goal.

Instead, `spinr` uses the same approach as `wrk2`: pre-build the HTTP request as raw bytes at startup, `write()` them to the socket, `read()` the response, parse with `httparse` (zero-copy, zero-alloc). No HTTP client library in the hot path.

`httparse` is the HTTP parser that hyper uses internally. It is runtime-agnostic, does zero allocation, and parses directly from the read buffer. We get hyper's parsing quality without hyper's async machinery.

### Per-process layout

```
Process N:
  ┌─────────────────────────────────────────────────────┐
  │  mio::Poll (kqueue/epoll)                           │
  │                                                     │
  │  Connections [0..M]:                                 │
  │    ├─ TcpStream (non-blocking, TCP_NODELAY)         │
  │    ├─ state: Idle | Writing | ReadingHead | Drain   │
  │    ├─ read_buf: &mut [u8] into shared slab          │
  │    ├─ write_pos: usize                              │
  │    ├─ body_remaining: usize                         │
  │    └─ scheduled_send_time: u64 (rate-limited only)  │
  │                                                     │
  │  Shared (read-only within process):                 │
  │    ├─ request_bytes: &[u8] (pre-built HTTP request) │
  │    └─ config (parsed once at startup)               │
  │                                                     │
  │  Per-process metrics:                               │
  │    ├─ HdrHistogram (latency)                        │
  │    ├─ status_counts: [u64; 600]                     │
  │    ├─ request_count: u64                            │
  │    └─ bytes_received: u64                           │
  │                                                     │
  │  Buffer slab (one allocation at startup):           │
  │    └─ Vec<u8> of size M × BUF_SIZE                  │
  └─────────────────────────────────────────────────────┘
```

Memory per process with 128 connections and 8 KB buffers: ~1 MB for buffers + ~40 KB for histogram = ~1.1 MB. With 8 processes: ~9 MB total. Negligible.

## Non-Negotiable Rules

- No heap allocation on the steady-state request path.
- No per-request URL parsing.
- No per-request method matching.
- No per-request header map construction.
- No per-request `String` cloning.
- No per-request response-body buffering.
- No per-request logging.
- No process spawn, JSON serialization, or IPC on the hot path.
- No async runtime, task scheduler, or future polling in the hot path.
- No HTTP client library (reqwest, hyper) in the hot path. Raw socket I/O + `httparse` only.
- Status-code-only by default. Body reading is opt-in via `--verify-body`.
- Exact status-code output is preserved. The hot path uses fixed `[u64; 600]` counters and converts them to the existing `HashMap<u16, u64>` schema only at shutdown.
- Every `Instant::now()`, branch, allocation, copy, and syscall must justify itself in a flamegraph or benchmark diff.

## Allocation Budget

| Phase | Allowed allocations | Not allowed |
|---|---|---|
| Startup | Process spawn, sockets, buffer slab, request bytes, histograms, status counters | Anything deferred to "first request" without a reason |
| Per connection | TCP handshake kernel buffers | Rebuilding request bytes, allocating new read buffers, any userspace heap allocation |
| Per request | Nothing. Stack-only mutation of pre-allocated state. | Heap allocs, `String` clones, `Vec` growth, `bytes()`/`collect()`, `Box::new()`, `format!()` |
| Shutdown | Metrics serialization, JSON output, report formatting, histogram merge | Backfilling missing runtime metrics because we failed to collect them cheaply |

## Current Hot-Path Waste In Spinr

| Area | Current code | Why it costs us | Planned fix |
|---|---|---|---|
| HTTP client | `reqwest` (wraps hyper, wraps `httparse`) | URL parsing, header map construction, redirect/cookie/proxy machinery, async future polling per request | Replace with raw `write()` of pre-built bytes + `httparse` for response |
| Async runtime | `tokio` multi-thread runtime in max-throughput mode | Work-stealing scheduler, waker registration, future polling overhead | Replace with `mio` event loop per process |
| Rate limiter | `worker.rs` busy-polls `limiter.check()` and sleeps in a loop | Wasted CPU, jitter, extra `Instant::now()` calls | wrk2-style schedule: `next_send = start + (completed / throughput)`, sleep via `mio::Poll` timeout |
| Concurrency model | `manager.rs` spawns N worker processes, passes config as JSON argv, collects metrics via JSON files | JSON ser/de overhead at startup/shutdown, no shared connection pool, duplicated client instances | Keep process-per-core but: pass config via pipe, collect metrics via pipe, pre-build request bytes in parent |
| Request build | Both modes rebuild the request every iteration | Repeats method match, URL handling, header iteration, body setup | Pre-build raw HTTP request bytes once: `GET /path HTTP/1.1\r\nHost: ...\r\n\r\n` |
| Request body | `body.clone()` from `Option<String>` on each request | Heap allocation for POST/PUT/PATCH | Body baked into pre-built request bytes at startup |
| Response body | `response.bytes()` / `response.bytes().await` | Allocates and copies the whole body into a `Bytes` buffer | Default: read only status code + `Content-Length` header. `--verify-body`: drain into pre-allocated buffer |
| Status counters | `HashMap<u16, u64>` updates per request | Hash function, bucket lookup, potential resize | `[u64; 600]` indexed by exact status code; convert non-zero slots to `HashMap<u16, u64>` only when reporting |
| Progress output | Rate-limited worker logs every 1000 requests | `eprintln!` → `format!` → `write()` to stderr, locks global stderr mutex | Remove entirely from hot path. Summary at shutdown only. |

## What To Copy From `wrk2`

References:

- `https://github.com/giltene/wrk2/blob/master/src/wrk.c`
- `https://github.com/giltene/wrk2/blob/master/src/wrk.h`

### Schedule-based rate limiting

`wrk2` does not poll a token bucket. Each connection knows when its next request should be sent:

```c
uint64_t next_start_time = c->thread_start + (c->complete / c->throughput);
```

If `next_start_time > now`, the connection sleeps until then (via the event loop's timer facility). If `next_start_time <= now`, it sends immediately. When falling behind, a catch-up throughput of 2x kicks in to recover without distorting latency:

```c
next_start_time = c->catch_up_start_time
    + (complete_since_catch_up_start / c->catch_up_throughput);
```

The Rust equivalent: compute `next_send_time` per connection, use it as the `mio::Poll` timeout. No governor crate, no busy-polling, no `thread::sleep`. The event loop's own timeout provides sub-millisecond precision.

### Coordinated omission correction

Standard load generators measure latency from `request_sent` to `response_received`. When the server slows down, the client naturally backs off (it is waiting for the response before sending the next request). This hides latency spikes because the generator "coordinates" with the server's slowness.

Concrete failure mode:

- Target rate: `1000 req/s`
- Request `#500` is scheduled for `t=500ms`
- The server stalls and responds at `t=1500ms`
- Request `#501` was supposed to go at `t=501ms`, but a naive client waits for `#500` to finish and only sends it at `t=1500ms`
- The socket-observed latency for `#501` might be only `10ms`, but the user-visible latency relative to the target schedule is `1009ms`

If we only record socket-observed latency, the generator hides the stall. Tail latency looks healthy when the service is actually violating the rate contract.

`wrk2` fixes this by recording latency from the **scheduled** send time, not the actual send time:

```c
uint64_t expected_latency_start = c->thread_start
    + (c->complete_at_last_batch_start / c->throughput);
uint64_t expected_latency_timing = now - expected_latency_start;
hdr_record_value(thread->latency_histogram, expected_latency_timing);
```

`spinr` should record both:

- **Corrected latency** (from scheduled time): the metric that matters for SLA evaluation. This is what `wrk2` calls the default histogram.
- **Uncorrected latency** (from actual send): useful for debugging. This is what `wrk2` calls the `u_latency_histogram`.

Both are recorded into per-process `HdrHistogram` instances and merged at shutdown.

Why both matter:

| Latency | Corrected | Uncorrected |
|---|---:|---:|
| p50 | 2.1ms | 1.8ms |
| p99 | 847.0ms | 4.2ms |
| p99.9 | 1203.0ms | 9.1ms |

The gap is the lie. If corrected and uncorrected diverge sharply, the service is stalling relative to the requested schedule and the generator would otherwise under-report the damage.

Scope rule:

- Rate-limited mode records both corrected and uncorrected latency.
- Max-throughput mode records only uncorrected latency because there is no target schedule to violate.

### Per-thread event loop with connection state machine

`wrk2` runs one `ae` event loop per thread. Each connection is a state machine driven by socket readiness:

- `socket_writeable` → check rate schedule → write request bytes → register for readable
- `socket_readable` → read into buffer → parse with `http_parser` → record latency → back to idle

The event loop never blocks except in `poll()`. There is no future, no waker, no task queue. The state machine is a simple `enum` + `match`.

### Pre-built request bytes

`wrk2` formats the HTTP request as a byte string once at startup and writes it to the socket on each iteration. No URL parsing, no header map construction, no method dispatch. Just `write(fd, request_buf, request_len)`.

For `spinr`, that means:

- Parse the target once at startup.
- Build one immutable request buffer once:

```text
GET /path?query HTTP/1.1\r\n
Host: example.com\r\n
Connection: keep-alive\r\n
User-Agent: spinr\r\n
Content-Length: 123\r\n
X-Foo: bar\r\n
\r\n
<body bytes>
```

- Store that buffer as `Box<[u8]>` or `Arc<[u8]>`.
- Reuse the same bytes for every request on every connection.
- Track only `write_pos` per connection while handling partial writes.

The payoff is straightforward:

- no per-request method match
- no per-request string formatting
- no per-request header iteration
- no per-request body cloning
- no per-request URL/path reconstruction

This works as long as the request is static for the duration of the run, which is exactly what the barrier benchmark wants.

### What wrk2 does that we should NOT copy

- **Lua scripting in the hot path.** `wrk2` calls Lua on every response for custom processing. We do not need this.
- **Response body buffering.** `wrk2` buffers response bodies into a `buffer` struct for Lua access. We should not buffer bodies at all by default.
- **`http_parser` (Node.js legacy).** We use `httparse` instead, which is the Rust standard, zero-copy, zero-alloc, and faster.

## What To Copy From `oha`

References:

- `https://github.com/hatoo/oha/blob/master/src/request_generator.rs`
- `https://github.com/hatoo/oha/blob/master/src/client.rs`
- `https://github.com/hatoo/oha#performance`
- `https://github.com/hatoo/oha#profile-guided-optimization-pgo`

Useful ideas (adapted to our process+mio model):

- **Direct HTTP transport.** `oha` uses hyper's connection primitives directly instead of reqwest. We go further: raw sockets + `httparse`, no async HTTP library at all.
- **Cheap body reuse.** `oha` stores static bodies as `bytes::Bytes`. We go further: body is baked into the pre-built request byte string.
- **Connection reuse as first-class.** `oha` keeps `SendRequest` handles alive. We keep `TcpStream` handles alive in our connection state machine.
- **Performance modes.** `oha` uses a faster path when `--no-tui` is set and rate limiting is off. We adopt this: `--max-throughput` is the fast path, rate-limited mode is the instrumented path.
- **PGO.** `oha` documents a PGO workflow. We should do the same in Phase 4.

What we do NOT copy from `oha`:

- **Tokio async runtime.** `oha` uses tokio's multi-thread runtime. We use `mio` directly.
- **Hyper client.** `oha` uses hyper's connection API. We use raw sockets.
- **kanal channels for results.** `oha` sends per-request results through a channel. We accumulate per-process metrics locally and merge at shutdown.

## Body Policy

Default behavior (no flags): **status-code only**.

1. Parse response status line with `httparse`. Record status code.
2. Parse headers only far enough to find `Content-Length` (or detect chunked encoding).
3. If `Content-Length: 0` or status is `204`/`304`: done. Connection is immediately ready.
4. If `Content-Length > 0`: drain body bytes by reading into the same pre-allocated read buffer and discarding. This is required to reuse the HTTP/1.1 connection. No new allocation.
5. If chunked: parse chunk sizes, drain chunk data, same buffer.

With `--verify-body`:

1. Full response body is read into a pre-allocated body buffer (sized at startup based on expected response size or a default like 64 KB).
2. Body contents are available for verification (hash, exact match, size check).
3. If body exceeds buffer: error, not a silent reallocation.

With `--no-drain` (future, for the barrier benchmark only):

1. Do not read the response body at all. Rely on `Connection: close` or very small responses where TCP buffers absorb the data.
2. This is unsound for general use but valid for the `204 No Content` barrier benchmark where there is no body.

The default path (`status-code + drain`) is the production-safe fast path. `--verify-body` is for correctness testing. `--no-drain` is for benchmarking the absolute floor.

## Hot Loop Specification

This is the complete per-request work in the hot path. Nothing else is allowed.

### Max-throughput mode

```
IDLE → connection is ready, send immediately:
  write_pos = 0
  state = WRITING

WRITING:
  n = stream.write(&request_bytes[write_pos..])
  write_pos += n
  if write_pos == request_bytes.len():
    request_start = Instant::now()
    read_pos = 0
    state = READING_HEAD

READING_HEAD:
  n = stream.read(&read_buf[read_pos..])
  read_pos += n
  match httparse::Response::parse(&read_buf[..read_pos]):
    Complete(head_len):
      status_counts[code as usize] += 1
      content_length = find_content_length(headers)
      body_in_buf = read_pos - head_len
      if content_length == 0 or body_in_buf >= content_length:
        latency_us = request_start.elapsed().as_micros()
        histogram.record(latency_us)
        request_count += 1
        state = IDLE
      else:
        body_remaining = content_length - body_in_buf
        state = DRAINING
    Partial:
      // need more data, stay in READING_HEAD

DRAINING:
  n = stream.read(&read_buf[..])  // reuse full buffer, contents discarded
  body_remaining -= n
  if body_remaining == 0:
    latency_us = request_start.elapsed().as_micros()
    histogram.record(latency_us)
    request_count += 1
    state = IDLE
```

Per-request cost: two syscalls (`write` + `read`, possibly one `read` for drain), one `httparse::Response::parse` (stack-only), one `Instant::now`, one histogram record, one array index increment. Zero heap allocations.

### Rate-limited mode

Same state machine, but `IDLE` checks the rate schedule:

```
IDLE:
  next_send = process_start + (requests_completed / throughput_per_process)
  if now >= next_send:
    scheduled_time = next_send  // for coordinated omission
    state = WRITING
  else:
    // mio::Poll timeout = next_send - now
    // event loop sleeps precisely until next send time

// Latency recording uses scheduled_time, not request_start:
corrected_latency = now - scheduled_time
uncorrected_latency = now - request_start
histogram_corrected.record(corrected_latency)
histogram_uncorrected.record(uncorrected_latency)
```

The `mio::Poll` timeout replaces busy-polling entirely. The OS wakes the process when either a socket is ready or the timeout fires, whichever comes first.

Output rule:

- In rate-limited mode, report corrected latency as the primary latency column and expose uncorrected latency alongside it for debugging.
- In max-throughput mode, report only uncorrected latency.

## Feedback Loop

Every optimization round uses the same loop:

1. Lock the benchmark contract.
2. Capture a baseline before changing code.
3. Change one performance property at a time.
4. Re-run the same benchmarks.
5. Diff throughput, CPU, allocations, and flamegraphs.
6. Keep the change only if the win is measurable or it unlocks the next stage.

If a change makes the code more complex without moving `req/s`, allocation count, or CPU profile, revert the idea.

## Need-To-Have Measurements

Every serious run should capture:

- Achieved `req/s`
- User CPU%
- System CPU%
- Context switches / wakeups
- Allocations per request (target: zero in steady state)
- Bytes allocated per request (target: zero in steady state)
- p50, p95, p99, p99.9 latency (both corrected and uncorrected)
- Error rate
- Open connections
- Body bytes read per request
- Syscalls per request (`write` + `read` count, verify with `strace`/`dtruss`)
- CPU instructions / branch misses / cache misses on Linux perf hosts

We need at least three benchmark scenarios:

| Scenario | Purpose | Success condition |
|---|---|---|
| `local-empty-http1` | Primary `1M req/s` barrier run | Throughput climbs cleanly with zero hot-path allocations |
| `local-empty-http1-high-conn` | Validate scaling as connection count rises | Find the stable operating point for process count and connections |
| `local-small-fixed-body` | Ensure body drain is still cheap | No response buffering; connection reuse preserved; throughput within 10% of empty |
| `remote-real-service` | Confirm local wins carry to realistic latency | Client stays below service-side saturation cost |

## Need-To-Have Flamecharts

For each milestone we need these artifacts:

1. `spinr` Time Profiler flamechart on macOS.
2. `spinr` Allocations trace on macOS.
3. `spinr` flamegraph on a Linux perf host.
4. Target server flamechart for the same run.
5. One baseline vs one post-change diff screenshot for the hottest function group.

The flamegraphs should answer these questions:

- Is time dominated by `write()`/`read()` syscalls (good) or by userspace scaffolding (bad)?
- Are we allocating on any request? (Must be zero in steady state.)
- Is `httparse::Response::parse` visible? (Acceptable if small. If dominant, investigate buffer sizes.)
- Are `Instant::now()` calls visible? (One per request is acceptable. More is not.)
- Is the OS scheduler visible? (If yes, check core pinning and process count.)
- Is the server already the bottleneck, making client-side work invisible?

## Benchmark And Profiling Commands

### Baseline throughput

```sh
cargo build --release

# spinr (current)
target/release/spinr load-test http://127.0.0.1:3001/empty204 \
  --max-throughput -c 1024 -t 8 -d 15

# oha (reference)
oha --no-tui -z 15s -c 1024 http://127.0.0.1:3001/empty204

# wrk2 (reference, max throughput)
wrk -t8 -c1024 -d15s -R2000000 http://127.0.0.1:3001/empty204
```

### macOS Time Profiler

```sh
xcrun xctrace record \
  --template 'Time Profiler' \
  --output artifacts/time-profiler-empty.trace \
  --time-limit 20s \
  --launch -- \
  target/release/spinr load-test http://127.0.0.1:3001/empty204 \
    --max-throughput -c 1024 -t 8 -d 15
```

### macOS Allocations

```sh
xcrun xctrace record \
  --template 'Allocations' \
  --output artifacts/allocations-empty.trace \
  --time-limit 20s \
  --launch -- \
  target/release/spinr load-test http://127.0.0.1:3001/empty204 \
    --max-throughput -c 1024 -t 8 -d 15
```

### Linux perf host

```sh
perf stat -d target/release/spinr load-test http://127.0.0.1:3001/empty204 \
  --max-throughput -c 1024 -t 8 -d 15

cargo flamegraph --bin spinr -- \
  load-test http://127.0.0.1:3001/empty204 --max-throughput -c 1024 -t 8 -d 15
```

### Syscall tracing (verify per-request syscall count)

```sh
# macOS
sudo dtruss -c target/release/spinr load-test http://127.0.0.1:3001/empty204 \
  --max-throughput -c 128 -t 1 -d 5

# Linux
strace -c target/release/spinr load-test http://127.0.0.1:3001/empty204 \
  --max-throughput -c 128 -t 1 -d 5
```

## Linux Startup Checks

On Linux, the benchmark should verify a small set of local kernel settings and process limits at startup.

Rules:

- These checks run only on Linux.
- On macOS and other platforms, skip them silently.
- Hard-fail only when the requested benchmark cannot run correctly.
- Warn for everything else.
- Print one concise startup block, not per-worker spam.

### Checks We Should Enforce

| Check | How to read | Target / rule | Why it matters | Action |
|---|---|---|---|---|
| `RLIMIT_NOFILE` soft limit | `getrlimit(RLIMIT_NOFILE)` | Must be greater than the per-process socket count plus margin. Start with `connections_per_process + 128`. | Each live connection consumes an FD. Pipes and internal bookkeeping consume a few more. | Hard fail if the requested benchmark exceeds the soft limit. |
| `RLIMIT_NOFILE` hard limit | `getrlimit(RLIMIT_NOFILE)` | Prefer hard limit high enough to raise soft limit automatically when possible. | Avoids mysterious failures when the user asked for many connections but the shell soft limit is low. | Warn if soft is low but hard is high; optionally raise soft to hard. |
| `kernel.io_uring_disabled` when `--uring` is requested | `/proc/sys/kernel/io_uring_disabled` | Must allow `io_uring`. | If the kernel disables `io_uring`, the optional backend cannot start. | Warn and fall back to `mio`. |

### Checks We Should Warn About

| Check | How to read | Recommended value | Why it matters | Action |
|---|---|---|---|---|
| `net.ipv4.ip_local_port_range` | `/proc/sys/net/ipv4/ip_local_port_range` | Wide enough that `range_width >> total_connections` | Our design relies on long-lived keep-alive sockets, so this is usually fine. But a narrow range makes reconnect storms painful if connections churn. | Warn only when the configured connection count gets close to the available range. |
| `net.ipv4.tcp_tw_reuse` | `/proc/sys/net/ipv4/tcp_tw_reuse` | `1` is helpful for reconnect-heavy experiments | Not important for the steady-state keep-alive design, but useful if the run is forcing reconnects. | Warn only; do not block startup. |

### What We Should Not Treat As Startup Blockers

- `net.core.somaxconn`
- `net.ipv4.tcp_max_syn_backlog`
- IRQ affinity
- NUMA placement

Those matter for server tuning or advanced Linux benchmarking, but they are not local hard requirements for the phase-1 client engine. They belong in advanced tuning docs, not in the startup gate.

### Startup Output Contract

At startup, print a single block like:

```text
Linux startup checks:
  nofile soft/hard: 65535 / 1048576   OK
  required nofile: 1152               OK
  ip_local_port_range: 32768 60999    WARN (tight for reconnect-heavy runs)
  io_uring: unavailable               WARN (falling back to mio)
```

Behavior:

- If a hard requirement fails, exit before spawning workers.
- If a warning-only check fails, continue.
- If `--uring` is requested and unavailable, continue on `mio` unless we later add a strict mode.

## Strategy To Break The Barrier

### Phase 0: Build A Trustworthy Harness

Before touching the client:

- Add or adopt a dedicated local target server (axum or hyper, minimal, in-tree under `bench/`).
- Expose `GET /empty204` → `204 No Content` (zero-byte body, `Content-Length: 0`).
- Expose `GET /fixed64` → `200 OK` with fixed `Content-Length: 64`.
- Expose `POST /echo1k` → `200 OK` with fixed in-memory body.
- Pin the benchmark contract in docs and scripts.
- Add Linux startup verification for local limits and sysctls.
- Compare `spinr` vs `oha` vs `wrk2` on the same endpoint, same concurrency, same machine.
- Save the baseline flamegraphs as the reference set.

Without this harness we cannot tell whether we improved the generator or just changed the server behavior.

### Phase 1: New Engine — mio + httparse + Process-Per-Core

This is the main rewrite. Replace `max_throughput.rs`, `worker.rs`, and `manager.rs` with a new engine.

**New file structure:**

```
src/loadtest/
  mod.rs              — public entry point
  types.rs            — configs, metrics (keep, mostly unchanged)
  engine.rs           — backend-agnostic event loop, connection state machine, httparse response parsing
  request.rs          — pre-build HTTP request bytes from config
  rate_schedule.rs    — wrk2-style rate calculation, coordinated omission
  backend_mio.rs      — default `mio` backend
  backend_uring.rs    — optional Linux `io_uring` backend
```

**Delete:**

- `src/loadtest/max_throughput.rs` — replaced by `engine.rs`
- `src/loadtest/worker.rs` — replaced by `engine.rs`
- `src/loadtest/manager.rs` — replaced by process spawn logic in `engine.rs` or `main.rs`

**Remove from `Cargo.toml`:**

- `reqwest` — replaced by raw sockets + `httparse`
- `governor` — replaced by schedule-based rate calculation

**Add to `Cargo.toml`:**

- `mio = { version = "1", features = ["os-poll", "net"] }` — event loop
- `httparse = "1"` — zero-copy HTTP response parsing
- `socket2 = "0.5"` — socket configuration (`TCP_NODELAY`, `SO_REUSEADDR`, buffer sizes)
- `core_affinity = "0.8"` — pin processes to cores (optional, Phase 4)
- `io-uring = { version = "0.7", optional = true }` — optional Linux-only experiment behind `--uring`

**Engine contract:**

1. Parent process parses CLI config, pre-builds request bytes, forks N children.
2. Each child receives config + request bytes via pipe (not JSON argv).
3. Each child runs `engine::run()`:
   - Selects backend: default `mio`; optional `io_uring` when `--uring` is requested and available
   - Connects M TCP sockets (non-blocking, `TCP_NODELAY`)
   - Allocates buffer slab: `vec![0u8; M * BUF_SIZE]` (one allocation)
   - Allocates one `HdrHistogram` and one `[u64; 600]` status counter
   - Runs event loop until deadline
   - Writes metrics to stdout pipe (parent reads)
4. Parent collects metrics from all children, merges, prints results.

**Connection state machine:** exactly as specified in "Hot Loop Specification" above.

**Expected outcome:**

- Zero per-request heap allocations (verify with Allocations trace).
- Per-request work reduced to: `write()` + `read()` + `httparse::parse()` + histogram record + counter increment.
- No reqwest, no hyper, no tokio on the load-test hot path.
- Flamegraph dominated by `write()`/`read()` syscalls and `httparse::parse()`.
- Exact status-code reporting preserved in CLI/JSON output.

### Phase 2: Rate Scheduling With Coordinated Omission

Add rate-limited mode to the new engine.

- Implement wrk2-style schedule: `next_send = start + (completed / throughput)`.
- Implement catch-up at 2x throughput when falling behind.
- Use `mio::Poll` timeout for precise sleep-until-next-send.
- Record both corrected and uncorrected latency into separate histograms.
- Add `--latency-correction` flag (on by default in rate-limited mode, off in max-throughput).
- Both rate-limited and max-throughput use the same event loop and state machine; the only difference is whether `IDLE` checks a rate schedule or sends immediately.

**Expected outcome:**

- Rate-limited mode achieves target RPS accurately (within 1%).
- No busy-polling, no `thread::sleep`, no governor.
- Coordinated omission correction produces correct tail latencies.
- Flamegraph in rate-limited mode shows time in `poll()` (sleeping) and `write()`/`read()` (working), nothing else.

### Phase 3: Body Policy and `--verify-body`

Implement the body policy as specified in the "Body Policy" section:

- Default: drain body into existing read buffer without allocation.
- `--verify-body`: read into pre-allocated body buffer, verify contents.
- Handle chunked transfer encoding (parse chunk sizes, drain chunk data).

Benchmark the drain path against the `local-small-fixed-body` scenario. Throughput should be within 10% of the empty-response benchmark for bodies under 1 KB.

### Phase 4: Build And Runtime Tuning

Once the algorithmic waste is gone:

- Enable `lto = "fat"` and `codegen-units = 1` in release profile.
- Use `RUSTFLAGS='-C target-cpu=native'` for local benchmark binaries.
- Add a PGO workflow similar to `oha`.
- Pin processes to cores with `core_affinity` and benchmark the difference.
- Benchmark process counts and connection counts; do not assume `num_cpus()` is optimal.
- Benchmark read buffer sizes (4 KB, 8 KB, 16 KB).
- Consider `TCP_QUICKACK` on Linux.
- Consider `SO_REUSEPORT` for connection distribution.
- Add optional Linux `io_uring` experiment behind `--uring`.
- Keep `mio` as the default portable backend on macOS and Linux.
- If `--uring` is requested on macOS or on a Linux build without `io-uring` support, print one startup warning and fall back to `mio`.
- If `--uring` is requested on Linux but the kernel/runtime setup fails, print one startup warning and fall back to `mio`.

PGO and release tuning should be the last multiplier, not the first crutch.

## Implementation Order

| Order | Work | Why now |
|---|---|---|
| 1 | Harness + baseline + flamegraphs | We need a stable truth source before any code changes |
| 2 | `engine.rs`: mio event loop + connection state machine + httparse | This is the barrier-breaking change. Everything else is incremental on top of it. |
| 3 | `request.rs`: pre-built HTTP request bytes | Required by engine. Trivial module but critical for zero per-request overhead. |
| 4 | Process orchestration: parent forks children, children report metrics via pipe | Replaces `manager.rs` JSON-file IPC. Keep it simple: serialize `WorkerMetrics` once at shutdown. |
| 5 | `rate_schedule.rs`: wrk2-style scheduling + coordinated omission | Completes rate-limited mode on the new engine. |
| 6 | Body policy: drain + `--verify-body` | Completes the response handling for non-empty bodies. |
| 7 | PGO / LTO / core pinning / `--uring` experiment / runtime tuning | Final multiplier after structural work is done. |

## Exit Criteria Per Phase

We should not merge a phase without proving:

- No new correctness regressions (existing tests pass, new engine produces valid metrics).
- Same or better latency distribution for the same workload.
- Zero allocations per request in steady state (Allocations trace).
- Higher or equal `req/s` vs previous phase.
- A flamegraph that shows the intended hotspot actually shrank.
- Syscall count per request is exactly what we expect (`write` + `read`, nothing else).

## What Will Probably Prevent `1M req/s` If We Ignore It

- Keeping any HTTP client library (reqwest, hyper) in the hot path.
- Keeping any async runtime (tokio) in the hot path.
- Keeping `HashMap`, `String`, or `Vec` growth in per-request accounting.
- Treating response-body buffering as acceptable.
- Mixing rich reporting with the fastest generator path.
- Optimizing without a stable empty-response benchmark.
- Claiming `1M req/s` on a benchmark that is really measuring body transfer or server saturation.
- Ignoring coordinated omission in rate-limited mode (produces wrong latency numbers that look good but lie).

## Recommended End State

The target architecture is:

- Process-per-core with `mio` event loops. No tokio, no async runtime on the hot path.
- Optional Linux-only `io_uring` backend via `--uring`; fallback to `mio` everywhere else.
- `httparse` for zero-copy, zero-alloc HTTP response parsing.
- Pre-built raw HTTP request bytes. One `write()` per request, no per-request construction.
- Connection state machine: `Idle` → `Writing` → `ReadingHead` → `Draining` → `Idle`.
- wrk2-style rate scheduling with coordinated omission correction in rate-limited mode.
- Open-loop (no rate limit) in max-throughput mode.
- Per-process `HdrHistogram` and fixed `[u64; 600]` exact status counters. Convert to the existing `HashMap<u16, u64>` output shape only at shutdown.
- Explicit body policy: `StatusOnly` (default) / `Drain` (non-empty responses) / `VerifyBody` (opt-in via `--verify-body`).
- No allocations, no `String` ops, no `HashMap`, no logging, no formatting on the hot path.
- IPC via pipes, not filesystem.
- Build tuning: `lto = "fat"`, `codegen-units = 1`, PGO, `target-cpu=native`.

That is the shortest credible path from the current implementation to a defensible `>1M req/s`.
