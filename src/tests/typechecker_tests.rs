fn check_ok(src: &str) {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .expect("resolve");
    crate::typechecker::TypeChecker::check(&prog, &symbols)
        .expect("type check failed unexpectedly");
}

fn check_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .expect("resolve");
    match crate::typechecker::TypeChecker::check(&prog, &symbols) {
        Ok(_) => panic!("expected type error"),
        Err(e) => format!("{:?}", e),
    }
}

fn resolve_or_type_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = match crate::resolver::Resolver::new().resolve(&prog, &[]) {
        Ok(symbols) => symbols,
        Err(errors) => return format!("{errors:?}"),
    };
    match crate::typechecker::TypeChecker::check(&prog, &symbols) {
        Ok(_) => panic!("expected resolve or type error"),
        Err(errors) => format!("{errors:?}"),
    }
}


#[test]
fn any_accepts_concrete_values_but_does_not_flow_back_implicitly() {
    use crate::ast::SparType;
    use crate::typechecker::is_assignable;

    for concrete in [
        SparType::Int,
        SparType::Str,
        SparType::List(Box::new(SparType::Int)),
        SparType::Applied {
            name: "Map".into(),
            arguments: vec![SparType::Str, SparType::Int],
        },
        SparType::Named("User".into()),
        SparType::Applied {
            name: "Option".into(),
            arguments: vec![SparType::Int],
        },
        SparType::Applied {
            name: "Result".into(),
            arguments: vec![SparType::Int, SparType::Str],
        },
    ] {
        assert!(is_assignable(&SparType::Any, &concrete), "{concrete:?}");
    }
    assert!(!is_assignable(&SparType::Int, &SparType::Any));

    check_ok("var integer: Any = 7; var text: Any = \"spar\"; var list: Any = [1, 2, 3];");

    let errors = check_err("var opaque: Any = 7; var concrete: int = opaque;");
    assert!(errors.contains("declared as 'int'") || errors.contains("declared as `int`"), "{errors}");
}

#[test]
fn async_call_returns_promise_and_await_unwraps_it() {
    check_ok(
        r#"
        async function identity<T>(value: T) -> T { return value; };
        async function main() -> int {
            var pending: Promise<int> = identity(value: 7);
            var answer: int = await pending;
            return answer;
        };
        "#,
    );
}

#[test]
fn await_in_sync_function_is_rejected() {
    let errors = check_err(
        "async function value() -> int { return 1; }; function main() -> int { return await value(); };",
    );
    assert!(
        errors.contains("only valid inside an async function"),
        "{errors}"
    );
}

#[test]
fn missing_await_has_actionable_hint() {
    let errors = check_err(
        "async function value() -> int { return 1; }; async function main() -> int { var answer: int = value(); return answer; };",
    );
    assert!(errors.contains("await"), "{errors}");
}

#[test]
fn promise_requires_exactly_one_type_argument() {
    let no_argument = resolve_or_type_err(
        "async function value() -> int { return 1; }; async function main() -> int { var pending: Promise = value(); return await pending; };",
    );
    assert!(
        no_argument.contains("expects 1 type argument"),
        "{no_argument}"
    );

    let two_arguments = resolve_or_type_err(
        "async function value() -> int { return 1; }; async function main() -> int { var pending: Promise<int, str> = value(); return await pending; };",
    );
    assert!(
        two_arguments.contains("expects 1 type argument"),
        "{two_arguments}"
    );
}

#[test]
fn awaiting_non_promise_is_rejected() {
    let errors = check_err("async function main() -> int { return await 1; };");
    assert!(errors.contains("expected `Promise<T>`"), "{errors}");
}

#[test]
fn panic_requires_a_string_message() {
    let errors = check_err("async function main() -> int { panic(message: 1); };");
    assert!(errors.contains("expects str"), "{errors}");
}

#[test]
fn top_level_await_is_rejected() {
    let errors =
        check_err("async function value() -> int { return 1; }; var answer: int = await value();");
    assert!(
        errors.contains("only valid inside an async function"),
        "{errors}"
    );
}

#[test]
fn generic_function_calls_infer_and_accept_explicit_types() {
    check_ok(
        r#"
        function identity<T>(value: T) -> T { return value; };
        var inferred: int = identity(value: 7);
        var explicit: str = identity<str>(value: "seven");
        "#,
    );
}

#[test]
fn generic_calls_support_multiple_nested_and_partial_explicit_arguments() {
    check_ok(
        r#"
        function first<T, U>(left: T, right: U) -> T { return left; };
        function passthrough<T>(values: [T]) -> [T] { return values; };
        var partial: int = first<int>(left: 1, right: "ignored");
        var nested: [str] = passthrough(values: ["a", "b"]);
        "#,
    );
}

