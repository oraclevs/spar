fn resolve_ok(src: &str) -> crate::resolver::SymbolTable {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    crate::resolver::Resolver::new()
        .resolve(&prog, &[])
        .unwrap()
}

fn resolve_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    match crate::resolver::Resolver::new().resolve(&prog, &[]) {
        Ok(_) => panic!("expected error"),
        Err(e) => format!("{:?}", e),
    }
}

#[test]
fn generic_type_parameters_are_lexical_and_have_arity() {
    resolve_ok(
        "type [Box<T>] { value: T; }; function unbox<T>(value: Box<T>) -> T { return value.value; };",
    );

    let duplicate = resolve_err("function bad<T, T>(value: T) -> T { return value; };");
    assert!(
        duplicate.contains("duplicate type parameter 'T'"),
        "{duplicate}"
    );

    let bare = resolve_err("type [Box<T>] { value: T; }; var value: Box = { value: 1; };");
    assert!(bare.contains("expects 1 type argument"), "{bare}");

    let excess =
        resolve_err("type [Box<T>] { value: T; }; var value: Box<int, str> = { value: 1; };");
    assert!(excess.contains("expects 1 type argument"), "{excess}");
}

#[test]
fn generic_call_type_argument_arity_is_resolved() {
    resolve_ok(
        "function identity<T>(value: T) -> T { return value; }; var value: int = identity<int>(value: 1);",
    );

    let non_generic = resolve_err(
        "function value(input: int) -> int { return input; }; var x: int = value<int>(input: 1);",
    );
    assert!(
        non_generic.contains("does not accept type arguments"),
        "{non_generic}"
    );

    let excess = resolve_err(
        "function identity<T>(value: T) -> T { return value; }; var x: int = identity<int, str>(value: 1);",
    );
    assert!(excess.contains("at most 1 type argument"), "{excess}");

    let unknown = resolve_err(
        "function identity<T>(value: T) -> T { return value; }; var x: int = identity<Ghost>(value: 1);",
    );
    assert!(
        unknown.contains("undefined type") && unknown.contains("Ghost"),
        "{unknown}"
    );
}

#[test]
fn exec_shell_at_module_scope_is_rejected() {
    let errors = resolve_err(
        "type [ExecResult]{ success: bool; exitCode: int; }; var x: ExecResult = exec shell { true; };",
    );
    assert!(
        errors.to_lowercase().contains("exec") && errors.to_lowercase().contains("function"),
        "got: {errors}"
    );
}

#[test]
fn exec_shell_inside_a_function_body_is_allowed() {
    resolve_ok(
        "type [ExecResult]{ success: bool; exitCode: int; }; function f() -> int { var r: ExecResult = exec shell { true; }; return 0; };",
    );
}

#[test]
fn plain_shell_construction_at_module_scope_is_allowed() {
    resolve_ok("var x: shell = shell { true; };");
}

// ── SparType::Named existence validation ─────────────────────────────────────

#[test]
fn named_type_on_var_resolves_when_type_exists() {
    let src = "type [Leaf]{ name: str; };\nvar someExpr: str = \"a\";\nvar x: Leaf = someExpr;\n";
    resolve_ok(src); // panics (test fails) if the declared type doesn't resolve
}

#[test]
fn named_type_on_var_errors_when_type_missing() {
    let src = "var x: Ghost = 1;\n";
    let errs = resolve_err(src);
    assert!(
        errs.contains("undefined type") && errs.contains("Ghost"),
        "got: {errs}"
    );
}

#[test]
fn named_type_on_list_var_errors_when_type_missing() {
    let src = "var xs: [Ghost] = [];\n";
    let errs = resolve_err(src);
    assert!(
        errs.contains("undefined type") && errs.contains("Ghost"),
        "got: {errs}"
    );
}

#[test]
fn named_type_on_function_param_and_return_errors_when_type_missing() {
    let src = "function f(l: Ghost) -> Ghost { return l; };\n";
    let errs = resolve_err(src);
    assert!(
        errs.contains("undefined type") && errs.contains("Ghost"),
        "got: {errs}"
    );
}

#[test]
fn named_type_on_section_field_errors_when_type_missing() {
    let src = "[Tree]{ root: Ghost = 1; };\n";
    let errs = resolve_err(src);
    assert!(
        errs.contains("undefined type") && errs.contains("Ghost"),
        "got: {errs}"
    );
}

