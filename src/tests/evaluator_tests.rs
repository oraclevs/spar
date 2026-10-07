fn eval_src(src: &str) -> crate::evaluator::EvalResult {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap();
    crate::evaluator::Evaluator::new(symbols, prog)
        .run()
        .unwrap()
}

/// Like `eval_src`, but runs the full `Engine` pipeline (which splices in
/// the Spar-written prelude — `some`/`none`/`print`/... — before resolving)
/// instead of the bare lex/parse/resolve/typecheck/eval chain above. Needed
/// for sources that call prelude functions; the bare chain leaves them
/// undefined since prelude injection is a pipeline-level step, not part of
/// `Resolver::new().resolve(...)`.
fn eval_src_with_prelude(src: &str) -> crate::evaluator::EvalResult {
    let compilation = crate::Engine::default().emit_source(src);
    assert!(compilation.is_ok(), "{:?}", compilation.errors);
    compilation.result.expect("no eval result")
}

#[test]
fn eval_function_returning_str() {
    let src = r#"
        function greet(name: str) -> str { return name; };
        var result: str = greet(name: "world");
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("world".into())
    );
}

#[test]
fn eval_indexed_for_uses_zero_based_int_index() {
    let result = eval_src(
        r#"
        function secondIndex(values: [str]) -> int {
            for (index, value) in values {
                if value == "second" { return index; }
            }
            return 99;
        };
        var result: int = secondIndex(values: ["first", "second"]);
        "#,
    );
    assert_eq!(
        result.globals["result"],
        crate::evaluator::ConfigValue::Int(1)
    );
}

#[test]
fn evaluator_executes_module_if_and_for_statements() {
    let source = r#"
        function fail(value: int) -> int { return value / 0; };
        var empty: [int] = [];
        if false { fail(value: 1); }
        for item in empty { fail(value: item); }
        if true { fail(value: 2); }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .resolve(&program, &[])
        .unwrap();
    crate::typechecker::TypeChecker::check(&program, &symbols).unwrap();
    let error = crate::evaluator::Evaluator::new(symbols, program)
        .run()
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"), "{error}");
}

#[test]
fn nested_block_shadowing_does_not_replace_outer_local() {
    let result = eval_src(
        r#"
        function value() -> str {
            var name: str = "outside";
            if true { var name: str = "inside"; }
            return name;
        };
        var result: str = value();
        "#,
    );
    assert_eq!(
        result.globals["result"],
        crate::evaluator::ConfigValue::Str("outside".into())
    );
}

#[test]
fn break_and_continue_target_the_innermost_loop() {
    let result = eval_src(
        r#"
        function afterContinue() -> int {
            for value in [1, 2] {
                if value == 1 { continue; }
                return value;
            }
            return 99;
        };
        function afterInnerBreak() -> int {
            for outer in [7] {
                for inner in [1] { break; }
                return outer;
            }
            return 99;
        };
        var continued: int = afterContinue();
        var innerBreak: int = afterInnerBreak();
        "#,
    );
    assert_eq!(
        result.globals["continued"],
        crate::evaluator::ConfigValue::Int(2)
    );
    assert_eq!(
        result.globals["innerBreak"],
        crate::evaluator::ConfigValue::Int(7)
    );
}

#[test]
fn mutable_assignments_cross_nested_lexical_blocks() {
    let result = eval_src(
        r#"
        function count(values: [int]) -> int {
            var mut total: int = 0;
            for value in values {
                if value == 2 { continue; }
                total = total + value;
            }
            return total;
        };
        var mut moduleCount: int = 0;
        if true { moduleCount = count(values: [1, 2, 3]); }
        "#,
    );
    assert_eq!(
        result.globals["moduleCount"],
        crate::evaluator::ConfigValue::Int(4)
    );
}

#[test]
fn void_function_bare_return_exits_early_and_call_statement_executes_side_effects() {
    let result = eval_src(
        r#"
        var mut count: int = 0;
        function bump(stop: bool) -> void {
            count = count + 1;
            if stop { return; }
            count = count + 100;
        };
        bump(stop: true);
        bump(stop: false);
        "#,
    );
    assert_eq!(
        result.globals["count"],
        crate::evaluator::ConfigValue::Int(102)
    );
}

