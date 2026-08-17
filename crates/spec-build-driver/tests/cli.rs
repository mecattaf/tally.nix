use std::process::Command;

#[test]
fn packaged_binary_exposes_the_native_action_surface() {
    let output = Command::new(env!("CARGO_BIN_EXE_spec-build-driver"))
        .arg("--help")
        .output()
        .expect("the Rust spec-build driver should launch");

    assert!(
        output.status.success(),
        "driver help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("driver help should be UTF-8");
    for action in [
        "worklist",
        "sweep",
        "reconcile",
        "diff",
        "steeringRecheck",
        "steer",
        "retry",
        "escalate",
        "continue",
        "preflight",
        "prep",
        "cleanup",
        "ownership",
        "treeDelta",
        "constraint",
        "checkpoint",
        "publish",
        "stagePublish",
        "rebase",
        "merge",
    ] {
        assert!(stdout.contains(action), "driver help omitted {action}");
    }
}