#[test]
fn resolver_registers_function() {
    let src = r#"function greet(name: str) -> str { return name; };"#;
    let sym = resolve_ok(src);
    assert!(sym.functions.contains_key("greet"));
    let f = &sym.functions["greet"];
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].0, "name");
}

#[test]
fn resolver_accepts_call_expression_statements_in_nested_blocks() {
    resolve_ok(
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
fn locals_declared_in_both_if_branches_do_not_leak() {
    let error = resolve_err(
        r#"
        function value(flag: bool) -> int {
            if flag { var inside: int = 1; } else { var inside: int = 2; }
            return inside;
        };
        "#,
    );
    assert!(error.contains("undefined reference: `inside`"), "{error}");
}

#[test]
fn local_declared_after_terminal_branch_stays_block_scoped() {
    let error = resolve_err(
        r#"
        function value(flag: bool) -> int {
            if flag { return 1; } else { var inside: int = 2; }
            return inside;
        };
        "#,
    );
    assert!(error.contains("undefined reference: `inside`"), "{error}");
}

#[test]
fn break_and_continue_outside_loops_are_rejected_with_spans() {
    for (src, keyword) in [("break;", "break"), ("continue;", "continue")] {
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let errors = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .unwrap_err();
        let error = errors
            .iter()
            .find(|error| error.to_string().contains(keyword))
            .unwrap();
        let crate::error::SparError::ResolveError { span, .. } = error else {
            panic!("expected resolve error");
        };
        assert_eq!((span.line, span.col), (1, 1));
    }
}

#[test]
fn break_and_continue_inside_nested_loop_blocks_resolve() {
    resolve_ok("for item in [1] { if true { continue; } if false { break; } }");
}

#[test]
fn void_function_with_no_return_at_all_resolves() {
    resolve_ok("function noop() -> void { };");
}

#[test]
fn non_void_function_still_requires_a_return_on_every_path() {
    let error = resolve_err("function f() -> int { };");
    assert!(error.contains("does not guarantee"), "got: {error}");
}

#[test]
fn assignment_requires_an_existing_mutable_binding() {
    let immutable = resolve_err("var count: int = 0; count = 1;");
    assert!(immutable.contains("immutable"), "got: {immutable}");
    assert!(immutable.contains("var mut count"), "got: {immutable}");

    let missing = resolve_err("missing = 1;");
    assert!(missing.contains("not declared"), "got: {missing}");

    resolve_ok(
        "function f() -> int { var mut count: int = 0; if true { count = 1; } return count; };",
    );
}

fn logging_host() -> crate::host::HostRegistry {
    let mut hosts = crate::host::HostRegistry::new();
    hosts
        .register(crate::host::HostFunction::new(
            "log",
            "write",
            vec![("message", crate::ast::SparType::Str)],
            crate::ast::SparType::Void,
            |_| Ok(crate::evaluator::ConfigValue::Int(0)),
        ))
        .unwrap();
    hosts
}

#[test]
fn host_call_with_wrong_param_name_is_a_resolve_error() {
    let src = "log::write(msg: \"hi\");";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let error = crate::resolver::Resolver::new()
        .with_hosts(logging_host())
        .resolve(&prog, &[])
        .unwrap_err();
    let message = format!("{error:?}");
    assert!(message.contains("has no param 'msg'"), "{message}");
}

#[test]
fn host_call_missing_a_required_argument_is_a_resolve_error() {
    let src = "log::write();";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let error = crate::resolver::Resolver::new()
        .with_hosts(logging_host())
        .resolve(&prog, &[])
        .unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains("missing required argument 'message'"),
        "{message}"
    );
}

#[test]
fn host_call_with_correct_args_resolves() {
    let src = "log::write(message: \"hi\");";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    crate::resolver::Resolver::new()
        .with_hosts(logging_host())
        .resolve(&prog, &[])
        .expect("a correctly-named host call should resolve");
}

#[test]
fn resolver_rejects_duplicate_function() {
    let src = r#"
        function f(x: str) -> str { return x; };
        function f(y: int) -> int { return y; };
    "#;
    let err = resolve_err(src);
    assert!(err.contains("already defined") || err.contains("duplicate"));
}

#[test]
fn resolver_call_unknown_function_error() {
    let src = r#"var x: str = unknown(name: "hi");"#;
    let err = resolve_err(src);
    assert!(err.contains("unknown") || err.contains("undefined"));
}

#[test]
fn resolver_call_wrong_arg_name_error() {
    let src = r#"
        function greet(name: str) -> str { return name; };
        var x: str = greet(wrong: "hi");
    "#;
    let err = resolve_err(src);
    assert!(err.contains("wrong") || err.contains("param"));
}

#[test]
fn resolver_var_in_only_one_branch_not_in_outer_scope() {
    // `a` declared only in then-branch, `b` only in else-branch;
    // after the if neither is in scope, so `return a` must produce an error
    let src = r#"
        function f(x: bool) -> str {
            if x { var a: str = "yes"; } else { var b: str = "no"; }
            return a;
        };
    "#;
    let err = resolve_err(src);
    assert!(!err.is_empty(), "expected a resolve error but got none");
}

#[test]
fn resolver_section_param_rejected() {
    let src = r#"function f(x: section) -> str { return "hi"; };"#;
    let err = resolve_err(src);
    assert!(err.contains("section") || err.contains("param"));
}

#[test]
fn resolver_closure_deps_captured() {
    let src = r#"
        var appName: str = "keel";
        function greet(prefix: str) -> str { return appName; };
    "#;
    let sym = resolve_ok(src);
    let f = &sym.functions["greet"];
    assert!(f
        .closure_deps
        .iter()
        .any(|d| matches!(d, crate::depgraph::DeclId::Global(n) if n == "appName")));
}

// ── Phase 11d tests ───────────────────────────────────────────────────────────

#[test]
fn private_function_registered_with_is_private_flag() {
    let src = r#"private function helper(x: int) -> int { return x; };"#;
    let sym = resolve_ok(src);
    assert!(sym.functions["helper"].is_private);
}

#[test]
fn public_function_registered_as_not_private() {
    let src = r#"function helper(x: int) -> int { return x; };"#;
    let sym = resolve_ok(src);
    assert!(!sym.functions["helper"].is_private);
}

#[test]
fn private_function_usable_within_same_file() {
    let src = r#"
        private function helper(x: int) -> int { return x; };
        var doubled: int = helper(x: 5);
    "#;
    assert!(resolve_ok(src).globals.contains_key("doubled"));
}

#[test]
fn for_loop_alone_does_not_satisfy_exhaustiveness() {
    let src = r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
        };
    "#;
    let err = resolve_err(src);
    assert!(err.contains("return") || err.contains("path") || err.contains("exhaustive"));
}

