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
    let build_pos = stderr
        .find("build ${appName}")
        .expect("Build command echoed");
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

task QuietFailure {
    quiet: true;
    run bash {
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
        r#"task QuietSuccess {
    quiet: true;
    run bash {
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
        r#"task LoudFailure {
    quiet: false;
    run bash {
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
        r#"task DefaultFailure {
    run bash {
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
        r#"task Build {{
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
        "task Build { description: \"Found\"; run { true; }; };",
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
task Build { description: "Compile"; run { true; }; };
task Deploy { group: "release"; run { true; }; };
task Secrets { private: true; group: "release"; run { true; }; };
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
task Build {{ run {{ printf chosen > '{}'; }}; }};
task Test {{ group: "quality"; run {{ printf named > '{}'; }}; }};
task Secret {{ private: true; run {{ true; }}; }};
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
task Build { run { echo dependency; }; };
task Deploy(environment: str) {
    dependsOn: [Build];
    confirm: "Do not prompt";
    run bash { echo deploy-${environment}; };
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
task Deploy(environment: str = "staging", *extra: str) {
    description: "Deploy app";
    private: true;
    group: "release";
    confirm: "Continue?";
    dependsOn: [Build];
    cwd: "deploy";
    env: { MODE: "release"; };
    run bash { deploy ${environment} ${extra}; };
};
task Build { run { build; }; };
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
    // Environment values are never printed; only the key names.
    assert_eq!(deploy["environment"]["MODE"], "<redacted>");
    assert_eq!(deploy["cwd"], "deploy");
    assert_eq!(deploy["commands"][0]["kind"], "bash");
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
            "task Secret {{ private: true; run {{ printf yes > '{}'; }}; }};",
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
        "task Build { run { true; }; };",
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
        r#"task Script {
    run bash {
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
        r#"task Check { default: true; run { true; }; };"#,
    );

    // `spar check <file>` must run the `check` subcommand (compile-check the
    // given file), not the task named `Check` — even though the task exists.
    let output = spar(&["check", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .ends_with(": ok\n"));
}

#[test]
fn task_name_shadowing_a_reserved_command_warns() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "shadow.spar",
        r#"task Check { default: true; run { true; }; };"#,
    );

    let output = spar(&["tasks", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("shadowed by the reserved `check` command"),
        "{stderr}"
    );
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
        r#"task Cpd(file: str = "main.dart", out: str = "main") {
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
    assert!(
        stdout.contains("compiling main.dart to result2"),
        "{stdout}"
    );
}

#[test]
fn named_arguments_cannot_mix_with_positional_ones_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "cpd.spar",
        r#"task Cpd(file: str = "main.dart", out: str = "main") {
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
    assert!(
        stderr.contains("mixes named and positional arguments"),
        "{stderr}"
    );
}

#[test]
fn native_run_block_executes_with_param_and_global_interpolation() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "native.spar",
        "export var app: str = \"demo\";\n\
         task Greet(name: str = \"world\") {\n\
             run { echo \"hello ${name} from ${app}\"; };\n\
         };\n",
    );
    let output = spar(&["run", "greet", "spar", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "hello spar from demo"
    );
}

#[test]
fn bash_run_block_supports_bash_only_syntax() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "bash.spar",
        "task B { run bash { [[ 1 -eq 1 ]] && echo ok; }; };\n",
    );
    let output = spar(&["run", "b", "-f", file.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "ok");
}

#[test]
fn native_block_failure_reports_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "fail.spar",
        "task F { run { exit 3; }; };\n",
    );
    let output = spar(&["run", "f", "-f", file.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("exited with status 3"), "{stderr}");
}

#[test]
fn native_block_spreads_variadic_parameters_with_ellipsis() {
    let directory = tempfile::tempdir().unwrap();
    let file = write_fixture(
        directory.path(),
        "variadic.spar",
        "task Cpd(file: str = \"main.dart\", *tags: str) {\n\
             run { echo compile ${file} ...${tags}; };\n\
         };\n",
    );
    let with_tags = spar(&[
        "run",
        "cpd",
        "lib.dart",
        "release",
        "fast",
        "-f",
        file.to_str().unwrap(),
    ]);
    assert!(
        with_tags.status.success(),
        "{}",
        String::from_utf8_lossy(&with_tags.stderr)
    );
    assert_eq!(
        String::from_utf8(with_tags.stdout).unwrap().trim(),
        "compile lib.dart release fast"
    );

    let without_tags = spar(&["run", "cpd", "-f", file.to_str().unwrap()]);
    assert!(without_tags.status.success());
    assert_eq!(
        String::from_utf8(without_tags.stdout).unwrap().trim(),
        "compile main.dart"
    );
}

#[test]
fn load_env_values_are_visible_to_stdlib_env_in_native_run_blocks() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), ".env", "SPAR_LOADENV_TEST_VALUE=from-dotenv\n");
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"@LoadEnv

