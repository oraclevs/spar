use std::fs;

use spar::Engine;

#[test]
fn execute_source_drives_async_main_to_completion() {
    let outcome = Engine::default()
        .execute_source(
            "async function answer() -> int { return 27; }; async function main() -> int { return await answer(); };",
        )
        .expect("async main should execute");
    assert_eq!(outcome.exit_status, 27);
}

#[test]
fn execute_source_drives_promise_created_during_module_initialization() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            async function answer() -> int { return 29; };
            var pending: Promise<int> = answer();
            async function main() -> int { return await pending; };
            "#,
        )
        .expect("module promise should belong to the execution runtime");
    assert_eq!(outcome.exit_status, 29);
}

#[test]
fn module_initialization_preserves_dependencies_between_promises() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            async function value() -> int { return 3; };
            async function increment(pending: Promise<int>) -> int {
                return await pending + 1;
            };
            var first: Promise<int> = value();
            var second: Promise<int> = increment(pending: first);
            async function main() -> int { return await second; };
            "#,
        )
        .expect("module promise dependency should execute");
    assert_eq!(outcome.exit_status, 4);
}

#[test]
fn async_main_preserves_void_and_shell_result_mapping() {
    let void_outcome = Engine::default()
        .execute_source("async function main() -> void { return; };")
        .expect("async void main should execute");
    assert_eq!(void_outcome.exit_status, 0);

    let shell_outcome = Engine::default()
        .execute_source("async function main() -> shell { return shell { false; }; };")
        .expect("async shell main should execute");
    assert_ne!(shell_outcome.exit_status, 0);
}

#[test]
fn execute_path_awaits_an_imported_async_function() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "async function answer() -> int { return 31; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "async function main() -> int { return await values::answer(); };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported async function should execute");
    assert_eq!(outcome.exit_status, 31);
}

#[test]
fn execute_path_drives_imported_promise_created_during_initialization() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "async function answer() -> int { return 37; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "var pending: Promise<int> = values::answer();\n",
            "async function main() -> int { return await pending; };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported module promise should execute");
    assert_eq!(outcome.exit_status, 37);
}

#[test]
fn phase4_language_fixtures_execute() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phase4");
    let struct_outcome = Engine::default()
        .execute_path(&root.join("struct_generics.spar"))
        .expect("generic struct fixture should execute");
    assert_eq!(struct_outcome.exit_status, 42);

    let catch_outcome = Engine::default()
        .execute_path(&root.join("try_catch.spar"))
        .expect("try/catch fixture should execute");
    assert_eq!(catch_outcome.exit_status, 7);

    let imported_outcome = Engine::default()
        .execute_path(&root.join("imported_struct.spar"))
        .expect("imported generic type fixture should execute");
    assert_eq!(imported_outcome.exit_status, 23);
}

#[test]
fn canonical_struct_uses_generic_defaults_and_field_access() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            type Config<T> { name: str = "default"; value: T; };
            struct App: Config<int> { value = 7; };
            function main() -> int {
                if App.name == "default" { return App.value; }
                return 0;
            };
            "#,
        )
        .expect("canonical struct should execute");
    assert_eq!(outcome.exit_status, 7);
}

#[test]
fn execute_path_runs_a_multi_file_projects_main() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("values.spar"),
        "function answer() -> int { return 42; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"values.spar\" as values;\n",
            "var result: int = values::answer();\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("execute should succeed");
    assert_eq!(outcome.exit_status, 42);
}

#[test]
fn check_path_never_evaluates_a_multi_file_project() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("broken.spar"),
        "export var answer: int = 1 / 0;\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"broken.spar\" as broken;\n",
            "var result: int = broken::answer;\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    // `check` never evaluates — the division by zero in `broken.spar`
    // would only be caught by a runtime that actually runs module init,
    // so a passing check here proves it stayed static.
    Engine::default()
        .check_path(&temp.path().join("main.spar"))
        .expect("check should succeed without evaluating anything");
}

#[test]
fn execute_path_reports_import_cycles_the_same_way_check_does() {
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
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"a.spar\" as a;\n",
            "var result: int = a::a;\n",
            "function main() -> int { return result; };\n",
        ),
    )
    .unwrap();

    let errors = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect_err("execute should fail on an import cycle");
    assert!(
        errors
            .iter()
            .any(|error| error.to_string().contains("import cycle detected")),
        "{errors:?}"
    );
}

#[test]
fn execute_path_runs_imported_generic_functions() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("generic.spar"),
        "function identity<T>(value: T) -> T { return value; };\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"generic.spar\" as generic;\n",
            "function main() -> int { return generic::identity<int>(value: 11); };\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported generic should execute");
    assert_eq!(outcome.exit_status, 11);
}