#[test]
fn for_loop_with_fallback_return_is_exhaustive() {
    let src = r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
            return 0;
        };
    "#;
    resolve_ok(src);
}

#[test]
fn for_loop_var_in_scope_inside_body() {
    let src = r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
            return 0;
        };
    "#;
    resolve_ok(src);
}

#[test]
fn for_loop_var_not_in_scope_after_loop() {
    let src = r#"
        function f(nums: [int]) -> int {
            for n in nums { var x: int = n; }
            return n;
        };
    "#;
    let err = resolve_err(src);
    assert!(!err.is_empty());
}

// ── Schema validation helpers ────────────────────────────────────────────────

fn schema_validate(
    schema_src: &str,
    config_src: &str,
) -> Result<
    std::collections::HashMap<String, Vec<crate::ast::SchemaField>>,
    Vec<crate::error::SparError>,
> {
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    let mut schema_file = NamedTempFile::new().unwrap();
    write!(schema_file, "{}", schema_src).unwrap();

    // Write config that imports the schema temp file
    let schema_path = schema_file.path().to_str().unwrap().to_string();
    // Replace the placeholder path in config_src
    let config_src_resolved = config_src.replace("SCHEMA_PATH", &schema_path);

    let tokens = crate::lexer::Lexer::new(&config_src_resolved)
        .tokenize()
        .unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let base = Path::new(".");
    crate::loader::validate_schema_imports(&prog, base)
}

