use spar::runner::{
    BoundValue, ExecutionOptions, RunnerError, ScalarKind, TaskInvocation, TaskSet,
};
use spar::{
    renderer::ErrorRenderer, Compilation, CompileOptions, Compiler, ConfigValue, EmitFormat,
    Engine, Evaluator,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Builds the `ExprEval` callback `runner::execute`/`CommandTemplate::render`
/// use to resolve a `TemplatePart::Expr` (a `${...}` interpolation that
/// mixes a task parameter with other values) at task-run time, once
/// parameter values are bound. Reuses the already-computed `Evaluator`
/// result the rest of `compilation` was built from.
fn task_expr_evaluator(
    compilation: &Compilation,
) -> impl Fn(usize, &BTreeMap<String, BoundValue>) -> Result<String, String> + '_ {
    let program = compilation.program.as_ref().expect("compiled program");
    let symbols = compilation.symbols.as_ref().expect("compiled symbols");
    let eval_result = compilation.result.as_ref().expect("compiled eval result");
    let entries = &compilation.task_exprs;
    move |id, values| {
        let entry = entries
            .get(id)
            .ok_or_else(|| format!("internal error: unknown task expression #{id}"))?;
        let mut local_scope = HashMap::new();
        for (name, kind) in &entry.param_kinds {
            if let Some(value) = values.get(name) {
                local_scope.insert(name.clone(), bound_to_config(value, *kind));
            }
        }
        Evaluator::eval_standalone(program, symbols, eval_result, &entry.expr, &local_scope)
            .map(|v| v.coerce_to_str())
            .map_err(|e| e.to_string())
    }
}

/// Builds the `NativeEval` callback `runner::execute_with_native` uses to
/// run a native `run { }` block: binds the task's parameters as locals,
/// evaluates the block's `Expr::Shell` to a `ShellPlan`, and executes it
/// through `spar-process`. `task_exprs` holds the block's expression (the
/// same table parameter-dependent `${...}` interpolations use).
#[allow(clippy::type_complexity)]
fn native_block_runner(
    compilation: &Compilation,
) -> impl Fn(
    usize,
    &BTreeMap<String, BoundValue>,
    &BTreeMap<String, String>,
    Option<&Path>,
) -> Result<i32, String>
       + '_ {
    let program = compilation.program.as_ref().expect("compiled program");
    let symbols = compilation.symbols.as_ref().expect("compiled symbols");
    let eval_result = compilation.result.as_ref().expect("compiled eval result");
    let entries = &compilation.task_exprs;
    move |id, values, environment, cwd| {
        let entry = entries
            .get(id)
            .ok_or_else(|| format!("internal error: unknown native block #{id}"))?;
        let mut local_scope = HashMap::new();
        for (name, kind) in &entry.param_kinds {
            if let Some(value) = values.get(name) {
                local_scope.insert(name.clone(), bound_to_config(value, *kind));
            }
        }
        let (value, requested_exit) = Evaluator::eval_task_block(
            program,
            symbols,
            eval_result,
            &entry.expr,
            &local_scope,
            environment,
        )
        .map_err(|e| e.to_string())?;
        let ConfigValue::Shell(plan) = value else {
            return Err("native run block did not evaluate to a shell plan".to_owned());
        };
        let mut options = spar_process::ExecutionOptions::default();
        let mut merged: Vec<(std::ffi::OsString, std::ffi::OsString)> =
            std::env::vars_os().collect();
        merged.extend(
            environment
                .iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        options.environment = Some(merged);
        // `spar-process` has no working-directory option, so switch the
        // process directory around the call. The runner is sequential.
        let previous = std::env::current_dir().ok();
        if let Some(cwd) = cwd {
            std::env::set_current_dir(cwd).map_err(|e| e.to_string())?;
        }
        let outcome = spar::evaluator::execute_shell_plan_with_options(&plan, &options);
        if let Some(previous) = previous {
            let _ = std::env::set_current_dir(previous);
        }
        // `exit(code: N)` decides the block's status once its queued commands
        // have run.
        outcome
            .map(|o| requested_exit.unwrap_or(o.exit_code))
            .map_err(|e| e.to_string())
    }
}