#[test]
fn execute_path_preserves_generics_through_selective_imports() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("generic.spar"),
        concat!(
            "export type [Box<T>] { value: T; };\n",
            "function identity<T>(value: T) -> T { return value; };\n",
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import { identity } from \"generic.spar\";\n",
            "import type { Box } from \"generic.spar\";\n",
            "function main() -> int {\n",
            "    var boxed: Box<int> = { value: identity(value: 19); };\n",
            "    return boxed.value;\n",
            "};\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("selectively imported generics should execute");
    assert_eq!(outcome.exit_status, 19);
}

#[test]
fn closure_outlives_defining_function_and_invokes_positionally() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function make(min: int) -> fn(int) -> bool {
                return fn(value) => value >= min;
            };
            function main() -> int {
                var check: fn(int) -> bool = make(min: 10);
                if check(11) { return 1; }
                return 0;
            };
            "#,
        )
        .expect("owned closure should remain callable after make returns");
    assert_eq!(outcome.exit_status, 1);
}

#[test]
fn closure_capture_is_by_value() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function main() -> int {
                var mut threshold: int = 10;
                var check: fn(int) -> bool = fn(value) => value > threshold;
                threshold = 20;
                if check(11) { return 1; }
                return 0;
            };
            "#,
        )
        .expect("closure should retain the value captured at creation time");
    assert_eq!(outcome.exit_status, 1);
}

#[test]
fn named_function_can_be_stored_and_invoked_as_callable_value() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function double(value: int) -> int { return value * 2; };
            function main() -> int {
                var callback: fn(int) -> int = double;
                return callback(7);
            };
            "#,
        )
        .expect("named function value should be dynamically callable");
    assert_eq!(outcome.exit_status, 14);
}

#[test]
fn struct_constructor_clones_defaults_and_applies_named_overrides() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; active: bool = true; };
            function main() -> int {
                var user = User(name: "Obi", age: 24);
                if user.name == "Obi" && user.active { return user.age; }
                return 0;
            };
            "#,
        )
        .expect("struct constructor should clone canonical values and override fields");
    assert_eq!(outcome.exit_status, 24);
}

#[test]
fn mutable_struct_binding_allows_field_assignment() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; };
            function main() -> int {
                var mut user = User();
                user.age = 25;
                return user.age;
            };
            "#,
        )
        .expect("mutable struct binding should support field mutation");
    assert_eq!(outcome.exit_status, 25);
}

#[test]
fn impl_methods_static_factories_and_mut_self_execute() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            struct User { name: str = "Unknown"; age: int = 18; active: bool = true; };
            impl User {
                function isAdult(self) -> bool { return self.age >= 18; };
                function deactivate(mut self) -> void { self.active = false; };
                function adult(name: str) -> User { return User(name: name, age: 18); };
                private function normalized(self) -> str { return self.name; };
            };
            function main() -> int {
                var mut user = User.adult("Obi");
                if !user.isAdult() { return 1; }
                user.deactivate();
                if user.active { return 2; }
                return user.age;
            };
            "#,
        )
        .expect("struct methods and static factories should execute");
    assert_eq!(outcome.exit_status, 18);
}

#[test]
fn multiple_impl_blocks_merge_into_one_method_set() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            struct User { name: str = "Obi"; age: int = 24; };
            impl User { function name(self) -> str { return self.name; }; };
            impl User { function age(self) -> int { return self.age; }; };
            function main() -> int {
                var user = User();
                if user.name() == "Obi" { return user.age(); }
                return 0;
            };
            "#,
        )
        .expect("multiple impl blocks should merge");
    assert_eq!(outcome.exit_status, 24);
}

#[test]
fn structured_pipe_executes_calls_bare_callables_and_closures() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function add(value: int, amount: int) -> int { return value + amount; };
            function double(value: int) -> int { return value * 2; };
            function main() -> int {
                var a: int = 5 |> add(3);
                var transform: fn(int) -> int = double;
                var b: int = a |> transform;
                var c: int = b |> fn(value: int) -> int => value + 1;
                return c;
            };
            "#,
        )
        .expect("structured pipe should execute ordinary calls, bare callables, and closures");
    assert_eq!(outcome.exit_status, 17);
}

#[test]
fn structured_pipe_binds_lower_than_arithmetic() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function double(value: int) -> int { return value * 2; };
            function main() -> int {
                return 1 + 2 |> double;
            };
            "#,
        )
        .expect("arithmetic should bind before structured pipe");
    assert_eq!(outcome.exit_status, 6);
}

