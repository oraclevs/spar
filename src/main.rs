use spar::runner::{ExecutionOptions, RunnerError, TaskInvocation, TaskSet};
use spar::{renderer::ErrorRenderer, CompileOptions, Compiler};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match parse_args(&args) {
        Cmd::Check(path) => cmd_check(&path),
        Cmd::Emit(path) => cmd_emit(&path),
        Cmd::Fmt { path, check } => cmd_fmt(&path, check),
        Cmd::Tasks { path, all } => cmd_tasks(path, all),
        Cmd::Run {
            path,
            task,
            args,
            dry_run,
            choose,
        } => cmd_run(path, task, args, dry_run, choose),
        Cmd::Show { path, task, args } => {
            let _ = (path, task, args);
            eprintln!("error: command is not implemented");
            std::process::exit(1);
        }
        Cmd::Dump { path } => {
            let _ = path;
            eprintln!("error: command is not implemented");
            std::process::exit(1);
        }
        Cmd::Help => print_help(),
        Cmd::Version => println!("spar {}", env!("CARGO_PKG_VERSION")),
        Cmd::BadArgs(msg) => {
            eprintln!("error: {msg}\nRun `spar --help` for usage.");
            std::process::exit(1);
        }
    }
}

// ── Argument parsing ──────────────────────────────────────────────────────────

#[derive(Debug)]
enum Cmd {
    Check(String),
    Emit(String),
    Fmt {
        path: String,
        check: bool,
    },
    Tasks {
        path: Option<PathBuf>,
        all: bool,
    },
    Run {
        path: Option<PathBuf>,
        task: Option<String>,
        args: Vec<String>,
        dry_run: bool,
        choose: bool,
    },
    Show {
        path: Option<PathBuf>,
        task: String,
        args: Vec<String>,
    },
    Dump {
        path: Option<PathBuf>,
    },
    Help,
    Version,
    BadArgs(String),
}

fn parse_args(args: &[String]) -> Cmd {
    match args.get(1).map(String::as_str) {
        Some("check") => match args.get(2) {
            Some(p) => Cmd::Check(p.clone()),
            None => Cmd::BadArgs("`check` requires a file path".into()),
        },
        Some("emit") => match args.get(2) {
            Some(p) => Cmd::Emit(p.clone()),
            None => Cmd::BadArgs("`emit` requires a file path".into()),
        },
        Some("fmt") => match (args.get(2).map(String::as_str), args.get(3)) {
            (Some("--check"), Some(p)) => Cmd::Fmt {
                path: p.clone(),
                check: true,
            },
            (Some(p), None) if p != "--check" => Cmd::Fmt {
                path: p.to_string(),
                check: false,
            },
            _ => Cmd::BadArgs("`fmt` requires a file path (optionally preceded by --check)".into()),
        },
        Some("tasks") => parse_tasks_args(&args[2..]),
        Some("run") => parse_run_args(&args[2..]),
        Some("show") => parse_show_args(&args[2..]),
        Some("dump") => parse_dump_args(&args[2..]),
        Some("--help") | Some("-h") | None => Cmd::Help,
        Some("--version") | Some("-V") => Cmd::Version,
        Some(other) => Cmd::BadArgs(format!("unknown command `{other}`")),
    }
}

fn set_task_file(path: &mut Option<PathBuf>, value: Option<&String>) -> Result<(), String> {
    if path.is_some() {
        return Err("file flag may only be specified once".to_owned());
    }
    let value = value.ok_or_else(|| "`-f`/`--file` requires a path".to_owned())?;
    *path = Some(PathBuf::from(value));
    Ok(())
}

fn parse_tasks_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut all = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "--all" => {
                all = true;
                index += 1;
            }
            other => return Cmd::BadArgs(format!("unexpected argument for `tasks`: {other}")),
        }
    }
    Cmd::Tasks { path, all }
}

fn parse_run_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut dry_run = false;
    let mut choose = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--choose" => {
                choose = true;
                index += 1;
            }
            other if positional.is_empty() && other.starts_with('-') => {
                return Cmd::BadArgs(format!("unknown option for `run`: {other}"));
            }
            other => {
                positional.push(other.to_owned());
                index += 1;
            }
        }
    }
    let task = positional.first().cloned();
    if choose && task.is_some() {
        return Cmd::BadArgs("`--choose` cannot be used with an explicit task".to_owned());
    }
    Cmd::Run {
        path,
        task,
        args: positional.into_iter().skip(1).collect(),
        dry_run,
        choose,
    }
}

