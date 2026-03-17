# PRD: spinr — Unified HTTP Performance & Debugging CLI

**Version:** v1
**Date:** 2026-03-17
**Status:** Draft

---

## 1. Overview

Consolidate two existing standalone MCP servers (`mcp-httptrace` and `mcp-load-tester`) and a CLI skeleton into a single Rust binary called **spinr**. The binary exposes two subcommands — `trace` and `load-test` — each usable as a direct CLI tool or as an MCP server (via `--mcp` flag). A shared `common` library provides JSON-RPC, MCP protocol, HTTP transport, and logging infrastructure.

---

## 2. Problem Statement

**Current state:** The HTTP trace tool and load tester exist as separate binaries in a separate workspace with a shared `common` crate that is not present in this repository. Each has its own `main.rs`, its own CLI parsing, and its own MCP server setup. Users must build, install, and invoke two different binaries. The `common` crate dependency is broken (path `../../common` doesn't resolve).

**Why this fails:**

- **Broken builds:** Neither subproject compiles because the `common` crate is missing from this repo.
- **Fragmented UX:** Two binaries with two names, two install paths, two sets of CLI flags.
- **Duplicated code:** Both tools implement identical MCP server init, transport selection, JSON-RPC routing, and ISO-8601 timestamp formatting.
- **No standalone CLI mode:** Both tools are MCP-only — there is no way to run `spinr trace https://example.com` and get timing output directly in the terminal.

**Business impact:** A single, cohesive tool is easier to distribute (one `cargo install`, one crates.io package), easier to document, and more useful to both human operators and AI agents.

---

## 3. Users & Value (Personas)

### P1: Developer Debugging HTTP Performance

- **Pain:** Needs to diagnose why an API call is slow — DNS? TLS? TTFB? — but existing tools (curl -w, httpstat) require memorizing format strings or installing Python scripts.
- **Key questions:** Where is the latency? Is it DNS, connect, TLS, or server processing? How does it change across HTTP versions?
- **Success signal:** Can run `spinr trace <url>` and immediately see a phase-by-phase breakdown in the terminal.

### P2: SRE / Performance Engineer

- **Pain:** Needs to validate service SLOs under load. Existing tools (wrk, hey, ab) lack MCP integration for AI-assisted analysis, or lack precise percentile histograms.
- **Key questions:** What are the p99/p99.9 latencies at target RPS? Where does the service start degrading? How do results compare across runs?
- **Success signal:** Can run `spinr load-test --rps 500 --duration 30s <url>` and get actionable latency percentiles, or hand the same config to an AI agent via MCP.

### P3: AI Agent (via MCP)

- **Pain:** Needs programmatic access to HTTP diagnostics. Current MCP servers work but are separate binaries with separate tool schemas.
- **Key questions:** Can I invoke trace or load-test tools via a single MCP server? Is the output structured JSON?
- **Success signal:** One MCP server process exposes both tools (`trace_request`, `load_test`).

---

## 4. Goals

1. **Single binary:** Ship one binary (`spinr`) installable via `cargo install spinr` that replaces both `mcp-httptrace` and `mcp-load-tester`.
2. **Dual-mode operation:** Every subcommand works as a direct CLI tool (human-readable output) and as an MCP server (`--mcp` flag, JSON-RPC over stdio or HTTP).
3. **Zero broken deps:** Inline or restructure the `common` crate so the project compiles as a self-contained Rust workspace from a fresh clone.
4. **Preserve all existing functionality:** HTTP trace timing phases, load-test rate-limited and max-throughput modes, HdrHistogram percentiles, multi-process worker model — all must carry over.
5. **Publishable:** The resulting crate must pass `cargo publish` on crates.io as `spinr`.

---

## 5. Job Stories

- As a **developer**, I can run `spinr trace https://api.example.com`, so that I see DNS, TCP, TLS, TTFB, and transfer times in my terminal without configuring anything.
- As a **developer**, I can run `spinr trace --http-version 2 https://api.example.com`, so that I can compare HTTP/1.1 vs HTTP/2 performance.
- As an **SRE**, I can run `spinr load-test -R 1000 -d 60s -t 4 https://api.example.com/health`, so that I get p50/p90/p95/p99/p99.9 latency stats and throughput numbers.
- As an **SRE**, I can run `spinr load-test --max-throughput -d 10s https://api.example.com`, so that I measure peak capacity without rate limiting.
- As a **developer**, I can run `spinr trace https://api-a.example.com https://api-b.example.com`, so that I can compare timing breakdowns side-by-side and see which endpoint is slower and why.
- As an **AI agent**, I can connect to `spinr --mcp` over stdio, so that I can invoke `trace_request` and `load_test` tools programmatically via JSON-RPC.
- As a **developer**, I can run `spinr trace --mcp -t http -p 3000`, so that a long-running MCP server exposes the trace tool over HTTP transport.

---

## 6. Assumptions

1. The existing logic in `mcp-load-tester/src/` and `mcp-httptrace/src/` is functionally correct and tested — this effort is restructuring, not rewriting.
2. The `common` crate functionality (JSON-RPC, MCP protocol, HTTP transport, logging setup) will be inlined as a workspace crate at `crates/common/` or integrated directly into the spinr crate.
3. `argh` remains the CLI parser. No migration to `clap`.
4. The multi-process worker model for load testing (manager spawns N worker processes) remains the architecture for rate-limited mode.
5. MCP protocol compatibility targets the current JSON-RPC 2.0 / MCP spec as implemented in the existing codebase.
6. The minimum supported Rust version is whatever the `edition = "2024"` implies (Rust 1.85+).

---

## 7. Functional Requirements

### FR-1: Unified binary with subcommand routing

The `spinr` binary must accept subcommands: `trace` and `load-test`.

- Acceptance: `spinr trace <url>` invokes the trace functionality. `spinr load-test <url>` invokes the load test functionality. `spinr --help` lists both subcommands with descriptions.

### FR-2: `spinr trace` — CLI mode (default)

When invoked without `--mcp`, the trace subcommand performs a single HTTP request and prints phase timing to stdout.

- Acceptance: Running `spinr trace https://example.com` prints a human-readable table showing DNS lookup, TCP connect, TLS handshake, TTFB, content transfer, and total time in milliseconds. Exit code 0 on success, non-zero on failure.

### FR-3: `spinr trace` — CLI options

The trace subcommand accepts one or more positional URL arguments and the following options:

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `<url>...` | — | — | One or more URLs to trace (positional, required). Multiple URLs are traced sequentially and results displayed side-by-side for comparison. |
| `--method` | `-m` | `GET` | HTTP method |
| `--header` | `-H` | — | Request header (repeatable) |
| `--data` | `-d` | — | Request body |
| `--http-version` | — | `1.1` | Protocol version: `1.0`, `1.1`, `2` |
| `--timeout` | — | `30` | Timeout in seconds |
| `--json` | — | off | Output as JSON instead of table |
| `--mcp` | — | off | Start as MCP server instead of running a single trace |

- Acceptance: Each flag is parsed and applied correctly. `--json` produces machine-parseable structured output matching the existing `TraceResult` schema (array of results when multiple URLs). Invalid flags produce a clear error message.

### FR-4: `spinr load-test` — CLI mode (default)

When invoked without `--mcp`, the load-test subcommand runs a load test and prints summary statistics to stdout.

- Acceptance: Running `spinr load-test -R 100 -d 30s https://example.com` executes the test and prints a summary including total requests, success/fail counts, RPS achieved, and latency percentiles (p50, p90, p95, p99, p99.9). Exit code 0 on success.

### FR-5: `spinr load-test` — CLI options

Modeled after wrk2 conventions where applicable. wrk2 uses `-t` threads, `-c` connections, `-d` duration, `-R` rate. spinr adapts this to its multi-process worker model.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--rate` | `-R` | — | Target requests per second, constant throughput (required unless `--max-throughput`). Matches wrk2's `-R` flag. Distributed evenly across worker processes. |
| `--duration` | `-d` | — | Test duration. Accepts human-readable format: `30s`, `5m`, `1h`, or plain integer (interpreted as seconds). Matches wrk2's `-d` flag. Required. |
| `--threads` | `-t` | num_cpus | Number of worker processes. Analogous to wrk2's `-t` (threads). Each worker is a separate OS process with its own rate limiter. |
| `--connections` | `-c` | 1 per thread | Number of concurrent HTTP connections per worker. Matches wrk2's `-c` semantics. |
| `--method` | `-m` | `GET` | HTTP method |
| `--header` | `-H` | — | Request header (repeatable) |
| `--data` | `-d` | — | Request body. Note: short flag conflicts with `--duration`; use long form `--data` when both are needed. |
| `--max-throughput` | — | off | Closed-loop mode (wrk-style). Ignores `--rate`, saturates with as many requests as possible. |
| `--warmup` | — | `10s` | Warmup duration before measurement begins. wrk2 uses a 10s calibration period by default; spinr matches this. |
| `--latency` | — | off | Print detailed latency percentile distribution (matches wrk2's `--latency` flag). Without this flag, only summary stats are shown. |
| `--json` | — | off | Output as JSON |
| `--mcp` | — | off | Start as MCP server |

- Acceptance: All flags are parsed and applied. `--rate` is required when `--max-throughput` is not set. `--duration` parses `30s`, `5m`, `1h`, `90` (seconds). `--json` produces structured output matching the existing `MergedMetrics` schema. Running tests under 10 seconds prints a warning (following wrk2's guidance that short runs may not yield meaningful results).

### FR-6: MCP server mode (`--mcp`)

When `--mcp` is passed to either subcommand (or to `spinr` directly), the binary starts an MCP server exposing the relevant tools. The MCP server exposes 2 tools: `trace_request` and `load_test`. The load_test tool is a unified tool that accepts a config object and returns results (replacing the previous 3-tool start/stop/get_status pattern with a simpler request-response model for MCP).

- Acceptance: `spinr --mcp` starts an MCP server exposing both tools: `trace_request` and `load_test`. `spinr trace --mcp` exposes only `trace_request`. `spinr load-test --mcp` exposes only `load_test`. Default transport is stdio. JSON-RPC 2.0 protocol is supported.

### FR-7: MCP transport selection

The MCP server must support both stdio and HTTP transports.

- Acceptance: `spinr --mcp` uses stdio. `spinr --mcp -t http -p 3000` starts an HTTP server on port 3000. The `--host` flag controls bind address (default `127.0.0.1`).

### FR-8: `trace_request` MCP tool

Preserves the existing MCP tool schema and behavior from `mcp-httptrace`.

- Acceptance: The tool accepts `url`, `method`, `headers`, `body`, `timeout_secs`, `http_version` parameters and returns `TimingInfo`, `ResponseInfo`, `ConnectionInfo`, and request metrics as structured JSON. All timing phases (DNS, TCP, TLS, TTFB, transfer, total) are measured and reported in milliseconds.

### FR-9: `load_test` MCP tool

Unified MCP tool that runs a load test to completion and returns results. Replaces the previous 3-tool (start/stop/get_status) pattern.

- Acceptance: The tool accepts `url`, `method`, `headers`, `body`, `rate` (RPS), `duration` (seconds), `threads`, `connections`, `warmup` (seconds), `max_throughput` (boolean) parameters. It runs the test synchronously and returns a structured result containing: total/successful/failed request counts, actual RPS, latency percentiles (p50, p90, p95, p99, p99.9, p99.99), min/max/mean latency, transfer bytes, and actual duration. The tool blocks until the test completes.

### FR-10: Workspace structure compiles from clean clone

The project must be structured as a Rust workspace where `cargo build` succeeds from a fresh clone with no external path dependencies.

- Acceptance: `git clone && cargo build --release` succeeds. `cargo test` passes. No `path = "../../"` references point outside the workspace root.

### FR-11: Human-readable CLI output for trace

The trace CLI output should present timing as a visual breakdown.

- Acceptance: Output shows each phase label, its duration in ms, and a visual indicator of relative proportion (e.g., ASCII bar, color, or alignment). Total time is clearly shown. Example format:
  ```
  DNS Lookup:        12.5 ms  ██
  TCP Connect:       25.3 ms  ████
  TLS Handshake:     45.2 ms  ███████
  TTFB:             120.1 ms  ██████████████████
  Content Transfer:  15.4 ms  ██
  Total:            218.5 ms

  Status: 200 OK
  Remote: 93.184.216.34
  Protocol: HTTP/1.1, TLSv1.3
  ```

### FR-12: Human-readable CLI output for load-test

Output format follows wrk2 conventions. When `--latency` is passed, a detailed percentile distribution is included.

- Acceptance: Output shows a summary table with request counts, RPS, and latency percentiles. Example format:
  ```
  Running 30s test @ https://api.example.com
    4 threads and 4 connections
    Rate: 100 req/s (target)

    Latency     Avg       Stdev     Max       +/- Stdev
                5.23ms    2.14ms    45.70ms   87.50%

    Latency Distribution (HdrHistogram - Recorded Latency)
      50.000%    5.20ms
      90.000%   12.10ms
      95.000%   18.40ms
      99.000%   45.70ms
      99.900%  120.30ms
      99.990%  225.00ms
      99.999%  250.10ms
     100.000%  250.10ms

    3000 requests in 30.01s, 1.20MB read
      2 errors (0 timeouts, 2 non-2xx)
    Requests/sec:     99.97
    Transfer/sec:     40.00KB
  ```
  Without `--latency`, only the summary lines (avg/stdev/max, totals, req/sec, transfer/sec) are shown.

### FR-13: Batch trace with comparison output

When `spinr trace` receives multiple URLs, it traces each sequentially and displays a comparison table.

- Acceptance: `spinr trace https://api-a.example.com https://api-b.example.com` outputs a side-by-side comparison showing each phase for both URLs, with the difference (delta) highlighted. Example format:
  ```
                       api-a.example.com    api-b.example.com    delta
  DNS Lookup:                    12.5 ms              8.2 ms    -4.3 ms
  TCP Connect:                   25.3 ms             22.1 ms    -3.2 ms
  TLS Handshake:                 45.2 ms             43.8 ms    -1.4 ms
  TTFB:                         120.1 ms            250.7 ms  +130.6 ms
  Content Transfer:              15.4 ms             12.3 ms    -3.1 ms
  Total:                        218.5 ms            337.1 ms  +118.6 ms

  Status:                           200                 200
  Protocol:              HTTP/1.1 TLSv1.3    HTTP/1.1 TLSv1.3
  ```
  With `--json`, output is an array of trace result objects. Supports 2-4 URLs; more than 4 produces an error suggesting JSON output mode.

### FR-14: Hidden worker subcommand for load-test process model

The load-tester's multi-process architecture requires spawning child processes. The main binary must support a hidden `--run-worker <config_json>` flag (not shown in `--help`) that the manager process uses to spawn workers.

- Acceptance: `spinr load-test --run-worker '{"target_url":...}'` launches a worker process that reads config from the argument, executes requests at the assigned rate, writes metrics to the metrics directory, and exits. This flag is not visible in `spinr load-test --help`. The manager invokes it via `std::env::current_exe()` (matching the existing implementation in `manager.rs:63`).

---

## 8. Non-Functional Requirements

### NFR-1: Latency measurement accuracy

Trace timing phases must be measured with sub-millisecond precision.

- Acceptance: Timing uses `std::time::Instant` (monotonic clock). Load-test histograms record in microseconds. Reported values are accurate to 0.1 ms.

### NFR-2: Load-test throughput

The load tester must sustain at least 10,000 RPS on a modern laptop (M-series Mac) against a local echo server.

- Acceptance: `spinr load-test -R 10000 -d 10s http://localhost:8080/echo` completes with actual RPS within 5% of target and no worker crashes.

### NFR-3: Binary size

The release binary should be reasonable for a CLI tool.

- Acceptance: `cargo build --release` produces a binary under 15 MB (stripped).

### NFR-4: Startup time

The tool should feel instant for CLI use.

- Acceptance: `spinr trace --help` completes in under 50 ms.

### NFR-5: Error messages

All errors should be actionable.

- Acceptance: Network errors include the target URL. DNS failures name the hostname. TLS errors indicate the negotiation stage. Invalid CLI args show usage help.

### NFR-6: No unsafe code

- Acceptance: `#![forbid(unsafe_code)]` at the crate root. Dependencies may use unsafe internally, but spinr source must not.

---

## 9. Technical Constraints

### Language & Tooling

- **Language:** Rust (edition 2024)
- **Build:** Cargo workspace
- **CLI parser:** `argh` (already in use — do not migrate to clap)
- **Async runtime:** Tokio (multi-threaded)

### Key Libraries (preserve from existing code)

| Purpose | Crate | Notes |
|---------|-------|-------|
| HTTP client (load-test) | `reqwest` | Blocking mode for worker processes |
| HTTP client (trace) | `hyper` 1.x + `hyper-util` | Low-level for phase timing |
| TLS | `tokio-rustls` + `rustls` | Ring crypto provider |
| DNS | `hickory-resolver` | Async resolution with timing |
| Rate limiting | `governor` | Token bucket for RPS control |
| Histograms | `hdrhistogram` | Microsecond precision, base64 serialization |
| Serialization | `serde` + `serde_json` | All inter-process and MCP communication |
| Logging | `tracing` + `tracing-subscriber` | Structured logging |
| Error handling | `anyhow` + `thiserror` | `anyhow` for binary, `thiserror` for library errors |

### Workspace Layout

```
spinr/
├── Cargo.toml              # workspace root
├── Cargo.lock
├── crates/
│   ├── spinr/              # main binary crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs     # subcommand dispatch
│   │       ├── cli.rs      # argh arg structs
│   │       └── output.rs   # human-readable formatters
│   ├── spinr-trace/        # trace library crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── tracer.rs   # from mcp-httptrace
│   │       └── types.rs
│   ├── spinr-loadtest/     # load-test library crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manager.rs  # from mcp-load-tester
│   │       ├── worker.rs
│   │       ├── types.rs
│   │       └── bench.rs
│   └── spinr-common/       # shared MCP, JSON-RPC, transport
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── jsonrpc.rs
│           ├── mcp.rs
│           └── transport.rs
├── prds/
└── docs/
```

### Deployment

- **Distribution:** crates.io (`cargo install spinr`)
- **Platforms:** macOS (ARM + x86), Linux (x86_64, aarch64)
- **No runtime dependencies** beyond libc / system TLS roots

### Explicit Prohibitions

- No `clap` — use `argh`
- No `openssl` — use `rustls`
- No `async-std` — use `tokio`
- No GUI, TUI, or interactive prompts
- No config files for v1 — all configuration via CLI flags

---

## 10. Non-Goals

1. **HTTP server / reverse proxy functionality** — spinr is a client-side tool only.
2. **Distributed load testing** — v1 runs on a single machine. No coordinator/agent model across hosts.
3. **Persistent storage / databases** — no result history, dashboards, or time-series storage.
4. **Custom scripting / Lua / JS** — no request scripting language (unlike wrk's Lua support).
5. **WebSocket or gRPC tracing** — HTTP only in v1.
6. **Config file support** — v1 is CLI-flags only. Config files may come in v2.
7. **CI integration / GitHub Actions** — out of scope. JSON output mode enables downstream integration but spinr itself doesn't provide it.

---

## 11. Success Metrics

| Metric | Target | Persona |
|--------|--------|---------|
| Time to first useful output | `spinr trace <url>` produces timing in < 2s (excluding network) | P1 |
| CLI adoption signal | `cargo install spinr` downloads > 100 in first month | P1, P2 |
| MCP tool invocations | AI agent can complete a "diagnose slow endpoint" workflow using only spinr MCP tools | P3 |
| Latency accuracy | Trace phase timings within 1 ms of `curl -w` measurements for the same request | P1 |
| Load-test percentile accuracy | p99 within 5% of wrk for the same target at same RPS | P2 |
| Build success rate | `cargo build` succeeds on fresh clone with no external deps, on macOS + Linux | All |

---

## 12. Design Principles

1. **CLI-first, MCP-second.** The tool must be immediately useful without any MCP knowledge. `--mcp` is an opt-in mode, not the default.
2. **One binary, zero config.** No config files, no environment variables required. Every option is a flag with a sensible default.
3. **Accurate over fast.** Prefer precise measurements (HdrHistogram, monotonic clocks, per-phase timing) over approximate ones, even if it costs some throughput.
4. **Structured output is a flag, not a mode.** `--json` on any subcommand produces machine-readable output. Human-readable is the default.
5. **Library crates are the real product.** The binary is a thin CLI wrapper. All logic lives in `spinr-trace` and `spinr-loadtest` library crates so they can be reused as Rust dependencies.

---

## 13. Open Questions

1. **`common` crate migration scope:** User will copy the original `common` crate into this workspace. Need to assess how much refactoring is needed once it lands — it may reference workspace dependencies from the old repo that need updating.
2. **`--data` / `-d` short flag conflict:** Both `--duration` and `--data` want `-d`. wrk2 uses `-d` for duration. Current resolution: `-d` maps to `--duration` (wrk2 convention), `--data` has no short form. Alternatively, use `-b` (body) as the short form for request body. Needs decision.
3. **Coordinated omission correction:** wrk2's primary contribution is correcting for coordinated omission in latency measurement. The current load-tester uses `governor` for rate limiting but it's unclear if the measurement accounts for coordinated omission. Should spinr v1 address this, or defer?
4. **Connection pooling semantics:** wrk2's `-c` flag means total concurrent connections across all threads. The current implementation uses 1 connection per worker process (via `reqwest` blocking client). Should spinr expose `-c` as connections-per-thread (simpler) or total connections (wrk2 compat)?

---

## 14. Diagram

```
┌─────────────────────────────────────────────────────┐
│                    spinr binary                      │
│                                                      │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │  CLI     │  │   trace      │  │  load-test    │  │
│  │  Parser  │──│  subcommand  │  │  subcommand   │  │
│  │  (argh)  │  │              │  │               │  │
│  └──────────┘  └──────┬───────┘  └──────┬────────┘  │
│                       │                  │           │
│              ┌────────┴──┐      ┌────────┴────────┐  │
│              │ --mcp?    │      │ --mcp?          │  │
│              ├───┬───────┤      ├───┬─────────────┤  │
│              │ N │  Y    │      │ N │  Y          │  │
│              └─┬─┘  │    │      └─┬─┘  │          │  │
│                │    │    │        │    │           │  │
│         ┌──────┘    │    │  ┌─────┘    │           │  │
│         ▼           ▼    │  ▼          ▼           │  │
│   ┌──────────┐ ┌────────┐│ ┌────────┐ ┌─────────┐ │  │
│   │ Direct   │ │  MCP   ││ │Direct  │ │  MCP    │ │  │
│   │ CLI run  │ │ Server ││ │CLI run │ │ Server  │ │  │
│   │ (stdout) │ │(stdio/ ││ │(stdout)│ │(stdio/  │ │  │
│   │          │ │ http)  ││ │        │ │ http)   │ │  │
│   └────┬─────┘ └───┬────┘│ └───┬────┘ └───┬─────┘ │  │
│        │           │     │     │           │       │  │
└────────┼───────────┼─────┼─────┼───────────┼───────┘  │
         │           │     │     │           │          │
         ▼           ▼     │     ▼           ▼          │
  ┌─────────────────────┐  │  ┌──────────────────────┐  │
  │   spinr-trace       │  │  │   spinr-loadtest     │  │
  │   (library crate)   │  │  │   (library crate)    │  │
  │                     │  │  │                      │  │
  │  • DNS resolution   │  │  │  • Manager/worker    │  │
  │  • TCP connect      │  │  │  • Rate limiting     │  │
  │  • TLS handshake    │  │  │  • HdrHistogram      │  │
  │  • TTFB measurement │  │  │  • Max-throughput    │  │
  │  • HTTP/1.x, 2      │  │  │  • Metrics merge     │  │
  └─────────┬───────────┘  │  └──────────┬───────────┘  │
            │              │             │              │
            └──────┬───────┘             │              │
                   │                     │              │
                   ▼                     │              │
          ┌────────────────┐             │              │
          │ spinr-common   │◄────────────┘              │
          │                │                            │
          │ • JSON-RPC 2.0 │                            │
          │ • MCP protocol │                            │
          │ • Transport    │                            │
          │ • Logging      │                            │
          └────────────────┘                            │
```
