use std::path::Path;
use spar::{
    evaluator::Evaluator,
    loader::LoadedImport,
    renderer::ErrorRenderer,
    SparError, Lexer, Parser,
    resolver::{Resolver, SymbolTable},
    typechecker::TypeChecker,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match parse_args(&args) {
        Cmd::Check(path)         => cmd_check(&path),
        Cmd::Emit(path)          => cmd_emit(&path),
        Cmd::Fmt { path, check } => cmd_fmt(&path, check),
        Cmd::Help                => print_help(),
        Cmd::Version             => println!("spar {}", env!("CARGO_PKG_VERSION")),
        Cmd::BadArgs(msg)        => {
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
    Fmt { path: String, check: bool },
    Help,
    Version,
    BadArgs(String),
}

fn parse_args(args: &[String]) -> Cmd {
    match args.get(1).map(String::as_str) {
        Some("check") => match args.get(2) {
            Some(p) => Cmd::Check(p.clone()),
            None    => Cmd::BadArgs("`check` requires a file path".into()),
        },
        Some("emit") => match args.get(2) {
            Some(p) => Cmd::Emit(p.clone()),
            None    => Cmd::BadArgs("`emit` requires a file path".into()),
        },
        Some("fmt") => {
            match (args.get(2).map(String::as_str), args.get(3)) {
                (Some("--check"), Some(p)) => Cmd::Fmt { path: p.clone(), check: true },
                (Some(p), None) if p != "--check" => Cmd::Fmt { path: p.to_string(), check: false },
                _ => Cmd::BadArgs("`fmt` requires a file path (optionally preceded by --check)".into()),
            }
        }
        Some("--help")    | Some("-h") | None => Cmd::Help,
        Some("--version") | Some("-V")        => Cmd::Version,
        Some(other) => Cmd::BadArgs(format!("unknown command `{other}`")),
    }
}

fn print_help() {
    println!(
        "spar — configuration language v{ver}

USAGE:
    spar <COMMAND> <FILE>

COMMANDS:
    check         <file.spar>           Validate a .spar file — runs lex, parse, resolve, and type check
    emit          <file.spar>           Evaluate and print the config as JSON to stdout
    fmt           <file.spar>           Format a .spar file in place
    fmt --check   <file.spar>           Exit non-zero if file is not already formatted

OPTIONS:
    -h, --help        Show this help
    -V, --version     Show version

ENVIRONMENT:
    NO_COLOR=1        Disable ANSI colour in error output

EXAMPLES:
    spar check server.spar
    spar emit  server.spar > config.json
    spar fmt   server.spar
    spar fmt --check server.spar",
        ver = env!("CARGO_PKG_VERSION")
    );
}

// ── Colour detection ──────────────────────────────────────────────────────────

pub fn use_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() { return false; }
    !matches!(std::env::var("TERM").as_deref(), Ok("dumb"))
}

// ── `check` command ───────────────────────────────────────────────────────────

fn cmd_check(path: &str) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let mut all_errors: Vec<SparError> = Vec::new();

    // Stage 1: Lex — single error
    let tokens = match Lexer::new(&src).tokenize() {
        Ok(t)  => t,
        Err(e) => {
            all_errors.push(e);
            eprintln!("{}", renderer.render_all(&all_errors));
            std::process::exit(1);
        }
    };

    // Stage 2: Parse — single error
    let program = match Parser::new(tokens).parse() {
        Ok(p)  => p,
        Err(e) => {
            all_errors.push(e);
            eprintln!("{}", renderer.render_all(&all_errors));
            std::process::exit(1);
        }
    };

    // Stage 2.5: Import loader — Vec<SparError>; exit before resolve if any file is missing
    let base = Path::new(path).parent().unwrap_or(Path::new("."));
    let mut loader = spar::loader::ImportLoader::new(base);
    let imports: std::collections::HashMap<String, LoadedImport> =
        match spar::loader::collect_imports(&program, &mut loader) {
            Ok(i)   => i,
            Err(es) => {
                all_errors.extend(es);
                eprintln!("{}", renderer.render_all(&all_errors));
                std::process::exit(1);
            }
        };

    // Stage 2.75: Schema validation — validate config against imported schema files
    let schema_base = Path::new(path).parent().unwrap_or(Path::new("."));
    if let Err(es) = spar::loader::validate_schema_imports(&program, schema_base) {
        all_errors.extend(es);
        eprintln!("{}", renderer.render_all(&all_errors));
        std::process::exit(1);
    }

    // Stage 3: Resolve — Vec<SparError>
    let symbols = match Resolver::resolve_with_imports(&program, &imports) {
        Ok(s)   => s,
        Err(es) => {
            all_errors.extend(es);
            eprintln!("{}", renderer.render_all(&all_errors));
            std::process::exit(1);
        }
    };

    // Stage 4: Type check — collect errors, continue to report all
    if let Err(es) = TypeChecker::check_with_imports(&program, &symbols, &imports) {
        all_errors.extend(es);
    }

    if all_errors.is_empty() {
        println!("{path}: ok");
    } else {
        eprintln!("{}", renderer.render_all(&all_errors));
        std::process::exit(1);
    }
}

