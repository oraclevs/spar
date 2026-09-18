use spar::{
    CompileOptions, Engine, NativeFunction, NativeRegistry, RuntimeContext, Value,
};
use spar::ast::SparType;

#[test]
fn compiled_runtime_dispatches_registered_native_by_resolved_id() {
    let mut natives = NativeRegistry::new();
    natives
        .register(NativeFunction::sync(
            "test_native",
            "answer",
            vec![],
            SparType::Int,
            false,
            |_context: &mut RuntimeContext, _args: &[Value]| Ok(Value::Int(42)),
        ))
        .unwrap();

    let outcome = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source("function main() -> int { return test_native::answer(); };")
    .expect("public native should compile and execute");

    assert_eq!(outcome.exit_status, 42);
}

#[test]
fn user_source_cannot_call_private_native_capability() {
    let mut natives = NativeRegistry::new();
    natives
        .register(NativeFunction::sync(
            "__native_test",
            "secret",
            vec![],
            SparType::Int,
            true,
            |_context: &mut RuntimeContext, _args: &[Value]| Ok(Value::Int(1)),
        ))
        .unwrap();

    let errors = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .check_source("function main() -> int { return __native_test::secret(); };")
    .expect_err("private native capability must not be callable from user source");

    assert!(errors.iter().any(|error| {
        let text = error.to_string();
        text.contains("private") && text.contains("__native_test::secret")
    }));
}

#[test]
fn public_native_arguments_are_typechecked_from_registry_signature() {
    let mut natives = NativeRegistry::new();
    natives
        .register(NativeFunction::sync(
            "test_native",
            "takesInt",
            vec![("value", SparType::Int)],
            SparType::Int,
            false,
            |_context: &mut RuntimeContext, args: &[Value]| Ok(args[0].clone()),
        ))
        .unwrap();

    let errors = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .check_source(
        r#"function main() -> int { return test_native::takesInt(value: "wrong"); };"#,
    )
    .expect_err("native argument types must be checked from NativeRegistry metadata");

    assert!(errors.iter().any(|error| {
        let text = error.to_string();
        text.contains("expects int") && text.contains("got str")
    }));
}

#[test]
fn generic_native_signature_is_instantiated_from_call_arguments() {
    let mut natives = NativeRegistry::new();
    natives
        .register(NativeFunction::sync(
            "test_native",
            "identity",
            vec![("value", SparType::TypeParameter("T".into()))],
            SparType::TypeParameter("T".into()),
            false,
            |_context: &mut RuntimeContext, args: &[Value]| Ok(args[0].clone()),
        ))
        .unwrap();

    let outcome = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source("function main() -> int { return test_native::identity(value: 9); };")
    .expect("native type parameters should instantiate from argument types");

    assert_eq!(outcome.exit_status, 9);
}
