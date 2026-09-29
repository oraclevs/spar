//! `while` and `loop`: diagnostics, control-flow analysis, formatting.
use spar::{CompileOptions, Engine};

fn run(source: &str) -> Result<i32, String> {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

#[test]
fn while_condition_must_be_bool() {
    let err = run("fn main() -> int { var mut i: int = 1; while i { i = 0; } return 0; };").unwrap_err();
    assert!(err.contains("while condition must be 'bool', found 'int'"), "{err}");
}

#[test]
fn break_and_continue_are_valid_in_while_and_loop_only() {
    assert_eq!(
        run("fn main() -> int { var mut i: int = 0; while true { i = i + 1; if i == 5 { break; } } return i; };"),
        Ok(5)
    );
    let err = run("fn main() -> int { break; return 0; };").unwrap_err();
    assert!(err.contains("'break' is only valid inside a loop"), "{err}");
}

#[test]
fn loop_without_break_never_falls_through() {
    // No `return` after the loop: not a "missing return" error.
    assert_eq!(
        run("fn f() -> int { loop { return 4; } }; fn main() -> int { return f(); };"),
        Ok(4)
    );
    // Code after an infinite loop is unreachable.
    let err = run("fn f() -> int { loop { } return 1; }; fn main() -> int { return 0; };").unwrap_err();
    assert!(err.contains("unreachable code"), "{err}");
}

#[test]
fn loop_with_break_falls_through_and_needs_a_return() {
    let err = run("fn f() -> int { loop { break; } }; fn main() -> int { return 0; };").unwrap_err();
    assert!(err.to_lowercase().contains("return"), "{err}");
    assert_eq!(
        run("fn f() -> int { loop { break; } return 6; }; fn main() -> int { return f(); };"),
        Ok(6)
    );
}

#[test]
fn a_break_in_a_nested_loop_does_not_end_the_outer_loop() {
    // The inner `break` belongs to the inner loop, so the outer `loop` still never falls through.
    assert_eq!(
        run("fn f() -> int { loop { while true { break; } return 8; } }; fn main() -> int { return f(); };"),
        Ok(8)
    );
}

#[test]
fn while_condition_can_call_functions_and_see_locals_shadowing_rules() {
    assert_eq!(
        run("fn small(n: int) -> bool { return n < 4; }; fn main() -> int { var mut i: int = 0; while small(n: i) { var t: int = i; i = t + 1; } return i; };"),
        Ok(4)
    );
}

#[test]
fn formatter_round_trips_while_and_loop() {
    let source = "fn f() -> int {\n    var mut i: int = 0;\n    while i < 3 {\n        i = i + 1;\n    }\n    loop {\n        if i > 5 {\n            break;\n        }\n        i = i + 1;\n    }\n    return i;\n};\n";
    let formatted = spar::formatter::format_source(source).unwrap();
    assert_eq!(formatted, source);
    let messy = "fn f() -> int {\nvar mut i: int = 0;\nwhile   i<3 {i = i + 1;}\nloop { break; }\nreturn i;\n};\n";
    let formatted = spar::formatter::format_source(messy).unwrap();
    assert!(formatted.contains("    while i < 3 {\n        i = i + 1;\n    }\n"), "{formatted}");
    assert!(formatted.contains("    loop {\n        break;\n    }\n"), "{formatted}");
    assert_eq!(spar::formatter::format_source(&formatted).unwrap(), formatted);
}

#[test]
fn comments_inside_loops_survive_formatting() {
    let source = "fn f() -> int {\n    var mut i: int = 0;\n    while i < 3 { // head\n        // note\n        i = i + 1;\n    }\n    return i;\n};\n";
    let formatted = spar::formatter::format_source(source).unwrap();
    assert!(formatted.contains("// note"), "{formatted}");
}