#[test]
fn generic_type_parameter_can_be_forwarded_explicitly() {
    check_ok(
        r#"
        function identity<T>(value: T) -> T { return value; };
        function forward<T>(value: T) -> T { return identity<T>(value: value); };
        var result: int = forward(value: 5);
        "#,
    );
}

#[test]
fn generic_inference_rejects_conflicts_and_unresolved_parameters() {
    let conflict = check_err(
        "function same<T>(left: T, right: T) -> T { return left; }; var x: int = same(left: 1, right: \"one\");",
    );
    assert!(conflict.contains("conflicting inference"), "{conflict}");

    // With an expected type the parameter is inferred from it; without one it
    // stays unresolved.
    check_ok("function make<T>() -> T { return 1; }; var x: int = make();");
    let unresolved = check_err("function make<T>() -> T { return 1; }; make();");
    assert!(
        unresolved.contains("explicit type argument"),
        "{unresolved}"
    );
}

#[test]
fn unconstrained_generic_parameters_reject_arithmetic_but_allow_equality() {
    let addition = check_err("function add<T>(left: T, right: T) -> T { return left + right; };");
    assert!(addition.contains("binary expression"), "{addition}");

    check_ok("function equal<T>(left: T, right: T) -> bool { return left == right; };");
}

#[test]
fn applied_generic_type_substitutes_fields() {
    check_ok(
        r#"
        struct Box<T> { value: T; };
        var boxed: Box<int> = Box<int>(value: 7);
        var value: int = boxed.value;
        "#,
    );

    let mismatch = check_err(
        r#"struct Box<T> { value: T; }; var broken: Box<int> = Box<int>(value: "bad");"#,
    );
    assert!(mismatch.contains("int"), "{mismatch}");
}

