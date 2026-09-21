//! Acceptance sweep for roadmap Tasks 14-18: structured codecs, the mixed
//! `|` / `|>` pipeline, laziness against infinite producers, and status/stderr
//! handling. Programs return `shell` and the harness captures stdout/stderr.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

struct Run {
    stdout: String,
    stderr: String,
    status: i32,
}

fn run_shell(source: &str) -> Run {
    let source = source.to_string();
    let (sender, receiver) = mpsc::channel();
    // A hang here means an infinite producer was not cancelled.
    std::thread::spawn(move || {
        let engine = Engine::new(CompileOptions::default());
        let program = engine
            .compile_source(&source)
            .unwrap_or_else(|errors| panic!("should compile: {errors:?}\n{source}"));
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut context = RuntimeContext::for_base_dir(program.base_dir());
        context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
        context.set_stderr(RuntimeOutput::Buffer(stderr.clone()));
        let outcome = engine
            .execute_compiled_with_context(&program, context)
            .unwrap_or_else(|errors| panic!("should run: {errors:?}\n{source}"));
        let run = Run {
            stdout: String::from_utf8_lossy(&stdout.lock().unwrap()).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.lock().unwrap()).into_owned(),
            status: outcome.exit_status,
        };
        let _ = sender.send(run);
    });
    receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("pipeline did not finish: an infinite producer was not cancelled")
}

fn compile_error(source: &str) -> String {
    match Engine::new(CompileOptions::default()).compile_source(source) {
        Ok(_) => panic!("expected a compile error for:\n{source}"),
        Err(errors) => format!("{errors:?}"),
    }
}

const DATA: &str =
    r#"import pkg { where, select, take, map, collectTable, sortBy, count } from "std/data";"#;

// ── the canonical example from the design ───────────────────────────────────

#[test]
fn canonical_docker_style_pipeline_filters_projects_and_reserializes() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf '%s\n' \
                    '{{"Names":"web","Image":"nginx","Status":"Up","State":"running"}}' \
                    '{{"Names":"db","Image":"pg","Status":"Exited","State":"exited"}}' \
                    '{{"Names":"cache","Image":"redis","Status":"Up","State":"running"}}'
                    | from jsonl
                    |> where(fn(container) => container.State == "running")
                    |> select(["Names", "Image", "Status"])
                    |> to jsonl
                    | cat;
            }};
        }};
        "#
    ));
    assert_eq!(run.status, 0, "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{}", run.stdout);
    assert!(lines[0].contains("\"Names\":\"web\""), "{}", lines[0]);
    assert!(lines[1].contains("\"Names\":\"cache\""), "{}", lines[1]);
    assert!(!run.stdout.contains("exited"), "{}", run.stdout);
    assert!(!run.stdout.contains("\"State\""), "{}", run.stdout);
}

#[test]
fn structured_output_can_be_redirected_to_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("out.jsonl");
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf '%s\n' '{{"n":1}}' '{{"n":2}}' '{{"n":3}}'
                    | from jsonl
                    |> take(2)
                    |> to jsonl
                    | cat > "{}";
            }};
        }};
        "#,
        path.display()
    ));
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"n\":1}\n{\"n\":2}\n"
    );
}

// ── codecs ──────────────────────────────────────────────────────────────────

#[test]
fn lines_codec_maps_text_lines() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf 'a\nb\nc\n'
                    | from lines
                    |> map(fn(line: str) -> str => line + "!")
                    |> to lines
                    | cat;
            }};
        }};
        "#
    ));
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "a!\nb!\nc!\n");
}

#[test]
fn csv_and_tsv_decode_to_records_and_encode_back() {
    for (format, input, expected_header) in [
        // Record fields are unordered, so columns come out sorted by name.
        ("csv", "name,age\\nObi,24\\nAda,31\\n", "age,name"),
        ("tsv", "name\\tage\\nObi\\t24\\nAda\\t31\\n", "age\tname"),
    ] {
        let run = run_shell(&format!(
            r#"{DATA}
            function main() -> shell {{
                return shell {{
                    printf '{input}'
                        | from {format}
                        |> take(2)
                        |> to {format}
                        | cat;
                }};
            }};
            "#
        ));
        assert_eq!(run.status, 0, "{format}: {}", run.stderr);
        let mut lines = run.stdout.lines();
        assert_eq!(
            lines.next(),
            Some(expected_header),
            "{format}: {}",
            run.stdout
        );
        assert_eq!(lines.count(), 2, "{format}: {}", run.stdout);
        assert!(
            run.stdout.contains("Obi") && run.stdout.contains("Ada"),
            "{}",
            run.stdout
        );
    }
}

