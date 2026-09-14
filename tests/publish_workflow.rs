use std::fs;

#[test]
fn publishes_tested_arm64_image_and_optionally_dispatches() {
    let workflow = fs::read_to_string(".github/workflows/publish.yml")
        .expect("Core publication workflow must exist");

    for required in [
        "branches: [main]",
        "workflow_dispatch:",
        "packages: write",
        "cargo test --locked",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo build --release --locked",
        "docker/login-action@v4",
        "docker/setup-buildx-action@v4",
        "docker/build-push-action@v7",
        "platforms: linux/arm64",
        "ghcr.io/vox-suite/vox-core:${{ github.sha }}",
        "digest: ${{ steps.build.outputs.digest }}",
        "vars.VOX_AUTO_DEPLOY == 'true'",
        "secrets.VOX_DEPLOY_DISPATCH_TOKEN",
        "repos/vox-suite/vox-deploy/dispatches",
        "component_published",
    ] {
        assert!(
            workflow.contains(required),
            "missing workflow contract: {required}"
        );
    }

    let test = workflow.find("cargo test --locked").unwrap();
    let lint = workflow.find("cargo clippy --locked").unwrap();
    let release = workflow.find("cargo build --release --locked").unwrap();
    let publish = workflow.find("docker/build-push-action@v7").unwrap();
    assert!(test < publish && lint < publish && release < publish);
}