#[test]
fn eval_function_parameter_default_and_explicit_override() {
    let src = r#"
        var defaultName: str = "world";
        function greet(name: str = defaultName) -> str { return name; };
        var implicit: str = greet();
        var explicit: str = greet(name: "Spar");
    "#;
    let result = eval_src(src);
    assert_eq!(
        result.globals["implicit"],
        crate::evaluator::ConfigValue::Str("world".into())
    );
    assert_eq!(
        result.globals["explicit"],
        crate::evaluator::ConfigValue::Str("Spar".into())
    );
}

#[test]
fn eval_function_returning_named_struct() {
    let src = r#"
        struct Conf { host: str; };
        function makeConf(host: str) -> Conf { return Conf(host: host); };
        var conf: Conf = makeConf(host: "localhost");
    "#;
    let r = eval_src(src);
    let crate::evaluator::ConfigValue::Object(conf) = &r.globals["conf"] else {
        panic!("expected named struct value")
    };
    assert_eq!(conf["host"], crate::evaluator::ConfigValue::Str("localhost".into()));
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
            if flag { return "yes"; } else { return "no"; }
        };
        var result: str = choose(flag: true);
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("yes".into())
    );
}

#[test]
fn eval_function_with_local_var() {
    let src = r#"
        function double(x: int) -> int {
            var twice: int = x + x;
            return twice;
        };
        var result: int = double(x: 5);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(10));
}

#[test]
fn eval_recursive_function_depth_limit() {
    let src = r#"
        function inf(n: int) -> int { return inf(n: n); };
        var x: int = inf(n: 0);
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .unwrap();
    let result = crate::evaluator::Evaluator::new(symbols, prog).run();
    assert!(result.is_err());
}

// ── Phase 11b tests ───────────────────────────────────────────────────────────

#[test]
fn arithmetic_precedence() {
    let _src = r#"function f() -> int { return 2 + 3 * 4; }; var n: int = f(n: 0);"#;
    // Can't call f() at global scope with named args; test via section field instead
    let src = r#"
        function mul(a: int, b: int) -> int { return a * b; };
        var n: int = 2 + mul(a: 3, b: 4);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(14));
}

#[test]
fn integer_division_truncates() {
    let src = r#"function f(a: int, b: int) -> int { return a / b; }; var n: int = f(a: 7, b: 2);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(3));
}

#[test]
fn unary_neg_int() {
    let src = r#"function neg(x: int) -> int { return 0 - x; }; var n: int = neg(x: 5);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(-5));
}

#[test]
fn conversion_float_to_int() {
    let src = r#"function f(x: float) -> int { return int(value: x); }; var n: int = f(x: 7.9);"#;
    let r = eval_src(src);
    assert_eq!(r.globals["n"], crate::evaluator::ConfigValue::Int(7));
}

#[test]
fn conversion_int_to_str() {
    let src = r#"function f(x: int) -> str { return str(value: x); }; var s: str = f(x: 42);"#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["s"],
        crate::evaluator::ConfigValue::Str("42".into())
    );
}

#[test]
fn list_index_basic() {
    let src = r#"var names: [str] = ["dev", "staging", "prod"]; var first: str = names[0];"#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["first"],
        crate::evaluator::ConfigValue::Str("dev".into())
    );
}

