use std::sync::{Arc, Mutex};

use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

#[test]
fn prelude_println_is_available_without_import() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> int {
                println(value: "hello from prelude");
                return len(value: "spar");
            };
            "#,
        )
        .expect("prelude source should compile");

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
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
            var app: App = App();
            println(value: app.name);
            print(value: "!");
            return len(value: app.name);
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
        .check_source("function println(value: str) -> void { return; };")
        .expect_err("reserved prelude declaration must be rejected");
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("reserved Spar prelude name")));
}

#[test]
fn intrinsic_panic_name_is_reserved_by_the_prelude() {
    let engine = Engine::new(CompileOptions::default());
    let errors = engine
        .check_source("function panic(message: str) -> void { return; };")
        .expect_err("panic must remain a reserved prelude/intrinsic binding");
    assert!(errors
        .iter()
        .any(|error| error.to_string().contains("reserved Spar prelude name")));
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
                var encoded: str = stringify(value: label);
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
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
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

#[test]
fn core_builtin_length_methods_share_normal_method_syntax() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            function main() -> int {
                var text: int = "spar".length();
                var items: int = [1, 2, 3].length();
                return text + items;
            };
            "#,
        )
        .expect("str.length and List<T>.length should be registered built-in methods");
    assert_eq!(outcome.exit_status, 7);
}

#[test]
fn range_end_only_form_is_available_without_import() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> int {
                var mut total: int = 0;
                for i in range(end: 5) {
                    total = total + i;
                }
                return total;
            };
            "#,
        )
        .expect("range prelude source should compile");
    let outcome = engine
        .execute_compiled_with_context(&program, RuntimeContext::for_base_dir(program.base_dir()))
        .expect("range prelude should execute");
    assert_eq!(outcome.exit_status, 10);
}

#[test]
fn range_from_starts_at_the_given_value() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            function main() -> int {
                var mut total: int = 0;
                for i in rangeFrom(start: 2, end: 5) {
                    total = total + i;
                }
                return total;
            };
            "#,
        )
        .expect("rangeFrom(start, end) should execute");
    assert_eq!(outcome.exit_status, 9);
}

#[test]
fn range_end_bound_is_exclusive_and_empty_when_start_reaches_end() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            function main() -> int {
                return rangeFrom(start: 5, end: 5).length();
            };
            "#,
        )
        .expect("empty range should execute");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn terminal_helpers_respect_buffered_io_and_expose_controls() {
    use spar::RuntimeInput;

    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            import pkg { isStdinTty, isStdoutTty, supportsColor, prompt, cyan, clearLine,
                hideCursor, showCursor, saveCursor, restoreCursor,
                enterAlternateScreen, leaveAlternateScreen, clearToEnd, clearToLineEnd, bell, moveUp, moveDown, moveLeft, moveRight, redIfColor, readKey, width, height } from "std/terminal";
            fn main() -> int {
                if isStdinTty() || isStdoutTty() || supportsColor() { return 1; }
                if !readKey(timeoutMs: 0).isNone() { return 8; }
                if width() != 111 || height() != 37 { return 9; }
                if prompt(message: "Name: ").unwrap() != "Ada" { return 2; }
                print(value: cyan(text: "hi"));
                print(value: clearLine());
                print(value: hideCursor());
                print(value: showCursor());
                print(value: saveCursor());
                print(value: restoreCursor());
                print(value: enterAlternateScreen());
                print(value: leaveAlternateScreen());
                print(value: clearToEnd());
                print(value: clearToLineEnd());
                print(value: bell());
                print(value: moveUp(count: 2));
                print(value: moveDown(count: 3));
                print(value: moveLeft(count: 4));
                print(value: moveRight(count: 5));
                print(value: redIfColor(text: "plain"));
                return 0;
            };
            "#,
        )
        .expect("terminal module should compile");
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdin(RuntimeInput::from_bytes(b"Ada\n".to_vec()));
    context.env_set("COLUMNS", "111");
    context.env_set("LINES", "37");
    context.set_stdout(RuntimeOutput::Buffer(output.clone()));
    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("terminal helpers should execute");
    assert_eq!(outcome.exit_status, 0);
    assert_eq!(&*output.lock().unwrap(), b"Name: \x1b[36mhi\x1b[0m\x1b[2K\r\x1b[?25l\x1b[?25h\x1b7\x1b8\x1b[?1049h\x1b[?1049l\x1b[0J\x1b[0K\x07\x1b[2A\x1b[3B\x1b[4D\x1b[5Cplain");
}
