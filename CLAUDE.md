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
- Performance design doc: `docs/perfomance-fix.md`

## Key Conventions

- Error handling: `anyhow` + `thiserror`
- CLI: `argh`
- No async runtime on load test hot path — uses `mio` directly
- `out!` macro pattern for JSON mode: prints to stderr when `--json`, stdout otherwise