#[test]
fn spread_named_struct_fields() {
    let result = eval_src(r#"struct Defaults { tier: str = "free"; }; var app: Record = { ...Defaults(); limit: 500; };"#);
    let crate::evaluator::ConfigValue::Object(app) = &result.globals["app"] else { panic!("expected record"); };
    assert_eq!(app["limit"], crate::evaluator::ConfigValue::Int(500));
}

#[test]
fn spread_named_struct_explicit_field_overrides() {
    let result = eval_src(r#"struct Defaults { env: str = "dev"; }; var app: Record = { ...Defaults(); env: "prod"; };"#);
    let crate::evaluator::ConfigValue::Object(app) = &result.globals["app"] else { panic!("expected record"); };
    assert_eq!(app["env"], crate::evaluator::ConfigValue::Str("prod".into()));
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
        #[emit] struct Server {
            host: str = "0.0.0.0";
            prod: Record = {
                host: "prod.example.com";
            };
        };
    "#;
    let r = eval_src(src);
    let server = &r.structs[&vec!["Server".to_string()]];
    assert_eq!(
        server["host"],
        crate::evaluator::ConfigValue::Str("0.0.0.0".into())
    );
    let crate::evaluator::ConfigValue::Object(server_prod) = &server["prod"] else {
        panic!("expected nested record for prod")
    };
    assert_eq!(
        server_prod["host"],
        crate::evaluator::ConfigValue::Str("prod.example.com".into())
    );
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
        };
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
        };
        var result: str = f(nums: [1, 2, 3]);
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("not found".into())
    );
}

#[test]
fn for_loop_return_propagates_out() {
    let src = r#"
        function summarize(nums: [int]) -> str {
            for n in nums {
                if n < 0 { return "found a negative"; }
            }
            return "all non-negative";
        };
        var a: str = summarize(nums: [4, 9, -2, 7]);
        var b: str = summarize(nums: [1, 2, 3]);
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["a"],
        crate::evaluator::ConfigValue::Str("found a negative".into())
    );
    assert_eq!(
        r.globals["b"],
        crate::evaluator::ConfigValue::Str("all non-negative".into())
    );
}

#[test]
fn bool_type_in_returned_named_struct_evaluates() {
    let src = r#"
        struct BuildResult {
            error: bool;
            version: Option<str> = none();
            message: Option<str> = none();
        };
        function builderFunc(major: int) -> BuildResult {
            if major <= 0 { return BuildResult(error: true, message: some(value: "bad")); }
            return BuildResult(error: false, version: some(value: "ok"));
        };
        var release: BuildResult = builderFunc(major: 2);
    "#;
    let r = eval_src_with_prelude(src);
    let crate::evaluator::ConfigValue::Object(release) = &r.globals["release"] else {
        panic!("expected named struct result")
    };
    assert_eq!(release["error"], crate::evaluator::ConfigValue::Bool(false));
}

#[test]
fn private_function_callable_within_same_file() {
    let src = r#"
        private function double(x: int) -> int { return x * 2; };
        var result: int = double(x: 5);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(10));
}

#[test]
fn rejects_same_section_qualified_self_reference() {
    assert!(crate::Engine::default().check_source(r#"struct A { a1: str = "hi"; a2: str = A.a1; };"#).is_err());
}

#[test]
fn eval_cross_section_nested_to_nested_reference() {
    let src = r#"
        #[emit] struct X {
            nested: Record = {
                v: Y().inner.val;
            };
        };
        struct Y {
            inner: Record = {
                val: "target";
            };
        };
    "#;
    // Run several times: before the fix this flakes because evaluation
    // order between X and Y is decided by HashMap iteration order.
    for _ in 0..20 {
        let r = eval_src(src);
        let x = &r.structs[&vec!["X".to_string()]];
        let crate::evaluator::ConfigValue::Object(nested) = &x["nested"] else {
            panic!("expected nested record")
        };
        assert_eq!(
            nested["v"],
            crate::evaluator::ConfigValue::Str("target".into())
        );
    }
}

#[test]
fn eval_genuine_nested_cycle_reports_cyclic_error_not_overflow() {
    let src = r#"
        #[emit] struct X {
            nested: Record = {
                v: Y().inner.val;
            };
        };
        struct Y {
            inner: Record = {
                val: X().nested.v;
            };
        };
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap();
    let result = crate::evaluator::Evaluator::new(symbols, prog).run();
    assert!(
        result.is_err(),
        "a genuine circular nested reference must error, not hang or panic"
    );
}

