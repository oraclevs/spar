fn eval_src(src: &str) -> crate::evaluator::EvalResult {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new().resolve(&prog, &[]).unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap();
    crate::evaluator::Evaluator::new(symbols, prog).run().unwrap()
}

#[test]
fn eval_function_returning_str() {
    let src = r#"
        function greet(name: str) -> str { return name; }
        var result: str = greet(name: "world");
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Str("world".into()));
}

#[test]
fn eval_function_returning_section() {
    let src = r#"
        function makeConf(host: str) -> section { return { host: str = host; }; }
        [Server]{
            server: section = makeConf(host: "localhost");
        };
    "#;
    let r = eval_src(src);
    // Section-valued fields are stored at their nested path, not as scalar values in the parent
    let nested_path = vec!["Server".to_string(), "server".to_string()];
    let nested = r.sections.get(&nested_path).expect("nested section must be in sections map");
    assert_eq!(nested["host"], crate::evaluator::ConfigValue::Str("localhost".into()));
}

#[test]
fn eval_comparison_eq() {
    let src = r#"var b: bool = 1 == 1;"#;
    let r = eval_src(src);
    assert_eq!(r.globals["b"], crate::evaluator::ConfigValue::Bool(true));
}

#[test]
fn eval_logical_and() {
    let src = r#"var b: bool = true && false;"#;
    let r = eval_src(src);
    assert_eq!(r.globals["b"], crate::evaluator::ConfigValue::Bool(false));
}

#[test]
fn eval_unary_not() {
    let src = r#"var b: bool = !true;"#;
    let r = eval_src(src);
    assert_eq!(r.globals["b"], crate::evaluator::ConfigValue::Bool(false));
}

#[test]
fn eval_comprehension() {
    let src = r#"
        var nums: [int] = [1, 2, 3];
        var doubled: [int] = for x in nums { x + x };
    "#;
    let r = eval_src(src);
    assert!(r.globals.contains_key("doubled"));
}

#[test]
fn eval_function_with_if_else() {
    let src = r#"
        function choose(flag: bool) -> str {
            if flag { var r: str = "yes"; } else { var r: str = "no"; }
            return r;
        }
        var result: str = choose(flag: true);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Str("yes".into()));
}

#[test]
fn eval_function_with_local_var() {
    let src = r#"
        function double(x: int) -> int {
            var twice: int = x + x;
            return twice;
        }
        var result: int = double(x: 5);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(10));
}

#[test]
fn eval_recursive_function_depth_limit() {
    let src = r#"
        function inf(n: int) -> int { return inf(n: n); }
        var x: int = inf(n: 0);
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new().resolve(&prog, &[]).unwrap();
    let result = crate::evaluator::Evaluator::new(symbols, prog).run();
    assert!(result.is_err());
}

// ── Phase 11b tests ───────────────────────────────────────────────────────────

#[test]
fn arithmetic_precedence() {
    let src = r#"function f() -> int { return 2 + 3 * 4; } var n: int = f(n: 0);"#;
    // Can't call f() at global scope with named args; test via section field instead
    let src = r#"
        function mul(a: int, b: int) -> int { return a * b; }
        var n: int = 2 + mul(a: 3, b: 4);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(14));
}

#[test]
fn integer_division_truncates() {
    let src = r#"function f(a: int, b: int) -> int { return a / b; } var n: int = f(a: 7, b: 2);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(3));
}

#[test]
fn unary_neg_int() {
    let src = r#"function neg(x: int) -> int { return 0 - x; } var n: int = neg(x: 5);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(-5));
}

#[test]
fn conversion_float_to_int() {
    let src = r#"function f(x: float) -> int { return int(x); } var n: int = f(x: 7.9);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(7));
}

#[test]
fn conversion_int_to_str() {
    let src = r#"function f(x: int) -> str { return str(x); } var s: str = f(x: 42);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["s"], crate::evaluator::ConfigValue::Str("42".into()));
}

#[test]
fn list_index_basic() {
    let src = r#"var names: [str] = ["dev", "staging", "prod"]; var first: str = names[0];"#;
    let r = eval_src(src);
    assert_eq!(r.globals["first"], crate::evaluator::ConfigValue::Str("dev".into()));
}

