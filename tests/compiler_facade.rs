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
