use spar::{CompileOptions, Engine, RuntimeContext};

#[test]
fn fs_path_env_and_time_modules_execute_against_runtime_context() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    });
    let program = engine
        .compile_source(
            r#"
            import pkg { writeText, appendText, readText, exists, metadata } from "std/fs";
            import pkg { join } from "std/path";
            import pkg { set, get, has } from "std/env";
            import pkg { nowMillis } from "std/time";

            function main() -> int {
                var file: str = join(left: ".", right: "hello.txt");
                writeText(path: file, content: "hello");
                appendText(path: file, content: " spar");
                if !exists(path: file) { return 1; }
                if readText(path: file) != "hello spar" { return 2; }
                var info: FileMetadata = metadata(path: file);
                if info.size != 10 { return 3; }
                set(name: "SPAR_STDLIB_TEST", value: "yes");
                if !has(name: "SPAR_STDLIB_TEST") { return 4; }
                if get(name: "SPAR_STDLIB_TEST") != "yes" { return 5; }
                if nowMillis() <= 0 { return 6; }
                return 0;
            };
            "#,
        )
        .expect("OS std modules should compile");

    let context = RuntimeContext::new(temp.path().to_path_buf());
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("OS std modules should execute");
    assert_eq!(outcome.exit_status, 0);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("hello.txt")).unwrap(),
        "hello spar"
    );
}

#[test]
fn time_module_formats_parses_and_measures_elapsed_millis() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { formatIso8601, parseIso8601, elapsedMillis } from "std/time";
            function main() -> int {
                var stamp: str = formatIso8601(millis: 0);
                if stamp != "1970-01-01T00:00:00.000Z" { return 1; }
                if parseIso8601(value: "2000-02-29T12:34:56.789Z") != 951827696789 { return 2; }
                if formatIso8601(millis: 951827696789) != "2000-02-29T12:34:56.789Z" { return 3; }
                if elapsedMillis(startMillis: 0) <= 0 { return 4; }
                return 0;
            };
            "#,
        )
        .expect("std/time basic format, parse and elapsed helpers should execute");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn time_module_rejects_invalid_iso_timestamp() {
    let errors = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { parseIso8601 } from "std/time";
            function main() -> int {
                return parseIso8601(value: "2026-02-30T00:00:00.000Z");
            };
            "#,
        )
        .expect_err("invalid calendar dates must be rejected");
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("invalid ISO-8601 UTC timestamp")));
}

#[test]
fn random_module_returns_values_in_requested_range() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { randomInt } from "std/random";
            function main() -> int {
                var value: int = randomInt(min: 10, max: 20);
                if value < 10 || value > 20 { return 1; }
                return 0;
            };
            "#,
        )
        .expect("random module should execute");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn random_choose_selects_from_non_empty_list() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { choose } from "std/random";
            function main() -> int {
                var picked: int = choose<int>(values: [11, 22, 33]);
                if picked == 11 || picked == 22 || picked == 33 { return 0; }
                return 1;
            };
            "#,
        )
        .expect("random choose should return an element from the input list");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn random_choose_rejects_empty_list() {
    let errors = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { choose } from "std/random";
            function main() -> int {
                var values: [int] = [];
                return choose<int>(values: values);
            };
            "#,
        )
        .expect_err("random choose must reject an empty list");
    assert!(errors.iter().any(|error| error
        .to_string()
        .contains("cannot choose from an empty list")));
}

#[test]
fn imported_function_shares_runtime_environment_with_caller() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("helper.spar"),
        r#"
        import pkg { set } from "std/env";
        function configure() -> void {
            set(name: "SPAR_IMPORTED_CONTEXT_TEST", value: "shared");
            return;
        };
        "#,
    )
    .unwrap();

    let engine = Engine::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    });
    let program = engine
        .compile_source(
            r#"
            import "./helper" as helper;
            import pkg { get } from "std/env";

            function main() -> int {
                helper::configure();
                if get(name: "SPAR_IMPORTED_CONTEXT_TEST") == "shared" {
                    return 0;
                }
                return 1;
            };
            "#,
        )
        .expect("imported helper should compile");

    let context = RuntimeContext::new(temp.path().to_path_buf());
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("imported function should share caller runtime context");
    assert_eq!(outcome.exit_status, 0);
}
