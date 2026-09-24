/**
* Contract tests for local release verification hooks.
*/
use std::fs;

#[test]
fn pre_push_hook_runs_lint_test_and_release_build() {
    let hook = fs::read_to_string(".githooks/pre-push").expect("pre-push hook must exist");

    for required in [
        "#!/usr/bin/env bash",
        "set -euo pipefail",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo test --locked",
        "cargo build --release --locked",
    ] {
        assert!(
            hook.contains(required),
            "missing pre-push contract: {required}"
        );
    }

    let lint = hook.find("cargo clippy --locked").unwrap();
    let test = hook.find("cargo test --locked").unwrap();
    let release = hook.find("cargo build --release --locked").unwrap();
    assert!(lint < test && test < release);
    assert!(!hook.contains("docker/build-push-action"));
    assert!(!hook.contains("vox-deploy/dispatches"));
}
