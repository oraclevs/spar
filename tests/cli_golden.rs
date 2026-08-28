use std::fs;
use std::process::Command;

#[test]
fn emit_output_matches_golden() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("app.spar");
    fs::write(&input, include_str!("fixtures/cli/app.spar")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["emit", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        include_str!("fixtures/cli/emit.stdout")
    );
}
