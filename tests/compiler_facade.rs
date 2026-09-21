use std::fs;

use spar::{CompileOptions, Compiler, Engine};

#[test]
fn record_is_a_builtin_dynamic_object_type() {
    let source = r#"
        var user: Record = { name: "Obi"; age: 24; active: true; };
        function main() -> int { return 0; };
    "#;

    Engine::default()
        .check_source(source)
        .expect("Record should accept ordinary object literals without a declared shape");
}

#[test]
fn record_type_does_not_disable_declared_struct_shape_checks() {
    let source = r#"
        type User { name: str; age: int; };
        var user: User = { name: "Obi"; };
        function main() -> int { return 0; };
    "#;

    let errors = Engine::default().check_source(source).unwrap_err();
    assert!(
        errors.iter().any(|error| error.to_string().contains("age")),
        "{errors:?}"
    );
}

#[test]
fn compiles_multi_file_project_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("shared.spar"),
        "export var port: int = 8080;\n",
    )
    .unwrap();
    let source = "import \"shared.spar\" as shared;\nexport var port: int = shared.port;\n";
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(source);

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["port"],
        spar::ConfigValue::Int(8080)
    );
}

#[test]
fn evaluates_transitive_cross_file_function_call() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "function value() -> int { return 42; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("derived.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "export var answer: int = values::value();\n",
        ),
    )
    .unwrap();
    let source = concat!(
        "import \"derived.spar\" as derived;\n",
        "export var result: int = derived::answer;\n",
    );
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(source);

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["result"],
        spar::ConfigValue::Int(42)
    );
}

#[test]
fn preserves_diagnostics_from_imported_evaluation() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("broken.spar"),
        "export var answer: int = 1 / 0;\n",
    )
    .unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(concat!(
        "import \"broken.spar\" as broken;\n",
        "export var result: int = broken::answer;\n",
    ));

    assert!(compilation.result.is_none());
    assert!(
        compilation
            .errors
            .iter()
            .any(|error| error.to_string().contains("division by zero")),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn rejects_transitive_aliased_import_cycle() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("a.spar"),
        concat!("import \"b.spar\" as b;\n", "export var a: int = b::b;\n",),
    )
    .unwrap();
    fs::write(
        temp.path().join("b.spar"),
        concat!("import \"a.spar\" as a;\n", "export var b: int = a::a;\n",),
    )
    .unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(concat!(
        "import \"a.spar\" as a;\n",
        "export var result: int = a::a;\n",
    ));

    assert!(compilation.result.is_none());
    assert!(
        compilation
            .errors
            .iter()
            .any(|error| error.to_string().contains("import cycle detected")),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn resolves_field_access_on_a_cross_file_plain_var() {
    // Regression: `alias::var::field` (3-segment field access into an
    // imported plain `var`, as opposed to `alias::EnumName::Variant`)
    // used to fall through to a bogus "cyclic reference" error — the
    // evaluator's 3+-segment namespace-ref arm assumed every extra
    // segment past the import alias was itself another namespace lookup,
    // when here it's ordinary field access on the imported var's value.
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("network.spar"),
        concat!(
            "export type [Network]{ name: str; driver: str; };\n",
            "export var appNetwork: Network = { name: \"app-net\"; driver: \"bridge\"; };\n",
        ),
    )
    .unwrap();
    let source = concat!(
        "import \"network.spar\" as net;\n",
        "export var netName: str = net::appNetwork::name;\n",
    );
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(source);

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["netName"],
        spar::ConfigValue::Str("app-net".to_string())
    );
}

#[test]
fn retains_program_for_rendered_diagnostics() {
    let source = "var port: int = \"wrong\";\nvar enabled: bool = 42;\n";
    let compilation = Compiler::default().compile(source);
    assert!(compilation.program.is_some());
    assert!(compilation.symbols.is_some());
    assert_eq!(compilation.errors.len(), 2);

    let rendered = spar::ErrorRenderer::new(source, "invalid.spar").render_all(&compilation.errors);
    assert!(rendered.contains("invalid.spar"));
    assert!(rendered.matches("error[type]").count() >= 2, "{rendered}");
}

#[test]
fn selective_import_brings_helpers_from_the_modules_own_imports() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("colors.spar"),
        "function red() -> str { return \"#f00\"; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("toolkit.spar"),
        concat!(
            "import { red } from \"colors.spar\";\n",
            "function shade() -> str { return red(); };\n",
            "function color() -> str { return shade(); };\n",
        ),
    )
    .unwrap();
    let source = concat!(
        "import { color } from \"toolkit.spar\";\n",
        "export var picked: str = color();\n",
    );
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(source);

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["picked"],
        spar::ConfigValue::Str("#f00".to_string())
    );
}

