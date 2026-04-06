# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.1] - 2026-03-29

### Added
- GitHub Actions release workflow for Linux binaries
- Formal verification: Kani proofs and TLA+ specs
- Formal verification documentation

### Fixed
- Kani proof bounds reduced from 64 to 8 slots to avoid resource exhaustion
- Array size in `proof_decoder_no_false_done` Kani harness

## [0.5.0] - 2026-03-22

### Added
- Formal verification: TestPhase enum, Kani proofs, TLA+ specs

### Changed
- Dropped legacy serde aliases, use `LoadPlan::workers()` from bench output

### Fixed
- Load-test correctness and MCP connections
- Chunked parsing, MCP race condition, IPv6, contract drift
- Chunked responses, MCP HTTP tools, H2 body, notifications, trace JSON

## [0.4.0] - 2026-03-21

### Changed
- Refactored load-test planning and response accounting
- Unified process topology: replaced old manager/worker with orchestrator/engine
- Bumped version to 0.5.1

## [0.3.0] - 2026-03-18

### Added
- Linux preflight checks (nofile, port range, tcp_tw_reuse)
- `.dockerignore`, mise tasks, and CLAUDE.md

### Changed
- Replaced reqwest/tokio loadtest engine with process-per-core mio architecture

## [0.2.0] - 2026-03-17

### Added
- Architecture diagram
- README with usage docs, Docker build commands, and project structure

### Changed
- Restructured into single crate with trace, loadtest, and MCP modules
- Moved PRD to `docs/prds/`

## [0.1.0] - 2026-03-16

### Added
- Initial release
- HTTP tracing with phase-by-phase timing (DNS, TCP, TLS, TTFB, transfer)
- Rate-limited load testing with HdrHistogram percentiles
- MCP server (stdio + HTTP transport) for AI agent integration
- Four Dockerfiles for platform/profile combinations

[0.5.1]: https://github.com/vectorian-rs/spinr/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/vectorian-rs/spinr/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/vectorian-rs/spinr/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/vectorian-rs/spinr/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/vectorian-rs/spinr/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/vectorian-rs/spinr/releases/tag/v0.1.0
