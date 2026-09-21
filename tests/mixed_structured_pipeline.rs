use std::sync::{Arc, Mutex};

use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

#[test]
fn mixed_pipeline_crosses_bytes_values_and_bytes_without_a_foreign_shell() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            import pkg { take } from "std/data";

            function firstOnly(rows: Stream<Record>) -> Stream<Record> {
                return rows |> take(1);
            };

            function main() -> shell {
                return shell {
                    printf '%s\n' '{"name":"Obi"}' '{"name":"Ada"}'
                        | from jsonl
                        |> firstOnly()
                        |> to jsonl
                        | cat;
                };
            };
            "#,
        )
        .expect("mixed pipeline should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    context.set_stderr(RuntimeOutput::Buffer(stderr.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("mixed pipeline should execute");

    assert_eq!(outcome.exit_status, 0);
    assert_eq!(&*stdout.lock().unwrap(), b"{\"name\":\"Obi\"}\n");
    assert!(stderr.lock().unwrap().is_empty());
}

#[test]
fn mixed_pipeline_keeps_stderr_out_of_structured_decoder_input() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> shell {
                return shell {
                    sh -c 'printf "%s\n" "{\"id\":1}"; printf "diagnostic\n" >&2'
                        | from jsonl
                        |> to jsonl
                        | cat;
                };
            };
            "#,
        )
        .expect("mixed stderr fixture should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    context.set_stderr(RuntimeOutput::Buffer(stderr.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("mixed stderr fixture should execute");

    assert_eq!(outcome.exit_status, 0);
    assert_eq!(&*stdout.lock().unwrap(), b"{\"id\":1}\n");
    assert_eq!(&*stderr.lock().unwrap(), b"diagnostic\n");
}

#[test]
fn mixed_pipeline_scoc_env_decodes_command_output() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> shell {
                return shell {
                    printf '%s\n' 'USER=obi' 'SHELL=/bin/sparsh'
                        | from env
                        |> to jsonl
                        | cat;
                };
            };
            "#,
        )
        .expect("SCOC env pipeline should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    context.set_stderr(RuntimeOutput::Buffer(stderr.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("SCOC env pipeline should execute");

    assert_eq!(outcome.exit_status, 0);
    assert_eq!(
        &*stdout.lock().unwrap(),
        b"{\"name\":\"USER\",\"value\":\"obi\"}\n{\"name\":\"SHELL\",\"value\":\"/bin/sparsh\"}\n"
    );
    assert!(stderr.lock().unwrap().is_empty());
}

#[test]
fn mixed_pipeline_scoc_env_raw_option_preserves_object_shape() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            function main() -> shell {
                return shell {
                    printf '%s\n' 'USER=obi' 'SHELL=/bin/sparsh'
                        | from env(raw: true)
                        |> to json
                        | cat;
                };
            };
            "#,
        )
        .expect("SCOC raw env pipeline should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("SCOC raw env pipeline should execute");

    assert_eq!(outcome.exit_status, 0);
    assert_eq!(
        &*stdout.lock().unwrap(),
        b"{\"USER\":\"obi\",\"SHELL\":\"/bin/sparsh\"}\n"
    );
}

#[test]
fn mixed_pipeline_scoc_ping_auto_streams_live_output() {
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            import pkg { take } from "std/data";

            function main() -> shell {
                return shell {
                    printf '%s\n' \
                        'PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.' \
                        '64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=10.0 ms' \
                        '64 bytes from 1.1.1.1: icmp_seq=2 ttl=57 time=11.0 ms'
                        | from ping
                        |> take(1)
                        |> to jsonl
                        | cat;
                };
            };
            "#,
        )
        .expect("SCOC streaming ping pipeline should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("SCOC streaming ping pipeline should execute");

    assert_eq!(outcome.exit_status, 0);
    let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
    assert!(output.contains("\"type\":\"reply\""), "got: {output}");
    assert!(output.contains("\"icmp_seq\":1"), "got: {output}");
    assert!(!output.contains("\"icmp_seq\":2"), "got: {output}");
}
