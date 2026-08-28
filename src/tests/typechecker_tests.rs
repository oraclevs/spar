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
    check_ok(
        r#"
        function f(nums: [int]) -> int {
            for n in nums { return n; }
            return 0;
        }
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
        }
    "#,
    );
}

#[test]
fn bool_type_in_return_section_typechecks() {
    check_ok(
        r#"
        function f(major: int) -> section {
            if major <= 0 {
                return { error: bool = true; message: str = "bad"; };
            }
            return { error: bool = false; };
        }
    "#,
    );
}

#[test]
fn typecheck_valid_type_binding_with_inferred_field_types_passes() {
    let src = r#"
        type [PostgresType]{
            image: str;
            restart?: str;
        }
        [Postgres] -> PostgresType {
            image: "postgres:16";
        };
    "#;
    check_ok(src);
}

#[test]
fn typecheck_valid_type_binding_with_explicit_redundant_type_passes() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
            image: str = "postgres:16";
        };
    "#;
    check_ok(src);
}

#[test]
fn typecheck_type_binding_missing_required_field() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("missing required field"), "got: {err}");
}

#[test]
fn typecheck_type_binding_rejects_extra_field() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
            image: "postgres:16";
            extra: "not allowed";
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("is not declared in type"), "got: {err}");
}

#[test]
fn typecheck_type_binding_rejects_wrong_inferred_type() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
            image: 16;
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("expects `str`"), "got: {err}");
}

#[test]
fn typecheck_type_binding_rejects_wrong_explicit_type() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
            image: int = 16;
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("expects `str`"), "got: {err}");
}

#[test]
fn typecheck_type_binding_validates_named_nested_type_with_inferred_fields() {
    let src = r#"
        type [Border]{
            width: int;
        }
        type [Decoration]{
            border: Border;
        }
        [Style] -> Decoration {
            border: {
                width: 4;
            };
        };
    "#;
    check_ok(src);
}

#[test]
fn typecheck_type_binding_rejects_bad_named_nested_field() {
    let src = r#"
        type [Border]{
            width: int;
        }
        type [Decoration]{
            border: Border;
        }
        [Style] -> Decoration {
            border: {
                width: "not an int";
            };
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("expects `int`"), "got: {err}");
}

#[test]
fn typecheck_untyped_field_without_binding_is_error() {
    let src = r#"
        [Man]{
            name: "Mike";
        };
    "#;
    let err = check_err(src);
    assert!(err.contains("has no type"), "got: {err}");
}

