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

#[test]
fn eval_same_section_qualified_self_reference() {
    let src = r#"
        [A]{
            a1: str = "hi";
            a2: str = A::a1;
        };
    "#;
    let r = eval_src(src);
    let path = vec!["A".to_string()];
    assert_eq!(
        r.sections[&path]["a2"],
        crate::evaluator::ConfigValue::Str("hi".into())
    );
}

#[test]
fn eval_cross_section_nested_to_nested_reference() {
    let src = r#"
        [X]{
            nested: section = {
                v: str = Y::inner::val;
            };
        };
        [Y]{
            inner: section = {
                val: str = "target";
            };
        };
    "#;
    // Run several times: before the fix this flakes because evaluation
    // order between X and Y is decided by HashMap iteration order.
    for _ in 0..20 {
        let r = eval_src(src);
        let path = vec!["X".to_string(), "nested".to_string()];
        assert_eq!(
            r.sections[&path]["v"],
            crate::evaluator::ConfigValue::Str("target".into())
        );
    }
}

#[test]
fn eval_genuine_nested_cycle_reports_cyclic_error_not_overflow() {
    let src = r#"
        [X]{
            nested: section = {
                v: str = Y::inner::val;
            };
        };
        [Y]{
            inner: section = {
                val: str = X::nested::v;
            };
        };
    "#;
    let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog    = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new().resolve(&prog, &[]).unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap();
    let result = crate::evaluator::Evaluator::new(symbols, prog).run();
    assert!(result.is_err(), "a genuine circular nested reference must error, not hang or panic");
}

#[test]
fn eval_self_reference_multi_level_nesting() {
    let src = r#"
        [Postgres]{
            environment: section = {
                postgresDb: str = "my_app";
                postgresUser: str = self::environment::postgresDb;
            };
        };
    "#;
    let r = eval_src(src);
    let path = vec!["Postgres".to_string(), "environment".to_string()];
    assert_eq!(
        r.sections[&path]["postgresUser"],
        crate::evaluator::ConfigValue::Str("my_app".into())
    );
}

#[test]
fn eval_self_reference_direct_child_field() {
    let src = r#"
        [A]{
            a1: str = "hi";
            a2: str = self::a1;
        };
    "#;
    let r = eval_src(src);
    let path = vec!["A".to_string()];
    assert_eq!(
        r.sections[&path]["a2"],
        crate::evaluator::ConfigValue::Str("hi".into())
    );
}

#[test]
fn eval_spread_inside_nested_field_body() {
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        [ProductionEnvironment] -> EnvironmentType {
            nodeEnv: "production";
            port: "3000";
        };
        [Api] -> ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    let r = eval_src(src);
    let path = vec!["Api".to_string(), "environment".to_string()];
    let nested = r.sections.get(&path).expect("nested environment section must be in sections map");
    assert_eq!(nested["nodeEnv"], crate::evaluator::ConfigValue::Str("production".into()));
    assert_eq!(nested["port"], crate::evaluator::ConfigValue::Str("3000".into()));
}

#[test]
fn eval_spread_inside_nested_field_body_ordering_is_deterministic() {
    // Regression guard for the class of bug Phase 1 fixed: build_dep_graph
    // must recurse into FieldValue::Nested to find this spread's
    // dependency on [ProductionEnvironment] — otherwise evaluation order
    // between the two top-level sections is left to HashMap iteration
    // order and flakes across runs. Run enough iterations that a flake
    // would show.
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        [ProductionEnvironment] -> EnvironmentType {
            nodeEnv: "production";
            port: "3000";
        };
        [Api] -> ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    for _ in 0..20 {
        let r = eval_src(src);
        let path = vec!["Api".to_string(), "environment".to_string()];
        let nested = r.sections.get(&path).expect("nested environment section must be in sections map");
        assert_eq!(nested["nodeEnv"], crate::evaluator::ConfigValue::Str("production".into()));
        assert_eq!(nested["port"], crate::evaluator::ConfigValue::Str("3000".into()));
    }
}

// ── Expr::Object evaluation ───────────────────────────────────────────────────

#[test]
fn object_literal_evaluates_to_section_config_value() {
    let src = "type [Leaf]{ name: str; }\nvar x: Leaf = { name: \"a\"; };\n";
    let r = eval_src(src);
    let crate::evaluator::ConfigValue::Section(map) = &r.globals["x"] else {
        panic!("expected ConfigValue::Section, got {:?}", r.globals["x"])
    };
    assert_eq!(map["name"], crate::evaluator::ConfigValue::Str("a".into()));
}

