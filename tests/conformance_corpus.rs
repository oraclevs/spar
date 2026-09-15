use std::fs;
use std::path::Path;

use spar::{CompileOptions, Compiler};

#[test]
fn compiler_accepts_shared_conformance_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    for name in manifest["valid"].as_array().unwrap() {
        let path = root.join(name.as_str().unwrap());
        let source = fs::read_to_string(&path).unwrap();
        let options = spar::CompileOptions {
            evaluate: false,
            ..spar::CompileOptions::for_path(&path)
        };
        let compilation = spar::Compiler::new(options).compile(&source);
        assert!(
            compilation.errors.is_empty(),
            "{}: {:?}",
            path.display(),
            compilation.errors
        );
    }
}

#[test]
fn phase0_compatibility_fixture_compiles_with_current_semantics() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/phase0_compatibility.spar");
    let source = fs::read_to_string(&path).unwrap();
    let compilation = Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(&path)
    })
    .compile(&source);

    assert!(
        compilation.errors.is_empty(),
        "{}: {:#?}",
        path.display(),
        compilation.errors
    );
}
