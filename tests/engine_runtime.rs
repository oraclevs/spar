use std::fs;

use spar::Engine;

#[test]
fn execute_source_drives_async_main_to_completion() {
    let outcome = Engine::default()
        .execute_source(
            "async function answer() -> int { return 27; }; async function main() -> int { return await answer(); };",
        )
        .expect("async main should execute");
    assert_eq!(outcome.exit_status, 27);
}

#[test]
fn execute_source_drives_promise_created_during_module_initialization() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            async function answer() -> int { return 29; };
            var pending: Promise<int> = answer();
            async function main() -> int { return await pending; };
            "#,
        )
        .expect("module promise should belong to the execution runtime");
    assert_eq!(outcome.exit_status, 29);
}

#[test]
fn module_initialization_preserves_dependencies_between_promises() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            async function value() -> int { return 3; };
            async function increment(pending: Promise<int>) -> int {
                return await pending + 1;
            };
            var first: Promise<int> = value();
            var second: Promise<int> = increment(pending: first);
            async function main() -> int { return await second; };
            "#,
        )
        .expect("module promise dependency should execute");
    assert_eq!(outcome.exit_status, 4);
}

#[test]
fn async_main_preserves_void_and_shell_result_mapping() {
    let void_outcome = Engine::default()
        .execute_source("async function main() -> void { return; };")
        .expect("async void main should execute");
    assert_eq!(void_outcome.exit_status, 0);

    let shell_outcome = Engine::default()
        .execute_source("async function main() -> shell { return shell { false; }; };")
        .expect("async shell main should execute");
    assert_ne!(shell_outcome.exit_status, 0);
}

#[test]
fn execute_path_awaits_an_imported_async_function() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "async function answer() -> int { return 31; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "async function main() -> int { return await values::answer(); };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported async function should execute");
    assert_eq!(outcome.exit_status, 31);
}

#[test]
fn execute_path_drives_imported_promise_created_during_initialization() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "async function answer() -> int { return 37; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "var pending: Promise<int> = values::answer();\n",
            "async function main() -> int { return await pending; };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported module promise should execute");
    assert_eq!(outcome.exit_status, 37);
}

#[test]
fn phase4_language_fixtures_execute() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phase4");
    let struct_outcome = Engine::default()
        .execute_path(&root.join("struct_generics.spar"))
        .expect("generic struct fixture should execute");
    assert_eq!(struct_outcome.exit_status, 42);

    let catch_outcome = Engine::default()
        .execute_path(&root.join("try_catch.spar"))
        .expect("try/catch fixture should execute");
    assert_eq!(catch_outcome.exit_status, 7);

    let imported_outcome = Engine::default()
        .execute_path(&root.join("imported_struct.spar"))
        .expect("imported generic type fixture should execute");
    assert_eq!(imported_outcome.exit_status, 23);
}

#[test]
fn canonical_struct_uses_generic_defaults_and_field_access() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            type Config<T> { name: str = "default"; value: T; };
            struct App: Config<int> { value = 7; };
            function main() -> int {
                if App.name == "default" { return App.value; }
                return 0;
            };
            "#,
        )
        .expect("canonical struct should execute");
    assert_eq!(outcome.exit_status, 7);
}

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

#[test]
fn execute_path_runs_imported_generic_functions() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("generic.spar"),
        "function identity<T>(value: T) -> T { return value; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"generic.spar\" as generic;\n",
            "function main() -> int { return generic::identity<int>(value: 11); };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported generic should execute");
    assert_eq!(outcome.exit_status, 11);
}

#[test]
fn execute_path_preserves_generics_through_selective_imports() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("generic.spar"),
        concat!(
            "export type [Box<T>] { value: T; };\n",
            "function identity<T>(value: T) -> T { return value; };\n",
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import { identity } from \"generic.spar\";\n",
            "import type { Box } from \"generic.spar\";\n",
            "function main() -> int {\n",
            "    var boxed: Box<int> = { value: identity(value: 19); };\n",
            "    return boxed.value;\n",
            "};\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("selectively imported generics should execute");
    assert_eq!(outcome.exit_status, 19);
}
