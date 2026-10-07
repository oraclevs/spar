use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn spar() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spar"))
}

fn project(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spar-errloc-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn division_by_zero_in_an_imported_function_names_the_imported_file() {
    let dir = project("import");
    fs::write(
        dir.join("lib.spar"),
        "fn boom(x: int) -> int {\n    var y: int = 10 / x;\n    return y;\n};\n",
    )
    .unwrap();
    fs::write(
        dir.join("script.spar"),
        "import { boom } from \"./lib.spar\";\n\nfn main() -> int {\n    var r: int = boom(x: 0);\n    return r;\n};\n",
    )
    .unwrap();

    let output = spar().current_dir(&dir).args(["exec", "script.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("lib.spar:2:"), "{stderr}");
    assert!(stderr.contains("var y: int = 10 / x;"), "{stderr}");
    assert!(!stderr.contains("script.spar:2:"), "{stderr}");
}

#[test]
fn diamond_import_reports_the_shared_file_once() {
    let dir = project("diamond");
    fs::write(dir.join("d.spar"), "fn dz(x: int) -> int {\n    return 1 / x;\n};\n").unwrap();
    fs::write(dir.join("b.spar"), "import { dz } from \"./d.spar\";\nfn viaB() -> int {\n    return dz(x: 0);\n};\n").unwrap();
    fs::write(dir.join("c.spar"), "import { dz } from \"./d.spar\";\nfn viaC() -> int {\n    return dz(x: 1);\n};\n").unwrap();
    fs::write(
        dir.join("a.spar"),
        "import { viaB } from \"./b.spar\";\nimport { viaC } from \"./c.spar\";\nfn main() -> int {\n    var c: int = viaC();\n    return viaB();\n};\n",
    )
    .unwrap();
    let output = spar().current_dir(&dir).args(["exec", "a.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("d.spar:2:"), "{stderr}");
    assert!(stderr.contains("return 1 / x;"), "{stderr}");
}

fn run_tiers(source: &str, extra: &[(&str, &str)]) -> Vec<String> {
    let dir = project("tiers");
    fs::write(dir.join("main.spar"), source).unwrap();
    for (name, text) in extra {
        fs::write(dir.join(name), text).unwrap();
    }
    // Tree walker, bytecode interpreter, then the default (JIT).
    let tiers: [&[(&str, &str)]; 3] = [
        &[("SPAR_DISABLE_VM", "1"), ("SPAR_DISABLE_BYTECODE", "1")],
        &[("SPAR_NO_JIT", "1")],
        &[],
    ];
    tiers
        .iter()
        .map(|env| {
            let mut command = spar();
            command.current_dir(&dir).args(["exec", "main.spar"]);
            for (key, value) in *env {
                command.env(key, value);
            }
            String::from_utf8_lossy(&command.output().unwrap().stderr).into_owned()
        })
        .collect()
}

fn trace_section(stderr: &str) -> String {
    stderr
        .split("trace (most recent call first):")
        .nth(1)
        .map(|rest| rest.trim_end().to_string())
        .unwrap_or_default()
}

const NESTED: &str = "fn inner(x: int) -> int {\n    return 10 / x;\n};\nfn middle(x: int) -> int {\n    return inner(x: x) + 1;\n};\nfn main() -> int {\n    return middle(x: 0);\n};\n";

#[test]
fn three_nested_calls_produce_three_ordered_frames() {
    for stderr in run_tiers(NESTED, &[]) {
        let trace = trace_section(&stderr);
        let inner = trace.find("in inner").unwrap_or_else(|| panic!("{stderr}"));
        let middle = trace.find("called from middle").unwrap_or_else(|| panic!("{stderr}"));
        let main = trace.find("called from main").unwrap_or_else(|| panic!("{stderr}"));
        assert!(inner < middle && middle < main, "{stderr}");
        assert!(trace.contains("main.spar:2:"), "{stderr}");
        assert!(trace.contains("main.spar:5:"), "{stderr}");
        assert!(trace.contains("main.spar:8:"), "{stderr}");
    }
}

#[test]
fn deep_recursion_is_capped_with_a_marker() {
    let source = "fn down(n: int) -> int {\n    if n == 0 {\n        return 1 / n;\n    }\n    return down(n: n - 1) + 1;\n};\nfn main() -> int {\n    return down(n: 30);\n};\n";
    for stderr in run_tiers(source, &[]) {
        let trace = trace_section(&stderr);
        assert!(trace.contains("more frames"), "{stderr}");
        let lines = trace
            .lines()
            .filter(|l| {
                let l = l.trim_start();
                l.starts_with("in ") || l.starts_with("called from") || l.contains("more frames")
            })
            .count();
        assert!(lines <= 10, "{lines} lines\n{stderr}");
        assert!(trace.contains("called from main"), "{stderr}");
    }
}

#[test]
fn top_level_entry_errors_have_no_trace_section() {
    let dir = project("toplevel");
    fs::write(dir.join("main.spar"), "fn main() -> int {\n    return 1 / 0;\n};\n").unwrap();
    let output = spar().current_dir(&dir).args(["exec", "main.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("division by zero"), "{stderr}");
}

#[test]
fn all_tiers_print_identical_traces() {
    let outputs = run_tiers(NESTED, &[]);
    let traces: Vec<String> = outputs.iter().map(|o| trace_section(o)).collect();
    assert!(!traces[0].is_empty(), "{}", outputs[0]);
    assert_eq!(traces[0], traces[1], "tree walker vs bytecode\n{}\n{}", outputs[0], outputs[1]);
    assert_eq!(traces[1], traces[2], "bytecode vs jit\n{}\n{}", outputs[1], outputs[2]);
}

#[test]
fn imported_function_frames_name_the_imported_file() {
    let lib = "fn inner(x: int) -> int {\n    return 10 / x;\n};\n";
    let main = "import { inner } from \"./lib.spar\";\nfn main() -> int {\n    return inner(x: 0);\n};\n";
    for stderr in run_tiers(main, &[("lib.spar", lib)]) {
        let trace = trace_section(&stderr);
        assert!(trace.contains("in inner"), "{stderr}");
        assert!(trace.contains("lib.spar:2:"), "{stderr}");
        assert!(trace.contains("called from main"), "{stderr}");
        assert!(trace.contains("main.spar:3:"), "{stderr}");
    }
}

#[test]
fn awaited_task_errors_keep_their_innermost_frame() {
    let source = "async fn worker(x: int) -> int {\n    return 10 / x;\n};\nasync fn main() -> int {\n    var p: Promise<int> = worker(x: 0);\n    return await p;\n};\n";
    let dir = project("async");
    fs::write(dir.join("main.spar"), source).unwrap();
    let output = spar().current_dir(&dir).args(["exec", "main.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("division by zero"), "{stderr}");
    assert!(stderr.contains("in worker"), "{stderr}");
}

#[test]
fn missing_shell_program_names_the_program_and_line() {
    let dir = project("shellmissing");
    fs::write(
        dir.join("main.spar"),
        "fn run() -> ShellResult<str, str> {\n    nonexistent_cmd_zzz --flag;\n    return ok(value: \"x\");\n};\nfn main() -> int {\n    var r = run();\n    return 0;\n};\n",
    )
    .unwrap();
    let output = spar().current_dir(&dir).args(["exec", "main.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not run 'nonexistent_cmd_zzz': not found"), "{stderr}");
    assert!(stderr.contains("main.spar:2:"), "{stderr}");
    assert!(!stderr.contains("os error 2"), "{stderr}");
}

#[test]
fn aliased_import_errors_name_the_imported_file() {
    let lib = "fn boom(x: int) -> int {\n    return 10 / x;\n};\nfn wrap(x: int) -> int {\n    return boom(x: x);\n};\n";
    let main = "import \"./lib.spar\" as m;\nfn main() -> int {\n    return m::wrap(x: 0);\n};\n";
    for stderr in run_tiers(main, &[("lib.spar", lib)]) {
        assert!(stderr.contains("lib.spar:2:"), "{stderr}");
        assert!(stderr.contains("return 10 / x;"), "{stderr}");
        let trace = trace_section(&stderr);
        assert!(trace.contains("in boom"), "{stderr}");
        assert!(trace.contains("lib.spar:2:"), "{stderr}");
    }
}

#[test]
fn a_failing_argument_does_not_add_a_frame_for_the_callee() {
    let source = "fn id(x: int) -> int {\n    return x;\n};\nfn main() -> int {\n    var n: int = 0;\n    return id(x: 10 / n);\n};\n";
    let outputs = run_tiers(source, &[]);
    for stderr in &outputs {
        let trace = trace_section(stderr);
        assert!(!trace.contains("id "), "callee never ran:\n{stderr}");
        assert!(trace.contains("in main"), "{stderr}");
    }
    let traces: Vec<String> = outputs.iter().map(|o| trace_section(o)).collect();
    assert_eq!(traces[0], traces[1], "tree vs bytecode\n{}\n{}", outputs[0], outputs[1]);
    assert_eq!(traces[1], traces[2], "bytecode vs jit\n{}\n{}", outputs[1], outputs[2]);
}

#[test]
fn a_nested_failing_argument_names_only_the_functions_that_ran() {
    let source = "fn inner(y: int) -> int {\n    return 10 / y;\n};\nfn outer(x: int) -> int {\n    return x;\n};\nfn main() -> int {\n    return outer(x: inner(y: 0));\n};\n";
    for stderr in run_tiers(source, &[]) {
        let trace = trace_section(&stderr);
        assert!(trace.contains("in inner"), "{stderr}");
        assert!(!trace.contains("outer"), "outer never ran:\n{stderr}");
        assert!(trace.contains("called from main"), "{stderr}");
    }
}

#[test]
fn a_returned_shell_plan_is_reported_at_its_own_line() {
    let dir = project("twoplans");
    fs::write(
        dir.join("main.spar"),
        "fn main() -> __shell {\n    var p: __shell = __shell { definitely_missing_zz; };\n    var q: __shell = __shell {\n\n\n        true;\n    };\n    return p;\n};\n",
    )
    .unwrap();
    let output = spar().current_dir(&dir).args(["exec", "main.spar"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not run 'definitely_missing_zz'"), "{stderr}");
    assert!(stderr.contains("main.spar:2:"), "{stderr}");
    assert!(!stderr.contains("main.spar:6:"), "{stderr}");
}