#[test]
fn shell_interpolation_spans_point_at_the_real_source() {
    let source =
        "function demo(input: str) -> shell { return shell { echo -n \"${input}\"; }; };\n";
    let tokens = spar::Lexer::new(source).tokenize().unwrap();
    let program = spar::Parser::new(tokens).parse().unwrap();
    let spar::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected a function");
    };
    let mut spans = Vec::new();
    fn walk_shell(shell: &spar::ast::ShellExpr, spans: &mut Vec<(usize, usize)>) {
        for (_, step) in &shell.steps {
            let spar::ast::ShellStep::Command(command) = step else {
                continue;
            };
            for word in std::iter::once(&command.program).chain(command.args.iter()) {
                for part in &word.parts {
                    if let spar::ast::ShellWordPart::Expr(spar::ast::Expr::NamespaceRef(
                        reference,
                    )) = part
                    {
                        spans.push((reference.span.start, reference.span.end));
                    }
                }
            }
        }
    }
    for statement in &function.body.stmts {
        if let spar::ast::Statement::Return(
            spar::ast::ReturnValue::Expr(spar::ast::Expr::Shell(shell)),
            _,
        ) = statement
        {
            walk_shell(shell, &mut spans);
        }
    }
    let start = source.find("input}").unwrap();
    assert_eq!(spans, vec![(start, start + "input".len())]);
}

#[test]
fn first_class_callable_types_accept_named_functions_and_infer_closure_parameters() {
    let source = r#"
        function inc(value: int) -> int { return value + 1; };
        function main() -> int {
            var named: fn(int) -> int = inc;
            var closure: fn(int) -> int = fn(value) => value + 1;
            return 0;
        };
    "#;
    spar::Engine::default()
        .check_source(source)
        .unwrap_or_else(|errors| panic!("{errors:?}"));
}

#[test]
fn callable_assignment_rejects_incompatible_named_function_signature() {
    let source = r#"
        function inc(value: int) -> int { return value + 1; };
        function main() -> int {
            var bad: fn(str) -> int = inc;
            return 0;
        };
    "#;
    let errors = spar::Engine::default().check_source(source).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("fn(str) -> int")
                || error.to_string().contains("callable")),
        "{errors:?}"
    );
}

#[test]
fn closure_without_expected_or_explicit_parameter_type_is_rejected() {
    let source = r#"
        function main() -> int {
            var unknown = fn(value) => value;
            return 0;
        };
    "#;
    let errors = spar::Engine::default().check_source(source).unwrap_err();
    assert!(
        errors.iter().any(|error| error
            .to_string()
            .contains("cannot infer closure parameter 'value'")),
        "{errors:?}"
    );
}

#[test]
fn struct_constructor_rejects_unknown_and_duplicate_overrides() {
    let unknown = Engine::default()
        .check_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; };
            function main() -> int {
                var user = User(email: "x@example.com");
                return user.age;
            };
            "#,
        )
        .expect_err("unknown constructor override must fail");
    assert!(
        unknown
            .iter()
            .any(|error| error.to_string().contains("email")),
        "{unknown:?}"
    );

    let duplicate = Engine::default()
        .check_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; };
            function main() -> int {
                var user = User(name: "A", name: "B");
                return user.age;
            };
            "#,
        )
        .expect_err("duplicate constructor override must fail");
    assert!(
        duplicate
            .iter()
            .any(|error| error.to_string().contains("duplicate")
                && error.to_string().contains("name")),
        "{duplicate:?}"
    );
}

#[test]
fn immutable_struct_binding_rejects_field_assignment() {
    let errors = Engine::default()
        .check_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; };
            function main() -> int {
                var user = User();
                user.age = 25;
                return user.age;
            };
            "#,
        )
        .expect_err("immutable struct binding must reject field mutation");
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("immutable binding 'user'")),
        "{errors:?}"
    );
}

#[test]
fn impl_blocks_reject_non_struct_targets_and_duplicate_methods() {
    let non_struct = Engine::default()
        .check_source(
            r#"
            type UserId { value: str; };
            impl UserId {
                function value(self) -> str { return self.value; };
            };
            "#,
        )
        .expect_err("impl on a non-struct type must fail");
    assert!(non_struct.iter().any(|error| error.to_string().contains("impl") && error.to_string().contains("struct")), "{non_struct:?}");

    let duplicate = Engine::default()
        .check_source(
            r#"
            struct User { name: str = "Unknown"; };
            impl User { function name(self) -> str { return self.name; }; };
            impl User { function name(self) -> str { return self.name; }; };
            "#,
        )
        .expect_err("duplicate method definitions must fail");
    assert!(
        duplicate.iter().any(
            |error| error.to_string().contains("name") && error.to_string().contains("already")
        ),
        "{duplicate:?}"
    );
}