#[test]
fn valid_config_against_occ_example_passes() {
    let schema_src = r#"schema MainRoute {
    routeOne: str;
    redirect: bool;
    main: [str];
    x: section = {
        host: str;
        port?: int;
        enabled?: bool;
    };
};
"#;
    let config_src = r#"import schema "SCHEMA_PATH";

[MainRoute]{
    routeOne: str = "/main";
    redirect: bool = false;
    main: [str] = ["main", "ask"];
    x: section = {
        host: str = "localhost";
    };
};
"#;
    let result = schema_validate(schema_src, config_src);
    assert!(result.is_ok(), "valid config must pass: {:?}", result.err());
}

#[test]
fn missing_required_field_is_schema_error() {
    let schema_src = "schema X { a: int; b: str; };\n";
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; };\n";
    let errs = schema_validate(schema_src, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("missing") || combined.contains("b"),
        "must mention missing field 'b': {}",
        combined
    );
}

#[test]
fn missing_optional_field_is_fine() {
    let schema_src = "schema X { a: int; b?: str; };\n";
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; };\n";
    let result = schema_validate(schema_src, config_src);
    assert!(
        result.is_ok(),
        "omitting optional field must be fine: {:?}",
        result.err()
    );
}

#[test]
fn extra_field_not_in_schema_is_error() {
    let schema_src = "schema X { a: int; };\n";
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; extra: str = \"x\"; };\n";
    let errs = schema_validate(schema_src, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("extra") || combined.contains("not declared"),
        "must mention extra field: {}",
        combined
    );
}

#[test]
fn wrong_type_on_present_field_is_schema_error() {
    let schema_src = "schema X { a: bool; };\n";
    // config declares `a` as `int` instead of `bool`
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; };\n";
    let errs = schema_validate(schema_src, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("type") || combined.contains("bool") || combined.contains("int"),
        "must mention type mismatch: {}",
        combined
    );
}

#[test]
fn schema_bound_section_does_not_require_explicit_field_types() {
    // Regression: a section with no `-> Type` binding of its own, but
    // matching a section declared by an `import schema`, must NOT be forced
    // to write `field: Type = value;` on every field — the schema already
    // tells the typechecker each field's expected shape.
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    let mut schema_file = NamedTempFile::new().unwrap();
    writeln!(
        schema_file,
        "schema Flutter {{ projectName: str; gitInit: bool; }};"
    )
    .unwrap();
    let schema_path = schema_file.path().to_str().unwrap().to_string();

    let config_src = format!(
        "import schema \"{schema_path}\";\n[Flutter]{{ projectName: \"oracle\"; gitInit: true; }};\n"
    );

    let tokens = crate::lexer::Lexer::new(&config_src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let schema_bindings = crate::loader::validate_schema_imports(&program, Path::new("."))
        .expect("schema validation must pass — fields match the schema");

    let symbols = crate::resolver::Resolver::new()
        .resolve(&program, &[])
        .unwrap();
    let result =
        crate::typechecker::TypeChecker::check_with_schema(&program, &symbols, schema_bindings);
    assert!(
        result.is_ok(),
        "schema-bound untyped fields must not error: {:?}",
        result.err()
    );
}

#[test]
fn schema_bound_section_still_checks_value_type_mismatch() {
    // The exemption above must not turn into "anything goes" — a value that
    // doesn't match the schema's declared type must still be caught.
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    let mut schema_file = NamedTempFile::new().unwrap();
    writeln!(schema_file, "schema Flutter {{ gitInit: bool; }};").unwrap();
    let schema_path = schema_file.path().to_str().unwrap().to_string();

    let config_src =
        format!("import schema \"{schema_path}\";\n[Flutter]{{ gitInit: \"yes\"; }};\n");

    let tokens = crate::lexer::Lexer::new(&config_src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();

    let schema_bindings = crate::loader::validate_schema_imports(&program, Path::new("."))
        .expect("schema validation must pass at the loader stage — value-type checking is the typechecker's job here");

    let symbols = crate::resolver::Resolver::new()
        .resolve(&program, &[])
        .unwrap();
    let errs =
        crate::typechecker::TypeChecker::check_with_schema(&program, &symbols, schema_bindings)
            .unwrap_err();
    assert!(
        !errs.is_empty(),
        "a str value for a schema-declared bool field must still error"
    );
}

#[test]
fn missing_required_section_is_schema_error() {
    let schema_src = "schema X { a: int; };\n";
    let config_src = "import schema \"SCHEMA_PATH\";\n[Y]{ z: int = 1; };\n"; // [Y] not [X]
    let errs = schema_validate(schema_src, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("X") || combined.contains("missing") || combined.contains("required"),
        "must mention missing section X: {}",
        combined
    );
}

#[test]
fn missing_optional_section_is_fine() {
    let _schema_src = "schema? X { a: int; };\n";
    // config has no [X] section at all
    let config_src = "import schema \"SCHEMA_PATH\";\n[Y]{ z: int = 1; };\n";
    // [inference] This will also fail on extra-section check since [Y] isn't in schema.
    // To isolate this test, schema must declare [Y] too.
    let schema_src2 = "schema? X { a: int; };\nschema Y { z: int; };\n";
    let result = schema_validate(schema_src2, config_src);
    assert!(
        result.is_ok(),
        "omitting optional section must be fine: {:?}",
        result.err()
    );
}

#[test]
fn config_section_with_no_schema_entry_is_ignored() {
    // a struct the schema never mentions is an ordinary struct
    let schema_src = "schema X { a: int; };\n";
    let config_src =
        "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; };\n[Unrelated]{ b: str = \"x\"; };\n";
    assert!(schema_validate(schema_src, config_src).is_ok());
}

#[test]
fn nested_section_field_validated_recursively() {
    let schema_src = r#"schema X {
    x: section = {
        host: str;
        port?: int;
    };
};
"#;
    // config's x section omits required `host`
    let config_src = r#"import schema "SCHEMA_PATH";
[X]{
    x: section = {
        port: int = 8080;
    };
};
"#;
    let errs = schema_validate(schema_src, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("host") || combined.contains("missing"),
        "must mention missing nested field 'host': {}",
        combined
    );
}

#[test]
fn importing_a_non_schema_file_as_schema_is_error() {
    let not_a_schema = "var x: int = 1;\n"; // no schema declarations
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ a: int = 1; };\n";
    let errs = schema_validate(not_a_schema, config_src).unwrap_err();
    let combined = format!("{:?}", errs);
    assert!(
        combined.contains("schema file"),
        "must explain that the imported file is not a schema file: {}",
        combined
    );
}

