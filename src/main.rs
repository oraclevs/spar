use spar::runner::{ExecutionOptions, RunnerError, TaskInvocation, TaskSet};
use spar::{renderer::ErrorRenderer, CompileOptions, Compiler};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match parse_args(&args) {
        Cmd::Check(path) => cmd_check(&path),
        Cmd::Emit(path) => cmd_emit(&path),
        Cmd::Fmt { path, check } => cmd_fmt(&path, check),
        Cmd::Tasks { path, global, all } => cmd_tasks(path, global, all),
        Cmd::Run {
            path,
            global,
            task,
            args,
            dry_run,
            choose,
        } => cmd_run(path, global, task, args, dry_run, choose),
        Cmd::Show {
            path,
            global,
            task,
            args,
        } => cmd_show(path, global, task, args),
        Cmd::Dump { path, global } => cmd_dump(path, global),
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
        global: bool,
        all: bool,
    },
    Run {
        path: Option<PathBuf>,
        global: bool,
        task: Option<String>,
        args: Vec<String>,
        dry_run: bool,
        choose: bool,
    },
    Show {
        path: Option<PathBuf>,
        global: bool,
        task: String,
        args: Vec<String>,
    },
    Dump {
        path: Option<PathBuf>,
        global: bool,
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

fn set_task_file(
    path: &mut Option<PathBuf>,
    global: bool,
    value: Option<&String>,
) -> Result<(), String> {
    if global {
        return Err("global flag and file flag may not be used together".to_owned());
    }
    if path.is_some() {
        return Err("file flag may only be specified once".to_owned());
    }
    let value = value.ok_or_else(|| "`-f`/`--file` requires a path".to_owned())?;
    *path = Some(PathBuf::from(value));
    Ok(())
}

fn set_global(global: &mut bool, path: &Option<PathBuf>) -> Result<(), String> {
    if path.is_some() {
        return Err("global flag and file flag may not be used together".to_owned());
    }
    if *global {
        return Err("global flag may only be specified once".to_owned());
    }
    *global = true;
    Ok(())
}

fn parse_tasks_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut global = false;
    let mut all = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, global, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "-G" | "--global" => {
                if let Err(message) = set_global(&mut global, &path) {
                    return Cmd::BadArgs(message);
                }
                index += 1;
            }
            "--all" => {
                all = true;
                index += 1;
            }
            other => return Cmd::BadArgs(format!("unexpected argument for `tasks`: {other}")),
        }
    }
    Cmd::Tasks { path, global, all }
}

fn parse_run_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut global = false;
    let mut dry_run = false;
    let mut choose = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, global, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "-G" | "--global" => {
                if let Err(message) = set_global(&mut global, &path) {
                    return Cmd::BadArgs(message);
                }
                index += 1;
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
        global,
        task,
        args: positional.into_iter().skip(1).collect(),
        dry_run,
        choose,
    }
}

fn parse_show_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut global = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, global, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "-G" | "--global" => {
                if let Err(message) = set_global(&mut global, &path) {
                    return Cmd::BadArgs(message);
                }
                index += 1;
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
        global,
        task,
        args: positional.into_iter().skip(1).collect(),
    }
}

fn parse_dump_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut global = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-f" | "--file" => {
                if let Err(message) = set_task_file(&mut path, global, args.get(index + 1)) {
                    return Cmd::BadArgs(message);
                }
                index += 2;
            }
            "-G" | "--global" => {
                if let Err(message) = set_global(&mut global, &path) {
                    return Cmd::BadArgs(message);
                }
                index += 1;
            }
            other => return Cmd::BadArgs(format!("unexpected argument for `dump`: {other}")),
        }
    }
    Cmd::Dump { path, global }
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
    tasks         [-f FILE | -G] [--all]
                                        List declared tasks
    run           [task] [args...] [-f FILE | -G] [--dry-run] [--choose]
                                        Run a task (default task if omitted)
    show          <task> [args...] [-f FILE | -G]
                                        Show one task's resolved commands
    dump          [-f FILE | -G]        Dump the lowered task catalog as JSON

OPTIONS:
    -h, --help        Show this help
    -V, --version     Show version
    -f, --file FILE   Use FILE instead of discovering SparMake.spar
    -G, --global      Use the global ~/.spar/SparMake.spar task file

ENVIRONMENT:
    NO_COLOR=1        Disable ANSI colour output

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

