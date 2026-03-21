mod common;

use common::spinr_cmd;
use predicates::prelude::*;

#[test]
fn no_args_exits_with_usage() {
    spinr_cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: spinr"));
}

#[test]
fn help_flag_exits_0() {
    spinr_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("HTTP performance"));
}
