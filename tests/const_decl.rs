//! `const`: compile-time constants, validation, folding, formatting, config emit.
use spar::{CompileOptions, Engine};

fn run(source: &str) -> Result<i32, String> {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

fn errors_of(source: &str) -> String {
    run(source).unwrap_err()
}

#[test]
fn top_level_and_local_consts_compute_and_chain() {
    let source = r#"
        const MAX: int = 10 * 4 + 2;
        const limit: int = MAX - 2;
        const label: str = "a" + "b";
        const ratio: float = 7.5 % 2.0;
        const on: bool = MAX > 40 && !(limit == 0);
        fn f(n: int) -> int {
            const local: int = limit % 7;
            var mut x: int = n;
            if on { x = x + local + MAX; }
            if label == "ab" && ratio == 1.5 { x = x + 1000; }
            return x;
        };
        fn main() -> int { return f(n: 1); };
    "#;
    // 40 % 7 = 5; 1 + 5 + 42 + 1000
    assert_eq!(run(source), Ok(1048));
}

#[test]
fn a_local_or_parameter_shadows_a_top_level_const() {
    let source = r#"
        const base: int = 100;
        fn shadowed(base: int) -> int { return base + 1; };
        fn inner() -> int {
            const base: int = 5;
            return base;
        };
        fn viaVar() -> int {
            var base: int = 7;
            return base;
        };
        fn main() -> int { return shadowed(base: 1) * 1000 + inner() * 100 + viaVar() * 10 + base - 100; };
    "#;
    assert_eq!(run(source), Ok(2000 + 500 + 70));
}

#[test]
fn constants_fold_to_literal_operands_in_the_vm() {
    let source = "const base: int = 40 + 2;\nfn f(n: int) -> int { return n + base; };\nfn main() -> int { return f(n: 1); };\n";
    let program = Engine::default().compile_source(source).expect("compiles");
    let listing = program.disassemble();
    assert!(listing.contains("AddII { dst: 1, a: 0, imm: 42 }") || listing.contains("imm: 42"), "{listing}");
    assert!(!listing.contains("LoadGlobal"), "{listing}");
}

#[test]
fn const_cannot_be_assigned_or_mut() {
    assert!(errors_of("const a: int = 1;\nfn main() -> int { a = 2; return a; };").contains("cannot assign to immutable binding 'a'"));
    assert!(errors_of("fn main() -> int { const a: int = 1; a = 2; return a; };").contains("cannot assign to immutable binding 'a'"));
    assert!(errors_of("const mut a: int = 1;\nfn main() -> int { return 0; };").contains("`const` bindings cannot be `mut`"));
}

#[test]
fn const_initializers_must_be_compile_time_constants() {
    for (body, needle) in [
        ("fn g() -> int { return 1; };\nconst A: int = g();\nfn main() -> int { return A; };", "calls are not allowed in a const initializer"),
        ("var z: int = 1;\nconst A: int = z;\nfn main() -> int { return A; };", "'z' is not a constant"),
        ("const A: int = 1 / 0;\nfn main() -> int { return A; };", "division by zero in constant expression"),
        ("const A: int = 9223372036854775807 + 1;\nfn main() -> int { return 0; };", "integer overflow in constant addition"),
        ("const A: int = B;\nconst B: int = A;\nfn main() -> int { return 0; };", "depends on itself"),
        ("const A: str = \"x${1}\";\nfn main() -> int { return 0; };", "interpolation is not allowed"),
        ("fn main() -> int { var q: int = 2; const A: int = q; return A; };", "'q' is not a constant"),
        ("const A: int;\nfn main() -> int { return 0; };", "needs an initializer"),
    ] {
        let err = errors_of(body);
        assert!(err.contains(needle), "{needle}: {err}");
    }
}

#[test]
fn const_type_mismatch_is_still_a_type_error() {
    let err = errors_of("const A: int = \"x\";\nfn main() -> int { return 0; };");
    assert!(err.to_lowercase().contains("type"), "{err}");
}

#[test]
fn both_camel_case_and_screaming_snake_case_are_accepted_but_not_other_shapes() {
    assert!(run("const MAX_RETRIES: int = 3;\nconst maxRetries: int = 3;\nfn main() -> int { return MAX_RETRIES + maxRetries; };") == Ok(6));
    let err = errors_of("const Max_Retries: int = 3;\nfn main() -> int { return 0; };");
    assert!(err.contains("camelCase"), "{err}");
    // plain `var` still has to be camelCase
    let err = errors_of("var MAX: int = 3;\nfn main() -> int { return 0; };");
    assert!(err.contains("camelCase"), "{err}");
}

#[test]
fn consts_work_in_config_emit_and_loops() {
    let engine = Engine::new(CompileOptions::default());
    let compilation = engine.emit_source("const PORT: int = 8000 + 80;\n#[emit]\nstruct Cfg { port: int = PORT; };");
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let json = spar::emit::build_emit_json(
        compilation.result.as_ref().unwrap(),
        compilation.symbols.as_ref().unwrap(),
    )
    .unwrap()
    .to_string();
    assert!(json.contains("\"port\":8080"), "{json}");
    let source = "const STEP: int = 3;\nfn main() -> int { var mut i: int = 0; var mut n: int = 0; while i < 30 { i = i + STEP; n = n + 1; } return n; };";
    assert_eq!(run(source), Ok(10));
}

#[test]
fn formatter_keeps_const() {
    let source = "export const MAX: int = 1;\nconst local: str = \"a\";\n\nfn f() -> int {\n    const inner: int = 2;\n    return inner;\n};\n";
    let formatted = spar::formatter::format_source(source).unwrap();
    assert!(formatted.contains("export const MAX: int = 1;"), "{formatted}");
    assert!(formatted.contains("    const inner: int = 2;"), "{formatted}");
    assert_eq!(spar::formatter::format_source(&formatted).unwrap(), formatted);
}
