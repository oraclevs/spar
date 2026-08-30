use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

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

fn spar_with_input(args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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
    let output = spar(&["run", "-f", "tests/fixtures/tasks/multiple_defaults.spar"]);
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
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("SparMake.spar"));
}

#[test]
fn tasks_groups_public_tasks_and_hides_private_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "groups.spar",
        r#"
task [Build] { description: "Compile"; run { true; }; }
task [Deploy] { group: "release"; run { true; }; }
task [Secrets] { private: true; group: "release"; run { true; }; }
"#,
    );

    let output = spar(&["tasks", "-f", file.to_str().unwrap()]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Ungrouped") && stdout.contains("release"),
        "{stdout}"
    );
    assert!(
        stdout.contains("build") && stdout.contains("deploy"),
        "{stdout}"
    );
    assert!(!stdout.contains("secrets"), "{stdout}");

    let output = spar(&["tasks", "--all", "-f", file.to_str().unwrap()]);
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("secrets"));
}

#[test]
fn run_choose_accepts_a_number_or_task_name_and_hides_private_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let first_marker = directory.path().join("first");
    let second_marker = directory.path().join("second");
    let file = write_fixture(
        directory.path(),
        "choose.spar",
        &format!(
            r#"
task [Build] {{ run {{ printf chosen > '{}'; }}; }}
task [Test] {{ group: "quality"; run {{ printf named > '{}'; }}; }}
task [Secret] {{ private: true; run {{ true; }}; }}
"#,
            first_marker.display(),
            second_marker.display()
        ),
    );

    let output = spar_with_input(&["run", "--choose", "-f", file.to_str().unwrap()], "1\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(first_marker.exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("secret"), "{stderr}");

    let output = spar_with_input(&["run", "--choose", "-f", file.to_str().unwrap()], "test\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(second_marker.exists());
}

#[test]
fn show_binds_only_the_requested_task() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "show.spar",
        r#"
task [Build] { run { echo dependency; }; }
task [Deploy](environment: str) {
    dependsOn: [Build];
    confirm: "Do not prompt";
    run { echo deploy-${environment}; };
}
"#,
    );

    let output = spar(&["show", "deploy", "production", "-f", file.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("echo deploy-production"), "{stdout}");
    assert!(!stdout.contains("dependency"), "{stdout}");
    assert!(!String::from_utf8(output.stderr).unwrap().contains("[y/N]"));
}

#[test]
fn dump_emits_the_complete_lowered_catalog_as_json() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "dump.spar",
        r#"
task [Deploy](environment: str = "staging", *extra: str) {
    description: "Deploy app";
    private: true;
    group: "release";
    confirm: "Continue?";
    os: ["linux", "macos", "windows"];
    dependsOn: [Build];
    cwd: "deploy";
    shell: ["bash", "-c"];
    env: { MODE: "release"; };
    run { deploy ${environment} ${extra}; };
}
task [Build] { run { build; }; }
"#,
    );

    let output = spar(&["dump", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let deploy = value["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["name"] == "Deploy")
        .unwrap();

    assert_eq!(deploy["description"], "Deploy app");
    assert_eq!(deploy["group"], "release");
    assert_eq!(deploy["private"], true);
    assert_eq!(deploy["confirm"], "Continue?");
    assert_eq!(
        deploy["os"],
        serde_json::json!(["linux", "macos", "windows"])
    );
    assert_eq!(deploy["dependencies"], serde_json::json!(["Build"]));
    assert_eq!(deploy["parameters"][0]["default"], "staging");
    assert_eq!(deploy["parameters"][1]["variadic"], true);
    assert_eq!(deploy["environment"]["MODE"], "release");
    assert_eq!(deploy["cwd"], "deploy");
    assert_eq!(deploy["shell"], serde_json::json!(["bash", "-c"]));
    assert_eq!(deploy["commands"][0]["kind"], "shell");
    assert_eq!(
        deploy["commands"][0]["template"],
        "deploy ${environment} ${extra}"
    );
}

#[test]
fn private_task_remains_explicitly_runnable() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("private");
    let file = write_fixture(
        directory.path(),
        "private.spar",
        &format!(
            "task [Secret] {{ private: true; run {{ printf yes > '{}'; }}; }}",
            marker.display()
        ),
    );

    let output = spar(&["run", "secret", "-f", file.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists());
}

#[test]
fn run_choose_rejects_an_invalid_selection() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "choose-invalid.spar",
        "task [Build] { run { true; }; }",
    );

    let output = spar_with_input(
        &["run", "--choose", "-f", file.to_str().unwrap()],
        "missing\n",
    );

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("invalid task choice"));
}

#[test]
fn show_prints_a_shebang_script_in_full() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "script.spar",
        r#"task [Script] {
    run {
        #!/bin/sh
        echo first; echo second
    };
}"#,
    );

    let output = spar(&["show", "script", "-f", file.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("#!/bin/sh"), "{stdout}");
    assert!(stdout.contains("echo first; echo second"), "{stdout}");
}