fn bound_to_config(value: &BoundValue, kind: ScalarKind) -> ConfigValue {
    match value {
        BoundValue::Variadic(items) => {
            ConfigValue::List(items.iter().cloned().map(ConfigValue::Str).collect())
        }
        BoundValue::Scalar(s) => match kind {
            ScalarKind::Str => ConfigValue::Str(s.clone()),
            ScalarKind::Int => s
                .parse()
                .map(ConfigValue::Int)
                .unwrap_or_else(|_| ConfigValue::Str(s.clone())),
            ScalarKind::Float => s
                .parse()
                .map(ConfigValue::Float)
                .unwrap_or_else(|_| ConfigValue::Str(s.clone())),
            ScalarKind::Bool => s
                .parse()
                .map(ConfigValue::Bool)
                .unwrap_or_else(|_| ConfigValue::Str(s.clone())),
        },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match parse_args(&args) {
        Cmd::Check(path) => cmd_check(&path),
        Cmd::Emit { path, format } => cmd_emit(&path, format),
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
        Cmd::Exec { path, program_args } => cmd_exec(&path, program_args),
        Cmd::Repl => cmd_repl(),
        Cmd::PackageInit { name, kind } => cmd_package_init(name, kind),
        Cmd::PackageAdd { alias, request } => cmd_package_add(&alias, &request),
        Cmd::PackageRemove { alias } => cmd_package_remove(&alias),
        Cmd::PackageInstall { offline } => cmd_package_install(offline),
        Cmd::PackageUpdate { alias } => cmd_package_update(alias),
        Cmd::PackageTree => cmd_package_tree(),
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
    Emit {
        path: String,
        format: EmitFormat,
    },
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
    Exec {
        path: String,
        program_args: Vec<String>,
    },
    Repl,
    PackageInit {
        name: Option<String>,
        kind: spar::package::PackageKind,
    },
    PackageAdd {
        alias: String,
        request: String,
    },
    PackageRemove {
        alias: String,
    },
    PackageInstall {
        offline: bool,
    },
    PackageUpdate {
        alias: Option<String>,
    },
    PackageTree,
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
        Some("emit") => parse_emit_args(&args[2..]),
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
        Some("exec") => parse_exec_args(&args[2..]),
        Some("repl") => Cmd::Repl,
        Some("init") => parse_package_init_args(&args[2..]),
        Some("add") => parse_package_add_args(&args[2..]),
        Some("remove") => parse_package_remove_args(&args[2..]),
        Some("install") => parse_package_install_args(&args[2..]),
        Some("update") => parse_package_update_args(&args[2..]),
        Some("tree") => Cmd::PackageTree,
        Some("--help") | Some("-h") | Some("help") => Cmd::Help,
        Some("--version") | Some("-V") | Some("version") => Cmd::Version,
        // Script-execution shorthand: `spar ./foo.spar` or `spar foo.spar` runs
        // `main` in that file, exactly like `spar exec foo.spar`, when the name
        // looks path-like (a `.spar` suffix or an explicit path separator/`.`
        // prefix) and really does name an existing file — so a task that just
        // happens to share a name with some unrelated file in cwd isn't
        // accidentally swallowed by this shorthand.
        Some(name)
            if (name.ends_with(".spar") || name.contains('/') || name.starts_with('.'))
                && Path::new(name).is_file() =>
        {
            Cmd::Exec {
                path: name.to_string(),
                program_args: args[2..].to_vec(),
            }
        }
        // Anything else is treated as a task-runner shorthand: `spar <name> [args...]`
        // is exactly `spar run <name> [args...]`. Task-name validity (does this task
        // even exist?) is checked later, once a task file is actually loaded.
        Some(_) => parse_run_args(&args[1..]),
        None => Cmd::Help,
    }
}

fn parse_exec_args(args: &[String]) -> Cmd {
    let Some(path) = args.first() else {
        return Cmd::BadArgs("`exec` requires a file path".into());
    };
    // Everything after the path is a program argument; an optional leading
    // `--` (the documented CLI/program-argument boundary) is dropped rather
    // than passed through literally.
    let rest = &args[1..];
    let program_args = match rest.first().map(String::as_str) {
        Some("--") => rest[1..].to_vec(),
        _ => rest.to_vec(),
    };
    Cmd::Exec {
        path: path.clone(),
        program_args,
    }
}

fn parse_package_init_args(args: &[String]) -> Cmd {
    let mut name = None;
    let mut kind = spar::package::PackageKind::Application;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--lib" | "--library" => {
                kind = spar::package::PackageKind::Library;
                index += 1;
            }
            "--config" => {
                kind = spar::package::PackageKind::Config;
                index += 1;
            }
            "--app" | "--application" => {
                kind = spar::package::PackageKind::Application;
                index += 1;
            }
            other if name.is_none() && !other.starts_with('-') => {
                name = Some(other.to_string());
                index += 1;
            }
            other => return Cmd::BadArgs(format!("unexpected argument for `init`: {other}")),
        }
    }
    Cmd::PackageInit { name, kind }
}