import pkg { getOr, has } from "std/env";

task ShowEnv {
    run {
        var value: str = getOr(name: "SPAR_LOADENV_TEST_VALUE", fallback: "missing");
        echo "${value}";
    };
};
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["run", "ShowEnv", "-f", "tasks.spar"])
        .current_dir(dir.path())
        .env_remove("SPAR_LOADENV_TEST_VALUE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "from-dotenv"
    );
}

#[test]
fn host_environment_wins_over_load_env_in_stdlib_env() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), ".env", "SPAR_LOADENV_TEST_VALUE=from-dotenv\n");
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"@LoadEnv

import pkg { getOr } from "std/env";

task ShowEnv {
    run {
        var value: str = getOr(name: "SPAR_LOADENV_TEST_VALUE", fallback: "missing");
        echo "${value}";
    };
};
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["run", "ShowEnv", "-f", "tasks.spar"])
        .current_dir(dir.path())
        .env("SPAR_LOADENV_TEST_VALUE", "from-host")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "from-host");
}

#[test]
fn load_env_values_are_visible_to_stdlib_env_in_file_level_vars() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), ".env", "SPAR_LOADENV_TEST_VALUE=from-dotenv\n");
    write_fixture(
        dir.path(),
        "config.spar",
        r#"@LoadEnv

import pkg { getOr } from "std/env";

export var value: str = getOr(name: "SPAR_LOADENV_TEST_VALUE", fallback: "missing");
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["emit", "config.spar"])
        .current_dir(dir.path())
        .env_remove("SPAR_LOADENV_TEST_VALUE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("\"from-dotenv\""),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn pound_brace_escape_is_not_interpreted_in_native_run_blocks() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        "task Show {\n    run {\n        echo \"#{HOME}\";\n    };\n};\n",
    );
    let output = spar_in(&["run", "Show", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "#{HOME}");
}

#[test]
fn trailing_comment_on_native_command_does_not_swallow_next_command() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        "task Cmds {\n    run {\n        echo a; // ta\n        echo b;\n    };\n};\n",
    );
    let output = spar_in(&["run", "Cmds", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a\nb\n");
}

#[test]
fn exit_in_a_native_run_body_ends_the_task_with_that_code() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"import pkg { exit } from "std/process";

task Guard {
    run {
        echo before;
        exit(code: 3);
        echo after;
    };
};
"#,
    );
    let output = spar_in(&["run", "Guard", "-f", "tasks.spar"], dir.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exited with status 3"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Commands queued before `exit` still run; nothing after it does.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
}

#[test]
fn exit_zero_in_a_native_run_body_succeeds_and_skips_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"import pkg { exit } from "std/process";

task Done {
    run {
        echo before;
        exit(code: 0);
        false;
    };
};
"#,
    );
    let output = spar_in(&["run", "Done", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exit_command_in_a_native_run_body_sets_the_task_status_after_earlier_commands() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        "task Leave {\n    run {\n        echo one;\n        exit 3;\n        echo two;\n    };\n};\n",
    );
    let output = spar_in(&["run", "Leave", "-f", "tasks.spar"], dir.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exited with status 3"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "one\n");
}

#[test]
fn exec_shell_can_be_used_as_a_statement_and_runs_in_order() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        "task Ordered {\n    run {\n        exec shell { echo first; };\n        exec shell { echo second; };\n        echo third;\n    };\n};\n",
    );
    let output = spar_in(&["run", "Ordered", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `exec` runs while the body is evaluated; queued commands run after it.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "first\nsecond\nthird\n"
    );
}

