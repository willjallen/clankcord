use std::path::PathBuf;
use std::process::Command;

#[test]
fn dashboard_frontend_contracts() {
    let test_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests").join("behavior")
        .join("dashboard_frontend.mjs");
    let output = Command::new("node")
        .arg("--test")
        .arg(&test_path)
        .output()
        .expect("Node.js is required to test the dashboard frontend contracts");
    assert!(
        output.status.success(),
        "dashboard frontend tests failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
