use std::fs;

use spar::{CompileOptions, Compiler};

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
