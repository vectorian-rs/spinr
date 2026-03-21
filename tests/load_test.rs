mod common;

use common::{TestServer, spinr_cmd};
use predicates::prelude::*;

#[test]
fn rate_limited_json_output() {
    let server = TestServer::start();
    let output = spinr_cmd()
        .args([
            "load-test",
            &server.url(),
            "-R",
            "50",
            "-d",
            "1",
            "-t",
            "1",
            "-j",
        ])
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

    assert!(json["total_requests"].as_u64().unwrap_or(0) > 0);
    assert!(json["rps"].is_number());
    assert!(json["latency_p50_ms"].is_number());
    assert!(json["latency_p99_ms"].is_number());
    assert!(json["duration_secs"].is_number());
    assert!(json["payload_bytes"].is_number());
    assert!(json["wire_bytes"].is_number());
    assert!(json["payload_transfer_per_sec"].is_number());
    assert!(json["wire_transfer_per_sec"].is_number());
}

#[test]
fn max_throughput_json_output() {
    let server = TestServer::start();
    let output = spinr_cmd()
        .args([
            "load-test",
            &server.url(),
            "--max-throughput",
            "-c",
            "2",
            "-d",
            "1",
            "-t",
            "1",
            "-j",
        ])
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

    assert!(json["total_requests"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn bad_url_exits_with_error() {
    spinr_cmd()
        .args([
            "load-test",
            "http://this-host-does-not-exist.invalid/",
            "-R",
            "10",
            "-d",
            "1",
            "-t",
            "1",
        ])
        .assert()
        .failure();
}

#[test]
fn https_url_rejected() {
    spinr_cmd()
        .args([
            "load-test",
            "https://127.0.0.1:1/",
            "-R",
            "10",
            "-d",
            "1",
            "-t",
            "1",
        ])
        .assert()
        .failure();
}

#[test]
fn post_with_body_and_headers() {
    let server = TestServer::start();
    let output = spinr_cmd()
        .args([
            "load-test",
            &server.url(),
            "-R",
            "50",
            "-d",
            "1",
            "-t",
            "1",
            "-m",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-b",
            r#"{"key":"value"}"#,
            "-j",
        ])
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

    assert!(json["total_requests"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn human_readable_output_has_labels() {
    let server = TestServer::start();
    spinr_cmd()
        .args(["load-test", &server.url(), "-R", "50", "-d", "1", "-t", "1"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Total requests")
                .and(predicate::str::contains("Latency avg"))
                .and(predicate::str::contains("Actual RPS")),
        );
}