#[test]
fn json_yaml_toml_documents_decode_and_encode() {
    for (format, document) in [
        ("json", r#"{"name":"Ada","age":31}"#),
        ("yaml", "name: Ada\\nage: 31\\n"),
        ("toml", "name = \"Ada\"\\nage = 31\\n"),
    ] {
        let run = run_shell(&format!(
            r#"{DATA}
            function main() -> shell {{
                return shell {{
                    printf '{document}'
                        | from {format}
                        |> to {format}
                        | cat;
                }};
            }};
            "#
        ));
        assert_eq!(run.status, 0, "{format}: {}", run.stderr);
        assert!(run.stdout.contains("Ada"), "{format}: {}", run.stdout);
        assert!(run.stdout.contains("31"), "{format}: {}", run.stdout);
    }
}

#[test]
fn malformed_input_reports_an_error_instead_of_silently_dropping_rows() {
    let source = format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf '%s\n' '{{"ok":1}}' 'this is not json'
                    | from jsonl
                    |> to jsonl
                    | cat;
            }};
        }};
        "#
    );
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let engine = Engine::new(CompileOptions::default());
        let program = engine.compile_source(&source).unwrap();
        let mut context = RuntimeContext::for_base_dir(program.base_dir());
        context.set_stdout(RuntimeOutput::Buffer(Arc::new(Mutex::new(Vec::new()))));
        context.set_stderr(RuntimeOutput::Buffer(Arc::new(Mutex::new(Vec::new()))));
        let result = engine.execute_compiled_with_context(&program, context);
        let _ =
            sender.send(result.is_err() || result.is_ok_and(|outcome| outcome.exit_status != 0));
    });
    assert!(
        receiver.recv_timeout(Duration::from_secs(20)).unwrap(),
        "a malformed JSONL row must fail the pipeline"
    );
}

// ── laziness, backpressure, cancellation ────────────────────────────────────

#[test]
fn take_stops_an_infinite_producer() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                yes | from lines |> take(3) |> to lines | cat;
            }};
        }};
        "#
    ));
    assert_eq!(run.stdout, "y\ny\ny\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
}

#[test]
fn a_downstream_that_stops_early_does_not_hang_the_producer() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                yes | from lines |> to lines | head -n 2;
            }};
        }};
        "#
    ));
    assert_eq!(run.stdout, "y\ny\n");
}

#[test]
fn transforms_are_lazy_so_map_over_infinite_input_is_fine_with_take() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                yes abc
                    | from lines
                    |> map(fn(line: str) -> str => line + line)
                    |> take(2)
                    |> to lines
                    | cat;
            }};
        }};
        "#
    ));
    assert_eq!(run.stdout, "abcabc\nabcabc\n");
}

// ── stderr and status ───────────────────────────────────────────────────────

#[test]
fn upstream_stderr_stays_on_stderr() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                sh -c 'printf "diagnostic\n" >&2; printf "%s\n" "{{\"a\":1}}"'
                    | from jsonl
                    |> to jsonl
                    | cat;
            }};
        }};
        "#
    ));
    assert_eq!(run.stdout, "{\"a\":1}\n");
    assert_eq!(run.stderr, "diagnostic\n");
    assert_eq!(run.status, 0);
}

#[test]
fn a_failing_upstream_command_fails_the_pipeline() {
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                sh -c 'printf "%s\n" "{{\"a\":1}}"; exit 3'
                    | from jsonl
                    |> to jsonl
                    | cat;
            }};
        }};
        "#
    ));
    assert_ne!(
        run.status, 0,
        "stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
}

// ── boundaries: nothing is implicit ─────────────────────────────────────────

