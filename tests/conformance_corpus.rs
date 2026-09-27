use std::fs;
use std::path::Path;

use spar::formatter::format_source;
use spar::{CompileOptions, Compiler};

fn compile(path: &Path, source: &str) -> spar::Compilation {
    Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(path)
    })
    .compile(source)
}

#[test]
fn compiler_accepts_shared_conformance_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();

    for name in manifest["valid"].as_array().unwrap() {
        let path = root.join(name.as_str().unwrap());
        let source = fs::read_to_string(&path).unwrap();
        let compilation = compile(&path, &source);
        assert!(
            compilation.errors.is_empty(),
            "{}: {:#?}",
            path.display(),
            compilation.errors
        );
    }
}

#[test]
fn removed_language_forms_have_stable_migration_diagnostics() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();

    for fixture in manifest["invalid"].as_array().unwrap() {
        let name = fixture["file"].as_str().unwrap();
        let expected = fixture["contains"].as_str().unwrap();
        let path = root.join(name);
        let source = fs::read_to_string(&path).unwrap();
        let compilation = compile(&path, &source);

        assert!(
            !compilation.errors.is_empty(),
            "{} unexpectedly compiled successfully",
            path.display()
        );

        let rendered = compilation
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains(expected),
            "{}: expected diagnostic containing {expected:?}, got:\n{rendered}",
            path.display()
        );
    }
}

#[test]
fn function_alias_is_accepted_but_formatter_canonicalizes_to_fn() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let path = root.join("function_alias.spar");
    let source = fs::read_to_string(&path).unwrap();

    let compilation = compile(&path, &source);
    assert!(compilation.errors.is_empty(), "{:#?}", compilation.errors);

    let formatted = format_source(&source).expect("function compatibility alias should format");
    assert!(formatted.contains("fn identity(value: int) -> int"), "{formatted}");
    assert!(!formatted.contains("function identity"), "{formatted}");
}
