Title: Add Kani proof suite for response-framing and body-kind invariants

Problem

The boundary between response-head parsing and body-drain selection is another
place where small logic slips can distort load-test results. This logic is
stateful enough to deserve proof-backed invariants, but still local enough for
Kani.

Relevant code

- `src/loadtest/engine.rs:641`
- `src/loadtest/engine.rs:655`
- `src/loadtest/engine.rs:687`
- `src/loadtest/engine.rs:689`
- `src/loadtest/engine.rs:692`

Goal

Machine-check the key body-kind invariants around `None`, `Fixed`, and
`Chunked`.

Proof targets

- HEAD responses are bodyless.
- `1xx`, `204`, and `304` are bodyless regardless of framing headers.
- `Transfer-Encoding: chunked` selects chunked framing.
- Absent chunked encoding falls back to content length or zero-length rules.
- The chosen body kind is stable under irrelevant header-order variation.

Implementation notes

- Keep the proof target focused on the post-parse classification logic.
- If direct proof over `httparse` inputs is awkward, extract a smaller pure
  classifier that operates on normalized header facts.

Acceptance criteria

- Kani harnesses exist for the classifier/body-kind selection rules.
- The proof target is small and deterministic enough to be maintained.

Out of scope

- Proving `httparse`
- Proving socket read loops