fn parse_package_add_args(args: &[String]) -> Cmd {
    match (args.first(), args.get(1)) {
        (Some(alias), Some(request)) if args.len() == 2 => Cmd::PackageAdd {
            alias: alias.clone(),
            request: request.clone(),
        },
        _ => Cmd::BadArgs(
            "`add` requires an alias and a dependency request: `spar add <alias> <request>`".into(),
        ),
    }
}

fn parse_package_remove_args(args: &[String]) -> Cmd {
    match args.first() {
        Some(alias) if args.len() == 1 => Cmd::PackageRemove {
            alias: alias.clone(),
        },
        _ => Cmd::BadArgs("`remove` requires exactly one alias".into()),
    }
}

fn parse_package_install_args(args: &[String]) -> Cmd {
    match args {
        [] => Cmd::PackageInstall { offline: false },
        [flag] if flag == "--offline" => Cmd::PackageInstall { offline: true },
        _ => Cmd::BadArgs("`install` accepts only an optional `--offline` flag".into()),
    }
}

fn parse_package_update_args(args: &[String]) -> Cmd {
    match args {
        [] => Cmd::PackageUpdate { alias: None },
        [alias] => Cmd::PackageUpdate {
            alias: Some(alias.clone()),
        },
        _ => Cmd::BadArgs("`update` accepts at most one dependency alias".into()),
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
                return Cmd::BadArgs(format!("unknown option: {other}"));
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

fn parse_emit_args(args: &[String]) -> Cmd {
    let mut path = None;
    let mut format = None;
    for arg in args {
        match arg.as_str() {
            "-j" | "--json" => {
                if format.is_some() {
                    return Cmd::BadArgs("`emit` accepts only one format flag".into());
                }
                format = Some(EmitFormat::Json);
            }
            "-y" | "--yaml" => {
                if format.is_some() {
                    return Cmd::BadArgs("`emit` accepts only one format flag".into());
                }
                format = Some(EmitFormat::Yaml);
            }
            "-t" | "--toml" => {
                if format.is_some() {
                    return Cmd::BadArgs("`emit` accepts only one format flag".into());
                }
                format = Some(EmitFormat::Toml);
            }
            other if path.is_none() => path = Some(other.to_owned()),
            other => return Cmd::BadArgs(format!("unexpected argument for `emit`: {other}")),
        }
    }
    let Some(path) = path else {
        return Cmd::BadArgs("`emit` requires a file path".into());
    };
    Cmd::Emit {
        path,
        format: format.unwrap_or(EmitFormat::Json),
    }
}

fn print_help() {
    println!(
        "spar — scripting language v{ver}

USAGE:
    spar <task> [args...] [OPTIONS]     Shorthand for `spar run <task> [args...]`
    spar <COMMAND> [OPTIONS]

COMMANDS:
    <task>        [args...] [-f FILE | -G] [--dry-run] [--choose]
                                        Shorthand for `run <task>` — any name that isn't a
                                        command below is treated as a task name
    check         <file.spar>           Validate a .spar file — runs lex, parse, resolve, and type check
    emit          <file.spar> [-j|-y|-t]
                                        Evaluate and print the config to stdout as JSON (default), YAML (-y/--yaml), or TOML (-t/--toml)
    fmt           <file.spar>           Format a .spar file in place
    fmt --check   <file.spar>           Exit non-zero if file is not already formatted
    tasks         [-f FILE | -G] [--all]
                                        List declared tasks
    run           [task] [args...] [-f FILE | -G] [--dry-run] [--choose]
                                        Run a task (default task if omitted); same as bare `spar <task>`
    show          <task> [args...] [-f FILE | -G]
                                        Show one task's resolved commands
    dump          [-f FILE | -G]        Dump the lowered task catalog as JSON
    exec          <file.spar> [-- args...]
                                        Run file.spar's `main` and exit with its status
    repl                                Start an interactive scripting session
    init          [name] [--app|--lib|--config]
                                        Create spar.package.spar in the current directory
    add           <alias> <request>     Add/update a dependency, resolve, and lock it
    remove        <alias>               Remove a dependency and re-lock
    install       [--offline]           Materialize every locked dependency into the store
    update        [alias]               Re-resolve one dependency, or all of them
    tree                                Print the locked dependency tree

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
    spar emit  server.spar -y > config.yaml
    spar emit  server.spar -t > config.toml
    spar fmt   server.spar
    spar fmt --check server.spar
    spar tasks -f server.spar
    spar run -f server.spar
    spar run deploy production -f server.spar
    spar run test --dry-run -f server.spar
    spar deploy production -f server.spar   (same as `run` above)
    spar test --dry-run -f server.spar      (same as `run` above)
    spar exec  app.spar
    spar exec  app.spar -- arg1 arg2
    spar ./app.spar                         (same as `exec` above)
    spar repl",
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

/// Builds offline compile options for a source path. When the source belongs
/// to a Spar package project with an existing lockfile, explicit `import pkg`
/// requests resolve through that lock and the durable global store. No provider/network code
/// is involved in ordinary language or task commands.
fn compile_options_for_path(path: &Path) -> Result<CompileOptions, String> {
    let mut options = CompileOptions::for_path(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot determine current directory: {error}"))?
            .join(path)
    };
    let Some(project_dir) = absolute.parent().and_then(|parent| {
        parent
            .ancestors()
            .find(|directory| directory.join("spar.package.spar").is_file())
    }) else {
        return Ok(options);
    };
    let lock_path = project_dir.join(spar::package::PACKAGE_LOCK_FILE);
    if lock_path.is_file() {
        let lockfile =
            spar::package::Lockfile::read(&lock_path).map_err(|error| error.to_string())?;
        let store = spar::package::PackageStore::new(spar::package::StorePaths::from_env());
        options.locator = Some(spar::package::ModuleLocator::for_root(lockfile, store));
    }
    Ok(options)
}

fn compile_options_for_path_or_exit(path: &Path) -> CompileOptions {
    compile_options_for_path(path).unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    })
}

// ── `check` command ───────────────────────────────────────────────────────────

fn cmd_check(path: &str) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let options = CompileOptions {
        evaluate: false,
        ..compile_options_for_path_or_exit(Path::new(path))
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

fn cmd_emit(path: &str, format: EmitFormat) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let options = CompileOptions {
        allow_schema_file: false,
        ..compile_options_for_path_or_exit(Path::new(path))
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
    let value = spar::emit::build_emit_json(result, symbols).unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });
    let rendered = match format {
        EmitFormat::Json => serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()),
        EmitFormat::Yaml => serde_yaml::to_string(&value).unwrap_or_else(|e| {
            eprintln!("error: failed to render YAML: {e}");
            std::process::exit(1);
        }),
        EmitFormat::Toml => toml::to_string_pretty(&value).unwrap_or_else(|e| {
            eprintln!("error: failed to render TOML: {e}");
            std::process::exit(1);
        }),
    };
    print!("{rendered}");
    if format == EmitFormat::Json {
        println!();
    }
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
    let compilation = Compiler::new(compile_options_for_path_or_exit(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    if let Some(tasks) = compilation.tasks.as_ref() {
        for warning in tasks.reserved_name_warnings() {
            eprintln!("warning: {warning}");
        }
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
    let options = compile_options_for_path_or_exit(&path);
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
    for warning in tasks.reserved_name_warnings() {
        eprintln!("warning: {warning}");
    }

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
    let exprs = task_expr_evaluator(&compilation);
    let native = native_block_runner(&compilation);
    if let Err(e) = spar::runner::execute_with_native(&plan, &exec_options, &exprs, &native) {
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
    let compilation = Compiler::new(compile_options_for_path_or_exit(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    let Some(tasks) = compilation.tasks.as_ref() else {
        eprintln!("error: {} declares no tasks", path.display());
        std::process::exit(1);
    };
    for warning in tasks.reserved_name_warnings() {
        eprintln!("warning: {warning}");
    }
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
    let exprs = task_expr_evaluator(&compilation);
    for command in &bound.task.commands {
        match command.render(&bound.parameter_values, &exprs) {
            Ok(rendered) => print_resolved_command(&rendered, color, &mut output),
            Err(message) => {
                eprintln!("error: {message}");
                std::process::exit(1);
            }
        }
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
    let compilation = Compiler::new(compile_options_for_path_or_exit(&path)).compile(&src);
    if !compilation.errors.is_empty() {
        eprintln!("{}", renderer.render_all(&compilation.errors));
        std::process::exit(1);
    }
    if let Some(tasks) = compilation.tasks.as_ref() {
        for warning in tasks.reserved_name_warnings() {
            eprintln!("warning: {warning}");
        }
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

// ── `exec` command ────────────────────────────────────────────────────────────

fn cmd_exec(path: &str, program_args: Vec<String>) {
    // Phase 0's `main` takes no parameters and there's no language-level API
    // to read `program_args` yet (spec section 16) — accepted and parsed for
    // the `--` boundary now so scripts/tooling can rely on it, exposed to
    // Spar source itself once the runtime foundation grows one.
    let _ = program_args;
    match Engine::new(compile_options_for_path_or_exit(Path::new(path)))
        .execute_path(Path::new(path))
    {
        Ok(outcome) => std::process::exit(outcome.exit_status),
        Err(errors) => {
            let src = read_file(path);
            let renderer = make_renderer(&src, path);
            eprintln!("{}", renderer.render_all(&errors));
            std::process::exit(1);
        }
    }
}

// ── `repl` command ────────────────────────────────────────────────────────────

fn cmd_repl() {
    let mut session = Engine::default().session();
    let interactive = std::io::stdin().is_terminal();
    let mut input = std::io::stdin().lock();
    let mut buffer = String::new();
    loop {
        if interactive {
            eprint!(
                "{}",
                if buffer.is_empty() {
                    "spar> "
                } else {
                    "....> "
                }
            );
            let _ = std::io::stderr().flush();
        }
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error — exit cleanly either way
            Ok(_) => {}
        }
        buffer.push_str(&line);
        if repl_fragment_complete(&buffer) {
            let fragment = std::mem::take(&mut buffer);
            if let Err(errors) = session.eval(&fragment) {
                let renderer = make_renderer(&fragment, "<repl>");
                eprintln!("{}", renderer.render_all(&errors));
            }
        }
    }
    std::process::exit(0);
}

/// A REPL fragment is ready to evaluate once its brace/bracket/paren nesting
/// (ignoring string contents) returns to zero and it ends with the `;` every
/// top-level Spar declaration/statement requires — so a multi-line function
/// or task body keeps prompting for more input instead of being evaluated
/// one line at a time.
fn repl_fragment_complete(buffer: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for c in buffer.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    !in_string && depth <= 0 && buffer.trim_end().ends_with(';')
}

// ── package commands ─────────────────────────────────────────────────────────

fn package_store() -> spar::package::PackageStore {
    spar::package::PackageStore::new(spar::package::StorePaths::from_env())
}

fn package_project_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        std::process::exit(1);
    })
}

fn package_exit_on_error<T>(result: Result<T, spar::package::PackageError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn cmd_package_init(name: Option<String>, kind: spar::package::PackageKind) {
    let dir = package_project_dir();
    let name = name.unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".to_string())
    });
    let manifest = package_exit_on_error(spar::package::commands::init(&dir, &name, kind));
    println!(
        "created spar.package.spar for '{}' ({})",
        manifest.name,
        kind.as_str()
    );
}

fn cmd_package_add(alias: &str, request: &str) {
    let dir = package_project_dir();
    let provider = spar::package::GitCommandProvider::default();
    let store = package_store();
    let lockfile = package_exit_on_error(spar::package::commands::add(
        &dir,
        alias,
        request,
        &provider,
        spar::package::NetworkPolicy::Allow,
        &store,
    ));
    println!(
        "added '{alias}' — {} package(s) locked",
        lockfile.packages.len()
    );
}

fn cmd_package_remove(alias: &str) {
    let dir = package_project_dir();
    let provider = spar::package::GitCommandProvider::default();
    let store = package_store();
    let lockfile = package_exit_on_error(spar::package::commands::remove(
        &dir,
        alias,
        &provider,
        spar::package::NetworkPolicy::Allow,
        &store,
    ));
    println!(
        "removed '{alias}' — {} package(s) locked",
        lockfile.packages.len()
    );
}

fn cmd_package_install(offline: bool) {
    let dir = package_project_dir();
    let provider = spar::package::GitCommandProvider::default();
    let store = package_store();
    let network = if offline {
        spar::package::NetworkPolicy::Offline
    } else {
        spar::package::NetworkPolicy::Allow
    };
    let lockfile = package_exit_on_error(spar::package::commands::install(
        &dir, &provider, network, &store,
    ));
    println!("installed — {} package(s) locked", lockfile.packages.len());
}

fn cmd_package_update(alias: Option<String>) {
    let dir = package_project_dir();
    let provider = spar::package::GitCommandProvider::default();
    let store = package_store();
    let lockfile = package_exit_on_error(spar::package::commands::update(
        &dir,
        alias.as_deref(),
        &provider,
        &store,
    ));
    println!("updated — {} package(s) locked", lockfile.packages.len());
}

fn cmd_package_tree() {
    let dir = package_project_dir();
    let output = package_exit_on_error(spar::package::commands::tree(&dir));
    if output.is_empty() {
        println!("no dependencies (no {})", spar::package::PACKAGE_LOCK_FILE);
    } else {
        print!("{output}");
    }
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
                spar::runner::TaskCommand::Bash(_) => "bash",
                spar::runner::TaskCommand::BashScript(_) => "bash-script",
                spar::runner::TaskCommand::Native(_) => "native",
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
        // Keys only. Values come from `.env` files and `env:` blocks and are
        // often secrets; a catalog dump must never print them.
        "environment": task
            .environment
            .keys()
            .map(|key| (key.clone(), serde_json::Value::from("<redacted>")))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        "cwd": task.cwd.as_ref().map(|path| path.display().to_string()),
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
        std::fs::write(&parent_file, "task Build { run { true; }; };").unwrap();

        assert_eq!(discover_task_file(&nested).unwrap(), parent_file);

        let current_file = nested.join("SparMake.spar");
        std::fs::write(&current_file, "task Build { run { true; }; };").unwrap();
        assert_eq!(discover_task_file(&nested).unwrap(), current_file);
    }

    #[test]
    fn global_task_file_resolves_below_the_given_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let spar_directory = home.path().join(".spar");
        std::fs::create_dir(&spar_directory).unwrap();
        let task_file = spar_directory.join("SparMake.spar");
        std::fs::write(&task_file, "task Build { run { true; }; };").unwrap();

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
task Build { description: "Compile"; run { true; }; };
task Deploy { group: "release"; description: "Ship it"; run { true; }; };
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
task Build { description: "Compile"; run { true; }; };
task Deploy { group: "release"; description: "Ship it"; run { true; }; };
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
    fn parse_args_bare_word_is_shorthand_for_run() {
        let args = vec![
            "spar".to_owned(),
            "deploy".to_owned(),
            "production".to_owned(),
            "--dry-run".to_owned(),
            "-f".to_owned(),
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
                assert_eq!(args, ["production"]);
                assert!(dry_run);
                assert!(!choose);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_bare_flags_with_no_task_name_still_run_default() {
        let args = vec!["spar".to_owned(), "--dry-run".to_owned()];
        assert!(matches!(
            parse_args(&args),
            Cmd::Run {
                task: None,
                dry_run: true,
                ..
            }
        ));
    }

    #[test]
    fn parse_args_reserved_keywords_win_over_bare_task_dispatch() {
        // `run` is excluded: `spar run` with no further args legitimately
        // produces Cmd::Run — that's the real `run` subcommand, not ambiguity.
        for keyword in ["check", "emit", "fmt", "tasks", "show", "dump"] {
            let args = vec!["spar".to_owned(), keyword.to_owned()];
            assert!(
                !matches!(parse_args(&args), Cmd::Run { .. }),
                "`{keyword}` must not be treated as a bare task name"
            );
        }
    }

    #[test]
    fn parse_args_bare_help_and_version_words_still_work() {
        assert!(matches!(
            parse_args(&["spar".to_owned(), "help".to_owned()]),
            Cmd::Help
        ));
        assert!(matches!(
            parse_args(&["spar".to_owned(), "version".to_owned()]),
            Cmd::Version
        ));
    }

    #[test]
    fn parse_args_no_args_is_help() {
        assert!(matches!(parse_args(&["spar".to_owned()]), Cmd::Help));
    }

    #[test]
    fn parse_args_explicit_exec_requires_a_path() {
        assert!(matches!(
            parse_args(&["spar".to_owned(), "exec".to_owned()]),
            Cmd::BadArgs(_)
        ));
    }

    #[test]
    fn parse_args_exec_preserves_arguments_after_double_dash() {
        let args = ["spar", "exec", "app.spar", "--", "one", "two", "--flag"].map(str::to_owned);
        match parse_args(&args) {
            Cmd::Exec { path, program_args } => {
                assert_eq!(path, "app.spar");
                assert_eq!(program_args, vec!["one", "two", "--flag"]);
            }
            other => panic!("expected Cmd::Exec, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_exec_without_double_dash_still_collects_trailing_args() {
        let args = ["spar", "exec", "app.spar", "one"].map(str::to_owned);
        match parse_args(&args) {
            Cmd::Exec { path, program_args } => {
                assert_eq!(path, "app.spar");
                assert_eq!(program_args, vec!["one"]);
            }
            other => panic!("expected Cmd::Exec, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_bare_repl_dispatches_to_repl() {
        assert!(matches!(
            parse_args(&["spar".to_owned(), "repl".to_owned()]),
            Cmd::Repl
        ));
    }

    #[test]
    fn parse_args_existing_spar_path_beats_task_shorthand() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.spar");
        std::fs::write(&file, "function main() -> int { return 0; };").unwrap();
        let args = vec!["spar".to_owned(), file.to_string_lossy().into_owned()];
        match parse_args(&args) {
            Cmd::Exec { path, .. } => assert_eq!(path, file.to_string_lossy()),
            other => panic!("expected Cmd::Exec for an existing .spar path, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_nonexistent_spar_looking_name_falls_back_to_task_shorthand() {
        // No such file on disk — must NOT be treated as script shorthand,
        // even though the name ends in `.spar`.
        let args = vec!["spar".to_owned(), "definitely-missing.spar".to_owned()];
        assert!(matches!(parse_args(&args), Cmd::Run { .. }));
    }

    #[test]
    fn parse_args_run_can_still_name_a_task_literally_called_exec() {
        let args = vec!["spar".to_owned(), "run".to_owned(), "exec".to_owned()];
        assert!(matches!(
            parse_args(&args),
            Cmd::Run { task: Some(name), .. } if name == "exec"
        ));
    }

    #[test]
    fn package_commands_are_reserved_but_explicit_run_can_use_same_task_name() {
        for name in ["init", "add", "remove", "install", "update", "tree"] {
            assert!(
                !matches!(
                    parse_args(&[String::from("spar"), name.to_owned()]),
                    Cmd::Run { .. }
                ),
                "`{name}` must dispatch as the package command, not a bare task name"
            );
            assert!(matches!(
                parse_args(&[String::from("spar"), "run".to_owned(), name.to_owned()]),
                Cmd::Run { task: Some(task), .. } if task == name
            ));
        }
    }

    #[test]
    fn parse_args_install_offline_sets_network_policy_offline() {
        assert!(matches!(
            parse_args(&[
                String::from("spar"),
                "install".to_owned(),
                "--offline".to_owned()
            ]),
            Cmd::PackageInstall { offline: true }
        ));
        assert!(matches!(
            parse_args(&[String::from("spar"), "install".to_owned()]),
            Cmd::PackageInstall { offline: false }
        ));
    }

    #[test]
    fn parse_args_add_requires_alias_and_request() {
        assert!(matches!(
            parse_args(&[String::from("spar"), "add".to_owned()]),
            Cmd::BadArgs(_)
        ));
        match parse_args(&[
            String::from("spar"),
            "add".to_owned(),
            "http".to_owned(),
            "github:owner/http@1.0.0".to_owned(),
        ]) {
            Cmd::PackageAdd { alias, request } => {
                assert_eq!(alias, "http");
                assert_eq!(request, "github:owner/http@1.0.0");
            }
            other => panic!("expected Cmd::PackageAdd, got {other:?}"),
        }
    }

    #[test]
    fn repl_fragment_completion_waits_for_balanced_braces_and_trailing_semicolon() {
        assert!(!repl_fragment_complete("var x: int = 1"));
        assert!(repl_fragment_complete("var x: int = 1;"));
        assert!(!repl_fragment_complete("function f() -> int {"));
        assert!(repl_fragment_complete(
            "function f() -> int {\n    return 1;\n};"
        ));
        // A `;` inside a string must not be mistaken for the statement
        // terminator.
        assert!(!repl_fragment_complete("var x: str = \"a;b\""));
        assert!(repl_fragment_complete("var x: str = \"a;b\";"));
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
        build_emit_json(&result, &symbols).unwrap()
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
