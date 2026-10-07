use crate::runtime::execute_self_contained_entry;
use crate::{Engine, Value};

fn execute(source: &str) -> Result<Value, Vec<crate::SparError>> {
    let program = Engine::default().compile_source(source)?;
    execute_self_contained_entry(&program)
}

#[test]
fn shell_result_function_executes_body_and_returns_typed_result() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("ran.txt");
    let source = format!(r#"
        fn mark() -> ShellResult<str, str> {{
            echo ran > "{}";
            return ok(value: "payload");
        }};
        fn main() -> int {{
            var result: ShellResult<str, str> = mark();
            return 0;
        }};
    "#, marker.display());
    assert_eq!(Engine::default().execute_source(&source).unwrap().exit_status, 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran\n");
}

#[test]
fn shell_result_exposes_result_methods() {
    let source = r#"
        fn pass() -> ShellResult<int, str> { return ok(value: 7); };
        fn fail() -> ShellResult<int, str> { return err(error: "no"); };
        fn main() -> int {
            var success: ShellResult<int, str> = pass();
            var failure: ShellResult<int, str> = fail();
            if !success.isOk() || success.unwrap() != 7 { return 1; }
            if !failure.isErr() || failure.unwrapErr() != "no" { return 2; }
            return 0;
        };
    "#;
    assert_eq!(Engine::default().execute_source(source).unwrap().exit_status, 0);
}

#[test]
fn async_call_is_scheduled_once_and_await_returns_value() {
    let value = execute(
        r#"
        async function value() -> int { return 21; };
        async function main() -> int {
            var pending: Promise<int> = value();
            var first: int = await pending;
            var second: int = await pending;
            return first + second;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(42));
}

#[test]
fn async_dependency_chain_completes() {
    let value = execute(
        r#"
        async function leaf(value: int) -> int { return value + 1; };
        async function branch(value: int) -> int { return await leaf(value: value) + 1; };
        async function main() -> int {
            var left: Promise<int> = branch(value: 10);
            var right: Promise<int> = branch(value: 20);
            return await left + await right;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(34));
}

#[test]
fn awaited_failure_is_caught_by_existing_error_model() {
    let value = execute(
        r#"
        async function fail() -> int { return 1 / 0; };
        async function main() -> int {
            try {
                var ignored: int = await fail();
                return ignored;
            } catch error {
                if error.kind == "runtime" { return 7; }
                return 8;
            }
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
fn unawaited_failure_does_not_replace_main_result() {
    let value = execute(
        r#"
        async function fail() -> int { return 1 / 0; };
        async function main() -> int {
            var pending: Promise<int> = fail();
            return 9;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(9));
}

#[test]
fn panic_in_async_task_bypasses_try_catch() {
    let errors = execute(
        r#"
        async function broken() -> int { panic(message: "stop"); };
        async function main() -> int {
            try {
                return await broken();
            } catch error {
                return 1;
            }
        };
        "#,
    )
    .unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("stop")));
}

#[test]
fn panic_in_sync_function_bypasses_try_catch() {
    let errors = execute(
        r#"
        function main() -> int {
            try {
                panic(message: "fatal-stop");
            } catch error {
                return 1;
            }
        };
        "#,
    )
    .unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("fatal-stop")));
}

#[test]
fn unawaited_async_panic_aborts_the_runtime() {
    let errors = execute(
        r#"
        async function broken() -> int { panic(message: "detached-stop"); };
        async function main() -> int {
            var pending: Promise<int> = broken();
            return 0;
        };
        "#,
    )
    .unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("detached-stop")));
}

#[test]
fn compiled_function_uses_slots_across_nested_control_flow() {
    let value = execute("function main() -> int { var mut total: int = 0; for (index, value) in [2, 4, 6] { if index == 1 { continue; } total = total + value; } return total; };").unwrap();
    assert_eq!(value, Value::Int(8));
}

#[test]
fn compiled_functions_support_recursion_defaults_and_named_arguments() {
    let value = execute("function sum(value: int, carry: int = 0) -> int { if value == 0 { return carry; } return sum(carry: carry + value, value: value - 1); }; function main() -> int { return sum(value: 4); };").unwrap();
    assert_eq!(value, Value::Int(10));
}

#[test]
fn compiled_expressions_build_lists_structs_interpolation_and_comprehensions() {
    let value = execute("struct ResultValue { label: str; }; function main() -> int { var values: [int] = for value in [1, 2, 3] { value }; var object: ResultValue = ResultValue(label: \"sum-${values[0] + values[2]}\"); if object.label == \"sum-4\" { return 0; } return 1; };").unwrap();
    assert_eq!(value, Value::Int(0));
}

#[test]
fn compiled_integer_division_by_zero_is_an_error() {
    let errors = execute("function main() -> int { return 1 / 0; };").unwrap_err();
    assert!(errors[0].to_string().contains("division by zero"));
}

#[test]
fn compiled_try_catch_exposes_error_and_supports_ignored_binding() {
    let value = execute(
        r#"
        function main() -> int {
            try { var broken: int = 1 / 0; }
            catch err {
                if err.kind == "runtime" { return 7; }
                return 1;
            }
            try { var broken: int = 1 / 0; } catch { return 9; }
            return 0;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
fn compiled_try_catch_without_binding_executes_handler() {
    let value = execute(
        "function main() -> int { try { var broken: int = 1 / 0; } catch { return 9; } return 0; };",
    )
    .unwrap();
    assert_eq!(value, Value::Int(9));
}

#[test]
fn compiled_generic_functions_are_erased_and_reusable() {
    let value = execute(
        r#"
        function identity<T>(value: T) -> T { return value; };
        function main() -> int {
            var number: int = identity(value: 7);
            var word: str = identity<str>(value: "spar");
            if word == "spar" { return number; }
            return 0;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
fn compiled_generic_named_types_substitute_nested_fields() {
    let value = execute(
        r#"
        struct Box<T> { value: T; };
        function unbox<T>(box: Box<T>) -> T { return box.value; };
        function main() -> int {
            var boxed: Box<int> = Box<int>(value: 9);
            return unbox(box: boxed);
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(9));
}

#[test]
fn compiled_generic_function_cannot_construct_applied_return_type_from_anonymous_object() {
    let errors = execute(
        r#"
        struct Box<T> { value: T; };
        function box<T>(value: T) -> Box<T> { return { value: value; }; };
        function main() -> int { return 0; };
        "#,
    )
    .expect_err("anonymous object literals must not construct named generic types");
    let rendered = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("Box") || rendered.contains("Record") || rendered.contains("object"), "{rendered}");
}

#[test]
fn compiled_function_group_member_can_be_generic() {
    let value = execute(
        r#"
        functionGroup Values {
            function identity<T>(value: T) -> T { return value; }
        };
        function main() -> int { return Values::identity(value: 17); };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(17));
}

#[test]
fn compiled_generic_calls_specialize_callers_and_recurse_erased() {
    let value = execute(
        r#"
        function identity<T>(value: T) -> T { return value; };
        function repeat<T>(value: T, count: int) -> T {
            if count == 0 { return value; }
            return repeat(value: value, count: count - 1);
        };
        function main() -> int {
            return identity(value: 4) + repeat(value: 5, count: 2);
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, Value::Int(9));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn compiled_recursion_supports_one_thousand_calls() {
    let value = execute("function count(n: int) -> int { if n == 0 { return 0; } return 1 + count(n: n - 1); }; function main() -> int { return count(n: 998); };").unwrap();
    assert_eq!(value, Value::Int(998));
}

#[test]
fn compiled_recursion_limit_is_a_catchable_error() {
    let value = execute("function forever() -> int { return forever(); }; function main() -> int { try { return forever(); } catch error { return 7; } };").unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn compiled_fibonacci_exceeds_the_old_depth_limit() {
    let value = execute("function fib(n: int) -> int { if n <= 1 { return n; } return fib(n: n - 1) + fib(n: n - 2); }; function main() -> int { return fib(n: 21); };").unwrap();
    assert_eq!(value, Value::Int(10946));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn compiled_closure_recursion_supports_one_thousand_calls() {
    let value = execute("function count(n: int) -> int { if n == 0 { return 0; } var next: fn() -> int = || count(n: n - 1); return 1 + next(); }; function main() -> int { return count(n: 499); };").unwrap();
    assert_eq!(value, Value::Int(499));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn compiled_async_recursion_supports_one_thousand_calls() {
    let value = execute("async function count(n: int) -> int { if n == 0 { return 0; } return 1 + await count(n: n - 1); }; async function main() -> int { return await count(n: 998); };").unwrap();
    assert_eq!(value, Value::Int(998));
}

#[test]
fn compiled_async_recursion_limit_returns_an_error() {
    let errors = execute("async function forever() -> int { return await forever(); }; async function main() -> int { return await forever(); };").unwrap_err();
    assert!(errors.iter().any(|error| error.to_string().contains("maximum function call depth")));
}

#[test]
fn compiled_arithmetic_stops_after_the_left_operand_fails() {
    let value = execute(
        r#"
        function left() -> int { return 1 / 0; };
        function right() -> int { panic(message: "right operand must not run"); };
        function main() -> int {
            try { return left() + right(); }
            catch error { return 7; }
        };
        "#,
    ).unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
fn tilde_statement_runs_in_a_plain_function() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("ran.txt");
    let source = format!(r#"
        function main() -> int {{
            ~ echo ran > "{}";
            return 0;
        }};
    "#, marker.display());
    assert_eq!(Engine::default().execute_source(&source).unwrap().exit_status, 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran\n");
}

#[test]
fn tilde_statement_failure_does_not_abort_the_function() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("after.txt");
    let source = format!(r#"
        function main() -> int {{
            ~ false;
            ~ echo after > "{}";
            return 0;
        }};
    "#, marker.display());
    assert_eq!(Engine::default().execute_source(&source).unwrap().exit_status, 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "after\n");
}

#[test]
fn tilde_statement_runs_inside_shell_result_function() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("sr.txt");
    let source = format!(r#"
        function go() -> ShellResult<int, str> {{
            ~ echo sr > "{}";
            return ok(value: 0);
        }};
        function main() -> int {{
            var r = go();
            return 0;
        }};
    "#, marker.display());
    assert_eq!(Engine::default().execute_source(&source).unwrap().exit_status, 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "sr\n");
}

#[test]
fn tilde_pipeline_statement_runs() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("pipe.txt");
    let source = format!(r#"
        function main() -> int {{
            ~ echo piped | cat > "{}";
            return 0;
        }};
    "#, marker.display());
    assert_eq!(Engine::default().execute_source(&source).unwrap().exit_status, 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "piped\n");
}

#[test]
fn shell_result_body_runs_a_program_named_command() {
    // `command` is no longer a keyword: in a ShellResult body it is just a
    // program word, resolved on PATH like any other. Put a script of that
    // name first on PATH and check it is the thing that ran.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("spar_command_prog_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("command");
    std::fs::write(&program, "#!/bin/sh\necho \"ran:$1\"\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = dir.join("out.txt");
    let source = format!(
        r#"
        function probe() -> ShellResult<int, str> {{
            PATH={dir}:/usr/bin:/bin command -v > "{out}";
            return ok(value: 0);
        }};
        function main() -> int {{
            probe();
            return 0;
        }};
    "#,
        dir = dir.display(),
        out = out.display()
    );
    let result = Engine::default().execute_source(&source);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "ran:-v\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn marker_statement_runs_a_command_group() {
    let out = std::env::temp_dir().join(format!("spar_marker_group_{}", std::process::id()));
    let source = format!(
        r#"
        function main() -> int {{
            ~ echo a > "{p}" && echo b >> "{p}";
            return 0;
        }};
    "#,
        p = out.display()
    );
    let result = Engine::default().execute_source(&source);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "a\nb\n");
    let _ = std::fs::remove_file(&out);
}
