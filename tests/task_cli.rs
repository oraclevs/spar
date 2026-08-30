use std::fs;
use std::process::Command;

fn spar(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(args)
        .output()
        .unwrap()
}

fn spar_in(args: &[&str], directory: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap()
}

fn write_fixture(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn tasks_lists_declared_tasks_with_descriptions() {
    let output = spar(&["tasks", "-f", "tests/fixtures/tasks/basic.spar"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("build") && stdout.contains("Build the project"));
    assert!(stdout.contains("test") && stdout.contains("Run the test suite"));
    assert!(stdout.contains("deploy") && stdout.contains("Deploy to an environment"));
}

#[test]
fn run_default_task_executes_when_no_task_given() {
    let output = spar(&["run", "-f", "tests/fixtures/tasks/basic.spar"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_explicit_task_runs_its_dependency_first() {
    let output = spar(&["run", "test", "-f", "tests/fixtures/tasks/basic.spar"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let build_pos = stderr.find("build demo").expect("Build command echoed");
    let test_pos = stderr.find("echo \"test\"").unwrap_or_else(|| {
        stderr
            .rfind("test")
            .expect("Test command echoed after Build")
    });
    assert!(
        build_pos < test_pos,
        "expected Build's command before Test's in:\n{stderr}"
    );
}

#[test]
fn run_task_with_typed_argument_interpolates_it() {
    let output = spar(&[
        "run",
        "deploy",
        "production",
        "-f",
        "tests/fixtures/tasks/basic.spar",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("deploy production"), "{stdout}");
}

#[test]
fn run_unknown_task_fails_with_diagnostic() {
    let output = spar(&[
        "run",
        "does-not-exist",
        "-f",
        "tests/fixtures/tasks/basic.spar",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("does-not-exist"), "{stderr}");
}

#[test]
fn run_with_no_default_task_configured_fails_and_lists_tasks() {
    let output = spar(&["run", "-f", "tests/fixtures/tasks/no_default.spar"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("default"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("build"), "{stdout}");
}

#[test]
fn multiple_default_tasks_fail_at_compile_time() {
    let output = spar(&[
        "run",
        "-f",
        "tests/fixtures/tasks/multiple_defaults.spar",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("multiple default"), "{stderr}");
}

#[test]
fn failing_command_produces_nonzero_exit() {
    let output = spar(&["run", "-f", "tests/fixtures/tasks/failing.spar"]);
    assert!(!output.status.success());
}

#[test]
fn dry_run_prints_commands_without_executing_them() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("marker");
    let src = format!(
        r#"task [Build] {{
    default: true;

    run {{
        printf marker > '{}';
    }};
}}"#,
        marker.display()
    );
    let file = write_fixture(temp.path(), "dry.spar", &src);

    let output = spar(&["run", "--file", file.to_str().unwrap(), "--dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists(), "dry-run must not spawn a process");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("marker"), "{stderr}");
}

#[test]
fn dry_run_flag_accepted_after_task_arguments() {
    let output = spar(&[
        "run",
        "deploy",
        "production",
        "--dry-run",
        "-f",
        "tests/fixtures/tasks/basic.spar",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn task_commands_discover_sparmake_in_a_parent_directory() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("one").join("two");
    fs::create_dir_all(&nested).unwrap();
    write_fixture(
        directory.path(),
        "SparMake.spar",
        "task [Build] { description: \"Found\"; run { true; }; }",
    );

    let output = spar_in(&["tasks"], &nested);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout).unwrap().contains("build"));
}

#[test]
fn missing_discovered_task_file_names_sparmake() {
    let directory = tempfile::tempdir().unwrap();

    let output = spar_in(&["tasks"], directory.path());

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("SparMake.spar")
    );
}