fn use_stdout_color() -> bool {
    use_color() && std::io::stdout().is_terminal()
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

fn cmd_tasks(path: Option<PathBuf>, global: bool, _all: bool) {
    let path = resolve_task_path(path, global);
    let path_text = path.to_string_lossy();
    let src = read_file(&path_text);
    let renderer = make_renderer(&src, &path_text);
    let compilation = Compiler::new(CompileOptions::for_path(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    print_task_list(
        compilation.tasks.as_ref(),
        _all,
        use_stdout_color(),
        &mut std::io::stdout().lock(),
    );
}

fn listed_tasks(tasks: &TaskSet, include_private: bool) -> Vec<&spar::runner::Task> {
    let mut tasks: Vec<_> = tasks
        .iter()
        .filter(|task| include_private || !task.private)
        .collect();
    tasks.sort_by(|left, right| {
        (
            left.group.is_some(),
            left.group.as_deref().unwrap_or(""),
            left.name.as_str(),
        )
            .cmp(&(
                right.group.is_some(),
                right.group.as_deref().unwrap_or(""),
                right.name.as_str(),
            ))
    });
    tasks
}

fn print_task_list(
    tasks: Option<&TaskSet>,
    include_private: bool,
    color: bool,
    output: &mut dyn Write,
) {
    let _ = writeln!(output, "Available tasks:\n");
    let Some(tasks) = tasks else {
        let _ = writeln!(output, "  (none)");
        return;
    };
    let rows = listed_tasks(tasks, include_private);
    if rows.is_empty() {
        let _ = writeln!(output, "  (none)");
        return;
    }
    let width = rows.iter().map(|task| task.name.len()).max().unwrap_or(0);
    let mut current_group: Option<Option<&str>> = None;
    for task in rows {
        let group = task.group.as_deref();
        if current_group != Some(group) {
            if current_group.is_some() {
                let _ = writeln!(output);
            }
            if color {
                let _ = writeln!(output, "\x1b[1;33m{}\x1b[0m:", group.unwrap_or("Ungrouped"));
            } else {
                let _ = writeln!(output, "{}:", group.unwrap_or("Ungrouped"));
            }
            current_group = Some(group);
        }
        let name = task.name.to_lowercase();
        if let Some(description) = task
            .description
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            if color {
                let _ = writeln!(
                    output,
                    "  \x1b[1;32m{name:width$}\x1b[0m   \x1b[37m{description}\x1b[0m"
                );
            } else {
                let _ = writeln!(output, "  {name:width$}   {description}");
            }
        } else if color {
            let _ = writeln!(output, "  \x1b[1;32m{name}\x1b[0m");
        } else {
            let _ = writeln!(output, "  {name}");
        }
    }
}

// ── `run` command ─────────────────────────────────────────────────────────────

fn cmd_run(
    path: Option<PathBuf>,
    global: bool,
    task: Option<String>,
    args: Vec<String>,
    dry_run: bool,
    choose: bool,
) {
    let path = resolve_task_path(path, global);
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

    let task = if choose {
        match choose_task(tasks) {
            Ok(task) => Some(task),
            Err(message) => {
                eprintln!("error: {message}");
                std::process::exit(1);
            }
        }
    } else {
        task
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
                print_task_list(
                    Some(tasks),
                    false,
                    use_stdout_color(),
                    &mut std::io::stdout().lock(),
                );
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
        match e {
            RunnerError::QuietCommandFailed {
                task,
                source_line: Some(line),
                status,
            } => eprintln!(
                "error: task {task} ({}:{line}) failed: {status}",
                path.display()
            ),
            error => eprintln!("error: {error}"),
        }
        std::process::exit(1);
    }
}

fn choose_task(tasks: &TaskSet) -> Result<String, String> {
    let tasks = listed_tasks(tasks, false);
    if tasks.is_empty() {
        return Err("no runnable tasks are available".to_owned());
    }

    let mut output = std::io::stderr().lock();
    let mut current_group: Option<Option<&str>> = None;
    for (index, task) in tasks.iter().enumerate() {
        let group = task.group.as_deref();
        if current_group != Some(group) {
            let _ = writeln!(output, "{}:", group.unwrap_or("Ungrouped"));
            current_group = Some(group);
        }
        let _ = writeln!(output, "  {}. {}", index + 1, task.name.to_lowercase());
    }
    let _ = write!(output, "Choose a task: ");
    let _ = output.flush();

    let mut selection = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut selection)
        .map_err(|error| format!("could not read task choice: {error}"))?;
    let selection = selection.trim();
    if let Ok(number) = selection.parse::<usize>() {
        return tasks
            .get(number.saturating_sub(1))
            .map(|task| task.name.clone())
            .ok_or_else(|| format!("invalid task choice: {selection}"));
    }
    tasks
        .iter()
        .find(|task| task.name.eq_ignore_ascii_case(selection))
        .map(|task| task.name.clone())
        .ok_or_else(|| format!("invalid task choice: {selection}"))
}

fn cmd_show(path: Option<PathBuf>, global: bool, task: String, args: Vec<String>) {
    let path = resolve_task_path(path, global);
    let path_text = path.to_string_lossy();
    let src = read_file(&path_text);
    let renderer = make_renderer(&src, &path_text);
    let compilation = Compiler::new(CompileOptions::for_path(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    let Some(tasks) = compilation.tasks.as_ref() else {
        eprintln!("error: {} declares no tasks", path.display());
        std::process::exit(1);
    };
    let bound = tasks
        .bind(&TaskInvocation {
            task,
            arguments: args,
        })
        .unwrap_or_else(|error| {
            eprintln!("error: {error}");
            std::process::exit(1);
        });
    let color = use_stdout_color();
    let mut output = std::io::stdout().lock();
    for command in &bound.task.commands {
        print_resolved_command(&command.render(&bound.parameter_values), color, &mut output);
    }
}

fn print_resolved_command(command: &str, color: bool, output: &mut dyn Write) {
    if color {
        let _ = writeln!(output, "\x1b[1;36m{command}\x1b[0m");
    } else {
        let _ = writeln!(output, "{command}");
    }
}

fn cmd_dump(path: Option<PathBuf>, global: bool) {
    let path = resolve_task_path(path, global);
    let path_text = path.to_string_lossy();
    let src = read_file(&path_text);
    let renderer = make_renderer(&src, &path_text);
    let compilation = Compiler::new(CompileOptions::for_path(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    let tasks = compilation
        .tasks
        .as_ref()
        .map(|tasks| tasks.iter().map(task_json).collect::<Vec<_>>())
        .unwrap_or_default();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "tasks": tasks })).unwrap()
    );
}

fn task_json(task: &spar::runner::Task) -> serde_json::Value {
    let parameters: Vec<_> = task
        .parameters
        .iter()
        .map(|parameter| {
            let kind = match parameter.kind {
                spar::runner::ScalarKind::Str => "str",
                spar::runner::ScalarKind::Int => "int",
                spar::runner::ScalarKind::Float => "float",
                spar::runner::ScalarKind::Bool => "bool",
            };
            serde_json::json!({
                "name": parameter.name,
                "type": kind,
                "default": parameter.default,
                "variadic": parameter.variadic,
            })
        })
        .collect();
    let commands: Vec<_> = task
        .commands
        .iter()
        .map(|command| {
            let kind = match command {
                spar::runner::TaskCommand::Shell(_) => "shell",
                spar::runner::TaskCommand::Script(_) => "script",
            };
            serde_json::json!({
                "kind": kind,
                "template": command.render_unbound(),
            })
        })
        .collect();
    serde_json::json!({
        "name": task.name,
        "description": task.description,
        "default": task.default,
        "quiet": task.quiet,
        "group": task.group,
        "private": task.private,
        "confirm": task.confirm,
        "dependencies": task.dependencies,
        "parameters": parameters,
        "environment": task.environment,
        "cwd": task.cwd.as_ref().map(|path| path.display().to_string()),
        "shell": task.shell,
        "commands": commands,
    })
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

fn global_task_file(home: &Path) -> Result<PathBuf, String> {
    let path = home.join(".spar").join("SparMake.spar");
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "global task file not found: {} — create it first",
            path.display()
        ))
    }
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf, String> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "cannot determine home directory: HOME is not set".to_owned())
}

