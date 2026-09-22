/**
* Integration tests verifying independent API and worker binaries.
*/
#[test]
fn builds_independent_api_and_worker_binaries() {
    assert!(env!("CARGO_BIN_EXE_vox-core-api").ends_with("vox-core-api"));
    assert!(env!("CARGO_BIN_EXE_vox-core-worker").ends_with("vox-core-worker"));
}