#[test]
fn list_of_object_literals_evaluates_to_list_of_section_config_values() {
    let src = "type [Leaf]{ name: str; }\nvar xs: [Leaf] = [{ name: \"a\"; }, { name: \"b\"; }];\n";
    let r = eval_src(src);
    let crate::evaluator::ConfigValue::List(items) = &r.globals["xs"] else {
        panic!("expected ConfigValue::List, got {:?}", r.globals["xs"])
    };
    assert_eq!(items.len(), 2);
    let crate::evaluator::ConfigValue::Section(first) = &items[0] else { panic!("expected Section element") };
    assert_eq!(first["name"], crate::evaluator::ConfigValue::Str("a".into()));
}

#[test]
fn enum_variant_evaluates_to_bare_string() {
    let src = "enum Devices { Ios, Android };\nvar x: Devices = Devices::Android;\n";
    let r = eval_src(src);
    assert_eq!(r.globals["x"], crate::evaluator::ConfigValue::Str("Android".into()));
}

#[test]
fn eval_named_field_access_on_loop_var() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        function looper(people: [Human]) -> str {
            for person in people {
                return person::name;
            }
            return "none";
        }
        var people: [Human] = [{ name: "jude"; age: 5; }];
        var result: str = looper(people: people);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Str("jude".into()));
}

#[test]
fn eval_named_field_access_on_global_var() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        var person: Human = { name: "Mike"; age: 5; };
        var pname: str = person::name;
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["pname"], crate::evaluator::ConfigValue::Str("Mike".into()));
}

#[test]
fn eval_original_bug_report_repro() {
    // Closest verbatim reconstruction of the original bug report
    // (docs/superpowers/specs/2026-08-26-functiongroup-and-named-field-access-design.md,
    // Part B), minus the unrelated `print` builtin finding.
    let src = r#"
        type [Human]{ name: str; age: int; }

        function looper(people: [Human]) -> int {
            for person in people {
                if person::name == "jude" {
                    return 6;
                }
                return 0;
            }
            return 0;
        }

        var people: [Human] = [{ name: "jude"; age: 5; }];
        var result: int = looper(people: people);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(6));
}

#[test]
fn eval_function_group_call() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 5; }
        }
        var result: int = EdgeInsect::only();
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(5));
}

#[test]
fn eval_function_group_call_with_args() {
    let src = r#"
        functionGroup Math {
            function double(n: int) -> int { return n + n; }
        }
        var result: int = Math::double(n: 21);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(42));
}

#[test]
fn eval_function_group_design_doc_example() {
    // Verbatim from docs/superpowers/specs/2026-08-26-functiongroup-and-named-field-access-design.md, Part A.
    let src = r#"
        private functionGroup EdgeInsect {
            function only() -> [int] { return [1, 2, 3, 5]; }
            private function semantic(hor: float, vet: float) -> [int] { return [1, 2, 3, 5]; }
        }

        [MainCont]{
            padding: [int] = EdgeInsect::only();
        };
    "#;
    let r = eval_src(src);
    let path = vec!["MainCont".to_string()];
    let padding = r.sections[&path]["padding"].clone();
    match padding {
        crate::evaluator::ConfigValue::List(items) => {
            assert_eq!(items.len(), 4);
        }
        other => panic!("expected a list, got {:?}", other),
    }
}

#[test]
fn eval_cross_file_function_group_call() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("shared.spar"),
        r#"
            functionGroup EdgeInsect {
                function only() -> int { return 7; }
            }
            private functionGroup Hidden {
                function f() -> int { return 1; }
            }
        "#,
    ).unwrap();

    let src = r#"
        import "shared.spar" as shared;
        var result: int = shared::EdgeInsect::only();
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let mut loader = crate::loader::ImportLoader::new(dir.path());
    let loaded = crate::loader::collect_imports(&program, &mut loader).expect("import must succeed");

    let symbols = crate::resolver::Resolver::resolve_with_imports(&program, &loaded)
        .expect("resolve failed");

    let result = crate::evaluator::Evaluator::evaluate_with_imports_and_base(
        &program, &symbols, &loaded, dir.path(),
    ).expect("eval failed");

    assert_eq!(result.globals["result"], crate::evaluator::ConfigValue::Int(7));
}

#[test]
fn eval_cross_file_private_function_group_not_exported() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("shared.spar"),
        r#"
            private functionGroup Hidden {
                function f() -> int { return 1; }
            }
        "#,
    ).unwrap();

    let src = r#"
        import "shared.spar" as shared;
        var result: int = shared::Hidden::f();
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let mut loader = crate::loader::ImportLoader::new(dir.path());
    let loaded = crate::loader::collect_imports(&program, &mut loader).expect("import must succeed");

    let result = crate::resolver::Resolver::resolve_with_imports(&program, &loaded);
    assert!(result.is_err(), "private functionGroup must not be reachable via import alias");
}
