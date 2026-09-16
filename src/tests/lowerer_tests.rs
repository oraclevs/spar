use std::fs;

use crate::compiled::{FunctionId, ModuleId};
use crate::{CompileOptions, Engine};

#[test]
fn function_ids_are_deterministic_and_include_imported_functions() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("lib.spar"),
        "function helper() -> int { return 2; }; function main() -> int { return 99; };",
    )
    .unwrap();
    let source = "import \"lib.spar\" as lib; function main() -> int { return 0; };";
    let engine = Engine::new(CompileOptions {
        base_dir: dir.path().into(),
        ..Default::default()
    });

    let first = engine.compile_source(source).unwrap();
    let second = engine.compile_source(source).unwrap();

    assert_eq!(first.debug_function_keys(), second.debug_function_keys());
    assert_eq!(
        first.debug_function_keys(),
        ["<source>::main", "lib.spar::helper", "lib.spar::main"]
    );
    assert_eq!(first.entry, ModuleId(0));
    assert_eq!(first.entry_main, Some(FunctionId(0)));
}
