use spar::{CompileOptions, Engine};

#[cfg(unix)]
#[test]
fn process_run_returns_typed_status_and_bytes() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { run } from "std/process";
            function main() -> int {
                var result: ProcessResult = run(program: "/bin/sh", args: ["-c", "printf hello; printf err >&2; exit 7"]);
                if result.exitCode != 7 { return 1; }
                if result.success { return 2; }
                if result.stdout[0] != 104 { return 3; }
                if result.stderr[0] != 101 { return 4; }
                return 0;
            };
            "#,
        )
        .expect("process std module should execute");
    assert_eq!(outcome.exit_status, 0);
}

#[cfg(unix)]
#[test]
fn process_spawn_returns_opaque_runtime_handle_that_can_be_waited() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { spawn, pid, wait } from "std/process";
            function main() -> int {
                var child: Process = spawn(program: "/bin/sh", args: ["-c", "exit 6"]);
                if pid(process: child) <= 0 { return 1; }
                var status: ProcessStatus = wait(process: child);
                if status.code != 6 { return 2; }
                if status.success { return 3; }
                return 0;
            };
            "#,
        )
        .expect("spawned process should use a runtime-owned opaque resource");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn process_exit_requests_runtime_exit_without_terminating_embedder() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { exit } from "std/process";
            function main() -> int {
                exit(code: 23);
                return 99;
            };
            "#,
        )
        .expect("std/process exit should stop only the Spar runtime");
    assert_eq!(outcome.exit_status, 23);
}