// ── Fix 1: two schema imports — Rule 2 must be per combined set ──────────────

/// A config that imports two separate schema files, each declaring one section,
/// must pass with no errors.  Before the fix, schema A's Rule 2 check would
/// reject the section declared by schema B (and vice-versa).
#[test]
fn two_schema_imports_each_owning_one_section_passes() {
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    // Schema A declares [A]
    let mut schema_a = NamedTempFile::new().unwrap();
    writeln!(schema_a, "schema A {{ x: int; }};").unwrap();

    // Schema B declares [B]
    let mut schema_b = NamedTempFile::new().unwrap();
    writeln!(schema_b, "schema B {{ y: str; }};").unwrap();

    let path_a = schema_a.path().to_str().unwrap().to_string();
    let path_b = schema_b.path().to_str().unwrap().to_string();

    // Config imports both schemas and has both sections
    let config_src = format!(
        "import schema \"{path_a}\";\nimport schema \"{path_b}\";\n\
         [A]{{ x: int = 1; }};\n[B]{{ y: str = \"hello\"; }};\n"
    );

    let tokens = crate::lexer::Lexer::new(&config_src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    let result = crate::loader::validate_schema_imports(&prog, Path::new("."));
    assert!(
        result.is_ok(),
        "two schema imports, each owning one section, must pass: {:?}",
        result.err()
    );
}

// ── Fix 2: spread items — no false "missing field" errors ────────────────────

/// A config section that contains a spread (`...SomeName;`) must not produce
/// a "missing required field" error, because the spread may supply the field
/// at runtime.  Before the fix, the validator would report every required field
/// not literally present in the section's own `Field` items as missing.
#[test]
fn section_with_spread_skips_field_validation() {
    // Schema requires both `a` and `b`
    let schema_src = "schema X { a: int; b: str; };\n";
    // Config only has `a` explicitly; `b` is expected to come from the spread
    let config_src = "import schema \"SCHEMA_PATH\";\n[X]{ ...Defaults; a: int = 1; };\n";
    let result = schema_validate(schema_src, config_src);
    assert!(
        result.is_ok(),
        "section with a spread must not produce false missing-field errors: {:?}",
        result.err()
    );
}

#[test]
fn resolver_accepts_self_reference_inside_section() {
    let src = r#"
        [Postgres]{
            environment: section = {
                postgresDb: str = "my_app";
                postgresUser: str = self.environment.postgresDb;
            };
        };
    "#;
    let _ = resolve_ok(src); // must not error
}

#[test]
fn resolver_rejects_self_reference_outside_section() {
    let src = r#"var x: str = self::y;"#;
    let err = resolve_err(src);
    assert!(
        err.contains("self"),
        "expected an error mentioning `self`, got: {err}"
    );
}

#[test]
fn resolver_registers_type_with_named_field_reference() {
    let src = r#"
        type [Border]{
            width?: int;
        };
        type [Decoration]{
            border?: Border;
        };
    "#;
    let sym = resolve_ok(src);
    assert!(sym.types.contains_key("Border"));
    assert!(sym.types.contains_key("Decoration"));
}

#[test]
fn resolver_rejects_duplicate_type() {
    let src = r#"
        type [Border]{ width?: int; };
        type [Border]{ width?: int; };
    "#;
    let err = resolve_err(src);
    assert!(err.contains("already defined"), "got: {err}");
}

#[test]
fn resolver_rejects_type_named_schema() {
    let src = r#"type [Schema]{ a: int; };"#;
    let err = resolve_err(src);
    assert!(
        err.contains("reserved") || err.contains("Schema"),
        "got: {err}"
    );
}

#[test]
fn resolver_rejects_non_pascal_case_type_name() {
    let src = r#"type [border]{ width?: int; };"#;
    let err = resolve_err(src);
    assert!(err.contains("PascalCase"), "got: {err}");
}

#[test]
fn resolver_rejects_undefined_named_type_reference() {
    let src = r#"
        type [Decoration]{
            border?: NoSuchType;
        };
    "#;
    let err = resolve_err(src);
    assert!(
        err.contains("undefined type") || err.contains("NoSuchType"),
        "got: {err}"
    );
}

#[test]
fn resolver_rejects_unknown_type_binding() {
    let src = r#"
        [Postgres] -> NoSuchType {
            image: str = "postgres:16";
        };
    "#;
    let err = resolve_err(src);
    assert!(
        err.contains("undefined type") || err.contains("NoSuchType"),
        "got: {err}"
    );
}

#[test]
fn resolver_accepts_known_type_binding() {
    let src = r#"
        type [PostgresType]{
            image: str;
        };
        [Postgres] -> PostgresType {
            image: str = "postgres:16";
        };
    "#;
    let _ = resolve_ok(src);
}

#[test]
fn imported_type_selectively_can_bind_a_section() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("types.spar"),
        "export type [PostgresType]{ image: str; };\n",
    )
    .unwrap();
    let src = concat!(
        "import type { PostgresType } from \"types.spar\";\n",
        "[Postgres] -> PostgresType {\n",
        "    image: \"postgres:16\";\n",
        "};\n",
    );
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let mut program = crate::parser::Parser::new(tokens).parse().unwrap();
    let mut loader = crate::loader::ImportLoader::new(dir.path());
    crate::loader::expand_imports(&mut program, &mut loader).expect("expand must succeed");
    let symbols = crate::resolver::Resolver::resolve_with_imports(
        &program,
        &std::collections::HashMap::new(),
    );
    assert!(symbols.is_ok(), "got: {:?}", symbols.err());
}

