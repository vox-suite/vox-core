use std::fs;

#[test]
fn validates_release_and_optionally_dispatches() {
    let workflow = fs::read_to_string(".github/workflows/publish.yml")
        .expect("Core publication workflow must exist");

    for required in [
        "branches: [main]",
        "workflow_dispatch:",
        "cargo test --locked",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo build --release --locked",
        "vars.VOX_AUTO_DEPLOY == 'true'",
        "secrets.VOX_DEPLOY_DISPATCH_TOKEN",
        "repos/vox-suite/vox-deploy/dispatches",
        "component_ready",
    ] {
        assert!(
            workflow.contains(required),
            "missing workflow contract: {required}"
        );
    }

    let test = workflow.find("cargo test --locked").unwrap();
    let lint = workflow.find("cargo clippy --locked").unwrap();
    let release = workflow.find("cargo build --release --locked").unwrap();
    let dispatch = workflow.find("Notify Vox Deploy").unwrap();
    assert!(test < dispatch && lint < dispatch && release < dispatch);
    assert!(!workflow.contains("docker/build-push-action"));
}
