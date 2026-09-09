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
    // quiet defaults true now, so use --dry-run to see the echoed plan order.
    let output = spar(&[
        "run",
        "test",
        "-f",
        "tests/fixtures/tasks/basic.spar",
        "--dry-run",
    ]);
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

#[cfg(unix)]
#[test]
fn quiet_failing_task_reports_source_location_without_dumping_script() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "quiet-failure.spar",
        r#"

task [QuietFailure] {
    quiet: true;
    run {
        #!/bin/sh
        # FULL_SCRIPT_SHOULD_NOT_APPEAR
        printf 'short failure detail\n' >&2
        exit 7
    };
};
"#,
    );

    let output = spar(&["run", "quietfailure", "-f", file.to_str().unwrap()]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("short failure detail"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: task QuietFailure ({}:3) failed: exit status: 7",
            file.display()
        )),
        "{stderr}"
    );
    assert!(
        !stderr.contains("FULL_SCRIPT_SHOULD_NOT_APPEAR"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn quiet_successful_task_does_not_echo_script() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "quiet-success.spar",
        r#"task [QuietSuccess] {
    quiet: true;
    run {
        #!/bin/sh
        # QUIET_SUCCESS_SCRIPT_SHOULD_NOT_APPEAR
        printf quiet-success-output
    };
};"#,
    );

    let output = spar(&["run", "quietsuccess", "-f", file.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "quiet-success-output"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.is_empty(), "{stderr}");
}

#[cfg(unix)]
#[test]
fn explicit_quiet_false_failing_task_still_dumps_full_script() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "loud-failure.spar",
        r#"task [LoudFailure] {
    quiet: false;
    run {
        #!/bin/sh
        # LOUD_FAILURE_FULL_SCRIPT
        exit 7
    };
};"#,
    );

    let output = spar(&["run", "loudfailure", "-f", file.to_str().unwrap()]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("LOUD_FAILURE_FULL_SCRIPT"), "{stderr}");
    assert!(
        stderr.contains(
            "error: task LoudFailure command \"#!/bin/sh\\n        # LOUD_FAILURE_FULL_SCRIPT\\n        exit 7\" failed with status exit status: 7"
        ),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn default_task_is_quiet_and_hides_full_script_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "default-quiet-failure.spar",
        r#"task [DefaultFailure] {
    run {
        #!/bin/sh
        # DEFAULT_QUIET_FULL_SCRIPT_SHOULD_NOT_APPEAR
        printf 'quiet by default\n' >&2
        exit 7
    };
};"#,
    );

    let output = spar(&["run", "defaultfailure", "-f", file.to_str().unwrap()]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("quiet by default"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: task DefaultFailure ({}:1) failed: exit status: 7",
            file.display()
        )),
        "{stderr}"
    );
    assert!(
        !stderr.contains("DEFAULT_QUIET_FULL_SCRIPT_SHOULD_NOT_APPEAR"),
        "{stderr}"
    );
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
}};"#,
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
        "task [Build] { description: \"Found\"; run { true; }; };",
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
task [Build] { description: "Compile"; run { true; }; };
task [Deploy] { group: "release"; run { true; }; };
task [Secrets] { private: true; group: "release"; run { true; }; };
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
task [Build] {{ run {{ printf chosen > '{}'; }}; }};
task [Test] {{ group: "quality"; run {{ printf named > '{}'; }}; }};
task [Secret] {{ private: true; run {{ true; }}; }};
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
task [Build] { run { echo dependency; }; };
task [Deploy](environment: str) {
    dependsOn: [Build];
    confirm: "Do not prompt";
    run { echo deploy-${environment}; };
};
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
    dependsOn: [Build];
    cwd: "deploy";
    shell: ["bash", "-c"];
    env: { MODE: "release"; };
    run { deploy ${environment} ${extra}; };
};
task [Build] { run { build; }; };
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
            "task [Secret] {{ private: true; run {{ printf yes > '{}'; }}; }};",
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
        "task [Build] { run { true; }; };",
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
};"#,
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

#[test]
fn bare_task_name_is_shorthand_for_run() {
    let output = spar(&[
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
}

#[test]
fn bare_invocation_with_no_task_name_runs_default_task() {
    let output = spar(&["-f", "tests/fixtures/tasks/basic.spar"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bare_unknown_task_name_still_fails_with_diagnostic() {
    let output = spar(&["does-not-exist", "-f", "tests/fixtures/tasks/basic.spar"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("does-not-exist"), "{stderr}");
}

#[test]
fn bare_reserved_keyword_still_runs_the_subcommand_not_a_same_named_task() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "shadow.spar",
        r#"task [Check] { default: true; run { true; }; };"#,
    );

    // `spar check <file>` must run the `check` subcommand (compile-check the
    // given file), not the task named `Check` — even though the task exists.
    let output = spar(&["check", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout).unwrap().ends_with(": ok\n"));
}

#[test]
fn task_name_shadowing_a_reserved_command_warns() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "shadow.spar",
        r#"task [Check] { default: true; run { true; }; };"#,
    );

    let output = spar(&["tasks", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("shadowed by the reserved `check` command"), "{stderr}");
    assert!(stderr.contains("spar run check"), "{stderr}");

    // Still reachable via explicit `run`.
    let output = spar(&["run", "check", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn named_argument_overrides_one_default_and_leaves_the_other() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "cpd.spar",
        r#"task [Cpd](file: str = "main.dart", out: str = "main") {
    default: true;
    run { echo "compiling ${file} to ${out}"; };
};"#,
    );

    let output = spar(&["run", "cpd", "out=result", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("compiling main.dart to result"), "{stdout}");

    // Bare dispatch gets the same feature for free.
    let output = spar(&["cpd", "out=result2", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("compiling main.dart to result2"), "{stdout}");
}

#[test]
fn named_arguments_cannot_mix_with_positional_ones_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "cpd.spar",
        r#"task [Cpd](file: str = "main.dart", out: str = "main") {
    run { echo "compiling ${file} to ${out}"; };
};"#,
    );

    let output = spar(&[
        "run",
        "cpd",
        "out=result",
        "lib.dart",
        "-f",
        file.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("mixes named and positional arguments"), "{stderr}");
}