// ── Expr::Object resolution ───────────────────────────────────────────────────

#[test]
fn object_literal_resolves_inner_namespace_ref() {
    // resolve_ok's own `.unwrap()` panics with the error detail if this
    // fails to resolve — that panic-on-Err IS the test's failure mode.
    let src =
        "var host: str = \"h\";\ntype [Leaf]{ name: str; };\nvar x: Leaf = { name: host; };\n";
    resolve_ok(src);
}

#[test]
fn object_literal_errors_on_undefined_inner_reference() {
    let src = "type [Leaf]{ name: str; };\nvar x: Leaf = { name: ghost; };\n";
    let errs = resolve_err(src);
    assert!(errs.contains("ghost"), "got: {errs}");
}

// ── enum declarations ─────────────────────────────────────────────────────────

#[test]
fn enum_registers_ok() {
    resolve_ok("enum Devices { Ios, Android };");
}

#[test]
fn enum_duplicate_name_errors() {
    let errs = resolve_err("enum Devices { Ios };\nenum Devices { Android };\n");
    assert!(errs.contains("already defined"), "got: {errs}");
}

#[test]
fn enum_non_pascal_case_name_errors() {
    let errs = resolve_err("enum devices { Ios };");
    assert!(errs.contains("PascalCase"), "got: {errs}");
}