#[test]
fn typecheck_unbound_section_still_requires_explicit_types() {
    // Regression: sections with no binding are completely unaffected.
    check_ok(r#"[Man]{ name: str = "Mike"; };"#);
}

// ── Spread in nested field bodies ────────────────────────────────────────

#[test]
fn spread_in_nested_field_matching_bound_source_passes() {
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
    check_ok(src);
}

#[test]
fn spread_in_nested_field_missing_required_field_errors() {
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        type [PartialEnvType]{ nodeEnv: str; }
        [Partial] -> PartialEnvType {
            nodeEnv: "production";
        };
        [Api] -> ServiceType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        type [BadEnvType]{ nodeEnv: str; port: int; }
        [Bad] -> BadEnvType {
            nodeEnv: "production";
            port: 3000;
        };
        [Api] -> ServiceType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        type [ExtraEnvType]{ nodeEnv: str; port: str; extra: str; }
        [WithExtra] -> ExtraEnvType {
            nodeEnv: "production";
            port: "3000";
            extra: "surprise";
        };
        [Api] -> ServiceType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        [ProductionEnvironment]{
            nodeEnv: str = "production";
            port: str = "3000";
        };
        [Api] -> ServiceType {
            image: "my-api";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    check_ok(src);
}

#[test]
fn spread_in_nested_field_unbound_source_wrong_shape_errors() {
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        [ProductionEnvironment]{
            nodeEnv: str = "production";
        };
        [Api] -> ServiceType {
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
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        [ProductionEnvironment] -> EnvironmentType {
            nodeEnv: "production";
            port: "3000";
        };
        [Backup] -> EnvironmentType {
            ...ProductionEnvironment;
        };
    "#;
    check_ok(src);
}

#[test]
fn spread_only_top_level_bound_section_wrong_shape_errors() {
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [PartialEnvType]{ nodeEnv: str; }
        [Partial] -> PartialEnvType {
            nodeEnv: "production";
        };
        [Backup] -> EnvironmentType {
            ...Partial;
        };
    "#;
    let err = check_err(src);
    assert!(
        err.contains("missing required field") && err.contains("port"),
        "got: {err}"
    );
}

#[test]
fn spread_mixed_with_explicit_fields_full_coverage_passes() {
    // A resolvable spread's contribution is now merged with the explicit
    // fields for coverage purposes: Partial covers nodeEnv, the explicit
    // field covers port — together they satisfy EnvironmentType.
    let src = r#"
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        type [PartialEnvType]{ nodeEnv: str; }
        [Partial] -> PartialEnvType {
            nodeEnv: "production";
        };
        [Api] -> ServiceType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        type [PartialEnvType]{ nodeEnv: str; }
        [Partial] -> PartialEnvType {
            nodeEnv: "production";
        };
        [Api] -> ServiceType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; databaseUrl: str; redisUrl: str; }
        type [VolumeType]{ postgresData: [str]; }
        type [PostgresType]{ volumes: VolumeType; }
        [ProductionEnvironment] -> EnvironmentType {
            nodeEnv: "production";
            port: "3000";
            databaseUrl: "None";
            redisUrl: "None";
        };
        [Postgres] -> PostgresType {
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
        type [EnvironmentType]{ nodeEnv: str; port: str; }
        type [ServiceType]{ image: str; environment: EnvironmentType; }
        function makeEnv() -> section {
            return { nodeEnv: str = "production"; };
        }
        [Api] -> ServiceType {
            image: "my-api";
            environment: { ...makeEnv(); };
        };
    "#;
    check_ok(src);
}

// ── Expr::Object shape validation against SparType::Named ────────────────────

#[test]
fn object_literal_matching_named_type_passes() {
    check_ok("type [Leaf]{ name: str; size: int; }\nvar x: Leaf = { name: \"a\"; size: 1; };\n");
}

#[test]
fn object_literal_missing_required_field_errors() {
    let err = check_err("type [Leaf]{ name: str; size: int; }\nvar x: Leaf = { name: \"a\"; };\n");
    assert!(err.contains("missing required field"), "got: {err}");
}

#[test]
fn object_literal_extra_field_errors() {
    let err = check_err("type [Leaf]{ name: str; }\nvar x: Leaf = { name: \"a\"; extra: 1; };\n");
    assert!(err.contains("not declared in type"), "got: {err}");
}

#[test]
fn object_literal_wrong_field_type_errors() {
    let err = check_err("type [Leaf]{ name: str; }\nvar x: Leaf = { name: 1; };\n");
    assert!(err.contains("expects"), "got: {err}");
}

#[test]
fn object_literal_against_primitive_type_errors() {
    let err = check_err("var x: int = { a: 1; };");
    assert!(err.contains("object literal"), "got: {err}");
}

#[test]
fn list_of_named_type_object_literals_passes() {
    check_ok("type [Leaf]{ name: str; }\nvar xs: [Leaf] = [{ name: \"a\"; }, { name: \"b\"; }];\n");
}

#[test]
fn list_of_named_type_bad_element_errors() {
    let err = check_err(
        "type [Leaf]{ name: str; }\nvar xs: [Leaf] = [{ name: \"a\"; }, { wrong: 1; }];\n",
    );
    assert!(err.contains("not declared in type"), "got: {err}");
}

#[test]
fn nested_object_literal_inside_object_literal_validates_recursively() {
    check_ok(concat!(
        "type [Branch]{ label: str; }\n",
        "type [Leaf]{ name: str; sub: Branch; }\n",
        "var x: Leaf = { name: \"a\"; sub: { label: \"b\"; }; };\n",
    ));
}

// ── Function-body-local shape validation ──────────────────────────────────────

#[test]
fn local_var_object_literal_matching_named_type_passes() {
    // Confirmed via resolver.rs::check_ns_ref_with_locals: this language has
    // no field-access-on-a-local-variable syntax (`local.field`/`local::field`
    // only resolves for top-level SECTIONS, not locals) — so this test only
    // exercises that the local var's own declaration typechecks, not that
    // its fields are later readable.
    check_ok(concat!(
        "type [Leaf]{ name: str; }\n",
        "function f() -> str {\n",
        "    var l: Leaf = { name: \"a\"; };\n",
        "    return \"ok\";\n",
        "}\n",
    ));
}

#[test]
fn function_return_bare_object_literal_matching_named_type_passes() {
    check_ok(concat!(
        "type [Leaf]{ name: str; size: int; }\n",
        "function makeLeaf(n: str) -> Leaf {\n",
        "    return { name: n; size: 0; };\n",
        "}\n",
    ));
}

#[test]
fn function_return_bare_object_literal_missing_field_errors() {
    let err = check_err(concat!(
        "type [Leaf]{ name: str; size: int; }\n",
        "function makeLeaf(n: str) -> Leaf {\n",
        "    return { name: n; };\n",
        "}\n",
    ));
    assert!(err.contains("missing required field"), "got: {err}");
}

#[test]
fn function_return_list_of_named_type_passes() {
    check_ok(concat!(
        "type [Leaf]{ name: str; }\n",
        "function makeLeaves() -> [Leaf] {\n",
        "    return [{ name: \"a\"; }, { name: \"b\"; }];\n",
        "}\n",
    ));
}

// Regression guard: the EXISTING `-> section { return { field: type = value; }; }`
// form must keep working exactly as before.
#[test]
fn section_return_block_with_explicit_types_still_works_regression() {
    check_ok(concat!(
        "function borderConf() -> section {\n",
        "    return { sides: [int] = [2, 4]; width: int = 5; };\n",
        "}\n",
    ));
}

// ── Function-call argument checking for Named types ───────────────────────────

#[test]
fn function_call_with_named_type_object_literal_arg_passes() {
    // As in the local-var/return tests above: no field-access-on-param
    // syntax exists in this language, so `useLeaf`'s body doesn't read
    // `l`'s fields — the point is that the CALL SITE's object-literal
    // argument typechecks.
    check_ok(concat!(
        "type [Leaf]{ name: str; }\n",
        "function useLeaf(l: Leaf) -> str { return \"ok\"; }\n",
        "var r: str = useLeaf(l: { name: \"a\"; });\n",
    ));
}

#[test]
fn function_call_with_named_type_object_literal_arg_missing_field_errors() {
    let err = check_err(concat!(
        "type [Leaf]{ name: str; size: int; }\n",
        "function useLeaf(l: Leaf) -> int { return 1; }\n",
        "var r: int = useLeaf(l: { name: \"a\"; });\n",
    ));
    assert!(err.contains("expects"), "got: {err}");
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
fn named_field_access_on_global_var_has_correct_type() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        var person: Human = { name: "Mike"; age: 5; };
        var pname: str = person.name;
    "#;
    check_ok(src);
}

#[test]
fn named_field_access_on_global_var_type_mismatch_errors() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        var person: Human = { name: "Mike"; age: 5; };
        var pname: int = person.name;
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn named_field_access_on_loop_var_has_correct_type() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        }
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
        }
    "#;
    check_ok(src);
}