#[test]
fn eval_self_reference_multi_level_nesting() {
    // `self` in a field's default-value expression is rejected (see
    // rejects_self_reference_direct_child_field / rejects_self_dot_field_access
    // above and resolver_tests.rs:775) — `self` is only meaningful inside an
    // `impl` method body now, not in struct field defaults. This confirms
    // the rejection holds even when `self` is reached through a nested
    // `Record` literal, not just directly on a struct's own top-level field.
    assert!(crate::Engine::default()
        .check_source(
            r#"
        struct Postgres {
            environment: Record = {
                postgresDb: "my_app";
                postgresUser: self.environment.postgresDb;
            };
        };
    "#
        )
        .is_err());
}

#[test]
fn rejects_self_reference_direct_child_field() {
    assert!(crate::Engine::default().check_source(r#"struct A { a1: str = "hi"; a2: str = self.a1; };"#).is_err());
}

#[test]
fn eval_named_struct_inside_nested_field() {
    let src = r#"
        struct EnvironmentType { nodeEnv: str; port: str; };
        #[emit] struct Api {
            image: str = "my-api";
            environment: EnvironmentType = EnvironmentType(nodeEnv: "production", port: "3000");
        };
    "#;
    let r = eval_src(src);
    let api = r.structs.get(&vec!["Api".to_string()]).expect("Api must be materialized");
    let crate::evaluator::ConfigValue::Object(nested) = &api["environment"] else {
        panic!("nested named struct must be materialized")
    };
    assert_eq!(nested["nodeEnv"], crate::evaluator::ConfigValue::Str("production".into()));
    assert_eq!(nested["port"], crate::evaluator::ConfigValue::Str("3000".into()));
}

#[test]
fn eval_nested_named_struct_ordering_is_deterministic() {
    let src = r#"
        struct EnvironmentType { nodeEnv: str; port: str; };
        #[emit] struct Api {
            image: str = "my-api";
            environment: EnvironmentType = EnvironmentType(nodeEnv: "production", port: "3000");
        };
    "#;
    for _ in 0..20 {
        let r = eval_src(src);
        let api = r.structs.get(&vec!["Api".to_string()]).expect("Api must be materialized");
        let crate::evaluator::ConfigValue::Object(nested) = &api["environment"] else {
            panic!("nested environment struct")
        };
        assert_eq!(nested["nodeEnv"], crate::evaluator::ConfigValue::Str("production".into()));
        assert_eq!(nested["port"], crate::evaluator::ConfigValue::Str("3000".into()));
    }
}

#[test]
fn named_struct_constructor_evaluates_to_struct_config_value() {
    let src = "struct Leaf { name: str; };\nvar x: Leaf = Leaf(name: \"a\");\n";
    let r = eval_src(src);
    let crate::evaluator::ConfigValue::Object(map) = &r.globals["x"] else {
        panic!("expected named struct ConfigValue, got {:?}", r.globals["x"])
    };
    assert_eq!(map["name"], crate::evaluator::ConfigValue::Str("a".into()));
}

#[test]
fn list_of_named_structs_evaluates_to_list_of_struct_config_values() {
    let src = r#"
        struct Leaf { name: str; };
        var xs: List<Leaf> = [Leaf(name: "a"), Leaf(name: "b")];
    "#;
    let r = eval_src(src);
    let crate::evaluator::ConfigValue::List(items) = &r.globals["xs"] else {
        panic!("expected ConfigValue::List, got {:?}", r.globals["xs"])
    };
    assert_eq!(items.len(), 2);
    let crate::evaluator::ConfigValue::Object(first) = &items[0] else {
        panic!("expected named struct element")
    };
    assert_eq!(first["name"], crate::evaluator::ConfigValue::Str("a".into()));
}

#[test]
fn enum_variant_evaluates_to_bare_string() {
    let src = "enum Devices { Ios, Android };\nvar x: Devices = Devices::Android;\n";
    let r = eval_src(src);
    assert_eq!(
        r.globals["x"],
        crate::evaluator::ConfigValue::Str("Android".into())
    );
}

#[test]
fn eval_named_field_access_on_loop_var() {
    let src = r#"
        struct Human{ name: str; age: int; };
        function looper(people: [Human]) -> str {
            for person in people {
                return person.name;
            }
            return "none";
        };
        var people: [Human] = [Human(name: "jude", age: 5)];
        var result: str = looper(people: people);
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("jude".into())
    );
}

