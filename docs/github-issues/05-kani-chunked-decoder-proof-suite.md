Title: Add Kani proof suite for chunked-decoder framing and accounting

Problem

The chunked decoder is a compact state machine with subtle partial-read and
trailer semantics. That is the exact kind of code Kani can stress more
effectively than hand-written examples.

Relevant code

- `src/loadtest/engine.rs:709`
- `src/loadtest/engine.rs:741`
- `src/loadtest/engine.rs:788`
- `src/loadtest/engine.rs:791`

Precondition

Land the byte-accounting refactor first so the decoder API exposes payload bytes
and wire bytes explicitly.

Goal

Add Kani proofs for the decoder's completion and accounting invariants.

Proof targets

- Split-feed equivalence:
  feeding a valid message in pieces is equivalent to feeding it in one shot.
- No early completion:
  `Done` must not be reachable before the final trailer terminator.
- Full-consumption on completion:
  if the decoder reports complete, the consumed byte count reaches the end of
  the framed message.
- Payload accounting:
  payload bytes equal the sum of chunk sizes and exclude chunk framing.
- No false completion when payload bytes contain `0\\r\\n\\r\\n`.

Implementation notes

- Start with bounded message generators or structured symbolic inputs rather
  than arbitrary byte streams.
- Keep the first proofs local to the decoder; do not try to prove the full
  engine event loop.

Acceptance criteria

- Kani harnesses cover completion, partial feeds, trailers, and accounting.
- Existing hand-written tests are complemented by machine-checked invariants.
- The current trailer-completion bug pattern is ruled out by proof.