fn parse_show_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            other if positional.is_empty() && other.starts_with('-') => {
                return Cmd::BadArgs(format!("unknown option for `show`: {other}"));
            }
            other => {
                positional.push(other.to_owned());
                index += 1;
            }
        }
    }
    let Some(task) = positional.first().cloned() else {
        return Cmd::BadArgs("`show` requires a task name".to_owned());
    };
    Cmd::Show {
        path,
        task,
        args: positional.into_iter().skip(1).collect(),
    }
}

fn parse_dump_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            other => return Cmd::BadArgs(format!("unexpected argument for `dump`: {other}")),
        }
    }
    Cmd::Dump { path }
}

fn print_help() {
    println!(
        "spar — configuration language v{ver}

USAGE:
    spar <COMMAND> [OPTIONS]

COMMANDS:
    check         <file.spar>           Validate a .spar file — runs lex, parse, resolve, and type check
    emit          <file.spar>           Evaluate and print the config as JSON to stdout
    fmt           <file.spar>           Format a .spar file in place
    fmt --check   <file.spar>           Exit non-zero if file is not already formatted
    tasks         [-f FILE] [--all]     List declared tasks
    run           [task] [args...] [-f FILE] [--dry-run] [--choose]
                                        Run a task (default task if omitted)
    show          <task> [args...] [-f FILE]
                                        Show one task's resolved commands
    dump          [-f FILE]             Dump the lowered task catalog as JSON

OPTIONS:
    -h, --help        Show this help
    -V, --version     Show version
    -f, --file FILE   Use FILE instead of discovering SparMake.spar

ENVIRONMENT:
    NO_COLOR=1        Disable ANSI colour in error output

EXAMPLES:
    spar check server.spar
    spar emit  server.spar > config.json
    spar fmt   server.spar
    spar fmt --check server.spar
    spar tasks -f server.spar
    spar run -f server.spar
    spar run deploy production -f server.spar
    spar run test --dry-run -f server.spar",
        ver = env!("CARGO_PKG_VERSION")
    );
}

// ── Colour detection ──────────────────────────────────────────────────────────

pub fn use_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    !matches!(std::env::var("TERM").as_deref(), Ok("dumb"))
}

// ── `check` command ───────────────────────────────────────────────────────────

fn cmd_check(path: &str) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let options = CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(path)
    };
    let compilation = Compiler::new(options).compile(&src);
    if compilation.errors.is_empty() {
        println!("{path}: ok");
    } else {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
}

// ── `emit` command ────────────────────────────────────────────────────────────

fn cmd_emit(path: &str) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let options = CompileOptions {
        allow_schema_file: false,
        ..CompileOptions::for_path(path)
    };
    let compilation = Compiler::new(options).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    let result = compilation
        .result
        .as_ref()
        .expect("successful compilation evaluates");
    let symbols = compilation
        .symbols
        .as_ref()
        .expect("successful compilation resolves");
    for w in &result.warnings {
        eprintln!("warning: {w}");
    }
    println!("{}", emit_json(result, symbols));
}

// ── `fmt` command ─────────────────────────────────────────────────────────────

fn cmd_fmt(path: &str, check: bool) {
    let src = read_file(path);
    let formatted = match spar::formatter::format_source(&src) {
        Ok(s) => s,
        Err(e) => {
            let renderer = make_renderer(&src, path);
            eprintln!("{}", renderer.render(&e));
            std::process::exit(1);
        }
    };
    if check {
        if formatted != src {
            eprintln!("{path}: not formatted");
            std::process::exit(1);
        }
    } else {
        if formatted != src {
            let tmp_path = format!("{path}.tmp");
            std::fs::write(&tmp_path, &formatted).unwrap_or_else(|e| {
                eprintln!("error: cannot write `{tmp_path}`: {e}");
                std::process::exit(1);
            });
            std::fs::rename(&tmp_path, path).unwrap_or_else(|e| {
                let _ = std::fs::remove_file(&tmp_path); // best-effort cleanup
                eprintln!("error: cannot rename `{tmp_path}` to `{path}`: {e}");
                std::process::exit(1);
            });
        }
    }
}