#[test]
fn eval_named_field_access_on_global_var() {
    let src = r#"
        struct Human{ name: str; age: int; };
        var person: Human = Human(name: "Mike", age: 5);
        var pname: str = person.name;
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["pname"],
        crate::evaluator::ConfigValue::Str("Mike".into())
    );
}

#[test]
fn eval_original_bug_report_repro() {
    // Closest verbatim reconstruction of the original bug report
    // (docs/superpowers/specs/2026-08-26-functiongroup-and-named-field-access-design.md,
    // Part B), minus the unrelated `print` builtin finding.
    let src = r#"
        struct Human{ name: str; age: int; };

        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" {
                    return 6;
                }
                return 0;
            }
            return 0;
        };

        var people: [Human] = [Human(name: "jude", age: 5)];
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
        };
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
        };
        var result: int = Math::double(n: 21);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(42));
}

#[test]
fn eval_function_group_call_with_defaulted_arg() {
    let src = r#"
        functionGroup Math {
            function double(n: int = 21) -> int { return n + n; }
        };
        var result: int = Math::double();
    "#;
    let result = eval_src(src);
    assert_eq!(
        result.globals["result"],
        crate::evaluator::ConfigValue::Int(42)
    );
}

#[test]
fn eval_function_group_design_doc_example() {
    // Verbatim from docs/superpowers/specs/2026-08-26-functiongroup-and-named-field-access-design.md, Part A.
    let src = r#"
        private functionGroup EdgeInsect {
            function only() -> [int] { return [1, 2, 3, 5]; }
            private function semantic(hor: float, vet: float) -> [int] { return [1, 2, 3, 5]; }
        };

        #[emit] struct MainCont {
            padding: [int] = EdgeInsect::only();
        };
    "#;
    let r = eval_src(src);
    let path = vec!["MainCont".to_string()];
    let padding = r.structs[&path]["padding"].clone();
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
            };
            private functionGroup Hidden {
                function f() -> int { return 1; }
            };
        "#,
    )
    .unwrap();

    let src = r#"
        import "shared.spar" as shared;
        var result: int = shared::EdgeInsect::only();
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let mut loader = crate::loader::ImportLoader::new(dir.path());
    let loaded =
        crate::loader::collect_imports(&program, &mut loader).expect("import must succeed");

    let symbols =
        crate::resolver::Resolver::resolve_with_imports(&program, &loaded).expect("resolve failed");

    let result = crate::evaluator::Evaluator::evaluate_with_imports_and_base(
        &program,
        &symbols,
        &loaded,
        dir.path(),
        crate::host::HostRegistry::default(),
    )
    .expect("eval failed");

    assert_eq!(
        result.globals["result"],
        crate::evaluator::ConfigValue::Int(7)
    );
}

#[test]
fn eval_cross_file_function_group_call_with_defaulted_arg() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("shared.spar"),
        r#"
            var defaultValue: int = 7;
            functionGroup EdgeInsect {
                function only(value: int = defaultValue) -> int { return value; }
            };
        "#,
    )
    .unwrap();

    let src = r#"
        import "shared.spar" as shared;
        var result: int = shared::EdgeInsect::only();
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let mut loader = crate::loader::ImportLoader::new(dir.path());
    let loaded =
        crate::loader::collect_imports(&program, &mut loader).expect("import must succeed");
    let symbols =
        crate::resolver::Resolver::resolve_with_imports(&program, &loaded).expect("resolve failed");
    let result = crate::evaluator::Evaluator::evaluate_with_imports_and_base(
        &program,
        &symbols,
        &loaded,
        dir.path(),
        crate::host::HostRegistry::default(),
    )
    .expect("eval failed");

    assert_eq!(
        result.globals["result"],
        crate::evaluator::ConfigValue::Int(7)
    );
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
            };
        "#,
    )
    .unwrap();

    let src = r#"
        import "shared.spar" as shared;
        var result: int = shared::Hidden::f();
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let mut loader = crate::loader::ImportLoader::new(dir.path());
    let loaded =
        crate::loader::collect_imports(&program, &mut loader).expect("import must succeed");

    let result = crate::resolver::Resolver::resolve_with_imports(&program, &loaded);
    assert!(
        result.is_err(),
        "private functionGroup must not be reachable via import alias"
    );
}

