use indexmap::IndexMap;
use spar::Value;

#[test]
fn display_renderer_is_recursive_and_structural() {
    let value = Value::List(vec![
        Value::String("spar".into()),
        Value::Map(vec![(Value::String("answer".into()), Value::Int(42))].into()),
        Value::Option(Some(Box::new(Value::Bool(true)))),
        Value::Result(Err(Box::new(Value::String("boom".into())))),
    ]);

    assert_eq!(
        value.render_display(),
        "[\"spar\", {\"answer\": 42}, Some(true), Err(\"boom\")]"
    );
}

#[test]
fn display_renderer_uses_safe_summaries_for_bytes_and_records() {
    assert_eq!(Value::Bytes(vec![0, 1, 2]).render_display(), "Bytes(3)");

    let record = Value::Object(IndexMap::from([
        ("name".into(), Value::String("OCC".into())),
        ("active".into(), Value::Bool(true)),
    ]));
    assert_eq!(record.render_display(), "{name: \"OCC\", active: true}");
}

#[test]
fn top_level_string_rendering_is_raw() {
    assert_eq!(Value::String("hello".into()).render_display(), "hello");
}


#[test]
fn println_accepts_primitive_and_structural_values() {
    use std::sync::{Arc, Mutex};
    use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            fn main() -> int {
                println(value: 42);
                println(value: ["spar", "lang"]);
                return 0;
            };
            "#,
        )
        .expect("print APIs should accept Any values");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    engine
        .execute_compiled_with_context(&program, context)
        .expect("print APIs should render Any values");

    assert_eq!(
        String::from_utf8(stdout.lock().unwrap().clone()).unwrap(),
        "42\n[\"spar\", \"lang\"]\n"
    );
}


#[test]
fn omitted_option_struct_field_materializes_as_none() {
    use std::sync::{Arc, Mutex};
    use spar::{CompileOptions, Engine, RuntimeContext, RuntimeOutput};

    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(
            r#"
            struct User {
                name: str = "Mike";
                sex: Option<str> = none();
            };

            fn main() -> int {
                var user: User = User();
                println(value: user.sex);
                return 0;
            };
            "#,
        )
        .expect("an omitted Option<T> field should be a valid struct construction");

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let mut context = RuntimeContext::for_base_dir(program.base_dir());
    context.set_stdout(RuntimeOutput::Buffer(stdout.clone()));
    engine
        .execute_compiled_with_context(&program, context)
        .expect("omitted Option<T> field should exist at runtime");

    assert_eq!(
        String::from_utf8(stdout.lock().unwrap().clone()).unwrap(),
        "None\n"
    );
}

#[test]
fn option_struct_field_with_explicit_none_default_materializes_as_none() {
    use spar::{CompileOptions, Engine};

    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            struct Response {
                status: int = 0;
                contentType: Option<str> = none();
            };

            fn main() -> int {
                var response: Response = Response();
                if response.contentType.isSome() { return 1; }
                return 0;
            };
            "#,
        )
        .expect("Option<T> fields with none() defaults should materialize as None");

    assert_eq!(outcome.exit_status, 0);
}


#[test]
fn universal_to_string_and_type_name_methods_use_runtime_value_semantics() {
    use spar::{CompileOptions, Engine};

    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var values: List<int> = [1, 2];
                if values.toString() != "[1, 2]" { return 1; }
                if values.typeName() != "list" { return 2; }

                var maybe: Option<int> = some(value: 7);
                if maybe.toString() != "Some(7)" { return 3; }
                if maybe.typeName() != "Option" { return 4; }

                var mut valuesByName: Map<str, int> = { "answer": 42; };
                if valuesByName.typeName() != "Map" { return 5; }
                if valuesByName.toString() != "{\"answer\": 42}" { return 6; }
                return 0;
            };
            "#,
        )
        .expect("Any methods should be available on every runtime-storable value");

    assert_eq!(outcome.exit_status, 0);
}
