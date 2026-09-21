//! Acceptance sweep for roadmap Tasks 9-14: Record/Schema, `Table<T>`,
//! table transforms, method/free-function unification, and lazy `Stream<T>`.

use spar::Engine;

fn status(source: &str) -> i32 {
    Engine::default()
        .execute_source(source)
        .unwrap_or_else(|errors| panic!("program should run: {errors:?}\n{source}"))
        .exit_status
}

fn error(source: &str) -> String {
    match Engine::default().execute_source(source) {
        Ok(outcome) => panic!(
            "expected a compile error, got status {}",
            outcome.exit_status
        ),
        Err(errors) => format!("{errors:?}"),
    }
}

const PRELUDE: &str = r#"
    import pkg { map, filter, where, take, skip, first, last, collect, collectTable,
                 count, sortBy, groupBy, unique, uniqueBy, flatten, get, select, schema,
                 inspect } from "std/data";
    struct User { name: str = ""; age: int = 0; team: str = "none"; };
"#;

fn users() -> &'static str {
    r#"
    var people: [User] = [
        User(name: "Obi", age: 24, team: "core"),
        User(name: "Ada", age: 31, team: "core"),
        User(name: "Zed", age: 17, team: "labs"),
        User(name: "Ada", age: 31, team: "core")
    ];
    "#
}

// ── Task 10-11: Table<T> and transforms ─────────────────────────────────────

#[test]
fn table_filter_map_sort_take_skip_first_last_count() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                {users}
                var table: Table<User> = people |> collectTable();
                var adults: Table<User> = table |> where(fn(u: User) -> bool => u.age >= 18);
                if (adults |> count()) != 3 {{ return 1; }}
                var sorted: Table<User> = table |> sortBy(fn(u: User) -> int => u.age);
                if first(source: sorted).name != "Zed" {{ return 2; }}
                if last(source: sorted).age != 31 {{ return 3; }}
                var page: Table<User> = sorted |> skip(1) |> take(2);
                if (page |> count()) != 2 {{ return 4; }}
                var ages: [int] = table |> map(fn(u: User) -> int => u.age) |> collect();
                return ages[0] + ages[2];
            }};
            "#,
            users = users()
        )),
        24 + 17
    );
}

#[test]
fn table_unique_by_group_by_and_get() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                {users}
                var table: Table<User> = people |> collectTable();
                var byName: Table<User> = table |> uniqueBy(fn(u: User) -> str => u.name);
                if (byName |> count()) != 3 {{ return 1; }}
                var groups: Map<str, Table<User>> = table |> groupBy(fn(u: User) -> str => u.team);
                var core: Table<User> = get(source: groups, key: "core");
                var labs: Table<User> = get(source: groups, key: "labs");
                return (core |> count()) * 10 + (labs |> count());
            }};
            "#,
            users = users()
        )),
        31
    );
}

#[test]
fn select_projects_to_records_with_only_the_requested_fields() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                {users}
                var projected: Table<Record> = people |> collectTable() |> select(["name", "team"]);
                var row: Record = first(source: projected);
                if !row.has("name") {{ return 1; }}
                if row.has("age") {{ return 2; }}
                if row.name != "Obi" {{ return 3; }}
                return row.keys().length();
            }};
            "#,
            users = users()
        )),
        2
    );
}

#[test]
fn lists_are_not_silently_tables() {
    // A list keeps list semantics: it does not gain Table-only members.
    let message = error(&format!(
        r#"{PRELUDE}
        function main() -> int {{
            {users}
            var t: Table<User> = people;
            return 0;
        }};
        "#,
        users = users()
    ));
    assert!(message.contains("TypeError"), "{message}");
}

// ── Task 9: Record and Schema ───────────────────────────────────────────────

#[test]
fn schema_lists_fields_in_a_stable_order_with_optional_and_dynamic() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                var rows: [Record] = [
                    {{ id: 1; name: "a"; }},
                    {{ id: 2; extra: true; }},
                    {{ id: "three"; name: "c"; }}
                ];
                var s: Schema = rows |> collectTable() |> schema();
                return 0;
            }};
            "#
        )),
        0
    );
}

// ── Task 12: methods and free functions share one implementation ────────────

#[test]
fn method_and_free_function_forms_agree_on_lists_and_tables() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                {users}
                var table: Table<User> = people |> collectTable();
                var freeForm: Table<User> = filter(source: table, predicate: fn(u: User) -> bool => u.age > 20);
                var methodForm: Table<User> = table.filter(fn(u: User) -> bool => u.age > 20);
                var pipeForm: Table<User> = table |> filter(fn(u: User) -> bool => u.age > 20);
                if freeForm.length() != methodForm.length() {{ return 1; }}
                if pipeForm.length() != methodForm.length() {{ return 2; }}
                var xs: [int] = [1, 2, 3, 4];
                var evens: [int] = xs.filter(fn(n: int) -> bool => n == 2 || n == 4);
                var evens2: [int] = xs |> filter(fn(n: int) -> bool => n == 2 || n == 4);
                return evens.length() * 10 + evens2.length();
            }};
            "#,
            users = users()
        )),
        22
    );
}

#[test]
fn wrong_predicate_type_is_a_compile_error_for_every_form() {
    let message = error(&format!(
        r#"{PRELUDE}
        function main() -> int {{
            {users}
            var t: Table<User> = people |> collectTable();
            var bad: Table<User> = t |> filter(fn(u: User) -> int => u.age);
            return 0;
        }};
        "#,
        users = users()
    ));
    assert!(message.contains("TypeError"), "{message}");
}

#[test]
fn untyped_closure_parameters_are_inferred_from_the_piped_input() {
    assert_eq!(
        status(&format!(
            r#"{PRELUDE}
            function main() -> int {{
                {users}
                var table: Table<User> = people |> collectTable();
                var adults: Table<User> = table |> where(fn(u) => u.age > 20);
                var names: [str] = adults |> map(fn(u) => u.name) |> collect();
                var total: [int] = people |> map(fn(u) => u.age * 2) |> collect();
                if names.length() != 3 {{ return 1; }}
                return total[0];
            }};
            "#,
            users = users()
        )),
        48
    );
}