// ── `emit` command ────────────────────────────────────────────────────────────

fn cmd_emit(path: &str) {
    let src = read_file(path);
    let renderer = make_renderer(&src, path);
    let mut all_errors: Vec<SparError> = Vec::new();

    // Stage 1: Lex — single error
    let tokens = match Lexer::new(&src).tokenize() {
        Ok(t)  => t,
        Err(e) => { all_errors.push(e); eprintln!("{}", renderer.render_all(&all_errors)); std::process::exit(1); }
    };

    // Stage 2: Parse — single error
    let program = match Parser::new(tokens).parse() {
        Ok(p)  => p,
        Err(e) => { all_errors.push(e); eprintln!("{}", renderer.render_all(&all_errors)); std::process::exit(1); }
    };

    // Guard: schema files cannot be emitted
    if program.is_schema_file {
        let e = SparError::SchemaError {
            message: format!(
                "`{}` is a schema file and cannot be emitted — \
                 schema files declare shape only; use `import schema` from a config file",
                path
            ),
            span: spar::Span::new(0, 0, 1, 1),
        };
        all_errors.push(e);
        eprintln!("{}", renderer.render_all(&all_errors));
        std::process::exit(1);
    }

    // Stage 2.5: Import loader — exit before resolve if any file is missing
    let base = Path::new(path).parent().unwrap_or(Path::new("."));
    let mut loader = spar::loader::ImportLoader::new(base);
    let imports: std::collections::HashMap<String, LoadedImport> =
        match spar::loader::collect_imports(&program, &mut loader) {
            Ok(i)   => i,
            Err(es) => {
                all_errors.extend(es);
                eprintln!("{}", renderer.render_all(&all_errors));
                std::process::exit(1);
            }
        };

    // Stage 2.75: Schema validation — validate config against imported schema files
    let schema_base = Path::new(path).parent().unwrap_or(Path::new("."));
    if let Err(es) = spar::loader::validate_schema_imports(&program, schema_base) {
        all_errors.extend(es);
        eprintln!("{}", renderer.render_all(&all_errors));
        std::process::exit(1);
    }

    // Stage 3: Resolve — Vec<SparError>
    let symbols = match Resolver::resolve_with_imports(&program, &imports) {
        Ok(s)   => s,
        Err(es) => { all_errors.extend(es); eprintln!("{}", renderer.render_all(&all_errors)); std::process::exit(1); }
    };

    // Stage 4: Type check — collect errors before deciding to proceed
    if let Err(es) = TypeChecker::check_with_imports(&program, &symbols, &imports) {
        all_errors.extend(es);
    }

    if !all_errors.is_empty() {
        eprintln!("{}", renderer.render_all(&all_errors));
        std::process::exit(1);
    }

    // Stage 5: Evaluate — only reached if no prior errors
    let result = match Evaluator::evaluate_with_imports_and_base(&program, &symbols, &imports, base) {
        Ok(r)   => r,
        Err(es) => { eprintln!("{}", renderer.render_all(&es)); std::process::exit(1); }
    };

    for w in &result.warnings {
        eprintln!("warning: {w}");
    }

    println!("{}", emit_json(&result, &symbols));
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

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn emit_json(result: &spar::evaluator::EvalResult, symbols: &SymbolTable) -> String {
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
            hint:    None,
            span:    spar::Span::new(4, 8, 1, 5),
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
        let args = vec!["spar".to_string(), "fmt".to_string(), "foo.spar".to_string()];
        match parse_args(&args) {
            Cmd::Fmt { path, check } => {
                assert_eq!(path, "foo.spar");
                assert!(!check);
            }
            other => panic!("expected Cmd::Fmt, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn parse_args_fmt_with_check() {
        let args = vec!["spar".to_string(), "fmt".to_string(), "--check".to_string(), "foo.spar".to_string()];
        match parse_args(&args) {
            Cmd::Fmt { path, check } => {
                assert_eq!(path, "foo.spar");
                assert!(check);
            }
            other => panic!("expected Cmd::Fmt, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn parse_args_fmt_missing_path_is_bad_args() {
        let args = vec!["spar".to_string(), "fmt".to_string()];
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
            "both type errors must be reported, got {} error(s): {:?}", errs.len(), errs
        );
    }
}

#[cfg(test)]
mod emit_tests {
   
    use spar::emit::build_emit_json;
use spar::{Lexer, Parser};
    use spar::resolver::Resolver;
    use spar::typechecker::TypeChecker;
    use spar::evaluator::Evaluator;

    fn emit_src(src: &str) -> serde_json::Value {
        let tokens  = Lexer::new(src).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        TypeChecker::check(&program, &symbols).unwrap();
        let result  = Evaluator::evaluate(&program, &symbols).unwrap();
        build_emit_json(&result, &symbols)
    }

    #[test]
    fn plain_var_not_in_emit_output() {
        let json = emit_src(r#"var secret: str = "hidden";"#);
        assert!(json.get("secret").is_none(), "plain var must not appear in emit output");
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
        assert!(json.get("Defaults").is_none(), "private section must not appear in emit");
    }

    #[test]
    fn private_section_still_resolvable_by_public_section() {
        let src = r#"
private [Defaults]{ timeout: int = 30; };
[Server]{ timeout: int = Defaults::timeout; };
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
        assert!(json.get("manual").is_none(), "nested section must NOT appear at root");
        assert_eq!(json["MetaData"]["manual"]["author"], "occ");
    }

    #[test]
    fn schema_file_is_rejected_by_parser_not_emit() {
        // The parser already sets is_schema_file=true; the emit path checks this.
        let src = "@SchemaFile\n[X]<Schema>{ a: int; }\n";
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
    version: int = Man::aster;
    askter:  bool = false;
    manual: section = {
        main: str = "MainMan";
        more: section = { see: int = 5; };
        options: [str] = options;
    };
};
"#;
        let json = emit_src(src);

        assert!(json.get("options").is_none(), "plain var 'options' must be hidden");
        assert_eq!(json["Man"]["aster"], 6);
        assert_eq!(json["MetaData"]["tool"],   "stackforge");
        assert_eq!(json["MetaData"]["version"], 6);
        assert_eq!(json["MetaData"]["askter"], false);
        assert_eq!(json["MetaData"]["manual"]["main"], "MainMan");
        assert_eq!(json["MetaData"]["manual"]["more"]["see"], 5);
        assert_eq!(
            json["MetaData"]["manual"]["options"],
            serde_json::json!(["one","two","three"])
        );
    }
}
