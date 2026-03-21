# spinr — Claude Code Project Notes

## Build & Test

```sh
cargo build            # dev build
cargo build --release  # release build
cargo test             # run all tests
```

## Docker

Four Dockerfiles: `{prod,dev}.{arm64,x86}.Dockerfile`. Build via mise tasks:

```sh
mise run docker:prod-arm64
mise run docker:dev-arm64
mise run docker:prod-x86
mise run docker:dev-x86
mise run docker:all        # build all four
```

Equivalent raw commands use `--provenance=false` — see `.mise.toml`.

Runtime base image: `gcr.io/distroless/cc-debian13`.

## Project Structure

- Binary crate (no lib target) — tests run with `cargo test`, not `cargo test --lib`
- `src/loadtest/` — mio + httparse load test engine (process-per-core)
- `src/trace/` — HTTP request tracing with timing breakdown
- `src/mcp/` — MCP server (stdio + HTTP transport)
- `src/loadtest/preflight.rs` — Linux-only startup checks (nofile, port range, tcp_tw_reuse)
- `src/bench.rs` — Multi-scenario benchmark runner (TOML config → sequential load tests + summary)
- Performance design doc: `docs/perfomance-fix.md`

## Integration Tests

16 integration tests in `tests/` invoke the compiled binary end-to-end. Each test that needs an HTTP target spins up a `TestServer` on `127.0.0.1:0` (ephemeral port) — no external network access needed.

### Test helpers (`tests/common/mod.rs`)

- **`TestServer::start()`** — HTTP/1.1 keep-alive server, 200 OK, body "ok"
- **`TestServer::start_with_response(status, body)`** — custom status/body
- **`server.url()`** — returns `http://127.0.0.1:{port}`
- **`spinr_cmd()`** — `assert_cmd::Command::cargo_bin("spinr")`
- **`write_bench_toml(dir, content)`** — writes TOML to temp dir, returns path

### Test files

| File | Tests | Coverage |
|------|-------|----------|
| `tests/cli_basics.rs` | 2 | No-args usage error, `--help` output |
| `tests/load_test.rs` | 6 | Rate-limited JSON, max-throughput JSON, bad URL, HTTPS rejection, POST with body/headers, human-readable labels |
| `tests/bench.rs` | 5 | Single/multi scenario JSON, missing config, invalid TOML, empty scenarios |
| `tests/trace.rs` | 3 | Trace JSON fields, missing URL error, human-readable output |

### Anti-flakiness

- `127.0.0.1:0` for ephemeral ports (no conflicts)
- `-t 1` forces single worker thread
- `-d 1` / low rates keeps tests fast (~1s each)
- Asserts field presence/type, not exact values

### Running

```sh
cargo test                    # all 76 unit + 16 integration
cargo test --test load_test   # just one integration test file
```

## Key Conventions

- Error handling: `anyhow` + `thiserror`
- CLI: `argh`
- No async runtime on load test hot path — uses `mio` directly
- `out!` macro pattern for JSON mode: prints to stderr when `--json`, stdout otherwise