#[test]
fn eval_dot_field_access_on_loop_var() {
    let src = r#"
        struct Human{ name: str; age: int; };
        function looper(people: [Human]) -> str {
            for person in people {
                return person.name;
            }
            return "none";
        };
        var people: [Human] = [Human(name: "jude", age: 5)];
        var result: str = looper(people: people);
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("jude".into())
    );
}

#[test]
fn eval_dot_field_access_after_index() {
    let src = r#"
        struct Human{ name: str; age: int; };
        var people: [Human] = [Human(name: "jude", age: 5)];
        var result: str = people[0].name;
    "#;
    let r = eval_src(src);
    assert_eq!(
        r.globals["result"],
        crate::evaluator::ConfigValue::Str("jude".into())
    );
}

#[test]
fn rejects_self_dot_field_access() {
    assert!(crate::Engine::default().check_source(r#"struct Server { port: int = 8080; display: str = "port-${self.port}"; };"#).is_err());
}

#[test]
fn eval_original_bug_report_repro_with_dot_syntax() {
    // Same repro as the prior session's regression test, updated to the
    // new dot syntax — confirms the whole pipeline still produces the
    // same result under the new grammar.
    let src = r#"
        struct Human{ name: str; age: int; };
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" {
                    return 6;
                }
                return 0;
            }
            return 0;
        };
        var people: [Human] = [Human(name: "jude", age: 5)];
        var result: int = looper(people: people);
    "#;
    let r = eval_src(src);
    assert_eq!(r.globals["result"], crate::evaluator::ConfigValue::Int(6));
}

#[test]
fn eval_shell_block_lowers_to_expected_plan() {
    let result = eval_src("var x: shell = shell { echo hi; };");
    let crate::evaluator::ConfigValue::Shell(plan) = &result.globals["x"] else {
        panic!("expected ConfigValue::Shell")
    };
    let spar_command::Step::Command(command) = &plan.steps[0].1 else {
        panic!("expected command")
    };
    assert_eq!(command.program, "echo");
    assert_eq!(command.args, ["hi"]);
}

#[test]
fn eval_pipeline_and_redirect_lower_correctly() {
    let result = eval_src("var x: shell = shell { cat input | grep x > out.log; };");
    let crate::evaluator::ConfigValue::Shell(plan) = &result.globals["x"] else {
        panic!("expected ConfigValue::Shell")
    };
    let spar_command::Step::Pipeline(pipeline) = &plan.steps[0].1 else {
        panic!("expected pipeline")
    };
    assert_eq!(pipeline.commands.len(), 2);
    assert_eq!(
        pipeline.commands[1].stdout,
        Some(spar_command::Redirection::File {
            path: "out.log".into(),
            mode: spar_command::RedirectMode::Truncate,
        })
    );
}

#[test]
fn eval_shell_plus_shell_composes_in_order() {
    let result = eval_src(
        r#"
        function lint() -> shell { return shell { cargo clippy; }; };
        function build() -> shell { return shell { cargo build; }; };
        var x: shell = lint() + build();
        "#,
    );
    let crate::evaluator::ConfigValue::Shell(plan) = &result.globals["x"] else {
        panic!("expected ConfigValue::Shell")
    };
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].0, spar_command::Join::Always);
    assert_eq!(plan.steps[1].0, spar_command::Join::Always);
    let spar_command::Step::Command(second) = &plan.steps[1].1 else {
        panic!("expected command")
    };
    assert_eq!(second.program, "cargo");
    assert_eq!(second.args, ["build"]);
}

#[test]
fn deferred_shell_interpolation_uses_module_variables() {
    let result = eval_src(
        r#"
        var name: str = "OCC";
        var plan: shell = shell { echo "Hello ${name}"; };
        "#,
    );
    let crate::evaluator::ConfigValue::Shell(plan) = &result.globals["plan"] else {
        panic!("expected ConfigValue::Shell")
    };
    let spar_command::Step::Command(command) = &plan.steps[0].1 else {
        panic!("expected command")
    };
    assert_eq!(command.program, "echo");
    assert_eq!(command.args, ["Hello OCC"]);
}

