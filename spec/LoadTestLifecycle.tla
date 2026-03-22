---- MODULE LoadTestLifecycle --------------------------------------------------
\* Models the MCP load-test lifecycle state machine.
\*
\* States: Idle -> Running -> Finished -> Idle (cycle)
\*
\* Invariants verify that handle_present and metrics_present are consistent
\* with the current phase, matching the Rust TestPhase enum.
-------------------------------------------------------------------------------

VARIABLES phase, handle_present, metrics_present

vars == <<phase, handle_present, metrics_present>>

Phases == {"Idle", "Running", "Finished"}

Init ==
    /\ phase = "Idle"
    /\ handle_present = FALSE
    /\ metrics_present = FALSE

StartTest ==
    /\ phase = "Idle"
    /\ phase' = "Running"
    /\ handle_present' = TRUE
    /\ UNCHANGED metrics_present

FinishSuccess ==
    /\ phase = "Running"
    /\ phase' = "Finished"
    /\ metrics_present' = TRUE
    /\ UNCHANGED handle_present

FinishFailure ==
    /\ phase = "Running"
    /\ phase' = "Finished"
    /\ UNCHANGED <<handle_present, metrics_present>>

Reset ==
    /\ phase = "Finished"
    /\ phase' = "Idle"
    /\ handle_present' = FALSE
    /\ metrics_present' = FALSE

Next ==
    \/ StartTest
    \/ FinishSuccess
    \/ FinishFailure
    \/ Reset

Spec == Init /\ [][Next]_vars

\* --- Invariants ---

TypeOK ==
    /\ phase \in Phases
    /\ handle_present \in BOOLEAN
    /\ metrics_present \in BOOLEAN

\* A running test always has a join handle
InvSingleRun == phase = "Running" => handle_present

\* Metrics are only present when finished (with success)
InvNoMetricsBeforeFinish == phase /= "Finished" => ~metrics_present

================================================================================