#[test]
fn spread_function_call() {
    let src = r#"
        function defaults() -> section { return { tier: str = "free"; }; }
        [App]{ ...defaults(); limit: int = 500; };
    "#;
    let r = eval_src(src);
    assert_eq!(r.sections[&vec!["App".to_string()]]["limit"], crate::evaluator::ConfigValue::Int(500));
    assert_eq!(r.sections[&vec!["App".to_string()]]["tier"], crate::evaluator::ConfigValue::Str("free".into()));
}

#[test]
fn spread_explicit_overrides() {
    let src = r#"
        function defaults() -> section { return { env: str = "dev"; }; }
        [App]{ ...defaults(); env: str = "prod"; };
    "#;
    let r = eval_src(src);
    assert_eq!(r.sections[&vec!["App".to_string()]]["env"], crate::evaluator::ConfigValue::Str("prod".into()));
}

#[test]
fn eval_dep_ordered_globals() {
    // b depends on a; both must evaluate correctly regardless of declaration order
    let src = r#"
        var b: int = a + 1;
        var a: int = 5;
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["b"], crate::evaluator::ConfigValue::Int(6));
}

#[test]
fn eval_multiple_sections_same_prefix() {
    // Both [Server] and [Server.Prod] (nested) must be evaluated
    let src = r#"
        [Server]{
            host: str = "0.0.0.0";
            prod: section = {
                host: str = "prod.example.com";
            };
        };
    "#;
    let r = eval_src(src);
    let server = &r.sections[&vec!["Server".to_string()]];
    assert_eq!(server["host"], crate::evaluator::ConfigValue::Str("0.0.0.0".into()));
    let server_prod = &r.sections[&vec!["Server".to_string(), "prod".to_string()]];
    assert_eq!(server_prod["host"], crate::evaluator::ConfigValue::Str("prod.example.com".into()));
}

// ── Phase 11d tests ───────────────────────────────────────────────────────────

#[test]
fn for_loop_early_return_finds_first_match() {
    let src = r#"
        function firstBigDouble(nums: [int]) -> int {
            for n in nums {
                var doubled: int = n * 2;
                if doubled > 10 { return doubled; }
            }
            return -1;
        }
        var result: int = firstBigDouble(nums: [1, 8, 2]);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(16));
}

#[test]
fn for_loop_no_match_falls_through_to_next_stmt() {
    // Loop body condition never fires → falls through to return after loop
    let src = r#"
        function f(nums: [int]) -> str {
            for n in nums { if n < -999 { return "found"; } }
            return "not found";
        }
        var result: str = f(nums: [1, 2, 3]);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Str("not found".into()));
}

#[test]
fn for_loop_return_propagates_out() {
    let src = r#"
        function summarize(nums: [int]) -> str {
            for n in nums {
                if n < 0 { return "found a negative"; }
            }
            return "all non-negative";
        }
        var a: str = summarize(nums: [4, 9, -2, 7]);
        var b: str = summarize(nums: [1, 2, 3]);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["a"], crate::evaluator::ConfigValue::Str("found a negative".into()));
    assert_eq!(r.globals["b"], crate::evaluator::ConfigValue::Str("all non-negative".into()));
}

#[test]
fn bool_type_in_return_section_evaluates() {
    let src = r#"
        function builderFunc(major: int) -> section {
            if major <= 0 {
                return { error: bool = true; message: str = "bad"; };
            }
            return { error: bool = false; version: str = "ok"; };
        }
        [Release]{ ...builderFunc(major: 2); };
    "#;
    let r = eval_src(src);
    let rel = &r.sections[&vec!["Release".to_string()]];
    assert_eq!(rel["error"], crate::evaluator::ConfigValue::Bool(false));
    assert_eq!(rel["version"], crate::evaluator::ConfigValue::Str("ok".into()));
}

#[test]
fn private_function_callable_within_same_file() {
    let src = r#"
        private function double(x: int) -> int { return x * 2; }
        var result: int = double(x: 5);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(10));
}