#[test]
fn plain_unix_pipes_stay_byte_pipes() {
    // A returned shell value runs with inherited stdio, so capture via a file.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sorted.txt");
    let run = run_shell(&format!(
        r#"
        function main() -> shell {{
            return shell {{
                printf 'b\na\nc\n' | sort | head -n 2 > "{}";
            }};
        }};
        "#,
        path.display()
    ));
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
}

#[test]
fn structured_stage_directly_after_bytes_without_from_is_rejected() {
    let message = compile_error(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf 'x' |> take(1) | cat;
            }};
        }};
        "#
    ));
    assert!(!message.is_empty());
}

#[test]
fn structured_values_are_not_implicitly_serialized_into_unix_commands() {
    let message = compile_error(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf '%s\n' '{{"a":1}}' | from jsonl |> take(1) | cat;
            }};
        }};
        "#
    ));
    assert!(
        !message.is_empty(),
        "a `to` step is required before a Unix command"
    );
}

// ── Task 18: process values ─────────────────────────────────────────────────

#[test]
fn run_reports_exit_code_stdout_and_success() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            import pkg { run } from "std/process";
            function main() -> int {
                var ok: ProcessResult = run(program: "sh", args: ["-c", "printf hi"]);
                var bad: ProcessResult = run(program: "sh", args: ["-c", "printf oops >&2; exit 3"]);
                if !ok.success { return 1; }
                if ok.stdout.length() != 2 { return 2; }
                if bad.success { return 3; }
                if bad.stderr.length() != 4 { return 4; }
                return bad.exitCode;
            };
            "#,
        )
        .expect("process API should run");
    assert_eq!(outcome.exit_status, 3);
}

// ── `to FORMAT > file` and every codec ──────────────────────────────────────

#[test]
fn to_can_redirect_straight_to_a_file_for_every_format() {
    for (format, input) in [
        ("json", r#"{"name":"Ada","age":31}"#),
        ("jsonl", "{\"name\":\"Ada\",\"age\":31}\\n"),
        ("csv", "name,age\\nAda,31\\n"),
        ("tsv", "name\\tage\\nAda\\t31\\n"),
        ("yaml", "name: Ada\\nage: 31\\n"),
        ("toml", "name = \"Ada\"\\nage = 31\\n"),
        ("lines", "Ada\\nBob\\n"),
        ("text", "Ada and Bob\\n"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("out.{format}"));
        let run = run_shell(&format!(
            r#"{DATA}
            function main() -> shell {{
                return shell {{
                    printf '{input}' | from {format} |> to {format} > "{}";
                }};
            }};
            "#,
            path.display()
        ));
        assert_eq!(run.status, 0, "{format}: {}", run.stderr);
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(written.contains("Ada"), "{format}: wrote {written:?}");
    }
}

#[test]
fn to_append_redirect_adds_to_an_existing_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log.jsonl");
    std::fs::write(&path, "{\"first\":true}\n").unwrap();
    let run = run_shell(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf '%s\n' '{{"second":true}}' | from jsonl |> to jsonl >> "{}";
            }};
        }};
        "#,
        path.display()
    ));
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"first\":true}\n{\"second\":true}\n"
    );
}

#[test]
fn to_cannot_both_redirect_and_pipe() {
    let message = compile_error(&format!(
        r#"{DATA}
        function main() -> shell {{
            return shell {{
                printf 'a\n' | from lines |> to lines > "out.txt" | cat;
            }};
        }};
        "#
    ));
    assert!(!message.is_empty());
}

// ── `exec shell` interpolates like every other shell form ───────────────────

#[test]
fn exec_shell_interpolates_arguments_and_redirect_targets() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().display().to_string();
    let outcome = Engine::default()
        .execute_source(&format!(
            r#"
            function main() -> int {{
                var dir: str = "{base}";
                var name: str = "made";
                var word: str = "interpolated";
                exec shell {{ echo "value ${{word}}" > "${{dir}}/${{name}}.txt"; }};
                exec shell {{ echo again >> ${{dir}}/${{name}}.txt; }};
                return 0;
            }};
            "#
        ))
        .expect("exec shell should run");
    assert_eq!(outcome.exit_status, 0);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("made.txt")).unwrap(),
        "value interpolated\nagain\n"
    );
}
