mod common;

use common::{TestServer, spinr_cmd};
use predicates::prelude::*;

#[test]
fn trace_local_url_json() {
    let server = TestServer::start();
    let output = spinr_cmd()
        .args(["trace", &server.url(), "-j"])
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

    assert!(json["timing"]["dns_lookup_ms"].is_number());
    assert!(json["timing"]["tcp_connect_ms"].is_number());
    assert!(json["timing"]["time_to_first_byte_ms"].is_number());
    assert!(json["timing"]["total_ms"].is_number());
    assert_eq!(json["response"]["status"].as_u64().unwrap(), 200);
    assert!(json["connection"]["remote_ip"].is_string());
    assert!(json["connection"]["protocol"].is_string());
}

#[test]
fn trace_no_url_exits_with_error() {
    spinr_cmd().args(["trace"]).assert().failure().stderr(
        predicate::str::contains("URL is required")
            .or(predicate::str::contains("at least one URL")),
    );
}

#[test]
fn trace_human_readable_output() {
    let server = TestServer::start();
    spinr_cmd()
        .args(["trace", &server.url()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("DNS lookup")
                .and(predicate::str::contains("TCP connect"))
                .and(predicate::str::contains("TTFB"))
                .and(predicate::str::contains("Total")),
        );
}