// ── `tasks` command ───────────────────────────────────────────────────────────

fn cmd_tasks(path: Option<PathBuf>, _all: bool) {
    let path = resolve_task_path(path);
    let path_text = path.to_string_lossy();
    let src = read_file(&path_text);
    let renderer = make_renderer(&src, &path_text);
    let compilation = Compiler::new(CompileOptions::for_path(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    print_task_list(compilation.tasks.as_ref());
}

fn print_task_list(tasks: Option<&TaskSet>) {
    println!("Available tasks:\n");
    let Some(tasks) = tasks else {
        println!("  (none)");
        return;
    };
    let rows: Vec<(String, String)> = tasks
        .iter()
        .map(|t| {
            (
                t.name.to_lowercase(),
                t.description.clone().unwrap_or_default(),
            )
        })
        .collect();
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, description) in rows {
        if description.is_empty() {
            println!("  {name}");
        } else {
            println!("  {name:width$}   {description}");
        }
    }
}

// ── `run` command ─────────────────────────────────────────────────────────────

fn cmd_run(
    path: Option<PathBuf>,
    task: Option<String>,
    args: Vec<String>,
    dry_run: bool,
    _choose: bool,
) {
    let path = resolve_task_path(path);
    let path_text = path.to_string_lossy();
    let src = read_file(&path_text);
    let renderer = make_renderer(&src, &path_text);
    let options = CompileOptions::for_path(&path);
    let base_dir = options.base_dir.clone();
    let compilation = Compiler::new(options).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }

    let Some(tasks) = compilation.tasks.as_ref() else {
        eprintln!("error: {} declares no tasks", path.display());
        std::process::exit(1);
    };

    let requested = match &task {
        Some(name) => tasks.get(name),
        None => tasks.default_task(),
    };
    let requested_name = match requested {
        Ok(t) => t.name.clone(),
        Err(e) => {
            eprintln!("error: {e}");
            if matches!(e, RunnerError::MissingDefaultTask) {
                print_task_list(Some(tasks));
            }
            std::process::exit(1);
        }
    };

    let invocation = TaskInvocation {
        task: requested_name,
        arguments: args,
    };
    let plan = match tasks.plan(&invocation) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    let exec_options = ExecutionOptions { dry_run, base_dir };
    if let Err(e) = spar::runner::execute(&plan, &exec_options) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn discover_task_file(start: &Path) -> Result<PathBuf, String> {
    for directory in start.ancestors() {
        let candidate = directory.join("SparMake.spar");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "could not find SparMake.spar in `{}` or any parent directory",
        start.display()
    ))
}

fn resolve_task_path(path: Option<PathBuf>) -> PathBuf {
    match path {
        Some(path) => path,
        None => std::env::current_dir()
            .map_err(|error| format!("cannot read current directory: {error}"))
            .and_then(|directory| discover_task_file(&directory))
            .unwrap_or_else(|message| {
                eprintln!("error: {message}");
                std::process::exit(1);
            }),
    }
}

fn read_file(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read `{path}`: {e}");
        std::process::exit(1);
    })
}

fn make_renderer<'a>(src: &'a str, path: &'a str) -> ErrorRenderer<'a> {
    if use_color() {
        ErrorRenderer::with_color(src, path)
    } else {
        ErrorRenderer::new(src, path)
    }
}

// ── JSON emission ─────────────────────────────────────────────────────────────

