use std::fs;
use std::process::Command;

fn run_emit(input: &std::path::Path, extra_args: &[&str]) -> String {
    let mut args = vec!["emit", input.to_str().unwrap()];
    args.extend_from_slice(extra_args);
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(&args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn emit_output_matches_golden() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("app.spar");
    fs::write(&input, include_str!("fixtures/cli/app.spar")).unwrap();
    assert_eq!(
        run_emit(&input, &[]),
        include_str!("fixtures/cli/emit.stdout")
    );
}

#[test]
fn emit_yaml_output_matches_golden() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("app.spar");
    fs::write(&input, include_str!("fixtures/cli/app.spar")).unwrap();
    assert_eq!(
        run_emit(&input, &["-y"]),
        include_str!("fixtures/cli/emit.yaml.stdout")
    );
    assert_eq!(
        run_emit(&input, &["--yaml"]),
        include_str!("fixtures/cli/emit.yaml.stdout")
    );
}

#[test]
fn emit_toml_output_matches_golden() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("app.spar");
    fs::write(&input, include_str!("fixtures/cli/app.spar")).unwrap();
    assert_eq!(
        run_emit(&input, &["-t"]),
        include_str!("fixtures/cli/emit.toml.stdout")
    );
    assert_eq!(
        run_emit(&input, &["--toml"]),
        include_str!("fixtures/cli/emit.toml.stdout")
    );
}

#[test]
fn emit_rejects_multiple_format_flags() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("app.spar");
    fs::write(&input, include_str!("fixtures/cli/app.spar")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["emit", input.to_str().unwrap(), "-y", "-t"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("only one format flag"));
}

#[test]
fn emit_toml_preserves_int_vs_float_typing() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("numbers.spar");
    fs::write(
        &input,
        "#[emit]\nvar count: int = 2;\n#[emit]\nvar rate: float = 2.0;\n",
    )
    .unwrap();
    let stdout = run_emit(&input, &["-t"]);
    assert!(stdout.contains("count = 2\n"), "{stdout}");
    assert!(stdout.contains("rate = 2.0\n"), "{stdout}");
}

#[test]
fn emit_without_marked_items_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain.spar");
    fs::write(&file, "struct A { x: int = 1; };\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["emit", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing to emit: mark top-level structs or vars with #[emit]"),
        "{stderr}"
    );
}
