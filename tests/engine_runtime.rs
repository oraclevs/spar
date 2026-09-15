use std::fs;

use spar::Engine;

#[test]
fn execute_path_runs_a_multi_file_projects_main() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "function answer() -> int { return 42; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "var result: int = values::answer();\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("execute should succeed");
    assert_eq!(outcome.exit_status, 42);
}

#[test]
fn check_path_never_evaluates_a_multi_file_project() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("broken.spar"),
        "export var answer: int = 1 / 0;\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"broken.spar\" as broken;\n",
            "var result: int = broken::answer;\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    // `check` never evaluates — the division by zero in `broken.spar`
    // would only be caught by a runtime that actually runs module init,
    // so a passing check here proves it stayed static.
    Engine::default()
        .check_path(&temp.path().join("main.spar"))
        .expect("check should succeed without evaluating anything");
}

#[test]
fn execute_path_reports_import_cycles_the_same_way_check_does() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("a.spar"),
        concat!("import \"b.spar\" as b;\n", "export var a: int = b::b;\n",),
    )
    .unwrap();
    fs::write(
        temp.path().join("b.spar"),
        concat!("import \"a.spar\" as a;\n", "export var b: int = a::a;\n",),
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"a.spar\" as a;\n",
            "var result: int = a::a;\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    let errors = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect_err("execute should fail on an import cycle");
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("import cycle detected")),
        "{errors:?}"
    );
}