fn emit_json(result: &spar::evaluator::EvalResult, symbols: &spar::SymbolTable) -> String {
    let val = spar::emit::build_emit_json(result, symbols);
    serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colored_renderer_contains_ansi() {
        let src = "var port: int = 3000;";
        let r = spar::ErrorRenderer::with_color(src, "test.spar");
        let e = spar::SparError::ResolveError {
            message: "test".into(),
            hint: None,
            span: spar::Span::new(4, 8, 1, 5),
        };
        let out = r.render(&e);
        assert!(out.contains("\x1b["), "no ANSI codes in:\n{out}");
    }

    #[test]
    fn test_use_color_no_color_env() {
        std::env::set_var("NO_COLOR", "1");
        assert!(!use_color());
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn parse_args_fmt_no_check() {
        let args = vec![
            "spar".to_string(),
            "fmt".to_string(),
            "foo.spar".to_string(),
        ];
        match parse_args(&args) {
            Cmd::Fmt { path, check } => {
                assert_eq!(path, "foo.spar");
                assert!(!check);
            }
            other => panic!(
                "expected Cmd::Fmt, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn parse_args_fmt_with_check() {
        let args = vec![
            "spar".to_string(),
            "fmt".to_string(),
            "--check".to_string(),
            "foo.spar".to_string(),
        ];
        match parse_args(&args) {
            Cmd::Fmt { path, check } => {
                assert_eq!(path, "foo.spar");
                assert!(check);
            }
            other => panic!(
                "expected Cmd::Fmt, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn parse_args_fmt_missing_path_is_bad_args() {
        let args = vec!["spar".to_string(), "fmt".to_string()];
        assert!(matches!(parse_args(&args), Cmd::BadArgs(_)));
    }

    #[test]
    fn discover_task_file_searches_current_directory_then_parents() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("one").join("two");
        std::fs::create_dir_all(&nested).unwrap();
        let parent_file = directory.path().join("SparMake.spar");
        std::fs::write(&parent_file, "task [Build] { run { true; }; }").unwrap();

        assert_eq!(discover_task_file(&nested).unwrap(), parent_file);

        let current_file = nested.join("SparMake.spar");
        std::fs::write(&current_file, "task [Build] { run { true; }; }").unwrap();
        assert_eq!(discover_task_file(&nested).unwrap(), current_file);
    }

    #[test]
    fn parse_args_tasks_accepts_optional_file_and_all_flag() {
        let args = vec![
            "spar".to_owned(),
            "tasks".to_owned(),
            "--all".to_owned(),
            "-f".to_owned(),
            "Tasks.spar".to_owned(),
        ];

        match parse_args(&args) {
            Cmd::Tasks { path, all } => {
                assert_eq!(path, Some(PathBuf::from("Tasks.spar")));
                assert!(all);
            }
            other => panic!("expected tasks command, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_run_keeps_dash_prefixed_task_arguments() {
        let args = vec![
            "spar".to_owned(),
            "run".to_owned(),
            "deploy".to_owned(),
            "production".to_owned(),
            "--force".to_owned(),
            "--dry-run".to_owned(),
            "--file".to_owned(),
            "Tasks.spar".to_owned(),
        ];

        match parse_args(&args) {
            Cmd::Run {
                path,
                task,
                args,
                dry_run,
                choose,
            } => {
                assert_eq!(path, Some(PathBuf::from("Tasks.spar")));
                assert_eq!(task.as_deref(), Some("deploy"));
                assert_eq!(args, ["production", "--force"]);
                assert!(dry_run);
                assert!(!choose);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_supports_choose_show_and_dump() {
        let choose = vec!["spar".to_owned(), "run".to_owned(), "--choose".to_owned()];
        assert!(matches!(
            parse_args(&choose),
            Cmd::Run {
                task: None,
                choose: true,
                ..
            }
        ));

        let show = vec![
            "spar".to_owned(),
            "show".to_owned(),
            "deploy".to_owned(),
            "production".to_owned(),
        ];
        assert!(matches!(
            parse_args(&show),
            Cmd::Show { task, args, .. }
                if task == "deploy" && args == ["production"]
        ));

        let dump = vec!["spar".to_owned(), "dump".to_owned()];
        assert!(matches!(parse_args(&dump), Cmd::Dump { path: None }));
    }

    #[test]
    fn parse_args_rejects_old_bare_file_for_tasks() {
        let args = vec![
            "spar".to_owned(),
            "tasks".to_owned(),
            "old-style.spar".to_owned(),
        ];
        assert!(matches!(parse_args(&args), Cmd::BadArgs(_)));
    }

    #[test]
    fn pipeline_reports_both_type_errors() {
        // Two independent type errors — both must be reported
        let src = "var x: int = \"string\";\nvar y: bool = 42;\n";
        let tokens = spar::Lexer::new(src).tokenize().unwrap();
        let program = spar::Parser::new(tokens).parse().unwrap();
        let symbols = spar::Resolver::new().resolve(&program, &[]).unwrap();
        let errs = spar::TypeChecker::check(&program, &symbols).unwrap_err();
        assert!(
            errs.len() >= 2,
            "both type errors must be reported, got {} error(s): {:?}",
            errs.len(),
            errs
        );
    }
}

#[cfg(test)]
mod emit_tests {

    use spar::emit::build_emit_json;
    use spar::evaluator::Evaluator;
    use spar::resolver::Resolver;
    use spar::typechecker::TypeChecker;
    use spar::{Lexer, Parser};

    fn emit_src(src: &str) -> serde_json::Value {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        TypeChecker::check(&program, &symbols).unwrap();
        let result = Evaluator::evaluate(&program, &symbols).unwrap();
        build_emit_json(&result, &symbols)
    }

    #[test]
    fn plain_var_not_in_emit_output() {
        let json = emit_src(r#"var secret: str = "hidden";"#);
        assert!(
            json.get("secret").is_none(),
            "plain var must not appear in emit output"
        );
    }

    #[test]
    fn export_var_in_emit_output() {
        let json = emit_src(r#"export var name: str = "keel";"#);
        assert_eq!(json["name"], "keel");
    }

    #[test]
    fn regular_section_in_emit_output() {
        let json = emit_src("[Server]{ port: int = 8080; };");
        assert!(json.get("Server").is_some());
        assert_eq!(json["Server"]["port"], 8080);
    }

    #[test]
    fn private_section_not_in_emit_output() {
        let json = emit_src("private [Defaults]{ timeout: int = 30; };");
        assert!(
            json.get("Defaults").is_none(),
            "private section must not appear in emit"
        );
    }

    #[test]
    fn private_section_still_resolvable_by_public_section() {
        let src = r#"
private [Defaults]{ timeout: int = 30; };
[Server]{ timeout: int = Defaults.timeout; };
"#;
        let json = emit_src(src);
        assert!(json.get("Defaults").is_none());
        assert_eq!(json["Server"]["timeout"], 30);
    }

    #[test]
    fn nested_section_embedded_in_parent() {
        let src = r#"
[MetaData]{
    tool: str = "stackforge";
    manual: section = { author: str = "occ"; };
};
"#;
        let json = emit_src(src);
        assert!(
            json.get("manual").is_none(),
            "nested section must NOT appear at root"
        );
        assert_eq!(json["MetaData"]["manual"]["author"], "occ");
    }

    #[test]
    fn schema_file_is_rejected_by_parser_not_emit() {
        // The parser already sets is_schema_file=true; the emit path checks this.
        let src = "@SchemaFile\nSchema [X]{ a: int; }\n";
        let tokens = spar::Lexer::new(src).tokenize().unwrap();
        let prog = spar::Parser::new(tokens).parse().unwrap();
        assert!(prog.is_schema_file, "schema file flag must be set");
        // The emit guard in cmd_emit rejects it; test the flag is present for the guard to work.
    }

    #[test]
    fn full_example_matches_expected_output() {
        let src = r#"
var options: [str] = ["one","two","three"];
[Man]{ aster: int = 6; };
[MetaData]{
    tool:    str = "stackforge";
    version: int = Man.aster;
    askter:  bool = false;
    manual: section = {
        main: str = "MainMan";
        more: section = { see: int = 5; };
        options: [str] = options;
    };
};
"#;
        let json = emit_src(src);

        assert!(
            json.get("options").is_none(),
            "plain var 'options' must be hidden"
        );
        assert_eq!(json["Man"]["aster"], 6);
        assert_eq!(json["MetaData"]["tool"], "stackforge");
        assert_eq!(json["MetaData"]["version"], 6);
        assert_eq!(json["MetaData"]["askter"], false);
        assert_eq!(json["MetaData"]["manual"]["main"], "MainMan");
        assert_eq!(json["MetaData"]["manual"]["more"]["see"], 5);
        assert_eq!(
            json["MetaData"]["manual"]["options"],
            serde_json::json!(["one", "two", "three"])
        );
    }
}