#[test]
fn exec_children_see_variables_set_with_std_env_and_loaded_from_dotenv() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), ".env", "SPAR_EXEC_FROM_DOTENV=dot\n");
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"@LoadEnv

import pkg { set } from "std/env";

task Show {
    env: { SPAR_EXEC_FROM_TASK: "task"; };
    run {
        set(name: "SPAR_EXEC_FROM_SET", value: "set");
        exec { printenv SPAR_EXEC_FROM_SET; };
        exec { printenv SPAR_EXEC_FROM_DOTENV; };
        exec { printenv SPAR_EXEC_FROM_TASK; };
    };
};
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["run", "Show", "-f", "tasks.spar"])
        .current_dir(dir.path())
        .env_remove("SPAR_EXEC_FROM_DOTENV")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "set\ndot\ntask\n");
}

#[test]
fn else_if_chains_work_in_native_run_bodies() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        "task Pick(n: int) {\n    run {\n        if n == 1 {\n            echo one;\n        } else if n == 2 {\n            echo two;\n        } else {\n            echo many;\n        }\n    };\n};\n",
    );
    for (arg, expected) in [("1", "one\n"), ("2", "two\n"), ("9", "many\n")] {
        let output = spar_in(&["run", "Pick", arg, "-f", "tasks.spar"], dir.path());
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
}

#[test]
fn single_line_control_blocks_work_in_native_run_bodies() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        concat!(
            "task Inline(n: int) {\n    run {\n",
            "        if n == 1 { echo one; } else { echo other; }\n",
            "        for i in [1, 2] { echo item-${i}; }\n",
            "        if n > 5 { echo big; } else if n > 1 { echo mid; } else { echo small; }\n",
            "        echo done;\n",
            "    };\n};\n",
        ),
    );
    let output = spar_in(&["run", "Inline", "1", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "one\nitem-1\nitem-2\nsmall\ndone\n"
    );
}

#[test]
fn env_prefix_values_interpolate_in_native_run_bodies() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"var secret: str = "sec ret";
var port: int = 5432;

task Show {
    run {
        SPAR_A=${secret} printenv SPAR_A;
        SPAR_B="${secret}" printenv SPAR_B;
        SPAR_C="db://u:${secret}@h:${port}/x" printenv SPAR_C;
        SPAR_D=plain printenv SPAR_D;
        SPAR_E=$SPAR_OUTER printenv SPAR_E;
    };
};
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["run", "Show", "-f", "tasks.spar"])
        .current_dir(dir.path())
        .env("SPAR_OUTER", "outer")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "sec ret\nsec ret\ndb://u:sec ret@h:5432/x\nplain\nouter\n"
    );
}

#[test]
fn env_prefix_values_interpolate_in_scripts_run_by_the_compiled_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(
        dir.path(),
        "script.spar",
        r#"function main() -> shell {
    var secret: str = "s3";
    return shell {
        SPAR_X="${secret}-x" printenv SPAR_X;
    };
};
"#,
    );
    let output = spar_in(&["exec", file.to_str().unwrap()], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "s3-x\n");
}

#[test]
fn env_prefix_values_survive_formatting() {
    let source = "task Show {\n    run {\n        SPAR_A=\"${secret}\" printenv SPAR_A;\n        SPAR_B=plain printenv SPAR_B;\n    };\n};\n";
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path(), "f.spar", source);
    let output = spar_in(&["fmt", "--check", file.to_str().unwrap()], dir.path());
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dump_lists_environment_keys_but_never_their_values() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), ".env", "SPAR_DUMP_SECRET=hunter2-from-dotenv\n");
    write_fixture(
        dir.path(),
        "tasks.spar",
        r#"@LoadEnv

task Serve {
    env: { PORT: "8080"; TOKEN: "hunter2-from-task"; };
    run { echo hi; };
};
"#,
    );
    let output = spar_in(&["dump", "-f", "tasks.spar"], dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("hunter2"), "secret leaked:\n{text}");
    assert!(!text.contains("8080"), "value leaked:\n{text}");
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let environment = &json["tasks"][0]["environment"];
    assert_eq!(environment["SPAR_DUMP_SECRET"], "<redacted>", "{text}");
    assert_eq!(environment["PORT"], "<redacted>", "{text}");
    assert_eq!(environment["TOKEN"], "<redacted>", "{text}");
}
