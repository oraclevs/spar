use std::sync::{Arc, Mutex};

use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

#[test]
fn prelude_println_is_available_without_import() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> int {
                println(message: "hello from prelude");
                return len(value: "spar");
            };
            "#,
        )
        .expect("prelude source should compile");

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(&program.options.base_dir);
    context.set_stdout(RuntimeOutput::Buffer(buffer.clone()));
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("prelude should execute through std native providers");

    assert_eq!(outcome.exit_status, 4);
    assert_eq!(&*buffer.lock().unwrap(), b"hello from prelude\n");
}

#[test]
fn check_mode_resolves_implicit_prelude_native_capabilities() {
    let compilation = spar::Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::default()
    })
    .compile(
        r#"
        struct App {
            name: str = "Spar";
        };

        function main() -> int {
            println(message: App.name);
            print(message: "!");
            return len(value: App.name);
        };
        "#,
    );

    assert!(
        compilation.errors.is_empty(),
        "check mode must resolve the prelude's trusted nativeIo/nativeCore calls: {:#?}",
        compilation.errors
    );
}

#[test]
fn explicit_std_root_import_can_replace_implicit_binding() {
    let engine = Engine::new(CompileOptions::default());
    let outcome = engine
        .execute_source(
            r#"
            import pkg { len } from "std";
            function main() -> int { return len(value: [1, 2, 3]); };
            "#,
        )
        .expect("canonical explicit std prelude import should compile");
    assert_eq!(outcome.exit_status, 3);
}

#[test]
fn reserved_prelude_name_cannot_be_redeclared() {
    let engine = Engine::new(CompileOptions::default());
    let errors = engine
        .check_source("function println(message: str) -> void { return; };")
        .expect_err("reserved prelude declaration must be rejected");
    assert!(errors.iter().any(|error| error.to_string().contains("reserved Spar prelude name")));
}

#[test]
fn intrinsic_panic_name_is_reserved_by_the_prelude() {
    let engine = Engine::new(CompileOptions::default());
    let errors = engine
        .check_source("function panic(message: str) -> void { return; };")
        .expect_err("panic must remain a reserved prelude/intrinsic binding");
    assert!(errors.iter().any(|error| error.to_string().contains("reserved Spar prelude name")));
}

#[test]
fn text_math_json_and_regex_modules_execute() {
    let engine = Engine::new(CompileOptions::default());
    let outcome = engine
        .execute_source(
            r#"
            import pkg { upper, contains } from "std/text";
            import pkg { absInt } from "std/math";
            import pkg { stringify } from "std/json";
            import pkg { isMatch } from "std/regex";

            function main() -> int {
                var label: str = upper(value: "spar");
                if !contains(value: label, needle: "PAR") { return 1; }
                if absInt(value: -9) != 9 { return 2; }
                var encoded: str = stringify<str>(value: label);
                if encoded != "\"SPAR\"" { return 3; }
                if !isMatch(pattern: "^SP", text: label) { return 4; }
                return 0;
            };
            "#,
        )
        .expect("core std modules should compile and execute");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn io_supports_blank_println_and_byte_oriented_runtime_io() {
    use spar::RuntimeInput;

    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            import pkg { readBytes, writeBytes } from "std/io";
            function main() -> int {
                println();
                var input: Bytes = readBytes();
                writeBytes(content: input);
                return input[0];
            };
            "#,
        )
        .expect("std/io byte APIs should compile");

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(&program.options.base_dir);
    context.set_stdin(RuntimeInput::from_bytes(b"A".to_vec()));
    context.set_stdout(RuntimeOutput::Buffer(buffer.clone()));
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("std/io byte APIs should execute");
    assert_eq!(outcome.exit_status, 65);
    assert_eq!(&*buffer.lock().unwrap(), b"\nA");
}
#[test]
fn stdlib_smoke_fixture_runs_through_normal_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let source = include_str!("fixtures/stdlib_smoke/main.spar");
    let engine = Engine::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    });
    let program = engine
        .compile_source(source)
        .expect("stdlib smoke fixture should compile");
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::new(temp.path().to_path_buf());
    context.set_stdout(RuntimeOutput::Buffer(buffer.clone()));
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("stdlib smoke fixture should execute without shelling out");
    assert_eq!(outcome.exit_status, 0);
    assert_eq!(&*buffer.lock().unwrap(), b"Hello from Spar\n");
    assert!(!temp.path().join("spar-stdlib-smoke.txt").exists());
}

