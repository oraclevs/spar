fn check_ok(src: &str) {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = crate::resolver::Resolver::new().resolve(&prog, &[]).expect("resolve");
    crate::typechecker::TypeChecker::check(&prog, &symbols).expect("type check failed unexpectedly");
}

fn check_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = crate::resolver::Resolver::new().resolve(&prog, &[]).expect("resolve");
    match crate::typechecker::TypeChecker::check(&prog, &symbols) {
        Ok(_) => panic!("expected type error"),
        Err(e) => format!("{:?}", e),
    }
}

#[test]
fn typecheck_function_arg_type_mismatch() {
    let src = r#"
        function double(x: int) -> int { return x; }
        var y: int = double(x: "not an int");
    "#;
    let err = check_err(src);
    assert!(err.contains("int") || err.contains("str") || err.contains("type"));
}

#[test]
fn typecheck_function_return_type_mismatch() {
    let src = r#"function f(x: str) -> int { return x; }"#;
    let err = check_err(src);
    assert!(err.contains("return") || err.contains("int") || err.contains("str"));
}

#[test]
fn typecheck_comparison_produces_bool() {
    let src = r#"var b: bool = 1 == 2;"#;
    check_ok(src);
}

#[test]
fn typecheck_or_requires_bool_operands() {
    let src = r#"var b: bool = 1 || 2;"#;
    let err = check_err(src);
    assert!(err.contains("bool") || err.contains("int"));
}

#[test]
fn typecheck_unary_not_requires_bool() {
    let src = r#"var b: bool = !42;"#;
    let err = check_err(src);
    assert!(err.contains("bool") || err.contains("int"));
}

#[test]
fn typecheck_comprehension_source_must_be_list() {
    // `x` is a str, not a list; the comprehension body uses a literal to avoid
    // resolver issues with the loop variable at global scope.
    let src = r#"
        var x: str = "not a list";
        var y: [int] = for item in x { 42 };
    "#;
    let err = check_err(src);
    assert!(err.contains("list") || err.contains("str"));
}

#[test]
fn typecheck_section_field_can_be_function_call() {
    let src = r#"
        function makeServer(host: str) -> section {
            return { host: str = host; };
        }
        [App]{
            server: section = makeServer(host: "localhost");
        };
    "#;
    check_ok(src);
}

#[test]
fn typecheck_if_condition_must_be_bool() {
    let src = r#"
        function f(x: int) -> str {
            if x { var r: str = "a"; } else { var r: str = "b"; }
            return r;
        }
    "#;
    let err = check_err(src);
    assert!(err.contains("bool") || err.contains("int"));
}

#[test]
fn typecheck_function_body_call_arg_type_mismatch() {
    // Wrong arg type inside a function body must be caught
    let src = r#"
        function double(x: int) -> int { return x; }
        function caller(s: str) -> int {
            var result: int = double(x: s);
            return result;
        }
    "#;
    let err = check_err(src);
    assert!(
        err.contains("int") || err.contains("str") || err.contains("type") || err.contains("arg"),
        "expected type mismatch error, got: {err}"
    );
}

#[test]
fn typecheck_cross_type_eq_in_function_body_rejected() {
    let src = r#"
        function f(x: int) -> bool {
            var b: bool = x == "hello";
            return b;
        }
    "#;
    let err = check_err(src);
    assert!(err.contains("type") || err.contains("int") || err.contains("str"));
}

// ── Phase 11d tests ───────────────────────────────────────────────────────────

#[test]
fn for_loop_over_non_list_is_type_error() {
    let src = r#"
        function f(x: str) -> int {
            for c in x { return 0; }
            return 1;
        }
    "#;
    let err = check_err(src);
    assert!(err.contains("list") || err.contains("str"));
}

#[test]
fn for_loop_over_list_typechecks_ok() {
    check_ok(r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
            return 0;
        }
    "#);
}

#[test]
fn nested_for_loops_typecheck() {
    check_ok(r#"
        function flatten(grid: [[int]]) -> int {
            for row in grid {
                for cell in row {
                    if cell > 100 { return cell; }
                }
            }
            return 0;
        }
    "#);
}

#[test]
fn bool_type_in_return_section_typechecks() {
    check_ok(r#"
        function f(major: int) -> section {
            if major <= 0 {
                return { error: bool = true; message: str = "bad"; };
            }
            return { error: bool = false; };
        }
    "#);
}