#[test]
fn schema_inference_over_records_is_public_and_deterministic() {
    use spar::{Schema, SchemaField, SchemaType, Value};

    let rows = vec![
        Value::Object(indexmap::IndexMap::from([
            ("name".into(), Value::String("Obi".into())),
            ("age".into(), Value::Int(24)),
        ])),
        Value::Object(indexmap::IndexMap::from([
            ("name".into(), Value::String("Ada".into())),
            ("active".into(), Value::Bool(true)),
        ])),
    ];

    let schema = Schema::infer_records(&rows).unwrap();
    assert_eq!(
        schema.fields,
        vec![
            SchemaField {
                name: "active".into(),
                ty: SchemaType::Bool,
                optional: true,
            },
            SchemaField {
                name: "age".into(),
                ty: SchemaType::Int,
                optional: true,
            },
            SchemaField {
                name: "name".into(),
                ty: SchemaType::Str,
                optional: false,
            },
        ]
    );
}

#[test]
fn schema_inference_rejects_non_record_rows() {
    use spar::{Schema, Value};

    let error = Schema::infer_records(&[Value::Int(1)]).unwrap_err();
    assert_eq!(error.row_index, 0);
    assert_eq!(error.actual_type, "int");
}

#[test]
fn materialized_table_preserves_rows_schema_and_slice_operations() {
    use spar::{SchemaType, TableValue, Value};

    let rows = vec![
        Value::Object(indexmap::IndexMap::from([
            ("name".into(), Value::String("Obi".into())),
            ("age".into(), Value::Int(24)),
        ])),
        Value::Object(indexmap::IndexMap::from([
            ("name".into(), Value::String("Ada".into())),
            ("age".into(), Value::Int(31)),
        ])),
    ];
    let table = TableValue::from_records(rows.clone()).unwrap();

    assert_eq!(table.rows(), rows.as_slice());
    assert_eq!(table.len(), 2);
    assert!(!table.is_empty());
    assert_eq!(table.schema().fields[0].name, "age");
    assert_eq!(table.schema().fields[0].ty, SchemaType::Int);
    assert_eq!(table.take(1).rows(), &rows[..1]);
    assert_eq!(table.skip(1).rows(), &rows[1..]);
}

#[test]
fn stream_type_is_a_builtin_single_argument_generic() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function accepts(values: Stream<int>) -> int { return 1; };
            function main() -> int { return 0; };
            "#,
        )
        .expect("Stream<T> should resolve and type-check as a built-in generic runtime type");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn completed_stream_is_removed_from_runtime_resources() {
    use spar::ast::SparType;
    use spar::{RuntimeContext, StreamResource, Value};

    let mut emitted = false;
    let mut context = RuntimeContext::new(std::env::temp_dir());
    let id = context.insert_stream(StreamResource::new(SparType::Int, move || {
        if emitted {
            Ok(None)
        } else {
            emitted = true;
            Ok(Some(Value::Int(7)))
        }
    }));

    assert!(context.resources().contains(id));
    assert_eq!(context.stream_next(id).unwrap(), Some(Value::Int(7)));
    assert!(context.resources().contains(id));
    assert_eq!(context.stream_next(id).unwrap(), None);
    assert!(!context.resources().contains(id));
}

#[test]
fn failed_stream_is_removed_and_runs_cleanup_hook() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use spar::ast::SparType;
    use spar::{RuntimeContext, SparError, StreamResource};

    let cleanups = Arc::new(AtomicUsize::new(0));
    let marker = cleanups.clone();
    let mut context = RuntimeContext::new(std::env::temp_dir());
    let id = context.insert_stream(StreamResource::with_cancel(
        SparType::Int,
        || {
            Err(SparError::EvalError {
                message: "boom".into(),
                span: spar::Span::dummy(),
            })
        },
        move || {
            marker.fetch_add(1, Ordering::SeqCst);
        },
    ));

    assert!(context.stream_next(id).is_err());
    assert!(!context.resources().contains(id));
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
}

#[test]
fn cancelling_stream_removes_resource_and_invokes_hook_once() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use spar::ast::SparType;
    use spar::{RuntimeContext, StreamResource, Value};

    let cancellations = Arc::new(AtomicUsize::new(0));
    let marker = cancellations.clone();
    let mut context = RuntimeContext::new(std::env::temp_dir());
    let id = context.insert_stream(StreamResource::with_cancel(
        SparType::Int,
        || Ok(Some(Value::Int(1))),
        move || {
            marker.fetch_add(1, Ordering::SeqCst);
        },
    ));

    assert!(context.cancel_stream(id));
    assert!(!context.resources().contains(id));
    assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    assert!(!context.cancel_stream(id));
    assert_eq!(cancellations.load(Ordering::SeqCst), 1);
}