#[test]
fn immutable_receiver_rejects_mut_self_method_call() {
    let errors = Engine::default()
        .check_source(
            r#"
            struct User { active: bool = true; };
            impl User {
                function deactivate(mut self) -> void { self.active = false; };
            };
            function main() -> int {
                var user = User();
                user.deactivate();
                return 0;
            };
            "#,
        )
        .expect_err("mut self must require a mutable receiver binding");
    assert!(
        errors.iter().any(
            |error| error.to_string().contains("mutable") && error.to_string().contains("user")
        ),
        "{errors:?}"
    );
}

#[test]
fn private_impl_method_is_not_available_to_external_callers() {
    let errors = Engine::default()
        .check_source(
            r#"
            export struct User { name: str = "Obi"; };
            impl User {
                function publicName(self) -> str { return self.normalized(); };
                private function normalized(self) -> str { return self.name; };
            };
            function main() -> int {
                var user = User();
                var value: str = user.normalized();
                return 0;
            };
            "#,
        )
        .expect_err("private helper must not be callable from ordinary code");
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("private")
                && error.to_string().contains("normalized")),
        "{errors:?}"
    );
}

#[test]
fn structured_pipe_typechecks_function_calls_and_reports_input_mismatch() {
    Engine::default()
        .check_source(
            r#"
            function add(value: int, amount: int) -> int { return value + amount; };
            function main() -> int {
                var value: int = 5 |> add(3);
                return value;
            };
            "#,
        )
        .expect("structured pipe should inject the lhs as the first callable argument");

    let errors = Engine::default()
        .check_source(
            r#"
            function add(value: int, amount: int) -> int { return value + amount; };
            function main() -> int {
                var value: int = "bad" |> add(3);
                return value;
            };
            "#,
        )
        .expect_err("structured pipe must reject an incompatible lhs type");
    assert!(
        errors.iter().any(|error| {
            let message = error.to_string();
            message.contains("structured pipe")
                && message.contains("int")
                && message.contains("str")
        }),
        "{errors:?}"
    );
}

#[test]
fn table_is_a_builtin_generic_type_with_schema_aware_methods() {
    let source = r#"
        function inspect(rows: Table<Record>) -> int {
            var copied: List<Record> = rows.rows();
            var first: Table<Record> = rows.take(1);
            var rest: Table<Record> = rows.skip(1);
            var columns: List<str> = rows.columns();
            var schema: Schema = rows.schema();
            if rows.isEmpty() { return 0; }
            return first.length() + rest.length() + copied.length() + columns.length();
        };
        function main() -> int { return 0; };
    "#;

    Engine::default()
        .check_source(source)
        .expect("Table<Record> and its materialized methods should typecheck");
}

#[test]
fn table_requires_exactly_one_type_argument() {
    let errors = Engine::default()
        .check_source("var rows: Table; function main() -> int { return 0; };")
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("Table")
                && error.to_string().contains("1 type argument")),
        "{errors:?}"
    );
}

#[test]
fn stream_type_requires_exactly_one_type_argument() {
    let ok = Engine::default().check_source(
        r#"
            function identity(stream: Stream<int>) -> Stream<int> { return stream; };
            "#,
    );
    assert!(ok.is_ok(), "Stream<int> should resolve: {ok:?}");

    let errors = Engine::default()
        .check_source("function invalid(stream: Stream) -> void { return; };")
        .expect_err("bare Stream must require an element type");
    assert!(
        errors.iter().any(|error| error
            .to_string()
            .contains("type 'Stream' expects 1 type argument")),
        "{errors:?}"
    );
}

#[test]
fn mixed_structured_pipelines_are_rejected_in_byte_only_shell_surfaces() {
    let substitution = Engine::default()
        .check_source(
            r#"
            function main() -> int {
                var value: str = $(printf x | from lines |> to lines | cat);
                return 0;
            };
            "#,
        )
        .expect_err("mixed command substitution is not supported in v1");
    assert!(
        substitution
            .iter()
            .any(|error| error.to_string().contains("command substitution")
                && error.to_string().contains("structured mixed pipelines")),
        "{substitution:?}"
    );

    let exec_shell = Engine::default()
        .check_source(
            r#"
            function main() -> int {
                exec shell { printf x | from lines |> to lines | cat; };
                return 0;
            };
            "#,
        )
        .expect_err("exec shell must not erase structured stages");
    assert!(
        exec_shell
            .iter()
            .any(|error| error.to_string().contains("exec shell")
                && error.to_string().contains("structured mixed pipelines")),
        "{exec_shell:?}"
    );
}
