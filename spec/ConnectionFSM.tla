---- MODULE ConnectionFSM ------------------------------------------------------
\* Models the per-connection state machine in the load-test engine.
\*
\* Mirrors ConnectionState in src/loadtest/engine.rs:
\*   Connecting -> Idle -> Writing -> ReadingHead ->
\*     DrainingCL | DrainingChunked -> Idle | Closed
\*
\* Invariants verify bytes_remaining consistency and state reachability.
-------------------------------------------------------------------------------

LOCAL INSTANCE Naturals

VARIABLES state, bytes_remaining, response_complete

vars == <<state, bytes_remaining, response_complete>>

States == {"Connecting", "Idle", "Writing", "ReadingHead",
           "DrainingCL", "DrainingChunked", "Closed"}

Init ==
    /\ state = "Connecting"
    /\ bytes_remaining = 0
    /\ response_complete = FALSE

\* TCP connect completes
Connected ==
    /\ state = "Connecting"
    /\ state' = "Idle"
    /\ UNCHANGED <<bytes_remaining, response_complete>>

\* Start sending a request
SendRequest ==
    /\ state = "Idle"
    /\ state' = "Writing"
    /\ response_complete' = FALSE
    /\ UNCHANGED bytes_remaining

\* Request fully written, begin reading response headers
RequestSent ==
    /\ state = "Writing"
    /\ state' = "ReadingHead"
    /\ UNCHANGED <<bytes_remaining, response_complete>>

\* Headers parsed: no body (HEAD, 204, 304, 1xx)
HeadNoBody ==
    /\ state = "ReadingHead"
    /\ state' = "Idle"
    /\ bytes_remaining' = 0
    /\ response_complete' = TRUE

\* Headers parsed: Content-Length body
HeadContentLength ==
    /\ state = "ReadingHead"
    /\ \E n \in 1..10:
        /\ bytes_remaining' = n
        /\ state' = "DrainingCL"
    /\ UNCHANGED response_complete

\* Headers parsed: chunked body
HeadChunked ==
    /\ state = "ReadingHead"
    /\ \E n \in 1..10:
        /\ bytes_remaining' = n
        /\ state' = "DrainingChunked"
    /\ UNCHANGED response_complete

\* Drain Content-Length: read some bytes
DrainCLProgress ==
    /\ state = "DrainingCL"
    /\ bytes_remaining > 0
    /\ \E consumed \in 1..bytes_remaining:
        bytes_remaining' = bytes_remaining - consumed
    /\ state' = "DrainingCL"
    /\ UNCHANGED response_complete

\* Drain Content-Length: done -> Idle (keep-alive)
DrainCLDoneIdle ==
    /\ state = "DrainingCL"
    /\ bytes_remaining = 0
    /\ state' = "Idle"
    /\ response_complete' = TRUE
    /\ UNCHANGED bytes_remaining

\* Drain Content-Length: done -> Closed (connection: close)
DrainCLDoneClosed ==
    /\ state = "DrainingCL"
    /\ bytes_remaining = 0
    /\ state' = "Closed"
    /\ response_complete' = TRUE
    /\ UNCHANGED bytes_remaining

\* Drain chunked: read some bytes
DrainChunkedProgress ==
    /\ state = "DrainingChunked"
    /\ bytes_remaining > 0
    /\ \E consumed \in 1..bytes_remaining:
        bytes_remaining' = bytes_remaining - consumed
    /\ state' = "DrainingChunked"
    /\ UNCHANGED response_complete

\* Drain chunked: done -> Idle (keep-alive)
DrainChunkedDoneIdle ==
    /\ state = "DrainingChunked"
    /\ bytes_remaining = 0
    /\ state' = "Idle"
    /\ response_complete' = TRUE
    /\ UNCHANGED bytes_remaining

\* Drain chunked: done -> Closed (connection: close)
DrainChunkedDoneClosed ==
    /\ state = "DrainingChunked"
    /\ bytes_remaining = 0
    /\ state' = "Closed"
    /\ response_complete' = TRUE
    /\ UNCHANGED bytes_remaining

\* Connection error at any non-terminal state -> Closed
ConnectionError ==
    /\ state \in {"Connecting", "Writing", "ReadingHead", "DrainingCL", "DrainingChunked"}
    /\ state' = "Closed"
    /\ bytes_remaining' = 0
    /\ response_complete' = FALSE

\* Reconnect from Closed
Reconnect ==
    /\ state = "Closed"
    /\ state' = "Connecting"
    /\ bytes_remaining' = 0
    /\ response_complete' = FALSE

Next ==
    \/ Connected
    \/ SendRequest
    \/ RequestSent
    \/ HeadNoBody
    \/ HeadContentLength
    \/ HeadChunked
    \/ DrainCLProgress
    \/ DrainCLDoneIdle
    \/ DrainCLDoneClosed
    \/ DrainChunkedProgress
    \/ DrainChunkedDoneIdle
    \/ DrainChunkedDoneClosed
    \/ ConnectionError
    \/ Reconnect

Spec == Init /\ [][Next]_vars

\* --- Invariants ---

TypeOK ==
    /\ state \in States
    /\ bytes_remaining \in Nat
    /\ bytes_remaining <= 10
    /\ response_complete \in BOOLEAN

\* Idle connections have no pending bytes to drain
InvNoReuseBeforeDrain ==
    state = "Idle" => bytes_remaining = 0

\* Draining states have non-negative remaining (always true for Nat,
\* but verifies no underflow in the model)
InvDrainNonNegative ==
    state \in {"DrainingCL", "DrainingChunked"} => bytes_remaining >= 0

================================================================================