#[cfg(windows)]
fn home_dir() -> Result<PathBuf, String> {
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .map_err(|_| "cannot determine home directory: USERPROFILE is not set".to_owned())
}

fn resolve_task_path(path: Option<PathBuf>, global: bool) -> PathBuf {
    let resolved = match path {
        Some(path) => Ok(path),
        None if global => home_dir().and_then(|home| global_task_file(&home)),
        None => std::env::current_dir()
            .map_err(|error| format!("cannot read current directory: {error}"))
            .and_then(|directory| discover_task_file(&directory)),
    };
    resolved.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        std::process::exit(1);
    })
}

fn read_file(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read `{path}`: {e}");
        std::process::exit(1);
    })
}

fn make_renderer<'a>(src: &'a str, path: &'a str) -> ErrorRenderer<'a> {
    if use_color() && std::io::stderr().is_terminal() {
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
        std::fs::write(&parent_file, "task [Build] { run { true; }; };").unwrap();

        assert_eq!(discover_task_file(&nested).unwrap(), parent_file);

        let current_file = nested.join("SparMake.spar");
        std::fs::write(&current_file, "task [Build] { run { true; }; };").unwrap();
        assert_eq!(discover_task_file(&nested).unwrap(), current_file);
    }

    #[test]
    fn global_task_file_resolves_below_the_given_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let spar_directory = home.path().join(".spar");
        std::fs::create_dir(&spar_directory).unwrap();
        let task_file = spar_directory.join("SparMake.spar");
        std::fs::write(&task_file, "task [Build] { run { true; }; };").unwrap();

        assert_eq!(global_task_file(home.path()).unwrap(), task_file);
    }

    #[test]
    fn missing_global_task_file_has_an_actionable_error() {
        let home = tempfile::tempdir().unwrap();

        let error = global_task_file(home.path()).unwrap_err();

        assert!(error.contains("global task file not found"), "{error}");
        assert!(error.contains(".spar/SparMake.spar"), "{error}");
        assert!(error.contains("create it first"), "{error}");
    }

    #[test]
    fn task_list_colors_groups_names_and_descriptions_when_enabled() {
        let source = r#"
task [Build] { description: "Compile"; run { true; }; };
task [Deploy] { group: "release"; description: "Ship it"; run { true; }; };
"#;
        let compilation = Compiler::new(CompileOptions::for_path("Tasks.spar")).compile(source);
        assert!(compilation.errors.is_empty());
        let mut output = Vec::new();

        print_task_list(compilation.tasks.as_ref(), false, true, &mut output);

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[1;33mUngrouped\x1b[0m:"), "{output:?}");
        assert!(output.contains("\x1b[1;32mbuild \x1b[0m"), "{output:?}");
        assert!(output.contains("\x1b[37mCompile\x1b[0m"), "{output:?}");
        assert!(output.contains("\x1b[1;33mrelease\x1b[0m:"), "{output:?}");
    }

    #[test]
    fn task_list_plain_output_remains_byte_for_byte_unchanged() {
        let source = r#"
task [Build] { description: "Compile"; run { true; }; };
task [Deploy] { group: "release"; description: "Ship it"; run { true; }; };
"#;
        let compilation = Compiler::new(CompileOptions::for_path("Tasks.spar")).compile(source);
        assert!(compilation.errors.is_empty());
        let mut output = Vec::new();

        print_task_list(compilation.tasks.as_ref(), false, false, &mut output);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Available tasks:\n\nUngrouped:\n  build    Compile\n\nrelease:\n  deploy   Ship it\n"
        );
    }

    #[test]
    fn resolved_command_output_has_explicit_colored_and_plain_modes() {
        let mut colored = Vec::new();
        print_resolved_command("echo deploy-production", true, &mut colored);
        assert_eq!(
            String::from_utf8(colored).unwrap(),
            "\x1b[1;36mecho deploy-production\x1b[0m\n"
        );

        let mut plain = Vec::new();
        print_resolved_command("echo deploy-production", false, &mut plain);
        assert_eq!(
            String::from_utf8(plain).unwrap(),
            "echo deploy-production\n"
        );
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
            Cmd::Tasks { path, all, .. } => {
                assert_eq!(path, Some(PathBuf::from("Tasks.spar")));
                assert!(all);
            }
            other => panic!("expected tasks command, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_task_commands_accept_global_and_reject_file_with_global() {
        let tasks = vec!["spar".to_owned(), "tasks".to_owned(), "-G".to_owned()];
        assert!(matches!(
            parse_args(&tasks),
            Cmd::Tasks { global: true, .. }
        ));

        let run = vec!["spar".to_owned(), "run".to_owned(), "--global".to_owned()];
        assert!(matches!(parse_args(&run), Cmd::Run { global: true, .. }));

        let show = vec![
            "spar".to_owned(),
            "show".to_owned(),
            "deploy".to_owned(),
            "-G".to_owned(),
        ];
        assert!(matches!(parse_args(&show), Cmd::Show { global: true, .. }));

        let dump = vec!["spar".to_owned(), "dump".to_owned(), "--global".to_owned()];
        assert!(matches!(parse_args(&dump), Cmd::Dump { global: true, .. }));

        let conflicting = vec![
            "spar".to_owned(),
            "tasks".to_owned(),
            "-G".to_owned(),
            "-f".to_owned(),
            "Tasks.spar".to_owned(),
        ];
        assert!(matches!(
            parse_args(&conflicting),
            Cmd::BadArgs(message)
                if message == "global flag and file flag may not be used together"
        ));
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
                ..
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
        assert!(matches!(parse_args(&dump), Cmd::Dump { path: None, .. }));
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
        let src = "@SchemaFile\nSchema [X]{ a: int; };\n";
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