#[test]
fn enum_and_type_name_collision_errors() {
    let errs = resolve_err("type [Devices]{ x: str; };\nenum Devices { Ios };\n");
    assert!(errs.contains("already declared as a type"), "got: {errs}");
}

#[test]
fn type_and_enum_name_collision_errors_reverse_order() {
    let errs = resolve_err("enum Devices { Ios };\ntype [Devices]{ x: str; };\n");
    assert!(errs.contains("already declared as an enum"), "got: {errs}");
}

#[test]
fn enum_variant_ref_resolves() {
    resolve_ok("enum Devices { Ios, Android };\nvar x: Devices = Devices::Android;\n");
}

#[test]
fn enum_undeclared_variant_errors() {
    let errs = resolve_err("enum Devices { Ios, Android };\nvar x: Devices = Devices::Ghost;\n");
    assert!(
        errs.contains("Ghost") && errs.contains("Devices"),
        "got: {errs}"
    );
}

#[test]
fn enum_variant_ref_resolves_inside_function_body() {
    resolve_ok(concat!(
        "enum Devices { Ios, Android };\n",
        "function f() -> Devices {\n",
        "    return Devices::Android;\n",
        "};\n",
    ));
}

// ── Named::field access on vars/locals (not just sections) ──────────────────

#[test]
fn named_field_access_on_global_var_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        var person: Human = { name: "Mike"; age: 5; };
        var pname: str = person.name;
    "#;
    resolve_ok(src);
}

#[test]
fn named_field_access_on_function_local_var_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        function greet(h: Human) -> str {
            var local: Human = h;
            return local.name;
        };
    "#;
    resolve_ok(src);
}

#[test]
fn named_field_access_on_loop_var_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        };
    "#;
    resolve_ok(src);
}

// ── functionGroup registration ───────────────────────────────────────────────

#[test]
fn function_group_registers_its_functions() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
            private function semantic(hor: float, vet: float) -> int { return 2; }
        };
    "#;
    let table = resolve_ok(src);
    let group = table
        .function_groups
        .get("EdgeInsect")
        .expect("group must be registered");
    assert!(!group.is_private);
    assert!(group.functions.contains_key("only"));
    assert!(group.functions.contains_key("semantic"));
    assert!(group.functions["semantic"].is_private);
}

#[test]
fn function_group_duplicate_name_errors() {
    let src = r#"
        functionGroup EdgeInsect { function only() -> int { return 1; } };
        functionGroup EdgeInsect { function other() -> int { return 2; } };
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("already defined") && errs.contains("EdgeInsect"),
        "got: {errs}"
    );
}

#[test]
fn function_group_duplicate_inner_function_errors() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
            function only() -> int { return 2; }
        };
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("already defined") && errs.contains("only"),
        "got: {errs}"
    );
}

#[test]
fn function_group_name_must_be_pascal_case() {
    let src = r#"functionGroup edgeInsect { function only() -> int { return 1; } };"#;
    let errs = resolve_err(src);
    assert!(errs.contains("PascalCase"), "got: {errs}");
}

#[test]
fn function_group_missing_return_on_all_paths_errors() {
    let src = r#"
        functionGroup EdgeInsect {
            function bad() -> int { }
        };
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("does not guarantee a value is returned"),
        "got: {errs}"
    );
}

// ── functionGroup call resolution ────────────────────────────────────────────

#[test]
fn function_group_call_resolves() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
        var x: int = EdgeInsect::only();
    "#;
    resolve_ok(src);
}

#[test]
fn function_group_call_undefined_function_errors() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
        var x: int = EdgeInsect::missing();
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("missing") && errs.contains("EdgeInsect"),
        "got: {errs}"
    );
}

