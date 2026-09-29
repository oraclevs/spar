//! `%`: precedence, type rules, formatting, the config evaluator.
use spar::{CompileOptions, Engine};

fn run(source: &str) -> Result<i32, String> {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

#[test]
fn modulo_binds_like_multiplication() {
    // 2 + 7 % 4 * 2 == 2 + ((7 % 4) * 2) == 8
    assert_eq!(run("fn main() -> int { return 2 + 7 % 4 * 2; };"), Ok(8));
    // left associative: 20 % 7 % 4 == (20 % 7) % 4 == 2
    assert_eq!(run("fn main() -> int { return 20 % 7 % 4; };"), Ok(2));
    assert_eq!(run("fn main() -> int { return (2 + 7) % 4; };"), Ok(1));
}

#[test]
fn modulo_needs_matching_numeric_operands() {
    for body in [
        "fn main() -> int { var s: str = \"a\" % \"b\"; return 0; };",
        "fn main() -> int { var x: float = 1.5 % 2; return 0; };",
        "fn main() -> int { var b: bool = true % false; return 0; };",
    ] {
        let err = run(body).unwrap_err();
        assert!(err.contains("%") || err.contains("incompatible"), "{body}: {err}");
    }
}

#[test]
fn modulo_works_in_the_config_evaluator_and_reports_zero() {
    let engine = Engine::new(CompileOptions::default());
    let compilation = engine.emit_source(
        "#[emit]\nstruct Cfg { a: int = 17 % 5; b: int = -17 % 5; c: float = 7.5 % 2.0; };",
    );
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let json = spar::emit::build_emit_json(
        compilation.result.as_ref().unwrap(),
        compilation.symbols.as_ref().unwrap(),
    )
    .unwrap()
    .to_string();
    assert!(json.contains("\"a\":2") && json.contains("\"b\":-2") && json.contains("\"c\":1.5"), "{json}");
    let bad = engine.emit_source("#[emit]\nstruct Cfg { a: int = 1 % 0; };");
    assert!(
        bad.errors.iter().any(|e| e.to_string().contains("division by zero")),
        "{:?}",
        bad.errors
    );
}

#[test]
fn formatter_spaces_and_parenthesises_modulo() {
    let formatted = spar::formatter::format_source("fn f(a: int) -> int {\n    return a%3+(a+1)%2;\n};\n").unwrap();
    assert!(formatted.contains("return a % 3 + (a + 1) % 2;"), "{formatted}");
    assert_eq!(spar::formatter::format_source(&formatted).unwrap(), formatted);
}