#[test]
fn shell_returning_function_interpolates_named_argument() {
    let result = eval_src(
        r#"
        function greet(name: str) -> shell {
            return shell { echo "Hello ${name}"; };
        };
        var plan: shell = greet(name: "OCC");
        "#,
    );
    let crate::evaluator::ConfigValue::Shell(plan) = &result.globals["plan"] else {
        panic!("expected ConfigValue::Shell")
    };
    let spar_command::Step::Command(command) = &plan.steps[0].1 else {
        panic!("expected command")
    };
    assert_eq!(command.args, ["Hello OCC"]);
}

#[test]
fn eval_shell_construction_at_module_scope_is_deferred_data() {
    eval_src("var x: shell = shell { this-program-does-not-exist-xyz; };");
}

#[test]
fn runtime_errors_without_their_own_span_point_at_the_failing_expression() {
    // Exceeding the call depth carries no span of its own; the diagnostic
    // must still land on the line of the expression that ran away, not on
    // line 0/1 of the file.
    let src = "var pad: int = 1;\n\nfunction spin(n: int) -> int {\n    return spin(n: n);\n};\n\nvar result: int = spin(n: 1);\n";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap();
    let error = crate::evaluator::Evaluator::new(symbols, prog)
        .run()
        .unwrap_err();
    let crate::error::SparError::EvalError { span, message } = error else {
        panic!("expected an eval error");
    };
    assert!(message.contains("maximum call depth"), "{message}");
    assert!(span.line >= 4, "span was {span:?}");
}

/// Regression: a JSON object whose value is itself a Record with a nested
/// array field (e.g. `{"verses": [...]}`) has no clean way to pull that
/// field back out as a `List<Record>` — `Record` only bridged scalars
/// (`asStr`/`asInt`/`asFloat`/`asBool`). `Record.asList()` closes that gap.
/// `parse`/method calls on a `Record` need the compiled runtime (they're
/// rejected under plain `emit_source`), so this goes through
/// `execute_self_contained_entry` the same way `runtime_tests.rs` does.
#[test]
fn record_as_list_bridges_a_nested_json_array_field() {
    let program = crate::Engine::default()
        .compile_source(
            r#"
            import pkg { parse } from "std/json";
            async function main() -> int {
                var raw: str = "{\"verses\":[{\"book\":\"John\"},{\"book\":\"Mark\"}]}";
                var parsed: Record = parse<Record>(text: raw);
                var verses: List<Record> = parsed.verses.asList();
                return len(value: verses);
            };
            "#,
        )
        .unwrap();
    let value = crate::runtime::execute_self_contained_entry(&program).unwrap();
    assert_eq!(value, crate::Value::Int(2));
}

#[test]
fn record_as_list_rejects_a_non_list_field() {
    let program = crate::Engine::default()
        .compile_source(
            r#"
            import pkg { parse } from "std/json";
            async function main() -> int {
                var raw: str = "{\"verses\":\"not a list\"}";
                var parsed: Record = parse<Record>(text: raw);
                var verses: List<Record> = parsed.verses.asList();
                return len(value: verses);
            };
            "#,
        )
        .unwrap();
    let error = crate::runtime::execute_self_contained_entry(&program).unwrap_err();
    assert!(
        error.iter().any(|e| e.to_string().contains("asList")),
        "{:?}",
        error
    );
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn eval_recursion_supports_one_thousand_calls() {
    let result = eval_src("function count(n: int) -> int { if n == 0 { return 0; } return 1 + count(n: n - 1); }; var result: int = count(n: 999);");
    assert_eq!(result.globals["result"], crate::evaluator::ConfigValue::Int(999));
}

#[test]
fn eval_tilde_statement_runs_in_evaluator_tier() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("eval.txt");
    let src = format!(r#"
        function f() -> int {{
            ~ echo ev > "{}";
            return 1;
        }};
        export var r: int = f();
    "#, marker.display());
    eval_src(&src);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "ev\n");
}
