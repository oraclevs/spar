use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use spar::ast::SparType;
use spar::{CompileOptions, Engine, SchemaType, StreamResource, TableValue, Value};

#[test]
fn v2_runtime_acceptance_covers_record_schema_table_and_lazy_stream() {
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

    let table = TableValue::from_records(rows).expect("record rows should infer a table schema");
    assert_eq!(table.len(), 2);
    assert_eq!(table.schema().fields[0].name, "age");
    assert_eq!(table.schema().fields[0].ty, SchemaType::Int);
    assert_eq!(table.schema().fields[1].name, "name");
    assert_eq!(table.schema().fields[1].ty, SchemaType::Str);

    let pulls = Arc::new(AtomicUsize::new(0));
    let producer_pulls = Arc::clone(&pulls);
    let stream = StreamResource::new(SparType::Int, move || {
        producer_pulls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Value::Int(1)))
    });
    let mut empty = stream.take_lazy(0);

    assert_eq!(empty.next().expect("take(0) should complete cleanly"), None);
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
}

#[test]
fn v2_language_acceptance_composes_data_functions_and_value_methods() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { collectTable, filter, groupBy, get, select } from "std/data";

            struct User { name: str = ""; team: str = ""; active: bool = false; };

            function main() -> int {
                var users: Table<User> = [
                    User(name: "Obi", team: "red", active: true),
                    User(name: "Ada", team: "blue", active: false),
                    User(name: "Ngozi", team: "red", active: true)
                ] |> collectTable();

                var active: Table<User> = users
                    |> filter(fn(user: User) -> bool => user.active);
                var projected: Table<Record> = active |> select(["name", "team"]);
                var groups: Map<str, Table<User>> = users
                    |> groupBy(fn(user: User) -> str => user.team);

                if projected.count() != 2 { return 1; }
                if active.take(1).count() != 1 { return 2; }
                if get(source: groups, key: "red").count() != groups.get("red").count() { return 3; }
                if !"  spar  ".trim().upper().contains("SPAR") { return 4; }
                return 0;
            };
            "#,
        )
        .expect("Plan 2 language features should compose end-to-end");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn v2_language_acceptance_covers_option_and_result_values() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { some, none, ok, err } from "std";

            function main() -> int {
                var someValue: Option<int> = some<int>(value: 7);
                var noneValue: Option<int> = none<int>();
                var okValue: Result<int, str> = ok<int, str>(value: 11);
                var errValue: Result<int, str> = err<int, str>(error: "boom");

                if someValue.unwrapOr(0) != 7 { return 1; }
                if noneValue.unwrapOr(9) != 9 { return 2; }
                if okValue.unwrapOr(0) != 11 { return 3; }
                if errValue.unwrapOr(13) != 13 { return 4; }
                if !errValue.isErr() || !okValue.isOk() { return 5; }
                return 0;
            };
            "#,
        )
        .expect("Option/Result values should execute as first-class Plan 2 values");

    assert_eq!(outcome.exit_status, 0);
}