#[test]
fn canonical_structs_conform_to_generic_types_and_lists() {
    check_ok(r#"struct Pair<T, V> { left: T; right: V; }; var example: Pair<str, int> = Pair<str, int>(left: "hello", right: 42); var names: List<str> = ["Obi", "Ada"];"#);
    let error = check_err(r#"struct Pair<T, V> { left: T; right: V; }; var broken: Pair<str, int> = Pair<str, int>(left: 42, right: "wrong");"#);
    assert!(error.contains("str"), "{error}");
}

#[test]
fn structural_type_defaults_are_checked_and_satisfy_required_fields() {
    check_ok(r#"struct Server { host: str = "localhost"; port: int = 8080; debug: bool = false; }; var development: Server = Server(debug: true);"#);
    let error = check_err(r#"struct Server { port: int = "wrong"; };"#);
    assert!(error.contains("int"), "{error}");
}

#[test]
fn caught_error_binding_has_error_fields_and_ignored_catch_is_valid() {
    check_ok(
        r#"
        function main() -> void {
            try { return; } catch err {
                var message: str = err.message;
                var kind: str = err.kind;
            }
            try { return; } catch { return; }
        };
        "#,
    );
}

#[test]
fn nested_applied_generic_fields_are_instantiated_recursively() {
    check_ok(
        r#"
        struct Pair<T, U> { left: T; right: U; };
        struct Box<T> { value: T; };
        var nested: Box<Pair<int, str>> = Box<Pair<int, str>>(value: Pair<int, str>(left: 1, right: "one"));
        var left: int = nested.value.left;
        "#,
    );
}

#[test]
fn shell_block_has_shell_type() {
    check_ok("var x: shell = shell { echo hi; };");
}

#[test]
fn shell_type_mismatch_errors() {
    let errors = check_err("var x: int = shell { echo hi; };");
    assert!(errors.contains("type mismatch"), "got: {errors}");
}

#[test]
fn exec_shell_has_exec_result_type() {
    check_ok(
        r#"
        struct ExecResult{ success: bool; exitCode: int; };
        function f() -> bool {
            var r = exec shell { true; };
            return r.success;
        };
        "#,
    );
}

#[test]
fn shell_plus_shell_is_legal() {
    check_ok(
        r#"
        function a() -> shell { return shell { true; }; };
        function b() -> shell { return shell { true; }; };
        var x: shell = a() + b();
        "#,
    );
}

#[test]
fn shell_plus_int_is_a_clear_error() {
    let errors = check_err("var x: shell = shell { true; } + 1;");
    assert!(errors.to_lowercase().contains("shell"), "got: {errors}");
}

/// Regression: a `var` statement inside a `shell { ... }` block calling a
/// method that doesn't exist used to pass `spar check` with zero
/// diagnostics — `check_mixed_shell_with_locals` only ever validated the
/// block's `.steps` (mixed-pipeline decoders), never its ordinary
/// `.statements` — and only crashed later, during lowering, with an opaque
/// internal error. The identical code outside a shell block always produced
/// this same clean error. See spar_shell_block_method_validation_gap.md.
#[test]
fn shell_block_statement_with_unknown_method_call_is_a_clear_type_error() {
    let errors = check_err(
        r#"
        struct Widget { name: str; };
        impl Widget {
            fn label(self) -> str { return self.name; };
        };
        function main() -> shell {
            return shell {
                var w: Widget = Widget(name: "gauge");
                var text: str = w.missingMethod();
            };
        };
        "#,
    );
    assert!(
        errors.contains("no method 'missingMethod'"),
        "got: {errors}"
    );
}

/// The fix must not regress ordinary, valid statements inside a shell block
/// — a real method call, a field access, and a local var all still
/// typecheck cleanly.
#[test]
fn shell_block_statement_with_valid_method_call_still_checks_ok() {
    check_ok(
        r#"
        struct Widget { name: str; };
        impl Widget {
            fn label(self) -> str { return self.name; };
        };
        function main() -> shell {
            return shell {
                var w: Widget = Widget(name: "gauge");
                var text: str = w.label();
                echo ${text};
            };
        };
        "#,
    );
}

/// Regression: the shell-block validation fix above initially broke a bare
/// `return;` inside a `shell { ... }` block's `if` — a real, live failure
/// ("function declares return type 'Any' but this 'return;' provides no
/// value") once a user's actual sparsh session hit it. `check_return_value`
/// only accepts `ReturnValue::Void` against `SparType::Void`; the shell-block
/// checker uses `SparType::Any` as a stand-in for the (unavailable) enclosing
/// function's real return type, and `Any` alone still rejects a void return.
/// Fixed via `TypeChecker::in_shell_statement_scope`.
#[test]
fn shell_block_bare_early_return_inside_if_does_not_false_positive() {
    check_ok(
        r#"
        function main() -> shell {
            return shell {
                var x: int = 1;
                if x == 1 {
                    return;
                }
                echo done;
            };
        };
        "#,
    );
}

#[test]
fn typecheck_function_arg_type_mismatch() {
    let src = r#"
        function double(x: int) -> int { return x; };
        var y: int = double(x: "not an int");
    "#;
    let err = check_err(src);
    assert!(err.contains("int") || err.contains("str") || err.contains("type"));
}

#[test]
fn typecheck_call_expression_statements_in_nested_blocks() {
    check_ok(
        r#"
        function sink(value: int) -> int { return value; };
        function main() -> int {
            sink(value: 1);
            if true { sink(value: 2); }
            for item in [3] { sink(value: item); }
            return 0;
        };
        "#,
    );
}

#[test]
fn indexed_for_binds_int_index_and_list_element_type() {
    check_ok(
        r#"
        function inspect(values: [str]) -> int {
            for (index, value) in values {
                var checkedIndex: int = index;
                var checkedValue: str = value;
                return checkedIndex;
            }
            return 0;
        };
        "#,
    );
}

#[test]
fn module_if_and_for_are_typechecked() {
    check_ok(
        r#"
        function sink(value: int) -> int { return value; };
        if true { sink(value: 1); }
        for (index, value) in [2] { sink(value: index + value); }
        "#,
    );
}

#[test]
fn mutable_assignment_must_match_the_declared_type() {
    check_ok("var mut count: int = 0; count = 1;");
    let error = check_err("var mut count: int = 0; count = \"wrong\";");
    assert!(error.contains("assigned") && error.contains("int") && error.contains("str"));
}

#[test]
fn void_function_accepts_bare_return_and_implicit_fallthrough() {
    check_ok("function noisy() -> void { return; };");
    check_ok("function quiet() -> void { };");
}

#[test]
fn void_function_rejects_a_value_returning_return() {
    let error = check_err("function f() -> void { return 1; };");
    assert!(error.contains("void"), "{error}");
}

#[test]
fn non_void_function_rejects_bare_return() {
    let error = check_err("function f() -> int { return; };");
    assert!(
        error.contains("int") && error.contains("no value"),
        "{error}"
    );
}

#[test]
fn host_call_return_type_typechecks_against_declared_var_type() {
    let mut hosts = crate::host::HostRegistry::new();
    hosts
        .register(crate::host::HostFunction::new(
            "math",
            "answer",
            vec![],
            crate::ast::SparType::Int,
            |_| Ok(crate::evaluator::ConfigValue::Int(42)),
        ))
        .unwrap();

    let src = "var x: int = math::answer();";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .with_hosts(hosts.clone())
        .resolve(&prog, &[])
        .unwrap();
    crate::typechecker::TypeChecker::check(&prog, &symbols)
        .expect("host call return type should match declared var type");

    let bad_src = "var x: str = math::answer();";
    let tokens = crate::lexer::Lexer::new(bad_src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let symbols = crate::resolver::Resolver::new()
        .with_hosts(hosts)
        .resolve(&prog, &[])
        .unwrap();
    let error = crate::typechecker::TypeChecker::check(&prog, &symbols).unwrap_err();
    assert!(!error.is_empty());
}

#[test]
fn typecheck_function_parameter_default_type_mismatch() {
    let error = check_err(r#"function greet(name: str = 42) -> str { return name; };"#);
    assert!(
        error.contains("parameter 'name' default") && error.contains("str"),
        "{error}"
    );
}

#[test]
fn typecheck_function_return_type_mismatch() {
    let src = r#"function f(x: str) -> int { return x; };"#;
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
fn record_field_can_be_function_call() {
    let src = r#"
        function makeServer(host: str) -> Record {
            return { host: host; };
        };
        struct App {
            server: Record = makeServer(host: "localhost");
        };
    "#;
    check_ok(src);
}

#[test]
fn typecheck_if_condition_must_be_bool() {
    let src = r#"
        function f(x: int) -> str {
            if x { return "a"; } else { return "b"; }
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("bool") || err.contains("int"));
}

#[test]
fn typecheck_function_body_call_arg_type_mismatch() {
    // Wrong arg type inside a function body must be caught
    let src = r#"
        function double(x: int) -> int { return x; };
        function caller(s: str) -> int {
            var result: int = double(x: s);
            return result;
        };
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
        };
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
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("list") || err.contains("str"));
}

#[test]
fn for_loop_over_list_typechecks_ok() {
    check_ok(
        r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
            return 0;
        };
    "#,
    );
}

#[test]
fn nested_for_loops_typecheck() {
    check_ok(
        r#"
        function flatten(grid: [[int]]) -> int {
            for row in grid {
                for cell in row {
                    if cell > 100 { return cell; }
                }
            }
            return 0;
        };
    "#,
    );
}

#[test]
fn record_object_return_typechecks() {
    check_ok(
        r#"
        function f(major: int) -> Record {
            if major <= 0 {
                return { error: true; message: "bad"; };
            }
            return { error: false; };
        };
    "#,
    );
}

#[test]
fn typecheck_valid_type_binding_with_inferred_field_types_passes() {
    let src = r#"
        struct PostgresType{
            image: str;
            restart: Option<str> = none();
        };
        var postgres: PostgresType = PostgresType(image: "postgres:16");
    "#;
    // Goes through Engine (not the bare check_ok helper) because `none()`
    // is a prelude function spliced in by inject_prelude, not visible to
    // the bare `Resolver::new()` path.
    crate::Engine::default()
        .check_source(src)
        .expect("type check failed unexpectedly");
}

#[test]
fn typecheck_valid_type_binding_with_explicit_redundant_type_passes() {
    check_ok(r#"struct Postgres { image: str = "postgres:16"; }; var postgres: Postgres = Postgres();"#);
}

#[test]
fn typecheck_type_binding_missing_required_field() {
    let error = check_err(r#"struct Postgres { image: str; }; var postgres: Postgres = Postgres();"#);
    assert!(error.contains("missing required argument"), "{error}");
}

#[test]
fn typecheck_type_binding_rejects_extra_field() {
    let error = resolve_or_type_err(r#"struct Postgres { image: str; }; var postgres: Postgres = Postgres(image: "postgres:16", extra: "not allowed");"#);
    assert!(error.contains("extra"), "{error}");
}

#[test]
fn typecheck_type_binding_rejects_wrong_inferred_type() {
    let error = check_err(r#"struct Postgres { image: str; }; var postgres: Postgres = Postgres(image: 16);"#);
    assert!(error.contains("str"), "{error}");
}

#[test]
fn typecheck_type_binding_rejects_wrong_explicit_type() {
    let error = check_err(r#"struct Postgres { image: str; }; var postgres: Postgres = Postgres(image: 16);"#);
    assert!(error.contains("str"), "{error}");
}

#[test]
fn typecheck_type_binding_validates_named_nested_type_with_inferred_fields() {
    let src = r#"
        struct Border{
            width: int;
        };
        struct Decoration{
            border: Border;
        };
        var style: Decoration = Decoration(border: Border(width: 4));
    "#;
    check_ok(src);
}

#[test]
fn typecheck_type_binding_rejects_bad_named_nested_field() {
    let src = r#"
        struct Border{
            width: int;
        };
        struct Decoration{
            border: Border;
        };
        var style: Decoration = Decoration(border: Border(width: "not an int"));
    "#;
    let err = check_err(src);
    assert!(err.contains("expects int but got str"), "got: {err}");
}

#[test]
fn typecheck_untyped_field_without_binding_is_error() {
    let errors = crate::Engine::default().check_source("struct Server { port = 8080; };").unwrap_err();
    assert!(errors.iter().any(|error| error.to_string().contains("expected ':'")));
}

#[test]
fn typecheck_unbound_section_still_requires_explicit_types() {
    // Regression: sections with no binding are completely unaffected.
    check_ok(r#"struct Man { name: str = "Mike"; };"#);
}

// ── Spread in nested field bodies ────────────────────────────────────────

#[test]
fn spread_in_nested_field_matching_bound_source_passes() {
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct ProductionEnvironment: EnvironmentType {
            nodeEnv: "production";
            port: "3000";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    check_ok(src);
}

#[test]
fn spread_in_nested_field_missing_required_field_errors() {
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct PartialEnvType{ nodeEnv: str; };
        struct Partial: PartialEnvType {
            nodeEnv: "production";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...Partial; };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("missing required field") && err.contains("port"),
        "got: {err}"
    );
}

#[test]
fn spread_in_nested_field_wrong_primitive_type_errors() {
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct BadEnvType{ nodeEnv: str; port: int; };
        struct Bad: BadEnvType {
            nodeEnv: "production";
            port: 3000;
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...Bad; };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("port") && err.contains("int") && err.contains("str"),
        "got: {err}"
    );
}

#[test]
fn spread_in_nested_field_extra_field_errors() {
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct ExtraEnvType{ nodeEnv: str; port: str; extra: str; };
        struct WithExtra: ExtraEnvType {
            nodeEnv: "production";
            port: "3000";
            extra: "surprise";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...WithExtra; };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("extra") && err.contains("not declared"),
        "got: {err}"
    );
}

#[test]
fn spread_in_nested_field_unbound_source_with_explicit_types_passes() {
    // Source has no `-> Type` binding, but every field is explicitly
    // typed — shape derives straight from those, no Named type needed.
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct ProductionEnvironment {
            nodeEnv: str = "production";
            port: str = "3000";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    check_ok(src);
}

#[test]
fn spread_in_nested_field_unbound_source_wrong_shape_errors() {
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct ProductionEnvironment {
            nodeEnv: str = "production";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("missing required field") && err.contains("port"),
        "got: {err}"
    );
}

#[test]
fn spread_only_top_level_bound_section_checked_against_whole_type() {
    check_ok(r#"struct Environment { nodeEnv: str; port: str; }; var production: Environment = Environment(nodeEnv: "production", port: "3000"); var backup: Environment = Environment(nodeEnv: production.nodeEnv, port: production.port);"#);
}

#[test]
fn spread_only_top_level_bound_section_wrong_shape_errors() {
    let error = check_err(r#"struct Environment { nodeEnv: str; port: str; }; var backup: Environment = Environment(nodeEnv: "production");"#);
    assert!(error.contains("missing required argument 'port'"), "{error}");
}

#[test]
fn spread_mixed_with_explicit_fields_full_coverage_passes() {
    // A resolvable spread's contribution is now merged with the explicit
    // fields for coverage purposes: Partial covers nodeEnv, the explicit
    // field covers port — together they satisfy EnvironmentType.
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct PartialEnvType{ nodeEnv: str; };
        struct Partial: PartialEnvType {
            nodeEnv: "production";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...Partial; port: "3000"; };
        };
    "#;
    check_ok(src);
}

#[test]
fn spread_mixed_with_explicit_fields_still_missing_required_errors() {
    // Neither the spread nor the explicit fields cover `port` — must
    // still be a missing-required-field error, not silently accepted.
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        struct PartialEnvType{ nodeEnv: str; };
        struct Partial: PartialEnvType {
            nodeEnv: "production";
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...Partial; };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("missing required field") && err.contains("port"),
        "got: {err}"
    );
}

#[test]
fn spread_mixed_with_explicit_fields_contributes_undeclared_field_errors() {
    // Direct regression for the reported bug: a spread mixed with an
    // explicit field, where the spread's source has fields the target
    // type doesn't declare at all, must be a type error — not silently
    // accepted just because it's "mixed" with another field.
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; databaseUrl: str; redisUrl: str; };
        struct VolumeType{ postgresData: [str]; };
        struct PostgresType{ volumes: VolumeType; };
        struct ProductionEnvironment: EnvironmentType {
            nodeEnv: "production";
            port: "3000";
            databaseUrl: "None";
            redisUrl: "None";
        };
        struct Postgres: PostgresType {
            volumes: {
                postgresData: ["postgres_data:/var/lib/postgresql/data"];
                ...ProductionEnvironment;
            };
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("nodeEnv") && err.contains("not declared"),
        "got: {err}"
    );
}

#[test]
fn spread_mixed_with_unresolvable_source_still_skipped() {
    // A spread whose source can't be statically resolved (a function
    // call) keeps the conservative "can't verify, skip" fallback, even
    // when mixed with other fields — this deliberately has a WRONG shape
    // (missing `port`) and must still pass.
    let src = r#"
        struct EnvironmentType{ nodeEnv: str; port: str; };
        struct ServiceType{ image: str; environment: EnvironmentType; };
        function makeEnv() -> Record {
            return { nodeEnv: "production"; };
        };
        struct Api: ServiceType {
            image: "my-api";
            environment: { ...makeEnv(); };
        };
    "#;
    check_ok(src);
}

// ── Dynamic object literals vs named structured values ────────────────────────

#[test]
fn object_literal_cannot_implicitly_construct_named_type() {
    let err = check_err("struct Leaf { name: str = \"\"; size: int = 0; };\nvar x: Leaf = { name: \"a\"; size: 1; };\n");
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn named_type_constructor_is_required_for_structured_value() {
    check_ok("struct Leaf { name: str = \"\"; size: int = 0; };\nvar x: Leaf = Leaf(name: \"a\", size: 1);\n");
}

#[test]
fn object_literal_against_primitive_type_errors() {
    let err = check_err("var x: int = { a: 1; };");
    assert!(err.contains("object literal"), "got: {err}");
}

#[test]
fn list_of_named_type_requires_named_constructors() {
    let err = check_err(
        "struct Leaf { name: str = \"\"; };\nvar xs: [Leaf] = [{ name: \"a\"; }, { name: \"b\"; }];\n",
    );
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn list_of_named_type_constructors_passes() {
    check_ok(
        "struct Leaf { name: str = \"\"; };\nvar xs: [Leaf] = [Leaf(name: \"a\"), Leaf(name: \"b\")];\n",
    );
}

#[test]
fn nested_named_values_require_named_constructors() {
    check_ok(concat!(
        "struct Branch { label: str = \"\"; };\n",
        "struct Leaf { name: str = \"\"; sub: Branch = Branch(label: \"\"); };\n",
        "var x: Leaf = Leaf(name: \"a\", sub: Branch(label: \"b\"));\n",
    ));
}

// ── Function-body structured-value validation ─────────────────────────────────

#[test]
fn local_var_named_type_requires_constructor() {
    let err = check_err(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function f() -> str {\n",
        "    var l: Leaf = { name: \"a\"; };\n",
        "    return \"ok\";\n",
        "};\n",
    ));
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn local_var_named_type_constructor_passes() {
    check_ok(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function f() -> str {\n",
        "    var l: Leaf = Leaf(name: \"a\");\n",
        "    return \"ok\";\n",
        "};\n",
    ));
}

#[test]
fn function_return_bare_object_literal_cannot_construct_named_type() {
    let err = check_err(concat!(
        "struct Leaf { name: str = \"\"; size: int = 0; };\n",
        "function makeLeaf(n: str) -> Leaf {\n",
        "    return { name: n; size: 0; };\n",
        "};\n",
    ));
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn function_return_named_constructor_passes() {
    check_ok(concat!(
        "struct Leaf { name: str = \"\"; size: int = 0; };\n",
        "function makeLeaf(n: str) -> Leaf {\n",
        "    return Leaf(name: n, size: 0);\n",
        "};\n",
    ));
}

#[test]
fn function_return_list_of_named_type_requires_constructors() {
    let err = check_err(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function makeLeaves() -> [Leaf] {\n",
        "    return [{ name: \"a\"; }, { name: \"b\"; }];\n",
        "};\n",
    ));
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn function_return_list_of_named_type_constructors_passes() {
    check_ok(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function makeLeaves() -> [Leaf] {\n",
        "    return [Leaf(name: \"a\"), Leaf(name: \"b\")];\n",
        "};\n",
    ));
}

#[test]
fn record_return_uses_dynamic_object_literal() {
    check_ok(concat!(
        "function borderConf() -> Record {\n",
        "    return { sides: [2, 4]; width: 5; };\n",
        "};\n",
    ));
}

// ── Function-call argument checking for named structured values ───────────────

#[test]
fn function_call_named_type_requires_constructor() {
    let err = check_err(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function useLeaf(l: Leaf) -> str { return \"ok\"; };\n",
        "var r: str = useLeaf(l: { name: \"a\"; });\n",
    ));
    assert!(
        err.contains("constructor") || err.contains("Leaf("),
        "got: {err}"
    );
}

#[test]
fn function_call_named_type_constructor_passes() {
    check_ok(concat!(
        "struct Leaf { name: str = \"\"; };\n",
        "function useLeaf(l: Leaf) -> str { return \"ok\"; };\n",
        "var r: str = useLeaf(l: Leaf(name: \"a\"));\n",
    ));
}

// ── enum variant type-checking ────────────────────────────────────────────────

#[test]
fn enum_variant_matching_declared_enum_type_passes() {
    check_ok("enum Devices { Ios, Android };\nvar x: Devices = Devices::Android;\n");
}

#[test]
fn enum_variant_from_wrong_enum_errors() {
    let err = check_err(concat!(
        "enum Devices { Ios, Android };\n",
        "enum Os { Linux, Windows };\n",
        "var x: Devices = Os::Linux;\n",
    ));
    assert!(err.contains("Devices") || err.contains("Os"), "got: {err}");
}

#[test]
fn plain_string_literal_rejected_for_enum_typed_field() {
    let err = check_err("enum Devices { Ios, Android };\nvar x: Devices = \"Android\";\n");
    assert!(err.contains("Devices") || err.contains("str"), "got: {err}");
}


#[test]
fn named_function_arguments_can_be_reordered() {
    check_ok(
        r#"
        fn describe(name: str, age: int) -> str { return name; };
        var value: str = describe(age: 34, name: "Mike");
        "#,
    );
}

#[test]
fn named_function_call_rejects_duplicate_argument() {
    // This validation now runs during resolve, not typecheck (see
    // struct_constructor_rejects_unknown_named_argument below for the same
    // shift on struct constructors) — resolve_or_type_err reports whichever
    // stage errors first.
    let errors = resolve_or_type_err(
        r#"
        fn greet(name: str) -> str { return name; };
        var value: str = greet(name: "Mike", name: "Obi");
        "#,
    );
    assert!(errors.contains("duplicate argument 'name'"), "got: {errors}");
}

#[test]
fn named_function_call_rejects_unknown_argument() {
    let errors = resolve_or_type_err(
        r#"
        fn greet(name: str) -> str { return name; };
        var value: str = greet(value: "Mike");
        "#,
    );
    assert!(errors.contains("has no param 'value'"), "got: {errors}");
}

#[test]
fn named_function_call_rejects_missing_required_argument() {
    let errors = resolve_or_type_err(
        r#"
        fn greet(name: str, prefix: str) -> str { return name; };
        var value: str = greet(name: "Mike");
        "#,
    );
    assert!(
        errors.contains("missing arguments for function 'greet'") && errors.contains("prefix"),
        "got: {errors}"
    );
}

#[test]
fn named_function_call_allows_omitting_defaulted_argument() {
    check_ok(
        r#"
        fn greet(name: str, prefix: str = "Hello") -> str { return name; };
        var value: str = greet(name: "Mike");
        "#,
    );
}

#[test]
fn function_valued_parameter_is_called_with_its_declared_parameter_name() {
    check_ok(
        r#"
        fn apply(callback: fn(value: int) -> int) -> int {
            return callback(value: 7);
        };
        fn double(value: int) -> int { return value * 2; };
        var answer: int = apply(callback: double);
        "#,
    );
}

#[test]
fn function_valued_parameter_rejects_wrong_named_argument() {
    let errors = check_err(
        r#"
        fn apply(callback: fn(value: int) -> int) -> int {
            return callback(input: 7);
        };
        "#,
    );
    assert!(errors.contains("no parameter named 'input'"), "got: {errors}");
}

#[test]
fn struct_constructor_rejects_unknown_named_argument() {
    // Struct-constructor argument validation now runs during resolve, not
    // typecheck.
    let errors = resolve_or_type_err(
        r#"
        struct User { name: str = ""; age: int = 0; };
        var user: User = User(name: "Mike", unknown: 1);
        "#,
    );
    assert!(errors.contains("has no field 'unknown'"), "got: {errors}");
}

#[test]
fn named_field_access_on_global_var_has_correct_type() {
    let src = r#"
        struct Human { name: str = ""; age: int = 0; };
        var person: Human = Human(name: "Mike", age: 5);
        var pname: str = person.name;
    "#;
    check_ok(src);
}

#[test]
fn named_field_access_on_global_var_type_mismatch_errors() {
    let src = r#"
        struct Human { name: str = ""; age: int = 0; };
        var person: Human = Human(name: "Mike", age: 5);
        var pname: int = person.name;
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn named_field_access_on_loop_var_has_correct_type() {
    let src = r#"
        struct Human{ name: str; age: int; };
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        };
    "#;
    check_ok(src);
}

#[test]
fn local_function_group_call_return_type_used_in_another_functions_return() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
            function useOnly() -> int {
                return EdgeInsect::only();
            }
        };
    "#;
    check_ok(src);
}

#[test]
fn local_function_group_call_return_type_mismatch_errors() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
        var x: str = EdgeInsect::only();
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn dot_field_access_on_global_var_has_correct_type() {
    let src = r#"
        struct Human { name: str = ""; age: int = 0; };
        var person: Human = Human(name: "Mike", age: 5);
        var pname: str = person.name;
    "#;
    check_ok(src);
}

#[test]
fn dot_field_access_type_mismatch_errors() {
    let src = r#"
        struct Human { name: str = ""; age: int = 0; };
        var person: Human = Human(name: "Mike", age: 5);
        var pname: int = person.name;
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn dot_field_access_on_loop_var_has_correct_type() {
    let src = r#"
        struct Human{ name: str; age: int; };
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        };
    "#;
    check_ok(src);
}

#[test]
fn self_dot_field_access_has_correct_type() {
    check_ok("struct Server { port: int = 8080; }; impl Server { fn doubled(self) -> int { return self.port + self.port; }; };");
}

#[test]
fn self_dot_field_access_type_mismatch_errors() {
    let error = check_err("struct Server { port: int = 8080; }; impl Server { fn bad(self) -> str { return self.port; }; };");
    assert!(error.contains("str"), "{error}");
}

#[test]
fn bound_type_accepts_list_of_named_object_literals() {
    check_ok(
        r#"
        struct AliasConfig {
            name: str;
            command: List<str>;
        };
        struct RootConfig {
            aliases: Option<List<AliasConfig>> = none();
        };
        struct Config: RootConfig {
            aliases = [
                { name: "ll"; command: ["eza", "--icons"]; }
            ];
        };
        "#,
    );
}

#[test]
fn contextual_native_shell_words_typecheck_as_ordinary_names() {
    check_ok(
        r#"
        struct Tool {
            command: str = "";
            exec: str = "";
            shell: str = "";
        };
        var command: str = "run";
        var exec: str = command;
        var shell: str = exec;
        var tool: Tool = Tool(command: command, exec: exec, shell: shell);
        function command(exec: str, shell: str) -> str {
            return "${exec}:${shell}";
        };
        var result: str = command(exec: tool.exec, shell: tool.shell);
        "#,
    );
}

#[test]
fn named_type_constructor_supports_typed_nested_values() {
    check_ok(
        r#"
        struct Address {
            country: str = "";
            city: str = "";
        };
        struct User {
            name: str = "";
            address: Address = Address();
        };
        var user: User = User(
            name: "Mike",
            address: Address(country: "Nigeria", city: "Awka"),
        );
        "#,
    );
}

#[test]
fn object_literal_is_not_an_implicit_named_struct_value() {
    let errors = check_err(
        r#"
        struct Address { city: str; };
        var address: Address = { city: "Awka"; };
        "#,
    );
    assert!(
        errors.contains("Address") && (errors.contains("constructor") || errors.contains("type mismatch")),
        "{errors}"
    );
}

#[test]
fn semantic_field_queries_resolve_named_and_generic_fields() {
    use crate::ast::SparType;
    use crate::semantics::SemanticSnapshot;

    let src = r#"
        struct Boxed<T> { value: T; };
        struct Address { city: str; };
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let prog = crate::parser::Parser::new(tokens).parse().expect("parse");
    let symbols = crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .expect("resolve");

    let semantic = SemanticSnapshot::new(symbols.clone());

    assert_eq!(
        semantic.field_type(&SparType::Named("Address".into()), "city"),
        Some(SparType::Str)
    );
    assert_eq!(
        semantic.field_type(
            &SparType::Applied {
                name: "Boxed".into(),
                arguments: vec![SparType::Int],
            },
            "value",
        ),
        Some(SparType::Int)
    );
    assert_eq!(
        semantic.field_type(
            &SparType::Applied {
                name: "MapEntry".into(),
                arguments: vec![SparType::Str, SparType::Bool],
            },
            "value",
        ),
        Some(SparType::Bool)
    );
}

#[test]
fn function_value_named_arguments_infer_by_name_not_source_order() {
    check_ok(
        r#"
        fn main() -> int {
            var choose: fn(first: int, second: str) -> int = |first: int, second: str| first;
            return choose(second: "ignored", first: 7);
        };
        "#,
    );
}

#[test]
fn semantic_fields_for_bound_struct_include_omitted_option_fields() {
    use crate::ast::SparType;
    use crate::semantics::SemanticSnapshot;

    let src = r#"struct User { name: str = "Mike"; sex: Option<str> = none(); };"#;
    let compilation = crate::Compiler::new(crate::CompileOptions { evaluate: false, ..Default::default() }).compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let symbols = compilation.symbols.unwrap();
    let semantic = SemanticSnapshot::new(symbols);

    let fields = semantic.fields_for_type(&SparType::Named("User".into()));
    assert!(fields.iter().any(|field| field.name == "name" && field.ty == SparType::Str));
    assert!(fields.iter().any(|field| {
        field.name == "sex"
            && field.ty
                == SparType::Applied {
                    name: "Option".into(),
                    arguments: vec![SparType::Str],
                }
    }));
    assert_eq!(
        semantic.field_type(&SparType::Named("User".into()), "sex"),
        Some(SparType::Applied {
            name: "Option".into(),
            arguments: vec![SparType::Str],
        })
    );
}
