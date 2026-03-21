Title: Separate payload bytes from wire bytes in response accounting

Problem

`total_bytes` is documented as payload bytes, but the chunked path currently
records parser-consumed bytes from the wire. The core problem is the API shape:
the code passes a plain integer through `record_response`, so payload and wire
accounting are easy to conflate.

Relevant code

- `src/loadtest/types.rs:76`
- `src/loadtest/engine.rs:47`
- `src/loadtest/engine.rs:56`
- `src/loadtest/engine.rs:464`
- `src/loadtest/engine.rs:469`
- `src/loadtest/engine.rs:476`
- `src/loadtest/engine.rs:598`
- `src/loadtest/engine.rs:606`
- `src/loadtest/engine.rs:741`

Goal

Make response accounting explicit enough that payload and wire-byte confusion is
hard to express in code.

Proposed design

- Replace the raw `body_len: u64` parameter in `record_response(...)` with a
  structured type, for example:
  - `ResponseAccounting { payload_bytes, wire_bytes }`
- Rename public metrics fields if needed:
  - keep `payload_bytes` as the user-facing metric
  - optionally add `wire_bytes` if that metric is useful
- Refactor the chunked path so the decoder reports both:
  - bytes consumed from the wire
  - bytes decoded into payload
- Update merged metrics and output code to use the renamed fields consistently.

Acceptance criteria

- No hot-path response accounting API takes an untyped raw byte count.
- Payload-byte metrics exclude chunk framing and trailers.
- Any wire-byte metric is explicitly named as such.
- Existing outputs and JSON shapes are updated or versioned intentionally.

Out of scope

- Full decoder proofs
- Connection state-machine changes outside accounting

Suggested follow-up

This should land before Kani work on chunked decoding.
