mod common;

use common::{TestServer, spinr_cmd, write_bench_toml};
use predicates::prelude::*;

#[test]
fn single_scenario_json() {
    let server = TestServer::start();
    let dir = tempfile::tempdir().unwrap();
    let toml_content = format!(
        r#"
[[scenario]]
name = "basic-get"
url = "{}"
rate = 30
duration = 1
threads = 1
"#,
        server.url()
    );
    let config_path = write_bench_toml(dir.path(), &toml_content);

    let output = spinr_cmd()
        .args(["bench", config_path.to_str().unwrap(), "-j"])
        .output()
        .expect("failed to run spinr");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout: {stdout}"));

    let scenarios = json["scenarios"].as_array().expect("scenarios array");
    assert_eq!(scenarios.len(), 1);
    assert_eq!(scenarios[0]["name"].as_str().unwrap(), "basic-get");
    assert!(json["total_requests"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn multi_scenario_runs_all() {
    let server = TestServer::start();
    let dir = tempfile::tempdir().unwrap();
    let toml_content = format!(
        r#"
[[scenario]]
name = "first"
url = "{url}"
rate = 20
duration = 1
threads = 1

[[scenario]]
name = "second"
url = "{url}"
rate = 20
duration = 1
threads = 1
"#,
        url = server.url()
    );
    let config_path = write_bench_toml(dir.path(), &toml_content);

    let output = spinr_cmd()
        .args(["bench", config_path.to_str().unwrap(), "-j"])
        .output()
        .expect("failed to run spinr");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout: {stdout}"));

    let scenarios = json["scenarios"].as_array().expect("scenarios array");
    assert_eq!(scenarios.len(), 2);
    assert_eq!(scenarios[0]["name"].as_str().unwrap(), "first");
    assert_eq!(scenarios[1]["name"].as_str().unwrap(), "second");
}

#[test]
fn missing_config_file() {
    spinr_cmd()
        .args(["bench", "/tmp/nonexistent_spinr_bench_config_xyz.toml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read bench config"));
}

#[test]
fn invalid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = write_bench_toml(dir.path(), "this is not valid [[[ toml");

    spinr_cmd()
        .args(["bench", config_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse bench config"));
}

#[test]
fn empty_scenarios() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = write_bench_toml(dir.path(), "scenario = []");

    spinr_cmd()
        .args(["bench", config_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least one"));
}
