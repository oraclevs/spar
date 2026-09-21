use spar::{CompileOptions, Engine};

#[test]
fn command_run_returns_process_result_without_breaking_free_run() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command, run } from "std/process";

            function main() -> int {
                var cmd: Command = command(program: "sh", args: ["-c", "printf hello; printf err >&2"]);
                var modern: ProcessResult = cmd.run();
                var legacy: ProcessResult = run(program: "sh", args: ["-c", "printf hello; printf err >&2"]);

                if !modern.success || modern.exitCode != 0 { return 1; }
                if modern.stdout.length() != 5 || modern.stderr.length() != 3 { return 2; }
                if legacy.stdout.length() != modern.stdout.length() { return 3; }
                if legacy.stderr.length() != modern.stderr.length() { return 4; }
                return 0;
            };
            "#,
        )
        .expect("Command.run and legacy process.run should both execute");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn process_stream_exposes_tagged_stdout_chunks_and_collects_remaining_output() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command } from "std/process";

            function main() -> int {
                var cmd: Command = command(program: "sh", args: ["-c", "printf hello"]);
                var live: ProcessStream = cmd.stream();
                if live.pid() <= 0 { return 1; }

                var first: Option<ProcessChunk> = live.next();
                if first.isNone() { return 2; }
                var chunk: ProcessChunk = first.unwrap();
                if chunk.source != "stdout" { return 3; }
                if chunk.bytes.length() != 5 { return 4; }

                var done: Option<ProcessChunk> = live.next();
                if !done.isNone() { return 5; }

                var status: PipelineStatus = live.wait();
                if !status.success || status.code != 0 { return 6; }
                return 0;
            };
            "#,
        )
        .expect("ProcessStream.next should expose tagged byte chunks");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn process_stream_keeps_stderr_separate_and_collect_can_capture_all_output() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command, stream } from "std/process";

            function main() -> int {
                var errOnly: ProcessStream = command(
                    program: "sh",
                    args: ["-c", "printf boom >&2"]
                ).stream();
                var event: ProcessChunk = errOnly.next().unwrap();
                if event.source != "stderr" || event.bytes.length() != 4 { return 1; }
                if !errOnly.next().isNone() { return 2; }
                var errStatus: PipelineStatus = errOnly.wait();
                if !errStatus.success { return 3; }

                var collected: ProcessResult = stream(
                    program: "sh",
                    args: ["-c", "printf out; printf err >&2"]
                ).collect();
                if collected.stdout.length() != 3 { return 4; }
                if collected.stderr.length() != 3 { return 5; }
                if collected.status.processes.length() != 1 { return 6; }
                return 0;
            };
            "#,
        )
        .expect("ProcessStream must keep stdout and stderr distinct");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn process_stream_cancel_remains_a_valid_collectable_resource() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command } from "std/process";

            function main() -> int {
                var live: ProcessStream = command(
                    program: "sh",
                    args: ["-c", "sleep 30"]
                ).stream();
                live.cancel();
                var result: ProcessResult = live.collect();
                if result.success { return 1; }
                return 0;
            };
            "#,
        )
        .expect("cancelled ProcessStream should remain collectable for final status");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn collect_rejects_a_stream_after_incremental_chunks_have_been_consumed() {
    let errors = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { command } from "std/process";

            function main() -> int {
                var live: ProcessStream = command(program: "sh", args: ["-c", "printf hello"]).stream();
                live.next();
                var result: ProcessResult = live.collect();
                return result.exitCode;
            };
            "#,
        )
        .expect_err("collect after next must not silently return a partial ProcessResult");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("cannot collect a complete ProcessResult after consuming stream chunks")
    );
}

#[test]
fn command_and_process_stream_methods_typecheck_as_first_class_runtime_values() {
    Engine::new(CompileOptions::default())
        .check_source(
            r#"
            import pkg { command } from "std/process";

            function consume(commandValue: Command) -> ProcessResult {
                return commandValue.run();
            };

            function streamOne(commandValue: Command) -> Option<ProcessChunk> {
                var live: ProcessStream = commandValue.stream();
                var chunk: Option<ProcessChunk> = live.next();
                live.wait();
                return chunk;
            };
            "#,
        )
        .expect("Command and ProcessStream method signatures should be type visible");
}