#[test]
fn function_group_call_from_function_body_resolves() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
        function useIt() -> int {
            return EdgeInsect::only();
        };
    "#;
    resolve_ok(src);
}

#[test]
fn function_group_name_colliding_with_import_alias_errors() {
    let src = r#"
        import "does_not_matter.spar" as EdgeInsect;
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("EdgeInsect") && errs.contains("import alias"),
        "got: {errs}"
    );
}

// ── enum-typed field inside a `type [X]{...}` declaration ───────────────────

#[test]
fn type_field_referencing_enum_resolves() {
    let src = r#"
        enum Protocol { Http, Https };
        type [Port]{ protocol?: Protocol; };
    "#;
    resolve_ok(src);
}

// ── Dot field access ──────────────────────────────────────────────────────

#[test]
fn dot_field_access_on_section_resolves() {
    let src = r#"
        [Database]{ pool: int = 5; };
        var p: int = Database.pool;
    "#;
    resolve_ok(src);
}

#[test]
fn dot_field_access_on_global_var_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        var person: Human = { name: "Mike"; age: 5; };
        var pname: str = person.name;
    "#;
    resolve_ok(src);
}

#[test]
fn dot_field_access_on_loop_var_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        };
    "#;
    resolve_ok(src);
}

#[test]
fn dot_field_access_after_index_resolves() {
    let src = r#"
        type [Human]{ name: str; age: int; };
        var people: [Human] = [{ name: "jude"; age: 5; }];
        var pname: str = people[0].name;
    "#;
    resolve_ok(src);
}

#[test]
fn self_dot_field_access_resolves() {
    let src = r#"
        [Server]{
            port: int = 8080;
            display: str = "port-${self.port}";
        };
    "#;
    resolve_ok(src);
}

#[test]
fn bare_self_without_field_errors() {
    let src = r#"[Server]{ x: int = self; };"#;
    let errs = resolve_err(src);
    assert!(errs.contains("self"), "got: {errs}");
}

#[test]
fn global_dot_field_access_resolves() {
    let src = r#"
        var port: int = 3000;
        function f() -> int {
            var port: int = 1;
            return global.port;
        };
    "#;
    resolve_ok(src);
}

// ── Old '::' field-access syntax — must fail with a migration hint ────────

#[test]
fn old_style_section_double_colon_field_access_errors_with_migration_hint() {
    let src = r#"
        [Database]{ pool: int = 5; };
        var p: int = Database::pool;
    "#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("'.'") || errs.to_lowercase().contains("no longer supported"),
        "got: {errs}"
    );
}

#[test]
fn old_style_self_double_colon_errors_with_migration_hint() {
    let src = r#"[Server]{ port: int = 8080; display: str = self::port; };"#;
    let errs = resolve_err(src);
    assert!(
        errs.to_lowercase().contains("no longer supported") || errs.contains("'.'"),
        "got: {errs}"
    );
}

#[test]
fn unknown_double_colon_namespace_still_gets_generic_error() {
    let src = r#"var x: str = totallyUnknownThing::field;"#;
    let errs = resolve_err(src);
    assert!(
        errs.contains("undefined namespace") || errs.contains("undefined function"),
        "got: {errs}"
    );
}

#[test]
fn undefined_interpolation_in_a_native_task_run_block_is_a_resolve_error() {
    let in_task = resolve_err("task Build { run { echo ${nope}; }; };");
    assert!(in_task.contains("nope"), "{in_task}");
}

#[test]
fn defined_interpolations_and_task_params_in_shell_commands_resolve() {
    resolve_ok("var name: str = \"a\"; task T { run { echo ${name}; }; };");
    resolve_ok("task Greet(who: str) { run { echo ${who} | cat > out.txt; }; };");
}

#[test]
fn builtin_stream_type_requires_exactly_one_type_argument() {
    resolve_ok("function consume(values: Stream<int>) -> int { return 0; };");

    let bare = resolve_err("function consume(values: Stream) -> int { return 0; };");
    assert!(bare.contains("expects 1 type argument"), "{bare}");

    let excess = resolve_err("function consume(values: Stream<int, str>) -> int { return 0; };");
    assert!(excess.contains("expects 1 type argument"), "{excess}");
}
