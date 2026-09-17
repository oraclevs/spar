use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

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

#[test]
fn exec_uses_main_integer_as_process_status() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("exit-7.spar"),
        "function main() -> int { return 7; };",
    )
    .unwrap();
    let output = spar_in(&["exec", "exit-7.spar"], dir.path());
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}

#[test]
fn exec_drives_async_main_and_uses_its_integer_status() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("async-main.spar"),
        "async function result() -> int { return 6; }; async function main() -> int { return await result(); };",
    )
    .unwrap();
    let output = spar_in(&["exec", "async-main.spar"], dir.path());
    assert_eq!(output.status.code(), Some(6), "{output:?}");
}

#[test]
fn exec_void_main_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("void-main.spar"),
        "function main() -> void { };",
    )
    .unwrap();
    let output = spar_in(&["exec", "void-main.spar"], dir.path());
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn bare_dot_slash_spar_path_is_shorthand_for_exec() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("app.spar"),
        "function main() -> int { return 3; };",
    )
    .unwrap();
    let output = spar_in(&["./app.spar"], dir.path());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
}

#[test]
fn bare_existing_spar_filename_is_shorthand_for_exec() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("app.spar"),
        "function main() -> int { return 5; };",
    )
    .unwrap();
    let output = spar_in(&["app.spar"], dir.path());
    assert_eq!(output.status.code(), Some(5), "{output:?}");
}

#[test]
fn run_exec_still_runs_a_task_literally_named_exec() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("SparMake.spar"),
        concat!(
            "task [Exec] {\n",
            "    run linux { touch ran-exec-task; };\n",
            "    run macos { touch ran-exec-task; };\n",
            "};\n",
        ),
    )
    .unwrap();
    let output = spar_in(&["run", "exec"], dir.path());
    assert!(output.status.success(), "{output:?}");
    assert!(dir.path().join("ran-exec-task").exists());
}

#[test]
fn check_does_not_call_main() {
    let dir = tempfile::tempdir().unwrap();
    // If `check` ever called `main`, this would still exit 0 (division
    // never runs), so this test only proves check doesn't *crash* trying
    // to call it — paired with the unit-level engine tests that prove
    // check never evaluates at all.
    fs::write(
        dir.path().join("exit-7.spar"),
        "function main() -> int { return 7; };",
    )
    .unwrap();
    let output = spar_in(&["check", "exit-7.spar"], dir.path());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "exit-7.spar: ok"
    );
}

#[test]
fn exec_reports_a_missing_main_with_a_clear_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("no-main.spar"), "var x: int = 1;").unwrap();
    let output = spar_in(&["exec", "no-main.spar"], dir.path());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no 'main' function"), "{stderr}");
}

#[test]
fn exec_imports_a_sibling_module() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("values.spar"),
        "function answer() -> int { return 9; };\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "var result: int = values::answer();\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();
    let output = spar_in(&["exec", "main.spar"], dir.path());
    assert_eq!(output.status.code(), Some(9), "{output:?}");
}

#[test]
fn fmt_then_exec_preserves_native_shell_loops_and_local_mutation() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loop.spar"),
        r#"function main() -> shell {
    var files: [str] = ["one", "two"];
    return shell {
        var mut count: int = 0;
        for file in files {
            echo "${file}";
            count += 1;
        }
        echo "${count}";
    };
};
"#,
    )
    .unwrap();

    let formatted = spar_in(&["fmt", "loop.spar"], dir.path());
    assert!(formatted.status.success(), "{formatted:?}");
    let output = spar_in(&["exec", "loop.spar"], dir.path());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"one\ntwo\n2\n");
}

#[test]
fn repl_evaluates_fragments_and_exits_zero_on_eof() {
    let output = spar_with_input(
        &["repl"],
        concat!(
            "var mut count: int = 1;\n",
            "count = count + 1;\n",
            "count = \"bad\";\n",
        ),
    );
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("count") || stderr.to_lowercase().contains("type"),
        "expected a diagnostic for the bad assignment, got: {stderr}"
    );
}

#[test]
fn repl_waits_for_a_multiline_function_body_before_evaluating() {
    let output = spar_with_input(
        &["repl"],
        concat!(
            "function double(x: int) -> int {\n",
            "    return x * 2;\n",
            "};\n",
            "var y: int = double(x: 21);\n",
        ),
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
