use std::sync::{Arc, Mutex};

use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

#[test]
fn plan3_acceptance_mixes_process_bytes_structured_values_and_back_to_bytes() {
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
                    sh -c 'printf "diagnostic\n" >&2; printf "%s\n" "{\"name\":\"Obi\"}" "{\"name\":\"Ada\"}"'
                        | from jsonl
                        |> firstOnly()
                        |> to jsonl
                        | cat;
                };
            };
            "#,
        )
        .expect("Plan 3 mixed pipeline fixture should compile");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    context.set_stderr(RuntimeOutput::Buffer(stderr.clone()));

    let outcome = engine
        .execute_compiled_with_context(&program, context)
        .expect("Plan 3 mixed pipeline fixture should execute");

    assert_eq!(outcome.exit_status, 0);
    assert_eq!(&*stdout.lock().unwrap(), b"{\"name\":\"Obi\"}\n");
    assert_eq!(&*stderr.lock().unwrap(), b"diagnostic\n");
}

#[test]
fn plan3_acceptance_process_values_preserve_stream_and_result_boundaries() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command } from "std/process";

            function main() -> int {
                var liveCommand: Command = command(
                    program: "sh",
                    args: ["-c", "printf out"]
                );
                var live: ProcessStream = liveCommand.stream();
                var first: ProcessChunk = live.next().unwrap();
                if first.source != "stdout" || first.bytes.length() != 3 { return 1; }
                if !live.next().isNone() { return 2; }
                var status: PipelineStatus = live.wait();
                if !status.success { return 3; }

                var completed: Command = command(
                    program: "sh",
                    args: ["-c", "printf out; printf err >&2"]
                );
                var result: ProcessResult = completed.run();
                if !result.success { return 4; }
                if result.stdout.length() != 3 || result.stderr.length() != 3 { return 5; }
                return 0;
            };
            "#,
        )
        .expect("Plan 3 process values should execute");

    assert_eq!(outcome.exit_status, 0);
}