#[test]
fn local_function_group_call_return_type_mismatch_errors() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        }
        var x: str = EdgeInsect::only();
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn dot_field_access_on_global_var_has_correct_type() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        var person: Human = { name: "Mike"; age: 5; };
        var pname: str = person.name;
    "#;
    check_ok(src);
}

#[test]
fn dot_field_access_type_mismatch_errors() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        var person: Human = { name: "Mike"; age: 5; };
        var pname: int = person.name;
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}

#[test]
fn dot_field_access_on_loop_var_has_correct_type() {
    let src = r#"
        type [Human]{ name: str; age: int; }
        function looper(people: [Human]) -> int {
            for person in people {
                if person.name == "jude" { return 6; }
                return 0;
            }
            return 0;
        }
    "#;
    check_ok(src);
}

#[test]
fn self_dot_field_access_has_correct_type() {
    // New coverage — self.field was never type-checked at all before
    // this change (confirmed: no "self" handling existed in typechecker.rs).
    let src = r#"
        [Server]{
            port: int = 8080;
            doubled: int = self.port + self.port;
        };
    "#;
    check_ok(src);
}

#[test]
fn self_dot_field_access_type_mismatch_errors() {
    let src = r#"
        [Server]{
            port: int = 8080;
            bad: str = self.port;
        };
    "#;
    let errs = check_err(src);
    assert!(errs.contains("type mismatch"), "got: {errs}");
}
