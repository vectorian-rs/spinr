use std::process::Command;

#[test]
fn tla_lifecycle_spec() {
    let output = Command::new("tla")
        .arg("spec/LoadTestLifecycle.tla")
        .arg("--allow-deadlock")
        .output()
        .expect("tla-checker not installed; run: cargo install tla-checker");
    assert!(
        output.status.success(),
        "TLA+ lifecycle spec failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tla_connection_fsm_spec() {
    let output = Command::new("tla")
        .arg("spec/ConnectionFSM.tla")
        .arg("--allow-deadlock")
        .output()
        .expect("tla-checker not installed; run: cargo install tla-checker");
    assert!(
        output.status.success(),
        "TLA+ connection FSM spec failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
