use spar::ast::SparType;
use spar::{CompileOptions, Engine, NativeMethod, NativeRegistry, RuntimeContext, Value};

fn engine_with_test_push() -> Engine {
    let mut natives = NativeRegistry::new();
    natives
        .register_method(NativeMethod::sync_mut(
            "List",
            "testPush",
            SparType::List(Box::new(SparType::TypeParameter("T".into()))),
            vec![("value", SparType::TypeParameter("T".into()))],
            SparType::Void,
            false,
            |_context: &mut RuntimeContext, receiver: &mut Value, args: &[Value]| {
                let Value::List(values) = receiver else {
                    panic!("mutable native receiver must be a list")
                };
                values.push(args[0].clone());
                Ok(Value::Void)
            },
        ))
        .unwrap();

    Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
}

#[test]
fn mutable_native_method_writes_back_local_receiver() {
    let outcome = engine_with_test_push()
        .execute_source(
            r#"
            fn main() -> int {
                var mut values: List<int> = [1];
                values.testPush(value: 2);
                return values.length();
            };
            "#,
        )
        .expect("mutable native method on local list");
    assert_eq!(outcome.exit_status, 2);
}

#[test]
fn mutable_native_method_writes_back_global_receiver() {
    let outcome = engine_with_test_push()
        .execute_source(
            r#"
            var mut values: List<int> = [1];
            fn main() -> int {
                values.testPush(value: 2);
                return values.length();
            };
            "#,
        )
        .expect("mutable native method on global list");
    assert_eq!(outcome.exit_status, 2);
}

#[test]
fn mutable_native_method_writes_back_nested_field_once() {
    let outcome = engine_with_test_push()
        .execute_source(
            r#"
            struct Holder {
                items: List<int> = [1];
                sibling: int = 41;
            };
            fn main() -> int {
                var mut holder: Holder = Holder();
                holder.items.testPush(value: 2);
                return holder.items.length() + holder.sibling;
            };
            "#,
        )
        .expect("nested mutable receiver should write back through the field path");
    assert_eq!(outcome.exit_status, 43);
}

#[test]
fn mutable_native_method_rejects_immutable_or_rvalue_receiver() {
    let engine = engine_with_test_push();
    let immutable = engine
        .check_source(
            r#"
            fn main() -> int {
                var values: List<int> = [1];
                values.testPush(value: 2);
                return 0;
            };
            "#,
        )
        .expect_err("immutable binding must reject mutable native method");
    assert!(immutable.iter().any(|error| error.to_string().contains("mutable")));

    let immutable_global = engine
        .check_source(
            r#"
            var values: List<int> = [1];
            fn main() -> int {
                values.testPush(value: 2);
                return 0;
            };
            "#,
        )
        .expect_err("immutable global binding must reject mutable native method");
    assert!(immutable_global
        .iter()
        .any(|error| error.to_string().contains("mutable")));

    let rvalue = engine
        .check_source(
            r#"
            fn main() -> int {
                [1].testPush(value: 2);
                return 0;
            };
            "#,
        )
        .expect_err("temporary receiver must reject mutable native method");
    assert!(rvalue.iter().any(|error| error.to_string().contains("mutable")));
}
