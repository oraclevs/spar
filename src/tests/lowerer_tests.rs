use std::fs;

use crate::compiled::{FunctionId, LocalSlot, ModuleId, TypedOperation};
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
    let keys = first.debug_function_keys();
    assert!(keys.iter().any(|key| key == "<source>::main"));
    assert!(keys.iter().any(|key| key == "lib.spar::helper"));
    assert!(keys.iter().any(|key| key == "lib.spar::main"));
    assert_eq!(first.entry, ModuleId(0));
    assert_eq!(first.entry_main, Some(FunctionId(0)));
}

#[test]
fn parameters_locals_loop_bindings_and_shadows_get_stable_slots() {
    let source = "function f(input: int) -> int { var mut total: int = input; if true { var total: int = 2; } for (index, value) in [3] { total = total + index + value; } return total; };";
    let program = Engine::default().compile_source(source).unwrap();
    let function = &program.modules[0].functions[0];

    assert_eq!(function.parameter_slots, [LocalSlot(0)]);
    assert_eq!(function.slot_count, 5);
    assert_eq!(
        function.debug_slot_names(),
        ["input", "total", "total", "index", "value"]
    );
}

#[test]
fn primitive_operations_and_calls_are_resolved_during_lowering() {
    let source = "function double(value: int) -> int { return value + value; }; function main() -> int { return double(value: 3); };";
    let program = Engine::default().compile_source(source).unwrap();

    assert!(program.debug_operations().contains(&TypedOperation::IntAdd));
    assert_eq!(program.debug_direct_call_ids(), [FunctionId(0)]);
    let reads = program.debug_local_read_slots();
    assert!(
        reads.iter().filter(|slot| **slot == LocalSlot(0)).count() >= 2,
        "double(value) must still lower both reads of its parameter: {reads:?}"
    );
}
